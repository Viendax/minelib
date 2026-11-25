//! (v270) Voie rapide des parties FPS : paquets UDP portés par des « passeurs ».
//!
//! Le canal du relais (TCP) livre tout, dans l'ordre : un seul paquet perdu retient tous
//! ceux qui le suivent jusqu'à son renvoi. Pour ce qui se périme en quelques millisecondes
//! (les entrées d'un invité, les positions que l'hôte envoie à chaque pas), la voie rapide
//! envoie des paquets UDP sans renvoi : le suivant remplace le perdu.
//!
//! Sans serveur, quelqu'un doit quand même recevoir ces paquets : un **passeur**, le jeu
//! d'un joueur sans lien avec la partie, choisi par le relais, dont le port UDP est
//! joignable. L'invité et l'hôte envoient tous deux vers lui (aucun perçage de box à
//! coordonner) et il transmet de l'un à l'autre. Qui voit quoi :
//!
//! - le passeur voit deux adresses IP et des paquets scellés : ni pseudo, ni partie, ni
//!   contenu ;
//! - l'invité et l'hôte ne voient que l'adresse du passeur, jamais celle de l'autre ;
//! - un joueur ne connaît que ses propres passeurs (ceux de son équipe).
//!
//! Chaque route (un invité et l'hôte) a jusqu'à deux passeurs. Les entrées partent par les
//! deux : la première arrivée compte, le doublon est jeté (`Window`). Les positions, plus
//! lourdes, alternent entre les passeurs qui répondent (`Endpoint::send_one`). Chaque bout
//! dit à l'autre, dans chacun de ses paquets, par quels passeurs il vient de recevoir : un
//! passeur muet est délaissé en moins d'une demi-seconde, et quand aucun ne répond la voie
//! est fermée (`Endpoint::open`), l'appelant repasse par le relais comme avant.
//!
//! Un passeur peut aussi être mal placé : par lui, les paquets arriveraient plus tard que
//! par le relais. Celui qui reçoit le voit (c'est au jeu de comparer) et le dit à l'autre
//! bout (`Endpoint::set_slow`) : `send_one` refuse alors, comme si la voie était fermée, et
//! seules de petites sondes passent encore (`send_any`) pour savoir quand ce n'est plus
//! vrai. La voie rapide ne doit jamais faire moins bien que le relais seul.
//!
//! (v274) Chaque bout mesure le temps d'aller-retour par chaque passeur (`FLAG_PING`,
//! `FLAG_PONG`, deux fois par seconde) : quand l'un est nettement plus rapide que l'autre,
//! `send_one` ne prend que lui au lieu d'alterner, et le jeu peut demander au relais de
//! remplacer un passeur lent ou muet (`Endpoint::paths`).
//!
//! Rien de ce qui doit arriver à coup sûr ne passe par ici : le canal du relais reste la
//! référence (les entrées y partent toujours aussi), la voie rapide ne fait qu'arriver plus
//! tôt.
//!
//! Paquet (tout en petit-boutiste) :
//!
//! | octets | contenu                                                                  |
//! |--------|--------------------------------------------------------------------------|
//! | 0      | `MAGIC`                                                                  |
//! | 1..9   | route                                                                    |
//! | 9      | sens (`TO_HOST`, `TO_GUEST`)                                             |
//! | 10..18 | numéro, croissant par route et par sens                                  |
//! | 18..26 | sceau du passeur : HMAC-SHA256 (tronqué) des octets 0..18                |
//! | 26..   | charge scellée de bout en bout (ChaCha20-Poly1305, l'en-tête authentifié) |
//!
//! La charge commence par un octet : par quels passeurs celui qui écrit vient de recevoir
//! (bits 0 et 1 ; (v331) bits 0 à 2 avec le chemin par TURN, qui vient après les
//! passeurs), `FLAG_SLOW` s'il reçoit plus vite par le relais, et `FLAG_KEEP` pour un
//! paquet d'entretien sans message.
//!
//! (v331) Une route peut aussi avoir un **chemin par TURN** : l'hôte et le joueur s'écrivent
//! par leurs adresses relayées chez un serveur TURN (voir `TurnLink`), sans relais au milieu
//! ni passeur. Il ne sert que quand aucun passeur ne passe, ou qu'il est nettement plus
//! rapide qu'eux : c'est le seul chemin qui coûte (quota mensuel du serveur).

use std::collections::HashMap;
use std::net::{SocketAddr, SocketAddrV4, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use hmac::{Hmac, Mac};
use sha2::Sha256;

/// Premier octet d'un paquet de la voie rapide.
pub const MAGIC: u8 = 0xA7;
/// Premier octet d'une sonde du relais (suivi d'un numéro sur 8 octets) : le passeur qui la
/// reçoit le dit au relais par son canal habituel, ce qui prouve que son port est joignable.
pub const MAGIC_PROBE: u8 = 0xA8;
/// Sens d'un paquet : de l'invité vers l'hôte, de l'hôte vers l'invité.
pub const TO_HOST: u8 = 0;
pub const TO_GUEST: u8 = 1;
/// Message le plus long (octets) : au-delà, le paquet risquerait d'être découpé en route.
pub const MAX_MSG: usize = 1150;
/// Passeurs d'une route, au plus.
pub const VIAS: usize = 2;

const HEAD: usize = 18;
const SEAL: usize = 8;
const TAG: usize = 16;
/// Paquet d'entretien (aucun message : il garde le chemin ouvert et dit ce qu'on reçoit).
const FLAG_KEEP: u8 = 0x80;
/// Celui qui écrit reçoit plus vite par le relais que par les passeurs.
const FLAG_SLOW: u8 = 0x40;
/// Mesure de l'aller-retour par un passeur : un numéro suit, à renvoyer tel quel par le
/// même passeur (`FLAG_PONG`). Ni l'un ni l'autre ne porte de message.
const FLAG_PING: u8 = 0x20;
const FLAG_PONG: u8 = 0x10;
/// Une mesure d'aller-retour par chaque passeur toutes les…
const PING_EVERY: Duration = Duration::from_millis(500);
/// Écart d'aller-retour (ms) au-delà duquel le passeur le plus rapide est pris à chaque
/// fois, au lieu d'alterner.
const PREFER_MS: f32 = 4.0;
/// Un passeur par lequel rien n'est arrivé depuis ce temps ne compte plus.
const FRESH: Duration = Duration::from_millis(300);
/// (v331) Le chemin par TURN, quand il ne sert pas, n'est entretenu que par les mesures
/// d'aller-retour (une toutes les `PING_EVERY`, pas de paquet d'entretien toutes les
/// `KEEP` : chacun compte dans le quota) : il ne compte plus après deux mesures perdues.
const FRESH_TURN: Duration = Duration::from_millis(1200);
/// (v331) Le chemin par TURN est pris quand il est plus rapide que le meilleur passeur de
/// la route d'au moins cet écart d'aller-retour (ms) ; sinon seulement quand aucun passeur
/// ne passe.
const TURN_PREFER_MS: f32 = 25.0;
/// (v331) Bits de l'octet de tête qui disent par quels chemins on reçoit : les passeurs
/// (`VIAS` au plus), puis le chemin par TURN.
const HEARD: u8 = (1 << (VIAS + 1)) - 1;
/// Sans rien d'autre à envoyer par un passeur, un paquet d'entretien tous les…
const KEEP: Duration = Duration::from_millis(100);
/// Paquets par seconde qu'un passeur accepte pour une route (256 positions et quelques
/// centaines d'entrées par seconde au plus : au-delà, c'est un abus).
const RATE_MAX: u32 = 4000;
/// (v301) Octets par seconde, au plus, pour une route (les deux sens ensemble). Les
/// routes portent maintenant aussi les tunnels iroh, aux paquets bien plus gros : sans
/// cette limite, une seule route pourrait prendre tout le débit d'un passeur volontaire.
/// Ce qui dépasse est perdu, le tunnel ralentit de lui-même.
const BYTES_MAX: u32 = 1_000_000;
/// Routes qu'un passeur garde au plus.
pub const ROUTES_MAX: usize = 256;

fn head(route: u64, dir: u8, seq: u64) -> [u8; HEAD] {
    let mut h = [0u8; HEAD];
    h[0] = MAGIC;
    h[1..9].copy_from_slice(&route.to_le_bytes());
    h[9] = dir;
    h[10..18].copy_from_slice(&seq.to_le_bytes());
    h
}

/// Sceau du passeur : prouve que le paquet vient d'un bout de la route (le passeur ne peut
/// pas ouvrir la charge, mais il n'a pas à transmettre n'importe quoi).
fn hop_seal(key: &[u8; 32], head: &[u8]) -> [u8; SEAL] {
    let mut m = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("clé HMAC");
    m.update(head);
    let full = m.finalize().into_bytes();
    let mut out = [0u8; SEAL];
    out.copy_from_slice(&full[..SEAL]);
    out
}

/// (v301) Paquets « bruts » : même en-tête et même sceau de passeur, mais la charge n'est
/// pas scellée ici. Ils portent les paquets QUIC d'iroh (qui se chiffre et s'authentifie
/// de bout en bout lui-même) : voir `minelib_p2p::passeur`. Le passeur les transmet comme
/// les autres, sans rien y lire. Charge : genre (`RAW_KEEP` : rien, pour que le passeur
/// connaisse notre adresse ; `RAW_DATA` : un paquet), longueur sur 2 octets, contenu.
pub const RAW_KEEP: u8 = 0;
pub const RAW_DATA: u8 = 1;
/// (v304) « Réponds-moi » et sa réponse : un bout s'assure que la route passe de bout en
/// bout avant d'y ouvrir son tunnel.
pub const RAW_PING: u8 = 2;
pub const RAW_PONG: u8 = 3;
/// Plus gros contenu d'un paquet brut. Un paquet QUIC fait 1 200 octets au départ, puis
/// QUIC en essaie de plus gros : ils sont jetés ici, il en reste à 1 200. Avec l'en-tête,
/// le paquet tient ainsi dans 1 232 octets, ce que toute liaison laisse passer sans le
/// couper en deux.
pub const RAW_MAX: usize = 1203;

pub fn raw_pack(route: u64, dir: u8, seq: u64, seal: &[u8; 32], kind: u8, body: &[u8]) -> Vec<u8> {
    let h = head(route, dir, seq);
    let mut p = Vec::with_capacity(HEAD + SEAL + 3 + body.len().max(TAG));
    p.extend_from_slice(&h);
    p.extend_from_slice(&hop_seal(seal, &h));
    p.push(kind);
    p.extend_from_slice(&(body.len() as u16).to_le_bytes());
    p.extend_from_slice(body);
    // (Le passeur ne transmet rien de plus court qu'un paquet scellé vide.)
    while p.len() < HEAD + SEAL + TAG + 1 {
        p.push(0);
    }
    p
}

/// Paquet brut reçu : route, sens, genre et contenu ; `seal` donne la clé du sceau de la
/// route (rien si on ne la connaît pas). Rien si le paquet est mal fait ou mal scellé.
pub fn raw_open(
    p: &[u8],
    seal: impl Fn(u64) -> Option<[u8; 32]>,
) -> Option<(u64, u8, u8, &[u8])> {
    let x = parse(p)?;
    if !same(&hop_seal(&seal(x.route)?, x.head), x.seal) {
        return None;
    }
    let (kind, rest) = x.sealed.split_first()?;
    let len = u16::from_le_bytes(rest.get(..2)?.try_into().ok()?) as usize;
    Some((x.route, x.dir, *kind, rest.get(2..2 + len)?))
}

// ───────────────────────── Tunnels par les passeurs (v301) ─────────────────────────
//
// Le jeu joint les relais hébergés par d'autres joueurs (région, province, secours) par un
// tunnel iroh. Ce qui suit relie trois morceaux qui ne se connaissent pas : le nœud iroh
// du jeu (`minelib_p2p`), qui dit vers quels nœuds il ouvre un tunnel ; les connexions au
// relais du monde (`crate::client`), qui le lui demandent (`Request::TunnelWant`) et
// reçoivent les routes (`Response::TunnelWay`) ; et le passeur de ce jeu, s'il en a un.

/// Route d'un tunnel, donnée par le relais du monde : vers le nœud `node` (`host` faux),
/// ou vers un joueur qui joint notre nœud `node` (`host` vrai). Aucun passeur ni saut
/// TURN : fermée.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TunnelRoute {
    pub node: [u8; 32],
    pub route: u64,
    pub host: bool,
    pub vias: Vec<Via>,
    /// (v329) L'adresse relayée de l'autre bout chez un serveur TURN, et la clé du sceau
    /// de ce saut : nos paquets y vont par notre propre adresse relayée.
    pub turn: Option<Via>,
}

