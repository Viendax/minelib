//! Serveurs de construction : leurs OP et leurs bannis. Le créateur du serveur est OP
//! d'office (on ne peut ni le bannir ni lui retirer l'OP) ; il nomme les autres OP. Les
//! OP bannissent et débannissent. Gardé par le relais (pas par l'hôte du moment : le
//! serveur peut changer de propriétaire en cours de route), par créateur et par monde.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Admin {
    /// OP nommés (sans le créateur), en minuscules.
    pub ops: Vec<String>,
    /// Bannis : (pseudo en minuscules, raison).
    pub bans: Vec<(String, String)>,
}

/// Liste blanche d'un serveur : quand elle est active, seuls le créateur, ses OP et les
/// pseudos de la liste entrent. Gardée à part (fichier `….whitelist`) : l'ancien fichier
/// des OP reste lisible.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Whitelist {
    pub on: bool,
    /// Pseudos en minuscules.
    pub names: Vec<String>,
}

pub struct Store {
    path: PathBuf,
    servers: HashMap<String, Admin>,
    whitelists: HashMap<String, Whitelist>,
}

/// Clé d'un serveur : son créateur et son monde.
pub fn key(creator: &str, world: &str) -> String {
    format!("{}/{}", creator.to_ascii_lowercase(), world)
}

/// Action d'un OP.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Action {
    Ban,
    Unban,
    Op,
    Deop,
    List,
    /// Tuer un joueur (OP) : rien à garder, juste la permission.
    Kill,
    /// Liste blanche (OP).
    WhitelistOn,
    WhitelistOff,
    WhitelistAdd,
    WhitelistRemove,
    WhitelistList,
}

impl Store {
    pub fn open(path: PathBuf) -> Self {
        let servers = std::fs::read(&path)
            .ok()
            .and_then(|b| postcard::from_bytes(&b).ok())
            .unwrap_or_default();
        let whitelists = std::fs::read(Self::wl_path(&path))
            .ok()
            .and_then(|b| postcard::from_bytes(&b).ok())
            .unwrap_or_default();
        Self {
            path,
            servers,
            whitelists,
        }
    }

    fn wl_path(path: &std::path::Path) -> PathBuf {
        path.with_extension("whitelist")
    }

    fn save(&self) {
        if let Ok(b) = postcard::to_allocvec(&self.servers) {
            let _ = crate::accounts::write_atomic(&self.path, &b);
        }
        if let Ok(b) = postcard::to_allocvec(&self.whitelists) {
            let _ = crate::accounts::write_atomic(&Self::wl_path(&self.path), &b);
        }
    }

    /// Liste blanche active et `who` n'y est pas (ni créateur, ni OP) : entrée refusée.
    pub fn whitelisted_out(&self, server: &str, creator: &str, who: &str) -> bool {
        self.whitelists.get(server).is_some_and(|w| {
            w.on && !self.is_op(server, creator, who)
                && !w.names.iter().any(|n| n.eq_ignore_ascii_case(who))
        })
    }

    pub fn is_op(&self, server: &str, creator: &str, who: &str) -> bool {
        who.eq_ignore_ascii_case(creator)
            || self
                .servers
                .get(server)
                .is_some_and(|a| a.ops.iter().any(|o| o.eq_ignore_ascii_case(who)))
    }

    pub fn banned(&self, server: &str, who: &str) -> Option<String> {
        self.servers.get(server).and_then(|a| {
            a.bans
                .iter()
                .find(|b| b.0.eq_ignore_ascii_case(who))
                .map(|b| b.1.clone())
        })
    }

    /// Pseudos bannis de ce serveur.
    pub fn bans(&self, server: &str) -> Vec<String> {
        self.servers
            .get(server)
            .map(|a| a.bans.iter().map(|b| b.0.clone()).collect())
            .unwrap_or_default()
    }

