//! Cartes FPS : réglages (`fps.txt` dans le dossier du monde : zones de départ CT / T et
//! sites de bombe A / B / C, posés avec `/fps set …` après une sélection à la baguette),
//! paquet partageable et signature.
//!
//! La signature est l'empreinte du paquet (terrain, modifications, torches, réglages FPS et,
//! depuis la v224, lumières de l'éditeur) : changer un départ ou un site, c'est une autre
//! carte ; deux copies identiques (même sous deux noms) sont la même carte et n'apparaissent
//! qu'une fois dans la liste.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result};
use minelib_net::game::{self, FpsMap, MapPackage, Zone};
use minelib_world::gen::Generator;
use minelib_world::lap::wrap_block;
use minelib_world::ChunkPos;

const FILE: &str = "fps.txt";

fn zone_text(z: &Zone) -> String {
    format!(
        "{},{},{},{},{},{}",
        z.min[0], z.min[1], z.min[2], z.max[0], z.max[1], z.max[2]
    )
}

fn parse_zone(v: &str) -> Option<Zone> {
    let n: Vec<i32> = v.split(',').filter_map(|x| x.trim().parse().ok()).collect();
    (n.len() == 6).then(|| Zone::new([n[0], n[1], n[2]], [n[3], n[4], n[5]]))
}

/// (v322) Les zones d'une carte : départs, sites, portes.
fn zones(m: &FpsMap) -> impl Iterator<Item = &Zone> {
    [&m.ct, &m.t]
        .into_iter()
        .chain(m.sites.iter())
        .flatten()
        .chain(m.door_list.iter())
}

/// (v322) Origine des parties FPS sur la carte `m` (réglages en places absolues, comme dans
/// `fps.txt` et dans le paquet de la carte) : le multiple de 4 096 blocs le plus proche du
/// milieu de ses zones (`session::origin_near` : le centre du monde à moins de 32 768 blocs de
/// lui, comme toutes les cartes d'avant la v322). La même chez tous ceux qui ont la carte :
/// l'hôte, ses invités, le garant, le vérificateur, l'arbitre ; une partie FPS y compte tout
/// depuis elle (places des joueurs, balles, grenades, bots : en simple précision, exactes près
/// d'elle). Aucune : la carte n'a pas de zones.
pub fn origin(m: &FpsMap) -> Option<ChunkPos> {
    // Le milieu sur la carte bouclée : les écarts au premier coin, les plus courts.
    let mut first: Option<[i64; 2]> = None;
    let (mut sum, mut n) = ([0i64; 2], 0i64);
    for z in zones(m) {
        let mid = [
            (z.min[0] as i64 + z.max[0] as i64) / 2,
            (z.min[2] as i64 + z.max[2] as i64) / 2,
        ];
        let f = *first.get_or_insert(mid);
        for k in 0..2 {
            sum[k] += wrap_block(mid[k] - f[k]) as i64;
        }
        n += 1;
    }
    let f = first?;
    let at = |k: usize| wrap_block(f[k] + sum[k] / n) as f64 + 0.5;
    Some(crate::session::origin_near([at(0), 0.0, at(1)]))
}

/// (v322) Origine d'une partie FPS en ligne sur la carte `m` : celle de ses zones, sinon le
/// centre du monde (sans zones, on y joue autour du point de départ (8, 8) du monde).
pub fn match_origin(m: &FpsMap) -> ChunkPos {
    origin(m).unwrap_or(ChunkPos { x: 0, z: 0 })
}

/// (v322) La zone `z` déplacée de `by` blocs (sur x et z) : son coin le plus bas ramené sur la
/// carte bouclée, sa taille gardée (elle reste d'un seul tenant).
fn shift_zone(z: &Zone, by: [i64; 3]) -> Zone {
    let min = [
        wrap_block(z.min[0] as i64 + by[0]),
        z.min[1],
        wrap_block(z.min[2] as i64 + by[2]),
    ];
    Zone {
        min,
        max: [
            min[0] + (z.max[0] - z.min[0]),
            z.max[1],
            min[2] + (z.max[2] - z.min[2]),
        ],
    }
}

fn shift_map(m: &FpsMap, by: [i64; 3]) -> FpsMap {
    let z = |z: &Option<Zone>| z.as_ref().map(|z| shift_zone(z, by));
    FpsMap {
        ct: z(&m.ct),
        t: z(&m.t),
        sites: [z(&m.sites[0]), z(&m.sites[1]), z(&m.sites[2])],
        max_players: m.max_players,
        door_list: m.door_list.iter().map(|d| shift_zone(d, by)).collect(),
    }
}

/// (v322) Les réglages `m` (places absolues) comptés depuis l'origine `o` d'une partie (en
/// blocs, `World::origin_blocks`) : ce que la partie, ses bots et son radar prennent.
pub fn local(m: &FpsMap, o: [i32; 3]) -> FpsMap {
    shift_map(m, [-(o[0] as i64), 0, -(o[2] as i64)])
}

/// (v322) Les réglages `m` comptés depuis l'origine `o` (en blocs), en places absolues : ce
/// que garde `fps.txt`.
pub fn absolute(m: &FpsMap, o: [i32; 3]) -> FpsMap {
    shift_map(m, [o[0] as i64, 0, o[2] as i64])
}

/// (v322) Une zone comptée depuis `o` est-elle à cheval sur la limite du monde (une partie de
/// chaque côté) ? Une zone de la carte doit être d'un seul côté (`fps.txt` la garde par ses
/// deux coins absolus).
pub fn straddles(z: &Zone, o: [i32; 3]) -> bool {
    let a = shift_zone(z, [o[0] as i64, 0, o[2] as i64]);
    (a.max[0] as i64) >= minelib_world::lap::HALF || (a.max[2] as i64) >= minelib_world::lap::HALF
}

