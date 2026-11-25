//! Limitation des essais : après 5 échecs (par adresse ou par pseudo), attente de 30 s qui
//! double à chaque nouvel échec (15 min au plus). Inscriptions : 3 par heure et par adresse.
//! Par le tunnel iroh, l'adresse est celle que donne le préambule du tunnel ; quand le
//! joueur passe par un serveur relais d'iroh ou par des passeurs, on n'a que son identifiant
//! iroh (« iroh:… »), qui ne coûte rien à changer. (v313) C'est maintenant le cas de tous
//! les joueurs (leur jeu ne montre plus son adresse IP au relais) : la limite commune de
//! 20 inscriptions par heure, qui aurait fermé le relais aux nouveaux dès qu'un seul en
//! abusait, est remplacée par une preuve de travail qui monte avec l'affluence
//! (`extra_bits`) : l'inscription coûte plus de calcul, elle n'est jamais refusée.

use std::collections::HashMap;
use std::time::{Duration, Instant};

#[derive(Default)]
pub struct Limits {
    failures: HashMap<String, (u32, Instant)>,
    registrations: HashMap<String, Vec<Instant>>,
}

const FREE_TRIES: u32 = 5;
const MAX_WAIT: u64 = 15 * 60;
/// Inscriptions par heure, toutes ensemble, des joueurs dont on ne connaît pas l'IP, au
/// prix ordinaire. (v313) Au-delà, chaque doublement ajoute un bit à leur preuve de travail.
const IROH_PER_HOUR: usize = 20;
/// (v313) Bits ajoutés au plus (1024 fois le calcul ordinaire : il faudrait plus de dix
/// mille inscriptions dans l'heure pour y arriver).
const EXTRA_BITS_MAX: u8 = 10;
const IROH_ALL: &str = "iroh:*";

impl Limits {
    /// Secondes à attendre avant le prochain essai, s'il y en a.
    pub fn wait(&self, key: &str) -> Option<u64> {
        let (n, last) = self.failures.get(key)?;
        if *n < FREE_TRIES {
            return None;
        }
        let wait = (30u64 << (n - FREE_TRIES).min(10)).min(MAX_WAIT);
        let left = wait.saturating_sub(last.elapsed().as_secs());
        (left > 0).then_some(left)
    }

    pub fn failed(&mut self, key: &str) {
        let e = self
            .failures
            .entry(key.to_string())
            .or_insert((0, Instant::now()));
        e.0 += 1;
        e.1 = Instant::now();
        // Ménage : oublie les adresses calmes depuis longtemps.
        if self.failures.len() > 100_000 {
            self.failures
                .retain(|_, (_, t)| t.elapsed() < Duration::from_secs(MAX_WAIT));
        }
    }

    pub fn succeeded(&mut self, key: &str) {
        self.failures.remove(key);
    }

    pub fn register_wait(&mut self, ip: &str) -> Option<u64> {
        // Ce PC lui-même (relais intégré, essais) : pas de limite.
        if ip == "127.0.0.1" || ip == "::1" {
            return None;
        }
        // (v313) Plus de limite commune aux joueurs sans adresse : voir `extra_bits`.
        self.bucket_wait(ip, 3)
    }

    /// (v313) Bits de preuve de travail en plus pour l'inscription de ce joueur. Aucun
    /// pour une adresse IP connue ou pour ce PC ; pour un joueur sans adresse (« iroh:… »),
    /// selon les inscriptions de l'heure de tous les joueurs sans adresse : 0 en dessous
    /// de 20, puis 1 de 20 à 39, 2 de 40 à 79, 3 de 80 à 159… Qui inscrit des comptes en
    /// masse paie donc chaque compte de plus en plus cher, et les autres avec lui tant que
    /// ça dure ; personne n'est refusé.
    pub fn extra_bits(&mut self, ip: &str) -> u8 {
        if !ip.starts_with("iroh:") {
            return 0;
        }
        let list = self.registrations.entry(IROH_ALL.to_string()).or_default();
        list.retain(|t| t.elapsed() < Duration::from_secs(3600));
        let over = list.len() / IROH_PER_HOUR;
        if over == 0 {
            0
        } else {
            ((usize::BITS - over.leading_zeros()) as u8).min(EXTRA_BITS_MAX)
        }
    }

    fn bucket_wait(&mut self, key: &str, max: usize) -> Option<u64> {
        let list = self.registrations.entry(key.to_string()).or_default();
        list.retain(|t| t.elapsed() < Duration::from_secs(3600));
        if list.len() >= max {
            let oldest = list.iter().min()?;
            Some(3600u64.saturating_sub(oldest.elapsed().as_secs()).max(1))
        } else {
            None
        }
    }

