//! (v301) Transport d'iroh par les passeurs.
//!
//! Un hôte anonyme (relais de région, de province, de secours) n'était joignable que par
//! les serveurs relais publics d'iroh : ceux d'une autre société, au débit limité, prévus
//! pour le développement. Ici, les paquets QUIC d'iroh partent aussi en UDP par les
//! passeurs, ces jeux de joueurs volontaires qui portent déjà la voie rapide des parties
//! FPS (`minelib_net::fast`) : pas de serveur à payer, pas de limite de débit imposée par
//! un tiers, et ni le joueur ni l'hôte ne voit l'adresse IP de l'autre (seul le passeur
//! voit les deux ; il ne peut rien lire, QUIC chiffre de bout en bout).
//!
//! iroh choisit lui-même entre ses chemins, et range déjà un transport comme celui-ci
//! avec les chemins directs : le plus rapide des deux quand il y a les deux, et ses
//! serveurs relais seulement quand il ne reste rien d'autre. L'adresse d'un pair sur ce
//! transport est le numéro de la route que le relais du monde a ouverte entre les deux.

use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
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
const KEEP: Duration = Duration::from_secs(1);
/// Un passeur dont rien n'est venu depuis ce temps est tenu pour muet (l'autre bout lui
/// envoie, comme nous, un paquet vide toutes les `KEEP`) : on passe par un autre.
const SILENT: Duration = Duration::from_secs(3);

/// Adresse, sur ce transport, du pair au bout de la route `route`.
pub fn addr(route: u64) -> CustomAddr {
    CustomAddr::from_parts(TRANSPORT_ID, &route.to_le_bytes())
}

fn route_of(a: &CustomAddr) -> Option<u64> {
    (a.id() == TRANSPORT_ID).then(|| a.data().try_into().ok().map(u64::from_le_bytes))?
}

struct Route {
    /// Notre sens sur cette route (`fast::TO_HOST` ou `fast::TO_GUEST`).
    dir: u8,
    /// Passeurs de la route (adresse, clé du sceau, dernier paquet venu par lui), et celui
    /// qu'on emploie.
    vias: Vec<(SocketAddr, [u8; 32], Option<Instant>)>,
    via: usize,
    seq: u64,
}

impl Route {
    /// Le passeur à employer : celui en cours tant qu'il donne signe de vie, sinon celui
    /// dont on a eu des nouvelles le plus récemment.
    fn pick(&mut self, now: Instant) -> usize {
        let alive = |h: Option<Instant>| h.is_some_and(|t| now.duration_since(t) < SILENT);
        if !alive(self.vias[self.via].2) {
            if let Some((i, _)) = self
                .vias
                .iter()
                .enumerate()
                .filter(|(_, v)| alive(v.2))
                .max_by_key(|(_, v)| v.2)
            {
                log::info!("tunnel : un passeur se tait, la route passe par l'autre");
                self.via = i;
            }
        }
        self.via
    }
}

/// Transports des nœuds de ce jeu (pour leur donner les routes venues du relais).
static NODES: Mutex<Vec<([u8; 32], std::sync::Weak<Inner>)>> = Mutex::new(Vec::new());

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
    for (node, t) in nodes {
        let dir = t.0.routes.lock().unwrap().get(&r.route).map(|x| x.dir);
        match dir {
            Some(_) if vias.is_empty() => t.drop_route(r.route),
            Some(dir) => t.set_route(r.route, dir, vias.clone()),
            None if r.host && node == r.node && !vias.is_empty() => {
                t.set_route(r.route, fast::TO_GUEST, vias.clone())
            }
            None => {}
        }
    }
}

struct Inner {
    sock: tokio::net::UdpSocket,
    routes: Mutex<HashMap<u64, Route>>,
    addrs: n0_watcher::Watchable<Vec<CustomAddr>>,
}

impl std::fmt::Debug for Inner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "passeurs ({} routes)", self.routes.lock().unwrap().len())
    }
}

