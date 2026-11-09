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

pub struct LowLatency {
    kind: LowLatencyKind,
    enabled: bool,
    frame: u64,
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

    /// Début d'image, avant la lecture des entrées : attente pilotée par le pilote.
    pub fn frame_begin(&mut self) {
        self.frame += 1;
        #[cfg(windows)]
        if self.enabled {
            self.imp.sleep();
            self.imp.marker(self.frame, Marker::SimulationStart);
            self.imp.marker(self.frame, Marker::InputSample);
        }
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
