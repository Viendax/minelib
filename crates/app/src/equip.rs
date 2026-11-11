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
/// Tunnel creusé par une balle : rayon, ouvert 1 s, refermé à 1,5 s.
const TUNNEL_R: f32 = 0.38;
const TUNNEL_OPEN: f32 = 1.0;
const TUNNEL_SHUT: f32 = 1.5;
const MAX_TUNNELS: usize = 48;

/// Bouffée d'un nuage : décalage depuis le centre, rayon, moment où elle apparaît et où
/// elle se dissipe, teinte et phase de son mouvement.
#[derive(Clone, Debug)]
pub struct Puff {
    pub off: Vec3,
    pub r: f32,
    delay: f32,
    end: f32,
    pub shade: f32,
    phase: f32,
    /// Entourée de bouffées de tous côtés : cachée tant que le nuage est plein.
    pub inner: bool,
}

/// Trait de balle à travers la fumée (segment) et son âge.
#[derive(Clone, Debug)]
struct Tunnel {
    a: Vec3,
    b: Vec3,
    age: f32,
}

/// Nuage de fumigène : des bouffées opaques serrées qui remplissent un dôme (sans passer
/// à travers les murs), gonflent depuis la grenade, ondulent, et se dissipent une à une à
/// la fin. Les balles y creusent des tunnels qui se referment entre 1 et 1,5 s.
#[derive(Clone, Debug)]
pub struct SmokeCloud {
    pub center: Vec3,
    pub age: f32,
    pub puffs: Vec<Puff>,
    tunnels: Vec<Tunnel>,
}

impl SmokeCloud {
    pub fn new(p: Vec3, world: &World, rnd: &mut dyn FnMut() -> f32) -> Self {
        let center = p - Vec3::Y * 0.1;
        let solid = |q: Vec3| {
            world
                .get(q.x.floor() as i32, q.y.floor() as i32, q.z.floor() as i32)
                .is_solid()
        };
        let step = 0.28;
        let n = (SMOKE_RX / step).ceil() as i32;
        let ny = (SMOKE_H / step).ceil() as i32;
        let mut puffs = Vec::new();
        let mut grid = std::collections::HashMap::new();
        for iy in 0..=ny {
            for iz in -n..=n {
                for ix in -n..=n {
                    let jitter = Vec3::new(rnd(), rnd(), rnd()) * 0.1;
                    let off = Vec3::new(ix as f32, iy as f32 + 0.6, iz as f32) * step + jitter;
                    let e = (off.x * off.x + off.z * off.z) / (SMOKE_RX * SMOKE_RX)
                        + (off.y / SMOKE_H).powi(2);
                    // Dôme, et pas de fumée dans les blocs ni derrière un mur.
                    if e > 1.0
                        || solid(center + off)
                        || !visible(world, center + Vec3::Y * 0.4, center + off)
                    {
                        continue;
                    }
                    let reach = e.sqrt();
                    grid.insert([ix, iy, iz], puffs.len());
                    puffs.push(Puff {
                        off,
                        r: 0.22 + rnd().abs() * 0.1,
                        delay: reach * 1.0 + rnd().abs() * 0.15,
                        // CS : nuage plein jusqu'à ~15 s, puis les bords s'effilochent en
                        // premier et tout a disparu à 18 s (dissipation de 1,2 s par bouffée).
                        end: 15.2 - reach * 1.0 + rnd().abs() * 1.6,
                        // Haut du nuage plus clair (éclairé), bas plus sombre.
                        shade: 0.82 + 0.18 * (off.y / SMOKE_H) + rnd() * 0.04,
                        phase: rnd() * std::f32::consts::TAU,
                        inner: false,
                    });
                }
            }
        }
        // Bouffées intérieures : leurs 6 voisines existent (on ne dessine que la surface).
        for (&[x, y, z], &i) in &grid {
            puffs[i].inner = [
                [1, 0, 0],
                [-1, 0, 0],
                [0, 1, 0],
                [0, -1, 0],
                [0, 0, 1],
                [0, 0, -1],
            ]
            .iter()
            .all(|d| grid.contains_key(&[x + d[0], y + d[1], z + d[2]]) || (y == 0 && d[1] == -1));
        }
        Self {
            center,
            age: 0.0,
            puffs,
            tunnels: Vec::new(),
        }
    }

