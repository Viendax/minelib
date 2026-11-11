//! Maillage d'un chunk : une quad par face visible (voisin non opaque),
//! occlusion ambiante par sommet, diagonale de la quad choisie selon l'AO
//! pour éviter les artefacts d'interpolation.

use std::sync::Arc;

use bytemuck::{Pod, Zeroable};

use std::collections::VecDeque;

use crate::registry::{light_cover, post_arms, PostKind, Shape};
use crate::world::{Light, LIGHT_RADIUS};
use crate::{BlockId, Chunk, ChunkPos, World, CHUNK_HEIGHT, CHUNK_SIZE};

/// Sommet compact envoyé au GPU.
/// `data` : bits 0..4 = ciel, 4..8 = reflet, 8..11 = face (0..6), 11..13 = AO (0 = sombre,
/// 3 = clair), bit 13 = métal (reflet court), bits 14..32 = couleur RVB 6:6:6 :
/// - blocs : lumière colorée reçue des torches (intensité par canal) ;
/// - flamme ([`BlockId::FLAME`]) : couleur émise.
///
/// `tex` : bits 0..12 = couche du tableau de textures, bit 12 = émissif (flamme).
#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable, PartialEq)]
pub struct Vertex {
    pub pos: [f32; 3],
    pub data: u32,
    pub tex: u32,
}

/// Bit « émissif » du champ `tex`.
pub const TEX_EMISSIVE: u32 = 1 << 12;
/// Coordonnées de texture fournies par le sommet (bits 16..23 = u, 24..31 = v, en 1/128)
/// au lieu d'être tirées de la position monde : faces penchées (torche murale).
pub const TEX_LOCAL_UV: u32 = 1 << 13;
/// Texture toujours lue en pleine résolution (pas de mipmap) : modèles fins découpés dans
/// une texture surtout transparente (torche). Avec les mipmaps, de loin, la moyenne des
/// pixels devient transparente et le modèle disparaît, sauf quelques bords.
pub const TEX_SHARP: u32 = 1 << 14;

/// Couche de texture d'une face (0..6, 6 = plante en croix) d'un bloc.
pub fn face_tex(id: BlockId, face: u32) -> u32 {
    let t = id.def().tex;
    (match face {
        2 => t[0],
        3 => t[2],
        _ => t[1],
    }) as u32
}

impl Vertex {
    /// Lumière du ciel reçue (0 = sous terre, 15 = ciel ouvert).
    pub fn sky(&self) -> u32 {
        self.data & 0xf
    }
    pub fn face(&self) -> u32 {
        (self.data >> 8) & 0x7
    }
    pub fn ao(&self) -> u32 {
        (self.data >> 11) & 0x3
    }

    /// Lumière colorée (ou couleur) portée par le sommet, 0..63 par canal.
    pub fn rgb6(&self) -> [u32; 3] {
        [
            (self.data >> 14) & 63,
            (self.data >> 20) & 63,
            (self.data >> 26) & 63,
        ]
    }

    /// `light` : lumière colorée RVB (0..255) + lumière du ciel (0..15).
    fn block_data(face: u32, ao: u32, light: [u8; 4]) -> u32 {
        let [r, g, b] = [light[0], light[1], light[2]].map(|c| (c >> 2) as u32);
        (light[3] as u32 & 15) | (face << 8) | (ao << 11) | (r << 14) | (g << 20) | (b << 26)
    }
}

/// Bit 13 des données de sommet : métal, reflet court (4 blocs) et flou.
pub const REFLECT_SHORT: u32 = 1 << 13;

/// Bits 4-7 des données de sommet : reflet du bloc (0 mat … 15 miroir) ; bit 13 : métal.
fn reflect_bits(id: BlockId) -> u32 {
    let d = id.def();
    (d.reflect as u32 & 15) << 4 | if d.metal { REFLECT_SHORT } else { 0 }
}

#[derive(Default, Debug)]
pub struct MeshData {
    pub vertices: Vec<Vertex>,
    /// Triangles opaques (et découpés : feuilles, verre clair).
    pub indices: Vec<u32>,
    /// Triangles opaques qui reflètent (miroir, métaux) : dessinés à part avec le shader des
    /// reflets, pour que les autres blocs gardent un shader léger (plus de FPS).
    pub reflective: Vec<u32>,
    /// Triangles semi-transparents (eau, glace, verre teinté), dessinés après les opaques.
    pub translucent: Vec<u32>,
    /// `indices` triés par direction de face : nombre d'indices tournés vers +X, −X, +Y, −Y,
    /// +Z, −Z, puis les autres (plantes en croix, torches penchées). Le rendu ne dessine que
    /// les directions qui peuvent faire face à la caméra.
    pub face_counts: [u32; 7],
    /// Dans chaque direction, les indices sont rangés par tranche de 16 blocs de haut :
    /// `section_counts[direction][tranche]` (somme = `face_counts[direction]`).
    pub section_counts: [[u32; SECTIONS]; 7],
    /// Passages par l'air de chaque tranche (voir `section_connectivity`) ; `None` : pas de
    /// découpage, tout est toujours dessiné.
    pub connectivity: Option<[u64; SECTIONS]>,
}

/// Tranches de 16 blocs de haut dans une colonne de chunk.
pub const SECTIONS: usize = CHUNK_HEIGHT / 16;

/// Bit « la face `a` communique avec la face `b` » (faces dans l'ordre de `FACES`).
pub const fn link(a: usize, b: usize) -> u64 {
    1 << (a * 6 + b)
}

/// Tous les passages (tranche vide).
pub const ALL_LINKS: u64 = (1 << 36) - 1;

/// Passages par l'air des 16 tranches d'un chunk : pour chaque région d'air (blocs non
/// pleins) connexe d'une tranche, les faces de la tranche qu'elle touche communiquent entre
/// elles. Une grotte fermée ne touche aucune face : rien ne la relie au reste.
pub fn section_connectivity(chunk: &Chunk) -> [u64; SECTIONS] {
    let mut out = [0u64; SECTIONS];
    let top = chunk.top();
    for (s, o) in out.iter_mut().enumerate() {
        let y0 = s * 16;
        // Au-dessus du plus haut bloc ou sous le plus bas : que de l'air.
        if y0 >= top || y0 + 16 <= chunk.bottom() {
            *o = ALL_LINKS;
            continue;
        }
        let idx = |x: usize, y: usize, z: usize| (y * 16 + z) * 16 + x;
        let mut solid = [false; 4096];
        let mut any_air = false;
        for y in 0..16 {
            for z in 0..16 {
                for x in 0..16 {
                    let full = chunk.get(x, y0 + y, z).is_opaque();
                    solid[idx(x, y, z)] = full;
                    any_air |= !full;
                }
            }
        }
        if !any_air {
            continue;
        }
        // Aucun bloc plein (air, plantes, torches…) : toutes les faces communiquent.
        if !solid.iter().any(|&f| f) {
            *o = ALL_LINKS;
            continue;
        }
        let mut seen = [false; 4096];
        let mut stack = Vec::new();
        for start in 0..4096 {
            if solid[start] || seen[start] {
                continue;
            }
            seen[start] = true;
            stack.push(start);
            let mut touched = 0u8;
            while let Some(i) = stack.pop() {
                let (x, z, y) = (i % 16, (i / 16) % 16, i / 256);
                touched |= ((x == 15) as u8)
                    | ((x == 0) as u8) << 1
                    | ((y == 15) as u8) << 2
                    | ((y == 0) as u8) << 3
                    | ((z == 15) as u8) << 4
                    | ((z == 0) as u8) << 5;
                let mut go = |j: usize| {
                    if !solid[j] && !seen[j] {
                        seen[j] = true;
                        stack.push(j);
                    }
                };
                if x < 15 {
                    go(i + 1);
                }
                if x > 0 {
                    go(i - 1);
                }
                if z < 15 {
                    go(i + 16);
                }
                if z > 0 {
                    go(i - 16);
                }
                if y < 15 {
                    go(i + 256);
                }
                if y > 0 {
                    go(i - 256);
                }
            }
            for a in 0..6 {
                for b in 0..6 {
                    if touched & (1 << a) != 0 && touched & (1 << b) != 0 {
                        *o |= link(a, b);
                    }
                }
            }
        }
    }
    out
}

