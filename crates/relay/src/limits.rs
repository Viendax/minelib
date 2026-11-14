//! Limitation des essais : après 5 échecs (par adresse ou par pseudo), attente de 30 s qui
//! double à chaque nouvel échec (15 min au plus). Inscriptions : 3 par heure et par adresse.
//! Par le tunnel iroh, l'adresse est celle que donne le préambule du tunnel ; quand le
//! joueur passe par un serveur relais d'iroh, on n'a que son identifiant iroh (« iroh:… »),
//! qui ne coûte rien à changer : ces inscriptions-là ont en plus une limite commune.

use std::collections::HashMap;
use std::time::{Duration, Instant};

#[derive(Default)]
pub struct Limits {
    failures: HashMap<String, (u32, Instant)>,
    registrations: HashMap<String, Vec<Instant>>,
}

const FREE_TRIES: u32 = 5;
const MAX_WAIT: u64 = 15 * 60;
/// Inscriptions par heure, toutes ensemble, des joueurs dont on ne connaît pas l'IP.
const IROH_PER_HOUR: usize = 20;
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
        let own = self.bucket_wait(ip, 3);
        let shared = if ip.starts_with("iroh:") {
            self.bucket_wait(IROH_ALL, IROH_PER_HOUR)
        } else {
            None
        };
        own.max(shared)
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
        // Sans IP connue : 3 par identifiant, 20 en tout.
        for i in 0..20 {
            let k = format!("iroh:{i}");
            assert!(l.register_wait(&k).is_none(), "{i}");
            l.registered(&k);
        }
        assert!(l.register_wait("iroh:neuf").is_some());
        assert!(l.register_wait("1.2.3.4").is_none());
    }
}
