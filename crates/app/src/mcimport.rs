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
    added_block(full, props)
        .or_else(|| recent_block(full))
        .or_else(|| modern_block(full, props))
        .unwrap_or_else(|| map_block_name(full))
}

/// (v179) Blocs récents qui ont maintenant leur bloc dans Minelib (`recent_blocks` du
/// registre) ; avant, `recent_block` les approchait (ardoise des abîmes en andésite claire,
/// mousse en herbe, cuivre en béton orange, lumières des cartes en vide…).
fn added_block(full: &str, props: &HashMap<String, String>) -> Option<BlockId> {
    let reg = registry::registry();
    let n = full.strip_prefix("minecraft:").unwrap_or(full);
    // Cuivre ciré : le même à l'œil.
    let n = n.strip_prefix("waxed_").unwrap_or(n);
    let prop = |k: &str| props.get(k).map(String::as_str);
    let key = |k: &str| reg.by_key(k);
    // Escaliers, dalles et murets : formes de `modern_block`, qui connaît les nouveaux
    // matériaux (`modern_material`) ; le cuivre n'y passait pas (`recent_block` le prenait
    // avant).
    if n.ends_with("_stairs") || n.ends_with("_slab") || n.ends_with("_wall") {
        return modern_block(&format!("minecraft:{n}"), props);
    }
    // Côté d'un bloc posé contre une face (lichen, résine) : la première face occupée.
    let face = || {
        [
            ("east", 0u8),
            ("west", 1),
            ("up", 2),
            ("down", 3),
            ("south", 4),
            ("north", 5),
        ]
        .into_iter()
        .find(|(k, _)| prop(k) == Some("true"))
        .map_or(3, |(_, s)| s)
    };
    let axis = || match prop("axis") {
        Some("x") => ":x",
        Some("z") => ":z",
        _ => "",
    };
    match n {
        "deepslate"
        | "infested_deepslate"
        | "cobbled_deepslate"
        | "polished_deepslate"
        | "deepslate_bricks"
        | "deepslate_tiles"
        | "cracked_deepslate_bricks"
        | "cracked_deepslate_tiles"
        | "chiseled_deepslate"
        | "reinforced_deepslate" => {
            return key(n.strip_prefix("infested_").unwrap_or(n));
        }
        "tuff"
        | "polished_tuff"
        | "tuff_bricks"
        | "chiseled_tuff"
        | "chiseled_tuff_bricks"
        | "blackstone"
        | "polished_blackstone"
        | "polished_blackstone_bricks"
        | "cracked_polished_blackstone_bricks"
        | "chiseled_polished_blackstone"
        | "gilded_blackstone"
        | "mud"
        | "packed_mud"
        | "mud_bricks"
        | "raw_copper_block"
        | "raw_iron_block"
        | "raw_gold_block"
        | "basalt"
        | "polished_basalt"
        | "smooth_basalt"
        | "moss_block"
        | "pale_moss_block"
        | "moss_carpet"
        | "pale_moss_carpet"
        | "calcite"
        | "dripstone_block"
        | "amethyst_block"
        | "budding_amethyst"
        | "quartz_bricks"
        | "netherite_block"
        | "ancient_debris"
        | "lodestone"
        | "tinted_glass"
        | "brown_mushroom_block"
        | "red_mushroom_block"
        | "mushroom_stem"
        | "bamboo_block"
        | "stripped_bamboo_block"
        | "bamboo_mosaic"
        | "ochre_froglight"
        | "verdant_froglight"
        | "pearlescent_froglight"
        | "loom"
        | "composter"
        | "fletching_table"
        | "cartography_table"
        | "smithing_table"
        | "beehive"
        | "bee_nest"
        | "target"
        | "leaf_litter"
        | "pink_petals"
        | "wildflowers" => {
            return key(n);
        }
        "campfire" | "soul_campfire" => {
            return key(if prop("lit") == Some("false") {
                "campfire:off"
            } else {
                "campfire"
            });
        }
        "glow_lichen" | "resin_clump" => return key(&format!("{n}:{}", face())),
        // Blocs de corail, vivants et morts.
        _ if n.ends_with("_coral_block") => return key(n),
        // Chaîne de cuivre : comme la chaîne de fer (debout, ou en barreaux couchée).
        _ if n.ends_with("copper_chain") => return modern_block("minecraft:iron_chain", props),
        "pale_hanging_moss" | "pale_hanging_moss_tip" | "hanging_roots" | "firefly_bush" => {
            return key(&format!("plant:{}", n.strip_suffix("_tip").unwrap_or(n)));
        }
        // Azalée (buisson) : ses feuilles.
        "azalea" | "azalea_leaves" => return key("leaves_azalea"),
        "flowering_azalea" | "flowering_azalea_leaves" => return key("leaves_flowering_azalea"),
        // Cœur de grinceur : une bûche de chêne pâle.
        "creaking_heart" => return key(&format!("log_pale_oak{}", axis())),
        // Feu : rien (il ne se garde pas dans le monde), pas de la pierre.
        "fire" | "soul_fire" => return Some(BlockId::AIR),
        // Cornichons de mer : l'eau (ou le vide) où ils sont posés.
        "sea_pickle" => {
            return Some(if prop("waterlogged") == Some("true") {
                registry::Fluid::Water.block(registry::FLUID_SOURCE)
            } else {
                BlockId::AIR
            });
        }
        // Lumière invisible des cartes : niveau de Minecraft regroupé en 4, 8, 12, 15.
        "light" => {
            let level: u8 = prop("level").and_then(|l| l.parse().ok()).unwrap_or(15);
            return match level {
                0 => Some(BlockId::AIR),
                1..=5 => key("light:4"),
                6..=9 => key("light:8"),
                10..=13 => key("light:12"),
                _ => key("light:15"),
            };
        }
        _ => {}
    }
    // Cuivre : bloc, taillé, sculpté, grille, pour ses quatre états.
    for stage in ["", "exposed_", "weathered_", "oxidized_"] {
        let Some(rest) = n.strip_prefix(stage) else {
            continue;
        };
        let k = match rest {
            "copper_block" if stage.is_empty() => "copper_block".to_string(),
            "copper" if !stage.is_empty() => format!("{stage}copper"),
            "cut_copper" | "chiseled_copper" | "copper_grate" => format!("{stage}{rest}"),
            "lightning_rod" => {
                let k = match prop("facing") {
                    Some("east" | "west") => "lightning_rod:x",
                    Some("north" | "south") => "lightning_rod:z",
                    _ => "lightning_rod",
                };
                return key(k);
            }
            _ => continue,
        };
        return key(&k);
    }
    // Nouvelles essences : planches, clôtures, bûches (tiges), bois (hyphes), feuilles.
    for w in registry::NEW_WOODS {
        let Some(rest) = n
            .strip_prefix("stripped_")
            .unwrap_or(n)
            .strip_prefix(w)
            .and_then(|r| r.strip_prefix('_'))
        else {
            continue;
        };
        let stripped = if n.starts_with("stripped_") {
            "stripped_"
        } else {
            ""
        };
        let k = match rest {
            "planks" => format!("planks_{w}"),
            "fence" | "fence_gate" => format!("fence_{w}"),
            "log" | "stem" => format!("log_{stripped}{w}{}", axis()),
            "wood" | "hyphae" => format!("wood_{stripped}{w}"),
            "leaves" => format!("leaves_{w}"),
            _ => continue,
        };
        if let Some(b) = key(&k) {
            return Some(b);
        }
    }
    None
}

