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
mod hardware;
mod limits;
mod maps;
mod registry;
mod servers;
pub mod shard;
mod stats;
mod zones;

use std::collections::{HashSet, VecDeque};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use ed25519_dalek::{Signature, SigningKey, Verifier, VerifyingKey};
use minelib_net::proto::{self, ErrorCode, Request, Response};
use minelib_net::transport::{self, Channel, Inbound, Incoming};
use minelib_net::{crypto, game, totp};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
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
    /// État du relais (essais seulement).
    #[cfg(test)]
    state: Arc<Mutex<State>>,
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
    tx: Sender<Inbound>,
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
}

impl Conn {
    /// Une connexion qui vient d'arriver (personne n'y est encore connecté à un compte).
    fn new(tx: Sender<Inbound>) -> Self {
        Self {
            tx,
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
        }
    }
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
    /// Construction : garants de zones.
    build: zones::Build,
    /// FPS : garant de secours désigné par l'hôte.
    backup: Option<u64>,
    /// Garants indépendants en attente (membres cachés, pas des joueurs) et prêts.
    hidden: Vec<u64>,
    ready_standby: Vec<u64>,
    /// La partie est servie par un garant indépendant (pas par un joueur).
    garant_hosted: bool,
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
}

impl Room {
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
}

/// Démarre le relais (fil d'écoute en arrière-plan).
pub fn start(cfg: Config) -> Result<Handle> {
    std::fs::create_dir_all(&cfg.dir)
        .with_context(|| format!("dossier du relais {}", cfg.dir.display()))?;
    let id = Identity::load_or_create(&cfg.dir)?;
    let signer = SigningKey::from_bytes(&id.sign_seed);
    let registry = Registry::open(&cfg.dir.join("registry.log"), signer)?;
    let accounts = accounts::Store::new(cfg.dir.join("accounts"))?;
    let stats = stats::Store::open(cfg.dir.join("stats"))?;
    let cfg_dir = cfg.dir.clone();
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
    let state = Arc::new(Mutex::new(State {
        whois: whois.clone(),
        peer_servers: peer_servers.clone(),
        shard_from: shard_from.clone(),
        shard_share: shard_share.clone(),
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
    }));
    #[cfg(test)]
    let state_for_tests = state.clone();
    std::thread::Builder::new()
        .name("relais : écoute".into())
        .spawn(move || {
            // Connexions ouvertes par adresse : au-delà de `MAX_PER_PEER`, les suivantes
            // sont refusées (un seul PC ne peut pas épuiser le relais en connexions).
            let open: Arc<Mutex<HashMap<String, usize>>> = Arc::default();
            for conn in listener.incoming() {
                let Ok(stream) = conn else { continue };
                let state = state.clone();
                let open = open.clone();
                let _ = std::thread::Builder::new()
                    .name("relais : client".into())
                    .spawn(move || {
                        let ip = stream.peer_addr().map(|a| a.ip()).ok();
                        let mut peer = ip.map(|a| a.to_string()).unwrap_or_default();
                        let _slot = match PeerSlot::take(&open, &peer, ip) {
                            Some(s) => s,
                            None => return,
                        };
                        // Par le tunnel iroh, le joueur arrive de ce PC : le tunnel dit
                        // d'abord qui il est vraiment.
                        if ip.is_some_and(|a| a.is_loopback()) {
                            match tunnel_peer(&stream) {
                                Ok(Some(who)) => peer = who,
                                Ok(None) => {}
                                Err(_) => return,
                            }
                        }
                        if let Err(e) = serve(stream, &state, &peer) {
                            log::debug!("client {peer} : {e:#}");
                        }
                    });
            }
        })?;
    Ok(Handle {
        addr,
        public,
        whois,
        peer_servers,
        shard_from,
        shard_share,
        #[cfg(test)]
        state: state_for_tests,
    })
}

/// Connexions ouvertes en même temps par une même adresse.
const MAX_PER_PEER: usize = 48;
/// Connexions ouvertes en tout (au-delà, les nouvelles sont refusées).
const MAX_CONNECTIONS: usize = 60_000;

