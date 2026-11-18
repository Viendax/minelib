//! Test des touches (v178, `/testtouche`) : une touche annonce-t-elle son relâchement en
//! retard ?
//!
//! Le joueur tient une touche et un bouton de la souris une dizaine de secondes, puis lâche
//! les deux d'un coup. La souris est un autre appareil, avec ses propres contacts : l'écart
//! entre les deux relâchements mesure le retard de la touche elle-même (à la précision du
//! geste près, quelques centièmes de seconde), sans rien supposer des habitudes du joueur.
//!
//! Pourquoi : jusqu'à la v177, le jeu concluait qu'« avancer » traînait en la comparant à
//! « courir » en pleine partie. Mais lâcher « courir » un peu avant « avancer », c'est aussi
//! le geste d'un joueur qui ralentit avant de s'arrêter : ces mesures ne séparaient pas la
//! touche du geste (voir `stick`). Ici, le geste est imposé.
//!
//! Pendant le test, le jeu ne reçoit ni touches ni clics (le joueur ne bouge pas, ne casse
//! rien) ; la souris tourne toujours la vue. Échap, ou une minute sans rien, le termine.

use std::time::Instant;

use minelib_platform::KeyCode;

/// Durée d'appui demandée (s), et durée à partir de laquelle un essai compte pour le bilan.
pub const HOLD_S: f32 = 10.0;
const COUNTS_FROM_S: f32 = 3.0;
/// Écart (ms) jusqu'auquel la touche remonte normalement (deux mains qui lâchent
/// « ensemble » sont à quelques centièmes de seconde l'une de l'autre).
pub const NORMAL_MS: f32 = 80.0;
/// Écart (ms) à partir duquel la touche traîne.
pub const LATE_MS: f32 = 150.0;
/// La touche lâchée bien avant le clic (ms), ou encore tenue longtemps après (s) : le geste
/// n'était pas « les deux ensemble », l'essai ne dit rien.
const EARLY_MS: f32 = -300.0;
const GIVE_UP_S: f32 = 3.0;
/// Sans rien tenir pendant ce temps (s), le test se termine.
const IDLE_S: f32 = 60.0;

/// Ton d'un message (couleur dans le tchat).
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Tone {
    Info,
    Good,
    Bad,
}

/// Un essai mesuré.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Trial {
    pub key: KeyCode,
    /// Durée de l'appui (s).
    pub held: f32,
    /// Relâchement de la touche après celui de la souris (ms ; négatif : avant).
    pub gap_ms: f32,
}

enum Phase {
    Idle,
    Holding {
        key: KeyCode,
        since: Instant,
        told: bool,
    },
    Releasing {
        key: KeyCode,
        held: f32,
        first: Instant,
        key_up: Option<Instant>,
        mouse_up: Option<Instant>,
    },
}

pub struct KeyTest {
    phase: Phase,
    trials: Vec<Trial>,
    idle: f32,
    pub finished: bool,
}

impl KeyTest {
    pub fn new() -> Self {
        Self {
            phase: Phase::Idle,
            trials: Vec::new(),
            idle: 0.0,
            finished: false,
        }
    }

    pub fn trials(&self) -> &[Trial] {
        &self.trials
    }

