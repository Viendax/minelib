//! (v270) Passeurs de la voie rapide des parties FPS (règles : `minelib_net::fast`).
//!
//! Côté relais : qui peut être passeur, et qui porte quelle partie.
//!
//! - **Un passeur s'annonce** (`Request::PasseurOffer`) avec son port UDP, et l'adresse que
//!   sa box lui a donnée si elle a ouvert ce port. Le relais ne le croit pas sur parole : il
//!   y envoie une sonde UDP, et seul celui qui la reçoit (`PasseurEcho`) est retenu.
//! - **L'hôte d'une partie FPS demande des passeurs** (`Request::FastWant`) pour ses
//!   joueurs, groupés par équipe. Le relais choisit pour chaque équipe jusqu'à deux voies,
//!   parmi les jeux qui n'ont aucun lien avec les joueurs (mêmes règles que pour un garant :
//!   `super::unrelated`), de préférence les moins chargés et les mieux connectés.
//! - **(v308) Une voie est une chaîne de deux passeurs** : le joueur n'écrit qu'au premier,
//!   l'hôte qu'au second. Aucun des deux ne voit l'adresse des deux bouts ; aucun joueur ne
//!   voit l'adresse d'un autre joueur de la partie, ni celle de l'hôte. Il faut donc deux
//!   passeurs par voie ; avec moins de deux passeurs par équipe, pas de voie rapide.
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
//! (v301) Les passeurs portent aussi les **tunnels iroh** vers les relais que des joueurs
//! hébergent (région, province, secours) : voir `tunnel_want`. Sans eux, ces tunnels ne
//! passent que par les serveurs relais publics d'iroh.
//!
//! Tout est redonné d'un bloc à chaque demande (routes et clés neuves) : quand un passeur
//! s'en va ou que l'hôte change, l'ancienne attribution est retirée à ceux qui la portaient.

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::time::Instant;

use minelib_net::proto::{FastHop, FastWay, Request, Response};
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
/// (v274) Un passeur signalé par l'hôte (`FastBad`) n'est remplacé que si la dernière
/// attribution de la partie a au moins cet âge : chaque remplacement redonne toutes les
/// routes, la voie rapide s'interrompt un instant.
#[cfg(not(test))]
const BAD_EVERY: std::time::Duration = std::time::Duration::from_secs(20);
#[cfg(test)]
const BAD_EVERY: std::time::Duration = std::time::Duration::from_millis(200);
/// Annonces acceptées d'une même connexion par minute (chacune fait partir une sonde vers
/// l'adresse annoncée : pas question de s'en servir pour arroser quelqu'un).
const OFFERS_PER_MIN: u8 = 6;

/// (v301) Passeurs d'un tunnel, au plus. (v304) Quand il y en a deux, ils sont mis en
/// chaîne : joueur → premier → second → hôte. Chacun ne voit qu'un des deux bouts.
const TUNNEL_VIAS: usize = 2;
/// Routes que chaque passeur garde libres pour les parties FPS : les tunnels ne prennent
/// pas ses dernières places.
const TUNNEL_ROOM: usize = 8;
/// (v313) Échange direct : des 16 places de tunnel d'un passeur, 8 sont gardées aux joueurs
/// dont le jeu est lui-même un passeur vérifié. Tant que les passeurs ont de la place, tout
/// le monde est servi ; quand ils se remplissent, ceux qui portent les paquets des autres
/// continuent de recevoir des routes, les autres gardent les serveurs relais d'iroh.
const TUNNEL_GIVERS: usize = 8;
/// Un tunnel que son joueur ne redemande plus (il le fait toutes les 30 s tant qu'il s'en
/// sert) est repris après ce temps ; bien plus tard si le joueur n'a plus aucune connexion
/// à ce relais (endormi au fond d'un pays, il s'en sert encore sans pouvoir le redire).
#[cfg(not(test))]
const TUNNEL_LEASE: std::time::Duration = std::time::Duration::from_secs(100);
#[cfg(test)]
const TUNNEL_LEASE: std::time::Duration = std::time::Duration::from_secs(6);
const TUNNEL_ORPHAN: std::time::Duration = std::time::Duration::from_secs(900);
/// (v304) Un tunnel signalé muet ne reçoit pas d'autres passeurs plus souvent que cela.
#[cfg(not(test))]
const TUNNEL_BAD_EVERY: std::time::Duration = std::time::Duration::from_secs(4);
#[cfg(test)]
const TUNNEL_BAD_EVERY: std::time::Duration = std::time::Duration::from_millis(500);
/// Tunnels d'un même compte, au plus (le plus ancien est repris au-delà).
const TUNNELS_PER_PLAYER: usize = 12;
/// Demandes de tunnel acceptées d'une connexion par minute.
const WANTS_PER_MIN: u16 = 240;

/// (v301) Tunnel iroh d'un joueur vers un relais hébergé, porté par des passeurs.
struct Tunnel {
    node: [u8; 32],
    pseudo: String,
    /// Connexion du joueur (la dernière qui l'a demandé) et de l'hôte du nœud.
    guest: u64,
    host: u64,
    /// Passeurs retenus (connexion, clé du sceau de la route chez lui). (v308) Toujours
    /// deux, en chaîne : le premier du côté du joueur, le second du côté de l'hôte.
    passeurs: Vec<(u64, [u8; 32])>,
    /// (v304) Passeurs par lesquels ce tunnel ne passait plus (signalés par ses bouts) :
    /// plus jamais pour lui. Et quand ses passeurs lui ont été donnés.
    bad: Vec<u64>,
    at: Instant,
    renewed: Instant,
}

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
    /// (v274) Passeurs de chaque équipe, dans l'ordre donné aux joueurs ; ceux que l'hôte
    /// a signalés (muets ou lents pour cette partie : plus jamais pour elle) ; et quand
    /// cette attribution a été faite.
    picked: Vec<Vec<u64>>,
    bad: Vec<u64>,
    at: Instant,
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
    /// (v301) Nœuds iroh des relais hébergés par des connexions de ce relais (région,
    /// province, secours), tunnels portés (par numéro de route), et demandes récentes de
    /// chaque connexion.
    nodes: HashMap<[u8; 32], u64>,
    tunnels: HashMap<u64, Tunnel>,
    asks: HashMap<u64, (Instant, u16)>,
    /// (v304) Connexions à qui on a dit s'il y a des passeurs, et ce qu'on a dit.
    told: std::collections::HashSet<u64>,
    said: bool,
    /// (v307) Journaux réseau (`super::journaux`) : ce qu'on a dit aux connexions, lignes
    /// reçues de chacune depuis une minute ; demandes refusées depuis le dernier relevé
    /// (annonces de passeur, demandes de tunnel, lignes de journal), et connexions au
    /// relevé d'avant.
    pub(crate) logs_said: bool,
    pub(crate) log_rate: HashMap<u64, (Instant, u16)>,
    pub(crate) refused: [u32; 3],
    pub(crate) conns_before: usize,
}