/// Demandes gardées (les connexions au relais les envoient chacune à leur tour).
const WANTS_KEPT: usize = 64;

/// Ce que le nœud iroh du jeu fait dire au relais du monde.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TunnelAsk {
    /// Une route vers ce nœud (demande, ou renouvellement), et (v329) notre adresse
    /// relayée chez un serveur TURN si on en a une.
    Want([u8; 32], Option<SocketAddrV4>),
    /// (v304) Plus rien ne passe par cette route depuis quelques secondes : qu'on lui
    /// donne d'autres passeurs.
    Bad(u64),
}

struct Tunnels {
    /// Demandes récentes : numéro (croissant), demande.
    wants: Vec<(u64, TunnelAsk)>,
    next: u64,
    /// Routes vers des nœuds distants (une par nœud, la dernière donnée).
    routes: Vec<TunnelRoute>,
    hook: Option<fn(&TunnelRoute)>,
    /// Passeurs de ce jeu : port local, et l'adresse que la box leur a donnée.
    own: Vec<(u16, Option<SocketAddrV4>)>,
    /// Essais : seuls ces comptes envoient les demandes (vide : tous).
    askers: Vec<String>,
}

static TUNNELS: Mutex<Tunnels> = Mutex::new(Tunnels {
    wants: Vec::new(),
    next: 1,
    routes: Vec::new(),
    hook: None,
    own: Vec::new(),
    askers: Vec::new(),
});

/// Ce jeu ouvre (ou garde ouvert) un tunnel vers le nœud `node` : ses connexions au relais
/// demandent une route par des passeurs. À refaire toutes les `TUNNEL_RENEW` tant que le
/// tunnel sert : le relais reprend les routes qu'on ne redemande plus. (v329) `turn` :
/// l'adresse relayée du nœud qui ouvre le tunnel, s'il en a une (le relais en fait un saut
/// de la route si l'autre bout en a une aussi).
pub fn tunnel_want(node: [u8; 32], turn: Option<SocketAddrV4>) {
    tunnel_ask(TunnelAsk::Want(node, turn));
}

/// (v304) Plus rien ne passe par la route `route` : ses bouts le disent au relais, qui
/// lui donne d'autres passeurs (même route : le tunnel reprend sans se refermer).
pub fn tunnel_bad(route: u64) {
    tunnel_ask(TunnelAsk::Bad(route));
}

fn tunnel_ask(ask: TunnelAsk) {
    let mut t = TUNNELS.lock().unwrap();
    let n = t.next;
    t.next += 1;
    t.wants.push((n, ask));
    let extra = t.wants.len().saturating_sub(WANTS_KEPT);
    t.wants.drain(..extra);
}

/// Rythme auquel redemander la route d'un tunnel qui sert.
pub const TUNNEL_RENEW: Duration = Duration::from_secs(30);

/// Demandes faites après la numéro `after` (sans doublon), et le numéro de la dernière,
/// pour la connexion du compte `pseudo`.
pub fn tunnel_wants(after: u64, pseudo: &str) -> (Vec<TunnelAsk>, u64) {
    let t = TUNNELS.lock().unwrap();
    let mut nodes: Vec<TunnelAsk> = Vec::new();
    if t.askers.is_empty() || t.askers.iter().any(|a| a == pseudo) {
        for (n, node) in &t.wants {
            if *n > after && !nodes.contains(node) {
                nodes.push(*node);
            }
        }
    }
    (nodes, t.next - 1)
}

/// (v320) Toutes les demandes faites après la numéro `after` (sans le tri des essais par
/// compte), et le numéro de la dernière : le relais y lit ce que dit le bout hôte des
/// routes vers son propre nœud, qu'il tient lui-même (une route qui ne passe plus).
pub fn tunnel_log(after: u64) -> (Vec<TunnelAsk>, u64) {
    let t = TUNNELS.lock().unwrap();
    let mut asks: Vec<TunnelAsk> = Vec::new();
    for (n, ask) in &t.wants {
        if *n > after && !asks.contains(ask) {
            asks.push(*ask);
        }
    }
    (asks, t.next - 1)
}

/// (v320) Oublie la route connue vers le nœud `node` (une route d'un tunnel fermé, qu'un
/// nouveau tunnel prendrait pour bonne avant que le relais ait répondu).
pub fn forget_route(node: [u8; 32]) {
    TUNNELS.lock().unwrap().routes.retain(|r| r.node != node);
}

/// Essais, où plusieurs jeux vivent dans un même programme : seules les connexions de
/// ces comptes envoient les demandes de tunnel (dans le jeu, un programme n'a qu'un
/// compte, et toutes ses connexions les envoient).
#[doc(hidden)]
pub fn tunnel_asker(pseudo: &str) {
    TUNNELS.lock().unwrap().askers.push(pseudo.to_string());
}

/// Le relais a donné (ou repris) une route. `own` : ses passeurs sont celui de ce jeu
/// même (on lui écrit sans sortir de la machine).
pub fn tunnel_given(mut r: TunnelRoute, own: bool) {
    let hook = {
        let mut t = TUNNELS.lock().unwrap();
        if own {
            // Notre propre passeur : on lui écrit sans sortir de la machine (passer par
            // la box pour se joindre soi-même ne marche pas partout). Si c'est celui d'un
            // autre jeu de cette machine, on ne le connaît pas : le tunnel garde les
            // serveurs d'iroh.
            let mine = &t.own;
            r.vias = r
                .vias
                .iter()
                .filter_map(|v| {
                    let (local, _) = mine.iter().find(|(local, ext)| {
                        v.addr.port() == *local || ext.map(SocketAddr::V4) == Some(v.addr)
                    })?;
                    Some(Via {
                        addr: SocketAddr::from(([127, 0, 0, 1], *local)),
                        seal: v.seal,
                    })
                })
                .collect();
        }
        if !r.host {
            if r.vias.is_empty() && r.turn.is_none() {
                t.routes.retain(|x| x.route != r.route);
            } else {
                t.routes.retain(|x| x.node != r.node);
                t.routes.push(r.clone());
            }
        }
        t.hook
    };
    if let Some(f) = hook {
        f(&r);
    }
}

/// Route connue vers le nœud `node`.
pub fn tunnel_route(node: [u8; 32]) -> Option<TunnelRoute> {
    let t = TUNNELS.lock().unwrap();
    t.routes.iter().find(|r| r.node == node).cloned()
}

/// (v329) Adresses relayées (serveur TURN) des nœuds de ce programme, et le numéro de leur
/// dernier changement : les connexions aux relais les disent (`Request::TurnAt`) quand il
/// change, et à chaque nouvelle connexion. Un nœud qui n'en a plus garde sa place (rien),
/// pour que les relais l'oublient aussi.
struct TurnAddrs {
    version: u64,
    nodes: Vec<([u8; 32], Option<SocketAddrV4>)>,
}

static TURN_ADDRS: Mutex<TurnAddrs> = Mutex::new(TurnAddrs {
    version: 0,
    nodes: Vec::new(),
});

/// Nœuds gardés au plus (les plus anciens s'en vont).
const TURN_NODES: usize = 32;

pub fn set_turn_addr(node: [u8; 32], addr: Option<SocketAddrV4>) {
    let mut t = TURN_ADDRS.lock().unwrap();
    match t.nodes.iter_mut().find(|(n, _)| *n == node) {
        Some((_, a)) if *a == addr => return,
        Some((_, a)) => *a = addr,
        None => t.nodes.push((node, addr)),
    }
    let extra = t.nodes.len().saturating_sub(TURN_NODES);
    t.nodes.drain(..extra);
    t.version += 1;
}

/// Adresse relayée du nœud `node` de ce programme.
pub fn turn_addr(node: [u8; 32]) -> Option<SocketAddrV4> {
    let t = TURN_ADDRS.lock().unwrap();
    t.nodes.iter().find(|(n, _)| *n == node).and_then(|(_, a)| *a)
}

/// Adresse relayée de chaque nœud (rien : il n'en a plus).
pub type TurnAddrList = Vec<([u8; 32], Option<SocketAddrV4>)>;

/// Toutes, si elles ont changé depuis le numéro `seen` (0 : jamais dites) : (numéro,
/// adresses).
pub fn turn_addrs(seen: u64) -> Option<(u64, TurnAddrList)> {
    let t = TURN_ADDRS.lock().unwrap();
    (t.version != seen && t.version > 0).then(|| (t.version, t.nodes.clone()))
}

// ─────────────────── (v331) Voie rapide des parties FPS par TURN ───────────────────
//
// Sans assez de passeurs, tout passait par le relais : l'hôte vers le relais, le relais
// vers le joueur, deux liens qui passent chacun par TURN quand il n'y a rien d'autre,
// soit quatre passages par le serveur TURN pour chaque paquet (deux allocations par
// lien). L'hôte et chaque joueur ont déjà une adresse relayée chez lui (celle de leur
// nœud iroh) : le relais leur donne celle de l'autre (`Way::turn`), et la route a un
// chemin par TURN de l'un à l'autre, sans relais au milieu : deux passages, et moins de
// délai. Aucun des deux ne voit l'adresse IP de l'autre.
//
// Ce chemin ne sert que quand aucun passeur de la route ne passe, ou qu'il est nettement
// plus rapide qu'eux (`TURN_PREFER_MS`) : chaque octet par TURN compte dans le quota du
// mois, les passeurs ne coûtent rien.

/// Un client TURN de ce programme (`minelib_p2p::turn`), vu de la voie rapide.
pub trait TurnLink: Send + Sync {
    /// Un paquet vers l'adresse relayée `to` (celle de l'autre bout), par notre allocation.
    fn send(&self, to: SocketAddr, packet: &[u8]);
}

/// Le client TURN de ce programme dont l'adresse relayée est celle-ci.
pub type TurnFind = fn(SocketAddrV4) -> Option<Arc<dyn TurnLink>>;

static TURN_FIND: OnceLock<TurnFind> = OnceLock::new();

/// Fourni une fois par le nœud iroh du jeu (ce module ne connaît pas son client TURN).
pub fn set_turn_find(f: TurnFind) {
    let _ = TURN_FIND.set(f);
}

/// Bouts de la voie rapide dont une route a un chemin par TURN : (route, sens des paquets
/// qu'ils y reçoivent) → le bout. (Dans les essais, l'hôte et le joueur d'une même route
/// vivent dans un même programme : c'est le sens qui les distingue.)
type TurnEnds = Vec<((u64, u8), std::sync::Weak<EndInner>)>;
static TURN_ENDS: Mutex<TurnEnds> = Mutex::new(Vec::new());

/// Un paquet venu par un client TURN de ce programme, de l'adresse relayée `from` : vrai
/// s'il était pour la voie rapide (il y est remis) ; sinon, à l'appelant de voir (les
/// tunnels iroh ont les mêmes en-têtes, mais d'autres numéros de route).
pub fn turn_received(from: SocketAddr, p: &[u8]) -> bool {
    let Some(x) = parse(p) else {
        return false;
    };
    let end = {
        let ends = TURN_ENDS.lock().unwrap();
        ends.iter()
            .find(|(k, _)| *k == (x.route, x.dir))
            .and_then(|(_, w)| w.upgrade())
    };
    match end {
        Some(e) => {
            e.on_packet(p, from, true);
            true
        }
        None => false,
    }
}


