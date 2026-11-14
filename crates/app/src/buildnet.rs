//! Construction en ligne, sans limite de joueurs.
//!
//! Le relais découpe le monde en zones (`proto::ZONE` × `ZONE` chunks) et confie chaque
//! zone active à un garant, choisi parmi les joueurs selon ses capacités et sa charge
//! (`minelib_relay::zones`). Un joueur qui pose ou casse un bloc l'envoie au garant de la
//! zone ; le garant vérifie (bloc valide, dans la zone, pas plus de 20 par seconde) et
//! publie la modification : le relais la transmet aux joueurs proches, à l'hôte (qui
//! enregistre le monde) et la garde pour les arrivants. Les positions des joueurs ne vont
//! qu'aux joueurs proches.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::Sender;
use std::time::Instant;

use glam::Vec3;
use minelib_net::game;
use minelib_net::proto::{self, Request};
use minelib_net::transport::{self, Inbound};
use minelib_world::{BlockId, ChunkPos, World};
use serde::{Deserialize, Serialize};

/// Modifications acceptées par seconde et par joueur.
const MAX_EDITS_PER_S: u32 = 20;
/// Position envoyée 20 fois par seconde.
const POS_EVERY: f32 = 0.05;
/// Un joueur dont on n'a plus de nouvelles depuis ce temps n'est plus montré.
const STALE_S: f32 = 3.0;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum BuildMsg {
    /// Demande d'un joueur au garant.
    Edit { pos: [i32; 3], block: u16 },
    /// Modification validée par le garant.
    Set {
        pos: [i32; 3],
        block: u16,
        by: String,
    },
    Player {
        pseudo: String,
        pos: [f32; 3],
        yaw: f32,
        pitch: f32,
        crouch: f32,
        speed: f32,
    },
}

pub struct Other {
    pub pseudo: String,
    pub pos: Vec3,
    pub yaw: f32,
    pub pitch: f32,
    pub crouch: f32,
    pub speed: f32,
    seen: f32,
}

pub struct BuildNet {
    link: Sender<Inbound>,
    owner: bool,
    pseudo: String,
    /// Zones dont ce jeu est le garant.
    pub zones: HashSet<[i32; 2]>,
    pub others: HashMap<u64, Other>,
    chunk: Option<[i32; 2]>,
    pos_timer: f32,
    /// Modifications acceptées par joueur dans la seconde en cours.
    rate: HashMap<u64, (Instant, u32)>,
    caps_sent: bool,
}

fn chunk_of(x: i32, z: i32) -> [i32; 2] {
    let c = ChunkPos::containing(x, z);
    [c.x, c.z]
}

impl BuildNet {
    pub fn new(link: Sender<Inbound>, pseudo: String, owner: bool) -> Self {
        Self {
            link,
            owner,
            pseudo,
            zones: HashSet::new(),
            others: HashMap::new(),
            chunk: None,
            pos_timer: 0.0,
            rate: HashMap::new(),
            caps_sent: false,
        }
    }

    /// Ce jeu a ouvert le monde (il l'enregistre).
    pub fn is_owner(&self) -> bool {
        self.owner
    }

    /// Le propriétaire est parti : ce jeu le devient (il enregistrera le monde).
    pub fn set_owner(&mut self) {
        self.owner = true;
    }

    fn send(&self, req: Request) {
        transport::push(&self.link, &req);
    }

    fn garant_of(&self, chunk: [i32; 2]) -> bool {
        self.zones.contains(&proto::zone_of(chunk))
    }

    /// Capacités de ce PC pour être garant (annoncées une fois).
    pub fn caps(fps: f32) -> proto::GarantCaps {
        proto::GarantCaps {
            cores: std::thread::available_parallelism().map_or(1, |n| n.get() as u16),
            fps: fps.clamp(0.0, 1000.0) as u16,
            ram_mb: crate::session::ram_view_limit().max(0) as u32 * 256,
        }
    }

    /// À chaque image : modifications du joueur local (journal du monde), position.
    #[allow(clippy::too_many_arguments)]
    pub fn update(
        &mut self,
        world: &mut World,
        feet: Vec3,
        look: (f32, f32),
        crouch: f32,
        speed: f32,
        fps: f32,
        dt: f32,
    ) -> Option<proto::GarantCaps> {
        let mut caps = None;
        if !self.caps_sent && fps > 0.0 {
            self.caps_sent = true;
            caps = Some(Self::caps(fps));
        }
        // Ce que le joueur a changé.
        let edits = world
            .journal
            .as_mut()
            .map(std::mem::take)
            .unwrap_or_default();
        for (pos, id) in edits {
            let chunk = chunk_of(pos[0], pos[2]);
            let msg = if self.garant_of(chunk) {
                BuildMsg::Set {
                    pos,
                    block: id.0,
                    by: self.pseudo.clone(),
                }
            } else {
                BuildMsg::Edit { pos, block: id.0 }
            };
            self.send(Request::ToZone {
                chunk,
                data: game::encode(&msg),
            });
        }
        // Position : aux joueurs proches ; nouveau chunk : au relais.
        let chunk = chunk_of(feet.x.floor() as i32, feet.z.floor() as i32);
        if self.chunk != Some(chunk) {
            self.chunk = Some(chunk);
            self.send(Request::Position { chunk });
        }
        self.pos_timer -= dt;
        if self.pos_timer <= 0.0 {
            self.pos_timer = POS_EVERY;
            let msg = BuildMsg::Player {
                pseudo: self.pseudo.clone(),
                pos: feet.to_array(),
                yaw: look.0,
                pitch: look.1,
                crouch,
                speed,
            };
            self.send(Request::Nearby {
                chunk,
                data: game::encode(&msg),
            });
        }
        for o in self.others.values_mut() {
            o.seen += dt;
        }
        self.others.retain(|_, o| o.seen < STALE_S);
        caps
    }

