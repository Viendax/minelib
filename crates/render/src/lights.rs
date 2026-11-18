//! Lumières dynamiques placées par le joueur : lumières ponctuelles et projecteurs, avec
//! leur couleur, leur portée, leur cône, des ombres en temps réel, de la lumière
//! volumétrique (rayons dans la poussière) et des motifs projetés (gobos).
//!
//! Inspiré d'IRLights (mod Minecraft de qualet, licence MIT) : même modèle de lumière
//! (fenêtre radiale de Frostbite, cône intérieur / extérieur, diffusion de
//! Henyey-Greenstein, gobo projeté dans le cône), réécrit pour le moteur de Minelib :
//!
//! - **éclairage** : tout se fait dans les shaders du monde et des entités (`dynlights.wgsl`),
//!   en avant (pas de G-buffer) ; l'écran est découpé en 32 × 18 tuiles et chaque tuile ne
//!   parcourt que les lumières qui peuvent la toucher ;
//! - **ombres** : le monde est fait de blocs, déjà rangés dans la grille du raytracing
//!   (128 × 64 × 128 autour de la caméra). La carte d'ombre de chaque lumière (une case
//!   d'un atlas pour un projecteur, six pour une lumière ponctuelle) est **calculée en
//!   lançant un rayon par texel dans cette grille** (`shadow_bake.wgsl`) : formes exactes des
//!   dalles, escaliers, clôtures, trous du feuillage. Elle est gardée tant que rien ne
//!   change : une lumière n'est recalculée que si elle bouge ou si un bloc change près d'elle
//!   (empreintes de la grille par cubes de 8 blocs), au plus quelques cases par image. Les
//!   joueurs (boîtes) font de l'ombre directement dans le shader ;
//! - **volumétrique** : passe en demi-résolution après le monde (`volumetric.wgsl`), puis
//!   recomposée en tenant compte de la profondeur (`vl_composite.wgsl`) ; le TAA lisse le
//!   tramage.

use std::collections::HashMap;

use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec3, Vec4};

use crate::taa::COLOR_FORMAT;

/// Au plus autant de lumières dynamiques à la fois (les premières de la liste gardées).
pub const MAX_DYN_LIGHTS: usize = 256;
/// Tuiles de l'écran (chacune garde la liste des lumières qui peuvent la toucher).
pub const TILES_X: u32 = 32;
pub const TILES_Y: u32 = 18;
const WORDS: u32 = (MAX_DYN_LIGHTS / 32) as u32;
/// Boîtes des joueurs (ombres) : autant que les ombres du soleil.
const MAX_BOXES: usize = crate::MAX_SHADOW_BOXES;
/// Cases d'ombre calculées au plus par image.
const MAX_JOBS: usize = 256;
/// Taille des gobos sur la carte graphique (ils sont ramenés à cette taille).
pub const GOBO_SIZE: u32 = 256;
pub const MAX_GOBOS: usize = 64;
/// Projecteur plus large que ça (angle total) : ombres en cube, comme une lumière ponctuelle.
const SPOT_MAP_MAX_DEG: f32 = 120.0;
/// Qualité des ombres : (taille de l'atlas, taille d'une case), en pixels.
pub const SHADOW_QUALITY: [(u32, u32); 4] = [(2048, 128), (4096, 256), (8192, 512), (8192, 1024)];
/// Noms des qualités d'ombre.
pub const SHADOW_QUALITY_NAMES: [&str; 4] = ["Basse", "Moyenne", "Haute", "Ultra"];

const NOISE_SIZE: u32 = 32;
const VL_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
const ATLAS_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// Sorte de lumière.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DynKind {
    /// Lumière ponctuelle : dans toutes les directions.
    Point,
    /// Projecteur : dans un cône.
    Spot,
}

/// Une lumière dynamique pour une image (coordonnées monde).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DynLight {
    /// Identifiant stable (cache de sa carte d'ombre).
    pub id: u64,
    pub kind: DynKind,
    pub pos: [f32; 3],
    /// Direction du projecteur (normalisée ici).
    pub dir: [f32; 3],
    /// Couleur linéaire (0..1).
    pub color: [f32; 3],
    /// Intensité (0..20).
    pub intensity: f32,
    /// Portée en blocs (rayon d'une lumière ponctuelle, longueur d'un projecteur).
    pub range: f32,
    /// Angles du cône (degrés, angle total) : extérieur, et intérieur (pleine lumière).
    pub outer_deg: f32,
    pub inner_deg: f32,
    /// N'éclaire que les entités, ou que les blocs.
    pub entities_only: bool,
    pub blocks_only: bool,
    /// Taille de la source (blocs) : douceur des ombres ; 0 = réglage général.
    pub bulb: f32,
    pub shadows: bool,
    /// Lumière au centre d'un bloc lumineux : sa propre case ne lui fait pas d'ombre.
    pub inside_block: bool,
    /// Rayons volumétriques : force (0 = aucun), densité, anisotropie (−0,95..0,95).
    pub beam: f32,
    pub density: f32,
    pub anisotropy: f32,
    /// Gobo (projecteur) : couche (voir `Renderer::set_gobos`), rotation (rad), échelle,
    /// inversé.
    pub gobo: Option<u32>,
    pub gobo_rotation: f32,
    pub gobo_scale: f32,
    pub gobo_invert: bool,
}

impl Default for DynLight {
    fn default() -> Self {
        Self {
            id: 0,
            kind: DynKind::Point,
            pos: [0.0; 3],
            dir: [0.0, -1.0, 0.0],
            color: [1.0; 3],
            intensity: 1.0,
            range: 6.0,
            outer_deg: 35.0,
            inner_deg: 25.0,
            entities_only: false,
            blocks_only: false,
            bulb: 0.0,
            shadows: true,
            inside_block: false,
            beam: 1.0,
            density: 0.05,
            anisotropy: 0.4,
            gobo: None,
            gobo_rotation: 0.0,
            gobo_scale: 1.0,
            gobo_invert: false,
        }
    }
}

/// Réglages généraux des lumières dynamiques.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DynSettings {
    /// Intensité générale (surfaces et rayons).
    pub intensity: f32,
    /// Reflets des lumières sur les surfaces (0 = aucun).
    pub specular: f32,
    /// Éclairage en bandes (style dessin animé).
    pub toon: bool,
    pub toon_bands: u32,
    pub toon_smooth: f32,
    /// Ombres des lumières (interrupteur général).
    pub shadows: bool,
    /// Les blocs font de l'ombre (cartes calculées dans la grille de blocs).
    pub block_shadows: bool,
    /// Les joueurs font de l'ombre.
    pub entity_shadows: bool,
    /// Qualité des ombres (0 basse … 3 ultra), voir `SHADOW_QUALITY`.
    pub shadow_quality: u8,
    /// Cartes gardées tant que rien ne change (sinon recalculées à chaque image).
    pub shadow_cache: bool,
    /// Essai : ombres des surfaces par un rayon dans la grille de blocs (pénombre lissée
    /// par l'anticrénelage) au lieu des cartes d'ombre ; les cartes restent pour les rayons
    /// volumétriques, les translucides et hors de la grille.
    pub rt_shadows: bool,
    /// Douceur des ombres par défaut (taille de la source, blocs).
    pub softness: f32,
    /// Cases d'ombre recalculées au plus par image (les premiers calculs passent avant).
    pub bake_budget: u32,
    /// Lumière volumétrique.
    pub volumetric: bool,
    pub vl_intensity: f32,
    pub vl_steps: u32,
    pub vl_max_dist: f32,
    /// Lueur autour de la source : force et rayon (blocs).
    pub vl_tip_boost: f32,
    pub vl_tip_radius: f32,
    /// Ombres dans les rayons, un test tous les `vl_shadow_stride` pas.
    pub vl_shadows: bool,
    pub vl_shadow_stride: u32,
    /// Bruit de densité : part, taille (blocs), vitesse de dérive, métamorphose, pas.
    pub vl_noise: bool,
    pub vl_noise_amount: f32,
    pub vl_noise_scale: f32,
    pub vl_noise_speed: f32,
    pub vl_noise_morph: f32,
    pub vl_noise_stride: u32,
    /// Tramage qui tourne à chaque image (lissé par le TAA).
    pub vl_dither_temporal: bool,
}

impl Default for DynSettings {
    fn default() -> Self {
        Self {
            intensity: 1.0,
            specular: 1.0,
            toon: false,
            toon_bands: 3,
            toon_smooth: 0.1,
            shadows: true,
            block_shadows: true,
            entity_shadows: true,
            shadow_quality: 1,
            shadow_cache: true,
            rt_shadows: false,
            softness: 0.1,
            bake_budget: 12,
            volumetric: true,
            vl_intensity: 1.0,
            vl_steps: 48,
            vl_max_dist: 96.0,
            vl_tip_boost: 1.5,
            vl_tip_radius: 1.5,
            vl_shadows: true,
            vl_shadow_stride: 2,
            vl_noise: true,
            vl_noise_amount: 0.6,
            vl_noise_scale: 2.0,
            vl_noise_speed: 0.25,
            vl_noise_morph: 0.0,
            vl_noise_stride: 2,
            vl_dither_temporal: true,
        }
    }
}

/// Motif d'un gobo (niveaux de gris, ligne par ligne, du haut vers le bas) : blanc laisse
/// passer, noir arrête.
#[derive(Clone, Debug)]
pub struct GoboImage {
    pub width: u32,
    pub height: u32,
    pub luma: Vec<u8>,
}

/// Shaders des lumières dynamiques : compilés la première fois qu'une lumière est posée.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DynState {
    /// Aucune lumière n'a encore été posée : rien n'est compilé.
    #[default]
    Off,
    /// En cours de compilation (en tâche de fond) : les lumières s'afficheront après.
    Compiling,
    Ready,
    /// La carte graphique ou son pilote les refuse : pas de lumières (voir le journal).
    Failed,
}

/// État des lumières dynamiques (éditeur, écran de debug).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DynStats {
    pub state: DynState,
    pub lights: usize,
    /// Lumières qui ont une carte d'ombre prête.
    pub shadowed: usize,
    /// Lumières qui attendent une place ou leur premier calcul.
    pub waiting: usize,
    pub tiles_used: usize,
    pub tiles_total: usize,
    /// Cases calculées à la dernière image.
    pub baked: usize,
    pub volumetric: bool,
}

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable, Default, Debug, PartialEq)]
pub(crate) struct GpuLight {
    pos_range: [f32; 4],
    color: [f32; 4],
    dir_type: [f32; 4],
    cone: [f32; 4],
    vol: [f32; 4],
    cookie: [f32; 4],
    shadow: [f32; 4],
    boxes: [u32; 4],
    /// Calculé ici une fois pour toutes plutôt qu'à chaque pixel : repère du projecteur
    /// (droite, haut) avec 1 / tan du demi-angle et la rotation du gobo (cos, sin), case de
    /// sa carte d'ombre, sphère qui contient les boîtes des joueurs qui lui font de l'ombre.
    right: [f32; 4],
    up: [f32; 4],
    rect: [f32; 4],
    box_sphere: [f32; 4],
}

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
pub(crate) struct GpuHeader {
    counts: [u32; 4],
    vl_a: [f32; 4],
    vl_b: [f32; 4],
    vl_c: [u32; 4],
    sh: [f32; 4],
    surf: [f32; 4],
    misc: [f32; 4],
    /// Grille de blocs : coin bas (monde), w = 1 si elle est envoyée (ombres par rayons).
    vox: [i32; 4],
    /// Boîtes exactes des joueurs : lignes de la matrice monde → cube unité.
    box_x: [[f32; 4]; MAX_BOXES],
    box_y: [[f32; 4]; MAX_BOXES],
    box_z: [[f32; 4]; MAX_BOXES],
}

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable, Default)]
struct BakeJob {
    pos_range: [f32; 4],
    fwd: [f32; 4],
    right: [f32; 4],
    up: [f32; 4],
}

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable, Default)]
struct BakeUniform {
    origin: [i32; 4],
}

