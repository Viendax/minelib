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
/// Lumière volumétrique en demi- ou quart de résolution : deux couches (jusqu'à la surface la
/// plus proche et jusqu'à la plus lointaine des pixels couverts), chacune trois demi-flottants
/// de lumière et un de profondeur.
const VL_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba32Uint;
/// (v223) Rayons repris d'une image à l'autre (voir `fs_temporal`) : marge préparée autour de
/// l'écran, de chaque côté (part de l'image des rayons) ; ce que l'image de la marche y
/// trouve sans l'image du monde est cherché dans la grille de blocs.
const VL_MARGIN: f32 = 0.06;
/// La marge des tuiles de la marge (un peu plus large que `VL_MARGIN` : arrondis).
const VL_MARGIN_BIN: f32 = 0.09;
/// Part de la nouvelle image quand l'historique est bon (le reste : l'historique).
const VL_NEW: f32 = 0.12;
/// Caméra qui saute plus loin que ça d'une image à l'autre (téléportation) : historique oublié.
const VL_JUMP: f32 = 8.0;

/// (v223) Image des rayons effacée puis écrite par une passe.
fn vl_attach(view: &wgpu::TextureView) -> Option<wgpu::RenderPassColorAttachment<'_>> {
    Some(wgpu::RenderPassColorAttachment {
        view,
        resolve_target: None,
        ops: wgpu::Operations {
            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
            store: wgpu::StoreOp::Store,
        },
    })
}

/// (v223) `VlFrame` des shaders des rayons (volumetric.wgsl, vl_composite.wgsl).
#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable, Default)]
struct VlFrameGpu {
    cur_rel: [[f32; 4]; 4],
    prev_rel: [[f32; 4]; 4],
    delta: [f32; 4],
    margin: [f32; 4],
    size: [f32; 4],
    jitter: [f32; 4],
}

/// (v223) Décalage de l'échantillon de la marche dans son carré, image `n` (suite de Halton
/// en base 2 et 3, huit positions) : de −0,5 à 0,5.
fn vl_jitter(n: u64) -> (f32, f32) {
    let halton = |mut i: u64, b: u64| {
        let (mut f, mut r) = (1.0f32, 0.0f32);
        while i > 0 {
            f /= b as f32;
            r += f * (i % b) as f32;
            i /= b;
        }
        r
    };
    let i = n % 8 + 1;
    (halton(i, 2) - 0.5, halton(i, 3) - 0.5)
}

/// (v223) Marge de l'image des rayons (pixels de cette image) pour un écran de `on` pixels
/// des rayons : de chaque côté.
fn vl_margin(on: (u32, u32)) -> (u32, u32) {
    (
        (on.0 as f32 * VL_MARGIN).ceil() as u32,
        (on.1 as f32 * VL_MARGIN).ceil() as u32,
    )
}
const ATLAS_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
/// (v220) Atlas des ombres des joueurs (voir `EntJob`) : au plus ce côté (pixels). En
/// qualité Haute et Ultra, la moitié de l'atlas des blocs (mêmes cases, en proportion).
const ENT_ATLAS_MAX: u32 = 4096;
/// Joueurs au plus dans l'atlas des joueurs à une image, toutes lumières comprises.
pub(crate) const MAX_ENT_CASTERS: usize = 4096;
/// Format de l'atlas des joueurs (la passe des joueurs y écrit sa profondeur).
pub(crate) const ENT_ATLAS_FORMAT: wgpu::TextureFormat = ATLAS_FORMAT;

/// (v220) Une case de l'atlas des ombres des joueurs à dessiner à cette image : sa place
/// (pixels : x, y, côté), le repère de la lumière pour elle (le même que celui de sa carte des
/// blocs), et les joueurs à y dessiner (place dans `DynGpu::ent_casters`).
#[derive(Clone, Debug)]
pub(crate) struct EntJob {
    pub rect: [u32; 3],
    pub frame: EntFrame,
    pub casters: std::ops::Range<u32>,
}

/// Uniformes d'une case de l'atlas des joueurs (`LightJob` de players.wgsl).
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub(crate) struct EntFrame {
    pub pos_range: [f32; 4],
    pub right: [f32; 4],
    pub up: [f32; 4],
    pub fwd: [f32; 4],
}

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
    /// (v201) Lumière tenue (lampe torche) : les morceaux de celui qui la tient
    /// (`ShadowBox::owner`) ne lui font pas d'ombre ; 0 : à personne. (v202) En bits : sont
    /// écartés les morceaux qui ont un de ses bits. Non nul : lumière portée (elle bouge sans
    /// cesse) : ses bords d'ombre restent en pente douce, sa carte est refaite avant celles
    /// des autres.
    pub owner: u32,
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
            owner: 0,
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
    /// Douceur des ombres par défaut (taille de la source, blocs).
    pub softness: f32,
    /// Cases d'ombre recalculées au plus par image (les premiers calculs passent avant).
    pub bake_budget: u32,
    /// Lumière volumétrique.
    pub volumetric: bool,
    pub vl_intensity: f32,
    pub vl_steps: u32,
    pub vl_max_dist: f32,
    /// Résolution des rayons : pixels de l'image par pixel calculé, de côté (2 : moitié,
    /// 4 : quart, quatre fois moins de calcul ; la recomposition suit la profondeur).
    pub vl_scale: u32,
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
    /// Éclairage différé en demi-résolution, exact aux bords (`dyn_deferred.wgsl`) :
    /// décoché, chaque pixel est calculé exactement.
    pub half_lighting: bool,
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
            softness: 0.1,
            bake_budget: 12,
            volumetric: true,
            vl_intensity: 1.0,
            vl_steps: 48,
            vl_scale: 4,
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
            half_lighting: true,
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
    /// Boîtes exactes des joueurs : lignes de la matrice monde → cube unité.
    box_x: [[f32; 4]; MAX_BOXES],
    box_y: [[f32; 4]; MAX_BOXES],
    box_z: [[f32; 4]; MAX_BOXES],
    /// Sphère (centre, rayon) de chaque boîte, élargie : premier tri dans le shader.
    box_s: [[f32; 4]; MAX_BOXES],
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
/// Éclairage différé en demi-résolution, exact aux bords.
const F_HALF_LIGHT: u32 = 256;

