//! Modèle du joueur vu à la 3e personne (F5) : tête, corps, bras et jambes en pavés, avec
//! un skin original dessiné pixel par pixel. Les bras et les jambes se balancent en marchant,
//! la tête suit le regard, le corps se penche quand on s'accroupit.
//!
//! (v225) Aux proportions d'un personnage de CS2, à la même taille (1,8 bloc = 32 pixels) :
//! une tête d'un septième de la hauteur (un quart avant, comme dans les jeux de blocs), des
//! jambes de près de la moitié, des bras et des jambes plus fins. Chaque pavé garde les
//! pixels de sa zone du skin (`TEX`, la disposition des skins 64 × 64), étirés à sa taille
//! (`PARTS`).

use glam::{Mat4, Vec3, Vec4};
use minelib_render::{ArmVertex, EntityVertex, PlayerInstance, PlayerVertex, RayBox, RayBoxKind, ShadowBox};
use minelib_world::BlockId;

/// Taille d'un pixel du skin, en blocs (32 pixels = hauteur du joueur).
const PX: f32 = 1.8 / 32.0;

#[derive(Clone, Copy, PartialEq)]
enum Part {
    Head,
    Body,
    Arm,
    Leg,
}

#[derive(Clone, Copy, PartialEq)]
enum Face {
    Front,
    Back,
    Side,
    Top,
    Bottom,
}

/// État du joueur dont le modèle a besoin.
pub struct Pose {
    /// Pieds (centre).
    pub feet: Vec3,
    pub yaw: f32,
    pub pitch: f32,
    /// Phase de la marche (radians) et ampleur 0..1 (vitesse).
    pub walk: f32,
    pub swing: f32,
    /// 0 debout … 1 accroupi.
    pub crouch: f32,
    /// Lumière reçue (linéaire).
    pub light: [f32; 3],
    /// Objet en main.
    pub held: Option<Held>,
    /// Skin du joueur (sinon le modèle par défaut de Minelib).
    pub skin: Option<std::sync::Arc<crate::skin::Skin>>,
    /// (v272) Le haut du corps plié en avant aux hanches (rad), en plus de l'accroupi : « on
    /// doit se voir se pencher pour manger le morceau qu'on vise ». La tête garde son regard.
    /// 0 presque toujours (le joueur local seulement : le modèle partagé de la carte
    /// graphique ne le connaît pas).
    pub bow: f32,
}

fn srgb(c: u32) -> [f32; 3] {
    [(c >> 16) & 255, (c >> 8) & 255, c & 255].map(|v| (v as f32 / 255.0).powf(2.2))
}

const SKIN: u32 = 0xd6a684;
const SKIN_DARK: u32 = 0xc08f6d;
const HAIR: u32 = 0x241c18;
const EYE_WHITE: u32 = 0xf2f2f2;
const IRIS: u32 = 0x5a3a1e;
const MOUTH: u32 = 0x8a5a48;
const SHIRT: u32 = 0xb5523a;
const SHIRT_DARK: u32 = 0x8f3f2c;
const PANTS: u32 = 0x5b6b3a;
const SHOES: u32 = 0x5a3a22;

/// Couleur du pixel (`u`, `v`) d'une face (v = 0 en haut ; `n` = largeur de la face).
fn skin(part: Part, face: Face, u: usize, v: usize, n: usize) -> u32 {
    match part {
        Part::Head => match face {
            Face::Top => HAIR,
            Face::Bottom => SKIN,
            Face::Back => {
                if v <= 6 {
                    HAIR
                } else {
                    SKIN
                }
            }
            // Côtés : cheveux en haut et vers l'arrière (u grand = arrière à droite ou à
            // gauche selon le côté : symétrique, on prend les deux bords).
            Face::Side => {
                if v <= 1 || (v <= 4 && (u <= 1 || u + 2 >= n)) {
                    HAIR
                } else {
                    SKIN
                }
            }
            Face::Front => match (u, v) {
                (_, 0..=1) => HAIR,
                (0 | 7, 2) => HAIR,
                (1, 4) | (6, 4) => EYE_WHITE,
                (2, 4) | (5, 4) => IRIS,
                (3..=4, 5) => SKIN_DARK,
                (3..=4, 6) => MOUTH,
                _ => SKIN,
            },
        },
        Part::Body => match face {
            Face::Front if v == 0 && (3..=4).contains(&u) => SKIN,
            Face::Front | Face::Back if v == 11 => SHIRT_DARK,
            _ => SHIRT,
        },
        Part::Arm => match face {
            Face::Top => SHIRT,
            Face::Bottom => SKIN,
            _ if v <= 3 => {
                if v == 3 {
                    SHIRT_DARK
                } else {
                    SHIRT
                }
            }
            _ => SKIN,
        },
        Part::Leg => match face {
            Face::Top => PANTS,
            Face::Bottom => SHOES,
            _ if v >= 9 => SHOES,
            _ => PANTS,
        },
    }
}

/// Ombrage d'une face selon son orientation dans le monde (normale `n` unitaire).
pub fn face_shade(n: Vec3) -> f32 {
    (0.78 + 0.22 * n.y + 0.06 * n.x).clamp(0.45, 1.0)
}

/// Couleur (linéaire, grain compris, sans l'ombrage de la face) du pixel (`u`, `v`) de la
/// face `k` (ordre de `emit_box` : devant, derrière, côtés, dessus, dessous) du bras droit,
/// telle que le modèle du joueur la dessine (skin, sinon couleurs par défaut).
pub fn right_arm_pixel(skin: Option<&crate::skin::Skin>, k: usize, u: usize, v: usize) -> [f32; 3] {
    const RIGHT_ARM: usize = 4;
    let face = [
        Face::Front,
        Face::Back,
        Face::Side,
        Face::Side,
        Face::Top,
        Face::Bottom,
    ][k.min(5)];
    let rgb = skin
        .and_then(|t| t.color(RIGHT_ARM, k, u as u32, v as u32, [4, 12, 4]))
        .unwrap_or_else(|| self::skin(Part::Arm, face, u, v, 4));
    let g = grain(k * 4096 + v * 64 + u + Part::Arm as usize * 977);
    srgb(rgb).map(|c| c * g)
}

/// Petite variation par pixel (texture) : ±3 %.
fn grain(seed: usize) -> f32 {
    let h = (seed as u32).wrapping_mul(2654435761) >> 24;
    0.97 + (h as f32 / 255.0) * 0.06
}

/// Faces d'un pavé `[min, max]` de `w` × `h` × `d` pixels du skin, dans l'ordre de
/// `Skin::color` (devant, derrière, côtés, dessus, dessous) : (face, origine en haut à gauche
/// vue de face, axe u, axe v (un pixel chacun pour un pavé non agrandi), largeur, hauteur).
#[allow(clippy::type_complexity)]
fn box_faces(
    min: [f32; 3],
    max: [f32; 3],
    [w, h, d]: [usize; 3],
) -> [(Face, [f32; 3], [f32; 3], [f32; 3], usize, usize); 6] {
    let [x0, y0, z0] = min;
    let [x1, y1, z1] = max;
    [
        (
            Face::Front,
            [x1, y1, z0],
            [-1.0, 0.0, 0.0],
            [0.0, -1.0, 0.0],
            w,
            h,
        ),
        (
            Face::Back,
            [x0, y1, z1],
            [1.0, 0.0, 0.0],
            [0.0, -1.0, 0.0],
            w,
            h,
        ),
        (
            Face::Side,
            [x1, y1, z1],
            [0.0, 0.0, -1.0],
            [0.0, -1.0, 0.0],
            d,
            h,
        ),
        (
            Face::Side,
            [x0, y1, z0],
            [0.0, 0.0, 1.0],
            [0.0, -1.0, 0.0],
            d,
            h,
        ),
        (
            Face::Top,
            [x0, y1, z1],
            [1.0, 0.0, 0.0],
            [0.0, 0.0, -1.0],
            w,
            d,
        ),
        (
            Face::Bottom,
            [x0, y0, z0],
            [1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0],
            w,
            d,
        ),
    ]
}

/// Pavé `[min, max]` (pixels, repère du modèle : x droite, y haut, z arrière) transformé
/// par `xf` (pixels → monde), découpé en pixels colorés.
/// (v225) `zone` : pixels du skin du pavé (`ZONES`), étirés à sa taille.
#[allow(clippy::too_many_arguments)]
fn emit_box(
    out: &mut Vec<EntityVertex>,
    xf: Mat4,
    min: [f32; 3],
    max: [f32; 3],
    zone: Zone,
    part: Part,
    light: [f32; 3],
    tex: Option<&crate::skin::Skin>,
) {
    let tex = tex.map(|t| (t, zone.skin));
    emit_box_layer(out, xf, min, max, Some(zone), part, light, tex, None, 1);
}

/// Comme `emit_box` ; `layer` : pixels transparents laissés vides (têtes des cartes :
/// buissons…), avec les deux calques du skin (`false`) ou seulement celui du dessus
/// (`true`).
#[allow(clippy::too_many_arguments)]
fn emit_box_layer(
    out: &mut Vec<EntityVertex>,
    xf: Mat4,
    min: [f32; 3],
    max: [f32; 3],
    zone: Option<Zone>,
    part: Part,
    light: [f32; 3],
    tex: Option<(&crate::skin::Skin, usize)>,
    layer: Option<bool>,
    lod: usize,
) {
    let [x0, y0, z0] = min;
    let [x1, y1, z1] = max;
    let lit = EntityVertex::light(light, true);
    // Nombre de pixels du skin par côté : ceux de sa zone du skin (`zone`, v225), sinon la
    // taille du pavé arrondie (une boîte agrandie garde 8 pixels : chacun un peu plus grand).
    // (v256) `zone` dit aussi où ces pixels sont dans la partie entière du skin.
    let zone = zone.unwrap_or_else(|| Zone::whole(0, box_size(min, max)));
    let [w, h, d] = zone.size;
    let k = [
        (x1 - x0) / w.max(1) as f32,
        (y1 - y0) / h.max(1) as f32,
        (z1 - z0) / d.max(1) as f32,
    ];
    let faces = box_faces(min, max, [w, h, d]);
    let step = Vec3::from(k);
    for (k, (face, o, du, dv, nu, nv)) in faces.into_iter().enumerate() {
        let (o, du, dv) = (Vec3::from(o), Vec3::from(du) * step, Vec3::from(dv) * step);
        let p = |u: f32, v: f32| xf.transform_point3(o + du * u + dv * v);
        // Ombrage selon l'orientation de la face dans le monde.
        let n = xf.transform_vector3(du.cross(dv)).normalize_or_zero();
        let shade = face_shade(n);
        let lod = lod.max(1);
        for v in (0..nv).step_by(lod) {
            for u in (0..nu).step_by(lod) {
                // Pixel de la partie entière du skin (le pavé peut n'en montrer qu'un morceau).
                let (su, sv) = zone.pixel(k, u, v);
                let g = grain(k * 4096 + sv * 64 + su + part as usize * 977);
                let size = zone.full.map(|n| n as u32);
                let rgb = match (tex, layer) {
                    // De loin : un carré par groupe de `lod` × `lod` pixels, à leur couleur
                    // moyenne (vide si la plupart sont transparents).
                    (Some((t, i)), Some(_)) if lod > 1 => {
                        let (mut sum, mut n) = ([0u32; 3], 0u32);
                        for dv2 in 0..lod.min(nv - v) {
                            for du2 in 0..lod.min(nu - u) {
                                if let Some(c) =
                                    t.color(i, k, (su + du2) as u32, (sv + dv2) as u32, size)
                                {
                                    sum[0] += (c >> 16) & 255;
                                    sum[1] += (c >> 8) & 255;
                                    sum[2] += c & 255;
                                    n += 1;
                                }
                            }
                        }
                        if n * 2 < (lod * lod) as u32 {
                            continue;
                        }
                        (sum[0] / n) << 16 | (sum[1] / n) << 8 | (sum[2] / n)
                    }
                    (Some((t, i)), Some(top)) => {
                        let c = if top {
                            t.layer_color(i, k, su as u32, sv as u32, size, true)
                        } else {
                            t.color(i, k, su as u32, sv as u32, size)
                        };
                        match c {
                            Some(c) => c,
                            None => continue,
                        }
                    }
                    _ => tex
                        .and_then(|(t, i)| t.color(i, k, su as u32, sv as u32, size))
                        .unwrap_or_else(|| skin(part, face, su, sv, zone.width(k))),
                };
                let c = srgb(rgb);
                let g = if lod > 1 { 1.0 } else { g };
                let col = [0, 1, 2].map(|i| c[i] * shade * g);
                let (u1, v1) = ((u + lod).min(nu) as f32, (v + lod).min(nv) as f32);
                let (a, b, cc, dd) = (
                    p(u as f32, v as f32),
                    p(u1, v as f32),
                    p(u1, v1),
                    p(u as f32, v1),
                );
                for q in [a, b, cc, cc, dd, a] {
                    out.push(EntityVertex {
                        pos: q.to_array(),
                        color: col,
                        uv: [0.0; 2],
                        layer: -1,
                        light: lit,
                    });
                }
            }
        }
    }
}

/// (v272) Le milieu de la bouche du modèle dans cette pose (monde) : au bas du visage.
pub fn mouth_point(pose: &Pose) -> Vec3 {
    let (xf, lo, hi, _) = parts(pose)[5];
    xf.transform_point3(Vec3::new(0.0, lo[1] + 0.22 * (hi[1] - lo[1]), lo[2]))
}

/// (v272) Plié au plus de tant (rad) pour manger.
pub const BOW_MAX: f32 = 1.9;

/// (v272) Plie le modèle (`pose.bow`, de 0 à `BOW_MAX`) pour amener sa bouche au plus près
/// de `target` ; rend l'écart qui reste (blocs). La bouche décrit un arc autour des
/// hanches : l'écart n'a qu'un creux, cherché par tiers.
pub fn bow_to(pose: &mut Pose, target: Vec3) -> f32 {
    let mut miss = |bow: f32| {
        pose.bow = bow;
        mouth_point(pose).distance(target)
    };
    let (mut lo, mut hi) = (0.0f32, BOW_MAX);
    for _ in 0..18 {
        let (a, b) = (lo + (hi - lo) / 3.0, hi - (hi - lo) / 3.0);
        if miss(a) < miss(b) {
            hi = b;
        } else {
            lo = a;
        }
    }
    miss((lo + hi) / 2.0)
}

/// (v272) De combien les yeux du modèle se déplacent quand il se plie de `pose.bow` (monde) :
/// à la première personne, la caméra plonge avec la tête.
pub fn bow_shift(pose: &Pose) -> Vec3 {
    if pose.bow == 0.0 {
        return Vec3::ZERO;
    }
    let bent = crouch_lean(pose.crouch);
    let p = bent.neck() + Vec3::new(0.0, 0.6 * HEAD_S, 0.0) - Vec3::new(0.0, LEG - bent.drop, 0.0);
    let moved = Mat4::from_rotation_x(-pose.bow).transform_vector3(p) - p;
    (Mat4::from_rotation_y(-pose.yaw) * Mat4::from_scale(Vec3::splat(PX))).transform_vector3(moved)
}

/// (v272) Où est, dans le monde, le milieu du cube `cell` du pavé `part` (grille
/// `flesh_grids`) d'un joueur dans cette pose : l'endroit que L'Évidé vise, d'où le sang
/// goutte. Il suit le membre qui bouge.
pub fn cell_point(pose: &Pose, part: usize, cell: [usize; 3]) -> Vec3 {
    let all = parts(pose);
    let part = part.min(all.len() - 1);
    let (xf, lo, hi, _) = all[part];
    let d = ZONES[part].size;
    let (lo, hi) = (Vec3::from(lo), Vec3::from(hi));
    let t = Vec3::new(
        (cell[0] as f32 + 0.5) / d[0] as f32,
        (cell[1] as f32 + 0.5) / d[1] as f32,
        (cell[2] as f32 + 0.5) / d[2] as f32,
    );
    xf.transform_point3(lo + (hi - lo) * t)
}

/// (v269) Les mesures du modèle (pixels), pour L'Évidé : le même corps, étiré en hauteur.
pub(crate) struct Rig {
    /// Taille d'un pixel, en blocs.
    pub px: f32,
    /// Coins de chaque pavé dans son repère (l'ordre de `parts`).
    pub boxes: [([f32; 3], [f32; 3]); 7],
    /// Hauteur des hanches, écart des jambes, hauteur des épaules au-dessus des hanches,
    /// écart des bras, hauteur du cou au-dessus des hanches.
    pub leg: f32,
    pub leg_x: f32,
    pub shoulder_y: f32,
    pub arm_x: f32,
    pub body_h: f32,
}

pub(crate) fn rig() -> Rig {
    Rig {
        px: PX,
        boxes: PARTS.map(|(lo, hi, _)| (lo, hi)),
        leg: LEG,
        leg_x: LEG_X,
        shoulder_y: SHOULDER_Y,
        arm_x: ARM_X,
        body_h: BODY_H,
    }
}

/// (v269) Grille de cubes de chaque pavé du modèle (un cube par pixel de son skin), dans
/// l'ordre de `parts` : là où mord L'Évidé (`flesh::Flesh`).
pub fn flesh_grids() -> [[usize; 3]; crate::flesh::PARTS] {
    std::array::from_fn(|i| ZONES[i].size)
}

