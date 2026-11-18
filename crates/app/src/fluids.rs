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
//!   (qui coule), aussitôt ; la lave qui tombe sur l'eau la change en pierre.
//! - La barrière à fluides (et tout bloc solide) arrête l'écoulement. (v185) Les plantes,
//!   les torches et la redstone sont emportées (`washed_by_fluid`) : par l'eau, elles
//!   tombent en objets (`Fluids::washed`) ; la lave les brûle.
//!
//! Rythme : l'eau avance tous les 5 ticks de 50 ms, la lave tous les 30. Seuls les blocs
//! changés ici (par le joueur ou par l'écoulement lui-même) réveillent les fluides voisins :
//! une carte importée reste telle quelle tant qu'on n'y touche pas.
//!
//! (v185) Chaque pas suit `FlowingFluid.tick` de Minecraft : le bloc qui coule recalcule
//! son niveau, puis s'étale aussitôt avec ce niveau ; il ne remplit que des cases libres
//! (un voisin du même fluide se recalcule lui-même à son tour) ; une source qui peut tomber
//! tombe d'abord et ne s'étale qu'au pas suivant (sauf entourée d'au moins 3 sources).

use std::collections::{BTreeMap, HashSet};

use glam::Vec3;
use minelib_world::registry::{
    fluid, fluid_height, registry, washed_by_fluid, Fluid, FLUID_FALLING, FLUID_SOURCE,
};
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
    /// (v185) Blocs emportés par l'eau depuis le dernier relevé (position, bloc) : ils
    /// tombent en objets (voir la session). Ceux que la lave brûle n'y sont pas.
    pub washed: Vec<([i32; 3], BlockId)>,
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

/// Bloc que le fluide prend en coulant dessus : l'air, et ce qu'il emporte (plantes,
/// torches, redstone…).
fn replaceable(id: BlockId) -> bool {
    id == BlockId::AIR || washed_by_fluid(id)
}

/// Le fluide `f` peut-il passer par `id` (vide, ou même fluide qui coule) ?
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

/// Lave en `q` touchée par de l'eau (dessus ou côtés) : elle se fige.
fn wet_lava(world: &World, q: [i32; 3]) -> bool {
    matches!(fluid(at(world, q)), Some((Fluid::Lava, _)))
        && DIRS
            .iter()
            .map(|d| add(q, *d))
            .chain([add(q, [0, 1, 0])])
            .any(|n| matches!(fluid(at(world, n)), Some((Fluid::Water, _))))
}

impl Fluids {
    fn schedule(&mut self, p: [i32; 3], f: Fluid) {
        if self.queued.insert(p) {
            self.due.entry(self.tick + delay(f)).or_default().push(p);
        }
    }

    /// Une case a changé : elle et ses voisines, si ce sont des fluides, se remettent à
    /// couler. (v185) La lave que l'eau vient de toucher se fige au tick suivant, comme
    /// dans Minecraft (elle attendait son tour, jusqu'à 1,5 s).
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
                if wet_lava(world, q) {
                    self.due.entry(self.tick + 1).or_default().push(q);
                } else {
                    self.schedule(q, f);
                }
            }
        }
    }

    /// Avance l'écoulement de `dt` secondes ; renvoie les chunks à remailler. Les blocs
    /// changés sont notés dans `world.touched` (relevé par l'appelant, qui rappelle
    /// `notify`) ; ceux que l'eau emporte, dans `washed`.
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
                            let old = world.get(q[0], q[1], q[2]);
                            if washed_by_fluid(old) && matches!(fluid(id), Some((Fluid::Water, _)))
                            {
                                self.washed.push((q, old));
                            }
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

