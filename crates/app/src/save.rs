//! Sauvegarde des mondes dans `.minelib/saves/<monde>/` :
//!
//! - `level.txt`   : nom, graine, dates, joueur, horloge, inventaire (texte lisible) ;
//! - `edits.bin`   : blocs modifiés par le joueur (le reste est régénéré depuis la graine) ;
//! - `torches.txt` : torches posées (sel chimique, instant d'allumage) ;
//! - `chunks.bin`  : chunks importés (monde Minecraft converti), compressés.
//!
//! Un dossier qui contient un monde Minecraft (`level.dat` + `region/`) sans `level.txt`
//! est converti au chargement ; la sauvegarde suivante l'écrit au format Minelib à côté
//! des fichiers d'origine, qui ne sont jamais modifiés.

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use minelib_assets::parse_kv;
use minelib_world::gen::Generator;
use minelib_world::Torch;
use minelib_world::{BlockId, ChunkPos, World};

use crate::inventory::{Inventory, Item, Stack, SLOTS};
use crate::mcimport;

#[derive(Clone, Debug)]
pub struct WorldMeta {
    pub name: String,
    pub seed: u32,
    pub created: u64,
    pub last_played: u64,
}

#[derive(Clone, Debug)]
pub struct WorldSummary {
    pub dir: PathBuf,
    pub meta: WorldMeta,
    /// Monde Minecraft pas encore converti.
    pub minecraft: bool,
}

/// État du joueur enregistré.
#[derive(Clone, Copy, Debug)]
pub struct PlayerState {
    pub pos: [f32; 3],
    pub yaw: f32,
    pub pitch: f32,
    pub flying: bool,
}

/// Mode de jeu : survie (cœurs, dégâts de chute, pas de vol), créatif (vol, invulnérable)
/// ou FPS (comme la survie, sans inventaire ni blocs à casser / poser, avec le ping).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum GameMode {
    Survival,
    #[default]
    Creative,
    Fps,
    /// Spectateur : vol forcé, on traverse les blocs, aucune interaction.
    Spectator,
}

impl GameMode {
    pub fn key(self) -> &'static str {
        match self {
            GameMode::Survival => "survival",
            GameMode::Creative => "creative",
            GameMode::Fps => "fps",
            GameMode::Spectator => "spectator",
        }
    }

    /// `0`, `1`, `survival`, `creative` (et en français).
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "0" | "s" | "survival" | "survie" => Some(GameMode::Survival),
            "1" | "c" | "creative" | "créatif" | "creatif" => Some(GameMode::Creative),
            "2" | "fps" => Some(GameMode::Fps),
            "3" | "sp" | "spectator" | "spectateur" => Some(GameMode::Spectator),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            GameMode::Survival => "Survie",
            GameMode::Creative => "Créatif",
            GameMode::Fps => "FPS",
            GameMode::Spectator => "Spectateur",
        }
    }

    /// Cœurs, dégâts, pas de vol sans /fly.
    pub fn mortal(self) -> bool {
        matches!(self, GameMode::Survival | GameMode::Fps)
    }

    /// Inventaire, objet en main, blocs posés : ni en mode FPS ni en spectateur.
    pub fn has_inventory(self) -> bool {
        matches!(self, GameMode::Survival | GameMode::Creative)
    }
}

/// État de jeu du joueur (mode, vol autorisé en survie, santé en demi-cœurs).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GameState {
    pub mode: GameMode,
    pub can_fly: bool,
    pub health: u8,
}

impl Default for GameState {
    fn default() -> Self {
        Self {
            mode: GameMode::Creative,
            can_fly: false,
            health: MAX_HEALTH,
        }
    }
}

/// 20 demi-cœurs = 10 cœurs.
pub const MAX_HEALTH: u8 = 20;

