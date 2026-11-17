//! Inventaires tenus par le relais, pour les joueurs en survie d'un serveur de
//! construction. Le jeu d'un joueur dit ce qu'il a ; un jeu modifié pourrait « lâcher » ou
//! ranger dans un coffre des objets qu'il n'a jamais eus. Ici le relais compte lui-même :
//!
//! - ce qu'on ramasse ou prend dans un coffre entre dans le sac (le relais arbitre déjà
//!   ces échanges, il sait exactement quoi) ;
//! - chaque bloc cassé donne un « crédit de minage » : un objet que le relais ne connaît
//!   pas (il ne garde pas le terrain) mais que le joueur a bien pu obtenir ;
//! - lâcher ou ranger un objet le sort du sac ; s'il n'y est pas, il est pris sur les
//!   crédits de minage ; sinon c'est refusé ;
//! - poser un bloc le sort du sac (ou d'un crédit).
//!
//! Un jeu modifié ne peut donc plus créer d'objets à partir de rien : au pire autant
//! qu'il a cassé de blocs. Le mode qui compte est celui que le relais connaît (mode par
//! défaut du serveur, ou donné par un OP), pas celui que le jeu annonce. En créatif, rien
//! n'est compté : tout y est illimité.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Crédits de minage gardés au plus par joueur.
const MINED_MAX: u32 = 1_000_000;

#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq)]
pub struct Bag {
    /// Objets connus du relais : code → nombre.
    pub items: HashMap<u32, u32>,
    /// Blocs cassés pas encore « dépensés ».
    pub mined: u32,
}

impl Bag {
    fn gain(&mut self, item: u32, count: u32) {
        *self.items.entry(item).or_default() += count;
    }

    /// Sort `count` objets du sac (sinon des crédits de minage) ; faux s'il n'y a ni l'un
    /// ni l'autre (rien n'est alors retiré).
    fn spend(&mut self, item: u32, count: u32) -> bool {
        let have = self.items.get(&item).copied().unwrap_or(0);
        let from_bag = have.min(count);
        let rest = count - from_bag;
        if rest > self.mined {
            return false;
        }
        self.mined -= rest;
        if from_bag > 0 {
            if have == from_bag {
                self.items.remove(&item);
            } else {
                self.items.insert(item, have - from_bag);
            }
        }
        true
    }
}

#[derive(Serialize, Deserialize, Default)]
struct Data {
    /// Serveur → pseudo (minuscules) → sac.
    bags: HashMap<String, HashMap<String, Bag>>,
    /// Serveur → pseudo (minuscules) → mode donné par un OP.
    modes: HashMap<String, HashMap<String, String>>,
}

pub struct Store {
    path: PathBuf,
    data: Data,
    dirty: bool,
}

impl Store {
    pub fn open(path: PathBuf) -> Self {
        let data = std::fs::read(&path)
            .ok()
            .and_then(|b| postcard::from_bytes(&b).ok())
            .unwrap_or_default();
        Self {
            path,
            data,
            dirty: false,
        }
    }

    /// Au plus une écriture par entretien.
    pub fn save_if_dirty(&mut self) {
        if std::mem::take(&mut self.dirty) {
            if let Ok(b) = postcard::to_allocvec(&self.data) {
                let _ = std::fs::write(&self.path, b);
            }
        }
    }

    /// Un OP a donné ce mode à ce joueur sur ce serveur.
    pub fn set_mode(&mut self, server: &str, pseudo: &str, mode: &str) {
        self.data
            .modes
            .entry(server.to_string())
            .or_default()
            .insert(pseudo.to_ascii_lowercase(), mode.to_ascii_lowercase());
        self.dirty = true;
    }

    /// Mode donné par un OP à ce joueur, s'il y en a un.
    pub fn mode(&self, server: &str, pseudo: &str) -> Option<&str> {
        self.data
            .modes
            .get(server)?
            .get(&pseudo.to_ascii_lowercase())
            .map(String::as_str)
    }

