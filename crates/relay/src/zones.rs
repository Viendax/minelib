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
use std::path::PathBuf;
use std::time::{Duration, Instant};

use minelib_net::proto::GarantCaps;

/// Distance (en chunks) jusqu'à laquelle un joueur reçoit ce qui se passe.
pub const NEAR: i32 = 12;
/// Taille maximale gardée de l'historique d'une zone (pour les arrivants). L'historique est
/// compacté (la dernière modification de chaque case), cette limite ne sert qu'aux
/// messages qui ne sont pas des blocs.
const LOG_MAX: usize = 16 << 20;
/// Une zone dont l'historique n'a servi à rien pendant ce temps s'endort sur le disque du
/// relais du monde (`Build::sleep_cold`) ; elle se réveille dès qu'on en a besoin.
pub const COLD_AFTER: Duration = Duration::from_secs(600);
/// Les zones froides sont cherchées au plus une fois par ce temps, et endormies au plus
/// pendant `SLEEP_BUDGET` à chaque fois (le relais ne traite rien d'autre pendant).
const SLEEP_EVERY: Duration = Duration::from_secs(30);
const SLEEP_BUDGET: Duration = Duration::from_millis(20);
/// Entrée d'historique d'une zone : (zone, rang, message).
pub type Entry = ([i32; 2], u64, Vec<u8>);

/// Format d'une zone endormie.
const PAGE_VERSION: u8 = 1;
/// Zones endormies réveillées au plus pour un seul rattrapage de tout le monde.
const WAKE_MAX: usize = 256;

/// Historique d'une zone : chaque modification a un numéro croissant ; une nouvelle
/// modification d'une case remplace l'ancienne (l'historique ne grossit qu'avec le nombre
/// de cases changées, pas avec le temps).
///
/// Deux sortes d'entrées : les blocs, et le reste (objets au sol, coffres, et depuis la v224
/// les lumières de l'éditeur). Un joueur servi
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
    /// (v224) Lumière de l'éditeur : la dernière de chaque lumière dans cette zone.
    Light(u64),
}

/// Ce que décrit une entrée (la suivante de même clé la remplace).
fn key_of(data: &[u8]) -> Option<LogKey> {
    use minelib_net::game::BuildMsg;
    match minelib_net::game::decode::<BuildMsg>(data) {
        Some(BuildMsg::Set { pos, .. }) => Some(LogKey::Block(pos)),
        Some(BuildMsg::Item { id, .. }) => Some(LogKey::Item(id)),
        Some(BuildMsg::Slot { pos, slot, .. }) => Some(LogKey::Slot(pos, slot)),
        Some(BuildMsg::Light { uid, .. }) => Some(LogKey::Light(uid)),
        _ => None,
    }
}

impl ZoneLog {
    /// Pour le disque : entrées et derniers numéros.
    fn to_bytes(&self) -> Option<Vec<u8>> {
        let entries: Vec<(u64, bool, &[u8])> = self
            .entries
            .iter()
            .map(|(s, (b, d))| (*s, *b, d.as_slice()))
            .collect();
        postcard::to_allocvec(&(PAGE_VERSION, entries, self.last)).ok()
    }

    /// Relue du disque (`None` si le fichier est abîmé).
    fn from_bytes(b: &[u8]) -> Option<Self> {
        type Page = (u8, Vec<(u64, bool, Vec<u8>)>, [u64; 2]);
        let (version, entries, last): Page = postcard::from_bytes(b).ok()?;
        if version != PAGE_VERSION {
            return None;
        }
        let mut log = ZoneLog::default();
        for (seq, block, data) in entries {
            if let Some(k) = key_of(&data) {
                log.by_pos.insert(k, seq);
            }
            log.bytes += data.len();
            log.entries.insert(seq, (block, data));
        }
        log.last = last;
        Some(log)
    }