    /// `by` fait `action` sur `target` dans le serveur `server` de `creator`. Renvoie le
    /// message à lui afficher, ou pourquoi c'est refusé.
    pub fn act(
        &mut self,
        server: &str,
        creator: &str,
        by: &str,
        action: Action,
        target: &str,
        reason: &str,
    ) -> Result<String, String> {
        if !self.is_op(server, creator, by) {
            return Err("Réservé aux OP de ce serveur.".into());
        }
        let low = target.to_ascii_lowercase();
        let is_creator = target.eq_ignore_ascii_case(creator);
        let wl = self.whitelists.entry(server.to_string()).or_default();
        let msg = match action {
            Action::WhitelistList => {
                return Ok(format!(
                    "Liste blanche {} : {}.",
                    if wl.on { "active" } else { "inactive" },
                    if wl.names.is_empty() {
                        "personne".into()
                    } else {
                        wl.names.join(", ")
                    }
                ));
            }
            Action::WhitelistOn => {
                wl.on = true;
                Some(
                    "Liste blanche active : seuls les OP et les pseudos de la liste entrent."
                        .to_string(),
                )
            }
            Action::WhitelistOff => {
                wl.on = false;
                Some("Liste blanche désactivée : tout le monde peut entrer.".to_string())
            }
            Action::WhitelistAdd | Action::WhitelistRemove if target.is_empty() => {
                return Err("Pseudo manquant.".into())
            }
            Action::WhitelistAdd => {
                if !wl.names.contains(&low) {
                    wl.names.push(low.clone());
                }
                Some(format!("{target} est sur la liste blanche."))
            }
            Action::WhitelistRemove => {
                let before = wl.names.len();
                wl.names.retain(|n| *n != low);
                if wl.names.len() == before {
                    return Err(format!("{target} n'est pas sur la liste blanche."));
                }
                Some(format!("{target} n'est plus sur la liste blanche."))
            }
            _ => None,
        };
        if let Some(msg) = msg {
            self.save();
            return Ok(msg);
        }
        let a = self.servers.entry(server.to_string()).or_default();
        let msg = match action {
            Action::List => {
                let mut ops = vec![format!("{creator} (créateur)")];
                ops.extend(a.ops.iter().cloned());
                let bans: Vec<String> = a.bans.iter().map(|b| b.0.clone()).collect();
                return Ok(format!(
                    "OP : {}. Bannis : {}.",
                    ops.join(", "),
                    if bans.is_empty() {
                        "aucun".into()
                    } else {
                        bans.join(", ")
                    }
                ));
            }
            _ if target.is_empty() => return Err("Pseudo manquant.".into()),
            Action::Kill => return Ok(format!("{target} est tué.")),
            Action::WhitelistOn
            | Action::WhitelistOff
            | Action::WhitelistAdd
            | Action::WhitelistRemove
            | Action::WhitelistList => unreachable!("traité plus haut"),
            Action::Ban if is_creator => return Err("On ne bannit pas le créateur.".into()),
            Action::Ban if by.eq_ignore_ascii_case(target) => {
                return Err("On ne se bannit pas soi-même.".into())
            }
            Action::Ban => {
                // Un OP banni perd l'OP.
                a.ops.retain(|o| *o != low);
                a.bans.retain(|b| b.0 != low);
                a.bans.push((low, reason.chars().take(120).collect()));
                format!("{target} est banni de ce serveur.")
            }
            Action::Unban => {
                let before = a.bans.len();
                a.bans.retain(|b| b.0 != low);
                if a.bans.len() == before {
                    return Err(format!("{target} n'est pas banni."));
                }
                format!("{target} n'est plus banni.")
            }
            Action::Op | Action::Deop if !by.eq_ignore_ascii_case(creator) => {
                return Err("Seul le créateur du serveur nomme les OP.".into())
            }
            Action::Op if is_creator => return Err("Le créateur est déjà OP.".into()),
            Action::Op => {
                if !a.ops.contains(&low) {
                    a.ops.push(low);
                }
                a.bans.retain(|b| !b.0.eq_ignore_ascii_case(target));
                format!("{target} est OP de ce serveur.")
            }
            Action::Deop if is_creator => {
                return Err("Le créateur reste OP.".into());
            }
            Action::Deop => {
                a.ops.retain(|o| *o != low);
                format!("{target} n'est plus OP.")
            }
        };
        self.save();
        Ok(msg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whitelist_lets_in_only_ops_and_listed_players() {
        let path = std::env::temp_dir().join(format!("minelib-wl-{}", rand::random::<u32>()));
        let mut s = Store::open(path.clone());
        let k = key("Createur", "monde");
        let act = |s: &mut Store, by: &str, a: Action, t: &str| s.act(&k, "Createur", by, a, t, "");
        assert!(
            !s.whitelisted_out(&k, "Createur", "Eve"),
            "inactive : ouvert"
        );
        assert!(
            act(&mut s, "Eve", Action::WhitelistOn, "").is_err(),
            "pas OP"
        );
        act(&mut s, "Createur", Action::Op, "Bob").unwrap();
        act(&mut s, "Bob", Action::WhitelistOn, "").unwrap();
        assert!(s.whitelisted_out(&k, "Createur", "Eve"));
        assert!(!s.whitelisted_out(&k, "Createur", "bob"), "OP");
        assert!(!s.whitelisted_out(&k, "Createur", "createur"), "créateur");
        act(&mut s, "Bob", Action::WhitelistAdd, "Eve").unwrap();
        assert!(!s.whitelisted_out(&k, "Createur", "EVE"));
        assert!(act(&mut s, "Bob", Action::WhitelistList, "")
            .unwrap()
            .contains("eve"));
        // Gardé sur disque, et l'ancien fichier des OP reste lisible.
        let mut s2 = Store::open(path.clone());
        assert!(!s2.whitelisted_out(&k, "Createur", "eve"));
        assert!(s2.whitelisted_out(&k, "Createur", "Zoe"));
        assert!(s2.is_op(&k, "Createur", "bob"));
        act(&mut s2, "Bob", Action::WhitelistRemove, "eve").unwrap();
        assert!(s2.whitelisted_out(&k, "Createur", "eve"));
        act(&mut s2, "Bob", Action::WhitelistOff, "").unwrap();
        assert!(!s2.whitelisted_out(&k, "Createur", "eve"));
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("whitelist"));
    }

    #[test]
    fn the_creator_is_op_and_names_the_others() {
        let path = std::env::temp_dir().join(format!("minelib-ops-{}", rand::random::<u32>()));
        let mut s = Store::open(path.clone());
        let k = key("Createur", "monde");
        let act = |s: &mut Store, by: &str, a: Action, t: &str| s.act(&k, "Createur", by, a, t, "");
        assert!(
            act(&mut s, "Bob", Action::Ban, "Eve").is_err(),
            "Bob n'est pas OP"
        );
        assert!(act(&mut s, "createur", Action::Op, "Bob").is_ok());
        assert!(
            act(&mut s, "Bob", Action::Op, "Eve").is_err(),
            "seul le créateur nomme"
        );
        assert!(act(&mut s, "Bob", Action::Ban, "Eve").is_ok());
        assert!(s.banned(&k, "EVE").is_some());
        assert!(act(&mut s, "Bob", Action::Ban, "Createur").is_err());
        assert!(act(&mut s, "Createur", Action::Deop, "Createur").is_err());
        assert!(act(&mut s, "Bob", Action::Unban, "eve").is_ok());
        assert!(s.banned(&k, "Eve").is_none());
        act(&mut s, "Bob", Action::Ban, "Eve").unwrap();
        // Gardé sur disque.
        let s2 = Store::open(path.clone());
        assert!(s2.banned(&k, "eve").is_some());
        assert!(s2.is_op(&k, "Createur", "BOB"));
        assert!(s2.banned(&key("Createur", "autre"), "eve").is_none());
        let _ = std::fs::remove_file(path);
    }
}