/// (v269) Un pavé mordu, dessiné cube par cube : un cube par pixel du skin, moins ceux que les
/// morsures ont retirés (`flesh`). Une face tournée vers le dehors garde son pixel du skin
/// (exactement celui d'`emit_box`) ; une face mise à nu par une morsure prend la couleur de ce
/// qu'il y a là dans le corps (`inside` : os, organes, ou la gelée de L'Évidé), un peu dans
/// l'ombre du creux. « La forme reste carrée. »
#[allow(clippy::too_many_arguments)]
fn emit_flesh(
    out: &mut Vec<EntityVertex>,
    xf: Mat4,
    min: [f32; 3],
    max: [f32; 3],
    index: usize,
    part: Part,
    light: [f32; 3],
    tex: Option<&crate::skin::Skin>,
    flesh: &crate::flesh::Flesh,
    inside: fn(usize, [usize; 3], [usize; 3]) -> u32,
) {
    // Les six côtés d'un cube : (axe, sens, face dans l'ordre de `box_faces`).
    const SIDES: [(usize, i32, usize); 6] = [
        (2, -1, 0),
        (2, 1, 1),
        (0, 1, 2),
        (0, -1, 3),
        (1, 1, 4),
        (1, -1, 5),
    ];
    const FACES: [Face; 6] = [
        Face::Front,
        Face::Back,
        Face::Side,
        Face::Side,
        Face::Top,
        Face::Bottom,
    ];
    let zone = ZONES[index];
    let dims = zone.size;
    let (lo, hi) = (Vec3::from(min), Vec3::from(max));
    let step = (hi - lo) / Vec3::new(dims[0] as f32, dims[1] as f32, dims[2] as f32);
    let lit = EntityVertex::light(light, true);
    let full = zone.full.map(|n| n as u32);
    for z in 0..dims[2] {
        for y in 0..dims[1] {
            for x in 0..dims[0] {
                let c = [x, y, z];
                if flesh.gone(index, dims, c) {
                    continue;
                }
                for (axis, sign, k) in SIDES {
                    // Le voisin de ce côté : dehors (le pixel du skin), mangé (la chair à nu),
                    // ou encore là (pas de face).
                    let n = c[axis] as i32 + sign;
                    let outside = n < 0 || n >= dims[axis] as i32;
                    if !outside {
                        let mut nc = c;
                        nc[axis] = n as usize;
                        if !flesh.gone(index, dims, nc) {
                            continue;
                        }
                    }
                    // L'ombrage d'`emit_box` (`du × dv` : vers le dedans pour les quatre faces
                    // du tour, vers le dehors pour le dessus et le dessous), pour que les
                    // pixels qui restent soient exactement ceux du pavé entier.
                    let mut normal = Vec3::ZERO;
                    normal[axis] = if axis == 1 { sign as f32 } else { -sign as f32 };
                    let shade = face_shade(xf.transform_vector3(normal).normalize_or_zero());
                    let color = if outside {
                        let (u, v) = match k {
                            0 => (dims[0] - 1 - x, dims[1] - 1 - y),
                            1 => (x, dims[1] - 1 - y),
                            2 => (dims[2] - 1 - z, dims[1] - 1 - y),
                            3 => (z, dims[1] - 1 - y),
                            4 => (x, dims[2] - 1 - z),
                            _ => (x, z),
                        };
                        let (su, sv) = zone.pixel(k, u, v);
                        let rgb = tex
                            .and_then(|t| t.color(zone.skin, k, su as u32, sv as u32, full))
                            .unwrap_or_else(|| skin(part, FACES[k], su, sv, zone.width(k)));
                        let g = grain(k * 4096 + sv * 64 + su + part as usize * 977);
                        let mut lin = srgb(rgb);
                        // (v272) « Fais que du sang en coule » : sur les faces du tour, les
                        // cubes sous un cube mangé rougissent, de moins en moins, sur deux
                        // à cinq cubes selon la colonne.
                        if axis != 1 {
                            let j = flesh.drip(index, dims, c);
                            if j > 0 && j <= 2 + (x * 31 + z * 17 + index * 7) % 4 {
                                let t = 0.92 - 0.13 * (j as f32 - 1.0);
                                let blood = srgb(crate::flesh::BLOOD);
                                lin = [0, 1, 2].map(|i| lin[i] + (blood[i] - lin[i]) * t);
                            }
                        }
                        lin.map(|c| c * shade * g)
                    } else {
                        let g = grain(index * 7919 + (z * 64 + y) * 64 + x + k * 131);
                        srgb(inside(index, c, dims)).map(|c| c * shade * g * 0.86)
                    };
                    // Les quatre coins de la face.
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

/// (v269) Les pavés du corps d'un joueur (sans ce qu'il tient), morsures comprises : un pavé
/// mordu est dessiné cube par cube (`emit_flesh`, l'intérieur d'un joueur : `flesh::organ`),
/// les autres comme d'habitude. `skip` : les pavés à ne pas dessiner.
fn emit_body(
    out: &mut Vec<EntityVertex>,
    pose: &Pose,
    flesh: Option<&crate::flesh::Flesh>,
    skip: &[usize],
) {
    for (i, (xf, min, max, part)) in parts(pose).into_iter().enumerate() {
        if skip.contains(&i) {
            continue;
        }
        let tex = pose.skin.as_deref();
        match flesh.filter(|f| f.bitten(i)) {
            Some(f) => emit_flesh(
                out,
                xf,
                min,
                max,
                i,
                part,
                pose.light,
                tex,
                f,
                crate::flesh::organ,
            ),
            None => emit_box(out, xf, min, max, ZONES[i], part, pose.light, tex),
        }
    }
}

/// Objet tenu dans la main droite (vu dans les reflets).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Held {
    /// Bloc, avec sa forme (dalle, bannière de deux blocs…).
    Block(BlockId),
    /// Baguette de WorldEdit (couleur sRGB), tenue droite.
    Wand([u8; 3]),
    /// Torche (couleur de sa flamme, sRGB) : bâton levé devant la main, flamme au bout.
    Torch([u8; 3]),
    /// (v177) Équipement du mode FPS, avec son vrai modèle (celui de la vue à la 1re
    /// personne) : arme (indice dans `WEAPONS`, tenue à deux mains, pointée où l'on
    /// regarde), grenade (`ITEM_NADE` + type), bombe (`cs::W_BOMB`) ou couteau (le reste).
    Gear(u8),
    /// (v201) Lampe torche (allumée ou non), pointée où l'on regarde.
    Flashlight(bool),
    /// (v216) Foudre de Zeus : rien de solide (l'éclair est dessiné à part, voir `storm`).
    Zeus,
}

/// (v177) Arme à feu en main : les deux bras se tendent vers le regard (`aims`).
fn aims(pose: &Pose) -> bool {
    matches!(pose.held, Some(Held::Gear(c)) if (c as usize) < crate::weapons::WEAPONS.len())
}

/// (v215) Lampe torche en main : le bras droit seul tendu là où l'on regarde (comme pour
/// une arme à feu), la lampe pointée devant. Ainsi elle est dans le champ à la première
/// personne, à la place même où la voient les miroirs et les autres joueurs. (v239) Tout objet
/// tenu en construction aussi (bloc, tête, torche, baguette, écran), le bras pointé plus bas
/// que le regard (`ITEM_AIM_DROP`) : rend de combien. Pas la Foudre de Zeus (bras levé), ni
/// l'équipement du mode FPS.
fn aims_right(pose: &Pose) -> Option<f32> {
    match pose.held {
        Some(Held::Flashlight(_)) => Some(0.0),
        Some(Held::Block(_) | Held::Wand(_) | Held::Torch(_)) => Some(ITEM_AIM_DROP),
        _ => None,
    }
}

/// (v239) « Rends global ton travail sur la synchro du bras à la première personne et en F5
/// avec la lampe torche, … sauf qu'en dehors de la lampe torche le bras pointe entre le
/// milieu et le bas de l'écran, 50 % » : un objet tenu en construction (autre que la lampe)
/// se tient le bras tendu comme elle, pointé plus bas que le regard de cet angle (radians).
/// Le même que `ITEM_AIM_DROP` de players.wgsl.
pub const ITEM_AIM_DROP: f32 = 0.367;

/// (v239) Bras droit tendu (lampe, objet tenu) : angle sous l'horizontale (radians) le plus
/// bas où il pointe (regard tout en bas, objet tenu pointé encore plus bas).
const AIM_LOWEST: f32 = 1.4;

/// (v177) Bras tendus d'une arme à feu : rotation autour de la verticale (radians) qui
/// ramène la main vers le milieu du corps, pour le bras droit et pour le gauche (qui vient
/// tenir l'arme devant). Les mêmes que `AIM_TURN_*` de players.wgsl.
const AIM_TURN_RIGHT: f32 = 0.15;
const AIM_TURN_LEFT: f32 = -0.5;
/// (v217) Foudre de Zeus : le bras droit levé devant soi (radians), quel que soit le regard.
/// Le même que `ZEUS_RAISE` de players.wgsl (v225 : avant, les autres joueurs le baissaient
/// avec le regard).
const ZEUS_RAISE: f32 = 1.15;

// (v225) Proportions d'un personnage de CS2 (demande de Viendax, liste du 21 novembre,
// point 23 : « les proportions tête/corps d'un personnage de CS2, même taille totale »), en
// pixels de 1,8/32 bloc. Avant (jeux de blocs) : jambes et corps de 12, tête de 8, bras et
// jambes de 4 d'épaisseur. Les mêmes valeurs dans `part_matrix` (players.wgsl).

// (v234) Demande du 22 novembre : « si la tête augmente de 7,5 %, réduis ce qu'il faut pour
// que le corps soit à la même taille qu'actuellement ». La tête grandit de 7,5 % ; tout le
// reste (jambes, corps, bras, en hauteur comme en largeur) est réduit d'un même facteur
// (`BODY_K`), pour garder 32 pixels de haut.

// (v264) Demande du 24 novembre : la tête d'un personnage de CS ramenée à notre taille est un
// ovale de 0,165 × 0,24 bloc ; « si l'ovale était transformé en rond et qu'on prenait le carré
// qui colle à ce rond, arrondi pour que ça colle à la grille de pixels du skin, vers le côté
// le plus proche ». Le rond de même surface fait 0,199 bloc ; le carré autour, calé sur la
// grille du skin du corps, 4 colonnes : 0,203 bloc (3 en feraient 0,152, 5 feraient 0,254). La
// tête fait donc la moitié de la largeur de la poitrine et la profondeur du corps, et un
// pixel du visage vaut un demi-pixel du corps. Le corps grandit pour garder 32 pixels.
/// Côté de la tête (un cube, sur le cou : en haut, à 32 pixels) : 4,5 en v225, +7,5 % en
/// v234 ; (v264) 4 colonnes du skin du corps (`COL`), soit 3,5 × `BODY_K`.
const HEAD_S: f32 = 3.5 * BODY_K;
/// (v234) Facteur du reste du corps par rapport à la v225 (27,5 pixels sous la tête).
/// (v264) Tel que la tête (3,5 × ce facteur) et le corps fassent 32 pixels : 32/31.
const BODY_K: f32 = 32.0 / 31.0;
/// Hauteur d'une jambe (pixels) : la hanche est à cette hauteur quand on est debout.
const LEG: f32 = 15.0 * BODY_K;
/// Profondeur d'une jambe : les quatre colonnes de son skin.
const LEG_D: f32 = 3.5 * BODY_K;
/// (v256) Largeur d'une jambe : trois colonnes de son skin (la colonne extérieure n'est pas
/// montrée, voir `ZONES`). Avant : quatre (carrée), les deux jambes collées.
const LEG_W: f32 = 3.0 * COL;
/// (v256) Écart entre les deux jambes (pixels) : elles ne sont plus collées.
const LEG_GAP: f32 = 0.8;
/// Corps : hauteur (de la hanche au cou), largeur (de la poitrine), profondeur.
const BODY_H: f32 = 12.5 * BODY_K;
const BODY_W: f32 = 7.0 * BODY_K;
const BODY_D: f32 = 3.5 * BODY_K;
// (v256) Demande du 23 novembre : « refais le personnage avec le schéma "CS2", mais avec nos
// carrés ; les hitbox devront être celles de celui-ci aussi ». Sept pavés au lieu de six : le
// corps est coupé en une poitrine (large) et un ventre (plus étroit), les jambes sont
// écartées, et les bras ne tiennent plus au corps que par le coin du haut de l'épaule (« colle
// à l'épaule la pointe tout en haut de l'épaule, le bras reste placé sinon »). « Un pixel du
// skin ne doit pas être entre deux parties du corps » : la limite poitrine/ventre tombe entre
// deux rangées du skin. Choix du 24 novembre (« celui de droite ») : les pixels du ventre et
// des jambes gardent la taille de ceux de la poitrine et restent alignés du haut en bas ; ce
// sont les colonnes du bord qui ne sont pas montrées (une de chaque côté du ventre, la colonne
// extérieure de chaque jambe).
/// (v256) Une colonne et une rangée du skin sur le corps (pixels du modèle). Les jambes ont
/// les mêmes colonnes (`LEG_D` / 4).
const COL: f32 = BODY_W / 8.0;
const ROW: f32 = BODY_H / 12.0;
/// (v256) Ventre : les 5 rangées du bas du corps (la poitrine : les 7 du haut), 6 colonnes
/// de large (la poitrine : 8).
const BELLY_ROWS: usize = 5;
const BELLY_H: f32 = BELLY_ROWS as f32 * ROW;
const BELLY_W: f32 = 6.0 * COL;
/// Bras : épaisseur, longueur au-dessus du pivot de l'épaule (le haut du bras arrive en haut
/// du corps) et au-dessous (jusqu'au poing, un peu au-dessus de la hanche).
const ARM_W: f32 = 2.75 * BODY_K;
const ARM_UP: f32 = 1.75 * BODY_K;
const ARM_DOWN: f32 = 9.75 * BODY_K;
/// (v256) Écart d'un bras qui pend (radians, autour de l'axe avant-arrière, à l'épaule) : la
/// main à l'écart du corps, le bras ne le touche que par le coin du haut de l'épaule. Pas le
/// bras qui vise ou qui tient un objet (il est tendu devant). Son sinus et son cosinus (les
/// constantes ne savent pas les calculer ; vérifiés par `the_arm_splay_is_six_degrees`).
pub const ARM_SPLAY: f32 = 0.104_719_76;
const SPLAY_SIN: f32 = 0.104_528_46;
const SPLAY_COS: f32 = 0.994_521_9;
/// Pivot de l'épaule : hauteur au-dessus de la hanche, écart au milieu du corps. (v256) Placé
/// pour que le coin du haut du bras écarté (côté corps) soit sur le coin du haut de la
/// poitrine. Avant : `BODY_H - ARM_UP` et `BODY_W / 2 + ARM_W / 2` (bras collé au corps).
const SHOULDER_Y: f32 = BODY_H + ARM_W / 2.0 * SPLAY_SIN - ARM_UP * SPLAY_COS;
const ARM_X: f32 = BODY_W / 2.0 + ARM_W / 2.0 * SPLAY_COS + ARM_UP * SPLAY_SIN;
/// Écart d'une jambe au milieu.
const LEG_X: f32 = LEG_GAP / 2.0 + LEG_W / 2.0;
/// Cou (bas de la tête), debout.
const NECK_Y: f32 = LEG + BODY_H;

/// (v225) Hauteurs du modèle debout, en blocs : hanches, bas de la tête, côté de la tête
/// (visée des bots, boîte de la tête pour le couteau). (v256) Les zones des coups des balles
/// sont les pavés du modèle (`hit_boxes`) : les deux premières ne servent plus qu'aux essais.
#[cfg(test)]
pub const HIP_BLOCKS: f32 = LEG * PX;
#[cfg(test)]
pub const NECK_BLOCKS: f32 = NECK_Y * PX;
pub const HEAD_BLOCKS: f32 = HEAD_S * PX;
/// (v225) Épaisseur d'un bras par rapport à sa longueur (le bras à la première personne a
/// les mêmes proportions, `hand::ARM_THICK`).
pub const ARM_RATIO: f32 = ARM_W / (ARM_UP + ARM_DOWN);
// Debout : 32 pixels (1,8 bloc), la tête en haut (v264 : 4 colonnes du skin du corps, la
// profondeur du corps, un peu moins d'un neuvième de la hauteur), le poing au-dessus de la
// hanche.
const _: () = assert!(
    (NECK_Y + HEAD_S - 32.0).abs() < 1e-4
        && SHOULDER_Y - ARM_DOWN > 0.0
        && (HEAD_S - 4.0 * COL).abs() < 1e-5
        && (HEAD_S - BODY_D).abs() < 1e-5
        && (HEAD_BLOCKS / 1.8 - 1.0 / 8.857).abs() < 0.001
);

/// Posture accroupie (`crouch` de 0 à 1). (v260) « Plie légèrement le dos quand on sneak » :
/// le ventre se penche autour des hanches, la poitrine se plie un peu plus à la taille.
#[derive(Clone, Copy, Debug)]
struct Crouch {
    /// Penche du ventre vers l'avant (radians, autour des hanches).
    belly: f32,
    /// Penche de la poitrine (celle du ventre plus le pli du dos) ; les bras et la tête
    /// tiennent à la poitrine.
    chest: f32,
    /// Tassement des hanches (pixels).
    drop: f32,
    /// Origine du repère de la poitrine (là où tombe sa hanche), dans le repère des pieds
    /// (pixels) : la hanche debout ; accroupi, décalée par le pli de la taille.
    chest_at: Vec3,
}

impl Crouch {
    /// Cou (le milieu du haut de la poitrine, sous la tête), dans le repère des pieds
    /// (pixels ; −z : devant). (v234) « Quand je me penche en avant, ma tête doit rester au
    /// milieu de mon cou » : la tête reste posée dessus, elle avance avec lui.
    fn neck(&self) -> Vec3 {
        self.chest_at + Vec3::new(0.0, BODY_H * self.chest.cos(), -BODY_H * self.chest.sin())
    }
}

fn crouch_lean(crouch: f32) -> Crouch {
    let c = crouch.clamp(0.0, 1.0);
    let (belly, bend, drop) = (CROUCH_BELLY_LEAN * c, CROUCH_BEND * c, CROUCH_HIP_DROP * c);
    // Le pli se fait à la taille, autour du bord arrière du haut du ventre : le dos reste
    // joint, le bas de la poitrine entre un peu dans le ventre par-devant. Tourner la poitrine
    // de `bend` autour de ce point `p` (repère du ventre), c'est la tourner autour de sa
    // hanche puis la décaler de p − R(bend)·p ; le tout penché comme le ventre.
    let (py, pz) = (BELLY_H, BODY_D / 2.0);
    let (sb, cb) = bend.sin_cos();
    let (dy, dz) = (py * (1.0 - cb) - pz * sb, py * sb + pz * (1.0 - cb));
    let (sa, ca) = belly.sin_cos();
    let chest_at = Vec3::new(0.0, LEG - drop + dy * ca + dz * sa, -dy * sa + dz * ca);
    Crouch {
        belly,
        chest: belly + bend,
        drop,
        chest_at,
    }
}

/// (v251) Balancement du bras levé en marchant (part du balancement des jambes) : celui du
/// bras tendu de la lampe torche. Le même dans `part_matrix` (players.wgsl).
const RAISED_SWING: f32 = 0.1;

/// Penche du ventre accroupi (radians), et pli du dos à la taille : la poitrine est penchée
/// de leur somme. Les mêmes dans `part_matrix` (players.wgsl). (v260) Avant, tout le haut du
/// corps était penché d'un bloc de 0,45 ; le dos se plie maintenant de 0,25 (14°), et ces
/// valeurs laissent la tête exactement où elle était (à 0,0001 bloc près).
pub const CROUCH_BELLY_LEAN: f32 = 0.3248;
pub const CROUCH_BEND: f32 = 0.25;

/// (v247) Tassement des hanches accroupi (pixels). « Mon crâne dépasse dans les blocs du
/// dessus quand je passe accroupi sous 1,5 bloc ; mon corps ne descend pas assez » : avant,
/// 3,2 (0,18 bloc), le haut de la tête arrivait à 1,55 bloc (1,58 la tête levée ou baissée),
/// au-dessus de la boîte accroupie (1,5). Maintenant 4,85 : la tête accroupie, quel que soit le
/// regard, reste sous 1,49 bloc, et son milieu à la hauteur des yeux (1,32). Debout, elle
/// arrive à 1,8 (1,83 aux coins la tête penchée : rien ne passe sous 1,8 bloc debout). La même
/// dans `part_matrix` (players.wgsl). (v260) 4,266 : le dos plié raccourcit le haut du corps
/// (du cou aux hanches) ; les hanches descendent d'autant moins, la tête reste à sa hauteur.
/// (v264) 4,837 : la tête est plus petite et le corps plus grand ; accroupi, le milieu de la
/// tête reste à la hauteur des yeux (1,32), son haut à 1,42 (1,45 aux coins, la tête penchée).
pub const CROUCH_HIP_DROP: f32 = 4.837;

/// (v234) Tête du modèle accroupi de `crouch` (0 à 1) : de combien elle avance vers le regard
/// et la hauteur de son bas au-dessus des pieds (blocs). Sa boîte des coups en partie FPS
/// (`netgame::hitbox`) et la visée des bots la suivent. Debout : (0, 1,8 − son côté).
pub fn head_place(crouch: f32) -> (f32, f32) {
    let n = crouch_lean(crouch).neck();
    (-n.z * PX, n.y * PX)
}

/// Pavés des sept parties, dans l'ordre de `parts` (jambe gauche, jambe droite, poitrine,
/// bras gauche, bras droit, tête, ventre) : coins (pixels, repère de la partie) et sorte.
/// (v256) Le ventre est ajouté à la fin : les six premiers numéros n'ont pas changé.
const PARTS: [([f32; 3], [f32; 3], Part); 7] = [
    (
        [-LEG_W / 2.0, -LEG, -LEG_D / 2.0],
        [LEG_W / 2.0, 0.0, LEG_D / 2.0],
        Part::Leg,
    ),
    (
        [-LEG_W / 2.0, -LEG, -LEG_D / 2.0],
        [LEG_W / 2.0, 0.0, LEG_D / 2.0],
        Part::Leg,
    ),
    (
        [-BODY_W / 2.0, BELLY_H, -BODY_D / 2.0],
        [BODY_W / 2.0, BODY_H, BODY_D / 2.0],
        Part::Body,
    ),
    (
        [-ARM_W / 2.0, -ARM_DOWN, -ARM_W / 2.0],
        [ARM_W / 2.0, ARM_UP, ARM_W / 2.0],
        Part::Arm,
    ),
    (
        [-ARM_W / 2.0, -ARM_DOWN, -ARM_W / 2.0],
        [ARM_W / 2.0, ARM_UP, ARM_W / 2.0],
        Part::Arm,
    ),
    (
        [-HEAD_S / 2.0, 0.0, -HEAD_S / 2.0],
        [HEAD_S / 2.0, HEAD_S, HEAD_S / 2.0],
        Part::Head,
    ),
    (
        [-BELLY_W / 2.0, 0.0, -BODY_D / 2.0],
        [BELLY_W / 2.0, BELLY_H, BODY_D / 2.0],
        Part::Body,
    ),
];
/// Numéros de la poitrine et du ventre dans `PARTS`.
const CHEST: usize = 2;
const BELLY: usize = 6;

/// (v256) Ce qu'un pavé montre du skin : la partie du skin (numéro de `Skin::color` : jambe
/// gauche, jambe droite, corps, bras gauche, bras droit, tête), la taille de sa zone entière
/// (largeur, hauteur, profondeur en pixels : les zones des skins 64 × 64), les pixels que le
/// pavé en montre (`size`), à partir de la colonne `col` (comptée vers +x, la droite du
/// personnage) et de la rangée `row` (depuis le haut). Les pixels montrés sont étirés à la
/// taille du pavé (v225) ; ceux d'un même corps ont tous la même taille.
#[derive(Clone, Copy, Debug)]
struct Zone {
    skin: usize,
    full: [usize; 3],
    size: [usize; 3],
    col: usize,
    row: usize,
}

impl Zone {
    const fn whole(skin: usize, full: [usize; 3]) -> Self {
        Zone {
            skin,
            full,
            size: full,
            col: 0,
            row: 0,
        }
    }

    /// Pixel (`u`, `v`) de la face `k` du pavé (ordre de `box_faces`) → pixel de la même
    /// face de la partie entière (ce que `Skin::color` attend).
    fn pixel(&self, k: usize, u: usize, v: usize) -> (usize, usize) {
        match k {
            // Devant : u va vers −x depuis le bord +x.
            0 => (self.full[0] - self.col - self.size[0] + u, self.row + v),
            1 => (self.col + u, self.row + v),
            2 | 3 => (u, self.row + v),
            _ => (self.col + u, v),
        }
    }

    /// Largeur de la face `k` de la partie entière (pixels).
    fn width(&self, k: usize) -> usize {
        if k == 2 || k == 3 {
            self.full[2]
        } else {
            self.full[0]
        }
    }
}

/// Zones du skin des sept pavés (ordre de `PARTS`). Jambes : trois colonnes sur quatre, sans
/// la colonne extérieure (la gauche est du côté −x). Poitrine : les 7 rangées du haut du
/// corps. Ventre : ses 5 rangées du bas, sans les colonnes du bord ; ses côtés montrent le
/// bas des côtés du corps. Dessous de la poitrine (les deux rebords qui dépassent du ventre) :
/// le dessous du corps du skin, entier ; dessous du ventre : ses 6 colonnes du milieu.
const ZONES: [Zone; 7] = [
    Zone {
        skin: 0,
        full: [4, 12, 4],
        size: [3, 12, 4],
        col: 1,
        row: 0,
    },
    Zone {
        skin: 1,
        full: [4, 12, 4],
        size: [3, 12, 4],
        col: 0,
        row: 0,
    },
    Zone {
        skin: 2,
        full: [8, 12, 4],
        size: [8, 12 - BELLY_ROWS, 4],
        col: 0,
        row: 0,
    },
    Zone::whole(3, [4, 12, 4]),
    Zone::whole(4, [4, 12, 4]),
    Zone::whole(5, [8, 8, 8]),
    Zone {
        skin: 2,
        full: [8, 12, 4],
        size: [6, BELLY_ROWS, 4],
        col: 1,
        row: 12 - BELLY_ROWS,
    },
];

/// Pavés du modèle dans cette pose : repère (pixels → monde), coins (pixels), partie.
fn parts(pose: &Pose) -> Vec<(Mat4, [f32; 3], [f32; 3], Part)> {
    let mut out = Vec::with_capacity(7);
    // Repère du modèle : x à droite, y en haut, z vers l'arrière (le regard est −z).
    let base = Mat4::from_translation(pose.feet)
        * Mat4::from_rotation_y(-pose.yaw)
        * Mat4::from_scale(Vec3::splat(PX));
    let swing = pose.walk.sin() * 0.75 * pose.swing;
    let bent = crouch_lean(pose.crouch);
    // `lean` : la penche de la poitrine, à laquelle tiennent les bras.
    let (lean, drop) = (bent.chest, bent.drop);
    // Jambes : pivot à la hanche (elle descend de `drop` accroupi), balancement opposé.
    // (v189) La jambe reste un seul pavé : elle se tasse de `drop` au lieu de se faire
    // recouvrir au quart par le bas du corps, qui laissait voir un pli à la hanche. Les
    // pieds restent au sol.
    let shrink = (LEG - drop) / LEG;
    for (side, sign) in [(-LEG_X, 1.0f32), (LEG_X, -1.0)] {
        let xf = base
            * Mat4::from_translation(Vec3::new(side, LEG - drop, 0.0))
            * Mat4::from_rotation_x(swing * sign)
            * Mat4::from_scale(Vec3::new(1.0, shrink, 1.0));
        let (min, max, part) = PARTS[out.len()];
        out.push((xf, min, max, part));
    }
    // Haut du corps : penché vers l'avant quand on s'accroupit (pivot à la hanche). (v260)
    // Le ventre se penche autour des hanches ; la poitrine, pliée un peu plus à la taille, a
    // son propre repère (`Crouch::chest_at`). Debout, les deux sont le même.
    // (v272) Plié en avant pour manger (`Pose::bow`) : tout le haut du corps tourne autour
    // des hanches.
    let up = if pose.bow == 0.0 {
        base
    } else {
        let hip = Vec3::new(0.0, LEG - drop, 0.0);
        base * Mat4::from_translation(hip)
            * Mat4::from_rotation_x(-pose.bow)
            * Mat4::from_translation(-hip)
    };
    let belly = up
        * Mat4::from_translation(Vec3::new(0.0, LEG - drop, 0.0))
        * Mat4::from_rotation_x(-bent.belly);
    let upper = up * Mat4::from_translation(bent.chest_at) * Mat4::from_rotation_x(-lean);
    let (min, max, part) = PARTS[CHEST];
    out.push((upper, min, max, part));
    // Bras : pivot à l'épaule (`SHOULDER_Y` au-dessus de la hanche), balancement inverse
    // des jambes ; le bras droit (x + `ARM_X`) tient l'objet un peu levé devant lui.
    let aim = aims(pose);
    let aim_right = aims_right(pose);
    for (side, sign) in [(-ARM_X, -1.0f32), (ARM_X, 1.0)] {
        if aim || (aim_right.is_some() && side > 0.0) {
            // (v177) Arme à feu : les deux bras tendus là où l'on regarde (le haut du corps
            // penché compris), un peu tournés vers l'arme, qui est tenue devant, entre les
            // deux mains ; ils ne se balancent presque plus en marchant. (v239) Un objet
            // tenu : le bras droit pointé un peu plus bas (`ITEM_AIM_DROP`).
            let turn = if side > 0.0 {
                AIM_TURN_RIGHT
            } else {
                AIM_TURN_LEFT
            };
            let down = if aim { 0.0 } else { aim_right.unwrap_or(0.0) };
            let xf = upper
                * Mat4::from_translation(Vec3::new(side, SHOULDER_Y, 0.0))
                * Mat4::from_rotation_y(turn)
                * Mat4::from_rotation_x(
                    std::f32::consts::FRAC_PI_2
                        + (pose.pitch.clamp(-1.4, 1.4) - down).max(-AIM_LOWEST)
                        + lean
                        + swing * sign * 0.1,
                );
            let (min, max, part) = PARTS[out.len()];
            out.push((xf, min, max, part));
            continue;
        }
        // Bras tendu vers l'avant (rotation positive : la main part vers −z), qui suit
        // un peu le regard (l'objet tenu est tourné comme à la 1re personne).
        let raise = if side > 0.0 && pose.held == Some(Held::Zeus) {
            // (v217) Foudre de Zeus : le bras levé devant soi, quel que soit le regard
            // (l'éclair reste debout, loin du corps : sa lumière, qui en part, garde
            // l'ombre du joueur entier).
            ZEUS_RAISE
        } else if side > 0.0 && pose.held.is_some() {
            (0.65 + 0.6 * pose.pitch.clamp(-1.4, 1.4)).clamp(0.0, 2.0)
        } else {
            0.0
        };
        // (v251) « Le mouvement du bras quand j'avance avec la lampe torche est mieux ; avec un
        // objet, on dirait que mon joueur tape » : le bras levé (Foudre de Zeus) se balance
        // aussi peu que le bras tendu de la lampe (0,1 ; 0,3 avant).
        let swing_k = if raise != 0.0 { RAISED_SWING } else { 1.0 };
        // (v256) Le bras qui pend est écarté du corps (`ARM_SPLAY`) : il ne le touche que par
        // le coin du haut de l'épaule. Pas celui qui tient un objet.
        let splay = if side > 0.0 && pose.held.is_some() {
            0.0
        } else {
            ARM_SPLAY * sign
        };
        let xf = upper
            * Mat4::from_translation(Vec3::new(side, SHOULDER_Y, 0.0))
            * Mat4::from_rotation_z(splay)
            * Mat4::from_rotation_x(swing * sign * swing_k + lean * 0.6 + raise);
        let (min, max, part) = PARTS[out.len()];
        out.push((xf, min, max, part));
    }
    // Tête : pivot au cou (v234 : au milieu du haut du corps, même penché ; elle avance avec
    // lui), suit le regard (tangage), sans suivre la penche du corps.
    let head = up
        * Mat4::from_translation(bent.neck())
        * Mat4::from_rotation_x(pose.bow + pose.pitch.clamp(-1.4, 1.4));
    let (min, max, part) = PARTS[5];
    out.push((head, min, max, part));
    // (v260) Ventre : sous la poitrine, penché autour des hanches.
    let (min, max, part) = PARTS[BELLY];
    out.push((belly, min, max, part));
    out
}

/// Morceaux qui font l'ombre du joueur : jambes, corps, bras, tête, chacun avec sa boîte
/// exacte (tournée avec lui). Les deux jambes ne font qu'une boîte quand elles sont
/// parallèles (joueur immobile).
pub fn shadow_boxes(pose: &Pose) -> Vec<ShadowBox> {
    let parts = parts(pose);
    let mut out: Vec<ShadowBox> = Vec::with_capacity(parts.len());
    let mut i = 0;
    while i < parts.len() {
        let (xf, mn, mx, part) = parts[i];
        let (mut lo, mut hi) = (Vec3::from(mn), Vec3::from(mx));
        if part == Part::Leg {
            if let Some(&(xf2, mn2, mx2, Part::Leg)) = parts.get(i + 1) {
                // L'autre jambe vue du repère de la première : une simple translation
                // quand elles sont parallèles.
                let rel = xf.inverse() * xf2;
                let parallel = [rel.x_axis, rel.y_axis, rel.z_axis]
                    .into_iter()
                    .zip([Vec4::X, Vec4::Y, Vec4::Z])
                    .all(|(a, b)| a.abs_diff_eq(b, 1e-4));
                if parallel {
                    lo = lo.min(rel.transform_point3(Vec3::from(mn2)));
                    hi = hi.max(rel.transform_point3(Vec3::from(mx2)));
                    i += 1;
                }
            }
        }
        out.push(ShadowBox::oriented(xf, lo, hi));
        i += 1;
    }
    out
}

/// Repère de la main droite (pixels → monde) : poing en y = 0, objet devant (−z).
fn hand_frame(pose: &Pose) -> Option<Mat4> {
    parts(pose)
        .into_iter()
        .rev()
        .find(|p| p.3 == Part::Arm)
        .map(|p| p.0 * Mat4::from_translation(Vec3::new(0.0, -ARM_DOWN, 0.0)))
}

/// Repère d'un objet tenu (pixels → monde) : au poing (`fwd` pixels devant), tourné
/// comme à la 1re personne par rapport au regard (`rot`, voir `hand::block_rotation`).
fn item_frame(pose: &Pose, hand: Mat4, rot: Mat4, fwd: f32) -> Mat4 {
    let at = hand.transform_point3(Vec3::new(0.0, 0.0, -fwd));
    Mat4::from_translation(at)
        * Mat4::from_rotation_y(-pose.yaw)
        * Mat4::from_rotation_x(pose.pitch.clamp(-1.4, 1.4))
        * rot
        * Mat4::from_scale(Vec3::splat(PX))
}

/// (v201) Repère d'une lampe torche tenue : au poing, pointée où l'on regarde (−z).
fn flashlight_frame(pose: &Pose, hand: Mat4) -> Mat4 {
    // (v264) Tenue 0,6 pixel sous l'axe du poing : le corps a grandi sous une tête plus
    // petite, l'épaule est montée de 0,06 bloc vers les yeux ; à la première personne, la
    // lampe montait devant le viseur.
    item_frame(pose, hand, Mat4::IDENTITY, 2.0)
        * Mat4::from_translation(Vec3::new(0.0, -FLASHLIGHT_LOW, 0.0))
}

/// (v264) De combien la lampe torche tenue est sous l'axe du poing (pixels du modèle).
const FLASHLIGHT_LOW: f32 = 0.6;

/// (v201) Point de la lampe debout (`flashlight::parts`, seizièmes de bloc) dans son repère
/// tenu (pixels) : couchée vers l'avant, prise au milieu du manche, à la moitié de sa taille
/// en bloc (comme à la première personne : environ 40 cm de long).
fn flashlight_lay(p: [f32; 3]) -> [f32; 3] {
    [(p[0] - 8.0) * 0.5, (p[2] - 8.0) * 0.5, -(p[1] - 4.0) * 0.5]
}

/// (v215) Ampoule d'une lampe torche tenue par le modèle (monde), d'où part sa lumière :
/// dans la tête de la lampe, au centre de sa base (là où serait l'ampoule d'une vraie
/// lampe ; lampe debout : tête de y = 9 à 13). La même pour soi (à la première personne
/// aussi, voir `flashlight_matrix`), dans les miroirs et pour les autres joueurs. `None`
/// sans bras droit.
pub fn flashlight_bulb(pose: &Pose) -> Option<Vec3> {
    Some(flashlight_matrix(pose)?.transform_point3(Vec3::new(0.5, 9.6 / 16.0, 0.5)))
}

/// (v215) Bras droit du modèle : de la boîte 0..1 du bras de la première personne (x vers
/// l'extérieur, y vers l'avant du bras, z = 1 à l'épaule et 0 au poing ; voir `hand::arm`)
/// au monde. Pour dessiner à la première personne le bras qui tient la lampe là où le
/// modèle le tient.
pub fn right_arm_matrix(pose: &Pose) -> Option<Mat4> {
    let (m, _, _, _) = parts(pose)
        .into_iter()
        .filter(|p| p.3 == Part::Arm)
        .nth(1)?;
    // Pavé du bras (pixels) : x de −`ARM_W`/2 à `ARM_W`/2, y de −`ARM_DOWN` (poing) à
    // `ARM_UP` (épaule), z de −`ARM_W`/2 (devant) à `ARM_W`/2.
    let unit = Mat4::from_cols(
        glam::Vec4::new(ARM_W, 0.0, 0.0, 0.0),
        glam::Vec4::new(0.0, 0.0, -ARM_W, 0.0),
        glam::Vec4::new(0.0, ARM_UP + ARM_DOWN, 0.0, 0.0),
        glam::Vec4::new(-ARM_W / 2.0, -ARM_DOWN, ARM_W / 2.0, 1.0),
    );
    Some(m * unit)
}

/// (v215) Repère de la lampe tenue par le modèle : de son repère de bloc (0..1, lampe
/// debout sur +y, comme `flashlight::model`) au monde. Sert à la dessiner, à la première
/// personne comme sur le modèle, et à placer son ampoule.
pub fn flashlight_matrix(pose: &Pose) -> Option<Mat4> {
    let frame = flashlight_frame(pose, hand_frame(pose)?);
    // `flashlight_lay` en matrice, sur des coordonnées de bloc (seizièmes / 16).
    let lay = Mat4::from_cols(
        glam::Vec4::new(8.0, 0.0, 0.0, 0.0),
        glam::Vec4::new(0.0, 0.0, -8.0, 0.0),
        glam::Vec4::new(0.0, 8.0, 0.0, 0.0),
        glam::Vec4::new(-4.0, -4.0, 2.0, 1.0),
    );
    Some(frame * lay)
}

/// Repère d'une torche tenue : un peu devant le poing, bâton sur +y.
fn torch_frame(pose: &Pose, hand: Mat4) -> Mat4 {
    item_frame(pose, hand, crate::hand::torch_rotation(), 3.0)
}

/// (v239) Baguette ou écran tenu (objet plat, une image 16 × 16 en relief) : de cette taille
/// (pixels du modèle), debout, centré sur la prise (plus haut, il cachait le viseur à la
/// première personne).
const FLAT_ITEM: f32 = 6.0;
/// (v239) Torche tenue : à cette part de la taille du monde (celle de la première personne
/// jusqu'ici), tenue par le haut de son bâton (le bas à `TORCH_LOW` pixels sous le poing).
/// Entière, ou tenue plus bas, sa flamme montait au-dessus du viseur à la première personne.
const HELD_TORCH: f32 = 0.5;
/// (v264) 5,4 (5 avant) : le corps a grandi sous une tête plus petite, l'épaule est montée
/// de 0,06 bloc vers les yeux et la flamme repassait d'un cheveu au-dessus du viseur.
const TORCH_LOW: f32 = 5.4;

/// (v239) Repère du maillage de l'objet tenu à la première personne (`hand::item_mesh`), au
/// monde : là où le modèle le tient, à sa taille (voir `held_parts`). Un bloc ou une tête : son
/// cube 0..1 ; une torche, la lampe torche : leur bloc 0..1 (debout) ; la baguette, l'écran :
/// leur image 0..1. `None` pour la Foudre de Zeus, l'équipement du mode FPS, la main vide.
pub fn held_item_matrix(pose: &Pose) -> Option<Mat4> {
    let hand = hand_frame(pose)?;
    match pose.held? {
        Held::Block(_) => Some(
            item_frame(pose, hand, crate::hand::block_rotation(), 0.0)
                * Mat4::from_translation(Vec3::new(-3.0, -5.0, -7.0))
                * Mat4::from_scale(Vec3::splat(6.0)),
        ),
        // Bâton de 2 × 8 seizièmes de bloc, à la moitié de la taille du monde (un bloc :
        // 32 / 1,8 pixels).
        Held::Torch(_) => Some(
            torch_frame(pose, hand)
                * Mat4::from_translation(Vec3::new(0.0, -TORCH_LOW, 0.0))
                * Mat4::from_scale(Vec3::splat(32.0 / 1.8 * HELD_TORCH))
                * Mat4::from_translation(Vec3::new(-0.5, 0.0, -0.5)),
        ),
        Held::Wand(_) => Some(
            item_frame(pose, hand, crate::hand::wand_rotation(), 2.0)
                * Mat4::from_translation(Vec3::new(-FLAT_ITEM / 2.0, -FLAT_ITEM / 2.0, 0.0))
                * Mat4::from_scale(Vec3::splat(FLAT_ITEM)),
        ),
        Held::Flashlight(_) => flashlight_matrix(pose),
        Held::Gear(_) | Held::Zeus => None,
    }
}

/// (v177) Équipement du mode FPS (`Held::Gear`) : son modèle en triangles (mètres, canon
/// ou lame vers −z), le point qu'on tient (là où est la main) et sa boîte englobante (de
/// loin, et dans les reflets).
struct Gear {
    tris: std::borrow::Cow<'static, [crate::weapons::Tri]>,
    grip: Vec3,
    min: Vec3,
    max: Vec3,
    /// Agrandissement : les petits objets (couteau, grenade, bombe) seraient cachés dans le
    /// poing, plus gros que nature.
    scale: f32,
}

impl Gear {
    /// `grip` : point tenu, en fractions de la boîte (0 : le bas, l'arrière… 1 : le haut,
    /// l'avant ; z de l'avant, le bout du canon, vers l'arrière).
    fn new(
        tris: std::borrow::Cow<'static, [crate::weapons::Tri]>,
        grip: [f32; 3],
        scale: f32,
    ) -> Self {
        let mut min = Vec3::splat(f32::MAX);
        let mut max = Vec3::splat(f32::MIN);
        for p in tris.iter().flat_map(|t| t.p) {
            min = min.min(p);
            max = max.max(p);
        }
        if tris.is_empty() {
            (min, max) = (Vec3::ZERO, Vec3::ZERO);
        }
        Gear {
            grip: min + (max - min) * Vec3::from(grip),
            tris,
            min,
            max,
            scale,
        }
    }
}

/// (v177) L'équipement de code `code` (voir `Held::Gear`), préparé une fois.
fn gear(code: u8) -> &'static Gear {
    use crate::weapons::{self, WEAPONS};
    use std::borrow::Cow;
    static ALL: std::sync::OnceLock<Vec<Gear>> = std::sync::OnceLock::new();
    let all = ALL.get_or_init(|| {
        let mut all = Vec::new();
        for (i, d) in WEAPONS.iter().enumerate() {
            let m = weapons::model(i);
            // Dual Berettas : le modèle porte les deux pistolets ; un seul, tenu à deux mains.
            let tris = if d.name == "Dual Berettas" {
                &m.tris[..m.tris.len() / 2]
            } else {
                &m.tris[..]
            };
            // Poignée : vers l'arrière et le bas (plus en arrière sur un pistolet, un peu
            // plus gros que nature pour se voir entre les poings).
            let (back, scale) = if d.length > 0.4 {
                (0.68, 1.0)
            } else {
                (0.8, 1.25)
            };
            all.push(Gear::new(Cow::Borrowed(tris), [0.5, 0.4, back], scale));
        }
        // Grenade, bombe, couteau : tenus par l'arrière, le reste sort du poing.
        for n in crate::equip::Nade::ALL {
            all.push(Gear::new(
                Cow::Owned(crate::equip::nade_model(n)),
                [0.5, 0.4, 0.8],
                2.0,
            ));
        }
        all.push(Gear::new(
            Cow::Owned(crate::equip::bomb_model()),
            [0.5, 0.4, 0.85],
            1.5,
        ));
        // Couteau : le manche (la lame est vers l'avant).
        all.push(Gear::new(
            Cow::Borrowed(&weapons::knife_model().tris[..]),
            [0.5, 0.5, 0.85],
            1.4,
        ));
        all
    });
    let n = WEAPONS.len();
    let i = if (code as usize) < n {
        code as usize
    } else if let Some(k) = crate::cs::nade_code(code) {
        n + k
    } else if code == crate::cs::W_BOMB {
        n + crate::equip::Nade::ALL.len()
    } else {
        all.len() - 1
    };
    &all[i]
}

/// (v177) Repère (mètres → monde) de l'équipement tenu : tourné vers où l'on regarde, son
/// point de prise posé sur les poings (entre les deux pour une arme à feu, tenue à deux
/// mains), comme à la 1re personne.
fn gear_frame(pose: &Pose) -> Option<(Mat4, &'static Gear)> {
    let Some(Held::Gear(code)) = pose.held else {
        return None;
    };
    let g = gear(code);
    let arms: Vec<Mat4> = parts(pose)
        .into_iter()
        .filter(|p| p.3 == Part::Arm)
        .map(|p| p.0)
        .collect();
    let look =
        Mat4::from_rotation_y(-pose.yaw) * Mat4::from_rotation_x(pose.pitch.clamp(-1.4, 1.4));
    let at = if aims(pose) {
        // Arme à feu : entre les deux poings (leur milieu, à 2 pixels du bout des bras),
        // posée dessus.
        let fist = |arm: &Mat4| arm.transform_point3(Vec3::new(0.0, 2.0 - ARM_DOWN, 0.0));
        (fist(arms.get(1)?) + fist(&arms[0])) * 0.5 + look.transform_vector3(Vec3::Y) * (2.0 * PX)
    } else {
        // Le reste : tenu au bout du poing droit.
        arms.get(1)?.transform_point3(Vec3::new(0.0, 0.5 - ARM_DOWN, 0.0))
    };
    Some((
        Mat4::from_translation(at)
            * look
            * Mat4::from_scale(Vec3::splat(g.scale))
            * Mat4::from_translation(-g.grip),
        g,
    ))
}

/// Couleur de la silhouette d'un équipement (de loin, et dans les reflets).
const GEAR_FAR: [u8; 3] = [40, 40, 44];

/// (v177) Équipement du mode FPS tenu en main : son modèle (`detail`), sinon sa silhouette
/// (une boîte sombre de sa taille, de loin).
fn emit_gear(out: &mut Vec<EntityVertex>, pose: &Pose, detail: bool) {
    let Some((xf, g)) = gear_frame(pose) else {
        return;
    };
    if detail {
        tris_in_world(out, xf, &g.tris, pose.light);
    } else {
        emit_item(
            out,
            xf,
            g.min.to_array(),
            g.max.to_array(),
            RayBoxKind::Flat(GEAR_FAR),
            pose.light,
        );
    }
}

/// Pavés de l'objet tenu : repère, coins (pixels), sorte.
fn held_parts(pose: &Pose) -> Vec<(Mat4, [f32; 3], [f32; 3], RayBoxKind)> {
    let (Some(hand), Some(held)) = (hand_frame(pose), pose.held) else {
        return Vec::new();
    };
    // Un bloc tenu fait 6 pixels de côté (≈ un tiers de bloc), posé devant le poing.
    let block_box = |a: [u8; 3], b: [u8; 3], dy: f32| {
        let k = 6.0 / 16.0;
        (
            [
                -3.0 + a[0] as f32 * k,
                -5.0 + dy + a[1] as f32 * k,
                -7.0 + a[2] as f32 * k,
            ],
            [
                -3.0 + b[0] as f32 * k,
                -5.0 + dy + b[1] as f32 * k,
                -7.0 + b[2] as f32 * k,
            ],
        )
    };
    match held {
        Held::Torch(flame) => {
            // Même modèle que la torche posée ou tenue : bâton texturé (2 × 8 seizièmes de
            // bloc) éclairé par sa flamme (3 × 5 seizièmes), (v239) à la moitié de la taille
            // du monde (`HELD_TORCH`).
            let k = 32.0 / 1.8 / 16.0 * HELD_TORCH;
            let top = flame.iter().copied().max().unwrap_or(1).max(1) as u32;
            let bright = flame.map(|c| (c as u32 * 255 / top) as u8);
            let t = torch_frame(pose, hand);
            vec![
                (
                    t,
                    [-k, -TORCH_LOW, -k],
                    [k, -TORCH_LOW + 8.0 * k, k],
                    RayBoxKind::Block {
                        layer: BlockId::TORCH.def().tex[1] as u32,
                        uv: [0.4375, 0.5, 0.5625, 1.0],
                        glow: true,
                    },
                ),
                (
                    t,
                    [-1.5 * k, -TORCH_LOW + 8.0 * k, -1.5 * k],
                    [1.5 * k, -TORCH_LOW + 13.0 * k, 1.5 * k],
                    RayBoxKind::Flame(bright),
                ),
            ]
        }
        Held::Block(block) => {
            let def = block.def();
            let t = def.tex;
            let boxes = if def.full {
                vec![([0u8; 3], [16u8; 3])]
            } else {
                def.boxes()
            };
            let frame = item_frame(pose, hand, crate::hand::block_rotation(), 0.0);
            let part = |a: [u8; 3], b: [u8; 3], dy: f32, layer: u16| {
                let (min, max) = block_box(a, b, dy);
                (
                    frame,
                    min,
                    max,
                    RayBoxKind::Block {
                        layer: layer as u32,
                        // Partie de la texture couverte par le morceau (vue de face).
                        uv: [
                            a[0].min(a[2]) as f32 / 16.0,
                            1.0 - b[1] as f32 / 16.0,
                            b[0].max(b[2]) as f32 / 16.0,
                            1.0 - a[1] as f32 / 16.0,
                        ],
                        glow: false,
                    },
                )
            };
            if def.key.starts_with("lantern") || def.key.starts_with("soul_lantern") {
                // Lanterne : corps et chapeau, taillés dans sa texture (sans chaîne).
                let piece = |a: [u8; 3], b: [u8; 3], uv: [f32; 4]| {
                    let (min, max) = block_box(a, b, 0.0);
                    (
                        frame,
                        min,
                        max,
                        RayBoxKind::Block {
                            layer: t[1] as u32,
                            uv: uv.map(|v| v / 16.0),
                            glow: true,
                        },
                    )
                };
                vec![
                    piece([5, 0, 5], [11, 7, 11], [0.0, 2.0, 6.0, 9.0]),
                    piece([6, 7, 6], [10, 9, 10], [1.0, 0.0, 5.0, 2.0]),
                ]
            } else if let Some((top, bottom)) = crate::hand::tall_layers(block) {
                // Bannière, porte : deux blocs de haut, comme posées (panneau fin, deux
                // moitiés).
                let (a, b) = boxes[0];
                vec![part(a, b, 0.0, top as u16), part(a, b, -6.0, bottom as u16)]
            } else {
                // Au plus deux morceaux (escalier) : le reste des pavés est pour le corps.
                boxes
                    .into_iter()
                    .take(2)
                    .map(|(a, b)| part(a, b, 0.0, t[1]))
                    .collect()
            }
        }
        // (v239) Une plaque, là où la première personne dessine son image (`held_item_matrix`) :
        // avant, un bâton de 11 pixels qu'on ne voyait qu'en F5.
        Held::Wand(c) => vec![(
            item_frame(pose, hand, crate::hand::wand_rotation(), 2.0),
            [-FLAT_ITEM / 2.0, -FLAT_ITEM / 2.0, -FLAT_ITEM / 32.0],
            [FLAT_ITEM / 2.0, FLAT_ITEM / 2.0, FLAT_ITEM / 32.0],
            RayBoxKind::Flat(c),
        )],
        // (v177) En triangles : `emit_gear`. (v216) Foudre de Zeus : pas de pavés.
        Held::Gear(_) | Held::Zeus => Vec::new(),
        // (v201) Lampe torche : dans le poing, pointée droit devant (−z), verre au bout.
        Held::Flashlight(on) => {
            let frame = flashlight_frame(pose, hand);
            crate::flashlight::parts(on)
                .into_iter()
                .map(|(a, b, c, glow)| {
                    let (a, b) = (flashlight_lay(a), flashlight_lay(b));
                    let min = [a[0].min(b[0]), a[1].min(b[1]), a[2].min(b[2])];
                    let max = [a[0].max(b[0]), a[1].max(b[1]), a[2].max(b[2])];
                    let kind = if glow {
                        RayBoxKind::Glow(c)
                    } else {
                        RayBoxKind::Flat(c)
                    };
                    (frame, min, max, kind)
                })
                .collect()
        }
    }
}

/// (v216) Repère de la Foudre de Zeus tenue par le modèle (blocs → monde) : le poing à
/// l'origine, l'éclair le long de +y. (v217) Debout (à la verticale du monde), quel que
/// soit le regard. `None` sans bras droit.
pub fn zeus_frame(pose: &Pose) -> Option<Mat4> {
    let at = hand_frame(pose)?.transform_point3(Vec3::new(0.0, 0.0, -2.0));
    Some(
        Mat4::from_translation(at)
            * Mat4::from_rotation_y(-pose.yaw)
            * Mat4::from_rotation_x(-ZEUS_HELD_TILT),
    )
}

/// (v244) Penché de la Foudre de Zeus tenue (rad, depuis la verticale, sa pointe vers
/// l'avant) : « allonge un peu l'éclair vers l'avant en main, qu'on voie son bout ; 45
/// degrés suffiraient ». Debout, sa pointe sortait de l'écran par le haut à la première
/// personne, et le petit éclair semblait partir de nulle part.
pub const ZEUS_HELD_TILT: f32 = std::f32::consts::FRAC_PI_4;

/// Position (monde) de la flamme d'une torche tenue en main : la lumière part de là.
pub fn torch_tip(pose: &Pose) -> Option<Vec3> {
    match (hand_frame(pose), pose.held) {
        // (v239) Au milieu de la flamme (la torche tenue est deux fois plus petite).
        (Some(hand), Some(Held::Torch(_))) => {
            let k = 32.0 / 1.8 / 16.0 * HELD_TORCH;
            Some(torch_frame(pose, hand).transform_point3(Vec3::new(0.0, -TORCH_LOW + 10.5 * k, 0.0)))
        }
        _ => None,
    }
}

/// Pavé de couleur unie (ou d'une texture de bloc par face) : objet tenu, vu en F5.
pub(crate) fn emit_item(
    out: &mut Vec<EntityVertex>,
    xf: Mat4,
    min: [f32; 3],
    max: [f32; 3],
    kind: RayBoxKind,
    light: [f32; 3],
) {
    let mut rect = [0.0, 0.0, 1.0, 1.0];
    let (color, layer, lit) = match kind {
        RayBoxKind::Block { layer, uv, glow } => {
            rect = uv;
            ([1.0; 3], layer as i32, !glow)
        }
        RayBoxKind::Flat(c) => (
            srgb((c[0] as u32) << 16 | (c[1] as u32) << 8 | c[2] as u32),
            -1,
            true,
        ),
        RayBoxKind::Glow(c) => (
            srgb((c[0] as u32) << 16 | (c[1] as u32) << 8 | c[2] as u32),
            -1,
            false,
        ),
        // Flamme animée (voir entity.wgsl) : `uv.y` = hauteur dans le bloc de la torche,
        // comme celle de la première personne (de 8 à 13 seizièmes).
        RayBoxKind::Flame(c) => (
            srgb((c[0] as u32) << 16 | (c[1] as u32) << 8 | c[2] as u32),
            minelib_render::ENTITY_FLAME,
            false,
        ),
        _ => ([1.0; 3], -1, true),
    };
    let (a, b) = (Vec3::from(min), Vec3::from(max));
    // (coin d'origine, axe u, axe v) de chaque face, en fractions du pavé.
    let faces: [([f32; 3], [f32; 3], [f32; 3]); 6] = [
        ([1.0, 1.0, 0.0], [-1.0, 0.0, 0.0], [0.0, -1.0, 0.0]),
        ([0.0, 1.0, 1.0], [1.0, 0.0, 0.0], [0.0, -1.0, 0.0]),
        ([1.0, 1.0, 1.0], [0.0, 0.0, -1.0], [0.0, -1.0, 0.0]),
        ([0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [0.0, -1.0, 0.0]),
        ([0.0, 1.0, 1.0], [1.0, 0.0, 0.0], [0.0, 0.0, -1.0]),
        ([0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]),
    ];
    for (o, du, dv) in faces {
        let (o, du, dv) = (Vec3::from(o), Vec3::from(du), Vec3::from(dv));
        let p = |u: f32, v: f32| {
            let f = o + du * u + dv * v;
            xf.transform_point3(a + (b - a) * f)
        };
        let n = xf.transform_vector3(du.cross(dv)).normalize_or_zero();
        let flame = layer == minelib_render::ENTITY_FLAME;
        // Flamme : sans ombrage de face, comme à la première personne.
        let shade = if flame { 1.0 } else { face_shade(n) };
        let col = [0, 1, 2].map(|i| color[i] * shade);
        // Lumineux par lui-même (flamme) : ni l'éclairage du lieu, ni les lumières. La
        // flamme animée garde sa pleine couleur (×1, comme à la première personne).
        let vl = if lit {
            EntityVertex::light(light, true)
        } else if flame {
            EntityVertex::light([1.0; 3], false)
        } else {
            EntityVertex::light([0.5; 3], false)
        };
        for (u, v) in [
            (0.0, 0.0),
            (1.0, 0.0),
            (1.0, 1.0),
            (1.0, 1.0),
            (0.0, 1.0),
            (0.0, 0.0),
        ] {
            let uv = if flame {
                let f = o + du * u + dv * v;
                [f.x, 0.5 + 0.3125 * f.y]
            } else {
                [
                    rect[0] + (rect[2] - rect[0]) * u,
                    rect[1] + (rect[3] - rect[1]) * v,
                ]
            };
            out.push(EntityVertex {
                pos: p(u, v).to_array(),
                color: col,
                uv,
                layer,
                light: vl,
            });
        }
    }
}

/// (v185) Bloc devenu objet (posé au sol) : sa vraie forme, comme tenu en main (pavés du
/// bloc, quatre au plus ; plante en croix ; torche : bâton et flamme ; lanterne : corps et
/// chapeau), chaque face avec sa texture (dessus, côtés, dessous) et la partie de l'image
/// que couvre le pavé. `xf` place le cube 0..1 du bloc dans le monde ; `light` : éclairage
/// du lieu (un bloc lumineux brille par lui-même).
pub fn emit_block_item(out: &mut Vec<EntityVertex>, xf: Mat4, block: BlockId, light: [f32; 3]) {
    use minelib_world::registry::Shape;
    let def = block.def();
    let glow = def.light.is_some();
    match def.shape {
        Shape::Torch | Shape::WallTorch(_) => {
            emit_torch_item(out, xf, minelib_world::flame::WOOD_FLAME, light);
            return;
        }
        Shape::Cross => {
            // Deux plans en diagonale, la texture des côtés (découpée : `entity.wgsl`).
            let vl = if glow {
                EntityVertex::light([0.5; 3], false)
            } else {
                EntityVertex::light(light, true)
            };
            for (a, b) in [
                (Vec3::new(0.0, 0.0, 0.0), Vec3::new(1.0, 0.0, 1.0)),
                (Vec3::new(1.0, 0.0, 0.0), Vec3::new(0.0, 0.0, 1.0)),
            ] {
                for (u, v) in [
                    (0.0, 0.0),
                    (1.0, 0.0),
                    (1.0, 1.0),
                    (1.0, 1.0),
                    (0.0, 1.0),
                    (0.0, 0.0),
                ] {
                    let p = a.lerp(b, u) + Vec3::Y * (1.0 - v);
                    out.push(EntityVertex {
                        pos: xf.transform_point3(p).to_array(),
                        color: [0.92; 3],
                        uv: [u, v],
                        layer: def.tex[1] as i32,
                        light: vl,
                    });
                }
            }
            return;
        }
        _ => {}
    }
    if def.key.starts_with("lantern") || def.key.starts_with("soul_lantern") {
        // Comme tenue : corps et chapeau, taillés dans sa texture (sans chaîne).
        for (a, b, uv) in [
            ([5.0, 0.0, 5.0], [11.0, 7.0, 11.0], [0.0, 2.0, 6.0, 9.0]),
            ([6.0, 7.0, 6.0], [10.0, 9.0, 10.0], [1.0, 0.0, 5.0, 2.0]),
        ] {
            emit_item(
                out,
                xf * Mat4::from_scale(Vec3::splat(1.0 / 16.0)),
                a,
                b,
                RayBoxKind::Block {
                    layer: def.tex[1] as u32,
                    uv: uv.map(|v: f32| v / 16.0),
                    glow: true,
                },
                light,
            );
        }
        return;
    }
    let boxes = if def.full {
        vec![([0u8; 3], [16u8; 3])]
    } else {
        def.boxes()
    };
    let boxes = if boxes.is_empty() {
        vec![([0u8; 3], [16u8; 3])]
    } else {
        boxes
    };
    for (a, b) in boxes.into_iter().take(4) {
        emit_textured_box(out, xf, a, b, def.tex, glow, light);
    }
}

/// (v201) Lampe torche (objet au sol) : `xf` place le repère du bloc (0..1, lampe debout sur
/// +y), comme celui de la vue à la première personne.
pub fn emit_flashlight_item(out: &mut Vec<EntityVertex>, xf: Mat4, on: bool, light: [f32; 3]) {
    let px = xf * Mat4::from_scale(Vec3::splat(1.0 / 16.0));
    for (a, b, c, glow) in crate::flashlight::parts(on) {
        let kind = if glow {
            RayBoxKind::Glow(c)
        } else {
            RayBoxKind::Flat(c)
        };
        emit_item(out, px, a, b, kind, light);
    }
}

/// (v185) Torche posée au sol comme objet : le même modèle que tenue en main (bâton
/// texturé de 2 × 8 seizièmes, flamme de 3 × 5 qui brille de sa couleur `flame`, sRGB),
/// debout au milieu du cube 0..1 que place `xf`.
pub fn emit_torch_item(out: &mut Vec<EntityVertex>, xf: Mat4, flame: [u8; 3], light: [f32; 3]) {
    let top = flame.iter().copied().max().unwrap_or(1).max(1) as u32;
    let bright = flame.map(|c| (c as u32 * 255 / top) as u8);
    let px = xf
        * Mat4::from_translation(Vec3::new(0.5, 0.0, 0.5))
        * Mat4::from_scale(Vec3::splat(1.0 / 16.0));
    emit_item(
        out,
        px,
        [-1.0, 0.0, -1.0],
        [1.0, 8.0, 1.0],
        RayBoxKind::Block {
            layer: BlockId::TORCH.def().tex[1] as u32,
            uv: [0.4375, 0.5, 0.5625, 1.0],
            glow: true,
        },
        light,
    );
    emit_item(
        out,
        px,
        [-1.5, 8.0, -1.5],
        [1.5, 13.0, 1.5],
        RayBoxKind::Flame(bright),
        light,
    );
}

/// (v185) Pavé `a`..`b` (seizièmes) d'un bloc, ses faces texturées comme posé : dessus
/// `tex[0]`, côtés `tex[1]`, dessous `tex[2]`, chaque face prenant la partie de l'image
/// qu'elle couvre ; ombrée selon son orientation dans le monde.
fn emit_textured_box(
    out: &mut Vec<EntityVertex>,
    xf: Mat4,
    a: [u8; 3],
    b: [u8; 3],
    tex: [u16; 3],
    glow: bool,
    light: [f32; 3],
) {
    let mn = Vec3::from(a.map(|v| v as f32 / 16.0));
    let mx = Vec3::from(b.map(|v| v as f32 / 16.0));
    let vl = if glow {
        EntityVertex::light([0.5; 3], false)
    } else {
        EntityVertex::light(light, true)
    };
    // (coin d'origine, axe u, axe v : fractions du pavé ; normale ; couche ; [u0, v0, u1, v1]).
    #[allow(clippy::type_complexity)]
    let faces: [([f32; 3], [f32; 3], [f32; 3], Vec3, u16, [f32; 4]); 6] = [
        (
            [1.0, 1.0, 0.0],
            [-1.0, 0.0, 0.0],
            [0.0, -1.0, 0.0],
            Vec3::NEG_Z,
            tex[1],
            [1.0 - mx.x, 1.0 - mx.y, 1.0 - mn.x, 1.0 - mn.y],
        ),
        (
            [0.0, 1.0, 1.0],
            [1.0, 0.0, 0.0],
            [0.0, -1.0, 0.0],
            Vec3::Z,
            tex[1],
            [mn.x, 1.0 - mx.y, mx.x, 1.0 - mn.y],
        ),
        (
            [1.0, 1.0, 1.0],
            [0.0, 0.0, -1.0],
            [0.0, -1.0, 0.0],
            Vec3::X,
            tex[1],
            [1.0 - mx.z, 1.0 - mx.y, 1.0 - mn.z, 1.0 - mn.y],
        ),
        (
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
            [0.0, -1.0, 0.0],
            Vec3::NEG_X,
            tex[1],
            [mn.z, 1.0 - mx.y, mx.z, 1.0 - mn.y],
        ),
        (
            [0.0, 1.0, 1.0],
            [1.0, 0.0, 0.0],
            [0.0, 0.0, -1.0],
            Vec3::Y,
            tex[0],
            [mn.x, mx.z, mx.x, mn.z],
        ),
        (
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0],
            Vec3::NEG_Y,
            tex[2],
            [mn.x, mn.z, mx.x, mx.z],
        ),
    ];
    for (o, du, dv, n, layer, r) in faces {
        let (o, du, dv) = (Vec3::from(o), Vec3::from(du), Vec3::from(dv));
        let shade = face_shade(xf.transform_vector3(n).normalize_or_zero());
        for (u, v) in [
            (0.0, 0.0),
            (1.0, 0.0),
            (1.0, 1.0),
            (1.0, 1.0),
            (0.0, 1.0),
            (0.0, 0.0),
        ] {
            let f = o + du * u + dv * v;
            out.push(EntityVertex {
                pos: xf.transform_point3(mn + (mx - mn) * f).to_array(),
                color: [shade; 3],
                uv: [r[0] + (r[2] - r[0]) * u, r[1] + (r[3] - r[1]) * v],
                layer: layer as i32,
                light: vl,
            });
        }
    }
}

/// Tête seule (bloc « tête » à skin d'une carte importée) : `xf` va des pixels du modèle
/// (tête de -4..4 × 0..8 × -4..4, visage vers −z) au monde.
pub fn head(skin: &crate::skin::Skin, xf: Mat4, light: [f32; 3], lod: usize) -> Vec<EntityVertex> {
    let mut out = Vec::with_capacity(2400 / (lod * lod).max(1));
    // Exactement à la place de la hitbox du bloc « tête » : un seul pavé, le calque du
    // dessus posé sur le dessous ; les pixels transparents des deux restent vides.
    // `lod` : 1 de près ; 2, 4 de loin (carrés de 2 × 2 ou 4 × 4 pixels, moins de triangles).
    emit_box_layer(
        &mut out,
        xf,
        [-4.0, 0.0, -4.0],
        [4.0, 8.0, 4.0],
        None,
        Part::Head,
        light,
        Some((skin, 5)),
        Some(false),
        lod,
    );
    out
}

/// Tête au skin par défaut de Minelib (bloc « tête », ou texture de la tête pas encore
/// là) ; `lod` comme `head`.
pub fn default_head(xf: Mat4, light: [f32; 3], lod: usize) -> Vec<EntityVertex> {
    let mut out = Vec::with_capacity(2400 / (lod * lod).max(1));
    emit_box_layer(
        &mut out,
        xf,
        [-4.0, 0.0, -4.0],
        [4.0, 8.0, 4.0],
        None,
        Part::Head,
        light,
        None,
        None,
        lod,
    );
    out
}

/// Joueur au-delà de ceux que la carte dessine (`MAX_PLAYERS`) : trois pavés (jambes, corps
/// avec les bras, tête).
pub fn build_tiny(pose: &Pose) -> Vec<EntityVertex> {
    let mut out = Vec::with_capacity(108);
    let base = Mat4::from_translation(pose.feet)
        * Mat4::from_rotation_y(-pose.yaw)
        * Mat4::from_scale(Vec3::splat(PX));
    // (v225) Aux proportions du modèle : jambes, corps et bras, tête.
    let wide = BODY_W / 2.0 + ARM_W;
    let legs = LEG_X + LEG_W / 2.0;
    for (min, max, c) in [
        ([-legs, 0.0, -LEG_D / 2.0], [legs, LEG, LEG_D / 2.0], PANTS),
        ([-wide, LEG, -BODY_D / 2.0], [wide, NECK_Y, BODY_D / 2.0], SHIRT),
        (
            [-HEAD_S / 2.0, NECK_Y, -HEAD_S / 2.0],
            [HEAD_S / 2.0, NECK_Y + HEAD_S, HEAD_S / 2.0],
            SKIN,
        ),
    ] {
        let rgb = [(c >> 16) as u8, (c >> 8) as u8, c as u8];
        emit_item(&mut out, base, min, max, RayBoxKind::Flat(rgb), pose.light);
    }
    out
}

/// Triangles du modèle du joueur dans cette pose (avec l'objet tenu).
pub fn build(pose: &Pose) -> Vec<EntityVertex> {
    build_bitten(pose, None)
}

/// (v269) Comme `build`, avec ce que L'Évidé lui a mangé (`flesh`) : les morceaux qui manquent
/// et, dans les creux, ses os et ses organes.
pub fn build_bitten(pose: &Pose, flesh: Option<&crate::flesh::Flesh>) -> Vec<EntityVertex> {
    let mut out = Vec::with_capacity(12_000);
    // Ordre de `parts` : jambe gauche, jambe droite, poitrine, bras gauche, bras droit,
    // tête, ventre ; `ZONES` dit ce que chacun montre du skin.
    emit_body(&mut out, pose, flesh, &[]);
    for (xf, min, max, kind) in held_parts(pose) {
        emit_item(&mut out, xf, min, max, kind, pose.light);
    }
    emit_gear(&mut out, pose, true);
    out
}

// (v172) Les autres joueurs sont dessinés par la carte graphique, tous d'un coup : un seul
// modèle (les six pavés, une face = deux triangles), posé par instance (`instance`) comme
// `parts` le fait, avec l'image de leur skin cuite par `bake_skin`. Le résultat est celui de
// `build` (les mêmes pixels, le même ombrage des faces, la même lumière).

/// Taille d'un pavé en pixels du skin.
fn box_size(min: [f32; 3], max: [f32; 3]) -> [usize; 3] {
    [0, 1, 2].map(|i| (max[i] - min[i]).round() as usize)
}

/// Coin en haut à gauche (pixels) de la face `k` de la partie `i` dans l'image cuite d'un
/// skin (la disposition des skins : chaque partie a sa zone, sans recouvrement).
fn atlas(i: usize, k: usize, [w, _, d]: [usize; 3]) -> (usize, usize) {
    let (bx, by) = [(16, 48), (0, 16), (16, 16), (32, 48), (40, 16), (0, 0)][i];
    match k {
        0 => (bx + d, by + d),
        1 => (bx + 2 * d + w, by + d),
        2 => (bx, by + d),
        3 => (bx + d + w, by + d),
        4 => (bx + d, by),
        _ => (bx + d + w, by),
    }
}

/// Modèle commun des autres joueurs (sept pavés : 252 sommets), à envoyer une fois à la
/// carte.
pub fn player_mesh() -> Vec<PlayerVertex> {
    let mut out = Vec::with_capacity(252);
    for (i, &(min, max, _)) in PARTS.iter().enumerate() {
        // (v225) Les pixels de sa zone du skin, étirés à la taille du pavé. (v256) Un pavé
        // peut n'en montrer qu'un morceau (`Zone`).
        let zone = ZONES[i];
        let size = zone.size;
        let step = (Vec3::from(max) - Vec3::from(min)) / Vec3::from(size.map(|n| n as f32));
        for (k, (_, o, du, dv, nu, nv)) in box_faces(min, max, size).into_iter().enumerate() {
            let (o, du, dv) = (
                Vec3::from(o),
                Vec3::from(du) * step,
                Vec3::from(dv) * step,
            );
            let (ax, ay) = atlas(zone.skin, k, zone.full);
            let (ou, ov) = zone.pixel(k, 0, 0);
            let (fx, fy) = (ax + ou, ay + ov);
            let rect = [
                fx as f32 / 64.0,
                fy as f32 / 64.0,
                (fx + nu) as f32 / 64.0,
                (fy + nv) as f32 / 64.0,
            ];
            let normal = du.cross(dv).normalize().to_array();
            let corner = |u: usize, v: usize| PlayerVertex {
                pos: (o + du * u as f32 + dv * v as f32).to_array(),
                uv: [(fx + u) as f32 / 64.0, (fy + v) as f32 / 64.0],
                part: i as u32,
                normal,
                rect,
            };
            let (a, b, c, d) = (corner(0, 0), corner(nu, 0), corner(nu, nv), corner(0, nv));
            // (v201) Triangles tournés vers le dehors du pavé, comme ceux du monde : la carte
            // écarte les faces arrière des joueurs depuis la v197. `du × dv` (la normale de
            // l'ombrage, gardée telle quelle) pointe vers l'intérieur pour quatre faces sur
            // six (devant, derrière, côtés) : on voyait l'intérieur des pavés.
            let center = (Vec3::from(min) + Vec3::from(max)) / 2.0;
            let mid = o + (du * nu as f32 + dv * nv as f32) / 2.0;
            if du.cross(dv).dot(mid - center) > 0.0 {
                out.extend([a, b, c, c, d, a]);
            } else {
                out.extend([a, d, c, c, b, a]);
            }
        }
    }
    out
}

/// Image d'un skin pour la carte (64 × 64 couleurs linéaires) : chaque pixel de chaque face
/// à sa couleur dans `build` (le skin, sinon le modèle par défaut ; grain compris, sans
/// l'ombrage de la face ni la lumière), à sa place dans `player_mesh`.
pub fn bake_skin(tex: Option<&crate::skin::Skin>) -> Vec<[f32; 3]> {
    let mut out = vec![[0.0; 3]; 64 * 64];
    for (i, &(min, max, part)) in PARTS.iter().enumerate() {
        let zone = ZONES[i];
        let s = zone.full.map(|n| n as u32);
        for (k, (face, _, _, _, nu, nv)) in
            box_faces(min, max, zone.size).into_iter().enumerate()
        {
            let (fx, fy) = atlas(zone.skin, k, zone.full);
            for v in 0..nv {
                for u in 0..nu {
                    // (v256) À sa place dans la zone de la partie entière : la poitrine et
                    // le ventre se partagent celle du corps.
                    let (su, sv) = zone.pixel(k, u, v);
                    let rgb = tex
                        .and_then(|t| t.color(zone.skin, k, su as u32, sv as u32, s))
                        .unwrap_or_else(|| skin(part, face, su, sv, zone.width(k)));
                    let g = grain(k * 4096 + sv * 64 + su + part as usize * 977);
                    out[(fy + sv) * 64 + fx + su] = srgb(rgb).map(|c| c * g);
                }
            }
        }
    }
    out
}

/// (v189) Pose d'un joueur à l'image d'avant : l'anticrénelage temporel en a besoin pour
/// retrouver l'historique du bon pixel (voir `PlayerInstance`). Sa pose d'aujourd'hui s'il
/// vient d'apparaître : il n'a pas d'historique à retrouver.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Prev {
    pub feet: Vec3,
    pub yaw: f32,
    pub pitch: f32,
    pub walk: f32,
    pub swing: f32,
    pub crouch: f32,
}

impl Prev {
    /// La même pose qu'aujourd'hui (aucun mouvement à l'écran).
    pub fn still(pose: &Pose) -> Self {
        Prev {
            feet: pose.feet,
            yaw: pose.yaw,
            pitch: pose.pitch,
            walk: pose.walk,
            swing: pose.swing,
            crouch: pose.crouch,
        }
    }
}

/// Instance d'un autre joueur pour la carte : sa pose, sa lumière et la couche de son skin.
pub fn instance(pose: &Pose, layer: u32, prev: &Prev) -> PlayerInstance {
    let held = if aims(pose) {
        // (v177) Arme à feu : les deux bras tendus.
        PlayerInstance::HOLDS | PlayerInstance::AIMS
    } else if aims_right(pose) == Some(0.0) {
        // (v215) Lampe torche : le bras droit tendu.
        PlayerInstance::HOLDS | PlayerInstance::AIMS_RIGHT
    } else if aims_right(pose).is_some() {
        // (v239) Objet tenu en construction : le bras droit tendu, pointé plus bas.
        PlayerInstance::HOLDS | PlayerInstance::AIMS_RIGHT | PlayerInstance::AIMS_LOW
    } else if pose.held == Some(Held::Zeus) {
        // (v225) Foudre de Zeus : le bras droit levé, quel que soit le regard.
        PlayerInstance::HOLDS | PlayerInstance::RAISED
    } else if pose.held.is_some() {
        PlayerInstance::HOLDS
    } else {
        0
    };
    PlayerInstance {
        feet: pose.feet.to_array(),
        yaw: pose.yaw,
        pitch: pose.pitch,
        walk: pose.walk,
        swing: pose.swing,
        crouch: pose.crouch,
        light: pose.light.map(|c| c.clamp(0.0, 2.0)),
        skin: (layer & 0xffff) | held,
        prev_feet: prev.feet.to_array(),
        prev_yaw: prev.yaw,
        prev_pitch: prev.pitch,
        prev_walk: prev.walk,
        prev_swing: prev.swing,
        prev_crouch: prev.crouch,
    }
}

/// Couches de skins des autres joueurs sur la carte : 0 pour le modèle par défaut, les
/// autres données aux skins vus (le moins récemment vu cède sa place).
pub struct SkinLayers {
    /// Couche 1 + i : skin (gardé : son adresse ne peut pas resservir) et dernière image où
    /// il a servi.
    slots: Vec<Option<(std::sync::Arc<crate::skin::Skin>, u64)>>,
    /// Adresse du skin → place dans `slots`.
    index: std::collections::HashMap<usize, usize>,
    frame: u64,
}

impl Default for SkinLayers {
    fn default() -> Self {
        Self {
            slots: vec![None; minelib_render::PLAYER_SKINS as usize - 1],
            index: Default::default(),
            frame: 0,
        }
    }
}

impl SkinLayers {
    /// À chaque image, avant `layer` : le modèle commun et le skin par défaut sont envoyés
    /// à la carte (la première fois, et à un nouveau renderer, qui n'a encore rien).
    pub fn begin(&mut self, renderer: &mut minelib_render::Renderer) {
        self.frame += 1;
        if !renderer.has_player_mesh() {
            *self = Self {
                frame: self.frame,
                ..Self::default()
            };
            renderer.set_player_skin(0, &bake_skin(None));
            renderer.set_player_mesh(&player_mesh());
        }
    }

    /// Couche du skin d'un joueur (cuit et envoyé à la carte la première fois) ; 0 (modèle
    /// par défaut) sans skin, ou si toutes les couches servent déjà dans cette image.
    pub fn layer(
        &mut self,
        skin: Option<&std::sync::Arc<crate::skin::Skin>>,
        renderer: &mut minelib_render::Renderer,
    ) -> u32 {
        let Some(skin) = skin else {
            return 0;
        };
        let key = std::sync::Arc::as_ptr(skin) as usize;
        if let Some(&i) = self.index.get(&key) {
            if let Some(slot) = &mut self.slots[i] {
                slot.1 = self.frame;
            }
            return i as u32 + 1;
        }
        let frame = self.frame;
        let free = self.slots.iter().position(Option::is_none).or_else(|| {
            self.slots
                .iter()
                .enumerate()
                .filter_map(|(i, s)| s.as_ref().map(|s| (i, s.1)))
                .filter(|&(_, last)| last != frame)
                .min_by_key(|&(_, last)| last)
                .map(|(i, _)| i)
        });
        let Some(i) = free else {
            return 0;
        };
        if let Some((old, _)) = self.slots[i].take() {
            self.index.remove(&(std::sync::Arc::as_ptr(&old) as usize));
        }
        renderer.set_player_skin(i as u32 + 1, &bake_skin(Some(skin)));
        self.slots[i] = Some((skin.clone(), frame));
        self.index.insert(key, i);
        i as u32 + 1
    }
}

/// Objet tenu seul (le corps est dessiné par la carte : `instance`). (v177) `detail` :
/// l'équipement du mode FPS avec son vrai modèle (sinon sa silhouette, de loin).
pub fn held_model(pose: &Pose, detail: bool) -> Vec<EntityVertex> {
    let mut out = Vec::new();
    for (xf, min, max, kind) in held_parts(pose) {
        emit_item(&mut out, xf, min, max, kind, pose.light);
    }
    emit_gear(&mut out, pose, detail);
    out
}

/// Pavés du joueur (et de l'objet tenu) pour les reflets du raytracing.
pub fn ray_boxes(pose: &Pose) -> Vec<RayBox> {
    // Avec un skin : chaque partie le lit (numéro de `Skin::color`, plus un).
    let skinned = pose.skin.is_some();
    let body = parts(pose)
        .into_iter()
        .enumerate()
        .map(|(i, (xf, min, max, part))| {
            let kind = match part {
                Part::Head => RayBoxKind::Head,
                Part::Body => RayBoxKind::Body,
                Part::Arm => RayBoxKind::Arm,
                Part::Leg => RayBoxKind::Leg,
            };
            // (v256) Le repère du pavé est celui des pixels du skin de sa partie entière (x :
            // colonnes vers +x, y : rangées depuis le bas, z : de l'avant vers l'arrière) ;
            // le pavé en est le morceau qu'il montre (`Zone`). Avant, c'étaient les pixels du
            // modèle, qui ne sont plus ceux du skin depuis la v225.
            let zone = ZONES[i];
            let (lo, hi) = (Vec3::from(min), Vec3::from(max));
            let n = Vec3::from(zone.size.map(|v| v as f32));
            let full = zone.full.map(|v| v as f32);
            let at = Vec3::new(zone.col as f32, full[1] - zone.row as f32 - n.y, 0.0);
            let to_skin = Mat4::from_translation(at)
                * Mat4::from_scale(n / (hi - lo))
                * Mat4::from_translation(-lo);
            RayBox {
                world_to_local: to_skin * xf.inverse(),
                min: at.to_array(),
                max: (at + n).to_array(),
                kind,
                part: if skinned { zone.skin as u8 + 1 } else { 0 },
                full,
            }
        });
    // (v177) Équipement du mode FPS : sa boîte, sombre.
    let gear = gear_frame(pose).map(|(xf, g)| {
        (
            xf,
            g.min.to_array(),
            g.max.to_array(),
            RayBoxKind::Flat(GEAR_FAR),
        )
    });
    body.chain(
        held_parts(pose)
            .into_iter()
            .chain(gear)
            .map(|(xf, min, max, kind)| RayBox {
                world_to_local: xf.inverse(),
                min,
                max,
                kind,
                part: 0,
                full: [0.0; 3],
            }),
    )
    .collect()
}

/// (v256) Milieu de la poitrine du modèle (monde), que visent les bots : sur l'axe du corps,
/// aux six dixièmes de sa hauteur (dans la poitrine, sous les bras tendus), penché avec lui
/// accroupi. Avant, un point à la verticale des pieds : accroupi, il tombait derrière le dos.
pub fn chest_point(feet: Vec3, yaw: f32, crouch: f32) -> Vec3 {
    let bent = crouch_lean(crouch);
    let upper = Mat4::from_translation(feet)
        * Mat4::from_rotation_y(-yaw)
        * Mat4::from_scale(Vec3::splat(PX))
        * Mat4::from_translation(bent.chest_at)
        * Mat4::from_rotation_x(-bent.chest);
    upper.transform_point3(Vec3::new(0.0, BODY_H * 0.6, 0.0))
}

/// (v267) « On doit voir notre corps dans le jeu, pour voir les traces de morsure et car
/// j'aime ; pas en mode FPS » : de combien le corps vu de ses propres yeux est reculé (blocs).
/// La caméra est à la verticale des pieds, au milieu du corps ; de vrais yeux sont sur le
/// visage, devant. Reculé d'autant, le corps se voit comme on voit le sien en baissant les
/// yeux (la poitrine, le ventre, les jambes, les pieds), sans que la caméra soit dedans.
pub const FIRST_PERSON_BACK: f32 = 0.15;
/// (v267) Recul au plus (accroupi, la tête du modèle avance de 0,32 bloc : le corps suivrait
/// d'autant ; au-delà de la demi-largeur du joueur, il entrerait dans le mur derrière lui).
const FIRST_PERSON_BACK_MAX: f32 = 0.3;
/// (v267) Le regard doit être baissé d'au moins cet angle (radians) pour qu'un morceau du
/// corps entre dans l'image : au-dessus, rien n'est construit.
pub const FIRST_PERSON_PITCH: f32 = -0.15;

/// (v267) Le corps du joueur vu de ses propres yeux, à dessiner avec son bras de la première
/// personne (mêmes sommets, même éclairage : lumière du lieu et lumières dynamiques) : les
/// jambes, le ventre, la poitrine et le bras gauche du modèle, pixel par pixel de son skin,
/// avec ses plaies. Ni la tête (la caméra y est), ni le bras droit (il est dessiné tendu, avec
/// ce qu'il tient : `right_arm_matrix`). `view_proj` : du monde à l'écran.
pub fn first_person_body(
    pose: &Pose,
    wounds: &[Wound],
    flesh: Option<&crate::flesh::Flesh>,
    view_proj: Mat4,
) -> Vec<ArmVertex> {
    let (ahead, _) = head_place(pose.crouch);
    let back = (ahead + FIRST_PERSON_BACK).min(FIRST_PERSON_BACK_MAX);
    let seen = Pose {
        feet: pose.feet - Vec3::new(pose.yaw.sin(), 0.0, -pose.yaw.cos()) * back,
        yaw: pose.yaw,
        pitch: pose.pitch,
        walk: pose.walk,
        swing: pose.swing,
        crouch: pose.crouch,
        light: pose.light,
        held: pose.held,
        skin: pose.skin.clone(),
        bow: pose.bow,
    };
    let mut out = Vec::with_capacity(6400);
    let mut quads = Vec::with_capacity(1600);
    for (i, (xf, min, max, part)) in parts(&seen).into_iter().enumerate() {
        if i == 4 || i == 5 {
            continue;
        }
        quads.clear();
        // (v269) Un pavé mordu : cube par cube, avec ce qu'on voit dans le creux.
        match flesh.filter(|f| f.bitten(i)) {
            Some(f) => emit_flesh(
                &mut quads,
                xf,
                min,
                max,
                i,
                part,
                seen.light,
                seen.skin.as_deref(),
                f,
                crate::flesh::organ,
            ),
            None => {
                let tex = seen.skin.as_deref();
                emit_box(&mut quads, xf, min, max, ZONES[i], part, seen.light, tex)
            }
        }
        for w in wounds.iter().filter(|w| w.part as usize == i) {
            quads.extend(wound_model(&seen, std::slice::from_ref(w)));
        }
        // La normale de chaque carré, vers le dehors du pavé (pour les lumières).
        let center = xf.transform_point3((Vec3::from(min) + Vec3::from(max)) / 2.0);
        for q in quads.chunks_exact(6) {
            let (a, b, c) = (Vec3::from(q[0].pos), Vec3::from(q[1].pos), Vec3::from(q[2].pos));
            let mut n = (b - a).cross(c - a).normalize_or_zero();
            if n.dot((a + c) / 2.0 - center) < 0.0 {
                n = -n;
            }
            for v in q {
                out.push(ArmVertex {
                    clip: (view_proj * Vec3::from(v.pos).extend(1.0)).to_array(),
                    world: v.pos,
                    normal: n.to_array(),
                    albedo: v.color,
                    light: seen.light,
                });
            }
        }
    }
    out
}

/// (v264) Plaie qu'un coup laisse sur le corps d'un joueur en partie FPS (« un impact dans le
/// joueur ») : le pavé touché (numéro de `parts`), sa face (ordre de `box_faces`), la place de
/// la plaie sur cette face (pixels du pavé : x ou z, puis y ou z) et sa graine (chaque plaie a
/// sa forme). `knife` : une entaille de lame, sinon un trou de balle.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Wound {
    part: u8,
    face: u8,
    at: [f32; 2],
    seed: u32,
    knife: bool,
}

/// Axes d'une face de pavé (ordre de `box_faces` : devant, derrière, côté +x, côté −x,
/// dessus, dessous) : l'axe qui sort de la face (0 x, 1 y, 2 z) et son sens, puis les deux
/// axes dans la face (le second : la hauteur pour les quatre faces debout).
const WOUND_FACES: [(usize, f32, usize, usize); 6] = [
    (2, -1.0, 0, 1),
    (2, 1.0, 0, 1),
    (0, 1.0, 2, 1),
    (0, -1.0, 2, 1),
    (1, 1.0, 0, 2),
    (1, -1.0, 0, 2),
];

/// Côté d'un carré de plaie (pixels du modèle) : un demi-pixel du skin (2,4 cm).
const WOUND_CELL: f32 = COL * 0.5;

/// (v264) Plaie au point `hit` (monde) du joueur dans la pose `pose`, touché depuis `from` :
/// sur le pavé du modèle le plus proche du point, sur sa face tournée vers le tireur. Le
/// point vient de l'hôte, compté sur le joueur tel qu'il était au moment du tir : `None` s'il
/// est maintenant à plus de 0,6 bloc de tous ses pavés.
pub fn wound_at(pose: &Pose, hit: Vec3, from: Vec3, knife: bool, seed: u32) -> Option<Wound> {
    let mut best: Option<(f32, usize, Mat4, Vec3)> = None;
    for (i, (xf, min, max, _)) in parts(pose).into_iter().enumerate() {
        let inv = xf.inverse();
        let c = inv
            .transform_point3(hit)
            .clamp(Vec3::from(min), Vec3::from(max));
        let d = xf.transform_point3(c).distance(hit);
        if best.as_ref().is_none_or(|b| d < b.0) {
            best = Some((d, i, inv, c));
        }
    }
    let (d, part, inv, c) = best?;
    if d > 0.6 {
        return None;
    }
    let (min, max) = (Vec3::from(PARTS[part].0), Vec3::from(PARTS[part].1));
    // Vers le tireur, dans le repère du pavé.
    let to = inv.transform_vector3(from - hit).normalize_or_zero();
    let (face, _) = WOUND_FACES
        .iter()
        .enumerate()
        .map(|(f, &(axis, sign, _, _))| {
            // La face qui regarde le tireur, de préférence celle dont le point est proche.
            let plane = if sign > 0.0 { max[axis] } else { min[axis] };
            (f, to[axis] * sign - 0.2 * (c[axis] - plane).abs())
        })
        .max_by(|a, b| a.1.total_cmp(&b.1))?;
    let (_, _, a, b) = WOUND_FACES[face];
    Some(Wound {
        part: part as u8,
        face: face as u8,
        at: [c[a], c[b]],
        seed,
        knife,
    })
}

/// (v264) Essais (MINELIB_PLAIES) : des plaies posées d'avance sur le joueur local, de face
/// et de dos (poitrine, ventre, jambes, bras, tête ; une entaille).
pub fn test_wounds() -> Vec<Wound> {
    let w = |part: usize, face: u8, a: f32, b: f32, seed: u32, knife: bool| Wound {
        part: part as u8,
        face,
        at: [a, b],
        seed,
        knife,
    };
    vec![
        w(CHEST, 0, 1.2, BODY_H - 3.0, 11, false),
        w(CHEST, 0, -1.8, BODY_H - 5.2, 23, true),
        w(BELLY, 0, -0.6, 3.0, 37, false),
        w(0, 0, 0.2, -5.0, 41, false),
        w(1, 0, -0.3, -9.0, 59, false),
        w(4, 0, 0.3, -2.0, 61, false),
        w(5, 0, 1.0, 1.4, 67, false),
        w(CHEST, 1, 0.8, BODY_H - 2.5, 71, false),
        w(BELLY, 1, 0.5, 2.0, 83, false),
        w(1, 1, 0.2, -4.0, 97, true),
    ]
}

/// (v264) Carrés d'une plaie : (colonne, rangée vers le bas, couleur sRGB). Un trou de balle :
/// le trou presque noir, sa couronne sombre, le sang autour (jamais deux fois la même tache)
/// et une coulure ; une entaille : un trait en biais.
fn wound_cells(w: &Wound) -> Vec<(i32, i32, u32)> {
    const CORE: u32 = 0x1a0303;
    const RING: u32 = 0x470707;
    const BLOOD: u32 = 0x760e0e;
    const DRIP: u32 = 0x640b0b;
    let h = |k: u32| (w.seed ^ k.wrapping_mul(0x9e37_79b9)).wrapping_mul(0x85eb_ca6b) >> 13;
    let mut cells = Vec::with_capacity(24);
    if w.knife {
        let slope = if h(1) & 1 == 0 { 1 } else { -1 };
        for k in -3i32..=3 {
            let c = if k.abs() <= 1 { CORE } else { RING };
            cells.push((k, k * slope, c));
            if h((k + 13) as u32) % 3 != 0 {
                cells.push((k, k * slope + 1, BLOOD));
            }
        }
    } else {
        // Le sang d'abord (dessous), puis la couronne et le trou.
        for (n, (i, j)) in [
            (-1, -1),
            (1, -1),
            (-1, 1),
            (1, 1),
            (-2, 0),
            (2, 0),
            (0, -2),
            (0, 2),
            (-2, 1),
            (2, 1),
            (-1, 2),
            (1, 2),
        ]
        .into_iter()
        .enumerate()
        {
            if h(20 + n as u32) % 5 < 3 {
                cells.push((i, j, BLOOD));
            }
        }
        for (i, j) in [(-1, 0), (1, 0), (0, -1), (0, 1)] {
            cells.push((i, j, RING));
        }
        cells.push((0, 0, CORE));
    }
    // Coulure : sur les faces debout seulement.
    if w.face < 4 {
        let col = (h(3) % 3) as i32 - 1;
        for j in 2..4 + (h(4) % 4) as i32 {
            cells.push((col, j, DRIP));
        }
    }
    cells
}

/// (v264) Triangles des plaies d'un joueur dans cette pose : chaque plaie suit son pavé (un
/// bras, une jambe qui bouge), posée juste au-dessus de sa face et coupée à ses bords.
pub fn wound_model(pose: &Pose, wounds: &[Wound]) -> Vec<EntityVertex> {
    let mut out = Vec::with_capacity(wounds.len() * 96);
    if wounds.is_empty() {
        return out;
    }
    let parts = parts(pose);
    let lit = EntityVertex::light(pose.light, true);
    for w in wounds {
        let Some(&(xf, min, max, _)) = parts.get(w.part as usize) else {
            continue;
        };
        let (axis, sign, a, b) = WOUND_FACES[w.face.min(5) as usize];
        // Juste au-dessus de la face (0,04 pixel : 2 mm).
        let plane = if sign > 0.0 { max[axis] } else { min[axis] } + sign * 0.04;
        let mut n = Vec3::ZERO;
        n[axis] = sign;
        let shade = face_shade(xf.transform_vector3(n).normalize_or_zero());
        for (i, j, rgb) in wound_cells(w) {
            let s = WOUND_CELL;
            // La rangée descend : vers −y sur les faces debout.
            let (ca, cb) = (w.at[0] + i as f32 * s, w.at[1] - j as f32 * s);
            let (a0, a1) = ((ca - s / 2.0).max(min[a]), (ca + s / 2.0).min(max[a]));
            let (b0, b1) = ((cb - s / 2.0).max(min[b]), (cb + s / 2.0).min(max[b]));
            if a1 - a0 < 0.02 || b1 - b0 < 0.02 {
                continue;
            }
            let c = srgb(rgb);
            let g = grain((w.seed as usize & 0xffff) + (i * 7 + j * 13) as usize % 64);
            let color = [0, 1, 2].map(|k| c[k] * shade * g);
            let at = |pa: f32, pb: f32| {
                let mut p = Vec3::ZERO;
                p[axis] = plane;
                p[a] = pa;
                p[b] = pb;
                xf.transform_point3(p)
            };
            let (p0, p1, p2, p3) = (at(a0, b0), at(a1, b0), at(a1, b1), at(a0, b1));
            for q in [p0, p1, p2, p2, p3, p0] {
                out.push(EntityVertex {
                    pos: q.to_array(),
                    color,
                    uv: [0.0; 2],
                    layer: -1,
                    light: lit,
                });
            }
        }
    }
    out
}

/// (v256) Partie du corps touchée par une balle en partie FPS (`hit_boxes`) : les dégâts en
/// dépendent (`netgame::damage_at`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HitZone {
    Head,
    Chest,
    Arm,
    Belly,
    Leg,
}

