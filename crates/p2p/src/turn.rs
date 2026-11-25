//! (v329) TURN (RFC 8656) : un chemin de plus pour les routes des tunnels, sans passeur et
//! sans machine à nous.
//!
//! Un serveur TURN prête à chaque jeu une adresse publique (son « adresse relayée ») : ce
//! qui y arrive lui est remis, ce qu'il y envoie part de là. Quand les deux bouts d'une
//! route en ont une, le relais du monde donne à chacun celle de l'autre (le saut `turn` de
//! `Response::TunnelWay`) : leurs paquets vont de serveur TURN à serveur TURN. Aucun des
//! deux ne voit l'adresse IP de l'autre, seulement une adresse du serveur ; le serveur voit
//! les deux, comme un passeur seul, sans rien pouvoir lire (QUIC chiffre de bout en bout).
//!
//! Le service retenu est ExpressTURN : 1 000 Go par mois gratuits, sans carte bancaire (il
//! ne peut rien facturer : quota épuisé, TURN s'arrête jusqu'au mois suivant), avec un
//! serveur gratuit en France. Ses identifiants ne sont pas écrits dans le dépôt : un petit
//! programme chez Cloudflare (Worker, gratuit, voir `deploiement/cloudflare-turn`) les
//! donne aux jeux, et son adresse est dans `relais_turn.txt`. Le même programme sait donner
//! ceux du TURN de Cloudflare (identifiants de quelques heures, mais une carte bancaire
//! est demandée pour l'activer). Tout autre service qui donne des identifiants TURN au
//! format des « iceServers » de WebRTC convient, et un serveur à soi (coturn) aussi.
//!
//! Ici : le format des messages (STUN, RFC 8489), et un client qui obtient une adresse
//! relayée, la garde (rafraîchissements, nouveaux identifiants avant la fin des anciens),
//! relie des canaux vers les adresses relayées des autres bouts et porte les paquets.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use hmac::{Hmac, Mac};
use md5::{Digest, Md5};
use sha1::Sha1;
use tokio::sync::{oneshot, watch, Notify};

// ───────────────────────────── Messages STUN ─────────────────────────────

const MAGIC: u32 = 0x2112_A442;
const HEADER: usize = 20;

const ALLOCATE: u16 = 0x003;
const REFRESH: u16 = 0x004;
const DATA: u16 = 0x007;
const CHANNEL_BIND: u16 = 0x009;

const REQUEST: u16 = 0x0000;
const INDICATION: u16 = 0x0010;
const SUCCESS: u16 = 0x0100;
const FAILURE: u16 = 0x0110;

const A_USERNAME: u16 = 0x0006;
const A_INTEGRITY: u16 = 0x0008;
const A_ERROR: u16 = 0x0009;
const A_CHANNEL: u16 = 0x000C;
const A_LIFETIME: u16 = 0x000D;
const A_PEER: u16 = 0x0012;
const A_DATA: u16 = 0x0013;
const A_REALM: u16 = 0x0014;
const A_NONCE: u16 = 0x0015;
const A_RELAYED: u16 = 0x0016;
const A_TRANSPORT: u16 = 0x0019;
#[cfg(test)]
const A_MAPPED: u16 = 0x0020;

/// Type d'un message : méthode et classe mêlées bit à bit (RFC 8489, 5).
fn kind(method: u16, class: u16) -> u16 {
    (method & 0x000F) | ((method & 0x0070) << 1) | ((method & 0x0F80) << 2) | class
}

fn parts(t: u16) -> (u16, u16) {
    let class = t & 0x0110;
    let m = t & !0x0110;
    (
        (m & 0x000F) | ((m & 0x00E0) >> 1) | ((m & 0x3E00) >> 2),
        class,
    )
}

fn hmac_sha1(key: &[u8], data: &[u8]) -> [u8; 20] {
    let mut m = <Hmac<Sha1> as Mac>::new_from_slice(key).expect("toute clé convient");
    m.update(data);
    m.finalize().into_bytes().into()
}

/// Clé des identifiants « longue durée » : MD5(utilisateur:domaine:mot de passe).
fn long_term_key(user: &str, realm: &str, pass: &str) -> [u8; 16] {
    Md5::digest(format!("{user}:{realm}:{pass}").as_bytes()).into()
}

/// Un message reçu.
#[derive(Clone, Debug)]
struct Msg {
    method: u16,
    class: u16,
    tid: [u8; 12],
    attrs: Vec<(u16, Vec<u8>)>,
    /// Où commence MESSAGE-INTEGRITY dans `raw` (pour la vérifier).
    integrity_at: Option<usize>,
    raw: Vec<u8>,
}

impl Msg {
    fn attr(&self, t: u16) -> Option<&[u8]> {
        self.attrs.iter().find(|a| a.0 == t).map(|a| a.1.as_slice())
    }

    fn error(&self) -> u16 {
        self.attr(A_ERROR)
            .filter(|v| v.len() >= 4)
            .map_or(0, |v| u16::from(v[2] & 7) * 100 + u16::from(v[3]))
    }

    fn addr(&self, t: u16) -> Option<SocketAddr> {
        xor_decode(self.attr(t)?, &self.tid)
    }

    fn lifetime(&self) -> Option<u32> {
        let v = self.attr(A_LIFETIME)?;
        Some(u32::from_be_bytes(v.get(..4)?.try_into().ok()?))
    }

    /// MESSAGE-INTEGRITY présente et juste pour la clé `key`.
    fn integrity_ok(&self, key: &[u8]) -> bool {
        let (Some(at), Some(mac)) = (self.integrity_at, self.attr(A_INTEGRITY)) else {
            return false;
        };
        let mut head = self.raw[..at].to_vec();
        let len = (at - HEADER + 24) as u16;
        head[2..4].copy_from_slice(&len.to_be_bytes());
        same(&hmac_sha1(key, &head), mac)
    }
}

fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn parse(p: &[u8]) -> Option<Msg> {
    if p.len() < HEADER || p[0] & 0xC0 != 0 {
        return None;
    }
    let t = u16::from_be_bytes([p[0], p[1]]);
    let len = u16::from_be_bytes([p[2], p[3]]) as usize;
    if u32::from_be_bytes(p[4..8].try_into().ok()?) != MAGIC
        || len & 3 != 0
        || HEADER + len > p.len()
    {
        return None;
    }
    let (method, class) = parts(t);
    let tid: [u8; 12] = p[8..20].try_into().ok()?;
    let end = HEADER + len;
    let mut attrs = Vec::new();
    let mut integrity_at = None;
    let mut i = HEADER;
    while i + 4 <= end {
        let at = u16::from_be_bytes([p[i], p[i + 1]]);
        let al = u16::from_be_bytes([p[i + 2], p[i + 3]]) as usize;
        if i + 4 + al > end {
            return None;
        }
        // (Après MESSAGE-INTEGRITY, seule FINGERPRINT peut venir : rien à en garder.)
        if integrity_at.is_none() {
            if at == A_INTEGRITY {
                integrity_at = Some(i);
            }
            attrs.push((at, p[i + 4..i + 4 + al].to_vec()));
        }
        i += 4 + al.div_ceil(4) * 4;
    }
    Some(Msg {
        method,
        class,
        tid,
        attrs,
        integrity_at,
        raw: p[..end].to_vec(),
    })
}

