//! Touche « avancer » qui remonte mal (clavier de portable : la coupelle sous la touche,
//! usée et chauffée par la machine, met du temps à se relever). Le clavier la dit encore
//! enfoncée après qu'on l'a lâchée, d'un dixième de seconde à froid jusqu'à une seconde
//! et plus à chaud, et le joueur continue d'avancer, dans tous les jeux. Aucun logiciel ne
//! voit mieux que le clavier ; mais un joueur qui court lâche « avancer » et « courir »
//! ensemble, et « courir », elle, remonte bien.
//!
//! Le jeu mesure donc, à chaque fin de course, le temps entre « courir » relâchée et
//! « avancer » relâchée. Trois fois de suite nettement après (0,18 s ou plus) : la touche
//! traîne. Dès lors, lâcher « courir » arrête d'avancer tout de suite, sans attendre
//! « avancer ». Si elle reste vraiment enfoncée (le joueur voulait continuer en marchant),
//! la marche reprend après un délai réglé sur ce que la touche a montré de pire. La règle
//! se retient d'une partie à l'autre ; elle s'éteint si le joueur continue souvent de
//! marcher après avoir couru. Chez un joueur dont le clavier va bien, elle ne s'active
//! jamais.

/// Les deux touches tenues ensemble au moins ce temps (s) pour qu'un relâchement compte.
const HELD_MIN: f32 = 1.5;
/// « Avancer » relâchée au moins ce temps (s) après « courir » : elle traînait.
const LATE_FROM: f32 = 0.18;
/// Au-delà (s), ce n'est plus une touche qui traîne : le joueur continue en marchant.
const LATE_TO: f32 = 2.0;
/// Relâchements en retard à la suite qui activent la règle.
const IN_A_ROW: usize = 3;
/// Relâchements retenus (pour l'éteindre, et pour régler l'attente).
const HISTORY: usize = 8;
/// Fichier (dossier du jeu) qui retient que la touche traîne, d'une partie à l'autre.
const FILE: &str = "touche_avancer_lente";

/// Ce qui a suivi « courir » relâchée.
#[derive(Clone, Copy, PartialEq, Debug)]
enum After {
    /// « Avancer » relâchée ce temps (s) après.
    Stop(f32),
    /// « Avancer » gardée : le joueur a continué en marchant.
    Walk,
}

#[derive(Default)]
pub struct Guard {
    /// Temps depuis lequel les deux touches sont tenues ensemble.
    both: f32,
    mod_was: bool,
    /// « Courir » vient d'être lâchée avec « avancer » encore tenue : temps écoulé depuis.
    waiting: Option<f32>,
    history: std::collections::VecDeque<After>,
    active: bool,
    /// Avancer est retenue en attendant que la touche remonte.
    blocked: bool,
    /// La règle vient de s'activer (à dire au joueur, une fois).
    pub just_learned: bool,
    /// Dernier retard mesuré (s), pour le journal.
    pub last_gap: Option<f32>,
    /// Ce qui est noté dans le fichier.
    saved: bool,
}

impl Guard {
    /// Reprend ce qui a été appris aux parties précédentes.
    pub fn restored() -> Self {
        let mut g = Self::default();
        if minelib_assets::data_dir().join(FILE).exists() {
            g.active = true;
        }
        g.saved = g.active;
        g
    }

    /// Retient (ou oublie) la règle pour les prochaines parties, quand elle change.
    pub fn remember(&mut self) {
        if self.saved != self.active {
            self.saved = self.active;
            let path = minelib_assets::data_dir().join(FILE);
            let _ = if self.active {
                std::fs::write(path, "1")
            } else {
                std::fs::remove_file(path)
            };
        }
    }

    /// La règle est-elle active pour ce joueur ?
    pub fn active(&self) -> bool {
        self.active
    }

    /// Temps (s) pendant lequel « avancer » est retenue après « courir » relâchée : un peu
    /// plus que le pire retard vu récemment (une seconde et demie tant qu'on n'en sait rien).
    fn patience(&self) -> f32 {
        let worst = self
            .history
            .iter()
            .filter_map(|a| match a {
                After::Stop(g) => Some(*g),
                After::Walk => None,
            })
            .fold(None, |m: Option<f32>, g| Some(m.map_or(g, |m| m.max(g))));
        worst.map_or(1.5, |w| (w * 1.3 + 0.3).clamp(0.6, LATE_TO))
    }

    fn sample(&mut self, after: After) {
        self.history.push_back(after);
        if self.history.len() > HISTORY {
            self.history.pop_front();
        }
        let late = |a: &After| matches!(a, After::Stop(g) if *g >= LATE_FROM);
        if !self.active {
            let n = self.history.len();
            if n >= IN_A_ROW && self.history.iter().skip(n - IN_A_ROW).all(late) {
                self.active = true;
                self.just_learned = true;
            }
        } else {
            // Il continue souvent de marcher après avoir couru : la règle le gênerait.
            let walks = self.history.iter().filter(|a| **a == After::Walk).count();
            if walks * 2 >= HISTORY {
                self.active = false;
            }
        }
    }

