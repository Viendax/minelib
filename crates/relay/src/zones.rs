//! Construction en ligne sans limite de joueurs : le monde est découpé en zones de
//! `ZONE × ZONE` chunks. Le relais du monde accepte seul chaque modification de bloc, lui
//! donne son rang (`Build::publish`) et garde, zone par zone, l'historique compacté de ce
//! qui a changé ; chaque joueur ne reçoit que ce qui se passe autour de lui, et rattrape
//! en s'approchant ce qu'il n'a pas encore reçu. Les positions vont aux voisins, moins
//! souvent au loin (`Build::nearby`).
//!
//! Jusqu'à la v126, chaque zone active avait un garant (le jeu d'un joueur) qui validait
//! les modifications et disait qui les avait faites. Le relais le fait lui-même depuis la
//! v127 : personne d'autre que lui n'écrit l'histoire d'un monde.

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use minelib_net::proto::GarantCaps;

/// Distance (en chunks) jusqu'à laquelle un joueur reçoit ce qui se passe.
pub const NEAR: i32 = 12;
/// Taille maximale gardée de l'historique d'une zone (pour les arrivants). L'historique est
/// compacté (la dernière modification de chaque case), cette limite ne sert qu'aux
/// messages qui ne sont pas des blocs.
const LOG_MAX: usize = 16 << 20;

/// Historique d'une zone : chaque modification a un numéro croissant ; une nouvelle
/// modification d'une case remplace l'ancienne (l'historique ne grossit qu'avec le nombre
/// de cases changées, pas avec le temps).
///
/// Deux sortes d'entrées : les blocs, et le reste (objets au sol, coffres). Un joueur servi
/// par un relais de région reçoit les blocs de ce relais et le reste du relais du monde :
/// ce qu'il a déjà reçu se compte donc à part pour chaque sorte.
#[derive(Default)]
pub struct ZoneLog {
    /// (est un bloc, message).
    entries: BTreeMap<u64, (bool, Vec<u8>)>,
    by_pos: HashMap<LogKey, u64>,
    bytes: usize,
    /// Dernier numéro de chaque sorte (blocs, reste).
    last: [u64; 2],
}

/// Ce qu'une entrée d'historique décrit (la suivante de même clé la remplace).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum LogKey {
    Block([i32; 3]),
    Item(u64),
    Slot([i32; 3], u8),
}

impl ZoneLog {
    /// Garde une entrée (la dernière de sa case, de son objet, de son emplacement).
    /// Renvoie si c'est un bloc, et si elle est gardée : plus ancienne que celle déjà là
    /// (archivée en retard, d'une époque souveraine finie), elle est sans effet.
    fn push(&mut self, seq: u64, data: &[u8]) -> (bool, bool) {
        use minelib_net::game::BuildMsg;
        // Une seule entrée par case, par objet au sol, par emplacement de coffre : la
        // dernière.
        let key = match minelib_net::game::decode::<BuildMsg>(data) {
            Some(BuildMsg::Set { pos, .. }) => Some(LogKey::Block(pos)),
            Some(BuildMsg::Item { id, .. }) => Some(LogKey::Item(id)),
            Some(BuildMsg::Slot { pos, slot, .. }) => Some(LogKey::Slot(pos, slot)),
            _ => None,
        };
        let block = matches!(key, Some(LogKey::Block(_)));
        if let Some(key) = key {
            if let Some(&old) = self.by_pos.get(&key) {
                if old > seq {
                    return (block, false);
                }
                if let Some((_, d)) = self.entries.remove(&old) {
                    self.bytes -= d.len();
                }
            }
            self.by_pos.insert(key, seq);
        }
        self.bytes += data.len();
        self.entries.insert(seq, (block, data.to_vec()));
        // (Une modification archivée en retard, d'une époque souveraine finie, peut avoir
        // un rang plus petit que la dernière : le dernier rang ne recule jamais.)
        let k = !block as usize;
        self.last[k] = self.last[k].max(seq);
        while self.bytes > LOG_MAX {
            let Some((&k, _)) = self.entries.iter().next() else {
                break;
            };
            let (_, d) = self.entries.remove(&k).expect("entrée");
            self.bytes -= d.len();
            self.by_pos.retain(|_, s| *s != k);
        }
        (block, true)
    }

