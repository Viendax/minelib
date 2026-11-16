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

/// Têtes dessinées au plus (les plus proches), et distance maximale (blocs).
const MAX_SHOWN: usize = 48;
const RANGE: f32 = 40.0;
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
            let t = r as f32 * std::f32::consts::TAU / 16.0;
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
    // Un peu plus grande que le bloc « tête » uni qu'elle recouvre.
    Mat4::from_translation(center - Vec3::Y * 0.005)
        * Mat4::from_quat(Quat::from_rotation_y(yaw))
        * Mat4::from_scale(Vec3::splat(1.04 / 16.0))
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
    /// Triangles déjà faits par tête (indice dans `list`), avec la lumière utilisée.
    built: HashMap<usize, ([f32; 3], Vec<EntityVertex>)>,
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
        Self {
            list,
            cache,
            skins: HashMap::new(),
            asked: HashSet::new(),
            running: 0,
            tx,
            rx,
            built: HashMap::new(),
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

    /// Triangles des têtes proches de la caméra dont la texture est là.
    pub fn vertices(&mut self, cam: Vec3, light: [f32; 3]) -> Vec<EntityVertex> {
        let mut near: Vec<(usize, f32)> = self
            .list
            .iter()
            .enumerate()
            .filter(|(_, h)| self.skins.contains_key(&h.hash))
            .map(|(i, h)| (i, center(h).distance(cam)))
            .filter(|(_, d)| *d < RANGE)
            .collect();
        near.sort_by(|a, b| a.1.total_cmp(&b.1));
        near.truncate(MAX_SHOWN);
        let mut out = Vec::new();
        for (i, _) in near {
            let h = &self.list[i];
            let fresh = self
                .built
                .get(&i)
                .is_some_and(|(l, _)| l.iter().zip(light).all(|(a, b)| (a - b).abs() < 0.02));
            if !fresh {
                let skin = &self.skins[&h.hash];
                let v = crate::player_model::head(skin, transform(h), light);
                self.built.insert(i, (light, v));
            }
            out.extend_from_slice(&self.built[&i].1);
        }
        out
    }
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
