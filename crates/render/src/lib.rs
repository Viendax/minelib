//! Rendu via `wgpu` (Vulkan, DirectX 12 ou Metal selon la plateforme).
//!
//! - surface + tampon de profondeur, recréés au redimensionnement ;
//! - un tampon de sommets / d'indices par chunk ;
//! - élimination de ce qui est hors du champ de vision (frustum culling) et tri
//!   de l'avant vers l'arrière (moins de pixels calculés pour rien) ;
//! - profondeur en « Z inversé » avec projection infinie : précision quasi constante
//!   jusqu'à plusieurs kilomètres, sans plan lointain ;
//! - trois passes dans un même render pass : ciel, blocs, réticule.

mod arena;
mod capture;
mod entity;
mod gpu_timer;
pub mod igpu;
mod occlusion;
mod reflections;
mod smoke;
pub use wgpu;
mod latency;
mod lazy;
mod lights;
mod overlay;
pub mod perf;
pub mod rt;
mod screen;
mod taa;
pub mod textures;
pub mod thermal;
pub mod viewmodel;

pub use capture::Screenshot;

#[doc(hidden)]
pub use entity::draw_offscreen;
pub use entity::{
    EntityVertex, PlayerInstance, PlayerVertex, FLAME as ENTITY_FLAME, MAX_PLAYERS, PLAYER_SKINS,
};
pub use latency::{LowLatencyKind, Marker};
pub use lights::{
    bounding_sphere, DynKind, DynLight, DynSettings, DynState, DynStats, GoboImage, GOBO_SIZE,
    MAX_DYN_LIGHTS, MAX_GOBOS, SHADOW_QUALITY, SHADOW_QUALITY_NAMES,
};
pub use overlay::{set_translator, tr, Line, Overlay, Ui};
pub use screen::ScreenAnchor;
pub use smoke::{SmokeQuality, SmokeVolume};
pub use viewmodel::{ArmVertex, VmVertex, FLAME, NO_TEXTURE};

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec3, Vec4};
use minelib_assets::ResourcePack;
use minelib_world::mesh::{MeshData, Vertex};
use minelib_world::ChunkPos;
use winit::window::Window;

const VERTEX_ATTRS: [wgpu::VertexAttribute; 3] =
    wgpu::vertex_attr_array![0 => Float32x3, 1 => Uint32, 2 => Uint32];

/// Disposition des sommets des blocs.
pub(crate) fn world_vertex_layout() -> wgpu::VertexBufferLayout<'static> {
    wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<Vertex>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &VERTEX_ATTRS,
    }
}

/// Un pipeline du monde qui dessine dans l'image de l'écran (sans anticrénelage).
#[allow(clippy::too_many_arguments)]
fn surface_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    format: wgpu::TextureFormat,
    label: &str,
    vs: &str,
    fs: &str,
    buffers: &[wgpu::VertexBufferLayout],
    cull: Option<wgpu::Face>,
    depth_write: bool,
    depth_compare: wgpu::CompareFunction,
    blend: Option<wgpu::BlendState>,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some(vs),
            compilation_options: Default::default(),
            buffers,
        },
        primitive: wgpu::PrimitiveState {
            // Le contour du bloc visé est fait de lignes.
            topology: if vs == "vs_outline" {
                wgpu::PrimitiveTopology::LineList
            } else {
                wgpu::PrimitiveTopology::TriangleList
            },
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: cull,
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: depth_write,
            depth_compare,
            stencil: Default::default(),
            bias: Default::default(),
        }),
        multisample: Default::default(),
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some(fs),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview: None,
        cache: None,
    })
}

/// Passe des reflets (raytracing) : surfaces réfléchissantes seulement, dans l'image des
/// reflets.
fn reflect_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
) -> wgpu::RenderPipeline {
    let vertex_layout = world_vertex_layout();
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("reflets (raytracing)"),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_block"),
            compilation_options: Default::default(),
            buffers: std::slice::from_ref(&vertex_layout),
        },
        primitive: wgpu::PrimitiveState {
            cull_mode: Some(wgpu::Face::Back),
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: true,
            depth_compare: wgpu::CompareFunction::Greater,
            stencil: Default::default(),
            bias: Default::default(),
        }),
        multisample: Default::default(),
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some("fs_reflect"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: reflections::FORMAT,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview: None,
        cache: None,
    })
}

/// Pipelines d'une passe du monde : le ciel et le contour tout de suite, les shaders
/// lourds (blocs, miroirs, translucides) quand ils sont compilés.
struct ScenePipes<'p> {
    sky: &'p wgpu::RenderPipeline,
    outline: &'p wgpu::RenderPipeline,
    heavy: Option<&'p taa::Heavy>,
    /// Translucides éclairés par les lumières de l'éditeur (sinon : ceux de `heavy`).
    translucent: Option<&'p wgpu::RenderPipeline>,
    /// Anticrénelage temporel (passes à plusieurs cibles).
    taa: bool,
}

pub(crate) const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// Pipelines des lumières de l'éditeur : éclairage différé, cartes d'ombre, rayons.
/// Compilés sur des fils à part la première fois qu'il y a une lumière. Petits : le shader
/// du monde n'en a plus besoin (il laisse albédo et surface à l'éclairage différé).
struct LitPipes {
    deferred: wgpu::RenderPipeline,
    bake: wgpu::RenderPipeline,
    vl: lights::VlPipes,
    /// Bras à la première personne éclairé par les lumières.
    arm: wgpu::RenderPipeline,
}

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
struct Globals {
    view_proj: [[f32; 4]; 4],
    /// Gardée pour la disposition des uniformes, mais à ne pas lire : retrouver une
    /// direction ou une position avec l'inverse fait tourner ou sauter l'image (voir
    /// [`view_ray`]).
    inv_view_proj: [[f32; 4]; 4],
    cam_pos: [f32; 4],
    fog: [f32; 4],
    sun: [f32; 4],
    sel_min: [f32; 4],
    sel_max: [f32; 4],
    light: [f32; 4],
    cur_view_proj: [[f32; 4]; 4],
    prev_view_proj: [[f32; 4]; 4],
    screen: [f32; 4],
    held_pos: [f32; 4],
    held_color: [f32; 4],
    /// Raytracing : x = activé, yzw = coin bas de la grille de blocs.
    rt: [f32; 4],
    /// Sources de lumière proches (nombre dans `light[2]`), voir `PointLight`.
    lights_pos: [[f32; 4]; MAX_LIGHTS],
    lights_col: [[f32; 4]; MAX_LIGHTS],
    mirror_pos: [[f32; 4]; MAX_MIRROR_LIGHTS],
    mirror_col: [[f32; 4]; MAX_MIRROR_LIGHTS],
    mirror_plane: [[f32; 4]; MAX_MIRROR_LIGHTS],
    /// Reflets calculés à part : vue-projection et caméra de leur dernier calcul.
    refl_vp: [[f32; 4]; 4],
    refl_cam: [f32; 4],
    /// Modèle du joueur dans les reflets : nombre de pavés et lumière reçue (rgb).
    ray_info: [f32; 4],
    ray_inv: [[[f32; 4]; 4]; MAX_RAY_BOXES],
    ray_min: [[f32; 4]; MAX_RAY_BOXES],
    ray_max: [[f32; 4]; MAX_RAY_BOXES],
    ray_uv: [[f32; 4]; MAX_RAY_BOXES],
    /// Ombres des joueurs : x = nombre de boîtes ; coins min et max (monde).
    shadow_info: [f32; 4],
    shadow_min: [[f32; 4]; MAX_SHADOW_BOXES],
    shadow_max: [[f32; 4]; MAX_SHADOW_BOXES],
}

/// Rayon de la caméra qui passe par le point `ndc` de l'écran, à l'échelle d'une unité de
/// profondeur de vue (composante vers l'avant = 1), tiré des lignes de `view_proj`.
///
/// PIÈGE (ciel en v108, projecteurs en v175) : ne jamais retrouver une direction ou une
/// position avec l'inverse de `view_proj` (un point « proche » ou « lointain » dé-projeté,
/// moins la caméra). La projection du jeu n'a pas de plan lointain (Z inversé, à l'infini)
/// et les coordonnées du monde sont grandes : les arrondis de l'inversion et de la
/// soustraction faussent la direction, et l'image tourne ou saute quand on bouge la vue.
/// Même calcul côté carte graphique : `sky_dir` (world.wgsl), `dl_view_ray` (dynlights.wgsl).
/// Le test `nothing_rebuilds_the_view_with_the_inverse_matrix` refuse tout retour en arrière.
pub fn view_ray(view_proj: Mat4, ndc: [f32; 2]) -> Vec3 {
    let row = |r: usize| {
        Vec3::new(
            view_proj.x_axis[r],
            view_proj.y_axis[r],
            view_proj.z_axis[r],
        )
    };
    let (rx, ry, rw) = (row(0), row(1), row(3));
    let d = (rx - ndc[0] * rw).cross(ry - ndc[1] * rw);
    d / rw.dot(d)
}

/// `view_proj` comptée depuis la caméra : `eye_relative(m) * (p - œil, 1)` donne le même
/// point de l'écran que `m * (p, 1)`, sans la perte de précision des grandes coordonnées
/// (la projection du jeu, sans plan lointain, envoie l'œil en (0, 0, proche, 0)).
pub fn eye_relative(view_proj: Mat4) -> Mat4 {
    Mat4::from_cols(
        view_proj.x_axis,
        view_proj.y_axis,
        view_proj.z_axis,
        Vec4::new(0.0, 0.0, view_proj.w_axis.z, 0.0),
    )
}

/// Nombre maximal de boîtes (morceaux de joueurs) qui font de l'ombre.
pub const MAX_SHADOW_BOXES: usize = 48;

/// Morceau de joueur qui fait de l'ombre : sa boîte exacte (tournée avec lui), et la
/// boîte alignée sur les axes du monde qui la contient.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShadowBox {
    /// Boîte englobante dans le monde : ombres du soleil et des torches, lumières
    /// dynamiques qui l'atteignent.
    pub min: [f32; 3],
    pub max: [f32; 3],
    /// Les trois lignes de la matrice qui ramène le monde au cube unité de la boîte exacte
    /// (0 à 1 sur chaque axe) : ombres exactes des lumières dynamiques.
    pub to_unit: [[f32; 4]; 3],
    /// Le soleil l'atteint (aucun bloc entre elle et lui) : sinon, pas d'ombre du soleil.
    pub sunlit: bool,
}

impl ShadowBox {
    /// Boîte `lo..hi` d'un repère local placé dans le monde par `xf` (local → monde).
    pub fn oriented(xf: Mat4, lo: Vec3, hi: Vec3) -> Self {
        let size = (hi - lo).max(Vec3::splat(1e-6));
        let to_unit = Mat4::from_scale(size.recip()) * Mat4::from_translation(-lo) * xf.inverse();
        let mut min = Vec3::splat(f32::MAX);
        let mut max = Vec3::splat(f32::MIN);
        for k in 0..8 {
            let c = Vec3::new(
                if k & 1 == 0 { lo.x } else { hi.x },
                if k & 2 == 0 { lo.y } else { hi.y },
                if k & 4 == 0 { lo.z } else { hi.z },
            );
            let w = xf.transform_point3(c);
            min = min.min(w);
            max = max.max(w);
        }
        Self {
            min: min.into(),
            max: max.into(),
            to_unit: [0, 1, 2].map(|i| to_unit.row(i).to_array()),
            sunlit: true,
        }
    }
}

/// Uniformes des ombres des joueurs : (nombre, coins min, coins max).
fn shadow_globals(
    boxes: &[ShadowBox],
    lights: &[[f32; 4]],
    held: Option<[f32; 3]>,
) -> (
    [f32; 4],
    [[f32; 4]; MAX_SHADOW_BOXES],
    [[f32; 4]; MAX_SHADOW_BOXES],
) {
    let n = boxes.len().min(MAX_SHADOW_BOXES);
    let mut mn = [[0.0; 4]; MAX_SHADOW_BOXES];
    let mut mx = [[0.0; 4]; MAX_SHADOW_BOXES];
    for (i, b) in boxes.iter().take(n).enumerate() {
        let (a, bb) = (b.min, b.max);
        // Source de lumière la plus proche (à moins de 10 blocs) : la seule dont ce
        // morceau de joueur projette l'ombre (−1 : aucune, −2 : la torche tenue en main).
        // Le shader n'a ainsi qu'un essai par boîte.
        let c = [0, 1, 2].map(|k| (a[k] + bb[k]) / 2.0);
        let d2 = |l: &[f32]| -> f32 { (0..3).map(|k| (l[k] - c[k]).powi(2)).sum() };
        let near = lights
            .iter()
            .enumerate()
            .map(|(j, l)| (j as f32, d2(l)))
            .chain(held.map(|h| (-2.0, d2(&h))))
            .filter(|(_, d2)| *d2 < 100.0)
            .min_by(|x, y| x.1.total_cmp(&y.1));
        // Partie fractionnaire 0,5 : le soleil n'atteint pas la boîte (pas son ombre).
        let shade = if b.sunlit { 0.0 } else { 0.5 };
        mn[i] = [a[0], a[1], a[2], near.map_or(-1.0, |(j, _)| j) + shade];
        mx[i] = [bb[0], bb[1], bb[2], 0.0];
    }
    // Boule qui contient toutes les boîtes (centre dans `info.yzw`, rayon dans
    // `max[0].w`) : le shader écarte d'un seul essai les points qu'aucune ombre ne peut
    // toucher (presque tous), au lieu d'essayer chaque boîte.
    let mut info = [n as f32, 0.0, 0.0, 0.0];
    if n > 0 {
        let lo: [f32; 3] =
            std::array::from_fn(|k| mn[..n].iter().map(|b| b[k]).fold(f32::MAX, f32::min));
        let hi: [f32; 3] =
            std::array::from_fn(|k| mx[..n].iter().map(|b| b[k]).fold(f32::MIN, f32::max));
        let r = (0..3).map(|k| (hi[k] - lo[k]).powi(2)).sum::<f32>().sqrt() / 2.0;
        info = [
            n as f32,
            (lo[0] + hi[0]) / 2.0,
            (lo[1] + hi[1]) / 2.0,
            (lo[2] + hi[2]) / 2.0,
        ];
        mx[0][3] = r;
    }
    (info, mn, mx)
}

/// Nombre maximal de pavés du joueur vus dans les reflets.
pub const MAX_RAY_BOXES: usize = 8;