    /// Entrées d'une sorte (`blocks` : les blocs, sinon le reste) après le numéro `after`.
    pub fn since(&self, after: u64, blocks: bool) -> Vec<Vec<u8>> {
        self.entries
            .range(after + 1..)
            .filter(|(_, (b, _))| *b == blocks)
            .map(|(_, (_, d))| d.clone())
            .collect()
    }

    /// Dernier bloc publié en `pos` (rien si cette case n'a jamais été changée).
    fn block_at(&self, pos: [i32; 3]) -> Option<u16> {
        let seq = self.by_pos.get(&LogKey::Block(pos))?;
        match minelib_net::game::decode(&self.entries.get(seq)?.1)? {
            minelib_net::game::BuildMsg::Set { block, .. } => Some(block),
            _ => None,
        }
    }

    /// Auteur de la dernière modification de la case `pos`.
    #[cfg(test)]
    fn author_at(&self, pos: [i32; 3]) -> Option<String> {
        let seq = self.by_pos.get(&LogKey::Block(pos))?;
        match minelib_net::game::decode(&self.entries.get(seq)?.1)? {
            minelib_net::game::BuildMsg::Set { by, .. } => Some(by),
            _ => None,
        }
    }

    /// Dernier numéro d'une sorte.
    pub fn last_of(&self, blocks: bool) -> u64 {
        self.last[!blocks as usize]
    }

