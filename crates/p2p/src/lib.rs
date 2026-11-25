//! Jouer en ligne sans serveur à soi et sans ouvrir de port.
//!
//! - **Connexions** : [iroh](https://iroh.computer) (MIT / Apache-2.0) relie deux jeux par
//!   leur identifiant (une clé publique), en perçant les box (QUIC) ; quand c'est
//!   impossible, le trafic passe par les relais publics gratuits de l'équipe d'iroh. Le
//!   relais Minelib d'un joueur devient ainsi joignable de partout : le jeu ouvre un
//!   tunnel (port local → nœud distant) et le protocole Minelib, chiffré de bout en bout
//!   par Noise, passe dedans tel quel.
//! - **Rendez-vous** : le DHT de BitTorrent (« Mainline », des millions de nœuds,
//!   bibliothèque `mainline`, MIT) garde sous une clé connue de tous la liste des relais
//!   Minelib en ligne (identifiant iroh, clé Noise). Un jeu sans relais configuré y prend
//!   le plus ancien encore en ligne ; s'il n'y en a aucun, il devient lui-même le relais et
//!   s'y inscrit. Un relais allumé en permanence (`minelib-relay`) devient donc
//!   naturellement le relais de tout le monde.

use std::net::SocketAddr;
use std::time::Duration;

pub mod passeur;
pub mod portmap;
pub mod rendezvous;
pub mod turn;

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Protocole annoncé dans les connexions iroh.
pub const ALPN: &[u8] = b"minelib/relais/1";
/// Sel de la valeur du DHT (version du format).
const SALT: &[u8] = b"minelib/relais/v1";
/// Un relais qui ne s'est pas réinscrit depuis ce temps est oublié.
const STALE_S: u64 = 30 * 60;
/// Réinscription toutes les…
const REFRESH: Duration = Duration::from_secs(10 * 60);
/// Relais gardés dans la liste (la valeur du DHT fait au plus 1000 octets).
const MAX_ADS: usize = 6;

/// Un relais Minelib en ligne.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Ad {
    /// Identifiant iroh du nœud (clé publique).
    pub node: [u8; 32],
    /// Clé du canal chiffré du relais (à retenir).
    pub noise: [u8; 32],
    pub name: String,
    /// En ligne depuis (s, époque Unix) : le plus ancien est choisi.
    pub since: u64,
    /// Dernière inscription.
    pub seen: u64,
}

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Clé de la liste dans le DHT : connue de tous (n'importe quel relais peut s'y inscrire ;
/// un jeu vérifie chaque relais en s'y connectant, avec sa clé Noise).
fn shared_key() -> mainline::SigningKey {
    let seed: [u8; 32] = Sha256::digest(b"minelib/rendez-vous/v1").into();
    mainline::SigningKey::from_bytes(&seed)
}

/// Relais en ligne, du plus ancien au plus récent.
pub fn decode_ads(bytes: &[u8]) -> Vec<Ad> {
    let mut v: Vec<Ad> = postcard::from_bytes(bytes).unwrap_or_default();
    let t = now();
    v.retain(|a| a.seen + STALE_S > t);
    v.sort_by_key(|a| a.since);
    v
}

/// (v326) Liste mise à jour avec ce relais : les plus anciens d'abord, `MAX_ADS` au plus.
pub fn updated(mut list: Vec<Ad>, me: Ad) -> Vec<Ad> {
    list.retain(|a| a.node != me.node);
    list.push(me);
    list.sort_by_key(|a| a.since);
    list.truncate(MAX_ADS);
    list
}

/// Liste mise à jour avec ce relais.
pub fn upsert(mut list: Vec<Ad>, me: Ad) -> Vec<u8> {
    list.retain(|a| a.node != me.node);
    list.push(me);
    list.sort_by_key(|a| a.since);
    // Les plus anciens d'abord ; on retire les plus récents en trop.
    while list.len() > MAX_ADS {
        list.pop();
    }
    loop {
        let bytes = postcard::to_allocvec(&list).unwrap_or_default();
        if bytes.len() <= 1000 || list.len() <= 1 {
            return bytes;
        }
        list.pop();
    }
}

/// (v279) Relais officiels : la liste compilée dans le jeu (`relais_officiels.txt`, à la
/// racine du dépôt). La liste du DHT est ouverte à tous (sa clé se déduit d'un texte
/// public : n'importe qui peut la réécrire, la remplir de faux relais ou la bloquer avec
/// un numéro de version énorme) ; celle-ci ne change qu'avec une version publiée du jeu.
pub fn official() -> Vec<Ad> {
    parse_relays(include_str!("../../../relais_officiels.txt"))
}

/// Relais écrits un par ligne : nœud iroh et clé du canal chiffré en hexadécimal, puis le
/// nom (voir `relais_officiels.txt` ; même format pour les relais retenus par le jeu).
pub fn parse_relays(text: &str) -> Vec<Ad> {
    fn key(hex: &str) -> Option<[u8; 32]> {
        if hex.len() != 64 || !hex.is_ascii() {
            return None;
        }
        let mut out = [0u8; 32];
        for (i, b) in out.iter_mut().enumerate() {
            *b = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).ok()?;
        }
        Some(out)
    }
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| {
            let mut parts = l.splitn(3, char::is_whitespace);
            let node = key(parts.next()?)?;
            let noise = key(parts.next()?)?;
            let name: String = parts.next().unwrap_or("").trim().chars().take(40).collect();
            Some(Ad {
                node,
                noise,
                name,
                since: 0,
                seen: now(),
            })
        })
        .collect()
}

/// Ces relais écrits au format de `parse_relays`.
pub fn format_relays(list: &[Ad]) -> String {
    let hex = |k: &[u8; 32]| k.iter().map(|b| format!("{b:02x}")).collect::<String>();
    list.iter()
        .map(|a| format!("{} {} {}\n", hex(&a.node), hex(&a.noise), a.name.replace('\n', " ")))
        .collect()
}

/// Client du DHT, créé une fois et gardé (le rejoindre prend une à deux secondes).
fn dht() -> Result<mainline::Dht> {
    static DHT: std::sync::Mutex<Option<mainline::Dht>> = std::sync::Mutex::new(None);
    let mut g = DHT.lock().unwrap();
    if g.is_none() {
        // Pas de réseau au lancement : on réessaiera à la prochaine demande.
        *g = Some(mainline::Dht::client().map_err(|e| anyhow!("DHT : {e}"))?);
    }
    Ok(g.clone().expect("DHT"))
}

/// (v327) Nos serveurs relais d'iroh (`relais_iroh.txt`, compilé dans le jeu ; la variable
/// `MINELIB_RELAIS_IROH`, des adresses séparées par des virgules, les remplace) : le chemin
/// de secours des connexions, à la place de ceux de l'équipe d'iroh, limités en débit.
pub fn own_relay_urls() -> Vec<String> {
    match std::env::var("MINELIB_RELAIS_IROH") {
        Ok(v) => parse_relay_urls(&v.replace(',', "\n")),
        Err(_) => parse_relay_urls(include_str!("../../../relais_iroh.txt")),
    }
}

/// Une adresse `http(s)://…` par ligne ; lignes vides et « # » ignorées.
pub fn parse_relay_urls(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| l.starts_with("https://") || l.starts_with("http://"))
        .map(|l| l.trim_end_matches('/').to_string())
        .collect()
}

/// Ceux de `urls` qui répondent maintenant (`/ping`, `wait` au plus, tous en même temps).
pub fn reachable_relays(urls: &[String], wait: Duration) -> Vec<String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .build(),
        )
        .timeout_global(Some(wait))
        .build()
        .into();
    let checks: Vec<_> = urls
        .iter()
        .map(|u| {
            let (agent, u) = (agent.clone(), u.clone());
            std::thread::spawn(move || {
                let ok = agent
                    .get(&format!("{u}/ping"))
                    .call()
                    .is_ok_and(|r| r.status().is_success());
                (u, ok)
            })
        })
        .collect();
    checks
        .into_iter()
        .filter_map(|c| c.join().ok())
        .filter(|(_, ok)| *ok)
        .map(|(u, _)| u)
        .collect()
}

/// (v327) Nos serveurs relais qui répondent, vérifiés au plus toutes les 10 minutes (chaque
/// nœud du jeu ne les réinterroge pas).
fn own_relays_up() -> Vec<iroh::RelayUrl> {
    static UP: std::sync::Mutex<Option<(std::time::Instant, Vec<iroh::RelayUrl>)>> =
        std::sync::Mutex::new(None);
    let mut g = UP.lock().unwrap();
    if let Some((t, v)) = g.as_ref() {
        if t.elapsed() < Duration::from_secs(600) {
            return v.clone();
        }
    }
    let urls = own_relay_urls();
    let up: Vec<iroh::RelayUrl> = if urls.is_empty() {
        Vec::new()
    } else {
        reachable_relays(&urls, Duration::from_secs(2))
            .into_iter()
            .filter_map(|u| u.parse().ok())
            .collect()
    };
    if !urls.is_empty() {
        log::info!(
            "serveurs relais d'iroh à nous : {} sur {} répondent{}",
            up.len(),
            urls.len(),
            if up.is_empty() { " (ceux de l'équipe d'iroh à la place)" } else { "" }
        );
    }
    *g = Some((std::time::Instant::now(), up.clone()));
    up
}