/// Blocs récents de Minecraft (1.17 et après) sans équivalent exact : le bloc de Minelib
/// le plus proche à l'œil (ils devenaient de la pierre ou du vide, d'où des trous et des
/// textures fausses dans les cartes importées).
fn recent_block(full: &str) -> Option<BlockId> {
    let n = full.strip_prefix("minecraft:").unwrap_or(full);
    let reg = registry::registry();
    let k = match n {
        "calcite" => "stone_diorite_smooth",
        "pearlescent_froglight" => "sea_lantern",
        "ochre_froglight" => "glowstone",
        "verdant_froglight" => "lamp_on",
        "netherite_block" => "coal_block",
        "honey_block" | "honeycomb_block" => "concrete_orange",
        "dragon_egg" => "obsidian",
        "grindstone" | "stonecutter" => "smooth_stone",
        "lectern" => "bookshelf",
        "beehive" | "bee_nest" => "planks_birch",
        "bell" => "gold_block",
        "stripped_bamboo_block" | "bamboo_block" | "bamboo_mosaic" => "hay",
        // Chaîne : verticale, une vraie chaîne ; couchée, des barreaux.
        "iron_chain" | "chain" => return None,
        _ if n.ends_with("_coral") || n.ends_with("_coral_fan") => {
            return reg.by_key(&format!("plant:{n}"));
        }
        _ if n.ends_with("_coral_wall_fan") => {
            return reg.by_key(&format!("plant:{}", n.replace("_wall_fan", "_fan")));
        }
        "crimson_roots" | "warped_roots" => return reg.by_key(&format!("plant:{n}")),
        "cave_vines"
        | "cave_vines_plant"
        | "weeping_vines"
        | "weeping_vines_plant"
        | "twisting_vines"
        | "twisting_vines_plant" => "plant:cave_vines",
        _ if n.ends_with("candle") => "candle",
        "decorated_pot" => "decorated_pot",
        "scaffolding" => "scaffolding",
        "redstone_wire" => "redstone_wire",
        "small_dripleaf" | "big_dripleaf" | "big_dripleaf_stem" => "plant:fern",
        "bamboo" | "bamboo_sapling" => "plant:reeds",
        "torchflower" | "cactus_flower" | "torchflower_crop" => "plant:tulip_orange",
        "muddy_mangrove_roots" | "mangrove_roots" => "coarse_dirt",
        _ if n.ends_with("_bars") => "iron_bars",
        "moss_carpet" => "carpet_green",
        "pale_moss_carpet" => "carpet_silver",
        "bush" | "short_dry_grass" | "tall_dry_grass" => "plant:tallgrass",
        "wildflowers" | "pink_petals" | "leaf_litter" => "plant:dandelion",
        "sweet_berry_bush" => "plant:fern",
        "tuff" | "polished_tuff" | "chiseled_tuff" => "stone_andesite",
        "dripstone_block" => "hardened_clay",
        "amethyst_block" | "budding_amethyst" => "purpur",
        "raw_iron_block" => "iron_block",
        "raw_gold_block" => "gold_block",
        "raw_copper_block" => "concrete_orange",
        "mud" | "packed_mud" => "coarse_dirt",
        "sculk" => "wool_black",
        "reinforced_deepslate" => "stonebrick",
        "target" => "hay",
        "lodestone" => "stonebrick_carved",
        "respawn_anchor" => "obsidian_crying",
        _ if n.ends_with("_shelf") => "bookshelf",
        _ if n.starts_with("dead_") && n.ends_with("_coral_block") => "stone_andesite",
        _ if n.ends_with("_coral_block") => "wool_pink",
        _ if n.ends_with("_froglight") => "glowstone",
        _ if n.contains("copper") && !n.contains("ore") => {
            let oxid = n.contains("oxidized") || n.contains("weathered");
            let block = if oxid {
                "prismarine"
            } else {
                "concrete_orange"
            };
            if n.ends_with("_slab") {
                return reg
                    .by_key(&format!("slab:{block}:bottom"))
                    .or_else(|| reg.by_key(block));
            }
            if n.contains("_bulb") || n.contains("lantern") {
                "lamp_on"
            } else if n.contains("_bars") {
                "iron_bars"
            } else if n.contains("_door") || n.contains("_trapdoor") {
                return None;
            } else {
                block
            }
        }
        _ => return None,
    };
    reg.by_key(k)
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
        // (v179) Nouvelles essences : leurs propres planches.
        "mangrove" => "planks_mangrove",
        "cherry" => "planks_cherry",
        "bamboo" => "planks_bamboo",
        "crimson" => "planks_crimson",
        "warped" => "planks_warped",
        "pale_oak" => "planks_pale_oak",
        "oak" | "spruce" | "birch" | "jungle" | "acacia" | "dark_oak" | "petrified_oak" => {
            registry::planks_key(registry::wood_index(m))
        }
        "stone" => "stone",
        "smooth_stone" => "smooth_stone",
        "cobblestone" => "cobblestone",
        // (v179) Ardoise des abîmes, tuf, roche noire, boue, cuivre, bambou : leurs blocs.
        "cobbled_deepslate" => "cobbled_deepslate",
        "polished_deepslate" => "polished_deepslate",
        "deepslate_brick" => "deepslate_bricks",
        "deepslate_tile" => "deepslate_tiles",
        "tuff" => "tuff",
        "polished_tuff" => "polished_tuff",
        "tuff_brick" => "tuff_bricks",
        "blackstone" => "blackstone",
        "polished_blackstone" => "polished_blackstone",
        "polished_blackstone_brick" => "polished_blackstone_bricks",
        "mud_brick" => "mud_bricks",
        "cut_copper" => "cut_copper",
        "exposed_cut_copper" => "exposed_cut_copper",
        "weathered_cut_copper" => "weathered_cut_copper",
        "oxidized_cut_copper" => "oxidized_cut_copper",
        "bamboo_mosaic" => "bamboo_mosaic",
        "mossy_cobblestone" => "cobblestone_mossy",
        "stone_brick" => "stonebrick",
        "mossy_stone_brick" => "stonebrick_mossy",
        "brick" => "brick",
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
        "andesite" => "stone_andesite",
        "polished_andesite" => "stone_andesite_smooth",
        "end_stone_brick" => "end_bricks",
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

    // Eau et lave qui coulent (Minecraft : 0 source, 1 le plus fort … 7, 8 et plus qui
    // tombe).
    if n == "water" || n == "lava" || n == "flowing_water" || n == "flowing_lava" {
        let f = if n.ends_with("water") {
            registry::Fluid::Water
        } else {
            registry::Fluid::Lava
        };
        let level: u8 = prop("level").and_then(|l| l.parse().ok()).unwrap_or(0);
        return Some(match level {
            0 => f.block(registry::FLUID_SOURCE),
            1..=7 => f.block(8 - level),
            _ => f.block(registry::FLUID_FALLING),
        });
    }

    // Tête contre un mur : posée du côté opposé à son regard.
    if n.ends_with("_wall_head") || n.ends_with("_wall_skull") {
        return key(&format!("skull:wall:{}", wall_side(prop("facing"))));
    }
    // Tête au sol : tournée de `rotation` seizièmes de tour, comme dans Minecraft.
    if matches!(
        n,
        "skeleton_skull"
            | "player_head"
            | "zombie_head"
            | "creeper_head"
            | "dragon_head"
            | "piglin_head"
            | "wither_skeleton_skull"
    ) {
        let r: u8 = prop("rotation").and_then(|r| r.parse().ok()).unwrap_or(0);
        return Some(registry::skull_block(false, r & 15));
    }

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
        // Coin intérieur (trois quarts de bloc en haut) : les cartes s'en servent pour
        // des sols presque plats, souvent forcé sans les voisins qui le donneraient. Minelib
        // ne garde pas la forme des escaliers (il la tire des voisins) : on pose le bloc
        // plein de la matière, plus proche que la demi-marche (qui faisait des trous).
        if matches!(prop("shape"), Some("inner_left" | "inner_right")) {
            if let Some(b) = key(modern_material(m)) {
                return Some(b);
            }
        }
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
        // (v179) Le muret de sa pierre quand il existe, sinon celui de pierre (moussu).
        let own = n
            .strip_suffix("_wall")
            .and_then(|m| key(&format!("wall_{}", modern_material(m))));
        return own.or_else(|| {
            key(if n.contains("mossy") {
                "wall_cobblestone_mossy"
            } else {
                "wall_cobblestone"
            })
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
        let open = prop("open") == Some("true");
        let mut side = registry::door_side(horizontal(prop("facing")), open);
        // Porte ouverte gonds à droite : le battant se range de l'autre côté de la case.
        if open && prop("hinge") == Some("right") {
            side = match side {
                0 => 1,
                1 => 0,
                4 => 5,
                _ => 4,
            };
        }
        // Ouverte : `:o`, ou `:m` gonds à droite (le maillage y lit où sont les gonds).
        let state = match (open, prop("hinge") == Some("right")) {
            (false, _) => "",
            (true, false) => ":o",
            (true, true) => ":m",
        };
        return key(&format!(
            "door_{}:{half}:{side}{state}",
            registry::door_key(d)
        ));
    }
    if let Some(w) = n.strip_suffix("_trapdoor") {
        let kind = if w == "iron" {
            "iron_trapdoor"
        } else {
            "trapdoor"
        };
        // Côté contre lequel elle se relève (ouverte), ou se relèvera (fermée).
        let hinge = match prop("facing") {
            Some("north") => 4,
            Some("south") => 5,
            Some("west") => 0,
            _ => 1,
        };
        if prop("open") == Some("true") {
            return key(&format!("{kind}:{hinge}"));
        }
        let half = if prop("half") == Some("top") { 2 } else { 3 };
        return key(&format!("{kind}:{half}:{hinge}"));
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
            let base = registry::log_key(registry::wood_index(w));
            // Couchée le long de X ou de Z (`axis`).
            return match prop("axis") {
                Some(a @ ("x" | "z")) => key(&format!("{base}:{a}")),
                _ => key(base),
            };
        }
    }
    // Pot garni : la plante (en croix) dedans, sinon le pot vide.
    if let Some(plant) = n.strip_prefix("potted_") {
        let p = map_block_state(&format!("minecraft:{plant}"), &HashMap::new());
        return key(&format!("flower_pot:{}", p.def().key)).or_else(|| key("flower_pot"));
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
        "iron_chain" | "chain" => {
            return key(if matches!(prop("axis"), Some("x" | "z")) {
                "iron_bars"
            } else {
                "chain"
            });
        }
        "lantern" | "soul_lantern" => {
            let hang = if prop("hanging") == Some("true") {
                ":hanging"
            } else {
                ""
            };
            return key(&format!("{n}{hang}"));
        }
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

/// Version du convertisseur : un monde converti par une version plus ancienne l'est de
/// nouveau à son ouverture (2 : escaliers en coin intérieur → bloc plein ; 3 : blocs
/// récents, au lieu de pierre ou de vide ; 4 : eau et lave qui coulent, têtes murales ; 5 : décors en entités
/// d'affichage ; 11 : têtes au sol dans leurs 16 directions, têtes des porte-armures, des
/// entités d'affichage et des cadres, têtes d'avant 1.13 ; 12 : blocs récents qui ont
/// maintenant leur bloc, murets de chaque pierre, lumières invisibles des cartes).
pub const IMPORT_VERSION: u32 = 12;

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
    /// Têtes à skin (lignes de `tetes.txt`).
    pub heads: Vec<String>,
}

/// Fichier des têtes à skin d'un monde importé : une ligne `x,y,z,pose,empreinte` par
/// tête (pose : rotation 0..15 au sol, `w0`/`w1`/`w4`/`w5` contre un mur ; empreinte de la
/// texture sur textures.minecraft.net ; voir `heads::parse`). Aussi les cadres des murs
/// (`frame_line`).
pub const HEADS_FILE: &str = "tetes.txt";

pub fn write_heads(dir: &Path, heads: &[String]) {
    let mut t = heads.join("\n");
    if !t.is_empty() {
        t.push('\n');
    }
    let _ = fs::write(dir.join(HEADS_FILE), t);
}

/// Empreinte de la texture d'une tête (`profile` 1.20.5+, `SkullOwner` avant, `Owner` avant
/// 1.13).
fn head_texture(te: &Nbt) -> Option<String> {
    let props = te
        .path(&["profile", "properties"])
        .or_else(|| te.path(&["SkullOwner", "Properties", "textures"]))
        .or_else(|| te.path(&["Owner", "Properties", "textures"]))?;
    textures_hash(props)
}

/// Objet « tête » (porté par une entité) : son skin (`None` : ce n'est pas une tête). Les
/// têtes de monstres et celles sans texture ont le skin par défaut du jeu.
fn item_head(it: &Nbt) -> Option<crate::heads::HeadSkin> {
    use crate::heads::HeadSkin;
    let id = it.get("id").and_then(Nbt::as_str)?;
    let n = id.strip_prefix("minecraft:").unwrap_or(id);
    let head = matches!(
        n,
        "player_head"
            | "skeleton_skull"
            | "wither_skeleton_skull"
            | "zombie_head"
            | "creeper_head"
            | "dragon_head"
            | "piglin_head"
            // Avant 1.13 : `skull` (le type dans `Damage`).
            | "skull"
    );
    if !head {
        return None;
    }
    let hash = it
        .path(&["components", "minecraft:profile", "properties"])
        .or_else(|| it.path(&["tag", "SkullOwner", "Properties", "textures"]))
        .and_then(textures_hash);
    Some(
        hash.and_then(|h| HeadSkin::parse(&h))
            .unwrap_or(HeadSkin::Default),
    )
}

/// Empreinte de la texture dans une liste de propriétés d'un profil de joueur.
fn textures_hash(props: &Nbt) -> Option<String> {
    let value = props.as_list().iter().find_map(|p| {
        p.get("value")
            .or_else(|| p.get("Value"))
            .and_then(Nbt::as_str)
    })?;
    let json = base64_decode(value)?;
    let json = String::from_utf8_lossy(&json);
    let url = json.split("\"url\"").nth(1)?;
    let start = url.find("/texture/")? + "/texture/".len();
    let hash: String = url[start..]
        .chars()
        .take_while(|c| c.is_ascii_hexdigit())
        .collect();
    (hash.len() >= 32).then_some(hash)
}

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let val = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        } as u32)
    };
    let mut out = Vec::new();
    let (mut acc, mut bits) = (0u32, 0);
    for &c in s.as_bytes() {
        if c == b'=' || c.is_ascii_whitespace() {
            continue;
        }
        acc = (acc << 6) | val(c)?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// Têtes à skin du chunk (voir `HEADS_FILE`). Le bloc porte la pose de la tête (au sol :
/// sa rotation ; avant 1.13, elle est dans l'entité de bloc, `Rot`).
fn read_heads(level: &Nbt, chunk: &mut Chunk, cx: i32, cz: i32, y_offset: i32) -> Vec<String> {
    let list = level
        .get("block_entities")
        .or_else(|| level.get("TileEntities"))
        .map(Nbt::as_list)
        .unwrap_or(&[]);
    let mut out = Vec::new();
    for te in list {
        let id = te.get("id").and_then(Nbt::as_str).unwrap_or("");
        let id = id.to_ascii_lowercase();
        if !id.contains("skull") && !id.contains("head") {
            continue;
        }
        let (Some(x), Some(y), Some(z)) = (
            te.get("x").and_then(Nbt::as_i64),
            te.get("y").and_then(Nbt::as_i64),
            te.get("z").and_then(Nbt::as_i64),
        ) else {
            continue;
        };
        let (lx, lz) = (x as i32 - cx * 16, z as i32 - cz * 16);
        let wy = y as i32 + y_offset;
        if !(0..16).contains(&lx)
            || !(0..16).contains(&lz)
            || !(0..CHUNK_HEIGHT as i32).contains(&wy)
        {
            continue;
        }
        let (ux, uy, uz) = (lx as usize, wy as usize, lz as usize);
        let Some((wall, mut n)) = registry::skull_pose(chunk.get(ux, uy, uz)) else {
            continue;
        };
        if let Some(rot) = te.get("Rot").and_then(Nbt::as_i64).filter(|_| !wall) {
            n = (rot & 15) as u8;
            chunk.set(ux, uy, uz, registry::skull_block(false, n));
        }
        let Some(hash) = head_texture(te) else {
            continue;
        };
        let pose = if wall { format!("w{n}") } else { n.to_string() };
        out.push(format!("{x},{wy},{z},{pose},{hash}"));
    }
    out
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
    let heads = read_heads(level, &mut chunk, x, z, y_offset);
    Ok(Converted {
        chunk,
        torches,
        heads,
    })
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

/// Entités d'affichage (`block_display`, `item_display`, avec leurs passagers) du dossier
/// `entities/` : chaque bloc affiché remplit les cases dont le centre est dans sa boîte
/// (position, rotation de l'entité, puis translation, rotation gauche, échelle, rotation
/// droite). Seulement dans le vide ou une barrière (un bloc solide y remplace la barrière,
/// qui servait de collision au décor). Les plaques trop fines (moins d'une demi-case) ne
/// couvrent aucun centre de case et sont laissées. Renvoie le nombre de blocs posés.
fn import_displays(dir: &Path, y_offset: i32, chunks: &mut HashMap<ChunkPos, Chunk>) -> usize {
    let Ok(rd) = fs::read_dir(dir) else {
        return 0;
    };
    let mut files: Vec<_> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "mca"))
        .collect();
    files.sort();
    let mut placed = Vec::new();
    for f in files {
        let Ok(bytes) = fs::read(&f) else { continue };
        for root in read_region(&bytes) {
            for e in root.get("Entities").map(Nbt::as_list).unwrap_or(&[]) {
                display_cells(e, None, y_offset, &mut placed);
            }
        }
    }
    let mut n = 0;
    for (p, b, thin) in placed {
        let pos = ChunkPos::containing(p[0], p[2]);
        let Some(c) = chunks.get_mut(&pos) else {
            continue;
        };
        if !(0..CHUNK_HEIGHT as i32).contains(&p[1]) {
            continue;
        }
        let (lx, lz) = (p[0].rem_euclid(16) as usize, p[2].rem_euclid(16) as usize);
        let cur = c.get(lx, p[1] as usize, lz);
        let free = (cur == BlockId::AIR && !thin) || (cur == BlockId::BARRIER && b.is_solid());
        if free {
            c.set(lx, p[1] as usize, lz, b);
            n += 1;
        }
    }
    n
}

