//! Cartes FPS partagées : le relais garde chaque carte entière (vérifiée par sa
//! signature) pour que tout le monde puisse la télécharger avant de jouer.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Result;
use minelib_net::crypto;
use minelib_net::game;
use minelib_net::proto::{MapEntry, MAP_MAX, MAP_PART};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Meta {
    pub sig: [u8; 32],
    pub name: String,
    pub author: String,
    pub size: u64,
    pub plays: u32,
}

/// Envoi en cours : (expéditeur, nom, taille annoncée, octets reçus).
type Upload = (u64, String, u64, Vec<u8>);

pub struct Store {
    dir: PathBuf,
    pub index: HashMap<[u8; 32], Meta>,
    uploads: HashMap<[u8; 32], Upload>,
}

pub enum Put {
    /// Reçu jusque-là.
    Partial(u64),
    Complete,
    Refused(&'static str),
}

impl Store {
    pub fn open(dir: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&dir)?;
        let index = std::fs::read(dir.join("index.bin"))
            .ok()
            .and_then(|b| postcard::from_bytes::<Vec<Meta>>(&b).ok())
            .unwrap_or_default()
            .into_iter()
            .filter(|m| dir.join(file(&m.sig)).exists())
            .map(|m| (m.sig, m))
            .collect();
        Ok(Self {
            dir,
            index,
            uploads: HashMap::new(),
        })
    }

    fn save_index(&self) {
        let v: Vec<&Meta> = self.index.values().collect();
        if let Ok(b) = postcard::to_allocvec(&v) {
            let _ = crate::accounts::write_atomic(&self.dir.join("index.bin"), &b);
        }
    }

    pub fn has(&self, sig: &[u8; 32]) -> bool {
        self.index.contains_key(sig)
    }

    pub fn played(&mut self, sig: &[u8; 32]) {
        if let Some(m) = self.index.get_mut(sig) {
            m.plays += 1;
            self.save_index();
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn put(
        &mut self,
        from: u64,
        author: &str,
        sig: [u8; 32],
        name: &str,
        offset: u64,
        total: u64,
        data: &[u8],
    ) -> Put {
        if self.has(&sig) {
            return Put::Complete;
        }
        if total == 0 || total > MAP_MAX || data.len() > MAP_PART {
            return Put::Refused("carte trop grande");
        }
        if offset == 0 {
            self.uploads.insert(
                sig,
                (from, name.chars().take(40).collect(), total, Vec::new()),
            );
        }
        let Some(up) = self.uploads.get_mut(&sig) else {
            return Put::Partial(0);
        };
        if up.0 != from || up.2 != total || offset != up.3.len() as u64 {
            return Put::Partial(up.3.len() as u64);
        }
        up.3.extend_from_slice(data);
        if (up.3.len() as u64) < total {
            return Put::Partial(up.3.len() as u64);
        }
        let (_, name, _, bytes) = self.uploads.remove(&sig).expect("envoi");
        if game::signature(&bytes) != sig
            || postcard::from_bytes::<game::MapPackage>(&bytes).is_err()
        {
            return Put::Refused("signature de la carte fausse");
        }
        if crate::accounts::write_atomic(&self.dir.join(file(&sig)), &bytes).is_err() {
            return Put::Refused("écriture impossible");
        }
        self.index.insert(
            sig,
            Meta {
                sig,
                name,
                author: author.to_string(),
                size: total,
                plays: 0,
            },
        );
        self.save_index();
        log::info!("carte {} reçue de « {author} »", crypto::hex(&sig[..6]));
        Put::Complete
    }

    /// Morceau d'une carte : (taille totale, octets).
    pub fn get(&self, sig: &[u8; 32], offset: u64) -> Option<(u64, Vec<u8>)> {
        let m = self.index.get(sig)?;
        use std::io::{Read, Seek, SeekFrom};
        let mut f = std::fs::File::open(self.dir.join(file(sig))).ok()?;
        f.seek(SeekFrom::Start(offset)).ok()?;
        let mut buf = vec![0u8; MAP_PART.min(m.size.saturating_sub(offset) as usize)];
        f.read_exact(&mut buf).ok()?;
        Some((m.size, buf))
    }

    pub fn entries(&self) -> Vec<MapEntry> {
        self.index
            .values()
            .map(|m| MapEntry {
                sig: m.sig,
                name: m.name.clone(),
                author: m.author.clone(),
                size: m.size,
                plays: m.plays,
                waiting: 0,
                stored: true,
            })
            .collect()
    }

    /// Un jeu se déconnecte : ses envois en cours sont abandonnés.
    pub fn forget(&mut self, from: u64) {
        self.uploads.retain(|_, u| u.0 != from);
    }
}

fn file(sig: &[u8; 32]) -> String {
    format!("{}.map", crypto::hex(sig))
}
