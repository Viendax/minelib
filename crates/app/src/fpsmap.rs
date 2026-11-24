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

/// (v224) Préfixe des lumières de l'éditeur parmi les torches d'un paquet.
const LIGHT_PREFIX: &str = "lumiere,";

/// (v258) Préfixe des portes posées pour la partie parmi les torches d'un paquet.
const DOOR_PREFIX: &str = "porte,";

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
    // lignes au fichier du monde.
    let generator: String = p
        .generator
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .take(24)
        .collect();
    fs::write(
        dir.join("level.txt"),
        format!(
            "name={name}\nseed={}\ncreated=0\nlast_played=0\nclock=0\nnight=0\ngenerator={}\n\
             gamemode=fps\nfly=0\nhealth=20\n",
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
            !l.starts_with("heure,") && !l.starts_with(LIGHT_PREFIX) && !l.starts_with(DOOR_PREFIX)
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
fn copy_dir(src: &Path, dst: &Path) -> Result<()> {
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
}
