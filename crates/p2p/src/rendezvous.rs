//! (v326) Rendez-vous sans montrer son adresse IP aux nœuds du DHT.
//!
//! La liste des relais Minelib en ligne passait par le DHT de BitTorrent, que le jeu
//! interrogeait lui-même à chaque lancement, et où un relais hébergé par un joueur
//! s'inscrivait toutes les 10 minutes : n'importe qui pouvant y faire tourner des nœuds
//! (un joueur aussi) voyait l'adresse IP de chaque jeu qui cherchait la liste et de chaque
//! joueur qui hébergeait un relais. Maintenant, la liste passe par des **relais pkarr** :
//! des serveurs HTTPS qui gardent des enregistrements signés (et les portent dans le DHT).
//! Le premier est celui de l'équipe d'iroh, qui voit déjà les adresses de ceux qui passent
//! par ses serveurs relais ; le second, celui de Pubky (les relais par défaut de pkarr,
//! 1000 requêtes par minute et par adresse). Chaque relais Minelib écrit la liste chez les
//! deux, et un jeu prend la plus récente des deux : ils n'ont pas besoin de se parler.
//!
//! La liste est sous une clé connue de tous (comme avant : n'importe quel relais peut s'y
//! inscrire, et un jeu vérifie chaque relais en s'y connectant, avec sa clé Noise), au
//! format de pkarr (celui des nœuds iroh) : un enregistrement TXT `_minelib` par relais.

use std::sync::Mutex;
use std::time::Duration;

use anyhow::{anyhow, Result};
use iroh_dns::pkarr::SignedPacket;
use sha2::{Digest, Sha256};

use crate::Ad;

/// Relais pkarr, dans l'ordre où on les interroge.
pub const RELAYS: &[&str] = &["https://dns.iroh.link/pkarr", "https://pkarr.pubky.app"];
/// Nom des enregistrements (un par relais Minelib).
const NAME: &str = "_minelib";
const TTL: u32 = 1800;
/// Nom d'un relais gardé dans la liste (la liste entière tient en 1000 octets).
const NAME_MAX: usize = 20;
/// Attente d'un relais pkarr, au plus.
const WAIT: Duration = Duration::from_secs(5);

/// (Essais) relais pkarr à la place des vrais.
static OVERRIDE: Mutex<Option<Vec<String>>> = Mutex::new(None);

#[doc(hidden)]
pub fn set_relays(urls: Option<Vec<String>>) {
    *OVERRIDE.lock().unwrap() = urls;
}

fn relays() -> Vec<String> {
    OVERRIDE
        .lock()
        .unwrap()
        .clone()
        .unwrap_or_else(|| RELAYS.iter().map(|s| s.to_string()).collect())
}

/// Clé de la liste : connue de tous.
fn key() -> iroh::SecretKey {
    let seed: [u8; 32] = Sha256::digest(b"minelib/rendez-vous/v2").into();
    iroh::SecretKey::from_bytes(&seed)
}

fn http() -> ureq::Agent {
    use ureq::tls::{RootCerts, TlsConfig};
    ureq::Agent::config_builder()
        .tls_config(
            TlsConfig::builder()
                .root_certs(RootCerts::PlatformVerifier)
                .build(),
        )
        .timeout_global(Some(WAIT))
        .http_status_as_error(false)
        .build()
        .into()
}

fn url(relay: &str) -> String {
    format!(
        "{}/{}",
        relay.trim_end_matches('/'),
        key().public().to_z32()
    )
}

/// Un relais par enregistrement : son annonce (postcard), en base64.
fn encode(ad: &Ad) -> String {
    let mut ad = ad.clone();
    ad.name = ad.name.chars().take(NAME_MAX).collect();
    data_encoding::BASE64URL_NOPAD.encode(&postcard::to_allocvec(&ad).unwrap_or_default())
}

fn decode(txt: &str) -> Option<Ad> {
    let bytes = data_encoding::BASE64URL_NOPAD.decode(txt.as_bytes()).ok()?;
    postcard::from_bytes(&bytes).ok()
}

