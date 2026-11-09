//! Terrain lointain à niveaux de détail (LOD).
//!
//! Le monde est découpé en un **quadtree** de nœuds carrés de `32 << niveau` blocs :
//! - niveau 0 = 2 × 2 chunks, affichés en pleine résolution (maillage voxel normal) ;
//! - niveau `L ≥ 1` = tuile de 32 × 32 colonnes, chaque colonne couvrant `2^L` blocs.
//!
//! Un nœud est subdivisé tant que le joueur est à moins de `détail × 2^(L-1)` blocs :
//! la résolution baisse de moitié chaque fois que la distance double, ce qui garde un
//! nombre de triangles quasi constant par anneau et permet de voir à plusieurs
//! kilomètres.
//!
//! Les tuiles sont construites directement depuis le générateur (hauteur de surface,
//! type de sol, arbres), sans générer les chunks : c'est ce qui rend la très grande
//! distance abordable. Chaque colonne est une boîte (dessus + murs vers les voisins
//! plus bas) ; les bords de tuile reçoivent une « jupe » qui masque les fissures
//! entre niveaux différents.

use crate::gen::{surface_height, tree_at, Generator, SAND_LEVEL, SNOW_LEVEL};
use crate::mesh::{MeshData, Vertex, FACES};
use crate::noise::rand01;
use crate::{BlockId, ChunkPos, SurfaceMap, CHUNK_SIZE};

/// Colonnes par côté d'une tuile LOD.
pub const NODE_COLUMNS: i32 = 32;
/// Niveau maximal du quadtree (colonnes de 2^10 = 1024 blocs : bien assez).
pub const MAX_LEVEL: u8 = 10;

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Node {
    pub level: u8,
    pub x: i32,
    pub z: i32,
}

impl Node {
    /// Côté du nœud en blocs.
    pub fn size(self) -> i32 {
        NODE_COLUMNS << self.level
    }

    /// Côté d'une colonne en blocs.
    pub fn step(self) -> i32 {
        1 << self.level
    }

    pub fn origin(self) -> (i32, i32) {
        (self.x * self.size(), self.z * self.size())
    }

    /// Nœud de niveau `level` contenant le bloc (`wx`, `wz`).
    pub fn containing(level: u8, wx: i32, wz: i32) -> Self {
        let size = NODE_COLUMNS << level;
        Self {
            level,
            x: wx.div_euclid(size),
            z: wz.div_euclid(size),
        }
    }

    /// Nœud du niveau au-dessus qui contient celui-ci.
    pub fn parent(self) -> Node {
        Node {
            level: self.level + 1,
            x: self.x.div_euclid(2),
            z: self.z.div_euclid(2),
        }
    }

    pub fn children(self) -> [Node; 4] {
        let l = self.level - 1;
        let (x, z) = (self.x * 2, self.z * 2);
        [
            Node { level: l, x, z },
            Node {
                level: l,
                x: x + 1,
                z,
            },
            Node {
                level: l,
                x,
                z: z + 1,
            },
            Node {
                level: l,
                x: x + 1,
                z: z + 1,
            },
        ]
    }

    /// Chunks couverts par le nœud (niveau 0 : 2 × 2 chunks).
    pub fn chunks(self) -> impl Iterator<Item = ChunkPos> {
        let (ox, oz) = self.origin();
        let n = self.size() / CHUNK_SIZE as i32;
        let (cx, cz) = (
            ox.div_euclid(CHUNK_SIZE as i32),
            oz.div_euclid(CHUNK_SIZE as i32),
        );
        (0..n * n).map(move |i| ChunkPos {
            x: cx + i % n,
            z: cz + i / n,
        })
    }

    /// Les deux nœuds se recouvrent-ils ?
    pub fn overlaps(self, o: Node) -> bool {
        let (ax, az) = self.origin();
        let (bx, bz) = o.origin();
        let (sa, sb) = (self.size(), o.size());
        ax < bx + sb && bx < ax + sa && az < bz + sb && bz < az + sa
    }

