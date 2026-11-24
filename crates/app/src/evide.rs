//! (v269) L'Évidé : un monstre de gelée blanche, sur le modèle du joueur étiré à 3,5 blocs.
//!
//! Demandes de Viendax (23 et 24 novembre) : « basé sur notre modèle de joueur, la même
//! largeur, 3,5 blocs de haut » ; « tout blanc, avec du relief gris clair ou jaunâtre » ; « pas
//! d'yeux, juste leur contour, blanc/gris à cet endroit » ; « une bouche sombre, creuse,
//! effrayante, creusée dedans » ; « il est en gelée, même dedans » ; « il marche, court, rampe
//! (3,5 de long, 1 de large et de haut, à la vitesse du sneak), avec des transitions et des
//! mouvements effrayants » ; « il se déplace comme un joueur ou un PNJ : à la vitesse de la
//! marche quand il n'attaque pas, à celle d'un joueur quand il poursuit » ; « il nous voit si
//! on est sur son "écran", jusqu'à 50 blocs, pas à travers les blocs pleins » ; « il tourne
//! assez régulièrement » ; « il nous suit tant qu'il peut, jusqu'à nous manger ou être mangé » ;
//! « les épées ne lui font rien : il faut le manger vivant, de près ; on l'attrape et on en
//! mange un morceau, qui ne revient jamais » ; « il nous attrape avec ses mains, se penche pour
//! mordre (en rampant, seule sa tête bouge), ouvre grand la bouche et la referme sur nous ».
//!
//! Son corps est celui du joueur (`player_model::rig`), chaque pavé étiré en hauteur, découpé
//! en petits cubes (`grids` : ceux du joueur, deux fois plus de rangées). Ce qu'on lui mange
//! est retiré cube par cube (`flesh::Flesh`, sans repousse) ; sa bouche est un creux de la
//! tête qui s'agrandit et se referme (`mouth`). Il se déplace avec les allures, la pesanteur
//! et le saut du joueur (`player::GAIT`) et les mêmes boîtes de collision ; haut de 3,5 blocs,
//! il rampe partout où le plafond est plus bas.
//!
//! En solo seulement pour l'instant (en ligne, le relais devra tout valider).

use std::f32::consts::{FRAC_PI_2, PI, TAU};

use glam::{Mat4, Vec3};
use minelib_render::{EntityVertex, ShadowBox};
use minelib_world::{ChunkPos, World};

use crate::flesh::{Flesh, PARTS};

/// Sa taille debout (blocs) et son étirement par rapport au joueur.
pub const HEIGHT: f32 = 3.5;
const TALL: f32 = HEIGHT / crate::player::HEIGHT;
/// Demi-largeur de sa boîte de collision : celle du joueur (« la même largeur »).
const HALF: f32 = crate::player::HALF_WIDTH;
/// Hauteur de sa boîte quand il rampe (« 1 de large et de haut »).
const CRAWL_H: f32 = 0.95;
/// En rampant : hauteur de ses hanches, et leur recul derrière sa position (sa boîte est à
/// l'avant, autour de la poitrine et de la tête ; le corps traîne derrière, 3,5 de long).
const CRAWL_HIP: f32 = 0.5;
const CRAWL_BACK: f32 = 1.36;
/// Il nous voit jusque-là (blocs), dans le champ de son « écran » (celui du jeu : 75° de
/// haut, en 16:9).
pub const SEE: f32 = 50.0;
const VIEW_TAN_Y: f32 = 0.767_327;
const VIEW_TAN_X: f32 = VIEW_TAN_Y * 16.0 / 9.0;
/// Tout près (blocs), il nous sent sans nous voir (mais pas à travers un bloc plein).
const FEEL: f32 = 3.0;
/// Sans nous voir, il nous cherche encore ce temps-là (s) avant de reprendre sa ronde.
const GIVE_UP_S: f32 = 10.0;
/// Il s'arrête à cette distance (blocs, à plat) pour nous attraper : debout, en rampant.
const HOLD: f32 = 1.5;
const CRAWL_HOLD: f32 = 0.9;
/// Une morsure : durée du geste (s), instant où la bouche se referme.
const BITE_S: f32 = 1.6;
const BITE_AT: f32 = 1.04;
/// On peut le manger de cette distance (blocs) : « de près ».
pub const EAT_REACH: f32 = 2.2;
/// Côté (cubes) du morceau qu'on lui mange à chaque bouchée.
const OUR_BITE: usize = 4;
/// Il meurt quand on a mangé cette part de son corps, ou cette part de sa tête.
const EATEN_DEAD: f32 = 0.5;
const HEAD_DEAD: f32 = 0.8;
/// Sa fin : il s'affaisse comme une gelée (s).
const DIE_S: f32 = 0.7;
/// Au plus tant de monstres à la fois ; au plus tant venus tout seuls.
pub const MAX: usize = 16;
const NATURAL_MAX: usize = 2;
/// (v269) Apparition naturelle : la nuit, autour de chaque joueur, entre `SPAWN_NEAR` et
/// `SPAWN_FAR` blocs (« les mobs apparaissent dans les 256 blocs autour de nous »), en moyenne
/// une fois toutes les `SPAWN_EVERY_S` secondes de nuit. « 3 fois plus qu'un zombie avec un
/// lingot de fer » : dans Minecraft un zombie a 2,5 % de chances d'un butin rare, un tiers
/// d'un lingot de fer ; trois fois cela, 2,5 % des apparitions d'un monstre commun, soit à peu
/// près une par vingt minutes de nuit et par joueur. **Ce monstre est la référence des taux
/// d'apparition** : les prochains se régleront par rapport à lui.
pub const SPAWN_EVERY_S: f32 = 1200.0;
const SPAWN_NEAR: f32 = 24.0;
const SPAWN_FAR: f32 = 256.0;
/// Trop loin du joueur (blocs) : il disparaît.
const DESPAWN: f32 = 320.0;

const LEG_L: usize = 0;
const LEG_R: usize = 1;
const CHEST: usize = 2;
const ARM_L: usize = 3;
const ARM_R: usize = 4;
const HEAD: usize = 5;
const BELLY: usize = 6;

// Ses couleurs : blanc, relief gris clair ou jaunâtre, contour des yeux, bouche.
const WHITE: u32 = 0xf3f1ea;
const GREY: u32 = 0xdcdad2;
const YELLOW: u32 = 0xe9e2c4;
const HOLLOW: u32 = 0xcbc9c1;
const OUTLINE: u32 = 0xa3a29c;
const SOCKET: u32 = 0xd9d8d3;
const MOUTH: [u32; 3] = [0x2a1418, 0x160a0d, 0x070304];

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Rapproche `v` de `to` d'au plus `step`.
fn approach(v: f32, to: f32, step: f32) -> f32 {
    v + (to - v).clamp(-step, step)
}

/// Écart d'angle ramené entre −π et π.
fn turn(to: f32, from: f32) -> f32 {
    (to - from + PI).rem_euclid(TAU) - PI
}

/// Vers où regarde un corps tourné de `yaw` (le lacet du joueur).
fn forward(yaw: f32) -> Vec3 {
    Vec3::new(yaw.sin(), 0.0, -yaw.cos())
}

/// Grille de cubes de chaque pavé : celle du joueur, deux fois plus de rangées (le corps est
/// étiré de 1,94 : les cubes restent à peu près cubiques).
pub fn grids() -> [[usize; 3]; PARTS] {
    crate::player_model::flesh_grids().map(|[x, y, z]| [x, y * 2, z])
}

