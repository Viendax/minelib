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
mod gpu_timer;
pub mod igpu;
mod occlusion;
mod reflections;
pub use wgpu;
mod latency;
mod overlay;
pub mod perf;
pub mod rt;
mod taa;
pub mod textures;
pub mod viewmodel;

pub use capture::Screenshot;

pub use latency::{LowLatencyKind, Marker};
pub use overlay::{Line, Overlay, Ui};
pub use viewmodel::{VmVertex, FLAME, NO_TEXTURE};

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
pub(crate) const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
struct Globals {
    view_proj: [[f32; 4]; 4],
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
    pub eye: Vec3,
    pub view: Mat4,
    pub fov_y: f32,
    pub fog_start: f32,
    pub fog_end: f32,
    /// Boîte (coins min, max, en coordonnées monde) du bloc visé.
    pub selected: Option<([f32; 3], [f32; 3])>,
    pub time: f32,
    /// Lumière du jour : 1 = plein jour, 0 = nuit.
    pub daylight: f32,
    /// Interface (inventaire, barre d'objets…) dessinée par-dessus tout.
    pub ui: Option<&'a Ui>,
    /// Main et objet tenu (triangles déjà projetés).
    pub viewmodel: &'a [VmVertex],
    /// Torche tenue en main : couleur (sRGB) et portée en blocs.
    pub held_light: Option<([u8; 3], f32)>,
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
    mask: u16,
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
    block_pipeline: wgpu::RenderPipeline,
    reflective_pipeline: wgpu::RenderPipeline,
    reflect_pipeline: wgpu::RenderPipeline,
    translucent_pipeline: wgpu::RenderPipeline,
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
    voxels: rt::Voxels,
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
    vis: HashMap<ChunkPos, u16>,
    vis_cam: Option<(ChunkPos, i32)>,
    vis_dirty: bool,
    vis_job: Option<std::sync::mpsc::Receiver<HashMap<ChunkPos, u16>>>,
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
        let bind_groups = world_groups(
            &device,
            &bgl,
            &globals,
            &block_sampler,
            &block_view,
            &voxels,
            &refl,
        );
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("main"),
            bind_group_layouts: &[&bgl],
            push_constant_ranges: &[],
        });
        let shader = device.create_shader_module(wgpu::include_wgsl!("shaders/world.wgsl"));

        let vertex_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Vertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &VERTEX_ATTRS,
        };

        let make = |label: &str,
                    vs: &str,
                    fs: &str,
                    buffers: &[wgpu::VertexBufferLayout],
                    cull: Option<wgpu::Face>,
                    depth_write: bool,
                    depth_compare: wgpu::CompareFunction,
                    blend: Option<wgpu::BlendState>| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
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
                    module: &shader,
                    entry_point: Some(fs),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: config.format,
                        blend,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview: None,
                cache: None,
            })
        };

        let block_pipeline = make(
            "blocs",
            "vs_block",
            "fs_block_fast",
            std::slice::from_ref(&vertex_layout),
            Some(wgpu::Face::Back),
            true,
            wgpu::CompareFunction::Greater, // Z inversé : 1 = près, 0 = infini
            None,
        );
        // Passe des reflets (raytracing à son propre rythme) : surfaces réfléchissantes
        // seulement, dans l'image des reflets.
        let reflect_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("reflets (raytracing)"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
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
                module: &shader,
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
        });
        // Miroirs et métaux : shader complet (reflets), sur peu de pixels.
        let reflective_pipeline = make(
            "reflets",
            "vs_block",
            "fs_block",
            std::slice::from_ref(&vertex_layout),
            Some(wgpu::Face::Back),
            true,
            wgpu::CompareFunction::Greater,
            None,
        );
        // Eau, glace, verre teinté : mélange alpha, sans écrire la profondeur, visibles des deux côtés.
        let translucent_pipeline = make(
            "translucide",
            "vs_block",
            "fs_block_translucent",
            std::slice::from_ref(&vertex_layout),
            None,
            false,
            wgpu::CompareFunction::Greater,
            Some(wgpu::BlendState::ALPHA_BLENDING),
        );
        let sky_pipeline = make(
            "ciel",
            "vs_sky",
            "fs_sky",
            &[],
            None,
            false,
            wgpu::CompareFunction::Always,
            None,
        );
        let outline_pipeline = make(
            "contour",
            "vs_outline",
            "fs_outline",
            &[],
            None,
            false,
            wgpu::CompareFunction::GreaterEqual,
            Some(wgpu::BlendState::ALPHA_BLENDING),
        );

        let atlas = textures::build_gui_atlas(pack, &overlay::font_bitmap());
        let overlay =
            overlay::OverlayRenderer::new(&device, &queue, config.format, DEPTH_FORMAT, &atlas);

        let viewmodel = viewmodel::ViewModel::new(&device, &bgl, config.format);
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
            voxels,
            lights_pos: [[0.0; 4]; MAX_LIGHTS],
            lights_col: [[0.0; 4]; MAX_LIGHTS],
            light_count: 0,
            mirror_lights: [[[0.0; 4]; MAX_MIRROR_LIGHTS]; 3],
            mirror_count: 0,
            raytracing: false,
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
            block_pipeline,
            reflective_pipeline,
            reflect_pipeline,
            translucent_pipeline,
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
        chunks + depth + self.overlay.gpu_bytes + std::mem::size_of::<Globals>() as u64
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
        );
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

    pub fn set_raytracing(&mut self, on: bool) {
        self.raytracing = on;
    }

    /// Grille de blocs autour du joueur pour le raytracing (voir `rt::VOX`).
    pub fn set_voxels(&mut self, origin: [i32; 3], ids: &[u16]) {
        self.voxels.upload(&self.queue, origin, ids);
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
        if let Some(l) = &mut self.latency {
            l.set_enabled(on);
        }
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
                | minelib_world::registry::TexSrc::Tint(n, _) => Some(*n),
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

    /// Active ou coupe l'anticrénelage temporel (TAA).
    pub fn set_antialiasing(&mut self, on: bool) {
        if on == self.taa.is_some() {
            return;
        }
        self.taa = on.then(|| {
            let vertex_layout = wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<Vertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &VERTEX_ATTRS,
            };
            taa::Taa::new(
                &self.device,
                &self.world_layout,
                &self.world_shader,
                &vertex_layout,
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
            self.overlay.draw(&mut pass);
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
        let items: Vec<(&ChunkGpu, u16)> = order
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
    fn draw_scene<'p>(
        &'p self,
        pass: &mut wgpu::RenderPass<'p>,
        pipes: [&'p wgpu::RenderPipeline; 5],
        stats: &mut FrameStats,
    ) {
        pass.set_bind_group(0, self.world_group(), &[]);

        pass.set_pipeline(pipes[0]);
        pass.draw(0..3, 0..1);

        // Blocs opaques : une page entière par appel (dessin indirect multiple), sinon un
        // appel par chunk mais sans changer de tampon.
        pass.set_pipeline(pipes[1]);
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
        pass.set_pipeline(pipes[4]);
        self.draw_reflective(pass);

        // Opaques réellement envoyés (faces tournées vers la caméra seulement).
        stats.triangles += self.drawn_triangles as usize;
        perf::gauge(
            "milliers de triangles par image",
            self.drawn_triangles as f32 / 1000.0,
        );
        if self.outline {
            pass.set_pipeline(pipes[3]);
            pass.draw(0..24, 0..1);
        }
        // Translucides de l'arrière vers l'avant, par-dessus la scène opaque.
        pass.set_pipeline(pipes[2]);
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
        let (sel_min, sel_max) = selection(p.selected);
        self.outline = p.selected.is_some();
        let globals = Globals {
            view_proj: view_proj.to_cols_array_2d(),
            inv_view_proj: view_proj.inverse().to_cols_array_2d(),
            cam_pos: [p.eye.x, p.eye.y, p.eye.z, p.time],
            fog: [p.fog_start, p.fog_end, self.aspect(), 0.0],
            sun: [0.45, 0.82, 0.35, 0.48],
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
                0.0,
                0.0,
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
        };
        self.queue
            .write_buffer(&self.globals, 0, bytemuck::bytes_of(&globals));

        self.overlay
            .prepare(&self.device, &self.queue, p.overlay, p.ui, self.size());
        let hand = self
            .viewmodel
            .prepare_with(&self.queue, p.viewmodel, &self.overlay.models);
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
                [
                    &self.sky_pipeline,
                    &self.block_pipeline,
                    &self.translucent_pipeline,
                    &self.outline_pipeline,
                    &self.reflective_pipeline,
                ],
                &mut stats,
            );
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
        let globals = Globals {
            view_proj: jittered.to_cols_array_2d(),
            inv_view_proj: jittered.inverse().to_cols_array_2d(),
            cam_pos: [p.eye.x, p.eye.y, p.eye.z, p.time],
            fog: [p.fog_start, p.fog_end, self.aspect(), 0.0],
            sun: [0.45, 0.82, 0.35, 0.48],
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
            screen: [world_size.0 as f32, world_size.1 as f32, 0.0, 0.0],
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
        };
        self.overlay
            .prepare(&self.device, &self.queue, p.overlay, p.ui, size);
        let hand = self
            .viewmodel
            .prepare_with(&self.queue, p.viewmodel, &self.overlay.models);
        let mut stats = {
            let _c = perf::scope("rendu : tri des chunks");
            self.cull(view_proj, p.eye, p.fog_end)
        };
        // Reflets : recalculés à leur rythme, seulement s'il y a des surfaces qui reflètent
        // à l'écran (raytracing activé).
        self.refl.tick();
        let has_reflective = self
            .draw_list
            .iter()
            .any(|(_, k)| self.meshes[k].reflective > 0);
        // Reflets synchronisés (par défaut) : calculés à chaque image sur les surfaces mêmes
        // (`refl_cam.w` reste à 0, le shader lance les rayons directement).
        let do_refl =
            self.async_reflections && self.raytracing && has_reflective && self.refl.due();
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
            pass.set_pipeline(&self.reflect_pipeline);
            self.draw_reflective(&mut pass);
        }
        if let Some(t) = &taa.targets {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("monde (TAA)"),
                color_attachments: &[
                    Some(wgpu::RenderPassColorAttachment {
                        view: &t.color_view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                            store: wgpu::StoreOp::Store,
                        },
                    }),
                    Some(wgpu::RenderPassColorAttachment {
                        view: &t.motion_view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: wgpu::StoreOp::Store,
                        },
                    }),
                ],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &t.depth_views[taa.depth_index()],
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(0.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: self.gpu_timer.as_ref().and_then(|t| t.pass(0)),
                occlusion_query_set: None,
            });
            self.draw_scene(
                &mut pass,
                [
                    &taa.sky,
                    &taa.block,
                    &taa.translucent,
                    &taa.outline,
                    &taa.reflective,
                ],
                &mut stats,
            );
        }
        taa.resolve(
            &self.queue,
            &mut encoder,
            self.gpu_timer.as_ref().and_then(|t| t.pass(1)),
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
                timestamp_writes: self.gpu_timer.as_ref().and_then(|t| t.pass(2)),
                occlusion_query_set: None,
            });
            taa.present(&mut pass);
            draw_hud(
                &mut pass,
                &self.viewmodel,
                &self.overlay,
                self.world_group(),
                hand,
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
) {
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
            let pos = p.eye + fwd * 0.4 - Vec3::Y * 0.2;
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
fn world_groups(
    device: &wgpu::Device,
    bgl: &wgpu::BindGroupLayout,
    globals: &wgpu::Buffer,
    sampler: &wgpu::Sampler,
    blocks: &wgpu::TextureView,
    voxels: &rt::Voxels,
    refl: &reflections::Reflections,
) -> [wgpu::BindGroup; 2] {
    std::array::from_fn(|i| {
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