/// Entités des cartes dessinées à part (lignes de `HEADS_FILE`) : cadres des murs
/// (`item_frame`, voir `frame_line`), et têtes portées par des porte-armures (avec leur
/// bâti s'il est visible), des entités d'affichage ou des cadres (voir `entity_heads`).
fn read_entities(dir: &Path, y_offset: i32) -> Vec<String> {
    let Ok(rd) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for f in rd.flatten() {
        let Ok(bytes) = fs::read(f.path()) else {
            continue;
        };
        for root in read_region(&bytes) {
            for e in root.get("Entities").map(Nbt::as_list).unwrap_or(&[]) {
                entity_lines(e, None, y_offset, &mut out);
            }
        }
    }
    out.sort();
    out
}

/// Lignes d'une entité et de ses passagers (`vehicle` : position de celle qui les porte).
fn entity_lines(e: &Nbt, vehicle: Option<[f64; 3]>, y_offset: i32, out: &mut Vec<String>) {
    let pos = match e.get("Pos").map(Nbt::as_list) {
        Some([x, y, z]) => match (x.as_f64(), y.as_f64(), z.as_f64()) {
            (Some(x), Some(y), Some(z)) => Some([x, y, z]),
            _ => vehicle,
        },
        _ => vehicle,
    };
    for p in e.get("Passengers").map(Nbt::as_list).unwrap_or(&[]) {
        entity_lines(p, pos, y_offset, out);
    }
    if let Some(l) = frame_line(e, y_offset) {
        out.push(l);
    }
    if let Some(pos) = pos {
        entity_heads(e, pos, y_offset, out);
    }
}

