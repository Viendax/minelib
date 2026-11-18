//! (v190) Miroirs sans raytracing : pour les plans de miroir les plus visibles, la scène est
//! redessinée vue de l'autre côté du plan (la caméra reflétée dans le miroir) dans une
//! couche d'une image ; chaque face de miroir y lit son reflet (`planar_mirror` dans
//! world.wgsl). Le dessin se limite à ce que le miroir laisse voir : les chunks dans le cône
//! qui passe par son rectangle, les pixels de son rectangle à l'écran. On y voit les blocs,
//! l'eau et le verre, les autres joueurs, les objets, et soi-même (le modèle du joueur, même
//! à la première personne).
//!
//! Miroir dans le miroir : un miroir vu dans l'image d'un autre a sa propre image,
//! redessinée avant (caméra reflétée deux fois), et ainsi de suite ; deux miroirs face à
//! face montrent un couloir de reflets. Les images les plus grandes à l'écran d'abord
//! ([`plan`]), [`MAX_PLANAR`] de chaque côté au plus : niveaux impairs (vus de la caméra,
//! puis à trois reflets...) dans la première image, pairs dans la seconde ; une passe lit
//! l'autre image pendant qu'elle dessine dans la sienne. Au-delà, le fond du couloir est
//! sombre.
//!
//! L'image est retournée de gauche à droite (`FLIP_X`) : le reflet inverse le sens des
//! faces, le retournement le rétablit, et les pipelines gardent leur élimination des faces
//! arrière. Le raytracing ne sert plus aux miroirs redessinés (il reste pour les métaux,
//! la glace, l'eau, les miroirs en trop vus de la caméra, et la lumière des torches
//! renvoyée par les miroirs).

use glam::{Mat4, Vec3, Vec4};
use minelib_world::mesh::MirrorRect;

/// Images de miroir lues par une passe, au plus (vue de la caméra : miroirs vus de face ;
/// image d'un miroir : miroirs vus dedans), et couches de chacune des deux images des
/// miroirs : huit images redessinées par image au plus.
pub const MAX_PLANAR: usize = 4;

/// Miroir dans le miroir plus petit que ça (pixels) : pas d'image à lui, fond sombre.
pub const MIN_AREA: f32 = 256.0;

/// Portée des miroirs redessinés (blocs, comme celle des reflets : au-delà, le reflet
/// s'est effacé).
pub const RANGE: f32 = 64.0;

/// La boîte `min`..`max` touche-t-elle l'intérieur de tous les `planes` (normales vers
/// l'intérieur) ?
pub fn aabb_inside(planes: &[Vec4], min: Vec3, max: Vec3) -> bool {
    planes.iter().all(|pl| {
        let p = Vec3::new(
            if pl.x >= 0.0 { max.x } else { min.x },
            if pl.y >= 0.0 { max.y } else { min.y },
            if pl.z >= 0.0 { max.z } else { min.z },
        );
        pl.truncate().dot(p) + pl.w >= 0.0
    })
}

/// Format des images des miroirs : celui de l'écran (les pipelines sans anticrénelage du
/// ciel, des entités et des joueurs y dessinent tels quels).
pub fn format(surface: wgpu::TextureFormat) -> wgpu::TextureFormat {
    surface
}

/// Retournement gauche-droite (espace de découpage) appliqué à l'image d'un miroir.
const FLIP_X: Mat4 = Mat4::from_cols(
    Vec4::new(-1.0, 0.0, 0.0, 0.0),
    Vec4::new(0.0, 1.0, 0.0, 0.0),
    Vec4::new(0.0, 0.0, 1.0, 0.0),
    Vec4::new(0.0, 0.0, 0.0, 1.0),
);

/// Un plan de miroir à redessiner : axe (0 x, 1 y, 2 z), côté vers lequel ses faces
/// regardent (+1 ou −1), coordonnée, et rectangle des faces sur les axes (axe + 1) % 3 puis
/// (axe + 2) % 3 (blocs du monde).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Plane {
    pub axis: usize,
    pub side: f32,
    pub coord: f32,
    pub lo: [f32; 2],
    pub hi: [f32; 2],
}

impl Plane {
    pub fn from_rect(r: &MirrorRect) -> Self {
        Self {
            axis: r.axis as usize,
            side: r.side as f32,
            coord: r.coord as f32,
            lo: [r.lo[0] as f32, r.lo[1] as f32],
            hi: [r.hi[0] as f32, r.hi[1] as f32],
        }
    }

    fn same_plane(&self, r: &MirrorRect) -> bool {
        self.axis == r.axis as usize && self.side == r.side as f32 && self.coord == r.coord as f32
    }

    /// Normale tournée vers le côté d'où l'on voit le miroir.
    pub fn normal(&self) -> Vec3 {
        let mut n = Vec3::ZERO;
        n[self.axis] = self.side;
        n
    }