/// (v327) Le dernier nœud créé passe-t-il par nos serveurs relais (rapides, sans limite d'un
/// tiers) plutôt que par ceux de l'équipe d'iroh ?
static OWN_IN_USE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn own_relays_in_use() -> bool {
    OWN_IN_USE.load(std::sync::atomic::Ordering::Relaxed)
}

/// (v326) Le DHT de BitTorrent est-il permis à ce programme ? Seulement à une machine sans
/// joueur (`minelib-relay`, relais ou passeur seul : `allow_dht`). Un jeu n'y parle jamais
/// lui-même : les nœuds du DHT verraient l'adresse IP du joueur.
static DHT_ALLOWED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// (v326) Permet le DHT de BitTorrent, en plus des relais pkarr : pour `minelib-relay`.
pub fn allow_dht() {
    DHT_ALLOWED.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// (v329) Les nœuds de ce programme prennent-ils une adresse chez un serveur TURN
/// (`relais_turn.txt`) ? Le jeu et le relais officiel oui (`enable_turn`) ; un passeur
/// sans joueur n'en a pas besoin, et les essais choisissent eux-mêmes.
static TURN_ON: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn enable_turn() {
    TURN_ON.store(true, std::sync::atomic::Ordering::Relaxed);
}

pub(crate) fn turn_enabled() -> bool {
    TURN_ON.load(std::sync::atomic::Ordering::Relaxed)
}

fn dht_allowed() -> bool {
    DHT_ALLOWED.load(std::sync::atomic::Ordering::Relaxed)
}

/// Lit la liste des relais (quelques secondes). (v326) Par les relais pkarr (HTTPS) : le DHT
/// de BitTorrent n'est lu directement que par une machine sans joueur (`allow_dht`), quand
/// ils ne répondent pas.
pub fn find_relays() -> Result<Vec<Ad>> {
    match rendezvous::fetch() {
        Ok(list) => Ok(list),
        Err(e) if dht_allowed() => {
            log::info!("rendez-vous : {e:#} ; lu dans le DHT de BitTorrent");
            find_relays_dht()
        }
        Err(e) => Err(e),
    }
}

/// La liste des relais lue directement dans le DHT de BitTorrent (ancien format, toujours
/// tenu par les machines sans joueur).
#[allow(deprecated)]
fn find_relays_dht() -> Result<Vec<Ad>> {
    let dht = dht()?;
    let key = shared_key();
    let item = dht.get_mutable_most_recent(key.verifying_key().as_bytes(), Some(SALT));
    Ok(item.map(|i| decode_ads(i.value())).unwrap_or_default())
}

/// Inscrit ce relais dans la liste, puis toutes les 10 minutes (fil de fond). (v326) Chez
/// les relais pkarr ; dans le DHT de BitTorrent aussi pour une machine sans joueur
/// (`allow_dht`), jamais pour le relais d'un joueur.
pub fn publish(me: Ad) {
    let _ = std::thread::Builder::new()
        .name("rendez-vous".into())
        .spawn(move || {
            let pkarr = |me: &Ad| -> Result<()> {
                let list = rendezvous::fetch()?;
                let mut ad = me.clone();
                ad.seen = now();
                rendezvous::put(&updated(list, ad))
            };
            #[allow(deprecated)]
            let run = |me: &Ad| -> Result<()> {
                let dht = dht()?;
                let key = shared_key();
                let current =
                    dht.get_mutable_most_recent(key.verifying_key().as_bytes(), Some(SALT));
                let list = current.map(|i| decode_ads(i.value())).unwrap_or_default();
                let mut ad = me.clone();
                ad.seen = now();
                let bytes = upsert(list, ad);
                let item = mainline::MutableItem::new(key, &bytes, now() as i64, Some(SALT));
                dht.put_mutable(item, None).map_err(|e| anyhow!("{e:?}"))?;
                Ok(())
            };
            loop {
                match pkarr(&me) {
                    Ok(()) => log::info!("relais inscrit chez les relais pkarr"),
                    Err(e) => log::warn!("inscription chez les relais pkarr : {e:#}"),
                }
                if dht_allowed() {
                    match run(&me) {
                        Ok(()) => log::info!("relais inscrit dans le DHT"),
                        Err(e) => log::warn!("inscription dans le DHT : {e:#}"),
                    }
                }
                std::thread::sleep(REFRESH);
            }
        });
}

/// Début d'une connexion passée par le tunnel : « MLPX », une longueur, puis qui est
/// le joueur distant (son adresse IP, ou à défaut son identifiant iroh).
pub const PREAMBLE: &[u8; 4] = b"MLPX";

pub fn preamble(who: &str) -> Vec<u8> {
    let who = &who.as_bytes()[..who.len().min(255)];
    let mut v = PREAMBLE.to_vec();
    v.push(who.len() as u8);
    v.extend_from_slice(who);
    v
}

/// Qui est au bout de `conn` : son IP quand on la connaît (connexion directe), sinon
/// son identifiant iroh (la connexion passe alors par un serveur relais d'iroh).
fn remote_key(conn: &iroh::endpoint::Connection, first: Option<std::net::IpAddr>) -> String {
    let direct = conn.paths().iter().find_map(|p| match p.remote_addr() {
        iroh::TransportAddr::Ip(a) => Some(a.ip()),
        _ => None,
    });
    match direct.or(first) {
        Some(ip) => ip.to_canonical().to_string(),
        None => {
            let id = conn.remote_id();
            let hex: String = id.as_bytes()[..8]
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            format!("iroh:{hex}")
        }
    }
}

/// Les relais de région, de province et de secours hébergés par ce jeu sont-ils joignables
/// en direct ? (v280 : `hote_direct=true` dans `options.txt`.) (v297) Trois réglages :
/// 1 toujours, 2 jamais, 0 automatique (sans la ligne). (v301) En automatique, plus jamais
/// de lui-même : voir `host_direct`.
static HOSTS_DIRECT: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

pub fn set_hosts_direct(choice: Option<bool>) {
    let v = match choice {
        None => 0,
        Some(true) => 1,
        Some(false) => 2,
    };
    HOSTS_DIRECT.store(v, std::sync::atomic::Ordering::Relaxed);
}

/// (v297) Un relais hébergé par ce jeu doit-il être joignable en direct ? `open_box` : ce
/// PC est un passeur vérifié en ce moment.
///
/// En direct, les joueurs servis voient l'adresse IP de l'hôte, et savent que c'est lui
/// qui héberge. (v301) Ce n'est plus fait que sur demande (`hote_direct=true`) : un hôte
/// anonyme est maintenant joint par des passeurs (`passeur`), sans le débit limité des
/// serveurs relais d'iroh. (v308) Toujours deux, en chaîne ; son propre jeu ne porte plus
/// jamais ses tunnels (il verrait l'adresse de ses joueurs). La v297 le mettait en direct
/// de lui-même quand sa box était ouverte.
pub fn host_direct(_open_box: bool) -> bool {
    HOSTS_DIRECT.load(std::sync::atomic::Ordering::Relaxed) == 1
}

/// (v320) Une adresse joignable de partout : ni réseau local, ni boucle locale, ni la plage
/// que les opérateurs partagent entre leurs abonnés (100.64.0.0/10).
fn is_public_v4(ip: std::net::Ipv4Addr) -> bool {
    let o = ip.octets();
    !(ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_documentation()
        || (o[0] == 100 && (64..128).contains(&o[1])))
}

/// (v285) Connexions iroh ouvertes par les tunnels de ce jeu, par nœud distant.
static TUNNELS: std::sync::Mutex<Vec<([u8; 32], iroh::endpoint::Connection)>> =
    std::sync::Mutex::new(Vec::new());

fn remember_tunnel(remote: [u8; 32], conn: &iroh::endpoint::Connection) {
    let mut t = TUNNELS.lock().unwrap();
    t.retain(|(_, c)| c.close_reason().is_none());
    t.push((remote, conn.clone()));
}

/// (v333) Tunnels de ce jeu, par l'adresse locale où l'on s'y connecte (celle que la
/// connexion au relais a en main) : de quoi dire si le lien passe en ce moment par TURN.
type LocalLinks = Vec<(
    SocketAddr,
    iroh::endpoint::Connection,
    passeur::PasseurTransport,
)>;
static LOCAL_LINKS: std::sync::Mutex<LocalLinks> = std::sync::Mutex::new(Vec::new());

fn remember_link(
    local: SocketAddr,
    conn: &iroh::endpoint::Connection,
    passeurs: &passeur::PasseurTransport,
) {
    let mut l = LOCAL_LINKS.lock().unwrap();
    l.retain(|(a, c, _)| *a != local && c.close_reason().is_none());
    l.push((local, conn.clone(), passeurs.clone()));
}

/// (v333) Le tunnel joint à l'adresse locale `addr` passe-t-il en ce moment par TURN ? (Le
/// chemin qu'iroh a choisi est-il une route de passeurs, et son saut en cours le saut
/// TURN ?) Rien : ce n'est pas un tunnel de ce jeu.
fn link_probe(addr: &str) -> Option<bool> {
    let a: SocketAddr = addr.parse().ok()?;
    let (conn, passeurs) = {
        let l = LOCAL_LINKS.lock().unwrap();
        let (_, c, p) = l
            .iter()
            .find(|(x, c, _)| *x == a && c.close_reason().is_none())?;
        (c.clone(), p.clone())
    };
    let paths = conn.paths();
    let selected = paths.iter().find(|p| p.is_selected())?;
    Some(match selected.remote_addr() {
        iroh::TransportAddr::Custom(c) => {
            passeur::route_of(c).is_some_and(|r| passeurs.via_turn(r))
        }
        _ => false,
    })
}

/// (v285) Ce jeu joint-il le nœud `remote` en direct ? `None` s'il n'y a pas de tunnel
/// vers lui (réseau local, même machine) ; `Some(false)` tant que tout passe par un
/// serveur relais d'iroh (hôte anonyme, ou box qui ne laisse pas faire) : ces serveurs
/// limitent le débit, le jeu n'y envoie pas ses positions 256 fois par seconde.
pub fn node_direct(remote: [u8; 32]) -> Option<bool> {
    let t = TUNNELS.lock().unwrap();
    let mut any = None;
    for (_, c) in t.iter().filter(|(n, c)| *n == remote && c.close_reason().is_none()) {
        // (v301) Par un passeur, le débit n'est pas limité non plus ; et c'est le chemin
        // qui sert en ce moment qui compte, pas un chemin direct ouvert à côté.
        let paths = c.paths();
        let direct = match paths.iter().find(|p| p.is_selected()) {
            Some(p) => !p.is_relay(),
            None => paths.iter().any(|p| !p.is_relay()),
        };
        any = Some(any.unwrap_or(false) || direct);
    }
    any
}

/// Nœud iroh de ce jeu (un par processus).
pub struct Node {
    rt: tokio::runtime::Runtime,
    ep: iroh::Endpoint,
    /// (v301) Ses routes par les passeurs (voir `passeur`).
    passeurs: passeur::PasseurTransport,
}

async fn pipe(
    tcp: tokio::net::TcpStream,
    mut send: iroh::endpoint::SendStream,
    mut recv: iroh::endpoint::RecvStream,
) {
    let (mut r, mut w) = tcp.into_split();
    let up = async {
        let _ = tokio::io::copy(&mut r, &mut send).await;
        let _ = send.finish();
    };
    let down = async {
        let _ = tokio::io::copy(&mut recv, &mut w).await;
    };
    tokio::join!(up, down);
}

/// (v329) Le nœud s'en va : son client TURN aussi (l'adresse relayée n'est plus dite aux
/// relais), même si iroh garde encore un moment de quoi joindre son transport.
impl Drop for Node {
    fn drop(&mut self) {
        self.passeurs.stop_turn();
    }
}

impl Node {
    pub fn start() -> Result<Self> {
        Self::start_with(None)
    }

    /// Nœud d'identité fixe (`secret`, 32 octets gardés sur le disque) : d'un lancement à
    /// l'autre il garde le même identifiant, et reconnaît donc ses propres annonces dans
    /// le DHT au lieu de les prendre pour le relais de quelqu'un d'autre.
    pub fn start_with(secret: Option<[u8; 32]>) -> Result<Self> {
        Self::build(secret, false)
    }

    /// Nœud anonyme : joignable seulement par les serveurs relais d'iroh, jamais en direct.
    /// Ceux qui s'y connectent ne voient pas son adresse IP (hôte d'une région d'un monde :
    /// un joueur d'une autre partie qui doit rester anonyme pour les joueurs servis).
    pub fn start_private() -> Result<Self> {
        Self::build(None, true)
    }

    /// (v297) Nœud d'un relais hébergé par ce jeu (région, province, secours) : anonyme
    /// (joint par des passeurs, sinon par les serveurs relais d'iroh), ou joignable en
    /// direct si le joueur l'a demandé (`host_direct`).
    pub fn start_host(open_box: bool) -> Result<Self> {
        Self::build(None, !host_direct(open_box))
    }

    /// (v313) Nœud du jeu lui-même (identité fixe `secret`) : anonyme lui aussi. Il n'a pas
    /// de transport IP : il ne joint les relais, et n'est joint quand il en devient un, que
    /// par des passeurs ou par les serveurs relais d'iroh. Avant, il ouvrait des liens
    /// directs dès que les box le permettaient : l'hôte d'un relais voyait l'adresse IP de
    /// chaque joueur (elle servait même de clé à ses limites), et chaque joueur la sienne.
    /// Sauf si le joueur a demandé les liens directs (`hote_direct=true`) : son adresse est
    /// alors visible de ceux qui l'ont demandé aussi, et d'eux seulement (un lien direct
    /// demande un transport IP aux deux bouts).
    pub fn start_game(secret: [u8; 32]) -> Result<Self> {
        Self::build(Some(secret), !host_direct(false))
    }

    /// (v301) Nœud sans serveur d'iroh ni adresse directe : il n'est joignable, et ne
    /// joint les autres, que par des passeurs (essais sans réseau).
    #[doc(hidden)]
    pub fn start_offline() -> Result<Self> {
        Self::build_with(None, true, true)
    }

    fn build(secret: Option<[u8; 32]>, private: bool) -> Result<Self> {
        Self::build_with(secret, private, false)
    }

    fn build_with(secret: Option<[u8; 32]>, private: bool, offline: bool) -> Result<Self> {
        // (v327) Nos serveurs relais, s'ils répondent : à la place de ceux de l'équipe d'iroh.
        let ours = if offline { Vec::new() } else { own_relays_up() };
        if !offline {
            OWN_IN_USE.store(!ours.is_empty(), std::sync::atomic::Ordering::Relaxed);
        }
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("iroh")
            .enable_all()
            .build()?;
        let (ep, passeurs) = rt
            .block_on(async {
                // (v301) Trois chemins pour un même nœud : le direct (sauf nœud anonyme),
                // les passeurs, et les serveurs relais d'iroh en dernier recours.
                let passeurs = passeur::PasseurTransport::bind()?;
                let b = if offline {
                    iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
                        .relay_mode(iroh::RelayMode::Disabled)
                        .clear_address_lookup()
                } else if ours.is_empty() {
                    iroh::Endpoint::builder(iroh::endpoint::presets::N0)
                } else {
                    iroh::Endpoint::builder(iroh::endpoint::presets::N0)
                        .relay_mode(iroh::RelayMode::Custom(ours.iter().cloned().collect()))
                };
                let mut b = b
                    .alpns(vec![ALPN.to_vec()])
                    .add_custom_transport(std::sync::Arc::new(passeurs.clone()));
                if let Some(s) = secret {
                    b = b.secret_key(iroh::SecretKey::from_bytes(&s));
                }
                if private {
                    b = b.clear_ip_transports();
                }
                let ep = b.bind().await.map_err(|e| anyhow!("{e}"))?;
                anyhow::Ok((ep, passeurs))
            })
            .map_err(|e| anyhow!("iroh : {e}"))?;
        passeurs.register(*ep.id().as_bytes());
        Ok(Self { rt, ep, passeurs })
    }

    /// (v301) Ses routes par les passeurs (essais, journal).
    pub fn passeurs(&self) -> &passeur::PasseurTransport {
        &self.passeurs
    }

    /// Comme [`Node::tunnel`], mais attend que le nœud distant réponde (au plus `wait`) :
    /// un relais éteint dont l'annonce traîne encore dans le DHT est écarté tout de suite,
    /// au lieu de faire échouer la connexion dix secondes plus tard.
    pub fn tunnel_checked(&self, remote: [u8; 32], wait: Duration) -> Result<SocketAddr> {
        let id = iroh::EndpointId::from_bytes(&remote).map_err(|e| anyhow!("{e}"))?;
        let addr: iroh::EndpointAddr = id.into();
        let ep = self.ep.clone();
        let (conn, listener) = self.rt.block_on(async move {
            let conn = tokio::time::timeout(wait, ep.connect(addr, ALPN))
                .await
                .map_err(|_| anyhow!("le relais distant ne répond pas"))?
                .map_err(|e| anyhow!("relais distant : {e}"))?;
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
            anyhow::Ok((conn, listener))
        })?;
        remember_tunnel(remote, &conn);
        let local = listener.local_addr()?;
        remember_link(local, &conn, &self.passeurs);
        self.rt.spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                let Ok((send, recv)) = conn.open_bi().await else {
                    break;
                };
                tokio::spawn(pipe(tcp, send, recv));
            }
        });
        Ok(local)
    }

    pub fn id(&self) -> [u8; 32] {
        *self.ep.id().as_bytes()
    }

    /// (v320) Adresse IPv4 publique de ce nœud, telle que la voient les serveurs d'iroh
    /// (nœud avec transport IP seulement), attendue `wait` au plus : de quoi annoncer un
    /// passeur sans joueur depuis un serveur, qui n'a pas de box à interroger.
    pub fn public_ipv4(&self, wait: Duration) -> Option<std::net::Ipv4Addr> {
        let start = std::time::Instant::now();
        loop {
            let found = self.ep.addr().addrs.iter().find_map(|a| match a {
                iroh::TransportAddr::Ip(SocketAddr::V4(s)) if is_public_v4(*s.ip()) => {
                    Some(*s.ip())
                }
                _ => None,
            });
            if found.is_some() || start.elapsed() >= wait {
                return found;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    /// Rend le relais local `local` joignable par ce nœud. Chaque connexion commence
    /// par un préambule ([`preamble`]) qui donne au relais la vraie adresse du joueur
    /// distant : sans lui, tout le monde arriverait de 127.0.0.1.
    pub fn serve(&self, local: SocketAddr) {
        let ep = self.ep.clone();
        self.rt.spawn(async move {
            while let Some(incoming) = ep.accept().await {
                tokio::spawn(async move {
                    let first = match incoming.remote_addr() {
                        iroh::endpoint::IncomingAddr::Ip(a) => Some(a.ip()),
                        _ => None,
                    };
                    let Ok(conn) = incoming.await else { return };
                    while let Ok((send, recv)) = conn.accept_bi().await {
                        let who = remote_key(&conn, first);
                        tokio::spawn(async move {
                            match tokio::net::TcpStream::connect(local).await {
                                Ok(mut tcp) => {
                                    use tokio::io::AsyncWriteExt;
                                    if tcp.write_all(&preamble(&who)).await.is_ok() {
                                        pipe(tcp, send, recv).await
                                    }
                                }
                                Err(e) => log::warn!("tunnel vers le relais local : {e}"),
                            }
                        });
                    }
                });
            }
        });
    }

    /// (v320) Tunnel vers le nœud `remote` qui ne s'ouvre que par une route de passeurs, et
    /// seulement si c'est elle qu'iroh choisit : le lien d'un joueur avec le relais du monde,
    /// une fois entré par les serveurs d'iroh (c'est par ce premier lien que la route est
    /// demandée). Rien d'ouvert sinon.
    ///
    /// À ouvrir depuis un nœud qui n'a encore aucune connexion vers `remote` : iroh fait
    /// partir une nouvelle connexion par le chemin déjà choisi vers ce nœud, et n'ajoute
    /// jamais celui des passeurs à une connexion ouverte.
    pub fn tunnel_by_passeurs(&self, remote: [u8; 32], wait: Duration) -> Result<SocketAddr> {
        let id = iroh::EndpointId::from_bytes(&remote).map_err(|e| anyhow!("{e}"))?;
        let ep = self.ep.clone();
        let passeurs = self.passeurs.clone();
        // (Une route d'un tunnel fermé, gardée d'avant, n'est plus bonne.)
        minelib_net::fast::forget_route(remote);
        let (conn, listener, route) = self.rt.block_on(async move {
            let route = passeur::ask(&passeurs, remote)
                .await
                .ok_or_else(|| anyhow!("pas de route par des passeurs"))?;
            let addr = iroh::EndpointAddr::from_parts(
                id,
                [iroh::TransportAddr::Custom(passeur::addr(route))],
            );
            let conn = match tokio::time::timeout(wait, ep.connect(addr, ALPN)).await {
                Ok(Ok(c)) => c,
                Ok(Err(e)) => {
                    passeurs.drop_route(route);
                    return Err(anyhow!("par les passeurs : {e}"));
                }
                Err(_) => {
                    passeurs.drop_route(route);
                    return Err(anyhow!("par les passeurs : délai dépassé"));
                }
            };
            // Le chemin en service doit être celui des passeurs (ni un serveur d'iroh, ni un
            // lien direct).
            let start = std::time::Instant::now();
            loop {
                let on = {
                    let paths = conn.paths();
                    let selected = paths.iter().find(|p| p.is_selected()).map(|p| {
                        !p.is_relay() && !p.is_ip()
                    });
                    selected
                };
                match on {
                    Some(true) => break,
                    Some(false) => {
                        conn.close(0u32.into(), b"pas par les passeurs");
                        passeurs.drop_route(route);
                        return Err(anyhow!("iroh a choisi un autre chemin que les passeurs"));
                    }
                    None if start.elapsed() > Duration::from_secs(2) => {
                        conn.close(0u32.into(), b"pas par les passeurs");
                        passeurs.drop_route(route);
                        return Err(anyhow!("aucun chemin choisi"));
                    }
                    None => tokio::time::sleep(Duration::from_millis(20)).await,
                }
            }
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
            anyhow::Ok((conn, listener, route))
        })?;
        remember_tunnel(remote, &conn);
        log::info!("tunnel : lien vers le relais ouvert par des passeurs");
        let local = listener.local_addr()?;
        remember_link(local, &conn, &self.passeurs);
        self.rt
            .spawn(passeur::keep(self.passeurs.clone(), remote, route, conn.clone()));
        self.rt.spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                let Ok((send, recv)) = conn.open_bi().await else {
                    break;
                };
                tokio::spawn(pipe(tcp, send, recv));
            }
        });
        Ok(local)
    }

    /// Tunnel vers le relais du nœud `remote` : renvoie une adresse locale où se
    /// connecter en TCP comme à un relais ordinaire.
    pub fn tunnel(&self, remote: [u8; 32]) -> Result<SocketAddr> {
        let id = iroh::EndpointId::from_bytes(&remote).map_err(|e| anyhow!("{e}"))?;
        self.tunnel_to(id.into(), remote)
    }

    fn tunnel_to(&self, id: iroh::EndpointAddr, remote: [u8; 32]) -> Result<SocketAddr> {
        let listener = self
            .rt
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))?;
        let addr = listener.local_addr()?;
        let ep = self.ep.clone();
        let passeurs = self.passeurs.clone();
        self.rt.spawn(async move {
            // (v301) Une route par des passeurs est demandée au relais du monde ; si elle
            // vient à temps, le tunnel y passe. Sinon, comme avant : en direct ou par les
            // serveurs relais d'iroh.
            let route = passeur::ask(&passeurs, remote).await;
            let id = match route {
                Some(r) => {
                    let mut addrs: Vec<iroh::TransportAddr> = id.addrs.iter().cloned().collect();
                    addrs.push(iroh::TransportAddr::Custom(passeur::addr(r)));
                    iroh::EndpointAddr::from_parts(id.id, addrs)
                }
                None => id,
            };
            let conn =
                match tokio::time::timeout(Duration::from_secs(20), ep.connect(id, ALPN)).await {
                    Ok(Ok(c)) => c,
                    Ok(Err(e)) => {
                        log::warn!("connexion au relais distant : {e}");
                        return;
                    }
                    Err(_) => {
                        log::warn!("connexion au relais distant : délai dépassé");
                        return;
                    }
                };
            remember_tunnel(remote, &conn);
            remember_link(addr, &conn, &passeurs);
            // (Journal : ni adresse ni nœud, seulement par où passe le tunnel.)
            match route {
                Some(r) => {
                    log::info!("tunnel : ouvert, avec une route par des passeurs");
                    tokio::spawn(passeur::keep(passeurs, remote, r, conn.clone()));
                }
                None => log::info!(
                    "tunnel : ouvert sans passeur (en direct, ou par les serveurs relais d'iroh)"
                ),
            }
            while let Ok((tcp, _)) = listener.accept().await {
                let Ok((send, recv)) = conn.open_bi().await else {
                    break;
                };
                tokio::spawn(pipe(tcp, send, recv));
            }
        });
        Ok(addr)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ad(n: u8, since: u64) -> Ad {
        Ad {
            node: [n; 32],
            noise: [n; 32],
            name: format!("relais {n}"),
            since,
            seen: now(),
        }
    }

    /// (v279) Les relais écrits un par ligne se relisent ; une ligne fausse est ignorée, et
    /// la liste officielle compilée dans le jeu se lit sans erreur.
    /// (v327) Nos serveurs relais : la liste compilée se lit (adresses seulement), et seuls
    /// ceux qui répondent à `/ping` sont gardés.
    #[test]
    fn our_relay_servers_are_kept_only_when_they_answer() {
        let urls = parse_relay_urls(
            "# commentaire\n\nhttps://a.example/\n  http://127.0.0.1:9  \npas une adresse\n",
        );
        assert_eq!(urls, vec!["https://a.example", "http://127.0.0.1:9"]);
        assert!(own_relay_urls().iter().all(|u| u.starts_with("http")));
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let up = format!("http://{}", l.local_addr().unwrap());
        std::thread::spawn(move || {
            use std::io::{BufRead, BufReader, Write};
            for c in l.incoming().flatten() {
                let mut r = BufReader::new(c.try_clone().unwrap());
                let mut line = String::new();
                let _ = r.read_line(&mut line);
                loop {
                    let mut h = String::new();
                    if r.read_line(&mut h).unwrap_or(0) == 0 || h.trim().is_empty() {
                        break;
                    }
                }
                let mut c = c;
                let status = if line.starts_with("GET /ping ") { "200 OK" } else { "404 Not Found" };
                let _ = write!(c, "HTTP/1.1 {status}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n");
            }
        });
        let down = "http://127.0.0.1:1".to_string();
        let got = reachable_relays(&[down, up.clone()], Duration::from_secs(2));
        assert_eq!(got, vec![up]);
    }

    /// (v297, v301) Un hôte n'est joint en direct que si le joueur l'a demandé : jamais de
    /// lui-même, même quand sa box est ouverte (il porte alors ses tunnels comme passeur).
    #[test]
    fn a_host_is_direct_only_when_asked() {
        set_hosts_direct(None);
        assert!(!host_direct(false) && !host_direct(true));
        set_hosts_direct(Some(true));
        assert!(host_direct(false) && host_direct(true));
        set_hosts_direct(Some(false));
        assert!(!host_direct(false) && !host_direct(true));
        set_hosts_direct(None);
    }

    /// (v320) Seule une adresse joignable de partout sert à annoncer un passeur.
    #[test]
    fn only_a_public_ipv4_is_announced() {
        for (ip, public) in [
            ("203.0.113.7", false),
            ("192.168.1.10", false),
            ("10.0.0.1", false),
            ("100.72.1.1", false),
            ("127.0.0.1", false),
            ("169.254.0.1", false),
            ("82.65.10.3", true),
            ("141.147.20.9", true),
        ] {
            assert_eq!(is_public_v4(ip.parse().unwrap()), public, "{ip}");
        }
    }

    #[test]
    fn relays_written_one_per_line_read_back() {
        let ad = |n: u8, name: &str| Ad {
            node: [n; 32],
            noise: [n + 1; 32],
            name: name.into(),
            since: 0,
            seen: 0,
        };
        let text = format_relays(&[ad(1, "Relais de Paris"), ad(7, "")]);
        let extra = format!("# commentaire\n\n{text}trop court abc nom\n");
        let back = parse_relays(&extra);
        assert_eq!(back.len(), 2);
        assert_eq!((back[0].node, back[0].noise), ([1; 32], [2; 32]));
        assert_eq!(back[0].name, "Relais de Paris");
        assert_eq!((back[1].node, back[1].name.as_str()), ([7; 32], ""));
        for a in official() {
            assert_ne!(a.node, a.noise);
        }
    }

    #[test]
    fn the_oldest_relay_comes_first_and_the_list_fits_the_dht() {
        let mut list = Vec::new();
        for n in 0..10u8 {
            let bytes = upsert(list.clone(), ad(n, 1000 - n as u64));
            assert!(bytes.len() <= 1000);
            list = decode_ads(&bytes);
        }
        assert!(list.len() <= MAX_ADS);
        assert_eq!(list[0].node, [9; 32], "le plus ancien d'abord");
        // Un relais oublié depuis longtemps disparaît.
        let mut old = ad(42, 1);
        old.seen = 0;
        let bytes = postcard::to_allocvec(&vec![old]).unwrap();
        assert!(decode_ads(&bytes).is_empty());
    }

    /// Un petit relais TCP local qui renvoie ce qu'il reçoit (après le préambule du tunnel).
    fn echo() -> SocketAddr {
        use std::io::{Read, Write};
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let local = server.local_addr().unwrap();
        std::thread::spawn(move || {
            while let Ok((mut s, _)) = server.accept() {
                std::thread::spawn(move || {
                    let mut head = [0u8; 5];
                    if s.read_exact(&mut head).is_err() {
                        return;
                    }
                    let mut who = vec![0u8; head[4] as usize];
                    let _ = s.read_exact(&mut who);
                    let mut b = [0u8; 5];
                    while s.read_exact(&mut b).is_ok() {
                        if s.write_all(&b).is_err() {
                            break;
                        }
                    }
                });
            }
        });
        local
    }

    fn said_back(c: &mut std::net::TcpStream, what: &[u8; 5]) -> bool {
        use std::io::{Read, Write};
        let mut back = [0u8; 5];
        c.write_all(what).is_ok() && c.read_exact(&mut back).is_ok() && &back == what
    }

    /// (v317) Les essais qui passent par l'état commun du réseau (passeurs connus, veille)
    /// vont l'un après l'autre.
    fn serial() -> std::sync::MutexGuard<'static, ()> {
        static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
        SERIAL.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// (v317) Un jeu en veille au fond d'un pays n'a plus de lien avec le relais du monde.
    /// Un tunnel doit s'ouvrir (une région de plus en vue) : au lieu de prendre d'office
    /// les serveurs d'iroh, il demande au jeu de revenir au relais du monde, attend sa
    /// route et passe par le passeur. Hors veille, ou si ce relais n'avait pas de passeur,
    /// rien n'attend.
    #[test]
    fn a_tunnel_opened_while_dozing_brings_the_game_back_for_a_route() {
        use minelib_net::fast;
        assert!(passeur::wait_for_relay(true, true));
        assert!(!passeur::wait_for_relay(false, true), "hors veille : pas d'attente");
        assert!(!passeur::wait_for_relay(true, false), "relais sans passeur");
        let _serial = serial();
        let host = Node::start_offline().unwrap();
        host.serve(echo());
        let guest = Node::start_offline().unwrap();
        let (tx, _probes) = std::sync::mpsc::channel();
        let p = fast::Passeur::start(0, tx).unwrap();
        // Le relais du monde avait des passeurs ; puis le jeu s'est endormi.
        fast::note_passeurs();
        std::thread::sleep(Duration::from_millis(3100));
        assert!(!fast::passeurs_known() && fast::passeurs_lately());
        fast::set_dozing(true);
        // Le jeu : prié de revenir, il revient ; le relais du monde donne alors la route.
        let hid = host.id();
        let game = std::thread::spawn(move || {
            let t = std::time::Instant::now();
            while !fast::take_route_wanted() {
                assert!(t.elapsed() < Duration::from_secs(5), "retour jamais demandé");
                std::thread::sleep(Duration::from_millis(5));
            }
            std::thread::sleep(Duration::from_millis(300));
            give(hid, 0x53, &[&p]);
            p
        });
        let t = guest.tunnel(host.id()).unwrap();
        let mut c = std::net::TcpStream::connect(t).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
        assert!(said_back(&mut c, b"salut"), "le tunnel ne passe pas");
        let p = game.join().unwrap();
        assert!(p.carried() > 0, "le tunnel n'est pas passé par le passeur");
        assert_eq!(node_direct(host.id()), Some(true));
        fast::set_dozing(false);
        assert!(!fast::take_route_wanted(), "la veille finie, plus rien n'attend");
        drop(c);
        drop(guest);
    }

    /// Une route par ces passeurs, donnée aux deux bouts comme le fait le relais du monde.
    fn give(node: [u8; 32], route: u64, passeurs: &[&minelib_net::fast::Passeur]) {
        use minelib_net::fast::{tunnel_given, TunnelRoute, Via};
        minelib_net::fast::note_passeurs();
        let vias: Vec<Via> = passeurs
            .iter()
            .map(|p| {
                let seal = [p.port() as u8; 32];
                p.set_lot(route, &[(route, seal)]);
                Via {
                    addr: SocketAddr::from(([127, 0, 0, 1], p.port())),
                    seal,
                }
            })
            .collect();
        for host in [true, false] {
            let r = TunnelRoute {
                node,
                route,
                host,
                vias: vias.clone(),
                turn: None,
            };
            tunnel_given(r, false);
        }
    }

    /// (v301) Sans aucun serveur d'iroh et sans adresse directe, un tunnel passe par un
    /// passeur : les deux nœuds ne se joignent que par lui, et le lien compte comme
    /// rapide (pas de débit limité par un tiers).
    #[test]
    fn a_tunnel_goes_through_a_passeur_without_any_server() {
        let _serial = serial();
        let host = Node::start_offline().unwrap();
        host.serve(echo());
        let guest = Node::start_offline().unwrap();
        let (tx, _probes) = std::sync::mpsc::channel();
        let p = minelib_net::fast::Passeur::start(0, tx).unwrap();
        give(host.id(), 0x51, &[&p]);
        assert_eq!(host.passeurs().routes(), 1, "la route de l'hôte est ouverte");
        let t = guest.tunnel(host.id()).unwrap();
        let mut c = std::net::TcpStream::connect(t).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
        assert!(said_back(&mut c, b"salut"));
        // Un gros envoi : bien plus d'un paquet, dans les deux sens.
        for i in 0..2000u32 {
            assert!(said_back(&mut c, &[i as u8; 5]), "envoi {i}");
        }
        assert!(p.carried() > 20, "le passeur a porté {} paquets", p.carried());
        assert_eq!(node_direct(host.id()), Some(true));
        // Les numéros de route ne sont pas des adresses du nœud : iroh, qui publie les
        // adresses d'un nœud, n'en sait rien.
        for n in [&host, &guest] {
            assert_eq!(n.passeurs().routes(), 1);
            let known = n.ep.addr();
            assert!(known
                .addrs
                .iter()
                .all(|a| !matches!(a, iroh::TransportAddr::Custom(_))));
        }
        // Le tunnel fermé, le joueur rend sa route.
        drop(c);
        drop(guest);
    }

    /// (v329) Une route par un serveur TURN, donnée aux deux bouts comme le fait le relais
    /// du monde : chacun reçoit l'adresse relayée de l'autre, après les passeurs `passeurs`.
    fn give_turn(host: &Node, guest: &Node, route: u64, passeurs: &[&minelib_net::fast::Passeur]) {
        use minelib_net::fast::{tunnel_given, TunnelRoute, Via};
        minelib_net::fast::note_passeurs();
        let vias: Vec<Via> = passeurs
            .iter()
            .map(|p| {
                let seal = [p.port() as u8; 32];
                p.set_lot(route, &[(route, seal)]);
                Via {
                    addr: SocketAddr::from(([127, 0, 0, 1], p.port())),
                    seal,
                }
            })
            .collect();
        let relayed = |n: &Node| Via {
            addr: SocketAddr::V4(n.passeurs().turn_addr().expect("adresse relayée")),
            seal: [0x77; 32],
        };
        // (L'hôte reçoit l'adresse du joueur, le joueur celle de l'hôte.)
        for (is_host, other) in [(true, guest), (false, host)] {
            let r = TunnelRoute {
                node: host.id(),
                route,
                host: is_host,
                vias: vias.clone(),
                turn: Some(relayed(other)),
            };
            tunnel_given(r, false);
        }
    }

    /// Deux nœuds sans serveur d'iroh ni adresse directe, chacun avec une adresse chez le
    /// serveur TURN `server`.
    fn turn_nodes(server: &turn::essai::MiniTurn) -> (Node, Node) {
        let host = Node::start_offline().unwrap();
        host.serve(echo());
        let guest = Node::start_offline().unwrap();
        for n in [&host, &guest] {
            n.passeurs().start_turn(vec![server.source()]);
        }
        let t = std::time::Instant::now();
        while host.passeurs().turn_addr().is_none() || guest.passeurs().turn_addr().is_none() {
            assert!(t.elapsed() < Duration::from_secs(10), "pas d'adresse relayée");
            std::thread::sleep(Duration::from_millis(10));
        }
        (host, guest)
    }

    /// (v329) Sans passeur, sans serveur d'iroh et sans adresse directe, un tunnel passe par
    /// un serveur TURN : chaque bout envoie à l'adresse relayée de l'autre par la sienne, et
    /// ni l'un ni l'autre ne voit l'adresse de l'autre. L'adresse relayée de chaque nœud est
    /// tenue prête pour le relais (`fast::turn_addr`), et oubliée quand le nœud s'en va.
    #[test]
    fn a_tunnel_goes_through_a_turn_server_without_any_passeur() {
        let _serial = serial();
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let server = turn::essai::MiniTurn::start(rt.handle());
        let (host, guest) = turn_nodes(&server);
        assert_eq!(
            minelib_net::fast::turn_addr(host.id()),
            host.passeurs().turn_addr()
        );
        give_turn(&host, &guest, 0x61, &[]);
        let t = guest.tunnel(host.id()).unwrap();
        let mut c = std::net::TcpStream::connect(t).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
        assert!(said_back(&mut c, b"salut"));
        for i in 0..1000u32 {
            assert!(said_back(&mut c, &[i as u8; 5]), "envoi {i}");
        }
        assert!(guest.passeurs().via_turn(0x61));
        let (sent, received) = guest.passeurs().turn_counts();
        assert!(sent > 20 && received > 20, "{sent} envoyés, {received} reçus");
        let carried = server.relayed.load(std::sync::atomic::Ordering::Relaxed);
        assert!(carried > 40, "le serveur TURN a porté {carried} paquets");
        assert_eq!(node_direct(host.id()), Some(true));
        drop(c);
        let gid = guest.id();
        drop(guest);
        assert_eq!(minelib_net::fast::turn_addr(gid), None);
    }

    /// (v329) Une route a un passeur et un saut TURN : elle passe par le passeur (le TURN de
    /// Cloudflare compte ce qu'il porte) ; le passeur tombe, le tunnel continue par TURN sans
    /// se refermer.
    #[test]
    fn a_tunnel_moves_to_turn_when_its_passeur_falls() {
        let _serial = serial();
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let server = turn::essai::MiniTurn::start(rt.handle());
        let (host, guest) = turn_nodes(&server);
        let (tx, _probes) = std::sync::mpsc::channel();
        let p = minelib_net::fast::Passeur::start(0, tx).unwrap();
        give_turn(&host, &guest, 0x62, &[&p]);
        let t = guest.tunnel(host.id()).unwrap();
        let mut c = std::net::TcpStream::connect(t).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(25))).unwrap();
        assert!(said_back(&mut c, b"avant"));
        for i in 0..100u32 {
            assert!(said_back(&mut c, &[i as u8; 5]), "envoi {i}");
        }
        assert!(!guest.passeurs().via_turn(0x62), "le passeur d'abord");
        assert!(p.carried() > 20);
        p.clear();
        let fell = std::time::Instant::now();
        assert!(said_back(&mut c, b"apres"), "le tunnel s'est refermé");
        eprintln!("TUNNEL : reprise par TURN en {:?}", fell.elapsed());
        assert!(fell.elapsed() < Duration::from_secs(12));
        assert!(guest.passeurs().via_turn(0x62));
        for i in 0..200u32 {
            assert!(said_back(&mut c, &[i as u8; 5]), "envoi {i}");
        }
        // (v331) Le passeur porte de nouveau la route : on y revient, sans attendre que TURN
        // se taise (il compte dans le quota du mois).
        let seal = [p.port() as u8; 32];
        p.set_lot(0x62, &[(0x62, seal)]);
        let back = std::time::Instant::now();
        while guest.passeurs().via_turn(0x62) || host.passeurs().via_turn(0x62) {
            assert!(
                back.elapsed() < Duration::from_secs(15),
                "toujours par TURN"
            );
            assert!(said_back(&mut c, b"retou"));
            std::thread::sleep(Duration::from_millis(50));
        }
        eprintln!("TUNNEL : retour au passeur en {:?}", back.elapsed());
        let (sent, _) = guest.passeurs().turn_counts();
        for i in 0..200u32 {
            assert!(said_back(&mut c, &[i as u8; 5]), "envoi {i}");
        }
        let (after, _) = guest.passeurs().turn_counts();
        assert!(
            after - sent < 30,
            "{} paquets encore par TURN",
            after - sent
        );
    }

    /// (v331) La voie rapide d'une partie FPS passe par les mêmes clients TURN que les
    /// tunnels des nœuds du jeu : chaque bout envoie par son adresse relayée à celle de
    /// l'autre, et ce qui revient est remis à la voie rapide ou au tunnel selon la route.
    #[test]
    fn the_fast_way_of_a_fps_game_shares_the_turn_clients_with_the_tunnels() {
        use minelib_net::fast::{Endpoint, TurnPair, Way, TO_GUEST, TO_HOST};
        let _serial = serial();
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let server = turn::essai::MiniTurn::start(rt.handle());
        let (host, guest) = turn_nodes(&server);
        give_turn(&host, &guest, 0x64, &[]);
        let t = guest.tunnel(host.id()).unwrap();
        let mut c = std::net::TcpStream::connect(t).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
        assert!(said_back(&mut c, b"tunne"));
        let (ha, ga) = (
            host.passeurs().turn_addr().unwrap(),
            guest.passeurs().turn_addr().unwrap(),
        );
        let (gtx, grx) = std::sync::mpsc::channel::<Vec<u8>>();
        let (htx, hrx) = std::sync::mpsc::channel::<Vec<u8>>();
        let (gtx, htx) = (std::sync::Mutex::new(gtx), std::sync::Mutex::new(htx));
        let g_end = Endpoint::start(
            TO_HOST,
            Box::new(move |_, m| {
                let _ = gtx.lock().unwrap().send(m);
            }),
        )
        .unwrap();
        let h_end = Endpoint::start(
            TO_GUEST,
            Box::new(move |_, m| {
                let _ = htx.lock().unwrap().send(m);
            }),
        )
        .unwrap();
        let way = |peer, mine, other| Way {
            peer,
            route: 0x65,
            key: [9; 32],
            vias: Vec::new(),
            turn: Some(TurnPair { mine, peer: other }),
        };
        g_end.set_ways(vec![way(0, ga, ha)]);
        h_end.set_ways(vec![way(3, ha, ga)]);
        let start = std::time::Instant::now();
        while !(g_end.open(0) && h_end.open(3)) {
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "voie rapide fermée"
            );
            g_end.send_all(0, b"bonjour");
            std::thread::sleep(Duration::from_millis(20));
        }
        while hrx.try_recv().is_ok() {}
        for i in 0..300u16 {
            assert!(h_end.send_one(3, &i.to_le_bytes()));
            assert!(g_end.send_all(0, &i.to_le_bytes()));
            if i % 50 == 0 {
                assert!(said_back(&mut c, &[i as u8; 5]), "le tunnel passe toujours");
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        let count = |rx: &std::sync::mpsc::Receiver<Vec<u8>>| {
            let mut n = 0;
            while rx.recv_timeout(Duration::from_millis(500)).is_ok() {
                n += 1;
            }
            n
        };
        let (to_guest, to_host) = (count(&grx), count(&hrx));
        eprintln!("VOIE RAPIDE PAR TURN : {to_guest}/300 vers l'invité, {to_host}/300 vers l'hôte");
        assert!(to_guest >= 290 && to_host >= 290);
        assert!(h_end.paths(3)[0].rtt_ms.is_some(), "aller-retour mesuré");
        assert!(said_back(&mut c, b"fin!!"));
    }

    /// (v332) Essai réel, sur ton PC : `MINELIB_TURN_ESSAI` donne la source (l'adresse du
    /// Worker, ou `turn:hôte:port utilisateur motdepasse`). La voie rapide d'une partie FPS
    /// entre deux nœuds de ce PC, par le vrai serveur TURN : 256 positions par seconde dans
    /// un sens et 256 entrées dans l'autre pendant 5 secondes, à la taille d'une partie à
    /// 10 ; ce qui arrive, et l'aller-retour mesuré de bout en bout.
    #[test]
    #[ignore = "serveur TURN réel (MINELIB_TURN_ESSAI)"]
    fn a_fps_fast_way_through_a_real_turn_server() {
        use minelib_net::fast::{Endpoint, TurnPair, Way, TO_GUEST, TO_HOST};
        let src = std::env::var("MINELIB_TURN_ESSAI").expect("MINELIB_TURN_ESSAI");
        let sources = turn::parse_sources(&src);
        let _serial = serial();
        let (host, guest) = (
            Node::start_offline().unwrap(),
            Node::start_offline().unwrap(),
        );
        for n in [&host, &guest] {
            n.passeurs().start_turn(sources.clone());
        }
        let t = std::time::Instant::now();
        while host.passeurs().turn_addr().is_none() || guest.passeurs().turn_addr().is_none() {
            assert!(
                t.elapsed() < Duration::from_secs(30),
                "pas d'adresse relayée"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        let (ha, ga) = (
            host.passeurs().turn_addr().unwrap(),
            guest.passeurs().turn_addr().unwrap(),
        );
        eprintln!("TURN : adresses relayées {ha} (hôte) et {ga} (invité)");
        let (gtx, grx) = std::sync::mpsc::channel::<Vec<u8>>();
        let (htx, hrx) = std::sync::mpsc::channel::<Vec<u8>>();
        let (gtx, htx) = (std::sync::Mutex::new(gtx), std::sync::Mutex::new(htx));
        let g_end = Endpoint::start(
            TO_HOST,
            Box::new(move |_, m| {
                let _ = gtx.lock().unwrap().send(m);
            }),
        )
        .unwrap();
        let h_end = Endpoint::start(
            TO_GUEST,
            Box::new(move |_, m| {
                let _ = htx.lock().unwrap().send(m);
            }),
        )
        .unwrap();
        let (route, key): (u64, [u8; 32]) = (rand::random(), rand::random());
        let way = |peer, mine, other| Way {
            peer,
            route,
            key,
            vias: Vec::new(),
            turn: Some(TurnPair { mine, peer: other }),
        };
        g_end.set_ways(vec![way(0, ga, ha)]);
        h_end.set_ways(vec![way(3, ha, ga)]);
        let t = std::time::Instant::now();
        while !(g_end.open(0) && h_end.open(3)) {
            assert!(t.elapsed() < Duration::from_secs(15), "voie rapide fermée");
            g_end.send_all(0, b"bonjour");
            std::thread::sleep(Duration::from_millis(20));
        }
        while grx.try_recv().is_ok() {}
        while hrx.try_recv().is_ok() {}
        // Positions (249 octets, comme pour 9 joueurs vus) et entrées (66 octets).
        let n = 256 * 5;
        let start = std::time::Instant::now();
        for i in 0..n {
            let mut pos = vec![0xEEu8; 249];
            pos[..4].copy_from_slice(&(i as u32).to_le_bytes());
            assert!(h_end.send_one(3, &pos) || !h_end.open(3));
            g_end.send_all(0, &[0xDDu8; 66]);
            let due = start + Duration::from_micros(3906 * (i + 1));
            if let Some(w) = due.checked_duration_since(std::time::Instant::now()) {
                std::thread::sleep(w);
            }
        }
        let count = |rx: &std::sync::mpsc::Receiver<Vec<u8>>| {
            let mut got = 0u64;
            while rx.recv_timeout(Duration::from_millis(500)).is_ok() {
                got += 1;
            }
            got
        };
        let (to_guest, to_host) = (count(&grx), count(&hrx));
        let rtt = h_end.paths(3)[0].rtt_ms.unwrap_or(f32::NAN);
        let (sent, received) = guest.passeurs().turn_counts();
        eprintln!(
            "TURN RÉEL (voie rapide FPS) : {to_guest}/{n} positions vers l'invité, \
             {to_host}/{n} entrées vers l'hôte, aller-retour {rtt:.1} ms ; client de l'invité : \
             {sent} paquets envoyés, {received} reçus"
        );
        assert!(to_guest * 100 >= n * 95 && to_host * 100 >= n * 95);
    }

    /// (v301) Le premier passeur d'un tunnel tombe : le tunnel continue par le second,
    /// sans se refermer.
    #[test]
    fn a_tunnel_moves_to_its_second_passeur_when_the_first_falls() {
        let _serial = serial();
        let host = Node::start_offline().unwrap();
        host.serve(echo());
        let guest = Node::start_offline().unwrap();
        let (tx, _probes) = std::sync::mpsc::channel();
        let first = minelib_net::fast::Passeur::start(0, tx.clone()).unwrap();
        let second = minelib_net::fast::Passeur::start(0, tx).unwrap();
        give(host.id(), 0x52, &[&first, &second]);
        let t = guest.tunnel(host.id()).unwrap();
        let mut c = std::net::TcpStream::connect(t).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(25))).unwrap();
        assert!(said_back(&mut c, b"avant"));
        let at = SocketAddr::from(([127, 0, 0, 1], first.port()));
        assert_eq!(guest.passeurs().via(0x52), Some(at));
        // Le premier ne porte plus rien.
        first.clear();
        let fell = std::time::Instant::now();
        assert!(said_back(&mut c, b"apres"), "le tunnel s'est refermé");
        let took = fell.elapsed();
        eprintln!("TUNNEL : reprise par le second passeur en {took:?}");
        assert!(took < Duration::from_secs(12), "reprise en {took:?}");
        let at = SocketAddr::from(([127, 0, 0, 1], second.port()));
        assert_eq!(guest.passeurs().via(0x52), Some(at));
        for i in 0..200u32 {
            assert!(said_back(&mut c, &[i as u8; 5]), "envoi {i}");
        }
    }

    /// Chemins de la connexion `i` de ce programme vers `remote` : (chemin employé par
    /// un serveur relais d'iroh ?, tous les chemins).
    fn paths_of(remote: [u8; 32], i: usize) -> (Option<bool>, Vec<String>) {
        let t = TUNNELS.lock().unwrap();
        let Some((_, c)) = t.iter().filter(|(n, _)| *n == remote).nth(i) else {
            return (None, Vec::new());
        };
        let paths = c.paths();
        let kinds = paths
            .iter()
            .map(|p| {
                let k = match p.remote_addr() {
                    iroh::TransportAddr::Ip(_) => "direct",
                    iroh::TransportAddr::Custom(_) => "passeur",
                    _ => "serveur d'iroh",
                };
                format!("{k}{}", if p.is_selected() { " (employé)" } else { "" })
            })
            .collect();
        (
            paths.iter().find(|p| p.is_selected()).map(|p| p.is_relay()),
            kinds,
        )
    }

    /// (v304) Sur le vrai réseau (serveurs relais et DNS d'iroh) : un hôte anonyme est
    /// joint par les serveurs d'iroh quand il n'y a pas de route ; avec une route par un
    /// passeur, le tunnel passe par le passeur et garde les serveurs d'iroh en secours ;
    /// le passeur tombé, le tunnel continue par eux sans se refermer.
    #[test]
    #[ignore = "réseau"]
    fn on_the_real_network_a_passeur_route_is_taken_and_iroh_servers_stay_as_a_fallback() {
        let host = Node::start_private().unwrap();
        host.serve(echo());
        // (Le temps que l'hôte soit annoncé chez les serveurs d'iroh.)
        std::thread::sleep(Duration::from_secs(4));
        // 1. Sans route.
        let first = Node::start().unwrap();
        let at = std::time::Instant::now();
        let t = first.tunnel(host.id()).unwrap();
        let mut c1 = std::net::TcpStream::connect(t).unwrap();
        c1.set_read_timeout(Some(Duration::from_secs(40))).unwrap();
        assert!(said_back(&mut c1, b"sans1"), "pas de tunnel par les serveurs d'iroh");
        eprintln!("RESEAU 1 : sans route, ouvert en {:?}", at.elapsed());
        std::thread::sleep(Duration::from_secs(2));
        let (relay, paths) = paths_of(host.id(), 0);
        eprintln!("RESEAU 1 : chemins {paths:?}");
        assert_eq!(relay, Some(true), "un hôte anonyme joint autrement que par un serveur");
        let at = std::time::Instant::now();
        for i in 0..200u32 {
            assert!(said_back(&mut c1, &[i as u8; 5]));
        }
        eprintln!("RESEAU 1 : 200 allers-retours en {:?}", at.elapsed());
        // 2. Avec une route par un passeur.
        let (tx, _probes) = std::sync::mpsc::channel();
        let p = minelib_net::fast::Passeur::start(0, tx).unwrap();
        give(host.id(), 0x91, &[&p]);
        let second = Node::start().unwrap();
        let at = std::time::Instant::now();
        let t = second.tunnel(host.id()).unwrap();
        let mut c2 = std::net::TcpStream::connect(t).unwrap();
        c2.set_read_timeout(Some(Duration::from_secs(40))).unwrap();
        assert!(said_back(&mut c2, b"avec2"), "pas de tunnel avec une route");
        eprintln!("RESEAU 2 : avec une route, ouvert en {:?}", at.elapsed());
        let mut seen = (None, Vec::new());
        for _ in 0..20 {
            std::thread::sleep(Duration::from_millis(500));
            assert!(said_back(&mut c2, b"encor"));
            seen = paths_of(host.id(), 1);
            if seen.0 == Some(false) && seen.1.len() >= 2 {
                break;
            }
        }
        eprintln!("RESEAU 2 : chemins {:?}", seen.1);
        assert_eq!(seen.0, Some(false), "le tunnel n'emploie pas le passeur");
        assert!(seen.1.iter().any(|k| k.starts_with("passeur (employé)")));
        let before = p.carried();
        let at = std::time::Instant::now();
        for i in 0..200u32 {
            assert!(said_back(&mut c2, &[i as u8; 5]));
        }
        eprintln!(
            "RESEAU 2 : 200 allers-retours en {:?}, {} paquets par le passeur",
            at.elapsed(),
            p.carried() - before
        );
        assert!(p.carried() - before >= 200);
        let backup = seen.1.iter().any(|k| k.starts_with("serveur d'iroh"));
        eprintln!("RESEAU 2 : serveurs d'iroh gardés en secours : {backup}");
        // 3. Le passeur tombe.
        p.clear();
        let at = std::time::Instant::now();
        assert!(said_back(&mut c2, b"apres"), "le tunnel s'est refermé");
        eprintln!("RESEAU 3 : passeur tombé, reprise en {:?}", at.elapsed());
        std::thread::sleep(Duration::from_secs(3));
        assert!(said_back(&mut c2, b"suite"));
        let (relay, paths) = paths_of(host.id(), 1);
        eprintln!("RESEAU 3 : chemins {paths:?}");
        assert_eq!(relay, Some(true));
    }

    /// (v317) Sur le vrai réseau : débit d'un transfert par les serveurs relais d'iroh, les
    /// deux bouts sans transport IP (le cas de tout lien sans passeurs depuis la v313) :
    /// 2 Mo envoyés à un relais qui les compte et répond quand il a tout. C'est ce que
    /// coûte l'envoi d'un monde ou d'une carte par ce chemin.
    /// `cargo test -p minelib-p2p --lib -- --ignored --nocapture one_way`
    #[test]
    #[ignore = "réseau"]
    fn on_the_real_network_one_way_two_megabytes() {
        use std::io::{Read, Write};
        const TOTAL: usize = 2_000_000;
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let local = server.local_addr().unwrap();
        std::thread::spawn(move || {
            while let Ok((mut s, _)) = server.accept() {
                std::thread::spawn(move || {
                    let mut head = [0u8; 5];
                    if s.read_exact(&mut head).is_err() {
                        return;
                    }
                    let mut who = vec![0u8; head[4] as usize];
                    let _ = s.read_exact(&mut who);
                    let mut buf = vec![0u8; 64 * 1024];
                    let mut got = 0usize;
                    while got < TOTAL {
                        match s.read(&mut buf) {
                            Ok(0) | Err(_) => return,
                            Ok(n) => got += n,
                        }
                    }
                    let _ = s.write_all(b"fini!");
                });
            }
        });
        let host = Node::start_private().unwrap();
        host.serve(local);
        std::thread::sleep(Duration::from_secs(4));
        let guest = Node::start_private().unwrap();
        let t = guest.tunnel(host.id()).unwrap();
        let mut c = std::net::TcpStream::connect(t).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(150))).unwrap();
        let chunk = vec![7u8; 10_000];
        let t0 = std::time::Instant::now();
        let mut sent = 0;
        while sent < TOTAL {
            c.write_all(&chunk).unwrap();
            sent += chunk.len();
        }
        let mut done = [0u8; 5];
        c.read_exact(&mut done).unwrap();
        let took = t0.elapsed();
        println!(
            "RESEAU UN SENS : {sent} octets reçus par l'hôte en {:.1} s ({:.0} Ko/s)",
            took.as_secs_f32(),
            sent as f32 / 1024.0 / took.as_secs_f32()
        );
        assert_eq!(&done, b"fini!");
    }

    /// (v308) Sur le vrai réseau : 256 messages par seconde et par joueur pendant 20 s
    /// vers un hôte anonyme, par les serveurs relais d'iroh (le cas sans passeur). Combien
    /// reviennent, en combien de temps : c'est ce que ces serveurs laissent passer d'un
    /// jeu. `MINELIB_ESSAI_JOUEURS` : nombre de joueurs en même temps (1 sans la variable,
    /// 4 au plus : ces serveurs sont partagés par tout le monde, on ne les charge pas pour
    /// voir quand ils cèdent), chacun avec son propre nœud ; l'hôte, lui, n'a qu'une
    /// connexion à son serveur.
    /// (v336) `MINELIB_ESSAI_PAR_S` (16 à 512, 256 sans la variable) et
    /// `MINELIB_ESSAI_OCTETS` (5 à 1200, 5 sans la variable) : la cadence et la taille des
    /// messages, pour faire passer ce qu'envoie l'hôte d'une partie FPS (128 par seconde de
    /// 260 octets en moyenne, en-têtes compris).
    #[test]
    #[ignore = "réseau"]
    fn on_the_real_network_256_messages_a_second_through_iroh_servers() {
        use std::io::{Read, Write};
        let env = |k: &str, d: usize, lo: usize, hi: usize| {
            std::env::var(k)
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(d)
                .clamp(lo, hi)
        };
        let players = env("MINELIB_ESSAI_JOUEURS", 1, 1, 4);
        let rate = env("MINELIB_ESSAI_PAR_S", 256, 16, 512) as u32;
        let size = env("MINELIB_ESSAI_OCTETS", 5, 5, 1200);
        let host = Node::start_private().unwrap();
        host.serve(echo());
        std::thread::sleep(Duration::from_secs(4));
        let n_msgs: u32 = rate * 20;
        let remote = host.id();
        let runs: Vec<_> = (0..players)
            .map(|_| {
                std::thread::spawn(move || {
                    // (v313) `MINELIB_ESSAI_ANONYME` : le joueur aussi est sans transport
                    // IP (le cas de tous les jeux depuis la v313).
                    let guest = if std::env::var("MINELIB_ESSAI_ANONYME").is_ok() {
                        Node::start_private().unwrap()
                    } else {
                        Node::start().unwrap()
                    };
                    let t = guest.tunnel(remote).unwrap();
                    let mut c = std::net::TcpStream::connect(t).unwrap();
                    c.set_nodelay(true).unwrap();
                    c.set_read_timeout(Some(Duration::from_secs(60))).unwrap();
                    assert!(said_back(&mut c, b"debut"));
                    let mut back = c.try_clone().unwrap();
                    let started = std::time::Instant::now();
                    let reader = std::thread::spawn(move || {
                        // Chaque message porte son numéro : son temps d'aller-retour.
                        let mut times = Vec::with_capacity(n_msgs as usize);
                        let mut b = vec![0u8; size];
                        for _ in 0..n_msgs {
                            if back.read_exact(&mut b).is_err() {
                                break;
                            }
                            let n = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
                            let sent = Duration::from_secs_f64(f64::from(n) / f64::from(rate));
                            times.push(started.elapsed().saturating_sub(sent));
                        }
                        times
                    });
                    for n in 0..n_msgs {
                        let due = Duration::from_secs_f64(f64::from(n) / f64::from(rate));
                        if let Some(wait) = due.checked_sub(started.elapsed()) {
                            std::thread::sleep(wait);
                        }
                        let mut m = vec![0u8; size];
                        m[..4].copy_from_slice(&n.to_le_bytes());
                        if c.write_all(&m).is_err() {
                            break;
                        }
                    }
                    let times = reader.join().unwrap();
                    drop(guest);
                    (times, started.elapsed())
                })
            })
            .collect();
        let mut times = Vec::new();
        let mut longest = Duration::ZERO;
        for r in runs {
            let (t, took) = r.join().unwrap();
            times.extend(t);
            longest = longest.max(took);
        }
        let got = times.len();
        times.sort();
        let at = |q: f64| times.get(((got as f64 - 1.0) * q) as usize).copied().unwrap_or_default();
        eprintln!(
            "RESEAU {rate}/s de {size} o : {players} joueur(s), {} messages envoyés, {got} revenus, \
             en {longest:?} au plus ; aller-retour médian {:?}, 95 % sous {:?}, pire {:?}",
            n_msgs as usize * players,
            at(0.5),
            at(0.95),
            at(1.0)
        );
        let (relay, paths) = paths_of(remote, 0);
        eprintln!("RESEAU : chemins {paths:?}");
        assert_eq!(relay, Some(true));
        assert_eq!(
            got,
            n_msgs as usize * players,
            "des messages ne sont pas revenus"
        );
    }

    /// Deux nœuds iroh sur cette machine : un relais TCP local joint par le tunnel.
    #[test]
    #[ignore = "réseau"]
    fn a_tunnel_reaches_a_relay_behind_another_node() {
        use std::io::{Read, Write};
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let local = server.local_addr().unwrap();
        let (who_tx, who_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let (mut s, _) = server.accept().unwrap();
            // Préambule : « MLPX », longueur, qui est au bout.
            let mut head = [0u8; 5];
            s.read_exact(&mut head).unwrap();
            assert_eq!(&head[..4], PREAMBLE);
            let mut who = vec![0u8; head[4] as usize];
            s.read_exact(&mut who).unwrap();
            who_tx.send(String::from_utf8(who).unwrap()).unwrap();
            let mut b = [0u8; 5];
            s.read_exact(&mut b).unwrap();
            s.write_all(&b).unwrap();
        });
        let a = Node::start().unwrap();
        a.serve(local);
        let b = Node::start().unwrap();
        let t = b.tunnel_to(a.ep.addr(), a.id()).unwrap();
        let mut c = std::net::TcpStream::connect(t).unwrap();
        c.write_all(b"salut").unwrap();
        let mut back = [0u8; 5];
        c.read_exact(&mut back).unwrap();
        assert_eq!(&back, b"salut");
        let who = who_rx.recv().unwrap();
        assert!(
            !who.is_empty() && who != "127.0.0.1",
            "vraie adresse : {who}"
        );
    }
}
