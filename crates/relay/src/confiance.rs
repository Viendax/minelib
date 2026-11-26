//! (v345) Passeurs de confiance, par invitation.
//!
//! Pour une partie FPS, le relais ne confie la voie rapide qu'à des passeurs « de
//! confiance » (`trusted` : au moins 3 parties jouées ici, peu signalés). Un passeur sans
//! joueur, sur une machine toujours allumée, ne joue jamais : il ne l'était donc jamais, et ne
//! portait que les tunnels. Un administrateur du relais (`admins.txt`) peut maintenant
//! l'inviter : un secret, valable pour quelques passeurs pendant `PASSEUR_INVITE_DAYS` jours,
//! que l'hébergeur met dans sa commande d'installation. Le passeur le donne en se connectant,
//! et son compte devient de confiance pour de bon : rien à refaire chez son hébergeur
//! (mises à jour, redémarrages, nouvelle adresse), jusqu'à ce qu'un administrateur la lui
//! retire. Le relais ne garde que l'empreinte du secret.

use std::path::PathBuf;

use minelib_net::proto::{PASSEUR_INVITE_DAYS, PASSEUR_INVITE_MAX};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Invitations gardées au plus (les plus anciennes partent).
const INVITATIONS_MAX: usize = 32;

#[derive(Serialize, Deserialize, Default)]
struct Data {
    invitations: Vec<Invitation>,
    trusted: Vec<Trusted>,
}

#[derive(Serialize, Deserialize, Clone)]
struct Invitation {
    /// Empreinte du secret (SHA-256).
    hash: [u8; 32],
    /// Passeurs qu'elle peut encore faire entrer.
    left: u8,
    /// Fin de validité (secondes depuis 1970).
    expires: u64,
    /// L'administrateur qui l'a donnée.
    by: String,
}

/// Un passeur de confiance : son compte (en minuscules), qui l'a invité, depuis quand.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Trusted {
    pub pseudo: String,
    pub by: String,
    pub since: u64,
}

/// Ce que devient une invitation donnée par un passeur.
#[derive(Debug, PartialEq, Eq)]
pub enum Claim {
    /// Le passeur est maintenant de confiance.
    Trusted,
    /// Il l'était déjà (l'invitation n'est pas comptée).
    Already,
    /// Secret inconnu, ou invitation épuisée.
    Unknown,
    /// Invitation trop vieille.
    Expired,
}

pub struct Store {
    path: PathBuf,
    data: Data,
}

