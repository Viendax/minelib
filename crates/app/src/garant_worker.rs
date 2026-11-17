//! Garant pendant qu'on joue : une deuxième connexion du compte au même relais, dans son
//! propre fil, sert (ou vérifie) la partie FPS d'autres joueurs pendant que ce joueur joue
//! la sienne. Le relais le choisit de préférence quand sa partie vient de commencer (il a
//! tout intérêt à rester en jeu) et le fait relever par un autre garant avant qu'elle ne
//! se termine. Rien ne passe par le fil du jeu : les images du joueur ne ralentissent pas.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use minelib_net::client::{Client, Cmd, Event};

use crate::garant::Garant;

/// De quoi ouvrir la connexion de garant (voir `Accounts::garant_login`).
#[derive(Clone)]
pub struct Login {
    pub addr: String,
    pub key: [u8; 32],
    pub pseudo: String,
    pub password: String,
    pub device: Option<[u8; 32]>,
    /// Dossier du nœud iroh du jeu (relais de région joignable de partout).
    pub node_dir: Option<PathBuf>,
}

pub struct GarantWorker {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl GarantWorker {
    /// Lance le fil. `maps` : cartes de ce PC (signature, dossier) ; `maps_root` : où
    /// ranger une carte téléchargée.
    pub fn start(login: Login, maps: Vec<([u8; 32], PathBuf)>, maps_root: PathBuf) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = std::thread::Builder::new()
            .name("garant".into())
            .spawn(move || run(login, maps, maps_root, flag))
            .ok();
        Self { stop, thread }
    }

    /// Le fil tourne encore.
    pub fn alive(&self) -> bool {
        self.thread.as_ref().is_some_and(|t| !t.is_finished())
    }
}

impl Drop for GarantWorker {
    /// Fin de sa propre partie : il rend la partie servie (le garant en attente reprend) et
    /// ferme sa connexion de garant.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Relais d'une région d'un monde de construction, hébergé par ce jeu pour d'autres
/// joueurs (voir `minelib_relay::shard`). Il leur transmet ce que publie le relais du monde.
pub struct ShardHost {
    pub region: [i32; 2],
    handle: minelib_relay::shard::ShardHandle,
    /// Connexion de garant (au relais du monde) : lots publiés, confirmations,
    /// accusations et passations d'une région souveraine.
    link: std::sync::mpsc::Sender<minelib_net::transport::Inbound>,
    /// Nœud iroh propre à ce relais (joignable de partout, sans gêner celui du jeu).
    _node: Option<Arc<minelib_p2p::Node>>,
}

/// Relais de secours d'un monde de construction, tenu par ce jeu pour les joueurs de ce
/// monde : il en garde une copie (historique, sacs de survie) et le reprend si le relais
/// du monde tombe (voir `minelib_relay::Handle::standby_begin`). Il reste lancé tant que ce
/// jeu tourne.
pub struct StandbyHost {
    pub room: u64,
    handle: minelib_relay::Handle,
    _node: Option<Arc<minelib_p2p::Node>>,
    /// Depuis quand le relais du monde ne répond plus.
    lost: Option<Instant>,
    taken: bool,
}

/// Le relais du monde ne répond plus depuis ce temps : le relais de secours reprend.
pub const TAKEOVER_AFTER: Duration = Duration::from_secs(5);

impl StandbyHost {
    /// Lance le relais de secours et annonce au relais du monde où le joindre.
    pub fn start(
        room: u64,
        root: [u8; 32],
        info: minelib_net::proto::StandbyInfo,
        link: &std::sync::mpsc::Sender<minelib_net::transport::Inbound>,
        p2p: bool,
    ) -> Option<Self> {
        let dir =
            std::env::temp_dir().join(format!("minelib-secours-{}-{room}", std::process::id()));
        let handle = minelib_relay::start(minelib_relay::Config {
            dir,
            bind: "0.0.0.0:0".into(),
            pow_bits: 20,
            name: "Relais de secours".into(),
        })
        .map_err(|e| log::warn!("relais de secours : {e:#}"))
        .ok()?;
        handle.standby_begin(room, root, info);
        let port = handle.addr.port();
        // Joignable de partout par iroh (sans donner d'adresse IP), sinon sur le réseau
        // local.
        let node = if p2p {
            minelib_p2p::Node::start_private().ok().map(Arc::new)
        } else {
            None
        };
        if let Some(n) = &node {
            n.serve(std::net::SocketAddr::from(([127, 0, 0, 1], port)));
        }
        minelib_net::transport::push(
            link,
            &minelib_net::proto::Request::StandbyReady {
                node: node.as_ref().map_or([0; 32], |n| n.id()),
                addr: if node.is_some() {
                    String::new()
                } else {
                    format!("{}:{port}", lan_ip())
                },
                key: handle.public,
            },
        );
        Some(Self {
            room,
            handle,
            _node: node,
            lost: None,
            taken: false,
        })
    }

