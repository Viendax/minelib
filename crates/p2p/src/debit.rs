//! (v347) Ce qui part par les serveurs relais de l'équipe d'iroh, comparé à la limite connue
//! de leur débit.
//!
//! Ces serveurs limitent ce que chaque nœud leur envoie, en-têtes compris : environ 120 Ko/s
//! mesurés en v336 (2 Mo en 16,4 s), et en v337, 950 messages de 74 octets par seconde
//! (70 Ko/s) les saturaient déjà, soit environ 55 octets d'en-têtes par paquet. Au-delà, ils
//! brident la connexion. Ce que les tunnels de ce programme envoient par eux est compté,
//! en-têtes estimés compris. Quand c'est plus que `PER_S`, une ligne du journal le dit
//! (toutes les 10 secondes au plus), avec ce que ces liens envoyaient, par genre de message
//! (`minelib_net::traffic`) : de quoi savoir quoi alléger.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use minelib_net::traffic::Key;

/// Octets par seconde envoyés par les serveurs relais d'iroh (en-têtes estimés compris)
/// au-delà desquels le journal le dit : les 120 Ko/s mesurés, moins une marge pour ce qu'on ne
/// compte pas (accusés de réception, maintien des connexions).
pub const PER_S: f64 = 100_000.0;
/// D'un coup, après un moment sans rien envoyer, sans compter comme trop.
const BURST: f64 = 32_000.0;
/// En-têtes estimés d'un paquet (QUIC et trame du serveur relais), et données par paquet au
/// plus.
const HEADER: usize = 60;
const PACKET: usize = 1150;
/// Une ligne du journal au plus par période.
const REPORT_EVERY: Duration = Duration::from_secs(10);

/// Ce que coûte l'envoi de `n` octets, en-têtes estimés compris.
pub(crate) fn cost(n: usize) -> f64 {
    (n + HEADER * n.div_ceil(PACKET).max(1)) as f64
}

/// Le compte partagé (un seau de jetons), et ce qui a dépassé depuis la dernière ligne du
/// journal.
pub(crate) struct Bucket {
    left: f64,
    at: Instant,
    since: Instant,
    offered: f64,
    over: f64,
    over_n: u32,
    /// Seconde en cours : son début et ce qui y est parti ; le plus fort débit d'une seconde.
    second: (Instant, f64),
    peak: f64,
    keys: Vec<Key>,
}

/// Ce qui a dépassé pendant une période.
#[derive(Debug)]
pub(crate) struct Report {
    secs: f64,
    offered: f64,
    over: f64,
    over_n: u32,
    peak: f64,
    pub(crate) keys: Vec<Key>,
}

impl Bucket {
    pub(crate) fn new(now: Instant) -> Self {
        Self {
            left: BURST,
            at: now,
            since: now,
            offered: 0.0,
            over: 0.0,
            over_n: 0,
            second: (now, 0.0),
            peak: 0.0,
            keys: Vec::new(),
        }
    }

    /// Compte l'envoi de `n` octets (de la connexion TCP `key`) : vrai s'il dépasse ce que
    /// `PER_S` permet.
    pub(crate) fn note(&mut self, n: usize, key: Option<Key>, now: Instant) -> bool {
        let dt = now.saturating_duration_since(self.at).as_secs_f64();
        self.left = (self.left + dt * PER_S).min(BURST);
        self.at = self.at.max(now);
        let c = cost(n);
        self.offered += c;
        let (start, sent) = self.second;
        let span = now.saturating_duration_since(start);
        if span >= Duration::from_secs(1) {
            self.peak = self.peak.max(sent / span.as_secs_f64());
            self.second = (now, 0.0);
        }
        self.second.1 += c;
        if self.left >= c {
            self.left -= c;
            return false;
        }
        self.over += c - self.left;
        self.left = 0.0;
        self.over_n += 1;
        if let Some(k) = key {
            if self.keys.len() < 64 && !self.keys.contains(&k) {
                self.keys.push(k);
            }
        }
        true
    }

    /// Une fois par période au plus : ce qui a dépassé (rien si rien n'a dépassé), puis une
    /// nouvelle période.
    pub(crate) fn report(&mut self, now: Instant) -> Option<Report> {
        let span = now.saturating_duration_since(self.since);
        if span < REPORT_EVERY {
            return None;
        }
        let (start, sent) = self.second;
        let last = now.saturating_duration_since(start).as_secs_f64().max(1.0);
        let r = (self.over_n > 0).then(|| Report {
            secs: span.as_secs_f64(),
            offered: self.offered,
            over: self.over,
            over_n: self.over_n,
            peak: self.peak.max(sent / last),
            keys: std::mem::take(&mut self.keys),
        });
        self.since = now;
        self.offered = 0.0;
        self.over = 0.0;
        self.over_n = 0;
        self.second = (now, 0.0);
        self.peak = 0.0;
        self.keys.clear();
        r
    }
}

