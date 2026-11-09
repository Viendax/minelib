//! Bilan des cartes graphiques au démarrage (en arrière-plan, dans le journal) : partie
//! graphique intégrée au processeur, écran branché sur quelle carte, débits mémoire, et ce
//! qu'il vaut mieux faire de chacune pour gagner des FPS.

use minelib_render::igpu::{self, IgpuReport};
use minelib_render::wgpu;

/// Lance le bilan dans un processus à part (aucune gêne pour le jeu : ni verrou partagé avec
/// sa carte graphique, ni saccade), une seule fois par machine : le résultat est gardé dans
/// `.minelib/logs/bilan_gpu.txt` (supprimer ce fichier pour le refaire).
pub fn spawn(render_gpu: String) {
    if std::env::var_os("MINELIB_NO_GPU_REPORT").is_some() {
        return;
    }
    let out = minelib_assets::data_dir().join("logs").join("bilan_gpu.txt");
    if out.exists() {
        return;
    }
    let Ok(exe) = std::env::current_exe() else { return };
    match std::process::Command::new(exe).arg("--bilan-gpu").arg(&render_gpu).spawn() {
        Ok(_) => log::info!("bilan des cartes graphiques lancé à part → {}", out.display()),
        Err(e) => log::warn!("bilan des cartes graphiques impossible : {e}"),
    }
}

/// Point d'entrée du processus de bilan (`minelib --bilan-gpu <carte du jeu>`).
pub fn run_process(render_gpu: &str) {
    // Sécurité : jamais plus de 30 s (pilote bloqué…).
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_secs(30));
        log::warn!("bilan interrompu (trop long)");
        std::process::exit(0);
    });
    // Laisse le jeu finir de démarrer.
    std::thread::sleep(std::time::Duration::from_secs(2));
    report(render_gpu);
}

fn gb(b: u64) -> f32 {
    b as f32 / (1024.0 * 1024.0 * 1024.0)
}

fn report(render_gpu: &str) {
    for (name, kind, backend) in igpu::adapters() {
        log::info!("carte vue par le rendu : {name} ({kind:?}, {backend:?})");
    }
    let dxgi = minelib_platform::gpu_adapters();
    for a in &dxgi {
        log::info!(
            "carte (Windows) : {} — {:.1} Go dédiés, {:.1} Go partagés, écran branché : {}",
            a.name,
            gb(a.dedicated),
            gb(a.shared),
            if a.has_display { "oui" } else { "non" }
        );
    }
    let main = dxgi.iter().find(|a| render_gpu.contains(&a.name) || a.name.contains(render_gpu));
    if let Some(m) = main {
        if !m.has_display && dxgi.iter().any(|a| a.has_display) {
            let shown = dxgi.iter().find(|a| a.has_display).map_or("?", |a| a.name.as_str());
            log::warn!(
                "affichage : l'écran est branché sur « {shown} », pas sur « {} ». Chaque image est recopiée \
                 d'une carte à l'autre (Optimus) : FPS plafonnés et latence en plus. Pour aller au-delà : mode \
                 « GPU dédié » / MUX dans le logiciel du portable (ou le panneau NVIDIA), ou un écran externe \
                 branché sur la prise reliée à la carte NVIDIA.",
                m.name
            );
        } else if m.has_display {
            log::info!("affichage : l'écran est branché directement sur « {} » (pas de recopie).", m.name);
        }
    }

    let ram = igpu::ram_copy_gbs();
    log::info!("mémoire vive : copie par le processeur {ram:.1} Go/s");
    let integrated = igpu::benchmark(wgpu::DeviceType::IntegratedGpu);
    let discrete = igpu::benchmark(wgpu::DeviceType::DiscreteGpu);
    let line = |r: &IgpuReport| {
        format!(
            "« {} » ({}) : calcul {:.0} GFLOPS, copie dans sa mémoire {:.1} Go/s, envoi depuis le processeur {:.1} Go/s",
            r.name, r.backend, r.gflops, r.copy_gbs, r.upload_gbs
        )
    };
    if let Some(d) = &discrete {
        log::info!("carte dédiée : {}", line(d));
    }
    let Some(i) = integrated else {
        log::info!("pas de partie graphique intégrée utilisable : rien à répartir");
        return;
    };
    log::info!("partie graphique intégrée : {}", line(&i));

    // Conclusions, chiffrées.
    let ratio = discrete.as_ref().map_or(0.0, |d| i.gflops / d.gflops.max(1.0));
    // Recopier une image 1080p HDR (8 octets par pixel) d'une carte à l'autre passe par la
    // mémoire vive : au moins deux transferts.
    let frame_mb = 1920.0 * 1080.0 * 8.0 / 1e6;
    let transfer_ms = 2.0 * frame_mb / 1e3 / i.upload_gbs.max(0.1) * 1e3;
    let power = if discrete.is_some() {
        format!("{:.0} % de la puissance de calcul de la carte dédiée", ratio * 100.0)
    } else {
        format!("{:.0} GFLOPS", i.gflops)
    };
    log::info!(
        "bilan iGPU : {power} ; recopier une image entre les deux cartes prendrait ≈ {transfer_ms:.2} ms \
         (1000 FPS = 1 ms par image en tout) → pas de rendu partagé image par image."
    );
    log::info!(
        "bilan mémoire : la mémoire de la partie intégrée EST la mémoire vive ({:.1} Go/s contre {ram:.1} Go/s par le \
         processeur){} → aucun gain à y placer des données ; tout ce qui sert à chaque image reste dans la mémoire \
         de la carte dédiée.",
        i.copy_gbs,
        discrete.as_ref().map_or(String::new(), |d| format!(", la mémoire de la carte dédiée fait {:.0} Go/s", d.copy_gbs))
    );
    log::info!(
        "bilan : partie intégrée laissée au repos pendant le jeu — sur un portable elle partage l'énergie et la \
         chaleur du processeur, la charger ferait baisser la fréquence du processeur qui prépare les images."
    );
}