/// Niveau que prendrait le fluide `f` en `q` d'après ses voisins (`getNewLiquid` de
/// Minecraft) : source entre deux sources d'eau posées sur un sol (ou une source), chute
/// sous le même fluide, sinon le voisin le plus fort moins la perte (≤ 0 : rien).
fn new_level(get: &dyn Fn([i32; 3]) -> BlockId, q: [i32; 3], f: Fluid) -> i32 {
    let mut best = 0;
    let mut sources = 0;
    for d in DIRS {
        if let Some((g, l)) = fluid(get(add(q, d))) {
            if g == f {
                best = best.max(strength(l));
                if l == FLUID_SOURCE {
                    sources += 1;
                }
            }
        }
    }
    if f == Fluid::Water && sources >= 2 {
        let below = get(add(q, [0, -1, 0]));
        if below.def().solid || fluid(below) == Some((f, FLUID_SOURCE)) {
            return FLUID_SOURCE as i32;
        }
    }
    if matches!(fluid(get(add(q, [0, 1, 0]))), Some((g, _)) if g == f) {
        return FLUID_FALLING as i32;
    }
    best - drop(f)
}

/// Un pas de l'écoulement en `p` : les blocs à changer.
pub fn step(world: &World, p: [i32; 3]) -> Vec<([i32; 3], BlockId)> {
    let get = |q: [i32; 3]| at(world, q);
    let id = get(p);
    let Some((f, mut level)) = fluid(id) else {
        return Vec::new();
    };
    let reg = registry();
    let obsidian = reg.by_key("obsidian").unwrap_or(BlockId::STONE);
    let cobble = BlockId::COBBLESTONE;
    let down = add(p, [0, -1, 0]);

    // Lave touchée par de l'eau (dessus ou côtés) : obsidienne ou pierre taillée.
    if f == Fluid::Lava && wet_lava(world, p) {
        return vec![(
            p,
            if level == FLUID_SOURCE {
                obsidian
            } else {
                cobble
            },
        )];
    }

    let mut out = Vec::new();
    // Niveau d'un bloc qui coule, d'après ses voisins ; il s'étale aussitôt avec le nouveau.
    if level != FLUID_SOURCE {
        let new = new_level(&get, p, f);
        if new <= 0 {
            return vec![(p, BlockId::AIR)];
        }
        if new as u8 != level {
            level = new as u8;
            out.push((p, f.block(level)));
        }
    }
    // La case telle qu'elle est maintenant, pour ses voisins.
    let here = f.block(level);
    let now = |q: [i32; 3]| if q == p { here } else { get(q) };

    // D'abord vers le bas.
    let below = get(down);
    let falls = replaceable(below) || matches!(fluid(below), Some((g, _)) if g != f);
    if falls {
        out.push((
            down,
            match (f, fluid(below)) {
                (Fluid::Lava, Some((Fluid::Water, _))) => BlockId::STONE,
                (Fluid::Water, Some((Fluid::Lava, l))) => {
                    if l == FLUID_SOURCE {
                        obsidian
                    } else {
                        cobble
                    }
                }
                _ => f.block(FLUID_FALLING),
            },
        ));
        // Sur les côtés aussi seulement au milieu d'au moins trois sources (bord d'un lac
        // au-dessus d'un trou) ; sinon une source s'étale au pas suivant, quand l'eau est
        // en dessous.
        let sources = DIRS
            .iter()
            .filter(|d| fluid(get(add(p, **d))) == Some((f, FLUID_SOURCE)))
            .count();
        if sources < 3 {
            return out;
        }
    } else if level != FLUID_SOURCE && matches!(fluid(below), Some((g, _)) if g == f) {
        // Au-dessus de son propre fluide : un bloc qui coule ne s'étale pas.
        return out;
    }

    // Sur les côtés, vers le trou le plus proche (ou partout s'il n'y en a pas).
    let spread = if level == FLUID_FALLING {
        7
    } else {
        strength(level) - drop(f)
    };
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
            // La lave qui touche l'eau se fige elle-même (voir plus haut). Le même fluide
            // se recalcule lui-même à son tour (il a été prévenu).
            (_, Some(_)) => {}
            _ => {
                let lvl = new_level(&now, q, f);
                if lvl > 0 {
                    out.push((q, f.block(lvl as u8)));
                }
            }
        }
    }
    out
}

