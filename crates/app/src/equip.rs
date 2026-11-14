//! Équipement du mode FPS en plus des armes à feu : couteau et grenades, comme dans
//! Counter-Strike.
//!
//! - **Couteau** (touche 3) : clic gauche, coup rapide ; clic droit, coup lourd et lent.
//!   Il marque le bloc touché.
//! - **Grenades** (touche 4, ou la touche de chacune) : clic gauche, lancer fort ; clic
//!   droit, lancer en cloche. Limites de CS : 4 grenades au plus, dont 2 flashs, une
//!   explosive, une fumigène, un leurre et un molotov.
//!   - explosive : explose 1,6 s après le lancer, blesse dans un rayon de 9 blocs ;
//!   - flash : éclate 1,6 s après le lancer, aveugle si on la voit (moins de dos) ;
//!   - fumigène : se déclenche une fois posée, nuage de 3,5 blocs pendant 18 s ;
//!   - leurre : une fois posé, imite des tirs de pistolet pendant 15 s ;
//!   - molotov : s'embrase en touchant le sol (ou après 2 s), feu au sol pendant 7 s.
//!
//! Les modèles des grenades sont dessinés ici (formes simples, dans le style des armes) ;
//! le couteau reprend la baïonnette du pack d'armes Quaternius (CC0).

use glam::Vec3;
use minelib_world::raycast::raycast;
use minelib_world::World;

use crate::audio::Sfx;
use crate::weapons::Tri;

/// Gravité des projectiles (celle de CS, 800 u/s², en blocs).
const GRAVITY: f32 = 800.0 * crate::player::cs::UNIT;
/// Vitesse au lancer fort et au lancer en cloche (blocs/s).
const THROW_FAST: f32 = 24.0;
const THROW_LOB: f32 = 7.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Nade {
    He,
    Flash,
    Smoke,
    Decoy,
    Molotov,
}

impl Nade {
    pub const ALL: [Nade; 5] = [
        Nade::He,
        Nade::Flash,
        Nade::Smoke,
        Nade::Decoy,
        Nade::Molotov,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Nade::He => "Grenade HE",
            Nade::Flash => "Flash",
            Nade::Smoke => "Fumigène",
            Nade::Decoy => "Leurre",
            Nade::Molotov => "Molotov",
        }
    }

    /// Prix de CS (affiché ; gratuit en solo).
    pub fn price(self) -> u32 {
        match self {
            Nade::He => 300,
            Nade::Flash => 200,
            Nade::Smoke => 300,
            Nade::Decoy => 50,
            Nade::Molotov => 400,
        }
    }

    /// Nombre maximal de cette grenade (CS : 2 flashs, une de chaque autre).
    pub fn max(self) -> u8 {
        match self {
            Nade::Flash => 2,
            _ => 1,
        }
    }

    pub fn index(self) -> usize {
        Nade::ALL.iter().position(|n| *n == self).unwrap_or(0)
    }

    /// Couleur repère dans le HUD (liste d'armes) pour distinguer les grenades.
    pub fn hud_color(self) -> [u8; 3] {
        match self {
            Nade::He => [225, 70, 55],
            Nade::Flash => [245, 245, 235],
            Nade::Smoke => [150, 160, 170],
            Nade::Decoy => [235, 200, 70],
            Nade::Molotov => [245, 135, 40],
        }
    }

    /// Couleurs du corps et de la bande / du bouchon.
    fn colors(self) -> ([f32; 3], [f32; 3]) {
        match self {
            Nade::He => ([0.10, 0.16, 0.05], [0.25, 0.25, 0.22]),
            Nade::Flash => ([0.32, 0.33, 0.35], [0.55, 0.55, 0.50]),
            Nade::Smoke => ([0.20, 0.22, 0.20], [0.60, 0.60, 0.62]),
            Nade::Decoy => ([0.35, 0.30, 0.18], [0.20, 0.20, 0.20]),
            Nade::Molotov => ([0.30, 0.16, 0.05], [0.75, 0.70, 0.60]),
        }
    }
}

/// Part de l'aveuglement selon l'angle (degrés) entre le regard et la flash : plein au
/// centre de l'écran, nettement moins sur les bords (vers 50°), presque rien de dos.
pub fn flash_angle_factor(angle: f32) -> f32 {
    const CURVE: [(f32, f32); 5] = [
        (0.0, 1.0),
        (20.0, 1.0),
        (53.0, 0.4),
        (90.0, 0.15),
        (180.0, 0.08),
    ];
    for w in CURVE.windows(2) {
        let ((a0, f0), (a1, f1)) = (w[0], w[1]);
        if angle <= a1 {
            let t = ((angle - a0) / (a1 - a0)).clamp(0.0, 1.0);
            return f0 + (f1 - f0) * t;
        }
    }
    0.08
}

/// Durée d'une fumigène (s), comme dans CS.
pub const SMOKE_LIFE: f32 = 18.0;
/// Demi-largeur et hauteur du nuage (blocs).
const SMOKE_RX: f32 = 4.65;
const SMOKE_H: f32 = 3.4;
/// Tunnel creusé par une balle : rayon (petit trou), ouvert 0,5 s, refermé à 0,75 s.
const TUNNEL_R: f32 = 0.1;
const TUNNEL_OPEN: f32 = 0.5;
const TUNNEL_SHUT: f32 = 0.75;
/// Au plus autant de tunnels par nuage (envoyés à la carte graphique).
pub const MAX_TUNNELS: usize = 16;