/// Sorte de pavé du joueur dans les reflets (couleurs calculées par le shader).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RayBoxKind {
    Head,
    Body,
    Arm,
    Leg,
    /// Morceau de bloc tenu : couche de texture, rectangle de la texture (u0, v0, u1, v1)
    /// plaqué sur chaque face, et `glow` s'il s'éclaire lui-même (bâton d'une torche).
    Block {
        layer: u32,
        uv: [f32; 4],
        glow: bool,
    },
    /// Objet tenu : couleur unie (sRGB).
    Flat([u8; 3]),
    /// Lumineux (flamme d'une torche, bâton éclairé par elle) : couleur finale, sans
    /// l'éclairage du lieu.
    Glow([u8; 3]),
    /// Flamme d'une torche tenue (couleur de la flamme, sRGB) : comme `Glow` dans les
    /// reflets ; sur le modèle, animée comme la torche à la première personne.
    Flame([u8; 3]),
}

/// Pavé du modèle du joueur pour les reflets : `world_to_local` ramène un point du monde
/// dans le repère du pavé (en pixels du skin), où il occupe `min`..`max`.
#[derive(Clone, Copy, Debug)]
pub struct RayBox {
    pub world_to_local: Mat4,
    pub min: [f32; 3],
    pub max: [f32; 3],
    pub kind: RayBoxKind,
    /// Partie du corps dans le skin (`Renderer::set_skin`) : 1 jambe gauche, 2 jambe
    /// droite, 3 corps, 4 bras gauche, 5 bras droit, 6 tête ; 0 : dessin par défaut.
    pub part: u8,
}

/// Uniformes des pavés du joueur : (nombre et lumière, matrices, coins min, coins max).
type RayGlobals = (
    [f32; 4],
    [[[f32; 4]; 4]; MAX_RAY_BOXES],
    [[f32; 4]; MAX_RAY_BOXES],
    [[f32; 4]; MAX_RAY_BOXES],
    [[f32; 4]; MAX_RAY_BOXES],
);

fn ray_globals(boxes: &[RayBox], light: [f32; 3]) -> RayGlobals {
    let n = boxes.len().min(MAX_RAY_BOXES);
    let mut inv = [[[0.0; 4]; 4]; MAX_RAY_BOXES];
    let mut mn = [[0.0; 4]; MAX_RAY_BOXES];
    let mut mx = [[0.0; 4]; MAX_RAY_BOXES];
    let mut uv = [[0.0, 0.0, 1.0, 1.0]; MAX_RAY_BOXES];
    for (i, b) in boxes.iter().take(n).enumerate() {
        inv[i] = b.world_to_local.to_cols_array_2d();
        let (kind, extra) = match b.kind {
            RayBoxKind::Head => (0.0, b.part as f32),
            RayBoxKind::Body => (1.0, b.part as f32),
            RayBoxKind::Arm => (2.0, b.part as f32),
            RayBoxKind::Leg => (3.0, b.part as f32),
            RayBoxKind::Block {
                layer,
                uv: rect,
                glow,
            } => {
                uv[i] = rect;
                (if glow { 7.0 } else { 4.0 }, layer as f32)
            }
            RayBoxKind::Flat(c) => (
                5.0,
                (c[0] as u32 * 65536 + c[1] as u32 * 256 + c[2] as u32) as f32,
            ),
            RayBoxKind::Glow(c) | RayBoxKind::Flame(c) => (
                6.0,
                (c[0] as u32 * 65536 + c[1] as u32 * 256 + c[2] as u32) as f32,
            ),
        };
        mn[i] = [b.min[0], b.min[1], b.min[2], kind];
        mx[i] = [b.max[0], b.max[1], b.max[2], extra];
    }
    ([n as f32, light[0], light[1], light[2]], inv, mn, mx, uv)
}

/// Nombre maximal de reflets de sources de lumière par les miroirs.
pub const MAX_MIRROR_LIGHTS: usize = 16;

/// Image d'une source de lumière derrière un miroir plan : la lumière renvoyée par le
/// miroir éclaire les blocs comme si elle venait de là.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MirrorLight {
    pub pos: [f32; 3],
    pub rgb: [u8; 3],
    pub reach: f32,
    /// Plan du miroir : axe (0 x, 1 y, 2 z), coordonnée, côté éclairé (+1 / −1).
    pub axis: u8,
    pub coord: f32,
    pub side: f32,
}

/// Nombre maximal de sources de lumière envoyées au shader (les plus proches).
pub const MAX_LIGHTS: usize = 32;

/// Source de lumière ponctuelle (flamme de torche, bloc lumineux) : reflets et éclairage
/// des surfaces vues dans les miroirs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PointLight {
    pub pos: [f32; 3],
    /// Rayon de la flamme visible dans les reflets (0 = bloc lumineux, déjà dans la grille).
    pub size: f32,
    /// Couleur (sRGB, intensité 0..255 par canal).
    pub rgb: [u8; 3],
    /// Portée (blocs).
    pub reach: f32,
}

/// Paramètres d'une image, fournis par le jeu.
pub struct FrameParams<'a> {
    /// Position de la caméra (les yeux, ou derrière le joueur à la 3e personne).
    pub eye: Vec3,
    /// Yeux du joueur (torche tenue en main), même à la 3e personne.
    pub player_eye: Vec3,
    pub view: Mat4,
    pub fov_y: f32,
    pub fog_start: f32,
    pub fog_end: f32,
    /// Boîte (coins min, max, en coordonnées monde) du bloc visé.
    pub selected: Option<([f32; 3], [f32; 3])>,
    pub time: f32,
    /// Horloge du ciel (dérive des nuages) : n'avance que si l'heure avance.
    pub sky_time: f32,
    /// Lumière du jour : 1 = plein jour, 0 = nuit.
    pub daylight: f32,
    /// Direction vers le soleil (unitaire).
    pub sun: [f32; 3],
    /// Interface (inventaire, barre d'objets…) dessinée par-dessus tout.
    pub ui: Option<&'a Ui>,
    /// Main et objet tenu (triangles déjà projetés).
    pub viewmodel: &'a [VmVertex],
    /// Bras à la première personne (dessiné avant la main), éclairé comme le modèle du
    /// joueur, lumières dynamiques comprises.
    pub arm: &'a [ArmVertex],
    /// Entités dans le monde (modèle du joueur à la 3e personne), coordonnées monde.
    pub entities: &'a [EntityVertex],
    /// Les premiers sommets d'`entities` suivent la caméra (modèle du joueur local) ; les
    /// autres sont fixés dans le monde.
    pub entities_follow: usize,
    /// Entités fixes qui changent rarement (têtes par zone, cadres) : (identifiant,
    /// triangles, version) ; chaque lot n'est renvoyé à la carte graphique que quand sa
    /// version change.
    pub static_entities: &'a [(u64, &'a [EntityVertex], u64)],
    /// (v172) Les autres joueurs : un modèle commun (`Renderer::set_player_mesh`) posé et
    /// éclairé par instance, avec leur skin (`Renderer::set_player_skin`).
    pub players: &'a [PlayerInstance],
    /// Torche tenue en main : couleur (sRGB) et portée en blocs.
    pub held_light: Option<([u8; 3], f32)>,
    /// Flamme de la torche tenue (bout de la torche dans la main du modèle du joueur) ;
    /// sinon un peu devant les yeux.
    pub held_light_pos: Option<Vec3>,
    /// Le joueur (et l'objet tenu) vu dans les reflets, et la lumière qu'il reçoit.
    pub ray_boxes: &'a [RayBox],
    /// Fumigènes à dessiner en volume.
    pub smokes: &'a [SmokeVolume<'a>],
    pub ray_light: [f32; 3],
    /// Morceaux des joueurs qui font de l'ombre (soleil, torches, lumières dynamiques).
    pub shadow_boxes: &'a [ShadowBox],
    /// Écran de debug (F3) à afficher par-dessus la scène.
    pub overlay: Option<&'a Overlay>,
}

#[derive(Default, Debug, Clone, Copy)]
pub struct FrameStats {
    pub chunks_drawn: usize,
    pub chunks_total: usize,
    pub triangles: usize,
}

/// Identifiant d'un maillage sur le GPU.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum MeshKey {
    Chunk(ChunkPos),
}

struct ChunkGpu {
    /// Emplacement dans les grands tampons partagés (voir `arena`).
    slot: arena::Slot,
    count: u32,
    /// Triangles qui reflètent, rangés après les opaques, puis les translucides.
    reflective: u32,
    /// Morceaux non vides des opaques, dans l'ordre du tampon : (direction 0..6, tranche,
    /// début relatif, nombre d'indices). Tranche 255 : toujours dessiné (pas de découpage).
    ranges: Vec<(u8, u8, u32, u32)>,
    /// Triangles translucides, rangés après les opaques dans le même tampon d'indices.
    translucent: u32,
    bytes: u64,
    min: Vec3,
    max: Vec3,
}

/// Commandes de dessin indirect d'un chunk (faces tournées vers la caméra, tranches
/// visibles), ajoutées à `cmds` ; `groups` : (page, première commande, nombre). Renvoie le
/// nombre de triangles dessinés.
fn chunk_commands(
    c: &ChunkGpu,
    mask: u32,
    eye: Vec3,
    cmds: &mut Vec<u8>,
    groups: &mut Vec<(usize, u32, u32)>,
) -> u64 {
    // Directions de faces qui peuvent être tournées vers la caméra : une face +X n'est
    // visible que si l'œil est du côté +X de son plan, donc au-delà du bord −X du chunk.
    let visible = [
        eye.x > c.min.x,
        eye.x < c.max.x,
        eye.y > c.min.y,
        eye.y < c.max.y,
        eye.z > c.min.z,
        eye.z < c.max.z,
        true,
    ];
    let mut push = |cmds: &mut Vec<u8>, start: u32, count: u32| {
        let n = (cmds.len() / 20) as u32;
        match groups.last_mut() {
            Some(g) if g.0 == c.slot.page => g.2 += 1,
            _ => groups.push((c.slot.page, n, 1)),
        }
        let args = wgpu::util::DrawIndexedIndirectArgs {
            index_count: count,
            instance_count: 1,
            first_index: start,
            base_vertex: c.slot.vstart as i32,
            first_instance: 0,
        };
        cmds.extend_from_slice(args.as_bytes());
    };
    // Groupes consécutifs visibles fusionnés en une seule commande.
    let mut run: Option<(u32, u32)> = None;
    let mut tris = 0u64;
    let base = c.slot.istart as u32;
    for &(k, sec, rel, n) in &c.ranges {
        let (y0, y1) = (sec as f32 * 16.0, sec as f32 * 16.0 + 16.0);
        let facing = match k {
            2 if sec != 255 => eye.y > y0,
            3 if sec != 255 => eye.y < y1,
            _ => visible[k as usize],
        };
        let shown = facing && (sec == 255 || mask & (1 << sec) != 0);
        let first = base + rel;
        if shown {
            run = Some(match run {
                Some((s, len)) if s + len == first => (s, len + n),
                Some((s, len)) => {
                    push(cmds, s, len);
                    (first, n)
                }
                None => (first, n),
            });
            tris += n as u64 / 3;
        } else if let Some((s, len)) = run.take() {
            push(cmds, s, len);
        }
    }
    if let Some((s, len)) = run {
        push(cmds, s, len);
    }
    tris
}

/// Commandes d'une part des chunks calculée sur un cœur : (commandes, groupes par page,
/// triangles).
type CmdPart = (Vec<u8>, Vec<(usize, u32, u32)>, u64);

/// Clé du cache de tri : vue-projection, caméra, distance max, versions des maillages et
/// de la visibilité des grottes.
type CullKey = ([f32; 16], [f32; 3], f32, u64, u64);

pub struct Renderer {
    /// Reflex / Anti-Lag 2 (déclaré en premier : libéré avant le device).
    latency: Option<latency::LowLatency>,
    /// Capture d'écran demandée (F2), relue quelques images plus tard.
    capture: capture::Capture,
    /// Anticrénelage temporel (TAA), si activé.
    taa: Option<taa::Taa>,
    world_layout: wgpu::PipelineLayout,
    world_shader: wgpu::ShaderModule,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    depth: wgpu::TextureView,
    globals: wgpu::Buffer,
    bgl: wgpu::BindGroupLayout,
    block_sampler: wgpu::Sampler,
    /// Groupes de liaison du monde ; `bind_groups[i]` lit l'image de reflets `i`.
    bind_groups: [wgpu::BindGroup; 2],
    block_view: wgpu::TextureView,
    refl: reflections::Reflections,
    pub pack_name: String,
    /// Pipelines du monde sans anticrénelage (essais) : compilés à la première utilisation.
    plain: std::sync::OnceLock<taa::Heavy>,
    /// Passe des reflets : prête un peu après le lancement.
    reflect_pipeline: lazy::Lazy<wgpu::RenderPipeline>,
    sky_pipeline: wgpu::RenderPipeline,
    outline_pipeline: wgpu::RenderPipeline,
    /// Le contour du bloc visé est-il à dessiner ?
    outline: bool,
    meshes: HashMap<MeshKey, ChunkGpu>,
    arena: arena::Arena,
    /// Commandes de dessin indirect de l'image (une par chunk visible), et leur tampon.
    indirect: wgpu::Buffer,
    indirect_cap: u64,
    /// Groupes (page, première commande, nombre) de la liste opaque.
    page_groups: Vec<(usize, u32, u32)>,
    /// La carte sait dessiner plusieurs maillages en un appel (`MULTI_DRAW_INDIRECT`).
    multi_draw: bool,
    /// Images restantes pendant lesquelles l'anticrénelage considère la scène modifiée.
    mesh_changes: u32,
    draw_list: Vec<(f32, MeshKey)>,
    overlay: overlay::OverlayRenderer,
    viewmodel: viewmodel::ViewModel,
    entities: entity::Entities,
    screen: screen::Screen,
    /// Fumigènes en volume.
    smokes: smoke::Smokes,
    voxels: rt::Voxels,
    /// Lumières dynamiques (éditeur de lumières) : ombres, volumétrique, gobos.
    dyn_gpu: lights::DynGpu,
    /// Leurs shaders, compilés à la première lumière posée (`Err` : refusés par la carte ou
    /// son pilote ; le jeu continue sans elles).
    lit: Option<lazy::Lazy<Result<LitPipes, String>>>,
    /// Translucides (eau, glace, verre) éclairés par les lumières : le shader du monde avec
    /// leur code, compilé ensuite (le plus long) ; d'ici là, les translucides ne prennent
    /// pas leur lumière (l'opaque derrière, si).
    lit_translucent: Option<lazy::Lazy<Result<wgpu::RenderPipeline, String>>>,
    /// Images restantes pendant lesquelles l'anticrénelage considère que les lumières ont
    /// changé (déplacées, réglées) : comme `mesh_changes`.
    dyn_changes: u32,
    /// Skin des reflets envoyé à la carte (voir `set_skin`).
    skin_key: Option<usize>,
    gpu_timer: Option<gpu_timer::GpuTimer>,
    cull_order: Vec<usize>,
    cull_cmds: Vec<u8>,
    /// Dernier tri (caméra, chunks, visibilité) et son résultat, réutilisés si rien n'a changé.
    cull_key: Option<CullKey>,
    cull_stats: FrameStats,
    mesh_version: u64,
    vis_version: u64,
    /// Échelle de rendu du monde (0,5 … 1).
    render_scale: f32,
    /// Triangles opaques réellement envoyés à la dernière image (faces cachées retirées).
    drawn_triangles: u64,
    /// Passages par l'air des tranches de chaque chunk, tranches visibles calculées depuis
    /// la caméra (voir `occlusion`), et calcul en cours.
    conn: HashMap<ChunkPos, [u64; minelib_world::mesh::SECTIONS]>,
    vis: HashMap<ChunkPos, u32>,
    vis_cam: Option<(ChunkPos, i32)>,
    vis_dirty: bool,
    vis_job: Option<std::sync::mpsc::Receiver<HashMap<ChunkPos, u32>>>,
    vis_launch: Option<std::time::Instant>,
    cave_culling: bool,
    /// Reflets calculés à leur propre rythme (désactivé : synchronisés avec chaque image).
    async_reflections: bool,
    lights_pos: [[f32; 4]; MAX_LIGHTS],
    lights_col: [[f32; 4]; MAX_LIGHTS],
    light_count: usize,
    mirror_lights: [[[f32; 4]; MAX_MIRROR_LIGHTS]; 3],
    mirror_count: usize,
    /// Reflets en raytracing (option graphique).
    raytracing: bool,
    /// Qualité du raytracing ajustée au temps GPU mesuré.
    rt_quality: rt::RtQuality,
    pub adapter_name: String,
    /// Type de la carte qui dessine (dédiée, intégrée, logicielle…).
    pub device_type: wgpu::DeviceType,
    pub backend: String,
}