// Drapeaux de `vl_c.w` (voir dynlights.wgsl).
const F_VL_SHADOWS: u32 = 1;
const F_VL_NOISE: u32 = 2;
const F_DITHER: u32 = 4;
const F_SHADOWS: u32 = 8;
const F_TOON: u32 = 16;
const F_SPECULAR: u32 = 32;
const F_VOLUMETRIC: u32 = 64;
const F_BOX_SHADOWS: u32 = 128;
const F_RT_SHADOWS: u32 = 256;

/// Repère d'un projecteur dirigé selon `d` : (droite, haut, avant) — le même que
/// `dl_spot_basis` dans dynlights.wgsl.
pub fn spot_basis(d: Vec3) -> (Vec3, Vec3, Vec3) {
    let up = if d.y.abs() > 0.99 { Vec3::Z } else { Vec3::Y };
    let s = d.cross(up).normalize();
    let u = s.cross(d);
    (s, u, d)
}

/// Repère d'une face du cube (0 +X, 1 −X, 2 +Y, 3 −Y, 4 +Z, 5 −Z) : (droite, haut, avant)
/// — le même que `dl_face_basis`.
pub fn face_basis(f: u32) -> (Vec3, Vec3, Vec3) {
    match f {
        0 => (Vec3::NEG_Z, Vec3::Y, Vec3::X),
        1 => (Vec3::Z, Vec3::Y, Vec3::NEG_X),
        2 => (Vec3::X, Vec3::NEG_Z, Vec3::Y),
        3 => (Vec3::X, Vec3::Z, Vec3::NEG_Y),
        4 => (Vec3::X, Vec3::Y, Vec3::Z),
        _ => (Vec3::NEG_X, Vec3::Y, Vec3::NEG_Z),
    }
}

/// Face du cube que regarde `d` (même règle que `dl_cube_face`).
#[cfg(test)]
fn cube_face(d: Vec3) -> u32 {
    let a = d.abs();
    if a.x >= a.y && a.x >= a.z {
        if d.x > 0.0 {
            0
        } else {
            1
        }
    } else if a.y >= a.z {
        if d.y > 0.0 {
            2
        } else {
            3
        }
    } else if d.z > 0.0 {
        4
    } else {
        5
    }
}

/// Direction normalisée, ou vers le bas si elle est nulle.
fn unit_dir(d: [f32; 3]) -> Vec3 {
    let v = Vec3::from(d);
    if v.length_squared() > 1e-8 {
        v.normalize()
    } else {
        Vec3::NEG_Y
    }
}

/// Carte d'ombre d'une lumière : sorte (1 projecteur, 2 cube) et tangente du demi-angle.
fn shadow_shape(l: &DynLight) -> (u8, f32) {
    match l.kind {
        DynKind::Spot if l.outer_deg <= SPOT_MAP_MAX_DEG => {
            // Un peu plus large que le cône : son bord reste dans la carte.
            let half = (l.outer_deg * 0.5 + 2.0).clamp(1.0, 62.0);
            (1, half.to_radians().tan())
        }
        _ => (2, 1.0),
    }
}

/// Sphère qui contient la lumière (centre, rayon) : toute sa portée, ou seulement son cône.
pub fn bounding_sphere(l: &DynLight) -> (Vec3, f32) {
    let p = Vec3::from(l.pos);
    let r = l.range.max(0.01);
    if l.kind == DynKind::Spot && l.outer_deg <= 120.0 {
        // Cône de sommet p, de longueur r, de demi-angle θ ≤ 60° : la sphère centrée à
        // mi-longueur, de rayon r √(5/4 − cos θ), contient le sommet et le bord du cap.
        let c = (l.outer_deg * 0.5).to_radians().cos();
        let d = unit_dir(l.dir);
        (p + d * r * 0.5, r * (1.25 - c).max(0.25).sqrt())
    } else {
        (p, r)
    }
}

/// Rectangle de l'écran (uv, 0..1, y vers le bas) que couvre la sphère, ou `None` si elle
/// est hors champ. Prudent : boîte de la sphère projetée ; tout l'écran si la caméra est
/// dedans ou derrière un de ses coins.
pub fn screen_rect(view_proj: Mat4, eye: Vec3, center: Vec3, radius: f32) -> Option<[f32; 4]> {
    if (center - eye).length() < radius * 1.74 + 0.1 {
        return Some([0.0, 0.0, 1.0, 1.0]);
    }
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    let mut behind = 0;
    for k in 0..8 {
        let o = Vec3::new(
            if k & 1 == 0 { -radius } else { radius },
            if k & 2 == 0 { -radius } else { radius },
            if k & 4 == 0 { -radius } else { radius },
        );
        let c = view_proj * Vec4::from((center + o, 1.0));
        if c.w <= 1e-4 {
            behind += 1;
            continue;
        }
        let (x, y) = (c.x / c.w, c.y / c.w);
        x0 = x0.min(x);
        x1 = x1.max(x);
        y0 = y0.min(y);
        y1 = y1.max(y);
    }
    // Toute la boîte derrière la caméra : invisible ; seulement une partie : elle peut
    // couvrir n'importe quel point de l'écran.
    if behind == 8 {
        return None;
    }
    if behind > 0 {
        return Some([0.0, 0.0, 1.0, 1.0]);
    }
    if x1 < -1.0 || x0 > 1.0 || y1 < -1.0 || y0 > 1.0 {
        return None;
    }
    let u0 = (x0 * 0.5 + 0.5).clamp(0.0, 1.0);
    let u1 = (x1 * 0.5 + 0.5).clamp(0.0, 1.0);
    let v0 = (0.5 - y1 * 0.5).clamp(0.0, 1.0);
    let v1 = (0.5 - y0 * 0.5).clamp(0.0, 1.0);
    Some([u0, v0, u1, v1])
}

/// Range chaque lumière dans les tuiles de l'écran qu'elle peut toucher (bit `i` du masque).
pub fn bin_tiles(lights: &[DynLight], view_proj: Mat4, eye: Vec3, tiles: &mut [u32]) {
    tiles.fill(0);
    for (i, l) in lights.iter().enumerate().take(MAX_DYN_LIGHTS) {
        let (c, r) = bounding_sphere(l);
        let Some([u0, v0, u1, v1]) = screen_rect(view_proj, eye, c, r) else {
            continue;
        };
        let tx0 = ((u0 * TILES_X as f32).floor() as u32).min(TILES_X - 1);
        let tx1 = ((u1 * TILES_X as f32).floor() as u32).min(TILES_X - 1);
        let ty0 = ((v0 * TILES_Y as f32).floor() as u32).min(TILES_Y - 1);
        let ty1 = ((v1 * TILES_Y as f32).floor() as u32).min(TILES_Y - 1);
        let (word, bit) = (i as u32 / 32, 1u32 << (i % 32));
        for ty in ty0..=ty1 {
            for tx in tx0..=tx1 {
                tiles[((ty * TILES_X + tx) * WORDS + word) as usize] |= bit;
            }
        }
    }
}

/// Rectangles (NDC : x0, y0, x1, y1) des cases d'écran où il y a au moins une lumière, les
/// cases voisines d'une même ligne réunies : les passes des lumières ne dessinent que là.
fn lit_rects(tiles: &[u32], out: &mut Vec<[f32; 4]>) {
    out.clear();
    let lit = |x: u32, y: u32| {
        let base = ((y * TILES_X + x) * WORDS) as usize;
        tiles[base..base + WORDS as usize].iter().any(|w| *w != 0)
    };
    for ty in 0..TILES_Y {
        let mut tx = 0;
        while tx < TILES_X {
            if !lit(tx, ty) {
                tx += 1;
                continue;
            }
            let start = tx;
            while tx < TILES_X && lit(tx, ty) {
                tx += 1;
            }
            out.push([
                start as f32 / TILES_X as f32 * 2.0 - 1.0,
                1.0 - ty as f32 / TILES_Y as f32 * 2.0,
                tx as f32 / TILES_X as f32 * 2.0 - 1.0,
                1.0 - (ty + 1) as f32 / TILES_Y as f32 * 2.0,
            ]);
        }
    }
}

/// Boîte (coins) et sphère se touchent-elles ?
fn box_touches_sphere(mn: [f32; 3], mx: [f32; 3], c: Vec3, r: f32) -> bool {
    let q = c.clamp(Vec3::from(mn), Vec3::from(mx));
    q.distance_squared(c) <= r * r
}

/// Une lumière dans le format du shader. `tile` : première case d'ombre prête (sinon pas
/// d'ombre des blocs).
/// `rect` : coin et taille (uv) de la case d'ombre d'un projecteur ; `box_sphere` : sphère
/// (centre, rayon ; rayon < 0 : aucune) qui contient les boîtes de `boxes`.
fn pack_light(
    l: &DynLight,
    tile: Option<u32>,
    boxes: u64,
    rect: [f32; 3],
    box_sphere: [f32; 4],
) -> GpuLight {
    let spot = l.kind == DynKind::Spot;
    let outer = l.outer_deg.clamp(1.0, 179.0);
    let inner = l.inner_deg.clamp(0.0, outer);
    let d = unit_dir(l.dir);
    let col = l.color.map(|c| c.max(0.0) * l.intensity.max(0.0));
    let mask = if l.entities_only {
        1.0
    } else if l.blocks_only {
        2.0
    } else {
        0.0
    };
    let (kind, tan_half) = shadow_shape(l);
    let (sk, slot) = match tile {
        Some(t) => (kind as f32, t as f32),
        None => (0.0, -1.0),
    };
    GpuLight {
        pos_range: [l.pos[0], l.pos[1], l.pos[2], l.range.max(0.05)],
        color: [col[0], col[1], col[2], l.intensity.max(0.0)],
        dir_type: [d.x, d.y, d.z, if spot { 1.0 } else { 0.0 }],
        cone: if spot {
            [
                (outer * 0.5).to_radians().cos(),
                (inner * 0.5).to_radians().cos(),
                mask,
                l.bulb.max(0.0),
            ]
        } else {
            [-1.0, -1.0, mask, l.bulb.max(0.0)]
        },
        vol: [
            l.anisotropy.clamp(-0.95, 0.95),
            l.density.clamp(0.0005, 2.0),
            l.beam.max(0.0),
            slot,
        ],
        cookie: match l.gobo {
            Some(layer) if spot => [
                layer as f32,
                l.gobo_rotation,
                l.gobo_scale.max(0.01),
                if l.gobo_invert { 1.0 } else { 0.0 },
            ],
            _ => [-1.0, 0.0, 1.0, 0.0],
        },
        shadow: [sk, tan_half, 0.0, 0.0],
        boxes: [boxes as u32, (boxes >> 32) as u32, 0, 0],
        right: {
            let (r, _, _) = spot_basis(d);
            // 1 / tan du demi-angle extérieur (le bord du cône tombe au bord du gobo).
            let c = (outer * 0.5).to_radians().cos().clamp(-1.0, 1.0);
            let fy = (c / (1.0 - c * c).max(1e-12).sqrt()).min(114.58865);
            [r.x, r.y, r.z, fy]
        },
        up: {
            let (_, u, _) = spot_basis(d);
            [u.x, u.y, u.z, l.gobo_rotation.cos()]
        },
        rect: [rect[0], rect[1], rect[2], l.gobo_rotation.sin()],
        box_sphere,
    }
}

