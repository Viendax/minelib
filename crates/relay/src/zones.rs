//! Construction en ligne sans limite de joueurs : le monde est découpé en zones de
//! `ZONE × ZONE` chunks ; chaque zone active a un garant (un des joueurs) qui valide les
//! modifications et les publie. Le relais choisit les garants parmi les joueurs
//! disponibles, d'abord les plus adaptés (machine, ancienneté, réputation) et les moins
//! chargés, de sorte que les zones les plus actives aillent à des garants différents ;
//! le garant d'une zone très active est aussi remplacé de temps en temps (on n'est pas
//! toujours jugé par le même).

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use minelib_net::proto::GarantCaps;

/// Distance (en chunks) jusqu'à laquelle un joueur reçoit ce qui se passe.
pub const NEAR: i32 = 12;
/// Une zone très active change de garant au bout de ce temps.
const ROTATE: Duration = Duration::from_secs(600);
/// Une zone sans joueur proche ni activité perd son garant au bout de ce temps.
const IDLE: Duration = Duration::from_secs(60);
/// Taille maximale gardée de l'historique d'une zone (pour les arrivants).
const LOG_MAX: usize = 4 << 20;
/// Rééquilibrage au plus toutes les…
const BALANCE: Duration = Duration::from_secs(5);

/// Un joueur pouvant être garant.
#[derive(Clone, Debug)]
pub struct Candidate {
    pub id: u64,
    pub caps: GarantCaps,
    /// Temps passé dans la partie.
    pub uptime: Duration,
    /// Signalements par partie.
    pub ratio: f64,
}

/// Aptitude d'un joueur à être garant, sans tenir compte de sa charge.
pub fn aptitude(c: &Candidate) -> f64 {
    let cores = c.caps.cores.clamp(1, 32) as f64;
    let fps = c.caps.fps.min(500) as f64;
    let ram = (c.caps.ram_mb.min(32 * 1024) as f64) / 1024.0;
    let minutes = (c.uptime.as_secs_f64() / 60.0).min(60.0);
    let base = cores * 8.0 + fps / 8.0 + ram * 2.0 + minutes;
    // Un joueur souvent signalé n'est garant qu'en dernier recours.
    base / (1.0 + 10.0 * c.ratio)
}

pub struct Zone {
    pub garant: u64,
    since: Instant,
    /// Messages par seconde (moyenne glissante).
    pub activity: f64,
    count: u32,
    last_use: Instant,
    pub log: Vec<Vec<u8>>,
    log_bytes: usize,
}

#[derive(Default)]
pub struct Build {
    pub zones: HashMap<[i32; 2], Zone>,
    /// Logs des zones sans garant (gardés pour les suivants).
    orphan_logs: HashMap<[i32; 2], (Vec<Vec<u8>>, usize)>,
    pub positions: HashMap<u64, [i32; 2]>,
    /// Zones dont chaque joueur a déjà reçu l'historique.
    known: HashMap<u64, HashSet<[i32; 2]>>,
    last_balance: Option<Instant>,
}

/// Changements de garants à annoncer : (joueur, zones gagnées, zones perdues).
pub type Changes = Vec<(u64, Vec<[i32; 2]>, Vec<[i32; 2]>)>;

fn near(a: [i32; 2], b: [i32; 2]) -> bool {
    (a[0] - b[0]).abs() <= NEAR && (a[1] - b[1]).abs() <= NEAR
}

/// Le chunk est-il proche d'une zone (bord compris) ?
fn near_zone(chunk: [i32; 2], zone: [i32; 2]) -> bool {
    let z = minelib_net::proto::ZONE;
    let cx = chunk[0].clamp(zone[0] * z, zone[0] * z + z - 1);
    let cz = chunk[1].clamp(zone[1] * z, zone[1] * z + z - 1);
    near(chunk, [cx, cz])
}

impl Build {
    /// Joueurs à qui envoyer ce qui se passe au chunk `chunk`.
    pub fn listeners(&self, chunk: [i32; 2], except: u64) -> Vec<u64> {
        self.positions
            .iter()
            .filter(|(&id, &p)| id != except && near(p, chunk))
            .map(|(&id, _)| id)
            .collect()
    }