/// (v256) Boîtes des coups d'un joueur en partie FPS : les sept pavés du modèle, tournés avec
/// lui (« les hitbox devront être celles de celui-ci aussi »). Pour chacun : le passage du
/// monde à son repère, ses coins dans ce repère, sa zone. La pose est celle que tous les
/// postes connaissent au même pas (pieds, cap, regard, accroupi) : la tête suit le regard, le
/// corps se penche accroupi, les jambes sont droites (leur balancement n'est qu'un dessin,
/// propre à chaque écran) et les bras sont tendus vers le regard, comme ceux d'un joueur qui
/// tient une arme à feu.
pub fn hit_boxes(feet: Vec3, yaw: f32, pitch: f32, crouch: f32) -> [(Mat4, Vec3, Vec3, HitZone); 7] {
    let pose = Pose {
        feet,
        yaw,
        pitch,
        walk: 0.0,
        swing: 0.0,
        crouch,
        light: [1.0; 3],
        held: Some(Held::Gear(0)),
        skin: None,
        bow: 0.0,
    };
    let p = parts(&pose);
    std::array::from_fn(|i| {
        let (xf, min, max, part) = p[i];
        let zone = match part {
            Part::Head => HitZone::Head,
            Part::Arm => HitZone::Arm,
            Part::Leg => HitZone::Leg,
            Part::Body if i == BELLY => HitZone::Belly,
            Part::Body => HitZone::Chest,
        };
        (xf.inverse(), Vec3::from(min), Vec3::from(max), zone)
    })
}

