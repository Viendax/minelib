//! Réseau local : un relais intégré s'annonce sur le réseau de la maison (diffusion UDP),
//! et un jeu qui n'a pas de relais trouve celui d'un autre PC tout seul, sans réglage.

use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

/// Port des annonces.
pub const PORT: u16 = 27651;
const MAGIC: &[u8; 8] = b"MINELIB1";

/// Message d'annonce : port du relais et clé publique.
pub fn announcement(port: u16, key: &[u8; 32]) -> Vec<u8> {
    let mut v = MAGIC.to_vec();
    v.extend_from_slice(&port.to_le_bytes());
    v.extend_from_slice(key);
    v
}

pub fn parse(msg: &[u8], from: IpAddr) -> Option<(SocketAddr, [u8; 32])> {
    if msg.len() != 42 || &msg[..8] != MAGIC {
        return None;
    }
    let port = u16::from_le_bytes([msg[8], msg[9]]);
    let key: [u8; 32] = msg[10..42].try_into().ok()?;
    Some((SocketAddr::new(from, port), key))
}

/// Annonce le relais toutes les 2 secondes (fil de fond).
pub fn announce(port: u16, key: [u8; 32]) {
    let _ = std::thread::Builder::new()
        .name("relais : annonce".into())
        .spawn(move || {
            let Ok(sock) = UdpSocket::bind("0.0.0.0:0") else {
                return;
            };
            let _ = sock.set_broadcast(true);
            let msg = announcement(port, &key);
            loop {
                let _ = sock.send_to(&msg, ("255.255.255.255", PORT));
                std::thread::sleep(Duration::from_secs(2));
            }
        });
}

/// Écoute les annonces pendant `wait` : premier relais trouvé (adresse, clé).
pub fn discover(wait: Duration) -> Option<(SocketAddr, [u8; 32])> {
    let sock = UdpSocket::bind(("0.0.0.0", PORT)).ok()?;
    let end = Instant::now() + wait;
    let mut buf = [0u8; 64];
    while Instant::now() < end {
        let left = end
            .saturating_duration_since(Instant::now())
            .max(Duration::from_millis(1));
        sock.set_read_timeout(Some(left)).ok()?;
        if let Ok((n, from)) = sock.recv_from(&mut buf) {
            if let Some(found) = parse(&buf[..n], from.ip()) {
                return Some(found);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn announcements_round_trip() {
        let m = announcement(27650, &[7; 32]);
        let (addr, key) = parse(&m, "192.168.1.20".parse().unwrap()).unwrap();
        assert_eq!(addr.to_string(), "192.168.1.20:27650");
        assert_eq!(key, [7; 32]);
        assert!(parse(b"autre chose", "1.2.3.4".parse().unwrap()).is_none());
    }
}
