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
    /// (v216) Son calculé (tonnerre) : échantillons mono et leur cadence, au lieu d'un
    /// fichier.
    pcm: Option<(Vec<i16>, u32)>,
    /// (v216) Volume de la pluie (son de fond en boucle ; 0 : silence).
    rain: Option<f32>,
}

/// (v215) Volume du jeu (Paramètres → Son), à part du volume de chaque son : changé, il
/// vaut tout de suite pour les sons en cours aussi (feu, bips de la bombe), comme le volume
/// des écrans pour leurs vidéos. Bits d'un `f32`.
static MASTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0x3f19_999a);

fn master() -> f32 {
    f32::from_bits(MASTER.load(std::sync::atomic::Ordering::Relaxed))
}

/// (v215) Volume du jeu (0 à 1) : les sons en cours le prennent tout de suite.
pub fn set_master(v: f32) {
    let v = v.clamp(0.0, 1.0);
    let old = MASTER.swap(v.to_bits(), std::sync::atomic::Ordering::Relaxed);
    if old == v.to_bits() {
        return;
    }
    if let Some(tx) = sender() {
        if let Ok(tx) = tx.lock() {
            // (Commande vide : le fil du son redonne le volume à ses sons en cours.)
            let _ = tx.send(Cmd {
                file: "",
                volume: 0.0,
                delay: 0.0,
                voice: None,
                reopen: false,
                pcm: None,
                rain: None,
            });
        }
    }
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
                pcm: None,
                rain: None,
            });
        }
    }
}

/// (v215) Windows ne baisse plus le son du jeu pendant une communication (voir
/// `minelib_platform::audio_no_ducking`) : « quand je ne suis pas dans le jeu, le son se
/// réduit lentement, et quand je reviens il réaugmente ».
fn no_ducking() {
    match minelib_platform::audio_no_ducking() {
        Ok(()) => log::info!("audio : son du jeu jamais baissé par Windows (communications)"),
        Err(e) => log::warn!("audio : baisse du son par Windows pas désactivée ({e})"),
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
                no_ducking();
                let mut cache: HashMap<
                    &'static str,
                    rodio::source::Buffered<Decoder<Cursor<&'static [u8]>>>,
                > = HashMap::new();
                // Chaque son avec son volume propre (sans le volume du jeu, `master`).
                let mut sinks: Vec<(Sink, f32)> = Vec::new();
                // Une file par joueur qui parle.
                let mut voices: HashMap<u64, Sink> = HashMap::new();
                // (v216) La pluie : une boucle, créée à la première averse, et son volume
                // propre (sans celui du jeu).
                let mut rain: Option<(Sink, f32)> = None;
                while let Ok(cmd) = rx.recv() {
                    if cmd.reopen {
                        // Les sons en cours étaient liés à l'ancienne sortie.
                        sinks.clear();
                        voices.clear();
                        rain = None;
                        // (L'ancienne fermée d'abord : un périphérique peut ne s'ouvrir
                        // qu'une fois.)
                        drop(out.take());
                        out = open_output();
                        no_ducking();
                        continue;
                    }
                    let Some((_, handle)) = &out else {
                        continue;
                    };
                    sinks.retain(|s| !s.0.empty());
                    // (v216) Pluie : son de fond en boucle, à son volume (× celui du jeu).
                    if let Some(volume) = cmd.rain {
                        if volume <= 0.0 {
                            if let Some((r, base)) = &mut rain {
                                r.pause();
                                *base = 0.0;
                            }
                            continue;
                        }
                        if rain.is_none() {
                            rain = Sink::try_new(handle).ok().map(|s| {
                                let (pcm, rate) = synth::rain_loop();
                                s.append(
                                    rodio::buffer::SamplesBuffer::new(1, rate, pcm)
                                        .repeat_infinite(),
                                );
                                (s, volume)
                            });
                        }
                        if let Some((r, base)) = &mut rain {
                            *base = volume;
                            r.set_volume(volume * master());
                            r.play();
                        }
                        continue;
                    }
                    // (v216) Son calculé (tonnerre).
                    if let Some((samples, rate)) = cmd.pcm {
                        if sinks.len() >= 48 {
                            sinks.remove(0);
                        }
                        if let Ok(sink) = Sink::try_new(handle) {
                            sink.set_volume(cmd.volume * master());
                            sink.append(
                                rodio::buffer::SamplesBuffer::new(1, rate, samples)
                                    .delay(Duration::from_secs_f32(cmd.delay.max(0.0))),
                            );
                            sinks.push((sink, cmd.volume));
                        }
                        continue;
                    }
                    // (v215) Volume du jeu changé : tout de suite pour les sons en cours.
                    if cmd.file.is_empty() && cmd.voice.is_none() {
                        let m = master();
                        for (sink, base) in &sinks {
                            sink.set_volume(base * m);
                        }
                        if let Some((r, base)) = &rain {
                            r.set_volume(base * m);
                        }
                        continue;
                    }
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
                        sink.set_volume(cmd.volume * master());
                        sink.append(src.delay(Duration::from_secs_f32(cmd.delay.max(0.0))));
                        sinks.push((sink, cmd.volume));
                    }
                }
            });
        spawned.ok().map(|_| Mutex::new(tx))
    })
    .as_ref()
}

