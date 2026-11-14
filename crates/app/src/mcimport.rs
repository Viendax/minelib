//! Import des mondes Minecraft (Java Edition, format « Anvil ») déposés dans `saves/`.
//!
//! Lecture seule : les fichiers d'origine (`level.dat`, `region/*.mca`) ne sont jamais
//! modifiés. Le monde est converti en mémoire ; la sauvegarde suivante l'écrit au format
//! Minelib dans le même dossier (`level.txt`, `chunks.bin`, `edits.bin`, `torches.txt`).
//!
//! Formats de chunks pris en charge :
//! - 1.18 et plus : `sections[].block_states { palette, data }`, Y de −64 à 319 ;
//! - 1.13 à 1.17 : `Level.Sections[].{ Palette, BlockStates }` ;
//! - avant 1.13 : identifiants numériques (`Blocks`, `Add`).
//!
//! La hauteur de Minelib est de 512 blocs : pour les mondes 1.18+, Y −64..319 devient
//! 0..383 (décalage de +64) et tient en entier.

use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::Path;

use anyhow::{bail, Context, Result};
use minelib_world::registry;
use minelib_world::{BlockId, Chunk, ChunkPos, CHUNK_HEIGHT};

// ---------------------------------------------------------------- NBT

#[derive(Debug, Clone)]
#[allow(dead_code)] // tous les types NBT sont lus, même ceux que la conversion n'utilise pas
pub enum Nbt {
    Byte(i8),
    Short(i16),
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    ByteArray(Vec<i8>),
    String(String),
    List(Vec<Nbt>),
    Compound(HashMap<String, Nbt>),
    IntArray(Vec<i32>),
    LongArray(Vec<i64>),
}

impl Nbt {
    pub fn get(&self, key: &str) -> Option<&Nbt> {
        match self {
            Nbt::Compound(m) => m.get(key),
            _ => None,
        }
    }
    pub fn as_i64(&self) -> Option<i64> {
        Some(match self {
            Nbt::Byte(v) => *v as i64,
            Nbt::Short(v) => *v as i64,
            Nbt::Int(v) => *v as i64,
            Nbt::Long(v) => *v,
            _ => return None,
        })
    }
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Nbt::Float(v) => Some(*v as f64),
            Nbt::Double(v) => Some(*v),
            _ => self.as_i64().map(|v| v as f64),
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Nbt::String(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_list(&self) -> &[Nbt] {
        match self {
            Nbt::List(l) => l,
            _ => &[],
        }
    }
    /// Chemin de clés : `nbt.path(&["Data", "Player", "Pos"])`.
    pub fn path(&self, keys: &[&str]) -> Option<&Nbt> {
        keys.iter().try_fold(self, |n, k| n.get(k))
    }
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8]> {
        if self.pos + n > self.data.len() {
            bail!("NBT tronqué");
        }
        let s = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn i16(&mut self) -> Result<i16> {
        Ok(i16::from_be_bytes(self.take(2)?.try_into()?))
    }
    fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_be_bytes(self.take(4)?.try_into()?))
    }
    fn i64(&mut self) -> Result<i64> {
        Ok(i64::from_be_bytes(self.take(8)?.try_into()?))
    }
    fn len(&mut self) -> Result<usize> {
        let n = self.i32()?;
        if n < 0 || n as usize > self.data.len() * 8 {
            bail!("longueur NBT invalide");
        }
        Ok(n as usize)
    }
    fn string(&mut self) -> Result<String> {
        let n = self.i16()? as u16 as usize;
        // UTF-8 « modifié » de Java : identique à l'UTF-8 pour les noms de blocs.
        Ok(String::from_utf8_lossy(self.take(n)?).into_owned())
    }
    fn payload(&mut self, tag: u8, depth: usize) -> Result<Nbt> {
        if depth > 512 {
            bail!("NBT trop imbriqué");
        }
        Ok(match tag {
            1 => Nbt::Byte(self.u8()? as i8),
            2 => Nbt::Short(self.i16()?),
            3 => Nbt::Int(self.i32()?),
            4 => Nbt::Long(self.i64()?),
            5 => Nbt::Float(f32::from_bits(self.i32()? as u32)),
            6 => Nbt::Double(f64::from_bits(self.i64()? as u64)),
            7 => {
                let n = self.len()?;
                Nbt::ByteArray(self.take(n)?.iter().map(|&b| b as i8).collect())
            }
            8 => Nbt::String(self.string()?),
            9 => {
                let t = self.u8()?;
                let n = self.len()?;
                let mut l = Vec::with_capacity(n.min(4096));
                for _ in 0..n {
                    l.push(self.payload(t, depth + 1)?);
                }
                Nbt::List(l)
            }
            10 => {
                let mut m = HashMap::new();
                loop {
                    let t = self.u8()?;
                    if t == 0 {
                        break;
                    }
                    let name = self.string()?;
                    m.insert(name, self.payload(t, depth + 1)?);
                }
                Nbt::Compound(m)
            }
            11 => {
                let n = self.len()?;
                Nbt::IntArray((0..n).map(|_| self.i32()).collect::<Result<_>>()?)
            }
            12 => {
                let n = self.len()?;
                Nbt::LongArray((0..n).map(|_| self.i64()).collect::<Result<_>>()?)
            }
            t => bail!("tag NBT inconnu {t}"),
        })
    }
}

/// Lit un NBT non compressé (racine : compound nommé).
pub fn parse_nbt(data: &[u8]) -> Result<Nbt> {
    let mut r = Reader { data, pos: 0 };
    let tag = r.u8()?;
    if tag != 10 {
        bail!("la racine NBT n'est pas un compound");
    }
    let _name = r.string()?;
    r.payload(10, 0)
}

/// Décompresse en gardant ce qui a pu être lu si le flux est tronqué
/// (chunks abîmés : le NBT est souvent complet malgré tout).
fn read_tolerant(mut r: impl Read) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut buf = [0u8; 16384];
    loop {
        match r.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => out.extend_from_slice(&buf[..n]),
            Err(e) if out.is_empty() => return Err(e.into()),
            Err(_) => break,
        }
    }
    Ok(out)
}

fn gunzip(data: &[u8]) -> Result<Vec<u8>> {
    read_tolerant(flate2::read::GzDecoder::new(data))
}

fn unzlib(data: &[u8]) -> Result<Vec<u8>> {
    read_tolerant(flate2::read::ZlibDecoder::new(data))
}

// ---------------------------------------------------------------- level.dat

pub struct McLevel {
    pub name: String,
    pub seed: i64,
    pub data_version: i32,
    pub last_played_s: u64,
    /// Position (x, y, z) et orientation (lacet, tangage) Minecraft du joueur.
    pub player: Option<([f64; 3], [f32; 2])>,
    pub spawn: [i32; 3],
    /// `generatorName` (« flat » pour les mondes plats).
    pub generator: String,
}

pub fn is_minecraft_world(dir: &Path) -> bool {
    dir.join("level.dat").is_file() && dir.join("region").is_dir()
}

