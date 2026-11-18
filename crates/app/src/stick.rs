//! (v178) Cette règle ne s'active plus jamais toute seule : seulement avec `/touchelente
//! on`. Ce qui suit décrit l'hypothèse de départ (une touche qui remonte mal) ; les journaux
//! du 18 novembre montrent que l'écart entre « courir » et « avancer » relâchées ne dépend
//! pas de la température et ressemble autant au geste d'un joueur qui ralentit avant de
//! s'arrêter : il ne prouve pas un défaut de la touche. La preuve se fait avec
//! `/testtouche` (voir `keytest`), contre un bouton de la souris.
//!
//! Touche « avancer » qui remonte mal (clavier de portable : la coupelle sous la touche,
//! usée et chauffée par la machine, met du temps à se relever). Le clavier la dit encore
//! enfoncée après qu'on l'a lâchée, de quelques centièmes de seconde à froid jusqu'à une
//! seconde et plus à chaud ou après un long appui, et le joueur continue d'avancer, dans
//! tous les jeux. Aucun logiciel ne voit mieux que le clavier ; mais un joueur qui court
//! lâche « avancer » et « courir » ensemble, et « courir », elle, remonte bien.
//!
//! Le jeu mesure donc, à chaque fin de course, le temps entre « courir » relâchée et
//! « avancer » relâchée. Trois fois de suite nettement après (0,18 à 1,5 s), sans une
//! seule fois ensemble entre-temps : la touche traîne, la règle s'active.
//!
//! Dès lors, lâcher « courir » retient « avancer » juste le temps que la touche a mis à
//! remonter ces dernières fois (un peu plus, et selon la durée de l'appui : elle traîne
//! davantage après un long appui). Si « avancer » est toujours annoncée enfoncée après
//! cela, c'est que le joueur la tient vraiment, pour continuer sans courir : il repart.
//! Un nouvel appui (« courir », ou « avancer » relâchée puis renfoncée) repart toujours
//! tout de suite.
//!
//! Tant que la touche remonte bien (quelques centièmes de seconde, ce qui est le cas
//! après les appuis brefs et quand la machine est froide), rien n'est retenu du tout :
//! sinon le joueur qui lâche « courir » en gardant « avancer » serait arrêté à chaque
//! fois, pour rien. Les deux cas ne se distinguent pas pendant que la touche traîne :
//! c'est le retard mesuré, et lui seul, qui dit combien de temps il faut attendre.
//!
//! Ces retards se retiennent un quart d'heure d'une partie à l'autre : une touche qui
//! traînait à la fin d'une partie traîne encore si l'on relance aussitôt, et sans cela
//! le premier arrêt de la nouvelle partie n'était pas protégé (3,4 blocs de trop, mesuré).
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
/// Fins de course retenues (pour éteindre la règle).
const HISTORY: usize = 8;
/// Retards retenus pour régler l'attente, à part pour les appuis brefs et les longs (la
/// touche traîne peu après un petit sprint, beaucoup après une longue course : les uns
/// ne doivent pas faire oublier les autres).
const LAGS: usize = 5;
/// Durée d'appui sur « avancer » (s) à partir de laquelle l'appui est « long ».
const LONG_FROM: f32 = 2.0;
/// Retard (s) en dessous duquel la touche remonte bien. Tant qu'aucun des derniers
/// retards ne le dépasse, rien n'est retenu : arrêter le joueur pour si peu ne ferait
/// que saccader celui qui continue sans courir.
const FINE: f32 = 0.05;
/// (v165) « Courir » reprise moins de ce temps (s) après avoir été lâchée, « avancer »
/// toujours tenue : c'était un petit appui sur « courir » (mesuré dans les parties du
/// joueur : repris après 0,05 à 0,25 s, ce qui l'arrêtait à chaque fois). Un arrêt suivi
/// d'un redémarrage, que la touche qui traîne déguise de la même façon, dure plus.
const TAP_MAX: f32 = 0.28;
/// Après un tel appui, lâcher « courir » ne retient plus « avancer » pendant… (s) : le
/// joueur appuie sur « courir » à petits coups réguliers, il ne veut pas s'arrêter.
const TAPPING_S: f32 = 20.0;
/// Fichier (dossier du jeu) qui retient le réglage d'une partie à l'autre : « 1 » (la
/// touche traîne, appris), « oui » (demandé par le joueur), « non » (refusé).
const FILE: &str = "touche_avancer_lente";
/// Fichier qui retient les derniers retards mesurés (date, puis un retard par ligne), et
/// temps (s) pendant lequel ils valent encore : la touche traîne plus ou moins selon
/// l'état de la machine, qui change en quelques minutes.
const LAGS_FILE: &str = "touche_avancer_retards";
const LAGS_KEEP: u64 = 15 * 60;

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
    /// Attente décidée à ce moment (s).
    patience: f32,
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
    /// Arrêté dès « courir » relâchée (faux : la touche remontait bien ces derniers
    /// temps, rien n'a été retenu) ; et temps pendant lequel la marche avait repris avant
    /// que la touche remonte (0 : l'arrêt a tenu jusqu'au bout).
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
    /// Derniers retards mesurés (en secondes, et rapportés à `base_lag` : la touche
    /// traîne plus ou moins selon que la machine est chaude), après un appui bref et
    /// après un appui long : ils règlent l'attente. Vides au lancement : la touche est
    /// tenue pour bonne tant qu'elle n'a pas traîné dans cette partie.
    short: std::collections::VecDeque<(f32, f32)>,
    long: std::collections::VecDeque<(f32, f32)>,
    /// Un retard a été mesuré depuis la dernière écriture du fichier des retards.
    lags_dirty: bool,
    active: bool,
    /// Avancer est retenue en attendant un nouvel appui.
    blocked: bool,
    /// (v165) Temps depuis le dernier petit appui sur « courir » (voir `TAP_MAX`).
    since_tap: Option<f32>,
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