    fn bag(&mut self, server: &str, pseudo: &str) -> &mut Bag {
        self.dirty = true;
        self.data
            .bags
            .entry(server.to_string())
            .or_default()
            .entry(pseudo.to_ascii_lowercase())
            .or_default()
    }

    /// Sac d'un joueur (essais, diagnostics).
    pub fn get(&self, server: &str, pseudo: &str) -> Bag {
        self.data
            .bags
            .get(server)
            .and_then(|s| s.get(&pseudo.to_ascii_lowercase()))
            .cloned()
            .unwrap_or_default()
    }

    /// Le joueur pourrait-il lâcher ou ranger ces objets (sans rien retirer) ?
    pub fn can_spend(&self, server: &str, pseudo: &str, item: u32, count: u32) -> bool {
        self.get(server, pseudo).spend(item, count)
    }

    /// Objets gagnés par un échange que le relais vient d'accepter.
    pub fn gain(&mut self, server: &str, pseudo: &str, item: u32, count: u32) {
        if count > 0 {
            self.bag(server, pseudo).gain(item, count);
        }
    }

    /// Le joueur veut lâcher ou ranger ces objets : faux s'il ne peut pas les avoir.
    pub fn spend(&mut self, server: &str, pseudo: &str, item: u32, count: u32) -> bool {
        self.bag(server, pseudo).spend(item, count)
    }

    /// Un bloc changé par ce joueur vient d'être publié : cassé (`block` 0), un crédit de
    /// minage ; posé, il sort du sac (ou d'un crédit ; sans l'un ni l'autre on ne peut plus
    /// le refuser ici, son garant l'a déjà validé).
    pub fn block_set(&mut self, server: &str, pseudo: &str, block: u16) {
        let bag = self.bag(server, pseudo);
        if block == 0 {
            bag.mined = (bag.mined + 1).min(MINED_MAX);
        } else {
            bag.spend(block as u32, 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_comes_out_of_an_empty_bag() {
        let dir = std::env::temp_dir().join(format!("minelib-sacs-{}", rand::random::<u32>()));
        let mut s = Store::open(dir.clone());
        // Rien dans le sac, rien de miné : lâcher est refusé.
        assert!(!s.spend("srv", "Eve", 5, 1));
        // Ramassé puis lâché : d'accord, une fois.
        s.gain("srv", "eve", 5, 3);
        assert!(s.spend("srv", "EVE", 5, 2));
        assert!(!s.spend("srv", "Eve", 5, 2), "il n'en reste qu'un");
        assert_eq!(s.get("srv", "Eve").items.get(&5), Some(&1));
        // Trois blocs cassés : trois objets inconnus du relais au plus.
        for _ in 0..3 {
            s.block_set("srv", "Eve", 0);
        }
        assert!(s.spend("srv", "Eve", 77, 2));
        assert!(!s.spend("srv", "Eve", 77, 2), "un seul crédit restant");
        // Le dernier du sac et le dernier crédit ensemble.
        assert!(s.spend("srv", "Eve", 5, 2));
        assert_eq!(s.get("srv", "Eve"), Bag::default());
        // Poser un bloc le sort du sac ; un autre serveur a ses propres sacs.
        s.gain("srv", "Eve", 9, 1);
        s.block_set("srv", "Eve", 9);
        assert!(!s.spend("srv", "Eve", 9, 1));
        assert!(!s.spend("autre", "Eve", 5, 1));
        // Gardé sur le disque, avec le mode donné par un OP.
        s.gain("srv", "Eve", 4, 7);
        s.set_mode("srv", "Eve", "Survival");
        s.save_if_dirty();
        let again = Store::open(dir.clone());
        assert_eq!(again.get("srv", "eve").items.get(&4), Some(&7));
        assert_eq!(again.mode("srv", "EVE"), Some("survival"));
        let _ = std::fs::remove_file(dir);
    }
}
