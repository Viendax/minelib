//! (v200) Essai : les joueurs et les objets reflétés dans l'eau, sans raytracing. Le jeu
//! choisit le niveau de l'eau qu'on voit (`FrameParams::water_mirror`) ; la scène y est
//! redessinée reflétée, comme dans un miroir couché (voir `planar`), dans une image à part
//! à la taille du monde :
//! - les blocs n'y laissent que leur profondeur : ils cachent les joueurs qui sont derrière
//!   eux, vus depuis le reflet de l'œil ;
//! - les joueurs et les objets y laissent leur couleur ;
//! - le reste est transparent.
//!
//! Le dessus de l'eau à ce niveau y lit son reflet (`water_mirror` dans world.wgsl),
//! par-dessus celui du raytracing ou du ciel. Seulement à portée des reflets ([`RANGE`]),
//! et pas soi-même à la première personne (sa tête reflétée boucherait la vue au ras de
//! l'eau).

use crate::planar::Plane;

/// Portée (blocs, à l'horizontale) : celle des reflets, au-delà ils se sont effacés. Les
/// joueurs plus loin ne sont pas dessinés non plus (`WATER_MIRROR_RANGE` dans
/// players.wgsl) : les blocs qui les cacheraient n'y sont pas.
pub const RANGE: f32 = 64.0;

/// Le plan de l'eau au niveau `level`, vu d'au-dessus (un miroir couché).
pub fn plane(level: f32) -> Plane {
    Plane {
        axis: 1,
        side: 1.0,
        coord: level,
        lo: [0.0; 2],
        hi: [0.0; 2],
    }
}

/// Coupe au plan de l'eau (`dot(xyz, p) + w >= 0` au-dessus), multipliée par 1 000 : les
/// joueurs et les objets, gardés jusqu'à 0,9 bloc derrière un miroir (`MIRROR_SLACK` dans
/// entity.wgsl et players.wgsl), sont ici coupés au millième de bloc. Ce qui est sous l'eau
/// ne se reflète pas dans sa surface. (Les joueurs y reconnaissent aussi l'image de l'eau :
/// y > 500, une coupe de miroir a une normale unitaire.)
pub fn clip(level: f32) -> [f32; 4] {
    [0.0, 1000.0, 0.0, -1000.0 * level]
}

/// L'image des reflets dans l'eau et ce qu'il faut pour la dessiner.
pub(crate) struct WaterMirror {
    pub size: (u32, u32),
    format: wgpu::TextureFormat,
    /// Couleur (format de l'écran, comme les images des miroirs : les pipelines sans
    /// anticrénelage des joueurs et des entités y dessinent tels quels), et profondeur.
    pub color: wgpu::TextureView,
    pub depth: wgpu::TextureView,
    /// Image vide (1 × 1) liée à la place de la sienne dans sa propre passe (une passe ne
    /// peut lire l'image où elle dessine).
    pub dummy: wgpu::TextureView,
    /// Uniformes du monde vus du reflet de l'œil, et leur groupe de liaison.
    pub globals: wgpu::Buffer,
    pub group: Option<wgpu::BindGroup>,
    /// Dessins indirects des blocs (comme pour l'image) : tampon, capacité, commandes, et
    /// groupes (page, première commande, nombre).
    pub indirect: wgpu::Buffer,
    pub indirect_cap: u64,
    pub cmds: Vec<u8>,
    pub groups: Vec<(usize, u32, u32)>,
    /// Niveau de l'eau redessiné à cette image (None : pas d'image).
    pub level: Option<f32>,
}

fn texture(
    device: &wgpu::Device,
    label: &str,
    format: wgpu::TextureFormat,
    size: (u32, u32),
    usage: wgpu::TextureUsages,
) -> wgpu::TextureView {
    device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: size.0.max(1),
                height: size.1.max(1),
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
}

impl WaterMirror {
    pub fn new(device: &wgpu::Device, surface: wgpu::TextureFormat, size: (u32, u32)) -> Self {
        let format = crate::planar::format(surface);
        let both = wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING;
        Self {
            size,
            format,
            color: texture(device, "reflets dans l'eau", format, size, both),
            depth: texture(
                device,
                "reflets dans l'eau (profondeur)",
                crate::DEPTH_FORMAT,
                size,
                wgpu::TextureUsages::RENDER_ATTACHMENT,
            ),
            dummy: texture(
                device,
                "reflets dans l'eau (vide)",
                format,
                (1, 1),
                wgpu::TextureUsages::TEXTURE_BINDING,
            ),
            globals: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("uniformes des reflets dans l'eau"),
                size: std::mem::size_of::<crate::Globals>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            group: None,
            indirect: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("dessins indirects des reflets dans l'eau"),
                size: 20 * 256,
                usage: wgpu::BufferUsages::INDIRECT | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            indirect_cap: 256,
            cmds: Vec::new(),
            groups: Vec::new(),
            level: None,
        }
    }

    /// Images refaites à la taille `size` (le groupe de liaison est à refaire, et ceux du
    /// monde, qui lisent l'image).
    pub fn resize(&mut self, device: &wgpu::Device, size: (u32, u32)) {
        let fresh = Self::new(device, self.format, size);
        self.size = size;
        self.color = fresh.color;
        self.depth = fresh.depth;
        self.group = None;
    }

