//! Température de la carte graphique (NVIDIA, via `nvml.dll` fourni avec le pilote), lue
//! toutes les cinq secondes par un fil à part (jamais par celui qui dessine). Elle sert
//! au journal : sur un portable, une machine chaude explique des touches qui remontent
//! mal (le clavier est juste au-dessus) et des chutes d'images (bridage thermique).

use std::sync::atomic::{AtomicI32, Ordering};

/// Dernière température lue (°C) ; `i32::MIN` : inconnue.
static TEMP: AtomicI32 = AtomicI32::new(i32::MIN);
static STARTED: std::sync::Once = std::sync::Once::new();

/// Température de la carte graphique (°C), si elle est connue. Le premier appel lance
/// la lecture ; la valeur arrive un instant après.
pub fn gpu_temp() -> Option<f32> {
    STARTED.call_once(start);
    match TEMP.load(Ordering::Relaxed) {
        i32::MIN => None,
        t => Some(t as f32),
    }
}

#[cfg(windows)]
fn start() {
    let _ = std::thread::Builder::new()
        .name("température".into())
        .spawn(watch);
}

#[cfg(not(windows))]
fn start() {}

#[cfg(windows)]
fn watch() {
    use std::ffi::{c_char, c_void, CStr};

    #[link(name = "kernel32")]
    extern "system" {
        fn LoadLibraryA(name: *const c_char) -> *mut c_void;
        fn GetProcAddress(module: *mut c_void, name: *const c_char) -> *mut c_void;
    }
    type Init = unsafe extern "C" fn() -> i32;
    type ByIndex = unsafe extern "C" fn(u32, *mut *mut c_void) -> i32;
    type Temperature = unsafe extern "C" fn(*mut c_void, i32, *mut u32) -> i32;
    /// NVML_TEMPERATURE_GPU : la sonde de la puce graphique.
    const SENSOR_GPU: i32 = 0;
    /// NVML_TEMPERATURE_THRESHOLD_GPU_MAX et NVML_TEMPERATURE_THRESHOLD_SLOWDOWN.
    const THRESHOLDS: [(i32, &str); 2] = [(3, "maximum annoncé"), (1, "bridage matériel à")];

    // SAFETY : fonctions documentées de NVML (bibliothèque du pilote NVIDIA), appelées
    // avec les types de leur déclaration C ; chaque retour est vérifié avant la suite.
    unsafe {
        let module = LoadLibraryA(c"nvml.dll".as_ptr());
        if module.is_null() {
            return;
        }
        let get = |name: &CStr| {
            let f = GetProcAddress(module, name.as_ptr());
            (!f.is_null()).then_some(f)
        };
        let (Some(init), Some(by_index), Some(temperature)) = (
            get(c"nvmlInit_v2"),
            get(c"nvmlDeviceGetHandleByIndex_v2"),
            get(c"nvmlDeviceGetTemperature"),
        ) else {
            return;
        };
        // (Même forme que la lecture de la température : appareil, numéro, valeur rendue.)
        let threshold = get(c"nvmlDeviceGetTemperatureThreshold")
            .map(|f| std::mem::transmute::<*mut c_void, Temperature>(f));
        let init: Init = std::mem::transmute(init);
        let by_index: ByIndex = std::mem::transmute(by_index);
        let temperature: Temperature = std::mem::transmute(temperature);
        if init() != 0 {
            return;
        }
        let mut dev = std::ptr::null_mut();
        if by_index(0, &mut dev) != 0 || dev.is_null() {
            return;
        }
        // Seuils annoncés par le pilote (pour le journal seulement : sur un portable, la
        // machine tient souvent la carte bien en dessous, vers 87 °C).
        let mut limit = String::new();
        if let Some(threshold) = threshold {
            for (kind, name) in THRESHOLDS {
                let mut t = 0u32;
                if threshold(dev, kind, &mut t) == 0 && (60..=130).contains(&t) {
                    limit += &format!(", {name} {t} °C");
                }
            }
        }
        let mut told = false;
        loop {
            let mut t = 0u32;
            if temperature(dev, SENSOR_GPU, &mut t) == 0 && t < 200 {
                TEMP.store(t as i32, Ordering::Relaxed);
                if !std::mem::replace(&mut told, true) {
                    log::info!("carte graphique : {t} °C au lancement (lue toutes les 5 s){limit}");
                }
            } else {
                TEMP.store(i32::MIN, Ordering::Relaxed);
            }
            std::thread::sleep(std::time::Duration::from_secs(5));
        }
    }
}
