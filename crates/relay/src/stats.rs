//! Statistiques des joueurs (classement façon CS, Elo des échecs) et réputation
//! (signalements). Un fichier par compte dans `stats/`, tout gardé en mémoire.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Result;
use minelib_net::game::SummaryPlayer;
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
    /// Facteur de confiance (0 à 1000 ; 500 pour un compte neuf). Baisse un peu à chaque
    /// signalement, coéquipier tué ou longue absence, remonte lentement à chaque partie
    /// propre : les joueurs sérieux creusent l'écart. Les parties se forment entre
    /// facteurs proches (le plus bas des comptes de la même machine compte).
    pub trust: i32,
    /// Coéquipiers tués, et parties quittées trop longtemps.
    pub teamkills: u32,
    pub absences: u32,
    /// Triches prouvées par le rejeu d'un garant vérificateur (script de bunny hop,
    /// macro…), et les garants qui les ont trouvées (en minuscules) : deux garants
    /// différents, et c'est le bannissement.
    pub scripts: u32,
    pub script_by: Vec<String>,
    /// Parties aux statistiques invraisemblables (sans être impossibles).
    pub odd_games: u32,
}

/// Statistiques enregistrées avant les preuves de script (relues, puis complétées).
#[derive(Deserialize)]
struct StatsV2 {
    pseudo: String,
    elo: f64,
    matches: u32,
    wins: u32,
    losses: u32,
    ties: u32,
    kills: u32,
    deaths: u32,
    assists: u32,
    headshots: u32,
    damage: u64,
    rounds: u32,
    mvps: u32,
    games: u32,
    reports: u32,
    reported: Vec<String>,
    reported_by: Vec<String>,
    friends: Vec<String>,
    trust: i32,
    teamkills: u32,
    absences: u32,
}

impl From<StatsV2> for Stats {
    fn from(o: StatsV2) -> Self {
        Stats {
            pseudo: o.pseudo,
            elo: o.elo,
            matches: o.matches,
            wins: o.wins,
            losses: o.losses,
            ties: o.ties,
            kills: o.kills,
            deaths: o.deaths,
            assists: o.assists,
            headshots: o.headshots,
            damage: o.damage,
            rounds: o.rounds,
            mvps: o.mvps,
            games: o.games,
            reports: o.reports,
            reported: o.reported,
            reported_by: o.reported_by,
            friends: o.friends,
            trust: o.trust,
            teamkills: o.teamkills,
            absences: o.absences,
            ..Default::default()
        }
    }
}

/// Facteur de confiance d'un compte neuf, et bornes.
pub const TRUST_START: i32 = 500;
pub const TRUST_MAX: i32 = 1000;

/// Ce qui fait bouger le facteur de confiance. Trois paliers bien séparés (réglage choisi
/// par simulation : 2 000 joueurs sur 90 jours, et deux fois plus de faux signalements que
/// prévu) : les joueurs corrects vers 900 à 1000, les trolls (signalés, partent en cours)
/// vers 250 à 350, les tricheurs sous 200 avant même leur bannissement.
pub mod trust {
    /// Partie finie sans signalement compté, sans absence ni soupçon : remonte (plus 1 par
    /// tranche de 150 points déjà acquis).
    pub const CLEAN_GAME: i32 = 12;
    /// Par joueur qui signale, quand au moins `REPORTERS` joueurs de la partie ont signalé
    /// le même (un seul mauvais perdant ne coûte rien).
    pub const REPORT: i32 = -25;
    pub const REPORTERS: usize = 2;
    /// Partie abandonnée (jusqu'à la fin) ; une absence plus courte coûte moins.
    pub const ABANDON: i32 = -100;
    /// Signalements et absences ne font pas descendre sous ce plancher : c'est le palier
    /// des trolls. Seuls les soupçons du vérificateur et la triche prouvée vont plus bas.
    pub const SOFT_FLOOR: i32 = 300;
    /// Soupçon du vérificateur dans une partie (écart qui n'est pas une preuve), une fois
    /// par partie au plus.
    pub const SUSPICION: i32 = -80;
    /// Triche prouvée par les garants.
    pub const CHEAT: i32 = -400;
    /// Script prouvé par le rejeu d'un garant (bunny hop parfait, macro) : la confiance
    /// tombe à zéro ; deuxième preuve par un autre garant : bannissement.
    pub const SCRIPT: i32 = -super::TRUST_MAX;
    /// Partie aux statistiques invraisemblables (possibles, mais presque jamais vues chez
    /// un joueur honnête) : comme un soupçon du vérificateur.
    pub const ODD_STATS: i32 = SUSPICION;
}

