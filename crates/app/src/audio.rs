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
        // (v247) « Tomber dans l'eau : réduis-le de 25 % » (0,8 avant).
        Sfx::Splash => ("splash", 0.6),
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

/// (v217) Variation de hauteur d'un son à chaque fois qu'il est joué, au plus (demi-tons, de
/// part et d'autre) : le même son répété (pas, tirs en rafale, rebonds) ne sonne plus comme
/// un disque rayé. Rien sur le bip de la bombe (un signal que l'on compte) ni sur la lunette.
fn pitch_spread(sfx: Sfx) -> f32 {
    match sfx {
        Sfx::BombBeep | Sfx::ScopeIn | Sfx::ScopeOut => 0.0,
        Sfx::Shot(_) | Sfx::Decoy | Sfx::Dry => 0.4,
        Sfx::Reload(_)
        | Sfx::ReloadEnd
        | Sfx::Draw(_)
        | Sfx::DrawKnife
        | Sfx::DrawNade
        | Sfx::Cycle
        | Sfx::Shell
        | Sfx::Hammer
        | Sfx::Fire
        | Sfx::BombBlast => 0.5,
        Sfx::Pin | Sfx::Throw | Sfx::Pain => 0.7,
        Sfx::He | Sfx::Flash | Sfx::Smoke | Sfx::Molotov | Sfx::Door(_) => 0.8,
        Sfx::KnifeSwing | Sfx::KnifeHit | Sfx::KnifeFlesh | Sfx::Land | Sfx::Splash => 1.0,
        Sfx::Bounce | Sfx::Ladder => 1.2,
        Sfx::Step => 1.5,
    }
}

/// (v219) Variation de hauteur du tonnerre, de part et d'autre (demi-tons).
const THUNDER_SPREAD: f32 = 1.5;
/// (v219) Variation de hauteur du « pop » d'un objet ramassé (demi-tons).
const PICKUP_SPREAD: f32 = 4.0;
/// (v219) Volume du « pop » d'un objet ramassé.
const PICKUP_VOLUME: f32 = 0.35;

/// (v219) Une boucle lue sans fin, à une vitesse qui ondule lentement (deux sinus de
/// périodes 13 s et 7,3 s, ± un demi-ton en tout) : pas de raccord (la boucle se referme
/// sur elle-même), et la même boucle ne se reconnaît plus d'un tour à l'autre.
struct Wobble {
    buf: Vec<i16>,
    rate: u32,
    pos: f64,
    t: f64,
    phase: f64,
}

impl Wobble {
    fn new(buf: Vec<i16>, rate: u32, phase: f32) -> Self {
        Self {
            buf,
            rate,
            pos: 0.0,
            t: 0.0,
            phase: phase as f64 * std::f64::consts::PI,
        }
    }

    /// Vitesse de lecture au temps `t` (s).
    fn rate_at(&self, t: f64) -> f64 {
        use std::f64::consts::TAU;
        let semi = 0.32 * (t * TAU / 13.0 + self.phase).sin()
            + 0.18 * (t * TAU / 7.3 + 1.0 + self.phase).sin();
        2f64.powf(semi / 12.0)
    }
}

impl Iterator for Wobble {
    type Item = i16;

    fn next(&mut self) -> Option<i16> {
        let n = self.buf.len();
        if n < 2 {
            return None;
        }
        let i = self.pos as usize % n;
        let f = self.pos - self.pos.floor();
        let (a, b) = (self.buf[i] as f64, self.buf[(i + 1) % n] as f64);
        let v = a + (b - a) * f;
        self.pos = (self.pos + self.rate_at(self.t)) % n as f64;
        self.t += 1.0 / self.rate as f64;
        Some(v as i16)
    }
}

impl rodio::Source for Wobble {
    fn current_frame_len(&self) -> Option<usize> {
        None
    }
    fn channels(&self) -> u16 {
        1
    }
    fn sample_rate(&self) -> u32 {
        self.rate
    }
    fn total_duration(&self) -> Option<Duration> {
        None
    }
}

/// (v220) Crépitement électrique sans fin et jamais pareil (la Foudre de Zeus tenue). (v222)
/// Sourd, étouffé, demandé par Viendax (« un truc qui crépite plus sourdement ») : des
/// claquements graves (bruit filtré passe-bas de 400 à 1 600 Hz et un petit coup sourd de 55
/// à 135 Hz, 5 à 16 ms), parfois en rafale ; de temps en temps un grésillement étouffé (bruit
/// sous 350 à 700 Hz, 50 à 180 ms) ou un ronflement (dents de scie graves, 70 à 160 Hz, qui
/// tombent d'une demi-octave en 60 à 150 ms) ; le tout sous un passe-bas à 1 800 Hz. Avant :
/// des claquements secs de bruit blanc et des « zaps » aigus (centre du spectre vers 5 kHz,
/// 400 Hz maintenant). Instants, forces, hauteurs et filtres tirés au hasard à chaque fois :
/// rien ne se répète (règle des sons du jeu : voir `claude/sons.md` du projet).
pub struct Crackle {
    rng: u64,
    sparks: Vec<Spark>,
    /// Échantillons avant le prochain claquement, et avant le prochain grésillement ou
    /// ronflement ; claquements encore à venir dans la rafale en cours.
    next_click: u32,
    next_buzz: u32,
    burst: u32,
    /// Passe-bas (deux pôles) du tout : étouffé.
    lp1: f32,
    lp2: f32,
}

#[derive(Clone, Copy)]
struct Spark {
    /// 0 claquement, 1 grésillement, 2 ronflement.
    kind: u8,
    age: u32,
    len: u32,
    amp: f32,
    /// Coefficient de son passe-bas (deux pôles, `lp` et `lp2`).
    cut: f32,
    /// Claquement : hauteur du coup sourd et sa part ; ronflement : hauteur de départ.
    freq: f32,
    mix: f32,
    phase: f32,
    lp: f32,
    lp2: f32,
}