/// (v308) Le joueur a-t-il accepté que son jeu serve de passeur ? Un passeur montre son
/// adresse IP à ceux à qui il remet des paquets (un bout de route et un autre passeur) :
/// ce n'est plus fait d'office pour tout jeu qui peut être garant, seulement pour qui l'a
/// accepté (question posée une fois au menu d'accueil, paramètres avancés).
static PASSEUR_OK: AtomicBool = AtomicBool::new(false);

pub fn set_passeur_allowed(on: bool) {
    PASSEUR_OK.store(on, Ordering::Relaxed);
}

pub fn passeur_allowed() -> bool {
    PASSEUR_OK.load(Ordering::Relaxed)
}

/// (v304) Un relais auquel ce jeu est connecté a des passeurs en ce moment (redit par ses
/// connexions tant que c'est vrai) : sans cela, inutile de demander une route et de
/// l'attendre avant d'ouvrir un tunnel.
static PASSEURS_AT: AtomicU64 = AtomicU64::new(0);

fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

pub fn note_passeurs() {
    PASSEURS_AT.store(unix_ms(), Ordering::Relaxed);
}

pub fn passeurs_known() -> bool {
    unix_ms().saturating_sub(PASSEURS_AT.load(Ordering::Relaxed)) < 3000
}

/// (v317) Le relais du monde avait-il des passeurs la dernière fois que ce jeu lui a parlé,
/// il y a moins d'un quart d'heure ? (Le temps qu'il garde les routes d'un joueur qui ne
/// lui parle plus.)
pub fn passeurs_lately() -> bool {
    unix_ms().saturating_sub(PASSEURS_AT.load(Ordering::Relaxed)) < 900_000
}

/// (v317) Ce jeu est en veille au fond d'un pays : il a fermé son lien avec le relais du
/// monde (`set_dozing`, dit par le jeu). Un tunnel qui doit s'ouvrir à ce moment-là ne
/// peut pas demander de route : le jeu est prié de revenir au relais du monde
/// (`want_route`, relevé par `take_route_wanted`), le temps de la demander.
static DOZING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static ROUTE_WANTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_dozing(on: bool) {
    DOZING.store(on, Ordering::Relaxed);
    if !on {
        ROUTE_WANTED.store(false, Ordering::Relaxed);
    }
}

pub fn dozing() -> bool {
    DOZING.load(Ordering::Relaxed)
}

pub fn want_route() {
    ROUTE_WANTED.store(true, Ordering::Relaxed);
}

pub fn take_route_wanted() -> bool {
    ROUTE_WANTED.swap(false, Ordering::Relaxed)
}

/// Un passeur de ce jeu écoute sur le port `local` ; `external` : l'adresse que la box
/// lui a donnée, si elle a ouvert le port.
pub fn set_own_passeur(local: u16, external: Option<SocketAddrV4>) {
    let mut t = TUNNELS.lock().unwrap();
    t.own.retain(|(l, _)| *l != local);
    t.own.push((local, external));
    let extra = t.own.len().saturating_sub(64);
    t.own.drain(..extra);
}

/// À appeler à chaque route donnée ou reprise (le nœud iroh du jeu s'y branche).
pub fn set_tunnel_hook(f: fn(&TunnelRoute)) {
    TUNNELS.lock().unwrap().hook = Some(f);
}

fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn nonce(dir: u8, seq: u64) -> Nonce {
    let mut n = [0u8; 12];
    n[0] = dir;
    n[4..].copy_from_slice(&seq.to_le_bytes());
    *Nonce::from_slice(&n)
}

fn cipher(key: &[u8; 32]) -> ChaCha20Poly1305 {
    ChaCha20Poly1305::new(Key::from_slice(key))
}

/// Paquet lu, pas encore ouvert.
struct Parsed<'a> {
    route: u64,
    dir: u8,
    seq: u64,
    head: &'a [u8],
    seal: &'a [u8],
    sealed: &'a [u8],
}

fn parse(p: &[u8]) -> Option<Parsed<'_>> {
    if p.len() < HEAD + SEAL + TAG + 1 || p[0] != MAGIC || p[9] > TO_GUEST {
        return None;
    }
    Some(Parsed {
        route: u64::from_le_bytes(p[1..9].try_into().ok()?),
        dir: p[9],
        seq: u64::from_le_bytes(p[10..18].try_into().ok()?),
        head: &p[..HEAD],
        seal: &p[HEAD..HEAD + SEAL],
        sealed: &p[HEAD + SEAL..],
    })
}

/// Paquet prêt à partir par un passeur (`seal` : la clé de son sceau pour cette route).
fn pack(
    c: &ChaCha20Poly1305,
    route: u64,
    dir: u8,
    seq: u64,
    seal: &[u8; 32],
    body: &[u8],
) -> Vec<u8> {
    let h = head(route, dir, seq);
    let sealed = c
        .encrypt(&nonce(dir, seq), Payload { msg: body, aad: &h })
        .expect("scellement");
    let mut p = Vec::with_capacity(HEAD + SEAL + sealed.len());
    p.extend_from_slice(&h);
    p.extend_from_slice(&hop_seal(seal, &h));
    p.extend_from_slice(&sealed);
    p
}

/// Numéros déjà reçus sur une route : un paquet arrivé par deux passeurs, ou rejoué par
/// quelqu'un qui l'a copié en chemin, ne compte qu'une fois. Les 128 derniers numéros sont
/// retenus ; plus ancien, un paquet est refusé.
#[derive(Default, Clone, Copy, Debug)]
pub struct Window {
    /// Plus grand numéro reçu, plus un.
    top: u64,
    /// Bit `i` : le numéro `top - 1 - i` a été reçu.
    seen: u128,
}

impl Window {
    /// Vrai la première fois qu'un numéro arrive.
    pub fn accept(&mut self, seq: u64) -> bool {
        if seq >= self.top {
            let shift = seq + 1 - self.top;
            self.seen = if shift >= 128 { 0 } else { self.seen << shift };
            self.seen |= 1;
            self.top = seq + 1;
            return true;
        }
        let back = self.top - 1 - seq;
        if back >= 128 {
            return false;
        }
        let bit = 1u128 << back;
        if self.seen & bit != 0 {
            return false;
        }
        self.seen |= bit;
        true
    }
}

/// Sonde du relais vers le port annoncé par un passeur.
pub fn probe(sock: &UdpSocket, to: SocketAddr, nonce: u64) {
    let mut p = [0u8; 9];
    p[0] = MAGIC_PROBE;
    p[1..].copy_from_slice(&nonce.to_le_bytes());
    let _ = sock.send_to(&p, to);
}

/// Lecture d'un socket UDP qui rend la main régulièrement. (Sous Windows, un envoi vers un
/// port fermé fait échouer la lecture suivante : ce n'est pas une panne du socket.)
fn read(sock: &UdpSocket, buf: &mut [u8]) -> Option<(usize, SocketAddr)> {
    match sock.recv_from(buf) {
        Ok(r) => Some(r),
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            ) =>
        {
            None
        }
        Err(_) => {
            std::thread::sleep(Duration::from_millis(1));
            None
        }
    }
}

/// Ce qui demande à la box d'ouvrir un port UDP et rend l'adresse publique obtenue (IPv4,
/// port, qui peut différer du port demandé : avec une adresse partagée entre abonnés, la
/// box n'accorde que les ports de sa plage). Fourni une fois par le jeu (`set_mapper`) : ce
/// module n'a pas à connaître la bibliothèque qui parle aux box.
pub type Mapper = Box<dyn Fn(u16) -> Option<SocketAddrV4> + Send + Sync>;

static MAPPER: OnceLock<Mapper> = OnceLock::new();

/// (v297) Dernière sonde du relais reçue par le passeur de ce jeu (s, époque Unix ; 0 :
/// jamais). Une sonde reçue, c'est la preuve que la box laisse entrer l'UDP : ce PC sert de
/// passeur, et son adresse est déjà donnée aux joueurs des parties qu'il porte.
static PROBED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn now_s() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Le passeur de ce jeu vient de recevoir une sonde du relais.
pub fn note_probed() {
    PROBED.store(now_s(), std::sync::atomic::Ordering::Relaxed);
}

/// (v297) Ce jeu est un passeur vérifié en ce moment (sonde du relais reçue depuis moins
/// de 3 minutes ; le relais en renvoie une toutes les 30 s).
pub fn probed_recently() -> bool {
    let t = PROBED.load(std::sync::atomic::Ordering::Relaxed);
    t != 0 && now_s().saturating_sub(t) < 180
}

pub fn set_mapper(m: Mapper) {
    let _ = MAPPER.set(m);
}

/// Demande à la box d'ouvrir le port `port` (plusieurs secondes parfois ; rien sans
/// `set_mapper`, ou si la box refuse).
pub fn map_port(port: u16) -> Option<SocketAddrV4> {
    MAPPER.get().and_then(|m| m(port))
}

// ───────────────────────────── Passeur ─────────────────────────────

/// Ports que ce jeu a utilisés comme passeur et qui sont libres de nouveau.
static FREE_PORTS: Mutex<Vec<u16>> = Mutex::new(Vec::new());

/// Route portée par un passeur.
struct Hop {
    /// Attribution du relais dont elle fait partie (remplacée ou retirée d'un bloc).
    lot: u64,
    seal: [u8; 32],
    /// Adresse de celui qui envoie dans chaque sens (`TO_HOST` : l'invité).
    ends: [Option<SocketAddr>; 2],
    /// Plus grand numéro vu dans chaque sens, plus un : seul un paquet plus récent déplace
    /// une adresse (un vieux paquet copié en chemin ne détourne rien).
    top: [u64; 2],
    /// Paquets et octets portés depuis `since` (une seconde au plus).
    since: Instant,
    count: u32,
    bytes: u32,
    /// (v304) Chaîne de deux passeurs : dans ce sens, les paquets viennent d'un autre
    /// passeur, à une adresse fixée par le relais (elle ne s'apprend pas).
    fixed: [bool; 2],
}

struct PasseurInner {
    sock: UdpSocket,
    hops: Mutex<HashMap<u64, Hop>>,
    probes: Mutex<Sender<u64>>,
    stop: AtomicBool,
    carried: AtomicU64,
    /// (v307) Paquets refusés : route inconnue, sceau faux, trop pour une route, ou venus
    /// d'ailleurs que du bout connu. Beaucoup d'un coup : quelqu'un arrose ce port.
    refused: AtomicU64,
    /// Essais : un paquet sur tant est perdu (0 : aucun), et chaque paquet attend tant de
    /// millisecondes avant de repartir.
    lose: AtomicU64,
    delay: AtomicU64,
}

/// Passeur : transmet les paquets des routes que le relais lui confie, d'un bout à l'autre.
/// Il ne peut ni les lire ni en fabriquer (charge scellée de bout en bout) ; il peut
/// seulement en perdre ou en retarder, ce que le deuxième passeur de la route couvre.
pub struct Passeur {
    inner: Arc<PasseurInner>,
    port: u16,
}

impl Passeur {
    /// Comme `start`, sur un port que ce jeu a déjà utilisé comme passeur s'il y en a un de
    /// libre (la box l'a déjà ouvert : pas une redirection de plus à chaque fois), sinon
    /// sur n'importe lequel.
    pub fn start_any(probes: Sender<u64>) -> std::io::Result<Self> {
        let known: Vec<u16> = FREE_PORTS.lock().unwrap().clone();
        for port in known {
            if let Ok(sock) = UdpSocket::bind(("0.0.0.0", port)) {
                FREE_PORTS.lock().unwrap().retain(|p| *p != port);
                return Self::run(sock, probes);
            }
        }
        Self::start(0, probes)
    }

    /// Ouvre le port UDP `port` (0 : n'importe lequel) et commence à transmettre. Les
    /// numéros des sondes du relais reçues sont déposés dans `probes`.
    pub fn start(port: u16, probes: Sender<u64>) -> std::io::Result<Self> {
        Self::run(UdpSocket::bind(("0.0.0.0", port))?, probes)
    }

