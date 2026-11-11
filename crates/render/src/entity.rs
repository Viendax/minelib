//! Entités dans le monde (modèle du joueur vu à la 3e personne…) : triangles en
//! coordonnées monde, de couleur unie déjà éclairée par le jeu, dessinés dans la passe du
//! monde avec sa profondeur (cachés par les blocs) et la caméra du monde (tremblement de
//! l'anticrénelage temporel compris).

use bytemuck::{Pod, Zeroable};

use crate::taa::{COLOR_FORMAT, MOTION_FORMAT};
use crate::DEPTH_FORMAT;

/// Sommet d'entité : position monde et couleur linéaire, multipliée par la texture de
/// bloc `layer` aux coordonnées `uv` si `layer` ≥ 0 (objet tenu).
#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
pub struct EntityVertex {
    pub pos: [f32; 3],
    pub color: [f32; 3],
    pub uv: [f32; 2],
    pub layer: i32,
}

/// Au plus tant de sommets d'entités par image.
const MAX_VERTICES: usize = 65_536;

pub struct Entities {
    /// Rendu direct (une cible) et rendu avec anticrénelage temporel (couleur + mouvement).
    plain: wgpu::RenderPipeline,
    taa: wgpu::RenderPipeline,
    buffer: wgpu::Buffer,
    count: u32,
}

impl Entities {
    pub fn new(
        device: &wgpu::Device,
        bgl: &wgpu::BindGroupLayout,
        surface: wgpu::TextureFormat,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("entités"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/entity.wgsl").into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("entités"),
            bind_group_layouts: &[bgl],
            push_constant_ranges: &[],
        });
        const ATTRS: [wgpu::VertexAttribute; 4] = wgpu::vertex_attr_array![
            0 => Float32x3,
            1 => Float32x3,
            2 => Float32x2,
            3 => Sint32
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
        let plain = pipeline("entités", "fs_main", &[target(surface)]);
        let taa = pipeline(
            "entités (TAA)",
            "fs_taa",
            &[target(COLOR_FORMAT), target(MOTION_FORMAT)],
        );
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("entités (sommets)"),
            size: (MAX_VERTICES * std::mem::size_of::<EntityVertex>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            plain,
            taa,
            buffer,
            count: 0,
        }
    }

    pub fn prepare(&mut self, queue: &wgpu::Queue, vertices: &[EntityVertex]) {
        let n = vertices.len().min(MAX_VERTICES) / 3 * 3;
        self.count = n as u32;
        if n > 0 {
            queue.write_buffer(&self.buffer, 0, bytemuck::cast_slice(&vertices[..n]));
        }
    }

    /// Dessine les entités dans la passe du monde (`taa` : passe à deux cibles).
    pub fn draw<'a>(
        &'a self,
        pass: &mut wgpu::RenderPass<'a>,
        group: &'a wgpu::BindGroup,
        taa: bool,
    ) {
        if self.count == 0 {
            return;
        }
        pass.set_pipeline(if taa { &self.taa } else { &self.plain });
        pass.set_bind_group(0, group, &[]);
        pass.set_vertex_buffer(0, self.buffer.slice(..));
        pass.draw(0..self.count, 0..1);
    }
}
