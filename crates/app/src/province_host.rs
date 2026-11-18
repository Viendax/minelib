//! Provinces (v138), côté jeu : ce jeu tient le relais d'une province d'un monde de
//! construction (`ProvinceHost`, un relais embarqué, voir `minelib_relay::provinces`), ou
//! le relais d'une région rattaché au relais de sa province (`RegionAttach`).
//!
//! Tout passe par la connexion de garant du jeu au relais du monde : la tâche, l'historique,
//! les baux, et la copie de ce que publie la province.

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
    /// Où le joindre (nœud iroh, adresse, clé du relais).
    at: ([u8; 32], String, [u8; 32]),
}

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
    ) -> Option<Self> {
        let (handle, node, at) = Self::launch(
            room, province, size, epoch, secret, root, info, settings, p2p, false,
        )?;
        transport::push(
            link,
            &Request::ProvinceReady {
                room,
                province,
                epoch,
                node: at.0,
                addr: at.1.clone(),
                key: at.2,
                have: 0,
            },
        );
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
            at,
        })
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
    ) -> Option<Self> {
        let (handle, node, at) = Self::launch(
            room, province, size, epoch, secret, root, info, settings, p2p, true,
        )?;
        let client = dial(&follow.info, node_dir);
        client.send(Cmd::ProvinceFollow {
            room,
            province,
            epoch,
            pseudo: pseudo.to_string(),
            signer,
            token: follow.token,
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
            at,
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
            room, province, size, epoch, secret, root, info, settings, standby,
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
            } if (*room, *province) == (self.room, self.province) => {
                self.handle.province_leased(*epoch, *t, *ok, *copies, *dump);
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
                self.stopped = true;
                self.handle.province_stop(true)
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
            // Réglages du serveur (OP, bannis, modes).
            Response::ProvinceSettings { room, .. } if *room == self.room => {
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
                        Event::Province(Response::ProvinceCopy {
                            epoch,
                            n,
                            entries,
                            bags,
                            held,
                            ..
                        }) => {
                            let have = self.handle.province_copy(epoch, n, entries, bags, held);
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
        self.handle.province_tick()
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
    pub secret: [u8; 32],
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
        secret: [u8; 32],
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
            secret,
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
                a.secret,
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
