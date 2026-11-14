//! Modèle du joueur vu à la 3e personne (F5) : tête, corps, bras et jambes en pavés, aux
//! proportions classiques des jeux de blocs (32 pixels de haut pour 1,8 bloc), avec un skin
//! original dessiné pixel par pixel. Les bras et les jambes se balancent en marchant, la
//! tête suit le regard, le corps se penche quand on s'accroupit.

use glam::{Mat4, Vec3};
use minelib_render::{EntityVertex, RayBox, RayBoxKind};
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

/// Petite variation par pixel (texture) : ±3 %.
fn grain(seed: usize) -> f32 {
    let h = (seed as u32).wrapping_mul(2654435761) >> 24;
    0.97 + (h as f32 / 255.0) * 0.06
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
) {
    let [x0, y0, z0] = min;
    let [x1, y1, z1] = max;
    let (w, h, d) = ((x1 - x0) as usize, (y1 - y0) as usize, (z1 - z0) as usize);
    // (face, origine en haut à gauche vue de face, axe u, axe v, largeur, hauteur)
    let faces = [
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
    ];
    for (k, (face, o, du, dv, nu, nv)) in faces.into_iter().enumerate() {
        let (o, du, dv) = (Vec3::from(o), Vec3::from(du), Vec3::from(dv));
        let p = |u: f32, v: f32| xf.transform_point3(o + du * u + dv * v);
        // Ombrage selon l'orientation de la face dans le monde.
        let n = xf.transform_vector3(du.cross(dv)).normalize_or_zero();
        let shade = (0.78 + 0.22 * n.y + 0.06 * n.x).clamp(0.45, 1.0);
        for v in 0..nv {
            for u in 0..nu {
                let g = grain(k * 4096 + v * 64 + u + part as usize * 977);
                let c = srgb(skin(part, face, u, v, nu));
                let col = [0, 1, 2].map(|i| c[i] * shade * g * light[i]);
                let (a, b, cc, dd) = (
                    p(u as f32, v as f32),
                    p(u as f32 + 1.0, v as f32),
                    p(u as f32 + 1.0, v as f32 + 1.0),
                    p(u as f32, v as f32 + 1.0),
                );
                for q in [a, b, cc, cc, dd, a] {
                    out.push(EntityVertex {
                        pos: q.to_array(),
                        color: col,
                        uv: [0.0; 2],
                        layer: -1,
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
    /// Autre objet (baguette, arme) : longue pièce de couleur unie (sRGB).
    Stick([u8; 3]),
    /// Torche (couleur de sa flamme, sRGB) : bâton levé devant la main, flamme au bout.
    Torch([u8; 3]),
}

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
    // Jambes : pivot à la hanche (y = 12), balancement opposé.
    for (side, sign) in [(-2.0f32, 1.0f32), (2.0, -1.0)] {
        let xf = base
            * Mat4::from_translation(Vec3::new(side, 12.0, 0.0))
            * Mat4::from_rotation_x(swing * sign);
        out.push((xf, [-2.0, -12.0, -2.0], [2.0, 0.0, 2.0], Part::Leg));
    }
    // Haut du corps : penché vers l'avant quand on s'accroupit (pivot à la hanche).
    let upper = base
        * Mat4::from_translation(Vec3::new(0.0, 12.0 - drop, 0.0))
        * Mat4::from_rotation_x(-lean);
    out.push((upper, [-4.0, 0.0, -2.0], [4.0, 12.0, 2.0], Part::Body));
    // Bras : pivot à l'épaule (y = 22 depuis la hanche : 10), balancement inverse des
    // jambes ; le bras droit (x +6) tient l'objet un peu levé devant lui.
    for (side, sign) in [(-6.0f32, -1.0f32), (6.0, 1.0)] {
        // Bras tendu vers l'avant (rotation positive : la main part vers −z).
        let raise = if side > 0.0 && pose.held.is_some() {
            0.65
        } else {
            0.0
        };
        let swing_k = if raise != 0.0 { 0.3 } else { 1.0 };
        let xf = upper
            * Mat4::from_translation(Vec3::new(side, 10.0, 0.0))
            * Mat4::from_rotation_x(swing * sign * swing_k + lean * 0.6 + raise);
        out.push((xf, [-2.0, -10.0, -2.0], [2.0, 2.0, 2.0], Part::Arm));
    }
    // Tête : pivot au cou, suit le regard (tangage), sans suivre la penche du corps.
    let head = base
        * Mat4::from_translation(Vec3::new(0.0, 24.0 - drop * 1.6, -lean * 4.0))
        * Mat4::from_rotation_x(pose.pitch.clamp(-1.4, 1.4));
    out.push((head, [-4.0, 0.0, -4.0], [4.0, 8.0, 4.0], Part::Head));
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

/// Repère d'une torche tenue : un peu devant le poing, penchée vers l'avant, bâton sur +y.
fn torch_frame(hand: Mat4) -> Mat4 {
    hand * Mat4::from_translation(Vec3::new(0.0, 0.0, -3.0)) * Mat4::from_rotation_x(-0.95)
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
            let t = torch_frame(hand);
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
                    RayBoxKind::Glow(bright),
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
            let part = |a: [u8; 3], b: [u8; 3], dy: f32, layer: u16| {
                let (min, max) = block_box(a, b, dy);
                (
                    hand,
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
            if def.key.starts_with("banner_") {
                // Bannière : deux blocs de haut, comme posée (panneau fin, deux moitiés).
                let (a, b) = boxes[0];
                vec![part(a, b, 0.0, t[1]), part(a, b, -6.0, t[2])]
            } else {
                // Au plus deux morceaux (escalier) : le reste des pavés est pour le corps.
                boxes
                    .into_iter()
                    .take(2)
                    .map(|(a, b)| part(a, b, 0.0, t[1]))
                    .collect()
            }
        }
        Held::Stick(c) => vec![(
            hand,
            [-0.75, -1.0, -10.0],
            [0.75, 0.5, 0.0],
            RayBoxKind::Flat(c),
        )],
    }
}

/// Position (monde) de la flamme d'une torche tenue en main : la lumière part de là.
pub fn torch_tip(pose: &Pose) -> Option<Vec3> {
    match (hand_frame(pose), pose.held) {
        (Some(hand), Some(Held::Torch(_))) => {
            Some(torch_frame(hand).transform_point3(Vec3::new(0.0, 8.0, 0.0)))
        }
        _ => None,
    }
}

/// Pavé de couleur unie (ou d'une texture de bloc par face) : objet tenu, vu en F5.
fn emit_item(
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
        let shade = (0.78 + 0.22 * n.y + 0.06 * n.x).clamp(0.45, 1.0);
        let col = [0, 1, 2].map(|i| color[i] * shade * if lit { light[i] } else { 1.0 });
        for (u, v) in [
            (0.0, 0.0),
            (1.0, 0.0),
            (1.0, 1.0),
            (1.0, 1.0),
            (0.0, 1.0),
            (0.0, 0.0),
        ] {
            out.push(EntityVertex {
                pos: p(u, v).to_array(),
                color: col,
                uv: [
                    rect[0] + (rect[2] - rect[0]) * u,
                    rect[1] + (rect[3] - rect[1]) * v,
                ],
                layer,
            });
        }
    }
}

/// Triangles du modèle du joueur dans cette pose (avec l'objet tenu).
pub fn build(pose: &Pose) -> Vec<EntityVertex> {
    let mut out = Vec::with_capacity(12_000);
    for (xf, min, max, part) in parts(pose) {
        emit_box(&mut out, xf, min, max, part, pose.light);
    }
    for (xf, min, max, kind) in held_parts(pose) {
        emit_item(&mut out, xf, min, max, kind, pose.light);
    }
    out
}

/// Pavés du joueur (et de l'objet tenu) pour les reflets du raytracing.
pub fn ray_boxes(pose: &Pose) -> Vec<RayBox> {
    let body = parts(pose).into_iter().map(|(xf, min, max, part)| {
        let kind = match part {
            Part::Head => RayBoxKind::Head,
            Part::Body => RayBoxKind::Body,
            Part::Arm => RayBoxKind::Arm,
            Part::Leg => RayBoxKind::Leg,
        };
        (xf, min, max, kind)
    });
    body.chain(held_parts(pose))
        .map(|(xf, min, max, kind)| RayBox {
            world_to_local: xf.inverse(),
            min,
            max,
            kind,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

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
