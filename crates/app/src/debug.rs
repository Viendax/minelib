//! Contenu de l'écran de debug (F3).

use std::collections::VecDeque;

use minelib_platform::sysmon::SysStats;
use minelib_render::{FrameStats, Line, Overlay, Renderer};
use minelib_world::raycast::Hit;
use minelib_world::{ChunkPos, CHUNK_SIZE};

use crate::player::Player;
use crate::streaming::StreamStats;

/// Réglages et état du moteur affichés par F3.
pub struct EngineInfo {
    pub view_distance: i32,
    pub stream: StreamStats,
    pub workers: usize,
    pub fps_max: Option<f32>,
    pub fov: f32,
    pub world_name: String,
}

#[derive(Default, Clone, Copy)]
pub struct FrameSummary {
    pub fps: f32,
    pub avg_ms: f32,
    pub min_ms: f32,
    pub max_ms: f32,
}

/// Statistiques sur les durées d'image de la dernière seconde.
pub fn frame_summary(times: &VecDeque<f32>) -> FrameSummary {
    if times.is_empty() {
        return FrameSummary::default();
    }
    let total: f32 = times.iter().sum();
    let min = times.iter().copied().fold(f32::MAX, f32::min);
    let max = times.iter().copied().fold(0.0, f32::max);
    FrameSummary {
        fps: times.len() as f32 / total.max(1e-6),
        avg_ms: total / times.len() as f32 * 1000.0,
        min_ms: min * 1000.0,
        max_ms: max * 1000.0,
    }
}

pub struct DebugInfo<'a> {
    pub frames: FrameSummary,
    pub player: &'a Player,
    pub hit: Option<Hit>,
    pub seed: u32,
    pub chunks_loaded: usize,
    pub stats: FrameStats,
    pub held: String,
    pub torch_info: Option<String>,
    /// Mode FPS : affiche la fatigue des jambes.
    pub fps: bool,
    pub torches: usize,
    pub sys: SysStats,
    pub renderer: &'a Renderer,
    pub engine: EngineInfo,
}

fn mo(bytes: u64) -> String {
    format!("{:.0} Mo", bytes as f64 / (1024.0 * 1024.0))
}

fn go(bytes: u64) -> String {
    format!("{:.1} Go", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
}

fn usage_color(pct: f32) -> [f32; 4] {
    if pct < 70.0 {
        Line::GOOD
    } else if pct < 90.0 {
        Line::WARN
    } else {
        Line::BAD
    }
}

fn facing(yaw: f32) -> &'static str {
    // lacet 0 = regard vers -Z (nord), 90° = +X (est).
    let deg = yaw.to_degrees().rem_euclid(360.0);
    match ((deg + 45.0) / 90.0) as u32 % 4 {
        0 => "nord (-Z)",
        1 => "est (+X)",
        2 => "sud (+Z)",
        _ => "ouest (-X)",
    }
}