/// Trait de balle à travers la fumée (segment) et son âge.
#[derive(Clone, Debug)]
pub struct Tunnel {
    pub a: Vec3,
    pub b: Vec3,
    pub age: f32,
}

/// Moment d'arrivée de la fumée rangé ×0,8 dans la grille (0..204) : une case jamais
/// atteinte (255) se lit 1,25, bien au-delà du front, même lissée par la carte graphique.
pub const SMOKE_REACH_SCALE: f32 = 0.8;

/// Taille d'une case de la grille de fumée (blocs) : 4 cases par bloc.
pub const SMOKE_CELL: f32 = 0.25;

/// Nuage de fumigène simulé sur une grille fine (4 cases par bloc) : la fumée part de la
/// grenade et remplit un volume fixe (celui d'un dôme de 9,3 × 3,4 blocs en terrain
/// dégagé) en contournant les murs ; gênée, elle s'étale dans les couloirs comme dans CS2.
/// Chaque case garde le moment où la fumée l'atteint (`reach`, 0 au centre … 1 au bord) :
/// la carte graphique en tire la densité (gonflement, volutes, dissipation) et la dessine
/// en volume, opaque, vue de dehors comme de dedans. Les balles y percent de petits
/// tunnels qui se referment entre 1 et 1,5 s.
#[derive(Clone, Debug)]
pub struct SmokeCloud {
    /// Identifiant unique (la carte graphique garde la grille tant qu'il ne change pas).
    pub id: u64,
    pub age: f32,
    /// Coin bas de la grille (monde) et nombre de cases.
    pub origin: Vec3,
    pub dims: [u32; 3],
    /// Par case : (reach × 0,8 × 255, 255 = pas de fumée ; 255 = bloc plein).
    pub cells: Vec<[u8; 2]>,
    pub tunnels: Vec<Tunnel>,
    /// Boîte (monde) des cases que la fumée atteint, avec une case de marge.
    pub bounds: (Vec3, Vec3),
}