    fn run(sock: UdpSocket, probes: Sender<u64>) -> std::io::Result<Self> {
        sock.set_read_timeout(Some(Duration::from_millis(200)))?;
        let port = sock.local_addr()?.port();
        let inner = Arc::new(PasseurInner {
            sock,
            hops: Mutex::new(HashMap::new()),
            probes: Mutex::new(probes),
            stop: AtomicBool::new(false),
            carried: AtomicU64::new(0),
            refused: AtomicU64::new(0),
            lose: AtomicU64::new(0),
            delay: AtomicU64::new(0),
        });
        let run = inner.clone();
        std::thread::Builder::new()
            .name("passeur".into())
            .spawn(move || {
                let mut buf = [0u8; 2048];
                while !run.stop.load(Ordering::Relaxed) {
                    if let Some((n, from)) = read(&run.sock, &mut buf) {
                        run.on_packet(&buf[..n], from);
                    }
                }
            })?;
        Ok(Self { inner, port })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// Routes de l'attribution `lot` (route, clé du sceau) : elles remplacent celles qu'il
    /// portait pour elle. Liste vide : plus rien à porter pour cette attribution.
    pub fn set_lot(&self, lot: u64, routes: &[(u64, [u8; 32])]) {
        let mut hops = self.inner.hops.lock().unwrap();
        hops.retain(|_, h| h.lot != lot);
        for &(route, seal) in routes {
            if hops.len() >= ROUTES_MAX {
                break;
            }
            hops.insert(
                route,
                Hop {
                    lot,
                    seal,
                    ends: [None; 2],
                    top: [0; 2],
                    since: Instant::now(),
                    count: 0,
                    bytes: 0,
                    fixed: [false; 2],
                },
            );
        }
    }

    /// (v304) Route d'une chaîne de deux passeurs, dans l'attribution `lot` : les paquets du sens `dir` lui viennent de
    /// l'autre passeur, à l'adresse `from`, et c'est là qu'il envoie ceux de l'autre
    /// sens. Il ne voit ainsi qu'un des deux bouts de la route.
    pub fn set_link(&self, lot: u64, route: u64, seal: [u8; 32], dir: u8, from: SocketAddr) {
        let mut hops = self.inner.hops.lock().unwrap();
        // (v308) Elle s'ajoute aux autres routes de l'attribution (une partie FPS en a une
        // par joueur) ; une attribution se retire d'un bloc par `set_lot` vide.
        if (hops.len() >= ROUTES_MAX && !hops.contains_key(&route)) || dir > 1 {
            return;
        }
        let mut ends = [None; 2];
        let mut fixed = [false; 2];
        ends[dir as usize] = Some(from);
        fixed[dir as usize] = true;
        hops.insert(
            route,
            Hop {
                lot,
                seal,
                ends,
                top: [0; 2],
                since: Instant::now(),
                count: 0,
                bytes: 0,
                fixed,
            },
        );
    }

    /// Plus aucune route (connexion au relais perdue).
    pub fn clear(&self) {
        self.inner.hops.lock().unwrap().clear();
    }

    pub fn routes(&self) -> usize {
        self.inner.hops.lock().unwrap().len()
    }

    /// Paquets transmis depuis le début.
    pub fn carried(&self) -> u64 {
        self.inner.carried.load(Ordering::Relaxed)
    }

    /// (v307) Paquets refusés depuis le début (voir `PasseurInner::refused`).
    pub fn refused(&self) -> u64 {
        self.inner.refused.load(Ordering::Relaxed)
    }

    /// Essais : perd un paquet sur `n` (0 : aucun).
    #[doc(hidden)]
    pub fn lose_one_in(&self, n: u64) {
        self.inner.lose.store(n, Ordering::Relaxed);
    }

    /// Essais : retient chaque paquet `ms` millisecondes (un passeur mal placé).
    #[doc(hidden)]
    pub fn delay_ms(&self, ms: u64) {
        self.inner.delay.store(ms, Ordering::Relaxed);
    }
}

impl Drop for Passeur {
    fn drop(&mut self) {
        self.inner.stop.store(true, Ordering::Relaxed);
        // Le port resservira au prochain passeur de ce jeu (`start_any`).
        let mut free = FREE_PORTS.lock().unwrap();
        if !free.contains(&self.port) {
            free.push(self.port);
            if free.len() > 32 {
                free.remove(0);
            }
        }
    }
}

impl PasseurInner {
    fn on_packet(&self, p: &[u8], from: SocketAddr) {
        if p.len() == 9 && p[0] == MAGIC_PROBE {
            let nonce = u64::from_le_bytes(p[1..9].try_into().unwrap_or_default());
            let _ = self.probes.lock().unwrap().send(nonce);
            return;
        }
        let refuse = || {
            self.refused.fetch_add(1, Ordering::Relaxed);
        };
        let Some(x) = parse(p) else { return refuse() };
        let to = {
            let mut hops = self.hops.lock().unwrap();
            let Some(h) = hops.get_mut(&x.route) else {
                return refuse();
            };
            if !same(&hop_seal(&h.seal, x.head), x.seal) {
                return refuse();
            }
            if h.since.elapsed() >= Duration::from_secs(1) {
                h.since = Instant::now();
                h.count = 0;
                h.bytes = 0;
            }
            h.count += 1;
            h.bytes += p.len() as u32;
            if h.count > RATE_MAX || h.bytes > BYTES_MAX {
                return refuse();
            }
            let d = x.dir as usize;
            if h.fixed[d] {
                // (L'autre passeur de la chaîne : reconnu à son adresse, pas au port d'où
                // sa box fait sortir ses paquets.)
                if h.ends[d].map(|a| a.ip()) != Some(from.ip()) {
                    return refuse();
                }
            } else {
                if x.seq >= h.top[d] {
                    h.top[d] = x.seq + 1;
                    h.ends[d] = Some(from);
                }
                if h.ends[d] != Some(from) {
                    return refuse();
                }
            }
            h.ends[1 - d]
        };
        let Some(to) = to else { return };
        let n = self.carried.fetch_add(1, Ordering::Relaxed) + 1;
        let lose = self.lose.load(Ordering::Relaxed);
        if lose != 0 && n.is_multiple_of(lose) {
            return;
        }
        let delay = self.delay.load(Ordering::Relaxed);
        if delay != 0 {
            std::thread::sleep(Duration::from_millis(delay));
        }
        let _ = self.sock.send_to(p, to);
    }
}

// ───────────────────────────── Bouts d'une route ─────────────────────────────

/// Passeur d'une route, vu d'un de ses bouts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Via {
    pub addr: SocketAddr,
    /// Clé du sceau que ce passeur vérifie pour cette route.
    pub seal: [u8; 32],
}

/// (v331) Chemin par TURN d'une route : notre adresse relayée (le client TURN de ce
/// programme qui envoie) et celle de l'autre bout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TurnPair {
    pub mine: SocketAddrV4,
    pub peer: SocketAddrV4,
}

/// Route vers `peer` (0 : l'hôte, pour un invité) : son numéro, la clé qui scelle de bout
/// en bout, ses passeurs (dans le même ordre aux deux bouts), et (v331) son chemin par
/// TURN, s'il y en a un.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Way {
    pub peer: u64,
    pub route: u64,
    pub key: [u8; 32],
    pub vias: Vec<Via>,
    pub turn: Option<TurnPair>,
}

/// État d'une route, pour l'affichage et les essais.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Paquets envoyés (un message envoyé par deux passeurs compte deux fois).
    pub sent: u64,
    /// Messages reçus (une fois chacun).
    pub got: u64,
    /// Paquets arrivés en double ou rejoués, jetés.
    pub dup: u64,
    /// Chemins par lesquels l'autre bout dit recevoir en ce moment (bits 0 et 1 : les
    /// passeurs ; (v331) le chemin par TURN vient après eux).
    pub heard: u8,
}

struct LiveVia {
    addr: SocketAddr,
    seal: [u8; 32],
    rx_at: Option<Instant>,
    tx_at: Option<Instant>,
    /// Dernière mesure envoyée (numéro, quand), si sa réponse est encore attendue, et
    /// aller-retour lissé (ms).
    ping: (u8, Option<Instant>),
    ping_wait: bool,
    rtt: Option<f32>,
    /// (v331) Chemin par TURN (`addr` : l'adresse relayée de l'autre bout) : notre adresse
    /// relayée, notre client TURN une fois trouvé, et la dernière fois qu'on l'a cherché.
    turn: Option<SocketAddrV4>,
    link: Option<std::sync::Weak<dyn TurnLink>>,
    sought: Option<Instant>,
}

impl LiveVia {
    fn new(addr: SocketAddr, seal: [u8; 32], turn: Option<SocketAddrV4>) -> Self {
        LiveVia {
            addr,
            seal,
            rx_at: None,
            tx_at: None,
            ping: (0, None),
            ping_wait: false,
            rtt: None,
            turn,
            link: None,
            sought: None,
        }
    }

    /// Reçu par ce chemin il y a peu. (v331) Le chemin par TURN, en secours, n'est
    /// entretenu que par les mesures : on lui laisse `FRESH_TURN` ; quand il sert
    /// (`busy`), il est entretenu comme un passeur, et compte de la même façon.
    fn fresh(&self, now: Instant, busy: bool) -> bool {
        let keep = if self.turn.is_some() && !busy {
            FRESH_TURN
        } else {
            FRESH
        };
        self.rx_at.is_some_and(|t| now.duration_since(t) < keep)
    }

    /// (v331) Notre client TURN pour ce chemin (cherché au plus deux fois par seconde
    /// quand il manque : allocation pas encore là, ou perdue).
    fn turn_link(&mut self, now: Instant) -> Option<Arc<dyn TurnLink>> {
        let mine = self.turn?;
        if let Some(l) = self.link.as_ref().and_then(|w| w.upgrade()) {
            return Some(l);
        }
        if self
            .sought
            .is_some_and(|t| now.duration_since(t) < Duration::from_millis(500))
        {
            return None;
        }
        self.sought = Some(now);
        let l = TURN_FIND.get()?(mine)?;
        self.link = Some(Arc::downgrade(&l));
        Some(l)
    }
}

/// Un chemin d'une route, vu d'un bout : l'autre bout reçoit-il par lui, le temps
/// d'aller-retour par lui (ms), s'il a été mesuré, et (v331) est-ce le chemin par TURN
/// (sinon un passeur) ?
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PathInfo {
    pub open: bool,
    pub rtt_ms: Option<f32>,
    pub turn: bool,
}

struct Live {
    route: u64,
    cipher: ChaCha20Poly1305,
    vias: Vec<LiveVia>,
    seq: u64,
    win: Window,
    /// Dernier passeur pris par `send_one`.
    alt: usize,
    /// Ce que l'autre bout dit recevoir, et quand il l'a dit.
    heard: u8,
    heard_at: Option<Instant>,
    /// L'autre bout dit recevoir plus vite par le relais ; on le lui dit de notre côté.
    slow_in: bool,
    slow_out: bool,
    stats: Stats,
}

impl Live {
    /// Chemins par lesquels on vient de recevoir (à dire à l'autre bout).
    fn mask(&self, now: Instant) -> u8 {
        let busy = self.turn_now(self.open(now)).is_some_and(|(_, take)| take);
        self.vias
            .iter()
            .enumerate()
            .filter(|(_, v)| v.fresh(now, busy))
            .fold(0, |m, (i, _)| m | (1 << i))
    }

    /// (v331) Le chemin par TURN, s'il y en a un : son rang, et s'il faut le prendre en ce
    /// moment (`open` : les chemins par lesquels l'autre bout reçoit) plutôt que les
    /// passeurs : aucun passeur ne passe, ou il passe lui aussi et il est plus rapide que
    /// le meilleur d'entre eux d'au moins `TURN_PREFER_MS`.
    fn turn_now(&self, open: u8) -> Option<(usize, bool)> {
        let t = self.vias.iter().position(|v| v.turn.is_some())?;
        let passeurs = (0..self.vias.len()).filter(|&i| i != t && open & (1 << i) != 0);
        let mut any = false;
        let mut best: Option<f32> = None;
        for i in passeurs {
            any = true;
            if let Some(ms) = self.vias[i].rtt {
                best = Some(best.map_or(ms, |b: f32| b.min(ms)));
            }
        }
        let faster = open & (1 << t) != 0
            && matches!(
                (self.vias[t].rtt, best),
                (Some(turn), Some(best)) if turn + TURN_PREFER_MS < best
            );
        Some((t, !any || faster))
    }