/// Statistiques d'une partie impossibles (résultat faux) : par joueur, la règle violée.
/// `team` : joueurs par équipe (adversaires possibles à chaque manche).
pub fn impossible(p: &SummaryPlayer, team: u32) -> Option<&'static str> {
    let rounds = p.rounds as u32;
    if p.headshots > p.kills {
        Some("plus de tirs à la tête que d'éliminations")
    } else if p.deaths as u32 > rounds {
        Some("plus de morts que de manches")
    } else if p.mvps as u32 > rounds {
        Some("plus de MVP que de manches")
    } else if p.kills as u32 > rounds * team {
        Some("plus d'éliminations que d'adversaires")
    } else if p.damage > rounds * team * 100 {
        Some("plus de dégâts que la vie de tous les adversaires")
    } else {
        None
    }
}

/// Statistiques d'une partie invraisemblables pour un joueur honnête (seuils au-delà
/// des meilleures parties connues des joueurs professionnels de CS) : `Some(raison)`.
pub fn implausible(p: &SummaryPlayer) -> Option<&'static str> {
    let rounds = (p.rounds as f64).max(1.0);
    let kills = p.kills as f64;
    // Toutes les éliminations à la tête, sur au moins 20.
    if p.kills >= 20 && p.headshots == p.kills {
        return Some("100 % de tirs à la tête sur 20 éliminations ou plus");
    }
    // Plus de 2,2 éliminations par manche sur au moins 16 manches (record pro ≈ 1,9).
    if p.rounds >= 16 && kills / rounds > 2.2 {
        return Some("plus de 2,2 éliminations par manche");
    }
    // Plus de 220 dégâts par manche en moyenne sur au moins 16 manches (record ≈ 180).
    if p.rounds >= 16 && p.damage as f64 / rounds > 220.0 {
        return Some("plus de 220 dégâts par manche");
    }
    None
}

/// Statistiques enregistrées avant le facteur de confiance (relues, puis complétées).
#[derive(Deserialize)]
struct StatsV1 {
    pseudo: String,
    elo: f64,
    matches: u32,
    wins: u32,
    losses: u32,
    ties: u32,
    kills: u32,
    deaths: u32,
    assists: u32,
    headshots: u32,
    damage: u64,
    rounds: u32,
    mvps: u32,
    games: u32,
    reports: u32,
    reported: Vec<String>,
    reported_by: Vec<String>,
    friends: Vec<String>,
}

impl From<StatsV1> for Stats {
    fn from(o: StatsV1) -> Self {
        // Facteur de départ d'un ancien compte : 500, moins ses signalements passés.
        let trust = (TRUST_START + trust::REPORT * o.reports as i32).clamp(0, TRUST_MAX);
        Stats {
            pseudo: o.pseudo,
            elo: o.elo,
            matches: o.matches,
            wins: o.wins,
            losses: o.losses,
            ties: o.ties,
            kills: o.kills,
            deaths: o.deaths,
            assists: o.assists,
            headshots: o.headshots,
            damage: o.damage,
            rounds: o.rounds,
            mvps: o.mvps,
            games: o.games,
            reports: o.reports,
            reported: o.reported,
            reported_by: o.reported_by,
            friends: o.friends,
            trust,
            ..Default::default()
        }
    }
}

impl Stats {
    pub fn new(pseudo: &str) -> Self {
        Self {
            pseudo: pseudo.to_string(),
            elo: START_ELO,
            trust: TRUST_START,
            ..Default::default()
        }
    }

    /// Fait bouger le facteur de confiance (borné à 0..1000).
    pub fn trust_by(&mut self, d: i32) {
        self.trust = (self.trust + d).clamp(0, TRUST_MAX);
    }

    /// Baisse « douce » (signalements, absences) : jamais sous le plancher des trolls
    /// (`trust::SOFT_FLOOR`) ; déjà en dessous, elle ne retire rien.
    pub fn trust_soft(&mut self, d: i32) {
        if d < 0 {
            let floor = self.trust.min(trust::SOFT_FLOOR);
            self.trust = (self.trust + d).max(floor).clamp(0, TRUST_MAX);
        } else {
            self.trust_by(d);
        }
    }

