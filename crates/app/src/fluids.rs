//! Écoulement de l'eau et de la lave, comme dans Minecraft.
//!
//! - Une source (bloc plein) ne disparaît jamais ; elle coule en descendant d'abord, et sur
//!   les côtés en perdant un niveau par bloc (eau : 7 blocs, lave : 3 blocs).
//! - Un bloc qui coule prend le niveau de son voisin le plus fort moins la perte ; plus de
//!   voisin : il sèche. Sous une colonne qui tombe, il est plein et retombe.
//! - Sur les côtés, le fluide part vers le trou le plus proche (4 blocs pour l'eau, 2 pour
//!   la lave) s'il y en a un, sinon dans toutes les directions.
//! - Deux sources d'eau voisines au-dessus d'un sol (ou d'une autre source) en créent une
//!   troisième entre elles (source infinie).
//! - Lave et eau : la lave touchée par l'eau devient obsidienne (source) ou pierre taillée
//!   (qui coule) ; la lave qui tombe sur l'eau la change en pierre.
//! - La barrière à fluides (et tout bloc solide) arrête l'écoulement.
//!
//! Rythme : l'eau avance tous les 5 ticks de 50 ms, la lave tous les 30. Seuls les blocs
//! changés ici (par le joueur ou par l'écoulement lui-même) réveillent les fluides voisins :
//! une carte importée reste telle quelle tant qu'on n'y touche pas.

use std::collections::{BTreeMap, HashSet};

use minelib_world::registry::{fluid, registry, Fluid, Shape, FLUID_FALLING, FLUID_SOURCE};
use minelib_world::{BlockId, ChunkPos, World};

const TICK: f32 = 0.05;
/// Cases traitées au plus par image (une grosse inondation se fait en plusieurs images).
const BUDGET: usize = 4096;

const DIRS: [[i32; 3]; 4] = [[1, 0, 0], [-1, 0, 0], [0, 0, 1], [0, 0, -1]];

#[derive(Default)]
pub struct Fluids {
    tick: u64,
    acc: f32,
    due: BTreeMap<u64, Vec<[i32; 3]>>,
    queued: HashSet<[i32; 3]>,
}

fn delay(f: Fluid) -> u64 {
    match f {
        Fluid::Water => 5,
        Fluid::Lava => 30,
    }
}

/// Perte de niveau par bloc à l'horizontale.
fn drop(f: Fluid) -> i32 {
    match f {
        Fluid::Water => 1,
        Fluid::Lava => 2,
    }
}

/// Distance de recherche d'un trou.
fn slope(f: Fluid) -> u32 {
    match f {
        Fluid::Water => 4,
        Fluid::Lava => 2,
    }
}

/// Bloc en `q` ; hors des chunks chargés : comme de la pierre (on ne coule pas dans
/// l'inconnu).
fn at(world: &World, q: [i32; 3]) -> BlockId {
    if world.contains(ChunkPos::containing(q[0], q[2])) {
        world.get(q[0], q[1], q[2])
    } else {
        BlockId::STONE
    }
}

fn add(p: [i32; 3], d: [i32; 3]) -> [i32; 3] {
    [p[0] + d[0], p[1] + d[1], p[2] + d[2]]
}

/// Bloc que le fluide emporte en coulant dessus (air, plantes).
fn replaceable(id: BlockId) -> bool {
    id == BlockId::AIR || matches!(id.def().shape, Shape::Cross)
}

/// Le fluide `f` peut-il entrer dans `id` (vide, ou même fluide plus faible) ?
fn open_for(id: BlockId, f: Fluid) -> bool {
    replaceable(id) || matches!(fluid(id), Some((g, l)) if g == f && l != FLUID_SOURCE)
}

/// Force d'un bloc de fluide pour ses voisins (source et chute comptent pour 8).
fn strength(level: u8) -> i32 {
    if level == FLUID_SOURCE || level == FLUID_FALLING {
        8
    } else {
        level as i32
    }
}

