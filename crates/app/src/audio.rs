//! Sons du jeu (mode FPS) : tirs, rechargements, lunette. Enregistrements réels de la
//! « Free Firearm Sound Library » et bruitages d'OpenGameArt, tous sous licence CC0 (voir
//! `assets/sounds/CREDITS.txt`), découpés et convertis en Ogg Vorbis mono.
//!
//! La sortie audio vit dans son propre fil : `play` ne bloque jamais le jeu, et sans carte
//! son le jeu continue en silence.

use std::collections::HashMap;
use std::io::Cursor;
use std::sync::mpsc::{channel, Sender};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use rodio::{Decoder, OutputStream, Sink, Source};

use crate::weapons::{Category, WEAPONS};

/// Un son à jouer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Sfx {
    /// Tir de l'arme `i` (indice dans `WEAPONS`).
    Shot(usize),
    /// Début du rechargement de l'arme `i`.
    Reload(usize),
    /// Culasse ou pompe réarmée après le tir (AWP, SSG 08, fusils à pompe).
    Cycle,
    ScopeIn,
    ScopeOut,
    /// Détente sur chargeur vide.
    Dry,
    /// Couteau : coup dans le vide, coup sur un bloc.
    KnifeSwing,
    KnifeHit,
    /// Grenades : goupille, lancer, rebond, explosion, flash, fumée, verre du molotov, feu,
    /// faux tir du leurre.
    Pin,
    Throw,
    Bounce,
    He,
    Flash,
    Smoke,
    Molotov,
    Fire,
    Decoy,
    /// Arme `i` sortie (son propre à chaque arme), couteau sorti, grenade sortie.
    Draw(usize),
    DrawKnife,
    DrawNade,
    /// Fin de rechargement, chien du R8 qui se lève.
    ReloadEnd,
    Hammer,
}

macro_rules! sounds {
    ($($n:literal),* $(,)?) => {
        &[$(($n, include_bytes!(concat!("../assets/sounds/", $n, ".ogg")) as &[u8])),*]
    };
}

static FILES: &[(&str, &[u8])] = sounds!(
    "shot_glock",
    "shot_usp",
    "shot_p2000",
    "shot_dualies",
    "shot_p250",
    "shot_fiveseven",
    "shot_tec9",
    "shot_cz75",
    "shot_deagle",
    "shot_r8",
    "shot_mac10",
    "shot_mp9",
    "shot_mp7",
    "shot_mp5sd",
    "shot_ump",
    "shot_p90",
    "shot_bizon",
    "shot_nova",
    "shot_xm1014",
    "shot_sawedoff",
    "shot_mag7",
    "shot_m249",
    "shot_negev",
    "shot_galil",
    "shot_famas",
    "shot_ak47",
    "shot_m4a4",
    "shot_m4a1s",
    "shot_sg553",
    "shot_aug",
    "shot_ssg08",
    "shot_awp",
    "shot_g3sg1",
    "shot_scar20",
    "reload_pistol",
    "reload_rifle",
    "reload_sniper",
    "pump",
    "draw_glock",
    "draw_usp",
    "draw_p2000",
    "draw_dualies",
    "draw_p250",
    "draw_fiveseven",
    "draw_tec9",
    "draw_cz75",
    "draw_deagle",
    "draw_r8",
    "draw_mac10",
    "draw_mp9",
    "draw_mp7",
    "draw_mp5sd",
    "draw_ump",
    "draw_p90",
    "draw_bizon",
    "draw_nova",
    "draw_xm1014",
    "draw_sawedoff",
    "draw_mag7",
    "draw_m249",
    "draw_negev",
    "draw_galil",
    "draw_famas",
    "draw_ak47",
    "draw_m4a4",
    "draw_m4a1s",
    "draw_sg553",
    "draw_aug",
    "draw_ssg08",
    "draw_awp",
    "draw_g3sg1",
    "draw_scar20",
    "draw_knife",
    "draw_nade",
    "knife_swing2",
    "scope_in",
    "scope_out",
    "dry",
    "knife_swing",
    "knife_hit",
    "pin",
    "throw",
    "bounce",
    "he",
    "flash",
    "smoke",
    "molotov",
    "fire",
    "reload_end",
    "hammer",
);