    /// Une image : touches tenues, un bouton de souris tenu, l'heure. Renvoie les messages
    /// à dire au joueur. `label` : nom d'une touche pour le joueur.
    pub fn step(
        &mut self,
        keys: &[KeyCode],
        mouse: bool,
        now: Instant,
        dt: f32,
        label: &dyn Fn(KeyCode) -> String,
    ) -> Vec<(String, Tone)> {
        let mut out = Vec::new();
        match &mut self.phase {
            Phase::Idle => {
                // (Une seule touche : celle qu'on teste. « Entrée », encore tenue en
                // lançant la commande, ne compte donc pas avec une autre.)
                if mouse && keys.len() == 1 {
                    self.idle = 0.0;
                    let key = keys[0];
                    out.push((
                        format!(
                            "Touche {} : tiens-la avec le clic {HOLD_S:.0} secondes…",
                            label(key)
                        ),
                        Tone::Info,
                    ));
                    self.phase = Phase::Holding {
                        key,
                        since: now,
                        told: false,
                    };
                } else {
                    self.idle += dt;
                    if self.idle >= IDLE_S {
                        self.finished = true;
                    }
                }
            }
            Phase::Holding { key, since, told } => {
                let key_down = keys.contains(key);
                let held = now.duration_since(*since).as_secs_f32();
                if key_down && mouse {
                    if !*told && held >= HOLD_S {
                        *told = true;
                        out.push((
                            "Lâche les deux maintenant, d'un seul coup !".to_string(),
                            Tone::Good,
                        ));
                    }
                } else {
                    self.phase = Phase::Releasing {
                        key: *key,
                        held,
                        first: now,
                        key_up: (!key_down).then_some(now),
                        mouse_up: (!mouse).then_some(now),
                    };
                }
            }
            Phase::Releasing {
                key,
                held,
                first,
                key_up,
                mouse_up,
            } => {
                if key_up.is_none() && !keys.contains(key) {
                    *key_up = Some(now);
                }
                if mouse_up.is_none() && !mouse {
                    *mouse_up = Some(now);
                }
                let (key, held) = (*key, *held);
                if let (Some(k), Some(m)) = (*key_up, *mouse_up) {
                    let gap_ms = if k >= m {
                        k.duration_since(m).as_secs_f32() * 1000.0
                    } else {
                        -m.duration_since(k).as_secs_f32() * 1000.0
                    };
                    self.phase = Phase::Idle;
                    if gap_ms < EARLY_MS {
                        out.push((
                            format!(
                                "Essai annulé : {} lâchée bien avant le clic. Lâche les deux \
                                 en même temps.",
                                label(key)
                            ),
                            Tone::Info,
                        ));
                    } else {
                        let t = Trial { key, held, gap_ms };
                        self.trials.push(t);
                        out.push(say_trial(&t, label));
                    }
                } else if now.duration_since(*first).as_secs_f32() > GIVE_UP_S {
                    // L'un des deux est resté tenu : ce n'était pas « ensemble ».
                    let stuck_key = key_up.is_none();
                    self.phase = Phase::Idle;
                    out.push((
                        if stuck_key {
                            format!(
                                "Essai annulé : {} encore enfoncée {GIVE_UP_S:.0} s après le \
                                 clic. Si tu l'avais bien lâchée, elle reste collée ; sinon \
                                 lâche les deux en même temps.",
                                label(key)
                            )
                        } else {
                            "Essai annulé : le clic est resté tenu. Lâche les deux en même \
                             temps."
                                .to_string()
                        },
                        Tone::Info,
                    ));
                }
            }
        }
        out
    }

    /// Bilan par touche (essais d'au moins `COUNTS_FROM_S` secondes, sinon tous) : lignes à
    /// dire au joueur et à écrire au journal.
    pub fn summary(&self, label: &dyn Fn(KeyCode) -> String) -> Vec<(String, Tone)> {
        let mut keys: Vec<KeyCode> = Vec::new();
        for t in &self.trials {
            if !keys.contains(&t.key) {
                keys.push(t.key);
            }
        }
        if keys.is_empty() {
            return vec![("Test des touches terminé : aucun essai.".to_string(), Tone::Info)];
        }
        let mut out = Vec::new();
        let (mut good, mut bad) = (Vec::new(), Vec::new());
        for key in keys {
            let all: Vec<&Trial> = self.trials.iter().filter(|t| t.key == key).collect();
            let long: Vec<&Trial> = all.iter().copied().filter(|t| t.held >= COUNTS_FROM_S).collect();
            let used = if long.is_empty() { all } else { long };
            let mut gaps: Vec<f32> = used.iter().map(|t| t.gap_ms).collect();
            gaps.sort_by(f32::total_cmp);
            let med = gaps[gaps.len() / 2];
            let worst = gaps[gaps.len() - 1];
            let longest = used.iter().map(|t| t.held).fold(0.0, f32::max);
            let (verdict, tone) = if worst <= NORMAL_MS {
                good.push(label(key));
                ("remonte normalement", Tone::Good)
            } else if med >= LATE_MS {
                bad.push(label(key));
                ("traîne", Tone::Bad)
            } else {
                ("résultat incertain, refais le test", Tone::Info)
            };
            out.push((
                format!(
                    "Touche {} : {} essai(s), appui jusqu'à {longest:.0} s, relâchée {med:.0} ms \
                     après le clic (au pire {worst:.0} ms) : {verdict}.",
                    label(key),
                    gaps.len()
                ),
                tone,
            ));
        }
        if !bad.is_empty() {
            out.push((
                format!(
                    "{} : le clavier l'annonce relâchée en retard, aucun jeu ne peut le voir \
                     plus tôt. Mets son action sur une touche qui remonte normalement \
                     (Paramètres, Touches), ou fais nettoyer ou remplacer la touche.",
                    bad.join(", ")
                ),
                Tone::Bad,
            ));
        } else if !good.is_empty() {
            out.push((
                format!(
                    "{} : le clavier est bon. Le jeu suit exactement ce qu'il dit.",
                    good.join(", ")
                ),
                Tone::Good,
            ));
        }
        out
    }
}