    /// Reflet d'un point dans le plan.
    pub fn reflect(&self, p: Vec3) -> Vec3 {
        let mut q = p;
        q[self.axis] = 2.0 * self.coord - p[self.axis];
        q
    }

    /// Reflet (monde → monde) en matrice.
    pub fn matrix(&self) -> Mat4 {
        let mut s = Vec3::ONE;
        s[self.axis] = -1.0;
        let mut t = Vec3::ZERO;
        t[self.axis] = 2.0 * self.coord;
        Mat4::from_translation(t) * Mat4::from_scale(s)
    }

    /// Vue-projection de l'image du miroir, d'après celle de la caméra (`vp`) : la scène
    /// reflétée, vue de la même caméra, retournée de gauche à droite.
    pub fn view_proj(&self, vp: Mat4) -> Mat4 {
        FLIP_X * vp * self.matrix()
    }

    /// Plan qui coupe la scène du miroir : dot(xyz, p) + w >= 0 devant lui.
    pub fn clip(&self) -> [f32; 4] {
        let n = self.normal();
        [n.x, n.y, n.z, -self.side * self.coord]
    }

    /// Quatre coins du rectangle des faces (monde), dans l'ordre du tour.
    pub fn corners(&self) -> [Vec3; 4] {
        let (a, b) = ((self.axis + 1) % 3, (self.axis + 2) % 3);
        let at = |u: f32, v: f32| {
            let mut p = Vec3::ZERO;
            p[self.axis] = self.coord;
            p[a] = u;
            p[b] = v;
            p
        };
        [
            at(self.lo[0], self.lo[1]),
            at(self.hi[0], self.lo[1]),
            at(self.hi[0], self.hi[1]),
            at(self.lo[0], self.hi[1]),
        ]
    }

    /// Distance de `p` au rectangle des faces.
    pub fn distance(&self, p: Vec3) -> f32 {
        let (a, b) = ((self.axis + 1) % 3, (self.axis + 2) % 3);
        let da = (self.lo[0] - p[a]).max(p[a] - self.hi[0]).max(0.0);
        let db = (self.lo[1] - p[b]).max(p[b] - self.hi[1]).max(0.0);
        let dn = p[self.axis] - self.coord;
        (da * da + db * db + dn * dn).sqrt()
    }

    /// `eye` voit-il le miroir de face (de son côté, pas au ras) ?
    pub fn faces(&self, eye: Vec3) -> bool {
        (eye[self.axis] - self.coord) * self.side > 0.02
    }

    /// Cône de ce que le miroir laisse voir, depuis le reflet de l'œil `eye` : plans
    /// (normale vers l'intérieur, comme `frustum_planes`) qui passent par les bords du
    /// rectangle, et le plan du miroir lui-même (rien derrière lui, ni dessus : un autre
    /// miroir du même plan ne se voit pas dedans).
    pub fn portal(&self, eye: Vec3) -> [Vec4; 5] {
        let e = self.reflect(eye);
        let c = self.corners();
        let mid = (c[0] + c[2]) * 0.5;
        let side = |p: Vec3, q: Vec3| {
            let mut n = (p - e).cross(q - e);
            if n.dot(mid - e) < 0.0 {
                n = -n;
            }
            n.extend(-n.dot(e))
        };
        let n = self.normal();
        [
            side(c[0], c[1]),
            side(c[1], c[2]),
            side(c[2], c[3]),
            side(c[3], c[0]),
            n.extend(-n.dot(mid) - 0.01),
        ]
    }

    /// Rectangle (x, y, largeur, hauteur) de l'image du miroir à dessiner (`size` : sa
    /// taille), d'après le rectangle des faces vu par la caméra `vp`, retourné de gauche à
    /// droite comme l'image, et pris dans `within` (rectangle de l'image où la caméra le voit,
    /// retourné lui aussi : toute l'image pour la vue de la caméra). La partie du rectangle
    /// derrière la caméra est coupée : un miroir qui passe à côté d'elle (un mur de miroirs
    /// sur le côté) ne prend plus toute l'image. `None` : rien à dessiner.
    pub fn scissor(&self, vp: Mat4, size: (u32, u32), within: [u32; 4]) -> Option<[u32; 4]> {
        const NEAR: f32 = 1e-3;
        let (w, h) = (size.0 as f32, size.1 as f32);
        let c = self.corners().map(|p| vp * p.extend(1.0));
        let mut lo = glam::Vec2::splat(f32::MAX);
        let mut hi = glam::Vec2::splat(f32::MIN);
        let mut add = |v: Vec4| {
            // Pixels, l'image du miroir étant retournée : x' = largeur − x.
            let px = glam::Vec2::new((0.5 - v.x / v.w * 0.5) * w, (0.5 - v.y / v.w * 0.5) * h);
            lo = lo.min(px);
            hi = hi.max(px);
        };
        // Coins devant la caméra, et points où les bords passent derrière elle.
        for k in 0..4 {
            let (a, b) = (c[k], c[(k + 1) % 4]);
            if a.w >= NEAR {
                add(a);
            }
            if (a.w >= NEAR) != (b.w >= NEAR) {
                add(a + (b - a) * ((NEAR - a.w) / (b.w - a.w)));
            }
        }
        if lo.x > hi.x {
            return None;
        }
        // `within` retourné comme cette image.
        let x0w = size.0.saturating_sub(within[0] + within[2]) as f32;
        let (y0w, x1w, y1w) = (
            within[1] as f32,
            x0w + within[2] as f32,
            (within[1] + within[3]) as f32,
        );
        let x0 = (lo.x - 2.0).floor().clamp(x0w, x1w);
        let y0 = (lo.y - 2.0).floor().clamp(y0w, y1w);
        let x1 = (hi.x + 2.0).ceil().clamp(x0w, x1w);
        let y1 = (hi.y + 2.0).ceil().clamp(y0w, y1w);
        (x1 > x0 && y1 > y0).then_some([x0 as u32, y0 as u32, (x1 - x0) as u32, (y1 - y0) as u32])
    }
}