pub struct SaveData {
    pub game: GameState,
    pub meta: WorldMeta,
    pub generator: Generator,
    pub player: Option<PlayerState>,
    pub clock: f64,
    pub night: bool,
    pub inventory: Option<Inventory>,
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Graine à partir du texte saisi : nombre tel quel, sinon hachage du texte, vide = aléatoire.
pub fn parse_seed(text: &str) -> u32 {
    let t = text.trim();
    if t.is_empty() {
        let n = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(1, |d| d.as_nanos() as u64);
        return (n ^ (n >> 29)).wrapping_mul(0x9e37_79b9_7f4a_7c15) as u32;
    }
    if let Ok(v) = t.parse::<i64>() {
        return v as u32;
    }
    t.bytes()
        .fold(2166136261u32, |h, b| (h ^ b as u32).wrapping_mul(16777619))
}

/// Date « jj/mm/aaaa hh:mm » (UTC) depuis un horodatage Unix.
pub fn format_date(ts: u64) -> String {
    let days = (ts / 86400) as i64;
    let secs = ts % 86400;
    // Conversion jours → date civile (algorithme de H. Hinnant).
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!(
        "{d:02}/{m:02}/{y} {:02}:{:02}",
        secs / 3600,
        (secs / 60) % 60
    )
}

pub fn list_worlds(saves: &Path) -> Vec<WorldSummary> {
    let mut out: Vec<WorldSummary> = fs::read_dir(saves)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| {
            if let Ok(text) = fs::read_to_string(e.path().join("level.txt")) {
                return Some(WorldSummary {
                    dir: e.path(),
                    meta: parse_meta(&text, &e.file_name().to_string_lossy()),
                    minecraft: false,
                });
            }
            if mcimport::is_minecraft_world(&e.path()) {
                let lvl = mcimport::read_level(&e.path()).ok();
                let meta = WorldMeta {
                    name: lvl.as_ref().map_or_else(
                        || e.file_name().to_string_lossy().to_string(),
                        |l| l.name.clone(),
                    ),
                    seed: lvl.as_ref().map_or(0, |l| l.seed as u32),
                    created: 0,
                    last_played: lvl.as_ref().map_or(0, |l| l.last_played_s),
                };
                return Some(WorldSummary {
                    dir: e.path(),
                    meta,
                    minecraft: true,
                });
            }
            None
        })
        .collect();
    out.sort_by_key(|w| std::cmp::Reverse(w.meta.last_played));
    out
}

fn parse_meta(text: &str, fallback: &str) -> WorldMeta {
    let kv = parse_kv(text);
    let num = |k: &str| kv.get(k).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
    WorldMeta {
        name: kv
            .get("name")
            .cloned()
            .unwrap_or_else(|| fallback.to_owned()),
        seed: kv.get("seed").and_then(|v| v.parse().ok()).unwrap_or(0),
        created: num("created"),
        last_played: num("last_played"),
    }
}

/// Crée le dossier d'un nouveau monde (nom de dossier dérivé du nom, rendu unique).
pub fn create_world(saves: &Path, name: &str, seed: u32) -> Result<PathBuf> {
    let base: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches('_')
        .to_owned();
    let base = if base.is_empty() {
        "monde".to_owned()
    } else {
        base
    };
    let mut dir = saves.join(&base);
    let mut n = 2;
    while dir.exists() {
        dir = saves.join(format!("{base}_{n}"));
        n += 1;
    }
    fs::create_dir_all(&dir).with_context(|| format!("création de {}", dir.display()))?;
    let t = now();
    let meta = WorldMeta {
        name: name.trim().to_owned(),
        seed,
        created: t,
        last_played: t,
    };
    write_level(
        &dir,
        &SaveData {
            game: GameState::default(),
            meta,
            generator: Generator::Procedural,
            player: None,
            clock: 0.0,
            night: false,
            inventory: None,
        },
    )?;
    Ok(dir)
}

fn item_code(item: Item) -> String {
    match item {
        Item::Block(b) => format!("block;{}", b.0),
        Item::Wand => "wand;-".into(),
        Item::Torch(None) => "torch;-".into(),
        Item::Torch(Some(i)) => format!("torch;{i}"),
    }
}

