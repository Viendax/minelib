//! Main et objet tenu à la première personne : bras, bloc ou torche en 3D, balancement
//! pendant la marche, geste quand on casse ou pose, objet qui remonte quand on en change.

use glam::{Mat4, Vec3, Vec4};
use minelib_render::{VmVertex, FLAME, NO_TEXTURE};
use minelib_world::registry::Shape;
use minelib_world::BlockId;

use crate::inventory::Item;

/// Champ de vision fixe de la main (indépendant du zoom).
const HAND_FOV: f32 = 70.0;

#[derive(Default)]
pub struct Hand {
    /// Geste (1 → 0).
    swing: f32,
    /// Phase du balancement de marche.
    bob: f32,
    /// Amplitude du balancement (suit la vitesse).
    bob_amp: f32,
    /// Objet qui remonte après un changement (1 = caché en bas).
    equip: f32,
    last: Option<Item>,
}

// Rotation de chaque objet tenu par rapport au regard (x à droite, y en haut, −z
// devant). La même sert à la 1re personne et sur le modèle du joueur (F5, reflets,
// les autres joueurs) : changer l'une change l'autre.

/// Bloc tenu.
pub fn block_rotation() -> Mat4 {
    Mat4::from_rotation_y(0.8) * Mat4::from_rotation_x(0.12)
}

/// Torche tenue.
pub fn torch_rotation() -> Mat4 {
    Mat4::from_rotation_y(0.4) * Mat4::from_rotation_x(0.2)
}

/// Baguette de WorldEdit (tenue droite).
pub fn wand_rotation() -> Mat4 {
    Mat4::from_rotation_y(-0.45) * Mat4::from_rotation_z(0.1)
}

/// Objet de deux blocs de haut (bannière, porte) : textures du haut et du bas. Dessiné
/// ainsi partout (inventaire, main, modèle du joueur).
pub fn tall_layers(block: BlockId) -> Option<(u32, u32)> {
    let def = block.def();
    if def.key.starts_with("banner_") {
        return Some((def.tex[1] as u32, def.tex[2] as u32));
    }
    if !def.key.starts_with("door_") {
        return None;
    }
    let other = |k: String| minelib_world::registry::registry().by_key(&k);
    let own = def.tex[1] as u32;
    if def.key.contains(":upper:") {
        other(def.key.replacen(":upper:", ":lower:", 1)).map(|lo| (own, lo.def().tex[1] as u32))
    } else {
        other(def.key.replacen(":lower:", ":upper:", 1)).map(|up| (up.def().tex[1] as u32, own))
    }
}

/// Lanterne tenue ou en icône : son modèle 3D posé (corps et chapeau, sans chaîne), taillé
/// dans sa texture. `m` : transformation d'un bloc 0..1. Faux si ce n'est pas une lanterne.
fn push_lantern(out: &mut Vec<VmVertex>, m: Mat4, block: BlockId, light: [f32; 3]) -> bool {
    let def = block.def();
    if !(def.key.starts_with("lantern") || def.key.starts_with("soul_lantern")) {
        return false;
    }
    // Les coordonnées de texture sont tirées de la position : chaque pavé est placé sur
    // son morceau de texture, puis ramené au milieu du bloc (grossi : l'objet est petit).
    let s = 1.0 / 16.0;
    let m = m
        * Mat4::from_translation(Vec3::new(0.5, 0.1, 0.5))
        * Mat4::from_scale(Vec3::splat(1.6))
        * Mat4::from_translation(Vec3::new(-3.0 * s, -7.0 * s, -3.0 * s));
    let layer = [def.tex[1] as u32; 3];
    for (a, b) in [
        ([0.0, 7.0, 0.0], [6.0, 14.0, 6.0]),
        ([1.0, 14.0, 1.0], [5.0, 16.0, 5.0]),
    ] {
        push_box(
            out,
            m,
            Vec3::from(a) * s,
            Vec3::from(b) * s,
            Some(layer),
            [1.0; 3],
            light,
            true,
        );
    }
    true
}