    /// Distance horizontale du point (`px`, `pz`) au carré du nœud (0 à l'intérieur).
    pub fn distance(self, px: f32, pz: f32) -> f32 {
        let (ox, oz) = self.origin();
        let s = self.size() as f32;
        let dx = (ox as f32 - px).max(0.0).max(px - (ox as f32 + s));
        let dz = (oz as f32 - pz).max(0.0).max(pz - (oz as f32 + s));
        (dx * dx + dz * dz).sqrt()
    }
}

/// Paramètres de sélection du quadtree.
#[derive(Copy, Clone, Debug)]
pub struct LodSettings {
    /// Rayon (blocs) affiché en pleine résolution.
    pub detail: f32,
    /// Distance maximale affichée (blocs). `<= detail` désactive le LOD.
    pub max_distance: f32,
}

impl LodSettings {
    pub fn enabled(&self) -> bool {
        self.max_distance > self.detail
    }

    fn top_level(&self) -> u8 {
        if !self.enabled() {
            return 1;
        }
        let ratio = (self.max_distance / self.detail).max(1.0);
        ((ratio.log2().ceil() as i32) + 1).clamp(1, MAX_LEVEL as i32) as u8
    }
}

/// Nœuds à afficher autour du point (`px`, `pz`) : niveau 0 = chunks pleine résolution,
/// niveaux ≥ 1 = tuiles LOD. Les nœuds renvoyés ne se recouvrent jamais.
pub fn select(px: f32, pz: f32, s: &LodSettings) -> Vec<Node> {
    let top = s.top_level();
    let reach = s.max_distance.max(s.detail);
    let size = (NODE_COLUMNS << top) as f32;
    let (x0, x1) = (
        ((px - reach) / size).floor() as i32,
        ((px + reach) / size).floor() as i32,
    );
    let (z0, z1) = (
        ((pz - reach) / size).floor() as i32,
        ((pz + reach) / size).floor() as i32,
    );
    let mut out = Vec::new();
    for z in z0..=z1 {
        for x in x0..=x1 {
            visit(Node { level: top, x, z }, px, pz, s, &mut out);
        }
    }
    out
}

fn visit(n: Node, px: f32, pz: f32, s: &LodSettings, out: &mut Vec<Node>) {
    // Un nœud de niveau 0 n'est visité que si son parent a été subdivisé :
    // on le garde toujours, sinon on laisserait un trou à la place du parent.
    if n.level == 0 {
        out.push(n);
        return;
    }
    let d = n.distance(px, pz);
    if d > s.max_distance.max(s.detail) {
        return;
    }
    if d < s.detail * (1u32 << (n.level - 1)) as f32 {
        for c in n.children() {
            visit(c, px, pz, s, out);
        }
    } else if s.enabled() {
        out.push(n);
    }
}

// ---------------------------------------------------------------- maillage des tuiles

/// Blocs utilisés pour texturer le terrain lointain (mêmes textures que de près).
mod color {
    use crate::BlockId;
    pub const GRASS: BlockId = BlockId::GRASS;
    pub const GRASS_SIDE: BlockId = BlockId::GRASS;
    pub const STONE: BlockId = BlockId::STONE;
    pub const SAND: BlockId = BlockId::SAND;
    pub const SNOW: BlockId = BlockId::SNOW;
    pub const LEAVES: BlockId = BlockId::LEAVES;
}

/// Forêt assez dense : la colonne prend le feuillage.
fn mix(a: BlockId, b: BlockId, t: f32) -> BlockId {
    if t >= 0.5 {
        b
    } else {
        a
    }
}

#[derive(Copy, Clone)]
struct Column {
    /// Altitude du dessus de la colonne (bloc de surface + 1).
    top: i32,
    color: BlockId,
    side_top: BlockId,
    side_bottom: BlockId,
}

