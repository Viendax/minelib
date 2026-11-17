//! Construction en ligne sans limite de joueurs : le monde est découpé en zones de
//! `ZONE × ZONE` chunks ; chaque zone active a un garant (un des joueurs) qui valide les
//! modifications et les publie. Le relais choisit les garants parmi les joueurs
//! disponibles, d'abord les plus adaptés (machine, ancienneté, réputation) et les moins
//! chargés, de sorte que les zones les plus actives aillent à des garants différents ;
//! le garant d'une zone très active est aussi remplacé de temps en temps (on n'est pas
//! toujours jugé par le même).

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use minelib_net::proto::GarantCaps;

/// Distance (en chunks) jusqu'à laquelle un joueur reçoit ce qui se passe.
pub const NEAR: i32 = 12;
/// Une zone très active change de garant au bout de ce temps.
const ROTATE: Duration = Duration::from_secs(600);
/// Une zone sans joueur proche ni activité perd son garant au bout de ce temps.
const IDLE: Duration = Duration::from_secs(60);
/// Taille maximale gardée de l'historique d'une zone (pour les arrivants). L'historique est
/// compacté (la dernière modification de chaque case), cette limite ne sert qu'aux
/// messages qui ne sont pas des blocs.
const LOG_MAX: usize = 16 << 20;

/// Historique d'une zone : chaque modification a un numéro croissant ; une nouvelle
/// modification d'une case remplace l'ancienne (l'historique ne grossit qu'avec le nombre
/// de cases changées, pas avec le temps).
#[derive(Default)]
pub struct ZoneLog {
    entries: BTreeMap<u64, Vec<u8>>,
    by_pos: HashMap<LogKey, u64>,
    bytes: usize,
}

/// Ce qu'une entrée d'historique décrit (la suivante de même clé la remplace).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum LogKey {
    Block([i32; 3]),
    Item(u64),
    Slot([i32; 3], u8),
}

impl ZoneLog {
    fn push(&mut self, seq: u64, data: &[u8]) {
        use minelib_net::game::BuildMsg;
        // Une seule entrée par case, par objet au sol, par emplacement de coffre : la
        // dernière.
        let key = match minelib_net::game::decode::<BuildMsg>(data) {
            Some(BuildMsg::Set { pos, .. }) => Some(LogKey::Block(pos)),
            Some(BuildMsg::Item { id, .. }) => Some(LogKey::Item(id)),
            Some(BuildMsg::Slot { pos, slot, .. }) => Some(LogKey::Slot(pos, slot)),
            _ => None,
        };
        if let Some(key) = key {
            if let Some(old) = self.by_pos.insert(key, seq) {
                if let Some(d) = self.entries.remove(&old) {
                    self.bytes -= d.len();
                }
            }
        }
        self.bytes += data.len();
        self.entries.insert(seq, data.to_vec());
        while self.bytes > LOG_MAX {
            let Some((&k, _)) = self.entries.iter().next() else {
                break;
            };
            let d = self.entries.remove(&k).expect("entrée");
            self.bytes -= d.len();
            self.by_pos.retain(|_, s| *s != k);
        }
    }

    /// Modifications après le numéro `after`.
    pub fn since(&self, after: u64) -> Vec<Vec<u8>> {
        self.entries
            .range(after + 1..)
            .map(|(_, d)| d.clone())
            .collect()
    }

    pub fn last(&self) -> u64 {
        self.entries.keys().next_back().copied().unwrap_or(0)
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }
}
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
    /// Aller-retour jusqu'au relais (ms), s'il est connu.
    pub rtt: Option<f32>,
}