/// Place prise par une connexion (rendue quand elle se ferme).
pub(crate) struct PeerSlot {
    open: Arc<Mutex<HashMap<String, usize>>>,
    key: String,
}

impl PeerSlot {
    fn take(
        open: &Arc<Mutex<HashMap<String, usize>>>,
        peer: &str,
        ip: Option<std::net::IpAddr>,
    ) -> Option<Self> {
        let mut m = open.lock().unwrap();
        let total: usize = m.values().sum();
        // Ce PC (tunnel iroh, relais intégré, essais) : pas de limite par adresse, le
        // tunnel a les siennes.
        let local = ip.is_none_or(|a| a.is_loopback());
        let n = m.entry(peer.to_string()).or_insert(0);
        if total >= MAX_CONNECTIONS || (!local && *n >= MAX_PER_PEER) {
            return None;
        }
        *n += 1;
        Some(Self {
            open: open.clone(),
            key: peer.to_string(),
        })
    }
}

impl PeerSlot {
    /// Comme `take`, pour un joueur nommé par le tunnel (toujours limité).
    pub(crate) fn take_named(open: &Arc<Mutex<HashMap<String, usize>>>, who: &str) -> Option<Self> {
        let mut m = open.lock().unwrap();
        let total: usize = m.values().sum();
        let n = m.entry(who.to_string()).or_insert(0);
        if total >= MAX_CONNECTIONS || *n >= MAX_PER_PEER {
            return None;
        }
        *n += 1;
        Some(Self {
            open: open.clone(),
            key: who.to_string(),
        })
    }
}

impl Drop for PeerSlot {
    fn drop(&mut self) {
        let mut m = self.open.lock().unwrap();
        if let Some(n) = m.get_mut(&self.key) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                m.remove(&self.key);
            }
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
}

fn err(code: ErrorCode, message: impl Into<String>) -> Response {
    Response::Error {
        code,
        message: message.into(),
    }
}

fn serve(stream: TcpStream, state: &Mutex<State>, peer: &str) -> Result<()> {
    let private = state.lock().unwrap().id.noise_private;
    let mut chan = Channel::accept(stream, &private, Duration::from_secs(10))?;
    let id = {
        let mut st = state.lock().unwrap();
        let id = st.next_id;
        st.next_id += 1;
        st.conns.insert(id, Conn::new(chan.pusher()));
        id
    };
    let result = serve_loop(&mut chan, state, peer, id);
    let mut st = state.lock().unwrap();
    party_leave(&mut st, id);
    leave_room(&mut st, id);
    st.maps.forget(id);
    st.conns.remove(&id);
    result
}

