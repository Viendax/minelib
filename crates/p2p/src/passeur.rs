//! (v301) Transport d'iroh par les passeurs.
//!
//! Un hôte anonyme (relais de région, de province, de secours) n'était joignable que par
//! les serveurs relais publics d'iroh : ceux d'une autre société, au débit limité, prévus
//! pour le développement. Ici, les paquets QUIC d'iroh partent aussi en UDP par les
//! passeurs, ces jeux de joueurs volontaires qui portent déjà la voie rapide des parties
//! FPS (`minelib_net::fast`) : pas de serveur à payer, pas de limite de débit imposée par
//! un tiers, et ni le joueur ni l'hôte ne voit l'adresse IP de l'autre. Le passeur ne
//! peut rien lire (QUIC chiffre de bout en bout) ; seul, il voit les deux adresses ;
//! (v304) à deux, mis en chaîne par le relais, chacun n'en voit qu'une.
//!
//! iroh choisit lui-même entre ses chemins, et range déjà un transport comme celui-ci
//! avec les chemins directs : le plus rapide des deux quand il y a les deux, et ses
//! serveurs relais seulement quand il ne reste rien d'autre. L'adresse d'un pair sur ce
//! transport est le numéro de la route que le relais du monde a ouverte entre les deux.
//!
//! (v329) Une route peut aussi passer par un serveur TURN (`crate::turn`) : chaque bout y
//! a une adresse relayée, et le relais du monde donne à chacun
//! celle de l'autre comme un saut de plus, après ses passeurs. Les paquets de ce saut
//! partent par notre client TURN au lieu de la socket des passeurs, et en reviennent par
//! lui ; le reste (sceau, sens, passeur muet, signes de vie) est le même.

use std::collections::{HashMap, VecDeque};
use std::io;
use std::net::{SocketAddr, SocketAddrV4};
use std::sync::{Arc, Mutex, OnceLock};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use iroh::endpoint::transports::{
    CustomEndpoint, CustomSender, CustomTransport, RecvInfo, Transmit,
};
use iroh_base::CustomAddr;
use minelib_net::fast;

/// Numéro de ce transport dans les adresses d'iroh (« MLPASSEU »).
pub const TRANSPORT_ID: u64 = 0x4D4C_5041_5353_4555;
/// Un paquet vide part sur chaque route à ce rythme : le passeur garde notre adresse, la
/// box garde le passage ouvert.
const KEEP: Duration = Duration::from_millis(500);
/// Un passeur dont rien n'est venu depuis ce temps est tenu pour muet (l'autre bout lui
/// envoie, comme nous, un paquet vide toutes les `KEEP`) : on passe par un autre.
const SILENT: Duration = Duration::from_secs(2);

/// Une route muette est redite au relais à ce rythme, tant qu'elle le reste.
const REPORT_EVERY: Duration = Duration::from_secs(5);
/// (v331) Une route passée par TURN revient à un passeur qui répond de nouveau depuis ce
/// temps (sans attendre que TURN se taise : il compte dans le quota du mois).
const BACK: Duration = Duration::from_secs(3);

/// Adresse, sur ce transport, du pair au bout de la route `route`.
pub fn addr(route: u64) -> CustomAddr {
    CustomAddr::from_parts(TRANSPORT_ID, &route.to_le_bytes())
}

fn route_of(a: &CustomAddr) -> Option<u64> {
    (a.id() == TRANSPORT_ID).then(|| a.data().try_into().ok().map(u64::from_le_bytes))?
}

/// Un saut de la route : un passeur, ou (v329, `turn`) l'adresse relayée de l'autre bout
/// chez un serveur TURN, que nos paquets joignent par notre propre client TURN.
struct Hop {
    addr: SocketAddr,
    /// Clé du sceau de la route pour ce saut.
    seal: [u8; 32],
    /// Dernier paquet de l'autre bout venu par lui, et (v331) depuis quand il en vient sans
    /// s'être tu.
    heard: Option<Instant>,
    since: Option<Instant>,
    turn: bool,
}