fn lin(c: [u8; 3]) -> [f32; 3] {
    c.map(|v| (v as f32 / 255.0).powf(2.2))
}

/// Une boîte (coordonnées 0..1 du modèle) transformée vers l'écran ; textures par face
/// (dessus, côtés, dessous) ou couleur unie.
#[allow(clippy::too_many_arguments)]
fn push_box(
    out: &mut Vec<VmVertex>,
    mvp: Mat4,
    min: Vec3,
    max: Vec3,
    layers: Option<[u32; 3]>,
    color: [f32; 3],
    light: [f32; 3],
    emissive: bool,
) {
    // (normale, coins) ; uv = position sur la face, comme les textures de bloc.
    let faces: [(usize, [Vec3; 4]); 6] = [
        (
            0,
            [
                Vec3::new(max.x, min.y, max.z),
                Vec3::new(max.x, min.y, min.z),
                Vec3::new(max.x, max.y, min.z),
                Vec3::new(max.x, max.y, max.z),
            ],
        ),
        (
            1,
            [
                Vec3::new(min.x, min.y, min.z),
                Vec3::new(min.x, min.y, max.z),
                Vec3::new(min.x, max.y, max.z),
                Vec3::new(min.x, max.y, min.z),
            ],
        ),
        (
            2,
            [
                Vec3::new(min.x, max.y, max.z),
                Vec3::new(max.x, max.y, max.z),
                Vec3::new(max.x, max.y, min.z),
                Vec3::new(min.x, max.y, min.z),
            ],
        ),
        (
            3,
            [
                Vec3::new(min.x, min.y, min.z),
                Vec3::new(max.x, min.y, min.z),
                Vec3::new(max.x, min.y, max.z),
                Vec3::new(min.x, min.y, max.z),
            ],
        ),
        (
            4,
            [
                Vec3::new(min.x, min.y, max.z),
                Vec3::new(max.x, min.y, max.z),
                Vec3::new(max.x, max.y, max.z),
                Vec3::new(min.x, max.y, max.z),
            ],
        ),
        (
            5,
            [
                Vec3::new(max.x, min.y, min.z),
                Vec3::new(min.x, min.y, min.z),
                Vec3::new(min.x, max.y, min.z),
                Vec3::new(max.x, max.y, min.z),
            ],
        ),
    ];
    for (face, quad) in faces {
        let shade = if emissive {
            1.0
        } else {
            match face {
                2 => 1.0,
                3 => 0.5,
                0 | 1 => 0.62,
                _ => 0.8,
            }
        };
        let layer = layers.map_or(NO_TEXTURE, |l| match face {
            2 => l[0],
            3 => l[2],
            _ => l[1],
        });
        let k = if emissive { [1.0; 3] } else { light };
        let c = [
            color[0] * shade * k[0],
            color[1] * shade * k[1],
            color[2] * shade * k[2],
            1.0,
        ];
        let layer = if emissive && layers.is_none() {
            FLAME
        } else {
            layer
        };
        let uv = |p: Vec3| -> [f32; 2] {
            if layer == FLAME {
                return [p.x, p.y];
            }
            match face {
                0 | 1 => [p.z, 1.0 - p.y],
                4 | 5 => [p.x, 1.0 - p.y],
                _ => [p.x, p.z],
            }
        };
        for i in [0usize, 1, 2, 2, 3, 0] {
            let p = quad[i];
            let clip: Vec4 = mvp * p.extend(1.0);
            out.push(VmVertex {
                clip: clip.to_array(),
                uv: uv(p),
                layer,
                _pad: 0,
                color: c,
            });
        }
    }
}