/// Échantillonne une colonne LOD de `step` blocs dont le coin est (`x0`, `z0`).
fn sample_column(
    seed: u32,
    surface: &SurfaceMap,
    gen: Generator,
    x0: i32,
    z0: i32,
    step: i32,
) -> Column {
    let (cx, cz) = (x0 + step / 2, z0 + step / 2);
    // Zone importée (monde converti) : on lit sa vraie surface.
    if let Some(cols) = surface.get(&ChunkPos::containing(cx, cz)) {
        let [y, b] = cols
            [(cx.rem_euclid(CHUNK_SIZE as i32) + 16 * cz.rem_euclid(CHUNK_SIZE as i32)) as usize];
        let block = BlockId(b);
        let side_bottom = match block {
            BlockId::SAND | BlockId::WATER | BlockId::SANDSTONE => block,
            _ => color::STONE,
        };
        let top = if b == 0 { 0 } else { y as i32 + 1 };
        return Column {
            top,
            color: block,
            side_top: block,
            side_bottom,
        };
    }
    match gen {
        Generator::Void => {
            return Column {
                top: 0,
                color: BlockId::AIR,
                side_top: BlockId::AIR,
                side_bottom: BlockId::AIR,
            }
        }
        Generator::Flat => {
            return Column {
                top: Generator::FLAT_TOP,
                color: BlockId::GRASS,
                side_top: BlockId::GRASS,
                side_bottom: BlockId::DIRT,
            }
        }
        Generator::Procedural => {}
    }
    let h = surface_height(seed, cx, cz);
    let (mut color, side_top) = if h <= SAND_LEVEL {
        (color::SAND, color::SAND)
    } else if h >= SNOW_LEVEL {
        (color::SNOW, color::SNOW)
    } else {
        (color::GRASS, color::GRASS_SIDE)
    };
    let mut top = h + 1;
    let mut side_top = side_top;
    let mut side_bottom = if h <= SAND_LEVEL {
        color::SAND
    } else {
        color::STONE
    };

    if h > SAND_LEVEL && h < SNOW_LEVEL {
        if step <= 4 {
            // Petites colonnes : on cherche un vrai arbre dont la couronne (rayon 2) touche la colonne.
            let mut canopy = None;
            for z in z0 - 2..z0 + step + 2 {
                for x in x0 - 2..x0 + step + 2 {
                    if let Some((th, trunk)) = tree_here(seed, x, z) {
                        let t = th + 1 + trunk + 1;
                        canopy = Some(canopy.map_or(t, |c: i32| c.max(t)));
                    }
                }
            }
            if let Some(t) = canopy {
                top = top.max(t);
                color = color::LEAVES;
                side_top = color::LEAVES;
                side_bottom = color::LEAVES;
            }
        } else {
            // Grandes colonnes : densité d'arbres → teinte de forêt et léger relief.
            let stride = (step / 16).max(1);
            let (mut trees, mut tested) = (0, 0);
            for z in (z0..z0 + step).step_by(stride as usize) {
                for x in (x0..x0 + step).step_by(stride as usize) {
                    tested += 1;
                    if rand01(seed.wrapping_add(20), x, 0, z) <= crate::gen::TREE_CHANCE
                        && tree_edge_ok(x, z)
                    {
                        trees += 1;
                    }
                }
            }
            // Chaque arbre couvre ~21 colonnes de feuillage.
            let coverage = (trees as f32 * 21.0 / tested as f32).min(1.0);
            color = mix(color, color::LEAVES, coverage * 1.6);
            side_top = mix(side_top, color::LEAVES, coverage * 1.6);
            if coverage > 0.35 {
                top += 5;
            }
        }
    }
    Column {
        top,
        color,
        side_top,
        side_bottom,
    }
}

fn tree_edge_ok(x: i32, z: i32) -> bool {
    let edge = 2..CHUNK_SIZE as i32 - 2;
    edge.contains(&x.rem_euclid(CHUNK_SIZE as i32))
        && edge.contains(&z.rem_euclid(CHUNK_SIZE as i32))
}

/// Arbre sur la colonne (x, z) ? Teste d'abord le hasard (rapide), puis la hauteur.
fn tree_here(seed: u32, x: i32, z: i32) -> Option<(i32, i32)> {
    if !tree_edge_ok(x, z) || rand01(seed.wrapping_add(20), x, 0, z) > crate::gen::TREE_CHANCE {
        return None;
    }
    let h = surface_height(seed, x, z);
    tree_at(seed, x, z, h).map(|t| (h, t))
}

struct QuadBuilder<'a> {
    out: &'a mut MeshData,
    sink: f32,
}

