//! (v350) Mondes gardés par le relais : « que je puisse quand même le rejoindre sans que mon PC
//! ou celui d'Haumax ait la carte ».
//!
//! Tant qu'un monde de construction est ouvert, le relais en tient une copie sur son disque :
//! de quoi le rouvrir (nom, carte, signature, créateur, clé du monde), puis chaque entrée de
//! son historique (blocs, objets au sol et coffres), ajoutée au fil de l'eau, à chaque
//! entretien (toutes les 2 secondes). Sa carte, elle, est dans `maps` depuis son ouverture.
//! Quand son dernier joueur s'en va, la copie est simplement close ; si le relais s'arrête
//! d'un coup pendant que le monde est ouvert, elle a tout jusqu'aux dernières secondes, et le
//! monde s'endort au redémarrage avec elle. Tout joueur qui peut y entrer (pas banni, sur la
//! liste blanche s'il y en a une) le réveille alors depuis la liste, sans copie sur son PC
//! (`Request::WakeKept`) : le relais le rouvre avec cet historique, et son jeu télécharge la
//! carte comme pour le rejoindre.
//!
//! Le fichier : « MLGARDE1 », puis des enregistrements (longueur sur 4 octets, puis postcard) :
//! l'`Info`, puis les entrées, dans l'ordre. Un dernier enregistrement à moitié écrit (le
//! relais arrêté au milieu) est ignoré. Ces copies comptent dans la place des mondes
//! (`place`) : ce sont elles qui partent en premier quand il en faut.

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::zones::Entry;

const MAGIC: &[u8; 8] = b"MLGARDE1";

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

/// Une copie gardée : la carte qu'elle demande, sa taille sur le disque, depuis quand elle
/// dort, et si le monde est ouvert en ce moment (la copie grandit).
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Meta {
    pub map_sig: [u8; 32],
    pub size: u64,
    pub saved: u64,
    pub open: bool,
}

/// Les copies (`mondes_gardes/`, une par monde, et `index.bin`).
pub struct Store {
    dir: PathBuf,
    index: HashMap<String, Meta>,
}

fn record(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(bytes);
}

/// Lit une copie : son `Info` et ses entrées complètes.
fn parse(bytes: &[u8]) -> Option<(Info, Vec<Entry>)> {
    let mut rest = bytes.strip_prefix(MAGIC)?;
    let mut next = || -> Option<&[u8]> {
        let n = u32::from_le_bytes(rest.get(..4)?.try_into().ok()?) as usize;
        let r = rest.get(4..4 + n)?;
        rest = &rest[4 + n..];
        Some(r)
    };
    let info: Info = postcard::from_bytes(next()?).ok()?;
    let mut entries = Vec::new();
    while let Some(r) = next() {
        match postcard::from_bytes::<Entry>(r) {
            Ok(e) => entries.push(e),
            Err(_) => break,
        }
    }
    Some((info, entries))
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
        let mut changed = false;
        for (k, mut m) in index {
            let Ok(md) = std::fs::metadata(s.path(&k)) else {
                changed = true;
                continue;
            };
            // Le relais s'est arrêté pendant que ce monde était ouvert : il dort maintenant,
            // avec tout ce qui a été écrit.
            if m.open {
                m.open = false;
                m.saved = md
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(m.saved, |d| d.as_secs());
                changed = true;
            }
            m.size = md.len();
            s.index.insert(k, m);
        }
        if changed {
            s.save_index();
        }
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

    /// Le monde `info.server` s'ouvre : sa copie repart de ces entrées (celle d'avant est
    /// remplacée d'un coup).
    pub fn begin(&mut self, info: &Info, entries: &[Entry], now: u64) -> bool {
        let mut bytes = MAGIC.to_vec();
        let Ok(head) = postcard::to_allocvec(info) else {
            return false;
        };
        record(&mut bytes, &head);
        for e in entries {
            if let Ok(b) = postcard::to_allocvec(e) {
                record(&mut bytes, &b);
            }
        }
        if crate::accounts::write_atomic(&self.path(&info.server), &bytes).is_err() {
            return false;
        }
        self.index.insert(
            info.server.clone(),
            Meta {
                map_sig: info.map_sig,
                size: bytes.len() as u64,
                saved: now,
                open: true,
            },
        );
        self.save_index();
        true
    }

    /// Ce qui vient d'être fait dans le monde ouvert `key`, ajouté à sa copie.
    pub fn append(&mut self, key: &str, entries: &[Entry]) -> bool {
        if entries.is_empty() {
            return true;
        }
        let path = self.path(key);
        let Some(m) = self.index.get_mut(key) else {
            return false;
        };
        let mut bytes = Vec::new();
        for e in entries {
            if let Ok(b) = postcard::to_allocvec(e) {
                record(&mut bytes, &b);
            }
        }
        let ok = std::fs::OpenOptions::new()
            .append(true)
            .open(path)
            .and_then(|mut f| f.write_all(&bytes))
            .is_ok();
        if ok {
            m.size += bytes.len() as u64;
        }
        ok
    }

    /// Son dernier joueur est parti : la copie est close, le monde dort.
    pub fn close(&mut self, key: &str, now: u64) {
        if let Some(m) = self.index.get_mut(key) {
            m.open = false;
            m.saved = now;
            self.save_index();
        }
    }

    /// La copie du monde `key` (il se rouvre avec elle ; elle reste sur le disque jusqu'à ce
    /// que `begin` la remplace).
    pub fn read(&self, key: &str) -> Option<(Info, Vec<Entry>)> {
        self.index.get(key)?;
        let (info, entries) = parse(&std::fs::read(self.path(key)).ok()?)?;
        (info.server == key).then_some((info, entries))
    }

    /// La copie du monde `key` ne sert plus (supprimé, oublié pour faire de la place, ou sa
    /// carte n'est pas sur ce relais).
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Une copie s'écrit au fil de l'eau ; relue après un arrêt du relais au milieu d'un
    /// enregistrement, elle a tout ce qui était complet, et le monde y dort.
    #[test]
    fn a_kept_world_survives_the_relay_stopping_mid_write() {
        let dir = std::env::temp_dir().join(format!("minelib-gardes-{}", rand::random::<u32>()));
        let info = Info {
            name: "Monde".into(),
            map: "monde".into(),
            map_sig: [3; 32],
            max_players: 0,
            creator: "Ana".into(),
            server: "ana/monde".into(),
        };
        let mut s = Store::open(dir.clone());
        assert!(s.begin(&info, &[([0, 0], 1, vec![1])], 10));
        assert!(s.append(
            "ana/monde",
            &[([0, 1], 2, vec![2, 2]), ([1, 0], 3, vec![3])]
        ));
        assert!(s.meta("ana/monde").unwrap().open);
        // Le relais s'arrête au milieu d'une écriture.
        let path = s.path("ana/monde");
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(&[9, 0, 0, 0, 1, 2])
            .unwrap();
        drop(s);
        let s = Store::open(dir.clone());
        let m = s.meta("ana/monde").unwrap();
        assert!(!m.open, "il dort maintenant");
        assert_eq!(m.size, std::fs::metadata(&path).unwrap().len());
        let (back, entries) = s.read("ana/monde").unwrap();
        assert_eq!(back, info);
        assert_eq!(
            entries,
            vec![
                ([0, 0], 1, vec![1]),
                ([0, 1], 2, vec![2, 2]),
                ([1, 0], 3, vec![3])
            ]
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