/// Bras droit avec le skin du joueur : la boîte 0..1 (long selon z, l'épaule en z = 1,
/// la main en z = 0, le dessus du bras vers +y) découpée en pixels du skin, colorés comme
/// le bras du modèle (`skin::Skin::color`, partie 4, 4 × 12 × 4 pixels).
fn push_skin_arm(
    out: &mut Vec<VmVertex>,
    mvp: Mat4,
    skin: &crate::skin::Skin,
    fallback: [f32; 3],
    light: [f32; 3],
) {
    let (mn, mx) = (Vec3::ZERO, Vec3::ONE);
    // Mêmes faces (et sens) que `push_box` : 0 +x, 1 −x, 2 +y, 3 −y, 4 +z, 5 −z.
    let faces: [(usize, [Vec3; 4]); 6] = [
        (
            0,
            [
                Vec3::new(mx.x, mn.y, mx.z),
                Vec3::new(mx.x, mn.y, mn.z),
                Vec3::new(mx.x, mx.y, mn.z),
                Vec3::new(mx.x, mx.y, mx.z),
            ],
        ),
        (
            1,
            [
                Vec3::new(mn.x, mn.y, mn.z),
                Vec3::new(mn.x, mn.y, mx.z),
                Vec3::new(mn.x, mx.y, mx.z),
                Vec3::new(mn.x, mx.y, mn.z),
            ],
        ),
        (
            2,
            [
                Vec3::new(mn.x, mx.y, mx.z),
                Vec3::new(mx.x, mx.y, mx.z),
                Vec3::new(mx.x, mx.y, mn.z),
                Vec3::new(mn.x, mx.y, mn.z),
            ],
        ),
        (
            3,
            [
                Vec3::new(mn.x, mn.y, mn.z),
                Vec3::new(mx.x, mn.y, mn.z),
                Vec3::new(mx.x, mn.y, mx.z),
                Vec3::new(mn.x, mn.y, mx.z),
            ],
        ),
        (
            4,
            [
                Vec3::new(mn.x, mn.y, mx.z),
                Vec3::new(mx.x, mn.y, mx.z),
                Vec3::new(mx.x, mx.y, mx.z),
                Vec3::new(mn.x, mx.y, mx.z),
            ],
        ),
        (
            5,
            [
                Vec3::new(mx.x, mn.y, mn.z),
                Vec3::new(mn.x, mn.y, mn.z),
                Vec3::new(mn.x, mx.y, mn.z),
                Vec3::new(mx.x, mx.y, mn.z),
            ],
        ),
    ];
    let cells = |a: Vec3, b: Vec3| if (b - a).z.abs() > 0.5 { 12 } else { 4 };
    for (face, q) in faces {
        let shade = match face {
            2 => 1.0,
            3 => 0.5,
            0 | 1 => 0.62,
            _ => 0.8,
        };
        let (ns, nt) = (cells(q[0], q[1]), cells(q[0], q[3]));
        let at = |s: f32, t: f32| q[0] + (q[1] - q[0]) * s + (q[3] - q[0]) * t;
        for j in 0..nt {
            for i in 0..ns {
                let (s0, s1) = (i as f32 / ns as f32, (i + 1) as f32 / ns as f32);
                let (t0, t1) = (j as f32 / nt as f32, (j + 1) as f32 / nt as f32);
                let c = at((s0 + s1) * 0.5, (t0 + t1) * 0.5);
                let px = |x: f32, n: f32| ((x * n).floor() as u32).min(n as u32 - 1);
                let along = px(1.0 - c.z, 12.0);
                let (sf, u, v) = match face {
                    2 => (0, px(1.0 - c.x, 4.0), along),
                    3 => (1, px(c.x, 4.0), along),
                    0 => (2, px(c.y, 4.0), along),
                    1 => (3, px(1.0 - c.y, 4.0), along),
                    4 => (4, px(c.x, 4.0), px(c.y, 4.0)),
                    _ => (5, px(c.x, 4.0), px(c.y, 4.0)),
                };
                let rgb = skin.color(4, sf, u, v, [4, 12, 4]).map_or(fallback, |h| {
                    lin([(h >> 16) as u8, (h >> 8) as u8, h as u8])
                });
                let col = [
                    rgb[0] * shade * light[0],
                    rgb[1] * shade * light[1],
                    rgb[2] * shade * light[2],
                    1.0,
                ];
                let quad = [at(s0, t0), at(s1, t0), at(s1, t1), at(s0, t1)];
                for k in [0usize, 1, 2, 2, 3, 0] {
                    out.push(VmVertex {
                        clip: (mvp * quad[k].extend(1.0)).to_array(),
                        uv: [0.0, 0.0],
                        layer: NO_TEXTURE,
                        _pad: 0,
                        color: col,
                    });
                }
            }
        }
    }
}

