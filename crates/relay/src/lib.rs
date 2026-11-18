//! Relais Minelib : comptes (inscription, connexion, double authentification) et, plus
//! tard, transmission des parties. Lancé seul (`minelib-relay`) ou dans le jeu pour jouer
//! sur un seul PC.
//!
//! Pas de base de données : un registre public chaîné et signé (`registry`) pour les
//! pseudos et les bans, et un fichier par compte pour ce qui ne doit pas être public
//! (preuve de mot de passe, coffre chiffré, secret 2FA chiffré).

mod accounts;
mod bags;
mod contacts;
mod guards;
mod hardware;
mod limits;
mod maps;
mod provinces;
mod registry;
mod servers;
pub mod shard;
mod stats;
mod tasks;
mod zones;

use std::collections::{HashSet, VecDeque};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use ed25519_dalek::{Signature, SigningKey, Verifier, VerifyingKey};
use minelib_net::proto::{self, ErrorCode, Request, Response};
use minelib_net::transport;
#[cfg(test)]
use minelib_net::transport::Inbound;
use minelib_net::{crypto, game, sovereign, totp};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
#[cfg(test)]
use std::sync::mpsc::Sender;

pub use registry::{Body, Registry};

/// Réglages du relais.
#[derive(Clone, Debug)]
pub struct Config {
    /// Dossier des données (clés, registre, comptes).
    pub dir: PathBuf,
    /// Adresse d'écoute (« 0.0.0.0:27650 » pour Internet, « 127.0.0.1:… » en local).
    pub bind: String,
    /// Difficulté de la preuve de travail à l'inscription (bits à zéro).
    pub pow_bits: u8,
    pub name: String,
}

pub struct Handle {
    pub addr: SocketAddr,
    /// Clé publique du canal chiffré (celle que les jeux retiennent).
    pub public: [u8; 32],
    whois: Arc<Mutex<Option<WhoisFn>>>,
    peer_servers: Arc<Mutex<Option<ServersFn>>>,
    shard_from: Arc<std::sync::atomic::AtomicUsize>,
    shard_share: Arc<std::sync::atomic::AtomicUsize>,
    sov_faults: Arc<std::sync::atomic::AtomicUsize>,
    sov_committed: Arc<std::sync::atomic::AtomicUsize>,
    province_size: Arc<std::sync::atomic::AtomicUsize>,
    province_from: Arc<std::sync::atomic::AtomicUsize>,
    /// État du relais (relais de secours, essais).
    state: Arc<Mutex<State>>,
    /// Relais arrêté (essais : sa chute).
    killed: Arc<std::sync::atomic::AtomicBool>,
}

/// Interroge les autres relais : clés des comptes qui y portent ce pseudo.
pub type WhoisFn = Arc<dyn Fn(&str) -> Vec<[u8; 32]> + Send + Sync>;

/// Parties ouvertes sur les autres relais (sans attendre : la dernière liste connue),
/// chacune avec de quoi joindre son relais (`ServerEntry::relay_*`).
pub type ServersFn = Arc<dyn Fn(u8) -> Vec<proto::ServerEntry> + Send + Sync>;

/// Joueurs dans une région d'un monde de construction à partir desquels elle a son propre
/// relais (hébergé par un joueur sans lien avec ce monde).
pub const SHARD_FROM: usize = 24;

/// Règles du jeu reprises pour les simulations (le jeu n'est pas visible d'ici).
#[cfg(test)]
pub(crate) mod tests_support {
    /// Positions envoyées par seconde par un joueur qui en voit `others` autour de lui
    /// (même règle que `pos_every` dans crates/app/src/buildnet.rs).
    pub fn pos_hz(others: usize) -> f32 {
        (960.0 / others.max(1) as f32).clamp(10.0, 60.0)
    }
}

/// Joueurs de plus dans la région pour chaque relais de plus (chez un autre hôte), et
/// relais au plus par région : une foule est partagée entre plusieurs connexions de
/// joueurs, aucune ne porte toute la région.
///
/// 32 relais au plus (8 avant) : jusqu'à 3000 joueurs dans la région, aucun relais n'en
/// porte plus d'une centaine. À 8, la simulation donnait 375 joueurs et 160 Mbit/s à
/// envoyer par relais pour 3000 joueurs serrés.
pub const SHARD_SHARE: usize = 96;
pub const SHARD_MAX: usize = 32;

/// Relais voulus pour une région de `players` joueurs.
fn shards_wanted(players: usize, from: usize, share: usize) -> usize {
    if players < from {
        0
    } else {
        (1 + (players - from) / share.max(1)).min(SHARD_MAX)
    }
}

impl Handle {
    /// Change le seuil `SHARD_FROM` (essais à petite échelle).
    pub fn set_shard_from(&self, players: usize) {
        self.shard_from
            .store(players, std::sync::atomic::Ordering::Relaxed);
    }

    /// Relais fédérés : les joueurs de ce relais voient aussi les parties des autres
    /// relais, données par `f`.
    pub fn set_peer_servers(&self, f: ServersFn) {
        *self.peer_servers.lock().unwrap() = Some(f);
    }

    /// Change `SHARD_SHARE` (essais à petite échelle).
    pub fn set_shard_share(&self, players: usize) {
        self.shard_share
            .store(players, std::sync::atomic::Ordering::Relaxed);
    }

    /// Provinces (v138) : régions par côté, et joueurs à partir desquels une province
    /// reçoit son relais (essais à petite échelle).
    pub fn set_provinces(&self, size: usize, from: usize) {
        self.province_size
            .store(size.max(1), std::sync::atomic::Ordering::Relaxed);
        self.province_from
            .store(from.max(1), std::sync::atomic::Ordering::Relaxed);
    }

    /// (v143) Les provinces confiées par ce relais sont des pays, qui confient eux-mêmes
    /// leurs provinces de `size` régions de côté peuplées d'au moins `from` joueurs (`size`
    /// 0 : pas de pays). La taille des pays est celle des provinces (`set_provinces`).
    pub fn set_nest(&self, size: usize, from: usize) {
        self.state.lock().unwrap().nest = (size > 0).then_some(proto::Nest {
            size: size as i32,
            from: from.max(1) as u32,
        });
    }

    /// (v148) Étages de pays sous ceux de ce relais (essais) : leurs provinces de cette
    /// taille, peuplées d'au moins tant de joueurs, sont à leur tour des pays, et ainsi de
    /// suite.
    pub fn set_deeper(&self, levels: &[(usize, usize)]) {
        self.state.lock().unwrap().deeper = levels
            .iter()
            .filter(|(size, _)| *size > 0)
            .map(|&(size, from)| proto::Nest {
                size: size as i32,
                from: from.max(1) as u32,
            })
            .collect();
    }

    /// (v146) Joueurs connectés ici dans cette partie, par pseudo (essais : ceux qui sont en
    /// veille au fond d'un pays n'y sont plus).
    pub fn players_here(&self, room: u64) -> Vec<String> {
        let st = self.state.lock().unwrap();
        let Some(r) = st.rooms.get(&room) else {
            return Vec::new();
        };
        r.everyone()
            .filter_map(|m| st.conns.get(&m)?.pseudo.clone())
            .collect()
    }

    /// (v143) Provinces de pays servies, telles que les pays les disent ici (essais).
    pub fn pays_children(&self) -> Vec<proto::ProvinceInfo> {
        self.state
            .lock()
            .unwrap()
            .rooms
            .values()
            .flat_map(|r| {
                r.provinces
                    .values()
                    .flat_map(|p| p.children.iter().cloned())
            })
            .collect()
    }

    /// (v143) Relais de province : il s'arrête (un pays attend d'abord que ses provinces lui
    /// aient tout rendu).
    pub fn province_stopped(&self) -> bool {
        let st = self.state.lock().unwrap();
        st.province_room
            .and_then(|r| st.rooms.get(&r))
            .and_then(|r| r.province.as_ref())
            .is_none_or(|p| p.is_stopped())
    }

    /// Provinces servies par un relais de province, prêtes (essais).
    pub fn delegated_provinces(&self) -> Vec<[i32; 2]> {
        self.state
            .lock()
            .unwrap()
            .rooms
            .values()
            .flat_map(|r| {
                r.provinces
                    .iter()
                    .filter(|(_, p)| p.info.is_some())
                    .map(|(k, _)| *k)
            })
            .collect()
    }

    /// Provinces servies ici par un relais de province, prêtes (essais).
    pub fn provinces_delegated(&self) -> usize {
        self.state
            .lock()
            .unwrap()
            .rooms
            .values()
            .map(|r| r.provinces.values().filter(|p| p.info.is_some()).count())
            .sum()
    }

    /// Relais de province (hôte, v138) : tient la province `province` du monde `room` d'un
    /// autre relais (voir `crate::provinces`).
    #[allow(clippy::too_many_arguments)]
    pub fn province_begin(
        &self,
        room: u64,
        province: [i32; 2],
        size: i32,
        epoch: u64,
        secret: [u8; 32],
        root: [u8; 32],
        info: proto::StandbyInfo,
        settings: proto::ProvinceSettings,
        standby: bool,
        nest: Option<proto::Nest>,
        deeper: Vec<proto::Nest>,
    ) -> bool {
        provinces::begin(
            &mut self.state.lock().unwrap(),
            room,
            province,
            size,
            epoch,
            secret,
            root,
            info,
            settings,
            standby,
            nest,
            deeper,
        )
    }

    /// Relais de secours d'une province (v141) : un envoi gardé par le relais du monde ;
    /// renvoie jusqu'où la copie est à jour.
    #[allow(clippy::too_many_arguments)]
    pub fn province_copy(
        &self,
        epoch: u64,
        n: u64,
        entries: Vec<([i32; 2], u64, Vec<u8>)>,
        bags: Vec<proto::BagCopy>,
        held: Vec<String>,
        spots: Vec<(String, [f32; 5])>,
    ) -> Option<u64> {
        provinces::copy(
            &mut self.state.lock().unwrap(),
            epoch,
            n,
            entries,
            bags,
            held,
            spots,
        )
    }

    /// (v144) Relais de secours d'un pays : ce que le pays sait de ses provinces.
    pub fn province_children(
        &self,
        epoch: u64,
        list: Vec<proto::ChildCopy>,
        gone: Vec<[i32; 2]>,
        full: bool,
    ) {
        provinces::children_here(&mut self.state.lock().unwrap(), epoch, list, gone, full);
    }

    /// (v144) Relais d'un pays : ses provinces servies (coordonnées, époque) ; essais.
    pub fn province_kids(&self) -> Vec<([i32; 2], u64, bool)> {
        let st = self.state.lock().unwrap();
        let mut v: Vec<([i32; 2], u64, bool)> = st
            .province_room
            .and_then(|r| st.rooms.get(&r))
            .map(|r| {
                r.provinces
                    .iter()
                    .filter(|(_, p)| p.info.is_some())
                    .map(|(k, p)| (*k, p.epoch, p.big))
                    .collect()
            })
            .unwrap_or_default();
        v.sort();
        v
    }

    /// Relais de secours : il prend la place du relais de la province ; jusqu'où il a la
    /// copie.
    pub fn province_promote(&self, old: u64, epoch: u64) -> Option<u64> {
        provinces::promote_here(&mut self.state.lock().unwrap(), old, epoch)
    }

    /// Provinces confiées à leur relais de secours (essais).
    pub fn province_promotions(&self) -> usize {
        self.state.lock().unwrap().province_promotions
    }

    /// (v142) Joueurs d'un monde à partir desquels la copie de ce que publient ses provinces
    /// est gardée par leurs relais de secours (essais).
    pub fn set_copies_until(&self, players: usize) {
        self.state.lock().unwrap().copies_until = players;
    }

    /// (v147) Plage de rangs donnée à un pays (essais : petite, pour qu'il en redemande).
    pub fn set_pays_seqs(&self, n: u64) {
        self.state.lock().unwrap().pays_seqs = n;
    }

    /// (v147) Nouvelles plages de rangs données à des pays (essais).
    pub fn pays_refills(&self) -> usize {
        self.state.lock().unwrap().pays_refills
    }

    /// (v147) Hôtes de provinces qu'un pays a choisis lui-même parmi ses joueurs (essais).
    pub fn local_tasks(&self) -> usize {
        self.state.lock().unwrap().local_tasks
    }

    /// (v147) Relais de province : où le joindre (le jeu qui le tient le dit) ; un pays le
    /// donne à ses garants et à ses provinces.
    pub fn province_self(&self, node: [u8; 32], addr: String, key: [u8; 32]) {
        provinces::set_me(&mut self.state.lock().unwrap(), node, addr, key);
    }

    /// Provinces dont la copie est gardée par leur relais de secours (essais).
    pub fn provinces_big(&self) -> usize {
        self.state
            .lock()
            .unwrap()
            .rooms
            .values()
            .map(|r| r.provinces.values().filter(|p| p.big).count())
            .sum()
    }

    /// Entrées gardées ici pour les zones de la province `p` du monde `room` (essais).
    pub fn province_entries(&self, room: u64, p: [i32; 2]) -> usize {
        let mut st = self.state.lock().unwrap();
        let size = provinces::size(&st);
        let Some(r) = st.rooms.get_mut(&room) else {
            return 0;
        };
        r.build
            .entries_after(
                |z| {
                    proto::province_of(
                        proto::region_of([z[0] * proto::ZONE, z[1] * proto::ZONE]),
                        size,
                    ) == p
                },
                0,
            )
            .len()
    }

    /// Relais de province (v139) : du relais du monde, un sac de survie prêté ou rappelé,
    /// les réglages du serveur, un WorldEdit d'un OP.
    pub fn province_event(&self, r: &Response) {
        let mut st = self.state.lock().unwrap();
        match r {
            Response::ProvinceBagLent {
                epoch,
                pseudo,
                items,
                mined,
                mode,
                ..
            } => provinces::bag_lent(&mut st, *epoch, pseudo, items.clone(), *mined, mode.clone()),
            Response::ProvinceBagRecall { pseudo, .. } => provinces::bag_back(&mut st, pseudo),
            Response::ProvinceSettings { settings, .. } => {
                provinces::settings(&mut st, settings.clone())
            }
            Response::ProvinceBulk {
                epoch,
                pseudo,
                edits,
                ..
            } => provinces::bulk(&mut st, *epoch, pseudo, edits.clone()),
            // (v145) Ses voisines, dites par son pays.
            Response::ProvinceNeighbours { epoch, list, .. } => {
                provinces::neighbours(&mut st, *epoch, list.clone())
            }
            // (v146) Un OP a tué un joueur qui n'est plus connecté au relais du monde.
            Response::ProvinceSlay { pseudo, by, .. } => provinces::slay(&mut st, pseudo, by),
            _ => {}
        }
    }

    /// Relais de province : historique reçu (et, à la fin, sa plage de rangs : il sert dès
    /// son premier bail).
    pub fn province_seed(&self, entries: Vec<([i32; 2], u64, Vec<u8>)>, go: Option<u64>) {
        provinces::seed(&mut self.state.lock().unwrap(), entries, go);
    }

    /// Relais de province : ce qu'il faut envoyer maintenant au relais du monde.
    pub fn province_tick(&self) -> Vec<Request> {
        provinces::tick(&mut self.state.lock().unwrap())
    }

    /// Relais de province : le relais du monde a gardé l'envoi `n`.
    pub fn province_archived(&self, epoch: u64, n: u64) {
        provinces::archived(&mut self.state.lock().unwrap(), epoch, n);
    }

    /// Objets du monde `room` tels que les garde ce relais : au sol et dans les coffres
    /// (nombre total ; essais).
    pub fn world_items(&self, room: u64) -> u64 {
        let st = self.state.lock().unwrap();
        let Some(r) = st.rooms.get(&room) else {
            return 0;
        };
        let ground: u64 = r
            .build
            .items
            .live_drops()
            .map(|(_, d)| d.pile.count as u64)
            .sum();
        let chests: u64 = r
            .build
            .items
            .chests
            .values()
            .flat_map(|c| c.iter().flatten())
            .map(|p| p.count as u64)
            .sum();
        ground + chests
    }

    /// Sac de survie de `pseudo` dans le monde `room` tel que le compte ce relais : objets
    /// et crédits de minage, avec les blocs cassés comptés pendant qu'il est prêté à une
    /// province (essais).
    pub fn bag_total(&self, room: u64, pseudo: &str) -> u64 {
        let st = self.state.lock().unwrap();
        let Some(r) = st.rooms.get(&room) else {
            return 0;
        };
        let b = st.bags.get(&r.server, pseudo);
        let later = r
            .bag_deltas
            .get(&pseudo.to_ascii_lowercase())
            .map_or(0, |d| d.iter().filter(|b| **b == 0).count() as u64);
        b.items.values().map(|n| *n as u64).sum::<u64>() + b.mined as u64 + later
    }

    /// Positions (changements de chunk) reçues de joueurs de mondes de construction (essais).
    pub fn positions_seen(&self) -> usize {
        self.state.lock().unwrap().positions_seen
    }

    /// Sacs de survie prêtés à des provinces depuis le début (essais).
    pub fn province_lends(&self) -> usize {
        self.state.lock().unwrap().province_lends
    }

    /// Sacs de survie prêtés à une province en ce moment (essais).
    pub fn bags_lent(&self, room: u64) -> usize {
        let st = self.state.lock().unwrap();
        st.rooms.get(&room).map_or(0, |r| r.bag_holders.len())
    }

    /// Relais du monde : envois de relais de province gardés ici (essais).
    pub fn province_archives(&self) -> usize {
        self.state.lock().unwrap().province_archived
    }

    /// Relais de province : réponse du relais du monde à une demande de bail.
    #[allow(clippy::too_many_arguments)]
    pub fn province_leased(
        &self,
        epoch: u64,
        t: u64,
        ok: bool,
        copies: bool,
        dump: u64,
        ms: u32,
        call: Option<proto::GarantCall>,
        more: Option<u64>,
        dense: u32,
        kids: bool,
    ) {
        provinces::leased(
            &mut self.state.lock().unwrap(),
            epoch,
            t,
            ok,
            copies,
            dump,
            ms,
            call,
            more,
            dense,
            kids,
        );
    }

    /// Relais de province : arrêt ; `release` : départ propre (le relais du monde reprend
    /// tout de suite). Ce qu'il reste à lui envoyer.
    pub fn province_stop(&self, release: bool) -> Vec<Request> {
        provinces::stop_here(&mut self.state.lock().unwrap(), release)
    }

    /// Relais de province : il sert (bail en cours ; essais).
    pub fn province_serving(&self) -> bool {
        let st = self.state.lock().unwrap();
        let now = guards::now_ms(&st);
        st.province_room
            .and_then(|r| st.rooms.get(&r))
            .and_then(|r| r.province.as_ref())
            .is_some_and(|p| p.serving(now))
    }

    /// Hôtes principaux de régions souveraines écartés pour une faute prouvée.
    pub fn sov_faults(&self) -> usize {
        self.sov_faults.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Messages de voix transmis par ce relais (essais).
    pub fn voice_relayed(&self) -> usize {
        self.state.lock().unwrap().voice_relayed
    }

    /// Modifications de régions souveraines archivées ici.
    pub fn sov_committed(&self) -> usize {
        self.sov_committed
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Essais : le relais de secours peut être sur la même machine que le relais du monde
    /// (tous les joueurs simulés y sont).
    pub fn set_standby_local(&self, ok: bool) {
        self.state.lock().unwrap().standby_local = ok;
    }

    /// Ce relais devient le relais de secours du monde `room` d'un autre relais (clé de
    /// ses certificats et de ses listes de gardiens : `root`), tenu par le joueur `me` ;
    /// il en reçoit la copie (`standby_feed`) jusqu'à la reprise (`standby_tick`,
    /// `standby_released`).
    pub fn standby_begin(&self, room: u64, root: [u8; 32], info: proto::StandbyInfo, me: &str) {
        let mut st = self.state.lock().unwrap();
        let guard =
            minelib_net::guard::Keeper::new(room, me, root).map(|keeper| guards::StandbyGuard {
                keeper,
                votes: HashMap::new(),
                root_paused: false,
            });
        let Ok(root) = VerifyingKey::from_bytes(&root) else {
            return;
        };
        let mut build = zones::Build::default();
        build.set_page_dir(st.cfg.dir.join("zones").join("secours"));
        st.bags.watch(&info.server, false);
        st.standby = Some(StandbyCopy {
            room,
            root,
            info,
            build: Some(build),
            active: false,
            resumed: None,
            guard,
            proof: None,
        });
    }

    /// Relais de secours : battement du relais du monde. Renvoie la version de la liste à
    /// rappeler dans le bail (aucune : il a voté, il ne soutient plus ce relais).
    pub fn standby_beat(
        &self,
        committee: &minelib_net::guard::Committee,
        paused: bool,
    ) -> Option<u64> {
        guards::standby_beat(&mut self.state.lock().unwrap(), committee, paused)
    }

    /// Relais de secours, régulièrement : il vote quand le relais du monde se tait, et
    /// reprend le monde avec les votes d'une majorité des gardiens. Vrai au moment de la
    /// reprise.
    pub fn standby_tick(&self) -> bool {
        guards::standby_tick(&mut self.state.lock().unwrap())
    }

    /// Relais de secours : le relais du monde s'arrête proprement et lui confie le monde
    /// (`sig` : cet arrêt, signé par lui ; c'est la preuve montrée aux joueurs).
    pub fn standby_released(&self, room: u64, sig: &[u8]) -> bool {
        let mut st = self.state.lock().unwrap();
        match st.standby.as_mut() {
            Some(sb)
                if sb.room == room
                    && !sb.active
                    && minelib_net::guard::release_valid(&sb.root, room, sig) =>
            {
                sb.active = true;
                sb.proof = Some(minelib_net::guard::Proof::Released { sig: sig.to_vec() });
                log::info!(
                    "relais de secours : le relais du monde s'arrête proprement, reprise du \
                     monde « {} »",
                    sb.info.name
                );
                true
            }
            _ => false,
        }
    }

    /// Relais de secours : une liste plus récente, entendue du relais du monde, en nomme un
    /// autre (il n'a plus rien à faire).
    pub fn standby_replaced(&self) -> bool {
        let st = self.state.lock().unwrap();
        st.standby.as_ref().is_some_and(|sb| {
            !sb.active
                && sb.guard.as_ref().is_some_and(|g| {
                    g.keeper
                        .committee
                        .as_ref()
                        .is_some_and(|c| !c.is_standby(&g.keeper.me))
                })
        })
    }

    /// Arrêt propre de ce relais : chaque monde qui a un relais de secours lui est confié
    /// tout de suite (il est en pause ici pour toujours). Renvoie le nombre de mondes
    /// confiés. Laisser ensuite partir les messages (quelques centaines de ms).
    pub fn release(&self) -> usize {
        guards::release_all(&mut self.state.lock().unwrap())
    }

    /// Essais : le réseau entre ce relais et ces gardiens est coupé (ni battements, ni
    /// baux) ; liste vide : rétabli.
    #[doc(hidden)]
    pub fn guard_cut(&self, pseudos: &[&str]) {
        let mut st = self.state.lock().unwrap();
        st.guard_cut = pseudos.iter().map(|p| p.to_ascii_lowercase()).collect();
    }

    /// Essais : ce relais est coupé de tous les gardiens (les joueurs le joignent encore).
    #[doc(hidden)]
    pub fn guard_isolate(&self, on: bool) {
        self.state.lock().unwrap().guard_isolated = on;
    }

    /// Essais : le monde est-il en pause ici (aucun : pas de gardiens) ?
    #[doc(hidden)]
    pub fn frozen(&self, room: u64) -> Option<bool> {
        let st = self.state.lock().unwrap();
        st.rooms.get(&room)?.guards.as_ref()?;
        Some(guards::frozen(&st, room))
    }

    /// Essais : la liste des gardiens d'un monde.
    #[doc(hidden)]
    pub fn committee(&self, room: u64) -> Option<minelib_net::guard::Committee> {
        let st = self.state.lock().unwrap();
        Some(st.rooms.get(&room)?.guards.as_ref()?.warden.current.clone())
    }

    /// Relais de secours : ce que le relais du monde lui envoie (historique, sacs, modes).
    pub fn standby_feed(&self, msg: &Response) {
        let mut st = self.state.lock().unwrap();
        let st = &mut *st;
        let Some(sb) = st.standby.as_mut() else {
            return;
        };
        match msg {
            Response::Replica { zone, seq, data } => {
                if let Some(b) = sb.build.as_mut() {
                    b.mirror_at(*zone, data, Some(*seq), &[]);
                }
            }
            Response::ReplicaBag {
                pseudo,
                items,
                mined,
            } => {
                let bag = bags::Bag {
                    items: items.iter().copied().collect(),
                    mined: *mined,
                };
                st.bags.put(&sb.info.server, pseudo, bag);
            }
            Response::ReplicaMode { pseudo, mode } => {
                st.bags.set_mode(&sb.info.server, pseudo, mode);
            }
            _ => {}
        }
    }

    /// Essais seulement : ce relais de secours reprend le monde sans vote (en vrai, voir
    /// `standby_tick` et `standby_released`). Les joueurs y entrent avec le certificat de
    /// leur compte (`Request::Takeover`) ; le premier en devient le propriétaire.
    #[doc(hidden)]
    pub fn take_over(&self) -> bool {
        let mut st = self.state.lock().unwrap();
        match st.standby.as_mut() {
            Some(sb) if !sb.active => {
                sb.active = true;
                log::info!(
                    "relais de secours : le relais du monde est tombé, reprise du monde « {} »",
                    sb.info.name
                );
                true
            }
            _ => false,
        }
    }

    /// Relais de secours : le monde repris (son numéro ici), une fois qu'un joueur y est.
    pub fn resumed(&self) -> Option<u64> {
        self.state
            .lock()
            .unwrap()
            .standby
            .as_ref()
            .and_then(|s| s.resumed)
    }

    /// Essais seulement : le relais tombe (connexions coupées, plus rien n'est accepté).
    #[doc(hidden)]
    pub fn kill(&self) {
        self.killed
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let st = self.state.lock().unwrap();
        for c in st.conns.values() {
            use transport::Sink;
            c.tx.put(transport::Inbound::Closed);
        }
    }

    /// Les pseudos deviennent uniques avec les autres relais : avant chaque inscription,
    /// `f` leur demande si le pseudo est déjà pris.
    pub fn set_whois(&self, f: WhoisFn) {
        *self.whois.lock().unwrap() = Some(f);
    }
}

/// Recherche dans les autres relais du DHT (par tunnel iroh), sauf ce relais (`me`).
/// La liste des relais est relue au plus toutes les 5 minutes.
pub fn federated_whois(node: &'static minelib_p2p::Node, me: [u8; 32]) -> WhoisFn {
    let cache: Arc<Mutex<(Option<Instant>, Vec<minelib_p2p::Ad>)>> =
        Arc::new(Mutex::new((None, Vec::new())));
    Arc::new(move |pseudo: &str| {
        let ads = {
            let mut c = cache.lock().unwrap();
            if c.0.is_none_or(|t| t.elapsed() > Duration::from_secs(300)) {
                if let Ok(list) = minelib_p2p::find_relays() {
                    c.1 = list;
                }
                c.0 = Some(Instant::now());
            }
            c.1.clone()
        };
        let now = minelib_p2p::now();
        let (tx, rx) = std::sync::mpsc::channel();
        let mut asked = 0;
        for ad in ads
            .into_iter()
            .filter(|a| a.noise != me && now.saturating_sub(a.seen) < 3600)
            .take(8)
        {
            asked += 1;
            let tx = tx.clone();
            let pseudo = pseudo.to_string();
            std::thread::spawn(move || {
                let r = node
                    .tunnel(ad.node)
                    .and_then(|a| minelib_net::client::whois(&a.to_string(), ad.noise, &pseudo));
                let _ = tx.send(r.ok().flatten());
            });
        }
        drop(tx);
        let end = Instant::now() + Duration::from_secs(6);
        let mut keys = Vec::new();
        for _ in 0..asked {
            match rx.recv_timeout(end.saturating_duration_since(Instant::now())) {
                Ok(Some(k)) => keys.push(k),
                Ok(None) => {}
                Err(_) => break,
            }
        }
        keys
    })
}

/// Parties des autres relais du DHT (par tunnel iroh), sauf ce relais (`me`). Un fil de
/// fond relit leurs listes toutes les 15 s tant que des joueurs les demandent ; la
/// fonction rendue répond tout de suite avec la dernière liste connue.
pub fn federated_servers(node: &'static minelib_p2p::Node, me: [u8; 32]) -> ServersFn {
    type Lists = HashMap<u8, Vec<proto::ServerEntry>>;
    let lists: Arc<Mutex<Lists>> = Arc::default();
    let asked: Arc<Mutex<Option<Instant>>> = Arc::default();
    let (l2, a2) = (lists.clone(), asked.clone());
    let _ = std::thread::Builder::new()
        .name("relais fédérés".into())
        .spawn(move || {
            let mut ads: (Option<Instant>, Vec<minelib_p2p::Ad>) = (None, Vec::new());
            loop {
                std::thread::sleep(Duration::from_secs(3));
                // Personne ne regarde la liste des serveurs : rien à demander.
                let wanted = a2
                    .lock()
                    .unwrap()
                    .is_some_and(|t| t.elapsed() < Duration::from_secs(60));
                if !wanted {
                    continue;
                }
                if ads.0.is_none_or(|t| t.elapsed() > Duration::from_secs(300)) {
                    if let Ok(list) = minelib_p2p::find_relays() {
                        ads.1 = list;
                    }
                    ads.0 = Some(Instant::now());
                }
                let now = minelib_p2p::now();
                let mut fresh = Lists::new();
                for ad in ads
                    .1
                    .iter()
                    .filter(|a| a.noise != me && now.saturating_sub(a.seen) < 3600)
                    .take(8)
                {
                    let Ok(addr) = node.tunnel(ad.node) else {
                        continue;
                    };
                    for kind in [0u8, 1] {
                        let Ok(list) =
                            minelib_net::client::peer_servers(&addr.to_string(), ad.noise, kind)
                        else {
                            break;
                        };
                        fresh
                            .entry(kind)
                            .or_default()
                            .extend(list.into_iter().map(|mut e| {
                                e.relay_node = ad.node;
                                e.relay_key = ad.noise;
                                e.relay_addr = String::new();
                                e
                            }));
                    }
                }
                *l2.lock().unwrap() = fresh;
                std::thread::sleep(Duration::from_secs(12));
            }
        });
    Arc::new(move |kind: u8| {
        *asked.lock().unwrap() = Some(Instant::now());
        lists
            .lock()
            .unwrap()
            .get(&kind)
            .cloned()
            .unwrap_or_default()
    })
}

/// Clé publique du relais de ce dossier, s'il en a déjà été un (sans le lancer).
pub fn public_key(dir: &Path) -> Option<[u8; 32]> {
    let bytes = std::fs::read(dir.join("relay.key")).ok()?;
    postcard::from_bytes::<Identity>(&bytes)
        .ok()
        .map(|i| i.noise_public)
}

/// Clés du relais, créées au premier lancement.
#[derive(Serialize, Deserialize)]
struct Identity {
    noise_private: [u8; 32],
    noise_public: [u8; 32],
    /// Signe les lignes du registre.
    sign_seed: [u8; 32],
    /// Chiffre les secrets 2FA des comptes.
    vault_key: [u8; 32],
}

impl Identity {
    fn load_or_create(dir: &Path) -> Result<Self> {
        let path = dir.join("relay.key");
        if let Ok(bytes) = std::fs::read(&path) {
            return postcard::from_bytes(&bytes).context("relay.key illisible");
        }
        let (noise_private, noise_public) = transport::generate_static();
        let mut sign_seed = [0u8; 32];
        let mut vault_key = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut sign_seed);
        rand::rngs::OsRng.fill_bytes(&mut vault_key);
        let id = Self {
            noise_private,
            noise_public,
            sign_seed,
            vault_key,
        };
        accounts::write_atomic(&path, &postcard::to_allocvec(&id)?)?;
        log::info!("nouvelles clés du relais dans {}", path.display());
        Ok(id)
    }
}

/// Connexion d'un jeu.
struct Conn {
    /// Dépose un message à envoyer à ce jeu.
    tx: tasks::Outbox,
    pseudo: Option<String>,
    /// Partie hébergée ou rejointe.
    room: Option<u64>,
    /// Capacités annoncées (garant de zones).
    caps: proto::GarantCaps,
    /// Arrivée dans la partie en cours.
    joined: Instant,
    /// Banni pendant la session : la connexion est coupée.
    kicked: bool,
    /// Disponible comme garant indépendant (au menu, ou connexion de garant).
    garant: bool,
    /// Connexion de garant d'un joueur qui joue sa propre partie (deuxième connexion du
    /// même compte, client « minelib-garant ») : elle ne sert que de garant, on ne la
    /// prend jamais pour le joueur lui-même.
    garant_only: bool,
    /// Garant écarté (désaccords rejetés par les joueurs) : plus choisi cette session.
    garant_banned: bool,
    /// Aller-retour jusqu'au relais mesuré par son jeu (ms, moyenne glissante).
    rtt: Option<f32>,
    /// Écart moyen (ms) entre deux mesures : une connexion instable (qui saute) est
    /// écartée comme une connexion lente.
    rtt_dev: f32,
    /// Le même aller-retour mesuré par le relais lui-même (question en cours : numéro et
    /// envoi ; moyenne et écart) : c'est le pire des deux qui compte.
    probe: Option<(u32, Instant)>,
    probed: Option<Instant>,
    probe_rtt: Option<f32>,
    probe_dev: f32,
    /// Joueurs à qui il a parlé (vocal) : dernier moment compté pour la fréquentation.
    talked: HashMap<u64, Instant>,
    /// Demandes d'objets (au sol, coffres) dans la seconde en cours.
    item_rate: (Instant, u32),
    /// Modifications de blocs dans la seconde en cours (voir `EDIT_RATE`), et celles
    /// dues à l'écoulement d'un fluide (`FLUID_RATE`).
    edit_rate: (Instant, u32),
    fluid_rate: (Instant, u32),
    /// Régions dont un relais de région sert les blocs à ce joueur (il le dit lui-même,
    /// `Request::Served`) : ce relais ne les lui envoie pas une deuxième fois.
    served: HashSet<[i32; 2]>,
    /// Garant arbitre d'une affaire de cette partie, en ce moment.
    arbitrating: Option<u64>,
    /// Lobby (n°), et lobbies qui l'ont invité.
    party: Option<u64>,
    invited: Vec<u64>,
    /// Version du jeu (annoncée au bonjour, 0 : inconnue). Une partie FPS se joue entre
    /// jeux de la même version, avec un garant de cette version.
    version: u32,
    /// Empreinte de la machine (connexion au compte) : une seule partie FPS à la fois par
    /// machine.
    hw: Vec<[u8; 32]>,
    /// Connexion venue de la machine de ce relais (pas par un tunnel).
    local: bool,
    /// Clé du compte connue par son certificat (joueur entré par un relais de secours,
    /// inconnu du registre d'ici).
    account: Option<[u8; 32]>,
}

impl Conn {
    /// Une connexion qui vient d'arriver (personne n'y est encore connecté à un compte).
    fn new(tx: impl Into<tasks::Outbox>) -> Self {
        Self {
            tx: tx.into(),
            pseudo: None,
            room: None,
            caps: proto::GarantCaps::default(),
            joined: Instant::now(),
            kicked: false,
            garant: false,
            garant_only: false,
            garant_banned: false,
            rtt: None,
            rtt_dev: 0.0,
            probe: None,
            probed: None,
            probe_rtt: None,
            probe_dev: 0.0,
            talked: HashMap::new(),
            item_rate: (Instant::now(), 0),
            edit_rate: (Instant::now(), 0),
            fluid_rate: (Instant::now(), 0),
            served: HashSet::new(),
            arbitrating: None,
            party: None,
            invited: Vec::new(),
            version: 0,
            hw: Vec::new(),
            local: false,
            account: None,
        }
    }
}

/// Relais de secours d'un monde, vu du relais du monde : l'hôte qui le tient (connexion
/// de garant d'un joueur), et où le joindre une fois prêt.
struct StandbyRef {
    host: u64,
    at: Option<proto::StandbyAt>,
}

/// Ce relais est le relais de secours du monde `room` d'un autre relais.
struct StandbyCopy {
    room: u64,
    /// Clé des certificats de comptes de l'ancien relais du monde.
    root: VerifyingKey,
    info: proto::StandbyInfo,
    /// Copie de l'historique (prise par le monde repris).
    build: Option<zones::Build>,
    /// Le relais du monde est tombé : les joueurs peuvent entrer.
    active: bool,
    /// Le monde repris ici, une fois que le premier joueur y est entré.
    resumed: Option<u64>,
    /// Ses baux, son vote, et les votes des témoins (voir `guards`).
    guard: Option<guards::StandbyGuard>,
    /// Ce qui prouve aux joueurs qu'il a le droit de servir le monde (une fois repris).
    proof: Option<minelib_net::guard::Proof>,
}

/// Région d'un monde servie par son propre relais.
struct RegionShard {
    host: u64,
    /// Rang dans la région (jamais redonné : le plus ancien relais a le plus petit).
    slot: u16,
    info: Option<proto::ShardInfo>,
    /// Hôte qu'il remplace, fermé dès que celui-ci est prêt.
    previous: Option<u64>,
}

/// Époque d'une région souveraine, vue du relais du monde : l'hôte principal met en ordre
/// les modifications de la région et les publie par lots signés ; ce relais n'archive un
/// lot que quand un autre hôte de la région l'a vérifié (`Request::Ack`).
struct RootSov {
    epoch: u64,
    /// Connexion de l'hôte principal, sa clé, son rang.
    primary: u64,
    key: ed25519_dalek::VerifyingKey,
    slot: u16,
    first: u64,
    floor_t: u64,
    /// Archivé : rang suivant et empreinte.
    base: u64,
    chain: [u8; 32],
    /// Lots reçus de l'hôte principal, pas encore confirmés (ils se suivent à partir de
    /// `base`).
    pending: VecDeque<PendingBatch>,
    /// Depuis quand quelque chose attend (lot pas confirmé, modification transmise pas
    /// encore publiée) sans que rien n'avance : au-delà de `SOV_STALL`, l'hôte principal
    /// est remplacé.
    waiting: Option<Instant>,
    /// Modifications arrivées ici (signées par ce relais) transmises à l'hôte principal,
    /// pas encore revenues dans un lot confirmé : à la fin de l'époque, elles repartent
    /// (rien ne se perd) ; l'une d'elles qui tarde trop fait remplacer l'hôte principal
    /// (il ne peut pas en écarter une en silence).
    sent: VecDeque<(Instant, sovereign::Edit)>,
    /// Dernière confirmation de chaque autre hôte de la région : rang et empreinte.
    acks: HashMap<u64, (u64, [u8; 32])>,
}

/// Époque finie : de quoi archiver les lots qu'un hôte remet en passation.
struct PastSov {
    epoch: u64,
    /// Connexion de l'hôte principal de cette époque (écarté si une faute est prouvée
    /// après coup).
    host: u64,
    key: ed25519_dalek::VerifyingKey,
    floor_t: u64,
    base: u64,
    chain: [u8; 32],
}

/// Lot reçu de l'hôte principal d'une région souveraine, pas encore archivé : son rang de
/// fin et son empreinte, calculés une fois à l'arrivée.
struct PendingBatch {
    batch: sovereign::Batch,
    end: u64,
    chain: [u8; 32],
}

/// Surveillance mutuelle (v136) : un lot sur `AUDIT_EVERY` de l'hôte principal, tiré au
/// hasard, est vérifié par l'hôte d'un relais d'une autre région du monde (le relais du
/// monde ne revérifie que ce qui lui est signalé), et une de ces vérifications sur
/// `AUDIT_SELF` aussi par le relais du monde lui-même (un vérificateur qui dirait « rien
/// à signaler » sans regarder finit par être pris).
const AUDIT_EVERY: u32 = 8;
const AUDIT_SELF: u32 = 32;
/// Vérification restée sans réponse au bout de ce temps : comptée contre le vérificateur,
/// qui n'en reçoit plus après `AUDIT_MISSES` oublis.
const AUDIT_WAIT: Duration = Duration::from_secs(10);
const AUDIT_MISSES: u32 = 3;
/// Lots au plus par message `Archive`.
const ARCHIVE_MAX: usize = 256;

/// Vérification d'un lot confiée à un autre hôte.
struct AuditRec {
    auditor: u64,
    region: [i32; 2],
    epoch: u64,
    batch: sovereign::Batch,
    at: Instant,
    /// Vérifié aussi ici : la faute trouvée (ou non).
    here: Option<Option<String>>,
}
/// Une deuxième confirmation qui tarde plus que cela : le relais du monde vérifie lui-même.
const ACK_WAIT: Duration = Duration::from_secs(2);

/// Une époque souveraine qui n'avance plus pendant ce temps (lots pas confirmés,
/// modifications transmises pas publiées) : l'hôte principal est remplacé (sans être
/// puni : il peut être seulement lent ou parti).
const SOV_STALL: Duration = Duration::from_secs(15);
/// Plage de rangs réservée à une époque souveraine.
const SOV_RANGE: u64 = 1 << 32;
/// Une modification signée dans une époque peut dater d'au plus ce temps avant son début.
const SOV_FLOOR_MS: u64 = 60_000;

/// Partie annoncée : l'hôte fait tourner le serveur, le relais transmet les données.
struct Room {
    kind: u8,
    name: String,
    map: String,
    /// 0 : sans limite.
    max_players: u32,
    host: u64,
    host_pseudo: String,
    /// Version du jeu de l'hôte à l'ouverture (FPS : seuls les jeux de cette version
    /// rejoignent).
    version: u32,
    members: Vec<u64>,
    started: bool,
    map_sig: [u8; 32],
    /// Joueurs présents au début de la partie (ceux dont le résultat compte).
    roster: Vec<String>,
    /// Résultats reçus : empreinte → (résultat, joueurs qui l'ont envoyé).
    results: HashMap<[u8; 32], (game::MatchSummary, HashSet<u64>)>,
    applied: bool,
    /// Début de la partie FPS, et absences des joueurs du début (pseudo en minuscules →
    /// parti depuis, secondes d'absence déjà cumulées). Réglées une fois (`settled`).
    started_at: Option<Instant>,
    away: HashMap<String, (Option<Instant>, f64)>,
    settled: bool,
    /// Pause en cours (depuis) et temps de pause déjà écoulé : ni dans la durée de la
    /// partie ni dans les absences.
    paused_since: Option<Instant>,
    paused_total: f64,
    /// Partis pendant la pause : leur absence commence à la reprise.
    paused_away: HashSet<String>,
    /// Places gardées pour les joueurs d'un lobby qui arrivent (pseudo, depuis).
    reserved: Vec<(String, Instant)>,
    /// Signalements de cette partie (de, contre), comptés à la fin de la partie.
    reports: HashSet<(String, String)>,
    /// Joueurs soupçonnés par le vérificateur dans cette partie (pseudos en minuscules).
    suspects: HashSet<String>,
    /// Construction : régions servies par leur propre relais (hôte, relais prêt, hôte
    /// précédent à fermer quand le suivant est prêt), et secret de leurs billets.
    shards: HashMap<[i32; 2], Vec<RegionShard>>,
    /// Rang du prochain relais de région de ce monde.
    shard_slot: u16,
    shard_secret: [u8; 32],
    /// Régions dont la liste des relais vient de changer (à redire aux joueurs qui les
    /// ont en vue), et empreinte de la liste que chaque joueur a reçue.
    shards_dirty: HashSet<[i32; 2]>,
    shards_sent: HashMap<u64, u64>,
    /// Régions souveraines (voir `RootSov`), époques finies (pour une passation), hôtes
    /// écartés du rôle d'hôte principal par région (ils restent relais), prochaine époque.
    sov: HashMap<[i32; 2], RootSov>,
    sov_past: HashMap<[i32; 2], VecDeque<PastSov>>,
    sov_skip: HashMap<[i32; 2], HashSet<u64>>,
    next_epoch: u64,
    /// Dates (uniques) des modifications signées par ce relais déjà archivées : une même
    /// modification arrivée par deux chemins (passation, puis renvoi) ne compte qu'une fois.
    root_done: std::collections::BTreeSet<u64>,
    /// Surveillance mutuelle : vérifications de lots confiées à des hôtes (par numéro),
    /// hôtes prêts qui peuvent en recevoir (connexion, région ; refait à chaque
    /// entretien), oublis de chacun, prochain numéro.
    audits: HashMap<u64, AuditRec>,
    auditors: Vec<(u64, [i32; 2])>,
    audit_misses: HashMap<u64, u32>,
    next_audit: u64,
    /// Joueurs qui doivent recevoir d'ici les blocs d'une région (elle est à portée de vue
    /// et aucun relais de région qu'ils ont rejoint ne la leur sert), par région ; et ces
    /// régions pour chaque joueur. Archiver une modification ne regarde qu'eux (dans un
    /// monde où tout est servi par des relais de région : presque personne).
    needs: HashMap<[i32; 2], HashSet<u64>>,
    needs_of: HashMap<u64, Vec<[i32; 2]>>,
    /// Construction : joueurs qu'aucun relais de région ne sert (ils reçoivent d'ici le
    /// battement deux fois par seconde ; les autres, par leur relais de région).
    unserved: HashSet<u64>,
    /// Passations arrivées pendant une pause (hôte, région, époque, lots) : archivées à la
    /// reprise.
    handovers: Vec<(u64, [i32; 2], u64, Vec<sovereign::Batch>)>,
    /// Construction : joueurs par pseudo (en minuscules), pour la voix adressée par pseudo
    /// (v137), et le pseudo de chacun.
    by_pseudo: HashMap<String, u64>,
    pseudo_of: HashMap<u64, String>,
    /// Construction : garants de zones.
    build: zones::Build,
    /// FPS : garant de secours désigné par l'hôte.
    backup: Option<u64>,
    /// Garants indépendants en attente (membres cachés, pas des joueurs) et prêts.
    hidden: std::collections::BTreeSet<u64>,
    ready_standby: Vec<u64>,
    /// La partie est servie par un garant indépendant (pas par un joueur).
    garant_hosted: bool,
    /// Monde de construction : son relais de secours.
    standby: Option<StandbyRef>,
    /// Monde de construction : ses gardiens, qui l'empêchent de se séparer en deux (voir
    /// `guards`).
    guards: Option<guards::RoomGuards>,
    /// Dernier changement d'hôte pour cause de mauvaise connexion.
    rebalanced: Option<Instant>,
    /// Désaccords : en attente de jugement (n° → joueur jugé, vérificateur), confirmés,
    /// rejetés.
    judges: HashMap<u64, (u64, u64)>,
    confirmed: u32,
    dismissed: u32,
    /// Résultat trouvé par le garant vérificateur (empreinte).
    audited: Option<[u8; 32]>,
    /// Pièces gardées par le relais lui-même pour un arbitrage : ce que le vérificateur
    /// reçoit, dans l'ordre (demande d'un joueur, ou copie entière de la partie par
    /// l'hôte). Les 12 dernières copies.
    evidence: VecDeque<(Option<u64>, Vec<u8>)>,
    evidence_parts: Vec<Vec<u8>>,
    /// Affaire en cours : numéro, arbitre, vérificateur qui accuse, début.
    arbitration: Option<(u64, u64, u64, Instant)>,
    /// Contre-arbitrage en cours : le compte à bannir si le deuxième arbitre confirme.
    ban_review: Option<String>,
    /// Construction : créateur du serveur (OP d'office) et clé de ses OP et bannis.
    creator: String,
    server: String,
    /// Provinces (v138), relais du monde : celles confiées (ou en train de l'être) à un
    /// relais de province, et dernière liste envoyée à chaque joueur (empreinte).
    provinces: HashMap<[i32; 2], provinces::ProvinceRef>,
    provinces_sent: HashMap<u64, u64>,
    /// Les mêmes, prêtes (rangées à part : on les teste à chaque position de joueur).
    delegated: Arc<HashSet<[i32; 2]>>,
    /// Sacs de survie (v139), relais du monde : prêtés à une province (pseudo en minuscules
    /// → province, époque), et ceux qu'une autre province attend (rendus d'abord).
    bag_holders: HashMap<String, ([i32; 2], u64)>,
    bag_waiting: HashMap<String, ([i32; 2], u64)>,
    /// Blocs changés par des joueurs en survie dont le sac est prêté : comptés à son retour.
    bag_deltas: HashMap<String, Vec<u16>>,
    /// Relais de province : cette salle sert une province du monde de même numéro d'un
    /// autre relais (le relais du monde).
    province: Option<provinces::ProvinceRoom>,
}

impl Room {
    /// Régions dont ce joueur doit maintenant recevoir les blocs d'ici (voir `needs`).
    fn set_needs(&mut self, id: u64, want: Vec<[i32; 2]>) {
        let old = self.needs_of.remove(&id).unwrap_or_default();
        for r in old.iter().filter(|r| !want.contains(r)) {
            if let Some(set) = self.needs.get_mut(r) {
                set.remove(&id);
                if set.is_empty() {
                    self.needs.remove(r);
                }
            }
        }
        for r in &want {
            self.needs.entry(*r).or_default().insert(id);
        }
        if !want.is_empty() {
            self.needs_of.insert(id, want);
        }
    }

    /// Un joueur quitte le monde de construction.
    fn build_left(&mut self, id: u64) {
        self.build.left(id);
        self.shards_sent.remove(&id);
        self.set_needs(id, Vec::new());
        self.unserved.remove(&id);
        if let Some(p) = self.pseudo_of.remove(&id) {
            if self.by_pseudo.get(&p) == Some(&id) {
                self.by_pseudo.remove(&p);
            }
        }
    }

    /// Construction : ce joueur porte ce pseudo (voix adressée par pseudo).
    fn name_player(&mut self, id: u64, pseudo: &str) {
        let p = pseudo.to_ascii_lowercase();
        self.by_pseudo.insert(p.clone(), id);
        self.pseudo_of.insert(id, p);
    }

    /// Joueurs de la partie (l'hôte s'il joue, les membres ; pas les garants).
    fn everyone(&self) -> impl Iterator<Item = u64> + '_ {
        let host = (!self.garant_hosted).then_some(self.host);
        host.into_iter().chain(
            self.members
                .iter()
                .copied()
                .filter(|m| !self.hidden.contains(m)),
        )
    }

    fn player_count(&self) -> usize {
        self.everyone().count()
    }

    /// Places gardées (lobby en route), une minute au plus.
    fn reserved_count(&self) -> usize {
        self.reserved
            .iter()
            .filter(|(_, t)| t.elapsed() < Duration::from_secs(60))
            .count()
    }
}

struct State {
    /// Recherche des pseudos dans les autres relais (voir `Handle::set_whois`).
    whois: Arc<Mutex<Option<WhoisFn>>>,
    peer_servers: Arc<Mutex<Option<ServersFn>>>,
    shard_from: Arc<std::sync::atomic::AtomicUsize>,
    shard_share: Arc<std::sync::atomic::AtomicUsize>,
    /// Fautes prouvées d'hôtes principaux, modifications de régions souveraines archivées
    /// (voir `Handle::sov_faults`).
    sov_faults: Arc<std::sync::atomic::AtomicUsize>,
    sov_committed: Arc<std::sync::atomic::AtomicUsize>,
    conns: HashMap<u64, Conn>,
    rooms: HashMap<u64, Room>,
    /// Lobbies : n° → connexions (la première est l'hôte du lobby).
    parties: HashMap<u64, Vec<u64>>,
    next_id: u64,
    /// Dernier passage de l'entretien des garants.
    last_upkeep: Instant,
    cfg: Config,
    id: Identity,
    registry: Registry,
    accounts: accounts::Store,
    limits: limits::Limits,
    stats: stats::Store,
    /// Fréquentation des joueurs entre eux (voir `contacts`).
    contacts: contacts::Contacts,
    maps: maps::Store,
    hardware: hardware::Store,
    servers: servers::Store,
    /// Inventaires des joueurs en survie (voir `bags`).
    bags: bags::Store,
    /// Clé du relais (la même que pour le registre) : certifie les clés des comptes et
    /// signe à la place d'un joueur dans une région souveraine (voir
    /// `minelib_net::sovereign`) ; date de la dernière signature (strictement croissante).
    cert_key: SigningKey,
    root_t: u64,
    /// Surveillance mutuelle : un lot sur `audit_every` vérifié par un autre hôte, une de
    /// ces vérifications sur `audit_self` aussi ici (`AUDIT_EVERY`, `AUDIT_SELF` ; les
    /// essais en changent).
    audit_every: u32,
    audit_self: u32,
    /// Messages de voix transmis par ce relais (essais : la voix doit passer par les relais
    /// de région).
    voice_relayed: usize,
    /// Ce relais tient le relais de secours d'un monde d'un autre relais.
    standby: Option<StandbyCopy>,
    /// Essais : un relais de secours sur la machine du relais du monde (voir
    /// `Handle::set_standby_local`).
    standby_local: bool,
    /// Horloge des gardiens (voir `guards::now_ms`).
    guard_epoch: Instant,
    /// Essais : gardiens coupés de ce relais (pseudos en minuscules), ou tous.
    guard_cut: HashSet<String>,
    guard_isolated: bool,
    /// Provinces (v138), relais du monde : régions par côté, joueurs à partir desquels une
    /// province reçoit son relais (essais : réglables).
    province_size: Arc<std::sync::atomic::AtomicUsize>,
    province_from: Arc<std::sync::atomic::AtomicUsize>,
    /// Ce relais tient le relais d'une province d'un monde d'un autre relais (numéro du
    /// monde, celui de la salle ici).
    province_room: Option<u64>,
    /// Envois de relais de province gardés ici (essais).
    province_archived: usize,
    /// Sacs de survie prêtés à des provinces (essais).
    province_lends: usize,
    /// Dernières vérifications lentes des hôtes de région (voir `SLOW_CHECK`).
    slow_check: Instant,
    /// Positions (changements de chunk) reçues de joueurs de mondes de construction
    /// (essais : au fond d'une province, elles ne passent plus par ici).
    positions_seen: usize,
    /// Dernier index des joueurs (voir `players_index`).
    index_cache: std::cell::RefCell<Option<(Instant, PlayersIndex)>>,
    /// Dernière recherche d'hôte sans hôte libre (voir `HOSTS_WAIT`).
    hosts_wait: Option<Instant>,
    /// Provinces confiées à leur relais de secours (essais).
    province_promotions: usize,
    /// (v142) Joueurs d'un monde à partir desquels la copie de ce que publie une province
    /// est gardée par son relais de secours plutôt qu'ici (essais : réglable).
    copies_until: usize,
    /// (v147) Plage de rangs donnée à un pays à chaque fois (réglable pour les essais) ;
    /// nouvelles plages données ; hôtes que des pays ont choisis eux-mêmes parmi leurs
    /// joueurs (essais).
    pays_seqs: u64,
    pays_refills: usize,
    local_tasks: usize,
    /// (v143) Les provinces confiées par ce relais sont des pays, qui confient eux-mêmes
    /// leurs provinces peuplées (aucun : pas de pays) ; hôtes déjà donnés aux pays (salle,
    /// province, époque, relais de secours).
    nest: Option<proto::Nest>,
    wants_served: HashMap<(u64, [i32; 2], u64, bool), Instant>,
    /// (v148) Étages sous les pays : leurs provinces sont à leur tour des pays (le premier
    /// étage est le leur), et ainsi de suite.
    deeper: Vec<proto::Nest>,
}

/// (v143) `MINELIB_PAYS=taille,joueurs` : les provinces confiées par ce relais sont des pays
/// qui confient eux-mêmes leurs provinces de `taille` régions de côté, peuplées d'au moins
/// `joueurs`.
/// (v148) `MINELIB_PAYS=taille,joueurs;taille,joueurs;…` : plusieurs étages de pays (le
/// premier est celui des provinces confiées par ce relais).
fn parse_nests(v: &str) -> Vec<proto::Nest> {
    v.split(';').map_while(parse_nest).collect()
}

fn parse_nest(v: &str) -> Option<proto::Nest> {
    let mut it = v.split(',').map(|x| x.trim().parse::<i64>().ok());
    let size = it.next()??;
    let from = it
        .next()
        .flatten()
        .unwrap_or(provinces::PROVINCE_FROM as i64);
    (size >= 1 && from >= 1).then_some(proto::Nest {
        size: size as i32,
        from: from as u32,
    })
}

/// Démarre le relais (fil d'écoute en arrière-plan).
pub fn start(cfg: Config) -> Result<Handle> {
    raise_open_files();
    std::fs::create_dir_all(&cfg.dir)
        .with_context(|| format!("dossier du relais {}", cfg.dir.display()))?;
    let id = Identity::load_or_create(&cfg.dir)?;
    let signer = SigningKey::from_bytes(&id.sign_seed);
    let cert_key = signer.clone();
    let registry = Registry::open(&cfg.dir.join("registry.log"), signer)?;
    let accounts = accounts::Store::new(cfg.dir.join("accounts"))?;
    let stats = stats::Store::open(cfg.dir.join("stats"))?;
    let cfg_dir = cfg.dir.clone();
    // Zones endormies d'une session précédente (le relais a été arrêté) : les mondes
    // ouverts alors n'existent plus.
    let _ = std::fs::remove_dir_all(cfg.dir.join("zones"));
    let maps = maps::Store::open(cfg.dir.join("maps"))?;
    let listener =
        TcpListener::bind(&cfg.bind).with_context(|| format!("écoute sur {}", cfg.bind))?;
    let addr = listener.local_addr()?;
    let public = id.noise_public;
    log::info!(
        "relais « {} » sur {addr} — clé {} — {} comptes au registre",
        cfg.name,
        crypto::hex(&public),
        registry.accounts()
    );
    let whois: Arc<Mutex<Option<WhoisFn>>> = Arc::new(Mutex::new(None));
    let peer_servers: Arc<Mutex<Option<ServersFn>>> = Arc::default();
    let shard_from = Arc::new(std::sync::atomic::AtomicUsize::new(SHARD_FROM));
    let shard_share = Arc::new(std::sync::atomic::AtomicUsize::new(SHARD_SHARE));
    let sov_faults = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let sov_committed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let province_size = Arc::new(std::sync::atomic::AtomicUsize::new(
        proto::PROVINCE as usize,
    ));
    let province_from = Arc::new(std::sync::atomic::AtomicUsize::new(
        provinces::PROVINCE_FROM,
    ));
    let state = Arc::new(Mutex::new(State {
        whois: whois.clone(),
        peer_servers: peer_servers.clone(),
        shard_from: shard_from.clone(),
        shard_share: shard_share.clone(),
        sov_faults: sov_faults.clone(),
        sov_committed: sov_committed.clone(),
        conns: HashMap::new(),
        rooms: HashMap::new(),
        parties: HashMap::new(),
        next_id: 1,
        last_upkeep: Instant::now(),
        cfg,
        id,
        registry,
        accounts,
        limits: limits::Limits::default(),
        stats,
        contacts: contacts::Contacts::open(cfg_dir.join("frequentation.bin")),
        maps,
        hardware: hardware::Store::open(cfg_dir.join("hardware.bin")),
        servers: servers::Store::open(cfg_dir.join("serveurs.bin")),
        bags: bags::Store::open(cfg_dir.join("sacs.bin")),
        cert_key,
        root_t: 0,
        audit_every: AUDIT_EVERY,
        audit_self: AUDIT_SELF,
        voice_relayed: 0,
        standby: None,
        standby_local: false,
        guard_epoch: Instant::now(),
        guard_cut: HashSet::new(),
        guard_isolated: false,
        province_size: province_size.clone(),
        province_from: province_from.clone(),
        province_room: None,
        province_archived: 0,
        province_lends: 0,
        slow_check: Instant::now(),
        positions_seen: 0,
        index_cache: std::cell::RefCell::new(None),
        hosts_wait: None,
        province_promotions: 0,
        copies_until: COPIES_UNTIL,
        pays_seqs: provinces::PAYS_SEQS,
        pays_refills: 0,
        local_tasks: 0,
        nest: std::env::var("MINELIB_PAYS")
            .ok()
            .and_then(|v| parse_nests(&v).first().copied()),
        wants_served: HashMap::new(),
        deeper: std::env::var("MINELIB_PAYS")
            .ok()
            .map(|v| parse_nests(&v).into_iter().skip(1).collect())
            .unwrap_or_default(),
    }));
    let state_for_handle = state.clone();
    let state_for_guards = state.clone();
    let killed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let killed_accept = killed.clone();
    let killed_guards = killed.clone();
    // Les connexions sont servies par des tâches, sur quelques fils (voir `tasks`).
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(tasks::workers())
        .thread_name("relais : tâches")
        .enable_all()
        .build()?;
    listener.set_nonblocking(true)?;
    std::thread::Builder::new()
        .name("relais : écoute".into())
        .spawn(move || {
            runtime.block_on(async move {
                // Gardiens des mondes : battements, baux, pause (toutes les 50 ms, même
                // quand personne n'envoie rien).
                tokio::spawn(async move {
                    let mut every = tokio::time::interval(Duration::from_millis(50));
                    every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                    loop {
                        every.tick().await;
                        if killed_guards.load(std::sync::atomic::Ordering::Relaxed) {
                            return;
                        }
                        guards::upkeep(&mut state_for_guards.lock().unwrap());
                    }
                });
                let Ok(listener) = tokio::net::TcpListener::from_std(listener) else {
                    log::error!("relais : écoute impossible");
                    return;
                };
                // Connexions ouvertes par adresse : au-delà de `MAX_PER_PEER`, les
                // suivantes sont refusées (un seul PC ne peut pas épuiser le relais).
                let open: Arc<Mutex<OpenSlots>> = Arc::default();
                loop {
                    let (stream, addr) = match listener.accept().await {
                        Ok(c) => c,
                        Err(_) => {
                            // (Plus de descripteurs, par exemple : on réessaie un peu après.)
                            tokio::time::sleep(Duration::from_millis(20)).await;
                            continue;
                        }
                    };
                    // (Relais tombé, essais : plus rien n'est accepté.)
                    if killed_accept.load(std::sync::atomic::Ordering::Relaxed) {
                        continue;
                    }
                    let state = state.clone();
                    let open = open.clone();
                    tokio::spawn(async move {
                        let ip = addr.ip();
                        let mut peer = ip.to_string();
                        let _slot = match PeerSlot::take(&open, &peer, Some(ip)) {
                            Some(s) => s,
                            None => return,
                        };
                        // Par le tunnel iroh, le joueur arrive de ce PC : le tunnel dit
                        // d'abord qui il est vraiment.
                        if ip.is_loopback() {
                            match tasks::tunnel_peer(&stream).await {
                                Ok(Some(who)) => peer = who,
                                Ok(None) => {}
                                Err(_) => return,
                            }
                        }
                        if let Err(e) = tasks::serve(stream, state, peer.clone()).await {
                            log::debug!("client {peer} : {e:#}");
                        }
                    });
                }
            });
        })?;
    Ok(Handle {
        addr,
        public,
        whois,
        peer_servers,
        shard_from,
        shard_share,
        sov_faults,
        sov_committed,
        province_size,
        province_from,
        state: state_for_handle,
        killed,
    })
}

/// Connexions ouvertes en même temps par une même adresse.
const MAX_PER_PEER: usize = 48;
/// Connexions ouvertes en tout (au-delà, les nouvelles sont refusées). Chaque joueur d'un
/// monde de construction en ouvre deux (la sienne et celle de garant) : 250 000, c'est
/// environ 120 000 joueurs (v136 ; avant, 60 000 connexions, soit 30 000 joueurs). Il faut
/// alors au relais environ 2,5 Go de mémoire.
const MAX_CONNECTIONS: usize = 250_000;

/// Linux, macOS : un programme n'a droit par défaut qu'à 1024 connexions ouvertes (fichiers
/// compris) ; le relais demande tout ce que le système lui permet.
fn raise_open_files() {
    #[cfg(unix)]
    // SAFETY : appels au système avec une structure locale bien formée.
    unsafe {
        let mut lim = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) == 0 && lim.rlim_cur < lim.rlim_max {
            let want = lim.rlim_max.min(MAX_CONNECTIONS as libc::rlim_t + 4096);
            if want > lim.rlim_cur {
                lim.rlim_cur = want;
                if libc::setrlimit(libc::RLIMIT_NOFILE, &lim) != 0 {
                    log::warn!("relais : impossible d'ouvrir plus de connexions à la fois");
                }
            }
        }
    }
}

/// Connexions ouvertes : par adresse (ou par joueur nommé par le tunnel), et en tout
/// (tenu à jour : avant la v136, chaque nouvelle connexion additionnait celles de toutes
/// les adresses).
#[derive(Default)]
pub(crate) struct OpenSlots {
    per: HashMap<String, usize>,
    total: usize,
}

/// Place prise par une connexion (rendue quand elle se ferme).
pub(crate) struct PeerSlot {
    open: Arc<Mutex<OpenSlots>>,
    key: String,
}

impl PeerSlot {
    fn take(
        open: &Arc<Mutex<OpenSlots>>,
        peer: &str,
        ip: Option<std::net::IpAddr>,
    ) -> Option<Self> {
        // Ce PC (tunnel iroh, relais intégré, essais) : pas de limite par adresse, le
        // tunnel a les siennes.
        let local = ip.is_none_or(|a| a.is_loopback());
        Self::take_with(open, peer, if local { usize::MAX } else { MAX_PER_PEER })
    }

    /// Comme `take`, pour un joueur nommé par le tunnel (toujours limité).
    pub(crate) fn take_named(open: &Arc<Mutex<OpenSlots>>, who: &str) -> Option<Self> {
        Self::take_with(open, who, MAX_PER_PEER)
    }

    fn take_with(open: &Arc<Mutex<OpenSlots>>, key: &str, per_max: usize) -> Option<Self> {
        let mut m = open.lock().unwrap();
        if m.total >= MAX_CONNECTIONS || m.per.get(key).is_some_and(|n| *n >= per_max) {
            return None;
        }
        *m.per.entry(key.to_string()).or_insert(0) += 1;
        m.total += 1;
        Some(Self {
            open: open.clone(),
            key: key.to_string(),
        })
    }
}

impl Drop for PeerSlot {
    fn drop(&mut self) {
        let mut m = self.open.lock().unwrap();
        if let Some(n) = m.per.get_mut(&self.key) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                m.per.remove(&self.key);
            }
            m.total = m.total.saturating_sub(1);
        }
    }
}

/// Lit le préambule « MLPX » que met le tunnel iroh devant chaque connexion (voir
/// `minelib_p2p::preamble`). `None` : connexion locale ordinaire, rien n'est consommé.
/// Seules les connexions venues de ce PC sont lues ainsi : un joueur distant ne peut
/// donc pas se faire passer pour un autre.
pub(crate) fn tunnel_peer(stream: &TcpStream) -> std::io::Result<Option<String>> {
    use std::io::Read;
    const MAGIC: &[u8; 4] = minelib_p2p::PREAMBLE;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let start = Instant::now();
    let mut head = [0u8; 4];
    loop {
        let n = stream.peek(&mut head)?;
        if n == 0 || head[..n] != MAGIC[..n] {
            stream.set_read_timeout(None)?;
            return Ok(None);
        }
        if n == 4 {
            break;
        }
        if start.elapsed() > Duration::from_secs(10) {
            return Err(std::io::ErrorKind::TimedOut.into());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let mut r = stream;
    let mut buf = [0u8; 5];
    r.read_exact(&mut buf)?;
    let mut who = vec![0u8; buf[4] as usize];
    r.read_exact(&mut who)?;
    stream.set_read_timeout(None)?;
    let who = String::from_utf8_lossy(&who).into_owned();
    Ok(Some(if who.is_empty() { "iroh:?".into() } else { who }))
}

/// Étape de la session d'un client.
enum Session {
    Anonymous,
    /// Mot de passe vérifié, signature du défi attendue.
    Proving {
        pseudo: String,
        pubkey: [u8; 32],
        challenge: [u8; 32],
    },
    LoggedIn {
        pseudo: String,
    },
    /// Relais de secours : certificat vérifié, signature du défi attendue.
    TakingOver {
        pseudo: String,
        key: [u8; 32],
        challenge: [u8; 32],
    },
    /// (v146) Relais du monde : un joueur revient du fond d'un pays (`Doze`), certificat
    /// vérifié, signature du défi attendue ; il rentre dans la partie `room`.
    Waking {
        pseudo: String,
        key: [u8; 32],
        challenge: [u8; 32],
        room: u64,
    },
    /// Relais de secours : témoin dont le certificat est vérifié, signature du défi
    /// attendue ; puis témoin entré (il dit sa liste et son vote, `WitnessBeat`).
    Witnessing {
        cert: sovereign::Cert,
        challenge: [u8; 32],
    },
    Witness {
        cert: sovereign::Cert,
    },
    /// Relais d'un monde repris : connexion de garant d'un joueur, certificat vérifié,
    /// signature du défi attendue.
    GarantProving {
        pseudo: String,
        key: [u8; 32],
        challenge: [u8; 32],
    },
}

fn err(code: ErrorCode, message: impl Into<String>) -> Response {
    Response::Error {
        code,
        message: message.into(),
    }
}

/// Inscription d'un pseudo qu'un autre relais connu porte déjà, avec une autre clé :
/// refusée (les pseudos sont uniques entre relais). Même clé : c'est le compte qui
/// arrive d'un autre relais, il passe. La question est posée sans bloquer le relais.
fn taken_elsewhere(req: &Request, state: &Mutex<State>) -> Option<Response> {
    let Request::Register { pseudo, pubkey, .. } = req else {
        return None;
    };
    let ask = {
        let st = state.lock().unwrap();
        if !proto::valid_pseudo(pseudo) || st.registry.lookup(pseudo).is_some() {
            return None;
        }
        let f = st.whois.lock().unwrap().clone();
        f
    }?;
    let keys = ask(pseudo);
    keys.iter()
        .any(|k| k != pubkey)
        .then(|| err(ErrorCode::PseudoTaken, "pris sur un autre relais"))
}

/// Le jeu `id` quitte sa partie : ferme celle qu'il héberge, prévient l'hôte sinon.
fn leave_room(st: &mut State, id: u64) {
    let Some(room_id) = st.conns.get_mut(&id).and_then(|c| c.room.take()) else {
        return;
    };
    // (v141 : relais de secours d'une province, ou copie qui la suit.)
    provinces::left(st, room_id, id);
    let me = st.conns.get(&id).and_then(|c| c.pseudo.clone());
    let Some(room) = st.rooms.get_mut(&room_id) else {
        return;
    };
    // Ses numéros de demande d'objets repartiront d'ailleurs à sa prochaine venue.
    if let Some(p) = &me {
        room.build.items.forget(p);
    }
    // Joueur du début qui quitte une partie FPS en cours : absent (il peut revenir).
    if let Some(p) = me {
        let low = p.to_ascii_lowercase();
        let in_roster = room.roster.iter().any(|r| r.eq_ignore_ascii_case(&p));
        if room.kind == 0 && room.started && !room.settled && in_roster {
            let paused = room.paused_since.is_some();
            let e = room.away.entry(low).or_insert((None, 0.0));
            // En pause : l'absence ne compte qu'à la reprise.
            if !paused {
                e.0.get_or_insert_with(Instant::now);
            } else if e.0.is_none() {
                room.paused_away.insert(p.to_ascii_lowercase());
            }
        }
    }
    // Garant en attente (ou garant extérieur d'un monde) qui s'en va : simplement retiré,
    // ses zones passent à un autre.
    if room.hidden.contains(&id) {
        room.hidden.remove(&id);
        room.ready_standby.retain(|&h| h != id);
        room.members.retain(|&m| m != id);
        if room.backup == Some(id) {
            room.backup = None;
        }
        if room.kind == 1 {
            // Hôte d'une région qui s'en va : la région revient à ce relais (ou à l'hôte
            // suivant déjà prêt) ; les joueurs reçoivent la nouvelle liste.
            let mut changed = Vec::new();
            for (region, list) in &mut room.shards {
                for sh in list.iter_mut() {
                    if sh.previous == Some(id) {
                        sh.previous = None;
                    }
                }
                let before = list.len();
                list.retain(|sh| sh.host != id);
                if list.len() != before {
                    changed.push(*region);
                }
            }
            room.shards.retain(|_, list| !list.is_empty());
            room.shards_dirty.extend(changed.iter().copied());
            for region in changed {
                announce_shard_peers(st, room_id, region);
            }
            let room = st.rooms.get_mut(&room_id).expect("partie");
            room.build_left(id);
            return;
        }
        // Plus de vérificateur : le résultat des joueurs peut suffire.
        try_apply_result(st, room_id);
        return;
    }
    // FPS : le garant de secours reprend la partie (sans coupure pour les autres).
    // Construction : le joueur le plus adapté devient propriétaire du monde.
    if room.host == id
        && room
            .members
            .iter()
            .any(|m| !room.hidden.contains(m) || room.kind == 0)
    {
        let kind = room.kind;
        let new = if kind == 1 {
            let cands = candidates(st, room_id);
            cands
                .iter()
                .filter(|c| c.id != id)
                .max_by(|a, b| zones::aptitude(a).total_cmp(&zones::aptitude(b)))
                .map(|c| c.id)
                .unwrap_or(st.rooms[&room_id].members[0])
        } else {
            // Un garant indépendant prêt d'abord, sinon le garant de secours, sinon un
            // joueur.
            // (Sans copie de la partie chez personne : le joueur à la meilleure
            // connexion, jamais une connexion lente ou instable s'il y a mieux.)
            let best_link = room
                .members
                .iter()
                .copied()
                .filter(|&m| m != id)
                .min_by(|a, b| {
                    let ms = |m: &u64| st.conns.get(m).and_then(link_ms).unwrap_or(BAD_RTT - 1.0);
                    ms(a).total_cmp(&ms(b))
                });
            room.ready_standby
                .first()
                .copied()
                .or(room.backup.filter(|b| room.members.contains(b)))
                .or(best_link)
                .unwrap_or(room.members[0])
        };
        switch_host(st, room_id, new, None);
        if kind == 1 {
            if let Some(room) = st.rooms.get_mut(&room_id) {
                room.build_left(id);
            }
        }
        return;
    }
    if room.host == id {
        let mut room = st.rooms.remove(&room_id).expect("partie");
        settle_absences(st, &mut room, &[]);
        for m in room.members {
            if let Some(c) = st.conns.get_mut(&m) {
                c.room = None;
                transport::push(&c.tx, &Response::RoomClosed);
            }
        }
        log::info!("partie {room_id} fermée");
    } else {
        room.members.retain(|&m| m != id);
        let host = room.host;
        if let Some(c) = st.conns.get(&host) {
            transport::push(&c.tx, &Response::PeerLeft { peer: id });
        }
        if room.kind == 1 {
            let room = st.rooms.get_mut(&room_id).expect("partie");
            room.build_left(id);
        }
    }
}

/// `new` devient l'hôte de la partie ; `stays` : l'ancien hôte reste (comme joueur).
fn switch_host(st: &mut State, room_id: u64, new: u64, stays: Option<u64>) {
    let Some(room) = st.rooms.get_mut(&room_id) else {
        return;
    };
    let old = room.host;
    let kind = room.kind;
    // Nouvel hôte : les pièces gardées (copies de l'ancien) ne valent plus.
    room.evidence.clear();
    room.evidence_parts.clear();
    room.members.retain(|&m| m != new);
    let was_hidden = room.hidden.contains(&new);
    room.hidden.remove(&new);
    room.ready_standby.retain(|&h| h != new);
    if let Some(o) = stays {
        if !room.members.contains(&o) {
            room.members.push(o);
        }
    }
    room.host = new;
    room.backup = None;
    room.garant_hosted = was_hidden;
    room.judges.clear();
    room.confirmed = 0;
    room.dismissed = 0;
    let members: Vec<u64> = room
        .members
        .iter()
        .copied()
        .filter(|m| !room.hidden.contains(m))
        .collect();
    let hidden: Vec<u64> = room.hidden.iter().copied().collect();
    let pseudo_of = |c: u64| {
        st.conns
            .get(&c)
            .and_then(|c| c.pseudo.clone())
            .unwrap_or_default()
    };
    let host_pseudo = pseudo_of(new);
    let list: Vec<(u64, String)> = members.iter().map(|&m| (m, pseudo_of(m))).collect();
    if let Some(r) = st.rooms.get_mut(&room_id) {
        r.host_pseudo = host_pseudo.clone();
    }
    if let Some(c) = st.conns.get(&new) {
        transport::push(
            &c.tx,
            &Response::Promoted {
                members: list,
                old_host: stays.map(|_| old),
            },
        );
    }
    if kind == 1 {
        // Le nouveau propriétaire reçoit tout ce qui a été construit (pour l'enregistrer).
        if let Some(room) = st.rooms.get_mut(&room_id) {
            if let Some(c) = st.conns.get(&new) {
                for (zone, log) in room.build.all_logs(new) {
                    let chunk = [zone[0] * proto::ZONE, zone[1] * proto::ZONE];
                    for data in log {
                        push_zone(&c.tx, chunk, data);
                    }
                }
            }
        }
    }
    // Un garant qui reprend la partie reste anonyme pour les joueurs.
    let shown = if was_hidden {
        GARANT_NAME.to_string()
    } else {
        host_pseudo.clone()
    };
    for m in members {
        if let Some(c) = st.conns.get(&m) {
            transport::push(
                &c.tx,
                &Response::HostChanged {
                    host: shown.clone(),
                },
            );
        }
    }
    // Les autres garants en attente : nouvel hôte à qui demander les copies.
    for h in hidden {
        if let Some(c) = st.conns.get(&new) {
            transport::push(&c.tx, &Response::BackupIs { peer: h });
        }
    }
    log::info!("partie {room_id} : « {host_pseudo} » sert maintenant la partie");
}

/// Après un jugement : 2 désaccords confirmés, l'hôte est remplacé par le vérificateur
/// et perd en réputation ; 3 rejetés, le vérificateur est écarté.
fn judge_outcome(st: &mut State, rid: u64, host: u64, auditor: u64) {
    let Some(room) = st.rooms.get(&rid) else {
        return;
    };
    if room.confirmed >= 2 {
        let stays = (!room.garant_hosted).then_some(host);
        let cheater = st.conns.get(&host).and_then(|c| c.pseudo.clone());
        if let Some(p) = &cheater {
            let s = st.stats.get_mut(p);
            s.reports += 2;
            s.trust_by(stats::trust::CHEAT);
            st.stats.save(p);
            log::warn!("partie {rid} : l'hôte « {p} » triche, le vérificateur reprend");
        }
        if let Some(c) = st.conns.get_mut(&host) {
            c.garant_banned = true;
        }
        if let Some(r) = st.rooms.get_mut(&rid) {
            if !r.ready_standby.contains(&auditor) {
                r.ready_standby.push(auditor);
            }
        }
        switch_host(st, rid, auditor, stays);
        // Un garant tricheur quitte la partie (il n'y jouait pas).
        if stays.is_none() {
            if let Some(c) = st.conns.get_mut(&host) {
                c.room = None;
                transport::push(&c.tx, &Response::RoomClosed);
            }
        }
    } else if room.dismissed >= 3 {
        if let Some(c) = st.conns.get_mut(&auditor) {
            c.garant_banned = true;
        }
        leave_room(st, auditor);
        if let Some(c) = st.conns.get(&auditor) {
            transport::push(&c.tx, &Response::RoomClosed);
        }
    }
}

/// Garde une copie de la partie (reçue en morceaux) parmi les pièces ; 12 au plus.
fn keep_evidence(r: &mut Room, part: u16, of: u16, data: Vec<u8>) {
    if part == 0 {
        r.evidence_parts.clear();
    }
    if part as usize != r.evidence_parts.len() || of == 0 {
        r.evidence_parts.clear();
        return;
    }
    r.evidence_parts.push(data);
    if r.evidence_parts.len() < of as usize {
        return;
    }
    let whole = std::mem::take(&mut r.evidence_parts).concat();
    r.evidence.push_back((None, whole));
    let copies = |r: &Room| r.evidence.iter().filter(|e| e.0.is_none()).count();
    while copies(r) > 12 {
        // Jusqu'à la plus vieille copie comprise.
        while let Some((who, _)) = r.evidence.pop_front() {
            if who.is_none() {
                break;
            }
        }
    }
}

/// Lance un arbitrage : choisit un troisième garant sans lien avec la partie (ni avec
/// l'hôte, ni avec le vérificateur) et lui envoie les pièces. Faux s'il n'y en a pas.
fn arbitrate(st: &mut State, rid: u64, auditor: u64) -> bool {
    arbitrate_apart(st, rid, auditor, None)
}

/// `first` : l'arbitre qui a déjà jugé l'affaire (contre-arbitrage avant un bannissement :
/// un deuxième arbitre, sans lien avec le premier ni avec le vérificateur).
fn arbitrate_apart(st: &mut State, rid: u64, auditor: u64, first: Option<u64>) -> bool {
    let Some(room) = st.rooms.get(&rid) else {
        return false;
    };
    let mut people: Vec<String> = room
        .everyone()
        .chain([room.host, auditor])
        .chain(first)
        .filter_map(|m| st.conns.get(&m).and_then(|c| c.pseudo.clone()))
        .collect();
    people.extend(room.roster.iter().cloned());
    let apart: Vec<u64> = std::iter::once(auditor).chain(first).collect();
    let Some(arbiter) = pick_garant_apart(st, &people, &apart) else {
        return false;
    };
    let case = st.next_id;
    st.next_id += 1;
    let items: Vec<(Option<u64>, Vec<u8>)> = room.evidence.iter().cloned().collect();
    let bytes = postcard::to_allocvec(&items).unwrap_or_default();
    let (map_sig, map) = (room.map_sig, room.map.clone());
    let Some(c) = st.conns.get_mut(&arbiter) else {
        return false;
    };
    c.arbitrating = Some(rid);
    transport::push(&c.tx, &Response::Arbitrate { case, map_sig, map });
    let parts: Vec<&[u8]> = bytes.chunks(proto::MAP_PART).collect();
    let of = parts.len() as u16;
    for (i, p) in parts.into_iter().enumerate() {
        transport::push(
            &c.tx,
            &Response::Evidence {
                case,
                part: i as u16,
                of,
                data: p.to_vec(),
            },
        );
    }
    log::info!("partie {rid} : arbitrage n° {case} confié à un troisième garant");
    if let Some(r) = st.rooms.get_mut(&rid) {
        r.arbitration = Some((case, arbiter, auditor, Instant::now()));
    }
    true
}

/// Fin d'un arbitrage : `guilty` (l'hôte a triché : le vérificateur reprend la partie),
/// innocent (le vérificateur accusait à tort : il est écarté), ou pas de réponse (`None` :
/// on retombe sur la règle sans arbitre, deux preuves du vérificateur).
fn arbitration_done(st: &mut State, rid: u64, guilty: Option<bool>) {
    let Some(room) = st.rooms.get_mut(&rid) else {
        return;
    };
    let Some((case, arbiter, auditor, _)) = room.arbitration.take() else {
        return;
    };
    // Contre-arbitrage (voir plus bas) : seul le bannissement en dépend, la partie a déjà
    // suivi le premier arbitre.
    if let Some(p) = room.ban_review.take() {
        if let Some(c) = st.conns.get_mut(&arbiter) {
            c.arbitrating = None;
        }
        let why = format!("triche constatée par deux arbitres (partie {rid}, n° {case})");
        match guilty {
            Some(true) => match ban_cheater(st, &p, &why) {
                Ok(n) => log::warn!("« {p} » banni automatiquement ({n} machine(s)) : {why}"),
                Err(e) => log::warn!("ban automatique de « {p} » impossible : {e}"),
            },
            _ => log::warn!(
                "partie {rid} : le deuxième arbitre ne confirme pas la triche de « {p} » \
                 ({guilty:?}) : pas de bannissement automatique"
            ),
        }
        return;
    }
    let host = room.host;
    let host_pseudo = (!room.garant_hosted || guilty == Some(true))
        .then(|| st.conns.get(&host).and_then(|c| c.pseudo.clone()))
        .flatten();
    let room = st.rooms.get_mut(&rid).expect("partie");
    match guilty {
        Some(true) => room.confirmed = 2,
        Some(false) => {
            room.confirmed = 0;
            room.dismissed = 3;
        }
        None => room.confirmed += 1,
    }
    log::info!("partie {rid} : arbitrage n° {case} : {guilty:?}");
    if let Some(c) = st.conns.get_mut(&arbiter) {
        c.arbitrating = None;
    }
    if room_has(st, rid, auditor) {
        judge_outcome(st, rid, host, auditor);
    }
    // Triche sûre (le vérificateur et un arbitre sans lien la constatent, en rejouant la
    // partie) : bannissement automatique du compte et de ses machines.
    if let Some(r) = st.rooms.get_mut(&rid) {
        r.confirmed = r.confirmed.min(1);
        r.dismissed = 0;
    }
    // Triche constatée par le vérificateur et un arbitre : la partie change d'hôte tout de
    // suite, mais le bannissement (à vie, machines comprises) attend un deuxième arbitre,
    // sur une autre machine et sans lien avec les deux premiers. Deux comptes complices ne
    // suffisent plus à faire bannir un innocent ; sans deuxième arbitre disponible, pas de
    // bannissement automatique (un administrateur peut toujours bannir).
    if guilty == Some(true) {
        if let Some(p) = host_pseudo {
            let second = arbitrate_apart(st, rid, auditor, Some(arbiter));
            match st.rooms.get_mut(&rid) {
                Some(r) if second => r.ban_review = Some(p),
                _ => log::warn!(
                    "partie {rid} : triche de « {p} » constatée par un arbitre (n° {case}), \
                     pas de deuxième arbitre disponible : pas de bannissement automatique"
                ),
            }
        }
    }
}

fn room_has(st: &State, rid: u64, who: u64) -> bool {
    st.rooms
        .get(&rid)
        .is_some_and(|r| r.hidden.contains(&who) || r.members.contains(&who))
}

/// Ce garant peut servir une partie de ces joueurs : disponible, au menu, au moins 3
/// parties jouées, peu signalé, aucun lien avec eux (amis, signalements).
fn garant_ok(st: &State, g: u64, players: &[String]) -> bool {
    let Some(c) = st.conns.get(&g) else {
        return false;
    };
    let Some(p) = c.pseudo.as_deref() else {
        return false;
    };
    if !c.garant || c.garant_banned || c.room.is_some() || c.kicked || c.arbitrating.is_some() {
        return false;
    }
    // Connexion lente ou instable : jamais garant (toute la partie passerait par elle).
    if link_bad(c) {
        return false;
    }
    let me = st.stats.get(p).cloned().unwrap_or_default();
    if me.games < 3 || me.ratio() > 0.25 {
        return false;
    }
    // Les autres comptes de sa machine comptent comme lui : un compte secondaire ne sert
    // pas à garantir la partie du compte principal, ni celle de ses fréquentations.
    let mine = if machines_apart(st) {
        st.hardware.siblings(p)
    } else {
        Vec::new()
    };
    players.iter().all(|q| {
        !q.eq_ignore_ascii_case(p)
            && !mine.iter().any(|m| {
                m.eq_ignore_ascii_case(q)
                    || st.contacts.acquainted(m, q)
                    || st
                        .stats
                        .get(m)
                        .is_some_and(|s| s.friends.iter().any(|f| f.eq_ignore_ascii_case(q)))
            })
            // Ils se fréquentent (se parlent, jouent ou traînent ensemble) : pas garant.
            && !st.contacts.acquainted(p, q)
            && !me.avoids(q)
            && !me.friends.iter().any(|f| f.eq_ignore_ascii_case(q))
            && st.stats.get(q).is_none_or(|s| {
                !s.avoids(p) && !s.friends.iter().any(|f| f.eq_ignore_ascii_case(p))
            })
    })
}

/// Garant pris parmi les joueurs qui viennent de lancer leur propre partie : ils ont tout
/// intérêt à rester en jeu pendant qu'ils servent. Au-delà, ils sont remplacés (leur partie
/// approche de sa fin).
const GARANT_FRESH: Duration = Duration::from_secs(6 * 60);
const GARANT_RETIRE: Duration = Duration::from_secs(25 * 60);

/// Depuis quand le joueur de ce pseudo joue sa propre partie FPS (sur sa connexion de
/// joueur), s'il en joue une.
fn own_game_age(st: &State, pseudo: &str) -> Option<Duration> {
    st.conns
        .values()
        .filter(|c| !c.garant_only)
        .filter(|c| {
            c.pseudo
                .as_deref()
                .is_some_and(|p| p.eq_ignore_ascii_case(pseudo))
        })
        .filter_map(|c| st.rooms.get(&c.room?))
        .filter(|r| r.kind == 0)
        .filter_map(|r| r.started_at)
        .map(|t| t.elapsed())
        .min()
}

/// Ce garant doit-il passer la main (sa propre partie touche à sa fin, ou il n'y est plus) ?
fn garant_retiring(st: &State, g: u64) -> bool {
    let Some(c) = st.conns.get(&g) else {
        return false;
    };
    if !c.garant_only {
        return false;
    }
    let p = c.pseudo.as_deref().unwrap_or_default();
    own_game_age(st, p).is_none_or(|a| a > GARANT_RETIRE)
}

/// Le garant disponible le plus adapté pour ces joueurs : d'abord un joueur qui vient de
/// lancer sa propre partie (connexion de garant), sinon un joueur au menu.
fn pick_garant(st: &State, players: &[String]) -> Option<u64> {
    pick_garant_apart(st, players, &[])
}

/// Deux connexions viennent-elles de la même machine (un composant en commun) ?
fn same_machine(a: &Conn, b: &Conn) -> bool {
    minelib_net::hw::same_machine(&a.hw, &b.hw)
}

/// Les règles « pas sur la même machine » (garants, arbitres) s'appliquent-elles ? Pas sur
/// un relais qui n'écoute que cette machine (essais, jeu seul) : tout le monde y est sur
/// la même.
fn machines_apart(st: &State) -> bool {
    !st.cfg.bind.starts_with("127.")
}

/// Comme `pick_garant`, sur une autre machine que les connexions `apart` (arbitres : deux
/// comptes d'une même personne ne doivent pas pouvoir s'accuser et se confirmer).
fn pick_garant_apart(st: &State, players: &[String], apart: &[u64]) -> Option<u64> {
    // Un garant rejoue la partie : il doit avoir la même version du jeu.
    let want = players_version(st, players);
    st.conns
        .iter()
        .filter(|(_, c)| c.version == want)
        .filter(|(_, c)| {
            !machines_apart(st)
                || !apart
                    .iter()
                    .filter_map(|o| st.conns.get(o))
                    .any(|o| same_machine(c, o))
        })
        .filter(|(&g, _)| garant_ok(st, g, players))
        .filter_map(|(&g, c)| {
            let cand = zones::Candidate {
                id: g,
                caps: c.caps,
                uptime: c.joined.elapsed(),
                ratio: c.pseudo.as_deref().map_or(0.0, |p| st.stats.ratio(p)),
                rtt: link_ms(c),
            };
            let mut score = zones::aptitude(&cand);
            if c.garant_only {
                let p = c.pseudo.as_deref().unwrap_or_default();
                match own_game_age(st, p) {
                    Some(a) if a < GARANT_FRESH => score += 1_000_000.0,
                    // Partie déjà avancée (ou finie) : il faudrait le remplacer bientôt.
                    _ => return None,
                }
            }
            Some((g, score))
        })
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(g, _)| g)
}

/// Relève les garants dont la propre partie approche de sa fin : le garant en attente
/// (prêt) reprend la partie servie avant que l'hôte ne s'en aille ; un garant en attente
/// qui doit partir est simplement remplacé.
fn relieve_garants(st: &mut State, rid: u64) {
    let Some(room) = st.rooms.get(&rid) else {
        return;
    };
    let host = room.host;
    let retiring_host = room.garant_hosted && garant_retiring(st, host);
    let ready = room
        .ready_standby
        .iter()
        .copied()
        .find(|&g| !garant_retiring(st, g));
    let leaving: Vec<u64> = room
        .hidden
        .iter()
        .copied()
        .filter(|&g| garant_retiring(st, g))
        .collect();
    let mut out = leaving;
    if retiring_host && ready.is_some() {
        out.push(host);
    }
    for g in out {
        log::info!("partie {rid} : le garant {g} passe la main (sa partie se termine)");
        leave_room(st, g);
        if let Some(c) = st.conns.get(&g) {
            transport::push(&c.tx, &Response::RoomClosed);
        }
    }
}

/// Modifications en masse au plus par demande (WorldEdit d'un OP).
const BULK_MAX: usize = 8192;

/// WorldEdit d'un OP dans un monde de construction : chaque bloc est publié comme un bloc
/// validé (historique des zones, joueurs proches, copie du relais de région s'il y en a
/// un). Renvoie pourquoi c'est refusé (vide : publié).
fn bulk_edit(
    st: &mut State,
    my_room: Option<u64>,
    id: u64,
    pseudo: &str,
    cells: Vec<([i32; 3], u16)>,
) -> String {
    let Some(rid) = my_room.filter(|r| st.rooms.get(r).is_some_and(|r| r.kind == 1)) else {
        return "Seulement sur un serveur de construction.".into();
    };
    let (server, creator) = {
        let r = &st.rooms[&rid];
        (r.server.clone(), r.creator.clone())
    };
    if !st.servers.is_op(&server, &creator, pseudo) {
        return "WorldEdit est réservé aux OP de ce serveur.".into();
    }
    if cells.len() > BULK_MAX {
        return "Trop de blocs d'un coup.".into();
    }
    // (Provinces, v139 : les cases d'une province servie par un relais de province lui
    // partent, signées ici à la place de l'OP ; elle les publie.)
    let cells = provinces::bulk_split(st, rid, pseudo, cells);
    let mut touched = Vec::new();
    for (pos, block) in cells {
        if !(0..256).contains(&pos[1]) {
            continue;
        }
        // (Son auteur les a déjà : pas de confirmation, la réponse `BulkDone` suffit.)
        if publish_block(st, rid, id, false, pseudo, pos, block, false) {
            touched.push(pos);
        }
    }
    // Un coffre remplacé par WorldEdit : son contenu tombe au sol.
    for pos in touched {
        spill_chest(st, rid, pos);
    }
    String::new()
}

/// Auditeurs d'un message vocal, au plus.
const VOICE_MAX: usize = 256;
/// Taille au plus d'un message de voix (v137 : par pseudo).
const VOICE_BYTES: usize = 4096;

/// Demandes d'objets (au sol, coffres) acceptées par joueur et par seconde.
const ITEM_RATE: u32 = 40;

/// Modifications de blocs acceptées par joueur et par seconde ; celles que le jeu dit dues
/// à l'écoulement d'un fluide ont un budget à part, plus large (une inondation change
/// beaucoup de cases d'un coup).
const EDIT_RATE: u32 = 20;
const FLUID_RATE: u32 = 400;

/// Remet à un joueur, ou à l'hôte d'un relais de région, une entrée de l'historique d'une
/// zone. Un bloc changé part regroupé avec ceux qui le suivent de près, dans un paquet
/// compact (`transport::push_cell`) : dans une foule qui construit, c'est six fois moins
/// d'octets qu'un message par bloc. Le reste (objets, coffres) part tel quel.
pub(crate) fn push_zone(tx: &impl transport::Sink, chunk: [i32; 2], data: Vec<u8>) {
    match game::cell_of(&data) {
        Some(cell) => {
            transport::push_cell(tx, cell);
        }
        None => {
            transport::push(
                tx,
                &Response::FromZone {
                    peer: 0,
                    chunk,
                    data,
                },
            );
        }
    }
}

/// Région d'une zone.
fn zone_region(zone: [i32; 2]) -> [i32; 2] {
    proto::region_of([zone[0] * proto::ZONE, zone[1] * proto::ZONE])
}

/// Régions dont un relais de région sert vraiment les blocs à ce joueur : celles qu'il
/// annonce, si elles ont un relais prêt (un relais parti ne prive personne).
fn served_regions(st: &State, rid: u64, id: u64) -> HashSet<[i32; 2]> {
    let (Some(room), Some(c)) = (st.rooms.get(&rid), st.conns.get(&id)) else {
        return HashSet::new();
    };
    c.served
        .iter()
        .copied()
        .filter(|r| {
            room.shards
                .get(r)
                .is_some_and(|l| l.iter().any(|s| s.info.is_some()))
        })
        .collect()
}

/// Range ce joueur selon les régions dont il doit recevoir les blocs d'ici (voir
/// `Room::needs`) : après chaque changement de position ou de régions servies.
fn refresh_needs(st: &mut State, rid: u64, id: u64) {
    let served = st
        .conns
        .get(&id)
        .map(|c| c.served.clone())
        .unwrap_or_default();
    // (Provinces servies par un relais de province, v138 : leurs blocs viennent de lui.)
    let delegated = provinces::delegated_set(st, rid);
    let Some(room) = st.rooms.get_mut(&rid) else {
        return;
    };
    let mut want = Vec::new();
    if let Some(&c) = room.build.positions.get(&id) {
        let lo = proto::region_of([c[0] - zones::NEAR, c[1] - zones::NEAR]);
        let hi = proto::region_of([c[0] + zones::NEAR, c[1] + zones::NEAR]);
        for rx in lo[0]..=hi[0] {
            for rz in lo[1]..=hi[1] {
                if !served.contains(&[rx, rz]) && !provinces::in_set(&delegated, [rx, rz]) {
                    want.push([rx, rz]);
                }
            }
        }
    }
    room.set_needs(id, want);
}

/// Envoie à un joueur ce qui lui manque des zones proches de `chunk` (les blocs des
/// régions qu'un relais de région lui sert n'en font pas partie).
fn send_catch_up(st: &mut State, rid: u64, id: u64, chunk: [i32; 2], moved: bool) {
    let served = served_regions(st, rid, id);
    let delegated = provinces::delegated_set(st, rid);
    let Some(room) = st.rooms.get_mut(&rid) else {
        return;
    };
    let blocks = |zone: [i32; 2]| {
        !served.contains(&zone_region(zone)) && !provinces::in_set(&delegated, zone_region(zone))
    };
    let logs = if moved {
        room.build.moved(id, chunk, &blocks)
    } else {
        room.build.catch_up(id, Some(chunk), &blocks)
    };
    let Some(c) = st.conns.get(&id) else { return };
    for (zone, log) in logs {
        let chunk = [zone[0] * proto::ZONE, zone[1] * proto::ZONE];
        for data in log {
            push_zone(&c.tx, chunk, data);
        }
    }
}

/// Ce relais publie le bloc `block` en `pos`, au nom de `pseudo` (joueur `author`). Lui
/// seul écrit l'histoire des blocs d'un monde, comme celle des objets : il donne à la
/// modification son rang, la garde, et la remet
/// - à son auteur (c'est sa confirmation) et aux joueurs à portée qu'aucun relais de
///   région ne sert ;
/// - aux hôtes des relais de la région, qui la transmettent à leurs joueurs.
///
/// Avant la v127, le jeu d'un joueur décidait : le garant de la zone validait et disait
/// qui avait posé le bloc, et dans une région peuplée le relais de région (hébergé par un
/// joueur) mettait les modifications en ordre et en rendait compte. L'un comme l'autre
/// pouvait inventer une modification ou l'attribuer à quelqu'un d'autre (crédit de minage,
/// sac vidé en survie). Ils ne peuvent plus que tarder à transmettre.
///
/// `confirm` : l'auteur la reçoit aussi (sa confirmation).
///
/// Dans une région souveraine, c'est son hôte principal qui met en ordre : la modification
/// lui est transmise, signée par ce relais au nom de son auteur, et sera archivée (et
/// comptée dans le sac de son auteur) quand elle reviendra dans un lot confirmé. Renvoie
/// vrai si elle a été publiée ici.
#[allow(clippy::too_many_arguments)]
fn publish_block(
    st: &mut State,
    rid: u64,
    author: u64,
    confirm: bool,
    pseudo: &str,
    pos: [i32; 3],
    block: u16,
    fluid: bool,
) -> bool {
    let chunk = [pos[0].div_euclid(16), pos[2].div_euclid(16)];
    let region = proto::region_of(chunk);
    let Some(room) = st.rooms.get_mut(&rid) else {
        return false;
    };
    if let Some(sv) = room.sov.get_mut(&region) {
        st.root_t = (st.root_t + 1).max(now_ms());
        let edit = sovereign::Edit::sign(
            &st.cert_key,
            rid,
            pseudo,
            pos,
            block,
            fluid,
            chunk,
            st.root_t,
            true,
        );
        // (Trop de modifications en attente : l'hôte principal ne suit plus, il va être
        // remplacé ; celle-ci est refusée.)
        if sv.sent.len() >= 4096 {
            return false;
        }
        sv.waiting.get_or_insert_with(Instant::now);
        sv.sent.push_back((Instant::now(), edit.clone()));
        if let Some(c) = st.conns.get(&sv.primary) {
            transport::push(
                &c.tx,
                &Response::RootEdits {
                    region,
                    edits: vec![edit],
                    certs: Vec::new(),
                },
            );
        }
        return false;
    }
    let sharded = room
        .shards
        .get(&region)
        .is_some_and(|l| l.iter().any(|s| s.info.is_some()));
    let conns = &st.conns;
    let served = |m: &u64| sharded && conns.get(m).is_some_and(|c| c.served.contains(&region));
    let mut to: Vec<u64> = Vec::new();
    if confirm && !served(&author) {
        to.push(author);
    }
    to.extend(
        room.build
            .listeners(chunk, author)
            .into_iter()
            .filter(|m| !served(m)),
    );
    let (data, sends) = room.build.publish(pos, block, pseudo, &to);
    // L'auteur servi par un relais de région : sa confirmation lui arrive quand même
    // d'ici, tout de suite (le relais de région la lui remettra aussi : sans effet).
    if confirm && !sends.iter().any(|(m, _)| *m == author) {
        if let Some(c) = st.conns.get(&author) {
            push_zone(&c.tx, chunk, data.clone());
        }
    }
    for (m, items) in sends {
        if let Some(c) = st.conns.get(&m) {
            for data in items {
                push_zone(&c.tx, chunk, data);
            }
        }
    }
    // Aux relais de la région, prêts ou pas encore (ils ont reçu l'historique à leur
    // création : la suite leur arrive dans l'ordre, rien ne manque).
    for sh in room.shards.get(&region).into_iter().flatten() {
        if let Some(c) = st.conns.get(&sh.host) {
            push_zone(&c.tx, chunk, data.clone());
        }
    }
    true
}

/// Un joueur demande à changer un bloc. Ce relais vérifie ce qu'il peut vérifier sans
/// connaître le terrain : la demande est bien formée, à portée de vue de son auteur, et il
/// n'en fait pas trop (`EDIT_RATE`, `FLUID_RATE`). Refusée : le jeu remet le bloc d'avant.
#[allow(clippy::too_many_arguments)]
fn build_edit(
    st: &mut State,
    rid: u64,
    id: u64,
    pseudo: &str,
    chunk: [i32; 2],
    pos: [i32; 3],
    block: u16,
    fluid: bool,
) {
    let well_formed =
        [pos[0].div_euclid(16), pos[2].div_euclid(16)] == chunk && (0..256).contains(&pos[1]);
    let near = st
        .rooms
        .get(&rid)
        .and_then(|r| r.build.positions.get(&id))
        .is_some_and(|h| {
            (h[0] - chunk[0]).abs() <= zones::NEAR && (h[1] - chunk[1]).abs() <= zones::NEAR
        });
    let calm = st.conns.get_mut(&id).is_some_and(|c| {
        let (r, max) = if fluid {
            (&mut c.fluid_rate, FLUID_RATE)
        } else {
            (&mut c.edit_rate, EDIT_RATE)
        };
        if r.0.elapsed() >= Duration::from_secs(1) {
            *r = (Instant::now(), 0);
        }
        r.1 += 1;
        r.1 <= max
    });
    if !(well_formed && near && calm) {
        if let Some(c) = st.conns.get(&id) {
            transport::push(
                &c.tx,
                &Response::FromZone {
                    peer: 0,
                    chunk,
                    data: game::encode(&game::BuildMsg::Reject { pos, peer: id }),
                },
            );
        }
        return;
    }
    // Ce qui était là d'après l'historique (inconnu si la case n'a jamais été changée).
    let before = st.rooms.get_mut(&rid).and_then(|r| r.build.block_at(pos));
    if !publish_block(st, rid, id, true, pseudo, pos, block, fluid) {
        // (Région souveraine : comptée quand elle revient dans un lot confirmé.)
        return;
    }
    // Survie : un bloc cassé donne un crédit de minage, un bloc posé sort du sac. Pas pour
    // l'écoulement d'un fluide (de l'eau qui sèche n'est pas du minage), ni pour une case
    // que l'historique dit déjà vide (« casser » dix fois le même vide).
    if !fluid && !(block == 0 && before == Some(0)) {
        note_block(st, rid, pseudo, block);
    }
    spill_chest(st, rid, pos);
}

/// Demande d'objet d'un joueur (lâcher, ramasser, ranger, prendre) : ce relais arbitre
/// (une demande après l'autre), publie le résultat aux joueurs proches, ou refuse au seul
/// demandeur. (Relais du monde, ou relais de province pour les objets de sa province.)
fn item_request(st: &mut State, rid: u64, id: u64, pseudo: &str, m: &game::BuildMsg) {
    let Some(room) = st.rooms.get(&rid) else {
        return;
    };
    // Seulement près de soi (2 chunks) : on ne lâche, ne ramasse ni ne fouille
    // à l'autre bout du monde ; et pas plus de 40 demandes par seconde.
    let near = match (
        room.build.positions.get(&id),
        room.build.items.target_chunk(m),
    ) {
        (Some(h), Some(t)) => (h[0] - t[0]).abs() <= 2 && (h[1] - t[1]).abs() <= 2,
        (_, None) => true,
        (None, _) => false,
    };
    let calm = st.conns.get_mut(&id).is_some_and(|c| {
        if c.item_rate.0.elapsed() >= Duration::from_secs(1) {
            c.item_rate = (Instant::now(), 0);
        }
        c.item_rate.1 += 1;
        c.item_rate.1 <= ITEM_RATE
    });
    // En survie, le relais tient l'inventaire : ce que la demande sort du sac
    // (lâcher, ranger) doit y être ; ce qu'elle rapporte y entre.
    let (server, creator) = (room.server.clone(), room.creator.clone());
    let items = &room.build.items;
    let fresh = !items.is_duplicate(m, pseudo);
    let (spend, gain) = match *m {
        game::BuildMsg::DropItem { pile, .. } | game::BuildMsg::ChestPut { pile, .. } => {
            (Some(pile), None)
        }
        game::BuildMsg::TakeItem { id: item, .. } => (
            None,
            items
                .live_drops()
                .find(|(i, _)| *i == item)
                .map(|(_, d)| d.pile),
        ),
        game::BuildMsg::ChestTake {
            pos, slot, count, ..
        } => (
            None,
            items
                .chest(pos)
                .get(slot as usize)
                .copied()
                .flatten()
                .map(|p| minelib_net::items::Pile {
                    item: p.item,
                    count: p.count.min(count),
                }),
        ),
        _ => (None, None),
    };
    let survival = fresh && in_survival(st, &server, &creator, pseudo);
    let broke = survival
        && spend.is_some_and(|p| !st.bags.can_spend(&server, pseudo, p.item, p.count as u32));
    let room = st.rooms.get_mut(&rid).expect("partie");
    let out = if !near || !calm || broke {
        let seq = minelib_net::items::request_seq(m).unwrap_or(0);
        vec![game::BuildMsg::Denied { seq }]
    } else {
        match room.build.items.apply(m, pseudo) {
            Ok(v) => {
                if survival && !v.is_empty() {
                    if let Some(p) = spend {
                        st.bags.spend(&server, pseudo, p.item, p.count as u32);
                    }
                    if let Some(p) = gain {
                        st.bags.gain(&server, pseudo, p.item, p.count as u32);
                    }
                }
                v
            }
            Err(seq) => vec![game::BuildMsg::Denied { seq }],
        }
    };
    publish_items(st, rid, id, out);
}

/// Publie des états d'objets (voir `minelib_net::items`) : gardés dans l'historique de
/// leur zone et remis aux joueurs proches, le demandeur `asker` en premier (c'est lui qui
/// attend la réponse) ; un refus ne va qu'à lui.
fn publish_items(st: &mut State, rid: u64, asker: u64, msgs: Vec<game::BuildMsg>) {
    for m in msgs {
        let data = game::encode(&m);
        if matches!(m, game::BuildMsg::Denied { .. }) {
            if let Some(c) = st.conns.get(&asker) {
                transport::push(
                    &c.tx,
                    &Response::FromZone {
                        peer: 0,
                        chunk: [0, 0],
                        data,
                    },
                );
            }
            continue;
        }
        let Some(chunk) = minelib_net::items::chunk_of_msg(&m) else {
            continue;
        };
        let Some(room) = st.rooms.get_mut(&rid) else {
            return;
        };
        // (`asker` 0 : personne n'attend, coffre cassé ou sauvegarde remise.)
        let mut to: Vec<u64> = (asker != 0).then_some(asker).into_iter().collect();
        to.extend(room.build.listeners(chunk, asker));
        let sends = room.build.mirror_to(proto::zone_of(chunk), &data, &to);
        // Le demandeur, même loin de là (ou pas encore placé) : sa réponse lui arrive.
        let direct = asker != 0 && !sends.iter().any(|(m, _)| *m == asker);
        for (m, items) in sends {
            if let Some(c) = st.conns.get(&m) {
                for data in items {
                    push_zone(&c.tx, chunk, data);
                }
            }
        }
        if direct {
            if let Some(c) = st.conns.get(&asker) {
                push_zone(&c.tx, chunk, data);
            }
        }
    }
}

/// Le bloc en `pos` vient de changer : si c'était un coffre garni, son contenu tombe au
/// sol (une seule fois).
fn spill_chest(st: &mut State, rid: u64, pos: [i32; 3]) {
    let Some(room) = st.rooms.get_mut(&rid) else {
        return;
    };
    let out = room.build.items.spill(pos, rand::random::<u64>);
    if !out.is_empty() {
        publish_items(st, rid, 0, out);
    }
}

/// Aller-retour (ms) au-delà duquel l'hôte d'une partie a une mauvaise connexion.
const BAD_RTT: f32 = 180.0;

/// Qualité d'une connexion pour servir les autres (hôte, garant) : son aller-retour moyen
/// plus deux fois son écart moyen, en ms. Une connexion qui saute (Wi-Fi chargé, 4G) vaut
/// donc comme une connexion bien plus lente : tout le monde subirait ses à-coups.
fn link_ms(c: &Conn) -> Option<f32> {
    // Ce que dit le jeu et ce que le relais mesure lui-même : le pire des deux (un jeu
    // modifié peut mentir sur le premier, pas raccourcir le second).
    let said = c.rtt.map(|r| r + 2.0 * c.rtt_dev);
    let seen = c.probe_rtt.map(|r| r + 2.0 * c.probe_dev);
    match (said, seen) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    }
}

/// Une mesure d'aller-retour faite par le relais.
fn probe_sample(c: &mut Conn, ms: f32) {
    if let Some(r) = c.probe_rtt {
        c.probe_dev = c.probe_dev * 0.8 + (ms - r).abs() * 0.2;
    }
    c.probe_rtt = Some(c.probe_rtt.map_or(ms, |r| r * 0.8 + ms * 0.2));
}

/// À chaque entretien : le relais mesure lui-même l'aller-retour de chaque joueur (une
/// question toutes les 4 s ; sans réponse en 6 s, la mesure vaut 6 s).
fn probe_links(st: &mut State) {
    // Grands mondes de construction : leurs joueurs ne sont mesurés que toutes les minutes
    // (v136 : il ne s'agit que de choisir parmi eux un propriétaire mieux relié ; les
    // garants, eux, restent mesurés toutes les 4 s).
    let big: HashSet<u64> = st
        .rooms
        .iter()
        .filter(|(_, r)| r.kind == 1 && r.members.len() > PROBE_ALL_UPTO)
        .map(|(&id, _)| id)
        .collect();
    for c in st.conns.values_mut().filter(|c| c.pseudo.is_some()) {
        if let Some((_, at)) = c.probe {
            if at.elapsed() < Duration::from_secs(6) {
                continue;
            }
            c.probe = None;
            probe_sample(c, 6000.0);
        }
        let every = if !c.garant && c.room.is_some_and(|r| big.contains(&r)) {
            Duration::from_secs(60)
        } else {
            Duration::from_secs(4)
        };
        if c.probed.is_none_or(|t| t.elapsed() >= every) {
            let nonce = rand::rngs::OsRng.next_u32();
            c.probe = Some((nonce, Instant::now()));
            c.probed = Some(Instant::now());
            transport::push(&c.tx, &Response::Probe { nonce });
        }
    }
}

/// Jusqu'à ce nombre de joueurs, chaque joueur d'un monde de construction est mesuré
/// toutes les 4 s ; au-delà, toutes les minutes.
const PROBE_ALL_UPTO: usize = 64;

/// Connexion à écarter des rôles d'hôte et de garant (mesurée, et mauvaise).
fn link_bad(c: &Conn) -> bool {
    link_ms(c).is_some_and(|ms| ms >= BAD_RTT)
}

/// Écarte les candidats à mauvaise connexion, s'il en reste d'autres.
fn good_links(mut cands: Vec<zones::Candidate>) -> Vec<zones::Candidate> {
    if cands.iter().any(|c| c.rtt.is_none_or(|r| r < BAD_RTT)) {
        cands.retain(|c| c.rtt.is_none_or(|r| r < BAD_RTT));
    }
    cands
}
/// Un remplaçant doit avoir au plus cet aller-retour (et moitié moins que l'hôte).
const GOOD_RTT: f32 = 120.0;

/// L'hôte d'une partie (FPS ou construction, servie par un joueur) a une mauvaise
/// connexion au relais et un autre joueur une bien meilleure : la partie lui passe (l'ancien
/// hôte reste comme joueur). En FPS, il faut qu'il ait la copie de la partie : il est
/// d'abord désigné garant de secours, la bascule se fait à l'entretien suivant.
/// Renvoie vrai si l'hôte a changé.
fn rebalance_host(st: &mut State, rid: u64) -> bool {
    let Some(room) = st.rooms.get(&rid) else {
        return false;
    };
    if room.garant_hosted
        || room.arbitration.is_some()
        || room
            .rebalanced
            .is_some_and(|t| t.elapsed() < Duration::from_secs(90))
    {
        return false;
    }
    let rtt = |id: u64| st.conns.get(&id).and_then(link_ms);
    let host = room.host;
    let Some(h) = rtt(host).filter(|&h| h >= BAD_RTT) else {
        return false;
    };
    // (Ensembles : dans un monde de 100 000 joueurs, comparer chacun à la liste des
    // garants coûtait des centaines de millions de comparaisons par entretien.)
    let not_players: HashSet<u64> = room
        .shards
        .values()
        .flatten()
        .map(|s| s.host)
        .chain(room.hidden.iter().copied())
        .collect();
    let best = room
        .members
        .iter()
        .filter(|m| !not_players.contains(m))
        .filter_map(|&m| Some((m, rtt(m)?)))
        .filter(|&(_, r)| r <= GOOD_RTT && r * 2.0 <= h)
        .min_by(|a, b| a.1.total_cmp(&b.1));
    let Some((best, r)) = best else {
        // (Personne de mieux : on ne regarde de nouveau qu'au bout du même délai ; v140 :
        // dans un monde de 100 000 joueurs, il les passait tous en revue à chaque entretien.)
        if let Some(room) = st.rooms.get_mut(&rid) {
            room.rebalanced = Some(Instant::now());
        }
        return false;
    };
    if room.kind == 0 && room.backup != Some(best) {
        // Il reçoit d'abord les copies de la partie.
        if let Some(c) = st.conns.get(&host) {
            transport::push(&c.tx, &Response::BackupIs { peer: best });
        }
        return false;
    }
    if let Some(room) = st.rooms.get_mut(&rid) {
        room.rebalanced = Some(Instant::now());
    }
    log::info!(
        "partie {rid} : l'hôte a {h:.0} ms vers le relais, un joueur {r:.0} ms : il reprend la partie"
    );
    switch_host(st, rid, best, Some(host));
    true
}

/// Fréquentation (à chaque entretien) : les joueurs d'une même partie FPS lancée, et ceux
/// qui restent dans le même chunk d'un monde de construction (chacun avec ses 8 voisins
/// de rang : le travail ne dépend pas de la taille d'une foule).
fn upkeep_contacts(st: &mut State) {
    let mut pairs: Vec<(u64, u64, f32)> = Vec::new();
    for r in st.rooms.values() {
        if r.kind == 0 {
            if !r.started {
                continue;
            }
            let players: Vec<u64> = r.everyone().filter(|m| !r.hidden.contains(m)).collect();
            for (i, &a) in players.iter().enumerate() {
                for &b in &players[i + 1..] {
                    pairs.push((a, b, contacts::SAME_MATCH));
                }
            }
        } else {
            // (v140 : pas dans les provinces servies par un relais de province : leurs
            // joueurs ne disent plus ici où ils sont.)
            let size = st
                .province_size
                .load(std::sync::atomic::Ordering::Relaxed)
                .max(1) as i32;
            for (chunk, members) in r.build.crowded() {
                if r.delegated
                    .contains(&proto::province_of(proto::region_of(chunk), size))
                {
                    continue;
                }
                for (i, &a) in members.iter().enumerate() {
                    for k in 1..=8.min(members.len() - 1) {
                        let b = members[(i + k) % members.len()];
                        if a != b {
                            pairs.push((a.min(b), a.max(b), contacts::NEARBY));
                        }
                    }
                }
            }
        }
    }
    // Chaque paire une seule fois.
    pairs.sort_unstable_by_key(|x| (x.0, x.1));
    pairs.dedup_by_key(|x| (x.0, x.1));
    for (a, b, points) in pairs {
        let name = |id: u64| st.conns.get(&id).and_then(|c| c.pseudo.clone());
        if let (Some(p), Some(q)) = (name(a), name(b)) {
            st.contacts.add(&p, &q, points);
        }
    }
    st.contacts.save_if_due();
}

/// Entretien (toutes les 2 s) : chaque partie FPS reçoit un garant indépendant en
/// attente ; une partie servie par un joueur passe à ce garant dès qu'il est prêt.
fn upkeep(st: &mut State) {
    if st.last_upkeep.elapsed() < Duration::from_secs(2) {
        return;
    }
    st.last_upkeep = Instant::now();
    probe_links(st);
    upkeep_contacts(st);
    upkeep_build(st);
    let rooms: Vec<u64> = st
        .rooms
        .iter()
        .filter(|(_, r)| r.kind == 0)
        .map(|(&id, _)| id)
        .collect();
    for rid in rooms {
        let Some(room) = st.rooms.get(&rid) else {
            continue;
        };
        // Arbitre parti ou muet depuis une minute : on fait sans lui.
        if let Some((_, arbiter, _, since)) = room.arbitration {
            if !st.conns.contains_key(&arbiter) || since.elapsed() > Duration::from_secs(60) {
                arbitration_done(st, rid, None);
            }
            continue;
        }
        // Relève des garants dont la propre partie se termine.
        relieve_garants(st, rid);
        // Hôte à la connexion pourrie : la partie passe au joueur le mieux connecté.
        if rebalance_host(st, rid) {
            continue;
        }
        let Some(room) = st.rooms.get(&rid) else {
            continue;
        };
        // Passage au garant prêt.
        if !room.garant_hosted {
            if let Some(&g) = room.ready_standby.first() {
                let old = room.host;
                switch_host(st, rid, g, Some(old));
                continue;
            }
        }
        if !room.hidden.is_empty() {
            continue;
        }
        let names: Vec<String> = room
            .everyone()
            .chain(std::iter::once(room.host))
            .filter_map(|m| st.conns.get(&m).and_then(|c| c.pseudo.clone()))
            .collect();
        let Some(g) = pick_garant(st, &names) else {
            continue;
        };
        let room = st.rooms.get_mut(&rid).expect("partie");
        room.members.push(g);
        room.hidden.insert(g);
        let (host, map_sig, map) = (room.host, room.map_sig, room.map.clone());
        let members: Vec<(u64, String)> = room
            .everyone()
            .filter(|&m| m != host)
            .map(|m| (m, String::new()))
            .collect();
        if let Some(c) = st.conns.get_mut(&g) {
            c.room = Some(rid);
            transport::push(
                &c.tx,
                &Response::GarantTask {
                    room: rid,
                    map_sig,
                    map,
                    members,
                    standby: true,
                },
            );
        }
        if let Some(c) = st.conns.get(&host) {
            transport::push(&c.tx, &Response::BackupIs { peer: g });
        }
    }
}

/// Joueurs d'une partie de construction (pas ses garants extérieurs) : parmi eux se
/// choisit le propriétaire du monde.
fn candidates(st: &State, room_id: u64) -> Vec<zones::Candidate> {
    let Some(room) = st.rooms.get(&room_id) else {
        return Vec::new();
    };
    good_links(
        room.everyone()
            .filter(|id| !room.hidden.contains(id))
            .filter_map(|id| candidate(st, id))
            .collect(),
    )
}

fn candidate(st: &State, id: u64) -> Option<zones::Candidate> {
    let c = st.conns.get(&id)?;
    Some(zones::Candidate {
        id,
        caps: c.caps,
        uptime: c.joined.elapsed(),
        ratio: c.pseudo.as_deref().map_or(0.0, |p| st.stats.ratio(p)),
        rtt: link_ms(c),
    })
}

/// Position d'un joueur de construction, au nom de `pseudo` (celui de sa session).
pub(crate) fn player_as(data: &[u8], pseudo: &str) -> Option<Vec<u8>> {
    match game::decode::<game::BuildMsg>(data)? {
        game::BuildMsg::Player {
            pos,
            yaw,
            pitch,
            crouch,
            speed,
            ping,
            ..
        } => Some(game::encode(&game::BuildMsg::Player {
            pseudo: pseudo.to_string(),
            pos,
            yaw,
            pitch,
            crouch,
            speed,
            ping,
        })),
        _ => None,
    }
}

/// Parties ouvertes sur ce relais (liste des serveurs).
/// Parties ouvertes ici. Un serveur de construction sous liste blanche n'y figure que pour
/// qui peut y entrer (sur la liste, OP ou créateur) ; jamais pour un autre relais (`who`
/// absent), qui ne dit pas qui regarde.
fn local_servers(st: &State, kind: u8, who: Option<&str>) -> Vec<proto::ServerEntry> {
    let mut list: Vec<proto::ServerEntry> = st
        .rooms
        .iter()
        .filter(|(_, r)| r.kind == kind && !(kind == 0 && r.started))
        .filter(|(_, r)| {
            r.kind != 1
                || match who {
                    Some(w) => !st.servers.whitelisted_out(&r.server, &r.creator, w),
                    None => !st.servers.whitelist_on(&r.server),
                }
        })
        .map(|(&rid, r)| proto::ServerEntry {
            id: rid,
            // Garant : jamais nommé aux joueurs.
            host: if r.garant_hosted {
                GARANT_NAME.to_string()
            } else {
                r.host_pseudo.clone()
            },
            name: r.name.clone(),
            map: r.map.clone(),
            // (v146) Au fond d'un pays, les joueurs ne sont plus connectés ici : comptés par
            // les provinces.
            players: provinces::world_players(r) as u32,
            max_players: r.max_players,
            region: st.cfg.name.clone(),
            relay_node: [0; 32],
            relay_key: [0; 32],
            relay_addr: String::new(),
        })
        .collect();
    list.sort_by_key(|e| e.id);
    list.truncate(200);
    list
}

/// Ce joueur est-il en survie sur ce serveur, d'après le relais (mode donné par un OP,
/// sinon mode par défaut du serveur pour qui n'est pas OP) ? Son inventaire est alors tenu
/// par le relais (`bags`) ; ce que son jeu annonce ne compte pas.
fn in_survival(st: &State, server: &str, creator: &str, pseudo: &str) -> bool {
    let mode = st
        .bags
        .mode(server, pseudo)
        .map(str::to_string)
        .or_else(|| {
            (!st.servers.is_op(server, creator, pseudo))
                .then(|| st.servers.default_gamemode(server))
                .flatten()
        });
    mode.is_some_and(|m| {
        matches!(
            m.trim().to_lowercase().as_str(),
            "0" | "s" | "survival" | "survie"
        )
    })
}

/// Un bloc vient d'être publié dans ce monde : compté dans le sac de son auteur s'il est
/// en survie (cassé : un crédit de minage ; posé : sorti du sac).
fn note_block(st: &mut State, rid: u64, by: &str, block: u16) {
    let Some((server, creator)) = st
        .rooms
        .get(&rid)
        .map(|r| (r.server.clone(), r.creator.clone()))
    else {
        return;
    };
    // (Provinces, v139 : le sac n'est compté que là où il est.)
    if in_survival(st, &server, &creator, by) && !provinces::bag_elsewhere(st, rid, by, block) {
        st.bags.block_set(&server, by, block);
    }
}

/// Aux hôtes des relais d'une région : la liste des relais prêts (ils se relient entre
/// eux, et le plus petit rang met les modifications en ordre).
fn announce_shard_peers(st: &State, rid: u64, region: [i32; 2]) {
    // (Région d'une province servie par un relais de province, v138 : c'est lui.)
    if provinces::delegated(st, rid, region) {
        return;
    }
    let Some(room) = st.rooms.get(&rid) else {
        return;
    };
    let Some(list) = room.shards.get(&region) else {
        return;
    };
    let ready = ready_infos(room, region);
    let sov = room.sov.get(&region).map(|s| proto::Sov {
        epoch: s.epoch,
        primary: s.slot,
        first: s.first,
        base: s.base,
        chain: s.chain,
        floor_t: s.floor_t,
    });
    for sh in list {
        if let Some(c) = st.conns.get(&sh.host) {
            transport::push(
                &c.tx,
                &Response::ShardPeers {
                    region,
                    list: ready.clone(),
                    sov: sov.clone(),
                },
            );
        }
    }
}

/// Relais prêts d'une région, avec l'époque souveraine en cours (0 : aucune).
fn ready_infos(room: &Room, region: [i32; 2]) -> Vec<proto::ShardInfo> {
    let epoch = room.sov.get(&region).map_or(0, |s| s.epoch);
    room.shards
        .get(&region)
        .into_iter()
        .flatten()
        .filter_map(|s| s.info.clone())
        .map(|mut i| {
            i.epoch = epoch;
            i
        })
        .collect()
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// Clé du compte d'un joueur connecté (registre).
fn account_key(st: &State, id: u64) -> Option<[u8; 32]> {
    let c = st.conns.get(&id)?;
    let p = c.pseudo.as_deref()?;
    st.registry.lookup(p).map(|r| r.1).or(c.account)
}

/// Clé qui certifie les comptes de ce monde et fait les règles de ses régions
/// souveraines : celle de ce relais, ou, pour un relais de province (v138), celle du relais
/// du monde.
fn root_of(st: &State, rid: u64) -> [u8; 32] {
    st.rooms
        .get(&rid)
        .and_then(|r| r.province.as_ref())
        .map_or(st.cert_key.verifying_key().to_bytes(), |p| {
            p.root.to_bytes()
        })
}

/// Clé du compte `pseudo` au registre de ce relais.
fn registry_key(st: &State, pseudo: &str) -> Option<[u8; 32]> {
    st.registry.lookup(pseudo).map(|r| r.1)
}

/// Billet d'un joueur pour les relais de région de son monde, avec le certificat de la
/// clé de son compte (régions souveraines, relais de secours).
fn ticket_for(st: &State, rid: u64, id: u64) -> Option<Response> {
    st.rooms.get(&rid)?;
    let c = st.conns.get(&id)?;
    let pseudo = c.pseudo.clone()?;
    let expires = now_secs() + 24 * 3600;
    let cert =
        account_key(st, id).map(|k| sovereign::Cert::issue(&st.cert_key, rid, &pseudo, k, expires));
    Some(Response::ShardTicket {
        room: rid,
        sig: sign_ticket(st, rid, &pseudo, expires),
        pseudo,
        expires,
        cert,
        root: st.cert_key.verifying_key().to_bytes(),
    })
}

/// (v155) Billet pour les relais de région de ce monde, signé par sa clé des certificats :
/// les hôtes des régions le vérifient sans pouvoir en faire.
fn sign_ticket(st: &State, rid: u64, pseudo: &str, expires: u64) -> Vec<u8> {
    use ed25519_dalek::Signer;
    st.cert_key
        .sign(&proto::ticket_msg(rid, pseudo, expires))
        .to_bytes()
        .to_vec()
}

/// Monde de construction : un relais de secours, sur le PC d'un autre joueur (connexion
/// de garant), qui garde une copie du monde et le reprend si ce relais tombe. Choisi dès
/// que le monde a deux joueurs ; remplacé s'il s'en va.
fn upkeep_standby(st: &mut State, rid: u64) {
    let Some(room) = st.rooms.get(&rid) else {
        return;
    };
    if room.kind != 1 {
        return;
    }
    // Son hôte parti : plus de relais de secours (un autre sera choisi).
    if let Some(sb) = &room.standby {
        if !room.members.contains(&sb.host) || !st.conns.contains_key(&sb.host) {
            let server = room.server.clone();
            let room = st.rooms.get_mut(&rid).expect("partie");
            room.standby = None;
            room.build.set_replicating(false);
            st.bags.watch(&server, false);
            announce_standby(st, rid);
        }
        return;
    }
    let mut players: Vec<u64> = room
        .members
        .iter()
        .filter(|m| !room.hidden.contains(m))
        .copied()
        .collect();
    if !players.contains(&room.host) {
        players.push(room.host);
    }
    if players.len() < 2 {
        return;
    }
    let host = room.host;
    let local_ok = st.standby_local;
    // (Gardiens retirés de ce monde à l'instant : pas repris tout de suite. Un témoin du
    // monde ne devient relais de secours qu'à défaut d'autre garant.)
    let cooling = guards::cooling(st, rid);
    let witness = |p: &str| {
        room.guards
            .as_ref()
            .is_some_and(|g| g.warden.current.is_witness(p))
    };
    // (Une connexion de garant d'un jeu, qui sait tenir un relais de secours ; pas celle
    // d'un joueur au menu.)
    let Some(g) = st
        .conns
        .iter()
        .filter(|(&g, c)| {
            c.garant
                && c.garant_only
                && !c.garant_banned
                && !c
                    .pseudo
                    .as_deref()
                    .is_some_and(|p| cooling.contains(&p.to_ascii_lowercase()))
                && c.room.is_none()
                && g != host
                && (local_ok
                    || (!c.local && !st.conns.get(&host).is_some_and(|h| same_machine(c, h))))
        })
        .max_by(|a, b| {
            let score = |c: &Conn| {
                let w = c.pseudo.as_deref().is_some_and(witness);
                let s = zones::aptitude(&zones::Candidate {
                    id: 0,
                    caps: c.caps,
                    uptime: c.joined.elapsed(),
                    ratio: c.pseudo.as_deref().map_or(0.0, |p| st.stats.ratio(p)),
                    rtt: link_ms(c),
                });
                (!w, s)
            };
            let (x, y) = (score(a.1), score(b.1));
            x.0.cmp(&y.0).then(x.1.total_cmp(&y.1))
        })
        .map(|(&g, _)| g)
    else {
        return;
    };
    let root = st.cert_key.verifying_key().to_bytes();
    let room = st.rooms.get_mut(&rid).expect("partie");
    let owner = st
        .conns
        .get(&room.host)
        .and_then(|c| c.pseudo.clone())
        .unwrap_or_else(|| room.host_pseudo.clone());
    let info = proto::StandbyInfo {
        name: room.name.clone(),
        map: room.map.clone(),
        map_sig: room.map_sig,
        max_players: room.max_players,
        creator: room.creator.clone(),
        server: room.server.clone(),
        owner,
    };
    room.members.push(g);
    room.hidden.insert(g);
    room.standby = Some(StandbyRef { host: g, at: None });
    room.build.set_replicating(true);
    let entries = room.build.export_entries();
    let server = room.server.clone();
    st.bags.watch(&server, true);
    let (bags, modes) = st.bags.all_of(&server);
    let Some(c) = st.conns.get_mut(&g) else {
        return;
    };
    c.room = Some(rid);
    transport::push(
        &c.tx,
        &Response::StandbyTask {
            room: rid,
            root,
            info,
        },
    );
    // La copie : tout l'historique, les sacs et les modes ; la suite au fil de l'eau.
    for (zone, seq, data) in entries {
        transport::push(&c.tx, &Response::Replica { zone, seq, data });
    }
    for (pseudo, bag) in bags {
        transport::push(
            &c.tx,
            &Response::ReplicaBag {
                pseudo,
                items: bag.items.into_iter().collect(),
                mined: bag.mined,
            },
        );
    }
    for (pseudo, mode) in modes {
        transport::push(&c.tx, &Response::ReplicaMode { pseudo, mode });
    }
    log::info!("monde {rid} : relais de secours confié à {g}");
}

/// Ce qui vient de changer dans un monde part à son relais de secours.
fn flush_replica(st: &mut State, rid: u64) {
    let Some(room) = st.rooms.get_mut(&rid) else {
        return;
    };
    let Some(host) = room.standby.as_ref().map(|s| s.host) else {
        return;
    };
    let entries = room.build.take_replica();
    let server = room.server.clone();
    let (bags, modes) = st.bags.take_changed(&server);
    let Some(c) = st.conns.get(&host) else {
        return;
    };
    for (zone, seq, data) in entries {
        transport::push(&c.tx, &Response::Replica { zone, seq, data });
    }
    for (pseudo, bag) in bags {
        transport::push(
            &c.tx,
            &Response::ReplicaBag {
                pseudo,
                items: bag.items.into_iter().collect(),
                mined: bag.mined,
            },
        );
    }
    for (pseudo, mode) in modes {
        transport::push(&c.tx, &Response::ReplicaMode { pseudo, mode });
    }
}

/// Aux joueurs d'un monde : où est son relais de secours, et le certificat de la clé de
/// leur compte (pour y entrer si ce relais tombe).
fn announce_standby(st: &State, rid: u64) {
    let Some(room) = st.rooms.get(&rid) else {
        return;
    };
    let at = room.standby.as_ref().and_then(|s| s.at.clone());
    // (L'hôte n'est pas compté dans les membres.)
    let mut to: Vec<u64> = room
        .members
        .iter()
        .filter(|m| !room.hidden.contains(m))
        .copied()
        .collect();
    if !to.contains(&room.host) {
        to.push(room.host);
    }
    for m in to {
        send_standby(st, rid, m, at.clone());
    }
}

fn send_standby(st: &State, rid: u64, id: u64, at: Option<proto::StandbyAt>) {
    let Some(c) = st.conns.get(&id) else {
        return;
    };
    if at.is_some() {
        if let Some(t) = ticket_for(st, rid, id) {
            transport::push(&c.tx, &t);
        }
    }
    transport::push(&c.tx, &Response::Standby { at });
}

/// Relais de secours : un joueur du monde repris entre (le premier en devient le
/// propriétaire, avec la copie du monde).
fn take_over_join(st: &mut State, id: u64, pseudo: &str, key: [u8; 32]) -> Response {
    let Some(sb) = st.standby.as_ref() else {
        return err(ErrorCode::Refused, "Pas de relais de secours ici.");
    };
    if let Some(c) = st.conns.get_mut(&id) {
        c.pseudo = Some(pseudo.to_string());
        c.account = Some(key);
    }
    if let Some(rid) = sb.resumed.filter(|r| st.rooms.contains_key(r)) {
        return join_room(st, id, pseudo, rid);
    }
    let info = sb.info.clone();
    let rid = open_room(
        st,
        id,
        pseudo,
        1,
        info.name.clone(),
        info.map.clone(),
        info.map_sig,
        u16::try_from(info.max_players).unwrap_or(u16::MAX),
    );
    let sb = st.standby.as_mut().expect("relais de secours");
    sb.resumed = Some(rid);
    let build = sb.build.take();
    let room = st.rooms.get_mut(&rid).expect("partie");
    room.creator = info.creator.clone();
    room.server = info.server.clone();
    if let Some(mut b) = build {
        // Objets au sol et coffres : retrouvés à partir des états publiés.
        for (_, _, data) in b.export_entries() {
            if let Some(m) = game::decode::<game::BuildMsg>(&data) {
                b.items.absorb(&m);
            }
        }
        room.build = b;
    }
    log::info!(
        "relais de secours : monde « {} » repris (partie {rid})",
        info.name
    );
    Response::Joined {
        room: rid,
        host: pseudo.to_string(),
        map: info.map,
        map_sig: info.map_sig,
    }
}

/// Régions d'un monde : une région d'au moins deux relais prêts devient souveraine (son
/// hôte principal met en ordre ses modifications, les autres vérifient) ; une époque
/// commence quand l'hôte principal change (parti, écarté, trop lent), et finit quand la
/// région n'a plus deux relais.
fn upkeep_sovereignty(st: &mut State, rid: u64) {
    // Monde en pause : pas de changement d'époque (il archiverait).
    if guards::frozen(st, rid) {
        return;
    }
    // Passations arrivées pendant la pause.
    let waiting = st
        .rooms
        .get_mut(&rid)
        .map(|r| std::mem::take(&mut r.handovers))
        .unwrap_or_default();
    for (host, region, epoch, batches) in waiting {
        take_handover(st, rid, host, region, epoch, batches);
    }
    let root_key = root_of(st, rid);
    // (Provinces servies par un relais de province, v138 : c'est lui qui tient leurs
    // époques.)
    let delegated = provinces::delegated_set(st, rid);
    let Some(room) = st.rooms.get(&rid) else {
        return;
    };
    let regions: HashSet<[i32; 2]> = room
        .shards
        .keys()
        .chain(room.sov.keys())
        .filter(|r| !provinces::in_set(&delegated, **r))
        .copied()
        .collect();
    for region in regions {
        // (Une confirmation qui tarde : voir `try_commit`.)
        try_commit(st, rid, region);
        let Some(room) = st.rooms.get(&rid) else {
            return;
        };
        let ready: Vec<(u64, u16)> = room
            .shards
            .get(&region)
            .into_iter()
            .flatten()
            .filter(|s| s.info.is_some())
            .map(|s| (s.host, s.slot))
            .collect();
        let current = room.sov.get(&region);
        let gone = current.is_some_and(|s| !ready.iter().any(|r| r.0 == s.primary));
        let stalled = current.is_some_and(|s| {
            s.waiting.is_some_and(|t| t.elapsed() > SOV_STALL)
                || s.sent.front().is_some_and(|(t, _)| t.elapsed() > SOV_STALL)
        });
        if current.is_some() && !gone && !stalled && ready.len() >= 2 {
            continue;
        }
        if current.is_none() && ready.len() < 2 {
            continue;
        }
        // Fin de l'époque en cours (gardée pour les passations).
        let room = st.rooms.get_mut(&rid).expect("partie");
        let mut ended = None;
        if let Some(mut old) = room.sov.remove(&region) {
            if stalled && !gone {
                log::info!(
                    "monde {rid} : région {region:?}, l'hôte principal n'avance plus : remplacé"
                );
                room.sov_skip.entry(region).or_default().insert(old.primary);
            }
            // Les lots reçus de l'hôte principal et pas encore confirmés : ce relais les
            // vérifie lui-même et les archive (rien ne se perd si les autres hôtes ne les
            // ont pas eus) ; une faute écarte l'hôte.
            let rules = sovereign::Rules {
                room: rid,
                region,
                root: root_key,
                floor_t: old.floor_t,
            };
            let pending: Vec<sovereign::Batch> = std::mem::take(&mut old.pending)
                .into_iter()
                .map(|p| p.batch)
                .collect();
            let (done, fault) = sound_prefix(&rules, old.epoch, &old.key, pending);
            if let Some(last) = done.last() {
                (old.base, old.chain) = (last.end(), last.chain());
            }
            let past = room.sov_past.entry(region).or_default();
            past.push_back(PastSov {
                epoch: old.epoch,
                host: old.primary,
                key: old.key,
                floor_t: old.floor_t,
                base: old.base,
                chain: old.chain,
            });
            while past.len() > 4 {
                past.pop_front();
            }
            let sent: Vec<sovereign::Edit> = old.sent.into_iter().map(|(_, e)| e).collect();
            ended = Some((old.primary, old.epoch, done, fault, sent));
        }
        // Nouvelle époque : l'hôte principal est le plus petit rang pas écarté.
        if ready.len() >= 2 {
            let skip = room.sov_skip.entry(region).or_default();
            if ready.iter().all(|r| skip.contains(&r.0)) {
                skip.clear();
            }
            let (primary, slot) = *ready
                .iter()
                .filter(|r| !skip.contains(&r.0))
                .min_by_key(|r| r.1)
                .expect("relais");
            let key = account_key(st, primary)
                .and_then(|k| ed25519_dalek::VerifyingKey::from_bytes(&k).ok());
            let room = st.rooms.get_mut(&rid).expect("partie");
            if let Some(key) = key {
                let epoch = room.next_epoch;
                room.next_epoch += 1;
                let first = room.build.reserve(SOV_RANGE);
                let chain = sovereign::genesis(rid, region, epoch, first);
                room.sov.insert(
                    region,
                    RootSov {
                        epoch,
                        primary,
                        key,
                        slot,
                        first,
                        floor_t: now_ms().saturating_sub(SOV_FLOOR_MS),
                        base: first,
                        chain,
                        pending: VecDeque::new(),
                        waiting: None,
                        sent: VecDeque::new(),
                        acks: HashMap::new(),
                    },
                );
                log::info!(
                    "monde {rid} : région {region:?} souveraine, époque {epoch}, hôte \
                     principal {primary} (rang {slot})"
                );
            }
        }
        let room = st.rooms.get_mut(&rid).expect("partie");
        room.shards_dirty.insert(region);
        announce_shard_peers(st, rid, region);
        // L'époque finie : ses lots vérifiés ici, archivés ; ce qu'elle n'avait pas encore
        // publié des modifications arrivées ici repart (vers le nouvel hôte principal, ou
        // publié ici si la région n'est plus souveraine).
        if let Some((host, epoch, done, fault, sent)) = ended {
            commit_batches(st, rid, region, done);
            if let Some(why) = fault {
                log::warn!(
                    "monde {rid} : région {region:?}, faute de l'hôte principal trouvée en \
                     fin d'époque {epoch} : {why}"
                );
                ban_host(st, host);
            }
            for e in sent {
                resubmit(st, rid, region, e);
            }
        }
    }
}

/// Archive ce que les autres hôtes d'une région souveraine ont confirmé : deux d'entre eux
/// s'il y en a deux (un complice de l'hôte principal ne suffit pas), sinon le seul autre.
/// Une deuxième confirmation qui tarde (hôte en retard, ou qui refuse de confirmer) : ce
/// relais vérifie lui-même ce que la première couvre. Appelé à chaque confirmation, à
/// chaque lot reçu (les confirmations peuvent le devancer) et régulièrement.
fn try_commit(st: &mut State, rid: u64, region: [i32; 2]) {
    // Monde en pause : rien n'est archivé (les lots attendent la reprise).
    if guards::frozen(st, rid) {
        return;
    }
    let root = root_of(st, rid);
    let Some(room) = st.rooms.get_mut(&rid) else {
        return;
    };
    let ready: Vec<u64> = room
        .shards
        .get(&region)
        .into_iter()
        .flatten()
        .filter(|s| s.info.is_some())
        .map(|s| s.host)
        .collect();
    let Some(sv) = room.sov.get_mut(&region) else {
        return;
    };
    let primary = sv.primary;
    sv.acks.retain(|h, _| ready.contains(h) && *h != primary);
    if sv.pending.is_empty() {
        return;
    }
    let need = ready.len().saturating_sub(1).clamp(1, 2);
    let mut covered: Vec<usize> = sv
        .acks
        .values()
        .filter_map(|(s, c)| sv.pending.iter().position(|p| p.end == *s && p.chain == *c))
        .collect();
    covered.sort_unstable_by(|a, b| b.cmp(a));
    let late =
        need == 2 && covered.len() == 1 && sv.waiting.is_some_and(|t| t.elapsed() > ACK_WAIT);
    let upto = match covered.get(need - 1) {
        Some(&i) => i,
        None if late => {
            let rules = sovereign::Rules {
                room: rid,
                region,
                root,
                floor_t: sv.floor_t,
            };
            let head: Vec<sovereign::Batch> = sv
                .pending
                .iter()
                .take(covered[0] + 1)
                .map(|p| p.batch.clone())
                .collect();
            let (sound, fault) = sound_prefix(&rules, sv.epoch, &sv.key, head);
            if let Some(why) = fault {
                log::warn!(
                    "monde {rid} : région {region:?}, faute de l'hôte principal vue par ce \
                     relais : {why}"
                );
                ban_host(st, primary);
                upkeep_sovereignty(st, rid);
                return;
            }
            match sound.len().checked_sub(1) {
                Some(i) => i,
                None => return,
            }
        }
        None => return,
    };
    let done: Vec<PendingBatch> = sv.pending.drain(..=upto).collect();
    let last = done.last().expect("lot");
    (sv.base, sv.chain) = (last.end, last.chain);
    commit_batches(st, rid, region, done.into_iter().map(|p| p.batch).collect());
    // Ça avance : ce qui attend encore attend depuis maintenant.
    if let Some(sv) = st.rooms.get_mut(&rid).and_then(|r| r.sov.get_mut(&region)) {
        sv.waiting = (!sv.pending.is_empty() || !sv.sent.is_empty()).then(Instant::now);
    }
}

/// Ce relais vérifie lui-même des lots de l'hôte principal d'une époque, qui se suivent
/// (chaque modification, et dates et cadence de chaque auteur sur les 64 derniers lots) :
/// le plus long début sans faute (au plus 256 lots), et la faute qui l'arrête.
fn sound_prefix(
    rules: &sovereign::Rules,
    epoch: u64,
    key: &ed25519_dalek::VerifyingKey,
    batches: Vec<sovereign::Batch>,
) -> (Vec<sovereign::Batch>, Option<String>) {
    let mut done: Vec<sovereign::Batch> = Vec::new();
    for b in batches.into_iter().take(256) {
        done.push(b);
        let from = done.len().saturating_sub(64);
        if let Some(why) = sovereign::judge(rules, epoch, key, &done[from..]) {
            done.pop();
            return (done, Some(why));
        }
    }
    (done, None)
}

/// Surveillance mutuelle : un lot tiré au hasard part chez l'hôte d'un relais d'une autre
/// région du monde, qui le vérifie (`Response::Audit`). Sans un tel hôte (petit monde),
/// ou une fois sur `AUDIT_SELF`, ce relais le vérifie aussi lui-même.
fn audit_batch(st: &mut State, rid: u64, region: [i32; 2], batch: sovereign::Batch) {
    let root = root_of(st, rid);
    let Some(room) = st.rooms.get(&rid) else {
        return;
    };
    let Some(sv) = room.sov.get(&region) else {
        return;
    };
    let (epoch, key, floor_t) = (sv.epoch, sv.key, sv.floor_t);
    // Au hasard parmi les hôtes prêts des autres régions (pas un de la région : ils
    // vérifient déjà chaque lot, et l'un d'eux peut être complice), encore dans ce monde
    // et jamais écartés (la liste n'est refaite qu'à l'entretien).
    let mut auditor = None;
    if !room.auditors.is_empty() {
        for _ in 0..16 {
            let (a, r) = room.auditors[rand::random::<u32>() as usize % room.auditors.len()];
            let usable = r != region
                && room.audit_misses.get(&a).is_none_or(|m| *m < AUDIT_MISSES)
                && st
                    .conns
                    .get(&a)
                    .is_some_and(|c| !c.garant_banned && c.room == Some(rid));
            if usable {
                auditor = Some(a);
                break;
            }
        }
    }
    let self_every = st.audit_self.max(1);
    let here = (auditor.is_none() || rand::random::<u32>().is_multiple_of(self_every)).then(|| {
        let rules = sovereign::Rules {
            room: rid,
            region,
            root,
            floor_t,
        };
        batch.audit(&rules, epoch, &key)
    });
    if let Some(Some(why)) = &here {
        log::warn!(
            "monde {rid} : région {region:?}, faute de l'hôte principal vue par ce relais : {why}"
        );
        convict(st, rid, region, epoch, &batch);
    }
    let Some(auditor) = auditor else {
        return;
    };
    let Some(room) = st.rooms.get_mut(&rid) else {
        return;
    };
    let id = room.next_audit;
    room.next_audit += 1;
    let msg = Response::Audit {
        id,
        room: rid,
        region,
        epoch,
        key: key.to_bytes(),
        root,
        floor_t,
        batch: batch.clone(),
    };
    room.audits.insert(
        id,
        AuditRec {
            auditor,
            region,
            epoch,
            batch,
            at: Instant::now(),
            here,
        },
    );
    if let Some(c) = st.conns.get(&auditor) {
        transport::push(&c.tx, &msg);
    }
}

/// Réponse d'un vérificateur. Une faute signalée est revérifiée ici sur la copie que ce
/// relais a reçue de l'hôte principal lui-même : prouvée, l'hôte principal est écarté
/// (et l'autre hôte de la région qui avait confirmé ce lot) ; fausse, c'est le
/// vérificateur qui est écarté. Un « rien à signaler » sur un lot où ce relais a trouvé
/// une faute écarte aussi le vérificateur.
fn audited(st: &mut State, rid: u64, from: u64, aid: u64, fault: Option<String>) {
    let root = root_of(st, rid);
    let Some(room) = st.rooms.get_mut(&rid) else {
        return;
    };
    if room.audits.get(&aid).is_none_or(|a| a.auditor != from) {
        return;
    }
    let rec = room.audits.remove(&aid).expect("vérification");
    room.audit_misses.remove(&from);
    let found = match rec.here {
        Some(here) => here,
        None => {
            if fault.is_none() {
                return;
            }
            // Clé de l'hôte principal de cette époque (en cours ou finie).
            let key = room
                .sov
                .get(&rec.region)
                .filter(|s| s.epoch == rec.epoch)
                .map(|s| (s.key, s.floor_t))
                .or_else(|| {
                    room.sov_past
                        .get(&rec.region)?
                        .iter()
                        .find(|p| p.epoch == rec.epoch)
                        .map(|p| (p.key, p.floor_t))
                });
            let Some((key, floor_t)) = key else {
                return;
            };
            let rules = sovereign::Rules {
                room: rid,
                region: rec.region,
                root,
                floor_t,
            };
            let found = rec.batch.audit(&rules, rec.epoch, &key);
            if let Some(why) = &found {
                log::warn!(
                    "monde {rid} : région {:?}, faute de l'hôte principal signalée par \
                     l'hôte {from} et vérifiée ici : {why}",
                    rec.region
                );
                convict(st, rid, rec.region, rec.epoch, &rec.batch);
            }
            found
        }
    };
    // Le vérificateur a dit autre chose que ce que ce relais a trouvé : écarté.
    if found.is_some() != fault.is_some() {
        log::warn!(
            "monde {rid} : l'hôte {from} a mal vérifié un lot de la région {:?} ({}) : écarté",
            rec.region,
            if fault.is_some() {
                "faute inventée"
            } else {
                "faute pas vue"
            }
        );
        ban_host(st, from);
    }
}

/// Faute prouvée dans un lot de l'hôte principal d'une époque : il est écarté, et les
/// autres hôtes de la région qui avaient confirmé ce lot aussi (ils disaient l'avoir
/// vérifié). Une nouvelle époque commence avec ceux qui restent.
fn convict(st: &mut State, rid: u64, region: [i32; 2], epoch: u64, batch: &sovereign::Batch) {
    let Some(room) = st.rooms.get(&rid) else {
        return;
    };
    let (host, witnesses): (Option<u64>, Vec<u64>) = match room.sov.get(&region) {
        Some(s) if s.epoch == epoch => (
            Some(s.primary),
            s.acks
                .iter()
                .filter(|(_, (seq, _))| *seq >= batch.end())
                .map(|(w, _)| *w)
                .collect(),
        ),
        _ => (
            room.sov_past
                .get(&region)
                .and_then(|p| p.iter().find(|p| p.epoch == epoch))
                .map(|p| p.host),
            Vec::new(),
        ),
    };
    for w in witnesses {
        log::warn!("monde {rid} : région {region:?}, l'hôte {w} avait confirmé ce lot : écarté");
        ban_host(st, w);
    }
    if let Some(h) = host {
        ban_host(st, h);
    }
    upkeep_sovereignty(st, rid);
}

/// Surveillance mutuelle, à chaque entretien : les hôtes prêts qui peuvent vérifier des
/// lots, et les vérifications restées sans réponse (comptées contre leur vérificateur).
fn upkeep_audits(st: &mut State, rid: u64) {
    let Some(room) = st.rooms.get_mut(&rid) else {
        return;
    };
    let conns = &st.conns;
    room.auditors = room
        .shards
        .iter()
        .flat_map(|(r, l)| {
            l.iter()
                .filter(|s| s.info.is_some())
                .map(move |s| (s.host, *r))
        })
        .filter(|(h, _)| conns.get(h).is_some_and(|c| !c.garant_banned))
        .collect();
    let late: Vec<u64> = room
        .audits
        .iter()
        .filter(|(_, a)| a.at.elapsed() > AUDIT_WAIT)
        .map(|(id, _)| *id)
        .collect();
    for id in late {
        if let Some(a) = room.audits.remove(&id) {
            *room.audit_misses.entry(a.auditor).or_default() += 1;
        }
    }
    room.audit_misses.retain(|h, _| conns.contains_key(h));
}

/// D'un hôte de la région en fin d'époque : les lots qu'il avait vérifiés (signés par
/// l'hôte principal de cette époque), archivés s'ils suivent ce qui l'est déjà.
fn take_handover(
    st: &mut State,
    rid: u64,
    id: u64,
    region: [i32; 2],
    epoch: u64,
    batches: Vec<sovereign::Batch>,
) {
    let root = root_of(st, rid);
    let Some(room) = st.rooms.get_mut(&rid) else {
        return;
    };
    let is_host = room
        .shards
        .get(&region)
        .is_some_and(|l| l.iter().any(|s| s.host == id));
    let Some(past) = room
        .sov_past
        .get_mut(&region)
        .and_then(|p| p.iter_mut().find(|p| p.epoch == epoch))
    else {
        return;
    };
    if !is_host {
        return;
    }
    let (mut base, mut chain) = (past.base, past.chain);
    let mut next = Vec::new();
    for b in batches {
        if b.end() <= base || b.epoch != epoch || !b.verify(&past.key) {
            continue;
        }
        if b.first != base || b.prev != chain {
            break;
        }
        (base, chain) = (b.end(), b.chain());
        next.push(b);
    }
    // Ce relais les vérifie lui-même avant de les archiver (l'hôte principal de cette
    // époque peut remettre lui-même des lots que personne n'a vus) : ce qui précède une
    // faute est gardé, l'hôte fautif écarté.
    let rules = sovereign::Rules {
        room: rid,
        region,
        root,
        floor_t: past.floor_t,
    };
    let (done, fault) = sound_prefix(&rules, epoch, &past.key, next);
    if let Some(last) = done.last() {
        (past.base, past.chain) = (last.end(), last.chain());
    }
    let host = past.host;
    commit_batches(st, rid, region, done);
    if let Some(why) = fault {
        log::warn!("monde {rid} : région {region:?}, passation fautive (époque {epoch}) : {why}");
        ban_host(st, host);
    }
}

/// Fréquentation : parler à quelqu'un compte, au plus une fois par 30 s et par auditeur.
fn note_talk(st: &mut State, id: u64, pseudo: &str, heard: Vec<u64>) {
    let now = Instant::now();
    let mut fresh = Vec::new();
    if let Some(c) = st.conns.get_mut(&id) {
        c.talked
            .retain(|_, t| now.duration_since(*t) < Duration::from_secs(120));
        for t in heard {
            let due = c
                .talked
                .get(&t)
                .is_none_or(|l| now.duration_since(*l) >= Duration::from_secs(30));
            if due && c.talked.len() < 1024 {
                c.talked.insert(t, now);
                fresh.push(t);
            }
        }
    }
    for t in fresh {
        if let Some(q) = st.conns.get(&t).and_then(|c| c.pseudo.clone()) {
            st.contacts.add(pseudo, &q, contacts::VOICE);
        }
    }
}

/// Faute prouvée d'un hôte (hôte principal, hôte qui avait confirmé un lot fautif,
/// vérificateur qui a menti) : écarté de tout rôle de garant, il quitte le monde.
fn ban_host(st: &mut State, host: u64) {
    st.sov_faults
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if let Some(c) = st.conns.get_mut(&host) {
        c.garant_banned = true;
    }
    leave_room(st, host);
    if let Some(c) = st.conns.get(&host) {
        transport::push(&c.tx, &Response::RoomClosed);
    }
}

/// Une modification signée par ce relais que l'époque où elle était partie n'a pas
/// publiée : au nouvel hôte principal, ou publiée ici si la région n'est plus souveraine
/// (une seule fois en tout, voir `Room::root_done`).
fn resubmit(st: &mut State, rid: u64, region: [i32; 2], mut edit: sovereign::Edit) {
    let Some(room) = st.rooms.get_mut(&rid) else {
        return;
    };
    if room.root_done.contains(&edit.t) {
        return;
    }
    if let Some(sv) = room.sov.get_mut(&region) {
        // Trop ancienne pour la nouvelle époque : signée de nouveau.
        if edit.t < sv.floor_t {
            st.root_t = (st.root_t + 1).max(now_ms());
            edit = sovereign::Edit::sign(
                &st.cert_key,
                rid,
                &edit.pseudo,
                edit.pos,
                edit.block,
                edit.fluid,
                edit.at,
                st.root_t,
                true,
            );
        }
        sv.waiting.get_or_insert_with(Instant::now);
        sv.sent.push_back((Instant::now(), edit.clone()));
        if let Some(c) = st.conns.get(&sv.primary) {
            transport::push(
                &c.tx,
                &Response::RootEdits {
                    region,
                    edits: vec![edit],
                    certs: Vec::new(),
                },
            );
        }
        return;
    }
    note_root_done(room, edit.t);
    let before = room.build.block_at(edit.pos);
    let e = edit;
    if publish_block(st, rid, 0, false, &e.pseudo, e.pos, e.block, e.fluid) {
        if !e.fluid && !(e.block == 0 && before == Some(0)) {
            note_block(st, rid, &e.pseudo, e.block);
        }
        spill_chest(st, rid, e.pos);
    }
}

/// Retient qu'une modification signée par ce relais est archivée (les plus anciennes
/// s'oublient au-delà de 65 536).
fn note_root_done(room: &mut Room, t: u64) -> bool {
    let new = room.root_done.insert(t);
    while room.root_done.len() > 1 << 16 {
        room.root_done.pop_first();
    }
    new
}

/// Archive les lots confirmés d'une région souveraine : chaque modification entre dans
/// l'historique à son rang, part aux joueurs proches qu'aucun relais de la région ne sert
/// et aux hôtes de la région pas encore prêts, et compte dans le sac de son auteur en
/// survie.
fn commit_batches(st: &mut State, rid: u64, region: [i32; 2], batches: Vec<sovereign::Batch>) {
    for b in batches {
        for r in b.records {
            let e = r.edit;
            let chunk = [e.pos[0].div_euclid(16), e.pos[2].div_euclid(16)];
            let Some(room) = st.rooms.get_mut(&rid) else {
                return;
            };
            // Signée par ce relais : plus en attente ; déjà archivée par un autre chemin
            // (passation d'une époque finie, puis renvoi) : pas une deuxième fois.
            if e.by_root {
                if let Some(sv) = room.sov.get_mut(&region) {
                    sv.sent.retain(|(_, s)| s.t != e.t);
                }
                if !note_root_done(room, e.t) {
                    continue;
                }
            }
            let before = room.build.block_at(e.pos);
            st.sov_committed
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let data = game::encode(&game::BuildMsg::Set {
                pos: e.pos,
                block: e.block,
                by: e.pseudo.clone(),
                seq: r.seq,
            });
            // Ceux à portée qu'aucun relais de la région ne sert (v136 : rangés à part,
            // plus besoin de regarder tous les joueurs proches).
            let to: Vec<u64> = room
                .needs
                .get(&region)
                .map(|set| {
                    set.iter()
                        .copied()
                        .filter(|m| {
                            room.build
                                .positions
                                .get(m)
                                .is_some_and(|&p| zones::near(p, chunk))
                        })
                        .collect()
                })
                .unwrap_or_default();
            let sends = room
                .build
                .mirror_at(proto::zone_of(chunk), &data, Some(r.seq), &to);
            for (m, items) in sends {
                if let Some(c) = st.conns.get(&m) {
                    for d in items {
                        push_zone(&c.tx, chunk, d);
                    }
                }
            }
            for sh in room.shards.get(&region).into_iter().flatten() {
                if sh.info.is_none() {
                    if let Some(c) = st.conns.get(&sh.host) {
                        push_zone(&c.tx, chunk, data.clone());
                    }
                }
            }
            if !e.fluid && !(e.block == 0 && before == Some(0)) {
                note_block(st, rid, &e.pseudo, e.block);
            }
            spill_chest(st, rid, e.pos);
        }
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Où joue chaque joueur connecté : pseudo en minuscules → (connexion de joueur, partie).
/// Calculé une fois par entretien : sans lui, chaque question « le joueur de ce garant
/// est-il encore en jeu ? » parcourait toutes les connexions (des dizaines de millions de
/// comparaisons par entretien dans un monde de 60 000 joueurs).
type PlayersIndex = Arc<HashMap<String, (u64, Option<u64>)>>;

/// Index des joueurs connectés (pseudo → connexion, partie), pour choisir les hôtes : refait
/// au plus toutes les `INDEX_FRESH` (v140 : dans un monde de 100 000 joueurs, le refaire
/// coûtait 60 ms à chaque entretien où une région attendait un hôte).
fn players_index(st: &State) -> PlayersIndex {
    if let Some((t, i)) = st.index_cache.borrow().as_ref() {
        if t.elapsed() < INDEX_FRESH {
            return i.clone();
        }
    }
    players_index_fresh(st)
}

/// Le même, refait maintenant.
fn players_index_fresh(st: &State) -> PlayersIndex {
    let i: PlayersIndex = Arc::new(
        st.conns
            .iter()
            .filter(|(_, c)| !c.garant_only)
            .filter_map(|(&id, c)| Some((c.pseudo.as_deref()?.to_lowercase(), (id, c.room))))
            .collect(),
    );
    *st.index_cache.borrow_mut() = Some((Instant::now(), i.clone()));
    i
}

/// (v142) Joueurs d'un monde à partir desquels la copie de ce que publient ses provinces
/// est gardée par leurs relais de secours (en dessous, ici : c'est elle qui sert à
/// l'enregistrement du monde chez son propriétaire).
const COPIES_UNTIL: usize = 20_000;

/// Durée de vie de l'index des joueurs.
const INDEX_FRESH: Duration = Duration::from_secs(3);

/// Plus d'hôte libre pour une région ou une province : on cherche de nouveau au bout de…
const HOSTS_WAIT: Duration = Duration::from_secs(5);

/// Chunks autour d'un joueur dont les relais de région lui sont annoncés (le jeu rejoint
/// ceux à moins de 12 chunks ; la liste est refaite à chaque changement de chunk).
const SHARD_VIEW: i32 = 16;

/// Régions en vue d'un joueur placé en `chunk`.
fn regions_in_view(chunk: [i32; 2]) -> impl Iterator<Item = [i32; 2]> {
    let lo = proto::region_of([chunk[0] - SHARD_VIEW, chunk[1] - SHARD_VIEW]);
    let hi = proto::region_of([chunk[0] + SHARD_VIEW, chunk[1] + SHARD_VIEW]);
    (lo[0]..=hi[0]).flat_map(move |x| (lo[1]..=hi[1]).map(move |z| [x, z]))
}

/// Donne à un joueur les relais des régions en vue de là où il est, et son billet, si
/// cette liste a changé pour lui. Avant la v128, chaque joueur recevait la liste de tous
/// les relais du monde à chaque changement n'importe où : dans un monde de 60 000 joueurs
/// (600 relais), 60 Ko à chacun à chaque fois, soit 3,6 Go par changement.
fn send_shards(st: &mut State, rid: u64, id: u64) {
    use std::hash::{Hash, Hasher};
    let size = provinces::size(st);
    let Some(room) = st.rooms.get_mut(&rid) else {
        return;
    };
    let Some(&chunk) = room.build.positions.get(&id) else {
        return;
    };
    // Provinces (v138) : les relais des régions d'une province servie par un relais de
    // province sont annoncés par lui ; un relais de province n'annonce que les siens (et
    // pas de billet : il vient du relais du monde, qui certifie les clés).
    let delegated = room.delegated.clone();
    let mine = |r: [i32; 2]| match &room.province {
        Some(p) => p.has(r),
        None => !delegated.contains(&proto::province_of(r, size)),
    };
    let list: Vec<proto::ShardInfo> = regions_in_view(chunk)
        .filter(|r| mine(*r))
        .flat_map(|r| ready_infos(room, r))
        .collect();
    let province_here = room.province.is_some();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for i in &list {
        (i.region, i.slot, i.node, &i.addr, i.key, i.epoch).hash(&mut h);
    }
    // (Jamais zéro : zéro veut dire « rien reçu », et une liste vide ne s'envoie pas à
    // qui n'a jamais rien reçu.)
    let print = h.finish() | 1;
    let had = room.shards_sent.get(&id).copied().unwrap_or(0);
    if had == print || (had == 0 && list.is_empty()) {
        return;
    }
    room.shards_sent.insert(id, print);
    let Some(c) = st.conns.get(&id) else { return };
    if province_here {
        transport::push(&c.tx, &Response::Shards { list });
        return;
    }
    let pseudo = c.pseudo.clone().unwrap_or_default();
    let expires = now_secs() + 24 * 3600;
    // Certificat de la clé du compte : ses modifications signées seront reconnues dans
    // les régions souveraines.
    let cert = st
        .registry
        .lookup(&pseudo)
        .map(|r| sovereign::Cert::issue(&st.cert_key, rid, &pseudo, r.1, expires));
    transport::push(
        &c.tx,
        &Response::ShardTicket {
            room: rid,
            sig: sign_ticket(st, rid, &pseudo, expires),
            pseudo,
            expires,
            cert,
            root: st.cert_key.verifying_key().to_bytes(),
        },
    );
    transport::push(&c.tx, &Response::Shards { list });
}

/// L'hôte d'un relais de région doit-il passer la main ? Oui si son joueur n'est plus en
/// jeu, si sa partie FPS approche de sa fin, ou s'il est entré dans la région qu'il sert.
/// (Avant la v128 la question était celle des garants FPS : un hôte qui jouait dans un
/// monde de construction était toujours « sur le départ », et remplacé à chaque entretien
/// dès qu'un autre hôte était libre.)
/// Vérifications qui passent en revue toutes les connexions (hôtes dont le joueur est parti
/// du jeu, ou joue depuis longtemps ailleurs) : au plus toutes les…
const SLOW_CHECK: Duration = Duration::from_secs(20);

/// Le joueur de cet hôte joue-t-il dans la région qu'il sert (d'après ce relais) ?
fn host_in_region(st: &State, room: &Room, region: [i32; 2], host: u64) -> bool {
    st.conns
        .get(&host)
        .and_then(|c| c.pseudo.as_deref())
        .and_then(|p| room.by_pseudo.get(&p.to_ascii_lowercase()))
        .and_then(|id| room.build.positions.get(id))
        .is_some_and(|c| proto::region_of(*c) == region)
}

fn host_leaving(st: &State, index: &PlayersIndex, rid: u64, region: [i32; 2], host: u64) -> bool {
    let Some(p) = st.conns.get(&host).and_then(|c| c.pseudo.as_deref()) else {
        return true;
    };
    let Some(&(player, Some(own))) = index.get(&p.to_lowercase()) else {
        return true;
    };
    match st.rooms.get(&own) {
        None => true,
        Some(r) if r.kind == 0 => r.started_at.is_some_and(|t| t.elapsed() > GARANT_RETIRE),
        Some(r) => {
            own == rid
                && r.build
                    .positions
                    .get(&player)
                    .is_none_or(|c| proto::region_of(*c) == region)
        }
    }
}

/// L'hôte du prochain relais d'une région : la connexion de garant d'un joueur en jeu, à
/// bonne connexion, de la même version du jeu que le monde. Dans l'ordre : un joueur d'un
/// autre monde de construction (il reste longtemps), d'une partie FPS récente, puis un
/// joueur de ce monde qui se trouve dans une autre région.
///
/// Depuis la v127 un hôte ne décide plus rien (il transmet ce que publie ce relais) : il
/// n'a plus à être sans lien avec tous les joueurs du monde, ce qui se vérifiait joueur
/// par joueur pour chaque candidat, et supposait qu'il existe des joueurs ailleurs. Un
/// monde où tout le monde joue trouve maintenant ses hôtes parmi ses propres joueurs.
fn pick_shard_host(st: &State, index: &PlayersIndex, rid: u64, region: [i32; 2]) -> Option<u64> {
    pick_host(st, index, rid, &|chunk| proto::region_of(chunk) != region)
}

/// Une connexion de garant libre pour servir une partie de ce monde : `outside` dit si un
/// joueur de ce monde placé dans ce chunk peut la servir (pas celui d'un joueur de la
/// région, ou de la province, qu'il servirait).
fn pick_host(
    st: &State,
    index: &PlayersIndex,
    rid: u64,
    outside: &dyn Fn([i32; 2]) -> bool,
) -> Option<u64> {
    let room = st.rooms.get(&rid)?;
    let want = st.conns.get(&room.host).map_or(0, |c| c.version);
    st.conns
        .iter()
        .filter(|(_, c)| {
            c.garant_only
                && c.garant
                && !c.garant_banned
                && !c.kicked
                && c.room.is_none()
                && c.arbitrating.is_none()
                && c.version == want
                && !link_bad(c)
        })
        .filter_map(|(&g, c)| {
            let p = c.pseudo.as_deref()?;
            if st.stats.ratio(p) > 0.25 {
                return None;
            }
            let &(player, own) = index.get(&p.to_lowercase())?;
            let own = own?;
            let r = st.rooms.get(&own)?;
            let bonus = if own == rid {
                // Un joueur de ce monde : pas celui d'un joueur de la région elle-même.
                let at = r.build.positions.get(&player)?;
                if !outside(*at) {
                    return None;
                }
                1.0
            } else if r.kind == 1 {
                3.0
            } else if r.started_at.is_none_or(|t| t.elapsed() < GARANT_FRESH) {
                2.0
            } else {
                return None;
            };
            Some((g, zones::aptitude(&candidate(st, g)?) + bonus * 1_000_000.0))
        })
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(g, _)| g)
}

/// Régions d'un monde de construction : une région peuplée reçoit ses propres relais, que
/// font tourner des joueurs (voir `pick_shard_host`), relevés quand ils s'en vont. Les
/// joueurs reçoivent les relais des régions qu'ils ont en vue et leur billet ; l'historique
/// de la région part chez l'hôte.
fn upkeep_regions(st: &mut State, rid: u64, index: &mut Option<PlayersIndex>) {
    let size = provinces::size(st);
    let from = st
        .shard_from
        .load(std::sync::atomic::Ordering::Relaxed)
        .max(1);
    // Joueurs par région : ceux que ce relais voit (comptés à chaque déplacement) hors des
    // provinces servies par un relais de province, et (v140) ceux que compte chaque
    // province servie : leurs positions ne passent plus par ici.
    let Some(room) = st.rooms.get_mut(&rid) else {
        return;
    };
    let delegated = room.delegated.clone();
    let counted = provinces::counted_provinces(room);
    let in_province = |r: [i32; 2]| delegated.contains(&proto::province_of(r, size));
    let mut count: HashMap<[i32; 2], usize> = room
        .build
        .region_counts()
        .iter()
        .filter(|(r, _)| !counted.contains(&proto::province_of(**r, size)))
        .map(|(r, n)| (*r, *n as usize))
        .collect();
    let mut inside: HashSet<([i32; 2], u16)> = HashSet::new();
    for p in &counted {
        let pr = &room.provinces[p];
        count.extend(pr.counts.iter().flatten().map(|(r, n)| (*r, *n as usize)));
        inside.extend(pr.inside.iter().copied());
    }
    let room = st.rooms.get(&rid).expect("partie");
    // (La plupart des mondes : aucune région peuplée, aucun relais, rien à faire.)
    if room.shards.is_empty() && room.shards_dirty.is_empty() && count.values().all(|&n| n < from) {
        return;
    }
    let share = st
        .shard_share
        .load(std::sync::atomic::Ordering::Relaxed)
        .max(1);
    // Hôtes à relever : celui dont le joueur est entré dans la région qu'il sert (vu ici,
    // ou dit par sa province), à chaque entretien ; celui dont le joueur est parti du jeu
    // ou joue depuis longtemps une partie FPS, toutes les `SLOW_CHECK` (v140 : il faut
    // pour cela passer en revue toutes les connexions).
    let slow = st.slow_check.elapsed() >= SLOW_CHECK;
    if slow {
        *index = Some(players_index_fresh(st));
    }
    // Relais à ajouter : régions peuplées qui n'en ont pas assez pour leur foule, et hôtes
    // qui doivent passer la main (remplacés, ou seulement relevés si la région en a
    // assez sans eux).
    let mut wanted: Vec<([i32; 2], Option<u64>)> = Vec::new();
    let mut relieved: Vec<u64> = Vec::new();
    for (&r, &n) in &count {
        let have = room.shards.get(&r).map_or(0, Vec::len);
        for _ in have..shards_wanted(n, from, share) {
            wanted.push((r, None));
        }
    }
    for (&r, list) in &room.shards {
        let need = shards_wanted(count.get(&r).copied().unwrap_or(0), from, share).max(1);
        let mut ready = list.iter().filter(|s| s.info.is_some()).count();
        for sh in list {
            let replaced = list.iter().any(|o| o.previous == Some(sh.host));
            if sh.info.is_none() || replaced {
                continue;
            }
            let entered = if in_province(r) {
                inside.contains(&(r, sh.slot))
            } else {
                host_in_region(st, room, r, sh.host)
            };
            let leaving = entered
                || index
                    .as_ref()
                    .is_some_and(|idx| host_leaving(st, idx, rid, r, sh.host));
            if !leaving {
                continue;
            }
            if ready > need {
                ready -= 1;
                relieved.push(sh.host);
            } else {
                wanted.push((r, Some(sh.host)));
            }
        }
    }
    wanted.sort();
    if slow {
        st.slow_check = Instant::now();
    }
    // (Plus d'hôte libre au dernier essai : on attend un peu avant de chercher de nouveau,
    // v140 ; chercher demande l'index de tous les joueurs.)
    if st.hosts_wait.is_some_and(|t| t.elapsed() < HOSTS_WAIT) {
        wanted.clear();
    }
    if !wanted.is_empty() && index.is_none() {
        *index = Some(players_index(st));
    }
    let empty = PlayersIndex::default();
    let index = index.as_ref().unwrap_or(&empty);
    for (region, previous) in wanted {
        // (Province en train d'être confiée à un relais de province : ses relais de région
        // se rattacheront à lui.)
        if provinces::starting(st, rid, region) {
            continue;
        }
        // Plus d'hôte libre : la région reste servie par ce relais (ou par les relais
        // qu'elle a déjà) jusqu'à ce qu'il y en ait un.
        let Some(g) = pick_shard_host(st, index, rid, region) else {
            st.hosts_wait = Some(Instant::now());
            break;
        };
        let pseudo = st
            .conns
            .get(&g)
            .and_then(|c| c.pseudo.clone())
            .unwrap_or_default();
        let room = st.rooms.get_mut(&rid).expect("partie");
        room.members.push(g);
        room.hidden.insert(g);
        let slot = room.shard_slot;
        room.shard_slot = room.shard_slot.wrapping_add(1);
        room.shards.entry(region).or_default().push(RegionShard {
            host: g,
            slot,
            info: None,
            previous,
        });
        // Région d'une province servie par un relais de province (v138) : le relais de
        // région s'y rattache, et c'est lui qui donne l'historique.
        let province = provinces::shard_at(st, rid, region, slot, &pseudo);
        let room = st.rooms.get_mut(&rid).expect("partie");
        let history: Vec<([i32; 2], Vec<Vec<u8>>)> = if province.is_some() {
            Vec::new()
        } else {
            room.build.logs_in(|zone| {
                proto::region_of([zone[0] * proto::ZONE, zone[1] * proto::ZONE]) == region
            })
        };
        // (v155) Son billet pour les autres relais de la région (une semaine), à la place
        // du secret du monde.
        let peer_expires = now_secs() + 7 * 24 * 3600;
        let peer = sign_ticket(st, rid, &proto::shard_peer_name(slot), peer_expires);
        if let Some(c) = st.conns.get_mut(&g) {
            c.room = Some(rid);
            transport::push(
                &c.tx,
                &Response::ShardTask {
                    room: rid,
                    region,
                    peer,
                    peer_expires,
                    slot,
                    root: st.cert_key.verifying_key().to_bytes(),
                    province,
                },
            );
            for (zone, log) in history {
                let chunk = [zone[0] * proto::ZONE, zone[1] * proto::ZONE];
                for data in log {
                    push_zone(&c.tx, chunk, data);
                }
            }
        }
        log::info!("monde {rid} : région {region:?} confiée au relais de {g} (rang {slot})");
    }
    // Hôtes relevés sans remplaçant : la région a assez de relais sans eux.
    for old in relieved {
        leave_room(st, old);
        if let Some(c) = st.conns.get(&old) {
            transport::push(&c.tx, &Response::RoomClosed);
        }
    }
    // La liste des relais d'une région a changé : aux joueurs qui ont cette région en vue.
    let Some(room) = st.rooms.get_mut(&rid) else {
        return;
    };
    let dirty = std::mem::take(&mut room.shards_dirty);
    if dirty.is_empty() {
        return;
    }
    // (Cherchés autour de chaque région changée, plus parmi tous les joueurs.)
    let mut players: HashSet<u64> = HashSet::new();
    for r in &dirty {
        let lo = [
            r[0] * proto::REGION - SHARD_VIEW,
            r[1] * proto::REGION - SHARD_VIEW,
        ];
        let hi = [
            (r[0] + 1) * proto::REGION - 1 + SHARD_VIEW,
            (r[1] + 1) * proto::REGION - 1 + SHARD_VIEW,
        ];
        players.extend(room.build.players_in(lo, hi));
    }
    let mut players: Vec<u64> = players.into_iter().collect();
    players.sort_unstable();
    for m in players {
        send_shards(st, rid, m);
    }
}

/// Mondes de construction : chacun reçoit ses garants extérieurs, à qui les zones passent.
fn upkeep_build(st: &mut State) {
    // Positions des joueurs sur les serveurs (au plus une écriture par entretien).
    st.servers.save_positions();
    st.bags.save_if_dirty();
    let rooms: Vec<u64> = st
        .rooms
        .iter()
        .filter(|(_, r)| r.kind == 1)
        .map(|(&id, _)| id)
        .collect();
    let mut index = None;
    for rid in rooms {
        // Zones restées sans servir : leur historique s'endort sur le disque.
        if let Some(room) = st.rooms.get_mut(&rid) {
            room.build.sleep_cold(zones::COLD_AFTER);
        }
        // Relais de province (v138) : les époques, lots et vérifications de ses régions ;
        // le relais du monde en choisit les hôtes et garde le reste. (v143) Un pays confie
        // lui-même ses provinces peuplées.
        if st.rooms.get(&rid).is_some_and(|r| r.province.is_some()) {
            provinces::upkeep(st, rid, &mut index);
            upkeep_sovereignty(st, rid);
            upkeep_audits(st, rid);
            continue;
        }
        // Propriétaire à la connexion pourrie : le monde passe au mieux connecté.
        rebalance_host(st, rid);
        upkeep_regions(st, rid, &mut index);
        provinces::upkeep(st, rid, &mut index);
        upkeep_sovereignty(st, rid);
        upkeep_audits(st, rid);
        upkeep_standby(st, rid);
        flush_replica(st, rid);
        // Le propriétaire (qui enregistre le monde) reçoit ce qui a changé, par paquets.
        if let Some(room) = st.rooms.get_mut(&rid) {
            let owner = room.host;
            let items = room.build.catch_up(owner, None, &|_| true);
            if let Some(c) = st.conns.get(&owner) {
                for (zone, log) in items {
                    let chunk = [zone[0] * proto::ZONE, zone[1] * proto::ZONE];
                    for data in log {
                        push_zone(&c.tx, chunk, data);
                    }
                }
            }
        }
    }
}

/// Pause d'une partie FPS : les absences s'arrêtent pendant la pause (et reprennent
/// ensuite), le temps de pause ne compte pas dans la durée de la partie.
fn room_paused(r: &mut Room, paused: bool) {
    let now = Instant::now();
    if paused && r.paused_since.is_none() {
        r.paused_since = Some(now);
        for (p, (since, total)) in &mut r.away {
            if let Some(t) = since.take() {
                *total += t.elapsed().as_secs_f64();
                r.paused_away.insert(p.clone());
            }
        }
    } else if !paused {
        if let Some(t) = r.paused_since.take() {
            r.paused_total += t.elapsed().as_secs_f64();
        }
        for p in std::mem::take(&mut r.paused_away) {
            if let Some(e) = r.away.get_mut(&p) {
                e.0.get_or_insert(now);
            }
        }
    }
}

/// Règle les absences d'une partie FPS (une fois) : chaque joueur du début parti trop
/// longtemps perd en confiance (rien sous 2 minutes, beaucoup vers 10 minutes sur 30,
/// abandon jusqu'à la fin). `spared` : exclus par la partie (coéquipiers tués), déjà
/// punis par l'exclusion. Rend les joueurs pénalisés.
fn settle_absences(st: &mut State, room: &mut Room, spared: &[String]) -> HashSet<String> {
    let mut hit = HashSet::new();
    if room.settled || room.kind != 0 {
        return hit;
    }
    let Some(t0) = room.started_at else {
        return hit;
    };
    room.settled = true;
    let pause = room.paused_total + room.paused_since.map_or(0.0, |t| t.elapsed().as_secs_f64());
    let len = (t0.elapsed().as_secs_f64() - pause).max(1.0);
    for (p, (since, total)) in &room.away {
        let away = total + since.map_or(0.0, |t| t.elapsed().as_secs_f64());
        let pen = stats::Stats::absence_penalty(away, len);
        if pen == 0 || spared.contains(p) {
            continue;
        }
        let s = st.stats.get_mut(p);
        s.trust_soft(pen);
        s.absences += 1;
        st.stats.save(p);
        hit.insert(p.clone());
        log::info!("« {p} » absent {away:.0} s sur {len:.0} s : confiance {pen}");
    }
    // Signalements : comptés seulement si au moins deux joueurs de la partie ont signalé
    // le même (un mauvais perdant seul ne coûte rien) ; baisse douce, jusqu'au plancher.
    let mut by_target: HashMap<&String, usize> = HashMap::new();
    for (_, target) in &room.reports {
        *by_target.entry(target).or_default() += 1;
    }
    for (target, n) in by_target {
        if n < stats::trust::REPORTERS {
            continue;
        }
        let s = st.stats.get_mut(target);
        s.trust_soft(stats::trust::REPORT * n as i32);
        s.reports += n as u32;
        st.stats.save(target);
        hit.insert(target.clone());
        log::info!("« {target} » signalé par {n} joueurs de la partie");
    }
    // Soupçons du vérificateur : la seule baisse, avec la triche prouvée, qui passe sous le
    // plancher des trolls.
    for p in &room.suspects {
        let s = st.stats.get_mut(p);
        s.trust_by(stats::trust::SUSPICION);
        st.stats.save(p);
        hit.insert(p.clone());
        log::info!("« {p} » soupçonné par le vérificateur");
    }
    hit
}

/// Facteur de confiance effectif : le plus bas des comptes vus sur ses machines (un
/// nouveau compte ne blanchit pas un tricheur).
fn trust_of(st: &State, pseudo: &str) -> i32 {
    st.hardware
        .siblings(pseudo)
        .iter()
        .map(|p| st.stats.trust(p))
        .min()
        .unwrap_or(stats::TRUST_START)
}

/// Bannit un compte pour triche : registre (tous les relais), machines 3 mois,
/// connexions coupées.
fn ban_cheater(st: &mut State, target: &str, reason: &str) -> Result<usize, String> {
    let Some(rec) = st.accounts.load(target) else {
        return Err(format!("Joueur « {target} » inconnu."));
    };
    if !st.registry.is_banned(&rec.pubkey) {
        use sha2::{Digest, Sha256};
        let proof: [u8; 32] = Sha256::digest(reason.as_bytes()).into();
        st.registry
            .append(Body::Ban {
                pubkey: rec.pubkey,
                proof,
            })
            .map_err(|e| format!("{e:#}"))?;
    }
    let machines = st.hardware.ban_account_machines(&rec.pseudo);
    let ids: Vec<u64> = st
        .conns
        .iter()
        .filter(|(_, c)| {
            c.pseudo
                .as_deref()
                .is_some_and(|p| p.eq_ignore_ascii_case(target))
        })
        .map(|(&i, _)| i)
        .collect();
    for i in ids {
        leave_room(st, i);
        if let Some(c) = st.conns.get_mut(&i) {
            c.kicked = true;
            transport::push(&c.tx, &err(ErrorCode::Banned, "Compte banni."));
        }
    }
    Ok(machines)
}

/// Le résultat d'une partie est accepté quand plus de la moitié des joueurs présents au
/// début (l'hôte compris) ont envoyé le même.
fn try_apply_result(st: &mut State, room_id: u64) {
    let Some(room) = st.rooms.get_mut(&room_id) else {
        return;
    };
    if room.applied {
        return;
    }
    let need = room.roster.len().max(1) / 2 + 1;
    let Some((hash, summary)) = room
        .results
        .iter()
        .find(|(_, (_, who))| who.len() >= need)
        .map(|(h, (m, _))| (*h, m.clone()))
    else {
        return;
    };
    // Un garant vérificateur suit la partie : il faut aussi son accord (une majorité
    // complice de l'hôte ne peut pas imposer un faux résultat).
    if room.ready_standby.iter().any(|g| room.hidden.contains(g)) {
        match room.audited {
            None => return,
            Some(a) if a != hash => {
                log::warn!("partie {room_id} : résultat refusé par le vérificateur");
                return;
            }
            Some(_) => {}
        }
    }
    room.applied = true;
    // Statistiques impossibles (plus de têtes que d'éliminations, plus de dégâts que la
    // vie de tous les adversaires…) : résultat faux, rien n'est compté.
    let team = (room.max_players / 2).max(1);
    if let Some((p, why)) = summary
        .players
        .iter()
        .find_map(|p| stats::impossible(p, team).map(|w| (p.pseudo.clone(), w)))
    {
        log::warn!(
            "partie {room_id} : résultat refusé, statistiques impossibles pour « {p} » ({why})"
        );
        return;
    }
    let sig = room.map_sig;
    // Seules les parties à 5 contre 5 comptent pour le classement.
    let ranked = room.max_players == proto::FPS_PLAYERS as u32;
    // Classement : seulement sur les 10 cartes les plus jouées.
    let ranked_map = st.maps.among_top(&room.map_sig, 10);
    let room = st.rooms.get_mut(&room_id).expect("partie");
    let roster = room.roster.clone();
    // Exclus pour avoir tué 3 coéquipiers : pas de pénalité d'absence en plus.
    let tk_kicked: Vec<String> = summary
        .players
        .iter()
        .filter(|p| p.teamkills >= 3 || p.kicked)
        .map(|p| p.pseudo.to_ascii_lowercase())
        .collect();
    let mut room = st.rooms.remove(&room_id).expect("partie");
    let absent = settle_absences(st, &mut room, &tk_kicked);
    st.rooms.insert(room_id, room);
    for p in &summary.players {
        let s = st.stats.get_mut(&p.pseudo);
        s.teamkills += p.teamkills as u32;
    }
    // Partie propre (pas de signalement compté, pas parti longtemps, pas de soupçon) : la
    // confiance remonte un peu.
    for p in &roster {
        let low = p.to_ascii_lowercase();
        if ranked && !absent.contains(&low) {
            let s = st.stats.get_mut(p);
            let g = s.clean_gain();
            s.trust_by(g);
        }
        st.stats.save(p);
    }
    if ranked && ranked_map {
        st.stats.apply_match(&summary);
    }
    // Statistiques invraisemblables (au-delà des records des meilleurs joueurs) : comme
    // un soupçon du vérificateur, une fois par partie.
    for p in &summary.players {
        if let Some(why) = stats::implausible(p) {
            let s = st.stats.get_mut(&p.pseudo);
            s.odd_games += 1;
            s.trust_by(stats::trust::ODD_STATS);
            st.stats.save(&p.pseudo);
            log::warn!("« {} » : statistiques invraisemblables ({why})", p.pseudo);
        }
    }
    st.maps.played(&sig);
    log::info!(
        "partie {room_id} terminée {}-{} : classement mis à jour",
        summary.score[0],
        summary.score[1]
    );
}

/// Ouvre une partie hébergée par `id`.
#[allow(clippy::too_many_arguments)]
fn open_room(
    st: &mut State,
    id: u64,
    pseudo: &str,
    kind: u8,
    name: String,
    map: String,
    map_sig: [u8; 32],
    max_players: u16,
) -> u64 {
    leave_room(st, id);
    let rid = st.next_id;
    st.next_id += 1;
    if kind == 0 {
        st.maps.mark_fps(map_sig);
    }
    let name: String = name.chars().take(32).collect();
    // FPS : 10 joueurs ; construction : sans limite (0).
    let max_players = if kind == 0 {
        (max_players.clamp(2, proto::FPS_MAX_PLAYERS) & !1) as u32
    } else if max_players == 0 {
        0
    } else {
        max_players.max(2) as u32
    };
    st.rooms.insert(
        rid,
        Room {
            kind,
            name: if name.trim().is_empty() {
                format!("Partie de {pseudo}")
            } else {
                name
            },
            map: map.chars().take(64).collect(),
            max_players,
            host: id,
            host_pseudo: pseudo.to_string(),
            version: st.conns.get(&id).map_or(0, |c| c.version),
            members: Vec::new(),
            started: false,
            map_sig,
            roster: Vec::new(),
            results: HashMap::new(),
            applied: false,
            started_at: None,
            away: HashMap::new(),
            paused_since: None,
            paused_total: 0.0,
            paused_away: HashSet::new(),
            settled: false,
            reserved: Vec::new(),
            reports: HashSet::new(),
            suspects: HashSet::new(),
            shards: HashMap::new(),
            shard_slot: 0,
            shard_secret: {
                let mut k = [0u8; 32];
                rand::rngs::OsRng.fill_bytes(&mut k);
                k
            },
            shards_dirty: HashSet::new(),
            shards_sent: HashMap::new(),
            sov: HashMap::new(),
            sov_past: HashMap::new(),
            sov_skip: HashMap::new(),
            next_epoch: 1,
            root_done: std::collections::BTreeSet::new(),
            audits: HashMap::new(),
            auditors: Vec::new(),
            audit_misses: HashMap::new(),
            next_audit: 1,
            needs: HashMap::new(),
            needs_of: HashMap::new(),
            unserved: HashSet::new(),
            handovers: Vec::new(),
            by_pseudo: HashMap::new(),
            pseudo_of: HashMap::new(),
            build: zones::Build::default(),
            backup: None,
            hidden: std::collections::BTreeSet::new(),
            ready_standby: Vec::new(),
            garant_hosted: false,
            standby: None,
            guards: None,
            rebalanced: None,
            judges: HashMap::new(),
            confirmed: 0,
            dismissed: 0,
            audited: None,
            evidence: VecDeque::new(),
            evidence_parts: Vec::new(),
            arbitration: None,
            ban_review: None,
            creator: pseudo.to_string(),
            server: servers::key(pseudo, &map),
            provinces: HashMap::new(),
            provinces_sent: HashMap::new(),
            delegated: Arc::default(),
            bag_holders: HashMap::new(),
            bag_waiting: HashMap::new(),
            bag_deltas: HashMap::new(),
            province: None,
        },
    );
    if let Some(c) = st.conns.get_mut(&id) {
        c.room = Some(rid);
        c.joined = Instant::now();
    }
    // Monde de construction : ses zones froides dormiront sur le disque du relais.
    if kind == 1 {
        let dir = st.cfg.dir.join("zones").join(rid.to_string());
        if let Some(room) = st.rooms.get_mut(&rid) {
            room.build.set_page_dir(dir);
            room.name_player(id, pseudo);
        }
    }
    log::info!("partie {rid} ouverte par « {pseudo} »");
    rid
}

/// Une seule partie FPS à la fois par machine (coupé pour les essais où tous les joueurs
/// simulés sont sur la même machine).
pub static ONE_FPS_PER_MACHINE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(true);

/// Nom montré aux joueurs à la place du pseudo d'un garant (ceux qui servent les parties
/// et les régions restent anonymes).
const GARANT_NAME: &str = "garant";

/// Autre joueur (autre compte) de cette machine dans une partie FPS en ligne (en attente
/// ou commencée), s'il y en a un.
fn fps_elsewhere_on_machine(st: &State, id: u64, pseudo: &str) -> Option<String> {
    if !ONE_FPS_PER_MACHINE.load(std::sync::atomic::Ordering::Relaxed) {
        return None;
    }
    let mine = &st.conns.get(&id)?.hw;
    if mine.is_empty() {
        return None;
    }
    st.conns.iter().find_map(|(&o, c)| {
        let p = c.pseudo.as_deref()?;
        let busy = o != id
            && !c.garant_only
            && !p.eq_ignore_ascii_case(pseudo)
            && c.room
                .and_then(|r| st.rooms.get(&r))
                .is_some_and(|r| r.kind == 0 && !r.applied)
            && minelib_net::hw::same_machine(&c.hw, mine);
        busy.then(|| p.to_string())
    })
}

/// Version du jeu des joueurs nommés (le premier trouvé) ; 0 : inconnue.
fn players_version(st: &State, players: &[String]) -> u32 {
    st.conns
        .values()
        .filter(|c| !c.garant_only)
        .find(|c| {
            c.pseudo
                .as_deref()
                .is_some_and(|p| players.iter().any(|q| q.eq_ignore_ascii_case(p)))
        })
        .map_or(0, |c| c.version)
}

fn join_room(st: &mut State, id: u64, pseudo: &str, room: u64) -> Response {
    // FPS : même version du jeu que la partie, et une seule partie par machine.
    let (kind, room_version) = match st.rooms.get(&room) {
        Some(r) => (r.kind, r.version),
        None => return err(ErrorCode::Refused, "Partie introuvable."),
    };
    if kind == 0 {
        let me = st.conns.get(&id);
        let version = me.map_or(0, |c| c.version);
        if room_version != version {
            return err(
                ErrorCode::Refused,
                format!(
                    "Cette partie se joue en version {} (ton jeu : {version}).",
                    room_version
                ),
            );
        }
        if let Some(p) = fps_elsewhere_on_machine(st, id, pseudo) {
            return err(
                ErrorCode::Refused,
                format!("Une partie FPS est déjà en cours sur cet ordinateur (« {p} »)."),
            );
        }
    }
    let Some(r) = st.rooms.get_mut(&room) else {
        return err(ErrorCode::Refused, "Partie introuvable.");
    };
    // Une place gardée pour lui (lobby) : la sienne ; sinon les places gardées comptent.
    let low = pseudo.to_ascii_lowercase();
    let mine = r.reserved.iter().any(|(p, _)| *p == low);
    let held = if mine { 0 } else { r.reserved_count() };
    if r.max_players > 0 && r.player_count() + held >= r.max_players as usize {
        return err(ErrorCode::Refused, "Partie pleine.");
    }
    // Une partie FPS commencée : seuls ses joueurs du début peuvent y revenir.
    let back = r.roster.iter().any(|p| p.eq_ignore_ascii_case(pseudo));
    if r.kind == 0 && r.started && !back {
        return err(ErrorCode::Refused, "Partie déjà commencée.");
    }
    r.paused_away.remove(&pseudo.to_ascii_lowercase());
    if let Some((since, total)) = r.away.get_mut(&pseudo.to_ascii_lowercase()) {
        if let Some(t) = since.take() {
            *total += t.elapsed().as_secs_f64();
        }
    }
    if r.kind == 1 {
        if st.servers.whitelisted_out(&r.server, &r.creator, pseudo) {
            return err(
                ErrorCode::Refused,
                "Ce serveur est sur liste blanche : demande à un OP de t'y ajouter.",
            );
        }
        if let Some(why) = st.servers.banned(&r.server, pseudo) {
            let why = if why.is_empty() {
                String::new()
            } else {
                format!(" ({why})")
            };
            return err(
                ErrorCode::Refused,
                format!("Tu es banni de ce serveur{why}."),
            );
        }
        // Un nouveau compte sur la machine d'un banni ne passe pas non plus.
        let server = r.server.clone();
        if st
            .servers
            .bans(&server)
            .iter()
            .any(|b| st.hardware.share_machine(b, pseudo))
        {
            return err(
                ErrorCode::Refused,
                "Un compte de cette machine est banni de ce serveur.",
            );
        }
    }
    r.reserved
        .retain(|(p, t)| *p != low && t.elapsed() < Duration::from_secs(60));
    r.members.push(id);
    if r.kind == 1 {
        r.unserved.insert(id);
        r.name_player(id, pseudo);
    }
    // FPS : pas de nom tant que la partie n'a pas commencé (on ne choisit pas ses
    // adversaires en quittant la recherche).
    let shown = if r.kind == 0 {
        String::new()
    } else {
        r.host_pseudo.clone()
    };
    let (host, host_pseudo, map, map_sig) = (r.host, shown, r.map.clone(), r.map_sig);
    let party = st.conns.get(&id).and_then(|c| c.party).unwrap_or(0);
    if let Some(c) = st.conns.get_mut(&id) {
        c.room = Some(room);
        c.joined = Instant::now();
    }
    if let Some(c) = st.conns.get(&host) {
        transport::push(
            &c.tx,
            &Response::PeerJoined {
                peer: id,
                pseudo: pseudo.to_string(),
                party,
            },
        );
    }
    // Monde avec un relais de secours prêt : il le saura (après la réponse).
    if let Some(at) = st
        .rooms
        .get(&room)
        .and_then(|r| r.standby.as_ref())
        .and_then(|s| s.at.clone())
    {
        let (st2, id2) = (&*st, id);
        let pending = st2.conns.get(&id2).map(|c| c.tx.clone());
        if let (Some(tx), Some(t)) = (pending, ticket_for(st2, room, id2)) {
            // (Déposés dans la file : ils partent après la réponse.)
            transport::push(&tx, &t);
            transport::push(&tx, &Response::Standby { at: Some(at) });
        }
    }
    Response::Joined {
        room,
        host: host_pseudo,
        map,
        map_sig,
    }
}

/// Recherche FPS : une partie en attente sur cette carte, sans personne qu'on a signalé
/// (ou qui nous a signalés), avec une réputation proche : plus on est signalé (par partie
/// jouée), plus on est mis avec des joueurs dans le même cas. Parmi celles-là, la plus
/// proche en réputation, puis la plus remplie.
fn find_room(st: &State, group: &[String], map_sig: [u8; 32], version: u32) -> Option<u64> {
    let size = group.len().max(1);
    let stats_of = |p: &str| {
        st.stats
            .get(p)
            .cloned()
            .unwrap_or_else(|| stats::Stats::new(p))
    };
    let mine: Vec<stats::Stats> = group.iter().map(|p| stats_of(p)).collect();
    // Un lobby compte comme son plus bas facteur de confiance, son Elo moyen, et il est
    // « neuf » si la plupart de ses joueurs le sont.
    let my_trust = group
        .iter()
        .map(|p| trust_of(st, p))
        .min()
        .unwrap_or(stats::TRUST_START);
    let my_elo = mine.iter().map(|s| s.elo).sum::<f64>() / size as f64;
    let my_new = mine.iter().filter(|s| s.is_new()).count() * 2 >= size;
    // Clé de choix : d'abord l'écart d'Elo avec l'équipe déjà formée (par tranches de 100,
    // pour laisser jouer les critères suivants), puis neufs avec neufs, puis la confiance,
    // puis la partie la plus remplie.
    let mut best: Option<((i64, u8, i32, usize), u64)> = None;
    for (&rid, r) in &st.rooms {
        if r.kind != 0 || r.started || r.map_sig != map_sig || r.version != version {
            continue;
        }
        let n = r.player_count() + r.reserved_count();
        if n + size > r.max_players as usize {
            continue;
        }
        // Le lobby doit tenir dans une équipe (les autres se répartissent à parts égales).
        let team_max = (r.max_players as usize / 2).max(1);
        if size > 1 && size > team_max.saturating_sub(n / 2) {
            continue;
        }
        let names: Vec<&str> = r
            .everyone()
            .filter_map(|m| st.conns.get(&m).and_then(|c| c.pseudo.as_deref()))
            .collect();
        let blocked = names.iter().any(|p| {
            mine.iter().any(|me| me.avoids(p))
                || st
                    .stats
                    .get(p)
                    .is_some_and(|s| group.iter().any(|g| s.avoids(g)))
        });
        if blocked {
            continue;
        }
        // Confiance moyenne de la partie, et part de comptes neufs.
        let trust = names.iter().map(|p| trust_of(st, p)).sum::<i32>() / names.len().max(1) as i32;
        let news = names
            .iter()
            .filter(|p| st.stats.get(p).is_none_or(|s| s.is_new()))
            .count();
        let mostly_new = news * 2 >= names.len().max(1);
        if (trust - my_trust).abs() > trust_tolerance(my_trust, trust) {
            continue;
        }
        // Elo moyen de ceux qui attendent déjà (l'équipe formée en secret).
        let elo = names
            .iter()
            .map(|p| st.stats.get(p).map_or(stats::START_ELO, |s| s.elo))
            .sum::<f64>()
            / names.len().max(1) as f64;
        let key = (
            ((elo - my_elo).abs() / 100.0) as i64,
            u8::from(mostly_new != my_new),
            (trust - my_trust).abs(),
            usize::MAX - n,
        );
        if best.as_ref().is_none_or(|(k, _)| key < *k) {
            best = Some((key, rid));
        }
    }
    best.map(|b| b.1)
}

/// Fichier du skin d'un joueur (nom tiré du pseudo : rien de dangereux dans le chemin).
fn skin_path(dir: &Path, pseudo: &str) -> PathBuf {
    use sha2::{Digest, Sha256};
    let h: [u8; 32] = Sha256::digest(pseudo.to_ascii_lowercase().as_bytes()).into();
    dir.join("skins")
        .join(format!("{}.png", crypto::hex(&h[..12])))
}

/// Un skin acceptable : PNG de 64 × 64 ou 64 × 32, 24 Ko au plus (en-tête vérifié ; le jeu
/// qui l'affiche le décode et le revérifie).
fn valid_skin(png: &[u8]) -> bool {
    const MAGIC: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    if png.len() > 24 * 1024 || png.len() < 33 || png[..8] != MAGIC || &png[12..16] != b"IHDR" {
        return false;
    }
    let w = u32::from_be_bytes([png[16], png[17], png[18], png[19]]);
    let h = u32::from_be_bytes([png[20], png[21], png[22], png[23]]);
    w == 64 && (h == 64 || h == 32)
}

/// Lobby de cette connexion : ses membres (l'hôte d'abord).
fn party_of(st: &State, id: u64) -> Vec<u64> {
    st.conns
        .get(&id)
        .and_then(|c| c.party)
        .and_then(|p| st.parties.get(&p))
        .cloned()
        .unwrap_or_default()
}

/// Envoie à chaque membre l'état de son lobby (dissous s'il ne reste qu'un joueur).
fn party_push(st: &mut State, pid: u64) {
    let members = st.parties.get(&pid).cloned().unwrap_or_default();
    if members.len() <= 1 {
        st.parties.remove(&pid);
        for m in &members {
            if let Some(c) = st.conns.get_mut(m) {
                c.party = None;
                transport::push(
                    &c.tx,
                    &Response::Party {
                        id: 0,
                        members: Vec::new(),
                    },
                );
            }
        }
        return;
    }
    let names: Vec<String> = members
        .iter()
        .filter_map(|m| st.conns.get(m).and_then(|c| c.pseudo.clone()))
        .collect();
    for m in &members {
        if let Some(c) = st.conns.get(m) {
            transport::push(
                &c.tx,
                &Response::Party {
                    id: pid,
                    members: names.clone(),
                },
            );
        }
    }
}

/// Quitte son lobby (déconnexion, départ, exclusion).
fn party_leave(st: &mut State, id: u64) {
    let Some(pid) = st.conns.get_mut(&id).and_then(|c| c.party.take()) else {
        return;
    };
    if let Some(v) = st.parties.get_mut(&pid) {
        v.retain(|&m| m != id);
    }
    if let Some(c) = st.conns.get(&id) {
        transport::push(
            &c.tx,
            &Response::Party {
                id: 0,
                members: Vec::new(),
            },
        );
    }
    party_push(st, pid);
}

/// Connexion d'un joueur en ligne, par son pseudo.
fn conn_of(st: &State, pseudo: &str) -> Option<u64> {
    st.conns
        .iter()
        .find(|(_, c)| {
            !c.kicked
                && !c.garant_only
                && c.pseudo
                    .as_deref()
                    .is_some_and(|p| p.eq_ignore_ascii_case(pseudo))
        })
        .map(|(&i, _)| i)
}

/// Requêtes du lobby.
fn handle_party(st: &mut State, id: u64, pseudo: &str, action: u8, who: &str) -> Response {
    let target = conn_of(st, who);
    let mine = st.conns.get(&id).and_then(|c| c.party);
    let leader = mine
        .and_then(|p| st.parties.get(&p))
        .and_then(|v| v.first().copied());
    match action {
        proto::PARTY_INVITE => {
            let Some(t) = target.filter(|&t| t != id) else {
                return err(ErrorCode::Refused, format!("« {who} » n'est pas en ligne."));
            };
            // Pas d'invitation de quelqu'un qu'on évite (ou qui nous évite).
            let avoid = st.stats.get(pseudo).is_some_and(|s| s.avoids(who))
                || st.stats.get(who).is_some_and(|s| s.avoids(pseudo));
            if avoid {
                return err(
                    ErrorCode::Refused,
                    format!("Impossible d'inviter « {who} »."),
                );
            }
            let pid = match mine {
                Some(p) => p,
                None => {
                    let p = st.next_id;
                    st.next_id += 1;
                    st.parties.insert(p, vec![id]);
                    if let Some(c) = st.conns.get_mut(&id) {
                        c.party = Some(p);
                    }
                    p
                }
            };
            let v = &st.parties[&pid];
            if v.contains(&t) {
                return err(
                    ErrorCode::Refused,
                    format!("« {who} » est déjà dans le lobby."),
                );
            }
            if v.len() >= proto::PARTY_MAX {
                return err(ErrorCode::Refused, "Lobby complet (5 joueurs).");
            }
            if let Some(c) = st.conns.get_mut(&t) {
                // Une seule invitation par lobby (pas de harcèlement à coups d'invitations).
                if !c.invited.contains(&pid) {
                    c.invited.push(pid);
                    if c.invited.len() > 8 {
                        c.invited.remove(0);
                    }
                    transport::push(
                        &c.tx,
                        &Response::PartyInvite {
                            from: pseudo.to_string(),
                        },
                    );
                }
            }
            // Le lobby existe (au moins l'hôte) : on le montre à celui qui invite.
            if let Some(c) = st.conns.get(&id) {
                transport::push(
                    &c.tx,
                    &Response::Party {
                        id: pid,
                        members: st.parties[&pid]
                            .iter()
                            .filter_map(|m| st.conns.get(m).and_then(|c| c.pseudo.clone()))
                            .collect(),
                    },
                );
            }
            Response::Done
        }
        proto::PARTY_ACCEPT | proto::PARTY_DECLINE => {
            let pid = target.and_then(|t| st.conns.get(&t)).and_then(|c| c.party);
            let invited = pid.filter(|p| st.conns.get(&id).is_some_and(|c| c.invited.contains(p)));
            let Some(pid) = invited else {
                return err(ErrorCode::Refused, "Invitation expirée.");
            };
            if let Some(c) = st.conns.get_mut(&id) {
                c.invited.retain(|&p| p != pid);
            }
            if action == proto::PARTY_DECLINE {
                return Response::Done;
            }
            if st
                .parties
                .get(&pid)
                .is_none_or(|v| v.len() >= proto::PARTY_MAX)
            {
                return err(ErrorCode::Refused, "Lobby complet.");
            }
            party_leave(st, id);
            st.parties.get_mut(&pid).expect("lobby").push(id);
            if let Some(c) = st.conns.get_mut(&id) {
                c.party = Some(pid);
            }
            party_push(st, pid);
            Response::Done
        }
        proto::PARTY_LEAVE => {
            party_leave(st, id);
            Response::Done
        }
        proto::PARTY_KICK | proto::PARTY_PROMOTE => {
            if leader != Some(id) {
                return err(ErrorCode::Refused, "Réservé à l'hôte du lobby.");
            }
            let pid = mine.expect("lobby");
            let Some(t) = target.filter(|t| *t != id && st.parties[&pid].contains(t)) else {
                return err(
                    ErrorCode::Refused,
                    format!("« {who} » n'est pas dans le lobby."),
                );
            };
            if action == proto::PARTY_KICK {
                party_leave(st, t);
            } else {
                let v = st.parties.get_mut(&pid).expect("lobby");
                v.retain(|&m| m != t);
                v.insert(0, t);
                party_push(st, pid);
            }
            Response::Done
        }
        _ => err(ErrorCode::Refused, "Action de lobby inconnue."),
    }
}

/// Administrateurs : pseudos listés dans `admins.txt` (un par ligne) dans le dossier du
/// relais.
fn is_admin(dir: &Path, pseudo: &str) -> bool {
    std::fs::read_to_string(dir.join("admins.txt"))
        .unwrap_or_default()
        .lines()
        .any(|l| l.trim().eq_ignore_ascii_case(pseudo))
}

/// Écart de confiance accepté entre un joueur et une partie : large au milieu, serré
/// aux extrêmes (les joueurs très fiables entre eux, les douteux entre eux).
fn trust_tolerance(a: i32, b: i32) -> i32 {
    let lo = a.min(b);
    let hi = a.max(b);
    if hi >= 800 || lo <= 200 {
        120
    } else {
        250
    }
}

/// Requêtes de parties (connecté seulement).
fn handle_room(req: Request, st: &mut State, id: u64, pseudo: &str) -> Option<Response> {
    upkeep(st);
    let my_room = st.conns.get(&id).and_then(|c| c.room);
    Some(match req {
        Request::Servers { kind } => {
            let mut list = local_servers(st, kind, Some(pseudo));
            // Relais fédérés : les parties des autres relais connus, après celles d'ici
            // (le jeu s'y connecte avec le même compte pour les rejoindre).
            let peers = st.peer_servers.lock().unwrap().clone();
            if let Some(f) = peers {
                let mut far = f(kind);
                far.retain(|e| e.elsewhere() && e.relay_key != st.id.noise_public);
                far.truncate(200);
                list.extend(far);
            }
            Response::Servers { list }
        }
        Request::Host {
            kind,
            name,
            map,
            map_sig,
            max_players,
        } => {
            let rid = open_room(st, id, pseudo, kind, name, map, map_sig, max_players);
            Response::Hosted { room: rid }
        }
        Request::Search {
            map_sig,
            map,
            players,
        } => {
            // En lobby : l'hôte du lobby lance pour tout le monde.
            let party = party_of(st, id);
            if party.len() > 1 && party[0] != id {
                return Some(err(
                    ErrorCode::Refused,
                    "C'est l'hôte du lobby qui lance la partie.",
                ));
            }
            let group: Vec<u64> = if party.len() > 1 { party } else { vec![id] };
            let players = (players.clamp(2, proto::FPS_MAX_PLAYERS as u8) & !1) as u16;
            // Le lobby joue dans la même équipe : il doit y tenir.
            if group.len() > players as usize / 2 {
                return Some(err(
                    ErrorCode::Refused,
                    format!(
                        "Lobby de {} joueurs : cette carte n'en prend que {} par équipe.",
                        group.len(),
                        players / 2
                    ),
                ));
            }
            // Un membre en pleine partie FPS n'en est pas arraché (il serait pénalisé).
            let busy = group.iter().filter(|&&m| m != id).find_map(|m| {
                let c = st.conns.get(m)?;
                let r = st.rooms.get(&c.room?)?;
                (r.kind == 0 && r.started && !r.applied)
                    .then(|| c.pseudo.clone().unwrap_or_default())
            });
            if let Some(p) = busy {
                return Some(err(
                    ErrorCode::Refused,
                    format!("« {p} » est en pleine partie : attends qu'il ait fini."),
                ));
            }
            // Une seule partie FPS à la fois par ordinateur (deux comptes sur une machine).
            if let Some(p) = group.iter().find_map(|&m| {
                let mp = st.conns.get(&m)?.pseudo.clone()?;
                fps_elsewhere_on_machine(st, m, &mp)
            }) {
                return Some(err(
                    ErrorCode::Refused,
                    format!("Une partie FPS est déjà en cours sur cet ordinateur (« {p} »)."),
                ));
            }
            for &m in &group {
                leave_room(st, m);
            }
            let names: Vec<String> = group
                .iter()
                .filter_map(|m| st.conns.get(m).and_then(|c| c.pseudo.clone()))
                .collect();
            // Une partie FPS en cours qu'on a quittée : on y retourne d'abord (comme une
            // reconnexion), quelle que soit la carte demandée.
            let low = pseudo.to_ascii_lowercase();
            let unfinished = st
                .rooms
                .iter()
                .find(|(_, r)| {
                    r.kind == 0
                        && r.started
                        && !r.applied
                        && r.away.get(&low).is_some_and(|a| a.0.is_some())
                })
                .map(|(&rid, _)| rid);
            if let (Some(rid), 1) = (unfinished, group.len()) {
                return Some(join_room(st, id, pseudo, rid));
            }
            let version = st.conns.get(&id).map_or(0, |c| c.version);
            let resp = match find_room(st, &names, map_sig, version) {
                Some(room) => join_room(st, id, pseudo, room),
                // Personne en attente : un garant indépendant ouvre la partie s'il y en a un.
                None if pick_garant(st, &names).is_some() => {
                    let g = pick_garant(st, &names).expect("garant");
                    let gp = st.conns[&g].pseudo.clone().unwrap_or_default();
                    let rid = open_room(
                        st,
                        g,
                        &gp,
                        0,
                        "Partie servie par un garant".to_string(),
                        map.clone(),
                        map_sig,
                        players,
                    );
                    if let Some(r) = st.rooms.get_mut(&rid) {
                        r.garant_hosted = true;
                    }
                    if let Some(c) = st.conns.get(&g) {
                        transport::push(
                            &c.tx,
                            &Response::GarantTask {
                                room: rid,
                                map_sig,
                                map: map.clone(),
                                members: Vec::new(),
                                standby: false,
                            },
                        );
                    }
                    join_room(st, id, pseudo, rid)
                }
                None => {
                    let rid = open_room(
                        st,
                        id,
                        pseudo,
                        0,
                        String::new(),
                        map.clone(),
                        map_sig,
                        players,
                    );
                    Response::Hosted { room: rid }
                }
            };
            // Les autres joueurs du lobby suivent : leur place est gardée.
            let room = st.conns.get(&id).and_then(|c| c.room);
            if let (Some(rid), true) = (room, group.len() > 1) {
                let others: Vec<u64> = group.iter().copied().filter(|&m| m != id).collect();
                if let Some(r) = st.rooms.get_mut(&rid) {
                    for m in &others {
                        if let Some(p) = st.conns.get(m).and_then(|c| c.pseudo.clone()) {
                            r.reserved.push((p.to_ascii_lowercase(), Instant::now()));
                        }
                    }
                }
                for m in others {
                    if let Some(c) = st.conns.get(&m) {
                        transport::push(
                            &c.tx,
                            &Response::PartyGo {
                                room: rid,
                                map: map.clone(),
                                map_sig,
                            },
                        );
                    }
                }
            }
            resp
        }
        Request::Party {
            action,
            pseudo: who,
        } => handle_party(st, id, pseudo, action, &who),
        Request::SkinPut { png } => {
            let path = skin_path(&st.cfg.dir, pseudo);
            if png.is_empty() {
                let _ = std::fs::remove_file(&path);
            } else if !valid_skin(&png) {
                return Some(err(
                    ErrorCode::Refused,
                    "Skin refusé : PNG de 64 × 64 (ou 64 × 32), 24 Ko au plus.",
                ));
            } else {
                if let Some(d) = path.parent() {
                    let _ = std::fs::create_dir_all(d);
                }
                if let Err(e) = accounts::write_atomic(&path, &png) {
                    return Some(err(ErrorCode::Internal, format!("{e:#}")));
                }
            }
            // Construction : les joueurs de la partie le voient changer (pas en FPS, où le
            // skin reste celui du début de la partie).
            if let Some(r) = my_room
                .and_then(|r| st.rooms.get(&r))
                .filter(|r| r.kind == 1)
            {
                let to: Vec<u64> = r
                    .members
                    .iter()
                    .copied()
                    .chain([r.host])
                    .filter(|&m| m != id)
                    .collect();
                for m in to {
                    if let Some(c) = st.conns.get(&m) {
                        transport::push(
                            &c.tx,
                            &Response::SkinChanged {
                                pseudo: pseudo.to_string(),
                            },
                        );
                    }
                }
            }
            Response::Done
        }
        Request::SkinGet { pseudo: who } => {
            let png = std::fs::read(skin_path(&st.cfg.dir, &who))
                .ok()
                .filter(|p| valid_skin(p))
                .unwrap_or_default();
            Response::Skin { pseudo: who, png }
        }
        Request::Report { pseudo: target } => {
            let Some(room) = my_room.and_then(|r| st.rooms.get(&r)) else {
                return Some(err(
                    ErrorCode::Refused,
                    "On ne peut signaler qu'un joueur de sa partie, pendant la partie.",
                ));
            };
            let in_room = room.everyone().any(|m| {
                st.conns
                    .get(&m)
                    .and_then(|c| c.pseudo.as_deref())
                    .is_some_and(|p| p.eq_ignore_ascii_case(&target))
            });
            if !in_room || target.eq_ignore_ascii_case(pseudo) {
                return Some(err(
                    ErrorCode::Refused,
                    format!("« {target} » n'est pas dans ta partie."),
                ));
            }
            let key = (pseudo.to_ascii_lowercase(), target.to_ascii_lowercase());
            let fresh = st
                .rooms
                .get_mut(&my_room.unwrap())
                .is_some_and(|r| r.reports.insert(key.clone()));
            if fresh {
                let me = st.stats.get_mut(pseudo);
                if !me.reported.contains(&key.1) {
                    me.reported.push(key.1.clone());
                    if me.reported.len() > 1000 {
                        me.reported.remove(0);
                    }
                }
                // La confiance n'est touchée qu'à la fin de la partie, si un autre joueur
                // l'a signalé aussi (voir `settle_absences`).
                let them = st.stats.get_mut(&target);
                if !them.reported_by.contains(&key.0) {
                    them.reported_by.push(key.0.clone());
                    if them.reported_by.len() > 1000 {
                        them.reported_by.remove(0);
                    }
                }
                st.stats.save(pseudo);
                st.stats.save(&target);
                log::info!("« {pseudo} » signale « {target} »");
            }
            Response::Done
        }
        Request::Ban {
            pseudo: target,
            reason,
        } => {
            if !is_admin(&st.cfg.dir, pseudo) {
                return Some(err(
                    ErrorCode::Refused,
                    "Réservé aux administrateurs du relais (admins.txt), pour la triche.",
                ));
            }
            let machines = match ban_cheater(st, &target, &reason) {
                Ok(m) => m,
                Err(e) => return Some(err(ErrorCode::Refused, e)),
            };
            log::info!(
                "« {target} » banni par « {pseudo} » ({machines} machine(s) pour 3 mois) : {reason}"
            );
            Response::Done
        }
        Request::ServerAdmin {
            action,
            pseudo: target,
            reason,
        } => {
            let Some(rid) = my_room.filter(|r| st.rooms.get(r).is_some_and(|r| r.kind == 1)) else {
                return Some(err(
                    ErrorCode::Refused,
                    "Seulement sur un serveur de construction.",
                ));
            };
            if guards::frozen(st, rid) {
                return Some(err(
                    ErrorCode::Refused,
                    "Monde en pause (son relais ne joint plus ses gardiens) : réessaie dans un instant.",
                ));
            }
            let (server, creator) = {
                let r = &st.rooms[&rid];
                (r.server.clone(), r.creator.clone())
            };
            if action == proto::SERVER_GAMEMODE_QUERY {
                // Arrivée : on reprend sa position sur ce serveur.
                if let (Some(p), Some(c)) =
                    (st.servers.position(&server, pseudo), st.conns.get(&id))
                {
                    transport::push(
                        &c.tx,
                        &Response::BuildSpawn {
                            pos: [p[0], p[1], p[2]],
                            yaw: p[3],
                            pitch: p[4],
                        },
                    );
                }
                // Heure du serveur.
                if let Some(c) = st.conns.get(&id) {
                    let (hours, fixed) = st.servers.time(&server);
                    transport::push(&c.tx, &Response::SetTime { hours, fixed });
                }
                if !st.servers.is_op(&server, &creator, pseudo) {
                    if let (Some(mode), Some(c)) =
                        (st.servers.default_gamemode(&server), st.conns.get(&id))
                    {
                        transport::push(
                            &c.tx,
                            &Response::SetGamemode {
                                mode,
                                by: String::new(),
                            },
                        );
                    }
                }
                return Some(Response::ServerAdmin {
                    text: String::new(),
                });
            }
            let act = match action {
                proto::SERVER_BAN => servers::Action::Ban,
                proto::SERVER_UNBAN => servers::Action::Unban,
                proto::SERVER_OP => servers::Action::Op,
                proto::SERVER_DEOP => servers::Action::Deop,
                proto::SERVER_KILL => servers::Action::Kill,
                proto::SERVER_WL_ON => servers::Action::WhitelistOn,
                proto::SERVER_WL_OFF => servers::Action::WhitelistOff,
                proto::SERVER_WL_ADD => servers::Action::WhitelistAdd,
                proto::SERVER_WL_REMOVE => servers::Action::WhitelistRemove,
                proto::SERVER_WL_LIST => servers::Action::WhitelistList,
                proto::SERVER_GAMEMODE => servers::Action::Gamemode,
                proto::SERVER_DEFAULT_GAMEMODE => servers::Action::DefaultGamemode,
                proto::SERVER_RENAME => servers::Action::Rename,
                proto::SERVER_TIMESET => servers::Action::TimeSet,
                proto::SERVER_TIMEFIX => servers::Action::TimeFix,
                _ => servers::Action::List,
            };
            // Nouveau nom : vide ou trop long refusé avant de vérifier qu'on est OP.
            let new_name: String = reason.trim().chars().take(32).collect();
            if act == servers::Action::Rename && new_name.is_empty() {
                return Some(err(ErrorCode::Refused, "Nom vide."));
            }
            match st
                .servers
                .act(&server, &creator, pseudo, act, &target, &reason)
            {
                Ok(text) => {
                    // Provinces (v139) : leurs relais appliquent les mêmes réglages.
                    if matches!(
                        act,
                        servers::Action::Ban
                            | servers::Action::Unban
                            | servers::Action::Op
                            | servers::Action::Deop
                            | servers::Action::DefaultGamemode
                    ) {
                        provinces::push_settings(st, &server);
                    }
                    // Heure changée : à tous les joueurs du serveur (v146 : et par les
                    // provinces, à ceux qui ne sont plus connectés ici).
                    if matches!(act, servers::Action::TimeSet | servers::Action::TimeFix) {
                        let (hours, fixed) = st.servers.time(&server);
                        for c in st.conns.values().filter(|c| c.room == Some(rid)) {
                            transport::push(&c.tx, &Response::SetTime { hours, fixed });
                        }
                        provinces::push_time(st, &server, (hours, fixed));
                    }
                    // Nom affiché dans la liste des serveurs.
                    if act == servers::Action::Rename {
                        if let Some(r) = st.rooms.get_mut(&rid) {
                            r.name = new_name.clone();
                        }
                    }
                    // Mode de jeu : envoyé au joueur visé (soi-même sans pseudo).
                    if act == servers::Action::Gamemode {
                        let who = if target.is_empty() {
                            pseudo
                        } else {
                            target.as_str()
                        };
                        // Le relais s'en souvient : c'est ce mode qui décide si son
                        // inventaire est tenu ici.
                        st.bags.set_mode(&server, who, &reason);
                        provinces::push_settings(st, &server);
                        let to = st.conns.values().find(|c| {
                            c.room == Some(rid)
                                && c.pseudo
                                    .as_deref()
                                    .is_some_and(|p| p.eq_ignore_ascii_case(who))
                        });
                        match to {
                            Some(c) => {
                                transport::push(
                                    &c.tx,
                                    &Response::SetGamemode {
                                        mode: reason.chars().take(16).collect(),
                                        by: pseudo.to_string(),
                                    },
                                );
                            }
                            // (v146) Au fond d'un pays, il n'est plus connecté ici : sa
                            // province le lui dit (les modes partent à toutes).
                            None if provinces::has_provinces(st, rid) => {}
                            None => {
                                return Some(err(
                                    ErrorCode::Refused,
                                    format!("{who} n'est pas sur ce serveur."),
                                ))
                            }
                        }
                    }
                    if act == servers::Action::Kill {
                        let target_conn = st.conns.iter().find(|(_, c)| {
                            c.room == Some(rid)
                                && c.pseudo
                                    .as_deref()
                                    .is_some_and(|p| p.eq_ignore_ascii_case(&target))
                        });
                        match target_conn {
                            Some((_, c)) => {
                                transport::push(
                                    &c.tx,
                                    &Response::Slain {
                                        by: pseudo.to_string(),
                                    },
                                );
                            }
                            // (v146) Au fond d'un pays : par les provinces.
                            None if provinces::slay_down(st, rid, &target, pseudo) => {}
                            None => {
                                return Some(err(
                                    ErrorCode::Refused,
                                    format!("{target} n'est pas sur ce serveur."),
                                ))
                            }
                        }
                    }
                    // Banni : exclu tout de suite s'il est sur le serveur.
                    if act == servers::Action::Ban {
                        let ids: Vec<u64> = st
                            .conns
                            .iter()
                            .filter(|(_, c)| {
                                c.room == Some(rid)
                                    && c.pseudo
                                        .as_deref()
                                        .is_some_and(|p| p.eq_ignore_ascii_case(&target))
                            })
                            .map(|(&i, _)| i)
                            .collect();
                        for i in ids {
                            leave_room(st, i);
                            if let Some(c) = st.conns.get(&i) {
                                let why = if reason.is_empty() {
                                    format!("Banni de ce serveur par {pseudo}.")
                                } else {
                                    format!("Banni de ce serveur par {pseudo} : {reason}")
                                };
                                transport::push(&c.tx, &Response::Kicked { reason: why });
                            }
                        }
                        log::info!("« {target} » banni du serveur {server} par « {pseudo} »");
                    }
                    Response::ServerAdmin { text }
                }
                Err(e) => err(ErrorCode::Refused, e),
            }
        }
        Request::Unreport { pseudo: target } => {
            let low = target.to_ascii_lowercase();
            let me = st.stats.get_mut(pseudo);
            let had = me.reported.contains(&low);
            if !had {
                return Some(err(
                    ErrorCode::Refused,
                    format!("Tu n'as pas signalé « {target} »."),
                ));
            }
            me.reported.retain(|p| *p != low);
            let mine = pseudo.to_ascii_lowercase();
            // Retiré pendant la partie : il ne comptera pas à la fin.
            if let Some(r) = my_room.and_then(|r| st.rooms.get_mut(&r)) {
                r.reports.remove(&(mine.clone(), low.clone()));
            }
            let them = st.stats.get_mut(&target);
            them.reported_by.retain(|p| *p != mine);
            st.stats.save(pseudo);
            st.stats.save(&target);
            log::info!("« {pseudo} » annule son signalement de « {target} »");
            Response::Done
        }
        // Garant vérificateur : en rejouant la partie, il a trouvé un script (bunny hop
        // parfait, macro). Preuve faite sur les entrées mêmes du joueur : confiance à zéro ;
        // une deuxième preuve trouvée par un autre garant : bannissement.
        Request::CheatFound {
            pseudo: target,
            kind,
            detail,
        } => {
            let rid = my_room?;
            let room = st.rooms.get(&rid)?;
            let in_roster = room.roster.iter().any(|p| p.eq_ignore_ascii_case(&target));
            if !room.hidden.contains(&id) || room.host == id || !in_roster {
                return None;
            }
            let by = pseudo.to_ascii_lowercase();
            let s = st.stats.get_mut(&target);
            if s.script_by.contains(&by) && s.scripts > 0 {
                // Le même garant ne compte qu'une fois.
                return None;
            }
            s.scripts += 1;
            s.script_by.push(by);
            s.trust_by(stats::trust::SCRIPT);
            let distinct = s.script_by.len();
            st.stats.save(&target);
            log::warn!(
                "« {target} » : triche prouvée par le garant « {pseudo} » ({kind} : {detail})"
            );
            if distinct >= 2 {
                let _ = ban_cheater(st, &target, &format!("script ({detail})"));
            }
            return None;
        }
        Request::MatchResult { data } => {
            let Some(rid) = my_room else {
                return Some(Response::Done);
            };
            let Some(summary) = game::decode::<game::MatchSummary>(&data) else {
                return Some(err(ErrorCode::Internal, "résultat illisible"));
            };
            let Some(room) = st.rooms.get_mut(&rid) else {
                return Some(Response::Done);
            };
            // Le garant vérificateur a rejoué toute la partie : son résultat fait foi.
            if room.hidden.contains(&id) && room.host != id {
                if room.started && summary.map_sig == room.map_sig {
                    room.audited = Some(game::signature(&data));
                    try_apply_result(st, rid);
                }
                return Some(Response::Done);
            }
            // Seulement les joueurs du début, chacun une fois, sur la bonne carte.
            let names: HashSet<String> = summary
                .players
                .iter()
                .map(|p| p.pseudo.to_ascii_lowercase())
                .collect();
            let roster: HashSet<String> =
                room.roster.iter().map(|p| p.to_ascii_lowercase()).collect();
            let ok = room.started
                && summary.map_sig == room.map_sig
                && names.len() == summary.players.len()
                && names.is_subset(&roster)
                && roster.contains(&pseudo.to_ascii_lowercase());
            if ok {
                let h = game::signature(&data);
                room.results
                    .entry(h)
                    .or_insert_with(|| (summary, HashSet::new()))
                    .1
                    .insert(id);
                try_apply_result(st, rid);
            }
            Response::Done
        }
        Request::Leaderboard { friends } => {
            let (list, me) = if friends {
                st.stats.friends_top(pseudo)
            } else {
                st.stats.top(pseudo)
            };
            Response::Leaderboard { list, me }
        }
        Request::Friend { pseudo: who, add } => {
            if !who.is_empty() {
                let Some(found) = st.accounts.load(&who) else {
                    return Some(err(
                        ErrorCode::Refused,
                        format!("Joueur « {who} » inconnu."),
                    ));
                };
                let name = found.pseudo.clone();
                let me = st.stats.get_mut(pseudo);
                me.friends.retain(|f| !f.eq_ignore_ascii_case(&name));
                if add && !name.eq_ignore_ascii_case(pseudo) && me.friends.len() < 200 {
                    me.friends.push(name);
                }
                st.stats.save(pseudo);
            }
            let list = st
                .stats
                .get(pseudo)
                .map(|s| s.friends.clone())
                .unwrap_or_default();
            let online = list.iter().map(|f| conn_of(st, f).is_some()).collect();
            Response::Friends { list, online }
        }
        Request::Maps => {
            let mut list = st.maps.entries();
            for r in st.rooms.values().filter(|r| r.kind == 0) {
                let n = r.player_count() as u32;
                match list.iter_mut().find(|e| e.sig == r.map_sig) {
                    Some(e) => {
                        if !r.started {
                            e.waiting += n;
                        }
                    }
                    None => list.push(proto::MapEntry {
                        sig: r.map_sig,
                        name: r.map.clone(),
                        // Personne n'est nommé avant le début de la partie.
                        author: String::new(),
                        size: 0,
                        plays: 0,
                        waiting: if r.started { 0 } else { n },
                        stored: false,
                        players: r.max_players as u8,
                    }),
                }
            }
            list.sort_by(|a, b| {
                b.waiting
                    .cmp(&a.waiting)
                    .then(b.plays.cmp(&a.plays))
                    .then(a.name.cmp(&b.name))
            });
            list.truncate(200);
            Response::Maps { list }
        }
        Request::MapPut {
            sig,
            name,
            offset,
            total,
            data,
            fps,
        } => match st.maps.put(id, pseudo, sig, &name, offset, total, &data) {
            maps::Put::Partial(o) => Response::MapStored {
                offset: o,
                complete: false,
            },
            maps::Put::Complete => {
                if fps {
                    st.maps.mark_fps(sig);
                }
                Response::MapStored {
                    offset: total,
                    complete: true,
                }
            }
            maps::Put::Refused(m) => err(ErrorCode::Refused, m),
        },
        Request::MapGet { sig, offset } => match st.maps.get(&sig, offset) {
            Some((total, data)) => Response::MapPart {
                sig,
                offset,
                total,
                data,
            },
            None => err(ErrorCode::Refused, "Carte introuvable sur le relais."),
        },
        Request::Caps { caps } => {
            if let Some(c) = st.conns.get_mut(&id) {
                c.caps = caps;
            }
            Response::Done
        }
        Request::Position { chunk } => {
            if let Some(rid) = my_room.filter(|r| st.rooms.get(r).is_some_and(|r| r.kind == 1)) {
                st.positions_seen += 1;
                send_catch_up(st, rid, id, chunk, true);
                refresh_needs(st, rid, id);
                // Les relais des régions maintenant en vue, s'ils ont changé pour lui, et les
                // provinces servies par un relais de province (v138).
                send_shards(st, rid, id);
                provinces::send_list(st, rid, id);
            }
            return None;
        }
        Request::Served { regions } => {
            let rid = my_room.filter(|r| st.rooms.get(r).is_some_and(|r| r.kind == 1))?;
            let served = !regions.is_empty();
            let player = st.conns.get_mut(&id).is_some_and(|c| {
                c.served = regions.into_iter().take(16).collect();
                !c.garant_only
            });
            if let Some(room) = st.rooms.get_mut(&rid) {
                if served {
                    room.unserved.remove(&id);
                } else if player {
                    room.unserved.insert(id);
                }
            }
            refresh_needs(st, rid, id);
            // Une région qu'un relais ne lui sert plus : ce qui lui manque, tout de suite.
            let at = st.rooms.get(&rid)?.build.positions.get(&id).copied()?;
            send_catch_up(st, rid, id, at, false);
            return None;
        }
        Request::ToZone { chunk, data } => {
            let rid = my_room.filter(|r| st.rooms.get(r).is_some_and(|r| r.kind == 1))?;
            // Relais de province (v138) : les blocs de sa province seulement.
            if st.rooms.get(&rid).is_some_and(|r| r.province.is_some()) {
                provinces::to_zone(st, rid, id, pseudo, chunk, game::decode(&data));
                return None;
            }
            // Monde en pause (son relais n'entend plus assez de gardiens) : rien n'y change.
            // Une demande d'objet est refusée ; une modification de bloc reste sans
            // réponse (le jeu la garde et la renvoie à la reprise).
            if guards::frozen(st, rid) {
                if let Some(seq) = game::decode::<game::BuildMsg>(&data)
                    .as_ref()
                    .filter(|m| minelib_net::items::is_request(m))
                    .and_then(minelib_net::items::request_seq)
                {
                    publish_items(st, rid, id, vec![game::BuildMsg::Denied { seq }]);
                }
                return None;
            }
            let room = st.rooms.get_mut(&rid).expect("partie");
            let msg = game::decode::<game::BuildMsg>(&data);
            // Objets au sol et coffres : ce relais arbitre (une demande après l'autre),
            // publie le résultat aux joueurs proches, ou refuse au seul demandeur.
            if let Some(m) = msg.as_ref().filter(|m| minelib_net::items::is_request(m)) {
                // (Province servie par un relais de province, v139 : c'est lui qui arbitre ;
                // le jeu y envoie la demande.)
                // Refusée ici (le jeu la renvoie à qui en décide quand il l'apprend) ; de
                // même pour un joueur dont le sac de survie est prêté à une province : il
                // lui est redemandé.
                let target = room.build.items.target_chunk(m);
                if target.is_some_and(|c| provinces::delegated(st, rid, proto::region_of(c)))
                    || provinces::bag_away(st, rid, pseudo)
                {
                    let seq = minelib_net::items::request_seq(m).unwrap_or(0);
                    publish_items(st, rid, id, vec![game::BuildMsg::Denied { seq }]);
                    return None;
                }
                item_request(st, rid, id, pseudo, m);
                return None;
            }
            // Le propriétaire remet les objets de sa sauvegarde (à l'ouverture du monde).
            if matches!(
                msg,
                Some(game::BuildMsg::Item { .. } | game::BuildMsg::Slot { .. })
            ) {
                if room.host == id {
                    let out = msg.and_then(|m| room.build.items.seed(&m));
                    publish_items(st, rid, 0, out.into_iter().collect());
                }
                return None;
            }
            // Blocs : ce relais décide seul et publie (voir `publish_block`). Tout autre
            // message (un « Set » ou un refus fabriqué par un jeu modifié) est sans effet.
            // (Province servie par un relais de province, v138 : c'est lui qui décide ; le
            // jeu y renverra la modification quand il l'apprendra.)
            if let Some(game::BuildMsg::Edit { pos, block, fluid }) = msg {
                if !provinces::delegated(st, rid, proto::region_of(chunk)) {
                    build_edit(st, rid, id, pseudo, chunk, pos, block, fluid);
                }
            }
            return None;
        }
        // Provinces (v138), de l'hôte d'un relais de province (connexion de garant).
        Request::ProvinceReady {
            room,
            province,
            epoch,
            node,
            addr,
            key,
            have,
        } => {
            if my_room == Some(room) {
                provinces::ready(st, room, id, province, epoch, node, addr, key, have);
            }
            return None;
        }
        Request::ProvinceStandbyReady {
            room,
            province,
            epoch,
            have: _,
        } => {
            if my_room == Some(room) {
                provinces::standby_ready(st, room, id, province, epoch);
            }
            return None;
        }
        Request::ProvinceLease {
            room,
            province,
            epoch,
            t,
            counts,
            inside,
            followed,
            wants,
            children,
            arrived,
            left,
            seq,
            total,
        } => {
            if my_room == Some(room) {
                provinces::lease(
                    st, room, id, province, epoch, t, counts, inside, followed, wants, children,
                    arrived, left, seq, total,
                );
            }
            return None;
        }
        // (v142) Relais de secours d'une province (chez son relais) : envoi copié.
        Request::ProvinceCopied {
            room,
            province: _,
            epoch,
            n,
        } => {
            if my_room == Some(room) {
                provinces::copied(st, id, room, epoch, n);
            }
            return None;
        }
        Request::ProvinceArchive {
            room,
            province,
            epoch,
            n,
            entries,
            bags,
            spots,
            loose,
            dump,
        } => {
            if my_room == Some(room) {
                provinces::archive(
                    st, room, id, province, epoch, n, entries, bags, spots, loose, dump,
                );
            }
            return None;
        }
        Request::ProvinceBag {
            room,
            province,
            epoch,
            pseudo: who,
        } => {
            if my_room == Some(room) {
                provinces::bag_asked(st, room, id, province, epoch, &who);
            }
            return None;
        }
        Request::ProvinceBagBack {
            room,
            province,
            epoch,
            pseudo: who,
            items,
            mined,
        } => {
            if my_room == Some(room) {
                provinces::bag_returned(st, room, id, province, epoch, &who, items, mined);
            }
            return None;
        }
        Request::ProvinceShards {
            room,
            province,
            epoch,
            ready,
        } => {
            if my_room == Some(room) {
                provinces::shards_ready(st, room, id, province, epoch, ready);
            }
            return None;
        }
        Request::ProvinceRelease {
            room,
            province,
            epoch,
        } => {
            if my_room == Some(room) {
                provinces::released(st, room, id, province, epoch);
            }
            return None;
        }
        // Gardien d'un monde (relais de secours, témoin) : son bail, ou son départ.
        Request::Lease { room, version, t } => {
            guards::lease(st, room, id, pseudo, version, t);
            return None;
        }
        Request::Resign { room } => {
            guards::resign(st, room, id, pseudo);
            return None;
        }
        // Hôte du relais de secours d'un monde : son relais est prêt ; les joueurs savent
        // désormais où le trouver.
        Request::StandbyReady { node, addr, key } => {
            let rid = my_room?;
            let room = st.rooms.get_mut(&rid)?;
            let sb = room.standby.as_mut().filter(|s| s.host == id)?;
            sb.at = Some(proto::StandbyAt {
                node,
                addr,
                key,
                room: rid,
                pseudo: pseudo.to_string(),
                root: st.cert_key.verifying_key().to_bytes(),
            });
            log::info!("monde {rid} : relais de secours prêt");
            announce_standby(st, rid);
            return None;
        }
        Request::ShardReady {
            region,
            node,
            addr,
            key,
        } => {
            let rid = my_room?;
            let signer = account_key(st, id).unwrap_or([0; 32]);
            let room = st.rooms.get_mut(&rid)?;
            let sh = room
                .shards
                .get_mut(&region)?
                .iter_mut()
                .find(|s| s.host == id)?;
            sh.info = Some(proto::ShardInfo {
                region,
                slot: sh.slot,
                node,
                addr,
                key,
                signer,
                epoch: 0,
            });
            let (previous, slot) = (sh.previous.take(), sh.slot);
            room.shards_dirty.insert(region);
            log::info!("monde {rid} : relais {slot} de la région {region:?} prêt");
            announce_shard_peers(st, rid, region);
            // L'hôte précédent ferme : les joueurs sont déjà passés au nouveau.
            if let Some(old) = previous {
                leave_room(st, old);
                if let Some(c) = st.conns.get(&old) {
                    transport::push(&c.tx, &Response::RoomClosed);
                }
            }
            // Deux relais prêts : la région devient souveraine tout de suite.
            upkeep_sovereignty(st, rid);
            return None;
        }
        // Région souveraine, de l'hôte principal : un lot qu'il vient de publier, à
        // archiver quand un autre hôte l'aura confirmé.
        Request::Archive { region, batches } => {
            let rid = my_room?;
            let mut audit = Vec::new();
            let audit_every = st.audit_every.max(1);
            {
                let sv = st.rooms.get_mut(&rid)?.sov.get_mut(&region)?;
                if sv.primary != id {
                    return None;
                }
                for batch in batches.into_iter().take(ARCHIVE_MAX) {
                    let (end, chain) = sv
                        .pending
                        .back()
                        .map_or((sv.base, sv.chain), |p| (p.end, p.chain));
                    // Rien de signé n'est vérifié ici (v136) : l'autre hôte de la région
                    // vérifie chaque lot avant de le confirmer, et l'hôte d'une autre région
                    // un lot sur huit tiré au hasard (`audit_batch`). Seulement qu'il est de
                    // cette époque et suit le précédent.
                    let fits = batch.room == rid
                        && batch.region == region
                        && batch.epoch == sv.epoch
                        && batch.first == end
                        && batch.prev == chain
                        && batch.numbered();
                    if !fits || sv.pending.len() >= 4096 {
                        break;
                    }
                    if rand::random::<u32>().is_multiple_of(audit_every) {
                        audit.push(batch.clone());
                    }
                    sv.pending.push_back(PendingBatch {
                        end: batch.end(),
                        chain: batch.chain(),
                        batch,
                    });
                }
                sv.waiting.get_or_insert_with(Instant::now);
            }
            for b in audit {
                audit_batch(st, rid, region, b);
            }
            // (Les confirmations des autres hôtes ont pu arriver avant les lots.)
            try_commit(st, rid, region);
            return None;
        }
        // Vérification d'un lot confiée à cet hôte : rien à signaler, ou une faute, que ce
        // relais revérifie sur sa propre copie (il l'a reçue de l'hôte principal lui-même).
        Request::Audited { id: aid, fault } => {
            let rid = my_room?;
            audited(st, rid, id, aid, fault);
            return None;
        }
        // D'un autre hôte de la région : il a vérifié jusqu'au rang `seq`. Les lots
        // reçus jusque-là, s'ils ont la même empreinte, sont archivés.
        Request::Ack {
            region,
            epoch,
            seq,
            chain,
        } => {
            let rid = my_room?;
            let room = st.rooms.get_mut(&rid)?;
            let ready: Vec<u64> = room
                .shards
                .get(&region)
                .into_iter()
                .flatten()
                .filter(|s| s.info.is_some())
                .map(|s| s.host)
                .collect();
            let sv = room.sov.get_mut(&region)?;
            if !ready.contains(&id) || sv.primary == id || epoch != sv.epoch {
                return None;
            }
            sv.acks.insert(id, (seq, chain));
            try_commit(st, rid, region);
            return None;
        }
        // D'un autre hôte de la région : une faute de l'hôte principal. Ce relais refait la
        // vérification avec les seuls lots joints (signés par l'accusé) ; prouvée, l'hôte
        // est écarté de tout rôle de garant et un autre prend sa place.
        Request::Accuse {
            region,
            epoch,
            why,
            evidence,
        } => {
            let rid = my_room?;
            let room = st.rooms.get(&rid)?;
            let is_host = room
                .shards
                .get(&region)
                .is_some_and(|l| l.iter().any(|s| s.host == id));
            let (accused, current, key, floor_t) = match room.sov.get(&region) {
                Some(s) if s.epoch == epoch => (s.primary, true, s.key, s.floor_t),
                _ => {
                    let p = room
                        .sov_past
                        .get(&region)?
                        .iter()
                        .find(|p| p.epoch == epoch)?;
                    (p.host, false, p.key, p.floor_t)
                }
            };
            if !is_host {
                return None;
            }
            let rules = sovereign::Rules {
                room: rid,
                region,
                root: root_of(st, rid),
                floor_t,
            };
            match sovereign::judge(&rules, epoch, &key, &evidence) {
                Some(fault) => {
                    log::warn!(
                        "monde {rid} : région {region:?}, faute prouvée de l'hôte principal \
                         (époque {epoch}) : {fault}"
                    );
                    // (D'une époque finie aussi : il est écarté, où qu'il soit.)
                    ban_host(st, accused);
                    if current {
                        upkeep_sovereignty(st, rid);
                    }
                }
                None => {
                    log::info!("monde {rid} : région {region:?}, accusation non prouvée ({why})")
                }
            }
            return None;
        }
        // D'un hôte de la région en fin d'époque : les lots qu'il avait vérifiés (signés
        // par l'hôte principal de cette époque), archivés s'ils suivent ce qui l'est déjà.
        Request::Handover {
            region,
            epoch,
            batches,
        } => {
            let rid = my_room?;
            // Monde en pause : rien n'est archivé maintenant, mais rien n'est perdu (avant
            // la v136, la passation était oubliée) : gardée pour la reprise.
            if guards::frozen(st, rid) {
                let room = st.rooms.get_mut(&rid)?;
                if room.handovers.len() < 64 {
                    room.handovers.push((id, region, epoch, batches));
                }
                return None;
            }
            take_handover(st, rid, id, region, epoch, batches);
            return None;
        }
        // Position d'un joueur servi par un relais de région (ses positions ne passent plus
        // par ici), gardée pour son retour.
        Request::Spot { pos, yaw, pitch } => {
            let rid = my_room?;
            let server = st
                .rooms
                .get(&rid)
                .filter(|r| r.kind == 1)
                .map(|r| r.server.clone())?;
            let at = [pos[0], pos[1], pos[2], yaw, pitch];
            st.servers.set_position(&server, pseudo, at);
            // (Relais de province, v139 : transmise au relais du monde.)
            provinces::spot(st, rid, pseudo, at);
            return None;
        }
        Request::Nearby { chunk, data } => {
            // Position sur ce serveur, gardée pour son retour.
            if let Some(game::BuildMsg::Player {
                pos, yaw, pitch, ..
            }) = game::decode::<game::BuildMsg>(&data)
            {
                if let Some((rid, server)) = my_room.and_then(|rid| {
                    st.rooms
                        .get(&rid)
                        .filter(|r| r.kind == 1)
                        .map(|r| (rid, r.server.clone()))
                }) {
                    let at = [pos[0], pos[1], pos[2], yaw, pitch];
                    st.servers.set_position(&server, pseudo, at);
                    // (Relais de province, v139 : transmise au relais du monde.)
                    provinces::spot(st, rid, pseudo, at);
                }
            }
            // Le pseudo montré est celui de la session (pas celui que le joueur annonce) ;
            // une position absurde (infinie, hors du monde) n'est pas diffusée.
            let (_, pose) = game::pose_of(&data)?;
            if let Some(room) = my_room.and_then(|r| st.rooms.get_mut(&r)) {
                // Plus de positions que le jeu n'en envoie : pas diffusée.
                if !room.build.pos_due(id) {
                    return None;
                }
                // Vers chaque voisin, elle part regroupée avec celles des autres (voir
                // `transport::push_pose`) ; périssable : chez un joueur dont la connexion
                // ne suit pas, la position suivante la remplace.
                let pose = Arc::new(transport::PoseOf {
                    id,
                    name: pseudo.to_string(),
                    pose,
                });
                for m in room.build.nearby(id, chunk, 1) {
                    if let Some(c) = st.conns.get(&m) {
                        transport::push_pose(&c.tx, &pose);
                    }
                }
                // Modifications pas encore reçues (foule : elles ne sont poussées qu'à une
                // partie des joueurs, voir `zones::LIVE_MAX`).
                if room.kind == 1 && room.build.positions.contains_key(&id) {
                    if let Some(rid) = my_room {
                        send_catch_up(st, rid, id, chunk, false);
                    }
                }
            }
            return None;
        }
        Request::RoomStarted { started } => {
            let mut first = None;
            if let Some(r) = my_room.and_then(|r| st.rooms.get_mut(&r)) {
                if r.host == id {
                    if started && !r.started && r.kind == 0 {
                        first = Some(r.everyone().collect::<Vec<u64>>());
                    }
                    r.started = started;
                    if started && r.started_at.is_none() {
                        r.started_at = Some(Instant::now());
                    }
                }
            }
            // Début d'une partie FPS : elle compte pour chacun (rapport parties /
            // signalements), et ceux-là seuls figureront au résultat.
            if let Some(ids) = first {
                let names: Vec<String> = ids
                    .iter()
                    .filter_map(|m| st.conns.get(m).and_then(|c| c.pseudo.clone()))
                    .collect();
                for n in &names {
                    st.stats.get_mut(n).games += 1;
                    st.stats.save(n);
                }
                if let Some(r) = my_room.and_then(|r| st.rooms.get_mut(&r)) {
                    r.roster = names;
                }
            }
            Response::Done
        }
        Request::RoomPaused { paused } => {
            if let Some(r) = my_room.and_then(|r| st.rooms.get_mut(&r)) {
                if r.host == id && r.kind == 0 && r.started {
                    room_paused(r, paused);
                }
            }
            Response::Done
        }
        Request::Join { room } => {
            leave_room(st, id);
            join_room(st, id, pseudo, room)
        }
        Request::Leave => {
            leave_room(st, id);
            Response::Left
        }
        // (v146) Au fond d'un pays : le joueur quitte la partie ici sans quitter le monde
        // (ses provinces le servent ; son sac de survie et sa position de retour sont chez
        // elles). Il ferme ensuite sa connexion ; il reviendra avec son certificat (`Wake`).
        Request::Doze { room } => {
            let ok = my_room == Some(room)
                && st
                    .rooms
                    .get(&room)
                    .is_some_and(|r| r.kind == 1 && r.host != id && !r.provinces.is_empty());
            if !ok {
                return Some(err(
                    ErrorCode::Refused,
                    "Pas de veille ici (seulement au fond d'un pays).",
                ));
            }
            leave_room(st, id);
            log::debug!("monde {room} : « {pseudo} » en veille (au fond d'un pays)");
            Response::Dozed { room }
        }
        Request::ToHost { data } => {
            let rid = my_room?;
            // Fréquentation : écrire dans le tchat d'une partie, c'est parler à ses joueurs.
            if data.len() < 400 {
                if let Some(game::ToHost::Chat(_)) = game::decode::<game::ToHost>(&data) {
                    let others: Vec<String> = st
                        .rooms
                        .get(&rid)
                        .filter(|r| r.kind == 0)
                        .map(|r| {
                            r.everyone()
                                .filter(|m| *m != id && !r.hidden.contains(m))
                                .filter_map(|m| st.conns.get(&m)?.pseudo.clone())
                                .collect()
                        })
                        .unwrap_or_default();
                    for q in others {
                        st.contacts.add(pseudo, &q, contacts::CHAT);
                    }
                }
            }
            let r = st.rooms.get_mut(&rid)?;
            // Tirs, grenades, achats… : le garant vérificateur en reçoit une copie
            // attestée (l'hôte ne peut ni en inventer ni en taire), et le relais la garde
            // parmi les pièces d'un arbitrage.
            let witness = r.kind == 0
                && !r.hidden.contains(&id)
                && r.hidden.iter().any(|&h| h != r.host)
                && game::decode::<game::ToHost>(&data).is_some_and(|m| {
                    matches!(
                        m,
                        game::ToHost::Fire { .. }
                            | game::ToHost::Throw { .. }
                            | game::ToHost::Buy(_)
                            | game::ToHost::Sell(_)
                            | game::ToHost::Action { .. }
                            | game::ToHost::Team(_)
                            | game::ToHost::Drop { .. }
                            | game::ToHost::Pickup { .. }
                            | game::ToHost::Color(_)
                            | game::ToHost::Door { .. }
                            | game::ToHost::CallVote { .. }
                            | game::ToHost::Ballot(_)
                    )
                });
            if witness {
                r.evidence.push_back((Some(id), data.clone()));
                if r.evidence.len() > 50_000 {
                    r.evidence.pop_front();
                }
                let copy = Response::Witness {
                    peer: id,
                    data: data.clone(),
                };
                for h in r.hidden.iter().filter(|&&h| h != r.host) {
                    if let Some(c) = st.conns.get(h) {
                        transport::push(&c.tx, &copy);
                    }
                }
            }
            if let Some(c) = st.conns.get(&r.host) {
                transport::push(&c.tx, &Response::FromPeer { peer: id, data });
            }
            return None;
        }
        Request::Voice { to, data } => {
            let room = my_room.and_then(|r| st.rooms.get(&r))?;
            let fps = room.kind == 0;
            let from = if fps && room.host == id { 0 } else { id };
            let msg = Response::Voice { from, data };
            // Jusqu'à 256 auditeurs (une foule autour de soi : le jeu envoie aux plus
            // proches d'abord).
            let mut heard = Vec::new();
            let rid = my_room?;
            for t in to.into_iter().take(VOICE_MAX) {
                let t = if fps && t == 0 { room.host } else { t };
                if t == id {
                    continue;
                }
                // Un joueur de cette partie (par sa connexion : pas de recherche dans la
                // liste des membres, longue dans un grand monde), pas un garant.
                if let Some(c) = st
                    .conns
                    .get(&t)
                    .filter(|c| c.room == Some(rid) && !c.garant_only)
                {
                    transport::push(&c.tx, &msg);
                    heard.push(t);
                }
            }
            st.voice_relayed += heard.len();
            note_talk(st, id, pseudo, heard);
            return None;
        }
        // Construction (v137) : voix adressée par pseudo, hors des régions servies par un
        // relais de région (le jeu ne connaît pas toujours le numéro ici d'un joueur qu'il
        // voit par un relais de région).
        Request::VoiceTo { to, data } => {
            let rid = my_room?;
            let room = st.rooms.get(&rid).filter(|r| r.kind == 1)?;
            if data.len() > VOICE_BYTES {
                return None;
            }
            let msg = transport::encode(&Response::VoiceFrom {
                from: pseudo.to_string(),
                data,
            })?;
            // (Une fois à chacun, même nommé deux fois.)
            let mut names: Vec<String> = to
                .into_iter()
                .take(VOICE_MAX)
                .map(|p| p.to_ascii_lowercase())
                .collect();
            names.sort_unstable();
            names.dedup();
            let mut heard = Vec::new();
            for p in names {
                let Some(&t) = room.by_pseudo.get(&p) else {
                    continue;
                };
                if t == id {
                    continue;
                }
                if let Some(c) = st.conns.get(&t) {
                    transport::push_raw(&c.tx, msg.clone());
                    heard.push(t);
                }
            }
            st.voice_relayed += heard.len();
            note_talk(st, id, pseudo, heard);
            return None;
        }
        Request::GarantAvailable { on } => {
            if let Some(c) = st.conns.get_mut(&id) {
                c.garant = on;
            }
            return None;
        }
        Request::GarantReady => {
            if let Some(r) = my_room.and_then(|r| st.rooms.get_mut(&r)) {
                if r.hidden.contains(&id) && !r.ready_standby.contains(&id) {
                    r.ready_standby.push(id);
                }
            }
            return None;
        }
        Request::Conflict {
            player,
            seq,
            host_pos,
            audit_pos,
            proof,
        } => {
            let rid = my_room?;
            let room = st.rooms.get_mut(&rid)?;
            if !room.hidden.contains(&id) {
                return None;
            }
            let host = room.host;
            if proof || player == 0 {
                // Accusation de triche : un troisième garant, sans lien avec personne,
                // rejoue les pièces gardées par le relais et tranche.
                if room.arbitration.is_some() {
                    return None;
                }
                if arbitrate(st, rid, id) {
                    return None;
                }
                let room = st.rooms.get_mut(&rid)?;
                room.confirmed += 1;
            } else if room.members.contains(&player) {
                if let Some(p) = st.conns.get(&player).and_then(|c| c.pseudo.as_deref()) {
                    let low = p.to_ascii_lowercase();
                    if let Some(room) = st.rooms.get_mut(&rid) {
                        room.suspects.insert(low);
                    }
                }
                let n = st.next_id;
                st.next_id += 1;
                let room = st.rooms.get_mut(&rid)?;
                room.judges.insert(n, (player, id));
                if let Some(c) = st.conns.get(&player) {
                    transport::push(
                        &c.tx,
                        &Response::Judge {
                            conflict: n,
                            seq,
                            host_pos,
                            audit_pos,
                        },
                    );
                }
            }
            judge_outcome(st, rid, host, id);
            return None;
        }
        Request::Vote {
            conflict,
            auditor_right,
        } => {
            let rid = my_room?;
            let room = st.rooms.get_mut(&rid)?;
            let (player, auditor) = room.judges.remove(&conflict)?;
            if player != id {
                return None;
            }
            if auditor_right {
                room.confirmed += 1;
            } else {
                room.dismissed += 1;
            }
            let host = room.host;
            judge_outcome(st, rid, host, auditor);
            return None;
        }
        Request::BulkEdit { id: bulk, cells } => {
            let error = if my_room.is_some_and(|r| guards::frozen(st, r)) {
                "Monde en pause (son relais ne joint plus ses gardiens) : réessaie dans un instant."
                    .to_string()
            } else {
                bulk_edit(st, my_room, id, pseudo, cells)
            };
            if let Some(c) = st.conns.get(&id) {
                transport::push(&c.tx, &Response::BulkDone { id: bulk, error });
            }
            return None;
        }
        Request::Rtt { ms } => {
            if let Some(c) = st.conns.get_mut(&id) {
                let ms = ms as f32;
                if let Some(r) = c.rtt {
                    c.rtt_dev = c.rtt_dev * 0.8 + (ms - r).abs() * 0.2;
                }
                c.rtt = Some(c.rtt.map_or(ms, |r| r * 0.8 + ms * 0.2));
            }
            return None;
        }
        Request::Echo { nonce } => {
            if let Some(c) = st.conns.get_mut(&id) {
                if let Some((_, at)) = c.probe.take_if(|(n, _)| *n == nonce) {
                    probe_sample(c, at.elapsed().as_secs_f32() * 1000.0);
                }
            }
            return None;
        }
        Request::SetBackup { peer } => {
            if let Some(r) = my_room.and_then(|r| st.rooms.get_mut(&r)) {
                if r.host == id && r.members.contains(&peer) {
                    r.backup = Some(peer);
                }
            }
            return None;
        }
        Request::ToPeer { peer, data } => {
            // Copie de la partie pour le vérificateur : le relais en garde les dernières
            // (pièces d'un arbitrage : personne ne peut les retoucher après coup).
            if let Some(r) = my_room.and_then(|r| st.rooms.get_mut(&r)) {
                if r.kind == 0 && r.host == id && r.hidden.contains(&peer) {
                    if let Some(game::ToPeer::Backup { part, of, data }) = game::decode(&data) {
                        keep_evidence(r, part, of, data);
                    }
                }
            }
            if let Some(r) = my_room.and_then(|r| st.rooms.get(&r)) {
                if r.host == id && r.members.contains(&peer) {
                    if let Some(c) = st.conns.get(&peer) {
                        transport::push(&c.tx, &Response::FromHost { data });
                    }
                }
            }
            return None;
        }
        Request::Broadcast { data } => {
            if let Some(r) = my_room.and_then(|r| st.rooms.get(&r)) {
                if r.host == id {
                    let msg = Response::FromHost { data };
                    for m in &r.members {
                        if let Some(c) = st.conns.get(m) {
                            transport::push(&c.tx, &msg);
                        }
                    }
                }
            }
            return None;
        }
        _ => err(ErrorCode::Internal, "requête inattendue"),
    })
}

fn handle(
    req: Request,
    state: &Mutex<State>,
    session: &mut Session,
    peer: &str,
    id: u64,
) -> Option<Response> {
    let mut st = state.lock().unwrap();
    let st = &mut *st;
    let room_request = matches!(
        req,
        Request::Servers { .. }
            | Request::Host { .. }
            | Request::Search { .. }
            | Request::Report { .. }
            | Request::Unreport { .. }
            | Request::Ban { .. }
            | Request::ServerAdmin { .. }
            | Request::MatchResult { .. }
            | Request::CheatFound { .. }
            | Request::Leaderboard { .. }
            | Request::Friend { .. }
            | Request::Party { .. }
            | Request::SkinPut { .. }
            | Request::SkinGet { .. }
            | Request::Maps
            | Request::MapPut { .. }
            | Request::MapGet { .. }
            | Request::Caps { .. }
            | Request::Position { .. }
            | Request::ToZone { .. }
            | Request::Nearby { .. }
            | Request::ShardReady { .. }
            | Request::Archive { .. }
            | Request::Ack { .. }
            | Request::Accuse { .. }
            | Request::Audited { .. }
            | Request::Spot { .. }
            | Request::VoiceTo { .. }
            | Request::Handover { .. }
            | Request::RoomStarted { .. }
            | Request::Join { .. }
            | Request::Leave
            | Request::Doze { .. }
            | Request::ToHost { .. }
            | Request::SetBackup { .. }
            | Request::Rtt { .. }
            | Request::Echo { .. }
            | Request::BulkEdit { .. }
            | Request::Served { .. }
            | Request::StandbyReady { .. }
            | Request::Lease { .. }
            | Request::Resign { .. }
            | Request::GarantAvailable { .. }
            | Request::GarantReady
            | Request::Conflict { .. }
            | Request::Vote { .. }
            | Request::Voice { .. }
            | Request::ToPeer { .. }
            | Request::Broadcast { .. }
            | Request::ProvinceReady { .. }
            | Request::ProvinceStandbyReady { .. }
            | Request::ProvinceCopied { .. }
            | Request::ProvinceLease { .. }
            | Request::ProvinceArchive { .. }
            | Request::ProvinceShards { .. }
            | Request::ProvinceRelease { .. }
            | Request::ProvinceBag { .. }
            | Request::ProvinceBagBack { .. }
    );
    // Témoin entré chez ce relais de secours : sa liste et son vote.
    if let Request::WitnessBeat { committee, vote } = &req {
        if let Session::Witness { cert } = &*session {
            let cert = cert.clone();
            guards::witness_beat(st, id, &cert, committee, vote.as_ref());
        }
        return None;
    }
    if matches!(
        req,
        Request::WitnessLogin { .. } | Request::WitnessProve { .. }
    ) {
        return Some(guards::witness_login(st, session, req));
    }
    if room_request {
        let Session::LoggedIn { pseudo } = &*session else {
            return (!req.is_oneway()).then(|| err(ErrorCode::NotLoggedIn, ""));
        };
        let pseudo = pseudo.clone();
        let resp = handle_room(req, st, id, &pseudo);
        // Ce qui a changé dans son monde part aussi à son relais de secours.
        if let Some(rid) = st.conns.get(&id).and_then(|c| c.room) {
            flush_replica(st, rid);
        }
        return resp;
    }
    Some(handle_account(req, st, session, peer, id))
}

fn handle_account(
    req: Request,
    st: &mut State,
    session: &mut Session,
    peer: &str,
    id: u64,
) -> Response {
    match req {
        Request::Hello { .. } => err(ErrorCode::Protocol, "déjà dit bonjour"),
        Request::Ping => Response::Pong,
        Request::Verdict { case, guilty } => {
            let rid = st
                .rooms
                .iter()
                .find(|(_, r)| r.arbitration.is_some_and(|a| a.0 == case && a.1 == id))
                .map(|(&rid, _)| rid);
            if let Some(rid) = rid {
                arbitration_done(st, rid, Some(guilty));
            }
            Response::Done
        }
        Request::Whois { pseudo } => Response::Whois {
            pubkey: st.registry.lookup(&pseudo).map(|r| r.1),
        },
        Request::PeerServers { kind } => Response::Servers {
            list: local_servers(st, kind, None),
        },
        Request::Register {
            pseudo,
            pubkey,
            auth,
            vault,
            pow,
        } => {
            if !proto::valid_pseudo(&pseudo) {
                return err(ErrorCode::BadPseudo, "");
            }
            if let Some(wait) = st.limits.register_wait(peer) {
                return err(
                    ErrorCode::TooManyAttempts,
                    format!("Réessaie dans {wait} s."),
                );
            }
            if !crypto::pow_ok(&pseudo, &pubkey, pow, st.cfg.pow_bits) {
                return err(ErrorCode::BadProofOfWork, "");
            }
            if st.registry.lookup(&pseudo).is_some() {
                return err(ErrorCode::PseudoTaken, "");
            }
            if vault.len() > 256 {
                return err(ErrorCode::Internal, "coffre trop grand");
            }
            let record = match accounts::Account::new(&pseudo, pubkey, &auth, vault) {
                Ok(r) => r,
                Err(e) => return err(ErrorCode::Internal, format!("{e:#}")),
            };
            if let Err(e) = st.accounts.save(&record) {
                return err(ErrorCode::Internal, format!("{e:#}"));
            }
            match st.registry.append(Body::Register {
                pseudo: pseudo.clone(),
                pubkey,
            }) {
                Ok(index) => {
                    st.limits.registered(peer);
                    log::info!("compte « {pseudo} » inscrit (ligne {index})");
                    Response::Registered { index }
                }
                Err(e) => {
                    st.accounts.remove(&pseudo);
                    err(ErrorCode::Internal, format!("{e:#}"))
                }
            }
        }
        Request::Login {
            pseudo,
            auth,
            totp,
            hw,
            device,
        } => {
            // Par le tunnel iroh, `peer` est la vraie adresse du joueur (préambule).
            let mut keys = vec![format!("pseudo:{}", pseudo.to_ascii_lowercase())];
            if peer != "127.0.0.1" && peer != "::1" {
                keys.push(format!("ip:{peer}"));
            }
            if let Some(wait) = keys.iter().filter_map(|k| st.limits.wait(k)).max() {
                return err(
                    ErrorCode::TooManyAttempts,
                    format!("Réessaie dans {wait} s."),
                );
            }
            let record = st.accounts.load(&pseudo);
            // Même travail que le compte existe ou non (pas d'indice sur les pseudos).
            let ok = accounts::check_auth(record.as_ref().map(|r| r.auth_hash.as_str()), &auth);
            let Some(mut record) = record.filter(|_| ok) else {
                for k in &keys {
                    st.limits.failed(k);
                }
                return err(ErrorCode::WrongCredentials, "");
            };
            if st.registry.is_banned(&record.pubkey) {
                return err(ErrorCode::Banned, "(à vie)");
            }
            if let Some(until) = st.hardware.banned_until(&hw) {
                let days = until.saturating_sub(hardware::now()).div_ceil(86_400);
                return err(
                    ErrorCode::Banned,
                    format!("Cette machine est bannie encore {days} jour(s)."),
                );
            }
            if let Some(c) = st.conns.get_mut(&id) {
                c.hw = hw.clone();
            }
            st.hardware.seen(&record.pseudo, hw);
            // Appareil de confiance (code 2FA déjà donné sur cet appareil) : pas de 2FA.
            let trusted = device.is_some_and(|d| record.trusts(&d));
            let mut new_device = None;
            if let (Some(sealed), false) = (&record.totp, trusted) {
                let Some(code) = totp else {
                    return Response::NeedTotp;
                };
                let secret = crypto::open(&st.id.vault_key, sealed).unwrap_or_default();
                match totp::check(&secret, code, totp::now_step()) {
                    Some(step) if step > record.last_totp_step => {
                        record.last_totp_step = step;
                        new_device = Some(record.trust_new_device());
                        let _ = st.accounts.save(&record);
                    }
                    _ => {
                        for k in &keys {
                            st.limits.failed(k);
                        }
                        return err(ErrorCode::WrongTotp, "");
                    }
                }
            }
            for k in &keys {
                st.limits.succeeded(k);
            }
            let mut challenge = [0u8; 32];
            rand::rngs::OsRng.fill_bytes(&mut challenge);
            *session = Session::Proving {
                pseudo: record.pseudo.clone(),
                pubkey: record.pubkey,
                challenge,
            };
            Response::Vault {
                vault: record.vault,
                challenge,
                device: new_device,
            }
        }
        // Relais d'un monde repris : la connexion de garant d'un joueur entre avec le
        // certificat de la clé de son compte (de l'ancien relais du monde, ou de celui-ci).
        // Elle pourra tenir le prochain relais de secours du monde, ou en être témoin.
        Request::GarantLogin { cert } => {
            // (v147) Relais d'un pays : un de ses joueurs lui prête sa connexion de garant (le
            // pays choisit lui-même les hôtes de ses provinces).
            if let Some(ok) = provinces::admits_garant(st, &cert) {
                if !ok {
                    return err(ErrorCode::Refused, "garant refusé par le pays");
                }
                let mut challenge = [0u8; 32];
                rand::rngs::OsRng.fill_bytes(&mut challenge);
                *session = Session::GarantProving {
                    pseudo: cert.pseudo,
                    key: cert.key,
                    challenge,
                };
                return Response::TakeoverChallenge {
                    challenge,
                    proof: None,
                };
            }
            let Some(sb) = st.standby.as_ref().filter(|sb| sb.active) else {
                return err(ErrorCode::Refused, "Pas de monde repris ici.");
            };
            let mine = st.cert_key.verifying_key();
            let ok = cert.expires >= now_secs()
                && (cert.valid(&sb.root, sb.room)
                    || sb.resumed.is_some_and(|r| cert.valid(&mine, r)));
            if !ok {
                return err(ErrorCode::WrongCredentials, "certificat");
            }
            let mut challenge = [0u8; 32];
            rand::rngs::OsRng.fill_bytes(&mut challenge);
            *session = Session::GarantProving {
                pseudo: cert.pseudo,
                key: cert.key,
                challenge,
            };
            Response::TakeoverChallenge {
                challenge,
                proof: None,
            }
        }
        Request::GarantProve { signature } => {
            let Session::GarantProving {
                pseudo,
                key,
                challenge,
            } = &*session
            else {
                return err(ErrorCode::NotLoggedIn, "");
            };
            let ok = VerifyingKey::from_bytes(key)
                .ok()
                .zip(Signature::from_slice(&signature).ok())
                .is_some_and(|(k, s)| k.verify(challenge, &s).is_ok());
            if !ok {
                *session = Session::Anonymous;
                return err(ErrorCode::WrongCredentials, "signature");
            }
            let (pseudo, key) = (pseudo.clone(), *key);
            if let Some(c) = st.conns.get_mut(&id) {
                c.pseudo = Some(pseudo.clone());
                c.account = Some(key);
            }
            log::info!("monde repris : connexion de garant de « {pseudo} »");
            *session = Session::LoggedIn {
                pseudo: pseudo.clone(),
            };
            Response::Welcome {
                pseudo,
                totp: false,
            }
        }
        // Relais de secours, après la chute du relais du monde : un joueur de ce monde
        // entre avec le certificat de la clé de son compte.
        Request::Takeover { cert } => {
            // Relais de province (v138) : un joueur du monde entre, avec le certificat de
            // la clé de son compte.
            if let Some(ok) = provinces::admits(st, &cert) {
                if !ok {
                    return err(ErrorCode::WrongCredentials, "certificat");
                }
                let mut challenge = [0u8; 32];
                rand::rngs::OsRng.fill_bytes(&mut challenge);
                *session = Session::TakingOver {
                    pseudo: cert.pseudo,
                    key: cert.key,
                    challenge,
                };
                return Response::TakeoverChallenge {
                    challenge,
                    proof: None,
                };
            }
            let Some(sb) = st.standby.as_ref() else {
                return err(ErrorCode::Refused, "Pas de relais de secours ici.");
            };
            if !sb.active {
                return err(ErrorCode::Refused, "Le relais du monde répond encore.");
            }
            if !cert.valid(&sb.root, sb.room) || cert.expires < now_secs() {
                return err(ErrorCode::WrongCredentials, "certificat");
            }
            let mut challenge = [0u8; 32];
            rand::rngs::OsRng.fill_bytes(&mut challenge);
            *session = Session::TakingOver {
                pseudo: cert.pseudo,
                key: cert.key,
                challenge,
            };
            // (Le joueur vérifie qu'il a le droit de servir le monde avant d'entrer.)
            Response::TakeoverChallenge {
                challenge,
                proof: sb.proof.clone(),
            }
        }
        Request::Wake { room, cert } => {
            // (v146) Relais du monde : un joueur qui l'avait quitté au fond d'un pays revient,
            // avec le certificat de la clé de son compte signé ici pour ce monde.
            let ok = st.province_room.is_none()
                && st
                    .rooms
                    .get(&room)
                    .is_some_and(|r| r.kind == 1 && !r.provinces.is_empty())
                && cert.valid(&st.cert_key.verifying_key(), room)
                && cert.expires >= now_secs();
            if !ok {
                return err(ErrorCode::WrongCredentials, "certificat");
            }
            let mut challenge = [0u8; 32];
            rand::rngs::OsRng.fill_bytes(&mut challenge);
            *session = Session::Waking {
                pseudo: cert.pseudo,
                key: cert.key,
                challenge,
                room,
            };
            Response::TakeoverChallenge {
                challenge,
                proof: None,
            }
        }
        Request::TakeoverProve { signature } if matches!(&*session, Session::Waking { .. }) => {
            let Session::Waking {
                pseudo,
                key,
                challenge,
                room,
            } = &*session
            else {
                return err(ErrorCode::NotLoggedIn, "");
            };
            let ok = VerifyingKey::from_bytes(key)
                .ok()
                .zip(Signature::from_slice(&signature).ok())
                .is_some_and(|(k, s)| k.verify(challenge, &s).is_ok());
            if !ok {
                *session = Session::Anonymous;
                return err(ErrorCode::WrongCredentials, "signature");
            }
            let (pseudo, key, room) = (pseudo.clone(), *key, *room);
            *session = Session::LoggedIn {
                pseudo: pseudo.clone(),
            };
            if let Some(c) = st.conns.get_mut(&id) {
                c.pseudo = Some(pseudo.clone());
                c.account = Some(key);
            }
            log::info!("monde {room} : « {pseudo} » revient du fond d'un pays");
            join_room(st, id, &pseudo, room)
        }
        Request::TakeoverProve { signature } => {
            let Session::TakingOver {
                pseudo,
                key,
                challenge,
            } = &*session
            else {
                return err(ErrorCode::NotLoggedIn, "");
            };
            let ok = VerifyingKey::from_bytes(key)
                .ok()
                .zip(Signature::from_slice(&signature).ok())
                .is_some_and(|(k, s)| k.verify(challenge, &s).is_ok());
            if !ok {
                *session = Session::Anonymous;
                return err(ErrorCode::WrongCredentials, "signature");
            }
            let (pseudo, key) = (pseudo.clone(), *key);
            *session = Session::LoggedIn {
                pseudo: pseudo.clone(),
            };
            // Relais de province (v138) : dans la province ; (v147) l'appel de garants valable
            // ici.
            if let Some(rid) = st.province_room {
                if let Some(c) = st.conns.get_mut(&id) {
                    c.pseudo = Some(pseudo.clone());
                    c.account = Some(key);
                }
                let r = join_room(st, id, &pseudo, rid);
                if matches!(r, Response::Joined { .. }) {
                    provinces::greet(st, rid, id);
                }
                return r;
            }
            log::info!("relais de secours : « {pseudo} » reprend le monde");
            take_over_join(st, id, &pseudo, key)
        }
        Request::RegionHost {
            room,
            region,
            slot,
            pseudo,
            signer,
            token,
        } => provinces::region_host(st, session, id, room, region, slot, pseudo, signer, token),
        Request::ProvinceFollow {
            room,
            province,
            epoch,
            pseudo,
            signer,
            token,
            node,
            addr,
            key,
        } => provinces::follow(
            st,
            session,
            id,
            room,
            province,
            epoch,
            pseudo,
            signer,
            token,
            (node, addr, key),
        ),
        Request::ProvinceChild {
            room,
            province,
            size,
            epoch,
            pseudo,
            signer,
            token,
            standby,
        } => provinces::child_hello(
            st, session, id, room, province, size, epoch, pseudo, signer, token, standby,
        ),
        Request::Prove { signature } => {
            let Session::Proving {
                pseudo,
                pubkey,
                challenge,
            } = &*session
            else {
                return err(ErrorCode::NotLoggedIn, "");
            };
            let ok = VerifyingKey::from_bytes(pubkey)
                .ok()
                .zip(Signature::from_slice(&signature).ok())
                .is_some_and(|(k, s)| k.verify(challenge, &s).is_ok());
            if !ok {
                *session = Session::Anonymous;
                return err(ErrorCode::WrongCredentials, "signature");
            }
            let pseudo = pseudo.clone();
            let totp = st.accounts.load(&pseudo).is_some_and(|r| r.totp.is_some());
            log::info!("« {pseudo} » connecté");
            *session = Session::LoggedIn {
                pseudo: pseudo.clone(),
            };
            if let Some(c) = st.conns.get_mut(&id) {
                c.pseudo = Some(pseudo.clone());
            }
            Response::Welcome { pseudo, totp }
        }
        Request::TotpBegin | Request::TotpConfirm { .. } | Request::TotpDisable { .. } => {
            let Session::LoggedIn { pseudo } = &*session else {
                return err(ErrorCode::NotLoggedIn, "");
            };
            let Some(mut record) = st.accounts.load(pseudo) else {
                return err(ErrorCode::NotLoggedIn, "");
            };
            let vk = st.id.vault_key;
            let resp = match req {
                Request::TotpBegin => {
                    let mut secret = vec![0u8; 20];
                    rand::rngs::OsRng.fill_bytes(&mut secret);
                    record.totp_pending = Some(crypto::seal(&vk, &secret));
                    Response::TotpSecret { secret }
                }
                Request::TotpConfirm { code } => {
                    let secret = record
                        .totp_pending
                        .as_ref()
                        .and_then(|s| crypto::open(&vk, s).ok());
                    match secret
                        .and_then(|s| totp::check(&s, code, totp::now_step()).map(|st| (s, st)))
                    {
                        Some((secret, step)) => {
                            record.totp = Some(crypto::seal(&vk, &secret));
                            record.totp_pending = None;
                            record.last_totp_step = step;
                            Response::TotpOn
                        }
                        None => return err(ErrorCode::WrongTotp, ""),
                    }
                }
                Request::TotpDisable { code } => {
                    let secret = record.totp.as_ref().and_then(|s| crypto::open(&vk, s).ok());
                    match secret.and_then(|s| totp::check(&s, code, totp::now_step())) {
                        Some(_) => {
                            record.totp = None;
                            Response::TotpOff
                        }
                        None => return err(ErrorCode::WrongTotp, ""),
                    }
                }
                _ => unreachable!(),
            };
            if let Err(e) = st.accounts.save(&record) {
                return err(ErrorCode::Internal, format!("{e:#}"));
            }
            resp
        }
        _ => err(ErrorCode::Internal, "requête inattendue"),
    }
}

/// Essais (jeux en réseau simulés) : donne des parties jouées à un compte avant de lancer
/// le relais (un garant doit en avoir joué quelques-unes).
#[doc(hidden)]
pub fn test_seed_games(dir: &Path, pseudo: &str, games: u32) {
    if let Ok(mut st) = stats::Store::open(dir.join("stats")) {
        st.get_mut(pseudo).games = games;
        st.save(pseudo);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn tunnel_preamble_gives_the_real_address() {
        use std::io::{Read, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let mut c = TcpStream::connect(addr).unwrap();
        c.write_all(&minelib_p2p::preamble("203.0.113.7")).unwrap();
        c.write_all(b"suite").unwrap();
        let (s, _) = l.accept().unwrap();
        assert_eq!(
            super::tunnel_peer(&s).unwrap().as_deref(),
            Some("203.0.113.7")
        );
        let mut rest = [0u8; 5];
        (&s).read_exact(&mut rest).unwrap();
        assert_eq!(&rest, b"suite");
        // Connexion locale ordinaire : rien n'est consommé.
        let mut c = TcpStream::connect(addr).unwrap();
        c.write_all(b"\x00\x20abc").unwrap();
        let (s, _) = l.accept().unwrap();
        assert_eq!(super::tunnel_peer(&s).unwrap(), None);
        let mut rest = [0u8; 5];
        (&s).read_exact(&mut rest).unwrap();
        assert_eq!(&rest, b"\x00\x20abc");
    }

    use super::*;
    use minelib_net::client::{Client, Cmd, Event};
    use std::time::Instant;

    fn wait(c: &Client, pred: impl Fn(&Event) -> bool) -> Event {
        let t = Instant::now();
        loop {
            if let Some(e) = c.poll() {
                if pred(&e) {
                    return e;
                }
                if let Event::Error(m) = &e {
                    panic!("erreur : {m}");
                }
            }
            assert!(t.elapsed() < Duration::from_secs(30), "délai dépassé");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn wait_error(c: &Client) -> String {
        let t = Instant::now();
        loop {
            if let Some(Event::Error(m)) = c.poll() {
                return m;
            }
            assert!(t.elapsed() < Duration::from_secs(30));
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Des centaines de joueurs connectés : le relais du monde les sert sur quelques fils
    /// (avant la v131 : deux fils par connexion, 600 ici).
    #[cfg(target_os = "linux")]
    #[test]
    fn many_connections_are_served_by_a_few_threads() {
        use std::io::{Read, Write};
        let dir = std::env::temp_dir().join(format!("minelib-fils-{}", rand::random::<u32>()));
        let h = start(Config {
            dir: dir.clone(),
            bind: "127.0.0.1:0".into(),
            pow_bits: 4,
            name: "essai".into(),
        })
        .unwrap();
        let threads = || std::fs::read_dir("/proc/self/task").unwrap().count();
        fn frame(s: &mut std::net::TcpStream, data: &[u8]) {
            s.write_all(&(data.len() as u16).to_be_bytes()).unwrap();
            s.write_all(data).unwrap();
        }
        fn read(s: &mut std::net::TcpStream) -> Vec<u8> {
            let mut len = [0u8; 2];
            s.read_exact(&mut len).unwrap();
            let mut b = vec![0u8; u16::from_be_bytes(len) as usize];
            s.read_exact(&mut b).unwrap();
            b
        }
        // Une connexion : poignée de main, bonjour, réponse (sans fil côté joueur).
        let open = || {
            let mut s = std::net::TcpStream::connect(h.addr).unwrap();
            s.set_nodelay(true).unwrap();
            s.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
            let (private, _) = transport::generate_static();
            let mut hs = snow::Builder::new(transport::PATTERN.parse().unwrap())
                .local_private_key(&private)
                .build_initiator()
                .unwrap();
            let mut msg = vec![0u8; transport::MAX];
            let n = hs.write_message(&[], &mut msg).unwrap();
            frame(&mut s, &msg[..n]);
            let b = read(&mut s);
            hs.read_message(&b, &mut msg).unwrap();
            let n = hs.write_message(&[], &mut msg).unwrap();
            frame(&mut s, &msg[..n]);
            let mut noise = hs.into_transport_mode().unwrap();
            let hello = postcard::to_allocvec(&Request::Hello {
                protocol: proto::PROTOCOL,
                client: "essai".into(),
            })
            .unwrap();
            let n = noise.write_message(&hello, &mut msg).unwrap();
            frame(&mut s, &msg[..n]);
            let b = read(&mut s);
            let n = noise.read_message(&b, &mut msg).unwrap();
            let r: Response = postcard::from_bytes(&msg[..n]).unwrap();
            assert!(matches!(r, Response::Hello { .. }));
            s
        };
        // (Le relais a démarré ses fils.)
        let first = open();
        let before = threads();
        let conns: Vec<std::net::TcpStream> = (0..300).map(|_| open()).collect();
        let after = threads();
        eprintln!("fils : {before} avant, {after} avec 300 connexions de plus");
        assert!(
            after < before + 10,
            "300 connexions de plus : {} fils de plus",
            after - before
        );
        drop((first, conns));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Essai à la main d'un relais lancé à part (le relais Windows sous Wine, par exemple) :
    /// 200 connexions, bonjour et ping de chacune.
    /// `MINELIB_ADRESSE=127.0.0.1:27999 cargo test -p minelib-relay relais_lance -- --ignored`
    #[test]
    #[ignore]
    fn relais_lance_a_part() {
        use std::io::{Read, Write};
        let addr = std::env::var("MINELIB_ADRESSE").expect("MINELIB_ADRESSE");
        fn frame(s: &mut std::net::TcpStream, data: &[u8]) {
            let mut b = (data.len() as u16).to_be_bytes().to_vec();
            b.extend_from_slice(data);
            s.write_all(&b).unwrap();
        }
        fn read(s: &mut std::net::TcpStream) -> Vec<u8> {
            let mut len = [0u8; 2];
            s.read_exact(&mut len).unwrap();
            let mut b = vec![0u8; u16::from_be_bytes(len) as usize];
            s.read_exact(&mut b).unwrap();
            b
        }
        let mut msg = vec![0u8; transport::MAX];
        let mut conns = Vec::new();
        for _ in 0..200 {
            let mut s = std::net::TcpStream::connect(&addr).unwrap();
            s.set_nodelay(true).unwrap();
            s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
            let (private, _) = transport::generate_static();
            let mut hs = snow::Builder::new(transport::PATTERN.parse().unwrap())
                .local_private_key(&private)
                .build_initiator()
                .unwrap();
            let k = hs.write_message(&[], &mut msg).unwrap();
            frame(&mut s, &msg[..k]);
            let b = read(&mut s);
            hs.read_message(&b, &mut msg).unwrap();
            let k = hs.write_message(&[], &mut msg).unwrap();
            frame(&mut s, &msg[..k]);
            let mut noise = hs.into_transport_mode().unwrap();
            let hello = postcard::to_allocvec(&Request::Hello {
                protocol: proto::PROTOCOL,
                client: "essai".into(),
            })
            .unwrap();
            let k = noise.write_message(&hello, &mut msg).unwrap();
            frame(&mut s, &msg[..k]);
            let b = read(&mut s);
            let k = noise.read_message(&b, &mut msg).unwrap();
            let r: Response = postcard::from_bytes(&msg[..k]).unwrap();
            assert!(matches!(r, Response::Hello { .. }), "{r:?}");
            conns.push((s, noise));
        }
        let ping = postcard::to_allocvec(&Request::Ping).unwrap();
        for (s, noise) in &mut conns {
            let k = noise.write_message(&ping, &mut msg).unwrap();
            frame(s, &msg[..k]);
        }
        for (s, noise) in &mut conns {
            let b = read(s);
            let k = noise.read_message(&b, &mut msg).unwrap();
            let r: Response = postcard::from_bytes(&msg[..k]).unwrap();
            assert!(matches!(r, Response::Pong));
        }
        eprintln!("200 connexions : bonjour et ping, tout répond");
    }

    /// Mesure à la main : 8000 joueurs connectés au relais du monde (poignée de main et
    /// bonjour), puis un ping de chacun. Fils, mémoire, temps.
    /// `cargo test --release -p minelib-relay connexions -- --ignored --nocapture`
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore]
    fn connexions_du_relais_du_monde() {
        use std::io::{Read, Write};
        let dir = std::env::temp_dir().join(format!("minelib-cnx-{}", rand::random::<u32>()));
        let h = start(Config {
            dir: dir.clone(),
            bind: "127.0.0.1:0".into(),
            pow_bits: 4,
            name: "essai".into(),
        })
        .unwrap();
        let status = |key: &str| {
            std::fs::read_to_string("/proc/self/status")
                .unwrap()
                .lines()
                .find(|l| l.starts_with(key))
                .map(|l| l.split_whitespace().nth(1).unwrap().parse::<u64>().unwrap())
                .unwrap()
        };
        fn frame(s: &mut std::net::TcpStream, data: &[u8]) {
            let mut b = (data.len() as u16).to_be_bytes().to_vec();
            b.extend_from_slice(data);
            s.write_all(&b).unwrap();
        }
        fn read(s: &mut std::net::TcpStream) -> Vec<u8> {
            let mut len = [0u8; 2];
            s.read_exact(&mut len).unwrap();
            let mut b = vec![0u8; u16::from_be_bytes(len) as usize];
            s.read_exact(&mut b).unwrap();
            b
        }
        let mut msg = vec![0u8; transport::MAX];
        let mut open = |n: usize| -> Vec<(std::net::TcpStream, snow::TransportState)> {
            (0..n)
                .map(|_| {
                    let mut s = std::net::TcpStream::connect(h.addr).unwrap();
                    s.set_nodelay(true).unwrap();
                    s.set_read_timeout(Some(Duration::from_secs(60))).unwrap();
                    let (private, _) = transport::generate_static();
                    let mut hs = snow::Builder::new(transport::PATTERN.parse().unwrap())
                        .local_private_key(&private)
                        .build_initiator()
                        .unwrap();
                    let k = hs.write_message(&[], &mut msg).unwrap();
                    frame(&mut s, &msg[..k]);
                    let b = read(&mut s);
                    hs.read_message(&b, &mut msg).unwrap();
                    let k = hs.write_message(&[], &mut msg).unwrap();
                    frame(&mut s, &msg[..k]);
                    let mut noise = hs.into_transport_mode().unwrap();
                    let hello = postcard::to_allocvec(&Request::Hello {
                        protocol: proto::PROTOCOL,
                        client: "essai".into(),
                    })
                    .unwrap();
                    let k = noise.write_message(&hello, &mut msg).unwrap();
                    frame(&mut s, &msg[..k]);
                    let b = read(&mut s);
                    noise.read_message(&b, &mut msg).unwrap();
                    (s, noise)
                })
                .collect()
        };
        let first = open(1);
        let (threads0, rss0) = (status("Threads:"), status("VmRSS:"));
        let t = Instant::now();
        let n = 8000;
        let mut conns = open(n);
        let connect = t.elapsed();
        let (threads1, rss1) = (status("Threads:"), status("VmRSS:"));
        // Un ping de chacun, tous envoyés, puis toutes les réponses lues.
        let ping = postcard::to_allocvec(&Request::Ping).unwrap();
        let t = Instant::now();
        for (s, noise) in &mut conns {
            let k = noise.write_message(&ping, &mut msg).unwrap();
            frame(s, &msg[..k]);
        }
        for (s, noise) in &mut conns {
            let b = read(s);
            let k = noise.read_message(&b, &mut msg).unwrap();
            let r: Response = postcard::from_bytes(&msg[..k]).unwrap();
            assert!(matches!(r, Response::Pong));
        }
        let pings = t.elapsed();
        eprintln!(
            "{n} connexions en {:.1} s ; fils {threads0} -> {threads1} ; mémoire {} Mo -> {} Mo \
             ({:.1} Ko par connexion, joueur et relais compris) ; {n} pings en {:.0} ms",
            connect.as_secs_f32(),
            rss0 / 1024,
            rss1 / 1024,
            (rss1 - rss0) as f32 / n as f32,
            pings.as_secs_f32() * 1000.0
        );
        assert!(threads1 < threads0 + 10);
        drop((first, conns));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Relais de secours : on n'y entre qu'après la chute du relais du monde, avec un
    /// certificat de ce monde-là signé par ce relais-là, et la clé du compte qu'il nomme.
    #[test]
    fn a_standby_relay_only_lets_in_the_players_of_its_world() {
        let dir = std::env::temp_dir().join(format!("minelib-secours-{}", rand::random::<u32>()));
        let h = start(Config {
            dir: dir.clone(),
            bind: "127.0.0.1:0".into(),
            pow_bits: 4,
            name: "secours".into(),
        })
        .unwrap();
        let root = SigningKey::from_bytes(&[3; 32]);
        let info = proto::StandbyInfo {
            name: "Monde".into(),
            map: "monde".into(),
            map_sig: [1; 32],
            max_players: 0,
            creator: "Createur".into(),
            server: "createur/monde".into(),
            owner: "Createur".into(),
        };
        h.standby_begin(42, root.verifying_key().to_bytes(), info, "secours");
        let addr = h.addr.to_string();
        let eve = SigningKey::from_bytes(&[5; 32]);
        let cert = |root: &SigningKey, room: u64, key: &SigningKey| {
            sovereign::Cert::issue(
                root,
                room,
                "Eve",
                key.verifying_key().to_bytes(),
                now_secs() + 3600,
            )
        };
        let try_in = |c: sovereign::Cert, key: SigningKey| {
            let cl = Client::spawn(addr.clone(), Some(h.public));
            cl.send(Cmd::Takeover {
                cert: c,
                key,
                expect: None,
            });
            wait(&cl, |e| {
                matches!(
                    e,
                    Event::TakenOver { .. } | Event::Error(_) | Event::Offline(_)
                )
            })
        };
        // Le relais du monde répond encore : personne n'entre.
        assert!(matches!(
            try_in(cert(&root, 42, &eve), eve.clone()),
            Event::Error(_)
        ));
        assert!(h.take_over());
        // Autre monde, autre relais, ou clé qui n'est pas celle du certificat : refusé.
        let other = SigningKey::from_bytes(&[4; 32]);
        assert!(matches!(
            try_in(cert(&root, 43, &eve), eve.clone()),
            Event::Error(_)
        ));
        assert!(matches!(
            try_in(cert(&other, 42, &eve), eve.clone()),
            Event::Error(_)
        ));
        assert!(matches!(
            try_in(cert(&root, 42, &eve), other.clone()),
            Event::Error(_)
        ));
        assert_eq!(h.resumed(), None);
        // Le bon certificat et la bonne clé : le monde est repris, Eve en est la
        // propriétaire.
        match try_in(cert(&root, 42, &eve), eve.clone()) {
            Event::TakenOver { host, .. } => assert_eq!(host, "Eve"),
            e => panic!("{e:?}"),
        }
        assert!(h.resumed().is_some());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Le relais de secours ne reprend le monde qu'avec les votes signés d'une majorité de
    /// ses gardiens, et le prouve aux joueurs : un témoin entre avec le certificat de son
    /// compte (pas avec celui d'un autre monde), un vote signé d'une autre clé ne compte
    /// pas, et un joueur n'entre que si la preuve tient.
    #[test]
    fn a_standby_relay_takes_over_only_with_a_proven_majority() {
        use minelib_net::guard::{Committee, Vote};
        let dir = std::env::temp_dir().join(format!("minelib-secours-{}", rand::random::<u32>()));
        let h = start(Config {
            dir: dir.clone(),
            bind: "127.0.0.1:0".into(),
            pow_bits: 4,
            name: "secours".into(),
        })
        .unwrap();
        let root = SigningKey::from_bytes(&[3; 32]);
        let pk = root.verifying_key().to_bytes();
        let info = proto::StandbyInfo {
            name: "Monde".into(),
            map: "monde".into(),
            map_sig: [1; 32],
            max_players: 0,
            creator: "Createur".into(),
            server: "createur/monde".into(),
            owner: "Createur".into(),
        };
        h.standby_begin(42, pk, info, "Secours");
        let addr = h.addr.to_string();
        let (wit, eve) = (
            SigningKey::from_bytes(&[6; 32]),
            SigningKey::from_bytes(&[5; 32]),
        );
        let cert = |room: u64, who: &str, key: &SigningKey| {
            sovereign::Cert::issue(
                &root,
                room,
                who,
                key.verifying_key().to_bytes(),
                now_secs() + 3600,
            )
        };
        let c = Committee::sign(&root, 42, 1, Some("Secours".into()), vec!["Wit".into()]);
        // Le relais du monde se fait entendre, puis se tait ; le relais de secours seul ne
        // peut rien (sa voix ne compte pas).
        assert_eq!(h.standby_beat(&c, false), Some(1));
        assert!(!h.standby_tick());
        // Un témoin d'un autre monde : refusé ; le bon : entré.
        let login = |cert: sovereign::Cert| {
            let cl = Client::spawn(addr.clone(), Some(h.public));
            cl.send(Cmd::WitnessLogin {
                cert,
                key: wit.clone(),
            });
            let e = wait(&cl, |e| {
                matches!(e, Event::Link(_) | Event::Error(_) | Event::Offline(_))
            });
            (cl, e)
        };
        assert!(matches!(login(cert(43, "Wit", &wit)).1, Event::Error(_)));
        let (w, e) = login(cert(42, "Wit", &wit));
        let Event::Link(link) = e else {
            panic!("{e:?}")
        };
        std::thread::sleep(Duration::from_millis(minelib_net::guard::VOTE_MS + 100));
        assert!(!h.standby_tick(), "le relais de secours seul ne peut rien");
        // Un vote signé d'une autre clé que celle du certificat, ou pour une liste qui n'est
        // pas signée par le relais du monde : ne compte pas.
        let forged = Committee::sign(
            &SigningKey::from_bytes(&[8; 32]),
            42,
            1,
            Some("Secours".into()),
            vec!["Wit".into()],
        );
        for vote in [
            (c.clone(), Vote::sign(&eve, 42, 1, "Wit")),
            (forged, Vote::sign(&wit, 42, 1, "Wit")),
        ] {
            transport::push(
                &link,
                &Request::WitnessBeat {
                    committee: c.clone(),
                    vote: Some(vote),
                },
            );
        }
        std::thread::sleep(Duration::from_millis(200));
        assert!(!h.standby_tick());
        // Personne n'entre encore.
        let at = proto::StandbyAt {
            node: [0; 32],
            addr: addr.clone(),
            key: h.public,
            room: 42,
            pseudo: "Secours".into(),
            root: pk,
        };
        let try_in = |expect: Option<proto::StandbyAt>| {
            let cl = Client::spawn(addr.clone(), Some(h.public));
            cl.send(Cmd::Takeover {
                cert: cert(42, "Eve", &eve),
                key: eve.clone(),
                expect,
            });
            wait(&cl, |e| {
                matches!(
                    e,
                    Event::TakenOver { .. } | Event::Error(_) | Event::Offline(_)
                )
            })
        };
        assert!(matches!(try_in(Some(at.clone())), Event::Error(_)));
        // Le vote du témoin, signé de sa clé, pour la liste signée : quorum, reprise.
        transport::push(
            &link,
            &Request::WitnessBeat {
                committee: c.clone(),
                vote: Some((c.clone(), Vote::sign(&wit, 42, 1, "Wit"))),
            },
        );
        let t = Instant::now();
        while !h.standby_tick() {
            assert!(t.elapsed() < Duration::from_secs(5), "reprise");
            std::thread::sleep(Duration::from_millis(20));
        }
        // Un joueur qui attend un autre relais de secours (ou un autre monde) : il n'entre
        // pas ; le bon : il entre.
        let mut wrong = at.clone();
        wrong.pseudo = "Autre".into();
        assert!(matches!(try_in(Some(wrong)), Event::Error(_)));
        match try_in(Some(at)) {
            Event::TakenOver { host, .. } => assert_eq!(host, "Eve"),
            e => panic!("{e:?}"),
        }
        drop(w);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Un témoin menteur vote pour la reprise alors que le relais du monde sert encore
    /// (le relais de secours l'entend, et il ne se dit pas en pause) : un relais de secours
    /// honnête ne reprend pas. Dès que le relais du monde se dit en pause, il reprend.
    #[test]
    fn an_honest_standby_does_not_take_over_a_world_it_sees_served() {
        use minelib_net::guard::{Committee, Vote};
        let dir = std::env::temp_dir().join(format!("minelib-secours-{}", rand::random::<u32>()));
        let h = start(Config {
            dir: dir.clone(),
            bind: "127.0.0.1:0".into(),
            pow_bits: 4,
            name: "secours".into(),
        })
        .unwrap();
        let root = SigningKey::from_bytes(&[3; 32]);
        let info = proto::StandbyInfo {
            name: "Monde".into(),
            map: "monde".into(),
            map_sig: [1; 32],
            max_players: 0,
            creator: "Createur".into(),
            server: "createur/monde".into(),
            owner: "Createur".into(),
        };
        h.standby_begin(42, root.verifying_key().to_bytes(), info, "Secours");
        let wit = SigningKey::from_bytes(&[6; 32]);
        let cert = sovereign::Cert::issue(
            &root,
            42,
            "Wit",
            wit.verifying_key().to_bytes(),
            now_secs() + 3600,
        );
        let c = Committee::sign(&root, 42, 1, Some("Secours".into()), vec!["Wit".into()]);
        assert_eq!(h.standby_beat(&c, false), Some(1));
        let cl = Client::spawn(h.addr.to_string(), Some(h.public));
        cl.send(Cmd::WitnessLogin {
            cert,
            key: wit.clone(),
        });
        let Event::Link(link) = wait(&cl, |e| matches!(e, Event::Link(_))) else {
            unreachable!()
        };
        transport::push(
            &link,
            &Request::WitnessBeat {
                committee: c.clone(),
                vote: Some((c.clone(), Vote::sign(&wit, 42, 1, "Wit"))),
            },
        );
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(h.standby_beat(&c, false), Some(1));
        assert!(!h.standby_tick(), "le relais du monde sert sous ses yeux");
        assert_eq!(h.standby_beat(&c, true), Some(1));
        assert!(h.standby_tick(), "le relais du monde se dit en pause");
        drop(cl);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Un relais de secours qui reprend sans preuve (tricheur) : les joueurs n'y entrent
    /// pas.
    #[test]
    fn players_do_not_follow_a_standby_without_proof() {
        let dir = std::env::temp_dir().join(format!("minelib-secours-{}", rand::random::<u32>()));
        let h = start(Config {
            dir: dir.clone(),
            bind: "127.0.0.1:0".into(),
            pow_bits: 4,
            name: "secours".into(),
        })
        .unwrap();
        let root = SigningKey::from_bytes(&[3; 32]);
        let info = proto::StandbyInfo {
            name: "Monde".into(),
            map: "monde".into(),
            map_sig: [1; 32],
            max_players: 0,
            creator: "Createur".into(),
            server: "createur/monde".into(),
            owner: "Createur".into(),
        };
        h.standby_begin(42, root.verifying_key().to_bytes(), info, "Secours");
        assert!(h.take_over());
        let eve = SigningKey::from_bytes(&[5; 32]);
        let cl = Client::spawn(h.addr.to_string(), Some(h.public));
        cl.send(Cmd::Takeover {
            cert: sovereign::Cert::issue(
                &root,
                42,
                "Eve",
                eve.verifying_key().to_bytes(),
                now_secs() + 3600,
            ),
            key: eve,
            expect: Some(proto::StandbyAt {
                node: [0; 32],
                addr: h.addr.to_string(),
                key: h.public,
                room: 42,
                pseudo: "Secours".into(),
                root: root.verifying_key().to_bytes(),
            }),
        });
        let e = wait(&cl, |e| {
            matches!(
                e,
                Event::TakenOver { .. } | Event::Error(_) | Event::Offline(_)
            )
        });
        assert!(matches!(e, Event::Error(_)), "{e:?}");
        assert_eq!(h.resumed(), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn register_login_totp_and_registry_survive_a_restart() {
        let dir = std::env::temp_dir().join(format!("minelib-relais-{}", rand::random::<u32>()));
        let cfg = Config {
            dir: dir.clone(),
            bind: "127.0.0.1:0".into(),
            pow_bits: 8,
            name: "essai".into(),
        };
        let h = start(cfg.clone()).unwrap();
        let addr = h.addr.to_string();
        // Inscription (puis connexion automatique), clé du relais retenue.
        let c = Client::spawn(addr.clone(), None);
        c.send(Cmd::Register {
            pseudo: "Viendax".into(),
            password: "motdepasse1".into(),
        });
        match wait(&c, |e| matches!(e, Event::Pinned(_))) {
            Event::Pinned(k) => assert_eq!(k, h.public),
            _ => unreachable!(),
        }
        wait(&c, |e| matches!(e, Event::LoggedIn { totp: false, .. }));
        // Pseudo déjà pris, sans tenir compte des majuscules.
        let c2 = Client::spawn(addr.clone(), Some(h.public));
        c2.send(Cmd::Register {
            pseudo: "viendax".into(),
            password: "autremotdepasse".into(),
        });
        assert!(wait_error(&c2).contains("déjà pris"));
        // Mauvais mot de passe.
        c2.send(Cmd::Login {
            pseudo: "Viendax".into(),
            password: "mauvais!!".into(),
            totp: None,
            backup: None,
            device: None,
        });
        assert!(wait_error(&c2).contains("incorrect"));
        // Double authentification : activation, puis connexion qui la demande.
        c.send(Cmd::TotpBegin);
        let secret = match wait(&c, |e| matches!(e, Event::TotpSecret { .. })) {
            Event::TotpSecret { secret, uri } => {
                assert!(uri.starts_with("otpauth://totp/Minelib:Viendax?secret="));
                secret
            }
            _ => unreachable!(),
        };
        let now = totp::now_step();
        c.send(Cmd::TotpConfirm(totp::code(&secret, now)));
        wait(&c, |e| matches!(e, Event::TotpOn));
        c2.send(Cmd::Login {
            pseudo: "viendax".into(),
            password: "motdepasse1".into(),
            totp: None,
            backup: None,
            device: None,
        });
        wait(&c2, |e| matches!(e, Event::NeedTotp));
        // Le code déjà utilisé pour l'activation est refusé, le suivant accepté.
        c2.send(Cmd::Login {
            pseudo: "viendax".into(),
            password: "motdepasse1".into(),
            totp: Some(totp::code(&secret, now)),
            backup: None,
            device: None,
        });
        assert!(wait_error(&c2).contains("double authentification"));
        c2.send(Cmd::Login {
            pseudo: "viendax".into(),
            password: "motdepasse1".into(),
            totp: Some(totp::code(&secret, now + 1)),
            backup: None,
            device: None,
        });
        let token = match wait(&c2, |e| matches!(e, Event::DeviceTrusted { .. })) {
            Event::DeviceTrusted { token, .. } => token,
            _ => unreachable!(),
        };
        match wait(&c2, |e| matches!(e, Event::LoggedIn { .. })) {
            Event::LoggedIn { pseudo, totp, .. } => {
                assert_eq!(pseudo, "Viendax");
                assert!(totp);
            }
            _ => unreachable!(),
        }
        // Cet appareil est de confiance : plus de code 2FA (un autre jeton n'y suffit pas).
        let c3 = Client::spawn(addr.clone(), Some(h.public));
        c3.send(Cmd::Login {
            pseudo: "viendax".into(),
            password: "motdepasse1".into(),
            totp: None,
            backup: None,
            device: Some([7; 32]),
        });
        wait(&c3, |e| matches!(e, Event::NeedTotp));
        c3.send(Cmd::Login {
            pseudo: "viendax".into(),
            password: "motdepasse1".into(),
            totp: None,
            backup: None,
            device: Some(token),
        });
        wait(&c3, |e| matches!(e, Event::LoggedIn { .. }));
        // Le registre se relit et se vérifie (chaîne et signatures).
        let signer = SigningKey::from_bytes(
            &postcard::from_bytes::<Identity>(&std::fs::read(dir.join("relay.key")).unwrap())
                .unwrap()
                .sign_seed,
        );
        let reg = Registry::open(&dir.join("registry.log"), signer).unwrap();
        assert!(reg.lookup("VIENDAX").is_some());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn only_real_skin_headers_are_kept() {
        let mut png = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13];
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&64u32.to_be_bytes());
        png.extend_from_slice(&64u32.to_be_bytes());
        png.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
        assert!(valid_skin(&png));
        let mut big = png.clone();
        big[20..24].copy_from_slice(&128u32.to_be_bytes());
        assert!(!valid_skin(&big), "128 × 128");
        assert!(!valid_skin(b"GIF89a................................"));
        let mut huge = png.clone();
        huge.resize(25 * 1024, 0);
        assert!(!valid_skin(&huge));
        // Le nom du fichier ne dépend pas de ce que tape le joueur.
        let p = skin_path(Path::new("/r"), "../../etc/passwd");
        assert!(p.starts_with("/r/skins") && !p.to_string_lossy().contains(".."));
    }

    #[test]
    fn a_lobby_launches_together_and_stays_together() {
        let dir = std::env::temp_dir().join(format!("minelib-lobby-{}", rand::random::<u32>()));
        // Tous les joueurs simulés sont sur cette machine.
        ONE_FPS_PER_MACHINE.store(false, std::sync::atomic::Ordering::Relaxed);
        let h = start(Config {
            dir: dir.clone(),
            bind: "127.0.0.1:0".into(),
            pow_bits: 4,
            name: "essai".into(),
        })
        .unwrap();
        let addr = h.addr.to_string();
        let login = |pseudo: &str| {
            let c = Client::spawn(addr.clone(), Some(h.public));
            c.send(Cmd::Register {
                pseudo: pseudo.into(),
                password: "motdepasse".into(),
            });
            wait(&c, |e| matches!(e, Event::LoggedIn { .. }));
            c
        };
        let a = login("Ana");
        let b = login("Bob");
        let c = login("Cid");
        // Ana invite Bob : le lobby existe, Bob reçoit l'invitation et l'accepte.
        a.send(Cmd::Party {
            action: proto::PARTY_INVITE,
            pseudo: "bob".into(),
        });
        match wait(&b, |e| matches!(e, Event::PartyInvite(_))) {
            Event::PartyInvite(from) => assert_eq!(from, "Ana"),
            _ => unreachable!(),
        }
        // Accepter sans invitation : refusé.
        c.send(Cmd::Party {
            action: proto::PARTY_ACCEPT,
            pseudo: "Ana".into(),
        });
        assert!(wait_error(&c).contains("expirée"));
        b.send(Cmd::Party {
            action: proto::PARTY_ACCEPT,
            pseudo: "Ana".into(),
        });
        let pid = loop {
            match wait(&b, |e| matches!(e, Event::Party { .. })) {
                Event::Party { id, members } if members.len() == 2 => {
                    assert_eq!(members, vec!["Ana".to_string(), "Bob".to_string()]);
                    break id;
                }
                _ => {}
            }
        };
        assert_ne!(pid, 0);
        // Seule l'hôte du lobby lance ; Bob suit dans la même partie.
        let search = |p: &Client| {
            p.send(Cmd::Search {
                map_sig: [3; 32],
                map: "dust".into(),
                players: 10,
            })
        };
        search(&b);
        assert!(wait_error(&b).contains("hôte du lobby"));
        search(&a);
        let room = match wait(&a, |e| matches!(e, Event::Hosted(_))) {
            Event::Hosted(r) => r,
            _ => unreachable!(),
        };
        match wait(&b, |e| matches!(e, Event::PartyGo { .. })) {
            Event::PartyGo {
                room: r, map_sig, ..
            } => {
                assert_eq!((r, map_sig), (room, [3; 32]));
            }
            _ => unreachable!(),
        }
        b.send(Cmd::Join(room));
        wait(&b, |e| matches!(e, Event::Joined { .. }));
        match wait(&a, |e| matches!(e, Event::PeerJoined { .. })) {
            Event::PeerJoined { pseudo, party, .. } => {
                assert_eq!((pseudo.as_str(), party), ("Bob", pid));
            }
            _ => unreachable!(),
        }
        // Exclusion par l'hôte du lobby : le lobby est dissous (un seul joueur).
        b.send(Cmd::Party {
            action: proto::PARTY_KICK,
            pseudo: "Ana".into(),
        });
        assert!(wait_error(&b).contains("hôte du lobby"));
        a.send(Cmd::Party {
            action: proto::PARTY_PROMOTE,
            pseudo: "Bob".into(),
        });
        loop {
            if let Event::Party { members, .. } = wait(&a, |e| matches!(e, Event::Party { .. })) {
                if members.first().map(String::as_str) == Some("Bob") {
                    break;
                }
            }
        }
        b.send(Cmd::Party {
            action: proto::PARTY_KICK,
            pseudo: "Ana".into(),
        });
        loop {
            if let Event::Party { id, .. } = wait(&a, |e| matches!(e, Event::Party { .. })) {
                if id == 0 {
                    break;
                }
            }
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn rooms_are_listed_joined_and_relay_game_data_both_ways() {
        let dir = std::env::temp_dir().join(format!("minelib-salles-{}", rand::random::<u32>()));
        // Tous les joueurs simulés sont sur cette machine.
        ONE_FPS_PER_MACHINE.store(false, std::sync::atomic::Ordering::Relaxed);
        let h = start(Config {
            dir: dir.clone(),
            bind: "127.0.0.1:0".into(),
            pow_bits: 4,
            name: "essai".into(),
        })
        .unwrap();
        let addr = h.addr.to_string();
        let login = |pseudo: &str| {
            let c = Client::spawn(addr.clone(), Some(h.public));
            c.send(Cmd::Register {
                pseudo: pseudo.into(),
                password: "motdepasse".into(),
            });
            let link = match wait(&c, |e| matches!(e, Event::Link(_))) {
                Event::Link(l) => l,
                _ => unreachable!(),
            };
            wait(&c, |e| matches!(e, Event::LoggedIn { .. }));
            (c, link)
        };
        let (host, host_link) = login("Hote");
        let (guest, guest_link) = login("Invite");
        host.send(Cmd::Host {
            kind: 0,
            name: "Dust".into(),
            map: "de_dust".into(),
            map_sig: [7; 32],
            max_players: 10,
        });
        let room = match wait(&host, |e| matches!(e, Event::Hosted(_))) {
            Event::Hosted(r) => r,
            _ => unreachable!(),
        };
        guest.send(Cmd::Servers(0));
        match wait(&guest, |e| matches!(e, Event::Servers(_))) {
            Event::Servers(l) => {
                assert_eq!(l.len(), 1);
                assert_eq!(l[0].host, "Hote");
                assert_eq!(l[0].id, room);
            }
            _ => unreachable!(),
        }
        guest.send(Cmd::Join(room));
        match wait(&guest, |e| matches!(e, Event::Joined { .. })) {
            Event::Joined { map_sig, .. } => assert_eq!(map_sig, [7; 32]),
            _ => unreachable!(),
        }
        let peer = match wait(&host, |e| matches!(e, Event::PeerJoined { .. })) {
            Event::PeerJoined { peer, pseudo, .. } => {
                assert_eq!(pseudo, "Invite");
                peer
            }
            _ => unreachable!(),
        };
        // Données de jeu dans les deux sens, sans réponse.
        transport::push(
            &guest_link,
            &Request::ToHost {
                data: vec![1, 2, 3],
            },
        );
        match wait(&host, |e| matches!(e, Event::FromPeer { .. })) {
            Event::FromPeer { peer: p, data } => {
                assert_eq!(p, peer);
                assert_eq!(data, vec![1, 2, 3]);
            }
            _ => unreachable!(),
        }
        transport::push(&host_link, &Request::Broadcast { data: vec![9] });
        match wait(&guest, |e| matches!(e, Event::FromHost(_))) {
            Event::FromHost(d) => assert_eq!(d, vec![9]),
            _ => unreachable!(),
        }
        // Partie commencée : plus listée ; l'hôte part : partie fermée.
        host.send(Cmd::RoomStarted(true));
        std::thread::sleep(Duration::from_millis(100));
        guest.send(Cmd::Servers(0));
        match wait(&guest, |e| matches!(e, Event::Servers(_))) {
            Event::Servers(l) => assert!(l.is_empty()),
            _ => unreachable!(),
        }
        // L'hôte part : l'invité reprend la partie (garant de secours).
        host.send(Cmd::Leave);
        wait(&guest, |e| matches!(e, Event::Promoted { .. }));
        guest.send(Cmd::Leave);
        wait(&guest, |e| matches!(e, Event::Left));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn search_never_pairs_a_player_with_someone_he_reported() {
        let dir = std::env::temp_dir().join(format!("minelib-recherche-{}", rand::random::<u32>()));
        // Tous les joueurs simulés sont sur cette machine.
        ONE_FPS_PER_MACHINE.store(false, std::sync::atomic::Ordering::Relaxed);
        let h = start(Config {
            dir: dir.clone(),
            bind: "127.0.0.1:0".into(),
            pow_bits: 4,
            name: "essai".into(),
        })
        .unwrap();
        let addr = h.addr.to_string();
        let login = |pseudo: &str| {
            let c = Client::spawn(addr.clone(), Some(h.public));
            c.send(Cmd::Register {
                pseudo: pseudo.into(),
                password: "motdepasse".into(),
            });
            wait(&c, |e| matches!(e, Event::LoggedIn { .. }));
            c
        };
        let (a, b, c) = (login("Ana"), login("Bob"), login("Cid"));
        let map = [3u8; 32];
        let search = |cl: &Client| {
            cl.send(Cmd::Search {
                map_sig: map,
                map: "dust".into(),
                players: 10,
            });
            wait(cl, |e| matches!(e, Event::Hosted(_) | Event::Joined { .. }))
        };
        // Personne : Ana ouvre la partie ; Bob la rejoint.
        let ana_room = match search(&a) {
            Event::Hosted(r) => r,
            e => panic!("{e:?}"),
        };
        assert!(matches!(search(&b), Event::Joined { .. }));
        // Hors partie, on ne signale personne ; dans la partie, si.
        c.send(Cmd::Report("Ana".into()));
        assert!(wait_error(&c).contains("partie"));
        b.send(Cmd::Report("ana".into()));
        wait(&b, |e| matches!(e, Event::Reported(_)));
        // Nouvelle recherche : Bob ne retombe jamais avec Ana, Cid si.
        b.send(Cmd::Leave);
        wait(&b, |e| matches!(e, Event::Left));
        assert!(matches!(search(&b), Event::Hosted(_)), "pas avec Ana");
        // Signalement annulé : Bob peut de nouveau tomber avec Ana.
        b.send(Cmd::Unreport("Ana".into()));
        wait(&b, |e| matches!(e, Event::Unreported(_)));
        b.send(Cmd::Unreport("Ana".into()));
        assert!(wait_error(&b).contains("pas signalé"));
        b.send(Cmd::Leave);
        wait(&b, |e| matches!(e, Event::Left));
        match search(&b) {
            // Partie FPS : le nom de l'hôte n'est pas donné, on reconnaît la partie.
            Event::Joined { room, host, .. } => {
                assert_eq!(room, ana_room);
                assert!(host.is_empty(), "anonyme avant le début");
            }
            e => panic!("{e:?}"),
        }
        b.send(Cmd::Leave);
        wait(&b, |e| matches!(e, Event::Left));
        b.send(Cmd::Report("ana".into()));
        assert!(wait_error(&b).contains("partie"), "hors partie : refusé");
        match search(&c) {
            Event::Joined { map_sig, .. } => assert_eq!(map_sig, map),
            e => panic!("{e:?}"),
        }
        // Classement et amis.
        a.send(Cmd::Friend {
            pseudo: "cid".into(),
            add: true,
        });
        match wait(&a, |e| matches!(e, Event::Friends(_))) {
            Event::Friends(l) => assert_eq!(l, vec![("Cid".to_string(), true)]),
            _ => unreachable!(),
        }
        a.send(Cmd::Leaderboard { friends: true });
        match wait(&a, |e| matches!(e, Event::Leaderboard { .. })) {
            Event::Leaderboard { list, .. } => assert_eq!(list.len(), 2),
            _ => unreachable!(),
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn maps_are_stored_once_verified_and_downloaded() {
        let dir = std::env::temp_dir().join(format!("minelib-cartes-{}", rand::random::<u32>()));
        // Tous les joueurs simulés sont sur cette machine.
        ONE_FPS_PER_MACHINE.store(false, std::sync::atomic::Ordering::Relaxed);
        let h = start(Config {
            dir: dir.clone(),
            bind: "127.0.0.1:0".into(),
            pow_bits: 4,
            name: "essai".into(),
        })
        .unwrap();
        let c = Client::spawn(h.addr.to_string(), Some(h.public));
        c.send(Cmd::Register {
            pseudo: "Mapper".into(),
            password: "motdepasse".into(),
        });
        wait(&c, |e| matches!(e, Event::LoggedIn { .. }));
        // Grosse carte (plusieurs morceaux).
        let pkg = game::MapPackage {
            seed: 9,
            generator: "flat".into(),
            edits: (0..200_000u32).flat_map(|i| i.to_le_bytes()).collect(),
            ..Default::default()
        };
        let bytes = pkg.to_bytes();
        let sig = game::signature(&bytes);
        // Un monde de construction passe par le relais mais n'est pas proposé à tous.
        let build = game::MapPackage {
            seed: 3,
            generator: "flat".into(),
            ..Default::default()
        }
        .to_bytes();
        c.send(Cmd::UploadMap {
            sig: game::signature(&build),
            name: "Ma maison".into(),
            bytes: build,
            fps: false,
        });
        wait(&c, |e| matches!(e, Event::MapUploaded(_)));
        // Mauvaise signature : refusée.
        c.send(Cmd::UploadMap {
            sig: [0; 32],
            name: "Fausse".into(),
            bytes: bytes.clone(),
            fps: true,
        });
        assert!(matches!(
            wait(&c, |e| matches!(e, Event::MapFailed(..))),
            Event::MapFailed(..)
        ));
        c.send(Cmd::UploadMap {
            sig,
            name: "Plate".into(),
            bytes: bytes.clone(),
            fps: true,
        });
        wait(&c, |e| matches!(e, Event::MapUploaded(_)));
        c.send(Cmd::Maps);
        match wait(&c, |e| matches!(e, Event::Maps(_))) {
            Event::Maps(l) => {
                assert_eq!(l.len(), 1);
                assert_eq!(l[0].name, "Plate");
                assert!(l[0].stored);
            }
            _ => unreachable!(),
        }
        c.send(Cmd::DownloadMap(sig));
        match wait(&c, |e| matches!(e, Event::MapDownloaded { .. })) {
            Event::MapDownloaded { bytes: got, .. } => assert_eq!(got, bytes),
            _ => unreachable!(),
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_ban_locks_the_account_forever_and_the_machine_for_three_months() {
        let dir = std::env::temp_dir().join(format!("minelib-ban-{}", rand::random::<u32>()));
        // Tous les joueurs simulés sont sur cette machine.
        ONE_FPS_PER_MACHINE.store(false, std::sync::atomic::Ordering::Relaxed);
        let h = start(Config {
            dir: dir.clone(),
            bind: "127.0.0.1:0".into(),
            pow_bits: 4,
            name: "essai".into(),
        })
        .unwrap();
        std::fs::write(dir.join("admins.txt"), "Admin\n").unwrap();
        let addr = h.addr.to_string();
        let register = |pseudo: &str| {
            let c = Client::spawn(addr.clone(), Some(h.public));
            c.send(Cmd::Register {
                pseudo: pseudo.into(),
                password: "motdepasse".into(),
            });
            c
        };
        let tricheur = register("Tricheur");
        wait(&tricheur, |e| matches!(e, Event::LoggedIn { .. }));
        let admin = register("Admin");
        wait(&admin, |e| matches!(e, Event::LoggedIn { .. }));
        // Un joueur ordinaire ne peut pas bannir.
        tricheur.send(Cmd::Ban {
            pseudo: "Admin".into(),
            reason: "non".into(),
        });
        assert!(wait_error(&tricheur).contains("administrateurs"));
        admin.send(Cmd::Ban {
            pseudo: "tricheur".into(),
            reason: "aimbot".into(),
        });
        wait(&admin, |e| matches!(e, Event::BanDone(_)));
        assert!(
            wait_error(&tricheur).contains("Banni"),
            "coupé tout de suite"
        );
        // Reconnexion : compte banni.
        let again = Client::spawn(addr.clone(), Some(h.public));
        again.send(Cmd::Login {
            pseudo: "Tricheur".into(),
            password: "motdepasse".into(),
            totp: None,
            backup: None,
            device: None,
        });
        assert!(wait_error(&again).contains("vie"));
        // Nouveau compte sur la même machine : machine bannie.
        let other = register("Nouveau");
        let e = wait_error(&other);
        assert!(e.contains("machine"), "{e}");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Serveur de construction : le créateur est OP, nomme les OP, qui bannissent.
    #[test]
    fn build_server_ops_ban_and_the_creator_is_op_by_default() {
        let dir = std::env::temp_dir().join(format!("minelib-op-{}", rand::random::<u32>()));
        // Tous les joueurs simulés sont sur cette machine.
        ONE_FPS_PER_MACHINE.store(false, std::sync::atomic::Ordering::Relaxed);
        let h = start(Config {
            dir: dir.clone(),
            bind: "127.0.0.1:0".into(),
            pow_bits: 4,
            name: "essai".into(),
        })
        .unwrap();
        let addr = h.addr.to_string();
        let login = |pseudo: &str| {
            let c = Client::spawn(addr.clone(), Some(h.public));
            c.send(Cmd::Register {
                pseudo: pseudo.into(),
                password: "motdepasse".into(),
            });
            wait(&c, |e| matches!(e, Event::LoggedIn { .. }));
            c
        };
        let admin = |c: &Client, action: u8, pseudo: &str| {
            c.send(Cmd::ServerAdmin {
                action,
                pseudo: pseudo.into(),
                reason: "grief".into(),
            });
        };
        let ok = |c: &Client| match wait(c, |e| matches!(e, Event::ServerAdmin(_))) {
            Event::ServerAdmin(t) => t,
            _ => unreachable!(),
        };
        let creator = login("Createur");
        creator.send(Cmd::Host {
            kind: 1,
            name: "Mon serveur".into(),
            map: "monde".into(),
            map_sig: [3; 32],
            max_players: 0,
        });
        let room = match wait(&creator, |e| matches!(e, Event::Hosted(_))) {
            Event::Hosted(r) => r,
            _ => unreachable!(),
        };
        let bob = login("Bob");
        let eve = login("Eve");
        for c in [&bob, &eve] {
            c.send(Cmd::Join(room));
            wait(c, |e| matches!(e, Event::Joined { .. }));
        }
        // Bob n'est pas OP.
        admin(&bob, proto::SERVER_BAN, "Eve");
        assert!(wait_error(&bob).contains("OP"));
        admin(&creator, proto::SERVER_OP, "bob");
        assert!(ok(&creator).contains("OP"));
        // Seul le créateur nomme les OP ; personne ne bannit le créateur.
        admin(&bob, proto::SERVER_OP, "Eve");
        assert!(wait_error(&bob).contains("créateur"));
        admin(&bob, proto::SERVER_BAN, "Createur");
        assert!(wait_error(&bob).contains("créateur"));
        // Bob bannit Eve : exclue tout de suite, ne peut plus revenir.
        admin(&bob, proto::SERVER_BAN, "eve");
        ok(&bob);
        match wait(&eve, |e| matches!(e, Event::Kicked(_))) {
            Event::Kicked(r) => assert!(r.contains("Bob") && r.contains("grief"), "{r}"),
            _ => unreachable!(),
        }
        eve.send(Cmd::Join(room));
        assert!(wait_error(&eve).contains("banni"));
        admin(&creator, proto::SERVER_LIST, "");
        let list = ok(&creator);
        assert!(list.contains("bob") && list.contains("eve"), "{list}");
        // Débannie : elle revient.
        admin(&bob, proto::SERVER_UNBAN, "Eve");
        ok(&bob);
        eve.send(Cmd::Join(room));
        wait(&eve, |e| matches!(e, Event::Joined { .. }));
        // Mode de jeu : Eve (pas OP) ne peut pas en changer ; Bob (OP) la met en survie,
        // et fixe le mode par défaut, qu'Eve reçoit en revenant.
        let mode = |c: &Client, action: u8, who: &str, m: &str| {
            c.send(Cmd::ServerAdmin {
                action,
                pseudo: who.into(),
                reason: m.into(),
            });
        };
        mode(&eve, proto::SERVER_GAMEMODE, "", "creative");
        assert!(wait_error(&eve).contains("OP"));
        mode(&bob, proto::SERVER_GAMEMODE, "Eve", "survival");
        ok(&bob);
        match wait(&eve, |e| matches!(e, Event::SetGamemode { .. })) {
            Event::SetGamemode { mode, by } => {
                assert_eq!((mode.as_str(), by.as_str()), ("survival", "Bob"))
            }
            _ => unreachable!(),
        }
        mode(&eve, proto::SERVER_DEFAULT_GAMEMODE, "", "creative");
        assert!(wait_error(&eve).contains("OP"));
        mode(&bob, proto::SERVER_DEFAULT_GAMEMODE, "", "spectator");
        ok(&bob);
        mode(&eve, proto::SERVER_GAMEMODE_QUERY, "", "");
        match wait(&eve, |e| matches!(e, Event::SetGamemode { .. })) {
            Event::SetGamemode { mode, by } => {
                assert_eq!((mode.as_str(), by.as_str()), ("spectator", ""))
            }
            _ => unreachable!(),
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_account_moves_to_a_new_relay_with_its_kept_vault() {
        let mk = |n: &str| {
            let dir = std::env::temp_dir().join(format!("minelib-{n}-{}", rand::random::<u32>()));
            // Tous les joueurs simulés sont sur cette machine.
            ONE_FPS_PER_MACHINE.store(false, std::sync::atomic::Ordering::Relaxed);
            let h = start(Config {
                dir: dir.clone(),
                bind: "127.0.0.1:0".into(),
                pow_bits: 4,
                name: n.into(),
            })
            .unwrap();
            (dir, h)
        };
        let (d1, h1) = mk("ancien");
        let c = Client::spawn(h1.addr.to_string(), Some(h1.public));
        c.send(Cmd::Register {
            pseudo: "Nomade".into(),
            password: "motdepasse".into(),
        });
        let vault = match wait(&c, |e| matches!(e, Event::AccountBackup { .. })) {
            Event::AccountBackup { vault, .. } => vault,
            _ => unreachable!(),
        };
        let fp1 = match wait(&c, |e| matches!(e, Event::LoggedIn { .. })) {
            Event::LoggedIn { fingerprint, .. } => fingerprint,
            _ => unreachable!(),
        };
        // Nouveau relais : même compte (même clé) grâce au coffre gardé.
        let (d2, h2) = mk("nouveau");
        // Les deux relais se connaissent : un pseudo pris sur l'un l'est sur l'autre.
        let (a1, k1) = (h1.addr.to_string(), h1.public);
        h2.set_whois(Arc::new(move |p: &str| {
            minelib_net::client::whois(&a1, k1, p)
                .ok()
                .flatten()
                .into_iter()
                .collect()
        }));
        let squat = Client::spawn(h2.addr.to_string(), Some(h2.public));
        squat.send(Cmd::Register {
            pseudo: "NOMADE".into(),
            password: "autremotdepasse".into(),
        });
        let e = wait_error(&squat);
        assert!(e.to_lowercase().contains("pris"), "{e}");
        let c2 = Client::spawn(h2.addr.to_string(), Some(h2.public));
        c2.send(Cmd::Login {
            pseudo: "Nomade".into(),
            password: "mauvais!!".into(),
            totp: None,
            backup: Some(vault.clone()),
            device: None,
        });
        assert!(
            wait_error(&c2).contains("incorrect"),
            "mauvais mot de passe : rien"
        );
        c2.send(Cmd::Login {
            pseudo: "Nomade".into(),
            password: "motdepasse".into(),
            totp: None,
            backup: Some(vault),
            device: None,
        });
        match wait(&c2, |e| matches!(e, Event::LoggedIn { .. })) {
            Event::LoggedIn { fingerprint, .. } => assert_eq!(fingerprint, fp1),
            _ => unreachable!(),
        }
        let _ = std::fs::remove_dir_all(d1);
        let _ = std::fs::remove_dir_all(d2);
    }

    /// Un garant indépendant (au menu, 3 parties jouées, sans lien avec les joueurs) sert
    /// la partie ; un second la vérifie et la reprend quand l'hôte triche.
    /// Le relais mesure lui-même l'aller-retour de chaque joueur, et c'est le pire de sa
    /// mesure et de ce que le jeu annonce qui compte : un jeu modifié ne peut pas se dire
    /// mieux connecté qu'il ne l'est pour rester hôte ou garant.
    #[test]
    fn the_relay_measures_round_trips_itself() {
        let dir = std::env::temp_dir().join(format!("minelib-sonde-{}", rand::random::<u32>()));
        let h = start(Config {
            dir: dir.clone(),
            bind: "127.0.0.1:0".into(),
            pow_bits: 4,
            name: "essai".into(),
        })
        .unwrap();
        let c = Client::spawn(h.addr.to_string(), Some(h.public));
        c.send(Cmd::Register {
            pseudo: "Sonde".into(),
            password: "motdepasse".into(),
        });
        wait(&c, |e| matches!(e, Event::LoggedIn { .. }));
        let t = Instant::now();
        let seen = loop {
            c.send(Cmd::Servers(1));
            wait(&c, |e| matches!(e, Event::Servers(_)));
            let st = h.state.lock().unwrap();
            let me = st.conns.values().find(|c| c.pseudo.is_some()).unwrap();
            if let Some(r) = me.probe_rtt {
                break r;
            }
            drop(st);
            assert!(t.elapsed() < Duration::from_secs(20), "pas de mesure");
            std::thread::sleep(Duration::from_millis(100));
        };
        assert!(seen < 200.0, "mesure locale : {seen} ms");
        let mut st = h.state.lock().unwrap();
        let me = st.conns.values_mut().find(|c| c.pseudo.is_some()).unwrap();
        // Le jeu annonce 1 ms, le relais en a mesuré 300 : c'est 300 qui compte.
        me.rtt = Some(1.0);
        me.probe_rtt = Some(300.0);
        assert!(link_ms(me).unwrap() >= 300.0 && link_bad(me));
        // Et un jeu qui s'annonce lent l'est, même si le relais le mesure rapide.
        me.rtt = Some(400.0);
        me.probe_rtt = Some(5.0);
        assert!(link_bad(me));
        drop(st);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Relais fédérés : un joueur du relais A voit dans sa liste la partie ouverte sur le
    /// relais B, y passe avec le même compte (inscrit là-bas avec son coffre, même clé) et
    /// la rejoint. Un autre compte ne peut pas prendre son pseudo sur B.
    #[test]
    fn players_see_and_join_the_games_of_another_relay() {
        let open = |name: &str| {
            let dir = std::env::temp_dir().join(format!("minelib-fede-{}", rand::random::<u32>()));
            let h = start(Config {
                dir: dir.clone(),
                bind: "127.0.0.1:0".into(),
                pow_bits: 4,
                name: name.into(),
            })
            .unwrap();
            (h, dir)
        };
        let ((a, da), (b, db)) = (open("Relais A"), open("Relais B"));
        let (aaddr, akey) = (a.addr.to_string(), a.public);
        let (baddr, bkey) = (b.addr.to_string(), b.public);
        // Chacun connaît l'autre (dans le vrai jeu : par le DHT et un tunnel).
        let (ba, aa) = (baddr.clone(), aaddr.clone());
        a.set_peer_servers(Arc::new(move |kind| {
            minelib_net::client::peer_servers(&ba, bkey, kind)
                .unwrap_or_default()
                .into_iter()
                .map(|mut e| {
                    e.relay_key = bkey;
                    e.relay_addr = ba.clone();
                    e
                })
                .collect()
        }));
        b.set_whois(Arc::new(move |p: &str| {
            minelib_net::client::whois(&aa, akey, p)
                .ok()
                .flatten()
                .into_iter()
                .collect()
        }));
        // Un monde ouvert sur B.
        let host = Client::spawn(baddr.clone(), Some(bkey));
        host.send(Cmd::Register {
            pseudo: "Hote".into(),
            password: "motdepasse".into(),
        });
        wait(&host, |e| matches!(e, Event::LoggedIn { .. }));
        host.send(Cmd::Host {
            kind: 1,
            name: "Monde de B".into(),
            map: "monde".into(),
            map_sig: [5; 32],
            max_players: 0,
        });
        let room = match wait(&host, |e| matches!(e, Event::Hosted(_))) {
            Event::Hosted(r) => r,
            _ => unreachable!(),
        };
        // Un joueur de A le voit dans sa liste, marqué comme étant sur B.
        let p = Client::spawn(aaddr.clone(), Some(akey));
        p.send(Cmd::Register {
            pseudo: "Voyageur".into(),
            password: "motdepasse".into(),
        });
        let vault = match wait(&p, |e| matches!(e, Event::AccountBackup { .. })) {
            Event::AccountBackup { vault, .. } => vault,
            _ => unreachable!(),
        };
        wait(&p, |e| matches!(e, Event::LoggedIn { .. }));
        p.send(Cmd::Servers(1));
        let list = match wait(&p, |e| matches!(e, Event::Servers(_))) {
            Event::Servers(l) => l,
            _ => unreachable!(),
        };
        let e = list
            .iter()
            .find(|e| e.name == "Monde de B")
            .expect("la partie de B est dans la liste de A");
        assert!(e.elsewhere() && e.relay_key == bkey && e.id == room);
        assert_eq!(e.region, "Relais B");
        // Un imposteur ne peut pas prendre le pseudo du joueur sur B.
        let thief = Client::spawn(baddr.clone(), Some(bkey));
        thief.send(Cmd::Register {
            pseudo: "voyageur".into(),
            password: "autremotdepasse".into(),
        });
        assert!(wait_error(&thief).contains("autre relais"));
        // Le joueur passe sur B avec son compte (coffre gardé sur son PC) et rejoint.
        let q = Client::spawn(e.relay_addr.clone(), Some(e.relay_key));
        q.send(Cmd::Login {
            pseudo: "Voyageur".into(),
            password: "motdepasse".into(),
            totp: None,
            backup: Some(vault),
            device: None,
        });
        wait(&q, |e| matches!(e, Event::LoggedIn { .. }));
        q.send(Cmd::Join(room));
        wait(&q, |e| matches!(e, Event::Joined { .. }));
        match wait(&host, |e| matches!(e, Event::PeerJoined { .. })) {
            Event::PeerJoined { pseudo, .. } => assert_eq!(pseudo, "Voyageur"),
            _ => unreachable!(),
        }
        // La liste de B ne renvoie pas à A ses propres parties comme étant « ailleurs ».
        let own = minelib_net::client::peer_servers(&baddr, bkey, 1).unwrap();
        assert!(own.iter().all(|e| !e.elsewhere()));
        for d in [da, db] {
            let _ = std::fs::remove_dir_all(d);
        }
    }

    /// Serveur en survie : le relais tient l'inventaire des joueurs. Un jeu modifié ne
    /// peut pas lâcher un objet qu'il n'a jamais eu ; ce qu'on ramasse, on peut le lâcher
    /// (une fois) ; le créateur (OP, en créatif) n'est pas compté.
    #[test]
    fn a_survival_player_cannot_drop_what_the_relay_never_saw_him_get() {
        use minelib_net::items::Pile;
        let dir = std::env::temp_dir().join(format!("minelib-survie-{}", rand::random::<u32>()));
        let h = start(Config {
            dir: dir.clone(),
            bind: "127.0.0.1:0".into(),
            pow_bits: 4,
            name: "essai".into(),
        })
        .unwrap();
        let addr = h.addr.to_string();
        let login = |pseudo: &str| {
            let c = Client::spawn(addr.clone(), Some(h.public));
            c.send(Cmd::Register {
                pseudo: pseudo.into(),
                password: "motdepasse".into(),
            });
            let link = match wait(&c, |e| matches!(e, Event::Link(_))) {
                Event::Link(l) => l,
                _ => unreachable!(),
            };
            wait(&c, |e| matches!(e, Event::LoggedIn { .. }));
            (c, link)
        };
        let (alice, alink) = login("Alice");
        alice.send(Cmd::Host {
            kind: 1,
            name: "Survie".into(),
            map: "survie".into(),
            map_sig: [3; 32],
            max_players: 0,
        });
        let room = match wait(&alice, |e| matches!(e, Event::Hosted(_))) {
            Event::Hosted(r) => r,
            _ => unreachable!(),
        };
        alice.send(Cmd::ServerAdmin {
            action: proto::SERVER_DEFAULT_GAMEMODE,
            pseudo: String::new(),
            reason: "survival".into(),
        });
        wait(&alice, |e| matches!(e, Event::ServerAdmin(_)));
        let (eve, elink) = login("Eve");
        eve.send(Cmd::Join(room));
        wait(&eve, |e| matches!(e, Event::Joined { .. }));
        for l in [&alink, &elink] {
            transport::push(l, &Request::Position { chunk: [0, 0] });
        }
        let ask = |l: &std::sync::mpsc::Sender<crate::Inbound>, m: game::BuildMsg| {
            transport::push(
                l,
                &Request::ToZone {
                    chunk: [0, 0],
                    data: game::encode(&m),
                },
            );
        };
        let next = |c: &Client| loop {
            if let Event::FromZone { data, .. } = wait(c, |e| matches!(e, Event::FromZone { .. })) {
                match game::decode::<game::BuildMsg>(&data) {
                    Some(
                        m @ (game::BuildMsg::Item { .. }
                        | game::BuildMsg::Slot { .. }
                        | game::BuildMsg::Denied { .. }),
                    ) => return m,
                    _ => continue,
                }
            }
        };
        let drop = |seq: u32, id: u64, count: u8| game::BuildMsg::DropItem {
            seq,
            id,
            pos: [1.5, 64.0, 1.5],
            pile: Pile { item: 7, count },
        };
        // Eve (en survie) prétend lâcher 64 objets qu'elle n'a jamais eus : refusé.
        ask(&elink, drop(1, 900, 64));
        assert_eq!(next(&eve), game::BuildMsg::Denied { seq: 1 });
        // Alice (créatrice, en créatif) en lâche 5 : accepté, les deux le voient.
        ask(&alink, drop(1, 100, 5));
        for c in [&alice, &eve] {
            assert!(matches!(
                next(c),
                game::BuildMsg::Item {
                    id: 100,
                    drop: Some(_),
                    ..
                }
            ));
        }
        // Eve les ramasse : ils sont dans son sac, elle peut en lâcher 5, pas 6.
        ask(&elink, game::BuildMsg::TakeItem { seq: 2, id: 100 });
        assert!(matches!(
            next(&eve),
            game::BuildMsg::Item {
                id: 100,
                drop: None,
                ..
            }
        ));
        ask(&elink, drop(3, 901, 6));
        assert_eq!(next(&eve), game::BuildMsg::Denied { seq: 3 });
        let lay = drop(4, 902, 5);
        ask(&elink, lay.clone());
        assert!(matches!(
            next(&eve),
            game::BuildMsg::Item {
                id: 902,
                drop: Some(_),
                ..
            }
        ));
        // La même demande reçue deux fois : ni refus (elle a été faite), ni second objet.
        ask(&elink, lay);
        ask(&elink, drop(5, 903, 1));
        assert_eq!(next(&eve), game::BuildMsg::Denied { seq: 5 });
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Objets au sol et coffres sur un vrai relais : deux joueurs qui prennent la même
    /// chose en même temps, un coffre cassé, une demande rejouée, la sauvegarde du
    /// propriétaire remise deux fois : jamais un objet de plus qu'au départ.
    #[test]
    fn items_and_chests_are_arbitrated_by_the_relay_without_duplication() {
        use minelib_net::items::{Ledger, Pile};
        let dir = std::env::temp_dir().join(format!("minelib-objets-{}", rand::random::<u32>()));
        ONE_FPS_PER_MACHINE.store(false, std::sync::atomic::Ordering::Relaxed);
        let h = start(Config {
            dir: dir.clone(),
            bind: "127.0.0.1:0".into(),
            pow_bits: 4,
            name: "essai".into(),
        })
        .unwrap();
        let addr = h.addr.to_string();
        let login = |pseudo: &str| {
            let c = Client::spawn(addr.clone(), Some(h.public));
            c.send(Cmd::Register {
                pseudo: pseudo.into(),
                password: "motdepasse".into(),
            });
            let link = match wait(&c, |e| matches!(e, Event::Link(_))) {
                Event::Link(l) => l,
                _ => unreachable!(),
            };
            wait(&c, |e| matches!(e, Event::LoggedIn { .. }));
            (c, link)
        };
        let (alice, alink) = login("Alice");
        alice.send(Cmd::Host {
            kind: 1,
            name: "Monde".into(),
            map: "monde".into(),
            map_sig: [9; 32],
            max_players: 0,
        });
        let room = match wait(&alice, |e| matches!(e, Event::Hosted(_))) {
            Event::Hosted(r) => r,
            _ => unreachable!(),
        };
        let (bob, blink) = login("Bob");
        bob.send(Cmd::Join(room));
        wait(&bob, |e| matches!(e, Event::Joined { .. }));
        for l in [&alink, &blink] {
            transport::push(l, &Request::Position { chunk: [0, 0] });
        }
        let ask = |l: &std::sync::mpsc::Sender<crate::Inbound>, m: &game::BuildMsg| {
            transport::push(
                l,
                &Request::ToZone {
                    chunk: [0, 0],
                    data: game::encode(m),
                },
            );
        };
        // Prochain état d'objet (ou refus) reçu par un joueur.
        let next = |c: &Client| loop {
            if let Event::FromZone { data, .. } = wait(c, |e| matches!(e, Event::FromZone { .. })) {
                match game::decode::<game::BuildMsg>(&data) {
                    Some(
                        m @ (game::BuildMsg::Item { .. }
                        | game::BuildMsg::Slot { .. }
                        | game::BuildMsg::Denied { .. }),
                    ) => return m,
                    _ => continue,
                }
            }
        };
        let (mut la, mut lb) = (Ledger::starting_at(100), Ledger::starting_at(5000));
        let (mut bag_a, mut bag_b) = (0u32, 0u32);
        let give = |l: &mut Ledger, bag: &mut u32, m: &game::BuildMsg, me: &str| {
            if let Some(minelib_net::items::Credit::Give(p)) = l.on_message(m, me) {
                *bag += p.count as u32;
            }
        };
        let pile = |n: u8| Pile { item: 1, count: n };
        // Alice lâche 10 pierres ; la demande part deux fois (réseau) : un seul objet.
        let req = la.drop_item(42, [1.5, 64.0, 1.5], pile(10));
        ask(&alink, &req);
        ask(&alink, &req);
        for (c, l, bag, me) in [
            (&alice, &mut la, &mut bag_a, "Alice"),
            (&bob, &mut lb, &mut bag_b, "Bob"),
        ] {
            let m = next(c);
            assert!(
                matches!(
                    m,
                    game::BuildMsg::Item {
                        id: 42,
                        drop: Some(_),
                        ..
                    }
                ),
                "{m:?}"
            );
            give(l, bag, &m, me);
        }
        // Les deux le ramassent en même temps : un seul l'a, l'autre est refusé.
        ask(&alink, &la.take_item(42));
        ask(&blink, &lb.take_item(42));
        // Chacun voit l'objet disparaître ; le gagnant le reçoit, le perdant a en plus
        // son refus (dans un ordre ou l'autre).
        let m = next(&alice);
        give(&mut la, &mut bag_a, &m, "Alice");
        let m = next(&bob);
        give(&mut lb, &mut bag_b, &m, "Bob");
        assert_eq!(bag_a + bag_b, 10, "ramassé une seule fois");
        if bag_a == 0 {
            let m = next(&alice);
            give(&mut la, &mut bag_a, &m, "Alice");
        } else {
            let m = next(&bob);
            give(&mut lb, &mut bag_b, &m, "Bob");
        }
        assert_eq!(bag_a + bag_b, 10, "ramassé une seule fois");
        assert_eq!(la.pending() + lb.pending(), 0);
        // Coffre : Bob y pose 7 objets, Alice et Bob prennent tout en même temps.
        let chest = [2, 64, 2];
        ask(&blink, &lb.chest_put(chest, 0, pile(7)));
        for c in [&alice, &bob] {
            let m = next(c);
            assert!(
                matches!(m, game::BuildMsg::Slot { pile: Some(_), .. }),
                "{m:?}"
            );
            give(&mut lb, &mut bag_b, &m, "Bob");
        }
        let before = bag_a + bag_b;
        ask(&alink, &la.chest_take(chest, 0, 7));
        ask(&blink, &lb.chest_take(chest, 0, 7));
        let (was_a, was_b) = (bag_a, bag_b);
        let m = next(&alice);
        give(&mut la, &mut bag_a, &m, "Alice");
        let m = next(&bob);
        give(&mut lb, &mut bag_b, &m, "Bob");
        if bag_a == was_a {
            let m = next(&alice);
            give(&mut la, &mut bag_a, &m, "Alice");
        } else {
            let m = next(&bob);
            give(&mut lb, &mut bag_b, &m, "Bob");
        }
        assert_eq!(
            bag_a + bag_b,
            before + 7,
            "le coffre n'a été vidé qu'une fois"
        );
        assert!(bag_a == was_a || bag_b == was_b);
        assert_eq!(la.pending() + lb.pending(), 0);
        // Coffre garni puis cassé (son bloc change) : le contenu tombe au sol, une fois.
        ask(&alink, &la.chest_put(chest, 5, pile(3)));
        for c in [&alice, &bob] {
            next(c);
        }
        {
            let mut st = h.state.lock().unwrap();
            spill_chest(&mut st, room, chest);
            spill_chest(&mut st, room, chest);
            let items = &st.rooms[&room].build.items;
            assert!(items.chests.is_empty());
            assert_eq!(items.totals()[&1], 3);
            assert_eq!(items.live_drops().count(), 1);
        }
        // Bob voit le coffre se vider puis l'objet apparaître, et le ramasse.
        let mut id = None;
        while id.is_none() {
            if let game::BuildMsg::Item {
                id: i,
                drop: Some(_),
                ..
            } = next(&bob)
            {
                id = Some(i);
            }
        }
        ask(&blink, &lb.take_item(id.unwrap()));
        let before = bag_b;
        while bag_b == before {
            let m = next(&bob);
            give(&mut lb, &mut bag_b, &m, "Bob");
        }
        assert_eq!(bag_b, before + 3);
        // Loin de soi (plus de 2 chunks) : refusé, rien ne change.
        for far in [
            la.drop_item(4242, [200.5, 64.0, 1.5], pile(1)),
            la.chest_put([200, 64, 2], 0, pile(1)),
        ] {
            ask(&alink, &far);
            // (Après les états déjà en route pour elle.)
            while !matches!(next(&alice), game::BuildMsg::Denied { .. }) {}
        }
        // Le propriétaire remet sa sauvegarde : un objet inconnu revient, l'objet déjà
        // ramassé et le coffre déjà touché ne reviennent pas.
        let saved = [
            game::BuildMsg::Item {
                id: 42,
                at: [1.5, 64.0, 1.5],
                drop: Some(pile(10)),
                to: String::new(),
                seq: 0,
                got: None,
            },
            game::BuildMsg::Slot {
                pos: chest,
                slot: 0,
                pile: Some(pile(64)),
                by: String::new(),
                seq: 0,
                got: None,
            },
            game::BuildMsg::Item {
                id: 77,
                at: [1.5, 64.0, 1.5],
                drop: Some(pile(2)),
                to: String::new(),
                seq: 0,
                got: None,
            },
        ];
        for m in &saved {
            ask(&alink, m);
            // Un autre joueur ne peut rien remettre du tout.
            ask(&blink, m);
        }
        match next(&bob) {
            game::BuildMsg::Item {
                id: 77,
                drop: Some(p),
                ..
            } => assert_eq!(p, pile(2)),
            m => panic!("{m:?}"),
        }
        let st = h.state.lock().unwrap();
        let items = &st.rooms[&room].build.items;
        assert_eq!(items.totals()[&1], 2);
        assert!(items.chests.is_empty());
        // Tout compte fait : 10 + 7 + 3 posés par les joueurs, 20 dans leurs sacs.
        assert_eq!(bag_a + bag_b, 20);
    }

    /// Seul le relais du monde écrit l'histoire des blocs : il accepte une demande, lui
    /// donne son rang et son auteur (le pseudo de la session), et la publie. Ce qu'un jeu
    /// modifié fabrique (un « Set », le compte rendu d'un relais de région) est sans effet ;
    /// une demande mal formée, hors de portée ou en rafale est refusée à son auteur.
    /// Ce qu'un joueur simulé reçoit du relais au sujet des blocs.
    enum Got {
        Block([i32; 3], u16, u64),
        Reject([i32; 3]),
    }

    /// Prochain bloc changé, ou refus, reçu par ce joueur (un paquet de blocs en donne
    /// plusieurs : `queue` garde les suivants).
    fn next_block(c: &Client, queue: &mut VecDeque<Got>) -> Got {
        loop {
            if let Some(g) = queue.pop_front() {
                return g;
            }
            match wait(c, |e| matches!(e, Event::FromZone { .. })) {
                Event::FromZone { peer, data, .. } => {
                    assert_eq!(peer, 0, "publié par le relais");
                    match game::decode::<game::BuildMsg>(&data).expect("message") {
                        game::BuildMsg::Cells(p) => queue.extend(
                            minelib_net::cells::unpack_cells(&p)
                                .expect("paquet lisible")
                                .into_iter()
                                .map(|(pos, block, seq)| Got::Block(pos, block, seq)),
                        ),
                        game::BuildMsg::Reject { pos, .. } => queue.push_back(Got::Reject(pos)),
                        m => panic!("{m:?}"),
                    }
                }
                _ => unreachable!(),
            }
        }
    }

    #[test]
    fn only_the_world_relay_writes_the_history_of_blocks() {
        let dir = std::env::temp_dir().join(format!("minelib-notaire-{}", rand::random::<u32>()));
        // Tous les joueurs simulés sont sur cette machine.
        ONE_FPS_PER_MACHINE.store(false, std::sync::atomic::Ordering::Relaxed);
        let h = start(Config {
            dir: dir.clone(),
            bind: "127.0.0.1:0".into(),
            pow_bits: 4,
            name: "essai".into(),
        })
        .unwrap();
        let addr = h.addr.to_string();
        let login = |pseudo: &str| {
            let c = Client::spawn(addr.clone(), Some(h.public));
            c.send(Cmd::Register {
                pseudo: pseudo.into(),
                password: "motdepasse".into(),
            });
            let link = match wait(&c, |e| matches!(e, Event::Link(_))) {
                Event::Link(l) => l,
                _ => unreachable!(),
            };
            wait(&c, |e| matches!(e, Event::LoggedIn { .. }));
            (c, link)
        };
        let (createur, clink) = login("Createur");
        createur.send(Cmd::Host {
            kind: 1,
            name: "Monde".into(),
            map: "monde".into(),
            map_sig: [4; 32],
            max_players: 0,
        });
        let room = match wait(&createur, |e| matches!(e, Event::Hosted(_))) {
            Event::Hosted(r) => r,
            _ => unreachable!(),
        };
        let (eve, elink) = login("Eve");
        eve.send(Cmd::Join(room));
        wait(&eve, |e| matches!(e, Event::Joined { .. }));
        transport::push(&clink, &Request::Position { chunk: [0, 0] });
        transport::push(&elink, &Request::Position { chunk: [0, 0] });
        let ask = |link: &Sender<Inbound>, chunk: [i32; 2], msg: &game::BuildMsg| {
            transport::push(
                link,
                &Request::ToZone {
                    chunk,
                    data: game::encode(msg),
                },
            );
        };
        let edit = |pos: [i32; 3], block: u16| game::BuildMsg::Edit {
            pos,
            block,
            fluid: false,
        };
        // Auteur de la dernière modification d'une case, d'après l'historique du relais.
        let author = |pos: [i32; 3]| {
            let mut st = h.state.lock().unwrap();
            st.rooms.get_mut(&room).unwrap().build.author_at(pos)
        };
        let (mut eq, mut cq) = (VecDeque::new(), VecDeque::new());
        let block = |g: Got| match g {
            Got::Block(pos, block, seq) => (pos, block, seq),
            Got::Reject(pos) => panic!("refusé en {pos:?}"),
        };
        // Eve pose un bloc : elle reçoit sa confirmation, le créateur la modification, la
        // même, et l'historique la met au nom de sa session.
        ask(&elink, [0, 0], &edit([1, 64, 1], 1));
        let (pos, b, first) = block(next_block(&eve, &mut eq));
        assert_eq!((pos, b), ([1, 64, 1], 1));
        assert_eq!(author(pos).as_deref(), Some("Eve"));
        assert_eq!(
            block(next_block(&createur, &mut cq)),
            ([1, 64, 1], 1, first)
        );
        // Un jeu modifié fabrique une modification au nom d'un autre, avec un rang énorme,
        // puis un lot d'hôte principal de région souveraine (sans l'être) : personne ne
        // reçoit rien, et la modification suivante (une vraie) porte le rang suivant.
        let forged = game::BuildMsg::Set {
            pos: [2, 64, 1],
            block: 9,
            by: "Createur".into(),
            seq: u64::MAX,
        };
        ask(&elink, [0, 0], &forged);
        transport::push(
            &elink,
            &Request::Archive {
                region: [0, 0],
                batches: vec![sovereign::Batch {
                    room,
                    region: [0, 0],
                    epoch: 1,
                    first: 0,
                    prev: [0; 32],
                    records: Vec::new(),
                    certs: Vec::new(),
                    sig: Vec::new(),
                }],
            },
        );
        ask(&elink, [0, 0], &game::BuildMsg::Reject { pos, peer: 1 });
        ask(&clink, [0, 0], &edit([3, 64, 1], 2));
        let (pos, b, second) = block(next_block(&eve, &mut eq));
        assert_eq!((pos, b), ([3, 64, 1], 2));
        assert_eq!(author(pos).as_deref(), Some("Createur"));
        assert_eq!(
            author([2, 64, 1]),
            None,
            "la fausse n'est pas dans l'historique"
        );
        assert_eq!(second, first + 1, "rien ne s'est glissé entre les deux");
        assert_eq!(block(next_block(&createur, &mut cq)).2, second);
        // Mal formée (la case n'est pas dans le chunk annoncé, hauteur hors du monde) ou
        // hors de portée de son auteur : refusée à elle seule.
        let mut refused = |pos: [i32; 3]| match next_block(&eve, &mut eq) {
            Got::Reject(p) => assert_eq!(p, pos),
            Got::Block(p, ..) => panic!("accepté en {p:?}"),
        };
        ask(&elink, [5, 5], &edit([4, 64, 1], 1));
        refused([4, 64, 1]);
        ask(&elink, [0, 0], &edit([4, 300, 1], 1));
        refused([4, 300, 1]);
        ask(&elink, [40, 0], &edit([640, 64, 1], 1));
        refused([640, 64, 1]);
        // En rafale : 20 par seconde au plus ; l'écoulement d'un fluide a son budget à part.
        std::thread::sleep(Duration::from_millis(1100));
        for i in 0..30 {
            ask(&elink, [0, 0], &edit([i % 16, 70, 2], 1));
        }
        let mut done = (0, 0);
        while done.0 + done.1 < 30 {
            match next_block(&eve, &mut eq) {
                Got::Block(..) => done.0 += 1,
                Got::Reject(_) => done.1 += 1,
            }
        }
        assert_eq!(done, (EDIT_RATE, 30 - EDIT_RATE), "20 par seconde au plus");
        for i in 0..30 {
            let flow = game::BuildMsg::Edit {
                pos: [i % 16, 71, 2],
                block: 1,
                fluid: true,
            };
            ask(&elink, [0, 0], &flow);
        }
        for _ in 0..30 {
            assert!(matches!(next_block(&eve, &mut eq), Got::Block(..)));
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    /// WorldEdit en ligne : réservé aux OP ; leurs blocs vont aux joueurs proches.
    #[test]
    fn only_ops_send_bulk_edits_and_players_nearby_get_them() {
        let dir = std::env::temp_dir().join(format!("minelib-bulk-{}", rand::random::<u32>()));
        // Tous les joueurs simulés sont sur cette machine.
        ONE_FPS_PER_MACHINE.store(false, std::sync::atomic::Ordering::Relaxed);
        let h = start(Config {
            dir: dir.clone(),
            bind: "127.0.0.1:0".into(),
            pow_bits: 4,
            name: "essai".into(),
        })
        .unwrap();
        let addr = h.addr.to_string();
        let login = |pseudo: &str| {
            let c = Client::spawn(addr.clone(), Some(h.public));
            c.send(Cmd::Register {
                pseudo: pseudo.into(),
                password: "motdepasse".into(),
            });
            let link = match wait(&c, |e| matches!(e, Event::Link(_))) {
                Event::Link(l) => l,
                _ => unreachable!(),
            };
            wait(&c, |e| matches!(e, Event::LoggedIn { .. }));
            (c, link)
        };
        let (createur, clink) = login("Createur");
        createur.send(Cmd::Host {
            kind: 1,
            name: "Monde".into(),
            map: "monde".into(),
            map_sig: [5; 32],
            max_players: 0,
        });
        let room = match wait(&createur, |e| matches!(e, Event::Hosted(_))) {
            Event::Hosted(r) => r,
            _ => unreachable!(),
        };
        let (eve, elink) = login("Eve");
        eve.send(Cmd::Join(room));
        wait(&eve, |e| matches!(e, Event::Joined { .. }));
        transport::push(&elink, &Request::Position { chunk: [0, 0] });
        let cells = vec![([1, 70, 2], 3u16), ([2, 70, 2], 3)];
        // Eve n'est pas OP : refusé.
        transport::push(
            &elink,
            &Request::BulkEdit {
                id: 1,
                cells: cells.clone(),
            },
        );
        match wait(&eve, |e| matches!(e, Event::BulkDone { .. })) {
            Event::BulkDone { id, error } => assert!(id == 1 && error.contains("OP"), "{error}"),
            _ => unreachable!(),
        }
        // Le créateur (OP) : publié, Eve (tout près) reçoit les blocs.
        transport::push(&clink, &Request::BulkEdit { id: 7, cells });
        match wait(&createur, |e| matches!(e, Event::BulkDone { .. })) {
            Event::BulkDone { id, error } => assert!(id == 7 && error.is_empty(), "{error}"),
            _ => unreachable!(),
        }
        let mut eq = VecDeque::new();
        match next_block(&eve, &mut eq) {
            Got::Block(pos, block, _) => assert_eq!((pos, block), ([1, 70, 2], 3)),
            Got::Reject(pos) => panic!("refusé en {pos:?}"),
        }
        assert_eq!(
            h.state
                .lock()
                .unwrap()
                .rooms
                .get_mut(&room)
                .unwrap()
                .build
                .author_at([1, 70, 2])
                .as_deref(),
            Some("Createur")
        );
        // Voix par pseudo (v137, hors des régions servies par un relais de région) : une
        // fois à chacun, même nommé deux fois ; ni à soi-même, ni à un inconnu.
        transport::push(
            &clink,
            &Request::VoiceTo {
                to: vec![
                    "eve".into(),
                    "EVE".into(),
                    "Inconnu".into(),
                    "Createur".into(),
                ],
                data: vec![1, 2, 3],
            },
        );
        match wait(&eve, |e| matches!(e, Event::VoiceFrom { .. })) {
            Event::VoiceFrom { from, data } => {
                assert_eq!((from.as_str(), data), ("Createur", vec![1, 2, 3]))
            }
            _ => unreachable!(),
        }
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(h.voice_relayed(), 1);
        // Position d'Eve gardée par le relais : elle la retrouve en revenant.
        transport::push(
            &elink,
            &Request::Nearby {
                chunk: [0, 0],
                data: game::encode(&game::BuildMsg::Player {
                    pseudo: "Eve".into(),
                    pos: [3.5, 80.0, 4.5],
                    yaw: 1.0,
                    pitch: -0.2,
                    crouch: 0.0,
                    speed: 0.0,
                    ping: 0,
                }),
            },
        );
        eve.send(Cmd::Leave);
        std::thread::sleep(Duration::from_millis(200));
        eve.send(Cmd::Join(room));
        wait(&eve, |e| matches!(e, Event::Joined { .. }));
        eve.send(Cmd::ServerAdmin {
            action: proto::SERVER_GAMEMODE_QUERY,
            pseudo: String::new(),
            reason: String::new(),
        });
        match wait(&eve, |e| matches!(e, Event::BuildSpawn { .. })) {
            Event::BuildSpawn { pos, yaw, pitch } => {
                assert_eq!((pos, yaw, pitch), ([3.5, 80.0, 4.5], 1.0, -0.2))
            }
            _ => unreachable!(),
        }
        // Servie par un relais de région, ses positions n'arrivent plus ici : son jeu redit
        // de temps en temps où elle est, et c'est là qu'elle revient.
        transport::push(
            &elink,
            &Request::Spot {
                pos: [40.5, 90.0, -7.5],
                yaw: 2.0,
                pitch: 0.1,
            },
        );
        eve.send(Cmd::Leave);
        std::thread::sleep(Duration::from_millis(200));
        eve.send(Cmd::Join(room));
        wait(&eve, |e| matches!(e, Event::Joined { .. }));
        eve.send(Cmd::ServerAdmin {
            action: proto::SERVER_GAMEMODE_QUERY,
            pseudo: String::new(),
            reason: String::new(),
        });
        match wait(&eve, |e| matches!(e, Event::BuildSpawn { .. })) {
            Event::BuildSpawn { pos, yaw, pitch } => {
                assert_eq!((pos, yaw, pitch), ([40.5, 90.0, -7.5], 2.0, 0.1))
            }
            _ => unreachable!(),
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_player_whose_game_just_started_is_the_preferred_garant() {
        let dir =
            std::env::temp_dir().join(format!("minelib-garant-jeu-{}", rand::random::<u32>()));
        {
            let mut st = stats::Store::open(dir.join("stats")).unwrap();
            for g in ["Menu", "Menu2", "Actif"] {
                st.get_mut(g).games = 5;
                st.save(g);
            }
        }
        // Tous les joueurs simulés sont sur cette machine.
        ONE_FPS_PER_MACHINE.store(false, std::sync::atomic::Ordering::Relaxed);
        let h = start(Config {
            dir: dir.clone(),
            bind: "127.0.0.1:0".into(),
            pow_bits: 4,
            name: "essai".into(),
        })
        .unwrap();
        let addr = h.addr.to_string();
        let login = |pseudo: &str| {
            let c = Client::spawn(addr.clone(), Some(h.public));
            c.send(Cmd::Register {
                pseudo: pseudo.into(),
                password: "motdepasse".into(),
            });
            wait(&c, |e| matches!(e, Event::LoggedIn { .. }));
            c
        };
        let search = |c: &Client, sig: u8| {
            c.send(Cmd::Search {
                map_sig: [sig; 32],
                map: "carte".into(),
                players: 10,
            });
            wait(c, |e| matches!(e, Event::Joined { .. } | Event::Hosted(_)));
        };
        // « Actif » lance sa partie, servie par « Menu » (au menu) ; elle commence.
        let menu = login("Menu");
        menu.send(Cmd::GarantAvailable(true));
        let actif = login("Actif");
        std::thread::sleep(Duration::from_millis(200));
        search(&actif, 1);
        wait(&menu, |e| matches!(e, Event::GarantTask { .. }));
        menu.send(Cmd::RoomStarted(true));
        // Sa connexion de garant (même compte), et un autre joueur au menu.
        let g = Client::spawn_garant(addr.clone(), h.public);
        g.send(Cmd::Login {
            pseudo: "Actif".into(),
            password: "motdepasse".into(),
            totp: None,
            backup: None,
            device: None,
        });
        wait(&g, |e| matches!(e, Event::LoggedIn { .. }));
        g.send(Cmd::GarantAvailable(true));
        let menu2 = login("Menu2");
        menu2.send(Cmd::GarantAvailable(true));
        std::thread::sleep(Duration::from_millis(300));
        // Une autre partie : servie par le joueur qui vient de lancer la sienne.
        let autre = login("Autre");
        search(&autre, 2);
        match wait(&g, |e| matches!(e, Event::GarantTask { .. })) {
            Event::GarantTask { standby, .. } => assert!(!standby),
            _ => unreachable!(),
        }
        // (« Menu2 » peut être garant en attente d'une partie, pas son serveur.)
        while let Some(e) = menu2.poll() {
            assert!(!matches!(e, Event::GarantTask { standby: false, .. }));
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn independent_garants_serve_check_and_replace_a_cheating_host() {
        let dir = std::env::temp_dir().join(format!("minelib-garants-{}", rand::random::<u32>()));
        {
            let mut st = stats::Store::open(dir.join("stats")).unwrap();
            for g in ["Garant", "Verif"] {
                st.get_mut(g).games = 5;
                st.save(g);
            }
        }
        // Tous les joueurs simulés sont sur cette machine.
        ONE_FPS_PER_MACHINE.store(false, std::sync::atomic::Ordering::Relaxed);
        let h = start(Config {
            dir: dir.clone(),
            bind: "127.0.0.1:0".into(),
            pow_bits: 4,
            name: "essai".into(),
        })
        .unwrap();
        let addr = h.addr.to_string();
        let login = |pseudo: &str| {
            let c = Client::spawn(addr.clone(), Some(h.public));
            c.send(Cmd::Register {
                pseudo: pseudo.into(),
                password: "motdepasse".into(),
            });
            wait(&c, |e| matches!(e, Event::LoggedIn { .. }));
            c
        };
        let g = login("Garant");
        g.send(Cmd::GarantAvailable(true));
        let novice = login("Novice");
        novice.send(Cmd::GarantAvailable(true));
        let p = Client::spawn(addr.clone(), Some(h.public));
        p.send(Cmd::Register {
            pseudo: "Joueur".into(),
            password: "motdepasse".into(),
        });
        let plink = match wait(&p, |e| matches!(e, Event::Link(_))) {
            Event::Link(l) => l,
            _ => unreachable!(),
        };
        wait(&p, |e| matches!(e, Event::LoggedIn { .. }));
        std::thread::sleep(Duration::from_millis(200));
        p.send(Cmd::Search {
            map_sig: [9; 32],
            map: "dust".into(),
            players: 10,
        });
        match wait(&p, |e| matches!(e, Event::Joined { .. } | Event::Hosted(_))) {
            // Servie par « Garant » (pas « Novice », 0 partie) : il reçoit la tâche.
            Event::Joined { .. } => {}
            e => panic!("servie par un garant : {e:?}"),
        }
        match wait(&g, |e| matches!(e, Event::GarantTask { .. })) {
            Event::GarantTask { standby, .. } => assert!(!standby),
            _ => unreachable!(),
        }
        // Un second garant arrive : mis en attente (vérificateur), désigné à l'hôte.
        let v = login("Verif");
        v.send(Cmd::GarantAvailable(true));
        std::thread::sleep(Duration::from_millis(2100));
        p.send(Cmd::Maps);
        match wait(&v, |e| matches!(e, Event::GarantTask { .. })) {
            Event::GarantTask { standby, .. } => assert!(standby),
            _ => unreachable!(),
        }
        wait(&g, |e| matches!(e, Event::BackupIs(_)));
        // Le vérificateur reçoit une copie attestée des tirs du joueur (pas du tchat).
        let to_host = |m: &game::ToHost| {
            transport::push(
                &plink,
                &Request::ToHost {
                    data: game::encode(m),
                },
            );
        };
        to_host(&game::ToHost::Chat("salut".into()));
        let fire = game::ToHost::Fire {
            weapon: 1,
            origin: [0.0; 3],
            dirs: vec![[1.0, 0.0, 0.0]],
            view_tick: 5,
            blind: false,
        };
        to_host(&fire);
        match wait(&v, |e| matches!(e, Event::Witness { .. })) {
            Event::Witness { data, .. } => {
                assert_eq!(game::decode::<game::ToHost>(&data), Some(fire))
            }
            _ => unreachable!(),
        }
        wait(&g, |e| matches!(e, Event::FromPeer { .. }));
        wait(&g, |e| matches!(e, Event::FromPeer { .. }));
        // Résultat : la majorité des joueurs ne suffit pas, il faut aussi le vérificateur.
        g.send(Cmd::RoomStarted(true));
        v.send(Cmd::GarantReady);
        std::thread::sleep(Duration::from_millis(300));
        let summary = game::MatchSummary {
            map_sig: [9; 32],
            score: [13, 5],
            rounds: 18,
            players: vec![game::SummaryPlayer {
                pseudo: "Joueur".into(),
                start_team: 0,
                kills: 20,
                deaths: 10,
                assists: 2,
                headshots: 5,
                damage: 2500,
                mvps: 3,
                score: 50,
                rounds: 18,
                teamkills: 0,
                kicked: false,
            }],
        };
        let matches = |p: &Client| {
            p.send(Cmd::Leaderboard { friends: false });
            match wait(p, |e| matches!(e, Event::Leaderboard { .. })) {
                Event::Leaderboard { me, .. } => me.map_or(0, |m| m.wins + m.losses + m.ties),
                _ => unreachable!(),
            }
        };
        p.send(Cmd::MatchResult(game::encode(&summary)));
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(matches(&p), 0, "pas sans le vérificateur");
        let mut other = summary.clone();
        other.score = [5, 13];
        v.send(Cmd::MatchResult(game::encode(&other)));
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(matches(&p), 0, "pas contre le vérificateur");
        v.send(Cmd::MatchResult(game::encode(&summary)));
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(matches(&p), 1, "le vérificateur confirme");
        // Deux preuves de triche : le vérificateur reprend, l'hôte tricheur est écarté.
        for _ in 0..2 {
            v.send(Cmd::Conflict {
                player: 0,
                seq: 0,
                host_pos: [0.0; 3],
                audit_pos: [1.0; 3],
                proof: true,
            });
        }
        match wait(&v, |e| matches!(e, Event::Promoted { .. })) {
            Event::Promoted { old_host, .. } => assert_eq!(old_host, None),
            _ => unreachable!(),
        }
        wait(&g, |e| matches!(e, Event::RoomClosed));
        match wait(&p, |e| matches!(e, Event::HostChanged(_))) {
            // Le garant qui reprend reste anonyme pour les joueurs.
            Event::HostChanged(h) => assert_eq!(h, GARANT_NAME),
            _ => unreachable!(),
        }
        let _ = std::fs::remove_dir_all(dir);
    }
    /// Un vérificateur accuse l'hôte : un troisième garant, sans lien avec la partie,
    /// reçoit les pièces gardées par le relais et tranche. Coupable : le vérificateur
    /// reprend la partie. Innocent : c'est le vérificateur qui est écarté.
    #[test]
    fn a_third_garant_arbitrates_between_the_auditor_and_the_host() {
        for guilty in [true, false] {
            let dir = std::env::temp_dir().join(format!("minelib-arb-{}", rand::random::<u32>()));
            {
                let mut st = stats::Store::open(dir.join("stats")).unwrap();
                for g in ["Garant", "Verif", "Arbitre", "Arbitre2"] {
                    st.get_mut(g).games = 5;
                    st.save(g);
                }
            }
            // Tous les joueurs simulés sont sur cette machine.
            ONE_FPS_PER_MACHINE.store(false, std::sync::atomic::Ordering::Relaxed);
            let h = start(Config {
                dir: dir.clone(),
                bind: "127.0.0.1:0".into(),
                pow_bits: 4,
                name: "essai".into(),
            })
            .unwrap();
            let addr = h.addr.to_string();
            let login = |pseudo: &str| {
                let c = Client::spawn(addr.clone(), Some(h.public));
                c.send(Cmd::Register {
                    pseudo: pseudo.into(),
                    password: "motdepasse".into(),
                });
                let link = match wait(&c, |e| matches!(e, Event::Link(_))) {
                    Event::Link(l) => l,
                    _ => unreachable!(),
                };
                wait(&c, |e| matches!(e, Event::LoggedIn { .. }));
                (c, link)
            };
            let banned = |pseudo: &str| {
                let st = h.state.lock().unwrap();
                let key = st.registry.lookup(pseudo).expect("compte").1;
                st.registry.is_banned(&key)
            };
            let (g, glink) = login("Garant");
            g.send(Cmd::GarantAvailable(true));
            let (p, plink) = login("Joueur");
            std::thread::sleep(Duration::from_millis(200));
            p.send(Cmd::Search {
                map_sig: [9; 32],
                map: "dust".into(),
                players: 10,
            });
            wait(&p, |e| matches!(e, Event::Joined { .. }));
            wait(&g, |e| matches!(e, Event::GarantTask { .. }));
            let (v, _) = login("Verif");
            v.send(Cmd::GarantAvailable(true));
            std::thread::sleep(Duration::from_millis(2100));
            p.send(Cmd::Maps);
            wait(&v, |e| matches!(e, Event::GarantTask { .. }));
            let vid = match wait(&g, |e| matches!(e, Event::BackupIs(_))) {
                Event::BackupIs(id) => id,
                _ => unreachable!(),
            };
            // L'hôte envoie une copie au vérificateur, un joueur tire : le relais garde les deux.
            transport::push(
                &glink,
                &Request::ToPeer {
                    peer: vid,
                    data: game::encode(&game::ToPeer::Backup {
                        part: 0,
                        of: 1,
                        data: b"copie de la partie".to_vec(),
                    }),
                },
            );
            std::thread::sleep(Duration::from_millis(200));
            let fire = game::encode(&game::ToHost::Buy(7));
            transport::push(&plink, &Request::ToHost { data: fire.clone() });
            wait(&v, |e| matches!(e, Event::Witness { .. }));
            // Le troisième garant arrive ; le vérificateur accuse l'hôte.
            let (a, _) = login("Arbitre");
            a.send(Cmd::GarantAvailable(true));
            std::thread::sleep(Duration::from_millis(300));
            v.send(Cmd::Conflict {
                player: 0,
                seq: 0,
                host_pos: [0.0; 3],
                audit_pos: [1.0; 3],
                proof: true,
            });
            let case = match wait(&a, |e| matches!(e, Event::Arbitrate { .. })) {
                Event::Arbitrate { case, map_sig, .. } => {
                    assert_eq!(map_sig, [9; 32]);
                    case
                }
                _ => unreachable!(),
            };
            match wait(&a, |e| matches!(e, Event::Evidence { .. })) {
                Event::Evidence { part, of, data, .. } => {
                    assert_eq!((part, of), (0, 1));
                    let items: Vec<(Option<u64>, Vec<u8>)> = game::decode(&data).unwrap();
                    assert_eq!(items[0], (None, b"copie de la partie".to_vec()));
                    assert!(items[1].0.is_some() && items[1].1 == fire);
                }
                _ => unreachable!(),
            }
            // Un deuxième arbitre possible, pour le contre-arbitrage avant bannissement.
            let (a2, _) = login("Arbitre2");
            if guilty {
                a2.send(Cmd::GarantAvailable(true));
                std::thread::sleep(Duration::from_millis(300));
            }
            a.send(Cmd::Verdict { case, guilty });
            if guilty {
                wait(&v, |e| matches!(e, Event::Promoted { .. }));
                wait(&g, |e| matches!(e, Event::RoomClosed));
                match wait(&p, |e| matches!(e, Event::HostChanged(_))) {
                    // Le garant qui reprend reste anonyme pour les joueurs.
                    Event::HostChanged(h) => assert_eq!(h, GARANT_NAME),
                    _ => unreachable!(),
                }
                // L'hôte a perdu la partie, mais n'est banni qu'après un deuxième arbitre
                // (deux comptes complices ne suffisent pas).
                let case2 = match wait(&a2, |e| matches!(e, Event::Arbitrate { .. })) {
                    Event::Arbitrate { case, .. } => case,
                    _ => unreachable!(),
                };
                assert!(case2 != case && !banned("Garant"));
                a2.send(Cmd::Verdict {
                    case: case2,
                    guilty: true,
                });
                let t = Instant::now();
                while !banned("Garant") {
                    assert!(t.elapsed() < Duration::from_secs(10), "pas banni");
                    std::thread::sleep(Duration::from_millis(20));
                }
            } else {
                // Accusation à tort : le vérificateur est écarté, l'hôte reste.
                wait(&v, |e| matches!(e, Event::RoomClosed));
                std::thread::sleep(Duration::from_millis(200));
                while let Some(e) = p.poll() {
                    assert!(!matches!(e, Event::HostChanged(_)), "l'hôte reste");
                }
            }
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    /// Joueurs simulés sans réseau : une connexion par joueur dans l'état du relais, dont
    /// on lit la file au lieu de l'envoyer. Le relais traite leurs demandes par le vrai
    /// code (`handle_room`, `upkeep`).
    struct Crowd {
        rx: HashMap<u64, std::sync::mpsc::Receiver<Inbound>>,
        names: HashMap<u64, String>,
    }

    impl Crowd {
        fn new() -> Self {
            Self {
                rx: HashMap::new(),
                names: HashMap::new(),
            }
        }

        /// Une connexion de joueur, ou la connexion de garant d'un joueur.
        fn conn(&mut self, st: &mut State, pseudo: &str, garant: bool) -> u64 {
            let (tx, rx) = std::sync::mpsc::channel();
            let id = st.next_id;
            st.next_id += 1;
            let mut c = Conn::new(tx);
            c.pseudo = Some(pseudo.to_string());
            c.garant = garant;
            c.garant_only = garant;
            st.conns.insert(id, c);
            self.rx.insert(id, rx);
            self.names.insert(id, pseudo.to_string());
            id
        }

        fn ask(&self, st: &mut State, id: u64, req: Request) -> Option<Response> {
            handle_room(req, st, id, &self.names[&id])
        }

        /// L'entretien périodique, tout de suite.
        fn upkeep(&self, st: &mut State) {
            st.last_upkeep = Instant::now() - Duration::from_secs(3);
            // (Le temps de l'essai passe plus vite que celui des horloges.)
            st.hosts_wait = None;
            upkeep(st);
        }

        /// Ce que le relais a envoyé à ce joueur depuis la dernière lecture : messages, et
        /// nombre de blocs (ils partiraient regroupés).
        fn got(&self, id: u64) -> (Vec<Response>, usize) {
            let (mut msgs, mut cells) = (Vec::new(), 0);
            for m in self.rx[&id].try_iter() {
                match m {
                    Inbound::Push(b) | Inbound::Loose(b, _) => {
                        msgs.extend(postcard::from_bytes::<Response>(&b).ok())
                    }
                    Inbound::Cell(_) => cells += 1,
                    _ => {}
                }
            }
            (msgs, cells)
        }

        /// Octets à envoyer à tous depuis la dernière lecture (en-têtes de message
        /// compris ; les blocs à 7 octets, regroupés), et nombre de messages et de blocs.
        fn sent(&self) -> (usize, usize, usize) {
            let (mut bytes, mut msgs, mut cells) = (0, 0, 0);
            for rx in self.rx.values() {
                for m in rx.try_iter() {
                    match m {
                        Inbound::Push(b) | Inbound::Loose(b, _) => {
                            bytes += b.len() + 20;
                            msgs += 1;
                        }
                        Inbound::Cell(_) => {
                            bytes += 7;
                            cells += 1;
                        }
                        _ => {}
                    }
                }
            }
            (bytes, msgs, cells)
        }
    }

    /// Un monde de construction ouvert par « Proprio », sur un relais sans réseau. (Sans
    /// provinces : les essais qui en veulent le disent, `Handle::set_provinces`.)
    /// Relais de province (v141) : un relais de secours n'y entre qu'avec le jeton du
    /// relais du monde (pour cette province, cette époque, ce pseudo et cette clé) ; il
    /// reçoit tout ce qu'elle a, puis seulement ce que le relais du monde a gardé. Un relais
    /// de secours ne fait entrer aucun joueur tant qu'il n'a pas pris la place du relais.
    /// Un serveur de construction sous liste blanche ne paraît dans la liste que pour qui
    /// peut y entrer (son créateur, ses OP, ceux de la liste), jamais pour un autre relais.
    #[test]
    fn whitelisted_servers_are_listed_only_for_those_who_may_enter() {
        let (h, dir) = crowd_world("liste_blanche");
        {
            let mut guard = h.state.lock().unwrap();
            let st = &mut *guard;
            let mut crowd = Crowd::new();
            let (owner, room) = host_world(&mut crowd, st);
            let other = crowd.conn(st, "Joueur", false);
            let listed = |crowd: &Crowd, st: &mut State, id: u64| -> bool {
                match crowd.ask(st, id, Request::Servers { kind: 1 }) {
                    Some(Response::Servers { list }) => list.iter().any(|e| e.id == room),
                    r => panic!("{r:?}"),
                }
            };
            let admin = |crowd: &Crowd, st: &mut State, action: u8, who: &str| {
                crowd.ask(
                    st,
                    owner,
                    Request::ServerAdmin {
                        action,
                        pseudo: who.into(),
                        reason: String::new(),
                    },
                );
            };
            assert!(listed(&crowd, st, other), "sans liste blanche : visible");
            admin(&crowd, st, proto::SERVER_WL_ON, "");
            assert!(
                !listed(&crowd, st, other),
                "liste blanche : caché aux autres"
            );
            assert!(
                listed(&crowd, st, owner),
                "toujours visible pour son créateur"
            );
            let mut session = Session::Anonymous;
            let far = handle_account(
                Request::PeerServers { kind: 1 },
                st,
                &mut session,
                "1.2.3.4",
                other,
            );
            assert!(
                matches!(&far, Response::Servers { list } if list.iter().all(|e| e.id != room)),
                "jamais donné aux autres relais"
            );
            admin(&crowd, st, proto::SERVER_WL_ADD, "Joueur");
            assert!(listed(&crowd, st, other), "sur la liste : visible");
            admin(&crowd, st, proto::SERVER_WL_OFF, "");
            let far = handle_account(
                Request::PeerServers { kind: 1 },
                st,
                &mut session,
                "1.2.3.4",
                other,
            );
            assert!(
                matches!(&far, Response::Servers { list } if list.iter().any(|e| e.id == room)),
                "liste blanche retirée : de nouveau pour tous"
            );
        }
        drop(h);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// (v155) Les billets des relais de région sont signés par le relais du monde : ils se
    /// vérifient avec sa clé publique, et un hôte (qui n'a pas sa clé privée) ne sait pas en
    /// faire, ni pour un autre joueur ni pour plus longtemps.
    #[test]
    fn region_tickets_are_signed_by_the_world_relay() {
        use ed25519_dalek::Signer;
        let world = SigningKey::from_bytes(&[3u8; 32]);
        let host = SigningKey::from_bytes(&[4u8; 32]);
        let root = world.verifying_key().to_bytes();
        let sign = |k: &SigningKey, pseudo: &str, expires: u64| {
            k.sign(&proto::ticket_msg(42, pseudo, expires))
                .to_bytes()
                .to_vec()
        };
        let good = sign(&world, "Joueur", 1000);
        assert!(proto::ticket_ok(&root, 42, "Joueur", 1000, &good));
        assert!(
            proto::ticket_ok(&root, 42, "joueur", 1000, &good),
            "pseudo sans casse"
        );
        assert!(!proto::ticket_ok(&root, 42, "Autre", 1000, &good));
        assert!(!proto::ticket_ok(&root, 42, "Joueur", 9000, &good));
        assert!(!proto::ticket_ok(&root, 43, "Joueur", 1000, &good));
        assert!(!proto::ticket_ok(
            &root,
            42,
            "Joueur",
            1000,
            &sign(&host, "Joueur", 1000)
        ));
        assert!(!proto::ticket_ok(&root, 42, "Joueur", 1000, &[0; 64]));
        assert!(!proto::ticket_ok(&root, 42, "Joueur", 1000, &[]));
        // Clés des provinces : chacune la sienne, dérivée de celle de son pays.
        let s = [7u8; 32];
        let a = proto::province_key(&s, 42, [0, 0], 4);
        assert_ne!(a, proto::province_key(&s, 42, [1, 0], 4));
        assert_ne!(a, proto::province_key(&s, 42, [0, 0], 2));
        assert_ne!(proto::province_key(&a, 42, [0, 0], 1), a);
    }

    #[test]
    fn a_province_standby_needs_the_world_relay_token() {
        let root_key = SigningKey::from_bytes(&[3u8; 32]);
        let root = root_key.verifying_key().to_bytes();
        // (v155) La province n'a que sa clé, dérivée du secret du monde.
        let world = [7u8; 32];
        let secret = proto::province_key(&world, 42, [0, 0], 1);
        let neighbour = proto::province_key(&world, 42, [1, 0], 1);
        let info = proto::StandbyInfo {
            name: "Monde".into(),
            map: "monde".into(),
            map_sig: [6; 32],
            max_players: 0,
            creator: "Createur".into(),
            server: servers::key("Createur", "monde"),
            owner: "Createur".into(),
        };
        let (h, dir) = crowd_world("secours");
        {
            let mut guard = h.state.lock().unwrap();
            let st = &mut *guard;
            assert!(provinces::begin(
                st,
                42,
                [0, 0],
                1,
                5,
                secret,
                root,
                info.clone(),
                Default::default(),
                false,
                None,
                Vec::new(),
            ));
            // Un bloc déjà publié par la province.
            let data = game::encode(&game::BuildMsg::Set {
                pos: [3, 70, 3],
                block: 4,
                by: "J".into(),
                seq: 9,
            });
            st.rooms
                .get_mut(&42)
                .unwrap()
                .build
                .mirror_at([0, 0], &data, Some(9), &[]);
            let mut crowd = Crowd::new();
            let f = crowd.conn(st, "Suiveur", true);
            let signer = [9u8; 32];
            let mut session = Session::Anonymous;
            let follow = |st: &mut State, session: &mut Session, epoch: u64, token: [u8; 32]| {
                provinces::follow(
                    st,
                    session,
                    f,
                    42,
                    [0, 0],
                    epoch,
                    "Suiveur".into(),
                    signer,
                    token,
                    ([0; 32], String::new(), [0; 32]),
                )
            };
            // Jeton inventé, ou d'une autre époque, ou d'un autre joueur : refusé.
            assert!(matches!(
                follow(st, &mut session, 5, [0; 32]),
                Response::Error { .. }
            ));
            let other_epoch = proto::follow_token(&secret, 42, [0, 0], "Suiveur", &signer, 6);
            assert!(matches!(
                follow(st, &mut session, 6, other_epoch),
                Response::Error { .. }
            ));
            let other_player = proto::follow_token(&secret, 42, [0, 0], "Autre", &signer, 5);
            assert!(matches!(
                follow(st, &mut session, 5, other_player),
                Response::Error { .. }
            ));
            // (v155) Fait par l'hôte d'une autre province (avec sa clé à elle), ou avec le
            // secret du monde, qu'aucun hôte n'a plus : refusé.
            for key in [neighbour, world] {
                let forged = proto::follow_token(&key, 42, [0, 0], "Suiveur", &signer, 5);
                assert!(matches!(
                    follow(st, &mut session, 5, forged),
                    Response::Error { .. }
                ));
            }
            assert!(crowd.got(f).0.is_empty(), "rien n'est parti à un refusé");
            // Le bon : il reçoit ce qu'elle a.
            let token = proto::follow_token(&secret, 42, [0, 0], "Suiveur", &signer, 5);
            assert!(matches!(
                follow(st, &mut session, 5, token),
                Response::Welcome { .. }
            ));
            let got = crowd.got(f).0;
            assert!(
                got.iter()
                    .any(|r| matches!(r, Response::ProvinceSeed { entries, .. }
                    if entries.iter().any(|e| e.1 == 9))),
                "historique reçu : {got:?}"
            );
            assert!(got
                .iter()
                .any(|r| matches!(r, Response::ProvinceCopy { n: 0, .. })));
        }
        drop(h);
        let _ = std::fs::remove_dir_all(dir);
        // Un relais de secours ne fait entrer personne, même avec un certificat valable.
        let (h, dir) = crowd_world("secours2");
        {
            let mut guard = h.state.lock().unwrap();
            let st = &mut *guard;
            assert!(provinces::begin(
                st,
                42,
                [0, 0],
                1,
                5,
                secret,
                root,
                info,
                Default::default(),
                true,
                None,
                Vec::new(),
            ));
            let cert = minelib_net::sovereign::Cert::issue(
                &root_key,
                42,
                "Joueur",
                [1; 32],
                now_secs() + 3600,
            );
            assert_eq!(provinces::admits(st, &cert), Some(false));
            // Promu : il fait entrer.
            assert_eq!(provinces::promote_here(st, 5, 8), Some(0));
            assert_eq!(provinces::admits(st, &cert), Some(true));
            // (Une seconde promotion de la même époque ne fait rien.)
            assert_eq!(provinces::promote_here(st, 5, 9), None);
        }
        drop(h);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// (v148) Mesure à la main : ce que coûte un pays au PC de son hôte. Ses provinces (une
    /// région chacune) renouvellent leur bail toutes les 300 ms en disant leurs joueurs ; un
    /// joueur sur deux cents change de province chaque seconde ; son entretien (`tick`) passe
    /// à chaque bail. Calcul pour une seconde de jeu. (v149) Tous les joueurs connus du pays ;
    /// (v150) avec vingt joueurs par région (la Terre) et cent (une foule partout), et la
    /// taille de son propre bail à son pays.
    /// `cargo test --release -p minelib-relay charge_pays -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn charge_pays() {
        let sizes: Vec<usize> = std::env::var("MINELIB_CHARGE")
            .ok()
            .and_then(|v| v.parse().ok())
            .map_or(vec![1024, 4096], |n| vec![n]);
        for per in [20usize, 100] {
            for &kids in &sizes {
                charge_pays_once(kids, per);
            }
        }
    }

    fn charge_pays_once(kids: usize, per: usize) {
        let (h, dir) = crowd_world("pays");
        let mut guard = h.state.lock().unwrap();
        let st = &mut *guard;
        let mut crowd = Crowd::new();
        let side = (kids as f64).sqrt().ceil() as i32;
        let root = SigningKey::from_bytes(&[3; 32]).verifying_key().to_bytes();
        let info = proto::StandbyInfo {
            name: "Monde".into(),
            map: "monde".into(),
            map_sig: [6; 32],
            max_players: 0,
            creator: "Createur".into(),
            server: servers::key("Createur", "monde"),
            owner: "Createur".into(),
        };
        assert!(provinces::begin(
            st,
            42,
            [0, 0],
            side,
            5,
            [7; 32],
            root,
            info,
            Default::default(),
            false,
            Some(proto::Nest { size: 1, from: 3 }),
            Vec::new(),
        ));
        provinces::test_serving(st, 42);
        // (Son pays au-dessus : un pays, qui veut les régions à partir de 24 joueurs et pas
        // la liste de ses provinces.)
        let pays_epoch = provinces::test_epoch(st, 42);
        provinces::leased(
            st,
            pays_epoch,
            now_ms(),
            true,
            false,
            0,
            0,
            None,
            None,
            SHARD_FROM as u32,
            false,
        );
        let mut hosts = Vec::new();
        for k in 0..kids {
            let p = [k as i32 % side, k as i32 / side];
            let id = crowd.conn(st, &format!("Hote{k}"), true);
            st.conns.get_mut(&id).unwrap().room = Some(42);
            let epoch = provinces::test_child(st, 42, p, id);
            hosts.push((p, id, epoch));
        }
        // Une province dit ses régions peuplées une par une seulement à partir de 24 joueurs
        // (v150), et tous ses joueurs.
        let counts = |p: [i32; 2]| -> Vec<([i32; 2], u32)> {
            if per >= SHARD_FROM {
                vec![(p, per as u32)]
            } else {
                Vec::new()
            }
        };
        // (v149) Les joueurs de chaque province, dits au pays à leur premier bail : il sait
        // où est chacun.
        let mut names: Vec<Vec<String>> = (0..kids)
            .map(|i| (0..per).map(|k| format!("j{i}-{k}")).collect())
            .collect();
        for (i, &(p, id, epoch)) in hosts.iter().enumerate() {
            crowd.ask(
                st,
                id,
                Request::ProvinceLease {
                    room: 42,
                    province: p,
                    epoch,
                    t: now_ms(),
                    counts: counts(p),
                    inside: Vec::new(),
                    followed: true,
                    wants: Vec::new(),
                    children: None,
                    arrived: names[i].clone(),
                    left: Vec::new(),
                    seq: 0,
                    total: per as u32,
                },
            );
            let _ = crowd.got(id);
        }
        let known = provinces::test_known(st, 42);
        let rounds = 20;
        let (mut lease_t, mut tick_t) = (Duration::ZERO, Duration::ZERO);
        for round in 0..rounds {
            // (Un joueur sur deux cents passe chaque seconde dans la province voisine.)
            let mut moves: Vec<(Vec<String>, Vec<String>)> = vec![Default::default(); kids];
            for i in 0..kids {
                if (i * 100 / per + round).is_multiple_of(7) && i.is_multiple_of(100 / per) {
                    let j = (i + 1) % kids;
                    if let Some(w) = names[i].pop() {
                        moves[i].1.push(w.clone());
                        moves[j].0.push(w.clone());
                        names[j].push(w);
                    }
                }
            }
            let t = Instant::now();
            for (i, &(p, id, epoch)) in hosts.iter().enumerate() {
                let (arrived, left) = std::mem::take(&mut moves[i]);
                crowd.ask(
                    st,
                    id,
                    Request::ProvinceLease {
                        room: 42,
                        province: p,
                        epoch,
                        t: now_ms(),
                        counts: counts(p),
                        inside: Vec::new(),
                        followed: true,
                        wants: Vec::new(),
                        children: None,
                        arrived,
                        left,
                        seq: 0,
                        total: names[i].len() as u32,
                    },
                );
            }
            lease_t += t.elapsed();
            let t = Instant::now();
            let _ = provinces::tick(st);
            tick_t += t.elapsed();
            for &(_, id, _) in &hosts {
                let _ = crowd.got(id);
            }
        }
        // Son propre bail, à son pays : ce qui remonte, et ce que coûte ce tour-là (le
        // deuxième : au premier, il dit leurs voisines à toutes ses provinces).
        provinces::test_lease_due(st, 42);
        let up = provinces::tick(st)
            .iter()
            .find(|r| matches!(r, Request::ProvinceLease { .. }))
            .map_or(0, |r| postcard::to_allocvec(r).unwrap().len());
        provinces::test_lease_due(st, 42);
        let t = Instant::now();
        let _ = provinces::tick(st);
        let lease_tick = t.elapsed();
        // Un tour sans bail (la boucle de l'hôte tourne jusqu'à 500 fois par seconde).
        let t = Instant::now();
        for _ in 0..100 {
            let _ = provinces::tick(st);
        }
        let plain_tick = t.elapsed() / 100;
        let per_lease = lease_t / (rounds * kids) as u32;
        // Une seconde : les baux de ses provinces, trois tours avec son propre bail, 500
        // tours sans.
        let per_s = (lease_t / rounds as u32).as_secs_f64() / 0.3
            + lease_tick.as_secs_f64() / 0.3
            + plain_tick.as_secs_f64() * 500.0;
        // Taille d'un bail et de sa réponse (sans le chiffrement : 16 octets de plus).
        let (p, id, epoch) = hosts[0];
        let ask = postcard::to_allocvec(&Request::ProvinceLease {
            room: 42,
            province: p,
            epoch,
            t: now_ms(),
            counts: counts(p),
            inside: Vec::new(),
            followed: true,
            wants: Vec::new(),
            children: None,
            arrived: Vec::new(),
            left: Vec::new(),
            seq: 1 << 40,
            total: per as u32,
        })
        .unwrap()
        .len();
        crowd.ask(
            st,
            id,
            Request::ProvinceLease {
                room: 42,
                province: p,
                epoch,
                t: now_ms(),
                counts: counts(p),
                inside: Vec::new(),
                followed: true,
                wants: Vec::new(),
                children: None,
                arrived: Vec::new(),
                left: Vec::new(),
                seq: 0,
                total: per as u32,
            },
        );
        let answer = crowd
            .got(id)
            .0
            .iter()
            .find(|r| matches!(r, Response::ProvinceLeased { .. }))
            .map_or(0, |r| postcard::to_allocvec(r).unwrap().len());
        let mbit = |bytes: usize| (bytes + 20) as f64 * kids as f64 / 0.3 * 8.0 / 1e6;
        eprintln!(
            "pays de {kids} provinces de {per} joueurs ({known} connus du pays) : {per_lease:?} \
             par bail reçu, tour avec son bail {lease_tick:?}, tour sans {plain_tick:?} : \
             {per_s:.3} s de calcul par seconde de jeu (tours pendant les baux : {:?})",
            tick_t / rounds as u32
        );
        eprintln!(
            "  bail d'une province {ask} octets, réponse {answer} octets : {:.1} Mbit/s reçus, \
             {:.1} Mbit/s envoyés ; son propre bail à son pays : {up} octets",
            mbit(ask),
            mbit(answer)
        );
        drop(guard);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// (v149) Mesure à la main : ce que coûte au relais central un lancement du jeu. Un
    /// joueur entre dans le monde, dit où il est (au fond d'un pays), puis passe en veille ;
    /// avant, la connexion de son compte vérifie son mot de passe (Argon2). Sans la poignée de
    /// main chiffrée de la connexion.
    /// `cargo test --release -p minelib-relay charge_entrees -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn charge_entrees() {
        use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
        let salt = SaltString::encode_b64(&[7; 16]).unwrap();
        let hash = argon2::Argon2::default()
            .hash_password(b"preuve de mot de passe", &salt)
            .unwrap()
            .to_string();
        let parsed = PasswordHash::new(&hash).unwrap();
        let t = Instant::now();
        for _ in 0..10 {
            assert!(argon2::Argon2::default()
                .verify_password(b"preuve de mot de passe", &parsed)
                .is_ok());
        }
        let argon = t.elapsed() / 10;
        let (h, dir) = crowd_world("entrees");
        let mut guard = h.state.lock().unwrap();
        let st = &mut *guard;
        let mut crowd = Crowd::new();
        let (_, room) = host_world(&mut crowd, st);
        // Le monde a confié la région où entrent les joueurs.
        let host = crowd.conn(st, "Hote", true);
        let _ = provinces::test_child(st, room, [0, 0], host);
        let n = 2000;
        let t = Instant::now();
        for i in 0..n {
            let id = crowd.conn(st, &format!("Joueur{i}"), false);
            assert!(matches!(
                crowd.ask(st, id, Request::Join { room }),
                Some(Response::Joined { .. })
            ));
            crowd.ask(
                st,
                id,
                Request::Position {
                    chunk: [4 + i as i32 % 8, 4 + i as i32 / 300],
                },
            );
            assert!(matches!(
                crowd.ask(st, id, Request::Doze { room }),
                Some(Response::Dozed { .. })
            ));
            st.conns.remove(&id);
        }
        let entry = t.elapsed() / n;
        eprintln!(
            "lancement du jeu au relais central : mot de passe vérifié en {argon:?}, entrée dans \
             le monde puis veille en {entry:?} (sans la poignée de main de la connexion)"
        );
        for lancements in [100_000f64, 1_100_000.0] {
            eprintln!(
                "  {lancements} lancements par seconde : {:.0} cœurs pour les mots de passe, \
                 {:.1} pour les entrées",
                lancements * argon.as_secs_f64(),
                lancements * entry.as_secs_f64()
            );
        }
        drop(guard);
        let _ = std::fs::remove_dir_all(dir);
    }

    fn crowd_world(tag: &str) -> (Handle, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("minelib-{tag}-{}", rand::random::<u32>()));
        let h = start(Config {
            dir: dir.clone(),
            bind: "127.0.0.1:0".into(),
            pow_bits: 4,
            name: "essai".into(),
        })
        .unwrap();
        h.set_provinces(proto::PROVINCE as usize, usize::MAX);
        (h, dir)
    }

    fn host_world(crowd: &mut Crowd, st: &mut State) -> (u64, u64) {
        let owner = crowd.conn(st, "Proprio", false);
        let room = match crowd.ask(
            st,
            owner,
            Request::Host {
                kind: 1,
                name: "Monde".into(),
                map: "monde".into(),
                map_sig: [6; 32],
                max_players: 0,
            },
        ) {
            Some(Response::Hosted { room }) => room,
            r => panic!("{r:?}"),
        };
        (owner, room)
    }

    /// Un monde où tout le monde joue trouve ses hôtes de relais de région parmi ses
    /// propres joueurs (ceux d'une autre région), ne les remplace pas tant qu'ils restent,
    /// et ne dit à chaque joueur que les relais des régions qu'il a en vue.
    #[test]
    fn region_relays_are_hosted_by_players_of_the_world_and_stay() {
        let (h, dir) = crowd_world("hotes");
        let mut guard = h.state.lock().unwrap();
        let st = &mut *guard;
        let mut crowd = Crowd::new();
        let (_, room) = host_world(&mut crowd, st);
        // 60 joueurs dans la région (0, 0), 20 dans la région (5, 5), loin de là (trop peu
        // pour qu'elle ait son relais).
        let mut here = Vec::new();
        let mut far = Vec::new();
        for i in 0..80 {
            let id = crowd.conn(st, &format!("Joueur{i}"), false);
            assert!(matches!(
                crowd.ask(st, id, Request::Join { room }),
                Some(Response::Joined { .. })
            ));
            let chunk = if i < 60 {
                here.push(id);
                [4 + i % 8, 4 + i / 8]
            } else {
                far.push(id);
                [5 * 32 + 4 + i % 8, 5 * 32 + 4]
            };
            crowd.ask(st, id, Request::Position { chunk });
        }
        // Connexions de garant : deux joueurs de la région peuplée, trois de l'autre.
        let mut garants = HashMap::new();
        for i in [0, 1, 60, 61, 62] {
            garants.insert(crowd.conn(st, &format!("Joueur{i}"), true), i);
        }
        crowd.upkeep(st);
        let hosts = |st: &State| -> Vec<u64> {
            st.rooms[&room]
                .shards
                .get(&[0, 0])
                .into_iter()
                .flatten()
                .map(|s| s.host)
                .collect()
        };
        let first = hosts(st);
        assert_eq!(first.len(), 1, "la région peuplée a son relais");
        assert!(
            garants[&first[0]] >= 60,
            "hébergé par un joueur de l'autre région, pas par un joueur de celle-ci"
        );
        assert!(!st.rooms[&room].shards.contains_key(&[5, 5]));
        let ready = |crowd: &Crowd, st: &mut State, host: u64| {
            crowd.ask(
                st,
                host,
                Request::ShardReady {
                    region: [0, 0],
                    node: [0; 32],
                    addr: format!("127.0.0.1:{host}"),
                    key: [1; 32],
                },
            );
        };
        ready(&crowd, st, first[0]);
        for _ in 0..5 {
            crowd.upkeep(st);
        }
        assert_eq!(hosts(st), first, "l'hôte reste tant que son joueur reste");
        // Chaque joueur de la région a reçu son relais (une fois) ; ceux de l'autre région
        // n'ont rien reçu : ce relais n'est pas en vue de là où ils sont.
        let lists = |crowd: &Crowd, id: u64| -> Vec<usize> {
            crowd
                .got(id)
                .0
                .into_iter()
                .filter_map(|m| match m {
                    Response::Shards { list } => Some(list.len()),
                    _ => None,
                })
                .collect()
        };
        for &p in &here {
            assert_eq!(lists(&crowd, p), [1]);
        }
        for &p in &far {
            assert!(lists(&crowd, p).is_empty());
        }
        // Un joueur de l'autre région arrive : il reçoit le relais en entrant en vue.
        crowd.ask(st, far[5], Request::Position { chunk: [40, 4] });
        assert_eq!(lists(&crowd, far[5]), [1]);
        // Le joueur qui héberge entre dans la région qu'il sert : un autre prend sa place.
        let player = far[garants[&first[0]] - 60];
        crowd.ask(st, player, Request::Position { chunk: [5, 5] });
        crowd.upkeep(st);
        let both = hosts(st);
        assert_eq!(both.len(), 2, "le remplaçant est désigné");
        ready(&crowd, st, both[1]);
        crowd.upkeep(st);
        let after = hosts(st);
        assert_eq!(after.len(), 1);
        assert!(after[0] != first[0] && garants[&after[0]] >= 60);
        drop(guard);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Essai de charge du relais du monde, sans réseau : `n` joueurs dans un seul monde,
    /// par groupes de 100 dans des régions différentes (chacune a son relais de région,
    /// hébergé par un joueur d'une autre région), qui changent de chunk toutes les 4 s et
    /// posent un bloc toutes les 2 s. Mesure le temps de calcul du relais du monde pour une
    /// seconde de jeu (il ne traite qu'une demande à la fois : au-delà d'une seconde, il ne
    /// suit plus), et ce qu'il envoie. Sans le chiffrement ni les connexions elles-mêmes.
    /// `cargo test --release -p minelib-relay charge -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn charge_du_relais_du_monde() {
        for n in [2_000usize, 10_000, 30_000, 60_000, 120_000] {
            let (h, dir) = crowd_world("charge");
            let mut guard = h.state.lock().unwrap();
            let st = &mut *guard;
            let mut crowd = Crowd::new();
            let (_, room) = host_world(&mut crowd, st);
            let side = (n / 100).max(1).isqrt() as i32 + 1;
            let home = |i: usize| -> [i32; 2] {
                let g = (i / 100) as i32;
                let (rx, rz) = (g % side, g / side);
                // (Une région sur deux : les relais d'une région ne sont en vue que d'elle.)
                [
                    rx * 64 + 8 + (i % 10) as i32,
                    rz * 64 + 8 + (i / 10 % 10) as i32,
                ]
            };
            let t = Instant::now();
            let mut players = Vec::new();
            for i in 0..n {
                let name = format!("J{i}");
                let id = crowd.conn(st, &name, false);
                crowd.ask(st, id, Request::Join { room });
                crowd.ask(st, id, Request::Position { chunk: home(i) });
                players.push(id);
                // Un joueur sur 40 prête sa connexion de garant.
                if i % 40 == 7 {
                    crowd.conn(st, &name, true);
                }
            }
            let join = t.elapsed();
            // Les relais de région : désignés, prêts, annoncés.
            let t = Instant::now();
            crowd.upkeep(st);
            let first_upkeep = t.elapsed();
            let tasks: Vec<([i32; 2], u64)> = st.rooms[&room]
                .shards
                .iter()
                .flat_map(|(r, l)| l.iter().map(|s| (*r, s.host)))
                .collect();
            for (region, host) in &tasks {
                crowd.ask(
                    st,
                    *host,
                    Request::ShardReady {
                        region: *region,
                        node: [0; 32],
                        addr: format!("127.0.0.1:{host}"),
                        key: [1; 32],
                    },
                );
            }
            crowd.upkeep(st);
            for (i, &id) in players.iter().enumerate() {
                let regions = vec![proto::region_of(home(i))];
                crowd.ask(st, id, Request::Served { regions });
            }
            crowd.sent();
            // Trois secondes de jeu.
            let (mut work, mut upkeeps) = (Duration::ZERO, Duration::ZERO);
            let (mut bytes, mut msgs, mut cells) = (0, 0, 0);
            let mut seed = 99u64;
            let mut rnd = move || {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                seed
            };
            let secs = 3;
            for _ in 0..secs {
                st.last_upkeep = Instant::now();
                let t = Instant::now();
                for (i, &id) in players.iter().enumerate() {
                    let mut chunk = home(i);
                    if rnd() % 4 == 0 {
                        chunk[0] += (rnd() % 3) as i32 - 1;
                        crowd.ask(st, id, Request::Position { chunk });
                    }
                    if rnd() % 2 == 0 {
                        let c = home(i);
                        let pos = [
                            c[0] * 16 + (rnd() % 16) as i32,
                            60 + (rnd() % 30) as i32,
                            c[1] * 16 + (rnd() % 16) as i32,
                        ];
                        let data = game::encode(&game::BuildMsg::Edit {
                            pos,
                            block: 1 + (rnd() % 40) as u16,
                            fluid: false,
                        });
                        crowd.ask(st, id, Request::ToZone { chunk: c, data });
                    }
                }
                work += t.elapsed();
                let t = Instant::now();
                crowd.upkeep(st);
                upkeeps += t.elapsed();
                let s = crowd.sent();
                (bytes, msgs, cells) = (bytes + s.0, msgs + s.1, cells + s.2);
            }
            eprintln!(
                "CHARGE {n:>6} joueurs, {} relais de région | arrivée : {:.1} µs par joueur | \
                 premier entretien {:.0} ms | une seconde de jeu : {:.0} ms de calcul \
                 ({:.1} µs par demande) | entretien : {:.1} ms | envoyé par seconde : {} \
                 messages, {} blocs, {:.2} Mbit/s",
                tasks.len(),
                join.as_secs_f64() * 1e6 / n as f64,
                first_upkeep.as_secs_f64() * 1e3,
                work.as_secs_f64() * 1e3 / secs as f64,
                work.as_secs_f64() * 1e6 / (secs as f64 * n as f64 * 0.75),
                upkeeps.as_secs_f64() * 1e3 / secs as f64,
                msgs / secs,
                cells / secs,
                bytes as f64 * 8.0 / secs as f64 / 1e6,
            );
            drop(guard);
            drop(h);
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    /// Un monde de 400 joueurs en quatre régions souveraines (deux relais chacune, tenus par
    /// des joueurs d'autres régions), sans réseau : (joueurs, hôtes → rang du joueur, clés).
    fn sovereign_crowd(
        st: &mut State,
        crowd: &mut Crowd,
        room: u64,
    ) -> (Vec<u64>, HashMap<u64, usize>, Vec<SigningKey>) {
        let home = |i: usize| -> [i32; 2] {
            let g = (i / 100) as i32;
            [
                (g % 2) * 64 + 8 + (i % 10) as i32,
                (g / 2) * 64 + 8 + (i / 10 % 10) as i32,
            ]
        };
        let mut players = Vec::new();
        let mut keys = Vec::new();
        let mut garant_of = HashMap::new();
        for i in 0..400 {
            let name = format!("J{i}");
            let mut b = [9u8; 32];
            b[..8].copy_from_slice(&(i as u64).to_le_bytes());
            let k = SigningKey::from_bytes(&b);
            let id = crowd.conn(st, &name, false);
            st.conns.get_mut(&id).unwrap().account = Some(k.verifying_key().to_bytes());
            crowd.ask(st, id, Request::Join { room });
            crowd.ask(st, id, Request::Position { chunk: home(i) });
            players.push(id);
            if i % 25 == 7 {
                let g = crowd.conn(st, &name, true);
                st.conns.get_mut(&g).unwrap().account = Some(k.verifying_key().to_bytes());
                garant_of.insert(g, i);
            }
            keys.push(k);
        }
        crowd.upkeep(st);
        let tasks: Vec<([i32; 2], u64)> = st.rooms[&room]
            .shards
            .iter()
            .flat_map(|(r, l)| l.iter().map(|s| (*r, s.host)))
            .collect();
        for (region, host) in &tasks {
            crowd.ask(
                st,
                *host,
                Request::ShardReady {
                    region: *region,
                    node: [0; 32],
                    addr: format!("127.0.0.1:{host}"),
                    key: [1; 32],
                },
            );
        }
        crowd.upkeep(st);
        crowd.upkeep(st);
        (players, garant_of, keys)
    }

    /// Lot signé par l'hôte principal d'une région souveraine, qui suit ce que le relais du
    /// monde a reçu ; `forged` : une modification au nom d'un joueur, mal signée (un faux).
    fn next_batch(
        st: &State,
        room: u64,
        region: [i32; 2],
        primary_key: &SigningKey,
        author: (&str, &SigningKey),
        forged: bool,
    ) -> sovereign::Batch {
        let sv = &st.rooms[&room].sov[&region];
        let (first, prev) = sv
            .pending
            .back()
            .map_or((sv.base, sv.chain), |p| (p.end, p.chain));
        let c = [region[0] * proto::REGION + 9, region[1] * proto::REGION + 9];
        let signer = if forged {
            SigningKey::from_bytes(&[0x66; 32])
        } else {
            author.1.clone()
        };
        let edit = sovereign::Edit::sign(
            &signer,
            room,
            author.0,
            [c[0] * 16 + 3, 70, c[1] * 16 + 3],
            1,
            false,
            c,
            now_ms() + (first - sv.first),
            false,
        );
        let cert = sovereign::Cert::issue(
            &st.cert_key,
            room,
            author.0,
            author.1.verifying_key().to_bytes(),
            now_secs() + 3600,
        );
        let mut batch = sovereign::Batch {
            room,
            region,
            epoch: sv.epoch,
            first,
            prev,
            records: vec![sovereign::Record { seq: first, edit }],
            certs: vec![cert],
            sig: Vec::new(),
        };
        batch.sign(primary_key);
        batch
    }

    /// Surveillance mutuelle (v136) : le relais du monde ne vérifie plus lui-même les lots
    /// des régions souveraines ; il en confie au hasard à des hôtes d'autres régions, et ne
    /// revérifie que ce qu'ils signalent. Deux hôtes complices d'une même région (l'hôte
    /// principal qui glisse un faux, l'autre qui le confirme) sont pris et écartés tous les
    /// deux ; un vérificateur qui invente une faute, ou qui laisse passer celle que le relais
    /// trouve en vérifiant lui-même, est écarté ; un hôte honnête ne l'est jamais.
    #[test]
    fn hosts_of_other_regions_check_batches_and_catch_two_accomplices() {
        let (h, dir) = crowd_world("audit");
        h.set_shard_share(50);
        let mut guard = h.state.lock().unwrap();
        let st = &mut *guard;
        st.audit_every = 1;
        st.audit_self = u32::MAX;
        let mut crowd = Crowd::new();
        let (_, room) = host_world(&mut crowd, st);
        let (_, garant_of, keys) = sovereign_crowd(st, &mut crowd, room);
        let regions: Vec<[i32; 2]> = st.rooms[&room].sov.keys().copied().collect();
        assert_eq!(regions.len(), 4, "quatre régions souveraines");
        let hosts_of = |st: &State, r: [i32; 2]| -> (u64, u64) {
            let sv = &st.rooms[&room].sov[&r];
            let w = st.rooms[&room].shards[&r]
                .iter()
                .map(|s| s.host)
                .find(|&h| h != sv.primary)
                .unwrap();
            (sv.primary, w)
        };
        // Ce que le relais du monde a confié aux hôtes : (hôte, vérification).
        let audits = |crowd: &Crowd| -> Vec<(u64, Response)> {
            let mut out = Vec::new();
            for &g in garant_of.keys() {
                for m in crowd.got(g).0 {
                    if matches!(m, Response::Audit { .. }) {
                        out.push((g, m));
                    }
                }
            }
            out
        };
        // Réponse d'un vérificateur honnête : il refait la vérification.
        let honest = |m: &Response| -> Request {
            let Response::Audit {
                id,
                room,
                region,
                epoch,
                key,
                root,
                floor_t,
                batch,
            } = m
            else {
                unreachable!()
            };
            let rules = sovereign::Rules {
                room: *room,
                region: *region,
                root: *root,
                floor_t: *floor_t,
            };
            let key = VerifyingKey::from_bytes(key).unwrap();
            Request::Audited {
                id: *id,
                fault: batch.audit(&rules, *epoch, &key),
            }
        };
        crowd.sent();
        let r0 = regions[0];
        let (p0, w0) = hosts_of(st, r0);
        let author = (format!("J{}", garant_of[&p0]), keys[garant_of[&p0]].clone());
        // 1. Un lot honnête : vérifié par un hôte d'une autre région (jamais un de la
        // sienne), rien à signaler, personne d'écarté.
        let b = next_batch(
            st,
            room,
            r0,
            &keys[garant_of[&p0]],
            (&author.0, &author.1),
            false,
        );
        crowd.ask(
            st,
            p0,
            Request::Archive {
                region: r0,
                batches: vec![b],
            },
        );
        let got = audits(&crowd);
        assert_eq!(got.len(), 1, "le lot est confié à un vérificateur");
        let (auditor, m) = &got[0];
        let theirs = st.rooms[&room]
            .shards
            .iter()
            .find(|(_, l)| l.iter().any(|s| s.host == *auditor))
            .map(|(r, _)| *r)
            .unwrap();
        assert_ne!(theirs, r0, "un hôte d'une autre région");
        crowd.ask(st, *auditor, honest(m));
        assert_eq!(h.sov_faults(), 0);
        // 2. Deux complices : l'hôte principal glisse un faux, l'autre hôte le confirme ;
        // le relais du monde l'archive (il ne vérifie plus rien de signé), mais le
        // vérificateur le signale et le relais, en revérifiant sa propre copie, écarte les
        // deux.
        let committed = h.sov_committed();
        let b = next_batch(
            st,
            room,
            r0,
            &keys[garant_of[&p0]],
            (&author.0, &author.1),
            true,
        );
        let ack = Request::Ack {
            region: r0,
            epoch: b.epoch,
            seq: b.end(),
            chain: b.chain(),
        };
        crowd.ask(
            st,
            p0,
            Request::Archive {
                region: r0,
                batches: vec![b],
            },
        );
        crowd.ask(st, w0, ack);
        assert!(
            h.sov_committed() > committed,
            "archivé sur la foi du complice"
        );
        let got = audits(&crowd);
        assert_eq!(got.len(), 1);
        let (auditor, m) = &got[0];
        let Request::Audited { fault, .. } = honest(m) else {
            unreachable!()
        };
        assert!(fault.is_some(), "le vérificateur voit le faux");
        crowd.ask(st, *auditor, honest(m));
        assert_eq!(h.sov_faults(), 2, "l'hôte principal et son complice");
        assert!(st.conns[&p0].garant_banned && st.conns[&w0].garant_banned);
        assert!(!st.conns[auditor].garant_banned);
        // 3. Un vérificateur qui invente une faute dans un lot honnête : c'est lui qui est
        // écarté.
        let r1 = regions[1];
        let (p1, _) = hosts_of(st, r1);
        let author1 = (format!("J{}", garant_of[&p1]), keys[garant_of[&p1]].clone());
        let b = next_batch(
            st,
            room,
            r1,
            &keys[garant_of[&p1]],
            (&author1.0, &author1.1),
            false,
        );
        crowd.ask(
            st,
            p1,
            Request::Archive {
                region: r1,
                batches: vec![b],
            },
        );
        let got = audits(&crowd);
        assert_eq!(got.len(), 1);
        let (liar, m) = &got[0];
        let Response::Audit { id, .. } = m else {
            unreachable!()
        };
        crowd.ask(
            st,
            *liar,
            Request::Audited {
                id: *id,
                fault: Some("inventée".into()),
            },
        );
        assert_eq!(h.sov_faults(), 3, "le menteur, pas l'hôte principal");
        assert!(st.conns[liar].garant_banned);
        assert!(!st.conns[&p1].garant_banned);
        // 4. Un vérificateur qui laisse passer un faux que le relais du monde trouve en
        // vérifiant lui-même (une fois sur `audit_self`, ici à chaque fois) : écarté avec
        // l'hôte principal.
        st.audit_self = 1;
        crowd.upkeep(st);
        crowd.sent();
        // (Une région encore souveraine, dont aucun hôte n'a été écarté : celle du menteur,
        // tirée au hasard, a pu perdre la sienne.)
        let r2 = *regions
            .iter()
            .find(|r| {
                **r != r0
                    && **r != r1
                    && st.rooms[&room].sov.contains_key(*r)
                    && st.rooms[&room].shards[*r]
                        .iter()
                        .all(|s| !st.conns[&s.host].garant_banned)
            })
            .expect("une région souveraine intacte");
        let (p2, _) = hosts_of(st, r2);
        let author2 = (format!("J{}", garant_of[&p2]), keys[garant_of[&p2]].clone());
        let b = next_batch(
            st,
            room,
            r2,
            &keys[garant_of[&p2]],
            (&author2.0, &author2.1),
            true,
        );
        crowd.ask(
            st,
            p2,
            Request::Archive {
                region: r2,
                batches: vec![b],
            },
        );
        assert!(
            st.conns[&p2].garant_banned,
            "pris tout de suite par le relais du monde"
        );
        let got = audits(&crowd);
        assert_eq!(got.len(), 1);
        let (lazy, m) = &got[0];
        let Response::Audit { id, .. } = m else {
            unreachable!()
        };
        crowd.ask(
            st,
            *lazy,
            Request::Audited {
                id: *id,
                fault: None,
            },
        );
        assert!(
            st.conns[lazy].garant_banned,
            "« rien à signaler » sur un faux"
        );
        drop(guard);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Battement du monde (v136) : deux fois par seconde, le relais du monde le dit une fois
    /// à l'hôte de chaque relais de région (qui le redit à ses joueurs) et directement aux
    /// seuls joueurs qu'aucun relais de région ne sert ; un changement (pause, reprise)
    /// part tout de suite à tout le monde.
    #[test]
    fn the_world_beat_goes_through_region_relays() {
        let (h, dir) = crowd_world("battement");
        h.set_shard_share(50);
        let mut guard = h.state.lock().unwrap();
        let st = &mut *guard;
        let mut crowd = Crowd::new();
        let (_, room) = host_world(&mut crowd, st);
        let (players, garant_of, _) = sovereign_crowd(st, &mut crowd, room);
        // Les joueurs des trois premières régions sont servis par leur relais, ceux de la
        // quatrième non.
        let regions: Vec<[i32; 2]> = {
            let mut r: Vec<[i32; 2]> = st.rooms[&room].sov.keys().copied().collect();
            r.sort();
            r
        };
        let served = |i: usize| i < 300;
        for (i, &id) in players.iter().enumerate() {
            let at = st.rooms[&room].build.positions[&id];
            let region = proto::region_of(at);
            let regions = if served(i) { vec![region] } else { Vec::new() };
            crowd.ask(st, id, Request::Served { regions });
        }
        assert_eq!(regions.len(), 4);
        crowd.sent();
        let beats = |crowd: &Crowd, id: u64| -> (usize, usize) {
            let mut n = (0, 0);
            for m in crowd.got(id).0 {
                match m {
                    Response::Paused { .. } => n.0 += 1,
                    Response::WorldBeat { room: r, .. } if r == room => n.1 += 1,
                    _ => {}
                }
            }
            n
        };
        guards::send_pause(st, room, false, false);
        for (i, &id) in players.iter().enumerate() {
            let want = if served(i) { 0 } else { 1 };
            assert_eq!(beats(&crowd, id), (want, 0), "joueur {i}");
        }
        let mut hosts = 0;
        for &g in garant_of.keys() {
            let (p, w) = beats(&crowd, g);
            assert_eq!(p, 0);
            hosts += w;
        }
        assert_eq!(
            hosts, 8,
            "une fois à l'hôte de chacun des huit relais de région"
        );
        // La pause commence : tout le monde le sait tout de suite.
        guards::send_pause(st, room, true, true);
        for &id in &players {
            assert_eq!(beats(&crowd, id).0, 1);
        }
        drop(guard);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Essai de charge d'un monde souverain, sans réseau : `n` joueurs par groupes de 100
    /// dans des régions différentes, chacune souveraine avec deux relais de région
    /// (hébergés par des joueurs d'autres régions) qui servent ses joueurs. L'hôte principal
    /// de chaque région publie un lot toutes les 40 ms (deux modifications signées), l'autre
    /// hôte le confirme, un joueur sur quatre change de chunk chaque seconde, et le monde a
    /// des gardiens. Mesure poste par poste le calcul du relais du monde pour une seconde de
    /// jeu, et ce qu'il envoie.
    /// `cargo test --release -p minelib-relay charge_souveraine -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn charge_souveraine() {
        // (`MINELIB_CHARGE=20000` : une seule taille.)
        let sizes: Vec<usize> = std::env::var("MINELIB_CHARGE")
            .ok()
            .and_then(|v| v.parse().ok())
            .map_or(vec![10_000, 60_000, 120_000], |n| vec![n]);
        for n in sizes {
            let (h, dir) = crowd_world("souv");
            h.set_shard_share(50);
            let mut guard = h.state.lock().unwrap();
            let st = &mut *guard;
            let mut crowd = Crowd::new();
            let (_, room) = host_world(&mut crowd, st);
            let side = (n / 100).max(1).isqrt() as i32 + 1;
            let home = |i: usize| -> [i32; 2] {
                let g = (i / 100) as i32;
                let (rx, rz) = (g % side, g / side);
                [
                    rx * 64 + 8 + (i % 10) as i32,
                    rz * 64 + 8 + (i / 10 % 10) as i32,
                ]
            };
            let key = |i: usize| {
                let mut b = [7u8; 32];
                b[..8].copy_from_slice(&(i as u64).to_le_bytes());
                SigningKey::from_bytes(&b)
            };
            let t = Instant::now();
            let mut players = Vec::new();
            let mut keys = Vec::new();
            let mut garant_of = HashMap::new();
            for i in 0..n {
                let name = format!("J{i}");
                let k = key(i);
                let id = crowd.conn(st, &name, false);
                st.conns.get_mut(&id).unwrap().account = Some(k.verifying_key().to_bytes());
                crowd.ask(st, id, Request::Join { room });
                crowd.ask(st, id, Request::Position { chunk: home(i) });
                players.push(id);
                if i % 25 == 7 {
                    let g = crowd.conn(st, &name, true);
                    st.conns.get_mut(&g).unwrap().account = Some(k.verifying_key().to_bytes());
                    garant_of.insert(g, i);
                }
                keys.push(k);
            }
            let join = t.elapsed();
            crowd.upkeep(st);
            let tasks: Vec<([i32; 2], u64)> = st.rooms[&room]
                .shards
                .iter()
                .flat_map(|(r, l)| l.iter().map(|s| (*r, s.host)))
                .collect();
            for (region, host) in &tasks {
                crowd.ask(
                    st,
                    *host,
                    Request::ShardReady {
                        region: *region,
                        node: [0; 32],
                        addr: format!("127.0.0.1:{host}"),
                        key: [1; 32],
                    },
                );
            }
            crowd.upkeep(st);
            crowd.upkeep(st);
            for (i, &id) in players.iter().enumerate() {
                let regions = vec![proto::region_of(home(i))];
                crowd.ask(st, id, Request::Served { regions });
            }
            let regions: Vec<[i32; 2]> = st.rooms[&room].sov.keys().copied().collect();
            crowd.sent();
            // Joueurs de chaque région (leur rang), et certificats.
            let mut by_region: HashMap<[i32; 2], Vec<usize>> = HashMap::new();
            for i in 0..n {
                by_region
                    .entry(proto::region_of(home(i)))
                    .or_default()
                    .push(i);
            }
            let expires = now_secs() + 24 * 3600;
            let certs: Vec<sovereign::Cert> = (0..n)
                .map(|i| {
                    sovereign::Cert::issue(
                        &st.cert_key,
                        room,
                        &format!("J{i}"),
                        keys[i].verifying_key().to_bytes(),
                        expires,
                    )
                })
                .collect();
            let mut seed = 99u64;
            let mut rnd = move || {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                seed
            };
            let mut stamp = now_ms();
            let secs = 2;
            let (mut pos_t, mut arch_t, mut ack_t, mut beat_t, mut up_t) = (
                Duration::ZERO,
                Duration::ZERO,
                Duration::ZERO,
                Duration::ZERO,
                Duration::ZERO,
            );
            let (mut bytes, mut msgs, mut cells, mut batches) = (0, 0, 0, 0usize);
            let (mut audits, mut audit_t, mut misc_t) = (0usize, Duration::ZERO, Duration::ZERO);
            let committed0 = h.sov_committed();
            for _ in 0..secs {
                // Lots de la seconde, préparés d'abord (pas comptés) : l'hôte principal
                // les envoie par paquets toutes les 250 ms, l'autre hôte confirme le dernier.
                let mut work: Vec<(u64, u64, Request, Request)> = Vec::new();
                let mut made = 0;
                for region in &regions {
                    let sv = &st.rooms[&room].sov[region];
                    let (primary, epoch) = (sv.primary, sv.epoch);
                    let (mut first, mut prev) = sv
                        .pending
                        .back()
                        .map_or((sv.base, sv.chain), |p| (p.end, p.chain));
                    let witness = st.rooms[&room].shards[region]
                        .iter()
                        .map(|s| s.host)
                        .find(|&h| h != primary)
                        .unwrap();
                    let pkey = &keys[garant_of[&primary]];
                    let authors = &by_region[region];
                    for group in [6, 6, 6, 7] {
                        let mut batches = Vec::new();
                        for _ in 0..group {
                            let mut records = Vec::new();
                            let mut bcerts = Vec::new();
                            for k in 0..2 {
                                let i = authors[rnd() as usize % authors.len()];
                                let c = home(i);
                                stamp += 1;
                                let edit = sovereign::Edit::sign(
                                    &keys[i],
                                    room,
                                    &format!("J{i}"),
                                    [
                                        c[0] * 16 + (rnd() % 16) as i32,
                                        60 + (rnd() % 30) as i32,
                                        c[1] * 16 + (rnd() % 16) as i32,
                                    ],
                                    1 + (rnd() % 40) as u16,
                                    false,
                                    c,
                                    stamp,
                                    false,
                                );
                                records.push(sovereign::Record {
                                    seq: first + k,
                                    edit,
                                });
                                bcerts.push(certs[i].clone());
                            }
                            let mut batch = sovereign::Batch {
                                room,
                                region: *region,
                                epoch,
                                first,
                                prev,
                                records,
                                certs: bcerts,
                                sig: Vec::new(),
                            };
                            batch.sign(pkey);
                            (first, prev) = (batch.end(), batch.chain());
                            batches.push(batch);
                        }
                        made += batches.len();
                        let ack = Request::Ack {
                            region: *region,
                            epoch,
                            seq: first,
                            chain: prev,
                        };
                        work.push((
                            primary,
                            witness,
                            Request::Archive {
                                region: *region,
                                batches,
                            },
                            ack,
                        ));
                    }
                }
                batches += made;
                st.last_upkeep = Instant::now();
                let t = Instant::now();
                for (i, &id) in players.iter().enumerate() {
                    if rnd() % 4 == 0 {
                        let mut chunk = home(i);
                        chunk[0] += (rnd() % 3) as i32 - 1;
                        crowd.ask(st, id, Request::Position { chunk });
                    }
                }
                pos_t += t.elapsed();
                // Un joueur sur dix redit où il est (`Spot`, toutes les 10 s). (La voix — v137 :
                // bonjours, clés et paquets — passe par les relais de région : plus rien ici.)
                let t = Instant::now();
                for (i, &id) in players.iter().enumerate() {
                    if rnd() % 10 == 0 {
                        let c = home(i);
                        crowd.ask(
                            st,
                            id,
                            Request::Spot {
                                pos: [c[0] as f32 * 16.0, 70.0, c[1] as f32 * 16.0],
                                yaw: 0.0,
                                pitch: 0.0,
                            },
                        );
                    }
                }
                misc_t += t.elapsed();
                // Les lots arrivent au fil de la seconde, chacun suivi de sa confirmation.
                for (primary, witness, archive, ack) in work {
                    let t = Instant::now();
                    crowd.ask(st, primary, archive);
                    arch_t += t.elapsed();
                    let t = Instant::now();
                    crowd.ask(st, witness, ack);
                    ack_t += t.elapsed();
                }
                // Vérifications confiées aux hôtes (surveillance mutuelle) : ils répondent
                // « rien à signaler » (leur calcul à eux n'est pas compté).
                let mut asked = Vec::new();
                for &g in garant_of.keys() {
                    for m in crowd.rx[&g].try_iter() {
                        if let Inbound::Push(b) | Inbound::Loose(b, _) = m {
                            if let Ok(Response::Audit { id, .. }) = postcard::from_bytes(&b) {
                                asked.push((g, id));
                            }
                        }
                    }
                }
                audits += asked.len();
                let t = Instant::now();
                for (g, id) in asked {
                    crowd.ask(st, g, Request::Audited { id, fault: None });
                }
                audit_t += t.elapsed();
                // Battements des gardiens : deux fois par seconde (ceux d'entre eux qui
                // passent encore par ce relais).
                let t = Instant::now();
                for _ in 0..2 {
                    guards::send_pause(st, room, false, false);
                }
                beat_t += t.elapsed();
                let t = Instant::now();
                crowd.upkeep(st);
                up_t += t.elapsed();
                let s = crowd.sent();
                (bytes, msgs, cells) = (bytes + s.0, msgs + s.1, cells + s.2);
            }
            // Sondes d'aller-retour : toutes les 4 s aux garants, toutes les minutes aux
            // joueurs d'un grand monde.
            for c in st.conns.values_mut() {
                c.probed = None;
                c.probe = None;
            }
            let t = Instant::now();
            probe_links(st);
            let all = crowd.sent();
            let garants = garant_of.len();
            let rate = garants as f64 / 4.0 + (all.1 - garants) as f64 / 60.0;
            let share = rate / all.1.max(1) as f64;
            let probe = t.elapsed().mul_f64(share);
            let probes = (
                (all.0 as f64 * share) as usize * 4,
                (all.1 as f64 * share) as usize * 4,
                0,
            );
            let committed = h.sov_committed() - committed0;
            let per = |d: Duration| d.as_secs_f64() * 1e3 / secs as f64;
            let total = per(pos_t)
                + per(misc_t)
                + per(arch_t)
                + per(ack_t)
                + per(audit_t)
                + per(beat_t)
                + per(up_t)
                + probe.as_secs_f64() * 1e3;
            eprintln!(
                "SOUVERAIN {n:>6} joueurs, {} régions souveraines, {} lots/s, {} \
                 modifications archivées/s, {} vérifications confiées/s | arrivée {:.1} µs par \
                 joueur | une seconde de jeu : {total:.0} ms de calcul = positions {:.0} + \
                 retours et voix {:.0} + lots {:.0} + confirmations {:.0} + vérifications {:.0} \
                 + battements {:.0} + sondes {:.0} + entretien {:.0} | envoyé par seconde : {} \
                 messages ({} sondes), {} blocs, {:.2} Mbit/s",
                regions.len(),
                batches / secs,
                committed / secs,
                audits / secs,
                join.as_secs_f64() * 1e6 / n as f64,
                per(pos_t),
                per(misc_t),
                per(arch_t),
                per(ack_t),
                per(audit_t),
                per(beat_t),
                probe.as_secs_f64() * 1e3,
                per(up_t),
                msgs / secs + probes.1 / 4,
                probes.1 / 4,
                cells / secs,
                (bytes / secs + probes.0 / 4) as f64 * 8.0 / 1e6,
            );
            drop(guard);
            drop(h);
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    /// Essai de charge avec provinces (v138), sans réseau : comme `charge_souveraine`, mais
    /// chaque province peuplée (4 × 4 régions) est confiée à un relais de province, qui met
    /// en ordre les lots de ses régions souveraines et n'envoie au relais du monde qu'une
    /// copie de ce qu'il publie (toutes les 100 ms) et ses baux (toutes les 300 ms). Mesure
    /// ce qui reste au relais du monde pour une seconde de jeu.
    /// `cargo test --release -p minelib-relay charge_provinces -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn charge_provinces() {
        let sizes: Vec<usize> = std::env::var("MINELIB_CHARGE")
            .ok()
            .and_then(|v| v.parse().ok())
            .map_or(vec![10_000, 60_000, 120_000], |n| vec![n]);
        for n in sizes {
            let (h, dir) = crowd_world("prov");
            h.set_shard_share(50);
            h.set_provinces(proto::PROVINCE as usize, 64);
            let mut guard = h.state.lock().unwrap();
            let st = &mut *guard;
            let mut crowd = Crowd::new();
            let (_, room) = host_world(&mut crowd, st);
            let side = (n / 100).max(1).isqrt() as i32 + 1;
            let home = |i: usize| -> [i32; 2] {
                let g = (i / 100) as i32;
                let (rx, rz) = (g % side, g / side);
                [
                    rx * 64 + 8 + (i % 10) as i32,
                    rz * 64 + 8 + (i / 10 % 10) as i32,
                ]
            };
            let t = Instant::now();
            let mut players = Vec::new();
            let mut garants = 0;
            for i in 0..n {
                let name = format!("J{i}");
                let id = crowd.conn(st, &name, false);
                crowd.ask(st, id, Request::Join { room });
                crowd.ask(st, id, Request::Position { chunk: home(i) });
                players.push(id);
                if i % 25 == 7 {
                    crowd.conn(st, &name, true);
                    garants += 1;
                }
            }
            let join = t.elapsed();
            // Relais de région, puis provinces confiées ; leurs hôtes sont prêts, leurs
            // relais de région (rattachés à elles) aussi.
            crowd.upkeep(st);
            let provs: Vec<([i32; 2], u64, u64)> = st.rooms[&room]
                .provinces
                .iter()
                .map(|(p, r)| (*p, r.host, r.epoch))
                .collect();
            for &(p, host, epoch) in &provs {
                crowd.ask(
                    st,
                    host,
                    Request::ProvinceReady {
                        room,
                        province: p,
                        epoch,
                        node: [0; 32],
                        addr: format!("127.0.0.1:{host}"),
                        key: [2; 32],
                        have: 0,
                    },
                );
            }
            crowd.upkeep(st);
            let size = proto::PROVINCE;
            for &(p, host, epoch) in &provs {
                let ready: Vec<([i32; 2], u16)> = st.rooms[&room]
                    .shards
                    .iter()
                    .filter(|(r, _)| proto::province_of(**r, size) == p)
                    .flat_map(|(r, l)| l.iter().map(|s| (*r, s.slot)))
                    .collect();
                crowd.ask(
                    st,
                    host,
                    Request::ProvinceShards {
                        room,
                        province: p,
                        epoch,
                        ready,
                    },
                );
            }
            crowd.upkeep(st);
            for (i, &id) in players.iter().enumerate() {
                let regions = vec![proto::region_of(home(i))];
                crowd.ask(st, id, Request::Served { regions });
            }
            crowd.sent();
            let delegated = st.rooms[&room]
                .provinces
                .values()
                .filter(|p| p.info.is_some())
                .count();
            // Joueurs de chaque province.
            let mut by_prov: HashMap<[i32; 2], Vec<usize>> = HashMap::new();
            for i in 0..n {
                by_prov
                    .entry(proto::province_of(proto::region_of(home(i)), size))
                    .or_default()
                    .push(i);
            }
            let mut seq = st.rooms[&room].build.last_seq() + 1;
            let mut seed = 77u64;
            let mut rnd = move || {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                seed
            };
            let secs = 2;
            let (mut pos_t, mut misc_t, mut arch_t, mut lease_t, mut beat_t, mut up_t) = (
                Duration::ZERO,
                Duration::ZERO,
                Duration::ZERO,
                Duration::ZERO,
                Duration::ZERO,
                Duration::ZERO,
            );
            let (mut bytes, mut msgs, mut cells, mut entries_n) = (0, 0, 0, 0usize);
            let mut next_n = 1u64;
            // (v140) Joueurs de chaque région, comme les provinces les comptent dans leurs
            // baux.
            let mut census: HashMap<[i32; 2], Vec<([i32; 2], u32)>> = HashMap::new();
            for (p, list) in &by_prov {
                let mut per: HashMap<[i32; 2], u32> = HashMap::new();
                for &i in list {
                    *per.entry(proto::region_of(home(i))).or_default() += 1;
                }
                census.insert(*p, per.into_iter().collect());
            }
            for sec in 0..secs {
                // Copies de la seconde, préparées d'abord (pas comptées) : un bloc toutes
                // les 2 s par joueur, envoyés par chaque province toutes les 100 ms.
                let mut work: Vec<(u64, Request)> = Vec::new();
                for &(p, host, epoch) in &provs {
                    let Some(authors) = by_prov.get(&p) else {
                        continue;
                    };
                    let per = authors.len() / 2;
                    for k in 0..10 {
                        let mut entries = Vec::new();
                        for _ in (per * k / 10)..(per * (k + 1) / 10) {
                            let i = authors[rnd() as usize % authors.len()];
                            let c = home(i);
                            let pos = [
                                c[0] * 16 + (rnd() % 16) as i32,
                                60 + (rnd() % 30) as i32,
                                c[1] * 16 + (rnd() % 16) as i32,
                            ];
                            seq += 1;
                            let data = game::encode(&game::BuildMsg::Set {
                                pos,
                                block: 1 + (rnd() % 40) as u16,
                                by: format!("J{i}"),
                                seq,
                            });
                            entries.push((proto::zone_of(c), seq, data));
                        }
                        entries_n += entries.len();
                        // (v142) Grand monde : la copie est gardée par le relais de secours de
                        // la province ; il ne vient ici que les sacs et les retours.
                        if std::env::var("MINELIB_GRAND").is_ok() {
                            entries.clear();
                        }
                        // (v140) Positions de retour de ses joueurs, toutes les 2 s, avec
                        // ce qu'elle publie.
                        let spots: Vec<(String, [f32; 5])> = if k == 0 && sec % 2 == 0 {
                            authors
                                .iter()
                                .map(|&i| {
                                    let c = home(i);
                                    (
                                        format!("J{i}"),
                                        [c[0] as f32 * 16.0, 70.0, c[1] as f32 * 16.0, 0.0, 0.0],
                                    )
                                })
                                .collect()
                        } else {
                            Vec::new()
                        };
                        work.push((
                            host,
                            Request::ProvinceArchive {
                                room,
                                province: p,
                                epoch,
                                n: next_n,
                                entries,
                                bags: Vec::new(),
                                spots,
                                loose: Vec::new(),
                                dump: 0,
                            },
                        ));
                        next_n += 1;
                    }
                }
                st.last_upkeep = Instant::now();
                // (v140) Au fond de leur province, les joueurs ne redisent plus ici où ils
                // sont (seulement en changeant de province : jamais dans cet essai), ni leur
                // position de retour (elle vient avec ce que publie leur province).
                let t = Instant::now();
                for (i, &id) in players.iter().enumerate() {
                    let deep = st.rooms[&room]
                        .delegated
                        .contains(&proto::province_of(proto::region_of(home(i)), size));
                    if rnd() % 4 == 0 && !deep {
                        let mut chunk = home(i);
                        chunk[0] += (rnd() % 3) as i32 - 1;
                        crowd.ask(st, id, Request::Position { chunk });
                    }
                }
                pos_t += t.elapsed();
                let t = Instant::now();
                misc_t += t.elapsed();
                let t = Instant::now();
                for (host, req) in work {
                    crowd.ask(st, host, req);
                }
                arch_t += t.elapsed();
                // Baux : toutes les 300 ms par province.
                let t = Instant::now();
                for _ in 0..3 {
                    for &(p, host, epoch) in &provs {
                        crowd.ask(
                            st,
                            host,
                            Request::ProvinceLease {
                                room,
                                province: p,
                                epoch,
                                t: now_ms(),
                                counts: census.get(&p).cloned().unwrap_or_default(),
                                inside: Vec::new(),
                                followed: false,
                                wants: Vec::new(),
                                children: None,
                                arrived: Vec::new(),
                                left: Vec::new(),
                                seq: 0,
                                total: 100,
                            },
                        );
                    }
                }
                lease_t += t.elapsed();
                let t = Instant::now();
                for _ in 0..2 {
                    guards::send_pause(st, room, false, false);
                }
                beat_t += t.elapsed();
                let t = Instant::now();
                crowd.upkeep(st);
                up_t += t.elapsed();
                let s = crowd.sent();
                (bytes, msgs, cells) = (bytes + s.0, msgs + s.1, cells + s.2);
            }
            for c in st.conns.values_mut() {
                c.probed = None;
                c.probe = None;
            }
            let t = Instant::now();
            probe_links(st);
            let all = crowd.sent();
            let rate = garants as f64 / 4.0 + (all.1 - garants) as f64 / 60.0;
            let share = rate / all.1.max(1) as f64;
            let probe = t.elapsed().mul_f64(share);
            let probes = (
                (all.0 as f64 * share) as usize * 4,
                (all.1 as f64 * share) as usize * 4,
            );
            let per = |d: Duration| d.as_secs_f64() * 1e3 / secs as f64;
            let total = per(pos_t)
                + per(misc_t)
                + per(arch_t)
                + per(lease_t)
                + per(beat_t)
                + per(up_t)
                + probe.as_secs_f64() * 1e3;
            eprintln!(
                "PROVINCES {n:>6} joueurs, {} provinces confiées sur {}, {} modifications \
                 gardées/s | arrivée {:.1} µs par joueur | une seconde de jeu : {total:.0} ms de \
                 calcul = positions {:.0} + retours {:.0} + copies des provinces {:.0} + baux \
                 {:.0} + battements {:.0} + sondes {:.0} + entretien {:.0} | envoyé par \
                 seconde : {} messages ({} sondes), {} blocs, {:.2} Mbit/s",
                delegated,
                by_prov.len(),
                entries_n / secs,
                join.as_secs_f64() * 1e6 / n as f64,
                per(pos_t),
                per(misc_t),
                per(arch_t),
                per(lease_t),
                per(beat_t),
                probe.as_secs_f64() * 1e3,
                per(up_t),
                msgs / secs + probes.1 / 4,
                probes.1 / 4,
                cells / secs,
                (bytes / secs + probes.0 / 4) as f64 * 8.0 / 1e6,
            );
            drop(guard);
            drop(h);
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}