/// Un message à envoyer.
struct Out(Vec<u8>);

impl Out {
    fn new(method: u16, class: u16, tid: &[u8; 12]) -> Self {
        let mut b = Vec::with_capacity(128);
        b.extend_from_slice(&kind(method, class).to_be_bytes());
        b.extend_from_slice(&[0, 0]);
        b.extend_from_slice(&MAGIC.to_be_bytes());
        b.extend_from_slice(tid);
        Out(b)
    }

    fn attr(mut self, t: u16, v: &[u8]) -> Self {
        self.0.extend_from_slice(&t.to_be_bytes());
        self.0.extend_from_slice(&(v.len() as u16).to_be_bytes());
        self.0.extend_from_slice(v);
        while self.0.len() & 3 != 0 {
            self.0.push(0);
        }
        self
    }

    /// MESSAGE-INTEGRITY sur tout ce qui précède.
    fn integrity(mut self, key: &[u8]) -> Self {
        let len = (self.0.len() - HEADER + 24) as u16;
        self.0[2..4].copy_from_slice(&len.to_be_bytes());
        let mac = hmac_sha1(key, &self.0);
        self.attr(A_INTEGRITY, &mac)
    }

    fn finish(mut self) -> Vec<u8> {
        let len = (self.0.len() - HEADER) as u16;
        self.0[2..4].copy_from_slice(&len.to_be_bytes());
        self.0
    }
}

fn xor_encode(a: SocketAddr, tid: &[u8; 12]) -> Vec<u8> {
    let port = a.port() ^ (MAGIC >> 16) as u16;
    let mut v = vec![0];
    match a.ip() {
        IpAddr::V4(ip) => {
            v.push(1);
            v.extend_from_slice(&port.to_be_bytes());
            v.extend_from_slice(&(u32::from(ip) ^ MAGIC).to_be_bytes());
        }
        IpAddr::V6(ip) => {
            v.push(2);
            v.extend_from_slice(&port.to_be_bytes());
            let mut mask = [0u8; 16];
            mask[..4].copy_from_slice(&MAGIC.to_be_bytes());
            mask[4..].copy_from_slice(tid);
            v.extend(ip.octets().iter().zip(mask).map(|(x, m)| x ^ m));
        }
    }
    v
}

fn xor_decode(v: &[u8], tid: &[u8; 12]) -> Option<SocketAddr> {
    let port = u16::from_be_bytes(v.get(2..4)?.try_into().ok()?) ^ (MAGIC >> 16) as u16;
    match v.get(1)? {
        1 => {
            let ip = u32::from_be_bytes(v.get(4..8)?.try_into().ok()?) ^ MAGIC;
            Some(SocketAddr::from((Ipv4Addr::from(ip), port)))
        }
        2 => {
            let mut mask = [0u8; 16];
            mask[..4].copy_from_slice(&MAGIC.to_be_bytes());
            mask[4..].copy_from_slice(tid);
            let b: [u8; 16] = v.get(4..20)?.try_into().ok()?;
            let ip: [u8; 16] = std::array::from_fn(|i| b[i] ^ mask[i]);
            Some(SocketAddr::from((Ipv6Addr::from(ip), port)))
        }
        _ => None,
    }
}

/// ChannelData : numéro de canal, longueur, contenu (sans bourrage : c'est de l'UDP).
fn channel_data(ch: u16, data: &[u8]) -> Vec<u8> {
    let mut p = Vec::with_capacity(4 + data.len());
    p.extend_from_slice(&ch.to_be_bytes());
    p.extend_from_slice(&(data.len() as u16).to_be_bytes());
    p.extend_from_slice(data);
    p
}

// ───────────────────────────── Identifiants ─────────────────────────────

/// D'où viennent les identifiants TURN.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// Un service web qui les donne (le Worker de Cloudflare) : réponse au format des
    /// « iceServers » de WebRTC, plus `ttl` (secondes) s'il le sait.
    Service(String),
    /// Des identifiants fixes : serveur à soi, essais.
    Fixed {
        server: String,
        user: String,
        pass: String,
    },
}

/// `relais_turn.txt` : une source par ligne, `https://…` (service) ou
/// `turn:<hôte>:<port> <utilisateur> <mot de passe>` (fixe). Lignes vides et « # » ignorées.
pub fn parse_sources(text: &str) -> Vec<Source> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| {
            if l.starts_with("https://") || l.starts_with("http://") {
                return Some(Source::Service(l.to_string()));
            }
            let mut w = l.split_whitespace();
            let server = w.next()?.strip_prefix("turn:")?;
            let server = server.split('?').next()?.to_string();
            Some(Source::Fixed {
                server,
                user: w.next()?.to_string(),
                pass: w.next()?.to_string(),
            })
        })
        .collect()
}