/// Deux plans en croix (plantes tenues en main).
fn push_cross(out: &mut Vec<VmVertex>, mvp: Mat4, layer: u32, light: [f32; 3]) {
    for (a, b) in [((0.0, 0.0), (1.0, 1.0)), ((0.0, 1.0), (1.0, 0.0))] {
        let q = [
            Vec3::new(a.0, 0.0, a.1),
            Vec3::new(b.0, 0.0, b.1),
            Vec3::new(b.0, 1.0, b.1),
            Vec3::new(a.0, 1.0, a.1),
        ];
        for i in [0usize, 1, 2, 2, 3, 0] {
            let p = q[i];
            let u = if i == 1 || i == 2 { 1.0 } else { 0.0 };
            let clip = mvp * p.extend(1.0);
            out.push(VmVertex {
                clip: clip.to_array(),
                uv: [u, 1.0 - p.y],
                layer,
                _pad: 0,
                color: [light[0], light[1], light[2], 1.0],
            });
        }
    }
}

/// Modèle de torche (bâton texturé et flamme animée comme posée), coordonnées 0..1 du bloc
/// transformées par `mvp`.
pub fn torch_model(out: &mut Vec<VmVertex>, mvp: Mat4, chem: Option<usize>, light: [f32; 3]) {
    let t = BlockId::TORCH.def().tex;
    let layers = [t[0] as u32, t[1] as u32, t[2] as u32];
    push_box(
        out,
        mvp,
        Vec3::new(0.4375, 0.0, 0.4375),
        Vec3::new(0.5625, 0.5, 0.5625),
        Some(layers),
        [1.0; 3],
        light,
        false,
    );
    let rgb = chem
        .and_then(|i| minelib_world::flame::CHEMICALS.get(i))
        .map_or(minelib_world::flame::WOOD_FLAME, |c| c.rgb);
    // Même teinte que la flamme des torches posées (canal le plus fort à 1).
    let top = rgb.iter().copied().max().unwrap_or(1).max(1) as f32;
    let flame = rgb.map(|v| v as f32 / top);
    push_box(
        out,
        mvp,
        Vec3::new(0.40625, 0.5, 0.40625),
        Vec3::new(0.59375, 0.8125, 0.59375),
        None,
        flame,
        light,
        true,
    );
}

/// Torche en 3D pour une case d'inventaire : vue de trois quarts, centrée sur (`cx`, `cy`)
/// (pixels), hauteur d'environ `size` pixels.
pub fn torch_icon(cx: f32, cy: f32, size: f32, chem: Option<usize>) -> Vec<VmVertex> {
    let k = size / 0.85;
    let m = Mat4::from_translation(Vec3::new(cx, cy, 0.6))
        * Mat4::from_scale(Vec3::new(k, -k, 0.02))
        * Mat4::from_rotation_x(0.45)
        * Mat4::from_rotation_y(-std::f32::consts::FRAC_PI_4)
        * Mat4::from_translation(Vec3::new(-0.5, -0.42, -0.5));
    let mut out = Vec::new();
    torch_model(&mut out, m, chem, [1.0; 3]);
    out
}