impl Renderer {
    pub fn new(window: Arc<Window>, pack: &ResourcePack) -> Result<Self> {
        pollster::block_on(Self::new_async(window, pack))
    }

    /// Essaie chaque API graphique tour à tour : si les shaders ne compilent pas avec l'une
    /// (pilote, compilateur DirectX), on passe à la suivante au lieu de planter.
    async fn new_async(window: Arc<Window>, pack: &ResourcePack) -> Result<Self> {
        let mut attempts = Vec::new();
        if cfg!(windows) && std::env::var_os("WGPU_BACKEND").is_none() {
            let base = wgpu::InstanceDescriptor::from_env_or_default();
            attempts.push(wgpu::InstanceDescriptor {
                backends: wgpu::Backends::DX12,
                ..base.clone()
            });
            attempts.push(wgpu::InstanceDescriptor {
                backends: wgpu::Backends::all() - wgpu::Backends::DX12,
                ..base
            });
        } else {
            attempts.push(wgpu::InstanceDescriptor::from_env_or_default());
        }
        let mut last_err = anyhow::anyhow!("aucun GPU compatible trouvé");
        for desc in attempts {
            let backends = desc.backends;
            match Self::try_backends(window.clone(), pack, desc).await {
                Ok(r) => return Ok(r),
                Err(e) => {
                    log::error!("rendu impossible avec {backends:?} : {e:#}");
                    last_err = e;
                }
            }
        }
        Err(last_err)
    }