/// Têtes portées par une entité en `pos` (pieds, monde de Minecraft) : lignes `Free` de
/// `heads` (et `Stand` pour le bâti d'un porte-armure visible).
fn entity_heads(e: &Nbt, pos: [f64; 3], y_offset: i32, out: &mut Vec<String>) {
    use crate::heads::{self, Head, Pose};
    use glam::Vec3;
    let id = e.get("id").and_then(Nbt::as_str).unwrap_or("");
    let floats = |k: &str| -> Vec<f32> {
        e.get(k)
            .map(Nbt::as_list)
            .unwrap_or(&[])
            .iter()
            .filter_map(|v| v.as_f64().map(|f| f as f32))
            .collect()
    };
    let rot = floats("Rotation");
    let (yaw, pitch) = (
        rot.first().copied().unwrap_or(0.0),
        rot.get(1).copied().unwrap_or(0.0),
    );
    let origin = Vec3::new(
        pos[0] as f32,
        pos[1] as f32 + y_offset as f32,
        pos[2] as f32,
    );
    let flag = |k: &str| e.get(k).and_then(Nbt::as_i64).unwrap_or(0) != 0;
    fn push(out: &mut Vec<String>, m: glam::Mat4, skin: crate::heads::HeadSkin) {
        // Garde-fou : une entité mal lue ne fait pas une tête géante ou perdue.
        let ok = m.to_cols_array().iter().all(|f| f.is_finite())
            && m.transform_vector3(glam::Vec3::X).length() < 2.0;
        if ok {
            out.push(crate::heads::line(&crate::heads::Head {
                pos: crate::heads::cell_of(m),
                pose: crate::heads::Pose::Free(crate::heads::pack(m)),
                skin,
            }));
        }
    }
    match id {
        "minecraft:armor_stand" => {
            let small = flag("Small");
            // 1.20.5+ : `equipment.head` ; avant : `ArmorItems` (pieds … tête).
            let head = e
                .path(&["equipment", "head"])
                .or_else(|| e.get("ArmorItems").and_then(|l| l.as_list().get(3)));
            if !flag("Invisible") && !flag("Marker") {
                let m = heads::armor_stand_body(origin, yaw, small);
                let flags = u8::from(flag("NoBasePlate")) | u8::from(flag("ShowArms")) << 1;
                let skin = heads::HeadSkin::Default;
                // La case du bâti : celle des pieds.
                out.push(heads::line(&Head {
                    pos: [
                        origin.x.floor() as i32,
                        origin.y.floor() as i32,
                        origin.z.floor() as i32,
                    ],
                    pose: Pose::Stand(heads::pack(m), flags),
                    skin,
                }));
            }
            if let Some(skin) = head.and_then(item_head) {
                let p: Vec<f32> = e
                    .path(&["Pose", "Head"])
                    .map(Nbt::as_list)
                    .unwrap_or(&[])
                    .iter()
                    .filter_map(|v| v.as_f64().map(|f| f as f32))
                    .collect();
                let angles = match p.as_slice() {
                    [x, y, z] => [*x, *y, *z],
                    _ => [0.0; 3],
                };
                let m = heads::armor_stand_head(origin, yaw, angles, small);
                push(out, m, skin);
            }
        }
        "minecraft:item_display" => {
            let Some(skin) = e.get("item").and_then(item_head) else {
                return;
            };
            let frame = heads::display_frame(origin, yaw, pitch, display_transformation(e));
            let m = heads::display_head(frame);
            push(out, m, skin);
        }
        "minecraft:item_frame" | "minecraft:glow_item_frame" => {
            let Some(skin) = e.get("Item").or_else(|| e.get("item")).and_then(item_head) else {
                return;
            };
            let Some((cell, facing)) = frame_cell(e, y_offset) else {
                return;
            };
            let turn = e.get("ItemRotation").and_then(Nbt::as_i64).unwrap_or(0);
            let m = heads::frame_head(cell, facing, (turn & 7) as u8, flag("Invisible"));
            push(out, m, skin);
        }
        _ => {}
    }
}

/// `transformation` d'une entité d'affichage : décomposée (translation, rotation gauche,
/// échelle, rotation droite ; rotations en quaternion ou angle + axe) ou en matrice 4 × 4
/// (par lignes).
fn display_transformation(e: &Nbt) -> glam::Mat4 {
    use glam::{Mat4, Quat, Vec3};
    let Some(t) = e.get("transformation") else {
        return Mat4::IDENTITY;
    };
    let nums = |n: &Nbt| -> Vec<f32> {
        n.as_list()
            .iter()
            .filter_map(|v| v.as_f64().map(|f| f as f32))
            .collect()
    };
    if let Ok(a) = <[f32; 16]>::try_from(nums(t).as_slice()) {
        return Mat4::from_cols_array(&a).transpose();
    }
    let v3 = |k: &str, d: f32| match t.get(k).map(nums).as_deref() {
        Some([x, y, z]) => Vec3::new(*x, *y, *z),
        _ => Vec3::splat(d),
    };
    let quat = |k: &str| -> Quat {
        let Some(q) = t.get(k) else {
            return Quat::IDENTITY;
        };
        match nums(q).as_slice() {
            [x, y, z, w] => Quat::from_xyzw(*x, *y, *z, *w).normalize(),
            _ => {
                let angle = q.get("angle").and_then(Nbt::as_f64).unwrap_or(0.0) as f32;
                match q.get("axis").map(nums).as_deref() {
                    Some([x, y, z]) => {
                        let axis = Vec3::new(*x, *y, *z).normalize_or_zero();
                        if axis == Vec3::ZERO {
                            Quat::IDENTITY
                        } else {
                            Quat::from_axis_angle(axis, angle)
                        }
                    }
                    _ => Quat::IDENTITY,
                }
            }
        }
    };
    Mat4::from_translation(v3("translation", 0.0))
        * Mat4::from_quat(quat("left_rotation"))
        * Mat4::from_scale(v3("scale", 1.0))
        * Mat4::from_quat(quat("right_rotation"))
}

