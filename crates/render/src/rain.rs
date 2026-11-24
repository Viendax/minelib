//! (v216) Pluie : des gouttes filées par leur vitesse et leurs éclaboussures au sol, toutes
//! calculées par la carte graphique (rain.wgsl) : rien n'est simulé ni envoyé goutte par
//! goutte. Le jeu fournit la force de la pluie, le vent, et la carte des toits (le dessus
//! du plus haut bloc de chaque colonne autour du joueur) : il ne pleut pas sous un abri.
//! Sans pluie, rien n'est dessiné ni calculé.

use bytemuck::{Pod, Zeroable};
use glam::Vec3;

use crate::DEPTH_FORMAT;

/// Côté de la carte des toits (colonnes de blocs).
pub const TOPS_N: usize = 64;
/// Boîte des gouttes autour de la caméra (blocs) : la même que dans rain.wgsl.
const BOX: Vec3 = Vec3::new(40.0, 32.0, 40.0);
/// Gouttes et éclaboussures à pleine force.
const DROPS: f32 = 11_000.0;
const SPLASHES: f32 = 1_600.0;
/// Vitesse de chute (blocs/s) des quatre groupes de gouttes.
const SPEEDS: [f32; 4] = [17.0, 19.5, 21.0, 23.5];

/// Pluie de l'image.
#[derive(Clone, Copy, Debug, Default)]
pub struct Rain<'a> {
    /// Force (0 : pas de pluie … 1 : averse).
    pub amount: f32,
    /// Vent (blocs/s, x et z) : il couche la pluie.
    pub wind: [f32; 2],
    /// Carte des toits : coin (x, z) en blocs, `TOPS_N`² hauteurs (dessus du plus haut bloc
    /// de la colonne ; très négatif : rien), et son numéro (renvoyée à la carte graphique
    /// quand il change).
    pub tops: Option<(&'a [f32], [i32; 2], u64)>,
    /// Lumière du jour (0 … 1) et couleur du ciel couvert : la pluie en prend la teinte.
    pub daylight: f32,
}

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
struct RainUniform {
    origin: [f32; 4],
    fall: [[f32; 4]; 4],
    dir: [f32; 4],
    color: [f32; 4],
}

/// (v251) Le shader de la pluie, avec celui des lumières posées à la suite (elles éclairent les
/// gouttes et les éclaboussures).
pub(crate) fn source() -> String {
    crate::lights::with_dynlights(include_str!("shaders/rain.wgsl"))
}

pub struct RainRenderer {
    uniform: wgpu::Buffer,
    tops: wgpu::Texture,
    group: wgpu::BindGroup,
    /// Gouttes, éclaboussures : dans la passe du monde (sans anticrénelage), ou après
    /// l'anticrénelage, sur l'image finale (voir `bolts::SceneDepth`).
    drops: [wgpu::RenderPipeline; 2],
    splashes: [wgpu::RenderPipeline; 2],
    fall: [Vec3; 4],
    time: f32,
    last: Option<std::time::Instant>,
    uploaded: Option<u64>,
    origin: [i32; 2],
    counts: (u32, u32),
}

