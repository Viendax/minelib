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
    /// Couteau qui touche un joueur.
    KnifeFlesh,
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
    /// Une cartouche glissée dans un fusil à pompe.
    Shell,
    /// Fin de rechargement, chien du R8 qui se lève.
    ReloadEnd,
    Hammer,
    /// Bruit de pas (sol dur), un des six enregistrements pris à tour de rôle (pied
    /// gauche, pied droit).
    Step,
    /// Réception après une chute.
    Land,
    /// Cri de douleur (chute qui blesse), un des quatre à tour de rôle.
    Pain,
    /// Plongeon dans l'eau.
    Splash,
    /// Porte ou trappe ouverte (`true`) ou fermée.
    Door(bool),
    /// Barreaux d'une échelle qu'on grimpe ou descend à pleine vitesse.
    Ladder,
    /// (v177) Bip de la bombe posée (de plus en plus rapproché), et son explosion.
    BombBeep,
    BombBlast,
}

macro_rules! sounds {
    ($($n:literal),* $(,)?) => {
        &[$(($n, include_bytes!(concat!("../assets/sounds/", $n, ".ogg")) as &[u8])),*]
    };
}

static FILES: &[(&str, &[u8])] = sounds!(
    "bomb_beep",
    "bomb_explode",
    "step_1",
    "step_2",
    "step_3",
    "step_4",
    "step_5",
    "step_6",
    "land",
    "splash",
    "door_open",
    "door_close",
    "ladder",
    "pain_1",
    "pain_2",
    "pain_3",
    "pain_4",
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
    "knife_flesh",
    "shell",
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
        // Les tirs dominent nettement les pas (comme dans CS).
        Sfx::Shot(i) => (shot_file(WEAPONS[i].name), 1.0),
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
        Sfx::KnifeFlesh => ("knife_flesh", 0.9),
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
        Sfx::Shell => ("shell", 0.7),
        Sfx::Step => {
            // Jamais deux fois le même pas de suite.
            static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            const STEPS: [&str; 6] = ["step_1", "step_2", "step_3", "step_4", "step_5", "step_6"];
            let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            (STEPS[n % 6], 0.2)
        }
        Sfx::Land => ("land", 0.5),
        Sfx::Splash => ("splash", 0.8),
        Sfx::Door(true) => ("door_open", 0.8),
        Sfx::Door(false) => ("door_close", 0.8),
        Sfx::Ladder => ("ladder", 0.7),
        Sfx::BombBeep => ("bomb_beep", 0.35),
        Sfx::BombBlast => ("bomb_explode", 1.0),
        Sfx::Pain => {
            static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            const PAINS: [&str; 4] = ["pain_1", "pain_2", "pain_3", "pain_4"];
            let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            (PAINS[n % 4], 0.8)
        }
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
    /// Chat vocal : (joueur, échantillons 16 kHz mono) au lieu d'un son du jeu.
    voice: Option<(u64, Vec<i16>)>,
    /// La sortie son choisie a changé : elle est rouverte.
    reopen: bool,
}

/// Sortie son choisie (nom du périphérique ; vide : celle par défaut du système).
static OUTPUT: Mutex<String> = Mutex::new(String::new());

/// Sorties son du système (noms des périphériques), dans l'ordre où il les donne.
pub fn output_devices() -> Vec<String> {
    use rodio::cpal::traits::{DeviceTrait, HostTrait};
    rodio::cpal::default_host()
        .output_devices()
        .map(|it| it.filter_map(|d| d.name().ok()).collect())
        .unwrap_or_default()
}

/// Choisit la sortie son (vide : celle par défaut du système) : les sons y partent dès
/// maintenant.
pub fn set_output(name: &str) {
    let changed = OUTPUT
        .lock()
        .map(|mut o| {
            let changed = *o != name;
            *o = name.to_string();
            changed
        })
        .unwrap_or(false);
    if !changed {
        return;
    }
    if let Some(tx) = sender() {
        if let Ok(tx) = tx.lock() {
            let _ = tx.send(Cmd {
                file: "",
                volume: 0.0,
                delay: 0.0,
                voice: None,
                reopen: true,
            });
        }
    }
}