    /// Gain d'une partie propre : plus on a la confiance, plus elle monte (les joueurs
    /// sérieux creusent l'écart).
    pub fn clean_gain(&self) -> i32 {
        trust::CLEAN_GAME + self.trust.max(0) / 150
    }

    /// Compte neuf (moins de 5 parties) : mis de préférence avec d'autres comptes neufs.
    pub fn is_new(&self) -> bool {
        self.games < 5
    }

    /// Pénalité pour être parti `away` d'une partie qui a duré `len` (secondes) : rien
    /// sous 2 minutes, puis de plus en plus (10 minutes sur 30 : −33), jusqu'à l'abandon.
    pub fn absence_penalty(away: f64, len: f64) -> i32 {
        if away < 120.0 {
            return 0;
        }
        let frac = (away / len.max(600.0)).min(1.0);
        if frac >= 0.9 {
            return trust::ABANDON;
        }
        -((frac * frac * (-trust::ABANDON * 3) as f64).round() as i32)
            .clamp(5, -trust::ABANDON - 10)
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
                let bytes = std::fs::read(e.path()).unwrap_or_default();
                let read = postcard::from_bytes::<Stats>(&bytes)
                    .ok()
                    .or_else(|| {
                        postcard::from_bytes::<StatsV2>(&bytes)
                            .ok()
                            .map(Stats::from)
                    })
                    .or_else(|| {
                        postcard::from_bytes::<StatsV1>(&bytes)
                            .ok()
                            .map(Stats::from)
                    });
                if let Some(s) = read {
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

    /// Facteur de confiance d'un compte (500 s'il n'a jamais joué).
    pub fn trust(&self, pseudo: &str) -> i32 {
        self.get(pseudo).map_or(TRUST_START, |s| s.trust)
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
    fn trust_moves_slowly() {
        // Absences : rien sous 2 minutes, cher vers 10 minutes sur 30, abandon.
        assert_eq!(Stats::absence_penalty(90.0, 1800.0), 0);
        assert!(Stats::absence_penalty(600.0, 1800.0) <= -30);
        assert!(Stats::absence_penalty(300.0, 1800.0) > -20);
        assert_eq!(Stats::absence_penalty(1700.0, 1800.0), trust::ABANDON);
        let mut s = Stats::new("a");
        assert_eq!(s.trust, TRUST_START);
        s.trust_by(5000);
        assert_eq!(s.trust, TRUST_MAX);
        s.trust_by(-5000);
        assert_eq!(s.trust, 0);
        // Les joueurs fiables gagnent plus par partie propre.
        let (lo, hi) = (
            Stats::new("b"),
            Stats {
                trust: 900,
                ..Stats::new("c")
            },
        );
        assert!(hi.clean_gain() > lo.clean_gain());
        // Signalements et absences s'arrêtent au plancher des trolls ; les soupçons du
        // vérificateur passent dessous.
        let mut t = Stats {
            trust: 340,
            ..Stats::new("d")
        };
        t.trust_soft(trust::REPORT * 3);
        assert_eq!(t.trust, trust::SOFT_FLOOR);
        t.trust_soft(trust::ABANDON);
        assert_eq!(t.trust, trust::SOFT_FLOOR);
        t.trust_by(trust::SUSPICION);
        assert_eq!(t.trust, trust::SOFT_FLOOR + trust::SUSPICION);
        t.trust_soft(trust::REPORT);
        assert_eq!(
            t.trust,
            trust::SOFT_FLOOR + trust::SUSPICION,
            "déjà dessous : rien"
        );
    }

    #[test]
    fn old_stats_still_read() {
        let old = StatsV1 {
            pseudo: "x".into(),
            elo: 1000.0,
            matches: 3,
            wins: 1,
            losses: 1,
            ties: 1,
            kills: 0,
            deaths: 0,
            assists: 0,
            headshots: 0,
            damage: 0,
            rounds: 0,
            mvps: 0,
            games: 3,
            reports: 2,
            reported: vec![],
            reported_by: vec![],
            friends: vec![],
        };
        let s = Stats::from(old);
        assert_eq!(s.trust, TRUST_START + 2 * trust::REPORT);
    }

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
            teamkills: 0,
            kicked: false,
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