/// Joue `sfx` après `delay` secondes, à `gain` (distance… 0..1), au volume du jeu
/// (`set_master`).
pub fn play(sfx: Sfx, delay: f32, gain: f32) {
    if gain <= 0.0 || master() <= 0.0 {
        return;
    }
    let (file, volume) = file_of(sfx);
    if let Some(tx) = sender() {
        if let Ok(tx) = tx.lock() {
            let _ = tx.send(Cmd {
                file,
                volume: volume * gain,
                delay,
                voice: None,
                reopen: false,
                pcm: None,
                rain: None,
            });
        }
    }
}

/// (v216) Volume du bruit de la pluie (0 : elle s'arrête), sans le volume du jeu (il s'y
/// ajoute, voir `set_master`). À appeler quand il change : la pluie continue toute seule
/// entre deux appels.
pub fn rain(volume: f32) {
    if let Some(tx) = sender() {
        if let Ok(tx) = tx.lock() {
            let _ = tx.send(Cmd {
                file: "",
                volume: 0.0,
                delay: 0.0,
                voice: None,
                reopen: false,
                pcm: None,
                rain: Some(volume.clamp(0.0, 1.0)),
            });
        }
    }
}

/// (v216) Tonnerre d'un éclair tombé à `distance` blocs : calculé pour cette distance
/// (un claquement sec tout près, un roulement sourd au loin ; voir `synth::thunder`), il
/// arrive après le temps que met le son à venir (343 blocs par seconde, comme en vrai).
/// Le calcul se fait dans un fil à part : le jeu n'attend pas.
pub fn thunder(distance: f32, seed: u32) {
    if master() <= 0.0 {
        return;
    }
    let Some(tx) = sender().and_then(|tx| tx.lock().ok().map(|t| t.clone())) else {
        return;
    };
    let started = std::time::Instant::now();
    let _ = std::thread::Builder::new()
        .name("tonnerre".into())
        .spawn(move || {
            let (pcm, rate) = synth::thunder(distance, seed);
            let delay = distance / 343.0 - started.elapsed().as_secs_f32();
            let _ = tx.send(Cmd {
                file: "",
                volume: synth::thunder_gain(distance),
                delay: delay.max(0.0),
                voice: None,
                reopen: false,
                pcm: Some((pcm, rate)),
                rain: None,
            });
        });
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
                pcm: None,
                rain: None,
            });
        }
    }
}

/// (v216) Sons calculés : la pluie et le tonnerre. Pas d'enregistrement : le tonnerre est
/// refait pour chaque éclair selon sa distance, et la pluie est une boucle sans raccord.
pub mod synth {
    use std::f32::consts::TAU;

    /// Cadence des sons calculés (Hz).
    pub const RATE: u32 = 32_000;

    struct Noise(u32);

    impl Noise {
        /// Bruit blanc, −1 … 1.
        fn next(&mut self) -> f32 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 17;
            self.0 ^= self.0 << 5;
            (self.0 >> 8) as f32 / 8_388_608.0 - 1.0
        }