pub fn read_level(dir: &Path) -> Result<McLevel> {
    let raw = fs::read(dir.join("level.dat")).context("lecture de level.dat")?;
    let nbt = parse_nbt(&gunzip(&raw).unwrap_or(raw))?;
    let data = nbt.get("Data").context("level.dat sans Data")?;
    let seed = data
        .path(&["WorldGenSettings", "seed"])
        .or_else(|| data.get("RandomSeed"))
        .and_then(Nbt::as_i64)
        .unwrap_or(0);
    let player = data.get("Player").and_then(|p| {
        let pos: Vec<f64> = p
            .get("Pos")?
            .as_list()
            .iter()
            .filter_map(Nbt::as_f64)
            .collect();
        let rot: Vec<f64> = p
            .get("Rotation")
            .map(|r| r.as_list().iter().filter_map(Nbt::as_f64).collect())
            .unwrap_or_default();
        (pos.len() == 3).then(|| {
            (
                [pos[0], pos[1], pos[2]],
                [
                    rot.first().copied().unwrap_or(0.0) as f32,
                    rot.get(1).copied().unwrap_or(0.0) as f32,
                ],
            )
        })
    });
    let int = |k: &str| data.get(k).and_then(Nbt::as_i64).unwrap_or(0) as i32;
    Ok(McLevel {
        name: data
            .get("LevelName")
            .and_then(Nbt::as_str)
            .unwrap_or("Monde Minecraft")
            .to_owned(),
        seed,
        data_version: int("DataVersion"),
        last_played_s: (data.get("LastPlayed").and_then(Nbt::as_i64).unwrap_or(0) / 1000).max(0)
            as u64,
        player,
        spawn: [int("SpawnX"), int("SpawnY"), int("SpawnZ")],
        generator: data
            .get("generatorName")
            .and_then(Nbt::as_str)
            .unwrap_or("default")
            .to_lowercase(),
    })
}

// ---------------------------------------------------------------- blocs

/// Bloc Minelib pour un état de bloc moderne (1.13+) : nom + propriétés.
pub fn map_block_state(full: &str, props: &HashMap<String, String>) -> BlockId {
    modern_block(full, props).unwrap_or_else(|| map_block_name(full))
}

const MODERN_DYES: [&str; 16] = [
    "white",
    "orange",
    "magenta",
    "light_blue",
    "yellow",
    "lime",
    "pink",
    "gray",
    "light_gray",
    "cyan",
    "purple",
    "blue",
    "brown",
    "green",
    "red",
    "black",
];

/// Matériau Minelib pour un préfixe moderne (`stone_brick` dans `stone_brick_stairs`).
fn modern_material(m: &str) -> &'static str {
    match m {
        "oak" | "spruce" | "birch" | "jungle" | "acacia" | "dark_oak" | "mangrove" | "cherry"
        | "bamboo" | "crimson" | "warped" | "pale_oak" | "petrified_oak" => {
            registry::planks_key(registry::wood_index(m))
        }
        "stone" => "stone",
        "smooth_stone" => "smooth_stone",
        "cobblestone" | "cobbled_deepslate" => "cobblestone",
        "mossy_cobblestone" => "cobblestone_mossy",
        "stone_brick" | "deepslate_brick" | "deepslate_tile" | "tuff_brick" => "stonebrick",
        "mossy_stone_brick" => "stonebrick_mossy",
        "brick" | "mud_brick" => "brick",
        "sandstone" => "sandstone",
        "smooth_sandstone" | "cut_sandstone" => "sandstone_smooth",
        "red_sandstone" => "red_sandstone",
        "smooth_red_sandstone" | "cut_red_sandstone" => "red_sandstone_smooth",
        "quartz" | "smooth_quartz" => "quartz",
        "nether_brick" => "nether_brick",
        "red_nether_brick" => "red_nether_brick",
        "purpur" => "purpur",
        "prismarine" => "prismarine",
        "prismarine_brick" => "prismarine_bricks",
        "dark_prismarine" => "prismarine_dark",
        "granite" => "stone_granite",
        "polished_granite" => "stone_granite_smooth",
        "diorite" => "stone_diorite",
        "polished_diorite" => "stone_diorite_smooth",
        "andesite" | "tuff" | "polished_tuff" => "stone_andesite",
        "polished_andesite" | "polished_deepslate" => "stone_andesite_smooth",
        "end_stone_brick" => "end_bricks",
        "blackstone" | "polished_blackstone" | "polished_blackstone_brick" => "coal_block",
        _ => "stone",
    }
}

fn horizontal(f: Option<&str>) -> u8 {
    // Index de direction : 0 est, 1 sud, 2 ouest, 3 nord.
    match f {
        Some("south") => 1,
        Some("west") => 2,
        Some("north") => 3,
        _ => 0,
    }
}

/// Côté contre lequel est posé un objet mural regardant dans la direction `f`.
fn wall_side(f: Option<&str>) -> u8 {
    match f {
        Some("north") => 4,
        Some("south") => 5,
        Some("west") => 0,
        _ => 1,
    }
}

