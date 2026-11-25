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
//!
//! (v329) **Un tunnel peut aussi passer par un serveur TURN** (voir `minelib_p2p::turn`) :
//! quand le joueur (`TunnelWant::turn`) et l'hôte du nœud
//! (`Request::TurnAt`, ou ce relais lui-même pour ses propres nœuds) y ont chacun une
//! adresse relayée, chacun reçoit celle de l'autre comme un saut de plus de la route, après
//! ses passeurs, ou seul quand il n'y a pas de passeurs. Aucun des deux ne voit l'adresse
//! IP de l'autre ; le serveur TURN les voit toutes deux, comme un passeur seul, sans rien
//! lire.
//!
//! (v331) **La voie rapide des parties FPS aussi** : chaque route d'un joueur reçoit, en
//! plus de ses chaînes de passeurs, un chemin par TURN entre l'adresse relayée de son jeu
//! et celle de l'hôte (`fps_turn`), qui ne sert que quand aucun passeur ne passe (ou qu'il
//! est nettement plus rapide). Sans assez de passeurs, ce chemin est la voie rapide : avant,
//! tout passait alors par ce relais, par deux liens qui passent eux-mêmes par TURN quand il
//! n'y a rien d'autre (quatre passages par le serveur TURN au lieu de deux).

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::time::Instant;

use minelib_net::proto::{FastHop, FastTurn, FastWay, Request, Response};
use std::net::SocketAddr;
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
/// (v320) Hôte des tunnels vers le nœud de ce relais lui-même : le lien d'un joueur avec
/// lui, par des passeurs. Ce n'est pas une connexion : le relais tient ce bout de la route.
pub(crate) const OWN: u64 = u64::MAX;
/// (v320) Chaînes d'un lien vers ce relais, au plus : deux, sans passeur commun (quatre en
/// tout). Les paquets en prennent une ; si un passeur tombe, chaque bout passe sur l'autre
/// en deux secondes, sans qu'iroh s'en aperçoive, et la chaîne perdue est remplacée en
/// gardant l'autre telle quelle (même sceau) : c'est par elle qu'arrive la réparation.
const OWN_CHAINS: usize = 2;
/// Demandes de tunnel acceptées d'une connexion par minute.
const WANTS_PER_MIN: u16 = 240;
/// (v329) Adresses TURN acceptées d'une connexion par minute, et nœuds par connexion.
const TURN_ATS_PER_MIN: u8 = 30;
const TURN_NODES_PER_CONN: usize = 8;

/// (v301) Tunnel iroh d'un joueur vers un relais hébergé, porté par des passeurs.
struct Tunnel {
    node: [u8; 32],
    pseudo: String,
    /// Connexion du joueur (la dernière qui l'a demandé) et de l'hôte du nœud (`OWN` : ce
    /// relais lui-même). (v320) Et la machine du joueur : quand sa connexion se ferme, une
    /// autre du même compte sur la même machine reprend le tunnel.
    guest: u64,
    host: u64,
    guest_hw: Vec<[u8; 32]>,
    /// (v329) Adresse relayée du joueur chez un serveur TURN (redite à chaque demande), et
    /// le saut TURN donné : adresses relayées du joueur et de l'hôte, clé de son sceau
    /// (gardée tant qu'elles ne changent pas).
    guest_turn: Option<SocketAddrV4>,
    turn: Option<(SocketAddrV4, SocketAddrV4, [u8; 32])>,
    /// Passeurs retenus (connexion, clé du sceau de la route chez lui). (v308) Toujours
    /// deux, en chaîne : le premier du côté du joueur, le second du côté de l'hôte. (v320)
    /// Lien vers ce relais : une ou deux chaînes ainsi, l'une après l'autre (chacune son
    /// sceau).
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
    /// (v331) Routes données avec un chemin par TURN : le joueur, et ce chemin vu de lui.
    /// Sans passeur (`passeurs` vide), ce sont les seules routes de la partie : redonnées
    /// telles quelles tant qu'elles ne changent pas.
    turned: Vec<(u64, FastTurn)>,
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
    /// (v320) Nœuds iroh qui servent ce relais (`set_own_node`), empreintes de sa machine
    /// (les passeurs de cette machine ne portent pas les liens vers lui), et dernière
    /// demande lue de ce que disent les bouts hôtes qu'il tient (`fast::tunnel_log`).
    own_nodes: Vec<[u8; 32]>,
    own_hw: Vec<[u8; 32]>,
    log_seen: u64,
    /// (v329) Adresses relayées (serveur TURN) des nœuds hébergés par chaque connexion,
    /// celles des nœuds de ce relais lui-même (vues au dernier battement), et les
    /// annonces récentes de chaque connexion.
    turns: HashMap<(u64, [u8; 32]), SocketAddrV4>,
    own_turns: HashMap<[u8; 32], SocketAddrV4>,
    turn_asks: HashMap<u64, (Instant, u8)>,
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

    /// (v329) Ce que ce relais dit aux jeux (`PasseursKnown`) : ses passeurs vérifiés, plus
    /// un s'il connaît un nœud joignable par TURN. Sans rien de tout cela, un jeu ouvre ses
    /// tunnels sans demander de route ni l'attendre.
    fn routable(&self) -> usize {
        // (Seuls comptent les nœuds que leur connexion héberge ici : un jeu dit aussi
        // l'adresse de son propre nœud, qui ne sert qu'à ses demandes.)
        let hosted = self
            .turns
            .keys()
            .any(|(c, n)| self.nodes.get(n) == Some(c));
        self.verified() + usize::from(hosted || !self.own_turns.is_empty())
    }

    /// (v329) Nœuds joignables par TURN, et tunnels qui ont un saut par TURN (relevé de
    /// santé, essais).
    pub(crate) fn turn_counts(&self) -> (usize, usize) {
        (
            self.turns.len() + self.own_turns.len(),
            self.tunnels.values().filter(|t| t.turn.is_some()).count(),
        )
    }

    /// (v331) Joueurs de parties FPS dont la route a un chemin par TURN, et ceux de ces
    /// parties qui n'ont que lui (pas assez de passeurs) : relevé de santé, essais.
    pub(crate) fn fps_turns(&self) -> (usize, usize) {
        let all = self.rooms.values().map(|a| a.turned.len()).sum();
        let only = self
            .rooms
            .values()
            .filter(|a| a.passeurs.is_empty())
            .map(|a| a.turned.len())
            .sum();
        (all, only)
    }

