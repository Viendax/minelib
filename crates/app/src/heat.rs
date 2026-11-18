//! Garde thermique (v169) : en partie, le jeu limite ses images par seconde pour garder la
//! carte graphique sous une température cible.
//!
//! Sur le portable du joueur, la carte graphique tourne à 87–89 °C dix secondes après le
//! lancement (jeu sans limite d'images : 150 à 450 images par seconde), juste sous le
//! clavier ; et la touche « avancer » y remonte en retard, d'autant plus qu'elle a été tenue
//! longtemps (médiane 48 ms après moins d'une seconde, 464 ms après 8 à 30 s, une seconde
//! au-delà ; journaux du 18 novembre). Moins d'images inutiles, c'est moins de chaleur.
//!
//! Toutes les 5 s (le rythme de la sonde), au-dessus de la cible la limite baisse de 10 %
//! (20 % à 6 °C et plus au-dessus), jamais sous la fréquence de l'écran (v170 : on ne voit
//! donc aucune différence ; avant, elle descendait jusqu'à 60 et y restait, la carte ne
//! passant que de 89 à 80 °C) ; 3 °C sous la cible, elle remonte de 10 %, et disparaît
//! quand elle dépasse la limite choisie par le joueur (ou 1000).
//! Réglage : `/temperature <°C|off>`, retenu dans le fichier `garde_thermique`.

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

/// Cible par défaut (°C).
pub const DEFAULT_TARGET: u32 = 75;
/// La limite ne descend jamais sous la fréquence de l'écran, ni sous ce nombre d'images par
/// seconde (écran inconnu : `DEFAULT_HZ`).
pub const MIN_FPS: f32 = 60.0;
const DEFAULT_HZ: f32 = 144.0;
/// Fichier du réglage (dans le dossier du jeu).
const FILE: &str = "garde_thermique";
/// Rythme des décisions (celui de la sonde).
const PERIOD: Duration = Duration::from_secs(5);

/// Cible (°C) ; 0 : garde coupée ; `u32::MAX` : pas encore lue.
static TARGET: AtomicU32 = AtomicU32::new(u32::MAX);

/// Cible réglée (°C), ou `None` si la garde est coupée.
pub fn target() -> Option<u32> {
    let mut t = TARGET.load(Ordering::Relaxed);
    if t == u32::MAX {
        t = std::fs::read_to_string(minelib_assets::data_dir().join(FILE))
            .ok()
            .and_then(|s| {
                let s = s.trim();
                if s == "off" {
                    Some(0)
                } else {
                    s.parse::<u32>().ok().map(|v| v.clamp(50, 95))
                }
            })
            .unwrap_or(DEFAULT_TARGET);
        TARGET.store(t, Ordering::Relaxed);
    }
    (t > 0).then_some(t)
}

/// Règle la cible (`None` : garde coupée) et la retient.
pub fn set_target(t: Option<u32>) {
    let v = t.map_or(0, |v| v.clamp(50, 95));
    TARGET.store(v, Ordering::Relaxed);
    let text = if v == 0 { "off".to_string() } else { v.to_string() };
    let _ = std::fs::write(minelib_assets::data_dir().join(FILE), text);
}

/// La garde : limite d'images en cours (`None` : aucune).
pub struct Guard {
    cap: Option<f32>,
    next: Instant,
    /// Images comptées depuis la dernière décision (images par seconde réelles).
    frames: u32,
    since: Instant,
    /// Fréquence de l'écran (images par seconde), relue à chaque décision.
    hz: Option<f32>,
    /// Dernière limite écrite au journal.
    logged: Option<f32>,
}

impl Guard {
    pub fn new() -> Self {
        Self {
            cap: None,
            next: Instant::now() + PERIOD,
            frames: 0,
            since: Instant::now(),
            hz: None,
            logged: None,
        }
    }

    /// Faut-il relire la fréquence de l'écran (juste avant une décision) ?
    pub fn wants_hz(&self) -> bool {
        Instant::now() >= self.next
    }

    /// Fréquence de l'écran (Hz), si connue.
    pub fn set_hz(&mut self, hz: Option<f32>) {
        self.hz = hz.filter(|h| *h >= 30.0);
    }

    /// Plancher de la limite : la fréquence de l'écran.
    fn floor(&self) -> f32 {
        self.hz.unwrap_or(DEFAULT_HZ).max(MIN_FPS)
    }