impl MeshData {
    /// Range les triangles opaques par direction de face (voir `face_counts`), d'après leur
    /// normale géométrique : seules les faces parfaitement alignées sur un axe sont classées.
    pub fn sort_by_face(&mut self) {
        let mut buckets: Vec<Vec<u32>> = vec![Vec::new(); 7 * SECTIONS];
        for tri in self.indices.chunks_exact(3) {
            let v = |i: u32| self.vertices[i as usize];
            let (a, b, c) = (v(tri[0]).pos, v(tri[1]).pos, v(tri[2]).pos);
            let (u, w) = (
                [b[0] - a[0], b[1] - a[1], b[2] - a[2]],
                [c[0] - a[0], c[1] - a[1], c[2] - a[2]],
            );
            let n = [
                u[1] * w[2] - u[2] * w[1],
                u[2] * w[0] - u[0] * w[2],
                u[0] * w[1] - u[1] * w[0],
            ];
            // Face du sommet (0 +X, 1 −X, 2 +Y, 3 −Y, 4 +Z, 5 −Z), gardée seulement si le
            // triangle est vraiment perpendiculaire à cet axe (pas les torches penchées).
            let face = v(tri[0]).face() as usize;
            let axis = face / 2;
            // Le sens compte aussi : une torche murale tournée d'un demi-tour (mur −Z) garde
            // des faces perpendiculaires à X, mais une face « +X » y regarde vers −X ; rangée
            // en +X, elle serait cachée exactement quand on devrait la voir.
            // (Face 6 = plantes en croix : pas d'axe, toujours dessinées.)
            let aligned = face < 6
                && (if face.is_multiple_of(2) {
                    n[axis] > 0.0
                } else {
                    n[axis] < 0.0
                })
                && (0..3).all(|k| k == axis || n[k] == 0.0);
            let k = if aligned { face } else { 6 };
            // Tranche du côté d'où la face se voit : juste au-dessus d'une face du dessus,
            // juste en dessous d'une face du dessous. Un petit décalage seulement : une face
            // à mi-hauteur (dalle, marche d'escalier en y = 79,5) appartient à la case 79,
            // pas à la tranche suivante (sinon, l'œil entre 79,5 et 80 ne la voit plus).
            let mut cy = (a[1] + b[1] + c[1]) / 3.0;
            if aligned && axis == 1 {
                cy += if face == 2 { 0.01 } else { -0.01 };
            }
            let sec = ((cy.floor() as i32).div_euclid(16)).clamp(0, SECTIONS as i32 - 1) as usize;
            buckets[k * SECTIONS + sec].extend_from_slice(tri);
        }
        self.indices.clear();
        self.face_counts = [0; 7];
        for (i, b) in buckets.iter().enumerate() {
            let (k, sec) = (i / SECTIONS, i % SECTIONS);
            self.section_counts[k][sec] = b.len() as u32;
            self.face_counts[k] += b.len() as u32;
            self.indices.extend_from_slice(b);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.indices.is_empty() && self.reflective.is_empty() && self.translucent.is_empty()
    }
}

/// Les 6 faces : (normale, axe u, axe v) avec u × v = normale,
/// ce qui donne des quads dans le sens trigonométrique vus de l'extérieur.
/// Ordre : +X, -X, +Y, -Y, +Z, -Z.
pub const FACES: [([i32; 3], [i32; 3], [i32; 3]); 6] = [
    ([1, 0, 0], [0, 1, 0], [0, 0, 1]),
    ([-1, 0, 0], [0, 0, 1], [0, 1, 0]),
    ([0, 1, 0], [0, 0, 1], [1, 0, 0]),
    ([0, -1, 0], [1, 0, 0], [0, 0, 1]),
    ([0, 0, 1], [1, 0, 0], [0, 1, 0]),
    ([0, 0, -1], [0, 1, 0], [1, 0, 0]),
];

const P: usize = CHUNK_SIZE + 2; // largeur avec une bordure d'un bloc

/// Copie locale du chunk + une bordure prise chez les 8 voisins :
/// les lectures de voisinage deviennent de simples accès tableau.
struct Padded {
    blocks: Vec<BlockId>,
    height: usize, // nombre de couches (y de -1 à height-2)
}

/// Un chunk et ses 8 voisins, partagés (`Arc`) : de quoi mailler le chunk central
/// sur n'importe quel thread sans accéder au [`World`].
#[derive(Clone)]
pub struct Neighborhood {
    pub center: ChunkPos,
    chunks: [Option<Arc<Chunk>>; 9],
    /// Sources de lumière qui peuvent atteindre le chunk central.
    pub lights: Vec<Light>,
}

impl Neighborhood {
    /// `None` si le chunk central n'est pas chargé.
    pub fn from_world(world: &World, pos: ChunkPos) -> Option<Self> {
        world.chunk_arc(pos)?;
        let chunks = std::array::from_fn(|i| {
            let (dx, dz) = (i as i32 % 3 - 1, i as i32 / 3 - 1);
            world.chunk_arc(ChunkPos {
                x: pos.x + dx,
                z: pos.z + dz,
            })
        });
        Some(Self {
            center: pos,
            chunks,
            lights: world.lights_near(pos),
        })
    }

    fn get(&self, dx: i32, dz: i32) -> Option<&Chunk> {
        self.chunks[((dz + 1) * 3 + dx + 1) as usize].as_deref()
    }

    /// Bloc en coordonnées monde, si le chunk fait partie du voisinage.
    fn block_world(&self, x: i32, y: i32, z: i32) -> BlockId {
        if !(0..CHUNK_HEIGHT as i32).contains(&y) {
            return if y < 0 {
                BlockId::BEDROCK
            } else {
                BlockId::AIR
            };
        }
        let c = ChunkPos::containing(x, z);
        let (dx, dz) = (c.x - self.center.x, c.z - self.center.z);
        if dx.abs() > 1 || dz.abs() > 1 {
            return BlockId::AIR;
        }
        match self.get(dx, dz) {
            Some(ch) => ch.get(
                x.rem_euclid(CHUNK_SIZE as i32) as usize,
                y as usize,
                z.rem_euclid(CHUNK_SIZE as i32) as usize,
            ),
            None => BlockId::AIR,
        }
    }
}

/// Lumière colorée autour d'un chunk : propagation en largeur depuis chaque source,
/// atténuée de 25 % par bloc, arrêtée par les blocs opaques. Les trois canaux
/// gardent le maximum reçu : deux torches de couleurs différentes se mélangent.
struct LightGrid {
    x0: i32,
    y0: i32,
    z0: i32,
    w: usize,
    h: usize,
    data: Vec<[u8; 3]>,
}

impl LightGrid {
    const MARGIN: i32 = LIGHT_RADIUS + 1;

    fn build(n: &Neighborhood) -> Option<Self> {
        if n.lights.is_empty() {
            return None;
        }
        let (ox, oz) = n.center.origin();
        let m = Self::MARGIN;
        let ymin = n.lights.iter().map(|l| l.pos[1]).min()? - m;
        let ymax = n.lights.iter().map(|l| l.pos[1]).max()? + m;
        let (y0, y1) = (ymin.max(0), ymax.min(CHUNK_HEIGHT as i32 - 1));
        let w = (CHUNK_SIZE as i32 + 2 * m) as usize;
        let h = (y1 - y0 + 1).max(1) as usize;
        let mut g = Self {
            x0: ox - m,
            y0,
            z0: oz - m,
            w,
            h,
            data: vec![[0; 3]; w * w * h],
        };

        let mut queue = VecDeque::new();
        let lit_from: Vec<[i32; 3]> = n.lights.iter().map(|l| l.pos).collect();
        for l in &n.lights {
            if let Some(i) = g.index(l.pos[0], l.pos[1], l.pos[2]) {
                let cell = &mut g.data[i];
                *cell = std::array::from_fn(|k| cell[k].max(l.rgb[k]));
                queue.push_back(l.pos);
            }
        }
        while let Some(p) = queue.pop_front() {
            let Some(i) = g.index(p[0], p[1], p[2]) else {
                continue;
            };
            let v = g.data[i];
            let next = v.map(|c| (c as u16 * 3 / 4) as u8);
            if next.iter().all(|&c| c < 4) {
                continue;
            }
            // Couverture des faces de la case de départ (dalle, escalier) ; la source
            // elle-même rayonne de tous côtés.
            let here = match light_cover(n.block_world(p[0], p[1], p[2])) {
                [0, 0, 0, 0, 0, 0] => [0; 6],
                _ if lit_from.contains(&p) => [0; 6],
                c => c,
            };
            for (face, d) in [
                [1, 0, 0],
                [-1, 0, 0],
                [0, 1, 0],
                [0, -1, 0],
                [0, 0, 1],
                [0, 0, -1],
            ]
            .into_iter()
            .enumerate()
            {
                if here[face] == u16::MAX {
                    continue;
                }
                let q = add(p, d);
                let Some(j) = g.index(q[0], q[1], q[2]) else {
                    continue;
                };
                let cur = g.data[j];
                if (0..3).all(|k| next[k] <= cur[k]) {
                    continue;
                }
                // Passage fermé si les deux faces en contact (`face` ici, `face ^ 1` en face)
                // couvrent ensemble toute la surface : bloc plein, ou dalle du bas contre
                // dalle du haut…
                let there = light_cover(n.block_world(q[0], q[1], q[2]))[face ^ 1];
                if here[face] | there == u16::MAX {
                    continue;
                }
                g.data[j] = std::array::from_fn(|k| cur[k].max(next[k]));
                queue.push_back(q);
            }
        }
        Some(g)
    }

