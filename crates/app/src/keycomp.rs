//! Compensation du retard mesuré d'une touche « avancer » (v181).
//!
//! `/testtouche` (voir `keytest`) a mesuré, contre un bouton de la souris, que la touche
//! « avancer » du portable du joueur annonce son relâchement en retard dès qu'elle a été
//! tenue plus d'une seconde environ : 98 à 391 ms sur 16 essais de 1,5 à 11 s (aucun sous
//! 98 ms), contre 0 à 59 ms pour les appuis de moins d'une seconde. Pendant ce temps le
//! clavier la dit enfoncée (il la répète) : aucun logiciel ne voit le doigt se lever, et le
//! joueur continue d'avancer.
//!
//! Le jeu ne peut pas arrêter le joueur plus tôt (il arrêterait aussi celui qui tient la
//! touche : c'était le défaut de la règle « touche lente »). Mais quand le relâchement
//! arrive enfin, il sait que le doigt s'est levé au moins `lag` secondes plus tôt : il
//! reprend au joueur ce qu'il a avancé pendant ce temps-là, en le ramenant en douceur
//! (`SMOOTH_S`) là où il était. Rien ne se passe tant que la touche est tenue ; un nouvel
//! appui annule le retour.
//!
//! Le retard compensé est le bas de ce qui a été mesuré (voir `estimate`) : le joueur n'est
//! presque jamais ramené plus loin que là où il a lâché, et il reste en moyenne quelques
//! centièmes de seconde d'avance, comme le freinage normal.

use std::collections::VecDeque;

use glam::Vec3;
use minelib_platform::KeyCode;

use crate::keytest::Trial;

/// Fichier du réglage (dossier du jeu) : « KeyW 0.122 », précédé de « non » s'il est coupé.
const FILE: &str = "touche_retard";
/// Appui (s) en dessous duquel la touche remonte normalement, et à partir duquel le retard
/// est entier (entre les deux : en proportion ; mesuré : les deux se mêlent de 1,1 à 1,4 s).
const HOLD_FROM: f32 = 1.0;
const HOLD_FULL: f32 = 1.5;
/// Le retour se fait en ce temps (s).
const SMOOTH_S: f32 = 0.1;
/// Chemin gardé en mémoire (s), et retard compensé au plus (s).
const KEEP_S: f32 = 0.6;
pub const MAX_LAG: f32 = 0.5;
/// Écart normal (ms) entre une touche saine et le clic lâchés « ensemble », quand le test
/// n'a pas assez d'appuis brefs pour le mesurer.
const BASELINE_MS: f32 = 25.0;

/// Retard à compenser (s) d'après les essais d'une touche, ou `None` s'il n'y a pas de quoi
/// (moins de 3 essais de `HOLD_FULL` s et plus) ou si elle ne traîne pas.
///
/// Le 35ᵉ centile des écarts des appuis longs, moins l'écart normal du geste (médiane des
/// appuis de moins d'une seconde) : assez bas pour ne presque jamais ramener le joueur plus
/// loin que là où il a lâché.
pub fn estimate(trials: &[Trial], key: KeyCode) -> Option<f32> {
    let gaps = |f: &dyn Fn(f32) -> bool| {
        let mut g: Vec<f32> = trials
            .iter()
            .filter(|t| t.key == key && f(t.held))
            .map(|t| t.gap_ms)
            .collect();
        g.sort_by(f32::total_cmp);
        g
    };
    let long = gaps(&|h| h >= HOLD_FULL);
    if long.len() < 3 {
        return None;
    }
    let short = gaps(&|h| h < HOLD_FROM);
    let base = if short.len() >= 3 {
        short[short.len() / 2]
    } else {
        BASELINE_MS
    };
    let low = long[(long.len() as f32 * 0.35) as usize];
    let lag = (low - base) / 1000.0;
    (lag >= 0.05).then_some(lag.min(MAX_LAG))
}