/// (v190) Le miroir `r` donne-t-il sur une tranche visible de son chunk (bits de `mask`,
/// voir `occlusion`) ? Les miroirs d'une salle fermée ou de la surface, vus d'une grotte
/// fermée, ne sont pas redessinés.
pub fn in_sections(r: &MirrorRect, mask: u32) -> bool {
    if mask == u32::MAX {
        return true;
    }
    // Hauteurs de l'air devant les faces.
    let (y0, y1) = match r.axis {
        0 => (r.lo[0], r.hi[0]),
        2 => (r.lo[1], r.hi[1]),
        _ => {
            let y = if r.side > 0 { r.coord } else { r.coord - 1 };
            (y, y + 1)
        }
    };
    let s0 = (y0.max(0) / 16) as u32;
    let s1 = ((y1 - 1).max(0) / 16) as u32;
    (s0..=s1.min(31)).any(|s| mask & (1 << s) != 0)
}

/// Plans de miroir qu'on peut voir depuis `eye` : les miroirs `rects` (un rectangle par
/// miroir d'un seul tenant) vus de face, à moins de `range` blocs, qui touchent l'intérieur
/// de `cull` (champ de la caméra, ou cône d'un miroir dans lequel on les voit) ; ceux d'un
/// même plan réunis (une image par plan).
pub fn candidates<'a>(
    rects: impl Iterator<Item = &'a MirrorRect>,
    eye: Vec3,
    cull: &[Vec4],
    range: f32,
) -> Vec<Plane> {
    let mut planes: Vec<Plane> = Vec::new();
    for r in rects {
        let p = Plane::from_rect(r);
        let c = p.corners();
        if !p.faces(eye)
            || p.distance(eye) > range
            || !aabb_inside(cull, c[0].min(c[2]), c[0].max(c[2]))
        {
            continue;
        }
        match planes.iter_mut().find(|q| q.same_plane(r)) {
            Some(q) => {
                q.lo = [q.lo[0].min(p.lo[0]), q.lo[1].min(p.lo[1])];
                q.hi = [q.hi[0].max(p.hi[0]), q.hi[1].max(p.hi[1])];
            }
            None => planes.push(p),
        }
    }
    planes
}

/// Une image de miroir à redessiner à cette image.
#[derive(Clone, Debug)]
pub struct Node {
    pub plane: Plane,
    /// Image dans laquelle on voit ce miroir (`None` : vu de la caméra), et combien de
    /// reflets on traverse pour le voir (1 : vu de la caméra).
    pub parent: Option<usize>,
    pub level: u32,
    /// Vue-projection de l'image, reflet de l'œil, cône de ce que montre le miroir (plans
    /// comme `frustum_planes`), rectangle à dessiner.
    pub vp: Mat4,
    pub eye: Vec3,
    pub portal: [Vec4; 5],
    pub scissor: [u32; 4],
    /// Image (0 : niveaux impairs, lue par la vue de la caméra ; 1 : niveaux pairs) et
    /// couche.
    pub array: usize,
    pub layer: usize,
    /// Miroirs vus dans cette image qui ont la leur.
    pub children: Vec<usize>,
}

impl Node {
    /// `plane` vu par une caméra `vp` placée en `eye`, dans le rectangle `within` de son
    /// image ; `None` s'il n'y paraît pas.
    fn new(
        plane: Plane,
        parent: Option<usize>,
        level: u32,
        (vp, eye, within): (Mat4, Vec3, [u32; 4]),
        size: (u32, u32),
    ) -> Option<Self> {
        let scissor = plane.scissor(vp, size, within)?;
        Some(Self {
            plane,
            parent,
            level,
            vp: plane.view_proj(vp),
            eye: plane.reflect(eye),
            portal: plane.portal(eye),
            scissor,
            array: (level as usize + 1) % 2,
            layer: 0,
            children: Vec::new(),
        })
    }