/// Sa posture, pour le dessin.
#[derive(Clone, Copy, Debug, Default)]
pub struct Anim {
    /// Phase du pas et son ampleur (0 … 1).
    pub walk: f32,
    pub swing: f32,
    /// 0 debout … 1 à plat ventre.
    pub crawl: f32,
    /// Penche du buste vers l'avant, debout (rad) : voûté, penché sur sa proie.
    pub hunch: f32,
    /// Angle des bras dans le monde (0 : pendants, π/2 : tendus devant).
    pub arms: f32,
    /// 0 … 1 : les bras se referment sur la proie.
    pub grab: f32,
    /// Bouche : −0,5 refermée, 0 au repos (un creux), 1 grande ouverte.
    pub mouth: f32,
    /// Tête : lacet (rad, positif vers sa droite), tangage (positif vers le haut), roulis.
    pub head_yaw: f32,
    pub head_pitch: f32,
    pub head_roll: f32,
    /// 0 … 1 : il s'affaisse (sa fin).
    pub sink: f32,
}

/// Ses pavés dans cette posture : repère (pixels → monde) et coins (pixels), dans l'ordre
/// du modèle du joueur.
pub fn parts(pos: Vec3, yaw: f32, a: &Anim) -> [(Mat4, [f32; 3], [f32; 3]); PARTS] {
    let r = crate::player_model::rig();
    let tall = |i: usize| {
        let (mut lo, mut hi) = r.boxes[i];
        lo[1] *= TALL;
        hi[1] *= TALL;
        (lo, hi)
    };
    let c = a.crawl;
    // Penche du buste : voûté debout, à l'horizontale en rampant.
    let pitch = lerp(a.hunch, FRAC_PI_2, c);
    let hip_y = lerp(r.leg * TALL, CRAWL_HIP / r.px, c);
    let base = Mat4::from_translation(pos)
        * Mat4::from_rotation_y(-yaw)
        * Mat4::from_scale(Vec3::new(r.px, r.px * (1.0 - 0.94 * a.sink), r.px));
    let hip = base * Mat4::from_translation(Vec3::new(0.0, hip_y, c * CRAWL_BACK / r.px));
    let mut out = [(Mat4::IDENTITY, [0.0; 3], [0.0; 3]); PARTS];
    let stride = a.walk.sin() * a.swing;
    // Jambes : balancées debout ; traînées derrière, presque à plat, en rampant.
    for (i, side, sign) in [(LEG_L, -r.leg_x, 1.0f32), (LEG_R, r.leg_x, -1.0)] {
        let stand = stride * 0.75 * sign;
        let lie = -FRAC_PI_2 + 0.24 + stride * 0.1 * sign;
        let (lo, hi) = tall(i);
        let xf = hip
            * Mat4::from_translation(Vec3::new(side, 0.0, 0.0))
            * Mat4::from_rotation_x(lerp(stand, lie, c));
        out[i] = (xf, lo, hi);
    }
    let torso = hip * Mat4::from_rotation_x(-pitch);
    for i in [CHEST, BELLY] {
        let (lo, hi) = tall(i);
        out[i] = (torso, lo, hi);
    }
    // Bras : pendants, tendus vers la proie, refermés sur elle ; en rampant, ils tirent le
    // corps l'un après l'autre, tendus devant au ras du sol.
    for (i, side, sign) in [(ARM_L, -r.arm_x, -1.0f32), (ARM_R, r.arm_x, 1.0)] {
        // (Tendus vers la proie, ils ne se balancent presque plus.)
        let loose = (1.0 - a.grab) * (1.0 - 0.75 * (a.arms / 1.25).clamp(0.0, 1.0));
        let stand = a.arms - stride * 0.5 * sign * loose;
        let lie = FRAC_PI_2 - 0.3 + stride * 0.17 * sign;
        let (lo, hi) = tall(i);
        let xf = torso
            * Mat4::from_translation(Vec3::new(side, r.shoulder_y * TALL, 0.0))
            * Mat4::from_rotation_y(sign * 0.32 * a.grab)
            * Mat4::from_rotation_x(pitch + lerp(stand, lie, c))
            * Mat4::from_rotation_z(sign * 0.08);
        out[i] = (xf, lo, hi);
    }
    // Tête : elle se redresse d'autant que le buste penche, puis regarde où elle veut.
    let (lo, hi) = tall(HEAD);
    let xf = torso
        * Mat4::from_translation(Vec3::new(0.0, r.body_h * TALL, 0.0))
        * Mat4::from_rotation_y(-a.head_yaw)
        * Mat4::from_rotation_x(pitch + a.head_pitch)
        * Mat4::from_rotation_z(a.head_roll);
    out[HEAD] = (xf, lo, hi);
    out
}

/// Le cube `c` de la tête (grille 8 × 16 × 8) est dans le creux de la bouche, ouverte de
/// `open` (−0,5 refermée : une fente … 1 grande ouverte). Un ovale creusé dans le bas du
/// visage, profond de plus de la moitié de la tête.
pub fn mouth(c: [usize; 3], d: [usize; 3], open: f32) -> bool {
    let o = open.clamp(-0.5, 1.0);
    let (x, y, z) = (c[0] as f32, c[1] as f32, c[2] as f32);
    let (sx, sy, sz) = (d[0] as f32 / 8.0, d[1] as f32 / 16.0, d[2] as f32 / 8.0);
    // Demi-largeur autour du milieu, rangées du bas et du haut, profondeur.
    let half = (2.0 + o.max(0.0)) * sx;
    let lo = ((3.0 - 2.0 * o) * sy).floor();
    let hi = ((5.0 + 3.5 * o) * sy).floor().max(lo);
    let deep = (5.0 + o.max(0.0)) * sz;
    let mid = (d[0] as f32 - 1.0) / 2.0;
    if (x - mid).abs() >= half || y < lo || y > hi || z >= deep {
        return false;
    }
    // Coins arrondis dès qu'elle est assez ouverte.
    let corner = (x - mid).abs() + 1.0 >= half && (y == lo || y == hi);
    !(corner && hi - lo >= 3.0)
}

fn hash(a: usize, b: usize, c: usize, d: usize) -> u32 {
    let mut h = (a as u32).wrapping_mul(0x9e37_79b1)
        ^ (b as u32).wrapping_mul(0x85eb_ca77)
        ^ (c as u32).wrapping_mul(0xc2b2_ae3d)
        ^ (d as u32).wrapping_mul(0x27d4_eb2f);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2c1b_3c6d);
    h ^ (h >> 13)
}

/// Sa peau : la couleur du cube `c` du pavé `part` vu par sa face `k` (0 devant, 1 dos, 2 et
/// 3 côtés, 4 dessus, 5 dessous). Blanc ; des plaques gris clair ou jaunâtres ; des côtes et
/// un ventre creux qui se devinent ; à la place des yeux, leur seul contour.
pub fn skin(part: usize, c: [usize; 3], d: [usize; 3], k: usize) -> u32 {
    let [x, y, _] = c;
    if part == HEAD && k == 0 {
        // Deux orbites de 3 × 4 cubes au-dessus de la bouche : un contour gris, rien dedans.
        for x0 in [0usize, d[0] - 3] {
            if (x0..x0 + 3).contains(&x) && (10..=13).contains(&(y * 16 / d[1])) {
                let row = y * 16 / d[1];
                let edge = x == x0 || x == x0 + 2 || row == 10 || row == 13;
                return if edge { OUTLINE } else { SOCKET };
            }
        }
    }
    let middle = x == (d[0] - 1) / 2 || x == d[0] / 2;
    if part == CHEST && k <= 1 && y % 3 == 1 && !middle {
        return GREY;
    }
    if part == BELLY && k == 0 && x > 0 && x + 1 < d[0] && y >= 2 {
        return HOLLOW;
    }
    match hash(part, x / 2, y / 3, c[2] / 2) % 100 {
        0..=13 => GREY,
        14..=24 => YELLOW,
        _ => WHITE,
    }
}

fn srgb(c: u32) -> [f32; 3] {
    [16, 8, 0].map(|s| (((c >> s) & 255) as f32 / 255.0).powf(2.2))
}

