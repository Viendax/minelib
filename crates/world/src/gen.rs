//! Génération procédurale du terrain.
//!
//! Hauteur = continents (basse fréquence) + collines + montagnes « en crête »
//! masquées par les continents. Grottes en « spaghetti » (intersection de deux
//! iso-surfaces de bruit 3D). Arbres posés au hasard sur l'herbe.

use crate::lap::LAP;
use crate::noise::{fbm2, fbm2_lap, rand01, value3, value3_lap};
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

/// (v309) À partir de cette distance du centre du monde (sur x ou sur z), le relief et les
/// grottes sont calculés en double précision : en f32, la case de bruit des plus petites
/// collines ne valait plus qu'au huitième près vers 8 millions de blocs, et au-delà de
/// 16 777 216 deux colonnes voisines devenaient la même (le relief tombait en marches et en
/// bandes). En deçà, exactement le même calcul qu'avant : les mondes existants ne changent
/// pas. Sur la limite, les deux calculs ne diffèrent que d'un bloc ici ou là.
pub const FAR: i32 = 1 << 22;

/// La colonne (`wx`, `wz`) est-elle assez loin pour le relief en double précision ?
fn far(wx: i32, wz: i32) -> bool {
    wx.unsigned_abs() >= FAR as u32 || wz.unsigned_abs() >= FAR as u32
}

/// Hauteur (y du bloc de surface) pour la colonne monde (`wx`, `wz`).
pub fn surface_height(seed: u32, wx: i32, wz: i32) -> i32 {
    if far(wx, wz) {
        return surface_height_64(seed, wx, wz);
    }
    let (x, z) = (wx as f32, wz as f32);
    let continent = fbm2(seed, x / 320.0, z / 320.0, 3);
    let hills = fbm2(seed.wrapping_add(1), x / 70.0, z / 70.0, 5);
    let ridge = 1.0 - fbm2(seed.wrapping_add(2), x / 140.0, z / 140.0, 4).abs();
    let mask = ((continent + 0.1) * 2.0).clamp(0.0, 1.0);
    let h = 50.0 + continent * 18.0 + hills * 9.0 + ridge * ridge * ridge * 70.0 * mask;
    (h as i32).clamp(4, CHUNK_HEIGHT as i32 - 16)
}

/// (v309) Cases de bruit de chaque couche du relief et des grottes sur un tour de la carte
/// bouclée (`lap`) : un nombre exact (et pair) pour chacune.
const CONTINENT_CELLS: i64 = LAP / 320;
const HILLS_CELLS: i64 = LAP / 70;
const RIDGE_CELLS: i64 = LAP / 140;
const CAVE_CELLS: i64 = LAP / 28;
const _: () = assert!(
    LAP % 320 == 0
        && LAP % 140 == 0
        && LAP % 70 == 0
        && LAP % 28 == 0
        && CONTINENT_CELLS % 2 == 0
        && HILLS_CELLS % 2 == 0
        && RIDGE_CELLS % 2 == 0
        && CAVE_CELLS % 2 == 0
);

/// (v309) `surface_height` loin du centre (voir `FAR`) : le même relief, en double précision.
/// (v309) Ses bruits bouclent avec la carte (`lap`) : de part et d'autre de la limite, le
/// relief continue sans raccord ; dans la carte, exactement le même qu'avant.
fn surface_height_64(seed: u32, wx: i32, wz: i32) -> i32 {
    let (x, z) = (wx as f64, wz as f64);
    let continent = fbm2_lap(seed, x / 320.0, z / 320.0, 3, CONTINENT_CELLS);
    let hills = fbm2_lap(seed.wrapping_add(1), x / 70.0, z / 70.0, 5, HILLS_CELLS);
    let ridge = 1.0 - fbm2_lap(seed.wrapping_add(2), x / 140.0, z / 140.0, 4, RIDGE_CELLS).abs();
    let mask = ((continent + 0.1) * 2.0).clamp(0.0, 1.0);
    let h = 50.0 + continent * 18.0 + hills * 9.0 + ridge * ridge * ridge * 70.0 * mask;
    (h as i32).clamp(4, CHUNK_HEIGHT as i32 - 16)
}