    async fn try_backends(
        window: Arc<Window>,
        pack: &ResourcePack,
        desc: wgpu::InstanceDescriptor,
    ) -> Result<Self> {
        let size = window.inner_size();
        // Sous Windows : DirectX 12 d'abord (Reflex et Anti-Lag 2 passent par lui),
        // puis Vulkan / les autres API si le GPU ou le pilote refuse DX12.
        let attempts = [desc];
        let mut found = None;
        let mut last_err = anyhow::anyhow!("aucun GPU compatible trouvé");
        for desc in attempts {
            let instance = wgpu::Instance::new(&desc);
            let surface = match instance.create_surface(window.clone()) {
                Ok(s) => s,
                Err(e) => {
                    last_err = anyhow::Error::new(e).context("création de la surface");
                    continue;
                }
            };
            let Ok(adapter) = instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::HighPerformance,
                    compatible_surface: Some(&surface),
                    force_fallback_adapter: false,
                })
                .await
            else {
                continue;
            };
            let device = adapter
                .request_device(&wgpu::DeviceDescriptor {
                    label: Some("minelib"),
                    // Plusieurs maillages en un seul appel de dessin, si la carte le permet.
                    // Temps GPU par passe (journal des performances), si disponible.
                    required_features: adapter.features()
                        & (wgpu::Features::MULTI_DRAW_INDIRECT | wgpu::Features::TIMESTAMP_QUERY),
                    required_limits: wgpu::Limits {
                        // Une couche de texture par image de bloc (plus de 256 avec les mondes importés).
                        max_texture_array_layers: adapter.limits().max_texture_array_layers,
                        // Grands tampons de chunks (voir `arena`).
                        max_buffer_size: adapter.limits().max_buffer_size,
                        ..wgpu::Limits::downlevel_defaults().using_resolution(adapter.limits())
                    },
                    memory_hints: wgpu::MemoryHints::Performance,
                    trace: wgpu::Trace::Off,
                })
                .await;
            match device {
                Ok((device, queue)) => {
                    found = Some((surface, adapter, device, queue));
                    break;
                }
                Err(e) => {
                    log::warn!(
                        "{:?} refusé ({e}), essai de l'API suivante",
                        adapter.get_info().backend
                    );
                    last_err = anyhow::Error::new(e).context("création du device");
                }
            }
        }
        let (surface, adapter, device, queue) = found.ok_or(last_err)?;
        // Erreurs de création (shaders, pipelines) récupérées ici plutôt qu'un plantage.
        device.push_error_scope(wgpu::ErrorFilter::Validation);
        device.push_error_scope(wgpu::ErrorFilter::Internal);
        let info = adapter.get_info();
        let adapter_name = info.name.clone();
        let backend = format!("{:?}", info.backend);
        log::info!("GPU : {adapter_name} ({backend})");

        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(caps.formats[0]);
        // Sans synchro verticale : immédiat (le plus de FPS) si possible, sinon « mailbox ».
        let present_mode = [
            wgpu::PresentMode::Immediate,
            wgpu::PresentMode::Mailbox,
            wgpu::PresentMode::Fifo,
        ]
        .into_iter()
        .find(|m| caps.present_modes.contains(m))
        .unwrap_or(wgpu::PresentMode::Fifo);
        log::info!(
            "affichage : {present_mode:?} (modes possibles : {:?})",
            caps.present_modes
        );
        // Image de la fenêtre lisible (captures d'écran) quand la carte le permet.
        let usage =
            wgpu::TextureUsages::RENDER_ATTACHMENT | (caps.usages & wgpu::TextureUsages::COPY_SRC);
        let config = wgpu::SurfaceConfiguration {
            usage,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            // Pas de synchro verticale : FPS illimités.
            present_mode,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            // 3 images d'avance : le processeur prépare la suivante pendant que la carte
            // dessine, sans attendre (plus de FPS ; Reflex réduit la latence s'il est actif).
            desired_maximum_frame_latency: 3,
        };
        surface.configure(&device, &config);
        let depth = create_depth(&device, &config);

        let globals = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("globals"),
            size: std::mem::size_of::<Globals>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let dyn_entries = lights::world_layout_entries();
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("monde"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                // Raytracing : grille de blocs (identifiants) et palette de couleurs.
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Uint,
                        view_dimension: wgpu::TextureViewDimension::D3,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Uint,
                        view_dimension: wgpu::TextureViewDimension::D1,
                        multisampled: false,
                    },
                    count: None,
                },
                // Dernière image des reflets (voir `reflections`).
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                // Skin du joueur, pour les reflets.
                wgpu::BindGroupLayoutEntry {
                    binding: 6,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                // Lumières dynamiques (voir `lights`) : 7 à 13.
                dyn_entries[0],
                dyn_entries[1],
                dyn_entries[2],
                dyn_entries[3],
                dyn_entries[4],
                dyn_entries[5],
                dyn_entries[6],
            ],
        });
        // Pixel art : agrandissement net, réduction lissée entre les mipmaps.
        let block_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("blocs"),
            mag_filter: wgpu::FilterMode::Nearest,
            // Réduction lissée (trilinéaire) : plus de scintillement des textures au loin.
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let block_tex = textures::load_block_textures(pack);
        let voxels = rt::Voxels::new(&device);
        voxels.upload_palette(&queue, &block_tex);
        let block_view = upload_block_texture(&device, &queue, &block_tex);
        let refl = reflections::Reflections::new(&device, (1, 1));
        let mut dyn_gpu = lights::DynGpu::new(&device, &queue);
        dyn_gpu.bind_bake(&device, &voxels, &block_view, &block_sampler);
        let bind_groups = world_groups(
            &device,
            &bgl,
            &globals,
            &block_sampler,
            &block_view,
            &voxels,
            &refl,
            &dyn_gpu,
        );
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("main"),
            bind_group_layouts: &[&bgl],
            push_constant_ranges: &[],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("monde"),
            source: wgpu::ShaderSource::Wgsl(
                lights::without_dynlights(include_str!("shaders/world.wgsl")).into(),
            ),
        });

        // Reflets (raytracing à son propre rythme) : shader lourd, compilé sur un fil à part
        // sans retarder l'ouverture du jeu. Les pipelines du monde sans anticrénelage ne
        // servent qu'aux essais : compilés à la demande (`plain_pipes`).
        let reflect_pipeline = {
            let (device, layout, shader) = (device.clone(), layout.clone(), shader.clone());
            lazy::Lazy::spawn("shader des reflets", move || {
                let t0 = std::time::Instant::now();
                let p = reflect_pipeline(&device, &layout, &shader);
                log::info!(
                    "shader des reflets compilé en {:.1} s (en tâche de fond)",
                    t0.elapsed().as_secs_f32()
                );
                p
            })
        };
        let make = |label: &str, vs: &str, fs: &str, compare, blend| {
            surface_pipeline(
                &device,
                &layout,
                &shader,
                config.format,
                label,
                vs,
                fs,
                &[],
                None,
                false,
                compare,
                blend,
            )
        };
        let sky_pipeline = make(
            "ciel",
            "vs_sky",
            "fs_sky",
            wgpu::CompareFunction::Always,
            None,
        );
        let outline_pipeline = make(
            "contour",
            "vs_outline",
            "fs_outline",
            wgpu::CompareFunction::GreaterEqual,
            Some(wgpu::BlendState::ALPHA_BLENDING),
        );

        let atlas = textures::build_gui_atlas(pack, &overlay::font_bitmap());
        let overlay =
            overlay::OverlayRenderer::new(&device, &queue, config.format, DEPTH_FORMAT, &atlas);

        let viewmodel = viewmodel::ViewModel::new(&device, &bgl, config.format);
        let entities = entity::Entities::new(&device, &bgl, config.format);
        let screen = screen::Screen::new(&device, &bgl, config.format);
        let smokes = smoke::Smokes::new(&device, &queue, &bgl, config.format);
        let indirect = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("dessins indirects"),
            size: 20 * 4096,
            usage: wgpu::BufferUsages::INDIRECT | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let multi_draw = device
            .features()
            .contains(wgpu::Features::MULTI_DRAW_INDIRECT);
        log::info!(
            "dessin indirect multiple : {}",
            if multi_draw {
                "oui"
            } else {
                "non (un appel par chunk)"
            }
        );
        let latency = latency::LowLatency::detect(&device, &info);
        let gpu_timer = gpu_timer::GpuTimer::new(&device, &queue);
        log::info!(
            "mesure du temps GPU : {}",
            if gpu_timer.is_some() { "oui" } else { "non" }
        );
        let mut r = Self {
            gpu_timer,
            cull_order: Vec::new(),
            cull_cmds: Vec::new(),
            cull_key: None,
            cull_stats: FrameStats::default(),
            mesh_version: 0,
            vis_version: 0,
            render_scale: 1.0,
            drawn_triangles: 0,
            conn: HashMap::new(),
            vis: HashMap::new(),
            vis_cam: None,
            vis_dirty: true,
            vis_job: None,
            vis_launch: None,
            cave_culling: true,
            async_reflections: false,
            latency,
            capture: capture::Capture::default(),
            taa: None,
            world_layout: layout,
            world_shader: shader,
            overlay,
            viewmodel,
            entities,
            screen,
            smokes,
            voxels,
            dyn_gpu,
            lit: None,
            lit_translucent: None,
            dyn_changes: 0,
            skin_key: None,
            lights_pos: [[0.0; 4]; MAX_LIGHTS],
            lights_col: [[0.0; 4]; MAX_LIGHTS],
            light_count: 0,
            mirror_lights: [[[0.0; 4]; MAX_MIRROR_LIGHTS]; 3],
            mirror_count: 0,
            raytracing: false,
            rt_quality: rt::RtQuality::default(),
            backend,
            surface,
            device,
            queue,
            config,
            depth,
            globals,
            bgl,
            block_sampler,
            bind_groups,
            block_view,
            refl,
            pack_name: pack.name.clone(),
            plain: std::sync::OnceLock::new(),
            reflect_pipeline,
            sky_pipeline,
            outline_pipeline,
            outline: false,
            meshes: HashMap::new(),
            arena: arena::Arena::default(),
            indirect,
            indirect_cap: 4096,
            page_groups: Vec::new(),
            multi_draw,
            mesh_changes: 0,
            draw_list: Vec::new(),
            adapter_name,
            device_type: info.device_type,
        };
        // L'anticrénelage est toujours actif : ses pipelines sont créés ici, sous contrôle.
        r.set_antialiasing(true);
        let internal = r.device.pop_error_scope().await;
        let validation = r.device.pop_error_scope().await;
        if let Some(e) = internal.or(validation) {
            anyhow::bail!("{} : {e}", r.backend);
        }
        Ok(r)
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return; // fenêtre réduite
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
        self.depth = create_depth(&self.device, &self.config);
    }

    pub fn aspect(&self) -> f32 {
        self.config.width as f32 / self.config.height as f32
    }

    pub fn size(&self) -> (u32, u32) {
        (self.config.width, self.config.height)
    }

    pub fn has_mesh(&self, key: MeshKey) -> bool {
        self.meshes.contains_key(&key)
    }

    /// Envoie (ou remplace) le maillage d'un chunk sur le GPU.
    pub fn upload_chunk(&mut self, pos: ChunkPos, mesh: &MeshData) {
        self.upload(MeshKey::Chunk(pos), mesh);
        // Passages par l'air de ses tranches : servent à cacher les grottes fermées.
        match mesh.connectivity {
            Some(c) => {
                if self.conn.insert(pos, c) != Some(c) {
                    self.vis_dirty = true;
                }
            }
            None => {
                self.conn.remove(&pos);
            }
        }
    }

    pub fn remove_chunk(&mut self, pos: ChunkPos) {
        self.drop_mesh(MeshKey::Chunk(pos));
    }

    fn drop_mesh(&mut self, key: MeshKey) {
        if let Some(c) = self.meshes.remove(&key) {
            self.arena.free(c.slot);
            self.mesh_changes = 8;
            self.mesh_version += 1;
        }
        let MeshKey::Chunk(p) = key;
        if self.conn.remove(&p).is_some() {
            self.vis_dirty = true;
        }
    }

    /// Grottes cachées : relance le calcul des tranches visibles (en arrière-plan) quand la
    /// caméra change de tranche ou que des chunks ont changé, et récupère le résultat.
    fn update_visibility(&mut self, eye: Vec3) {
        if let Some(rx) = &self.vis_job {
            if let Ok(v) = rx.try_recv() {
                self.vis = v;
                self.vis_version += 1;
                self.vis_job = None;
                perf::event(|| "tranches visibles recalculées (grottes cachées)".into());
            }
        }
        if !self.cave_culling {
            if !self.vis.is_empty() {
                self.vis.clear();
                self.vis_version += 1;
            }
            return;
        }
        let cam = (
            ChunkPos::containing(eye.x.floor() as i32, eye.z.floor() as i32),
            (eye.y.floor() as i32).div_euclid(16),
        );
        let moved = self.vis_cam != Some(cam);
        let waited = self
            .vis_launch
            .is_none_or(|t| t.elapsed() > std::time::Duration::from_millis(100));
        if (moved || self.vis_dirty) && self.vis_job.is_none() && (moved || waited) {
            let conn = self.conn.clone();
            let (tx, rx) = std::sync::mpsc::channel();
            let spawned = std::thread::Builder::new()
                .name("minelib-grottes".into())
                .spawn(move || {
                    let _ = tx.send(occlusion::visible_sections(&conn, cam.0, cam.1));
                });
            if spawned.is_ok() {
                self.vis_job = Some(rx);
                self.vis_cam = Some(cam);
                self.vis_dirty = false;
                self.vis_launch = Some(std::time::Instant::now());
            }
        }
    }

    /// Ne plus dessiner les grottes et salles fermées (activé par défaut).
    pub fn set_cave_culling(&mut self, on: bool) {
        self.cave_culling = on;
        self.vis_dirty = true;
    }

    pub fn cave_culling(&self) -> bool {
        self.cave_culling
    }

    pub fn upload(&mut self, key: MeshKey, mesh: &MeshData) {
        self.drop_mesh(key);
        if mesh.is_empty() {
            return;
        }
        let indices = [
            mesh.indices.as_slice(),
            mesh.reflective.as_slice(),
            mesh.translucent.as_slice(),
        ]
        .concat();
        let Some(slot) = self
            .arena
            .alloc(&self.device, &self.queue, &mesh.vertices, &indices)
        else {
            // Mémoire vidéo pleine (ou maillage géant) : signalé une fois toutes les 1000.
            let n = self.arena.refused;
            if n % 1000 == 1 || n == 0 {
                log::warn!(
                    "maillage non affiché : mémoire vidéo réservée aux chunks pleine ({} Mo) — {} refus",
                    self.arena.bytes() / (1024 * 1024),
                    n
                );
            }
            perf::event(|| "mémoire vidéo des chunks pleine".into());
            return;
        };
        let (mut min, mut max) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
        for v in &mesh.vertices {
            let p = Vec3::from(v.pos);
            min = min.min(p);
            max = max.max(p);
        }
        let bytes = slot.vlen * std::mem::size_of::<Vertex>() as u64 + slot.ilen * 4;
        self.mesh_changes = 8;
        self.mesh_version += 1;
        self.meshes.insert(
            key,
            ChunkGpu {
                slot,
                count: mesh.indices.len() as u32,
                reflective: mesh.reflective.len() as u32,
                ranges: opaque_ranges(mesh),
                translucent: mesh.translucent.len() as u32,
                bytes,
                min,
                max,
            },
        );
    }

    /// Mémoire GPU allouée par le moteur lui-même (chunks, profondeur, police…), en octets.
    pub fn engine_gpu_bytes(&self) -> u64 {
        let chunks: u64 = self.arena.bytes();
        let depth = self.config.width as u64 * self.config.height as u64 * 4;
        chunks
            + depth
            + self.overlay.gpu_bytes
            + std::mem::size_of::<Globals>() as u64
            + self.dyn_gpu.gpu_bytes()
    }

    /// Mémoire GPU des maillages de chunks, en octets.
    pub fn chunk_gpu_bytes(&self) -> u64 {
        self.meshes.values().map(|c| c.bytes).sum()
    }

    pub fn low_latency(&self) -> Option<(LowLatencyKind, bool)> {
        self.latency.as_ref().map(|l| (l.kind(), l.enabled()))
    }

    pub fn raytracing(&self) -> bool {
        self.raytracing
    }

    /// Échelle de rendu du monde : 1 = pleine résolution ; en dessous, le monde est calculé
    /// en plus petit et reconstruit en pleine résolution par l'anticrénelage.
    /// Mémoire vidéo maximale pour les chunks (octets) : au-delà, les chunks lointains ne sont
    /// plus affichés au lieu de faire planter le pilote.
    /// Windows signale la mémoire vidéo presque pleine : plus de nouvelles pages pour les
    /// chunks tant que ça dure (sinon il déplace des données en mémoire vive et la carte se
    /// bloque).
    /// Capture d'écran de la prochaine image affichée (interface comprise).
    pub fn request_screenshot(&mut self) {
        self.capture.request();
    }

    /// Capture prête (quelques images après la demande), sans attendre la carte.
    pub fn take_screenshot(&mut self) -> Option<Screenshot> {
        let shot = self.capture.poll();
        let _ = self.device.poll(wgpu::PollType::Poll);
        shot
    }

    pub fn set_vram_pressure(&mut self, on: bool) {
        self.arena.pressure = on;
    }

    pub fn set_vram_budget(&mut self, bytes: u64) {
        self.arena.budget = bytes;
        log::info!(
            "mémoire vidéo pour les chunks : {} Mo au plus",
            bytes / (1024 * 1024)
        );
    }

    /// Skin du joueur vu dans les miroirs (64 × 64 RGBA) ; `None` : modèle par défaut.
    /// `key` identifie le skin : rien n'est renvoyé à la carte s'il n'a pas changé.
    pub fn set_skin(&mut self, key: usize, rgba: impl FnOnce() -> Option<Vec<u8>>) {
        if self.skin_key != Some(key) {
            self.skin_key = Some(key);
            self.voxels.upload_skin(&self.queue, rgba().as_deref());
        }
    }

    /// (v172) Modèle commun des autres joueurs (une fois au départ).
    pub fn set_player_mesh(&mut self, vertices: &[PlayerVertex]) {
        self.entities.set_player_mesh(&self.queue, vertices);
    }

    /// (v172) Le modèle commun des autres joueurs a été envoyé (à ce renderer : un nouveau
    /// n'a encore ni modèle ni skins).
    pub fn has_player_mesh(&self) -> bool {
        self.entities.has_player_mesh()
    }

    /// (v172) Skin (64 × 64 couleurs linéaires, cuit par le jeu) de la couche `layer` des
    /// autres joueurs (`PLAYER_SKINS` couches).
    pub fn set_player_skin(&mut self, layer: u32, rgb: &[[f32; 3]]) {
        self.entities.set_player_skin(&self.queue, layer, rgb);
    }

    fn world_group(&self) -> &wgpu::BindGroup {
        &self.bind_groups[self.refl.latest]
    }

    fn rebuild_world_groups(&mut self) {
        self.bind_groups = world_groups(
            &self.device,
            &self.bgl,
            &self.globals,
            &self.block_sampler,
            &self.block_view,
            &self.voxels,
            &self.refl,
            &self.dyn_gpu,
        );
        self.dyn_gpu.rebind = false;
    }

    /// Reflets du raytracing recalculés au plus `hz` fois par seconde (rendu principal
    /// indépendant).
    pub fn set_raytracing_rate(&mut self, hz: f32) {
        self.refl.rate = hz.clamp(10.0, 1000.0);
    }

    /// Reflets à leur propre rythme (true) ou à chaque image (false, par défaut).
    pub fn set_async_reflections(&mut self, on: bool) {
        self.async_reflections = on;
        if !on {
            self.refl.valid = false;
        }
    }

    pub fn async_reflections(&self) -> bool {
        self.async_reflections
    }

    /// (calculs de reflets par seconde, fréquence visée) — affiché par F3 (mode décalé).
    pub fn raytracing_fps(&self) -> (f32, f32) {
        (self.refl.fps, self.refl.rate)
    }

    pub fn set_render_scale(&mut self, scale: f32) {
        self.render_scale = scale.clamp(0.5, 1.0);
    }

    pub fn render_scale(&self) -> f32 {
        self.render_scale
    }

    /// Qualité des fumigènes (pas de la marche dans le volume).
    pub fn set_smoke_quality(&mut self, q: SmokeQuality) {
        self.smokes.quality = q;
    }

    pub fn set_raytracing(&mut self, on: bool) {
        self.raytracing = on;
    }

    /// Grille de blocs autour du joueur pour le raytracing (voir `rt::VOX`), et pour les
    /// ombres des lumières dynamiques.
    pub fn set_voxels(&mut self, origin: [i32; 3], ids: &[u16]) {
        self.voxels.upload(&self.queue, origin, ids);
        self.dyn_gpu.on_voxels(origin, ids);
    }

    /// Lumières dynamiques (éditeur de lumières) de la prochaine image, et leurs réglages.
    pub fn set_dyn_lights(&mut self, lights: &[DynLight], settings: &DynSettings) {
        // Leurs shaders : compilés la première fois qu'il y en a une. D'ici là, et s'ils
        // sont refusés, le monde s'affiche sans elles.
        if !lights.is_empty() {
            let _ = self.warm_dyn_lights();
        }
        let ready = self.lit_pipes().is_some();
        if self.dyn_gpu.set(if ready { lights } else { &[] }, settings) {
            self.dyn_changes = 8;
        }
    }

    /// Lance la compilation des shaders des lumières (sur des fils à part), sans attendre
    /// qu'une lumière soit posée : au lancement du jeu chez qui s'en sert, elles sont
    /// prêtes plus tôt. Avec peu de cœurs, seulement après les shaders du monde (pas deux
    /// compilations lourdes à la fois : le monde s'afficherait plus tard). Vrai quand tout est
    /// lancé (ou refusé) ; sinon, à redemander aux images suivantes.
    pub fn warm_dyn_lights(&mut self) -> bool {
        let many_cores = std::thread::available_parallelism().is_ok_and(|n| n.get() >= 8);
        if self.lit.is_none() && (many_cores || self.world_ready()) {
            self.lit = Some(self.spawn_lit());
        }
        // Puis les translucides éclairés (le plus long), une fois les autres prêts.
        if self.lit_pipes().is_some() && self.lit_translucent.is_none() {
            self.lit_translucent = Some(self.spawn_lit_translucent());
        }
        let failed = self
            .lit
            .as_ref()
            .is_some_and(|l| matches!(l.try_get(), Some(Err(_))));
        self.lit_translucent.is_some() || failed
    }

    /// Variantes éclairées, si elles sont prêtes.
    fn lit_pipes(&self) -> Option<&LitPipes> {
        self.lit.as_ref()?.try_get()?.as_ref().ok()
    }

    /// Compile les shaders des lumières sur un fil à part. Les erreurs (compilateur de
    /// DirectX, pilote) sont récupérées ici : elles coûtent les lumières, pas le jeu.
    fn spawn_lit(&self) -> lazy::Lazy<Result<LitPipes, String>> {
        let device = self.device.clone();
        let bgl = self.bgl.clone();
        let surface = self.config.format;
        let (bake_bgl, vl_bgl, comp_bgl, def_bgl) = (
            self.dyn_gpu.bake_bgl.clone(),
            self.dyn_gpu.vl_bgl.clone(),
            self.dyn_gpu.comp_bgl.clone(),
            self.dyn_gpu.def_bgl.clone(),
        );
        lazy::Lazy::spawn("shaders des lumières", move || {
            let t0 = std::time::Instant::now();
            // Essais : compilation lente comme sous DirectX (MINELIB_LUMIERES_LENTES=<s>).
            if let Some(s) = std::env::var("MINELIB_LUMIERES_LENTES")
                .ok()
                .and_then(|v| v.parse::<f32>().ok())
            {
                std::thread::sleep(std::time::Duration::from_secs_f32(s.clamp(0.0, 120.0)));
            }
            device.push_error_scope(wgpu::ErrorFilter::Validation);
            device.push_error_scope(wgpu::ErrorFilter::Internal);
            let (bake, vl, deferred, arm) = lights::light_pipes(
                &device, &bgl, &bake_bgl, &vl_bgl, &comp_bgl, &def_bgl, surface,
            );
            let internal = pollster::block_on(device.pop_error_scope());
            let validation = pollster::block_on(device.pop_error_scope());
            if let Some(e) = internal.or(validation) {
                log::error!("shaders des lumières refusés : lumières désactivées ({e})");
                return Err(e.to_string());
            }
            log::info!(
                "shaders des lumières compilés en {:.1} s (en tâche de fond)",
                t0.elapsed().as_secs_f32()
            );
            Ok(LitPipes {
                deferred,
                bake,
                vl,
                arm,
            })
        })
    }

    /// Translucides éclairés par les lumières, après les autres pipelines des lumières
    /// (jamais deux `push_error_scope` à la fois : la pile est commune à tout le device).
    fn spawn_lit_translucent(&self) -> lazy::Lazy<Result<wgpu::RenderPipeline, String>> {
        let device = self.device.clone();
        let layout = self.world_layout.clone();
        lazy::Lazy::spawn("translucides éclairés", move || {
            let t0 = std::time::Instant::now();
            device.push_error_scope(wgpu::ErrorFilter::Validation);
            device.push_error_scope(wgpu::ErrorFilter::Internal);
            let world = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("monde (lumières)"),
                source: wgpu::ShaderSource::Wgsl(
                    lights::with_dynlights(include_str!("shaders/world.wgsl")).into(),
                ),
            });
            let pipeline = taa::translucent_pipeline(&device, &layout, &world);
            let internal = pollster::block_on(device.pop_error_scope());
            let validation = pollster::block_on(device.pop_error_scope());
            if let Some(e) = internal.or(validation) {
                log::warn!("translucides éclairés refusés : sans les lumières ({e})");
                return Err(e.to_string());
            }
            log::info!(
                "translucides éclairés par les lumières compilés en {:.1} s (en tâche de fond)",
                t0.elapsed().as_secs_f32()
            );
            Ok(pipeline)
        })
    }

    /// Translucides éclairés, s'ils sont prêts.
    fn lit_translucent(&self) -> Option<&wgpu::RenderPipeline> {
        self.lit_translucent.as_ref()?.try_get()?.as_ref().ok()
    }

    /// Les lumières dynamiques ont-elles besoin de la grille de blocs (ombres) ?
    pub fn dyn_lights_want_voxels(&self) -> bool {
        self.dyn_gpu.wants_voxels()
    }

    /// État des lumières dynamiques à la dernière image.
    pub fn dyn_stats(&self) -> DynStats {
        let mut s = self.dyn_gpu.stats;
        s.state = match &self.lit {
            None => DynState::Off,
            Some(l) => match l.try_get() {
                None => DynState::Compiling,
                Some(Ok(_)) => DynState::Ready,
                Some(Err(_)) => DynState::Failed,
            },
        };
        s
    }

    /// Motifs des gobos (une couche chacun, dans l'ordre ; voir `DynLight::gobo`).
    pub fn set_gobos(&mut self, images: &[GoboImage]) {
        self.dyn_gpu.set_gobos(&self.device, &self.queue, images);
        self.rebuild_world_groups();
        self.dyn_changes = 8;
    }

    /// Prépare les lumières dynamiques de l'image (cartes d'ombre à calculer, tuiles).
    fn prepare_dyn(&mut self, view_proj: Mat4, p: &FrameParams) {
        let voxels = self.voxels.uploaded.then_some(self.voxels.origin);
        self.dyn_gpu.prepare(
            &self.device,
            &self.queue,
            view_proj,
            p.eye,
            p.shadow_boxes,
            p.time,
            voxels,
        );
        if self.dyn_gpu.rebind {
            self.rebuild_world_groups();
        }
    }

    /// Sources de lumière proches du joueur (au plus `MAX_LIGHTS`, les premières gardées).
    pub fn set_lights(&mut self, lights: &[PointLight]) {
        let n = lights.len().min(MAX_LIGHTS);
        self.light_count = n;
        for (i, l) in lights.iter().take(n).enumerate() {
            let c = l.rgb.map(|v| (v as f32 / 255.0).powf(2.2) * 1.25);
            self.lights_pos[i] = [l.pos[0], l.pos[1], l.pos[2], l.size];
            self.lights_col[i] = [c[0], c[1], c[2], l.reach];
        }
    }

    /// Images des sources de lumière derrière les miroirs (au plus `MAX_MIRROR_LIGHTS`).
    pub fn set_mirror_lights(&mut self, lights: &[MirrorLight]) {
        let n = lights.len().min(MAX_MIRROR_LIGHTS);
        self.mirror_count = n;
        for (i, l) in lights.iter().take(n).enumerate() {
            let c = l.rgb.map(|v| (v as f32 / 255.0).powf(2.2) * 1.25);
            self.mirror_lights[0][i] = [l.pos[0], l.pos[1], l.pos[2], 0.0];
            self.mirror_lights[1][i] = [c[0], c[1], c[2], l.reach];
            self.mirror_lights[2][i] = [l.axis as f32, l.coord, l.side, 0.0];
        }
    }

    fn rt_globals(&self) -> [f32; 4] {
        let o = self.voxels.origin;
        [
            if self.raytracing { 1.0 } else { 0.0 },
            o[0] as f32,
            o[1] as f32,
            o[2] as f32,
        ]
    }

    pub fn set_low_latency(&mut self, on: bool) {
        let Some(l) = &mut self.latency else { return };
        if l.enabled() == on {
            return;
        }
        l.set_enabled(on);
        // Chaîne d'images neuve : rien de l'état d'avant ne reste chez le pilote (après
        // une attente coupée, le processeur et la carte ne travaillaient plus en même
        // temps, voir `latency`).
        self.surface.configure(&self.device, &self.config);
    }

    /// À appeler en début d'image, avant de lire les entrées du joueur.
    pub fn frame_begin(&mut self) {
        if let Some(l) = &mut self.latency {
            l.frame_begin();
        }
    }

    fn marker(&self, m: Marker) {
        if let Some(l) = &self.latency {
            l.marker(m);
        }
    }

    /// Charge les textures d'un autre pack de ressources (blocs + interface).
    pub fn load_pack(&mut self, pack: &ResourcePack) {
        let block_tex = textures::load_block_textures(pack);
        self.block_view = upload_block_texture(&self.device, &self.queue, &block_tex);
        self.dyn_gpu.bind_bake(
            &self.device,
            &self.voxels,
            &self.block_view,
            &self.block_sampler,
        );
        self.rebuild_world_groups();
        self.voxels.upload_palette(&self.queue, &block_tex);
        let atlas = textures::build_gui_atlas(pack, &overlay::font_bitmap());
        self.overlay.set_atlas(&self.device, &self.queue, &atlas);
        self.pack_name = pack.name.clone();
        let names: Vec<&str> = minelib_world::registry::registry()
            .textures
            .iter()
            .filter_map(|t| match t {
                minelib_world::registry::TexSrc::Pack(n)
                | minelib_world::registry::TexSrc::Tint(n, _)
                | minelib_world::registry::TexSrc::Banner(n, _) => Some(*n),
                minelib_world::registry::TexSrc::Glass(_, _) => Some("glass"),
                _ => None,
            })
            .collect();
        log::info!(
            "pack de ressources : {} ({} / {} textures de blocs fournies par le pack, {} px)",
            pack.name,
            pack.own_block_textures(&names),
            names.len(),
            block_tex.size
        );
    }

    /// Taille d'origine (pixels) d'une image de l'interface.
    pub fn sprite_size(&self, name: &str) -> Option<(u32, u32)> {
        self.overlay.sprite_size(name)
    }

    /// Oublie tous les maillages (on quitte un monde).
    pub fn clear_meshes(&mut self) {
        self.meshes.clear();
        self.conn.clear();
        self.vis.clear();
        self.vis_cam = None;
        self.vis_version += 1;
        self.mesh_version += 1;
        // Les grands tampons sont rendus : le nouveau monde repart de zéro (même budget).
        let budget = self.arena.budget;
        self.arena = arena::Arena {
            budget,
            ..Default::default()
        };
        // Nouveau monde : l'historique de l'anticrénelage ne vaut plus rien.
        if let Some(t) = &mut self.taa {
            t.reset = true;
            t.prev_view_proj = None;
        }
    }

    pub fn antialiasing(&self) -> bool {
        self.taa.is_some()
    }

    /// Les shaders lourds du monde sont-ils compilés ? (Ils le sont en tâche de fond au
    /// lancement ; d'ici là le monde ne montre que le ciel.)
    pub fn world_ready(&self) -> bool {
        match &self.taa {
            Some(t) => t.heavy.try_get().is_some(),
            None => true,
        }
    }

    /// Attend la fin de la compilation des shaders lourds (essais, captures).
    pub fn wait_world_ready(&self) {
        if let Some(t) = &self.taa {
            let _ = t.heavy.get();
        }
    }

    /// Pipelines du monde sans anticrénelage, compilés à la première utilisation (essais).
    fn plain_pipes(&self) -> &taa::Heavy {
        self.plain.get_or_init(|| {
            let vertex_layout = world_vertex_layout();
            let vl = std::slice::from_ref(&vertex_layout);
            let greater = wgpu::CompareFunction::Greater; // Z inversé : 1 = près, 0 = infini
            let (device, layout, shader, format) = (
                &self.device,
                &self.world_layout,
                &self.world_shader,
                self.config.format,
            );
            let mk = |label: &str, fs: &str, cull, depth_write, blend| {
                surface_pipeline(
                    device,
                    layout,
                    shader,
                    format,
                    label,
                    "vs_block",
                    fs,
                    vl,
                    cull,
                    depth_write,
                    greater,
                    blend,
                )
            };
            let back = Some(wgpu::Face::Back);
            std::thread::scope(|sc| {
                let block = sc.spawn(|| mk("blocs", "fs_block_fast", back, true, None));
                // Miroirs et métaux : shader complet (reflets), sur peu de pixels.
                let reflective = sc.spawn(|| mk("reflets", "fs_block", back, true, None));
                // Eau, glace, verre : mélange alpha, sans écrire la profondeur, visibles des
                // deux côtés.
                let translucent = sc.spawn(|| {
                    mk(
                        "translucide",
                        "fs_block_translucent",
                        None,
                        false,
                        Some(wgpu::BlendState::ALPHA_BLENDING),
                    )
                });
                taa::Heavy {
                    block: block.join().expect("pipeline des blocs"),
                    reflective: reflective.join().expect("pipeline réfléchissant"),
                    translucent: translucent.join().expect("pipeline translucide"),
                }
            })
        })
    }

    /// Active ou coupe l'anticrénelage temporel (TAA).
    pub fn set_antialiasing(&mut self, on: bool) {
        if on == self.taa.is_some() {
            return;
        }
        self.taa = on.then(|| {
            taa::Taa::new(
                &self.device,
                &self.world_layout,
                &self.world_shader,
                self.config.format,
            )
        });
    }

    /// Image sans monde : seulement l'interface (menus).
    pub fn render_ui(&mut self, ui: &Ui) {
        let frame = match self.surface.get_current_texture() {
            Ok(f) => f,
            Err(wgpu::SurfaceError::Outdated | wgpu::SurfaceError::Lost) => {
                self.surface.configure(&self.device, &self.config);
                return;
            }
            Err(e) => {
                log::warn!("image ignorée : {e}");
                return;
            }
        };
        let view = frame.texture.create_view(&Default::default());
        self.overlay
            .prepare(&self.device, &self.queue, None, Some(ui), self.size());
        // Objets 3D des menus (logos, icônes de blocs) : sans main ni bras.
        let hand = self
            .viewmodel
            .prepare_with(&self.queue, &[], &self.overlay.models);
        self.viewmodel.prepare_arm(&self.queue, &[]);
        let mut encoder = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("menus"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.01,
                            g: 0.01,
                            b: 0.015,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(0.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            draw_hud(
                &mut pass,
                &self.viewmodel,
                &self.overlay,
                self.world_group(),
                hand,
                None,
            );
        }
        self.capture
            .encode(&self.device, &mut encoder, &frame.texture);
        self.marker(Marker::SimulationEnd);
        self.marker(Marker::RenderSubmitStart);
        self.queue.submit([encoder.finish()]);
        self.marker(Marker::RenderSubmitEnd);
        self.marker(Marker::PresentStart);
        frame.present();
        self.marker(Marker::PresentEnd);
    }

    /// Liste des maillages visibles, triés de l'avant vers l'arrière.
    fn cull(&mut self, view_proj: Mat4, eye: Vec3, max_dist: f32) -> FrameStats {
        self.update_visibility(eye);
        // Rien n'a bougé (caméra, chunks, tranches visibles) : la liste et les commandes de
        // l'image précédente sont encore bonnes, le tampon de dessin aussi.
        let key = (
            view_proj.to_cols_array(),
            eye.to_array(),
            max_dist,
            self.mesh_version,
            self.vis_version,
        );
        if self.cull_key.as_ref() == Some(&key) {
            return self.cull_stats;
        }
        self.cull_key = Some(key);
        let planes = frustum_planes(view_proj);
        let mut stats = FrameStats::default();
        self.draw_list.clear();
        let mut hidden = 0u32;
        for (key, c) in &self.meshes {
            stats.chunks_total += 1;
            // Chunk dont aucune tranche n'est visible depuis la caméra (sous la roche).
            let MeshKey::Chunk(p) = key;
            if self.vis.get(p) == Some(&0) {
                hidden += 1;
                continue;
            }
            // Distance horizontale de l'œil à la boîte.
            let dx = (c.min.x - eye.x).max(0.0).max(eye.x - c.max.x);
            let dz = (c.min.z - eye.z).max(0.0).max(eye.z - c.max.z);
            let d = (dx * dx + dz * dz).sqrt();
            if d > max_dist || !aabb_visible(&planes, c.min, c.max) {
                continue;
            }
            self.draw_list.push((d, *key));
        }
        perf::gauge("chunks entièrement cachés (grottes)", hidden as f32);
        // De l'avant vers l'arrière : le test de profondeur rejette tôt ce qui est caché.
        self.draw_list.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));

        // Commandes de dessin indirect, groupées par page (l'ordre avant → arrière est gardé
        // dans chaque page) : une page = un appel.
        // Tableaux réutilisés d'une image à l'autre (pas d'allocation par image).
        let mut order = std::mem::take(&mut self.cull_order);
        let mut cmds = std::mem::take(&mut self.cull_cmds);
        order.clear();
        cmds.clear();
        order.extend(0..self.draw_list.len());
        if self.arena.pages.len() > 1 {
            order.sort_by_key(|&i| self.meshes[&self.draw_list[i].1].slot.page);
        }
        self.page_groups.clear();
        // Chunks à dessiner, dans l'ordre (page, puis avant → arrière) avec leurs tranches
        // visibles. Les commandes de chaque chunk ne dépendent que de lui : avec beaucoup de
        // chunks (grande distance en pleine résolution), elles sont calculées en parallèle
        // sur plusieurs cœurs puis mises bout à bout dans le même ordre.
        let items: Vec<(&ChunkGpu, u32)> = order
            .iter()
            .map(|&i| {
                let key = self.draw_list[i].1;
                let MeshKey::Chunk(p) = key;
                let mask = self.vis.get(&p).copied().unwrap_or(occlusion::ALL);
                (&self.meshes[&key], mask)
            })
            .filter(|(c, _)| c.count > 0)
            .collect();
        let threads = std::thread::available_parallelism()
            .map_or(1, |n| n.get())
            .min(8);
        let mut drawn_tris = 0u64;
        if items.len() < 1024 || threads < 2 {
            for &(c, mask) in &items {
                drawn_tris += chunk_commands(c, mask, eye, &mut cmds, &mut self.page_groups);
            }
        } else {
            let per = items.len().div_ceil(threads);
            let parts: Vec<CmdPart> = std::thread::scope(|sc| {
                let handles: Vec<_> = items
                    .chunks(per)
                    .map(|part| {
                        sc.spawn(move || {
                            let (mut c_out, mut g_out) =
                                (Vec::with_capacity(part.len() * 80), Vec::new());
                            let mut tris = 0;
                            for &(c, mask) in part {
                                tris += chunk_commands(c, mask, eye, &mut c_out, &mut g_out);
                            }
                            (c_out, g_out, tris)
                        })
                    })
                    .collect();
                handles.into_iter().filter_map(|h| h.join().ok()).collect()
            });
            for (c_part, g_part, tris) in parts {
                let offset = (cmds.len() / 20) as u32;
                for (page, first, n) in g_part {
                    match self.page_groups.last_mut() {
                        Some(g) if g.0 == page && g.1 + g.2 == first + offset => g.2 += n,
                        _ => self.page_groups.push((page, first + offset, n)),
                    }
                }
                cmds.extend_from_slice(&c_part);
                drawn_tris += tris;
            }
        }
        self.drawn_triangles = drawn_tris;
        let n = (cmds.len() / 20) as u64;
        if n > self.indirect_cap {
            self.indirect_cap = n.next_power_of_two();
            self.indirect = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("dessins indirects"),
                size: 20 * self.indirect_cap,
                usage: wgpu::BufferUsages::INDIRECT | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        if !cmds.is_empty() {
            self.queue.write_buffer(&self.indirect, 0, &cmds);
        }
        self.cull_order = order;
        self.cull_cmds = cmds;
        self.cull_stats = stats;

        stats
    }

    /// Triangles qui reflètent des chunks visibles (pipeline déjà choisi).
    fn draw_reflective<'p>(&'p self, pass: &mut wgpu::RenderPass<'p>) {
        let mut bound = usize::MAX;
        for (_, key) in &self.draw_list {
            let c = &self.meshes[key];
            if c.reflective == 0 {
                continue;
            }
            if bound != c.slot.page {
                bound = c.slot.page;
                let pg = &self.arena.pages[bound];
                pass.set_vertex_buffer(0, pg.vertices.slice(..));
                pass.set_index_buffer(pg.indices.slice(..), wgpu::IndexFormat::Uint32);
            }
            let first = c.slot.istart as u32 + c.count;
            pass.draw_indexed(first..first + c.reflective, c.slot.vstart as i32, 0..1);
        }
    }

    /// Ciel, blocs opaques, blocs qui reflètent, contour du bloc visé, translucides
    /// (pipelines : [ciel, blocs, translucides, contour, reflets]).
    /// Ciel, blocs, miroirs, entités, puis contour, écrans vidéo et translucides, dans la
    /// même passe (sans l'anticrénelage).
    fn draw_scene<'p>(
        &'p self,
        pass: &mut wgpu::RenderPass<'p>,
        pipes: ScenePipes<'p>,
        stats: &mut FrameStats,
    ) {
        if self.draw_opaque(pass, &pipes, stats) {
            self.draw_after(pass, &pipes);
        }
    }

    /// Passe des opaques : ciel, blocs, miroirs, entités. Avec l'anticrénelage, elle laisse
    /// aussi albédo et surface pour l'éclairage différé des lumières dynamiques. Faux si les
    /// shaders lourds ne sont pas encore prêts (le ciel seulement).
    fn draw_opaque<'p>(
        &'p self,
        pass: &mut wgpu::RenderPass<'p>,
        pipes: &ScenePipes<'p>,
        stats: &mut FrameStats,
    ) -> bool {
        pass.set_bind_group(0, self.world_group(), &[]);

        pass.set_pipeline(pipes.sky);
        pass.draw(0..3, 0..1);
        // Shaders lourds pas encore compilés (tout début du jeu) : le ciel seulement.
        let Some(heavy) = pipes.heavy else {
            return false;
        };

        // Blocs opaques : une page entière par appel (dessin indirect multiple), sinon un
        // appel par chunk mais sans changer de tampon.
        pass.set_pipeline(&heavy.block);
        for &(page, first, count) in &self.page_groups {
            let pg = &self.arena.pages[page];
            pass.set_vertex_buffer(0, pg.vertices.slice(..));
            pass.set_index_buffer(pg.indices.slice(..), wgpu::IndexFormat::Uint32);
            if self.multi_draw {
                pass.multi_draw_indexed_indirect(&self.indirect, first as u64 * 20, count);
            } else {
                for k in 0..count {
                    pass.draw_indexed_indirect(&self.indirect, (first + k) as u64 * 20);
                }
            }
        }
        for (_, key) in &self.draw_list {
            let c = &self.meshes[key];
            stats.triangles += (c.reflective + c.translucent) as usize / 3;
            stats.chunks_drawn += 1;
        }

        // Miroirs et métaux : seulement les chunks qui en ont, avec le shader des reflets.
        pass.set_pipeline(&heavy.reflective);
        self.draw_reflective(pass);

        // Opaques réellement envoyés (faces tournées vers la caméra seulement).
        stats.triangles += self.drawn_triangles as usize;
        perf::gauge(
            "milliers de triangles par image",
            self.drawn_triangles as f32 / 1000.0,
        );
        // Entités (joueurs, têtes, cadres) avant le verre et l'eau : elles se voient à
        // travers, teintées, au lieu de passer devant.
        self.entities.draw(pass, self.world_group(), pipes.taa);
        true
    }

    /// Après les opaques (et l'éclairage différé) : contour du bloc visé, écrans vidéo,
    /// translucides de l'arrière vers l'avant.
    fn draw_after<'p>(&'p self, pass: &mut wgpu::RenderPass<'p>, pipes: &ScenePipes<'p>) {
        let Some(heavy) = pipes.heavy else {
            return;
        };
        pass.set_bind_group(0, self.world_group(), &[]);
        if self.outline {
            pass.set_pipeline(pipes.outline);
            pass.draw(0..24, 0..1);
        }
        self.screen.draw(pass, pipes.taa);
        // Translucides de l'arrière vers l'avant, par-dessus la scène opaque.
        pass.set_bind_group(0, self.world_group(), &[]);
        pass.set_pipeline(pipes.translucent.unwrap_or(&heavy.translucent));
        let mut bound = usize::MAX;
        for (_, key) in self.draw_list.iter().rev() {
            let c = &self.meshes[key];
            if c.translucent == 0 {
                continue;
            }
            if bound != c.slot.page {
                bound = c.slot.page;
                let pg = &self.arena.pages[bound];
                pass.set_vertex_buffer(0, pg.vertices.slice(..));
                pass.set_index_buffer(pg.indices.slice(..), wgpu::IndexFormat::Uint32);
            }
            let first = c.slot.istart as u32 + c.count + c.reflective;
            pass.draw_indexed(first..first + c.translucent, c.slot.vstart as i32, 0..1);
        }
    }

    pub fn render(&mut self, p: &FrameParams) -> FrameStats {
        if self.taa.is_some() {
            return self.render_taa(p);
        }
        let frame = match self.surface.get_current_texture() {
            Ok(f) => f,
            Err(wgpu::SurfaceError::Outdated | wgpu::SurfaceError::Lost) => {
                self.surface.configure(&self.device, &self.config);
                return FrameStats::default();
            }
            Err(e) => {
                log::warn!("image ignorée : {e}");
                return FrameStats::default();
            }
        };
        let view = frame.texture.create_view(&Default::default());

        let proj = Mat4::perspective_infinite_reverse_rh(p.fov_y, self.aspect(), 0.05);
        let view_proj = proj * p.view;
        self.screen.update(&self.queue);
        let (sel_min, sel_max) = selection(p.selected);
        self.outline = p.selected.is_some();
        let rg = ray_globals(p.ray_boxes, p.ray_light);
        let sg = shadow_globals(
            p.shadow_boxes,
            &self.lights_pos[..self.light_count.min(MAX_LIGHTS)],
            p.held_light.map(|_| {
                let h = held_light(p).0;
                [h[0], h[1], h[2]]
            }),
        );
        let globals = Globals {
            view_proj: view_proj.to_cols_array_2d(),
            inv_view_proj: view_proj.inverse().to_cols_array_2d(),
            cam_pos: [p.eye.x, p.eye.y, p.eye.z, p.time],
            fog: [p.fog_start, p.fog_end, self.aspect(), p.sky_time],
            sun: [p.sun[0], p.sun[1], p.sun[2], 0.48],
            sel_min,
            sel_max,
            light: [
                p.daylight.clamp(0.0, 1.0),
                1.25,
                self.light_count as f32,
                self.mirror_count as f32,
            ],
            cur_view_proj: view_proj.to_cols_array_2d(),
            prev_view_proj: view_proj.to_cols_array_2d(),
            screen: [
                self.config.width as f32,
                self.config.height as f32,
                self.rt_quality.uniforms()[0],
                self.rt_quality.uniforms()[1],
            ],
            held_pos: held_light(p).0,
            held_color: held_light(p).1,
            rt: self.rt_globals(),
            lights_pos: self.lights_pos,
            lights_col: self.lights_col,
            mirror_pos: self.mirror_lights[0],
            mirror_col: self.mirror_lights[1],
            mirror_plane: self.mirror_lights[2],
            refl_vp: self.refl.globals().0,
            refl_cam: self.refl.globals().1,
            ray_info: rg.0,
            ray_inv: rg.1,
            ray_min: rg.2,
            ray_max: rg.3,
            ray_uv: rg.4,
            shadow_info: sg.0,
            shadow_min: sg.1,
            shadow_max: sg.2,
        };
        self.queue
            .write_buffer(&self.globals, 0, bytemuck::bytes_of(&globals));

        self.overlay
            .prepare(&self.device, &self.queue, p.overlay, p.ui, self.size());
        let hand = self
            .viewmodel
            .prepare_with(&self.queue, p.viewmodel, &self.overlay.models);
        self.viewmodel.prepare_arm(&self.queue, p.arm);
        self.entities
            .prepare(&self.queue, p.entities, p.entities_follow);
        self.entities.prepare_players(&self.queue, p.players);
        self.entities
            .prepare_static(&self.device, &self.queue, p.static_entities);
        self.smokes
            .prepare(&self.device, &self.queue, p.smokes, p.eye);
        let mut stats = self.cull(view_proj, p.eye, p.fog_end);

        let mut encoder = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("monde"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(0.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            self.draw_scene(
                &mut pass,
                ScenePipes {
                    sky: &self.sky_pipeline,
                    outline: &self.outline_pipeline,
                    heavy: Some(self.plain_pipes()),
                    translucent: None,
                    taa: false,
                },
                &mut stats,
            );
            self.smokes.draw(&mut pass, self.world_group(), false);
        }
        // Main + objet tenu (profondeur à part), puis l'interface.
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("main et interface"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(0.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            draw_hud(
                &mut pass,
                &self.viewmodel,
                &self.overlay,
                self.world_group(),
                hand,
                None,
            );
        }
        self.capture
            .encode(&self.device, &mut encoder, &frame.texture);
        self.marker(Marker::SimulationEnd);
        self.marker(Marker::RenderSubmitStart);
        self.queue.submit([encoder.finish()]);
        self.marker(Marker::RenderSubmitEnd);
        self.marker(Marker::PresentStart);
        frame.present();
        self.marker(Marker::PresentEnd);
        stats
    }
}

impl Renderer {
    /// Image avec anticrénelage temporel : scène hors écran (couleur + mouvement +
    /// profondeur) avec décalage sous-pixel, résolution TAA, copie à l'écran + interface.
    fn render_taa(&mut self, p: &FrameParams) -> FrameStats {
        let prep = perf::scope("rendu : préparation");
        let size = self.size();
        let Some(mut taa) = self.taa.take() else {
            return FrameStats::default();
        };
        taa.prepare(&self.device, size, self.render_scale);
        // Taille du monde dessiné (plus petite que l'écran si l'échelle de rendu < 100 %).
        let world_size = taa.targets.as_ref().map_or(size, |t| t.internal);
        if self.refl.size != world_size {
            let rate = self.refl.rate;
            self.refl = reflections::Reflections::new(&self.device, world_size);
            self.refl.rate = rate;
            self.rebuild_world_groups();
        }

        let proj = Mat4::perspective_infinite_reverse_rh(p.fov_y, self.aspect(), 0.05);
        let view_proj = proj * p.view;
        self.screen.update(&self.queue);
        let (jx, jy) = taa.jitter();
        let shift = Mat4::from_translation(Vec3::new(
            2.0 * jx / world_size.0 as f32,
            -2.0 * jy / world_size.1 as f32,
            0.0,
        ));
        let jittered = shift * view_proj;
        let prev = taa.prev_view_proj.unwrap_or(view_proj);
        let still = taa
            .prev_view_proj
            .is_some_and(|m| m.abs_diff_eq(view_proj, 1e-6));
        taa.still_frames = if still {
            taa.still_frames.saturating_add(1)
        } else {
            0
        };
        // Chunk chargé ou bloc modifié : quelques images « en mouvement » (pas de fantômes).
        taa.scene_changed = self.mesh_changes > 0;
        if taa.scene_changed {
            self.mesh_changes -= 1;
            taa.still_frames = 0;
        }
        let (sel_min, sel_max) = selection(p.selected);
        self.outline = p.selected.is_some();
        let rg = ray_globals(p.ray_boxes, p.ray_light);
        let sg = shadow_globals(
            p.shadow_boxes,
            &self.lights_pos[..self.light_count.min(MAX_LIGHTS)],
            p.held_light.map(|_| {
                let h = held_light(p).0;
                [h[0], h[1], h[2]]
            }),
        );
        let globals = Globals {
            view_proj: jittered.to_cols_array_2d(),
            inv_view_proj: jittered.inverse().to_cols_array_2d(),
            cam_pos: [p.eye.x, p.eye.y, p.eye.z, p.time],
            fog: [p.fog_start, p.fog_end, self.aspect(), p.sky_time],
            sun: [p.sun[0], p.sun[1], p.sun[2], 0.48],
            sel_min,
            sel_max,
            light: [
                p.daylight.clamp(0.0, 1.0),
                1.25,
                self.light_count as f32,
                self.mirror_count as f32,
            ],
            cur_view_proj: view_proj.to_cols_array_2d(),
            prev_view_proj: prev.to_cols_array_2d(),
            screen: [
                world_size.0 as f32,
                world_size.1 as f32,
                self.rt_quality.uniforms()[0],
                self.rt_quality.uniforms()[1],
            ],
            held_pos: held_light(p).0,
            held_color: held_light(p).1,
            rt: self.rt_globals(),
            lights_pos: self.lights_pos,
            lights_col: self.lights_col,
            mirror_pos: self.mirror_lights[0],
            mirror_col: self.mirror_lights[1],
            mirror_plane: self.mirror_lights[2],
            refl_vp: self.refl.globals().0,
            refl_cam: self.refl.globals().1,
            ray_info: rg.0,
            ray_inv: rg.1,
            ray_min: rg.2,
            ray_max: rg.3,
            ray_uv: rg.4,
            shadow_info: sg.0,
            shadow_min: sg.1,
            shadow_max: sg.2,
        };
        self.overlay
            .prepare(&self.device, &self.queue, p.overlay, p.ui, size);
        let hand = self
            .viewmodel
            .prepare_with(&self.queue, p.viewmodel, &self.overlay.models);
        self.viewmodel.prepare_arm(&self.queue, p.arm);
        self.entities
            .prepare(&self.queue, p.entities, p.entities_follow);
        self.entities.prepare_players(&self.queue, p.players);
        self.entities
            .prepare_static(&self.device, &self.queue, p.static_entities);
        self.smokes
            .prepare(&self.device, &self.queue, p.smokes, p.eye);
        let mut stats = {
            let _c = perf::scope("rendu : tri des chunks");
            self.cull(view_proj, p.eye, p.fog_end)
        };
        {
            let _l = perf::scope("rendu : lumières dynamiques");
            self.prepare_dyn(view_proj, p);
        }
        // Lumière déplacée ou réglée, carte d'ombre recalculée : l'image change alors que la
        // caméra ne bouge pas. Sans cela, l'anticrénelage garde l'ancienne image (caméra
        // immobile : moyenne jusqu'à 1/64, historique accepté tant que la géométrie ne
        // change pas) et la lumière semble ne pas suivre.
        if self.dyn_changes > 0 || self.dyn_gpu.stats.baked > 0 {
            self.dyn_changes = self.dyn_changes.saturating_sub(1);
            taa.scene_changed = true;
            taa.still_frames = 0;
        }
        // Reflets : recalculés à leur rythme, seulement s'il y a des surfaces qui reflètent
        // à l'écran (raytracing activé).
        self.refl.tick();
        let has_reflective = self
            .draw_list
            .iter()
            .any(|(_, k)| self.meshes[k].reflective > 0);
        // Reflets synchronisés (par défaut) : calculés à chaque image sur les surfaces mêmes
        // (`refl_cam.w` reste à 0, le shader lance les rayons directement).
        let do_refl = self.async_reflections
            && self.raytracing
            && has_reflective
            && self.reflect_pipeline.try_get().is_some()
            && self.refl.due();
        let mut globals = globals;
        if do_refl {
            self.refl.ran(jittered.to_cols_array_2d(), p.eye.to_array());
            let (vp, cam) = self.refl.globals();
            globals.refl_vp = vp;
            globals.refl_cam = cam;
        }
        self.queue
            .write_buffer(&self.globals, 0, bytemuck::bytes_of(&globals));
        drop(prep);

        // Image de la fenêtre demandée le plus tard possible : tout le travail CPU ci-dessus
        // se fait pendant que le GPU termine les images précédentes.
        let acquire = perf::scope("rendu : attente de l'image (GPU)");
        let frame = match self.surface.get_current_texture() {
            Ok(f) => f,
            Err(wgpu::SurfaceError::Outdated | wgpu::SurfaceError::Lost) => {
                self.surface.configure(&self.device, &self.config);
                self.taa = Some(taa);
                return FrameStats::default();
            }
            Err(e) => {
                log::warn!("image ignorée : {e}");
                self.taa = Some(taa);
                return FrameStats::default();
            }
        };
        drop(acquire);
        let encode = perf::scope("rendu : commandes");
        let view = frame.texture.create_view(&Default::default());
        if let Some(t) = &mut self.gpu_timer {
            t.begin();
        }
        let mut encoder = self.device.create_command_encoder(&Default::default());
        // Cartes d'ombre des lumières dynamiques (seulement celles qui ont changé).
        if let Some(l) = self.lit_pipes().filter(|_| self.dyn_gpu.bake_pending()) {
            let ts = self
                .gpu_timer
                .as_ref()
                .and_then(|t| t.pass(gpu_timer::BAKE));
            self.dyn_gpu.encode_bake(&mut encoder, &l.bake, ts);
        }
        if do_refl {
            // Écrit l'image `latest` (déjà avancée par `ran`) en lisant l'autre.
            let write = self.refl.latest;
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("reflets (raytracing)"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.refl.views[write],
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.refl.depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(0.0),
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_bind_group(0, &self.bind_groups[1 - write], &[]);
            if let Some(p) = self.reflect_pipeline.try_get() {
                pass.set_pipeline(p);
                self.draw_reflective(&mut pass);
            }
            self.draw_reflective(&mut pass);
        }
        // Lumières de l'éditeur : actives cette image, leurs pipelines prêts, et au moins
        // une case de l'écran éclairée (sinon leurs passes n'ont rien à faire).
        let lights_on =
            self.dyn_gpu.active() && self.lit_pipes().is_some() && self.dyn_gpu.lit_rects() > 0;
        if let Some(t) = &taa.targets {
            // 1. Opaques : couleur, mouvement, et albédo et surface pour l'éclairage différé.
            let color = |view, load| {
                Some(wgpu::RenderPassColorAttachment {
                    view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load,
                        store: wgpu::StoreOp::Store,
                    },
                })
            };
            let clear = wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT);
            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("monde (TAA)"),
                    color_attachments: &[
                        color(&t.color_view, wgpu::LoadOp::Clear(wgpu::Color::BLACK)),
                        color(&t.motion_view, clear),
                        color(&t.gbuf_views[0], clear),
                        color(&t.gbuf_views[1], clear),
                    ],
                    depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                        view: &t.depth_views[taa.depth_index()],
                        depth_ops: Some(wgpu::Operations {
                            load: wgpu::LoadOp::Clear(0.0),
                            store: wgpu::StoreOp::Store,
                        }),
                        stencil_ops: None,
                    }),
                    timestamp_writes: self
                        .gpu_timer
                        .as_ref()
                        .and_then(|t| t.pass(gpu_timer::SCENE)),
                    occlusion_query_set: None,
                });
                self.draw_opaque(
                    &mut pass,
                    &ScenePipes {
                        sky: &taa.sky,
                        outline: &taa.outline,
                        heavy: taa.heavy.try_get(),
                        translucent: None,
                        taa: true,
                    },
                    &mut stats,
                );
            }
            // 2. Lumières de l'éditeur ajoutées une fois par pixel visible.
            if lights_on {
                if let Some(l) = self
                    .lit
                    .as_ref()
                    .and_then(|l| l.try_get())
                    .and_then(|r| r.as_ref().ok())
                {
                    let wg = self.world_group().clone();
                    let ts = self
                        .gpu_timer
                        .as_ref()
                        .and_then(|t| t.pass(gpu_timer::LIGHTS));
                    self.dyn_gpu.encode_deferred(
                        &self.device,
                        &mut encoder,
                        &wg,
                        &t.depth_views,
                        taa.depth_index(),
                        &t.gbuf_views,
                        t.generation,
                        &t.color_view,
                        &l.deferred,
                        ts,
                    );
                }
            }
            // 3. Contour, écrans vidéo, translucides (éclairés par les lumières si leur
            // variante est prête), fumigènes.
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("translucides (TAA)"),
                color_attachments: &[
                    color(&t.color_view, wgpu::LoadOp::Load),
                    color(&t.motion_view, wgpu::LoadOp::Load),
                ],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &t.depth_views[taa.depth_index()],
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: self
                    .gpu_timer
                    .as_ref()
                    .and_then(|t| t.pass(gpu_timer::AFTER)),
                occlusion_query_set: None,
            });
            self.draw_after(
                &mut pass,
                &ScenePipes {
                    sky: &taa.sky,
                    outline: &taa.outline,
                    heavy: taa.heavy.try_get(),
                    translucent: if lights_on {
                        self.lit_translucent()
                    } else {
                        None
                    },
                    taa: true,
                },
            );
            self.smokes.draw(&mut pass, self.world_group(), true);
        }
        // Lumière volumétrique des lumières dynamiques, ajoutée à l'image du monde.
        let vl = self
            .lit
            .as_ref()
            .and_then(|l| l.try_get())
            .and_then(|r| r.as_ref().ok())
            .map(|l| &l.vl);
        if let (Some(t), Some(vl)) = (&taa.targets, vl) {
            if self.dyn_gpu.volumetric_active() && lights_on {
                let wg = self.world_group().clone();
                let timer = self.gpu_timer.as_ref();
                self.dyn_gpu.encode_volumetric(
                    &self.device,
                    &mut encoder,
                    &wg,
                    &t.depth_views,
                    taa.depth_index(),
                    t.generation,
                    t.internal,
                    &t.color_view,
                    vl,
                    (
                        timer.and_then(|t| t.pass_begin(gpu_timer::VOLUMETRIC)),
                        timer.and_then(|t| t.pass_end(gpu_timer::VOLUMETRIC)),
                    ),
                );
            }
        }
        taa.resolve(
            &self.queue,
            &mut encoder,
            self.gpu_timer.as_ref().and_then(|t| t.pass(gpu_timer::TAA)),
        );
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("écran (TAA)"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(0.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: self
                    .gpu_timer
                    .as_ref()
                    .and_then(|t| t.pass(gpu_timer::PRESENT)),
                occlusion_query_set: None,
            });
            taa.present(&mut pass);
            // Bras avec les lumières dynamiques dès qu'il y en a et que leurs shaders sont
            // prêts (sinon comme sans lumières).
            let arm_lit = if self.dyn_gpu.active() {
                self.lit_pipes().map(|l| &l.arm)
            } else {
                None
            };
            draw_hud(
                &mut pass,
                &self.viewmodel,
                &self.overlay,
                self.world_group(),
                hand,
                arm_lit,
            );
        }
        if let Some(t) = &self.gpu_timer {
            t.resolve(&mut encoder);
        }
        drop(encode);
        self.capture
            .encode(&self.device, &mut encoder, &frame.texture);
        self.marker(Marker::SimulationEnd);
        self.marker(Marker::RenderSubmitStart);
        {
            let _s = perf::scope("rendu : envoi au GPU");
            self.queue.submit([encoder.finish()]);
        }
        self.marker(Marker::RenderSubmitEnd);
        self.marker(Marker::PresentStart);
        {
            let _s = perf::scope("rendu : présentation");
            frame.present();
        }
        self.marker(Marker::PresentEnd);
        if let Some(t) = &mut self.gpu_timer {
            if let Some(g) = t.after_submit(&self.device) {
                if self.raytracing {
                    self.rt_quality.update(g.scene);
                    // (Au journal : la portée des reflets dit à quel point la qualité a
                    // dû baisser pour tenir les images par seconde.)
                    perf::gauge("reflets : portée (blocs)", self.rt_quality.uniforms()[1]);
                } else {
                    self.rt_quality = rt::RtQuality::default();
                }
                perf::gpu(g);
            }
        }

        taa.prev_view_proj = Some(view_proj);
        self.taa = Some(taa);
        stats
    }
}

