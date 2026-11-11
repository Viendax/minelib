//! Fumigènes en volume : chaque nuage est une grille 3D (4 cases par bloc) envoyée une fois
//! à la carte graphique ; un shader avance le long du regard dans la boîte du nuage, en
//! tire la densité (front qui gonfle puis se retire, volutes de bruit, tunnels des balles)
//! et compose une fumée opaque, éclairée par le ciel, arrêtée par les murs de la grille.
//! Seuls les pixels couverts par la boîte travaillent, et la marche s'arrête dès que la
//! fumée est opaque : le coût reste faible même dedans.

use bytemuck::{Pod, Zeroable};
use glam::Vec3;

use crate::taa::{COLOR_FORMAT, MOTION_FORMAT};
use crate::DEPTH_FORMAT;

/// Au plus autant de nuages dessinés à la fois.
pub const MAX_SMOKES: usize = 6;
/// Au plus autant de tunnels de balle par nuage.
pub const MAX_SMOKE_TUNNELS: usize = 16;

/// Nuage de fumée fourni par le jeu pour une image.
pub struct SmokeVolume<'a> {
    /// Identifiant : la grille n'est envoyée qu'une fois par identifiant.
    pub id: u64,
    /// Coin bas de la grille (monde), taille d'une case et nombre de cases.
    pub origin: Vec3,
    pub cell: f32,
    pub dims: [u32; 3],
    /// Par case : moment où la fumée l'atteint (0 centre … 204 bord, ×0,8 ; 255 jamais) et mur
    /// (255 = bloc plein).
    pub cells: &'a [[u8; 2]],
    /// Front de la fumée (voir `equip::smoke_grow` côté jeu) et âge (s).
    pub grow: f32,
    pub age: f32,
    /// Tunnels des balles : extrémités et rayon actuel.
    pub tunnels: Vec<(Vec3, Vec3, f32)>,
    /// Boîte (monde) des cases que la fumée peut atteindre : seule partie dessinée.
    pub bounds: (Vec3, Vec3),
}

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
struct SmokeUniform {
    /// xyz = coin bas, w = taille d'une case.
    origin: [f32; 4],
    /// xyz = taille de la boîte (monde), w = front de la fumée.
    size: [f32; 4],
    /// x = nombre de tunnels, y = âge, z = nombre de pas max, w = longueur d'un pas.
    params: [f32; 4],
    tunnel_a: [[f32; 4]; MAX_SMOKE_TUNNELS],
    tunnel_b: [[f32; 4]; MAX_SMOKE_TUNNELS],
    box_min: [f32; 4],
    box_max: [f32; 4],
}

struct Slot {
    id: u64,
    _fill: wgpu::Texture,
    uniform: wgpu::Buffer,
    group: wgpu::BindGroup,
    used: bool,
    inside: bool,
}

/// Réglages de qualité (essais de performances).
#[derive(Clone, Copy, Debug)]
pub struct SmokeQuality {
    pub max_steps: u32,
    pub step: f32,
}

impl Default for SmokeQuality {
    fn default() -> Self {
        Self {
            max_steps: 80,
            step: 0.1,
        }
    }
}

pub struct Smokes {
    layout: wgpu::BindGroupLayout,
    /// [dehors, dedans] × [direct, TAA].
    pipelines: [[wgpu::RenderPipeline; 2]; 2],
    noise_view: wgpu::TextureView,
    _noise: wgpu::Texture,
    fill_sampler: wgpu::Sampler,
    noise_sampler: wgpu::Sampler,
    slots: Vec<Slot>,
    pub quality: SmokeQuality,
}