impl Fast {
    /// (v297) Cette connexion de garant est celle d'un passeur vérifié (sa box laisse
    /// entrer l'UDP) : le relais qu'il hébergerait pourrait être joint en direct.
    pub(crate) fn is_open(&self, id: u64) -> bool {
        self.offers.get(&id).is_some_and(|o| o.verified)
    }

    /// (v287) Pour le relevé de santé : passeurs annoncés, vérifiés, et parties portées.
    pub(crate) fn counts(&self) -> (usize, usize, usize) {
        (
            self.offers.len(),
            self.offers.values().filter(|o| o.verified).count(),
            self.rooms.len(),
        )
    }

    /// (v301) Tunnels portés par des passeurs en ce moment.
    pub(crate) fn tunnels(&self) -> usize {
        self.tunnels.len()
    }

    /// Routes confiées en ce moment à ce passeur.
    fn load(&self, id: u64) -> usize {
        self.rooms
            .values()
            .flat_map(|a| a.passeurs.iter())
            .filter(|(p, _)| *p == id)
            .map(|(_, n)| n)
            .sum::<usize>()
            + self
                .tunnels
                .values()
                .filter(|t| t.passeurs.iter().any(|(p, _)| *p == id))
                .count()
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
        st.fast.refused[0] += 1;
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
                announce(st);
            }
        }
    }
}

/// La connexion se ferme : ce qu'on savait d'elle est oublié.
pub(crate) fn gone(st: &mut State, id: u64) {
    st.fast.tries.remove(&id);
    st.fast.asks.remove(&id);
    st.fast.told.remove(&id);
    st.fast.log_rate.remove(&id);
    off(st, id);
    // (v301) Les relais qu'elle hébergeait ne sont plus là : leurs tunnels sont repris.
    st.fast.nodes.retain(|_, c| *c != id);
    let closed: Vec<u64> = st
        .fast
        .tunnels
        .iter()
        .filter(|(_, t)| t.host == id)
        .map(|(r, _)| *r)
        .collect();
    for route in closed {
        tunnel_close(st, route);
    }
}