impl QuadBuilder<'_> {
    /// Quad de la face `face` : coins `base`, `base+u·du`, `base+u·du+v·dv`, `base+v·dv`.
    /// `ao[i]` dans le même ordre ; `block` donne la texture.
    fn quad(
        &mut self,
        face: usize,
        base: [f32; 3],
        du: f32,
        dv: f32,
        block: BlockId,
        ao: [u32; 4],
    ) {
        let (_, u, v) = FACES[face];
        let start = self.out.vertices.len() as u32;
        let corners = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)];
        for (i, (a, c)) in corners.into_iter().enumerate() {
            let p = [
                base[0] + a * du * u[0] as f32 + c * dv * v[0] as f32,
                base[1] + a * du * u[1] as f32 + c * dv * v[1] as f32 - self.sink,
                base[2] + a * du * u[2] as f32 + c * dv * v[2] as f32,
            ];
            self.out.vertices.push(Vertex {
                pos: p,
                data: Vertex::lod_data(face as u32, ao[i], block),
                tex: crate::mesh::face_tex(block, face as u32),
            });
        }
        self.out
            .indices
            .extend_from_slice(&[0, 1, 2, 2, 3, 0].map(|i| start + i));
    }
}

/// Maillage d'une tuile LOD (niveau ≥ 1).
pub fn build_lod_mesh(seed: u32, node: Node, surface: &SurfaceMap, gen: Generator) -> MeshData {
    let n = NODE_COLUMNS;
    let step = node.step();
    let (ox, oz) = node.origin();
    let w = (n + 2) as usize;
    let mut cols = Vec::with_capacity(w * w);
    for j in -1..=n {
        for i in -1..=n {
            cols.push(sample_column(
                seed,
                surface,
                gen,
                ox + i * step,
                oz + j * step,
                step,
            ));
        }
    }
    let col = |i: i32, j: i32| cols[((j + 1) as usize) * w + (i + 1) as usize];

    let mut out = MeshData::default();
    let mut q = QuadBuilder {
        out: &mut out,
        sink: 0.04 * node.level as f32,
    };
    let s = step as f32;

    // Dessus : colonnes voisines identiques fusionnées en bandes le long de X.
    for j in 0..n {
        let mut i = 0;
        while i < n {
            let c = col(i, j);
            let mut end = i + 1;
            while end < n && col(end, j).top == c.top && col(end, j).color == c.color {
                end += 1;
            }
            if c.top <= 0 {
                i = end;
                continue;
            }
            let base = [(ox + i * step) as f32, c.top as f32, (oz + j * step) as f32];
            q.quad(2, base, s, (end - i) as f32 * s, c.color, [3; 4]);
            i = end;
        }
    }

    // Murs vers les voisins plus bas (+ jupe sur les bords de tuile).
    for j in 0..n {
        for i in 0..n {
            let c = col(i, j);
            let (x0, z0) = ((ox + i * step) as f32, (oz + j * step) as f32);
            let (x1, z1) = (x0 + s, z0 + s);
            for (face, di, dj) in [(0usize, 1, 0), (1, -1, 0), (4, 0, 1), (5, 0, -1)] {
                let nb = col(i + di, j + dj);
                let border = !(0..n).contains(&(i + di)) || !(0..n).contains(&(j + dj));
                let mut bottom = nb.top;
                if border {
                    bottom = bottom.min(c.top) - step;
                }
                let bottom = bottom.max(0);
                if bottom >= c.top {
                    continue;
                }
                let (y0, y1) = (bottom as f32, c.top as f32);
                let hgt = y1 - y0;
                let wall = c.top - bottom;
                let b = if wall > 4 {
                    c.side_bottom
                } else if wall > 1 && c.side_top == BlockId::GRASS {
                    BlockId::DIRT
                } else {
                    c.side_top
                };
                // Ordre des coins selon (u, v) de chaque face : voir mesh::FACES.
                match face {
                    0 => q.quad(0, [x1, y0, z0], hgt, s, b, [2, 3, 3, 2]), // u=Y v=Z
                    1 => q.quad(1, [x0, y0, z0], s, hgt, b, [2, 2, 3, 3]), // u=Z v=Y
                    4 => q.quad(4, [x0, y0, z1], s, hgt, b, [2, 2, 3, 3]), // u=X v=Y
                    _ => q.quad(5, [x0, y0, z0], hgt, s, b, [2, 3, 3, 2]), // u=Y v=X
                }
            }
        }
    }
    out.sort_by_face();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> LodSettings {
        LodSettings {
            detail: 160.0,
            max_distance: 3000.0,
        }
    }

    #[test]
    fn selection_does_not_overlap_and_has_detail_near_player() {
        let nodes = select(10.0, -20.0, &settings());
        for (i, a) in nodes.iter().enumerate() {
            for b in &nodes[i + 1..] {
                assert!(!a.overlaps(*b), "{a:?} recouvre {b:?}");
            }
        }
        let here = Node::containing(0, 10, -20);
        assert!(nodes.contains(&here));
        assert!(nodes.iter().any(|n| n.level >= 4));
        // Tout ce qui est près du joueur est en pleine résolution.
        for n in &nodes {
            if n.distance(10.0, -20.0) < 100.0 {
                assert_eq!(n.level, 0);
            }
        }
    }

    #[test]
    fn selection_covers_the_ground_without_holes() {
        let (px, pz) = (10.0, -20.0);
        let nodes = select(px, pz, &settings());
        // Chaque bloc échantillonné à moins de 2500 blocs est couvert par exactement un nœud.
        for i in -50..=50 {
            for j in -50..=50 {
                let (x, z) = (px as i32 + i * 50, pz as i32 + j * 50);
                if ((x as f32 - px).powi(2) + (z as f32 - pz).powi(2)).sqrt() > 2500.0 {
                    continue;
                }
                let covering = nodes
                    .iter()
                    .filter(|n| {
                        let (ox, oz) = n.origin();
                        (ox..ox + n.size()).contains(&x) && (oz..oz + n.size()).contains(&z)
                    })
                    .count();
                assert_eq!(covering, 1, "bloc ({x}, {z}) couvert {covering} fois");
            }
        }
    }

    #[test]
    fn selection_without_lod_is_only_detail() {
        let s = LodSettings {
            detail: 128.0,
            max_distance: 0.0,
        };
        let nodes = select(0.0, 0.0, &s);
        assert!(!nodes.is_empty());
        assert!(nodes.iter().all(|n| n.level == 0));
    }

    #[test]
    fn node_geometry() {
        let n = Node {
            level: 2,
            x: -1,
            z: 3,
        };
        assert_eq!(n.size(), 128);
        assert_eq!(n.step(), 4);
        assert_eq!(n.origin(), (-128, 384));
        assert_eq!(Node::containing(2, -1, 400), n);
        assert!(n.children().iter().all(|c| n.overlaps(*c)));
        assert_eq!(
            Node {
                level: 0,
                x: 0,
                z: 0
            }
            .chunks()
            .count(),
            4
        );
    }

    #[test]
    fn lod_mesh_is_built_and_flagged() {
        let m = build_lod_mesh(
            20_261_108,
            Node {
                level: 3,
                x: 2,
                z: -1,
            },
            &SurfaceMap::new(),
            Generator::Procedural,
        );
        assert!(!m.is_empty());
        assert!(m.vertices.iter().all(|v| v.is_lod()));
        assert_eq!(m.indices.len() % 6, 0);
    }

    #[test]
    fn lod_heights_match_terrain() {
        let seed = 5;
        let node = Node {
            level: 1,
            x: 4,
            z: 4,
        };
        let m = build_lod_mesh(seed, node, &SurfaceMap::new(), Generator::Procedural);
        let (ox, oz) = node.origin();
        // Le dessus d'une colonne sans arbre est à surface + 1 (moins le léger enfoncement).
        let h = surface_height(seed, ox + 1, oz + 1) as f32 + 1.0 - 0.04;
        let corner = m
            .vertices
            .iter()
            .find(|v| v.face() == 2 && v.pos[0] == ox as f32 && v.pos[2] == oz as f32)
            .expect("coin de la première colonne");
        assert!(corner.pos[1] >= h - 1e-3, "{} < {h}", corner.pos[1]);
    }
}
