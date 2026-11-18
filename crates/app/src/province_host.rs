//! Provinces (v138), côté jeu : ce jeu tient le relais d'une province d'un monde de
//! construction (`ProvinceHost`, un relais embarqué, voir `minelib_relay::provinces`), ou
//! le relais d'une région rattaché au relais de sa province (`RegionAttach`).
//!
//! Tout passe par la connexion de garant du jeu au relais du monde : la tâche, l'historique,
//! les baux, et la copie de ce que publie la province. (v143) Pour une province d'un pays,
//! seule la tâche vient du relais du monde : tout le reste passe par une connexion au relais
//! du pays (`Up`).

use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::{Duration, Instant};

use minelib_net::client::{Client, Cmd, Event};
use minelib_net::proto::{self, Request, Response};
use minelib_net::transport::{self, Inbound};

/// Une province sans bail depuis ce temps (relais du monde injoignable) : son relais
/// s'arrête (le relais du monde l'a reprise).
const LOST_AFTER: Duration = Duration::from_secs(10);

/// (v144) Province d'un pays dont le relais est tombé : elle essaie de se rattacher au
/// relais de secours du pays (toutes les `REATTACH_EVERY`) pendant au plus ce temps.
const REATTACH_MAX: Duration = Duration::from_secs(9);
const REATTACH_EVERY: Duration = Duration::from_millis(400);

/// (v143) Province d'un pays : où joindre le relais du pays (avec le jeton du relais du
/// monde), qui l'on est (pseudo, clé du compte), d'où passer par iroh.
#[derive(Clone)]
pub struct Up {
    pub at: proto::ProvinceAt,
    pub pseudo: String,
    pub signer: [u8; 32],
    pub node_dir: Option<PathBuf>,
}

/// (v143) Connexion au relais du pays : ce qui attend qu'elle soit prête, et si elle est
/// perdue (le pays est tombé : la province s'arrête à la fin de son bail).
struct UpLink {
    client: Client,
    link: Option<Sender<Inbound>>,
    queue: Vec<Request>,
    lost: bool,
}

/// Relais d'une province tenu par ce jeu (ou, v141, son relais de secours).
pub struct ProvinceHost {
    pub room: u64,
    pub province: [i32; 2],
    pub epoch: u64,
    handle: minelib_relay::Handle,
    _node: Option<Arc<minelib_p2p::Node>>,
    /// Il a servi (bail en cours) au dernier passage, ou depuis quand plus.
    lost_since: Option<Instant>,
    pub stopped: bool,
    /// (v141) Relais de secours : il suit le relais de la province (par cette connexion,
    /// tant qu'elle tient) jusqu'à prendre sa place ; il a dit au relais du monde que sa
    /// copie est à jour.
    pub standby: bool,
    follow: Option<Client>,
    follow_link: Option<Sender<Inbound>>,
    standby_said: bool,
    /// (v144) Relais de secours : jusqu'où sa copie est à jour (redit au relais de secours du
    /// pays s'il s'y rattache : il peut prendre la place même si la province est tombée avec
    /// le pays).
    last_have: Option<u64>,
    /// Où le joindre (nœud iroh, adresse, clé du relais).
    at: ([u8; 32], String, [u8; 32]),
    /// (v143) Province d'un pays : la connexion au relais du pays ; (v144) de quoi s'y
    /// rattacher de nouveau (taille, jeton, qui l'on est), où est le relais de secours du
    /// pays, depuis quand on essaie de s'y rattacher, prochain essai.
    up: Option<UpLink>,
    size: i32,
    parent: Option<Up>,
    parent_standby: Option<proto::ProvinceInfo>,
    reattach: Option<Instant>,
    next_try: Instant,
}

/// (v143, v148) Étage d'une province (`Some` : un pays, et la taille de ses provinces) et ceux
/// d'en dessous.
pub type Levels = (Option<proto::Nest>, Vec<proto::Nest>);