    #[inline]
    fn index(&self, x: i32, y: i32, z: i32) -> Option<usize> {
        let (lx, ly, lz) = (x - self.x0, y - self.y0, z - self.z0);
        if lx < 0
            || lz < 0
            || ly < 0
            || lx >= self.w as i32
            || lz >= self.w as i32
            || ly >= self.h as i32
        {
            return None;
        }
        Some((ly as usize * self.w + lz as usize) * self.w + lx as usize)
    }

    #[inline]
    fn get(&self, x: i32, y: i32, z: i32) -> [u8; 3] {
        self.index(x, y, z).map_or([0; 3], |i| self.data[i])
    }
}

/// Pavé `min`..`max` d'un bloc, avec une transformation des sommets (torche murale penchée).
#[allow(clippy::too_many_arguments)]
fn emit_box_xf(
    out: &mut MeshData,
    min: [f32; 3],
    max: [f32; 3],
    id: BlockId,
    light: [[u8; 4]; 6],
    extra: u32,
    skip: impl Fn(usize) -> bool,
    xf: &dyn Fn([f32; 3]) -> [f32; 3],
    local_uv: bool,
) {
    emit_box_t(
        out,
        min,
        max,
        id,
        &|f| face_tex(id, f),
        light,
        extra,
        skip,
        xf,
        local_uv,
    );
}

/// Comme `emit_box_xf`, avec la texture de chaque face donnée par `tex_of`.
#[allow(clippy::too_many_arguments)]
fn emit_box_t(
    out: &mut MeshData,
    min: [f32; 3],
    max: [f32; 3],
    id: BlockId,
    tex_of: &dyn Fn(u32) -> u32,
    light: [[u8; 4]; 6],
    extra: u32,
    skip: impl Fn(usize) -> bool,
    xf: &dyn Fn([f32; 3]) -> [f32; 3],
    local_uv: bool,
) {
    let e = [max[0] - min[0], max[1] - min[1], max[2] - min[2]];
    let cell = min.map(|v| v.floor());
    let axis = |a: [i32; 3]| a.iter().position(|&c| c != 0).unwrap_or(0);
    for (face, &(n, u, v)) in FACES.iter().enumerate() {
        if skip(face) {
            continue;
        }
        let base: [f32; 3] = std::array::from_fn(|i| min[i] + if n[i] > 0 { e[i] } else { 0.0 });
        let (du, dv) = (e[axis(u)], e[axis(v)]);
        let start = out.vertices.len() as u32;
        let tex = tex_of(face as u32) | extra;
        for (a, c) in [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)] {
            let p: [f32; 3] =
                std::array::from_fn(|i| base[i] + a * du * u[i] as f32 + c * dv * v[i] as f32);
            let mut tex = tex;
            if local_uv {
                // Mêmes coordonnées que le shader tire de la position d'un bloc droit.
                let l: [f32; 3] = std::array::from_fn(|i| p[i] - cell[i]);
                let uv = match axis(n) {
                    0 => [l[2], 1.0 - l[1]],
                    1 => [l[0], l[2]],
                    _ => [l[0], 1.0 - l[1]],
                };
                let q = uv.map(|t| (t.clamp(0.0, 1.99) * 128.0).round() as u32);
                tex |= TEX_LOCAL_UV | q[0] << 16 | q[1] << 24;
            }
            out.vertices.push(Vertex {
                pos: xf(p),
                data: Vertex::block_data(face as u32, 3, light[face]) | reflect_bits(id),
                tex,
            });
        }
        out.indices
            .extend_from_slice(&[0, 1, 2, 2, 3, 0].map(|i| start + i));
    }
}

/// Repère d'une torche murale : penchée de 22,5° en s'écartant du mur, pied contre le mur,
/// puis tournée vers le mur `d` (0 +X, 1 −X, 4 +Z, 5 −Z). Coordonnées locales 0..1.
pub fn wall_torch_xf(d: u8, p: [f32; 3]) -> [f32; 3] {
    let (s, c) = 22.5f32.to_radians().sin_cos();
    let dz = p[2] - 0.5;
    let y = p[1] * c + dz * s;
    let z = -p[1] * s + dz * c;
    let (x, y, z) = (p[0], y + 0.2, z + 0.9375);
    let (x, z) = match d {
        0 => (z, 1.0 - x),
        1 => (1.0 - z, x),
        5 => (1.0 - x, 1.0 - z),
        _ => (x, z),
    };
    [x, y, z]
}

/// Deux plans diagonaux visibles des deux côtés (plantes).
fn emit_cross(out: &mut MeshData, o: [f32; 3], id: BlockId, light: [u8; 4]) {
    let tex = face_tex(id, 6);
    let (y0, y1) = (o[1], o[1] + 1.0);
    for (a, b) in [([0.1, 0.1], [0.9, 0.9]), ([0.1, 0.9], [0.9, 0.1])] {
        let p = |c: [f32; 2], y: f32| [o[0] + c[0], y, o[2] + c[1]];
        let quad = [p(a, y0), p(b, y0), p(b, y1), p(a, y1)];
        for order in [[0u32, 1, 2, 2, 3, 0], [0, 2, 1, 2, 0, 3]] {
            let start = out.vertices.len() as u32;
            for q in quad {
                out.vertices.push(Vertex {
                    pos: q,
                    data: Vertex::block_data(6, 3, light),
                    tex,
                });
            }
            out.indices.extend_from_slice(&order.map(|i| start + i));
        }
    }
}

/// Les voisins horizontaux d'un poteau auxquels il se relie.
fn post_connects(kind: crate::registry::PostKind, neighbor: BlockId) -> bool {
    use crate::registry::Shape;
    let d = neighbor.def();
    d.full || matches!(d.shape, Shape::Post(k) if k == kind)
}

/// Lumière du ciel autour d'un chunk (niveaux 0..15) : 15 sous le ciel ouvert, puis −1
/// par bloc en s'éloignant des zones ouvertes (surplombs, entrées de grottes) ; les grottes
/// fermées restent à 0. Les blocs pleins l'arrêtent.
struct SkyGrid {
    x0: i32,
    z0: i32,
    w: usize,
    /// Couche la plus basse calculée : en dessous, rien ne peut éclairer une face du chunk.
    y0: usize,
    h: usize,
    data: Vec<u8>,
}

impl SkyGrid {
    const MARGIN: i32 = 15;

    fn build(n: &Neighborhood) -> Self {
        let (ox, oz) = n.center.origin();
        let m = Self::MARGIN;
        let w = (CHUNK_SIZE as i32 + 2 * m) as usize;
        let (mut top, mut bottom) = (0, CHUNK_HEIGHT);
        for dz in -1..=1 {
            for dx in -1..=1 {
                if let Some(c) = n.get(dx, dz) {
                    top = top.max(c.top());
                    bottom = bottom.min(c.bottom());
                }
            }
        }
        let h_abs = (top + 2).min(CHUNK_HEIGHT);
        // Tranches vides sous le plus bas bloc : la lumière du ciel ne va pas à plus de
        // 15 blocs, donc rien sous `bottom − 16` ne peut changer l'éclairage d'une face.
        // Une carte flottante (Dust II, îles) n'est plus calculée jusqu'au fond du monde.
        let y0 = bottom.saturating_sub(16).min(h_abs);
        let h = h_abs - y0;
        let (x0, z0) = (ox - m, oz - m);
        let idx = |x: usize, y: usize, z: usize| (y * w + z) * w + x;
        let mut opaque = vec![false; w * w * h];
        let mut data = vec![0u8; w * w * h];
        for z in 0..w {
            for x in 0..w {
                // Colonne lue directement dans son chunk (une recherche par colonne, pas par bloc).
                let (wx, wz) = (x0 + x as i32, z0 + z as i32);
                let c = ChunkPos::containing(wx, wz);
                let (dx, dz) = (c.x - n.center.x, c.z - n.center.z);
                let chunk = if dx.abs() <= 1 && dz.abs() <= 1 {
                    n.get(dx, dz)
                } else {
                    None
                };
                let (lx, lz) = (
                    wx.rem_euclid(CHUNK_SIZE as i32) as usize,
                    wz.rem_euclid(CHUNK_SIZE as i32) as usize,
                );
                let mut open = true;
                for y in (0..h).rev() {
                    let o = chunk.is_some_and(|ch| ch.get(lx, y + y0, lz).is_opaque());
                    opaque[idx(x, y, z)] = o;
                    if o {
                        open = false;
                    }
                    if open {
                        data[idx(x, y, z)] = 15;
                    }
                }
            }
        }
        // Diffusion vers les zones couvertes.
        let mut queue = VecDeque::new();
        let dirs: [[i32; 3]; 6] = [
            [1, 0, 0],
            [-1, 0, 0],
            [0, 1, 0],
            [0, -1, 0],
            [0, 0, 1],
            [0, 0, -1],
        ];
        let inside = |x: i32, y: i32, z: i32| {
            x >= 0 && z >= 0 && y >= 0 && (x as usize) < w && (z as usize) < w && (y as usize) < h
        };
        for y in 0..h {
            for z in 0..w {
                for x in 0..w {
                    if data[idx(x, y, z)] != 15 {
                        continue;
                    }
                    let edge = dirs.iter().any(|d| {
                        let (a, b, c) = (x as i32 + d[0], y as i32 + d[1], z as i32 + d[2]);
                        inside(a, b, c) && {
                            let i = idx(a as usize, b as usize, c as usize);
                            !opaque[i] && data[i] < 14
                        }
                    });
                    if edge {
                        queue.push_back((x as i32, y as i32, z as i32));
                    }
                }
            }
        }
        while let Some((x, y, z)) = queue.pop_front() {
            let v = data[idx(x as usize, y as usize, z as usize)];
            if v <= 1 {
                continue;
            }
            for d in dirs {
                let (a, b, c) = (x + d[0], y + d[1], z + d[2]);
                if !inside(a, b, c) {
                    continue;
                }
                let i = idx(a as usize, b as usize, c as usize);
                if !opaque[i] && data[i] < v - 1 {
                    data[i] = v - 1;
                    queue.push_back((a, b, c));
                }
            }
        }
        Self {
            x0,
            z0,
            w,
            y0,
            h,
            data,
        }
    }

