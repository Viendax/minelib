//! (v249) Traces du mode FPS : le sang (coups de couteau, tirs par balle), les impacts de
//! balles, les entailles du couteau, la suie des grenades et le sol brûlé par le molotov.
//!
//! Demande de Viendax (24 novembre) : « du sang réaliste pour le FPS, pour les coups de
//! couteau et les tirs par balle ; les traces de sang sur les blocs doivent rester jusqu'à
//! la fin du round, et les effets des grenades et les impacts de balle aussi : fin du
//! round, disparition ».
//!
//! - **Dans l'air** : un coup qui touche un joueur fait gicler le sang dans le sens du
//!   coup (il ressort derrière lui), un peu en retour vers celui qui frappe, avec une bruine
//!   fine. Chaque goutte vole (pesanteur, air) jusqu'au premier bloc.
//! - **Sur les blocs** : la goutte y laisse sa tache, étirée si elle arrive en biais ; le
//!   gros du jet marque le mur ou le sol derrière le joueur touché, s'il y en a un à portée,
//!   et le sang coule le long des murs. Tout cela reste jusqu'à la fin de la manche
//!   (`clear`), comme les impacts et les marques des grenades.
//!
//! Les traces sont dessinées par la carte graphique, toutes ensemble (`minelib_render::
//! Decal`, decal.wgsl) : rien n'est recalculé d'une image à l'autre, la liste ne lui est
//! renvoyée que quand elle change (`rev`).

use glam::Vec3;
use minelib_render::{
    Decal, KIND_BURN, KIND_DRIP, KIND_HOLE, KIND_SCORCH, KIND_SLASH, KIND_SPLAT, KIND_SPRAY,
    MAX_DECALS,
};
use minelib_world::raycast::raycast_solid;
use minelib_world::World;

/// Gouttes en vol au plus.
const MAX_DROPS: usize = 700;
/// Pesanteur des gouttes (blocs/s²), comme les étincelles et les objets lancés.
const GRAVITY: f32 = 14.0;

/// (v264) Matière d'une surface touchée par une balle (« rendre les impacts réalistes, même
/// ceux sur les blocs ») : l'impact n'y a ni la même forme ni les mêmes éclats. Pierre (et
/// béton, brique : le défaut), bois, métal, verre, matière meuble (terre, sable, laine,
/// feuilles, neige).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Matter {
    Stone,
    Wood,
    Metal,
    Glass,
    Soft,
}

impl Matter {
    /// D'après la clé du bloc (`BlockDef::key` : `stone`, `slab:planks_oak:top`…).
    pub fn of_key(key: &str) -> Matter {
        let has = |words: &[&str]| words.iter().any(|w| key.contains(w));
        if has(&["glass", "pane", "ice"]) {
            Matter::Glass
        } else if has(&[
            "iron", "gold", "copper", "anvil", "chain", "rail", "bars", "cauldron", "hopper",
            "metal", "steel", "netherite", "bell",
        ]) {
            Matter::Metal
        } else if has(&["sandstone", "concrete"]) && !key.contains("powder") {
            Matter::Stone
        } else if has(&[
            "dirt", "grass", "sand", "gravel", "clay", "snow", "wool", "leaves", "moss", "mud",
            "soul", "farmland", "hay", "carpet", "sponge", "podzol", "mycelium", "path",
            "cactus", "melon", "pumpkin", "slime", "honey", "cake", "bed", "powder", "web",
        ]) {
            Matter::Soft
        } else if has(&[
            "planks", "log", "wood", "oak", "spruce", "birch", "jungle", "acacia", "mangrove",
            "cherry", "bamboo", "crimson", "warped", "door", "fence", "chest", "barrel",
            "bookshelf", "crafting", "ladder", "sign", "stem", "table",
        ]) {
            Matter::Wood
        } else {
            Matter::Stone
        }
    }

    /// Numéro de la matière pour la carte graphique (`hole` de decal.wgsl).
    fn code(self) -> f32 {
        match self {
            Matter::Stone => 0.0,
            Matter::Wood => 1.0,
            Matter::Metal => 2.0,
            Matter::Glass => 3.0,
            Matter::Soft => 4.0,
        }
    }
}

/// Une goutte de sang en vol.
#[derive(Clone, Copy, Debug)]
pub struct Drop {
    pub pos: Vec3,
    pub vel: Vec3,
    age: f32,
    life: f32,
    /// Demi-largeur (blocs).
    pub size: f32,
    /// Bruine : très fine, vite freinée, elle ne laisse pas de tache.
    mist: bool,
}