pub fn build(d: &DebugInfo) -> Overlay {
    let f = d.frames;
    let p = d.player;
    let pos = p.pos;
    let (bx, by, bz) = (
        pos.x.floor() as i32,
        pos.y.floor() as i32,
        pos.z.floor() as i32,
    );
    let chunk = ChunkPos::containing(bx, bz);
    let cs = CHUNK_SIZE as i32;
    let fps_color = if f.fps >= 55.0 {
        Line::GOOD
    } else if f.fps >= 30.0 {
        Line::WARN
    } else {
        Line::BAD
    };
    let speed = (p.vel.x * p.vel.x + p.vel.z * p.vel.z).sqrt();
    let mode = if p.flying {
        "en vol"
    } else if p.on_ground {
        "au sol"
    } else {
        "en l'air"
    };

    let mut left = vec![
        Line::new(
            format!("Minelib {} (Rust + wgpu)", env!("CARGO_PKG_VERSION")),
            Line::TITLE,
        ),
        Line::new(
            format!(
                "{:.0} FPS   {:.2} ms  (min {:.2} / max {:.2})",
                f.fps, f.avg_ms, f.min_ms, f.max_ms
            ),
            fps_color,
        ),
        Line::white(""),
        Line::white(format!("XYZ : {:.3} / {:.3} / {:.3}", pos.x, pos.y, pos.z)),
        Line::white(format!("Bloc : {bx} {by} {bz}")),
        Line::white(format!(
            "Chunk : {} {}   (dans le chunk : {} {})",
            chunk.x,
            chunk.z,
            bx.rem_euclid(cs),
            bz.rem_euclid(cs)
        )),
        Line::white(format!(
            "Direction : {}   lacet {:.1}°  tangage {:.1}°",
            facing(p.yaw),
            p.yaw.to_degrees(),
            p.pitch.to_degrees()
        )),
        Line::white(format!(
            "Vitesse : {speed:.2} m/s  vertical {:.2} m/s  ({mode})",
            p.vel.y
        )),
        Line::white(""),
    ];
    if d.fps {
        let p = d.player;
        left.insert(
            left.len() - 1,
            Line::new(
                format!(
                    "Jambes : fatigue {:.0} / {:.0}   accroupi {:.1} / {:.0}",
                    p.stamina,
                    crate::player::cs::STAMINA_MAX,
                    p.duck_speed,
                    crate::player::cs::DUCK_SPEED_MAX
                ),
                if p.stamina > 0.0 {
                    Line::WARN
                } else {
                    Line::WHITE
                },
            ),
        );
    }
    // Une seule ligne sur le bloc visé (à portée), avec la durée des torches.
    match d.hit {
        Some(h) => {
            let face = match h.normal {
                [1, 0, 0] => "est",
                [-1, 0, 0] => "ouest",
                [0, 1, 0] => "dessus",
                [0, -1, 0] => "dessous",
                [0, 0, 1] => "sud",
                _ => "nord",
            };
            let mut text = format!(
                "Bloc visé : {} (#{}) en {} {} {}, face {face}, {:.1} m",
                h.id.name(),
                h.id.0,
                h.block[0],
                h.block[1],
                h.block[2],
                h.distance
            );
            let color = match &d.torch_info {
                Some(t) => {
                    text.push_str(" | ");
                    text.push_str(t);
                    Line::TITLE
                }
                None => Line::WHITE,
            };
            left.push(Line::new(text, color));
        }
        None => left.push(Line::new("Bloc visé : aucun (hors de portée)", Line::DIM)),
    }
    left.push(Line::white(format!("En main : {}", d.held)));
    left.push(Line::white(format!("Torches posées : {}", d.torches)));
    left.push(Line::white(""));
    let e = &d.engine;
    let st = &d.stats;
    left.push(Line::white(format!(
        "Chunks : {} dessinés / {} maillés / {} en mémoire",
        st.chunks_drawn, st.chunks_total, d.chunks_loaded
    )));
    left.push(Line::white(format!("Triangles : {}k", st.triangles / 1000)));
    left.push(Line::white(format!(
        "Distance de rendu : {} chunks ({} chunks voulus)",
        e.view_distance, e.stream.wanted_chunks
    )));
    left.push(Line::white(format!(
        "Threads : {} workers   {} tâches en cours",
        e.workers, e.stream.pending_jobs
    )));
    left.push(Line::new(
        format!(
            "Monde « {} »   graine {}   FOV {:.0}°",
            e.world_name, d.seed, e.fov
        ),
        Line::DIM,
    ));
    left.push(Line::new(
        format!("Pack de textures : {}", d.renderer.pack_name),
        Line::DIM,
    ));

    let s = &d.sys;
    let r = d.renderer;
    let na = || "n/d".to_string();
    let (w, h) = r.size();

    let mut right = vec![
        Line::new("Système", Line::TITLE),
        Line::new(
            format!(
                "CPU : processus {:.1} %   système {:.1} %",
                s.cpu_process, s.cpu_system
            ),
            usage_color(s.cpu_system),
        ),
        Line::new(
            format!("{}  ({} threads)", s.cpu_name, s.cpu_threads),
            Line::DIM,
        ),
        Line::white(format!(
            "RAM : processus {}   système {} / {}",
            mo(s.ram_process),
            go(s.ram_used),
            go(s.ram_total)
        )),
        Line::white(""),
        Line::new(
            format!("GPU : {} ({})", r.adapter_name, r.backend),
            Line::DIM,
        ),
    ];
    right.push(match (s.gpu_usage, s.gpu_process) {
        (Some(all), Some(me)) => Line::new(
            format!("GPU : {all:.0} %   (processus {me:.0} %)"),
            usage_color(all),
        ),
        (Some(all), None) => Line::new(format!("GPU : {all:.0} %"), usage_color(all)),
        _ => Line::new("GPU : n/d sur ce système", Line::DIM),
    });
    let vram_line = match (s.vram_process, s.vram_used, s.vram_total) {
        (None, None, None) => Line::new("VRAM : n/d sur ce système", Line::DIM),
        (proc_, used, total) => Line::white(format!(
            "VRAM : processus {}   utilisée {} / {}",
            proc_.map(mo).unwrap_or_else(na),
            used.map(go).unwrap_or_else(na),
            total.map(go).unwrap_or_else(na)
        )),
    };
    right.push(vram_line);
    if let Some(budget) = s.vram_budget {
        right.push(Line::new(
            format!("VRAM budget du processus : {}", go(budget)),
            Line::DIM,
        ));
    }
    right.push(Line::white(format!(
        "VRAM moteur : {}   (chunks {})",
        mo(r.engine_gpu_bytes()),
        mo(r.chunk_gpu_bytes())
    )));
    right.push(Line::white(""));
    let limit = match e.fps_max {
        Some(m) => format!("plafond {m:.0} FPS"),
        None => "FPS illimités".to_string(),
    };
    right.push(Line::white(format!(
        "Écran : {w}x{h}   {limit}   anticrénelage : {}",
        if r.antialiasing() { "TAA" } else { "non" }
    )));
    right.push(Line::white(format!(
        "Échelle de rendu : {:.0} %",
        r.render_scale() * 100.0
    )));
    if r.raytracing() && r.async_reflections() {
        let (fps, target) = r.raytracing_fps();
        right.push(Line::white(format!(
            "Raytracing : {fps:.0} images/s (visé {target:.0}, à part du rendu)"
        )));
    } else if r.raytracing() {
        right.push(Line::white("Raytracing : à chaque image (synchronisé)"));
    } else {
        right.push(Line::white("Raytracing : désactivé"));
    }
    if let Some((kind, on)) = r.low_latency() {
        right.push(Line::white(format!(
            "{} : {}",
            kind.label(),
            match (on, r.low_latency_waiting()) {
                (false, _) => "désactivé",
                (true, true) => "activé",
                // Images très courtes : l'attente du pilote coûterait plus qu'elle ne rapporte.
                (true, false) => "activé (au repos : plus de 250 FPS)",
            }
        )));
    }

    Overlay { left, right }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn facing_directions() {
        assert_eq!(facing(0.0), "nord (-Z)");
        assert_eq!(facing(std::f32::consts::FRAC_PI_2), "est (+X)");
        assert_eq!(facing(std::f32::consts::PI), "sud (+Z)");
        assert_eq!(facing(-std::f32::consts::FRAC_PI_2), "ouest (-X)");
    }

    #[test]
    fn frame_summary_stats() {
        let t: VecDeque<f32> = [0.010, 0.020, 0.010, 0.020].into_iter().collect();
        let s = frame_summary(&t);
        assert!((s.fps - 66.67).abs() < 0.1);
        assert!((s.min_ms - 10.0).abs() < 1e-3);
        assert!((s.max_ms - 20.0).abs() < 1e-3);
    }
}
