//! Garant pendant qu'on joue : une deuxième connexion du compte au même relais, dans son
//! propre fil, sert (ou vérifie) la partie FPS d'autres joueurs pendant que ce joueur joue
//! la sienne. Le relais le choisit de préférence quand sa partie vient de commencer (il a
//! tout intérêt à rester en jeu) et le fait relever par un autre garant avant qu'elle ne
//! se termine. Rien ne passe par le fil du jeu : les images du joueur ne ralentissent pas.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use minelib_net::client::{Client, Cmd, Event};
use minelib_net::guard;

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
/// monde : il en garde une copie (historique, sacs de survie) et le reprend quand une
/// majorité des gardiens du monde n'entend plus le relais du monde, ou quand celui-ci
/// s'arrête proprement (voir `minelib_net::guard`). Il reste lancé tant que ce jeu tourne.
pub struct StandbyHost {
    pub room: u64,
    handle: minelib_relay::Handle,
    _node: Option<Arc<minelib_p2p::Node>>,
    pub taken: bool,
    /// Depuis quand il a voté sans reprendre le monde (voir `STANDBY_GIVE_UP`).
    voted_at: Option<Instant>,
}

/// Un relais de secours qui a voté sans pouvoir reprendre le monde (les autres gardiens
/// entendaient encore le relais du monde) s'arrête au bout de ce temps.
const STANDBY_GIVE_UP: Duration = Duration::from_secs(600);

impl StandbyHost {
    /// Lance le relais de secours (tenu par le joueur `me`) et annonce au relais du monde
    /// où le joindre.
    pub fn start(
        room: u64,
        root: [u8; 32],
        info: minelib_net::proto::StandbyInfo,
        link: &std::sync::mpsc::Sender<minelib_net::transport::Inbound>,
        p2p: bool,
        me: &str,
    ) -> Option<Self> {
        // (Un dossier à lui : un autre relais de secours de ce jeu, d'avant ou d'un autre
        // relais du monde au même numéro de monde, ne s'y mélange pas.)
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("minelib-secours-{}-{room}-{n}", std::process::id()));
        let handle = minelib_relay::start(minelib_relay::Config {
            dir,
            bind: "0.0.0.0:0".into(),
            pow_bits: 20,
            name: "Relais de secours".into(),
        })
        .map_err(|e| log::warn!("relais de secours : {e:#}"))
        .ok()?;
        handle.standby_begin(room, root, info, me);
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
            taken: false,
            voted_at: None,
        })
    }

    /// Battement du relais du monde : le bail à lui répondre (aucun : ce relais de
    /// secours a voté pour la reprise, il ne le soutient plus).
    pub fn beat(
        &self,
        t: u64,
        committee: &guard::Committee,
    ) -> Option<minelib_net::proto::Request> {
        let version = self.handle.standby_beat(committee)?;
        Some(minelib_net::proto::Request::Lease {
            room: self.room,
            version,
            t,
        })
    }

    /// Le relais du monde s'arrête proprement et lui confie le monde (arrêt signé).
    pub fn released(&mut self, room: u64, sig: &[u8]) -> bool {
        if room == self.room && self.handle.standby_released(room, sig) {
            self.taken = true;
        }
        self.taken
    }

    /// Une liste plus récente nomme un autre relais de secours (et celui-ci n'a pas voté),
    /// ou il a voté il y a longtemps sans pouvoir reprendre le monde : il n'a plus rien à
    /// faire.
    pub fn replaced(&mut self) -> bool {
        if !self.taken && self.voted() {
            let t = *self.voted_at.get_or_insert_with(Instant::now);
            if t.elapsed() > STANDBY_GIVE_UP {
                return true;
            }
        }
        self.handle.standby_replaced()
    }

    /// Il a voté pour la reprise : il ne s'en va plus (la reprise peut lui revenir).
    pub fn voted(&self) -> bool {
        self.handle.standby_voted()
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

    /// À appeler régulièrement : vote quand le relais du monde se tait, reprend le monde
    /// avec une majorité des gardiens. Vrai au moment de la reprise.
    pub fn tick(&mut self) -> bool {
        if self.taken {
            return false;
        }
        self.taken = self.handle.standby_tick();
        self.taken
    }
}

/// Témoin d'un monde de construction (voir `minelib_net::guard`) : il répond aux
/// battements du relais du monde tant qu'il l'entend, et dit au relais de secours, quatre
/// fois par seconde, sa liste des gardiens et son vote. Il ne garde rien du monde ; il
/// peut l'être de plusieurs mondes à la fois.
pub struct WitnessRole {
    pub room: u64,
    keeper: guard::Keeper,
    cert: minelib_net::sovereign::Cert,
    /// Où joindre le relais de secours (dernier battement).
    at: Option<minelib_net::proto::StandbyAt>,
    /// Connexion au relais de secours, et sa file d'envoi une fois entré.
    client: Option<Client>,
    link: Option<std::sync::mpsc::Sender<minelib_net::transport::Inbound>>,
    next_try: Instant,
    last_report: Instant,
    /// Dossier du nœud iroh du jeu (relais de secours joint par un tunnel).
    node_dir: Option<PathBuf>,
    /// Dernier battement entendu (ou début du rôle) : un monde dont on n'entend plus
    /// parler depuis `WITNESS_GIVE_UP` (fermé, ou repris depuis longtemps) n'a plus besoin
    /// de ce témoin.
    heard: Instant,
}