/// Morceaux non vides des opaques d'un maillage (par direction puis par tranche).
fn opaque_ranges(mesh: &MeshData) -> Vec<(u8, u8, u32, u32)> {
    let total = mesh.indices.len() as u32;
    let sectioned = mesh.section_counts.iter().flatten().sum::<u32>() == total;
    if !sectioned {
        return if total > 0 {
            vec![(6, 255, 0, total)]
        } else {
            Vec::new()
        };
    }
    let mut out = Vec::new();
    let mut start = 0;
    for (k, secs) in mesh.section_counts.iter().enumerate() {
        for (sec, &n) in secs.iter().enumerate() {
            if n > 0 {
                out.push((k as u8, sec as u8, start, n));
            }
            start += n;
        }
    }
    out
}

/// Main et objet tenu, puis l'interface 2D avec ses objets 3D (torches animées) dessinés
/// à leur place dans l'ordre.
fn draw_hud<'a>(
    pass: &mut wgpu::RenderPass<'a>,
    vm: &'a viewmodel::ViewModel,
    overlay: &'a overlay::OverlayRenderer,
    group: &'a wgpu::BindGroup,
    hand: u32,
    arm_lit: Option<&'a wgpu::RenderPipeline>,
) {
    vm.draw_arm(pass, group, arm_lit);
    vm.draw_range(pass, group, 0..hand);
    let mut done = 0;
    for (mark, range) in &overlay.splits {
        overlay.draw_range(pass, done..*mark);
        vm.draw_range(pass, group, hand + range.start..hand + range.end);
        done = *mark;
    }
    overlay.draw_range(pass, done..u32::MAX);
}

