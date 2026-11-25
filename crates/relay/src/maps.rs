//! Cartes FPS partagées : le relais garde chaque carte entière (vérifiée par sa
//! signature) pour que tout le monde puisse la télécharger avant de jouer.
//!
//! Seules les cartes déjà lancées en partie FPS en ligne sont proposées à tous (`fps`) :
//! un monde ouvert en construction passe aussi par le relais, mais il n'apparaît pas dans
//! la liste des cartes. Les joueurs qui ont une carte proposée la renvoient au relais s'il
//! ne l'a pas (un relais qui change, un PC-relais éteint) : elle reste disponible tant
//! que quelqu'un l'a.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Result;
use minelib_net::crypto;
use minelib_net::game;
use minelib_net::proto::{MapEntry, MAP_MAX, MAP_PART};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Meta {
    pub sig: [u8; 32],
    pub name: String,
    pub author: String,
    pub size: u64,
    pub plays: u32,
    /// Joueurs d'une partie sur cette carte (10 : 5 contre 5).
    pub players: u8,
}

/// (v307) Ce que le relais sait d'une carte en plus de son nombre de parties : les
/// comptes différents qui y ont fini une partie, et ceux qui l'ont signalée (numéros
/// tirés des pseudos, pas les pseudos). `legacy` : carte déjà très jouée avant que ces
/// comptes soient tenus.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
struct Note {
    players: Vec<u64>,
    reports: Vec<u64>,
    legacy: bool,
}

/// (v307) Une carte ne compte pour le classement qu'une fois jouée par autant de comptes
/// différents : plus qu'un salon complet, pour qu'un groupe ne puisse pas, à lui seul, y
/// faire entrer une carte faite pour gonfler son classement (ou qui n'a rien d'une carte
/// FPS). Sur un relais qui connaît peu de joueurs, la moitié d'entre eux suffit, et jamais
/// moins de `RANK_PLAYERS_MIN` (un salon complet) : voir `Store::needed`. Et elle n'y
/// compte plus si un quart de ceux qui y ont joué l'ont signalée (`/reportmap`, pendant
/// une partie sur cette carte).
pub const RANK_PLAYERS: usize = 20;
pub const RANK_PLAYERS_MIN: usize = 10;
const REPORT_SHARE: usize = 4;
/// Parties qu'avait une carte avant la v307 pour rester comptée d'office.
const LEGACY_PLAYS: u32 = 30;

/// Numéro d'un compte (stable d'un lancement à l'autre ; FNV-1a sur le pseudo en
/// minuscules).
fn who(pseudo: &str) -> u64 {
    pseudo
        .to_lowercase()
        .bytes()
        .fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
            (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
        })
}

/// Envoi en cours : (expéditeur, nom, taille annoncée, octets reçus).
type Upload = (u64, String, u64, Vec<u8>);

pub struct Store {
    dir: PathBuf,
    pub index: HashMap<[u8; 32], Meta>,
    uploads: HashMap<[u8; 32], Upload>,
    /// Cartes déjà lancées en partie FPS en ligne (proposées à tous).
    fps: std::collections::HashSet<[u8; 32]>,
    /// (v307) Voir `Note`.
    notes: HashMap<[u8; 32], Note>,
}

pub enum Put {
    /// Reçu jusque-là.
    Partial(u64),
    Complete,
    Refused(&'static str),
}

impl Store {
    pub fn open(dir: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&dir)?;
        let index: HashMap<[u8; 32], Meta> = std::fs::read(dir.join("index.bin"))
            .ok()
            .and_then(|b| postcard::from_bytes::<Vec<Meta>>(&b).ok())
            .unwrap_or_default()
            .into_iter()
            .filter(|m| dir.join(file(&m.sig)).exists())
            .map(|m| (m.sig, m))
            .collect();
        let fps = match std::fs::read(dir.join("fps.bin")) {
            Ok(b) => postcard::from_bytes::<Vec<[u8; 32]>>(&b)
                .unwrap_or_default()
                .into_iter()
                .collect(),
            // Relais d'avant cette liste : les cartes déjà jouées en FPS y entrent.
            Err(_) => index
                .values()
                .filter(|m: &&Meta| m.plays > 0)
                .map(|m| m.sig)
                .collect(),
        };
        // (v307) Relais d'avant ces comptes : les cartes déjà très jouées restent comptées.
        let notes: HashMap<[u8; 32], Note> = match std::fs::read(dir.join("notes.bin")) {
            Ok(b) => postcard::from_bytes::<Vec<([u8; 32], Note)>>(&b)
                .unwrap_or_default()
                .into_iter()
                .collect(),
            Err(_) => index
                .values()
                .filter(|m| m.plays >= LEGACY_PLAYS)
                .map(|m| {
                    let note = Note {
                        legacy: true,
                        ..Note::default()
                    };
                    (m.sig, note)
                })
                .collect(),
        };
        Ok(Self {
            dir,
            index,
            uploads: HashMap::new(),
            fps,
            notes,
        })
    }