    /// Garde une entrée (la dernière de sa case, de son objet, de son emplacement).
    /// Renvoie si c'est un bloc, et si elle est gardée : plus ancienne que celle déjà là
    /// (archivée en retard, d'une époque souveraine finie), elle est sans effet.
    fn push(&mut self, seq: u64, data: &[u8]) -> (bool, bool) {
        // Une seule entrée par case, par objet au sol, par emplacement de coffre : la
        // dernière.
        let key = key_of(data);
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
    wide_tokens: HashMap<u64, (Instant, f32)>,
    /// Joueurs par chunk (même contenu que `positions`, rangé pour trouver les voisins
    /// sans parcourir tout le monde) ; `indexed` : nombre de joueurs rangés.
    by_chunk: HashMap<[i32; 2], Vec<u64>>,
    indexed: usize,
    /// Les mêmes par zone (4 × 4 chunks), avec leur chunk : pour trouver qui doit recevoir
    /// une modification, 49 zones à regarder au lieu de 625 chunks.
    by_zone: HashMap<[i32; 2], Vec<(u64, [i32; 2])>>,
    /// Nombre de joueurs par région (v140 : compté à chaque déplacement, plus en parcourant
    /// tous les joueurs à chaque entretien).
    by_region: HashMap<[i32; 2], u32>,
    /// Objets au sol et coffres du monde : ce relais en est l'arbitre.
    pub items: minelib_net::items::ItemStore,
    /// Zones endormies sur le disque (voir `sleep_cold`) : dossier (aucun : elles restent
    /// toutes en mémoire), zones qui y dorment (avec leurs derniers numéros, pour savoir
    /// sans les réveiller si un joueur en a besoin), dernière fois que chaque zone a servi.
    page_dir: Option<PathBuf>,
    asleep: HashMap<[i32; 2], [u64; 2]>,
    touched: HashMap<[i32; 2], Instant>,
    last_sleep: Option<Instant>,
    /// Monde qui a un relais de secours : chaque nouvelle entrée lui part aussi (voir
    /// `take_replica`).
    replica: Option<Vec<Entry>>,
}

/// Le monde fermé, ses zones endormies partent avec lui.
impl Drop for Build {
    fn drop(&mut self) {
        if let Some(dir) = &self.page_dir {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
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

/// Positions par seconde acceptées d'un joueur : au-delà, elles ne sont pas diffusées (jeu
/// modifié). De quoi absorber une rafale (réseau qui regroupe les envois).
///
/// (v275) Deux limites. Le jeu en envoie de nouveau jusqu'à 256, mais seulement quand il a
/// 3 voisins ou moins (`pos_every` dans crates/app/src/buildnet.rs) : `POS_MAX_HZ` vaut
/// pour une position qui part à `POS_FEW` joueurs au plus. Dès qu'elle part à davantage
/// (ou que la région a plusieurs relais : c'est une foule), c'est la limite d'avant,
/// `POS_WIDE_HZ` : un jeu modifié ne fait pas travailler le relais plus qu'avant.
pub const POS_MAX_HZ: f32 = 280.0;
const POS_BURST: f32 = 32.0;
pub const POS_WIDE_HZ: f32 = 66.0;
const POS_WIDE_BURST: f32 = 8.0;
pub const POS_FEW: usize = 4;
/// (v285) Voisins qui reçoivent les positions rapprochées d'un joueur (`closest`).
pub const POS_CLOSE_TO: usize = 3;

/// Une modification du monde est poussée tout de suite à ce nombre de joueurs au plus ;
/// au-delà (foule), chacun la reçoit avec sa prochaine position, compactée. Le travail par
/// modification ne dépend plus de la taille de la foule.
pub const LIVE_MAX: usize = 256;

/// Ce que des joueurs doivent recevoir : (joueur, messages).
pub type Sends = Vec<(u64, Vec<Vec<u8>>)>;

/// (v318) Sur la carte bouclée : de part et d'autre de sa limite, deux chunks voisins sont
/// proches.
pub fn near(a: [i32; 2], b: [i32; 2]) -> bool {
    minelib_net::proto::chunks_within(a, b, NEAR)
}

/// (v318) Le chunk à (`dx`, `dz`) de `c`, sur la carte bouclée.
fn step(c: [i32; 2], dx: i32, dz: i32) -> [i32; 2] {
    minelib_net::proto::wrap_chunks([c[0] as i64 + dx as i64, c[1] as i64 + dz as i64])
}

impl Build {
    /// Les zones froides de ce monde dormiront dans `dir` (voir `sleep_cold`).
    pub fn set_page_dir(&mut self, dir: PathBuf) {
        let _ = std::fs::remove_dir_all(&dir);
        if std::fs::create_dir_all(&dir).is_ok() {
            self.page_dir = Some(dir);
        }
    }

    fn page_path(&self, zone: [i32; 2]) -> Option<PathBuf> {
        Some(
            self.page_dir
                .as_ref()?
                .join(format!("{}_{}.zone", zone[0], zone[1])),
        )
    }

    /// Réveille une zone si elle dort sur le disque, et note qu'elle sert. À faire avant
    /// de lire ou d'écrire son historique.
    fn wake(&mut self, zone: [i32; 2]) {
        if self.page_dir.is_none() {
            return;
        }
        if self.asleep.remove(&zone).is_some() {
            if let Some(p) = self.page_path(zone) {
                match std::fs::read(&p).ok().and_then(|b| ZoneLog::from_bytes(&b)) {
                    Some(log) => {
                        self.logs.insert(zone, log);
                    }
                    None => log::warn!("zone {zone:?} : historique illisible sur le disque"),
                }
                let _ = std::fs::remove_file(&p);
            }
        }
        self.touched.insert(zone, Instant::now());
    }

    /// Endort sur le disque l'historique des zones qui n'ont pas servi depuis `idle` (au
    /// plus une fois toutes les 30 s, et 20 ms de travail au plus) : la mémoire du relais
    /// du monde ne grandit qu'avec les zones où il se passe quelque chose, pas avec tout
    /// ce qui a été construit. Renvoie combien de zones se sont endormies.
    pub fn sleep_cold(&mut self, idle: Duration) -> usize {
        let Some(dir) = self.page_dir.clone() else {
            return 0;
        };
        if self.last_sleep.is_some_and(|t| t.elapsed() < SLEEP_EVERY) {
            return 0;
        }
        let start = Instant::now();
        self.last_sleep = Some(start);
        let cold: Vec<[i32; 2]> = self
            .logs
            .keys()
            .filter(|z| self.touched.get(*z).is_none_or(|t| t.elapsed() >= idle))
            .copied()
            .collect();
        let mut n = 0;
        for zone in cold {
            if start.elapsed() > SLEEP_BUDGET {
                break;
            }
            let Some(log) = self.logs.get(&zone) else {
                continue;
            };
            let (Some(bytes), last) = (log.to_bytes(), log.last) else {
                continue;
            };
            let path = dir.join(format!("{}_{}.zone", zone[0], zone[1]));
            if std::fs::write(&path, bytes).is_ok() {
                self.logs.remove(&zone);
                self.touched.remove(&zone);
                self.asleep.insert(zone, last);
                n += 1;
            }
        }
        n
    }

    /// Endort tout ce qui dort pas encore (essais : sans attendre ni compter le temps).
    #[cfg(test)]
    pub fn sleep_all(&mut self) -> usize {
        let mut total = 0;
        loop {
            self.last_sleep = None;
            let n = self.sleep_cold(Duration::ZERO);
            total += n;
            if n == 0 {
                return total;
            }
        }
    }

    /// Tout l'historique, pour un relais de secours qui commence : (zone, rang, entrée),
    /// les zones qui dorment lues sur le disque sans les réveiller.
    pub fn export_entries(&self) -> Vec<Entry> {
        let mut out = Vec::new();
        for (&zone, log) in &self.logs {
            for (&seq, (_, d)) in &log.entries {
                out.push((zone, seq, d.clone()));
            }
        }
        for &zone in self.asleep.keys() {
            let log = self
                .page_path(zone)
                .and_then(|p| std::fs::read(p).ok())
                .and_then(|b| ZoneLog::from_bytes(&b));
            for (seq, (_, d)) in log.map(|l| l.entries).unwrap_or_default() {
                out.push((zone, seq, d));
            }
        }
        out
    }

    /// Relais du monde (v139) : tout ce qui a été publié dans les zones gardées après le rang
    /// `after` (blocs, objets au sol et coffres), dans l'ordre de chaque zone.
    pub fn entries_after(&mut self, keep: impl Fn([i32; 2]) -> bool, after: u64) -> Vec<Entry> {
        let sleeping: Vec<[i32; 2]> = self.asleep.keys().filter(|z| keep(**z)).copied().collect();
        for z in sleeping {
            self.wake(z);
        }
        let mut out = Vec::new();
        for (&zone, log) in self.logs.iter().filter(|(z, _)| keep(**z)) {
            for (&seq, (_, d)) in log.entries.range(after + 1..) {
                out.push((zone, seq, d.clone()));
            }
        }
        out
    }

    /// Dernier rang donné (les suivants viendront après).
    pub fn last_seq(&self) -> u64 {
        self.next_seq
    }

    /// Relais de province : il numérote à partir de `base` (plage réservée par le relais du
    /// monde).
    pub fn start_at(&mut self, base: u64) {
        self.next_seq = self.next_seq.max(base.saturating_sub(1));
    }

    /// Avec un relais de secours (ou sans) : chaque nouvelle entrée lui est gardée.
    pub fn set_replicating(&mut self, on: bool) {
        self.replica = on.then(Vec::new);
    }

    /// Nouvelles entrées pour le relais de secours.
    pub fn take_replica(&mut self) -> Vec<Entry> {
        self.replica
            .as_mut()
            .map(std::mem::take)
            .unwrap_or_default()
    }

    /// Zones en mémoire, et endormies sur le disque.
    #[cfg(test)]
    pub fn zones_awake_asleep(&self) -> (usize, usize) {
        (self.logs.len(), self.asleep.len())
    }

    /// Range les joueurs par chunk si ce n'est pas fait (positions changées en direct).
    fn reindex(&mut self) {
        if self.indexed == self.positions.len() {
            return;
        }
        self.by_chunk.clear();
        self.by_zone.clear();
        self.by_region.clear();
        let mut ids: Vec<(u64, [i32; 2])> = self.positions.iter().map(|(&i, &c)| (i, c)).collect();
        ids.sort_unstable();
        for (id, c) in ids {
            *self
                .by_region
                .entry(minelib_net::proto::region_of(c))
                .or_default() += 1;
            self.by_chunk.entry(c).or_default().push(id);
            self.by_zone
                .entry(minelib_net::proto::zone_of(c))
                .or_default()
                .push((id, c));
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
            let z = minelib_net::proto::zone_of(o);
            if let Some(v) = self.by_zone.get_mut(&z) {
                v.retain(|&(m, _)| m != id);
                if v.is_empty() {
                    self.by_zone.remove(&z);
                }
            }
            let r = minelib_net::proto::region_of(o);
            if let Some(n) = self.by_region.get_mut(&r) {
                *n = n.saturating_sub(1);
                if *n == 0 {
                    self.by_region.remove(&r);
                }
            }
        }
        if let Some(c) = chunk {
            *self
                .by_region
                .entry(minelib_net::proto::region_of(c))
                .or_default() += 1;
            self.by_chunk.entry(c).or_default().push(id);
            self.by_zone
                .entry(minelib_net::proto::zone_of(c))
                .or_default()
                .push((id, c));
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

    /// (v275) Cette position, qui partirait à `to` joueurs, peut-elle partir ? À
    /// `POS_FEW` au plus, oui (`pos_due` l'a déjà comptée) ; à davantage, `POS_WIDE_HZ`
    /// par seconde, lissé.
    pub fn pos_fits(&mut self, id: u64, to: usize) -> bool {
        self.pos_fits_at(id, to, Instant::now())
    }

    fn pos_fits_at(&mut self, id: u64, to: usize, now: Instant) -> bool {
        if to <= POS_FEW {
            return true;
        }
        let t = self.wide_tokens.entry(id).or_insert((now, POS_WIDE_BURST));
        let gained = now.saturating_duration_since(t.0).as_secs_f32() * POS_WIDE_HZ;
        *t = (now, (t.1 + gained).min(POS_WIDE_BURST));
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
                    let Some(members) = self.by_chunk.get(&step(chunk, dx, dz)) else {
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

    /// (v285) Les `POS_CLOSE_TO` voisins les plus proches du joueur `id`, parmi ceux que
    /// `local` retient (ceux que ce relais sert) : d'abord ceux qui le suivent dans son
    /// chunk, puis les chunks autour (2 au plus). Comme pour `nearby`, ce sont toujours les
    /// mêmes, et chacun est le voisin proche d'autant de joueurs : dans une foule, personne
    /// ne reçoit les positions rapprochées de plus de `POS_CLOSE_TO` joueurs par chunk.
    pub fn closest(&mut self, id: u64, chunk: [i32; 2], local: impl Fn(u64) -> bool) -> Vec<u64> {
        self.reindex();
        let mut out = Vec::new();
        for d in 0..=2i32 {
            for dx in -d..=d {
                for dz in -d..=d {
                    if dx.abs().max(dz.abs()) != d {
                        continue;
                    }
                    let Some(members) = self.by_chunk.get(&step(chunk, dx, dz)) else {
                        continue;
                    };
                    if members.is_empty() {
                        continue;
                    }
                    let start = members
                        .iter()
                        .position(|&m| m == id)
                        .map_or(id as usize % members.len(), |i| i + 1);
                    out.extend(
                        (0..members.len())
                            .map(|k| members[(start + k) % members.len()])
                            .filter(|&m| m != id && local(m))
                            .take(POS_CLOSE_TO - out.len()),
                    );
                    if out.len() >= POS_CLOSE_TO {
                        return out;
                    }
                }
            }
        }
        out
    }

    /// Chunks où se trouvent au moins deux joueurs (pour la fréquentation).
    /// Joueurs de chaque région occupée.
    pub fn region_counts(&mut self) -> &HashMap<[i32; 2], u32> {
        self.reindex();
        &self.by_region
    }

    /// Chunks où se trouvent au moins deux joueurs, avec eux.
    pub fn crowded(&self) -> impl Iterator<Item = ([i32; 2], &Vec<u64>)> {
        self.by_chunk
            .iter()
            .filter(|(_, m)| m.len() >= 2)
            .map(|(c, m)| (*c, m))
    }

    /// (v227) Joueurs à `radius` chunks au plus du chunk `chunk` (dans chaque direction),
    /// sauf `except` : ceux qui voient la foule d'essai d'un OP.
    pub fn within(&mut self, chunk: [i32; 2], radius: i32, except: u64) -> Vec<u64> {
        self.reindex();
        let mut out = Vec::new();
        // (v318) Les zones autour, sur la carte bouclée.
        for w in minelib_net::proto::zones_around(chunk, radius) {
            let Some(v) = self.by_zone.get(&w) else {
                continue;
            };
            out.extend(v.iter().filter_map(|&(m, c)| {
                let near = minelib_net::proto::chunks_within(c, chunk, radius);
                (near && m != except).then_some(m)
            }));
        }
        out
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
            // Par zones (49 au lieu de 625 chunks : essai de charge souverain, v136). (v318)
            // Sur la carte bouclée.
            let mut out = Vec::new();
            for w in minelib_net::proto::zones_around(chunk, NEAR) {
                if let Some(m) = self.by_zone.get(&w) {
                    out.extend(
                        m.iter()
                            .filter(|&&(id, c)| id != except && near(c, chunk))
                            .map(|&(id, _)| id),
                    );
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

    /// Joueurs placés dans les chunks de `lo` à `hi` (inclus) : par zones (une province
    /// n'est pas cherchée parmi tous les joueurs du monde).
    pub fn players_in(&self, lo: [i32; 2], hi: [i32; 2]) -> Vec<u64> {
        use minelib_net::proto::{cells_span, chunk_between, ZONE};
        // (v318) Sur la carte bouclée : la boîte peut passer la limite (ses chunks de l'autre
        // côté y sont).
        let inside = |c: [i32; 2]| {
            chunk_between(c[0], lo[0], hi[0]) && chunk_between(c[1], lo[1], hi[1])
        };
        if self.indexed != self.positions.len() {
            return self
                .positions
                .iter()
                .filter(|(_, &c)| inside(c))
                .map(|(&id, _)| id)
                .collect();
        }
        let mut out = Vec::new();
        for zx in cells_span(lo[0] as i64, hi[0] as i64, ZONE) {
            for zz in cells_span(lo[1] as i64, hi[1] as i64, ZONE) {
                if let Some(m) = self.by_zone.get(&[zx, zz]) {
                    out.extend(m.iter().filter(|(_, c)| inside(*c)).map(|&(id, _)| id));
                }
            }
        }
        out
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
        self.wake(zone);
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
        if let Some(r) = &mut self.replica {
            r.push((zone, seq, data.to_vec()));
        }
        let (kind, before) = (!blocks as usize, before[!blocks as usize]);
        // Archivée en retard (rang plus petit que le dernier de la zone : passation en fin
        // d'époque souveraine) : ceux qui ont déjà reçu plus récent de cette zone ne la
        // recevraient jamais à leur prochain rattrapage (ce qu'on leur envoie part de leur
        // dernier rang reçu). Leur dernier rang reçu recule juste avant elle ; ils
        // recevront de nouveau les plus récentes, sans effet (le jeu ignore ce qu'il a déjà).
        if seq < before {
            for (id, zones) in self.known.iter_mut() {
                if listeners.contains(id) {
                    continue;
                }
                if let Some(k) = zones.get_mut(&zone) {
                    if k[kind] >= seq {
                        k[kind] = seq.saturating_sub(1);
                    }
                }
            }
        }
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
    pub fn block_at(&mut self, pos: [i32; 3]) -> Option<u16> {
        let zone = minelib_net::proto::zone_of([pos[0].div_euclid(16), pos[2].div_euclid(16)]);
        self.wake(zone);
        self.logs.get(&zone)?.block_at(pos)
    }

    /// Auteur de la dernière modification de la case `pos`, si elle a déjà été modifiée.
    #[cfg(test)]
    pub fn author_at(&mut self, pos: [i32; 3]) -> Option<String> {
        let zone = minelib_net::proto::zone_of([pos[0].div_euclid(16), pos[2].div_euclid(16)]);
        self.wake(zone);
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
        self.wake(zone);
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
    pub fn logs_in(&mut self, keep: impl Fn([i32; 2]) -> bool) -> Vec<([i32; 2], Vec<Vec<u8>>)> {
        let sleeping: Vec<[i32; 2]> = self.asleep.keys().filter(|z| keep(**z)).copied().collect();
        for z in sleeping {
            self.wake(z);
        }
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
        // Les zones visées qui dorment se réveillent, seulement si ce joueur n'en a pas
        // tout reçu (le propriétaire, qui reçoit tout, ne les réveille pas toutes).
        // (v318) Les zones à portée, sur la carte bouclée.
        let reach: Vec<[i32; 2]> =
            chunk.map_or_else(Vec::new, |c| minelib_net::proto::zones_around(c, NEAR).collect());
        let in_reach = |w: &[i32; 2]| chunk.is_none() || reach.contains(w);
        let known = self.known.get(&id);
        let due = |w: &[i32; 2], last: &[u64; 2]| {
            let k = known.and_then(|k| k.get(w)).copied().unwrap_or_default();
            (last[0] > k[0] && blocks(*w)) || last[1] > k[1]
        };
        // (Tout le monde pour un nouveau propriétaire : par morceaux, le reste aux
        // entretiens suivants, pour ne pas bloquer le relais.)
        let wanted: Vec<[i32; 2]> = match chunk {
            // Autour d'un joueur : les zones à portée, cherchées par leurs coordonnées (v136 :
            // pas en parcourant toutes les zones endormies du monde à chaque pas).
            Some(_) => {
                let mut v = Vec::new();
                for &w in &reach {
                    if let Some(last) = self.asleep.get(&w) {
                        if due(&w, last) && v.len() < WAKE_MAX {
                            v.push(w);
                        }
                    }
                }
                v
            }
            None => self
                .asleep
                .iter()
                .filter(|(w, last)| in_reach(w) && due(w, last))
                .map(|(w, _)| *w)
                .take(WAKE_MAX)
                .collect(),
        };
        for w in wanted {
            self.wake(w);
        }
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
            Some(_) => {
                for &zone in &reach {
                    if let Some(l) = self.logs.get(&zone) {
                        take(zone, l);
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
        self.wide_tokens.remove(&id);
    }
}

#[cfg(test)]
mod tests {
    /// Mesure à la main : un monde où un million de cases ont été changées, réparties sur
    /// 2000 zones, avec et sans zones endormies (chaque cas dans son propre processus).
    /// Mémoire prise, place sur le disque.
    /// `cargo test --release -p minelib-relay memoire -- --ignored --nocapture`
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore]
    fn memoire_de_l_historique() {
        let rss = || {
            std::fs::read_to_string("/proc/self/status")
                .unwrap()
                .lines()
                .find(|l| l.starts_with("VmRSS:"))
                .and_then(|l| l.split_whitespace().nth(1)?.parse::<u64>().ok())
                .unwrap()
        };
        let z = minelib_net::proto::ZONE * 16;
        let fill = |b: &mut super::Build, paged: bool| {
            for k in 0..2000i32 {
                let (zx, zz) = (k % 50, k / 50);
                for i in 0..500i32 {
                    let pos = [zx * z + i % z, 40 + i / z, zz * z + (i * 7) % z];
                    b.publish(pos, (i % 50) as u16 + 1, "Constructeur", &[]);
                }
                if paged && k % 100 == 99 {
                    b.sleep_all();
                }
            }
        };
        match std::env::var("MINELIB_MESURE").as_deref() {
            Ok("memoire") => {
                let r0 = rss();
                let mut b = super::Build::default();
                fill(&mut b, false);
                eprintln!("MESURE {}", rss() - r0);
                return;
            }
            Ok("disque") => {
                let dir =
                    std::env::temp_dir().join(format!("minelib-mem-{}", rand::random::<u32>()));
                let r0 = rss();
                let mut b = super::Build::default();
                b.set_page_dir(dir.clone());
                fill(&mut b, true);
                b.sleep_all();
                let disk: u64 = std::fs::read_dir(&dir)
                    .unwrap()
                    .map(|e| e.unwrap().metadata().unwrap().len())
                    .sum();
                let asleep = rss() - r0;
                // Un nouveau propriétaire reçoit tout, par morceaux : le plus long.
                let (mut calls, mut longest) = (0, Duration::ZERO);
                loop {
                    let t = Instant::now();
                    let n = b.catch_up(1, None, &|_| true).len();
                    longest = longest.max(t.elapsed());
                    if n == 0 {
                        break;
                    }
                    calls += 1;
                    b.sleep_all();
                }
                eprintln!(
                    "MESURE {} {} {} {:.0}",
                    asleep,
                    disk / 1024,
                    calls,
                    longest.as_secs_f32() * 1000.0
                );
                return;
            }
            _ => {}
        }
        let run = |mode: &str| -> Vec<String> {
            let out = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--ignored",
                    "--exact",
                    "zones::tests::memoire_de_l_historique",
                ])
                .args(["--nocapture", "--test-threads=1"])
                .env("MINELIB_MESURE", mode)
                .output()
                .unwrap();
            let err = String::from_utf8_lossy(&out.stderr).into_owned();
            err.lines()
                .find_map(|l| l.strip_prefix("MESURE "))
                .unwrap_or_else(|| panic!("{err}"))
                .split(' ')
                .map(str::to_string)
                .collect()
        };
        let mem = run("memoire");
        let disk = run("disque");
        eprintln!(
            "1 million de cases changées sur 2000 zones : toutes en mémoire {} Mo ; \
             endormies {} Mo en mémoire et {} Mo sur le disque ; tout remettre à un \
             nouveau propriétaire : {} fois, {} ms au plus chaque fois",
            mem[0].parse::<u64>().unwrap() / 1024,
            disk[0].parse::<u64>().unwrap() / 1024,
            disk[1].parse::<u64>().unwrap() / 1024,
            disk[2],
            disk[3]
        );
    }

    /// Les zones froides s'endorment sur le disque et se réveillent quand on en a besoin :
    /// à la fin, tout est comme si elles étaient restées en mémoire (ce qu'un joueur qui
    /// arrive reçoit, les cases, les numéros), et le propriétaire, qui a déjà tout reçu,
    /// ne les réveille pas.
    #[test]
    fn cold_zones_sleep_on_disk_and_wake_unchanged() {
        use minelib_net::game::{self, BuildMsg};
        let dir = std::env::temp_dir().join(format!("minelib-zones-{}", rand::random::<u32>()));
        let mut paged = super::Build::default();
        paged.set_page_dir(dir.clone());
        let mut plain = super::Build::default();
        let z = minelib_net::proto::ZONE * 16;
        // Trois zones, quelques cases chacune (dont une changée deux fois).
        let edits: Vec<([i32; 3], u16)> = (0..3)
            .flat_map(|k| (0..4).map(move |i| ([k * z + i, 64, 1], (k * 10 + i) as u16 + 1)))
            .chain([([1, 64, 1], 77)])
            .collect();
        for (pos, block) in &edits {
            paged.publish(*pos, *block, "Eve", &[]);
            plain.publish(*pos, *block, "Eve", &[]);
        }
        // Le propriétaire (joueur 1) a tout reçu.
        let _ = paged.catch_up(1, None, &|_| true);
        assert_eq!(paged.sleep_cold(Duration::ZERO), 3);
        assert_eq!(paged.zones_awake_asleep(), (0, 3));
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 3);
        // Rien de nouveau pour le propriétaire : rien ne se réveille.
        assert!(paged.catch_up(1, None, &|_| true).is_empty());
        assert_eq!(paged.zones_awake_asleep(), (0, 3));
        // Une case lue : sa zone seule se réveille.
        assert_eq!(paged.block_at([1, 64, 1]), Some(77));
        assert_eq!(paged.zones_awake_asleep(), (1, 2));
        // Une modification dans une zone endormie : elle se réveille, les numéros suivent.
        let (a, _) = paged.publish([2 * z + 1, 64, 1], 99, "Eve", &[]);
        let (b, _) = plain.publish([2 * z + 1, 64, 1], 99, "Eve", &[]);
        assert_eq!(a, b, "même numéro");
        // Un joueur qui arrive reçoit exactement la même chose.
        let got = |b: &mut super::Build| {
            let mut v: Vec<([i32; 3], u16, u64)> = b
                .catch_up(9, None, &|_| true)
                .into_iter()
                .flat_map(|(_, l)| l)
                .filter_map(|d| match game::decode::<BuildMsg>(&d) {
                    Some(BuildMsg::Set {
                        pos, block, seq, ..
                    }) => Some((pos, block, seq)),
                    _ => None,
                })
                .collect();
            v.sort();
            v
        };
        assert_eq!(got(&mut paged), got(&mut plain));
        // Le monde fermé, ses zones endormies partent avec lui.
        drop(paged);
        assert!(!dir.exists());
    }

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

    /// Une modification archivée en retard (fin d'une époque souveraine : passation par
    /// l'autre hôte), plus ancienne que ce qu'un joueur a déjà reçu de sa zone mais seule
    /// de sa case, lui arrive quand même à son prochain rattrapage (le propriétaire du
    /// monde, qui reçoit tout ainsi, la ratait : son monde enregistré n'avait pas ce bloc).
    #[test]
    fn a_late_archived_edit_still_reaches_those_who_got_newer_ones() {
        use minelib_net::game::{self, BuildMsg};
        let set = |pos: [i32; 3], block: u16, seq: u64| {
            game::encode(&BuildMsg::Set {
                pos,
                block,
                by: "Eve".into(),
                seq,
            })
        };
        let cells = |logs: Vec<([i32; 2], Vec<Vec<u8>>)>| -> Vec<([i32; 3], u16)> {
            let mut v: Vec<([i32; 3], u16)> = logs
                .iter()
                .flat_map(|(_, l)| l.iter())
                .filter_map(|d| match game::decode::<BuildMsg>(d) {
                    Some(BuildMsg::Set { pos, block, .. }) => Some((pos, block)),
                    _ => None,
                })
                .collect();
            v.sort();
            v
        };
        let mut b = super::Build::default();
        let zone = minelib_net::proto::zone_of([0, 0]);
        let (a, late, owner) = ([1, 70, 1], [2, 70, 2], 1);
        b.mirror_at(zone, &set(a, 5, 100), Some(100), &[]);
        assert_eq!(cells(b.catch_up(owner, None, &|_| true)), vec![(a, 5)]);
        // Archivée en retard, à un rang plus petit, sans destinataire direct.
        b.mirror_at(zone, &set(late, 3, 60), Some(60), &[]);
        assert_eq!(
            cells(b.catch_up(owner, None, &|_| true)),
            vec![(a, 5), (late, 3)],
            "le propriétaire la reçoit (et l'autre, déjà reçue, de nouveau : sans effet)"
        );
        assert!(cells(b.catch_up(owner, None, &|_| true)).is_empty());
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
            ping: 40,
            held: 0,
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
                                                (r() % span) as f64 + 0.37,
                                                64.0 + (r() % 4) as f64,
                                                (r() % span) as f64 + 0.81,
                                            ],
                                            yaw: (r() % 628) as f32 / 100.0,
                                            pitch: 0.1,
                                            crouch: 0.0,
                                            speed: 4.3,
                                            ping: None,
                                            held: 0,
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

    /// (v275) Une position qui part à peu de joueurs passe 256 fois par seconde ; dès
    /// qu'elle part à davantage, 66 fois comme avant, quel que soit le rythme d'envoi.
    #[test]
    fn positions_pass_256_times_a_second_to_few_players_and_66_to_a_crowd() {
        let passed = |to: usize, hz: u32| {
            let mut b = Build::default();
            let t0 = Instant::now();
            (0..hz * 4)
                .filter(|i| {
                    let now = t0 + Duration::from_secs_f64(f64::from(*i) / f64::from(hz));
                    b.pos_due_at(7, now) && b.pos_fits_at(7, to, now)
                })
                .count() as f32
                / 4.0
        };
        // Le jeu : 256 par seconde à 3 voisins, 60 au-delà ; rien n'est écarté.
        assert_eq!(passed(3, 256), 256.0);
        assert_eq!(passed(POS_FEW, 256), 256.0);
        assert_eq!(passed(16, 60), 60.0);
        assert_eq!(passed(2048, 10), 10.0);
        // Un jeu modifié : 280 au plus vers peu de joueurs, 66 vers une foule (rafale du
        // début comprise).
        let few = passed(3, 1000);
        assert!((270.0..=290.0).contains(&few), "{few}");
        let crowd = passed(POS_FEW + 1, 1000);
        assert!((60.0..=70.0).contains(&crowd), "{crowd}");
        let crowd = passed(2048, 256);
        assert!((60.0..=70.0).contains(&crowd), "{crowd}");
    }

    /// (v285) Positions rapprochées : dans une foule, chacun a 3 voisins proches, toujours
    /// les mêmes, et est le voisin proche de 3 joueurs ; seul dans son chunk, ce sont ceux
    /// des chunks voisins ; et jamais un joueur que ce relais ne sert pas.
    #[test]
    fn close_positions_go_to_three_neighbours_each() {
        let mut b = Build::default();
        for id in 1..=40u64 {
            b.positions.insert(id, [0, 0]);
        }
        b.positions.insert(100, [1, 0]);
        let mut got: HashMap<u64, u32> = HashMap::new();
        for id in 1..=40u64 {
            let near = b.closest(id, [0, 0], |_| true);
            assert_eq!(near.len(), POS_CLOSE_TO);
            assert_eq!(near, b.closest(id, [0, 0], |_| true), "toujours les mêmes");
            assert!(!near.contains(&id) && !near.contains(&100));
            for m in near {
                *got.entry(m).or_default() += 1;
            }
        }
        assert!((1..=40u64).all(|id| got.get(&id) == Some(&(POS_CLOSE_TO as u32))), "{got:?}");
        // Seul dans son chunk : ses voisins sont dans le chunk d'à côté.
        assert_eq!(b.closest(100, [1, 0], |_| true).len(), POS_CLOSE_TO);
        // Trop loin de tous (plus de 2 chunks) : personne.
        b.positions.insert(200, [9, 9]);
        assert!(b.closest(200, [9, 9], |_| true).is_empty());
        // Ceux que ce relais ne sert pas sont passés.
        let near = b.closest(1, [0, 0], |m| m % 2 == 0);
        assert_eq!(near.len(), POS_CLOSE_TO);
        assert!(near.iter().all(|m| m % 2 == 0));
    }

    /// (v318) Sur la carte bouclée : de part et d'autre de sa limite, deux joueurs voisins se
    /// voient (positions, modifications, foule, voisins proches), et une boîte à cheval sur
    /// elle trouve ceux des deux côtés ; un joueur plus loin, non.
    #[test]
    fn neighbours_across_the_edge_of_the_map_are_neighbours() {
        let h = (minelib_net::proto::LAP_CHUNKS / 2) as i32;
        let mut b = Build::default();
        b.moved(1, [h - 1, 7], &|_| true);
        b.moved(2, [-h, 7], &|_| true);
        b.moved(3, [-h + 20, 7], &|_| true);
        let mut got = std::collections::HashSet::new();
        for _ in 0..40 {
            got.extend(b.nearby(1, [h - 1, 7], 1));
        }
        assert!(got.contains(&2) && !got.contains(&3), "{got:?}");
        let l = b.listeners([h - 1, 7], 0);
        assert!(l.contains(&1) && l.contains(&2) && !l.contains(&3), "{l:?}");
        let w = b.within([-h, 7], 2, 0);
        assert!(w.contains(&1) && w.contains(&2) && !w.contains(&3), "{w:?}");
        let p = b.players_in([h - 2, 0], [h + 1, 10]);
        assert!(p.contains(&1) && p.contains(&2) && !p.contains(&3), "{p:?}");
        assert!(b.closest(1, [h - 1, 7], |_| true).contains(&2));
        // (Avec beaucoup de chunks occupés, par les zones : pareil.)
        for id in 100..800u64 {
            b.moved(id, [(id as i32) * 3, 1000], &|_| true);
        }
        let l = b.listeners([-h, 7], 0);
        assert!(l.contains(&1) && l.contains(&2) && !l.contains(&3), "{l:?}");
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
