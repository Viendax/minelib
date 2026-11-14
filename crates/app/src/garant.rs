//! Garant indépendant : quand le joueur est au menu, son jeu peut servir une partie FPS
//! d'autres joueurs (il ne la joue pas, il n'a aucun lien avec eux : choisi par le relais
//! selon `garant_ok`). Deux rôles :
//! - **hôte** : il fait tourner la partie (règles, tirs, grenades) sans personnage ;
//! - **en attente** : il reçoit chaque seconde une copie de la partie, la vérifie en
//!   rejouant tous les coups (chaîne des coups et positions de chacun) et signale les
//!   désaccords au relais ; il reprend la partie si l'hôte part ou triche.
//!
//! Dès que le joueur lance une partie, le garant s'en va : le garant en attente (ou un
//! joueur) reprend aussitôt, sans coupure.

use std::path::PathBuf;
use std::sync::mpsc::Sender;

use glam::Vec3;
use minelib_net::client::{Cmd, Event};
use minelib_net::game;
use minelib_net::transport::Inbound;
use minelib_world::{ChunkPos, World};

use crate::netgame::{NadeOut, NetGame};
use crate::player::{MoveInput, Player};

pub struct Garant {
    pub room: u64,
    pub sig: [u8; 32],
    pub map: String,
    standby: bool,
    members: Vec<(u64, String)>,
    link: Sender<Inbound>,
    pseudo: String,
    dir: Option<PathBuf>,
    world: Option<World>,
    net: Option<NetGame>,
    fps: crate::fps::Fps,
    phantom: Player,
    acc: f32,
    /// Événements reçus avant d'être prêt (carte en téléchargement).
    queue: Vec<Event>,
    ready_sent: bool,
    started_sent: bool,
}

