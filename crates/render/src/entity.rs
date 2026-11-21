//! Entités dans le monde (modèle du joueur vu à la 3e personne…) : triangles en
//! coordonnées monde, de couleur unie déjà éclairée par le jeu, dessinés dans la passe du
//! monde avec sa profondeur (cachés par les blocs) et la caméra du monde (tremblement de
//! l'anticrénelage temporel compris).

use bytemuck::{Pod, Zeroable};

use crate::taa::{COLOR_FORMAT, GB_ALBEDO_FORMAT, GB_SURF_FORMAT, MOTION_FORMAT};
use crate::DEPTH_FORMAT;

/// Sommet d'entité : position monde et couleur linéaire (sans l'éclairage du lieu),
/// multipliée par la texture de bloc `layer` aux coordonnées `uv` si `layer` ≥ 0 (objet
/// tenu), puis par `light`.
#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
pub struct EntityVertex {
    pub pos: [f32; 3],
    pub color: [f32; 3],
    pub uv: [f32; 2],
    /// Couche du tableau de textures des blocs ; −1 : couleur unie ; `FLAME` : flamme animée
    /// (`uv.y` = hauteur dans le bloc de la torche, couleur = celle de la flamme).
    pub layer: i32,
    /// Éclairage du lieu (rgb, 0..2 ramené à 0..255, voir `EntityVertex::light`) ; a = 255 :
    /// éclairé aussi par les lumières dynamiques, 0 : lumineux par lui-même (flamme).
    pub light: [u8; 4],
}

/// Couche « flamme » d'un sommet d'entité (torche tenue, vue en F5) : animée comme la
/// flamme de la torche à la première personne (viewmodel.wgsl) et des torches posées.
pub const FLAME: i32 = -2;

impl EntityVertex {
    /// Éclairage du lieu (0..2 par canal) au format du sommet ; `dynamic` : reçoit aussi la
    /// lumière des lumières dynamiques.
    pub fn light(rgb: [f32; 3], dynamic: bool) -> [u8; 4] {
        let c = |v: f32| (v.clamp(0.0, 2.0) * 127.5).round() as u8;
        [
            c(rgb[0]),
            c(rgb[1]),
            c(rgb[2]),
            if dynamic { 255 } else { 0 },
        ]
    }
}

/// Au plus tant de sommets d'entités par image.
const MAX_VERTICES: usize = 1_048_576;

/// (v172) Sommet du modèle des autres joueurs (envoyé une fois) : position en pixels dans le
/// repère de sa partie, coordonnées dans l'image du skin (0..1), partie (0 jambe gauche,
/// 1 jambe droite, 2 corps, 3 bras gauche, 4 bras droit, 5 tête), normale de sa face (repère
/// de la partie).
#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
pub struct PlayerVertex {
    pub pos: [f32; 3],
    pub uv: [f32; 2],
    pub part: u32,
    pub normal: [f32; 3],
    /// Rectangle de sa face dans l'image du skin (0..1 : u0, v0, u1, v1).
    pub rect: [f32; 4],
}

/// (v172) Un autre joueur à dessiner : sa pose, la lumière qu'il reçoit (0..2 par canal) et
/// la couche de son skin (`Renderer::set_player_skin` ; bit 16 : il tient un objet, bras
/// droit levé).
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Pod, Zeroable)]
pub struct PlayerInstance {
    pub feet: [f32; 3],
    pub yaw: f32,
    pub pitch: f32,
    pub walk: f32,
    pub swing: f32,
    pub crouch: f32,
    pub light: [f32; 3],
    pub skin: u32,
    /// (v187) Ses pieds à l'image d'avant : l'anticrénelage temporel a besoin de savoir où
    /// il était pour retrouver son historique. Sans cela, un joueur qui marche devant une
    /// caméra immobile n'a aucun mouvement à l'écran, et l'anticrénelage mélange plusieurs
    /// images de lui : il traîne (vu dans l'essai de foule).
    ///
    /// (Ces champs à la fin : `vertex_attr_array!` place les attributs dans l'ordre
    /// déclaré, qui doit suivre l'ordre des champs.)
    pub prev_feet: [f32; 3],
    /// (v189) Le reste de sa pose à l'image d'avant : son regard, puis sa marche et son
    /// accroupissement. Sans eux, un joueur qui tourne ou balance les bras sans se déplacer
    /// n'a toujours aucun mouvement à l'écran, et ses membres traînent.
    pub prev_yaw: f32,
    pub prev_pitch: f32,
    pub prev_walk: f32,
    pub prev_swing: f32,
    pub prev_crouch: f32,
}

impl PlayerInstance {
    /// Bit de `skin` : bras droit levé (objet tenu).
    pub const HOLDS: u32 = 1 << 16;
    /// (v177) Bit de `skin` : arme à feu en main, les deux bras tendus vers le regard.
    pub const AIMS: u32 = 1 << 17;
    /// (v215) Bit de `skin` : lampe torche en main, le bras droit seul tendu vers le regard.
    pub const AIMS_RIGHT: u32 = 1 << 18;
}

/// Demi-flottant (IEEE 754 binaire16) le plus proche d'un nombre positif (couleurs).
fn half(x: f32) -> u16 {
    let x = if x.is_finite() {
        x.clamp(0.0, 65504.0)
    } else {
        0.0
    };
    if x < 6.103_515_6e-5 {
        // Dénormaux : multiples de 2^-24.
        return (x * 16_777_216.0).round() as u16;
    }
    let b = x.to_bits();
    let e = ((b >> 23) & 0xff) - 112;
    let m = b & 0x7f_ffff;
    let h = (e << 10) | (m >> 13);
    // Arrondi au plus proche, à égalité vers le pair (la retenue passe dans l'exposant).
    let r = m & 0x1fff;
    (h + u32::from(r > 0x1000 || (r == 0x1000 && h & 1 == 1))) as u16
}