fn modern_block(full: &str, props: &HashMap<String, String>) -> Option<BlockId> {
    let reg = registry::registry();
    let n = full.strip_prefix("minecraft:").unwrap_or(full);
    let prop = |k: &str| props.get(k).map(String::as_str);
    let key = |k: &str| reg.by_key(k);

    // Torche murale : `facing` = côté vers lequel elle penche ; le mur est à l'opposé.
    if n == "wall_torch" || n == "soul_wall_torch" {
        let wall = match prop("facing") {
            Some("east") => 1,
            Some("west") => 0,
            Some("south") => 5,
            _ => 4,
        };
        return Some(BlockId::wall_torch(wall));
    }

    // Familles colorées.
    for (i, c) in MODERN_DYES.iter().enumerate() {
        let Some(rest) = n.strip_prefix(c).and_then(|r| r.strip_prefix('_')) else {
            continue;
        };
        let d = registry::DYES[i];
        let k = match rest {
            "wool" => format!("wool_{d}"),
            "concrete" => format!("concrete_{d}"),
            "concrete_powder" => format!("concrete_powder_{d}"),
            "terracotta" => format!("clay_{d}"),
            "glazed_terracotta" => format!("glazed_{d}"),
            "stained_glass" => format!("glass_{d}"),
            "stained_glass_pane" => format!("pane_{d}"),
            "carpet" => format!("carpet_{d}"),
            "shulker_box" => format!("shulker_{d}"),
            "wall_banner" => format!("banner_{d}:{}", wall_side(prop("facing"))),
            "banner" => format!("banner_{d}:5"),
            "bed" => "bed".into(),
            "candle" | "candle_cake" => return Some(BlockId::AIR),
            _ => continue,
        };
        return key(&k);
    }
    if let Some(m) = n.strip_suffix("_stairs") {
        let dir = match prop("facing") {
            Some("west") => 1,
            Some("south") => 2,
            Some("north") => 3,
            _ => 0,
        };
        let half = if prop("half") == Some("top") {
            "top"
        } else {
            "bottom"
        };
        return key(&format!("stairs:{}:{dir}:{half}", modern_material(m)));
    }
    if let Some(m) = n.strip_suffix("_slab") {
        let mat = modern_material(m);
        return match prop("type") {
            Some("double") => key(mat),
            Some("top") => key(&format!("slab:{mat}:top")),
            _ => key(&format!("slab:{mat}:bottom")),
        };
    }
    if n.ends_with("_fence_gate") || n.ends_with("_fence") {
        if n.starts_with("nether_brick") {
            return key("fence_nether");
        }
        let wood = n.split("_fence").next().unwrap_or("oak");
        return key(&format!(
            "fence_{}",
            registry::wood_key(registry::wood_index(wood))
        ));
    }
    if n.ends_with("_wall")
        && !n.ends_with("_banner")
        && !n.contains("sign")
        && !n.contains("torch")
        && !n.contains("head")
        && !n.contains("skull")
    {
        return key(if n.contains("mossy") {
            "wall_cobblestone_mossy"
        } else {
            "wall_cobblestone"
        });
    }
    if let Some(w) = n.strip_suffix("_door") {
        let d = if w == "iron" {
            6
        } else {
            registry::wood_index(w)
        };
        let half = if prop("half") == Some("upper") {
            "upper"
        } else {
            "lower"
        };
        let side = registry::door_side(horizontal(prop("facing")), prop("open") == Some("true"));
        return key(&format!("door_{}:{half}:{side}", registry::door_key(d)));
    }
    if let Some(w) = n.strip_suffix("_trapdoor") {
        let kind = if w == "iron" {
            "iron_trapdoor"
        } else {
            "trapdoor"
        };
        let side = if prop("open") == Some("true") {
            match prop("facing") {
                Some("north") => 4,
                Some("south") => 5,
                Some("west") => 0,
                _ => 1,
            }
        } else if prop("half") == Some("top") {
            2
        } else {
            3
        };
        return key(&format!("{kind}:{side}"));
    }
    if n.ends_with("_button") {
        let mat = if n.contains("stone") || n.contains("blackstone") {
            "button_stone"
        } else {
            "button_wood"
        };
        return match prop("face") {
            Some("floor") => key(&format!("{mat}:floor")),
            Some("ceiling") => key(&format!("{mat}:ceiling")),
            _ => key(&format!("{mat}:{}", wall_side(prop("facing")))),
        };
    }
    if n.ends_with("_wall_sign") || n.ends_with("_wall_hanging_sign") {
        return key(&format!("wall_sign:{}", wall_side(prop("facing"))));
    }
    if n.ends_with("_sign") || n.ends_with("_hanging_sign") {
        return key("sign");
    }
    if n.ends_with("_pressure_plate") {
        return key(if n.contains("stone") || n.contains("weighted") {
            "pressure_plate_stone"
        } else {
            "pressure_plate_wood"
        });
    }
    if let Some(w) = n.strip_suffix("_leaves") {
        return key(&format!(
            "leaves_{}",
            registry::wood_key(registry::wood_index(w))
        ));
    }
    if let Some(w) = n.strip_suffix("_planks") {
        return key(registry::planks_key(registry::wood_index(w)));
    }
    for suf in ["_log", "_wood", "_stem", "_hyphae"] {
        if let Some(w) = n.strip_suffix(suf) {
            let w = w.strip_prefix("stripped_").unwrap_or(w);
            return key(registry::log_key(registry::wood_index(w)));
        }
    }
    if n.starts_with("potted_") {
        return key("flower_pot");
    }
    let k = match n {
        "stone" | "infested_stone" | "deepslate" | "calcite" => "stone",
        "granite" => "stone_granite",
        "polished_granite" => "stone_granite_smooth",
        "diorite" => "stone_diorite",
        "polished_diorite" => "stone_diorite_smooth",
        "andesite" | "tuff" => "stone_andesite",
        "polished_andesite" | "polished_deepslate" => "stone_andesite_smooth",
        "smooth_stone" => "smooth_stone",
        "coarse_dirt" | "rooted_dirt" => "coarse_dirt",
        "podzol" => "podzol",
        "red_sand" => "red_sand",
        "chiseled_sandstone" => "sandstone_carved",
        "cut_sandstone" | "smooth_sandstone" => "sandstone_smooth",
        "red_sandstone" => "red_sandstone",
        "chiseled_red_sandstone" => "red_sandstone_carved",
        "cut_red_sandstone" | "smooth_red_sandstone" => "red_sandstone_smooth",
        "quartz_block" | "smooth_quartz" | "quartz_bricks" => "quartz",
        "chiseled_quartz_block" => "quartz_chiseled",
        "quartz_pillar" => "quartz_pillar",
        "stone_bricks" | "deepslate_bricks" | "deepslate_tiles" => "stonebrick",
        "mossy_stone_bricks" => "stonebrick_mossy",
        "cracked_stone_bricks" => "stonebrick_cracked",
        "chiseled_stone_bricks" => "stonebrick_carved",
        "prismarine" => "prismarine",
        "prismarine_bricks" => "prismarine_bricks",
        "dark_prismarine" => "prismarine_dark",
        "nether_bricks" => "nether_brick",
        "red_nether_bricks" => "red_nether_brick",
        "end_stone_bricks" | "end_stone" => "end_bricks",
        "purpur_block" | "purpur_pillar" => "purpur",
        "iron_block" => "iron_block",
        "gold_block" => "gold_block",
        "diamond_block" => "diamond_block",
        "emerald_block" => "emerald_block",
        "lapis_block" => "lapis_block",
        "coal_block" => "coal_block",
        "redstone_block" => "redstone_block",
        "packed_ice" | "blue_ice" => "packed_ice",
        "hay_block" => "hay",
        "bookshelf" | "chiseled_bookshelf" => "bookshelf",
        "tnt" => "tnt",
        "netherrack" => "netherrack",
        "soul_sand" | "soul_soil" => "soul_sand",
        "sponge" | "wet_sponge" => "sponge",
        "crafting_table" => "crafting_table",
        "furnace" | "blast_furnace" | "smoker" => "furnace",
        "pumpkin" | "carved_pumpkin" => "pumpkin",
        "melon" => "melon",
        "piston" | "sticky_piston" | "piston_head" => "piston",
        "cauldron" | "water_cauldron" | "lava_cauldron" => "cauldron",
        "command_block"
        | "chain_command_block"
        | "repeating_command_block"
        | "structure_block"
        | "jigsaw" => "command_block",
        "slime_block" => "slime",
        "jukebox" => "jukebox",
        "note_block" => "noteblock",
        "magma_block" => "magma",
        "bone_block" => "bone_block",
        "nether_wart_block" | "warped_wart_block" => "nether_wart_block",
        "observer" => "observer",
        "dispenser" | "dropper" => "dispenser",
        "glowstone" => "glowstone",
        "sea_lantern" => "sea_lantern",
        "jack_o_lantern" => "jack_o_lantern",
        "redstone_lamp" => {
            if prop("lit") == Some("true") {
                "lamp_on"
            } else {
                "lamp_off"
            }
        }
        "barrier" => "barrier",
        "glass" | "tinted_glass" => "glass",
        "glass_pane" => "pane",
        "iron_bars" => "iron_bars",
        "torch"
        | "wall_torch"
        | "soul_torch"
        | "soul_wall_torch"
        | "redstone_torch"
        | "redstone_wall_torch" => "torch",
        "ladder" => return key(&format!("ladder:{}", wall_side(prop("facing")))),
        "vine" => {
            let side = if prop("south") == Some("true") {
                4
            } else if prop("west") == Some("true") {
                1
            } else if prop("north") == Some("true") {
                5
            } else {
                0
            };
            return key(&format!("vine:{side}"));
        }
        "tripwire_hook" => return key(&format!("tripwire_hook:{}", wall_side(prop("facing")))),
        "snow" => {
            return key(&format!(
                "snow_layer:{}",
                prop("layers")
                    .and_then(|l| l.parse::<u8>().ok())
                    .unwrap_or(1)
                    .clamp(1, 8)
            ))
        }
        "chest" | "trapped_chest" | "ender_chest" | "barrel" => "chest",
        "flower_pot" => "flower_pot",
        "end_rod" => "end_rod",
        "daylight_detector" => "daylight_detector",
        "farmland" => "farmland",
        "dirt_path" | "grass_path" => "grass_path",
        "cactus" => "cactus",
        "cake" => "cake",
        "anvil" | "chipped_anvil" | "damaged_anvil" => "anvil",
        "enchanting_table" => "enchanting_table",
        "brewing_stand" => "brewing_stand",
        "hopper" => "hopper",
        "repeater" => "repeater",
        "comparator" => "comparator",
        "lever" => "lever",
        "rail" | "powered_rail" | "detector_rail" | "activator_rail" => "rail",
        "lily_pad" => "lily_pad",
        "skeleton_skull"
        | "player_head"
        | "zombie_head"
        | "creeper_head"
        | "dragon_head"
        | "piglin_head"
        | "wither_skeleton_skull" => "skull",
        "short_grass" | "grass" => "plant:tallgrass",
        "fern" => "plant:fern",
        "dead_bush" => "plant:deadbush",
        "dandelion" => "plant:dandelion",
        "poppy" => "plant:rose",
        "blue_orchid" => "plant:blue_orchid",
        "allium" => "plant:allium",
        "azure_bluet" => "plant:houstonia",
        "red_tulip" => "plant:tulip_red",
        "orange_tulip" => "plant:tulip_orange",
        "white_tulip" => "plant:tulip_white",
        "pink_tulip" => "plant:tulip_pink",
        "oxeye_daisy" | "cornflower" | "lily_of_the_valley" => "plant:oxeye_daisy",
        "brown_mushroom" => "plant:mushroom_brown",
        "red_mushroom" => "plant:mushroom_red",
        "sugar_cane" => "plant:reeds",
        "wheat" | "carrots" | "potatoes" | "beetroots" => "plant:wheat",
        "cobweb" => "plant:web",
        "tall_grass" | "large_fern" | "sunflower" | "lilac" | "rose_bush" | "peony" => {
            let base = match n {
                "tall_grass" => "double_grass",
                "large_fern" => "double_fern",
                "rose_bush" => "rose_bush",
                other => other,
            };
            let half = if prop("half") == Some("upper") {
                "upper"
            } else {
                "lower"
            };
            return key(&format!("plant:{base}_{half}"));
        }
        _ if n.ends_with("_sapling") || n.ends_with("_propagule") => "plant:sapling",
        _ => return None,
    };
    key(k)
}