/// Un témoin sans nouvelles du monde depuis ce temps s'arrête (son vote, s'il en a fait
/// un, reste : il ne redevient jamais gardien de ce monde).
const WITNESS_GIVE_UP: Duration = Duration::from_secs(120);

impl WitnessRole {
    pub fn new(
        room: u64,
        me: &str,
        root: [u8; 32],
        cert: minelib_net::sovereign::Cert,
        node_dir: Option<PathBuf>,
    ) -> Option<Self> {
        Some(Self {
            room,
            keeper: guard::Keeper::new(room, me, root)?,
            cert,
            at: None,
            client: None,
            link: None,
            next_try: Instant::now(),
            last_report: Instant::now(),
            node_dir,
            heard: Instant::now(),
        })
    }

    /// Battement du relais du monde : le bail à lui répondre (aucun : il a voté).
    pub fn beat(
        &mut self,
        t: u64,
        committee: &guard::Committee,
        at: Option<minelib_net::proto::StandbyAt>,
    ) -> Option<minelib_net::proto::Request> {
        if at.is_some() && at != self.at {
            // Nouveau relais de secours : on va lui parler.
            self.at = at;
            self.client = None;
            self.link = None;
            self.next_try = Instant::now();
        }
        self.heard = Instant::now();
        let version = self.keeper.beat(committee, guard::clock())?;
        Some(minelib_net::proto::Request::Lease {
            room: self.room,
            version,
            t,
        })
    }

    /// A-t-il voté pour la reprise ?
    pub fn voted(&self) -> bool {
        self.keeper.voted().is_some()
    }