/// Ce que la carte lit dans chaque `PlayerInstance`. Les décalages sont calculés dans
/// l'ordre déclaré : cet ordre doit donc suivre celui des champs de la structure (essai
/// `instance_attributes_follow_the_struct_fields`). Les emplacements 0 à 3 et 9 sont pris
/// par le modèle (`MESH`).
const PLAYER_ATTRS: [wgpu::VertexAttribute; 8] = wgpu::vertex_attr_array![
    4 => Float32x3,
    5 => Float32x2,
    6 => Float32x3,
    7 => Float32x3,
    8 => Uint32,
    10 => Float32x3,
    11 => Float32x2,
    12 => Float32x3
];

/// Joueurs dessinés au plus par image, et skins différents gardés sur la carte graphique.
pub const MAX_PLAYERS: usize = 8192;
pub const PLAYER_SKINS: u32 = 64;

/// Sommets au plus du modèle commun (six pavés : 216).
const MESH_MAX: usize = 512;
/// (v220) Cases au plus de l'atlas des joueurs à une image, et place des uniformes de
/// chacune (alignement des décalages dynamiques).
const MAX_LIGHT_JOBS: usize = 384;
const LIGHT_JOB_STRIDE: u64 = 256;

/// (v215) Le ciel des blocs, recopié tel quel à l'assemblage : le brouillard des entités
/// et des joueurs va vers le même ciel que celui des blocs, sans un second ciel à tenir à
/// jour. (v216) C'est le bloc « CIEL » de world.wgsl, entre ses deux repères : l'atmosphère,
/// le soleil, la lune, les étoiles et les nuages (`sky_shade`), avec leurs liaisons.
pub(crate) fn sky_wgsl() -> String {
    let src = include_str!("shaders/world.wgsl");
    let (from, to) = ("// CIEL : DÉBUT", "// CIEL : FIN");
    let a = src
        .find(from)
        .unwrap_or_else(|| panic!("world.wgsl : « {from} » introuvable"));
    let b = src
        .find(to)
        .unwrap_or_else(|| panic!("world.wgsl : « {to} » introuvable"));
    src[a..b].to_string()
}

/// (v215) Shader des entités (entity.wgsl), avec le ciel de leur brouillard.
pub(crate) fn entity_wgsl() -> String {
    format!("{}\n{}", include_str!("shaders/entity.wgsl"), sky_wgsl())
}

/// (v215) Shader des autres joueurs : celui des entités, puis players.wgsl.
pub(crate) fn players_wgsl() -> String {
    format!(
        "{}\n{}",
        entity_wgsl(),
        include_str!("shaders/players.wgsl")
    )
}

/// Les autres joueurs : un modèle, une instance chacun, une couche de skin par skin.
struct Players {
    plain: wgpu::RenderPipeline,
    taa: wgpu::RenderPipeline,
    mesh: wgpu::Buffer,
    mesh_count: u32,
    instances: wgpu::Buffer,
    count: u32,
    /// (v190) Instances après `count` dessinées seulement dans les miroirs (soi-même).
    extra: u32,
    /// (v202) Ceux qu'on peut voir reflétés dans l'eau (voir `water_mirror`), à part.
    water: wgpu::Buffer,
    water_count: u32,
    /// (v217) Carte d'ombre du soleil (voir `sunmap`) : profondeur des joueurs vue du soleil,
    /// sans couleur, et ceux qui y sont dessinés (au-delà des boîtes d'ombre exactes).
    sun: wgpu::RenderPipeline,
    sun_instances: wgpu::Buffer,
    sun_count: u32,
    /// (v220) Atlas des ombres des joueurs aux lumières dynamiques (voir `lights::EntJob`) :
    /// leur profondeur vue de chaque lumière (distance rapportée à sa portée), une case par
    /// case de la carte des blocs ; uniformes de chaque case (décalage dynamique), joueurs
    /// de chacune (à la suite), et les cases de l'image (place, décalage, joueurs).
    light: wgpu::RenderPipeline,
    light_jobs: wgpu::Buffer,
    light_group: wgpu::BindGroup,
    light_instances: wgpu::Buffer,
    light_draws: Vec<([u32; 3], u32, std::ops::Range<u32>)>,
    skins: wgpu::Texture,
    group: wgpu::BindGroup,
}