    /// Le chemin d'un message qui part par un seul (`send_one`) : parmi ceux par lesquels
    /// l'autre bout reçoit. (v331) Le chemin par TURN quand il le faut (`turn_now`) ;
    /// sinon les passeurs, en alternant, ou le nettement plus rapide.
    fn pick(&mut self, open: u8) -> Option<usize> {
        let n = self.vias.len();
        let turn = self.turn_now(open);
        if let Some((t, true)) = turn {
            return (open & (1 << t) != 0).then_some(t);
        }
        let skip = turn.map(|(t, _)| t);
        let ok = |i: usize| open & (1 << i) != 0 && Some(i) != skip;
        let next = (1..=n).map(|k| (self.alt + k) % n).find(|&i| ok(i))?;
        // Un passeur nettement plus rapide que les autres est pris à chaque fois ; sinon
        // on alterne (deux chemins valent mieux qu'un quand l'un perd des paquets).
        let fastest = (0..n)
            .filter(|&i| ok(i))
            .filter_map(|i| Some((i, self.vias[i].rtt?)))
            .min_by(|a, b| a.1.total_cmp(&b.1));
        let i = match (fastest, self.vias[next].rtt) {
            (Some((best, ms)), Some(mine)) if mine > ms + PREFER_MS => best,
            _ => next,
        };
        self.alt = i;
        Some(i)
    }

    /// Passeurs par lesquels l'autre bout reçoit en ce moment.
    fn open(&self, now: Instant) -> u8 {
        match self.heard_at {
            Some(t) if now.duration_since(t) < FRESH => self.heard,
            _ => 0,
        }
    }

    fn packet(
        &mut self,
        dir: u8,
        i: usize,
        seq: u64,
        flags: u8,
        msg: &[u8],
        now: Instant,
    ) -> Vec<u8> {
        let mut body = Vec::with_capacity(1 + msg.len());
        let slow = if self.slow_out { FLAG_SLOW } else { 0 };
        body.push(self.mask(now) | flags | slow);
        body.extend_from_slice(msg);
        self.vias[i].tx_at = Some(now);
        self.stats.sent += 1;
        pack(
            &self.cipher,
            self.route,
            dir,
            seq,
            &self.vias[i].seal,
            &body,
        )
    }

    /// Un paquet par le chemin `i` : la socket de ce bout vers un passeur, ou (v331) notre
    /// client TURN vers l'adresse relayée de l'autre bout.
    fn out(&mut self, sock: &UdpSocket, i: usize, p: &[u8], now: Instant) {
        let v = &mut self.vias[i];
        if v.turn.is_some() {
            if let Some(l) = v.turn_link(now) {
                l.send(v.addr, p);
            }
            return;
        }
        let _ = sock.send_to(p, v.addr);
    }
}

#[derive(Default)]
struct EndState {
    ways: HashMap<u64, Live>,
    by_route: HashMap<u64, u64>,
}

/// Ce qu'un bout fait d'un message reçu : (de qui, message).
pub type Deliver = Box<dyn Fn(u64, Vec<u8>) + Send + Sync>;

struct EndInner {
    sock: UdpSocket,
    /// Sens de ce qu'on envoie.
    dir: u8,
    st: Mutex<EndState>,
    stop: AtomicBool,
    out: Deliver,
}

/// Un bout de la voie rapide : l'invité (une route, vers l'hôte) ou l'hôte (une route par
/// invité). Les messages reçus sont remis à `out` depuis un fil à part, une fois chacun.
#[derive(Clone)]
pub struct Endpoint {
    inner: Arc<EndInner>,
}

impl std::fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Endpoint")
    }
}

impl Endpoint {
    /// `dir` : `TO_HOST` pour un invité, `TO_GUEST` pour l'hôte.
    pub fn start(dir: u8, out: Deliver) -> std::io::Result<Self> {
        let sock = UdpSocket::bind(("0.0.0.0", 0))?;
        sock.set_read_timeout(Some(Duration::from_millis(20)))?;
        let inner = Arc::new(EndInner {
            sock,
            dir,
            st: Mutex::new(EndState::default()),
            stop: AtomicBool::new(false),
            out,
        });
        let run = Arc::downgrade(&inner);
        std::thread::Builder::new()
            .name("voie rapide".into())
            .spawn(move || {
                let mut buf = [0u8; 2048];
                loop {
                    // Plus personne ne tient ce bout : le fil s'arrête.
                    let Some(me) = run.upgrade() else { break };
                    if me.stop.load(Ordering::Relaxed) {
                        break;
                    }
                    if let Some((n, from)) = read(&me.sock, &mut buf) {
                        me.on_packet(&buf[..n], from, false);
                    }
                    me.keep();
                }
            })?;
        Ok(Self { inner })
    }

    /// Remplace toutes les routes (nouvelle attribution du relais ; aucune : voie fermée).
    pub fn set_ways(&self, ways: Vec<Way>) {
        let mut st = self.inner.st.lock().unwrap();
        st.ways.clear();
        st.by_route.clear();
        // (v331) Les routes qui ont un chemin par TURN : ce qui en vient nous est remis
        // par le client TURN du nœud iroh du jeu (`turn_received`).
        let back = if self.inner.dir == TO_HOST {
            TO_GUEST
        } else {
            TO_HOST
        };
        let me = Arc::downgrade(&self.inner);
        let turned: Vec<u64> = ways
            .iter()
            .filter(|w| w.turn.is_some())
            .map(|w| w.route)
            .collect();
        {
            let mut ends = TURN_ENDS.lock().unwrap();
            ends.retain(|(_, w)| w.strong_count() > 0 && !w.ptr_eq(&me));
            ends.extend(turned.into_iter().map(|r| ((r, back), me.clone())));
        }
        for w in ways {
            st.by_route.insert(w.route, w.peer);
            let mut vias: Vec<LiveVia> = w
                .vias
                .iter()
                .take(VIAS)
                .map(|v| LiveVia::new(v.addr, v.seal, None))
                .collect();
            // (v331) Le chemin par TURN, après les passeurs (même rang aux deux bouts).
            // Aucun passeur ne vérifie son sceau : la clé n'a pas d'importance.
            if let Some(t) = w.turn {
                vias.push(LiveVia::new(SocketAddr::V4(t.peer), [0; 32], Some(t.mine)));
            }
            st.ways.insert(
                w.peer,
                Live {
                    route: w.route,
                    cipher: cipher(&w.key),
                    vias,
                    seq: 0,
                    win: Window::default(),
                    alt: 0,
                    heard: 0,
                    heard_at: None,
                    slow_in: false,
                    slow_out: false,
                    stats: Stats::default(),
                },
            );
        }
    }

    /// Le message part par tous les passeurs de la route (le premier arrivé compte), et
    /// (v331) par le chemin par TURN quand aucun passeur ne passe ou qu'il est nettement
    /// plus rapide. Faux si rien n'est parti (aucune route, message trop long).
    pub fn send_all(&self, peer: u64, msg: &[u8]) -> bool {
        if msg.len() > MAX_MSG {
            return false;
        }
        let now = Instant::now();
        let mut st = self.inner.st.lock().unwrap();
        let Some(l) = st.ways.get_mut(&peer) else {
            return false;
        };
        if l.vias.is_empty() {
            return false;
        }
        let open = l.open(now);
        let turn = l.turn_now(open);
        let seq = l.seq;
        l.seq += 1;
        for i in 0..l.vias.len() {
            if turn.is_some_and(|(t, take)| t == i && !take) {
                continue;
            }
            let p = l.packet(self.inner.dir, i, seq, 0, msg, now);
            l.out(&self.inner.sock, i, &p, now);
        }
        true
    }

    /// Le message part par un seul passeur, en alternant entre ceux par lesquels l'autre
    /// bout reçoit (v331 : ou par le chemin par TURN, voir `send_all`). Faux si la voie est
    /// fermée, ou si l'autre bout reçoit plus vite par le relais : à l'appelant de passer
    /// par le relais.
    pub fn send_one(&self, peer: u64, msg: &[u8]) -> bool {
        self.send_alt(peer, msg, false)
    }

    /// Comme `send_one`, même si l'autre bout reçoit plus vite par le relais : pour les
    /// sondes qui lui permettent de dire quand ce n'est plus vrai.
    pub fn send_any(&self, peer: u64, msg: &[u8]) -> bool {
        self.send_alt(peer, msg, true)
    }

    fn send_alt(&self, peer: u64, msg: &[u8], even_slow: bool) -> bool {
        if msg.len() > MAX_MSG {
            return false;
        }
        let now = Instant::now();
        let mut st = self.inner.st.lock().unwrap();
        let Some(l) = st.ways.get_mut(&peer) else {
            return false;
        };
        let open = l.open(now);
        if l.slow_in && !even_slow {
            return false;
        }
        let Some(i) = l.pick(open) else {
            return false;
        };
        let seq = l.seq;
        l.seq += 1;
        let p = l.packet(self.inner.dir, i, seq, 0, msg, now);
        l.out(&self.inner.sock, i, &p, now);
        true
    }

    /// (v331) Les messages vers `peer` partent-ils en ce moment par le chemin par TURN ?
    /// (Le jeu en redit moins dans chaque paquet : chaque octet compte dans le quota.)
    pub fn on_turn(&self, peer: u64) -> bool {
        let st = self.inner.st.lock().unwrap();
        st.ways.get(&peer).is_some_and(|l| {
            let open = l.open(Instant::now());
            l.turn_now(open)
                .is_some_and(|(t, take)| take && open & (1 << t) != 0)
        })
    }

    /// La voie vers `peer` est ouverte : l'autre bout dit recevoir par au moins un passeur.
    pub fn open(&self, peer: u64) -> bool {
        let st = self.inner.st.lock().unwrap();
        st.ways
            .get(&peer)
            .is_some_and(|l| l.open(Instant::now()) != 0)
    }

    /// Dit à l'autre bout qu'on reçoit plus vite par le relais (ou que ce n'est plus vrai).
    pub fn set_slow(&self, peer: u64, slow: bool) {
        if let Some(l) = self.inner.st.lock().unwrap().ways.get_mut(&peer) {
            l.slow_out = slow;
        }
    }

    /// L'autre bout dit recevoir plus vite par le relais (et la voie est ouverte).
    pub fn slow(&self, peer: u64) -> bool {
        let st = self.inner.st.lock().unwrap();
        st.ways
            .get(&peer)
            .is_some_and(|l| l.slow_in && l.open(Instant::now()) != 0)
    }

    pub fn stats(&self, peer: u64) -> Option<Stats> {
        let st = self.inner.st.lock().unwrap();
        let l = st.ways.get(&peer)?;
        Some(Stats {
            heard: l.open(Instant::now()),
            ..l.stats
        })
    }

    /// État de chaque chemin de la route vers `peer`, dans l'ordre de l'attribution : ses
    /// passeurs, puis (v331) son chemin par TURN.
    pub fn paths(&self, peer: u64) -> Vec<PathInfo> {
        let st = self.inner.st.lock().unwrap();
        let Some(l) = st.ways.get(&peer) else {
            return Vec::new();
        };
        let open = l.open(Instant::now());
        l.vias
            .iter()
            .enumerate()
            .map(|(i, v)| PathInfo {
                open: open & (1 << i) != 0,
                rtt_ms: v.rtt,
                turn: v.turn.is_some(),
            })
            .collect()
    }