/// L'enregistrement signé de cette liste (les plus anciens relais d'abord : s'il faut en
/// retirer pour tenir en 1000 octets, ce sont les plus récents qui partent).
pub fn packet(list: &[Ad]) -> Result<SignedPacket> {
    let mut list = list.to_vec();
    list.sort_by_key(|a| a.since);
    loop {
        let values: Vec<String> = list.iter().map(encode).collect();
        match SignedPacket::from_txt_strings(&key(), NAME, values, TTL) {
            Ok(p) => return Ok(p),
            Err(e) if list.len() <= 1 => return Err(anyhow!("liste des relais : {e}")),
            Err(_) => {
                list.pop();
            }
        }
    }
}

/// Les relais d'un enregistrement reçu, vérifié (signé par la clé de la liste), encore en
/// ligne, du plus ancien au plus récent.
pub fn ads_of(payload: &[u8]) -> Result<(u64, Vec<Ad>)> {
    let p = SignedPacket::from_relay_payload(&key().public(), payload)
        .map_err(|e| anyhow!("enregistrement refusé : {e}"))?;
    let mut list: Vec<Ad> = p
        .txt_records(NAME)
        .iter()
        .filter_map(|t| decode(t))
        .collect();
    let t = crate::now();
    list.retain(|a| a.seen + crate::STALE_S > t);
    list.sort_by_key(|a| a.since);
    Ok((p.timestamp().as_micros(), list))
}

/// Lit la liste chez les relais pkarr : la plus récente de celles qu'ils donnent.
pub fn fetch() -> Result<Vec<Ad>> {
    let agent = http();
    let mut best: Option<(u64, Vec<Ad>)> = None;
    let mut errors = Vec::new();
    for relay in relays() {
        let got = agent
            .get(&url(&relay))
            .call()
            .map_err(|e| anyhow!("{e}"))
            .and_then(|mut r| match r.status().as_u16() {
                200 => r.body_mut().read_to_vec().map_err(|e| anyhow!("{e}")),
                // (Personne ne s'y est encore inscrit.)
                404 => Ok(Vec::new()),
                s => Err(anyhow!("réponse {s}")),
            });
        match got {
            Ok(b) if b.is_empty() => {
                best.get_or_insert((0, Vec::new()));
            }
            Ok(b) => match ads_of(&b) {
                Ok(found) => {
                    if best.as_ref().is_none_or(|(t, _)| found.0 > *t) {
                        best = Some(found);
                    }
                }
                Err(e) => errors.push(format!("{relay} : {e:#}")),
            },
            Err(e) => errors.push(format!("{relay} : {e:#}")),
        }
    }
    best.map(|b| b.1)
        .ok_or_else(|| anyhow!("aucun relais pkarr ne répond ({})", errors.join(" ; ")))
}