/// Un pavé de L'Évidé, cube par cube : sa peau au-dehors, le noir de sa bouche dans le creux
/// de la tête, de la gelée (`flesh::jelly`) là où on l'a mangé.
#[allow(clippy::too_many_arguments)]
fn emit_part(
    out: &mut Vec<EntityVertex>,
    xf: Mat4,
    min: [f32; 3],
    max: [f32; 3],
    part: usize,
    dims: [usize; 3],
    light: [f32; 3],
    flesh: &Flesh,
    open: f32,
) {
    // Les six côtés d'un cube : (axe, sens), dans l'ordre des faces de `skin`.
    const SIDES: [(usize, i32); 6] = [(2, -1), (2, 1), (0, 1), (0, -1), (1, 1), (1, -1)];
    let (lo, hi) = (Vec3::from(min), Vec3::from(max));
    let step = (hi - lo) / Vec3::new(dims[0] as f32, dims[1] as f32, dims[2] as f32);
    let lit = EntityVertex::light(light, true);
    let hollow = |c: [usize; 3]| part == HEAD && mouth(c, dims, open);
    for z in 0..dims[2] {
        for y in 0..dims[1] {
            for x in 0..dims[0] {
                let c = [x, y, z];
                if flesh.gone(part, dims, c) || hollow(c) {
                    continue;
                }
                for (k, (axis, sign)) in SIDES.into_iter().enumerate() {
                    let n = c[axis] as i32 + sign;
                    let outside = n < 0 || n >= dims[axis] as i32;
                    let mut nc = c;
                    if !outside {
                        nc[axis] = n as usize;
                    }
                    let rgb = if outside {
                        skin(part, c, dims, k)
                    } else if hollow(nc) {
                        // Plus noir au fond de la gorge.
                        MOUTH[(z * 8 / dims[2].max(1)).min(4) / 2]
                    } else if flesh.gone(part, dims, nc) {
                        crate::flesh::jelly(c, dims)
                    } else {
                        continue;
                    };
                    let mut normal = Vec3::ZERO;
                    normal[axis] = sign as f32;
                    let shade = crate::player_model::face_shade(
                        xf.transform_vector3(normal).normalize_or_zero(),
                    );
                    let g = 0.97 + (hash(part * 7 + k, x, y, z) >> 24) as f32 / 255.0 * 0.06;
                    let color = srgb(rgb).map(|v| v * shade * g);
                    let mut o = lo + Vec3::new(x as f32, y as f32, z as f32) * step;
                    if sign > 0 {
                        o[axis] += step[axis];
                    }
                    let (a1, a2) = ((axis + 1) % 3, (axis + 2) % 3);
                    let (mut e1, mut e2) = (Vec3::ZERO, Vec3::ZERO);
                    e1[a1] = step[a1];
                    e2[a2] = step[a2];
                    let p = |s: f32, t: f32| xf.transform_point3(o + e1 * s + e2 * t);
                    let q = [p(0.0, 0.0), p(1.0, 0.0), p(1.0, 1.0), p(0.0, 1.0)];
                    for v in [q[0], q[1], q[2], q[2], q[3], q[0]] {
                        out.push(EntityVertex {
                            pos: v.to_array(),
                            color,
                            uv: [0.0; 2],
                            layer: -1,
                            light: lit,
                        });
                    }
                }
            }
        }
    }
}

/// Rayon contre un pavé `[lo, hi]` : la distance d'entrée, s'il le touche devant soi.
fn ray_box(o: Vec3, d: Vec3, lo: Vec3, hi: Vec3) -> Option<f32> {
    let (mut t0, mut t1) = (0.0f32, f32::MAX);
    for a in 0..3 {
        if d[a].abs() < 1e-9 {
            if o[a] < lo[a] || o[a] > hi[a] {
                return None;
            }
            continue;
        }
        let (x, y) = ((lo[a] - o[a]) / d[a], (hi[a] - o[a]) / d[a]);
        t0 = t0.max(x.min(y));
        t1 = t1.min(x.max(y));
    }
    (t0 <= t1).then_some(t0)
}

/// Une boîte debout sur `pos` (pieds), haute de `h`, ne touche aucun bloc solide.
fn fits(world: &World, pos: Vec3, h: f32) -> bool {
    let (min, max) = (
        pos - Vec3::new(HALF, 0.0, HALF),
        pos + Vec3::new(HALF, h, HALF),
    );
    let (lo, hi) = (min.floor().as_ivec3(), (max - Vec3::splat(1e-4)).floor().as_ivec3());
    for y in lo.y..=hi.y {
        for z in lo.z..=hi.z {
            for x in lo.x..=hi.x {
                for (a, b) in crate::player::collision_boxes(world, x, y, z) {
                    if (0..3).all(|k| min[k] < b[k] - 1e-4 && max[k] > a[k] + 1e-4) {
                        return false;
                    }
                }
            }
        }
    }
    true
}

/// Sa proie : le joueur (pieds, yeux).
#[derive(Clone, Copy, Debug)]
pub struct Prey {
    pub feet: Vec3,
    pub eye: Vec3,
}

/// Ce qui vient de se passer, pour la partie (sons, blessures).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Event {
    /// Il grogne ou ricane, là.
    Voice(Vec3),
    /// Sa bouche vient de se refermer sur nous (`low` : en rampant, sur nos jambes).
    Bites { low: bool, at: Vec3, seed: u32 },
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Mode {
    /// Sa ronde : il marche, s'arrête, tourne.
    Wander,
    /// Il nous a vus : il vient.
    Chase,
}

/// Un Évidé.
#[derive(Clone, Debug)]
pub struct Evide {
    /// Ses pieds (en rampant : le milieu de sa boîte, à l'avant du corps).
    pub pos: Vec3,
    pub vel: Vec3,
    pub yaw: f32,
    pub anim: Anim,
    /// Ce qu'on lui a mangé (ne revient pas).
    pub flesh: Flesh,
    on_ground: bool,
    crawling: bool,
    /// Depuis combien de temps il a la place de se relever (s).
    rise: f32,
    mode: Mode,
    /// Où il nous a vus pour la dernière fois, et depuis quand il ne nous voit plus (s).
    known: Option<Vec3>,
    unseen: f32,
    /// Ronde : cap, temps avant d'en changer, à l'arrêt.
    heading: f32,
    turn_in: f32,
    pausing: bool,
    /// Bloqué contre un mur depuis (s) ; détour en cours (s).
    blocked: f32,
    detour: f32,
    /// Morsure en cours : temps dans le geste (s).
    feed: f32,
    feeding: bool,
    /// Il sursaute quand on le mord (s).
    flinch: f32,
    /// Sa fin : depuis quand il s'affaisse (s).
    dying: Option<f32>,
    /// Regard : lacet par rapport au corps (positif vers sa droite), tangage.
    look: (f32, f32),
    /// Tics de la tête : roulis visé, temps avant le prochain.
    tic: (f32, f32),
    /// Temps avant sa prochaine voix (s).
    voice: f32,
    age: f32,
    rng: u32,
    /// Essais : figé dans cette posture (aucun mouvement, aucune pensée).
    frozen: bool,
}

impl Evide {
    pub fn new(pos: Vec3, yaw: f32, seed: u32) -> Self {
        let mut e = Self {
            pos,
            vel: Vec3::ZERO,
            yaw,
            anim: Anim {
                hunch: 0.18,
                ..Anim::default()
            },
            flesh: Flesh::default(),
            on_ground: false,
            crawling: false,
            rise: 0.0,
            mode: Mode::Wander,
            known: None,
            unseen: 0.0,
            heading: yaw,
            turn_in: 0.0,
            pausing: false,
            blocked: 0.0,
            detour: 0.0,
            feed: 0.0,
            feeding: false,
            flinch: 0.0,
            dying: None,
            look: (0.0, -0.12),
            tic: (0.0, 1.0),
            voice: 0.0,
            age: 0.0,
            rng: seed.wrapping_mul(0x9e37_79b9) | 1,
            frozen: false,
        };
        e.voice = 2.0 + 4.0 * e.unit();
        e
    }