fn parse_item(kind: &str, arg: &str) -> Option<Item> {
    match kind {
        "block" => Some(Item::Block(BlockId(arg.parse().ok()?))),
        "torch" => Some(Item::Torch(arg.parse().ok())),
        "wand" => Some(Item::Wand),
        _ => None,
    }
}

fn write_level(dir: &Path, d: &SaveData) -> Result<()> {
    let mut s = String::new();
    s += &format!(
        "name={}\nseed={}\ncreated={}\nlast_played={}\n",
        d.meta.name, d.meta.seed, d.meta.created, d.meta.last_played
    );
    s += &format!(
        "clock={}\nnight={}\ngenerator={}\n",
        d.clock,
        d.night as u8,
        d.generator.name()
    );
    s += &format!(
        "gamemode={}\nfly={}\nhealth={}\n",
        d.game.mode.key(),
        d.game.can_fly as u8,
        d.game.health
    );
    if let Some(p) = d.player {
        s += &format!(
            "player={},{},{},{},{},{}\n",
            p.pos[0], p.pos[1], p.pos[2], p.yaw, p.pitch, p.flying as u8
        );
    }
    if let Some(inv) = &d.inventory {
        s += &format!("selected={}\n", inv.selected);
        for (i, slot) in inv.slots.iter().enumerate() {
            if let Some(st) = slot {
                s += &format!("slot{i}={};{}\n", item_code(st.item), st.count);
            }
        }
    }
    fs::write(dir.join("level.txt"), s)?;
    Ok(())
}

/// Enregistre tout le monde (méta, joueur, inventaire, modifications, torches).
pub fn save_world(dir: &Path, d: &SaveData, world: &World) -> Result<()> {
    write_level(dir, d)?;
    let mut bin = Vec::new();
    let edits: Vec<_> = world.edits().collect();
    bin.extend_from_slice(&(edits.len() as u32).to_le_bytes());
    for (c, i, b) in edits {
        bin.extend_from_slice(&c.x.to_le_bytes());
        bin.extend_from_slice(&c.z.to_le_bytes());
        bin.extend_from_slice(&i.to_le_bytes());
        bin.extend_from_slice(&b.0.to_le_bytes());
    }
    fs::write(dir.join("edits.bin"), bin)?;
    if world.base_count() > 0 {
        // Écriture dans un fichier temporaire puis renommage : pas de fichier à moitié écrit.
        let mut bin = b"MLCH".to_vec();
        bin.extend_from_slice(&(world.base_count() as u32).to_le_bytes());
        for (c, data) in world.bases() {
            bin.extend_from_slice(&c.x.to_le_bytes());
            bin.extend_from_slice(&c.z.to_le_bytes());
            bin.extend_from_slice(&(data.len() as u32).to_le_bytes());
            bin.extend_from_slice(data);
        }
        let tmp = dir.join("chunks.bin.tmp");
        fs::write(&tmp, bin)?;
        fs::rename(&tmp, dir.join("chunks.bin"))?;
    }
    let mut f = fs::File::create(dir.join("torches.txt"))?;
    for (p, t) in world.torches() {
        let chem = t.chemical.map_or(-1, |c| c as i32);
        let inf = if t.infinite { ",inf" } else { "" };
        writeln!(f, "{},{},{},{},{}{inf}", p[0], p[1], p[2], chem, t.lit_at)?;
    }
    log::info!("monde enregistré : {}", dir.display());
    Ok(())
}