/// Correspondance nom de bloc Minecraft → bloc Minelib (repli pour les noms inconnus).
pub fn map_block_name(full: &str) -> BlockId {
    let n = full.strip_prefix("minecraft:").unwrap_or(full);
    let has = |s: &str| n.contains(s);
    match n {
        "air" | "cave_air" | "void_air" | "structure_void" | "light" | "barrier" => {
            return BlockId::AIR
        }
        "water" | "bubble_column" | "seagrass" | "tall_seagrass" | "kelp" | "kelp_plant" => {
            return BlockId::WATER
        }
        "lava" | "magma_block" => return BlockId::LAVA,
        "grass_block" | "mycelium" | "moss_block" => return BlockId::GRASS,
        "bedrock" => return BlockId::BEDROCK,
        "gravel" | "suspicious_gravel" => return BlockId::GRAVEL,
        "clay" => return BlockId::CLAY,
        "obsidian" | "crying_obsidian" => return BlockId::OBSIDIAN,
        "bricks" => return BlockId::BRICK,
        "snow_block" | "powder_snow" => return BlockId::SNOW,
        "torch" | "wall_torch" | "soul_torch" | "soul_wall_torch" => return BlockId::TORCH,
        "glowstone" | "sea_lantern" | "shroomlight" => return BlockId::SANDSTONE,
        _ => {}
    }
    // Objets non cubiques, plantes, décor fin : rien (Minelib n'a que des cubes).
    const THIN: &[&str] = &[
        "door",
        "rail",
        "button",
        "pressure_plate",
        "sign",
        "banner",
        "carpet",
        "flower",
        "sapling",
        "grass",
        "fern",
        "bush",
        "lantern",
        "fence",
        "pane",
        "bars",
        "chain",
        "ladder",
        "vine",
        "lever",
        "redstone_wire",
        "tripwire",
        "head",
        "skull",
        "pot",
        "candle",
        "rod",
        "coral",
        "wheat",
        "carrots",
        "potatoes",
        "beetroots",
        "cocoa",
        "roots",
        "sprouts",
        "lichen",
        "dripleaf",
        "snow",
        "cobweb",
        "dandelion",
        "poppy",
        "orchid",
        "allium",
        "tulip",
        "daisy",
        "bluet",
        "lily",
        "rose",
        "peony",
        "lilac",
        "sunflower",
        "mushroom",
        "sugar_cane",
        "bamboo",
        "cactus_flower",
        "torchflower",
        "pitcher",
        "spore",
        "hanging",
        "string",
        "scaffolding",
        "repeater",
        "comparator",
        "trapdoor",
        "end_rod",
        "brewing",
        "cake",
        "bed",
        "frogspawn",
        "petals",
        "glow_berries",
        "cave_vines",
        "amethyst_cluster",
        "bud",
        "dead_bush",
        "nether_wart",
        "melon_stem",
        "pumpkin_stem",
        "attached",
    ];
    const SOLID_HINTS: &[&str] = &[
        "block", "log", "wood", "planks", "slab", "stairs", "leaves", "hyphae", "path",
    ];
    if THIN.iter().any(|t| has(t)) && !SOLID_HINTS.iter().any(|t| has(t)) {
        return BlockId::AIR;
    }
    if has("_ore") {
        return if has("coal") {
            BlockId::COAL_ORE
        } else if has("gold") {
            BlockId::GOLD_ORE
        } else if has("diamond") || has("emerald") || has("lapis") || has("redstone") {
            BlockId::DIAMOND_ORE
        } else {
            BlockId::IRON_ORE
        };
    }
    if has("leaves") {
        return BlockId::LEAVES;
    }
    if has("_log") || has("_wood") || has("_stem") || has("_hyphae") {
        return BlockId::WOOD;
    }
    if has("planks") || has("bookshelf") || has("crafting_table") || has("barrel") || has("chest") {
        return BlockId::PLANKS;
    }
    for wood in [
        "oak", "spruce", "birch", "jungle", "acacia", "mangrove", "cherry", "bamboo", "crimson",
        "warped",
    ] {
        if has(wood) && (has("stairs") || has("slab")) {
            return BlockId::PLANKS;
        }
    }
    if has("mossy_cobblestone") || has("mossy_stone") {
        return BlockId::MOSSY_COBBLESTONE;
    }
    if has("cobblestone") || has("cobbled") {
        return BlockId::COBBLESTONE;
    }
    if has("stone_brick") || has("deepslate_brick") || has("deepslate_tile") || has("polished") {
        return BlockId::STONE_BRICKS;
    }
    if has("brick") {
        return BlockId::BRICK;
    }
    if has("sandstone") {
        return BlockId::SANDSTONE;
    }
    if has("sand") {
        return BlockId::SAND;
    }
    if has("dirt") || has("farmland") || has("podzol") || has("mud") || has("path") {
        return BlockId::DIRT;
    }
    if has("ice") {
        return BlockId::ICE;
    }
    if has("glass") {
        return BlockId::GLASS;
    }
    if has("terracotta") || has("concrete") {
        return BlockId::TERRACOTTA;
    }
    if has("wool") {
        return BlockId::WOOL;
    }
    if has("snow") {
        return BlockId::SNOW;
    }
    BlockId::STONE
}

