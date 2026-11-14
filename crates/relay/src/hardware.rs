//! Machines vues par compte et machines bannies (3 mois). Seules des empreintes salées par
//! la clé du relais sont gardées (`minelib_net::hw`).

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Durée du ban d'une machine.
pub const BAN_SECS: u64 = 90 * 24 * 3600;

#[derive(Serialize, Deserialize, Default)]
struct Data {
    seen: HashMap<String, Vec<[u8; 32]>>,
    bans: Vec<([u8; 32], u64)>,
}

pub struct Store {
    path: PathBuf,
    data: Data,
}

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

impl Store {
    pub fn open(path: PathBuf) -> Self {
        let data = std::fs::read(&path)
            .ok()
            .and_then(|b| postcard::from_bytes(&b).ok())
            .unwrap_or_default();
        Self { path, data }
    }

    fn save(&self) {
        if let Ok(b) = postcard::to_allocvec(&self.data) {
            let _ = crate::accounts::write_atomic(&self.path, &b);
        }
    }

    /// Le compte se connecte depuis cette machine.
    pub fn seen(&mut self, pseudo: &str, hw: [u8; 32]) {
        let v = self
            .data
            .seen
            .entry(pseudo.to_ascii_lowercase())
            .or_default();
        if !v.contains(&hw) {
            v.push(hw);
            if v.len() > 8 {
                v.remove(0);
            }
            self.save();
        }
    }

    /// Fin du ban de cette machine, si elle est bannie.
    pub fn banned_until(&self, hw: &[u8; 32]) -> Option<u64> {
        let t = now();
        self.data
            .bans
            .iter()
            .filter(|(h, until)| h == hw && *until > t)
            .map(|(_, u)| *u)
            .max()
    }

    /// Bannit pour 3 mois toutes les machines de ce compte.
    pub fn ban_account_machines(&mut self, pseudo: &str) -> usize {
        let until = now() + BAN_SECS;
        let t = now();
        self.data.bans.retain(|(_, u)| *u > t);
        let hws = self
            .data
            .seen
            .get(&pseudo.to_ascii_lowercase())
            .cloned()
            .unwrap_or_default();
        for h in &hws {
            self.data.bans.push((*h, until));
        }
        self.save();
        hws.len()
    }
}
