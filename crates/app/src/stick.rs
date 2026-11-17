//! Touche « avancer » qui remonte mal (clavier de portable : la coupelle sous la touche,
//! usée et chauffée par la machine, met du temps à se relever). Le clavier la dit encore
//! enfoncée après qu'on l'a lâchée, de quelques centièmes de seconde à froid jusqu'à une
//! seconde et plus à chaud ou après un long appui, et le joueur continue d'avancer, dans
//! tous les jeux. Aucun logiciel ne voit mieux que le clavier ; mais un joueur qui court
//! lâche « avancer » et « courir » ensemble, et « courir », elle, remonte bien.
//!
//! Le jeu mesure donc, à chaque fin de course, le temps entre « courir » relâchée et
//! « avancer » relâchée. Trois fois de suite nettement après (0,18 à 1,5 s), sans une
//! seule fois ensemble entre-temps : la touche traîne. Dès lors, lâcher « courir » arrête
//! d'avancer tout de suite, après une longue course comme après un petit sprint, et à
//! chaque fois (la règle est la même partout : le joueur sait à quoi s'attendre). Si
//! « avancer » reste annoncée enfoncée un peu plus longtemps qu'elle n'a traîné récemment
//! pour un appui de cette durée (le jeu mesure chaque retard), c'est que le joueur la
//! tient vraiment, pour continuer en marchant : la marche reprend. Un nouvel appui
//! (« courir », ou « avancer » relâchée puis renfoncée) repart toujours tout de suite.
//!
//! La règle se retient d'une partie à l'autre, ne s'active jamais sur un clavier sain,
//! s'éteint si le joueur continue souvent de marcher après avoir couru, et se règle à la
//! main (`/touchelente on`, `off`, `auto`). Elle ne vaut pas en mode FPS, où « marcher »
//! relâchée veut dire courir. Elle ne peut rien quand « avancer » est lâchée seule (en
//! marchant) : rien ne dit alors au jeu que le doigt s'est levé.

/// « Courir » et « avancer » tenues ensemble au moins ce temps (s) pour que lâcher
/// « courir » retienne « avancer » (moins : la touche a été frôlée).
const TOGETHER_MIN: f32 = 0.1;
/// Tenues ensemble au moins ce temps (s) pour qu'un relâchement serve à apprendre si la
/// touche traîne (les petits sprints ne disent rien d'un clavier).
const HELD_MIN: f32 = 1.5;
/// « Avancer » relâchée moins de ce temps (s) après « courir » : ensemble (clavier sain).
const CLEAN: f32 = 0.12;
/// Entre ces deux temps (s) : elle traînait.
const LATE_FROM: f32 = 0.18;
const LATE_TO: f32 = 1.5;
/// Relâchements en retard à la suite qui activent la règle.
const IN_A_ROW: u32 = 3;
/// Fins de course retenues (pour éteindre la règle).
const HISTORY: usize = 8;
/// Retards retenus pour régler l'attente : assez pour que quelques petits sprints (où
/// la touche traîne peu) ne fassent pas oublier ce qu'elle fait après une longue course.
const LAGS: usize = 16;
/// Bornes du retard rapporté à `base_lag`. En dessous du plancher, l'attente serait trop
/// juste dès que la machine chauffe ; au-dessus du plafond, ce n'est plus la touche qui
/// traîne mais le joueur qui a continué un instant en marchant.
const SCALE_MIN: f32 = 0.5;
const SCALE_MAX: f32 = 2.0;
/// Fichier (dossier du jeu) qui retient le réglage d'une partie à l'autre : « 1 » (la
/// touche traîne, appris), « oui » (demandé par le joueur), « non » (refusé).
const FILE: &str = "touche_avancer_lente";

/// Réglage du joueur.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub enum Mode {
    /// Le jeu apprend tout seul.
    #[default]
    Auto,
    On,
    Off,
}

/// « Courir » vient d'être lâchée avec « avancer » encore tenue.
#[derive(Clone, Copy)]
struct Wait {
    /// Temps écoulé depuis.
    t: f32,
    /// Durée de l'appui sur « avancer » à ce moment, et temps passé à courir.
    held: f32,
    ran: f32,
    /// Le joueur a-t-il été arrêté, et combien de temps la marche a repris ensuite
    /// (attente dépassée) avant que la touche remonte.
    stopped: bool,
    resumed: f32,
}

