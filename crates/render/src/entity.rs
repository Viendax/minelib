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
}

impl PlayerInstance {
    /// Bit de `skin` : bras droit levé (objet tenu).
    pub const HOLDS: u32 = 1 << 16;
    /// (v177) Bit de `skin` : arme à feu en main, les deux bras tendus vers le regard.
    pub const AIMS: u32 = 1 << 17;
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

/// Joueurs dessinés au plus par image, et skins différents gardés sur la carte graphique.
pub const MAX_PLAYERS: usize = 8192;
pub const PLAYER_SKINS: u32 = 64;

/// Sommets au plus du modèle commun (six pavés : 216).
const MESH_MAX: usize = 512;

/// Les autres joueurs : un modèle, une instance chacun, une couche de skin par skin.
struct Players {
    plain: wgpu::RenderPipeline,
    taa: wgpu::RenderPipeline,
    mesh: wgpu::Buffer,
    mesh_count: u32,
    instances: wgpu::Buffer,
    count: u32,
    skins: wgpu::Texture,
    group: wgpu::BindGroup,
}

impl Players {
    fn new(
        device: &wgpu::Device,
        world_bgl: &wgpu::BindGroupLayout,
        surface: wgpu::TextureFormat,
    ) -> Self {
        let src = crate::lights::without_dynlights(&format!(
            "{}\n{}",
            include_str!("shaders/entity.wgsl"),
            include_str!("shaders/players.wgsl")
        ));
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
        const INSTANCE: [wgpu::VertexAttribute; 5] = wgpu::vertex_attr_array![
            4 => Float32x3,
            5 => Float32x2,
            6 => Float32x3,
            7 => Float32x3,
            8 => Uint32
        ];
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
        Self {
            plain: pipeline("joueurs", "fs_player", &[target(surface)]),
            taa: pipeline("joueurs (TAA)", "fs_player_taa", &taa_targets),
            mesh,
            mesh_count: 0,
            instances,
            count: 0,
            skins,
            group,
        }
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
        if n > 0 {
            queue.write_buffer(&self.instances, 0, bytemuck::cast_slice(&players[..n]));
        }
    }

    fn draw<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>, world: &'a wgpu::BindGroup, taa: bool) {
        if self.count == 0 || self.mesh_count == 0 {
            return;
        }
        pass.set_pipeline(if taa { &self.taa } else { &self.plain });
        pass.set_bind_group(0, world, &[]);
        pass.set_bind_group(1, &self.group, &[]);
        pass.set_vertex_buffer(0, self.mesh.slice(..));
        pass.set_vertex_buffer(1, self.instances.slice(..));
        pass.draw(0..self.mesh_count, 0..self.count);
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
        let src = crate::lights::without_dynlights(include_str!("shaders/entity.wgsl"));
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
    // images des blocs (ici une seule, vide).
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
        assert_eq!(std::mem::size_of::<PlayerInstance>(), 48);
    }
}
