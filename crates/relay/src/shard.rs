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

use std::collections::HashMap;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use minelib_net::game::{self, BuildMsg};
use minelib_net::proto::{self, ErrorCode, Request, Response};
use minelib_net::transport::{self, Channel, Inbound, Incoming};

use crate::zones;

/// Modifications acceptées par seconde et par joueur.
const MAX_EDITS_PER_S: u32 = 20;
/// Hauteur d'un monde (cases).
const HEIGHT: i32 = 256;

struct Conn {
    tx: Sender<Inbound>,
    pseudo: Option<String>,
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
        st.conns.values().filter(|c| c.pseudo.is_some()).count()
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

/// Lance le relais de la région `region` du monde `room` (écoute sur `bind`).
pub fn start(
    bind: &str,
    room: u64,
    region: [i32; 2],
    secret: [u8; 32],
    blocks: u16,
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
        next_id: 1,
        build: zones::Build::default(),
        rate: HashMap::new(),
        published: tx,
        closed: false,
    }));
    let st = state.clone();
    std::thread::Builder::new()
        .name("relais de région".into())
        .spawn(move || {
            for conn in listener.incoming() {
                if st.lock().unwrap().closed {
                    return;
                }
                let Ok(stream) = conn else { continue };
                let st = st.clone();
                let _ = std::thread::Builder::new()
                    .name("relais de région : joueur".into())
                    .spawn(move || {
                        // Par le tunnel iroh : le préambule d'abord.
                        let local = stream.peer_addr().is_ok_and(|a| a.ip().is_loopback());
                        if local && crate::tunnel_peer(&stream).is_err() {
                            return;
                        }
                        let _ = serve(stream, &st, &private);
                    });
            }
        })?;
    log::info!("relais de la région {region:?} du monde {room} sur {addr}");
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
                c.pseudo = Some(pseudo.clone());
            }
            Some(Response::Welcome {
                pseudo,
                totp: false,
            })
        }
        _ if logged.is_none() => (!req.is_oneway()).then(|| err(ErrorCode::NotLoggedIn, "")),
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
            let zone = proto::zone_of(chunk);
            // D'abord la copie pour le relais du monde, puis les joueurs proches (celui qui
            // a modifié compris : c'est sa confirmation).
            let _ = st.published.send((chunk, set.clone()));
            let mut to = st.build.listeners(chunk, u64::MAX);
            if !to.contains(&id) {
                to.push(id);
            }
            for (m, items) in st.build.mirror_to(zone, &set, &to) {
                if let Some(c) = st.conns.get(&m) {
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
            None
        }
        r => (!r.is_oneway()).then(|| err(ErrorCode::Protocol, "relais de région")),
    }
}