/// Nom de fichier du son de tir d'une arme.
fn shot_file(name: &str) -> &'static str {
    match name {
        "Glock-18" => "shot_glock",
        "USP-S" => "shot_usp",
        "P2000" => "shot_p2000",
        "Dual Berettas" => "shot_dualies",
        "P250" => "shot_p250",
        "Five-SeveN" => "shot_fiveseven",
        "Tec-9" => "shot_tec9",
        "CZ75-Auto" => "shot_cz75",
        "Desert Eagle" => "shot_deagle",
        "R8 Revolver" => "shot_r8",
        "MAC-10" => "shot_mac10",
        "MP9" => "shot_mp9",
        "MP7" => "shot_mp7",
        "MP5-SD" => "shot_mp5sd",
        "UMP-45" => "shot_ump",
        "P90" => "shot_p90",
        "PP-Bizon" => "shot_bizon",
        "Nova" => "shot_nova",
        "XM1014" => "shot_xm1014",
        "Sawed-Off" => "shot_sawedoff",
        "MAG-7" => "shot_mag7",
        "M249" => "shot_m249",
        "Negev" => "shot_negev",
        "Galil AR" => "shot_galil",
        "FAMAS" => "shot_famas",
        "AK-47" => "shot_ak47",
        "M4A4" => "shot_m4a4",
        "M4A1-S" => "shot_m4a1s",
        "SG 553" => "shot_sg553",
        "AUG" => "shot_aug",
        "SSG 08" => "shot_ssg08",
        "AWP" => "shot_awp",
        "G3SG1" => "shot_g3sg1",
        "SCAR-20" => "shot_scar20",
        _ => "shot_glock",
    }
}

/// Son de l'arme sortie : même nom que le tir, préfixe `draw_`.
fn draw_file(name: &str) -> &'static str {
    let shot = shot_file(name);
    FILES
        .iter()
        .find(|f| f.0.strip_prefix("draw_") == shot.strip_prefix("shot_"))
        .map_or("draw_glock", |f| f.0)
}

/// Fichier et volume d'un son.
fn file_of(sfx: Sfx) -> (&'static str, f32) {
    match sfx {
        Sfx::Shot(i) => (shot_file(WEAPONS[i].name), 0.55),
        Sfx::Reload(i) => {
            let d = &WEAPONS[i];
            let f = if d.category == Category::Pistol {
                "reload_pistol"
            } else if d.pellets > 1 {
                "pump"
            } else if d.scope.len() > 1 {
                "reload_sniper"
            } else {
                "reload_rifle"
            };
            (f, 0.7)
        }
        Sfx::Cycle => ("pump", 0.6),
        Sfx::ScopeIn => ("scope_in", 0.7),
        Sfx::ScopeOut => ("scope_out", 0.7),
        Sfx::Dry => ("dry", 0.6),
        Sfx::KnifeSwing => {
            // Deux coups dans le vide différents, en alternance.
            static ALT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
            let alt = ALT.fetch_xor(true, std::sync::atomic::Ordering::Relaxed);
            (if alt { "knife_swing2" } else { "knife_swing" }, 0.6)
        }
        Sfx::KnifeHit => ("knife_hit", 0.7),
        Sfx::Pin => ("pin", 0.6),
        Sfx::Throw => ("throw", 0.5),
        Sfx::Bounce => ("bounce", 0.5),
        Sfx::He => ("he", 1.0),
        Sfx::Flash => ("flash", 0.9),
        Sfx::Smoke => ("smoke", 0.5),
        Sfx::Molotov => ("molotov", 0.8),
        Sfx::Fire => ("fire", 0.45),
        Sfx::Decoy => ("shot_p2000", 0.5),
        Sfx::Draw(i) => (draw_file(WEAPONS[i].name), 0.6),
        Sfx::DrawKnife => ("draw_knife", 0.7),
        Sfx::DrawNade => ("draw_nade", 0.6),
        Sfx::ReloadEnd => ("reload_end", 0.55),
        Sfx::Hammer => ("hammer", 0.6),
    }
}