/// Ce passeur s'en va (`PasseurOff`, ou sa connexion se ferme) : les parties qu'il portait
/// reçoivent une nouvelle attribution.
pub(crate) fn off(st: &mut State, id: u64) {
    if st.fast.offers.remove(&id).is_none() {
        return;
    }
    reassign(st, id);
    announce(st);
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
    let any_lost = !lost.is_empty();
    for id in lost {
        log::info!("passeur muet : retiré ({} vérifiés)", st.fast.verified());
        reassign(st, id);
    }
    if any_lost {
        announce(st);
    }
    // (v301) Tunnels que leur joueur ne redemande plus.
    let old: Vec<u64> = st
        .fast
        .tunnels
        .iter()
        .filter(|(_, t)| {
            let idle = t.renewed.elapsed();
            idle >= TUNNEL_ORPHAN
                || (idle >= TUNNEL_LEASE
                    && st
                        .conns
                        .values()
                        .any(|c| c.pseudo.as_deref() == Some(t.pseudo.as_str())))
        })
        .map(|(r, _)| *r)
        .collect();
    for route in old {
        tunnel_close(st, route);
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
    // (v301) Ses tunnels reçoivent d'autres passeurs (même route : le tunnel continue).
    let routes: Vec<u64> = st
        .fast
        .tunnels
        .iter()
        .filter(|(_, t)| t.passeurs.iter().any(|(p, _)| *p == id))
        .map(|(r, _)| *r)
        .collect();
    for route in routes {
        tunnel_assign(st, route);
    }
}

/// (v304) Cette connexion vient d'entrer : on lui dit s'il y a des passeurs (une fois).
pub(crate) fn greet(st: &mut State, id: u64) {
    if st.fast.told.insert(id) {
        if st.fast.said {
            if let Some(c) = st.conns.get(&id) {
                let n = st.fast.verified() as u32;
                transport::push(&c.tx, &Response::PasseursKnown { n });
            }
        }
        super::journaux::tell(st, id);
    }
}

/// (v307) À chaque battement : ceux qui sont entrés sans rien demander depuis sont
/// accueillis aussi.
pub(crate) fn greet_all(st: &mut State) {
    let new: Vec<u64> = st
        .conns
        .iter()
        .filter(|(id, c)| c.pseudo.is_some() && !st.fast.told.contains(id))
        .map(|(id, _)| *id)
        .collect();
    for id in new {
        greet(st, id);
    }
}

/// (v304) Il y a maintenant des passeurs, ou il n'y en a plus : dit à tous ceux qui sont
/// entrés. Sans passeur, leur jeu ouvre ses tunnels sans demander de route.
fn announce(st: &mut State) {
    let n = st.fast.verified() as u32;
    if (n > 0) == st.fast.said {
        return;
    }
    st.fast.said = n > 0;
    for c in st.conns.values().filter(|c| c.pseudo.is_some()) {
        transport::push(&c.tx, &Response::PasseursKnown { n });
    }
}

/// (v301) Le nœud iroh qu'annonce cette demande d'un hôte (son relais de région, de
/// province ou de secours est prêt).
pub(crate) fn node_of(req: &Request) -> Option<[u8; 32]> {
    match req {
        Request::ShardReady { node, .. }
        | Request::StandbyReady { node, .. }
        | Request::ProvinceReady { node, .. }
        | Request::ProvinceFollow { node, .. } => Some(*node).filter(|n| *n != [0; 32]),
        _ => None,
    }
}

/// (v301) Cette connexion héberge le relais du nœud `node`. La première qui l'annonce le
/// garde tant qu'elle est là (personne ne détourne les tunnels d'un autre : au pire, ils
/// repassent par les serveurs d'iroh, le nœud lui-même ne s'imite pas).
pub(crate) fn node_ready(st: &mut State, id: u64, node: [u8; 32]) {
    let live = st
        .fast
        .nodes
        .get(&node)
        .is_some_and(|c| *c != id && st.conns.contains_key(c));
    if !live {
        st.fast.nodes.insert(node, id);
    }
}

/// (v301) `TunnelWant` : un joueur ouvre, ou garde ouvert, un tunnel iroh vers le relais
/// du nœud `node`. S'il y a des passeurs, une route lui est donnée, à lui et à l'hôte du
/// nœud : leurs paquets y passent en UDP, sans que l'un voie l'adresse de l'autre, au lieu
/// des serveurs relais d'iroh. Le passeur ne sait ni qui ils sont ni ce qu'il porte (le
/// tunnel est chiffré de bout en bout).
pub(crate) fn tunnel_want(st: &mut State, id: u64, node: [u8; 32]) {
    let Some(&host) = st.fast.nodes.get(&node) else {
        return;
    };
    let Some(pseudo) = st.conns.get(&id).and_then(|c| c.pseudo.clone()) else {
        return;
    };
    let (Some(g), Some(h)) = (st.conns.get(&id), st.conns.get(&host)) else {
        return;
    };
    // Vers son propre relais : pas besoin de tunnel.
    if host == id || (super::machines_apart(st) && super::same_machine(g, h)) {
        return;
    }
    let a = st.fast.asks.entry(id).or_insert((Instant::now(), 0));
    if a.0.elapsed() >= std::time::Duration::from_secs(60) {
        *a = (Instant::now(), 0);
    }
    if a.1 >= WANTS_PER_MIN {
        st.fast.refused[1] += 1;
        return;
    }
    a.1 += 1;
    let had = st
        .fast
        .tunnels
        .iter()
        .find(|(_, t)| t.node == node && t.pseudo == pseudo)
        .map(|(r, t)| (*r, t.host));
    match had {
        // Il s'en sert toujours.
        Some((route, h)) if h == host => {
            if let Some(t) = st.fast.tunnels.get_mut(&route) {
                t.renewed = Instant::now();
                t.guest = id;
            }
            return;
        }
        // Le nœud a changé d'hôte : tout est refait.
        Some((route, _)) => tunnel_close(st, route),
        None => {}
    }
    let mine: Vec<(u64, Instant)> = st
        .fast
        .tunnels
        .iter()
        .filter(|(_, t)| t.pseudo == pseudo)
        .map(|(r, t)| (*r, t.renewed))
        .collect();
    if mine.len() >= TUNNELS_PER_PLAYER {
        if let Some((route, _)) = mine.iter().min_by_key(|(_, at)| *at) {
            tunnel_close(st, *route);
        }
    }
    let route: u64 = rand::random();
    st.fast.tunnels.insert(
        route,
        Tunnel {
            node,
            pseudo,
            guest: id,
            host,
            passeurs: Vec::new(),
            bad: Vec::new(),
            at: Instant::now(),
            renewed: Instant::now(),
        },
    );
    tunnel_assign(st, route);
}

/// (v304) `TunnelBad` : un bout du tunnel dit que plus rien ne passe par sa route. Ses
/// passeurs sont écartés pour lui (on ne sait pas lequel s'est tu) et d'autres lui sont
/// donnés tout de suite, sans attendre que les sondes du relais trouvent le muet.
pub(crate) fn tunnel_bad(st: &mut State, id: u64, route: u64) {
    let pseudo = st.conns.get(&id).and_then(|c| c.pseudo.clone());
    let Some(t) = st.fast.tunnels.get_mut(&route) else {
        return;
    };
    let mine = t.guest == id || t.host == id || pseudo.as_deref() == Some(t.pseudo.as_str());
    if !mine || t.at.elapsed() < TUNNEL_BAD_EVERY {
        return;
    }
    let was: Vec<u64> = t.passeurs.iter().map(|(p, _)| *p).collect();
    t.bad.extend(was);
    // (Il s'en sert toujours.)
    t.renewed = Instant::now();
    log::info!("tunnel : passeurs muets pour un tunnel, d'autres lui sont donnés");
    tunnel_assign(st, route);
}

/// Choisit les passeurs du tunnel `route` (les mêmes qu'avant s'ils sont encore là) et
/// donne la route à ses deux bouts. Sans passeur : le tunnel est repris.
fn tunnel_assign(st: &mut State, route: u64) {
    let Some(t) = st.fast.tunnels.get(&route) else {
        return;
    };
    let (guest, host, node) = (t.guest, t.host, t.node);
    let bad = t.bad.clone();
    let before: Vec<u64> = t.passeurs.iter().map(|(p, _)| *p).collect();
    let apart = super::machines_apart(st);
    let ends: Vec<&super::Conn> = [guest, host]
        .iter()
        .filter_map(|c| st.conns.get(c))
        .collect();
    let usable = |p: &u64, o: &Offer| -> Option<(usize, f32, u64, SocketAddrV4)> {
        let c = st.conns.get(p)?;
        if !o.verified || c.kicked || c.garant_banned || super::link_bad(c) {
            return None;
        }
        let load = st.fast.load(*p);
        let ms = super::link_ms(c).unwrap_or(999.0);
        Some(((ms / 20.0) as usize * 1000 + load, ms, *p, o.addr))
    };
    // (v313) Celui qui demande est-il passeur lui-même ? (Voir `TUNNEL_GIVERS`.)
    let giver = st.fast.offers.get(&guest).is_some_and(|o| o.verified);
    let kept = if giver {
        TUNNEL_ROOM
    } else {
        TUNNEL_ROOM + TUNNEL_GIVERS
    };
    // Des passeurs sans rapport avec les deux bouts.
    let mut cands: Vec<(usize, f32, u64, SocketAddrV4)> = st
        .fast
        .offers
        .iter()
        .filter(|(p, _)| **p != guest && **p != host && !bad.contains(p))
        .filter_map(|(p, o)| usable(p, o))
        .filter(|(_, _, p, _)| {
            let c = &st.conns[p];
            !(apart && ends.iter().any(|e| super::same_machine(c, e)))
        })
        .filter(|(_, _, p, _)| {
            before.contains(p) || st.fast.load(*p) + kept < LOAD_MAX
        })
        .collect();
    cands.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then(a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
            .then(a.2.cmp(&b.2))
    });
    let mut picked: Vec<(u64, SocketAddrV4)> = Vec::new();
    for (_, _, p, addr) in cands {
        if picked.len() < TUNNEL_VIAS && picked.iter().all(|(_, a)| a.ip() != addr.ip() || !apart)
        {
            picked.push((p, addr));
        }
    }
    // (v308) Deux passeurs en chaîne, ou rien. Un passeur seul verrait l'adresse des deux
    // bouts ; celui du jeu de l'hôte montrerait à l'hôte l'adresse de ses joueurs (les
    // v301 et v304 faisaient l'un et l'autre). Sans chaîne, le tunnel garde les serveurs
    // relais d'iroh, qui ne sont pas des joueurs.
    if picked.len() < 2 {
        picked.clear();
    }
    // Ceux qui ne le portent plus n'ont plus sa route.
    for p in before.iter().filter(|p| picked.iter().all(|(q, _)| q != *p)) {
        if let Some(c) = st.conns.get(p) {
            transport::push(
                &c.tx,
                &Response::PasseurTask {
                    lot: route,
                    routes: Vec::new(),
                },
            );
        }
    }
    if picked.is_empty() {
        st.fast.tunnels.remove(&route);
        // (Jamais donnée : rien à reprendre. Le joueur n'attend pas de réponse, d'autres
        // relais à qui il a fait la même demande ne connaissent même pas ce nœud.)
        if !before.is_empty() {
            tunnel_tell(st, guest, host, node, route, &[], &[]);
        }
        return;
    }
    // Le premier parle au joueur, le second à l'hôte ; entre eux, une adresse fixée ici.
    // Une seule clé de sceau pour toute la route (neuve à chaque attribution).
    let seal: [u8; 32] = rand::random();
    let (first, second) = (picked[0], picked[1]);
    for (me, other, dir) in [
        (first, second, minelib_net::fast::TO_GUEST),
        (second, first, minelib_net::fast::TO_HOST),
    ] {
        if let Some(c) = st.conns.get(&me.0) {
            transport::push(
                &c.tx,
                &Response::PasseurLink {
                    lot: route,
                    route,
                    seal,
                    dir,
                    addr: other.1.ip().octets(),
                    port: other.1.port(),
                },
            );
        }
    }
    let hop = |a: SocketAddrV4| FastHop {
        addr: a.ip().octets(),
        port: a.port(),
        seal,
    };
    if let Some(t) = st.fast.tunnels.get_mut(&route) {
        t.passeurs = vec![(first.0, seal), (second.0, seal)];
        t.at = Instant::now();
    }
    tunnel_tell(st, guest, host, node, route, &[hop(first.1)], &[hop(second.1)]);
}