/// Case d'un cadre (devant le mur) et le côté vers lequel il regarde.
fn frame_cell(e: &Nbt, y_offset: i32) -> Option<([i32; 3], u8)> {
    let facing = e.get("Facing").and_then(Nbt::as_i64)?.clamp(0, 5) as u8;
    let tile = |k: &str| e.get(k).and_then(Nbt::as_i64);
    let (x, y, z) = match e.get("block_pos") {
        Some(Nbt::IntArray(a)) if a.len() == 3 => (a[0] as i64, a[1] as i64, a[2] as i64),
        _ => (tile("TileX")?, tile("TileY")?, tile("TileZ")?),
    };
    Some(([x as i32, y as i32 + y_offset, z as i32], facing))
}

/// Cadre (`item_frame`) : ligne `x,y,z,f<côté>,bloc:<clé>` de `HEADS_FILE` (côté vers
/// lequel le cadre regarde, comme Minecraft : 0 bas, 1 haut, 2 nord, 3 sud, 4 ouest, 5 est ;
/// `bloc:` vide : cadre vide visible). Une tête qu'il tient est une ligne de `heads`.
fn frame_line(e: &Nbt, y_offset: i32) -> Option<String> {
    let id = e.get("id").and_then(Nbt::as_str)?;
    if id != "minecraft:item_frame" && id != "minecraft:glow_item_frame" {
        return None;
    }
    let ([x, y, z], facing) = frame_cell(e, y_offset)?;
    let invisible = e.get("Invisible").and_then(Nbt::as_i64).unwrap_or(0) != 0;
    let it = e.get("Item").or_else(|| e.get("item"));
    let item = it
        .and_then(|i| i.get("id"))
        .and_then(Nbt::as_str)
        .unwrap_or("");
    let n = item.strip_prefix("minecraft:").unwrap_or(item);
    let reg = registry::registry();
    let content = if item.is_empty() || it.and_then(item_head).is_some() {
        // Vide, ou une tête (dessinée avec les têtes).
        "bloc:".to_string()
    } else if reg.item_texture(n).is_some() {
        // Un objet (bâton, bouteille, pain…) : son image du pack.
        format!("objet:{n}")
    } else {
        let b = map_block_state(item, &HashMap::new());
        let d = b.def();
        // Un bloc : en volume (plein, tapis, verre) ou à plat (vitre, fleur, rail…).
        if b == BlockId::AIR
            || (b == BlockId::STONE && !n.contains("stone"))
            || d.hidden
            || matches!(d.shape, registry::Shape::Invisible)
        {
            "bloc:".to_string()
        } else {
            format!("bloc:{}", d.key)
        }
    };
    // Invisible et sans rien à montrer : il n'y a rien à voir.
    if invisible && content == "bloc:" {
        return None;
    }
    let kind = if invisible { 'i' } else { 'f' };
    Some(format!("{x},{y},{z},{kind}{facing},{content}"))
}

/// Cases couvertes par une entité d'affichage (et ses passagers).
fn display_cells(
    e: &Nbt,
    vehicle: Option<[f64; 3]>,
    y_offset: i32,
    out: &mut Vec<([i32; 3], BlockId, bool)>,
) {
    use glam::{Mat4, Quat, Vec3};
    let list = |k: &str| -> Vec<f64> {
        e.get(k)
            .map(Nbt::as_list)
            .unwrap_or(&[])
            .iter()
            .filter_map(Nbt::as_f64)
            .collect()
    };
    let pos = match list("Pos").as_slice() {
        [x, y, z] => [*x, *y, *z],
        _ => match vehicle {
            Some(v) => v,
            None => return,
        },
    };
    for p in e.get("Passengers").map(Nbt::as_list).unwrap_or(&[]) {
        display_cells(p, Some(pos), y_offset, out);
    }
    let id = e.get("id").and_then(Nbt::as_str).unwrap_or("");
    let (block, centered) = match id {
        "minecraft:block_display" => {
            let Some(bs) = e.get("block_state") else {
                return;
            };
            let name = bs
                .get("Name")
                .and_then(Nbt::as_str)
                .unwrap_or("minecraft:air");
            let mut props = HashMap::new();
            if let Some(Nbt::Compound(m)) = bs.get("Properties") {
                for (k, v) in m {
                    if let Some(v) = v.as_str() {
                        props.insert(k.clone(), v.to_string());
                    }
                }
            }
            (map_block_state(name, &props), false)
        }
        "minecraft:item_display" => {
            // Une tête est dessinée à part, à sa place exacte (`entity_heads`).
            if e.get("item").and_then(item_head).is_some() {
                return;
            }
            let item = e.path(&["item", "id"]).and_then(Nbt::as_str).unwrap_or("");
            let b = map_block_state(item, &HashMap::new());
            // Objet qui n'est pas un bloc (casque, épée…) : le repli « pierre » de la table.
            if b == BlockId::STONE && !item.contains("stone") {
                return;
            }
            (b, true)
        }
        _ => return,
    };
    if block == BlockId::AIR {
        return;
    }
    let f3 = |v: &[f64], d: f32| -> Vec3 {
        match v {
            [x, y, z, ..] => Vec3::new(*x as f32, *y as f32, *z as f32),
            _ => Vec3::splat(d),
        }
    };
    let quat = |v: &[f64]| -> Quat {
        match v {
            [x, y, z, w] => Quat::from_xyzw(*x as f32, *y as f32, *z as f32, *w as f32).normalize(),
            _ => Quat::IDENTITY,
        }
    };
    let t = |k: &str| -> Vec<f64> {
        e.path(&["transformation", k])
            .map(Nbt::as_list)
            .unwrap_or(&[])
            .iter()
            .filter_map(Nbt::as_f64)
            .collect()
    };
    let rot = list("Rotation");
    let (yaw, pitch) = match rot.as_slice() {
        [y, p] => (*y as f32, *p as f32),
        _ => (0.0, 0.0),
    };
    let entity =
        Quat::from_rotation_y((-yaw).to_radians()) * Quat::from_rotation_x(pitch.to_radians());
    let origin = Vec3::new(
        pos[0] as f32,
        pos[1] as f32 + y_offset as f32,
        pos[2] as f32,
    );
    let m = Mat4::from_translation(origin)
        * Mat4::from_quat(entity)
        * Mat4::from_translation(f3(&t("translation"), 0.0))
        * Mat4::from_quat(quat(&t("left_rotation")))
        * Mat4::from_scale(f3(&t("scale"), 1.0))
        * Mat4::from_quat(quat(&t("right_rotation")));
    if m.determinant().abs() < 1e-6 {
        return;
    }
    let (lo, hi) = if centered { (-0.5, 0.5) } else { (0.0, 1.0) };
    let corners = (0..8).map(|i| {
        m.transform_point3(Vec3::new(
            if i & 1 == 0 { lo } else { hi },
            if i & 2 == 0 { lo } else { hi },
            if i & 4 == 0 { lo } else { hi },
        ))
    });
    let (mut mn, mut mx) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
    for c in corners {
        mn = mn.min(c);
        mx = mx.max(c);
    }
    // Garde-fou : un décor géant mal lu ne remplit pas la carte.
    if (mx - mn).max_element() > 16.0 {
        return;
    }
    let inv = m.inverse();
    let eps = 1e-3;
    // Épaisseur de moins d'une demi-case dans un sens : plaque fine (panneau, affiche).
    let len = |i: usize| m.col(i).truncate().length() * (hi - lo);
    let thin = (0..3).any(|i| len(i) < 0.5);
    let before = out.len();
    for x in mn.x.floor() as i32..=mx.x.floor() as i32 {
        for y in mn.y.floor() as i32..=mx.y.floor() as i32 {
            for z in mn.z.floor() as i32..=mx.z.floor() as i32 {
                let l =
                    inv.transform_point3(Vec3::new(x as f32 + 0.5, y as f32 + 0.5, z as f32 + 0.5));
                if !thin
                    && l.cmpge(Vec3::splat(lo - eps)).all()
                    && l.cmple(Vec3::splat(hi + eps)).all()
                {
                    out.push(([x, y, z], block, false));
                }
            }
        }
    }
    // Plaque fine (panneau, affiche) qui ne couvre aucun centre de case : les cases
    // qu'elle traverse, seulement là où la carte a mis une barrière (sa collision).
    if out.len() == before {
        let n = |i: usize| ((len(i) / 0.3).ceil() as usize).clamp(1, 64);
        let (nx, ny, nz) = (n(0), n(1), n(2));
        let mut seen = std::collections::HashSet::new();
        for i in 0..nx {
            for j in 0..ny {
                for k in 0..nz {
                    let f = |c: usize, n: usize| lo + (hi - lo) * (c as f32 + 0.5) / n as f32;
                    let w = m.transform_point3(Vec3::new(f(i, nx), f(j, ny), f(k, nz)));
                    let c = [w.x.floor() as i32, w.y.floor() as i32, w.z.floor() as i32];
                    if seen.insert(c) {
                        out.push((c, block, true));
                    }
                }
            }
        }
    }
}