struct Route {
    /// Notre sens sur cette route (`fast::TO_HOST` ou `fast::TO_GUEST`).
    dir: u8,
    /// Sauts de la route (ses passeurs, puis le saut TURN s'il y en a un), et celui qu'on
    /// emploie.
    vias: Vec<Hop>,
    via: usize,
    seq: u64,
    /// (v304) Dernière fois qu'on a dit au relais que plus rien n'y passait.
    reported: Option<Instant>,
}

impl Route {
    /// Le saut à employer : celui en cours tant qu'il donne signe de vie, sinon celui
    /// dont on a eu des nouvelles le plus récemment. (v331) Un passeur avant TURN : quand
    /// le saut en cours se tait, un passeur qui répond est pris avant TURN ; et sur TURN,
    /// on revient à un passeur qui répond de nouveau depuis `BACK`.
    fn pick(&mut self, now: Instant) -> usize {
        let alive = |h: Option<Instant>| h.is_some_and(|t| now.duration_since(t) < SILENT);
        if self.vias[self.via].turn {
            let back = self
                .vias
                .iter()
                .enumerate()
                .filter(|(_, v)| !v.turn && alive(v.heard))
                .filter(|(_, v)| v.since.is_some_and(|s| now.duration_since(s) >= BACK))
                .max_by_key(|(_, v)| v.heard);
            if let Some((i, _)) = back {
                log::info!("tunnel : un passeur répond de nouveau, la route quitte TURN");
                self.via = i;
                return self.via;
            }
        }
        if !alive(self.vias[self.via].heard) {
            if let Some((i, v)) = self
                .vias
                .iter()
                .enumerate()
                .filter(|(_, v)| alive(v.heard))
                .max_by_key(|(_, v)| (!v.turn, v.heard))
            {
                if v.turn {
                    log::info!("tunnel : plus aucun passeur ne répond, la route passe par TURN");
                } else {
                    log::info!("tunnel : un passeur se tait, la route passe par l'autre");
                }
                self.via = i;
            }
        }
        self.via
    }
}

/// Transports des nœuds de ce jeu (pour leur donner les routes venues du relais).
static NODES: Mutex<Vec<([u8; 32], std::sync::Weak<Inner>)>> = Mutex::new(Vec::new());

/// (v331) La voie rapide des parties FPS envoie par le client TURN d'un de nos nœuds.
impl fast::TurnLink for crate::turn::TurnClient {
    fn send(&self, to: SocketAddr, packet: &[u8]) {
        crate::turn::TurnClient::send(self, to, packet);
    }
}

/// (v331) Le client TURN d'un nœud de ce jeu dont l'adresse relayée est `mine` (la voie
/// rapide d'une partie FPS envoie par lui : c'est cette adresse que l'autre bout connaît).
fn turn_find(mine: SocketAddrV4) -> Option<Arc<dyn fast::TurnLink>> {
    let nodes: Vec<Arc<Inner>> = NODES
        .lock()
        .unwrap()
        .iter()
        .filter_map(|(_, w)| w.upgrade())
        .collect();
    nodes.iter().find_map(|i| {
        let t = i.turn.lock().unwrap().clone()?;
        (t.relayed() == Some(mine)).then_some(t as Arc<dyn fast::TurnLink>)
    })
}