impl Drop {
    /// 1 à sa naissance … 0 quand elle s'efface (la bruine seulement).
    pub fn fade(&self) -> f32 {
        if self.mist {
            (1.0 - self.age / self.life).clamp(0.0, 1.0)
        } else {
            1.0
        }
    }
}

pub struct Traces {
    decals: Vec<Decal>,
    /// Révision de la liste : la carte ne la reçoit que quand elle change. `epoch` change
    /// quand des traces sont retirées (fin de la manche, trop de traces), `pushed` à chaque
    /// trace ajoutée : tant que `epoch` ne change pas, la carte ne reçoit que les nouvelles
    /// (`minelib_render::Decals::prepare`).
    epoch: u32,
    pushed: u32,
    /// Horloge des traces (s depuis le début de la manche).
    clock: f32,
    drops: Vec<Drop>,
    rng: u32,
    /// (v272) Prochain tri des vieilles taches de sang (horloge des traces).
    next_expire: f32,
}

impl Default for Traces {
    fn default() -> Self {
        Self {
            decals: Vec::new(),
            epoch: 0,
            pushed: 0,
            clock: 0.0,
            drops: Vec::new(),
            rng: 0x7ace_5eed,
            next_expire: 0.0,
        }
    }
}

fn v3(n: [i32; 3]) -> Vec3 {
    Vec3::new(n[0] as f32, n[1] as f32, n[2] as f32)
}

/// Un axe dans la surface de normale `n`, tourné de `angle`.
fn tangent(n: Vec3, angle: f32) -> Vec3 {
    let base = if n.y.abs() > 0.9 { Vec3::X } else { Vec3::Y };
    let a = base.cross(n).normalize_or(Vec3::X);
    let b = n.cross(a);
    a * angle.cos() + b * angle.sin()
}

impl Traces {
    /// Fin de la manche : plus rien sur les blocs ni dans l'air.
    pub fn clear(&mut self) {
        if !self.decals.is_empty() {
            self.epoch = self.epoch.wrapping_add(1);
            self.pushed = 0;
        }
        self.decals.clear();
        self.drops.clear();
        self.clock = 0.0;
        self.next_expire = 0.0;
    }

    /// Pour le rendu : la liste, sa révision, l'horloge de leurs heures de naissance.
    pub fn list(&self) -> (&[Decal], u64, f32) {
        let rev = u64::from(self.epoch) << 32 | u64::from(self.pushed);
        (&self.decals, rev, self.clock)
    }

    /// Gouttes en vol, pour le rendu.
    pub fn drops(&self) -> &[Drop] {
        &self.drops
    }

    /// Nombre de traces de cette sorte.
    #[cfg(test)]
    pub fn count(&self, kind: f32) -> usize {
        self.decals
            .iter()
            .filter(|d| d.kind.floor() == kind)
            .count()
    }