/// Relais embarqué d'une province, lancé : sa poignée, son nœud iroh, où le joindre.
type Launched = (
    minelib_relay::Handle,
    Option<Arc<minelib_p2p::Node>>,
    ([u8; 32], String, [u8; 32]),
);

impl ProvinceHost {
    /// Lance le relais de la province et annonce au relais du monde (par `link`) où le
    /// joindre.
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        room: u64,
        province: [i32; 2],
        size: i32,
        epoch: u64,
        secret: [u8; 32],
        root: [u8; 32],
        info: proto::StandbyInfo,
        settings: proto::ProvinceSettings,
        link: &Sender<Inbound>,
        p2p: bool,
        nest: Levels,
        parent: Option<Up>,
    ) -> Option<Self> {
        let (handle, node, at) = Self::launch(
            room, province, size, epoch, secret, root, info, settings, p2p, false, nest,
        )?;
        let ready = Request::ProvinceReady {
            room,
            province,
            epoch,
            node: at.0,
            addr: at.1.clone(),
            key: at.2,
            have: 0,
        };
        // (v143) Province d'un pays : au relais du pays (il lui envoie son historique dès
        // qu'elle entre).
        let up = parent
            .clone()
            .map(|u| Self::dial_up(room, province, size, epoch, u, false, Some(ready.clone())));
        if up.is_none() {
            transport::push(link, &ready);
        }
        log::info!("relais de province : province {province:?} du monde {room}");
        Some(Self {
            room,
            province,
            epoch,
            handle,
            _node: node,
            lost_since: None,
            stopped: false,
            standby: false,
            follow: None,
            follow_link: None,
            standby_said: false,
            last_have: None,
            at,
            up,
            size,
            parent,
            parent_standby: None,
            reattach: None,
            next_try: Instant::now(),
        })
    }

    /// (v143) Connexion au relais du pays, avec le jeton du relais du monde ; `first` part
    /// dès qu'elle est prête.
    #[allow(clippy::too_many_arguments)]
    fn dial_up(
        room: u64,
        province: [i32; 2],
        size: i32,
        epoch: u64,
        u: Up,
        standby: bool,
        first: Option<Request>,
    ) -> UpLink {
        let client = dial(&u.at.info, u.node_dir.clone());
        client.send(Cmd::ProvinceChild {
            room,
            province,
            size,
            epoch,
            pseudo: u.pseudo.clone(),
            signer: u.signer,
            token: u.at.token,
            standby,
        });
        log::info!(
            "province {province:?} : relais du pays {:?} (époque {epoch})",
            u.at.info.province
        );
        UpLink {
            client,
            link: None,
            queue: first.into_iter().collect(),
            lost: false,
        }
    }

    /// (v143) Province d'un pays (ou son relais de secours).
    pub fn is_child(&self) -> bool {
        self.up.is_some()
    }

    /// (v141) Relais de secours de la province : un relais embarqué qui suit celui de la
    /// province (`follow`, avec le jeton du relais du monde) sans servir personne, prêt à
    /// prendre sa place.
    #[allow(clippy::too_many_arguments)]
    pub fn standby(
        room: u64,
        province: [i32; 2],
        size: i32,
        epoch: u64,
        secret: [u8; 32],
        root: [u8; 32],
        info: proto::StandbyInfo,
        settings: proto::ProvinceSettings,
        follow: proto::ProvinceAt,
        pseudo: &str,
        signer: [u8; 32],
        node_dir: Option<PathBuf>,
        p2p: bool,
        parent: Option<proto::ProvinceAt>,
        nest: Levels,
    ) -> Option<Self> {
        let (handle, node, at) = Self::launch(
            room, province, size, epoch, secret, root, info, settings, p2p, true, nest,
        )?;
        // (v143) Relais de secours d'une province d'un pays : il se signale au relais du pays
        // (qui le promouvra), et suit la province comme les autres.
        let parent = parent.map(|at| Up {
            at,
            pseudo: pseudo.to_string(),
            signer,
            node_dir: node_dir.clone(),
        });
        let up = parent
            .clone()
            .map(|u| Self::dial_up(room, province, size, epoch, u, true, None));
        let client = dial(&follow.info, node_dir);
        // (v144) Avec l'adresse de son propre relais : les provinces d'un pays s'y
        // rattacheront s'il prend la place du pays.
        client.send(Cmd::ProvinceFollow {
            room,
            province,
            epoch,
            pseudo: pseudo.to_string(),
            signer,
            token: follow.token,
            node: at.0,
            addr: at.1.clone(),
            key: at.2,
        });
        log::info!("relais de secours : province {province:?} du monde {room}");
        Some(Self {
            room,
            province,
            epoch,
            handle,
            _node: node,
            lost_since: None,
            stopped: false,
            standby: true,
            follow: Some(client),
            follow_link: None,
            standby_said: false,
            last_have: None,
            at,
            up,
            size,
            parent,
            parent_standby: None,
            reattach: None,
            next_try: Instant::now(),
        })
    }

    /// Lance le relais embarqué de la province (`standby` : en relais de secours).
    #[allow(clippy::too_many_arguments)]
    fn launch(
        room: u64,
        province: [i32; 2],
        size: i32,
        epoch: u64,
        secret: [u8; 32],
        root: [u8; 32],
        info: proto::StandbyInfo,
        settings: proto::ProvinceSettings,
        p2p: bool,
        standby: bool,
        nest: Levels,
    ) -> Option<Launched> {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "minelib-province-{}-{room}-{n}",
            std::process::id()
        ));
        let handle = minelib_relay::start(minelib_relay::Config {
            dir,
            bind: "0.0.0.0:0".into(),
            pow_bits: 20,
            name: "Relais de province".into(),
        })
        .map_err(|e| log::warn!("relais de province : {e:#}"))
        .ok()?;
        if !handle.province_begin(
            room, province, size, epoch, secret, root, info, settings, standby, nest.0, nest.1,
        ) {
            return None;
        }
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
        let at = (
            node.as_ref().map_or([0; 32], |n| n.id()),
            if node.is_some() {
                String::new()
            } else {
                format!("{}:{port}", crate::garant_worker::lan_ip())
            },
            handle.public,
        );
        // (v147) Le relais sait où on le joint (un pays le donne à ses garants).
        handle.province_self(at.0, at.1.clone(), at.2);
        Some((handle, node, at))
    }

    /// Du relais du monde : `ProvinceSeed`, `ProvinceLeased`, `ProvinceStop`, et (v139) les
    /// sacs de survie, les réglages, les WorldEdit (à renvoyer à la connexion de garant : ce
    /// qu'il faut lui dire).
    pub fn event(&mut self, r: &Response) -> Vec<Request> {
        match r {
            Response::ProvinceSeed {
                room,
                province,
                epoch,
                entries,
                go,
            } if (*room, *province, *epoch) == (self.room, self.province, self.epoch) => {
                self.handle.province_seed(entries.clone(), *go);
                Vec::new()
            }
            Response::ProvinceLeased {
                room,
                province,
                epoch,
                t,
                ok,
                copies,
                dump,
                ms,
                call,
                more,
                dense,
                kids,
            } if (*room, *province) == (self.room, self.province) => {
                self.handle.province_leased(
                    *epoch,
                    *t,
                    *ok,
                    *copies,
                    *dump,
                    *ms,
                    call.clone(),
                    *more,
                    *dense,
                    *kids,
                );
                Vec::new()
            }
            Response::ProvinceArchived {
                room,
                province,
                epoch,
                n,
            } if (*room, *province) == (self.room, self.province) => {
                self.handle.province_archived(*epoch, *n);
                Vec::new()
            }
            Response::ProvinceStop {
                room,
                province,
                epoch,
            } if (*room, *province, *epoch) == (self.room, self.province, self.epoch) => {
                // (v143) Un pays attend d'abord que ses provinces lui aient tout rendu.
                let out = self.handle.province_stop(true);
                self.stopped = self.handle.province_stopped();
                out
            }
            // (v139) Sacs de survie prêtés ou rappelés, WorldEdit d'un OP : pour cette
            // province, à cette époque.
            Response::ProvinceBagLent {
                room,
                province,
                epoch,
                ..
            }
            | Response::ProvinceBagRecall {
                room,
                province,
                epoch,
                ..
            }
            | Response::ProvinceBulk {
                room,
                province,
                epoch,
                ..
            } if (*room, *province, *epoch) == (self.room, self.province, self.epoch) => {
                self.handle.province_event(r);
                Vec::new()
            }
            // (v145) Ses voisines, dites par son pays (annoncées à ses joueurs).
            Response::ProvinceNeighbours {
                room,
                province,
                epoch,
                ..
            } if (*room, *province, *epoch) == (self.room, self.province, self.epoch) => {
                self.handle.province_event(r);
                Vec::new()
            }
            // (v144) Où est le relais de secours de son pays (pour s'y rattacher s'il prend la
            // place du pays).
            Response::ProvinceParent {
                room,
                province,
                epoch,
                standby,
            } if (*room, *province, *epoch) == (self.room, self.province, self.epoch) => {
                self.parent_standby = standby.clone();
                Vec::new()
            }
            // Réglages du serveur (OP, bannis, modes) ; (v146) joueur tué par un OP.
            Response::ProvinceSettings { room, .. } | Response::ProvinceSlay { room, .. }
                if *room == self.room =>
            {
                self.handle.province_event(r);
                Vec::new()
            }
            // (v141) Relais de secours : le relais de la province est tombé (ou parti), il
            // prend sa place ; le relais du monde lui enverra ce qui manque à sa copie.
            Response::ProvincePromote {
                room,
                province,
                old,
                epoch,
            } if (*room, *province, *old) == (self.room, self.province, self.epoch)
                && self.standby =>
            {
                let Some(have) = self.handle.province_promote(*old, *epoch) else {
                    return Vec::new();
                };
                self.epoch = *epoch;
                self.standby = false;
                self.follow = None;
                self.follow_link = None;
                self.lost_since = None;
                vec![Request::ProvinceReady {
                    room: self.room,
                    province: self.province,
                    epoch: self.epoch,
                    node: self.at.0,
                    addr: self.at.1.clone(),
                    key: self.at.2,
                    have,
                }]
            }
            _ => Vec::new(),
        }
    }

    /// À appeler souvent : ce qu'il faut envoyer au relais du monde (bail, copie de ce qui
    /// a été publié, relais de région prêts). Une province sans bail depuis `LOST_AFTER`
    /// s'arrête (`stopped`).
    pub fn tick(&mut self) -> Vec<Request> {
        if self.stopped {
            return Vec::new();
        }
        // (v143) Province d'un pays : tout passe par le relais du pays.
        if self.up.is_some() {
            let mut events = Vec::new();
            let mut linked = false;
            let reattaching = self.reattach.is_some();
            if let Some(u) = self.up.as_mut() {
                while let Some(e) = u.client.poll() {
                    match e {
                        Event::Link(l) => {
                            for r in u.queue.drain(..) {
                                transport::push(&l, &r);
                            }
                            u.link = Some(l);
                            linked = true;
                        }
                        Event::Province(r) => events.push(r),
                        Event::RoomClosed | Event::Error(_) | Event::Offline(_) | Event::Lost => {
                            if !u.lost && !reattaching {
                                log::info!("province : plus de nouvelles du relais du pays");
                            }
                            u.lost = true;
                            u.link = None;
                            break;
                        }
                        _ => {}
                    }
                }
            }
            // (v144) Rattachée au relais de secours du pays, qui a pris sa place.
            let mut out = Vec::new();
            if linked && self.reattach.take().is_some() {
                log::info!(
                    "province {:?} : rattachée au relais de secours de son pays",
                    self.province
                );
                // (Relais de secours : il redit tout de suite au nouveau pays jusqu'où sa copie
                // est à jour — la province a pu tomber avec l'ancien.)
                if let (true, Some(have)) = (self.standby, self.last_have) {
                    out.push(Request::ProvinceStandbyReady {
                        room: self.room,
                        province: self.province,
                        epoch: self.epoch,
                        have,
                    });
                }
            }
            for r in events {
                out.extend(self.event(&r));
            }
            out.extend(self.tick_own());
            // (v144) Le pays est tombé : elle se rattache au relais de secours du pays dès
            // qu'il a pris sa place (même jeton) ; sans lui, ou trop tard, plus de bail
            // possible : à la fin du sien, elle s'arrête.
            if self.up.as_ref().is_some_and(|u| u.lost) && !self.stopped {
                let since = *self.reattach.get_or_insert_with(Instant::now);
                let target = self
                    .parent_standby
                    .clone()
                    .filter(|_| since.elapsed() < REATTACH_MAX);
                match (target, self.parent.clone()) {
                    (Some(info), Some(p)) => {
                        if Instant::now() >= self.next_try {
                            self.next_try = Instant::now() + REATTACH_EVERY;
                            let queue = self.up.take().map(|u| u.queue).unwrap_or_default();
                            let mut u = Self::dial_up(
                                self.room,
                                self.province,
                                self.size,
                                self.epoch,
                                Up {
                                    at: proto::ProvinceAt {
                                        info,
                                        token: p.at.token,
                                    },
                                    ..p
                                },
                                self.standby,
                                None,
                            );
                            u.queue = queue;
                            self.up = Some(u);
                        }
                    }
                    _ if self.standby || !self.handle.province_serving() => {
                        log::info!("province : relais du pays perdu, arrêt");
                        self.stopped = true;
                        self.handle.province_stop(false);
                        out.clear();
                    }
                    _ => {}
                }
            }
            if let Some(u) = self.up.as_mut() {
                match &u.link {
                    Some(l) => {
                        for r in &out {
                            transport::push(l, r);
                        }
                    }
                    None => u.queue.extend(out),
                }
            }
            return Vec::new();
        }
        self.tick_own()
    }

    /// Bail, copie de ce qui a été publié, relais de région prêts (ou, relais de secours, sa
    /// copie) : ce qu'il faut envoyer plus haut.
    fn tick_own(&mut self) -> Vec<Request> {
        if self.stopped {
            return Vec::new();
        }
        // (v141) Relais de secours : sa copie suit le relais de la province.
        if self.standby {
            let mut out = Vec::new();
            let mut lost = false;
            if let Some(f) = &self.follow {
                while let Some(e) = f.poll() {
                    match e {
                        Event::Link(l) => self.follow_link = Some(l),
                        Event::Province(Response::ProvinceSeed { entries, .. }) => {
                            self.handle.province_seed(entries, None);
                        }
                        // (v144) Un pays : ses provinces (pour les reprendre avec leurs hôtes).
                        Event::Province(Response::ProvinceChildren {
                            epoch,
                            children,
                            gone,
                            full,
                            ..
                        }) => {
                            self.handle.province_children(epoch, children, gone, full);
                        }
                        Event::Province(Response::ProvinceCopy {
                            epoch,
                            n,
                            entries,
                            bags,
                            held,
                            spots,
                            ..
                        }) => {
                            let have = self
                                .handle
                                .province_copy(epoch, n, entries, bags, held, spots);
                            // (v142) Copié : son relais peut le confirmer à ses joueurs quand
                            // c'est ce relais de secours qui garde la copie.
                            if let (Some(_), Some(l)) = (have, &self.follow_link) {
                                transport::push(
                                    l,
                                    &Request::ProvinceCopied {
                                        room: self.room,
                                        province: self.province,
                                        epoch,
                                        n,
                                    },
                                );
                            }
                            if have.is_some() {
                                self.last_have = have;
                            }
                            if let (Some(have), false) = (have, self.standby_said) {
                                self.standby_said = true;
                                out.push(Request::ProvinceStandbyReady {
                                    room: self.room,
                                    province: self.province,
                                    epoch: self.epoch,
                                    have,
                                });
                            }
                        }
                        Event::RoomClosed | Event::Error(_) | Event::Offline(_) | Event::Lost => {
                            lost = true;
                            break;
                        }
                        _ => {}
                    }
                }
            }
            // (Le relais suivi est tombé : on attend que le relais du monde décide.)
            if lost {
                log::info!("relais de secours : plus de nouvelles du relais de la province");
                self.follow = None;
                self.follow_link = None;
            }
            return out;
        }
        if self.handle.province_serving() {
            self.lost_since = None;
        } else if self.lost_since.get_or_insert_with(Instant::now).elapsed() > LOST_AFTER {
            log::info!("relais de province : plus de bail depuis longtemps, arrêt");
            self.stopped = true;
            return self.handle.province_stop(false);
        }
        let out = self.handle.province_tick();
        // (v143) Un pays qui s'arrêtait : ses provinces lui ont tout rendu, il a rendu tout.
        if self.handle.province_stopped() {
            self.stopped = true;
        }
        out
    }

    /// Départ propre (le jeu se ferme) : tout part au relais du monde, qui reprend la
    /// province tout de suite.
    pub fn release(&mut self) -> Vec<Request> {
        if self.stopped {
            return Vec::new();
        }
        self.stopped = true;
        self.handle.province_stop(true)
    }

    /// Essais : sa chute (plus rien ne part, plus personne n'est servi).
    #[cfg(test)]
    pub fn kill(&self) {
        self.handle.kill();
    }

    /// Essais : changements de chunk reçus de joueurs (v145 : un pays n'en reçoit plus de
    /// ceux qui sont au fond de ses provinces).
    #[cfg(test)]
    pub fn positions_seen(&self) -> usize {
        self.handle.positions_seen()
    }

    /// Essais (v147) : hôtes de ses provinces qu'un pays a choisis parmi ses joueurs.
    #[cfg(test)]
    pub fn local_tasks(&self) -> usize {
        self.handle.local_tasks()
    }

    /// Essais (v148) : régions par côté de sa province.
    #[cfg(test)]
    pub fn size_of(&self) -> i32 {
        self.size
    }

    /// Essais : il sert (bail en cours).
    #[cfg(test)]
    pub fn serving(&self) -> bool {
        !self.stopped && self.handle.province_serving()
    }

    /// Essais : entrées gardées ici pour les zones de la province `p` (de la taille de ses
    /// provinces, pour un pays).
    #[cfg(test)]
    pub fn entries_of(&self, p: [i32; 2]) -> usize {
        self.handle.province_entries(self.room, p)
    }

    /// Essais : relais de secours dont la copie est à jour.
    #[cfg(test)]
    pub fn standby_ready(&self) -> bool {
        self.standby && self.standby_said
    }

    /// Essais : messages de voix remis par ce relais de province.
    #[cfg(test)]
    pub fn voice_relayed(&self) -> usize {
        self.handle.voice_relayed()
    }
}