impl Fluids {
    fn schedule(&mut self, p: [i32; 3], f: Fluid) {
        if self.queued.insert(p) {
            self.due.entry(self.tick + delay(f)).or_default().push(p);
        }
    }

    /// Une case a changé : elle et ses voisines, si ce sont des fluides, se remettent à
    /// couler.
    pub fn notify(&mut self, world: &World, p: [i32; 3]) {
        for d in [
            [0, 0, 0],
            [1, 0, 0],
            [-1, 0, 0],
            [0, 1, 0],
            [0, -1, 0],
            [0, 0, 1],
            [0, 0, -1],
        ] {
            let q = add(p, d);
            if let Some((f, _)) = fluid(world.get(q[0], q[1], q[2])) {
                self.schedule(q, f);
            }
        }
    }

    /// Avance l'écoulement de `dt` secondes ; renvoie les chunks à remailler. Les blocs
    /// changés sont notés dans `world.touched` (relevé par l'appelant, qui rappelle
    /// `notify`).
    pub fn update(&mut self, world: &mut World, dt: f32) -> Vec<ChunkPos> {
        let mut dirty = Vec::new();
        self.acc = (self.acc + dt).min(1.0);
        let mut budget = BUDGET;
        while self.acc >= TICK {
            self.acc -= TICK;
            self.tick += 1;
            let ready: Vec<u64> = self.due.range(..=self.tick).map(|(&t, _)| t).collect();
            for t in ready {
                let Some(cells) = self.due.remove(&t) else {
                    continue;
                };
                let mut cells = cells.into_iter();
                for p in cells.by_ref() {
                    if budget == 0 {
                        break;
                    }
                    budget -= 1;
                    self.queued.remove(&p);
                    for (q, id) in step(world, p) {
                        if world.contains(ChunkPos::containing(q[0], q[2])) {
                            dirty.extend(world.set(q[0], q[1], q[2], id));
                        }
                    }
                }
                // Reste pour l'image suivante.
                let rest: Vec<_> = cells.collect();
                if !rest.is_empty() {
                    self.due.entry(self.tick + 1).or_default().extend(rest);
                }
            }
            if budget == 0 {
                self.acc = 0.0;
                break;
            }
        }
        dirty
    }
}

/// Distance (en blocs) de `p` au trou le plus proche en partant de `p`, sans revenir
/// vers `from` ; `None` au-delà de `limit`.
fn hole_distance(
    world: &World,
    f: Fluid,
    p: [i32; 3],
    from: Option<usize>,
    depth: u32,
    limit: u32,
) -> Option<u32> {
    let below = at(world, add(p, [0, -1, 0]));
    if open_for(below, f) || matches!(fluid(below), Some((g, _)) if g == f) {
        return Some(depth);
    }
    if depth >= limit {
        return None;
    }
    let mut best = None;
    for (i, d) in DIRS.iter().enumerate() {
        if from == Some(i ^ 1) {
            continue;
        }
        let q = add(p, *d);
        if !open_for(at(world, q), f) {
            continue;
        }
        if let Some(n) = hole_distance(world, f, q, Some(i), depth + 1, limit) {
            best = Some(best.map_or(n, |b: u32| b.min(n)));
        }
    }
    best
}

