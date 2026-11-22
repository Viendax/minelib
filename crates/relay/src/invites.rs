//! (v242) Invitations dans un monde de construction, gardées sans limite de temps jusqu'à ce
//! que l'invité y réponde (Accepter, Refuser, Ignorer ; « pas de temps limite », Viendax) :
//! une par joueur qui invite (la plus récente), dans un fichier (elles restent quand le relais
//! redémarre). Un refus attend aussi celui qui avait invité, s'il n'était pas en ligne.
//! Avant (v232), une invitation pour un ami hors ligne attendait deux heures au plus, en
//! mémoire, et n'était plus gardée une fois donnée.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Une invitation : de qui, dans quel serveur de construction (`servers::key` : le même monde
/// rouvert garde sa clé), son nom, quand (secondes Unix), et si l'invité en a déjà été prévenu
/// (notification).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Invite {
    pub from: String,
    pub server: String,
    pub name: String,
    pub at: u64,
    pub seen: bool,
}

/// Refus à dire à celui qui avait invité : par qui, dans quel monde.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Declined {
    pub by: String,
    pub name: String,
}

#[derive(Serialize, Deserialize, Default)]
struct Saved {
    /// Par invité (pseudo en minuscules), la plus récente d'abord.
    invites: HashMap<String, Vec<Invite>>,
    /// Par joueur qui avait invité (en minuscules).
    declined: HashMap<String, Vec<Declined>>,
}

/// Refus gardés au plus pour un joueur pas en ligne (les plus anciens partent).
pub const DECLINED_MAX: usize = 20;

pub struct Store {
    path: PathBuf,
    data: Saved,
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

    /// Invitations de `to`, la plus récente d'abord.
    pub fn of(&self, to: &str) -> &[Invite] {
        self.data
            .invites
            .get(&to.to_ascii_lowercase())
            .map_or(&[], |v| v.as_slice())
    }

    /// Nouvelle invitation pour `to` (celle d'avant du même joueur est remplacée).
    pub fn add(&mut self, to: &str, inv: Invite) {
        let list = self.data.invites.entry(to.to_ascii_lowercase()).or_default();
        list.retain(|i| !i.from.eq_ignore_ascii_case(&inv.from));
        list.insert(0, inv);
        self.save();
    }

    /// `to` a été prévenu de l'invitation de `from`.
    pub fn mark_seen(&mut self, to: &str, from: &str) {
        let mut changed = false;
        if let Some(list) = self.data.invites.get_mut(&to.to_ascii_lowercase()) {
            for i in list.iter_mut().filter(|i| i.from.eq_ignore_ascii_case(from)) {
                changed |= !i.seen;
                i.seen = true;
            }
        }
        if changed {
            self.save();
        }
    }

    /// Les invitations de `to` dont il n'a pas encore été prévenu (elles le sont, dès lors).
    pub fn take_unseen(&mut self, to: &str) -> Vec<Invite> {
        let mut out = Vec::new();
        if let Some(list) = self.data.invites.get_mut(&to.to_ascii_lowercase()) {
            for i in list.iter_mut().filter(|i| !i.seen) {
                i.seen = true;
                out.push(i.clone());
            }
        }
        if !out.is_empty() {
            self.save();
        }
        out
    }

    /// Retire l'invitation de `from` pour `to` (répondue).
    pub fn remove(&mut self, to: &str, from: &str) -> Option<Invite> {
        let key = to.to_ascii_lowercase();
        let list = self.data.invites.get_mut(&key)?;
        let i = list.iter().position(|i| i.from.eq_ignore_ascii_case(from))?;
        let inv = list.remove(i);
        if list.is_empty() {
            self.data.invites.remove(&key);
        }
        self.save();
        Some(inv)
    }

    /// Ne garde, pour `to`, que les invitations qui passent `keep` (par exemple : de joueurs
    /// encore amis dans les deux sens).
    pub fn retain(&mut self, to: &str, keep: impl Fn(&Invite) -> bool) {
        let key = to.to_ascii_lowercase();
        let Some(list) = self.data.invites.get_mut(&key) else {
            return;
        };
        let before = list.len();
        list.retain(|i| keep(i));
        if list.len() != before {
            if list.is_empty() {
                self.data.invites.remove(&key);
            }
            self.save();
        }
    }

    /// Refus à dire plus tard à `inviter` (pas en ligne).
    pub fn note_declined(&mut self, inviter: &str, d: Declined) {
        let list = self
            .data
            .declined
            .entry(inviter.to_ascii_lowercase())
            .or_default();
        list.push(d);
        if list.len() > DECLINED_MAX {
            list.remove(0);
        }
        self.save();
    }

    /// Refus gardés pour `inviter` (dits maintenant : retirés).
    pub fn take_declined(&mut self, inviter: &str) -> Vec<Declined> {
        let out = self
            .data
            .declined
            .remove(&inviter.to_ascii_lowercase())
            .unwrap_or_default();
        if !out.is_empty() {
            self.save();
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inv(from: &str, at: u64) -> Invite {
        Invite {
            from: from.into(),
            server: "ana/monde".into(),
            name: "Monde".into(),
            at,
            seen: false,
        }
    }

    /// Une par joueur qui invite (la plus récente, en tête), sans limite de temps, gardées
    /// dans le fichier ; prévenu une seule fois ; retirée à la réponse ; refus gardés pour qui
    /// n'est pas en ligne.
    #[test]
    fn invitations_wait_for_an_answer_even_after_a_restart() {
        let dir = std::env::temp_dir().join(format!("minelib_invites_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("invitations.bin");
        let mut s = Store::open(path.clone());
        s.add("Bob", inv("Ana", 1));
        s.add("bob", inv("Cid", 2));
        s.add("BOB", inv("ana", 3));
        let from: Vec<(&str, u64)> = s.of("bob").iter().map(|i| (i.from.as_str(), i.at)).collect();
        assert_eq!(from, vec![("ana", 3), ("Cid", 2)]);
        // Prévenu une fois.
        s.mark_seen("bob", "Cid");
        assert_eq!(s.take_unseen("bob").len(), 1);
        assert!(s.take_unseen("bob").is_empty());
        // Le relais redémarre : tout est encore là.
        let mut s = Store::open(path.clone());
        assert_eq!(s.of("Bob").len(), 2);
        assert!(s.of("Bob").iter().all(|i| i.seen));
        // Répondue : retirée.
        assert_eq!(s.remove("bob", "ANA").map(|i| i.at), Some(3));
        assert!(s.remove("bob", "ana").is_none());
        s.retain("bob", |i| i.from != "Cid");
        assert!(s.of("bob").is_empty());
        // Refus pour Ana, hors ligne : gardé, dit une fois.
        for k in 0..DECLINED_MAX + 3 {
            s.note_declined(
                "Ana",
                Declined {
                    by: format!("J{k}"),
                    name: "Monde".into(),
                },
            );
        }
        let s = &mut Store::open(path);
        let d = s.take_declined("ana");
        assert_eq!(d.len(), DECLINED_MAX);
        assert_eq!(d[0].by, "J3", "les plus anciens partent");
        assert!(s.take_declined("ana").is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