/// Écrit la liste chez les relais pkarr ; réussi si l'un d'eux l'a prise.
pub fn put(list: &[Ad]) -> Result<()> {
    let p = packet(list)?;
    let body = p.to_relay_payload();
    let agent = http();
    let mut errors = Vec::new();
    let mut done = false;
    for relay in relays() {
        match agent.put(&url(&relay)).send(&body[..]) {
            Ok(r) if r.status().is_success() => done = true,
            Ok(r) => errors.push(format!("{relay} : réponse {}", r.status().as_u16())),
            Err(e) => errors.push(format!("{relay} : {e}")),
        }
    }
    if done {
        Ok(())
    } else {
        Err(anyhow!(
            "aucun relais pkarr n'a pris la liste ({})",
            errors.join(" ; ")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::sync::Arc;

    fn ad(n: u8, name: &str, since: u64) -> Ad {
        Ad {
            node: [n; 32],
            noise: [n + 100; 32],
            name: name.into(),
            since,
            seen: crate::now(),
        }
    }

    /// Enregistrements d'un faux relais, par chemin.
    type Store = Arc<Mutex<HashMap<String, Vec<u8>>>>;

    /// Un faux relais pkarr (HTTP) : GET et PUT d'un enregistrement par clé, comme ceux de
    /// l'équipe d'iroh et de Pubky ; il compte les requêtes.
    fn fake_relay() -> (String, Store, Arc<Mutex<u32>>) {
        let store: Store = Arc::default();
        let hits: Arc<Mutex<u32>> = Arc::default();
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/pkarr", l.local_addr().unwrap());
        let (s2, h2) = (store.clone(), hits.clone());
        std::thread::spawn(move || {
            for c in l.incoming().flatten() {
                *h2.lock().unwrap() += 1;
                let mut r = BufReader::new(c.try_clone().unwrap());
                let mut line = String::new();
                r.read_line(&mut line).unwrap();
                let mut parts = line.split_whitespace();
                let (method, path) = (
                    parts.next().unwrap().to_string(),
                    parts.next().unwrap().to_string(),
                );
                let mut len = 0usize;
                loop {
                    let mut h = String::new();
                    r.read_line(&mut h).unwrap();
                    if h.trim().is_empty() {
                        break;
                    }
                    if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
                        len = v.trim().parse().unwrap();
                    }
                }
                let mut body = vec![0u8; len];
                r.read_exact(&mut body).unwrap();
                let mut c = c;
                let reply = |c: &mut std::net::TcpStream, status: &str, body: &[u8]| {
                    let _ = write!(
                        c,
                        "HTTP/1.1 {status}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = c.write_all(body);
                };
                match method.as_str() {
                    "PUT" => {
                        s2.lock().unwrap().insert(path, body);
                        reply(&mut c, "204 No Content", b"");
                    }
                    _ => match s2.lock().unwrap().get(&path).cloned() {
                        Some(b) => reply(&mut c, "200 OK", &b),
                        None => reply(&mut c, "404 Not Found", b""),
                    },
                }
            }
        });
        (url, store, hits)
    }

    /// La liste fait l'aller-retour par les relais pkarr, signée par la clé de la liste :
    /// un enregistrement falsifié est refusé ; sans enregistrement, la liste est vide ; un
    /// relais qui ne répond pas n'empêche pas l'autre de servir.
    #[test]
    fn the_relay_list_goes_through_pkarr_relays_signed() {
        let (a, store_a, _) = fake_relay();
        let (b, _, hits_b) = fake_relay();
        set_relays(Some(vec![
            "http://127.0.0.1:1/pkarr".into(),
            a.clone(),
            b.clone(),
        ]));
        assert!(fetch().unwrap().is_empty(), "personne encore : liste vide");
        let list = vec![ad(1, "Minelib", 10), ad(2, "Relais d'un joueur", 20)];
        put(&list).unwrap();
        let got = fetch().unwrap();
        assert_eq!(
            got.iter().map(|x| x.node[0]).collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(got[1].name, "Relais d'un joueur");
        assert!(*hits_b.lock().unwrap() >= 2, "les deux relais servent");
        // Un enregistrement qui n'est pas signé par la clé de la liste : refusé.
        let path = store_a.lock().unwrap().keys().next().unwrap().clone();
        let mut forged = store_a.lock().unwrap()[&path].clone();
        let last = forged.len() - 1;
        forged[last] ^= 1;
        assert!(ads_of(&forged).is_err());
        set_relays(None);
    }

    /// Six relais aux noms longs tiennent dans un enregistrement (1000 octets) ; au-delà,
    /// les plus récents sont retirés, jamais les plus anciens.
    #[test]
    fn the_list_fits_one_record_and_keeps_the_oldest() {
        let long = "Un nom de relais vraiment très très long";
        let six: Vec<Ad> = (0..6).map(|i| ad(i, long, 100 + i as u64)).collect();
        let p = packet(&six).unwrap();
        let (_, back) = ads_of(&p.to_relay_payload()).unwrap();
        assert_eq!(back.len(), 6);
        assert!(back.iter().all(|a| a.name.chars().count() == NAME_MAX));
        let many: Vec<Ad> = (0..12).map(|i| ad(i, long, 100 + i as u64)).collect();
        let (_, back) = ads_of(&packet(&many).unwrap().to_relay_payload()).unwrap();
        assert!(back.len() >= 6 && back.len() < 12);
        assert_eq!(back[0].node[0], 0, "le plus ancien reste");
    }
}
