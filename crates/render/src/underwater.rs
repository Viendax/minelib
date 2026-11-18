//! (v194) Sous l'eau : ce que la caméra voit sous la surface de l'eau est bleuté et s'efface
//! dans un brouillard bleu avec la distance (voir `shaders/underwater.wgsl`). Seulement sous
//! la ligne de l'eau, main et bras compris ; l'interface reste nette.

use glam::{Mat4, Vec3};

/// Couleur de la brume sous l'eau : la teinte de l'eau (`WATER_TINT` des textures,
/// 63, 118, 228 en sRGB), en linéaire.
const WATER: [f32; 3] = [0.0497, 0.1812, 0.7758];
/// Densité de la brume (par bloc) : à moitié vers 9 blocs, presque tout vers 35.
const HAZE: f32 = 0.075;
/// Absorption par bloc : le rouge s'éteint d'abord, puis le vert ; le bleu traverse.
const ABSORB: [f32; 3] = [0.16, 0.07, 0.03];
/// Filtre au plus près : bleuté même tout contre l'œil.
const NEAREST: [f32; 3] = [0.62, 0.82, 1.0];
/// Distance de la main et du bras (blocs).
const HAND: f32 = 0.45;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct WaterUniform {
    view_proj: [[f32; 4]; 4],
    cam: [f32; 4],
    color: [f32; 4],
    absorb: [f32; 4],
    near: [f32; 4],
    misc: [f32; 4],
}

/// Brume de l'eau à la lumière du jour `daylight` (0 nuit … 1 midi) : sombre la nuit.
pub fn fog_color(daylight: f32) -> [f32; 3] {
    let k = 0.12 + 0.45 * daylight.clamp(0.0, 1.0);
    WATER.map(|c| c * k)
}

pub struct Underwater {
    /// Monde puis main et bras : filtre (multiplié), puis brume (ajoutée).
    pipes: [wgpu::RenderPipeline; 4],
    bgl: wgpu::BindGroupLayout,
    buf: wgpu::Buffer,
    /// Groupe de liaison de cette image (profondeur du monde de l'image), s'il y a de l'eau.
    group: Option<wgpu::BindGroup>,
}

impl Underwater {
    pub fn new(device: &wgpu::Device, surface: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("sous l'eau"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/underwater.wgsl").into()),
        });
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("sous l'eau"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
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
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("sous l'eau"),
            bind_group_layouts: &[&bgl],
            push_constant_ranges: &[],
        });
        // Profondeur de l'interface (Z inversé, effacée à 0) : 0 là où rien n'est devant le
        // monde, plus là où la main et le bras sont dessinés. Le triangle plein écran est à 0.
        let multiply = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::Zero,
            dst_factor: wgpu::BlendFactor::Src,
            operation: wgpu::BlendOperation::Add,
        };
        let add = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::One,
            operation: wgpu::BlendOperation::Add,
        };
        let pipe = |fs: &str, compare: wgpu::CompareFunction, color: wgpu::BlendComponent| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("sous l'eau"),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_full"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                primitive: Default::default(),
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: crate::DEPTH_FORMAT,
                    depth_write_enabled: false,
                    depth_compare: compare,
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: Default::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(fs),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: surface,
                        blend: Some(wgpu::BlendState {
                            color,
                            alpha: wgpu::BlendComponent::OVER,
                        }),
                        write_mask: wgpu::ColorWrites::COLOR,
                    })],
                }),
                multiview: None,
                cache: None,
            })
        };
        use wgpu::CompareFunction::{GreaterEqual, Less};
        let pipes = [
            pipe("fs_world_filter", GreaterEqual, multiply),
            pipe("fs_world_haze", GreaterEqual, add),
            pipe("fs_hand_filter", Less, multiply),
            pipe("fs_hand_haze", Less, add),
        ];
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("sous l'eau"),
            size: std::mem::size_of::<WaterUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            pipes,
            bgl,
            buf,
            group: None,
        }
    }

    /// Prépare l'image : `level` = surface de l'eau au-dessus de la caméra (`None` hors de
    /// l'eau), `depth` = profondeur du monde de cette image, `size` = taille de l'écran.
    #[allow(clippy::too_many_arguments)]
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view_proj: Mat4,
        eye: Vec3,
        level: Option<f32>,
        daylight: f32,
        depth: &wgpu::TextureView,
        size: (u32, u32),
    ) {
        let Some(level) = level else {
            self.group = None;
            return;
        };
        let c = fog_color(daylight);
        let u = WaterUniform {
            view_proj: view_proj.to_cols_array_2d(),
            cam: [eye.x, eye.y, eye.z, level],
            color: [c[0], c[1], c[2], HAZE],
            absorb: [ABSORB[0], ABSORB[1], ABSORB[2], HAND],
            near: [NEAREST[0], NEAREST[1], NEAREST[2], 0.0],
            misc: [0.0, 0.0, size.0 as f32, size.1 as f32],
        };
        queue.write_buffer(&self.buf, 0, bytemuck::bytes_of(&u));
        self.group = Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("sous l'eau"),
            layout: &self.bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(depth),
                },
            ],
        }));
    }

    /// Après la main et le bras, avant l'interface : le monde, puis la main et le bras.
    pub fn draw<'p>(&'p self, pass: &mut wgpu::RenderPass<'p>) {
        let Some(group) = &self.group else { return };
        pass.set_bind_group(0, group, &[]);
        for p in &self.pipes {
            pass.set_pipeline(p);
            pass.draw(0..3, 0..1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_water_fog_is_blue_and_darker_at_night() {
        let day = fog_color(1.0);
        let night = fog_color(0.0);
        assert!(day[2] > day[1] && day[1] > day[0], "{day:?}");
        assert!(
            night.iter().zip(day).all(|(n, d)| *n < d * 0.3),
            "{night:?}"
        );
    }
}