/// Aptitude d'un joueur à être garant, sans tenir compte de sa charge.
pub fn aptitude(c: &Candidate) -> f64 {
    let cores = c.caps.cores.clamp(1, 32) as f64;
    let fps = c.caps.fps.min(500) as f64;
    let ram = (c.caps.ram_mb.min(32 * 1024) as f64) / 1024.0;
    let minutes = (c.uptime.as_secs_f64() / 60.0).min(60.0);
    let base = cores * 8.0 + fps / 8.0 + ram * 2.0 + minutes;
    // Un joueur souvent signalé n'est garant qu'en dernier recours ; une connexion lente
    // compte beaucoup (tout passe par lui) : 200 ms divisent l'aptitude par 5.
    let slow = 1.0 + (c.rtt.unwrap_or(40.0) as f64 / 50.0).powi(2) / 4.0;
    base / (1.0 + 10.0 * c.ratio) / slow
}

pub struct Zone {
    pub garant: u64,
    since: Instant,
    /// Messages par seconde (moyenne glissante).
    pub activity: f64,
    count: u32,
    last_use: Instant,
    pub log: ZoneLog,
}

#[derive(Default)]
pub struct Build {
    pub zones: HashMap<[i32; 2], Zone>,
    /// Logs des zones sans garant (gardés pour les suivants).
    orphan_logs: HashMap<[i32; 2], ZoneLog>,
    pub positions: HashMap<u64, [i32; 2]>,
    /// Ce que chaque joueur a déjà reçu de chaque zone (dernier numéro).
    known: HashMap<u64, HashMap<[i32; 2], u64>>,
    last_balance: Option<Instant>,
    /// Numéro de la prochaine modification.
    next_seq: u64,
    /// Positions envoyées par chaque joueur (pour n'en transmettre qu'une partie au loin).
    pos_count: HashMap<u64, u32>,
    /// Droit d'envoyer de chaque joueur : (dernier compte, positions encore permises).
    pos_tokens: HashMap<u64, (Instant, f32)>,
    /// Joueurs par chunk (même contenu que `positions`, rangé pour trouver les voisins
    /// sans parcourir tout le monde) ; `indexed` : nombre de joueurs rangés.
    by_chunk: HashMap<[i32; 2], Vec<u64>>,
    indexed: usize,
    /// Objets au sol et coffres du monde : ce relais en est l'arbitre.
    pub items: minelib_net::items::ItemStore,
}

/// Positions dans une foule : chaque joueur est vu de tous ceux à portée (jusqu'à
/// `FANOUT`), mais plus un voisin est loin dans le rang, moins sa position est rafraîchie :
/// les `FANOUT_FULL` plus proches reçoivent chaque position, puis une sur 4 jusqu'à 128,
/// une sur 16 jusqu'à 512, une sur 64 jusqu'à `FANOUT` (le jeu fait avancer chacun en
/// douceur entre deux). Le travail moyen par position reste borné (≈ 100 envois) quelle
/// que soit la taille de la foule.
///
/// Ces nombres valent pour toute la région : quand elle a plusieurs relais, chacun n'en
/// sert que sa part (voir `nearby`). Avant, chaque relais appliquait la limite entière de
/// son côté, et une région à 8 relais envoyait 8 fois plus qu'une région à un seul
/// (simulation : 1 million de messages par seconde et par relais pour 700 joueurs
/// serrés, soit 855 Mbit/s à sortir de la connexion du joueur qui héberge le relais).
pub const FANOUT: usize = 2048;
pub const FANOUT_FULL: usize = 32;

/// À combien de voisins part la `n`-ième position d'un joueur (voir `FANOUT`).
pub fn reach(n: u32) -> usize {
    if n.is_multiple_of(64) {
        FANOUT
    } else if n.is_multiple_of(16) {
        512
    } else if n.is_multiple_of(4) {
        128
    } else {
        FANOUT_FULL
    }
}

/// Positions par seconde acceptées d'un joueur (le jeu en envoie 60 au plus) : au-delà,
/// elles ne sont pas diffusées (ancienne version du jeu, qui en envoyait jusqu'à 256, ou
/// jeu modifié). De quoi absorber une rafale (réseau qui regroupe les envois).
pub const POS_MAX_HZ: f32 = 66.0;
const POS_BURST: f32 = 8.0;