/// (v322) Réglages d'une carte comptés depuis l'origine de ses parties en ligne, et cette
/// origine : ce que prennent le garant, le vérificateur, l'arbitre et le relevé de la carte.
pub fn load_for_match(dir: &Path) -> (FpsMap, ChunkPos) {
    let m = load(dir);
    let o = match_origin(&m);
    let (x, z) = o.origin();
    (local(&m, [x, 0, z]), o)
}

/// Réglages FPS d'un monde (vides si absents).
pub fn load(dir: &Path) -> FpsMap {
    let mut m = FpsMap::default();
    let Ok(text) = fs::read_to_string(dir.join(FILE)) else {
        return m;
    };
    // (v209) Doubles portes réglées sans dire si les portes s'ouvrent : elles s'ouvrent
    // (on ne règle pas des doubles portes pour les garder fermées). Inferno n'avait que
    // `portes_doubles=1` : ses portes restaient fixes, la touche Utiliser n'y faisait rien.
    // CHANGEMENT DE CARTE : ces cartes changent de signature (`MapPackage::fps`).
    let mut doors_said = false;
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        if k.trim() == "joueurs" {
            m.set_players(v.trim().parse().unwrap_or(0));
            continue;
        }
        if k.trim() == "portes_doubles" {
            m.set_door_sync(v.trim() == "1");
            continue;
        }
        if k.trim() == "portes" {
            m.set_doors(v.trim() == "1");
            doors_said = true;
            continue;
        }
        // (v258) Une porte (ou double porte) posée pour la partie.
        if k.trim() == "porte" {
            if let Some(z) = parse_zone(v) {
                if !m.door_list.contains(&z) {
                    m.door_list.push(z);
                }
            }
            continue;
        }
        let z = parse_zone(v);
        match k.trim() {
            "ct" => m.ct = z,
            "t" => m.t = z,
            "a" => m.sites[0] = z,
            "b" => m.sites[1] = z,
            "c" => m.sites[2] = z,
            _ => {}
        }
    }
    if m.door_sync() && !doors_said {
        m.set_doors(true);
    }
    // (v258) Toujours dans le même ordre (le paquet de la carte range ses lignes : chez qui
    // la reçoit, elles arrivent triées), au plus `MAX_DOORS`.
    m.door_list.sort_by_key(|z| (z.min, z.max));
    m.door_list.truncate(MAX_DOORS);
    m
}

pub fn save(dir: &Path, m: &FpsMap) -> Result<()> {
    let mut s = String::new();
    let mut put = |k: &str, z: &Option<Zone>| {
        if let Some(z) = z {
            s += &format!("{k}={}\n", zone_text(z));
        }
    };
    put("ct", &m.ct);
    put("t", &m.t);
    put("a", &m.sites[0]);
    put("b", &m.sites[1]);
    put("c", &m.sites[2]);
    let n = m.chosen_players();
    if n != 0 && n != 10 {
        s += &format!("joueurs={n}\n");
    }
    if m.doors() {
        s += "portes=1\n";
    } else if m.door_sync() {
        // (v209) Fixes malgré les doubles portes : à écrire (sinon elles s'ouvriraient).
        s += "portes=0\n";
    }
    if m.door_sync() {
        s += "portes_doubles=1\n";
    }
    for z in &m.door_list {
        s += &format!("porte={}\n", zone_text(z));
    }
    fs::write(dir.join(FILE), s)?;
    Ok(())
}

pub const SITE_NAMES: [&str; 3] = ["A", "B", "C"];

/// Heure fixe de la carte (`/fps set time`), dans son dossier ; absente : le soleil de
/// Dust2 (`/fps set time default`).
const TIME_FILE: &str = "heure.txt";

pub fn load_time(dir: &Path) -> Option<f32> {
    fs::read_to_string(dir.join(TIME_FILE))
        .ok()?
        .trim()
        .parse::<f32>()
        .ok()
        .filter(|h| (0.0..24.0).contains(h))
}

pub fn save_time(dir: &Path, time: Option<f32>) -> Result<()> {
    match time {
        Some(h) => fs::write(dir.join(TIME_FILE), format!("{h}\n"))?,
        None => {
            let _ = fs::remove_file(dir.join(TIME_FILE));
        }
    }
    Ok(())
}