/// Relais d'une région rattaché au relais de sa province : la connexion à ce relais (qui
/// tient lieu de relais du monde pour la région), et l'historique arrivé avant qu'elle ne
/// soit prête.
pub struct RegionAttach {
    pub room: u64,
    pub region: [i32; 2],
    pub province: [i32; 2],
    /// (v155) Son billet pour les autres relais de la région (signé par le relais du monde).
    pub peer: (Vec<u8>, u64),
    pub slot: u16,
    pub root: [u8; 32],
    pub client: Client,
    pub link: Option<Sender<Inbound>>,
    pub early: Vec<Vec<u8>>,
}

impl RegionAttach {
    /// Se connecte au relais de la province (directement, sinon par un tunnel iroh vers son
    /// hôte) avec le jeton du relais du monde.
    #[allow(clippy::too_many_arguments)]
    pub fn connect(
        room: u64,
        region: [i32; 2],
        peer: (Vec<u8>, u64),
        slot: u16,
        root: [u8; 32],
        at: proto::ProvinceAt,
        pseudo: &str,
        signer: [u8; 32],
        node_dir: Option<PathBuf>,
    ) -> Self {
        let client = dial(&at.info, node_dir);
        client.send(Cmd::RegionHost {
            room,
            region,
            slot,
            pseudo: pseudo.to_string(),
            signer,
            token: at.token,
        });
        Self {
            room,
            region,
            province: at.info.province,
            peer,
            slot,
            root,
            client,
            link: None,
            early: Vec::new(),
        }
    }
}