/// Coefficient d'un passe-bas d'un pôle coupant à `hz`.
fn lowpass(hz: f32) -> f32 {
    1.0 - (-std::f32::consts::TAU * hz / synth::RATE as f32).exp()
}

impl Crackle {
    pub fn new(seed: u64) -> Self {
        let mut c = Self {
            rng: seed | 1,
            sparks: Vec::new(),
            next_click: 0,
            next_buzz: 0,
            burst: 0,
            lp1: 0.0,
            lp2: 0.0,
        };
        c.next_click = c.between(300, 2_500);
        c.next_buzz = c.between(6_000, 24_000);
        c
    }

    fn next_u64(&mut self) -> u64 {
        // xorshift64*.
        self.rng ^= self.rng >> 12;
        self.rng ^= self.rng << 25;
        self.rng ^= self.rng >> 27;
        self.rng.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Au hasard dans 0..1.
    fn unit(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }

    /// Nombre d'échantillons au hasard dans `a..b`.
    fn between(&mut self, a: u32, b: u32) -> u32 {
        a + (self.unit() * (b - a) as f32) as u32
    }

    fn noise(&mut self) -> f32 {
        self.unit() * 2.0 - 1.0
    }

    fn spark(&mut self, kind: u8) {
        let ms = synth::RATE / 1000;
        let mut s = Spark {
            kind,
            age: 0,
            len: 0,
            amp: 0.0,
            cut: 0.0,
            freq: 0.0,
            mix: 0.0,
            phase: 0.0,
            lp: 0.0,
            lp2: 0.0,
        };
        match kind {
            0 => {
                s.len = self.between(5 * ms, 16 * ms);
                s.amp = 0.3 + 0.7 * self.unit();
                s.cut = lowpass(400.0 * 2f32.powf(self.unit() * 2.0));
                s.freq = 55.0 * 2f32.powf(self.unit() * 1.3);
                s.mix = 0.05 + 0.2 * self.unit();
            }
            1 => {
                s.len = self.between(50 * ms, 180 * ms);
                s.amp = 0.15 + 0.25 * self.unit();
                s.cut = lowpass(350.0 * 2f32.powf(self.unit()));
            }
            _ => {
                s.len = self.between(60 * ms, 150 * ms);
                s.amp = 0.15 + 0.2 * self.unit();
                s.cut = lowpass(600.0);
                s.freq = 70.0 * 2f32.powf(self.unit() * 1.2);
            }
        }
        if self.sparks.len() < 24 {
            self.sparks.push(s);
        }
    }
}

/// Bruit filtré par deux pôles de coefficient `a` : son écart-type est à peu près
/// 0,29·√a ; ramené à 0,72 (le bruit blanc de départ est à 0,58).
const NOISE_LEVEL: f32 = 2.5;

impl Iterator for Crackle {
    type Item = i16;

