//! Machines vues par compte et machines bannies (3 mois). Seules des empreintes salées par
//! la clé du relais sont gardées, une par composant (`minelib_net::hw`) : une machine est
//! reconnue tant qu'elle garde au moins deux composants.

use std::collections::HashMap;
use std::path::PathBuf;

use minelib_net::hw::same_machine;
use serde::{Deserialize, Serialize};

/// Durée du ban d'une machine.
pub const BAN_SECS: u64 = 90 * 24 * 3600;

type Machine = Vec<[u8; 32]>;

#[derive(Serialize, Deserialize, Default)]
struct Data {
    seen: HashMap<String, Vec<Machine>>,
    bans: Vec<(Machine, u64)>,
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
    pub fn seen(&mut self, pseudo: &str, hw: Machine) {
        if hw.is_empty() {
            return;
        }
        let v = self
            .data
            .seen
            .entry(pseudo.to_ascii_lowercase())
            .or_default();
        match v.iter_mut().find(|m| same_machine(m, &hw)) {
            // Même machine : composants changés retenus aussi (le ban suivra).
            Some(m) => {
                for c in hw {
                    if !m.contains(&c) {
                        m.push(c);
                    }
                }
                m.truncate(32);
            }
            None => {
                v.push(hw);
                if v.len() > 8 {
                    v.remove(0);
                }
            }
        }
        self.save();
    }

    /// Fin du ban de cette machine, si elle est bannie.
    pub fn banned_until(&self, hw: &[[u8; 32]]) -> Option<u64> {
        let t = now();
        self.data
            .bans
            .iter()
            .filter(|(m, until)| *until > t && same_machine(m, hw))
            .map(|(_, u)| *u)
            .max()
    }

    /// Bannit pour 3 mois toutes les machines de ce compte.
    pub fn ban_account_machines(&mut self, pseudo: &str) -> usize {
        let until = now() + BAN_SECS;
        let t = now();
        self.data.bans.retain(|(_, u)| *u > t);
        let machines = self
            .data
            .seen
            .get(&pseudo.to_ascii_lowercase())
            .cloned()
            .unwrap_or_default();
        for m in &machines {
            self.data.bans.push((m.clone(), until));
        }
        self.save();
        machines.len()
    }
}
