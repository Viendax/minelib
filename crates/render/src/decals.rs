//! (v249) Traces laissées sur les blocs en mode FPS : sang, impacts de balles, entailles du
//! couteau, suie des grenades, sol brûlé par le molotov (voir decal.wgsl). Le jeu en tient la
//! liste (elles restent jusqu'à la fin de la manche) et la donne à chaque image avec son
//! numéro de révision : la carte graphique ne la reçoit que quand elle a changé. Chaque
//! trace est une boîte mince que la carte dessine d'un seul appel, toutes ensemble. Sans
//! trace, rien n'est dessiné ni envoyé.

use bytemuck::{Pod, Zeroable};

use crate::DEPTH_FORMAT;

/// Une trace sur une surface.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct Decal {
    /// Milieu de la trace, sur la surface (monde).
    pub pos: [f32; 3],
    /// Sorte (`KIND_…`). Sol brûlé : plus un seizième par côté où la case voisine a brûlé
    /// aussi (bits : 1 +x, 2 −x, 4 +z, 8 −z).
    pub kind: f32,
    /// Normale de la surface.
    pub normal: [f32; 3],
    /// Graine : chaque trace a sa forme.
    pub seed: f32,
    /// Axe u de la trace, dans la surface (v = normale × u) : le sens où allait le sang
    /// d'une giclée, l'horizontale d'une coulure (v : le haut).
    pub tangent: [f32; 3],
    /// Heure de naissance (s, horloge de `Decals::prepare`) : le sang sèche, une coulure
    /// descend, le sol noircit sous le feu.
    pub born: f32,
    /// Demi-côtés le long de u et de v (blocs).
    pub size: [f32; 2],
    /// Demi-épaisseur de la boîte (blocs) : la trace épouse ce qui est à moins de ça de sa
    /// surface.
    pub depth: f32,
    /// Force (0 … 1).
    pub force: f32,
}

/// Tache de sang ronde, entourée de gouttes.
pub const KIND_SPLAT: f32 = 1.0;
/// Giclée de sang, étalée le long de `tangent`.
pub const KIND_SPRAY: f32 = 2.0;
/// Coulure de sang le long d'un mur (elle descend peu à peu).
pub const KIND_DRIP: f32 = 3.0;
/// Impact de balle.
pub const KIND_HOLE: f32 = 4.0;
/// Entaille du couteau, le long de `tangent`.
pub const KIND_SLASH: f32 = 5.0;
/// Suie d'une explosion.
pub const KIND_SCORCH: f32 = 6.0;
/// Sol brûlé par le molotov (une case).
pub const KIND_BURN: f32 = 7.0;

/// Au plus autant de traces à la fois (le jeu retire les plus vieilles au-delà).
pub const MAX_DECALS: usize = 4096;

pub struct Decals {
    buffer: wgpu::Buffer,
    count: u32,
    /// Révision de la liste déjà envoyée à la carte.
    rev: Option<u64>,
    clock: wgpu::Buffer,
    group: wgpu::BindGroup,
    /// Dans la passe du monde (sans anticrénelage) ; après l'anticrénelage, sur l'image
    /// finale (voir `bolts::SceneDepth`).
    plain: wgpu::RenderPipeline,
    post: wgpu::RenderPipeline,
}

