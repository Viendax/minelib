//! Main et objet tenu à la première personne : le jeu fournit des triangles déjà projetés
//! (`VmVertex`), dessinés en dernier avec leur propre profondeur, sans anticrénelage
//! temporel (pas de flou ni de tremblement sur l'objet en main).

use bytemuck::{Pod, Zeroable};

use crate::DEPTH_FORMAT;

/// Couche « couleur unie » (pas de texture).
pub const NO_TEXTURE: u32 = u32::MAX;
/// Couche « flamme » : animée comme les torches posées ; `uv[1]` = hauteur dans le bloc,
/// couleur = teinte sRGB de la flamme.
pub const FLAME: u32 = u32::MAX - 1;

#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
pub struct VmVertex {
    /// Position écran (clip space, Z inversé).
    pub clip: [f32; 4],
    pub uv: [f32; 2],
    /// Couche du tableau de textures des blocs, ou `NO_TEXTURE`.
    pub layer: u32,
    pub _pad: u32,
    /// Couleur (linéaire) : multiplie la texture, ou couleur unie.
    pub color: [f32; 4],
}

const MAX_VERTICES: usize = 32768;

pub struct ViewModel {
    pipeline: wgpu::RenderPipeline,
    buffer: wgpu::Buffer,
    count: u32,
}

impl ViewModel {
    pub fn new(
        device: &wgpu::Device,
        world_bgl: &wgpu::BindGroupLayout,
        format: wgpu::TextureFormat,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::include_wgsl!("shaders/viewmodel.wgsl"));
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("main"),
            bind_group_layouts: &[world_bgl],
            push_constant_ranges: &[],
        });
        // clip (0), uv (16), couche (24), remplissage (28), couleur (32)
        const ATTRS: [wgpu::VertexAttribute; 4] = [
            wgpu::VertexAttribute {
                offset: 0,
                shader_location: 0,
                format: wgpu::VertexFormat::Float32x4,
            },
            wgpu::VertexAttribute {
                offset: 16,
                shader_location: 1,
                format: wgpu::VertexFormat::Float32x2,
            },
            wgpu::VertexAttribute {
                offset: 24,
                shader_location: 2,
                format: wgpu::VertexFormat::Uint32,
            },
            wgpu::VertexAttribute {
                offset: 32,
                shader_location: 3,
                format: wgpu::VertexFormat::Float32x4,
            },
        ];
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("main (première personne)"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<VmVertex>() as u64,
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
                depth_compare: wgpu::CompareFunction::Greater,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview: None,
            cache: None,
        });
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("main (sommets)"),
            size: (MAX_VERTICES * std::mem::size_of::<VmVertex>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            pipeline,
            buffer,
            count: 0,
        }
    }

    pub fn prepare(&mut self, queue: &wgpu::Queue, vertices: &[VmVertex]) {
        let n = vertices.len().min(MAX_VERTICES);
        self.count = n as u32;
        if n > 0 {
            queue.write_buffer(&self.buffer, 0, bytemuck::cast_slice(&vertices[..n]));
        }
    }

    /// Envoie la main puis les objets 3D de l'interface ; renvoie le nombre de sommets de
    /// la main (les objets suivent).
    pub fn prepare_with(&mut self, queue: &wgpu::Queue, hand: &[VmVertex], ui: &[VmVertex]) -> u32 {
        // Deux envois directs (main, puis objets de l'interface), sans copie intermédiaire.
        let h = hand.len().min(MAX_VERTICES);
        let u = ui.len().min(MAX_VERTICES - h);
        let size = std::mem::size_of::<VmVertex>() as u64;
        if h > 0 {
            queue.write_buffer(&self.buffer, 0, bytemuck::cast_slice(&hand[..h]));
        }
        if u > 0 {
            queue.write_buffer(
                &self.buffer,
                h as u64 * size,
                bytemuck::cast_slice(&ui[..u]),
            );
        }
        self.count = (h + u) as u32;
        h as u32
    }

    /// À dessiner dans une passe dont la profondeur vient d'être effacée.
    pub fn draw<'p>(&'p self, pass: &mut wgpu::RenderPass<'p>, world_group: &'p wgpu::BindGroup) {
        self.draw_range(pass, world_group, 0..self.count);
    }

    pub fn draw_range<'p>(
        &'p self,
        pass: &mut wgpu::RenderPass<'p>,
        world_group: &'p wgpu::BindGroup,
        range: std::ops::Range<u32>,
    ) {
        let range = range.start.min(self.count)..range.end.min(self.count);
        if range.is_empty() {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, world_group, &[]);
        pass.set_vertex_buffer(0, self.buffer.slice(..));
        pass.draw(range, 0..1);
    }
}