    /// Essais : un Évidé figé dans une posture (`debout`, `court`, `rampe`, `mord`, `mange`).
    pub fn posed(pos: Vec3, yaw: f32, pose: &str) -> Self {
        let mut e = Self::new(pos, yaw, 7);
        e.frozen = true;
        let a = &mut e.anim;
        match pose {
            "court" => {
                (a.walk, a.swing, a.hunch, a.arms, a.mouth) = (1.2, 0.9, 0.38, 1.25, 0.35);
            }
            "rampe" => {
                (a.walk, a.swing, a.crawl, a.mouth) = (1.2, 1.0, 1.0, 0.35);
                e.crawling = true;
            }
            "mord" => {
                (a.hunch, a.arms, a.grab, a.mouth, a.head_pitch) = (1.5, 0.35, 1.0, 1.0, -0.2);
            }
            "mange" => {
                let grids = grids();
                for k in 0..9u32 {
                    e.flesh
                        .bite_somewhere(k * 53 + 11, OUR_BITE, &[0, 1, 2, 3, 4, 5, 6], &grids);
                }
            }
            _ => {}
        }
        e
    }

    fn unit(&mut self) -> f32 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 17;
        self.rng ^= self.rng << 5;
        (self.rng >> 8) as f32 / 16_777_216.0
    }

    pub fn is_dead(&self) -> bool {
        self.dying.is_some_and(|t| t >= DIE_S)
    }

    pub fn is_dying(&self) -> bool {
        self.dying.is_some()
    }

    /// Son cou (à peu près) : d'où il regarde, vers où il se penche.
    fn neck(&self) -> Vec3 {
        let r = crate::player_model::rig();
        let c = self.anim.crawl;
        let pitch = lerp(self.anim.hunch, FRAC_PI_2, c);
        let hip = lerp(r.leg * TALL * r.px, CRAWL_HIP, c);
        let len = r.body_h * TALL * r.px;
        self.pos
            + Vec3::Y * (hip + len * pitch.cos())
            + forward(self.yaw) * (len * pitch.sin() - c * CRAWL_BACK)
    }

    /// Ses yeux (il n'en a pas : là où ils seraient).
    pub fn eye(&self) -> Vec3 {
        self.neck() + Vec3::Y * 0.28
    }

    /// Voit-il ce point ? Sur son « écran » (le champ du jeu, tourné comme sa tête), à moins
    /// de `SEE` blocs, sans bloc plein entre les deux.
    pub fn sees(&self, world: &World, target: Vec3) -> bool {
        let eye = self.eye();
        let to = target - eye;
        let dist = to.length();
        if dist > SEE {
            return false;
        }
        // (À moins de `FEEL` blocs il nous sent tout autour de lui : penché sur nous pour
        // mordre, sa tête nous dépasse et on n'est plus sur son « écran ».)
        if dist > FEEL {
            let f = forward(self.yaw + self.look.0);
            let right = Vec3::new(-f.z, 0.0, f.x);
            let (depth, side) = (to.dot(f), to.dot(right));
            if depth <= 0.05 || side.abs() > depth * VIEW_TAN_X {
                return false;
            }
            let up = to.y.atan2(depth) - self.look.1;
            if up.tan().abs() > VIEW_TAN_Y || up.abs() > FRAC_PI_2 {
                return false;
            }
        }
        minelib_world::raycast::raycast_with(world, eye.into(), (to / dist.max(1e-3)).into(), dist, |id| {
            id.def().full
        })
        .is_none()
    }

    /// Déplace sa boîte de `delta`, axe par axe, jusqu'au contact : (un mur l'a arrêté, le sol
    /// l'a arrêté, le plafond l'a arrêté).
    fn slide(&mut self, world: &World, delta: Vec3, h: f32) -> (bool, bool, bool) {
        let mut hit = [false; 3];
        for axis in [0usize, 2, 1] {
            let d = delta[axis];
            if d == 0.0 {
                continue;
            }
            let mut p = self.pos;
            p[axis] += d;
            if fits(world, p, h) {
                self.pos = p;
                continue;
            }
            hit[axis] = true;
            let (mut ok, mut bad) = (0.0f32, d);
            for _ in 0..6 {
                let mid = (ok + bad) / 2.0;
                let mut p = self.pos;
                p[axis] += mid;
                if fits(world, p, h) {
                    ok = mid;
                } else {
                    bad = mid;
                }
            }
            self.pos[axis] += ok;
        }
        (
            hit[0] || hit[2],
            hit[1] && delta.y < 0.0,
            hit[1] && delta.y > 0.0,
        )
    }

    /// Un pas de sa vie : il regarde, décide, se déplace, s'anime.
    pub fn update(&mut self, world: &World, dt: f32, prey: Option<Prey>) -> Vec<Event> {
        let mut events = Vec::new();
        if let Some(t) = &mut self.dying {
            *t += dt;
            self.anim.sink = (*t / DIE_S).min(1.0);
            self.anim.mouth = approach(self.anim.mouth, 1.0, dt * 6.0);
            return events;
        }
        let here = ChunkPos::containing(self.pos.x.floor() as i32, self.pos.z.floor() as i32);
        if self.frozen || dt <= 0.0 || !world.contains(here) {
            return events;
        }
        let [walk, run, sneak, gravity, jump] = crate::player::GAIT;
        self.age += dt;
        self.flinch = (self.flinch - dt).max(0.0);
        let grids = grids();
        let legless = self.flesh.eaten_of(LEG_L, grids[LEG_L]) > 0.6
            && self.flesh.eaten_of(LEG_R, grids[LEG_R]) > 0.6;

        // Ce qu'il voit.
        let seen = prey
            .is_some_and(|p| self.sees(world, p.eye) || self.sees(world, p.feet + Vec3::Y * 0.5));
        if let (Some(p), true) = (prey, seen) {
            if self.mode == Mode::Wander {
                // Il vient de nous voir : il le dit.
                self.voice = self.voice.min(0.15);
            }
            self.mode = Mode::Chase;
            self.known = Some(p.feet);
            self.unseen = 0.0;
        } else {
            self.unseen += dt;
            if self.mode == Mode::Chase && self.unseen > GIVE_UP_S {
                self.mode = Mode::Wander;
                self.known = None;
                self.turn_in = 0.0;
            }
        }
        self.voice -= dt;
        if self.voice <= 0.0 {
            self.voice = 6.0 + 8.0 * self.unit();
            events.push(Event::Voice(self.eye()));
        }

        // Ce qu'il veut : un cap, une allure.
        let mut wish = self.yaw;
        let mut speed = 0.0;
        let mut feeding = false;
        match (self.mode, self.known) {
            (Mode::Chase, Some(target)) => {
                let to = target - self.pos;
                let flat = Vec3::new(to.x, 0.0, to.z);
                let d = flat.length();
                if d > 0.05 {
                    wish = flat.x.atan2(-flat.z);
                }
                let hold = if self.crawling { CRAWL_HOLD } else { HOLD };
                let slack = if self.feeding { 0.6 } else { 0.0 };
                if seen && d <= hold + slack && (-2.0..=2.6).contains(&to.y) {
                    feeding = true;
                } else if self.detour > 0.0 {
                    self.detour -= dt;
                    wish = self.heading;
                    speed = run;
                } else if seen || d > 0.7 {
                    speed = run;
                } else {
                    // Arrivé là où il nous a vus : il continue un peu, il cherche.
                    wish = self.yaw + 0.6 * (self.age * 0.7).sin();
                    speed = walk;
                }
            }
            _ => {
                self.turn_in -= dt;
                if self.turn_in <= 0.0 {
                    // « Il tourne assez régulièrement. »
                    self.pausing = !self.pausing && self.unit() < 0.4;
                    self.turn_in = if self.pausing {
                        1.0 + 1.5 * self.unit()
                    } else {
                        2.0 + 3.5 * self.unit()
                    };
                    self.heading = self.yaw + (self.unit() * 2.0 - 1.0) * 2.1;
                }
                wish = self.heading;
                speed = if self.pausing { 0.0 } else { walk };
            }
        }
        if !feeding {
            self.feed = 0.0;
        }
        self.feeding = feeding;

        // Pris dans un bloc (sorti d'un œuf tout contre un mur, bloc posé sur lui) : il s'en
        // dégage, par le côté le plus proche ou par le dessus.
        if !fits(world, self.pos, CRAWL_H) {
            let free = [0.35f32, 0.7, 1.05].into_iter().find_map(|r| {
                [
                    Vec3::X,
                    Vec3::NEG_X,
                    Vec3::Z,
                    Vec3::NEG_Z,
                    Vec3::new(1.0, 0.0, 1.0),
                    Vec3::new(1.0, 0.0, -1.0),
                    Vec3::new(-1.0, 0.0, 1.0),
                    Vec3::new(-1.0, 0.0, -1.0),
                    Vec3::Y,
                ]
                .into_iter()
                .map(|d| self.pos + d * r)
                .find(|p| fits(world, *p, CRAWL_H))
            });
            if let Some(p) = free {
                self.pos = p;
            }
        }
        // Sa posture : il rampe sous un plafond trop bas (ou sans jambes), se relève dès
        // qu'il en a la place.
        let dir = forward(self.yaw);
        if legless || (!self.crawling && !fits(world, self.pos, HEIGHT)) {
            self.crawling = true;
            self.rise = 0.0;
        } else if self.crawling {
            let room = fits(world, self.pos, HEIGHT) && fits(world, self.pos + dir * 0.7, HEIGHT);
            self.rise = if room { self.rise + dt } else { 0.0 };
            if self.rise > 0.5 {
                self.crawling = false;
            }
        }
        if self.crawling && speed > 0.0 {
            speed = sneak;
        }
        let h = if self.crawling { CRAWL_H } else { HEIGHT };

        // Il se tourne, puis avance où il regarde.
        let gap = turn(wish, self.yaw);
        let rate = if self.mode == Mode::Chase { 6.0 } else { 2.5 };
        self.yaw += gap.clamp(-rate * dt, rate * dt);
        let dir = forward(self.yaw);
        let go = speed * gap.cos().max(0.0);
        self.vel.x = dir.x * go;
        self.vel.z = dir.z * go;
        self.vel.y = (self.vel.y - gravity * dt).max(-60.0);
        let before = self.pos;
        let steps = (dt / 0.02).ceil().max(1.0);
        let (mut wall, mut floor, mut ceiling) = (false, false, false);
        for _ in 0..steps as usize {
            let (w, f, c) = self.slide(world, self.vel * (dt / steps), h);
            wall |= w;
            floor |= f;
            ceiling |= c;
        }
        if floor {
            self.on_ground = true;
            self.vel.y = 0.0;
        } else if (self.pos.y - before.y).abs() > 1e-4 {
            self.on_ground = false;
        }
        if ceiling {
            self.vel.y = 0.0;
        }
        if wall && go > 0.1 {
            self.blocked += dt;
            if self.on_ground {
                let up = self.pos + Vec3::Y * 1.05;
                let ahead = dir * 0.5;
                if fits(world, up, h) && fits(world, up + ahead, h) {
                    // Une marche d'un bloc : il saute, comme un joueur.
                    self.vel.y = jump;
                    self.on_ground = false;
                } else if !self.crawling
                    && (fits(world, self.pos + ahead * 1.3, CRAWL_H)
                        || (fits(world, up, CRAWL_H) && fits(world, up + ahead, CRAWL_H)))
                {
                    // Trop bas pour lui debout : il se met à plat ventre.
                    self.crawling = true;
                    self.rise = 0.0;
                } else if self.blocked > 0.7 {
                    // Rien à faire ici : il longe l'obstacle un moment.
                    let side = if self.unit() < 0.5 { -1.0 } else { 1.0 };
                    self.heading = self.yaw + side * 1.4;
                    self.detour = 0.9;
                    self.turn_in = self.turn_in.max(0.9);
                    self.blocked = 0.0;
                }
            }
        } else {
            self.blocked = (self.blocked - dt).max(0.0);
        }
        let moved = Vec3::new(self.pos.x - before.x, 0.0, self.pos.z - before.z).length() / dt;

        // Sa morsure : il attrape, se penche, ouvre grand, referme.
        let mut mouth = match self.mode {
            Mode::Chase => 0.35,
            Mode::Wander => 0.0,
        };
        let mut lean = match self.mode {
            Mode::Chase => 0.38,
            Mode::Wander => 0.18 + 0.03 * (self.age * 0.8).sin(),
        };
        let mut arms = match self.mode {
            Mode::Chase => 1.25,
            Mode::Wander => 0.06 + 0.05 * (self.age * 1.1).sin(),
        };
        if let (true, Some(p)) = (feeding, prey) {
            let was = self.feed % BITE_S;
            self.feed += dt;
            let t = self.feed % BITE_S;
            if was < BITE_AT && t >= BITE_AT && self.feed > BITE_S * 0.5 {
                let seed = self.rng;
                self.unit();
                events.push(Event::Bites {
                    low: self.crawling,
                    at: self.eye(),
                    seed,
                });
            }
            mouth = if t < 0.45 {
                0.3
            } else if t < BITE_AT - 0.04 {
                0.3 + 0.7 * ((t - 0.45) / 0.45).min(1.0)
            } else if t < 1.3 {
                -0.5
            } else {
                -0.5 + 0.8 * (t - 1.3) / (BITE_S - 1.3)
            };
            // Penché jusqu'à nous : son cou arrive au-dessus de notre tête.
            let to = p.eye - self.pos;
            let d = Vec3::new(to.x, 0.0, to.z).length();
            let hip = crate::player_model::rig().leg * TALL * crate::player_model::rig().px;
            lean = (d - 0.05).atan2(to.y + 0.4 - hip).clamp(0.8, 1.75);
            // Ses bras descendent vers nous.
            let chest = p.feet + Vec3::Y * 1.1 - self.neck();
            arms = (FRAC_PI_2 + chest.y.atan2(d.max(0.2))).clamp(0.2, 1.9) - lean * 0.15;
        }
        if self.flinch > 0.0 {
            mouth = 1.0;
        }

        // Son regard : sur nous s'il nous voit, sinon il balaie.
        let (mut look_yaw, mut look_pitch) = if self.pausing {
            (0.9 * (self.age * 0.9).sin(), -0.1)
        } else {
            (0.25 * (self.age * 0.6).sin(), -0.12)
        };
        if let (Some(p), true) = (prey, seen) {
            let to = p.eye - self.eye();
            let flat = Vec3::new(to.x, 0.0, to.z).length();
            look_yaw = turn(to.x.atan2(-to.z), self.yaw).clamp(-1.3, 1.3);
            look_pitch = to.y.atan2(flat.max(0.05)).clamp(-1.45, 0.9);
        }
        self.look.0 = approach(self.look.0, look_yaw, dt * 8.0);
        self.look.1 = approach(self.look.1, look_pitch, dt * 8.0);
        // Ses tics : la tête qui se couche d'un coup sur l'épaule, puis revient.
        self.tic.1 -= dt;
        if self.tic.1 <= 0.0 {
            let calm = if self.mode == Mode::Chase { 0.5 } else { 1.0 };
            self.tic = if self.tic.0 == 0.0 {
                ((self.unit() - 0.5) * 1.1, 0.3 + 0.9 * self.unit())
            } else {
                (0.0, (1.2 + 2.8 * self.unit()) * calm)
            };
        }
        let shiver = if seen { 0.03 * (self.age * 37.0).sin() } else { 0.0 };

        let a = &mut self.anim;
        a.crawl = approach(a.crawl, if self.crawling { 1.0 } else { 0.0 }, dt / 0.45);
        a.hunch = approach(a.hunch, lean, dt * 3.0);
        a.arms = approach(a.arms, arms, dt * 5.0);
        let grab = if feeding && !self.crawling { 1.0 } else { 0.0 };
        a.grab = approach(a.grab, grab, dt * 4.0);
        a.mouth = approach(a.mouth, mouth, dt * if mouth < a.mouth { 22.0 } else { 5.0 });
        a.head_yaw = self.look.0;
        a.head_pitch = self.look.1;
        a.head_roll = approach(a.head_roll, self.tic.0, dt * 14.0) + shiver * dt * 30.0;
        a.head_roll = a.head_roll.clamp(-0.7, 0.7);
        a.walk += moved * dt * if self.crawling { 4.0 } else { 1.6 };
        let swing = if self.crawling {
            (moved / (sneak * 0.5)).min(1.0)
        } else {
            (moved / walk).min(1.0) * 0.9
        };
        a.swing = approach(a.swing, swing, dt * 4.0);
        events
    }

    /// Son maillage, éclairé par `light`.
    pub fn mesh(&self, light: [f32; 3]) -> Vec<EntityVertex> {
        let dims = grids();
        let mut out = Vec::with_capacity(20_000);
        for (i, (xf, lo, hi)) in parts(self.pos, self.yaw, &self.anim).into_iter().enumerate() {
            emit_part(&mut out, xf, lo, hi, i, dims[i], light, &self.flesh, self.anim.mouth);
        }
        out
    }

    /// Les pavés qui font son ombre.
    pub fn shadow_boxes(&self) -> Vec<ShadowBox> {
        let dims = grids();
        parts(self.pos, self.yaw, &self.anim)
            .into_iter()
            .enumerate()
            .filter(|(i, _)| self.flesh.eaten_of(*i, dims[*i]) < 0.9)
            .map(|(_, (xf, lo, hi))| ShadowBox::oriented(xf, Vec3::from(lo), Vec3::from(hi)))
            .collect()
    }

    /// Le pavé et le cube visés par ce rayon, à au plus `reach` blocs : (distance, pavé, cube).
    pub fn aimed(&self, eye: Vec3, dir: Vec3, reach: f32) -> Option<(f32, usize, [usize; 3])> {
        if self.dying.is_some() {
            return None;
        }
        let dims = grids();
        let mut best: Option<(f32, usize, [usize; 3])> = None;
        for (i, (xf, lo, hi)) in parts(self.pos, self.yaw, &self.anim).into_iter().enumerate() {
            if self.flesh.eaten_of(i, dims[i]) >= 1.0 {
                continue;
            }
            let inv = xf.inverse();
            let (o, d) = (inv.transform_point3(eye), inv.transform_vector3(dir));
            let (lo, hi) = (Vec3::from(lo), Vec3::from(hi));
            let Some(t) = ray_box(o, d, lo, hi) else {
                continue;
            };
            if t > reach || best.is_some_and(|b| b.0 <= t) {
                continue;
            }
            let p = (o + d * t - lo) / (hi - lo);
            let cell = [0, 1, 2].map(|a| {
                ((p[a] * dims[i][a] as f32).floor().max(0.0) as usize).min(dims[i][a] - 1)
            });
            best = Some((t, i, cell));
        }
        best
    }

    /// On lui mange un morceau, là (pavé, cube) : « on mange toujours par les bords », le
    /// morceau ne revient pas. Rend ce qu'on a mangé, et s'il en meurt.
    pub fn eaten_at(&mut self, part: usize, cell: [usize; 3]) -> Option<bool> {
        if self.dying.is_some() {
            return None;
        }
        let dims = grids();
        self.flesh.bite_at(part, cell, OUR_BITE, &dims)?;
        self.flinch = 0.25;
        let dead = self.flesh.eaten(&dims) >= EATEN_DEAD
            || self.flesh.eaten_of(HEAD, dims[HEAD]) >= HEAD_DEAD;
        if dead {
            self.dying = Some(0.0);
        }
        Some(dead)
    }
}

