//! (v216) Éclairs : la foudre, l'arc de la Foudre de Zeus tenue en main et ses étincelles.
//! Le jeu fournit chaque image une liste de segments lumineux ; chacun est dessiné comme un
//! rectangle tourné vers l'écran (un cœur blanc très fin, un halo bleuté), ajouté à l'image.
//! Deux listes : dans le monde (cachés par les murs devant eux), et à la première personne
//! (dans la main, par-dessus la main).

use bytemuck::{Pod, Zeroable};

use crate::DEPTH_FORMAT;

/// Profondeur du monde de l'image, pour ce qui est dessiné après l'anticrénelage (éclairs,
/// pluie) : nets dès la première image et sans traînée, ils sont posés sur l'image finale,
/// et c'est leur shader qui lit cette profondeur pour savoir ce qui les cache.
pub struct SceneDepth {
    pub layout: wgpu::BindGroupLayout,
    group: Option<wgpu::BindGroup>,
}

impl SceneDepth {
    pub fn new(device: &wgpu::Device) -> Self {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("profondeur du monde"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Depth,
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            }],
        });
        Self {
            layout,
            group: None,
        }
    }

    /// Profondeur de l'image en cours (`None` : rien à dessiner après l'anticrénelage).
    pub fn prepare(&mut self, device: &wgpu::Device, depth: Option<&wgpu::TextureView>) {
        self.group = depth.map(|view| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("profondeur du monde"),
                layout: &self.layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(view),
                }],
            })
        });
    }

    pub fn group(&self) -> Option<&wgpu::BindGroup> {
        self.group.as_ref()
    }
}

/// Un segment lumineux.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct BoltSeg {
    /// Extrémités : dans le monde (xyz, w libre), ou déjà à l'écran (clip space, comme
    /// `VmVertex::clip`) si `screen` vaut 1.
    pub a: [f32; 4],
    pub b: [f32; 4],
    /// Teinte du halo (linéaire), et force à l'extrémité `a` (1 : plein éclat).
    pub color: [f32; 3],
    pub force_a: f32,
    /// Force à l'extrémité `b`.
    pub force_b: f32,
    /// Demi-largeur du halo en `a` et en `b` : en blocs dans le monde, en part de la
    /// hauteur de l'écran sinon.
    pub width_a: f32,
    pub width_b: f32,
    /// 1 : extrémités déjà à l'écran (première personne).
    pub screen: f32,
}

/// Teinte du halo de la foudre (linéaire) : bleu-violet. La même pour la foudre du ciel,
/// l'arc tenu en main et ses étincelles.
pub const BOLT_TINT: [f32; 3] = [0.42, 0.56, 1.0];

/// Couleur de la lumière que jette un éclair (linéaire).
pub const BOLT_LIGHT: [f32; 3] = [0.72, 0.82, 1.0];

struct Batch {
    buffer: wgpu::Buffer,
    capacity: usize,
    count: u32,
}

impl Batch {
    fn new(device: &wgpu::Device, label: &'static str) -> Self {
        let capacity = 256;
        Self {
            buffer: Self::alloc(device, label, capacity),
            capacity,
            count: 0,
        }
    }

    fn alloc(device: &wgpu::Device, label: &'static str, capacity: usize) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: (capacity * std::mem::size_of::<BoltSeg>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    fn set(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        label: &'static str,
        segs: &[BoltSeg],
    ) {
        let n = segs.len().min(MAX_SEGS);
        if n > self.capacity {
            self.capacity = n.next_power_of_two();
            self.buffer = Self::alloc(device, label, self.capacity);
        }
        if n > 0 {
            queue.write_buffer(&self.buffer, 0, bytemuck::cast_slice(&segs[..n]));
        }
        self.count = n as u32;
    }
}

/// Au plus autant de segments par liste et par image.
pub const MAX_SEGS: usize = 16_384;

pub struct Bolts {
    world: Batch,
    hand: Batch,
    /// Dans le monde : dans la passe du monde (sans anticrénelage), ou après
    /// l'anticrénelage, sur l'image finale (voir `SceneDepth`).
    plain: wgpu::RenderPipeline,
    post: wgpu::RenderPipeline,
    /// À la première personne, par-dessus la main.
    over: wgpu::RenderPipeline,
}

