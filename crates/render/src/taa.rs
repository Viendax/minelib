//! Anticrénelage temporel (TAA), écrit pour Minelib.
//!
//! 1. La scène est dessinée hors écran avec un décalage sous-pixel différent à chaque image
//!    (suite de Halton), plus un tampon de vecteurs de mouvement.
//! 2. Passe de résolution : mélange avec l'historique reprojeté (voir `shaders/taa.wgsl`).
//!    Deux images d'historique servent alternativement de lecture et d'écriture.
//! 3. Copie à l'écran avec un léger renforcement de netteté, puis l'interface par-dessus.

use crate::DEPTH_FORMAT;

pub const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
pub const MOTION_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rg16Float;
/// Part de l'image courante dans le mélange (le reste vient de l'historique).
const BLEND: f32 = 0.1;
/// Longueur de la séquence de décalages.
const PHASES: u32 = 8;

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    size: [f32; 4],
    still: [f32; 4],
    /// xy = taille de l'image du monde (échelle de rendu), z = échelle, w = 0.
    internal: [f32; 4],
    /// xy = décalage sous-pixel de cette image (pixels du monde).
    jitter: [f32; 4],
}

pub struct Targets {
    /// Taille de l'écran (historique) ; `internal` : celle du monde dessiné.
    pub size: (u32, u32),
    pub internal: (u32, u32),
    pub color_view: wgpu::TextureView,
    pub motion_view: wgpu::TextureView,
    /// Profondeurs en alternance : la scène écrit `depth_views[i]` pendant que l'image
    /// précédente reste lisible dans l'autre (plus de copie de la profondeur à chaque image).
    pub depth_views: [wgpu::TextureView; 2],
    history_views: [wgpu::TextureView; 2],
    /// Résolution qui écrit l'historique `i` en lisant l'autre.
    resolve_groups: [wgpu::BindGroup; 2],
    present_groups: [wgpu::BindGroup; 2],
}

pub struct Taa {
    pub block: wgpu::RenderPipeline,
    pub reflective: wgpu::RenderPipeline,
    pub translucent: wgpu::RenderPipeline,
    pub sky: wgpu::RenderPipeline,
    pub outline: wgpu::RenderPipeline,
    resolve: wgpu::RenderPipeline,
    resolve_layout: wgpu::BindGroupLayout,
    present: wgpu::RenderPipeline,
    present_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    params: wgpu::Buffer,
    pub targets: Option<Targets>,
    frame: u32,
    /// Nombre d'images depuis que la caméra ne bouge plus.
    pub still_frames: u32,
    /// La scène vient de changer : traiter les pixels comme en mouvement.
    pub scene_changed: bool,
    /// Historique écrit à la dernière résolution.
    written: usize,
    /// Vue-projection sans décalage de l'image précédente.
    pub prev_view_proj: Option<glam::Mat4>,
    pub reset: bool,
}

/// Suite de Halton (bases 2 et 3) : décalages bien répartis dans le pixel, dans [-0,5 ; 0,5].
pub fn halton_jitter(index: u32) -> (f32, f32) {
    fn halton(mut i: u32, base: u32) -> f32 {
        let (mut f, mut r) = (1.0f32, 0.0f32);
        while i > 0 {
            f /= base as f32;
            r += f * (i % base) as f32;
            i /= base;
        }
        r
    }
    (halton(index + 1, 2) - 0.5, halton(index + 1, 3) - 0.5)
}

fn tex_entry(binding: u32, sample_type: wgpu::TextureSampleType) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type,
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn fullscreen(
    device: &wgpu::Device,
    label: &str,
    shader: &wgpu::ShaderModule,
    fs: &str,
    bgl: &wgpu::BindGroupLayout,
    format: wgpu::TextureFormat,
    with_depth: bool,
) -> wgpu::RenderPipeline {
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(label),
        bind_group_layouts: &[bgl],
        push_constant_ranges: &[],
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_full"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        primitive: Default::default(),
        // L'interface est dessinée dans la passe de copie (elle attend une profondeur).
        depth_stencil: with_depth.then(|| wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: false,
            depth_compare: wgpu::CompareFunction::Always,
            stencil: Default::default(),
            bias: Default::default(),
        }),
        multisample: Default::default(),
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some(fs),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview: None,
        cache: None,
    })
}