/// Anciens identifiants numériques (avant 1.13) : registre (avec `meta`), sinon repli.
fn map_legacy(id: u16, meta: u8) -> BlockId {
    registry::registry()
        .legacy(id, meta)
        .unwrap_or_else(|| map_legacy_id(id))
}

fn map_legacy_id(id: u16) -> BlockId {
    match id {
        0 => BlockId::AIR,
        1 => BlockId::STONE,
        2 | 110 => BlockId::GRASS,
        3 | 60 => BlockId::DIRT,
        4 | 67 => BlockId::COBBLESTONE,
        5 | 53 | 134 | 135 | 136 | 125 | 126 | 47 | 58 | 54 => BlockId::PLANKS,
        7 => BlockId::BEDROCK,
        8 | 9 => BlockId::WATER,
        10 | 11 => BlockId::LAVA,
        12 => BlockId::SAND,
        13 => BlockId::GRAVEL,
        14 => BlockId::GOLD_ORE,
        15 => BlockId::IRON_ORE,
        16 => BlockId::COAL_ORE,
        17 | 162 => BlockId::WOOD,
        18 | 161 => BlockId::LEAVES,
        20 | 95 | 102 | 160 => BlockId::GLASS,
        21 | 56 | 73 | 74 | 129 => BlockId::DIAMOND_ORE,
        24 | 128 => BlockId::SANDSTONE,
        35 => BlockId::WOOL,
        45 | 108 => BlockId::BRICK,
        48 => BlockId::MOSSY_COBBLESTONE,
        49 => BlockId::OBSIDIAN,
        50 => BlockId::TORCH,
        79 | 174 => BlockId::ICE,
        80 => BlockId::SNOW,
        82 => BlockId::CLAY,
        98 | 109 => BlockId::STONE_BRICKS,
        159 | 172 => BlockId::TERRACOTTA,
        // Plantes, objets fins, décor : rien.
        6
        | 26
        | 27
        | 28
        | 30..=32
        | 37..=40
        | 50..=51
        | 55
        | 59
        | 63..=66
        | 68..=72
        | 75..=78
        | 81
        | 83
        | 85
        | 90
        | 93
        | 94
        | 96
        | 101
        | 104..=107
        | 111
        | 113
        | 115
        | 131
        | 132
        | 140
        | 141
        | 142
        | 143
        | 144
        | 147
        | 148
        | 149
        | 150
        | 157
        | 171
        | 175
        | 176
        | 177
        | 188..=197 => BlockId::AIR,
        _ => BlockId::STONE,
    }
}

// ---------------------------------------------------------------- chunks

/// Résultat de la conversion d'un chunk.
pub struct Converted {
    pub chunk: Chunk,
    pub torches: Vec<[i32; 3]>,
}

/// Valeur `i` d'un tableau compacté de `bits` bits par entrée.
fn packed(data: &[i64], i: usize, bits: usize, spanning: bool) -> usize {
    let mask = (1u64 << bits) - 1;
    if spanning {
        let bit = i * bits;
        let (w, off) = (bit / 64, bit % 64);
        let lo = data.get(w).copied().unwrap_or(0) as u64 >> off;
        let v = if off + bits > 64 {
            lo | (data.get(w + 1).copied().unwrap_or(0) as u64) << (64 - off)
        } else {
            lo
        };
        (v & mask) as usize
    } else {
        let per = 64 / bits;
        let w = data.get(i / per).copied().unwrap_or(0) as u64;
        ((w >> ((i % per) * bits)) & mask) as usize
    }
}

fn bits_for(palette_len: usize) -> usize {
    let need = usize::BITS as usize - (palette_len.max(1) - 1).leading_zeros() as usize;
    need.max(4)
}