impl SmokeCloud {
    pub fn new(p: Vec3, world: &World, _rnd: &mut dyn FnMut() -> f32) -> Self {
        use std::cmp::Reverse;
        use std::collections::BinaryHeap;
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let center = p - Vec3::Y * 0.1;
        // Grille alignée sur les blocs : 13 × 6 × 13 blocs autour de la grenade.
        let base = [
            center.x.floor() as i32 - 6,
            center.y.floor() as i32 - 1,
            center.z.floor() as i32 - 6,
        ];
        let blocks = [13, 6, 13];
        let per = (1.0 / SMOKE_CELL) as i32;
        let dims = blocks.map(|b| (b * per) as u32);
        let (nx, ny, nz) = (dims[0] as usize, dims[1] as usize, dims[2] as usize);
        let idx = |x: usize, y: usize, z: usize| (z * ny + y) * nx + x;
        let mut solid = vec![false; nx * ny * nz];
        for bz in 0..blocks[2] {
            for by in 0..blocks[1] {
                for bx in 0..blocks[0] {
                    let block = world.get(base[0] + bx, base[1] + by, base[2] + bz);
                    if !block.is_solid() {
                        continue;
                    }
                    // Bloc non plein (dalle, trappe, escalier, clôture…) : seules les cases
                    // dans sa matière sont fermées, la fumée remplit le reste.
                    let def = block.def();
                    let boxes = if def.full { Vec::new() } else { def.boxes() };
                    for k in 0..per * per * per {
                        let (cx, cy, cz) = (k % per, (k / per) % per, k / (per * per));
                        let mid = [cx, cy, cz].map(|c| (c * 16 / per + 8 / per) as u8);
                        if !def.full
                            && !boxes
                                .iter()
                                .any(|(a, b)| (0..3).all(|i| mid[i] >= a[i] && mid[i] < b[i]))
                        {
                            continue;
                        }
                        solid[idx(
                            (bx * per + cx) as usize,
                            (by * per + cy) as usize,
                            (bz * per + cz) as usize,
                        )] = true;
                    }
                }
            }
        }
        let origin = Vec3::new(base[0] as f32, base[1] as f32, base[2] as f32);
        // Case de départ : celle de la grenade (ou la première libre au-dessus).
        let cell_of = |q: Vec3| {
            let c = ((q - origin) / SMOKE_CELL).floor();
            [c.x as usize, c.y as usize, c.z as usize]
        };
        let mut start = cell_of(center + Vec3::Y * 0.15);
        while start[1] + 1 < ny && solid[idx(start[0], start[1], start[2])] {
            start[1] += 1;
        }
        // Volume du dôme en terrain dégagé, en cases.
        let target = (2.0 / 3.0 * std::f32::consts::PI * SMOKE_RX * SMOKE_RX * SMOKE_H
            / SMOKE_CELL.powi(3)) as usize;
        // Plus court chemin (Dijkstra, 26 voisins) ; monter coûte plus que s'étaler
        // (proportions du dôme), descendre un peu moins que monter (la fumée retombe).
        let mut dist = vec![f32::INFINITY; nx * ny * nz];
        let mut heap = BinaryHeap::new();
        let key = |d: f32| Reverse((d * 4096.0) as u32);
        dist[idx(start[0], start[1], start[2])] = 0.0;
        heap.push((key(0.0), start));
        let mut order: Vec<usize> = Vec::with_capacity(target);
        let up = SMOKE_RX / SMOKE_H;
        while let Some((Reverse(k), [x, y, z])) = heap.pop() {
            let i = idx(x, y, z);
            if (k as f32 / 4096.0) > dist[i] + 1e-3 {
                continue;
            }
            order.push(i);
            if order.len() >= target {
                break;
            }
            for dz in -1i32..=1 {
                for dy in -1i32..=1 {
                    for dx in -1i32..=1 {
                        if dx == 0 && dy == 0 && dz == 0 {
                            continue;
                        }
                        let (qx, qy, qz) = (x as i32 + dx, y as i32 + dy, z as i32 + dz);
                        if qx < 0
                            || qy < 0
                            || qz < 0
                            || qx >= nx as i32
                            || qy >= ny as i32
                            || qz >= nz as i32
                        {
                            continue;
                        }
                        let j = idx(qx as usize, qy as usize, qz as usize);
                        if solid[j] {
                            continue;
                        }
                        // Pas en diagonale à travers un coin de mur.
                        if (dx != 0 && solid[idx(qx as usize, y, z)])
                            || (dy != 0 && solid[idx(x, qy as usize, z)])
                            || (dz != 0 && solid[idx(x, y, qz as usize)])
                        {
                            continue;
                        }
                        let vy = if dy > 0 { up } else { up * 0.8 };
                        let step = Vec3::new(dx as f32, dy as f32 * vy, dz as f32).length();
                        let nd = dist[i] + step;
                        if nd < dist[j] {
                            dist[j] = nd;
                            heap.push((key(nd), [qx as usize, qy as usize, qz as usize]));
                        }
                    }
                }
            }
        }
        let far = order.last().map_or(1.0, |&i| dist[i]).max(1.0);
        // Front lissé : la distance par cases voisines fait des arêtes (octogone) ; deux
        // flous 3×3×3 (sans traverser les murs) arrondissent le nuage.
        let mut reach = vec![1.2f32; nx * ny * nz];
        for &i in &order {
            reach[i] = (dist[i] / far).min(1.0);
        }
        for _ in 0..2 {
            let src = reach.clone();
            for z in 1..nz - 1 {
                for y in 1..ny - 1 {
                    for x in 1..nx - 1 {
                        let i = idx(x, y, z);
                        if solid[i] {
                            continue;
                        }
                        let (mut sum, mut n) = (0.0, 0.0);
                        for k in 0..27 {
                            let j = idx(x + k % 3 - 1, y + (k / 3) % 3 - 1, z + k / 9 - 1);
                            if !solid[j] {
                                sum += src[j];
                                n += 1.0;
                            }
                        }
                        reach[i] = sum / n;
                    }
                }
            }
        }
        let (mut lo, mut hi) = ([usize::MAX; 3], [0usize; 3]);
        for (i, &r) in reach.iter().enumerate() {
            if r < 1.0 && !solid[i] {
                let c = [i % nx, (i / nx) % ny, i / (nx * ny)];
                for k in 0..3 {
                    lo[k] = lo[k].min(c[k]);
                    hi[k] = hi[k].max(c[k] + 1);
                }
            }
        }
        let bounds = if lo[0] == usize::MAX {
            (origin, origin)
        } else {
            let v = |c: [usize; 3]| Vec3::new(c[0] as f32, c[1] as f32, c[2] as f32);
            (
                origin + (v(lo) - Vec3::ONE).max(Vec3::ZERO) * SMOKE_CELL,
                origin
                    + (v(hi) + Vec3::ONE).min(Vec3::new(nx as f32, ny as f32, nz as f32))
                        * SMOKE_CELL,
            )
        };
        let mut cells = vec![[255u8, 0u8]; nx * ny * nz];
        for (c, &r) in cells.iter_mut().zip(&reach) {
            if r < 1.0 {
                c[0] = (r * SMOKE_REACH_SCALE * 255.0) as u8;
            }
        }
        for (c, &s) in cells.iter_mut().zip(&solid) {
            if s {
                c[1] = 255;
            }
        }
        Self {
            id: NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            age: 0.0,
            origin,
            dims,
            cells,
            tunnels: Vec::new(),
            bounds,
        }
    }

    fn tick(&mut self, dt: f32) {
        self.age += dt;
        for t in &mut self.tunnels {
            t.age += dt;
        }
        self.tunnels.retain(|t| t.age < TUNNEL_SHUT);
    }

    /// Une balle traverse la grille (rayon `from` + `dir` × `dist`) : petit tunnel.
    pub fn carve(&mut self, from: Vec3, dir: Vec3, dist: f32) {
        if self.age < 0.3 {
            return;
        }
        let max = self.origin
            + Vec3::new(
                self.dims[0] as f32,
                self.dims[1] as f32,
                self.dims[2] as f32,
            ) * SMOKE_CELL;
        let safe = |v: f32| {
            if v.abs() < 1e-6 {
                1e-6f32.copysign(v)
            } else {
                v
            }
        };
        let inv = Vec3::new(1.0 / safe(dir.x), 1.0 / safe(dir.y), 1.0 / safe(dir.z));
        let t1 = (self.origin - from) * inv;
        let t2 = (max - from) * inv;
        let t0 = t1.min(t2).max_element().max(0.0);
        let t1 = t1.max(t2).min_element().min(dist);
        if t1 <= t0 {
            return;
        }
        if self.tunnels.len() >= MAX_TUNNELS {
            self.tunnels.remove(0);
        }
        self.tunnels.push(Tunnel {
            a: from + dir * t0,
            b: from + dir * t1,
            age: 0.0,
        });
    }