    /// Pixels à dessiner.
    pub fn area(&self) -> f32 {
        self.scissor[2] as f32 * self.scissor[3] as f32
    }
}

/// Images de miroir à redessiner à cette image : les miroirs vus de la caméra (`roots`,
/// voir [`candidates`]), puis ceux qu'on voit dans leurs images (`inside` : les plans de
/// miroir dans le cône d'une image), et ainsi de suite ; la plus grande à l'écran d'abord,
/// [`MAX_PLANAR`] par image des miroirs, [`MAX_PLANAR`] vues dans une même image au plus.
/// Une image vient toujours après celle où on la voit.
pub fn plan(
    roots: Vec<Plane>,
    camera: (Mat4, Vec3),
    size: (u32, u32),
    mut inside: impl FnMut(&Node) -> Vec<Plane>,
) -> Vec<Node> {
    let full = [0, 0, size.0, size.1];
    let mut open: Vec<Node> = roots
        .into_iter()
        .filter_map(|p| Node::new(p, None, 1, (camera.0, camera.1, full), size))
        .collect();
    let mut nodes: Vec<Node> = Vec::new();
    let mut used = [0usize; 2];
    while let Some(best) =
        (0..open.len()).max_by(|&a, &b| open[a].area().total_cmp(&open[b].area()))
    {
        let mut n = open.swap_remove(best);
        if used[n.array] >= MAX_PLANAR {
            continue;
        }
        if let Some(p) = n.parent {
            if nodes[p].children.len() >= MAX_PLANAR {
                continue;
            }
        }
        n.layer = used[n.array];
        used[n.array] += 1;
        let at = nodes.len();
        if let Some(p) = n.parent {
            nodes[p].children.push(at);
        }
        for plane in inside(&n) {
            if let Some(c) = Node::new(plane, Some(at), n.level + 1, (n.vp, n.eye, n.scissor), size)
            {
                if c.area() >= MIN_AREA {
                    open.push(c);
                }
            }
        }
        nodes.push(n);
        if used == [MAX_PLANAR; 2] {
            break;
        }
    }
    nodes
}

/// Ce qu'il faut dessiner dans l'image d'un miroir à cette image.
#[derive(Default)]
pub(crate) struct MirrorDraw {
    /// Image des miroirs (voir [`Node`]), couche, et rectangle à dessiner (x, y, largeur,
    /// hauteur).
    pub array: usize,
    pub layer: usize,
    pub scissor: [u32; 4],
    /// Opaques : (page, première commande, nombre) dans le tampon des commandes.
    pub groups: Vec<(usize, u32, u32)>,
    /// Chunks (de l'avant vers l'arrière vu du reflet de l'œil) qui ont des faces qui
    /// reflètent ou translucides.
    pub chunks: Vec<crate::MeshKey>,
}

/// Une des deux images des miroirs : toutes ses couches (lues), et chaque couche (dessinée).
pub(crate) struct Images {
    pub view: wgpu::TextureView,
    pub layers: Vec<wgpu::TextureView>,
}

impl Images {
    fn new(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        size: (u32, u32),
        label: &str,
    ) -> Self {
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: size.0,
                height: size.1,
                depth_or_array_layers: MAX_PLANAR as u32,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = tex.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let layers = (0..MAX_PLANAR as u32)
            .map(|k| {
                tex.create_view(&wgpu::TextureViewDescriptor {
                    dimension: Some(wgpu::TextureViewDimension::D2),
                    base_array_layer: k,
                    array_layer_count: Some(1),
                    ..Default::default()
                })
            })
            .collect();
        Self { view, layers }
    }
}

/// Les images des miroirs et ce qu'il faut pour les dessiner.
pub(crate) struct Planar {
    pub size: (u32, u32),
    format: wgpu::TextureFormat,
    /// Niveaux impairs (lus par la vue de la caméra), niveaux pairs : la passe d'une image
    /// de l'une lit l'autre (une image ne peut être lue et dessinée dans la même passe).
    pub images: [Images; 2],
    pub depth: wgpu::TextureView,
    /// Uniformes du monde vus de chaque image (par image des miroirs, puis par couche).
    pub globals: [Vec<wgpu::Buffer>; 2],
    /// Groupes de liaison du monde de chaque image (refaits avec ceux du monde).
    pub groups: [Vec<wgpu::BindGroup>; 2],
    pub indirect: wgpu::Buffer,
    pub indirect_cap: u64,
    pub cmds: Vec<u8>,
    /// Passes à cette image, les plus profondes d'abord.
    pub draws: Vec<MirrorDraw>,
}

