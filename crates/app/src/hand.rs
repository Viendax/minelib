//! Main et objet tenu à la première personne : bras, bloc ou torche en 3D, balancement
//! pendant la marche, geste quand on casse ou pose, objet qui remonte quand on en change.

use glam::{Mat4, Vec3, Vec4};
use minelib_render::{ArmVertex, VmVertex, FLAME, NO_TEXTURE};
use minelib_world::registry::Shape;
use minelib_world::BlockId;

use crate::inventory::Item;

/// Champ de vision fixe de la main (indépendant du zoom).
const HAND_FOV: f32 = 70.0;
/// Bras à la première personne : milieu du poing (par rapport aux yeux : x à droite, y en
/// haut, −z devant ; là où les objets tenus sont placés) et longueur jusqu'à l'épaule, hors
/// de l'écran. Épaisseur : le tiers, comme sur le modèle (4 × 12 × 4 pixels).
const ARM_FIST: Vec3 = Vec3::new(0.474, -0.472, -0.938);
const ARM_LEN: f32 = 0.6;

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
    /// (v201) Lampe torche allumée (son verre brille).
    pub flashlight_on: bool,
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

/// (v216) Foudre de Zeus : l'éclair (le long de +y dans son repère) est tenu en biais, la
/// pointe vers le haut, l'avant et l'intérieur.
pub fn zeus_rotation() -> Mat4 {
    Mat4::from_quat(glam::Quat::from_rotation_arc(
        Vec3::Y,
        Vec3::new(0.05, 0.78, -0.62).normalize(),
    ))
}

/// Projection de la main et de l'objet tenu à la première personne.
pub fn hand_proj(aspect: f32) -> Mat4 {
    Mat4::perspective_infinite_reverse_rh(HAND_FOV.to_radians(), aspect, 0.01)
}

