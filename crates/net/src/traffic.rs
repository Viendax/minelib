//! (v347) Ce que chaque lien par le tunnel envoie, par genre de message.
//!
//! Le tunnel iroh (`minelib_p2p`) garde ce qui part par les serveurs relais de l'équipe
//! d'iroh sous la limite connue de leur débit. Quand cette limite retient quelque chose, il
//! écrit au journal ce que ces liens envoyaient, d'après ce compte : de quoi savoir quoi
//! alléger. Une connexion TCP au tunnel (127.0.0.1) se reconnaît des deux côtés à sa paire
//! d'adresses. Chaque côté compte chez lui, sans verrou partagé à chaque message, et dépose
//! sa dernière seconde ici.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Noms des variantes de `Request` et `Response`, dans l'ordre (tirés de `proto.rs` à la
/// compilation).
pub mod kinds {
    include!(concat!(env!("OUT_DIR"), "/genres.rs"));
}

/// Une connexion TCP par le tunnel : ses deux adresses, la plus petite d'abord.
pub type Key = (SocketAddr, SocketAddr);

/// La clé d'une connexion TCP (`a`, `b` : ses deux bouts), seulement par le tunnel (les deux
/// sur 127.0.0.1).
pub fn key(a: SocketAddr, b: SocketAddr) -> Option<Key> {
    (a.ip().is_loopback() && b.ip().is_loopback()).then_some(if a <= b { (a, b) } else { (b, a) })
}