impl Players {
    fn new(
        device: &wgpu::Device,
        world_bgl: &wgpu::BindGroupLayout,
        surface: wgpu::TextureFormat,
    ) -> Self {
        let src = crate::lights::without_dynlights(&players_wgsl());
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("joueurs"),
            source: wgpu::ShaderSource::Wgsl(src.into()),
        });
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("skins des joueurs"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let skins = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("skins des joueurs"),
            size: wgpu::Extent3d {
                width: 64,
                height: 64,
                depth_or_array_layers: PLAYER_SKINS,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            // Couleurs linéaires exactes (le grain peut dépasser 1 sur un pixel blanc).
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = skins.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        // Pixels nets, comme le modèle du joueur local (un carré par pixel du skin).
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("skins des joueurs"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("skins des joueurs"),
            layout: &bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("joueurs"),
            bind_group_layouts: &[world_bgl, &bgl],
            push_constant_ranges: &[],
        });
        const MESH: [wgpu::VertexAttribute; 5] = wgpu::vertex_attr_array![
            0 => Float32x3,
            1 => Float32x2,
            2 => Uint32,
            3 => Float32x3,
            9 => Float32x4
        ];
        const INSTANCE: [wgpu::VertexAttribute; 8] = PLAYER_ATTRS;
        let pipeline = |label: &str, entry: &str, targets: &[Option<wgpu::ColorTargetState>]| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_player"),
                    compilation_options: Default::default(),
                    buffers: &[
                        wgpu::VertexBufferLayout {
                            array_stride: std::mem::size_of::<PlayerVertex>() as u64,
                            step_mode: wgpu::VertexStepMode::Vertex,
                            attributes: &MESH,
                        },
                        wgpu::VertexBufferLayout {
                            array_stride: std::mem::size_of::<PlayerInstance>() as u64,
                            step_mode: wgpu::VertexStepMode::Instance,
                            attributes: &INSTANCE,
                        },
                    ],
                },
                primitive: wgpu::PrimitiveState {
                    // (v197) Faces arrière écartées : les six pavés d'un joueur sont
                    // fermés, on n'en voit jamais l'intérieur, et la foule faisait
                    // travailler la carte sur 864 000 sommets par image à 4 000 joueurs,
                    // la moitié pour rien. Le modèle tourne ses triangles dans le même sens
                    // que le monde (essai `players_wind_their_triangles_like_the_world`),
                    // dont les pipelines écartent déjà les faces arrière, y compris dans les
                    // passes de miroir (le retournement gauche-droite de `FLIP_X * vp * M`
                    // rétablit le sens, voir `planar`).
                    cull_mode: Some(wgpu::Face::Back),
                    ..Default::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: true,
                    depth_compare: wgpu::CompareFunction::GreaterEqual,
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: Default::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(entry),
                    compilation_options: Default::default(),
                    targets,
                }),
                multiview: None,
                cache: None,
            })
        };
        let target = |format| {
            Some(wgpu::ColorTargetState {
                format,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })
        };
        let taa_targets = [
            target(COLOR_FORMAT),
            target(MOTION_FORMAT),
            target(GB_ALBEDO_FORMAT),
            target(GB_SURF_FORMAT),
        ];
        let mesh = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("modèle des joueurs"),
            size: (MESH_MAX * std::mem::size_of::<PlayerVertex>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let instances = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("joueurs (instances)"),
            size: (MAX_PLAYERS * std::mem::size_of::<PlayerInstance>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let water = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("joueurs reflétés dans l'eau (instances)"),
            size: (MAX_PLAYERS * std::mem::size_of::<PlayerInstance>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        // (v217) Ombres au soleil des joueurs loin de la caméra : leur profondeur seule, vue
        // du soleil, dans la carte de `sunmap` (profondeur 0 près du soleil, faces des deux
        // côtés : un joueur n'y laisse jamais de trou). Le shader de sommets ne lit que les
        // uniformes du monde (groupe 0).
        let sun_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("joueurs : ombre au soleil"),
            bind_group_layouts: &[world_bgl],
            push_constant_ranges: &[],
        });
        let sun = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("joueurs : ombre au soleil"),
            layout: Some(&sun_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_player"),
                compilation_options: Default::default(),
                buffers: &[
                    wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<PlayerVertex>() as u64,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &MESH,
                    },
                    wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<PlayerInstance>() as u64,
                        step_mode: wgpu::VertexStepMode::Instance,
                        attributes: &INSTANCE,
                    },
                ],
            },
            primitive: wgpu::PrimitiveState {
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: crate::sunmap::FORMAT,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::Less,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: Default::default(),
            fragment: None,
            multiview: None,
            cache: None,
        });
        let sun_instances = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("joueurs : ombre au soleil (instances)"),
            size: (MAX_PLAYERS * std::mem::size_of::<PlayerInstance>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        // (v220) Ombres des joueurs aux lumières dynamiques : leur distance à la lumière,
        // dans l'atlas des joueurs (voir `lights::EntJob`). Une case à la fois (ses
        // uniformes par décalage dynamique) ; faces des deux côtés.
        let light_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("joueurs : ombre aux lumières"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 24,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: wgpu::BufferSize::new(std::mem::size_of::<
                        crate::lights::EntFrame,
                    >() as u64),
                },
                count: None,
            }],
        });
        let light_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("joueurs : ombre aux lumières"),
            bind_group_layouts: &[&light_bgl],
            push_constant_ranges: &[],
        });
        let light = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("joueurs : ombre aux lumières"),
            layout: Some(&light_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_player_light"),
                compilation_options: Default::default(),
                buffers: &[
                    wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<PlayerVertex>() as u64,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &MESH,
                    },
                    wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<PlayerInstance>() as u64,
                        step_mode: wgpu::VertexStepMode::Instance,
                        attributes: &INSTANCE,
                    },
                ],
            },
            primitive: wgpu::PrimitiveState {
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: crate::lights::ENT_ATLAS_FORMAT,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::Less,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_player_light"),
                compilation_options: Default::default(),
                targets: &[],
            }),
            multiview: None,
            cache: None,
        });
        let light_jobs = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("joueurs : ombre aux lumières (cases)"),
            size: MAX_LIGHT_JOBS as u64 * LIGHT_JOB_STRIDE,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let light_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("joueurs : ombre aux lumières"),
            layout: &light_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 24,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &light_jobs,
                    offset: 0,
                    size: wgpu::BufferSize::new(
                        std::mem::size_of::<crate::lights::EntFrame>() as u64
                    ),
                }),
            }],
        });
        let light_instances = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("joueurs : ombre aux lumières (instances)"),
            size: (crate::lights::MAX_ENT_CASTERS * std::mem::size_of::<PlayerInstance>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            plain: pipeline("joueurs", "fs_player", &[target(surface)]),
            taa: pipeline("joueurs (TAA)", "fs_player_taa", &taa_targets),
            mesh,
            mesh_count: 0,
            instances,
            count: 0,
            extra: 0,
            water,
            water_count: 0,
            sun,
            sun_instances,
            sun_count: 0,
            light,
            light_jobs,
            light_group,
            light_instances,
            light_draws: Vec::new(),
            skins,
            group,
        }
    }

    /// (v220) Cases de l'atlas des joueurs à dessiner (`jobs`), avec leurs joueurs :
    /// `casters` (indices dans `players`) dans l'ordre des cases.
    fn set_light_casters(
        &mut self,
        queue: &wgpu::Queue,
        jobs: &[crate::lights::EntJob],
        casters: &[u32],
        players: &[PlayerInstance],
    ) {
        self.light_draws.clear();
        let n = casters.len().min(crate::lights::MAX_ENT_CASTERS);
        let list: Vec<PlayerInstance> = casters[..n]
            .iter()
            .filter_map(|&i| players.get(i as usize).copied())
            .collect();
        if list.len() != n || n == 0 {
            return;
        }
        queue.write_buffer(&self.light_instances, 0, bytemuck::cast_slice(&list));
        let mut frames = vec![0u8; jobs.len().min(MAX_LIGHT_JOBS) * LIGHT_JOB_STRIDE as usize];
        for (k, j) in jobs.iter().take(MAX_LIGHT_JOBS).enumerate() {
            let at = k * LIGHT_JOB_STRIDE as usize;
            let bytes = bytemuck::bytes_of(&j.frame);
            frames[at..at + bytes.len()].copy_from_slice(bytes);
            let end = j.casters.end.min(n as u32);
            if j.casters.start < end {
                self.light_draws.push((
                    j.rect,
                    (k as u64 * LIGHT_JOB_STRIDE) as u32,
                    j.casters.start..end,
                ));
            }
        }
        if !frames.is_empty() {
            queue.write_buffer(&self.light_jobs, 0, &frames);
        }
    }

    /// (v220) La passe de l'atlas des joueurs : effacé (distance 1 : personne), puis chaque
    /// case et ses joueurs.
    fn draw_light_casters(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        atlas: &wgpu::TextureView,
        draw: bool,
    ) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("ombres des joueurs aux lumières"),
            color_attachments: &[],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: atlas,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        if !draw || self.mesh_count == 0 || self.light_draws.is_empty() {
            return;
        }
        pass.set_pipeline(&self.light);
        pass.set_vertex_buffer(0, self.mesh.slice(..));
        pass.set_vertex_buffer(1, self.light_instances.slice(..));
        for (rect, offset, range) in &self.light_draws {
            let [x, y, size] = *rect;
            pass.set_viewport(x as f32, y as f32, size as f32, size as f32, 0.0, 1.0);
            pass.set_scissor_rect(x, y, size, size);
            pass.set_bind_group(0, &self.light_group, &[*offset]);
            pass.draw(0..self.mesh_count, range.clone());
        }
    }

    /// (v217) Joueurs de la carte d'ombre du soleil.
    fn set_sun_casters(&mut self, queue: &wgpu::Queue, players: &[PlayerInstance]) {
        let n = players.len().min(MAX_PLAYERS);
        self.sun_count = n as u32;
        if n > 0 {
            queue.write_buffer(&self.sun_instances, 0, bytemuck::cast_slice(&players[..n]));
        }
    }

    /// (v217) Ceux-là, en profondeur seule, avec la vue du soleil (`world` : groupe du monde
    /// de la passe de la carte, ses propres uniformes).
    fn draw_sun<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>, world: &'a wgpu::BindGroup) {
        if self.sun_count == 0 || self.mesh_count == 0 {
            return;
        }
        pass.set_pipeline(&self.sun);
        pass.set_bind_group(0, world, &[]);
        pass.set_vertex_buffer(0, self.mesh.slice(..));
        pass.set_vertex_buffer(1, self.sun_instances.slice(..));
        pass.draw(0..self.mesh_count, 0..self.sun_count);
    }

    fn set_mesh(&mut self, queue: &wgpu::Queue, vertices: &[PlayerVertex]) {
        let n = vertices.len().min(MESH_MAX) / 3 * 3;
        queue.write_buffer(&self.mesh, 0, bytemuck::cast_slice(&vertices[..n]));
        self.mesh_count = n as u32;
    }

    fn set_skin(&self, queue: &wgpu::Queue, layer: u32, rgb: &[[f32; 3]]) {
        if layer >= PLAYER_SKINS || rgb.len() != 64 * 64 {
            return;
        }
        let one = half(1.0);
        let texels: Vec<u16> = rgb
            .iter()
            .flat_map(|c| [half(c[0]), half(c[1]), half(c[2]), one])
            .collect();
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.skins,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: 0,
                    y: 0,
                    z: layer,
                },
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(&texels),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(64 * 8),
                rows_per_image: Some(64),
            },
            wgpu::Extent3d {
                width: 64,
                height: 64,
                depth_or_array_layers: 1,
            },
        );
    }

    fn set_instances(&mut self, queue: &wgpu::Queue, players: &[PlayerInstance]) {
        let n = players.len().min(MAX_PLAYERS);
        self.count = n as u32;
        self.extra = 0;
        if n > 0 {
            queue.write_buffer(&self.instances, 0, bytemuck::cast_slice(&players[..n]));
        }
    }

    /// (v190) Le joueur local, vu seulement dans les miroirs : après les autres.
    fn set_mirror_self(&mut self, queue: &wgpu::Queue, me: Option<PlayerInstance>) {
        self.extra = 0;
        if let Some(me) = me {
            if (self.count as usize) < MAX_PLAYERS {
                let at = self.count as u64 * std::mem::size_of::<PlayerInstance>() as u64;
                queue.write_buffer(&self.instances, at, bytemuck::bytes_of(&me));
                self.extra = 1;
            }
        }
    }

    fn draw<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>, world: &'a wgpu::BindGroup, taa: bool) {
        self.draw_n(pass, world, taa, self.count);
    }

    /// (v202) Les joueurs de l'image des reflets dans l'eau (choisis par `water_mirror`).
    fn set_water(&mut self, queue: &wgpu::Queue, players: &[PlayerInstance]) {
        let n = players.len().min(MAX_PLAYERS);
        self.water_count = n as u32;
        if n > 0 {
            queue.write_buffer(&self.water, 0, bytemuck::cast_slice(&players[..n]));
        }
    }

    /// (v202) Ceux-là, dans l'image des reflets dans l'eau (sans anticrénelage).
    fn draw_water<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>, world: &'a wgpu::BindGroup) {
        if self.water_count == 0 || self.mesh_count == 0 {
            return;
        }
        pass.set_pipeline(&self.plain);
        pass.set_bind_group(0, world, &[]);
        pass.set_bind_group(1, &self.group, &[]);
        pass.set_vertex_buffer(0, self.mesh.slice(..));
        pass.set_vertex_buffer(1, self.water.slice(..));
        pass.draw(0..self.mesh_count, 0..self.water_count);
    }

    /// `n` premières instances (les autres joueurs ; plus soi-même dans un miroir).
    fn draw_n<'a>(
        &'a self,
        pass: &mut wgpu::RenderPass<'a>,
        world: &'a wgpu::BindGroup,
        taa: bool,
        n: u32,
    ) {
        if n == 0 || self.mesh_count == 0 {
            return;
        }
        pass.set_pipeline(if taa { &self.taa } else { &self.plain });
        pass.set_bind_group(0, world, &[]);
        pass.set_bind_group(1, &self.group, &[]);
        pass.set_vertex_buffer(0, self.mesh.slice(..));
        pass.set_vertex_buffer(1, self.instances.slice(..));
        pass.draw(0..self.mesh_count, 0..n);
    }
}

