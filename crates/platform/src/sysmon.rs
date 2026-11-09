//! Moniteur système pour l'écran de debug (F3).
//!
//! Un thread mesure une fois par seconde :
//! - CPU et RAM (processus + système) via `sysinfo`, toutes plateformes ;
//! - GPU et VRAM :
//!   * Windows : DXGI (`QueryVideoMemoryInfo` → VRAM du processus et budget,
//!     `DedicatedVideoMemory` → VRAM totale) et compteurs de performance PDH
//!     (`GPU Engine` → utilisation, `GPU Adapter Memory` → VRAM utilisée), les mêmes
//!     sources que le Gestionnaire des tâches ;
//!   * Linux : sysfs des pilotes `amdgpu` (`gpu_busy_percent`, `mem_info_vram_*`).
//!
//! Ce qui n'est pas disponible reste à `None` et s'affiche « n/d ».

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use sysinfo::{CpuRefreshKind, MemoryRefreshKind, Pid, ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System};

#[derive(Clone, Debug, Default)]
pub struct SysStats {
    pub cpu_name: String,
    pub cpu_threads: usize,
    /// Utilisation CPU du processus, en % de la machine entière (0..100).
    pub cpu_process: f32,
    /// Utilisation CPU globale (0..100).
    pub cpu_system: f32,
    pub ram_process: u64,
    pub ram_used: u64,
    pub ram_total: u64,
    /// Utilisation du GPU, toutes applications confondues (0..100).
    pub gpu_usage: Option<f32>,
    /// Part du GPU utilisée par ce processus (0..100).
    pub gpu_process: Option<f32>,
    pub vram_process: Option<u64>,
    pub vram_used: Option<u64>,
    pub vram_total: Option<u64>,
    pub vram_budget: Option<u64>,
}

pub struct SysMonitor {
    stats: Arc<Mutex<SysStats>>,
    gpu_name: Arc<Mutex<Option<String>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl SysMonitor {
    pub fn start() -> Self {
        let stats = Arc::new(Mutex::new(SysStats::default()));
        let gpu_name = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (stats, gpu_name, stop) = (stats.clone(), gpu_name.clone(), stop.clone());
            std::thread::Builder::new()
                .name("minelib-sysmon".into())
                .spawn(move || monitor_loop(stats, gpu_name, stop))
                .ok()
        };
        Self { stats, gpu_name, stop, thread }
    }

    /// Nom du GPU utilisé par le rendu, pour choisir le bon adaptateur.
    pub fn set_gpu_name(&self, name: &str) {
        *self.gpu_name.lock().unwrap() = Some(name.to_owned());
    }

    pub fn latest(&self) -> SysStats {
        self.stats.lock().unwrap().clone()
    }
}

impl Drop for SysMonitor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn monitor_loop(out: Arc<Mutex<SysStats>>, gpu_name: Arc<Mutex<Option<String>>>, stop: Arc<AtomicBool>) {
    // Mesures système désactivables (diagnostic, Wine).
    if std::env::var_os("MINELIB_NO_PDH").is_some() {
        return;
    }
    let pid = sysinfo::get_current_pid().ok();
    let mut sys = System::new_with_specifics(
        RefreshKind::nothing().with_cpu(CpuRefreshKind::everything()).with_memory(MemoryRefreshKind::everything()),
    );
    let mut gpu = gpu::GpuProbe::new();

