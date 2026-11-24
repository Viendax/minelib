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
/// (v199) Ce que les translucides (eau, verre, glace) laissent pour l'éclairage différé de
/// leur face : profondeur, albédo, normale et poids (voir `fs_water`, dyn_deferred.wgsl).
pub const WATER_GB_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba32Uint;

/// Cibles en plus de la couleur et du mouvement.
#[derive(Clone, Copy)]
pub(crate) enum Extra {
    /// Passe des opaques : albédo et surface (éclairage différé).
    Gbuffer(wgpu::ColorWrites),
    /// (v199) Passe des translucides : la face pour l'éclairage différé de l'eau.
    Water(wgpu::ColorWrites),
}
/// Ce que le monde opaque laisse pour l'éclairage différé des lumières dynamiques (voir
/// `shaders/gbuffer.wgsl`) : albédo et brillance, normale et poids.
pub const GB_ALBEDO_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
pub const GB_SURF_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
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
    /// Change à chaque fois que les images sont recréées (liaisons qui les lisent à refaire).
    pub generation: u64,
    pub color_view: wgpu::TextureView,
    pub motion_view: wgpu::TextureView,
    /// Albédo et surface (éclairage différé des lumières dynamiques).
    pub gbuf_views: [wgpu::TextureView; 2],
    /// (v199) Face translucide de chaque pixel pour l'éclairage différé de l'eau (effacée à
    /// chaque image par la passe des translucides).
    pub water_view: wgpu::TextureView,
    /// Profondeurs en alternance : la scène écrit `depth_views[i]` pendant que l'image
    /// précédente reste lisible dans l'autre (plus de copie de la profondeur à chaque image).
    pub depth_views: [wgpu::TextureView; 2],
    history_views: [wgpu::TextureView; 2],
    /// Résolution qui écrit l'historique `i` en lisant l'autre.
    resolve_groups: [wgpu::BindGroup; 2],
    present_groups: [wgpu::BindGroup; 2],
}

pub struct Taa {
    /// Blocs, miroirs, translucides : prêts un peu après le lancement.
    pub heavy: crate::lazy::Lazy<Heavy>,
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
    /// (v268) Pas de la caméra depuis l'image d'avant, rapporté au plan proche (blocs / 0,05),
    /// et angle dont elle a tourné (rad) : marge de la comparaison des profondeurs (taa.wgsl,
    /// `conf`). Zéro : immobile.
    pub cam_step: f32,
    pub cam_turn: f32,
    /// Historique écrit à la dernière résolution.
    written: usize,
    /// Vue-projection sans décalage de l'image précédente.
    pub prev_view_proj: Option<glam::Mat4>,
    /// (v254) Et sa caméra : les shaders la reçoivent comptée depuis celle d'aujourd'hui.
    pub prev_eye: Option<glam::Vec3>,
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

/// Pipelines lourds du monde (shader complet).
pub struct Heavy {
    pub block: wgpu::RenderPipeline,
    pub reflective: wgpu::RenderPipeline,
    pub translucent: wgpu::RenderPipeline,
    /// (v202) Translucides sans le code du raytracing, quand il est coupé (anticrénelage
    /// seulement) : même image, plus léger pour la carte.
    pub translucent_flat: Option<wgpu::RenderPipeline>,
}

/// Un pipeline du monde qui écrit la couleur et le mouvement (TAA), et, dans la passe des
/// opaques, l'albédo et la surface (`gbuffer` : ce qu'il en écrit).
#[allow(clippy::too_many_arguments)]
fn world_pipeline(
    device: &wgpu::Device,
    world_layout: &wgpu::PipelineLayout,
    world_shader: &wgpu::ShaderModule,
    label: &str,
    vs: &str,
    fs: &str,
    buffers: &[wgpu::VertexBufferLayout],
    cull: Option<wgpu::Face>,
    depth_write: bool,
    compare: wgpu::CompareFunction,
    blend: Option<wgpu::BlendState>,
    motion_writes: wgpu::ColorWrites,
    extra: Option<Extra>,
) -> wgpu::RenderPipeline {
    // (v290) Translucides (`Extra::Water`) : leur mouvement est mêlé à celui de ce qui est
    // derrière, à la part que donne le shader (`translucent_out` de world.wgsl : rien quand
    // la face est immobile à l'écran, pour ne pas effacer celui d'un mob qui passe derrière).
    // (Pas le contour du bloc visé, qui n'écrit pas de mouvement et n'en sort que deux
    // composantes.)
    let mixed = matches!(extra, Some(Extra::Water(_))) && !motion_writes.is_empty();
    let motion_blend = mixed.then_some(wgpu::BlendState {
        color: wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::SrcAlpha,
            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
            operation: wgpu::BlendOperation::Add,
        },
        alpha: wgpu::BlendComponent::REPLACE,
    });
    let mut targets = vec![
        Some(wgpu::ColorTargetState {
            format: COLOR_FORMAT,
            blend,
            write_mask: wgpu::ColorWrites::ALL,
        }),
        Some(wgpu::ColorTargetState {
            format: MOTION_FORMAT,
            blend: motion_blend,
            write_mask: motion_writes,
        }),
    ];
    match extra {
        Some(Extra::Gbuffer(mask)) => {
            for format in [GB_ALBEDO_FORMAT, GB_SURF_FORMAT] {
                targets.push(Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: mask,
                }));
            }
        }
        Some(Extra::Water(mask)) => targets.push(Some(wgpu::ColorTargetState {
            format: WATER_GB_FORMAT,
            blend: None,
            write_mask: mask,
        })),
        None => {}
    }
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
            targets: &targets,
        }),
        multiview: None,
        cache: None,
    })
}

