//! Écrans vidéo dans le monde : des quads fixés sur un mur (ou dressés sur un sol) là où
//! le joueur les a posés, chacun texturé par ses propres images (`set_frame`).
//!
//! Le contenu (mire de test aujourd'hui, navigateur CEF qui lit YouTube demain) vit côté
//! jeu ; ici on ne fait que l'afficher. Dessiné dans la passe du monde, après les entités,
//! avec la profondeur (les blocs le cachent) et ses vecteurs de mouvement (pas de traînée
//! de l'anticrénelage).

use bytemuck::{Pod, Zeroable};
use glam::Vec3;

use crate::taa::{COLOR_FORMAT, MOTION_FORMAT, WATER_GB_FORMAT};
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
    /// (v206) Apparition des boutons, de 0 (invisible) à 1 : le jeu la monte quand le
    /// réticule glisse vers eux depuis le bord, et la redescend sinon (v212 : une seule pour
    /// tous les boutons, en premier). (v213) En second : les pages où aller, précédente (1)
    /// et suivante (2), pour griser les boutons retour et suivant.
    pub reveal: [f32; 2],
    /// (v243) Le curseur (le rond là où vise le réticule) : `x`, `y` (0..1 sur la page),
    /// `z` = 0 pas de curseur, 1 curseur, 2 curseur quand le clavier va dans la page. Dessiné
    /// par le shader **au-dessus** des boutons (avant, dans l'image de la page, dessous).
    pub cursor: [f32; 4],
    /// (v243) L'onde d'un appui : `x`, `y`, `z` = son âge de 0 à 1 (négatif : pas d'onde).
    pub ripple: [f32; 4],
}

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
struct ScreenVertex {
    pos: [f32; 3],
    uv: [f32; 2],
    reveal: [f32; 2],
    cursor: [f32; 4],
    ripple: [f32; 4],
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
    /// Image de chaque écran (par numéro) : texture, groupe de liaison et version.
    textures: std::collections::HashMap<u32, (wgpu::Texture, wgpu::BindGroup, u64)>,
    /// Écrans posés : numéro et emplacement.
    anchors: Vec<(u32, ScreenAnchor)>,
    /// Écrans dont les sommets ont été envoyés (`None` : rien d'envoyé).
    uploaded: Option<Vec<(u32, ScreenAnchor)>>,
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

        const ATTRS: [wgpu::VertexAttribute; 5] =
            wgpu::vertex_attr_array![
                0 => Float32x3,
                1 => Float32x2,
                2 => Float32x2,
                3 => Float32x4,
                4 => Float32x4
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
        // (v199) Dessiné dans la passe des translucides : sa 3e cible (l'eau) n'est pas
        // écrite.
        let taa = pipeline(
            "écran (TAA)",
            "fs_taa_world",
            &[
                target(COLOR_FORMAT),
                target(MOTION_FORMAT),
                Some(wgpu::ColorTargetState {
                    format: WATER_GB_FORMAT,
                    blend: None,
                    write_mask: wgpu::ColorWrites::empty(),
                }),
            ],
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
            textures: std::collections::HashMap::new(),
            anchors: Vec::new(),
            uploaded: None,
        }
    }

    /// Écrans posés : numéro et emplacement (les suivants au-delà de `MAX_SCREENS` sont
    /// ignorés). L'image d'un écran retiré est oubliée.
    pub fn set_anchors(&mut self, anchors: &[(u32, ScreenAnchor)]) {
        if self.anchors != anchors {
            self.anchors.clear();
            self.anchors
                .extend_from_slice(&anchors[..anchors.len().min(MAX_SCREENS)]);
            let ids: std::collections::HashSet<u32> = self.anchors.iter().map(|a| a.0).collect();
            self.textures.retain(|id, _| ids.contains(id));
        }
    }

    /// Nouvelle image de l'écran `id` (BGRA 8 bits, `width * height * 4` octets), envoyée
    /// seulement si sa `version` a changé.
    #[allow(clippy::too_many_arguments)]
    pub fn set_frame(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        id: u32,
        version: u64,
        width: u32,
        height: u32,
        pixels: &[u8],
    ) {
        if self.textures.get(&id).is_some_and(|t| t.2 == version)
            || width == 0
            || height == 0
            || pixels.len() < (width * height * 4) as usize
            || !self.anchors.iter().any(|a| a.0 == id)
        {
            return;
        }
        let size = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        if self.textures.get(&id).is_none_or(|t| t.0.size() != size) {
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
            self.textures.insert(id, (texture, group, version));
        }
        let Some(entry) = self.textures.get_mut(&id) else {
            return;
        };
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &entry.0,
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
        entry.2 = version;
    }

    /// Image suivante : renvoie les sommets si des écrans ont été posés ou retirés.
    pub fn update(&mut self, queue: &wgpu::Queue) {
        if self.uploaded.as_deref() == Some(&self.anchors[..]) {
            return;
        }
        let mut verts = Vec::with_capacity(self.anchors.len() * 6);
        for (_, a) in &self.anchors {
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
                reveal: a.reveal,
                cursor: a.cursor,
                ripple: a.ripple,
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

    /// Dessine les écrans dans la passe du monde (le group 0 est déjà lié), chacun avec son
    /// image (un écran sans image encore reçue n'est pas dessiné).
    pub fn draw<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>, taa: bool) {
        if self.anchors.is_empty() || self.uploaded.as_deref() != Some(&self.anchors[..]) {
            return;
        }
        pass.set_pipeline(if taa { &self.taa } else { &self.plain });
        pass.set_vertex_buffer(0, self.vbuf.slice(..));
        for (k, (id, _)) in self.anchors.iter().enumerate() {
            if let Some((_, group, _)) = self.textures.get(id) {
                pass.set_bind_group(1, group, &[]);
                let first = 6 * k as u32;
                pass.draw(first..first + 6, 0..1);
            }
        }
    }
}
