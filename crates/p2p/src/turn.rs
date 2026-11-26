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
/// (v333) Sur TCP, chaque message tient sur un multiple de 4 octets (RFC 8656, 12.5).
fn pad4(p: &[u8]) -> Vec<u8> {
    let mut v = p.to_vec();
    while v.len() & 3 != 0 {
        v.push(0);
    }
    v
}

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
    /// `hôte:port`, et (v334) `true` pour un serveur qui ne se joint que par TCP
    /// (`?transport=tcp`) ; les autres sont essayés en UDP, puis par TCP.
    servers: Vec<(String, bool)>,
    user: String,
    pass: String,
    ttl: Duration,
}

/// Réponse d'un service d'identifiants : `{"iceServers": [...]}`, une liste, ou un seul
/// serveur ; `urls` seule ou en liste. Seuls les serveurs `turn:` en UDP sont gardés.
fn parse_ice(text: &str) -> Vec<Creds> {
    parse_ice_groups(text).unwrap_or_default()
}

/// (v334) Tous les groupes de la réponse (un par serveur ICE avec ses identifiants), dans
/// l'ordre : le premier qui donne une allocation sert, les suivants sont en secours (un
/// service épuisé ou en panne, un autre prend la suite).
fn parse_ice_groups(text: &str) -> Option<Vec<Creds>> {
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
    let mut groups = Vec::new();
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
        // (v334) `?transport=tcp` : par TCP seulement ; sans précision ou en UDP : UDP, puis
        // TCP au même port. Un même `hôte:port` n'est gardé qu'une fois. (`turns:`, TLS,
        // n'est pas pris.)
        let mut servers: Vec<(String, bool)> = Vec::new();
        for u in &urls {
            let Some(rest) = u.strip_prefix("turn:") else {
                continue;
            };
            let (hostport, query) = rest.split_once('?').unwrap_or((rest, ""));
            let tcp = query.contains("transport=tcp");
            if !query.is_empty() && !tcp && !query.contains("transport=udp") {
                continue;
            }
            let hp = if hostport.contains(':') {
                hostport.to_string()
            } else {
                format!("{hostport}:3478")
            };
            match servers.iter_mut().find(|(h, _)| *h == hp) {
                Some(e) => e.1 &= tcp,
                None => servers.push((hp, tcp)),
            }
        }
        if !servers.is_empty() {
            groups.push(Creds {
                servers,
                user: user.to_string(),
                pass: pass.to_string(),
                ttl,
            });
        }
    }
    (!groups.is_empty()).then_some(groups)
}

