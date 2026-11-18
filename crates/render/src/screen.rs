//! Écrans vidéo dans le monde : des quads fixés sur un mur (ou dressés sur un sol) là où
//! le joueur les a posés, texturés par les images que le jeu leur donne (`set_frame`) ;
//! tous montrent la même image.
//!
//! Le contenu (mire de test aujourd'hui, navigateur CEF qui lit YouTube demain) vit côté
//! jeu ; ici on ne fait que l'afficher. Dessiné dans la passe du monde, après les entités,
//! avec la profondeur (les blocs le cachent) et ses vecteurs de mouvement (pas de traînée
//! de l'anticrénelage).

use bytemuck::{Pod, Zeroable};
use glam::Vec3;

use crate::taa::{COLOR_FORMAT, MOTION_FORMAT};
use crate::DEPTH_FORMAT;

/// Où est posé l'écran : centre, axes unitaires dans le plan de l'écran et taille en
/// blocs. Vu de devant, `right` va vers la droite du spectateur et `up` vers le haut.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct ScreenAnchor {
    pub center: [f32; 3],
    pub right: [f32; 3],
    pub up: [f32; 3],
    /// Largeur et hauteur, en blocs.
    pub size: [f32; 2],
}

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
struct ScreenVertex {
    pos: [f32; 3],
    uv: [f32; 2],
}

/// Au plus tant d'écrans dessinés.
const MAX_SCREENS: usize = 64;

pub struct Screen {
    /// Rendu direct (une cible) et rendu avec anticrénelage temporel (couleur + mouvement).
    plain: wgpu::RenderPipeline,
    taa: wgpu::RenderPipeline,
    vbuf: wgpu::Buffer,
    bgl: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    /// Texture de l'image et son groupe de liaison (recréés si la taille change).
    texture: Option<(wgpu::Texture, wgpu::BindGroup)>,
    /// Version de la dernière image envoyée.
    frame_version: Option<u64>,
    /// Écrans posés.
    anchors: Vec<ScreenAnchor>,
    /// Écrans dont les sommets ont été envoyés (`None` : rien d'envoyé).
    uploaded: Option<Vec<ScreenAnchor>>,
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
        let taa = pipeline(
            "écran (TAA)",
            "fs_taa_world",
            &[target(COLOR_FORMAT), target(MOTION_FORMAT)],
        );

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("écran"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let vbuf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("écran (sommets)"),
            size: (MAX_SCREENS * 6 * std::mem::size_of::<ScreenVertex>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            plain,
            taa,
            vbuf,
            bgl,
            sampler,
            texture: None,
            frame_version: None,
            anchors: Vec::new(),
            uploaded: None,
        }
    }

    /// Écrans posés (les suivants au-delà de `MAX_SCREENS` sont ignorés).
    pub fn set_anchors(&mut self, anchors: &[ScreenAnchor]) {
        if self.anchors != anchors {
            self.anchors.clear();
            self.anchors
                .extend_from_slice(&anchors[..anchors.len().min(MAX_SCREENS)]);
        }
    }

    /// Nouvelle image (BGRA 8 bits, `width * height * 4` octets), envoyée seulement si sa
    /// `version` a changé.
    pub fn set_frame(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        version: u64,
        width: u32,
        height: u32,
        pixels: &[u8],
    ) {
        if self.frame_version == Some(version)
            || width == 0
            || height == 0
            || pixels.len() < (width * height * 4) as usize
        {
            return;
        }
        let size = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        if self.texture.as_ref().is_none_or(|(t, _)| t.size() != size) {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("écran (vidéo)"),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Bgra8UnormSrgb, // CEF sort du BGRA
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let view = texture.create_view(&Default::default());
            let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("écran (vidéo)"),
                layout: &self.bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                ],
            });
            self.texture = Some((texture, group));
        }
        let (texture, _) = self.texture.as_ref().expect("texture créée juste avant");
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &pixels[..(width * height * 4) as usize],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4 * width),
                rows_per_image: Some(height),
            },
            size,
        );
        self.frame_version = Some(version);
    }

    /// Image suivante : renvoie les sommets si des écrans ont été posés ou retirés.
    pub fn update(&mut self, queue: &wgpu::Queue) {
        if self.uploaded.as_deref() == Some(&self.anchors[..]) {
            return;
        }
        let mut verts = Vec::with_capacity(self.anchors.len() * 6);
        for a in &self.anchors {
            let c = Vec3::from(a.center);
            let r = Vec3::from(a.right) * (a.size[0] * 0.5);
            let u = Vec3::from(a.up) * (a.size[1] * 0.5);
            let tl = c - r + u;
            let tr = c + r + u;
            let bl = c - r - u;
            let br = c + r - u;
            let v = |p: Vec3, uv: [f32; 2]| ScreenVertex {
                pos: [p.x, p.y, p.z],
                uv,
            };
            // uv (0,0) en haut-gauche.
            verts.extend([
                v(tl, [0.0, 0.0]),
                v(bl, [0.0, 1.0]),
                v(br, [1.0, 1.0]),
                v(tl, [0.0, 0.0]),
                v(br, [1.0, 1.0]),
                v(tr, [1.0, 0.0]),
            ]);
        }
        if !verts.is_empty() {
            queue.write_buffer(&self.vbuf, 0, bytemuck::cast_slice(&verts));
        }
        self.uploaded = Some(self.anchors.clone());
    }

    /// Dessine les écrans dans la passe du monde (le group 0 est déjà lié).
    pub fn draw<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>, taa: bool) {
        let Some((_, group)) = &self.texture else {
            return;
        };
        let n = self.anchors.len() as u32;
        if n == 0 || self.uploaded.as_deref() != Some(&self.anchors[..]) {
            return;
        }
        pass.set_pipeline(if taa { &self.taa } else { &self.plain });
        pass.set_bind_group(1, group, &[]);
        pass.set_vertex_buffer(0, self.vbuf.slice(..));
        pass.draw(0..6 * n, 0..1);
    }
}