/// Charge un monde : renvoie les données et un `World` avec ses modifications et torches.
pub fn load_world(dir: &Path) -> Result<(SaveData, World)> {
    if !dir.join("level.txt").exists() && mcimport::is_minecraft_world(dir) {
        return import_minecraft(dir);
    }
    let text = fs::read_to_string(dir.join("level.txt"))
        .with_context(|| format!("{}/level.txt", dir.display()))?;
    let kv = parse_kv(&text);
    let meta = parse_meta(
        &text,
        &dir.file_name().unwrap_or_default().to_string_lossy(),
    );
    let player = kv.get("player").and_then(|v| {
        let f: Vec<f32> = v.split(',').filter_map(|x| x.trim().parse().ok()).collect();
        (f.len() >= 6).then(|| PlayerState {
            pos: [f[0], f[1], f[2]],
            yaw: f[3],
            pitch: f[4],
            flying: f[5] != 0.0,
        })
    });
    let inventory = kv.contains_key("selected").then(|| {
        let mut inv = Inventory::empty();
        inv.selected = kv
            .get("selected")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0)
            .min(9);
        for i in 0..SLOTS {
            let Some(v) = kv.get(&format!("slot{i}")) else {
                continue;
            };
            let parts: Vec<&str> = v.split(';').collect();
            if parts.len() == 3 {
                if let (Some(item), Ok(count)) = (parse_item(parts[0], parts[1]), parts[2].parse())
                {
                    inv.slots[i] = Some(Stack { item, count });
                }
            }
        }
        inv
    });
    let data = SaveData {
        game: GameState {
            mode: kv
                .get("gamemode")
                .and_then(|v| GameMode::parse(v))
                .unwrap_or_default(),
            can_fly: kv.get("fly").is_some_and(|v| v == "1"),
            health: kv
                .get("health")
                .and_then(|v| v.parse().ok())
                .unwrap_or(MAX_HEALTH)
                .min(MAX_HEALTH),
        },
        generator: kv
            .get("generator")
            .map_or(Generator::Procedural, |g| Generator::parse(g)),
        clock: kv.get("clock").and_then(|v| v.parse().ok()).unwrap_or(0.0),
        night: kv.get("night").is_some_and(|v| v == "1"),
        meta,
        player,
        inventory,
    };

    let mut world = World::new(data.meta.seed);
    world.generator = data.generator;
    world.clock = data.clock;
    if let Ok(bin) = fs::read(dir.join("edits.bin")) {
        let n = bin
            .get(..4)
            .map_or(0, |b| u32::from_le_bytes(b.try_into().unwrap()) as usize);
        for k in 0..n {
            let o = 4 + k * 14;
            let Some(e) = bin.get(o..o + 14) else { break };
            let x = i32::from_le_bytes(e[0..4].try_into().unwrap());
            let z = i32::from_le_bytes(e[4..8].try_into().unwrap());
            let i = u32::from_le_bytes(e[8..12].try_into().unwrap());
            let b = u16::from_le_bytes(e[12..14].try_into().unwrap());
            world.load_edit(ChunkPos { x, z }, i, BlockId(b));
        }
    }
    if let Ok(bin) = fs::read(dir.join("chunks.bin")) {
        world.set_bases(read_chunks_bin(&bin));
    }
    if let Ok(text) = fs::read_to_string(dir.join("torches.txt")) {
        for line in text.lines() {
            let v: Vec<&str> = line.split(',').collect();
            if !(5..=6).contains(&v.len()) {
                continue;
            }
            let (Ok(x), Ok(y), Ok(z), Ok(c), Ok(t)) = (
                v[0].parse(),
                v[1].parse(),
                v[2].parse(),
                v[3].parse::<i32>(),
                v[4].parse(),
            ) else {
                continue;
            };
            world.load_torch(
                [x, y, z],
                Torch {
                    chemical: (c >= 0).then_some(c as usize),
                    lit_at: t,
                    infinite: v.get(5).is_some_and(|s| s.trim() == "inf"),
                },
            );
        }
    }
    Ok((data, world))
}

fn read_chunks_bin(bin: &[u8]) -> HashMap<ChunkPos, Vec<u8>> {
    let mut out = HashMap::new();
    if bin.get(..4) != Some(b"MLCH") {
        return out;
    }
    let mut o = 8;
    while let Some(h) = bin.get(o..o + 12) {
        let x = i32::from_le_bytes(h[0..4].try_into().unwrap());
        let z = i32::from_le_bytes(h[4..8].try_into().unwrap());
        let n = u32::from_le_bytes(h[8..12].try_into().unwrap()) as usize;
        let Some(data) = bin.get(o + 12..o + 12 + n) else {
            break;
        };
        out.insert(ChunkPos { x, z }, data.to_vec());
        o += 12 + n;
    }
    out
}