    fn tick(&mut self, dt: f32) {
        self.age += dt;
        for t in &mut self.tunnels {
            t.age += dt;
        }
        self.tunnels.retain(|t| t.age < TUNNEL_SHUT);
    }

    /// Position actuelle d'une bouffée : elle part de la grenade, s'écarte en gonflant,
    /// puis tourne lentement autour du centre en ondulant.
    pub fn puff_pos(&self, p: &Puff) -> Vec3 {
        let t = ((self.age - p.delay * 0.6) / 1.2).clamp(0.0, 1.0);
        let out = 1.0 - (1.0 - t).powi(3);
        let spin = self.age * 0.05 * (1.2 - p.off.length() / SMOKE_RX);
        let (sn, cs) = spin.sin_cos();
        let o = Vec3::new(
            p.off.x * cs - p.off.z * sn,
            p.off.y,
            p.off.x * sn + p.off.z * cs,
        );
        let bob = Vec3::new(
            (self.age * 0.5 + p.phase).sin() * 0.08,
            (self.age * 0.7 + p.phase * 1.3).sin() * 0.1,
            (self.age * 0.4 + p.phase * 0.7).cos() * 0.08,
        );
        self.center + o * (0.25 + 0.75 * out) + bob
    }

    /// Taille relative d'une bouffée (0 = invisible) : naissance, dissipation, tunnels.
    pub fn puff_scale(&self, p: &Puff) -> f32 {
        let born = ((self.age - p.delay) / 0.7).clamp(0.0, 1.0);
        let born = 1.0 - (1.0 - born) * (1.0 - born);
        let gone = 1.0 - ((self.age - p.end) / 1.2).clamp(0.0, 1.0);
        let breathe = 1.0 + 0.06 * (self.age * 0.9 + p.phase).sin();
        let mut k = born * gone * breathe;
        if k <= 0.0 {
            return 0.0;
        }
        let pos = self.puff_pos(p);
        for t in &self.tunnels {
            let r = tunnel_radius(t.age);
            if r <= 0.0 {
                continue;
            }
            let d = dist_to_segment(pos, t.a, t.b);
            // La bouffée rétrécit près du trou : plus rien sur l'axe, normale à r + 0,6 p.r
            // (le passage dégagé fait alors un peu plus d'un bloc de large).
            k *= ((d - r) / (p.r * 0.6)).clamp(0.0, 1.0);
        }
        k
    }

