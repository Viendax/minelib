//! Données du monde : indépendantes du rendu et de la plateforme.
//!
//! - [`block`]   : types de blocs et leurs propriétés.
//! - [`Chunk`]   : colonne de 16 × 256 × 16 blocs.
//! - [`gen`]     : génération procédurale du terrain.
//! - [`mesh`]    : construction du maillage d'un chunk (faces cachées supprimées, AO).
//! - [`World`]   : ensemble des chunks chargés, accès en coordonnées monde.
//! - [`raycast`] : lancer de rayon voxel (visée, casse/pose de blocs).

pub mod block;
pub mod flame;
pub mod gen;
pub mod lod;
pub mod mesh;
pub mod noise;
pub mod raycast;
pub mod registry;
mod world;

pub use block::BlockId;
pub use world::{Light, Torch, World, LIGHT_RADIUS};

pub const CHUNK_SIZE: usize = 16;
pub const CHUNK_HEIGHT: usize = 256;

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct ChunkPos {
    pub x: i32,
    pub z: i32,
}

impl ChunkPos {
    /// Chunk qui contient la colonne monde (`wx`, `wz`).
    pub fn containing(wx: i32, wz: i32) -> Self {
        Self {
            x: wx.div_euclid(CHUNK_SIZE as i32),
            z: wz.div_euclid(CHUNK_SIZE as i32),
        }
    }

    /// Coordonnée monde du coin (x, z) minimal du chunk.
    pub fn origin(self) -> (i32, i32) {
        (self.x * CHUNK_SIZE as i32, self.z * CHUNK_SIZE as i32)
    }

    pub fn neighbors4(self) -> [ChunkPos; 4] {
        [
            ChunkPos {
                x: self.x + 1,
                z: self.z,
            },
            ChunkPos {
                x: self.x - 1,
                z: self.z,
            },
            ChunkPos {
                x: self.x,
                z: self.z + 1,
            },
            ChunkPos {
                x: self.x,
                z: self.z - 1,
            },
        ]
    }
}

#[derive(Clone)]
pub struct Chunk {
    pub pos: ChunkPos,
    blocks: Vec<BlockId>,
    /// Plus haute couche contenant au moins un bloc non-air (+1). Accélère le maillage.
    top: usize,
    /// Plus basse couche contenant un bloc non-air (`CHUNK_HEIGHT` si aucune). Comme `top`,
    /// borne prudente : ne remonte pas quand un bloc est retiré. Sous elle, que du vide.
    bottom: usize,
    /// Index des blocs qui émettent de la lumière (pierre lumineuse, lanternes…).
    emitters: Vec<u32>,
}

impl Chunk {
    pub fn new(pos: ChunkPos) -> Self {
        Self {
            pos,
            blocks: vec![BlockId::AIR; CHUNK_SIZE * CHUNK_SIZE * CHUNK_HEIGHT],
            top: 0,
            bottom: CHUNK_HEIGHT,
            emitters: Vec::new(),
        }
    }

    #[inline]
    fn index(x: usize, y: usize, z: usize) -> usize {
        (y * CHUNK_SIZE + z) * CHUNK_SIZE + x
    }

    #[inline]
    pub fn get(&self, x: usize, y: usize, z: usize) -> BlockId {
        self.blocks[Self::index(x, y, z)]
    }

    pub fn set(&mut self, x: usize, y: usize, z: usize, id: BlockId) {
        let i = Self::index(x, y, z);
        let old = self.blocks[i];
        if old.def().light.is_some() {
            self.emitters.retain(|&e| e as usize != i);
        }
        if id.def().light.is_some() {
            self.emitters.push(i as u32);
        }
        self.blocks[i] = id;
        if id != BlockId::AIR {
            self.top = self.top.max(y + 1);
            self.bottom = self.bottom.min(y);
        }
    }