/// Blocs, miroirs (passe des opaques : ils laissent aussi albédo et surface pour
/// l'éclairage différé), translucides (passe suivante, à deux cibles), compilés en parallèle.
pub(crate) fn build_heavy(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
) -> Heavy {
    let all = wgpu::ColorWrites::ALL;
    let back = Some(wgpu::Face::Back);
    let mk = |label: &str, fs: &str| {
        block_pipeline(
            device,
            layout,
            shader,
            label,
            fs,
            back,
            true,
            None,
            all,
            Some(Extra::Gbuffer(all)),
        )
    };
    std::thread::scope(|sc| {
        let block = sc.spawn(|| mk("blocs (TAA)", "fs_block_fast_mv"));
        let reflective = sc.spawn(|| mk("reflets (TAA)", "fs_block_mv"));
        let translucent = sc.spawn(|| {
            translucent_pipeline(
                device,
                layout,
                shader,
                "translucide (TAA)",
                "fs_block_translucent_mv",
            )
        });
        let translucent_flat = sc.spawn(|| {
            translucent_pipeline(
                device,
                layout,
                shader,
                "translucide sans raytracing (TAA)",
                "fs_block_translucent_flat_mv",
            )
        });
        Heavy {
            block: block.join().expect("pipeline des blocs (TAA)"),
            reflective: reflective.join().expect("pipeline réfléchissant (TAA)"),
            translucent: translucent.join().expect("pipeline translucide (TAA)"),
            translucent_flat: Some(translucent_flat.join().expect("pipeline translucide (TAA)")),
        }
    })
}

/// Translucides (eau, glace, verre teinté) : couleur mélangée. (v268) Leur mouvement remplace
/// celui de ce qui est derrière (`translucent_out` de world.wgsl : avant, il était gardé, et un
/// amas de verre devant le ciel s'étalait en se déplaçant). (v290) Sauf quand la face est
/// immobile à l'écran : celui de derrière reste (un mob derrière une vitre traînait). (v199) Ils laissent aussi leur face
/// (profondeur, albédo, normale, poids) : les lumières dynamiques y sont ajoutées après coup,
/// comme sur l'opaque.
fn translucent_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    label: &str,
    fs: &str,
) -> wgpu::RenderPipeline {
    block_pipeline(
        device,
        layout,
        shader,
        label,
        fs,
        None,
        false,
        Some(wgpu::BlendState::ALPHA_BLENDING),
        wgpu::ColorWrites::ALL,
        Some(Extra::Water(wgpu::ColorWrites::ALL)),
    )
}