/// Sources de ce jeu : `relais_turn.txt` (compilé dans le jeu), ou la variable
/// `MINELIB_TURN` (lignes séparées par des virgules) qui la remplace.
pub fn sources() -> Vec<Source> {
    match std::env::var("MINELIB_TURN") {
        Ok(v) => parse_sources(&v.replace(',', "\n")),
        Err(_) => parse_sources(include_str!("../../../relais_turn.txt")),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Creds {
    /// `hôte:port`, UDP.
    servers: Vec<String>,
    user: String,
    pass: String,
    ttl: Duration,
}

/// Réponse d'un service d'identifiants : `{"iceServers": [...]}`, une liste, ou un seul
/// serveur ; `urls` seule ou en liste. Seuls les serveurs `turn:` en UDP sont gardés.
fn parse_ice(text: &str) -> Option<Creds> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let ttl = v
        .get("ttl")
        .and_then(|t| t.as_u64())
        .map_or(Duration::from_secs(3600), Duration::from_secs);
    let list: Vec<serde_json::Value> = match &v {
        serde_json::Value::Array(a) => a.clone(),
        serde_json::Value::Object(o) => match o.get("iceServers") {
            Some(serde_json::Value::Array(a)) => a.clone(),
            Some(one @ serde_json::Value::Object(_)) => vec![one.clone()],
            _ => vec![v.clone()],
        },
        _ => return None,
    };
    for s in list {
        let (Some(user), Some(pass)) = (
            s.get("username").and_then(|x| x.as_str()),
            s.get("credential").and_then(|x| x.as_str()),
        ) else {
            continue;
        };
        let urls: Vec<String> = match s.get("urls").or_else(|| s.get("url")) {
            Some(serde_json::Value::String(u)) => vec![u.clone()],
            Some(serde_json::Value::Array(a)) => a
                .iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect(),
            _ => continue,
        };
        let servers: Vec<String> = urls
            .iter()
            .filter_map(|u| {
                let rest = u.strip_prefix("turn:")?;
                let (hostport, query) = rest.split_once('?').unwrap_or((rest, ""));
                if !query.is_empty() && !query.contains("transport=udp") {
                    return None;
                }
                Some(if hostport.contains(':') {
                    hostport.to_string()
                } else {
                    format!("{hostport}:3478")
                })
            })
            .collect();
        if !servers.is_empty() {
            return Some(Creds {
                servers,
                user: user.to_string(),
                pass: pass.to_string(),
                ttl,
            });
        }
    }
    None
}

fn fetch(source: &Source) -> Option<Creds> {
    match source {
        Source::Fixed { server, user, pass } => Some(Creds {
            servers: vec![server.clone()],
            user: user.clone(),
            pass: pass.clone(),
            ttl: Duration::from_secs(365 * 86400),
        }),
        Source::Service(url) => {
            let agent: ureq::Agent = ureq::Agent::config_builder()
                .tls_config(
                    ureq::tls::TlsConfig::builder()
                        .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                        .build(),
                )
                .timeout_global(Some(Duration::from_secs(10)))
                .build()
                .into();
            let body = agent
                .get(url)
                .header("User-Agent", "minelib")
                .call()
                .ok()?
                .body_mut()
                .read_to_string()
                .ok()?;
            let c = parse_ice(&body);
            if c.is_none() {
                log::info!("TURN : réponse sans serveur TURN utilisable");
            }
            c
        }
    }
}

// ───────────────────────────── Client ─────────────────────────────

/// Durée demandée pour une allocation ; elle est rafraîchie à mi-chemin.
const LIFETIME: u32 = 600;
/// Un canal qui sert est relié de nouveau à ce rythme (il en tient 10 minutes).
#[cfg(not(test))]
const REBIND: Duration = Duration::from_secs(240);
#[cfg(test)]
const REBIND: Duration = Duration::from_secs(2);
/// Une liaison demandée sans réponse est redemandée après ce temps.
const ASK_AGAIN: Duration = Duration::from_secs(3);
/// Un pair à qui rien n'est parti depuis ce temps est oublié ; son canal ne resert qu'un
/// quart d'heure après (le serveur le garde 10 minutes, puis 5 sans pouvoir le relier
/// ailleurs).
const IDLE: Duration = Duration::from_secs(600);
const CHANNEL_REST: Duration = Duration::from_secs(900);
/// Pairs à la fois, au plus.
const PEERS_MAX: usize = 512;
/// Délais d'attente d'une réponse, une réémission après chacun (UDP).
const RTO: [u64; 4] = [500, 1000, 2000, 4000];
/// Nouvel essai d'allocation après un échec : de 15 s à 10 min.
const RETRY_MIN: Duration = Duration::from_secs(15);
const RETRY_MAX: Duration = Duration::from_secs(600);

/// Ce que le client remet : les paquets reçus d'un pair (son adresse relayée), et son
/// adresse relayée quand elle change (rien : il n'en a plus).
pub type Sink = Box<dyn Fn(SocketAddr, &[u8]) + Send + Sync>;
pub type Told = Box<dyn Fn(Option<SocketAddrV4>) + Send + Sync>;

struct Auth {
    user: String,
    pass: String,
    realm: String,
    nonce: Vec<u8>,
    key: [u8; 16],
}

struct Peer {
    channel: u16,
    bound: Option<Instant>,
    asked: Option<Instant>,
    used: Instant,
}

#[derive(Default)]
struct State {
    server: Option<SocketAddr>,
    auth: Option<Auth>,
    relayed: Option<SocketAddrV4>,
    refresh_at: Option<Instant>,
    renew_at: Option<Instant>,
    retry_at: Option<Instant>,
    retry: Duration,
    peers: HashMap<SocketAddr, Peer>,
    channels: HashMap<u16, SocketAddr>,
    freed: Vec<(u16, Instant)>,
    next: u16,
    waiting: HashMap<[u8; 12], oneshot::Sender<Msg>>,
    sent: u64,
    received: u64,
}

impl State {
    fn take_channel(&mut self, now: Instant) -> Option<u16> {
        if let Some(i) = self
            .freed
            .iter()
            .position(|(_, t)| now.duration_since(*t) >= CHANNEL_REST)
        {
            return Some(self.freed.remove(i).0);
        }
        if self.next < 0x4000 {
            self.next = 0x4000;
        }
        (self.next <= 0x4FFF).then(|| {
            self.next += 1;
            self.next - 1
        })
    }

    /// L'allocation est perdue : plus d'adresse relayée, plus de canal.
    fn lost(&mut self) {
        self.relayed = None;
        self.refresh_at = None;
        self.renew_at = None;
        self.channels.clear();
        for p in self.peers.values_mut() {
            p.bound = None;
            p.asked = None;
        }
    }
}

/// Client TURN d'un transport : une allocation, chez le premier serveur qui en donne une.
pub struct TurnClient {
    sock: Arc<tokio::net::UdpSocket>,
    st: Mutex<State>,
    wake: Notify,
    stop: watch::Sender<bool>,
    sources: Vec<Source>,
    sink: Sink,
    told: Told,
}

impl Drop for TurnClient {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
        // (Le nœud s'en va : son adresse relayée n'est plus à dire.)
        if self
            .st
            .get_mut()
            .map(|st| st.relayed.is_some())
            .unwrap_or(false)
        {
            (self.told)(None);
        }
    }
}

impl TurnClient {
    /// Démarre le client (dans le moteur `rt`) : il cherche des identifiants, obtient une
    /// adresse relayée, la garde, et réessaie tant qu'il vit.
    pub fn start(
        rt: &tokio::runtime::Handle,
        sources: Vec<Source>,
        sink: Sink,
        told: Told,
    ) -> std::io::Result<Arc<Self>> {
        let sock = std::net::UdpSocket::bind("0.0.0.0:0")?;
        sock.set_nonblocking(true)?;
        let sock = {
            let _g = rt.enter();
            Arc::new(tokio::net::UdpSocket::from_std(sock)?)
        };
        let (stop, stopped) = watch::channel(false);
        let me = Arc::new(TurnClient {
            sock: sock.clone(),
            st: Mutex::new(State {
                retry: RETRY_MIN,
                ..Default::default()
            }),
            wake: Notify::new(),
            stop,
            sources,
            sink,
            told,
        });
        let weak = Arc::downgrade(&me);
        let mut s1 = stopped.clone();
        rt.spawn(async move {
            let mut buf = vec![0u8; 2048];
            loop {
                tokio::select! {
                    r = sock.recv_from(&mut buf) => match r {
                        Ok((n, from)) => match weak.upgrade() {
                            Some(me) => me.on_packet(from, &buf[..n]),
                            None => return,
                        },
                        // (Windows : un paquet vers un port fermé revient en erreur.)
                        Err(_) => tokio::time::sleep(Duration::from_millis(5)).await,
                    },
                    _ = s1.changed() => return,
                }
            }
        });
        let weak: Weak<TurnClient> = Arc::downgrade(&me);
        let mut s2 = stopped;
        rt.spawn(async move {
            loop {
                let Some(me) = weak.upgrade() else { return };
                me.tend().await;
                let pause = tokio::time::sleep(Duration::from_millis(500));
                tokio::select! {
                    _ = me.wake.notified() => {}
                    _ = pause => {}
                    _ = s2.changed() => return,
                }
            }
        });
        Ok(me)
    }