    /// Limite d'images voulue par la garde.
    pub fn cap(&self) -> Option<f32> {
        self.cap
    }

    /// Une image de partie vient de passer (`dt` secondes) ; `user` : la limite choisie par
    /// le joueur. Toutes les 5 s, la limite suit la température.
    pub fn update(&mut self, user: Option<f32>) {
        self.frames += 1;
        if Instant::now() < self.next {
            return;
        }
        let fps = self.frames as f32 / self.since.elapsed().as_secs_f32().max(0.001);
        self.frames = 0;
        self.since = Instant::now();
        self.next = Instant::now() + PERIOD;
        let temp = minelib_render::thermal::gpu_temp();
        self.cap = step(self.cap, temp, target(), fps, user, self.floor());
        self.note(temp);
    }

    /// Une ligne au journal quand la limite apparaît, disparaît ou bouge de 10 %.
    fn note(&mut self, temp: Option<f32>) {
        let moved = match (self.logged, self.cap) {
            (None, None) => false,
            (Some(a), Some(b)) => (b - a).abs() > a * 0.1,
            _ => true,
        };
        if !moved {
            return;
        }
        self.logged = self.cap;
        let t = temp.map_or("?".into(), |t| format!("{t:.0}"));
        match (self.cap, target()) {
            (Some(c), Some(goal)) => log::info!(
                "garde thermique : carte graphique à {t} °C (cible {goal} °C), images limitées \
                 à {c:.0} par seconde (écran : {:.0} Hz)",
                self.floor()
            ),
            _ => log::info!("garde thermique : carte graphique à {t} °C, plus de limite"),
        }
    }
}

/// Nouvelle limite d'images, d'après l'ancienne, la température, la cible, les images par
/// seconde mesurées, la limite du joueur et le plancher (fréquence de l'écran).
fn step(
    cap: Option<f32>,
    temp: Option<f32>,
    goal: Option<u32>,
    fps: f32,
    user: Option<f32>,
    floor: f32,
) -> Option<f32> {
    let (Some(t), Some(goal)) = (temp, goal) else {
        return None;
    };
    let goal = goal as f32;
    let top = user.unwrap_or(1000.0);
    // Le joueur limite déjà sous l'écran : rien à faire de plus.
    if top <= floor {
        return None;
    }
    if t > goal + 0.5 {
        // Limite sous ce qui tourne vraiment (la carte peut déjà tourner moins vite ; une
        // mesure faite pendant un chargement ne descend pas sous le plancher).
        let base = cap.unwrap_or(top).min(fps.max(floor)).min(top);
        let k = if t >= goal + 6.0 { 0.8 } else { 0.9 };
        Some((base * k).max(floor))
    } else if t < goal - 3.0 {
        cap.map(|c| c * 1.1).filter(|c| *c < top)
    } else {
        cap
    }
}

/// Bilan des fins de course (retard d'« avancer » sur « courir ») par durée d'appui, au
/// journal toutes les 5 minutes : il dit si la touche se comporte encore autrement qu'un
/// clavier normal (les deux touches lâchées ensemble remontent à moins de `NORMAL_MS` l'une
/// de l'autre) et à quelle température.
#[derive(Default)]
pub struct LagBook {
    /// (retard en s, durée d'appui en s, température de la carte graphique).
    stops: Vec<(f32, f32, Option<f32>)>,
    since: Option<Instant>,
}

/// Retard (ms) d'un clavier normal : deux touches lâchées ensemble.
pub const NORMAL_MS: f32 = 50.0;
/// Tranches de durée d'appui (s).
const BUCKETS: [(f32, f32, &str); 5] = [
    (0.0, 1.0, "moins d'1 s"),
    (1.0, 3.0, "1 à 3 s"),
    (3.0, 8.0, "3 à 8 s"),
    (8.0, 30.0, "8 à 30 s"),
    (30.0, f32::INFINITY, "plus de 30 s"),
];

impl LagBook {
    pub fn add(&mut self, gap: f32, held: f32, temp: Option<f32>) {
        self.since.get_or_insert_with(Instant::now);
        self.stops.push((gap, held, temp));
    }