fn serve_loop(chan: &mut Channel, state: &Mutex<State>, peer: &str, id: u64) -> Result<()> {
    let mut session = Session::Anonymous;
    let mut hello = false;
    loop {
        // Le jeu envoie un ping toutes les 20 s : 60 s sans rien, la connexion est morte.
        let req: Request = match chan.recv_any(Duration::from_secs(60))? {
            None => return Ok(()),
            Some(Incoming::Push(bytes)) => {
                chan.send_raw(&bytes)?;
                continue;
            }
            Some(Incoming::Msg(r)) => r,
        };
        if state
            .lock()
            .unwrap()
            .conns
            .get(&id)
            .is_some_and(|c| c.kicked)
        {
            chan.send(&err(ErrorCode::Banned, "Compte banni."))?;
            return Ok(());
        }
        let resp = if !hello {
            match req {
                Request::Hello { protocol, client } if protocol == proto::PROTOCOL => {
                    hello = true;
                    let mut st = state.lock().unwrap();
                    if let Some(c) = st.conns.get_mut(&id) {
                        c.garant_only = client.starts_with("minelib-garant");
                        c.version = proto::client_version(&client);
                    }
                    Some(Response::Hello {
                        protocol: proto::PROTOCOL,
                        relay: st.cfg.name.clone(),
                        pow_bits: st.cfg.pow_bits,
                    })
                }
                Request::Hello { .. } => Some(err(ErrorCode::Protocol, "")),
                _ => Some(err(ErrorCode::Protocol, "bonjour attendu")),
            }
        } else if let Some(taken) = taken_elsewhere(&req, state) {
            Some(taken)
        } else {
            handle(req, state, &mut session, peer, id)
        };
        if let Some(r) = resp {
            chan.send(&r)?;
        }
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
        room.hidden.retain(|&h| h != id);
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
            room.build.left(id);
            room.shards_sent.remove(&id);
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
                room.build.left(id);
                room.shards_sent.remove(&id);
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
            room.build.left(id);
            room.shards_sent.remove(&id);
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
    room.hidden.retain(|&h| h != new);
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
    let hidden: Vec<u64> = room.hidden.clone();
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
    let mut touched = Vec::new();
    for (pos, block) in cells {
        if !(0..256).contains(&pos[1]) {
            continue;
        }
        // (Son auteur les a déjà : pas de confirmation, la réponse `BulkDone` suffit.)
        publish_block(st, rid, id, false, pseudo, pos, block);
        touched.push(pos);
    }
    // Un coffre remplacé par WorldEdit : son contenu tombe au sol.
    for pos in touched {
        spill_chest(st, rid, pos);
    }
    String::new()
}

/// Auditeurs d'un message vocal, au plus.
const VOICE_MAX: usize = 256;

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
pub(crate) fn push_zone(tx: &Sender<Inbound>, chunk: [i32; 2], data: Vec<u8>) {
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

/// Envoie à un joueur ce qui lui manque des zones proches de `chunk` (les blocs des
/// régions qu'un relais de région lui sert n'en font pas partie).
fn send_catch_up(st: &mut State, rid: u64, id: u64, chunk: [i32; 2], moved: bool) {
    let served = served_regions(st, rid, id);
    let Some(room) = st.rooms.get_mut(&rid) else {
        return;
    };
    let blocks = |zone: [i32; 2]| !served.contains(&zone_region(zone));
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
fn publish_block(
    st: &mut State,
    rid: u64,
    author: u64,
    confirm: bool,
    pseudo: &str,
    pos: [i32; 3],
    block: u16,
) {
    let chunk = [pos[0].div_euclid(16), pos[2].div_euclid(16)];
    let region = proto::region_of(chunk);
    let Some(room) = st.rooms.get_mut(&rid) else {
        return;
    };
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
    let before = st.rooms.get(&rid).and_then(|r| r.build.block_at(pos));
    publish_block(st, rid, id, true, pseudo, pos, block);
    // Survie : un bloc cassé donne un crédit de minage, un bloc posé sort du sac. Pas pour
    // l'écoulement d'un fluide (de l'eau qui sèche n'est pas du minage), ni pour une case
    // que l'historique dit déjà vide (« casser » dix fois le même vide).
    if !fluid && !(block == 0 && before == Some(0)) {
        note_block(st, rid, pseudo, block);
    }
    spill_chest(st, rid, pos);
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
    for c in st.conns.values_mut().filter(|c| c.pseudo.is_some()) {
        if let Some((_, at)) = c.probe {
            if at.elapsed() < Duration::from_secs(6) {
                continue;
            }
            c.probe = None;
            probe_sample(c, 6000.0);
        }
        if c.probed
            .is_none_or(|t| t.elapsed() >= Duration::from_secs(4))
        {
            let nonce = rand::rngs::OsRng.next_u32();
            c.probe = Some((nonce, Instant::now()));
            c.probed = Some(Instant::now());
            transport::push(&c.tx, &Response::Probe { nonce });
        }
    }
}

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
    let shard_hosts: Vec<u64> = room.shards.values().flatten().map(|s| s.host).collect();
    let best = room
        .members
        .iter()
        .filter(|m| !room.hidden.contains(m) && !shard_hosts.contains(m))
        .filter_map(|&m| Some((m, rtt(m)?)))
        .filter(|&(_, r)| r <= GOOD_RTT && r * 2.0 <= h)
        .min_by(|a, b| a.1.total_cmp(&b.1));
    let Some((best, r)) = best else {
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
            for members in r.build.crowded_chunks() {
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
        room.hidden.push(g);
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
            ..
        } => Some(game::encode(&game::BuildMsg::Player {
            pseudo: pseudo.to_string(),
            pos,
            yaw,
            pitch,
            crouch,
            speed,
        })),
        _ => None,
    }
}

/// Parties ouvertes sur ce relais (liste des serveurs).
fn local_servers(st: &State, kind: u8) -> Vec<proto::ServerEntry> {
    let mut list: Vec<proto::ServerEntry> = st
        .rooms
        .iter()
        .filter(|(_, r)| r.kind == kind && !(kind == 0 && r.started))
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
            players: r.player_count() as u32,
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
    if in_survival(st, &server, &creator, by) {
        st.bags.block_set(&server, by, block);
    }
}

/// Aux hôtes des relais d'une région : la liste des relais prêts (ils se relient entre
/// eux, et le plus petit rang met les modifications en ordre).
fn announce_shard_peers(st: &State, rid: u64, region: [i32; 2]) {
    let Some(list) = st.rooms.get(&rid).and_then(|r| r.shards.get(&region)) else {
        return;
    };
    let ready: Vec<proto::ShardInfo> = list.iter().filter_map(|s| s.info.clone()).collect();
    for sh in list {
        if let Some(c) = st.conns.get(&sh.host) {
            transport::push(
                &c.tx,
                &Response::ShardPeers {
                    region,
                    list: ready.clone(),
                },
            );
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
type PlayersIndex = HashMap<String, (u64, Option<u64>)>;

fn players_index(st: &State) -> PlayersIndex {
    st.conns
        .iter()
        .filter(|(_, c)| !c.garant_only)
        .filter_map(|(&id, c)| Some((c.pseudo.as_deref()?.to_lowercase(), (id, c.room))))
        .collect()
}

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
    let Some(room) = st.rooms.get_mut(&rid) else {
        return;
    };
    let Some(&chunk) = room.build.positions.get(&id) else {
        return;
    };
    let list: Vec<proto::ShardInfo> = regions_in_view(chunk)
        .filter_map(|r| room.shards.get(&r))
        .flatten()
        .filter_map(|s| s.info.clone())
        .collect();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for i in &list {
        (i.region, i.slot, i.node, &i.addr, i.key).hash(&mut h);
    }
    // (Jamais zéro : zéro veut dire « rien reçu », et une liste vide ne s'envoie pas à
    // qui n'a jamais rien reçu.)
    let print = h.finish() | 1;
    let had = room.shards_sent.get(&id).copied().unwrap_or(0);
    if had == print || (had == 0 && list.is_empty()) {
        return;
    }
    room.shards_sent.insert(id, print);
    let secret = room.shard_secret;
    let Some(c) = st.conns.get(&id) else { return };
    let pseudo = c.pseudo.clone().unwrap_or_default();
    let expires = now_secs() + 24 * 3600;
    transport::push(
        &c.tx,
        &Response::ShardTicket {
            room: rid,
            mac: proto::shard_mac(&secret, rid, &pseudo, expires),
            pseudo,
            expires,
        },
    );
    transport::push(&c.tx, &Response::Shards { list });
}

/// L'hôte d'un relais de région doit-il passer la main ? Oui si son joueur n'est plus en
/// jeu, si sa partie FPS approche de sa fin, ou s'il est entré dans la région qu'il sert.
/// (Avant la v128 la question était celle des garants FPS : un hôte qui jouait dans un
/// monde de construction était toujours « sur le départ », et remplacé à chaque entretien
/// dès qu'un autre hôte était libre.)
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
                if proto::region_of(*at) == region {
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
    let Some(room) = st.rooms.get(&rid) else {
        return;
    };
    let from = st
        .shard_from
        .load(std::sync::atomic::Ordering::Relaxed)
        .max(1);
    // Joueurs par région (positions annoncées).
    let mut count: HashMap<[i32; 2], usize> = HashMap::new();
    for chunk in room.build.positions.values() {
        *count.entry(proto::region_of(*chunk)).or_default() += 1;
    }
    // (La plupart des mondes : aucune région peuplée, aucun relais, rien à faire.)
    if room.shards.is_empty() && room.shards_dirty.is_empty() && count.values().all(|&n| n < from) {
        return;
    }
    let share = st
        .shard_share
        .load(std::sync::atomic::Ordering::Relaxed)
        .max(1);
    if index.is_none() {
        *index = Some(players_index(st));
    }
    let index = index.as_ref().expect("index");
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
            if sh.info.is_none() || replaced || !host_leaving(st, index, rid, r, sh.host) {
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
    let secret = room.shard_secret;
    for (region, previous) in wanted {
        // Plus d'hôte libre : la région reste servie par ce relais (ou par les relais
        // qu'elle a déjà) jusqu'à ce qu'il y en ait un.
        let Some(g) = pick_shard_host(st, index, rid, region) else {
            break;
        };
        let room = st.rooms.get_mut(&rid).expect("partie");
        room.members.push(g);
        room.hidden.push(g);
        let slot = room.shard_slot;
        room.shard_slot = room.shard_slot.wrapping_add(1);
        room.shards.entry(region).or_default().push(RegionShard {
            host: g,
            slot,
            info: None,
            previous,
        });
        let history: Vec<([i32; 2], Vec<Vec<u8>>)> = room.build.logs_in(|zone| {
            proto::region_of([zone[0] * proto::ZONE, zone[1] * proto::ZONE]) == region
        });
        if let Some(c) = st.conns.get_mut(&g) {
            c.room = Some(rid);
            transport::push(
                &c.tx,
                &Response::ShardTask {
                    room: rid,
                    region,
                    secret,
                    slot,
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
    let players: Vec<u64> = room
        .build
        .positions
        .iter()
        .filter(|(_, c)| regions_in_view(**c).any(|r| dirty.contains(&r)))
        .map(|(&m, _)| m)
        .collect();
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
        // Propriétaire à la connexion pourrie : le monde passe au mieux connecté.
        rebalance_host(st, rid);
        upkeep_regions(st, rid, &mut index);
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
            build: zones::Build::default(),
            backup: None,
            hidden: Vec::new(),
            ready_standby: Vec::new(),
            garant_hosted: false,
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
        },
    );
    if let Some(c) = st.conns.get_mut(&id) {
        c.room = Some(rid);
        c.joined = Instant::now();
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
            let mut list = local_servers(st, kind);
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
                    // Heure changée : à tous les joueurs du serveur.
                    if matches!(act, servers::Action::TimeSet | servers::Action::TimeFix) {
                        let (hours, fixed) = st.servers.time(&server);
                        for c in st.conns.values().filter(|c| c.room == Some(rid)) {
                            transport::push(&c.tx, &Response::SetTime { hours, fixed });
                        }
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
                send_catch_up(st, rid, id, chunk, true);
                // Les relais des régions maintenant en vue, s'ils ont changé pour lui.
                send_shards(st, rid, id);
            }
            return None;
        }
        Request::Served { regions } => {
            let rid = my_room.filter(|r| st.rooms.get(r).is_some_and(|r| r.kind == 1))?;
            if let Some(c) = st.conns.get_mut(&id) {
                c.served = regions.into_iter().take(16).collect();
            }
            // Une région qu'un relais ne lui sert plus : ce qui lui manque, tout de suite.
            let at = st.rooms.get(&rid)?.build.positions.get(&id).copied()?;
            send_catch_up(st, rid, id, at, false);
            return None;
        }
        Request::ToZone { chunk, data } => {
            let rid = my_room.filter(|r| st.rooms.get(r).is_some_and(|r| r.kind == 1))?;
            let room = st.rooms.get_mut(&rid).expect("partie");
            let msg = game::decode::<game::BuildMsg>(&data);
            // Objets au sol et coffres : ce relais arbitre (une demande après l'autre),
            // publie le résultat aux joueurs proches, ou refuse au seul demandeur.
            if let Some(m) = msg.as_ref().filter(|m| minelib_net::items::is_request(m)) {
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
                let room = st.rooms.get(&rid).expect("partie");
                let (server, creator) = (room.server.clone(), room.creator.clone());
                let items = &room.build.items;
                let fresh = !items.is_duplicate(m, pseudo);
                let (spend, gain) = match *m {
                    game::BuildMsg::DropItem { pile, .. }
                    | game::BuildMsg::ChestPut { pile, .. } => (Some(pile), None),
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
                    && spend.is_some_and(|p| {
                        !st.bags.can_spend(&server, pseudo, p.item, p.count as u32)
                    });
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
            if let Some(game::BuildMsg::Edit { pos, block, fluid }) = msg {
                build_edit(st, rid, id, pseudo, chunk, pos, block, fluid);
            }
            return None;
        }
        Request::ShardReady {
            region,
            node,
            addr,
            key,
        } => {
            let rid = my_room?;
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
            return None;
        }
        // Sans effet depuis la v127 : ce relais ne croit plus un hôte sur parole (il publie
        // lui-même chaque modification, les relais de région ne font que transmettre).
        Request::RegionLog { .. } => return None,
        Request::Nearby { chunk, data } => {
            // Position sur ce serveur, gardée pour son retour.
            if let Some(game::BuildMsg::Player {
                pos, yaw, pitch, ..
            }) = game::decode::<game::BuildMsg>(&data)
            {
                if let Some(server) = my_room
                    .and_then(|r| st.rooms.get(&r))
                    .filter(|r| r.kind == 1)
                    .map(|r| r.server.clone())
                {
                    st.servers
                        .set_position(&server, pseudo, [pos[0], pos[1], pos[2], yaw, pitch]);
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
            for t in to.into_iter().take(VOICE_MAX) {
                let t = if fps && t == 0 { room.host } else { t };
                if t != id && (room.host == t || room.members.contains(&t)) {
                    if let Some(c) = st.conns.get(&t) {
                        transport::push(&c.tx, &msg);
                        heard.push(t);
                    }
                }
            }
            // Fréquentation : parler à quelqu'un compte, au plus une fois par 30 s et par
            // auditeur.
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
            let error = bulk_edit(st, my_room, id, pseudo, cells);
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
            | Request::RegionLog { .. }
            | Request::RoomStarted { .. }
            | Request::Join { .. }
            | Request::Leave
            | Request::ToHost { .. }
            | Request::SetBackup { .. }
            | Request::Rtt { .. }
            | Request::Echo { .. }
            | Request::BulkEdit { .. }
            | Request::Served { .. }
            | Request::GarantAvailable { .. }
            | Request::GarantReady
            | Request::Conflict { .. }
            | Request::Vote { .. }
            | Request::Voice { .. }
            | Request::ToPeer { .. }
            | Request::Broadcast { .. }
    );
    if room_request {
        let Session::LoggedIn { pseudo } = &*session else {
            return (!req.is_oneway()).then(|| err(ErrorCode::NotLoggedIn, ""));
        };
        let pseudo = pseudo.clone();
        return handle_room(req, st, id, &pseudo);
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
            list: local_servers(st, kind),
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
            let st = h.state.lock().unwrap();
            st.rooms[&room].build.author_at(pos)
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
        // puis le compte rendu d'un relais de région : personne ne reçoit rien, et la
        // modification suivante (une vraie) porte le rang suivant.
        let forged = game::BuildMsg::Set {
            pos: [2, 64, 1],
            block: 9,
            by: "Createur".into(),
            seq: u64::MAX,
        };
        ask(&elink, [0, 0], &forged);
        transport::push(
            &elink,
            &Request::RegionLog {
                entries: vec![([0, 0], game::encode(&forged))],
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
            h.state.lock().unwrap().rooms[&room]
                .build
                .author_at([1, 70, 2])
                .as_deref(),
            Some("Createur")
        );
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

    /// Un monde de construction ouvert par « Proprio », sur un relais sans réseau.
    fn crowd_world(tag: &str) -> (Handle, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("minelib-{tag}-{}", rand::random::<u32>()));
        let h = start(Config {
            dir: dir.clone(),
            bind: "127.0.0.1:0".into(),
            pow_bits: 4,
            name: "essai".into(),
        })
        .unwrap();
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
        for n in [2_000usize, 10_000, 30_000, 60_000] {
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
}