    /// À chaque image : « avancer » tenue, « avancer » enfoncée à cette image, « courir »
    /// (ou « marcher » en FPS) tenue. Renvoie vrai s'il ne faut pas avancer.
    pub fn update(&mut self, dt: f32, fwd: bool, fwd_pressed: bool, modifier: bool) -> bool {
        if fwd_pressed {
            // Nouvel appui : c'est voulu.
            self.blocked = false;
            self.waiting = None;
        }
        if let Some(t) = &mut self.waiting {
            *t += dt;
            let t = *t;
            if !fwd {
                // Relâchée : tout de suite (clavier sain), ou un instant après (elle traînait).
                self.waiting = None;
                self.blocked = false;
                self.last_gap = Some(t);
                self.sample(After::Stop(t));
            } else if modifier {
                // Il recourt : on n'en sait pas plus.
                self.waiting = None;
                self.blocked = false;
            } else {
                if t > LATE_TO {
                    self.waiting = None;
                    self.sample(After::Walk);
                }
                if t > self.patience() {
                    // Toujours tenue : le joueur voulait continuer en marchant.
                    self.blocked = false;
                }
            }
        }
        if fwd && modifier {
            self.both += dt;
        }
        if self.mod_was && !modifier && fwd && self.both >= HELD_MIN {
            self.waiting = Some(0.0);
            self.blocked = self.active;
        }
        if !fwd || !modifier {
            self.both = 0.0;
        }
        if !fwd {
            self.blocked = false;
        }
        self.mod_was = modifier;
        self.blocked
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DT: f32 = 1.0 / 200.0;

    /// Court `run` secondes, lâche « courir », puis « avancer » `gap` secondes après
    /// (`None` : continue de marcher 4 s). Renvoie le temps passé à avancer après avoir
    /// lâché « courir ».
    fn sprint(g: &mut Guard, run: f32, gap: Option<f32>) -> f32 {
        let mut first = true;
        for _ in 0..(run / DT) as usize {
            g.update(DT, true, std::mem::take(&mut first), true);
        }
        let mut moved = 0.0;
        let hold = gap.unwrap_or(4.0);
        for _ in 0..(hold / DT).round() as usize {
            if !g.update(DT, true, false, false) {
                moved += DT;
            }
        }
        for _ in 0..20 {
            g.update(DT, false, false, false);
        }
        moved
    }

    #[test]
    fn a_healthy_keyboard_never_triggers_the_rule() {
        let mut g = Guard::default();
        for i in 0..40 {
            // Relâchées presque ensemble (jusqu'à 0,15 s d'écart, des doigts qui ne partent
            // pas au même instant), ou on continue en marchant.
            let gap = match i % 4 {
                0 => None,
                k => Some(0.05 * k as f32),
            };
            let moved = sprint(&mut g, 6.0, gap);
            assert!(!g.active());
            assert!(
                (moved - gap.unwrap_or(4.0)).abs() < 0.02,
                "rien n'est retenu"
            );
        }
    }

    /// Les écarts mesurés sur le portable du joueur qui a signalé le défaut, dans l'ordre
    /// (la touche traîne de plus en plus à mesure que la machine chauffe).
    #[test]
    fn a_key_that_gets_slower_as_the_laptop_warms_is_learned_then_stopped_at_once() {
        let mut g = Guard::default();
        let seen = [0.042, 0.117, 0.125, 0.200, 0.338, 0.549];
        for gap in seen {
            assert!(!g.active());
            sprint(&mut g, 17.0, Some(gap));
        }
        assert!(g.active() && g.just_learned, "trois retards de suite");
        // Ensuite, lâcher « courir » arrête d'avancer tout de suite, même si la touche
        // traîne de plus en plus.
        for gap in [0.6, 0.8, 1.0, 1.2, 1.4] {
            assert!(sprint(&mut g, 17.0, Some(gap)) < 0.02, "retard {gap}");
        }
        // Il veut vraiment continuer en marchant : la marche reprend (après le pire retard
        // vu, plus une marge), et une fois ne suffit pas à oublier la règle.
        let moved = sprint(&mut g, 17.0, None);
        assert!((1.5..3.5).contains(&moved), "{moved}");
        assert!(g.active());
        // Un sprint très court n'est jamais retenu.
        assert!((sprint(&mut g, 0.5, Some(0.5)) - 0.5).abs() < 0.02);
    }

    #[test]
    fn a_player_who_keeps_walking_after_sprinting_is_left_alone() {
        let mut g = Guard::default();
        for _ in 0..3 {
            sprint(&mut g, 10.0, Some(0.5));
        }
        assert!(g.active());
        // Il continue de marcher la plupart du temps : la règle s'éteint.
        for _ in 0..4 {
            sprint(&mut g, 10.0, None);
        }
        assert!(!g.active());
        assert!(
            (sprint(&mut g, 10.0, None) - 4.0).abs() < 0.02,
            "plus rien n'est retenu"
        );
    }

    #[test]
    fn pressing_forward_again_always_moves() {
        let mut g = Guard::default();
        for _ in 0..3 {
            sprint(&mut g, 10.0, Some(0.9));
        }
        for _ in 0..1000 {
            g.update(DT, true, false, true);
        }
        assert!(
            g.update(DT, true, false, false),
            "retenue au relâchement de courir"
        );
        g.update(DT, false, false, false);
        assert!(!g.update(DT, true, true, false), "nouvel appui : on avance");
    }
}