/// Convertit un chunk NBT (n'importe quelle version) en chunk Minelib.
pub fn convert_chunk(root: &Nbt, y_offset: i32) -> Result<Converted> {
    let level = root.get("Level").unwrap_or(root);
    let x = level.get("xPos").and_then(Nbt::as_i64).context("xPos")? as i32;
    let z = level.get("zPos").and_then(Nbt::as_i64).context("zPos")? as i32;
    let data_version = root.get("DataVersion").and_then(Nbt::as_i64).unwrap_or(0);
    let pos = ChunkPos { x, z };
    let mut chunk = Chunk::new(pos);
    let mut torches = Vec::new();
    let sections = level
        .get("sections")
        .or_else(|| level.get("Sections"))
        .map(Nbt::as_list)
        .unwrap_or(&[]);

    for sec in sections {
        let sy = sec.get("Y").and_then(Nbt::as_i64).unwrap_or(0) as i32;
        let base_y = sy * 16 + y_offset;
        if base_y + 16 <= 0 || base_y >= CHUNK_HEIGHT as i32 {
            continue;
        }
        let mut put = |i: usize, id: BlockId| {
            if id == BlockId::AIR {
                return;
            }
            let (lx, lz, ly) = (i % 16, (i / 16) % 16, i / 256);
            let y = base_y + ly as i32;
            if (0..CHUNK_HEIGHT as i32).contains(&y) {
                chunk.set(lx, y as usize, lz, id);
                if id.is_torch() {
                    torches.push([x * 16 + lx as i32, y, z * 16 + lz as i32]);
                }
            }
        };

        // Format à palette (1.13+).
        let (palette, states) = match sec.get("block_states") {
            Some(bs) => (bs.get("palette"), bs.get("data")),
            None => (sec.get("Palette"), sec.get("BlockStates")),
        };
        if let Some(palette) = palette {
            let ids: Vec<BlockId> = palette
                .as_list()
                .iter()
                .map(|p| {
                    let props: HashMap<String, String> = match p.get("Properties") {
                        Some(Nbt::Compound(m)) => m
                            .iter()
                            .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned())))
                            .collect(),
                        _ => HashMap::new(),
                    };
                    map_block_state(
                        p.get("Name")
                            .and_then(Nbt::as_str)
                            .unwrap_or("minecraft:air"),
                        &props,
                    )
                })
                .collect();
            if ids.is_empty() {
                continue;
            }
            match states {
                Some(Nbt::LongArray(data)) if ids.len() > 1 => {
                    let bits = bits_for(ids.len());
                    // Avant 1.16 (20w17a), les valeurs peuvent chevaucher deux entiers 64 bits.
                    let spanning = data_version < 2529;
                    for i in 0..4096 {
                        let k = packed(data, i, bits, spanning);
                        put(i, ids.get(k).copied().unwrap_or(BlockId::STONE));
                    }
                }
                _ => {
                    for i in 0..4096 {
                        put(i, ids[0]);
                    }
                }
            }
            continue;
        }

        // Ancien format numérique.
        if let Some(Nbt::ByteArray(blocks)) = sec.get("Blocks") {
            let add = match sec.get("Add") {
                Some(Nbt::ByteArray(a)) => Some(a),
                _ => None,
            };
            let data = match sec.get("Data") {
                Some(Nbt::ByteArray(d)) => Some(d),
                _ => None,
            };
            for (i, &b) in blocks.iter().enumerate().take(4096) {
                let mut id = b as u8 as u16;
                if let Some(a) = add {
                    let nib = a.get(i / 2).copied().unwrap_or(0) as u8;
                    id |= (if i % 2 == 0 { nib & 15 } else { nib >> 4 } as u16) << 8;
                }
                let meta = data.and_then(|d| d.get(i / 2)).map_or(0, |&n| {
                    let n = n as u8;
                    if i % 2 == 0 {
                        n & 15
                    } else {
                        n >> 4
                    }
                });
                put(i, map_legacy(id, meta));
            }
        }
    }
    fix_upper_halves(&mut chunk);
    color_banners(&mut chunk, level, x, z, y_offset);
    Ok(Converted { chunk, torches })
}

/// Moitiés hautes des portes et plantes doubles : leur forme vient du bloc du dessous.
fn fix_upper_halves(chunk: &mut Chunk) {
    let reg = registry::registry();
    for y in 1..chunk.top() {
        for z in 0..16 {
            for x in 0..16 {
                let id = chunk.get(x, y, z);
                let key = &id.def().key;
                let below = &chunk.get(x, y - 1, z).def().key;
                let fixed = if key.starts_with("door_")
                    && key.contains(":upper:")
                    && below.starts_with("door_")
                    && below.contains(":lower:")
                {
                    reg.by_key(&below.replace(":lower:", ":upper:"))
                } else if key.starts_with("plant:")
                    && key.ends_with("_upper")
                    && below.starts_with("plant:")
                    && below.ends_with("_lower")
                {
                    reg.by_key(&below.replace("_lower", "_upper"))
                } else {
                    None
                };
                if let Some(f) = fixed {
                    chunk.set(x, y, z, f);
                }
            }
        }
    }
}

/// Couleur des bannières (1.12 : `TileEntities[].Base`, indice de teinture inversé).
fn color_banners(chunk: &mut Chunk, level: &Nbt, cx: i32, cz: i32, y_offset: i32) {
    let reg = registry::registry();
    let list = level
        .get("TileEntities")
        .or_else(|| level.get("block_entities"))
        .map(Nbt::as_list)
        .unwrap_or(&[]);
    for te in list {
        let Some(base) = te.get("Base").and_then(Nbt::as_i64) else {
            continue;
        };
        let (Some(x), Some(y), Some(z)) = (
            te.get("x").and_then(Nbt::as_i64),
            te.get("y").and_then(Nbt::as_i64),
            te.get("z").and_then(Nbt::as_i64),
        ) else {
            continue;
        };
        let (lx, ly, lz) = (x as i32 - cx * 16, y as i32 + y_offset, z as i32 - cz * 16);
        if !(0..16).contains(&lx)
            || !(0..16).contains(&lz)
            || !(0..CHUNK_HEIGHT as i32).contains(&ly)
        {
            continue;
        }
        let id = chunk.get(lx as usize, ly as usize, lz as usize);
        let key = &id.def().key;
        if let Some(side) = key
            .strip_prefix("banner_")
            .and_then(|r| r.split(':').nth(1))
        {
            let color = registry::DYES[(15 - base.clamp(0, 15)) as usize];
            if let Some(b) = reg.by_key(&format!("banner_{color}:{side}")) {
                chunk.set(lx as usize, ly as usize, lz as usize, b);
            }
        }
    }
}

/// Lit tous les chunks d'un fichier de région `.mca`.
pub fn read_region(bytes: &[u8]) -> Vec<Nbt> {
    let mut out = Vec::new();
    if bytes.len() < 8192 {
        return out;
    }
    for i in 0..1024 {
        let e = &bytes[i * 4..i * 4 + 4];
        let offset = ((e[0] as usize) << 16 | (e[1] as usize) << 8 | e[2] as usize) * 4096;
        if offset == 0 || e[3] == 0 || offset + 5 > bytes.len() {
            continue;
        }
        let len = u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
        let kind = bytes[offset + 4];
        let Some(payload) = bytes.get(offset + 5..offset + 4 + len) else {
            continue;
        };
        let raw = match kind {
            1 => gunzip(payload),
            2 => unzlib(payload),
            3 => Ok(payload.to_vec()),
            _ => continue, // chunk externe (.mcc) ou LZ4 : ignoré
        };
        if let Ok(nbt) = raw.and_then(|r| parse_nbt(&r)) {
            out.push(nbt);
        }
    }
    out
}

/// Monde Minecraft converti en mémoire.
pub struct Import {
    pub level: McLevel,
    pub y_offset: i32,
    pub chunks: HashMap<ChunkPos, Vec<u8>>,
    pub torches: Vec<[i32; 3]>,
}

