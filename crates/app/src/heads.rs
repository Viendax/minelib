//! Têtes à skin des cartes importées (décors de Minecraft faits de têtes de joueur) : la
//! texture de chaque tête est téléchargée une fois depuis textures.minecraft.net, gardée
//! dans `.minelib/tetes/`, puis la tête est dessinée pixel par pixel comme celle du modèle
//! du joueur, par-dessus le bloc « tête » uni.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

use glam::{Mat4, Quat, Vec3};
use minelib_render::EntityVertex;

use crate::skin::Skin;

/// Distance maximale des têtes dessinées (blocs).
const RANGE: f32 = 200.0;
/// Téléchargements en même temps.
const PARALLEL: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Pose {
    /// Au sol, rotation 0..15 (comme Minecraft : 0 regarde le sud, 4 l'ouest…).
    Floor(u8),
    /// Contre un mur (côté du mur : 0 +X, 1 −X, 4 +Z, 5 −Z).
    Wall(u8),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Head {
    pub pos: [i32; 3],
    pub pose: Pose,
    pub hash: String,
}

/// Lit une ligne `x,y,z,pose,empreinte` de `tetes.txt`.
pub fn parse(line: &str) -> Option<Head> {
    let v: Vec<&str> = line.trim().split(',').collect();
    let [x, y, z, pose, hash] = v.as_slice() else {
        return None;
    };
    let pose = match pose.strip_prefix('w') {
        Some(s) => Pose::Wall(s.parse().ok().filter(|s| matches!(s, 0 | 1 | 4 | 5))?),
        None => Pose::Floor(pose.parse::<u8>().ok()? & 15),
    };
    let hash = hash.trim();
    if hash.len() < 32 || hash.len() > 80 || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some(Head {
        pos: [x.parse().ok()?, y.parse().ok()?, z.parse().ok()?],
        pose,
        hash: hash.to_ascii_lowercase(),
    })
}

/// Passage des pixels du modèle de la tête au monde.
pub fn transform(h: &Head) -> Mat4 {
    let [x, y, z] = h.pos.map(|v| v as f32);
    let (center, dir) = match h.pose {
        Pose::Floor(r) => {
            // Calée sur la hitbox du bloc (un pavé droit) : le quart de tour le plus
            // proche de sa rotation.
            let q = ((r as u32 + 2) / 4 % 4) * 4;
            let t = q as f32 * std::f32::consts::TAU / 16.0;
            (
                Vec3::new(x + 0.5, y, z + 0.5),
                Vec3::new(-t.sin(), 0.0, t.cos()),
            )
        }
        Pose::Wall(s) => {
            let wall = match s {
                0 => Vec3::X,
                1 => -Vec3::X,
                4 => Vec3::Z,
                _ => -Vec3::Z,
            };
            (Vec3::new(x + 0.5, y + 0.25, z + 0.5) + wall * 0.25, -wall)
        }
    };
    // Le visage du modèle regarde −z : on le tourne vers `dir`.
    let yaw = (-dir.x).atan2(-dir.z);
    // Juste la hitbox du bloc « tête » (8 pixels = un demi-bloc).
    Mat4::from_translation(center)
        * Mat4::from_quat(Quat::from_rotation_y(yaw))
        * Mat4::from_scale(Vec3::splat(1.0 / 16.0))
}

pub struct Heads {
    list: Vec<Head>,
    cache: PathBuf,
    skins: HashMap<String, Arc<Skin>>,
    /// Demandées (téléchargement en cours ou raté) : pas redemandées.
    asked: HashSet<String>,
    running: usize,
    tx: Sender<(String, Option<Vec<u8>>)>,
    rx: Receiver<(String, Option<Vec<u8>>)>,
    /// Têtes par zone de 16 × 16 blocs : leurs triangles sont faits une fois et gardés
    /// sur la carte graphique (rien n'est refait quand on se déplace).
    cells: HashMap<(i32, i32), Cell>,
    next_gen: u64,
    /// Têtes cassées (indices dans `list`) : plus dessinées.
    gone: HashSet<usize>,
}

/// Têtes d'une zone et leurs triangles.
struct Cell {
    heads: Vec<usize>,
    verts: Vec<EntityVertex>,
    /// Version des triangles (change quand ils sont refaits) ; 0 : pas encore faits.
    gen: u64,
    light: [f32; 3],
    /// Têtes texturées lors de la dernière construction.
    textured: usize,
    /// Finesse de la dernière construction (1 de près, 2 ou 4 de loin).
    lod: usize,
}

/// Finesse voulue à cette distance ; `current` : celle en place (on n'en change qu'une
/// fois la limite franchement passée, pour ne pas refaire la zone sans arrêt).
fn lod_for(d: f32, current: usize) -> usize {
    let want = if d < 40.0 {
        1
    } else if d < 90.0 {
        2
    } else {
        4
    };
    let keep = match current {
        1 => d < 48.0,
        2 => (32.0..104.0).contains(&d),
        4 => d >= 76.0,
        _ => false,
    };
    if keep {
        current
    } else {
        want
    }
}

impl Heads {
    /// Têtes du monde `dir` ; textures gardées dans `cache`.
    pub fn load(dir: &Path, cache: PathBuf) -> Self {
        let list = std::fs::read_to_string(dir.join(crate::mcimport::HEADS_FILE))
            .unwrap_or_default()
            .lines()
            .filter_map(parse)
            .collect();
        let (tx, rx) = channel();
        let list: Vec<Head> = list;
        let mut cells: HashMap<(i32, i32), Cell> = HashMap::new();
        for (i, h) in list.iter().enumerate() {
            cells
                .entry((h.pos[0] >> 4, h.pos[2] >> 4))
                .or_insert_with(|| Cell {
                    heads: Vec::new(),
                    verts: Vec::new(),
                    gen: 0,
                    light: [0.0; 3],
                    textured: 0,
                    lod: 0,
                })
                .heads
                .push(i);
        }
        Self {
            list,
            cache,
            skins: HashMap::new(),
            asked: HashSet::new(),
            running: 0,
            tx,
            rx,
            cells,
            next_gen: 0,
            gone: HashSet::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    fn file(&self, hash: &str) -> PathBuf {
        self.cache.join(format!("{hash}.png"))
    }

    /// Textures reçues, nouvelles demandes (les têtes proches d'abord).
    pub fn update(&mut self, cam: Vec3) {
        while let Ok((hash, bytes)) = self.rx.try_recv() {
            self.running = self.running.saturating_sub(1);
            if let Some(skin) = bytes.as_deref().and_then(|b| Skin::from_png(b).ok()) {
                if let Some(b) = &bytes {
                    let _ = std::fs::create_dir_all(&self.cache);
                    let _ = std::fs::write(self.file(&hash), b);
                }
                self.skins.insert(hash, skin);
            }
        }
        if self.running >= PARALLEL {
            return;
        }
        let mut near: Vec<&Head> = self
            .list
            .iter()
            .filter(|h| !self.skins.contains_key(&h.hash) && !self.asked.contains(&h.hash))
            .filter(|h| center(h).distance(cam) < RANGE * 1.5)
            .collect();
        near.sort_by(|a, b| center(a).distance(cam).total_cmp(&center(b).distance(cam)));
        let wanted: Vec<String> = near
            .iter()
            .map(|h| h.hash.clone())
            .take(PARALLEL - self.running)
            .collect();
        for hash in wanted {
            self.asked.insert(hash.clone());
            // Déjà sur le disque.
            if let Ok(b) = std::fs::read(self.file(&hash)) {
                if let Ok(skin) = Skin::from_png(&b) {
                    self.skins.insert(hash, skin);
                    continue;
                }
            }
            self.running += 1;
            let tx = self.tx.clone();
            let _ = std::thread::Builder::new()
                .name("tête".into())
                .spawn(move || {
                    let bytes = download(&hash)
                        .map_err(|e| log::info!("texture de tête {hash} : {e:#}"))
                        .ok();
                    let _ = tx.send((hash, bytes));
                });
        }
    }

    /// Zones proches de la caméra à (re)faire : pas encore construites, texture arrivée,
    /// lumière changée. Une seule par appel (jamais d'à-coup).
    pub fn refresh(&mut self, cam: Vec3, light: [f32; 3]) {
        let mut best: Option<((i32, i32), f32)> = None;
        for (&key, c) in &self.cells {
            let d = cell_distance(key, cam);
            if d > RANGE + 16.0 {
                continue;
            }
            let textured = c
                .heads
                .iter()
                .filter(|&&i| {
                    !self.gone.contains(&i) && self.skins.contains_key(&self.list[i].hash)
                })
                .count();
            let dirty = c.gen == 0
                || lod_for(d, c.lod) != c.lod
                || textured != c.textured
                || c.light.iter().zip(light).any(|(a, b)| (a - b).abs() > 0.03);
            if dirty && best.is_none_or(|b| d < b.1) {
                best = Some((key, d));
            }
        }
        let Some((key, d)) = best else { return };
        let lod = lod_for(d, self.cells[&key].lod);
        let ids = self.cells[&key].heads.clone();
        let mut verts = Vec::new();
        let mut textured = 0;
        for i in ids {
            if self.gone.contains(&i) {
                continue;
            }
            let h = &self.list[i];
            match self.skins.get(&h.hash) {
                Some(skin) => {
                    textured += 1;
                    verts.extend(crate::player_model::head(skin, transform(h), light, lod));
                }
                None => verts.extend(crate::player_model::default_head(transform(h), light)),
            }
        }
        self.next_gen += 1;
        let c = self.cells.get_mut(&key).expect("zone");
        c.verts = verts;
        c.gen = self.next_gen;
        c.light = light;
        c.textured = textured;
        c.lod = lod;
    }

    /// Le bloc « tête » en `pos` a été cassé : sa tête texturée disparaît (de l'écran et
    /// du fichier du monde `dir`).
    pub fn remove_at(&mut self, pos: [i32; 3], dir: &Path) {
        let Some(i) =
            (0..self.list.len()).find(|i| self.list[*i].pos == pos && !self.gone.contains(i))
        else {
            return;
        };
        self.gone.insert(i);
        if let Some(c) = self.cells.get_mut(&(pos[0] >> 4, pos[2] >> 4)) {
            // À refaire (version changée même si la zone devient vide).
            self.next_gen += 1;
            c.textured = usize::MAX;
            if c.heads.iter().all(|h| self.gone.contains(h)) {
                c.verts.clear();
                c.gen = self.next_gen;
            }
        }
        let path = dir.join(crate::mcimport::HEADS_FILE);
        if let Ok(text) = std::fs::read_to_string(&path) {
            let prefix = format!("{},{},{},", pos[0], pos[1], pos[2]);
            let kept: Vec<&str> = text
                .lines()
                .filter(|l| {
                    let frame = l.split(',').nth(3).is_some_and(|f| f.starts_with('f'));
                    !l.starts_with(&prefix) || frame
                })
                .collect();
            let mut t = kept.join("\n");
            if !t.is_empty() {
                t.push('\n');
            }
            let _ = std::fs::write(&path, t);
        }
    }

    /// Zones à dessiner : (identifiant, triangles, version).
    pub fn visible(&self, cam: Vec3) -> Vec<(u64, &[EntityVertex], u64)> {
        self.cells
            .iter()
            .filter(|(&k, c)| c.gen != 0 && cell_distance(k, cam) < RANGE + 8.0)
            .map(|(&(x, z), c)| {
                (
                    (x as u32 as u64) << 32 | z as u32 as u64,
                    c.verts.as_slice(),
                    c.gen,
                )
            })
            .collect()
    }
}

/// Distance horizontale de la caméra au centre d'une zone.
fn cell_distance(key: (i32, i32), cam: Vec3) -> f32 {
    let c = Vec3::new(key.0 as f32 * 16.0 + 8.0, cam.y, key.1 as f32 * 16.0 + 8.0);
    c.distance(cam)
}

fn center(h: &Head) -> Vec3 {
    Vec3::new(
        h.pos[0] as f32 + 0.5,
        h.pos[1] as f32 + 0.5,
        h.pos[2] as f32 + 0.5,
    )
}

/// Texture d'une tête (PNG 64 × 64) depuis les serveurs de textures de Minecraft.
fn download(hash: &str) -> anyhow::Result<Vec<u8>> {
    use ureq::tls::{RootCerts, TlsConfig};
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .tls_config(
            TlsConfig::builder()
                .root_certs(RootCerts::PlatformVerifier)
                .build(),
        )
        .timeout_global(Some(std::time::Duration::from_secs(20)))
        .build()
        .into();
    let mut resp = agent
        .get(&format!("https://textures.minecraft.net/texture/{hash}"))
        .header("User-Agent", "minelib")
        .call()?;
    let bytes = resp
        .body_mut()
        .with_config()
        .limit(256 * 1024)
        .read_to_vec()?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_head_lines_and_turns_faces_the_right_way() {
        let hash = "31a7973f25cbbb078f595c04349fc541ac1df9a44b8c1a16c528581aa4170";
        let h = parse(&format!("6,168,-298,8,{hash}")).unwrap();
        assert_eq!(h.pose, Pose::Floor(8));
        assert!(
            parse("1,2,3,w2,abcdefabcdefabcdefabcdefabcdefab").is_none(),
            "mur inconnu"
        );
        assert!(parse("1,2,3,0,zzz").is_none());
        // Visage (−z du modèle) : rotation 0 regarde le sud (+z), 8 le nord.
        let face = |p: Pose| {
            let m = transform(&Head {
                pos: [0, 0, 0],
                pose: p,
                hash: hash.into(),
            });
            m.transform_vector3(-Vec3::Z).normalize()
        };
        assert!(face(Pose::Floor(0)).distance(Vec3::Z) < 1e-4);
        assert!(face(Pose::Floor(4)).distance(-Vec3::X) < 1e-4);
        assert!(face(Pose::Floor(8)).distance(-Vec3::Z) < 1e-4);
        // Calée sur la hitbox : rotation au quart de tour le plus proche, demi-bloc.
        assert!(face(Pose::Floor(3)).distance(-Vec3::X) < 1e-4);
        assert!(face(Pose::Floor(1)).distance(Vec3::Z) < 1e-4);
        let m = transform(&Head {
            pos: [0, 0, 0],
            pose: Pose::Floor(5),
            hash: hash.into(),
        });
        let (a, b) = (
            m.transform_point3(Vec3::new(-4.0, 0.0, -4.0)),
            m.transform_point3(Vec3::new(4.0, 8.0, 4.0)),
        );
        let (lo, hi) = (a.min(b), a.max(b));
        assert!(lo.distance(Vec3::new(0.25, 0.0, 0.25)) < 1e-4, "{lo}");
        assert!(hi.distance(Vec3::new(0.75, 0.5, 0.75)) < 1e-4, "{hi}");
        // Contre le mur nord (côté 5, −z) : regarde le sud.
        assert!(face(Pose::Wall(5)).distance(Vec3::Z) < 1e-4);
        let m = transform(&Head {
            pos: [0, 0, 0],
            pose: Pose::Wall(5),
            hash: hash.into(),
        });
        let c = m.transform_point3(Vec3::new(0.0, 4.0, 0.0));
        assert!((c.z - 0.25).abs() < 1e-3 && (c.y - 0.5).abs() < 0.01, "{c}");
    }
}