/// Fin de course : « avancer » relâchée peu après « courir » (pour le journal).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stop {
    /// Retard d'« avancer » sur « courir » (s).
    pub gap: f32,
    /// Durée de l'appui sur « avancer », et temps passé à courir (s).
    pub held: f32,
    pub ran: f32,
    /// Attente prévue (s) : au-delà, la marche reprend.
    pub patience: f32,
    /// Arrêté dès « courir » relâchée ; et temps pendant lequel la marche avait repris
    /// avant que la touche remonte (0 : l'arrêt a tenu jusqu'au bout).
    pub stopped: bool,
    pub resumed: f32,
}

#[derive(Default)]
pub struct Guard {
    mode: Mode,
    /// Temps depuis lequel « avancer » est tenue, et les deux touches ensemble.
    fwd_time: f32,
    both: f32,
    mod_was: bool,
    /// Temps depuis que « courir » n'est plus tenue (pour le journal).
    since_mod: f32,
    waiting: Option<Wait>,
    /// Retards à la suite (appris), et dernières fins de course : vrai si le joueur a
    /// continué en marchant.
    streak: u32,
    walked: std::collections::VecDeque<bool>,
    /// Derniers retards mesurés, rapportés à `base_lag` (la touche traîne plus ou moins
    /// selon que la machine est chaude) : ils règlent l'attente.
    scales: std::collections::VecDeque<f32>,
    active: bool,
    /// Avancer est retenue en attendant un nouvel appui.
    blocked: bool,
    /// La règle vient de s'activer (à dire au joueur, une fois).
    pub just_learned: bool,
    /// Fin de course qui vient d'être mesurée (pour le journal).
    pub stop: Option<Stop>,
    /// Ce qui vient de se passer après « courir » relâchée (une ligne pour le journal).
    pub note: Option<String>,
    /// Ce qui est noté dans le fichier.
    saved: Option<(Mode, bool)>,
}

/// Retard attendu de la touche (s) après un appui de `held` secondes, à l'échelle 1 : il
/// grandit avec la durée de l'appui (mesuré à chaud : 0,3 s après un petit sprint, 0,4 s
/// après 3 s, 1,3 s après 36 s, et pas plus ensuite).
fn base_lag(held: f32) -> f32 {
    if held < 2.0 {
        0.25 + 0.1 * held
    } else {
        0.4 + 0.025 * held.min(40.0)
    }
}

impl Guard {
    /// Reprend ce qui a été appris ou réglé aux parties précédentes.
    pub fn restored() -> Self {
        let mut g = Self::default();
        match std::fs::read_to_string(minelib_assets::data_dir().join(FILE))
            .ok()
            .as_deref()
            .map(str::trim)
        {
            Some("non") => g.mode = Mode::Off,
            Some("oui") => {
                g.mode = Mode::On;
                g.active = true;
            }
            Some(_) => g.active = true,
            None => {}
        }
        g.saved = Some((g.mode, g.active));
        g
    }

    /// Retient le réglage pour les prochaines parties, quand il change.
    pub fn remember(&mut self) {
        let now = (self.mode, self.active);
        if self.saved.is_some() && self.saved != Some(now) {
            self.saved = Some(now);
            let path = minelib_assets::data_dir().join(FILE);
            let _ = match now {
                (Mode::Off, _) => std::fs::write(path, "non"),
                (Mode::On, _) => std::fs::write(path, "oui"),
                (Mode::Auto, true) => std::fs::write(path, "1"),
                (Mode::Auto, false) => std::fs::remove_file(path),
            };
        }
    }