    fn save_notes(&self) {
        let v: Vec<(&[u8; 32], &Note)> = self.notes.iter().collect();
        if let Ok(b) = postcard::to_allocvec(&v) {
            let _ = crate::accounts::write_atomic(&self.dir.join("notes.bin"), &b);
        }
    }

    /// (v307) Comptes différents qu'il faut à une carte pour compter : la moitié de ceux
    /// que ce relais a vus jouer en FPS, entre `RANK_PLAYERS_MIN` et `RANK_PLAYERS`.
    pub fn needed(&self) -> usize {
        let mut all: Vec<u64> = self
            .notes
            .values()
            .flat_map(|n| n.players.iter().copied())
            .collect();
        all.sort_unstable();
        all.dedup();
        (all.len() / 2).clamp(RANK_PLAYERS_MIN, RANK_PLAYERS)
    }

    /// (v307) Cette carte peut-elle compter pour le classement ? Voir `RANK_PLAYERS`.
    pub fn rankable(&self, sig: &[u8; 32]) -> bool {
        let Some(n) = self.notes.get(sig) else {
            return false;
        };
        let known = n.players.len().max(if n.legacy { RANK_PLAYERS } else { 0 });
        known >= self.needed() && n.reports.len() * REPORT_SHARE < known
    }

    /// (v307) Ces comptes ont joué une partie sur cette carte (dit avant de savoir si
    /// elle compte : la partie qui finit compte pour sa propre carte).
    pub fn seen(&mut self, sig: &[u8; 32], players: &[String]) {
        let n = self.notes.entry(*sig).or_default();
        let before = n.players.len();
        for p in players.iter().filter(|p| !p.starts_with("BOT ")) {
            let id = who(p);
            if !n.players.contains(&id) {
                n.players.push(id);
            }
        }
        if n.players.len() != before {
            self.save_notes();
        }
    }

    /// Essais : cette carte compte d'office, comme une carte déjà très jouée.
    #[cfg(test)]
    pub fn grandfather(&mut self, sig: [u8; 32]) {
        self.notes.entry(sig).or_default().legacy = true;
    }

    /// (v307) Le compte `pseudo` signale la carte (il est en partie dessus : vérifié par
    /// le relais). Une fois par compte. Vrai si le signalement est nouveau.
    pub fn report(&mut self, sig: &[u8; 32], pseudo: &str) -> bool {
        if !self.notes.contains_key(sig) && !self.index.contains_key(sig) {
            return false;
        }
        let n = self.notes.entry(*sig).or_default();
        let id = who(pseudo);
        if n.reports.contains(&id) {
            return false;
        }
        n.reports.push(id);
        self.save_notes();
        true
    }

    /// Cette carte a été lancée en partie FPS en ligne : proposée à tous désormais.
    pub fn mark_fps(&mut self, sig: [u8; 32]) {
        if self.fps.insert(sig) {
            let v: Vec<[u8; 32]> = self.fps.iter().copied().collect();
            if let Ok(b) = postcard::to_allocvec(&v) {
                let _ = crate::accounts::write_atomic(&self.dir.join("fps.bin"), &b);
            }
        }
    }

    fn save_index(&self) {
        let v: Vec<&Meta> = self.index.values().collect();
        if let Ok(b) = postcard::to_allocvec(&v) {
            let _ = crate::accounts::write_atomic(&self.dir.join("index.bin"), &b);
        }
    }