    #[inline]
    fn get(&self, x: i32, y: i32, z: i32) -> u8 {
        let (lx, lz) = (x - self.x0, z - self.z0);
        let y = y - self.y0 as i32;
        if y >= self.h as i32 || lx < 0 || lz < 0 || lx >= self.w as i32 || lz >= self.w as i32 {
            return 15;
        }
        if y < 0 {
            return 0;
        }
        self.data[(y as usize * self.w + lz as usize) * self.w + lx as usize]
    }
}

impl Padded {
    fn build(n: &Neighborhood) -> Self {
        let pos = n.center;
        let mut top = 0;
        for dz in -1..=1 {
            for dx in -1..=1 {
                if let Some(c) = n.get(dx, dz) {
                    top = top.max(c.top());
                }
            }
        }
        let height = (top + 2).min(CHUNK_HEIGHT + 2);
        let mut blocks = vec![BlockId::AIR; P * P * height];
        let (ox, oz) = pos.origin();

        for dz in -1..=1i32 {
            for dx in -1..=1i32 {
                let Some(c) = n.get(dx, dz) else {
                    continue;
                };
                for pz in 0..P {
                    let wz = oz + pz as i32 - 1;
                    if ChunkPos::containing(ox, wz).z != pos.z + dz {
                        continue;
                    }
                    let lz = wz.rem_euclid(CHUNK_SIZE as i32) as usize;
                    for px in 0..P {
                        let wx = ox + px as i32 - 1;
                        if ChunkPos::containing(wx, oz).x != pos.x + dx {
                            continue;
                        }
                        let lx = wx.rem_euclid(CHUNK_SIZE as i32) as usize;
                        for py in 1..height.min(c.top() + 1) {
                            blocks[(py * P + pz) * P + px] = c.get(lx, py - 1, lz);
                        }
                    }
                }
            }
        }
        // Couche py = 0 (sous le monde) : opaque pour ne pas générer la face du dessous.
        blocks[..P * P].fill(BlockId::BEDROCK);
        Self { blocks, height }
    }

    /// Coordonnées locales au chunk (x, z dans -1..=16, y dans -1..).
    #[inline]
    fn get(&self, x: i32, y: i32, z: i32) -> BlockId {
        let py = (y + 1) as usize;
        if py >= self.height {
            return BlockId::AIR;
        }
        self.blocks[(py * P + (z + 1) as usize) * P + (x + 1) as usize]
    }
}