/// Lumière de la torche tenue : un peu devant et à droite des yeux.
fn held_light(p: &FrameParams) -> ([f32; 4], [f32; 4]) {
    match p.held_light {
        Some((rgb, radius)) => {
            let fwd = (p.view.inverse().transform_vector3(-Vec3::Z)).normalize_or_zero();
            let pos = p
                .held_light_pos
                .unwrap_or(p.player_eye + fwd * 0.4 - Vec3::Y * 0.2);
            // sRGB → linéaire, même échelle que les torches posées.
            let c = rgb.map(|v| (v as f32 / 255.0).powf(2.2) * 1.25);
            ([pos.x, pos.y, pos.z, radius], [c[0], c[1], c[2], 0.0])
        }
        None => ([0.0; 4], [0.0; 4]),
    }
}

/// Boîte du bloc visé pour les uniformes (w = 1 si un bloc est visé).
fn selection(sel: Option<([f32; 3], [f32; 3])>) -> ([f32; 4], [f32; 4]) {
    match sel {
        Some((a, b)) => ([a[0], a[1], a[2], 1.0], [b[0], b[1], b[2], 0.0]),
        None => ([0.0; 4], [0.0; 4]),
    }
}

/// Groupe de liaison du monde : uniformes + tableau de textures des blocs (avec mipmaps).
/// Tableau de textures des blocs (avec mipmaps) envoyé au GPU.
fn upload_block_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    tex: &textures::BlockTextures,
) -> wgpu::TextureView {
    let mips = tex.size.trailing_zeros() + 1;
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("blocs"),
        size: wgpu::Extent3d {
            width: tex.size,
            height: tex.size,
            depth_or_array_layers: tex.layers.len() as u32,
        },
        mip_level_count: mips,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    for (layer, img) in tex.layers.iter().enumerate() {
        let mut level = img.clone();
        for mip in 0..mips {
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: mip,
                    origin: wgpu::Origin3d {
                        x: 0,
                        y: 0,
                        z: layer as u32,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                &level.data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(level.width * 4),
                    rows_per_image: Some(level.height),
                },
                wgpu::Extent3d {
                    width: level.width,
                    height: level.height,
                    depth_or_array_layers: 1,
                },
            );
            level = textures::downsample(&level);
        }
    }
    texture.create_view(&wgpu::TextureViewDescriptor {
        dimension: Some(wgpu::TextureViewDimension::D2Array),
        ..Default::default()
    })
}