    /// (v320) Un passeur vérifié s'est-il annoncé sur ce port (essais) ?
    #[cfg(test)]
    pub(crate) fn offered_at(&self, port: u16) -> bool {
        self.offers.values().any(|o| o.verified && o.addr.port() == port)
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

/// (v329) Une adresse relayée (serveur TURN) qu'on peut donner à l'autre bout d'une route :
/// publique, sauf sur un relais qui n'écoute que cette machine (essais, jeu seul).
fn turn_ok(st: &State, a: &SocketAddrV4) -> bool {
    !super::machines_apart(st) || public(*a.ip())
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
    st.fast.turn_asks.remove(&id);
    let had_turns = st.fast.turns.keys().any(|(c, _)| *c == id);
    st.fast.turns.retain(|(c, _), _| *c != id);
    if had_turns {
        announce(st);
    }
    // (v320) Les tunnels qu'elle avait demandés passent à une autre connexion du même
    // compte sur la même machine, s'il y en a une : c'est ainsi que le jeu change de lien
    // avec ce relais (la nouvelle connexion, venue par les passeurs, entre ; puis l'ancienne,
    // venue par les serveurs d'iroh, se ferme). Sinon le tunnel attend d'être redemandé ou
    // repris, comme avant.
    let moved: Vec<(u64, u64)> = st
        .fast
        .tunnels
        .iter()
        .filter(|(_, t)| t.guest == id)
        .filter_map(|(r, t)| {
            let other = st.conns.iter().find(|(c, x)| {
                **c != id
                    && x.pseudo.as_deref() == Some(t.pseudo.as_str())
                    && minelib_net::hw::same_machine(&x.hw, &t.guest_hw)
            })?;
            Some((*r, *other.0))
        })
        .collect();
    for (route, other) in moved {
        if let Some(t) = st.fast.tunnels.get_mut(&route) {
            t.guest = other;
        }
    }
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
    own_reports(st);
    own_turns(st);
    probe_all(st);
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

/// Sondes des passeurs (voir `upkeep`). (v329 : à part, pour que les tunnels d'un relais
/// sans passeur, qui passent par TURN, soient repris eux aussi.)
fn probe_all(st: &mut State) {
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
}

/// (v329) Les adresses relayées des nœuds de ce relais lui-même (son client TURN les dit à
/// `minelib_net::fast`) : quand l'une change, les liens vers ce nœud reçoivent leur route
/// de nouveau.
fn own_turns(st: &mut State) {
    let mut changed = Vec::new();
    for node in st.fast.own_nodes.clone() {
        let now = minelib_net::fast::turn_addr(node);
        if st.fast.own_turns.get(&node).copied() != now {
            match now {
                Some(a) => st.fast.own_turns.insert(node, a),
                None => st.fast.own_turns.remove(&node),
            };
            changed.push(node);
        }
    }
    if changed.is_empty() {
        return;
    }
    let routes: Vec<u64> = st
        .fast
        .tunnels
        .iter()
        .filter(|(_, t)| t.host == OWN && changed.contains(&t.node))
        .map(|(r, _)| *r)
        .collect();
    for r in routes {
        tunnel_assign(st, r);
    }
    announce(st);
}

/// (v329) `TurnAt` : le nœud `node` hébergé par cette connexion a (ou n'a plus) une
/// adresse relayée chez un serveur TURN. Les tunnels vers lui reçoivent leur route de
/// nouveau, avec ou sans saut TURN.
pub(crate) fn turn_at(st: &mut State, id: u64, node: [u8; 32], turn: Option<SocketAddrV4>) {
    let a = st.fast.turn_asks.entry(id).or_insert((Instant::now(), 0));
    if a.0.elapsed() >= std::time::Duration::from_secs(60) {
        *a = (Instant::now(), 0);
    }
    if a.1 >= TURN_ATS_PER_MIN {
        st.fast.refused[1] += 1;
        return;
    }
    a.1 += 1;
    // (Une adresse de réseau local ne mène qu'à des machines d'ici.)
    let turn = turn.filter(|a| turn_ok(st, a));
    let key = (id, node);
    if st.fast.turns.get(&key).copied() == turn {
        return;
    }
    let before = conn_turns(st, id);
    match turn {
        Some(a) => {
            let mine = st.fast.turns.keys().filter(|(c, _)| *c == id).count();
            if !st.fast.turns.contains_key(&key) && mine >= TURN_NODES_PER_CONN {
                return;
            }
            st.fast.turns.insert(key, a);
        }
        None => {
            st.fast.turns.remove(&key);
        }
    }
    let routes: Vec<u64> = st
        .fast
        .tunnels
        .iter()
        .filter(|(_, t)| t.host == id && t.node == node)
        .map(|(r, _)| *r)
        .collect();
    for r in routes {
        tunnel_assign(st, r);
    }
    // (v331) Les parties FPS où joue cette connexion (ou qu'elle héberge) reçoivent leurs
    // routes de nouveau : avec ou sans chemin par TURN.
    if conn_turns(st, id) != before {
        let rooms: Vec<u64> = st
            .fast
            .rooms
            .iter()
            .filter(|(_, a)| a.host == id || a.groups.iter().flatten().any(|p| *p == id))
            .map(|(r, _)| *r)
            .collect();
        for rid in rooms {
            assign(st, rid);
        }
    }
    announce(st);
}

/// (v331) Adresses relayées que dit cette connexion (celles des nœuds de son jeu), dans
/// l'ordre de leurs nœuds.
fn conn_turns(st: &State, id: u64) -> Vec<SocketAddrV4> {
    let mut v: Vec<([u8; 32], SocketAddrV4)> = st
        .fast
        .turns
        .iter()
        .filter(|((c, _), _)| *c == id)
        .map(|((_, n), a)| (*n, *a))
        .collect();
    v.sort();
    v.into_iter().map(|(_, a)| a).collect()
}

/// (v329) Le saut TURN du tunnel `route`, s'il y en a un : les deux bouts ont chacun une
/// adresse relayée. La clé de son sceau reste la même tant que les deux adresses ne
/// changent pas (le saut redonné à chaque demande ne coupe rien). Ce qui va au joueur
/// (l'adresse de l'hôte), ce qui va à l'hôte (celle du joueur).
fn turn_hops(st: &mut State, route: u64) -> Option<(FastHop, FastHop)> {
    let pair = st.fast.tunnels.get(&route).and_then(|t| {
        let g = t.guest_turn?;
        let h = if t.host == OWN {
            st.fast.own_turns.get(&t.node).copied()?
        } else {
            st.fast.turns.get(&(t.host, t.node)).copied()?
        };
        (g != h).then_some((g, h))
    });
    let t = st.fast.tunnels.get_mut(&route)?;
    let Some((g, h)) = pair else {
        t.turn = None;
        return None;
    };
    let seal = match t.turn {
        Some((og, oh, seal)) if og == g && oh == h => seal,
        _ => rand::random(),
    };
    t.turn = Some((g, h, seal));
    let hop = |a: SocketAddrV4| FastHop {
        addr: a.ip().octets(),
        port: a.port(),
        seal,
    };
    Some((hop(h), hop(g)))
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

/// (v320) Le nœud iroh `node` sert ce relais (`Node::serve`) : ses joueurs, entrés par les
/// serveurs d'iroh, peuvent lui demander une route par des passeurs vers lui-même (leur
/// lien avec lui : comptes, mondes, cartes, parties FPS), et il en tient le bout hôte.
pub(crate) fn set_own_node(st: &mut State, node: [u8; 32]) {
    if !st.fast.own_nodes.contains(&node) {
        st.fast.own_nodes.push(node);
    }
    st.fast.own_hw = minelib_net::hw::fingerprints_for(&st.id.noise_public);
    // (Ce qui a été dit avant ne le concerne pas.)
    st.fast.log_seen = minelib_net::fast::tunnel_log(u64::MAX).1;
}

/// (v320) Ce que dit le bout hôte des liens vers ce relais, tenu ici même : une route où
/// plus rien ne passe par aucune de ses chaînes reçoit d'autres passeurs.
fn own_reports(st: &mut State) {
    if st.fast.own_nodes.is_empty() {
        return;
    }
    let (asks, last) = minelib_net::fast::tunnel_log(st.fast.log_seen);
    st.fast.log_seen = last;
    for ask in asks {
        if let minelib_net::fast::TunnelAsk::Bad(route) = ask {
            if st.fast.tunnels.get(&route).is_some_and(|t| t.host == OWN) {
                tunnel_bad(st, OWN, route);
            }
        }
    }
}

/// (v304) Cette connexion vient d'entrer : on lui dit s'il y a des passeurs (une fois).
pub(crate) fn greet(st: &mut State, id: u64) {
    if st.fast.told.insert(id) {
        if st.fast.said {
            if let Some(c) = st.conns.get(&id) {
                let n = st.fast.routable() as u32;
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
    let n = st.fast.routable() as u32;
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
        // (v329) Il a peut-être déjà dit son adresse relayée.
        announce(st);
    }
}

/// (v301) `TunnelWant` : un joueur ouvre, ou garde ouvert, un tunnel iroh vers le relais
/// du nœud `node`. S'il y a des passeurs, une route lui est donnée, à lui et à l'hôte du
/// nœud : leurs paquets y passent en UDP, sans que l'un voie l'adresse de l'autre, au lieu
/// des serveurs relais d'iroh. Le passeur ne sait ni qui ils sont ni ce qu'il porte (le
/// tunnel est chiffré de bout en bout).
pub(crate) fn tunnel_want(
    st: &mut State,
    id: u64,
    node: [u8; 32],
    turn: Option<SocketAddrV4>,
) {
    // (v329) Son adresse relayée : une adresse publique seulement.
    let turn = turn.filter(|a| turn_ok(st, a));
    // (v320) Vers ce relais lui-même : il tient le bout hôte (`OWN`).
    let own = st.fast.own_nodes.contains(&node);
    let host = if own {
        OWN
    } else {
        match st.fast.nodes.get(&node) {
            Some(&h) => h,
            None => return,
        }
    };
    let Some(pseudo) = st.conns.get(&id).and_then(|c| c.pseudo.clone()) else {
        return;
    };
    let Some(g) = st.conns.get(&id) else {
        return;
    };
    let guest_hw = g.hw.clone();
    if own {
        // (Le jeu de la machine du relais le joint sans tunnel.)
        if super::machines_apart(st) && minelib_net::hw::same_machine(&g.hw, &st.fast.own_hw) {
            return;
        }
    } else {
        let Some(h) = st.conns.get(&host) else {
            return;
        };
        // Vers son propre relais : pas besoin de tunnel.
        if host == id || (super::machines_apart(st) && super::same_machine(g, h)) {
            return;
        }
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
            let mut moved = false;
            if let Some(t) = st.fast.tunnels.get_mut(&route) {
                t.renewed = Instant::now();
                t.guest = id;
                // (v329) Son adresse relayée a changé (ou il en a une, ou plus) : le saut TURN
                // est redonné.
                moved = t.guest_turn != turn;
                t.guest_turn = turn;
            }
            // (v320) Un lien vers ce relais : sa route est redite à chaque demande (celle-ci
            // peut venir d'une connexion neuve, entrée par ce lien même), et complétée d'une
            // seconde chaîne si des passeurs sont arrivés depuis.
            if own {
                own_assign(st, route);
            } else if moved {
                tunnel_assign(st, route);
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
            guest_hw,
            guest_turn: turn,
            turn: None,
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
    if t.host == OWN {
        own_assign(st, route);
        return;
    }
    let (guest, host, node) = (t.guest, t.host, t.node);
    let bad = t.bad.clone();
    let before: Vec<u64> = t.passeurs.iter().map(|(p, _)| *p).collect();
    // (v329) Donnée avant (par des passeurs ou par TURN) : sa reprise doit être dite.
    let given = !before.is_empty() || t.turn.is_some();
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
    // (v329) Le saut TURN, si les deux bouts ont une adresse relayée : avec les passeurs,
    // ou seul.
    let turn = turn_hops(st, route);
    if picked.is_empty() && turn.is_none() {
        st.fast.tunnels.remove(&route);
        // (Jamais donnée : rien à reprendre. Le joueur n'attend pas de réponse, d'autres
        // relais à qui il a fait la même demande ne connaissent même pas ce nœud.)
        if given {
            tunnel_tell(st, guest, host, node, route, &[], &[], None);
        }
        return;
    }
    if picked.is_empty() {
        if let Some(t) = st.fast.tunnels.get_mut(&route) {
            if !t.passeurs.is_empty() {
                t.passeurs.clear();
                t.at = Instant::now();
            }
        }
        tunnel_tell(st, guest, host, node, route, &[], &[], turn);
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
    tunnel_tell(
        st,
        guest,
        host,
        node,
        route,
        &[hop(first.1)],
        &[hop(second.1)],
        turn,
    );
}

/// (v320) Passeurs d'un lien vers ce relais (`OWN`) : les chaînes qui peuvent encore servir
/// sont gardées telles quelles (mêmes passeurs, même sceau), et complétées jusqu'à
/// `OWN_CHAINS` par des paires neuves, chacune son sceau. Garder la chaîne vivante compte
/// ici plus qu'ailleurs : la route est redite au joueur par son lien avec ce relais, qui
/// passe justement par elle.
fn own_assign(st: &mut State, route: u64) {
    let Some(t) = st.fast.tunnels.get(&route) else {
        return;
    };
    let (guest, node) = (t.guest, t.node);
    let bad = t.bad.clone();
    let before = t.passeurs.clone();
    let given = !before.is_empty() || t.turn.is_some();
    let guest_hw = t.guest_hw.clone();
    let apart = super::machines_apart(st);
    // Un passeur sert : vérifié, toujours là, pas signalé pour ce lien, ni le joueur, ni
    // sur sa machine ou sur celle de ce relais (il verrait les deux bouts).
    let ok = |p: &u64| -> Option<SocketAddrV4> {
        let o = st.fast.offers.get(p)?;
        let c = st.conns.get(p)?;
        if !o.verified || c.kicked || c.garant_banned || super::link_bad(c) {
            return None;
        }
        if *p == guest || bad.contains(p) {
            return None;
        }
        if apart
            && (minelib_net::hw::same_machine(&c.hw, &guest_hw)
                || minelib_net::hw::same_machine(&c.hw, &st.fast.own_hw))
        {
            return None;
        }
        Some(o.addr)
    };
    // Les chaînes gardées.
    let mut chains: Vec<[(u64, SocketAddrV4, [u8; 32]); 2]> = Vec::new();
    for c in before.chunks(2) {
        if let [a, b] = c {
            if let (Some(x), Some(y)) = (ok(&a.0), ok(&b.0)) {
                if chains.len() < OWN_CHAINS {
                    chains.push([(a.0, x, a.1), (b.0, y, b.1)]);
                }
            }
        }
    }
    let kept = chains.len();
    // Des paires neuves : passeurs libres, chacun sur sa propre adresse (deux passeurs d'une
    // même maison pourraient s'entendre), les moins chargés et les plus proches d'abord.
    let giver = st.fast.offers.get(&guest).is_some_and(|o| o.verified);
    let room = if giver {
        TUNNEL_ROOM
    } else {
        TUNNEL_ROOM + TUNNEL_GIVERS
    };
    let used: Vec<u64> = chains.iter().flat_map(|c| c.iter().map(|x| x.0)).collect();
    let mut cands: Vec<(usize, f32, u64, SocketAddrV4)> = st
        .fast
        .offers
        .keys()
        .filter(|p| !used.contains(p))
        .filter_map(|p| {
            let addr = ok(p)?;
            let load = st.fast.load(*p);
            if load + room >= LOAD_MAX {
                return None;
            }
            let ms = super::link_ms(&st.conns[p]).unwrap_or(999.0);
            Some(((ms / 20.0) as usize * 1000 + load, ms, *p, addr))
        })
        .collect();
    cands.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then(a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
            .then(a.2.cmp(&b.2))
    });
    let mut ips: Vec<Ipv4Addr> = chains
        .iter()
        .flat_map(|c| c.iter().map(|x| *x.1.ip()))
        .collect();
    let mut fresh: Vec<(u64, SocketAddrV4)> = Vec::new();
    for (_, _, p, addr) in cands {
        if !apart || !ips.contains(addr.ip()) {
            ips.push(*addr.ip());
            fresh.push((p, addr));
        }
    }
    let mut fresh = fresh.into_iter();
    while chains.len() < OWN_CHAINS {
        let (Some(a), Some(b)) = (fresh.next(), fresh.next()) else {
            break;
        };
        let seal: [u8; 32] = rand::random();
        chains.push([(a.0, a.1, seal), (b.0, b.1, seal)]);
    }
    // Ceux qui ne le portent plus n'ont plus sa route.
    let now: Vec<u64> = chains.iter().flat_map(|c| c.iter().map(|x| x.0)).collect();
    for (p, _) in before.iter().filter(|(p, _)| !now.contains(p)) {
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
    // (v329) Le saut TURN, si le joueur et ce relais ont chacun une adresse relayée.
    let turn = turn_hops(st, route);
    if chains.is_empty() && turn.is_none() {
        st.fast.tunnels.remove(&route);
        if given {
            tunnel_tell(st, guest, OWN, node, route, &[], &[], None);
        }
        return;
    }
    // Les passeurs des chaînes neuves apprennent leur lien (les gardés l'ont déjà) : le
    // premier parle au joueur, le second à ce relais.
    for c in &chains[kept..] {
        let (first, second) = (c[0], c[1]);
        for (me, other, dir) in [
            (first, second, minelib_net::fast::TO_GUEST),
            (second, first, minelib_net::fast::TO_HOST),
        ] {
            if let Some(conn) = st.conns.get(&me.0) {
                transport::push(
                    &conn.tx,
                    &Response::PasseurLink {
                        lot: route,
                        route,
                        seal: me.2,
                        dir,
                        addr: other.1.ip().octets(),
                        port: other.1.port(),
                    },
                );
            }
        }
    }
    let hop = |a: SocketAddrV4, seal: [u8; 32]| FastHop {
        addr: a.ip().octets(),
        port: a.port(),
        seal,
    };
    let to_guest: Vec<FastHop> = chains.iter().map(|c| hop(c[0].1, c[0].2)).collect();
    let to_host: Vec<FastHop> = chains.iter().map(|c| hop(c[1].1, c[1].2)).collect();
    let passeurs: Vec<(u64, [u8; 32])> = chains
        .iter()
        .flat_map(|c| [(c[0].0, c[0].2), (c[1].0, c[1].2)])
        .collect();
    if let Some(t) = st.fast.tunnels.get_mut(&route) {
        if t.passeurs != passeurs {
            t.passeurs = passeurs;
            t.at = Instant::now();
        }
    }
    tunnel_tell(st, guest, OWN, node, route, &to_guest, &to_host, turn);
}

/// La route du tunnel (ou sa reprise : aucun passeur) est dite à ses deux bouts, l'hôte
/// d'abord (il doit s'être montré au passeur avant le premier paquet du joueur). Chaque
/// bout ne reçoit que les passeurs auxquels il écrit lui-même (`to_guest`, `to_host`).
#[allow(clippy::too_many_arguments)]
fn tunnel_tell(
    st: &State,
    guest: u64,
    host: u64,
    node: [u8; 32],
    route: u64,
    to_guest: &[FastHop],
    to_host: &[FastHop],
    turn: Option<(FastHop, FastHop)>,
) {
    let (turn_guest, turn_host) = match turn {
        Some((g, h)) => (Some(g), Some(h)),
        None => (None, None),
    };
    for (c, is_host, hops, turn) in [
        (host, true, to_host, turn_host),
        (guest, false, to_guest, turn_guest),
    ] {
        // (v320) Le bout hôte d'un lien vers ce relais : tenu ici même, par le nœud iroh
        // qui le sert (dans ce programme).
        if c == OWN {
            let vias = hops
                .iter()
                .map(|h| minelib_net::fast::Via {
                    addr: SocketAddr::from((h.addr, h.port)),
                    seal: h.seal,
                })
                .collect();
            minelib_net::fast::tunnel_given(
                minelib_net::fast::TunnelRoute {
                    node,
                    route,
                    host: true,
                    vias,
                    turn: turn.map(|h| h.via()),
                },
                false,
            );
            continue;
        }
        if let Some(c) = st.conns.get(&c) {
            transport::push(
                &c.tx,
                &Response::TunnelWay {
                    node,
                    route,
                    host: is_host,
                    own: false,
                    passeurs: hops.to_vec(),
                    turn,
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
    tunnel_tell(st, t.guest, t.host, t.node, route, &[], &[], None);
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
            turned: Vec::new(),
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
        // (v331) Pas assez de passeurs : la voie rapide passe par TURN, de l'hôte à chaque
        // joueur qui y a, comme lui, une adresse relayée (les autres passent par le relais).
        // Rien ne change : rien n'est redit (la voie n'est pas interrompue).
        let turned: Vec<(u64, (FastTurn, FastTurn))> = groups
            .iter()
            .flatten()
            .filter_map(|&p| Some((p, fps_turn(st, p, old.host)?)))
            .collect();
        let mine: Vec<(u64, FastTurn)> = turned.iter().map(|(p, (g, _))| (*p, *g)).collect();
        let same = old.passeurs.is_empty() && old.groups == groups && old.turned == mine;
        if same && !mine.is_empty() {
            st.fast.rooms.insert(rid, Assign { bad, ..old });
            return;
        }
        if turned.is_empty() {
            close(st, &old.groups, old.host);
        } else {
            let mut ways = Vec::new();
            for p in groups.iter().flatten() {
                let (route, key): (u64, [u8; 32]) = (rand::random(), rand::random());
                let t = turned.iter().find(|(q, _)| q == p).map(|(_, t)| *t);
                if let Some(c) = st.conns.get(p) {
                    transport::push(
                        &c.tx,
                        &Response::FastGuest {
                            route,
                            key,
                            passeurs: Vec::new(),
                            turn: t.map(|t| t.0),
                        },
                    );
                }
                if let Some((_, h)) = t {
                    ways.push(FastWay {
                        peer: *p,
                        route,
                        key,
                        passeurs: Vec::new(),
                        turn: Some(h),
                    });
                }
            }
            if let Some(c) = st.conns.get(&old.host) {
                transport::push(&c.tx, &Response::FastHost { ways });
            }
            log::info!(
                "voie rapide : partie {rid}, pas assez de passeurs, {} joueur(s) par TURN",
                turned.len()
            );
        }
        // La demande est gardée : dès qu'un passeur de plus est vérifié, on réessaie.
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
                turned: mine,
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
    let mut turned: Vec<(u64, FastTurn)> = Vec::new();
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
            // (v331) Et un chemin par TURN, de secours (ou plus rapide que les passeurs).
            let turn = fps_turn(st, peer, old.host);
            if let Some((g, _)) = turn {
                turned.push((peer, g));
            }
            guests.push((
                peer,
                Response::FastGuest {
                    route,
                    key,
                    passeurs: to_guest,
                    turn: turn.map(|t| t.0),
                },
            ));
            ways.push(FastWay {
                peer,
                route,
                key,
                passeurs: to_host,
                turn: turn.map(|t| t.1),
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
        "voie rapide : partie {rid}, {} équipe(s), {per} chaîne(s) de deux passeurs par équipe, \
         {} joueur(s) avec un chemin par TURN",
        groups.len(),
        turned.len()
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
            turned,
        },
    );
}

/// (v331) Le chemin par TURN entre le joueur `guest` et l'hôte `host` d'une partie FPS :
/// une adresse relayée de chacun (celles que leurs connexions disent, `TurnAt` : celles
/// des nœuds de leur jeu), deux différentes. Ce qui va au joueur (la sienne d'abord), et ce
/// qui va à l'hôte.
fn fps_turn(st: &State, guest: u64, host: u64) -> Option<(FastTurn, FastTurn)> {
    let (gs, hs) = (conn_turns(st, guest), conn_turns(st, host));
    let (g, h) = gs
        .iter()
        .find_map(|g| hs.iter().find(|h| *h != g).map(|h| (*g, *h)))?;
    let v4 = |a: SocketAddrV4| (a.ip().octets(), a.port());
    Some((
        FastTurn {
            mine: v4(g),
            peer: v4(h),
        },
        FastTurn {
            mine: v4(h),
            peer: v4(g),
        },
    ))
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
                    turn: None,
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
        until_within(Duration::from_secs(30), f);
    }

    fn until_within(limit: Duration, f: impl Fn() -> bool) {
        let t = Instant::now();
        while !f() {
            assert!(t.elapsed() < limit, "délai dépassé");
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

    /// (v331) Les essais qui donnent des adresses relayées aux nœuds de ce programme : un
    /// à la fois (les jeux d'essai disent au relais celles de tous les nœuds du programme).
    static TURN_TESTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// (v331) Sans passeur, une partie FPS a quand même sa voie rapide : par TURN, de l'hôte
    /// au joueur et retour, sans passer par le relais, et sans que l'un voie l'adresse de
    /// l'autre. Deux passeurs arrivent : la route passe par eux, TURN reste en secours.
    #[test]
    fn without_passeurs_a_fps_game_gets_its_fast_way_through_turn() {
        let _turn = TURN_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let server = minelib_p2p::turn::essai::MiniTurn::start(rt.handle());
        // Deux nœuds de ce programme ont une adresse relayée : les jeux d'essai (eux aussi
        // dans ce programme) les disent au relais.
        let nodes: Vec<minelib_p2p::Node> = (0..2)
            .map(|_| {
                let n = minelib_p2p::Node::start_offline().unwrap();
                n.passeurs().start_turn(vec![server.source()]);
                n
            })
            .collect();
        until(|| nodes.iter().all(|n| n.passeurs().turn_addr().is_some()));
        let dir = std::env::temp_dir().join(format!("minelib-fpsturn-{}", rand::random::<u32>()));
        {
            let mut st = stats::Store::open(dir.join("stats")).unwrap();
            for g in ["PasT1", "PasT2"] {
                st.get_mut(g).games = 5;
                st.save(g);
            }
        }
        crate::ONE_FPS_PER_MACHINE.store(false, std::sync::atomic::Ordering::Relaxed);
        let h = start(Config {
            dir,
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
        let host = login("HoteT");
        host.send(Cmd::Search {
            map_sig: [6; 32],
            map: "carte".into(),
            players: 10,
        });
        wait(&host, |e| matches!(e, Event::Hosted(_)));
        let a = login("AliceT");
        a.send(Cmd::Search {
            map_sig: [6; 32],
            map: "carte".into(),
            players: 10,
        });
        wait(&a, |e| matches!(e, Event::Joined { .. }));
        let a_id = match wait(&host, |e| matches!(e, Event::PeerJoined { .. })) {
            Event::PeerJoined { peer, .. } => peer,
            _ => unreachable!(),
        };
        until(|| {
            let st = h.state.lock().unwrap();
            st.rooms
                .values()
                .find(|r| r.kind == 0 && r.members.contains(&a_id))
                .is_some_and(|r| {
                    conn_turns(&st, r.host).len() >= 2 && conn_turns(&st, a_id).len() >= 2
                })
        });
        host.send(Cmd::FastWant(vec![vec![a_id]]));
        let eh = fast(&host).expect("voie rapide de l'hôte");
        let ea = fast(&a).expect("voie rapide du joueur");
        assert!(
            eh.vias(a_id).is_empty() && ea.vias(0).is_empty(),
            "aucun passeur"
        );
        let relayed: Vec<std::net::SocketAddr> = nodes
            .iter()
            .map(|n| std::net::SocketAddr::V4(n.passeurs().turn_addr().unwrap()))
            .collect();
        let (to_a, to_h) = (eh.turn_peer(a_id).unwrap(), ea.turn_peer(0).unwrap());
        assert!(relayed.contains(&to_a) && relayed.contains(&to_h) && to_a != to_h);
        assert_eq!(h.state.lock().unwrap().fast.fps_turns(), (1, 1));
        until(|| {
            ea.send_all(0, b"ouvre");
            eh.open(a_id) && ea.open(0)
        });
        while a.poll().is_some() {}
        while host.poll().is_some() {}
        for i in 0..100u8 {
            assert!(eh.send_one(a_id, &[0xEE, i]));
            assert!(ea.send_all(0, &[0xDD, i]));
            std::thread::sleep(Duration::from_millis(2));
        }
        let mut got = (0, 0);
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(3) && got != (100, 100) {
            while let Some(e) = a.poll() {
                if matches!(&e, Event::FromHost(d) if d.first() == Some(&0xEE)) {
                    got.0 += 1;
                }
            }
            while let Some(e) = host.poll() {
                if matches!(&e, Event::FromPeer { peer, data } if *peer == a_id && data.first() == Some(&0xDD))
                {
                    got.1 += 1;
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(got, (100, 100), "par TURN : (vers le joueur, vers l'hôte)");
        // Deux passeurs : la route passe par eux (une chaîne), TURN reste en secours.
        let ps: Vec<Client> = ["PasT1", "PasT2"]
            .iter()
            .map(|p| {
                let c = login(p);
                c.send(Cmd::Passeur(true));
                c
            })
            .collect();
        until(|| h.state.lock().unwrap().fast.verified() == 2);
        let eh = fast(&host).expect("voie rapide de l'hôte");
        let ea = fast(&a).expect("voie rapide du joueur");
        assert_eq!((eh.vias(a_id).len(), ea.vias(0).len()), (1, 1));
        assert!(eh.turn_peer(a_id).is_some() && ea.turn_peer(0).is_some());
        assert_eq!(h.state.lock().unwrap().fast.fps_turns(), (1, 0));
        until(|| eh.stats(a_id).is_some_and(|s| s.heard == 3));
        assert!(!eh.on_turn(a_id), "les passeurs d'abord");
        drop(ps);
    }

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

    /// Lignes du classement reçues (une demande, sa réponse) : le lien avec le relais sert.
    fn answered(c: &Client, within: Duration) -> bool {
        c.send(Cmd::Leaderboard { friends: false });
        let t = Instant::now();
        while t.elapsed() < within {
            match c.poll() {
                Some(Event::Leaderboard { .. }) => return true,
                Some(Event::Lost) | Some(Event::LoggedOut) => panic!("lien perdu"),
                _ => std::thread::sleep(Duration::from_millis(2)),
            }
        }
        false
    }

    /// (v320) Le lien d'un joueur avec ce relais lui-même, par des passeurs. Le relais est
    /// servi par un nœud iroh, comme dans le jeu ; le joueur y entre d'abord sans eux (dans
    /// le jeu : par les serveurs d'iroh), demande une route vers ce nœud, et ouvre par elle,
    /// depuis un nœud à part, une seconde connexion qui entre avec le même compte ; la
    /// première se ferme, le tunnel passe à la seconde. Avec quatre passeurs, la route a
    /// deux chaînes sans passeur commun, un sceau chacune. Le premier passeur de la chaîne
    /// en service tombe : le lien continue par l'autre sans se refermer, la chaîne perdue
    /// est retirée et l'autre gardée telle quelle ; un passeur de plus arrive : la route
    /// retrouve une seconde chaîne, la première toujours la même.
    #[test]
    fn a_player_reaches_this_relay_itself_through_two_chains_of_passeurs() {
        let (h, login) = relay("lien");
        let relay_node = minelib_p2p::Node::start_offline().unwrap();
        relay_node.serve(h.addr);
        h.set_own_node(relay_node.id());
        let mut pas: Vec<Option<Client>> = Vec::new();
        for i in 0..4 {
            let (c, _) = login(&format!("PasLien{i}"));
            c.send(Cmd::Passeur(true));
            pas.push(Some(c));
        }
        until(|| h.state.lock().unwrap().fast.verified() == 4);
        minelib_net::fast::tunnel_asker("MoiLien");
        let (first, _) = login("MoiLien");
        let mine = minelib_p2p::Node::start_offline().unwrap();
        let t = mine
            .tunnel_by_passeurs(relay_node.id(), Duration::from_secs(10))
            .expect("tunnel par les passeurs");
        let second = Client::spawn(t.to_string(), Some(h.public));
        second.send(Cmd::Login {
            pseudo: "MoiLien".into(),
            password: "motdepasse".into(),
            totp: None,
            backup: None,
            device: None,
        });
        wait(&second, |e| matches!(e, Event::LoggedIn { .. }));
        let (route, chains) = {
            let st = h.state.lock().unwrap();
            let (r, t) = st
                .fast
                .tunnels
                .iter()
                .find(|(_, t)| t.host == OWN)
                .expect("un lien vers ce relais");
            (*r, t.passeurs.clone())
        };
        assert_eq!(chains.len(), 4, "deux chaînes de deux");
        let ids: std::collections::HashSet<u64> = chains.iter().map(|x| x.0).collect();
        assert_eq!(ids.len(), 4, "aucun passeur commun");
        assert!(chains[0].1 == chains[1].1 && chains[2].1 == chains[3].1);
        assert_ne!(chains[0].1, chains[2].1, "un sceau par chaîne");
        assert_eq!(minelib_p2p::node_direct(relay_node.id()), Some(true));
        assert_eq!(relay_node.passeurs().routes(), 1, "le relais tient le bout hôte");
        // La première connexion se ferme : le tunnel passe à la seconde. (Sous la charge de
        // tous les essais à la fois, la fermeture peut se perdre : le relais ne la voit alors
        // qu'à l'expiration de la connexion, 30 s.)
        drop(first);
        until_within(Duration::from_secs(60), || {
            let st = h.state.lock().unwrap();
            let mine = st
                .conns
                .values()
                .filter(|c| c.pseudo.as_deref() == Some("MoiLien"))
                .count();
            mine == 1 && st.conns.contains_key(&st.fast.tunnels[&route].guest)
        });
        assert!(answered(&second, Duration::from_secs(10)), "le lien sert");
        // Le premier passeur de la chaîne en service tombe.
        let fallen = {
            let st = h.state.lock().unwrap();
            st.conns[&chains[0].0].pseudo.clone().unwrap()
        };
        let i: usize = fallen.trim_start_matches("PasLien").parse().unwrap();
        let before = mine.passeurs().via(route);
        let t0 = Instant::now();
        drop(pas[i].take());
        // La route du joueur change de passeur : dite par le relais, par l'autre chaîne.
        until(|| mine.passeurs().via(route) != before);
        println!("LIEN : le joueur change de passeur après {:?}", t0.elapsed());
        assert!(answered(&second, Duration::from_secs(10)), "le lien continue");
        let took = t0.elapsed();
        println!("LIEN : un passeur tombe, le lien répond de nouveau après {took:?}");
        until(|| {
            let st = h.state.lock().unwrap();
            st.fast.tunnels.get(&route).is_some_and(|t| t.passeurs == chains[2..4])
        });
        for _ in 0..20 {
            assert!(answered(&second, Duration::from_secs(5)));
        }
        // Un passeur de plus : la route retrouve une seconde chaîne à la demande suivante
        // (le jeu redemande sa route toutes les 30 s ; ici, tout de suite).
        let (c5, _) = login("PasLien4");
        c5.send(Cmd::Passeur(true));
        until(|| h.state.lock().unwrap().fast.verified() == 4);
        minelib_net::fast::tunnel_want(relay_node.id(), None);
        until(|| {
            let st = h.state.lock().unwrap();
            st.fast
                .tunnels
                .get(&route)
                .is_some_and(|t| t.passeurs.len() == 4 && t.passeurs[..2] == chains[2..4])
        });
        assert!(answered(&second, Duration::from_secs(10)));
    }

    /// (v320) Lien vers ce relais : deux ou trois passeurs donnent une chaîne, un seul n'en
    /// donne aucune (il verrait les deux bouts) ; le jeu de la machine du relais ne demande
    /// jamais de route vers lui.
    #[test]
    fn a_link_to_this_relay_takes_one_chain_with_two_or_three_passeurs() {
        let (h, login) = relay("lien-une");
        h.set_own_node([0x5a; 32]);
        let (_g, gl) = login("MoiUne");
        let (a, _) = login("PasUne0");
        a.send(Cmd::Passeur(true));
        until(|| h.state.lock().unwrap().fast.verified() == 1);
        transport::push(&gl, &Request::TunnelWant {
                node: [0x5a; 32],
                turn: None,
            });
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(h.state.lock().unwrap().fast.tunnels(), 0, "un passeur seul : rien");
        let (b, _) = login("PasUne1");
        let (c, _) = login("PasUne2");
        b.send(Cmd::Passeur(true));
        c.send(Cmd::Passeur(true));
        until(|| h.state.lock().unwrap().fast.verified() == 3);
        transport::push(&gl, &Request::TunnelWant {
                node: [0x5a; 32],
                turn: None,
            });
        until(|| h.state.lock().unwrap().fast.tunnels() == 1);
        let st = h.state.lock().unwrap();
        let t = st.fast.tunnels.values().next().unwrap();
        assert_eq!((t.host, t.passeurs.len()), (OWN, 2));
        assert_eq!(t.passeurs[0].1, t.passeurs[1].1);
    }

    /// (v320) Débit d'un lien vers ce relais par une chaîne de passeurs (ici sur cette
    /// machine : ce qui est mesuré, c'est ce que le chemin laisse passer, pas un réseau) :
    /// un monde de 2 Mo, puis un flux de partie FPS (256 messages de 300 octets par seconde,
    /// ce que l'hôte envoie à un joueur). Par les serveurs d'iroh, 2 Mo prennent 43 s
    /// (mesuré depuis un PC, v317).
    #[test]
    fn a_link_through_passeurs_carries_a_world_and_a_256_tick_stream() {
        use std::io::{Read, Write};
        let (h, login) = relay("debit");
        // Un nœud que ce relais tient pour le sien, servant un puits qui répond à chaque
        // envoi : ici, ce n'est pas le relais (on mesure le chemin seul).
        let sink = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let sink_addr = sink.local_addr().unwrap();
        std::thread::spawn(move || {
            for c in sink.incoming().flatten() {
                std::thread::spawn(move || {
                    let mut c = c;
                    let mut head = [0u8; 5];
                    if c.read_exact(&mut head).is_err() {
                        return;
                    }
                    let mut who = vec![0u8; head[4] as usize];
                    let _ = c.read_exact(&mut who);
                    // Chaque bloc : 4 octets de longueur, puis le bloc ; réponse : 1 octet.
                    loop {
                        let mut n = [0u8; 4];
                        if c.read_exact(&mut n).is_err() {
                            return;
                        }
                        let mut buf = vec![0u8; u32::from_le_bytes(n) as usize];
                        if c.read_exact(&mut buf).is_err() || c.write_all(&[1]).is_err() {
                            return;
                        }
                    }
                });
            }
        });
        let node = minelib_p2p::Node::start_offline().unwrap();
        node.serve(sink_addr);
        h.set_own_node(node.id());
        let mut pas = Vec::new();
        for i in 0..2 {
            let (c, _) = login(&format!("PasDebit{i}"));
            c.send(Cmd::Passeur(true));
            pas.push(c);
        }
        until(|| h.state.lock().unwrap().fast.verified() == 2);
        minelib_net::fast::tunnel_asker("MoiDebit");
        let (_me, _) = login("MoiDebit");
        let mine = minelib_p2p::Node::start_offline().unwrap();
        let t = mine
            .tunnel_by_passeurs(node.id(), Duration::from_secs(10))
            .expect("tunnel par les passeurs");
        let mut c = std::net::TcpStream::connect(t).unwrap();
        c.set_nodelay(true).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
        let send = |c: &mut std::net::TcpStream, block: &[u8]| {
            c.write_all(&(block.len() as u32).to_le_bytes()).unwrap();
            c.write_all(block).unwrap();
        };
        // Un monde : 2 Mo en blocs de 64 Kio, puis l'accusé du dernier.
        let block = vec![7u8; 64 * 1024];
        let t0 = Instant::now();
        let n = 2_000_000 / block.len() + 1;
        for _ in 0..n {
            send(&mut c, &block);
        }
        let mut ack = vec![0u8; n];
        c.read_exact(&mut ack).unwrap();
        let took = t0.elapsed();
        let kbs = (n * block.len()) as f64 / 1024.0 / took.as_secs_f64();
        println!("DEBIT : {} octets en {:.2} s, {:.0} Ko/s", n * block.len(), took.as_secs_f64(), kbs);
        assert!(kbs > 400.0, "{kbs:.0} Ko/s");
        // (Le jeu redemande sa route toutes les 30 s ; ici, où le relais la reprend au bout
        // de 6 s, avant le flux.)
        minelib_net::fast::tunnel_want(node.id(), None);
        // Une partie FPS : 256 messages de 300 octets par seconde pendant 3 s.
        let msg = vec![3u8; 300];
        let mut late = Vec::new();
        let start = Instant::now();
        for i in 0..768u32 {
            let due = start + Duration::from_micros(i as u64 * 3906);
            if let Some(d) = due.checked_duration_since(Instant::now()) {
                std::thread::sleep(d);
            }
            let sent = Instant::now();
            send(&mut c, &msg);
            let mut a = [0u8; 1];
            c.read_exact(&mut a).unwrap();
            late.push(sent.elapsed());
        }
        late.sort();
        let (p50, p99) = (late[late.len() / 2], late[late.len() * 99 / 100]);
        println!(
            "DEBIT : 256 par seconde, aller-retour médian {:?}, 99 % sous {:?}, en {:.2} s",
            p50,
            p99,
            start.elapsed().as_secs_f64()
        );
        assert!(start.elapsed() < Duration::from_secs(5), "le flux a pris du retard");
    }

    /// (v329) Sans aucun passeur, un joueur ouvre un tunnel vers le relais qu'un autre
    /// héberge : le nœud de l'hôte (`TurnAt`) et celui du joueur (`TunnelWant::turn`) ont
    /// chacun une adresse chez un serveur TURN, le relais du monde donne à chacun celle de
    /// l'autre, et le tunnel passe par le serveur TURN, sans serveur d'iroh. Le relais se dit
    /// joignable tant que l'hôte a son adresse relayée ; l'hôte la perd : la route est
    /// reprise.
    #[test]
    fn without_passeurs_a_tunnel_goes_through_a_turn_server() {
        let _turn = TURN_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let server = minelib_p2p::turn::essai::MiniTurn::start(rt.handle());
        let (h, login) = relay("turn");
        let (_host, hl) = login("HoteTurn");
        let host_node = hosted(&h, &hl);
        let hid = host_node.id();
        assert_eq!(h.state.lock().unwrap().fast.routable(), 0, "ni passeur ni TURN");
        host_node.passeurs().start_turn(vec![server.source()]);
        until(|| {
            let st = h.state.lock().unwrap();
            st.fast
                .nodes
                .get(&hid)
                .is_some_and(|c| st.fast.turns.contains_key(&(*c, hid)))
        });
        assert_eq!(h.state.lock().unwrap().fast.routable(), 1, "joignable par TURN");
        minelib_net::fast::tunnel_asker("MoiTurn");
        let (_me, _) = login("MoiTurn");
        let mine = minelib_p2p::Node::start_offline().unwrap();
        mine.passeurs().start_turn(vec![server.source()]);
        until(|| mine.passeurs().turn_addr().is_some());
        let t = mine
            .tunnel_by_passeurs(hid, Duration::from_secs(10))
            .expect("tunnel par TURN");
        let mut c = std::net::TcpStream::connect(t).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
        for i in 0..300u32 {
            assert!(said_back(&mut c, &[i as u8; 5]), "envoi {i}");
        }
        {
            let st = h.state.lock().unwrap();
            let tun = st.fast.tunnels.values().find(|t| t.node == hid).unwrap();
            assert!(tun.passeurs.is_empty(), "aucun passeur");
            let (g, hh, _) = tun.turn.unwrap();
            assert_eq!(Some(g), mine.passeurs().turn_addr());
            assert_eq!(Some(hh), host_node.passeurs().turn_addr());
            assert_eq!(st.fast.turn_counts().1, 1);
        }
        let (sent, received) = mine.passeurs().turn_counts();
        assert!(sent > 20 && received > 20, "{sent} envoyés, {received} reçus");
        // L'hôte n'a plus d'adresse relayée : plus de saut TURN, la route est reprise.
        host_node.passeurs().stop_turn();
        until(|| {
            let st = h.state.lock().unwrap();
            st.fast.tunnels.values().all(|t| t.node != hid) && st.fast.routable() == 0
        });
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
                    turn: None,
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
            transport::push(&g1, &Request::TunnelWant {
                    node: node(i),
                    turn: None,
                });
        }
        until(|| h.state.lock().unwrap().fast.tunnels() == 12);
        {
            let st = h.state.lock().unwrap();
            let mut loads: Vec<usize> = st.fast.offers.keys().map(|p| st.fast.load(*p)).collect();
            loads.sort();
            assert_eq!(loads, vec![8, 8, 8]);
        }
        // Un autre joueur qui ne relaie pas : plus de place pour lui.
        transport::push(&g2, &Request::TunnelWant {
                node: node(12),
                turn: None,
            });
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(h.state.lock().unwrap().fast.tunnels(), 12);
        // Celui qui relaie : servi, par les deux autres passeurs.
        transport::push(&cl, &Request::TunnelWant {
                node: node(12),
                turn: None,
            });
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
        // (v331) Pas pendant qu'un essai donne des adresses relayées aux nœuds du programme.
        let _turn = TURN_TESTS.lock().unwrap_or_else(|e| e.into_inner());
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
        // (v331) Pas pendant qu'un essai donne des adresses relayées aux nœuds du programme.
        let _turn = TURN_TESTS.lock().unwrap_or_else(|e| e.into_inner());
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
        // (v331) Pas pendant qu'un essai donne des adresses relayées aux nœuds du programme.
        let _turn = TURN_TESTS.lock().unwrap_or_else(|e| e.into_inner());
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
