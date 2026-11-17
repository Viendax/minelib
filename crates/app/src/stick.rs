//! Touche « avancer » qui remonte mal (clavier de portable, coupelle fatiguée) : après un
//! long appui, le clavier la dit encore enfoncée une seconde après qu'on l'a lâchée, et le
//! joueur continue d'avancer. Aucun logiciel ne voit mieux que le clavier ; mais un joueur
//! qui court lâche presque toujours « avancer » et « courir » ensemble, et « courir »,
//! elle, remonte bien.
//!
//! Le jeu apprend donc tout seul : si, chez ce joueur, « avancer » arrive régulièrement
//! un instant après « courir » (ni ensemble, ni longtemps après), c'est que sa touche
//! traîne. Dès lors, lâcher « courir » arrête d'avancer tout de suite ; si « avancer »
//! reste vraiment enfoncée (le joueur voulait continuer en marchant), la marche reprend
//! au bout d'une seconde et demie. Chez un joueur dont le clavier va bien, la règle ne
//! s'active jamais.

/// Les deux touches tenues ensemble au moins ce temps (s) pour qu'un relâchement compte.
const HELD_MIN: f32 = 3.0;
/// « Avancer » relâchée entre ces deux délais (s) après « courir » : elle traînait.
const LATE_FROM: f32 = 0.2;
const LATE_TO: f32 = 1.5;
/// Relâchements retenus, et combien il en faut pour juger.
const HISTORY: usize = 8;
const ENOUGH: usize = 4;

#[derive(Default)]
pub struct Guard {
    /// Temps depuis lequel les deux touches sont tenues ensemble.
    both: f32,
    mod_was: bool,
    /// « Courir » vient d'être lâchée avec « avancer » encore tenue : temps écoulé depuis.
    waiting: Option<f32>,
    /// Derniers relâchements : vrai si « avancer » traînait.
    history: std::collections::VecDeque<bool>,
    active: bool,
    /// Avancer est retenue en attendant que la touche remonte.
    blocked: bool,
    /// La règle vient de s'activer (à dire au joueur, une fois).
    pub just_learned: bool,
    /// Ce qui est noté dans le fichier.
    saved: bool,
}

/// Fichier (dossier du jeu) qui retient que la touche traîne, d'une partie à l'autre.
const FILE: &str = "touche_avancer_lente";

impl Guard {
    /// Reprend ce qui a été appris aux parties précédentes.
    pub fn restored() -> Self {
        let mut g = Self::default();
        if minelib_assets::data_dir().join(FILE).exists() {
            g.active = true;
            g.history = std::iter::repeat_n(true, HISTORY).collect();
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

    fn sample(&mut self, late: bool) {
        self.history.push_back(late);
        if self.history.len() > HISTORY {
            self.history.pop_front();
        }
        let n = self.history.len();
        let lates = self.history.iter().filter(|&&l| l).count();
        if !self.active && n >= ENOUGH && lates * 4 >= n * 3 {
            self.active = true;
            self.just_learned = true;
        } else if self.active && lates * 2 < n {
            // Le clavier va mieux (ou c'était un autre joueur) : on arrête.
            self.active = false;
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
                self.sample(t >= LATE_FROM);
            } else if t > LATE_TO || modifier {
                // Toujours tenue : le joueur voulait continuer (ou il recourt).
                self.waiting = None;
                self.blocked = false;
                if t > LATE_TO {
                    self.sample(false);
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

    const DT: f32 = 1.0 / 100.0;

    /// Court `run` secondes, lâche « courir », puis « avancer » `gap` secondes après
    /// (`None` : continue de marcher 3 s). Renvoie le temps passé à avancer après avoir
    /// lâché « courir ».
    fn sprint(g: &mut Guard, run: f32, gap: Option<f32>) -> f32 {
        let mut first = true;
        for _ in 0..(run / DT) as usize {
            g.update(DT, true, std::mem::take(&mut first), true);
        }
        let mut moved = 0.0;
        let hold = gap.unwrap_or(3.0);
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
        for i in 0..30 {
            // Relâchées ensemble (à 3 centièmes près), ou on continue en marchant.
            let gap = if i % 3 == 0 { None } else { Some(0.03) };
            let moved = sprint(&mut g, 6.0, gap);
            assert!(!g.active());
            assert!(
                (moved - gap.unwrap_or(3.0)).abs() < 0.02,
                "rien n'est retenu"
            );
        }
    }

    #[test]
    fn a_slow_forward_key_is_learned_then_stopped_at_once() {
        let mut g = Guard::default();
        // Les quatre premières fois, la touche traîne et le joueur avance encore.
        for _ in 0..4 {
            assert!(sprint(&mut g, 10.0, Some(0.9)) > 0.8);
        }
        assert!(g.active() && g.just_learned);
        // Ensuite, lâcher « courir » arrête d'avancer tout de suite.
        for _ in 0..5 {
            assert!(sprint(&mut g, 10.0, Some(0.9)) < 0.02);
        }
        // Il veut vraiment continuer en marchant : la marche reprend après 1,5 s.
        let moved = sprint(&mut g, 10.0, None);
        assert!((moved - 1.5).abs() < 0.05, "{moved}");
        assert!(g.active(), "une fois ne suffit pas à l'oublier");
        // Un sprint court (moins de 3 s) n'est jamais retenu.
        assert!((sprint(&mut g, 1.0, Some(0.5)) - 0.5).abs() < 0.02);
        // Le clavier est changé : la règle s'éteint d'elle-même.
        for _ in 0..8 {
            sprint(&mut g, 6.0, Some(0.02));
        }
        assert!(!g.active());
    }

    #[test]
    fn pressing_forward_again_always_moves() {
        let mut g = Guard::default();
        for _ in 0..4 {
            sprint(&mut g, 10.0, Some(0.9));
        }
        for _ in 0..500 {
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
