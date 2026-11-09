//! Répartition simple du travail selon les cartes graphiques présentes.
//!
//! | carte qui dessine        | rendu + reflets | reflets (raytracing) | échelle de rendu (1er lancement) |
//! |--------------------------|-----------------|----------------------|----------------------------------|
//! | dédiée (NVIDIA, AMD…)    | elle            | 144 fois/s           | 100 %                            |
//! | intégrée au processeur   | elle            | 30 fois/s            | 75 %                             |
//! | rendu logiciel           | processeur      | 10 fois/s            | 50 %                             |
//!
//! Une partie graphique intégrée qui s'ajoute à une carte dédiée reste au repos : partager
//! une image entre les deux coûte plus que ce qu'elle rapporte, et sa mémoire est la mémoire
//! vive (voir `gpu_report`). Le processeur (voir `jobs`) : par défaut tous les cœurs font
//! tourner les tâches de fond à la priorité la plus basse, et le thread du jeu et du rendu
//! a une priorité haute : chaque cœur sert d'abord au jeu, le reste de son temps au monde.
//! Option « cœurs réservés » : 1 cœur jeu, 1 pilote, les autres pour les tâches de fond.

use minelib_assets::Options;
use minelib_render::wgpu::DeviceType;
use minelib_render::Renderer;

/// Applique la répartition ; `first_launch` : pas encore d'options enregistrées, on choisit
/// aussi l'échelle de rendu.
pub fn apply(r: &mut Renderer, options: &mut Options, first_launch: bool) {
    let (role, rate, scale) = match r.device_type {
        DeviceType::DiscreteGpu => ("carte dédiée", 144.0, 1.0),
        DeviceType::IntegratedGpu => ("partie graphique intégrée", 30.0, 0.75),
        DeviceType::VirtualGpu => ("carte virtuelle", 60.0, 1.0),
        _ => ("rendu logiciel (processeur)", 10.0, 0.5),
    };
    r.set_raytracing_rate(rate);
    if first_launch {
        options.render_scale = scale;
        r.set_render_scale(scale);
    }
    let cores = std::thread::available_parallelism().map_or(2, |n| n.get());
    let cpu = if crate::jobs::uniform_sharing() {
        format!(
            "{cores} cœurs, tous pour le jeu en priorité et {} tâches de fond à basse priorité",
            crate::jobs::worker_count(cores)
        )
    } else {
        format!(
            "{cores} cœurs (1 jeu + rendu, 1 pilote, {} tâches de fond)",
            crate::jobs::worker_count(cores)
        )
    };
    log::info!(
        "répartition : rendu et reflets sur « {} » ({role}), reflets à chaque image (mode décalé : {rate:.0} fois/s), échelle de rendu {:.0} % ; \
         processeur : {cpu}",
        r.adapter_name,
        r.render_scale() * 100.0,
    );
}