impl Inner {
    /// Un paquet (`kind`, `body`) sur la route `route`, par le passeur en cours.
    fn send(&self, route: u64, kind: u8, body: &[u8]) -> io::Result<()> {
        let (to, packet) = {
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
            let (to, seal, _) = r.vias[via];
            (to, fast::raw_pack(route, r.dir, r.seq, &seal, kind, body))
        };
        // (Tampon plein : le paquet est perdu, QUIC le renverra.)
        match self.sock.try_send_to(&packet, to) {
            Ok(_) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Un paquet vide sur chaque route, par chacun de ses passeurs.
    fn keep(&self) {
        let mut out = Vec::new();
        for (id, r) in self.routes.lock().unwrap().iter_mut() {
            for (to, seal, _) in &r.vias {
                r.seq += 1;
                out.push((*to, fast::raw_pack(*id, r.dir, r.seq, seal, fast::RAW_KEEP, &[])));
            }
        }
        for (to, p) in out {
            let _ = self.sock.try_send_to(&p, to);
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
        NODES.lock().unwrap().push((node, Arc::downgrade(&self.0)));
    }

    /// Ouvre (ou remplace) la route `route` : notre sens, et ses passeurs.
    pub fn set_route(&self, route: u64, dir: u8, vias: Vec<(SocketAddr, [u8; 32])>) {
        {
            let mut routes = self.0.routes.lock().unwrap();
            let old = routes.get(&route);
            let seq = old.map_or(0, |r| r.seq);
            // (Un passeur gardé garde ce qu'on sait de lui.)
            let vias = vias
                .into_iter()
                .map(|(a, s)| {
                    let heard = old
                        .and_then(|r| r.vias.iter().find(|v| v.0 == a && v.1 == s))
                        .and_then(|v| v.2);
                    (a, s, heard)
                })
                .collect();
            let vias: Vec<(SocketAddr, [u8; 32], Option<Instant>)> = vias;
            // (Le passeur employé le reste s'il est toujours là.)
            let via = old
                .and_then(|r| r.vias.get(r.via))
                .and_then(|cur| vias.iter().position(|v| v.0 == cur.0))
                .unwrap_or(0);
            routes.insert(
                route,
                Route {
                    dir,
                    vias,
                    via,
                    seq,
                },
            );
            let _ = self.0.addrs.set(routes.keys().map(|r| addr(*r)).collect());
        }
        // Tout de suite : le passeur ne peut rien nous remettre avant de nous connaître.
        self.0.keep();
    }

    pub fn drop_route(&self, route: u64) {
        let mut routes = self.0.routes.lock().unwrap();
        routes.remove(&route);
        let _ = self.0.addrs.set(routes.keys().map(|r| addr(*r)).collect());
    }

    pub fn routes(&self) -> usize {
        self.0.routes.lock().unwrap().len()
    }

    /// Passeur employé en ce moment pour la route `route` (essais, journal).
    pub fn via(&self, route: u64) -> Option<SocketAddr> {
        let routes = self.0.routes.lock().unwrap();
        let r = routes.get(&route)?;
        r.vias.get(r.via).map(|v| v.0)
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
        while count < bufs.len() {
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
                    let got = read.filled();
                    let mut routes = self.0.routes.lock().unwrap();
                    let opened = fast::raw_open(got, |r| {
                        routes
                            .get(&r)
                            .and_then(|x| x.vias.iter().find(|v| v.0 == from).map(|v| v.1))
                    });
                    let Some((route, dir, kind, body)) = opened else {
                        continue;
                    };
                    let Some(r) = routes.get_mut(&route) else {
                        continue;
                    };
                    // (Notre propre sens : un paquet à nous, renvoyé par erreur.)
                    if dir == r.dir {
                        continue;
                    }
                    if let Some(v) = r.vias.iter_mut().find(|v| v.0 == from) {
                        v.2 = Some(Instant::now());
                    }
                    if kind != fast::RAW_DATA || body.len() > bufs[count].len() {
                        continue;
                    }
                    bufs[count][..body.len()].copy_from_slice(body);
                    metas[count].len = body.len();
                    metas[count].stride = body.len();
                    recv_infos[count] = RecvInfo::new(addr(route), Some(addr(route)));
                    count += 1;
                }
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

/// Temps laissé au relais du monde pour donner une route avant d'ouvrir le tunnel sans.
const ASK_WAIT: Duration = Duration::from_millis(400);

/// Demande au relais du monde une route par des passeurs vers le nœud `remote`, et
/// l'ouvre dans `t` si elle vient à temps. Rien : pas de relais du monde au bout du fil,
/// ou pas de passeur.
pub(crate) async fn ask(t: &PasseurTransport, remote: [u8; 32]) -> Option<u64> {
    fast::tunnel_want(remote);
    let start = Instant::now();
    loop {
        if let Some(r) = fast::tunnel_route(remote) {
            let vias = r.vias.iter().map(|v| (v.addr, v.seal)).collect();
            t.set_route(r.route, fast::TO_HOST, vias);
            return Some(r.route);
        }
        if start.elapsed() >= ASK_WAIT {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
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
            _ = tokio::time::sleep(fast::TUNNEL_RENEW) => fast::tunnel_want(remote),
        }
    }
    t.drop_route(route);
}
