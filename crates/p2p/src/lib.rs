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

use anyhow::{anyhow, Context, Result};
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

/// Lit la liste des relais dans le DHT (quelques secondes).
#[allow(deprecated)]
pub fn find_relays() -> Result<Vec<Ad>> {
    let dht = mainline::Dht::client().context("DHT")?;
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
                let dht = mainline::Dht::client()?;
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
            let hex: String = id.as_bytes()[..8].iter().map(|b| format!("{b:02x}")).collect();
            format!("iroh:{hex}")
        }
    }
}

/// Nœud iroh de ce jeu (un par processus).
pub struct Node {
    rt: tokio::runtime::Runtime,
    ep: iroh::Endpoint,
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
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("iroh")
            .enable_all()
            .build()?;
        let ep = rt
            .block_on(async {
                iroh::Endpoint::builder(iroh::endpoint::presets::N0)
                    .alpns(vec![ALPN.to_vec()])
                    .bind()
                    .await
            })
            .map_err(|e| anyhow!("iroh : {e}"))?;
        Ok(Self { rt, ep })
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
        self.tunnel_to(id.into())
    }

    fn tunnel_to(&self, id: iroh::EndpointAddr) -> Result<SocketAddr> {
        let listener = self
            .rt
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))?;
        let addr = listener.local_addr()?;
        let ep = self.ep.clone();
        self.rt.spawn(async move {
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
        let t = b.tunnel_to(a.ep.addr()).unwrap();
        let mut c = std::net::TcpStream::connect(t).unwrap();
        c.write_all(b"salut").unwrap();
        let mut back = [0u8; 5];
        c.read_exact(&mut back).unwrap();
        assert_eq!(&back, b"salut");
        let who = who_rx.recv().unwrap();
        assert!(!who.is_empty() && who != "127.0.0.1", "vraie adresse : {who}");
    }
}