impl Report {
    /// La ligne du journal (« tunnel : … », partagée avec le journal réseau : ni pseudo, ni
    /// adresse, ni nœud). `kinds` : ce que ces liens envoyaient (genre, octets et messages par
    /// seconde), du plus lourd au plus léger.
    pub(crate) fn line(&self, kinds: &[(&str, f32, f32)]) -> String {
        let mut line = format!(
            "tunnel : serveurs relais d'iroh, au-delà de {:.0} Ko/s (limite mesurée : environ \
             120 Ko/s) : {:.0} Ko de trop en {:.0} s ({} envois), jusqu'à {:.0} Ko/s sur une \
             seconde, {:.0} Ko/s en moyenne, en-têtes compris",
            PER_S / 1000.0,
            self.over / 1000.0,
            self.secs,
            self.over_n,
            self.peak / 1000.0,
            self.offered / self.secs.max(0.001) / 1000.0,
        );
        if !kinds.is_empty() {
            line.push_str(" ; ces liens envoyaient :");
            for (i, (name, bytes, msgs)) in kinds.iter().take(6).enumerate() {
                line.push_str(&format!(
                    "{} {name} {:.1} Ko/s ({:.0}/s)",
                    if i == 0 { "" } else { "," },
                    bytes / 1000.0,
                    msgs
                ));
            }
        }
        line
    }
}

static BUCKET: Mutex<Option<Bucket>> = Mutex::new(None);

/// Compte l'envoi de `n` octets par les serveurs relais d'iroh ; écrit la ligne du journal
/// quand c'est le moment.
pub(crate) fn note(n: usize, key: Option<Key>) {
    let now = Instant::now();
    let report = {
        let mut g = BUCKET.lock().unwrap();
        let b = g.get_or_insert_with(|| Bucket::new(now));
        b.note(n, key, now);
        b.report(now)
    };
    if let Some(r) = report {
        let kinds = minelib_net::traffic::recent(&r.keys);
        log::warn!("{}", r.line(&kinds));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ce qui dépasse `PER_S` (après la réserve) est compté, et la ligne du journal dit
    /// combien, le plus fort débit d'une seconde et ce que ces liens envoyaient.
    #[test]
    fn going_beyond_iroh_servers_known_limit_is_logged() {
        let t0 = Instant::now();
        let mut b = Bucket::new(t0);
        // 25 Ko d'un coup : sous la réserve.
        assert!(!b.note(25_000, None, t0));
        // Puis 2 Mo d'un coup : de trop, sauf ce qui restait de la réserve.
        let a: std::net::SocketAddr = "127.0.0.1:50001".parse().unwrap();
        let c: std::net::SocketAddr = "127.0.0.1:50002".parse().unwrap();
        let k = minelib_net::traffic::key(a, c);
        for _ in 0..125 {
            assert!(b.note(16_000, k, t0));
        }
        let over = cost(25_000) + 125.0 * cost(16_000) - BURST;
        // Ensuite, sous le rythme permis (9 Ko tous les dixièmes de seconde) : plus rien de trop.
        for i in 11..=50u64 {
            assert!(!b.note(9_000, None, t0 + Duration::from_millis(100 * i)));
        }
        assert!(b.report(t0 + Duration::from_secs(5)).is_none());
        let r = b
            .report(t0 + Duration::from_secs(25))
            .expect("quelque chose a dépassé");
        assert_eq!(r.over_n, 125, "{r:?}");
        assert!((r.over - over).abs() < 1.0, "{r:?}");
        assert!(r.peak > 1_000_000.0, "{r:?}");
        assert_eq!(r.keys.len(), 1);
        let line = r.line(&[("Position", 61_000.0, 256.0), ("Skin", 18_000.0, 1.0)]);
        assert!(
            line.starts_with("tunnel : serveurs relais d'iroh, au-delà de 100 Ko/s"),
            "{line}"
        );
        assert!(
            line.contains(
                "ces liens envoyaient : Position 61.0 Ko/s (256/s), Skin 18.0 Ko/s (1/s)"
            ),
            "{line}"
        );
        // Rien de trop pendant la période suivante : pas de ligne.
        assert!(!b.note(1_000, None, t0 + Duration::from_secs(30)));
        assert!(b.report(t0 + Duration::from_secs(40)).is_none());
        // Les petits messages coûtent leurs en-têtes.
        assert_eq!(cost(74), 134.0);
        assert_eq!(cost(2300), 2420.0);
    }
}