/// La route du tunnel (ou sa reprise : aucun passeur) est dite à ses deux bouts, l'hôte
/// d'abord (il doit s'être montré au passeur avant le premier paquet du joueur). Chaque
/// bout ne reçoit que les passeurs auxquels il écrit lui-même (`to_guest`, `to_host`).
fn tunnel_tell(
    st: &State,
    guest: u64,
    host: u64,
    node: [u8; 32],
    route: u64,
    to_guest: &[FastHop],
    to_host: &[FastHop],
) {
    for (c, is_host, hops) in [(host, true, to_host), (guest, false, to_guest)] {
        if let Some(c) = st.conns.get(&c) {
            transport::push(
                &c.tx,
                &Response::TunnelWay {
                    node,
                    route,
                    host: is_host,
                    own: false,
                    passeurs: hops.to_vec(),
                },
            );
        }
    }
}

/// Le tunnel `route` est repris : ses passeurs ne le portent plus, ses bouts le savent.
fn tunnel_close(st: &mut State, route: u64) {
    let Some(t) = st.fast.tunnels.remove(&route) else {
        return;
    };
    for (p, _) in &t.passeurs {
        if let Some(c) = st.conns.get(p) {
            transport::push(
                &c.tx,
                &Response::PasseurTask {
                    lot: route,
                    routes: Vec::new(),
                },
            );
        }
    }
    tunnel_tell(st, t.guest, t.host, t.node, route, &[], &[]);
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
    let old = st.fast.rooms.remove(&rid);
    // (Même hôte : les passeurs qu'il a signalés restent écartés.)
    let bad = old
        .as_ref()
        .filter(|a| a.host == id)
        .map(|a| a.bad.clone())
        .unwrap_or_default();
    if let Some(a) = &old {
        release(st, a);
    }
    st.fast.rooms.insert(
        rid,
        Assign {
            lot: 0,
            host: id,
            groups,
            passeurs: Vec::new(),
            picked: Vec::new(),
            bad,
            at: Instant::now(),
        },
    );
    assign(st, rid);
}

/// `FastBad` : l'hôte signale le passeur numéro `via` de la route de `peer` (muet, ou
/// nettement plus lent que l'autre). Il est écarté de cette partie, et l'attribution est
/// refaite avec ceux qui restent.
pub(crate) fn bad(st: &mut State, id: u64, peer: u64, via: u8) {
    let Some(rid) = st.conns.get(&id).and_then(|c| c.room) else {
        return;
    };
    let Some(a) = st.fast.rooms.get_mut(&rid) else {
        return;
    };
    if a.host != id || a.at.elapsed() < BAD_EVERY {
        return;
    }
    let Some(g) = a.groups.iter().position(|g| g.contains(&peer)) else {
        return;
    };
    // (v308) Une voie est une chaîne de deux passeurs : on ne sait pas lequel s'est tu,
    // les deux sont écartés.
    let chain: Vec<u64> = a
        .picked
        .get(g)
        .map(|v| v.iter().skip(via as usize * 2).take(2).copied().collect())
        .unwrap_or_default();
    if chain.is_empty() {
        return;
    }
    for p in chain {
        if !a.bad.contains(&p) && a.bad.len() < 16 {
            a.bad.push(p);
        }
    }
    log::info!("voie rapide : partie {rid}, un passeur signalé par l'hôte est écarté");
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
    // Sans les passeurs signalés par l'hôte ; s'il n'en reste pas assez pour en donner un à
    // chaque équipe, on les reprend (un passeur lent vaut mieux qu'aucun : au pire, les
    // positions repassent par le relais).
    let mut bad = old.bad.clone();
    let (picked, per) = loop {
        let (picked, per) = choose(st, &groups, &inside, &names, &bad, apart);
        if per > 0 || bad.is_empty() {
            break (picked, per);
        }
        bad.clear();
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
                picked: Vec::new(),
                bad,
                at: Instant::now(),
            },
        );
        return;
    }
    let lot: u64 = rand::random();
    let groups_n = groups.len();
    // (v308) Chaque voie d'une équipe est une chaîne de deux passeurs : le joueur n'écrit
    // qu'au premier, l'hôte qu'au second, aucun des deux ne voit les deux adresses. (Avant,
    // un seul passeur par voie, qui voyait le joueur et l'hôte.)
    let mut links: Vec<(u64, Response)> = Vec::new();
    let mut load: HashMap<u64, usize> = HashMap::new();
    let mut ways: Vec<FastWay> = Vec::new();
    let mut guests: Vec<(u64, Response)> = Vec::new();
    for (gi, g) in groups.iter().enumerate() {
        let mine = &picked[gi * per * 2..(gi + 1) * per * 2];
        for &peer in g {
            let route: u64 = rand::random();
            let key: [u8; 32] = rand::random();
            let (mut to_guest, mut to_host) = (Vec::new(), Vec::new());
            for pair in mine.chunks(2) {
                let (first, second) = (pair[0], pair[1]);
                let seal: [u8; 32] = rand::random();
                for (me, other, dir) in [
                    (first, second, minelib_net::fast::TO_GUEST),
                    (second, first, minelib_net::fast::TO_HOST),
                ] {
                    *load.entry(me.0).or_default() += 1;
                    links.push((
                        me.0,
                        Response::PasseurLink {
                            lot,
                            route,
                            seal,
                            dir,
                            addr: other.1.ip().octets(),
                            port: other.1.port(),
                        },
                    ));
                }
                let hop = |a: SocketAddrV4| FastHop {
                    addr: a.ip().octets(),
                    port: a.port(),
                    seal,
                };
                to_guest.push(hop(first.1));
                to_host.push(hop(second.1));
            }
            guests.push((
                peer,
                Response::FastGuest {
                    route,
                    key,
                    passeurs: to_guest,
                },
            ));
            ways.push(FastWay {
                peer,
                route,
                key,
                passeurs: to_host,
            });
        }
    }
    for (to, msg) in links.iter().chain(&guests) {
        if let Some(c) = st.conns.get(to) {
            transport::push(&c.tx, msg);
        }
    }
    if let Some(c) = st.conns.get(&old.host) {
        transport::push(&c.tx, &Response::FastHost { ways });
    }
    log::info!(
        "voie rapide : partie {rid}, {} équipe(s), {per} chaîne(s) de deux passeurs par équipe",
        groups.len()
    );
    st.fast.rooms.insert(
        rid,
        Assign {
            lot,
            host: old.host,
            groups,
            passeurs: load.into_iter().collect(),
            picked: (0..groups_n)
                .map(|gi| {
                    picked[gi * per * 2..(gi + 1) * per * 2]
                        .iter()
                        .map(|(p, _)| *p)
                        .collect()
                })
                .collect(),
            bad,
            at: Instant::now(),
        },
    );
}