    /// Une ligne de bilan toutes les 5 minutes (ou tout de suite : `force`).
    pub fn report(&mut self, force: bool) -> Option<String> {
        let since = self.since?;
        if self.stops.is_empty() || (!force && since.elapsed().as_secs() < 300) {
            return None;
        }
        let mut parts = Vec::new();
        for (lo, hi, name) in BUCKETS {
            let mut gaps: Vec<f32> = self
                .stops
                .iter()
                .filter(|s| s.1 >= lo && s.1 < hi)
                .map(|s| s.0 * 1000.0)
                .collect();
            if gaps.is_empty() {
                continue;
            }
            gaps.sort_by(f32::total_cmp);
            let med = gaps[gaps.len() / 2];
            parts.push(format!(
                "{name} : {} fois, médiane {med:.0} ms, pire {:.0} ms{}",
                gaps.len(),
                gaps[gaps.len() - 1],
                if med < NORMAL_MS { " (comme un clavier normal)" } else { "" }
            ));
        }
        let temps: Vec<f32> = self.stops.iter().filter_map(|s| s.2).collect();
        let temp = if temps.is_empty() {
            String::new()
        } else {
            format!(
                " ; carte graphique en moyenne à {:.0} °C",
                temps.iter().sum::<f32>() / temps.len() as f32
            )
        };
        self.stops.clear();
        self.since = Some(Instant::now());
        Some(format!(
            "touche avancer, bilan (retard sur courir selon la durée d'appui ; clavier normal : \
             moins de {NORMAL_MS:.0} ms) : {}{temp}",
            parts.join(" ; ")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn la_limite_suit_la_temperature() {
        const HZ: f32 = 144.0;
        // Pas de sonde, ou garde coupée : aucune limite.
        assert_eq!(step(None, None, Some(75), 300.0, None, HZ), None);
        assert_eq!(step(Some(200.0), Some(88.0), None, 300.0, None, HZ), None);
        // Chaud (88 °C pour 75) : 20 % sous ce qui tourne.
        let c = step(None, Some(88.0), Some(75), 300.0, None, HZ).unwrap();
        assert!((c - 240.0).abs() < 0.1, "{c}");
        // Encore chaud : la limite baisse encore, jamais sous la fréquence de l'écran.
        let mut cap = Some(c);
        for _ in 0..30 {
            cap = step(cap, Some(80.0), Some(75), cap.unwrap(), None, HZ);
        }
        assert_eq!(cap, Some(HZ));
        // Mesure faite pendant un chargement (12 images par seconde) : pas sous l'écran
        // (la v169 tombait à 60 et y restait).
        assert_eq!(step(None, Some(82.0), Some(75), 12.0, None, HZ), Some(HZ));
        // Près de la cible : on ne bouge pas.
        assert_eq!(step(Some(200.0), Some(74.0), Some(75), 200.0, None, HZ), Some(200.0));
        // Bien refroidie : la limite remonte, puis disparaît au-delà de celle du joueur.
        assert!((step(Some(200.0), Some(70.0), Some(75), 200.0, None, HZ).unwrap() - 220.0).abs() < 0.1);
        assert_eq!(step(Some(230.0), Some(70.0), Some(75), 230.0, Some(240.0), HZ), None);
        // Le joueur limite déjà à la fréquence de l'écran ou dessous : rien.
        assert_eq!(step(None, Some(88.0), Some(75), 300.0, Some(144.0), HZ), None);
        // Chaud avec une limite du joueur plus haute que l'écran : on part de la sienne.
        let c = step(None, Some(80.0), Some(75), 300.0, Some(200.0), HZ).unwrap();
        assert!((c - 180.0).abs() < 0.1, "{c}");
    }

    #[test]
    fn bilan_des_retards_par_duree_d_appui() {
        let mut b = LagBook::default();
        assert_eq!(b.report(true), None);
        b.add(0.03, 0.5, Some(70.0));
        b.add(0.02, 0.8, Some(72.0));
        b.add(0.45, 12.0, Some(88.0));
        let r = b.report(true).unwrap();
        assert!(r.contains("moins d'1 s : 2 fois, médiane 30 ms"), "{r}");
        assert!(r.contains("(comme un clavier normal)"), "{r}");
        assert!(r.contains("8 à 30 s : 1 fois, médiane 450 ms"), "{r}");
        assert!(r.contains("en moyenne à 77 °C"), "{r}");
        assert_eq!(b.report(true), None);
    }
}