    /// La fumée cache-t-elle `b` vu de `a` ? (impacts de balle derrière ou dans le nuage)
    pub fn hides(&self, a: Vec3, b: Vec3) -> bool {
        let len = (b - a).length();
        let n = (len / 0.3).ceil().max(1.0) as usize;
        let mut depth = 0.0;
        for i in 0..=n {
            depth += self.density(a + (b - a) * (i as f32 / n as f32)) * len / n as f32;
            if depth > 0.2 {
                return true;
            }
        }
        false
    }

    /// Fumée (0..1) au point `q`, comme le shader la calcule (sans les volutes).
    pub fn density(&self, q: Vec3) -> f32 {
        let c = ((q - self.origin) / SMOKE_CELL).floor();
        if c.min_element() < 0.0
            || c.x >= self.dims[0] as f32
            || c.y >= self.dims[1] as f32
            || c.z >= self.dims[2] as f32
        {
            return 0.0;
        }
        let (nx, ny) = (self.dims[0] as usize, self.dims[1] as usize);
        let cell = self.cells[(c.z as usize * ny + c.y as usize) * nx + c.x as usize];
        if cell[1] != 0 || cell[0] == 255 {
            return 0.0;
        }
        let reach = cell[0] as f32 / 255.0 / SMOKE_REACH_SCALE;
        let mut d = ((smoke_grow(self.age) - reach) * 8.0).clamp(0.0, 1.0);
        for t in &self.tunnels {
            let r = tunnel_radius(t.age);
            if r > 0.0 {
                d *= ((dist_to_segment(q, t.a, t.b) - r) / 0.08).clamp(0.0, 1.0);
            }
        }
        d
    }
}

/// Front de la fumée (même formule que le shader) : gonfle en ~1,6 s, reste pleine, puis
/// se retire du bord vers le centre de 14,5 s à 18 s.
pub fn smoke_grow(age: f32) -> f32 {
    let grow = 1.0 - (1.0 - (age / 1.6).clamp(0.0, 1.0)).powi(3);
    let fade = ((age - 14.5) / (SMOKE_LIFE - 14.5)).clamp(0.0, 1.0);
    grow * 1.12 - fade * 1.25
}

/// Rayon d'un tunnel selon son âge : ouvert 0,5 s, puis se referme jusqu'à 0,75 s.
pub fn tunnel_radius(age: f32) -> f32 {
    if age < TUNNEL_OPEN {
        TUNNEL_R
    } else {
        TUNNEL_R * (1.0 - (age - TUNNEL_OPEN) / (TUNNEL_SHUT - TUNNEL_OPEN)).max(0.0)
    }
}

fn dist_to_segment(p: Vec3, a: Vec3, b: Vec3) -> f32 {
    let ab = b - a;
    let t = ((p - a).dot(ab) / ab.length_squared().max(1e-6)).clamp(0.0, 1.0);
    (a + ab * t - p).length()
}

/// Au plus 4 grenades portées (CS).
pub const MAX_NADES: u8 = 4;

/// Grenade lancée.
pub struct Thrown {
    pub kind: Nade,
    pub pos: Vec3,
    pub vel: Vec3,
    pub age: f32,
    /// Temps passé immobile (fumigène, leurre).
    pub still: f32,
    /// Lanceur (partie en ligne : 0 = soi chez l'hôte, sinon le joueur).
    pub owner: u64,
    /// Graine du hasard de l'effet (même fumée chez tout le monde).
    pub seed: u32,
}

impl Thrown {
    /// Nouvelle grenade : la graine vient de son point de départ (identique chez tous les
    /// joueurs qui la simulent).
    pub fn new(kind: Nade, pos: Vec3, vel: Vec3, owner: u64) -> Self {
        let bits = pos.x.to_bits()
            ^ pos.y.to_bits().rotate_left(11)
            ^ pos.z.to_bits().rotate_left(22)
            ^ vel.x.to_bits().rotate_left(5)
            ^ vel.z.to_bits().rotate_left(17);
        Self {
            kind,
            pos,
            vel,
            age: 0.0,
            still: 0.0,
            owner,
            seed: bits | 1,
        }
    }
}

/// Effet en cours dans le monde.
pub enum Effect {
    /// Boule de l'explosion (centre, âge).
    Blast(Vec3, f32),
    /// Nuage de fumée (voir `SmokeCloud`).
    Smoke(SmokeCloud),
    /// Feu au sol : cases en flammes (dessus des blocs), âge, lanceur.
    Fire(Vec<Vec3>, f32, u64),
    /// Leurre : position, âge, prochain faux tir.
    Decoy(Vec3, f32, f32),
}

impl Effect {
    fn alive(&self) -> bool {
        match self {
            Effect::Blast(_, a) => *a < 0.6,
            Effect::Smoke(c) => c.age < SMOKE_LIFE,
            Effect::Fire(_, a, _) => *a < 7.0,
            Effect::Decoy(_, a, _) => *a < 15.0,
        }
    }
}

/// Ce que subit le joueur à cette image.
#[derive(Default)]
pub struct Hits {
    pub damage: f32,
    /// Aveuglement (0..1) d'une flash.
    pub flash: f32,
}