    fn next(&mut self) -> Option<i16> {
        // Claquements : un à la fois (15 à 120 ms d'écart), ou une rafale de 3 à 6
        // rapprochés (3 à 12 ms).
        if self.next_click == 0 {
            self.spark(0);
            if self.burst > 0 {
                self.burst -= 1;
                self.next_click = self.between(90, 380);
            } else if self.unit() < 0.2 {
                self.burst = self.between(2, 5);
                self.next_click = self.between(90, 380);
            } else {
                self.next_click = self.between(480, 3_800);
            }
        }
        self.next_click -= 1;
        if self.next_buzz == 0 {
            let rumble = self.unit() < 0.45;
            self.spark(if rumble { 2 } else { 1 });
            self.next_buzz = self.between(12_000, 48_000);
        }
        self.next_buzz -= 1;
        let mut v = 0.0f32;
        let mut sparks = std::mem::take(&mut self.sparks);
        for sp in &mut sparks {
            let u = sp.age as f32 / sp.len.max(1) as f32;
            let a = sp.cut;
            let x = if sp.kind == 2 {
                // Dents de scie graves qui tombent d'une demi-octave.
                let f = sp.freq * 2f32.powf(-0.5 * u);
                sp.phase = (sp.phase + f / synth::RATE as f32).fract();
                sp.phase * 2.0 - 1.0
            } else {
                self.noise()
            };
            sp.lp += a * (x - sp.lp);
            sp.lp2 += a * (sp.lp - sp.lp2);
            v += match sp.kind {
                0 => {
                    // Montée d'un tiers de milliseconde (pas de clic sec), baisse
                    // exponentielle ; un coup sourd qui descend un peu dessous.
                    let env = (sp.age as f32 / 12.0).min(1.0) * (-4.5 * u).exp();
                    let f = sp.freq * (1.0 - 0.35 * u);
                    sp.phase = (sp.phase + f / synth::RATE as f32).fract();
                    let thump = (std::f32::consts::TAU * sp.phase).sin() * (-6.0 * u).exp();
                    let n = sp.lp2 * NOISE_LEVEL / a.sqrt();
                    sp.amp * (n * (1.0 - sp.mix) + thump * sp.mix) * env
                }
                1 => {
                    let env = (std::f32::consts::PI * u).sin();
                    sp.amp * sp.lp2 * NOISE_LEVEL / a.sqrt() * env
                }
                _ => sp.amp * sp.lp2 * 1.4 * (1.0 - u) * (1.0 - u),
            };
            sp.age += 1;
        }
        sparks.retain(|sp| sp.age < sp.len);
        self.sparks = sparks;
        // Étouffé (passe-bas à 1 800 Hz), et jamais saturé.
        let g = lowpass(1_800.0);
        self.lp1 += g * (v - self.lp1);
        self.lp2 += g * (self.lp1 - self.lp2);
        Some(((self.lp2 * 0.55).tanh() * 0.8 * i16::MAX as f32) as i16)
    }
}

impl rodio::Source for Crackle {
    fn current_frame_len(&self) -> Option<usize> {
        None
    }
    fn channels(&self) -> u16 {
        1
    }
    fn sample_rate(&self) -> u32 {
        synth::RATE
    }
    fn total_duration(&self) -> Option<Duration> {
        None
    }
}

/// (v220) Éclairs tenus qui crépitent cette image (le sien, ceux des autres joueurs à moins
/// de `ZEUS_HEAR` blocs) : chacun sa boucle sans fin (`Crackle`), qui vient de sa direction
/// (oreille gauche ou droite) et dont le volume suit sa distance. Une liste vide arrête tout.
pub fn crackles(list: Vec<CrackleAt>) {
    if let Some(tx) = sender() {
        if let Ok(tx) = tx.lock() {
            let _ = tx.send(Cmd {
                file: "",
                volume: 0.0,
                delay: 0.0,
                speed: 1.0,
                voice: None,
                reopen: false,
                pcm: None,
                rain: None,
                crackle: Some(list),
            });
        }
    }
}

/// (v220) Portée (blocs) du crépitement d'un éclair tenu, et son volume à `d` blocs (1
/// tout près, 0 à la portée).
pub const ZEUS_HEAR: f32 = 10.0;

pub fn crackle_gain(d: f32) -> f32 {
    let x = (1.0 - d / ZEUS_HEAR).clamp(0.0, 1.0);
    x * x
}

/// (v219) Objet ramassé : un petit « pop » calculé (voir `synth::pop`), plus ou moins aigu
/// à chaque fois.
pub fn pickup() {
    if master() <= 0.0 {
        return;
    }
    static POP: OnceLock<(Vec<i16>, u32)> = OnceLock::new();
    let (pcm, rate) = POP.get_or_init(synth::pop).clone();
    if let Some(tx) = sender() {
        if let Ok(tx) = tx.lock() {
            let _ = tx.send(Cmd {
                file: "",
                volume: PICKUP_VOLUME,
                delay: 0.0,
                speed: 2f32.powf(PICKUP_SPREAD * jitter() / 12.0),
                voice: None,
                reopen: false,
                pcm: Some((pcm, rate)),
                rain: None,
                crackle: None,
            });
        }
    }
}

/// (v226) Personnage caché trouvé (foule d'essai) : un petit air calculé (voir
/// `synth::found`), tiré à chaque fois (forme, tempo, timbre, tonalité) : jamais deux fois
/// le même.
pub fn found() {
    if master() <= 0.0 {
        return;
    }
    let seed = ((jitter() + 1.0) * 1e6) as u32 | 1;
    let (pcm, rate) = synth::found(seed);
    if let Some(tx) = sender() {
        if let Ok(tx) = tx.lock() {
            let _ = tx.send(Cmd {
                file: "",
                volume: FOUND_VOLUME,
                delay: 0.0,
                speed: 1.0,
                voice: None,
                reopen: false,
                pcm: Some((pcm, rate)),
                rain: None,
                crackle: None,
            });
        }
    }
}

/// (v226) Volume du petit air du personnage caché trouvé.
const FOUND_VOLUME: f32 = 0.4;

/// (v247) Un son calculé, joué une fois à `volume` (× le volume du jeu), à la vitesse
/// `speed` (hauteur et durée).
fn play_pcm(pcm: Vec<i16>, rate: u32, volume: f32, speed: f32) {
    if master() <= 0.0 || volume <= 0.0 {
        return;
    }
    if let Some(tx) = sender() {
        if let Ok(tx) = tx.lock() {
            let _ = tx.send(Cmd {
                file: "",
                volume,
                delay: 0.0,
                speed,
                voice: None,
                reopen: false,
                pcm: Some((pcm, rate)),
                rain: None,
                crackle: None,
            });
        }
    }
}

/// (v247) Graine au hasard pour un son calculé à chaque fois (jamais deux fois le même).
fn fresh_seed() -> u32 {
    ((jitter() + 1.0) * 1e6) as u32 | 1
}

/// (v247) « Peut-être un autre bruit pour quand on tape avec l'éclair de 8 blocs max » : l'arc
/// court de la Foudre de Zeus sans charge ne tonne plus, il grésille et claque (décharge
/// électrique, `synth::zap`), tiré à chaque fois. (v247) « Réduis de 25 % le son de
/// l'éclair quand c'est le petit éclair de 8 blocs » : 75 % du tonnerre qu'il faisait.
/// (v266) « Change le son de l'éclair à 8 blocs, mais un son d'éclair, pas celui qu'on a
/// déjà : un petit coup sec » : le claquement bref d'un éclair tout proche (`synth::crack`),
/// au même volume.
pub fn zap() {
    let (pcm, rate) = synth::crack(fresh_seed());
    play_pcm(pcm, rate, ZAP_VOLUME, 2f32.powf(1.5 * jitter() / 12.0));
}

/// (v247) Volume de l'arc court : 0,75 × celui du tonnerre qu'il faisait (40 blocs : 0,73).
const ZAP_VOLUME: f32 = 0.55;

/// (v247) « Mets un son quand on lâche les objets aussi » : un petit souffle d'objet lancé
/// (`synth::toss`), tiré à chaque fois.
pub fn toss() {
    let (pcm, rate) = synth::toss(fresh_seed());
    play_pcm(pcm, rate, TOSS_VOLUME, 2f32.powf(2.0 * jitter() / 12.0));
}

/// (v247) Volume du souffle d'un objet lancé.
const TOSS_VOLUME: f32 = 0.3;

/// (v247) Un objet tombé dans la lave brûle : grésillement et petits craquements
/// (`synth::sizzle`), tirés à chaque fois ; `gain` : selon la distance.
pub fn burn(gain: f32) {
    let (pcm, rate) = synth::sizzle(fresh_seed());
    play_pcm(pcm, rate, BURN_VOLUME * gain.clamp(0.0, 1.0), 2f32.powf(2.0 * jitter() / 12.0));
}

/// (v247) Volume d'un objet qui brûle dans la lave, tout près.
const BURN_VOLUME: f32 = 0.45;

/// (v217) Nombre au hasard entre −1 et 1 (xorshift, pour la hauteur des sons).
fn jitter() -> f32 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static STATE: AtomicU64 = AtomicU64::new(0);
    let mut x = STATE.load(Ordering::Relaxed);
    if x == 0 {
        x = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0x9E37_79B9_7F4A_7C15, |d| d.as_nanos() as u64)
            | 1;
    }
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    STATE.store(x, Ordering::Relaxed);
    (x >> 40) as f32 / (1u64 << 23) as f32 - 1.0
}

