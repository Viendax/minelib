//! Mesure des performances, image par image (thread principal) : temps de chaque phase,
//! événements (chunks reçus, grille du raytracing refaite, sauvegarde…), temps GPU.
//!
//! - **image lente** : une image bien plus longue que les précédentes est écrite dans le
//!   journal avec le détail de ses phases et de ce qui s'est passé pendant celle-ci ;
//! - **bilan** toutes les 10 s : FPS moyen, 1 % le plus lent, pire image, phases et GPU.

use std::cell::RefCell;
use std::time::{Duration, Instant};

/// Temps GPU d'une image (ms) : scène, anticrénelage, affichage + interface.
#[derive(Clone, Copy, Debug, Default)]
pub struct GpuTimes {
    pub scene: f32,
    pub taa: f32,
    pub present: f32,
    /// Du début de la scène à la fin de l'affichage, temps morts entre passes compris.
    pub span: f32,
}

impl GpuTimes {
    pub fn total(&self) -> f32 {
        self.scene + self.taa + self.present
    }
}

#[derive(Default)]
struct Frame {
    phases: Vec<(&'static str, Duration)>,
    events: Vec<String>,
}

struct Perf {
    frame: Frame,
    /// Durées des images des ~2 dernières secondes, et leur somme (FPS moyen glissant).
    recent: std::collections::VecDeque<f32>,
    recent_sum: f32,
    // Bilan périodique.
    window_start: Instant,
    window_times: Vec<f32>,
    window_phases: Vec<(&'static str, Duration)>,
    window_gpu: (GpuTimes, u32),
    window_slow: u32,
    /// Journal des images lentes limité (pas plus de 5 par seconde).
    slow_logged: (Instant, u32),
    gpu_last: Option<GpuTimes>,
    /// Temps GPU de la scène lissé (ms), pour les réglages automatiques.
    gpu_smooth: Option<f32>,
    /// Temps « hors mesure » de la dernière image (ms).
    last_other_ms: f32,
    last_end: Instant,
    /// Compteurs du bilan (chunks maillés…), remis à zéro toutes les 10 s.
    window_counts: Vec<(&'static str, u64)>,
    /// Moyennes du bilan (somme, nombre).
    window_gauges: Vec<(&'static str, f64, u32)>,
}

impl Default for Perf {
    fn default() -> Self {
        Self {
            frame: Frame::default(),
            recent: std::collections::VecDeque::with_capacity(4096),
            recent_sum: 0.0,
            window_start: Instant::now(),
            window_times: Vec::with_capacity(4096),
            window_phases: Vec::new(),
            window_gpu: (GpuTimes::default(), 0),
            window_slow: 0,
            slow_logged: (Instant::now(), 0),
            gpu_last: None,
            gpu_smooth: None,
            last_other_ms: 0.0,
            last_end: Instant::now(),
            window_counts: Vec::new(),
            window_gauges: Vec::new(),
        }
    }
}

thread_local! {
    static PERF: RefCell<Perf> = RefCell::new(Perf::default());
}

/// Mesure une phase jusqu'à la fin de la portée (`let _p = perf::scope("…");`).
pub struct Scope {
    name: &'static str,
    start: Instant,
}

impl Drop for Scope {
    fn drop(&mut self) {
        let d = self.start.elapsed();
        add_phase(self.name, d);
    }
}

pub fn scope(name: &'static str) -> Scope {
    Scope {
        name,
        start: Instant::now(),
    }
}

pub fn add_phase(name: &'static str, d: Duration) {
    PERF.with(|p| {
        let mut p = p.borrow_mut();
        match p.frame.phases.iter_mut().find(|(n, _)| *n == name) {
            Some((_, t)) => *t += d,
            None => p.frame.phases.push((name, d)),
        }
    });
}

/// Ce qui s'est passé pendant l'image (affiché si elle est lente).
pub fn event(text: impl FnOnce() -> String) {
    PERF.with(|p| {
        let mut p = p.borrow_mut();
        if p.frame.events.len() < 16 {
            let t = text();
            p.frame.events.push(t);
        }
    });
}

/// Compte un événement pour le bilan (affiché par seconde).
pub fn count(name: &'static str, n: u64) {
    PERF.with(|p| {
        let mut p = p.borrow_mut();
        match p.window_counts.iter_mut().find(|(k, _)| *k == name) {
            Some((_, v)) => *v += n,
            None => p.window_counts.push((name, n)),
        }
    });
}

/// Valeur dont le bilan donne la moyenne (triangles par image…).
pub fn gauge(name: &'static str, v: f32) {
    PERF.with(|p| {
        let mut p = p.borrow_mut();
        match p.window_gauges.iter_mut().find(|(k, _, _)| *k == name) {
            Some((_, s, n)) => {
                *s += v as f64;
                *n += 1;
            }
            None => p.window_gauges.push((name, v as f64, 1)),
        }
    });
}

/// Temps « hors mesure » de la dernière image (ms) : temps perdu par le thread du jeu en
/// dehors de son travail (système, fenêtre, autres threads).
pub fn last_unmeasured_ms() -> f32 {
    PERF.with(|p| p.borrow().last_other_ms)
}

/// Temps GPU de la scène par image (ms), lissé sur quelques centaines d'images. Sans
/// l'anticrénelage ni l'affichage, dont le coût ne dépend pas de ce qui est dessiné.
pub fn gpu_scene_recent() -> Option<f32> {
    PERF.with(|p| p.borrow().gpu_smooth)
}

/// Temps GPU mesurés (avec quelques images de retard).
pub fn gpu(t: GpuTimes) {
    PERF.with(|p| {
        let mut p = p.borrow_mut();
        p.gpu_last = Some(t);
        p.gpu_smooth = Some(match p.gpu_smooth {
            Some(v) => v + (t.scene - v) * 0.02,
            None => t.scene,
        });
        p.window_gpu.0.scene += t.scene;
        p.window_gpu.0.taa += t.taa;
        p.window_gpu.0.present += t.present;
        p.window_gpu.0.span += t.span;
        p.window_gpu.1 += 1;
    });
}

fn fmt_phases(phases: &[(&'static str, Duration)], div: f32) -> String {
    let mut v: Vec<_> = phases
        .iter()
        .map(|(n, d)| (*n, d.as_secs_f32() * 1000.0 / div))
        .collect();
    v.sort_by(|a, b| b.1.total_cmp(&a.1));
    v.iter()
        .filter(|(_, ms)| *ms >= 0.01)
        .map(|(n, ms)| format!("{n} {ms:.2}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Fin d'image : mesure le temps réel depuis la fin de la précédente, journalise les
/// chutes et le bilan.
pub fn frame_end() {
    PERF.with(|p| {
        let mut p = p.borrow_mut();
        let p = &mut *p;
        let now = Instant::now();
        let dt = now.duration_since(p.last_end).as_secs_f32();
        p.last_end = now;
        let mut frame = std::mem::take(&mut p.frame);
        // Temps hors des phases mesurées (fenêtre, événements, système).
        let measured: Duration = frame.phases.iter().map(|(_, d)| *d).sum();
        let other = Duration::from_secs_f32(dt).saturating_sub(measured);
        p.last_other_ms = other.as_secs_f32() * 1000.0;
        frame.phases.push(("hors mesure (fenêtre, système)", other));

        // Durée moyenne d'une image sur les ~2 dernières secondes (avant celle-ci).
        let avg = if p.recent.is_empty() { dt } else { p.recent_sum / p.recent.len() as f32 };
        let warm = p.recent_sum > 1.0;
        p.recent.push_back(dt);
        p.recent_sum += dt;
        while p.recent_sum > 2.0 && p.recent.len() > 1 {
            p.recent_sum -= p.recent.pop_front().unwrap_or(0.0);
        }

        // Chute : image au moins 2 × plus longue que la moyenne et 2 ms de plus.
        let slow = warm && dt > (avg * 2.0).max(avg + 0.002);
        if slow {
            p.window_slow += 1;
            let now = Instant::now();
            if now.duration_since(p.slow_logged.0) > Duration::from_secs(1) {
                p.slow_logged = (now, 0);
            }
            if p.slow_logged.1 < 5 {
                p.slow_logged.1 += 1;
                let gpu = p.gpu_last.map_or(String::new(), |g| {
                    format!(" | GPU (dernière mesure) : scène {:.2}, TAA {:.2}, affichage {:.2} ms", g.scene, g.taa, g.present)
                });
                let ev = if frame.events.is_empty() { "aucun".to_string() } else { frame.events.join(" ; ") };
                log::warn!(
                    "chute : {:.1} ms soit {:.0} FPS (moyenne {:.0} FPS, {:.2} ms) | CPU : {}{gpu} | événements : {ev}",
                    dt * 1000.0,
                    1.0 / dt.max(1e-6),
                    1.0 / avg.max(1e-6),
                    avg * 1000.0,
                    fmt_phases(&frame.phases, 1.0),
                );
            }
        }

        // Bilan toutes les 10 s.
        p.window_times.push(dt);
        for (n, d) in &frame.phases {
            match p.window_phases.iter_mut().find(|(m, _)| m == n) {
                Some((_, t)) => *t += *d,
                None => p.window_phases.push((n, *d)),
            }
        }
        if p.window_start.elapsed() >= Duration::from_secs(10) && !p.window_times.is_empty() {
            let n = p.window_times.len() as f32;
            let total: f32 = p.window_times.iter().sum();
            let mut sorted = p.window_times.clone();
            sorted.sort_by(|a, b| b.total_cmp(a));
            let worst1 = &sorted[..(sorted.len() / 100).max(1)];
            let low1 = worst1.len() as f32 / worst1.iter().sum::<f32>().max(1e-6);
            let gpu = if p.window_gpu.1 > 0 {
                let k = p.window_gpu.1 as f32;
                let g = p.window_gpu.0;
                format!(" | GPU moyen : scène {:.2}, TAA {:.2}, affichage {:.2} ms (du début à la fin : {:.2})", g.scene / k, g.taa / k, g.present / k, g.span / k)
            } else {
                " | GPU : mesure indisponible".to_string()
            };
            let secs = p.window_start.elapsed().as_secs_f32().max(1e-3);
            let counts = if p.window_counts.is_empty() {
                String::new()
            } else {
                let c: Vec<String> = p.window_counts.iter().map(|(k, v)| format!("{k} {:.1}/s", *v as f32 / secs)).collect();
                format!(" | {}", c.join(", "))
            };
            p.window_counts.clear();
            let gauges: String = p
                .window_gauges
                .iter()
                .map(|(k, s, n)| format!(" | {k} : {:.0}", s / (*n).max(1) as f64))
                .collect();
            let counts = counts + &gauges;
            p.window_gauges.clear();
            log::info!(
                "bilan 10 s : {:.0} FPS moyen, 1 % bas {:.0} FPS, pire image {:.1} ms, {} chutes | CPU moyen (ms) : {}{gpu}{counts}",
                n / total.max(1e-6),
                low1,
                sorted[0] * 1000.0,
                p.window_slow,
                fmt_phases(&p.window_phases, n),
            );
            p.window_start = Instant::now();
            p.window_times.clear();
            p.window_phases.clear();
            p.window_gpu = (GpuTimes::default(), 0);
            p.window_slow = 0;
        }
    });
}