/// (v216) Icône d'inventaire de la Foudre de Zeus : un éclair en zigzag, blanc bleuté,
/// cerné de bleu, centré en (`cx`, `cy`), haut d'environ `size` pixels.
pub fn zeus_icon(cx: f32, cy: f32, size: f32) -> Vec<VmVertex> {
    let m = Mat4::from_translation(Vec3::new(cx, cy, 0.6))
        * Mat4::from_scale(Vec3::new(size, -size, 0.02));
    // Zigzag (y vers le haut, hauteur 1).
    let pts = [
        Vec3::new(0.17, 0.5, 0.0),
        Vec3::new(-0.15, 0.04, 0.0),
        Vec3::new(0.11, 0.0, 0.0),
        Vec3::new(-0.17, -0.5, 0.0),
    ];
    let mut out = Vec::new();
    // Le cerne d'abord (derrière), puis le cœur.
    for (width, z, color) in [
        (0.2, -0.3, [0.07, 0.2, 0.95]),
        (0.085, 0.0, [0.8, 0.9, 1.0]),
    ] {
        for w in pts.windows(2) {
            let d = w[1] - w[0];
            let bar = m
                * Mat4::from_translation(w[0] + Vec3::new(0.0, 0.0, z))
                * Mat4::from_rotation_z(-d.x.atan2(d.y));
            // (Un peu plus long que le segment : les barres se rejoignent aux coudes.)
            let over = width * 0.35;
            push_box(
                &mut out,
                bar,
                Vec3::new(-width * 0.5, -over, 0.0),
                Vec3::new(width * 0.5, d.length() + over, 0.1),
                None,
                color,
                [1.0; 3],
                true,
            );
        }
    }
    out
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

/// Une boîte (coordonnées 0..1 du modèle) transformée vers l'écran ; textures par face
/// (dessus, côtés, dessous) ou couleur unie.
#[allow(clippy::too_many_arguments)]
pub(crate) fn push_box(
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

    /// Geste (arc vers l'avant et le bas) et balancement en huit pendant la marche : repère
    /// de la main par rapport aux yeux.
    fn base(&self) -> Mat4 {
        let s = (std::f32::consts::PI * (1.0 - self.swing)).sin() * (self.swing > 0.0) as u8 as f32;
        let bob =
            Vec3::new(self.bob.sin() * 0.025, -(self.bob.cos().abs()) * 0.03, 0.0) * self.bob_amp;
        Mat4::from_translation(bob + Vec3::new(0.0, -0.35 * self.equip - 0.12 * s, -0.12 * s))
            * Mat4::from_rotation_x(-0.6 * s)
    }

    /// (v216) Repère de la Foudre de Zeus tenue, dans la vue (x à droite, y en haut, −z
    /// devant) : le poing à l'origine, l'éclair le long de +y. Elle suit les gestes de la
    /// main (balancement de la marche, coup).
    pub fn zeus_frame(&self) -> Mat4 {
        self.base() * Mat4::from_translation(ARM_FIST) * zeus_rotation()
    }

    /// Bras droit à la première personne, tel que le modèle du joueur le montre en F5 :
    /// mêmes pixels du skin (4 × 12 × 4), mêmes couleurs, même ombrage selon l'orientation
    /// de chaque face dans le monde. Tendu droit devant (parallèle au regard), sorti du bas
    /// à droite de l'écran, le poing là où l'objet tenu se place. Chaque sommet a sa place
    /// dans le monde (devant les yeux : `to_world`, repère de la caméra → monde) et sa
    /// normale : les lumières dynamiques l'éclairent comme le modèle. `fov_deg` : angle de
    /// vue du monde à cette image (75°, plus en courant, moins au zoom).
    pub fn arm(
        &self,
        aspect: f32,
        fov_deg: f32,
        light: [f32; 3],
        skin: Option<&crate::skin::Skin>,
        to_world: Mat4,
    ) -> Vec<ArmVertex> {
        let proj = Mat4::perspective_infinite_reverse_rh(HAND_FOV.to_radians(), aspect, 0.01);
        // Boîte 0..1 : x vers la droite, y vers le haut, z = 1 à l'épaule, 0 au poing.
        let arm = self.base()
            * Mat4::from_translation(ARM_FIST)
            * Mat4::from_scale(Vec3::new(ARM_LEN / 3.0, ARM_LEN / 3.0, ARM_LEN))
            * Mat4::from_translation(Vec3::new(-0.5, -0.5, 0.0));
        // Le bras est dessiné avec l'angle de vue de la main (70°), le monde avec le sien.
        // Pour l'éclairer, il est placé là où il paraît être dans l'image du monde, à la même
        // distance des yeux : x et y du repère de la caméra multipliés par le rapport des
        // tangentes des demi-angles. Sinon la lumière le touche ailleurs que le monde juste
        // derrière lui (un bord de faisceau décalé d'environ 10 %, 25 % en courant).
        let k = (fov_deg.to_radians() * 0.5).tan() / (HAND_FOV.to_radians() * 0.5).tan();
        let world = to_world * Mat4::from_scale(Vec3::new(k, k, 1.0)) * arm;
        arm_mesh(proj * arm, world, light, skin)
    }

    /// (v215) Bras droit du modèle à la première personne, là où il tient la lampe torche :
    /// `world`, boîte 0..1 du bras → monde (`player_model::right_arm_matrix`), dessiné avec
    /// la vue du monde (`view_proj`), pas celle de la main.
    pub fn arm_world(
        view_proj: Mat4,
        world: Mat4,
        light: [f32; 3],
        skin: Option<&crate::skin::Skin>,
    ) -> Vec<ArmVertex> {
        arm_mesh(view_proj * world, world, light, skin)
    }
}

/// Pavé du bras droit, pixel par pixel du skin : `clip`, boîte 0..1 → écran ; `world`,
/// boîte 0..1 → monde (éclairage et ombrage des faces).
fn arm_mesh(
    clip: Mat4,
    world: Mat4,
    light: [f32; 3],
    skin: Option<&crate::skin::Skin>,
) -> Vec<ArmVertex> {
    {
        let center = world.transform_point3(Vec3::splat(0.5));
        // Faces : 0 +x, 1 −x, 2 +y, 3 −y, 4 +z (épaule), 5 −z (poing) ; coins dans l'ordre
        // (origine, +s, +s +t, +t).
        let faces: [(usize, [Vec3; 4]); 6] = [
            (
                0,
                [
                    Vec3::new(1.0, 0.0, 1.0),
                    Vec3::new(1.0, 0.0, 0.0),
                    Vec3::new(1.0, 1.0, 0.0),
                    Vec3::new(1.0, 1.0, 1.0),
                ],
            ),
            (
                1,
                [
                    Vec3::new(0.0, 0.0, 0.0),
                    Vec3::new(0.0, 0.0, 1.0),
                    Vec3::new(0.0, 1.0, 1.0),
                    Vec3::new(0.0, 1.0, 0.0),
                ],
            ),
            (
                2,
                [
                    Vec3::new(0.0, 1.0, 1.0),
                    Vec3::new(1.0, 1.0, 1.0),
                    Vec3::new(1.0, 1.0, 0.0),
                    Vec3::new(0.0, 1.0, 0.0),
                ],
            ),
            (
                3,
                [
                    Vec3::new(0.0, 0.0, 0.0),
                    Vec3::new(1.0, 0.0, 0.0),
                    Vec3::new(1.0, 0.0, 1.0),
                    Vec3::new(0.0, 0.0, 1.0),
                ],
            ),
            (
                4,
                [
                    Vec3::new(0.0, 0.0, 1.0),
                    Vec3::new(1.0, 0.0, 1.0),
                    Vec3::new(1.0, 1.0, 1.0),
                    Vec3::new(0.0, 1.0, 1.0),
                ],
            ),
            (
                5,
                [
                    Vec3::new(1.0, 0.0, 0.0),
                    Vec3::new(0.0, 0.0, 0.0),
                    Vec3::new(0.0, 1.0, 0.0),
                    Vec3::new(1.0, 1.0, 0.0),
                ],
            ),
        ];
        let cells = |a: Vec3, b: Vec3| if (b - a).z.abs() > 0.5 { 12 } else { 4 };
        let mut out = Vec::with_capacity(224 * 6);
        for (face, q) in faces {
            // Normale dans le monde, vers l'extérieur de la boîte.
            let w = q.map(|p| world.transform_point3(p));
            let mut n = (w[1] - w[0]).cross(w[3] - w[0]).normalize_or_zero();
            if n.dot((w[0] + w[2]) * 0.5 - center) < 0.0 {
                n = -n;
            }
            let shade = crate::player_model::face_shade(n);
            let (ns, nt) = (cells(q[0], q[1]), cells(q[0], q[3]));
            let at = |s: f32, t: f32| q[0] + (q[1] - q[0]) * s + (q[3] - q[0]) * t;
            for j in 0..nt {
                for i in 0..ns {
                    let (s0, s1) = (i as f32 / ns as f32, (i + 1) as f32 / ns as f32);
                    let (t0, t1) = (j as f32 / nt as f32, (j + 1) as f32 / nt as f32);
                    let c = at((s0 + s1) * 0.5, (t0 + t1) * 0.5);
                    let px = |x: f32, n: usize| ((x * n as f32).floor() as usize).min(n - 1);
                    // Pixel du skin : la longueur du bras de l'épaule (v = 0) au poing.
                    let along = px(1.0 - c.z, 12);
                    let (k, u, v) = match face {
                        2 => (0, px(1.0 - c.x, 4), along),
                        3 => (1, px(c.x, 4), along),
                        0 => (2, px(c.y, 4), along),
                        1 => (3, px(1.0 - c.y, 4), along),
                        4 => (4, px(c.x, 4), px(c.y, 4)),
                        _ => (5, px(c.x, 4), px(1.0 - c.y, 4)),
                    };
                    let albedo =
                        crate::player_model::right_arm_pixel(skin, k, u, v).map(|c| c * shade);
                    let quad = [at(s0, t0), at(s1, t0), at(s1, t1), at(s0, t1)];
                    for k in [0usize, 1, 2, 2, 3, 0] {
                        out.push(ArmVertex {
                            clip: (clip * quad[k].extend(1.0)).to_array(),
                            world: world.transform_point3(quad[k]).to_array(),
                            normal: n.to_array(),
                            albedo,
                            light,
                        });
                    }
                }
            }
        }
        out
    }
}

impl Hand {
    /// Triangles de l'objet tenu, pour un écran de rapport `aspect` (le bras : `arm`).
    pub fn build(&self, item: Option<Item>, aspect: f32, light: [f32; 3]) -> Vec<VmVertex> {
        let proj = Mat4::perspective_infinite_reverse_rh(HAND_FOV.to_radians(), aspect, 0.01);
        let mut out = Vec::new();
        let base = self.base();

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
            // (v201) Lampe torche : pointée devant, un peu vers le viseur, le verre au loin.
            Item::Flashlight => {
                let m = base
                    * Mat4::from_translation(Vec3::new(0.4, -0.42, -0.86))
                    * Mat4::from_rotation_y(0.16)
                    * Mat4::from_rotation_x(-std::f32::consts::FRAC_PI_2 + 0.06)
                    * Mat4::from_scale(Vec3::splat(0.44))
                    * Mat4::from_translation(Vec3::new(-0.5, -0.3, -0.5));
                crate::flashlight::model(&mut out, proj * m, light, self.flashlight_on);
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
            // (v216) Foudre de Zeus : rien de solide, l'éclair est dessiné à part (voir
            // `storm`).
            Item::Zeus => {}
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
            Some(Item::Block(BlockId::STONE)),
            Some(Item::Torch(Some(2))),
        ] {
            let v = h.build(item, 16.0 / 9.0, [1.0; 3]);
            assert!(!v.is_empty());
            for p in &v {
                let [x, y, _z, w] = p.clip;
                assert!(w > 0.0, "derrière la caméra {item:?} {:?}", p.clip);
                // Dans la moitié droite / basse de l'écran (avec un peu de marge).
                assert!(x / w > -0.3 && y / w < 0.5, "{:?}", p.clip);
            }
        }
    }

    /// Les lumières touchent le bras là où on le voit : chaque sommet, placé dans le monde
    /// pour l'éclairage, tombe au même endroit de l'écran par la vue du monde que par celle
    /// de la main qui le dessine, quel que soit l'angle de vue du monde (zoom, normal, course).
    #[test]
    fn the_arm_is_lit_where_it_is_seen() {
        let h = Hand::default();
        let aspect = 16.0 / 9.0;
        let eye = Vec3::new(-92.0, 131.6, -62.2);
        let to_world =
            Mat4::from_translation(eye) * Mat4::from_rotation_y(2.9) * Mat4::from_rotation_x(-0.45);
        let view = to_world.inverse();
        for fov in [18.0, crate::session::FOV, crate::session::FOV * 1.1] {
            let world_vp =
                Mat4::perspective_infinite_reverse_rh(f32::to_radians(fov), aspect, 0.05) * view;
            for p in h.arm(aspect, fov, [1.0; 3], None, to_world) {
                let [x, y, _, w] = p.clip;
                let c = world_vp * Vec3::from(p.world).extend(1.0);
                assert!(
                    (c.x / c.w - x / w).abs() < 1e-3 && (c.y / c.w - y / w).abs() < 1e-3,
                    "angle {fov} : main ({}, {}), monde ({}, {})",
                    x / w,
                    y / w,
                    c.x / c.w,
                    c.y / c.w
                );
                // Et à la même distance des yeux, devant eux.
                assert!(c.w > 0.2 && (c.w - w).abs() < 1e-3, "{} {}", c.w, w);
            }
        }
    }

    #[test]
    fn arm_points_straight_ahead_like_the_model_arm() {
        let h = Hand::default();
        // Caméra en (10, 70, 5) tournée de 0,7 rad : le bras suit, dans le monde.
        let eye = Vec3::new(10.0, 70.0, 5.0);
        let to_world = Mat4::from_translation(eye) * Mat4::from_rotation_y(0.7);
        let v = h.arm(16.0 / 9.0, crate::session::FOV, [0.5; 3], None, to_world);
        // 4 × 12 pixels sur quatre faces, 4 × 4 aux deux bouts, deux triangles chacun.
        assert_eq!(v.len(), (4 * 12 * 4 + 4 * 4 * 2) * 6);
        let fwd = to_world.transform_vector3(-Vec3::Z);
        let mut on_screen = 0;
        for p in &v {
            let [x, y, _z, w] = p.clip;
            assert!(w > 0.0, "derrière la caméra {:?}", p.clip);
            if (x / w).abs() <= 1.0 && (y / w).abs() <= 1.0 {
                on_screen += 1;
                // Visible en bas à droite seulement.
                assert!(x / w > 0.0 && y / w < 0.0, "{:?}", p.clip);
            }
            // Devant les yeux, à portée de main ; normale unitaire.
            let at = Vec3::from(p.world) - eye;
            assert!(at.length() < 1.3 && at.dot(fwd) > 0.2, "{at:?}");
            assert!((Vec3::from(p.normal).length() - 1.0).abs() < 1e-4);
            // Couleurs du modèle (ombrage de la face compris), lumière du lieu.
            assert!(p.albedo.iter().all(|c| (0.0..=1.0).contains(c)));
            assert_eq!(p.light, [0.5; 3]);
        }
        assert!(on_screen > 0, "bras hors de l'écran");
        // Tendu droit devant : les arêtes du bras sont parallèles au regard.
        let side: Vec<Vec3> = v
            .iter()
            .filter(|p| {
                (Vec3::from(p.normal).dot(to_world.transform_vector3(Vec3::X)) - 1.0).abs() < 1e-4
            })
            .map(|p| Vec3::from(p.world))
            .collect();
        assert!(!side.is_empty());
        for p in &side {
            let d = *p - side[0];
            let across = d - fwd * d.dot(fwd);
            // Face de droite : tous ses points sur un plan qui contient le regard.
            assert!(
                across.dot(to_world.transform_vector3(Vec3::X)).abs() < 1e-4,
                "{d:?}"
            );
        }
        // Le poing est là où les objets tenus se placent.
        let fist = to_world.transform_point3(ARM_FIST);
        let closest = v
            .iter()
            .map(|p| (Vec3::from(p.world) - fist).length())
            .fold(f32::MAX, f32::min);
        assert!(closest < 0.2, "{closest}");
    }
}