/// Les deux groupes de liaison du monde (un par image de reflets lue).
#[allow(clippy::too_many_arguments)]
fn world_groups(
    device: &wgpu::Device,
    bgl: &wgpu::BindGroupLayout,
    globals: &wgpu::Buffer,
    sampler: &wgpu::Sampler,
    blocks: &wgpu::TextureView,
    voxels: &rt::Voxels,
    refl: &reflections::Reflections,
    dyn_gpu: &lights::DynGpu,
) -> [wgpu::BindGroup; 2] {
    std::array::from_fn(|i| {
        let [d7, d8, d9, d10, d11, d12, d13] = dyn_gpu.bind_entries();
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("monde"),
            layout: bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: globals.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(blocks),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&voxels.view),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(&voxels.palette_view),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::TextureView(&refl.views[i]),
                },
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: wgpu::BindingResource::TextureView(&voxels.skin_view),
                },
                d7,
                d8,
                d9,
                d10,
                d11,
                d12,
                d13,
            ],
        })
    })
}

fn create_depth(device: &wgpu::Device, config: &wgpu::SurfaceConfiguration) -> wgpu::TextureView {
    device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some("profondeur"),
            size: wgpu::Extent3d {
                width: config.width,
                height: config.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: DEPTH_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        })
        .create_view(&Default::default())
}

