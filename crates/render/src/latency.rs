//! Modes basse latence des pilotes graphiques (DirectX 12, Windows) :
//! - **NVIDIA Reflex** via NVAPI (`nvapi64.dll`, fourni avec le pilote NVIDIA) ;
//! - **AMD Anti-Lag 2** via l'extension du pilote AMD (`amdxc64.dll`).
//!
//! Principe commun : juste avant de lire les entrées du joueur, le pilote fait attendre
//! le CPU le temps qu'il faut pour que l'image ne patiente pas dans la file du GPU.
//! L'option n'existe que si la carte et le pilote la proposent (sinon `detect` → `None`).
//! Interfaces reprises des en-têtes publics NVAPI et Anti-Lag 2 SDK (licence MIT).

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LowLatencyKind {
    NvidiaReflex,
    AmdAntiLag2,
}

impl LowLatencyKind {
    pub fn label(self) -> &'static str {
        match self {
            LowLatencyKind::NvidiaReflex => "NVIDIA Reflex",
            LowLatencyKind::AmdAntiLag2 => "AMD Anti-Lag 2",
        }
    }
}

/// Étapes d'une image, pour les marqueurs de latence de Reflex.
#[derive(Clone, Copy, Debug)]
pub enum Marker {
    SimulationStart = 0,
    SimulationEnd = 1,
    RenderSubmitStart = 2,
    RenderSubmitEnd = 3,
    PresentStart = 4,
    PresentEnd = 5,
    InputSample = 6,
}

/// Décide, image par image, s'il faut laisser le pilote faire attendre le jeu (Reflex,
/// Anti-Lag). Cette attente ne réduit la latence que si la carte graphique est le frein
/// (des images s'empilent devant elle) ; quand c'est le processeur qui limite, la file est
/// déjà vide et l'attente du pilote, arrondie à ~0,5 ms par Windows, ne fait que plafonner
/// les FPS (paliers à 320 / 390 images par seconde). On reconnaît la carte saturée au temps
/// que le jeu passe bloqué à lui remettre l'image.
#[derive(Clone, Debug)]
pub struct Pacer {
    /// Attente du pilote en service.
    pub driver_wait: bool,
    /// Part lissée de l'image passée bloquée sur la carte graphique.
    blocked: f32,
    /// Temps depuis le dernier changement d'état.
    timer: f32,
    /// Délai avant le prochain essai sans attente (doublé à chaque essai manqué).
    retry: f32,
    /// Durée lissée d'une image sans l'attente du pilote (s).
    frame: f32,
}

impl Default for Pacer {
    fn default() -> Self {
        Self {
            driver_wait: true,
            blocked: 0.0,
            timer: 0.0,
            retry: Self::RETRY_MIN,
            frame: 0.0,
        }
    }
}

impl Pacer {
    const RETRY_MIN: f32 = 3.0;
    const RETRY_MAX: f32 = 60.0;
    /// Au-delà de cette part de l'image passée bloquée, la carte graphique est le frein.
    const GPU_BOUND: f32 = 0.6;
    /// En dessous de cette durée d'image (plus de 250 images par seconde), l'attente du
    /// pilote ne sert plus : même avec la carte saturée, les images en file ne coûtent que
    /// 2 à 4 ms de retard, alors que l'attente retire un tiers des images par seconde
    /// (mesuré : 2,1 ms par image sans elle, 3,2 ms avec).
    const SLOW_FRAME: f32 = 0.004;
    /// Durée d'un essai sans attente avant de le juger réussi.
    const TRIAL: f32 = 0.5;

    /// `dt` : durée de l'image précédente ; `blocked` : temps passé bloqué à attendre la
    /// carte graphique (image de la fenêtre, envoi, présentation). Renvoie vrai si l'état
    /// a changé.
    pub fn blocked_share(&self) -> f32 {
        self.blocked
    }

    pub fn update(&mut self, dt: f32, blocked: f32) -> bool {
        if dt <= 0.0 || dt > 0.25 {
            return false;
        }
        self.timer += dt;
        if self.driver_wait {
            // Avec l'attente du pilote, rien ne dit qui limite : essai périodique sans.
            if self.timer >= self.retry {
                self.driver_wait = false;
                self.timer = 0.0;
                self.blocked = 0.0;
                self.frame = 0.0;
                return true;
            }
            return false;
        }
        let k = (dt / 0.1).min(1.0);
        self.blocked += ((blocked / dt).clamp(0.0, 1.0) - self.blocked) * k;
        self.frame = if self.frame == 0.0 {
            dt
        } else {
            self.frame + (dt - self.frame) * k
        };
        if self.blocked > Self::GPU_BOUND && self.frame > Self::SLOW_FRAME {
            // Carte saturée : l'attente du pilote reprend ; essai suivant plus tard si
            // celui-ci venait de commencer.
            if self.timer < Self::TRIAL * 4.0 {
                self.retry = (self.retry * 2.0).min(Self::RETRY_MAX);
            } else {
                self.retry = Self::RETRY_MIN;
            }
            self.driver_wait = true;
            self.timer = 0.0;
            return true;
        }
        if self.timer > Self::TRIAL * 4.0 {
            self.retry = Self::RETRY_MIN;
        }
        false
    }
}

