//! Entités dans le monde (modèle du joueur vu à la 3e personne…) : triangles en
//! coordonnées monde, de couleur unie déjà éclairée par le jeu, dessinés dans la passe du
//! monde avec sa profondeur (cachés par les blocs) et la caméra du monde (tremblement de
//! l'anticrénelage temporel compris).

use bytemuck::{Pod, Zeroable};

use crate::taa::{COLOR_FORMAT, MOTION_FORMAT};
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
    pub layer: i32,
    /// Éclairage du lieu (rgb, 0..2 ramené à 0..255, voir `EntityVertex::light`) ; a = 255 :
    /// éclairé aussi par les lumières dynamiques, 0 : lumineux par lui-même (flamme).
    pub light: [u8; 4],
}

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

/// Rendu direct (une cible) et rendu avec anticrénelage temporel (couleur + mouvement).
pub(crate) struct EntityPipes {
    plain: wgpu::RenderPipeline,
    taa: wgpu::RenderPipeline,
    taa_world: wgpu::RenderPipeline,
}

impl EntityPipes {
    /// `lit` : variante éclairée par les lumières de l'éditeur (compilée à la demande).
    pub(crate) fn new(
        device: &wgpu::Device,
        bgl: &wgpu::BindGroupLayout,
        surface: wgpu::TextureFormat,
        lit: bool,
    ) -> Self {
        let src = include_str!("shaders/entity.wgsl");
        let src = if lit {
            crate::lights::with_dynlights(src)
        } else {
            crate::lights::without_dynlights(src)
        };
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
        Self {
            plain: pipeline("entités", "fs_main", &[target(surface)]),
            taa: pipeline(
                "entités (TAA)",
                "fs_taa",
                &[target(COLOR_FORMAT), target(MOTION_FORMAT)],
            ),
            taa_world: pipeline(
                "entités fixes (TAA)",
                "fs_taa_world",
                &[target(COLOR_FORMAT), target(MOTION_FORMAT)],
            ),
        }
    }
}

pub struct Entities {
    /// Pipelines de tous les jours (sans les lumières de l'éditeur).
    pipes: EntityPipes,
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
        let pipes = EntityPipes::new(device, bgl, surface, false);
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("entités (sommets)"),
            size: (MAX_VERTICES * std::mem::size_of::<EntityVertex>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            pipes,
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
        lit: Option<&'a EntityPipes>,
    ) {
        let p = lit.unwrap_or(&self.pipes);
        pass.set_bind_group(0, group, &[]);
        if !self.static_draw.is_empty() {
            pass.set_pipeline(if taa { &p.taa_world } else { &p.plain });
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
            pass.set_pipeline(&p.plain);
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