    /// Le monde repris ici (son numéro), une fois qu'un joueur y est entré.
    #[cfg(test)]
    pub fn resumed(&self) -> Option<u64> {
        self.handle.resumed()
    }

    /// Ce que le relais du monde envoie de son monde.
    pub fn feed(&self, msg: &minelib_net::proto::Response) {
        self.handle.standby_feed(msg);
    }

    /// Le relais du monde ne répond plus.
    pub fn root_lost(&mut self) {
        self.lost.get_or_insert_with(Instant::now);
    }

    /// À appeler régulièrement : reprend le monde quand le relais du monde est perdu
    /// depuis `after`. Vrai au moment de la reprise.
    pub fn tick(&mut self, after: Duration) -> bool {
        if self.taken || !self.lost.is_some_and(|t| t.elapsed() >= after) {
            return false;
        }
        self.taken = self.handle.take_over();
        self.taken
    }
}

/// Adresse de ce PC sur le réseau local (sans rien envoyer), sinon la boucle locale.
fn lan_ip() -> std::net::IpAddr {
    std::net::UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| {
            s.connect("192.0.2.1:9")?;
            s.local_addr()
        })
        .map(|a| a.ip())
        .ok()
        .filter(|ip| !ip.is_unspecified())
        .unwrap_or(std::net::IpAddr::from([127, 0, 0, 1]))
}