#[allow(clippy::too_many_arguments)]
fn block_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    label: &str,
    fs: &str,
    cull: Option<wgpu::Face>,
    depth_write: bool,
    blend: Option<wgpu::BlendState>,
    motion_writes: wgpu::ColorWrites,
    extra: Option<Extra>,
) -> wgpu::RenderPipeline {
    let vertex_layout = crate::world_vertex_layout();
    world_pipeline(
        device,
        layout,
        shader,
        label,
        "vs_block",
        fs,
        std::slice::from_ref(&vertex_layout),
        cull,
        depth_write,
        wgpu::CompareFunction::Greater, // Z inversé
        blend,
        motion_writes,
        extra,
    )
}

impl Taa {
    pub fn new(
        device: &wgpu::Device,
        world_layout: &wgpu::PipelineLayout,
        world_shader: &wgpu::ShaderModule,
        surface_format: wgpu::TextureFormat,
    ) -> Self {
        // Shaders lourds du monde : compilés en parallèle sur des fils à part (plusieurs
        // secondes chacun sous DirectX), sans retarder l'ouverture du jeu. Les menus n'en
        // ont pas besoin ; le monde s'affiche dès qu'ils sont prêts.
        let heavy = {
            let (device, layout, shader) =
                (device.clone(), world_layout.clone(), world_shader.clone());
            crate::lazy::Lazy::spawn("shaders du monde (TAA)", move || {
                let t0 = std::time::Instant::now();
                let out = build_heavy(&device, &layout, &shader);
                log::info!(
                    "shaders du monde (TAA) compilés en {:.1} s (en tâche de fond)",
                    t0.elapsed().as_secs_f32()
                );
                out
            })
        };
        // (v203) Dessiné après les opaques, seulement là où rien ne l'a été : sa profondeur
        // (0, l'infini) ne passe le test « supérieur ou égal » que sur la valeur effacée.
        // Plus calculé sous tout le monde.
        let sky = world_pipeline(
            device,
            world_layout,
            world_shader,
            "ciel (TAA)",
            "vs_sky",
            "fs_sky_mv",
            &[],
            None,
            false,
            wgpu::CompareFunction::GreaterEqual,
            None,
            wgpu::ColorWrites::ALL,
            // Dans la passe des opaques : rien pour l'éclairage différé (le ciel n'en prend pas).
            Some(Extra::Gbuffer(wgpu::ColorWrites::empty())),
        );
        let outline = world_pipeline(
            device,
            world_layout,
            world_shader,
            "contour (TAA)",
            "vs_outline",
            "fs_outline_mv",
            &[],
            None,
            false,
            wgpu::CompareFunction::GreaterEqual,
            Some(wgpu::BlendState::ALPHA_BLENDING),
            wgpu::ColorWrites::empty(),
            // Passe des translucides : rien pour l'eau.
            Some(Extra::Water(wgpu::ColorWrites::empty())),
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
            heavy,
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
            cam_step: 0.0,
            cam_turn: 0.0,
            prev_view_proj: None,
            prev_eye: None,
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
        let gbuf_views = [
            tex("albédo (éclairage différé)", GB_ALBEDO_FORMAT, rt),
            tex("surface (éclairage différé)", GB_SURF_FORMAT, rt),
        ];
        let water_view = tex("eau (éclairage différé)", WATER_GB_FORMAT, rt);
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
        static GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        self.targets = Some(Targets {
            size,
            internal,
            generation: GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            color_view,
            motion_view,
            gbuf_views,
            water_view,
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
            still: [
                still,
                if self.scene_changed { 1.0 } else { 0.0 },
                self.cam_step,
                self.cam_turn,
            ],
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