    pub fn has(&self, sig: &[u8; 32]) -> bool {
        self.index.contains_key(sig)
    }

    /// Cette carte est-elle parmi les `n` cartes 5 contre 5 les plus jouées ? Le classement
    /// ne compte que celles-là (pas de classement gonflé sur une carte faite pour ça).
    /// (v307) Et seulement si elle peut compter (`rankable`) ; les cartes qui ne le
    /// peuvent pas ne prennent pas de place parmi les `n`.
    pub fn among_top(&self, sig: &[u8; 32], n: usize) -> bool {
        if !self.rankable(sig) {
            return false;
        }
        let mine = self.index.get(sig).map_or(0, |m| m.plays);
        let ahead = self
            .index
            .iter()
            .filter(|(s, m)| *s != sig && m.players == 10 && m.plays > mine && self.rankable(s))
            .count();
        ahead < n
    }

    /// Une partie vient de finir sur cette carte, jouée par ces comptes.
    pub fn played(&mut self, sig: &[u8; 32], players: &[String]) {
        if let Some(m) = self.index.get_mut(sig) {
            m.plays += 1;
            self.save_index();
        }
        self.seen(sig, players);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn put(
        &mut self,
        from: u64,
        author: &str,
        sig: [u8; 32],
        name: &str,
        offset: u64,
        total: u64,
        data: &[u8],
    ) -> Put {
        if self.has(&sig) {
            return Put::Complete;
        }
        if total == 0 || total > MAP_MAX || data.len() > MAP_PART {
            return Put::Refused("carte trop grande");
        }
        if offset == 0 {
            self.uploads.insert(
                sig,
                (from, name.chars().take(40).collect(), total, Vec::new()),
            );
        }
        let Some(up) = self.uploads.get_mut(&sig) else {
            return Put::Partial(0);
        };
        if up.0 != from || up.2 != total || offset != up.3.len() as u64 {
            return Put::Partial(up.3.len() as u64);
        }
        up.3.extend_from_slice(data);
        if (up.3.len() as u64) < total {
            return Put::Partial(up.3.len() as u64);
        }
        let (_, name, _, bytes) = self.uploads.remove(&sig).expect("envoi");
        let Some(package) = postcard::from_bytes::<game::MapPackage>(&bytes)
            .ok()
            .filter(|_| game::signature(&bytes) == sig)
        else {
            return Put::Refused("signature de la carte fausse");
        };
        if crate::accounts::write_atomic(&self.dir.join(file(&sig)), &bytes).is_err() {
            return Put::Refused("écriture impossible");
        }
        self.index.insert(
            sig,
            Meta {
                sig,
                name,
                author: author.to_string(),
                size: total,
                plays: 0,
                players: package.fps.players(),
            },
        );
        self.save_index();
        log::info!("carte {} reçue de « {author} »", crypto::hex(&sig[..6]));
        Put::Complete
    }

    /// Morceau d'une carte : (taille totale, octets).
    pub fn get(&self, sig: &[u8; 32], offset: u64) -> Option<(u64, Vec<u8>)> {
        let m = self.index.get(sig)?;
        use std::io::{Read, Seek, SeekFrom};
        let mut f = std::fs::File::open(self.dir.join(file(sig))).ok()?;
        f.seek(SeekFrom::Start(offset)).ok()?;
        let mut buf = vec![0u8; MAP_PART.min(m.size.saturating_sub(offset) as usize)];
        f.read_exact(&mut buf).ok()?;
        Some((m.size, buf))
    }

    pub fn entries(&self) -> Vec<MapEntry> {
        self.index
            .values()
            .filter(|m| self.fps.contains(&m.sig))
            .map(|m| MapEntry {
                sig: m.sig,
                name: m.name.clone(),
                author: m.author.clone(),
                size: m.size,
                plays: m.plays,
                waiting: 0,
                stored: true,
                players: m.players,
            })
            .collect()
    }

    /// Un jeu se déconnecte : ses envois en cours sont abandonnés.
    pub fn forget(&mut self, from: u64) {
        self.uploads.retain(|_, u| u.0 != from);
    }
}

fn file(sig: &[u8; 32]) -> String {
    format!("{}.map", crypto::hex(sig))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(maps: &[(u8, u32)]) -> Store {
        let dir = std::env::temp_dir().join(format!("minelib-cartes-{}", rand::random::<u32>()));
        let mut s = Store::open(dir).unwrap();
        for &(n, plays) in maps {
            let meta = Meta {
                sig: [n; 32],
                name: format!("carte {n}"),
                author: "essai".into(),
                size: 1,
                plays,
                players: 10,
            };
            s.index.insert([n; 32], meta);
        }
        s
    }

    fn names(from: usize, n: usize) -> Vec<String> {
        (from..from + n).map(|i| format!("Joueur{i}")).collect()
    }

    /// (v307) Une carte ne compte pour le classement qu'une fois jouée par 20 comptes
    /// différents : un même salon qui la rejoue cent fois n'y suffit pas, les bots ne
    /// comptent pas. Et elle n'y compte plus quand un quart de ses joueurs l'ont signalée.
    #[test]
    fn a_map_counts_for_the_ranking_only_with_enough_different_players_and_few_reports() {
        // Sur un relais qui connaît peu de joueurs, un salon complet suffit (pas 9).
        let mut small = store(&[(1, 0)]);
        small.seen(&[1; 32], &names(0, 9));
        assert!(!small.rankable(&[1; 32]));
        small.seen(&[1; 32], &names(9, 1));
        assert!(small.rankable(&[1; 32]) && small.needed() == RANK_PLAYERS_MIN);
        // Un relais qui a vu jouer 50 comptes : il en faut 20.
        let mut s = store(&[(1, 0), (2, 0), (3, 0)]);
        s.seen(&[3; 32], &names(100, 40));
        let lobby = names(0, 10);
        for _ in 0..100 {
            s.played(&[1; 32], &lobby);
        }
        assert_eq!(s.index[&[1; 32]].plays, 100);
        assert!(!s.rankable(&[1; 32]) && !s.among_top(&[1; 32], 10));
        s.played(&[1; 32], &["BOT Albert".into(), "BOT Fox".into()]);
        s.played(&[1; 32], &names(10, 9));
        assert!(!s.rankable(&[1; 32]), "19 comptes");
        s.played(&[1; 32], &names(19, 1));
        assert!(s.rankable(&[1; 32]) && s.among_top(&[1; 32], 10));
        assert_eq!(s.needed(), RANK_PLAYERS);
        // Une carte qui ne peut pas compter ne prend pas de place devant les autres.
        for _ in 0..500 {
            s.played(&[2; 32], &lobby);
        }
        assert!(!s.rankable(&[2; 32]) && s.among_top(&[1; 32], 1));
        // Signalements : un par compte ; au quart des joueurs, la carte ne compte plus.
        for p in names(0, 4) {
            assert!(s.report(&[1; 32], &p));
            assert!(!s.report(&[1; 32], &p.to_uppercase()), "une fois par compte");
        }
        assert!(s.rankable(&[1; 32]), "4 sur 20");
        assert!(s.report(&[1; 32], "Joueur4"));
        assert!(!s.rankable(&[1; 32]) && !s.among_top(&[1; 32], 10));
        assert!(!s.report(&[9; 32], "Joueur1"), "carte inconnue");
        // Tout cela se relit après un redémarrage du relais.
        let dir = s.dir.clone();
        std::fs::write(dir.join(file(&[1; 32])), b"x").unwrap();
        s.save_index();
        let again = Store::open(dir).unwrap();
        assert!(again.index.contains_key(&[1; 32]) && !again.rankable(&[1; 32]));
        assert_eq!(again.notes[&[1; 32]].players.len(), 20);
    }

    /// Une carte déjà très jouée avant la v307 reste comptée ; une carte peu jouée devra
    /// trouver ses 20 joueurs.
    #[test]
    fn maps_already_much_played_stay_ranked() {
        let s = store(&[(1, 40), (2, 5)]);
        let dir = s.dir.clone();
        for n in [1u8, 2] {
            std::fs::write(dir.join(file(&[n; 32])), b"x").unwrap();
        }
        s.save_index();
        let again = Store::open(dir).unwrap();
        assert!(again.rankable(&[1; 32]) && !again.rankable(&[2; 32]));
    }
}