/// Réglage retenu : la touche mesurée, son retard (s), et s'il est compensé.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Setting {
    pub key: KeyCode,
    pub lag: f32,
    pub on: bool,
}

fn parse(text: &str) -> Option<Setting> {
    let mut words = text.split_whitespace().peekable();
    let on = words.peek() != Some(&"non");
    if !on {
        words.next();
    }
    let name = words.next()?;
    let lag: f32 = words.next()?.parse().ok()?;
    let crate::keys::Bind::Key(key) = crate::keys::Bind::from_name(name)? else {
        return None;
    };
    (lag > 0.0 && lag <= MAX_LAG).then_some(Setting { key, lag, on })
}

pub fn load() -> Option<Setting> {
    parse(&std::fs::read_to_string(minelib_assets::data_dir().join(FILE)).ok()?)
}

pub fn save(s: &Setting) {
    let text = format!("{}{:?} {:.3}", if s.on { "" } else { "non " }, s.key, s.lag);
    if let Err(e) = std::fs::write(minelib_assets::data_dir().join(FILE), text) {
        log::warn!("retard de touche : réglage pas enregistré ({e})");
    }
}

/// Un pas de simulation gardé en mémoire.
#[derive(Clone, Copy)]
struct Step {
    /// Chemin fait vers l'avant pendant ce pas.
    forward: Vec3,
    /// Hauteur des pieds et appui au sol à la fin du pas.
    y: f32,
    on_ground: bool,
}

/// Ce qu'il faut rendre au joueur à ce pas.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Nudge {
    /// Déplacement horizontal (à faire avec les collisions).
    pub back: Vec3,
    /// Hauteur à reprendre tout de suite (il est tombé d'un bord pendant le retard).
    pub lift: f32,
}

/// Compensation en cours de partie.
#[derive(Default)]
pub struct Comp {
    steps: VecDeque<Step>,
    held: f32,
    was: bool,
    /// Retour en cours : ce qu'il reste à rendre, et la part de chaque pas.
    left: Vec3,
    rate: Vec3,
    /// Dernier retour décidé (s compensées, blocs), pour le journal.
    pub last: Option<(f32, f32)>,
}

impl Comp {
    /// Retard compensé (s) après un appui de `held` secondes, pour une touche qui traîne de
    /// `lag` secondes.
    fn lag_for(lag: f32, held: f32) -> f32 {
        lag * ((held - HOLD_FROM) / (HOLD_FULL - HOLD_FROM)).clamp(0.0, 1.0)
    }

