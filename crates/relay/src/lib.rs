//! Relais Minelib : comptes (inscription, connexion, double authentification) et, plus
//! tard, transmission des parties. Lancé seul (`minelib-relay`) ou dans le jeu pour jouer
//! sur un seul PC.
//!
//! Pas de base de données : un registre public chaîné et signé (`registry`) pour les
//! pseudos et les bans, et un fichier par compte pour ce qui ne doit pas être public
//! (preuve de mot de passe, coffre chiffré, secret 2FA chiffré).

mod accounts;
mod hardware;
mod limits;
mod maps;
mod registry;
mod servers;
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
}

/// Interroge les autres relais : clés des comptes qui y portent ce pseudo.
pub type WhoisFn = Arc<dyn Fn(&str) -> Vec<[u8; 32]> + Send + Sync>;

impl Handle {
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
    /// Disponible comme garant indépendant (au menu).
    garant: bool,
    /// Garant écarté (désaccords rejetés par les joueurs) : plus choisi cette session.
    garant_banned: bool,
    /// Garant arbitre d'une affaire de cette partie, en ce moment.
    arbitrating: Option<u64>,
    /// Lobby (n°), et lobbies qui l'ont invité.
    party: Option<u64>,
    invited: Vec<u64>,
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
    /// Places gardées pour les joueurs d'un lobby qui arrivent (pseudo, depuis).
    reserved: Vec<(String, Instant)>,
    /// Signalements déjà comptés dans cette partie (de, contre).
    reports: HashSet<(String, String)>,
    /// Construction : garants de zones.
    build: zones::Build,
    /// FPS : garant de secours désigné par l'hôte.
    backup: Option<u64>,
    /// Garants indépendants en attente (membres cachés, pas des joueurs) et prêts.
    hidden: Vec<u64>,
    ready_standby: Vec<u64>,
    /// La partie est servie par un garant indépendant (pas par un joueur).
    garant_hosted: bool,
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
    maps: maps::Store,
    hardware: hardware::Store,
    servers: servers::Store,
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
    let state = Arc::new(Mutex::new(State {
        whois: whois.clone(),
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
        maps,
        hardware: hardware::Store::open(cfg_dir.join("hardware.bin")),
        servers: servers::Store::open(cfg_dir.join("serveurs.bin")),
    }));
    std::thread::Builder::new()
        .name("relais : écoute".into())
        .spawn(move || {
            for conn in listener.incoming() {
                let Ok(stream) = conn else { continue };
                let state = state.clone();
                let _ = std::thread::Builder::new()
                    .name("relais : client".into())
                    .spawn(move || {
                        let ip = stream.peer_addr().map(|a| a.ip()).ok();
                        let mut peer = ip.map(|a| a.to_string()).unwrap_or_default();
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
    })
}

/// Lit le préambule « MLPX » que met le tunnel iroh devant chaque connexion (voir
/// `minelib_p2p::preamble`). `None` : connexion locale ordinaire, rien n'est consommé.
/// Seules les connexions venues de ce PC sont lues ainsi : un joueur distant ne peut
/// donc pas se faire passer pour un autre.
fn tunnel_peer(stream: &TcpStream) -> std::io::Result<Option<String>> {
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
        st.conns.insert(
            id,
            Conn {
                tx: chan.pusher(),
                pseudo: None,
                room: None,
                caps: proto::GarantCaps::default(),
                joined: Instant::now(),
                kicked: false,
                garant: false,
                garant_banned: false,
                arbitrating: None,
                party: None,
                invited: Vec::new(),
            },
        );
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
                Request::Hello { protocol, .. } if protocol == proto::PROTOCOL => {
                    hello = true;
                    let st = state.lock().unwrap();
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
    // Joueur du début qui quitte une partie FPS en cours : absent (il peut revenir).
    if let Some(p) = me {
        let low = p.to_ascii_lowercase();
        let in_roster = room.roster.iter().any(|r| r.eq_ignore_ascii_case(&p));
        if room.kind == 0 && room.started && !room.settled && in_roster {
            let e = room.away.entry(low).or_insert((None, 0.0));
            e.0.get_or_insert_with(Instant::now);
        }
    }
    // Garant en attente qui s'en va : simplement retiré.
    if room.hidden.contains(&id) {
        room.hidden.retain(|&h| h != id);
        room.ready_standby.retain(|&h| h != id);
        room.members.retain(|&m| m != id);
        if room.backup == Some(id) {
            room.backup = None;
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
            room.ready_standby
                .first()
                .copied()
                .or(room.backup.filter(|b| room.members.contains(b)))
                .unwrap_or(room.members[0])
        };
        switch_host(st, room_id, new, None);
        if kind == 1 {
            let cands = candidates(st, room_id);
            if let Some(room) = st.rooms.get_mut(&room_id) {
                let changes = room.build.left(id, &cands);
                announce(st, room_id, changes);
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
            let cands = candidates(st, room_id);
            let room = st.rooms.get_mut(&room_id).expect("partie");
            let changes = room.build.left(id, &cands);
            announce(st, room_id, changes);
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
        if let Some(room) = st.rooms.get(&room_id) {
            if let Some(c) = st.conns.get(&new) {
                for (zone, log) in room.build.all_logs() {
                    let chunk = [zone[0] * proto::ZONE, zone[1] * proto::ZONE];
                    for data in log {
                        transport::push(
                            &c.tx,
                            &Response::FromZone {
                                peer: 0,
                                chunk,
                                data,
                            },
                        );
                    }
                }
            }
        }
    }
    for m in members {
        if let Some(c) = st.conns.get(&m) {
            transport::push(
                &c.tx,
                &Response::HostChanged {
                    host: host_pseudo.clone(),
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
    let Some(room) = st.rooms.get(&rid) else {
        return false;
    };
    let mut people: Vec<String> = room
        .everyone()
        .chain([room.host, auditor])
        .filter_map(|m| st.conns.get(&m).and_then(|c| c.pseudo.clone()))
        .collect();
    people.extend(room.roster.iter().cloned());
    let Some(arbiter) = pick_garant(st, &people) else {
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
    if guilty == Some(true) {
        if let Some(p) = host_pseudo {
            let why = format!("triche constatée par arbitrage (partie {rid}, n° {case})");
            match ban_cheater(st, &p, &why) {
                Ok(n) => log::warn!("« {p} » banni automatiquement ({n} machine(s)) : {why}"),
                Err(e) => log::warn!("ban automatique de « {p} » impossible : {e}"),
            }
        }
    }
    if let Some(r) = st.rooms.get_mut(&rid) {
        r.confirmed = r.confirmed.min(1);
        r.dismissed = 0;
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
    let me = st.stats.get(p).cloned().unwrap_or_default();
    if me.games < 3 || me.ratio() > 0.25 {
        return false;
    }
    players.iter().all(|q| {
        !q.eq_ignore_ascii_case(p)
            && !me.avoids(q)
            && !me.friends.iter().any(|f| f.eq_ignore_ascii_case(q))
            && st.stats.get(q).is_none_or(|s| {
                !s.avoids(p) && !s.friends.iter().any(|f| f.eq_ignore_ascii_case(p))
            })
    })
}

/// Le garant disponible le plus adapté pour ces joueurs.
fn pick_garant(st: &State, players: &[String]) -> Option<u64> {
    st.conns
        .iter()
        .filter(|(&g, _)| garant_ok(st, g, players))
        .map(|(&g, c)| {
            let cand = zones::Candidate {
                id: g,
                caps: c.caps,
                uptime: c.joined.elapsed(),
                ratio: c.pseudo.as_deref().map_or(0.0, |p| st.stats.ratio(p)),
            };
            (g, zones::aptitude(&cand))
        })
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(g, _)| g)
}

/// Entretien (toutes les 2 s) : chaque partie FPS reçoit un garant indépendant en
/// attente ; une partie servie par un joueur passe à ce garant dès qu'il est prêt.
fn upkeep(st: &mut State) {
    if st.last_upkeep.elapsed() < Duration::from_secs(2) {
        return;
    }
    st.last_upkeep = Instant::now();
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

/// Joueurs d'une partie de construction pouvant être garants.
fn candidates(st: &State, room_id: u64) -> Vec<zones::Candidate> {
    let Some(room) = st.rooms.get(&room_id) else {
        return Vec::new();
    };
    room.everyone()
        .filter_map(|id| {
            let c = st.conns.get(&id)?;
            Some(zones::Candidate {
                id,
                caps: c.caps,
                uptime: c.joined.elapsed(),
                ratio: c.pseudo.as_deref().map_or(0.0, |p| st.stats.ratio(p)),
            })
        })
        .collect()
}

/// Prévient les garants de leurs nouvelles zones (avec leur historique) et des zones
/// perdues.
fn announce(st: &State, room_id: u64, changes: zones::Changes) {
    let Some(room) = st.rooms.get(&room_id) else {
        return;
    };
    for (id, gained, lost) in changes {
        let Some(c) = st.conns.get(&id) else { continue };
        for z in &gained {
            for data in room.build.log(*z) {
                let chunk = [z[0] * proto::ZONE, z[1] * proto::ZONE];
                transport::push(
                    &c.tx,
                    &Response::FromZone {
                        peer: 0,
                        chunk,
                        data,
                    },
                );
            }
        }
        transport::push(&c.tx, &Response::Zones { gained, lost });
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
    let len = t0.elapsed().as_secs_f64();
    for (p, (since, total)) in &room.away {
        let away = total + since.map_or(0.0, |t| t.elapsed().as_secs_f64());
        let pen = stats::Stats::absence_penalty(away, len);
        if pen == 0 || spared.contains(p) {
            continue;
        }
        let s = st.stats.get_mut(p);
        s.trust_by(pen);
        s.absences += 1;
        st.stats.save(p);
        hit.insert(p.clone());
        log::info!("« {p} » absent {away:.0} s sur {len:.0} s : confiance {pen}");
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
    let sig = room.map_sig;
    // Seules les parties à 5 contre 5 comptent pour le classement.
    let ranked = room.max_players == proto::FPS_PLAYERS as u32;
    // Classement : seulement sur les 10 cartes les plus jouées.
    let ranked_map = st.maps.among_top(&room.map_sig, 10);
    let room = st.rooms.get_mut(&room_id).expect("partie");
    let reported: HashSet<String> = room.reports.iter().map(|r| r.1.clone()).collect();
    let roster = room.roster.clone();
    // Exclus pour avoir tué 3 coéquipiers : pas de pénalité d'absence en plus.
    let tk_kicked: Vec<String> = summary
        .players
        .iter()
        .filter(|p| p.teamkills >= 3)
        .map(|p| p.pseudo.to_ascii_lowercase())
        .collect();
    let mut room = st.rooms.remove(&room_id).expect("partie");
    let absent = settle_absences(st, &mut room, &tk_kicked);
    st.rooms.insert(room_id, room);
    for p in &summary.players {
        let s = st.stats.get_mut(&p.pseudo);
        s.teamkills += p.teamkills as u32;
    }
    // Partie propre (pas signalé, pas parti longtemps) : la confiance remonte un peu.
    for p in &roster {
        let low = p.to_ascii_lowercase();
        if ranked && !reported.contains(&low) && !absent.contains(&low) {
            let s = st.stats.get_mut(p);
            let g = s.clean_gain();
            s.trust_by(g);
        }
        st.stats.save(p);
    }
    if ranked && ranked_map {
        st.stats.apply_match(&summary);
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
        (max_players.clamp(2, proto::FPS_PLAYERS) & !1) as u32
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
            members: Vec::new(),
            started: false,
            map_sig,
            roster: Vec::new(),
            results: HashMap::new(),
            applied: false,
            started_at: None,
            away: HashMap::new(),
            settled: false,
            reserved: Vec::new(),
            reports: HashSet::new(),
            build: zones::Build::default(),
            backup: None,
            hidden: Vec::new(),
            ready_standby: Vec::new(),
            garant_hosted: false,
            judges: HashMap::new(),
            confirmed: 0,
            dismissed: 0,
            audited: None,
            evidence: VecDeque::new(),
            evidence_parts: Vec::new(),
            arbitration: None,
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

fn join_room(st: &mut State, id: u64, pseudo: &str, room: u64) -> Response {
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
    if let Some((since, total)) = r.away.get_mut(&pseudo.to_ascii_lowercase()) {
        if let Some(t) = since.take() {
            *total += t.elapsed().as_secs_f64();
        }
    }
    if r.kind == 1 {
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
    r.reserved.retain(|(p, t)| *p != low && t.elapsed() < Duration::from_secs(60));
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
fn find_room(st: &State, group: &[String], map_sig: [u8; 32]) -> Option<u64> {
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
        if r.kind != 0 || r.started || r.map_sig != map_sig {
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
        let trust =
            names.iter().map(|p| trust_of(st, p)).sum::<i32>() / names.len().max(1) as i32;
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
                return err(ErrorCode::Refused, format!("Impossible d'inviter « {who} »."));
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
                return err(ErrorCode::Refused, format!("« {who} » est déjà dans le lobby."));
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
            let invited = pid.filter(|p| {
                st.conns
                    .get(&id)
                    .is_some_and(|c| c.invited.contains(p))
            });
            let Some(pid) = invited else {
                return err(ErrorCode::Refused, "Invitation expirée.");
            };
            if let Some(c) = st.conns.get_mut(&id) {
                c.invited.retain(|&p| p != pid);
            }
            if action == proto::PARTY_DECLINE {
                return Response::Done;
            }
            if st.parties.get(&pid).is_none_or(|v| v.len() >= proto::PARTY_MAX) {
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
                return err(ErrorCode::Refused, format!("« {who} » n'est pas dans le lobby."));
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
            let mut list: Vec<proto::ServerEntry> = st
                .rooms
                .iter()
                .filter(|(_, r)| r.kind == kind && !(kind == 0 && r.started))
                .map(|(&rid, r)| proto::ServerEntry {
                    id: rid,
                    host: r.host_pseudo.clone(),
                    name: r.name.clone(),
                    map: r.map.clone(),
                    players: r.player_count() as u32,
                    max_players: r.max_players,
                    region: st.cfg.name.clone(),
                })
                .collect();
            list.sort_by_key(|e| e.id);
            list.truncate(200);
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
            // Un membre en pleine partie FPS n'en est pas arraché (il serait pénalisé).
            let busy = group.iter().filter(|&&m| m != id).find_map(|m| {
                let c = st.conns.get(m)?;
                let r = st.rooms.get(&c.room?)?;
                (r.kind == 0 && r.started && !r.applied).then(|| c.pseudo.clone().unwrap_or_default())
            });
            if let Some(p) = busy {
                return Some(err(
                    ErrorCode::Refused,
                    format!("« {p} » est en pleine partie : attends qu'il ait fini."),
                ));
            }
            for &m in &group {
                leave_room(st, m);
            }
            let names: Vec<String> = group
                .iter()
                .filter_map(|m| st.conns.get(m).and_then(|c| c.pseudo.clone()))
                .collect();
            let players = (players.clamp(2, 10) & !1) as u16;
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
            let resp = match find_room(st, &names, map_sig) {
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
                        format!("Partie servie par {gp}"),
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
            if let Some(r) = my_room.and_then(|r| st.rooms.get(&r)).filter(|r| r.kind == 1) {
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
                let them = st.stats.get_mut(&target);
                them.reports += 1;
                them.trust_by(stats::trust::REPORT);
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
            let act = match action {
                proto::SERVER_BAN => servers::Action::Ban,
                proto::SERVER_UNBAN => servers::Action::Unban,
                proto::SERVER_OP => servers::Action::Op,
                proto::SERVER_DEOP => servers::Action::Deop,
                proto::SERVER_KILL => servers::Action::Kill,
                _ => servers::Action::List,
            };
            match st
                .servers
                .act(&server, &creator, pseudo, act, &target, &reason)
            {
                Ok(text) => {
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
            let them = st.stats.get_mut(&target);
            them.reported_by.retain(|p| *p != mine);
            if them.reports > 0 {
                them.trust_by(-stats::trust::REPORT);
            }
            them.reports = them.reports.saturating_sub(1);
            st.stats.save(pseudo);
            st.stats.save(&target);
            log::info!("« {pseudo} » annule son signalement de « {target} »");
            Response::Done
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
                let room = st.rooms.get_mut(&rid).expect("partie");
                let logs = room.build.moved(id, chunk);
                if let Some(c) = st.conns.get(&id) {
                    for (zone, log) in logs {
                        let chunk = [zone[0] * proto::ZONE, zone[1] * proto::ZONE];
                        for data in log {
                            transport::push(
                                &c.tx,
                                &Response::FromZone {
                                    peer: 0,
                                    chunk,
                                    data,
                                },
                            );
                        }
                    }
                }
                let cands = candidates(st, rid);
                let changes = st
                    .rooms
                    .get_mut(&rid)
                    .expect("partie")
                    .build
                    .balance(&cands);
                announce(st, rid, changes);
            }
            return None;
        }
        Request::ToZone { chunk, data } => {
            let rid = my_room.filter(|r| st.rooms.get(r).is_some_and(|r| r.kind == 1))?;
            let zone = proto::zone_of(chunk);
            let cands = candidates(st, rid);
            let room = st.rooms.get_mut(&rid).expect("partie");
            room.build.touch(zone);
            let (garant, changes) = room.build.garant(zone, &cands);
            // Nommé garant à l'instant : sa demande lui revient, à valider comme les autres.
            let fresh = garant == Some(id) && !changes.is_empty();
            announce(st, rid, changes);
            let room = st.rooms.get_mut(&rid).expect("partie");
            if fresh {
                if let Some(c) = st.conns.get(&id) {
                    transport::push(
                        &c.tx,
                        &Response::FromZone {
                            peer: id,
                            chunk,
                            data,
                        },
                    );
                }
            } else if garant == Some(id) {
                // Le garant publie : à tous les joueurs proches, à l'hôte (qui enregistre
                // le monde), et dans l'historique de la zone.
                room.build.record(zone, &data);
                let mut to = room.build.listeners(chunk, id);
                if room.host != id && !to.contains(&room.host) {
                    to.push(room.host);
                }
                let msg = Response::FromZone {
                    peer: id,
                    chunk,
                    data,
                };
                for m in to {
                    if let Some(c) = st.conns.get(&m) {
                        transport::push(&c.tx, &msg);
                    }
                }
            } else if let Some(c) = garant.and_then(|g| st.conns.get(&g)) {
                transport::push(
                    &c.tx,
                    &Response::FromZone {
                        peer: id,
                        chunk,
                        data,
                    },
                );
            }
            return None;
        }
        Request::Nearby { chunk, data } => {
            if let Some(room) = my_room.and_then(|r| st.rooms.get(&r)) {
                let msg = Response::FromZone {
                    peer: id,
                    chunk,
                    data,
                };
                for m in room.build.listeners(chunk, id) {
                    if let Some(c) = st.conns.get(&m) {
                        transport::push(&c.tx, &msg);
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
            for t in to.into_iter().take(64) {
                let t = if fps && t == 0 { room.host } else { t };
                if t != id && (room.host == t || room.members.contains(&t)) {
                    if let Some(c) = st.conns.get(&t) {
                        transport::push(&c.tx, &msg);
                    }
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
            | Request::RoomStarted { .. }
            | Request::Join { .. }
            | Request::Leave
            | Request::ToHost { .. }
            | Request::SetBackup { .. }
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
            Event::PartyGo { room: r, map_sig, .. } => {
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
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_account_moves_to_a_new_relay_with_its_kept_vault() {
        let mk = |n: &str| {
            let dir = std::env::temp_dir().join(format!("minelib-{n}-{}", rand::random::<u32>()));
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
            Event::HostChanged(h) => assert_eq!(h, "Verif"),
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
                for g in ["Garant", "Verif", "Arbitre"] {
                    st.get_mut(g).games = 5;
                    st.save(g);
                }
            }
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
            a.send(Cmd::Verdict { case, guilty });
            if guilty {
                wait(&v, |e| matches!(e, Event::Promoted { .. }));
                wait(&g, |e| matches!(e, Event::RoomClosed));
                match wait(&p, |e| matches!(e, Event::HostChanged(_))) {
                    Event::HostChanged(h) => assert_eq!(h, "Verif"),
                    _ => unreachable!(),
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
}