/// Dernière seconde comptée de chaque lien : quand, et (genre, octets par seconde, messages
/// par seconde).
type Seen = (Instant, Vec<(&'static str, f32, f32)>);

static LAST: Mutex<Option<(Instant, HashMap<Key, Seen>)>> = Mutex::new(None);

/// Ce qui est plus vieux n'est plus montré (et finit par être oublié).
const KEPT: Duration = Duration::from_secs(15);

/// Le compte d'un lien, tenu par celui qui envoie.
pub struct Meter {
    key: Option<Key>,
    names: &'static [&'static str],
    since: Instant,
    /// Par numéro de variante : octets écrits, messages.
    counts: Vec<(u64, u32)>,
}

impl Meter {
    /// Compte d'une connexion TCP (`local`, `peer`) : seulement par le tunnel. `responses` :
    /// ce côté envoie des `Response` (le relais), sinon des `Request` (le jeu).
    pub fn new(local: Option<SocketAddr>, peer: Option<SocketAddr>, responses: bool) -> Self {
        Self {
            key: local.zip(peer).and_then(|(a, b)| key(a, b)),
            names: if responses {
                kinds::RESPONSE_KINDS
            } else {
                kinds::REQUEST_KINDS
            },
            since: Instant::now(),
            counts: Vec::new(),
        }
    }

    /// Un message parti : `plain` sérialisé (postcard : il commence par le numéro de sa
    /// variante), `wire` octets écrits pour lui.
    pub fn sent(&mut self, plain: &[u8], wire: usize) {
        if self.key.is_none() {
            return;
        }
        let i = (variant(plain) as usize).min(self.names.len());
        if self.counts.len() <= i {
            self.counts.resize(i + 1, (0, 0));
        }
        self.counts[i].0 += wire as u64;
        self.counts[i].1 += 1;
        let secs = self.since.elapsed();
        if secs >= Duration::from_secs(1) {
            self.publish(secs);
        }
    }

    fn publish(&mut self, secs: Duration) {
        let Some(key) = self.key else {
            return;
        };
        let s = secs.as_secs_f32();
        let names = self.names;
        let kinds: Vec<(&'static str, f32, f32)> = self
            .counts
            .iter_mut()
            .enumerate()
            .filter(|(_, c)| c.1 > 0)
            .map(|(i, c)| {
                let seen = (
                    names.get(i).copied().unwrap_or("?"),
                    c.0 as f32 / s,
                    c.1 as f32 / s,
                );
                *c = (0, 0);
                seen
            })
            .collect();
        let now = Instant::now();
        self.since = now;
        let mut g = LAST.lock().unwrap();
        let (pruned, m) = g.get_or_insert_with(|| (now, HashMap::new()));
        // (Liens fermés : oubliés de temps en temps, pas à chaque fois.)
        if now.duration_since(*pruned) >= KEPT {
            m.retain(|_, (t, _)| now.duration_since(*t) < KEPT);
            *pruned = now;
        }
        m.insert(key, (now, kinds));
    }
}

/// Ce que ces liens envoyaient (leur dernière seconde comptée, il y a moins de 15 s), en tout
/// par genre, du plus lourd au plus léger : (genre, octets par seconde, messages par seconde).
pub fn recent(keys: &[Key]) -> Vec<(&'static str, f32, f32)> {
    let g = LAST.lock().unwrap();
    let Some((_, m)) = g.as_ref() else {
        return Vec::new();
    };
    let mut sum: Vec<(&'static str, f32, f32)> = Vec::new();
    for k in keys {
        let Some((t, kinds)) = m.get(k) else {
            continue;
        };
        if t.elapsed() >= KEPT {
            continue;
        }
        for &(name, bytes, msgs) in kinds {
            match sum.iter_mut().find(|s| s.0 == name) {
                Some(s) => {
                    s.1 += bytes;
                    s.2 += msgs;
                }
                None => sum.push((name, bytes, msgs)),
            }
        }
    }
    sum.sort_by(|a, b| b.1.total_cmp(&a.1));
    sum
}

/// Le numéro de variante au début d'un message postcard (entier variable, 5 octets au plus).
fn variant(plain: &[u8]) -> u32 {
    let mut v = 0u32;
    for (i, b) in plain.iter().take(5).enumerate() {
        v |= ((b & 0x7f) as u32) << (7 * i);
        if b & 0x80 == 0 {
            break;
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::{Request, Response};

    fn name_of(names: &'static [&'static str], plain: &[u8]) -> &'static str {
        names[variant(plain) as usize]
    }

    /// Les noms tirés de `proto.rs` sont bien ceux des numéros que postcard écrit, jusqu'aux
    /// dernières variantes.
    #[test]
    fn message_kinds_are_named_like_the_protocol() {
        let req = |r: Request| postcard::to_allocvec(&r).unwrap();
        let resp = |r: Response| postcard::to_allocvec(&r).unwrap();
        let q = kinds::REQUEST_KINDS;
        let s = kinds::RESPONSE_KINDS;
        assert_eq!(name_of(q, &req(Request::Maps)), "Maps");
        assert_eq!(
            name_of(q, &req(Request::SkinGet { pseudo: "x".into() })),
            "SkinGet"
        );
        assert_eq!(q.last(), Some(&"AccountTotp"));
        assert_eq!(
            name_of(
                q,
                &req(Request::AccountTotp {
                    totp: vec![],
                    code: 0,
                    devices: vec![],
                })
            ),
            "AccountTotp"
        );
        assert_eq!(
            name_of(
                s,
                &resp(Response::Skin {
                    pseudo: "x".into(),
                    png: vec![],
                })
            ),
            "Skin"
        );
        assert_eq!(s.last(), Some(&"WorldsForgotten"));
        assert_eq!(
            name_of(s, &resp(Response::WorldsForgotten { worlds: vec![] })),
            "WorldsForgotten"
        );
        assert_eq!(
            name_of(
                s,
                &resp(Response::Moved {
                    index: 0,
                    device: [0; 32],
                })
            ),
            "Moved"
        );
        assert_eq!(variant(&[0xac, 0x02]), 300);
    }

    /// Un lien par le tunnel dépose chaque seconde ce qu'il a envoyé, par genre ; un lien
    /// direct (autre machine) ne compte rien.
    #[test]
    fn a_tunnel_link_tells_what_it_sends_by_kind() {
        let a: SocketAddr = "127.0.0.1:40001".parse().unwrap();
        let b: SocketAddr = "127.0.0.1:40002".parse().unwrap();
        let mut m = Meter::new(Some(b), Some(a), false);
        let skin = postcard::to_allocvec(&Request::SkinGet { pseudo: "x".into() }).unwrap();
        let maps = postcard::to_allocvec(&Request::Maps).unwrap();
        for _ in 0..10 {
            m.sent(&skin, 100);
        }
        m.sent(&maps, 30);
        m.since -= Duration::from_secs(2);
        m.sent(&maps, 30);
        let seen = recent(&[key(a, b).unwrap()]);
        assert_eq!(seen[0].0, "SkinGet");
        assert!((seen[0].1 - 500.0).abs() < 5.0, "{seen:?}");
        assert!((seen[0].2 - 5.0).abs() < 0.1, "{seen:?}");
        assert_eq!(seen[1].0, "Maps");
        assert!((seen[1].2 - 1.0).abs() < 0.1, "{seen:?}");
        // Les deux bouts ont la même clé.
        assert_eq!(key(a, b), key(b, a));
        let far: SocketAddr = "203.0.113.5:9000".parse().unwrap();
        assert!(key(a, far).is_none());
        let mut direct = Meter::new(Some(a), Some(far), false);
        direct.sent(&skin, 100);
        assert!(direct.counts.is_empty());
    }
}