/// Sphère (centre, rayon) qui contient les boîtes de `mask` ; rayon −1 sans boîte.
fn boxes_sphere(boxes: &[crate::ShadowBox], mask: u64) -> [f32; 4] {
    let mut lo = Vec3::splat(f32::MAX);
    let mut hi = Vec3::splat(f32::MIN);
    for (k, b) in boxes.iter().enumerate().take(64) {
        if mask & (1 << k) != 0 {
            lo = lo.min(Vec3::from(b.min));
            hi = hi.max(Vec3::from(b.max));
        }
    }
    if lo.x > hi.x {
        return [0.0, 0.0, 0.0, -1.0];
    }
    let c = (lo + hi) * 0.5;
    // Un peu plus grande : jamais une ombre de joueur écartée par erreur d'arrondi.
    [c.x, c.y, c.z, (hi - lo).length() * 0.5 + 0.05]
}

/// Ce qui fait changer la carte d'ombre d'une lumière.
#[derive(Clone, Copy, Debug, PartialEq)]
struct ShadowKey {
    kind: u8,
    tan_half: f32,
    pos: [f32; 3],
    range: f32,
    dir: [f32; 3],
    inside: bool,
}

impl ShadowKey {
    fn of(l: &DynLight) -> Self {
        let (kind, tan_half) = shadow_shape(l);
        let d = if kind == 1 {
            unit_dir(l.dir).to_array()
        } else {
            [0.0; 3]
        };
        Self {
            kind,
            tan_half,
            pos: l.pos,
            range: l.range.max(0.05),
            dir: d,
            inside: l.inside_block,
        }
    }

    fn tiles(&self) -> u32 {
        if self.kind == 2 {
            6
        } else {
            1
        }
    }
}

struct Slot {
    tile: u32,
    count: u32,
    key: ShadowKey,
    /// Carte calculée au moins une fois (sinon la lumière attend, sans ombre).
    baked: bool,
    /// À recalculer (lumière déplacée, bloc changé près d'elle).
    dirty: bool,
    seen: u64,
}

/// Places dans l'atlas des ombres : cases libres, et carte de chaque lumière.
#[derive(Default)]
pub(crate) struct ShadowCache {
    used: Vec<bool>,
    slots: HashMap<u64, Slot>,
}

impl ShadowCache {
    fn reset(&mut self, tiles: usize) {
        self.used = vec![false; tiles];
        self.slots.clear();
    }

    /// `n` cases consécutives libres.
    fn alloc(&mut self, n: u32) -> Option<u32> {
        let n = n as usize;
        let mut run = 0;
        for i in 0..self.used.len() {
            run = if self.used[i] { 0 } else { run + 1 };
            if run == n {
                let start = i + 1 - n;
                self.used[start..=i].fill(true);
                return Some(start as u32);
            }
        }
        None
    }

    fn free(&mut self, s: &Slot) {
        let (a, b) = (s.tile as usize, (s.tile + s.count) as usize);
        if b <= self.used.len() {
            self.used[a..b].fill(false);
        }
    }

    /// Libère la place d'une lumière qui n'en a plus demandé depuis le plus longtemps (pas
    /// à cette image).
    fn evict_oldest(&mut self, frame: u64) -> bool {
        let Some((&id, _)) = self
            .slots
            .iter()
            .filter(|(_, s)| s.seen < frame)
            .min_by_key(|(_, s)| s.seen)
        else {
            return false;
        };
        if let Some(s) = self.slots.remove(&id) {
            self.free(&s);
        }
        true
    }

    /// Place de la lumière `l` (vue à l'image `frame`) ; `None` : l'atlas est plein.
    fn request(&mut self, l: &DynLight, frame: u64) -> Option<&mut Slot> {
        let key = ShadowKey::of(l);
        let n = key.tiles();
        let same_size = self.slots.get(&l.id).map(|s| s.count == n);
        if same_size == Some(false) {
            if let Some(s) = self.slots.remove(&l.id) {
                self.free(&s);
            }
        }
        if !self.slots.contains_key(&l.id) {
            let tile = loop {
                if let Some(t) = self.alloc(n) {
                    break t;
                }
                if !self.evict_oldest(frame) {
                    return None;
                }
            };
            self.slots.insert(
                l.id,
                Slot {
                    tile,
                    count: n,
                    key,
                    baked: false,
                    dirty: true,
                    seen: frame,
                },
            );
        }
        let s = self.slots.get_mut(&l.id)?;
        if s.key != key {
            s.key = key;
            s.dirty = true;
        }
        s.seen = frame;
        Some(s)
    }

    /// Oublie les lumières qui n'ont rien demandé depuis `keep` images.
    fn sweep(&mut self, frame: u64, keep: u64) {
        let old: Vec<u64> = self
            .slots
            .iter()
            .filter(|(_, s)| s.seen + keep < frame)
            .map(|(&id, _)| id)
            .collect();
        for id in old {
            if let Some(s) = self.slots.remove(&id) {
                self.free(&s);
            }
        }
    }

    /// Des blocs ont changé dans ces boîtes (monde) : cartes des lumières qui les touchent à
    /// recalculer.
    fn invalidate(&mut self, boxes: &[([i32; 3], [i32; 3])]) {
        for s in self.slots.values_mut() {
            let c = Vec3::from(s.key.pos);
            let r = s.key.range;
            let hit = boxes.iter().any(|(a, b)| {
                box_touches_sphere(a.map(|v| v as f32), b.map(|v| v as f32), c, r + 1.0)
            });
            if hit {
                s.dirty = true;
            }
        }
    }

    fn invalidate_all(&mut self) {
        for s in self.slots.values_mut() {
            s.dirty = true;
        }
    }
}

/// Empreintes de la grille de blocs par cubes de 8 (coin monde du cube → empreinte), pour
/// savoir où des blocs ont changé d'une grille à l'autre.
fn brick_hashes(origin: [i32; 3], ids: &[u16]) -> HashMap<[i32; 3], u64> {
    use crate::rt::VOX;
    let [vx, vy, vz] = VOX.map(|v| v as usize);
    let mut out = HashMap::with_capacity((vx / 8) * (vy / 8) * (vz / 8));
    if ids.len() != vx * vy * vz {
        return out;
    }
    for bz in 0..vz / 8 {
        for by in 0..vy / 8 {
            for bx in 0..vx / 8 {
                let mut h: u64 = 0xcbf2_9ce4_8422_2325;
                for z in bz * 8..bz * 8 + 8 {
                    for y in by * 8..by * 8 + 8 {
                        let row = (bx * 8) + vx * (y + vy * z);
                        let mut v: u64 = 0;
                        for (k, &id) in ids[row..row + 8].iter().enumerate() {
                            // Sans la lumière du ciel (bits 12..15) : seuls les blocs comptent.
                            v ^= ((id & 0x0fff) as u64)
                                .wrapping_mul(0x9e37_79b9_7f4a_7c15)
                                .rotate_left(k as u32 * 7);
                        }
                        h = (h ^ v).wrapping_mul(0x0100_0000_01b3);
                    }
                }
                let corner = [
                    origin[0] + (bx * 8) as i32,
                    origin[1] + (by * 8) as i32,
                    origin[2] + (bz * 8) as i32,
                ];
                out.insert(corner, h);
            }
        }
    }
    out
}

/// Cubes de 8 blocs qui diffèrent entre deux grilles (y compris ceux qui entrent ou sortent
/// de la grille) : boîtes monde (coin bas, coin haut).
fn changed_bricks(
    old: &HashMap<[i32; 3], u64>,
    new: &HashMap<[i32; 3], u64>,
) -> Vec<([i32; 3], [i32; 3])> {
    let mut out = Vec::new();
    for (c, h) in new {
        if old.get(c) != Some(h) {
            out.push((*c, [c[0] + 8, c[1] + 8, c[2] + 8]));
        }
    }
    for c in old.keys() {
        if !new.contains_key(c) {
            out.push((*c, [c[0] + 8, c[1] + 8, c[2] + 8]));
        }
    }
    out
}

/// Bruit de valeur 3D qui se répète (`n`³ octets).
fn value_noise(n: u32) -> Vec<u8> {
    let n = n as usize;
    let hash = |x: usize, y: usize, z: usize| -> f32 {
        let mut h = (x as u32).wrapping_mul(73_856_093)
            ^ (y as u32).wrapping_mul(19_349_663)
            ^ (z as u32).wrapping_mul(83_492_791);
        h = (h ^ (h >> 13)).wrapping_mul(1_274_126_177);
        h ^= h >> 16;
        (h & 0xffff) as f32 / 65535.0
    };
    // Grille grossière (4 texels par cellule) interpolée en douceur : des bouffées, pas du
    // grain.
    let cell = 4;
    let m = n / cell;
    let mut out = vec![0u8; n * n * n];
    for z in 0..n {
        for y in 0..n {
            for x in 0..n {
                let (fx, fy, fz) = (
                    x as f32 / cell as f32,
                    y as f32 / cell as f32,
                    z as f32 / cell as f32,
                );
                let (ix, iy, iz) = (
                    fx.floor() as usize,
                    fy.floor() as usize,
                    fz.floor() as usize,
                );
                let s = |t: f32| t * t * (3.0 - 2.0 * t);
                let (ux, uy, uz) = (s(fx.fract()), s(fy.fract()), s(fz.fract()));
                let c = |dx: usize, dy: usize, dz: usize| {
                    hash((ix + dx) % m, (iy + dy) % m, (iz + dz) % m)
                };
                let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t;
                let v = lerp(
                    lerp(
                        lerp(c(0, 0, 0), c(1, 0, 0), ux),
                        lerp(c(0, 1, 0), c(1, 1, 0), ux),
                        uy,
                    ),
                    lerp(
                        lerp(c(0, 0, 1), c(1, 0, 1), ux),
                        lerp(c(0, 1, 1), c(1, 1, 1), ux),
                        uy,
                    ),
                    uz,
                );
                out[x + n * (y + n * z)] = (v * 255.0).round() as u8;
            }
        }
    }
    out
}

/// Image ramenée à `GOBO_SIZE` × `GOBO_SIZE` (moyenne des pixels couverts).
fn resample_gobo(img: &GoboImage) -> Vec<u8> {
    let s = GOBO_SIZE as usize;
    let mut out = vec![255u8; s * s];
    if img.width == 0 || img.height == 0 || img.luma.len() < (img.width * img.height) as usize {
        return out;
    }
    let (w, h) = (img.width as usize, img.height as usize);
    for y in 0..s {
        let y0 = y * h / s;
        let y1 = ((y + 1) * h / s).max(y0 + 1).min(h);
        for x in 0..s {
            let x0 = x * w / s;
            let x1 = ((x + 1) * w / s).max(x0 + 1).min(w);
            let mut sum = 0u32;
            for yy in y0..y1 {
                for xx in x0..x1 {
                    sum += img.luma[yy * w + xx] as u32;
                }
            }
            out[y * s + x] = (sum / ((y1 - y0) * (x1 - x0)) as u32) as u8;
        }
    }
    out
}