/// Une modification du monde est poussée tout de suite à ce nombre de joueurs au plus ;
/// au-delà (foule), chacun la reçoit avec sa prochaine position, compactée. Le travail par
/// modification ne dépend plus de la taille de la foule.
pub const LIVE_MAX: usize = 256;

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
    /// Range les joueurs par chunk si ce n'est pas fait (positions changées en direct).
    fn reindex(&mut self) {
        if self.indexed == self.positions.len() {
            return;
        }
        self.by_chunk.clear();
        let mut ids: Vec<(u64, [i32; 2])> = self.positions.iter().map(|(&i, &c)| (i, c)).collect();
        ids.sort_unstable();
        for (id, c) in ids {
            self.by_chunk.entry(c).or_default().push(id);
        }
        self.indexed = self.positions.len();
    }

    /// Note le chunk d'un joueur (`None` : parti).
    fn place(&mut self, id: u64, chunk: Option<[i32; 2]>) {
        self.reindex();
        let old = match chunk {
            Some(c) => self.positions.insert(id, c),
            None => self.positions.remove(&id),
        };
        if old == chunk {
            return;
        }
        if let Some(o) = old {
            if let Some(v) = self.by_chunk.get_mut(&o) {
                v.retain(|&m| m != id);
                if v.is_empty() {
                    self.by_chunk.remove(&o);
                }
            }
        }
        if let Some(c) = chunk {
            self.by_chunk.entry(c).or_default().push(id);
        }
        self.indexed = self.positions.len();
    }

    /// Ce joueur peut-il encore envoyer une position maintenant (`POS_MAX_HZ` par
    /// seconde, lissé) ? Sinon elle n'est pas diffusée.
    pub fn pos_due(&mut self, id: u64) -> bool {
        self.pos_due_at(id, Instant::now())
    }

    fn pos_due_at(&mut self, id: u64, now: Instant) -> bool {
        let t = self.pos_tokens.entry(id).or_insert((now, POS_BURST));
        let gained = now.saturating_duration_since(t.0).as_secs_f32() * POS_MAX_HZ;
        *t = (now, (t.1 + gained).min(POS_BURST));
        if t.1 < 1.0 {
            return false;
        }
        t.1 -= 1.0;
        true
    }

    /// À qui transmettre la position d'un joueur : à chaque fois aux joueurs tout proches
    /// (2 chunks), une fois sur 4 jusqu'à 6 chunks, une fois sur 20 au-delà (12 chunks au
    /// plus), les plus proches d'abord, et à `reach(n)` joueurs au plus. Dans une foule,
    /// chacun voit bien ses voisins (toujours les mêmes : ceux rangés après lui dans son
    /// chunk), et le relais ne parcourt ni n'envoie jamais à toute la foule.
    ///
    /// `relays` : nombre de relais de la région (1 pour le relais du monde). Chacun ne
    /// sert que sa part de la limite, puisqu'il ne porte que sa part des joueurs.
    pub fn nearby(&mut self, id: u64, chunk: [i32; 2], relays: usize) -> Vec<u64> {
        self.reindex();
        let n = self.pos_count.entry(id).or_insert(0);
        *n = n.wrapping_add(1);
        let n = *n;
        let limit = reach(n).div_ceil(relays.max(1));
        let mut out = Vec::new();
        for d in 0..=NEAR {
            let every = match d {
                0..=2 => 1,
                3..=6 => 4,
                _ => 20,
            };
            if !n.is_multiple_of(every) {
                continue;
            }
            // Anneau de chunks à la distance `d`.
            for dx in -d..=d {
                for dz in -d..=d {
                    if dx.abs().max(dz.abs()) != d {
                        continue;
                    }
                    let Some(members) = self.by_chunk.get(&[chunk[0] + dx, chunk[1] + dz]) else {
                        continue;
                    };
                    let room = limit - out.len();
                    if members.len() <= room {
                        out.extend(members.iter().copied().filter(|&m| m != id));
                    } else {
                        // Plus de monde que de place : les voisins de rang (ceux qui
                        // suivent ce joueur dans son chunk). Pour un autre chunk, un rang
                        // de départ propre à chaque joueur : en partant toujours du début,
                        // les premiers du chunk recevaient les positions de toute la foule
                        // voisine (16 fois la moyenne à 3000 joueurs, en simulation).
                        let start = members
                            .iter()
                            .position(|&m| m == id)
                            .map_or(id as usize % members.len(), |i| i + 1);
                        out.extend(
                            (0..members.len())
                                .map(|k| members[(start + k) % members.len()])
                                .filter(|&m| m != id)
                                .take(room),
                        );
                    }
                    if out.len() >= limit {
                        return out;
                    }
                }
            }
        }
        out
    }

    /// Chunks où se trouvent au moins deux joueurs (pour la fréquentation).
    pub fn crowded_chunks(&self) -> impl Iterator<Item = &Vec<u64>> {
        self.by_chunk.values().filter(|m| m.len() >= 2)
    }

    /// Zones actives (des messages ces dernières secondes).
    pub fn active(&self) -> usize {
        self.zones.values().filter(|z| z.activity > 0.05).count()
    }

    /// Joueurs à qui envoyer ce qui se passe au chunk `chunk` (tous ceux à portée : une
    /// modification du monde doit arriver à chacun).
    pub fn listeners(&self, chunk: [i32; 2], except: u64) -> Vec<u64> {
        // Peu de chunks occupés (une foule au même endroit) ou index pas à jour : par les
        // chunks occupés ; sinon par les chunks voisins.
        if self.indexed == self.positions.len() {
            return self
                .by_chunk
                .iter()
                .filter(|(&c, _)| near(c, chunk))
                .flat_map(|(_, m)| m.iter().copied())
                .filter(|&id| id != except)
                .collect();
        }
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
        let log = self.orphan_logs.remove(&zone).unwrap_or_default();
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

    /// Le garant publie une modification validée : gardée pour les arrivants. Renvoie, pour
    /// chaque joueur proche, ce qu'il doit recevoir : la modification seule s'il avait tout
    /// le reste, sinon tout ce qui lui manque (rien n'est jamais perdu, même s'il s'était
    /// éloigné un moment).
    pub fn record(
        &mut self,
        zone: [i32; 2],
        data: &[u8],
        listeners: &[u64],
    ) -> Vec<(u64, Vec<Vec<u8>>)> {
        if !self.zones.contains_key(&zone) {
            return Vec::new();
        }
        self.mirror_to(zone, data, listeners)
    }

    /// Comme `record`, même pour une zone sans garant (relais de région, copie gardée par
    /// le relais du monde).
    pub fn mirror_to(
        &mut self,
        zone: [i32; 2],
        data: &[u8],
        listeners: &[u64],
    ) -> Vec<(u64, Vec<Vec<u8>>)> {
        let log = match self.zones.get_mut(&zone) {
            Some(z) => &mut z.log,
            None => self.orphan_logs.entry(zone).or_default(),
        };
        let before = log.last();
        self.next_seq = self.next_seq.max(before) + 1;
        let seq = self.next_seq;
        log.push(seq, data);
        let mut out = Vec::new();
        // Dans une foule, la modification n'est poussée tout de suite qu'aux `LIVE_MAX`
        // premiers ; les autres la reçoivent à leur prochaine position (20 fois par
        // seconde), compactée : la dernière valeur de chaque case, pas chaque étape.
        for &id in listeners.iter().take(LIVE_MAX) {
            let known = self.known.entry(id).or_default().entry(zone).or_insert(0);
            if *known >= before {
                out.push((id, vec![data.to_vec()]));
            } else {
                out.push((id, log.since(*known)));
            }
            *known = seq;
        }
        out
    }

    /// Garde une modification venue d'ailleurs (relais de région, historique reçu).
    pub fn mirror(&mut self, zone: [i32; 2], data: &[u8]) {
        self.mirror_to(zone, data, &[]);
    }

    /// Zones dont on a l'historique (avec ou sans garant).
    pub fn logs_in(&self, keep: impl Fn([i32; 2]) -> bool) -> Vec<([i32; 2], Vec<Vec<u8>>)> {
        self.zones
            .iter()
            .map(|(&k, z)| (k, &z.log))
            .chain(self.orphan_logs.iter().map(|(&k, l)| (k, l)))
            .filter(|(k, l)| keep(*k) && l.last() > 0)
            .map(|(k, l)| (k, l.since(0)))
            .collect()
    }

    /// Ce qu'un joueur n'a pas encore reçu : des zones proches de `chunk`, ou de toutes les
    /// zones (`None` : le propriétaire, qui enregistre le monde).
    pub fn catch_up(&mut self, id: u64, chunk: Option<[i32; 2]>) -> Vec<([i32; 2], Vec<Vec<u8>>)> {
        let known = self.known.entry(id).or_default();
        let mut out = Vec::new();
        let mut take = |zone: [i32; 2], log: &ZoneLog| {
            let k = known.entry(zone).or_insert(0);
            if log.last() > *k {
                out.push((zone, log.since(*k)));
                *k = log.last();
            }
        };
        match chunk {
            // Autour d'un joueur : seulement les zones à portée (cherchées par leurs
            // coordonnées, sans parcourir toutes les zones du monde).
            Some(c) => {
                let z = minelib_net::proto::ZONE;
                for zx in (c[0] - NEAR).div_euclid(z)..=(c[0] + NEAR).div_euclid(z) {
                    for zz in (c[1] - NEAR).div_euclid(z)..=(c[1] + NEAR).div_euclid(z) {
                        let zone = [zx, zz];
                        if let Some(zn) = self.zones.get(&zone) {
                            take(zone, &zn.log);
                        }
                        if let Some(l) = self.orphan_logs.get(&zone) {
                            take(zone, l);
                        }
                    }
                }
            }
            None => {
                for (&zone, zn) in &self.zones {
                    take(zone, &zn.log);
                }
                for (&zone, l) in &self.orphan_logs {
                    take(zone, l);
                }
            }
        }
        out
    }

    /// Le joueur s'est déplacé : ce qui lui manque des zones proches.
    pub fn moved(&mut self, id: u64, chunk: [i32; 2]) -> Vec<([i32; 2], Vec<Vec<u8>>)> {
        self.place(id, Some(chunk));
        self.catch_up(id, Some(chunk))
    }

    /// Historique de toutes les zones (nouveau propriétaire du monde), noté comme reçu.
    pub fn all_logs(&mut self, id: u64) -> Vec<([i32; 2], Vec<Vec<u8>>)> {
        self.known.remove(&id);
        self.catch_up(id, None)
    }

    /// Un joueur part : ses zones vont à d'autres.
    pub fn left(&mut self, id: u64, cands: &[Candidate]) -> Changes {
        self.place(id, None);
        self.known.remove(&id);
        self.pos_count.remove(&id);
        self.pos_tokens.remove(&id);
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
                        self.orphan_logs.insert(zone, z.log);
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
                self.orphan_logs.insert(k, z.log);
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
    /// Simulation de foule dans une seule région, avec le vrai code de diffusion
    /// (`Build::nearby`, répartition `shard_for`, tailles réelles des messages) : ce que
    /// chaque relais de région doit envoyer et ce que chaque joueur reçoit, par seconde.
    /// Lancée à la main : `cargo test -p minelib-relay foule -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn foule_dans_une_region() {
        use minelib_net::game::{self, BuildMsg};
        use minelib_net::proto::{self, Response, ShardInfo};
        // Règle d'envoi du jeu (crates/app/src/buildnet.rs, `pos_every`).
        let hz = |others: usize| crate::tests_support::pos_hz(others);
        // Taille réelle d'une position sur le fil : message chiffré (étiquette de 16
        // octets, longueur sur 2). Les messages partent regroupés (voir
        // `transport::OUT_MAX`) : les 40 octets d'en-têtes IP et TCP se partagent entre
        // une vingtaine de positions, comptés 2 chacune. (Avant le regroupement, chaque
        // position partait seule : 105 octets.)
        let data = game::encode(&BuildMsg::Player {
            pseudo: "Joueur1234".into(),
            pos: [123.456, 78.9, -321.5],
            yaw: 1.234,
            pitch: -0.2,
            crouch: 0.0,
            speed: 5.5,
        });
        let payload = postcard::to_allocvec(&Response::FromZone {
            peer: 123_456,
            chunk: [17, -3],
            data,
        })
        .unwrap()
        .len();
        let wire = payload + 16 + 2 + 2;
        eprintln!("FOULE une position : {payload} octets utiles, {wire} sur le fil");
        let mut seed = 0x1234_5678u32;
        let mut rnd = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        // (nom, côté en chunks de la zone où se tiennent les joueurs)
        for (layout, side) in [
            ("serrés (2 chunks)", 2),
            ("village (8 chunks)", 8),
            ("étalés (région entière)", 32),
        ] {
            for n in [100usize, 300, 700, 1500, 3000, 6000] {
                let relays = crate::shards_wanted(n, crate::SHARD_FROM, crate::SHARD_SHARE).max(1);
                let list: Vec<ShardInfo> = (0..relays)
                    .map(|i| ShardInfo {
                        region: [0, 0],
                        slot: i as u16,
                        node: [i as u8; 32],
                        addr: String::new(),
                        key: [i as u8 + 1; 32],
                    })
                    .collect();
                let mut builds: Vec<Build> = (0..relays).map(|_| Build::default()).collect();
                // Joueurs : numéro, chunk, relais.
                let players: Vec<(u64, [i32; 2], usize)> = (0..n)
                    .map(|i| {
                        let c = [
                            16 - side / 2 + (rnd() % side as u32) as i32,
                            16 - side / 2 + (rnd() % side as u32) as i32,
                        ];
                        let s =
                            proto::shard_for(&list, &format!("Joueur{i}")).unwrap().slot as usize;
                        (i as u64 + 1, c, s)
                    })
                    .collect();
                for &(id, c, s) in &players {
                    builds[s].moved(id, c);
                }
                let mut rate = vec![20.0f32; n];
                let mut report = String::new();
                // Deux passes : la première sert à savoir combien de joueurs chacun voit
                // (le jeu règle son rythme d'envoi là-dessus), la seconde mesure.
                for pass in 0..2 {
                    let mut seen: Vec<std::collections::HashSet<u64>> = vec![Default::default(); n];
                    let mut got = vec![0u64; n]; // positions reçues par joueur
                    let mut out = vec![0u64; relays]; // messages envoyés par relais (joueurs + relais)
                    let mut inn = vec![0u64; relays]; // messages reçus par relais
                    let t0 = std::time::Instant::now();
                    for (i, &(id, c, s)) in players.iter().enumerate() {
                        for _ in 0..rate[i].round() as usize {
                            inn[s] += 1;
                            for (r, b) in builds.iter_mut().enumerate() {
                                if r != s {
                                    out[s] += 1; // au relais voisin
                                    inn[r] += 1;
                                }
                                for m in b.nearby(id, c, relays) {
                                    out[r] += 1;
                                    got[m as usize - 1] += 1;
                                    if pass == 0 {
                                        seen[m as usize - 1].insert(id);
                                    }
                                }
                            }
                        }
                    }
                    let cpu = t0.elapsed().as_secs_f32();
                    if pass == 0 {
                        for i in 0..n {
                            rate[i] = hz(seen[i].len());
                        }
                        continue;
                    }
                    let sent: f32 = rate.iter().sum();
                    let worst_out = *out.iter().max().unwrap();
                    let worst_got = *got.iter().max().unwrap();
                    let mean_got = got.iter().sum::<u64>() / n as u64;
                    report = format!(
                        "{n:>5} joueurs, {relays} relais | envoi moyen {:.0}/s par joueur | relais le plus chargé : {} msg/s sortants = {:.1} Mbit/s, {} entrants | joueur : {} reçus/s en moyenne ({:.0} kbit/s), {} au pire | calcul de la diffusion : {:.2} s de processeur par seconde",
                        sent / n as f32,
                        worst_out,
                        worst_out as f32 * wire as f32 * 8.0 / 1e6,
                        inn.iter().max().unwrap(),
                        mean_got,
                        mean_got as f32 * wire as f32 * 8.0 / 1e3,
                        worst_got,
                        cpu,
                    );
                }
                eprintln!("FOULE {layout:<24} {report}");
                // Construction : chaque joueur pose ou casse un bloc toutes les deux
                // secondes, là où il est ; chaque modification va à tous les joueurs à
                // portée (le monde doit rester le même pour tous).
                let set = postcard::to_allocvec(&Response::FromZone {
                    peer: 0,
                    chunk: [17, -3],
                    data: game::encode(&BuildMsg::Set {
                        pos: [123, 64, -321],
                        block: 37,
                        by: "Joueur1234".into(),
                    }),
                })
                .unwrap()
                .len()
                    + 16
                    + 2
                    + 2;
                let mut out = vec![0f64; relays];
                let mut got = 0f64;
                for &(_, c, _) in &players {
                    for (r, b) in builds.iter().enumerate() {
                        let l = b.listeners(c, u64::MAX).len() as f64 * 0.5;
                        out[r] += l;
                        got += l;
                    }
                }
                let worst = out.iter().cloned().fold(0.0, f64::max);
                eprintln!(
                    "FOULE {layout:<24} {n:>5} joueurs, construction (1 bloc / 2 s chacun, {set} octets) | relais le plus chargé : {:.0} msg/s = {:.1} Mbit/s | joueur : {:.0} modifications reçues/s ({:.0} kbit/s)",
                    worst,
                    worst * set as f64 * 8.0 / 1e6,
                    got / n as f64,
                    got / n as f64 * set as f64 * 8.0 / 1e3,
                );
            }
        }
    }

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
            rtt: None,
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
        // Historique compacté : la dernière modification de chaque case.
        let set = |x: i32, block: u16| {
            minelib_net::game::encode(&minelib_net::game::BuildMsg::Set {
                pos: [x, 64, 0],
                block,
                by: String::new(),
            })
        };
        for i in 0..100 {
            b.record([0, 0], &set(i % 3, i as u16), &[]);
        }
        assert_eq!(
            b.zones[&[0, 0]].log.len(),
            3,
            "3 cases changées : 3 entrées"
        );
        // Un joueur proche reçoit seulement la nouveauté ; un joueur revenu après s'être
        // éloigné reçoit tout ce qu'il a manqué (compacté).
        b.positions.insert(10, [0, 0]);
        let first = b.moved(10, [0, 0]);
        assert_eq!(first.iter().map(|(_, l)| l.len()).sum::<usize>(), 3);
        let live = b.record([0, 0], &set(5, 1), &[10]);
        assert_eq!(live, vec![(10, vec![set(5, 1)])]);
        b.record([0, 0], &set(6, 1), &[]);
        b.record([0, 0], &set(6, 2), &[]);
        let back = b.record([0, 0], &set(7, 1), &[10]);
        assert_eq!(
            back[0].1,
            vec![set(6, 2), set(7, 1)],
            "rien de perdu, rien en double"
        );
        // Le propriétaire (toutes les zones) rattrape tout, une fois.
        assert_eq!(
            b.catch_up(99, None)
                .iter()
                .map(|(_, l)| l.len())
                .sum::<usize>(),
            6
        );
        assert!(b.catch_up(99, None).is_empty());
        // Positions : tout aux voisins, une sur 4 un peu plus loin, une sur 20 au loin.
        b.positions.insert(20, [1, 0]);
        b.positions.insert(21, [5, 0]);
        b.positions.insert(22, [11, 0]);
        b.positions.insert(23, [30, 0]);
        let mut got: HashMap<u64, u32> = HashMap::new();
        for _ in 0..80 {
            for id in b.nearby(10, [0, 0], 1) {
                *got.entry(id).or_default() += 1;
            }
        }
        assert_eq!(got.get(&20), Some(&80));
        assert_eq!(got.get(&21), Some(&20));
        assert_eq!(got.get(&22), Some(&4));
        assert_eq!(got.get(&23), None, "trop loin");
        // Une foule au même endroit : chaque position ne part qu'à `FANOUT` voisins,
        // toujours les mêmes, et chacun est le voisin d'autant de joueurs.
        let mut crowd = Build::default();
        for id in 0..5000u64 {
            crowd.moved(id, [3, 3]);
        }
        // Les 32 plus proches à chaque fois, puis de plus en plus loin, de moins en
        // moins souvent : tout le monde à portée finit par voir chacun.
        let mut sizes = Vec::new();
        let mut first = Vec::new();
        for k in 0..64 {
            let a = crowd.nearby(100, [3, 3], 1);
            assert!(!a.contains(&100));
            if k == 0 {
                first = a.clone();
            }
            assert_eq!(a[..FANOUT_FULL], first[..FANOUT_FULL], "mêmes voisins");
            sizes.push(a.len());
        }
        let count = |n: usize| sizes.iter().filter(|&&s| s == n).count();
        assert_eq!(
            (count(FANOUT_FULL), count(128), count(512), count(FANOUT)),
            (48, 12, 3, 1)
        );
        let mut seen: HashMap<u64, usize> = HashMap::new();
        for id in 0..5000u64 {
            for _ in 0..64 {
                for m in crowd.nearby(id, [3, 3], 1) {
                    *seen.entry(m).or_default() += 1;
                }
            }
        }
        let each = 48 * FANOUT_FULL + 12 * 128 + 3 * 512 + FANOUT;
        assert!(seen.values().all(|&n| n == each), "chacun reçoit autant");
        assert_eq!(seen.len(), 5000);
        // Modifications dans une foule : poussées à `LIVE_MAX` joueurs, les autres les
        // reçoivent à leur prochaine position, compactées (la dernière valeur par case).
        let to = crowd.listeners([3, 3], u64::MAX);
        for v in 0..10u16 {
            let live = crowd.mirror_to([0, 0], &set(1, v), &to);
            assert_eq!(live.len(), LIVE_MAX);
        }
        let late = to[LIVE_MAX + 5];
        let got = crowd.catch_up(late, Some([3, 3]));
        assert_eq!(got, vec![([0, 0], vec![set(1, 9)])]);
        assert!(crowd.catch_up(late, Some([3, 3])).is_empty());
        // Une modification du monde arrive toujours à tous ceux à portée.
        assert_eq!(crowd.listeners([3, 3], 0).len(), 4999);
        crowd.left(7, &[]);
        assert_eq!(crowd.listeners([3, 3], 0).len(), 4998);
        // Le garant part : sa zone passe à un autre.
        let ch = b.left(1, &cands);
        assert!(ch.iter().any(|(id, g, _)| *id != 1 && g.contains(&[0, 0])));
    }
}