/// Convertit tout le dossier `region/` (dimension principale).
pub fn import_world(dir: &Path) -> Result<Import> {
    let level = read_level(dir)?;
    // 1.18 (21w37a) a abaissé le bas du monde à Y = −64.
    let y_offset = if level.data_version >= 2825 { 64 } else { 0 };
    let mut chunks = HashMap::new();
    let mut torches = Vec::new();
    let mut skipped = 0;
    let mut files: Vec<_> = fs::read_dir(dir.join("region"))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "mca"))
        .collect();
    files.sort();
    for f in files {
        let Ok(bytes) = fs::read(&f) else { continue };
        for nbt in read_region(&bytes) {
            // Chunks pas encore entièrement générés par Minecraft : ignorés.
            let status = nbt
                .get("Status")
                .or_else(|| nbt.path(&["Level", "Status"]))
                .and_then(Nbt::as_str)
                .unwrap_or("full");
            let status = status.strip_prefix("minecraft:").unwrap_or(status);
            // Étapes de génération où le terrain n'est pas encore complet (noms 1.13 à 1.21).
            const EARLY: &[&str] = &[
                "empty",
                "structure_starts",
                "structure_references",
                "biomes",
                "noise",
                "surface",
                "carvers",
                "liquid_carvers",
                "base",
                "carved",
                "liquid_carved",
            ];
            if EARLY.contains(&status) {
                skipped += 1;
                continue;
            }
            match convert_chunk(&nbt, y_offset) {
                Ok(c) => {
                    torches.extend(c.torches);
                    chunks.insert(c.chunk.pos, c.chunk.to_rle());
                }
                Err(_) => skipped += 1,
            }
        }
    }
    log::info!(
        "monde Minecraft « {} » : {} chunks convertis, {} ignorés",
        level.name,
        chunks.len(),
        skipped
    );
    Ok(Import {
        level,
        y_offset,
        chunks,
        torches,
    })
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// Écriture NBT minimale, pour fabriquer des fichiers de test.
    pub fn write(n: &Nbt, out: &mut Vec<u8>) {
        match n {
            Nbt::Byte(v) => out.push(*v as u8),
            Nbt::Short(v) => out.extend(v.to_be_bytes()),
            Nbt::Int(v) => out.extend(v.to_be_bytes()),
            Nbt::Long(v) => out.extend(v.to_be_bytes()),
            Nbt::Float(v) => out.extend(v.to_bits().to_be_bytes()),
            Nbt::Double(v) => out.extend(v.to_bits().to_be_bytes()),
            Nbt::ByteArray(a) => {
                out.extend((a.len() as i32).to_be_bytes());
                out.extend(a.iter().map(|&b| b as u8));
            }
            Nbt::String(s) => {
                out.extend((s.len() as u16).to_be_bytes());
                out.extend(s.as_bytes());
            }
            Nbt::List(l) => {
                out.push(l.first().map_or(0, tag));
                out.extend((l.len() as i32).to_be_bytes());
                l.iter().for_each(|e| write(e, out));
            }
            Nbt::Compound(m) => {
                for (k, v) in m {
                    out.push(tag(v));
                    out.extend((k.len() as u16).to_be_bytes());
                    out.extend(k.as_bytes());
                    write(v, out);
                }
                out.push(0);
            }
            Nbt::IntArray(a) => {
                out.extend((a.len() as i32).to_be_bytes());
                a.iter().for_each(|v| out.extend(v.to_be_bytes()));
            }
            Nbt::LongArray(a) => {
                out.extend((a.len() as i32).to_be_bytes());
                a.iter().for_each(|v| out.extend(v.to_be_bytes()));
            }
        }
    }

    fn tag(n: &Nbt) -> u8 {
        match n {
            Nbt::Byte(_) => 1,
            Nbt::Short(_) => 2,
            Nbt::Int(_) => 3,
            Nbt::Long(_) => 4,
            Nbt::Float(_) => 5,
            Nbt::Double(_) => 6,
            Nbt::ByteArray(_) => 7,
            Nbt::String(_) => 8,
            Nbt::List(_) => 9,
            Nbt::Compound(_) => 10,
            Nbt::IntArray(_) => 11,
            Nbt::LongArray(_) => 12,
        }
    }

    pub fn root(n: &Nbt) -> Vec<u8> {
        let mut out = vec![10, 0, 0];
        if let Nbt::Compound(_) = n {
            write(n, &mut out);
        }
        out
    }

    pub fn compound(items: Vec<(&str, Nbt)>) -> Nbt {
        Nbt::Compound(items.into_iter().map(|(k, v)| (k.to_owned(), v)).collect())
    }

    fn block(name: &str) -> Nbt {
        compound(vec![("Name", Nbt::String(name.into()))])
    }

    /// Chunk 1.18+ : sol de pierre à Y = −64..−49, herbe à Y = 64, torche à Y = 65 en (3, 4).
    pub fn modern_chunk(x: i32, z: i32) -> Nbt {
        let stone_section = compound(vec![
            ("Y", Nbt::Byte(-4)),
            (
                "block_states",
                compound(vec![(
                    "palette",
                    Nbt::List(vec![block("minecraft:deepslate")]),
                )]),
            ),
        ]);
        // Section Y = 4 (64..79) : palette [air, grass_block, torch], 4 bits par entrée.
        let mut idx = vec![0usize; 4096];
        for v in idx.iter_mut().take(256) {
            *v = 1; // couche 64 entière en herbe
        }
        idx[256 + 4 * 16 + 3] = 2; // torche en (3, 65, 4)
        let mut longs = vec![0i64; 256];
        for (i, v) in idx.iter().enumerate() {
            longs[i / 16] |= (*v as i64) << ((i % 16) * 4);
        }
        let grass_section = compound(vec![
            ("Y", Nbt::Byte(4)),
            (
                "block_states",
                compound(vec![
                    (
                        "palette",
                        Nbt::List(vec![
                            block("minecraft:air"),
                            block("minecraft:grass_block"),
                            block("minecraft:torch"),
                        ]),
                    ),
                    ("data", Nbt::LongArray(longs)),
                ]),
            ),
        ]);
        compound(vec![
            ("DataVersion", Nbt::Int(3700)),
            ("xPos", Nbt::Int(x)),
            ("zPos", Nbt::Int(z)),
            ("Status", Nbt::String("minecraft:full".into())),
            ("sections", Nbt::List(vec![stone_section, grass_section])),
        ])
    }

    /// Fichier de région avec les chunks donnés (compression zlib).
    pub fn region(chunks: &[Nbt]) -> Vec<u8> {
        use std::io::Write;
        let mut header = vec![0u8; 8192];
        let mut body = Vec::new();
        for (k, c) in chunks.iter().enumerate() {
            let mut enc =
                flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            enc.write_all(&root(c)).unwrap();
            let z = enc.finish().unwrap();
            let sector = 2 + body.len() / 4096;
            let mut blob = ((z.len() + 1) as u32).to_be_bytes().to_vec();
            blob.push(2);
            blob.extend(z);
            let sectors = blob.len().div_ceil(4096);
            blob.resize(sectors * 4096, 0);
            body.extend(blob);
            let lx = c.get("xPos").and_then(Nbt::as_i64).unwrap() as usize & 31;
            let lz = c.get("zPos").and_then(Nbt::as_i64).unwrap() as usize & 31;
            let e = (lx + lz * 32) * 4;
            header[e..e + 3].copy_from_slice(&(sector as u32).to_be_bytes()[1..]);
            header[e + 3] = sectors as u8;
            let _ = k;
        }
        header.extend(body);
        header
    }

    #[test]
    fn modern_chunk_converts_with_offset() {
        let c = convert_chunk(&modern_chunk(2, -1), 64).unwrap();
        assert_eq!(c.chunk.pos, ChunkPos { x: 2, z: -1 });
        assert_eq!(c.chunk.get(0, 0, 0), BlockId::STONE); // deepslate à Y = −64
        assert_eq!(c.chunk.get(5, 128, 9), BlockId::GRASS); // Y 64 → 128
        assert_eq!(c.chunk.get(3, 129, 4), BlockId::TORCH);
        assert_eq!(c.chunk.get(3, 130, 4), BlockId::AIR);
        assert_eq!(c.torches, vec![[2 * 16 + 3, 129, -16 + 4]]);
    }

    #[test]
    fn region_roundtrip_and_spanning_states() {
        let bytes = region(&[modern_chunk(0, 0), modern_chunk(1, 0)]);
        let chunks = read_region(&bytes);
        assert_eq!(chunks.len(), 2);
        // Valeurs à cheval sur deux entiers (format 1.13-1.15), 5 bits.
        let data = vec![0b11111i64 << 60, 0b1];
        assert_eq!(packed(&data, 12, 5, true), 0b11111 & 0b01111 | 0b10000);
        assert_eq!(bits_for(1), 4);
        assert_eq!(bits_for(17), 5);
    }

    #[test]
    fn block_names() {
        assert_eq!(map_block_name("minecraft:oak_log"), BlockId::WOOD);
        assert_eq!(
            map_block_name("minecraft:deepslate_iron_ore"),
            BlockId::IRON_ORE
        );
        assert_eq!(map_block_name("minecraft:short_grass"), BlockId::AIR);
        assert_eq!(map_block_name("minecraft:grass_block"), BlockId::GRASS);
        assert_eq!(map_block_name("minecraft:spruce_planks"), BlockId::PLANKS);
        assert_eq!(map_block_name("minecraft:wall_torch"), BlockId::TORCH);
        assert_eq!(map_block_name("minecraft:water"), BlockId::WATER);
        assert_eq!(
            map_block_name("minecraft:stone_bricks"),
            BlockId::STONE_BRICKS
        );
        assert_eq!(
            map_block_name("minecraft:red_mushroom_block"),
            BlockId::STONE
        );
        assert_eq!(map_legacy_id(17), BlockId::WOOD);
        assert_eq!(map_block_name("minecraft:crimson_stem"), BlockId::WOOD);
        assert_eq!(map_block_name("minecraft:bamboo_planks"), BlockId::PLANKS);
        assert_eq!(
            map_block_name("minecraft:flowering_azalea_leaves"),
            BlockId::LEAVES
        );
        assert_eq!(map_block_name("minecraft:dirt_path"), BlockId::DIRT);
        assert_eq!(map_block_name("minecraft:snow"), BlockId::AIR);
    }
}

