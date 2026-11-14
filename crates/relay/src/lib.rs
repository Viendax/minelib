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
mod stats;
mod zones;

use std::collections::HashSet;
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
    /// Signalements déjà comptés dans cette partie (de, contre).
    reports: HashSet<(String, String)>,
    /// Construction : garants de zones.
    build: zones::Build,
    /// FPS : garant de secours désigné par l'hôte.
    backup: Option<u64>,
}

impl Room {
    fn everyone(&self) -> impl Iterator<Item = u64> + '_ {
        std::iter::once(self.host).chain(self.members.iter().copied())
    }
}

struct State {
    conns: HashMap<u64, Conn>,
    rooms: HashMap<u64, Room>,
    next_id: u64,
    cfg: Config,
    id: Identity,
    registry: Registry,
    accounts: accounts::Store,
    limits: limits::Limits,
    stats: stats::Store,
    maps: maps::Store,
    hardware: hardware::Store,
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
    let state = Arc::new(Mutex::new(State {
        conns: HashMap::new(),
        rooms: HashMap::new(),
        next_id: 1,
        cfg,
        id,
        registry,
        accounts,
        limits: limits::Limits::default(),
        stats,
        maps,
        hardware: hardware::Store::open(cfg_dir.join("hardware.bin")),
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
                        let peer = stream
                            .peer_addr()
                            .map(|a| a.ip().to_string())
                            .unwrap_or_default();
                        if let Err(e) = serve(stream, &state, &peer) {
                            log::debug!("client {peer} : {e:#}");
                        }
                    });
            }
        })?;
    Ok(Handle { addr, public })
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
            },
        );
        id
    };
    let result = serve_loop(&mut chan, state, peer, id);
    let mut st = state.lock().unwrap();
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
        } else {
            handle(req, state, &mut session, peer, id)
        };
        if let Some(r) = resp {
            chan.send(&r)?;
        }
    }
}