/// Connexion à un relais de province : directement s'il donne une adresse (réseau local),
/// sinon par un tunnel iroh vers le nœud de son hôte.
pub fn dial(info: &proto::ProvinceInfo, node_dir: Option<PathBuf>) -> Client {
    let (addr, key, node) = (info.addr.clone(), info.key, info.node);
    Client::spawn_resolved(Box::new(move || {
        if let Ok(a) = addr.parse::<std::net::SocketAddr>() {
            if std::net::TcpStream::connect_timeout(&a, Duration::from_millis(800)).is_ok() {
                return Ok((addr.clone(), Some(key)));
            }
        }
        let n = node_dir
            .as_deref()
            .and_then(crate::account::p2p_node)
            .ok_or_else(|| anyhow::anyhow!("relais de province injoignable"))?;
        Ok((n.tunnel(node)?.to_string(), Some(key)))
    }))
}

/// Événement d'un relais de province pour le relais de région qui s'y rattache :
/// `Some(vrai)` s'il faut fermer ce relais de région.
pub fn attach_event(
    a: &mut RegionAttach,
    shard: &mut Option<crate::garant_worker::ShardHost>,
    e: Event,
    identity: Option<&minelib_net::ed25519_dalek::SigningKey>,
    p2p: bool,
) -> bool {
    match e {
        Event::Link(l) => {
            a.link = Some(l.clone());
            *shard = crate::garant_worker::ShardHost::start(
                a.room,
                a.region,
                a.peer.clone(),
                l,
                p2p,
                a.slot,
                a.root,
                identity.cloned(),
            );
            if let Some(s) = shard {
                for d in std::mem::take(&mut a.early) {
                    s.seed(&d);
                }
            }
            shard.is_none()
        }
        Event::FromZone { ref data, .. } => {
            match shard {
                Some(s) => s.seed(data),
                None => a.early.push(data.clone()),
            }
            false
        }
        Event::ShardPeers { region, list, sov } => {
            if let Some(s) = shard.as_ref().filter(|s| s.region == region) {
                s.set_peers(&list, sov);
            }
            false
        }
        Event::RootEdits {
            region,
            edits,
            certs,
        } => {
            if let Some(s) = shard.as_ref().filter(|s| s.region == region) {
                s.root_edits(edits, certs);
            }
            false
        }
        Event::WorldBeat { paused, .. } => {
            if let Some(s) = shard.as_ref() {
                s.world_beat(paused);
            }
            false
        }
        Event::Audit { .. } => {
            if let (Some(r), Some(l)) = (crate::garant_worker::audit_reply(&e), &a.link) {
                transport::push(l, &r);
            }
            false
        }
        Event::RoomClosed | Event::Offline(_) | Event::Error(_) | Event::Lost => true,
        _ => false,
    }
}
