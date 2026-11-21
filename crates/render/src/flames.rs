//! (v216) Flammes du molotov : un liquide qui brûle au sol, comme de l'alcool (voir
//! flame.wgsl). (v217) Le jeu fournit chaque image la liste des cases en feu ; chacune est
//! une boîte que la carte graphique traverse pas à pas dans un champ de flammes posé dans
//! le monde : un vrai volume, pas des panneaux tournés vers l'œil. Sans feu, rien n'est
//! dessiné ni calculé.

use bytemuck::{Pod, Zeroable};

use crate::DEPTH_FORMAT;

/// Une case en feu.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct Flame {
    /// Milieu de la case, au sol (monde), et hauteur de ses plus hautes flammes (blocs).
    pub pos: [f32; 3],
    pub height: f32,
    /// Graine : la lueur au sol de chaque case vacille à sa façon.
    pub seed: f32,
    /// Force (0 : éteinte … 1).
    pub force: f32,
    /// Côtés où la case voisine brûle aussi (bits : 1 +x, 2 −x, 4 +z, 8 −z) : ailleurs, le
    /// feu s'arrête en s'amincissant.
    pub sides: f32,
    pub _pad: f32,
}

/// Au plus autant de flammes par image.
pub const MAX_FLAMES: usize = 1024;

pub struct Flames {
    buffer: wgpu::Buffer,
    count: u32,
    group: wgpu::BindGroup,
    /// Dans la passe du monde (sans anticrénelage) ; après l'anticrénelage, sur l'image
    /// finale (voir `bolts::SceneDepth`).
    plain: wgpu::RenderPipeline,
    post: wgpu::RenderPipeline,
}

impl Flames {
    pub fn new(
        device: &wgpu::Device,
        world_bgl: &wgpu::BindGroupLayout,
        depth_bgl: &wgpu::BindGroupLayout,
        noise: (&wgpu::TextureView, &wgpu::Sampler),
        surface: wgpu::TextureFormat,
    ) -> Self {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("flammes"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D3,
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
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("flammes"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(noise.0),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(noise.1),
                },
            ],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("flammes"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/flame.wgsl").into()),
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("flammes"),
            bind_group_layouts: &[world_bgl, &layout],
            push_constant_ranges: &[],
        });
        let pl_post = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("flammes (après l'anticrénelage)"),
            bind_group_layouts: &[world_bgl, &layout, depth_bgl],
            push_constant_ranges: &[],
        });
        // La lumière des flammes s'ajoute à l'image ; leur corps jaune la couvre en partie
        // (le shader dit combien : son alpha).
        let add = wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::Zero,
                dst_factor: wgpu::BlendFactor::One,
                operation: wgpu::BlendOperation::Add,
            },
        };
        let attrs = wgpu::vertex_attr_array![0 => Float32x4, 1 => Float32x4];
        // `post` : après l'anticrénelage, les faces arrière de la boîte (dessinées même
        // l'œil dedans), et le shader lit la profondeur du monde ; sinon ses faces avant,
        // cachées par ce qui est devant.
        let pipeline = |post: bool| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("flammes"),
                layout: Some(if post { &pl_post } else { &pl }),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some(if post { "vs_post" } else { "vs_main" }),
                    compilation_options: Default::default(),
                    buffers: &[wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<Flame>() as u64,
                        step_mode: wgpu::VertexStepMode::Instance,
                        attributes: &attrs,
                    }],
                },
                primitive: wgpu::PrimitiveState {
                    cull_mode: Some(if post {
                        wgpu::Face::Front
                    } else {
                        wgpu::Face::Back
                    }),
                    ..Default::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: false,
                    depth_compare: if post {
                        wgpu::CompareFunction::Always
                    } else {
                        wgpu::CompareFunction::GreaterEqual
                    },
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: Default::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(if post { "fs_post" } else { "fs_main" }),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: surface,
                        blend: Some(add),
                        write_mask: wgpu::ColorWrites::COLOR,
                    })],
                }),
                multiview: None,
                cache: None,
            })
        };
        Self {
            buffer: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("flammes"),
                size: (MAX_FLAMES * std::mem::size_of::<Flame>()) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            count: 0,
            group,
            plain: pipeline(false),
            post: pipeline(true),
        }
    }

    /// Cases en feu de l'image, vues de `eye` : dessinées de la plus lointaine à la plus
    /// proche (chacune couvre un peu ce qui est derrière elle).
    pub fn prepare(&mut self, queue: &wgpu::Queue, flames: &[Flame], eye: glam::Vec3) {
        let n = flames.len().min(MAX_FLAMES);
        if n > 0 {
            let mut sorted = flames[..n].to_vec();
            let far = |f: &Flame| (glam::Vec3::from(f.pos) - eye).length_squared();
            sorted.sort_by(|a, b| far(b).total_cmp(&far(a)));
            queue.write_buffer(&self.buffer, 0, bytemuck::cast_slice(&sorted));
        }
        self.count = n as u32;
    }

    /// Y a-t-il du feu à dessiner ?
    pub fn any(&self) -> bool {
        self.count > 0
    }

    /// Flammes, dans la passe du monde (sans anticrénelage) ; ou, avec `depth` (la
    /// profondeur du monde), après l'anticrénelage sur l'image finale.
    pub fn draw<'a>(
        &'a self,
        pass: &mut wgpu::RenderPass<'a>,
        world: &'a wgpu::BindGroup,
        depth: Option<&'a wgpu::BindGroup>,
    ) {
        if self.count == 0 {
            return;
        }
        pass.set_pipeline(if depth.is_some() {
            &self.post
        } else {
            &self.plain
        });
        pass.set_bind_group(0, world, &[]);
        pass.set_bind_group(1, &self.group, &[]);
        if let Some(d) = depth {
            pass.set_bind_group(2, d, &[]);
        }
        pass.set_vertex_buffer(0, self.buffer.slice(..));
        pass.draw(0..36, 0..self.count);
    }
}