/// (v185) Sens du courant en `p` (`getFlow` de Minecraft) : vers où la surface descend,
/// d'après la hauteur des voisins (un voisin vide au-dessus d'un fluide : vers lui, la
/// pente d'une chute) ; une chute contre un mur tire vers le bas. Vecteur de longueur 1,
/// nul dans un lac.
pub fn flow(world: &World, p: [i32; 3]) -> Vec3 {
    let get = |q: [i32; 3]| at(world, q);
    let Some((f, level)) = fluid(get(p)) else {
        return Vec3::ZERO;
    };
    let own = |id: BlockId| -> Option<f32> {
        match fluid(id) {
            Some((g, l)) if g == f => Some(match l {
                FLUID_SOURCE | FLUID_FALLING => 8.0 / 9.0,
                l => l as f32 / 9.0,
            }),
            Some(_) => None,
            None => Some(0.0),
        }
    };
    let me = own(get(p)).unwrap_or(0.0);
    let mut v = Vec3::ZERO;
    for d in DIRS {
        let q = add(p, d);
        let id = get(q);
        let Some(mut h) = own(id) else {
            continue;
        };
        let mut push = 0.0;
        if h == 0.0 {
            if !id.def().solid {
                if let Some(hb) = own(get(add(q, [0, -1, 0]))) {
                    h = hb;
                    if h > 0.0 {
                        push = me - (h - 8.0 / 9.0);
                    }
                }
            }
        } else {
            push = me - h;
        }
        v += Vec3::new(d[0] as f32, 0.0, d[2] as f32) * push;
    }
    if level == FLUID_FALLING {
        let wall = DIRS.iter().any(|d| {
            let q = add(p, *d);
            get(q).def().solid || get(add(q, [0, 1, 0])).def().solid
        });
        if wall {
            v = v.normalize_or_zero() + Vec3::new(0.0, -6.0, 0.0);
        }
    }
    v.normalize_or_zero()
}

/// (v185) Hauteur de la surface du fluide de la case `p` (0..1) et son fluide, comme
/// dans Minecraft (voir `fluid_height`).
pub fn surface(world: &World, p: [i32; 3]) -> Option<(Fluid, f32)> {
    fluid_height(world.get(p[0], p[1], p[2]), world.get(p[0], p[1] + 1, p[2]))
}