/// Convertit un monde Minecraft (lecture seule) en monde Minelib en mémoire.
fn import_minecraft(dir: &Path) -> Result<(SaveData, World)> {
    let imp = mcimport::import_world(dir)?;
    let seed = imp.level.seed as u32;
    let mut world = World::new(seed);
    // Autour de la carte : plat pour un monde « superflat », vide sinon.
    world.generator = if imp.level.generator.contains("flat") {
        Generator::Flat
    } else {
        Generator::Void
    };
    world.set_bases(imp.chunks);
    for t in &imp.torches {
        world.load_torch(
            *t,
            Torch {
                chemical: None,
                lit_at: 0.0,
                infinite: false,
            },
        );
    }
    let off = imp.y_offset as f32;
    let player = match imp
        .level
        .player
        .filter(|(p, _)| p.iter().any(|v| *v != 0.0))
    {
        Some((p, rot)) => PlayerState {
            pos: [p[0] as f32, p[1] as f32 + off, p[2] as f32],
            // Minecraft : lacet 0 = sud (+Z), tangage positif = vers le bas.
            yaw: rot[0].to_radians() + std::f32::consts::PI,
            pitch: -rot[1].to_radians(),
            flying: false,
        },
        None => {
            let [sx, _, sz] = imp.level.spawn;
            world.ensure_chunk(ChunkPos::containing(sx, sz));
            let y = world
                .top_solid(sx, sz)
                .map_or(imp.level.spawn[1] + imp.y_offset, |t| t + 1);
            PlayerState {
                pos: [sx as f32 + 0.5, y as f32, sz as f32 + 0.5],
                yaw: 0.0,
                pitch: 0.0,
                flying: false,
            }
        }
    };
    let t = now();
    let data = SaveData {
        game: GameState::default(),
        generator: world.generator,
        meta: WorldMeta {
            name: imp.level.name.clone(),
            seed,
            created: t,
            last_played: imp.level.last_played_s,
        },
        player: Some(player),
        clock: 0.0,
        night: false,
        inventory: None,
    };
    Ok((data, world))
}

#[cfg(test)]
mod tests {
    #[test]
    fn spectator_mode_parses_and_has_no_inventory() {
        for k in ["3", "sp", "spectator", "spectateur"] {
            assert_eq!(GameMode::parse(k), Some(GameMode::Spectator));
        }
        assert!(!GameMode::Spectator.has_inventory() && !GameMode::Spectator.mortal());
        assert_eq!(
            GameMode::parse(GameMode::Spectator.key()),
            Some(GameMode::Spectator)
        );
    }

    use super::*;

