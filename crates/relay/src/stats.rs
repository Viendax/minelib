//! Statistiques des joueurs (classement façon CS, Elo des échecs) et réputation
//! (signalements). Un fichier par compte dans `stats/`, tout gardé en mémoire.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Result;
use minelib_net::game::{MatchSummary, CT, T};
use minelib_net::proto::RankEntry;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Elo de départ.
pub const START_ELO: f64 = 1200.0;

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Stats {
    pub pseudo: String,
    pub elo: f64,
    pub matches: u32,
    pub wins: u32,
    pub losses: u32,
    pub ties: u32,
    pub kills: u32,
    pub deaths: u32,
    pub assists: u32,
    pub headshots: u32,
    pub damage: u64,
    pub rounds: u32,
    pub mvps: u32,
    /// Parties commencées (pour le rapport parties / signalements).
    pub games: u32,
    /// Signalements reçus (un par joueur et par partie).
    pub reports: u32,
    /// Joueurs qu'on a signalés, et qui nous ont signalés (pseudos en minuscules) : on ne
    /// tombe plus jamais avec eux.
    pub reported: Vec<String>,
    pub reported_by: Vec<String>,
    pub friends: Vec<String>,
}

impl Stats {
    fn new(pseudo: &str) -> Self {
        Self {
            pseudo: pseudo.to_string(),
            elo: START_ELO,
            ..Default::default()
        }
    }

    /// Signalements par partie jouée.
    pub fn ratio(&self) -> f64 {
        self.reports as f64 / self.games.max(1) as f64
    }

    pub fn entry(&self, rank: u32) -> RankEntry {
        RankEntry {
            rank,
            pseudo: self.pseudo.clone(),
            elo: self.elo.round() as i32,
            matches: self.matches,
            wins: self.wins,
            losses: self.losses,
            ties: self.ties,
            kills: self.kills,
            deaths: self.deaths,
            assists: self.assists,
            headshots: self.headshots,
            damage: self.damage,
            rounds: self.rounds,
            mvps: self.mvps,
        }
    }

    /// Évite (on l'a signalé ou il nous a signalés).
    pub fn avoids(&self, other: &str) -> bool {
        let o = other.to_ascii_lowercase();
        self.reported.contains(&o) || self.reported_by.contains(&o)
    }
}

/// Coefficient K de la FIDE : 40 pour les 30 premières parties, 20 ensuite, 10 au-dessus
/// de 2400.
pub fn k_factor(matches: u32, elo: f64) -> f64 {
    if matches < 30 {
        40.0
    } else if elo < 2400.0 {
        20.0
    } else {
        10.0
    }
}

/// Score attendu contre un adversaire (formule de l'Elo).
pub fn expected(own: f64, opp: f64) -> f64 {
    1.0 / (1.0 + 10f64.powf((opp - own) / 400.0))
}

pub struct Store {
    dir: PathBuf,
    all: HashMap<String, Stats>,
}