impl Bolts {
    pub fn new(
        device: &wgpu::Device,
        world_bgl: &wgpu::BindGroupLayout,
        depth_bgl: &wgpu::BindGroupLayout,
        surface: wgpu::TextureFormat,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("éclairs"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/bolt.wgsl").into()),
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("éclairs"),
            bind_group_layouts: &[world_bgl],
            push_constant_ranges: &[],
        });
        let pl_post = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("éclairs (après l'anticrénelage)"),
            bind_group_layouts: &[world_bgl, depth_bgl],
            push_constant_ranges: &[],
        });
        // La lumière s'ajoute à l'image.
        let add = wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::One,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::Zero,
                dst_factor: wgpu::BlendFactor::One,
                operation: wgpu::BlendOperation::Add,
            },
        };
        let attrs = wgpu::vertex_attr_array![0 => Float32x4, 1 => Float32x4, 2 => Float32x4, 3 => Float32x4];
        // `post` : après l'anticrénelage (le shader lit la profondeur du monde) ; `tested` :
        // caché par ce qui est devant dans la passe en cours.
        let pipeline = |post: bool, tested: bool| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("éclairs"),
                layout: Some(if post { &pl_post } else { &pl }),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some(if post { "vs_post" } else { "vs_main" }),
                    compilation_options: Default::default(),
                    buffers: &[wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<BoltSeg>() as u64,
                        step_mode: wgpu::VertexStepMode::Instance,
                        attributes: &attrs,
                    }],
                },
                primitive: wgpu::PrimitiveState {
                    cull_mode: None,
                    ..Default::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: false,
                    depth_compare: if tested {
                        wgpu::CompareFunction::GreaterEqual
                    } else {
                        wgpu::CompareFunction::Always
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
            world: Batch::new(device, "éclairs (monde)"),
            hand: Batch::new(device, "éclairs (main)"),
            plain: pipeline(false, true),
            post: pipeline(true, false),
            over: pipeline(false, false),
        }
    }

    /// Segments de l'image : dans le monde, et à la première personne.
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        world: &[BoltSeg],
        hand: &[BoltSeg],
    ) {
        self.world.set(device, queue, "éclairs (monde)", world);
        self.hand.set(device, queue, "éclairs (main)", hand);
    }

    /// Éclairs du monde, dans la passe du monde (sans anticrénelage).
    pub fn draw<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>, world: &'a wgpu::BindGroup) {
        if self.world.count == 0 {
            return;
        }
        pass.set_pipeline(&self.plain);
        pass.set_bind_group(0, world, &[]);
        pass.set_vertex_buffer(0, self.world.buffer.slice(..));
        pass.draw(0..6, 0..self.world.count);
    }

    /// Éclairs du monde après l'anticrénelage, sur l'image finale ; `depth` : la
    /// profondeur du monde (`SceneDepth`).
    pub fn draw_post<'a>(
        &'a self,
        pass: &mut wgpu::RenderPass<'a>,
        world: &'a wgpu::BindGroup,
        depth: &'a wgpu::BindGroup,
    ) {
        if self.world.count == 0 {
            return;
        }
        pass.set_pipeline(&self.post);
        pass.set_bind_group(0, world, &[]);
        pass.set_bind_group(1, depth, &[]);
        pass.set_vertex_buffer(0, self.world.buffer.slice(..));
        pass.draw(0..6, 0..self.world.count);
    }

    /// Y a-t-il des éclairs à dessiner dans le monde ?
    pub fn any(&self) -> bool {
        self.world.count > 0
    }

    /// Éclairs tenus en main, par-dessus la main (passe de la main et de l'interface).
    pub fn draw_hand<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>, world: &'a wgpu::BindGroup) {
        if self.hand.count == 0 {
            return;
        }
        pass.set_pipeline(&self.over);
        pass.set_bind_group(0, world, &[]);
        pass.set_vertex_buffer(0, self.hand.buffer.slice(..));
        pass.draw(0..6, 0..self.hand.count);
    }
}