    #[test]
    fn create_save_load_roundtrip() {
        let dir = std::env::temp_dir().join(format!("minelib-saves-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let w1 = create_world(&dir, "Mon monde !", 42).unwrap();
        let w2 = create_world(&dir, "Mon monde !", 7).unwrap();
        assert_ne!(w1, w2);
        assert_eq!(list_worlds(&dir).len(), 2);

        let (mut data, mut world) = load_world(&w1).unwrap();
        assert_eq!(data.meta.seed, 42);
        assert_eq!(data.meta.name, "Mon monde !");
        world.ensure_chunk(ChunkPos { x: 0, z: 0 });
        world.set(3, 150, 4, BlockId::PLANKS);
        world.place_torch(5, 150, 5, Some(2));
        world.place_torch(6, 150, 5, Some(1));
        world.set_torch_infinite([6, 150, 5]);
        world.clock = 99.5;
        data.clock = world.clock;
        data.player = Some(PlayerState {
            pos: [1.5, 80.0, -3.0],
            yaw: 1.0,
            pitch: -0.2,
            flying: true,
        });
        let mut inv = Inventory::starter();
        inv.selected = 4;
        data.inventory = Some(inv);
        save_world(&w1, &data, &world).unwrap();

        let (d2, mut w) = load_world(&w1).unwrap();
        w.ensure_chunk(ChunkPos { x: 0, z: 0 });
        assert_eq!(w.get(3, 150, 4), BlockId::PLANKS);
        assert_eq!(w.get(5, 150, 5), BlockId::TORCH);
        assert_eq!(w.torch([5, 150, 5]).unwrap().chemical, Some(2));
        assert!(!w.torch([5, 150, 5]).unwrap().infinite);
        assert!(w.torch([6, 150, 5]).unwrap().infinite);
        assert_eq!(d2.clock, 99.5);
        assert!(d2.player.unwrap().flying);
        let inv = d2.inventory.unwrap();
        assert_eq!(inv.selected, 4);
        assert_eq!(inv.slots, Inventory::starter().slots);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn minecraft_world_is_imported_then_saved_our_way() {
        use crate::mcimport::tests::{compound, modern_chunk, region, root};
        use crate::mcimport::Nbt;
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("minelib-mc-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let w = dir.join("MonMondeMC");
        fs::create_dir_all(w.join("region")).unwrap();
        fs::write(
            w.join("region/r.0.0.mca"),
            region(&[modern_chunk(0, 0), modern_chunk(1, 0)]),
        )
        .unwrap();
        let level = compound(vec![(
            "Data",
            compound(vec![
                ("LevelName", Nbt::String("Château".into())),
                ("DataVersion", Nbt::Int(3700)),
                ("LastPlayed", Nbt::Long(1_700_000_000_000)),
                ("WorldGenSettings", compound(vec![("seed", Nbt::Long(-42))])),
                (
                    "Player",
                    compound(vec![
                        (
                            "Pos",
                            Nbt::List(vec![Nbt::Double(5.5), Nbt::Double(65.0), Nbt::Double(6.5)]),
                        ),
                        (
                            "Rotation",
                            Nbt::List(vec![Nbt::Float(90.0), Nbt::Float(10.0)]),
                        ),
                    ]),
                ),
            ]),
        )]);
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(&root(&level)).unwrap();
        fs::write(w.join("level.dat"), gz.finish().unwrap()).unwrap();

        let list = list_worlds(&dir);
        assert_eq!(list.len(), 1);
        assert!(list[0].minecraft);
        assert_eq!(list[0].meta.name, "Château");

        let (data, mut world) = load_world(&w).unwrap();
        assert_eq!(data.meta.seed, (-42i64) as u32);
        let p = data.player.unwrap();
        assert_eq!(p.pos, [5.5, 129.0, 6.5]);
        world.ensure_chunk(ChunkPos { x: 1, z: 0 });
        assert_eq!(world.get(20, 128, 3), BlockId::GRASS);
        assert_eq!(world.get(19, 129, 4), BlockId::TORCH);
        assert!(world.torch([19, 129, 4]).is_some());
        // Hors de la zone importée : vide (le monde n'est pas « superflat »).
        assert_eq!(world.generator, Generator::Void);
        world.ensure_chunk(ChunkPos { x: 9, z: 9 });
        assert_eq!(world.get(150, 0, 150), BlockId::AIR);
        world.set(20, 140, 3, BlockId::BRICK);
        save_world(&w, &data, &world).unwrap();

        // Rechargé au format Minelib (level.txt), fichiers Minecraft intacts.
        assert!(
            w.join("level.txt").exists()
                && w.join("chunks.bin").exists()
                && w.join("level.dat").exists()
        );
        let list = list_worlds(&dir);
        assert!(!list[0].minecraft);
        let (_, mut again) = load_world(&w).unwrap();
        again.ensure_chunk(ChunkPos { x: 1, z: 0 });
        assert_eq!(again.get(20, 128, 3), BlockId::GRASS);
        assert_eq!(again.get(20, 140, 3), BlockId::BRICK);
        assert_eq!(again.base_count(), 2);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn dates_and_seeds() {
        assert_eq!(format_date(0), "01/01/1970 00:00");
        assert_eq!(format_date(1_794_130_000), "08/11/2026 09:26");
        assert_eq!(parse_seed("12345"), 12345);
        assert_eq!(parse_seed("bonjour"), parse_seed("bonjour"));
        assert_ne!(parse_seed("bonjour"), parse_seed("salut"));
    }
}