    pub fn registered(&mut self, ip: &str) {
        let now = Instant::now();
        self.registrations
            .entry(ip.to_string())
            .or_default()
            .push(now);
        if ip.starts_with("iroh:") {
            self.registrations
                .entry(IROH_ALL.to_string())
                .or_default()
                .push(now);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locks_after_five_failures() {
        let mut l = Limits::default();
        for _ in 0..4 {
            l.failed("a");
        }
        assert_eq!(l.wait("a"), None);
        l.failed("a");
        assert!(l.wait("a").is_some_and(|w| w > 25));
        l.succeeded("a");
        assert_eq!(l.wait("a"), None);
        for _ in 0..3 {
            assert!(l.register_wait("ip").is_none());
            l.registered("ip");
        }
        assert!(l.register_wait("ip").is_some());
        // Sans IP connue : 3 par identifiant ; (v313) plus de limite commune, la preuve
        // de travail monte à la place (un bit de plus à chaque doublement).
        assert_eq!(l.extra_bits("iroh:neuf"), 0);
        for i in 0..20 {
            let k = format!("iroh:{i}");
            assert!(l.register_wait(&k).is_none(), "{i}");
            l.registered(&k);
        }
        assert!(l.register_wait("iroh:neuf").is_none());
        assert_eq!(l.extra_bits("iroh:neuf"), 1);
        assert_eq!(l.extra_bits("1.2.3.4"), 0);
        assert_eq!(l.extra_bits("127.0.0.1"), 0);
        for i in 20..40 {
            l.registered(&format!("iroh:{i}"));
        }
        assert_eq!(l.extra_bits("iroh:neuf"), 2);
        for i in 40..160 {
            l.registered(&format!("iroh:{i}"));
        }
        assert_eq!(l.extra_bits("iroh:neuf"), 4);
        for i in 0..40_000 {
            l.registered(&format!("iroh:x{i}"));
        }
        assert_eq!(l.extra_bits("iroh:neuf"), EXTRA_BITS_MAX);
        assert!(l.register_wait("iroh:neuf").is_none());
        // Le même identifiant, lui, garde sa limite de 3 par heure.
        for _ in 0..3 {
            l.registered("iroh:meme");
        }
        assert!(l.register_wait("iroh:meme").is_some());
    }

    /// (v313) Un joueur sans adresse connue (tous, depuis que le jeu ne montre plus son IP
    /// au relais) arrive sur un relais où vingt autres viennent de s'inscrire : il n'est
    /// pas refusé, le relais lui annonce un bit de preuve de travail de plus, son jeu le
    /// calcule et il est inscrit. Une preuve au prix ordinaire, elle, ne suffit plus.
    #[test]
    fn an_addressless_player_registers_on_a_busy_relay_for_one_more_bit() {
        use crate::{handle_account, start, Config, Session};
        use minelib_net::client::{Client, Cmd, Event};
        use minelib_net::proto::{ErrorCode, Request, Response};
        let dir = std::env::temp_dir().join(format!("minelib-prix-{}", rand::random::<u32>()));
        let h = start(Config {
            dir,
            bind: "127.0.0.1:0".into(),
            pow_bits: 4,
            name: "essai".into(),
        })
        .unwrap();
        {
            let mut st = h.state.lock().unwrap();
            for i in 0..20 {
                st.limits.registered(&format!("iroh:{i}"));
            }
        }
        // Le tunnel iroh, en court : une porte locale qui dit au relais « iroh:neuf »,
        // puis laisse tout passer dans les deux sens.
        let relay = h.addr;
        let door = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let door_addr = door.local_addr().unwrap();
        std::thread::spawn(move || {
            for c in door.incoming().flatten() {
                use std::io::Write;
                let Ok(mut up) = std::net::TcpStream::connect(relay) else {
                    continue;
                };
                up.write_all(&minelib_p2p::preamble("iroh:neuf")).unwrap();
                let (mut c2, mut up2) = (c.try_clone().unwrap(), up.try_clone().unwrap());
                std::thread::spawn(move || {
                    let _ = std::io::copy(&mut c2, &mut up2);
                });
                std::thread::spawn(move || {
                    let mut c = c;
                    let _ = std::io::copy(&mut up, &mut c);
                });
            }
        });
        let c = Client::spawn(door_addr.to_string(), Some(h.public));
        c.send(Cmd::Register {
            pseudo: "Neuf".into(),
            password: "motdepasse".into(),
        });
        let t = Instant::now();
        loop {
            match c.poll() {
                Some(Event::LoggedIn { .. }) => break,
                Some(Event::Error(m)) => panic!("inscription refusée : {m}"),
                _ => {}
            }
            assert!(t.elapsed() < Duration::from_secs(60), "délai dépassé");
            std::thread::sleep(Duration::from_millis(2));
        }
        let mut st = h.state.lock().unwrap();
        let id = *st
            .conns
            .iter()
            .find(|(_, c)| c.pow_extra == 1)
            .expect("un bit de plus annoncé à cette connexion")
            .0;
        assert!(st.registry.lookup("Neuf").is_some());
        // Une preuve à 4 bits (le prix ordinaire de ce relais d'essai) mais pas à 5.
        let pubkey = [7u8; 32];
        let pow = (0u64..)
            .find(|n| {
                minelib_net::crypto::pow_ok("Autre", &pubkey, *n, 4)
                    && !minelib_net::crypto::pow_ok("Autre", &pubkey, *n, 5)
            })
            .unwrap();
        let mut session = Session::Anonymous;
        let r = handle_account(
            Request::Register {
                pseudo: "Autre".into(),
                pubkey,
                auth: [1; 32],
                vault: Vec::new(),
                pow,
            },
            &mut st,
            &mut session,
            "iroh:neuf",
            id,
        );
        assert!(
            matches!(
                r,
                Response::Error {
                    code: ErrorCode::BadProofOfWork,
                    ..
                }
            ),
            "{r:?}"
        );
    }
}