/// Plus long retard (s) qu'on puisse encore attribuer à la touche après un appui de
/// `held` secondes (une fois et demie ce qu'elle a montré de pire) : « avancer »
/// relâchée plus tard que cela après « courir », c'est le joueur qui a continué.
fn plausible(held: f32) -> f32 {
    (1.5 * base_lag(held)).min(1.5)
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
            // (v178) « 1 » : appris tout seul par une ancienne version. Plus repris : le jeu
            // n'active plus jamais la règle de lui-même (voir `sample`).
            Some(_) | None => {}
        }
        g.saved = Some((g.mode, g.active));
        if let Ok(text) = std::fs::read_to_string(minelib_assets::data_dir().join(LAGS_FILE)) {
            let kept = g.decode_lags(&text, unix_now());
            if kept > 0 {
                log::info!(
                    "touche avancer : {kept} retards de la partie précédente repris (le \
                     pire : {:.0} ms)",
                    g.short
                        .iter()
                        .chain(&g.long)
                        .map(|l| l.0)
                        .fold(0.0f32, f32::max)
                        * 1000.0
                );
            }
        }
        g
    }

    /// Les derniers retards mesurés, datés (texte du fichier des retards).
    fn encode_lags(&self, now: u64) -> String {
        let mut out = format!("{now}\n");
        for (name, lags) in [("bref", &self.short), ("long", &self.long)] {
            for (gap, scale) in lags {
                out += &format!("{name} {gap} {scale}\n");
            }
        }
        out
    }

    /// Reprend les retards d'un fichier s'ils datent de moins d'un quart d'heure.
    /// Renvoie combien ont été repris.
    fn decode_lags(&mut self, text: &str, now: u64) -> usize {
        let mut lines = text.lines();
        let fresh = lines
            .next()
            .and_then(|l| l.trim().parse::<u64>().ok())
            .is_some_and(|at| at <= now + 60 && now.saturating_sub(at) <= LAGS_KEEP);
        if !fresh {
            return 0;
        }
        let mut kept = 0;
        for line in lines {
            let mut w = line.split_whitespace();
            let lags = match w.next() {
                Some("bref") => &mut self.short,
                Some("long") => &mut self.long,
                _ => continue,
            };
            let (Some(gap), Some(scale)) = (
                w.next().and_then(|v| v.parse::<f32>().ok()),
                w.next().and_then(|v| v.parse::<f32>().ok()),
            ) else {
                continue;
            };
            // (Un fichier abîmé ne doit pas faire retenir le joueur sans fin.)
            if !(0.0..=1.5).contains(&gap) || !(0.0..=1.5).contains(&scale) {
                continue;
            }
            lags.push_back((gap, scale));
            if lags.len() > LAGS {
                lags.pop_front();
            }
            kept += 1;
        }
        kept
    }

    /// Écrit les retards au fichier (seulement pour la règle du joueur, pas en essai).
    fn save_lags(&mut self) {
        self.lags_dirty = false;
        if self.saved.is_some() && !(self.short.is_empty() && self.long.is_empty()) {
            let _ = std::fs::write(
                minelib_assets::data_dir().join(LAGS_FILE),
                self.encode_lags(unix_now()),
            );
        }
    }

    /// Retient le réglage pour les prochaines parties, quand il change, et les retards
    /// quand un nouveau vient d'être mesuré.
    pub fn remember(&mut self) {
        if self.lags_dirty {
            self.save_lags();
        }
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
    /// appui de `held` secondes : un peu plus que le pire des derniers retards mesurés
    /// pour un appui de ce genre (bref ou long), rapporté à sa durée. Au-delà, elle est
    /// tenue pour de bon et le joueur repart. Sans retard mesuré dans cette partie, ou
    /// avec des retards de quelques centièmes seulement (voir `FINE`) : zéro, rien n'est
    /// retenu.
    ///
    /// Appui long alors qu'aucun retard n'a encore été mesuré après un appui long : la
    /// touche ne remonte pas plus vite après un long appui qu'après un bref, on retient
    /// donc au moins le temps qu'elle a mis après les derniers appuis brefs. (Mesuré :
    /// premier arrêt après 86 s d'appui, 469 ms de retard, rien de retenu et 2,4 blocs
    /// de trop, alors que les appuis brefs venaient de traîner 94 et 174 ms.) L'inverse
    /// ne vaut pas : un retard d'appui long ne dit rien d'un appui bref.
    fn patience(&self, held: f32) -> f32 {
        if held >= LONG_FROM && self.long.is_empty() {
            let worst = self.short.iter().map(|l| l.0).fold(0.0f32, f32::max);
            return if worst < FINE {
                0.0
            } else {
                1.3 * worst + 0.03
            };
        }
        let lags = if held < LONG_FROM {
            &self.short
        } else {
            &self.long
        };
        if lags.iter().all(|l| l.0 < FINE) {
            return 0.0;
        }
        let scale = lags.iter().map(|l| l.1).fold(0.0f32, f32::max);
        1.3 * scale * base_lag(held) + 0.03
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
        // (v178) Le jeu n'active plus la règle de lui-même : seul `/touchelente on` le fait.
        // « Avancer » relâchée quelques dixièmes de seconde après « courir », c'est aussi le
        // geste normal d'un joueur qui ralentit avant de s'arrêter (les journaux ne
        // distinguent pas les deux : écarts de 0,4 à 3 s après les longues courses, sans
        // lien avec la température) ; la règle arrêtait alors un joueur qui tenait
        // « avancer » (22 fois de suite dans une partie). La série reste comptée pour le
        // journal ; la touche se vérifie avec `/testtouche`.
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
            if !fwd {
                self.waiting = None;
                self.blocked = false;
                if w.t <= plausible(w.held).max(w.patience) {
                    // Relâchée : ensemble (clavier sain), ou après (elle traînait). Ce
                    // retard règle l'attente des prochaines fois.
                    self.stop = Some(Stop {
                        gap: w.t,
                        held: w.held,
                        ran: w.ran,
                        patience: w.patience,
                        stopped: w.stopped,
                        resumed: w.resumed,
                    });
                    let lags = if w.held < LONG_FROM {
                        &mut self.short
                    } else {
                        &mut self.long
                    };
                    lags.push_back((w.t, w.t / base_lag(w.held)));
                    if lags.len() > LAGS {
                        lags.pop_front();
                    }
                    self.lags_dirty = true;
                    if w.ran >= HELD_MIN {
                        self.sample(Some(w.t));
                    }
                } else {
                    // Bien après : il avait continué sans courir.
                    self.note = Some(format!(
                        "courir relâchée après {:.2} s de course, avancer gardée {:.2} s de \
                         plus ({})",
                        w.ran,
                        w.t,
                        if w.stopped {
                            format!("retenue {:.2} s", w.patience)
                        } else {
                            "rien de retenu".to_string()
                        }
                    ));
                    if w.ran >= HELD_MIN {
                        self.sample(None);
                    }
                }
            } else if modifier {
                // Il recourt sans avoir lâché « avancer » : il repart tout de suite. (v165)
                // Repris aussitôt : c'était un petit appui sur « courir », les prochains
                // relâchements ne le retiennent plus (voir `TAPPING_S`).
                if w.t <= TAP_MAX {
                    self.since_tap = Some(0.0);
                }
                if w.stopped && w.t <= w.patience {
                    self.note = Some(format!(
                        "courir relâchée puis reprise après {:.2} s, avancer toujours tenue \
                         (retenue jusque-là{})",
                        w.t,
                        if w.t <= TAP_MAX {
                            " ; petits appuis sur courir : plus retenu pendant 20 s"
                        } else {
                            ""
                        }
                    ));
                }
                self.waiting = None;
                self.blocked = false;
            } else {
                if w.t > w.patience {
                    // Toujours tenue après le pire retard récent : il la tient pour de bon.
                    if self.blocked {
                        self.note = Some(format!(
                            "courir relâchée, avancer toujours tenue après {:.2} s : il \
                             repart (retenu {:.2} s)",
                            w.t, w.patience
                        ));
                    }
                    self.blocked = false;
                    if w.stopped {
                        w.resumed += dt;
                    }
                }
                if w.t > plausible(w.held).max(w.patience) + 6.0 {
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
        if let Some(t) = &mut self.since_tap {
            *t += dt;
        }
        let tapping = self.since_tap.is_some_and(|t| t < TAPPING_S);
        if self.mod_was && !modifier && fwd && self.both >= TOGETHER_MIN {
            let patience = self.patience(self.fwd_time);
            self.blocked = self.active && patience > 0.0 && !tapping;
            self.waiting = Some(Wait {
                t: 0.0,
                held: self.fwd_time,
                ran: self.both,
                patience,
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

/// En quittant la partie : les retards sont réécrits avec l'heure du moment (le quart
/// d'heure se compte depuis la fin de la partie, pas depuis le dernier arrêt).
impl Drop for Guard {
    fn drop(&mut self) {
        self.save_lags();
    }
}

/// Secondes écoulées depuis 1970 (pour dater le fichier des retards).
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
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

    /// Règle demandée, sur un clavier dont la touche vient de traîner `gap` secondes
    /// après des courses de `run` secondes.
    fn lagging(run: f32, gap: f32) -> Guard {
        let mut g = Guard::default();
        g.set_mode(Mode::On);
        for _ in 0..3 {
            sprint(&mut g, run, Some(gap));
        }
        g
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
        // (v178) Le jeu n'active plus la règle de lui-même : il faut la demander.
        assert!(!g.active() && !g.just_learned, "jamais activée toute seule");
        g.set_mode(Mode::On);
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

    /// Le défaut signalé en v122 : la touche remontait bien (quelques millièmes de
    /// seconde), le joueur gardait « avancer » et lâchait « courir » de temps en temps, et
    /// le jeu l'arrêtait net près d'une seconde à chaque fois. Tant que la touche remonte
    /// bien, rien n'est retenu.
    #[test]
    fn a_key_that_comes_up_fine_never_holds_the_player_who_keeps_walking() {
        let mut g = Guard::default();
        g.set_mode(Mode::On);
        // Au lancement, sans aucune mesure : rien n'est retenu.
        assert!((sprint(&mut g, 1.0, None) - 8.0).abs() < 0.02);
        assert!((sprint(&mut g, 10.0, None) - 8.0).abs() < 0.02);
        // Fins de course mesurées dans cette partie : 3 à 40 ms, appuis brefs et longs.
        for (run, gap) in [
            (0.2, 0.004),
            (0.6, 0.02),
            (2.8, 0.024),
            (7.7, 0.01),
            (1.0, 0.04),
        ] {
            assert!((sprint(&mut g, run, Some(gap)) - gap).abs() < 0.011);
            assert!(!g.stop.take().expect("mesurée").stopped);
        }
        // « Avancer » gardée 34 s, « courir » lâchée puis reprise toutes les deux
        // secondes : il n'est jamais arrêté.
        let mut first = true;
        let mut blocked = 0;
        for i in 0..(34.0 / DT) as usize {
            let run = (i as f32 * DT) % 2.2 < 1.2;
            if g.update(DT, true, std::mem::take(&mut first), run) {
                blocked += 1;
            }
        }
        assert_eq!(blocked, 0, "jamais retenu");
    }

    /// Quand la touche traîne, l'attente suit le retard mesuré de près : celui qui garde
    /// « avancer » repart dès que ce retard est passé, pas une seconde plus tard.
    #[test]
    fn the_wait_follows_the_measured_lag_closely() {
        // Elle traîne 0,15 s après des courses de 3 s : retenue un quart de seconde.
        let mut g = lagging(3.0, 0.15);
        assert!(sprint(&mut g, 3.0, Some(0.15)) < 0.02, "arrêt net");
        let moved = sprint(&mut g, 3.0, None);
        assert!((7.7..7.8).contains(&moved), "repart vite : {moved}");
        // Elle se remet à bien remonter (cinq fins de course nettes) : plus rien n'est
        // retenu.
        for _ in 0..5 {
            sprint(&mut g, 3.0, Some(0.01));
        }
        assert!((sprint(&mut g, 3.0, None) - 8.0).abs() < 0.02);
        // Elle traîne de nouveau : le premier retard passe (rien ne l'annonçait), les
        // suivants sont arrêtés.
        assert!(sprint(&mut g, 3.0, Some(0.3)) > 0.25);
        assert!(sprint(&mut g, 3.0, Some(0.3)) < 0.02);
    }

    /// Les petits sprints (une demi-seconde, une seconde) sont arrêtés comme les longues
    /// courses quand la touche traîne après eux aussi.
    #[test]
    fn short_sprints_are_stopped_at_once_too() {
        let mut g = lagging(0.6, 0.2);
        for (run, gap) in [
            (0.3, 0.15),
            (0.6, 0.14),
            (1.0, 0.2),
            (0.5, 0.2),
            (1.2, 0.25),
        ] {
            assert!(sprint(&mut g, run, Some(gap)) < 0.02, "sprint {run} s");
        }
        // Après un petit sprint, s'il continue en marchant, la marche reprend vite.
        let moved = sprint(&mut g, 0.6, None);
        assert!(moved > 7.5, "{moved}");
        // « Courir » seulement frôlée : rien n'est retenu.
        assert!((sprint(&mut g, 0.05, Some(0.5)) - 0.5).abs() < 0.02);
    }

    /// Quelques petits sprints, où la touche traîne peu, ne font pas oublier ce qu'elle
    /// fait après une longue course.
    #[test]
    fn short_sprints_do_not_shorten_the_wait_after_a_long_run() {
        let mut g = lagging(20.0, 1.1);
        assert!(sprint(&mut g, 20.0, Some(1.1)) < 0.02);
        for _ in 0..10 {
            sprint(&mut g, 0.5, Some(0.01));
        }
        assert!(sprint(&mut g, 20.0, Some(1.1)) < 0.02);
    }

    #[test]
    fn a_player_who_keeps_walking_after_sprinting_is_left_alone() {
        let mut g = Guard::default();
        for _ in 0..3 {
            sprint(&mut g, 10.0, Some(0.5));
        }
        // (v178) Trois écarts de suite n'activent plus rien : ce peut être son geste.
        assert!(!g.active());
        for _ in 0..4 {
            assert!((sprint(&mut g, 10.0, None) - 8.0).abs() < 0.02);
        }
        assert!(!g.active());
        assert!(
            (sprint(&mut g, 10.0, None) - 8.0).abs() < 0.02,
            "plus rien n'est retenu"
        );
    }

    /// Un joueur qui continue un instant en marchant puis s'arrête ne fait pas grandir
    /// l'attente sans fin : au-delà de ce que la touche peut expliquer, ce n'est plus
    /// compté comme un retard.
    #[test]
    fn walking_on_a_little_does_not_make_the_wait_grow() {
        let mut g = lagging(5.0, 0.3);
        let before = g.patience(5.0);
        assert!(before < 0.5, "{before}");
        for _ in 0..12 {
            sprint(&mut g, 5.0, Some(plausible(5.0) + 0.2));
        }
        assert!((g.patience(5.0) - before).abs() < 1e-3);
        // Et même en s'arrêtant juste dans la marge, l'attente reste bornée.
        for _ in 0..12 {
            sprint(&mut g, 5.0, Some(plausible(5.0) - 0.02));
        }
        assert!(g.patience(5.0) < 1.1, "{}", g.patience(5.0));
    }

    #[test]
    fn pressing_forward_or_sprint_again_always_moves() {
        let mut g = lagging(5.0, 0.4);
        let mut first = true;
        for _ in 0..1000 {
            g.update(DT, true, std::mem::take(&mut first), true);
        }
        assert!(
            g.update(DT, true, false, false),
            "retenue au relâchement de courir"
        );
        // (Retenue jusqu'à ce qu'il recoure, un moment après : pas un petit appui.)
        for _ in 0..(0.35 / DT) as usize {
            assert!(g.update(DT, true, false, false));
        }
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

    /// (v165) Le joueur appuie sur « courir » à petits coups réguliers en gardant
    /// « avancer » : seul le premier relâchement le retient (jusqu'à ce qu'il réappuie),
    /// les suivants ne l'arrêtent plus. Après 20 s sans petit appui, les arrêts sont de
    /// nouveau tenus.
    #[test]
    fn tapping_sprint_regularly_does_not_stop_the_player() {
        let mut g = lagging(5.0, 0.4);
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
        assert_eq!(step(&mut g, true, 3.0), 0.0, "il court");
        assert!(step(&mut g, false, 0.15) > 0.14, "le premier retient");
        for _ in 0..8 {
            assert_eq!(step(&mut g, true, 0.8), 0.0);
            assert_eq!(
                step(&mut g, false, 0.2),
                0.0,
                "les suivants n'arrêtent plus"
            );
        }
        // Il marche un moment sans courir (plus de 20 s), puis court et s'arrête : tenu.
        assert_eq!(step(&mut g, false, 21.0), 0.0);
        assert_eq!(step(&mut g, true, 3.0), 0.0);
        assert!(
            step(&mut g, false, 0.35) > 0.34,
            "un vrai arrêt est tenu de nouveau"
        );
    }

    /// Arrêt puis redémarrage rapide (les deux touches lâchées un tiers de seconde puis
    /// renfoncées) : la touche qui traîne cache au jeu qu'« avancer » a été lâchée, il ne
    /// voit que « courir » lâchée puis reprise. Chaque arrêt doit pourtant tenir, y
    /// compris le dernier.
    #[test]
    fn every_stop_holds_even_when_forward_never_seems_released() {
        let mut g = lagging(5.0, 0.4);
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

    /// Le cas mesuré : à la fin d'une partie la touche traînait 286 ms après 8,4 s de
    /// course ; relancé aussitôt, le premier arrêt de la partie suivante n'était pas
    /// retenu (3,4 blocs de trop). Les retards se reprennent pendant un quart d'heure.
    #[test]
    fn lags_are_remembered_for_a_quarter_of_an_hour() {
        let mut before = Guard::default();
        before.set_mode(Mode::On);
        sprint(&mut before, 8.4, Some(0.286));
        sprint(&mut before, 0.3, Some(0.006));
        let text = before.encode_lags(1_000_000);
        // Dix minutes après : le premier arrêt de la nouvelle partie est tenu.
        let mut g = Guard::default();
        g.set_mode(Mode::On);
        assert_eq!(g.decode_lags(&text, 1_000_600), 2);
        assert!(sprint(&mut g, 8.4, Some(0.286)) < 0.02, "arrêt tenu");
        // Et après un appui bref, où la touche remontait bien : rien n'est retenu.
        assert!((sprint(&mut g, 0.3, None) - 8.0).abs() < 0.02);
        // Vingt minutes après : la machine a refroidi, rien n'est repris.
        let mut g = Guard::default();
        g.set_mode(Mode::On);
        assert_eq!(g.decode_lags(&text, 1_001_200), 0);
        assert!((sprint(&mut g, 8.4, Some(0.286)) - 0.286).abs() < 0.02);
        // Un fichier abîmé, ou daté du futur, ne fait rien retenir.
        for bad in [
            "",
            "n'importe quoi",
            "1000000\nlong 99 99\nlong x y\n",
            "9999999999\nlong 1 1\n",
        ] {
            let mut g = Guard::default();
            g.set_mode(Mode::On);
            assert_eq!(g.decode_lags(bad, 1_000_100), 0, "{bad:?}");
        }
    }

    /// Le cas mesuré : en début de partie, deux appuis brefs traînent (94 et 174 ms),
    /// puis le premier arrêt après un long appui (86 s) traîne 469 ms. Aucun retard
    /// d'appui long n'était connu : rien n'était retenu. Les appuis brefs servent de
    /// plancher ; dès le deuxième, la mesure de l'appui long fait foi.
    #[test]
    fn the_first_long_hold_is_held_as_long_as_the_short_ones_lagged() {
        let mut g = Guard::default();
        g.set_mode(Mode::On);
        sprint(&mut g, 1.0, Some(0.094));
        sprint(&mut g, 0.2, Some(0.174));
        // Retenu 1,3 × 174 ms + 30 ms = 256 ms : il ne marche plus que 213 ms au lieu
        // de 469.
        let moved = sprint(&mut g, 86.4, Some(0.469));
        assert!((moved - 0.213).abs() < 0.02, "{moved}");
        // Le suivant est tenu en entier (retard d'appui long connu).
        assert!(sprint(&mut g, 86.4, Some(0.469)) < 0.02);
        // Des appuis brefs qui remontent bien ne font rien retenir après un long appui.
        let mut g = Guard::default();
        g.set_mode(Mode::On);
        sprint(&mut g, 1.0, Some(0.02));
        sprint(&mut g, 0.3, Some(0.04));
        assert!((sprint(&mut g, 30.0, None) - 8.0).abs() < 0.02);
        // Et un retard d'appui long ne fait rien retenir après un appui bref.
        let mut g = Guard::default();
        g.set_mode(Mode::On);
        sprint(&mut g, 30.0, Some(0.5));
        assert!((sprint(&mut g, 0.5, None) - 8.0).abs() < 0.02);
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