/// Pas de projectile : rebond sur les blocs pleins (vitesse amortie), roulé au sol.
pub fn step_thrown(t: &mut Thrown, world: &World, dt: f32, sounds: &mut Vec<(Sfx, f32, Vec3)>) {
    t.age += dt;
    t.vel.y -= GRAVITY * dt;
    let solid = |p: Vec3| {
        world
            .get(p.x.floor() as i32, p.y.floor() as i32, p.z.floor() as i32)
            .is_solid()
    };
    let steps = ((t.vel.length() * dt) / 0.1).ceil().max(1.0) as usize;
    let h = dt / steps as f32;
    let mut bounced = false;
    let mut grounded = false;
    for _ in 0..steps {
        for axis in 0..3 {
            let mut next = t.pos;
            next[axis] += t.vel[axis] * h;
            if solid(next) {
                let speed = t.vel[axis].abs();
                let floor = axis == 1 && t.vel[1] < 0.0;
                if floor && speed < 4.5 {
                    // Posée au sol : plus de rebond, elle roule (freinée plus bas).
                    t.vel[1] = 0.0;
                    grounded = true;
                    continue;
                }
                // Rebond : sur un mur 45 % de la vitesse sur l'axe touché ; sur le sol
                // seulement 25 %, et le glissement est plus freiné (le sol amortit).
                let (keep, slide) = if floor { (0.25, 0.6) } else { (0.45, 0.75) };
                t.vel[axis] = -t.vel[axis] * keep;
                for k in 0..3 {
                    if k != axis {
                        t.vel[k] *= slide;
                    }
                }
                if speed > 2.0 {
                    bounced = true;
                }
            } else {
                t.pos = next;
            }
        }
    }
    // Au sol : roulement freiné, indépendamment du nombre d'images par seconde.
    if grounded {
        let k = (1.0 - 5.0 * dt).max(0.0);
        t.vel.x *= k;
        t.vel.z *= k;
        if t.vel.x.hypot(t.vel.z) < 0.25 {
            t.vel.x = 0.0;
            t.vel.z = 0.0;
        }
    }
    if bounced {
        sounds.push((Sfx::Bounce, 0.0, t.pos));
    }
    if t.vel.length() < 0.3 {
        t.still += dt;
    } else {
        t.still = 0.0;
    }
}

/// Le projectile se déclenche-t-il maintenant ? (et s'il a touché le sol, pour le molotov)
pub fn detonates(t: &Thrown, world: &World) -> bool {
    let on_ground = world
        .get(
            t.pos.x.floor() as i32,
            (t.pos.y - 0.08).floor() as i32,
            t.pos.z.floor() as i32,
        )
        .is_solid();
    match t.kind {
        Nade::He | Nade::Flash => t.age >= 1.6,
        Nade::Smoke | Nade::Decoy => t.still >= 0.25 || t.age >= 6.0,
        Nade::Molotov => (on_ground && t.age > 0.05) || t.age >= 2.0,
    }
}

/// Ligne de vue libre entre deux points.
fn visible(world: &World, a: Vec3, b: Vec3) -> bool {
    let d = b - a;
    let len = d.length();
    if len < 0.01 {
        return true;
    }
    raycast(world, a.into(), (d / len).into(), len - 0.05).is_none()
}

/// Déclenche la grenade `t` : effets dans le monde, sons, et ce que le joueur subit.
#[allow(clippy::too_many_arguments)]
pub fn detonate(
    t: &Thrown,
    world: &World,
    eye: Vec3,
    look: Vec3,
    effects: &mut Vec<Effect>,
    sounds: &mut Vec<(Sfx, f32, Vec3)>,
    hits: &mut Hits,
    rnd: &mut dyn FnMut() -> f32,
) {
    let p = t.pos;
    match t.kind {
        Nade::He => {
            effects.push(Effect::Blast(p, 0.0));
            sounds.push((Sfx::He, 0.0, p));
            let d = (eye - p).length();
            // Dégâts de CS : jusqu'à ~98 au contact, nuls à 9 blocs ; à couvert, rien.
            if d < 9.0 && visible(world, p + Vec3::Y * 0.2, eye) {
                let k = 1.0 - d / 9.0;
                hits.damage += 98.0 * k * k;
            }
        }
        Nade::Flash => {
            effects.push(Effect::Blast(p, 0.25));
            sounds.push((Sfx::Flash, 0.0, p));
            let to = p - eye;
            let d = to.length();
            if d < 30.0 && visible(world, p + Vec3::Y * 0.1, eye) {
                let facing = look.dot(to / d.max(0.01)).clamp(-1.0, 1.0);
                let f = flash_angle_factor(facing.acos().to_degrees());
                hits.flash = hits.flash.max(f * (1.0 - d / 30.0).clamp(0.0, 1.0).sqrt());
            }
        }
        Nade::Smoke => {
            sounds.push((Sfx::Smoke, 0.0, p));
            effects.push(Effect::Smoke(SmokeCloud::new(p, world, rnd)));
        }
        Nade::Decoy => effects.push(Effect::Decoy(p, 0.0, 0.5)),
        Nade::Molotov => {
            sounds.push((Sfx::Molotov, 0.0, p));
            sounds.push((Sfx::Fire, 0.1, p));
            // Feu sur le dessus des blocs pleins autour du point d'impact (rayon ~2).
            let mut cells = Vec::new();
            let (cx, cz) = (p.x.floor() as i32, p.z.floor() as i32);
            for dz in -2..=2i32 {
                for dx in -2..=2i32 {
                    if dx * dx + dz * dz > 5 {
                        continue;
                    }
                    let (x, z) = (cx + dx, cz + dz);
                    // Sol le plus proche sous le point (3 blocs au plus).
                    let mut y = p.y.floor() as i32 + 1;
                    let mut found = None;
                    for _ in 0..4 {
                        if world.get(x, y - 1, z).is_solid() && !world.get(x, y, z).is_solid() {
                            found = Some(y);
                            break;
                        }
                        y -= 1;
                    }
                    if let Some(y) = found {
                        cells.push(Vec3::new(x as f32 + 0.5, y as f32, z as f32 + 0.5));
                    }
                }
            }
            effects.push(Effect::Fire(cells, 0.0, t.owner));
        }
    }
}

