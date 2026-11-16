//! Fréquentation : qui joue, parle ou traîne avec qui. Deux joueurs qui se fréquentent ne
//! peuvent pas être garants l'un pour l'autre (un garant arbitre une partie : il ne doit
//! avoir aucun lien avec ses joueurs).
//!
//! Le relais donne des points à une paire de joueurs quand ils se parlent (vocal, tchat à
//! portée), jouent la même partie FPS ou restent côte à côte dans un monde de construction.
//! Le score s'efface avec le temps (moitié tous les 14 jours) : d'anciens coéquipiers
//! redeviennent des inconnus.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

/// Score à partir duquel deux joueurs se fréquentent.
pub const ACQUAINTED: f32 = 5.0;
/// Score maximal (au-delà, rien ne s'ajoute : l'oubli reste possible).
const MAX_SCORE: f32 = 60.0;
/// Le score est divisé par deux tous les… (jours).
const HALF_LIFE_DAYS: f32 = 14.0;
/// Fréquentations gardées par joueur (les plus faibles partent).
const MAX_PER_PLAYER: usize = 512;

/// Points donnés par chaque occasion.
pub const VOICE: f32 = 1.0;
pub const CHAT: f32 = 1.0;
/// Par entretien (2 s) passé dans la même partie FPS : 1 point toutes les 2 minutes.
pub const SAME_MATCH: f32 = 1.0 / 60.0;
/// Par entretien (2 s) passé côte à côte en construction : 1 point toutes les 5 minutes.
pub const NEARBY: f32 = 1.0 / 150.0;

type Links = HashMap<String, HashMap<String, (f32, u32)>>;

pub struct Contacts {
    path: PathBuf,
    /// pseudo (minuscules) → autre pseudo → (score, jour de la dernière mise à jour).
    links: Links,
    dirty: bool,
    saved: Instant,
}

fn today() -> u32 {
    (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
        / 86_400) as u32
}

fn faded(score: f32, since: u32, now: u32) -> f32 {
    score * 0.5f32.powf(now.saturating_sub(since) as f32 / HALF_LIFE_DAYS)
}

impl Contacts {
    pub fn open(path: PathBuf) -> Self {
        let links = std::fs::read(&path)
            .ok()
            .and_then(|b| postcard::from_bytes::<Links>(&b).ok())
            .unwrap_or_default();
        Self {
            path,
            links,
            dirty: false,
            saved: Instant::now(),
        }
    }

    /// `a` et `b` viennent de se fréquenter : `points` de plus, des deux côtés.
    pub fn add(&mut self, a: &str, b: &str, points: f32) {
        self.add_on(a, b, points, today());
    }

    fn add_on(&mut self, a: &str, b: &str, points: f32, day: u32) {
        let (a, b) = (a.to_ascii_lowercase(), b.to_ascii_lowercase());
        if a == b || a.is_empty() || b.is_empty() || !points.is_finite() || points <= 0.0 {
            return;
        }
        for (x, y) in [(&a, &b), (&b, &a)] {
            let mine = self.links.entry(x.clone()).or_default();
            let e = mine.entry(y.clone()).or_insert((0.0, day));
            *e = ((faded(e.0, e.1, day) + points).min(MAX_SCORE), day);
            if mine.len() > MAX_PER_PLAYER {
                let weakest = mine
                    .iter()
                    .min_by(|p, q| {
                        faded(p.1 .0, p.1 .1, day).total_cmp(&faded(q.1 .0, q.1 .1, day))
                    })
                    .map(|(k, _)| k.clone());
                if let Some(k) = weakest {
                    mine.remove(&k);
                }
            }
        }
        self.dirty = true;
    }

    /// Score de fréquentation de deux joueurs, aujourd'hui.
    pub fn score(&self, a: &str, b: &str) -> f32 {
        self.score_on(a, b, today())
    }

    fn score_on(&self, a: &str, b: &str, day: u32) -> f32 {
        self.links
            .get(&a.to_ascii_lowercase())
            .and_then(|m| m.get(&b.to_ascii_lowercase()))
            .map_or(0.0, |e| faded(e.0, e.1, day))
    }

    /// Se fréquentent-ils (assez pour ne pas arbitrer l'un pour l'autre) ?
    pub fn acquainted(&self, a: &str, b: &str) -> bool {
        self.score(a, b) >= ACQUAINTED
    }

    /// Écrit le fichier s'il a changé (au plus une fois par minute).
    pub fn save_if_due(&mut self) {
        if !self.dirty || self.saved.elapsed().as_secs() < 60 {
            return;
        }
        self.saved = Instant::now();
        let day = today();
        // Les liens effacés par le temps ne sont plus gardés.
        for m in self.links.values_mut() {
            m.retain(|_, e| faded(e.0, e.1, day) > 0.05);
        }
        self.links.retain(|_, m| !m.is_empty());
        if let Ok(b) = postcard::to_allocvec(&self.links) {
            if crate::accounts::write_atomic(&self.path, &b).is_ok() {
                self.dirty = false;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn talking_or_playing_together_makes_acquaintances_that_fade() {
        let path = std::env::temp_dir().join(format!("minelib-freq-{}", rand::random::<u32>()));
        let mut c = Contacts::open(path.clone());
        // Quelques mots échangés : pas encore des connaissances.
        c.add_on("Alice", "bob", VOICE, 100);
        c.add_on("BOB", "alice", CHAT, 100);
        assert_eq!(c.score_on("alice", "Bob", 100), 2.0);
        assert!(c.score_on("alice", "bob", 100) < ACQUAINTED);
        // Une partie FPS d'une demi-heure ensemble : ils se fréquentent, dans les deux sens.
        for _ in 0..900 {
            c.add_on("Alice", "Bob", SAME_MATCH, 100);
        }
        assert!(c.score_on("bob", "alice", 100) >= ACQUAINTED);
        assert!(c.score_on("alice", "bob", 100) >= ACQUAINTED);
        // Personne d'autre n'est concerné, et on ne se fréquente pas soi-même.
        assert_eq!(c.score_on("alice", "carol", 100), 0.0);
        c.add_on("alice", "ALICE", 50.0, 100);
        assert_eq!(c.score_on("alice", "alice", 100), 0.0);
        // Le temps passe : moitié tous les 14 jours, puis de nouveau des inconnus.
        let s = c.score_on("alice", "bob", 100);
        assert!((c.score_on("alice", "bob", 114) - s / 2.0).abs() < 1e-3);
        assert!(c.score_on("alice", "bob", 100 + 90) < ACQUAINTED);
        // Jamais au-delà du plafond.
        c.add_on("alice", "bob", 1e9, 100);
        assert_eq!(c.score_on("alice", "bob", 100), MAX_SCORE);
        // Gardé d'un lancement du relais à l'autre.
        c.add_on("alice", "bob", 1e9, today());
        c.saved = Instant::now() - std::time::Duration::from_secs(120);
        c.save_if_due();
        let back = Contacts::open(path.clone());
        assert_eq!(back.score_on("bob", "alice", today()), MAX_SCORE);
        let _ = std::fs::remove_file(path);
    }
}