/// Images de la lumière volumétrique (taille du monde dessiné / 2) et leurs liaisons.
struct VlTargets {
    key: (u32, u32, u64),
    view: wgpu::TextureView,
    /// Groupes de la passe volumétrique et de la recomposition, selon la profondeur lue.
    vl_groups: [wgpu::BindGroup; 2],
    comp_groups: [wgpu::BindGroup; 2],
}

pub(crate) struct VlPipes {
    march: wgpu::RenderPipeline,
    composite: wgpu::RenderPipeline,
}

/// Liaisons de l'éclairage différé (profondeur lue, albédo, surface), refaites quand les
/// images du TAA sont recréées.
struct DeferredTargets {
    generation: u64,
    groups: [wgpu::BindGroup; 2],
}

/// Ressources GPU des lumières dynamiques.
pub(crate) struct DynGpu {
    header: wgpu::Buffer,
    lights_buf: wgpu::Buffer,
    tiles_buf: wgpu::Buffer,
    /// Rectangles des cases éclairées (voir `lit_rects`) : où dessiner les passes des
    /// lumières.
    rects_buf: wgpu::Buffer,
    rects: Vec<[f32; 4]>,
    atlas: Option<(wgpu::Texture, wgpu::TextureView)>,
    atlas_size: u32,
    tile_size: u32,
    quality: u8,
    max_texture: u32,
    cmp_sampler: wgpu::Sampler,
    gobo_view: wgpu::TextureView,
    gobo_sampler: wgpu::Sampler,
    // Calcul des cartes d'ombre (le pipeline est compilé avec les autres shaders des
    // lumières, voir `LightPipes`).
    pub(crate) bake_bgl: wgpu::BindGroupLayout,
    bake_group: Option<wgpu::BindGroup>,
    jobs_buf: wgpu::Buffer,
    bake_uniform: wgpu::Buffer,
    jobs: Vec<(u32, BakeJob)>,
    // Volumétrique.
    pub(crate) vl_bgl: wgpu::BindGroupLayout,
    pub(crate) comp_bgl: wgpu::BindGroupLayout,
    noise_view: wgpu::TextureView,
    noise_sampler: wgpu::Sampler,
    vl_targets: Option<VlTargets>,
    // Éclairage différé.
    pub(crate) def_bgl: wgpu::BindGroupLayout,
    def_targets: Option<DeferredTargets>,
    // État de l'image.
    lights: Vec<DynLight>,
    settings: DynSettings,
    /// Ombres par lancer de rayons demandées et possibles cette image (voir `prepare`).
    rt_on: bool,
    cache: ShadowCache,
    bricks: HashMap<[i32; 3], u64>,
    frame: u64,
    pub stats: DynStats,
    /// Les liaisons du monde sont à refaire (atlas ou gobos recréés).
    pub rebind: bool,
    vl_active: bool,
    gpu_lights: Vec<GpuLight>,
    tiles: Vec<u32>,
}

/// Entrées du groupe du monde (liaisons 7 à 13) pour les lumières dynamiques.
pub(crate) fn world_layout_entries() -> [wgpu::BindGroupLayoutEntry; 7] {
    let frag = wgpu::ShaderStages::FRAGMENT;
    [
        wgpu::BindGroupLayoutEntry {
            binding: 7,
            visibility: frag,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        },
        wgpu::BindGroupLayoutEntry {
            binding: 8,
            visibility: frag,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only: true },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        },
        wgpu::BindGroupLayoutEntry {
            binding: 9,
            visibility: frag,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only: true },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        },
        wgpu::BindGroupLayoutEntry {
            binding: 10,
            visibility: frag,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Depth,
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        },
        wgpu::BindGroupLayoutEntry {
            binding: 11,
            visibility: frag,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
            count: None,
        },
        wgpu::BindGroupLayoutEntry {
            binding: 12,
            visibility: frag,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2Array,
                multisampled: false,
            },
            count: None,
        },
        wgpu::BindGroupLayoutEntry {
            binding: 13,
            visibility: frag,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            count: None,
        },
    ]
}

fn tex_entry(
    binding: u32,
    sample_type: wgpu::TextureSampleType,
    dim: wgpu::TextureViewDimension,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type,
            view_dimension: dim,
            multisampled: false,
        },
        count: None,
    }
}

/// Source WGSL complète d'un shader qui utilise les lumières dynamiques.
pub(crate) fn with_dynlights(src: &str) -> String {
    format!(
        "{src}\n{}\n{}\n{}",
        include_str!("shaders/gbuffer.wgsl"),
        include_str!("shaders/dynlights.wgsl"),
        include_str!("shaders/dl_rt_off.wgsl")
    )
}

/// Comme `with_dynlights`, avec les ombres par lancer de rayons dans la grille de blocs
/// (essai, éclairage différé seulement : il déclare les liaisons 1 à 4 du monde).
pub(crate) fn with_dynlights_rt(src: &str) -> String {
    format!(
        "{src}\n{}\n{}\n{}",
        include_str!("shaders/gbuffer.wgsl"),
        include_str!("shaders/dynlights.wgsl"),
        include_str!("shaders/dl_rt.wgsl")
    )
}

/// Le même shader sans le code des lumières (`dyn_active()` toujours faux) : celui de tous
/// les jours, tant qu'aucune lumière n'est posée.
pub(crate) fn without_dynlights(src: &str) -> String {
    format!(
        "{src}\n{}\n{}",
        include_str!("shaders/gbuffer.wgsl"),
        include_str!("shaders/dynlights_off.wgsl")
    )
}

/// Éclairage différé avec les ombres par lancer de rayons (essai) : compilé à part, et
/// seulement quand le réglage est coché (le code de la marche dans la grille allonge la
/// compilation sous DirectX).
pub(crate) fn deferred_rt_pipeline(
    device: &wgpu::Device,
    world_bgl: &wgpu::BindGroupLayout,
    def_bgl: &wgpu::BindGroupLayout,
) -> wgpu::RenderPipeline {
    let src = with_dynlights_rt(include_str!("shaders/dyn_deferred.wgsl"));
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("éclairage différé (rayons)"),
        source: wgpu::ShaderSource::Wgsl(src.into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("éclairage différé (rayons)"),
        bind_group_layouts: &[world_bgl, def_bgl],
        push_constant_ranges: &[],
    });
    let add = wgpu::BlendComponent {
        src_factor: wgpu::BlendFactor::One,
        dst_factor: wgpu::BlendFactor::One,
        operation: wgpu::BlendOperation::Add,
    };
    let keep_alpha = wgpu::BlendComponent {
        src_factor: wgpu::BlendFactor::Zero,
        dst_factor: wgpu::BlendFactor::One,
        operation: wgpu::BlendOperation::Add,
    };
    fullscreen_pipeline(
        device,
        "éclairage différé (rayons)",
        &layout,
        &shader,
        "vs_tiles",
        "fs_deferred",
        COLOR_FORMAT,
        Some(wgpu::BlendState {
            color: add,
            alpha: keep_alpha,
        }),
    )
}

/// Compile `f` et note son temps dans le journal (`label`).
fn timed<T>(label: &str, f: impl FnOnce() -> T) -> T {
    let t0 = std::time::Instant::now();
    let out = f();
    log::info!("  {label} : {:.1} s", t0.elapsed().as_secs_f32());
    out
}

/// Pipelines des lumières : éclairage différé, calcul des cartes d'ombre, rayons
/// volumétriques et leur recomposition, chacun sur son fil (`Renderer::spawn_lit`).
pub(crate) fn light_pipes(
    device: &wgpu::Device,
    world_bgl: &wgpu::BindGroupLayout,
    bake_bgl: &wgpu::BindGroupLayout,
    vl_bgl: &wgpu::BindGroupLayout,
    comp_bgl: &wgpu::BindGroupLayout,
    def_bgl: &wgpu::BindGroupLayout,
    surface: wgpu::TextureFormat,
) -> (
    wgpu::RenderPipeline,
    VlPipes,
    wgpu::RenderPipeline,
    wgpu::RenderPipeline,
) {
    let bake_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("ombres : calcul"),
        bind_group_layouts: &[bake_bgl],
        push_constant_ranges: &[],
    });
    let bake_shader = device.create_shader_module(wgpu::include_wgsl!("shaders/shadow_bake.wgsl"));
    // Volumétrique : la marche lit la scène (groupe 1) et les lumières (groupe 0 du monde) ;
    // la recomposition a son propre groupe.
    let src = with_dynlights(include_str!("shaders/volumetric.wgsl"));
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("volumétrique"),
        source: wgpu::ShaderSource::Wgsl(src.into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("volumétrique"),
        bind_group_layouts: &[world_bgl, vl_bgl],
        push_constant_ranges: &[],
    });
    let comp_shader = device.create_shader_module(wgpu::include_wgsl!("shaders/vl_composite.wgsl"));
    let comp_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("volumétrique : recomposition"),
        bind_group_layouts: &[comp_bgl],
        push_constant_ranges: &[],
    });
    // Éclairage différé : lit le monde (groupe 0, lumières comprises) et ce que les
    // opaques ont laissé (groupe 1).
    let def_src = with_dynlights(include_str!("shaders/dyn_deferred.wgsl"));
    let def_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("éclairage différé"),
        source: wgpu::ShaderSource::Wgsl(def_src.into()),
    });
    let def_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("éclairage différé"),
        bind_group_layouts: &[world_bgl, def_bgl],
        push_constant_ranges: &[],
    });
    let add = wgpu::BlendComponent {
        src_factor: wgpu::BlendFactor::One,
        dst_factor: wgpu::BlendFactor::One,
        operation: wgpu::BlendOperation::Add,
    };
    let keep_alpha = wgpu::BlendComponent {
        src_factor: wgpu::BlendFactor::Zero,
        dst_factor: wgpu::BlendFactor::One,
        operation: wgpu::BlendOperation::Add,
    };
    let additive = Some(wgpu::BlendState {
        color: add,
        alpha: keep_alpha,
    });
    // Tous en même temps, chacun sur son fil (compilation lente sous DirectX).
    std::thread::scope(|sc| {
        let arm = sc.spawn(|| {
            timed("bras à la première personne", || {
                crate::viewmodel::arm_lit_pipeline(device, world_bgl, surface)
            })
        });
        let deferred = sc.spawn(|| {
            timed("éclairage différé", || {
                fullscreen_pipeline(
                    device,
                    "éclairage différé",
                    &def_layout,
                    &def_shader,
                    "vs_tiles",
                    "fs_deferred",
                    COLOR_FORMAT,
                    additive,
                )
            })
        });
        let bake = sc.spawn(|| {
            timed("cartes d'ombre", || {
                device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some("ombres des lumières : calcul"),
                    layout: Some(&bake_layout),
                    vertex: wgpu::VertexState {
                        module: &bake_shader,
                        entry_point: Some("vs_bake"),
                        compilation_options: Default::default(),
                        buffers: &[],
                    },
                    primitive: Default::default(),
                    depth_stencil: Some(wgpu::DepthStencilState {
                        format: ATLAS_FORMAT,
                        depth_write_enabled: true,
                        depth_compare: wgpu::CompareFunction::Always,
                        stencil: Default::default(),
                        bias: Default::default(),
                    }),
                    multisample: Default::default(),
                    fragment: Some(wgpu::FragmentState {
                        module: &bake_shader,
                        entry_point: Some("fs_bake"),
                        compilation_options: Default::default(),
                        targets: &[],
                    }),
                    multiview: None,
                    cache: None,
                })
            })
        });
        let march = sc.spawn(|| {
            timed("rayons volumétriques", || {
                fullscreen_pipeline(
                    device,
                    "volumétrique",
                    &layout,
                    &shader,
                    "vs_tiles",
                    "fs_vl",
                    VL_FORMAT,
                    None,
                )
            })
        });
        let composite = sc.spawn(|| {
            timed("recomposition des rayons", || {
                fullscreen_pipeline(
                    device,
                    "volumétrique : recomposition",
                    &comp_layout,
                    &comp_shader,
                    "vs_full",
                    "fs_composite",
                    COLOR_FORMAT,
                    additive,
                )
            })
        });
        (
            bake.join().expect("pipeline des ombres des lumières"),
            VlPipes {
                march: march.join().expect("pipeline volumétrique"),
                composite: composite.join().expect("pipeline de recomposition"),
            },
            deferred.join().expect("pipeline de l'éclairage différé"),
            arm.join().expect("pipeline du bras éclairé"),
        )
    })
}

