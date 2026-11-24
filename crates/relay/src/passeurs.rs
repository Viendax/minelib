//! (v270) Passeurs de la voie rapide des parties FPS (règles : `minelib_net::fast`).
//!
//! Côté relais : qui peut être passeur, et qui porte quelle partie.
//!
//! - **Un passeur s'annonce** (`Request::PasseurOffer`) avec son port UDP, et l'adresse que
//!   sa box lui a donnée si elle a ouvert ce port. Le relais ne le croit pas sur parole : il
//!   y envoie une sonde UDP, et seul celui qui la reçoit (`PasseurEcho`) est retenu.
//! - **L'hôte d'une partie FPS demande des passeurs** (`Request::FastWant`) pour ses
//!   joueurs, groupés par équipe. Le relais en choisit deux par équipe, parmi les jeux qui
//!   n'ont aucun lien avec les joueurs (mêmes règles que pour un garant : `super::unrelated`),
//!   de préférence les moins chargés et les mieux connectés.
//! - **Un passeur ne porte qu'une équipe d'une partie** : un joueur ne connaît que les
//!   adresses des passeurs de sa propre équipe, il ne peut donc gêner que les siens.
//! - **Toutes les équipes ont autant de passeurs**, ou aucune n'en a : s'il en manque, la
//!   voie rapide reste fermée pour tout le monde plutôt que d'avantager une équipe.
//! - **Rien ne dit au passeur ce qu'il porte** : il reçoit des numéros de route tirés au
//!   hasard et la clé d'un sceau, jamais une partie ni un pseudo.
//!
//! - **Un passeur est resondé régulièrement** (`upkeep`) : une sonde perdue est refaite, et
//!   celui qui ne répond plus (PC en veille, box qui a refermé le port, adresse qui a
//!   changé) n'est plus retenu ; les parties qu'il portait reçoivent une autre attribution.
//!
//! Tout est redonné d'un bloc à chaque demande (routes et clés neuves) : quand un passeur
//! s'en va ou que l'hôte change, l'ancienne attribution est retirée à ceux qui la portaient.

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::time::Instant;

use minelib_net::proto::{FastHop, FastWay, Response};
use minelib_net::transport;

use super::State;

/// Passeurs d'une équipe, au plus.
const PER_GROUP: usize = minelib_net::fast::VIAS;
/// Équipes d'une partie, et joueurs d'une équipe, au plus (10 contre 10).
const GROUPS_MAX: usize = 2;
const GROUP_MAX: usize = 16;
/// Routes qu'un passeur porte au plus, toutes parties confondues.
const LOAD_MAX: usize = 24;
/// Un passeur vérifié est resondé toutes les… (essais : bien plus souvent).
#[cfg(not(test))]
const REPROBE: std::time::Duration = std::time::Duration::from_secs(30);
#[cfg(test)]
const REPROBE: std::time::Duration = std::time::Duration::from_millis(300);
/// Une sonde restée sans réponse depuis ce temps est perdue.
#[cfg(not(test))]
const PROBE_WAIT: std::time::Duration = std::time::Duration::from_secs(4);
#[cfg(test)]
const PROBE_WAIT: std::time::Duration = std::time::Duration::from_millis(150);
/// Sondes perdues de suite avant de ne plus compter sur un passeur.
const MISSES_MAX: u8 = 3;
/// Annonces acceptées d'une même connexion par minute (chacune fait partir une sonde vers
/// l'adresse annoncée : pas question de s'en servir pour arroser quelqu'un).
const OFFERS_PER_MIN: u8 = 6;

/// Passeur annoncé par une connexion.
struct Offer {
    addr: SocketAddrV4,
    /// Numéro de la sonde envoyée, tant que la réponse n'est pas venue.
    probe: Option<u64>,
    /// Dernière sonde envoyée, et sondes perdues de suite.
    probed_at: Instant,
    missed: u8,
    verified: bool,
    since: Instant,
}

/// Attribution d'une partie.
struct Assign {
    lot: u64,
    host: u64,
    groups: Vec<Vec<u64>>,
    /// Passeurs retenus, et le nombre de routes confiées à chacun.
    passeurs: Vec<(u64, usize)>,
}