    /// Zones gagnées ou perdues (annoncé par le relais).
    pub fn zones_changed(&mut self, gained: &[[i32; 2]], lost: &[[i32; 2]]) -> Option<String> {
        for z in lost {
            self.zones.remove(z);
        }
        for z in gained {
            self.zones.insert(*z);
        }
        log::info!("garant : {} zone(s)", self.zones.len());
        (!gained.is_empty()).then(|| {
            format!(
                "Tu es garant de {} zone(s) : tu valides les constructions qui s'y font.",
                self.zones.len()
            )
        })
    }

    /// Message d'un autre joueur ou d'un garant. Renvoie les chunks à remailler.
    pub fn receive(
        &mut self,
        peer: u64,
        chunk: [i32; 2],
        data: &[u8],
        world: &mut World,
    ) -> Vec<ChunkPos> {
        let Some(msg) = game::decode::<BuildMsg>(data) else {
            return Vec::new();
        };
        match msg {
            BuildMsg::Edit { pos, block } => {
                // Seul le garant reçoit les demandes.
                if !self.validate(peer, chunk, pos, block) {
                    return Vec::new();
                }
                let set = BuildMsg::Set {
                    pos,
                    block,
                    by: self
                        .others
                        .get(&peer)
                        .map_or(String::new(), |o| o.pseudo.clone()),
                };
                self.send(Request::ToZone {
                    chunk,
                    data: game::encode(&set),
                });
                world.set_remote(pos[0], pos[1], pos[2], BlockId(block))
            }
            BuildMsg::Set { pos, block, by } => {
                log::info!("construction : bloc {block} en {pos:?} (par {by})");
                if chunk_of(pos[0], pos[2]) != chunk
                    || block as usize >= minelib_world::registry::registry().len()
                {
                    return Vec::new();
                }
                world.set_remote(pos[0], pos[1], pos[2], BlockId(block))
            }
            BuildMsg::Player {
                pseudo,
                pos,
                yaw,
                pitch,
                crouch,
                speed,
            } => {
                self.others.insert(
                    peer,
                    Other {
                        pseudo: pseudo.chars().take(16).collect(),
                        pos: Vec3::from(pos),
                        yaw,
                        pitch,
                        crouch,
                        speed,
                        seen: 0.0,
                    },
                );
                Vec::new()
            }
        }
    }

    /// Vérification du garant : bloc connu, dans une de ses zones, dans le chunk annoncé,
    /// hauteur valide, débit raisonnable.
    fn validate(&mut self, peer: u64, chunk: [i32; 2], pos: [i32; 3], block: u16) -> bool {
        if !self.garant_of(chunk)
            || chunk_of(pos[0], pos[2]) != chunk
            || !(0..minelib_world::CHUNK_HEIGHT as i32).contains(&pos[1])
            || block as usize >= minelib_world::registry::registry().len()
        {
            return false;
        }
        let now = Instant::now();
        let e = self.rate.entry(peer).or_insert((now, 0));
        if now.duration_since(e.0).as_secs_f32() >= 1.0 {
            *e = (now, 0);
        }
        e.1 += 1;
        e.1 <= MAX_EDITS_PER_S
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_garant_refuses_edits_outside_its_zones_and_floods() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut b = BuildNet::new(tx, "G".into(), false);
        b.zones.insert([0, 0]);
        assert!(b.validate(7, [1, 1], [20, 64, 20], 1));
        assert!(!b.validate(7, [9, 9], [150, 64, 150], 1), "pas sa zone");
        assert!(
            !b.validate(7, [1, 1], [0, 64, 0], 1),
            "pas le chunk annoncé"
        );
        assert!(!b.validate(7, [1, 1], [20, 64, 20], 60000), "bloc inconnu");
        let ok = (0..30)
            .filter(|_| b.validate(8, [1, 1], [20, 64, 20], 1))
            .count();
        assert_eq!(ok, MAX_EDITS_PER_S as usize, "20 par seconde au plus");
    }
}