/// Paquet partageable d'un monde enregistré.
pub fn package(dir: &Path) -> Result<MapPackage> {
    let level = fs::read_to_string(dir.join("level.txt"))
        .with_context(|| format!("{}/level.txt", dir.display()))?;
    let kv: HashMap<&str, &str> = level
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim(), v.trim()))
        .collect();
    let seed = kv.get("seed").and_then(|v| v.parse().ok()).unwrap_or(0);
    let generator = kv
        .get("generator")
        .copied()
        .unwrap_or("procedural")
        .to_string();
    // (v330) Un monde créé depuis la v330 : la carte porte sa clé de terrain (pas sa graine)
    // avec son générateur, « procedural2:clé ». Un serveur de construction ne la met pas dans
    // la carte qu'il envoie (`without_seed`) : le relais la donne.
    let generator = if Generator::parse(&generator) == Generator::Procedural2 {
        let key = kv
            .get("cle")
            .and_then(|v| u64::from_str_radix(v, 16).ok())
            .filter(|k| *k != 0)
            .or_else(|| {
                kv.get("graine")
                    .and_then(|v| v.parse::<u64>().ok())
                    .map(minelib_net::graine::terrain_key)
            });
        match key {
            Some(k) => format!("procedural2:{k:016x}"),
            None => "procedural2".to_string(),
        }
    } else {
        generator
    };
    // Modifications : enregistrements de 14 octets, triés.
    let mut records: Vec<[u8; 14]> = Vec::new();
    if let Ok(bin) = fs::read(dir.join("edits.bin")) {
        let n = bin
            .get(..4)
            .map_or(0, |b| u32::from_le_bytes(b.try_into().unwrap()) as usize);
        for k in 0..n {
            let o = 4 + k * 14;
            let Some(e) = bin.get(o..o + 14) else { break };
            records.push(e.try_into().unwrap());
        }
    }
    records.sort_unstable();
    records.dedup();
    let edits: Vec<u8> = records.concat();
    let mut chunks = Vec::new();
    if let Ok(bin) = fs::read(dir.join("chunks.bin")) {
        if bin.get(..4) == Some(b"MLCH") {
            let mut o = 8;
            while let Some(h) = bin.get(o..o + 12) {
                let x = i32::from_le_bytes(h[0..4].try_into().unwrap());
                let z = i32::from_le_bytes(h[4..8].try_into().unwrap());
                let n = u32::from_le_bytes(h[8..12].try_into().unwrap()) as usize;
                let Some(data) = bin.get(o + 12..o + 12 + n) else {
                    break;
                };
                chunks.push((x, z, data.to_vec()));
                o += 12 + n;
            }
        }
    }
    chunks.sort_by_key(|c| (c.0, c.1));
    // Torches : allumées à neuf (l'heure d'allumage dépend de la partie jouée).
    let mut torches: Vec<String> = fs::read_to_string(dir.join("torches.txt"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let v: Vec<&str> = l.split(',').collect();
            if !(5..=6).contains(&v.len()) {
                return None;
            }
            let inf = if v.len() == 6 { ",inf" } else { "" };
            Some(format!("{},{},{},{},0{inf}", v[0], v[1], v[2], v[3]))
        })
        .collect();
    // Heure fixe de la carte : avec les torches, « heure,… ».
    if let Some(h) = load_time(dir) {
        torches.push(format!("heure,{h}"));
    }
    // Têtes à skin (carte importée) : avec les torches, préfixées « tete, ».
    torches.extend(
        fs::read_to_string(dir.join(crate::mcimport::HEADS_FILE))
            .unwrap_or_default()
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| format!("tete,{}", l.trim())),
    );
    // CHANGEMENT DE CARTE (v224) : les lumières de l'éditeur (projecteurs, lumières
    // ponctuelles) font partie de la carte, avec les torches, préfixées « lumiere, » (leur
    // texte, identifiant compris : le même chez tous ceux qui ont la carte). Une carte qui a
    // des lumières change donc de signature ; une carte sans lumières garde la sienne.
    torches.extend(
        crate::lights::load_scene(dir)
            .iter()
            .map(|l| format!("{LIGHT_PREFIX}{}", l.to_text())),
    );
    // CHANGEMENT DE CARTE (v258) : les portes posées pour la partie (`/fps set door`), avec
    // les torches, préfixées « porte, » (hors de l'encodage de `FpsMap` : les cartes déjà
    // partagées se lisent toujours). Une carte qui en a change donc de signature.
    let fps = load(dir);
    torches.extend(
        fps.door_list
            .iter()
            .map(|z| format!("{DOOR_PREFIX}{}", zone_text(z))),
    );
    // CHANGEMENT DE CARTE (v283) : les piles de tapis de plusieurs couleurs (`tapis.txt`),
    // avec les torches, préfixées « tapis, » : ceux qui reçoivent la carte (serveur de
    // construction, partie FPS) voient chaque couche de sa couleur. Une carte qui en a change
    // de signature ; une carte sans en garde la sienne.
    torches.extend(
        fs::read_to_string(dir.join(crate::save::CARPETS_FILE))
            .unwrap_or_default()
            .lines()
            .filter_map(crate::save::parse_carpet)
            .map(|(p, l)| format!("{CARPET_PREFIX}{}", crate::save::carpet_line(p, &l))),
    );
    torches.sort();
    torches.dedup();
    Ok(MapPackage {
        seed,
        generator,
        edits,
        chunks,
        torches,
        fps,
    })
}

/// (v330) La carte d'un serveur de construction, telle qu'elle part au relais : sans la graine
/// ni la clé de terrain (le relais les garde, et ne donne la graine qu'aux OP du serveur) ;
/// et ce qu'elle en avait : (graine, clé de terrain), 0 : pas connue. D'un monde d'avant la
/// v330, la graine fait le terrain : c'est aussi sa clé.
pub fn without_seed(mut p: MapPackage, dir: &Path) -> (MapPackage, u64, u64) {
    let generator = Generator::parse(&p.generator);
    let (seed, terrain) = if generator == Generator::Procedural2 {
        let key = p
            .generator
            .split_once(':')
            .and_then(|(_, k)| u64::from_str_radix(k.trim(), 16).ok())
            .unwrap_or(0);
        let graine = fs::read_to_string(dir.join("level.txt"))
            .ok()
            .and_then(|l| {
                l.lines()
                    .filter_map(|l| l.split_once('='))
                    .find(|(k, _)| k.trim() == "graine")
                    .and_then(|(_, v)| v.trim().parse::<u64>().ok())
            })
            .unwrap_or(0);
        (graine, key)
    } else {
        (u64::from(p.seed), u64::from(p.seed))
    };
    p.seed = 0;
    p.generator = generator.name().to_string();
    (p, seed, terrain)
}

/// (v224) Préfixe des lumières de l'éditeur parmi les torches d'un paquet.
const LIGHT_PREFIX: &str = "lumiere,";

/// (v258) Préfixe des portes posées pour la partie parmi les torches d'un paquet.
const DOOR_PREFIX: &str = "porte,";