/// Cube blanc en 3D (dessus clair, côtés ombrés), centré en (`cx`, `cy`), large de `size`
/// pixels : logo du mode construction.
pub fn white_cube_icon(cx: f32, cy: f32, size: f32) -> Vec<VmVertex> {
    let k = size / 1.62;
    let m = Mat4::from_translation(Vec3::new(cx, cy, 0.5))
        * Mat4::from_scale(Vec3::new(k, -k, 0.02))
        * Mat4::from_rotation_x(std::f32::consts::FRAC_PI_6)
        * Mat4::from_rotation_y(-std::f32::consts::FRAC_PI_4)
        * Mat4::from_translation(Vec3::splat(-0.5));
    let mut out = Vec::new();
    push_box(
        &mut out,
        m,
        Vec3::ZERO,
        Vec3::ONE,
        None,
        [1.0; 3],
        [1.0; 3],
        false,
    );
    out
}

/// Icône 3D d'un bloc pour l'interface (vue isométrique, en pixels d'écran), centrée en
/// (`cx`, `cy`) et large de `size` pixels. Dessinée par la carte avec les vraies textures
/// des blocs : nette à toutes les tailles d'écran (plus d'image pré-calculée agrandie).
/// Plantes en croix : la texture à plat.
pub fn block_icon(cx: f32, cy: f32, size: f32, block: BlockId) -> Vec<VmVertex> {
    let def = block.def();
    let t = def.tex;
    let layers = [t[0] as u32, t[1] as u32, t[2] as u32];
    let mut out = Vec::new();
    let tall = tall_layers(block);
    if let Some((top, bottom)) = tall {
        // Deux blocs de haut, à plat, moitié du haut sur celle du bas (raccordées comme
        // une fois posée).
        let (w, h) = (size / 4.0, size / 2.0);
        for (layer, y0, y1) in [(top, -h, 0.0), (bottom, 0.0, h)] {
            let corners = [
                (-w, y1, 0.0, 1.0),
                (w, y1, 1.0, 1.0),
                (w, y0, 1.0, 0.0),
                (-w, y0, 0.0, 0.0),
            ];
            for i in [0usize, 1, 2, 2, 3, 0] {
                let (x, y, u, v) = corners[i];
                out.push(VmVertex {
                    clip: [cx + x, cy + y, 0.5, 1.0],
                    uv: [u, v],
                    layer,
                    _pad: 0,
                    color: [1.0, 1.0, 1.0, 1.0],
                });
            }
        }
        return out;
    }
    if matches!(def.shape, Shape::Cross | Shape::Air | Shape::Invisible) {
        // Image plate : un carré texturé face à l'écran.
        let h = size / 2.0;
        let corners = [
            (-h, h, 0.0, 1.0),
            (h, h, 1.0, 1.0),
            (h, -h, 1.0, 0.0),
            (-h, -h, 0.0, 0.0),
        ];
        for i in [0usize, 1, 2, 2, 3, 0] {
            let (x, y, u, v) = corners[i];
            out.push(VmVertex {
                clip: [cx + x, cy + y, 0.5, 1.0],
                uv: [u, v],
                layer: layers[1],
                _pad: 0,
                color: [1.0, 1.0, 1.0, 1.0],
            });
        }
        return out;
    }
    // Cube de côté 1 vu à 45° et 30° : environ 1,6 × k de haut.
    let k = size / 1.62;
    let m = Mat4::from_translation(Vec3::new(cx, cy, 0.5))
        * Mat4::from_scale(Vec3::new(k, -k, 0.02))
        * Mat4::from_rotation_x(std::f32::consts::FRAC_PI_6)
        * Mat4::from_rotation_y(-std::f32::consts::FRAC_PI_4)
        * Mat4::from_translation(Vec3::splat(-0.5));
    if push_lantern(&mut out, m, block, [1.0; 3]) {
        return out;
    }
    let boxes = if def.full {
        vec![([0u8, 0, 0], [16u8, 16, 16])]
    } else {
        def.boxes()
    };
    let v = |p: [u8; 3]| Vec3::new(p[0] as f32, p[1] as f32, p[2] as f32) / 16.0;
    for (a, c) in boxes {
        push_box(
            &mut out,
            m,
            v(a),
            v(c),
            Some(layers),
            [1.0; 3],
            [1.0; 3],
            def.light.is_some(),
        );
    }
    out
}