/// Rendu direct (une cible) et rendu avec anticrénelage temporel (couleur + mouvement).
pub(crate) struct EntityPipes {
    /// Sans anticrénelage.
    plain: wgpu::RenderPipeline,
    /// Avec : dans la passe des opaques (albédo et surface pour l'éclairage différé).
    taa: wgpu::RenderPipeline,
    taa_world: wgpu::RenderPipeline,
}

impl EntityPipes {
    pub(crate) fn new(
        device: &wgpu::Device,
        bgl: &wgpu::BindGroupLayout,
        surface: wgpu::TextureFormat,
    ) -> Self {
        let src = crate::lights::without_dynlights(&entity_wgsl());
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("entités"),
            source: wgpu::ShaderSource::Wgsl(src.into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("entités"),
            bind_group_layouts: &[bgl],
            push_constant_ranges: &[],
        });
        const ATTRS: [wgpu::VertexAttribute; 5] = wgpu::vertex_attr_array![
            0 => Float32x3,
            1 => Float32x3,
            2 => Float32x2,
            3 => Sint32,
            4 => Unorm8x4
        ];
        let pipeline = |label: &str, entry: &str, targets: &[Option<wgpu::ColorTargetState>]| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    compilation_options: Default::default(),
                    buffers: &[wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<EntityVertex>() as u64,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &ATTRS,
                    }],
                },
                primitive: wgpu::PrimitiveState {
                    cull_mode: None,
                    ..Default::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: true,
                    depth_compare: wgpu::CompareFunction::GreaterEqual,
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: Default::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(entry),
                    compilation_options: Default::default(),
                    targets,
                }),
                multiview: None,
                cache: None,
            })
        };
        let target = |format| {
            Some(wgpu::ColorTargetState {
                format,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })
        };
        let taa_targets = [
            target(COLOR_FORMAT),
            target(MOTION_FORMAT),
            target(GB_ALBEDO_FORMAT),
            target(GB_SURF_FORMAT),
        ];
        Self {
            plain: pipeline("entités", "fs_main", &[target(surface)]),
            taa: pipeline("entités (TAA)", "fs_taa", &taa_targets),
            taa_world: pipeline("entités fixes (TAA)", "fs_taa_world", &taa_targets),
        }
    }
}

