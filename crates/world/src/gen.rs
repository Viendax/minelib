//! Génération procédurale du terrain.
//!
//! Hauteur = continents (basse fréquence) + collines + montagnes « en crête »
//! masquées par les continents. Grottes en « spaghetti » (intersection de deux
//! iso-surfaces de bruit 3D). Arbres posés au hasard sur l'herbe.

use crate::noise::{fbm2, rand01, value3};
use crate::{BlockId, Chunk, ChunkPos, CHUNK_HEIGHT, CHUNK_SIZE};

/// Sous ce niveau, la surface devient du sable.
pub const SAND_LEVEL: i32 = 40;
/// Au-dessus de ce niveau, la surface est enneigée.
pub const SNOW_LEVEL: i32 = 112;
/// Probabilité qu'un arbre pousse sur une colonne d'herbe éligible.
pub const TREE_CHANCE: f32 = 0.006;

/// Façon de générer les chunks qui ne viennent pas d'une sauvegarde importée.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum Generator {
    /// Terrain procédural Minelib.
    #[default]
    Procedural,
    /// Monde plat (socle, 2 terre, herbe), comme les cartes Minecraft « superflat ».
    Flat,
    /// Rien : seulement les chunks importés.
    Void,
}

impl Generator {
    pub fn name(self) -> &'static str {
        match self {
            Generator::Procedural => "procedural",
            Generator::Flat => "flat",
            Generator::Void => "void",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s.trim() {
            "flat" => Generator::Flat,
            "void" => Generator::Void,
            _ => Generator::Procedural,
        }
    }

    /// Hauteur du dessus du sol plat (y du bloc d'herbe + 1).
    pub const FLAT_TOP: i32 = 4;
}

/// Génère un chunk selon le générateur choisi.
pub fn generate(g: Generator, seed: u32, pos: ChunkPos) -> Chunk {
    match g {
        Generator::Procedural => generate_chunk(seed, pos),
        Generator::Void => Chunk::new(pos),
        Generator::Flat => {
            let mut c = Chunk::new(pos);
            for z in 0..CHUNK_SIZE {
                for x in 0..CHUNK_SIZE {
                    c.set(x, 0, z, BlockId::BEDROCK);
                    c.set(x, 1, z, BlockId::DIRT);
                    c.set(x, 2, z, BlockId::DIRT);
                    c.set(x, 3, z, BlockId::GRASS);
                }
            }
            c
        }
    }
}

/// Hauteur (y du bloc de surface) pour la colonne monde (`wx`, `wz`).
pub fn surface_height(seed: u32, wx: i32, wz: i32) -> i32 {
    let (x, z) = (wx as f32, wz as f32);
    let continent = fbm2(seed, x / 320.0, z / 320.0, 3);
    let hills = fbm2(seed.wrapping_add(1), x / 70.0, z / 70.0, 5);
    let ridge = 1.0 - fbm2(seed.wrapping_add(2), x / 140.0, z / 140.0, 4).abs();
    let mask = ((continent + 0.1) * 2.0).clamp(0.0, 1.0);
    let h = 50.0 + continent * 18.0 + hills * 9.0 + ridge * ridge * ridge * 70.0 * mask;
    (h as i32).clamp(4, CHUNK_HEIGHT as i32 - 16)
}

fn is_cave(seed: u32, wx: i32, y: i32, wz: i32) -> bool {
    let (x, y, z) = (wx as f32 / 28.0, y as f32 / 18.0, wz as f32 / 28.0);
    let a = value3(seed.wrapping_add(10), x, y, z);
    if a.abs() > 0.07 {
        return false;
    }
    value3(seed.wrapping_add(11), x, y, z).abs() < 0.07
}