impl Decals {
    pub fn new(
        device: &wgpu::Device,
        world_bgl: &wgpu::BindGroupLayout,
        depth_bgl: &wgpu::BindGroupLayout,
        surface: wgpu::TextureFormat,
    ) -> Self {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("traces"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let clock = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("traces (horloge)"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("traces"),
            layout: &layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: clock.as_entire_binding(),
            }],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("traces"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/decal.wgsl").into()),
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("traces"),
            bind_group_layouts: &[world_bgl, &layout],
            push_constant_ranges: &[],
        });
        let pl_post = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("traces (après l'anticrénelage)"),
            bind_group_layouts: &[world_bgl, &layout, depth_bgl],
            push_constant_ranges: &[],
        });
        // Une trace multiplie l'image : image × (rgb + 1 − alpha). Le shader écrit le
        // facteur voulu sous cette forme (decal.wgsl, `trace`) : de 0 (noir) à un peu plus
        // de 1 (la matière plus claire mise à nu autour d'un impact).
        let multiply = wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::Dst,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::Zero,
                dst_factor: wgpu::BlendFactor::One,
                operation: wgpu::BlendOperation::Add,
            },
        };
        let attrs = wgpu::vertex_attr_array![
            0 => Float32x4, 1 => Float32x4, 2 => Float32x4, 3 => Float32x4
        ];
        // `post` : après l'anticrénelage, les faces arrière de la boîte (dessinées même
        // l'œil dedans), et le shader lit la profondeur du monde ; sinon le rectangle posé
        // sur la surface, caché par ce qui est devant.
        let pipeline = |post: bool| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("traces"),
                layout: Some(if post { &pl_post } else { &pl }),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some(if post { "vs_post" } else { "vs_main" }),
                    compilation_options: Default::default(),
                    buffers: &[wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<Decal>() as u64,
                        step_mode: wgpu::VertexStepMode::Instance,
                        attributes: &attrs,
                    }],
                },
                primitive: wgpu::PrimitiveState {
                    cull_mode: post.then_some(wgpu::Face::Front),
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
                        blend: Some(multiply),
                        write_mask: wgpu::ColorWrites::COLOR,
                    })],
                }),
                multiview: None,
                cache: None,
            })
        };
        Self {
            buffer: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("traces"),
                size: (MAX_DECALS * std::mem::size_of::<Decal>()) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            count: 0,
            rev: None,
            clock,
            group,
            plain: pipeline(false),
            post: pipeline(true),
        }
    }

    /// Traces de l'image. `rev` : révision de la liste (la carte ne reçoit rien tant
    /// qu'elle ne change pas). Ses 32 bits du haut changent quand des traces ont été
    /// retirées ; tant qu'ils ne changent pas, la liste n'a fait que s'allonger, et seules
    /// les nouvelles traces sont envoyées (pendant une fusillade, il en arrive à chaque
    /// image). `now` : l'horloge de leurs heures de naissance.
    pub fn prepare(&mut self, queue: &wgpu::Queue, decals: &[Decal], rev: u64, now: f32) {
        let n = decals.len().min(MAX_DECALS);
        if self.rev != Some(rev) {
            let grown = self.rev.is_some_and(|r| r >> 32 == rev >> 32) && n >= self.count as usize;
            let from = if grown { self.count as usize } else { 0 };
            if n > from {
                queue.write_buffer(
                    &self.buffer,
                    (from * std::mem::size_of::<Decal>()) as u64,
                    bytemuck::cast_slice(&decals[from..n]),
                );
            }
            self.rev = Some(rev);
            self.count = n as u32;
        }
        if self.count > 0 {
            queue.write_buffer(&self.clock, 0, bytemuck::cast_slice(&[now, 0.0, 0.0, 0.0]));
        }
    }

    /// Y a-t-il des traces à dessiner ?
    pub fn any(&self) -> bool {
        self.count > 0
    }

    /// Traces, dans la passe du monde (sans anticrénelage) ; ou, avec `depth` (la
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
        pass.draw(0..if depth.is_some() { 36 } else { 6 }, 0..self.count);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Une trace tient en quatre vecteurs de quatre nombres, dans l'ordre que lit le shader.
    #[test]
    fn a_decal_is_four_vectors() {
        assert_eq!(std::mem::size_of::<Decal>(), 64);
        let d = Decal {
            pos: [1.0, 2.0, 3.0],
            kind: KIND_HOLE,
            normal: [0.0, 1.0, 0.0],
            seed: 5.0,
            tangent: [1.0, 0.0, 0.0],
            born: 6.0,
            size: [7.0, 8.0],
            depth: 9.0,
            force: 1.0,
        };
        let f: &[f32] = bytemuck::cast_slice(std::slice::from_ref(&d));
        assert_eq!(f[3], KIND_HOLE);
        assert_eq!(f[7], 5.0);
        assert_eq!(f[11], 6.0);
        assert_eq!(&f[12..16], &[7.0, 8.0, 9.0, 1.0]);
    }
}