impl Garant {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        room: u64,
        sig: [u8; 32],
        map: String,
        members: Vec<(u64, String)>,
        standby: bool,
        link: Sender<Inbound>,
        pseudo: String,
        dir: Option<PathBuf>,
    ) -> Self {
        Self {
            room,
            sig,
            map,
            standby,
            members,
            link,
            pseudo,
            dir,
            world: None,
            net: None,
            fps: crate::fps::Fps::new(),
            phantom: Player::new(Vec3::new(0.0, -500.0, 0.0)),
            acc: 0.0,
            queue: Vec::new(),
            ready_sent: false,
            started_sent: false,
        }
    }

    /// La carte est arrivée (téléchargée).
    pub fn set_dir(&mut self, dir: PathBuf) {
        self.dir = Some(dir);
    }

    pub fn needs_map(&self) -> bool {
        self.dir.is_none()
    }

    /// Charge la carte et démarre (une fois la carte là).
    fn start(&mut self) {
        let Some(dir) = &self.dir else { return };
        let Ok((_, mut world)) = crate::save::load_world(dir) else {
            log::warn!("garant : carte illisible");
            return;
        };
        world.ensure_chunk(ChunkPos { x: 0, z: 0 });
        let top = world.top_solid(8, 8).unwrap_or(64);
        let spawn = Vec3::new(8.5, top as f32 + 1.0, 8.5);
        let map = crate::fpsmap::load(dir);
        let mut net = if self.standby {
            NetGame::standby(self.link.clone(), self.sig)
        } else {
            NetGame::headless(self.link.clone(), self.pseudo.clone(), map, self.sig, spawn)
        };
        for (id, p) in &self.members {
            net.peer_joined(*id, p.clone());
        }
        self.world = Some(world);
        self.net = Some(net);
        log::info!(
            "garant : partie {} {}",
            self.room,
            if self.standby {
                "en attente (vérification)"
            } else {
                "servie"
            }
        );
        for e in std::mem::take(&mut self.queue) {
            self.event(&e);
        }
    }

    /// Événement du relais pour cette partie.
    pub fn event(&mut self, e: &Event) {
        let (Some(net), Some(world)) = (&mut self.net, &mut self.world) else {
            self.queue.push(e.clone());
            return;
        };
        let locals = match e {
            Event::PeerJoined { peer, pseudo } => {
                net.peer_joined(*peer, pseudo.clone());
                Vec::new()
            }
            Event::PeerLeft(peer) => {
                net.peer_left(*peer);
                Vec::new()
            }
            Event::FromPeer { peer, data } => net.receive(Some(*peer), data, world),
            Event::Witness { peer, data } => {
                net.witness(*peer, data);
                Vec::new()
            }
            Event::FromHost(data) => {
                // Garder les chunks autour des joueurs (vérification des coups).
                for p in net.positions() {
                    ensure_around(world, p);
                }
                net.receive(None, data, world)
            }
            Event::Promoted { members, old_host } => {
                let map = self
                    .dir
                    .as_ref()
                    .map(|d| crate::fpsmap::load(d))
                    .unwrap_or_default();
                net.promote(self.pseudo.clone(), members, &self.phantom, map, *old_host);
                self.standby = false;
                log::info!("garant : reprend la partie {}", self.room);
                Vec::new()
            }
            Event::HostChanged(_) => {
                net.host_changed();
                Vec::new()
            }
            Event::BackupIs(peer) => {
                net.designate_backup(*peer);
                Vec::new()
            }
            _ => Vec::new(),
        };
        self.apply(locals);
    }

    fn apply(&mut self, locals: Vec<crate::netgame::Local>) {
        for l in locals {
            match l {
                crate::netgame::Local::Thrown {
                    owner,
                    nade,
                    pos,
                    vel,
                } => self.fps.add_thrown(owner, nade, pos, vel),
                crate::netgame::Local::Started => self.started_sent = false,
                _ => {}
            }
        }
    }

    /// À chaque image : simulation (hôte) ; commandes à envoyer au relais.
    pub fn update(&mut self, dt: f32) -> Vec<Cmd> {
        let mut cmds = Vec::new();
        if self.net.is_none() {
            if self.dir.is_some() {
                self.start();
            }
            return cmds;
        }
        let (Some(net), Some(world)) = (&mut self.net, &mut self.world) else {
            return cmds;
        };
        let mut locals = Vec::new();
        if net.is_host() {
            for p in net.positions() {
                ensure_around(world, p);
            }
            let input = MoveInput {
                fps: true,
                ..Default::default()
            };
            self.acc = (self.acc + dt).min(crate::session::SIM_TICK * 32.0);
            while self.acc >= crate::session::SIM_TICK {
                net.on_tick(world, &self.phantom, &input, crate::session::SIM_TICK);
                self.acc -= crate::session::SIM_TICK;
            }
            self.fps.simulate_nades(dt, world);
            let nades = NadeOut {
                throws: Vec::new(),
                blasts: std::mem::take(&mut self.fps.blasts),
                fires: self.fps.fire_cells(),
            };
            locals = net.end_frame(world, Vec::new(), 255, nades, dt);
            if locals
                .iter()
                .any(|l| matches!(l, crate::netgame::Local::Started))
                && !self.started_sent
            {
                self.started_sent = true;
                cmds.push(Cmd::RoomStarted(true));
            }
        } else {
            // Vérificateur : le monde autour des joueurs, pour rejouer leurs pas et tirs.
            for p in net.positions() {
                ensure_around(world, p);
            }
            if net.has_backup() && !self.ready_sent {
                self.ready_sent = true;
                cmds.push(Cmd::GarantReady);
            }
            // Fin de partie rejouée : le relais n'accepte le résultat que s'il concorde.
            for s in net.take_results() {
                cmds.push(Cmd::MatchResult(game::encode(&s)));
            }
            for c in net.take_conflicts() {
                log::warn!("garant : désaccord avec l'hôte sur le joueur {}", c.player);
                cmds.push(Cmd::Conflict {
                    player: c.player,
                    seq: c.seq,
                    host_pos: c.host_pos,
                    audit_pos: c.audit_pos,
                    proof: c.proof,
                });
            }
        }
        self.apply(locals);
        cmds
    }
}

/// Chunks autour d'un joueur, générés si besoin.
fn ensure_around(world: &mut World, p: Vec3) {
    let c = ChunkPos::containing(p.x.floor() as i32, p.z.floor() as i32);
    for dz in -2..=2 {
        for dx in -2..=2 {
            world.ensure_chunk(ChunkPos {
                x: c.x + dx,
                z: c.z + dz,
            });
        }
    }
}
