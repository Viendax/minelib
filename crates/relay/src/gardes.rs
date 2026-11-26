//! (v350) Mondes endormis gardés par le relais : « que je puisse quand même le rejoindre sans
//! que mon PC ou celui d'Haumax ait la carte ».
//!
//! Quand le dernier joueur d'un monde de construction s'en va, le relais en garde de quoi le
//! refaire : la carte envoyée à son ouverture (déjà dans `maps`) et tout ce qui y a été fait
//! depuis (l'historique de ses zones, `zones::Build::export_entries`). Tout joueur qui peut y
//! entrer (pas banni, sur la liste blanche s'il y en a une) peut alors le réveiller depuis la
//! liste, sans copie sur son PC (`Request::WakeKept`) : le relais le rouvre avec cet
//! historique, et son jeu télécharge la carte comme pour le rejoindre. Le dernier joueur qui
//! part en garde aussi sa copie, comme avant.
//!
//! Ces copies comptent dans la place des mondes (`place`) : ce sont elles qui partent en
//! premier quand il en faut.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::zones::Entry;

/// Ce qu'il faut pour rouvrir un monde.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Info {
    pub name: String,
    pub map: String,
    pub map_sig: [u8; 32],
    pub max_players: u32,
    pub creator: String,
    pub server: String,
}

#[derive(Serialize, Deserialize)]
struct Kept {
    info: Info,
    entries: Vec<Entry>,
}

/// Une copie gardée : la carte qu'elle demande, sa taille sur le disque, quand.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Meta {
    pub map_sig: [u8; 32],
    pub size: u64,
    pub saved: u64,
}

/// Les copies (`mondes_gardes/`, une par monde, et `index.bin`).
pub struct Store {
    dir: PathBuf,
    index: HashMap<String, Meta>,
}

impl Store {
    pub fn open(dir: PathBuf) -> Self {
        let _ = std::fs::create_dir_all(&dir);
        let mut s = Self {
            dir,
            index: HashMap::new(),
        };
        let index: Vec<(String, Meta)> = std::fs::read(s.dir.join("index.bin"))
            .ok()
            .and_then(|b| postcard::from_bytes(&b).ok())
            .unwrap_or_default();
        s.index = index
            .into_iter()
            .filter(|(k, _)| s.path(k).exists())
            .collect();
        s
    }

    fn path(&self, key: &str) -> PathBuf {
        use sha2::{Digest, Sha256};
        let h: [u8; 32] = Sha256::digest(key.as_bytes()).into();
        self.dir
            .join(format!("{}.monde", minelib_net::crypto::hex(&h[..12])))
    }

    fn save_index(&self) {
        let v: Vec<(&String, &Meta)> = self.index.iter().collect();
        if let Ok(b) = postcard::to_allocvec(&v) {
            let _ = crate::accounts::write_atomic(&self.dir.join("index.bin"), &b);
        }
    }

    /// Le monde `info.server` s'endort : sa copie (celle d'avant est remplacée).
    pub fn save(&mut self, info: Info, entries: Vec<Entry>, now: u64) -> bool {
        let key = info.server.clone();
        let map_sig = info.map_sig;
        let Ok(bytes) = postcard::to_allocvec(&Kept { info, entries }) else {
            return false;
        };
        if crate::accounts::write_atomic(&self.path(&key), &bytes).is_err() {
            return false;
        }
        self.index.insert(
            key,
            Meta {
                map_sig,
                size: bytes.len() as u64,
                saved: now,
            },
        );
        self.save_index();
        true
    }

    /// La copie du monde `key`, retirée d'ici (il se rouvre avec elle).
    pub fn take(&mut self, key: &str) -> Option<(Info, Vec<Entry>)> {
        self.index.get(key)?;
        let bytes = std::fs::read(self.path(key)).ok();
        self.drop(key);
        let k: Kept = postcard::from_bytes(&bytes?).ok()?;
        (k.info.server == key).then_some((k.info, k.entries))
    }

    /// La copie du monde `key` ne sert plus (rouvert depuis le PC de son gardien, supprimé,
    /// oublié pour faire de la place).
    pub fn drop(&mut self, key: &str) {
        if self.index.remove(key).is_some() {
            let _ = std::fs::remove_file(self.path(key));
            self.save_index();
        }
    }

    pub fn meta(&self, key: &str) -> Option<&Meta> {
        self.index.get(key)
    }

    /// Les cartes dont ces copies ont besoin.
    pub fn map_sigs(&self) -> impl Iterator<Item = [u8; 32]> + '_ {
        self.index.values().map(|m| m.map_sig)
    }
}
