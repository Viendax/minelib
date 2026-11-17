//! Relais de région : une région d'un monde de construction très peuplé est servie par son
//! propre relais, que fait tourner le jeu d'un joueur sans lien avec ce monde (sa connexion
//! de garant, en arrière-plan, comme les garants des parties FPS). Personne dans le monde
//! n'a intérêt à l'attaquer, et le relais du monde n'a plus à tout transmettre.
//!
//! Il fait deux choses, pour sa région seulement :
//! - les joueurs y entrent avec le billet du relais du monde (`proto::shard_mac`) ;
//! - il transmet les positions aux joueurs proches (moins souvent au loin), et les
//!   modifications de blocs que publie le relais du monde (reçues par la connexion de
//!   l'hôte, `ShardHandle::seed`), dont il garde l'historique compacté pour ceux qui
//!   arrivent.
//!
//! Il ne décide rien : seul le relais du monde accepte une modification, lui donne son rang
//! et la garde (comme pour les objets). Jusqu'à la v126, ce relais validait lui-même les
//! modifications de sa région, le premier relais de la région les mettait en ordre et en
//! rendait compte au relais du monde, qui le croyait : un hôte malveillant pouvait inventer
//! une modification ou l'attribuer à un autre joueur. Il ne peut plus que tarder à
//! transmettre, ou montrer à ses propres joueurs des blocs qui n'existent pas (le jeu les
//! oublie en changeant de relais ; l'histoire du monde, elle, n'en garde rien).
//!
//! S'il disparaît, les joueurs repassent par le relais du monde ou par un autre hôte, qui
//! reçoit tout l'historique de la région : chacun reçoit ce qui lui manque.
//!
//! Une région très peuplée a plusieurs relais, chacun chez un hôte différent. Chaque
//! joueur n'entre que dans l'un d'eux (`proto::shard_for`), et les relais se relient entre
//! eux (`set_peers`) : la position d'un joueur part à ses voisins du même relais, et une
//! fois à chaque autre relais, qui la transmet à ses propres joueurs proches. Les
//! modifications, elles, arrivent à chaque relais directement du relais du monde.

use std::collections::HashMap;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use minelib_net::client::{Client, Cmd, Event, Resolver};
use minelib_net::game::{self, BuildMsg};
use minelib_net::proto::{self, ErrorCode, Request, Response};
use minelib_net::transport::{self, Channel, Inbound, Incoming};

use crate::zones;

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
    conns: HashMap<u64, Conn>,
    next_id: u64,
    build: zones::Build,
    closed: bool,
    /// Rang de ce relais dans sa région, et liens vers les autres (par rang).
    slot: u16,
    peers: HashMap<u16, PeerOut>,
    dial: Option<Dial>,
}

impl State {
    /// Une modification publiée par le relais du monde (dans son ordre) : gardée pour ceux
    /// qui arriveront, et transmise aux joueurs de ce relais proches de là.
    fn deliver(&mut self, set: &[u8]) {
        // Seulement des blocs de cette région (objets et coffres ne passent que par le
        // relais du monde).
        let Some(BuildMsg::Set { pos, .. }) = game::decode::<BuildMsg>(set) else {
            return;
        };
        let chunk = [pos[0].div_euclid(16), pos[2].div_euclid(16)];
        if proto::region_of(chunk) != self.region {
            return;
        }
        let to = self.build.listeners(chunk, u64::MAX);
        for (m, items) in self.build.mirror_to(proto::zone_of(chunk), set, &to) {
            if let Some(c) = self.conns.get(&m) {
                for data in items {
                    crate::push_zone(&c.tx, chunk, data);
                }
            }
        }
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
    state: Arc<Mutex<State>>,
}

impl ShardHandle {
    /// Du relais du monde : l'historique de la région (avant l'arrivée des joueurs), puis
    /// chaque modification qu'il publie, dans son ordre.
    pub fn seed(&self, data: &[u8]) {
        let mut st = self.state.lock().unwrap();
        match game::decode::<BuildMsg>(data) {
            // Blocs regroupés : chacun entre dans l'historique comme une modification
            // (dans un paquet, ceux d'une même case sont rangés du plus ancien au plus
            // récent).
            Some(BuildMsg::Cells(packet)) => {
                let cells = minelib_net::cells::unpack_cells(&packet).unwrap_or_default();
                for (pos, block, seq) in cells {
                    st.deliver(&game::encode(&BuildMsg::Set {
                        pos,
                        block,
                        by: String::new(),
                        seq,
                    }));
                }
            }
            _ => st.deliver(data),
        }
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
    }