impl Planar {
    pub fn new(device: &wgpu::Device, surface: wgpu::TextureFormat, size: (u32, u32)) -> Self {
        let size = (size.0.max(1), size.1.max(1));
        let format = format(surface);
        let images = [
            Images::new(device, format, size, "miroirs (vus de la caméra)"),
            Images::new(device, format, size, "miroirs (vus dans un miroir)"),
        ];
        let depth = device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("miroirs (profondeur)"),
                size: wgpu::Extent3d {
                    width: size.0,
                    height: size.1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: crate::DEPTH_FORMAT,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            })
            .create_view(&Default::default());
        let globals = std::array::from_fn(|_| {
            (0..MAX_PLANAR)
                .map(|_| {
                    device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some("uniformes d'un miroir"),
                        size: std::mem::size_of::<crate::Globals>() as u64,
                        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                        mapped_at_creation: false,
                    })
                })
                .collect()
        });
        let indirect = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("dessins indirects des miroirs"),
            size: 20 * 256,
            usage: wgpu::BufferUsages::INDIRECT | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            size,
            format,
            images,
            depth,
            globals,
            groups: [Vec::new(), Vec::new()],
            indirect,
            indirect_cap: 256,
            cmds: Vec::new(),
            draws: Vec::new(),
        }
    }

    /// Images refaites à la taille `size` (les groupes de liaison sont à refaire).
    pub fn resize(&mut self, device: &wgpu::Device, size: (u32, u32)) {
        let mut fresh = Self::new(device, self.format, size);
        fresh.format = self.format;
        std::mem::swap(&mut fresh.globals, &mut self.globals);
        std::mem::swap(&mut fresh.indirect, &mut self.indirect);
        fresh.indirect_cap = self.indirect_cap;
        *self = fresh;
    }
}

/// Pipelines des passes de miroir, coupés au plan du miroir : blocs opaques, blocs qui
/// reflètent (un miroir dans le miroir : son image, sinon sombre ; un métal : par rayon
/// avec le raytracing) et translucides.
pub(crate) struct Pipes {
    pub block: wgpu::RenderPipeline,
    pub reflective: wgpu::RenderPipeline,
    pub translucent: wgpu::RenderPipeline,
}

/// Rebonds des rayons lancés depuis l'image d'un miroir (l'eau vue dans un miroir, avec le
/// raytracing) : pas le coût de 64 rebonds.
pub const MIRROR_BOUNCES: f32 = 8.0;