    /// Notre adresse relayée, s'il y en a une.
    pub fn relayed(&self) -> Option<SocketAddrV4> {
        self.st.lock().unwrap().relayed
    }

    /// Paquets envoyés et reçus par le serveur TURN (essais, journal).
    pub fn counts(&self) -> (u64, u64) {
        let st = self.st.lock().unwrap();
        (st.sent, st.received)
    }

    /// Un paquet vers l'adresse relayée `peer` (celle de l'autre bout d'une route). Tant
    /// que son canal n'est pas relié, le paquet est perdu (QUIC le renvoie) et la
    /// liaison est demandée.
    pub fn send(&self, peer: SocketAddr, data: &[u8]) {
        let mut st = self.st.lock().unwrap();
        let (Some(server), Some(_)) = (st.server, st.relayed) else {
            return;
        };
        let now = Instant::now();
        let ch = match st.peers.get_mut(&peer) {
            Some(p) => {
                p.used = now;
                p.bound.map(|_| p.channel)
            }
            None => {
                if st.peers.len() >= PEERS_MAX {
                    return;
                }
                let Some(ch) = st.take_channel(now) else {
                    return;
                };
                st.peers.insert(
                    peer,
                    Peer {
                        channel: ch,
                        bound: None,
                        asked: None,
                        used: now,
                    },
                );
                None
            }
        };
        match ch {
            Some(ch) => {
                st.sent += 1;
                drop(st);
                let _ = self.sock.try_send_to(&channel_data(ch, data), server);
            }
            None => {
                drop(st);
                self.wake.notify_one();
            }
        }
    }

    fn on_packet(&self, from: SocketAddr, p: &[u8]) {
        let mut st = self.st.lock().unwrap();
        if st.server != Some(from) {
            return;
        }
        if p.len() >= 4 && (0x40..0x80).contains(&p[0]) {
            let ch = u16::from_be_bytes([p[0], p[1]]);
            let len = u16::from_be_bytes([p[2], p[3]]) as usize;
            let (Some(data), Some(&peer)) = (p.get(4..4 + len), st.channels.get(&ch)) else {
                return;
            };
            st.received += 1;
            drop(st);
            (self.sink)(peer, data);
            return;
        }
        let Some(m) = parse(p) else {
            return;
        };
        if m.class == INDICATION && m.method == DATA {
            if let (Some(peer), Some(data)) = (m.addr(A_PEER), m.attr(A_DATA)) {
                st.received += 1;
                drop(st);
                (self.sink)(peer, data);
            }
            return;
        }
        if m.class == SUCCESS || m.class == FAILURE {
            if let Some(tx) = st.waiting.remove(&m.tid) {
                let _ = tx.send(m);
            }
        }
    }

    /// Une demande au serveur, réémise tant qu'il ne répond pas. Les identifiants sont
    /// joints dès que le domaine est connu ; un refus 401 ou 438 (le serveur donne son
    /// domaine, ou un autre nonce) est suivi d'un nouvel essai. Erreur : le code du refus
    /// (0 : pas de réponse, ou réponse fausse).
    async fn transact(&self, method: u16, attrs: &[(u16, Vec<u8>)]) -> Result<Msg, u16> {
        for _round in 0..3 {
            let tid: [u8; 12] = rand::random();
            let (pkt, key, server, rx) = {
                let mut st = self.st.lock().unwrap();
                let server = st.server.ok_or(0u16)?;
                let mut out = Out::new(method, REQUEST, &tid);
                for (t, v) in attrs {
                    out = out.attr(*t, v);
                }
                let mut key = None;
                if let Some(a) = st.auth.as_ref().filter(|a| !a.realm.is_empty()) {
                    out = out
                        .attr(A_USERNAME, a.user.as_bytes())
                        .attr(A_REALM, a.realm.as_bytes())
                        .attr(A_NONCE, &a.nonce)
                        .integrity(&a.key);
                    key = Some(a.key);
                }
                let (tx, rx) = oneshot::channel();
                st.waiting.insert(tid, tx);
                (out.finish(), key, server, rx)
            };
            let mut rx = rx;
            let mut got = None;
            for wait in RTO {
                let _ = self.sock.send_to(&pkt, server).await;
                tokio::select! {
                    r = &mut rx => {
                        got = r.ok();
                        break;
                    }
                    _ = tokio::time::sleep(Duration::from_millis(wait)) => {}
                }
            }
            self.st.lock().unwrap().waiting.remove(&tid);
            let m = got.ok_or(0u16)?;
            if m.class == SUCCESS {
                // (Une réponse fausse ne compte pas.)
                if let Some(k) = key {
                    if m.integrity_at.is_some() && !m.integrity_ok(&k) {
                        return Err(0);
                    }
                }
                return Ok(m);
            }
            let code = m.error();
            if code == 401 || code == 438 {
                let mut st = self.st.lock().unwrap();
                if let (Some(a), Some(nonce)) = (st.auth.as_mut(), m.attr(A_NONCE)) {
                    if let Some(realm) = m.attr(A_REALM) {
                        a.realm = String::from_utf8_lossy(realm).into_owned();
                    }
                    a.nonce = nonce.to_vec();
                    a.key = long_term_key(&a.user, &a.realm, &a.pass);
                    continue;
                }
            }
            return Err(code);
        }
        Err(0)
    }