pub struct LowLatency {
    kind: LowLatencyKind,
    enabled: bool,
    frame: u64,
    pacer: Pacer,
    last: Option<std::time::Instant>,
    #[cfg(windows)]
    imp: win::Imp,
}

impl LowLatency {
    /// Détecte Reflex / Anti-Lag 2 sur le device (DirectX 12 uniquement).
    pub fn detect(device: &wgpu::Device, info: &wgpu::AdapterInfo) -> Option<Self> {
        #[cfg(windows)]
        {
            if info.backend != wgpu::Backend::Dx12 {
                return None;
            }
            let raw = unsafe {
                device.as_hal::<wgpu::hal::api::Dx12, _, _>(|d| {
                    d.map(|d| std::mem::transmute_copy::<_, *mut std::ffi::c_void>(d.raw_device()))
                })
            }?;
            let (kind, imp) = match info.vendor {
                0x10DE => (LowLatencyKind::NvidiaReflex, win::Imp::nvidia(raw)?),
                0x1002 => (LowLatencyKind::AmdAntiLag2, win::Imp::amd(raw)?),
                _ => return None,
            };
            log::info!("{} disponible", kind.label());
            Some(Self {
                kind,
                enabled: false,
                frame: 0,
                pacer: Pacer::default(),
                last: None,
                imp,
            })
        }
        #[cfg(not(windows))]
        {
            let _ = (device, info);
            None
        }
    }

    pub fn kind(&self) -> LowLatencyKind {
        self.kind
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn set_enabled(&mut self, on: bool) {
        if on == self.enabled {
            return;
        }
        self.enabled = on;
        #[cfg(windows)]
        self.imp.set_mode(on);
        log::info!(
            "{} : {}",
            self.kind.label(),
            if on { "activé" } else { "désactivé" }
        );
    }

    /// Début d'image, avant la lecture des entrées : attente pilotée par le pilote, si la
    /// carte graphique est le frein (voir `Pacer`). `blocked` : temps passé bloqué sur la
    /// carte pendant l'image précédente (s).
    pub fn frame_begin(&mut self, blocked: f32) {
        self.frame += 1;
        let now = std::time::Instant::now();
        let dt = self.last.map_or(0.0, |t| (now - t).as_secs_f32());
        if self.enabled && self.pacer.update(dt, blocked) {
            log::info!(
                "{} : attente du pilote {} — image {:.2} ms, part bloquée sur la carte {:.0} %",
                self.kind.label(),
                if self.pacer.driver_wait {
                    "reprise (carte graphique saturée, moins de 250 images par seconde)"
                } else {
                    "suspendue (essai)"
                },
                dt * 1000.0,
                self.pacer.blocked_share() * 100.0
            );
        }
        #[cfg(windows)]
        if self.enabled {
            if self.pacer.driver_wait {
                self.imp.sleep();
            }
            self.imp.marker(self.frame, Marker::SimulationStart);
            self.imp.marker(self.frame, Marker::InputSample);
        }
        // L'image se compte après l'attente : elle n'entre pas dans la part bloquée.
        self.last = Some(std::time::Instant::now());
        let _ = now;
    }

    /// L'attente du pilote est-elle en service en ce moment ?
    pub fn driver_wait(&self) -> bool {
        self.enabled && self.pacer.driver_wait
    }

    pub fn marker(&self, m: Marker) {
        if !self.enabled {
            return;
        }
        #[cfg(windows)]
        self.imp.marker(self.frame, m);
        #[cfg(not(windows))]
        let _ = m;
    }
}

#[cfg(windows)]
mod win {
    use super::Marker;
    use std::ffi::{c_char, c_void};

    #[link(name = "kernel32")]
    extern "system" {
        fn LoadLibraryA(name: *const c_char) -> *mut c_void;
        fn GetModuleHandleA(name: *const c_char) -> *mut c_void;
        fn GetProcAddress(module: *mut c_void, name: *const c_char) -> *mut c_void;
    }