/// (v185) Objet que laisse un bloc emporté par l'eau : une torche ordinaire pour une
/// torche (comme cassée à la main), le bloc lui-même pour la redstone et les fleurs ; rien
/// pour l'herbe, les fougères, les buissons morts et la toile, ni pour le haut d'une plante
/// de deux blocs (son bas laisse la plante).
pub fn washed_item(id: BlockId) -> Option<crate::inventory::Item> {
    use crate::inventory::Item;
    if id.is_torch() {
        return Some(Item::Torch(None));
    }
    if let Some(k) = id.def().key.strip_prefix("plant:") {
        let grass = [
            "tallgrass",
            "fern",
            "grass",
            "deadbush",
            "double_grass",
            "double_fern",
            "web",
        ]
        .iter()
        .any(|g| k.starts_with(g));
        if grass || k.ends_with("_upper") {
            return None;
        }
    }
    Some(Item::Block(id))
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

    /// (v185) Les durées de Minecraft (5 ticks par bloc) : 7 blocs atteints en 1,75 s, et
    /// tout est sec 1,75 s après qu'on a retiré la source (simulation de `FlowingFluid`).
    #[test]
    fn spreads_and_dries_at_minecraft_speed() {
        let mut w = world();
        let mut fl = Fluids::default();
        w.set(8, 10, 8, BlockId::WATER);
        let mut t = 0.0;
        while level(&w, 15, 10, 8).is_none() && t < 5.0 {
            run(&mut w, &mut fl, TICK);
            t += TICK;
        }
        assert!((1.65..=1.85).contains(&t), "7 blocs en {t:.2} s");
        run(&mut w, &mut fl, 3.0);
        w.set(8, 10, 8, BlockId::AIR);
        let mut t = 0.0;
        while (0..16).any(|x| level(&w, x, 10, 8).is_some()) && t < 5.0 {
            run(&mut w, &mut fl, TICK);
            t += TICK;
        }
        assert!((1.65..=1.85).contains(&t), "sec en {t:.2} s");
    }

    /// (v185) L'eau emporte torches et redstone (elles tombent en objets) ; la lave les
    /// brûle sans rien laisser.
    #[test]
    fn water_carries_torches_away_and_lava_burns_them() {
        let reg = registry();
        let wire = reg
            .defs
            .iter()
            .position(|d| d.key.starts_with("redstone_wire"))
            .map(|i| BlockId(i as u16))
            .unwrap();
        let mut w = world();
        let mut fl = Fluids::default();
        w.set(10, 10, 8, BlockId::TORCH);
        w.set(8, 10, 10, wire);
        w.set(8, 10, 8, BlockId::WATER);
        run(&mut w, &mut fl, 3.0);
        assert_eq!(level(&w, 10, 10, 8), Some((Fluid::Water, 6)));
        assert!(fluid(w.get(8, 10, 10)).is_some());
        let mut washed: Vec<BlockId> = fl.washed.iter().map(|w| w.1).collect();
        washed.sort_by_key(|b| b.0);
        let mut want = vec![BlockId::TORCH, wire];
        want.sort_by_key(|b| b.0);
        assert_eq!(washed, want);
        assert!(matches!(
            washed_item(BlockId::TORCH),
            Some(crate::inventory::Item::Torch(None))
        ));
        // Lave : brûlée, pas d'objet.
        let mut w = world();
        let mut fl = Fluids::default();
        w.set(9, 10, 8, BlockId::TORCH);
        w.set(8, 10, 8, BlockId::LAVA);
        run(&mut w, &mut fl, 5.0);
        assert!(matches!(fluid(w.get(9, 10, 8)), Some((Fluid::Lava, _))));
        assert!(fl.washed.is_empty());
    }

    /// (v185) Une source en l'air tombe d'abord, puis s'étale ; la lave touchée par l'eau
    /// se fige aussitôt (pas à son tour, 1,5 s plus tard).
    #[test]
    fn a_floating_source_falls_first_and_lava_sets_at_once() {
        let mut w = world();
        let mut fl = Fluids::default();
        w.set(8, 14, 8, BlockId::WATER);
        // (Posée : son premier pas vient 5 ticks après le tick où elle est relevée.)
        run(&mut w, &mut fl, 6.0 * TICK);
        assert_eq!(level(&w, 8, 13, 8), Some((Fluid::Water, FLUID_FALLING)));
        assert_eq!(level(&w, 9, 14, 8), None, "pas encore sur les côtés");
        run(&mut w, &mut fl, 5.0 * TICK);
        assert_eq!(level(&w, 9, 14, 8), Some((Fluid::Water, 7)));
        // Lave posée à côté d'eau immobile : figée dès le tick suivant.
        let mut w = world();
        let mut fl = Fluids::default();
        for x in 0..16 {
            for z in 0..16 {
                w.set(x, 10, z, BlockId::STONE);
            }
        }
        w.set(4, 11, 4, BlockId::STONE);
        w.set(6, 11, 4, BlockId::STONE);
        w.set(5, 11, 3, BlockId::STONE);
        w.set(5, 11, 5, BlockId::WATER);
        w.set(5, 12, 5, BlockId::STONE);
        w.set(4, 11, 5, BlockId::STONE);
        w.set(6, 11, 5, BlockId::STONE);
        w.set(5, 11, 6, BlockId::STONE);
        run(&mut w, &mut fl, 1.0);
        w.set(5, 11, 4, BlockId::LAVA);
        run(&mut w, &mut fl, 2.0 * TICK);
        let obsidian = registry().by_key("obsidian").unwrap();
        assert_eq!(w.get(5, 11, 4), obsidian);
    }

    /// (v185) Le courant va vers l'aval d'un ruisseau ; nul au milieu d'un lac.
    #[test]
    fn current_points_downstream() {
        let mut w = world();
        let mut fl = Fluids::default();
        // Rigole le long de +X.
        for x in 0..16 {
            w.set(x, 10, 7, BlockId::STONE);
            w.set(x, 10, 9, BlockId::STONE);
        }
        w.set(2, 10, 8, BlockId::WATER);
        run(&mut w, &mut fl, 5.0);
        let v = flow(&w, [5, 10, 8]);
        assert!(v.x > 0.9 && v.z.abs() < 0.1, "{v}");
        let v = flow(&w, [1, 10, 8]);
        assert!(v.x < -0.9, "{v}");
        // Lac : que des sources.
        let mut w = world();
        for x in 2..8 {
            for z in 2..8 {
                w.set(x, 10, z, BlockId::WATER);
            }
        }
        assert_eq!(flow(&w, [4, 10, 4]), Vec3::ZERO);
        // Surface : 8/9 pour une source, pleine sous le même fluide.
        assert_eq!(surface(&w, [4, 10, 4]), Some((Fluid::Water, 8.0 / 9.0)));
        w.set(4, 11, 4, BlockId::WATER);
        assert_eq!(surface(&w, [4, 10, 4]), Some((Fluid::Water, 1.0)));
    }

    /// Aperçu (à la demande) : un ruisseau qui descend deux marches, maillé comme dans le
    /// jeu et dessiné hors écran (eau en bleu, pierre en gris, ombrées selon leur pente).
    /// `FLUID_PNG=/chemin/image.png cargo test -p minelib preview_water -- --ignored`.
    #[test]
    #[ignore]
    fn preview_water_surface() {
        use glam::Mat4;
        use minelib_render::EntityVertex;
        let Ok(path) = std::env::var("FLUID_PNG") else {
            return;
        };
        let mut w = World::new(0);
        for dz in -1..=1 {
            for dx in -1..=1 {
                w.insert_empty(ChunkPos { x: dx, z: dz });
            }
        }
        // Plateau à y = 12 (x < 6), marche à 11 (x < 10), sol à 10.
        for x in -16..32 {
            for z in -16..32 {
                let top = if x < 6 {
                    12
                } else if x < 10 {
                    11
                } else {
                    10
                };
                for y in 8..=top {
                    w.set(x, y, z, BlockId::STONE);
                }
            }
        }
        w.touched = Some(Vec::new());
        let mut fl = Fluids::default();
        w.set(2, 13, 8, BlockId::WATER);
        run(&mut w, &mut fl, 12.0);
        let m = minelib_world::mesh::mesh_chunk(&w, ChunkPos { x: 0, z: 0 });
        let sun = Vec3::new(-0.8, 0.45, 0.3).normalize();
        let mut verts = Vec::new();
        let mut tri = |idx: &[u32], color: [f32; 3]| {
            for t in idx.chunks_exact(3) {
                let p: [Vec3; 3] =
                    std::array::from_fn(|k| Vec3::from(m.vertices[t[k] as usize].pos));
                let n = (p[1] - p[0]).cross(p[2] - p[0]).normalize_or_zero();
                let shade = 0.25 + 0.75 * n.dot(sun).abs();
                for q in p {
                    verts.push(EntityVertex {
                        pos: q.to_array(),
                        color: color.map(|c| c * shade),
                        uv: [0.0; 2],
                        layer: -1,
                        light: EntityVertex::light([1.0; 3], false),
                    });
                }
            }
        };
        tri(&m.indices, [0.45, 0.45, 0.45]);
        tri(&m.translucent, [0.15, 0.35, 0.85]);
        let size = 1024;
        let cam: Vec<f32> = std::env::var("FLUID_CAM")
            .unwrap_or_else(|_| "14,19,20,6,11,8".into())
            .split(',')
            .filter_map(|v| v.parse().ok())
            .collect();
        let eye = Vec3::new(cam[0], cam[1], cam[2]);
        let vp = Mat4::perspective_infinite_reverse_rh(50f32.to_radians(), 1.0, 0.05)
            * Mat4::look_at_rh(eye, Vec3::new(cam[3], cam[4], cam[5]), Vec3::Y);
        let Some([cpu, _]) = minelib_render::draw_offscreen(vp, size, &verts, &[], &[], &[]) else {
            eprintln!("pas de carte graphique");
            return;
        };
        let mut img = image::RgbImage::new(size, size);
        for (i, p) in cpu.iter().enumerate() {
            let bg = [0.6, 0.75, 0.9];
            let c = [0, 1, 2].map(|k| {
                let lin = if p[3] > 0.5 { p[k] } else { bg[k] };
                (lin.clamp(0.0, 1.0).powf(1.0 / 2.2) * 255.0) as u8
            });
            img.put_pixel(i as u32 % size, i as u32 / size, image::Rgb(c));
        }
        img.save(&path).unwrap();
    }
}