    async fn tend(self: &Arc<Self>) {
        let now = Instant::now();
        let (alloc, refresh) = {
            let st = self.st.lock().unwrap();
            let renew = st.renew_at.is_some_and(|t| now >= t);
            let alloc = (st.relayed.is_none() || renew) && st.retry_at.is_none_or(|t| now >= t);
            (alloc, st.refresh_at.is_some_and(|t| now >= t))
        };
        if alloc {
            self.allocate().await;
            return;
        }
        if refresh {
            self.refresh().await;
        }
        // Canaux à relier (nouveaux, ou à garder), pairs à oublier.
        let mut todo = Vec::new();
        {
            let mut st = self.st.lock().unwrap();
            if st.relayed.is_none() {
                return;
            }
            let mut gone = Vec::new();
            for (addr, p) in st.peers.iter_mut() {
                if now.duration_since(p.used) >= IDLE {
                    gone.push((*addr, p.channel));
                    continue;
                }
                let due = match p.bound {
                    None => p.asked.is_none_or(|t| now.duration_since(t) >= ASK_AGAIN),
                    Some(t) => {
                        now.duration_since(t) >= REBIND
                            && p.asked.is_none_or(|t| now.duration_since(t) >= ASK_AGAIN)
                    }
                };
                if due {
                    p.asked = Some(now);
                    todo.push((*addr, p.channel));
                }
            }
            for (addr, ch) in gone {
                st.peers.remove(&addr);
                st.channels.remove(&ch);
                st.freed.push((ch, now));
            }
        }
        for (peer, ch) in todo {
            let me = self.clone();
            tokio::spawn(async move { me.bind(peer, ch).await });
        }
    }

    async fn allocate(self: &Arc<Self>) {
        let had = self.st.lock().unwrap().relayed.is_some();
        let mut creds = None;
        for s in self.sources.clone() {
            let got = tokio::task::spawn_blocking(move || fetch(&s))
                .await
                .ok()
                .flatten();
            if got.is_some() {
                creds = got;
                break;
            }
        }
        let mut servers = Vec::new();
        if let Some(c) = &creds {
            for s in &c.servers {
                if let Ok(addrs) = tokio::net::lookup_host(s.as_str()).await {
                    servers.extend(addrs.filter(|a| a.is_ipv4()).take(1));
                }
            }
        }
        // L'ancienne allocation est rendue d'abord : une seule par adresse de ce client.
        if had {
            let _ = self
                .transact(REFRESH, &[(A_LIFETIME, 0u32.to_be_bytes().to_vec())])
                .await;
        }
        {
            let mut st = self.st.lock().unwrap();
            st.lost();
            st.server = None;
            st.auth = None;
        }
        let Some(c) = creds.filter(|_| !servers.is_empty()) else {
            self.failed(had, "pas d'identifiants");
            return;
        };
        for server in servers {
            {
                let mut st = self.st.lock().unwrap();
                st.server = Some(server);
                st.auth = Some(Auth {
                    user: c.user.clone(),
                    pass: c.pass.clone(),
                    realm: String::new(),
                    nonce: Vec::new(),
                    key: [0; 16],
                });
            }
            let attrs = [
                (A_TRANSPORT, vec![17, 0, 0, 0]),
                (A_LIFETIME, LIFETIME.to_be_bytes().to_vec()),
            ];
            match self.transact(ALLOCATE, &attrs).await {
                Ok(m) => {
                    let Some(SocketAddr::V4(relayed)) = m.addr(A_RELAYED) else {
                        continue;
                    };
                    let life = Duration::from_secs(u64::from(m.lifetime().unwrap_or(LIFETIME)));
                    let now = Instant::now();
                    {
                        let mut st = self.st.lock().unwrap();
                        st.relayed = Some(relayed);
                        st.refresh_at = Some(now + (life / 2).max(Duration::from_secs(5)));
                        let renew = c
                            .ttl
                            .mul_f32(0.8)
                            .min(c.ttl.saturating_sub(Duration::from_secs(60)));
                        st.renew_at = Some(now + renew.max(Duration::from_secs(30)));
                        st.retry = RETRY_MIN;
                        st.retry_at = None;
                    }
                    log::info!("TURN : adresse relayée obtenue");
                    (self.told)(Some(relayed));
                    self.wake.notify_one();
                    return;
                }
                Err(code) => log::info!("TURN : allocation refusée ({code})"),
            }
        }
        {
            let mut st = self.st.lock().unwrap();
            st.server = None;
            st.auth = None;
        }
        self.failed(had, "aucun serveur ne répond");
    }

    fn failed(&self, had: bool, why: &str) {
        {
            let mut st = self.st.lock().unwrap();
            st.retry_at = Some(Instant::now() + st.retry);
            st.retry = (st.retry * 2).min(RETRY_MAX);
        }
        log::info!("TURN : pas d'adresse relayée ({why}), nouvel essai plus tard");
        if had {
            (self.told)(None);
        }
    }

    async fn refresh(self: &Arc<Self>) {
        match self
            .transact(REFRESH, &[(A_LIFETIME, LIFETIME.to_be_bytes().to_vec())])
            .await
        {
            Ok(m) => {
                let life = Duration::from_secs(u64::from(m.lifetime().unwrap_or(LIFETIME)));
                self.st.lock().unwrap().refresh_at =
                    Some(Instant::now() + (life / 2).max(Duration::from_secs(5)));
            }
            Err(code) => {
                log::info!("TURN : allocation perdue ({code})");
                self.st.lock().unwrap().lost();
                (self.told)(None);
            }
        }
    }

    async fn bind(self: Arc<Self>, peer: SocketAddr, ch: u16) {
        let tid_for_peer = [0u8; 12];
        let attrs = [
            (A_CHANNEL, [ch.to_be_bytes().as_slice(), &[0, 0]].concat()),
            (A_PEER, xor_encode(peer, &tid_for_peer)),
        ];
        let r = self.transact(CHANNEL_BIND, &attrs).await;
        let mut st = self.st.lock().unwrap();
        match r {
            Ok(_) => {
                if let Some(p) = st.peers.get_mut(&peer).filter(|p| p.channel == ch) {
                    p.bound = Some(Instant::now());
                    p.asked = None;
                    st.channels.insert(ch, peer);
                }
            }
            // Refusé pour de bon (adresse interdite, canal déjà pris ailleurs) : oublié.
            Err(400) | Err(403) => {
                if st.peers.get(&peer).is_some_and(|p| p.channel == ch) {
                    st.peers.remove(&peer);
                    st.channels.remove(&ch);
                    st.freed.push((ch, Instant::now()));
                }
            }
            Err(_) => {}
        }
    }
}

/// Un petit serveur TURN (RFC 8656, UDP) pour les essais, ici et dans les autres paquets :
/// allocations avec identifiants à long terme, rafraîchissements, canaux, ChannelData et
/// indications Data. Assez pour essayer un client de bout en bout, sur cette machine.
#[doc(hidden)]
pub mod essai {
    use super::*;

    /// Serveur TURN minimal (RFC 8656, UDP) : allocations avec identifiants à long terme,
    /// rafraîchissements, canaux, ChannelData et indications Data. Assez pour essayer le
    /// client de bout en bout, chez soi.
    pub struct MiniTurn {
        pub addr: SocketAddr,
        /// Allocations faites, et paquets relayés (essais).
        pub allocations: Arc<std::sync::atomic::AtomicUsize>,
        pub relayed: Arc<std::sync::atomic::AtomicUsize>,
        _stop: watch::Sender<bool>,
    }