/// Une route est donnée, changée ou reprise par le relais (`fast::tunnel_given`) : les
/// nœuds qui la portent déjà la mettent à jour ; celle d'un joueur vers un de nos nœuds
/// y est ouverte.
fn on_route(r: &fast::TunnelRoute) {
    let nodes: Vec<([u8; 32], PasseurTransport)> = {
        let mut all = NODES.lock().unwrap();
        all.retain(|(_, w)| w.strong_count() > 0);
        all.iter()
            .filter_map(|(n, w)| Some((*n, PasseurTransport(w.upgrade()?))))
            .collect()
    };
    let vias: Vec<(SocketAddr, [u8; 32])> = r.vias.iter().map(|v| (v.addr, v.seal)).collect();
    let turn = r.turn.map(|v| (v.addr, v.seal));
    for (node, t) in nodes {
        // (Chaque bout a ses propres passeurs : ce qui est dit à l'hôte ne vaut que pour
        // le bout hôte de la route, et inversement.)
        let dir = t.0.routes.lock().unwrap().get(&r.route).map(|x| x.dir);
        let dir = dir.filter(|d| (*d == fast::TO_GUEST) == r.host);
        let none = vias.is_empty() && turn.is_none();
        match dir {
            Some(_) if none => t.drop_route(r.route),
            Some(dir) => t.set_route(r.route, dir, vias.clone(), turn),
            None if r.host && node == r.node && !none && !t.has(r.route) => {
                t.set_route(r.route, fast::TO_GUEST, vias.clone(), turn)
            }
            None => {}
        }
    }
}

struct Inner {
    sock: tokio::net::UdpSocket,
    routes: Mutex<HashMap<u64, Route>>,
    /// Adresses annoncées à iroh : aucune, jamais. iroh publie les adresses d'un nœud
    /// (dans le DNS de son éditeur, pour qu'on le trouve par sa clé) : les numéros de
    /// route n'ont rien à y faire, ils ne servent qu'aux deux bouts, à qui le relais du
    /// monde les donne ; et un hôte en a une par joueur, de quoi faire déborder
    /// l'annonce et la faire échouer.
    addrs: n0_watcher::Watchable<Vec<CustomAddr>>,
    /// Le moteur du nœud (pour ce qui part d'un autre fil que les siens).
    rt: tokio::runtime::Handle,
    /// (v329) Le nœud de ce transport, son client TURN s'il en a un, et ce qui en est venu
    /// sans avoir encore été lu (adresse relayée de l'autre bout, paquet).
    node: OnceLock<[u8; 32]>,
    turn: Mutex<Option<Arc<crate::turn::TurnClient>>>,
    turn_in: Mutex<VecDeque<(SocketAddr, Vec<u8>)>>,
    turn_waker: Mutex<Option<Waker>>,
}

/// Paquets venus par TURN gardés en attente de lecture, au plus.
const TURN_QUEUE: usize = 1024;

impl std::fmt::Debug for Inner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "passeurs ({} routes)", self.routes.lock().unwrap().len())
    }
}

impl Inner {
    /// Un paquet (`kind`, `body`) sur la route `route`, par le saut en cours.
    fn send(&self, route: u64, kind: u8, body: &[u8]) -> io::Result<()> {
        let (to, turn, packet) = {
            let mut routes = self.routes.lock().unwrap();
            let r = routes
                .get_mut(&route)
                .ok_or_else(|| io::Error::other("route inconnue"))?;
            // (Trop gros : jeté. QUIC essaie de plus gros paquets pour mesurer la liaison ;
            // ceux-là se perdent, il s'en tient à 1 200 octets.)
            if r.vias.is_empty() || body.len() > fast::RAW_MAX {
                return Ok(());
            }
            let via = r.pick(Instant::now());
            r.seq += 1;
            let h = &r.vias[via];
            let p = fast::raw_pack(route, r.dir, r.seq, &h.seal, kind, body);
            (h.addr, h.turn, p)
        };
        self.out(to, turn, &packet)
    }