impl Pipes {
    pub fn new(
        device: &wgpu::Device,
        layout: &wgpu::PipelineLayout,
        shader: &wgpu::ShaderModule,
        format: wgpu::TextureFormat,
    ) -> Self {
        let vertex_layout = crate::world_vertex_layout();
        let vl = std::slice::from_ref(&vertex_layout);
        let greater = wgpu::CompareFunction::Greater;
        let mk = |label: &str, fs: &str, cull, depth_write, blend| {
            crate::surface_pipeline(
                device,
                layout,
                shader,
                format,
                label,
                "vs_block",
                fs,
                vl,
                cull,
                depth_write,
                greater,
                blend,
            )
        };
        Self {
            block: mk(
                "miroir : blocs",
                "fs_block_mirror",
                Some(wgpu::Face::Back),
                true,
                None,
            ),
            reflective: mk(
                "miroir : blocs qui reflètent",
                "fs_block_mirror_reflect",
                Some(wgpu::Face::Back),
                true,
                None,
            ),
            translucent: mk(
                "miroir : translucides",
                "fs_block_translucent_mirror",
                None,
                false,
                Some(wgpu::BlendState::ALPHA_BLENDING),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(axis: u8, side: i8, coord: i32, lo: [i32; 2], hi: [i32; 2]) -> MirrorRect {
        MirrorRect {
            axis,
            side,
            coord,
            lo,
            hi,
        }
    }

    /// Le reflet d'un point vu dans l'image du miroir est à l'endroit de l'écran où la
    /// caméra voit le miroir (retourné de gauche à droite) : un point devant le miroir et
    /// son image derrière lui tombent au même pixel.
    #[test]
    fn the_mirror_image_lines_up_with_the_mirror_on_screen() {
        // Miroir vertical en x = 10 (faces vers −x), caméra à x = 4 qui le regarde.
        let plane = Plane::from_rect(&rect(0, -1, 10, [60, 0], [64, 4]));
        let eye = Vec3::new(4.0, 62.0, 2.0);
        let view = Mat4::look_at_rh(eye, Vec3::new(10.0, 62.0, 2.0), Vec3::Y);
        let vp = Mat4::perspective_infinite_reverse_rh(1.2, 16.0 / 9.0, 0.05) * view;
        let mvp = plane.view_proj(vp);
        // Un point devant le miroir (côté de la caméra) et son image derrière le plan.
        for q in [
            Vec3::new(7.0, 61.0, 1.5),
            Vec3::new(5.0, 62.5, 3.0),
            Vec3::new(9.5, 63.0, 0.5),
        ] {
            let seen = vp * plane.reflect(q).extend(1.0);
            let drawn = mvp * q.extend(1.0);
            let a = seen.truncate() / seen.w;
            let b = drawn.truncate() / drawn.w;
            assert!((a.x + b.x).abs() < 1e-4, "{a} {b}");
            assert!((a.y - b.y).abs() < 1e-4, "{a} {b}");
            assert!((a.z - b.z).abs() < 1e-4, "même profondeur : {a} {b}");
        }
        // Le reflet d'un point du plan est lui-même.
        let p = Vec3::new(10.0, 62.0, 1.0);
        assert!(plane.reflect(p).abs_diff_eq(p, 1e-6));
        // Coupe : devant le miroir gardé, derrière jeté.
        let c = Vec4::from(plane.clip());
        assert!(c.dot(Vec3::new(9.0, 62.0, 2.0).extend(1.0)) > 0.0);
        assert!(c.dot(Vec3::new(11.0, 62.0, 2.0).extend(1.0)) < 0.0);
    }

    /// Un miroir n'est choisi que vu de face, dans le champ et à portée ; les faces d'un
    /// même plan venues de plusieurs chunks n'en font qu'un ; le plus grand à l'écran
    /// d'abord.
    #[test]
    fn mirrors_are_chosen_when_seen_from_the_front_and_merged_by_plane() {
        let eye = Vec3::new(4.0, 62.0, 2.0);
        let view = Mat4::look_at_rh(eye, Vec3::new(10.0, 62.0, 2.0), Vec3::Y);
        let vp = Mat4::perspective_infinite_reverse_rh(1.2, 16.0 / 9.0, 0.05) * view;
        let frustum = crate::frustum_planes(vp);
        let rects = [
            // Le même mur en deux chunks.
            rect(0, -1, 10, [60, 0], [64, 2]),
            rect(0, -1, 10, [60, 2], [64, 4]),
            // Vu de dos (faces vers +x, caméra à x = 4 < 10).
            rect(0, 1, 10, [60, 0], [64, 4]),
            // Derrière la caméra.
            rect(0, 1, -20, [60, 0], [64, 4]),
            // Petit miroir au fond, plus loin.
            rect(0, -1, 30, [61, 1], [62, 2]),
            // Trop loin.
            rect(0, -1, 300, [0, 0], [100, 100]),
        ];
        let seen = candidates(rects.iter(), eye, &frustum, 64.0);
        assert_eq!(seen.len(), 2, "{seen:?}");
        let nodes = plan(seen, (vp, eye), (1280, 720), |_| Vec::new());
        assert_eq!(nodes.len(), 2, "{nodes:?}");
        assert_eq!(nodes[0].plane.coord, 10.0);
        assert_eq!(
            (nodes[0].plane.lo, nodes[0].plane.hi),
            ([60.0, 0.0], [64.0, 4.0])
        );
        assert_eq!(nodes[1].plane.coord, 30.0);
        assert!(nodes[0].area() > nodes[1].area());
        // Vus de la caméra : première image des miroirs, une couche chacun.
        assert_eq!([nodes[0].array, nodes[1].array], [0, 0]);
        assert_eq!([nodes[0].layer, nodes[1].layer], [0, 1]);
    }

    /// (v190) Un mur de miroirs qui passe à côté de la caméra (un coin derrière elle) ne
    /// prend que sa part de l'image, pas toute l'image comme avant.
    #[test]
    fn a_mirror_wall_beside_the_camera_takes_only_its_part_of_the_image() {
        // Caméra en (8,5 ; 52,6 ; 6,5) qui regarde vers +z ; mur de miroirs en x = 12, faces
        // vers −x, de z = 2 (derrière la caméra) à z = 11.
        let eye = Vec3::new(8.5, 52.6, 6.5);
        let view = Mat4::look_to_rh(eye, Vec3::Z, Vec3::Y);
        let vp = Mat4::perspective_infinite_reverse_rh(75f32.to_radians(), 16.0 / 9.0, 0.05) * view;
        let size = (1280, 720);
        let wall = Plane::from_rect(&rect(0, -1, 12, [51, 2], [55, 11]));
        let s = wall.scissor(vp, size, [0, 0, 1280, 720]).unwrap();
        // Vu à gauche de l'écran (x croît vers −x du monde en regardant +z), donc à droite
        // dans l'image retournée, et seulement sur sa part.
        assert!(s[0] > 900, "{s:?}");
        assert_eq!(s[0] + s[2], 1280, "{s:?}");
        // Dans le rectangle d'un parent : pris dedans (retourné).
        let s = wall.scissor(vp, size, [0, 0, 200, 720]).unwrap();
        assert!(s[0] >= 1080 && s[0] + s[2] <= 1280, "{s:?}");
        // Entièrement derrière la caméra : rien.
        let behind = Plane::from_rect(&rect(0, -1, 12, [51, -9], [55, 2]));
        assert!(behind.scissor(vp, size, [0, 0, 1280, 720]).is_none());
    }

    /// (v190) Deux miroirs face à face : un couloir de reflets. Chaque miroir vu dans
    /// l'autre a son image, de plus en plus petite, dans l'autre image des miroirs que celle
    /// où on le voit ; huit images au plus, et l'image d'un miroir vu dans un autre tombe
    /// au bon endroit de l'écran.
    #[test]
    fn two_facing_mirrors_make_a_corridor_of_images() {
        // Miroirs en x = 3 (faces vers +x) et x = 8 (faces vers −x), de z = 2 à 11 ; caméra
        // entre les deux, qui regarde le premier en biais.
        let rects = [
            rect(0, 1, 3, [51, 2], [55, 11]),
            rect(0, -1, 8, [51, 2], [55, 11]),
        ];
        let eye = Vec3::new(6.0, 52.6, 6.5);
        let view = Mat4::look_to_rh(eye, Vec3::new(-1.0, 0.0, 0.3), Vec3::Y);
        let vp = Mat4::perspective_infinite_reverse_rh(75f32.to_radians(), 16.0 / 9.0, 0.05) * view;
        let size = (1280, 720);
        let roots = candidates(rects.iter(), eye, &crate::frustum_planes(vp), RANGE);
        assert_eq!(
            roots.len(),
            1,
            "seul le miroir en x = 3 est dans le champ : {roots:?}"
        );
        assert_eq!(roots[0].coord, 3.0);
        let nodes = plan(roots, (vp, eye), size, |n| {
            candidates(rects.iter(), n.eye, &n.portal, RANGE)
        });
        assert_eq!(nodes.len(), 8, "{nodes:?}");
        for (k, n) in nodes.iter().enumerate() {
            // Une image de plus à chaque reflet, l'autre miroir à chaque fois, dans l'autre
            // image des miroirs que celle où on le voit.
            assert_eq!(n.level as usize, k + 1);
            assert_eq!(n.plane.coord, if k % 2 == 0 { 3.0 } else { 8.0 });
            assert_eq!(n.array, k % 2);
            assert_eq!(n.layer, k / 2);
            if k > 0 {
                assert_eq!(n.parent, Some(k - 1));
                assert_eq!(nodes[k - 1].children, vec![k]);
                assert!(
                    n.area() < nodes[k - 1].area(),
                    "{k} : de plus en plus petite"
                );
            }
        }
        // Un point entre les miroirs vu à travers les deux premiers (reflété en x = 8 puis en
        // x = 3) : l'image du second le dessine là où l'écran le montre.
        let (a, b) = (&nodes[0], &nodes[1]);
        let q = Vec3::new(5.0, 52.0, 8.0);
        let seen = vp * a.plane.reflect(b.plane.reflect(q)).extend(1.0);
        let drawn = b.vp * q.extend(1.0);
        let (sx, sy) = (seen.x / seen.w, seen.y / seen.w);
        assert!((sx - drawn.x / drawn.w).abs() < 1e-4 && (sy - drawn.y / drawn.w).abs() < 1e-4);
        // Dans le rectangle dessiné de cette image (pixels).
        let px = ((sx * 0.5 + 0.5) * 1280.0, (0.5 - sy * 0.5) * 720.0);
        let s = b.scissor;
        assert!(
            px.0 >= s[0] as f32
                && px.0 <= (s[0] + s[2]) as f32
                && px.1 >= s[1] as f32
                && px.1 <= (s[1] + s[3]) as f32,
            "{px:?} hors de {s:?}"
        );
    }

    /// (v190) La salle des miroirs de la démo : un miroir en face, deux murs de miroirs
    /// sur les côtés (le bout de chacun regarde aussi vers la caméra, sur le plan du miroir
    /// d'en face). Les trois miroirs ont leur image, les murs se voient dans celui d'en
    /// face, et le bout des murs n'est jamais pris pour un miroir vu dans celui d'en face.
    /// Tourné vers un mur, on voit l'autre dedans, et ainsi de suite.
    #[test]
    fn the_demo_mirror_room() {
        let rects = [
            // En face (z = 11, faces vers −z).
            rect(2, -1, 11, [5, 51], [11, 54]),
            // Murs des côtés, et leur bout (z = 11, faces vers +z).
            rect(0, 1, 3, [51, 2], [55, 11]),
            rect(0, -1, 12, [51, 2], [55, 11]),
            rect(2, 1, 11, [2, 51], [3, 55]),
            rect(2, 1, 11, [12, 51], [13, 55]),
        ];
        let eye = Vec3::new(8.5, 52.6, 6.5);
        let view = Mat4::look_to_rh(eye, Vec3::new(0.0, -0.12, 1.0), Vec3::Y);
        let vp = Mat4::perspective_infinite_reverse_rh(75f32.to_radians(), 16.0 / 9.0, 0.05) * view;
        let roots = candidates(rects.iter(), eye, &crate::frustum_planes(vp), RANGE);
        assert_eq!(roots.len(), 3, "{roots:?}");
        let nodes = plan(roots, (vp, eye), (1280, 720), |n| {
            candidates(rects.iter(), n.eye, &n.portal, RANGE)
        });
        assert!(
            nodes
                .iter()
                .all(|n| !(n.plane.axis == 2 && n.plane.side > 0.0)),
            "le bout d'un mur pris pour un miroir : {nodes:?}"
        );
        assert_eq!(
            nodes.iter().filter(|n| n.level == 1).count(),
            3,
            "{nodes:?}"
        );
        assert!(nodes.len() <= 2 * MAX_PLANAR);
        // Les deux murs vus dans le miroir d'en face.
        let front = nodes
            .iter()
            .position(|n| n.level == 1 && n.plane.axis == 2)
            .unwrap();
        let mut walls: Vec<f32> = nodes[front]
            .children
            .iter()
            .map(|&c| nodes[c].plane.coord)
            .collect();
        walls.sort_by(f32::total_cmp);
        assert_eq!(walls, vec![3.0, 12.0], "{nodes:?}");
        // Tourné vers le mur en x = 3 : l'autre dedans, le premier dans l'autre...
        let view = Mat4::look_to_rh(eye, Vec3::new(-1.0, -0.09, 0.0), Vec3::Y);
        let vp = Mat4::perspective_infinite_reverse_rh(75f32.to_radians(), 16.0 / 9.0, 0.05) * view;
        let roots = candidates(rects.iter(), eye, &crate::frustum_planes(vp), RANGE);
        let nodes = plan(roots, (vp, eye), (1280, 720), |n| {
            candidates(rects.iter(), n.eye, &n.portal, RANGE)
        });
        let mut k = nodes
            .iter()
            .position(|n| n.level == 1 && n.plane.coord == 3.0)
            .unwrap();
        for depth in 2..=3 {
            let want = if depth % 2 == 0 { 12.0 } else { 3.0 };
            k = *nodes[k]
                .children
                .iter()
                .find(|&&c| nodes[c].plane.axis == 0 && nodes[c].plane.coord == want)
                .unwrap_or_else(|| panic!("pas de mur en {want} au reflet {depth} : {nodes:?}"));
        }
    }

    /// (v190) Un miroir dans une tranche cachée (salle fermée, surface vue d'une grotte
    /// fermée) n'est pas redessiné ; plafond et sol : la tranche de l'air devant eux.
    #[test]
    fn mirrors_in_hidden_sections_are_skipped() {
        let wall = rect(0, 1, 3, [51, 2], [55, 11]);
        assert!(in_sections(&wall, u32::MAX));
        assert!(in_sections(&wall, 1 << 3));
        assert!(!in_sections(&wall, 1 << 2 | 1 << 4));
        // Mur à cheval sur deux tranches (y de 46 à 50).
        let tall = rect(2, -1, 11, [5, 46], [11, 50]);
        assert!(in_sections(&tall, 1 << 2) && in_sections(&tall, 1 << 3));
        // Sol miroir en y = 48 (faces vers le haut) : l'air au-dessus est dans la tranche 3 ;
        // plafond en y = 48 (faces vers le bas) : l'air en dessous, tranche 2.
        let floor = rect(1, 1, 48, [0, 0], [4, 4]);
        let ceiling = rect(1, -1, 48, [0, 0], [4, 4]);
        assert!(in_sections(&floor, 1 << 3) && !in_sections(&floor, 1 << 2));
        assert!(in_sections(&ceiling, 1 << 2) && !in_sections(&ceiling, 1 << 3));
    }

    /// Le cône d'un miroir garde ce qu'on voit à travers lui, et rien de ce qui est
    /// derrière son plan ni à côté.
    #[test]
    fn the_portal_keeps_only_what_the_mirror_shows() {
        // Miroir en z = 0 (x et y de 0 à 4), faces vers +z ; caméra devant, en z = 5.
        let plane = Plane::from_rect(&rect(2, 1, 0, [0, 0], [4, 4]));
        let eye = Vec3::new(2.0, 2.0, 5.0);
        let planes = plane.portal(eye);
        let inside = |p: Vec3| planes.iter().all(|pl| pl.truncate().dot(p) + pl.w >= -1e-4);
        // Devant le miroir, sur l'axe de vue : vu.
        assert!(inside(Vec3::new(2.0, 2.0, 3.0)));
        // Derrière le miroir : jamais.
        assert!(!inside(Vec3::new(2.0, 2.0, -1.0)));
        // Très à côté (hors du cône qui passe par le rectangle).
        assert!(!inside(Vec3::new(40.0, 2.0, 3.0)));
        // Écran : le rectangle du miroir, retourné.
        let view = Mat4::look_at_rh(eye, Vec3::new(2.0, 2.0, 0.0), Vec3::Y);
        let vp = Mat4::perspective_infinite_reverse_rh(1.2, 1.0, 0.05) * view;
        let s = plane.scissor(vp, (1000, 1000), [0, 0, 1000, 1000]).unwrap();
        assert!(s[0] > 0 && s[0] + s[2] < 1000 && s[2] > 100, "{s:?}");
    }
}