    while !stop.load(Ordering::Relaxed) {
        sys.refresh_cpu_usage();
        sys.refresh_memory();
        if let Some(pid) = pid {
            sys.refresh_processes_specifics(
                ProcessesToUpdate::Some(&[pid]),
                true,
                ProcessRefreshKind::nothing().with_cpu().with_memory(),
            );
        }
        let threads = sys.cpus().len().max(1);
        let proc = pid.and_then(|p: Pid| sys.process(p));

        let mut s = SysStats {
            cpu_name: sys.cpus().first().map(|c| c.brand().trim().to_owned()).unwrap_or_default(),
            cpu_threads: threads,
            // sysinfo donne le % d'un seul cœur : on ramène à la machine entière.
            cpu_process: proc.map(|p| p.cpu_usage() / threads as f32).unwrap_or(0.0),
            cpu_system: sys.global_cpu_usage(),
            ram_process: proc.map(|p| p.memory()).unwrap_or(0),
            ram_used: sys.used_memory(),
            ram_total: sys.total_memory(),
            ..Default::default()
        };
        let name = gpu_name.lock().unwrap().clone();
        gpu.sample(name.as_deref(), pid.map(|p| p.as_u32()), &mut s);
        *out.lock().unwrap() = s;

        // Attente de 1 s, interruptible.
        for _ in 0..10 {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

// ------------------------------------------------------------------ Windows

#[cfg(windows)]
mod gpu {
    use super::SysStats;
    use std::collections::HashMap;
    use windows::core::{Interface, PCWSTR};
    use windows::Win32::Graphics::Dxgi::*;
    use windows::Win32::System::Performance::*;

    pub struct GpuProbe {
        adapter: Option<IDXGIAdapter3>,
        adapter_for: Option<String>,
        vram_total: Option<u64>,
        pdh: Option<Pdh>,
    }

    impl GpuProbe {
        pub fn new() -> Self {
            Self { adapter: None, adapter_for: None, vram_total: None, pdh: Pdh::open() }
        }

        pub fn sample(&mut self, gpu_name: Option<&str>, pid: Option<u32>, s: &mut SysStats) {
            if self.adapter.is_none() || self.adapter_for.as_deref() != gpu_name {
                self.pick_adapter(gpu_name);
            }
            if let Some(a) = &self.adapter {
                let mut info = DXGI_QUERY_VIDEO_MEMORY_INFO::default();
                if unsafe { a.QueryVideoMemoryInfo(0, DXGI_MEMORY_SEGMENT_GROUP_LOCAL, &mut info) }.is_ok() {
                    s.vram_process = Some(info.CurrentUsage);
                    s.vram_budget = Some(info.Budget);
                }
            }
            s.vram_total = self.vram_total;
            if let Some(p) = &mut self.pdh {
                p.sample(pid, s);
            }
        }

        /// Choisit l'adaptateur DXGI dont le nom correspond au GPU du rendu,
        /// sinon celui qui a le plus de VRAM dédiée.
        fn pick_adapter(&mut self, gpu_name: Option<&str>) {
            self.adapter_for = gpu_name.map(str::to_owned);
            let Ok(factory) = (unsafe { CreateDXGIFactory1::<IDXGIFactory1>() }) else { return };
            let mut best: Option<(bool, u64, IDXGIAdapter1)> = None;
            for i in 0.. {
                let Ok(a) = (unsafe { factory.EnumAdapters1(i) }) else { break };
                let Ok(desc) = (unsafe { a.GetDesc1() }) else { continue };
                if desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0 {
                    continue;
                }
                let len = desc.Description.iter().position(|&c| c == 0).unwrap_or(desc.Description.len());
                let name = String::from_utf16_lossy(&desc.Description[..len]);
                let matches = gpu_name.is_some_and(|g| g.contains(name.trim()) || name.contains(g));
                let vram = desc.DedicatedVideoMemory as u64;
                let better = match &best {
                    None => true,
                    Some((m, v, _)) => (matches, vram) > (*m, *v),
                };
                if better {
                    best = Some((matches, vram, a));
                }
            }
            if let Some((_, vram, a)) = best {
                self.vram_total = Some(vram);
                self.adapter = a.cast::<IDXGIAdapter3>().ok();
            }
        }
    }

    struct Pdh {
        query: isize,
        engines: isize,
        memory: isize,
        primed: bool,
    }

    // Les handles PDH ne sont utilisés que depuis le thread du moniteur.
    impl Pdh {
        fn open() -> Option<Self> {
            // Compteurs GPU de Windows désactivables (absents sous Wine).
            if std::env::var_os("MINELIB_NO_PDH").is_some() {
                return None;
            }
            unsafe {
                let mut query = 0isize;
                if PdhOpenQueryW(PCWSTR::null(), 0, &mut query) != 0 {
                    return None;
                }
                let mut engines = 0isize;
                let mut memory = 0isize;
                let e = PdhAddEnglishCounterW(
                    query,
                    windows::core::w!("\\GPU Engine(*)\\Utilization Percentage"),
                    0,
                    &mut engines,
                );
                let m = PdhAddEnglishCounterW(
                    query,
                    windows::core::w!("\\GPU Adapter Memory(*)\\Dedicated Usage"),
                    0,
                    &mut memory,
                );
                if e != 0 && m != 0 {
                    let _ = PdhCloseQuery(query);
                    return None;
                }
                let _ = PdhCollectQueryData(query);
                Some(Self { query, engines, memory, primed: false })
            }
        }

        fn sample(&mut self, pid: Option<u32>, s: &mut SysStats) {
            unsafe {
                if PdhCollectQueryData(self.query) != 0 {
                    return;
                }
            }
            // Les pourcentages ont besoin de deux mesures : la première est ignorée.
            if !self.primed {
                self.primed = true;
                return;
            }
            if let Some(items) = read_array(self.engines) {
                // Comme le Gestionnaire des tâches : somme par type de moteur, puis le max.
                let tag = pid.map(|p| format!("pid_{p}_"));
                let mut all: HashMap<String, f64> = HashMap::new();
                let mut mine: HashMap<String, f64> = HashMap::new();
                for (name, v) in items {
                    let Some(i) = name.find("engtype_") else { continue };
                    let ty = name[i..].to_owned();
                    *all.entry(ty.clone()).or_default() += v;
                    if tag.as_ref().is_some_and(|t| name.starts_with(t.as_str())) {
                        *mine.entry(ty).or_default() += v;
                    }
                }
                let max = |m: &HashMap<String, f64>| m.values().copied().fold(0.0, f64::max).min(100.0) as f32;
                s.gpu_usage = Some(max(&all));
                s.gpu_process = Some(max(&mine));
            }
            if let Some(items) = read_array(self.memory) {
                let used = items.iter().map(|(_, v)| *v).fold(0.0, f64::max);
                s.vram_used = Some(used as u64);
            }
        }
    }

    impl Drop for Pdh {
        fn drop(&mut self) {
            unsafe {
                let _ = PdhCloseQuery(self.query);
            }
        }
    }

    fn read_array(counter: isize) -> Option<Vec<(String, f64)>> {
        unsafe {
            let mut size = 0u32;
            let mut count = 0u32;
            let _ = PdhGetFormattedCounterArrayW(counter, PDH_FMT_DOUBLE, &mut size, &mut count, None);
            if size == 0 {
                return None;
            }
            // Tampon aligné sur 8 octets pour les structures PDH.
            let mut buf = vec![0u64; (size as usize).div_ceil(8)];
            let items = buf.as_mut_ptr() as *mut PDH_FMT_COUNTERVALUE_ITEM_W;
            if PdhGetFormattedCounterArrayW(counter, PDH_FMT_DOUBLE, &mut size, &mut count, Some(items)) != 0 {
                return None;
            }
            let slice = std::slice::from_raw_parts(items, count as usize);
            Some(
                slice
                    .iter()
                    .filter(|it| it.FmtValue.CStatus == 0)
                    .map(|it| (it.szName.to_string().unwrap_or_default(), it.FmtValue.Anonymous.doubleValue))
                    .collect(),
            )
        }
    }
}

// ------------------------------------------------------------------ Linux et autres

#[cfg(not(windows))]
mod gpu {
    use super::SysStats;

    pub struct GpuProbe;

    impl GpuProbe {
        pub fn new() -> Self {
            Self
        }

        pub fn sample(&mut self, _gpu_name: Option<&str>, _pid: Option<u32>, s: &mut SysStats) {
            #[cfg(target_os = "linux")]
            {
                let read = |card: &std::path::Path, f: &str| -> Option<u64> {
                    std::fs::read_to_string(card.join("device").join(f)).ok()?.trim().parse().ok()
                };
                let Ok(dir) = std::fs::read_dir("/sys/class/drm") else { return };
                for e in dir.flatten() {
                    let p = e.path();
                    let name = e.file_name();
                    let name = name.to_string_lossy();
                    if !name.starts_with("card") || name.contains('-') {
                        continue;
                    }
                    if let Some(busy) = read(&p, "gpu_busy_percent") {
                        s.gpu_usage = Some(busy as f32);
                        s.vram_used = read(&p, "mem_info_vram_used");
                        s.vram_total = read(&p, "mem_info_vram_total");
                        break;
                    }
                }
            }
            #[cfg(not(target_os = "linux"))]
            let _ = s;
        }
    }
}
