//! Détection des scripts par le garant vérificateur, en rejouant les entrées des joueurs.
//!
//! Bunny hop : un saut pris au tout premier pas au sol (« parfait ») évite la friction ;
//! un humain qui appuie une fois par saut n'y arrive qu'environ une fois sur trois
//! (fenêtre de 1/64 s). Un script le réussit à tous les coups. Seuils choisis pour qu'un
//! humain n'y arrive jamais (moins d'une chance sur dix milliards, même pour un joueur
//! qui réussirait la moitié de ses sauts) :
//! - 25 sauts parfaits de suite, chacun avec un seul appui pendant le vol ;
//! - sur la partie, au moins 40 sauts enchaînés dont 95 % parfaits, ou 80 dont 85 %.
//!
//! Macro : plus de 9 appuis sur saut en 300 ms (plus de 30 par seconde), impossible avec
//! une touche du clavier ou un bouton de souris (la molette ne peut pas servir à sauter).

use std::collections::{HashMap, VecDeque};

/// Pas de simulation par seconde (voir `session::SIM_TICK`).
const TICKS_PER_S: u32 = 256;
/// Vol minimal d'un vrai saut (pas) : 0,25 s.
const MIN_AIR: u32 = TICKS_PER_S / 4;
/// Saut « enchaîné » : repris au plus 1/16 s après l'atterrissage.
const CHAIN_GROUND: u32 = TICKS_PER_S / 16;
pub const PERFECT_RUN: u32 = 25;
const SPAM_PRESSES: usize = 10;
const SPAM_WINDOW: u32 = TICKS_PER_S * 3 / 10;

#[derive(Clone, Debug, PartialEq)]
pub enum Finding {
    /// Sauts parfaits de suite (un seul appui chacun).
    PerfectRun(u32),
    /// Sur la partie : sauts enchaînés, dont parfaits.
    PerfectRate { chained: u32, perfect: u32 },
    /// Appuis sur saut dans la fenêtre de 300 ms.
    Spam(usize),
}

impl Finding {
    pub fn kind(&self) -> u8 {
        match self {
            Finding::Spam(_) => minelib_net::proto::CHEAT_MACRO,
            _ => minelib_net::proto::CHEAT_BHOP_SCRIPT,
        }
    }

    pub fn detail(&self) -> String {
        match self {
            Finding::PerfectRun(n) => format!("{n} sauts parfaits de suite"),
            Finding::PerfectRate { chained, perfect } => {
                format!("{perfect} sauts parfaits sur {chained} enchaînés")
            }
            Finding::Spam(n) => format!("{n} appuis sur saut en 0,3 s"),
        }
    }
}

#[derive(Default)]
struct Track {
    /// En l'air (après un saut), depuis tant de pas, et appuis pendant ce vol.
    air: Option<(u32, u32)>,
    /// Au sol depuis tant de pas après un vrai saut (fenêtre d'enchaînement).
    ground: Option<u32>,
    /// Le vol qui vient de finir : appuis pendant ce vol.
    last_presses: u32,
    run: u32,
    chained: u32,
    perfect: u32,
    presses: VecDeque<u32>,
    found: Option<Finding>,
}

/// Suivi des sauts de chaque joueur pendant le rejeu.
#[derive(Default)]
pub struct HopTracker {
    players: HashMap<u64, Track>,
}