/// (v217) Vitesse de lecture d'un `sfx` (hauteur et durée) : 1 à ± `pitch_spread` demi-tons.
fn pitch_of(sfx: Sfx) -> f32 {
    2f32.powf(pitch_spread(sfx) * jitter() / 12.0)
}

/// Après le tir, l'arme réarme-t-elle à la main (culasse, pompe) ?
pub fn cycles_after_shot(i: usize) -> bool {
    matches!(WEAPONS[i].name, "AWP" | "SSG 08" | "Nova" | "Sawed-Off")
}

struct Cmd {
    file: &'static str,
    volume: f32,
    delay: f32,
    /// (v217) Vitesse de lecture (hauteur), 1 : telle quelle.
    speed: f32,
    /// Chat vocal : (joueur, échantillons 16 kHz mono) au lieu d'un son du jeu.
    voice: Option<(u64, Vec<i16>)>,
    /// La sortie son choisie a changé : elle est rouverte.
    reopen: bool,
    /// (v216) Son calculé (tonnerre) : échantillons mono et leur cadence, au lieu d'un
    /// fichier.
    pcm: Option<(Vec<i16>, u32)>,
    /// (v216) Volume de la pluie (son de fond en boucle ; 0 : silence).
    rain: Option<f32>,
    /// (v220) Crépitements des Foudres de Zeus tenues (voir `crackles`).
    crackle: Option<Vec<CrackleAt>>,
}

/// (v220) Un éclair tenu qui crépite : son numéro (une boucle par éclair), sa direction
/// vue de l'oreille (repère de l'écoute : x à droite, unitaire) et son volume (distance).
#[derive(Clone, Copy, Debug)]
pub struct CrackleAt {
    pub id: u64,
    pub dir: [f32; 3],
    pub gain: f32,
}

/// (v220) Volume propre du crépitement (× la distance, × le volume du jeu). (v236) « Augmente
/// le son du crépitement de l'éclair dans les mains de 50 % » : 0,33 (0,22 avant). (v247)
/// « Augmente le bruit du crépitement de l'éclair de 10 % » : 0,363.
const CRACKLE_VOLUME: f32 = 0.363;

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
    refresh_volumes();
}

/// (v234) Volume de la météo (Paramètres → Son : pluie et tonnerre), × celui du jeu. Bits
/// d'un `f32`, 1 par défaut.
static WEATHER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0x3f80_0000);

/// (v234) Un menu est ouvert par-dessus la partie (Échap, paramètres…) : la pluie et le
/// tonnerre se taisent, et reprennent en revenant au jeu.
static WEATHER_HUSH: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// (v234) Ce que la météo garde de son volume (curseur, menu ouvert).
fn weather_gain() -> f32 {
    if WEATHER_HUSH.load(std::sync::atomic::Ordering::Relaxed) {
        0.0
    } else {
        f32::from_bits(WEATHER.load(std::sync::atomic::Ordering::Relaxed))
    }
}

/// (v234) Volume de la météo (0 à 1) : la pluie et le tonnerre en cours le prennent tout
/// de suite.
pub fn set_weather_volume(v: f32) {
    let v = v.clamp(0.0, 1.0);
    let old = WEATHER.swap(v.to_bits(), std::sync::atomic::Ordering::Relaxed);
    if old != v.to_bits() {
        refresh_volumes();
    }
}

/// (v234) « Coupe le son de la météo dans Échap » : `true` tant qu'un menu est ouvert
/// par-dessus la partie. Sans effet s'il ne change pas.
pub fn weather_hush(on: bool) {
    let old = WEATHER_HUSH.swap(on, std::sync::atomic::Ordering::Relaxed);
    if old != on {
        refresh_volumes();
    }
}