impl Store {
    pub fn open(dir: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&dir)?;
        let mut all = HashMap::new();
        for e in std::fs::read_dir(&dir)?.flatten() {
            if e.path().extension().is_some_and(|x| x == "stats") {
                if let Some(s) = std::fs::read(e.path())
                    .ok()
                    .and_then(|b| postcard::from_bytes::<Stats>(&b).ok())
                {
                    all.insert(s.pseudo.to_ascii_lowercase(), s);
                }
            }
        }
        Ok(Self { dir, all })
    }

    fn path(&self, pseudo: &str) -> PathBuf {
        let h = Sha256::digest(pseudo.to_ascii_lowercase().as_bytes());
        let name: String = h[..16].iter().map(|b| format!("{b:02x}")).collect();
        self.dir.join(format!("{name}.stats"))
    }

    pub fn get(&self, pseudo: &str) -> Option<&Stats> {
        self.all.get(&pseudo.to_ascii_lowercase())
    }

    /// Statistiques d'un compte (créées au besoin).
    pub fn get_mut(&mut self, pseudo: &str) -> &mut Stats {
        self.all
            .entry(pseudo.to_ascii_lowercase())
            .or_insert_with(|| Stats::new(pseudo))
    }

    pub fn ratio(&self, pseudo: &str) -> f64 {
        self.get(pseudo).map_or(0.0, |s| s.ratio())
    }

    pub fn save(&self, pseudo: &str) {
        if let Some(s) = self.get(pseudo) {
            if let Ok(b) = postcard::to_allocvec(s) {
                let _ = crate::accounts::write_atomic(&self.path(pseudo), &b);
            }
        }
    }

    /// Classement de tous les joueurs ayant fini au moins une partie.
    fn ranked(&self) -> Vec<&Stats> {
        let mut v: Vec<&Stats> = self.all.values().filter(|s| s.matches > 0).collect();
        v.sort_by(|a, b| {
            b.elo
                .total_cmp(&a.elo)
                .then(b.wins.cmp(&a.wins))
                .then(a.pseudo.cmp(&b.pseudo))
        });
        v
    }

    /// Les 50 meilleurs, et sa propre ligne.
    pub fn top(&self, me: &str) -> (Vec<RankEntry>, Option<RankEntry>) {
        let ranked = self.ranked();
        let mine = ranked
            .iter()
            .position(|s| s.pseudo.eq_ignore_ascii_case(me))
            .map(|i| ranked[i].entry(i as u32 + 1));
        let list = ranked
            .iter()
            .take(50)
            .enumerate()
            .map(|(i, s)| s.entry(i as u32 + 1))
            .collect();
        (list, mine)
    }

    /// Classement entre amis (soi compris).
    pub fn friends_top(&self, me: &str) -> (Vec<RankEntry>, Option<RankEntry>) {
        let friends: Vec<String> = self.get(me).map(|s| s.friends.clone()).unwrap_or_default();
        let mut v: Vec<Stats> = friends
            .iter()
            .chain(std::iter::once(&me.to_string()))
            .map(|f| self.get(f).cloned().unwrap_or_else(|| Stats::new(f)))
            .collect();
        v.sort_by(|a, b| b.elo.total_cmp(&a.elo).then(a.pseudo.cmp(&b.pseudo)));
        let list: Vec<RankEntry> = v
            .iter()
            .enumerate()
            .map(|(i, s)| s.entry(i as u32 + 1))
            .collect();
        let mine = list
            .iter()
            .find(|e| e.pseudo.eq_ignore_ascii_case(me))
            .cloned();
        (list, mine)
    }

    /// Partie terminée : statistiques et Elo de chaque joueur (Elo des équipes : moyenne
    /// des joueurs, chacun gagne ou perd K × (résultat − attendu)).
    pub fn apply_match(&mut self, m: &MatchSummary) {
        let avg = |team: u8, st: &Self| {
            let v: Vec<f64> = m
                .players
                .iter()
                .filter(|p| p.start_team == team)
                .map(|p| st.get(&p.pseudo).map_or(START_ELO, |s| s.elo))
                .collect();
            if v.is_empty() {
                START_ELO
            } else {
                v.iter().sum::<f64>() / v.len() as f64
            }
        };
        let team_elo = [avg(CT, self), avg(T, self)];
        for p in &m.players {
            let team = p.start_team.min(1) as usize;
            let (own, opp) = (m.score[team], m.score[1 - team]);
            let result = if own > opp {
                1.0
            } else if own == opp {
                0.5
            } else {
                0.0
            };
            let e = expected(team_elo[team], team_elo[1 - team]);
            let s = self.get_mut(&p.pseudo);
            let k = k_factor(s.matches, s.elo);
            s.elo = (s.elo + k * (result - e)).max(100.0);
            s.matches += 1;
            match result {
                r if r > 0.75 => s.wins += 1,
                r if r < 0.25 => s.losses += 1,
                _ => s.ties += 1,
            }
            s.kills += p.kills as u32;
            s.deaths += p.deaths as u32;
            s.assists += p.assists as u32;
            s.headshots += p.headshots as u32;
            s.damage += p.damage as u64;
            s.rounds += p.rounds as u32;
            s.mvps += p.mvps as u32;
            self.save(&p.pseudo);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use minelib_net::game::SummaryPlayer;

    #[test]
    fn elo_moves_like_chess() {
        assert!((expected(1200.0, 1200.0) - 0.5).abs() < 1e-9);
        assert!((expected(1600.0, 1200.0) - 0.909).abs() < 1e-3);
        let dir = std::env::temp_dir().join(format!("minelib-stats-{}", rand::random::<u32>()));
        let mut st = Store::open(dir.clone()).unwrap();
        let p = |n: &str, t: u8| SummaryPlayer {
            pseudo: n.into(),
            start_team: t,
            kills: 10,
            deaths: 5,
            assists: 2,
            headshots: 4,
            damage: 1500,
            mvps: 1,
            score: 25,
            rounds: 20,
        };
        st.apply_match(&MatchSummary {
            map_sig: [0; 32],
            score: [13, 7],
            rounds: 20,
            players: vec![p("Ana", CT), p("Bob", T)],
        });
        assert_eq!(st.get("ana").unwrap().elo, 1220.0, "K = 40, attendu 0,5");
        assert_eq!(st.get("BOB").unwrap().elo, 1180.0);
        assert_eq!(st.top("bob").0[0].pseudo, "Ana");
        assert_eq!(st.top("bob").1.unwrap().rank, 2);
        let again = Store::open(dir.clone()).unwrap();
        assert_eq!(again.get("ana").unwrap().wins, 1, "relu du disque");
        let _ = std::fs::remove_dir_all(dir);
    }
}