/// Passeurs connus du relais et parties servies.
#[derive(Default)]
pub(crate) struct Fast {
    offers: HashMap<u64, Offer>,
    rooms: HashMap<u64, Assign>,
    /// Annonces récentes de chaque connexion (début de la minute, nombre).
    tries: HashMap<u64, (Instant, u8)>,
    /// Socket des sondes (ouvert à la première annonce).
    sock: Option<UdpSocket>,
}

impl Fast {
    /// Routes confiées en ce moment à ce passeur.
    fn load(&self, id: u64) -> usize {
        self.rooms
            .values()
            .flat_map(|a| a.passeurs.iter())
            .filter(|(p, _)| *p == id)
            .map(|(_, n)| n)
            .sum()
    }

    /// Passeurs vérifiés (essais et journal).
    pub(crate) fn verified(&self) -> usize {
        self.offers.values().filter(|o| o.verified).count()
    }
}

/// Une adresse que des joueurs d'ailleurs peuvent joindre : pas une adresse de réseau
/// local, ni la plage que les opérateurs partagent entre leurs abonnés (100.64.0.0/10).
fn public(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    !(ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_multicast()
        || (o[0] == 100 && (64..128).contains(&o[1])))
}

/// `PasseurOffer` : où joindre ce passeur (`external`, sinon l'adresse d'où vient sa
/// connexion, avec son port), et une sonde pour vérifier qu'on y arrive vraiment.
pub(crate) fn offer(
    st: &mut State,
    id: u64,
    peer: &str,
    port: u16,
    external: Option<([u8; 4], u16)>,
) {
    let seen: Option<Ipv4Addr> = match peer.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(a)) => Some(a),
        Ok(std::net::IpAddr::V6(a)) => a.to_ipv4_mapped(),
        // (Venu par un serveur relais d'iroh : son adresse n'est pas connue.)
        Err(_) => None,
    };
    let addr = match external {
        Some((ip, p)) => Some(SocketAddrV4::new(Ipv4Addr::from(ip), p)),
        None => seen.map(|ip| SocketAddrV4::new(ip, port)),
    };
    // Sur un relais qui n'écoute que cette machine (essais, jeu seul), tout le monde est
    // en local ; ailleurs, une adresse de réseau local ne sert à personne.
    let Some(addr) =
        addr.filter(|a| a.port() != 0 && (!super::machines_apart(st) || public(*a.ip())))
    else {
        return;
    };
    let t = st.fast.tries.entry(id).or_insert((Instant::now(), 0));
    if t.0.elapsed() >= std::time::Duration::from_secs(60) {
        *t = (Instant::now(), 0);
    }
    if t.1 >= OFFERS_PER_MIN {
        return;
    }
    t.1 += 1;
    if st.fast.sock.is_none() {
        st.fast.sock = UdpSocket::bind(("0.0.0.0", 0)).ok();
    }
    let Some(sock) = &st.fast.sock else {
        return;
    };
    let nonce: u64 = rand::random();
    minelib_net::fast::probe(sock, std::net::SocketAddr::V4(addr), nonce);
    // Une nouvelle adresse (la box vient d'ouvrir le port) remplace l'ancienne ; ce
    // qu'il portait par l'ancienne continue tant qu'elle répond.
    let was = st
        .fast
        .offers
        .get(&id)
        .filter(|o| o.addr == addr && o.verified);
    let verified = was.is_some();
    st.fast.offers.insert(
        id,
        Offer {
            addr,
            probe: Some(nonce),
            probed_at: Instant::now(),
            missed: 0,
            verified,
            since: Instant::now(),
        },
    );
}

/// `PasseurEcho` : la sonde est arrivée, le passeur est joignable à l'adresse annoncée.
pub(crate) fn echo(st: &mut State, id: u64, nonce: u64) {
    if let Some(o) = st.fast.offers.get_mut(&id) {
        if o.probe == Some(nonce) {
            o.probe = None;
            o.missed = 0;
            if !o.verified {
                o.verified = true;
                let took = o.since.elapsed();
                log::info!(
                    "passeur vérifié en {took:?} ({} en tout)",
                    st.fast.verified()
                );
                // Les parties qui en manquaient réessaient.
                let waiting: Vec<u64> = st
                    .fast
                    .rooms
                    .iter()
                    .filter(|(_, a)| a.passeurs.is_empty())
                    .map(|(r, _)| *r)
                    .collect();
                for rid in waiting {
                    assign(st, rid);
                }
            }
        }
    }
}