/// Avance les effets : âge, faux tirs du leurre, feu qui brûle le joueur (`feet`).
pub fn tick_effects(
    effects: &mut Vec<Effect>,
    dt: f32,
    feet: Vec3,
    sounds: &mut Vec<(Sfx, f32, Vec3)>,
    hits: &mut Hits,
    rnd: &mut dyn FnMut() -> f32,
) {
    for e in effects.iter_mut() {
        match e {
            Effect::Blast(_, a) => *a += dt,
            Effect::Smoke(c) => c.tick(dt),
            Effect::Fire(cells, a, _) => {
                *a += dt;
                // Dans les flammes : environ 40 points de vie par seconde (comme CS).
                if cells.iter().any(|c| {
                    (c.x - feet.x).abs() < 0.8
                        && (c.z - feet.z).abs() < 0.8
                        && (feet.y - c.y).abs() < 1.2
                }) {
                    hits.damage += 40.0 * dt;
                }
                // Crépitement relancé régulièrement.
                if (*a % 1.8) < dt && *a > 1.0 {
                    if let Some(c) = cells.first() {
                        sounds.push((Sfx::Fire, 0.0, *c));
                    }
                }
            }
            Effect::Decoy(p, a, next) => {
                *a += dt;
                *next -= dt;
                if *next <= 0.0 {
                    sounds.push((Sfx::Decoy, 0.0, *p));
                    // Rafales irrégulières de pistolet.
                    *next = 0.15 + rnd().abs() * if rnd() > 0.3 { 0.25 } else { 1.6 };
                }
            }
        }
    }
    effects.retain(|e| e.alive());
}

// ------------------------------------------------------------ modèles

/// Grenade : cylindre (corps) + bouchon et cuillère, haut = +Y, centrée.
pub fn nade_model(kind: Nade) -> Vec<Tri> {
    let (body, cap) = kind.colors();
    let mut tris = Vec::new();
    if kind == Nade::Molotov {
        // Bouteille : corps, col, chiffon.
        cylinder(&mut tris, 0.04, -0.07, 0.04, body, 10);
        cylinder(&mut tris, 0.015, 0.04, 0.10, body, 8);
        cylinder(&mut tris, 0.02, 0.10, 0.13, cap, 6);
        return tris;
    }
    let r = if kind == Nade::He { 0.035 } else { 0.03 };
    cylinder(&mut tris, r, -0.05, 0.04, body, 12);
    // Bande colorée (fumigène, flash) ou bouchon.
    cylinder(&mut tris, r * 1.02, 0.0, 0.015, cap, 12);
    cylinder(&mut tris, 0.014, 0.04, 0.065, [0.35, 0.35, 0.33], 8);
    // Cuillère le long du corps.
    boxy(
        &mut tris,
        Vec3::new(r * 0.9, -0.03, -0.006),
        Vec3::new(r + 0.008, 0.065, 0.006),
        [0.4, 0.4, 0.38],
    );
    tris
}

fn cylinder(tris: &mut Vec<Tri>, r: f32, y0: f32, y1: f32, color: [f32; 3], n: usize) {
    let p = |i: usize, y: f32| {
        let a = i as f32 / n as f32 * std::f32::consts::TAU;
        Vec3::new(a.cos() * r, y, a.sin() * r)
    };
    for i in 0..n {
        let j = (i + 1) % n;
        let (a, b, c, d) = (p(i, y0), p(j, y0), p(j, y1), p(i, y1));
        tris.push(Tri {
            p: [a, c, b],
            color,
        });
        tris.push(Tri {
            p: [a, d, c],
            color,
        });
        tris.push(Tri {
            p: [Vec3::new(0.0, y1, 0.0), c, d],
            color,
        });
        tris.push(Tri {
            p: [Vec3::new(0.0, y0, 0.0), a, b],
            color,
        });
    }
}

