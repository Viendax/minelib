//! Écran vidéo dans le monde : un quad texturé par une image dynamique.
//!
//! Pour l'instant l'image est une mire de test générée sur le CPU, pour qu'un
//! écran soit visible dès l'apparition. Plus tard la texture sera alimentée par
//! le navigateur offscreen (CEF) qui lit YouTube — seul `update` changera.
//!
//! L'écran est posé en billboard quelques blocs devant la caméra (il te fait
//! toujours face) ; on l'ancrera à une surface du monde ensuite. Il est dessiné
//! dans la passe du monde, après les entités, avec la profondeur : les blocs le
//! cachent comme n'importe quel objet.

use bytemuck::{Pod, Zeroable};
use glam::Vec3;

use crate::taa::{COLOR_FORMAT, MOTION_FORMAT};
use crate::DEPTH_FORMAT;

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
struct ScreenVertex {
    pos: [f32; 3],
    uv: [f32; 2],
}

/// Résolution de la texture de l'écran (16:9).
const TEX_W: u32 = 640;
const TEX_H: u32 = 360;

pub struct Screen {
    /// Rendu direct (une cible) et rendu avec anticrénelage temporel (couleur + mouvement).
    plain: wgpu::RenderPipeline,
    taa: wgpu::RenderPipeline,
    vbuf: wgpu::Buffer,
    texture: wgpu::Texture,
    group: wgpu::BindGroup,
    pixels: Vec<u8>,
    /// Largeur et hauteur du quad, en blocs.
    size: (f32, f32),
}

impl Screen {
    pub fn new(
        device: &wgpu::Device,
        world_bgl: &wgpu::BindGroupLayout,
        surface: wgpu::TextureFormat,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("écran"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/screen.wgsl").into()),
        });

        // group 1 : texture vidéo + échantillonneur (le group 0 = uniformes du monde).
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("écran (vidéo)"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
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

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("écran"),
            bind_group_layouts: &[world_bgl, &bgl],
            push_constant_ranges: &[],
        });

        const ATTRS: [wgpu::VertexAttribute; 2] =
            wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x2];

        let pipeline = |label: &str, entry: &str, targets: &[Option<wgpu::ColorTargetState>]| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    compilation_options: Default::default(),
                    buffers: &[wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<ScreenVertex>() as u64,
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
        let plain = pipeline("écran", "fs_main", &[target(surface)]);
        let taa = pipeline("écran (TAA)", "fs_taa", &[target(COLOR_FORMAT), target(MOTION_FORMAT)]);

        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("écran (vidéo)"),
            size: wgpu::Extent3d {
                width: TEX_W,
                height: TEX_H,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Bgra8UnormSrgb, // CEF sort du BGRA
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("écran"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("écran (vidéo)"),
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

        let vbuf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("écran (sommets)"),
            size: (6 * std::mem::size_of::<ScreenVertex>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            plain,
            taa,
            vbuf,
            texture,
            group,
            pixels: vec![0; (TEX_W * TEX_H * 4) as usize],
            size: (4.0, 2.25), // 16:9, 4 blocs de large
        }
    }

    /// Régénère la mire et replace l'écran en billboard devant la caméra.
    /// `eye` = position des yeux, `fwd`/`up` = axes de la caméra (unitaires).
    pub fn update(&mut self, queue: &wgpu::Queue, time: f32, eye: Vec3, fwd: Vec3, up: Vec3) {
        // 1) Mire animée → texture (BGRA).
        mire(&mut self.pixels, TEX_W, TEX_H, time);
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &self.pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4 * TEX_W),
                rows_per_image: Some(TEX_H),
            },
            wgpu::Extent3d {
                width: TEX_W,
                height: TEX_H,
                depth_or_array_layers: 1,
            },
        );

        // 2) Quad billboard : centre quelques blocs devant les yeux, face à la caméra.
        let dist = 5.0;
        let center = eye + fwd * dist;
        let right = fwd.cross(up).normalize_or_zero();
        let upn = right.cross(fwd).normalize_or_zero();
        let (hw, hh) = (self.size.0 * 0.5, self.size.1 * 0.5);
        let tl = center - right * hw + upn * hh;
        let tr = center + right * hw + upn * hh;
        let bl = center - right * hw - upn * hh;
        let br = center + right * hw - upn * hh;
        let v = |p: Vec3, uv: [f32; 2]| ScreenVertex {
            pos: [p.x, p.y, p.z],
            uv,
        };
        // uv (0,0) en haut-gauche.
        let verts = [
            v(tl, [0.0, 0.0]),
            v(bl, [0.0, 1.0]),
            v(br, [1.0, 1.0]),
            v(tl, [0.0, 0.0]),
            v(br, [1.0, 1.0]),
            v(tr, [1.0, 0.0]),
        ];
        queue.write_buffer(&self.vbuf, 0, bytemuck::cast_slice(&verts));
    }

    /// Dessine l'écran dans la passe du monde (le group 0 est déjà lié).
    pub fn draw<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>, taa: bool) {
        pass.set_pipeline(if taa { &self.taa } else { &self.plain });
        pass.set_bind_group(1, &self.group, &[]);
        pass.set_vertex_buffer(0, self.vbuf.slice(..));
        pass.draw(0..6, 0..1);
    }
}

/// Mire de test : dégradé animé + barre de défilement + triangle « lecture ».
fn mire(px: &mut [u8], w: u32, h: u32, time: f32) {
    let w = w as i32;
    let h = h as i32;
    let scroll = (time * 60.0) as i32;
    let bar = ((time * 0.2).fract() * w as f32) as i32;
    // Triangle de lecture au centre.
    let cx = w / 2;
    let cy = h / 2;
    let tri = (h / 6).max(8);
    for y in 0..h {
        for x in 0..w {
            let idx = ((y * w + x) * 4) as usize;
            let mut b = 30u8;
            let mut g = (((x + y + scroll) / 3) % 256) as u8 / 2 + 20;
            let mut r = 40u8;
            // Barre de défilement (haut).
            if y < 6 && x < bar {
                b = 230;
                g = 230;
                r = 230;
            }
            // Triangle « play » blanc.
            let dx = x - (cx - tri / 3);
            let dy = (y - cy).abs();
            if dx >= 0 && dx < tri && dy < (tri - dx) / 2 {
                b = 240;
                g = 240;
                r = 240;
            }
            px[idx] = b;
            px[idx + 1] = g;
            px[idx + 2] = r;
            px[idx + 3] = 255;
        }
    }
}