fn is_cave(seed: u32, wx: i32, y: i32, wz: i32) -> bool {
    if far(wx, wz) {
        let (x, y, z) = (wx as f64 / 28.0, y as f64 / 18.0, wz as f64 / 28.0);
        let a = value3_lap(seed.wrapping_add(10), x, y, z, CAVE_CELLS);
        if a.abs() > 0.07 {
            return false;
        }
        return value3_lap(seed.wrapping_add(11), x, y, z, CAVE_CELLS).abs() < 0.07;
    }
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
            let under = if h <= SAND_LEVEL {
                BlockId::SAND
            } else {
                BlockId::DIRT
            };

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
/// du tronc.
pub fn tree_at(seed: u32, wx: i32, wz: i32, h: i32) -> Option<i32> {
    let edge = 2..CHUNK_SIZE as i32 - 2;
    let (lx, lz) = (
        wx.rem_euclid(CHUNK_SIZE as i32),
        wz.rem_euclid(CHUNK_SIZE as i32),
    );
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

    /// (v309) « Aucun problème jusqu'à 16,7 millions » : très loin du centre, le relief a
    /// la même allure qu'au centre (pentes de bloc en bloc pareilles, pas de marches ni de
    /// bandes), les grottes aussi, et il continue sans cassure sur la limite `FAR`.
    #[test]
    fn far_away_the_relief_stays_a_relief() {
        // Pente moyenne entre colonnes voisines (et nombre de hauteurs différentes) le long
        // de lignes de 2000 colonnes.
        let slope = |cx: i32, cz: i32| {
            let h: Vec<i32> = (0..2000).map(|i| surface_height(5, cx + i, cz + i / 3)).collect();
            let mean = h.windows(2).map(|w| (w[1] - w[0]).abs()).sum::<i32>() as f32 / 1999.0;
            let distinct = h.iter().collect::<std::collections::HashSet<_>>().len();
            (mean, distinct)
        };
        let (near, _) = slope(-1000, 2000);
        for &(cx, cz) in &[(16_700_000, 16_700_000), (-16_777_216, 3), (8_400_000, -9_000_000)]
        {
            let (far_slope, distinct) = slope(cx, cz);
            assert!(
                far_slope > near * 0.4 && far_slope < near * 2.5,
                "pente moyenne {far_slope} en {cx} {cz} (au centre : {near})"
            );
            assert!(distinct > 8, "relief plat en {cx} {cz}");
            // Le calcul en double précision, colonne par colonne (pas de bandes : au-delà de
            // 16 777 216, deux colonnes voisines ne sont plus la même colonne).
            for i in 0..50 {
                assert_eq!(surface_height(5, cx + i, cz), surface_height_64(5, cx + i, cz));
            }
        }
        // Sur la limite : d'un côté le calcul d'avant, de l'autre le nouveau, sans marche.
        for z in [1234, -77, 900_000] {
            let line: Vec<i32> = (-3..3).map(|i| surface_height(5, FAR + i, z)).collect();
            let old: Vec<i32> = (-3..3).map(|i| surface_height_64(5, FAR + i, z)).collect();
            let worst = line.iter().zip(&old).map(|(a, b)| (a - b).abs()).max().unwrap();
            assert!(worst <= 1, "cassure de {worst} blocs sur la limite ({line:?} / {old:?})");
        }
        // En deçà, rien ne change (mondes existants).
        assert!(!far(FAR - 1, -(FAR - 1)) && far(FAR, 0) && far(0, -FAR));
        // Des grottes loin aussi.
        let caves = (0..64)
            .flat_map(|x| (8..60).map(move |y| (x, y)))
            .filter(|&(x, y)| is_cave(5, 16_700_000 + x * 3, y, 16_700_000))
            .count();
        assert!(caves > 0);
    }

    /// (v309) La carte bouclée (`lap`) : passé la limite (une colonne prise au-delà, sans la
    /// ramener sur la carte), le relief et les grottes de l'autre bord, sur x comme sur z ; de
    /// part et d'autre, deux voisines ordinaires (pas de marche plus haute que sur le reste de
    /// la ligne) ; et dans la carte, exactement le relief d'avant (le même calcul sans boucle).
    #[test]
    fn the_map_comes_back_around_without_a_seam() {
        use crate::lap::HALF;
        use crate::noise::{fbm2_64, value3_64};
        let h = HALF as i32;
        // Le calcul d'avant (sans boucle).
        let before = |seed: u32, wx: i32, wz: i32| -> i32 {
            let (x, z) = (wx as f64, wz as f64);
            let continent = fbm2_64(seed, x / 320.0, z / 320.0, 3);
            let hills = fbm2_64(seed.wrapping_add(1), x / 70.0, z / 70.0, 5);
            let ridge = 1.0 - fbm2_64(seed.wrapping_add(2), x / 140.0, z / 140.0, 4).abs();
            let mask = ((continent + 0.1) * 2.0).clamp(0.0, 1.0);
            let h = 50.0 + continent * 18.0 + hills * 9.0 + ridge * ridge * ridge * 70.0 * mask;
            (h as i32).clamp(4, CHUNK_HEIGHT as i32 - 16)
        };
        for seed in [5u32, 42, 4242] {
            for z in [-h, -h + 1, -123_456, 0, 777, 16_777_216, h - 1] {
                for d in 0..40 {
                    assert_eq!(surface_height(seed, h + d, z), surface_height(seed, -h + d, z));
                    assert_eq!(
                        surface_height(seed, -h - 1 - d, z),
                        surface_height(seed, h - 1 - d, z)
                    );
                    // Sur z aussi.
                    assert_eq!(surface_height(seed, z, h + d), surface_height(seed, z, -h + d));
                }
                // Une ligne qui passe la limite : la marche sur la limite n'est pas plus haute
                // que les autres.
                let line: Vec<i32> = (-600..600).map(|i| surface_height(seed, h + i, z)).collect();
                let steps: Vec<i32> = line.windows(2).map(|w| (w[1] - w[0]).abs()).collect();
                let seam = steps[599];
                let worst = steps.iter().enumerate().filter(|(i, _)| *i != 599).map(|(_, s)| *s);
                assert!(
                    seam <= worst.max().unwrap().max(1),
                    "marche de {seam} blocs sur la limite (graine {seed}, z {z})"
                );
                // Dans la carte, loin de la dernière case de bruit (sur x et sur z) : le relief
                // d'avant.
                if z.unsigned_abs() > (h - 400) as u32 {
                    continue;
                }
                for i in 0..60 {
                    let x = h - 2_000 - i * 37;
                    assert_eq!(surface_height(seed, x, z), before(seed, x, z), "{x} {z}");
                    let x = -h + 2_000 + i * 37;
                    assert_eq!(surface_height(seed, x, z), before(seed, x, z), "{x} {z}");
                }
            }
            // Les grottes aussi reviennent.
            for d in 0..30 {
                for y in 8..60 {
                    assert_eq!(is_cave(seed, h + d, y, 99), is_cave(seed, -h + d, y, 99));
                }
            }
            // Et dans la carte, ce sont les mêmes qu'avant.
            for i in 0..40 {
                let x = h - 1_000 - i * 11;
                let (fx, fz) = (x as f64 / 28.0, 777.0 / 28.0);
                let old = value3_64(seed.wrapping_add(10), fx, 30.0 / 18.0, fz);
                let new = crate::noise::value3_lap(seed.wrapping_add(10), fx, 30.0 / 18.0, fz, CAVE_CELLS);
                assert_eq!(old, new);
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