    /// À appeler régulièrement, avec la clé du compte (pour entrer chez le relais de
    /// secours). Faux quand il n'est plus témoin de ce monde.
    pub fn pump(&mut self, key: Option<&minelib_net::ed25519_dalek::SigningKey>) -> bool {
        let now = guard::clock();
        if self.keeper.tick(now) {
            log::warn!(
                "témoin du monde {} : le relais du monde se tait depuis {} ms, vote pour la \
                 reprise",
                self.room,
                guard::VOTE_MS
            );
        }
        // Plus nommé dans la liste (sans vote en cours), ou plus de nouvelles du monde
        // depuis longtemps : fini.
        if (self.keeper.committee.is_some() && !self.keeper.member() && !self.voted())
            || self.heard.elapsed() > WITNESS_GIVE_UP
        {
            return false;
        }
        let mut broken = false;
        if let Some(c) = &self.client {
            while let Some(e) = c.poll() {
                match e {
                    Event::Link(l) => self.link = Some(l),
                    Event::Committee(c) => {
                        self.keeper.learn(&c, now);
                    }
                    Event::Error(_) | Event::Offline(_) | Event::Lost | Event::LoggedOut => {
                        broken = true;
                    }
                    _ => {}
                }
            }
        }
        if broken {
            self.client = None;
            self.link = None;
            self.next_try = Instant::now() + Duration::from_millis(guard::PROBE_MS * 4);
        }
        if self.client.is_none() && Instant::now() >= self.next_try {
            if let (Some(at), Some(key)) = (self.at.clone(), key) {
                let node = self.node_dir.clone();
                let c = Client::spawn_resolved(Box::new(move || {
                    if let Ok(a) = at.addr.parse::<std::net::SocketAddr>() {
                        if std::net::TcpStream::connect_timeout(&a, Duration::from_millis(800))
                            .is_ok()
                        {
                            return Ok((at.addr.clone(), Some(at.key)));
                        }
                    }
                    let n = node
                        .as_deref()
                        .and_then(crate::account::p2p_node)
                        .ok_or_else(|| anyhow::anyhow!("relais de secours injoignable"))?;
                    Ok((n.tunnel(at.node)?.to_string(), Some(at.key)))
                }));
                c.send(Cmd::WitnessLogin {
                    cert: self.cert.clone(),
                    key: key.clone(),
                });
                self.client = Some(c);
                self.next_try = Instant::now() + Duration::from_millis(guard::PROBE_MS * 4);
            }
        }
        if self.last_report.elapsed() >= Duration::from_millis(guard::PROBE_MS) {
            self.last_report = Instant::now();
            if let (Some(l), Some(c), Some(key)) = (&self.link, &self.keeper.committee, key) {
                // (Le vote, signé de la clé du compte : le relais de secours le montrera aux
                // joueurs.)
                let vote = self
                    .keeper
                    .voted()
                    .map(|v| guard::Vote::sign(key, self.room, v, &self.keeper.me));
                minelib_net::transport::push(
                    l,
                    &minelib_net::proto::Request::WitnessBeat {
                        committee: c.clone(),
                        vote,
                    },
                );
            }
        }
        true
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
    // Relais de secours d'un monde de construction, et témoin de mondes.
    let mut standby: Option<StandbyHost> = None;
    let mut witnesses: HashMap<u64, WitnessRole> = HashMap::new();
    // Mondes pour lesquels ce jeu a voté la reprise : un vote est pour toujours, il n'y
    // redevient jamais gardien (un nouveau rôle l'oublierait).
    let mut voted: std::collections::HashSet<u64> = Default::default();
    let mut last = Instant::now();
    let started = Instant::now();
    let mut wiped = Instant::now();
    log::info!("garant en jeu : connexion de garant ouverte");
    loop {
        if stop.load(Ordering::Relaxed) {
            // Gardien qui s'en va sans avoir voté : il le promet, il est remplacé tout de
            // suite.
            if let Some(l) = &link {
                let rooms = standby
                    .iter()
                    .filter(|s| !s.taken && !s.voted())
                    .map(|s| s.room)
                    .chain(witnesses.values().filter(|w| !w.voted()).map(|w| w.room));
                for room in rooms {
                    minelib_net::transport::push(l, &minelib_net::proto::Request::Resign { room });
                }
            }
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
            if s.tick() {
                log::info!(
                    "garant en jeu : le relais de secours reprend le monde {}",
                    s.room
                );
            }
            if s.replaced() {
                log::info!(
                    "garant en jeu : relais de secours du monde {} remplacé",
                    s.room
                );
                standby = None;
            }
        }
        witnesses.retain(|_, w| w.pump(identity.as_ref()));
        voted.extend(standby.iter().filter(|s| s.voted()).map(|s| s.room));
        voted.extend(witnesses.values().filter(|w| w.voted()).map(|w| w.room));
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
                // (Jamais gardien d'un monde où il a voté ; et un relais de secours qui sert un
                // monde repris, ou qui a voté, garde sa place.)
                Event::StandbyTask { room, .. }
                    if voted.contains(&room)
                        || standby.as_ref().is_some_and(|s| s.taken || s.voted()) =>
                {
                    client.send(Cmd::Leave);
                }
                Event::StandbyTask { room, root, info } => {
                    if let Some(l) = &link {
                        log::info!("garant en jeu : relais de secours du monde {room}");
                        standby = StandbyHost::start(
                            room,
                            root,
                            info,
                            l,
                            login.node_dir.is_some(),
                            &login.pseudo,
                        );
                        if standby.is_none() {
                            client.send(Cmd::Leave);
                        }
                    }
                }
                // Gardien d'un monde : battement du relais du monde, auquel on répond par
                // un bail tant qu'on l'entend.
                Event::Beat {
                    t,
                    ref committee,
                    standby: ref at,
                } => {
                    let room = committee.room;
                    let mut leases = Vec::new();
                    if let Some(s) = standby.as_ref().filter(|s| s.room == room) {
                        leases.extend(s.beat(t, committee));
                    }
                    if let Some(w) = witnesses.get_mut(&room) {
                        leases.extend(w.beat(t, committee, at.clone()));
                    }
                    if let Some(l) = &link {
                        for r in leases {
                            minelib_net::transport::push(l, &r);
                        }
                    }
                }
                // (Déjà témoin de ce monde, ou il y a voté : on garde ce qu'on a.)
                Event::WitnessTask { room, .. }
                    if voted.contains(&room) || witnesses.contains_key(&room) => {}
                Event::WitnessTask { room, root, cert } => {
                    log::info!("garant en jeu : témoin du monde {room}");
                    if let Some(w) =
                        WitnessRole::new(room, &login.pseudo, root, cert, login.node_dir.clone())
                    {
                        witnesses.insert(room, w);
                    }
                }
                Event::Released { room, ref sig } => {
                    if let Some(s) = &mut standby {
                        if s.released(room, sig) {
                            log::info!(
                                "garant en jeu : le relais du monde s'arrête, le relais de secours \
                                 reprend le monde {room}"
                            );
                        }
                    }
                }
                Event::Replica(ref r) => {
                    if let Some(s) = &standby {
                        s.feed(r);
                    }
                }
                // Le relais du monde ne répond plus : le relais de secours (s'il y en a un)
                // le reprendra avec les votes des gardiens (plus de battements) ; le relais de
                // région, lui, n'a plus rien à transmettre.
                Event::Lost if logged => {
                    shard = None;
                }
                Event::RoomClosed => {
                    // (Le monde fermé par son relais, ou ce relais de secours rendu : plus
                    // rien à garder ; repris, ou s'il a voté pour la reprise, il reste.)
                    if standby.as_ref().is_some_and(|s| !s.taken && !s.voted()) {
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