impl RainRenderer {
    pub fn new(
        device: &wgpu::Device,
        world_bgl: &wgpu::BindGroupLayout,
        depth_bgl: &wgpu::BindGroupLayout,
        surface: wgpu::TextureFormat,
    ) -> Self {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("pluie"),
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
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("pluie"),
            size: std::mem::size_of::<RainUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let tops = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("pluie : toits"),
            size: wgpu::Extent3d {
                width: TOPS_N as u32,
                height: TOPS_N as u32,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = tops.create_view(&Default::default());
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("pluie"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
            ],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("pluie"),
            source: wgpu::ShaderSource::Wgsl(source().into()),
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("pluie"),
            bind_group_layouts: &[world_bgl, &layout],
            push_constant_ranges: &[],
        });
        let pl_post = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("pluie (après l'anticrénelage)"),
            bind_group_layouts: &[world_bgl, &layout, depth_bgl],
            push_constant_ranges: &[],
        });
        // Une goutte renvoie la lumière du ciel vers l'œil : elle éclaircit ce qui est
        // derrière elle, jamais elle ne l'assombrit (la nuit, on ne la voit presque pas).
        let blend = wgpu::BlendState {
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
        // `post` : après l'anticrénelage (le shader lit la profondeur du monde).
        let pipeline = |vs: &str, fs: &str, post: bool| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("pluie"),
                layout: Some(if post { &pl_post } else { &pl }),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some(vs),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                primitive: wgpu::PrimitiveState {
                    cull_mode: None,
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
                    entry_point: Some(fs),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: surface,
                        blend: Some(blend),
                        write_mask: wgpu::ColorWrites::COLOR,
                    })],
                }),
                multiview: None,
                cache: None,
            })
        };
        Self {
            uniform,
            tops,
            group,
            drops: [
                pipeline("vs_drop", "fs_drop", false),
                pipeline("vs_drop_post", "fs_drop_post", true),
            ],
            splashes: [
                pipeline("vs_splash", "fs_splash", false),
                pipeline("vs_splash_post", "fs_splash_post", true),
            ],
            fall: [Vec3::ZERO; 4],
            time: 0.0,
            last: None,
            uploaded: None,
            origin: [0, 0],
            counts: (0, 0),
        }
    }

    /// Prépare la pluie de l'image. `fog` : couleur du ciel couvert (les gouttes la
    /// renvoient).
    pub fn prepare(&mut self, queue: &wgpu::Queue, rain: &Rain, fog: [f32; 3]) {
        let now = std::time::Instant::now();
        let dt = self
            .last
            .replace(now)
            .map_or(0.0, |t| (now - t).as_secs_f32().min(0.1));
        let amount = rain.amount.clamp(0.0, 1.0);
        if amount <= 0.002 {
            self.counts = (0, 0);
            return;
        }
        // Le vent couche la pluie (jamais à plus de 35° : au-delà on ne verrait plus
        // tomber les gouttes).
        let wind = Vec3::new(rain.wind[0], 0.0, rain.wind[1]) * 0.9;
        let wind = wind.clamp_length_max(SPEEDS[0] * 0.7);
        for (f, speed) in self.fall.iter_mut().zip(SPEEDS) {
            let v = Vec3::new(wind.x, -speed, wind.z);
            let n = *f + v * dt;
            *f = Vec3::new(
                n.x.rem_euclid(BOX.x),
                n.y.rem_euclid(BOX.y),
                n.z.rem_euclid(BOX.z),
            );
        }
        self.time = (self.time + dt) % 4096.0;
        if let Some((tops, origin, version)) = rain.tops {
            if self.uploaded != Some(version) && tops.len() == TOPS_N * TOPS_N {
                queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &self.tops,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    bytemuck::cast_slice(tops),
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some((TOPS_N * 4) as u32),
                        rows_per_image: Some(TOPS_N as u32),
                    },
                    wgpu::Extent3d {
                        width: TOPS_N as u32,
                        height: TOPS_N as u32,
                        depth_or_array_layers: 1,
                    },
                );
                self.uploaded = Some(version);
                self.origin = origin;
            }
        }
        if self.uploaded.is_none() {
            self.counts = (0, 0);
            return;
        }
        let dir = Vec3::new(wind.x, -SPEEDS[1], wind.z).normalize();
        // Les gouttes renvoient la lumière du ciel couvert.
        let day = 0.04 + 0.96 * rain.daylight.clamp(0.0, 1.0);
        let c = (Vec3::from(fog) * 0.9 + Vec3::splat(0.08 * day)).min(Vec3::splat(1.0));
        let u = RainUniform {
            origin: [
                self.origin[0] as f32,
                self.origin[1] as f32,
                TOPS_N as f32,
                self.time,
            ],
            fall: self.fall.map(|f| [f.x, f.y, f.z, 0.0]),
            dir: [dir.x, dir.y, dir.z, amount],
            color: [c.x, c.y, c.z, 0.3 + 0.25 * amount],
        };
        queue.write_buffer(&self.uniform, 0, bytemuck::bytes_of(&u));
        // Moins de gouttes sous une pluie faible (les premières : toujours les mêmes).
        self.counts = ((DROPS * amount) as u32, (SPLASHES * amount) as u32);
    }

    /// Pluie, dans la passe du monde (sans anticrénelage) ; ou, avec `depth` (la
    /// profondeur du monde, `bolts::SceneDepth`), après l'anticrénelage sur l'image finale.
    pub fn draw<'a>(
        &'a self,
        pass: &mut wgpu::RenderPass<'a>,
        world: &'a wgpu::BindGroup,
        depth: Option<&'a wgpu::BindGroup>,
    ) {
        let (drops, splashes) = self.counts;
        if drops == 0 {
            return;
        }
        let k = usize::from(depth.is_some());
        pass.set_bind_group(0, world, &[]);
        pass.set_bind_group(1, &self.group, &[]);
        if let Some(d) = depth {
            pass.set_bind_group(2, d, &[]);
        }
        if splashes > 0 {
            pass.set_pipeline(&self.splashes[k]);
            pass.draw(0..6, 0..splashes);
        }
        pass.set_pipeline(&self.drops[k]);
        pass.draw(0..6, 0..drops);
    }

    /// Pleut-il (quelque chose à dessiner) ?
    pub fn any(&self) -> bool {
        self.counts.0 > 0
    }
}