pub struct Entities {
    /// Pipelines de tous les jours (sans les lumières de l'éditeur).
    pipes: EntityPipes,
    /// (v172) Les autres joueurs, instanciés.
    players: Players,
    buffer: wgpu::Buffer,
    count: u32,
    /// (v190) Sommets après `count` dessinés seulement dans les miroirs (ce que tient le
    /// joueur local à la première personne).
    mirror_extra: u32,
    /// Premiers sommets qui suivent la caméra (modèle du joueur local).
    follow: u32,
    /// Entités fixes qui changent rarement (têtes et cadres des cartes) : gardées sur la
    /// carte graphique, renvoyées seulement quand leur version change.
    statics: std::collections::HashMap<u64, (wgpu::Buffer, u32, u64)>,
    /// Lots à dessiner cette image.
    static_draw: Vec<u64>,
}

impl Entities {
    pub fn new(
        device: &wgpu::Device,
        bgl: &wgpu::BindGroupLayout,
        surface: wgpu::TextureFormat,
    ) -> Self {
        let pipes = EntityPipes::new(device, bgl, surface);
        let players = Players::new(device, bgl, surface);
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("entités (sommets)"),
            size: (MAX_VERTICES * std::mem::size_of::<EntityVertex>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            pipes,
            players,
            buffer,
            count: 0,
            mirror_extra: 0,
            follow: 0,
            statics: std::collections::HashMap::new(),
            static_draw: Vec::new(),
        }
    }