/// Monde Minecraft converti en mémoire.
pub struct Import {
    pub level: McLevel,
    pub y_offset: i32,
    pub chunks: HashMap<ChunkPos, Vec<u8>>,
    pub torches: Vec<[i32; 3]>,
    pub heads: Vec<String>,
}

/// Convertit tout le dossier `region/` (dimension principale).
pub fn import_world(dir: &Path) -> Result<Import> {
    let level = read_level(dir)?;
    // 1.18 (21w37a) a abaissé le bas du monde à Y = −64.
    let y_offset = if level.data_version >= 2825 { 64 } else { 0 };
    let mut built: HashMap<ChunkPos, Chunk> = HashMap::new();
    let mut torches = Vec::new();
    let mut heads = Vec::new();
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
                    heads.extend(c.heads);
                    built.insert(c.chunk.pos, c.chunk);
                }
                Err(_) => skipped += 1,
            }
        }
    }
    // Décors faits d'entités d'affichage (1.19.4+) : blocs et objets posés dans le vide ou
    // dans des barrières (qui en font la collision).
    // Cadres (objets accrochés aux murs) : dessinés à part, comme les têtes.
    heads.extend(read_entities(&dir.join("entities"), y_offset));
    let shown = import_displays(&dir.join("entities"), y_offset, &mut built);
    if shown > 0 {
        log::info!("{shown} blocs d'entités d'affichage ajoutés");
    }
    let chunks: HashMap<ChunkPos, Vec<u8>> =
        built.into_iter().map(|(p, c)| (p, c.to_rle())).collect();
    log::info!(
        "monde Minecraft « {} » : {} chunks convertis, {} ignorés",
        level.name,
        chunks.len(),
        skipped
    );
    heads.sort();
    Ok(Import {
        level,
        y_offset,
        chunks,
        torches,
        heads,
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

    #[test]
    fn display_entities_fill_the_cells_they_show() {
        let d = |v: &[f64]| Nbt::List(v.iter().map(|&x| Nbt::Double(x)).collect());
        let f = |v: &[f64]| Nbt::List(v.iter().map(|&x| Nbt::Float(x as f32)).collect());
        let shown = |name: &str, pos: &[f64], tr: &[f64], scale: &[f64]| {
            compound(vec![
                ("id", Nbt::String("minecraft:block_display".into())),
                ("Pos", d(pos)),
                ("block_state", block(name)),
                (
                    "transformation",
                    compound(vec![
                        ("translation", f(tr)),
                        ("scale", f(scale)),
                        ("left_rotation", f(&[0.0, 0.0, 0.0, 1.0])),
                        ("right_rotation", f(&[0.0, 0.0, 0.0, 1.0])),
                    ]),
                ),
            ])
        };
        // Un bloc centré sur sa position, et un véhicule « air » qui porte un pavé de 2×1×1.
        let mut out = Vec::new();
        display_cells(
            &shown(
                "minecraft:white_concrete",
                &[10.5, 70.5, 10.5],
                &[-0.5, -0.5, -0.5],
                &[1.0, 1.0, 1.0],
            ),
            None,
            64,
            &mut out,
        );
        let mut carrier = shown(
            "minecraft:air",
            &[3.0, 70.0, 3.0],
            &[0.0, 0.0, 0.0],
            &[1.0, 1.0, 1.0],
        );
        if let Nbt::Compound(m) = &mut carrier {
            m.insert(
                "Passengers".into(),
                Nbt::List(vec![shown(
                    "minecraft:quartz_block",
                    &[3.0, 70.0, 3.0],
                    &[0.0, 0.0, 0.0],
                    &[2.0, 1.0, 1.0],
                )]),
            );
        }
        display_cells(&carrier, None, 64, &mut out);
        // Plaque fine : seulement dans une barrière.
        display_cells(
            &shown(
                "minecraft:yellow_concrete",
                &[20.0, 70.0, 20.0],
                &[0.0, 0.0, 0.5],
                &[1.0, 1.0, 0.05],
            ),
            None,
            64,
            &mut out,
        );
        let at = |p: [i32; 3]| {
            out.iter()
                .find(|c| c.0 == p)
                .map(|c| (c.1.def().key.clone(), c.2))
        };
        assert_eq!(at([10, 134, 10]), Some(("concrete_white".into(), false)));
        assert_eq!(at([3, 134, 3]), Some(("quartz".into(), false)));
        assert_eq!(at([4, 134, 3]), Some(("quartz".into(), false)));
        assert_eq!(at([20, 134, 20]), Some(("concrete_yellow".into(), true)));
        assert_eq!(out.len(), 4, "{out:?}");
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
        // Ardoise des abîmes à Y = −64 : (v179) son propre bloc.
        assert_eq!(
            c.chunk.get(0, 0, 0),
            registry::registry().by_key("deepslate").unwrap()
        );
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

#[cfg(test)]
mod dump_tests {
    use super::*;

    /// États des chunks d'un dossier `region/` (`MC_REGION_DIR`) et erreurs de
    /// conversion.
    #[test]
    #[ignore]
    fn region_statuses() {
        let Some(dir) = std::env::var_os("MC_REGION_DIR") else {
            return;
        };
        let mut counts: HashMap<String, usize> = HashMap::new();
        for f in fs::read_dir(dir).unwrap().flatten() {
            let bytes = fs::read(f.path()).unwrap();
            for nbt in read_region(&bytes) {
                let status = nbt
                    .get("Status")
                    .or_else(|| nbt.path(&["Level", "Status"]))
                    .and_then(Nbt::as_str)
                    .unwrap_or("?")
                    .to_string();
                let r = match convert_chunk(&nbt, 64) {
                    Ok(c) => {
                        let mut n = 0;
                        for y in 0..CHUNK_HEIGHT {
                            for z in 0..16 {
                                for x in 0..16 {
                                    n += (c.chunk.get(x, y, z) != BlockId::AIR) as usize;
                                }
                            }
                        }
                        if n > 0 {
                            "pleins".to_string()
                        } else {
                            "vides".to_string()
                        }
                    }
                    Err(e) => format!("{e:#}"),
                };
                let v = nbt.get("DataVersion").and_then(Nbt::as_i64).unwrap_or(0);
                *counts.entry(format!("{status} v{v} {r}")).or_default() += 1;
            }
        }
        let mut v: Vec<_> = counts.into_iter().collect();
        v.sort_by_key(|x| std::cmp::Reverse(x.1));
        for (k, n) in v {
            eprintln!("{n:6} {k}");
        }
    }

    /// Noms de blocs d'une région Minecraft qui deviennent de l'air (`MC_REGION=…`).
    #[test]
    #[ignore]
    fn region_names_mapped_to_air() {
        let Some(path) = std::env::var_os("MC_REGION") else {
            return;
        };
        let bytes = std::fs::read(path).unwrap();
        let mut counts: HashMap<String, usize> = HashMap::new();
        for root in read_region(&bytes) {
            let level = root.get("Level").unwrap_or(&root);
            let sections = level
                .get("sections")
                .or_else(|| level.get("Sections"))
                .map(Nbt::as_list)
                .unwrap_or(&[]);
            for sec in sections {
                let palette = sec
                    .get("block_states")
                    .and_then(|b| b.get("palette"))
                    .or_else(|| sec.get("Palette"));
                for p in palette.map(Nbt::as_list).unwrap_or(&[]) {
                    let name = p.get("Name").and_then(Nbt::as_str).unwrap_or("");
                    let props: HashMap<String, String> = match p.get("Properties") {
                        Some(Nbt::Compound(m)) => m
                            .iter()
                            .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned())))
                            .collect(),
                        _ => HashMap::new(),
                    };
                    let id = map_block_state(name, &props);
                    let mut pv: Vec<_> = props.iter().map(|(k, v)| format!("{k}={v}")).collect();
                    pv.sort();
                    let key = if name.contains("stairs") {
                        format!("{name}[{}] -> {}", pv.join(","), id.def().key)
                    } else {
                        format!("{name} -> {}", id.def().key)
                    };
                    *counts.entry(key).or_default() += 1;
                }
            }
        }
        let mut v: Vec<_> = counts.into_iter().collect();
        v.sort_by_key(|x| std::cmp::Reverse(x.1));
        for (k, n) in v {
            eprintln!("{n:6} {k}");
        }
    }
}