/// Les Évidés du monde.
#[derive(Default)]
pub struct Horde {
    pub list: Vec<Evide>,
    /// Combien sont venus tout seuls (les autres sortent d'un œuf).
    natural: usize,
    rng: u32,
    /// Temps avant le prochain tirage d'une apparition (s).
    wait: f32,
}

/// Secondes entre deux tirages d'une apparition naturelle.
const SPAWN_TICK: f32 = 5.0;

impl Horde {
    fn unit(&mut self) -> f32 {
        if self.rng == 0 {
            self.rng = 0x2545_f491;
        }
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 17;
        self.rng ^= self.rng << 5;
        (self.rng >> 8) as f32 / 16_777_216.0
    }

    /// Un Évidé de plus, là (sorti d'un œuf). Faux : il y en a déjà trop.
    pub fn spawn(&mut self, pos: Vec3, yaw: f32) -> bool {
        if self.list.len() >= MAX {
            return false;
        }
        let seed = (self.unit() * 16_777_216.0) as u32;
        self.list.push(Evide::new(pos, yaw, seed));
        true
    }

    /// Un endroit où en faire apparaître un autour de `center` : un sol, de quoi tenir au
    /// moins à plat ventre, dans un chunk chargé.
    fn natural_spot(&mut self, world: &World, center: Vec3) -> Option<Vec3> {
        for _ in 0..12 {
            let (angle, far) = (self.unit() * TAU, lerp(SPAWN_NEAR, SPAWN_FAR, self.unit()));
            let (x, z) = (
                (center.x + angle.cos() * far).floor() as i32,
                (center.z + angle.sin() * far).floor() as i32,
            );
            if !world.contains(ChunkPos::containing(x, z)) {
                continue;
            }
            let y0 = center.y.floor() as i32;
            for y in ((y0 - 40)..=(y0 + 40)).rev() {
                let at = Vec3::new(x as f32 + 0.5, y as f32 + 1.0, z as f32 + 0.5);
                if world.get(x, y, z).def().full && fits(world, at, CRAWL_H) {
                    return Some(at);
                }
            }
        }
        None
    }