    /// À appeler régulièrement : liens vers les autres relais (prêts, cassés, retentés).
    pub fn tick(&self) {
        let mut st = self.state.lock().unwrap();
        let mut retry = Vec::new();
        for (&slot, p) in &mut st.peers {
            while let Some(e) = p.client.poll() {
                match e {
                    Event::Link(l) => p.link = Some(l),
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

/// Lance le relais de rang `slot` de la région `region` du monde `room` (écoute sur
/// `bind`).
pub fn start(
    bind: &str,
    room: u64,
    region: [i32; 2],
    secret: [u8; 32],
    slot: u16,
) -> Result<ShardHandle> {
    let listener = TcpListener::bind(bind)?;
    let addr = listener.local_addr()?;
    let (private, public) = transport::generate_static();
    let state = Arc::new(Mutex::new(State {
        room,
        region,
        secret,
        conns: HashMap::new(),
        // Numéros de joueurs propres à ce relais (uniques dans la région).
        next_id: ((slot as u64) << 40) | 1,
        build: zones::Build::default(),
        closed: false,
        slot,
        peers: HashMap::new(),
        dial: None,
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
    st.build.left(id);
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
            let (name, pose) = game::pose_of(&data)?;
            let pose = Arc::new(transport::PoseOf {
                id: from,
                name,
                pose,
            });
            let relays = st.peers.len() + 1;
            for m in st.build.nearby(from, chunk, relays) {
                if let Some(c) = st.conns.get(&m) {
                    transport::push_pose(&c.tx, &pose);
                }
            }
            None
        }
        Request::PeerNearby { .. } | Request::PeerToZone { .. } | Request::PeerEdit { .. } => None,
        _ if peer => None,
        Request::Position { chunk } => {
            let logs = st.build.moved(id, chunk, &|_| true);
            if let Some(c) = st.conns.get(&id) {
                for (zone, log) in logs {
                    let chunk = [zone[0] * proto::ZONE, zone[1] * proto::ZONE];
                    for data in log {
                        crate::push_zone(&c.tx, chunk, data);
                    }
                }
            }
            None
        }
        Request::Nearby { chunk, data } => {
            // Le pseudo montré est celui du billet (pas celui que le joueur annonce).
            let data = crate::player_as(&data, logged.as_deref().unwrap_or_default())?;
            // Plus de positions que le jeu n'en envoie : pas diffusée.
            if !st.build.pos_due(id) {
                return None;
            }
            // Vers chaque voisin, regroupée avec celles des autres ; périssable (la
            // suivante la remplace). Chaque relais de la région ne sert que sa part des
            // voisins.
            let relays = st.peers.len() + 1;
            let (name, pose) = game::pose_of(&data)?;
            let pose = Arc::new(transport::PoseOf { id, name, pose });
            for m in st.build.nearby(id, chunk, relays) {
                if let Some(c) = st.conns.get(&m) {
                    transport::push_pose(&c.tx, &pose);
                }
            }
            // Une fois à chaque autre relais de la région, pour ses propres joueurs.
            if let Some(fwd) = transport::encode(&Request::PeerNearby {
                peer: id,
                chunk,
                data,
            }) {
                for l in st.peers.values().filter_map(|p| p.link.as_ref()) {
                    transport::push_loose(l, fwd.clone());
                }
            }
            // Modifications pas encore reçues (foule, voir `zones::LIVE_MAX`).
            if st.build.positions.contains_key(&id) {
                let logs = st.build.catch_up(id, Some(chunk), &|_| true);
                if let Some(c) = st.conns.get(&id) {
                    for (zone, log) in logs {
                        let chunk = [zone[0] * proto::ZONE, zone[1] * proto::ZONE];
                        for data in log {
                            crate::push_zone(&c.tx, chunk, data);
                        }
                    }
                }
            }
            None
        }
        // Les modifications ne passent pas par ici : seul le relais du monde les accepte.
        // Une demande arrivée quand même (jeu modifié) est refusée à son auteur.
        Request::ToZone { chunk, data } => {
            if let Some(BuildMsg::Edit { pos, .. }) = game::decode::<BuildMsg>(&data) {
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
            }
            None
        }
        r => (!r.is_oneway()).then(|| err(ErrorCode::Protocol, "relais de région")),
    }
}