    struct Alloc {
        relay: Arc<tokio::net::UdpSocket>,
        /// Permissions (adresse IP du pair) et canaux (numéro → pair).
        perms: std::collections::HashSet<IpAddr>,
        channels: HashMap<u16, SocketAddr>,
    }

    pub const MINI_USER: &str = "essai";
    pub const MINI_PASS: &str = "secret";
    const MINI_REALM: &str = "minelib.test";

    impl MiniTurn {
        pub fn start(rt: &tokio::runtime::Handle) -> MiniTurn {
            let _g = rt.enter();
            let std_sock = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
            std_sock.set_nonblocking(true).unwrap();
            let addr = std_sock.local_addr().unwrap();
            let sock = Arc::new(tokio::net::UdpSocket::from_std(std_sock).unwrap());
            let (stop, mut stopped) = watch::channel(false);
            let allocations = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let relayed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let (allocs2, relayed2) = (allocations.clone(), relayed.clone());
            rt.spawn(async move {
                let allocs: Arc<Mutex<HashMap<SocketAddr, Alloc>>> = Default::default();
                let key = long_term_key(MINI_USER, MINI_REALM, MINI_PASS);
                let mut buf = vec![0u8; 2048];
                loop {
                    let (n, from) = tokio::select! {
                        r = sock.recv_from(&mut buf) => match r { Ok(x) => x, Err(_) => continue },
                        _ = stopped.changed() => return,
                    };
                    let p = buf[..n].to_vec();
                    if p.len() >= 4 && (0x40..0x80).contains(&p[0]) {
                        let ch = u16::from_be_bytes([p[0], p[1]]);
                        let len = u16::from_be_bytes([p[2], p[3]]) as usize;
                        let g = allocs.lock().unwrap();
                        if let (Some(a), Some(data)) = (g.get(&from), p.get(4..4 + len)) {
                            if let Some(peer) = a.channels.get(&ch) {
                                let _ = a.relay.try_send_to(data, *peer);
                                relayed2.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            }
                        }
                        continue;
                    }
                    let Some(m) = parse(&p) else { continue };
                    if m.class != REQUEST {
                        continue;
                    }
                    let reply = |class: u16, attrs: Vec<(u16, Vec<u8>)>, signed: bool| {
                        let mut o = Out::new(m.method, class, &m.tid);
                        for (t, v) in attrs {
                            o = o.attr(t, &v);
                        }
                        if signed {
                            o = o.integrity(&key);
                        }
                        o.finish()
                    };
                    let err = |code: u16| {
                        vec![(A_ERROR, vec![0, 0, (code / 100) as u8, (code % 100) as u8])]
                    };
                    // Identifiants : sans eux (ou faux), 401 avec le domaine et un nonce.
                    if m.attr(A_USERNAME) != Some(MINI_USER.as_bytes()) || !m.integrity_ok(&key) {
                        let mut a = err(401);
                        a.push((A_REALM, MINI_REALM.as_bytes().to_vec()));
                        a.push((A_NONCE, b"nonce-essai".to_vec()));
                        let _ = sock.send_to(&reply(FAILURE, a, false), from).await;
                        continue;
                    }
                    let out = match m.method {
                        ALLOCATE => {
                            if allocs.lock().unwrap().contains_key(&from) {
                                reply(FAILURE, err(437), true)
                            } else {
                                let relay =
                                    tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
                                let raddr = relay.local_addr().unwrap();
                                let relay = Arc::new(relay);
                                allocs.lock().unwrap().insert(
                                    from,
                                    Alloc {
                                        relay: relay.clone(),
                                        perms: Default::default(),
                                        channels: Default::default(),
                                    },
                                );
                                allocs2.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                // Ce qui arrive à l'adresse relayée : au client, si le pair
                                // a sa permission (canal, sinon indication Data).
                                let (allocs, sock, client, relayed2) =
                                    (allocs.clone(), sock.clone(), from, relayed2.clone());
                                tokio::spawn(async move {
                                    let mut b = vec![0u8; 2048];
                                    while let Ok((n, peer)) = relay.recv_from(&mut b).await {
                                        let out = {
                                            let g = allocs.lock().unwrap();
                                            let Some(a) = g.get(&client) else { return };
                                            if !a.perms.contains(&peer.ip()) {
                                                continue;
                                            }
                                            match a.channels.iter().find(|(_, p)| **p == peer) {
                                                Some((ch, _)) => channel_data(*ch, &b[..n]),
                                                None => {
                                                    let tid: [u8; 12] = rand::random();
                                                    Out::new(DATA, INDICATION, &tid)
                                                        .attr(A_PEER, &xor_encode(peer, &tid))
                                                        .attr(A_DATA, &b[..n])
                                                        .finish()
                                                }
                                            }
                                        };
                                        relayed2.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                        let _ = sock.send_to(&out, client).await;
                                    }
                                });
                                reply(
                                    SUCCESS,
                                    vec![
                                        (A_RELAYED, xor_encode(raddr, &m.tid)),
                                        (A_LIFETIME, 600u32.to_be_bytes().to_vec()),
                                    ],
                                    true,
                                )
                            }
                        }
                        REFRESH => {
                            let life = m.lifetime().unwrap_or(600);
                            if life == 0 {
                                allocs.lock().unwrap().remove(&from);
                            }
                            reply(
                                SUCCESS,
                                vec![(A_LIFETIME, life.to_be_bytes().to_vec())],
                                true,
                            )
                        }
                        CHANNEL_BIND => {
                            let ch = m.attr(A_CHANNEL).map(|v| u16::from_be_bytes([v[0], v[1]]));
                            let peer = m.addr(A_PEER);
                            let mut g = allocs.lock().unwrap();
                            match (g.get_mut(&from), ch, peer) {
                                (Some(a), Some(ch), Some(peer))
                                    if (0x4000..0x5000).contains(&ch) =>
                                {
                                    a.perms.insert(peer.ip());
                                    a.channels.insert(ch, peer);
                                    reply(SUCCESS, Vec::new(), true)
                                }
                                _ => reply(FAILURE, err(400), true),
                            }
                        }
                        _ => reply(FAILURE, err(400), true),
                    };
                    let _ = sock.send_to(&out, from).await;
                }
            });
            MiniTurn {
                addr,
                allocations,
                relayed,
                _stop: stop,
            }
        }