impl Taa {
    pub fn new(
        device: &wgpu::Device,
        world_layout: &wgpu::PipelineLayout,
        world_shader: &wgpu::ShaderModule,
        vertex_layout: &wgpu::VertexBufferLayout,
        surface_format: wgpu::TextureFormat,
    ) -> Self {
        let mk = |label: &str,
                  vs: &str,
                  fs: &str,
                  buffers: &[wgpu::VertexBufferLayout],
                  cull: Option<wgpu::Face>,
                  depth_write: bool,
                  compare: wgpu::CompareFunction,
                  blend: Option<wgpu::BlendState>,
                  motion_writes: wgpu::ColorWrites| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(world_layout),
                vertex: wgpu::VertexState {
                    module: world_shader,
                    entry_point: Some(vs),
                    compilation_options: Default::default(),
                    buffers,
                },
                primitive: wgpu::PrimitiveState {
                    topology: if vs == "vs_outline" {
                        wgpu::PrimitiveTopology::LineList
                    } else {
                        wgpu::PrimitiveTopology::TriangleList
                    },
                    cull_mode: cull,
                    ..Default::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: depth_write,
                    depth_compare: compare,
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: Default::default(),
                fragment: Some(wgpu::FragmentState {
                    module: world_shader,
                    entry_point: Some(fs),
                    compilation_options: Default::default(),
                    targets: &[
                        Some(wgpu::ColorTargetState {
                            format: COLOR_FORMAT,
                            blend,
                            write_mask: wgpu::ColorWrites::ALL,
                        }),
                        Some(wgpu::ColorTargetState {
                            format: MOTION_FORMAT,
                            blend: None,
                            write_mask: motion_writes,
                        }),
                    ],
                }),
                multiview: None,
                cache: None,
            })
        };
        let vl = std::slice::from_ref(vertex_layout);
        let greater = wgpu::CompareFunction::Greater; // Z inversé
        let block = mk(
            "blocs (TAA)",
            "vs_block",
            "fs_block_fast_mv",
            vl,
            Some(wgpu::Face::Back),
            true,
            greater,
            None,
            wgpu::ColorWrites::ALL,
        );
        let reflective = mk(
            "reflets (TAA)",
            "vs_block",
            "fs_block_mv",
            vl,
            Some(wgpu::Face::Back),
            true,
            greater,
            None,
            wgpu::ColorWrites::ALL,
        );
        // Translucides : couleur mélangée, on garde le mouvement de ce qui est derrière.
        let translucent = mk(
            "translucide (TAA)",
            "vs_block",
            "fs_block_translucent_mv",
            vl,
            None,
            false,
            greater,
            Some(wgpu::BlendState::ALPHA_BLENDING),
            wgpu::ColorWrites::empty(),
        );
        let sky = mk(
            "ciel (TAA)",
            "vs_sky",
            "fs_sky_mv",
            &[],
            None,
            false,
            wgpu::CompareFunction::Always,
            None,
            wgpu::ColorWrites::ALL,
        );

        let outline = mk(
            "contour (TAA)",
            "vs_outline",
            "fs_outline_mv",
            &[],
            None,
            false,
            wgpu::CompareFunction::GreaterEqual,
            Some(wgpu::BlendState::ALPHA_BLENDING),
            wgpu::ColorWrites::empty(),
        );
        let float = wgpu::TextureSampleType::Float { filterable: true };
        let resolve_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("TAA"),
            entries: &[
                tex_entry(0, float),
                tex_entry(1, float),
                tex_entry(2, wgpu::TextureSampleType::Depth),
                tex_entry(3, float),
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                tex_entry(6, wgpu::TextureSampleType::Depth),
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let present_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("copie TAA"),
            entries: &[tex_entry(0, float)],
        });
        let taa_shader = device.create_shader_module(wgpu::include_wgsl!("shaders/taa.wgsl"));
        let present_shader =
            device.create_shader_module(wgpu::include_wgsl!("shaders/present.wgsl"));
        let resolve = fullscreen(
            device,
            "TAA",
            &taa_shader,
            "fs_resolve",
            &resolve_layout,
            COLOR_FORMAT,
            false,
        );
        let present = fullscreen(
            device,
            "copie TAA",
            &present_shader,
            "fs_present",
            &present_layout,
            surface_format,
            true,
        );
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("TAA"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("TAA"),
            size: std::mem::size_of::<Params>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            block,
            reflective,
            translucent,
            sky,
            outline,
            resolve,
            resolve_layout,
            present,
            present_layout,
            sampler,
            params,
            targets: None,
            frame: 0,
            written: 0,
            still_frames: 0,
            scene_changed: false,
            prev_view_proj: None,
            reset: true,
        }
    }

    /// (Re)crée les images si la taille de la fenêtre ou l'échelle de rendu a changé.
    pub fn prepare(&mut self, device: &wgpu::Device, size: (u32, u32), scale: f32) {
        let internal = (
            ((size.0 as f32 * scale).round() as u32).max(1),
            ((size.1 as f32 * scale).round() as u32).max(1),
        );
        if self
            .targets
            .as_ref()
            .is_some_and(|t| t.size == size && t.internal == internal)
        {
            return;
        }
        // Monde (couleur, mouvement, profondeur) à l'échelle de rendu ; historique à l'écran.
        let raw = |label: &str, format, usage| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width: internal.0,
                    height: internal.1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            })
        };
        let tex_at = |label: &str, format, usage, sz: (u32, u32)| {
            device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d {
                        width: sz.0,
                        height: sz.1,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage,
                    view_formats: &[],
                })
                .create_view(&Default::default())
        };
        let rt = wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING;
        let tex = |label: &str, format, usage| tex_at(label, format, usage, internal);
        let color_view = tex("couleur (TAA)", COLOR_FORMAT, rt);
        let motion_view = tex("mouvement", MOTION_FORMAT, rt);
        let depth_views = [
            raw("profondeur (TAA) 0", DEPTH_FORMAT, rt).create_view(&Default::default()),
            raw("profondeur (TAA) 1", DEPTH_FORMAT, rt).create_view(&Default::default()),
        ];
        let history_views = [
            tex_at("historique 0", COLOR_FORMAT, rt, size),
            tex_at("historique 1", COLOR_FORMAT, rt, size),
        ];
        // Écrire l'historique `write` : lire l'autre historique, la profondeur de cette image
        // (`depth_views[write]`) et celle de l'image précédente (l'autre).
        let resolve_group = |write: usize| {
            let read = 1 - write;
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("TAA"),
                layout: &self.resolve_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&color_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&motion_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(&depth_views[write]),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::TextureView(&history_views[read]),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 5,
                        resource: self.params.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 6,
                        resource: wgpu::BindingResource::TextureView(&depth_views[read]),
                    },
                ],
            })
        };
        let resolve_groups = [resolve_group(0), resolve_group(1)];
        let present_group = |i: usize| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("copie TAA"),
                layout: &self.present_layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&history_views[i]),
                }],
            })
        };
        let present_groups = [present_group(0), present_group(1)];
        self.targets = Some(Targets {
            size,
            internal,
            color_view,
            motion_view,
            depth_views,
            history_views,
            resolve_groups,
            present_groups,
        });
        self.reset = true;
    }

    /// Décalage sous-pixel de cette image (pixels du monde, y vers le bas). Avec une échelle
    /// de rendu réduite, la séquence est plus longue : chaque pixel de l'écran finit par être
    /// couvert par des échantillons du monde.
    pub fn jitter(&self) -> (f32, f32) {
        let scaled = self.targets.as_ref().is_some_and(|t| t.internal != t.size);
        halton_jitter(self.frame % if scaled { PHASES * 4 } else { PHASES })
    }

    /// Passe de résolution : écrit le nouvel historique.
    pub fn resolve(
        &mut self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        timestamps: Option<wgpu::RenderPassTimestampWrites<'_>>,
    ) {
        let Some(t) = &self.targets else { return };
        let write = (self.frame % 2) as usize;
        let reset = std::mem::take(&mut self.reset);
        // Caméra immobile : moyenne de plus en plus longue (1/9 … 1/64) — tous les décalages
        // sous-pixel finissent par peser autant, l'image ne bouge plus.
        let still = 1.0 / (9 + self.still_frames).min(64) as f32;
        let (jx, jy) = self.jitter();
        let scale = t.internal.0 as f32 / t.size.0 as f32;
        let p = Params {
            size: [
                t.size.0 as f32,
                t.size.1 as f32,
                if reset { 1.0 } else { 0.0 },
                BLEND,
            ],
            still: [still, if self.scene_changed { 1.0 } else { 0.0 }, 0.0, 0.0],
            internal: [t.internal.0 as f32, t.internal.1 as f32, scale, 0.0],
            jitter: [jx, jy, 0.0, 0.0],
        };
        queue.write_buffer(&self.params, 0, bytemuck::bytes_of(&p));
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("TAA"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &t.history_views[write],
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: timestamps,
            occlusion_query_set: None,
        });
        pass.set_pipeline(&self.resolve);
        pass.set_bind_group(0, &t.resolve_groups[write], &[]);
        pass.draw(0..3, 0..1);
        drop(pass);
        self.written = write;
        self.frame = self.frame.wrapping_add(1);
    }

    /// Profondeur (`targets.depth_views[i]`) où la scène de cette image est dessinée.
    pub fn depth_index(&self) -> usize {
        (self.frame % 2) as usize
    }

    /// Copie de l'historique à l'écran (dans la passe qui dessine ensuite l'interface).
    pub fn present<'p>(&'p self, pass: &mut wgpu::RenderPass<'p>) {
        let Some(t) = &self.targets else { return };
        pass.set_pipeline(&self.present);
        pass.set_bind_group(0, &t.present_groups[self.written], &[]);
        pass.draw(0..3, 0..1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jitter_stays_inside_the_pixel_and_averages_to_zero() {
        let (mut sx, mut sy) = (0.0f32, 0.0f32);
        for i in 0..PHASES {
            let (x, y) = halton_jitter(i);
            assert!((-0.5..=0.5).contains(&x) && (-0.5..=0.5).contains(&y));
            sx += x;
            sy += y;
        }
        assert!(
            (sx / PHASES as f32).abs() < 0.08 && (sy / PHASES as f32).abs() < 0.08,
            "{sx} {sy}"
        );
    }
}
