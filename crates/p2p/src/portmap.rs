//! (v270) Ouverture d'un port UDP sur la box (UPnP, PCP, NAT-PMP).
//!
//! Sert aux passeurs de la voie rapide des parties FPS (`minelib_net::fast`) : leur port
//! doit être joignable de l'extérieur. La bibliothèque `portmapper` (MIT / Apache-2.0, déjà
//! là avec iroh) parle à la box et renouvelle la redirection tant que le jeu tourne.

use std::net::SocketAddrV4;
use std::num::NonZeroU16;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// Demande à la box d'ouvrir le port UDP `port` de ce PC et attend sa réponse (au plus
/// `wait`). Rend l'adresse publique obtenue : son port peut différer de `port` (avec une
/// adresse IPv4 partagée entre abonnés, la box n'accorde que les ports de sa plage). Rien si
/// la box refuse, ne répond pas, ou ne connaît aucun de ces protocoles.
pub fn map_udp(port: u16, wait: Duration) -> Option<SocketAddrV4> {
    static RT: OnceLock<Option<tokio::runtime::Runtime>> = OnceLock::new();
    // Redirections obtenues, par port, gardées en vie (les lâcher les laisse expirer) : un
    // port déjà ouvert n'est pas redemandé.
    static KEPT: Mutex<Vec<(u16, portmapper::Client)>> = Mutex::new(Vec::new());
    let local = port;
    let port = NonZeroU16::new(port)?;
    if let Some((_, c)) = KEPT.lock().unwrap().iter().find(|(p, _)| *p == local) {
        if let Some(a) = *c.watch_external_address().borrow() {
            return Some(a);
        }
    }
    let rt = RT
        .get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .thread_name("box")
                .enable_all()
                .build()
                .ok()
        })
        .as_ref()?;
    let client = {
        let _in = rt.enter();
        portmapper::Client::new(portmapper::Config::default())
    };
    client.update_local_port(port);
    let mut watch = client.watch_external_address();
    let got = rt.block_on(async {
        tokio::time::timeout(wait, async {
            loop {
                if let Some(a) = *watch.borrow_and_update() {
                    return Some(a);
                }
                if watch.changed().await.is_err() {
                    return None;
                }
            }
        })
        .await
        .ok()
        .flatten()
    });
    if got.is_some() {
        let mut kept = KEPT.lock().unwrap();
        kept.retain(|(p, _)| *p != local);
        kept.push((local, client));
        // (Un jeu a rarement plus de deux passeurs à la fois : les plus anciennes expirent.)
        if kept.len() > 4 {
            kept.remove(0);
        }
    }
    got
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sans box qui réponde (machine d'essai, réseau filtré) : pas d'adresse, et on rend la
    /// main dans le délai donné.
    #[test]
    fn no_answer_gives_nothing_within_the_wait() {
        let t = std::time::Instant::now();
        assert_eq!(map_udp(0, Duration::from_millis(200)), None);
        let got = map_udp(40_123, Duration::from_millis(400));
        assert!(t.elapsed() < Duration::from_secs(5));
        // (Sur un réseau avec une box qui accepte, une adresse publique peut revenir.)
        if let Some(a) = got {
            assert_ne!(a.port(), 0);
        }
    }
}