/// Plans latéraux du frustum (normales vers l'intérieur). Pas de plan proche/lointain :
/// la projection est infinie et le brouillard borne déjà la distance.
fn frustum_planes(m: Mat4) -> [Vec4; 4] {
    let (r0, r1, r3) = (m.row(0), m.row(1), m.row(3));
    [r3 + r0, r3 - r0, r3 + r1, r3 - r1]
}

fn aabb_visible(planes: &[Vec4; 4], min: Vec3, max: Vec3) -> bool {
    planes.iter().all(|pl| {
        // Sommet de la boîte le plus loin dans la direction de la normale.
        let p = Vec3::new(
            if pl.x >= 0.0 { max.x } else { min.x },
            if pl.y >= 0.0 { max.y } else { min.y },
            if pl.z >= 0.0 { max.z } else { min.z },
        );
        pl.truncate().dot(p) + pl.w >= 0.0
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Les shaders se compilent (erreur de WGSL vue avant de lancer le jeu), et les
    /// uniformes du monde ont la taille que le shader attend.
    /// La direction du ciel (`sky_dir` du shader, refaite ici) reste juste très loin de
    /// l'origine et sous tous les angles : elle ne dépend ni d'une inversion de matrice ni
    /// de la position de la caméra (ce qui faisait parfois tourner le ciel).
    #[test]
    fn the_sky_direction_is_exact_far_from_the_origin() {
        use glam::{DVec3, Mat4, Vec3};
        let sky_dir = |m: Mat4, ndc: [f32; 2]| {
            let c = m.to_cols_array_2d();
            let row = |r: usize| Vec3::new(c[0][r], c[1][r], c[2][r]);
            let (rx, ry, rw) = (row(0), row(1), row(3));
            let d = (rx - ndc[0] * rw).cross(ry - ndc[1] * rw);
            (d * if rw.dot(d) >= 0.0 { 1.0 } else { -1.0 }).normalize()
        };
        let mut worst = 0.0f64;
        for (i, pos) in [
            [0.0, 70.0, 0.0],
            [-30.7, 151.6, -0.07],
            [250_000.3, 180.2, -730_111.6],
        ]
        .iter()
        .enumerate()
        {
            for k in 0..200 {
                let yaw = k as f64 * 0.37 + i as f64;
                let pitch = ((k * 7 % 31) as f64 / 31.0 - 0.5) * 3.0;
                let fwd = DVec3::new(
                    yaw.sin() * pitch.cos(),
                    pitch.sin(),
                    -yaw.cos() * pitch.cos(),
                );
                let eye = DVec3::from_array(*pos);
                // La projection du jeu : sans plan lointain, profondeur inversée.
                let vp = Mat4::perspective_infinite_reverse_rh(1.2, 16.0 / 9.0, 0.05)
                    * Mat4::look_to_rh(eye.as_vec3(), fwd.as_vec3(), Vec3::Y);
                let right = fwd.cross(DVec3::Y).normalize();
                let up = right.cross(fwd);
                let t = (1.2f64 / 2.0).tan();
                for ndc in [[0.0, 0.0], [0.9, -0.8], [-1.0, 1.0], [0.3, 0.95]] {
                    let want =
                        (fwd + right * ndc[0] as f64 * t * 16.0 / 9.0 + up * ndc[1] as f64 * t)
                            .normalize();
                    let got = sky_dir(vp, ndc).as_dvec3();
                    worst = worst.max((got - want).length());
                }
            }
        }
        // Moins d'un centième de degré d'écart, partout.
        assert!(worst < 2e-4, "écart {worst}");
    }

    /// Les lumières retrouvent la position et la direction de chaque pixel depuis la
    /// caméra, sans inverser la matrice : exactes sous tous les angles, avec le décalage du
    /// TAA, même à 730 000 blocs de l'origine (le bug du ciel de la v108, revenu sur les
    /// projecteurs jusqu'en v174).
    #[test]
    fn the_light_pixels_are_exact_far_from_the_origin() {
        use glam::{DVec3, Mat4, Vec3, Vec4};
        let (fov, aspect, near) = (1.2f64, 16.0f64 / 9.0, 0.05f32);
        let (mut worst_dir, mut worst_pos, mut worst_ndc) = (0.0f64, 0.0f64, 0.0f64);
        for (i, pos) in [
            [0.0, 70.0, 0.0],
            [-30.7, 151.6, -0.07],
            [3_141.6, 64.2, -2_718.3],
            [250_000.3, 180.2, -730_111.6],
        ]
        .iter()
        .enumerate()
        {
            for k in 0..120 {
                let yaw = k as f64 * 0.37 + i as f64;
                let pitch = ((k * 7 % 31) as f64 / 31.0 - 0.5) * 3.0;
                let fwd = DVec3::new(
                    yaw.sin() * pitch.cos(),
                    pitch.sin(),
                    -yaw.cos() * pitch.cos(),
                );
                let eye = DVec3::from_array(*pos);
                // Comme `render_taa` : décalage du TAA appliqué après la projection.
                let (jx, jy) = ((k % 5) as f32 * 0.2 - 0.4, (k % 3) as f32 * 0.3 - 0.3);
                let shift =
                    Mat4::from_translation(Vec3::new(jx * 2.0 / 1600.0, -jy * 2.0 / 900.0, 0.0));
                let vp = shift
                    * Mat4::perspective_infinite_reverse_rh(fov as f32, aspect as f32, near)
                    * Mat4::look_to_rh(eye.as_vec3(), fwd.as_vec3(), Vec3::Y);
                let eye = eye.as_vec3().as_dvec3();
                let right = fwd.cross(DVec3::Y).normalize();
                let up = right.cross(fwd);
                let t = (fov / 2.0).tan();
                for ndc in [[0.0, 0.0], [0.9, -0.8], [-1.0, 1.0], [0.3, 0.95]] {
                    let ray = fwd + right * (ndc[0] * t * aspect) + up * (ndc[1] * t);
                    // Le pixel qui le montre, dans l'image décalée.
                    let ndc_j = [
                        ndc[0] as f32 + jx * 2.0 / 1600.0,
                        ndc[1] as f32 - jy * 2.0 / 900.0,
                    ];
                    let got = view_ray(vp, ndc_j).as_dvec3();
                    worst_dir = worst_dir.max((got.normalize() - ray.normalize()).length());
                    for dist in [0.3f64, 6.0, 90.0] {
                        // Profondeur enregistrée (Z inversé : proche / distance de vue).
                        let depth = (near as f64 / dist) as f32;
                        let off = view_ray(vp, ndc_j) * (vp.w_axis.z / depth);
                        let want = ray * dist;
                        worst_pos = worst_pos.max((off.as_dvec3() - want).length() / dist);
                        // Et dans l'autre sens : le point projeté depuis la caméra.
                        let c = eye_relative(vp) * Vec4::from((want.as_vec3(), 1.0));
                        let e = ((c.x / c.w) as f64 - ndc_j[0] as f64)
                            .abs()
                            .max(((c.y / c.w) as f64 - ndc_j[1] as f64).abs());
                        worst_ndc = worst_ndc.max(e);
                    }
                }
            }
        }
        // Moins d'un centième de degré, d'un dix-millième de la distance, d'un dixième de
        // pixel.
        assert!(worst_dir < 2e-4, "direction : écart {worst_dir}");
        assert!(worst_pos < 1e-4, "position : écart relatif {worst_pos}");
        assert!(worst_ndc < 1e-4, "écran : écart {worst_ndc}");
    }

    /// Note pour la suite : retrouver une direction ou une position avec l'inverse de la
    /// vue-projection fait tourner ou sauter l'image (projection sans plan lointain et
    /// grandes coordonnées). C'est arrivé au ciel (v108) puis aux projecteurs (v175) : ce
    /// test refuse que ça revienne, dans les shaders comme dans le code du jeu.
    #[test]
    fn nothing_rebuilds_the_view_with_the_inverse_matrix() {
        // Morceaux recollés : ce test ne doit pas se trouver lui-même.
        let inv_field = ["inv_", "view_proj"].concat();
        let inverse = [".inv", "erse()"].concat();
        let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let mut bad = Vec::new();
        let mut dirs = vec![crates];
        while let Some(dir) = dirs.pop() {
            for entry in std::fs::read_dir(&dir).unwrap().flatten() {
                let path = entry.path();
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if path.is_dir() {
                    if name != "target" && !name.starts_with('.') {
                        dirs.push(path);
                    }
                    continue;
                }
                let wgsl = name.ends_with(".wgsl");
                if !wgsl && !name.ends_with(".rs") {
                    continue;
                }
                let text = std::fs::read_to_string(&path).unwrap();
                for (n, line) in text.lines().enumerate() {
                    let l = line.trim_start();
                    // Commentaires, et le champ des uniformes (déclaré et rempli pour leur
                    // disposition).
                    if l.starts_with("//") || l.starts_with(&format!("{inv_field}:")) {
                        continue;
                    }
                    let wrong = if wgsl {
                        l.contains(&inv_field)
                    } else {
                        l.contains(&inverse)
                            && ["view_proj", "jittered", "proj", "vp"]
                                .iter()
                                .any(|m| l.contains(m))
                    };
                    if wrong {
                        bad.push(format!("{}:{}: {l}", path.display(), n + 1));
                    }
                }
            }
        }
        assert!(
            bad.is_empty(),
            "Ne retrouve pas une direction ou une position avec l'inverse de la \
             vue-projection : la projection n'a pas de plan lointain et les coordonnées sont \
             grandes, les arrondis font tourner ou sauter l'image (ciel v108, projecteurs \
             v175). Pars des lignes de la matrice : `view_ray` / `eye_relative` (Rust), \
             `dl_view_ray` / `dl_view_offset` (dynlights.wgsl), `sky_dir` (world.wgsl).\n{}",
            bad.join("\n")
        );
    }

    #[test]
    fn shaders_are_valid() {
        use wgpu::naga;
        for (name, src) in [
            (
                "world",
                lights::with_dynlights(include_str!("shaders/world.wgsl")),
            ),
            (
                "entity",
                lights::with_dynlights(include_str!("shaders/entity.wgsl")),
            ),
            // Shaders de tous les jours (sans le code des lumières de l'éditeur).
            (
                "world (sans lumières)",
                lights::without_dynlights(include_str!("shaders/world.wgsl")),
            ),
            (
                "entity (sans lumières)",
                lights::without_dynlights(include_str!("shaders/entity.wgsl")),
            ),
            (
                "joueurs",
                lights::without_dynlights(&format!(
                    "{}\n{}",
                    include_str!("shaders/entity.wgsl"),
                    include_str!("shaders/players.wgsl")
                )),
            ),
            (
                "volumetric",
                lights::with_dynlights(include_str!("shaders/volumetric.wgsl")),
            ),
            (
                "dyn_deferred",
                lights::with_dynlights(include_str!("shaders/dyn_deferred.wgsl")),
            ),
            (
                "viewmodel",
                include_str!("shaders/viewmodel.wgsl").to_string(),
            ),
            (
                "bras (sans lumières)",
                include_str!("shaders/viewmodel_arm.wgsl").to_string(),
            ),
            (
                "bras (lumières)",
                lights::with_dynlights(&format!(
                    "{}\n{}",
                    include_str!("shaders/viewmodel_arm.wgsl"),
                    include_str!("shaders/viewmodel_arm_lit.wgsl")
                )),
            ),
            ("smoke", include_str!("shaders/smoke.wgsl").to_string()),
            (
                "shadow_bake",
                include_str!("shaders/shadow_bake.wgsl").to_string(),
            ),
            (
                "vl_composite",
                include_str!("shaders/vl_composite.wgsl").to_string(),
            ),
        ] {
            let src = src.as_str();
            let module = naga::front::wgsl::parse_str(src)
                .unwrap_or_else(|e| panic!("{name}: {}", e.emit_to_string(src)));
            let info = naga::valid::Validator::new(
                naga::valid::ValidationFlags::all(),
                naga::valid::Capabilities::all(),
            )
            .validate(&module)
            .unwrap_or_else(|e| panic!("{name}: {e:?}"));
            // Les liaisons des lumières (7 à 13 du groupe 0) ne sont visibles que des pixels :
            // un sommet qui les lit fait refuser le pipeline à l'exécution.
            for (i, ep) in module.entry_points.iter().enumerate() {
                if ep.stage != naga::ShaderStage::Vertex {
                    continue;
                }
                let fi = info.get_entry_point(i);
                for (h, var) in module.global_variables.iter() {
                    if fi[h].is_empty() {
                        continue;
                    }
                    if let Some(b) = &var.binding {
                        assert!(
                            !(b.group == 0 && (7..=13).contains(&b.binding)),
                            "{name} : le sommet {} lit la liaison {} des lumières",
                            ep.name,
                            b.binding
                        );
                    }
                }
            }
            drop(info);
            if name.starts_with("world") {
                let g = module
                    .types
                    .iter()
                    .find(|(_, t)| t.name.as_deref() == Some("Globals"))
                    .unwrap();
                let size = g.1.inner.size(module.to_ctx());
                assert_eq!(size as usize, std::mem::size_of::<Globals>());
            }
            if name.ends_with("(sans lumières)") {
                for (_, v) in module.global_variables.iter() {
                    let b = v.binding.as_ref().map_or(0, |b| b.binding);
                    assert!(
                        !(7..=13).contains(&b),
                        "{name} : liaison {b} des lumières utilisée"
                    );
                }
            }
        }
    }

    #[test]
    fn frustum_culls_boxes_behind_camera() {
        let proj = Mat4::perspective_infinite_reverse_rh(1.2, 16.0 / 9.0, 0.1);
        let view = Mat4::look_to_rh(Vec3::ZERO, Vec3::NEG_Z, Vec3::Y);
        let pl = frustum_planes(proj * view);
        assert!(aabb_visible(
            &pl,
            Vec3::new(-1.0, -1.0, -11.0),
            Vec3::new(1.0, 1.0, -9.0)
        ));
        assert!(!aabb_visible(
            &pl,
            Vec3::new(-1.0, -1.0, 9.0),
            Vec3::new(1.0, 1.0, 11.0)
        ));
        assert!(!aabb_visible(
            &pl,
            Vec3::new(100.0, -1.0, -11.0),
            Vec3::new(102.0, 1.0, -9.0)
        ));
    }
}

impl Renderer {
    /// Écrans vidéo posés dans le monde : numéro et emplacement (aucun : liste vide).
    pub fn set_screens(&mut self, anchors: &[(u32, ScreenAnchor)]) {
        self.screen.set_anchors(anchors);
    }

    /// Image de l'écran `id` (BGRA 8 bits), envoyée à la carte seulement si `version`
    /// change.
    pub fn set_screen_frame(
        &mut self,
        id: u32,
        version: u64,
        width: u32,
        height: u32,
        pixels: &[u8],
    ) {
        self.screen.set_frame(
            &self.device,
            &self.queue,
            id,
            version,
            width,
            height,
            pixels,
        );
    }
}