    /// Blocs lumineux : (x, y, z) locaux et bloc.
    pub fn emitters(&self) -> impl Iterator<Item = ([usize; 3], BlockId)> + '_ {
        self.emitters.iter().map(|&i| {
            let i = i as usize;
            (
                [
                    i % CHUNK_SIZE,
                    i / (CHUNK_SIZE * CHUNK_SIZE),
                    (i / CHUNK_SIZE) % CHUNK_SIZE,
                ],
                self.blocks[i],
            )
        })
    }

    /// Borne supérieure (exclue) des couches non vides.
    pub fn top(&self) -> usize {
        self.top
    }

    /// Plus basse couche non vide (`CHUNK_HEIGHT` : chunk vide). En dessous : que de l'air.
    pub fn bottom(&self) -> usize {
        self.bottom.min(self.top)
    }

    /// Compression par plages : suite de (bloc u16, nombre u16) en petit-boutiste,
    /// dans l'ordre x, z, y (index = x + 16 z + 256 y).
    pub fn to_rle(&self) -> Vec<u8> {
        let mut out = Vec::new();
        let mut i = 0;
        let n = self.blocks.len();
        while i < n {
            let id = self.blocks[i];
            let mut run = 1;
            while i + run < n && self.blocks[i + run] == id && run < u16::MAX as usize {
                run += 1;
            }
            out.extend_from_slice(&id.0.to_le_bytes());
            out.extend_from_slice(&(run as u16).to_le_bytes());
            i += run;
        }
        out
    }

    pub fn from_rle(pos: ChunkPos, data: &[u8]) -> Option<Chunk> {
        let mut c = Chunk::new(pos);
        let mut i = 0usize;
        for pair in data.chunks_exact(4) {
            let id = BlockId(u16::from_le_bytes([pair[0], pair[1]]));
            let run = u16::from_le_bytes([pair[2], pair[3]]) as usize;
            if i + run > c.blocks.len() {
                return None;
            }
            c.blocks[i..i + run].fill(id);
            if id.def().light.is_some() {
                c.emitters.extend((i..i + run).map(|k| k as u32));
            }
            if id != BlockId::AIR {
                c.top = c.top.max((i + run - 1) / (CHUNK_SIZE * CHUNK_SIZE) + 1);
                c.bottom = c.bottom.min(i / (CHUNK_SIZE * CHUNK_SIZE));
            }
            i += run;
        }
        (i == c.blocks.len()).then_some(c)
    }

    /// Plus haut bloc non-air de chaque colonne : (y, bloc), index x + 16 z.
    pub fn surface(&self) -> Box<[[u16; 2]; 256]> {
        let mut out = Box::new([[0u16; 2]; 256]);
        for z in 0..CHUNK_SIZE {
            for x in 0..CHUNK_SIZE {
                if let Some(y) = (0..self.top)
                    .rev()
                    .find(|&y| self.get(x, y, z) != BlockId::AIR && !self.get(x, y, z).is_torch())
                {
                    out[x + 16 * z] = [y as u16, self.get(x, y, z).0];
                }
            }
        }
        out
    }
}

/// Surface des chunks importés (pour le LOD) : chunk → (y, bloc) par colonne.
pub type SurfaceMap = std::collections::HashMap<ChunkPos, Box<[[u16; 2]; 256]>>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_then_get() {
        let mut c = Chunk::new(ChunkPos { x: 0, z: 0 });
        c.set(3, 64, 5, BlockId(1));
        assert_eq!(c.get(3, 64, 5), BlockId(1));
        assert_eq!(c.get(0, 0, 0), BlockId::AIR);
        assert_eq!(c.top(), 65);
    }

    #[test]
    fn rle_roundtrip() {
        let mut c = Chunk::new(ChunkPos { x: 2, z: -3 });
        for x in 0..16 {
            c.set(x, 0, 3, BlockId::BEDROCK);
            c.set(x, 70, x, BlockId::STONE);
        }
        let d = Chunk::from_rle(c.pos, &c.to_rle()).unwrap();
        assert_eq!(d.blocks, c.blocks);
        assert_eq!(d.top(), 71);
        assert!(c.to_rle().len() < 400);
        assert_eq!(d.surface()[5 + 16 * 5], [70, BlockId::STONE.0]);
        assert!(Chunk::from_rle(c.pos, &[1, 0, 5, 0]).is_none());
    }

    #[test]
    fn chunk_pos_negative() {
        assert_eq!(ChunkPos::containing(-1, -16), ChunkPos { x: -1, z: -1 });
        assert_eq!(ChunkPos::containing(-17, 15), ChunkPos { x: -2, z: 0 });
        assert_eq!(ChunkPos::containing(16, 0), ChunkPos { x: 1, z: 0 });
    }
}