    /// Un paquet vers un saut : la socket des passeurs, ou (v329) notre client TURN.
    fn out(&self, to: SocketAddr, turn: bool, packet: &[u8]) -> io::Result<()> {
        if turn {
            let t = self.turn.lock().unwrap().clone();
            if let Some(t) = t {
                t.send(to, packet);
            }
            return Ok(());
        }
        // (Tampon plein : le paquet est perdu, QUIC le renverra.)
        match self.sock.try_send_to(packet, to) {
            Ok(_) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Un paquet vide sur chaque route, par chacun de ses sauts.
    fn keep(&self) {
        let mut out = Vec::new();
        let now = Instant::now();
        for (id, r) in self.routes.lock().unwrap().iter_mut() {
            for h in &r.vias {
                r.seq += 1;
                let p = fast::raw_pack(*id, r.dir, r.seq, &h.seal, fast::RAW_KEEP, &[]);
                out.push((h.addr, h.turn, p));
            }
            // (v304) Elle passait, et plus rien ne vient d'aucun de ses passeurs : on le
            // dit au relais, qui lui en donne d'autres. Le tunnel attend (iroh garde un
            // chemin muet 15 s avant d'y renoncer).
            let silent = |h: Option<Instant>| h.is_some_and(|t| now.duration_since(t) >= SILENT);
            if r.vias.iter().any(|v| v.heard.is_some())
                && r.vias.iter().all(|v| v.heard.is_none() || silent(v.heard))
                && r.reported.is_none_or(|t| now.duration_since(t) >= REPORT_EVERY)
            {
                r.reported = Some(now);
                log::info!("tunnel : plus rien ne passe par ses passeurs, d'autres sont demandés");
                fast::tunnel_bad(*id);
            }
        }
        for (to, turn, p) in out {
            let _ = self.out(to, turn, &p);
        }
    }

    /// (v304) Un paquet sans contenu (`kind`) sur la route `route` : par le saut `via`,
    /// ou par tous.
    fn signal(&self, route: u64, kind: u8, via: Option<SocketAddr>) {
        let mut out = Vec::new();
        if let Some(r) = self.routes.lock().unwrap().get_mut(&route) {
            for h in r.vias.iter().filter(|v| via.is_none_or(|a| a == v.addr)) {
                r.seq += 1;
                let p = fast::raw_pack(route, r.dir, r.seq, &h.seal, kind, &[]);
                out.push((h.addr, h.turn, p));
            }
        }
        for (to, turn, p) in out {
            let _ = self.out(to, turn, &p);
        }
    }

    /// Un paquet venu de `from` (par TURN si `turn`) : sa route et son contenu s'il est
    /// pour iroh. Les autres (signes de vie, sondes) sont traités ici.
    fn accept<'a>(&self, from: SocketAddr, turn: bool, got: &'a [u8]) -> Option<(u64, &'a [u8])> {
        let mut routes = self.routes.lock().unwrap();
        let hop = |r: &Route| -> Option<[u8; 32]> {
            r.vias
                .iter()
                .find(|v| v.addr == from && v.turn == turn)
                .map(|v| v.seal)
        };
        let (route, dir, kind, body) = fast::raw_open(got, |r| routes.get(&r).and_then(hop))?;
        let r = routes.get_mut(&route)?;
        // (Notre propre sens : un paquet à nous, renvoyé par erreur.)
        if dir == r.dir {
            return None;
        }
        if let Some(v) = r.vias.iter_mut().find(|v| v.addr == from && v.turn == turn) {
            let now = Instant::now();
            if v.heard.is_none_or(|t| now.duration_since(t) >= SILENT) {
                v.since = Some(now);
            }
            v.heard = Some(now);
        }
        // (v304) L'autre bout s'assure que la route passe : on lui répond par le même saut.
        if kind == fast::RAW_PING {
            drop(routes);
            self.signal(route, fast::RAW_PONG, Some(from));
            return None;
        }
        (kind == fast::RAW_DATA).then_some((route, body))
    }

    /// (v329) Reçu par notre client TURN : gardé pour la prochaine lecture d'iroh. (v331)
    /// Sauf ce qui est pour la voie rapide d'une partie FPS (mêmes en-têtes, d'autres
    /// numéros de route) : remis à son bout.
    fn turn_in(&self, from: SocketAddr, p: &[u8]) {
        let ours = p
            .get(1..9)
            .and_then(|r| r.try_into().ok())
            .map(u64::from_le_bytes)
            .is_some_and(|r| self.routes.lock().unwrap().contains_key(&r));
        if !ours && fast::turn_received(from, p) {
            return;
        }
        {
            let mut q = self.turn_in.lock().unwrap();
            if q.len() >= TURN_QUEUE {
                return;
            }
            q.push_back((from, p.to_vec()));
        }
        if let Some(w) = self.turn_waker.lock().unwrap().take() {
            w.wake();
        }
    }
}

/// Le transport : à ajouter au nœud iroh (`add_custom_transport`), et à garder pour lui
/// donner ses routes.
#[derive(Clone, Debug)]
pub struct PasseurTransport(Arc<Inner>);

impl PasseurTransport {
    /// À appeler dans un moteur tokio (celui du nœud).
    pub fn bind() -> io::Result<Self> {
        let sock = std::net::UdpSocket::bind("0.0.0.0:0")?;
        sock.set_nonblocking(true)?;
        let inner = Arc::new(Inner {
            sock: tokio::net::UdpSocket::from_std(sock)?,
            routes: Mutex::new(HashMap::new()),
            addrs: n0_watcher::Watchable::new(Vec::new()),
            rt: tokio::runtime::Handle::current(),
            node: OnceLock::new(),
            turn: Mutex::new(None),
            turn_in: Mutex::new(VecDeque::new()),
            turn_waker: Mutex::new(None),
        });
        let weak = Arc::downgrade(&inner);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(KEEP).await;
                match weak.upgrade() {
                    Some(i) => i.keep(),
                    None => return,
                }
            }
        });
        Ok(Self(inner))
    }

    /// Ce transport est celui du nœud `node` : les routes que le relais donne pour lui
    /// (`fast::tunnel_given`) y arrivent.
    pub(crate) fn register(&self, node: [u8; 32]) {
        fast::set_tunnel_hook(on_route);
        fast::set_turn_find(turn_find);
        NODES.lock().unwrap().push((node, Arc::downgrade(&self.0)));
        let _ = self.0.node.set(node);
        // (v329) Une adresse chez un serveur TURN, si ce programme en veut (`enable_turn`)
        // et que le jeu en connaît (`relais_turn.txt`).
        if crate::turn_enabled() {
            let sources = crate::turn::sources();
            if !sources.is_empty() {
                self.start_turn(sources);
            }
        }
    }

    /// (v329) Démarre le client TURN de ce transport : son adresse relayée est dite au
    /// relais (`fast::set_turn_addr`), et le saut TURN des routes passe par lui.
    pub fn start_turn(&self, sources: Vec<crate::turn::Source>) {
        let weak = Arc::downgrade(&self.0);
        let node = self.0.node.get().copied();
        let sink: crate::turn::Sink = Box::new(move |from, p| {
            if let Some(i) = weak.upgrade() {
                i.turn_in(from, p);
            }
        });
        let told: crate::turn::Told = Box::new(move |a| {
            if let Some(n) = node {
                fast::set_turn_addr(n, a);
            }
        });
        match crate::turn::TurnClient::start(&self.0.rt, sources, sink, told) {
            Ok(c) => *self.0.turn.lock().unwrap() = Some(c),
            Err(e) => log::info!("TURN : pas de socket ({e})"),
        }
    }

    /// (v329) Arrête le client TURN de ce transport (le nœud s'en va, essais) : son adresse
    /// relayée n'est plus dite aux relais.
    pub fn stop_turn(&self) {
        let t = self.0.turn.lock().unwrap().take();
        drop(t);
    }

    /// (v329) Notre adresse relayée chez le serveur TURN, s'il y en a une.
    pub fn turn_addr(&self) -> Option<SocketAddrV4> {
        self.0.turn.lock().unwrap().as_ref()?.relayed()
    }

    /// (v329) Paquets envoyés et reçus par TURN (essais, journal).
    pub fn turn_counts(&self) -> (u64, u64) {
        self.0
            .turn
            .lock()
            .unwrap()
            .as_ref()
            .map_or((0, 0), |t| t.counts())
    }

    /// Ouvre (ou remplace) la route `route` : notre sens, ses passeurs, et (v329) l'adresse
    /// relayée de l'autre bout chez un serveur TURN.
    pub fn set_route(
        &self,
        route: u64,
        dir: u8,
        vias: Vec<(SocketAddr, [u8; 32])>,
        turn: Option<(SocketAddr, [u8; 32])>,
    ) {
        {
            let mut routes = self.0.routes.lock().unwrap();
            let old = routes.get(&route);
            let seq = old.map_or(0, |r| r.seq);
            let hops = vias
                .into_iter()
                .map(|(a, s)| (a, s, false))
                .chain(turn.map(|(a, s)| (a, s, true)));
            // (Un saut gardé garde ce qu'on sait de lui.)
            let vias: Vec<Hop> = hops
                .map(|(addr, seal, turn)| {
                    let kept = old.and_then(|r| {
                        r.vias
                            .iter()
                            .find(|v| v.addr == addr && v.seal == seal && v.turn == turn)
                    });
                    Hop {
                        addr,
                        seal,
                        heard: kept.and_then(|v| v.heard),
                        since: kept.and_then(|v| v.since),
                        turn,
                    }
                })
                .collect();
            // (Le saut employé le reste s'il est toujours là.)
            let via = old
                .and_then(|r| r.vias.get(r.via))
                .and_then(|cur| {
                    vias.iter()
                        .position(|v| v.addr == cur.addr && v.turn == cur.turn)
                })
                .unwrap_or(0);
            let reported = old.and_then(|r| r.reported);
            routes.insert(
                route,
                Route {
                    dir,
                    vias,
                    via,
                    seq,
                    reported,
                },
            );
        }
        // Tout de suite : le passeur ne peut rien nous remettre avant de nous connaître.
        // (v304) Et encore quelques fois, de plus en plus espacées, tant que rien n'est
        // venu : le passeur reçoit sa route du relais en même temps que nous, il a pu ne
        // pas l'avoir encore à notre premier paquet.
        self.0.signal(route, fast::RAW_KEEP, None);
        let weak = Arc::downgrade(&self.0);
        self.0.rt.spawn(async move {
            for ms in [30u64, 60, 120, 240, 480] {
                tokio::time::sleep(Duration::from_millis(ms)).await;
                let Some(me) = weak.upgrade() else { return };
                if PasseurTransport(me.clone()).alive(route) {
                    return;
                }
                me.signal(route, fast::RAW_KEEP, None);
            }
        });
    }

    pub fn drop_route(&self, route: u64) {
        self.0.routes.lock().unwrap().remove(&route);
    }

    pub fn routes(&self) -> usize {
        self.0.routes.lock().unwrap().len()
    }

    fn has(&self, route: u64) -> bool {
        self.0.routes.lock().unwrap().contains_key(&route)
    }

    /// (v304) La route `route` passe-t-elle de bout en bout ? (Un paquet de l'autre bout
    /// est venu par un de ses passeurs il y a peu.)
    pub fn alive(&self, route: u64) -> bool {
        let routes = self.0.routes.lock().unwrap();
        routes.get(&route).is_some_and(|r| {
            r.vias
                .iter()
                .any(|v| v.heard.is_some_and(|t| t.elapsed() < SILENT))
        })
    }

    /// Passeur employé en ce moment pour la route `route` (essais, journal).
    pub fn via(&self, route: u64) -> Option<SocketAddr> {
        let routes = self.0.routes.lock().unwrap();
        let r = routes.get(&route)?;
        r.vias.get(r.via).map(|v| v.addr)
    }

    /// (v329) La route `route` passe-t-elle en ce moment par TURN (essais, journal) ?
    pub fn via_turn(&self, route: u64) -> bool {
        let routes = self.0.routes.lock().unwrap();
        routes
            .get(&route)
            .and_then(|r| r.vias.get(r.via))
            .is_some_and(|v| v.turn)
    }
}