/// Les couches d'un corps vues de face : huit images côte à côte (la tête a huit couches
/// de cubes, le reste quatre : chaque couche du corps y est deux fois). `grids` : la grille de
/// cubes de chaque pavé ; `inside` : la couleur d'un cube (`None` : un creux) ; `tall` :
/// hauteur du corps par rapport au joueur.
#[cfg(test)]
pub(crate) fn irm(
    grids: [[usize; 3]; 7],
    inside: impl Fn(usize, [usize; 3], [usize; 3]) -> Option<u32>,
    tall: f32,
) -> image::RgbImage {
    const S: f32 = 330.0; // pixels par bloc
    let (w, h) = ((1.0 * S) as u32, ((1.8 * tall + 0.15) * S) as u32);
    let mut img = image::RgbImage::from_pixel(w * 8, h, image::Rgb([24, 26, 30]));
    // Debout, de face, bras le long du corps (sans leur écart, pour des cubes droits).
    let boxes: [(f32, f32); 7] = [
        (-LEG_X, LEG),
        (LEG_X, LEG),
        (0.0, LEG),
        (-(BODY_W / 2.0 + ARM_W / 2.0 + 0.35), LEG + SHOULDER_Y),
        (BODY_W / 2.0 + ARM_W / 2.0 + 0.35, LEG + SHOULDER_Y),
        (0.0, NECK_Y),
        (0.0, LEG),
    ];
    for layer in 0..8usize {
        for (i, &(min, max, _)) in PARTS.iter().enumerate() {
            let d = grids[i];
            let z = layer * d[2] / 8;
            let (ox, oy) = boxes[i];
            for y in 0..d[1] {
                for x in 0..d[0] {
                    let Some(rgb) = inside(i, [x, y, z], d) else {
                        continue;
                    };
                    let cell = |v: usize, a: usize| {
                        min[a] + (max[a] - min[a]) * v as f32 / d[a] as f32
                    };
                    // De face : la droite du personnage (+x) à gauche de l'image.
                    let (x0, x1) = (ox + cell(x, 0), ox + cell(x + 1, 0));
                    let (y0, y1) = ((oy + cell(y, 1)) * tall, (oy + cell(y + 1, 1)) * tall);
                    let px = |v: f32| ((0.5 - v * PX) * S) as u32;
                    let py = |v: f32| h - 12 - (v * PX * S) as u32;
                    for yy in py(y1)..py(y0).saturating_sub(1) {
                        for xx in px(x1)..px(x0).saturating_sub(1) {
                            let p = [(rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8];
                            img.put_pixel(layer as u32 * w + xx.min(w - 1), yy.min(h - 1), image::Rgb(p));
                        }
                    }
                }
            }
        }
    }
    img
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec2;

    /// Repère d'une partie dans la pose d'une instance : `part_matrix` de players.wgsl,
    /// ligne pour ligne.
    fn shader_part_matrix(part: u32, p: &PlayerInstance) -> Mat4 {
        let px = 1.8 / 32.0;
        let t = |x: f32, y: f32, z: f32| Mat4::from_translation(Vec3::new(x, y, z));
        let base = Mat4::from_translation(Vec3::from(p.feet))
            * Mat4::from_rotation_y(-p.yaw)
            * Mat4::from_scale(Vec3::splat(px));
        let swing = p.walk.sin() * 0.75 * p.swing;
        let crouch = p.crouch.clamp(0.0, 1.0);
        let belly = CROUCH_BELLY_LEAN * crouch;
        let bend = CROUCH_BEND * crouch;
        let lean = belly + bend;
        let drop = CROUCH_HIP_DROP * crouch;
        let pitch = p.pitch.clamp(-1.4, 1.4);
        let shrink = Mat4::from_scale(Vec3::new(1.0, (LEG - drop) / LEG, 1.0));
        if part == 0 {
            return base * t(-LEG_X, LEG - drop, 0.0) * Mat4::from_rotation_x(swing) * shrink;
        }
        if part == 1 {
            return base * t(LEG_X, LEG - drop, 0.0) * Mat4::from_rotation_x(-swing) * shrink;
        }
        if part == 6 {
            return base * t(0.0, LEG - drop, 0.0) * Mat4::from_rotation_x(-belly);
        }
        let (sb, cb) = (bend.sin(), bend.cos());
        let dy = BELLY_H * (1.0 - cb) - BODY_D / 2.0 * sb;
        let dz = BELLY_H * sb + BODY_D / 2.0 * (1.0 - cb);
        let (sa, ca) = (belly.sin(), belly.cos());
        let chest_at = Vec3::new(0.0, LEG - drop + dy * ca + dz * sa, -dy * sa + dz * ca);
        let upper = base * t(chest_at.x, chest_at.y, chest_at.z) * Mat4::from_rotation_x(-lean);
        if part == 2 {
            return upper;
        }
        if ((part == 3 || part == 4) && p.skin & 0x20000 != 0)
            || (part == 4 && p.skin & 0x40000 != 0)
        {
            let (side, turn, sign) = if part == 4 {
                (ARM_X, 0.15, 1.0)
            } else {
                (-ARM_X, -0.5, -1.0)
            };
            let down = if part == 4 && p.skin & 0x100000 != 0 {
                ITEM_AIM_DROP
            } else {
                0.0
            };
            return upper
                * t(side, SHOULDER_Y, 0.0)
                * Mat4::from_rotation_y(turn)
                * Mat4::from_rotation_x(
                    1.570_796_4 + (pitch - down).max(-1.4) + lean + swing * sign * 0.1,
                );
        }
        if part == 3 {
            return upper
                * t(-ARM_X, SHOULDER_Y, 0.0)
                * Mat4::from_rotation_z(-ARM_SPLAY)
                * Mat4::from_rotation_x(-swing + lean * 0.6);
        }
        if part == 4 {
            let (mut raise, mut k) = (0.0, 1.0);
            if p.skin & 0x80000 != 0 {
                raise = ZEUS_RAISE;
                k = RAISED_SWING;
            } else if p.skin & 0x10000 != 0 {
                raise = (0.65 + 0.6 * pitch).clamp(0.0, 2.0);
                if raise != 0.0 {
                    k = RAISED_SWING;
                }
            }
            let splay = if p.skin & 0x10000 != 0 { 0.0 } else { ARM_SPLAY };
            return upper
                * t(ARM_X, SHOULDER_Y, 0.0)
                * Mat4::from_rotation_z(splay)
                * Mat4::from_rotation_x(swing * k + lean * 0.6 + raise);
        }
        let n = chest_at + Vec3::new(0.0, BODY_H * lean.cos(), -BODY_H * lean.sin());
        base * t(n.x, n.y, n.z) * Mat4::from_rotation_x(pitch)
    }

    /// Skin aux pixels pris au hasard, dont des transparents (le modèle par défaut se voit
    /// dessous), au format récent (les deux calques).
    fn test_skin() -> std::sync::Arc<crate::skin::Skin> {
        let mut img = image::RgbaImage::new(64, 64);
        for (x, y, p) in img.enumerate_pixels_mut() {
            let h = (x * 7919 + y * 104_729 + 17).wrapping_mul(2_654_435_761);
            let a = if h >> 30 == 0 { 0 } else { 255 };
            *p = image::Rgba([(h >> 4) as u8, (h >> 12) as u8, (h >> 20) as u8, a]);
        }
        let mut png = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        crate::skin::Skin::from_png(&png).unwrap()
    }

    /// Poses d'essai : immobile au skin par défaut, en marche avec un skin et une arme à feu
    /// (les deux bras tendus), accroupi regardant vers le haut avec un bloc en main, et
    /// couteau en main (le bras droit seul).
    /// (v241) La Foudre de Zeus dans le dos : derrière le corps (jamais dedans), en travers
    /// (de la hanche gauche à par-dessus l'épaule droite), de la taille de celle qu'on tient,
    /// et elle suit le joueur qui tourne ou s'accroupit.
    #[test]
    fn the_bolt_on_the_back_lies_across_the_back() {
        let half = crate::lightning::HELD_LEN * 0.5;
        for (yaw, crouch) in [(0.0f32, 0.0f32), (1.3, 0.0), (-2.0, 1.0)] {
            let pose = Pose {
                feet: Vec3::new(4.0, 70.0, -3.0),
                yaw,
                pitch: 0.2,
                walk: 0.4,
                swing: 0.5,
                crouch,
                light: [1.0; 3],
                held: None,
                skin: None,
                bow: 0.0,
            };
            let m = zeus_back_frame(&pose).unwrap();
            // Repère du joueur : x à droite, z vers l'arrière.
            let local = |p: Vec3| Mat4::from_rotation_y(pose.yaw).transform_vector3(p - pose.feet);
            let (low, mid, top) = (
                local(m.transform_point3(-Vec3::Y * half)),
                local(m.transform_point3(Vec3::ZERO)),
                local(m.transform_point3(Vec3::Y * half)),
            );
            assert!(((top - low).length() - crate::lightning::HELD_LEN).abs() < 1e-3);
            assert!(top.x > 0.15 && low.x < -0.15 && top.y > low.y + 0.6, "{low} {top}");
            // (v244) À 22,5° de la verticale (la moitié des 45° de son icône), debout.
            if crouch == 0.0 {
                let d = top - low;
                let tilt = std::f32::consts::FRAC_PI_8.tan();
                assert!((d.x / d.y - tilt).abs() < 0.03 && d.z.abs() < 0.05, "{d}");
            }
            // Derrière le dos (le corps fait 4 pixels d'épaisseur : son dos est à 2 pixels).
            if crouch == 0.0 {
                assert!(mid.z > 2.5 * PX && mid.z < 5.0 * PX, "{mid}");
                assert!(mid.x.abs() < 0.05 && mid.y > 0.9 && mid.y < 1.3, "{mid}");
            } else {
                assert!(mid.y < 1.2 && low.z > 0.0);
            }
        }
    }

    fn test_poses() -> Vec<Pose> {
        let skin = test_skin();
        let ak = crate::weapons::WEAPONS
            .iter()
            .position(|w| w.name == "AK-47")
            .unwrap() as u8;
        // (v225) Plus la Foudre de Zeus (bras levé) et la lampe torche (bras droit tendu).
        (0..6)
            .map(|n| Pose {
                feet: Vec3::new(3.0 + n as f32, 70.0, -5.0),
                yaw: 0.7 + n as f32 * 2.1,
                pitch: [-0.5, 0.3, 1.2, -0.2, -0.6, 0.4][n],
                walk: 1.3 + n as f32,
                swing: [0.0, 0.8, 0.5, 0.6, 0.7, 0.3][n],
                crouch: [0.0, 0.6, 1.0, 0.0, 0.3, 0.0][n],
                // Exactement représentable au format des sommets (k / 127,5).
                light: [0.8, 0.4, 1.2],
                held: [
                    None,
                    Some(Held::Gear(ak)),
                    Some(Held::Block(BlockId::STONE)),
                    Some(Held::Gear(crate::cs::W_KNIFE)),
                    Some(Held::Zeus),
                    Some(Held::Flashlight(true)),
                ][n],
                skin: (n > 0).then(|| skin.clone()),
                bow: 0.0,
            })
            .collect()
    }

    /// (v234) La tête reste posée au milieu du haut du corps (le cou), debout comme penché
    /// (accroupi), quel que soit le regard ; elle est là où le dit `head_place` (sa boîte des
    /// coups en partie FPS), vers le regard ; debout, son haut est à 1,8 bloc.
    #[test]
    fn the_head_stays_on_the_neck_when_leaning() {
        for pose in test_poses() {
            let p = parts(&pose);
            let neck = p[2].0.transform_point3(Vec3::new(0.0, BODY_H, 0.0));
            let head = p[5].0.transform_point3(Vec3::ZERO);
            assert!(
                (neck - head).length() < 1e-4,
                "accroupi à {} : tête à {head}, cou à {neck}",
                pose.crouch
            );
            let (ahead, bottom) = head_place(pose.crouch);
            let fwd = Vec3::new(pose.yaw.sin(), 0.0, -pose.yaw.cos());
            let want = pose.feet + fwd * ahead + Vec3::Y * bottom;
            assert!(
                (head - want).length() < 1e-4,
                "tête en {head}, `head_place` : {want}"
            );
        }
        let (ahead, bottom) = head_place(0.0);
        assert!(ahead.abs() < 1e-6 && (bottom + HEAD_BLOCKS - 1.8).abs() < 1e-4);
        // Accroupi : la tête avance de 0,3 bloc avec le cou.
        let (ahead, _) = head_place(1.0);
        assert!((0.25..0.35).contains(&ahead), "{ahead}");
    }

    /// (v239) « Le bras pointe entre le milieu et le bas de l'écran, 50 % » : avec un objet
    /// tenu en construction, le bras droit tendu pointe vers le point à mi-chemin du milieu et
    /// du bas de l'écran (champ vertical de 75°), quel que soit le regard ; avec la lampe
    /// torche, vers le milieu. L'objet est au bout du bras, là où la première personne le
    /// dessine (`held_item_matrix`) et où F5 le montre (`held_parts`).
    #[test]
    fn with_an_item_the_arm_points_halfway_down_the_screen() {
        let half = (crate::session::FOV.to_radians() * 0.5).tan();
        assert!((ITEM_AIM_DROP.tan() / half - 0.5).abs() < 2e-3, "{}", ITEM_AIM_DROP.tan() / half);
        for pitch in [-0.6f32, 0.0, 0.5] {
            for (held, want) in [
                (Held::Block(BlockId::STONE), -0.5),
                (Held::Torch([255, 200, 100]), -0.5),
                (Held::Wand([70, 60, 90]), -0.5),
                (Held::Flashlight(false), 0.0),
            ] {
                let pose = Pose {
                    feet: Vec3::new(1.0, 64.0, 2.0),
                    yaw: 0.4,
                    pitch,
                    walk: 0.0,
                    swing: 0.0,
                    crouch: 0.0,
                    light: [1.0; 3],
                    held: Some(held),
                    skin: None,
                    bow: 0.0,
                };
                let arm = right_arm_matrix(&pose).unwrap();
                // De l'épaule (z = 1 de la boîte du bras) au poing (z = 0).
                let dir = arm.transform_point3(Vec3::new(0.5, 0.5, 0.0))
                    - arm.transform_point3(Vec3::new(0.5, 0.5, 1.0));
                let look = Mat4::from_rotation_y(-pose.yaw) * Mat4::from_rotation_x(pitch);
                let d = look.inverse().transform_vector3(dir);
                let y = d.y / -d.z / half;
                assert!((y - want).abs() < 0.02, "{held:?}, regard {pitch} : {y}");
                // L'objet est au bout du bras : son repère commence au poing ou tout près.
                let fist = arm.transform_point3(Vec3::new(0.5, 0.5, 0.0));
                let item = held_item_matrix(&pose).unwrap();
                let c = item.transform_point3(Vec3::splat(0.5));
                assert!(c.distance(fist) < 0.5, "{held:?} : {}", c.distance(fist));
            }
        }
        // Un bloc : le cube de la première personne est la boîte de F5.
        let pose = Pose {
            feet: Vec3::ZERO,
            yaw: 1.1,
            pitch: 0.2,
            walk: 0.0,
            swing: 0.0,
            crouch: 0.3,
            light: [1.0; 3],
            held: Some(Held::Block(BlockId::STONE)),
            skin: None,
            bow: 0.0,
        };
        let (frame, min, max, _) = held_parts(&pose)[0];
        let m = held_item_matrix(&pose).unwrap();
        for (unit, px) in [(Vec3::ZERO, Vec3::from(min)), (Vec3::ONE, Vec3::from(max))] {
            let (a, b) = (m.transform_point3(unit), frame.transform_point3(px));
            assert!(a.distance(b) < 1e-4, "{a} {b}");
        }
    }

    /// (v247) « Mon crâne dépasse dans les blocs du dessus quand je passe accroupi sous
    /// 1,5 bloc » : accroupi, la tête (et tout le modèle), quel que soit le regard, reste sous
    /// la boîte accroupie (1,5 bloc) ; debout, regard droit, sa tête arrive à 1,8 bloc.
    #[test]
    fn the_crouched_head_stays_inside_the_crouched_box() {
        let top = |crouch: f32, pitch: f32| {
            let pose = Pose {
                feet: Vec3::ZERO,
                yaw: 0.7,
                pitch,
                walk: 0.0,
                swing: 0.0,
                crouch,
                light: [1.0; 3],
                held: None,
                skin: None,
                bow: 0.0,
            };
            let mut hi = f32::MIN;
            for (xf, mn, mx, _) in parts(&pose) {
                for k in 0..8 {
                    let c = Vec3::new(
                        if k & 1 == 0 { mn[0] } else { mx[0] },
                        if k & 2 == 0 { mn[1] } else { mx[1] },
                        if k & 4 == 0 { mn[2] } else { mx[2] },
                    );
                    hi = hi.max(xf.transform_point3(c).y);
                }
            }
            hi
        };
        let box_h = crate::player::HEIGHT - crate::player::CROUCH_DROP;
        for i in 0..=28 {
            let pitch = -1.4 + 0.1 * i as f32;
            let t = top(1.0, pitch);
            assert!(t <= box_h - 0.005, "accroupi, regard {pitch} : {t}");
        }
        assert!((top(0.0, 0.0) - 1.8).abs() < 1e-3);
        // Le milieu de la tête accroupie est à la hauteur des yeux (à 3 cm près).
        let (_, bottom) = head_place(1.0);
        let mid = bottom + HEAD_BLOCKS / 2.0;
        assert!((mid - (crate::player::EYE - crate::player::CROUCH_DROP)).abs() < 0.03, "{mid}");
    }

    /// (v240) La Foudre de Zeus à la première personne est l'éclair du modèle (`zeus_frame`,
    /// comme en F5) : debout au bout du bras levé, à droite, on en voit une bonne partie, le
    /// regard un peu en bas, droit devant ou un peu en haut.
    #[test]
    fn the_held_bolt_is_seen_in_first_person() {
        let half = (crate::session::FOV.to_radians() * 0.5).tan();
        for pitch in [-0.4f32, 0.0, 0.3] {
            let pose = Pose {
                feet: Vec3::new(2.0, 64.0, -1.0),
                yaw: 2.2,
                pitch,
                walk: 0.0,
                swing: 0.0,
                crouch: 0.0,
                light: [1.0; 3],
                held: Some(Held::Zeus),
                skin: None,
                bow: 0.0,
            };
            let eye = crate::session::eye_of(pose.feet, 0.0);
            let view = (Mat4::from_translation(eye)
                * Mat4::from_rotation_y(-pose.yaw)
                * Mat4::from_rotation_x(pitch))
            .inverse();
            let bolt = view * zeus_frame(&pose).unwrap();
            let len = crate::lightning::HELD_LEN;
            let seen = (0..=8)
                .map(|k| bolt.transform_point3(Vec3::Y * len * (k as f32 / 8.0 - 0.5)))
                .filter(|p| {
                    let (x, y) = (p.x / -p.z / (half * 16.0 / 9.0), p.y / -p.z / half);
                    p.z < -0.1 && x.abs() <= 1.0 && y.abs() <= 1.0 && x > 0.1
                })
                .count();
            assert!(seen >= 3, "regard {pitch} : {seen} points de l'éclair à l'écran");
        }
    }

    /// (v225) Les proportions du modèle (de CS2), et le bras levé de la Foudre de Zeus, sont
    /// les mêmes dans le shader des autres joueurs (`part_matrix` de players.wgsl).
    #[test]
    fn the_shader_has_the_same_proportions() {
        let src = include_str!("../../render/src/shaders/players.wgsl");
        for (name, v) in [
            ("LEG", LEG),
            ("LEG_X", LEG_X),
            ("SHOULDER_Y", SHOULDER_Y),
            ("ARM_X", ARM_X),
            ("ARM_SPLAY", ARM_SPLAY),
            ("BODY_H", BODY_H),
            ("BELLY_H", BELLY_H),
            ("HALF_D", BODY_D / 2.0),
            ("CROUCH_BELLY_LEAN", CROUCH_BELLY_LEAN),
            ("CROUCH_BEND", CROUCH_BEND),
            ("CROUCH_HIP_DROP", CROUCH_HIP_DROP),
            ("ZEUS_RAISE", ZEUS_RAISE),
            ("ITEM_AIM_DROP", ITEM_AIM_DROP),
        ] {
            let line = src
                .lines()
                .find(|l| l.starts_with(&format!("const {name}: f32 =")))
                .unwrap_or_else(|| panic!("{name} absent de players.wgsl"));
            let got: f32 = line
                .split('=')
                .nth(1)
                .unwrap()
                .trim()
                .trim_end_matches(';')
                .parse()
                .unwrap();
            // (v234) Écrites à six décimales dans le shader.
            assert!((got - v).abs() < 1e-4, "{name} : {got} au lieu de {v}");
        }
        // (Hauteur, tête, poing : vérifiés à la compilation, sous `ARM_RATIO`.)
    }

    /// (v256) Le modèle en sept pavés du schéma « CS2 » : poitrine large, ventre plus étroit,
    /// jambes écartées, bras tenus par le seul coin du haut de l'épaule. Moins d'un bloc de
    /// large (« confirme-moi que notre nouveau modèle ne fait pas plus d'un bloc de large »).
    #[test]
    fn the_model_has_a_chest_a_belly_and_parted_legs() {
        let pose = |held: Option<Held>| Pose {
            feet: Vec3::ZERO,
            yaw: 0.0,
            pitch: 0.0,
            walk: 0.0,
            swing: 0.0,
            crouch: 0.0,
            light: [1.0; 3],
            held,
            skin: None,
            bow: 0.0,
        };
        let p = parts(&pose(None));
        assert_eq!(p.len(), 7);
        // Coins de chaque pavé dans le monde (blocs), debout de face.
        let world: Vec<(Vec3, Vec3)> = p
            .iter()
            .map(|&(xf, mn, mx, _)| {
                let (mut lo, mut hi) = (Vec3::splat(1e9), Vec3::splat(-1e9));
                for k in 0..8 {
                    let c = Vec3::new(
                        if k & 1 == 0 { mn[0] } else { mx[0] },
                        if k & 2 == 0 { mn[1] } else { mx[1] },
                        if k & 4 == 0 { mn[2] } else { mx[2] },
                    );
                    let w = xf.transform_point3(c);
                    (lo, hi) = (lo.min(w), hi.max(w));
                }
                (lo, hi)
            })
            .collect();
        let width = |i: usize| world[i].1.x - world[i].0.x;
        let (chest, belly) = (world[CHEST], world[BELLY]);
        // Le ventre sous la poitrine, sans trou ni recouvrement, plus étroit qu'elle d'une
        // colonne du skin de chaque côté ; les jambes dessous, de trois colonnes chacune.
        assert!((belly.1.y - chest.0.y).abs() < 1e-5);
        assert!((belly.0.y - HIP_BLOCKS).abs() < 1e-5 && (chest.1.y - NECK_BLOCKS).abs() < 1e-5);
        let col = COL * PX;
        assert!((width(CHEST) - 8.0 * col).abs() < 1e-5, "{}", width(CHEST));
        assert!((width(BELLY) - 6.0 * col).abs() < 1e-5, "{}", width(BELLY));
        assert!((width(0) - 3.0 * col).abs() < 1e-5 && (width(1) - 3.0 * col).abs() < 1e-5);
        // Jambes écartées : la gauche du côté −x, 0,8 pixel entre elles.
        assert!((world[1].0.x - world[0].1.x - LEG_GAP * PX).abs() < 1e-5);
        assert!((world[0].1.x + world[1].0.x).abs() < 1e-5, "de part et d'autre du milieu");
        // Un pixel du skin a la même taille sur la poitrine, le ventre et les jambes, et
        // aucun n'est à cheval sur deux pavés : chaque pavé fait un nombre entier de pixels.
        for i in [0, 1, CHEST, BELLY] {
            let (_, mn, mx, _) = p[i];
            let z = ZONES[i];
            assert!(((mx[0] - mn[0]) / z.size[0] as f32 - COL).abs() < 1e-5, "{i}");
            assert!(((mx[2] - mn[2]) / z.size[2] as f32 - COL).abs() < 1e-5, "{i}");
        }
        assert!(((BODY_H - BELLY_H) / ZONES[CHEST].size[1] as f32 - ROW).abs() < 1e-5);
        assert!((BELLY_H / ZONES[BELLY].size[1] as f32 - ROW).abs() < 1e-5);
        assert_eq!(ZONES[CHEST].size[1] + ZONES[BELLY].size[1], 12);
        assert_eq!(ZONES[BELLY].row, ZONES[CHEST].size[1]);
        // Bras qui pendent : le coin du haut, côté corps, sur le coin du haut de la
        // poitrine ; le reste du bras s'en écarte (la main à plus d'un pixel du ventre).
        for (i, sign) in [(3usize, -1.0f32), (4, 1.0)] {
            let (xf, mn, mx, _) = p[i];
            let inner = if sign > 0.0 { mn[0] } else { mx[0] };
            let top = xf.transform_point3(Vec3::new(inner, mx[1], 0.0));
            let want = Vec3::new(sign * BODY_W / 2.0, NECK_Y, 0.0) * PX;
            assert!(top.distance(want) < 1e-4, "{i} : {top} au lieu de {want}");
            let hand = xf.transform_point3(Vec3::new(inner, mn[1], 0.0));
            assert!(hand.x * sign > (BODY_W / 2.0 + 1.0) * PX, "{i} : {hand}");
        }
        // D'une main à l'autre : moins d'un bloc (0,83 en v256 ; 0,86 depuis que le corps a
        // grandi sous une tête plus petite).
        let (lo, hi) = world
            .iter()
            .fold((f32::MAX, f32::MIN), |(lo, hi), w| (lo.min(w.0.x), hi.max(w.1.x)));
        assert!(hi - lo < 0.9 && hi - lo > 0.82, "{}", hi - lo);
        assert!((SPLAY_SIN - ARM_SPLAY.sin()).abs() < 1e-6);
        assert!((SPLAY_COS - ARM_SPLAY.cos()).abs() < 1e-6);
        assert!((ARM_SPLAY.to_degrees() - 6.0).abs() < 1e-4);
        // Un objet en main : ce bras-là n'est plus écarté (il est tendu devant).
        let (xf, _, _, _) = parts(&pose(Some(Held::Wand([255, 0, 0]))))[4];
        assert!(xf.transform_vector3(Vec3::X).y.abs() < 1e-6);
    }

    /// (v269) Le corps cube par cube : sans morsure, exactement les pixels du modèle habituel
    /// (mêmes carrés, mêmes couleurs) ; mordu, les carrés du dehors qui restent n'ont pas
    /// changé, ceux de la morsure ont disparu, et le creux montre l'intérieur (os, organes).
    #[test]
    fn a_bitten_body_keeps_its_skin_and_shows_its_inside() {
        use crate::flesh::{organ, Flesh};
        let skin = test_skin();
        let pose = Pose {
            feet: Vec3::new(2.0, 30.0, 5.0),
            yaw: 0.6,
            pitch: 0.2,
            walk: 0.7,
            swing: 0.8,
            crouch: 0.3,
            light: [1.0; 3],
            held: None,
            skin: Some(skin),
            bow: 0.0,
        };
        // Un carré : son milieu et sa couleur ; deux carrés sont les mêmes au centième de
        // millimètre et au dix-millième de couleur près (les deux dessins ne font pas leurs
        // additions dans le même ordre).
        let quads = |v: &[EntityVertex]| -> Vec<(Vec3, Vec3)> {
            v.chunks_exact(6)
                .map(|q| {
                    let c = (Vec3::from(q[0].pos) + Vec3::from(q[2].pos)) / 2.0;
                    (c, Vec3::from(q[0].color))
                })
                .collect()
        };
        // Les carrés de `a` qui ne sont pas dans `b`.
        let missing = |a: &[(Vec3, Vec3)], b: &[(Vec3, Vec3)]| -> usize {
            a.iter()
                .filter(|(p, c)| {
                    !b.iter()
                        .any(|(q, d)| (*p - *q).abs().max_element() < 1e-5 && (*c - *d).abs().max_element() < 1e-4)
                })
                .count()
        };
        let grids = flesh_grids();
        let parts = parts(&pose);
        for (i, &(xf, min, max, part)) in parts.iter().enumerate() {
            let tex = pose.skin.as_deref();
            let mut plain = Vec::new();
            emit_box(&mut plain, xf, min, max, ZONES[i], part, pose.light, tex);
            let mut whole = Vec::new();
            let none = Flesh::default();
            emit_flesh(&mut whole, xf, min, max, i, part, pose.light, tex, &none, organ);
            let (a, b) = (quads(&plain), quads(&whole));
            assert_eq!(a.len(), b.len(), "pavé {i} : sans morsure");
            assert_eq!((missing(&a, &b), missing(&b, &a)), (0, 0), "pavé {i} : sans morsure");
            // Une morsure dans un coin du haut, devant.
            let mut f = Flesh::default();
            let d = grids[i];
            let b = f.bite_at(i, [d[0] - 1, d[1] - 1, 0], 2, &grids).unwrap();
            let mut bitten = Vec::new();
            emit_flesh(&mut bitten, xf, min, max, i, part, pose.light, tex, &f, organ);
            let (before, after) = (quads(&plain), quads(&bitten));
            let (lost, new) = (missing(&before, &after), missing(&after, &before));
            // Le coin de 2 × 2 × 2 cubes : 3 faces de 4 carrés du skin en moins, 3 faces de
            // 4 carrés de chair en plus (là où le cube mordu touchait le reste). (v272) Et
            // le sang qui en coule : sous la morsure, sur les deux faces du tour entamées,
            // quatre colonnes de carrés du skin rougis (deux à cinq carrés chacune, moins
            // si le pavé finit avant).
            let size: Vec<usize> = (0..3).map(|a| (b.hi[a] - b.lo[a]) as usize).collect();
            let outer = size[0] * size[1] + size[1] * size[2] + size[0] * size[2];
            let below = (d[1] - size[1]).min(crate::flesh::DRIP);
            assert_eq!(lost, new, "pavé {i} : un carré pour un carré");
            let red = lost - outer;
            assert!((4 * below.min(2)..=4 * below).contains(&red), "pavé {i} : {red} rougis");
        }
        // Le modèle entier : `build_bitten` sans morsure est `build`.
        let (a, b) = (quads(&build(&pose)), quads(&build_bitten(&pose, Some(&Flesh::default()))));
        assert_eq!(a.len(), b.len());
        assert_eq!((missing(&a, &b), missing(&b, &a)), (0, 0), "modèle entier sans morsure");
    }

    /// (v272) « On doit se voir se pencher pour manger le morceau qu'on vise » : plié aux
    /// hanches, les pieds et les jambes ne bougent pas, la bouche arrive sur ce qu'on vise
    /// devant soi (de la hauteur de la cuisse à celle du visage, à la bonne distance), le regard
    /// reste où il était ; trop loin, elle n'y arrive pas.
    #[test]
    fn bowing_brings_the_mouth_onto_what_one_aims_at() {
        let feet = Vec3::new(4.0, 20.0, -3.0);
        let straight = |bow: f32| Pose {
            feet,
            yaw: 0.0,
            pitch: -0.3,
            walk: 0.0,
            swing: 0.0,
            crouch: 0.0,
            light: [1.0; 3],
            held: None,
            skin: None,
            bow,
        };
        let up = mouth_point(&straight(0.0));
        assert!((up.y - feet.y - 1.63).abs() < 0.06 && (up.z - feet.z + 0.1).abs() < 0.06, "{up}");
        // Devant soi (−z), de plus en plus bas : la bouche y arrive, de plus en plus plié.
        let mut last = 0.0;
        for (ahead, height) in [(0.45f32, 1.5f32), (0.72, 1.3), (0.82, 0.95), (0.79, 0.72)] {
            let target = feet + Vec3::new(0.0, height, -ahead);
            let mut pose = straight(0.0);
            let miss = bow_to(&mut pose, target);
            assert!(miss < 0.1, "{ahead} devant, {height} de haut : à {miss}");
            assert!(pose.bow > last && pose.bow <= BOW_MAX, "{}", pose.bow);
            last = pose.bow;
            let (a, b) = (parts(&straight(0.0)), parts(&pose));
            for leg in 0..2 {
                assert_eq!(a[leg].0, b[leg].0, "les jambes ne bougent pas");
            }
            // La tête garde son regard (elle ne tourne pas avec le dos).
            let look = |p: &[(Mat4, [f32; 3], [f32; 3], Part)]| p[5].0.transform_vector3(Vec3::NEG_Z);
            assert!(look(&a).angle_between(look(&b)) < 1e-3);
            // La caméra plonge d'autant que les yeux.
            let shift = bow_shift(&pose);
            assert!(shift.z < -0.2 && shift.y < 0.0 && shift.length() < 1.5, "{shift}");
        }
        // Trop loin, ou derrière soi : la bouche n'y arrive pas.
        for target in [feet + Vec3::new(0.0, 1.0, -1.5), feet + Vec3::new(0.0, 1.0, 0.8)] {
            assert!(bow_to(&mut straight(0.0), target) > 0.3);
        }
        assert_eq!(bow_shift(&straight(0.0)), Vec3::ZERO);
    }

    /// (v269) « Tu me montreras l'IRM des couches de notre modèle 3D joueur » : les couches du
    /// corps du joueur, de devant (à gauche) vers le dos, vues de face ; chaque cube à la
    /// couleur de ce qu'il y a là (`flesh::organ`). Écrit dans le fichier nommé par
    /// MINELIB_IRM (essai à lancer à la main).
    #[test]
    #[ignore]
    fn irm_of_the_player() {
        let Ok(path) = std::env::var("MINELIB_IRM") else {
            return;
        };
        let img = irm(flesh_grids(), |p, c, d| Some(crate::flesh::organ(p, c, d)), 1.0);
        img.save(path).unwrap();
    }

    /// (v267) « On doit voir notre corps dans le jeu » : à la première personne, en baissant
    /// les yeux, on voit sa poitrine, son ventre, ses jambes et son bras gauche ; ni sa tête,
    /// ni son bras droit (dessiné à part). Rien n'est trop près des yeux pour être coupé par
    /// le plan proche de la caméra (0,05), debout comme accroupi, quel que soit le regard.
    #[test]
    fn looking_down_one_sees_ones_own_body() {
        let near = 0.05;
        for crouch in [0.0f32, 1.0] {
            for pitch in [-1.5f32, -1.0, -0.5] {
                let pose = Pose {
                    feet: Vec3::new(3.0, 20.0, -7.0),
                    yaw: 0.8,
                    pitch,
                    walk: 0.0,
                    swing: 0.0,
                    crouch,
                    light: [1.0; 3],
                    held: None,
                    skin: None,
                    bow: 0.0,
                };
                let eye = crate::session::eye_of(pose.feet, crouch);
                let view = (Mat4::from_translation(eye)
                    * Mat4::from_rotation_y(-pose.yaw)
                    * Mat4::from_rotation_x(pitch))
                .inverse();
                let proj = Mat4::perspective_infinite_reverse_rh(
                    crate::session::FOV.to_radians(),
                    16.0 / 9.0,
                    near,
                );
                let body = first_person_body(&pose, &test_wounds(), None, proj * view);
                assert!(body.len() > 5000 && body.len() % 6 == 0, "{}", body.len());
                // Des carrés dans l'image, devant le plan proche.
                let seen = body
                    .iter()
                    .filter(|v| {
                        let [x, y, _, w] = v.clip;
                        w > near && x.abs() < w && y.abs() < w
                    })
                    .count();
                // (Debout sans bouger, le regard à peine baissé, on ne voit pas encore son
                // corps : il est sous soi.)
                if pitch <= -1.0 {
                    assert!(seen > 600, "accroupi {crouch}, regard {pitch} : {seen} sommets vus");
                }
                // Le plan proche ne coupe aucun carré dans l'image (on verrait dedans) : là
                // où un côté de triangle le traverse, c'est hors de l'écran.
                for t in body.chunks_exact(3) {
                    for (a, b) in [(0, 1), (1, 2), (2, 0)] {
                        let (a, b) = (glam::Vec4::from(t[a].clip), glam::Vec4::from(t[b].clip));
                        if (a.w - near) * (b.w - near) < 0.0 {
                            let c = a + (b - a) * ((near - a.w) / (b.w - a.w));
                            assert!(
                                c.x.abs() > c.w || c.y.abs() > c.w,
                                "accroupi {crouch}, regard {pitch} : coupé par le plan proche \
                                 dans l'image ({}, {})",
                                c.x / c.w,
                                c.y / c.w
                            );
                        }
                    }
                }
                // Normales vers le dehors : unitaires.
                assert!(body.iter().all(|v| (Vec3::from(v.normal).length() - 1.0).abs() < 1e-3));
            }
        }
    }

    /// (v264) « Un impact dans le joueur » : la plaie est sur le pavé touché, sur sa face
    /// tournée vers le tireur, et ses carrés restent sur cette face (coupés à ses bords) ;
    /// elle suit le membre qui bouge.
    #[test]
    fn a_hit_leaves_a_wound_on_the_box_it_touched() {
        let pose = |walk: f32| Pose {
            feet: Vec3::new(4.0, 10.0, -3.0),
            yaw: 0.0,
            pitch: 0.0,
            walk,
            swing: if walk == 0.0 { 0.0 } else { 1.0 },
            crouch: 0.0,
            light: [1.0; 3],
            held: None,
            skin: None,
            bow: 0.0,
        };
        let feet = pose(0.0).feet;
        // De face (il regarde vers −z) : au milieu de la poitrine, puis dans la jambe droite.
        let shooter = feet + Vec3::new(0.0, 1.3, -6.0);
        let chest = feet + Vec3::new(0.02, 1.3, -BODY_D / 2.0 * PX);
        let w = wound_at(&pose(0.0), chest, shooter, false, 7).expect("plaie");
        assert_eq!((w.part as usize, w.face), (CHEST, 0));
        let leg = feet + Vec3::new(LEG_X * PX, 0.4, -LEG_D / 2.0 * PX);
        let w2 = wound_at(&pose(0.0), leg, shooter, false, 9).expect("plaie");
        assert_eq!((w2.part, w2.face), (1, 0));
        // De dos : la face arrière ; de côté : le bras.
        let behind = wound_at(&pose(0.0), chest + Vec3::Z * 0.2, feet + Vec3::new(0.0, 1.3, 6.0), true, 3);
        assert_eq!(behind.map(|w| (w.part as usize, w.face)), Some((CHEST, 1)));
        let side = wound_at(
            &pose(0.0),
            feet + Vec3::new(0.3, 1.2, 0.0),
            feet + Vec3::new(6.0, 1.2, 0.0),
            false,
            5,
        );
        assert_eq!(side.map(|w| (w.part, w.face)), Some((4, 2)));
        // Trop loin de lui : rien.
        assert!(wound_at(&pose(0.0), feet + Vec3::X * 3.0, shooter, false, 1).is_none());
        // Les carrés : sur la face, à 2 mm au-dessus, dans ses bords ; ils suivent la jambe.
        for walk in [0.0, 1.2] {
            let p = pose(walk);
            let tris = wound_model(&p, &[w, w2]);
            assert!(tris.len() >= 2 * 6 * 6 && tris.len() % 6 == 0, "{}", tris.len());
            let parts = parts(&p);
            for (k, v) in tris.iter().enumerate() {
                let on = [CHEST, 1].iter().any(|&i| {
                    let (xf, min, max, _) = parts[i];
                    let l = xf.inverse().transform_point3(Vec3::from(v.pos));
                    (l.z - (min[2] - 0.04)).abs() < 1e-3
                        && l.x >= min[0] - 1e-3
                        && l.x <= max[0] + 1e-3
                        && l.y >= min[1] - 1e-3
                        && l.y <= max[1] + 1e-3
                });
                assert!(on, "sommet {k} hors de sa face (marche {walk})");
            }
        }
        // Deux plaies n'ont pas la même tache.
        assert_ne!(wound_cells(&w), wound_cells(&Wound { seed: 8, ..w }));
        assert!(!test_wounds().is_empty());
    }

    /// (v256) Ce que chaque pavé montre du skin : la poitrine les 7 rangées du haut du
    /// corps, le ventre ses 5 rangées du bas sans les colonnes du bord, une jambe trois
    /// colonnes sur quatre (pas la colonne extérieure) ; les pixels restent alignés du haut
    /// en bas. Le dessous de la poitrine est le dessous du corps du skin, entier.
    #[test]
    fn each_box_shows_its_own_piece_of_the_skin() {
        let (chest, belly) = (ZONES[CHEST], ZONES[BELLY]);
        // Devant : (colonne, rangée) du skin du premier et du dernier pixel montrés.
        assert_eq!((chest.pixel(0, 0, 0), chest.pixel(0, 7, 6)), ((0, 0), (7, 6)));
        assert_eq!((belly.pixel(0, 0, 0), belly.pixel(0, 5, 4)), ((1, 7), (6, 11)));
        assert_eq!((belly.pixel(1, 0, 0), belly.pixel(1, 5, 4)), ((1, 7), (6, 11)));
        // Côtés du ventre : toute la profondeur, les rangées du bas.
        assert_eq!((belly.pixel(2, 0, 0), belly.pixel(3, 3, 4)), ((0, 7), (3, 11)));
        // Dessous : celui de la poitrine entier (8 × 4), celui du ventre sans les bords.
        assert_eq!((chest.pixel(5, 0, 0), chest.pixel(5, 7, 3)), ((0, 0), (7, 3)));
        assert_eq!((belly.pixel(5, 0, 0), belly.pixel(5, 5, 3)), ((1, 0), (6, 3)));
        // Jambes, de face : le pixel u = 0 est du côté +x. La jambe droite (+x) cache sa
        // colonne 0 (extérieure), la gauche sa colonne 3 ; elles gardent celles du milieu,
        // sous les colonnes 1 à 6 du ventre.
        let (left, right) = (ZONES[0], ZONES[1]);
        assert_eq!((right.pixel(0, 0, 0).0, right.pixel(0, 2, 0).0), (1, 3));
        assert_eq!((left.pixel(0, 0, 0).0, left.pixel(0, 2, 0).0), (0, 2));
        // De dos (u = 0 du côté −x) : la colonne extérieure aussi.
        assert_eq!((right.pixel(1, 0, 0).0, right.pixel(1, 2, 0).0), (0, 2));
        assert_eq!((left.pixel(1, 0, 0).0, left.pixel(1, 2, 0).0), (1, 3));
        // Dans l'image cuite : chaque pixel du modèle commun lit un pixel de sa partie, et
        // ceux de la poitrine et du ventre ne se marchent pas dessus (mêmes couleurs là où
        // ils se recouvrent : dessus et dessous).
        let skin = test_skin();
        let baked = bake_skin(Some(&skin));
        for (i, &(min, max, part)) in PARTS.iter().enumerate() {
            let z = ZONES[i];
            for (k, (face, _, _, _, nu, nv)) in box_faces(min, max, z.size).into_iter().enumerate()
            {
                let (ax, ay) = atlas(z.skin, k, z.full);
                for v in 0..nv {
                    for u in 0..nu {
                        let (su, sv) = z.pixel(k, u, v);
                        assert!(su < z.width(k) && sv < 12, "{i} {k} {u} {v}");
                        let s = z.full.map(|n| n as u32);
                        let rgb = skin
                            .color(z.skin, k, su as u32, sv as u32, s)
                            .unwrap_or_else(|| super::skin(part, face, su, sv, z.width(k)));
                        let g = grain(k * 4096 + sv * 64 + su + part as usize * 977);
                        let want = srgb(rgb).map(|c| c * g);
                        assert_eq!(baked[(ay + sv) * 64 + ax + su], want, "{i} {k} {u} {v}");
                    }
                }
            }
        }
    }

    #[test]
    fn the_shared_model_draws_what_build_draws() {
        let mesh = player_mesh();
        assert_eq!(mesh.len(), 252);
        for pose in test_poses() {
            let baked = bake_skin(pose.skin.as_deref());
            let inst = instance(&pose, 7, &Prev::still(&pose));
            assert_eq!(inst.skin & 0xffff, 7);
            assert_eq!(inst.skin & PlayerInstance::HOLDS != 0, pose.held.is_some());
            let cpu = build(&pose);
            let mut at = 0;
            for face in mesh.chunks(6) {
                // (v201) Dans une face tournée dans l'autre sens, b et d sont échangés.
                let [a, x, c, _, y, _] = [0, 1, 2, 3, 4, 5].map(|i| face[i]);
                let (b, d) = if x.uv[1] == a.uv[1] { (x, y) } else { (y, x) };
                let xf = shader_part_matrix(a.part, &inst);
                let n = xf.transform_vector3(Vec3::from(a.normal)).normalize();
                let shade = face_shade(n);
                let (pa, pb, pd) = (Vec3::from(a.pos), Vec3::from(b.pos), Vec3::from(d.pos));
                let (ua, ub, ud) = (Vec2::from(a.uv), Vec2::from(b.uv), Vec2::from(d.uv));
                assert!(Vec3::from(c.pos).distance(pb + pd - pa) < 1e-5);
                // Pixels de la face : ceux de sa zone du skin ((b - a) et (d - a) en font un
                // chacun ; v225 : étirés à la taille du pavé).
                let nu = ((ub - ua).length() * 64.0).round() as usize;
                let nv = ((ud - ua).length() * 64.0).round() as usize;
                for v in 0..nv {
                    for u in 0..nu {
                        let quad = &cpu[at..at + 6];
                        at += 6;
                        let point = |u: f32, v: f32| {
                            xf.transform_point3(
                                pa + (pb - pa) * (u / nu as f32) + (pd - pa) * (v / nv as f32),
                            )
                        };
                        for (q, (du, dv)) in [(0, (0.0, 0.0)), (2, (1.0, 1.0))] {
                            let want = point(u as f32 + du, v as f32 + dv);
                            let got = Vec3::from(quad[q].pos);
                            assert!(got.distance(want) < 1e-4, "{got} {want}");
                        }
                        // Pixel du skin lu au centre du pixel (et resté dans sa face).
                        let uv = ua
                            + (ub - ua) * ((u as f32 + 0.5) / nu as f32)
                            + (ud - ua) * ((v as f32 + 0.5) / nv as f32);
                        let r = a.rect;
                        assert!(uv.x > r[0] && uv.x < r[2] && uv.y > r[1] && uv.y < r[3]);
                        let texel = baked[(uv.y * 64.0) as usize * 64 + (uv.x * 64.0) as usize];
                        let want = texel.map(|t| t * shade);
                        for ch in 0..3 {
                            assert!(
                                (quad[0].color[ch] - want[ch]).abs() < 1e-5,
                                "{:?} {want:?}",
                                quad[0].color
                            );
                            let l = quad[0].light[ch] as f32 / 255.0 * 2.0;
                            assert!((l - inst.light[ch]).abs() < 1e-6);
                        }
                    }
                }
            }
            // Ensuite, l'objet tenu (seul dans `held_model`).
            assert_eq!(cpu.len() - at, held_model(&pose, true).len());
        }
    }

    /// (v197) Les faces arrière des joueurs peuvent-elles être écartées par la carte
    /// graphique, comme celles du monde ? Il faut que les deux maillages tournent leurs
    /// triangles dans le même sens. Le monde est la référence : son pipeline écarte déjà
    /// les faces arrière (`cull_mode: Back`, `front_face: Ccw` dans `render/lib.rs`) et
    /// s'affiche bien. L'essai n'a pas besoin de carte graphique.
    #[test]
    fn players_wind_their_triangles_like_the_world() {
        use minelib_world::{BlockId, ChunkPos, World};
        let face_normal = |f: u32| match f {
            0 => Vec3::X,
            1 => -Vec3::X,
            2 => Vec3::Y,
            3 => -Vec3::Y,
            4 => Vec3::Z,
            _ => -Vec3::Z,
        };
        // Le sens du monde : un bloc posé, toutes ses faces.
        let mut w = World::new(0);
        w.insert_empty(ChunkPos { x: 0, z: 0 });
        for z in 2..6 {
            for x in 2..6 {
                w.set(x, 20, z, BlockId::STONE);
            }
        }
        w.set(4, 22, 4, BlockId::STONE);
        let m = minelib_world::mesh::mesh_chunk(&w, ChunkPos { x: 0, z: 0 });
        let mut n = 0;
        for t in m.indices.chunks(3) {
            let v = [0, 1, 2].map(|k| m.vertices[t[k] as usize]);
            let f = v[0].face();
            if f > 5 {
                continue; // plantes en croix, torches penchées : pas une face de cube
            }
            let p = v.map(|x| Vec3::from(x.pos));
            let wind = (p[1] - p[0]).cross(p[2] - p[0]);
            if wind.length_squared() < 1e-9 {
                continue;
            }
            assert!(
                wind.normalize().dot(face_normal(f)) > 0.9,
                "le monde tourne ce triangle dans l'autre sens : face {f}"
            );
            n += 1;
        }
        assert!(n > 20, "l'essai n'a regardé que {n} triangles du monde");
        // Le même sens pour les joueurs, avec la matrice du shader : vers le dehors du pavé.
        // (v201) Pas vers la normale des sommets : celle de l'ombrage (`du × dv`) pointe vers
        // l'intérieur pour quatre faces sur six, et l'essai passait quand même.
        let mesh = player_mesh();
        for pose in test_poses() {
            let inst = instance(&pose, 0, &Prev::still(&pose));
            for t in mesh.chunks(3) {
                let xf = shader_part_matrix(t[0].part, &inst);
                let p = [0, 1, 2].map(|k| xf.transform_point3(Vec3::from(t[k].pos)));
                let wind = (p[1] - p[0]).cross(p[2] - p[0]);
                let (mn, mx, _) = PARTS[t[0].part as usize];
                let center = xf.transform_point3((Vec3::from(mn) + Vec3::from(mx)) / 2.0);
                let out = (p[0] + p[1] + p[2]) / 3.0 - center;
                assert!(wind.length_squared() > 1e-12, "triangle plat du joueur");
                assert!(
                    wind.dot(out) > 0.0,
                    "joueur : triangle tourné vers l'intérieur (partie {})",
                    t[0].part
                );
                let n = xf.transform_vector3(Vec3::from(t[0].normal));
                assert!(
                    wind.normalize().dot(n.normalize()).abs() > 0.9,
                    "normale de travers"
                );
            }
        }
    }

    #[test]
    fn the_graphics_card_draws_players_like_the_model() {
        let mesh = player_mesh();
        for (n, pose) in test_poses().into_iter().enumerate() {
            let body = build(&pose);
            let body = &body[..body.len() - held_model(&pose, true).len()];
            let skins = vec![(3, bake_skin(pose.skin.as_deref()))];
            let players = [instance(&pose, 3, &Prev::still(&pose))];
            for view in 0..3 {
                let a = pose.yaw + [0.4, 2.8, -1.7][view] + n as f32;
                let target = pose.feet + Vec3::Y * 1.0;
                let eye = target + Vec3::new(a.sin(), [0.3, 0.6, -0.2][view], a.cos()) * 3.2;
                let size = 256;
                let proj = Mat4::perspective_infinite_reverse_rh(60f32.to_radians(), 1.0, 0.05);
                let vp = proj * Mat4::look_at_rh(eye, target, Vec3::Y);
                let Some([cpu, gpu]) =
                    minelib_render::draw_offscreen(vp, size, body, &mesh, &players, &skins)
                else {
                    eprintln!("pas de carte graphique : essai sauté");
                    return;
                };
                let (mut covered, mut alone, mut both, mut off) = (0, 0, 0, 0);
                for (p, q) in cpu.iter().zip(&gpu) {
                    let (cp, cq) = (p[3] > 0.5, q[3] > 0.5);
                    covered += usize::from(cp);
                    alone += usize::from(cp != cq);
                    if cp && cq {
                        both += 1;
                        let d = (0..3).map(|i| (p[i] - q[i]).abs()).fold(0.0, f32::max);
                        off += usize::from(d > 0.01);
                    }
                }
                eprintln!(
                    "pose {n}, vue {view} : {covered} pixels, {alone} d'un seul côté, \
                     {off} de couleur différente"
                );
                // (v234) 1 500 : le corps est un peu plus mince (tête plus grosse, même
                // taille en tout ; vue de haut, la pose 2 couvre 1 927 pixels).
                assert!(covered > 1500, "joueur trop petit à l'écran : {covered}");
                // Les écarts restants sont aux frontières des pixels du skin (le pixel de
                // l'écran à cheval prend l'un ou l'autre), et sur les contours.
                assert!(alone * 100 <= covered, "contours : {alone} sur {covered}");
                assert!(off * 100 <= both * 4, "couleurs : {off} sur {both}");
            }
        }
    }

    #[test]
    fn model_is_player_sized_and_faces_the_look_direction() {
        let pose = Pose {
            feet: Vec3::new(10.0, 64.0, 5.0),
            yaw: 0.0,
            pitch: 0.0,
            walk: 0.0,
            swing: 0.0,
            crouch: 0.0,
            light: [1.0; 3],
            held: None,
            skin: None,
            bow: 0.0,
        };
        let v = build(&pose);
        assert!(v.len() > 5000 && v.len().is_multiple_of(3));
        let ys: Vec<f32> = v.iter().map(|p| p.pos[1]).collect();
        let (lo, hi) = (
            ys.iter().cloned().fold(f32::MAX, f32::min),
            ys.iter().cloned().fold(f32::MIN, f32::max),
        );
        assert!(
            (lo - 64.0).abs() < 1e-3 && (hi - 65.8).abs() < 1e-3,
            "{lo} {hi}"
        );
        // Les yeux (blanc des yeux) sont du côté du regard (−z pour un lacet nul).
        let white = srgb(EYE_WHITE);
        let eye_z: Vec<f32> = v
            .iter()
            .filter(|p| {
                p.color
                    .iter()
                    .zip(white)
                    .all(|(c, w)| (c - w * 0.78).abs() < 0.12)
            })
            .map(|p| p.pos[2])
            .collect();
        assert!(
            !eye_z.is_empty() && eye_z.iter().all(|&z| z < 5.0),
            "{eye_z:?}"
        );
    }
    /// Même essai que `dl_box_shadow` (dynlights.wgsl), pour un point du joueur : le
    /// segment `p` → `light` traverse-t-il une boîte (hors boîtes qui contiennent `p`) ?
    fn shadowed(boxes: &[ShadowBox], p: Vec3, light: Vec3) -> bool {
        shadowed_on(boxes, p, light, true)
    }

    /// `entity` : `p` est sur le joueur (sinon sur le monde : une boîte qui le contient le
    /// cache aussi).
    fn shadowed_on(boxes: &[ShadowBox], p: Vec3, light: Vec3, entity: bool) -> bool {
        boxes.iter().any(|b| {
            let row = |i: usize, q: Vec3| Vec4::from(b.to_unit[i]).dot(q.extend(1.0));
            let a = Vec3::new(row(0, p), row(1, p), row(2, p));
            let d = Vec3::new(row(0, light), row(1, light), row(2, light)) - a;
            let inside = entity && a.cmpgt(Vec3::ZERO).all() && a.cmplt(Vec3::ONE).all();
            let inv = Vec3::select(
                d.abs().cmplt(Vec3::splat(1e-6)),
                Vec3::splat(1e6),
                d.recip(),
            );
            let (t0, t1) = (-a * inv, (Vec3::ONE - a) * inv);
            let near = t0.min(t1).max_element();
            let far = t0.max(t1).min_element();
            !inside && far >= near.max(0.0) && near < 1.0
        })
    }

    #[test]
    fn the_head_shadows_the_shoulders_exactly_whatever_the_yaw() {
        for yaw_deg in [0.0f32, 30.0, 45.0, 135.0] {
            let yaw = yaw_deg.to_radians();
            let pose = Pose {
                feet: Vec3::new(10.0, 64.0, 5.0),
                yaw,
                pitch: 0.0,
                walk: 0.0,
                swing: 0.0,
                crouch: 0.0,
                light: [1.0; 3],
                held: None,
                skin: None,
                bow: 0.0,
            };
            let boxes = shadow_boxes(&pose);
            // Immobile : les deux jambes ne font qu'une boîte (v256 : six avec le ventre).
            assert_eq!(boxes.len(), 6);
            // Repère du modèle (pixels) → monde ; l'avant est −z.
            let base = Mat4::from_translation(pose.feet)
                * Mat4::from_rotation_y(-yaw)
                * Mat4::from_scale(Vec3::splat(PX));
            let at = |x: f32, y: f32, z: f32| base.transform_point3(Vec3::new(x, y, z));
            // Lumière devant et au-dessus (45°), à 3 blocs.
            let front = -BODY_D / 2.0;
            let light = at(0.0, NECK_Y, front)
                + (at(0.0, NECK_Y, -10.0) - at(0.0, NECK_Y, front)).normalize() * 2.1
                + Vec3::Y * 2.1;
            // Devant du torse, décollé comme dans le shader (0,02 bloc).
            let off = (at(0.0, 0.0, -1.0) - at(0.0, 0.0, 0.0)).normalize() * 0.02;
            // Juste sous le menton. (v225) La tête dépassait du torse d'un demi-pixel : l'ombre
            // y tombait. (v264) La tête a la profondeur du corps (4 colonnes de son skin) : elle
            // ne dépasse plus, le devant du torse est éclairé jusqu'en haut.
            assert!((HEAD_S - BODY_D).abs() < 1e-5);
            assert!(
                !shadowed(&boxes, at(0.0, NECK_Y - 0.1, front) + off, light),
                "menton, {yaw_deg}°"
            );
            // Plus bas sur le torse et sur les côtés : rien (avant : un triangle).
            let side = BODY_W / 2.0 - 0.75;
            for (x, y) in [
                (0.0, NECK_Y - 6.0),
                (-side, NECK_Y - 3.0),
                (side, NECK_Y - 3.0),
                (0.0, LEG + 2.0),
            ] {
                assert!(
                    !shadowed(&boxes, at(x, y, front) + off, light),
                    "torse ({x}, {y}), {yaw_deg}°"
                );
            }
            // Le dessus de l'épaule gauche, lumière au-dessus de l'épaule droite : la tête
            // lui fait de l'ombre ; lumière juste au-dessus d'elle : non.
            let shoulder = at(-ARM_X, NECK_Y, 0.0) + Vec3::Y * 0.02;
            assert!(
                shadowed(&boxes, shoulder, at(10.0, 40.0, 0.0)),
                "épaule, {yaw_deg}°"
            );
            assert!(
                !shadowed(&boxes, shoulder, at(-ARM_X, 60.0, 0.0)),
                "épaule, {yaw_deg}°"
            );
        }
        // Pieds posés : le sol sous eux (dans la boîte des jambes, décollé de 0,02 bloc
        // comme dans le shader) reste dans l'ombre des jambes, comme le sol autour ; pas de
        // trou clair à leur place.
        let pose = Pose {
            feet: Vec3::new(10.0, 64.0, 5.0),
            yaw: 0.4,
            pitch: 0.0,
            walk: 0.0,
            swing: 0.0,
            crouch: 0.0,
            light: [1.0; 3],
            held: None,
            skin: None,
            bow: 0.0,
        };
        let boxes = shadow_boxes(&pose);
        let lamp = pose.feet + Vec3::new(1.5, 3.0, 2.0);
        for (dx, dz) in [(0.0, 0.0), (0.06, 0.04), (-0.06, -0.03)] {
            let ground = pose.feet + Vec3::new(dx, 0.02, dz);
            assert!(shadowed_on(&boxes, ground, lamp, false), "sol ({dx}, {dz})");
        }
        // Le sol à côté, que l'ombre n'atteint pas : éclairé.
        assert!(!shadowed_on(
            &boxes,
            pose.feet + Vec3::new(1.5, 0.02, -1.0),
            lamp,
            false
        ));
        // En marchant, les jambes ne sont plus parallèles : une boîte chacune (v256 : sept
        // avec le ventre).
        let walking = Pose {
            feet: Vec3::ZERO,
            yaw: 0.3,
            pitch: 0.0,
            walk: 1.2,
            swing: 1.0,
            crouch: 0.0,
            light: [1.0; 3],
            held: None,
            skin: None,
            bow: 0.0,
        };
        assert_eq!(shadow_boxes(&walking).len(), 7);
    }
}

#[cfg(test)]
mod held_tests {
    use super::*;

    #[test]
    fn torch_points_up_and_forward() {
        let pose = Pose {
            feet: Vec3::ZERO,
            yaw: 0.0,
            pitch: 0.0,
            walk: 0.0,
            swing: 0.0,
            crouch: 0.0,
            light: [1.0; 3],
            held: Some(Held::Torch([255, 200, 100])),
            skin: None,
            bow: 0.0,
        };
        let hand = hand_frame(&pose).unwrap().transform_point3(Vec3::ZERO);
        let tip = torch_tip(&pose).unwrap();
        let shoulder = (LEG + SHOULDER_Y) * PX;
        eprintln!("main {hand:?} flamme {tip:?}");
        // Main devant le corps (−z), plus bas que l'épaule (v239 : le bras tendu, pointé plus
        // bas que le regard), flamme au-dessus de la main (la torche tenue, deux fois plus
        // petite qu'en v238, l'est par le haut de son bâton) et encore plus devant.
        assert!(hand.z < -0.3 && hand.y < shoulder - 0.1, "{hand:?}");
        assert!(tip.y > hand.y + 0.03 && tip.z < hand.z, "{tip:?}");
    }
}

/// Bombe posée ou lâchée : boîtier kaki, voyant rouge (allumé quand `blink`).
pub fn bomb(pos: Vec3, blink: bool, light: [f32; 3]) -> Vec<EntityVertex> {
    let mut out = Vec::new();
    let xf = Mat4::from_translation(pos) * Mat4::from_scale(Vec3::splat(1.0 / 16.0));
    emit_item(
        &mut out,
        xf,
        [-4.0, 0.0, -3.0],
        [4.0, 3.0, 3.0],
        RayBoxKind::Flat([96, 92, 62]),
        light,
    );
    emit_item(
        &mut out,
        xf,
        [-3.0, 3.0, -2.0],
        [1.0, 3.5, 2.0],
        RayBoxKind::Flat([40, 40, 40]),
        light,
    );
    let led = if blink {
        RayBoxKind::Glow([255, 40, 30])
    } else {
        RayBoxKind::Flat([90, 20, 20])
    };
    emit_item(&mut out, xf, [2.0, 3.0, -1.0], [3.0, 3.6, 0.0], led, light);
    out
}

/// (v241) Foudre de Zeus portée dans le dos (il l'a sur lui sans la tenir), « comme la bombe
/// en FPS » : son repère dans le monde (son milieu à l'origine, l'éclair le long de +y, en
/// blocs). En travers du dos comme sur son icône (« dans le dos du joueur, en travers comme
/// l'item ») : à 45°, du bas à gauche au haut à droite pour qui le voit de dos, à un pixel du
/// dos ; elle suit le corps (accroupi, penché).
pub fn zeus_back_frame(pose: &Pose) -> Option<Mat4> {
    let body = parts(pose).into_iter().find(|p| p.3 == Part::Body)?;
    // Repère du corps en pixels (y de la hanche aux épaules, z = 2 : le dos).
    Some(
        body.0
            * Mat4::from_translation(Vec3::new(0.0, 7.0, 3.2))
            * Mat4::from_rotation_z(-ZEUS_BACK_TILT)
            * Mat4::from_scale(Vec3::splat(1.0 / PX)),
    )
}

/// (v241) Penché de la Foudre de Zeus dans le dos (rad, depuis la verticale) : celui de son
/// icône (45°). (v244) « Mets-le à moitié plus droit » : la moitié, 22,5°.
pub const ZEUS_BACK_TILT: f32 = std::f32::consts::FRAC_PI_8;

/// (v179) Bombe portée sur le dos : debout (sa longueur le long du dos), son dessous contre
/// le dos du joueur, le clavier vers l'arrière. Le modèle est celui de la main, à la même
/// taille ; elle suit le corps (accroupi, penché).
pub fn bomb_on_back(pose: &Pose) -> Vec<EntityVertex> {
    let mut out = Vec::new();
    let Some(body) = parts(pose).into_iter().find(|p| p.3 == Part::Body) else {
        return out;
    };
    let g = gear(crate::cs::W_BOMB);
    // Repère du corps en pixels (y de la hanche aux épaules, z = 2 : le dos).
    let half = -g.min.y * g.scale / PX;
    // Longueur (x du modèle) debout, dessus (y) vers l'arrière, largeur (z) en travers.
    let turn = Mat4::from_cols(Vec4::Y, Vec4::Z, Vec4::X, Vec4::W);
    let xf = body.0
        * Mat4::from_translation(Vec3::new(0.0, 6.5, 2.0 + half))
        * Mat4::from_scale(Vec3::splat(g.scale / PX))
        * turn;
    tris_in_world(&mut out, xf, &g.tris, pose.light);
    out
}

/// Modèle en triangles (le même qu'en main) placé dans le monde par `xf`, éclairé comme
/// la vue à la première personne.
pub fn tris_in_world(
    out: &mut Vec<EntityVertex>,
    xf: Mat4,
    tris: &[crate::weapons::Tri],
    light: [f32; 3],
) {
    let sun = Vec3::new(0.3, 0.9, 0.4).normalize();
    for t in tris {
        let n = (t.p[1] - t.p[0]).cross(t.p[2] - t.p[0]).normalize_or_zero();
        let n = xf.transform_vector3(n).normalize_or_zero();
        let shade = 0.45 + 0.55 * n.dot(sun).abs();
        let c = [0, 1, 2].map(|i| (t.color[i] * 2.2 + 0.015) * shade);
        let vl = EntityVertex::light(light, true);
        for p in t.p {
            out.push(EntityVertex {
                pos: xf.transform_point3(p).to_array(),
                color: c,
                uv: [0.0, 0.0],
                layer: -1,
                light: vl,
            });
        }
    }
}

/// Objet posé au sol (ou en train de retomber) : le modèle `tris` couché sur le côté,
/// tourné selon `turn`, `pos` étant le point du sol où il repose. `spin` fait tourner
/// l'objet pendant sa chute (0 une fois posé).
pub fn item_on_ground(
    out: &mut Vec<EntityVertex>,
    tris: &[crate::weapons::Tri],
    pos: Vec3,
    turn: f32,
    lie: bool,
    spin: f32,
    light: [f32; 3],
) {
    let tilt = if lie {
        Mat4::from_rotation_z(std::f32::consts::FRAC_PI_2)
    } else {
        Mat4::IDENTITY
    };
    let base = Mat4::from_rotation_y(turn) * Mat4::from_rotation_x(spin) * tilt;
    // Le point le plus bas touche le sol.
    let low = tris
        .iter()
        .flat_map(|t| t.p)
        .map(|p| base.transform_point3(p).y)
        .fold(f32::MAX, f32::min);
    let low = if low.is_finite() { low } else { 0.0 };
    let xf = Mat4::from_translation(pos - Vec3::Y * low) * base;
    tris_in_world(out, xf, tris, light);
}

/// Arme posée au sol : silhouette sombre de sa longueur, tournée selon `turn`.
pub fn gun_on_ground(pos: Vec3, length: f32, turn: f32, light: [f32; 3]) -> Vec<EntityVertex> {
    let mut out = Vec::new();
    let xf = Mat4::from_translation(pos)
        * Mat4::from_rotation_y(turn)
        * Mat4::from_scale(Vec3::splat(1.0 / 16.0));
    let half = (length * 16.0 / 2.0).max(3.0);
    emit_item(
        &mut out,
        xf,
        [-half, 0.0, -0.8],
        [half, 1.6, 0.8],
        RayBoxKind::Flat([46, 46, 50]),
        light,
    );
    // Crosse et poignée.
    emit_item(
        &mut out,
        xf,
        [-half * 0.4, 0.0, -0.6],
        [-half * 0.4 + 1.6, 0.6, 2.8],
        RayBoxKind::Flat([60, 48, 36]),
        light,
    );
    out
}