    // ------------------------------------------------------------ NVAPI (Reflex)
    type QueryInterface = unsafe extern "C" fn(u32) -> *mut c_void;
    type NvInit = unsafe extern "C" fn() -> i32;
    type NvSetSleepMode = unsafe extern "C" fn(*mut c_void, *mut SleepModeParams) -> i32;
    type NvSleep = unsafe extern "C" fn(*mut c_void) -> i32;
    type NvSetMarker = unsafe extern "C" fn(*mut c_void, *mut MarkerParams) -> i32;

    const ID_INITIALIZE: u32 = 0x0150_e828;
    const ID_SET_SLEEP_MODE: u32 = 0xac1c_a9e0;
    const ID_SLEEP: u32 = 0x852c_d1d2;
    const ID_SET_LATENCY_MARKER: u32 = 0xd998_4c05;

    #[repr(C)]
    pub struct SleepModeParams {
        version: u32,
        low_latency_mode: u8,
        low_latency_boost: u8,
        minimum_interval_us: u32,
        use_markers_to_optimize: u8,
        use_min_queue_time: u8,
        rsvd: [u8; 30],
    }

    #[repr(C)]
    pub struct MarkerParams {
        version: u32,
        frame_id: u64,
        marker_type: i32,
        rsvd0: u64,
        rsvd: [u8; 56],
    }

    /// MAKE_NVAPI_VERSION : taille de la structure | (version << 16).
    fn nv_version<T>(v: u32) -> u32 {
        std::mem::size_of::<T>() as u32 | (v << 16)
    }

    // ------------------------------------------------------------ AMD Anti-Lag 2
    #[repr(C)]
    struct Guid(u32, u16, u16, [u8; 8]);
    const IID_ANTI_LAG: Guid = Guid(
        0x4408_5fbe,
        0xe839,
        0x40c5,
        [0xbf, 0x38, 0x0e, 0xbc, 0x5a, 0xb4, 0xd0, 0xa6],
    );
    type AmdCreate = unsafe extern "C" fn(*mut c_void, *const Guid, *mut *mut c_void) -> i32;

    #[repr(C)]
    struct AntiLagVtbl {
        query_interface: *const c_void,
        add_ref: *const c_void,
        release: unsafe extern "system" fn(*mut c_void) -> u32,
        update_state: unsafe extern "system" fn(*mut c_void, *mut c_void) -> i32,
    }

    #[repr(C)]
    struct AntiLagData {
        size: u32,
        version: u32,
        mode: u32,
        control_str: *const c_char,
        control_str_len: u32,
        max_fps: u32,
    }

    pub enum Imp {
        Nvidia {
            dev: *mut c_void,
            set_sleep: NvSetSleepMode,
            sleep: NvSleep,
            set_marker: NvSetMarker,
        },
        Amd {
            api: *mut c_void,
        },
    }

    impl Imp {
        pub fn nvidia(dev: *mut c_void) -> Option<Self> {
            unsafe {
                let module = LoadLibraryA(c"nvapi64.dll".as_ptr());
                if module.is_null() {
                    return None;
                }
                let qi = GetProcAddress(module, c"nvapi_QueryInterface".as_ptr());
                if qi.is_null() {
                    return None;
                }
                let qi: QueryInterface = std::mem::transmute(qi);
                let get = |id| {
                    let f = qi(id);
                    (!f.is_null()).then_some(f)
                };
                let init: NvInit = std::mem::transmute(get(ID_INITIALIZE)?);
                if init() != 0 {
                    log::warn!("NVAPI : initialisation refusée");
                    return None;
                }
                let imp = Imp::Nvidia {
                    dev,
                    set_sleep: std::mem::transmute::<*mut c_void, NvSetSleepMode>(get(
                        ID_SET_SLEEP_MODE,
                    )?),
                    sleep: std::mem::transmute::<*mut c_void, NvSleep>(get(ID_SLEEP)?),
                    set_marker: std::mem::transmute::<*mut c_void, NvSetMarker>(get(
                        ID_SET_LATENCY_MARKER,
                    )?),
                };
                // Reflex accepté par le pilote ? (mode désactivé au départ)
                imp.try_set_mode(false).then_some(imp)
            }
        }

        pub fn amd(dev: *mut c_void) -> Option<Self> {
            unsafe {
                let module = GetModuleHandleA(c"amdxc64.dll".as_ptr());
                if module.is_null() {
                    return None;
                }
                let f = GetProcAddress(module, c"AmdExtD3DCreateInterface".as_ptr());
                if f.is_null() {
                    return None;
                }
                let create: AmdCreate = std::mem::transmute(f);
                let mut api = std::ptr::null_mut();
                if create(dev, &IID_ANTI_LAG, &mut api) != 0 || api.is_null() {
                    return None;
                }
                let imp = Imp::Amd { api };
                if imp.try_set_mode(false) {
                    Some(imp)
                } else {
                    drop(imp);
                    None
                }
            }
        }