#[cfg(test)]
mod dump_displays {
    use super::*;

    /// Blocs d'entités d'affichage d'un dossier `entities/` (`MC_ENTITIES`).
    #[test]
    #[ignore]
    fn entity_kinds() {
        let Some(dir) = std::env::var_os("MC_ENTITIES") else {
            return;
        };
        let mut counts: HashMap<String, usize> = HashMap::new();
        for f in fs::read_dir(&dir).unwrap().flatten() {
            let bytes = fs::read(f.path()).unwrap();
            for root in read_region(&bytes) {
                for e in root.get("Entities").map(Nbt::as_list).unwrap_or(&[]) {
                    let id = e.get("id").and_then(Nbt::as_str).unwrap_or("?");
                    let what = e
                        .path(&["block_state", "Name"])
                        .or_else(|| e.path(&["item", "id"]))
                        .or_else(|| e.path(&["Item", "id"]))
                        .and_then(Nbt::as_str)
                        .unwrap_or("");
                    let mut cells = Vec::new();
                    display_cells(e, None, 64, &mut cells);
                    *counts
                        .entry(format!("{id} {what} cells={}", cells.len().min(1)))
                        .or_default() += 1;
                }
            }
        }
        let mut v: Vec<_> = counts.into_iter().collect();
        v.sort_by_key(|x| std::cmp::Reverse(x.1));
        for (k, n) in v {
            eprintln!("{n:6} {k}");
        }
        let frames = read_entities(Path::new(&dir), 64);
        eprintln!(
            "{} cadres, ex. {:?}",
            frames.len(),
            &frames[..frames.len().min(5)]
        );
        for f in &frames {
            eprintln!("CADRE {f}");
        }
    }

    #[test]
    #[ignore]
    fn displays() {
        let Some(dir) = std::env::var_os("MC_ENTITIES") else {
            return;
        };
        let mut out = Vec::new();
        for f in fs::read_dir(dir).unwrap().flatten() {
            let bytes = fs::read(f.path()).unwrap();
            for root in read_region(&bytes) {
                for e in root.get("Entities").map(Nbt::as_list).unwrap_or(&[]) {
                    display_cells(e, None, 64, &mut out);
                }
            }
        }
        // Têtes à skin des régions voisines (`MC_REGIONS`).
        if let Some(rdir) = std::env::var_os("MC_REGIONS") {
            let mut heads = Vec::new();
            for f in fs::read_dir(rdir).unwrap().flatten() {
                let bytes = fs::read(f.path()).unwrap();
                for root in read_region(&bytes) {
                    if let Ok(c) = convert_chunk(&root, 64) {
                        heads.extend(c.heads);
                    }
                }
            }
            println!(
                "{} têtes, ex. {:?}",
                heads.len(),
                &heads[..heads.len().min(3)]
            );
            let poses: HashMap<String, usize> = heads.iter().fold(HashMap::new(), |mut m, h| {
                *m.entry(h.split(',').nth(3).unwrap().to_string())
                    .or_default() += 1;
                m
            });
            println!("poses {poses:?}");
        }
        let mut count: HashMap<String, usize> = HashMap::new();
        for (_, b, _) in &out {
            *count.entry(b.def().key.clone()).or_default() += 1;
        }
        println!("{} cases : {count:?}", out.len());
        for (p, b, _t) in &out {
            if (p[0] == -33 && p[2] == -230)
                || ((4..=7).contains(&p[0]) && (-218..=-216).contains(&p[2]))
            {
                println!("{p:?} {} {_t}", b.def().key);
            }
        }
    }
}

#[cfg(test)]
mod dump_column {
    use super::*;

    /// Colonne de blocs d'une région Minecraft (`MC_REGION`, `MC_X`, `MC_Z`).
    #[test]
    #[ignore]
    fn column() {
        let Some(path) = std::env::var_os("MC_REGION") else {
            return;
        };
        let gx: i32 = std::env::var("MC_X").unwrap().parse().unwrap();
        let gz: i32 = std::env::var("MC_Z").unwrap().parse().unwrap();
        let bytes = std::fs::read(path).unwrap();
        for root in read_region(&bytes) {
            let level = root.get("Level").unwrap_or(&root);
            let x = level.get("xPos").and_then(Nbt::as_i64).unwrap() as i32;
            let z = level.get("zPos").and_then(Nbt::as_i64).unwrap() as i32;
            if x != gx.div_euclid(16) || z != gz.div_euclid(16) {
                continue;
            }
            let (lx, lz) = (gx.rem_euclid(16) as usize, gz.rem_euclid(16) as usize);
            for sec in level.get("sections").map(Nbt::as_list).unwrap_or(&[]) {
                let sy = sec.get("Y").and_then(Nbt::as_i64).unwrap_or(0) as i32;
                let bs = sec.get("block_states");
                let pal = bs
                    .and_then(|b| b.get("palette"))
                    .map(Nbt::as_list)
                    .unwrap_or(&[]);
                let names: Vec<String> = pal
                    .iter()
                    .map(|p| {
                        p.get("Name")
                            .and_then(Nbt::as_str)
                            .unwrap_or("")
                            .to_string()
                    })
                    .collect();
                for ly in 0..16usize {
                    let i = ly * 256 + lz * 16 + lx;
                    let k = match bs.and_then(|b| b.get("data")) {
                        Some(Nbt::LongArray(d)) if names.len() > 1 => {
                            packed(d, i, bits_for(names.len()), false)
                        }
                        _ => 0,
                    };
                    let n = names.get(k).cloned().unwrap_or_default();
                    if n != "minecraft:air" {
                        eprintln!("y {} {n}", sy * 16 + ly as i32);
                    }
                }
            }
            let c = convert_chunk(&root, 64).unwrap();
            for y in 160..172 {
                eprintln!("minelib y {y} {}", c.chunk.get(lx, y, lz).def().key);
            }
        }
    }
}

#[cfg(test)]
mod dump_area {
    use super::*;

    /// Blocs non pleins d'une zone (`MC_REGION`, `MC_Y` en coordonnées Minelib).
    #[test]
    #[ignore]
    fn area() {
        let Some(path) = std::env::var_os("MC_REGION") else {
            return;
        };
        let y: usize = std::env::var("MC_Y").unwrap().parse().unwrap();
        let bytes = std::fs::read(path).unwrap();
        let mut counts: HashMap<String, usize> = HashMap::new();
        for root in read_region(&bytes) {
            let Ok(c) = convert_chunk(&root, 64) else {
                continue;
            };
            for lz in 0..16 {
                for lx in 0..16 {
                    let b = c.chunk.get(lx, y, lz);
                    let up = c.chunk.get(lx, y + 1, lz);
                    *counts
                        .entry(format!("{} / dessus {}", b.def().key, up.def().key))
                        .or_default() += 1;
                }
            }
        }
        let mut v: Vec<_> = counts.into_iter().collect();
        v.sort_by_key(|x| std::cmp::Reverse(x.1));
        for (k, n) in v.iter().take(60) {
            eprintln!("{n:6} {k}");
        }
    }
}

#[cfg(test)]
mod block_audit {
    use super::*;