    /// Une balle traverse le nuage (rayon `from` + `dir` × `dist`) : tunnel.
    pub fn carve(&mut self, from: Vec3, dir: Vec3, dist: f32) {
        if self.age < 0.3 {
            return;
        }
        // Entrée et sortie du dôme, approché par une sphère aplatie.
        let k = Vec3::new(1.0 / SMOKE_RX, 1.0 / SMOKE_H, 1.0 / SMOKE_RX);
        let o = (from - self.center) * k;
        let d = dir * k;
        let a = d.dot(d);
        let b = 2.0 * o.dot(d);
        let c = o.dot(o) - 1.3 * 1.3;
        let disc = b * b - 4.0 * a * c;
        if disc <= 0.0 {
            return;
        }
        let sq = disc.sqrt();
        let t0 = ((-b - sq) / (2.0 * a)).max(0.0);
        let t1 = ((-b + sq) / (2.0 * a)).min(dist);
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

    /// Faut-il dessiner les bouffées intérieures autour de `pos` ? Oui pendant que le
    /// nuage gonfle ou se dissipe, quand l'œil est dedans, ou près d'un tunnel ouvert.
    pub fn inner_visible(&self, pos: Vec3, eye_inside: bool) -> bool {
        self.age < 2.0
            || self.age > 14.0
            || eye_inside
            || self.tunnels.iter().any(|t| {
                tunnel_radius(t.age) > 0.0 && dist_to_segment(pos, t.a, t.b) < TUNNEL_R + 0.9
            })
    }

    /// L'œil est-il dans le nuage ?
    pub fn contains(&self, eye: Vec3) -> bool {
        let d = (eye - self.center) / Vec3::new(SMOKE_RX + 0.5, SMOKE_H + 0.5, SMOKE_RX + 0.5);
        d.y > -0.2 && d.length() < 1.0
    }

    /// 0..1 : l'œil est dans un tunnel encore ouvert.
    fn tunnel_open(&self, eye: Vec3) -> f32 {
        self.tunnels
            .iter()
            .map(|t| {
                let r = tunnel_radius(t.age);
                if r <= 0.0 {
                    0.0
                } else {
                    (1.0 - dist_to_segment(eye, t.a, t.b) / r).clamp(0.0, 1.0)
                }
            })
            .fold(0.0, f32::max)
    }
}

/// Rayon d'un tunnel selon son âge : ouvert 1 s, puis se referme jusqu'à 1,5 s.
fn tunnel_radius(age: f32) -> f32 {
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
}

/// Effet en cours dans le monde.
pub enum Effect {
    /// Boule de l'explosion (centre, âge).
    Blast(Vec3, f32),
    /// Nuage de fumée (voir `SmokeCloud`).
    Smoke(SmokeCloud),
    /// Feu au sol : cases en flammes (dessus des blocs), âge.
    Fire(Vec<Vec3>, f32),
    /// Leurre : position, âge, prochain faux tir.
    Decoy(Vec3, f32, f32),
}

impl Effect {
    fn alive(&self) -> bool {
        match self {
            Effect::Blast(_, a) => *a < 0.6,
            Effect::Smoke(c) => c.age < SMOKE_LIFE,
            Effect::Fire(_, a) => *a < 7.0,
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
            effects.push(Effect::Fire(cells, 0.0));
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
            Effect::Fire(cells, a) => {
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

/// Distance de l'œil au nuage de fumée le plus proche, relative au rayon (< 1 : dedans).
pub fn smoke_depth(effects: &[Effect], eye: Vec3) -> f32 {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::Smoke(c) if c.age > 0.8 && c.age < SMOKE_LIFE - 1.5 => {
                let d = eye - (c.center + Vec3::Y * 1.0);
                // Dans un tunnel creusé par les balles : on voit encore un peu.
                let open = c.tunnel_open(eye);
                Some((d / Vec3::new(SMOKE_RX, SMOKE_H * 0.6, SMOKE_RX)).length() + open * 0.6)
            }
            _ => None,
        })
        .fold(f32::MAX, f32::min)
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
    fn smoke_is_dense_and_bullets_carve_tunnels_that_close() {
        let w = floor();
        let mut rnd = {
            let mut x = 1u32;
            move || {
                x = x.wrapping_mul(1664525).wrapping_add(1013904223);
                (x >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0
            }
        };
        let mut c = SmokeCloud::new(Vec3::new(8.5, 11.05, 8.5), &w, &mut rnd);
        assert!(c.puffs.len() > 3000, "{} bouffées", c.puffs.len());
        assert!(c.puffs.iter().filter(|p| !p.inner).count() < c.puffs.len() / 2);
        eprintln!("{} bouffées", c.puffs.len());
        assert!(c.puffs.iter().all(|p| p.off.y > 0.0), "rien sous le sol");
        c.tick(4.0);
        // Bouffée la plus proche de l'axe du tir (à hauteur d'œil) : visible avant.
        let near = c
            .puffs
            .iter()
            .enumerate()
            .filter(|(_, p)| c.puff_scale(p) > 0.5)
            .min_by(|a, b| {
                let d = |p: &Puff| {
                    let q = c.puff_pos(p);
                    (q.y - 12.6).abs().max((q.z - 8.5).abs())
                };
                d(a.1).total_cmp(&d(b.1))
            })
            .map(|(i, _)| i)
            .unwrap();
        let axis = |c: &SmokeCloud| c.puff_scale(&c.puffs[near]);
        assert!(axis(&c) > 0.5);
        c.carve(Vec3::new(-2.0, 12.6, 8.5), Vec3::X, 30.0);
        c.tick(0.5);
        let open = axis(&c);
        assert!(open < 0.15, "trou ouvert à 0,5 s : {open}");
        // Il se referme entre 1 et 1,5 s.
        assert_eq!(tunnel_radius(0.9), TUNNEL_R);
        assert!(tunnel_radius(1.25) > 0.0 && tunnel_radius(1.25) < TUNNEL_R);
        c.tick(1.1);
        assert!(axis(&c) > 0.5, "refermé à 1,5 s : {}", axis(&c));
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