/// La connexion se ferme : ce qu'on savait d'elle est oublié.
pub(crate) fn gone(st: &mut State, id: u64) {
    st.fast.tries.remove(&id);
    off(st, id);
}

/// Ce passeur s'en va (`PasseurOff`, ou sa connexion se ferme) : les parties qu'il portait
/// reçoivent une nouvelle attribution.
pub(crate) fn off(st: &mut State, id: u64) {
    if st.fast.offers.remove(&id).is_none() {
        return;
    }
    reassign(st, id);
}

/// Entretien (à chaque battement du relais) : les sondes restées sans réponse sont
/// refaites, les passeurs vérifiés sont resondés de temps en temps, et celui qui a manqué
/// `MISSES_MAX` sondes de suite n'est plus retenu (jusqu'à sa prochaine annonce).
pub(crate) fn upkeep(st: &mut State) {
    let Some(sock) = &st.fast.sock else {
        return;
    };
    let mut lost: Vec<u64> = Vec::new();
    for (&id, o) in st.fast.offers.iter_mut() {
        let waited = o.probed_at.elapsed();
        let again = match o.probe {
            Some(_) if waited < PROBE_WAIT => false,
            Some(_) => {
                o.missed += 1;
                if o.missed >= MISSES_MAX {
                    o.probe = None;
                    if o.verified {
                        o.verified = false;
                        lost.push(id);
                    }
                    false
                } else {
                    true
                }
            }
            None => o.verified && waited >= REPROBE,
        };
        if again {
            let nonce: u64 = rand::random();
            minelib_net::fast::probe(sock, std::net::SocketAddr::V4(o.addr), nonce);
            o.probe = Some(nonce);
            o.probed_at = Instant::now();
        }
    }
    for id in lost {
        log::info!("passeur muet : retiré ({} vérifiés)", st.fast.verified());
        reassign(st, id);
    }
}

/// Nouvelle attribution pour les parties que portait ce passeur.
fn reassign(st: &mut State, id: u64) {
    let rooms: Vec<u64> = st
        .fast
        .rooms
        .iter()
        .filter(|(_, a)| a.passeurs.iter().any(|(p, _)| *p == id))
        .map(|(r, _)| *r)
        .collect();
    for rid in rooms {
        assign(st, rid);
    }
}

/// La partie est finie : ses passeurs n'ont plus rien à porter pour elle.
pub(crate) fn room_gone(st: &mut State, rid: u64) {
    if let Some(a) = st.fast.rooms.remove(&rid) {
        release(st, &a);
    }
}

fn release(st: &State, a: &Assign) {
    for (p, _) in &a.passeurs {
        if let Some(c) = st.conns.get(p) {
            transport::push(
                &c.tx,
                &Response::PasseurTask {
                    lot: a.lot,
                    routes: Vec::new(),
                },
            );
        }
    }
}

/// `FastWant` : l'hôte d'une partie FPS demande des passeurs pour ses joueurs.
pub(crate) fn want(st: &mut State, id: u64, groups: Vec<Vec<u64>>) {
    let Some(rid) = st.conns.get(&id).and_then(|c| c.room) else {
        return;
    };
    let Some(r) = st.rooms.get(&rid) else {
        return;
    };
    if r.kind != 0 || r.host != id {
        return;
    }
    // Seuls des joueurs de la partie, chacun une seule fois, jamais l'hôte ni un garant.
    let mut taken = std::collections::HashSet::new();
    let groups: Vec<Vec<u64>> = groups
        .into_iter()
        .take(GROUPS_MAX)
        .map(|g| {
            g.into_iter()
                .filter(|p| {
                    *p != id && r.members.contains(p) && !r.hidden.contains(p) && taken.insert(*p)
                })
                .take(GROUP_MAX)
                .collect::<Vec<u64>>()
        })
        .filter(|g| !g.is_empty())
        .collect();
    let old = st.fast.rooms.insert(
        rid,
        Assign {
            lot: 0,
            host: id,
            groups,
            passeurs: Vec::new(),
        },
    );
    if let Some(a) = old {
        release(st, &a);
    }
    assign(st, rid);
}

