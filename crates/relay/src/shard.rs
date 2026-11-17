//! Relais de région : une région d'un monde de construction très peuplé est servie par son
//! propre relais, que fait tourner le jeu d'un joueur sans lien avec ce monde (sa connexion
//! de garant, en arrière-plan, comme les garants des parties FPS). Personne dans le monde
//! n'a intérêt à l'attaquer, et le relais du monde n'a plus à tout transmettre.
//!
//! Il fait trois choses, pour sa région seulement :
//! - les joueurs y entrent avec le billet du relais du monde (`proto::shard_mac`) ;
//! - il transmet les positions aux joueurs proches (moins souvent au loin) ;
//! - il valide lui-même les modifications (bloc connu, dans la région, 20 par seconde au
//!   plus), les publie aux joueurs proches, garde l'historique compacté de la région, et
//!   en donne une copie (`published`) que le jeu de l'hôte renvoie au relais du monde.
//!
//! S'il disparaît, le relais du monde reprend la région (il en a la copie) ou la confie à
//! un autre hôte : chacun reçoit exactement ce qui lui manque.
//!
//! Une région très peuplée a plusieurs relais, chacun chez un hôte différent. Chaque
//! joueur n'entre que dans l'un d'eux (`proto::shard_for`), et les relais se relient entre
//! eux (`set_peers`) :
//! - la position d'un joueur part à ses voisins du même relais, et une fois à chaque autre
//!   relais, qui la transmet à ses propres joueurs proches : le travail d'une foule est
//!   partagé ;
//! - un seul relais met les modifications en ordre (le plus petit rang) : les autres lui
//!   envoient celles qu'ils ont vérifiées et transmettent à leurs joueurs ce qu'il publie.
//!   Tous gardent ainsi le même historique ; si le premier s'en va, le suivant prend sa
//!   place et republie les dernières secondes (rien n'est perdu).

use std::collections::{HashMap, VecDeque};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use minelib_net::client::{Client, Cmd, Event, Resolver};
use minelib_net::game::{self, BuildMsg};
use minelib_net::proto::{self, ErrorCode, Request, Response};
use minelib_net::transport::{self, Channel, Inbound, Incoming};

use crate::zones;

/// Modifications acceptées par seconde et par joueur.
const MAX_EDITS_PER_S: u32 = 20;
/// Hauteur d'un monde (cases).
const HEIGHT: i32 = 256;

/// Modifications gardées pour un relais qui arrive ou qui prend la première place.
const RECENT: Duration = Duration::from_secs(60);

struct Conn {
    tx: Sender<Inbound>,
    pseudo: Option<String>,
    /// Un autre relais de la région (pas un joueur).
    peer: bool,
}

/// Comment joindre un autre relais de la région (directement ou par un tunnel).
pub type Dial = Arc<dyn Fn(&proto::ShardInfo) -> Resolver + Send + Sync>;

/// Lien vers un autre relais de la région.
struct PeerOut {
    info: proto::ShardInfo,
    client: Client,
    link: Option<Sender<Inbound>>,
    /// Lien cassé : retenté après un instant.
    broken: Option<Instant>,
}

struct State {
    room: u64,
    region: [i32; 2],
    secret: [u8; 32],
    /// Nombre de sortes de blocs (un bloc au-delà est refusé).
    blocks: u16,
    conns: HashMap<u64, Conn>,
    next_id: u64,
    build: zones::Build,
    rate: HashMap<u64, (Instant, u32)>,
    published: Sender<([i32; 2], Vec<u8>)>,
    closed: bool,
    /// Rang de ce relais dans sa région, et liens vers les autres (par rang).
    slot: u16,
    peers: HashMap<u16, PeerOut>,
    dial: Option<Dial>,
    /// Dernières modifications publiées dans la région (moment, chunk, modification).
    recent: VecDeque<(Instant, [i32; 2], Vec<u8>)>,
    was_first: bool,
}

impl State {
    /// Ce relais met-il les modifications de la région en ordre (le plus petit rang) ?
    fn first(&self) -> bool {
        self.peers.keys().all(|&s| s > self.slot)
    }

    fn remember(&mut self, chunk: [i32; 2], set: &[u8]) {
        while self
            .recent
            .front()
            .is_some_and(|(t, _, _)| t.elapsed() > RECENT)
        {
            self.recent.pop_front();
        }
        self.recent.push_back((Instant::now(), chunk, set.to_vec()));
    }