impl Hand {
    /// `speed` : vitesse horizontale (m/s) au sol, 0 en l'air ; `swung` : geste commencé.
    pub fn update(&mut self, dt: f32, speed: f32, swung: bool, item: Option<Item>) {
        if swung {
            self.swing = 1.0;
        }
        self.swing = (self.swing - dt / 0.28).max(0.0);
        let target = (speed / 4.4).min(1.5);
        self.bob_amp += (target - self.bob_amp) * (dt * 8.0).min(1.0);
        self.bob += dt * speed * 1.9;
        if item != self.last {
            self.last = item;
            self.equip = 1.0;
        }
        self.equip = (self.equip - dt / 0.2).max(0.0);
    }

    /// Triangles de la main et de l'objet, pour un écran de rapport `aspect`.
    pub fn build(
        &self,
        item: Option<Item>,
        aspect: f32,
        light: [f32; 3],
        show_arm: bool,
        skin_tex: Option<&crate::skin::Skin>,
    ) -> Vec<VmVertex> {
        let proj = Mat4::perspective_infinite_reverse_rh(HAND_FOV.to_radians(), aspect, 0.01);
        let mut out = Vec::new();
        // Geste : arc vers l'avant et le bas ; balancement en huit pendant la marche.
        let s = (std::f32::consts::PI * (1.0 - self.swing)).sin() * (self.swing > 0.0) as u8 as f32;
        let bob =
            Vec3::new(self.bob.sin() * 0.025, -(self.bob.cos().abs()) * 0.03, 0.0) * self.bob_amp;
        let base =
            Mat4::from_translation(bob + Vec3::new(0.0, -0.35 * self.equip - 0.12 * s, -0.12 * s))
                * Mat4::from_rotation_x(-0.6 * s);

        if show_arm {
            // Bras : sort du bas à droite de l'écran, tendu vers le centre.
            let skin = lin([214, 160, 128]);
            // Épaule en bas à droite, bras tendu vers l'avant, légèrement relevé et rentré.
            let arm = base
                * Mat4::from_translation(Vec3::new(0.62, -0.74, -0.22))
                * Mat4::from_rotation_y(0.2)
                * Mat4::from_rotation_x(0.35)
                * Mat4::from_scale(Vec3::new(0.16, 0.16, 0.78))
                * Mat4::from_translation(Vec3::new(-0.5, -0.5, -1.0));
            match skin_tex {
                Some(t) => push_skin_arm(&mut out, proj * arm, t, skin, light),
                None => push_box(
                    &mut out,
                    proj * arm,
                    Vec3::ZERO,
                    Vec3::ONE,
                    None,
                    skin,
                    light,
                    false,
                ),
            }
        }

        let Some(item) = item else { return out };
        match item {
            Item::Torch(chem) => {
                let m = base
                    * Mat4::from_translation(Vec3::new(0.47, -0.5, -0.98))
                    * torch_rotation()
                    * Mat4::from_scale(Vec3::splat(0.5))
                    * Mat4::from_translation(Vec3::new(-0.5, -0.1, -0.5));
                torch_model(&mut out, proj * m, chem, light);
            }
            Item::Wand => {
                // Tenue droite, logo en haut, légèrement tournée vers le centre.
                let m = base
                    * Mat4::from_translation(Vec3::new(0.36, -0.58, -0.92))
                    * wand_rotation()
                    * Mat4::from_scale(Vec3::splat(0.34));
                crate::wand::model(&mut out, proj * m, light);
            }
            Item::Screen(_) => {
                // Tenu droit comme la baguette.
                let m = base
                    * Mat4::from_translation(Vec3::new(0.36, -0.58, -0.92))
                    * wand_rotation()
                    * Mat4::from_scale(Vec3::splat(0.34));
                crate::screen::model(&mut out, proj * m, light);
            }
            // Tête (simple ou à skin) : tenue comme un bloc, avec son skin, le visage vers soi.
            Item::Head(_) | Item::Block(_) if item.head_skin().is_some() => {
                let skin = item.head_skin().unwrap_or(crate::heads::HeadSkin::Default);
                let m = base
                    * Mat4::from_translation(Vec3::new(0.5, -0.36, -1.02))
                    * block_rotation()
                    * Mat4::from_scale(Vec3::splat(0.26))
                    * Mat4::from_rotation_y(std::f32::consts::PI)
                    * Mat4::from_scale(Vec3::splat(1.0 / 8.0))
                    * Mat4::from_translation(Vec3::new(0.0, -4.0, 0.0));
                let s = crate::heads::loaded(&skin);
                out.extend(crate::heads::head_vm(s.as_deref(), proj * m, light));
            }
            Item::Head(_) => {}
            Item::Block(b) => {
                let def = b.def();
                let t = def.tex;
                let layers = [t[0] as u32, t[1] as u32, t[2] as u32];
                let m = base
                    * Mat4::from_translation(Vec3::new(0.5, -0.36, -1.02))
                    * block_rotation()
                    * Mat4::from_scale(Vec3::splat(0.26))
                    * Mat4::from_translation(Vec3::splat(-0.5));
                match def.shape {
                    Shape::Cross => push_cross(&mut out, proj * m, layers[1], light),
                    Shape::Air | Shape::Invisible => {}
                    _ if push_lantern(&mut out, proj * m, b, light) => {}
                    _ => {
                        let boxes = if def.full {
                            vec![([0, 0, 0], [16, 16, 16])]
                        } else {
                            def.boxes()
                        };
                        // Bannière, porte : deux blocs de haut, comme posées.
                        let halves: Vec<(f32, [u32; 3])> = match tall_layers(b) {
                            Some((top, bottom)) => vec![(0.5, [top; 3]), (-0.5, [bottom; 3])],
                            None => vec![(0.0, layers)],
                        };
                        for &(dy, half) in &halves {
                            // Moitié déplacée par la transformation (pas par ses
                            // coordonnées, dont la texture est tirée).
                            let m = m * Mat4::from_translation(Vec3::new(0.0, dy, 0.0));
                            for &(a, c) in &boxes {
                                let v = |p: [u8; 3]| {
                                    Vec3::new(p[0] as f32, p[1] as f32, p[2] as f32) / 16.0
                                };
                                push_box(
                                    &mut out,
                                    proj * m,
                                    v(a),
                                    v(c),
                                    Some(half),
                                    [1.0; 3],
                                    light,
                                    false,
                                );
                            }
                        }
                    }
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hand_and_items_stay_in_front_of_the_camera() {
        let mut h = Hand::default();
        h.update(0.5, 0.0, false, Some(Item::Block(BlockId::STONE)));
        for item in [
            None,
            Some(Item::Block(BlockId::STONE)),
            Some(Item::Torch(Some(2))),
        ] {
            let v = h.build(item, 16.0 / 9.0, [1.0; 3], true, None);
            assert!(!v.is_empty());
            for p in &v {
                let [x, y, _z, w] = p.clip;
                assert!(w > 0.0, "derrière la caméra {item:?} {:?}", p.clip);
                // Dans la moitié droite / basse de l'écran (avec un peu de marge).
                assert!(x / w > -0.3 && y / w < 0.5, "{:?}", p.clip);
            }
        }
    }
}