impl CustomTransport for PasseurTransport {
    fn bind(&self) -> io::Result<Box<dyn CustomEndpoint>> {
        Ok(Box::new(End(self.0.clone())))
    }
}

#[derive(Debug)]
struct End(Arc<Inner>);

impl CustomEndpoint for End {
    fn watch_local_addrs(&self) -> n0_watcher::Direct<Vec<CustomAddr>> {
        self.0.addrs.watch()
    }

    fn create_sender(&self) -> Arc<dyn CustomSender> {
        Arc::new(Sender(self.0.clone()))
    }

    fn poll_recv(
        &mut self,
        cx: &mut Context,
        bufs: &mut [io::IoSliceMut<'_>],
        metas: &mut [noq_udp::RecvMeta],
        recv_infos: &mut [RecvInfo],
    ) -> Poll<io::Result<usize>> {
        let mut count = 0;
        let mut errors = 0;
        let mut packet = [0u8; 2048];
        let cap = bufs.len();
        let mut deliver = |count: &mut usize, route: u64, body: &[u8]| {
            if body.len() > bufs[*count].len() {
                return;
            }
            bufs[*count][..body.len()].copy_from_slice(body);
            metas[*count].len = body.len();
            metas[*count].stride = body.len();
            recv_infos[*count] = RecvInfo::new(addr(route), Some(addr(route)));
            *count += 1;
        };
        while count < cap {
            let mut read = tokio::io::ReadBuf::new(&mut packet);
            match self.0.sock.poll_recv_from(cx, &mut read) {
                Poll::Pending => break,
                // (Sous Windows, un paquet envoyé à un passeur éteint revient ici en
                // erreur : ce n'est pas une panne du transport, on lit la suite.)
                Poll::Ready(Err(_)) => {
                    errors += 1;
                    if errors >= 16 {
                        cx.waker().wake_by_ref();
                        break;
                    }
                }
                Poll::Ready(Ok(from)) => {
                    if let Some((route, body)) = self.0.accept(from, false, read.filled()) {
                        deliver(&mut count, route, body);
                    }
                }
            }
        }
        // (v329) Ce que notre client TURN a reçu entre-temps. (Le réveil est posé avant de
        // regarder : un paquet arrivé juste après ne peut pas être oublié.)
        if count < cap {
            *self.0.turn_waker.lock().unwrap() = Some(cx.waker().clone());
            let room = cap - count;
            let got: Vec<(SocketAddr, Vec<u8>)> = {
                let mut q = self.0.turn_in.lock().unwrap();
                let n = q.len().min(room);
                q.drain(..n).collect()
            };
            for (from, p) in got {
                if let Some((route, body)) = self.0.accept(from, true, &p) {
                    deliver(&mut count, route, body);
                }
            }
            if !self.0.turn_in.lock().unwrap().is_empty() {
                cx.waker().wake_by_ref();
            }
        }
        if count > 0 {
            Poll::Ready(Ok(count))
        } else {
            Poll::Pending
        }
    }
}

#[derive(Debug)]
struct Sender(Arc<Inner>);

impl CustomSender for Sender {
    fn is_valid_send_addr(&self, a: &CustomAddr) -> bool {
        route_of(a).is_some_and(|r| self.0.routes.lock().unwrap().contains_key(&r))
    }