/// Résultat d'un essai, pour le joueur.
fn say_trial(t: &Trial, label: &dyn Fn(KeyCode) -> String) -> (String, Tone) {
    let brief = if t.held < COUNTS_FROM_S {
        " (appui bref : tiens-la 10 s pour un vrai test)"
    } else {
        ""
    };
    let name = label(t.key);
    let held = t.held;
    let gap = t.gap_ms;
    if gap <= NORMAL_MS {
        (
            format!(
                "Touche {name} tenue {held:.1} s : relâchée {gap:.0} ms après le clic, elle \
                 remonte normalement{brief}."
            ),
            Tone::Good,
        )
    } else if gap < LATE_MS {
        (
            format!(
                "Touche {name} tenue {held:.1} s : relâchée {gap:.0} ms après le clic, petit \
                 écart (ton geste ou la touche) : refais-le{brief}."
            ),
            Tone::Info,
        )
    } else {
        (
            format!(
                "Touche {name} tenue {held:.1} s : relâchée {gap:.0} ms après le clic, elle \
                 traîne{brief}."
            ),
            Tone::Bad,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn name(k: KeyCode) -> String {
        format!("{k:?}")
    }

    /// Joue un essai : touche et clic tenus `hold` s, puis la touche relâchée `gap_ms`
    /// après le clic (négatif : avant). Renvoie les messages.
    fn play(t: &mut KeyTest, at: &mut Instant, key: KeyCode, hold: f32, gap_ms: i64) -> Vec<(String, Tone)> {
        let dt = 0.005;
        let step = Duration::from_millis(5);
        let mut out = Vec::new();
        let frames = (hold / dt) as usize;
        for _ in 0..frames {
            out.extend(t.step(&[key], true, *at, dt, &name));
            *at += step;
        }
        let n = (gap_ms.unsigned_abs() / 5) as usize;
        for _ in 0..n {
            // Le premier lâché des deux.
            let (keys, mouse): (&[KeyCode], bool) = if gap_ms >= 0 { (&[key], false) } else { (&[], true) };
            out.extend(t.step(keys, mouse, *at, dt, &name));
            *at += step;
        }
        for _ in 0..3 {
            out.extend(t.step(&[], false, *at, dt, &name));
            *at += step;
        }
        out
    }

    #[test]
    fn a_healthy_key_and_a_lagging_key_are_told_apart() {
        let mut t = KeyTest::new();
        let mut at = Instant::now();
        // Touche saine : lâchée 20 ms après le clic (le geste), même après 10 s.
        let said = play(&mut t, &mut at, KeyCode::KeyE, 10.5, 20);
        assert!(said.iter().any(|m| m.0.contains("Lâche les deux")), "{said:?}");
        assert!(said.last().is_some_and(|m| m.1 == Tone::Good), "{said:?}");
        // Touche qui traîne : 500 ms après le clic.
        let said = play(&mut t, &mut at, KeyCode::KeyW, 10.5, 500);
        assert!(said.last().is_some_and(|m| m.1 == Tone::Bad && m.0.contains("traîne")), "{said:?}");
        play(&mut t, &mut at, KeyCode::KeyW, 10.5, 620);
        assert_eq!(t.trials().len(), 3);
        assert!((t.trials()[1].gap_ms - 500.0).abs() < 11.0, "{:?}", t.trials()[1]);
        assert!((t.trials()[1].held - 10.5).abs() < 0.05);
        let s = t.summary(&name);
        assert!(s[0].0.contains("KeyE") && s[0].1 == Tone::Good, "{s:?}");
        assert!(s[1].0.contains("KeyW") && s[1].1 == Tone::Bad, "{s:?}");
        assert!(s[2].0.starts_with("KeyW") && s[2].1 == Tone::Bad, "{s:?}");
    }

    #[test]
    fn gestures_that_are_not_together_are_not_counted() {
        let mut t = KeyTest::new();
        let mut at = Instant::now();
        // Touche lâchée bien avant le clic.
        let said = play(&mut t, &mut at, KeyCode::KeyW, 4.0, -600);
        assert!(said.last().is_some_and(|m| m.0.contains("annulé")), "{said:?}");
        // Touche gardée plus de 3 s après le clic.
        let said = play(&mut t, &mut at, KeyCode::KeyW, 4.0, 3500);
        assert!(said.iter().any(|m| m.0.contains("annulé")), "{said:?}");
        assert!(t.trials().is_empty());
        // Touche lâchée un rien avant le clic : compte, et elle est saine.
        play(&mut t, &mut at, KeyCode::KeyW, 4.0, -40);
        assert_eq!(t.trials().len(), 1);
        let s = t.summary(&name);
        assert!(s.iter().all(|m| m.1 == Tone::Good), "{s:?}");
        // Deux touches tenues, ou pas de clic : rien ne commence ; une minute sans rien : fini.
        let mut t = KeyTest::new();
        for _ in 0..100 {
            assert!(t.step(&[KeyCode::KeyW, KeyCode::Enter], true, at, 0.005, &name).is_empty());
            assert!(t.step(&[KeyCode::KeyW], false, at, 0.005, &name).is_empty());
        }
        assert!(!t.finished);
        for _ in 0..61 {
            t.step(&[], false, at, 1.0, &name);
        }
        assert!(t.finished);
        assert!(t.summary(&name)[0].0.contains("aucun essai"));
    }
}