/// Ouvre la sortie choisie, sinon celle par défaut.
fn open_output() -> Option<(OutputStream, rodio::OutputStreamHandle)> {
    use rodio::cpal::traits::{DeviceTrait, HostTrait};
    let name = OUTPUT.lock().map(|o| o.clone()).unwrap_or_default();
    if !name.is_empty() {
        let dev = rodio::cpal::default_host()
            .output_devices()
            .ok()
            .and_then(|mut it| it.find(|d| d.name().ok().as_deref() == Some(name.as_str())));
        match dev.map(|d| OutputStream::try_from_device(&d)) {
            Some(Ok(out)) => {
                log::info!("audio : sortie « {name} »");
                return Some(out);
            }
            Some(Err(e)) => {
                log::warn!("audio : sortie « {name} » impossible à ouvrir ({e}), celle par défaut")
            }
            None => log::warn!("audio : sortie « {name} » introuvable, celle par défaut"),
        }
    }
    match OutputStream::try_default() {
        Ok(out) => Some(out),
        Err(_) => {
            log::warn!("audio : aucune sortie son, jeu sans son");
            None
        }
    }
}

fn sender() -> Option<&'static Mutex<Sender<Cmd>>> {
    static TX: OnceLock<Option<Mutex<Sender<Cmd>>>> = OnceLock::new();
    TX.get_or_init(|| {
        let (tx, rx) = channel::<Cmd>();
        let spawned = std::thread::Builder::new()
            .name("audio".into())
            .spawn(move || {
                let mut out = open_output();
                let mut cache: HashMap<
                    &'static str,
                    rodio::source::Buffered<Decoder<Cursor<&'static [u8]>>>,
                > = HashMap::new();
                let mut sinks: Vec<Sink> = Vec::new();
                // Une file par joueur qui parle.
                let mut voices: HashMap<u64, Sink> = HashMap::new();
                while let Ok(cmd) = rx.recv() {
                    if cmd.reopen {
                        // Les sons en cours étaient liés à l'ancienne sortie.
                        sinks.clear();
                        voices.clear();
                        // (L'ancienne fermée d'abord : un périphérique peut ne s'ouvrir
                        // qu'une fois.)
                        drop(out.take());
                        out = open_output();
                        continue;
                    }
                    let Some((_, handle)) = &out else {
                        continue;
                    };
                    sinks.retain(|s| !s.empty());
                    if let Some((who, samples)) = cmd.voice {
                        let Some(sink) = (match voices.entry(who) {
                            std::collections::hash_map::Entry::Occupied(e) => Some(e.into_mut()),
                            std::collections::hash_map::Entry::Vacant(e) => {
                                Sink::try_new(handle).ok().map(|s| e.insert(s))
                            }
                        }) else {
                            continue;
                        };
                        sink.set_volume(cmd.volume);
                        // Trop de retard accumulé : on repart au présent.
                        if sink.len() > 8 {
                            sink.clear();
                            sink.play();
                        }
                        sink.append(rodio::buffer::SamplesBuffer::new(
                            1,
                            minelib_net::voice::RATE,
                            samples,
                        ));
                        continue;
                    }
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
                    if let Ok(sink) = Sink::try_new(handle) {
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
                voice: None,
                reopen: false,
            });
        }
    }
}

/// Joue un paquet de voix d'un joueur (`volume` : réglage du joueur × distance).
pub fn voice(who: u64, samples: Vec<i16>, volume: f32) {
    if let Some(tx) = sender() {
        if let Ok(tx) = tx.lock() {
            let _ = tx.send(Cmd {
                file: "",
                volume,
                delay: 0.0,
                voice: Some((who, samples)),
                reopen: false,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// (v195) Le plongeon part dès qu'on touche l'eau : au-dessus du dixième de sa crête
    /// (−20 dB) dans les 20 premières millisecondes, son gros (−12 dB) dans les 60. Avant,
    /// le gros du plouf n'arrivait qu'à 0,19 s : entendu 0,2 s trop tard.
    #[test]
    fn the_splash_starts_at_once() {
        let bytes = FILES.iter().find(|x| x.0 == "splash").unwrap().1;
        let dec = Decoder::new(Cursor::new(bytes)).unwrap();
        let per_ms = dec.sample_rate() as usize * dec.channels() as usize / 1000;
        let s: Vec<f32> = dec.map(f32::from).collect();
        let peak = s.iter().fold(0.0f32, |m, x| m.max(x.abs()));
        let first_ms = |db: f32| {
            let level = peak * 10f32.powf(db / 20.0);
            s.chunks(per_ms * 5)
                .position(|c| {
                    (c.iter().map(|x| x * x).sum::<f32>() / c.len() as f32).sqrt() > level
                })
                .map(|k| k * 5)
        };
        assert!(
            first_ms(-20.0).is_some_and(|t| t <= 20),
            "{:?}",
            first_ms(-20.0)
        );
        assert!(
            first_ms(-12.0).is_some_and(|t| t <= 60),
            "{:?}",
            first_ms(-12.0)
        );
    }

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
            Sfx::BombBeep,
            Sfx::BombBlast,
        ] {
            let (f, _) = file_of(s);
            assert!(FILES.iter().any(|x| x.0 == f));
        }
    }
}