#[inline]
fn add(a: [i32; 3], b: [i32; 3]) -> [i32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
#[inline]
fn neg(a: [i32; 3]) -> [i32; 3] {
    [-a[0], -a[1], -a[2]]
}

/// Maille le chunk `pos` à partir du monde (thread courant).
/// Lumière colorée des torches en `p` (diagnostic).
#[doc(hidden)]
pub fn light_probe(world: &World, p: [i32; 3]) -> [u8; 3] {
    let c = ChunkPos::containing(p[0], p[2]);
    Neighborhood::from_world(world, c)
        .and_then(|n| LightGrid::build(&n))
        .map_or([0; 3], |g| g.get(p[0], p[1], p[2]))
}

pub fn mesh_chunk(world: &World, pos: ChunkPos) -> MeshData {
    match Neighborhood::from_world(world, pos) {
        Some(n) => mesh_neighborhood(&n),
        None => MeshData::default(),
    }
}

/// Maille le chunk central d'un voisinage (utilisable depuis un thread de travail).
pub fn mesh_neighborhood(n: &Neighborhood) -> MeshData {
    let mut out = MeshData::default();
    let pos = n.center;
    let Some(chunk) = n.get(0, 0) else {
        return out;
    };
    let pad = Padded::build(n);
    let light = LightGrid::build(n);
    let sky = SkyGrid::build(n);
    let (ox, oz) = pos.origin();
    let light_at = |p: [i32; 3]| {
        let [r, g, b] = light
            .as_ref()
            .map_or([0; 3], |l| l.get(ox + p[0], p[1], oz + p[2]));
        [r, g, b, sky.get(ox + p[0], p[1], oz + p[2])]
    };

    // Couches vides sous le plus bas bloc sautées (tranches 16 × 16 × 16 sans aucun bloc).
    for y in chunk.bottom() as i32..chunk.top() as i32 {
        for z in 0..CHUNK_SIZE as i32 {
            for x in 0..CHUNK_SIZE as i32 {
                let id = chunk.get(x as usize, y as usize, z as usize);
                if id == BlockId::AIR {
                    continue;
                }
                let def = id.def();
                // Les triangles d'un bloc translucide passent dans la liste à part, en fin de bloc.
                if def.translucent {
                    let start = out.indices.len();
                    mesh_block(&mut out, n, &pad, &light_at, ox, oz, [x, y, z], id);
                    let moved = out.indices.split_off(start);
                    out.translucent.extend(moved);
                    continue;
                }
                if def.reflect > 0 {
                    let start = out.indices.len();
                    mesh_block(&mut out, n, &pad, &light_at, ox, oz, [x, y, z], id);
                    let moved = out.indices.split_off(start);
                    out.reflective.extend(moved);
                    continue;
                }
                mesh_block(&mut out, n, &pad, &light_at, ox, oz, [x, y, z], id);
            }
        }
    }
    out.connectivity = Some(section_connectivity(chunk));
    out.sort_by_face();
    out
}

/// Faces d'un bloc (coordonnées locales au chunk `b`).
#[allow(clippy::too_many_arguments)]
fn mesh_block(
    out: &mut MeshData,
    n: &Neighborhood,
    pad: &Padded,
    light_at: &dyn Fn([i32; 3]) -> [u8; 4],
    ox: i32,
    oz: i32,
    b: [i32; 3],
    id: BlockId,
) {
    let opaque = |p: [i32; 3]| pad.get(p[0], p[1], p[2]).is_opaque();
    // La lumière passe-t-elle de la case `p` à sa voisine `p + d` (dalles, escaliers) ?
    let passage = |p: [i32; 3], d: [i32; 3]| {
        let face = match d {
            [1, 0, 0] => 0,
            [-1, 0, 0] => 1,
            [0, 1, 0] => 2,
            [0, -1, 0] => 3,
            [0, 0, 1] => 4,
            _ => 5,
        };
        let q = add(p, d);
        let a = light_cover(pad.get(p[0], p[1], p[2]))[face];
        let b = light_cover(pad.get(q[0], q[1], q[2]))[face ^ 1];
        a | b != u16::MAX
    };
    let [x, y, z] = b;
    {
        {
            {
                let def = id.def();
                let (wx, wy, wz) = ((ox + x) as f32, y as f32, (oz + z) as f32);
                match def.shape {
                    Shape::Air | Shape::Invisible => return,
                    Shape::Cube => {}
                    Shape::Torch | Shape::WallTorch(_) => {
                        let lit = light_at(b);
                        // Torche murale : même modèle, penché contre le mur.
                        let wall = if let Shape::WallTorch(d) = def.shape {
                            Some(d)
                        } else {
                            None
                        };
                        let xf = move |p: [f32; 3]| match wall {
                            Some(d) => {
                                let l = wall_torch_xf(d, [p[0] - wx, p[1] - wy, p[2] - wz]);
                                [l[0] + wx, l[1] + wy, l[2] + wz]
                            }
                            None => p,
                        };
                        // Bâton de 8 px, flamme de 5 px posée dessus (13 px en tout).
                        emit_box_xf(
                            out,
                            [wx + 0.4375, wy, wz + 0.4375],
                            [wx + 0.5625, wy + 0.5, wz + 0.5625],
                            id,
                            [lit; 6],
                            TEX_SHARP,
                            |_| false,
                            &xf,
                            wall.is_some(),
                        );
                        let flame = n
                            .lights
                            .iter()
                            .find(|l| l.pos == [ox + x, y, oz + z])
                            .map_or([255, 150, 55, 15], |l| [l.rgb[0], l.rgb[1], l.rgb[2], 15]);
                        emit_box_xf(
                            out,
                            [wx + 0.40625, wy + 0.5, wz + 0.40625],
                            [wx + 0.59375, wy + 0.8125, wz + 0.59375],
                            BlockId::TORCH,
                            [flame; 6],
                            TEX_EMISSIVE | TEX_SHARP,
                            |_| false,
                            &xf,
                            wall.is_some(),
                        );
                        return;
                    }
                    Shape::Cross => {
                        emit_cross(out, [wx, wy, wz], id, light_at(b));
                        return;
                    }
                    _ => {
                        let mut boxes = def.boxes();
                        if let Shape::Post(kind) = def.shape {
                            let mut any = false;
                            for (side, d) in [
                                (0u8, [1, 0, 0]),
                                (1, [-1, 0, 0]),
                                (4, [0, 0, 1]),
                                (5, [0, 0, -1]),
                            ] {
                                if post_connects(kind, pad.get(x + d[0], y, z + d[2])) {
                                    boxes.extend(post_arms(kind, side));
                                    any = true;
                                }
                            }
                            // Vitre isolée : en croix.
                            if !any && kind == PostKind::Pane {
                                for side in [0u8, 1, 4, 5] {
                                    boxes.extend(post_arms(kind, side));
                                }
                            }
                        }
                        // Bannière : deux blocs de haut, elle pend dans la case du dessous
                        // (seul le bloc du haut a une hitbox, comme dans Minecraft).
                        let banner = def.key.starts_with("banner_");
                        // Moitié du bas seulement dans le vide ou derrière du verre : dans
                        // un bloc, on ne voit que le bloc (pas de scintillement).
                        let below = pad.get(x, y - 1, z);
                        let banner_low =
                            banner && (below == BlockId::AIR || below.def().translucent);
                        type Part = (([u8; 3], [u8; 3]), i32);
                        let parts: Vec<Part> = boxes
                            .iter()
                            .map(|&bx| (bx, 0))
                            .chain(boxes.iter().filter(|_| banner_low).map(|&bx| (bx, -1)))
                            .collect();
                        for ((mn, mx), dy) in parts {
                            let b = add(b, [0, dy, 0]);
                            let lit = light_at(b);
                            let wy = wy + dy as f32;
                            let lo = mn.map(|v| v as f32 / 16.0);
                            let hi = mx.map(|v| v as f32 / 16.0);
                            // Face au bord du bloc : éclairée par la case voisine qu'elle
                            // regarde ; face intérieure (dessus d'une dalle du bas) : par
                            // la case du bloc.
                            let face_light: [[u8; 4]; 6] = std::array::from_fn(|face| {
                                let (nrm, _, _) = FACES[face];
                                let k = nrm.iter().position(|&c| c != 0).unwrap();
                                let on_edge = if nrm[k] > 0 { mx[k] == 16 } else { mn[k] == 0 };
                                let nb = add(b, nrm);
                                if on_edge && !opaque(nb) {
                                    light_at(nb)
                                } else {
                                    lit
                                }
                            });
                            // Face sur le bord du bloc et voisin plein : cachée.
                            let skip = |face: usize| {
                                let (nrm, _, _) = FACES[face];
                                // Jointure entre les deux moitiés d'une bannière : rien.
                                if banner_low
                                    && (nrm[1] == -1 && dy == 0 || nrm[1] == 1 && dy == -1)
                                {
                                    return true;
                                }
                                let k = nrm.iter().position(|&c| c != 0).unwrap();
                                let on_edge = if nrm[k] > 0 { mx[k] == 16 } else { mn[k] == 0 };
                                on_edge && opaque(add(b, nrm))
                            };
                            // Bannière : moitié du haut (texture des côtés) ou du bas
                            // (texture du dessous) sur toutes ses faces.
                            let t = id.def().tex;
                            let half = if dy == 0 { t[1] } else { t[2] } as u32;
                            let tex_of = |f: u32| if banner { half } else { face_tex(id, f) };
                            emit_box_t(
                                out,
                                [wx + lo[0], wy + lo[1], wz + lo[2]],
                                [wx + hi[0], wy + hi[1], wz + hi[2]],
                                id,
                                &tex_of,
                                face_light,
                                0,
                                skip,
                                &|p| p,
                                false,
                            );
                        }
                        return;
                    }
                }
                for (face, &(n, u, v)) in FACES.iter().enumerate() {
                    let front = add(b, n);
                    let facing = pad.get(front[0], front[1], front[2]);
                    // Cachée par un bloc plein, par le même bloc (verre, eau), ou par la
                    // face pleine d'une dalle ou d'un escalier collée contre elle (sinon
                    // les deux surfaces au même endroit scintillent sous du verre).
                    let covered = crate::registry::light_faces(facing) & (1 << (face ^ 1)) != 0;
                    if opaque(front)
                        || (def.cull_same && facing == id)
                        || (covered && !def.translucent)
                    {
                        continue;
                    }
                    // Origine de la face : côté positif de la normale si elle est positive.
                    let base = [x + n[0].max(0), y + n[1].max(0), z + n[2].max(0)];
                    let mut ao = [0u32; 4];
                    let mut lights = [[0u8; 4]; 4];
                    let corners = [(0, 0), (1, 0), (1, 1), (0, 1)];
                    for (i, &(a, c)) in corners.iter().enumerate() {
                        let su = if a == 1 { u } else { neg(u) };
                        let sv = if c == 1 { v } else { neg(v) };
                        let (p1, p2) = (add(front, su), add(front, sv));
                        let p3 = add(p1, sv);
                        let s1 = opaque(p1);
                        let s2 = opaque(p2);
                        let co = opaque(p3);
                        ao[i] = if s1 && s2 {
                            0
                        } else {
                            3 - (s1 as u32 + s2 as u32 + co as u32)
                        };
                        // Lumière des torches : seulement les cases que la lumière peut
                        // relier à la case devant la face (pas le vide sous une dalle).
                        let (o1, o2) = (passage(front, su), passage(front, sv));
                        let o3 = (o1 && passage(p1, sv)) || (o2 && passage(p2, su));
                        // Lumière lissée : moyenne des cases d'air autour du coin.
                        let mut sum = [0u32; 4];
                        let mut cnt = 0;
                        let mut rgb = [0u32; 3];
                        let mut rgb_cnt = 0;
                        for (p, solid, open) in [
                            (front, false, true),
                            (p1, s1, o1),
                            (p2, s2, o2),
                            (p3, co || (s1 && s2), o3),
                        ] {
                            if !solid {
                                let l = light_at(p);
                                (0..4).for_each(|k| sum[k] += l[k] as u32);
                                cnt += 1;
                                if open {
                                    (0..3).for_each(|k| rgb[k] += l[k] as u32);
                                    rgb_cnt += 1;
                                }
                            }
                        }
                        let mut l = sum.map(|v| (v / cnt) as u8);
                        (0..3).for_each(|k| l[k] = (rgb[k] / rgb_cnt) as u8);
                        lights[i] = l;
                    }
                    // Verre contre un bloc non plein (dalle, escalier, trappe…) : la face
                    // reste (sa teinte compte) mais rentre d'un cheveu, sinon elle est au
                    // même endroit que celle du voisin et passe devant en scintillant.
                    // Contre une face pleine, elle est éclairée par la case du verre (celle
                    // de la dalle est noire).
                    let inset = if def.translucent && facing != BlockId::AIR && facing != id {
                        0.002
                    } else {
                        0.0
                    };
                    if covered {
                        ao = [3; 4];
                        lights = [light_at(b); 4];
                    }
                    // Verre : bords cachés du côté des voisins du même verre.
                    let tex = if def.connect != 0 {
                        let k = n.iter().position(|&c| c != 0).unwrap();
                        let same = |d: [i32; 3]| pad.get(x + d[0], y + d[1], z + d[2]) == id;
                        // Voisins (haut, droite, bas, gauche) dans le repère de la texture
                        // (u, v) que le shader tire de la position monde.
                        let dirs: [[i32; 3]; 4] = match k {
                            0 => [[0, 1, 0], [0, 0, 1], [0, -1, 0], [0, 0, -1]],
                            1 => [[0, 0, -1], [1, 0, 0], [0, 0, 1], [-1, 0, 0]],
                            _ => [[0, 1, 0], [1, 0, 0], [0, -1, 0], [-1, 0, 0]],
                        };
                        let mask = dirs
                            .iter()
                            .enumerate()
                            .filter(|(_, &d)| same(d))
                            .fold(0u32, |m, (i, _)| m | 1 << i);
                        if mask == 0 {
                            face_tex(id, face as u32)
                        } else {
                            def.connect as u32 + mask - 1
                        }
                    } else {
                        face_tex(id, face as u32)
                    };
                    let start = out.vertices.len() as u32;
                    for (i, &(a, c)) in corners.iter().enumerate() {
                        let p = [
                            base[0] + a * u[0] + c * v[0],
                            base[1] + a * u[1] + c * v[1],
                            base[2] + a * u[2] + c * v[2],
                        ];
                        out.vertices.push(Vertex {
                            pos: [
                                (ox + p[0]) as f32 - n[0] as f32 * inset,
                                p[1] as f32 - n[1] as f32 * inset,
                                (oz + p[2]) as f32 - n[2] as f32 * inset,
                            ],
                            data: Vertex::block_data(face as u32, ao[i], lights[i])
                                | reflect_bits(id),
                            tex,
                        });
                    }
                    if ao[0] + ao[2] >= ao[1] + ao[3] {
                        out.indices
                            .extend_from_slice(&[0, 1, 2, 2, 3, 0].map(|i| start + i));
                    } else {
                        out.indices
                            .extend_from_slice(&[1, 2, 3, 3, 0, 1].map(|i| start + i));
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    }
    fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
        [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
    }

    fn empty_world_with_block(x: i32, y: i32, z: i32) -> World {
        let mut w = World::new(0);
        for dz in -1..=1 {
            for dx in -1..=1 {
                w.insert_empty(ChunkPos { x: dx, z: dz });
            }
        }
        w.set(x, y, z, BlockId::STONE);
        w
    }

    #[test]
    fn single_block_has_six_faces_facing_out() {
        let w = empty_world_with_block(4, 100, 4);
        let m = mesh_chunk(&w, ChunkPos { x: 0, z: 0 });
        assert_eq!(m.vertices.len(), 24);
        assert_eq!(m.indices.len(), 36);
        let center = [4.5, 100.5, 4.5];
        for tri in m.indices.chunks(3) {
            let [a, b, c] = [0, 1, 2].map(|i| m.vertices[tri[i] as usize].pos);
            let n = cross(sub(b, a), sub(c, a));
            let out = sub(a, center);
            let d = n[0] * out[0] + n[1] * out[1] + n[2] * out[2];
            assert!(d > 0.0, "triangle orienté vers l'intérieur");
        }
        // Bloc isolé : aucune occlusion.
        assert!(m.vertices.iter().all(|v| v.ao() == 3));
        assert!(
            m.vertices
                .iter()
                .all(|v| v.sky() == if v.face() == 3 { 14 } else { 15 }),
            "bloc isolé : ciel ouvert, dessous à 14"
        );
    }

    #[test]
    fn torch_light_is_colored_and_blocked_by_walls() {
        let mut w = World::new(0);
        for dz in -1..=1 {
            for dx in -1..=1 {
                w.insert_empty(ChunkPos { x: dx, z: dz });
            }
        }
        // Sol en pierre, torche au lithium (rouge) au milieu, mur à l'est.
        for z in -4..20 {
            for x in -4..20 {
                w.set(x, 99, z, BlockId::STONE);
            }
        }
        for y in 100..110 {
            for z in -4..20 {
                w.set(11, y, z, BlockId::STONE);
            }
        }
        let li = crate::flame::CHEMICALS
            .iter()
            .position(|c| c.symbol == "Li")
            .unwrap();
        w.place_torch(8, 100, 8, Some(li));
        let m = mesh_chunk(&w, ChunkPos { x: 0, z: 0 });
        let floor_light = |x: f32, z: f32| {
            m.vertices
                .iter()
                .filter(|v| v.face() == 2 && v.pos[1] == 100.0 && v.pos[0] == x && v.pos[2] == z)
                .map(|v| v.rgb6())
                .next()
                .unwrap()
        };
        let near = floor_light(8.0, 8.0);
        assert!(
            near[0] > 30 && near[0] > near[2] * 2,
            "rouge attendu : {near:?}"
        );
        let far = floor_light(2.0, 8.0);
        assert!(far[0] < near[0]);
        // Derrière le mur (x = 13), aucune lumière directe : seulement en contournant.
        let behind = floor_light(13.0, 8.0);
        assert!(
            behind[0] < far[0],
            "le mur doit arrêter la lumière : {behind:?} vs {far:?}"
        );
        // La flamme porte la couleur du lithium.
        assert!(m.vertices.iter().any(|v| v.tex & TEX_EMISSIVE != 0));
    }

    #[test]
    fn sealed_pocket_has_no_passage_open_one_does() {
        let mut c = Chunk::new(ChunkPos { x: 0, z: 0 });
        for y in 0..48 {
            for z in 0..16 {
                for x in 0..16 {
                    c.set(x, y, z, BlockId::STONE);
                }
            }
        }
        // Poche d'air fermée au milieu de la tranche 1 : aucun passage.
        c.set(8, 24, 8, BlockId::AIR);
        let conn = section_connectivity(&c);
        assert_eq!(conn[1], 0);
        assert_eq!(conn[5], ALL_LINKS, "au-dessus du sol : tranche vide");
        // Puits du haut en bas de la tranche 2 : le dessus communique avec le dessous.
        for y in 32..48 {
            c.set(3, y, 3, BlockId::AIR);
        }
        let conn = section_connectivity(&c);
        assert!(conn[2] & link(2, 3) != 0 && conn[2] & link(0, 1) == 0);
    }

    #[test]
    fn top_faces_belong_to_the_air_section_above() {
        let mut w = World::new(0);
        w.insert_empty(ChunkPos { x: 0, z: 0 });
        w.set(5, 15, 5, BlockId::STONE);
        let m = mesh_chunk(&w, ChunkPos { x: 0, z: 0 });
        // Dessus (y = 16) : tranche 1 ; dessous (y = 15) : tranche 0.
        assert_eq!(m.section_counts[2][1], 6);
        assert_eq!(m.section_counts[3][0], 6);
        assert_eq!(
            m.section_counts.iter().flatten().sum::<u32>() as usize,
            m.indices.len()
        );
    }

    #[test]
    fn half_height_tops_stay_in_their_own_section() {
        // Dalle posée en y = 79 : son dessus est en 79,5, sous la limite de tranche (80).
        // Il doit être rangé dans la tranche 4 (64..80), visible d'un œil en 79,6.
        let mut w = World::new(0);
        w.insert_empty(ChunkPos { x: 0, z: 0 });
        let slab = crate::registry::registry()
            .by_key("slab:stone:bottom")
            .unwrap();
        w.set(5, 79, 5, slab);
        let m = mesh_chunk(&w, ChunkPos { x: 0, z: 0 });
        assert!(
            m.section_counts[2][4] > 0,
            "dessus de la dalle en tranche 4"
        );
        assert_eq!(
            m.section_counts[2][5], 0,
            "rien du dessus de la dalle en tranche 5"
        );
        // Bloc plein en y = 79 : dessus en 80, côté air = case 80 → tranche 5.
        let mut w = World::new(0);
        w.insert_empty(ChunkPos { x: 0, z: 0 });
        w.set(5, 79, 5, BlockId::STONE);
        let m = mesh_chunk(&w, ChunkPos { x: 0, z: 0 });
        assert!(m.section_counts[2][5] > 0);
    }

    #[test]
    fn face_groups_match_real_normals() {
        // Torches murales des 4 côtés et torche droite : toute face rangée dans une
        // direction (0..5) doit vraiment regarder dans ce sens (sinon le tri par direction
        // la cache quand on s'éloigne).
        let mut w = World::new(0);
        w.insert_empty(ChunkPos { x: 0, z: 0 });
        for (i, d) in [0u8, 1, 4, 5].into_iter().enumerate() {
            let x = 2 + 3 * i as i32;
            w.set(x, 99, 8, BlockId::STONE);
            w.place_torch_block(x, 100, 8, BlockId::wall_torch(d), None);
        }
        w.place_torch_block(14, 100, 3, BlockId::TORCH, None);
        // Plante en croix (face 6) : ne doit pas faire planter le tri.
        let reg = crate::registry::registry();
        let flower = reg.by_key("plant:dandelion").expect("pissenlit");
        w.set(12, 99, 12, BlockId::STONE);
        w.set(12, 100, 12, flower);
        let m = mesh_chunk(&w, ChunkPos { x: 0, z: 0 });
        let mut start = 0usize;
        for k in 0..6 {
            let count = m.face_counts[k] as usize;
            for tri in m.indices[start..start + count].chunks_exact(3) {
                let p = |i: u32| m.vertices[i as usize].pos;
                let (a, b, c) = (p(tri[0]), p(tri[1]), p(tri[2]));
                let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
                let v = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
                let n = [
                    u[1] * v[2] - u[2] * v[1],
                    u[2] * v[0] - u[0] * v[2],
                    u[0] * v[1] - u[1] * v[0],
                ];
                let axis = k / 2;
                let sign = if k % 2 == 0 { 1.0 } else { -1.0 };
                assert!(
                    n[axis] * sign > 0.0,
                    "face rangée en {k} mais normale {n:?}"
                );
            }
            start += count;
        }
    }

    #[test]
    fn wall_torch_leans_out_of_its_wall() {
        let mut w = World::new(0);
        w.insert_empty(ChunkPos { x: 0, z: 0 });
        // Mur en +X (bloc en x = 6), torche en x = 5.
        w.set(6, 100, 5, BlockId::STONE);
        w.place_torch_block(5, 100, 5, BlockId::wall_torch(0), None);
        assert!(w.get(5, 100, 5).is_torch());
        let m = mesh_chunk(&w, ChunkPos { x: 0, z: 0 });
        let pts: Vec<[f32; 3]> = m
            .vertices
            .iter()
            .map(|v| v.pos)
            .filter(|p| {
                p[0] >= 5.0
                    && p[0] <= 6.0
                    && p[1] >= 100.0
                    && p[1] < 101.0
                    && p[2] >= 5.0
                    && p[2] <= 6.0
            })
            .collect();
        assert!(pts.len() >= 48, "torche murale maillée : {}", pts.len());
        let lowest =
            pts.iter()
                .cloned()
                .fold([0.0, f32::MAX, 0.0], |a, p| if p[1] < a[1] { p } else { a });
        let highest =
            pts.iter()
                .cloned()
                .fold([0.0, f32::MIN, 0.0], |a, p| if p[1] > a[1] { p } else { a });
        // Pied contre le mur (x proche de 6), sommet penché vers l'extérieur (x plus petit).
        assert!(lowest[0] > 5.8, "pied : {lowest:?}");
        assert!(highest[0] < lowest[0] - 0.2, "sommet : {highest:?}");
        // Zone visée du même côté.
        let (a, b) = w.get(5, 100, 5).def().boxes()[0];
        assert!(a[0] >= 10 && b[0] == 16, "{a:?} {b:?}");
        assert_eq!(
            crate::registry::registry().legacy(50, 1),
            Some(BlockId::wall_torch(1))
        );
    }

    #[test]
    fn torch_light_does_not_leak_through_slab_floors() {
        let reg = crate::registry::registry();
        let bottom = reg.by_key("slab:stone:bottom").unwrap();
        let top = reg.by_key("slab:stone:top").unwrap();
        assert_eq!(crate::registry::light_faces(bottom), 1 << 3);
        assert_eq!(crate::registry::light_faces(top), 1 << 2);
        assert_eq!(crate::registry::light_faces(BlockId::STONE), 0x3f);
        assert_eq!(crate::registry::light_faces(BlockId::TORCH), 0);
        for slab in [bottom, top] {
            let mut w = World::new(0);
            for dz in -1..=1 {
                for dx in -1..=1 {
                    w.insert_empty(ChunkPos { x: dx, z: dz });
                }
            }
            // Plancher de dalles sur toute la zone en y = 100, torche dessous.
            for z in -16..32 {
                for x in -16..32 {
                    w.set(x, 100, z, slab);
                }
            }
            w.place_torch(8, 98, 8, None);
            let n = Neighborhood::from_world(&w, ChunkPos { x: 0, z: 0 }).unwrap();
            let g = LightGrid::build(&n).unwrap();
            assert!(g.get(8, 99, 8)[0] > 100, "sous le plancher : éclairé");
            for x in 0..16 {
                for z in 0..16 {
                    assert_eq!(g.get(x, 101, z), [0; 3], "au-dessus en {x} {z}");
                }
            }
            // Un trou dans le plancher laisse passer la lumière.
            w.set(9, 100, 8, BlockId::AIR);
            let n = Neighborhood::from_world(&w, ChunkPos { x: 0, z: 0 }).unwrap();
            let g = LightGrid::build(&n).unwrap();
            assert!(g.get(9, 101, 8)[0] > 50);
        }
    }

    #[test]
    fn slab_on_glowstone_stays_dark() {
        let reg = crate::registry::registry();
        let glow = reg.by_key("glowstone").unwrap();
        let slab = reg.by_key("slab:hardened_clay:bottom").unwrap();
        let mut w = World::new(0);
        for dz in -1..=1 {
            for dx in -1..=1 {
                w.insert_empty(ChunkPos { x: dx, z: dz });
            }
        }
        // Sol plein en y = 100 avec une pierre lumineuse, dalle posée dessus.
        for z in -16..32 {
            for x in -16..32 {
                w.set(x, 100, z, BlockId::SANDSTONE);
            }
        }
        w.set(8, 100, 8, glow);
        let lit = |w: &World, p: [i32; 3]| {
            let n = Neighborhood::from_world(w, ChunkPos { x: 0, z: 0 }).unwrap();
            LightGrid::build(&n).map_or([0; 3], |g| g.get(p[0], p[1], p[2]))
        };
        assert!(
            lit(&w, [9, 101, 8])[0] > 0,
            "sans dalle : le sol autour est éclairé"
        );
        w.set(8, 101, 8, slab);
        for p in [[8, 101, 8], [9, 101, 8], [8, 102, 8], [7, 101, 8]] {
            assert_eq!(lit(&w, p), [0; 3], "{p:?} éclairé à travers la dalle");
        }
        // Dalle du haut posée sur la pierre lumineuse, entourée d'un sol de dalles du
        // bas : la lumière remplit le vide sous la dalle sans en sortir.
        let top = reg.by_key("slab:hardened_clay:top").unwrap();
        for z in -16..32 {
            for x in -16..32 {
                w.set(x, 101, z, slab);
            }
        }
        w.set(8, 101, 8, top);
        assert!(lit(&w, [8, 101, 8])[0] > 0, "vide sous la dalle éclairé");
        for p in [
            [9, 101, 8],
            [7, 101, 8],
            [8, 101, 9],
            [8, 101, 7],
            [8, 102, 8],
            [9, 102, 8],
        ] {
            assert_eq!(
                lit(&w, p),
                [0; 3],
                "{p:?} éclairé à travers la dalle du haut"
            );
        }
        // Les faces visibles de la dalle du haut (dessus, côtés) restent sombres.
        let n = Neighborhood::from_world(&w, ChunkPos { x: 0, z: 0 }).unwrap();
        let mesh = mesh_neighborhood(&n);
        let near = |v: &&Vertex| {
            (8.0..=9.0).contains(&v.pos[0])
                && (8.0..=9.0).contains(&v.pos[2])
                && v.pos[1] > 101.6
                && v.pos[1] <= 102.0
        };
        assert!(mesh.vertices.iter().filter(near).count() >= 4);
        let lit_faces = mesh
            .vertices
            .iter()
            .filter(|v| {
                (8.0..=9.0).contains(&v.pos[0])
                    && (8.0..=9.0).contains(&v.pos[2])
                    && v.pos[1] > 101.6
                    && v.pos[1] <= 102.0
            })
            .filter(|v| v.data >> 14 != 0)
            .count();
        assert_eq!(
            lit_faces, 0,
            "dessus et haut des côtés de la dalle éclairés"
        );
    }

    #[test]
    fn lit_void_under_floor_does_not_glow_on_walls() {
        let reg = crate::registry::registry();
        let glow = reg.by_key("glowstone").unwrap();
        let top = reg.by_key("slab:hardened_clay:top").unwrap();
        let bottom = reg.by_key("slab:hardened_clay:bottom").unwrap();
        let mut w = World::new(0);
        for dz in -1..=1 {
            for dx in -1..=1 {
                w.insert_empty(ChunkPos { x: dx, z: dz });
            }
        }
        // Sol plein (y 99) avec pierre lumineuse, dalles du haut (vide éclairé dessous),
        // puis plancher de dalles du bas (y 101) et un mur plein en x = 10.
        for z in -16..32 {
            for x in -16..32 {
                w.set(x, 99, z, BlockId::SANDSTONE);
                w.set(x, 100, z, top);
                w.set(x, 101, z, bottom);
            }
        }
        w.set(8, 99, 8, glow);
        for z in 0..16 {
            for y in 100..105 {
                w.set(10, y, z, BlockId::STONE);
            }
        }
        let n = Neighborhood::from_world(&w, ChunkPos { x: 0, z: 0 }).unwrap();
        let mesh = mesh_neighborhood(&n);
        // Faces (quads de 4 sommets) du mur au-dessus du plancher.
        let wall: Vec<_> = mesh
            .vertices
            .chunks(4)
            .filter(|q| {
                q.iter()
                    .all(|v| v.pos[0] == 10.0 && v.pos[1] >= 101.0 && (v.data >> 8) & 7 == 1)
            })
            .flatten()
            .collect();
        assert!(!wall.is_empty());
        assert!(
            wall.iter().all(|v| v.data >> 14 == 0),
            "le mur au-dessus du plancher reçoit la lumière du vide : {:?}",
            wall.iter()
                .filter(|v| v.data >> 14 != 0)
                .map(|v| (v.pos, (v.data >> 14) & 63))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn caves_are_dark_and_overhangs_dim() {
        let mut w = World::new(0);
        for dz in -1..=1 {
            for dx in -1..=1 {
                w.insert_empty(ChunkPos { x: dx, z: dz });
            }
        }
        // Dalle épaisse de pierre y 100..=104 sur tout le chunk et autour.
        for y in 100..=104 {
            for z in -16..32 {
                for x in -16..32 {
                    w.set(x, y, z, BlockId::STONE);
                }
            }
        }
        // Grotte fermée au milieu (y 101..=102), et un abri ouvert sur le côté sous la dalle.
        for x in 4..12 {
            for z in 4..12 {
                w.set(x, 101, z, BlockId::AIR);
                w.set(x, 102, z, BlockId::AIR);
            }
        }
        let m = mesh_chunk(&w, ChunkPos { x: 0, z: 0 });
        let floor_sky = |x: f32, y: f32, z: f32| {
            m.vertices
                .iter()
                .find(|v| v.face() == 2 && v.pos == [x, y, z])
                .map(|v| v.sky())
        };
        assert_eq!(floor_sky(6.0, 101.0, 6.0), Some(0), "grotte fermée : noire");
        assert_eq!(
            floor_sky(6.0, 105.0, 6.0),
            Some(15),
            "dessus de la dalle : plein ciel"
        );
    }

    #[test]
    fn banner_hangs_below_only_into_air_or_glass() {
        let reg = crate::registry::registry();
        let banner = reg.by_key("banner_red:4").unwrap();
        let below_count = |under: BlockId| {
            let mut w = empty_world_with_block(4, 100, 4);
            w.set(4, 100, 4, banner);
            w.set(4, 99, 4, under);
            let m = mesh_chunk(&w, ChunkPos { x: 0, z: 0 });
            m.vertices
                .iter()
                .filter(|v| {
                    v.pos[1] < 100.0 && v.pos[1] >= 99.0 && v.pos[0] >= 4.0 && v.pos[0] <= 5.0
                })
                .filter(|v| v.pos[2] > 4.0 && v.pos[2] < 5.0)
                .count()
        };
        assert!(below_count(BlockId::AIR) > 0, "moitié du bas dans le vide");
        assert!(
            below_count(reg.by_key("glass").unwrap()) > 0,
            "derrière du verre"
        );
        assert_eq!(below_count(BlockId::STONE), 0, "pas dans un bloc plein");
    }

    #[test]
    fn adjacent_blocks_hide_shared_faces() {
        let mut w = empty_world_with_block(4, 100, 4);
        w.set(5, 100, 4, BlockId::STONE);
        let m = mesh_chunk(&w, ChunkPos { x: 0, z: 0 });
        assert_eq!(m.vertices.len(), 40); // 2 × 6 − 2 faces
    }

    #[test]
    fn faces_across_chunk_border_are_culled() {
        let mut w = empty_world_with_block(15, 100, 3);
        w.set(16, 100, 3, BlockId::STONE);
        let a = mesh_chunk(&w, ChunkPos { x: 0, z: 0 });
        let b = mesh_chunk(&w, ChunkPos { x: 1, z: 0 });
        assert_eq!(a.vertices.len(), 20);
        assert_eq!(b.vertices.len(), 20);
    }

    #[test]
    fn ground_block_bottom_face_is_skipped_and_ao_darkens_corners() {
        let mut w = empty_world_with_block(4, 0, 4);
        w.set(5, 1, 4, BlockId::STONE); // mur à côté, au-dessus de la face +Y
        let m = mesh_chunk(&w, ChunkPos { x: 0, z: 0 });
        assert!(m.vertices.iter().all(|v| v.face() != 3 || v.pos[1] > 0.0));
        let top: Vec<_> = m
            .vertices
            .iter()
            .filter(|v| v.face() == 2 && v.pos[1] == 1.0)
            .collect();
        assert!(top.iter().any(|v| v.ao() < 3));
    }
}

#[cfg(test)]
mod bench_empty {
    use super::*;
    use crate::World;

    /// Temps de maillage d'une carte « flottante » (blocs entre y 90 et 110, vide dessous).
    #[test]
    #[ignore]
    fn mesh_time_floating_map() {
        let mut w = World::new(0);
        for cz in -1..=1 {
            for cx in -1..=1 {
                w.insert_empty(ChunkPos { x: cx, z: cz });
            }
        }
        for z in -16..32 {
            for x in -16..32 {
                for y in 90..96 {
                    w.set(x, y, z, BlockId::STONE);
                }
                if (x + z) % 5 == 0 {
                    for y in 96..110 {
                        w.set(x, y, z, BlockId::BRICK);
                    }
                }
            }
        }
        let n = Neighborhood::from_world(&w, ChunkPos { x: 0, z: 0 }).unwrap();
        let t = std::time::Instant::now();
        let runs = 50;
        for _ in 0..runs {
            std::hint::black_box(mesh_neighborhood(&n));
        }
        println!("maillage : {:?} par chunk", t.elapsed() / runs);
        let c = w.chunk(ChunkPos { x: 0, z: 0 }).unwrap();
        let t = std::time::Instant::now();
        for _ in 0..runs {
            std::hint::black_box(section_connectivity(c));
        }
        println!("passages : {:?} par chunk", t.elapsed() / runs);
        let t = std::time::Instant::now();
        for _ in 0..runs {
            std::hint::black_box(Padded::build(&n));
        }
        println!("copie : {:?}", t.elapsed() / runs);
        let t = std::time::Instant::now();
        for _ in 0..runs {
            std::hint::black_box(SkyGrid::build(&n));
        }
        println!("ciel : {:?}", t.elapsed() / runs);
        let t = std::time::Instant::now();
        for _ in 0..runs {
            std::hint::black_box(LightGrid::build(&n));
        }
        println!("lumière : {:?}", t.elapsed() / runs);
    }
}

#[cfg(test)]
mod cover_tests {
    use super::*;
    use crate::world::World;

    #[test]
    fn glass_face_against_a_top_slab_is_hidden() {
        let reg = crate::registry::registry();
        let glass = reg.by_key("glass").unwrap();
        let count = |under: BlockId| {
            let mut w = World::new(0);
            for dz in -1..=1 {
                for dx in -1..=1 {
                    w.insert_empty(ChunkPos { x: dx, z: dz });
                }
            }
            w.set(4, 100, 4, glass);
            w.set(4, 99, 4, under);
            let m = mesh_chunk(&w, ChunkPos { x: 0, z: 0 });
            m.vertices
                .iter()
                .filter(|v| v.face() == 3 && (v.pos[1] - 100.0).abs() < 0.01)
                .count()
        };
        assert_eq!(count(BlockId::AIR), 4);
        // Contre une dalle du haut : gardée, rentrée d'un cheveu (pas au même endroit).
        assert_eq!(count(reg.by_key("slab:stone:top").unwrap()), 4);
        let mut w = World::new(0);
        for dz in -1..=1 {
            for dx in -1..=1 {
                w.insert_empty(ChunkPos { x: dx, z: dz });
            }
        }
        w.set(4, 99, 4, reg.by_key("slab:stone:top").unwrap());
        w.set(4, 100, 4, glass);
        let m = mesh_chunk(&w, ChunkPos { x: 0, z: 0 });
        let inset: Vec<_> = m
            .vertices
            .iter()
            .filter(|v| v.face() == 3 && (v.pos[1] - 100.002).abs() < 1e-4)
            .collect();
        assert_eq!(inset.len(), 4);
        assert!(
            inset.iter().all(|v| v.sky() == 15),
            "éclairée par la case du verre"
        );
        assert_eq!(count(reg.by_key("slab:stone:bottom").unwrap()), 4);
    }
}