        fn try_set_mode(&self, on: bool) -> bool {
            unsafe {
                match self {
                    Imp::Nvidia { dev, set_sleep, .. } => {
                        let mut p = SleepModeParams {
                            version: nv_version::<SleepModeParams>(1),
                            low_latency_mode: on as u8,
                            low_latency_boost: 0,
                            minimum_interval_us: 0,
                            use_markers_to_optimize: on as u8,
                            use_min_queue_time: 0,
                            rsvd: [0; 30],
                        };
                        set_sleep(*dev, &mut p) == 0
                    }
                    Imp::Amd { api } => {
                        let mut d = AntiLagData {
                            size: std::mem::size_of::<AntiLagData>() as u32,
                            version: 1,
                            mode: if on { 1 } else { 2 },
                            control_str: std::ptr::null(),
                            control_str_len: 0,
                            max_fps: 0,
                        };
                        let vt = &**(*api as *const *const AntiLagVtbl);
                        (vt.update_state)(*api, &mut d as *mut _ as *mut c_void) >= 0
                    }
                }
            }
        }

        pub fn set_mode(&self, on: bool) {
            if !self.try_set_mode(on) {
                log::warn!("mode basse latence : refusé par le pilote");
            }
        }

        pub fn sleep(&self) {
            unsafe {
                match self {
                    Imp::Nvidia { dev, sleep, .. } => {
                        sleep(*dev);
                    }
                    Imp::Amd { api } => {
                        let vt = &**(*api as *const *const AntiLagVtbl);
                        (vt.update_state)(*api, std::ptr::null_mut());
                    }
                }
            }
        }

        pub fn marker(&self, frame: u64, m: Marker) {
            if let Imp::Nvidia {
                dev, set_marker, ..
            } = self
            {
                let mut p = MarkerParams {
                    version: nv_version::<MarkerParams>(1),
                    frame_id: frame,
                    marker_type: m as i32,
                    rsvd0: 0,
                    rsvd: [0; 56],
                };
                unsafe {
                    set_marker(*dev, &mut p);
                }
            }
        }
    }

    impl Drop for Imp {
        fn drop(&mut self) {
            if let Imp::Amd { api } = self {
                unsafe {
                    let vt = &**(*api as *const *const AntiLagVtbl);
                    (vt.release)(*api);
                }
            }
        }
    }

    // Tailles imposées par les en-têtes NVAPI / Anti-Lag 2 (vérifiées à la compilation).
    const _: () = assert!(std::mem::size_of::<SleepModeParams>() == 44);
    const _: () = assert!(std::mem::size_of::<MarkerParams>() == 88);
    const _: () = assert!(std::mem::size_of::<AntiLagData>() == 32);
}

#[cfg(test)]
mod pacer_tests {
    use super::Pacer;

    fn run(p: &mut Pacer, secs: f32, dt: f32, blocked: f32) {
        for _ in 0..(secs / dt) as usize {
            p.update(dt, blocked);
        }
    }

    #[test]
    fn driver_wait_is_dropped_when_the_cpu_is_the_limit_and_kept_when_the_gpu_is() {
        let mut p = Pacer::default();
        assert!(p.driver_wait);
        // Processeur limitant : 1 ms par image dont 0,4 ms de présentation.
        run(&mut p, 4.0, 0.001, 0.0004);
        assert!(!p.driver_wait, "essai lancé puis gardé");
        run(&mut p, 10.0, 0.001, 0.0004);
        assert!(!p.driver_wait);
        // Carte saturée mais images très courtes (2 ms) : l'attente coûterait plus qu'elle
        // ne rapporte.
        run(&mut p, 2.0, 0.002, 0.0015);
        assert!(
            !p.driver_wait,
            "plus de 250 images par seconde : pas d'attente"
        );
        // La carte sature pour de bon : 5 ms par image dont 4 ms bloquées.
        run(&mut p, 0.5, 0.005, 0.004);
        assert!(p.driver_wait, "attente reprise");
        // Tant qu'elle sature, les essais échouent vite et s'espacent.
        let mut free = 0.0;
        for _ in 0..(60.0f32 / 0.005) as usize {
            p.update(0.005, 0.004);
            if !p.driver_wait {
                free += 0.005;
            }
        }
        assert!(free < 3.0, "sans attente {free} s sur 60");
    }
}