    /// Un pas de simulation (`dt` secondes) vient d'être fait : « avancer » tenue (`fwd`),
    /// direction du regard, déplacement du joueur pendant ce pas, hauteur de ses pieds et
    /// appui au sol, chute en cours. `lag` : retard de la touche (0 : aucune compensation).
    /// Renvoie ce qu'il faut rendre au joueur maintenant.
    #[allow(clippy::too_many_arguments)]
    pub fn tick(
        &mut self,
        dt: f32,
        lag: f32,
        fwd: bool,
        yaw: f32,
        moved: Vec3,
        feet_y: f32,
        on_ground: bool,
        falling: bool,
    ) -> Nudge {
        let mut out = Nudge::default();
        if fwd {
            if !self.was {
                // Nouvel appui : c'est voulu, plus rien à rendre.
                self.held = 0.0;
                self.left = Vec3::ZERO;
                self.steps.clear();
            }
            self.held += dt;
            let (s, c) = yaw.sin_cos();
            let dir = Vec3::new(s, 0.0, -c);
            let along = (moved.x * dir.x + moved.z * dir.z).max(0.0);
            self.steps.push_back(Step {
                forward: dir * along,
                y: feet_y,
                on_ground,
            });
            while self.steps.len() as f32 * dt > KEEP_S {
                self.steps.pop_front();
            }
        } else if self.was {
            // Relâchement annoncé : le doigt s'est levé `lag` plus tôt.
            let lag = Self::lag_for(lag, self.held);
            let n = ((lag / dt).round() as usize).min(self.steps.len());
            if n > 0 {
                let from = self.steps.len() - n;
                let back: Vec3 = self.steps.iter().skip(from).map(|s| s.forward).sum();
                self.left = -back;
                self.rate = self.left * (dt / SMOOTH_S).min(1.0);
                // Au sol quand le doigt s'est levé, en chute maintenant : il est tombé d'un
                // bord pendant le retard, on l'y remonte.
                // (État à la fin du pas d'avant : le moment où le doigt s'est levé.)
                let then = self.steps[from.saturating_sub(1)];
                if then.on_ground && falling && then.y > feet_y && then.y - feet_y < 2.0 {
                    // Remonté et ramené d'un coup : un retour en douceur le laisserait
                    // retomber contre le flanc du bloc.
                    out.lift = then.y - feet_y;
                    self.rate = self.left;
                }
                self.last = Some((n as f32 * dt, back.length()));
            }
            self.steps.clear();
        }
        self.was = fwd;
        if self.left != Vec3::ZERO {
            let step = if self.rate.length_squared() >= self.left.length_squared() {
                self.left
            } else {
                self.rate
            };
            self.left -= step;
            if self.left.length_squared() < 1e-8 {
                self.left = Vec3::ZERO;
            }
            out.back = step;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DT: f32 = 1.0 / 64.0;

    /// Avance tout droit (vers -z) à `speed` blocs/s : `hold` s de vrai appui, puis `lag` s
    /// où la touche reste annoncée enfoncée, puis relâchée. Renvoie la position finale (le
    /// long du chemin) et celle où le doigt s'est levé.
    fn run(c: &mut Comp, setting: f32, speed: f32, hold: f32, lag: f32) -> (f32, f32) {
        let mut x = 0.0f32;
        let step = Vec3::new(0.0, 0.0, -speed * DT);
        let go = |c: &mut Comp, fwd: bool, x: &mut f32| {
            let moved = if fwd { step } else { Vec3::ZERO };
            if fwd {
                *x += speed * DT;
            }
            let n = c.tick(DT, setting, fwd, 0.0, moved, 10.0, true, false);
            // (Avancer : vers -z ; `back.z` positif ramène en arrière.)
            *x -= n.back.z;
            assert_eq!(n.lift, 0.0);
        };
        for _ in 0..(hold / DT).round() as usize {
            go(c, true, &mut x);
        }
        let lifted = x;
        for _ in 0..(lag / DT).round() as usize {
            go(c, true, &mut x);
        }
        for _ in 0..32 {
            go(c, false, &mut x);
        }
        (x, lifted)
    }

    #[test]
    fn the_player_comes_back_to_where_the_finger_lifted() {
        // Touche qui traîne de 0,2 s après un appui de 5 s, réglage 0,122 s : il reste
        // 0,08 s de marche (0,34 bloc) au lieu de 0,2 s (0,86 bloc).
        let mut c = Comp::default();
        let (end, lifted) = run(&mut c, 0.122, 4.3, 5.0, 0.2);
        let over = end - lifted;
        assert!((over - 4.3 * 0.078).abs() < 0.08, "{over}");
        let (secs, blocks) = c.last.unwrap();
        assert!((secs - 0.125).abs() < 0.01 && (blocks - 0.54).abs() < 0.03, "{secs} {blocks}");
        // Appui bref (0,6 s) : la touche remonte bien, rien n'est repris.
        let mut c = Comp::default();
        let (end, lifted) = run(&mut c, 0.122, 4.3, 0.6, 0.0);
        assert!((end - lifted).abs() < 1e-4 && c.last.is_none());
        // Sans réglage : rien.
        let mut c = Comp::default();
        let (end, lifted) = run(&mut c, 0.0, 4.3, 5.0, 0.2);
        assert!((end - lifted - 0.86).abs() < 0.08 && c.last.is_none(), "{}", end - lifted);
    }

    #[test]
    fn holding_never_moves_back_and_a_new_press_cancels_the_return() {
        let mut c = Comp::default();
        let step = Vec3::new(0.0, 0.0, -0.1);
        // Tant que la touche est tenue : jamais rien.
        for _ in 0..640 {
            assert_eq!(c.tick(DT, 0.3, true, 0.0, step, 10.0, true, false), Nudge::default());
        }
        // Relâchée : le retour commence ; rappuyée aussitôt : il s'arrête net.
        let n = c.tick(DT, 0.3, false, 0.0, Vec3::ZERO, 10.0, true, false);
        assert!(n.back.z > 0.0, "{n:?}");
        assert_eq!(c.tick(DT, 0.3, true, 0.0, step, 10.0, true, false), Nudge::default());
        // Seul le chemin vers l'avant est repris (pas le déplacement de côté ni en arrière).
        let mut c = Comp::default();
        let side = Vec3::new(0.1, 0.0, 0.0);
        for _ in 0..640 {
            c.tick(DT, 0.3, true, 0.0, side, 10.0, true, false);
        }
        let n = c.tick(DT, 0.3, false, 0.0, Vec3::ZERO, 10.0, true, false);
        assert_eq!(n, Nudge::default());
    }

    #[test]
    fn a_fall_from_an_edge_during_the_lag_is_undone() {
        let mut c = Comp::default();
        let step = Vec3::new(0.0, 0.0, -0.07);
        for _ in 0..640 {
            c.tick(DT, 0.2, true, 0.0, step, 10.0, true, false);
        }
        // Pendant le retard : il quitte le bord et tombe.
        for i in 0..13 {
            c.tick(DT, 0.2, true, 0.0, step, 10.0 - 0.02 * i as f32, false, true);
        }
        let n = c.tick(DT, 0.2, false, 0.0, Vec3::ZERO, 9.7, false, true);
        // Remonté, et ramené d'un seul coup de tout le chemin du retard (13 pas).
        assert!((n.lift - 0.3).abs() < 0.05, "{n:?}");
        assert!((n.back.z - 13.0 * 0.07).abs() < 0.01, "{n:?}");
    }

    #[test]
    fn the_lag_is_estimated_from_the_low_side_of_the_long_holds() {
        let k = KeyCode::KeyW;
        let t = |held: f32, gap_ms: f32| Trial { key: k, held, gap_ms };
        // Les essais du joueur (18 novembre) : appuis longs, puis quelques brefs.
        let mut trials: Vec<Trial> = [
            (10.5, 129.0), (10.9, 196.0), (5.7, 98.0), (5.0, 151.0), (2.7, 145.0), (2.0, 130.0),
            (1.8, 130.0), (2.1, 120.0), (4.4, 266.0), (5.8, 391.0), (2.1, 245.0), (1.7, 226.0),
            (1.5, 216.0), (1.7, 200.0), (1.6, 213.0), (1.7, 201.0),
        ]
        .into_iter()
        .map(|(h, g)| t(h, g))
        .collect();
        trials.extend([8.0, 15.0, 27.0, 23.0, 38.0, 20.0, 19.0, 34.0, 23.0].map(|g| t(0.4, g)));
        let lag = estimate(&trials, k).unwrap();
        assert!((lag - 0.122).abs() < 0.002, "{lag}");
        // Touche saine, ou pas assez d'essais longs : rien.
        let healthy: Vec<Trial> = [20.0, 31.0, 12.0, 40.0].map(|g| t(8.0, g)).to_vec();
        assert_eq!(estimate(&healthy, k), None);
        assert_eq!(estimate(&trials[..2], k), None);
        assert_eq!(estimate(&trials, KeyCode::KeyE), None);
        // Le réglage s'écrit et se relit.
        assert_eq!(
            parse("KeyW 0.122"),
            Some(Setting { key: k, lag: 0.122, on: true })
        );
        assert_eq!(parse("non KeyW 0.122").map(|s| s.on), Some(false));
        assert_eq!(parse("KeyW 3"), None);
        assert_eq!(parse("Rien 0.1"), None);
    }
}