        pub fn source(&self) -> Source {
            Source::Fixed {
                server: self.addr.to_string(),
                user: MINI_USER.into(),
                pass: MINI_PASS.into(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::essai::{MiniTurn, MINI_USER};
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        s.split_whitespace()
            .map(|b| u8::from_str_radix(b, 16).unwrap())
            .collect()
    }

    #[test]
    fn message_types_mix_method_and_class() {
        assert_eq!(kind(0x001, REQUEST), 0x0001);
        assert_eq!(kind(ALLOCATE, SUCCESS), 0x0103);
        assert_eq!(kind(ALLOCATE, FAILURE), 0x0113);
        assert_eq!(kind(DATA, INDICATION), 0x0017);
        assert_eq!(kind(CHANNEL_BIND, REQUEST), 0x0009);
        for m in [1u16, 3, 4, 6, 7, 8, 9, 0x0FFF, 0x0ABC] {
            for c in [REQUEST, INDICATION, SUCCESS, FAILURE] {
                assert_eq!(parts(kind(m, c)), (m, c));
            }
        }
    }

    /// Les exemples de la RFC 5769 : intégrité à court et à long terme, adresse XOR.
    #[test]
    fn the_rfc_5769_samples_check_out() {
        let req = hex(
            "00 01 00 58 21 12 a4 42 b7 e7 a7 01 bc 34 d6 86 fa 87 df ae 80 22 00 10 53 54 55 \
             4e 20 74 65 73 74 20 63 6c 69 65 6e 74 00 24 00 04 6e 00 01 ff 80 29 00 08 93 2f \
             f9 b1 51 26 3b 36 00 06 00 09 65 76 74 6a 3a 68 36 76 59 20 20 20 00 08 00 14 9a \
             ea a7 0c bf d8 cb 56 78 1e f2 b5 b2 d3 f2 49 c1 b5 71 a2 80 28 00 04 e5 7a 3b cf",
        );
        let m = parse(&req).unwrap();
        assert_eq!((m.method, m.class), (1, REQUEST));
        assert_eq!(m.attr(A_USERNAME), Some(&b"evtj:h6vY"[..]));
        assert!(m.integrity_ok(b"VOkJxbRl1RmTxUk/WvJxBt"));
        assert!(!m.integrity_ok(b"VOkJxbRl1RmTxUk/WvJxBu"));

        let resp = hex(
            "01 01 00 3c 21 12 a4 42 b7 e7 a7 01 bc 34 d6 86 fa 87 df ae 80 22 00 0b 74 65 73 \
             74 20 76 65 63 74 6f 72 20 00 20 00 08 00 01 a1 47 e1 12 a6 43 00 08 00 14 2b 91 \
             f5 99 fd 9e 90 c3 8c 74 89 f9 2a f9 ba 53 f0 6b e7 d7 80 28 00 04 c0 7d 4c 96",
        );
        let m = parse(&resp).unwrap();
        assert_eq!((m.method, m.class), (1, SUCCESS));
        assert_eq!(m.addr(A_MAPPED), Some("192.0.2.1:32853".parse().unwrap()));
        assert!(m.integrity_ok(b"VOkJxbRl1RmTxUk/WvJxBt"));
        // Et dans l'autre sens : la même adresse donne les mêmes octets.
        assert_eq!(
            xor_encode("192.0.2.1:32853".parse().unwrap(), &m.tid),
            hex("00 01 a1 47 e1 12 a6 43")
        );

        let long = hex(
            "00 01 00 60 21 12 a4 42 78 ad 34 33 c6 ad 72 c0 29 da 41 2e 00 06 00 12 e3 83 9e \
             e3 83 88 e3 83 aa e3 83 83 e3 82 af e3 82 b9 00 00 00 15 00 1c 66 2f 2f 34 39 39 \
             6b 39 35 34 64 36 4f 4c 33 34 6f 4c 39 46 53 54 76 79 36 34 73 41 00 14 00 0b 65 \
             78 61 6d 70 6c 65 2e 6f 72 67 00 00 08 00 14 f6 70 24 65 6d d6 4a 3e 02 b8 e0 71 \
             2e 85 c9 a2 8c a8 96 66",
        );
        let m = parse(&long).unwrap();
        let user = String::from_utf8(m.attr(A_USERNAME).unwrap().to_vec()).unwrap();
        assert_eq!(user, "\u{30DE}\u{30C8}\u{30EA}\u{30C3}\u{30AF}\u{30B9}");
        let key = long_term_key(&user, "example.org", "TheMatrIX");
        assert!(m.integrity_ok(&key));
        // Un message fait ici se vérifie de même.
        let out = Out::new(ALLOCATE, REQUEST, &[7; 12])
            .attr(A_USERNAME, user.as_bytes())
            .attr(A_REALM, b"example.org")
            .integrity(&key)
            .finish();
        assert!(parse(&out).unwrap().integrity_ok(&key));
    }

    #[test]
    fn credentials_come_from_webrtc_ice_servers_or_fixed_lines() {
        // Réponse de Cloudflare (relayée par le Worker, avec sa durée).
        let cf = r#"{"iceServers":[{"urls":["stun:stun.cloudflare.com:3478"]},
            {"urls":["turn:turn.cloudflare.com:3478?transport=udp",
                     "turn:turn.cloudflare.com:3478?transport=tcp",
                     "turns:turn.cloudflare.com:5349?transport=tcp",
                     "turn:turn.cloudflare.com:53?transport=udp"],
             "username":"u1","credential":"c1"}],"ttl":43200}"#;
        let c = parse_ice(cf).unwrap();
        assert_eq!(
            c.servers,
            ["turn.cloudflare.com:3478", "turn.cloudflare.com:53"]
        );
        assert_eq!((c.user.as_str(), c.pass.as_str()), ("u1", "c1"));
        assert_eq!(c.ttl, Duration::from_secs(43200));
        // Une liste nue, `urls` seule, sans port : 3478, et une heure par défaut.
        let c =
            parse_ice(r#"[{"urls":"turn:t.example","username":"a","credential":"b"}]"#).unwrap();
        assert_eq!(c.servers, ["t.example:3478"]);
        assert_eq!(c.ttl, Duration::from_secs(3600));
        assert!(parse_ice(r#"{"iceServers":[{"urls":"stun:x"}]}"#).is_none());
        assert!(parse_ice("pas du json").is_none());
        let s = parse_sources(
            "# commentaire\n\nhttps://turn.exemple.workers.dev\n\
             turn:10.0.0.1:3478?transport=udp moi secret\nturn:incomplet\n",
        );
        assert_eq!(
            s,
            [
                Source::Service("https://turn.exemple.workers.dev".into()),
                Source::Fixed {
                    server: "10.0.0.1:3478".into(),
                    user: "moi".into(),
                    pass: "secret".into()
                }
            ]
        );
    }

    fn wait_for(what: &str, f: impl Fn() -> bool) {
        let t = Instant::now();
        while !f() {
            assert!(t.elapsed() < Duration::from_secs(15), "{what} : trop long");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Deux clients obtiennent chacun une adresse relayée (en s'identifiant), relient un
    /// canal vers celle de l'autre, et s'échangent des paquets dans les deux sens par le
    /// serveur, sans jamais voir l'adresse de l'autre ; les canaux sont tenus en vie.
    #[test]
    fn two_clients_talk_through_a_turn_server_and_keep_their_channels() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let server = MiniTurn::start(rt.handle());
        type Got = Arc<Mutex<Vec<(SocketAddr, Vec<u8>)>>>;
        let mk = |got: Got, told: Arc<Mutex<Vec<Option<SocketAddrV4>>>>| {
            TurnClient::start(
                rt.handle(),
                vec![server.source()],
                Box::new(move |from, p| got.lock().unwrap().push((from, p.to_vec()))),
                Box::new(move |a| told.lock().unwrap().push(a)),
            )
            .unwrap()
        };
        let (got_a, got_b): (Got, Got) = Default::default();
        let told_a: Arc<Mutex<Vec<Option<SocketAddrV4>>>> = Default::default();
        let a = mk(got_a.clone(), told_a.clone());
        let b = mk(got_b.clone(), Default::default());
        wait_for("adresses relayées", || {
            a.relayed().is_some() && b.relayed().is_some()
        });
        assert_eq!(told_a.lock().unwrap().as_slice(), &[a.relayed()]);
        let (ra, rb) = (
            SocketAddr::V4(a.relayed().unwrap()),
            SocketAddr::V4(b.relayed().unwrap()),
        );
        assert_ne!(ra, rb);
        // Les premiers paquets demandent les canaux ; ensuite ils passent.
        wait_for("de A à B", || {
            a.send(rb, b"bonjour");
            b.send(ra, b"salut");
            std::thread::sleep(Duration::from_millis(20));
            got_b
                .lock()
                .unwrap()
                .iter()
                .any(|(f, p)| *f == ra && p == b"bonjour")
                && got_a
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|(f, p)| *f == rb && p == b"salut")
        });
        // Un gros échange.
        got_b.lock().unwrap().clear();
        // (Par paquets de 20 : en rafale, la mémoire des sockets de la machine déborde.)
        for i in 0..300u32 {
            a.send(rb, &i.to_le_bytes());
            if i % 20 == 19 {
                std::thread::sleep(Duration::from_millis(2));
            }
        }
        wait_for("300 paquets", || got_b.lock().unwrap().len() >= 290);
        // Les canaux sont reliés de nouveau avant la fin de leur durée (essais : 2 s).
        let bound_before = a.st.lock().unwrap().peers[&rb].bound.unwrap();
        wait_for("liaison renouvelée", || {
            a.send(rb, b"encore");
            std::thread::sleep(Duration::from_millis(50));
            a.st.lock().unwrap().peers[&rb]
                .bound
                .is_some_and(|t| t > bound_before)
        });
        assert!(server.relayed.load(std::sync::atomic::Ordering::Relaxed) >= 300);
        assert_eq!(
            server
                .allocations
                .load(std::sync::atomic::Ordering::Relaxed),
            2
        );
        let (sent, received) = a.counts();
        assert!(sent >= 300 && received >= 1, "{sent} {received}");
    }

    /// Avec un vrai serveur TURN (coturn sur cette machine, par exemple) : la variable
    /// `MINELIB_TURN_ESSAI` donne la source (`turn:127.0.0.1:3478 utilisateur motdepasse`).
    /// Deux clients s'y allouent une adresse et s'échangent des paquets.
    #[test]
    #[ignore = "serveur TURN réel (MINELIB_TURN_ESSAI)"]
    fn two_clients_talk_through_a_real_turn_server() {
        let src = std::env::var("MINELIB_TURN_ESSAI").expect("MINELIB_TURN_ESSAI");
        let source = parse_sources(&src).pop().expect("source");
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        type Got = Arc<Mutex<Vec<(SocketAddr, Vec<u8>)>>>;
        let (got_a, got_b): (Got, Got) = Default::default();
        let mk = |got: Got| {
            TurnClient::start(
                rt.handle(),
                vec![source.clone()],
                Box::new(move |from, p| got.lock().unwrap().push((from, p.to_vec()))),
                Box::new(|_| {}),
            )
            .unwrap()
        };
        let (a, b) = (mk(got_a.clone()), mk(got_b.clone()));
        wait_for("adresses relayées", || {
            a.relayed().is_some() && b.relayed().is_some()
        });
        let (ra, rb) = (
            SocketAddr::V4(a.relayed().unwrap()),
            SocketAddr::V4(b.relayed().unwrap()),
        );
        eprintln!("TURN : adresses relayées {ra} et {rb}");
        wait_for("échange", || {
            a.send(rb, b"bonjour");
            b.send(ra, b"salut");
            std::thread::sleep(Duration::from_millis(20));
            got_b
                .lock()
                .unwrap()
                .iter()
                .any(|(f, p)| *f == ra && p == b"bonjour")
                && got_a
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|(f, p)| *f == rb && p == b"salut")
        });
        got_b.lock().unwrap().clear();
        for i in 0..300u32 {
            a.send(rb, &i.to_le_bytes());
            if i % 20 == 19 {
                std::thread::sleep(Duration::from_millis(2));
            }
        }
        wait_for("300 paquets", || got_b.lock().unwrap().len() >= 290);
        let bound_before = a.st.lock().unwrap().peers[&rb].bound.unwrap();
        wait_for("liaison renouvelée", || {
            a.send(rb, b"encore");
            std::thread::sleep(Duration::from_millis(50));
            a.st.lock().unwrap().peers[&rb]
                .bound
                .is_some_and(|t| t > bound_before)
        });
        // Un rafraîchissement de l'allocation, tout de suite.
        a.st.lock().unwrap().refresh_at = Some(Instant::now());
        let before = a.st.lock().unwrap().refresh_at;
        wait_for("rafraîchie", || a.st.lock().unwrap().refresh_at > before);
        assert!(a.relayed().is_some());
        eprintln!("TURN : {:?} {:?}", a.counts(), b.counts());
    }

    /// Sans serveur qui réponde, ou avec de faux identifiants : pas d'adresse relayée, et
    /// rien ne part.
    #[test]
    fn without_a_working_server_nothing_is_relayed() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let server = MiniTurn::start(rt.handle());
        let wrong = Source::Fixed {
            server: server.addr.to_string(),
            user: MINI_USER.into(),
            pass: "faux".into(),
        };
        let c = TurnClient::start(
            rt.handle(),
            vec![wrong],
            Box::new(|_, _| {}),
            Box::new(|_| {}),
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(1500));
        assert!(c.relayed().is_none());
        c.send("203.0.113.9:4000".parse().unwrap(), b"x");
        assert_eq!(c.counts(), (0, 0));
        assert_eq!(
            server
                .allocations
                .load(std::sync::atomic::Ordering::Relaxed),
            0
        );
    }
}