/// Un pas de l'écoulement en `p` : les blocs à changer.
pub fn step(world: &World, p: [i32; 3]) -> Vec<([i32; 3], BlockId)> {
    let get = |q: [i32; 3]| at(world, q);
    let id = get(p);
    let Some((f, level)) = fluid(id) else {
        return Vec::new();
    };
    let reg = registry();
    let obsidian = reg.by_key("obsidian").unwrap_or(BlockId::STONE);
    let cobble = BlockId::COBBLESTONE;
    let up = add(p, [0, 1, 0]);
    let down = add(p, [0, -1, 0]);

    // Lave touchée par de l'eau (dessus ou côtés) : obsidienne ou pierre taillée.
    if f == Fluid::Lava {
        let wet = DIRS
            .iter()
            .map(|d| add(p, *d))
            .chain([up])
            .any(|q| matches!(fluid(get(q)), Some((Fluid::Water, _))));
        if wet {
            return vec![(
                p,
                if level == FLUID_SOURCE {
                    obsidian
                } else {
                    cobble
                },
            )];
        }
    }

    // Niveau d'un bloc qui coule, d'après ses voisins.
    if level != FLUID_SOURCE {
        let new = if matches!(fluid(get(up)), Some((g, _)) if g == f) {
            FLUID_FALLING as i32
        } else {
            let mut best = 0;
            let mut sources = 0;
            for d in DIRS {
                if let Some((g, l)) = fluid(get(add(p, d))) {
                    if g == f {
                        best = best.max(strength(l) - drop(f));
                        if l == FLUID_SOURCE {
                            sources += 1;
                        }
                    }
                }
            }
            let below = get(down);
            let held = below.def().solid || fluid(below) == Some((f, FLUID_SOURCE));
            if f == Fluid::Water && sources >= 2 && held {
                FLUID_SOURCE as i32
            } else {
                best
            }
        };
        if new != level as i32 {
            let b = if new <= 0 {
                BlockId::AIR
            } else {
                f.block(new as u8)
            };
            return vec![(p, b)];
        }
    }

    let mut out = Vec::new();
    // D'abord vers le bas.
    let below = get(down);
    let falls = open_for(below, f) || matches!(fluid(below), Some((g, _)) if g != f);
    if falls {
        match (f, fluid(below)) {
            (Fluid::Lava, Some((Fluid::Water, _))) => out.push((down, BlockId::STONE)),
            (Fluid::Water, Some((Fluid::Lava, l))) => {
                out.push((down, if l == FLUID_SOURCE { obsidian } else { cobble }))
            }
            _ => {
                if below != f.block(FLUID_FALLING) {
                    out.push((down, f.block(FLUID_FALLING)));
                }
            }
        }
    }
    // Sur les côtés : une source toujours ; un bloc qui coule seulement s'il ne peut pas
    // descendre (ni dans du vide ni dans son propre fluide).
    let hole_below = falls || matches!(fluid(below), Some((g, _)) if g == f);
    if level != FLUID_SOURCE && hole_below {
        return out;
    }
    let spread = strength(level) - drop(f);
    if spread <= 0 {
        return out;
    }
    let mut dist = [None; 4];
    for (i, d) in DIRS.iter().enumerate() {
        let q = add(p, *d);
        let t = get(q);
        if open_for(t, f) || matches!(fluid(t), Some((g, _)) if g != f) {
            dist[i] = Some(hole_distance(world, f, q, Some(i), 0, slope(f)).unwrap_or(1000));
        }
    }
    let Some(min) = dist.iter().flatten().min().copied() else {
        return out;
    };
    for (i, d) in DIRS.iter().enumerate() {
        if dist[i] != Some(min) {
            continue;
        }
        let q = add(p, *d);
        let t = get(q);
        match (f, fluid(t)) {
            (Fluid::Water, Some((Fluid::Lava, l))) => {
                out.push((q, if l == FLUID_SOURCE { obsidian } else { cobble }))
            }
            // La lave qui touche l'eau se change elle-même (voir plus haut).
            (Fluid::Lava, Some((Fluid::Water, _))) => {}
            (_, Some((_, l))) if strength(l) >= spread => {}
            _ => out.push((q, f.block(spread as u8))),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn world() -> World {
        let mut w = World::new(0);
        w.insert_empty(ChunkPos { x: 0, z: 0 });
        for x in 0..16 {
            for z in 0..16 {
                w.set(x, 9, z, BlockId::STONE);
            }
        }
        w.touched = Some(Vec::new());
        w
    }

    fn run(w: &mut World, fl: &mut Fluids, seconds: f32) {
        for _ in 0..(seconds / TICK) as usize {
            fl.update(w, TICK);
            for p in w.touched.as_mut().unwrap().drain(..).collect::<Vec<_>>() {
                fl.notify(w, p);
            }
        }
    }

    fn level(w: &World, x: i32, y: i32, z: i32) -> Option<(Fluid, u8)> {
        fluid(w.get(x, y, z))
    }

    #[test]
    fn water_spreads_seven_blocks_and_dries_when_the_source_goes() {
        let mut w = world();
        let mut fl = Fluids::default();
        w.set(8, 10, 8, BlockId::WATER);
        run(&mut w, &mut fl, 10.0);
        assert_eq!(level(&w, 9, 10, 8), Some((Fluid::Water, 7)));
        assert_eq!(level(&w, 14, 10, 8), Some((Fluid::Water, 2)));
        assert_eq!(level(&w, 15, 10, 8), Some((Fluid::Water, 1)));
        assert_eq!(level(&w, 8, 10, 1), Some((Fluid::Water, 1)));
        assert_eq!(level(&w, 8, 10, 0), None, "7 blocs au plus");
        w.set(8, 10, 8, BlockId::AIR);
        run(&mut w, &mut fl, 10.0);
        for x in 0..16 {
            assert_eq!(level(&w, x, 10, 8), None, "sèche en x = {x}");
        }
    }

    #[test]
    fn water_falls_then_spreads_and_a_barrier_stops_it() {
        let mut w = world();
        let mut fl = Fluids::default();
        let barrier = registry().by_key("fluid_barrier").unwrap();
        // Un pilier : la source en haut, l'eau tombe puis s'étale au sol.
        w.set(8, 14, 8, BlockId::STONE);
        w.set(8, 15, 8, BlockId::WATER);
        w.set(7, 10, 8, barrier);
        run(&mut w, &mut fl, 15.0);
        assert_eq!(level(&w, 9, 14, 8), Some((Fluid::Water, FLUID_FALLING)));
        assert_eq!(level(&w, 9, 11, 8), Some((Fluid::Water, FLUID_FALLING)));
        assert_eq!(level(&w, 9, 10, 8), Some((Fluid::Water, FLUID_FALLING)));
        assert_eq!(level(&w, 10, 10, 8), Some((Fluid::Water, 7)));
        assert_eq!(
            w.get(7, 10, 8),
            barrier,
            "la barrière à fluides arrête l'eau"
        );
        assert!(!barrier.def().solid, "mais pas les joueurs");
    }

    #[test]
    fn two_sources_make_a_third_and_lava_meets_water() {
        let mut w = world();
        let mut fl = Fluids::default();
        // Une rigole d'un bloc de large.
        for x in 3..8 {
            w.set(x, 10, 7, BlockId::STONE);
            w.set(x, 10, 9, BlockId::STONE);
        }
        w.set(3, 10, 8, BlockId::STONE);
        w.set(7, 10, 8, BlockId::STONE);
        w.set(4, 10, 8, BlockId::WATER);
        w.set(6, 10, 8, BlockId::WATER);
        run(&mut w, &mut fl, 2.0);
        assert_eq!(w.get(5, 10, 8), BlockId::WATER, "source infinie");
        // Lave source touchée par l'eau : obsidienne ; lave qui coule : pierre taillée.
        let mut w = world();
        let mut fl = Fluids::default();
        w.set(4, 10, 4, BlockId::LAVA);
        w.set(6, 10, 4, BlockId::WATER);
        run(&mut w, &mut fl, 10.0);
        let obsidian = registry().by_key("obsidian").unwrap();
        let blocks: Vec<BlockId> = (0..16).map(|x| w.get(x, 10, 4)).collect();
        assert!(
            blocks.contains(&obsidian) || blocks.contains(&BlockId::COBBLESTONE),
            "{:?}",
            blocks
                .iter()
                .map(|b| b.def().key.clone())
                .collect::<Vec<_>>()
        );
    }
}