    /// Transmet une modification en ordre aux joueurs de ce relais proches du chunk, et à
    /// son auteur s'il est ici (c'est sa confirmation).
    fn deliver(&mut self, chunk: [i32; 2], set: &[u8]) {
        let mut to = self.build.listeners(chunk, u64::MAX);
        if let Some(BuildMsg::Set { by, .. }) = game::decode::<BuildMsg>(set) {
            let author = self
                .conns
                .iter()
                .find(|(_, c)| !c.peer && c.pseudo.as_deref() == Some(by.as_str()))
                .map(|(&id, _)| id);
            if let Some(a) = author.filter(|a| !to.contains(a)) {
                to.push(a);
            }
        }
        for (m, items) in self.build.mirror_to(proto::zone_of(chunk), set, &to) {
            if let Some(c) = self.conns.get(&m) {
                for data in items {
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

    /// Premier relais : la modification prend sa place dans l'ordre. Copie au relais du
    /// monde, puis aux autres relais, puis aux joueurs d'ici.
    fn publish(&mut self, chunk: [i32; 2], set: Vec<u8>) {
        let _ = self.published.send((chunk, set.clone()));
        self.remember(chunk, &set);
        for p in self.peers.values() {
            if let Some(l) = &p.link {
                transport::push(
                    l,
                    &Request::PeerEdit {
                        chunk,
                        data: set.clone(),
                    },
                );
            }
        }
        self.deliver(chunk, &set);
    }

    /// Lien vers le premier relais de la région (si ce n'est pas celui-ci).
    fn first_link(&self) -> Option<&Sender<Inbound>> {
        let first = self.peers.keys().min().filter(|&&s| s < self.slot)?;
        self.peers.get(first)?.link.as_ref()
    }

    fn connect(&self, info: &proto::ShardInfo) -> Client {
        let resolve: Resolver = match &self.dial {
            Some(d) => d(info),
            None => {
                let (addr, key) = (info.addr.clone(), info.key);
                Box::new(move || Ok((addr.clone(), Some(key))))
            }
        };
        let client = Client::spawn_resolved(resolve);
        let pseudo = proto::shard_peer_name(self.slot);
        let expires = now_secs() + 24 * 3600;
        client.send(Cmd::ShardLogin {
            room: self.room,
            mac: proto::shard_mac(&self.secret, self.room, &pseudo, expires),
            pseudo,
            expires,
        });
        client
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

pub struct ShardHandle {
    pub addr: SocketAddr,
    /// Clé publique du canal chiffré.
    pub public: [u8; 32],
    /// Ce que ce relais a publié (à renvoyer au relais du monde).
    pub published: Receiver<([i32; 2], Vec<u8>)>,
    state: Arc<Mutex<State>>,
}

impl ShardHandle {
    /// Historique de la région reçu du relais du monde (avant l'arrivée des joueurs).
    pub fn seed(&self, chunk: [i32; 2], data: &[u8]) {
        let mut st = self.state.lock().unwrap();
        let zone = proto::zone_of(chunk);
        st.build.mirror(zone, data);
    }

    /// Joueurs entrés dans ce relais.
    pub fn players(&self) -> usize {
        let st = self.state.lock().unwrap();
        st.conns
            .values()
            .filter(|c| c.pseudo.is_some() && !c.peer)
            .count()
    }

    /// Autres relais de la région auxquels ce relais est relié.
    pub fn linked(&self) -> usize {
        let st = self.state.lock().unwrap();
        st.peers.values().filter(|p| p.link.is_some()).count()
    }

    /// Ce relais met-il les modifications de sa région en ordre ?
    pub fn is_first(&self) -> bool {
        self.state.lock().unwrap().first()
    }

    /// Comment joindre les autres relais (tunnel) ; sans cela, par leur adresse directe.
    pub fn set_dial(&self, dial: Dial) {
        self.state.lock().unwrap().dial = Some(dial);
    }

    /// Relais prêts de la région (liste du relais du monde, celui-ci compris) : ce relais
    /// se relie aux autres et quitte ceux qui sont partis.
    pub fn set_peers(&self, list: &[proto::ShardInfo]) {
        let mut st = self.state.lock().unwrap();
        let (region, slot) = (st.region, st.slot);
        let others: Vec<&proto::ShardInfo> = list
            .iter()
            .filter(|i| i.region == region && i.slot != slot)
            .collect();
        st.peers
            .retain(|s, p| others.iter().any(|i| i.slot == *s && **i == p.info));
        for info in others {
            if !st.peers.contains_key(&info.slot) {
                let client = st.connect(info);
                st.peers.insert(
                    info.slot,
                    PeerOut {
                        info: info.clone(),
                        client,
                        link: None,
                        broken: None,
                    },
                );
            }
        }
        promote(&mut st);
    }

    /// À appeler régulièrement : liens vers les autres relais (prêts, cassés, retentés).
    pub fn tick(&self) {
        let mut st = self.state.lock().unwrap();
        let first = st.first();
        let mut ready = Vec::new();
        let mut retry = Vec::new();
        for (&slot, p) in &mut st.peers {
            while let Some(e) = p.client.poll() {
                match e {
                    Event::Link(l) => {
                        p.link = Some(l);
                        ready.push(slot);
                    }
                    Event::Error(m) | Event::Offline(m) => {
                        log::info!("relais de région : lien vers le relais {slot} : {m}");
                        p.link = None;
                        p.broken.get_or_insert_with(Instant::now);
                    }
                    _ => {}
                }
            }
            if p.broken
                .is_some_and(|t| t.elapsed() > Duration::from_secs(2))
            {
                retry.push(slot);
            }
        }
        for slot in retry {
            let Some(info) = st.peers.get(&slot).map(|p| p.info.clone()) else {
                continue;
            };
            let client = st.connect(&info);
            if let Some(p) = st.peers.get_mut(&slot) {
                p.client = client;
                p.link = None;
                p.broken = None;
            }
        }
        // Un relais vient d'être relié : le premier lui donne les dernières modifications
        // (celles publiées depuis que le relais du monde lui a remis l'historique).
        if first {
            for slot in ready {
                let Some(l) = st.peers.get(&slot).and_then(|p| p.link.clone()) else {
                    continue;
                };
                for (_, chunk, set) in &st.recent {
                    transport::push(
                        &l,
                        &Request::PeerEdit {
                            chunk: *chunk,
                            data: set.clone(),
                        },
                    );
                }
            }
        }
        promote(&mut st);
    }

    /// Ferme le relais (les joueurs repassent par le relais du monde ou un autre hôte).
    pub fn close(&self) {
        let mut st = self.state.lock().unwrap();
        st.closed = true;
        for c in st.conns.values() {
            let _ = c.tx.send(Inbound::Closed);
        }
    }
}

impl Drop for ShardHandle {
    fn drop(&mut self) {
        self.close();
    }
}

/// Ce relais devient le premier de sa région (celui d'avant est parti) : il republie les
/// dernières modifications, au relais du monde et aux autres relais (déjà reçues : sans
/// effet ; manquées : rattrapées).
fn promote(st: &mut State) {
    let first = st.first();
    if first && !st.was_first {
        for (_, chunk, set) in &st.recent {
            let _ = st.published.send((*chunk, set.clone()));
            for p in st.peers.values() {
                if let Some(l) = &p.link {
                    transport::push(
                        l,
                        &Request::PeerEdit {
                            chunk: *chunk,
                            data: set.clone(),
                        },
                    );
                }
            }
        }
    }
    st.was_first = first;
}

/// Lance le relais de rang `slot` de la région `region` du monde `room` (écoute sur
/// `bind`).
pub fn start(
    bind: &str,
    room: u64,
    region: [i32; 2],
    secret: [u8; 32],
    blocks: u16,
    slot: u16,
) -> Result<ShardHandle> {
    let listener = TcpListener::bind(bind)?;
    let addr = listener.local_addr()?;
    let (private, public) = transport::generate_static();
    let (tx, published) = channel();
    let state = Arc::new(Mutex::new(State {
        room,
        region,
        secret,
        blocks,
        conns: HashMap::new(),
        // Numéros de joueurs propres à ce relais (uniques dans la région).
        next_id: ((slot as u64) << 40) | 1,
        build: zones::Build::default(),
        rate: HashMap::new(),
        published: tx,
        closed: false,
        slot,
        peers: HashMap::new(),
        dial: None,
        recent: VecDeque::new(),
        was_first: true,
    }));
    let st = state.clone();
    std::thread::Builder::new()
        .name("relais de région".into())
        .spawn(move || {
            // Connexions ouvertes par joueur (nommé par le tunnel) : limitées, comme sur le
            // relais du monde.
            let open: Arc<Mutex<HashMap<String, usize>>> = Arc::default();
            for conn in listener.incoming() {
                if st.lock().unwrap().closed {
                    return;
                }
                let Ok(stream) = conn else { continue };
                let st = st.clone();
                let open = open.clone();
                let _ = std::thread::Builder::new()
                    .name("relais de région : joueur".into())
                    .spawn(move || {
                        // Par le tunnel iroh : le préambule d'abord.
                        let local = stream.peer_addr().is_ok_and(|a| a.ip().is_loopback());
                        let who = if local {
                            match crate::tunnel_peer(&stream) {
                                Ok(w) => w,
                                Err(_) => return,
                            }
                        } else {
                            stream.peer_addr().ok().map(|a| a.ip().to_string())
                        };
                        // Connexion locale directe (essais) : pas de limite.
                        let _slot = match &who {
                            Some(w) => match crate::PeerSlot::take_named(&open, w) {
                                Some(s) => Some(s),
                                None => return,
                            },
                            None => None,
                        };
                        let _ = serve(stream, &st, &private);
                    });
            }
        })?;
    log::info!("relais {slot} de la région {region:?} du monde {room} sur {addr}");
    Ok(ShardHandle {
        addr,
        public,
        published,
        state,
    })
}

fn err(code: ErrorCode, message: &str) -> Response {
    Response::Error {
        code,
        message: message.into(),
    }
}

fn serve(stream: TcpStream, state: &Mutex<State>, private: &[u8; 32]) -> Result<()> {
    let mut chan = Channel::accept(stream, private, Duration::from_secs(10))?;
    let id = {
        let mut st = state.lock().unwrap();
        let id = st.next_id;
        st.next_id += 1;
        st.conns.insert(
            id,
            Conn {
                tx: chan.pusher(),
                pseudo: None,
                peer: false,
            },
        );
        id
    };
    let r = serve_loop(&mut chan, state, id);
    let mut st = state.lock().unwrap();
    st.conns.remove(&id);
    st.build.left(id, &[]);
    st.rate.remove(&id);
    r
}

fn serve_loop(chan: &mut Channel, state: &Mutex<State>, id: u64) -> Result<()> {
    loop {
        let req: Request = match chan.recv_any(Duration::from_secs(60))? {
            None => return Ok(()),
            Some(Incoming::Push(bytes)) => {
                chan.send_raw(&bytes)?;
                continue;
            }
            Some(Incoming::Msg(r)) => r,
        };
        if state.lock().unwrap().closed {
            return Ok(());
        }
        let resp = handle(req, state, id);
        if let Some(r) = resp {
            chan.send(&r)?;
        }
    }
}

fn handle(req: Request, state: &Mutex<State>, id: u64) -> Option<Response> {
    let mut st = state.lock().unwrap();
    let logged = st.conns.get(&id).and_then(|c| c.pseudo.clone());
    let peer = st.conns.get(&id).is_some_and(|c| c.peer);
    match req {
        Request::Hello { protocol, .. } => Some(if protocol == proto::PROTOCOL {
            Response::Hello {
                protocol,
                relay: "Relais de région".into(),
                pow_bits: 0,
            }
        } else {
            err(ErrorCode::Protocol, "")
        }),
        Request::Ping => Some(Response::Pong),
        Request::ShardLogin {
            room,
            pseudo,
            expires,
            mac,
        } => {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            let ok = room == st.room
                && expires > now
                && proto::shard_mac(&st.secret, room, &pseudo, expires) == mac;
            if !ok {
                return Some(err(ErrorCode::WrongCredentials, "billet refusé"));
            }
            if let Some(c) = st.conns.get_mut(&id) {
                // Un autre relais de la région : son nom ne peut pas être celui d'un
                // joueur, et seul un hôte de ce monde a pu le signer.
                c.peer = pseudo.starts_with('#');
                c.pseudo = Some(pseudo.clone());
            }
            Some(Response::Welcome {
                pseudo,
                totp: false,
            })
        }
        _ if logged.is_none() => (!req.is_oneway()).then(|| err(ErrorCode::NotLoggedIn, "")),
        // D'un autre relais de la région : la position d'un de ses joueurs, pour les
        // joueurs d'ici qui en sont proches.
        Request::PeerNearby {
            peer: from,
            chunk,
            data,
        } if peer => {
            let msg = Response::FromZone {
                peer: from,
                chunk,
                data,
            };
            for m in st.build.nearby(from, chunk) {
                if let Some(c) = st.conns.get(&m) {
                    transport::push(&c.tx, &msg);
                }
            }
            None
        }
        // D'un autre relais : une modification qu'il a vérifiée, à mettre en ordre.
        Request::PeerToZone { chunk, data } if peer => {
            if st.first() && proto::region_of(chunk) == st.region {
                st.publish(chunk, data);
            }
            None
        }
        // Du premier relais : une modification en ordre.
        Request::PeerEdit { chunk, data } if peer => {
            if proto::region_of(chunk) == st.region {
                st.remember(chunk, &data);
                st.deliver(chunk, &data);
            }
            None
        }
        Request::PeerNearby { .. } | Request::PeerToZone { .. } | Request::PeerEdit { .. } => None,
        _ if peer => None,
        Request::Position { chunk } => {
            let logs = st.build.moved(id, chunk);
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
            None
        }
        Request::Nearby { chunk, data } => {
            // Le pseudo montré est celui du billet (pas celui que le joueur annonce).
            let data = crate::player_as(&data, logged.as_deref().unwrap_or_default())?;
            let msg = Response::FromZone {
                peer: id,
                chunk,
                data,
            };
            for m in st.build.nearby(id, chunk) {
                if let Some(c) = st.conns.get(&m) {
                    transport::push(&c.tx, &msg);
                }
            }
            // Une fois à chaque autre relais de la région, pour ses propres joueurs.
            if let Response::FromZone { data, .. } = msg {
                for l in st.peers.values().filter_map(|p| p.link.as_ref()) {
                    transport::push(
                        l,
                        &Request::PeerNearby {
                            peer: id,
                            chunk,
                            data: data.clone(),
                        },
                    );
                }
            }
            // Modifications pas encore reçues (foule, voir `zones::LIVE_MAX`).
            if st.build.positions.contains_key(&id) {
                let logs = st.build.catch_up(id, Some(chunk));
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
            }
            None
        }
        Request::ToZone { chunk, data } => {
            let Some(BuildMsg::Edit { pos, block }) = game::decode::<BuildMsg>(&data) else {
                return None;
            };
            let in_region = proto::region_of(chunk) == st.region;
            let chunk_ok = [pos[0].div_euclid(16), pos[2].div_euclid(16)] == chunk;
            let mut ok =
                in_region && chunk_ok && (0..HEIGHT).contains(&pos[1]) && block < st.blocks;
            if ok {
                let now = Instant::now();
                let e = st.rate.entry(id).or_insert((now, 0));
                if now.duration_since(e.0).as_secs_f32() >= 1.0 {
                    *e = (now, 0);
                }
                e.1 += 1;
                ok = e.1 <= MAX_EDITS_PER_S;
            }
            if !ok {
                if let Some(c) = st.conns.get(&id) {
                    transport::push(
                        &c.tx,
                        &Response::FromZone {
                            peer: 0,
                            chunk,
                            data: game::encode(&BuildMsg::Reject { pos, peer: id }),
                        },
                    );
                }
                return None;
            }
            let set = game::encode(&BuildMsg::Set {
                pos,
                block,
                by: logged.unwrap_or_default(),
            });
            if st.first() {
                st.publish(chunk, set);
            } else if let Some(l) = st.first_link() {
                // Vérifiée ici, mise en ordre par le premier relais de la région (elle
                // revient par lui, comme pour tous).
                transport::push(l, &Request::PeerToZone { chunk, data: set });
            } else if let Some(c) = st.conns.get(&id) {
                // Pas de lien vers le premier relais pour l'instant : refusée (le jeu la
                // défait), plutôt que deux ordres différents.
                transport::push(
                    &c.tx,
                    &Response::FromZone {
                        peer: 0,
                        chunk,
                        data: game::encode(&BuildMsg::Reject { pos, peer: id }),
                    },
                );
            }
            None
        }
        r => (!r.is_oneway()).then(|| err(ErrorCode::Protocol, "relais de région")),
    }
}