    /// Un pas pour tous. `night` : il fait nuit (ils apparaissent tout seuls, rarement).
    pub fn update(&mut self, world: &World, dt: f32, prey: Option<Prey>, night: bool) -> Vec<Event> {
        let mut events = Vec::new();
        for e in &mut self.list {
            events.extend(e.update(world, dt, prey));
        }
        let before = self.list.len();
        self.list.retain(|e| {
            !e.is_dead()
                && e.pos.y > -80.0
                && prey.is_none_or(|p| e.frozen || p.feet.distance(e.pos) < DESPAWN)
        });
        self.natural = self.natural.saturating_sub(before - self.list.len());
        if let (true, Some(p)) = (night, prey) {
            self.wait -= dt;
            if self.wait <= 0.0 {
                self.wait = SPAWN_TICK;
                if self.natural < NATURAL_MAX
                    && self.list.len() < MAX
                    && self.unit() < SPAWN_TICK / SPAWN_EVERY_S
                {
                    if let Some(at) = self.natural_spot(world, p.feet) {
                        let yaw = self.unit() * TAU;
                        if self.spawn(at, yaw) {
                            self.natural += 1;
                        }
                    }
                }
            }
        }
        events
    }

    /// L'Évidé visé : (lequel, distance, pavé, cube).
    pub fn aimed(&self, eye: Vec3, dir: Vec3, reach: f32) -> Option<(usize, f32, usize, [usize; 3])> {
        self.list
            .iter()
            .enumerate()
            .filter_map(|(i, e)| e.aimed(eye, dir, reach).map(|(t, p, c)| (i, t, p, c)))
            .min_by(|a, b| a.1.total_cmp(&b.1))
    }
}

