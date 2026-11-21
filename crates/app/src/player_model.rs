//! Modèle du joueur vu à la 3e personne (F5) : tête, corps, bras et jambes en pavés, aux
//! proportions classiques des jeux de blocs (32 pixels de haut pour 1,8 bloc), avec un skin
//! original dessiné pixel par pixel. Les bras et les jambes se balancent en marchant, la
//! tête suit le regard, le corps se penche quand on s'accroupit.

use glam::{Mat4, Vec3, Vec4};
use minelib_render::{EntityVertex, PlayerInstance, PlayerVertex, RayBox, RayBoxKind, ShadowBox};
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
fn emit_box(
    out: &mut Vec<EntityVertex>,
    xf: Mat4,
    min: [f32; 3],
    max: [f32; 3],
    part: Part,
    light: [f32; 3],
    tex: Option<(&crate::skin::Skin, usize)>,
) {
    emit_box_layer(out, xf, min, max, part, light, tex, None, 1);
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
    part: Part,
    light: [f32; 3],
    tex: Option<(&crate::skin::Skin, usize)>,
    layer: Option<bool>,
    lod: usize,
) {
    let [x0, y0, z0] = min;
    let [x1, y1, z1] = max;
    let lit = EntityVertex::light(light, true);
    // Nombre de pixels du skin par côté (une boîte agrandie garde 8 pixels : chacun un
    // peu plus grand).
    let (w, h, d) = (
        (x1 - x0).round() as usize,
        (y1 - y0).round() as usize,
        (z1 - z0).round() as usize,
    );
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
                let g = grain(k * 4096 + v * 64 + u + part as usize * 977);
                let size = [w as u32, h as u32, d as u32];
                let rgb = match (tex, layer) {
                    // De loin : un carré par groupe de `lod` × `lod` pixels, à leur couleur
                    // moyenne (vide si la plupart sont transparents).
                    (Some((t, i)), Some(_)) if lod > 1 => {
                        let (mut sum, mut n) = ([0u32; 3], 0u32);
                        for dv2 in 0..lod.min(nv - v) {
                            for du2 in 0..lod.min(nu - u) {
                                if let Some(c) =
                                    t.color(i, k, (u + du2) as u32, (v + dv2) as u32, size)
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
                            t.layer_color(i, k, u as u32, v as u32, size, true)
                        } else {
                            t.color(i, k, u as u32, v as u32, size)
                        };
                        match c {
                            Some(c) => c,
                            None => continue,
                        }
                    }
                    _ => tex
                        .and_then(|(t, i)| t.color(i, k, u as u32, v as u32, size))
                        .unwrap_or_else(|| skin(part, face, u, v, nu)),
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
/// personne, à la place même où la voient les miroirs et les autres joueurs.
fn aims_right(pose: &Pose) -> bool {
    matches!(pose.held, Some(Held::Flashlight(_)))
}

/// (v177) Bras tendus d'une arme à feu : rotation autour de la verticale (radians) qui
/// ramène la main vers le milieu du corps, pour le bras droit et pour le gauche (qui vient
/// tenir l'arme devant). Les mêmes que `AIM_TURN_*` de players.wgsl.
const AIM_TURN_RIGHT: f32 = 0.15;
const AIM_TURN_LEFT: f32 = -0.5;

/// Hauteur d'une jambe (pixels) : la hanche est à cette hauteur quand on est debout.
const LEG: f32 = 12.0;

/// Pavés des six parties, dans l'ordre de `parts` et de `Skin::color` (jambe gauche, jambe
/// droite, corps, bras gauche, bras droit, tête) : coins (pixels, repère de la partie) et
/// sorte.
const PARTS: [([f32; 3], [f32; 3], Part); 6] = [
    ([-2.0, -12.0, -2.0], [2.0, 0.0, 2.0], Part::Leg),
    ([-2.0, -12.0, -2.0], [2.0, 0.0, 2.0], Part::Leg),
    ([-4.0, 0.0, -2.0], [4.0, 12.0, 2.0], Part::Body),
    ([-2.0, -10.0, -2.0], [2.0, 2.0, 2.0], Part::Arm),
    ([-2.0, -10.0, -2.0], [2.0, 2.0, 2.0], Part::Arm),
    ([-4.0, 0.0, -4.0], [4.0, 8.0, 4.0], Part::Head),
];

/// Pavés du modèle dans cette pose : repère (pixels → monde), coins (pixels), partie.
fn parts(pose: &Pose) -> Vec<(Mat4, [f32; 3], [f32; 3], Part)> {
    let mut out = Vec::with_capacity(6);
    // Repère du modèle : x à droite, y en haut, z vers l'arrière (le regard est −z).
    let base = Mat4::from_translation(pose.feet)
        * Mat4::from_rotation_y(-pose.yaw)
        * Mat4::from_scale(Vec3::splat(PX));
    let swing = pose.walk.sin() * 0.75 * pose.swing;
    let crouch = pose.crouch.clamp(0.0, 1.0);
    let lean = 0.45 * crouch;
    let drop = 0.3 / PX * crouch * 0.6;
    // Jambes : pivot à la hanche (y = 12 ; elle descend de `drop` accroupi), balancement
    // opposé. (v189) La jambe reste un seul pavé : elle se tasse de `drop` au lieu de se
    // faire recouvrir au quart par le bas du corps, qui laissait voir un pli à la hanche.
    // Les pieds restent au sol.
    let shrink = (LEG - drop) / LEG;
    for (side, sign) in [(-2.0f32, 1.0f32), (2.0, -1.0)] {
        let xf = base
            * Mat4::from_translation(Vec3::new(side, LEG - drop, 0.0))
            * Mat4::from_rotation_x(swing * sign)
            * Mat4::from_scale(Vec3::new(1.0, shrink, 1.0));
        let (min, max, part) = PARTS[out.len()];
        out.push((xf, min, max, part));
    }
    // Haut du corps : penché vers l'avant quand on s'accroupit (pivot à la hanche).
    let upper = base
        * Mat4::from_translation(Vec3::new(0.0, 12.0 - drop, 0.0))
        * Mat4::from_rotation_x(-lean);
    let (min, max, part) = PARTS[2];
    out.push((upper, min, max, part));
    // Bras : pivot à l'épaule (y = 22 depuis la hanche : 10), balancement inverse des
    // jambes ; le bras droit (x +6) tient l'objet un peu levé devant lui.
    let aim = aims(pose);
    let aim_right = aims_right(pose);
    for (side, sign) in [(-6.0f32, -1.0f32), (6.0, 1.0)] {
        if aim || (aim_right && side > 0.0) {
            // (v177) Arme à feu : les deux bras tendus là où l'on regarde (le haut du corps
            // penché compris), un peu tournés vers l'arme, qui est tenue devant, entre les
            // deux mains ; ils ne se balancent presque plus en marchant.
            let turn = if side > 0.0 {
                AIM_TURN_RIGHT
            } else {
                AIM_TURN_LEFT
            };
            let xf = upper
                * Mat4::from_translation(Vec3::new(side, 10.0, 0.0))
                * Mat4::from_rotation_y(turn)
                * Mat4::from_rotation_x(
                    std::f32::consts::FRAC_PI_2
                        + pose.pitch.clamp(-1.4, 1.4)
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
            1.15
        } else if side > 0.0 && pose.held.is_some() {
            (0.65 + 0.6 * pose.pitch.clamp(-1.4, 1.4)).clamp(0.0, 2.0)
        } else {
            0.0
        };
        let swing_k = if raise != 0.0 { 0.3 } else { 1.0 };
        let xf = upper
            * Mat4::from_translation(Vec3::new(side, 10.0, 0.0))
            * Mat4::from_rotation_x(swing * sign * swing_k + lean * 0.6 + raise);
        let (min, max, part) = PARTS[out.len()];
        out.push((xf, min, max, part));
    }
    // Tête : pivot au cou, suit le regard (tangage), sans suivre la penche du corps.
    let head = base
        * Mat4::from_translation(Vec3::new(0.0, 24.0 - drop * 1.6, -lean * 4.0))
        * Mat4::from_rotation_x(pose.pitch.clamp(-1.4, 1.4));
    let (min, max, part) = PARTS[5];
    out.push((head, min, max, part));
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
        .map(|p| p.0 * Mat4::from_translation(Vec3::new(0.0, -10.0, 0.0)))
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
    item_frame(pose, hand, Mat4::IDENTITY, 2.0)
}

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
    // Pavé du bras (pixels) : x de −2 à 2, y de −10 (poing) à 2 (épaule), z de −2 (devant)
    // à 2.
    let unit = Mat4::from_cols(
        glam::Vec4::new(4.0, 0.0, 0.0, 0.0),
        glam::Vec4::new(0.0, 0.0, -4.0, 0.0),
        glam::Vec4::new(0.0, 12.0, 0.0, 0.0),
        glam::Vec4::new(-2.0, -10.0, 2.0, 1.0),
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
        let fist = |arm: &Mat4| arm.transform_point3(Vec3::new(0.0, -8.0, 0.0));
        (fist(arms.get(1)?) + fist(&arms[0])) * 0.5 + look.transform_vector3(Vec3::Y) * (2.0 * PX)
    } else {
        // Le reste : tenu au bout du poing droit.
        arms.get(1)?.transform_point3(Vec3::new(0.0, -9.5, 0.0))
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
            // bloc) éclairé par sa flamme (3 × 5 seizièmes), à la taille du monde.
            let k = 32.0 / 1.8 / 16.0;
            let top = flame.iter().copied().max().unwrap_or(1).max(1) as u32;
            let bright = flame.map(|c| (c as u32 * 255 / top) as u8);
            let t = torch_frame(pose, hand);
            vec![
                (
                    t,
                    [-k, -3.0, -k],
                    [k, -3.0 + 8.0 * k, k],
                    RayBoxKind::Block {
                        layer: BlockId::TORCH.def().tex[1] as u32,
                        uv: [0.4375, 0.5, 0.5625, 1.0],
                        glow: true,
                    },
                ),
                (
                    t,
                    [-1.5 * k, -3.0 + 8.0 * k, -1.5 * k],
                    [1.5 * k, -3.0 + 13.0 * k, 1.5 * k],
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
        Held::Wand(c) => vec![(
            item_frame(pose, hand, crate::hand::wand_rotation(), 2.0),
            [-0.75, -2.0, -0.75],
            [0.75, 9.0, 0.75],
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
    Some(Mat4::from_translation(at) * Mat4::from_rotation_y(-pose.yaw))
}

/// Position (monde) de la flamme d'une torche tenue en main : la lumière part de là.
pub fn torch_tip(pose: &Pose) -> Option<Vec3> {
    match (hand_frame(pose), pose.held) {
        (Some(hand), Some(Held::Torch(_))) => {
            Some(torch_frame(pose, hand).transform_point3(Vec3::new(0.0, 8.0, 0.0)))
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
    for (min, max, c) in [
        ([-4.0, 0.0, -2.0], [4.0, 12.0, 2.0], PANTS),
        ([-8.0, 12.0, -2.0], [8.0, 24.0, 2.0], SHIRT),
        ([-4.0, 24.0, -4.0], [4.0, 32.0, 4.0], SKIN),
    ] {
        let rgb = [(c >> 16) as u8, (c >> 8) as u8, c as u8];
        emit_item(&mut out, base, min, max, RayBoxKind::Flat(rgb), pose.light);
    }
    out
}

/// Triangles du modèle du joueur dans cette pose (avec l'objet tenu).
pub fn build(pose: &Pose) -> Vec<EntityVertex> {
    let mut out = Vec::with_capacity(12_000);
    // Ordre de `parts` : jambe gauche, jambe droite, corps, bras gauche, bras droit, tête
    // (les mêmes numéros que `Skin::color`).
    for (i, (xf, min, max, part)) in parts(pose).into_iter().enumerate() {
        let tex = pose.skin.as_deref().map(|s| (s, i));
        emit_box(&mut out, xf, min, max, part, pose.light, tex);
    }
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

/// Modèle commun des autres joueurs (216 sommets), à envoyer une fois à la carte.
pub fn player_mesh() -> Vec<PlayerVertex> {
    let mut out = Vec::with_capacity(216);
    for (i, &(min, max, _)) in PARTS.iter().enumerate() {
        let size = box_size(min, max);
        for (k, (_, o, du, dv, nu, nv)) in box_faces(min, max, size).into_iter().enumerate() {
            let (o, du, dv) = (Vec3::from(o), Vec3::from(du), Vec3::from(dv));
            let (fx, fy) = atlas(i, k, size);
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
        let size = box_size(min, max);
        let s = size.map(|n| n as u32);
        for (k, (face, _, _, _, nu, nv)) in box_faces(min, max, size).into_iter().enumerate() {
            let (fx, fy) = atlas(i, k, size);
            for v in 0..nv {
                for u in 0..nu {
                    let rgb = tex
                        .and_then(|t| t.color(i, k, u as u32, v as u32, s))
                        .unwrap_or_else(|| skin(part, face, u, v, nu));
                    let g = grain(k * 4096 + v * 64 + u + part as usize * 977);
                    out[(fy + v) * 64 + fx + u] = srgb(rgb).map(|c| c * g);
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
    } else if aims_right(pose) {
        // (v215) Lampe torche : le bras droit tendu.
        PlayerInstance::HOLDS | PlayerInstance::AIMS_RIGHT
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
    // Avec un skin : chaque partie le lit (mêmes numéros que `build`, plus un).
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
            (xf, min, max, kind, if skinned { i as u8 + 1 } else { 0 })
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
            .map(|(xf, min, max, kind)| (xf, min, max, kind, 0)),
    )
    .map(|(xf, min, max, kind, part)| RayBox {
        world_to_local: xf.inverse(),
        min,
        max,
        kind,
        part,
    })
    .collect()
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
        let lean = 0.45 * crouch;
        let drop = 0.3 / px * crouch * 0.6;
        let pitch = p.pitch.clamp(-1.4, 1.4);
        let shrink = Mat4::from_scale(Vec3::new(1.0, (12.0 - drop) / 12.0, 1.0));
        if part == 0 {
            return base * t(-2.0, 12.0 - drop, 0.0) * Mat4::from_rotation_x(swing) * shrink;
        }
        if part == 1 {
            return base * t(2.0, 12.0 - drop, 0.0) * Mat4::from_rotation_x(-swing) * shrink;
        }
        let upper = base * t(0.0, 12.0 - drop, 0.0) * Mat4::from_rotation_x(-lean);
        if part == 2 {
            return upper;
        }
        if (part == 3 || part == 4) && p.skin & 0x20000 != 0 {
            let (side, turn, sign) = if part == 4 {
                (6.0, 0.15, 1.0)
            } else {
                (-6.0, -0.5, -1.0)
            };
            return upper
                * t(side, 10.0, 0.0)
                * Mat4::from_rotation_y(turn)
                * Mat4::from_rotation_x(1.570_796_4 + pitch + lean + swing * sign * 0.1);
        }
        if part == 3 {
            return upper * t(-6.0, 10.0, 0.0) * Mat4::from_rotation_x(-swing + lean * 0.6);
        }
        if part == 4 {
            let (mut raise, mut k) = (0.0, 1.0);
            if p.skin & 0x10000 != 0 {
                raise = (0.65 + 0.6 * pitch).clamp(0.0, 2.0);
                if raise != 0.0 {
                    k = 0.3;
                }
            }
            return upper
                * t(6.0, 10.0, 0.0)
                * Mat4::from_rotation_x(swing * k + lean * 0.6 + raise);
        }
        base * t(0.0, 24.0 - drop * 1.6, -lean * 4.0) * Mat4::from_rotation_x(pitch)
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
    fn test_poses() -> Vec<Pose> {
        let skin = test_skin();
        let ak = crate::weapons::WEAPONS
            .iter()
            .position(|w| w.name == "AK-47")
            .unwrap() as u8;
        (0..4)
            .map(|n| Pose {
                feet: Vec3::new(3.0 + n as f32, 70.0, -5.0),
                yaw: 0.7 + n as f32 * 2.1,
                pitch: [-0.5, 0.3, 1.2, -0.2][n],
                walk: 1.3 + n as f32,
                swing: [0.0, 0.8, 0.5, 0.6][n],
                crouch: [0.0, 0.6, 1.0, 0.0][n],
                // Exactement représentable au format des sommets (k / 127,5).
                light: [0.8, 0.4, 1.2],
                held: [
                    None,
                    Some(Held::Gear(ak)),
                    Some(Held::Block(BlockId::STONE)),
                    Some(Held::Gear(crate::cs::W_KNIFE)),
                ][n],
                skin: (n > 0).then(|| skin.clone()),
            })
            .collect()
    }

    #[test]
    fn the_shared_model_draws_what_build_draws() {
        let mesh = player_mesh();
        assert_eq!(mesh.len(), 216);
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
                assert_eq!(Vec3::from(c.pos), pb + pd - pa);
                // Pixels de la face : (b - a) et (d - a) font un pixel de long chacun.
                let nu = (pb - pa).length().round() as usize;
                let nv = (pd - pa).length().round() as usize;
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
                assert!(covered > 2000, "joueur trop petit à l'écran : {covered}");
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
            };
            let boxes = shadow_boxes(&pose);
            // Immobile : les deux jambes ne font qu'une boîte.
            assert_eq!(boxes.len(), 5);
            // Repère du modèle (pixels) → monde ; l'avant est −z.
            let base = Mat4::from_translation(pose.feet)
                * Mat4::from_rotation_y(-yaw)
                * Mat4::from_scale(Vec3::splat(PX));
            let at = |x: f32, y: f32, z: f32| base.transform_point3(Vec3::new(x, y, z));
            // Lumière devant et au-dessus (45°), à 3 blocs.
            let light = at(0.0, 24.0, -2.0)
                + (at(0.0, 24.0, -10.0) - at(0.0, 24.0, -2.0)).normalize() * 2.1
                + Vec3::Y * 2.1;
            // Devant du torse (z = −2), décollé comme dans le shader (0,02 bloc).
            let off = (at(0.0, 0.0, -1.0) - at(0.0, 0.0, 0.0)).normalize() * 0.02;
            // Juste sous le menton : la tête dépasse de 2 pixels, l'ombre y tombe.
            assert!(
                shadowed(&boxes, at(0.0, 23.0, -2.0) + off, light),
                "menton, {yaw_deg}°"
            );
            // Plus bas sur le torse et sur les côtés : rien (avant : un triangle).
            for (x, y) in [(0.0, 18.0), (-3.0, 21.0), (3.0, 21.0), (0.0, 14.0)] {
                assert!(
                    !shadowed(&boxes, at(x, y, -2.0) + off, light),
                    "torse ({x}, {y}), {yaw_deg}°"
                );
            }
            // Le dessus de l'épaule gauche, lumière au-dessus de l'épaule droite : la tête
            // lui fait de l'ombre ; lumière juste au-dessus d'elle : non.
            let shoulder = at(-6.0, 24.0, 0.0) + Vec3::Y * 0.02;
            assert!(
                shadowed(&boxes, shoulder, at(10.0, 40.0, 0.0)),
                "épaule, {yaw_deg}°"
            );
            assert!(
                !shadowed(&boxes, shoulder, at(-6.0, 60.0, 0.0)),
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
        // En marchant, les jambes ne sont plus parallèles : une boîte chacune.
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
        };
        assert_eq!(shadow_boxes(&walking).len(), 6);
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
        };
        let hand = hand_frame(&pose).unwrap().transform_point3(Vec3::ZERO);
        let tip = torch_tip(&pose).unwrap();
        let shoulder = 22.0 * PX;
        eprintln!("main {hand:?} flamme {tip:?}");
        // Main devant le corps (−z), flamme au-dessus de la main et encore plus devant.
        assert!(hand.z < -0.1 && hand.y < shoulder, "{hand:?}");
        assert!(tip.y > hand.y + 0.2 && tip.z < hand.z, "{tip:?}");
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