        /// 0 … 1.
        fn unit(&mut self) -> f32 {
            self.next() * 0.5 + 0.5
        }
    }

    /// Filtre passe-bas à un pôle : coefficient pour la fréquence de coupure `hz`.
    fn pole(hz: f32) -> f32 {
        1.0 - (-TAU * hz / RATE as f32).exp()
    }

    /// Ce que deux de ces filtres à la suite laissent d'un bruit blanc (rapport des
    /// niveaux) : de quoi rendre à un bruit filtré le niveau demandé.
    fn kept(a: f32) -> f32 {
        let r = 1.0 - a;
        let r2 = r * r;
        a * a * ((1.0 + r2) / (1.0 - r2).powi(3)).sqrt()
    }

    /// Une bouffée de bruit filtré (sous `hz`) à partir de l'instant `at` : montée en
    /// `attack` s, éteinte en `decay` s, de niveau `gain`.
    fn burst(
        out: &mut [f32],
        rng: &mut Noise,
        at: f32,
        gain: f32,
        attack: f32,
        decay: f32,
        hz: f32,
    ) {
        let start = (at * RATE as f32) as usize;
        let len = ((attack + decay * 7.0) * RATE as f32) as usize;
        let a = pole(hz.clamp(40.0, 12_000.0));
        let k = gain / kept(a);
        let (mut y1, mut y2) = (0.0f32, 0.0f32);
        for i in 0..len {
            let Some(o) = out.get_mut(start + i) else {
                break;
            };
            let t = i as f32 / RATE as f32;
            let env = if t < attack {
                t / attack
            } else {
                (-(t - attack) / decay).exp()
            };
            y1 += a * (rng.next() - y1);
            y2 += a * (y1 - y2);
            *o += y2 * env * k;
        }
    }

    /// Volume du tonnerre selon la distance (blocs) : fort tout près, il faiblit sans
    /// disparaître (on entend un orage de loin).
    pub fn thunder_gain(distance: f32) -> f32 {
        (1.0 / (1.0 + distance.max(0.0) / 110.0)).max(0.12)
    }

    /// Tonnerre d'un éclair tombé à `distance` blocs.
    ///
    /// Le canal de foudre est long de plusieurs centaines de blocs : chaque morceau fait
    /// son propre claquement, qui arrive plus ou moins tard selon sa distance à l'oreille.
    /// Tout près on entend d'abord un claquement sec et ses répliques (les morceaux les
    /// plus proches, presque ensemble), puis le roulement des morceaux lointains et de
    /// leurs échos ; au loin l'air a mangé les aigus : il ne reste que le roulement sourd,
    /// qui s'installe peu à peu.
    pub fn thunder(distance: f32, seed: u32) -> (Vec<i16>, u32) {
        let mut rng = Noise(seed.wrapping_mul(2_654_435_761) | 1);
        let near = (1.0 - distance / 260.0).clamp(0.0, 1.0);
        let far = (distance / 500.0).clamp(0.0, 1.0);
        let secs = 5.0 + 3.0 * far + rng.unit();
        let n = (secs * RATE as f32) as usize;
        let mut out = vec![0.0f32; n];
        // L'air mange les aigus avec la distance : 7 kHz tout près, 1 kHz à 300 blocs.
        let air = 7000.0 / (1.0 + distance / 55.0);
        // 1. Le claquement et ses répliques (tout près seulement).
        if near > 0.0 {
            burst(&mut out, &mut rng, 0.0, 1.0 * near, 0.0012, 0.05, air);
            for _ in 0..2 + (6.0 * near) as usize {
                let at = 0.03 + 0.45 * rng.unit() * rng.unit();
                let gain = (0.2 + 0.5 * rng.unit()) * near;
                let decay = 0.02 + 0.05 * rng.unit();
                let hz = air * (0.6 + 0.4 * rng.unit());
                burst(&mut out, &mut rng, at, gain, 0.002, decay, hz);
            }
        }
        // 2. Le roulement : les claquements des morceaux plus lointains et leurs échos, de
        //    plus en plus tardifs, faibles et sourds.
        let spread = 1.6 + 2.4 * far + 1.2 * rng.unit();
        for _ in 0..26 + (rng.unit() * 14.0) as usize {
            let late = rng.unit().powf(1.4);
            let at = 0.04 + 0.5 * far + spread * late;
            let gain = (0.3 - 0.2 * late) * (0.5 + 0.5 * rng.unit()) * (1.0 - 0.35 * near);
            let attack = 0.02 + 0.05 * rng.unit() + 0.05 * far;
            let decay = 0.18 + 0.5 * rng.unit();
            let hz = (air * (0.5 - 0.35 * late)).max(90.0);
            burst(&mut out, &mut rng, at, gain, attack, decay, hz);
        }
        // 3. Le grondement de fond : un bruit très grave qui roule (son niveau ondule) et
        //    s'éteint lentement.
        let a = pole(60.0 + 50.0 * (1.0 - far));
        let k = 0.2 / kept(a);
        let (mut y1, mut y2) = (0.0f32, 0.0f32);
        let (wobble, phase) = (2.2 + 3.0 * rng.unit(), rng.unit() * TAU);
        for (i, o) in out.iter_mut().enumerate() {
            let t = i as f32 / RATE as f32;
            let env = (t / (0.25 + 0.6 * far)).min(1.0) * (-t / (secs * 0.36)).exp();
            let roll = 0.72 + 0.28 * (TAU * wobble * t + phase).sin() * (TAU * 0.47 * t).cos();
            y1 += a * (rng.next() - y1);
            y2 += a * (y1 - y2);
            *o += y2 * k * env * roll;
        }
        (finish(out), RATE)
    }

    /// Met le son à l'échelle (crête à 90 %), fond sa fin, et le passe en entiers 16 bits.
    fn finish(mut s: Vec<f32>) -> Vec<i16> {
        let n = s.len();
        let fade = (RATE as usize / 2).min(n);
        for (i, v) in s[n - fade..].iter_mut().enumerate() {
            *v *= 1.0 - i as f32 / fade as f32;
        }
        let peak = s.iter().fold(1e-6f32, |m, v| m.max(v.abs()));
        let k = 0.9 * 32767.0 / peak;
        s.iter().map(|v| (v * k) as i16).collect()
    }

    /// Boucle de pluie (7 s, sans raccord) : le chuintement des gouttes dans l'air et sur
    /// le sol (bruit entre 0,9 et 6,5 kHz), le fond sourd de l'averse au loin, et le
    /// crépitement des gouttes proches (de brefs « plic » aigus, serrés).
    pub fn rain_loop() -> (Vec<i16>, u32) {
        let n = 7 * RATE as usize;
        // Une demi-seconde de plus, fondue avec le début : la boucle se referme sans
        // raccord.
        let extra = RATE as usize / 2;
        let mut rng = Noise(0x9a17_3d05);
        let mut raw = vec![0.0f32; n + extra];
        let (lo, hi, deep) = (pole(900.0), pole(6500.0), pole(420.0));
        let (mut l1, mut h1, mut h2, mut d1, mut d2) = (0.0f32, 0.0f32, 0.0f32, 0.0f32, 0.0f32);
        for (i, o) in raw.iter_mut().enumerate() {
            let t = i as f32 / RATE as f32;
            let w = rng.next();
            // Chuintement : au-dessus de 0,9 kHz (le bruit moins sa partie grave), sous
            // 6,5 kHz.
            l1 += lo * (w - l1);
            h1 += hi * ((w - l1) - h1);
            h2 += hi * (h1 - h2);
            // Fond sourd, qui respire lentement (périodes entières sur la boucle).
            d1 += deep * (rng.next() - d1);
            d2 += deep * (d1 - d2);
            let breath =
                1.0 + 0.25 * (TAU * t * 2.0 / 7.0).sin() + 0.15 * (TAU * t * 5.0 / 7.0).cos();
            *o = h2 * 0.9 + d2 * 2.4 * breath;
        }
        // Gouttes proches : 150 par seconde, chacune un « plic » de quelques millisecondes.
        let drops = 150 * (n + extra) / RATE as usize;
        for _ in 0..drops {
            let at = (rng.unit() * (n + extra) as f32) as usize;
            let hz = 1800.0 + 4200.0 * rng.unit() * rng.unit();
            let decay = 0.0015 + 0.004 * rng.unit();
            let gain = 0.5 * rng.unit().powi(3);
            let len = (decay * 7.0 * RATE as f32) as usize;
            for i in 0..len {
                let Some(o) = raw.get_mut(at + i) else {
                    break;
                };
                let t = i as f32 / RATE as f32;
                *o += (TAU * hz * t).sin() * (-t / decay).exp() * gain;
            }
        }
        let mut out = raw[..n].to_vec();
        for i in 0..extra {
            let k = i as f32 / extra as f32;
            // (Deux bruits sans lien : fondu à puissance constante.)
            out[i] = raw[i] * (k * TAU / 4.0).sin() + raw[n + i] * (k * TAU / 4.0).cos();
        }
        let peak = out.iter().fold(1e-6f32, |m, v| m.max(v.abs()));
        let k = 0.9 * 32767.0 / peak;
        (out.iter().map(|v| (v * k) as i16).collect(), RATE)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn rms(s: &[i16]) -> f32 {
            (s.iter().map(|v| (*v as f32).powi(2)).sum::<f32>() / s.len().max(1) as f32).sqrt()
        }

        /// Part des aigus : niveau de ce qui change d'un échantillon au suivant, rapporté
        /// au niveau du son (un son grave change lentement).
        fn brightness(s: &[i16]) -> f32 {
            let d: Vec<i16> = s.windows(2).map(|w| w[1].saturating_sub(w[0])).collect();
            rms(&d) / rms(s).max(1.0)
        }

        /// Tout près : un claquement sec, tout de suite, puis le roulement. Au loin : pas
        /// de claquement, un roulement sourd qui monte peu à peu, plus long.
        #[test]
        fn thunder_cracks_nearby_and_rumbles_far_away() {
            let ms = RATE as usize / 1000;
            let (near, rate) = thunder(15.0, 7);
            let (far, _) = thunder(700.0, 7);
            assert_eq!(rate, RATE);
            for s in [&near, &far] {
                let peak = s.iter().map(|v| v.unsigned_abs()).max().unwrap();
                assert!((29_000..=29_500).contains(&peak), "crête {peak}");
                assert_eq!(*s.last().unwrap(), 0, "fini en fondu");
                let mean = s.iter().map(|v| *v as f64).sum::<f64>() / s.len() as f64;
                assert!(mean.abs() < 400.0, "pas de décalage ({mean})");
            }
            assert!(near.len() >= 5 * RATE as usize && far.len() > near.len() + RATE as usize);
            // Le claquement : la crête dans les 30 premières millisecondes, bien plus fort
            // que la suite.
            let crack = rms(&near[..30 * ms]);
            let tail = rms(&near[1500 * ms..2500 * ms]);
            assert!(crack > tail * 2.5, "claquement {crack}, suite {tail}");
            assert!(near[..30 * ms].iter().any(|v| v.unsigned_abs() > 20_000));
            // Au loin : rien de sec au départ, le plus fort vient après.
            let start = rms(&far[..100 * ms]);
            let later = rms(&far[800 * ms..2800 * ms]);
            assert!(later > start * 3.0, "départ {start}, suite {later}");
            // Et c'est bien plus sourd.
            assert!(
                brightness(&near[..200 * ms]) > brightness(&far[800 * ms..2800 * ms]) * 4.0,
                "{} contre {}",
                brightness(&near[..200 * ms]),
                brightness(&far[800 * ms..2800 * ms])
            );
            // Chaque éclair a son tonnerre ; le même éclair, le même.
            assert_ne!(thunder(15.0, 8).0, near);
            assert_eq!(thunder(15.0, 7).0, near);
            assert!(thunder_gain(0.0) > 0.99 && thunder_gain(110.0) < 0.51);
            assert!(thunder_gain(5000.0) >= 0.12, "un orage s'entend de loin");
        }

        /// La pluie : un son régulier d'un bout à l'autre, plein d'aigus, qui se referme
        /// sur lui-même sans raccord.
        #[test]
        fn rain_is_a_steady_seamless_loop() {
            let (s, rate) = rain_loop();
            assert_eq!((s.len(), rate), (7 * RATE as usize, RATE));
            let level = rms(&s);
            assert!(level > 2500.0 && level < 12_000.0, "niveau {level}");
            // Régulière : chaque demi-seconde au même niveau, à 25 % près.
            for c in s.chunks(RATE as usize / 2) {
                let r = rms(c) / level;
                assert!((0.75..1.25).contains(&r), "demi-seconde à {r}");
            }
            assert!(brightness(&s) > 0.35, "chuintante ({})", brightness(&s));
            // Le raccord : la fin et le début s'enchaînent comme deux instants voisins
            // (pas de saut plus grand que ceux du son lui-même).
            let jump = (s[0] as i32 - s[s.len() - 1] as i32).abs();
            let usual = s
                .windows(2)
                .map(|w| (w[1] as i32 - w[0] as i32).abs())
                .max()
                .unwrap();
            assert!(
                jump <= usual,
                "saut de {jump} au raccord (au plus {usual} ailleurs)"
            );
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