fn digest(secret: &[u8; 16]) -> [u8; 32] {
    Sha256::digest(secret).into()
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

    /// Une invitation pour `uses` passeurs (1 à `PASSEUR_INVITE_MAX`), donnée par `by` : son
    /// secret (le relais n'en garde que l'empreinte).
    pub fn invite(&mut self, by: &str, uses: u8, now: u64) -> [u8; 16] {
        self.data
            .invitations
            .retain(|v| v.left > 0 && v.expires > now);
        let secret: [u8; 16] = rand::random();
        self.data.invitations.push(Invitation {
            hash: digest(&secret),
            left: uses.clamp(1, PASSEUR_INVITE_MAX),
            expires: now + PASSEUR_INVITE_DAYS * 86_400,
            by: by.to_string(),
        });
        let extra = self.data.invitations.len().saturating_sub(INVITATIONS_MAX);
        self.data.invitations.drain(..extra);
        self.save();
        secret
    }

    /// Le passeur `pseudo` donne ce secret.
    pub fn claim(&mut self, secret: &[u8; 16], pseudo: &str, now: u64) -> Claim {
        if self.trusts(pseudo) {
            return Claim::Already;
        }
        let h = digest(secret);
        let Some(i) = self
            .data
            .invitations
            .iter()
            .position(|v| v.hash == h && v.left > 0)
        else {
            return Claim::Unknown;
        };
        if self.data.invitations[i].expires <= now {
            self.data.invitations.remove(i);
            self.save();
            return Claim::Expired;
        }
        let inv = &mut self.data.invitations[i];
        inv.left -= 1;
        let by = inv.by.clone();
        if inv.left == 0 {
            self.data.invitations.remove(i);
        }
        self.data.trusted.push(Trusted {
            pseudo: pseudo.to_ascii_lowercase(),
            by,
            since: now,
        });
        self.save();
        Claim::Trusted
    }

    /// Ce compte est-il un passeur de confiance (invité) ?
    pub fn trusts(&self, pseudo: &str) -> bool {
        self.data
            .trusted
            .iter()
            .any(|t| t.pseudo.eq_ignore_ascii_case(pseudo))
    }

    /// Retire sa confiance au passeur `pseudo` (vrai s'il l'avait).
    pub fn remove(&mut self, pseudo: &str) -> bool {
        let before = self.data.trusted.len();
        self.data
            .trusted
            .retain(|t| !t.pseudo.eq_ignore_ascii_case(pseudo));
        let removed = self.data.trusted.len() != before;
        if removed {
            self.save();
        }
        removed
    }

    pub fn list(&self) -> &[Trusted] {
        &self.data.trusted
    }

    /// Invitations encore valables, et les passeurs qu'elles peuvent encore faire entrer.
    pub fn open_invitations(&self, now: u64) -> (usize, usize) {
        let open: Vec<&Invitation> = self
            .data
            .invitations
            .iter()
            .filter(|v| v.left > 0 && v.expires > now)
            .collect();
        (open.len(), open.iter().map(|v| v.left as usize).sum())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Une invitation pour deux passeurs : deux entrent, le troisième non ; un passeur déjà
    /// de confiance ne compte pas ; trop vieille, elle ne sert plus ; la confiance reste
    /// après une relecture du fichier, jusqu'à ce qu'on la retire.
    #[test]
    fn an_invitation_lets_in_as_many_passeurs_as_it_says_for_a_week() {
        let dir = std::env::temp_dir().join(format!("minelib-confiance-{}", rand::random::<u32>()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("passeurs_confiance.bin");
        let now = 1_800_000_000;
        let mut s = Store::open(path.clone());
        let secret = s.invite("Viendax", 2, now);
        assert_eq!(s.open_invitations(now), (1, 2));
        assert_eq!(s.claim(&[0; 16], "PsInconnu", now), Claim::Unknown);
        assert_eq!(s.claim(&secret, "PsUn", now + 10), Claim::Trusted);
        assert_eq!(s.claim(&secret, "psun", now + 20), Claim::Already);
        assert_eq!(s.claim(&secret, "PsDeux", now + 30), Claim::Trusted);
        assert_eq!(
            s.claim(&secret, "PsTrois", now + 40),
            Claim::Unknown,
            "épuisée"
        );
        assert!(s.trusts("PSUN") && s.trusts("PsDeux") && !s.trusts("PsTrois"));
        assert_eq!(s.list()[0].by, "Viendax");
        // Une autre, laissée passer sa semaine.
        let late = s.invite("Viendax", 4, now);
        let week = PASSEUR_INVITE_DAYS * 86_400;
        assert_eq!(s.claim(&late, "PsTard", now + week), Claim::Expired);
        assert!(!s.trusts("PsTard"));
        assert_eq!(s.open_invitations(now + week), (0, 0));
        // Relu du fichier : les deux sont toujours de confiance.
        let mut back = Store::open(path);
        assert!(back.trusts("PsUn") && back.trusts("PsDeux"));
        assert!(back.remove("psdeux"));
        assert!(!back.remove("PsDeux"));
        assert!(!back.trusts("PsDeux") && back.trusts("PsUn"));
        // Bornes : au moins un passeur, au plus `PASSEUR_INVITE_MAX`.
        back.invite("Viendax", 0, now);
        back.invite("Viendax", 200, now);
        assert_eq!(
            back.open_invitations(now),
            (2, 1 + PASSEUR_INVITE_MAX as usize)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