fn boxy(tris: &mut Vec<Tri>, a: Vec3, b: Vec3, color: [f32; 3]) {
    let v = |x: bool, y: bool, z: bool| {
        Vec3::new(
            if x { b.x } else { a.x },
            if y { b.y } else { a.y },
            if z { b.z } else { a.z },
        )
    };
    let faces = [
        [
            v(true, false, false),
            v(true, true, false),
            v(true, true, true),
            v(true, false, true),
        ],
        [
            v(false, false, false),
            v(false, false, true),
            v(false, true, true),
            v(false, true, false),
        ],
        [
            v(false, true, false),
            v(false, true, true),
            v(true, true, true),
            v(true, true, false),
        ],
        [
            v(false, false, false),
            v(true, false, false),
            v(true, false, true),
            v(false, false, true),
        ],
        [
            v(false, false, true),
            v(true, false, true),
            v(true, true, true),
            v(false, true, true),
        ],
        [
            v(false, false, false),
            v(false, true, false),
            v(true, true, false),
            v(true, false, false),
        ],
    ];
    for f in faces {
        tris.push(Tri {
            p: [f[0], f[1], f[2]],
            color,
        });
        tris.push(Tri {
            p: [f[2], f[3], f[0]],
            color,
        });
    }
}

/// Lancer : vitesse de départ (fort ou en cloche), un peu vers le haut comme dans CS.
pub fn throw_velocity(look: Vec3, lob: bool, player_vel: Vec3) -> Vec3 {
    let speed = if lob { THROW_LOB } else { THROW_FAST };
    let dir = (look + Vec3::Y * 0.12).normalize_or_zero();
    dir * speed + player_vel * 0.8
}

#[cfg(test)]
mod tests {
    use super::*;
    use minelib_world::{BlockId, ChunkPos};

    fn floor() -> World {
        let mut w = World::new(0);
        for dz in -1..=1 {
            for dx in -1..=1 {
                w.insert_empty(ChunkPos { x: dx, z: dz });
            }
        }
        for z in -16..32 {
            for x in -16..32 {
                w.set(x, 10, z, BlockId::STONE);
            }
        }
        w
    }

    #[test]
    fn grenades_fly_bounce_and_settle() {
        let w = floor();
        let mut t = Thrown {
            kind: Nade::Smoke,
            pos: Vec3::new(8.5, 12.6, 8.5),
            vel: throw_velocity(Vec3::new(0.0, 0.0, -1.0), true, Vec3::ZERO),
            age: 0.0,
            still: 0.0,
            owner: 0,
            seed: 1,
        };
        let mut sounds = Vec::new();
        let mut fired = false;
        for _ in 0..600 {
            step_thrown(&mut t, &w, 1.0 / 60.0, &mut sounds);
            assert!(t.pos.y > 10.95, "à travers le sol : {}", t.pos.y);
            if detonates(&t, &w) {
                fired = true;
                break;
            }
        }
        assert!(fired, "la fumigène doit se poser et partir");
        assert!(t.pos.z < 4.0, "lancée vers l'avant : {}", t.pos.z);
        assert!(sounds.iter().any(|s| s.0 == Sfx::Bounce));
    }

    #[test]
    fn grenades_settle_even_at_low_fps() {
        let w = floor();
        for dt in [1.0 / 240.0, 1.0 / 60.0, 1.0 / 10.0] {
            let mut t = Thrown {
                kind: Nade::Smoke,
                pos: Vec3::new(8.5, 12.6, 8.5),
                vel: throw_velocity(Vec3::new(0.0, -0.3, -1.0).normalize(), false, Vec3::ZERO),
                age: 0.0,
                still: 0.0,
                owner: 0,
                seed: 1,
            };
            let mut sounds = Vec::new();
            let mut fired = None;
            for i in 0..(5.0 / dt) as usize {
                step_thrown(&mut t, &w, dt, &mut sounds);
                if detonates(&t, &w) {
                    fired = Some(i as f32 * dt);
                    break;
                }
            }
            let at = fired.expect("doit partir");
            assert!(at < 3.0, "dt {dt} : posée seulement après {at} s");
        }
    }

    #[test]
    fn he_hurts_near_but_not_behind_a_wall() {
        let mut w = floor();
        let t = Thrown {
            kind: Nade::He,
            pos: Vec3::new(8.5, 11.1, 8.5),
            vel: Vec3::ZERO,
            age: 2.0,
            still: 0.0,
            owner: 0,
            seed: 1,
        };
        let mut rnd = || 0.5;
        let mut hits = Hits::default();
        let (mut e, mut s) = (Vec::new(), Vec::new());
        let eye = Vec3::new(10.5, 12.6, 8.5);
        detonate(&t, &w, eye, Vec3::X, &mut e, &mut s, &mut hits, &mut rnd);
        assert!(hits.damage > 40.0, "{}", hits.damage);
        for y in 11..16 {
            w.set(9, y, 8, BlockId::STONE);
        }
        let mut hits = Hits::default();
        detonate(&t, &w, eye, Vec3::X, &mut e, &mut s, &mut hits, &mut rnd);
        assert_eq!(hits.damage, 0.0, "à couvert");
    }

    #[test]
    fn flash_blinds_when_facing_it() {
        let w = floor();
        let t = Thrown {
            kind: Nade::Flash,
            pos: Vec3::new(8.5, 12.0, 4.5),
            vel: Vec3::ZERO,
            age: 2.0,
            still: 0.0,
            owner: 0,
            seed: 1,
        };
        let eye = Vec3::new(8.5, 12.6, 8.5);
        let mut rnd = || 0.5;
        let (mut e, mut s) = (Vec::new(), Vec::new());
        let mut face = Hits::default();
        detonate(&t, &w, eye, -Vec3::Z, &mut e, &mut s, &mut face, &mut rnd);
        let mut back = Hits::default();
        detonate(&t, &w, eye, Vec3::Z, &mut e, &mut s, &mut back, &mut rnd);
        // Sur le bord de l'écran (~50°) : beaucoup moins long qu'en plein centre.
        assert!(flash_angle_factor(50.0) < 0.5 && flash_angle_factor(10.0) == 1.0);
        assert!(
            face.flash > 0.8 && back.flash < 0.3,
            "{} / {}",
            face.flash,
            back.flash
        );
    }