#[allow(clippy::too_many_arguments)]
fn fullscreen_pipeline(
    device: &wgpu::Device,
    label: &str,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    vs: &str,
    fs: &str,
    format: wgpu::TextureFormat,
    blend: Option<wgpu::BlendState>,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some(vs),
            compilation_options: Default::default(),
            buffers: &[],
        },
        primitive: Default::default(),
        depth_stencil: None,
        multisample: Default::default(),
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some(fs),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend,
                write_mask: wgpu::ColorWrites::COLOR,
            })],
        }),
        multiview: None,
        cache: None,
    })
}

impl DynGpu {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let buf = |label: &str, size: u64, usage| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size,
                usage: usage | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        };
        let header = buf(
            "lumières : en-tête",
            std::mem::size_of::<GpuHeader>() as u64,
            wgpu::BufferUsages::UNIFORM,
        );
        let lights_buf = buf(
            "lumières",
            (MAX_DYN_LIGHTS * std::mem::size_of::<GpuLight>()) as u64,
            wgpu::BufferUsages::STORAGE,
        );
        let tiles_buf = buf(
            "lumières : tuiles",
            (TILES_X * TILES_Y * WORDS * 4) as u64,
            wgpu::BufferUsages::STORAGE,
        );
        let rects_buf = buf(
            "lumières : cases éclairées",
            (TILES_X * TILES_Y * 16) as u64,
            wgpu::BufferUsages::STORAGE,
        );
        let jobs_buf = buf(
            "ombres : travaux",
            (MAX_JOBS * std::mem::size_of::<BakeJob>()) as u64,
            wgpu::BufferUsages::STORAGE,
        );
        let bake_uniform = buf(
            "ombres : grille",
            std::mem::size_of::<BakeUniform>() as u64,
            wgpu::BufferUsages::UNIFORM,
        );
        let cmp_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("ombres des lumières"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            compare: Some(wgpu::CompareFunction::LessEqual),
            ..Default::default()
        });
        let gobo_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("gobos"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let noise_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("bruit des rayons"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            address_mode_w: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        // Bruit 3D des rayons.
        let noise = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("bruit des rayons"),
            size: wgpu::Extent3d {
                width: NOISE_SIZE,
                height: NOISE_SIZE,
                depth_or_array_layers: NOISE_SIZE,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D3,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &noise,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &value_noise(NOISE_SIZE),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(NOISE_SIZE),
                rows_per_image: Some(NOISE_SIZE),
            },
            wgpu::Extent3d {
                width: NOISE_SIZE,
                height: NOISE_SIZE,
                depth_or_array_layers: NOISE_SIZE,
            },
        );
        let noise_view = noise.create_view(&Default::default());

        // Calcul des cartes d'ombre : rayons dans la grille de blocs.
        let bake_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ombres : calcul"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                tex_entry(
                    2,
                    wgpu::TextureSampleType::Uint,
                    wgpu::TextureViewDimension::D3,
                ),
                tex_entry(
                    3,
                    wgpu::TextureSampleType::Uint,
                    wgpu::TextureViewDimension::D1,
                ),
                tex_entry(
                    4,
                    wgpu::TextureSampleType::Float { filterable: true },
                    wgpu::TextureViewDimension::D2Array,
                ),
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        // Volumétrique : la marche lit la scène (groupe 1) et les lumières (groupe 0 du
        // monde) ; la recomposition a son propre groupe.
        let vl_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("volumétrique"),
            entries: &[
                tex_entry(
                    0,
                    wgpu::TextureSampleType::Depth,
                    wgpu::TextureViewDimension::D2,
                ),
                tex_entry(
                    1,
                    wgpu::TextureSampleType::Float { filterable: true },
                    wgpu::TextureViewDimension::D3,
                ),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                // Cases éclairées : seulement là que la passe dessine.
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let comp_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("volumétrique : recomposition"),
            entries: &[
                tex_entry(
                    0,
                    wgpu::TextureSampleType::Float { filterable: false },
                    wgpu::TextureViewDimension::D2,
                ),
                tex_entry(
                    1,
                    wgpu::TextureSampleType::Depth,
                    wgpu::TextureViewDimension::D2,
                ),
            ],
        });
        let def_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("éclairage différé"),
            entries: &[
                tex_entry(
                    0,
                    wgpu::TextureSampleType::Depth,
                    wgpu::TextureViewDimension::D2,
                ),
                tex_entry(
                    1,
                    wgpu::TextureSampleType::Float { filterable: false },
                    wgpu::TextureViewDimension::D2,
                ),
                tex_entry(
                    2,
                    wgpu::TextureSampleType::Float { filterable: false },
                    wgpu::TextureViewDimension::D2,
                ),
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let max_texture = device.limits().max_texture_dimension_2d;
        let mut g = Self {
            header,
            lights_buf,
            tiles_buf,
            rects_buf,
            rects: Vec::new(),
            atlas: None,
            atlas_size: 0,
            tile_size: 0,
            quality: u8::MAX,
            max_texture,
            cmp_sampler,
            gobo_view: Self::gobo_texture(device, queue, &[]),
            gobo_sampler,
            bake_bgl,
            bake_group: None,
            jobs_buf,
            bake_uniform,
            jobs: Vec::new(),
            vl_bgl,
            comp_bgl,
            noise_view,
            noise_sampler,
            vl_targets: None,
            def_bgl,
            def_targets: None,
            lights: Vec::new(),
            settings: DynSettings::default(),
            rt_on: false,
            cache: ShadowCache::default(),
            bricks: HashMap::new(),
            frame: 0,
            stats: DynStats::default(),
            rebind: false,
            vl_active: false,
            gpu_lights: Vec::new(),
            tiles: vec![0; (TILES_X * TILES_Y * WORDS) as usize],
        };
        g.placeholder_atlas(device);
        g.rebind = false;
        g
    }

    /// Petit atlas tant qu'aucune lumière ne demande d'ombre (pas de mémoire vidéo pour
    /// rien) : une seule case, jamais lue.
    fn placeholder_atlas(&mut self, device: &wgpu::Device) {
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("ombres des lumières (vide)"),
            size: wgpu::Extent3d {
                width: 16,
                height: 16,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: ATLAS_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = tex.create_view(&Default::default());
        self.atlas = Some((tex, view));
        self.atlas_size = 16;
        self.tile_size = 16;
        self.quality = u8::MAX;
        self.cache.reset(0);
        self.rebind = true;
    }

    /// Tableau des gobos (au moins une couche blanche).
    fn gobo_texture(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        images: &[GoboImage],
    ) -> wgpu::TextureView {
        let layers = images.len().clamp(1, MAX_GOBOS) as u32;
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("gobos"),
            size: wgpu::Extent3d {
                width: GOBO_SIZE,
                height: GOBO_SIZE,
                depth_or_array_layers: layers,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        for layer in 0..layers {
            let data = match images.get(layer as usize) {
                Some(img) => resample_gobo(img),
                None => vec![255u8; (GOBO_SIZE * GOBO_SIZE) as usize],
            };
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &tex,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: 0,
                        y: 0,
                        z: layer,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                &data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(GOBO_SIZE),
                    rows_per_image: Some(GOBO_SIZE),
                },
                wgpu::Extent3d {
                    width: GOBO_SIZE,
                    height: GOBO_SIZE,
                    depth_or_array_layers: 1,
                },
            );
        }
        tex.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        })
    }

    pub fn set_gobos(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, images: &[GoboImage]) {
        self.gobo_view = Self::gobo_texture(device, queue, images);
        self.rebind = true;
    }

    /// Atlas des ombres à la qualité voulue (recréé s'il change : toutes les cartes sont à
    /// refaire).
    fn ensure_atlas(&mut self, device: &wgpu::Device, quality: u8) {
        let q = quality.min(SHADOW_QUALITY.len() as u8 - 1);
        if self.atlas.is_some() && self.quality == q {
            return;
        }
        let (mut size, tile) = SHADOW_QUALITY[q as usize];
        // Carte qui ne permet pas une si grande texture : moins de cases.
        size = size.min(self.max_texture.max(tile));
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("ombres des lumières"),
            size: wgpu::Extent3d {
                width: size,
                height: size,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: ATLAS_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = tex.create_view(&Default::default());
        self.atlas = Some((tex, view));
        self.atlas_size = size;
        self.tile_size = tile;
        self.quality = q;
        let per = (size / tile) as usize;
        self.cache.reset(per * per);
        self.rebind = true;
        log::info!(
            "ombres des lumières : qualité {} (atlas {size} px, {} cases de {tile} px, {} Mo)",
            SHADOW_QUALITY_NAMES[q as usize],
            per * per,
            size as u64 * size as u64 * 4 / (1024 * 1024)
        );
    }

    /// Liaisons 7 à 13 du groupe du monde.
    pub fn bind_entries(&self) -> [wgpu::BindGroupEntry<'_>; 7] {
        let atlas = &self.atlas.as_ref().expect("atlas des ombres").1;
        [
            wgpu::BindGroupEntry {
                binding: 7,
                resource: self.header.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 8,
                resource: self.lights_buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 9,
                resource: self.tiles_buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 10,
                resource: wgpu::BindingResource::TextureView(atlas),
            },
            wgpu::BindGroupEntry {
                binding: 11,
                resource: wgpu::BindingResource::Sampler(&self.cmp_sampler),
            },
            wgpu::BindGroupEntry {
                binding: 12,
                resource: wgpu::BindingResource::TextureView(&self.gobo_view),
            },
            wgpu::BindGroupEntry {
                binding: 13,
                resource: wgpu::BindingResource::Sampler(&self.gobo_sampler),
            },
        ]
    }

    /// Lumières et réglages de la prochaine image. Renvoie vrai si quelque chose a changé
    /// depuis l'image précédente (lumière déplacée, réglée, ajoutée ou retirée).
    /// Réglages en cours.
    pub fn settings(&self) -> &DynSettings {
        &self.settings
    }

    /// Ombres par lancer de rayons cette image (réglage, ombres des blocs, grille envoyée).
    pub fn rt_shadows_on(&self) -> bool {
        self.rt_on && self.active()
    }

    pub fn set(&mut self, lights: &[DynLight], settings: &DynSettings) -> bool {
        let lights = &lights[..lights.len().min(MAX_DYN_LIGHTS)];
        let changed = self.lights.as_slice() != lights || self.settings != *settings;
        if changed {
            self.lights.clear();
            self.lights.extend_from_slice(lights);
            self.settings = *settings;
        }
        changed
    }

    /// Des lumières à dessiner cette image ?
    pub fn active(&self) -> bool {
        !self.lights.is_empty()
    }

    /// Une lumière demande-t-elle des ombres des blocs (il faut alors la grille de blocs) ?
    pub fn wants_voxels(&self) -> bool {
        self.settings.shadows
            && self.settings.block_shadows
            && self.lights.iter().any(|l| l.shadows)
    }

    /// Nouvelle grille de blocs : les cartes des lumières près des blocs qui ont changé sont
    /// à refaire.
    pub fn on_voxels(&mut self, origin: [i32; 3], ids: &[u16]) {
        if self.cache.slots.is_empty() && !self.wants_voxels() {
            // Personne n'en a besoin : l'empreinte sera prise à la prochaine grille utile.
            self.bricks.clear();
            return;
        }
        let new = brick_hashes(origin, ids);
        if self.bricks.is_empty() {
            self.cache.invalidate_all();
        } else {
            let changed = changed_bricks(&self.bricks, &new);
            if !changed.is_empty() {
                self.cache.invalidate(&changed);
            }
        }
        self.bricks = new;
    }

    /// Prépare l'image : cartes d'ombre à calculer, lumières, tuiles, en-tête. Renvoie le
    /// nombre de cases à calculer.
    #[allow(clippy::too_many_arguments)]
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view_proj: Mat4,
        eye: Vec3,
        boxes: &[crate::ShadowBox],
        time: f32,
        voxels: Option<[i32; 3]>,
    ) -> u32 {
        self.frame += 1;
        let frame = self.frame;
        let st = self.settings;
        let block_maps = st.shadows && st.block_shadows && voxels.is_some();
        if block_maps && self.lights.iter().any(|l| l.shadows) {
            self.ensure_atlas(device, st.shadow_quality);
        }
        let block_maps = block_maps && self.quality != u8::MAX;
        // 1. Places dans l'atlas, dans l'ordre des lumières (les premières passent avant).
        let mut tiles_of: Vec<Option<u32>> = vec![None; self.lights.len()];
        let mut first = Vec::new();
        let mut again = Vec::new();
        let mut waiting = 0;
        if block_maps {
            if !st.shadow_cache {
                self.cache.invalidate_all();
            }
            for (i, l) in self.lights.iter().enumerate() {
                if !l.shadows {
                    continue;
                }
                match self.cache.request(l, frame) {
                    Some(s) => {
                        if !s.baked {
                            first.push(i);
                        } else if s.dirty {
                            again.push(i);
                        }
                    }
                    None => waiting += 1,
                }
            }
        }
        self.cache.sweep(frame, 120);
        // 2. Cases à calculer : premiers calculs d'abord (jusqu'à 4 fois le budget), puis les
        // cartes périmées dans la limite du budget (au moins une lumière par image).
        let budget = st.bake_budget.max(1) as usize;
        self.jobs.clear();
        let per = (self.atlas_size / self.tile_size.max(1)).max(1);
        for (list, limit) in [(&first, budget * 4), (&again, budget)] {
            let mut used = 0usize;
            for &i in list.iter() {
                let l = self.lights[i];
                let Some(s) = self.cache.slots.get_mut(&l.id) else {
                    continue;
                };
                let n = s.count as usize;
                if used > 0 && used + n > limit {
                    if !s.baked {
                        waiting += 1;
                    }
                    continue;
                }
                if self.jobs.len() + n > MAX_JOBS {
                    break;
                }
                used += n;
                let p = Vec3::from(l.pos);
                let range = l.range.max(0.05);
                let inside = if l.inside_block { 1.0 } else { 0.0 };
                let mut push = |tile: u32, (r, u, f): (Vec3, Vec3, Vec3), tan_half: f32| {
                    self.jobs.push((
                        tile,
                        BakeJob {
                            pos_range: [p.x, p.y, p.z, range],
                            fwd: [f.x, f.y, f.z, tan_half],
                            right: [r.x, r.y, r.z, inside],
                            up: [u.x, u.y, u.z, 0.0],
                        },
                    ));
                };
                if s.key.kind == 2 {
                    for f in 0..6 {
                        push(s.tile + f, face_basis(f), 1.0);
                    }
                } else {
                    push(s.tile, spot_basis(unit_dir(l.dir)), s.key.tan_half);
                }
                s.baked = true;
                s.dirty = false;
            }
        }
        // 3. Lumières pour le shader : case d'ombre seulement si la carte est prête (ou
        // calculée à cette image).
        let mut shadowed = 0;
        for (i, l) in self.lights.iter().enumerate() {
            if !block_maps || !l.shadows {
                continue;
            }
            if let Some(s) = self.cache.slots.get(&l.id) {
                if s.baked && s.seen == frame {
                    tiles_of[i] = Some(s.tile);
                    shadowed += 1;
                }
            }
        }
        let nboxes = boxes.len().min(MAX_BOXES);
        let box_shadows = st.shadows && st.entity_shadows && nboxes > 0;
        self.gpu_lights.clear();
        let atlas_px = self.atlas_size.max(1) as f32;
        let tile_uv = self.tile_size as f32 / atlas_px;
        for (i, l) in self.lights.iter().enumerate() {
            let mut mask = 0u64;
            if box_shadows && l.shadows {
                let (c, r) = (Vec3::from(l.pos), l.range);
                for (k, b) in boxes.iter().take(nboxes).enumerate() {
                    if box_touches_sphere(b.min, b.max, c, r) {
                        mask |= 1 << k;
                    }
                }
            }
            // Case de la carte d'ombre (même calcul que `dl_tile_rect`).
            let rect = tiles_of[i].map_or([0.0; 3], |t| {
                let (row, col) = (t / per, t % per);
                [col as f32 * tile_uv, row as f32 * tile_uv, tile_uv]
            });
            let sphere = boxes_sphere(&boxes[..nboxes], mask);
            self.gpu_lights
                .push(pack_light(l, tiles_of[i], mask, rect, sphere));
        }
        bin_tiles(&self.lights, view_proj, eye, &mut self.tiles);
        lit_rects(&self.tiles, &mut self.rects);
        // 4. En-tête.
        let vl_on = st.volumetric
            && st.vl_intensity > 0.0
            && self
                .lights
                .iter()
                .any(|l| l.beam > 0.0 && l.intensity > 0.0);
        let mut flags = 0;
        if st.vl_shadows {
            flags |= F_VL_SHADOWS;
        }
        if st.vl_noise {
            flags |= F_VL_NOISE;
        }
        if st.vl_dither_temporal {
            flags |= F_DITHER;
        }
        if st.shadows {
            flags |= F_SHADOWS;
        }
        if st.toon {
            flags |= F_TOON;
        }
        if st.specular > 0.0 {
            flags |= F_SPECULAR;
        }
        if vl_on {
            flags |= F_VOLUMETRIC;
        }
        if box_shadows {
            flags |= F_BOX_SHADOWS;
        }
        // Les rayons remplacent les cartes pour les ombres des blocs : sans elles, rien.
        self.rt_on = st.shadows && st.block_shadows && st.rt_shadows && voxels.is_some();
        if self.rt_on {
            flags |= F_RT_SHADOWS;
        }
        let mut box_x = [[0.0; 4]; MAX_BOXES];
        let mut box_y = [[0.0; 4]; MAX_BOXES];
        let mut box_z = [[0.0; 4]; MAX_BOXES];
        for (k, b) in boxes.iter().take(nboxes).enumerate() {
            [box_x[k], box_y[k], box_z[k]] = b.to_unit;
        }
        // Vitesses arrondies au quart : le bruit reprend sa place quand le temps repart à 0
        // (toutes les heures).
        let q4 = |v: f32| (v * 4.0).round() / 4.0;
        let header = GpuHeader {
            counts: [self.gpu_lights.len() as u32, TILES_X, TILES_Y, WORDS],
            vl_a: [
                st.vl_intensity,
                st.vl_max_dist.max(1.0),
                st.vl_tip_boost.max(0.0),
                st.vl_tip_radius.max(0.01),
            ],
            vl_b: [
                st.vl_noise_amount.clamp(0.0, 1.0),
                st.vl_noise_scale.max(0.05),
                q4(st.vl_noise_speed),
                time % 3600.0,
            ],
            vl_c: [
                st.vl_steps.clamp(1, 96),
                st.vl_shadow_stride.clamp(1, 8),
                st.vl_noise_stride.clamp(1, 8),
                flags,
            ],
            sh: [
                st.softness.max(0.0),
                self.atlas_size as f32,
                self.tile_size as f32,
                per as f32,
            ],
            surf: [
                st.intensity.max(0.0),
                st.specular.max(0.0),
                st.toon_bands.clamp(1, 16) as f32,
                st.toon_smooth.clamp(0.0, 1.0),
            ],
            misc: [
                (frame % 1024) as f32,
                q4(st.vl_noise_morph),
                nboxes as f32,
                0.0,
            ],
            vox: voxels.map_or([0; 4], |o| [o[0], o[1], o[2], 1]),
            box_x,
            box_y,
            box_z,
        };
        queue.write_buffer(&self.header, 0, bytemuck::bytes_of(&header));
        if !self.gpu_lights.is_empty() {
            queue.write_buffer(&self.lights_buf, 0, bytemuck::cast_slice(&self.gpu_lights));
            queue.write_buffer(&self.tiles_buf, 0, bytemuck::cast_slice(&self.tiles));
        }
        if !self.rects.is_empty() {
            queue.write_buffer(&self.rects_buf, 0, bytemuck::cast_slice(&self.rects));
        }
        // 5. Travaux du calcul des ombres.
        if !self.jobs.is_empty() {
            let jobs: Vec<BakeJob> = self.jobs.iter().map(|j| j.1).collect();
            queue.write_buffer(&self.jobs_buf, 0, bytemuck::cast_slice(&jobs));
            let o = voxels.unwrap_or_default();
            queue.write_buffer(
                &self.bake_uniform,
                0,
                bytemuck::bytes_of(&BakeUniform {
                    origin: [o[0], o[1], o[2], 0],
                }),
            );
        }
        self.vl_active = vl_on && !self.gpu_lights.is_empty();
        self.stats = DynStats {
            lights: self.lights.len(),
            shadowed,
            waiting,
            tiles_used: self.cache.used.iter().filter(|u| **u).count(),
            tiles_total: self.cache.used.len(),
            baked: self.jobs.len(),
            volumetric: self.vl_active,
            // Rempli par le moteur de rendu (compilation de leurs shaders).
            state: DynState::Off,
        };
        self.jobs.len() as u32
    }

    /// Groupe du calcul des ombres (grille de blocs, palette, textures des blocs).
    pub fn bind_bake(
        &mut self,
        device: &wgpu::Device,
        voxels: &crate::rt::Voxels,
        blocks: &wgpu::TextureView,
        sampler: &wgpu::Sampler,
    ) {
        self.bake_group = Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ombres : calcul"),
            layout: &self.bake_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.jobs_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: self.bake_uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&voxels.view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&voxels.palette_view),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(blocks),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
            ],
        }));
    }

    /// Calcule les cases d'ombre préparées par `prepare` (`pipeline` : voir `light_pipes`).
    pub fn encode_bake(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        pipeline: &wgpu::RenderPipeline,
        timestamps: Option<wgpu::RenderPassTimestampWrites<'_>>,
    ) {
        let (Some(group), Some((_, atlas))) = (&self.bake_group, &self.atlas) else {
            return;
        };
        if self.jobs.is_empty() {
            return;
        }
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("ombres des lumières"),
            color_attachments: &[],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: atlas,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: timestamps,
            occlusion_query_set: None,
        });
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, group, &[]);
        let per = (self.atlas_size / self.tile_size.max(1)).max(1);
        let t = self.tile_size;
        for (k, (tile, _)) in self.jobs.iter().enumerate() {
            let (x, y) = ((tile % per) * t, (tile / per) * t);
            pass.set_viewport(x as f32, y as f32, t as f32, t as f32, 0.0, 1.0);
            pass.set_scissor_rect(x, y, t, t);
            pass.draw(0..3, k as u32..k as u32 + 1);
        }
    }

    /// Lumière volumétrique à ajouter à cette image ?
    pub fn volumetric_active(&self) -> bool {
        self.vl_active
    }

    /// Des cartes d'ombre à calculer à cette image ?
    pub fn bake_pending(&self) -> bool {
        !self.jobs.is_empty() && self.bake_group.is_some() && self.atlas.is_some()
    }

    /// Éclairage différé : ajoute la lumière des lumières dynamiques à `color` (image du
    /// monde opaque), d'après la profondeur de cette image et ce que les opaques ont laissé
    /// (`gbuf` : albédo, surface). `generation` change quand ces images sont recréées.
    #[allow(clippy::too_many_arguments)]
    pub fn encode_deferred(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        world_group: &wgpu::BindGroup,
        depths: &[wgpu::TextureView; 2],
        depth_index: usize,
        gbuf: &[wgpu::TextureView; 2],
        generation: u64,
        color: &wgpu::TextureView,
        pipeline: &wgpu::RenderPipeline,
        timestamps: Option<wgpu::RenderPassTimestampWrites<'_>>,
    ) {
        if self
            .def_targets
            .as_ref()
            .is_none_or(|t| t.generation != generation)
        {
            let groups = std::array::from_fn(|i| {
                device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("éclairage différé"),
                    layout: &self.def_bgl,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(&depths[i]),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::TextureView(&gbuf[0]),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: wgpu::BindingResource::TextureView(&gbuf[1]),
                        },
                        wgpu::BindGroupEntry {
                            binding: 3,
                            resource: self.rects_buf.as_entire_binding(),
                        },
                    ],
                })
            });
            self.def_targets = Some(DeferredTargets { generation, groups });
        }
        let Some(t) = &self.def_targets else {
            return;
        };
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("lumières (éclairage différé)"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: color,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: timestamps,
            occlusion_query_set: None,
        });
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, world_group, &[]);
        pass.set_bind_group(1, &t.groups[depth_index], &[]);
        // Seulement les cases éclairées (deux triangles chacune).
        pass.draw(0..6, 0..self.rects.len() as u32);
    }

    /// Nombre de rectangles de cases éclairées à l'écran (0 : aucune lumière en vue, les
    /// passes des lumières n'ont rien à faire).
    pub fn lit_rects(&self) -> usize {
        self.rects.len()
    }

    /// Passe volumétrique (demi-résolution) puis recomposition dans `color` (image du monde).
    /// `depths` : les deux profondeurs du TAA (celle de cette image : `depth_index`) ;
    /// `generation` change quand elles sont recréées.
    #[allow(clippy::too_many_arguments)]
    pub fn encode_volumetric(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        world_group: &wgpu::BindGroup,
        depths: &[wgpu::TextureView; 2],
        depth_index: usize,
        generation: u64,
        size: (u32, u32),
        color: &wgpu::TextureView,
        pipes: &VlPipes,
        timestamps: (
            Option<wgpu::RenderPassTimestampWrites<'_>>,
            Option<wgpu::RenderPassTimestampWrites<'_>>,
        ),
    ) {
        if !self.volumetric_active() {
            return;
        }
        let half = (size.0.div_ceil(2), size.1.div_ceil(2));
        let key = (half.0, half.1, generation);
        if self.vl_targets.as_ref().is_none_or(|t| t.key != key) {
            let tex = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("volumétrique"),
                size: wgpu::Extent3d {
                    width: half.0.max(1),
                    height: half.1.max(1),
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: VL_FORMAT,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            let view = tex.create_view(&Default::default());
            let vl_groups = std::array::from_fn(|i| {
                device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("volumétrique"),
                    layout: &self.vl_bgl,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(&depths[i]),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::TextureView(&self.noise_view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: wgpu::BindingResource::Sampler(&self.noise_sampler),
                        },
                        wgpu::BindGroupEntry {
                            binding: 3,
                            resource: self.rects_buf.as_entire_binding(),
                        },
                    ],
                })
            });
            let comp_groups = std::array::from_fn(|i| {
                device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("volumétrique : recomposition"),
                    layout: &self.comp_bgl,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(&view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::TextureView(&depths[i]),
                        },
                    ],
                })
            });
            self.vl_targets = Some(VlTargets {
                key,
                view,
                vl_groups,
                comp_groups,
            });
        }
        let Some(t) = &self.vl_targets else {
            return;
        };
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("lumière volumétrique"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &t.view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: timestamps.0,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&pipes.march);
            pass.set_bind_group(0, world_group, &[]);
            pass.set_bind_group(1, &t.vl_groups[depth_index], &[]);
            pass.draw(0..6, 0..self.rects.len() as u32);
        }
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("lumière volumétrique : recomposition"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: color,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: timestamps.1,
            occlusion_query_set: None,
        });
        pass.set_pipeline(&pipes.composite);
        pass.set_bind_group(0, &t.comp_groups[depth_index], &[]);
        pass.draw(0..3, 0..1);
    }

    /// Mémoire vidéo des lumières (atlas des ombres surtout), en octets.
    pub fn gpu_bytes(&self) -> u64 {
        self.atlas_size as u64 * self.atlas_size as u64 * 4
            + (MAX_DYN_LIGHTS * std::mem::size_of::<GpuLight>()) as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spot(id: u64, pos: [f32; 3], dir: [f32; 3], range: f32, outer: f32) -> DynLight {
        DynLight {
            id,
            kind: DynKind::Spot,
            pos,
            dir,
            range,
            outer_deg: outer,
            inner_deg: outer * 0.7,
            ..Default::default()
        }
    }

    #[test]
    fn gpu_layouts_match_the_shader() {
        use wgpu::naga;
        let src = with_dynlights(include_str!("shaders/volumetric.wgsl"));
        let module = naga::front::wgsl::parse_str(&src).expect("WGSL");
        let size = |name: &str| {
            let t = module
                .types
                .iter()
                .find(|(_, t)| t.name.as_deref() == Some(name))
                .unwrap_or_else(|| panic!("{name}"));
            t.1.inner.size(module.to_ctx()) as usize
        };
        assert_eq!(size("DynLight"), std::mem::size_of::<GpuLight>());
        assert_eq!(size("DynHeader"), std::mem::size_of::<GpuHeader>());
        let bake = naga::front::wgsl::parse_str(include_str!("shaders/shadow_bake.wgsl")).unwrap();
        let job = bake
            .types
            .iter()
            .find(|(_, t)| t.name.as_deref() == Some("BakeJob"))
            .unwrap();
        assert_eq!(
            job.1.inner.size(bake.to_ctx()) as usize,
            std::mem::size_of::<BakeJob>()
        );
    }

    #[test]
    fn cube_faces_cover_every_direction_and_map_back_to_themselves() {
        // Chaque direction tombe dans la face qui la regarde, au milieu de sa case : le
        // calcul (repère de la face) et la lecture (choix de la face) sont d'accord.
        for i in 0..2000 {
            let a = i as f32 * 0.7548777;
            let b = i as f32 * 0.5698403;
            let d = Vec3::new(a.sin() * b.cos(), (a * 1.3).cos(), b.sin() + 0.01).normalize();
            let f = cube_face(d);
            let (r, u, fw) = face_basis(f);
            let z = fw.dot(d);
            assert!(z > 0.0, "{d:?} face {f}");
            let ndc = Vec3::new(r.dot(d), u.dot(d), 0.0) / z;
            assert!(
                ndc.x.abs() <= 1.0 + 1e-5 && ndc.y.abs() <= 1.0 + 1e-5,
                "{d:?} {ndc:?}"
            );
            // Direction refaite depuis la case, comme le calcul de la carte.
            let back = (fw + r * ndc.x + u * ndc.y).normalize();
            assert!(back.distance(d) < 1e-4);
        }
        // Repères orthonormés (droite × haut = ± avant).
        for f in 0..6 {
            let (r, u, fw) = face_basis(f);
            assert!(r.dot(u).abs() < 1e-6 && r.dot(fw).abs() < 1e-6 && u.dot(fw).abs() < 1e-6);
            assert!((r.cross(u).abs() - fw.abs()).length() < 1e-6);
        }
    }

    #[test]
    fn a_spot_cone_fits_in_its_shadow_map_and_its_bounding_sphere() {
        for outer in [5.0f32, 35.0, 90.0, 119.0] {
            let l = spot(1, [3.0, 70.0, -2.0], [0.3, -1.0, 0.2], 12.0, outer);
            let (kind, tan_half) = shadow_shape(&l);
            assert_eq!(kind, 1);
            let d = unit_dir(l.dir);
            let (s, u, f) = spot_basis(d);
            let (c, r) = bounding_sphere(&l);
            let half = (outer * 0.5).to_radians();
            for k in 0..64 {
                let phi = k as f32 / 64.0 * std::f32::consts::TAU;
                // Bord du cône, au bout de la portée.
                let dir =
                    (f * half.cos() + (s * phi.cos() + u * phi.sin()) * half.sin()).normalize();
                let ndc = Vec3::new(s.dot(dir), u.dot(dir), 0.0) / (f.dot(dir) * tan_half);
                assert!(ndc.x.abs() < 1.0 && ndc.y.abs() < 1.0, "{outer} {ndc:?}");
                let tip = Vec3::from(l.pos) + dir * l.range;
                assert!(
                    tip.distance(c) <= r + 1e-3,
                    "{outer} : bord hors de la sphère"
                );
            }
            assert!(Vec3::from(l.pos).distance(c) <= r + 1e-3);
        }
        // Très large : ombres en cube.
        assert_eq!(
            shadow_shape(&spot(1, [0.0; 3], [0.0, -1.0, 0.0], 8.0, 150.0)).0,
            2
        );
    }

    #[test]
    fn lights_only_touch_the_screen_tiles_they_cover() {
        let eye = Vec3::new(0.0, 70.0, 0.0);
        let vp = Mat4::perspective_infinite_reverse_rh(1.2, 16.0 / 9.0, 0.05)
            * Mat4::look_to_rh(eye, Vec3::NEG_Z, Vec3::Y);
        let lights = [
            // Devant, à droite.
            DynLight {
                id: 1,
                pos: [6.0, 70.0, -20.0],
                range: 2.0,
                ..Default::default()
            },
            // Derrière la caméra : hors champ.
            DynLight {
                id: 2,
                pos: [0.0, 70.0, 30.0],
                range: 3.0,
                ..Default::default()
            },
            // Caméra dedans : tout l'écran.
            DynLight {
                id: 3,
                pos: [0.5, 70.0, 0.5],
                range: 4.0,
                ..Default::default()
            },
        ];
        let mut tiles = vec![0u32; (TILES_X * TILES_Y * WORDS) as usize];
        bin_tiles(&lights, vp, eye, &mut tiles);
        let has = |tx: u32, ty: u32, i: u32| {
            tiles[((ty * TILES_X + tx) * WORDS + i / 32) as usize] & (1 << (i % 32)) != 0
        };
        let count = |i: u32| {
            (0..TILES_Y)
                .flat_map(|ty| (0..TILES_X).map(move |tx| (tx, ty)))
                .filter(|&(tx, ty)| has(tx, ty, i))
                .count()
        };
        // La lumière 0 : quelques tuiles à droite du centre, à mi-hauteur.
        let n0 = count(0);
        assert!(n0 > 0 && n0 < 60, "{n0}");
        assert!(has(TILES_X / 2 + 3, TILES_Y / 2, 0));
        assert!(!has(2, TILES_Y / 2, 0));
        assert_eq!(count(1), 0);
        assert_eq!(count(2), (TILES_X * TILES_Y) as usize);
        // Le point projeté est bien dans une tuile marquée (même convention que le shader :
        // y vers le bas).
        let c = vp * Vec4::new(6.0, 70.0, -20.0, 1.0);
        let (u, v) = (c.x / c.w * 0.5 + 0.5, 0.5 - c.y / c.w * 0.5);
        assert!(has(
            (u * TILES_X as f32) as u32,
            (v * TILES_Y as f32) as u32,
            0
        ));
    }

    #[test]
    fn the_shadow_atlas_reuses_places_and_rebakes_only_what_changed() {
        let mut c = ShadowCache::default();
        c.reset(16);
        let mut point = DynLight {
            id: 7,
            pos: [1.0, 2.0, 3.0],
            ..Default::default()
        };
        let s = spot(8, [10.0, 70.0, 0.0], [0.0, -1.0, 0.0], 12.0, 40.0);
        // Premier calcul : 6 cases pour le cube, 1 pour le projecteur.
        let t_point = c
            .request(&point, 1)
            .map(|s| (s.tile, s.count, s.dirty))
            .unwrap();
        assert_eq!((t_point.1, t_point.2), (6, true));
        let t_spot = c.request(&s, 1).map(|s| (s.tile, s.count)).unwrap();
        assert_eq!(t_spot.1, 1);
        for sl in c.slots.values_mut() {
            sl.baked = true;
            sl.dirty = false;
        }
        // Rien n'a bougé : rien à refaire, même place.
        assert_eq!(
            c.request(&point, 2).map(|s| (s.tile, s.dirty)),
            Some((t_point.0, false))
        );
        // La lumière bouge : à refaire, même place.
        point.pos[0] += 0.5;
        assert_eq!(
            c.request(&point, 3).map(|s| (s.tile, s.dirty)),
            Some((t_point.0, true))
        );
        c.slots.get_mut(&7).unwrap().dirty = false;
        // Un bloc change loin d'elle : rien ; près d'elle : à refaire.
        c.invalidate(&[([100, 60, 100], [108, 68, 108])]);
        assert!(!c.slots[&7].dirty);
        assert!(!c.slots[&8].dirty);
        c.invalidate(&[([0, 0, 0], [8, 8, 8])]);
        assert!(c.slots[&7].dirty);
        assert!(!c.slots[&8].dirty, "le projecteur est loin");
        // Atlas plein : la lumière qui n'a rien demandé depuis le plus longtemps laisse sa
        // place.
        let others: Vec<DynLight> = (0..3)
            .map(|k| DynLight {
                id: 100 + k,
                ..Default::default()
            })
            .collect();
        assert!(c.request(&others[0], 4).is_some()); // 6 + 1 + 6 = 13 cases
        assert!(
            c.request(&others[1], 5).is_some(),
            "place reprise à une ancienne"
        );
        assert!(c.slots.len() <= 3);
        let used = c.used.iter().filter(|u| **u).count();
        let want: u32 = c.slots.values().map(|s| s.count).sum();
        assert_eq!(
            used as u32, want,
            "cases occupées = cases des lumières gardées"
        );
        c.sweep(1000, 120);
        assert!(c.slots.is_empty());
        assert!(c.used.iter().all(|u| !u));
    }

    #[test]
    fn a_block_change_marks_only_its_brick() {
        use crate::rt::VOX;
        let n = (VOX[0] * VOX[1] * VOX[2]) as usize;
        let origin = [-64, 32, 16];
        let mut ids = vec![0u16; n];
        let a = brick_hashes(origin, &ids);
        assert_eq!(a.len(), (VOX[0] / 8 * VOX[1] / 8 * VOX[2] / 8) as usize);
        // Un bloc posé en (x, y, z) de la grille ; la lumière du ciel seule ne compte pas.
        let (x, y, z) = (13usize, 40usize, 100usize);
        ids[x + VOX[0] as usize * (y + VOX[1] as usize * z)] = 0x0005;
        ids[1 + VOX[0] as usize * (2 + VOX[1] as usize * 3)] = 0xf000;
        let b = brick_hashes(origin, &ids);
        let ch = changed_bricks(&a, &b);
        assert_eq!(ch.len(), 1);
        let (lo, hi) = ch[0];
        let w = [
            origin[0] + x as i32,
            origin[1] + y as i32,
            origin[2] + z as i32,
        ];
        assert!(
            (0..3).all(|k| lo[k] <= w[k] && w[k] < hi[k]),
            "{lo:?} {hi:?} {w:?}"
        );
        // La grille avance de 8 blocs : les cubes qui entrent et ceux qui sortent changent.
        let c = brick_hashes([origin[0] + 8, origin[1], origin[2]], &ids);
        let moved = changed_bricks(&b, &c);
        assert!(moved.len() >= (VOX[1] / 8 * VOX[2] / 8 * 2) as usize);
    }

    #[test]
    fn packed_lights_keep_their_cone_mask_and_gobo() {
        let mut l = spot(1, [1.0, 2.0, 3.0], [0.0, 0.0, -2.0], 9.0, 60.0);
        l.inner_deg = 80.0; // plus large que l'extérieur : ramené
        l.blocks_only = true;
        l.gobo = Some(3);
        l.gobo_invert = true;
        l.color = [1.0, 0.5, 0.0];
        l.intensity = 2.0;
        let g = pack_light(
            &l,
            Some(5),
            0b101,
            [0.25, 0.5, 0.0625],
            [1.0, 2.0, 3.0, 4.0],
        );
        assert_eq!(g.dir_type, [0.0, 0.0, -1.0, 1.0]);
        assert!((g.cone[0] - 30f32.to_radians().cos()).abs() < 1e-6);
        assert!((g.cone[1] - g.cone[0]).abs() < 1e-6);
        assert_eq!(g.cone[2], 2.0);
        assert_eq!(g.cookie, [3.0, 0.0, 1.0, 1.0]);
        assert_eq!(g.color, [2.0, 1.0, 0.0, 2.0]);
        assert_eq!((g.shadow[0], g.vol[3]), (1.0, 5.0));
        assert_eq!(g.boxes[0], 0b101);
        // Repère du projecteur, case et sphère : passés tels quels au shader ; le repère
        // est celui des cartes d'ombre (`spot_basis`).
        let (r, u, _) = spot_basis(Vec3::new(0.0, 0.0, -1.0));
        assert_eq!(&g.right[..3], &r.to_array());
        assert_eq!(&g.up[..3], &u.to_array());
        assert!((g.right[3] - 1.0 / 30f32.to_radians().tan()).abs() < 1e-4);
        assert_eq!((g.up[3], g.rect[3]), (1.0, 0.0));
        assert_eq!(&g.rect[..3], &[0.25, 0.5, 0.0625]);
        assert_eq!(g.box_sphere, [1.0, 2.0, 3.0, 4.0]);
        // Une lumière ponctuelle n'a ni cône ni gobo ; sans carte prête : pas d'ombre.
        let p = pack_light(
            &DynLight {
                gobo: Some(2),
                ..Default::default()
            },
            None,
            0,
            [0.0; 3],
            [0.0, 0.0, 0.0, -1.0],
        );
        assert_eq!(p.cookie[0], -1.0);
        assert_eq!((p.shadow[0], p.vol[3]), (0.0, -1.0));
        assert_eq!(p.dir_type[3], 0.0);
    }

    #[test]
    fn the_box_sphere_holds_every_box_of_the_mask() {
        let b = |a: [f32; 3], c: [f32; 3]| crate::ShadowBox {
            min: a,
            max: c,
            to_unit: [[0.0; 4]; 3],
            sunlit: true,
        };
        let boxes = [
            b([0.0, 0.0, 0.0], [1.0, 2.0, 1.0]),
            b([10.0, 0.0, 0.0], [11.0, 1.0, 1.0]),
            b([0.0, 2.0, 0.0], [1.0, 3.0, 1.0]),
        ];
        // Boîtes 0 et 2 : centre au milieu, chaque coin dedans.
        let s = boxes_sphere(&boxes, 0b101);
        let c = Vec3::new(s[0], s[1], s[2]);
        for k in [0, 2] {
            for corner in [boxes[k].min, boxes[k].max] {
                assert!(c.distance(Vec3::from(corner)) <= s[3], "{k}");
            }
        }
        assert!(s[3] < 2.0);
        assert_eq!(boxes_sphere(&boxes, 0)[3], -1.0);
    }

    #[test]
    fn gobos_are_resampled_to_the_texture_size() {
        // Damier 2 × 2 : chaque quart de l'image garde sa valeur.
        let img = GoboImage {
            width: 2,
            height: 2,
            luma: vec![0, 255, 255, 0],
        };
        let out = resample_gobo(&img);
        let s = GOBO_SIZE as usize;
        assert_eq!(out[0], 0);
        assert_eq!(out[s - 1], 255);
        assert_eq!(out[(s - 1) * s], 255);
        assert_eq!(out[s * s - 1], 0);
        // Image vide : blanc (laisse tout passer).
        assert!(resample_gobo(&GoboImage {
            width: 0,
            height: 0,
            luma: vec![]
        })
        .iter()
        .all(|&v| v == 255));
    }

    #[test]
    fn the_noise_repeats_and_varies() {
        let n = value_noise(NOISE_SIZE);
        assert_eq!(n.len(), (NOISE_SIZE * NOISE_SIZE * NOISE_SIZE) as usize);
        let mean = n.iter().map(|&v| v as f32).sum::<f32>() / n.len() as f32;
        assert!((80.0..175.0).contains(&mean), "{mean}");
        let s = NOISE_SIZE as usize;
        // Raccord : la dernière colonne ressemble à la première (pas de couture).
        let mut seam = 0.0;
        for y in 0..s {
            seam += (n[(s - 1) + s * y] as f32 - n[s * y] as f32).abs();
        }
        assert!(seam / (s as f32) < 40.0);
    }
}