    /// Entités fixes par lots : (identifiant, sommets, version). Un lot n'est renvoyé à
    /// la carte graphique que si sa version a changé ; les lots absents sont oubliés.
    pub fn prepare_static(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        lots: &[(u64, &[EntityVertex], u64)],
    ) {
        self.static_draw.clear();
        for &(key, vertices, gen) in lots {
            let n = vertices.len() / 3 * 3;
            if n == 0 {
                continue;
            }
            self.static_draw.push(key);
            if self.statics.get(&key).is_some_and(|e| e.2 == gen) {
                continue;
            }
            let bytes = (n * std::mem::size_of::<EntityVertex>()) as u64;
            let reuse = self.statics.remove(&key).filter(|e| e.0.size() >= bytes);
            let buffer = match reuse {
                Some(e) => e.0,
                None => device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("entités fixes (sommets)"),
                    size: bytes.next_power_of_two(),
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }),
            };
            queue.write_buffer(&buffer, 0, bytemuck::cast_slice(&vertices[..n]));
            self.statics.insert(key, (buffer, n as u32, gen));
        }
        if self.statics.len() > self.static_draw.len() + 64 {
            let keep: std::collections::HashSet<u64> = self.static_draw.iter().copied().collect();
            self.statics.retain(|k, _| keep.contains(k));
        }
    }

    /// (v172) Modèle des autres joueurs (une fois).
    pub fn set_player_mesh(&mut self, queue: &wgpu::Queue, vertices: &[PlayerVertex]) {
        self.players.set_mesh(queue, vertices);
    }

    /// (v172) Image d'un skin (64 × 64 couleurs linéaires, déjà cuite par le jeu) dans la
    /// couche `layer`.
    pub fn set_player_skin(&mut self, queue: &wgpu::Queue, layer: u32, rgb: &[[f32; 3]]) {
        self.players.set_skin(queue, layer, rgb);
    }

    pub fn has_player_mesh(&self) -> bool {
        self.players.mesh_count > 0
    }

    /// (v172) Les autres joueurs de cette image.
    pub fn prepare_players(&mut self, queue: &wgpu::Queue, players: &[PlayerInstance]) {
        self.players.set_instances(queue, players);
    }

    /// (v217) Ceux qui font leur ombre au soleil par la carte (voir `sunmap`).
    pub(crate) fn prepare_sun_casters(&mut self, queue: &wgpu::Queue, players: &[PlayerInstance]) {
        self.players.set_sun_casters(queue, players);
    }

    /// (v220) Joueurs de l'atlas des ombres des joueurs (voir `lights::EntJob`).
    pub(crate) fn prepare_light_casters(
        &mut self,
        queue: &wgpu::Queue,
        jobs: &[crate::lights::EntJob],
        casters: &[u32],
        players: &[PlayerInstance],
    ) {
        self.players
            .set_light_casters(queue, jobs, casters, players);
    }

    /// (v220) Passe de l'atlas des ombres des joueurs : effacé, puis (`draw`) les joueurs de
    /// chaque case.
    pub(crate) fn draw_light_casters(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        atlas: &wgpu::TextureView,
        draw: bool,
    ) {
        self.players.draw_light_casters(encoder, atlas, draw);
    }

    /// (v217) Passe de la carte d'ombre du soleil : les joueurs, vus du soleil.
    pub(crate) fn draw_sun_casters<'a>(
        &'a self,
        pass: &mut wgpu::RenderPass<'a>,
        world: &'a wgpu::BindGroup,
    ) {
        self.players.draw_sun(pass, world);
    }

    /// `follow` : les premiers sommets suivent la caméra (modèle du joueur local), les
    /// autres sont fixés dans le monde.
    pub fn prepare(&mut self, queue: &wgpu::Queue, vertices: &[EntityVertex], follow: usize) {
        let n = vertices.len().min(MAX_VERTICES) / 3 * 3;
        self.count = n as u32;
        self.follow = (follow.min(n) / 3 * 3) as u32;
        if n > 0 {
            queue.write_buffer(&self.buffer, 0, bytemuck::cast_slice(&vertices[..n]));
        }
    }

    /// (v190) Ce que seuls les miroirs montrent, à la première personne : le joueur local
    /// (`me`, après les autres joueurs) et ce qu'il tient (`held`, après les entités de
    /// l'image). À appeler après `prepare` et `prepare_players`.
    pub fn prepare_mirror(
        &mut self,
        queue: &wgpu::Queue,
        me: Option<PlayerInstance>,
        held: &[EntityVertex],
    ) {
        self.players.set_mirror_self(queue, me);
        let room = MAX_VERTICES.saturating_sub(self.count as usize);
        let n = held.len().min(room) / 3 * 3;
        self.mirror_extra = n as u32;
        if n > 0 {
            let at = self.count as u64 * std::mem::size_of::<EntityVertex>() as u64;
            queue.write_buffer(&self.buffer, at, bytemuck::cast_slice(&held[..n]));
        }
    }

    /// (v190) Les entités dans la passe d'un miroir (pipelines sans anticrénelage) : les
    /// autres joueurs et soi-même, les entités fixes, celles de l'image et ce qu'on tient.
    pub(crate) fn draw_mirror<'a>(
        &'a self,
        pass: &mut wgpu::RenderPass<'a>,
        group: &'a wgpu::BindGroup,
    ) {
        let p = &self.pipes;
        self.players
            .draw_n(pass, group, false, self.players.count + self.players.extra);
        pass.set_bind_group(0, group, &[]);
        pass.set_pipeline(&p.plain);
        for key in &self.static_draw {
            if let Some((b, n, _)) = self.statics.get(key) {
                pass.set_vertex_buffer(0, b.slice(..));
                pass.draw(0..*n, 0..1);
            }
        }
        let n = self.count + self.mirror_extra;
        if n > 0 {
            pass.set_vertex_buffer(0, self.buffer.slice(..));
            pass.draw(0..n, 0..1);
        }
    }

    /// (v200) Y a-t-il de quoi refléter dans l'eau (joueurs, entités) ?
    pub(crate) fn any_in_water_mirror(&self) -> bool {
        self.players.count > 0 || self.count > 0 || !self.static_draw.is_empty()
    }

    /// (v202) Les autres joueurs qu'on peut voir reflétés dans l'eau (choisis par
    /// `water_mirror`) : à dessiner dans son image.
    pub(crate) fn prepare_water_players(
        &mut self,
        queue: &wgpu::Queue,
        players: &[PlayerInstance],
    ) {
        self.players.set_water(queue, players);
    }

    /// (v200) Les entités dans l'image des reflets dans l'eau (pipelines sans anticrénelage,
    /// comme dans un miroir) : les autres joueurs (v202 : ceux qu'on peut y voir), les
    /// entités fixes et celles de l'image, sans soi-même à la première personne (sa tête
    /// reflétée boucherait la vue au ras de l'eau ; à la 3e personne, son modèle est parmi
    /// les entités de l'image).
    pub(crate) fn draw_water_mirror<'a>(
        &'a self,
        pass: &mut wgpu::RenderPass<'a>,
        group: &'a wgpu::BindGroup,
    ) {
        let p = &self.pipes;
        self.players.draw_water(pass, group);
        pass.set_bind_group(0, group, &[]);
        pass.set_pipeline(&p.plain);
        for key in &self.static_draw {
            if let Some((b, n, _)) = self.statics.get(key) {
                pass.set_vertex_buffer(0, b.slice(..));
                pass.draw(0..*n, 0..1);
            }
        }
        if self.count > 0 {
            pass.set_vertex_buffer(0, self.buffer.slice(..));
            pass.draw(0..self.count, 0..1);
        }
    }

    /// Dessine les entités dans la passe du monde (`taa` : passe à deux cibles ; `lit` :
    /// pipelines éclairés par les lumières de l'éditeur, s'il y en a).
    pub(crate) fn draw<'a>(
        &'a self,
        pass: &mut wgpu::RenderPass<'a>,
        group: &'a wgpu::BindGroup,
        taa: bool,
    ) {
        let p = &self.pipes;
        let plain = &p.plain;
        // (v172) Les autres joueurs (instanciés), puis les entités de triangles.
        self.players.draw(pass, group, taa);
        pass.set_bind_group(0, group, &[]);
        if !self.static_draw.is_empty() {
            pass.set_pipeline(if taa { &p.taa_world } else { plain });
            for key in &self.static_draw {
                if let Some((b, n, _)) = self.statics.get(key) {
                    pass.set_vertex_buffer(0, b.slice(..));
                    pass.draw(0..*n, 0..1);
                }
            }
        }
        if self.count == 0 {
            return;
        }
        pass.set_vertex_buffer(0, self.buffer.slice(..));
        if !taa {
            pass.set_pipeline(plain);
            pass.draw(0..self.count, 0..1);
            return;
        }
        if self.follow > 0 {
            pass.set_pipeline(&p.taa);
            pass.draw(0..self.follow, 0..1);
        }
        if self.count > self.follow {
            pass.set_pipeline(&p.taa_world);
            pass.draw(self.follow..self.count, 0..1);
        }
    }
}