    /// Commandes de dessin indirect des blocs à envoyer (capacité du tampon agrandie au
    /// besoin).
    pub fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        let n = (self.cmds.len() / 20) as u64;
        if n > self.indirect_cap {
            self.indirect_cap = n.next_power_of_two();
            self.indirect = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("dessins indirects des reflets dans l'eau"),
                size: 20 * self.indirect_cap,
                usage: wgpu::BufferUsages::INDIRECT | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        if !self.cmds.is_empty() {
            queue.write_buffer(&self.indirect, 0, &self.cmds);
        }
    }
}

/// Blocs de l'image : leur profondeur seulement (`fs_block_depth`), coupés au plan de
/// l'eau. Faces arrière écartées, comme dans les miroirs (le retournement gauche-droite de
/// l'image rétablit le sens des faces).
pub(crate) fn depth_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    surface: wgpu::TextureFormat,
) -> wgpu::RenderPipeline {
    let vertex_layout = crate::world_vertex_layout();
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("reflets dans l'eau : blocs (profondeur)"),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_block"),
            compilation_options: Default::default(),
            buffers: std::slice::from_ref(&vertex_layout),
        },
        primitive: wgpu::PrimitiveState {
            cull_mode: Some(wgpu::Face::Back),
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: crate::DEPTH_FORMAT,
            depth_write_enabled: true,
            depth_compare: wgpu::CompareFunction::Greater,
            stencil: Default::default(),
            bias: Default::default(),
        }),
        multisample: Default::default(),
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some("fs_block_depth"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: crate::planar::format(surface),
                blend: None,
                write_mask: wgpu::ColorWrites::empty(),
            })],
        }),
        multiview: None,
        cache: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::{Mat4, Vec3, Vec4};

    /// Un joueur au bord de l'eau et son reflet : vu de la caméra, le point de la surface où
    /// l'on voit son reflet tombe, dans l'image (retournée de gauche à droite), au pixel où
    /// le joueur y est dessiné. Ce qui est sous l'eau est coupé, net.
    #[test]
    fn the_water_image_lines_up_with_the_water_on_screen() {
        let level = 62.0 + 8.0 / 9.0;
        let pl = plane(level);
        let eye = Vec3::new(3.0, 66.0, 7.0);
        let view = Mat4::look_at_rh(eye, Vec3::new(12.0, 62.0, 2.0), Vec3::Y);
        let vp = Mat4::perspective_infinite_reverse_rh(1.2, 16.0 / 9.0, 0.05) * view;
        let wvp = pl.view_proj(vp);
        for q in [
            Vec3::new(14.0, 64.5, 1.0),
            Vec3::new(10.0, 63.2, 3.5),
            Vec3::new(18.0, 66.0, -2.0),
        ] {
            // Le joueur dessiné dans l'image de l'eau.
            let drawn = wvp * q.extend(1.0);
            let drawn = drawn.truncate() / drawn.w;
            // Point de la surface où la caméra voit son reflet : sur la droite œil → image
            // du point sous l'eau.
            let mirrored = pl.reflect(q);
            let t = (eye.y - level) / (eye.y - mirrored.y);
            let surface = eye + (mirrored - eye) * t;
            assert!((surface.y - level).abs() < 1e-4);
            // Ce que fait `water_mirror` (world.wgsl) : la place de la surface à l'écran,
            // retournée de gauche à droite.
            let seen = vp * surface.extend(1.0);
            let seen = seen.truncate() / seen.w;
            assert!((seen.x + drawn.x).abs() < 1e-4, "{seen} {drawn}");
            assert!((seen.y - drawn.y).abs() < 1e-4, "{seen} {drawn}");
        }
        // Coupe : au-dessus gardé, un millième de bloc dessous déjà au-delà de la marge
        // des miroirs (0,9) pour les joueurs.
        let c = Vec4::from(clip(level));
        assert!(c.dot(Vec3::new(5.0, level + 0.01, 5.0).extend(1.0)) > 0.0);
        assert!(c.dot(Vec3::new(5.0, level - 0.001, 5.0).extend(1.0)) < -0.9);
        // Le reflet de l'œil est sous l'eau, à la même distance.
        let r = pl.reflect(eye);
        assert!((r.y - (2.0 * level - eye.y)).abs() < 1e-5 && r.x == eye.x && r.z == eye.z);
        // Les joueurs reconnaissent la passe de l'eau à sa coupe (y > 500), pas celle d'un
        // miroir (normale unitaire), et ils ont la même portée que les blocs.
        assert!(c.y > 500.0);
        for axis in 0..3 {
            for side in [-1.0, 1.0] {
                let m = Plane {
                    axis,
                    side,
                    coord: 3.0,
                    lo: [0.0; 2],
                    hi: [0.0; 2],
                };
                assert!(m.clip()[1] <= 1.0);
            }
        }
        let players = include_str!("shaders/players.wgsl");
        assert!(players.contains(&format!("WATER_MIRROR_RANGE: f32 = {RANGE:.1};")));
    }
}