/// Passeurs retenus pour une partie, les meilleurs d'abord, et combien chaque équipe en
/// reçoit de voies (le même nombre pour toutes ; 0 : pas assez de passeurs ; (v308) une
/// voie prend deux passeurs). `skip` : ceux que l'hôte a signalés.
fn choose(
    st: &State,
    groups: &[Vec<u64>],
    inside: &[u64],
    names: &[String],
    skip: &[u64],
    apart: bool,
) -> (Vec<(u64, SocketAddrV4)>, usize) {
    let mut cands: Vec<(usize, f32, u64, SocketAddrV4)> = st
        .fast
        .offers
        .iter()
        .filter(|(_, o)| o.verified)
        .filter(|(p, _)| !inside.contains(p) && !skip.contains(p))
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
            if !super::unrelated(st, name, names) || !super::trusted(st, name) {
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
    // (v308) Deux passeurs par voie (une chaîne).
    let per = if groups.is_empty() {
        0
    } else {
        (picked.len() / (2 * groups.len())).min(PER_GROUP)
    };
    (picked, per)
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

    type Link = std::sync::mpsc::Sender<transport::Inbound>;

    /// Un relais d'essai, de quoi y entrer, et un petit relais TCP qui renvoie ce qu'il
    /// reçoit (après le préambule du tunnel).
    fn relay(tag: &str) -> (crate::Handle, impl Fn(&str) -> (Client, Link)) {
        let dir = std::env::temp_dir().join(format!("minelib-{tag}-{}", rand::random::<u32>()));
        let h = start(Config {
            dir,
            bind: "127.0.0.1:0".into(),
            pow_bits: 4,
            name: "essai".into(),
        })
        .unwrap();
        let (addr, public) = (h.addr.to_string(), h.public);
        let login = move |pseudo: &str| {
            let c = Client::spawn(addr.clone(), Some(public));
            c.send(Cmd::Register {
                pseudo: pseudo.into(),
                password: "motdepasse".into(),
            });
            let link = match wait(&c, |e| matches!(e, Event::Link(_))) {
                Event::Link(l) => l,
                _ => unreachable!(),
            };
            wait(&c, |e| matches!(e, Event::LoggedIn { .. }));
            (c, link)
        };
        (h, login)
    }

    fn echo() -> std::net::SocketAddr {
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

    /// Le relais d'un hôte : un nœud sans serveur d'iroh ni adresse directe (il ne peut
    /// être joint que par un passeur), annoncé au relais du monde.
    fn hosted(h: &crate::Handle, link: &Link) -> minelib_p2p::Node {
        let node = minelib_p2p::Node::start_offline().unwrap();
        node.serve(echo());
        transport::push(
            link,
            &Request::ShardReady {
                region: [0, 0],
                node: node.id(),
                addr: String::new(),
                key: [0; 32],
            },
        );
        let id = node.id();
        until(|| h.state.lock().unwrap().fast.nodes.contains_key(&id));
        node
    }

    /// (v301, v308) Un joueur ouvre un tunnel vers le relais qu'un autre héberge : le
    /// relais du monde leur donne une route par deux passeurs en chaîne, et le tunnel y
    /// passe, sans serveur d'iroh et sans que l'un ait l'adresse de l'autre. Le tunnel que
    /// le joueur ne redemande plus est repris : ni lui, ni l'hôte, ni les passeurs n'en
    /// gardent la route.
    #[test]
    fn a_tunnel_to_a_hosted_relay_goes_through_passeurs() {
        let (h, login) = relay("tunnel");
        let (_host, hl) = login("HoteTun");
        let (_guest, _) = login("InviteTun");
        minelib_net::fast::tunnel_asker("InviteTun");
        let (a, _) = login("PasTun");
        let (b, _) = login("PasTun2");
        a.send(Cmd::Passeur(true));
        b.send(Cmd::Passeur(true));
        until(|| h.state.lock().unwrap().fast.verified() == 2);
        let host_node = hosted(&h, &hl);
        let guest_node = minelib_p2p::Node::start_offline().unwrap();
        let t = guest_node.tunnel(host_node.id()).unwrap();
        let mut c = std::net::TcpStream::connect(t).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
        assert!(said_back(&mut c, b"salut"), "le tunnel ne passe pas");
        for i in 0..300u32 {
            assert!(said_back(&mut c, &[i as u8; 5]), "envoi {i}");
        }
        {
            let st = h.state.lock().unwrap();
            assert_eq!(st.fast.tunnels(), 1);
            let t = st.fast.tunnels.values().next().unwrap();
            assert_eq!((t.pseudo.as_str(), t.passeurs.len()), ("InviteTun", 2));
        }
        assert_eq!(minelib_p2p::node_direct(host_node.id()), Some(true));
        // (Dans cet essai, le joueur ne redemande pas sa route avant 30 s.)
        until(|| h.state.lock().unwrap().fast.tunnels() == 0);
        until(|| guest_node.passeurs().routes() == 0 && host_node.passeurs().routes() == 0);
    }

    /// (v308) Un passeur seul ne porte jamais un tunnel (il verrait l'adresse des deux
    /// bouts), et le jeu de l'hôte ne porte jamais les tunnels de ses propres joueurs (il
    /// verrait leurs adresses) : avec un seul passeur en plus de celui de l'hôte, le relais
    /// ne donne aucune route.
    #[test]
    fn one_passeur_alone_never_carries_a_tunnel() {
        let (h, login) = relay("tunnel-seul");
        let (host, hl) = login("HoteSeul");
        let (_guest, gl) = login("InviteSeul");
        let (pas, _) = login("PasSeul");
        host.send(Cmd::Passeur(true));
        pas.send(Cmd::Passeur(true));
        until(|| h.state.lock().unwrap().fast.verified() == 2);
        let host_node = hosted(&h, &hl);
        for _ in 0..3 {
            transport::push(
                &gl,
                &Request::TunnelWant {
                    node: host_node.id(),
                },
            );
            std::thread::sleep(Duration::from_millis(150));
            assert_eq!(h.state.lock().unwrap().fast.tunnels(), 0);
        }
        assert_eq!(host_node.passeurs().routes(), 0);
    }

    /// (v313) Échange direct. Trois passeurs, chacun déjà chargé de huit tunnels : un
    /// joueur qui ne relaie pas n'obtient plus de route (il garde les serveurs d'iroh),
    /// celui dont le jeu est passeur en obtient encore une, par les deux autres.
    #[test]
    fn those_who_relay_still_get_routes_when_passeurs_fill_up() {
        let (h, login) = relay("echange");
        let (_host, hl) = login("HoteEch");
        let (_g1, g1) = login("PrendEch");
        let (_g2, g2) = login("PrendEch2");
        let (a, _) = login("PasEch");
        let (b, _) = login("PasEch2");
        let (c, cl) = login("DonneEch");
        for p in [&a, &b, &c] {
            p.send(Cmd::Passeur(true));
        }
        until(|| h.state.lock().unwrap().fast.verified() == 3);
        // Treize relais hébergés (des nœuds de façade : seules les routes comptent ici).
        let node = |i: u8| [100 + i; 32];
        for i in 0..13u8 {
            transport::push(
                &hl,
                &Request::ShardReady {
                    region: [i as i32, 0],
                    node: node(i),
                    addr: String::new(),
                    key: [0; 32],
                },
            );
        }
        until(|| h.state.lock().unwrap().fast.nodes.len() >= 13);
        // Douze tunnels d'un joueur qui ne relaie pas : deux passeurs chacun, les moins
        // chargés d'abord ; à la fin, chacun des trois en porte huit.
        for i in 0..12u8 {
            transport::push(&g1, &Request::TunnelWant { node: node(i) });
        }
        until(|| h.state.lock().unwrap().fast.tunnels() == 12);
        {
            let st = h.state.lock().unwrap();
            let mut loads: Vec<usize> = st.fast.offers.keys().map(|p| st.fast.load(*p)).collect();
            loads.sort();
            assert_eq!(loads, vec![8, 8, 8]);
        }
        // Un autre joueur qui ne relaie pas : plus de place pour lui.
        transport::push(&g2, &Request::TunnelWant { node: node(12) });
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(h.state.lock().unwrap().fast.tunnels(), 12);
        // Celui qui relaie : servi, par les deux autres passeurs.
        transport::push(&cl, &Request::TunnelWant { node: node(12) });
        until(|| h.state.lock().unwrap().fast.tunnels() == 13);
        let st = h.state.lock().unwrap();
        let t = st
            .fast
            .tunnels
            .values()
            .find(|t| t.pseudo == "DonneEch")
            .unwrap();
        assert_eq!(t.passeurs.len(), 2);
        assert!(t.passeurs.iter().all(|(p, _)| *p != t.guest));
    }

    /// (v307) Un joueur qui partage son journal réseau : ses lignes arrivent dans le
    /// fichier du relais qui a demandé à les recevoir, sous un numéro, sans son pseudo ni
    /// son adresse ; une ligne qui n'est pas une ligne réseau n'y arrive pas.
    #[test]
    fn shared_network_lines_reach_the_relay_file_without_names() {
        let (h, login) = relay("journaux");
        crate::journaux::set_receive_logs(true);
        minelib_net::netlog::set_share(true);
        let (_c, _) = login("JournalJoueur");
        let file = h.state.lock().unwrap().cfg.dir.join(crate::journaux::FILE);
        let t = Instant::now();
        let text = loop {
            // (D'autres essais tournent dans ce programme : leurs connexions peuvent
            // prendre la ligne pour leur propre relais. On la redit jusqu'à la voir ici.)
            minelib_net::netlog::offer("réseau (essai) : ligne partagée");
            minelib_net::netlog::offer("compte : JournalJoueur depuis 127.0.0.1");
            std::thread::sleep(Duration::from_millis(100));
            let text = std::fs::read_to_string(&file).unwrap_or_default();
            if text.contains("ligne partagée") {
                break text;
            }
            assert!(t.elapsed() < Duration::from_secs(30), "rien dans {file:?}");
        };
        minelib_net::netlog::set_share(false);
        crate::journaux::set_receive_logs(false);
        let line = text.lines().find(|l| l.contains("ligne partagée")).unwrap();
        assert!(line.contains(" UTC [") && line.ends_with("] réseau (essai) : ligne partagée"));
        assert!(!text.contains("JournalJoueur") && !text.contains("127.0.0.1"), "{text}");
    }

    /// (v304) Avec deux passeurs, le tunnel passe par les deux à la suite : le joueur
    /// n'écrit qu'au premier, l'hôte qu'au second, et chacun des deux passeurs ne connaît
    /// qu'un bout. (v308) Quand l'un des deux s'en va et qu'il n'y en a pas d'autre, la
    /// route est reprise : un passeur seul ne porte rien.
    #[test]
    fn two_passeurs_carry_a_tunnel_in_a_chain() {
        let (h, login) = relay("tunnel-chaine");
        let (_host, hl) = login("HoteChaine");
        let (_guest, _) = login("InviteChaine");
        minelib_net::fast::tunnel_asker("InviteChaine");
        let (first, _) = login("PasChaine1");
        let (second, _) = login("PasChaine2");
        first.send(Cmd::Passeur(true));
        second.send(Cmd::Passeur(true));
        until(|| h.state.lock().unwrap().fast.verified() == 2);
        let host_node = hosted(&h, &hl);
        let guest_node = minelib_p2p::Node::start_offline().unwrap();
        let t = guest_node.tunnel(host_node.id()).unwrap();
        let mut c = std::net::TcpStream::connect(t).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(25))).unwrap();
        assert!(said_back(&mut c, b"salut"), "le tunnel ne passe pas");
        for i in 0..300u32 {
            assert!(said_back(&mut c, &[i as u8; 5]), "envoi {i}");
        }
        let route = {
            let st = h.state.lock().unwrap();
            let (r, t) = st.fast.tunnels.iter().next().unwrap();
            assert_eq!(t.passeurs.len(), 2);
            *r
        };
        // Chaque bout n'écrit qu'à son passeur, et ce n'est pas le même.
        let (g, o) = (
            guest_node.passeurs().via(route).unwrap(),
            host_node.passeurs().via(route).unwrap(),
        );
        assert_ne!(g, o);
        assert_eq!(minelib_p2p::node_direct(host_node.id()), Some(true));
        // L'un des deux s'en va : il n'en reste qu'un, qui ne porte rien seul. La route
        // est reprise aux deux bouts.
        first.send(Cmd::Passeur(false));
        until(|| h.state.lock().unwrap().fast.tunnels() == 0);
        until(|| guest_node.passeurs().routes() == 0 && host_node.passeurs().routes() == 0);
    }

    /// (v304) Un passeur se tait sans prévenir (PC éteint, box qui referme le port) : les
    /// bouts du tunnel le disent au relais, qui leur donne d'autres passeurs sur la même
    /// route. Le tunnel reprend sans se refermer.
    #[test]
    fn a_tunnel_whose_passeurs_fall_silent_gets_others() {
        let (h, login) = relay("tunnel-muet");
        let (_host, hl) = login("HoteMuet");
        let (_guest, _) = login("InviteMuet");
        minelib_net::fast::tunnel_asker("InviteMuet");
        let (a, _) = login("PasMuet1");
        let (b, _) = login("PasMuet2");
        a.send(Cmd::Passeur(true));
        b.send(Cmd::Passeur(true));
        until(|| h.state.lock().unwrap().fast.verified() == 2);
        let host_node = hosted(&h, &hl);
        let guest_node = minelib_p2p::Node::start_offline().unwrap();
        let t = guest_node.tunnel(host_node.id()).unwrap();
        let mut c = std::net::TcpStream::connect(t).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
        assert!(said_back(&mut c, b"avant"), "le tunnel ne passe pas");
        // Deux autres passeurs arrivent ; les deux premiers lâchent la route sans rien
        // dire au relais.
        let (c2, _) = login("PasMuet3");
        let (d2, _) = login("PasMuet4");
        c2.send(Cmd::Passeur(true));
        d2.send(Cmd::Passeur(true));
        until(|| h.state.lock().unwrap().fast.verified() == 4);
        let (route, before) = {
            let st = h.state.lock().unwrap();
            let (r, t) = st.fast.tunnels.iter().next().unwrap();
            let before: Vec<u64> = t.passeurs.iter().map(|(p, _)| *p).collect();
            for p in &before {
                transport::push(
                    &st.conns[p].tx,
                    &Response::PasseurTask {
                        lot: *r,
                        routes: Vec::new(),
                    },
                );
            }
            (*r, before)
        };
        let fell = Instant::now();
        assert!(said_back(&mut c, b"apres"), "le tunnel s'est refermé");
        eprintln!("TUNNEL : passeurs muets, reprise en {:?}", fell.elapsed());
        assert!(fell.elapsed() < Duration::from_secs(14));
        let st = h.state.lock().unwrap();
        let t = &st.fast.tunnels[&route];
        assert!(t.passeurs.len() == 2 && t.passeurs.iter().all(|(p, _)| !before.contains(p)));
    }

    /// (v301, v308) Un des deux passeurs d'un tunnel s'en va : le relais refait la chaîne
    /// avec un autre, sur la même route, et le tunnel continue sans se refermer.
    #[test]
    fn a_tunnel_gets_another_passeur_when_one_of_its_own_leaves() {
        let (h, login) = relay("tunnel-suite");
        let (_host, hl) = login("HoteSuite");
        let (_guest, _) = login("InviteSuite");
        minelib_net::fast::tunnel_asker("InviteSuite");
        let (first, _) = login("PasSuite1");
        let (second, _) = login("PasSuite2");
        first.send(Cmd::Passeur(true));
        second.send(Cmd::Passeur(true));
        until(|| h.state.lock().unwrap().fast.verified() == 2);
        let host_node = hosted(&h, &hl);
        let guest_node = minelib_p2p::Node::start_offline().unwrap();
        let t = guest_node.tunnel(host_node.id()).unwrap();
        let mut c = std::net::TcpStream::connect(t).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(25))).unwrap();
        assert!(said_back(&mut c, b"avant"), "le tunnel ne passe pas");
        let (route, gone) = {
            let st = h.state.lock().unwrap();
            let (r, t) = st.fast.tunnels.iter().next().unwrap();
            (*r, t.passeurs[0].0)
        };
        // Un troisième passeur arrive, le premier de la chaîne s'en va.
        let (third, _) = login("PasSuite3");
        third.send(Cmd::Passeur(true));
        until(|| h.state.lock().unwrap().fast.verified() == 3);
        let leaver = if h.state.lock().unwrap().conns[&gone].pseudo.as_deref() == Some("PasSuite1")
        {
            &first
        } else {
            &second
        };
        leaver.send(Cmd::Passeur(false));
        until(|| h.state.lock().unwrap().fast.verified() == 2);
        assert!(said_back(&mut c, b"apres"), "le tunnel s'est refermé");
        let st = h.state.lock().unwrap();
        let t = &st.fast.tunnels[&route];
        assert!(t.passeurs.len() == 2 && t.passeurs.iter().all(|(p, _)| *p != gone));
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

    /// (v274) L'hôte signale un passeur muet ou lent pour sa partie : le relais l'écarte et
    /// en donne un autre. Quand il les a tous signalés, on reprend les mêmes plutôt que de
    /// fermer la voie rapide.
    #[test]
    fn a_passeur_reported_by_the_host_is_replaced() {
        let dir = std::env::temp_dir().join(format!("minelib-passeurs-{}", rand::random::<u32>()));
        {
            let mut st = stats::Store::open(dir.join("stats")).unwrap();
            for g in ["Pas1", "Pas2", "Pas3", "Pas4", "Pas5", "Pas6"] {
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
            let link = match wait(&c, |e| matches!(e, Event::Link(_))) {
                Event::Link(l) => l,
                _ => unreachable!(),
            };
            wait(&c, |e| matches!(e, Event::LoggedIn { .. }));
            (c, link)
        };
        let (host, hl) = login("Hote");
        host.send(Cmd::Search {
            map_sig: [5; 32],
            map: "carte".into(),
            players: 10,
        });
        wait(&host, |e| matches!(e, Event::Hosted(_)));
        let (a, _) = login("Alice");
        a.send(Cmd::Search {
            map_sig: [5; 32],
            map: "carte".into(),
            players: 10,
        });
        wait(&a, |e| matches!(e, Event::Joined { .. }));
        let a_id = match wait(&host, |e| matches!(e, Event::PeerJoined { .. })) {
            Event::PeerJoined { peer, .. } => peer,
            _ => unreachable!(),
        };
        let ps: Vec<Client> = ["Pas1", "Pas2", "Pas3", "Pas4", "Pas5", "Pas6"]
            .iter()
            .map(|p| {
                let (c, _) = login(p);
                c.send(Cmd::Passeur(true));
                c
            })
            .collect();
        until(|| h.state.lock().unwrap().fast.verified() == 6);
        host.send(Cmd::FastWant(vec![vec![a_id]]));
        let first = fast(&host).unwrap().vias(a_id);
        assert_eq!(first.len(), 2);
        // (v308) Chaque voie est une chaîne de deux passeurs : le joueur et l'hôte
        // n'écrivent pas aux mêmes.
        let first_a = fast(&a).unwrap().vias(0);
        assert!(first_a.len() == 2 && first_a.iter().all(|v| !first.contains(v)));
        // La première voie est signalée : ses deux passeurs ne sont plus donnés, d'autres
        // les remplacent.
        std::thread::sleep(BAD_EVERY * 2);
        transport::push(
            &hl,
            &minelib_net::proto::Request::FastBad { peer: a_id, via: 0 },
        );
        let second = fast(&host).unwrap().vias(a_id);
        let second_a = fast(&a).unwrap().vias(0);
        assert_eq!((second.len(), second_a.len()), (2, 2));
        for gone in [first[0], first_a[0]] {
            assert!(
                !second.contains(&gone) && !second_a.contains(&gone),
                "{first:?} {first_a:?} puis {second:?} {second_a:?}"
            );
        }
        // Un signalement trop rapproché n'est pas suivi (pas d'attribution en rafale).
        transport::push(
            &hl,
            &minelib_net::proto::Request::FastBad { peer: a_id, via: 0 },
        );
        std::thread::sleep(Duration::from_millis(50));
        assert!(host.poll().is_none_or(|e| !matches!(e, Event::Fast(_))));
        // Les deux qui restent sont signalés à leur tour : il n'en resterait aucun, donc
        // on les reprend tous plutôt que de fermer la voie.
        for _ in 0..2 {
            std::thread::sleep(BAD_EVERY * 2);
            transport::push(
                &hl,
                &minelib_net::proto::Request::FastBad { peer: a_id, via: 0 },
            );
            assert!(!fast(&host).unwrap().vias(a_id).is_empty());
            let _ = fast(&a);
        }
        assert_eq!(h.state.lock().unwrap().fast.verified(), 6);
        drop(ps);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Un passeur qui ne répond plus aux sondes (PC en veille, port refermé, adresse qui a
    /// changé) n'est plus retenu : sa partie reçoit une autre attribution, sans lui.
    #[test]
    fn a_passeur_that_stops_answering_is_dropped() {
        let dir = std::env::temp_dir().join(format!("minelib-passeurs-{}", rand::random::<u32>()));
        {
            let mut st = stats::Store::open(dir.join("stats")).unwrap();
            for g in ["Pas1", "Pas2", "Pas3", "Pas4", "Pas5"] {
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
        let ps: Vec<Client> = ["Pas1", "Pas2", "Pas3", "Pas4", "Pas5"]
            .iter()
            .map(|p| {
                let c = login(p);
                c.send(Cmd::Passeur(true));
                c
            })
            .collect();
        until(|| h.state.lock().unwrap().fast.verified() == 5);
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
        // Trois sondes perdues : il n'est plus retenu, et la partie passe par les autres.
        until(|| h.state.lock().unwrap().fast.verified() == 4);
        let after = loop {
            let _ = fast(&host);
            let v = fast(&a).unwrap().vias(0);
            if !v.contains(&dead) {
                break v;
            }
        };
        // (v308) Les quatre qui restent font deux chaînes.
        assert_eq!(after.len(), 2);
        until(|| eh.open(a_id) && ea.open(0));
        // Les deux qui répondent restent vérifiés, sonde après sonde.
        std::thread::sleep(Duration::from_millis(1200));
        assert_eq!(h.state.lock().unwrap().fast.verified(), 4);
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
            for g in ["Pas1", "Pas2", "Pas3", "Pas4", "Pas5", "Pas6", "Pas7", "Pas8", "Novice"] {
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
        // Huit passeurs, et un compte trop neuf pour qu'on lui confie quoi que ce soit.
        let mut ps: Vec<Client> = ["Pas1", "Pas2", "Pas3", "Pas4", "Pas5", "Pas6", "Pas7", "Pas8", "Novice"]
            .iter()
            .map(|p| {
                let c = login(p);
                c.send(Cmd::Passeur(true));
                c
            })
            .collect();
        until(|| h.state.lock().unwrap().fast.verified() == 9);
        host.send(Cmd::FastWant(vec![vec![a_id], vec![b_id]]));
        let (eh, ea, eb) = (fast(&host).unwrap(), fast(&a).unwrap(), fast(&b).unwrap());
        // Deux voies par équipe, aucun passeur en commun.
        let (va, vb) = (ea.vias(0), eb.vias(0));
        assert_eq!((va.len(), vb.len()), (2, 2));
        assert!(va.iter().all(|v| !vb.contains(v)), "{va:?} {vb:?}");
        // (v308) Chaque voie est une chaîne de deux passeurs : l'hôte écrit aux seconds,
        // les joueurs aux premiers. Huit passeurs en tout, tous différents.
        let (ha, hb) = (eh.vias(a_id), eh.vias(b_id));
        assert_eq!((ha.len(), hb.len()), (2, 2));
        let mut all: Vec<_> = va.iter().chain(&vb).chain(&ha).chain(&hb).collect();
        all.sort();
        all.dedup();
        assert_eq!(all.len(), 8);
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
        // Un passeur s'en va : il en reste sept pour deux équipes, donc une chaîne chacune.
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
        // Il n'en reste que trois (en plus du compte trop neuf), pas de quoi faire une
        // chaîne par équipe : personne n'en a. (Les bouts
        // sont les mêmes d'une attribution à l'autre tant que la voie n'a pas été fermée.)
        ps[0].send(Cmd::Passeur(false));
        for _ in 0..3 {
            drop(ps.remove(1));
        }
        until(|| {
            let st = h.state.lock().unwrap();
            st.fast.verified() == 4 && st.fast.rooms.values().all(|a| a.passeurs.is_empty())
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