    /// (v165) Revue des blocs d'une vraie carte, un par un (`MC_WORLDS` : dossiers de
    /// mondes Minecraft séparés par `;`) : chaque état de bloc employé, combien de fois,
    /// et ce qu'il devient. Ceux qui passent par le repli par nom (`map_block_name`) sont
    /// devinés : à vérifier. Écrit le rapport dans `MC_AUDIT` (sinon à l'écran).
    #[test]
    #[ignore]
    fn audit_real_world_blocks() {
        let Some(dirs) = std::env::var_os("MC_WORLDS") else {
            return;
        };
        let dirs = dirs.to_string_lossy().to_string();
        // (nom, propriétés triées) → (nombre, cartes)
        let mut seen: std::collections::BTreeMap<
            (String, String),
            (u64, std::collections::BTreeSet<String>),
        > = Default::default();
        for dir in dirs.split(';').filter(|d| !d.is_empty()) {
            let map = Path::new(dir)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let Ok(rd) = std::fs::read_dir(Path::new(dir).join("region")) else {
                continue;
            };
            for f in rd.flatten() {
                let Ok(bytes) = std::fs::read(f.path()) else {
                    continue;
                };
                for root in read_region(&bytes) {
                    let level = root.get("Level").unwrap_or(&root);
                    let sections = level
                        .get("sections")
                        .or_else(|| level.get("Sections"))
                        .map(Nbt::as_list)
                        .unwrap_or(&[]);
                    for sec in sections {
                        let (palette, states) = match sec.get("block_states") {
                            Some(bs) => (bs.get("palette"), bs.get("data")),
                            None => (sec.get("Palette"), sec.get("BlockStates")),
                        };
                        let Some(palette) = palette else { continue };
                        let list = palette.as_list();
                        // Nombre de cases de chaque entrée de la palette.
                        let mut counts = vec![0u64; list.len()];
                        match states {
                            Some(Nbt::LongArray(data)) if list.len() > 1 => {
                                let bits = bits_for(list.len());
                                let dv = root.get("DataVersion").and_then(Nbt::as_i64).unwrap_or(0);
                                for i in 0..4096 {
                                    let k = packed(data, i, bits, dv < 2529);
                                    if let Some(c) = counts.get_mut(k) {
                                        *c += 1;
                                    }
                                }
                            }
                            _ => {
                                if let Some(c) = counts.first_mut() {
                                    *c += 4096;
                                }
                            }
                        }
                        for (p, n) in list.iter().zip(counts) {
                            if n == 0 {
                                continue;
                            }
                            let name = p.get("Name").and_then(Nbt::as_str).unwrap_or("?");
                            let mut props: Vec<(String, String)> = match p.get("Properties") {
                                Some(Nbt::Compound(m)) => m
                                    .iter()
                                    .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned())))
                                    .collect(),
                                _ => Vec::new(),
                            };
                            props.sort();
                            let key = props
                                .iter()
                                .map(|(k, v)| format!("{k}={v}"))
                                .collect::<Vec<_>>()
                                .join(",");
                            let e = seen.entry((name.to_string(), key)).or_default();
                            e.0 += n;
                            e.1.insert(map.clone());
                        }
                    }
                }
            }
        }
        // Par nom de bloc : total, comment il est converti, et les états qui donnent un
        // autre bloc que le premier.
        let mut by_name: std::collections::BTreeMap<String, Vec<(String, u64, String, String)>> =
            Default::default();
        for ((name, key), (n, maps)) in &seen {
            let props: HashMap<String, String> = key
                .split(',')
                .filter_map(|kv| kv.split_once('='))
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            let how = if added_block(name, &props).is_some() {
                "ajouté"
            } else if recent_block(name).is_some() {
                "proche"
            } else if modern_block(name, &props).is_some() {
                "exact"
            } else {
                "DEVINÉ"
            };
            let id = map_block_state(name, &props);
            let to = format!("{} ({how})", id.def().key);
            let maps = maps.iter().cloned().collect::<Vec<_>>().join(" | ");
            by_name
                .entry(name.clone())
                .or_default()
                .push((key.clone(), *n, to, maps));
        }
        let mut out = String::new();
        let mut rows: Vec<_> = by_name.iter().collect();
        // Les devinés d'abord, puis les plus employés.
        rows.sort_by_key(|(_, v)| {
            let guessed = v.iter().any(|x| x.2.contains("DEVINÉ"));
            let total: u64 = v.iter().map(|x| x.1).sum();
            (!guessed, std::cmp::Reverse(total))
        });
        for (name, states) in rows {
            let total: u64 = states.iter().map(|x| x.1).sum();
            out += &format!("{name}  ×{total}\n");
            for (key, n, to, maps) in states {
                out += &format!("    [{key}] ×{n} → {to}   ({maps})\n");
            }
        }
        match std::env::var_os("MC_AUDIT") {
            Some(p) => std::fs::write(p, out).expect("rapport"),
            None => println!("{out}"),
        }
    }
}

#[cfg(test)]
mod real_heads {
    use super::*;

    /// Têtes d'une vraie carte (`MC_WORLD` : dossier du monde Minecraft) : combien de
    /// chaque sorte, et les têtes d'entités près de leur entité.
    #[test]
    #[ignore]
    fn real_world_heads() {
        let Some(dir) = std::env::var_os("MC_WORLD") else {
            return;
        };
        let imp = import_world(Path::new(&dir)).expect("conversion");
        let mut kinds: std::collections::BTreeMap<String, usize> = Default::default();
        let mut skins: std::collections::BTreeMap<String, usize> = Default::default();
        for l in &imp.heads {
            let pose = l.split(',').nth(3).unwrap_or("");
            let k = match pose.chars().next() {
                Some('f') => "cadre".to_string(),
                Some('w') => "mur".to_string(),
                Some('m') => "entité".to_string(),
                Some('a') => "porte-armure".to_string(),
                _ => format!("sol {pose}"),
            };
            *kinds.entry(k).or_default() += 1;
            if let Some(h) = crate::heads::parse(l) {
                let s = match h.skin {
                    crate::heads::HeadSkin::Default => "défaut",
                    crate::heads::HeadSkin::Mc(_) => "minecraft",
                    crate::heads::HeadSkin::Player(_) => "joueur",
                };
                *skins.entry(s.into()).or_default() += 1;
                // Tête d'entité : un demi-bloc à peu près, dans sa case.
                if let crate::heads::Pose::Free(_) = h.pose {
                    let m = crate::heads::transform(&h);
                    let c = m.transform_point3(glam::Vec3::new(0.0, 4.0, 0.0));
                    let cell = [c.x.floor() as i32, c.y.floor() as i32, c.z.floor() as i32];
                    assert_eq!(cell, h.pos, "{l}");
                }
            } else if !l.split(',').nth(3).unwrap_or("").starts_with('f') {
                panic!("ligne illisible : {l}");
            }
        }
        // Blocs « tête » des chunks : leur pose.
        let mut blocks: std::collections::BTreeMap<String, usize> = Default::default();
        for rle in imp.chunks.values() {
            let c = Chunk::from_rle(ChunkPos { x: 0, z: 0 }, rle).expect("chunk");
            for y in c.bottom()..c.top() {
                for z in 0..16 {
                    for x in 0..16 {
                        let id = c.get(x, y, z);
                        if let Some((wall, n)) = registry::skull_pose(id) {
                            let k = if wall {
                                format!("mur {n}")
                            } else {
                                format!("sol {n}")
                            };
                            *blocks.entry(k).or_default() += 1;
                        }
                    }
                }
            }
        }
        eprintln!("lignes : {kinds:?}\nskins : {skins:?}\nblocs « tête » : {blocks:?}");
        for l in imp
            .heads
            .iter()
            .filter(|l| {
                l.split(',')
                    .nth(3)
                    .is_some_and(|p| p.starts_with(['m', 'a']))
            })
            .take(6)
        {
            eprintln!("  {l}");
        }
    }
    /// Lignes des entités d'un dossier `entities/` (`MC_ENTITIES`) : porte-armures et
    /// têtes d'entités.
    #[test]
    #[ignore]
    fn real_entity_heads() {
        let Some(dir) = std::env::var_os("MC_ENTITIES") else {
            return;
        };
        let lines = read_entities(Path::new(&dir), 64);
        for l in lines.iter().filter(|l| {
            l.split(',')
                .nth(3)
                .is_some_and(|p| p.starts_with(['m', 'a']))
        }) {
            let h = crate::heads::parse(l).expect("ligne");
            let m = crate::heads::transform(&h);
            let c = m.transform_point3(glam::Vec3::new(0.0, 4.0, 0.0));
            let f = m.transform_vector3(-glam::Vec3::Z).normalize();
            eprintln!(
                "{} centre ({:.2}, {:.2}, {:.2}) visage ({:.2}, {:.2}, {:.2})",
                l.split(',')
                    .nth(3)
                    .unwrap_or("")
                    .chars()
                    .next()
                    .unwrap_or('?'),
                c.x,
                c.y,
                c.z,
                f.x,
                f.y,
                f.z
            );
        }
    }
}