    /// Réglage à la main (`/touchelente`).
    pub fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
        match mode {
            Mode::On => self.active = true,
            Mode::Off => {
                self.active = false;
                self.blocked = false;
            }
            Mode::Auto => {}
        }
        self.streak = 0;
        self.walked.clear();
    }

    /// Temps (s) pendant lequel « avancer » est retenue après « courir » relâchée, pour un
    /// appui de `held` secondes : un peu plus que le pire retard que la touche a montré
    /// récemment pour un appui de cette durée. Au-delà, elle est tenue pour de bon (le
    /// joueur continue en marchant) et la marche reprend.
    fn patience(&self, held: f32) -> f32 {
        let scale = self
            .scales
            .iter()
            .copied()
            .fold(None, |m: Option<f32>, v| Some(m.map_or(v, |m| m.max(v))))
            .unwrap_or(1.0)
            .max(SCALE_MIN);
        (1.2 * scale * base_lag(held) + 0.2).clamp(0.35, 4.0)
    }

    /// La règle est-elle active pour ce joueur ?
    pub fn active(&self) -> bool {
        self.active
    }

    /// Durée de l'appui en cours sur « avancer », et temps depuis que « courir » n'est
    /// plus tenue (s) : pour le journal, à lire avant `update`.
    pub fn forward_held(&self) -> f32 {
        self.fwd_time
    }
    pub fn since_sprint(&self) -> f32 {
        self.since_mod
    }

    /// Fin de course : « avancer » relâchée `gap` secondes après « courir » (`None` : le
    /// joueur a continué en marchant).
    fn sample(&mut self, gap: Option<f32>) {
        self.walked.push_back(gap.is_none());
        if self.walked.len() > HISTORY {
            self.walked.pop_front();
        }
        if self.mode != Mode::Auto {
            return;
        }
        match gap {
            Some(g) if (LATE_FROM..=LATE_TO).contains(&g) => self.streak += 1,
            // Relâchées ensemble : ce clavier va bien.
            Some(g) if g < CLEAN => self.streak = 0,
            _ => {}
        }
        if !self.active && self.streak >= IN_A_ROW {
            self.active = true;
            self.just_learned = true;
        }
        // Il continue souvent de marcher après avoir couru : la règle le gênerait.
        if self.active && self.walked.iter().filter(|w| **w).count() * 2 >= HISTORY {
            self.active = false;
            self.streak = 0;
            self.walked.clear();
        }
    }

    /// À chaque image : « avancer » tenue, « avancer » enfoncée à cette image, « courir »
    /// tenue. Renvoie vrai s'il ne faut pas avancer.
    pub fn update(&mut self, dt: f32, fwd: bool, fwd_pressed: bool, modifier: bool) -> bool {
        if fwd_pressed {
            // Nouvel appui : c'est voulu.
            self.blocked = false;
            self.waiting = None;
            self.fwd_time = 0.0;
        }
        if let Some(mut w) = self.waiting {
            w.t += dt;
            let patience = self.patience(w.held);
            if !fwd {
                self.waiting = None;
                self.blocked = false;
                // Un peu après l'attente, c'est encore la touche (elle traîne plus que
                // prévu : l'attente s'allongera) ; bien après, il marchait.
                if w.t <= patience + 0.25 {
                    self.stop = Some(Stop {
                        gap: w.t,
                        held: w.held,
                        ran: w.ran,
                        patience,
                        stopped: w.stopped,
                        resumed: w.resumed,
                    });
                    self.scales
                        .push_back((w.t / base_lag(w.held)).min(SCALE_MAX));
                    if self.scales.len() > LAGS {
                        self.scales.pop_front();
                    }
                    if w.ran >= HELD_MIN {
                        self.sample(Some(w.t));
                    }
                } else {
                    self.note = Some(format!(
                        "courir relâchée après {:.2} s de course, avancer gardée {:.2} s de                          plus (retenue {patience:.2} s)",
                        w.ran, w.t
                    ));
                    if w.ran >= HELD_MIN {
                        self.sample(None);
                    }
                }
            } else if modifier {
                // Il recourt sans avoir lâché « avancer » : il repart tout de suite. Le
                // prochain relâchement de « courir » l'arrêtera comme les autres.
                if w.stopped && w.t <= patience {
                    self.note = Some(format!(
                        "courir relâchée puis reprise après {:.2} s, avancer toujours tenue                          (retenue jusque-là)",
                        w.t
                    ));
                }
                self.waiting = None;
                self.blocked = false;
            } else {
                if w.t > patience {
                    // Toujours tenue après le pire retard connu : il la tient pour de bon.
                    self.blocked = false;
                    w.resumed += dt;
                }
                if w.t > patience + 6.0 {
                    self.waiting = None;
                    if w.ran >= HELD_MIN {
                        self.sample(None);
                    }
                } else {
                    self.waiting = Some(w);
                }
            }
        }
        if fwd {
            self.fwd_time += dt;
        }
        if fwd && modifier {
            self.both += dt;
        }
        if self.mod_was && !modifier && fwd && self.both >= TOGETHER_MIN {
            self.blocked = self.active;
            self.waiting = Some(Wait {
                t: 0.0,
                held: self.fwd_time,
                ran: self.both,
                stopped: self.blocked,
                resumed: 0.0,
            });
        }
        if !fwd || !modifier {
            self.both = 0.0;
        }
        if !fwd {
            self.blocked = false;
            self.fwd_time = 0.0;
        }
        self.since_mod = if modifier { 0.0 } else { self.since_mod + dt };
        self.mod_was = modifier;
        self.blocked
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DT: f32 = 1.0 / 200.0;

    /// Court `run` secondes, lâche « courir », puis « avancer » `gap` secondes après
    /// (`None` : continue de marcher 8 s). Renvoie le temps passé à avancer après avoir
    /// lâché « courir ».
    fn sprint(g: &mut Guard, run: f32, gap: Option<f32>) -> f32 {
        let mut first = true;
        for _ in 0..(run / DT) as usize {
            g.update(DT, true, std::mem::take(&mut first), true);
        }
        let mut moved = 0.0;
        let hold = gap.unwrap_or(8.0);
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
        for i in 0..60 {
            // Relâchées ensemble (à quelques centièmes près), parfois « avancer » un peu
            // après, parfois on continue en marchant : jamais trois retards de suite sans
            // un relâchement net entre deux. Les petits sprints ne comptent pas.
            let (run, gap) = match i % 6 {
                0 => (6.0, None),
                1 | 3 => (6.0, Some(0.04)),
                2 => (6.0, Some(0.6)),
                4 => (0.6, Some(0.3)),
                _ => (6.0, Some(0.25)),
            };
            let moved = sprint(&mut g, run, gap);
            assert!(!g.active(), "course {i}");
            assert!(
                (moved - gap.unwrap_or(8.0)).abs() < 0.02,
                "rien n'est retenu"
            );
        }
        // Même une série de petits sprints lâchés en décalé n'apprend rien.
        for _ in 0..10 {
            sprint(&mut g, 0.8, Some(0.3));
        }
        assert!(!g.active());
    }

    /// Les écarts mesurés sur le portable du joueur qui a signalé le défaut, dans l'ordre
    /// (la touche traîne de plus en plus à mesure que la machine chauffe).
    #[test]
    fn a_key_that_gets_slower_as_the_laptop_warms_is_learned_then_stopped_at_once() {
        let mut g = Guard::default();
        for gap in [0.042, 0.117, 0.125, 0.200, 0.338, 0.549] {
            assert!(!g.active());
            sprint(&mut g, 17.0, Some(gap));
        }
        assert!(g.active() && g.just_learned, "trois retards de suite");
        // Ensuite, lâcher « courir » arrête d'avancer tout de suite, même quand la touche
        // traîne de plus en plus (les retards mesurés ensuite sur ce portable, à chaud).
        for (run, gap) in [(20.0, 0.746), (36.0, 1.357), (50.0, 1.205), (12.0, 0.515)] {
            assert!(sprint(&mut g, run, Some(gap)) < 0.02, "retard {gap}");
            let s = g.stop.take().expect("fin de course mesurée");
            assert!(s.stopped && s.resumed == 0.0 && (s.gap - gap).abs() < 0.02);
        }
        assert!(g.active());
        // Il veut continuer en marchant après une course de 7 s : la marche reprend vite
        // (un peu plus que le retard de la touche pour un appui de cette durée), et une
        // fois ne suffit pas à oublier la règle.
        let moved = sprint(&mut g, 7.0, None);
        assert!((6.0..7.6).contains(&moved), "{moved}");
        assert!(g.active());
    }

    /// Les petits sprints (une demi-seconde, une seconde) sont arrêtés comme les longues
    /// courses : c'est là que le joueur voyait encore la touche traîner.
    #[test]
    fn short_sprints_are_stopped_at_once_too() {
        let mut g = Guard::default();
        g.set_mode(Mode::On);
        for (run, gap) in [
            (0.3, 0.05),
            (0.6, 0.14),
            (1.0, 0.2),
            (0.5, 0.3),
            (1.2, 0.25),
        ] {
            assert!(sprint(&mut g, run, Some(gap)) < 0.02, "sprint {run} s");
        }
        // Après un petit sprint, s'il continue en marchant, la marche reprend vite (en
        // deux tiers de seconde au plus, après les retards ci-dessus).
        let moved = sprint(&mut g, 0.6, None);
        assert!(moved > 7.3, "{moved}");
        // « Courir » seulement frôlée : rien n'est retenu.
        assert!((sprint(&mut g, 0.05, Some(0.5)) - 0.5).abs() < 0.02);
    }

    /// Quelques petits sprints, où la touche traîne peu, ne font pas oublier ce qu'elle
    /// fait après une longue course.
    #[test]
    fn short_sprints_do_not_shorten_the_wait_after_a_long_run() {
        let mut g = Guard::default();
        g.set_mode(Mode::On);
        assert!(sprint(&mut g, 20.0, Some(1.1)) < 0.02);
        for _ in 0..10 {
            assert!(sprint(&mut g, 0.5, Some(0.05)) < 0.02);
        }
        assert!(sprint(&mut g, 20.0, Some(1.1)) < 0.02);
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
            (sprint(&mut g, 10.0, None) - 8.0).abs() < 0.02,
            "plus rien n'est retenu"
        );
    }

    /// Un joueur qui continue un instant en marchant puis s'arrête ne fait pas grandir
    /// l'attente sans fin : au-delà d'un quart de seconde après l'attente, ce n'est plus
    /// compté comme un retard de la touche.
    #[test]
    fn walking_on_a_little_does_not_make_the_wait_grow() {
        let mut g = Guard::default();
        g.set_mode(Mode::On);
        for _ in 0..4 {
            sprint(&mut g, 5.0, Some(0.3));
        }
        let before = g.patience(5.0);
        for _ in 0..12 {
            sprint(&mut g, 5.0, Some(before + 0.6));
        }
        assert!((g.patience(5.0) - before).abs() < 1e-3);
        assert!(before < 0.7, "{before}");
    }

    #[test]
    fn pressing_forward_or_sprint_again_always_moves() {
        let mut g = Guard::default();
        g.set_mode(Mode::On);
        let mut first = true;
        for _ in 0..1000 {
            g.update(DT, true, std::mem::take(&mut first), true);
        }
        assert!(
            g.update(DT, true, false, false),
            "retenue au relâchement de courir"
        );
        // Il recourt sans avoir lâché « avancer » : il avance.
        assert!(!g.update(DT, true, false, true));
        // « Avancer » relâchée, puis une nouvelle course : retenue de nouveau à la fin.
        g.update(DT, false, false, false);
        let mut first = true;
        for _ in 0..1000 {
            g.update(DT, true, std::mem::take(&mut first), true);
        }
        assert!(g.update(DT, true, false, false));
        // « Avancer » relâchée puis renfoncée : il avance.
        g.update(DT, false, false, false);
        assert!(!g.update(DT, true, true, false));
    }

    /// Arrêt puis redémarrage rapide (les deux touches lâchées un tiers de seconde puis
    /// renfoncées) : la touche qui traîne cache au jeu qu'« avancer » a été lâchée, il ne
    /// voit que « courir » lâchée puis reprise. Chaque arrêt doit pourtant tenir, y
    /// compris le dernier (avant, seul le premier tenait).
    #[test]
    fn every_stop_holds_even_when_forward_never_seems_released() {
        let mut g = Guard::default();
        g.set_mode(Mode::On);
        let mut held = 0;
        let mut step = |g: &mut Guard, modifier: bool, secs: f32| {
            let mut blocked = 0.0;
            for _ in 0..(secs / DT).round() as usize {
                held += 1;
                if g.update(DT, true, held == 1, modifier) {
                    blocked += DT;
                }
            }
            blocked
        };
        for _ in 0..6 {
            assert_eq!(step(&mut g, true, 4.0), 0.0, "il court");
            let stop = step(&mut g, false, 0.3);
            assert!(stop > 0.29, "arrêté tout le temps de la pause : {stop}");
        }
        // Dernier arrêt, pour de bon : tenu aussi.
        assert_eq!(step(&mut g, true, 1.0), 0.0);
        assert!(step(&mut g, false, 0.35) > 0.34);
    }

    #[test]
    fn the_player_can_force_or_refuse_the_rule() {
        let mut g = Guard::default();
        g.set_mode(Mode::Off);
        for _ in 0..6 {
            assert!((sprint(&mut g, 10.0, Some(0.8)) - 0.8).abs() < 0.02);
        }
        assert!(!g.active(), "refusée : jamais apprise");
        g.set_mode(Mode::On);
        assert!(sprint(&mut g, 10.0, Some(0.8)) < 0.02);
        for _ in 0..6 {
            sprint(&mut g, 10.0, None);
        }
        assert!(g.active(), "demandée : jamais éteinte");
    }
}