#[allow(clippy::needless_range_loop)] // indices locaux et monde utilisés ensemble
pub fn generate_chunk(seed: u32, pos: ChunkPos) -> Chunk {
    let mut chunk = Chunk::new(pos);
    let (ox, oz) = pos.origin();
    let mut heights = [[0i32; CHUNK_SIZE]; CHUNK_SIZE];

    for lz in 0..CHUNK_SIZE {
        for lx in 0..CHUNK_SIZE {
            let (wx, wz) = (ox + lx as i32, oz + lz as i32);
            let h = surface_height(seed, wx, wz);
            heights[lz][lx] = h;
            let top = if h <= SAND_LEVEL {
                BlockId::SAND
            } else if h >= SNOW_LEVEL {
                BlockId::SNOW
            } else {
                BlockId::GRASS
            };
            let under = if h <= SAND_LEVEL { BlockId::SAND } else { BlockId::DIRT };

            for y in 0..=h {
                let id = if y == 0 {
                    BlockId::BEDROCK
                } else if y == h {
                    top
                } else if y >= h - 3 {
                    under
                } else {
                    BlockId::STONE
                };
                // Les grottes ne percent ni le socle ni la toute première couche de surface.
                if y > 4 && y < h - 1 && is_cave(seed, wx, y, wz) {
                    continue;
                }
                chunk.set(lx, y as usize, lz, id);
            }
        }
    }

    // Arbres : uniquement loin des bords pour rester dans le chunk.
    for lz in 2..CHUNK_SIZE - 2 {
        for lx in 2..CHUNK_SIZE - 2 {
            let h = heights[lz][lx];
            let (wx, wz) = (ox + lx as i32, oz + lz as i32);
            if let Some(trunk) = tree_at(seed, wx, wz, h) {
                place_tree(&mut chunk, lx as i32, h + 1, lz as i32, trunk);
            }
        }
    }
    chunk
}

/// Un arbre pousse-t-il sur la colonne (`wx`, `wz`) de hauteur `h` ? Renvoie la hauteur
/// du tronc. Partagé par la génération des chunks et celle des LOD lointains.
pub fn tree_at(seed: u32, wx: i32, wz: i32, h: i32) -> Option<i32> {
    let edge = 2..CHUNK_SIZE as i32 - 2;
    let (lx, lz) = (wx.rem_euclid(CHUNK_SIZE as i32), wz.rem_euclid(CHUNK_SIZE as i32));
    if !edge.contains(&lx) || !edge.contains(&lz) || h <= SAND_LEVEL || h >= SNOW_LEVEL {
        return None;
    }
    if rand01(seed.wrapping_add(20), wx, 0, wz) > TREE_CHANCE {
        return None;
    }
    Some(4 + (rand01(seed.wrapping_add(21), wx, 0, wz) * 3.0) as i32)
}

fn place_tree(chunk: &mut Chunk, x: i32, y: i32, z: i32, trunk: i32) {
    let top = y + trunk;
    if top + 2 >= CHUNK_HEIGHT as i32 {
        return;
    }
    for dy in -2..=1i32 {
        let r: i32 = if dy >= 0 { 1 } else { 2 };
        for dz in -r..=r {
            for dx in -r..=r {
                // Coins arrondis.
                if dx.abs() == r && dz.abs() == r && r > 1 {
                    continue;
                }
                let (lx, ly, lz) = ((x + dx) as usize, (top + dy) as usize, (z + dz) as usize);
                if chunk.get(lx, ly, lz) == BlockId::AIR {
                    chunk.set(lx, ly, lz, BlockId::LEAVES);
                }
            }
        }
    }
    for ty in y..top {
        chunk.set(x as usize, ty as usize, z as usize, BlockId::WOOD);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generation_is_deterministic() {
        let p = ChunkPos { x: 3, z: -2 };
        let a = generate_chunk(7, p);
        let b = generate_chunk(7, p);
        for y in 0..CHUNK_HEIGHT {
            for z in 0..CHUNK_SIZE {
                for x in 0..CHUNK_SIZE {
                    assert_eq!(a.get(x, y, z), b.get(x, y, z));
                }
            }
        }
    }

    #[test]
    fn bedrock_and_surface() {
        let c = generate_chunk(1, ChunkPos { x: 0, z: 0 });
        assert_eq!(c.get(5, 0, 5), BlockId::BEDROCK);
        let h = surface_height(1, 5, 5) as usize;
        assert!(c.get(5, h, 5) != BlockId::AIR);
        assert!(c.top() > h);
    }
}