impl Smokes {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        world_bgl: &wgpu::BindGroupLayout,
        surface: wgpu::TextureFormat,
    ) -> Self {
        let tex3d = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D3,
                multisampled: false,
            },
            count: None,
        };
        let sampler = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("fumée"),
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
                tex3d(1),
                tex3d(2),
                sampler(3),
                sampler(4),
            ],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("fumée"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/smoke.wgsl").into()),
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("fumée"),
            bind_group_layouts: &[world_bgl, &layout],
            push_constant_ranges: &[],
        });
        let blend = wgpu::BlendState {
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
        let pipeline = |inside: bool, taa: bool| {
            let color = wgpu::ColorTargetState {
                format: if taa { COLOR_FORMAT } else { surface },
                blend: Some(blend),
                write_mask: wgpu::ColorWrites::ALL,
            };
            let motion = wgpu::ColorTargetState {
                format: MOTION_FORMAT,
                blend: None,
                write_mask: wgpu::ColorWrites::empty(),
            };
            let targets: Vec<Option<wgpu::ColorTargetState>> = if taa {
                vec![Some(color), Some(motion)]
            } else {
                vec![Some(color)]
            };
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("fumée"),
                layout: Some(&pl),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                primitive: wgpu::PrimitiveState {
                    // Dehors : faces avant de la boîte (cachées par les murs devant elle) ;
                    // dedans : faces arrière, toujours dessinées.
                    cull_mode: Some(if inside {
                        wgpu::Face::Front
                    } else {
                        wgpu::Face::Back
                    }),
                    ..Default::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: false,
                    depth_compare: if inside {
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
                    entry_point: Some(if taa { "fs_taa" } else { "fs_main" }),
                    compilation_options: Default::default(),
                    targets: &targets,
                }),
                multiview: None,
                cache: None,
            })
        };
        let pipelines = [
            [pipeline(false, false), pipeline(false, true)],
            [pipeline(true, false), pipeline(true, true)],
        ];
        // Bruit de valeur 32³ qui se répète, pour les volutes.
        const N: u32 = 32;
        let noise_data = value_noise(N as usize);
        let noise = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("fumée : bruit"),
            size: wgpu::Extent3d {
                width: N,
                height: N,
                depth_or_array_layers: N,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D3,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            noise.as_image_copy(),
            &noise_data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(N),
                rows_per_image: Some(N),
            },
            wgpu::Extent3d {
                width: N,
                height: N,
                depth_or_array_layers: N,
            },
        );
        let linear = |mode| {
            device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("fumée"),
                address_mode_u: mode,
                address_mode_v: mode,
                address_mode_w: mode,
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                ..Default::default()
            })
        };
        Self {
            layout,
            pipelines,
            noise_view: noise.create_view(&Default::default()),
            _noise: noise,
            fill_sampler: linear(wgpu::AddressMode::ClampToEdge),
            noise_sampler: linear(wgpu::AddressMode::Repeat),
            slots: Vec::new(),
            quality: SmokeQuality::default(),
        }
    }

    /// Nuages de cette image : grilles envoyées pour les nouveaux, uniformes mis à jour,
    /// emplacements des nuages disparus libérés.
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        smokes: &[SmokeVolume],
        eye: Vec3,
    ) {
        for s in &mut self.slots {
            s.used = false;
        }
        for v in smokes.iter().take(MAX_SMOKES) {
            let i = match self.slots.iter().position(|s| s.id == v.id) {
                Some(i) => i,
                None => {
                    self.slots.push(self.create_slot(device, queue, v));
                    self.slots.len() - 1
                }
            };
            let size = Vec3::new(v.dims[0] as f32, v.dims[1] as f32, v.dims[2] as f32) * v.cell;
            let mut u = SmokeUniform {
                origin: [v.origin.x, v.origin.y, v.origin.z, v.cell],
                size: [size.x, size.y, size.z, v.grow],
                params: [
                    v.tunnels.len().min(MAX_SMOKE_TUNNELS) as f32,
                    v.age,
                    self.quality.max_steps as f32,
                    self.quality.step,
                ],
                tunnel_a: [[0.0; 4]; MAX_SMOKE_TUNNELS],
                tunnel_b: [[0.0; 4]; MAX_SMOKE_TUNNELS],
                box_min: v.bounds.0.extend(0.0).to_array(),
                box_max: v.bounds.1.extend(0.0).to_array(),
            };
            for (k, (a, b, r)) in v.tunnels.iter().take(MAX_SMOKE_TUNNELS).enumerate() {
                u.tunnel_a[k] = [a.x, a.y, a.z, *r];
                u.tunnel_b[k] = [b.x, b.y, b.z, 0.0];
            }
            let s = &mut self.slots[i];
            queue.write_buffer(&s.uniform, 0, bytemuck::bytes_of(&u));
            s.used = true;
            // Œil dans la boîte (avec une marge pour le plan proche de la caméra).
            let m = 0.2;
            s.inside = eye.cmpgt(v.bounds.0 - Vec3::splat(m)).all()
                && eye.cmplt(v.bounds.1 + Vec3::splat(m)).all();
        }
        self.slots.retain(|s| s.used);
    }

    fn create_slot(&self, device: &wgpu::Device, queue: &wgpu::Queue, v: &SmokeVolume) -> Slot {
        let extent = wgpu::Extent3d {
            width: v.dims[0],
            height: v.dims[1],
            depth_or_array_layers: v.dims[2],
        };
        let fill = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("fumée : grille"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D3,
            format: wgpu::TextureFormat::Rg8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            fill.as_image_copy(),
            bytemuck::cast_slice(v.cells),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(v.dims[0] * 2),
                rows_per_image: Some(v.dims[1]),
            },
            extent,
        );
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("fumée"),
            size: std::mem::size_of::<SmokeUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let fill_view = fill.create_view(&Default::default());
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("fumée"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&fill_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&self.noise_view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(&self.fill_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::Sampler(&self.noise_sampler),
                },
            ],
        });
        Slot {
            id: v.id,
            _fill: fill,
            uniform,
            group,
            used: true,
            inside: false,
        }
    }

    /// Dessine les nuages dans la passe du monde (après les blocs translucides).
    pub fn draw<'a>(
        &'a self,
        pass: &mut wgpu::RenderPass<'a>,
        world: &'a wgpu::BindGroup,
        taa: bool,
    ) {
        for s in &self.slots {
            pass.set_pipeline(&self.pipelines[s.inside as usize][taa as usize]);
            pass.set_bind_group(0, world, &[]);
            pass.set_bind_group(1, &s.group, &[]);
            pass.draw(0..36, 0..1);
        }
    }
}