    /// Adresses des passeurs de la route vers `peer` (sans le chemin par TURN).
    pub fn vias(&self, peer: u64) -> Vec<SocketAddr> {
        let st = self.inner.st.lock().unwrap();
        st.ways
            .get(&peer)
            .map(|l| {
                l.vias
                    .iter()
                    .filter(|v| v.turn.is_none())
                    .map(|v| v.addr)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// (v331) L'adresse relayée de l'autre bout, si la route vers `peer` a un chemin par
    /// TURN (essais, journal).
    pub fn turn_peer(&self, peer: u64) -> Option<SocketAddr> {
        let st = self.inner.st.lock().unwrap();
        let l = st.ways.get(&peer)?;
        l.vias.iter().find(|v| v.turn.is_some()).map(|v| v.addr)
    }

    /// Routes en place.
    pub fn ways(&self) -> usize {
        self.inner.st.lock().unwrap().ways.len()
    }

    /// Arrête ce bout (toutes ses copies).
    pub fn close(&self) {
        self.inner.stop.store(true, Ordering::Relaxed);
        self.set_ways(Vec::new());
    }
}

impl EndInner {
    /// Un paquet venu de `from` : par la socket de ce bout (un passeur), ou (v331, `turn`)
    /// par un client TURN de ce programme (`from` : l'adresse relayée de l'autre bout).
    fn on_packet(&self, p: &[u8], from: SocketAddr, turn: bool) {
        let Some(x) = parse(p) else { return };
        // Seuls les paquets de l'autre sens nous sont destinés.
        if x.dir == self.dir {
            return;
        }
        let now = Instant::now();
        let (peer, msg) = {
            let mut st = self.st.lock().unwrap();
            let Some(&peer) = st.by_route.get(&x.route) else {
                return;
            };
            let Some(l) = st.ways.get_mut(&peer) else {
                return;
            };
            // Il ne peut venir que d'un chemin de la route.
            let Some(i) = l
                .vias
                .iter()
                .position(|v| v.addr == from && v.turn.is_some() == turn)
            else {
                return;
            };
            let Ok(body) = l.cipher.decrypt(
                &nonce(x.dir, x.seq),
                Payload {
                    msg: x.sealed,
                    aad: x.head,
                },
            ) else {
                return;
            };
            l.vias[i].rx_at = Some(now);
            if !l.win.accept(x.seq) {
                l.stats.dup += 1;
                return;
            }
            let Some((&flags, msg)) = body.split_first() else {
                return;
            };
            l.heard = flags & HEARD;
            l.heard_at = Some(now);
            l.slow_in = flags & FLAG_SLOW != 0;
            // Mesure d'aller-retour : on renvoie le numéro par le même chemin ; au
            // retour du nôtre, le temps écoulé (lissé).
            if flags & FLAG_PING != 0 {
                let id = msg.first().copied().unwrap_or(0);
                let seq = l.seq;
                l.seq += 1;
                let p = l.packet(self.dir, i, seq, FLAG_KEEP | FLAG_PONG, &[id], now);
                l.out(&self.sock, i, &p, now);
            }
            if flags & FLAG_PONG != 0 {
                let v = &mut l.vias[i];
                if let (Some(&id), (mine, Some(at))) = (msg.first(), v.ping) {
                    if id == mine && v.ping_wait {
                        let ms = now.duration_since(at).as_secs_f32() * 1000.0;
                        v.rtt = Some(v.rtt.map_or(ms, |r| r * 0.75 + ms * 0.25));
                        v.ping_wait = false;
                    }
                }
            }
            if flags & FLAG_KEEP != 0 {
                return;
            }
            l.stats.got += 1;
            (peer, msg.to_vec())
        };
        (self.out)(peer, msg);
    }

    /// Paquets d'entretien : par chaque passeur resté sans envoi depuis `KEEP`. Ils ouvrent
    /// le chemin (le passeur apprend d'où l'on parle) et disent à l'autre bout par où l'on
    /// reçoit, même quand il n'y a rien d'autre à envoyer. (v331) Par le chemin par TURN,
    /// quand il attend en secours, seulement les mesures d'aller-retour : l'autre bout le
    /// tient pour ouvert plus longtemps (`FRESH_TURN`), et ce qu'on reçoit lui est dit dans
    /// chaque paquet, par n'importe quel chemin. Quand c'est lui qui sert, il est entretenu
    /// comme un passeur.
    fn keep(&self) {
        let now = Instant::now();
        let mut st = self.st.lock().unwrap();
        for l in st.ways.values_mut() {
            let turn_used = l.turn_now(l.open(now)).is_some_and(|(_, take)| take);
            for i in 0..l.vias.len() {
                // Mesure d'aller-retour par ce chemin, de temps en temps (elle vaut
                // aussi paquet d'entretien). Une mesure restée sans réponse est refaite.
                let due = l.vias[i]
                    .ping
                    .1
                    .is_none_or(|t| now.duration_since(t) >= PING_EVERY);
                if due {
                    let id = l.vias[i].ping.0.wrapping_add(1);
                    l.vias[i].ping = (id, Some(now));
                    l.vias[i].ping_wait = true;
                    let seq = l.seq;
                    l.seq += 1;
                    let p = l.packet(self.dir, i, seq, FLAG_KEEP | FLAG_PING, &[id], now);
                    l.out(&self.sock, i, &p, now);
                    continue;
                }
                if (l.vias[i].turn.is_some() && !turn_used)
                    || l.vias[i]
                        .tx_at
                        .is_some_and(|t| now.duration_since(t) < KEEP)
                {
                    continue;
                }
                let seq = l.seq;
                l.seq += 1;
                let p = l.packet(self.dir, i, seq, FLAG_KEEP, &[], now);
                l.out(&self.sock, i, &p, now);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::{channel, Receiver};

    fn key(n: u8) -> [u8; 32] {
        [n; 32]
    }

    fn local(port: u16) -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], port))
    }

    /// Ce qu'un bout a reçu : (de qui, message).
    type Inbox = Receiver<(u64, Vec<u8>)>;

    /// Bout qui dépose ce qu'il reçoit dans une file.
    fn end(dir: u8) -> (Endpoint, Inbox) {
        let (tx, rx) = channel();
        let tx = Mutex::new(tx);
        let e = Endpoint::start(
            dir,
            Box::new(move |peer, msg| {
                let _ = tx.lock().unwrap().send((peer, msg));
            }),
        )
        .unwrap();
        (e, rx)
    }

    fn passeur() -> (Passeur, Receiver<u64>) {
        let (tx, rx) = channel();
        (Passeur::start(0, tx).unwrap(), rx)
    }

    /// Une partie à un invité (numéro 7 chez l'hôte) par les passeurs donnés.
    fn link(ps: &[&Passeur]) -> (Endpoint, Inbox, Endpoint, Inbox) {
        let (guest, grx) = end(TO_HOST);
        let (host, hrx) = end(TO_GUEST);
        let vias: Vec<Via> = ps
            .iter()
            .enumerate()
            .map(|(i, p)| {
                p.set_lot(1, &[(42, key(10 + i as u8))]);
                Via {
                    addr: local(p.port()),
                    seal: key(10 + i as u8),
                }
            })
            .collect();
        guest.set_ways(vec![Way {
            peer: 0,
            route: 42,
            key: key(1),
            vias: vias.clone(),
            turn: None,
        }]);
        host.set_ways(vec![Way {
            peer: 7,
            route: 42,
            key: key(1),
            vias,
            turn: None,
        }]);
        (guest, grx, host, hrx)
    }

    fn wait(f: impl Fn() -> bool) -> bool {
        let end = Instant::now() + Duration::from_secs(3);
        while Instant::now() < end {
            if f() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    }

    #[test]
    fn window_takes_each_number_once() {
        let mut w = Window::default();
        assert!(w.accept(0));
        assert!(!w.accept(0));
        assert!(w.accept(5));
        // En retard, mais jamais vu : accepté une fois.
        assert!(w.accept(3));
        assert!(!w.accept(3));
        assert!(!w.accept(5));
        // Très en avance : tout ce qui précède de plus de 128 est refusé.
        assert!(w.accept(1000));
        assert!(!w.accept(4));
        assert!(w.accept(1000 - 127));
        assert!(!w.accept(1000 - 128));
        assert!(w.accept(999));
        assert!(!w.accept(999));
    }

    #[test]
    fn a_packet_opens_only_with_its_key_and_untouched() {
        let c = cipher(&key(1));
        let p = pack(&c, 9, TO_HOST, 3, &key(2), b"\x00bonjour");
        let x = parse(&p).unwrap();
        assert_eq!((x.route, x.dir, x.seq), (9, TO_HOST, 3));
        assert!(same(&hop_seal(&key(2), x.head), x.seal));
        assert!(!same(&hop_seal(&key(3), x.head), x.seal));
        let open = |c: &ChaCha20Poly1305, p: &[u8]| {
            let x = parse(p)?;
            c.decrypt(
                &nonce(x.dir, x.seq),
                Payload {
                    msg: x.sealed,
                    aad: x.head,
                },
            )
            .ok()
        };
        assert_eq!(open(&c, &p).as_deref(), Some(&b"\x00bonjour"[..]));
        assert!(open(&cipher(&key(4)), &p).is_none());
        // Un numéro ou un sens changé en route : la charge ne s'ouvre plus.
        let mut q = p.clone();
        q[10] ^= 1;
        assert!(open(&c, &q).is_none());
        let mut q = p.clone();
        q[9] = TO_GUEST;
        assert!(open(&c, &q).is_none());
        let mut q = p.clone();
        *q.last_mut().unwrap() ^= 1;
        assert!(open(&c, &q).is_none());
        assert!(parse(&p[..HEAD + SEAL + TAG]).is_none());
    }

    #[test]
    fn messages_cross_a_passeur_both_ways() {
        let (p, _probes) = passeur();
        let (guest, grx, host, hrx) = link(&[&p]);
        // Les paquets d'entretien ouvrent le chemin dans les deux sens.
        assert!(wait(|| guest.open(0) && host.open(7)));
        assert!(guest.send_all(0, b"entrees"));
        assert_eq!(
            hrx.recv_timeout(Duration::from_secs(2)).unwrap(),
            (7, b"entrees".to_vec())
        );
        assert!(host.send_one(7, b"positions"));
        assert_eq!(
            grx.recv_timeout(Duration::from_secs(2)).unwrap(),
            (0, b"positions".to_vec())
        );
        assert!(p.carried() > 0);
        // Trop long pour un paquet : refusé, l'appelant passera par le relais.
        assert!(!host.send_one(7, &[0u8; MAX_MSG + 1]));
        assert!(!host.send_one(8, b"inconnu"));
    }

    #[test]
    fn a_duplicate_through_the_second_passeur_is_dropped() {
        let (a, _pa) = passeur();
        let (b, _pb) = passeur();
        let (guest, _grx, host, hrx) = link(&[&a, &b]);
        assert!(wait(|| host.stats(7).is_some_and(|s| s.heard == 3)));
        for i in 0..50u8 {
            assert!(guest.send_all(0, &[i]));
        }
        let mut got = Vec::new();
        while let Ok((peer, m)) = hrx.recv_timeout(Duration::from_millis(300)) {
            assert_eq!(peer, 7);
            got.push(m[0]);
        }
        got.sort_unstable();
        assert_eq!(got, (0..50).collect::<Vec<u8>>());
        let s = host.stats(7).unwrap();
        assert_eq!(s.got, 50);
        assert!(s.dup >= 50, "doublons jetés : {}", s.dup);
    }

    #[test]
    fn loss_on_both_passeurs_is_mostly_covered_by_the_other() {
        let (a, _pa) = passeur();
        let (b, _pb) = passeur();
        let (guest, _grx, host, hrx) = link(&[&a, &b]);
        assert!(wait(|| host.stats(7).is_some_and(|s| s.heard == 3)));
        // Un paquet sur 3 perdu par l'un, un sur 4 par l'autre.
        a.lose_one_in(3);
        b.lose_one_in(4);
        for i in 0..200u8 {
            assert!(guest.send_all(0, &[i]));
            std::thread::sleep(Duration::from_micros(300));
        }
        let mut got = 0;
        while hrx.recv_timeout(Duration::from_millis(300)).is_ok() {
            got += 1;
        }
        // Un seul passeur en aurait perdu un quart à un tiers ; à deux, il n'en manque que
        // quand les deux perdent le même (un sur douze environ).
        assert!(got >= 170, "{got} messages sur 200");
    }

    #[test]
    fn a_dead_passeur_is_left_and_the_way_closes_when_none_answers() {
        let (a, _pa) = passeur();
        let (b, _pb) = passeur();
        let (guest, grx, host, _hrx) = link(&[&a, &b]);
        assert!(wait(|| host.stats(7).is_some_and(|s| s.heard == 3)));
        // Le premier passeur ne porte plus la route : l'hôte n'envoie plus que par l'autre.
        a.set_lot(1, &[]);
        assert!(wait(|| host.stats(7).is_some_and(|s| s.heard == 2)));
        while grx.try_recv().is_ok() {}
        for i in 0..20u8 {
            assert!(host.send_one(7, &[i]));
        }
        let mut got = 0;
        while grx.recv_timeout(Duration::from_millis(300)).is_ok() {
            got += 1;
        }
        assert_eq!(got, 20);
        // Plus aucun passeur : voie fermée des deux côtés, rien ne part.
        b.set_lot(1, &[]);
        assert!(wait(|| !host.open(7) && !guest.open(0)));
        assert!(!host.send_one(7, b"x"));
        // Les entrées partent quand même (elles passent aussi par le relais).
        assert!(guest.send_all(0, b"y"));
        drop(guest);
    }

    #[test]
    fn positions_alternate_between_live_passeurs() {
        let (a, _pa) = passeur();
        let (b, _pb) = passeur();
        let (_guest, grx, host, _hrx) = link(&[&a, &b]);
        assert!(wait(|| host.stats(7).is_some_and(|s| s.heard == 3)));
        let (ca, cb) = (a.carried(), b.carried());
        for i in 0..100u8 {
            assert!(host.send_one(7, &[i]));
        }
        let mut got = 0;
        while grx.recv_timeout(Duration::from_millis(300)).is_ok() {
            got += 1;
        }
        assert_eq!(got, 100);
        // Chacun en a porté la moitié (plus quelques paquets d'entretien).
        let (da, db) = (a.carried() - ca, b.carried() - cb);
        assert!(
            (50..90).contains(&da) && (50..90).contains(&db),
            "{da} et {db}"
        );
    }

    #[test]
    fn a_copied_packet_sent_from_elsewhere_diverts_nothing() {
        let (p, _probes) = passeur();
        let (guest, grx, host, hrx) = link(&[&p]);
        assert!(wait(|| guest.open(0) && host.open(7)));
        // Quelqu'un a copié un paquet de l'invité en chemin et le renvoie d'ailleurs.
        let c = cipher(&key(1));
        let old = pack(&c, 42, TO_HOST, 0, &key(10), &[0, 9]);
        let spy = UdpSocket::bind("127.0.0.1:0").unwrap();
        spy.set_read_timeout(Some(Duration::from_millis(300)))
            .unwrap();
        spy.send_to(&old, local(p.port())).unwrap();
        // Sans le sceau du passeur : même pas regardé.
        let forged = pack(&c, 42, TO_HOST, 1 << 40, &key(99), &[0, 9]);
        spy.send_to(&forged, local(p.port())).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        while hrx.try_recv().is_ok() {}
        // L'hôte répond toujours au vrai invité, et l'espion ne reçoit rien.
        assert!(host.send_one(7, b"pour l'invite"));
        assert_eq!(
            grx.recv_timeout(Duration::from_secs(2)).unwrap().1,
            b"pour l'invite".to_vec()
        );
        let mut buf = [0u8; 2048];
        assert!(spy.recv_from(&mut buf).is_err());
        // Et le paquet rejoué n'est pas remis une seconde fois à l'hôte.
        assert!(hrx.try_recv().is_err());
    }

    #[test]
    fn a_route_of_another_key_or_another_passeur_is_ignored() {
        let (p, _probes) = passeur();
        let (guest, _grx, host, hrx) = link(&[&p]);
        assert!(wait(|| guest.open(0) && host.open(7)));
        // Un autre invité, sur la même route mais sans la clé : le passeur transmet (il a
        // le bon sceau), l'hôte n'ouvre pas.
        let (bad, _brx) = end(TO_HOST);
        bad.set_ways(vec![Way {
            peer: 0,
            route: 42,
            key: key(77),
            vias: vec![Via {
                addr: local(p.port()),
                seal: key(10),
            }],
            turn: None,
        }]);
        // (Ses numéros repartent de zéro : le passeur ne lui donne même pas l'adresse.)
        assert!(bad.send_all(0, b"faux"));
        std::thread::sleep(Duration::from_millis(150));
        assert!(hrx.try_recv().is_err());
        assert!(guest.send_all(0, b"vrai"));
        assert_eq!(
            hrx.recv_timeout(Duration::from_secs(2)).unwrap(),
            (7, b"vrai".to_vec())
        );
    }

    #[test]
    fn the_relay_probe_reaches_the_passeur() {
        let (p, probes) = passeur();
        let relay = UdpSocket::bind("127.0.0.1:0").unwrap();
        probe(&relay, local(p.port()), 0xDEAD_BEEF_0042);
        assert_eq!(
            probes.recv_timeout(Duration::from_secs(2)).unwrap(),
            0xDEAD_BEEF_0042
        );
        assert_eq!(p.routes(), 0);
    }

    #[test]
    fn nothing_but_probes_goes_to_a_peer_who_gets_it_faster_by_the_relay() {
        let (p, _probes) = passeur();
        let (guest, grx, host, _hrx) = link(&[&p]);
        assert!(wait(|| guest.open(0) && host.open(7)));
        assert!(host.send_one(7, b"positions") && !host.slow(7));
        // L'invité reçoit plus vite par le relais : il le dit, l'hôte n'envoie plus par ici.
        guest.set_slow(0, true);
        assert!(wait(|| host.slow(7)));
        assert!(!host.send_one(7, b"positions"));
        // La voie reste ouverte, et les sondes passent.
        assert!(host.open(7));
        while grx.try_recv().is_ok() {}
        assert!(host.send_any(7, b"sonde"));
        assert_eq!(
            grx.recv_timeout(Duration::from_secs(2)).unwrap().1,
            b"sonde".to_vec()
        );
        // Les entrées de l'invité, elles, partent toujours (l'hôte prend la première arrivée).
        assert!(guest.send_all(0, b"entrees"));
        // Ce n'est plus vrai : tout repart.
        guest.set_slow(0, false);
        assert!(wait(|| !host.slow(7)));
        assert!(host.send_one(7, b"positions"));
    }

    #[test]
    fn the_faster_passeur_is_preferred_once_both_are_timed() {
        let (a, _pa) = passeur();
        let (b, _pb) = passeur();
        let (_guest, grx, host, _hrx) = link(&[&a, &b]);
        // Le second passeur retient chaque paquet 15 ms (dans chaque sens).
        b.delay_ms(15);
        assert!(wait(|| {
            let p = host.paths(7);
            p.len() == 2
                && p.iter().all(|x| x.open)
                && matches!((p[0].rtt_ms, p[1].rtt_ms), (Some(x), Some(y)) if y > x + 20.0)
        }));
        while grx.try_recv().is_ok() {}
        let (ca, cb) = (a.carried(), b.carried());
        for i in 0..60u8 {
            assert!(host.send_one(7, &[i]));
            std::thread::sleep(Duration::from_millis(2));
        }
        // Presque tout est parti par le premier (le second ne porte plus que l'entretien).
        let (da, db) = (a.carried() - ca, b.carried() - cb);
        assert!(da >= 60 && db < 20, "{da} par le rapide, {db} par le lent");
        let mut got = 0;
        while grx.recv_timeout(Duration::from_millis(300)).is_ok() {
            got += 1;
        }
        assert_eq!(got, 60);
    }

    #[test]
    fn a_new_passeur_takes_back_a_port_this_game_already_used() {
        let (p, _probes) = passeur();
        let port = p.port();
        drop(p);
        // (Le fil de l'ancien passeur rend le port en moins d'un quart de seconde.)
        let t = Instant::now();
        loop {
            let (tx, _rx) = channel();
            let q = Passeur::start_any(tx).unwrap();
            if q.port() == port {
                break;
            }
            assert!(
                t.elapsed() < Duration::from_secs(5),
                "port {port} jamais repris"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// (v301) Un paquet brut garde son contenu, porte le sceau que vérifie le passeur, et
    /// n'est lu qu'avec la clé de sa route ; il n'est jamais plus court que ce qu'un
    /// passeur accepte de transmettre.
    #[test]
    fn a_raw_packet_keeps_its_content_and_its_seal() {
        let seal = key(7);
        for body in [&b""[..], b"x", &[9u8; RAW_MAX]] {
            let p = raw_pack(42, TO_HOST, 5, &seal, RAW_DATA, body);
            assert!(p.len() > HEAD + SEAL + TAG);
            let x = parse(&p).unwrap();
            assert!(same(&hop_seal(&seal, x.head), x.seal));
            let got = raw_open(&p, |r| (r == 42).then_some(seal)).unwrap();
            assert_eq!(got, (42, TO_HOST, RAW_DATA, body));
            assert!(raw_open(&p, |_| Some(key(8))).is_none());
            assert!(raw_open(&p, |_| None).is_none());
        }
    }

    /// (v301) Les routes de tunnel données par le relais : celle vers un nœud distant est
    /// gardée (la dernière), une route reprise est oubliée, et quand le passeur est celui
    /// de ce jeu, on lui écrit sur cette machine, à son port local.
    #[test]
    fn tunnel_routes_given_by_the_relay_are_kept_and_taken_back() {
        let node = [0xA9; 32];
        let via = |ip: [u8; 4], port: u16| Via {
            addr: SocketAddr::from((ip, port)),
            seal: key(3),
        };
        let route = |route: u64, host: bool, vias: Vec<Via>| TunnelRoute {
            node,
            route,
            host,
            vias,
            turn: None,
        };
        assert!(tunnel_route(node).is_none());
        tunnel_given(route(1, false, vec![via([82, 1, 2, 3], 4000)]), false);
        tunnel_given(route(2, false, vec![via([82, 1, 2, 4], 4001)]), false);
        assert_eq!(tunnel_route(node).unwrap().route, 2);
        // Une autre route reprise ne retire pas celle qui sert.
        tunnel_given(route(1, false, Vec::new()), false);
        assert_eq!(tunnel_route(node).unwrap().route, 2);
        tunnel_given(route(2, false, Vec::new()), false);
        assert!(tunnel_route(node).is_none());
        // Notre propre passeur : reconnu à son port local, ou à l'adresse de la box.
        set_own_passeur(47_001, None);
        set_own_passeur(47_002, Some(SocketAddrV4::new([82, 9, 9, 9].into(), 51_000)));
        static SEEN: Mutex<Vec<TunnelRoute>> = Mutex::new(Vec::new());
        fn hook(r: &TunnelRoute) {
            if r.node == [0xA9; 32] {
                SEEN.lock().unwrap().push(r.clone());
            }
        }
        let before = TUNNELS.lock().unwrap().hook;
        set_tunnel_hook(hook);
        let mine = vec![
            via([82, 9, 9, 8], 47_001),
            via([82, 9, 9, 9], 51_000),
            via([82, 7, 7, 7], 12_345),
        ];
        tunnel_given(route(3, true, mine), true);
        TUNNELS.lock().unwrap().hook = before;
        let got = SEEN.lock().unwrap().pop().unwrap();
        let addrs: Vec<SocketAddr> = got.vias.iter().map(|v| v.addr).collect();
        assert_eq!(
            addrs,
            vec![
                SocketAddr::from(([127, 0, 0, 1], 47_001)),
                SocketAddr::from(([127, 0, 0, 1], 47_002)),
            ]
        );
        // (v329) Une route par TURN seulement (aucun passeur) est gardée comme une autre.
        let relayed = Via {
            addr: SocketAddr::from(([104, 30, 1, 2], 50_000)),
            seal: key(4),
        };
        tunnel_given(
            TunnelRoute {
                turn: Some(relayed),
                ..route(4, false, Vec::new())
            },
            false,
        );
        assert_eq!(tunnel_route(node).unwrap().turn, Some(relayed));
        tunnel_given(route(4, false, Vec::new()), false);
        assert!(tunnel_route(node).is_none());
        // Les demandes : chaque connexion ne voit que celles faites après elle.
        let (_, start) = tunnel_wants(u64::MAX, "");
        tunnel_want(node, None);
        tunnel_want(node, None);
        let (nodes, last) = tunnel_wants(start, "");
        assert!(nodes.contains(&TunnelAsk::Want(node, None)) && last >= start + 2);
        assert!(!tunnel_wants(last, "").0.contains(&TunnelAsk::Want(node, None)));
    }

    /// (v329) Les adresses relayées des nœuds : dites quand elles changent, et redites à
    /// une connexion qui n'en a rien entendu.
    #[test]
    fn relayed_addresses_are_told_again_only_when_they_change() {
        let (a, b) = ([0xC1; 32], [0xC2; 32]);
        let x = SocketAddrV4::new([104, 30, 2, 3].into(), 40_001);
        let start = turn_addrs(0).map_or(0, |(v, _)| v);
        set_turn_addr(a, Some(x));
        let (v1, all) = turn_addrs(start).unwrap();
        assert!(all.contains(&(a, Some(x))));
        assert_eq!(turn_addr(a), Some(x));
        set_turn_addr(a, Some(x));
        assert!(turn_addrs(v1).is_none(), "rien de neuf");
        set_turn_addr(b, None);
        set_turn_addr(a, None);
        let (_, all) = turn_addrs(v1).unwrap();
        assert!(all.contains(&(a, None)) && all.contains(&(b, None)));
        assert_eq!(turn_addr(a), None);
        // Une connexion neuve (0) les entend toutes.
        assert!(turn_addrs(0).is_some());
    }

    #[test]
    fn a_passeur_keeps_a_bounded_number_of_routes_per_lot() {
        let (p, _probes) = passeur();
        let many: Vec<(u64, [u8; 32])> =
            (0..ROUTES_MAX as u64 + 50).map(|i| (i, key(1))).collect();
        p.set_lot(1, &many);
        assert_eq!(p.routes(), ROUTES_MAX);
        p.set_lot(1, &many[..3]);
        assert_eq!(p.routes(), 3);
        p.set_lot(2, &many[10..12]);
        assert_eq!(p.routes(), 5);
        p.set_lot(1, &[]);
        assert_eq!(p.routes(), 2);
        p.clear();
        assert_eq!(p.routes(), 0);
    }

    // ─────────────── (v331) Chemin par TURN, avec un serveur TURN pour rire ───────────────

    /// Une allocation chez un faux serveur TURN : ce qu'on lui confie arrive à l'autre bout
    /// (`turn_received`) comme si le serveur l'avait relayé, depuis notre adresse relayée,
    /// dans un fil à part (comme un vrai serveur : jamais pendant l'envoi). Elle peut tout
    /// perdre, ou tout retenir quelques millisecondes ; elle compte ce qu'elle porte.
    struct FakeTurn {
        mine: SocketAddrV4,
        tx: Mutex<std::sync::mpsc::Sender<(SocketAddr, Vec<u8>, Instant)>>,
        lose: AtomicBool,
        delay_ms: AtomicU64,
        carried: AtomicU64,
    }

    impl TurnLink for FakeTurn {
        fn send(&self, _to: SocketAddr, packet: &[u8]) {
            if self.lose.load(Ordering::Relaxed) {
                return;
            }
            self.carried.fetch_add(1, Ordering::Relaxed);
            let at = Instant::now() + Duration::from_millis(self.delay_ms.load(Ordering::Relaxed));
            let _ = self
                .tx
                .lock()
                .unwrap()
                .send((SocketAddr::V4(self.mine), packet.to_vec(), at));
        }
    }

    static FAKES: Mutex<Vec<Arc<FakeTurn>>> = Mutex::new(Vec::new());

    fn fake_find(mine: SocketAddrV4) -> Option<Arc<dyn TurnLink>> {
        let all = FAKES.lock().unwrap();
        let f = all.iter().find(|f| f.mine == mine)?.clone();
        Some(f as Arc<dyn TurnLink>)
    }

    /// Une allocation neuve, à une adresse relayée que personne d'autre n'a.
    fn fake_turn() -> Arc<FakeTurn> {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        set_turn_find(fake_find);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let mine = SocketAddrV4::new([104, 31, 0, n as u8].into(), 30_000 + n as u16);
        let (tx, rx) = std::sync::mpsc::channel::<(SocketAddr, Vec<u8>, Instant)>();
        std::thread::spawn(move || {
            while let Ok((from, p, at)) = rx.recv() {
                let wait = at.saturating_duration_since(Instant::now());
                if !wait.is_zero() {
                    std::thread::sleep(wait);
                }
                turn_received(from, &p);
            }
        });
        let f = Arc::new(FakeTurn {
            mine,
            tx: Mutex::new(tx),
            lose: AtomicBool::new(false),
            delay_ms: AtomicU64::new(0),
            carried: AtomicU64::new(0),
        });
        FAKES.lock().unwrap().push(f.clone());
        f
    }

    /// Une partie à un invité (7 chez l'hôte) par ces passeurs et par TURN (chacun son
    /// allocation), sur une route à elle.
    fn link_turn(
        ps: &[&Passeur],
    ) -> (
        Endpoint,
        Inbox,
        Endpoint,
        Inbox,
        Arc<FakeTurn>,
        Arc<FakeTurn>,
    ) {
        let route: u64 = 0x7000_0000 + u64::from(rand_u16());
        let (guest, grx) = end(TO_HOST);
        let (host, hrx) = end(TO_GUEST);
        let (tg, th) = (fake_turn(), fake_turn());
        let vias: Vec<Via> = ps
            .iter()
            .enumerate()
            .map(|(i, p)| {
                p.set_lot(route, &[(route, key(20 + i as u8))]);
                Via {
                    addr: local(p.port()),
                    seal: key(20 + i as u8),
                }
            })
            .collect();
        guest.set_ways(vec![Way {
            peer: 0,
            route,
            key: key(2),
            vias: vias.clone(),
            turn: Some(TurnPair {
                mine: tg.mine,
                peer: th.mine,
            }),
        }]);
        host.set_ways(vec![Way {
            peer: 7,
            route,
            key: key(2),
            vias,
            turn: Some(TurnPair {
                mine: th.mine,
                peer: tg.mine,
            }),
        }]);
        (guest, grx, host, hrx, tg, th)
    }

    fn rand_u16() -> u16 {
        use std::hash::{BuildHasher, Hasher};
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(unix_ms());
        h.finish() as u16
    }

    fn drain(rx: &Inbox) -> usize {
        let mut got = 0;
        while rx.recv_timeout(Duration::from_millis(300)).is_ok() {
            got += 1;
        }
        got
    }

    /// (v331) Sans passeur, la voie rapide passe par TURN, de l'invité à l'hôte et retour,
    /// sans relais au milieu : chacun par sa propre allocation, vers celle de l'autre.
    #[test]
    fn without_passeurs_the_fast_way_goes_through_turn() {
        let (guest, grx, host, hrx, tg, th) = link_turn(&[]);
        // Les entrées partent tout de suite (rien n'est encore ouvert : on essaie).
        assert!(guest.send_all(0, b"premier"));
        assert!(
            wait(|| host.open(7) && guest.open(0)),
            "ouverte par les mesures"
        );
        assert!(host.on_turn(7) && guest.on_turn(0));
        // Rien d'autre à dire pendant une seconde : elle reste ouverte (seul chemin, il est
        // entretenu comme un passeur).
        for _ in 0..20 {
            std::thread::sleep(Duration::from_millis(50));
            assert!(host.open(7) && guest.open(0), "fermée sans rien à envoyer");
        }
        for i in 0..50u8 {
            assert!(host.send_one(7, &[i]));
            assert!(guest.send_all(0, &[i]));
        }
        assert_eq!(drain(&grx), 50);
        assert_eq!(drain(&hrx), 51);
        let paths = host.paths(7);
        assert_eq!(paths.len(), 1);
        assert!(paths[0].turn && paths[0].open && paths[0].rtt_ms.is_some());
        assert_eq!(host.turn_peer(7), Some(SocketAddr::V4(tg.mine)));
        assert!(host.vias(7).is_empty(), "aucun passeur");
        assert!(
            tg.carried.load(Ordering::Relaxed) >= 51 && th.carried.load(Ordering::Relaxed) >= 50
        );
        // L'allocation de l'hôte ne passe plus rien : la voie se ferme, au relais de
        // reprendre.
        th.lose.store(true, Ordering::Relaxed);
        assert!(wait(|| !guest.open(0)), "fermée côté invité");
        assert!(
            wait(|| !host.open(7)),
            "fermée côté hôte (ses paquets ne passent plus)"
        );
        assert!(!host.send_one(7, b"x"), "au relais de reprendre");
    }

    /// (v331) Quand un passeur porte la route, TURN ne sert pas (seulement ses mesures,
    /// deux par seconde) ; le passeur se tait : les positions passent par TURN dans la
    /// demi-seconde ; il revient : on le reprend.
    #[test]
    fn turn_stands_by_while_a_passeur_carries_and_takes_over_when_it_falls() {
        let (p, _probes) = passeur();
        let (guest, grx, host, _hrx, tg, th) = link_turn(&[&p]);
        assert!(
            wait(|| host.stats(7).is_some_and(|s| s.heard == 3)),
            "les deux chemins répondent"
        );
        assert!(!host.on_turn(7), "le passeur d'abord");
        let before = th.carried.load(Ordering::Relaxed);
        for i in 0..200u8 {
            assert!(host.send_one(7, &[i]));
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(drain(&grx), 200);
        let by_turn = th.carried.load(Ordering::Relaxed) - before;
        assert!(
            by_turn < 10,
            "{by_turn} paquets par TURN pendant que le passeur porte"
        );
        // Le passeur perd tout.
        p.lose_one_in(1);
        assert!(wait(|| host.on_turn(7)), "TURN prend le relais");
        let before = th.carried.load(Ordering::Relaxed);
        for i in 0..100u8 {
            assert!(host.send_one(7, &[i]));
        }
        assert_eq!(drain(&grx), 100);
        assert!(th.carried.load(Ordering::Relaxed) - before >= 100);
        // Les entrées aussi passent par TURN (et toujours par le passeur, qui ne coûte rien).
        let before = tg.carried.load(Ordering::Relaxed);
        assert!(guest.send_all(0, b"entrees"));
        assert!(tg.carried.load(Ordering::Relaxed) > before);
        // Il revient.
        p.lose_one_in(0);
        assert!(wait(|| !host.on_turn(7)), "retour au passeur");
    }

    /// (v331) Un passeur bien plus lent que TURN (80 ms de plus sur l'aller-retour) : TURN
    /// est pris ; un écart faible ne suffit pas.
    #[test]
    fn turn_is_taken_when_it_is_much_faster_than_the_passeurs() {
        let (p, _probes) = passeur();
        let (_guest, grx, host, _hrx, _tg, _th) = link_turn(&[&p]);
        assert!(wait(|| host.stats(7).is_some_and(|s| s.heard == 3)));
        // (Le passeur d'essai retient chaque paquet dans son seul fil : 3 ms par paquet,
        // un peu plus de 6 ms sur l'aller-retour.)
        p.delay_ms(3);
        std::thread::sleep(Duration::from_millis(1500));
        assert!(!host.on_turn(7), "quelques ms d'écart : le passeur reste");
        p.delay_ms(40);
        let end = Instant::now() + Duration::from_secs(6);
        while Instant::now() < end && !host.on_turn(7) {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(host.on_turn(7), "40 ms d'écart : TURN");
        assert!(host.send_one(7, b"vite"));
        assert!(drain(&grx) >= 1);
    }
}