    /// Choisit le meilleur garant pour une zone : aptitude moins la charge actuelle.
    fn best(&self, cands: &[Candidate], skip: Option<u64>) -> Option<u64> {
        let load = |id: u64| -> f64 {
            self.zones
                .values()
                .filter(|z| z.garant == id)
                .map(|z| 1.0 + z.activity)
                .sum()
        };
        cands
            .iter()
            .filter(|c| Some(c.id) != skip)
            .map(|c| (c.id, aptitude(c) / (1.0 + load(c.id))))
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(id, _)| id)
            .or_else(|| cands.first().map(|c| c.id))
    }

    /// Garant de la zone (en nomme un si besoin). Renvoie le garant et les changements.
    pub fn garant(&mut self, zone: [i32; 2], cands: &[Candidate]) -> (Option<u64>, Changes) {
        if let Some(z) = self.zones.get(&zone) {
            return (Some(z.garant), Vec::new());
        }
        let Some(g) = self.best(cands, None) else {
            return (None, Vec::new());
        };
        let (log, log_bytes) = self.orphan_logs.remove(&zone).unwrap_or_default();
        let now = Instant::now();
        self.zones.insert(
            zone,
            Zone {
                garant: g,
                since: now,
                activity: 0.0,
                count: 0,
                last_use: now,
                log,
                log_bytes,
            },
        );
        (Some(g), vec![(g, vec![zone], Vec::new())])
    }

    /// Un message pour la zone (activité).
    pub fn touch(&mut self, zone: [i32; 2]) {
        if let Some(z) = self.zones.get_mut(&zone) {
            z.count += 1;
            z.last_use = Instant::now();
        }
    }

    /// Le garant publie une modification validée : gardée pour les arrivants.
    pub fn record(&mut self, zone: [i32; 2], data: &[u8]) {
        if let Some(z) = self.zones.get_mut(&zone) {
            z.log_bytes += data.len();
            z.log.push(data.to_vec());
            while z.log_bytes > LOG_MAX && !z.log.is_empty() {
                z.log_bytes -= z.log.remove(0).len();
            }
        }
    }

    /// Le joueur s'est déplacé : historiques des zones proches qu'il n'a pas encore.
    pub fn moved(&mut self, id: u64, chunk: [i32; 2]) -> Vec<([i32; 2], Vec<Vec<u8>>)> {
        self.positions.insert(id, chunk);
        let known = self.known.entry(id).or_default();
        let mut out = Vec::new();
        for (&zone, z) in &self.zones {
            if near_zone(chunk, zone) && !known.contains(&zone) && !z.log.is_empty() {
                known.insert(zone);
                out.push((zone, z.log.clone()));
            }
        }
        for (&zone, (log, _)) in &self.orphan_logs {
            if near_zone(chunk, zone) && !known.contains(&zone) && !log.is_empty() {
                known.insert(zone);
                out.push((zone, log.clone()));
            }
        }
        out
    }

    /// Historique de toutes les zones (nouveau propriétaire du monde).
    pub fn all_logs(&self) -> Vec<([i32; 2], Vec<Vec<u8>>)> {
        self.zones
            .iter()
            .map(|(&k, z)| (k, z.log.clone()))
            .chain(self.orphan_logs.iter().map(|(&k, (l, _))| (k, l.clone())))
            .filter(|(_, l)| !l.is_empty())
            .collect()
    }

    /// Historique d'une zone (nouveau garant).
    pub fn log(&self, zone: [i32; 2]) -> Vec<Vec<u8>> {
        self.zones
            .get(&zone)
            .map(|z| z.log.clone())
            .unwrap_or_default()
    }

    /// Un joueur part : ses zones vont à d'autres.
    pub fn left(&mut self, id: u64, cands: &[Candidate]) -> Changes {
        self.positions.remove(&id);
        self.known.remove(&id);
        let mine: Vec<[i32; 2]> = self
            .zones
            .iter()
            .filter(|(_, z)| z.garant == id)
            .map(|(&k, _)| k)
            .collect();
        let mut changes = Changes::new();
        for zone in mine {
            let others: Vec<Candidate> = cands.iter().filter(|c| c.id != id).cloned().collect();
            match self.best(&others, Some(id)) {
                Some(g) => {
                    if let Some(z) = self.zones.get_mut(&zone) {
                        z.garant = g;
                        z.since = Instant::now();
                    }
                    changes.push((g, vec![zone], Vec::new()));
                }
                None => {
                    if let Some(z) = self.zones.remove(&zone) {
                        self.orphan_logs.insert(zone, (z.log, z.log_bytes));
                    }
                }
            }
        }
        changes
    }

    /// Rééquilibrage périodique : activité mesurée, zones abandonnées libérées, zones
    /// actives réparties sur des garants différents et renouvelées au bout de `ROTATE`.
    pub fn balance(&mut self, cands: &[Candidate]) -> Changes {
        let now = Instant::now();
        let dt = match self.last_balance {
            Some(t) if now - t < BALANCE => return Vec::new(),
            Some(t) => (now - t).as_secs_f64(),
            None => BALANCE.as_secs_f64(),
        };
        self.last_balance = Some(now);
        type Lists = (Vec<[i32; 2]>, Vec<[i32; 2]>);
        let mut changes: HashMap<u64, Lists> = HashMap::new();
        for z in self.zones.values_mut() {
            let rate = z.count as f64 / dt;
            z.activity = z.activity * 0.5 + rate * 0.5;
            z.count = 0;
        }
        // Zones sans personne autour depuis longtemps.
        let idle: Vec<[i32; 2]> = self
            .zones
            .iter()
            .filter(|(&k, z)| {
                now - z.last_use > IDLE && !self.positions.values().any(|&p| near_zone(p, k))
            })
            .map(|(&k, _)| k)
            .collect();
        for k in idle {
            if let Some(z) = self.zones.remove(&k) {
                changes.entry(z.garant).or_default().1.push(k);
                self.orphan_logs.insert(k, (z.log, z.log_bytes));
            }
        }
        // Les zones les plus actives d'abord, chacune au meilleur garant compte tenu
        // de ce qui a déjà été donné.
        let mut busy: Vec<([i32; 2], f64)> =
            self.zones.iter().map(|(&k, z)| (k, z.activity)).collect();
        busy.sort_by(|a, b| b.1.total_cmp(&a.1));
        let mut load: HashMap<u64, f64> = HashMap::new();
        for (zone, activity) in busy {
            let current = self.zones[&zone].garant;
            let since = self.zones[&zone].since;
            let rotate = activity > 1.0 && now - since > ROTATE && cands.len() > 1;
            let score = |c: &Candidate, load: &HashMap<u64, f64>| {
                aptitude(c) / (1.0 + load.get(&c.id).copied().unwrap_or(0.0))
            };
            let pick = cands
                .iter()
                .filter(|c| !(rotate && c.id == current))
                .max_by(|a, b| score(a, &load).total_cmp(&score(b, &load)));
            let Some(pick) = pick else { continue };
            let cur_score = cands
                .iter()
                .find(|c| c.id == current)
                .map(|c| score(c, &load));
            // On ne change que pour nettement mieux (ou pour renouveler, ou garant parti).
            let better = match cur_score {
                None => true,
                Some(s) => rotate || score(pick, &load) > s * 1.5,
            };
            let g = if better { pick.id } else { current };
            *load.entry(g).or_default() += 1.0 + activity;
            if g != current {
                let z = self.zones.get_mut(&zone).expect("zone");
                z.garant = g;
                z.since = now;
                changes.entry(current).or_default().1.push(zone);
                changes.entry(g).or_default().0.push(zone);
            }
        }
        changes.into_iter().map(|(id, (g, l))| (id, g, l)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(id: u64, cores: u16) -> Candidate {
        Candidate {
            id,
            caps: GarantCaps {
                cores,
                fps: 200,
                ram_mb: 8000,
            },
            uptime: Duration::from_secs(600),
            ratio: 0.0,
        }
    }

    #[test]
    fn busy_zones_go_to_different_suited_garants() {
        let cands = vec![cand(1, 16), cand(2, 8), cand(3, 2)];
        let mut b = Build::default();
        // La première zone va au plus adapté.
        assert_eq!(b.garant([0, 0], &cands).0, Some(1));
        // La suivante, au même s'il n'est pas chargé, sinon à un autre.
        for _ in 0..50 {
            b.touch([0, 0]);
        }
        b.balance(&cands);
        let (g, _) = b.garant([5, 5], &cands);
        assert_ne!(
            g,
            Some(1),
            "zone active déjà chez 1 : une autre va ailleurs"
        );
        // Signalé souvent : en dernier.
        let mut bad = cand(4, 32);
        bad.ratio = 2.0;
        assert!(aptitude(&bad) < aptitude(&cand(3, 2)));
        // Le garant part : sa zone passe à un autre.
        let ch = b.left(1, &cands);
        assert!(ch.iter().any(|(id, g, _)| *id != 1 && g.contains(&[0, 0])));
    }
}