/// L'œuf d'apparition : image 16 × 16 (`W` blanc, `g` plaque grise, `s` ombre du bord, `D`
/// le creux sombre de la bouche).
const EGG: [&str; 16] = [
    "................",
    "......WWWW......",
    ".....WWWWWs.....",
    "....WWgWWWWs....",
    "....WWWWWgWs....",
    "...WWWWWWWWWs...",
    "...WgWWWWWWWs...",
    "...WWWDDDDWWs...",
    "..WWWDDDDDDWgs..",
    "..WWWDDDDDDWWs..",
    "..WWWWDDDDWWWs..",
    "..WgWWWWWWWWWs..",
    "...WWWWWWgWWs...",
    "...WWWWWWWWss...",
    "....sWWWWWss....",
    "......ssss......",
];

/// Couleur de l'œuf au sol, en main vu de loin (sRGB).
pub const EGG_RGB: [u8; 3] = [236, 234, 227];

/// Pixels pleins de l'œuf : (colonne, ligne, couleur linéaire).
pub fn egg_pixels() -> impl Iterator<Item = (usize, usize, [f32; 3])> {
    EGG.iter().enumerate().flat_map(|(y, row)| {
        row.chars().enumerate().filter_map(move |(x, c)| {
            let rgb = match c {
                'W' => WHITE,
                'g' => 0xcfcdc5,
                's' => 0xc4c2ba,
                'D' => MOUTH[1],
                _ => return None,
            };
            Some((x, y, srgb(rgb)))
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use minelib_world::BlockId;

    /// Un sol plat de pierre en y = 10 (on marche en y = 11), de 96 × 96 blocs.
    fn flat() -> World {
        let mut w = World::new(0);
        for cz in -3..3 {
            for cx in -3..3 {
                w.insert_empty(ChunkPos { x: cx, z: cz });
            }
        }
        for z in -48..48 {
            for x in -48..48 {
                w.set(x, 10, z, BlockId::STONE);
            }
        }
        w
    }

    fn prey(at: Vec3) -> Prey {
        Prey {
            feet: at,
            eye: at + Vec3::Y * 1.62,
        }
    }

    fn run(e: &mut Evide, w: &World, secs: f32, p: Option<Prey>) -> Vec<Event> {
        let mut all = Vec::new();
        for _ in 0..(secs * 60.0) as usize {
            all.extend(e.update(w, 1.0 / 60.0, p));
        }
        all
    }

    /// « La même largeur, 3,5 blocs de haut » : debout il fait 3,5 blocs, large comme le
    /// joueur ; à plat ventre, « 3,5 de long, 1 de large et de haut ».
    #[test]
    fn it_is_three_and_a_half_blocks_tall_and_crawls_one_block_high() {
        let size = |a: &Anim| {
            let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
            for (xf, a, b) in parts(Vec3::ZERO, 0.0, a) {
                for k in 0..8 {
                    let c = Vec3::new(
                        if k & 1 == 0 { a[0] } else { b[0] },
                        if k & 2 == 0 { a[1] } else { b[1] },
                        if k & 4 == 0 { a[2] } else { b[2] },
                    );
                    let p = xf.transform_point3(c);
                    (lo, hi) = (lo.min(p), hi.max(p));
                }
            }
            (lo, hi)
        };
        let (lo, hi) = size(&Anim::default());
        assert!((hi.y - HEIGHT).abs() < 0.02 && lo.y.abs() < 0.02, "{lo} {hi}");
        // Large comme le joueur (bras compris : moins d'un bloc), pas plus épais.
        let (plo, phi) = crate::player_model::rig().boxes[CHEST];
        let px = crate::player_model::rig().px;
        assert!((hi.x - lo.x) < 1.0 && (hi.x - lo.x) > (phi[0] - plo[0]) * px, "{lo} {hi}");
        let lying = Anim {
            crawl: 1.0,
            ..Anim::default()
        };
        let (lo, hi) = size(&lying);
        // (3,5 de long des pieds à la tête, à peu près ; ses bras tendus devant en plus.)
        assert!((3.2..=4.1).contains(&(hi.z - lo.z)), "long de {}", hi.z - lo.z);
        assert!(hi.y <= 1.0 && lo.y > -0.05, "haut de {} à {}", lo.y, hi.y);
        assert!(hi.x - lo.x < 1.0, "large de {}", hi.x - lo.x);
    }

    /// Sa bouche : un creux toujours là, qui s'agrandit beaucoup et se referme en fente ; sa
    /// peau : presque toute blanche, deux contours d'yeux sans rien dedans.
    #[test]
    fn its_mouth_is_a_hollow_and_its_eyes_are_only_outlines() {
        let d = grids()[HEAD];
        let count = |open: f32| {
            let mut n = 0;
            for z in 0..d[2] {
                for y in 0..d[1] {
                    for x in 0..d[0] {
                        n += mouth([x, y, z], d, open) as usize;
                    }
                }
            }
            n
        };
        let (shut, rest, wide) = (count(-0.5), count(0.0), count(1.0));
        assert!(shut > 0 && shut < rest && rest * 3 < wide, "{shut} {rest} {wide}");
        // Jamais jusqu'au sommet du crâne ni jusqu'aux yeux.
        for x in 0..d[0] {
            for y in 10..d[1] {
                assert!(!mouth([x, y, 0], d, 1.0));
            }
        }
        let mut seen = std::collections::HashMap::new();
        for y in 0..d[1] {
            for x in 0..d[0] {
                *seen.entry(skin(HEAD, [x, y, 0], d, 0)).or_insert(0usize) += 1;
            }
        }
        assert_eq!(seen.get(&OUTLINE), Some(&20), "{seen:?}");
        assert_eq!(seen.get(&SOCKET), Some(&4), "{seen:?}");
        // Blanc avant tout, sur tout le corps.
        let grids = grids();
        let (mut white, mut all) = (0usize, 0usize);
        for (p, d) in grids.iter().enumerate() {
            for y in 0..d[1] {
                for x in 0..d[0] {
                    all += 1;
                    white += (skin(p, [x, y, 0], *d, 0) == WHITE) as usize;
                }
            }
        }
        assert!(white * 10 > all * 5 && white < all, "{white} sur {all}");
    }

    /// Il nous voit devant lui jusqu'à 50 blocs, pas derrière lui, pas à travers un mur
    /// plein ; il vient alors à l'allure d'un joueur qui court, et marche sinon.
    #[test]
    fn it_sees_what_is_on_its_screen_and_runs_at_us() {
        let mut w = flat();
        let at = Vec3::new(0.5, 11.0, 0.5);
        let e = Evide::new(at, 0.0, 3);
        // Il regarde vers −z.
        assert!(e.sees(&w, Vec3::new(0.5, 12.6, -30.0)));
        assert!(!e.sees(&w, Vec3::new(0.5, 12.6, -60.0)), "plus de 50 blocs");
        assert!(!e.sees(&w, Vec3::new(0.5, 12.6, 30.0)), "derrière lui");
        for y in 11..16 {
            for x in -3..4 {
                w.set(x, y, -8, BlockId::STONE);
            }
        }
        assert!(!e.sees(&w, Vec3::new(0.5, 12.6, -30.0)), "à travers un mur");
        // Sans proie : il marche, s'arrête, tourne.
        let w = flat();
        let mut e = Evide::new(at, 0.0, 3);
        let yaw = e.yaw;
        run(&mut e, &w, 12.0, None);
        let [walk, run_speed, ..] = crate::player::GAIT;
        assert!(turn(e.yaw, yaw).abs() > 0.05 || e.pos.distance(at) > 1.0);
        assert!(e.pos.distance(at) <= walk * 12.0 + 0.1);
        // Une proie devant lui, à 30 blocs : il court vers elle.
        let mut e = Evide::new(at, 0.0, 3);
        let p = prey(Vec3::new(0.5, 11.0, -30.0));
        run(&mut e, &w, 0.2, Some(p));
        let from = e.pos;
        run(&mut e, &w, 2.0, Some(p));
        let speed = (e.pos - from).length() / 2.0;
        assert!((speed - run_speed).abs() < 0.4, "il court à {speed}");
        assert!((e.pos.y - 11.0).abs() < 0.01, "il reste au sol : {}", e.pos.y);
    }

    /// Sous un plafond trop bas il rampe, à l'allure du sneak, et se relève dehors ; une
    /// marche d'un bloc, il la saute.
    #[test]
    fn it_crawls_under_low_ceilings_and_jumps_one_block_steps() {
        let mut w = flat();
        // Un tunnel d'un bloc de haut, de z = −6 à −14, avec des murs autour.
        for z in -14..=-6 {
            for x in -2..=2 {
                for y in 11..16 {
                    if !(x == 0 && y == 11) {
                        w.set(x, y, z, BlockId::STONE);
                    }
                }
            }
        }
        let mut e = Evide::new(Vec3::new(0.5, 11.0, -0.5), 0.0, 5);
        let p = prey(Vec3::new(0.5, 11.0, -22.0));
        // (Il nous a vus avant qu'on passe derrière le mur.)
        e.mode = Mode::Chase;
        e.known = Some(p.feet);
        let [_, _, sneak, ..] = crate::player::GAIT;
        let mut crawled = false;
        let mut slow = true;
        for _ in 0..(40.0 * 60.0) as usize {
            let before = e.pos;
            e.update(&w, 1.0 / 60.0, Some(p));
            if e.pos.z < -7.0 && e.pos.z > -13.0 {
                crawled |= e.crawling && e.anim.crawl > 0.99;
                slow &= (e.pos - before).length() * 60.0 <= sneak + 0.05;
            }
            if e.feeding {
                break;
            }
        }
        assert!(crawled && slow, "rampé {crawled}, lentement {slow}");
        assert!(e.pos.z < -19.0 && !e.crawling, "ressorti debout : {} {}", e.pos, e.crawling);
        // Apparu à moitié dans un mur, il s'en dégage et vient quand même.
        let mut w = flat();
        for y in 11..16 {
            for x in -1..=1 {
                w.set(x, y, 1, BlockId::STONE);
            }
        }
        let mut e = Evide::new(Vec3::new(0.5, 11.0, 0.9), 0.0, 5);
        run(&mut e, &w, 3.0, Some(prey(Vec3::new(0.5, 11.0, -12.0))));
        assert!(e.pos.z < -6.0, "{}", e.pos);
        // Une marche d'un bloc sur son chemin.
        let mut w = flat();
        for z in -40..=-5 {
            for x in -3..=3 {
                w.set(x, 11, z, BlockId::STONE);
            }
        }
        let mut e = Evide::new(Vec3::new(0.5, 11.0, -0.5), 0.0, 5);
        run(&mut e, &w, 4.0, Some(prey(Vec3::new(0.5, 12.0, -30.0))));
        assert!(e.pos.y > 11.9 && e.pos.z < -8.0, "{}", e.pos);
    }

    /// Près de nous il s'arrête, se penche, et sa bouche se referme sur nous à intervalles ;
    /// on le mange par morceaux qui ne reviennent pas, et il en meurt.
    #[test]
    fn it_bites_us_and_dies_when_we_eat_it() {
        let w = flat();
        let p = prey(Vec3::new(0.5, 11.0, -1.6));
        let mut e = Evide::new(Vec3::new(0.5, 11.0, 0.5), 0.0, 9);
        let events = run(&mut e, &w, 8.0, Some(p));
        let bites = events
            .iter()
            .filter(|e| matches!(e, Event::Bites { low: false, .. }))
            .count();
        assert!((3..=5).contains(&bites), "{bites} morsures en 8 s");
        assert!(events.iter().any(|e| matches!(e, Event::Voice(_))));
        assert!(e.anim.hunch > 0.8 && e.anim.grab > 0.9, "{:?}", e.anim);
        let d = Vec3::new(e.pos.x - p.feet.x, 0.0, e.pos.z - p.feet.z).length();
        assert!((0.8..=HOLD + 0.1).contains(&d), "à {d} de nous");
        // On le vise et on le mange : chaque bouchée retire quelque chose de plus.
        let eye = p.eye;
        let dims = grids();
        let mut eaten = 0.0;
        let mut dead = false;
        for n in 0..400 {
            // On vise le milieu de chacun de ses pavés, tour à tour.
            let (xf, lo, hi) = parts(e.pos, e.yaw, &e.anim)[n % PARTS];
            let mid = xf.transform_point3((Vec3::from(lo) + Vec3::from(hi)) / 2.0);
            let dir = (mid - eye).normalize();
            let Some((t, part, cell)) = e.aimed(eye, dir, 6.0) else {
                continue;
            };
            assert!((0.0..6.0).contains(&t));
            if let Some(d) = e.eaten_at(part, cell) {
                let now = e.flesh.eaten(&dims);
                assert!(now > eaten, "bouchée {n} : rien de plus");
                eaten = now;
                dead = d;
                if dead {
                    break;
                }
            }
        }
        assert!(dead && eaten >= 0.3, "mangé à {eaten}");
        assert!(e.aimed(eye, Vec3::NEG_Z, 6.0).is_none());
        run(&mut e, &w, 1.0, Some(p));
        assert!(e.is_dead());
        // Rien ne repousse.
        e.flesh.tick(600.0, None, &dims);
        assert_eq!(e.flesh.eaten(&dims), eaten);
    }

    /// Il apparaît tout seul la nuit, rarement (une fois par vingt minutes en moyenne), dans
    /// les 256 blocs autour de nous, jamais le jour ; un œuf en pose un tout de suite.
    #[test]
    fn it_comes_at_night_rarely_and_from_an_egg_at_once() {
        let w = flat();
        let p = prey(Vec3::new(0.5, 11.0, 0.5));
        let mut h = Horde::default();
        for _ in 0..(3600.0 / SPAWN_TICK) as usize {
            h.update(&w, SPAWN_TICK, Some(p), false);
        }
        assert!(h.list.is_empty(), "le jour");
        // Dix heures de nuit, les monstres retirés à mesure : une trentaine d'apparitions.
        let mut came = 0;
        for _ in 0..(36_000.0 / SPAWN_TICK) as usize {
            h.update(&w, SPAWN_TICK, Some(p), true);
            for e in h.list.drain(..) {
                let d = Vec3::new(e.pos.x - 0.5, 0.0, e.pos.z - 0.5).length();
                assert!((SPAWN_NEAR - 1.0..=SPAWN_FAR + 1.0).contains(&d), "à {d}");
                assert!((e.pos.y - 11.0).abs() < 0.01);
                came += 1;
            }
            h.natural = 0;
        }
        assert!((12..=55).contains(&came), "{came} en dix heures de nuit");
        assert!(h.spawn(Vec3::new(2.5, 11.0, 2.5), 0.0));
        assert_eq!(h.list.len(), 1);
        assert_eq!(egg_pixels().count(), EGG.iter().flat_map(|r| r.chars()).filter(|c| *c != '.').count());
        assert!(EGG.iter().all(|r| r.chars().count() == 16));
    }

    /// « Tu me montreras l'IRM des couches du monstre » : ses couches de devant vers le dos
    /// (sa peau, sa gelée, le creux de sa bouche), écrites dans le fichier nommé par
    /// MINELIB_IRM (essai à lancer à la main).
    #[test]
    #[ignore]
    fn irm_of_the_evide() {
        let Ok(path) = std::env::var("MINELIB_IRM") else {
            return;
        };
        let img = crate::player_model::irm(
            grids(),
            |part, c, d| {
                if part == HEAD && mouth(c, d, 0.35) {
                    return None;
                }
                let [x, y, z] = c;
                let face = if z == 0 {
                    Some(0)
                } else if z + 1 == d[2] {
                    Some(1)
                } else if x + 1 == d[0] {
                    Some(2)
                } else if x == 0 {
                    Some(3)
                } else if y + 1 == d[1] {
                    Some(4)
                } else if y == 0 {
                    Some(5)
                } else {
                    None
                };
                Some(match face {
                    Some(k) => skin(part, c, d, k),
                    None => crate::flesh::jelly(c, d),
                })
            },
            TALL,
        );
        img.save(path).unwrap();
    }
}