/// Choisit les passeurs de la partie et donne leurs routes à chacun (passeurs, joueurs,
/// hôte). S'il n'y en a pas assez pour que chaque équipe en ait autant, la voie rapide est
/// fermée pour tous.
fn assign(st: &mut State, rid: u64) {
    let Some(old) = st.fast.rooms.remove(&rid) else {
        return;
    };
    release(st, &old);
    let Some(r) = st.rooms.get(&rid) else {
        return;
    };
    // L'hôte a changé depuis la demande : au nouvel hôte de demander.
    if r.host != old.host || r.kind != 0 {
        close(st, &old.groups, old.host);
        return;
    }
    let groups: Vec<Vec<u64>> = old
        .groups
        .iter()
        .map(|g| {
            g.iter()
                .copied()
                .filter(|p| r.members.contains(p))
                .collect::<Vec<u64>>()
        })
        .filter(|g| !g.is_empty())
        .collect();
    // Tous ceux de la partie (joueurs, hôte, garants) : un passeur n'a de lien avec aucun.
    let inside: Vec<u64> = r
        .members
        .iter()
        .copied()
        .chain(std::iter::once(r.host))
        .collect();
    let names: Vec<String> = inside
        .iter()
        .filter_map(|m| st.conns.get(m)?.pseudo.clone())
        .collect();
    let apart = super::machines_apart(st);
    let mut cands: Vec<(usize, f32, u64, SocketAddrV4)> = st
        .fast
        .offers
        .iter()
        .filter(|(_, o)| o.verified)
        .filter(|(p, _)| !inside.contains(p))
        .filter_map(|(&p, o)| {
            let c = st.conns.get(&p)?;
            let name = c.pseudo.as_deref()?;
            if c.kicked || c.garant_banned || super::link_bad(c) {
                return None;
            }
            // Pas sur la machine d'un joueur de la partie (même box : même adresse).
            if apart
                && inside
                    .iter()
                    .filter_map(|m| st.conns.get(m))
                    .any(|m| super::same_machine(c, m))
            {
                return None;
            }
            if !super::unrelated(st, name, &names) || !super::trusted(st, name) {
                return None;
            }
            let load = st.fast.load(p);
            // Délai jusqu'au relais, par tranches de 20 ms : faute de mieux, c'est ce qui
            // dit qu'un passeur n'est pas à l'autre bout du monde.
            let ms = super::link_ms(c).unwrap_or(999.0);
            let tier = (ms / 20.0) as usize;
            (load < LOAD_MAX).then(|| (tier * 1000 + load, ms, p, o.addr))
        })
        .collect();
    // Les mieux connectés d'abord (par tranches), puis les moins chargés (le numéro
    // départage).
    cands.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then(a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
            .then(a.2.cmp(&b.2))
    });
    // Jamais deux passeurs d'une même partie à la même adresse (une seule box).
    let mut picked: Vec<(u64, SocketAddrV4)> = Vec::new();
    for (_, _, p, addr) in cands {
        if !apart || picked.iter().all(|(_, a)| a.ip() != addr.ip()) {
            picked.push((p, addr));
        }
    }
    let per = if groups.is_empty() {
        0
    } else {
        (picked.len() / groups.len()).min(PER_GROUP)
    };
    if per == 0 {
        // La demande est gardée : dès qu'un passeur de plus est vérifié, on réessaie.
        close(st, &old.groups, old.host);
        st.fast.rooms.insert(
            rid,
            Assign {
                lot: 0,
                host: old.host,
                groups: old.groups,
                passeurs: Vec::new(),
            },
        );
        return;
    }
    let lot: u64 = rand::random();
    let mut tasks: HashMap<u64, Vec<(u64, [u8; 32])>> = HashMap::new();
    let mut ways: Vec<FastWay> = Vec::new();
    for (gi, g) in groups.iter().enumerate() {
        let mine = &picked[gi * per..(gi + 1) * per];
        for &peer in g {
            let route: u64 = rand::random();
            let passeurs: Vec<FastHop> = mine
                .iter()
                .map(|(p, addr)| {
                    let seal: [u8; 32] = rand::random();
                    tasks.entry(*p).or_default().push((route, seal));
                    FastHop {
                        addr: addr.ip().octets(),
                        port: addr.port(),
                        seal,
                    }
                })
                .collect();
            ways.push(FastWay {
                peer,
                route,
                key: rand::random(),
                passeurs,
            });
        }
    }
    for (p, routes) in &tasks {
        if let Some(c) = st.conns.get(p) {
            transport::push(
                &c.tx,
                &Response::PasseurTask {
                    lot,
                    routes: routes.clone(),
                },
            );
        }
    }
    for w in &ways {
        if let Some(c) = st.conns.get(&w.peer) {
            transport::push(
                &c.tx,
                &Response::FastGuest {
                    route: w.route,
                    key: w.key,
                    passeurs: w.passeurs.clone(),
                },
            );
        }
    }
    if let Some(c) = st.conns.get(&old.host) {
        transport::push(&c.tx, &Response::FastHost { ways });
    }
    log::info!(
        "voie rapide : partie {rid}, {} équipe(s), {per} passeur(s) par équipe",
        groups.len()
    );
    st.fast.rooms.insert(
        rid,
        Assign {
            lot,
            host: old.host,
            groups,
            passeurs: tasks.iter().map(|(p, r)| (*p, r.len())).collect(),
        },
    );
}

