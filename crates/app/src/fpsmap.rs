//! Cartes FPS : réglages (`fps.txt` dans le dossier du monde : zones de départ CT / T et
//! sites de bombe A / B / C, posés avec `/fps set …` après une sélection à la baguette),
//! paquet partageable et signature.
//!
//! La signature est l'empreinte du paquet (terrain, modifications, torches, réglages FPS) :
//! changer un départ ou un site, c'est une autre carte ; deux copies identiques (même sous
//! deux noms) sont la même carte et n'apparaissent qu'une fois dans la liste.

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
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
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
    fs::write(dir.join(FILE), s)?;
    Ok(())
}

pub const SITE_NAMES: [&str; 3] = ["A", "B", "C"];

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
    torches.sort();
    torches.dedup();
    Ok(MapPackage {
        seed,
        generator,
        edits,
        chunks,
        torches,
        fps: load(dir),
    })
}

/// Signatures déjà calculées : (dossier) → (date des fichiers, signature).
#[derive(Default)]
pub struct SigCache {
    cache: HashMap<PathBuf, (u64, [u8; 32])>,
}

fn stamp(dir: &Path) -> u64 {
    ["level.txt", "edits.bin", "chunks.bin", "torches.txt", FILE]
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
    fs::write(
        dir.join("level.txt"),
        format!(
            "name={name}\nseed={}\ncreated=0\nlast_played=0\nclock=0\nnight=0\ngenerator={}\n\
             gamemode=fps\nfly=0\nhealth=20\n",
            p.seed, p.generator
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
    let mut t = p.torches.join("\n");
    if !t.is_empty() {
        t.push('\n');
    }
    fs::write(dir.join("torches.txt"), t)?;
    save(&dir, &p.fps)?;
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