impl HopTracker {
    /// Un pas du joueur `who` au pas `tick` : appui sur saut, au sol avant et après le
    /// pas, saut pris pendant ce pas.
    pub fn observe(
        &mut self,
        who: u64,
        tick: u32,
        pressed: bool,
        ground_before: bool,
        ground_after: bool,
        jumped: bool,
    ) {
        let t = self.players.entry(who).or_default();
        if t.found.is_some() {
            return;
        }
        if pressed {
            t.presses.push_back(tick);
            while t
                .presses
                .front()
                .is_some_and(|&p| tick.saturating_sub(p) > SPAM_WINDOW)
            {
                t.presses.pop_front();
            }
            if t.presses.len() >= SPAM_PRESSES {
                t.found = Some(Finding::Spam(t.presses.len()));
                return;
            }
        }
        if jumped {
            // Repris depuis le sol après un vrai saut : enchaîné ; parfait s'il part dès le
            // premier pas au sol, avec un seul appui pendant le vol précédent.
            if let Some(g) = t.ground.take() {
                if g <= CHAIN_GROUND {
                    t.chained += 1;
                    if g <= 1 {
                        t.perfect += 1;
                        if t.last_presses == 1 {
                            t.run += 1;
                        } else {
                            t.run = 0;
                        }
                    } else {
                        t.run = 0;
                    }
                }
            } else {
                t.run = 0;
            }
            t.air = Some((0, 0));
            if t.run >= PERFECT_RUN {
                t.found = Some(Finding::PerfectRun(t.run));
            }
            let (c, p) = (t.chained, t.perfect);
            if (c >= 40 && p * 100 >= c * 95) || (c >= 80 && p * 100 >= c * 85) {
                t.found = Some(Finding::PerfectRate {
                    chained: c,
                    perfect: p,
                });
            }
            return;
        }
        match (&mut t.air, ground_before, ground_after) {
            // En vol.
            (Some((air, presses)), _, false) => {
                *air += 1;
                if pressed {
                    *presses += 1;
                }
            }
            // Atterrissage.
            (Some((air, presses)), _, true) => {
                let (a, p) = (*air, *presses + pressed as u32);
                t.air = None;
                if a >= MIN_AIR {
                    t.ground = Some(1);
                    t.last_presses = p;
                } else {
                    t.ground = None;
                    t.run = 0;
                }
            }
            // Au sol : la fenêtre d'enchaînement passe.
            (None, true, true) => {
                if let Some(g) = &mut t.ground {
                    *g += 1;
                    if *g > CHAIN_GROUND {
                        t.ground = None;
                        t.run = 0;
                    }
                }
            }
            // Tombé d'un rebord sans sauter : un vol qui ne compte pas.
            (None, _, false) => {
                t.ground = None;
            }
            _ => {}
        }
    }