/// (v283) Préfixe des piles de tapis de plusieurs couleurs parmi les torches d'un paquet.
const CARPET_PREFIX: &str = "tapis,";

/// Signatures déjà calculées : (dossier) → (date des fichiers, signature).
#[derive(Default)]
pub struct SigCache {
    cache: HashMap<PathBuf, (u64, [u8; 32])>,
}

pub(crate) fn stamp(dir: &Path) -> u64 {
    [
        "level.txt",
        "edits.bin",
        "chunks.bin",
        "torches.txt",
        crate::mcimport::HEADS_FILE,
        TIME_FILE,
        FILE,
        crate::lights::SCENE_FILE,
        crate::save::CARPETS_FILE,
    ]
    .iter()
    .filter_map(|f| fs::metadata(dir.join(f)).ok())
    .map(|m| {
        let t = m
            .modified()
            .ok()
            .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_millis() as u64);
        t ^ m.len().rotate_left(17)
    })
    .fold(0u64, |a, b| a.rotate_left(7) ^ b)
}

impl SigCache {
    pub fn signature(&mut self, dir: &Path) -> Option<[u8; 32]> {
        let st = stamp(dir);
        if let Some((s, sig)) = self.cache.get(dir) {
            if *s == st {
                return Some(*sig);
            }
        }
        let sig = package(dir).ok()?.signature();
        // Carte signée : son relevé (où un joueur peut aller depuis les départs, et les
        // murs qui le bornent) est refait en tâche de fond pour la minimap.
        crate::radar::survey_in_background(dir);
        self.cache.insert(dir.to_path_buf(), (st, sig));
        Some(sig)
    }
}

/// Dossier d'une carte téléchargée.
pub fn downloaded_dir(maps: &Path, sig: &[u8; 32]) -> PathBuf {
    maps.join(minelib_net::crypto::hex(&sig[..12]))
}

/// Installe une carte téléchargée (dossier de monde ordinaire, en mode FPS).
pub fn unpack(maps: &Path, name: &str, bytes: &[u8]) -> Result<PathBuf> {
    let p: MapPackage = game::decode(bytes).context("carte illisible")?;
    let sig = game::signature(bytes);
    let dir = downloaded_dir(maps, &sig);
    fs::create_dir_all(&dir)?;
    let name: String = name
        .chars()
        .filter(|c| !c.is_control() && *c != '=')
        .take(40)
        .collect();
    // La carte vient d'un autre joueur : rien de ce qu'elle contient ne peut ajouter de
    // lignes au fichier du monde. (v330) « procedural2:clé » : la clé de terrain, à part.
    let (gen_text, key_text) = p.generator.split_once(':').unwrap_or((&p.generator, ""));
    let generator: String = gen_text
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .take(24)
        .collect();
    let key = u64::from_str_radix(key_text.trim(), 16)
        .ok()
        .filter(|k| *k != 0 && Generator::parse(&generator) == Generator::Procedural2)
        .map_or(String::new(), |k| format!("cle={k:016x}\n"));
    fs::write(
        dir.join("level.txt"),
        format!(
            "name={name}\nseed={}\ncreated=0\nlast_played=0\nclock=0\nnight=0\ngenerator={}\n\
             {key}gamemode=fps\nfly=0\nhealth=20\n",
            p.seed, generator
        ),
    )?;
    let mut edits = ((p.edits.len() / 14) as u32).to_le_bytes().to_vec();
    edits.extend_from_slice(&p.edits);
    fs::write(dir.join("edits.bin"), edits)?;
    if !p.chunks.is_empty() {
        let mut bin = b"MLCH".to_vec();
        bin.extend_from_slice(&(p.chunks.len() as u32).to_le_bytes());
        for (x, z, data) in &p.chunks {
            bin.extend_from_slice(&x.to_le_bytes());
            bin.extend_from_slice(&z.to_le_bytes());
            bin.extend_from_slice(&(data.len() as u32).to_le_bytes());
            bin.extend_from_slice(data);
        }
        fs::write(dir.join("chunks.bin"), bin)?;
    }
    let time = p
        .torches
        .iter()
        .find_map(|l| l.strip_prefix("heure,")?.parse::<f32>().ok())
        .filter(|h| h.is_finite() && (0.0..24.0).contains(h));
    save_time(&dir, time)?;
    // (v224) Lumières de l'éditeur de la carte (ramenées dans leurs bornes : elle vient d'un
    // autre joueur).
    let lights: Vec<crate::lights::PlacedLight> = p
        .torches
        .iter()
        .filter_map(|l| l.strip_prefix(LIGHT_PREFIX))
        .filter_map(|t| crate::lights::PlacedLight::from_text(t.as_bytes()))
        .filter(|l| l.uid != 0)
        .take(crate::lights::MAX_PLACED)
        .collect();
    crate::lights::save_scene(&dir, &lights)?;
    let (heads, torches): (Vec<&String>, Vec<&String>) = p
        .torches
        .iter()
        .filter(|l| {
            !l.starts_with("heure,")
                && !l.starts_with(LIGHT_PREFIX)
                && !l.starts_with(DOOR_PREFIX)
                && !l.starts_with(CARPET_PREFIX)
        })
        .partition(|l| l.starts_with("tete,"));
    let mut t = torches
        .iter()
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    if !t.is_empty() {
        t.push('\n');
    }
    fs::write(dir.join("torches.txt"), t)?;
    let heads: Vec<String> = heads
        .iter()
        .map(|l| l["tete,".len()..].to_string())
        .collect();
    crate::mcimport::write_heads(&dir, &heads);
    // (v283) Les piles de tapis de plusieurs couleurs.
    let carpets: Vec<String> = p
        .torches
        .iter()
        .filter_map(|l| l.strip_prefix(CARPET_PREFIX))
        .filter_map(crate::save::parse_carpet)
        .map(|(pos, l)| crate::save::carpet_line(pos, &l))
        .collect();
    if carpets.is_empty() {
        let _ = fs::remove_file(dir.join(crate::save::CARPETS_FILE));
    } else {
        fs::write(dir.join(crate::save::CARPETS_FILE), carpets.join("\n") + "\n")?;
    }
    // (v258) Les portes posées pour la partie : dans les lignes du paquet.
    let mut fps = p.fps.clone();
    fps.door_list = p
        .torches
        .iter()
        .filter_map(|l| parse_zone(l.strip_prefix(DOOR_PREFIX)?))
        .take(MAX_DOORS)
        .collect();
    save(&dir, &fps)?;
    Ok(dir)
}