fn fetch(source: &Source) -> Vec<Creds> {
    match source {
        Source::Fixed { server, user, pass } => vec![Creds {
            servers: vec![(server.clone(), false)],
            user: user.clone(),
            pass: pass.clone(),
            ttl: Duration::from_secs(365 * 86400),
        }],
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
                .ok()
                .and_then(|mut r| r.body_mut().read_to_string().ok());
            let Some(body) = body else {
                log::info!("TURN : service d'identifiants injoignable");
                return Vec::new();
            };
            let c = parse_ice(&body);
            if c.is_empty() {
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
/// (v334) Un serveur qui a échoué passe après les autres pendant ce temps.
const BAD_FOR: Duration = Duration::from_secs(600);
/// (v338) Sondes des adresses d'un même nom : combien par adresse, à quel intervalle, et
/// combien de temps attendre les réponses.
const PROBES: usize = 3;
const PROBE_GAP: Duration = Duration::from_millis(15);
const PROBE_WAIT: Duration = Duration::from_millis(400);
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

/// (v338) Les adresses d'un même nom de serveur, rangées par temps de réponse : la plus
/// rapide d'abord. `free.expressturn.com` en a deux, et le DNS rend l'une ou l'autre en
/// premier : mesuré depuis un PC en France, 11 à 13 ms d'aller-retour de bout en bout par
/// l'une, 34 à 38 ms par l'autre. Chaque adresse reçoit `PROBES` demandes d'allocation sans
/// identifiants, à laquelle tout serveur TURN répond tout de suite (401, sans rien créer) ;
/// la meilleure des réponses compte. Celles qui ne répondent pas dans `PROBE_WAIT` (UDP
/// bloqué, serveur éteint) restent dans l'ordre du DNS, après les autres.
async fn by_speed(addrs: Vec<SocketAddr>) -> Vec<SocketAddr> {
    if addrs.len() < 2 {
        return addrs;
    }
    let Ok(sock) = tokio::net::UdpSocket::bind("0.0.0.0:0").await else {
        return addrs;
    };
    let start = Instant::now();
    // (adresse, transaction, envoyée à)
    let mut asked: Vec<(usize, [u8; 12], Instant)> = Vec::new();
    let mut best: Vec<Option<Duration>> = vec![None; addrs.len()];
    let mut buf = [0u8; 1500];
    let mut next = start;
    let mut round = 0;
    loop {
        if round < PROBES && Instant::now() >= next {
            for (i, a) in addrs.iter().enumerate() {
                let tid: [u8; 12] = rand::random();
                let p = Out::new(ALLOCATE, REQUEST, &tid)
                    .attr(A_TRANSPORT, &[17, 0, 0, 0])
                    .finish();
                if sock.send_to(&p, a).await.is_ok() {
                    asked.push((i, tid, Instant::now()));
                }
            }
            round += 1;
            next += PROBE_GAP;
        }
        let all = round >= PROBES && best.iter().all(Option::is_some);
        let left = (start + PROBE_WAIT).saturating_duration_since(Instant::now());
        if all || left.is_zero() {
            break;
        }
        let wait = if round < PROBES {
            left.min(next.saturating_duration_since(Instant::now()))
        } else {
            left
        };
        let Ok(Ok((n, from))) = tokio::time::timeout(wait, sock.recv_from(&mut buf)).await else {
            continue;
        };
        let Some(m) = parse(&buf[..n]) else { continue };
        if let Some(&(i, _, at)) = asked
            .iter()
            .find(|(i, t, _)| *t == m.tid && addrs[*i] == from)
        {
            let rtt = at.elapsed();
            best[i] = Some(best[i].map_or(rtt, |b| b.min(rtt)));
        }
    }
    let mut order: Vec<usize> = (0..addrs.len()).collect();
    order.sort_by_key(|&i| best[i].unwrap_or(Duration::MAX));
    log::info!(
        "TURN : {} adresses pour un même serveur, rangées par temps de réponse : {}",
        addrs.len(),
        order
            .iter()
            .map(|&i| match best[i] {
                Some(t) => format!("{} ({} ms)", addrs[i], t.as_millis()),
                None => format!("{} (pas de réponse)", addrs[i]),
            })
            .collect::<Vec<_>>()
            .join(", ")
    );
    order.into_iter().map(|i| addrs[i]).collect()
}

/// Client TURN d'un transport : une allocation, chez le premier serveur qui en donne une.
/// (v333) Par UDP, ou par TCP quand le serveur ne répond pas en UDP (réseau d'entreprise,
/// d'école, qui ne laisse sortir que TCP) : l'adresse relayée reste en UDP, les autres
/// bouts n'y voient aucune différence.
pub struct TurnClient {
    sock: Arc<tokio::net::UdpSocket>,
    /// (v333) Allocation par TCP : ce qui part au serveur passe par cette file (le fil
    /// d'écriture de la connexion, avec son numéro), rien sinon. `via_tcp` : la dernière
    /// allocation s'est faite par TCP, la suivante y va d'abord (sans attendre UDP).
    tcp: Mutex<Option<(u64, tokio::sync::mpsc::UnboundedSender<Vec<u8>>)>>,
    tcp_n: std::sync::atomic::AtomicU64,
    via_tcp: std::sync::atomic::AtomicBool,
    /// (v334) Serveurs qui ont échoué récemment (passés après les autres).
    bad: Mutex<HashMap<SocketAddr, Instant>>,
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
            tcp: Mutex::new(None),
            tcp_n: Default::default(),
            bad: Default::default(),
            // (Réglage d'essai, ou de secours pour un réseau où UDP passe mal : TCP d'abord.)
            via_tcp: std::sync::atomic::AtomicBool::new(
                std::env::var_os("MINELIB_TURN_TCP").is_some_and(|v| !v.is_empty()),
            ),
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
                self.to_server_now(&channel_data(ch, data), server);
            }
            None => {
                drop(st);
                self.wake.notify_one();
            }
        }
    }

    /// (v333) Un message au serveur, tout de suite : par la connexion TCP s'il y en a une
    /// (complété à un multiple de 4 octets, comme le veut TURN sur TCP), sinon en UDP.
    fn to_server_now(&self, p: &[u8], server: SocketAddr) {
        if let Some((_, tx)) = self.tcp.lock().unwrap().as_ref() {
            let _ = tx.send(pad4(p));
            return;
        }
        let _ = self.sock.try_send_to(p, server);
    }

    async fn to_server(&self, p: &[u8], server: SocketAddr) {
        let tx = self.tcp.lock().unwrap().as_ref().map(|(_, tx)| tx.clone());
        match tx {
            Some(tx) => {
                let _ = tx.send(pad4(p));
            }
            None => {
                let _ = self.sock.send_to(p, server).await;
            }
        }
    }

    /// (v333) Connexion TCP au serveur `server` : un fil écrit ce qui part (il s'arrête,
    /// et ferme, quand la file est lâchée), un autre lit les messages (STUN, ou données
    /// d'un canal complétées à 4 octets) et les traite comme ceux venus en UDP. Fermée par
    /// le serveur : l'allocation est perdue, une autre est demandée.
    async fn connect_tcp(self: &Arc<Self>, server: SocketAddr) -> bool {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let Ok(Ok(stream)) = tokio::time::timeout(
            Duration::from_secs(4),
            tokio::net::TcpStream::connect(server),
        )
        .await
        else {
            return false;
        };
        let _ = stream.set_nodelay(true);
        let (mut rd, mut wr) = stream.into_split();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
        tokio::spawn(async move {
            while let Some(b) = rx.recv().await {
                if wr.write_all(&b).await.is_err() {
                    break;
                }
            }
        });
        let n = self
            .tcp_n
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        *self.tcp.lock().unwrap() = Some((n, tx));
        let weak = Arc::downgrade(self);
        let mut stopped = self.stop.subscribe();
        tokio::spawn(async move {
            loop {
                let mut head = [0u8; 4];
                tokio::select! {
                    r = rd.read_exact(&mut head) => if r.is_err() { break },
                    _ = stopped.changed() => return,
                }
                let len = u16::from_be_bytes([head[2], head[3]]) as usize;
                let total = if (0x40..0x80).contains(&head[0]) {
                    4 + ((len + 3) & !3)
                } else {
                    HEADER + len
                };
                let mut frame = vec![0u8; total];
                frame[..4].copy_from_slice(&head);
                tokio::select! {
                    r = rd.read_exact(&mut frame[4..]) => if r.is_err() { break },
                    _ = stopped.changed() => return,
                }
                match weak.upgrade() {
                    Some(me) => me.on_packet(server, &frame),
                    None => return,
                }
            }
            // La connexion est tombée : si c'est toujours la nôtre, l'allocation est perdue.
            let Some(me) = weak.upgrade() else { return };
            let ours = me.tcp.lock().unwrap().as_ref().is_some_and(|t| t.0 == n);
            if ours {
                *me.tcp.lock().unwrap() = None;
                let had = {
                    let mut st = me.st.lock().unwrap();
                    let had = st.relayed.is_some();
                    st.lost();
                    st.server = None;
                    had
                };
                log::info!("TURN : connexion TCP fermée, nouvelle allocation");
                if had {
                    (me.told)(None);
                }
                me.wake.notify_one();
            }
        });
        true
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
        let mut last = 0u16;
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
            // (v333) Par TCP, rien ne se perd : envoyée une fois, la réponse est attendue
            // aussi longtemps qu'en UDP avec ses réémissions.
            let tcp = self.tcp.lock().unwrap().is_some();
            let waits: &[u64] = if tcp { &[7500] } else { &RTO };
            for &wait in waits {
                self.to_server(&pkt, server).await;
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
            last = code;
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
        // (v334) Toujours refusé après un nouveau nonce : le dernier refus (identifiants
        // faux, service fermé), pas « pas de réponse ».
        Err(last)
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
        // (v334) Tous les groupes d'identifiants de toutes les sources, dans l'ordre : le
        // premier serveur qui donne une allocation sert. Un service épuisé (quota du mois)
        // ou en panne laisse la place au suivant.
        let mut groups = Vec::new();
        for s in self.sources.clone() {
            let got = tokio::task::spawn_blocking(move || fetch(&s))
                .await
                .unwrap_or_default();
            groups.extend(got);
        }
        let mut tries: Vec<(SocketAddr, bool, usize)> = Vec::new();
        // (v338) Toutes les adresses d'un nom (une seconde en secours de la première), la
        // plus rapide d'abord (`by_speed`) ; sans sonde quand l'UDP ne passe pas.
        let udp = !self.via_tcp.load(std::sync::atomic::Ordering::Relaxed);
        for (g, c) in groups.iter().enumerate() {
            for (s, tcp_only) in &c.servers {
                if let Ok(addrs) = tokio::net::lookup_host(s.as_str()).await {
                    let mut all: Vec<SocketAddr> = Vec::new();
                    for a in addrs.filter(|a| a.is_ipv4()) {
                        if !all.contains(&a) {
                            all.push(a);
                        }
                    }
                    if udp && !*tcp_only {
                        all = by_speed(all).await;
                    }
                    for a in all {
                        if !tries.iter().any(|t| t.0 == a && t.2 == g) {
                            tries.push((a, *tcp_only, g));
                        }
                    }
                }
            }
        }
        // (v334) Un serveur qui a échoué depuis moins de `BAD_FOR` passe après les autres :
        // pas d'attente de réponse à chaque nouvel essai tant que d'autres répondent.
        {
            let now = Instant::now();
            let mut bad = self.bad.lock().unwrap();
            bad.retain(|_, t| now.duration_since(*t) < BAD_FOR);
            tries.sort_by_key(|t| bad.contains_key(&t.0));
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
        // (v333) L'ancienne connexion TCP, s'il y en avait une, se ferme.
        *self.tcp.lock().unwrap() = None;
        if tries.is_empty() {
            self.failed(had, "pas d'identifiants");
            return;
        }
        for (server, tcp_only, g) in tries {
            let c = &groups[g];
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
            // (v333) UDP d'abord, puis TCP s'il ne répond pas (ou TCP d'abord, si c'est par
            // là que la dernière allocation s'est faite ; (v334) TCP seul pour un serveur
            // donné ainsi). Un refus net (identifiants, quota) vaut pour les deux.
            let tcp_first = tcp_only || self.via_tcp.load(std::sync::atomic::Ordering::Relaxed);
            let modes: &[bool] = if tcp_only {
                &[true]
            } else if tcp_first {
                &[true, false]
            } else {
                &[false, true]
            };
            let mut got = Err(0);
            for &tcp in modes {
                *self.tcp.lock().unwrap() = None;
                if tcp && !self.connect_tcp(server).await {
                    continue;
                }
                got = self.transact(ALLOCATE, &attrs).await;
                if !matches!(got, Err(0)) {
                    break;
                }
                if !tcp {
                    log::info!("TURN : pas de réponse en UDP, essai par TCP");
                }
            }
            let tcp = self.tcp.lock().unwrap().is_some();
            if got.is_err() {
                *self.tcp.lock().unwrap() = None;
                self.bad.lock().unwrap().insert(server, Instant::now());
            } else {
                self.via_tcp
                    .store(tcp, std::sync::atomic::Ordering::Relaxed);
            }
            match got {
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
                    self.bad.lock().unwrap().remove(&server);
                    log::info!(
                        "TURN : adresse relayée obtenue{}{}",
                        if g > 0 { " (service de secours)" } else { "" },
                        if tcp { " par TCP" } else { "" }
                    );
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
        *self.tcp.lock().unwrap() = None;
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
                log::info!("TURN : canal refusé");
                if st.peers.get(&peer).is_some_and(|p| p.channel == ch) {
                    st.peers.remove(&peer);
                    st.channels.remove(&ch);
                    st.freed.push((ch, Instant::now()));
                }
            }
            Err(code) => log::info!("TURN : canal sans réponse ({code}), nouvel essai"),
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
        /// (v333) Octets relayés (essais : ce qu'un serveur TURN compterait), et UDP coupé
        /// (un réseau qui ne laisse sortir que TCP : le serveur ne répond plus en UDP).
        pub bytes: Arc<std::sync::atomic::AtomicU64>,
        pub udp_off: Arc<std::sync::atomic::AtomicBool>,
        _stop: watch::Sender<bool>,
    }

    /// (v333) Par où répondre à un client : UDP, ou sa connexion TCP.
    type Backs = Arc<Mutex<HashMap<SocketAddr, tokio::sync::mpsc::UnboundedSender<Vec<u8>>>>>;

    async fn back(backs: &Backs, sock: &tokio::net::UdpSocket, to: SocketAddr, p: &[u8]) {
        let tcp = backs.lock().unwrap().get(&to).cloned();
        match tcp {
            Some(tx) => {
                let _ = tx.send(super::pad4(p));
            }
            None => {
                let _ = sock.send_to(p, to).await;
            }
        }
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
            // (v333) UDP et TCP sur le même numéro de port, comme un vrai serveur.
            let (std_sock, tcp) = loop {
                let u = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
                let port = u.local_addr().unwrap().port();
                if let Ok(t) = std::net::TcpListener::bind(("127.0.0.1", port)) {
                    break (u, t);
                }
            };
            std_sock.set_nonblocking(true).unwrap();
            tcp.set_nonblocking(true).unwrap();
            let addr = std_sock.local_addr().unwrap();
            let sock = Arc::new(tokio::net::UdpSocket::from_std(std_sock).unwrap());
            let tcp = tokio::net::TcpListener::from_std(tcp).unwrap();
            let (stop, mut stopped) = watch::channel(false);
            let allocations = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let relayed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let bytes = Arc::new(std::sync::atomic::AtomicU64::new(0));
            let udp_off = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let (allocs2, relayed2) = (allocations.clone(), relayed.clone());
            let (bytes2, udp_off2) = (bytes.clone(), udp_off.clone());
            let backs: Backs = Default::default();
            // Connexions TCP : chacune lue dans son fil, ses messages traités comme ceux
            // venus en UDP (par la même boucle).
            let (frames_tx, mut frames) =
                tokio::sync::mpsc::unbounded_channel::<(Vec<u8>, SocketAddr)>();
            {
                let backs = backs.clone();
                let mut stopped = stopped.clone();
                rt.spawn(async move {
                    loop {
                        let (stream, from) = tokio::select! {
                            r = tcp.accept() => match r { Ok(x) => x, Err(_) => continue },
                            _ = stopped.changed() => return,
                        };
                        use tokio::io::{AsyncReadExt, AsyncWriteExt};
                        let (mut rd, mut wr) = stream.into_split();
                        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
                        backs.lock().unwrap().insert(from, tx);
                        tokio::spawn(async move {
                            while let Some(b) = rx.recv().await {
                                if wr.write_all(&b).await.is_err() {
                                    break;
                                }
                            }
                        });
                        let frames_tx = frames_tx.clone();
                        tokio::spawn(async move {
                            loop {
                                let mut head = [0u8; 4];
                                if rd.read_exact(&mut head).await.is_err() {
                                    return;
                                }
                                let len = u16::from_be_bytes([head[2], head[3]]) as usize;
                                let total = if (0x40..0x80).contains(&head[0]) {
                                    4 + ((len + 3) & !3)
                                } else {
                                    HEADER + len
                                };
                                let mut f = vec![0u8; total];
                                f[..4].copy_from_slice(&head);
                                if rd.read_exact(&mut f[4..]).await.is_err() {
                                    return;
                                }
                                if frames_tx.send((f, from)).is_err() {
                                    return;
                                }
                            }
                        });
                    }
                });
            }
            let backs2 = backs.clone();
            rt.spawn(async move {
                let backs = backs2;
                let allocs: Arc<Mutex<HashMap<SocketAddr, Alloc>>> = Default::default();
                let key = long_term_key(MINI_USER, MINI_REALM, MINI_PASS);
                let mut buf = vec![0u8; 2048];
                loop {
                    let (p, from) = tokio::select! {
                        r = sock.recv_from(&mut buf) => match r {
                            Ok((n, from)) => {
                                if udp_off2.load(std::sync::atomic::Ordering::Relaxed) {
                                    continue;
                                }
                                (buf[..n].to_vec(), from)
                            }
                            Err(_) => continue,
                        },
                        f = frames.recv() => match f { Some(x) => x, None => continue },
                        _ = stopped.changed() => return,
                    };
                    if p.len() >= 4 && (0x40..0x80).contains(&p[0]) {
                        let ch = u16::from_be_bytes([p[0], p[1]]);
                        let len = u16::from_be_bytes([p[2], p[3]]) as usize;
                        let g = allocs.lock().unwrap();
                        if let (Some(a), Some(data)) = (g.get(&from), p.get(4..4 + len)) {
                            if let Some(peer) = a.channels.get(&ch) {
                                let _ = a.relay.try_send_to(data, *peer);
                                relayed2.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                bytes2.fetch_add(
                                    data.len() as u64,
                                    std::sync::atomic::Ordering::Relaxed,
                                );
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
                        back(&backs, &sock, from, &reply(FAILURE, a, false)).await;
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
                                let (backs, bytes2) = (backs.clone(), bytes2.clone());
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
                                        bytes2.fetch_add(
                                            n as u64,
                                            std::sync::atomic::Ordering::Relaxed,
                                        );
                                        back(&backs, &sock, client, &out).await;
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
                    back(&backs, &sock, from, &out).await;
                }
            });
            MiniTurn {
                addr,
                allocations,
                relayed,
                bytes,
                udp_off,
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

        /// (v331) Le même, dans son propre moteur (pour les paquets qui n'ont pas tokio) :
        /// il vit tant qu'on garde ce qui est rendu.
        pub fn start_alone() -> (MiniTurn, impl Drop) {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .enable_all()
                .build()
                .expect("moteur du serveur TURN d'essai");
            let server = MiniTurn::start(rt.handle());
            (server, rt)
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
        let all = parse_ice(cf);
        assert_eq!(all.len(), 1);
        let c = &all[0];
        let hp = |h: &str, tcp: bool| (h.to_string(), tcp);
        // (v334) Le même port en UDP et en TCP : gardé une fois, UDP puis TCP.
        assert_eq!(
            c.servers,
            [
                hp("turn.cloudflare.com:3478", false),
                hp("turn.cloudflare.com:53", false)
            ]
        );
        assert_eq!((c.user.as_str(), c.pass.as_str()), ("u1", "c1"));
        assert_eq!(c.ttl, Duration::from_secs(43200));
        // Une liste nue, `urls` seule, sans port : 3478, et une heure par défaut.
        let c = &parse_ice(r#"[{"urls":"turn:t.example","username":"a","credential":"b"}]"#)[0];
        assert_eq!(c.servers, [hp("t.example:3478", false)]);
        assert_eq!(c.ttl, Duration::from_secs(3600));
        assert!(parse_ice(r#"{"iceServers":[{"urls":"stun:x"}]}"#).is_empty());
        assert!(parse_ice("pas du json").is_empty());
        // (v334) Plusieurs services, chacun ses identifiants, dans l'ordre ; un serveur
        // donné en TCP seulement l'est.
        let two = parse_ice(
            r#"{"iceServers":[{"urls":"turn:a.example:3478","username":"u1","credential":"p1"},
                {"urls":["turn:b.example:443?transport=tcp"],"username":"u2","credential":"p2"}],
               "ttl":600}"#,
        );
        assert_eq!(two.len(), 2);
        assert_eq!(two[0].servers, [hp("a.example:3478", false)]);
        assert_eq!(two[1].servers, [hp("b.example:443", true)]);
        assert_eq!((two[1].user.as_str(), two[1].pass.as_str()), ("u2", "p2"));
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

    /// (v338) Plusieurs adresses pour un même serveur : la plus rapide à répondre passe
    /// en premier, une adresse muette après celles qui répondent, et sans réponse du tout,
    /// l'ordre reste celui du DNS.
    #[test]
    fn the_quickest_address_of_a_turn_server_comes_first() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        // Un serveur qui répond 401 à tout, après `ms` millisecondes.
        let answering = |ms: u64| {
            let _g = rt.enter();
            let std_sock = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
            std_sock.set_nonblocking(true).unwrap();
            let addr = std_sock.local_addr().unwrap();
            let sock = Arc::new(tokio::net::UdpSocket::from_std(std_sock).unwrap());
            rt.spawn(async move {
                let mut buf = [0u8; 1500];
                while let Ok((n, from)) = sock.recv_from(&mut buf).await {
                    let Some(m) = parse(&buf[..n]) else { continue };
                    let sock = sock.clone();
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_millis(ms)).await;
                        let p = Out::new(m.method, FAILURE, &m.tid)
                            .attr(A_ERROR, &[0, 0, 4, 1])
                            .finish();
                        let _ = sock.send_to(&p, from).await;
                    });
                }
            });
            addr
        };
        let silent = || {
            let s = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
            let a = s.local_addr().unwrap();
            (s, a)
        };
        let (slow, quick) = (answering(60), answering(0));
        let ((_s1, mute1), (_s2, mute2)) = (silent(), silent());
        let t = Instant::now();
        assert_eq!(rt.block_on(by_speed(vec![slow, quick])), vec![quick, slow]);
        assert!(
            t.elapsed() < PROBE_WAIT,
            "pas d'attente quand tout a répondu"
        );
        assert_eq!(
            rt.block_on(by_speed(vec![mute1, slow, quick])),
            vec![quick, slow, mute1]
        );
        assert_eq!(
            rt.block_on(by_speed(vec![mute2, mute1])),
            vec![mute2, mute1]
        );
        assert_eq!(rt.block_on(by_speed(vec![slow])), vec![slow]);
    }

    /// (v334) Plusieurs sources : un serveur muet, puis des identifiants refusés, puis un
    /// bon serveur. Le client finit chez le bon ; à l'allocation suivante, les deux qui ont
    /// échoué passent après lui (pas d'attente).
    #[test]
    fn a_dead_or_refusing_turn_service_hands_over_to_the_next() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let refusing = MiniTurn::start(rt.handle());
        let good = MiniTurn::start(rt.handle());
        // Un port où personne ne répond (ni UDP ni TCP).
        let dead = {
            let u = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
            u.local_addr().unwrap()
        };
        let sources = vec![
            Source::Fixed {
                server: dead.to_string(),
                user: MINI_USER.into(),
                pass: "x".into(),
            },
            Source::Fixed {
                server: refusing.addr.to_string(),
                user: MINI_USER.into(),
                pass: "faux".into(),
            },
            good.source(),
        ];
        let c =
            TurnClient::start(rt.handle(), sources, Box::new(|_, _| {}), Box::new(|_| {})).unwrap();
        wait_for("allocation chez le bon serveur", || c.relayed().is_some());
        assert_eq!(c.st.lock().unwrap().server, Some(good.addr));
        {
            let bad = c.bad.lock().unwrap();
            assert!(bad.contains_key(&dead) && bad.contains_key(&refusing.addr));
        }
        let n = |m: &MiniTurn| m.allocations.load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!((n(&refusing), n(&good)), (0, 1));
        // Nouvelle allocation : le bon d'abord, tout de suite.
        let before = c.relayed();
        c.st.lock().unwrap().renew_at = Some(Instant::now());
        c.wake.notify_one();
        let t = Instant::now();
        wait_for("nouvelle allocation", || {
            c.relayed().is_some_and(|r| Some(r) != before)
        });
        assert!(t.elapsed() < Duration::from_secs(3), "{:?}", t.elapsed());
        assert_eq!(c.st.lock().unwrap().server, Some(good.addr));
        assert_eq!(n(&good), 2);
    }

    /// (v333) Un réseau qui ne laisse sortir que TCP (le serveur ne répond plus en UDP) :
    /// les clients s'allouent leur adresse par TCP, s'échangent des paquets intacts, et
    /// parlent aussi avec un client resté en UDP ; la prochaine allocation va droit à TCP.
    #[test]
    fn a_network_without_udp_gets_its_relayed_address_over_tcp() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let server = MiniTurn::start(rt.handle());
        server
            .udp_off
            .store(true, std::sync::atomic::Ordering::Relaxed);
        type Got = Arc<Mutex<Vec<(SocketAddr, Vec<u8>)>>>;
        let mk = |got: Got| {
            TurnClient::start(
                rt.handle(),
                vec![server.source()],
                Box::new(move |from, p| got.lock().unwrap().push((from, p.to_vec()))),
                Box::new(|_| {}),
            )
            .unwrap()
        };
        let (got_a, got_b, got_c): (Got, Got, Got) = Default::default();
        let (a, b) = (mk(got_a.clone()), mk(got_b.clone()));
        wait_for("adresses relayées par TCP", || {
            a.relayed().is_some() && b.relayed().is_some()
        });
        assert!(a.tcp.lock().unwrap().is_some() && b.tcp.lock().unwrap().is_some());
        assert!(a.via_tcp.load(std::sync::atomic::Ordering::Relaxed));
        let (ra, rb) = (
            SocketAddr::V4(a.relayed().unwrap()),
            SocketAddr::V4(b.relayed().unwrap()),
        );
        // Des tailles qui ne tombent pas sur 4 : le bourrage de TCP ne doit rien changer.
        wait_for("de A à B et retour", || {
            a.send(rb, b"bonjour");
            b.send(ra, b"salut!");
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
                    .any(|(f, p)| *f == rb && p == b"salut!")
        });
        got_b.lock().unwrap().clear();
        for i in 0..300u32 {
            let mut p = i.to_le_bytes().to_vec();
            p.extend(std::iter::repeat_n(i as u8, (i % 7) as usize));
            a.send(rb, &p);
        }
        wait_for("300 paquets par TCP", || got_b.lock().unwrap().len() >= 300);
        for (i, (f, p)) in got_b.lock().unwrap().iter().enumerate() {
            let n = u32::from_le_bytes(p[..4].try_into().unwrap());
            assert_eq!((*f, n as usize, p.len()), (ra, i, 4 + (n % 7) as usize));
        }
        // UDP revient (pour d'autres) : un client en UDP et un client en TCP se parlent.
        server
            .udp_off
            .store(false, std::sync::atomic::Ordering::Relaxed);
        let c = mk(got_c.clone());
        wait_for("adresse relayée en UDP", || c.relayed().is_some());
        assert!(c.tcp.lock().unwrap().is_none());
        let rc = SocketAddr::V4(c.relayed().unwrap());
        wait_for("entre TCP et UDP", || {
            a.send(rc, b"par tcp");
            c.send(ra, b"par udp");
            std::thread::sleep(Duration::from_millis(20));
            got_c
                .lock()
                .unwrap()
                .iter()
                .any(|(f, p)| *f == ra && p == b"par tcp")
                && got_a
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|(f, p)| *f == rc && p == b"par udp")
        });
        // Nouvelle allocation (identifiants renouvelés) : droit à TCP, sans attendre UDP.
        server
            .udp_off
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let before = a.relayed();
        a.st.lock().unwrap().renew_at = Some(Instant::now());
        a.wake.notify_one();
        let t = Instant::now();
        wait_for("nouvelle adresse par TCP", || {
            a.relayed().is_some_and(|r| Some(r) != before)
        });
        assert!(
            t.elapsed() < Duration::from_secs(3),
            "TCP d'abord : {:?}",
            t.elapsed()
        );
        assert!(a.tcp.lock().unwrap().is_some());
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

    /// (v333) Mesure avec un vrai serveur TURN (`MINELIB_TURN_ESSAI`), par UDP puis par TCP :
    /// paquets perdus selon la cadence (lente, 256/s, 512/s, rafale) et, par TCP, selon
    /// qu'ils partent un par un ou collés dans un même envoi. Affiche les comptes.
    #[test]
    #[ignore = "serveur TURN réel (MINELIB_TURN_ESSAI)"]
    fn real_turn_losses_by_pace() {
        struct Say(Instant);
        impl log::Log for Say {
            fn enabled(&self, _: &log::Metadata) -> bool {
                true
            }
            fn log(&self, r: &log::Record) {
                if r.target().contains("turn") {
                    eprintln!("{:6.2} s  {}", self.0.elapsed().as_secs_f32(), r.args());
                }
            }
            fn flush(&self) {}
        }
        let _ = log::set_boxed_logger(Box::new(Say(Instant::now())));
        log::set_max_level(log::LevelFilter::Info);
        let src = std::env::var("MINELIB_TURN_ESSAI").expect("MINELIB_TURN_ESSAI");
        let source = parse_sources(&src).pop().expect("source");
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        type Got = Arc<Mutex<Vec<(SocketAddr, Vec<u8>)>>>;
        let only = std::env::var("MINELIB_TURN_MODE").unwrap_or_default();
        for want_tcp in [false, true] {
            if (only == "tcp" && !want_tcp) || (only == "udp" && want_tcp) {
                continue;
            }
            let (got_a, got_b): (Got, Got) = Default::default();
            let mk = |got: Got| {
                let c = TurnClient::start(
                    rt.handle(),
                    vec![source.clone()],
                    Box::new(move |from, p| got.lock().unwrap().push((from, p.to_vec()))),
                    Box::new(|_| {}),
                )
                .unwrap();
                c.via_tcp
                    .store(want_tcp, std::sync::atomic::Ordering::Relaxed);
                c
            };
            let (a, b) = (mk(got_a.clone()), mk(got_b.clone()));
            let t = Instant::now();
            while !(a.relayed().is_some() && b.relayed().is_some()) {
                assert!(t.elapsed() < Duration::from_secs(40), "pas d'allocation");
                std::thread::sleep(Duration::from_millis(10));
            }
            for c in [&a, &b] {
                if c.tcp.lock().unwrap().is_some() != want_tcp {
                    // Allouée avant que la préférence ne soit posée : une autre.
                    let before = c.relayed();
                    c.st.lock().unwrap().renew_at = Some(Instant::now());
                    c.wake.notify_one();
                    let t = Instant::now();
                    while !c.relayed().is_some_and(|r| Some(r) != before) {
                        assert!(t.elapsed() < Duration::from_secs(40), "pas de réallocation");
                        std::thread::sleep(Duration::from_millis(10));
                    }
                }
            }
            let tcp = (
                a.tcp.lock().unwrap().is_some(),
                b.tcp.lock().unwrap().is_some(),
            );
            let (ra, rb) = (
                SocketAddr::V4(a.relayed().unwrap()),
                SocketAddr::V4(b.relayed().unwrap()),
            );
            eprintln!("allocations en {:?} (TCP : {tcp:?})", t.elapsed());
            let t = Instant::now();
            loop {
                a.send(rb, b"x");
                b.send(ra, b"y");
                std::thread::sleep(Duration::from_millis(30));
                let (ga, gb) = (got_a.lock().unwrap().len(), got_b.lock().unwrap().len());
                if ga > 0 && gb > 0 {
                    eprintln!("canaux en {:?}", t.elapsed());
                    break;
                }
                assert!(
                    t.elapsed() < Duration::from_secs(45),
                    "canaux : rien après 45 s ; reçus A {ga} B {gb} ; compteurs {:?} {:?}",
                    a.counts(),
                    b.counts()
                );
            }
            std::thread::sleep(Duration::from_millis(300));
            let count = |got: &Got, tag: u8| {
                got.lock()
                    .unwrap()
                    .iter()
                    .filter(|(_, p)| p.first() == Some(&tag))
                    .count()
            };
            let mut lines = Vec::new();
            let mut phase = |name: &str, tag: u8, n: usize, every: Duration, size: usize| {
                let t = Instant::now();
                for i in 0..n {
                    let mut p = vec![tag; size];
                    p[1..3].copy_from_slice(&(i as u16).to_le_bytes());
                    a.send(rb, &p);
                    b.send(ra, &p);
                    if !every.is_zero() {
                        let due = t + every * (i as u32 + 1);
                        if let Some(w) = due.checked_duration_since(Instant::now()) {
                            std::thread::sleep(w);
                        }
                    }
                }
                std::thread::sleep(Duration::from_millis(800));
                lines.push(format!(
                    "{name} ({size} o) : A→B {}/{n}, B→A {}/{n}",
                    count(&got_b, tag),
                    count(&got_a, tag)
                ));
            };
            phase("lent 50/s", 1, 100, Duration::from_millis(20), 60);
            phase("256/s", 2, 512, Duration::from_micros(3906), 60);
            phase("256/s gros", 3, 512, Duration::from_micros(3906), 600);
            phase("512/s", 4, 512, Duration::from_micros(1953), 60);
            phase("rafale", 5, 100, Duration::ZERO, 60);
            // (v336) Ce qu'envoie l'hôte d'une partie à 10 dont les 9 joueurs passent par
            // TURN, à 128 mises à jour par seconde : ~1 150 paquets par seconde de 230 à
            // 410 octets (~320 Ko/s), par sa seule allocation ; ici 2 s, dans chaque sens.
            phase(
                "hôte de 10 (320 Ko/s)",
                7,
                2300,
                Duration::from_micros(870),
                280,
            );
            // Deux messages collés dans un même envoi (TCP seulement).
            if tcp.0 {
                let ch = a.st.lock().unwrap().peers[&rb].channel;
                let tx = a.tcp.lock().unwrap().as_ref().unwrap().1.clone();
                for i in 0..100u16 {
                    let mut both = Vec::new();
                    for k in 0..2u16 {
                        let mut p = vec![6u8; 61];
                        p[1..3].copy_from_slice(&(i * 2 + k).to_le_bytes());
                        both.extend(pad4(&channel_data(ch, &p)));
                    }
                    let _ = tx.send(both);
                    std::thread::sleep(Duration::from_millis(10));
                }
                std::thread::sleep(Duration::from_millis(800));
                let got: Vec<u16> = got_b
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|(_, p)| p.first() == Some(&6))
                    .map(|(_, p)| u16::from_le_bytes([p[1], p[2]]))
                    .collect();
                let firsts = got.iter().filter(|n| *n % 2 == 0).count();
                lines.push(format!(
                    "collés par deux : {}/200 (premiers {firsts}/100, seconds {}/100)",
                    got.len(),
                    got.len() - firsts
                ));
            }
            eprintln!(
                "TURN RÉEL par {} :\n  {}",
                if tcp.0 { "TCP" } else { "UDP" },
                lines.join("\n  ")
            );
            eprintln!("compteurs A {:?} B {:?}", a.counts(), b.counts());
        }
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