/// Bruit de valeur lissé, répété tous les `n` (3 octaves mêlées), 0..255.
fn value_noise(n: usize) -> Vec<u8> {
    let hash = |x: usize, y: usize, z: usize, p: usize| {
        let (x, y, z) = (x % p, y % p, z % p);
        let mut h = (x as u32).wrapping_mul(73856093)
            ^ (y as u32).wrapping_mul(19349663)
            ^ (z as u32).wrapping_mul(83492791)
            ^ (p as u32).wrapping_mul(2654435761);
        h ^= h >> 13;
        h = h.wrapping_mul(0x5bd1e995);
        h ^= h >> 15;
        (h & 0xffff) as f32 / 65535.0
    };
    let smooth = |t: f32| t * t * (3.0 - 2.0 * t);
    let octave = |x: usize, y: usize, z: usize, cells: usize| {
        let s = n / cells;
        let f = |v: usize| (v % s) as f32 / s as f32;
        let (cx, cy, cz) = (x / s, y / s, z / s);
        let (fx, fy, fz) = (smooth(f(x)), smooth(f(y)), smooth(f(z)));
        let mut acc = 0.0;
        for k in 0..8 {
            let (dx, dy, dz) = (k & 1, (k >> 1) & 1, (k >> 2) & 1);
            let w = (if dx == 1 { fx } else { 1.0 - fx })
                * (if dy == 1 { fy } else { 1.0 - fy })
                * (if dz == 1 { fz } else { 1.0 - fz });
            acc += w * hash(cx + dx, cy + dy, cz + dz, cells);
        }
        acc
    };
    let mut out = Vec::with_capacity(n * n * n);
    for z in 0..n {
        for y in 0..n {
            for x in 0..n {
                let v = octave(x, y, z, 4) * 0.55
                    + octave(x, y, z, 8) * 0.3
                    + octave(x, y, z, 16) * 0.15;
                out.push((v.clamp(0.0, 1.0) * 255.0) as u8);
            }
        }
    }
    out
}
