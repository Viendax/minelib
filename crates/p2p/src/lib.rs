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

/// Lit la liste des relais dans le DHT (quelques secondes).
#[allow(deprecated)]
pub fn find_relays() -> Result<Vec<Ad>> {
    let dht = dht()?;
    let key = shared_key();
    let item = dht.get_mutable_most_recent(key.verifying_key().as_bytes(), Some(SALT));
    Ok(item.map(|i| decode_ads(i.value())).unwrap_or_default())
}

/// Inscrit ce relais dans le DHT, puis toutes les 10 minutes (fil de fond).
pub fn publish(me: Ad) {
    let _ = std::thread::Builder::new()
        .name("rendez-vous DHT".into())
        .spawn(move || {
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
                match run(&me) {
                    Ok(()) => log::info!("relais inscrit dans le DHT"),
                    Err(e) => log::warn!("inscription dans le DHT : {e:#}"),
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
/// serveurs relais d'iroh, et celui dont la box est ouverte porte lui-même ses tunnels
/// quand il n'y a pas d'autre passeur : aussi vite qu'en direct, et les joueurs n'y
/// voient qu'une adresse de passeur comme une autre. La v297 le mettait en direct de
/// lui-même dans ce cas.
pub fn host_direct(_open_box: bool) -> bool {
    HOSTS_DIRECT.load(std::sync::atomic::Ordering::Relaxed) == 1
}

/// (v285) Connexions iroh ouvertes par les tunnels de ce jeu, par nœud distant.
static TUNNELS: std::sync::Mutex<Vec<([u8; 32], iroh::endpoint::Connection)>> =
    std::sync::Mutex::new(Vec::new());

fn remember_tunnel(remote: [u8; 32], conn: &iroh::endpoint::Connection) {
    let mut t = TUNNELS.lock().unwrap();
    t.retain(|(_, c)| c.close_reason().is_none());
    t.push((remote, conn.clone()));
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
                } else {
                    iroh::Endpoint::builder(iroh::endpoint::presets::N0)
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

    /// Une route par ces passeurs, donnée aux deux bouts comme le fait le relais du monde.
    fn give(node: [u8; 32], route: u64, passeurs: &[&minelib_net::fast::Passeur]) {
        use minelib_net::fast::{tunnel_given, TunnelRoute, Via};
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
            };
            tunnel_given(r, false);
        }
    }

    /// (v301) Sans aucun serveur d'iroh et sans adresse directe, un tunnel passe par un
    /// passeur : les deux nœuds ne se joignent que par lui, et le lien compte comme
    /// rapide (pas de débit limité par un tiers).
    #[test]
    fn a_tunnel_goes_through_a_passeur_without_any_server() {
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

    /// (v301) Le premier passeur d'un tunnel tombe : le tunnel continue par le second,
    /// sans se refermer.
    #[test]
    fn a_tunnel_moves_to_its_second_passeur_when_the_first_falls() {
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