/// Le jeu `id` quitte sa partie : ferme celle qu'il héberge, prévient l'hôte sinon.
fn leave_room(st: &mut State, id: u64) {
    let Some(room_id) = st.conns.get_mut(&id).and_then(|c| c.room.take()) else {
        return;
    };
    let Some(room) = st.rooms.get_mut(&room_id) else {
        return;
    };
    // FPS : le garant de secours reprend la partie (sans coupure pour les autres).
    if room.host == id && room.kind == 0 && !room.members.is_empty() {
        let new = room
            .backup
            .filter(|b| room.members.contains(b))
            .unwrap_or(room.members[0]);
        room.members.retain(|&m| m != new);
        room.host = new;
        room.backup = None;
        let members: Vec<u64> = room.members.clone();
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
            transport::push(&c.tx, &Response::Promoted { members: list });
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
        log::info!("partie {room_id} : l'hôte est parti, « {host_pseudo} » reprend");
        return;
    }
    if room.host == id {
        let room = st.rooms.remove(&room_id).expect("partie");
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
    let Some(summary) = room
        .results
        .values()
        .find(|(_, who)| who.len() >= need)
        .map(|(m, _)| m.clone())
    else {
        return;
    };
    room.applied = true;
    let sig = room.map_sig;
    st.stats.apply_match(&summary);
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
    let name: String = name.chars().take(32).collect();
    // FPS : 10 joueurs ; construction : sans limite (0).
    let max_players = if kind == 0 {
        proto::FPS_PLAYERS as u32
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
            reports: HashSet::new(),
            build: zones::Build::default(),
            backup: None,
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
    if r.max_players > 0 && r.members.len() + 1 >= r.max_players as usize {
        return err(ErrorCode::Refused, "Partie pleine.");
    }
    if r.kind == 0 && r.started {
        return err(ErrorCode::Refused, "Partie déjà commencée.");
    }
    r.members.push(id);
    let (host, host_pseudo, map, map_sig) =
        (r.host, r.host_pseudo.clone(), r.map.clone(), r.map_sig);
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
fn find_room(st: &State, pseudo: &str, map_sig: [u8; 32]) -> Option<u64> {
    let me = st.stats.get(pseudo).cloned().unwrap_or_default();
    let my_ratio = me.ratio();
    let mut best: Option<(f64, usize, u64)> = None;
    for (&rid, r) in &st.rooms {
        if r.kind != 0 || r.started || r.map_sig != map_sig {
            continue;
        }
        let n = r.members.len() + 1;
        if n >= proto::FPS_PLAYERS as usize {
            continue;
        }
        let names: Vec<&str> = r
            .everyone()
            .filter_map(|m| st.conns.get(&m).and_then(|c| c.pseudo.as_deref()))
            .collect();
        let blocked = names
            .iter()
            .any(|p| me.avoids(p) || st.stats.get(p).is_some_and(|s| s.avoids(pseudo)));
        if blocked {
            continue;
        }
        let ratio =
            names.iter().map(|p| st.stats.ratio(p)).sum::<f64>() / names.len().max(1) as f64;
        let diff = (ratio - my_ratio).abs();
        if diff > reputation_tolerance(my_ratio, ratio) {
            continue;
        }
        let better = best.is_none_or(|(d, m, _)| diff < d - 1e-9 || (diff <= d + 1e-9 && n > m));
        if better {
            best = Some((diff, n, rid));
        }
    }
    best.map(|b| b.2)
}

/// Administrateurs : pseudos listés dans `admins.txt` (un par ligne) dans le dossier du
/// relais.
fn is_admin(dir: &Path, pseudo: &str) -> bool {
    std::fs::read_to_string(dir.join("admins.txt"))
        .unwrap_or_default()
        .lines()
        .any(|l| l.trim().eq_ignore_ascii_case(pseudo))
}

/// Écart de réputation accepté entre un joueur et une partie.
fn reputation_tolerance(a: f64, b: f64) -> f64 {
    0.1 + 0.5 * a.max(b)
}

/// Requêtes de parties (connecté seulement).
fn handle_room(req: Request, st: &mut State, id: u64, pseudo: &str) -> Option<Response> {
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
                    players: r.members.len() as u32 + 1,
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
        Request::Search { map_sig, map } => {
            leave_room(st, id);
            match find_room(st, pseudo, map_sig) {
                Some(room) => join_room(st, id, pseudo, room),
                None => {
                    let rid = open_room(
                        st,
                        id,
                        pseudo,
                        0,
                        String::new(),
                        map,
                        map_sig,
                        proto::FPS_PLAYERS,
                    );
                    Response::Hosted { room: rid }
                }
            }
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
                    "Réservé aux administrateurs du relais (admins.txt).",
                ));
            }
            let Some(rec) = st.accounts.load(&target) else {
                return Some(err(
                    ErrorCode::Refused,
                    format!("Joueur « {target} » inconnu."),
                ));
            };
            if !st.registry.is_banned(&rec.pubkey) {
                use sha2::{Digest, Sha256};
                let proof: [u8; 32] = Sha256::digest(reason.as_bytes()).into();
                if let Err(e) = st.registry.append(Body::Ban {
                    pubkey: rec.pubkey,
                    proof,
                }) {
                    return Some(err(ErrorCode::Internal, format!("{e:#}")));
                }
            }
            let machines = st.hardware.ban_account_machines(&rec.pseudo);
            // Connexions de ce compte : coupées.
            let ids: Vec<u64> = st
                .conns
                .iter()
                .filter(|(_, c)| {
                    c.pseudo
                        .as_deref()
                        .is_some_and(|p| p.eq_ignore_ascii_case(&target))
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
            log::info!(
                "« {target} » banni par « {pseudo} » ({machines} machine(s) pour 3 mois) : {reason}"
            );
            Response::Done
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
            Response::Friends { list }
        }
        Request::Maps => {
            let mut list = st.maps.entries();
            for r in st.rooms.values().filter(|r| r.kind == 0) {
                let n = r.members.len() as u32 + 1;
                match list.iter_mut().find(|e| e.sig == r.map_sig) {
                    Some(e) => {
                        if !r.started {
                            e.waiting += n;
                        }
                    }
                    None => list.push(proto::MapEntry {
                        sig: r.map_sig,
                        name: r.map.clone(),
                        author: r.host_pseudo.clone(),
                        size: 0,
                        plays: 0,
                        waiting: if r.started { 0 } else { n },
                        stored: false,
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
        } => match st.maps.put(id, pseudo, sig, &name, offset, total, &data) {
            maps::Put::Partial(o) => Response::MapStored {
                offset: o,
                complete: false,
            },
            maps::Put::Complete => Response::MapStored {
                offset: total,
                complete: true,
            },
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
            if let Some(r) = my_room.and_then(|r| st.rooms.get(&r)) {
                if let Some(c) = st.conns.get(&r.host) {
                    transport::push(&c.tx, &Response::FromPeer { peer: id, data });
                }
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
        Request::SetBackup { peer } => {
            if let Some(r) = my_room.and_then(|r| st.rooms.get_mut(&r)) {
                if r.host == id && r.members.contains(&peer) {
                    r.backup = Some(peer);
                }
            }
            return None;
        }
        Request::ToPeer { peer, data } => {
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
            | Request::MatchResult { .. }
            | Request::Leaderboard { .. }
            | Request::Friend { .. }
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
        } => {
            let keys = [
                format!("ip:{peer}"),
                format!("pseudo:{}", pseudo.to_ascii_lowercase()),
            ];
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
            if let Some(sealed) = &record.totp {
                let Some(code) = totp else {
                    return Response::NeedTotp;
                };
                let secret = crypto::open(&st.id.vault_key, sealed).unwrap_or_default();
                match totp::check(&secret, code, totp::now_step()) {
                    Some(step) if step > record.last_totp_step => {
                        record.last_totp_step = step;
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
        });
        wait(&c2, |e| matches!(e, Event::NeedTotp));
        // Le code déjà utilisé pour l'activation est refusé, le suivant accepté.
        c2.send(Cmd::Login {
            pseudo: "viendax".into(),
            password: "motdepasse1".into(),
            totp: Some(totp::code(&secret, now)),
        });
        assert!(wait_error(&c2).contains("double authentification"));
        c2.send(Cmd::Login {
            pseudo: "viendax".into(),
            password: "motdepasse1".into(),
            totp: Some(totp::code(&secret, now + 1)),
        });
        match wait(&c2, |e| matches!(e, Event::LoggedIn { .. })) {
            Event::LoggedIn { pseudo, totp, .. } => {
                assert_eq!(pseudo, "Viendax");
                assert!(totp);
            }
            _ => unreachable!(),
        }
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
            Event::PeerJoined { peer, pseudo } => {
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
        wait(&guest, |e| matches!(e, Event::Promoted(_)));
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
            });
            wait(cl, |e| matches!(e, Event::Hosted(_) | Event::Joined { .. }))
        };
        // Personne : Ana ouvre la partie ; Bob la rejoint.
        assert!(matches!(search(&a), Event::Hosted(_)));
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
            Event::Joined { host, .. } => assert_eq!(host, "Ana"),
            e => panic!("{e:?}"),
        }
        b.send(Cmd::Leave);
        wait(&b, |e| matches!(e, Event::Left));
        b.send(Cmd::Report("ana".into()));
        assert!(wait_error(&b).contains("partie"), "hors partie : refusé");
        match search(&c) {
            Event::Joined { host, map_sig, .. } => {
                assert_eq!(map_sig, map);
                assert!(host == "Ana" || host == "Bob");
            }
            e => panic!("{e:?}"),
        }
        // Classement et amis.
        a.send(Cmd::Friend {
            pseudo: "cid".into(),
            add: true,
        });
        match wait(&a, |e| matches!(e, Event::Friends(_))) {
            Event::Friends(l) => assert_eq!(l, vec!["Cid".to_string()]),
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
        // Mauvaise signature : refusée.
        c.send(Cmd::UploadMap {
            sig: [0; 32],
            name: "Fausse".into(),
            bytes: bytes.clone(),
        });
        assert!(matches!(
            wait(&c, |e| matches!(e, Event::MapFailed(..))),
            Event::MapFailed(..)
        ));
        c.send(Cmd::UploadMap {
            sig,
            name: "Plate".into(),
            bytes: bytes.clone(),
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
        });
        assert!(wait_error(&again).contains("vie"));
        // Nouveau compte sur la même machine : machine bannie.
        let other = register("Nouveau");
        let e = wait_error(&other);
        assert!(e.contains("machine"), "{e}");
        let _ = std::fs::remove_dir_all(dir);
    }
}