    fn poll_send(
        &self,
        _cx: &mut Context,
        dst: &CustomAddr,
        _src: Option<&CustomAddr>,
        transmit: &Transmit<'_>,
    ) -> Poll<io::Result<()>> {
        let Some(route) = route_of(dst) else {
            return Poll::Ready(Err(io::Error::other("adresse d'un autre transport")));
        };
        let size = transmit.segment_size.unwrap_or(transmit.contents.len()).max(1);
        for part in transmit.contents.chunks(size) {
            if let Err(e) = self.0.send(route, fast::RAW_DATA, part) {
                return Poll::Ready(Err(e));
            }
        }
        Poll::Ready(Ok(()))
    }
}

/// (v317) Temps laissé à un jeu en veille pour revenir au relais du monde quand un tunnel
/// doit s'ouvrir (retour par certificat : une poignée de main et deux échanges).
const WAKE_WAIT: Duration = Duration::from_millis(2500);
/// Temps laissé au relais du monde pour donner une route, puis à la route pour répondre
/// de bout en bout, avant d'ouvrir le tunnel sans elle. (Un relais qui a des passeurs
/// répond en quelques dizaines de millisecondes ; sans passeur, on n'attend pas du tout.)
const ASK_WAIT: Duration = Duration::from_millis(1500);
const CHECK_WAIT: Duration = Duration::from_millis(1500);

/// (v317) Faut-il attendre que le jeu revienne au relais du monde avant d'ouvrir le
/// tunnel ? Seulement en veille, et si ce relais avait des passeurs.
pub(crate) fn wait_for_relay(dozing: bool, passeurs_lately: bool) -> bool {
    dozing && passeurs_lately
}

/// Demande au relais du monde une route par des passeurs vers le nœud `remote`, l'ouvre
/// dans `t`, et s'assure qu'elle passe de bout en bout. Rien : aucun relais de ce jeu n'a
/// de passeur, la route n'est pas venue à temps, ou elle ne passe pas.
///
/// (v304) Il faut que la route passe **avant** d'ouvrir le tunnel : iroh n'ajoute pas un
/// chemin comme celui-ci à une connexion déjà ouverte (il n'ouvre de lui-même que des
/// chemins directs ou par ses serveurs). Le tunnel doit donc partir par le passeur ; les
/// serveurs d'iroh lui sont ajoutés ensuite, en secours.
pub(crate) async fn ask(t: &PasseurTransport, remote: [u8; 32]) -> Option<u64> {
    if !fast::passeurs_known() {
        // (v317) En veille au fond d'un pays, ce jeu n'a plus de lien avec le relais du
        // monde : s'il avait des passeurs, le jeu y revient (il sait le faire, c'est un
        // retour ordinaire) et le tunnel l'attend un peu. Avant, un tunnel ouvert en
        // veille prenait toujours les serveurs d'iroh. Hors veille (relais perdu, pas
        // encore connecté), rien n'attend.
        if !wait_for_relay(fast::dozing(), fast::passeurs_lately()) {
            return None;
        }
        fast::want_route();
        let start = Instant::now();
        while !fast::passeurs_known() {
            if start.elapsed() >= WAKE_WAIT {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
    fast::tunnel_want(remote, t.turn_addr());
    let start = Instant::now();
    let route = loop {
        if let Some(r) = fast::tunnel_route(remote) {
            let vias = r.vias.iter().map(|v| (v.addr, v.seal)).collect();
            t.set_route(
                r.route,
                fast::TO_HOST,
                vias,
                r.turn.map(|v| (v.addr, v.seal)),
            );
            break r.route;
        }
        if start.elapsed() >= ASK_WAIT {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    let start = Instant::now();
    while !t.alive(route) {
        if start.elapsed() >= CHECK_WAIT {
            log::info!("tunnel : la route par des passeurs ne répond pas, on s'en passe");
            t.drop_route(route);
            return None;
        }
        t.0.signal(route, fast::RAW_PING, None);
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    Some(route)
}

/// Tant que le tunnel vers `remote` vit, sa route est redemandée au relais (qui reprend
/// celles qu'on ne redemande plus) ; à sa fermeture, elle est rendue.
pub(crate) async fn keep(
    t: PasseurTransport,
    remote: [u8; 32],
    route: u64,
    conn: iroh::endpoint::Connection,
) {
    loop {
        tokio::select! {
            _ = conn.closed() => break,
            _ = tokio::time::sleep(fast::TUNNEL_RENEW) => fast::tunnel_want(remote, t.turn_addr()),
        }
    }
    t.drop_route(route);
}