    #[test]
    fn smoke_fills_a_dome_and_bullets_carve_small_tunnels_that_close() {
        let w = floor();
        let mut rnd = || 0.5;
        let t0 = std::time::Instant::now();
        let mut c = SmokeCloud::new(Vec3::new(8.5, 11.05, 8.5), &w, &mut rnd);
        eprintln!("simulation : {:?}", t0.elapsed());
        c.tick(4.0);
        let eye_h = 12.6;
        // Pleine au centre et jusqu'à ~4 blocs ; rien sous le sol ni au-delà de ~5,5 blocs.
        assert!(c.density(Vec3::new(8.5, eye_h, 8.5)) > 0.99);
        assert!(c.density(Vec3::new(12.0, eye_h - 1.0, 8.5)) > 0.99);
        assert_eq!(c.density(Vec3::new(14.5, eye_h, 8.5)), 0.0);
        assert_eq!(c.density(Vec3::new(8.5, 10.5, 8.5)), 0.0);
        assert!(
            c.density(Vec3::new(8.5, 13.5, 8.5)) > 0.99,
            "3 blocs de haut"
        );
        // Tir à travers : trou de 0,2 bloc de rayon, ouvert à 0,5 s, refermé à 1,5 s.
        c.carve(Vec3::new(-2.0, eye_h, 8.5), Vec3::X, 30.0);
        c.tick(0.5);
        assert_eq!(c.density(Vec3::new(8.5, eye_h, 8.5)), 0.0);
        assert!(
            c.density(Vec3::new(8.5, eye_h + 0.35, 8.5)) > 0.99,
            "petit trou"
        );
        assert_eq!(tunnel_radius(0.4), TUNNEL_R);
        c.tick(1.1);
        assert!(c.density(Vec3::new(8.5, eye_h, 8.5)) > 0.99, "refermé");
        // Dissipée à la fin.
        c.tick(SMOKE_LIFE - c.age);
        assert_eq!(c.density(Vec3::new(8.5, eye_h, 8.5)), 0.0);
    }

    #[test]
    fn smoke_flows_around_walls_and_down_corridors() {
        // Couloir de 2 blocs de large fermé de chaque côté : la fumée s'y étale plus loin.
        let mut w = floor();
        for x in -8..24 {
            for y in 11..16 {
                w.set(x, y, 7, BlockId::STONE);
                w.set(x, y, 10, BlockId::STONE);
            }
        }
        let mut rnd = || 0.5;
        let mut c = SmokeCloud::new(Vec3::new(8.5, 11.05, 8.5), &w, &mut rnd);
        c.tick(4.0);
        assert!(
            c.density(Vec3::new(14.0, 11.5, 8.5)) > 0.99,
            "plus loin dans le couloir"
        );
        assert_eq!(
            c.density(Vec3::new(8.5, 11.5, 11.5)),
            0.0,
            "pas derrière le mur"
        );
    }

    #[test]
    fn smoke_fills_around_partial_blocks() {
        // Trappe au plafond d'une case et dalle du bas : la fumée remplit le reste.
        let mut w = floor();
        let reg = minelib_world::registry::registry();
        w.set(9, 12, 8, reg.by_key("trapdoor:2").unwrap());
        w.set(7, 11, 8, reg.by_key("slab:stone:bottom").unwrap());
        let mut rnd = || 0.5;
        let mut c = SmokeCloud::new(Vec3::new(8.5, 11.05, 8.5), &w, &mut rnd);
        c.tick(4.0);
        assert!(
            c.density(Vec3::new(9.5, 12.4, 8.5)) > 0.99,
            "sous la trappe"
        );
        assert_eq!(c.density(Vec3::new(9.5, 12.95, 8.5)), 0.0, "dans la trappe");
        assert!(c.density(Vec3::new(7.5, 11.8, 8.5)) > 0.99, "sur la dalle");
        assert_eq!(c.density(Vec3::new(7.5, 11.2, 8.5)), 0.0, "dans la dalle");
    }

    #[test]
    fn molotov_burns_the_ground_and_hurts() {
        let w = floor();
        let t = Thrown {
            kind: Nade::Molotov,
            pos: Vec3::new(8.5, 11.05, 8.5),
            vel: Vec3::ZERO,
            age: 0.2,
            still: 0.0,
            owner: 0,
            seed: 1,
        };
        assert!(detonates(&t, &w));
        let mut rnd = || 0.5;
        let (mut e, mut s) = (Vec::new(), Vec::new());
        let mut hits = Hits::default();
        detonate(
            &t,
            &w,
            Vec3::ZERO,
            Vec3::X,
            &mut e,
            &mut s,
            &mut hits,
            &mut rnd,
        );
        let mut burn = Hits::default();
        for _ in 0..60 {
            tick_effects(
                &mut e,
                1.0 / 60.0,
                Vec3::new(9.2, 11.0, 8.5),
                &mut s,
                &mut burn,
                &mut rnd,
            );
        }
        assert!((30.0..50.0).contains(&burn.damage), "{}", burn.damage);
    }
}