/// (v258) Portes posées pour la partie, au plus.
pub const MAX_DOORS: usize = 512;

/// (v232) Fichier d'une copie de carte dans ses mondes : la signature de la carte d'origine
/// (en hexadécimal), pour ne pas la copier deux fois.
const SOURCE_FILE: &str = "carte_source.txt";

/// (v232) Copie d'une carte FPS (téléchargée) dans ses mondes solo, sous ce nom : un monde
/// ordinaire en mode FPS, qui garde la carte d'origine (`solo_copy`).
pub fn copy_to_saves(src: &Path, saves: &Path, name: &str, sig: &[u8; 32]) -> Result<PathBuf> {
    let level = fs::read_to_string(src.join("level.txt"))
        .with_context(|| format!("{}/level.txt", src.display()))?;
    let dir = crate::save::create_world(saves, name, 0)?;
    copy_dir(src, &dir)?;
    // Son nom à lui, et la date d'aujourd'hui (le reste comme la carte : graine, mode FPS…).
    let now = crate::save::now();
    let name: String = name
        .chars()
        .filter(|c| !c.is_control() && *c != '=')
        .take(40)
        .collect();
    let mut out = String::new();
    for l in level.lines() {
        let key = l.split_once('=').map_or("", |(k, _)| k.trim());
        match key {
            "name" => out += &format!("name={name}\n"),
            "created" | "last_played" => out += &format!("{key}={now}\n"),
            _ => {
                out += l;
                out.push('\n');
            }
        }
    }
    fs::write(dir.join("level.txt"), out)?;
    fs::write(dir.join(SOURCE_FILE), minelib_net::crypto::hex(sig))?;
    Ok(dir)
}

/// (v232) Monde de ses mondes solo copié de cette carte (`copy_to_saves`), s'il y en a un.
pub fn solo_copy(saves: &Path, sig: &[u8; 32]) -> Option<PathBuf> {
    let want = minelib_net::crypto::hex(sig);
    crate::save::list_worlds(saves)
        .into_iter()
        .map(|w| w.dir)
        .find(|d| fs::read_to_string(d.join(SOURCE_FILE)).is_ok_and(|t| t.trim() == want))
}

/// (v232) Cartes déjà copiées dans ses mondes solo (leurs signatures d'origine).
pub fn solo_copies(saves: &Path) -> Vec<[u8; 32]> {
    crate::save::list_worlds(saves)
        .into_iter()
        .filter_map(|w| fs::read_to_string(w.dir.join(SOURCE_FILE)).ok())
        .filter_map(|t| minelib_net::crypto::unhex(t.trim())?.try_into().ok())
        .collect()
}