impl ShardHost {
    /// Lance le relais de la région et annonce au relais du monde qu'il est prêt.
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        room: u64,
        region: [i32; 2],
        secret: [u8; 32],
        link: std::sync::mpsc::Sender<minelib_net::transport::Inbound>,
        p2p: bool,
        slot: u16,
        root: [u8; 32],
        signer: Option<minelib_net::ed25519_dalek::SigningKey>,
    ) -> Option<Self> {
        let handle = minelib_relay::shard::start("0.0.0.0:0", room, region, secret, slot, root)
            .map_err(|e| log::warn!("relais de région : {e:#}"))
            .ok()?;
        if let Some(k) = signer {
            handle.set_signer(k);
        }
        let port = handle.addr.port();
        // Hôte anonyme : joignable seulement par les relais d'iroh (pas d'adresse IP
        // donnée aux joueurs de la région, ni directe ni de réseau local).
        let node = if p2p {
            minelib_p2p::Node::start_private().ok().map(Arc::new)
        } else {
            None
        };
        if let Some(n) = &node {
            n.serve(std::net::SocketAddr::from(([127, 0, 0, 1], port)));
            // Les autres relais de la région se joignent comme les joueurs : directement
            // s'ils donnent une adresse (réseau local), sinon par un tunnel vers leur nœud.
            let n = n.clone();
            handle.set_dial(Arc::new(move |info: &minelib_net::proto::ShardInfo| {
                let (addr, key, remote, n) = (info.addr.clone(), info.key, info.node, n.clone());
                Box::new(move || {
                    if let Ok(a) = addr.parse::<std::net::SocketAddr>() {
                        if std::net::TcpStream::connect_timeout(&a, Duration::from_millis(800))
                            .is_ok()
                        {
                            return Ok((addr.clone(), Some(key)));
                        }
                    }
                    Ok((n.tunnel(remote)?.to_string(), Some(key)))
                })
            }));
        }
        minelib_net::transport::push(
            &link,
            &minelib_net::proto::Request::ShardReady {
                region,
                node: node.as_ref().map_or([0; 32], |n| n.id()),
                addr: if node.is_some() {
                    String::new()
                } else {
                    format!("{}:{port}", lan_ip())
                },
                key: handle.public,
            },
        );
        Some(Self {
            region,
            handle,
            link,
            _node: node,
        })
    }

    /// Du relais du monde : l'historique de la région, puis chaque modification publiée.
    pub fn seed(&self, data: &[u8]) {
        self.handle.seed(data);
    }

    /// Relais prêts de la région (liste du relais du monde), et son époque si elle est
    /// souveraine.
    pub fn set_peers(
        &self,
        list: &[minelib_net::proto::ShardInfo],
        sov: Option<minelib_net::proto::Sov>,
    ) {
        self.handle.set_peers(list, sov);
    }

    /// Hôte principal d'une région souveraine : modifications signées par le relais du
    /// monde à la place de leurs auteurs.
    pub fn root_edits(&self, edits: Vec<minelib_net::sovereign::Edit>) {
        self.handle.root_edits(edits);
    }

    /// Clé du compte de l'hôte (arrivée après le lancement du relais).
    pub fn set_signer(&self, key: minelib_net::ed25519_dalek::SigningKey) {
        self.handle.set_signer(key);
    }

    /// Région souveraine : ce relais en est l'hôte principal.
    #[cfg(test)]
    pub fn is_primary(&self) -> bool {
        self.handle.is_primary()
    }

    /// Région souveraine : modifications mises en ordre dans l'époque en cours.
    #[cfg(test)]
    pub fn ordered(&self) -> u64 {
        self.handle.ordered()
    }

    /// Essais : comme hôte principal, le prochain lot contiendra une modification
    /// inventée.
    #[cfg(test)]
    pub fn forge_once(&self) {
        self.handle.forge_once();
    }

    /// Relié à ce nombre d'autres relais de la région.
    #[cfg(test)]
    pub fn linked(&self) -> usize {
        self.handle.linked()
    }

    /// Joueurs entrés dans ce relais.
    #[cfg(test)]
    pub fn players(&self) -> usize {
        self.handle.players()
    }

    /// À appeler régulièrement : liens vers les autres relais de la région, et ce que le
    /// relais doit dire au relais du monde (par la connexion de garant).
    pub fn pump(&self) {
        self.handle.tick();
        for req in self.handle.take_outbox() {
            minelib_net::transport::push(&self.link, &req);
        }
    }
}

fn local_dir(
    maps: &[([u8; 32], PathBuf)],
    root: &std::path::Path,
    sig: &[u8; 32],
) -> Option<PathBuf> {
    if let Some((_, d)) = maps.iter().find(|(s, _)| s == sig) {
        return Some(d.clone());
    }
    let dir = crate::fpsmap::downloaded_dir(root, sig);
    dir.join("level.txt").exists().then_some(dir)
}