    /// −1 … 1.
    fn rnd(&mut self) -> f32 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 17;
        self.rng ^= self.rng << 5;
        (self.rng as f32 / u32::MAX as f32) * 2.0 - 1.0
    }

    /// 0 … 1.
    fn unit(&mut self) -> f32 {
        self.rnd() * 0.5 + 0.5
    }

    fn rnd3(&mut self) -> Vec3 {
        Vec3::new(self.rnd(), self.rnd(), self.rnd())
    }

    fn push(&mut self, d: Decal) {
        if !(Vec3::from(d.pos).is_finite() && d.size[0] > 0.0 && d.size[1] > 0.0) {
            return;
        }
        // Trop de traces : les plus vieilles s'en vont, par paquets.
        if self.decals.len() >= MAX_DECALS {
            self.decals.drain(..MAX_DECALS / 16);
            self.epoch = self.epoch.wrapping_add(1);
            self.pushed = 0;
        }
        self.decals.push(d);
        self.pushed += 1;
    }

    /// Impact de balle en `at`, sur une surface de normale `n`. (v264) Selon la matière du
    /// bloc touché (`Matter`), qu'il rend : éclat clair et fissures dans la pierre, bois
    /// arraché le long du fil, trou net au bord brillant dans le métal, étoile de fissures
    /// dans le verre, simple creux dans la terre.
    pub fn hole(&mut self, world: &World, at: Vec3, n: Vec3) -> Matter {
        let cell = (at - n * 0.02).floor();
        let id = world.get(cell.x as i32, cell.y as i32, cell.z as i32);
        let key = &minelib_world::registry::registry().get(id).key;
        let matter = Matter::of_key(key);
        let (seed, angle) = (self.unit() * 900.0, self.rnd() * std::f32::consts::PI);
        let vary = 1.0 + 0.15 * self.rnd();
        let (size, tan) = match matter {
            Matter::Stone => ([0.085 * vary; 2], tangent(n, angle)),
            Matter::Wood => {
                // L'axe u de la trace suit le fil du bois : debout pour un tronc, une porte,
                // une barrière ; couché pour les planches.
                let upright = ["log", "stem", "door", "fence", "ladder", "sign"]
                    .iter()
                    .any(|w| key.contains(w));
                let wall = n.y.abs() < 0.9;
                let grain = if wall && upright {
                    Vec3::Y
                } else if wall {
                    Vec3::Y.cross(n).normalize_or(Vec3::X)
                } else {
                    Vec3::X
                };
                ([0.095 * vary, 0.045 * vary], grain)
            }
            Matter::Metal => ([0.05 * vary; 2], tangent(n, angle)),
            Matter::Glass => ([0.17 * vary; 2], tangent(n, angle)),
            Matter::Soft => ([0.06 * vary; 2], tangent(n, angle)),
        };
        self.push(Decal {
            pos: at.to_array(),
            // La matière : en seizièmes, après la sorte (comme les côtés du sol brûlé).
            kind: KIND_HOLE + matter.code() / 16.0,
            normal: n.to_array(),
            seed,
            tangent: tan.to_array(),
            born: self.clock,
            size,
            depth: 0.05,
            force: 1.0,
        });
        matter
    }

    /// Entaille du couteau en `at`.
    pub fn slash(&mut self, at: Vec3, n: Vec3) {
        let (seed, angle) = (self.unit() * 900.0, self.rnd() * std::f32::consts::PI);
        let long = 0.17 * (1.0 + 0.2 * self.rnd());
        self.push(Decal {
            pos: at.to_array(),
            kind: KIND_SLASH,
            normal: n.to_array(),
            seed,
            tangent: tangent(n, angle).to_array(),
            born: self.clock,
            size: [long, 0.035],
            depth: 0.04,
            force: 1.0,
        });
    }

    /// Suie d'une explosion en `at`, de rayon `radius` : sur le sol, le plafond et les murs
    /// à portée, d'autant plus large et noire qu'ils sont près.
    pub fn scorch(&mut self, world: &World, at: Vec3, radius: f32, force: f32) {
        let from = at + Vec3::Y * 0.06;
        for dir in [
            Vec3::NEG_Y,
            Vec3::Y,
            Vec3::X,
            Vec3::NEG_X,
            Vec3::Z,
            Vec3::NEG_Z,
        ] {
            let Some(h) = raycast_solid(world, from.into(), dir.into(), radius * 0.8) else {
                continue;
            };
            let k = 1.0 - h.distance / radius;
            let n = v3(h.normal);
            let half = radius * (0.45 + 0.55 * k);
            let (seed, angle) = (self.unit() * 900.0, self.rnd() * std::f32::consts::PI);
            self.push(Decal {
                pos: (from + dir * h.distance).to_array(),
                kind: KIND_SCORCH,
                normal: n.to_array(),
                seed,
                tangent: tangent(n, angle).to_array(),
                born: self.clock,
                size: [half, half],
                depth: (half * 0.25).min(0.3),
                force: force * (0.35 + 0.65 * k),
            });
        }
    }

    /// Sol brûlé par un molotov : `cells`, le dessus des blocs en feu (milieu de la case).
    /// Il noircit pendant que le feu brûle (decal.wgsl, `burn`).
    pub fn burn(&mut self, cells: &[Vec3]) {
        let key = |c: &Vec3| (c.x.floor() as i32, c.y.round() as i32, c.z.floor() as i32);
        let burning: std::collections::HashSet<(i32, i32, i32)> = cells.iter().map(key).collect();
        for c in cells {
            let (x, y, z) = key(c);
            // Côtés où la case voisine brûle aussi, à la même hauteur (bits de `Flame`).
            let sides = [(1, 0), (-1, 0), (0, 1), (0, -1)]
                .iter()
                .enumerate()
                .filter(|(_, (dx, dz))| burning.contains(&(x + dx, y, z + dz)))
                .fold(0u32, |m, (k, _)| m | 1 << k);
            self.push(Decal {
                pos: c.to_array(),
                kind: KIND_BURN + sides as f32 / 16.0,
                normal: [0.0, 1.0, 0.0],
                seed: 0.0,
                tangent: [1.0, 0.0, 0.0],
                born: self.clock,
                size: [0.5, 0.5],
                depth: 0.12,
                force: 1.0,
            });
        }
    }

    /// Tache de sang posée en `at` sur une surface de normale `n` par du sang qui arrivait
    /// à la vitesse `vel` : ronde s'il arrive de face, étirée dans son sens s'il arrive en
    /// biais. `half` : demi-largeur. Sur un mur, une grosse tache coule parfois.
    fn stain(&mut self, at: Vec3, n: Vec3, vel: Vec3, half: f32) {
        let slide = vel - n * vel.dot(n);
        let k = slide.length() / vel.length().max(1e-4);
        let (seed, angle) = (self.unit() * 900.0, self.rnd() * std::f32::consts::PI);
        let u = if k > 0.2 {
            slide.normalize_or(Vec3::X)
        } else {
            tangent(n, angle)
        };
        self.push(Decal {
            pos: at.to_array(),
            kind: KIND_SPLAT,
            normal: n.to_array(),
            seed,
            tangent: u.to_array(),
            born: self.clock,
            size: [half * (1.0 + 1.3 * k), half],
            depth: 0.04,
            force: 1.0,
        });
        if n.y.abs() < 0.5 && half > 0.07 && self.unit() < 0.35 {
            self.drip(at, n, half * 0.3);
        }
    }

    /// Coulure le long du mur de normale `n`, qui part d'autour de `at`.
    fn drip(&mut self, at: Vec3, n: Vec3, spread: f32) {
        let side = Vec3::Y.cross(n).normalize_or(Vec3::X);
        let top = at + side * (self.rnd() * spread) + Vec3::Y * (self.rnd() * spread * 0.5);
        let len = 0.10 + 0.38 * self.unit().powi(2);
        let (seed, wait, wide) = (self.unit() * 900.0, self.unit(), self.unit());
        self.push(Decal {
            pos: (top - Vec3::Y * (len * 0.5)).to_array(),
            kind: KIND_DRIP,
            normal: n.to_array(),
            seed,
            // (v = normale × u : le haut.)
            tangent: side.to_array(),
            // Elle se met à couler un peu après.
            born: self.clock + 0.3 + 1.5 * wait,
            size: [0.011 + 0.008 * wide, len * 0.5],
            depth: 0.05,
            force: 1.0,
        });
    }

    /// Un coup parti de `from` touche un joueur en `at` : le sang gicle dans le sens du coup
    /// (il ressort derrière lui), un peu en retour, avec une bruine ; le gros du jet marque
    /// le mur ou le sol derrière lui s'il y en a un à portée. `knife` : un coup de couteau
    /// (moins de force, plus large).
    pub fn blood(&mut self, world: &World, at: Vec3, from: Vec3, knife: bool) {
        if !(at.is_finite() && from.is_finite()) {
            return;
        }
        let dir = (at - from).normalize_or(Vec3::NEG_Z);
        if self.drops.len() > MAX_DROPS {
            let extra = self.drops.len() - MAX_DROPS;
            self.drops.drain(..extra);
        }
        let (drops, mist) = if knife { (12, 10) } else { (16, 18) };
        for k in 0..drops {
            let back = k % 4 == 3;
            let axis = if back { -dir } else { dir };
            let wide = if knife {
                0.95
            } else if back {
                0.7
            } else {
                0.42
            };
            let u = self.unit();
            let speed = if back {
                1.2 + 1.8 * u
            } else if knife {
                1.5 + 2.5 * u
            } else {
                2.5 + 5.5 * u
            };
            let vel = (axis + self.rnd3() * wide).normalize_or(axis) * speed
                + Vec3::Y * (0.6 + 1.2 * self.unit());
            let size = 0.012 + 0.022 * self.unit().powi(2);
            self.drops.push(Drop {
                pos: at,
                vel,
                age: 0.0,
                life: 3.0,
                size,
                mist: false,
            });
        }
        for _ in 0..mist {
            let vel = (dir + self.rnd3() * 0.6).normalize_or(dir) * (1.5 + 4.0 * self.unit());
            let (life, size) = (0.18 + 0.14 * self.unit(), 0.006 + 0.006 * self.unit());
            self.drops.push(Drop {
                pos: at,
                vel,
                age: 0.0,
                life,
                size,
                mist: true,
            });
        }
        // Ce que le sang marque derrière le joueur touché. Comme dans CS : des traits tirés
        // du point touché le long du coup, un peu écartés, chacun jusqu'au premier bloc à
        // portée ; là où il arrive, sa trace, posée sur la surface et étalée dans le sens
        // où le sang allait. Un mur loin derrière reçoit donc des taches plus écartées
        // qu'un mur tout proche. Le premier trait, dans l'axe, porte le gros du jet.
        let (rays, reach, base) = if knife { (2, 1.8, 0.22) } else { (4, 4.5, 0.42) };
        for k in 0..rays {
            let aim = if k == 0 {
                dir
            } else {
                (dir + self.rnd3() * 0.17).normalize_or(dir)
            };
            let Some(h) = raycast_solid(world, at.into(), aim.into(), reach) else {
                continue;
            };
            let n = v3(h.normal);
            let p = at + aim * h.distance;
            let near = 1.0 - h.distance / (reach * 1.3);
            let share = if k == 0 { 1.0 } else { 0.40 + 0.30 * self.unit() };
            let half = base * share * (0.6 + 0.4 * self.unit()) * (0.55 + 0.45 * near);
            let slide = aim - n * aim.dot(n);
            let seed = self.unit() * 900.0;
            if slide.length() < 0.38 {
                // De face : une tache ronde.
                let angle = self.rnd() * std::f32::consts::PI;
                self.push(Decal {
                    pos: p.to_array(),
                    kind: KIND_SPLAT,
                    normal: n.to_array(),
                    seed,
                    tangent: tangent(n, angle).to_array(),
                    born: self.clock,
                    size: [half * 0.9, half * 0.9],
                    depth: 0.08,
                    force: 1.0,
                });
            } else {
                // En biais : une giclée étalée dans le sens du coup, le gros de la tache là
                // où il arrive (à 0,4 du milieu de la trace, vers l'arrière : decal.wgsl,
                // `spray`).
                let u = slide.normalize();
                let long = half * (1.0 + 0.6 * slide.length());
                self.push(Decal {
                    pos: (p + u * (0.4 * long)).to_array(),
                    kind: KIND_SPRAY,
                    normal: n.to_array(),
                    seed,
                    tangent: u.to_array(),
                    born: self.clock,
                    size: [long, half * 0.78],
                    depth: 0.08,
                    force: 1.0,
                });
            }
            // Sur un mur, le gros du jet coule.
            if n.y.abs() < 0.5 && (k == 0 || half > 0.16) {
                let count = if k == 0 { 1 + (self.unit() * 2.6) as usize } else { 1 };
                for _ in 0..count {
                    self.drip(p, n, half * 0.45);
                }
            }
        }
    }

    /// (v272) Du sang goutte d'une blessure en `at` (qui bouge à `vel`) : `count` gouttes qui
    /// tombent presque droit et tachent le sol où elles arrivent ; `spurt` : un jet (la
    /// morsure vient d'arracher le morceau), elles partent plus fort, tout autour.
    pub fn bleed(&mut self, at: Vec3, vel: Vec3, count: usize, spurt: bool) {
        if !(at.is_finite() && vel.is_finite()) {
            return;
        }
        if self.drops.len() + count > MAX_DROPS {
            let extra = (self.drops.len() + count - MAX_DROPS).min(self.drops.len());
            self.drops.drain(..extra);
        }
        for _ in 0..count {
            let out = self.rnd3();
            let push = if spurt { 0.4 + 1.0 * self.unit() } else { 0.15 + 0.3 * self.unit() };
            let up = if spurt { 0.4 + 1.0 * self.unit() } else { 0.0 };
            let size = 0.010 + 0.016 * self.unit().powi(2);
            self.drops.push(Drop {
                pos: at + out * 0.03,
                vel: vel * 0.5 + Vec3::new(out.x, 0.0, out.z) * push + Vec3::Y * up,
                age: 0.0,
                life: 4.0,
                size,
                mist: false,
            });
        }
    }

    /// (v272) « En build le sang reste 5 minutes au sol » : hors partie FPS, les taches de
    /// sang (taches, giclées, coulures) plus vieilles que `max_age` secondes s'en vont ; les
    /// autres traces restent. (En partie FPS, tout reste jusqu'à la fin de la manche.)
    pub fn expire_blood(&mut self, max_age: f32) {
        // Une fois par seconde suffit.
        if self.clock < self.next_expire {
            return;
        }
        self.next_expire = self.clock + 1.0;
        let clock = self.clock;
        let before = self.decals.len();
        self.decals.retain(|d| {
            let blood = d.kind == KIND_SPLAT || d.kind == KIND_SPRAY || d.kind == KIND_DRIP;
            !(blood && clock - d.born > max_age)
        });
        if self.decals.len() != before {
            self.epoch = self.epoch.wrapping_add(1);
            self.pushed = 0;
        }
    }

    /// Avance de `dt` : les gouttes volent ; celle qui touche un bloc y laisse sa tache.
    pub fn tick(&mut self, world: &World, dt: f32) {
        self.clock += dt;
        if self.drops.is_empty() {
            return;
        }
        let mut landed = Vec::new();
        for d in &mut self.drops {
            d.age += dt;
            if d.mist {
                d.vel *= (-6.0 * dt).exp();
                d.vel.y -= 3.0 * dt;
                d.pos += d.vel * dt;
                continue;
            }
            d.vel.y -= GRAVITY * dt;
            d.vel *= (-0.9 * dt).exp();
            let step = d.vel * dt;
            let len = step.length();
            if len > 1e-5 {
                let dir = step / len;
                if let Some(h) = raycast_solid(world, d.pos.into(), dir.into(), len + 0.01) {
                    landed.push((d.pos + dir * h.distance, v3(h.normal), d.vel, d.size));
                    d.age = f32::MAX;
                    continue;
                }
            }
            d.pos += step;
        }
        self.drops.retain(|d| d.age < d.life);
        for (at, n, vel, size) in landed {
            let half = size * (2.8 + 1.8 * self.unit());
            self.stain(at, n, vel, half);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use minelib_world::ChunkPos;

    /// Un sol de pierre à y = 10 (dessus à 11) et un mur en x = 12, de 11 à 15.
    fn room() -> World {
        let reg = minelib_world::registry::registry();
        let stone = reg.by_key("stone").unwrap();
        let mut w = World::new(0);
        w.insert_empty(ChunkPos { x: 0, z: 0 });
        for x in 0..16 {
            for z in 0..16 {
                w.set(x, 10, z, stone);
            }
        }
        for y in 11..16 {
            for z in 0..16 {
                w.set(12, y, z, stone);
            }
        }
        w
    }

    /// (v272) Une morsure saigne : les gouttes tombent et tachent le sol sous elle ; hors
    /// partie FPS ces taches s'en vont au bout de cinq minutes, pas les impacts de balles.
    #[test]
    fn a_bite_bleeds_on_the_ground_and_the_blood_goes_after_five_minutes() {
        let w = room();
        let mut t = Traces::default();
        t.hole(&w, Vec3::new(5.5, 11.0, 5.5), Vec3::Y);
        t.bleed(Vec3::new(6.0, 12.2, 8.0), Vec3::ZERO, 14, true);
        t.bleed(Vec3::new(6.0, 12.2, 8.0), Vec3::ZERO, 6, false);
        assert_eq!(t.drops().len(), 20);
        for _ in 0..300 {
            t.tick(&w, 1.0 / 60.0);
        }
        assert!(t.drops().is_empty());
        let (list, ..) = t.list();
        let blood: Vec<_> = list.iter().filter(|d| d.kind != KIND_HOLE).collect();
        assert!(blood.len() >= 18, "{} taches", blood.len());
        for d in &blood {
            // Au sol (dessus du bloc), à moins de deux blocs de la morsure.
            assert!((d.pos[1] - 11.0).abs() < 1e-3 && d.normal == [0.0, 1.0, 0.0]);
            assert!(Vec3::new(d.pos[0] - 6.0, 0.0, d.pos[2] - 8.0).length() < 2.0);
        }
        // Quatre minutes et demie plus tard elles sont encore là ; à cinq minutes passées,
        // il ne reste que l'impact.
        let total = list.len();
        for _ in 0..265 {
            t.tick(&w, 1.0);
            t.expire_blood(300.0);
        }
        assert_eq!(t.list().0.len(), total);
        let rev = t.list().1;
        for _ in 0..40 {
            t.tick(&w, 1.0);
            t.expire_blood(300.0);
        }
        let (list, after, _) = t.list();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].kind, KIND_HOLE);
        assert_ne!(rev, after, "la carte doit recevoir la liste raccourcie");
    }

    /// Un tir qui touche un joueur devant un mur : le sang gicle, marque le mur derrière
    /// lui dans le sens du tir, y coule, et les gouttes retombées tachent le sol et le mur.
    /// Tout reste jusqu'à la fin de la manche.
    #[test]
    fn blood_sprays_stains_the_wall_behind_and_stays_until_the_round_ends() {
        let w = room();
        let mut t = Traces::default();
        // Tiré d'en haut, en biais vers le mur : touché en x = 10,8, le mur 1,9 bloc derrière.
        let (from, at) = (Vec3::new(9.0, 15.2, 8.0), Vec3::new(10.8, 13.0, 8.0));
        t.blood(&w, at, from, false);
        assert!(t.drops().len() >= 30, "{}", t.drops().len());
        let (list, rev, _) = t.list();
        let wall: Vec<_> = list.iter().filter(|d| d.normal == [-1.0, 0.0, 0.0]).collect();
        // La giclée sur le mur (étalée vers le bas : le tir descendait), et ses coulures.
        let spray = wall.iter().find(|d| d.kind == KIND_SPRAY).expect("giclée");
        assert!((spray.pos[0] - 12.0).abs() < 1e-3 && spray.tangent[1] < -0.9);
        assert!(spray.size[0] > spray.size[1] && spray.size[0] < 0.7);
        // (Les autres traits, écartés du premier, laissent des taches plus petites.)
        let stains = list.iter().filter(|d| d.kind == KIND_SPRAY || d.kind == KIND_SPLAT);
        assert!(stains.clone().count() >= 2 && stains.clone().count() <= 4);
        assert!(stains.clone().all(|d| d.size[0] <= spray.size[0] + 1e-6));
        let drips: Vec<_> = wall.iter().filter(|d| d.kind == KIND_DRIP).collect();
        assert!(!drips.is_empty() && drips.len() <= 6);
        for d in &drips {
            // Mince, haute, son axe u à l'horizontale (v : le haut), et elle coule plus tard.
            assert!(d.size[1] > 2.0 * d.size[0] && d.tangent[1].abs() < 1e-5 && d.born > 0.0);
        }
        assert!(rev > 0);
        // Les gouttes retombent : des taches au sol et sur le mur, plus rien en vol.
        let before = list.len();
        for _ in 0..600 {
            t.tick(&w, 1.0 / 120.0);
        }
        assert!(t.drops().is_empty());
        let (list, rev2, clock) = t.list();
        assert!(list.len() >= before + 8, "{} taches", list.len() - before);
        assert!(rev2 > rev && (clock - 5.0).abs() < 0.01);
        let floor = list
            .iter()
            .filter(|d| d.kind == KIND_SPLAT && d.normal == [0.0, 1.0, 0.0])
            .count();
        assert!(floor >= 3, "{floor} taches au sol");
        for d in list {
            let on_floor = d.normal == [0.0, 1.0, 0.0] && (d.pos[1] - 11.0).abs() < 1e-3;
            let on_wall = d.normal == [-1.0, 0.0, 0.0] && (d.pos[0] - 12.0).abs() < 0.01;
            assert!(on_floor || on_wall, "{d:?}");
            assert!(d.size[0] < 0.75 && d.size[1] < 0.5);
        }
        // Rien ne s'efface tout seul…
        let kept = list.len();
        for _ in 0..2000 {
            t.tick(&w, 0.1);
        }
        assert_eq!(t.list().0.len(), kept);
        assert!(t.count(KIND_SPLAT) >= floor);
        // … jusqu'à la fin de la manche.
        let rev3 = t.list().1;
        t.clear();
        let (list, rev4, clock) = t.list();
        assert!(list.is_empty() && rev4 > rev3 && clock == 0.0);
    }

    /// Le couteau fait moins gicler, plus près ; un coup de face laisse une tache ronde.
    #[test]
    fn a_knife_sprays_less_and_a_straight_hit_leaves_a_round_stain() {
        let w = room();
        let mut gun = Traces::default();
        let mut knife = Traces::default();
        // De face contre le mur, à un bloc.
        let (from, at) = (Vec3::new(9.0, 12.5, 8.0), Vec3::new(11.0, 12.5, 8.0));
        gun.blood(&w, at, from, false);
        knife.blood(&w, at, from, true);
        assert!(knife.drops().len() < gun.drops().len());
        // (Vitesse moyenne des gouttes, sans la bruine.)
        let speed = |t: &Traces| {
            let v: Vec<f32> = t
                .drops()
                .iter()
                .filter(|d| !d.mist)
                .map(|d| d.vel.length())
                .collect();
            v.iter().sum::<f32>() / v.len() as f32
        };
        assert!(speed(&knife) < speed(&gun));
        // (La première trace : celle du trait dans l'axe.)
        let round = |t: &Traces| {
            t.list().0.iter().find(|d| d.kind == KIND_SPLAT).map(|d| d.size)
        };
        let (g, k) = (round(&gun).expect("tache"), round(&knife).expect("tache"));
        assert!(g[0] == g[1] && k[0] == k[1] && k[0] < g[0]);
        // Trop loin de tout : seulement des gouttes.
        let mut far = Traces::default();
        far.blood(&w, Vec3::new(4.0, 14.0, 8.0), Vec3::new(2.0, 14.0, 8.0), true);
        assert!(far.list().0.is_empty() && !far.drops().is_empty());
    }

    /// (v264) La matière d'un bloc d'après sa clé : l'impact d'une balle en dépend.
    #[test]
    fn the_impact_depends_on_what_the_block_is_made_of() {
        for (key, m) in [
            ("stone", Matter::Stone),
            ("bricks", Matter::Stone),
            ("sandstone", Matter::Stone),
            ("concrete_white", Matter::Stone),
            ("concrete_powder_white", Matter::Soft),
            ("slab:planks_oak:top", Matter::Wood),
            ("log_oak", Matter::Wood),
            ("leaves_oak", Matter::Soft),
            ("iron_block", Matter::Metal),
            ("door_iron", Matter::Metal),
            ("door_oak", Matter::Wood),
            ("glass", Matter::Glass),
            ("glass_pane", Matter::Glass),
            ("sand", Matter::Soft),
            ("wool_red", Matter::Soft),
            ("grass", Matter::Soft),
        ] {
            assert_eq!(Matter::of_key(key), m, "{key}");
        }
        // La matière part avec la trace (en seizièmes après la sorte), et la trace d'un
        // impact dans le bois est allongée.
        let w = room();
        let mut t = Traces::default();
        let m = t.hole(&w, Vec3::new(5.0, 11.0, 5.0), Vec3::Y);
        let d = t.list().0[0];
        assert_eq!(d.kind.floor(), KIND_HOLE);
        assert!(((d.kind - KIND_HOLE) * 16.0 - m.code()).abs() < 1e-3);
    }

    /// Impacts, entailles, suie et sol brûlé : posés sur leur surface, gardés, et au plus
    /// `MAX_DECALS` à la fois (les plus vieux s'en vont).
    #[test]
    fn impacts_and_grenade_marks_are_kept_up_to_the_cap() {
        let w = room();
        let mut t = Traces::default();
        t.hole(&w, Vec3::new(5.0, 11.0, 5.0), Vec3::Y);
        t.slash(Vec3::new(12.0, 13.0, 5.0), Vec3::NEG_X);
        assert_eq!((t.count(KIND_HOLE), t.count(KIND_SLASH)), (1, 1));
        for d in t.list().0 {
            let (n, u) = (Vec3::from(d.normal), Vec3::from(d.tangent));
            assert!(n.dot(u).abs() < 1e-5 && (u.length() - 1.0).abs() < 1e-5);
        }
        // Une grenade posée au sol, à 1,5 bloc du mur : suie au sol (large) et sur le mur.
        t.scorch(&w, Vec3::new(10.5, 11.1, 8.0), 2.4, 1.0);
        let soot: Vec<_> = t.list().0.iter().filter(|d| d.kind == KIND_SCORCH).collect();
        assert_eq!(soot.len(), 2);
        let (floor, wall) = (soot[0], soot[1]);
        assert_eq!((floor.normal, wall.normal), ([0.0, 1.0, 0.0], [-1.0, 0.0, 0.0]));
        assert!(floor.size[0] > wall.size[0] && floor.force > wall.force);
        assert!(floor.size[0] > 2.0 && floor.size[0] <= 2.4);
        // Molotov : une trace par case, avec ses voisines en feu.
        let cells = [Vec3::new(5.5, 11.0, 5.5), Vec3::new(6.5, 11.0, 5.5)];
        t.burn(&cells);
        let burnt: Vec<_> = t.list().0.iter().filter(|d| d.kind.floor() == KIND_BURN).collect();
        assert_eq!(burnt.len(), 2);
        assert_eq!(burnt[0].kind, KIND_BURN + 1.0 / 16.0, "voisine en +x");
        assert_eq!(burnt[1].kind, KIND_BURN + 2.0 / 16.0, "voisine en −x");
        assert_eq!(t.count(KIND_BURN), 2);
        // Le plafond.
        for k in 0..MAX_DECALS * 2 {
            t.hole(&w, Vec3::new(5.0 + (k % 7) as f32 * 0.1, 11.0, 5.0), Vec3::Y);
        }
        assert!(t.list().0.len() <= MAX_DECALS);
        assert!(t.list().0.len() > MAX_DECALS * 3 / 4);
        assert_eq!(t.count(KIND_SCORCH), 0, "les plus vieilles sont parties");
    }
}