/// Copie récursive d'un dossier (fichiers remplacés).
pub(crate) fn copy_dir(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst)?;
    for e in fs::read_dir(src)?.flatten() {
        let to = dst.join(e.file_name());
        let ty = e.file_type()?;
        if ty.is_dir() {
            copy_dir(&e.path(), &to)?;
        } else if ty.is_file() {
            fs::copy(e.path(), &to)
                .with_context(|| format!("copie de {}", e.path().display()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// (v232) Une carte téléchargée, copiée dans ses mondes : même carte (signature), son nom,
    /// en mode FPS, retrouvée ensuite (pas copiée deux fois).
    /// Tous les fichiers d'un dossier, mis bout à bout (pour y chercher une graine).
    fn all_bytes(dir: &Path) -> Vec<u8> {
        let mut out = Vec::new();
        for e in fs::read_dir(dir).unwrap().flatten() {
            if e.path().is_dir() {
                out.extend(all_bytes(&e.path()));
            } else {
                out.extend(fs::read(e.path()).unwrap());
            }
        }
        out
    }

    fn contains(hay: &[u8], needle: &str) -> bool {
        hay.windows(needle.len()).any(|w| w == needle.as_bytes())
    }

    /// (v330) La carte d'un serveur de construction part sans la graine ni la clé de terrain
    /// (le relais les garde) : la copie de qui le rejoint n'en a rien, ni en clair ni en
    /// hexadécimal ; avec la clé que donne le relais, son terrain est celui du monde. Un monde
    /// d'avant la v330 : sa graine fait le terrain, c'est aussi la clé. Une carte FPS, elle,
    /// porte sa clé de terrain (pas sa graine).
    #[test]
    fn a_server_map_carries_no_seed() {
        use minelib_world::gen::Terrain;
        let root = std::env::temp_dir().join(format!("minelib-sans-graine-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let graine = 9_876_543_210_123_456_789u64;
        let dir = crate::save::create_world(&root.join("src"), "Secret", graine).unwrap();
        let key = minelib_net::graine::terrain_key(graine);
        // Carte FPS : la clé, pas la graine.
        let fps = package(&dir).unwrap();
        assert_eq!(fps.generator, format!("procedural2:{key:016x}"));
        let fps_copy = unpack(&root.join("maps"), "Secret", &fps.to_bytes()).unwrap();
        let (meta, world) = crate::save::load_world(&fps_copy).map(|(d, w)| (d.meta, w)).unwrap();
        assert_eq!((meta.graine, meta.key), (None, Some(key)));
        assert_eq!(world.terrain, Terrain::v2(key));
        assert!(!contains(&all_bytes(&fps_copy), &graine.to_string()));
        // Serveur de construction : ni l'une ni l'autre.
        let (p, seed, terrain) = without_seed(package(&dir).unwrap(), &dir);
        assert_eq!((seed, terrain), (graine, key));
        assert_eq!((p.seed, p.generator.as_str()), (0, "procedural2"));
        let copy = unpack(&root.join("maps"), "Secret", &p.to_bytes()).unwrap();
        let bytes = all_bytes(&copy);
        for secret in [graine.to_string(), format!("{key:016x}"), key.to_string()] {
            assert!(!contains(&bytes, &secret), "{secret} dans la copie");
        }
        // La copie se reconnaît (même signature que la carte envoyée), sans clé.
        assert_eq!(package(&copy).unwrap().signature(), p.signature());
        let (meta, mut guest) = crate::save::load_world(&copy).map(|(d, w)| (d.meta, w)).unwrap();
        assert_eq!((meta.seed, meta.graine, meta.key), (0, None, None));
        // Avec la clé du relais : le terrain et la météo du monde du créateur.
        let (_, mut own) = crate::save::load_world(&dir).unwrap();
        crate::session::give_terrain(&mut guest, terrain);
        assert_eq!((guest.terrain, guest.seed), (own.terrain, own.seed));
        let at = ChunkPos { x: 3, z: -7 };
        own.ensure_chunk(at);
        guest.ensure_chunk(at);
        for y in (0..300).step_by(3) {
            assert_eq!(own.get(53, y, -110), guest.get(53, y, -110), "y {y}");
        }
        // Monde d'avant la v330 (graine de 32 bits, écrite à la main).
        let old = root.join("src").join("ancien");
        fs::create_dir_all(&old).unwrap();
        fs::write(old.join("level.txt"), "name=Ancien\nseed=3141592653\ngenerator=procedural\n").unwrap();
        let (p, seed, terrain) = without_seed(package(&old).unwrap(), &old);
        assert_eq!((seed, terrain, p.seed), (3_141_592_653, 3_141_592_653, 0));
        let copy = unpack(&root.join("maps"), "Ancien", &p.to_bytes()).unwrap();
        assert!(!contains(&all_bytes(&copy), "3141592653"));
        let (_, mut guest) = crate::save::load_world(&copy).unwrap();
        crate::session::give_terrain(&mut guest, terrain);
        assert_eq!(guest.terrain, minelib_world::gen::Terrain::v1(3_141_592_653));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_downloaded_map_is_copied_once_into_the_solo_worlds() {
        let root = std::env::temp_dir().join(format!("minelib-copie-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let dir = crate::save::create_world(&root.join("src"), "Mirage", 9).unwrap();
        let mut m = load(&dir);
        m.ct = Some(Zone::new([0, 64, 0], [5, 64, 5]));
        m.t = Some(Zone::new([40, 64, 0], [45, 64, 5]));
        save(&dir, &m).unwrap();
        let p = package(&dir).unwrap();
        let sig = p.signature();
        let got = unpack(&root.join("maps"), "Mirage", &p.to_bytes()).unwrap();
        let saves = root.join("saves");
        assert_eq!(solo_copy(&saves, &sig), None);
        let copy = copy_to_saves(&got, &saves, "Mirage", &sig).unwrap();
        assert_eq!(package(&copy).unwrap().signature(), sig, "la même carte");
        assert_eq!(load(&copy), m);
        let w = crate::save::list_worlds(&saves);
        assert_eq!(w.len(), 1);
        assert_eq!(w[0].meta.name, "Mirage");
        assert!(w[0].meta.created > 0);
        let level = fs::read_to_string(copy.join("level.txt")).unwrap();
        assert!(level.contains("gamemode=fps"));
        assert_eq!(solo_copy(&saves, &sig), Some(copy));
        assert_eq!(solo_copies(&saves), vec![sig]);
        let _ = fs::remove_dir_all(&root);
    }

    /// (v322) Une carte FPS construite loin du centre du monde : ses parties comptent depuis une
    /// origine proche de ses zones (`origin`), où elles sont petites (exactes en simple
    /// précision) et d'où elles reviennent en places absolues à l'identique ; près du centre
    /// (toutes les cartes d'avant), rien ne change ; le long de la limite du monde, le milieu
    /// des zones est pris de leur côté (pas au centre), et une zone à cheval sur elle n'est pas
    /// gardée.
    #[test]
    fn far_maps_count_from_an_origin_close_to_their_zones() {
        let zero = ChunkPos { x: 0, z: 0 };
        let near = FpsMap {
            ct: Some(Zone::new([0, 64, 0], [5, 64, 5])),
            t: Some(Zone::new([40, 64, 0], [45, 64, 5])),
            ..Default::default()
        };
        assert_eq!(origin(&near), Some(zero));
        assert_eq!(local(&near, [0, 0, 0]), near);
        assert_eq!(origin(&FpsMap::default()), None);
        assert_eq!(match_origin(&FpsMap::default()), zero);

        let x = 1_000_000_000;
        let far = FpsMap {
            ct: Some(Zone::new([x, 64, 5000], [x + 5, 64, 5005])),
            t: Some(Zone::new([x + 40, 64, 5000], [x + 45, 64, 5005])),
            sites: [Some(Zone::new([x + 20, 64, 5020], [x + 24, 66, 5024])), None, None],
            door_list: vec![Zone::new([x + 10, 64, 5002], [x + 10, 65, 5003])],
            ..Default::default()
        };
        let o = origin(&far).unwrap();
        assert_ne!(o, zero);
        assert_eq!(match_origin(&far), o);
        let (ox, oz) = o.origin();
        assert!((ox as i64 - x as i64).abs() <= 2048 && oz == 0, "{ox} {oz}");
        let l = local(&far, [ox, 0, oz]);
        for z in [&l.ct, &l.t, &l.sites[0]]
            .into_iter()
            .flatten()
            .chain(l.door_list.iter())
        {
            assert!(z.min[0].abs() < 8192 && z.max[0].abs() < 8192, "{z:?}");
            assert!(z.min[2].abs() < 8192 && z.max[2].abs() < 8192, "{z:?}");
        }
        assert_eq!(l.t.unwrap().min[0] - l.ct.unwrap().min[0], 40);
        assert_eq!(absolute(&l, [ox, 0, oz]), far);

        let h = minelib_world::lap::HALF as i32;
        let seam = FpsMap {
            ct: Some(Zone::new([h - 50, 64, 0], [h - 45, 64, 5])),
            t: Some(Zone::new([-h + 40, 64, 0], [-h + 45, 64, 5])),
            ..Default::default()
        };
        let (sx, sz) = origin(&seam).unwrap().origin();
        assert!((sx as i64).abs() > minelib_world::lap::HALF - 4096, "{sx}");
        let l = local(&seam, [sx, 0, sz]);
        let (ct, t) = (l.ct.unwrap(), l.t.unwrap());
        assert!(ct.min[0].abs() < 4096 && t.min[0].abs() < 4096, "{ct:?} {t:?}");
        assert_eq!(t.min[0] - ct.min[0], 90, "voisines de part et d'autre de la limite");
        assert_eq!(absolute(&l, [sx, 0, sz]), seam);
        let across = Zone::new([ct.min[0] + 40, 64, 0], [ct.min[0] + 60, 64, 5]);
        assert!(straddles(&across, [sx, 0, sz]));
        assert!(!straddles(&ct, [sx, 0, sz]) && !straddles(&t, [sx, 0, sz]));
    }

    /// (v322) Une carte FPS construite loin du centre, reçue ailleurs : ses parties y comptent
    /// depuis la même origine, ses zones aux mêmes places (`load_for_match`, ce que prennent
    /// l'hôte, le garant, le vérificateur et l'arbitre), et le monde relu pour un relevé
    /// (`radar::load_map_world`) a ses blocs aux places de la partie.
    #[test]
    fn a_far_map_counts_from_the_same_origin_wherever_it_is_played() {
        use minelib_world::BlockId;
        let root = std::env::temp_dir().join(format!("minelib-carte-loin-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let dir = crate::save::create_world(&root, "Loin", 11).unwrap();
        let x = 1_000_000_000;
        let mut m = load(&dir);
        m.ct = Some(Zone::new([x, 70, 0], [x + 4, 70, 4]));
        m.t = Some(Zone::new([x + 60, 70, 0], [x + 64, 70, 4]));
        save(&dir, &m).unwrap();
        // Un bloc de la carte, posé depuis l'origine de ses parties.
        let (data, mut world) = crate::save::load_world(&dir).unwrap();
        let (map, o) = load_for_match(&dir);
        world.set_origin(o);
        let (ox, oz) = o.origin();
        let at = [x + 30 - ox, 150, 2 - oz];
        world.ensure_chunk(ChunkPos::containing(at[0], at[2]));
        world.set(at[0], at[1], at[2], BlockId::PLANKS);
        crate::save::save_world(&dir, &data, &world).unwrap();

        let p = package(&dir).unwrap();
        assert_eq!(p.fps.ct, m.ct, "la carte partagée garde ses places absolues");
        let copy = unpack(&root.join("maps"), "Copie", &p.to_bytes()).unwrap();
        let (theirs, o2) = load_for_match(&copy);
        assert_eq!((o2, &theirs), (o, &map));
        assert_eq!(theirs.ct.unwrap().min, [x - ox, 70, -oz]);
        let w = crate::radar::load_map_world(&copy, &theirs, o2).unwrap();
        assert_eq!(w.origin(), o);
        assert_eq!(w.get(at[0], at[1], at[2]), BlockId::PLANKS);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_map_survives_the_trip_and_spawns_make_it_another_map() {
        let root = std::env::temp_dir().join(format!("minelib-carte-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let dir = crate::save::create_world(&root, "Dust", 42).unwrap();
        let a = package(&dir).unwrap();
        let mut m = load(&dir);
        m.ct = Some(Zone::new([0, 64, 0], [5, 64, 5]));
        m.sites[1] = Some(Zone::new([20, 64, 20], [24, 66, 24]));
        save(&dir, &m).unwrap();
        let b = package(&dir).unwrap();
        assert_ne!(
            a.signature(),
            b.signature(),
            "départ CT ajouté : autre carte"
        );
        assert_eq!(load(&dir), m);
        // Téléchargée ailleurs sous un autre nom : même signature.
        let bytes = b.to_bytes();
        let copy = unpack(&root.join("maps"), "Copie", &bytes).unwrap();
        assert_eq!(package(&copy).unwrap().signature(), b.signature());
        let mut cache = SigCache::default();
        assert_eq!(cache.signature(&copy), Some(b.signature()));
        let _ = fs::remove_dir_all(&root);
    }

    /// (v224) Les lumières de l'éditeur partent avec la carte : une carte sans lumières garde
    /// sa signature, une carte avec en change ; reçue ailleurs, elle a les mêmes lumières (même
    /// identifiant, même chez une lumière d'avant la v224, qui n'en avait pas).
    #[test]
    fn the_lights_of_a_map_go_with_it() {
        use crate::lights::{Kind, PlacedLight};
        let root = std::env::temp_dir().join(format!("minelib-carte-lum-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let dir = crate::save::create_world(&root, "Scène", 7).unwrap();
        let bare = package(&dir).unwrap();
        assert!(!bare.torches.iter().any(|t| t.starts_with(LIGHT_PREFIX)));
        // Un fichier d'avant la v224 (sans identifiants).
        let old = r#"{"version":1,"lumieres":[{"nom":"Projecteur 1","sorte":"projecteur",
            "position":[10.5,70.0,-3.25],"couleur":[1.0,0.2,0.4]},{"nom":"Lumière 1"}]}"#;
        fs::write(dir.join(crate::lights::SCENE_FILE), old).unwrap();
        let lit = package(&dir).unwrap();
        assert_ne!(lit.signature(), bare.signature(), "des lumières : une autre carte");
        assert_eq!(
            package(&dir).unwrap().signature(),
            lit.signature(),
            "les identifiants tirés du fichier sont toujours les mêmes"
        );
        let mine = crate::lights::load_scene(&dir);
        assert_eq!(mine.len(), 2);
        assert!(mine.iter().all(|l| l.uid != 0) && mine[0].uid != mine[1].uid);
        let copy = unpack(&root.join("maps"), "Copie", &lit.to_bytes()).unwrap();
        let theirs = crate::lights::load_scene(&copy);
        let key = |v: &[PlacedLight]| {
            let mut k: Vec<(u64, String)> = v.iter().map(|l| (l.uid, l.to_text())).collect();
            k.sort();
            k
        };
        assert_eq!(key(&theirs), key(&mine));
        assert!(theirs.iter().any(|l| l.kind == Kind::Spot && l.position[2] == -3.25));
        assert_eq!(package(&copy).unwrap().signature(), lit.signature());
        // Ni heure ni tête parmi les torches de la copie.
        let t = fs::read_to_string(copy.join("torches.txt")).unwrap_or_default();
        assert!(!t.contains(LIGHT_PREFIX));
        // Enregistrées par le propriétaire (avec leurs identifiants) : même carte.
        crate::lights::save_scene(&dir, &mine).unwrap();
        assert_eq!(package(&dir).unwrap().signature(), lit.signature());
        let _ = fs::remove_dir_all(&root);
    }

    /// (v258) Les portes posées pour la partie partent avec la carte (dans ses lignes, pas dans
    /// l'encodage de `FpsMap`) : une autre signature, les mêmes portes chez celui qui la reçoit,
    /// et elles seules s'ouvrent quand toutes ne le font pas.
    #[test]
    fn doors_set_for_the_match_go_with_the_map() {
        let root = std::env::temp_dir().join(format!("minelib-carte-portes-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let dir = crate::save::create_world(&root, "Nuke", 9).unwrap();
        let bare = package(&dir).unwrap();
        let mut m = load(&dir);
        let single = Zone::new([3, 64, 5], [3, 65, 5]);
        let double = Zone::new([10, 64, 2], [10, 65, 3]);
        m.door_list = vec![single, double];
        save(&dir, &m).unwrap();
        assert_eq!(load(&dir).door_list, vec![single, double]);
        let with = package(&dir).unwrap();
        assert_ne!(with.signature(), bare.signature(), "des portes posées : une autre carte");
        assert_eq!(with.fps.door_list, vec![single, double]);
        let copy = unpack(&root.join("maps"), "Copie", &with.to_bytes()).unwrap();
        let theirs = load(&copy);
        assert_eq!(theirs.door_list, vec![single, double]);
        assert_eq!(package(&copy).unwrap().signature(), with.signature());
        let t = fs::read_to_string(copy.join("torches.txt")).unwrap_or_default();
        assert!(!t.contains(DOOR_PREFIX));
        // Elles seules s'ouvrent ; avec `/fps set door on`, toutes.
        assert!(!theirs.doors());
        assert!(theirs.door_usable([3, 65, 5]) && theirs.door_usable([10, 64, 3]));
        assert!(!theirs.door_usable([4, 64, 5]));
        let mut all = theirs.clone();
        all.set_doors(true);
        assert!(all.door_usable([4, 64, 5]));
        let _ = fs::remove_dir_all(&root);
    }

    /// (v283) Les piles de tapis de plusieurs couleurs partent avec la carte : une autre
    /// signature (une carte sans en garde la sienne), les mêmes couleurs chez qui la reçoit.
    #[test]
    fn carpet_stacks_go_with_the_map() {
        let root = std::env::temp_dir().join(format!("minelib-carte-tapis-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let dir = crate::save::create_world(&root, "Salon", 4).unwrap();
        let bare = package(&dir).unwrap();
        fs::write(dir.join(crate::save::CARPETS_FILE), "3,70,4,red white\n-2,71,9,moss blue black\n").unwrap();
        let with = package(&dir).unwrap();
        assert_ne!(with.signature(), bare.signature());
        let copy = unpack(&root.join("maps"), "Copie", &with.to_bytes()).unwrap();
        let mut lines: Vec<String> = fs::read_to_string(copy.join(crate::save::CARPETS_FILE))
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect();
        lines.sort();
        assert_eq!(lines, ["-2,71,9,moss blue black", "3,70,4,red white"]);
        let t = fs::read_to_string(copy.join("torches.txt")).unwrap_or_default();
        assert!(!t.contains(CARPET_PREFIX));
        assert_eq!(package(&copy).unwrap().signature(), with.signature());
        let _ = fs::remove_dir_all(&root);
    }
}