/// Redonne leur volume aux sons en cours (volume du jeu ou de la météo changé).
fn refresh_volumes() {
    if let Some(tx) = sender() {
        if let Ok(tx) = tx.lock() {
            // (Commande vide : le fil du son redonne le volume à ses sons en cours.)
            let _ = tx.send(Cmd {
                file: "",
                volume: 0.0,
                delay: 0.0,
                speed: 1.0,
                voice: None,
                reopen: false,
                pcm: None,
                rain: None,
                crackle: None,
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
                speed: 1.0,
                voice: None,
                reopen: true,
                pcm: None,
                rain: None,
                crackle: None,
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
                // (v217) Les tonnerres en cours, à part : ils se taisent quand on quitte la
                // partie.
                let mut thunders: Vec<(Sink, f32)> = Vec::new();
                // (v220) Crépitements des éclairs tenus : une boucle par éclair.
                let mut crackling: HashMap<u64, rodio::SpatialSink> = HashMap::new();
                while let Ok(cmd) = rx.recv() {
                    if cmd.reopen {
                        // Les sons en cours étaient liés à l'ancienne sortie.
                        sinks.clear();
                        thunders.clear();
                        voices.clear();
                        rain = None;
                        crackling.clear();
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
                    // (v220) Crépitements : une boucle par éclair tenu, qui suit sa direction
                    // (l'oreille la plus proche l'entend plus fort) et sa distance (volume).
                    // Les oreilles à un bloc l'une de l'autre, l'éclair à un bloc : rodio ne
                    // baisse pas le son de lui-même (la distance est dans `gain`).
                    if let Some(list) = cmd.crackle {
                        crackling.retain(|id, sink| {
                            let keep = list.iter().any(|c| c.id == *id);
                            if !keep {
                                sink.stop();
                            }
                            keep
                        });
                        let (left, right) = ([-0.5, 0.0, 0.0], [0.5, 0.0, 0.0]);
                        for c in list {
                            let sink = match crackling.entry(c.id) {
                                std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
                                std::collections::hash_map::Entry::Vacant(e) => {
                                    match rodio::SpatialSink::try_new(handle, c.dir, left, right) {
                                        Ok(s) => {
                                            s.append(Crackle::new(
                                                c.id ^ (jitter().to_bits() as u64) << 20,
                                            ));
                                            e.insert(s)
                                        }
                                        Err(_) => continue,
                                    }
                                }
                            };
                            sink.set_emitter_position(c.dir);
                            sink.set_volume(CRACKLE_VOLUME * c.gain * master());
                        }
                        continue;
                    }
                    // (v216) Pluie : son de fond en boucle, à son volume (× celui du jeu).
                    if let Some(volume) = cmd.rain {
                        // (v217) Partie quittée : le tonnerre en cours se tait aussi.
                        if volume < 0.0 {
                            thunders.clear();
                        }
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
                                // (v219) Sa hauteur ondule lentement : la boucle de 7 s ne
                                // se reconnaît plus.
                                s.append(Wobble::new(pcm, rate, jitter()));
                                (s, volume)
                            });
                        }
                        if let Some((r, base)) = &mut rain {
                            *base = volume;
                            r.set_volume(volume * master() * weather_gain());
                            r.play();
                        }
                        continue;
                    }
                    // (v216) Son calculé (tonnerre).
                    if let Some((samples, rate)) = cmd.pcm {
                        thunders.retain(|s| !s.0.empty());
                        if thunders.len() >= 16 {
                            thunders.remove(0);
                        }
                        if let Ok(sink) = Sink::try_new(handle) {
                            sink.set_volume(cmd.volume * master() * weather_gain());
                            sink.append(
                                rodio::buffer::SamplesBuffer::new(1, rate, samples)
                                    .speed(cmd.speed.clamp(0.5, 2.0))
                                    .delay(Duration::from_secs_f32(cmd.delay.max(0.0))),
                            );
                            thunders.push((sink, cmd.volume));
                        }
                        continue;
                    }
                    // (v215) Volume du jeu changé : tout de suite pour les sons en cours.
                    // (v234) Pareil pour celui de la météo (curseur, menu ouvert).
                    if cmd.file.is_empty() && cmd.voice.is_none() {
                        let m = master();
                        for (sink, base) in &sinks {
                            sink.set_volume(base * m);
                        }
                        let w = m * weather_gain();
                        for (sink, base) in &thunders {
                            sink.set_volume(base * w);
                        }
                        if let Some((r, base)) = &rain {
                            r.set_volume(base * w);
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
                        sink.append(
                            src.speed(cmd.speed.clamp(0.5, 2.0))
                                .delay(Duration::from_secs_f32(cmd.delay.max(0.0))),
                        );
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
                speed: pitch_of(sfx),
                voice: None,
                reopen: false,
                pcm: None,
                rain: None,
                crackle: None,
            });
        }
    }
}

/// (v217) Numéro de la partie pour les sons de la météo : un tonnerre encore en calcul
/// quand on la quitte ne part plus.
static WEATHER_EPOCH: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// (v217) La partie est quittée : la pluie s'arrête et les tonnerres en cours se taisent
/// (ils continuaient à l'accueil).
pub fn weather_off() {
    WEATHER_EPOCH.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    send_rain(-1.0);
}

/// (v216) Volume du bruit de la pluie (0 : elle s'arrête), sans le volume du jeu (il s'y
/// ajoute, voir `set_master`). À appeler quand il change : la pluie continue toute seule
/// entre deux appels.
pub fn rain(volume: f32) {
    send_rain(volume.clamp(0.0, 1.0));
}

fn send_rain(volume: f32) {
    if let Some(tx) = sender() {
        if let Ok(tx) = tx.lock() {
            let _ = tx.send(Cmd {
                file: "",
                volume: 0.0,
                delay: 0.0,
                speed: 1.0,
                voice: None,
                reopen: false,
                pcm: None,
                rain: Some(volume),
                crackle: None,
            });
        }
    }
}

/// (v216) Tonnerre d'un éclair tombé à `distance` blocs : calculé pour cette distance
/// (un claquement sec tout près, un roulement sourd au loin ; voir `synth::thunder`), il
/// arrive après le temps que met le son à venir (343 blocs par seconde, comme en vrai).
/// Le calcul se fait dans un fil à part : le jeu n'attend pas.
pub fn thunder(distance: f32, seed: u32, gain: f32) {
    // (v234) Météo à zéro ou menu ouvert : pas de tonnerre à calculer.
    if master() <= 0.0 || weather_gain() <= 0.0 {
        return;
    }
    let Some(tx) = sender().and_then(|tx| tx.lock().ok().map(|t| t.clone())) else {
        return;
    };
    let started = std::time::Instant::now();
    let epoch = WEATHER_EPOCH.load(std::sync::atomic::Ordering::Relaxed);
    let _ = std::thread::Builder::new()
        .name("tonnerre".into())
        .spawn(move || {
            let (pcm, rate) = synth::thunder(distance, seed);
            // (v217) Partie quittée entre-temps : il ne part plus.
            if WEATHER_EPOCH.load(std::sync::atomic::Ordering::Relaxed) != epoch {
                return;
            }
            let delay = distance / 343.0 - started.elapsed().as_secs_f32();
            let _ = tx.send(Cmd {
                file: "",
                volume: synth::thunder_gain(distance) * gain,
                delay: delay.max(0.0),
                // (v219) Chaque coup de tonnerre un peu plus aigu ou plus grave (déjà
                // différent par son tirage).
                speed: 2f32.powf(THUNDER_SPREAD * jitter() / 12.0),
                voice: None,
                reopen: false,
                pcm: Some((pcm, rate)),
                rain: None,
                crackle: None,
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
                speed: 1.0,
                voice: Some((who, samples)),
                reopen: false,
                pcm: None,
                rain: None,
                crackle: None,
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

    /// (v219) « Pop » d'un objet ramassé (90 ms) : une note qui monte vite (de 520 à
    /// 1 250 Hz), avec un peu de son harmonique et un claquement de bruit au tout début,
    /// éteinte en douceur.
    pub fn pop() -> (Vec<i16>, u32) {
        let n = (RATE as f32 * 0.09) as usize;
        let mut rng = Noise(0x51a7_c0de);
        let mut phase = 0.0f32;
        let mut s: Vec<f32> = (0..n)
            .map(|i| {
                let t = i as f32 / RATE as f32;
                let k = (t / 0.09).min(1.0);
                let f = 520.0 * (1250.0f32 / 520.0).powf(k.sqrt());
                phase += TAU * f / RATE as f32;
                let attack = (t / 0.003).min(1.0);
                let env = attack * (-t / 0.028).exp();
                let click = if t < 0.0015 {
                    rng.next() * (1.0 - t / 0.0015)
                } else {
                    0.0
                };
                (phase.sin() + 0.25 * (2.0 * phase).sin()) * env + 0.3 * click
            })
            .collect();
        let fade = (RATE as usize / 200).min(n);
        for (i, v) in s[n - fade..].iter_mut().enumerate() {
            *v *= 1.0 - i as f32 / fade as f32;
        }
        let peak = s.iter().fold(1e-6f32, |m, v| m.max(v.abs()));
        let k = 0.9 * 32767.0 / peak;
        (s.iter().map(|v| (v * k) as i16).collect(), RATE)
    }

    /// (v247) Filtre passe-bande à état (Chamberlin) : un pas, pour la fréquence `hz` et
    /// l'amortissement `q` (0,1 : étroit … 1,4 : large). Rend la bande.
    fn svf(st: &mut (f32, f32), x: f32, hz: f32, q: f32) -> f32 {
        let f = 2.0 * (std::f32::consts::PI * hz.min(RATE as f32 / 6.0) / RATE as f32).sin();
        let (low, band) = st;
        *low += f * *band;
        let high = x - *low - q * *band;
        *band += f * high;
        *band
    }

    /// (v247) Normalise et convertit un son court (crête à 90 %, fondu des 5 dernières ms).
    fn finish_short(mut s: Vec<f32>) -> (Vec<i16>, u32) {
        let n = s.len();
        let fade = (RATE as usize / 200).min(n);
        for (i, v) in s[n - fade..].iter_mut().enumerate() {
            *v *= 1.0 - i as f32 / fade as f32;
        }
        let peak = s.iter().fold(1e-6f32, |m, v| m.max(v.abs()));
        let k = 0.9 * 32767.0 / peak;
        (s.iter().map(|v| (v * k) as i16).collect(), RATE)
    }

    /// (v266) Arc court de la Foudre de Zeus (8 blocs) : « un son d'éclair, mais pas celui
    /// qu'on a déjà : un petit coup sec ». Le claquement d'un éclair tout proche, sans son
    /// roulement : un coup de fouet (bruit large, très bref), deux ou trois éclats plus
    /// petits dans les 50 ms qui suivent (l'air qui se déchire), un coup sourd qui lui donne
    /// du corps et une courte queue de souffle. Un cinquième de seconde ; tirés de `seed` :
    /// hauteur, place et force des éclats. Jamais deux fois le même. (v247 : une décharge
    /// qui bourdonnait et grésillait un tiers de seconde.)
    pub fn crack(seed: u32) -> (Vec<i16>, u32) {
        let mut rng = Noise(seed.wrapping_mul(2_246_822_519) | 1);
        let secs = 0.17 + 0.05 * rng.unit();
        let tone = 2400.0 + 1600.0 * rng.unit();
        let thump = 85.0 + 30.0 * rng.unit();
        // Éclats secondaires : (instant, force).
        let mut bursts = [(0.0f32, 0.0f32); 3];
        let count = 2 + (rng.unit() * 1.99) as usize;
        for b in bursts.iter_mut().take(count) {
            *b = (0.008 + 0.047 * rng.unit(), 0.35 + 0.4 * rng.unit());
        }
        let n = (secs * RATE as f32) as usize;
        let (mut bright, mut air) = ((0.0f32, 0.0f32), (0.0f32, 0.0f32));
        let mut phase = 0.0f32;
        let s = (0..n)
            .map(|i| {
                let t = i as f32 / RATE as f32;
                let noise = rng.next();
                // Le coup : plein bruit pendant 1,5 ms, puis il s'éteint en une dizaine de ms.
                let hit = (t / 0.0004).min(1.0) * (-t / 0.009).exp();
                let mut env = 1.7 * hit;
                for &(at, force) in &bursts {
                    if t >= at {
                        env += force * (-(t - at) / 0.004).exp();
                    }
                }
                let snap = noise * env * 0.55 + svf(&mut bright, noise, tone, 0.7) * env;
                // Le corps : un coup sourd qui glisse vers le grave.
                phase = (phase + thump * (1.0 - 0.35 * (t / secs)) / RATE as f32).fract();
                let body = (std::f32::consts::TAU * phase).sin() * (-t / 0.028).exp() * 0.75;
                // La queue : un souffle grave, très court.
                let tail = svf(&mut air, noise, 520.0, 1.2) * (-t / 0.05).exp() * 0.45;
                snap + body + tail
            })
            .collect();
        finish_short(s)
    }

    /// (v247) Objet lancé : un petit souffle (bruit en bande qui monte de 700 à 2 200 Hz puis
    /// redescend), et un léger coup grave au départ ; tirés de `seed` : durée (110 à 170 ms),
    /// bande, éclat.
    pub fn toss(seed: u32) -> (Vec<i16>, u32) {
        let mut rng = Noise(seed.wrapping_mul(3_266_489_917) | 1);
        let secs = 0.11 + 0.06 * rng.unit();
        let lo = 600.0 + 250.0 * rng.unit();
        let hi = 1800.0 + 900.0 * rng.unit();
        let q = 0.5 + 0.4 * rng.unit();
        let n = (secs * RATE as f32) as usize;
        let mut st = (0.0f32, 0.0f32);
        let mut phase = 0.0f32;
        let s = (0..n)
            .map(|i| {
                let t = i as f32 / RATE as f32;
                let k = t / secs;
                // Montée puis descente de la bande, comme l'objet qui passe devant l'oreille.
                let hump = (std::f32::consts::PI * k).sin();
                let hz = lo + (hi - lo) * hump;
                let env = hump.powf(1.5);
                let whoosh = svf(&mut st, rng.next(), hz, q) * env;
                phase += TAU * 150.0 / RATE as f32;
                let thump = phase.sin() * (-t / 0.012).exp() * 0.35;
                whoosh + thump
            })
            .collect();
        finish_short(s)
    }

    /// (v247) Objet qui brûle dans la lave : un « pff » grave au départ, puis un grésillement
    /// aigu qui s'éteint, semé de petits craquements ; tirés de `seed` : durée (0,5 à 0,8 s),
    /// grain du grésillement, craquements.
    pub fn sizzle(seed: u32) -> (Vec<i16>, u32) {
        let mut rng = Noise(seed.wrapping_mul(2_654_435_769) | 1);
        let secs = 0.5 + 0.3 * rng.unit();
        let hz = 3500.0 + 2500.0 * rng.unit();
        let pops = 25.0 + 30.0 * rng.unit();
        let n = (secs * RATE as f32) as usize;
        let mut st = (0.0f32, 0.0f32);
        let mut low = (0.0f32, 0.0f32);
        let mut pop = 0.0f32;
        let s = (0..n)
            .map(|i| {
                let t = i as f32 / RATE as f32;
                let env = (t / 0.01).min(1.0) * (-t / (secs * 0.4)).exp();
                let hiss = svf(&mut st, rng.next(), hz, 1.2) * env;
                let fwump = svf(&mut low, rng.next(), 180.0, 0.7) * (-t / 0.06).exp() * 2.5;
                if rng.unit() < pops / RATE as f32 {
                    pop = 1.0;
                }
                pop *= 0.992;
                let crack = rng.next() * pop * pop * 0.8;
                hiss + fwump + crack
            })
            .collect();
        finish_short(s)
    }

    /// (v226) Petit air joyeux du personnage caché trouvé : quatre ou cinq notes qui montent
    /// (accord majeur ou suspendu, dans l'un des ordres prévus), la dernière tenue ; tirés de
    /// `seed` : la forme, la tonalité (± 3 demi-tons autour du mi), le tempo et le timbre
    /// (cloche plus ou moins claire). Jamais deux fois le même.
    pub fn found(seed: u32) -> (Vec<i16>, u32) {
        let mut rng = Noise(seed | 1);
        for _ in 0..4 {
            rng.next();
        }
        const SHAPES: [&[f32]; 5] = [
            &[0.0, 4.0, 7.0, 12.0],
            &[0.0, 7.0, 4.0, 12.0],
            &[0.0, 4.0, 7.0, 11.0, 12.0],
            &[0.0, 5.0, 9.0, 12.0],
            &[0.0, 2.0, 4.0, 7.0, 12.0],
        ];
        let shape = SHAPES[((rng.unit() * SHAPES.len() as f32) as usize).min(SHAPES.len() - 1)];
        let root = 659.3 * 2f32.powf(((rng.unit() * 7.0).floor() - 3.0) / 12.0);
        let step = 0.07 + 0.035 * rng.unit();
        let bright = 0.15 + 0.35 * rng.unit();
        let ring = 0.22 + 0.15 * rng.unit();
        let last = step * (shape.len() - 1) as f32;
        let total = last + ring * 3.5;
        let n = (RATE as f32 * total) as usize;
        let mut s = vec![0.0f32; n];
        for (k, semi) in shape.iter().enumerate() {
            let f = root * 2f32.powf(semi / 12.0);
            let at = step * k as f32;
            // Chaque note un peu plus ou moins forte, la dernière tenue plus longtemps.
            let gain = 0.75 + 0.25 * rng.unit();
            let decay = if k + 1 == shape.len() { ring * 1.6 } else { ring * 0.7 };
            let start = (at * RATE as f32) as usize;
            for (i, v) in s[start..].iter_mut().enumerate() {
                let t = i as f32 / RATE as f32;
                let env = (t / 0.004).min(1.0) * (-t / decay).exp();
                if env < 1e-4 && t > 0.01 {
                    break;
                }
                let p = TAU * f * t;
                *v += gain
                    * env
                    * (p.sin() + bright * (2.0 * p).sin() + 0.5 * bright * (3.0 * p).sin());
            }
        }
        let fade = (RATE as usize / 100).min(n);
        for (i, v) in s[n - fade..].iter_mut().enumerate() {
            *v *= 1.0 - i as f32 / fade as f32;
        }
        let peak = s.iter().fold(1e-6f32, |m, v| m.max(v.abs()));
        let k = 0.85 * 32767.0 / peak;
        (s.iter().map(|v| (v * k) as i16).collect(), RATE)
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

        /// (v266) Le petit éclair de 8 blocs : « un petit coup sec ». Un cinquième de seconde
        /// au plus, le plus fort tout au début, presque toute sa force dans les 60 premières
        /// millisecondes (pas de roulement, pas de bourdonnement), jamais deux fois le même.
        #[test]
        fn the_short_arc_is_a_brief_dry_crack() {
            let (a, rate) = crack(1);
            let (b, _) = crack(2);
            assert_eq!(rate, RATE);
            let secs = a.len() as f32 / rate as f32;
            assert!((0.16..=0.23).contains(&secs), "{secs}");
            let peak = a.iter().map(|v| v.unsigned_abs()).max().unwrap();
            let at = a.iter().position(|v| v.unsigned_abs() == peak).unwrap();
            assert!(peak > 28_000 && at < rate as usize / 100, "crête à {at}");
            let energy = |s: &[i16]| s.iter().map(|v| (*v as f64).powi(2)).sum::<f64>();
            let early = energy(&a[..rate as usize * 60 / 1000]) / energy(&a);
            assert!(early > 0.9, "{early}");
            assert_ne!(a, b);
            assert!(a.len() != b.len() || a.iter().zip(&b).any(|(x, y)| x != y));
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

    /// (v220) Le crépitement de l'éclair tenu : de vrais craquements (ni silence ni
    /// saturation), jamais deux fois la même chose (deux demi-secondes de suite diffèrent,
    /// deux éclairs aussi), sourd (v222), et entendu jusqu'à `ZEUS_HEAR` blocs seulement.
    #[test]
    fn the_crackle_never_repeats() {
        let half = synth::RATE as usize / 2;
        let a: Vec<i16> = Crackle::new(7).take(half * 6).collect();
        let b: Vec<i16> = Crackle::new(8).take(half * 6).collect();
        let peak = a.iter().map(|v| v.unsigned_abs()).max().unwrap_or(0);
        assert!(peak > 3_000 && peak < i16::MAX as u16, "pic {peak}");
        // Des silences entre les craquements (ce n'est pas un souffle continu).
        let quiet = a.iter().filter(|v| v.unsigned_abs() < 200).count();
        assert!(quiet > a.len() / 4, "silences : {quiet}");
        for w in 0..5 {
            assert_ne!(
                a[w * half..(w + 1) * half],
                a[(w + 1) * half..(w + 2) * half]
            );
        }
        assert_ne!(a, b);
        // (v222) Sourd : la fréquence moyenne (d'après la pente du signal) reste sous
        // 1 200 Hz (plus de 5 kHz pour les claquements secs de la v220).
        let x: Vec<f64> = a.iter().map(|&v| v as f64).collect();
        let e: f64 = x.iter().map(|v| v * v).sum();
        let d: f64 = x.windows(2).map(|w| (w[1] - w[0]).powi(2)).sum();
        let hz = synth::RATE as f64 / std::f64::consts::TAU * (d / e).sqrt();
        assert!(hz < 1_200.0, "trop aigu : {hz:.0} Hz");
        assert_eq!(crackle_gain(0.0), 1.0);
        assert!(crackle_gain(5.0) > 0.2 && crackle_gain(5.0) < 0.3);
        assert_eq!(crackle_gain(ZEUS_HEAR), 0.0);
        assert_eq!(crackle_gain(30.0), 0.0);
    }

    /// (v234) Curseur de la météo, et menu ouvert par-dessus la partie.
    #[test]
    fn the_weather_has_its_own_volume_and_hushes_in_menus() {
        set_weather_volume(0.4);
        assert_eq!(weather_gain(), 0.4);
        weather_hush(true);
        assert_eq!(weather_gain(), 0.0, "menu ouvert : ni pluie ni tonnerre");
        weather_hush(false);
        assert_eq!(weather_gain(), 0.4);
        set_weather_volume(1.0);
        assert_eq!(weather_gain(), 1.0);
    }

    /// (v219) Le « pop » d'un objet ramassé : court, pas saturé, sans clic à la fin ; la
    /// pluie lue en ondulant ne s'arrête jamais et reste dans ± un demi-ton.
    #[test]
    fn the_pickup_pop_and_the_wobbling_rain() {
        let (pop, rate) = synth::pop();
        assert_eq!(rate, synth::RATE);
        assert!(pop.len() < rate as usize / 10);
        assert!(
            pop.last().is_some_and(|v| v.unsigned_abs() < 200),
            "pas de clic à la fin"
        );
        let w = Wobble::new((0..1000).map(|i| (i % 100) as i16).collect(), 1000, 0.3);
        for k in 0..200 {
            let sp = w.rate_at(k as f64 * 0.1);
            assert!(
                (2f64.powf(-0.5 / 12.0)..=2f64.powf(0.5 / 12.0)).contains(&sp),
                "{sp}"
            );
        }
        let mut w = w;
        assert!(
            (0..50_000).all(|_| w.next().is_some()),
            "la pluie ne s'arrête pas"
        );
    }

    /// (v226) Le petit air du personnage caché : court, pas saturé, sans clic à la fin, et
    /// jamais le même d'une fois à l'autre.
    #[test]
    fn the_found_jingle_changes_every_time() {
        let mut seen = Vec::new();
        for seed in [1u32, 2, 77, 4242, 99_999, 123_456_789] {
            let (pcm, rate) = synth::found(seed);
            assert_eq!(rate, synth::RATE);
            let secs = pcm.len() as f32 / rate as f32;
            assert!((0.4..2.0).contains(&secs), "{secs} s");
            let peak = pcm.iter().map(|v| v.unsigned_abs()).max().unwrap();
            assert!(peak > 20_000 && peak < 32_000, "{peak}");
            assert!(pcm.last().is_some_and(|v| v.unsigned_abs() < 200), "clic à la fin");
            assert!(!seen.contains(&pcm), "même air pour {seed}");
            seen.push(pcm);
        }
    }

    /// (v217) La hauteur des sons varie un peu à chaque fois (pas plus que prévu), jamais
    /// celle du bip de la bombe.
    #[test]
    fn repeated_sounds_change_pitch_a_little() {
        let steps: Vec<f32> = (0..200).map(|_| pitch_of(Sfx::Step)).collect();
        let (lo, hi) = (2f32.powf(-1.5 / 12.0), 2f32.powf(1.5 / 12.0));
        assert!(steps.iter().all(|&p| p >= lo - 1e-4 && p <= hi + 1e-4));
        let spread = steps.iter().cloned().fold(f32::MIN, f32::max)
            - steps.iter().cloned().fold(f32::MAX, f32::min);
        assert!(spread > 0.1, "les pas varient : {spread}");
        assert!((0..50).all(|_| pitch_of(Sfx::BombBeep) == 1.0));
        assert!((0..50).all(|_| (pitch_of(Sfx::Shot(0)) - 1.0).abs() < 0.024));
    }

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