    /// Triches trouvées (une par joueur), à signaler une seule fois.
    pub fn take_found(&mut self) -> Vec<(u64, Finding)> {
        let mut out = Vec::new();
        for (who, t) in &mut self.players {
            if let Some(f) = &t.found {
                if t.run != u32::MAX {
                    out.push((*who, f.clone()));
                    // Marque « déjà signalé » sans effacer la preuve (on n'observe plus).
                    t.run = u32::MAX;
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::player::{MoveInput, Player};
    use glam::Vec3;
    use minelib_world::{BlockId, ChunkPos, World};

    fn flat() -> World {
        let mut w = World::new(0);
        for dz in -2..=2 {
            for dx in -2..=2 {
                w.insert_empty(ChunkPos { x: dx, z: dz });
            }
        }
        for z in -40..40 {
            for x in -40..40 {
                w.set(x, 10, z, BlockId::STONE);
            }
        }
        w
    }

    /// Petit générateur pseudo-aléatoire (reproductible).
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> f32 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((self.0 >> 33) as f32) / (u32::MAX >> 1) as f32
        }
        /// Loi normale (Box-Muller).
        fn normal(&mut self) -> f32 {
            let (a, b) = (self.next().max(1e-6), self.next());
            (-2.0 * a.ln()).sqrt() * (std::f32::consts::TAU * b).cos()
        }
    }

    /// Joue `secs` secondes de bunny hop en mode FPS : `press(rng, pas avant
    /// l'atterrissage prévu, pas passés au sol (0 en l'air))` dit s'il appuie à ce pas.
    /// Rend ce qui est trouvé.
    fn hop(
        secs: f32,
        mut press: impl FnMut(&mut Rng, i32, i32) -> bool,
        seed: u64,
    ) -> Option<Finding> {
        let w = flat();
        let mut p = Player::new(Vec3::new(0.5, 11.0, 0.5));
        let mut rng = Rng(seed);
        let mut tr = HopTracker::default();
        let dt = 1.0 / TICKS_PER_S as f32;
        let mut grounded = 0;
        for tick in 0..(secs * TICKS_PER_S as f32) as u32 {
            // Pas avant l'atterrissage (chute libre prévue : y et vitesse).
            let until = if p.on_ground {
                0
            } else {
                let (y, v) = (p.pos.y - 11.0, p.vel.y);
                let g = crate::player::cs::GRAVITY;
                let t = (v + (v * v + 2.0 * g * y.max(0.0)).sqrt()) / g;
                (t / dt).round() as i32
            };
            // Départ arrêté, ou saut raté (resté au sol) : on rappuie, comme un joueur.
            grounded = if p.on_ground { grounded + 1 } else { 0 };
            let pressed = press(&mut rng, until, grounded) || tick == 0 || grounded == 30;
            let m = MoveInput {
                forward: 1.0,
                fps: true,
                jump_pressed: pressed,
                sprint: true,
                ..Default::default()
            };
            let before = p.on_ground;
            p.update(&w, &m, dt);
            let jumped = before && !p.on_ground && p.vel.y > 0.0;
            tr.observe(1, tick, pressed, before, p.on_ground, jumped);
            if p.pos.x > 35.0 || p.pos.z > 35.0 {
                p.pos = Vec3::new(0.5, p.pos.y, 0.5);
            }
        }
        tr.take_found().pop().map(|f| f.1)
    }

    #[test]
    fn bhop_scripts_are_caught_and_humans_are_not() {
        // Script 1 : un appui pile au pas d'avant l'atterrissage.
        let found = hop(30.0, |_, until, ground| ground == 0 && until == 1, 1);
        assert!(matches!(found, Some(Finding::PerfectRun(_))), "{found:?}");
        // Script 2 : instant tiré au hasard dans la fenêtre de 1/64 s.
        let mut want = 0;
        let found = hop(
            30.0,
            move |r, until, ground| {
                if ground > 0 {
                    return false;
                }
                if until == 5 {
                    want = 1 + (r.next() * 1.99) as i32;
                }
                until == want
            },
            2,
        );
        assert!(found.is_some(), "script au hasard : {found:?}");
        // Script 3 : rafale d'appuis à chaque image (144 im/s) près du sol.
        let found = hop(
            10.0,
            |_, until, ground| ground == 0 && until < 40 && until % 2 == 0,
            3,
        );
        assert!(matches!(found, Some(Finding::Spam(_))), "{found:?}");
        // Script 4 « humanisé » : parfait 9 fois sur 10, raté exprès sinon.
        let found = hop(
            120.0,
            |r, until, ground| ground == 0 && until == if r.next() < 0.9 { 1 } else { 0 },
            4,
        );
        assert!(found.is_some(), "script humanisé à 90 % : {found:?}");
        // Humains : un appui par saut, en avance ou en retard de ±25 ms autour de
        // l'atterrissage (ce qu'un très bon joueur fait), plusieurs parties.
        for seed in 10..40 {
            let mut aim = None::<i32>;
            let found = hop(
                90.0,
                move |r, until, ground| {
                    if ground == 0 && until == 30 {
                        // Instant visé : 1 pas avant l'atterrissage, avec l'erreur humaine
                        // (en pas de 1/256 s, négatif : après l'atterrissage).
                        aim = Some(1 + (r.normal() * 25.0 / 3.9).round() as i32);
                    }
                    let hit = match aim {
                        Some(a) if a >= 0 => ground == 0 && until == a,
                        Some(a) => ground == -a,
                        None => false,
                    };
                    if hit {
                        aim = None;
                    }
                    hit
                },
                seed,
            );
            assert!(
                found.is_none(),
                "humain pris pour un script (graine {seed}) : {found:?}"
            );
        }
        // Humain qui martèle la touche (une dizaine d'appuis par seconde, irréguliers) :
        // jamais pris non plus.
        let mut next = 0i32;
        let found = hop(
            60.0,
            move |r, _, _| {
                next -= 1;
                if next <= 0 {
                    next = (TICKS_PER_S as f32 / 11.0 * (0.8 + 0.4 * r.next())) as i32;
                    true
                } else {
                    false
                }
            },
            7,
        );
        assert!(found.is_none(), "{found:?}");
    }
}