/// (v172) Pour les tests du jeu : dessine hors écran, avec les vrais pipelines (sans
/// anticrénelage), des triangles d'entités dans une image et des joueurs instanciés dans une
/// autre (`size` × `size`, `size` multiple de 16, couleurs linéaires, fond transparent).
/// `None` sans carte graphique (même logicielle).
#[doc(hidden)]
pub fn draw_offscreen(
    view_proj: glam::Mat4,
    size: u32,
    entities: &[EntityVertex],
    mesh: &[PlayerVertex],
    players: &[PlayerInstance],
    skins: &[(u32, Vec<[f32; 3]>)],
) -> Option<[Vec<[f32; 4]>; 2]> {
    const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba32Float;
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
    let adapter = pollster::block_on(instance.request_adapter(&Default::default())).ok()?;
    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()?;
    // Ce que lisent les entités et les joueurs des uniformes du monde : la caméra, et les
    // images des blocs (ici une seule, vide). (v216) Et le ciel de leur brouillard (liaisons
    // 16 à 21, ici vides : le brouillard est nul ou le même dans les deux images).
    let sky_entries = crate::sky::layout_entries();
    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: None,
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
            sky_entries[0],
            sky_entries[1],
            sky_entries[2],
            sky_entries[3],
            sky_entries[4],
            sky_entries[5],
        ],
    });
    let globals = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: 1024,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut g = [0f32; 256];
    g[..16].copy_from_slice(&view_proj.to_cols_array());
    queue.write_buffer(&globals, 0, bytemuck::cast_slice(&g));
    let blocks = device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let blocks = blocks.create_view(&wgpu::TextureViewDescriptor {
        dimension: Some(wgpu::TextureViewDimension::D2Array),
        ..Default::default()
    });
    let sampler = device.create_sampler(&Default::default());
    // Le ciel : une image cube et une carte des nuages d'un pixel, des uniformes à zéro.
    let empty = |layers: u32| {
        device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: layers,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        })
    };
    let sky_cube = empty(6).create_view(&wgpu::TextureViewDescriptor {
        dimension: Some(wgpu::TextureViewDimension::Cube),
        ..Default::default()
    });
    let sky_map = empty(1).create_view(&Default::default());
    let sky_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });
    let sky_uniform = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: std::mem::size_of::<crate::sky::SkyGlobals>() as u64,
        usage: wgpu::BufferUsages::UNIFORM,
        mapped_at_creation: false,
    });
    let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &bgl,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: globals.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(&blocks),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::Sampler(&sampler),
            },
            wgpu::BindGroupEntry {
                binding: 16,
                resource: wgpu::BindingResource::TextureView(&sky_cube),
            },
            wgpu::BindGroupEntry {
                binding: 17,
                resource: wgpu::BindingResource::TextureView(&sky_map),
            },
            wgpu::BindGroupEntry {
                binding: 18,
                resource: wgpu::BindingResource::TextureView(&sky_map),
            },
            wgpu::BindGroupEntry {
                binding: 19,
                resource: wgpu::BindingResource::TextureView(&sky_map),
            },
            wgpu::BindGroupEntry {
                binding: 20,
                resource: wgpu::BindingResource::Sampler(&sky_sampler),
            },
            wgpu::BindGroupEntry {
                binding: 21,
                resource: sky_uniform.as_entire_binding(),
            },
        ],
    });
    let pipes = EntityPipes::new(&device, &bgl, FORMAT);
    let mut gpu = Players::new(&device, &bgl, FORMAT);
    gpu.set_mesh(&queue, mesh);
    for (layer, rgb) in skins {
        gpu.set_skin(&queue, *layer, rgb);
    }
    gpu.set_instances(&queue, players);
    let tris = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: (std::mem::size_of_val(entities) as u64).max(64),
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    queue.write_buffer(&tris, 0, bytemuck::cast_slice(entities));
    let image = |instanced: bool| -> Option<Vec<[f32; 4]>> {
        let target = |format, usage| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: None,
                size: wgpu::Extent3d {
                    width: size,
                    height: size,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            })
        };
        let color = target(
            FORMAT,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        );
        let depth = target(DEPTH_FORMAT, wgpu::TextureUsages::RENDER_ATTACHMENT);
        let (cv, dv) = (
            color.create_view(&Default::default()),
            depth.create_view(&Default::default()),
        );
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &cv,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &dv,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(0.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            if instanced {
                gpu.draw(&mut pass, &group, false);
            } else if !entities.is_empty() {
                pass.set_pipeline(&pipes.plain);
                pass.set_bind_group(0, &group, &[]);
                pass.set_vertex_buffer(0, tris.slice(..));
                pass.draw(0..entities.len() as u32, 0..1);
            }
        }
        let out = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: (size * size * 16) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            color.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &out,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(size * 16),
                    rows_per_image: Some(size),
                },
            },
            color.size(),
        );
        queue.submit([encoder.finish()]);
        let slice = out.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r.is_ok());
        });
        let _ = device.poll(wgpu::PollType::Wait);
        if !rx.recv().unwrap_or(false) {
            return None;
        }
        let pixels = bytemuck::cast_slice::<u8, [f32; 4]>(&slice.get_mapped_range()).to_vec();
        Some(pixels)
    };
    Some([image(false)?, image(true)?])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Les attributs d'instance lus par la carte tombent bien sur les champs de
    /// `PlayerInstance` : `vertex_attr_array!` enchaîne les décalages dans l'ordre déclaré,
    /// donc un champ ajouté ailleurs qu'à la fin décalerait tout (joueurs déformés).
    #[test]
    fn instance_attributes_follow_the_struct_fields() {
        let want = [
            (4, std::mem::offset_of!(PlayerInstance, feet)),
            (5, std::mem::offset_of!(PlayerInstance, yaw)),
            (6, std::mem::offset_of!(PlayerInstance, walk)),
            (7, std::mem::offset_of!(PlayerInstance, light)),
            (8, std::mem::offset_of!(PlayerInstance, skin)),
            (10, std::mem::offset_of!(PlayerInstance, prev_feet)),
            (11, std::mem::offset_of!(PlayerInstance, prev_yaw)),
            (12, std::mem::offset_of!(PlayerInstance, prev_walk)),
        ];
        assert_eq!(PLAYER_ATTRS.len(), want.len());
        for (a, (location, offset)) in PLAYER_ATTRS.iter().zip(want) {
            assert_eq!((a.shader_location, a.offset), (location, offset as u64));
        }
        // Les champs lus deux par deux ou trois par trois se suivent vraiment.
        assert_eq!(std::mem::offset_of!(PlayerInstance, pitch), 16);
        assert_eq!(std::mem::offset_of!(PlayerInstance, swing), 24);
        assert_eq!(std::mem::offset_of!(PlayerInstance, crouch), 28);
        assert_eq!(std::mem::offset_of!(PlayerInstance, prev_pitch), 64);
        assert_eq!(std::mem::offset_of!(PlayerInstance, prev_swing), 72);
        assert_eq!(std::mem::offset_of!(PlayerInstance, prev_crouch), 76);
    }

    #[test]
    fn half_floats_round_to_nearest() {
        assert_eq!(half(0.0), 0);
        assert_eq!(half(1.0), 0x3c00);
        assert_eq!(half(0.5), 0x3800);
        assert_eq!(half(2.0), 0x4000);
        assert_eq!(half(0.1), 0x2e66);
        assert_eq!(half(65504.0), 0x7bff);
        assert_eq!(half(1e9), 0x7bff);
        assert_eq!(half(-1.0), 0);
        assert_eq!(half(f32::NAN), 0);
        // Plus petit normal, et un dénormal.
        assert_eq!(half(6.103_515_6e-5), 0x0400);
        assert_eq!(half(5.960_464_5e-8), 1);
        // Tous les demi-flottants positifs finis reviennent à eux-mêmes.
        for h in 0u16..0x7c00 {
            let e = (h >> 10) as i32;
            let m = (h & 0x3ff) as f32;
            let x = if e == 0 {
                m * 2f32.powi(-24)
            } else {
                (1.0 + m / 1024.0) * 2f32.powi(e - 15)
            };
            assert_eq!(half(x), h, "{x}");
        }
    }

    #[test]
    fn player_buffers_match_the_shader_layout() {
        assert_eq!(std::mem::size_of::<PlayerVertex>(), 52);
        // (48, plus la pose de l'image d'avant : pieds, regard, marche.)
        assert_eq!(std::mem::size_of::<PlayerInstance>(), 80);
    }
}