fn run(login: Login, maps: Vec<([u8; 32], PathBuf)>, root: PathBuf, stop: Arc<AtomicBool>) {
    let client = Client::spawn_garant(login.addr.clone(), login.key);
    client.send(Cmd::Login {
        pseudo: login.pseudo.clone(),
        password: login.password.clone(),
        totp: None,
        backup: None,
        device: login.device,
    });
    let mut logged = false;
    let mut link = None;
    // Clé du compte : signe ce que publie le relais de région quand il est hôte principal.
    let mut identity: Option<minelib_net::ed25519_dalek::SigningKey> = None;
    let mut garant: Option<Garant> = None;
    // Garant extérieur d'un monde de construction : il valide les modifications de ses
    // zones (sans avoir besoin du monde lui-même).
    let mut build: Option<(crate::buildnet::BuildNet, minelib_world::World)> = None;
    // Relais d'une région d'un monde de construction.
    let mut shard: Option<ShardHost> = None;
    // Relais de secours d'un monde de construction.
    let mut standby: Option<StandbyHost> = None;
    let mut last = Instant::now();
    let started = Instant::now();
    let mut wiped = Instant::now();
    log::info!("garant en jeu : connexion de garant ouverte");
    loop {
        if stop.load(Ordering::Relaxed) {
            if garant.is_some() || build.is_some() || shard.is_some() {
                client.send(Cmd::Leave);
            }
            client.send(Cmd::GarantAvailable(false));
            client.send(Cmd::Logout);
            // Le temps que ces messages partent.
            std::thread::sleep(Duration::from_millis(300));
            log::info!("garant en jeu : connexion de garant fermée");
            return;
        }
        if let Some(s) = &mut standby {
            if s.tick(TAKEOVER_AFTER) {
                log::info!(
                    "garant en jeu : le relais de secours reprend le monde {}",
                    s.room
                );
            }
        }
        while let Some(e) = client.poll() {
            match e {
                Event::LoggedIn { .. } => {
                    logged = true;
                    client.send(Cmd::GarantAvailable(true));
                }
                Event::Link(l) => link = Some(l),
                Event::NeedTotp | Event::Offline(_) if !logged => {
                    log::info!("garant en jeu : connexion impossible, abandon");
                    return;
                }
                Event::GarantTask {
                    room,
                    map_sig,
                    map,
                    members,
                    standby,
                } => {
                    let Some(l) = link.clone() else {
                        client.send(Cmd::Leave);
                        continue;
                    };
                    let dir = local_dir(&maps, &root, &map_sig);
                    if dir.is_none() {
                        client.send(Cmd::DownloadMap(map_sig));
                    }
                    garant = Some(Garant::new(
                        room,
                        map_sig,
                        map,
                        members,
                        standby,
                        l,
                        login.pseudo.clone(),
                        dir,
                    ));
                }
                Event::Arbitrate { case, map_sig, map } if garant.is_none() => {
                    if let Some(l) = link.clone() {
                        let dir = local_dir(&maps, &root, &map_sig);
                        if dir.is_none() {
                            client.send(Cmd::DownloadMap(map_sig));
                        }
                        garant = Some(Garant::arbiter(case, map_sig, map, l, dir));
                    }
                }
                Event::Evidence {
                    case,
                    part,
                    of,
                    ref data,
                } => {
                    if let Some(g) = &mut garant {
                        g.evidence(case, part, of, data);
                    }
                }
                Event::MapDownloaded { sig, ref bytes } => {
                    if let Some(g) = garant.as_mut().filter(|g| g.needs_map() && g.sig == sig) {
                        match crate::fpsmap::unpack(&root, &g.map, bytes) {
                            Ok(dir) => g.set_dir(dir),
                            Err(e) => log::warn!("garant en jeu : carte : {e:#}"),
                        }
                    }
                }
                Event::StandbyTask { room, root, info } => {
                    if let Some(l) = &link {
                        log::info!("garant en jeu : relais de secours du monde {room}");
                        standby = StandbyHost::start(room, root, info, l, login.node_dir.is_some());
                        if standby.is_none() {
                            client.send(Cmd::Leave);
                        }
                    }
                }
                Event::Replica(ref r) => {
                    if let Some(s) = &standby {
                        s.feed(r);
                    }
                }
                // Le relais du monde ne répond plus : le relais de secours (s'il y en a un)
                // reprendra le monde si ça dure ; le relais de région, lui, n'a plus rien à
                // transmettre.
                Event::Lost if logged => {
                    if let Some(s) = &mut standby {
                        s.root_lost();
                    }
                    shard = None;
                }
                Event::RoomClosed => {
                    // (Le monde fermé par son relais : plus rien à garder ; repris, le relais
                    // de secours sert désormais ce monde, il reste.)
                    if standby.as_ref().is_some_and(|s| !s.taken) {
                        standby = None;
                    }
                    garant = None;
                    build = None;
                    if let Some(s) = shard.take() {
                        s.pump();
                    }
                }
                Event::ShardPeers {
                    region,
                    ref list,
                    ref sov,
                } => {
                    if let Some(s) = shard.as_ref().filter(|s| s.region == region) {
                        s.set_peers(list, sov.clone());
                    }
                }
                Event::RootEdits { region, ref edits } => {
                    if let Some(s) = shard.as_ref().filter(|s| s.region == region) {
                        s.root_edits(edits.clone());
                    }
                }
                Event::Identity(ref key) => {
                    identity = Some(key.clone());
                    if let Some(s) = &shard {
                        s.set_signer(key.clone());
                    }
                }
                Event::ShardTask {
                    room,
                    region,
                    secret,
                    slot,
                    root,
                } => {
                    if let Some(l) = link.clone() {
                        log::info!(
                            "garant en jeu : relais de la région {region:?} du monde {room}"
                        );
                        shard = ShardHost::start(
                            room,
                            region,
                            secret,
                            l,
                            login.node_dir.is_some(),
                            slot,
                            root,
                            identity.clone(),
                        );
                        if shard.is_none() {
                            client.send(Cmd::Leave);
                        }
                    }
                }
                Event::BuildGarant(room) => {
                    if let Some(l) = link.clone() {
                        log::info!("garant en jeu : zones du monde {room}");
                        let net = crate::buildnet::BuildNet::new(l, login.pseudo.clone(), false);
                        build = Some((net, minelib_world::World::new(0)));
                    }
                }
                Event::Zones {
                    ref gained,
                    ref lost,
                } => {
                    if let Some((b, _)) = &mut build {
                        b.zones_changed(gained, lost);
                    }
                }
                Event::FromZone {
                    peer,
                    chunk,
                    ref data,
                } => {
                    if let Some((b, w)) = &mut build {
                        b.receive(peer, chunk, data, w);
                    }
                    if let Some(s) = &shard {
                        s.seed(data);
                    }
                }
                Event::PeerJoined { .. }
                | Event::PeerLeft(_)
                | Event::FromPeer { .. }
                | Event::Witness { .. }
                | Event::FromHost(_)
                | Event::Promoted { .. }
                | Event::HostChanged(_)
                | Event::BackupIs(_) => {
                    if let Some(g) = &mut garant {
                        g.event(&e);
                    }
                }
                _ => {}
            }
        }
        // Pas de réponse du relais : on n'insiste pas.
        if !logged && started.elapsed() > Duration::from_secs(30) {
            log::info!("garant en jeu : pas de session, abandon");
            return;
        }
        // Le monde du garant extérieur ne sert à rien (il ne fait que valider) : on vide
        // de temps en temps les modifications qu'il a gardées.
        if let Some((_, w)) = &mut build {
            if wiped.elapsed() > Duration::from_secs(600) {
                wiped = Instant::now();
                *w = minelib_world::World::new(0);
            }
        }
        if let Some(s) = &shard {
            s.pump();
        }
        let dt = last.elapsed().as_secs_f32().min(0.1);
        last = Instant::now();
        if let Some(g) = &mut garant {
            for cmd in g.update(dt) {
                client.send(cmd);
            }
            if g.done {
                garant = None;
            }
        }
        let busy = garant.is_some() || shard.is_some();
        std::thread::sleep(Duration::from_millis(if busy { 2 } else { 20 }));
    }
}