    pub fn last(&self) -> u64 {
        self.entries.keys().next_back().copied().unwrap_or(0)
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

/// Un joueur pouvant garder le monde (propriétaire) ou héberger un relais de région.
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

/// Aptitude d'un joueur à tenir ce rôle, sans tenir compte de sa charge.
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

#[derive(Default)]
pub struct Build {
    /// Historique de chaque zone où quelque chose a changé.
    logs: HashMap<[i32; 2], ZoneLog>,
    pub positions: HashMap<u64, [i32; 2]>,
    /// Ce que chaque joueur a déjà reçu de chaque zone : dernier numéro des blocs, et du
    /// reste (objets, coffres).
    known: HashMap<u64, HashMap<[i32; 2], [u64; 2]>>,
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

/// Ce que des joueurs doivent recevoir : (joueur, messages).
pub type Sends = Vec<(u64, Vec<Vec<u8>>)>;

fn near(a: [i32; 2], b: [i32; 2]) -> bool {
    (a[0] - b[0]).abs() <= NEAR && (a[1] - b[1]).abs() <= NEAR
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
        // Les joueurs des autres relais de la région ne « partent » jamais d'ici : leurs
        // compteurs s'accumulaient sans fin. Quand il y en a bien plus que de joueurs
        // présents, on ne garde que ceux des présents (les autres repartent de zéro).
        if self.pos_count.len() > 4 * self.positions.len() + 4096 {
            let here = &self.positions;
            self.pos_count.retain(|id, _| here.contains_key(id));
        }
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

    /// Joueurs à qui envoyer ce qui se passe au chunk `chunk` (tous ceux à portée : une
    /// modification du monde doit arriver à chacun).
    pub fn listeners(&self, chunk: [i32; 2], except: u64) -> Vec<u64> {
        // Peu de chunks occupés (une foule au même endroit) : par les chunks occupés.
        // Beaucoup (un monde où les joueurs sont étalés) : par les chunks à portée, pour
        // que le travail ne grandisse pas avec le monde (essai de charge : à 60 000
        // joueurs, chaque modification coûtait 170 µs, presque tout ici).
        if self.indexed == self.positions.len() {
            let ring = (2 * NEAR + 1) as usize;
            if self.by_chunk.len() <= ring * ring {
                return self
                    .by_chunk
                    .iter()
                    .filter(|(&c, _)| near(c, chunk))
                    .flat_map(|(_, m)| m.iter().copied())
                    .filter(|&id| id != except)
                    .collect();
            }
            let mut out = Vec::new();
            for dx in -NEAR..=NEAR {
                for dz in -NEAR..=NEAR {
                    if let Some(m) = self.by_chunk.get(&[chunk[0] + dx, chunk[1] + dz]) {
                        out.extend(m.iter().copied().filter(|&id| id != except));
                    }
                }
            }
            return out;
        }
        self.positions
            .iter()
            .filter(|(&id, &p)| id != except && near(p, chunk))
            .map(|(&id, _)| id)
            .collect()
    }

    /// Garde un message (bloc publié, état d'un objet ou d'un coffre) dans l'historique de
    /// sa zone. Renvoie, pour chaque joueur de `listeners`, ce qu'il doit recevoir : le
    /// message seul s'il avait tout le reste, sinon tout ce qui lui manque de cette sorte
    /// (rien n'est jamais perdu, même s'il s'était éloigné un moment).
    pub fn mirror_to(&mut self, zone: [i32; 2], data: &[u8], listeners: &[u64]) -> Sends {
        self.mirror_at(zone, data, None, listeners)
    }

    /// Comme `mirror_to`, au rang `seq` s'il est donné (modification mise en ordre par
    /// l'hôte principal d'une région souveraine, voir `minelib_net::sovereign`).
    pub fn mirror_at(
        &mut self,
        zone: [i32; 2],
        data: &[u8],
        seq: Option<u64>,
        listeners: &[u64],
    ) -> Sends {
        let log = self.logs.entry(zone).or_default();
        let seq = match seq {
            Some(s) => {
                self.next_seq = self.next_seq.max(s);
                s
            }
            None => {
                self.next_seq = self.next_seq.max(log.last()) + 1;
                self.next_seq
            }
        };
        let before = log.last;
        let (blocks, kept) = log.push(seq, data);
        // (Plus ancienne que ce qui est gardé pour cette case : personne n'en a besoin.)
        if !kept {
            return Vec::new();
        }
        let (kind, before) = (!blocks as usize, before[!blocks as usize]);
        let mut out = Vec::new();
        // Dans une foule, la modification n'est poussée tout de suite qu'aux `LIVE_MAX`
        // premiers ; les autres la reçoivent à leur prochaine position (20 fois par
        // seconde), compactée : la dernière valeur de chaque case, pas chaque étape.
        for &id in listeners.iter().take(LIVE_MAX) {
            let known = &mut self.known.entry(id).or_default().entry(zone).or_default()[kind];
            if *known >= before {
                out.push((id, vec![data.to_vec()]));
            } else {
                out.push((id, log.since(*known, blocks)));
            }
            *known = (*known).max(seq);
        }
        out
    }

    /// Réserve `n` rangs à la suite (une époque souveraine : l'hôte principal de la région
    /// numérote lui-même dans cette plage). Renvoie le premier.
    pub fn reserve(&mut self, n: u64) -> u64 {
        let first = self.next_seq + 1;
        self.next_seq += n;
        first
    }

    /// Dernier bloc publié en `pos`, si cette case a déjà été modifiée (le relais ne
    /// connaît pas le terrain d'origine).
    pub fn block_at(&self, pos: [i32; 3]) -> Option<u16> {
        let zone = minelib_net::proto::zone_of([pos[0].div_euclid(16), pos[2].div_euclid(16)]);
        self.logs.get(&zone)?.block_at(pos)
    }

    /// Auteur de la dernière modification de la case `pos`, si elle a déjà été modifiée.
    #[cfg(test)]
    pub fn author_at(&self, pos: [i32; 3]) -> Option<String> {
        let zone = minelib_net::proto::zone_of([pos[0].div_euclid(16), pos[2].div_euclid(16)]);
        self.logs.get(&zone)?.author_at(pos)
    }

    /// Le relais du monde accepte une modification de bloc : elle prend son rang dans
    /// l'ordre du monde (`seq`, écrit dans le message : le jeu n'applique jamais une
    /// modification plus ancienne que celle qu'il a déjà pour la même case) et entre dans
    /// l'historique. Renvoie le message, et ce que chaque joueur de `listeners` doit
    /// recevoir (voir `mirror_to`).
    pub fn publish(
        &mut self,
        pos: [i32; 3],
        block: u16,
        by: &str,
        listeners: &[u64],
    ) -> (Vec<u8>, Sends) {
        let zone = minelib_net::proto::zone_of([pos[0].div_euclid(16), pos[2].div_euclid(16)]);
        let last = self.logs.get(&zone).map_or(0, ZoneLog::last);
        let data = minelib_net::game::encode(&minelib_net::game::BuildMsg::Set {
            pos,
            block,
            by: by.to_string(),
            // (Le numéro que `mirror_to` va donner.)
            seq: self.next_seq.max(last) + 1,
        });
        let out = self.mirror_to(zone, &data, listeners);
        (data, out)
    }

    /// Historique des blocs des zones choisies par `keep` (pour un relais de région : les
    /// objets et les coffres ne passent que par le relais du monde).
    pub fn logs_in(&self, keep: impl Fn([i32; 2]) -> bool) -> Vec<([i32; 2], Vec<Vec<u8>>)> {
        self.logs
            .iter()
            .filter(|(k, l)| keep(**k) && l.last_of(true) > 0)
            .map(|(k, l)| (*k, l.since(0, true)))
            .collect()
    }

    /// Ce qu'un joueur n'a pas encore reçu : des zones proches de `chunk`, ou de toutes les
    /// zones (`None` : le propriétaire, qui enregistre le monde). `blocks` dit, zone par
    /// zone, s'il faut aussi les blocs : pas quand un relais de région les sert déjà à ce
    /// joueur (ils restent dus, pour le jour où ce relais s'en va).
    pub fn catch_up(
        &mut self,
        id: u64,
        chunk: Option<[i32; 2]>,
        blocks: &dyn Fn([i32; 2]) -> bool,
    ) -> Vec<([i32; 2], Vec<Vec<u8>>)> {
        let known = self.known.entry(id).or_default();
        let mut out = Vec::new();
        let mut take = |zone: [i32; 2], log: &ZoneLog| {
            let k = known.entry(zone).or_default();
            for kind in [true, false] {
                let i = !kind as usize;
                if log.last_of(kind) > k[i] && (!kind || blocks(zone)) {
                    out.push((zone, log.since(k[i], kind)));
                    k[i] = log.last_of(kind);
                }
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
                        if let Some(l) = self.logs.get(&zone) {
                            take(zone, l);
                        }
                    }
                }
            }
            None => {
                for (&zone, l) in &self.logs {
                    take(zone, l);
                }
            }
        }
        out
    }

    /// Le joueur s'est déplacé : ce qui lui manque des zones proches (`blocks` : voir
    /// `catch_up`).
    pub fn moved(
        &mut self,
        id: u64,
        chunk: [i32; 2],
        blocks: &dyn Fn([i32; 2]) -> bool,
    ) -> Vec<([i32; 2], Vec<Vec<u8>>)> {
        self.place(id, Some(chunk));
        self.catch_up(id, Some(chunk), blocks)
    }

    /// Historique de toutes les zones (nouveau propriétaire du monde), noté comme reçu.
    pub fn all_logs(&mut self, id: u64) -> Vec<([i32; 2], Vec<Vec<u8>>)> {
        self.known.remove(&id);
        self.catch_up(id, None, &|_| true)
    }

    /// Un joueur part.
    pub fn left(&mut self, id: u64) {
        self.place(id, None);
        self.known.remove(&id);
        self.pos_count.remove(&id);
        self.pos_tokens.remove(&id);
    }
}

#[cfg(test)]
mod tests {
    /// Une modification d'une époque souveraine finie, archivée en retard (passation) avec
    /// un rang plus petit qu'une plus récente de la même case : l'historique garde la plus
    /// récente. Celle d'une autre case, archivée en retard aussi, est gardée et remise
    /// comme un bloc.
    #[test]
    fn a_late_archived_edit_never_replaces_a_newer_one() {
        use minelib_net::game::{self, BuildMsg};
        let set = |pos: [i32; 3], block: u16, seq: u64| {
            game::encode(&BuildMsg::Set {
                pos,
                block,
                by: "Eve".into(),
                seq,
            })
        };
        let mut b = super::Build::default();
        let (a, other) = ([1, 70, 1], [2, 70, 2]);
        let zone = minelib_net::proto::zone_of([0, 0]);
        b.mirror_at(zone, &set(a, 5, 100), Some(100), &[7]);
        let late = b.mirror_at(zone, &set(a, 9, 50), Some(50), &[7]);
        assert!(late.is_empty(), "plus ancienne : rien à remettre");
        assert_eq!(b.block_at(a), Some(5));
        let other_late = b.mirror_at(zone, &set(other, 3, 60), Some(60), &[8]);
        assert_eq!(other_late.len(), 1);
        assert_eq!(b.block_at(other), Some(3));
        // Un joueur qui arrive reçoit les deux cases à leur dernière valeur.
        let logs = b.all_logs(9);
        let mut cells: Vec<([i32; 3], u16)> = logs
            .iter()
            .flat_map(|(_, l)| l.iter())
            .filter_map(|d| match game::decode::<BuildMsg>(d) {
                Some(BuildMsg::Set { pos, block, .. }) => Some((pos, block)),
                _ => None,
            })
            .collect();
        cells.sort();
        assert_eq!(cells, vec![(a, 5), (other, 3)]);
    }

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
                        signer: [0; 32],
                        epoch: 0,
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
                    builds[s].moved(id, c, &|_| true);
                }
                let mut rate = vec![20.0f32; n];
                // Joueurs différents dont chacun reçoit des positions (pour le pseudo, redit
                // toutes les 5 s sur chaque connexion).
                let mut distinct = vec![0usize; n];
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
                            distinct[i] = seen[i].len();
                        }
                        continue;
                    }
                    // Depuis la v129, les positions vont aux joueurs regroupées (voir
                    // `minelib_net::poses`) : 50 paquets par seconde au plus, chacun avec
                    // ses en-têtes (y compris IP et TCP, 40 octets), mesurés avec le vrai
                    // codec. Les copies vers les autres relais de la région partent encore
                    // une par une.
                    let mut cache = std::collections::HashMap::new();
                    let mut packet = |k: usize| -> f64 {
                        *cache.entry(k).or_insert_with(|| {
                            let mut seed = 77u64 + k as u64;
                            let mut r = move || {
                                seed ^= seed << 13;
                                seed ^= seed >> 7;
                                seed ^= seed << 17;
                                seed
                            };
                            let span = (side * 16) as u64;
                            let mut entries: Vec<(u64, Option<&str>, minelib_net::poses::Pose)> =
                                (0..k)
                                    .map(|i| {
                                        let pose = minelib_net::poses::Pose {
                                            pos: [
                                                (r() % span) as f32 + 0.37,
                                                64.0 + (r() % 4) as f32,
                                                (r() % span) as f32 + 0.81,
                                            ],
                                            yaw: (r() % 628) as f32 / 100.0,
                                            pitch: 0.1,
                                            crouch: 0.0,
                                            speed: 4.3,
                                        };
                                        ((i as u64) * (n / k.max(1)).max(1) as u64 + 1, None, pose)
                                    })
                                    .collect();
                            let data = minelib_net::poses::pack_poses(&mut entries);
                            let msg = postcard::to_allocvec(&Response::FromZone {
                                peer: 0,
                                chunk: [0, 0],
                                data: game::encode(&BuildMsg::Poses(data)),
                            })
                            .unwrap()
                            .len();
                            (msg + 16 + 2 + 40) as f64
                        })
                    };
                    let mut grouped = |got: f64, distinct: usize| -> f64 {
                        if got <= 0.0 {
                            return 0.0;
                        }
                        let packets = got.min(50.0);
                        let k = (got / packets).round().max(1.0) as usize;
                        packets * packet(k) + distinct as f64 * 11.0 / 5.0
                    };
                    let mut relay_bytes = vec![0f64; relays];
                    let mut delivered = vec![0u64; relays];
                    let mut player_bytes = 0f64;
                    for (i, &(_, _, s)) in players.iter().enumerate() {
                        let b = grouped(got[i] as f64, distinct[i]);
                        relay_bytes[s] += b;
                        delivered[s] += got[i];
                        player_bytes += b;
                    }
                    for r in 0..relays {
                        // Le reste de ce qu'envoie ce relais : les copies aux autres relais.
                        relay_bytes[r] += (out[r] - delivered[r]) as f64 * wire as f64;
                    }
                    let worst_bytes = relay_bytes.iter().cloned().fold(0.0, f64::max);
                    let sent: f32 = rate.iter().sum();
                    let worst_out = *out.iter().max().unwrap();
                    let worst_got = *got.iter().max().unwrap();
                    let mean_got = got.iter().sum::<u64>() / n as u64;
                    report = format!(
                        "{n:>5} joueurs, {relays} relais | envoi moyen {:.0}/s par joueur | relais le plus chargé : {:.1} Mbit/s ({} msg/s, {:.1} Mbit/s avant les paquets), {} entrants | joueur : {} reçus/s en moyenne = {:.0} kbit/s ({:.0} avant), {} au pire | calcul de la diffusion : {:.2} s de processeur par seconde",
                        sent / n as f32,
                        worst_bytes * 8.0 / 1e6,
                        worst_out,
                        worst_out as f32 * wire as f32 * 8.0 / 1e6,
                        inn.iter().max().unwrap(),
                        mean_got,
                        player_bytes * 8.0 / 1e3 / n as f64,
                        mean_got as f32 * wire as f32 * 8.0 / 1e3,
                        worst_got,
                        cpu,
                    );
                }
                eprintln!("FOULE {layout:<24} {report}");
                // Construction : chaque joueur pose ou casse un bloc toutes les deux
                // secondes, là où il est ; chaque modification va à tous les joueurs à
                // portée (le monde doit rester le même pour tous). Les blocs partent
                // regroupés (`minelib_net::cells`) : 50 paquets par seconde au plus vers
                // chaque joueur, le premier bloc sans attendre.
                let mut out = vec![0f64; relays];
                let mut got = 0f64;
                for &(_, c, _) in &players {
                    for (r, b) in builds.iter().enumerate() {
                        let l = b.listeners(c, u64::MAX).len() as f64 * 0.5;
                        out[r] += l;
                        got += l;
                    }
                }
                // Octets par seconde vers une connexion qui reçoit `rate` blocs par
                // seconde, d'après le vrai codec (blocs pris au hasard à portée de vue) ;
                // un message par bloc avant la v128.
                let mut seed = 4242u64;
                let mut rnd = move || {
                    seed ^= seed << 13;
                    seed ^= seed >> 7;
                    seed ^= seed << 17;
                    seed
                };
                let mut bytes_at = |rate: f64| -> f64 {
                    if rate <= 0.0 {
                        return 0.0;
                    }
                    let packets = rate.min(50.0);
                    let per = (rate / packets).round().max(1.0) as usize;
                    let cells = (0..per)
                        .map(|i| {
                            let x = 8000 + (rnd() % 400) as i32;
                            let z = -5000 + (rnd() % 400) as i32;
                            (
                                [x, 50 + (rnd() % 60) as i32, z],
                                (rnd() % 300) as u16,
                                5_000_000 + i as u64,
                            )
                        })
                        .collect();
                    let packet = postcard::to_allocvec(&Response::FromZone {
                        peer: 0,
                        chunk: [0, 0],
                        data: game::encode(&BuildMsg::Cells(
                            minelib_net::cells::pack_cells(cells).remove(0),
                        )),
                    })
                    .unwrap()
                    .len()
                        + 16
                        + 2;
                    packets * packet as f64
                };
                let old = postcard::to_allocvec(&Response::FromZone {
                    peer: 0,
                    chunk: [17, -3],
                    data: game::encode(&BuildMsg::Set {
                        pos: [123, 64, -321],
                        block: 37,
                        by: "Joueur1234".into(),
                        seq: 123_456,
                    }),
                })
                .unwrap()
                .len()
                    + 16
                    + 2
                    + 2;
                let worst = out.iter().cloned().fold(0.0, f64::max);
                // Un relais sert ses joueurs (`share`), chacun au débit moyen d'un joueur.
                let per_player = got / n as f64;
                let share = n as f64 / relays as f64;
                let relay_bytes = share * bytes_at(worst / share);
                // Le relais du monde accepte chaque modification (une demande reçue), la
                // confirme à son auteur (un bloc seul) et la remet à chaque relais de la
                // région (regroupée).
                let asks = n as f64 * 0.5;
                let world = asks * bytes_at(1.0) + relays as f64 * bytes_at(asks);
                eprintln!(
                    "FOULE {layout:<24} {n:>5} joueurs, construction (1 bloc / 2 s chacun) | relais le plus chargé : {:.0} blocs/s = {:.1} Mbit/s ({:.1} avant les paquets) | joueur : {:.0} blocs reçus/s = {:.0} kbit/s ({:.0} avant) | relais du monde : {:.0} demandes/s, {:.2} Mbit/s sortants ({:.1} avant)",
                    worst,
                    relay_bytes * 8.0 / 1e6,
                    worst * old as f64 * 8.0 / 1e6,
                    per_player,
                    bytes_at(per_player) * 8.0 / 1e3,
                    per_player * old as f64 * 8.0 / 1e3,
                    asks,
                    world * 8.0 / 1e6,
                    asks * (relays as f64 + 1.0) * old as f64 * 8.0 / 1e6,
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
    fn history_is_compacted_and_everyone_gets_exactly_what_is_missing() {
        // Signalé souvent : en dernier.
        let mut bad = cand(4, 32);
        bad.ratio = 2.0;
        assert!(aptitude(&bad) < aptitude(&cand(3, 2)));
        let mut b = Build::default();
        // Le relais donne à chaque modification son rang, écrit dans le message.
        let seq = |data: &[u8]| match minelib_net::game::decode(data) {
            Some(minelib_net::game::BuildMsg::Set { seq, by, .. }) => (seq, by),
            m => panic!("{m:?}"),
        };
        let mut last = 0;
        for i in 0..100 {
            let (data, _) = b.publish([i % 3, 64, 0], i as u16, "Eve", &[]);
            let (n, by) = seq(&data);
            assert!(n > last, "rangs croissants");
            assert_eq!(by, "Eve");
            last = n;
        }
        // Historique compacté : la dernière modification de chaque case.
        assert_eq!(b.logs[&[0, 0]].len(), 3, "3 cases changées : 3 entrées");
        assert_eq!(b.block_at([1, 64, 0]), Some(97), "la dernière : 97");
        assert_eq!(b.block_at([9, 64, 0]), None, "jamais changée : inconnue");
        // Un joueur proche reçoit seulement la nouveauté ; un joueur revenu après s'être
        // éloigné reçoit tout ce qu'il a manqué (compacté).
        b.positions.insert(10, [0, 0]);
        let first = b.moved(10, [0, 0], &|_| true);
        assert_eq!(first.iter().map(|(_, l)| l.len()).sum::<usize>(), 3);
        let (five, live) = b.publish([5, 64, 0], 1, "Eve", &[10]);
        assert_eq!(live, vec![(10, vec![five])]);
        b.publish([6, 64, 0], 1, "Eve", &[]);
        let (six, _) = b.publish([6, 64, 0], 2, "Eve", &[]);
        let (seven, back) = b.publish([7, 64, 0], 1, "Eve", &[10]);
        assert_eq!(back[0].1, vec![six, seven], "rien de perdu, rien en double");
        // Le propriétaire (toutes les zones) rattrape tout, une fois.
        assert_eq!(
            b.catch_up(99, None, &|_| true)
                .iter()
                .map(|(_, l)| l.len())
                .sum::<usize>(),
            6
        );
        assert!(b.catch_up(99, None, &|_| true).is_empty());
    }

    /// Un joueur servi par un relais de région ne reçoit pas les blocs une deuxième fois
    /// du relais du monde, mais toujours les objets et les coffres (seul le relais du
    /// monde les arbitre) ; les blocs lui restent dus : le relais de région parti, il
    /// reçoit tout ce qui a changé depuis.
    #[test]
    fn blocks_served_by_a_region_relay_are_not_sent_twice_but_stay_owed() {
        use minelib_net::game::{encode, BuildMsg};
        let mut b = Build::default();
        b.positions.insert(10, [0, 0]);
        let item = |id: u64| {
            encode(&BuildMsg::Item {
                id,
                at: [0.5, 64.0, 0.5],
                drop: None,
                to: String::new(),
                seq: 0,
                got: None,
            })
        };
        let (a, _) = b.publish([1, 64, 1], 5, "Eve", &[]);
        b.mirror_to([0, 0], &item(1), &[]);
        let (c, _) = b.publish([2, 64, 1], 6, "Eve", &[]);
        // Servi par un relais de région : seulement l'objet.
        let got = b.catch_up(10, Some([0, 0]), &|_| false);
        assert_eq!(got, vec![([0, 0], vec![item(1)])]);
        assert!(b.catch_up(10, Some([0, 0]), &|_| false).is_empty());
        // En direct aussi, chaque sorte se compte à part : un nouvel objet seul.
        let live = b.mirror_to([0, 0], &item(2), &[10]);
        assert_eq!(live, vec![(10, vec![item(2)])]);
        // Le relais de région s'en va : les deux blocs, dans l'ordre.
        let got = b.catch_up(10, Some([0, 0]), &|_| true);
        assert_eq!(got, vec![([0, 0], vec![a, c])]);
        // L'historique remis à un relais de région ne contient que les blocs.
        assert_eq!(b.logs_in(|_| true)[0].1.len(), 2);
    }

    #[test]
    fn positions_go_to_neighbours_and_less_often_far_away() {
        let mut b = Build::default();
        b.positions.insert(10, [0, 0]);
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
            crowd.moved(id, [3, 3], &|_| true);
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
        let mut last = Vec::new();
        for v in 0..10u16 {
            let (data, live) = crowd.publish([1, 64, 0], v, "Eve", &to);
            assert_eq!(live.len(), LIVE_MAX);
            last = data;
        }
        let late = to[LIVE_MAX + 5];
        let got = crowd.catch_up(late, Some([3, 3]), &|_| true);
        assert_eq!(got, vec![([0, 0], vec![last])]);
        assert!(crowd.catch_up(late, Some([3, 3]), &|_| true).is_empty());
        // Une modification du monde arrive toujours à tous ceux à portée.
        assert_eq!(crowd.listeners([3, 3], 0).len(), 4999);
        crowd.left(7);
        assert_eq!(crowd.listeners([3, 3], 0).len(), 4998);
    }
}