/// Voie rapide fermée pour ces joueurs et leur hôte (pas assez de passeurs).
fn close(st: &State, groups: &[Vec<u64>], host: u64) {
    for p in groups.iter().flatten() {
        if let Some(c) = st.conns.get(p) {
            transport::push(
                &c.tx,
                &Response::FastGuest {
                    route: 0,
                    key: [0; 32],
                    passeurs: Vec::new(),
                },
            );
        }
    }
    if let Some(c) = st.conns.get(&host) {
        transport::push(&c.tx, &Response::FastHost { ways: Vec::new() });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{start, stats, Config};
    use minelib_net::client::{Client, Cmd, Event};
    use minelib_net::fast::Endpoint;
    use std::time::Duration;

    fn wait(c: &Client, pred: impl Fn(&Event) -> bool) -> Event {
        let t = Instant::now();
        loop {
            if let Some(e) = c.poll() {
                if pred(&e) {
                    return e;
                }
                if let Event::Error(m) = &e {
                    panic!("erreur : {m}");
                }
            }
            assert!(t.elapsed() < Duration::from_secs(30), "délai dépassé");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn until(f: impl Fn() -> bool) {
        let t = Instant::now();
        while !f() {
            assert!(t.elapsed() < Duration::from_secs(30), "délai dépassé");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Le bout de la voie rapide que le relais vient de donner à ce jeu.
    fn fast(c: &Client) -> Option<Endpoint> {
        match wait(c, |e| matches!(e, Event::Fast(_))) {
            Event::Fast(e) => e,
            _ => unreachable!(),
        }
    }

    #[test]
    fn public_addresses_only() {
        for (ip, ok) in [
            ("82.66.1.2", true),
            ("192.168.1.10", false),
            ("10.0.0.3", false),
            ("172.20.1.1", false),
            ("100.72.0.9", false),
            ("100.128.0.9", true),
            ("127.0.0.1", false),
            ("169.254.3.3", false),
            ("0.0.0.0", false),
        ] {
            assert_eq!(public(ip.parse().unwrap()), ok, "{ip}");
        }
    }

    /// Un passeur qui ne répond plus aux sondes (PC en veille, port refermé, adresse qui a
    /// changé) n'est plus retenu : sa partie reçoit une autre attribution, sans lui.
    #[test]
    fn a_passeur_that_stops_answering_is_dropped() {
        let dir = std::env::temp_dir().join(format!("minelib-passeurs-{}", rand::random::<u32>()));
        {
            let mut st = stats::Store::open(dir.join("stats")).unwrap();
            for g in ["Pas1", "Pas2", "Pas3"] {
                st.get_mut(g).games = 5;
                st.save(g);
            }
        }
        crate::ONE_FPS_PER_MACHINE.store(false, std::sync::atomic::Ordering::Relaxed);
        let h = start(Config {
            dir: dir.clone(),
            bind: "127.0.0.1:0".into(),
            pow_bits: 4,
            name: "essai".into(),
        })
        .unwrap();
        let addr = h.addr.to_string();
        let login = |pseudo: &str| {
            let c = Client::spawn(addr.clone(), Some(h.public));
            c.send(Cmd::Register {
                pseudo: pseudo.into(),
                password: "motdepasse".into(),
            });
            wait(&c, |e| matches!(e, Event::LoggedIn { .. }));
            c
        };
        let host = login("Hote");
        host.send(Cmd::Search {
            map_sig: [4; 32],
            map: "carte".into(),
            players: 10,
        });
        wait(&host, |e| matches!(e, Event::Hosted(_)));
        let a = login("Alice");
        a.send(Cmd::Search {
            map_sig: [4; 32],
            map: "carte".into(),
            players: 10,
        });
        wait(&a, |e| matches!(e, Event::Joined { .. }));
        let a_id = match wait(&host, |e| matches!(e, Event::PeerJoined { .. })) {
            Event::PeerJoined { peer, .. } => peer,
            _ => unreachable!(),
        };
        let ps: Vec<Client> = ["Pas1", "Pas2", "Pas3"]
            .iter()
            .map(|p| {
                let c = login(p);
                c.send(Cmd::Passeur(true));
                c
            })
            .collect();
        until(|| h.state.lock().unwrap().fast.verified() == 3);
        host.send(Cmd::FastWant(vec![vec![a_id]]));
        let (eh, ea) = (fast(&host).unwrap(), fast(&a).unwrap());
        let before = ea.vias(0);
        assert_eq!(before.len(), 2);
        // Un des deux passeurs retenus devient injoignable : le relais le sonde encore,
        // mais à une adresse où plus personne ne répond.
        let dead = before[0];
        {
            let mut st = h.state.lock().unwrap();
            let o = st
                .fast
                .offers
                .values_mut()
                .find(|o| std::net::SocketAddr::V4(o.addr) == dead)
                .unwrap();
            o.addr = SocketAddrV4::new(Ipv4Addr::LOCALHOST, 9);
        }
        // Trois sondes perdues : il n'est plus retenu, et la partie passe par les deux autres.
        until(|| h.state.lock().unwrap().fast.verified() == 2);
        let after = loop {
            let _ = fast(&host);
            let v = fast(&a).unwrap().vias(0);
            if !v.contains(&dead) {
                break v;
            }
        };
        assert_eq!(after.len(), 2);
        assert!(after.contains(&before[1]));
        until(|| eh.open(a_id) && ea.open(0));
        // Les deux qui répondent restent vérifiés, sonde après sonde.
        std::thread::sleep(Duration::from_millis(1200));
        assert_eq!(h.state.lock().unwrap().fast.verified(), 2);
        drop(ps);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Une partie à deux équipes d'un joueur, quatre passeurs vérifiés par la sonde : deux
    /// par équipe, jamais les mêmes ; les messages passent de bout en bout dans les deux
    /// sens. Puis les passeurs s'en vont un à un : un par équipe tant qu'il en reste deux,
    /// et quand il n'en reste qu'un, la voie est fermée pour tout le monde.
    #[test]
    fn two_passeurs_per_team_then_fewer_then_none() {
        let dir = std::env::temp_dir().join(format!("minelib-passeurs-{}", rand::random::<u32>()));
        {
            let mut st = stats::Store::open(dir.join("stats")).unwrap();
            for g in ["Pas1", "Pas2", "Pas3", "Pas4", "Novice"] {
                st.get_mut(g).games = if g == "Novice" { 0 } else { 5 };
                st.save(g);
            }
        }
        crate::ONE_FPS_PER_MACHINE.store(false, std::sync::atomic::Ordering::Relaxed);
        let h = start(Config {
            dir: dir.clone(),
            bind: "127.0.0.1:0".into(),
            pow_bits: 4,
            name: "essai".into(),
        })
        .unwrap();
        let addr = h.addr.to_string();
        let login = |pseudo: &str| {
            let c = Client::spawn(addr.clone(), Some(h.public));
            c.send(Cmd::Register {
                pseudo: pseudo.into(),
                password: "motdepasse".into(),
            });
            wait(&c, |e| matches!(e, Event::LoggedIn { .. }));
            c
        };
        let search = |c: &Client| {
            c.send(Cmd::Search {
                map_sig: [3; 32],
                map: "carte".into(),
                players: 10,
            });
            wait(c, |e| matches!(e, Event::Joined { .. } | Event::Hosted(_)));
        };
        let peer_of = |host: &Client| match wait(host, |e| matches!(e, Event::PeerJoined { .. })) {
            Event::PeerJoined { peer, .. } => peer,
            _ => unreachable!(),
        };
        let host = login("Hote");
        search(&host);
        let a = login("Alice");
        search(&a);
        let a_id = peer_of(&host);
        let b = login("Bob");
        search(&b);
        let b_id = peer_of(&host);
        // Quatre passeurs, et un compte trop neuf pour qu'on lui confie quoi que ce soit.
        let mut ps: Vec<Client> = ["Pas1", "Pas2", "Pas3", "Pas4", "Novice"]
            .iter()
            .map(|p| {
                let c = login(p);
                c.send(Cmd::Passeur(true));
                c
            })
            .collect();
        until(|| h.state.lock().unwrap().fast.verified() == 5);
        host.send(Cmd::FastWant(vec![vec![a_id], vec![b_id]]));
        let (eh, ea, eb) = (fast(&host).unwrap(), fast(&a).unwrap(), fast(&b).unwrap());
        // Deux passeurs par équipe, aucun en commun ; l'hôte a les mêmes pour chaque joueur.
        let (va, vb) = (ea.vias(0), eb.vias(0));
        assert_eq!((va.len(), vb.len()), (2, 2));
        assert!(va.iter().all(|v| !vb.contains(v)), "{va:?} {vb:?}");
        assert_eq!(eh.vias(a_id), va);
        assert_eq!(eh.vias(b_id), vb);
        {
            let st = h.state.lock().unwrap();
            let rid = *st.fast.rooms.keys().next().unwrap();
            // Le compte trop neuf ne porte rien.
            let novice = st
                .conns
                .iter()
                .find(|(_, c)| c.pseudo.as_deref() == Some("Novice"))
                .map(|(id, _)| *id)
                .unwrap();
            assert!(st.fast.rooms[&rid]
                .passeurs
                .iter()
                .all(|(p, _)| *p != novice));
            assert_eq!(st.fast.load(novice), 0);
        }
        // De bout en bout, dans les deux sens.
        until(|| eh.open(a_id) && eh.open(b_id) && ea.open(0) && eb.open(0));
        assert!(ea.send_all(0, b"entrees d'Alice"));
        match wait(&host, |e| matches!(e, Event::FromPeer { .. })) {
            Event::FromPeer { peer, data } => {
                assert_eq!((peer, data.as_slice()), (a_id, &b"entrees d'Alice"[..]))
            }
            _ => unreachable!(),
        }
        assert!(eh.send_one(b_id, b"positions pour Bob"));
        match wait(&b, |e| matches!(e, Event::FromHost(_))) {
            Event::FromHost(data) => assert_eq!(data, b"positions pour Bob"),
            _ => unreachable!(),
        }
        // Un passeur s'en va : il en reste trois pour deux équipes, donc un chacune.
        drop(ps.remove(0));
        let (eh, ea, eb) = (fast(&host).unwrap(), fast(&a).unwrap(), fast(&b).unwrap());
        assert_eq!((ea.vias(0).len(), eb.vias(0).len()), (1, 1));
        assert_ne!(ea.vias(0), eb.vias(0));
        until(|| eh.open(a_id) && eh.open(b_id));
        assert!(eh.send_one(a_id, b"encore"));
        match wait(&a, |e| matches!(e, Event::FromHost(_))) {
            Event::FromHost(data) => assert_eq!(data, b"encore"),
            _ => unreachable!(),
        }
        // Il n'en reste qu'un (en plus du compte trop neuf) : personne n'en a. (Les bouts
        // sont les mêmes d'une attribution à l'autre tant que la voie n'a pas été fermée.)
        ps[0].send(Cmd::Passeur(false));
        drop(ps.remove(1));
        until(|| {
            let st = h.state.lock().unwrap();
            st.fast.verified() == 2 && st.fast.rooms.values().all(|a| a.passeurs.is_empty())
        });
        until(|| eh.ways() == 0 && ea.ways() == 0 && eb.ways() == 0);
        assert!(!eh.send_one(a_id, b"ferme") && !ea.send_all(0, b"ferme"));
        for c in [&host, &a, &b] {
            while c.poll().is_some() {}
        }
        // Un passeur revient : la demande gardée par le relais est servie sans que l'hôte
        // la refasse.
        ps[0].send(Cmd::Passeur(true));
        let (eh, ea, eb) = (fast(&host).unwrap(), fast(&a).unwrap(), fast(&b).unwrap());
        assert_eq!((ea.vias(0).len(), eb.vias(0).len()), (1, 1));
        assert_ne!(ea.vias(0), eb.vias(0));
        assert_eq!(eh.ways(), 2);
        drop(ps);
        let _ = std::fs::remove_dir_all(dir);
    }
}
