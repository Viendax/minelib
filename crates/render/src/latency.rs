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

/// Décide s'il faut laisser le pilote faire attendre le jeu (Reflex, Anti-Lag). Cette
/// attente ne réduit la latence que si la carte graphique est le frein (des images
/// s'empilent devant elle) et que les images sont lentes ; ailleurs elle ne fait que
/// coûter des images par seconde. Et même là, elle peut coûter cher : sur un portable
/// (RTX 4050 chaude, 160 à 260 images par seconde), elle retirait 15 à 35 % des images
/// et faisait durer une image sur cent de 16 à 90 ms (1 % bas de 11 à 60 images par
/// seconde, contre 140 à 280 sans elle).
///
/// La règle est donc : ne pas nuire. L'attente n'est essayée que si la carte est le frein
/// et les images lentes (moins de 144 par seconde) pendant une seconde ; elle n'est
/// gardée que si, mesure faite, elle ne coûte ni images par seconde ni à-coups ; sinon
/// elle est abandonnée, et réessayée bien plus tard. Tout se juge sur des moyennes d'une
/// demi-seconde : un à-coup isolé (chargement, autre programme) ne décide de rien.
#[derive(Clone, Debug, Default)]
pub struct Pacer {
    /// Attente du pilote en service.
    pub driver_wait: bool,
    /// Demi-seconde en cours : durée, images, temps bloqué sur la carte, images longues.
    win_t: f32,
    win_n: u32,
    win_blocked: f32,
    win_long: u32,
    /// Durée moyenne d'une image à la demi-seconde précédente (repère des images longues).
    last_avg: f32,
    /// Sans l'attente : demi-secondes de suite où la carte est le frein, images lentes.
    bound: u32,
    /// Référence prise sans l'attente juste avant de l'essayer : durée moyenne d'une
    /// image (s) et images longues par seconde.
    ref_frame: f32,
    ref_long: f32,
    /// Avec l'attente : demi-secondes écoulées depuis qu'elle est en service.
    on_windows: u32,
    /// Temps restant avant de pouvoir réessayer l'attente, et délai du prochain abandon.
    ban: f32,
    ban_next: f32,
    /// Dernière décision, pour le journal.
    pub why: String,
}

impl Pacer {
    /// Durée d'une mesure (s).
    const WINDOW: f32 = 0.5;
    /// Au-delà de cette part de l'image passée bloquée, la carte graphique est le frein.
    const GPU_BOUND: f32 = 0.6;
    /// Au-dessus de 144 images par seconde, l'attente du pilote ne sert plus : même avec
    /// la carte saturée, les images en file ne coûtent que quelques millisecondes.
    const SLOW_FRAME: f32 = 1.0 / 144.0;
    /// L'attente est abandonnée si elle allonge l'image moyenne de plus de 8 %, ou si
    /// les images longues (plus de 2,5 fois la moyenne) deviennent bien plus fréquentes.
    const COST_MAX: f32 = 1.08;
    /// Avec l'attente, plus rien ne dit qui limite : toutes les 30 s, une seconde sans
    /// elle pour reprendre une référence (et l'abandonner si elle ne sert plus).
    const CHECK_EVERY: u32 = 60;
    /// Après un abandon : délai avant le prochain essai (doublé à chaque abandon).
    const BAN_MIN: f32 = 120.0;
    const BAN_MAX: f32 = 600.0;

    /// `period` : temps écoulé d'un début d'image au suivant (attente du pilote
    /// comprise) ; `blocked` : temps passé bloqué à attendre la carte graphique pendant
    /// cette image (image de la fenêtre, envoi, présentation). Renvoie vrai si l'état a
    /// changé (`why` dit pourquoi).
    pub fn update(&mut self, period: f32, blocked: f32) -> bool {
        if period <= 0.0 || period > 0.25 {
            return false;
        }
        self.ban = (self.ban - period).max(0.0);
        self.win_t += period;
        self.win_n += 1;
        self.win_blocked += blocked.clamp(0.0, period);
        if self.last_avg > 0.0 && period > 2.5 * self.last_avg {
            self.win_long += 1;
        }
        if self.win_t < Self::WINDOW {
            return false;
        }
        let avg = self.win_t / self.win_n as f32;
        let share = self.win_blocked / self.win_t;
        let long = self.win_long as f32 / self.win_t;
        (self.win_t, self.win_n, self.win_blocked, self.win_long) = (0.0, 0, 0.0, 0);
        self.last_avg = avg;
        if !self.driver_wait {
            if share > Self::GPU_BOUND && avg > Self::SLOW_FRAME && self.ban == 0.0 {
                self.bound += 1;
            } else {
                self.bound = 0;
            }
            if self.bound < 2 {
                return false;
            }
            self.bound = 0;
            self.driver_wait = true;
            self.on_windows = 0;
            (self.ref_frame, self.ref_long) = (avg, long);
            self.why = format!(
                "essayée : carte graphique saturée ({:.0} % de l'image) à {:.0} images par \
                 seconde",
                share * 100.0,
                1.0 / avg
            );
            return true;
        }
        self.on_windows += 1;
        // (La première demi-seconde après le changement ne compte pas : transition.)
        if self.on_windows < 2 {
            return false;
        }
        if avg > self.ref_frame * Self::COST_MAX || long > self.ref_long * 2.0 + 4.0 {
            self.driver_wait = false;
            self.ban_next = (self.ban_next * 2.0).clamp(Self::BAN_MIN, Self::BAN_MAX);
            self.ban = self.ban_next;
            self.why = format!(
                "abandonnée : {:.0} images par seconde avec elle contre {:.0} sans, {:.0} \
                 images longues par seconde contre {:.0} ; nouvel essai dans {:.0} s au \
                 plus tôt",
                1.0 / avg,
                1.0 / self.ref_frame,
                long,
                self.ref_long,
                self.ban
            );
            return true;
        }
        if self.on_windows >= Self::CHECK_EVERY {
            self.driver_wait = false;
            self.ban_next = 0.0;
            self.why = "suspendue une seconde (contrôle : sert-elle encore ?)".to_string();
            return true;
        }
        false
    }
}

