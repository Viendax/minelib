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
    /// Mode de jeu par défaut de chaque serveur (fichier `….defaults`).
    defaults: HashMap<String, String>,
    /// Dernière position de chaque joueur sur chaque serveur (fichier `….positions`) :
    /// serveur → pseudo (minuscules) → [x, y, z, lacet, tangage]. Le relais en est le
    /// gardien : on revient là où on était, quoi qu'on ait fait ailleurs entre-temps.
    positions: HashMap<String, HashMap<String, [f32; 5]>>,
    /// Positions changées depuis le dernier enregistrement.
    positions_dirty: bool,
    /// Heure de chaque serveur (fichier `….times`) : (heure à l'instant `since` en
    /// secondes Unix, arrêtée). Elle avance comme dans Minecraft (24 h en 20 minutes).
    times: HashMap<String, (f32, u64, bool)>,
}

/// Heure d'un monde neuf, et durée d'une journée (s).
pub const START_HOUR: f32 = 8.0;
pub const DAY_S: f32 = 1200.0;

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
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
    /// Changer de mode de jeu (OP) : `target` = qui, `reason` = le mode.
    Gamemode,
    /// Mode de jeu par défaut à l'arrivée (OP) : `reason` = le mode (vide : aucun).
    DefaultGamemode,
    /// Renommer le serveur (OP) : `reason` = le nom.
    Rename,
    /// Heure (OP) : `reason` = l'heure (0..24) ; l'arrêter ou la relancer (`reason` 1/0).
    TimeSet,
    TimeFix,
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
        let defaults = std::fs::read(path.with_extension("defaults"))
            .ok()
            .and_then(|b| postcard::from_bytes(&b).ok())
            .unwrap_or_default();
        let positions = std::fs::read(path.with_extension("positions"))
            .ok()
            .and_then(|b| postcard::from_bytes(&b).ok())
            .unwrap_or_default();
        let times = std::fs::read(path.with_extension("times"))
            .ok()
            .and_then(|b| postcard::from_bytes(&b).ok())
            .unwrap_or_default();
        Self {
            path,
            servers,
            whitelists,
            defaults,
            positions,
            positions_dirty: false,
            times,
        }
    }

    /// Heure du serveur maintenant, et si elle est arrêtée.
    pub fn time(&self, server: &str) -> (f32, bool) {
        let (h, since, fixed) =
            self.times
                .get(server)
                .copied()
                .unwrap_or((START_HOUR, unix_now(), false));
        if fixed {
            return (h, true);
        }
        let dt = unix_now().saturating_sub(since) as f32;
        ((h + dt * 24.0 / DAY_S).rem_euclid(24.0), false)
    }

    fn set_time(&mut self, server: &str, h: f32, fixed: bool) {
        self.times
            .insert(server.to_string(), (h.rem_euclid(24.0), unix_now(), fixed));
        if let Ok(b) = postcard::to_allocvec(&self.times) {
            let _ = crate::accounts::write_atomic(&self.path.with_extension("times"), &b);
        }
    }

    /// Dernière position connue de `pseudo` sur ce serveur.
    pub fn position(&self, server: &str, pseudo: &str) -> Option<[f32; 5]> {
        self.positions
            .get(server)?
            .get(&pseudo.to_ascii_lowercase())
            .copied()
    }

    /// Position actuelle de `pseudo` sur ce serveur (gardée en mémoire ; écrite par
    /// `save_positions`).
    pub fn set_position(&mut self, server: &str, pseudo: &str, p: [f32; 5]) {
        if p.iter().all(|v| v.is_finite()) {
            self.positions
                .entry(server.to_string())
                .or_default()
                .insert(pseudo.to_ascii_lowercase(), p);
            self.positions_dirty = true;
        }
    }

    /// Écrit les positions si elles ont changé.
    pub fn save_positions(&mut self) {
        if !std::mem::take(&mut self.positions_dirty) {
            return;
        }
        if let Ok(b) = postcard::to_allocvec(&self.positions) {
            let _ = crate::accounts::write_atomic(&self.path.with_extension("positions"), &b);
        }
    }

    /// Mode de jeu par défaut du serveur (donné aux joueurs qui ne sont pas OP à leur
    /// arrivée).
    pub fn default_gamemode(&self, server: &str) -> Option<String> {
        self.defaults.get(server).cloned()
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
        if let Ok(b) = postcard::to_allocvec(&self.defaults) {
            let _ = crate::accounts::write_atomic(&self.path.with_extension("defaults"), &b);
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
        let mode: String = reason.chars().take(16).collect();
        match action {
            Action::TimeSet => {
                let h: f32 = reason
                    .trim()
                    .parse()
                    .map_err(|_| "Heure illisible.".to_string())?;
                if !(0.0..24.0).contains(&h) {
                    return Err("Heure entre 0 et 24.".into());
                }
                let fixed = self.time(server).1;
                self.set_time(server, h, fixed);
                return Ok("Heure changée.".into());
            }
            Action::TimeFix => {
                let fixed = matches!(reason.trim(), "1" | "true" | "oui" | "on");
                let (h, _) = self.time(server);
                self.set_time(server, h, fixed);
                return Ok(if fixed {
                    "Heure arrêtée.".into()
                } else {
                    "Le temps passe de nouveau.".into()
                });
            }
            Action::Rename => {
                return Ok(format!("Serveur renommé : {}.", reason.trim()));
            }
            Action::Gamemode => {
                return Ok(if target.is_empty() || target.eq_ignore_ascii_case(by) {
                    format!("Mode de jeu : {mode}.")
                } else {
                    format!("{target} passe en mode {mode}.")
                })
            }
            Action::DefaultGamemode => {
                let msg = if mode.is_empty() {
                    self.defaults.remove(server);
                    "Plus de mode de jeu par défaut : chacun garde le sien.".to_string()
                } else {
                    self.defaults.insert(server.to_string(), mode.clone());
                    format!("Mode de jeu par défaut : {mode} (pour qui n'est pas OP, à l'arrivée).")
                };
                self.save();
                return Ok(msg);
            }
            _ => {}
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
            | Action::WhitelistList
            | Action::Gamemode
            | Action::Rename
            | Action::TimeSet
            | Action::TimeFix
            | Action::DefaultGamemode => unreachable!("traité plus haut"),
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