/// Repère d'un projecteur dirigé selon `d` : (droite, haut, avant) — le même que
/// `dl_spot_basis` dans dynlights.wgsl. La droite est horizontale, de côté par rapport au
/// cap de `d` : elle tourne avec lui, sans à-coup, jusqu'à la verticale (v222 ; avant, au-delà
/// de 0,99 de pente, elle passait d'un coup à un repère fixe : un gobo tournait brusquement).
/// L'éditeur et la caméra s'arrêtent à 89,9° : seul un `d` tout à fait vertical, sans cap,
/// prend le repère fixe.
pub fn spot_basis(d: Vec3) -> (Vec3, Vec3, Vec3) {
    let flat = (d.x * d.x + d.z * d.z).sqrt();
    let up = if flat < 1e-6 { Vec3::Z } else { Vec3::Y };
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
    // Depuis la caméra : précis même loin de l'origine (voir `view_ray`).
    let rel = crate::eye_relative(view_proj);
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    let mut behind = 0;
    for k in 0..8 {
        let o = Vec3::new(
            if k & 1 == 0 { -radius } else { radius },
            if k & 2 == 0 { -radius } else { radius },
            if k & 4 == 0 { -radius } else { radius },
        );
        let c = rel * Vec4::from((center + o - eye, 1.0));
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
/// (v223) Une tuile de plus, après celles de l'écran : les lumières posées qui ont des rayons
/// et touchent la marge préparée autour de l'écran (voir `VL_MARGIN`).
pub fn bin_tiles(lights: &[DynLight], view_proj: Mat4, eye: Vec3, tiles: &mut [u32]) {
    tiles.fill(0);
    let wide = Mat4::from_scale(Vec3::new(
        1.0 / (1.0 + 2.0 * VL_MARGIN_BIN),
        1.0 / (1.0 + 2.0 * VL_MARGIN_BIN),
        1.0,
    )) * view_proj;
    let margin = (TILES_X * TILES_Y * WORDS) as usize;
    for (i, l) in lights.iter().enumerate().take(MAX_DYN_LIGHTS) {
        let (c, r) = bounding_sphere(l);
        let (word, bit) = (i / 32, 1u32 << (i % 32));
        if l.owner == 0
            && l.beam > 0.0
            && tiles.len() > margin + word
            && screen_rect(wide, eye, c, r).is_some()
        {
            tiles[margin + word] |= bit;
        }
        let Some([u0, v0, u1, v1]) = screen_rect(view_proj, eye, c, r) else {
            continue;
        };
        let tx0 = ((u0 * TILES_X as f32).floor() as u32).min(TILES_X - 1);
        let tx1 = ((u1 * TILES_X as f32).floor() as u32).min(TILES_X - 1);
        let ty0 = ((v0 * TILES_Y as f32).floor() as u32).min(TILES_Y - 1);
        let ty1 = ((v1 * TILES_Y as f32).floor() as u32).min(TILES_Y - 1);
        for ty in ty0..=ty1 {
            for tx in tx0..=tx1 {
                tiles[((ty * TILES_X + tx) * WORDS) as usize + word] |= bit;
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

/// Morceaux de joueurs (bits de `boxes`) qui peuvent faire de l'ombre à `l` : ceux que sa
/// lumière atteint. (v201) Pas ceux de celui qui la tient : sa lampe est contre lui, parfois
/// dans sa boîte (tout serait alors dans l'ombre). (v202) Propriétaires en bits : une
/// lumière écarte les morceaux qui ont un de ses bits (la lampe torche, tout le joueur ;
/// la torche tenue, seulement le morceau tout contre sa flamme).
fn box_mask(l: &DynLight, boxes: &[crate::ShadowBox]) -> u64 {
    let (c, r) = (Vec3::from(l.pos), l.range);
    let mut mask = 0u64;
    for (k, b) in boxes.iter().enumerate() {
        let holder = l.owner & b.owner != 0;
        if !holder && box_touches_sphere(b.min, b.max, c, r) {
            mask |= 1 << k;
        }
    }
    mask
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
    soft: bool,
    ent: bool,
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
        // (v202) w = 1 : lumière portée (elle bouge sans cesse avec celui qui la tient) :
        // bords d'ombre en pente douce, sans le resserrement, pour qu'ils glissent au lieu de
        // sauter d'un texel de la carte à l'autre.
        shadow: [
            sk,
            tan_half,
            if soft { 0.0 } else { 1.0 },
            if l.owner != 0 { 1.0 } else { 0.0 },
        ],
        // (v220) w = 1 : des joueurs à sa portée dans l'atlas des ombres des joueurs.
        boxes: [
            boxes as u32,
            (boxes >> 32) as u32,
            box_cone(Vec3::from(l.pos), box_sphere).1.to_bits(),
            ent as u32,
        ],
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
        box_sphere: box_cone(Vec3::from(l.pos), box_sphere).0,
    }
}

/// Le cône, vu de la lumière `lp`, qui contient la sphère `s` (centre, rayon ; rayon < 0 :
/// aucune boîte) : axe et cosinus du demi-angle (−2 : la lumière est dans la sphère), et la
/// distance en deçà de laquelle aucun point ne peut être caché. Le shader n'essaie les
/// boîtes que pour un point dans ce cône et au-delà de cette distance (deux produits, avec
/// la direction et la distance de la lumière qu'il a déjà). Élargi pour le point essayé, à
/// 2 centièmes de la surface, et pour les arrondis (plus loin de l'origine, plus large) :
/// jamais une ombre écartée par erreur.
fn box_cone(lp: Vec3, s: [f32; 4]) -> ([f32; 4], f32) {
    if s[3] < 0.0 {
        // Aucune boîte : aucune direction n'a un cosinus de 2.
        return ([0.0, 0.0, 0.0, 2.0], f32::MAX);
    }
    let c = Vec3::new(s[0], s[1], s[2]);
    let r = s[3] + 0.05 + 2e-6 * (lp.abs().max_element() + c.abs().max_element());
    let to = c - lp;
    let dc = to.length();
    if dc <= r * 1.01 {
        return ([0.0, 0.0, 0.0, -2.0], -1.0);
    }
    let axis = to / dc;
    let cos = (1.0 - (r / dc) * (r / dc)).max(0.0).sqrt();
    ([axis.x, axis.y, axis.z, cos - 1e-5], dc - r)
}

/// Sphère (centre, rayon) d'une boîte de joueur pour le premier tri du shader : celle de sa
/// boîte englobante, élargie comme `box_cone`.
fn box_bound(b: &crate::ShadowBox) -> [f32; 4] {
    let (lo, hi) = (Vec3::from(b.min), Vec3::from(b.max));
    let c = (lo + hi) * 0.5;
    let r = (hi - lo).length() * 0.5 + 0.05 + 2e-6 * c.abs().max_element();
    [c.x, c.y, c.z, r]
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
    /// (v222) La lumière telle qu'elle était quand sa carte a été calculée.
    baked_key: Option<ShadowKey>,
}

impl Slot {
    /// (v222) Carte calculée pour la lumière telle qu'elle est (ni déplacée, ni tournée, ni
    /// changée depuis) : à refaire seulement si des blocs ont changé.
    fn current(&self) -> bool {
        self.baked && self.baked_key == Some(self.key)
    }
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
                    baked_key: None,
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

/// (v203) Empreintes d'une grille de blocs (voir `brick_hashes`), calculées avec elle en
/// arrière-plan : à son arrivée, l'image n'a plus à parcourir son million de cases (une
/// demi à plus d'une milliseconde, à chaque grille refaite pendant qu'on se déplace).
pub struct VoxelBricks(HashMap<[i32; 3], u64>);

impl VoxelBricks {
    pub fn new(origin: [i32; 3], ids: &[u16]) -> Self {
        Self(brick_hashes(origin, ids))
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

/// Cubes de 8 blocs à refaire dans les cartes, en boîtes monde (coin bas, coin haut) : ceux
/// dont les blocs ont changé, et ceux qui entrent dans la grille (on ne les voyait pas). Ceux
/// qui en sortent n'ont pas changé : on ne les voit plus, c'est tout ; les cartes calculées
/// quand on les voyait restent justes (sinon, de loin, une carte refaite sans eux laisserait
/// passer la lumière à travers les murs).
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
    out
}

/// La grille de blocs (coin `origin`) voit-elle tout le volume de la lumière (sa sphère, ou
/// celle de son cône) ? Sur un axe où le volume est plus grand que la grille, il suffit
/// qu'elle en couvre presque toute la largeur. Sinon une carte calculée maintenant laisserait
/// passer la lumière là où la grille ne voit pas les blocs.
/// Une carte d'ombre à refaire attend-elle que la grille de blocs voie toute sa lumière ?
/// Oui pour une lumière posée loin du joueur : on garde celle d'avant, refaite quand la grille
/// la verra (une première carte se calcule quand même : mieux qu'aucune). (v217) Jamais pour
/// une lumière portée (lampe torche, torche tenue) : elle est au milieu de la grille, qui ne
/// voit pas toujours toute sa portée (la lampe porte jusqu'à 128 blocs), et sa carte d'avant,
/// prise d'ailleurs et dans une autre direction, ne vaut plus rien : sa lumière était coupée
/// par des ombres de ce qu'elle éclairait au moment où on l'a allumée. Au-delà de la grille,
/// rien ne l'arrête. (v222) `baked` : une carte faite pour la lumière telle qu'elle est
/// (`Slot::current`) ; déplacée, tournée ou changée, elle est refaite.
fn keeps_old_map(baked: bool, l: &DynLight, voxels: Option<[i32; 3]>) -> bool {
    baked && l.owner == 0 && voxels.is_some_and(|o| !grid_covers(o, l))
}

fn grid_covers(origin: [i32; 3], l: &DynLight) -> bool {
    use crate::rt::VOX;
    let (c, r) = bounding_sphere(l);
    (0..3).all(|a| {
        let (g0, g1) = (origin[a] as f32, (origin[a] + VOX[a] as i32) as f32);
        let need = (2.0 * r).min(VOX[a] as f32 - 8.0);
        let got = (c[a] + r).min(g1) - (c[a] - r).max(g0);
        got >= need - 0.5
    })
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

/// Images de la lumière volumétrique (taille du monde dessiné / 2 ou / 4, (v223) plus la
/// marge de chaque côté) et leurs liaisons.
struct VlTargets {
    key: (u32, u32, u64),
    /// Marge de chaque côté de l'écran (pixels des rayons).
    margin: (u32, u32),
    /// (v223) La marche : lumières posées et portées.
    fixed_view: wgpu::TextureView,
    carried_view: wgpu::TextureView,
    /// (v223) Historique des lumières posées (une image sur deux chacun) et image reprise
    /// que lit la recomposition.
    hist_views: [wgpu::TextureView; 2],
    combined_view: wgpu::TextureView,
    /// Groupes de la marche et de la recomposition, selon la profondeur lue ; de la reprise,
    /// selon l'historique écrit puis la profondeur.
    vl_groups: [wgpu::BindGroup; 2],
    temporal_groups: [[wgpu::BindGroup; 2]; 2],
    comp_groups: [wgpu::BindGroup; 2],
}

pub(crate) struct VlPipes {
    march: wgpu::RenderPipeline,
    /// La même marche sans le code du bruit (bruit décoché, le cas habituel).
    march_plain: wgpu::RenderPipeline,
    /// (v223) La marche de la marge autour de l'écran (voir `VL_MARGIN`).
    margin: wgpu::RenderPipeline,
    margin_plain: wgpu::RenderPipeline,
    /// (v223) Reprise d'une image à l'autre et lissage (voir `fs_temporal` ; remplace le
    /// lissage seul de la v215).
    temporal: wgpu::RenderPipeline,
    composite: wgpu::RenderPipeline,
}

/// Éclairage différé (voir `dyn_deferred.wgsl`) : un pixel sur quatre (`half`), la moyenne
/// des voisins (`interp`), le calcul exact des autres (`exact`). Ceux qui calculent la
/// lumière ont leur variante sans ombres douces (aucune lumière à l'écran n'en a :
/// préréglage Rapide, « Ombres des blocs » décochée).
pub(crate) struct DeferredPipes {
    half: wgpu::RenderPipeline,
    half_hard: wgpu::RenderPipeline,
    interp: wgpu::RenderPipeline,
    exact: wgpu::RenderPipeline,
    exact_hard: wgpu::RenderPipeline,
    /// (v199) Les mêmes sur la face translucide de chaque pixel (eau, verre, glace).
    water_half: wgpu::RenderPipeline,
    water_half_hard: wgpu::RenderPipeline,
    water_interp: wgpu::RenderPipeline,
    water: wgpu::RenderPipeline,
    water_hard: wgpu::RenderPipeline,
    /// (v220) Ombres des joueurs aux lumières : un peu de mouvement pour l'anticrénelage
    /// (`fs_shadow_motion`).
    motion: wgpu::RenderPipeline,
}

/// (v199) Groupes des passes de l'eau (voir `encode_water`), pour les images du TAA de
/// génération `generation`.
struct WaterGroups {
    generation: u64,
    group: wgpu::BindGroup,
    interp: wgpu::BindGroup,
}

/// Liaisons de l'éclairage différé (profondeur lue, albédo, surface), refaites quand les
/// images du TAA sont recréées.
struct DeferredTargets {
    generation: u64,
    groups: [wgpu::BindGroup; 2],
    /// Lumière d'un pixel sur quatre (demi-résolution, `fs_half`), pochoir des pixels
    /// laissés au calcul exact, et les liaisons de `fs_interp` (qui lit la première).
    half_view: wgpu::TextureView,
    stencil_view: wgpu::TextureView,
    interp_groups: [wgpu::BindGroup; 2],
}

/// Lumière d'un pixel sur quatre : diffus et reflets en demi-flottants, clé de la surface.
const HALF_LIGHT_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba32Uint;
/// Pochoir de l'éclairage différé : 1 là où la moyenne des voisins a suffi.
const LIGHT_STENCIL_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Stencil8;

/// Images par bloc d'essai de l'éclairage en demi-résolution, blocs par essai (la moitié
/// dans chaque mode, en alternance), images ignorées au début d'un bloc (les temps GPU
/// arrivent avec quelques images de retard), images avant un nouvel essai.
const HALF_BLOCK: u32 = 32;
const HALF_BLOCKS: u32 = 8;
const HALF_SKIP: u32 = 6;
/// (v197) Ressayé toutes les ~20 000 images (une minute et demie à 230 images/s, contre
/// 17 s avant) : chaque essai passe 256 images en mode alterné, dont une partie dans le
/// mode le plus lent. L'image est la même dans les deux modes.
const HALF_RETRY: u32 = 20_000;

/// Éclairage en demi-résolution : essayé contre le calcul exact par blocs d'images, et
/// gardé seulement s'il est plus rapide sur cette carte graphique (même image dans les deux
/// cas). Ressayé de temps en temps : la scène change.
#[derive(Default)]
struct HalfTrial {
    /// Images depuis le début de l'essai (ou de la décision).
    frames: u32,
    /// Temps GPU des lumières (ms) : somme et nombre, calcul exact (0) et demi (1).
    sum: [f64; 2],
    n: [u32; 2],
    /// Décision de l'essai (vrai : demi-résolution), et celle d'avant (journal).
    decided: Option<bool>,
    last: Option<bool>,
}

impl HalfTrial {
    /// Mode de l'image en cours (vrai : demi-résolution).
    fn mode(&self) -> bool {
        self.decided
            .unwrap_or((self.frames / HALF_BLOCK).is_multiple_of(2))
    }

    /// Une image de plus où les lumières sont dessinées.
    fn tick(&mut self) {
        self.frames += 1;
        match self.decided {
            None if self.frames >= HALF_BLOCK * HALF_BLOCKS => {
                let mean = |k: usize| self.sum[k] / self.n[k].max(1) as f64;
                let half = self.n[0] > 0 && self.n[1] > 0 && mean(1) < mean(0) * 0.97;
                if self.last != Some(half) {
                    log::info!(
                        "éclairage en demi-résolution : {:.2} ms, calcul exact : {:.2} ms → {}",
                        mean(1),
                        mean(0),
                        if half { "gardé" } else { "pas gardé" }
                    );
                }
                self.decided = Some(half);
                self.last = Some(half);
                self.frames = 0;
            }
            Some(_) if self.frames >= HALF_RETRY => {
                *self = Self {
                    last: self.last,
                    ..Self::default()
                };
            }
            _ => {}
        }
    }

    /// Temps GPU des lumières d'une image (quelques images plus tôt, dans le même bloc).
    fn note(&mut self, ms: f32) {
        if self.decided.is_some() || self.frames % HALF_BLOCK < HALF_SKIP {
            return;
        }
        let k = self.mode() as usize;
        self.sum[k] += ms as f64;
        self.n[k] += 1;
    }
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
    /// (v223) Rayons repris d'une image à l'autre : `VlFrameGpu` de l'image, caméra de cette
    /// image et de la précédente (vue-projection comptée depuis la caméra, position),
    /// empreinte des lumières posées à rayons (changée : historique oublié), image où les
    /// rayons ont été faits la dernière fois, des lumières posées touchent la marge.
    vl_frame_buf: wgpu::Buffer,
    vl_cur: Option<(Mat4, Vec3)>,
    vl_prev: Option<(Mat4, Vec3)>,
    vl_sig: u64,
    vl_sig_changed: bool,
    vl_last: u64,
    vl_margin_lights: bool,
    // Éclairage différé.
    pub(crate) def_bgl: wgpu::BindGroupLayout,
    /// Celles de `fs_interp` : les mêmes, plus la lumière d'un pixel sur quatre.
    pub(crate) interp_bgl: wgpu::BindGroupLayout,
    def_targets: Option<DeferredTargets>,
    /// (v199) Celles de `fs_water` et `fs_water_half` : cases éclairées et face translucide
    /// de chaque pixel ; celles de `fs_water_interp` : les mêmes, plus la lumière d'un pixel
    /// sur quatre. Leurs groupes, refaits quand les images du TAA sont recréées.
    pub(crate) water_bgl: wgpu::BindGroupLayout,
    pub(crate) water_interp_bgl: wgpu::BindGroupLayout,
    water_groups: Option<WaterGroups>,
    // État de l'image.
    lights: Vec<DynLight>,
    settings: DynSettings,
    cache: ShadowCache,
    bricks: HashMap<[i32; 3], u64>,
    frame: u64,
    pub stats: DynStats,
    /// Les liaisons du monde sont à refaire (atlas ou gobos recréés).
    pub rebind: bool,
    vl_active: bool,
    /// Bruit des rayons coché (sinon : la marche sans son code).
    vl_noise: bool,
    /// Une lumière au moins a des ombres douces (sinon : l'éclairage différé sans leur code).
    any_soft: bool,
    /// (v220) Une lumière au moins a des ombres de joueurs (boîtes ou atlas des joueurs) :
    /// passe du mouvement de leurs ombres (voir `encode_deferred`).
    any_player_shadow: bool,
    /// Éclairage en demi-résolution à cette image (réglage coché et plus rapide ici).
    half_now: bool,
    half_trial: HalfTrial,
    gpu_lights: Vec<GpuLight>,
    tiles: Vec<u32>,
    /// (v220) Atlas des ombres des joueurs : mêmes cases que `atlas`, en proportion. Effacé
    /// puis redessiné à chaque image où des joueurs sont à portée d'une lumière qui a sa
    /// carte : toute une foule y fait son ombre, au-delà des boîtes exactes.
    ent_atlas: Option<(wgpu::Texture, wgpu::TextureView)>,
    ent_size: u32,
    /// Cases de l'atlas des joueurs à dessiner à cette image, et leurs joueurs (indices dans
    /// les joueurs donnés à `prepare`).
    pub(crate) ent_jobs: Vec<EntJob>,
    pub(crate) ent_casters: Vec<u32>,
    /// Il y a quelque chose dans l'atlas des joueurs (à effacer quand il n'y a plus rien).
    pub(crate) ent_dirty: bool,
}

/// Entrées du groupe du monde (liaisons 7 à 13, et (v220) 23) pour les lumières
/// dynamiques.
pub(crate) fn world_layout_entries() -> [wgpu::BindGroupLayoutEntry; 8] {
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
        // (v220) Atlas des ombres des joueurs.
        wgpu::BindGroupLayoutEntry {
            binding: 23,
            visibility: frag,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Depth,
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        },
    ]
}

/// (v220) Un atlas des ombres des joueurs de `size` pixels de côté.
fn ent_atlas(device: &wgpu::Device, size: u32) -> (wgpu::Texture, wgpu::TextureView) {
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("ombres des joueurs aux lumières"),
        size: wgpu::Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: ENT_ATLAS_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let view = tex.create_view(&Default::default());
    (tex, view)
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
        "{src}\n{}\n{}",
        include_str!("shaders/gbuffer.wgsl"),
        include_str!("shaders/dynlights.wgsl")
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

/// Compile `f` et note son temps dans le journal (`label`).
fn timed<T>(label: &str, f: impl FnOnce() -> T) -> T {
    let t0 = std::time::Instant::now();
    let out = f();
    log::info!("  {label} : {:.1} s", t0.elapsed().as_secs_f32());
    out
}

/// Pipelines des lumières : éclairage différé, calcul des cartes d'ombre, rayons
/// volumétriques et leur recomposition, chacun sur son fil (`Renderer::spawn_lit`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn light_pipes(
    device: &wgpu::Device,
    world_bgl: &wgpu::BindGroupLayout,
    bake_bgl: &wgpu::BindGroupLayout,
    vl_bgl: &wgpu::BindGroupLayout,
    comp_bgl: &wgpu::BindGroupLayout,
    def_bgl: &wgpu::BindGroupLayout,
    interp_bgl: &wgpu::BindGroupLayout,
    water_bgl: &wgpu::BindGroupLayout,
    water_interp_bgl: &wgpu::BindGroupLayout,
    surface: wgpu::TextureFormat,
) -> (
    wgpu::RenderPipeline,
    VlPipes,
    DeferredPipes,
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
    let interp_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("éclairage différé : moyenne des voisins"),
        bind_group_layouts: &[world_bgl, interp_bgl],
        push_constant_ranges: &[],
    });
    let water_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("lumières sur l'eau"),
        bind_group_layouts: &[world_bgl, water_bgl],
        push_constant_ranges: &[],
    });
    let water_interp_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("lumières sur l'eau : moyenne des voisins"),
        bind_group_layouts: &[world_bgl, water_interp_bgl],
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
        // Éclairage différé : un pixel sur quatre, la moyenne des voisins, le calcul exact des
        // autres (avec et sans le code des ombres douces), chacun sur son fil.
        let (def_layout, def_shader) = (&def_layout, &def_shader);
        let def =
            |label: &'static str, vs: &'static str, fs: &'static str, hard: bool, half: bool| {
                sc.spawn(move || {
                    timed(label, || {
                        stencil_variant(
                            device,
                            label,
                            def_layout,
                            def_shader,
                            vs,
                            fs,
                            if half {
                                HALF_LIGHT_FORMAT
                            } else {
                                COLOR_FORMAT
                            },
                            if half { None } else { additive },
                            if hard {
                                &[("DL_SOFT_SHADOWS", 0.0)]
                            } else {
                                &[]
                            },
                            (!half).then(|| light_stencil(false)),
                        )
                    })
                })
            };
        let half = def(
            "éclairage différé : un pixel sur quatre",
            "vs_tiles_half",
            "fs_half",
            false,
            true,
        );
        let half_hard = def(
            "éclairage différé : un pixel sur quatre (ombres nettes)",
            "vs_tiles_half",
            "fs_half",
            true,
            true,
        );
        let exact = def(
            "éclairage différé : calcul exact",
            "vs_tiles",
            "fs_deferred",
            false,
            false,
        );
        let exact_hard = def(
            "éclairage différé : calcul exact (ombres nettes)",
            "vs_tiles",
            "fs_deferred",
            true,
            false,
        );
        // (v220) Ombres des joueurs : mouvement ajouté sur l'image des mouvements.
        let motion = sc.spawn(move || {
            timed("ombres des joueurs : mouvement", || {
                stencil_variant(
                    device,
                    "ombres des joueurs : mouvement",
                    def_layout,
                    def_shader,
                    "vs_tiles",
                    "fs_shadow_motion",
                    crate::taa::MOTION_FORMAT,
                    Some(wgpu::BlendState {
                        color: add,
                        alpha: add,
                    }),
                    &[("DL_SOFT_SHADOWS", 0.0)],
                    None,
                )
            })
        });
        // (v199) Eau, verre, glace : la face translucide de chaque pixel, de la même façon.
        let (water_layout, water_interp_layout) = (&water_layout, &water_interp_layout);
        let water =
            |label: &'static str, vs: &'static str, fs: &'static str, hard: bool, half: bool| {
                sc.spawn(move || {
                    timed(label, || {
                        stencil_variant(
                            device,
                            label,
                            water_layout,
                            def_shader,
                            vs,
                            fs,
                            if half {
                                HALF_LIGHT_FORMAT
                            } else {
                                COLOR_FORMAT
                            },
                            if half { None } else { additive },
                            if hard {
                                &[("DL_SOFT_SHADOWS", 0.0)]
                            } else {
                                &[]
                            },
                            (!half).then(|| light_stencil(false)),
                        )
                    })
                })
            };
        let water_half = water(
            "lumières sur l'eau : un pixel sur quatre",
            "vs_tiles_half",
            "fs_water_half",
            false,
            true,
        );
        let water_half_hard = water(
            "lumières sur l'eau : un pixel sur quatre (ombres nettes)",
            "vs_tiles_half",
            "fs_water_half",
            true,
            true,
        );
        let water_exact = water(
            "lumières sur l'eau : calcul exact",
            "vs_tiles",
            "fs_water",
            false,
            false,
        );
        let water_exact_hard = water(
            "lumières sur l'eau : calcul exact (ombres nettes)",
            "vs_tiles",
            "fs_water",
            true,
            false,
        );
        let water_interp = sc.spawn(move || {
            timed("lumières sur l'eau : moyenne des voisins", || {
                stencil_variant(
                    device,
                    "lumières sur l'eau : moyenne des voisins",
                    water_interp_layout,
                    def_shader,
                    "vs_tiles",
                    "fs_water_interp",
                    COLOR_FORMAT,
                    additive,
                    &[],
                    Some(light_stencil(true)),
                )
            })
        });
        let interp = sc.spawn(|| {
            timed("éclairage différé : moyenne des voisins", || {
                stencil_variant(
                    device,
                    "éclairage différé : moyenne des voisins",
                    &interp_layout,
                    def_shader,
                    "vs_tiles",
                    "fs_interp",
                    COLOR_FORMAT,
                    additive,
                    &[],
                    Some(light_stencil(true)),
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
        // (v223) Deux images : lumières posées et lumières portées ; l'écran (`vs_tiles`)
        // et la marge (`vs_margin`), avec ou sans le code du bruit.
        let march = sc.spawn(|| {
            timed("rayons volumétriques", || {
                pair_pipeline(device, "volumétrique", &layout, &shader, "vs_tiles", "fs_vl", &[])
            })
        });
        let march_plain = sc.spawn(|| {
            timed("rayons volumétriques (sans bruit)", || {
                pair_pipeline(
                    device,
                    "volumétrique (sans bruit)",
                    &layout,
                    &shader,
                    "vs_tiles",
                    "fs_vl",
                    &[("VL_NOISE", 0.0)],
                )
            })
        });
        let margin = sc.spawn(|| {
            timed("rayons volumétriques (marge)", || {
                pair_pipeline(
                    device,
                    "volumétrique : marge",
                    &layout,
                    &shader,
                    "vs_margin",
                    "fs_vl",
                    &[],
                )
            })
        });
        let margin_plain = sc.spawn(|| {
            timed("rayons volumétriques (marge, sans bruit)", || {
                pair_pipeline(
                    device,
                    "volumétrique : marge (sans bruit)",
                    &layout,
                    &shader,
                    "vs_margin",
                    "fs_vl",
                    &[("VL_NOISE", 0.0)],
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
                    "vs_tiles",
                    "fs_composite",
                    COLOR_FORMAT,
                    additive,
                )
            })
        });
        let temporal = sc.spawn(|| {
            timed("reprise des rayons", || {
                pair_pipeline(
                    device,
                    "volumétrique : reprise",
                    &comp_layout,
                    &comp_shader,
                    "vs_full",
                    "fs_temporal",
                    &[],
                )
            })
        });
        (
            bake.join().expect("pipeline des ombres des lumières"),
            VlPipes {
                march: march.join().expect("pipeline volumétrique"),
                march_plain: march_plain
                    .join()
                    .expect("pipeline volumétrique (sans bruit)"),
                margin: margin.join().expect("pipeline volumétrique (marge)"),
                margin_plain: margin_plain
                    .join()
                    .expect("pipeline volumétrique (marge, sans bruit)"),
                temporal: temporal.join().expect("pipeline de reprise des rayons"),
                composite: composite.join().expect("pipeline de recomposition"),
            },
            DeferredPipes {
                half: half
                    .join()
                    .expect("éclairage différé : un pixel sur quatre"),
                half_hard: half_hard
                    .join()
                    .expect("éclairage différé : un pixel sur quatre"),
                interp: interp.join().expect("éclairage différé : moyenne"),
                exact: exact.join().expect("éclairage différé : calcul exact"),
                exact_hard: exact_hard.join().expect("éclairage différé : calcul exact"),
                water_half: water_half.join().expect("lumières sur l'eau"),
                water_half_hard: water_half_hard.join().expect("lumières sur l'eau"),
                water_interp: water_interp.join().expect("lumières sur l'eau"),
                water: water_exact.join().expect("lumières sur l'eau"),
                water_hard: water_exact_hard.join().expect("lumières sur l'eau"),
                motion: motion.join().expect("ombres des joueurs : mouvement"),
            },
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
    fullscreen_variant(device, label, layout, shader, vs, fs, format, blend, &[])
}

/// Comme `fullscreen_pipeline`, avec des constantes du shader de pixels fixées à la
/// compilation (`override` en WGSL) : le code qu'elles coupent disparaît du shader.
#[allow(clippy::too_many_arguments)]
fn fullscreen_variant(
    device: &wgpu::Device,
    label: &str,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    vs: &str,
    fs: &str,
    format: wgpu::TextureFormat,
    blend: Option<wgpu::BlendState>,
    constants: &[(&str, f64)],
) -> wgpu::RenderPipeline {
    stencil_variant(
        device, label, layout, shader, vs, fs, format, blend, constants, None,
    )
}

/// (v223) Passe à deux images des rayons (`VL_FORMAT`), écrites en entier, sans mélange.
fn pair_pipeline(
    device: &wgpu::Device,
    label: &str,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    vs: &str,
    fs: &str,
    constants: &[(&str, f64)],
) -> wgpu::RenderPipeline {
    let target = Some(wgpu::ColorTargetState {
        format: VL_FORMAT,
        blend: None,
        write_mask: wgpu::ColorWrites::ALL,
    });
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
            compilation_options: wgpu::PipelineCompilationOptions {
                constants,
                ..Default::default()
            },
            targets: &[target.clone(), target],
        }),
        multiview: None,
        cache: None,
    })
}

/// Canaux écrits : l'image du monde garde son alpha (on y ajoute la lumière) ; les images à
/// part (éclairage en demi-résolution, rayons et leurs profondeurs) sont écrites en entier.
/// (De la v151 à la v186, les rayons n'écrivaient pas leur alpha, la profondeur de leurs
/// pixels : la recomposition ne savait plus séparer un objet du fond, d'où des bords sombres
/// en escalier autour des blocs.)
fn write_mask(format: wgpu::TextureFormat) -> wgpu::ColorWrites {
    if format == COLOR_FORMAT {
        wgpu::ColorWrites::COLOR
    } else {
        wgpu::ColorWrites::ALL
    }
}

/// Pochoir de l'éclairage différé : `fs_interp` met 1 là où la moyenne a suffi
/// (`write`), le calcul exact ne passe que là où il vaut 0.
fn light_stencil(write: bool) -> wgpu::DepthStencilState {
    let face = wgpu::StencilFaceState {
        compare: if write {
            wgpu::CompareFunction::Always
        } else {
            wgpu::CompareFunction::Equal
        },
        fail_op: wgpu::StencilOperation::Keep,
        depth_fail_op: wgpu::StencilOperation::Keep,
        pass_op: if write {
            wgpu::StencilOperation::Replace
        } else {
            wgpu::StencilOperation::Keep
        },
    };
    wgpu::DepthStencilState {
        format: LIGHT_STENCIL_FORMAT,
        depth_write_enabled: false,
        depth_compare: wgpu::CompareFunction::Always,
        stencil: wgpu::StencilState {
            front: face,
            back: face,
            read_mask: 0xff,
            write_mask: if write { 0xff } else { 0 },
        },
        bias: Default::default(),
    }
}

/// Comme `fullscreen_variant`, avec le pochoir de l'éclairage différé (`light_stencil`).
#[allow(clippy::too_many_arguments)]
fn stencil_variant(
    device: &wgpu::Device,
    label: &str,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    vs: &str,
    fs: &str,
    format: wgpu::TextureFormat,
    blend: Option<wgpu::BlendState>,
    constants: &[(&str, f64)],
    stencil: Option<wgpu::DepthStencilState>,
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
        depth_stencil: stencil,
        multisample: Default::default(),
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some(fs),
            compilation_options: wgpu::PipelineCompilationOptions {
                constants,
                ..Default::default()
            },
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend,
                write_mask: write_mask(format),
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
        // (v223) Une tuile de plus : la marge des rayons (voir `bin_tiles`).
        let tiles_buf = buf(
            "lumières : tuiles",
            ((TILES_X * TILES_Y + 1) * WORDS * 4) as u64,
            wgpu::BufferUsages::STORAGE,
        );
        let vl_frame_buf = buf(
            "volumétrique : image précédente",
            std::mem::size_of::<VlFrameGpu>() as u64,
            wgpu::BufferUsages::UNIFORM,
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
                // (v223) Image des rayons d'une image à l'autre (`VlFrame`).
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
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
                    wgpu::TextureSampleType::Uint,
                    wgpu::TextureViewDimension::D2,
                ),
                tex_entry(
                    1,
                    wgpu::TextureSampleType::Depth,
                    wgpu::TextureViewDimension::D2,
                ),
                // Cases éclairées : seulement là que la recomposition dessine.
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                // (v223) Reprise : marche des lumières portées, historique, `VlFrame`.
                tex_entry(
                    3,
                    wgpu::TextureSampleType::Uint,
                    wgpu::TextureViewDimension::D2,
                ),
                tex_entry(
                    4,
                    wgpu::TextureSampleType::Uint,
                    wgpu::TextureViewDimension::D2,
                ),
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
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
        let interp_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("éclairage différé : moyenne des voisins"),
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
                tex_entry(
                    4,
                    wgpu::TextureSampleType::Uint,
                    wgpu::TextureViewDimension::D2,
                ),
            ],
        });
        let water_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("lumières sur l'eau"),
            entries: &[
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
                tex_entry(
                    5,
                    wgpu::TextureSampleType::Uint,
                    wgpu::TextureViewDimension::D2,
                ),
            ],
        });
        let water_interp_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("lumières sur l'eau : moyenne des voisins"),
            entries: &[
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
                tex_entry(
                    4,
                    wgpu::TextureSampleType::Uint,
                    wgpu::TextureViewDimension::D2,
                ),
                tex_entry(
                    5,
                    wgpu::TextureSampleType::Uint,
                    wgpu::TextureViewDimension::D2,
                ),
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
            vl_frame_buf,
            vl_cur: None,
            vl_prev: None,
            vl_sig: 0,
            vl_sig_changed: true,
            vl_last: 0,
            vl_margin_lights: false,
            def_bgl,
            interp_bgl,
            def_targets: None,
            water_bgl,
            water_interp_bgl,
            water_groups: None,
            lights: Vec::new(),
            settings: DynSettings::default(),
            cache: ShadowCache::default(),
            bricks: HashMap::new(),
            frame: 0,
            stats: DynStats::default(),
            rebind: false,
            vl_active: false,
            vl_noise: true,
            any_soft: true,
            any_player_shadow: false,
            half_now: false,
            half_trial: HalfTrial::default(),
            gpu_lights: Vec::new(),
            tiles: vec![0; ((TILES_X * TILES_Y + 1) * WORDS) as usize],
            ent_atlas: None,
            ent_size: 0,
            ent_jobs: Vec::new(),
            ent_casters: Vec::new(),
            ent_dirty: false,
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
        self.ent_atlas = Some(ent_atlas(device, 16));
        self.ent_size = 16;
        self.ent_dirty = true;
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
        // (v220) L'atlas des joueurs : mêmes cases, en proportion (au plus `ENT_ATLAS_MAX`).
        let ent = size.min(ENT_ATLAS_MAX);
        self.ent_atlas = Some(ent_atlas(device, ent));
        self.ent_size = ent;
        self.ent_dirty = true;
        let per = (size / tile) as usize;
        self.cache.reset(per * per);
        self.rebind = true;
        log::info!(
            "ombres des lumières : qualité {} (atlas {size} px, {} cases de {tile} px, {} Mo ; joueurs {ent} px)",
            SHADOW_QUALITY_NAMES[q as usize],
            per * per,
            (size as u64 * size as u64 + ent as u64 * ent as u64) * 4 / (1024 * 1024)
        );
    }

    /// (v220) L'atlas des ombres des joueurs (pour sa passe : voir `Entities::draw_light_casters`)
    /// et la taille d'une de ses cases.
    pub(crate) fn ent_target(&self) -> Option<(&wgpu::TextureView, u32)> {
        let per = (self.atlas_size / self.tile_size.max(1)).max(1);
        self.ent_atlas
            .as_ref()
            .map(|(_, v)| (v, (self.ent_size / per).max(1)))
    }

    /// Liaisons 7 à 13 du groupe du monde, et (v220) 23.
    pub fn bind_entries(&self) -> [wgpu::BindGroupEntry<'_>; 8] {
        let atlas = &self.atlas.as_ref().expect("atlas des ombres").1;
        let ent = &self.ent_atlas.as_ref().expect("atlas des joueurs").1;
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
            wgpu::BindGroupEntry {
                binding: 23,
                resource: wgpu::BindingResource::TextureView(ent),
            },
        ]
    }

    /// Lumières et réglages de la prochaine image. Renvoie vrai si quelque chose a changé
    /// depuis l'image précédente (lumière déplacée, réglée, ajoutée ou retirée).
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
        // Les murs arrêtent toujours la lumière (cartes calculées dans la grille).
        !self.lights.is_empty()
    }

    /// Nouvelle grille de blocs (ses empreintes, `VoxelBricks`) : les cartes des lumières près
    /// des blocs qui ont changé sont à refaire.
    pub fn on_voxels(&mut self, bricks: VoxelBricks) {
        if self.cache.slots.is_empty() && !self.wants_voxels() {
            // Personne n'en a besoin : l'empreinte sera prise à la prochaine grille utile.
            self.bricks.clear();
            return;
        }
        let new = bricks.0;
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
        casters: &[[f32; 3]],
        time: f32,
        voxels: Option<[i32; 3]>,
    ) -> u32 {
        self.frame += 1;
        let frame = self.frame;
        let st = self.settings;
        // Comme pour les torches du jeu, les murs arrêtent toujours la lumière : une carte
        // pour chaque lumière dès que la grille de blocs est là. Les réglages d'ombre ne
        // décident que de leur douceur (ombres douces, ou simple arrêt net, bien moins cher).
        let block_maps = voxels.is_some();
        if block_maps && !self.lights.is_empty() {
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
        // (v202) Les lumières portées (torche tenue, lampe torche) d'abord : elles bougent à
        // chaque image, leur carte doit suivre. Derrière des torches à refaire (blocs changés,
        // grille de blocs déplacée), elles attendaient leur tour et leurs ombres sautaient.
        again.sort_by_key(|&i| self.lights[i].owner == 0);
        // (v217) Et avant tout le reste, sans limite : une lumière portée dont la carte est à
        // faire ou à refaire (derrière les premiers calculs d'autres lumières, elle pouvait
        // garder sa carte d'avant une image ou plus : l'ombre de ce qu'elle éclairait avant).
        let carried: Vec<usize> = first
            .iter()
            .chain(again.iter())
            .copied()
            .filter(|&i| self.lights[i].owner != 0)
            .collect();
        first.retain(|&i| self.lights[i].owner == 0);
        again.retain(|&i| self.lights[i].owner == 0);
        // 2. Cases à calculer : premiers calculs d'abord (jusqu'à 4 fois le budget), puis les
        // cartes périmées dans la limite du budget (au moins une lumière par image).
        let budget = st.bake_budget.max(1) as usize;
        self.jobs.clear();
        let per = (self.atlas_size / self.tile_size.max(1)).max(1);
        // (v223) Carte refaite d'une lumière posée qui a des rayons : ses ombres dans les rayons
        // changent, l'image des rayons d'avant ne vaut plus (voir `vl_sig_changed`).
        let mut rebaked_beam = false;
        for (list, limit) in [(&carried, usize::MAX), (&first, budget * 4), (&again, budget)] {
            let mut used = 0usize;
            for &i in list.iter() {
                let l = self.lights[i];
                let Some(s) = self.cache.slots.get_mut(&l.id) else {
                    continue;
                };
                // Carte à refaire, mais la grille ne voit pas toute la lumière (joueur loin) :
                // on garde celle d'avant (voir `keeps_old_map`), (v222) si elle a été faite
                // pour la lumière telle qu'elle est. Déplacée ou tournée, sa carte d'avant ne
                // vaut plus rien : son ombre ne suivait plus (un projecteur mis tout droit
                // vers le bas, loin de la caméra, gardait l'ombre de sa direction d'avant).
                if keeps_old_map(s.current(), &l, voxels) {
                    continue;
                }
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
                s.baked_key = Some(s.key);
                s.dirty = false;
                rebaked_beam |= l.owner == 0 && l.beam > 0.0;
            }
        }
        // 3. Lumières pour le shader : case d'ombre seulement si la carte est prête (ou
        // calculée à cette image).
        let mut shadowed = 0;
        for (i, l) in self.lights.iter().enumerate() {
            if !block_maps {
                continue;
            }
            if let Some(s) = self.cache.slots.get(&l.id) {
                if s.baked && s.seen == frame {
                    tiles_of[i] = Some(s.tile);
                    shadowed += 1;
                }
            }
        }
        // (v220) 3 bis. Atlas des ombres des joueurs : pour chaque lumière qui a sa carte, les
        // joueurs à sa portée (toute une foule, au-delà des boîtes exactes), dessinés dans les
        // mêmes cases que sa carte des blocs (une, ou six pour un cube), avec le même repère.
        self.ent_jobs.clear();
        self.ent_casters.clear();
        let mut ent_of = vec![false; self.lights.len()];
        if st.shadows && st.entity_shadows && !casters.is_empty() {
            let per = (self.atlas_size / self.tile_size.max(1)).max(1);
            let et = (self.ent_size / per).max(1);
            for (i, l) in self.lights.iter().enumerate() {
                let (Some(tile), true) = (tiles_of[i], l.shadows) else {
                    continue;
                };
                let Some(slot) = self.cache.slots.get(&l.id) else {
                    continue;
                };
                let p = Vec3::from(l.pos);
                let reach = l.range.max(0.05) + 1.5;
                let start = self.ent_casters.len() as u32;
                for (k, f) in casters.iter().enumerate() {
                    if self.ent_casters.len() >= MAX_ENT_CASTERS {
                        break;
                    }
                    // (Le milieu du joueur, à 1,5 bloc près : sa taille.)
                    let mid = Vec3::from(*f) + Vec3::Y * 0.9;
                    if mid.distance_squared(p) < reach * reach {
                        self.ent_casters.push(k as u32);
                    }
                }
                let end = self.ent_casters.len() as u32;
                if end == start {
                    continue;
                }
                ent_of[i] = true;
                let range = l.range.max(0.05);
                let mut push = |t: u32, (r, u, f): (Vec3, Vec3, Vec3), tan_half: f32| {
                    let (row, col) = (t / per, t % per);
                    self.ent_jobs.push(EntJob {
                        rect: [col * et, row * et, et],
                        frame: EntFrame {
                            pos_range: [p.x, p.y, p.z, range],
                            right: [r.x, r.y, r.z, 0.0],
                            up: [u.x, u.y, u.z, 0.0],
                            fwd: [f.x, f.y, f.z, tan_half],
                        },
                        casters: start..end,
                    });
                };
                if slot.key.kind == 2 {
                    for f in 0..6 {
                        push(tile + f, face_basis(f), 1.0);
                    }
                } else {
                    push(tile, spot_basis(unit_dir(l.dir)), slot.key.tan_half);
                }
            }
        }
        let nboxes = boxes.len().min(MAX_BOXES);
        let box_shadows = st.shadows && st.entity_shadows && nboxes > 0;
        self.gpu_lights.clear();
        self.any_soft = false;
        self.any_player_shadow = false;
        let atlas_px = self.atlas_size.max(1) as f32;
        let tile_uv = self.tile_size as f32 / atlas_px;
        for (i, l) in self.lights.iter().enumerate() {
            let mask = if box_shadows && l.shadows {
                box_mask(l, &boxes[..nboxes])
            } else {
                0
            };
            // Case de la carte d'ombre (même calcul que `dl_tile_rect`).
            let rect = tiles_of[i].map_or([0.0; 3], |t| {
                let (row, col) = (t / per, t % per);
                [col as f32 * tile_uv, row as f32 * tile_uv, tile_uv]
            });
            let sphere = boxes_sphere(&boxes[..nboxes], mask);
            // Ombres douces des blocs : réglage général, ombres des blocs, et celles de la
            // lumière ; sinon arrêt net par les murs.
            let soft = st.shadows && st.block_shadows && l.shadows;
            // (Une lumière sans carte prête n'a pas d'ombre des blocs, douce ou non.)
            self.any_soft |= soft && tiles_of[i].is_some();
            self.any_player_shadow |= mask != 0 || ent_of[i];
            self.gpu_lights.push(pack_light(
                l,
                tiles_of[i],
                mask,
                rect,
                sphere,
                soft,
                ent_of[i],
            ));
        }
        bin_tiles(&self.lights, view_proj, eye, &mut self.tiles);
        lit_rects(&self.tiles, &mut self.rects);
        // (v223) Rayons repris d'une image à l'autre : la caméra de cette image, et l'empreinte
        // des lumières posées à rayons (une lumière ajoutée, retirée, déplacée, changée, ou dont
        // la carte d'ombre est refaite : l'image d'avant ne vaut plus, elle est oubliée).
        self.vl_cur = Some((crate::eye_relative(view_proj), eye));
        let mut sig = std::collections::hash_map::DefaultHasher::new();
        for (l, g) in self.lights.iter().zip(&self.gpu_lights) {
            if l.owner == 0 && l.beam > 0.0 {
                // (Sans les joueurs qui lui font de l'ombre : les rayons ne les voient pas.)
                let mut g = *g;
                g.boxes = [0; 4];
                g.box_sphere = [0.0; 4];
                std::hash::Hash::hash(bytemuck::bytes_of(&g), &mut sig);
            }
        }
        let sig = std::hash::Hasher::finish(&sig);
        self.vl_sig_changed = sig != self.vl_sig || rebaked_beam;
        self.vl_sig = sig;
        let m = (TILES_X * TILES_Y * WORDS) as usize;
        self.vl_margin_lights = self.tiles[m..m + WORDS as usize].iter().any(|w| *w != 0);
        // 4. En-tête.
        let vl_on = st.volumetric
            && st.vl_intensity > 0.0
            && self
                .lights
                .iter()
                .any(|l| l.beam > 0.0 && l.intensity > 0.0);
        let mut flags = 0;
        // Les rayons s'arrêtent aussi aux murs (tous les `vl_shadow_stride` pas).
        if block_maps || st.vl_shadows {
            flags |= F_VL_SHADOWS;
        }
        if st.vl_noise {
            flags |= F_VL_NOISE;
        }
        self.vl_noise = st.vl_noise;
        if st.vl_dither_temporal {
            flags |= F_DITHER;
        }
        if block_maps || st.shadows {
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
        // Demi-résolution : seulement quand elle est plus rapide que le calcul exact ici.
        if !st.half_lighting {
            self.half_trial = HalfTrial::default();
        } else if !self.rects.is_empty() && !self.gpu_lights.is_empty() {
            self.half_trial.tick();
        }
        self.half_now = st.half_lighting && self.half_trial.mode();
        if self.half_now {
            flags |= F_HALF_LIGHT;
        }
        let mut box_x = [[0.0; 4]; MAX_BOXES];
        let mut box_y = [[0.0; 4]; MAX_BOXES];
        let mut box_z = [[0.0; 4]; MAX_BOXES];
        let mut box_s = [[0.0; 4]; MAX_BOXES];
        for (k, b) in boxes.iter().take(nboxes).enumerate() {
            [box_x[k], box_y[k], box_z[k]] = b.to_unit;
            box_s[k] = box_bound(b);
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
                st.vl_scale.clamp(2, 4) as f32,
            ],
            box_x,
            box_y,
            box_z,
            box_s,
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

    /// Temps GPU de l'éclairage différé d'une image passée (ms ; v202 : opaque et eau) : pour
    /// l'essai de la demi-résolution.
    pub fn note_light_gpu(&mut self, ms: f32) {
        self.half_trial.note(ms);
    }

    /// Des cartes d'ombre à calculer à cette image ?
    pub fn bake_pending(&self) -> bool {
        !self.jobs.is_empty() && self.bake_group.is_some() && self.atlas.is_some()
    }

    /// (v199) Lumières sur la face translucide de chaque pixel (eau, verre, glace), après la
    /// passe des translucides : `water` est ce qu'elle a laissé (images du TAA de génération
    /// `generation`), ajouté à `color` dans les cases éclairées, comme `encode_deferred` (un
    /// pixel sur quatre, puis la moyenne des voisins ou le calcul exact, dans ses images à
    /// elle, libres une fois l'opaque éclairé). `timestamps` : début et fin des deux passes.
    #[allow(clippy::too_many_arguments)]
    pub fn encode_water(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        world_group: &wgpu::BindGroup,
        water: &wgpu::TextureView,
        generation: u64,
        color: &wgpu::TextureView,
        pipes: &DeferredPipes,
        timer: Option<&crate::gpu_timer::GpuTimer>,
    ) {
        if self.rects.is_empty() || self.gpu_lights.is_empty() {
            return;
        }
        // Images de l'éclairage de l'opaque (même génération : refaites avant, cette image).
        let Some(t) = self
            .def_targets
            .as_ref()
            .filter(|t| t.generation == generation)
        else {
            return;
        };
        if self
            .water_groups
            .as_ref()
            .is_none_or(|g| g.generation != generation)
        {
            let rects = wgpu::BindGroupEntry {
                binding: 3,
                resource: self.rects_buf.as_entire_binding(),
            };
            let face = wgpu::BindGroupEntry {
                binding: 5,
                resource: wgpu::BindingResource::TextureView(water),
            };
            let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("lumières sur l'eau"),
                layout: &self.water_bgl,
                entries: &[rects.clone(), face.clone()],
            });
            let interp = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("lumières sur l'eau : moyenne des voisins"),
                layout: &self.water_interp_bgl,
                entries: &[
                    rects,
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: wgpu::BindingResource::TextureView(&t.half_view),
                    },
                    face,
                ],
            });
            self.water_groups = Some(WaterGroups {
                generation,
                group,
                interp,
            });
        }
        let Some(g) = &self.water_groups else {
            return;
        };
        let soft = self.any_soft;
        // (v202) Mesure demandée seulement quand les passes ont lieu (sinon l'ancienne mesure,
        // restée dans les requêtes, était relue à chaque image).
        let timestamps = timer.and_then(|t| t.pass(crate::gpu_timer::WATER));
        // Début dans la première passe, fin dans la seconde (une seule mesure pour les deux).
        let (ts_half, ts_main) = match timestamps {
            Some(ts) if self.half_now => (
                Some(wgpu::RenderPassTimestampWrites {
                    query_set: ts.query_set,
                    beginning_of_pass_write_index: ts.beginning_of_pass_write_index,
                    end_of_pass_write_index: None,
                }),
                Some(wgpu::RenderPassTimestampWrites {
                    query_set: ts.query_set,
                    beginning_of_pass_write_index: None,
                    end_of_pass_write_index: ts.end_of_pass_write_index,
                }),
            ),
            ts => (None, ts),
        };
        // 1. Un pixel sur quatre.
        if self.half_now {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("lumières sur l'eau : un pixel sur quatre"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &t.half_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: ts_half,
                occlusion_query_set: None,
            });
            pass.set_pipeline(if soft {
                &pipes.water_half
            } else {
                &pipes.water_half_hard
            });
            pass.set_bind_group(0, world_group, &[]);
            pass.set_bind_group(1, &g.group, &[]);
            pass.draw(0..6, 0..self.rects.len() as u32);
        }
        // 2. La moyenne des voisins là où elle suffit (pochoir à 1), le calcul exact ailleurs.
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("lumières sur l'eau"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: color,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &t.stencil_view,
                depth_ops: None,
                stencil_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(0),
                    store: wgpu::StoreOp::Discard,
                }),
            }),
            timestamp_writes: ts_main,
            occlusion_query_set: None,
        });
        pass.set_bind_group(0, world_group, &[]);
        if self.half_now {
            pass.set_stencil_reference(1);
            pass.set_pipeline(&pipes.water_interp);
            pass.set_bind_group(1, &g.interp, &[]);
            pass.draw(0..6, 0..self.rects.len() as u32);
        }
        pass.set_stencil_reference(0);
        pass.set_pipeline(if soft {
            &pipes.water
        } else {
            &pipes.water_hard
        });
        pass.set_bind_group(1, &g.group, &[]);
        pass.draw(0..6, 0..self.rects.len() as u32);
    }

    /// Éclairage différé : ajoute la lumière des lumières dynamiques à `color` (image du
    /// monde opaque), d'après la profondeur de cette image et ce que les opaques ont laissé
    /// (`gbuf` : albédo, surface). `generation` change quand ces images sont recréées ;
    /// `size` : leur taille. Deux passes (voir `dyn_deferred.wgsl`) : la lumière d'un pixel
    /// sur quatre, puis la moyenne des voisins là où elle vaut le calcul exact, et le calcul
    /// exact ailleurs. `timestamps` : temps GPU de la première, puis de la seconde.
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
        size: (u32, u32),
        color: &wgpu::TextureView,
        motion: &wgpu::TextureView,
        pipes: &DeferredPipes,
        timer: Option<&crate::gpu_timer::GpuTimer>,
    ) {
        if self
            .def_targets
            .as_ref()
            .is_none_or(|t| t.generation != generation)
        {
            let texture = |label: &str, size: (u32, u32), format, usage| {
                device
                    .create_texture(&wgpu::TextureDescriptor {
                        label: Some(label),
                        size: wgpu::Extent3d {
                            width: size.0.max(1),
                            height: size.1.max(1),
                            depth_or_array_layers: 1,
                        },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format,
                        usage,
                        view_formats: &[],
                    })
                    .create_view(&Default::default())
            };
            let half_view = texture(
                "éclairage différé : un pixel sur quatre",
                (size.0.div_ceil(2), size.1.div_ceil(2)),
                HALF_LIGHT_FORMAT,
                wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            );
            let stencil_view = texture(
                "éclairage différé : pochoir",
                size,
                LIGHT_STENCIL_FORMAT,
                wgpu::TextureUsages::RENDER_ATTACHMENT,
            );
            let entries = |i: usize| {
                [
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
                ]
            };
            let groups = std::array::from_fn(|i| {
                device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("éclairage différé"),
                    layout: &self.def_bgl,
                    entries: &entries(i),
                })
            });
            let interp_groups = std::array::from_fn(|i| {
                let [e0, e1, e2, e3] = entries(i);
                device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("éclairage différé : moyenne des voisins"),
                    layout: &self.interp_bgl,
                    entries: &[
                        e0,
                        e1,
                        e2,
                        e3,
                        wgpu::BindGroupEntry {
                            binding: 4,
                            resource: wgpu::BindingResource::TextureView(&half_view),
                        },
                    ],
                })
            });
            self.def_targets = Some(DeferredTargets {
                generation,
                groups,
                half_view,
                stencil_view,
                interp_groups,
            });
        }
        let Some(t) = &self.def_targets else {
            return;
        };
        let soft = self.any_soft;
        // (v202) Chaque mesure demandée seulement si sa passe a lieu : en calcul exact, la
        // dernière mesure d'« un pixel sur quatre », restée dans les requêtes, était relue à
        // chaque image (et comptée dans l'essai de la demi-résolution).
        let ts = |pass: u32| timer.and_then(|t| t.pass(pass));
        // 1. Un pixel sur quatre (rien d'autre n'est écrit : 0, aucune lumière calculée).
        if self.half_now {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("lumières : un pixel sur quatre"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &t.half_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: ts(crate::gpu_timer::LIGHTS_HALF),
                occlusion_query_set: None,
            });
            pass.set_pipeline(if soft { &pipes.half } else { &pipes.half_hard });
            pass.set_bind_group(0, world_group, &[]);
            pass.set_bind_group(1, &t.groups[depth_index], &[]);
            pass.draw(0..6, 0..self.rects.len() as u32);
        }
        // 2. La moyenne des voisins là où elle suffit (pochoir à 1), le calcul exact ailleurs.
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
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &t.stencil_view,
                depth_ops: None,
                stencil_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(0),
                    store: wgpu::StoreOp::Discard,
                }),
            }),
            timestamp_writes: ts(crate::gpu_timer::LIGHTS),
            occlusion_query_set: None,
        });
        pass.set_bind_group(0, world_group, &[]);
        if self.half_now {
            pass.set_stencil_reference(1);
            pass.set_pipeline(&pipes.interp);
            pass.set_bind_group(1, &t.interp_groups[depth_index], &[]);
            pass.draw(0..6, 0..self.rects.len() as u32);
        }
        pass.set_stencil_reference(0);
        pass.set_pipeline(if soft {
            &pipes.exact
        } else {
            &pipes.exact_hard
        });
        pass.set_bind_group(1, &t.groups[depth_index], &[]);
        pass.draw(0..6, 0..self.rects.len() as u32);
        drop(pass);
        // (v220) 3. Ombres des joueurs : un peu de mouvement là où elles tombent (voir
        // `fs_shadow_motion`), seulement si une lumière en a.
        if self.any_player_shadow {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("ombres des joueurs : mouvement"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: motion,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&pipes.motion);
            pass.set_bind_group(0, world_group, &[]);
            pass.set_bind_group(1, &t.groups[depth_index], &[]);
            pass.draw(0..6, 0..self.rects.len() as u32);
        }
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
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        world_group: &wgpu::BindGroup,
        depths: &[wgpu::TextureView; 2],
        depth_index: usize,
        generation: u64,
        size: (u32, u32),
        color: &wgpu::TextureView,
        pipes: &VlPipes,
        timer: Option<&crate::gpu_timer::GpuTimer>,
    ) {
        if !self.volumetric_active() {
            return;
        }
        // (v202) Mesures demandées seulement quand leurs passes ont lieu (voir
        // `encode_deferred`).
        let ts = |pass: u32| timer.and_then(|t| t.pass(pass));
        let k = self.settings.vl_scale.clamp(2, 4);
        let on = (size.0.div_ceil(k), size.1.div_ceil(k));
        let margin = vl_margin(on);
        let ext = (on.0 + 2 * margin.0, on.1 + 2 * margin.1);
        let key = (on.0, on.1, generation);
        let fresh_targets = self.vl_targets.as_ref().is_none_or(|t| t.key != key);
        if fresh_targets {
            let image = |label: &str| {
                device
                    .create_texture(&wgpu::TextureDescriptor {
                        label: Some(label),
                        size: wgpu::Extent3d {
                            width: ext.0.max(1),
                            height: ext.1.max(1),
                            depth_or_array_layers: 1,
                        },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format: VL_FORMAT,
                        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                            | wgpu::TextureUsages::TEXTURE_BINDING,
                        view_formats: &[],
                    })
                    .create_view(&Default::default())
            };
            let fixed_view = image("volumétrique : lumières posées");
            let carried_view = image("volumétrique : lumières portées");
            let hist_views = [
                image("volumétrique : historique 0"),
                image("volumétrique : historique 1"),
            ];
            let combined_view = image("volumétrique : reprise");
            // Reprise puis recomposition : chacune lit les images de la passe d'avant.
            let comp_group = |i: usize,
                              src: &wgpu::TextureView,
                              carried: &wgpu::TextureView,
                              hist: &wgpu::TextureView,
                              label: &str| {
                device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some(label),
                    layout: &self.comp_bgl,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(src),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::TextureView(&depths[i]),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: self.rects_buf.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 3,
                            resource: wgpu::BindingResource::TextureView(carried),
                        },
                        wgpu::BindGroupEntry {
                            binding: 4,
                            resource: wgpu::BindingResource::TextureView(hist),
                        },
                        wgpu::BindGroupEntry {
                            binding: 5,
                            resource: self.vl_frame_buf.as_entire_binding(),
                        },
                    ],
                })
            };
            // Historique écrit `w` : lu, celui de l'autre image.
            let temporal_groups = std::array::from_fn(|w| {
                std::array::from_fn(|i| {
                    comp_group(
                        i,
                        &fixed_view,
                        &carried_view,
                        &hist_views[1 - w],
                        "volumétrique : reprise",
                    )
                })
            });
            let comp_groups = std::array::from_fn(|i| {
                comp_group(
                    i,
                    &combined_view,
                    &carried_view,
                    &fixed_view,
                    "volumétrique : recomposition",
                )
            });
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
                        wgpu::BindGroupEntry {
                            binding: 4,
                            resource: self.vl_frame_buf.as_entire_binding(),
                        },
                    ],
                })
            });
            self.vl_targets = Some(VlTargets {
                key,
                margin,
                fixed_view,
                carried_view,
                hist_views,
                combined_view,
                vl_groups,
                temporal_groups,
                comp_groups,
            });
        }
        // (v223) L'image d'avant vaut-elle encore ? Pas si les images viennent d'être
        // recréées, si les rayons n'ont pas été faits à l'image d'avant, si une lumière posée
        // à rayons a changé, ou si la caméra a sauté.
        let (cur_rel, eye) = self.vl_cur.unwrap_or((Mat4::IDENTITY, Vec3::ZERO));
        let (prev_rel, prev_eye) = self.vl_prev.unwrap_or((cur_rel, eye));
        let delta = eye - prev_eye;
        let valid = !fresh_targets
            && self.vl_last + 1 == self.frame
            && self.vl_prev.is_some()
            && !self.vl_sig_changed
            && delta.length() < VL_JUMP;
        self.vl_last = self.frame;
        self.vl_prev = self.vl_cur;
        // La marge, une image sur deux (et toujours quand l'historique repart de zéro).
        let margin_now = self.vl_margin_lights && (!valid || self.frame % 2 == 0);
        let (jx, jy) = vl_jitter(self.frame);
        let frame = VlFrameGpu {
            cur_rel: cur_rel.to_cols_array_2d(),
            prev_rel: prev_rel.to_cols_array_2d(),
            delta: [delta.x, delta.y, delta.z, if valid { 1.0 } else { 0.0 }],
            margin: [
                margin.0 as f32,
                margin.1 as f32,
                k as f32,
                if margin_now { 1.0 } else { 0.0 },
            ],
            size: [size.0 as f32, size.1 as f32, ext.0 as f32, ext.1 as f32],
            // (En se déplaçant vite, la nouvelle image compte un peu plus : les rayons vus
            // d'ailleurs changent, l'image d'avant traînerait.)
            jitter: [
                jx * k as f32,
                jy * k as f32,
                (VL_NEW + delta.length() * 0.5).min(0.5),
                (self.settings.vl_steps / 2).max(8) as f32,
            ],
        };
        queue.write_buffer(&self.vl_frame_buf, 0, bytemuck::bytes_of(&frame));
        let Some(t) = &self.vl_targets else {
            return;
        };
        let write = (self.frame % 2) as usize;
        let attach = vl_attach;
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("lumière volumétrique"),
                color_attachments: &[attach(&t.fixed_view), attach(&t.carried_view)],
                depth_stencil_attachment: None,
                timestamp_writes: ts(crate::gpu_timer::VOLUMETRIC),
                occlusion_query_set: None,
            });
            pass.set_pipeline(if self.vl_noise {
                &pipes.march
            } else {
                &pipes.march_plain
            });
            pass.set_bind_group(0, world_group, &[]);
            pass.set_bind_group(1, &t.vl_groups[depth_index], &[]);
            pass.draw(0..6, 0..self.rects.len() as u32);
            // (v223) La marge autour de l'écran : ce qui va y entrer en tournant.
            if margin_now && t.margin.0 + t.margin.1 > 0 {
                pass.set_pipeline(if self.vl_noise {
                    &pipes.margin
                } else {
                    &pipes.margin_plain
                });
                pass.draw(0..6, 0..4);
            }
        }
        // (v223) Reprise d'une image à l'autre (lumières posées), lissage (portées) : toute
        // l'image des rayons, un triangle.
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("lumière volumétrique : reprise"),
                color_attachments: &[attach(&t.hist_views[write]), attach(&t.combined_view)],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&pipes.temporal);
            pass.set_bind_group(0, &t.temporal_groups[write][depth_index], &[]);
            pass.draw(0..3, 0..1);
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
            timestamp_writes: ts(crate::gpu_timer::VL_COMPOSITE),
            occlusion_query_set: None,
        });
        pass.set_pipeline(&pipes.composite);
        pass.set_bind_group(0, &t.comp_groups[depth_index], &[]);
        pass.draw(0..6, 0..self.rects.len() as u32);
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

    /// (v217) La lampe torche, au milieu de la grille de blocs mais plus longue qu'elle,
    /// refait sa carte à chaque fois qu'elle bouge ; une lumière posée que la grille ne voit
    /// pas toute garde la sienne en attendant.
    #[test]
    fn a_carried_light_always_rebakes_its_map() {
        let origin = [-64, 40, -64];
        let mut lamp = spot(9, [0.5, 72.0, 0.5], [0.0, -0.2, -1.0], 128.0, 45.0);
        assert!(
            !grid_covers(origin, &lamp),
            "la grille ne voit pas toute la lampe"
        );
        assert!(
            keeps_old_map(true, &lamp, Some(origin)),
            "sans porteur : gardée"
        );
        lamp.owner = 1;
        assert!(
            !keeps_old_map(true, &lamp, Some(origin)),
            "portée : refaite"
        );
        assert!(!keeps_old_map(false, &lamp, Some(origin)));
        let far = spot(10, [300.0, 72.0, 0.5], [0.0, -1.0, 0.0], 20.0, 45.0);
        assert!(keeps_old_map(true, &far, Some(origin)));
        assert!(
            !keeps_old_map(false, &far, Some(origin)),
            "une première carte se calcule"
        );
    }

    /// (v222) Une lumière posée que la grille ne voit pas toute garde sa carte tant qu'elle
    /// ne change pas (des blocs changés près d'elle) ; tournée (un projecteur mis tout droit
    /// vers le bas, loin de la caméra), sa carte d'avant ne vaut plus rien : refaite.
    #[test]
    fn a_turned_light_rebakes_even_out_of_the_grid() {
        let origin = [-64, 40, -64];
        let mut far = spot(10, [300.0, 72.0, 0.5], [0.13, -0.99, -0.08], 20.0, 45.0);
        let mut c = ShadowCache::default();
        c.reset(16);
        let s = c.request(&far, 1).unwrap();
        s.baked = true;
        s.baked_key = Some(s.key);
        s.dirty = true; // un bloc a changé près d'elle
        assert!(keeps_old_map(c.slots[&10].current(), &far, Some(origin)));
        far.dir = [0.0, -1.0, 0.0];
        let s = c.request(&far, 2).unwrap();
        assert!(s.dirty && !s.current());
        assert!(!keeps_old_map(s.current(), &far, Some(origin)), "refaite");
    }

    /// (v222) Le repère d'un projecteur tourne avec son cap jusqu'à la verticale, sans
    /// à-coup : 360° sans souci, gobo et carte d'ombre compris.
    #[test]
    fn the_spot_frame_turns_smoothly_down_to_vertical() {
        let cap = 58f32.to_radians();
        let dir = |pitch: f32| {
            let p = pitch.to_radians();
            Vec3::new(cap.sin() * p.cos(), p.sin(), -cap.cos() * p.cos())
        };
        let (r0, _, _) = spot_basis(dir(-60.0));
        for k in 0..3000 {
            let pitch = -60.0 - k as f32 * 0.01; // jusqu'à −89,99°
            let (r, u, f) = spot_basis(dir(pitch));
            assert!((r - r0).length() < 1e-4, "{pitch} : {r} / {r0}");
            assert!(r.dot(u).abs() < 1e-4 && r.dot(f).abs() < 1e-4 && u.dot(f).abs() < 1e-4);
            assert!((r.length() - 1.0).abs() < 1e-4 && (u.length() - 1.0).abs() < 1e-4);
        }
        // Tout à fait vertical : un repère quand même.
        let (r, u, _) = spot_basis(Vec3::NEG_Y);
        assert!(r.is_finite() && u.is_finite() && r.length() > 0.99);
    }

    /// (v223) Les rayons repris d'une image à l'autre : l'échantillon de la marche visite tout
    /// son carré en huit images (moyenne au centre), et la marge autour de l'écran reçoit les
    /// lumières posées à rayons qui la touchent, pas les lumières portées.
    #[test]
    fn the_rays_prepare_a_margin_around_the_screen() {
        let pts: Vec<(f32, f32)> = (0..8).map(vl_jitter).collect();
        let sum = pts.iter().fold((0.0, 0.0), |a, p| (a.0 + p.0, a.1 + p.1));
        let mean = (sum.0 / 8.0, sum.1 / 8.0);
        assert!(mean.0.abs() < 0.07 && mean.1.abs() < 0.07, "{mean:?}");
        assert!(pts.iter().all(|p| p.0.abs() <= 0.5 && p.1.abs() <= 0.5));
        for q in [0.0f32, 0.5] {
            assert!(pts.iter().any(|p| (p.0.abs() <= 0.25) == (q == 0.0)));
        }
        assert_eq!(vl_margin((400, 225)), (24, 14));
        // Une caméra vers −z ; un projecteur à rayons juste à droite du champ (dans la
        // marge), un autre porté au même endroit, un troisième derrière la caméra.
        let proj = Mat4::perspective_infinite_reverse_rh(75f32.to_radians(), 16.0 / 9.0, 0.05);
        let eye = Vec3::new(0.0, 64.0, 0.0);
        let vp = proj * Mat4::look_to_rh(eye, Vec3::NEG_Z, Vec3::Y);
        let mut side = spot(1, [0.0, 64.0, 0.0], [0.0, -1.0, 0.0], 3.0, 30.0);
        let half_h = (75f32.to_radians() * 0.5).tan() * 16.0 / 9.0;
        // Hors de l'écran, juste au-delà de son bord droit (dans la marge), à 30 blocs.
        side.pos = [30.0 * half_h * 1.08 + 1.5, 64.0, -30.0];
        side.beam = 1.0;
        let mut held = side;
        held.id = 2;
        held.owner = 1;
        let mut behind = side;
        behind.id = 3;
        behind.pos = [0.0, 64.0, 40.0];
        let lights = [side, held, behind];
        let mut tiles = vec![0u32; ((TILES_X * TILES_Y + 1) * WORDS) as usize];
        bin_tiles(&lights, vp, eye, &mut tiles);
        let m = (TILES_X * TILES_Y * WORDS) as usize;
        assert_eq!(tiles[m] & 0b111, 0b001, "{:b}", tiles[m]);
        let on_screen: u32 = tiles[..m].iter().fold(0, |a, w| a | w);
        assert_eq!(on_screen & 1, 0, "hors de l'écran");
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
        // (v223) L'image des rayons d'une image à l'autre, dans les deux shaders.
        assert_eq!(size("VlFrame"), std::mem::size_of::<VlFrameGpu>());
        let comp = naga::front::wgsl::parse_str(include_str!("shaders/vl_composite.wgsl"))
            .expect("WGSL de la recomposition");
        let vf = comp
            .types
            .iter()
            .find(|(_, t)| t.name.as_deref() == Some("VlFrame"))
            .unwrap();
        assert_eq!(
            vf.1.inner.size(comp.to_ctx()) as usize,
            std::mem::size_of::<VlFrameGpu>()
        );
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
    }

    #[test]
    fn side_images_are_written_whole() {
        // Rayons (lumière et profondeur de chaque couche), éclairage en demi-résolution :
        // tous les canaux. L'image du monde : sans son alpha.
        assert_eq!(write_mask(VL_FORMAT), wgpu::ColorWrites::ALL);
        assert_eq!(write_mask(HALF_LIGHT_FORMAT), wgpu::ColorWrites::ALL);
        assert_eq!(write_mask(COLOR_FORMAT), wgpu::ColorWrites::COLOR);
    }

    #[test]
    fn moving_the_grid_redoes_only_what_enters_it() {
        use crate::rt::VOX;
        let [vx, vy, vz] = VOX.map(|v| v as i32);
        // Le même monde vu par deux grilles : la seconde a avancé de 8 blocs en x.
        let world = |x: i32, y: i32, z: i32| ((x * 7 + y * 13 + z * 3).rem_euclid(5) as u16) * 2;
        let grid = |o: [i32; 3]| {
            let mut ids = Vec::with_capacity((vx * vy * vz) as usize);
            for z in 0..vz {
                for y in 0..vy {
                    for x in 0..vx {
                        ids.push(world(o[0] + x, o[1] + y, o[2] + z));
                    }
                }
            }
            ids
        };
        let o = [-40, 64, 24];
        let o2 = [o[0] + 8, o[1], o[2]];
        let a = brick_hashes(o, &grid(o));
        let b = brick_hashes(o2, &grid(o2));
        let ch = changed_bricks(&a, &b);
        // Seulement la tranche qui entre (aucun bloc n'a changé, rien de ce qui sort).
        assert_eq!(ch.len(), (vy / 8 * vz / 8) as usize);
        assert!(ch.iter().all(|(lo, _)| lo[0] == o[0] + vx), "{ch:?}");
    }

    #[test]
    fn a_far_grid_does_not_cover_the_light() {
        use crate::rt::VOX;
        let o = [0, 64, 0];
        let mid = [
            VOX[0] as f32 * 0.5,
            64.0 + VOX[1] as f32 * 0.5,
            VOX[2] as f32 * 0.5,
        ];
        // Projecteur vers le bas au milieu de la grille : vu en entier.
        let s = spot(
            1,
            [mid[0], mid[1] + 12.0, mid[2]],
            [0.0, -1.0, 0.0],
            26.0,
            58.0,
        );
        assert!(grid_covers(o, &s));
        // Le même, 100 blocs plus loin, ou 30 blocs plus haut : la grille ne le voit pas.
        let mut far = s;
        far.pos[0] += 100.0;
        assert!(!grid_covers(o, &far));
        let mut high = s;
        high.pos[1] += 30.0;
        assert!(!grid_covers(o, &high));
        // Lumière plus grande que la grille : couverte quand la grille est centrée sur elle.
        let mut big = s;
        big.kind = DynKind::Point;
        big.range = 60.0;
        big.pos = mid;
        assert!(grid_covers(o, &big));
        big.pos[2] += 20.0;
        assert!(!grid_covers(o, &big));
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
            false,
            true,
        );
        // Ombres douces décochées : arrêt net par les murs.
        assert_eq!(g.shadow[2], 1.0);
        assert_eq!(g.dir_type, [0.0, 0.0, -1.0, 1.0]);
        assert!((g.cone[0] - 30f32.to_radians().cos()).abs() < 1e-6);
        assert!((g.cone[1] - g.cone[0]).abs() < 1e-6);
        assert_eq!(g.cone[2], 2.0);
        assert_eq!(g.cookie, [3.0, 0.0, 1.0, 1.0]);
        assert_eq!(g.color, [2.0, 1.0, 0.0, 2.0]);
        assert_eq!((g.shadow[0], g.vol[3]), (1.0, 5.0));
        assert_eq!(g.boxes[0], 0b101);
        // (v220) Des joueurs à portée dans l'atlas des joueurs.
        assert_eq!(g.boxes[3], 1);
        // Repère du projecteur, case et sphère : passés tels quels au shader ; le repère
        // est celui des cartes d'ombre (`spot_basis`).
        let (r, u, _) = spot_basis(Vec3::new(0.0, 0.0, -1.0));
        assert_eq!(&g.right[..3], &r.to_array());
        assert_eq!(&g.up[..3], &u.to_array());
        assert!((g.right[3] - 1.0 / 30f32.to_radians().tan()).abs() < 1e-4);
        assert_eq!((g.up[3], g.rect[3]), (1.0, 0.0));
        assert_eq!(&g.rect[..3], &[0.25, 0.5, 0.0625]);
        // La lumière est dans la sphère des boîtes : elles sont toujours essayées.
        assert_eq!(g.box_sphere, [0.0, 0.0, 0.0, -2.0]);
        assert_eq!(f32::from_bits(g.boxes[2]), -1.0);
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
            true,
            false,
        );
        assert_eq!(p.cookie[0], -1.0);
        assert_eq!(p.boxes[3], 0);
        assert_eq!(p.shadow[2], 0.0);
        assert_eq!((p.shadow[0], p.vol[3]), (0.0, -1.0));
        assert_eq!(p.dir_type[3], 0.0);
    }

    /// Les tris des ombres des joueurs (cône vu de la lumière, sphère de chaque boîte)
    /// n'écartent jamais un trajet que l'essai exact trouve caché : même image, moins de
    /// calcul. Les essais du shader refaits ici sur des milliers de trajets au hasard, près de
    /// l'origine et loin d'elle.
    #[test]
    fn the_box_shadow_culling_never_drops_a_shadow() {
        let mut seed = 0x9e37_79b9u32;
        let mut rnd = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed as f32 / u32::MAX as f32
        };
        // L'essai exact du shader (`dl_box_shadow`), point hors des boîtes.
        let exact = |b: &crate::ShadowBox, p: Vec3, lp: Vec3| {
            let row = |i: usize, q: Vec3| glam::Vec4::from(b.to_unit[i]).dot(q.extend(1.0));
            let a = Vec3::new(row(0, p), row(1, p), row(2, p));
            let d = Vec3::new(row(0, lp), row(1, lp), row(2, lp)) - a;
            let inv = Vec3::select(
                d.abs().cmplt(Vec3::splat(1e-6)),
                Vec3::splat(1e6),
                d.recip(),
            );
            let (t0, t1) = (-a * inv, (Vec3::ONE - a) * inv);
            let near = t0.min(t1).max_element();
            let far = t0.max(t1).min_element();
            far >= near.max(0.0) && near < 1.0
        };
        let mut hidden = 0;
        for origin in [
            Vec3::new(0.0, 64.0, 0.0),
            Vec3::new(30_000.5, 90.0, -20_000.25),
        ] {
            for _ in 0..60 {
                // Un joueur : jambes, corps, tête, tournés avec lui.
                let feet = origin + Vec3::new(rnd() * 4.0 - 2.0, 0.0, rnd() * 4.0 - 2.0);
                let xf = Mat4::from_translation(feet)
                    * Mat4::from_rotation_y(rnd() * std::f32::consts::TAU);
                let boxes: Vec<crate::ShadowBox> = [
                    ([-0.25, 0.0, -0.12], [0.25, 0.75, 0.12]),
                    ([-0.25, 0.75, -0.12], [0.25, 1.5, 0.12]),
                    ([-0.25, 1.5, -0.25], [0.25, 2.0, 0.25]),
                ]
                .iter()
                .map(|(lo, hi)| crate::ShadowBox::oriented(xf, Vec3::from(*lo), Vec3::from(*hi)))
                .collect();
                let lp =
                    feet + Vec3::new(rnd() * 24.0 - 12.0, rnd() * 10.0 + 0.3, rnd() * 24.0 - 12.0);
                let (cone, near) = box_cone(lp, boxes_sphere(&boxes, 0b111));
                let axis = Vec3::new(cone[0], cone[1], cone[2]);
                let spheres: Vec<[f32; 4]> = boxes.iter().map(box_bound).collect();
                for _ in 0..400 {
                    // Un point de la surface autour du joueur (sol, murs proches), et le point
                    // essayé à 2 centièmes de lui, comme dans `dl_one`.
                    let n = [Vec3::Y, Vec3::X, Vec3::NEG_Z][(rnd() * 2.99) as usize];
                    let p =
                        feet + Vec3::new(rnd() * 8.0 - 4.0, rnd() * 3.0 - 0.6, rnd() * 8.0 - 4.0);
                    let q = p + n * 0.02;
                    if !boxes.iter().any(|b| exact(b, q, lp)) {
                        continue;
                    }
                    hidden += 1;
                    let to_l = lp - p;
                    let d = to_l.length();
                    let l = to_l / d;
                    assert!(
                        (-l).dot(axis) >= cone[3] && d >= near,
                        "cône : {p:?} → {lp:?}"
                    );
                    let seg = lp - q;
                    let inv = 1.0 / seg.length_squared().max(1e-8);
                    let kept = boxes.iter().zip(&spheres).any(|(b, s)| {
                        let v = Vec3::new(s[0], s[1], s[2]) - q;
                        let off = seg * (v.dot(seg) * inv).clamp(0.0, 1.0) - v;
                        off.length_squared() <= s[3] * s[3] && exact(b, q, lp)
                    });
                    assert!(kept, "sphères : {p:?} → {lp:?}");
                }
            }
        }
        assert!(
            hidden > 500,
            "trop peu de trajets cachés essayés : {hidden}"
        );
    }

    /// (v201) Une lampe torche n'a pas l'ombre de celui qui la tient, mais celle des autres
    /// joueurs ; une lumière à personne a l'ombre de tous.
    #[test]
    fn a_held_light_is_not_shadowed_by_its_holder() {
        let b = |a: [f32; 3], c: [f32; 3], owner: u32| crate::ShadowBox {
            min: a,
            max: c,
            to_unit: [[0.0; 4]; 3],
            sunlit: true,
            owner,
        };
        // (v202) Le troisième morceau (sa main) est aussi tout contre la flamme de sa torche
        // (bit 2).
        let boxes = [
            b([0.0, 0.0, 0.0], [1.0, 2.0, 1.0], 1),
            b([3.0, 0.0, 0.0], [4.0, 2.0, 1.0], 0),
            b([0.0, 2.0, 0.0], [1.0, 3.0, 1.0], 1 | 2),
            b([60.0, 0.0, 0.0], [61.0, 2.0, 1.0], 0),
        ];
        let mut l = spot(9, [0.5, 1.5, 0.5], [1.0, 0.0, 0.0], 20.0, 30.0);
        assert_eq!(box_mask(&l, &boxes), 0b0111, "trop loin : pas d'ombre");
        // Sa lampe torche : aucun de ses morceaux.
        l.owner = 1;
        assert_eq!(box_mask(&l, &boxes), 0b0010);
        // Sa torche tenue : son corps fait son ombre, pas sa main.
        l.owner = 2;
        assert_eq!(box_mask(&l, &boxes), 0b0011);
    }

    #[test]
    fn the_box_sphere_holds_every_box_of_the_mask() {
        let b = |a: [f32; 3], c: [f32; 3]| crate::ShadowBox {
            min: a,
            max: c,
            to_unit: [[0.0; 4]; 3],
            sunlit: true,
            owner: 0,
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