#[cfg(test)]
mod real_files {
    use super::*;

    /// Lit de vrais fichiers .mca : `MINELIB_MCA=chemin cargo test real_region -- --nocapture`.
    #[test]
    fn real_region() {
        let Ok(paths) = std::env::var("MINELIB_MCA") else {
            return;
        };
        for path in paths.split(';') {
            let bytes = fs::read(path).unwrap();
            let chunks = read_region(&bytes);
            let mut hist: HashMap<BlockId, usize> = HashMap::new();
            let mut ok = 0;
            for c in &chunks {
                let dv = c.get("DataVersion").and_then(Nbt::as_i64).unwrap_or(0);
                let off = if dv >= 2825 { 64 } else { 0 };
                if let Ok(conv) = convert_chunk(c, off) {
                    ok += 1;
                    for y in 0..256 {
                        for z in 0..16 {
                            for x in 0..16 {
                                *hist.entry(conv.chunk.get(x, y, z)).or_default() += 1;
                            }
                        }
                    }
                }
            }
            let dv = chunks
                .first()
                .and_then(|c| c.get("DataVersion"))
                .and_then(Nbt::as_i64);
            let mut h: Vec<_> = hist.into_iter().collect();
            h.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
            println!(
                "{path}: {} chunks lus, {ok} convertis, DataVersion {dv:?}",
                chunks.len()
            );
            for (b, n) in h.iter().take(10) {
                println!("   {:<22} {n}", b.name());
            }
            assert!(ok > 0);
        }
    }
}

#[cfg(test)]
mod real_world {
    use super::*;

    /// `MINELIB_MC_WORLD=dossier cargo test real_world -- --nocapture`
    #[test]
    fn real_world_coverage() {
        let Ok(dir) = std::env::var("MINELIB_MC_WORLD") else {
            return;
        };
        let dir = Path::new(&dir);
        let reg = registry::registry();
        let mut missing: HashMap<(u16, u8), usize> = HashMap::new();
        for f in fs::read_dir(dir.join("region")).unwrap().flatten() {
            let bytes = fs::read(f.path()).unwrap();
            for nbt in read_region(&bytes) {
                let level = nbt.get("Level").unwrap_or(&nbt);
                for sec in level.get("Sections").map(Nbt::as_list).unwrap_or(&[]) {
                    let (Some(Nbt::ByteArray(b)), Some(Nbt::ByteArray(d))) =
                        (sec.get("Blocks"), sec.get("Data"))
                    else {
                        continue;
                    };
                    for i in 0..4096 {
                        let id = b[i] as u8 as u16;
                        let meta = (d[i / 2] as u8 >> ((i % 2) * 4)) & 15;
                        if id != 0 && reg.legacy(id, meta).is_none() {
                            *missing.entry((id, meta)).or_default() += 1;
                        }
                    }
                }
            }
        }
        println!("variantes sans correspondance : {missing:?}");
        let t = std::time::Instant::now();
        let imp = import_world(dir).unwrap();
        println!(
            "« {} » : {} chunks en {:.2} s, générateur {:?}, joueur {:?}, apparition {:?}",
            imp.level.name,
            imp.chunks.len(),
            t.elapsed().as_secs_f32(),
            imp.level.generator,
            imp.level.player,
            imp.level.spawn
        );
        let mut hist: HashMap<BlockId, usize> = HashMap::new();
        let mut bytes = 0;
        for (p, rle) in &imp.chunks {
            bytes += rle.len();
            let c = Chunk::from_rle(*p, rle).unwrap();
            for y in 0..c.top() {
                for z in 0..16 {
                    for x in 0..16 {
                        *hist.entry(c.get(x, y, z)).or_default() += 1;
                    }
                }
            }
        }
        let mut h: Vec<_> = hist
            .into_iter()
            .filter(|(b, _)| *b != BlockId::AIR)
            .collect();
        h.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
        println!(
            "chunks compressés : {} Ko ; {} types de blocs Minelib",
            bytes / 1024,
            h.len()
        );
        for (b, n) in h.iter().take(25) {
            println!("   {:<40} {n}", b.def().key);
        }
        assert!(missing.is_empty());
    }
}