pub struct LowLatency {
    kind: LowLatencyKind,
    enabled: bool,
    frame: u64,
    pacer: Pacer,
    /// Début de l'image précédente.
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

    /// Début d'image, avant la lecture des entrées : attente pilotée par le pilote, si
    /// elle sert (voir `Pacer`). `blocked` : temps passé bloqué sur la carte pendant
    /// l'image précédente (s).
    pub fn frame_begin(&mut self, blocked: f32) {
        self.frame += 1;
        let now = std::time::Instant::now();
        let period = self.last.map_or(0.0, |t| (now - t).as_secs_f32());
        self.last = Some(now);
        if self.enabled && self.pacer.update(period, blocked) {
            log::info!(
                "{} : attente du pilote {}",
                self.kind.label(),
                self.pacer.why
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
                            // Sans « Boost » : il faisait attendre le jeu 1,5 ms de plus
                            // par image (240 images par seconde au lieu de 340).
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

    /// Fait tourner `secs` secondes d'images de `dt` secondes dont `blocked` bloquées sur
    /// la carte ; avec l'attente du pilote en service, les images durent `with_wait`.
    /// Renvoie le temps passé avec l'attente.
    fn run(p: &mut Pacer, secs: f32, dt: f32, blocked: f32, with_wait: f32) -> f32 {
        let (mut t, mut on) = (0.0, 0.0);
        while t < secs {
            let d = if p.driver_wait { with_wait } else { dt };
            if p.driver_wait {
                on += d;
            }
            p.update(d, if p.driver_wait { 0.0 } else { blocked });
            t += d;
        }
        on
    }

    #[test]
    fn the_driver_wait_is_never_used_where_it_cannot_help() {
        let mut p = Pacer::default();
        assert!(!p.driver_wait, "au départ : sans attente");
        // Processeur limitant : 1 ms par image dont 0,4 ms de présentation.
        assert_eq!(run(&mut p, 10.0, 0.001, 0.0004, 0.001), 0.0);
        // Carte saturée mais images courtes : 230 images par seconde (4,3 ms dont 3
        // bloquées). C'est là que l'attente faisait chuter les images sur le portable.
        assert_eq!(run(&mut p, 30.0, 0.0043, 0.003, 0.0043), 0.0);
        // Des à-coups (une image de 60 ms presque toute bloquée, deux fois par seconde)
        // au milieu d'images de 4 ms : toujours rien.
        for _ in 0..40 {
            p.update(0.06, 0.055);
            assert_eq!(run(&mut p, 0.44, 0.004, 0.003, 0.004), 0.0);
        }
        assert!(!p.driver_wait);
    }

    #[test]
    fn the_driver_wait_is_kept_only_if_it_costs_nothing() {
        // Carte saturée à 100 images par seconde (10 ms dont 8 bloquées), et l'attente
        // ne coûte rien : essayée au bout d'une seconde, gardée (hors une seconde de
        // contrôle toutes les trente).
        let mut p = Pacer::default();
        assert_eq!(run(&mut p, 0.9, 0.010, 0.008, 0.010), 0.0);
        let on = run(&mut p, 120.0, 0.010, 0.008, 0.010);
        assert!(on > 110.0, "gardée : {on} s sur 120");
        // Même carte, mais l'attente retire un cinquième des images (12 ms au lieu de
        // 10) : abandonnée au bout d'une seconde, et pas réessayée avant longtemps.
        let mut p = Pacer::default();
        let on = run(&mut p, 600.0, 0.010, 0.008, 0.012);
        assert!(on < 6.0, "abandonnée : {on} s avec l'attente sur 600");
        assert!(p.why.contains("abandonnée"), "{}", p.why);
    }

    #[test]
    fn the_driver_wait_is_dropped_when_it_makes_frames_irregular() {
        // Même moyenne avec ou sans, mais avec l'attente une image sur vingt dure 60 ms
        // (les autres 7,4 ms) : abandonnée.
        let mut p = Pacer::default();
        let (mut t, mut on, mut i) = (0.0f32, 0.0f32, 0u32);
        while t < 300.0 {
            let d = if p.driver_wait {
                i += 1;
                if i % 20 == 0 {
                    0.060
                } else {
                    0.0074
                }
            } else {
                0.010
            };
            if p.driver_wait {
                on += d;
            }
            p.update(d, if p.driver_wait { 0.0 } else { 0.008 });
            t += d;
        }
        assert!(on < 5.0, "abandonnée : {on} s avec l'attente sur 300");
    }
}