/// Après le tir, l'arme réarme-t-elle à la main (culasse, pompe) ?
pub fn cycles_after_shot(i: usize) -> bool {
    matches!(WEAPONS[i].name, "AWP" | "SSG 08" | "Nova" | "Sawed-Off")
}

struct Cmd {
    file: &'static str,
    volume: f32,
    delay: f32,
}

fn sender() -> Option<&'static Mutex<Sender<Cmd>>> {
    static TX: OnceLock<Option<Mutex<Sender<Cmd>>>> = OnceLock::new();
    TX.get_or_init(|| {
        let (tx, rx) = channel::<Cmd>();
        let spawned = std::thread::Builder::new()
            .name("audio".into())
            .spawn(move || {
                let Ok((_stream, handle)) = OutputStream::try_default() else {
                    log::warn!("audio : aucune sortie son, jeu sans son");
                    return;
                };
                let mut cache: HashMap<
                    &'static str,
                    rodio::source::Buffered<Decoder<Cursor<&'static [u8]>>>,
                > = HashMap::new();
                let mut sinks: Vec<Sink> = Vec::new();
                while let Ok(cmd) = rx.recv() {
                    sinks.retain(|s| !s.empty());
                    let Some(src) = cache.get(cmd.file).cloned().or_else(|| {
                        let bytes = FILES.iter().find(|f| f.0 == cmd.file)?.1;
                        let dec = Decoder::new(Cursor::new(bytes)).ok()?.buffered();
                        cache.insert(cmd.file, dec.clone());
                        Some(dec)
                    }) else {
                        continue;
                    };
                    // Au plus 48 sons à la fois (rafales) : les plus anciens finissent.
                    if sinks.len() >= 48 {
                        sinks.remove(0);
                    }
                    if let Ok(sink) = Sink::try_new(&handle) {
                        sink.set_volume(cmd.volume);
                        sink.append(src.delay(Duration::from_secs_f32(cmd.delay.max(0.0))));
                        sinks.push(sink);
                    }
                }
            });
        spawned.ok().map(|_| Mutex::new(tx))
    })
    .as_ref()
}

/// Joue `sfx` après `delay` secondes, au volume général `master` (0..1).
pub fn play(sfx: Sfx, delay: f32, master: f32) {
    if master <= 0.0 {
        return;
    }
    let (file, volume) = file_of(sfx);
    if let Some(tx) = sender() {
        if let Ok(tx) = tx.lock() {
            let _ = tx.send(Cmd {
                file,
                volume: volume * master,
                delay,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_weapon_has_its_sounds() {
        for (i, d) in WEAPONS.iter().enumerate() {
            let (f, _) = file_of(Sfx::Shot(i));
            assert!(
                f != "shot_glock" || d.name == "Glock-18",
                "{} sans son",
                d.name
            );
            for s in [Sfx::Shot(i), Sfx::Reload(i), Sfx::Draw(i)] {
                let (f, _) = file_of(s);
                let bytes = FILES.iter().find(|x| x.0 == f).unwrap().1;
                assert!(Decoder::new(Cursor::new(bytes)).is_ok(), "{f} illisible");
            }
        }
        // Chaque arme a son propre son de sortie.
        let draws: std::collections::HashSet<&str> = (0..WEAPONS.len())
            .map(|i| file_of(Sfx::Draw(i)).0)
            .collect();
        assert_eq!(draws.len(), WEAPONS.len());
        for s in [
            Sfx::Cycle,
            Sfx::ScopeIn,
            Sfx::ScopeOut,
            Sfx::Dry,
            Sfx::KnifeSwing,
            Sfx::KnifeHit,
            Sfx::Pin,
            Sfx::Throw,
            Sfx::Bounce,
            Sfx::He,
            Sfx::Flash,
            Sfx::Smoke,
            Sfx::Molotov,
            Sfx::Fire,
            Sfx::Decoy,
            Sfx::Draw(0),
            Sfx::DrawKnife,
            Sfx::DrawNade,
            Sfx::ReloadEnd,
            Sfx::Hammer,
        ] {
            let (f, _) = file_of(s);
            assert!(FILES.iter().any(|x| x.0 == f));
        }
    }
}
