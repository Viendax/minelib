//! Gardiens d'un monde : ce qui l'empêche de se séparer en deux, même avec des tricheurs
//! parmi eux.
//!
//! Un monde de construction qui a un relais de secours a aussi des témoins (jusqu'à
//! `WITNESSES_MAX`) : des joueurs, même d'autres mondes, par leur connexion de garant, de
//! préférence sur d'autres machines. Le relais du monde, le relais de secours et les témoins
//! sont les gardiens du monde. Les règles :
//!
//! - **Seuls les témoins comptent.** Le relais de secours est celui qui gagne à une
//!   reprise : ni ses baux ni sa voix ne comptent (un relais de secours tricheur ne peut
//!   rien seul).
//! - **Le relais du monde ne change le monde qu'avec les baux d'un quorum de témoins**
//!   (`Committee::quorum`). Il leur envoie un battement toutes les `BEAT_MS` ; chacun de
//!   ceux qui l'entendent répond par un bail qui rappelle l'heure du battement. Un bail vaut
//!   `LEASE_MS` à compter de l'envoi du battement, sur l'horloge du relais du monde. Sans
//!   assez de baux, le monde est en pause : rien n'y change (`Warden::serving`).
//! - **Un témoin qui n'entend plus le relais du monde depuis `VOTE_MS` vote pour la
//!   reprise**, pour la liste qu'il a alors, et ne lui donnera plus jamais de bail
//!   (`Keeper::tick`). Comme `VOTE_MS` dépasse `LEASE_MS`, son dernier bail a expiré quand
//!   il vote.
//! - **Le relais de secours ne reprend le monde qu'avec les votes signés d'un quorum de
//!   témoins d'une même liste** (`Proof`, que chaque joueur vérifie avant de le suivre).
//!   Avec `w` témoins, dont au plus `f = (w - 1) / 3` menteurs (qui donnent leurs baux et
//!   votent quand même), le quorum est `q = ⌈(w + f + 1) / 2⌉` : un quorum de baux et un
//!   quorum de votes ont toujours au moins `f + 1` témoins en commun, donc au moins un
//!   honnête, qui n'a pas pu faire les deux. Le relais du monde et le relais de secours ne
//!   servent jamais le monde en même temps.
//! - **La liste des gardiens est numérotée et signée par le relais du monde**
//!   (`Committee`). Un seul gardien change à la fois, et une ancienne liste compte tant que
//!   trop peu de ses témoins ont appris la suivante pour qu'une reprise y soit impossible
//!   (en comptant que les menteurs peuvent faire semblant de l'avoir apprise).
//! - **Arrêt propre, ou pause sans issue** : le relais du monde se met en pause pour
//!   toujours et le dit (arrêt signé) au relais de secours, qui reprend aussitôt
//!   (`Warden::release`).
//!
//! Le prix, inévitable (théorème CAP) : un relais du monde coupé d'assez de témoins, mais
//! encore joint par des joueurs, met le monde en pause pour eux le temps que le relais de
//! secours reprenne. Continuer seul, ce serait la séparation. Et avec plus de `f` témoins
//! menteurs dans une même liste, rien ne tient plus : il en faut 2 sur 4, 3 sur 7.

use std::collections::HashMap;

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};

/// Un battement du relais du monde à ses gardiens toutes les `BEAT_MS` millisecondes
/// (quatre par bail : un battement perdu ne met pas le monde en pause).
pub const BEAT_MS: u64 = 250;
/// Durée d'un bail, à compter de l'envoi du battement qu'il rappelle. C'est le temps
/// qu'il faut pour être sûr qu'un relais du monde qui se tait ne sert plus le monde : une
/// coupure plus longue chez lui fait reprendre le monde par le relais de secours.
pub const LEASE_MS: u64 = 1000;
/// Un témoin qui n'entend plus le relais du monde depuis ce temps vote pour la reprise.
/// La marge sur `LEASE_MS` couvre l'écart de marche des horloges (très largement).
pub const VOTE_MS: u64 = 1250;
/// Un gardien sans bail depuis ce temps est remplacé (parti, coupé, ou il a voté).
pub const REPLACE_MS: u64 = 2000;
/// Un relais du monde en pause depuis ce temps, sans issue (il n'entend plus assez de
/// témoins), confie le monde à son relais de secours s'il l'entend encore.
pub const RELEASE_MS: u64 = 2000;
/// Le relais du monde dit aux joueurs où en est la pause au moins aussi souvent : un jeu
/// qui n'en entend plus parler depuis `SILENT_MS` cherche le relais de secours.
pub const HEARTBEAT_MS: u64 = 500;
pub const SILENT_MS: u64 = 1500;
/// Un jeu qui cherche le relais de secours réessaie à ce rythme (et un témoin lui dit où
/// il en est à ce rythme).
pub const PROBE_MS: u64 = 250;
/// Témoins d'un monde, au plus.
pub const WITNESSES_MAX: usize = 7;

/// Témoins voulus quand `available` garants peuvent l'être : 7 (deux menteurs tolérés,
/// deux témoins perdus sans pause), 6, 4 (un menteur), 3, sinon un seul.
pub fn witness_target(available: usize) -> usize {
    match available {
        7.. => 7,
        6 => 6,
        4..=5 => 4,
        3 => 3,
        1..=2 => 1,
        0 => 0,
    }
}

/// Heure des gardiens de ce programme : millisecondes depuis son premier appel, sur une
/// horloge qui ne recule pas.
pub fn clock() -> u64 {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    START
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_millis() as u64
}

/// Liste des gardiens d'un monde (version `version`), signée par son relais du monde.
/// Le relais du monde en fait partie sans y être nommé.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Committee {
    pub room: u64,
    pub version: u64,
    /// Pseudo du joueur qui tient le relais de secours (aucun : pas de reprise possible).
    pub standby: Option<String>,
    pub witnesses: Vec<String>,
    pub sig: Vec<u8>,
}

fn committee_bytes(room: u64, version: u64, standby: &Option<String>, w: &[String]) -> Vec<u8> {
    let mut m = b"minelib/gardiens/v2".to_vec();
    m.extend(room.to_le_bytes());
    m.extend(version.to_le_bytes());
    match standby {
        Some(s) => {
            m.push(1);
            m.extend(s.to_ascii_lowercase().as_bytes());
            m.push(0);
        }
        None => m.push(0),
    }
    m.extend((w.len() as u32).to_le_bytes());
    for p in w {
        m.extend(p.to_ascii_lowercase().as_bytes());
        m.push(0);
    }
    m
}

impl Committee {
    pub fn sign(
        root: &SigningKey,
        room: u64,
        version: u64,
        standby: Option<String>,
        witnesses: Vec<String>,
    ) -> Self {
        let sig = root
            .sign(&committee_bytes(room, version, &standby, &witnesses))
            .to_bytes()
            .to_vec();
        Self {
            room,
            version,
            standby,
            witnesses,
            sig,
        }
    }

    /// Signée par ce relais du monde ?
    pub fn valid(&self, root: &VerifyingKey) -> bool {
        let Ok(b) = <[u8; 64]>::try_from(self.sig.as_slice()) else {
            return false;
        };
        root.verify(
            &committee_bytes(self.room, self.version, &self.standby, &self.witnesses),
            &Signature::from_bytes(&b),
        )
        .is_ok()
    }

    /// Témoins menteurs tolérés : un sur trois (moins un).
    pub fn liars(&self) -> usize {
        self.witnesses.len().saturating_sub(1) / 3
    }

    /// Témoins qu'il faut, pour servir le monde (baux) comme pour le reprendre (votes) :
    /// deux tels groupes ont toujours au moins `liars() + 1` témoins en commun.
    pub fn quorum(&self) -> usize {
        (self.witnesses.len() + self.liars() + 2) / 2
    }

    /// Une reprise est-elle possible avec cette liste (un relais de secours, et au moins
    /// un témoin) ?
    pub fn can_take_over(&self) -> bool {
        self.standby.is_some() && !self.witnesses.is_empty()
    }

    pub fn is_standby(&self, pseudo: &str) -> bool {
        self.standby
            .as_deref()
            .is_some_and(|s| s.eq_ignore_ascii_case(pseudo))
    }

    pub fn is_witness(&self, pseudo: &str) -> bool {
        self.witnesses
            .iter()
            .any(|w| w.eq_ignore_ascii_case(pseudo))
    }

    /// Gardien nommé dans la liste (relais de secours ou témoin).
    pub fn has(&self, pseudo: &str) -> bool {
        self.is_standby(pseudo) || self.is_witness(pseudo)
    }

    /// Gardiens nommés (en minuscules).
    pub fn members(&self) -> impl Iterator<Item = String> + '_ {
        self.standby
            .iter()
            .chain(self.witnesses.iter())
            .map(|p| p.to_ascii_lowercase())
    }
}

/// Un gardien (témoin, ou relais de secours), de son côté : il répond aux battements tant
/// qu'il entend le relais du monde, et vote, pour toujours, quand il ne l'entend plus.
/// Les heures sont en millisecondes sur une horloge qui ne recule pas.
pub struct Keeper {
    pub room: u64,
    /// Son pseudo (en minuscules).
    pub me: String,
    root: VerifyingKey,
    /// Dernière liste connue (la plus récente vue, du relais du monde ou d'un autre
    /// gardien).
    pub committee: Option<Committee>,
    /// Dernier battement entendu (ou moment où il s'est su gardien, s'il n'en a pas encore
    /// entendu).
    heard: Option<u64>,
    /// La liste qu'il avait en votant : son vote ne compte que pour elle.
    vote: Option<Committee>,
}

impl Keeper {
    pub fn new(room: u64, me: &str, root: [u8; 32]) -> Option<Self> {
        Some(Self {
            room,
            me: me.to_ascii_lowercase(),
            root: VerifyingKey::from_bytes(&root).ok()?,
            committee: None,
            heard: None,
            vote: None,
        })
    }

    /// Battement du relais du monde : la liste est apprise ; renvoie la version à rappeler
    /// dans le bail (aucune : liste fausse, ou il a voté et ne soutient plus ce relais).
    pub fn beat(&mut self, c: &Committee, now: u64) -> Option<u64> {
        if c.room != self.room || !c.valid(&self.root) {
            return None;
        }
        self.learn(c, now);
        if self.vote.is_some() {
            return None;
        }
        self.heard = Some(now);
        self.committee.as_ref().map(|c| c.version)
    }

    /// Liste plus récente (d'un battement, ou d'un autre gardien) : gardée. Vrai si elle
    /// est nouvelle.
    pub fn learn(&mut self, c: &Committee, now: u64) -> bool {
        if c.room != self.room
            || self
                .committee
                .as_ref()
                .is_some_and(|k| k.version >= c.version)
            || !c.valid(&self.root)
        {
            return false;
        }
        self.committee = Some(c.clone());
        // Il se sait gardien à partir de maintenant : il ne peut pas voter avant de s'être
        // tu `VOTE_MS` (le relais du monde compte là-dessus).
        if self.heard.is_none() && c.has(&self.me) {
            self.heard = Some(now);
        }
        true
    }

    /// Témoin dans la dernière liste connue.
    pub fn member(&self) -> bool {
        self.committee
            .as_ref()
            .is_some_and(|c| c.is_witness(&self.me))
    }

    /// À appeler régulièrement : un témoin vote quand le relais du monde se tait depuis
    /// `VOTE_MS`. Vrai au moment du vote.
    pub fn tick(&mut self, now: u64) -> bool {
        if self.vote.is_some() || !self.member() {
            return false;
        }
        if self.heard.is_some_and(|h| now.saturating_sub(h) >= VOTE_MS) {
            self.vote = self.committee.clone();
            return true;
        }
        false
    }

    /// Version de la liste pour laquelle il a voté.
    pub fn voted(&self) -> Option<u64> {
        self.vote.as_ref().map(|c| c.version)
    }

    /// La liste pour laquelle il a voté.
    pub fn vote_committee(&self) -> Option<&Committee> {
        self.vote.as_ref()
    }

    /// Clé des listes de ce relais du monde.
    pub fn root(&self) -> &VerifyingKey {
        &self.root
    }

    /// Depuis combien de temps il n'entend plus le relais du monde (aucun : jamais
    /// entendu, ni su gardien).
    pub fn silent_for(&self, now: u64) -> Option<u64> {
        self.heard.map(|h| now.saturating_sub(h))
    }
}

/// Vote d'un témoin pour la reprise du monde `room`, pour la liste `version`, signé avec
/// la clé de son compte (certifiée par le relais du monde) : le relais de secours le
/// montre aux joueurs, qui n'ont pas à le croire sur parole.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Vote {
    pub version: u64,
    pub sig: Vec<u8>,
}

fn vote_bytes(room: u64, version: u64, pseudo: &str) -> Vec<u8> {
    let mut m = b"minelib/vote/v1".to_vec();
    m.extend(room.to_le_bytes());
    m.extend(version.to_le_bytes());
    m.extend(pseudo.to_ascii_lowercase().as_bytes());
    m
}

impl Vote {
    pub fn sign(key: &SigningKey, room: u64, version: u64, pseudo: &str) -> Self {
        Self {
            version,
            sig: key
                .sign(&vote_bytes(room, version, pseudo))
                .to_bytes()
                .to_vec(),
        }
    }

    pub fn valid(&self, key: &[u8; 32], room: u64, pseudo: &str) -> bool {
        let (Ok(k), Ok(b)) = (
            VerifyingKey::from_bytes(key),
            <[u8; 64]>::try_from(self.sig.as_slice()),
        ) else {
            return false;
        };
        k.verify(
            &vote_bytes(room, self.version, pseudo),
            &Signature::from_bytes(&b),
        )
        .is_ok()
    }
}

fn release_bytes(room: u64) -> Vec<u8> {
    let mut m = b"minelib/arret/v1".to_vec();
    m.extend(room.to_le_bytes());
    m
}

/// Arrêt du relais du monde, signé par lui : il ne servira plus jamais ce monde.
pub fn sign_release(root: &SigningKey, room: u64) -> Vec<u8> {
    root.sign(&release_bytes(room)).to_bytes().to_vec()
}

pub fn release_valid(root: &VerifyingKey, room: u64, sig: &[u8]) -> bool {
    <[u8; 64]>::try_from(sig).is_ok_and(|b| {
        root.verify(&release_bytes(room), &Signature::from_bytes(&b))
            .is_ok()
    })
}

/// Ce qui prouve aux joueurs que le relais de secours a le droit de servir le monde :
/// l'arrêt signé par le relais du monde, ou la liste des gardiens (signée par lui) avec
/// les votes signés d'un quorum de ses témoins.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum Proof {
    Released {
        sig: Vec<u8>,
    },
    Votes {
        committee: Committee,
        votes: Vec<(crate::sovereign::Cert, Vote)>,
    },
}

impl Proof {
    /// La preuve tient-elle, pour le monde `room` de ce relais du monde (`root`) et le
    /// relais de secours `standby` (pseudo de son hôte, donné par le relais du monde) ?
    pub fn check(&self, root: &[u8; 32], room: u64, standby: &str) -> bool {
        let Ok(root) = VerifyingKey::from_bytes(root) else {
            return false;
        };
        match self {
            Proof::Released { sig } => release_valid(&root, room, sig),
            Proof::Votes { committee, votes } => {
                if committee.room != room
                    || !committee.valid(&root)
                    || !committee.is_standby(standby)
                    || !committee.can_take_over()
                {
                    return false;
                }
                let mut yes: Vec<String> = Vec::new();
                for (cert, vote) in votes {
                    let who = cert.pseudo.to_ascii_lowercase();
                    if committee.is_witness(&who)
                        && !yes.contains(&who)
                        && cert.valid(&root, room)
                        && vote.version == committee.version
                        && vote.valid(&cert.key, room, &who)
                    {
                        yes.push(who);
                    }
                }
                yes.len() >= committee.quorum()
            }
        }
    }
}

/// Le relais de secours `me` peut-il reprendre le monde avec la liste `c` : il y est
/// nommé, et un quorum de ses témoins a voté pour elle (`votes` : version de la liste du
/// vote de chaque témoin, par pseudo en minuscules) ?
pub fn enough_votes(c: &Committee, me: &str, votes: impl Fn(&str) -> Option<u64>) -> bool {
    if !c.is_standby(me) || !c.can_take_over() {
        return false;
    }
    let yes = c
        .witnesses
        .iter()
        .filter(|w| votes(&w.to_ascii_lowercase()) == Some(c.version))
        .count();
    yes >= c.quorum()
}

/// Ce que le relais du monde sait d'un gardien.
#[derive(Clone, Copy, Debug, Default)]
struct Seen {
    /// Heure de la première liste qui l'a nommé : il ne pouvait pas voter avant de la
    /// connaître puis de se taire `VOTE_MS`.
    first: u64,
    /// Heure du dernier battement rappelé par un bail.
    lease: Option<u64>,
    /// Plus haute version de liste qu'il a rappelée.
    version: u64,
    /// Parti proprement (il a promis de ne jamais voter).
    resigned: bool,
}

/// Le relais du monde, côté gardiens : listes, baux reçus, pause.
pub struct Warden {
    pub room: u64,
    pub current: Committee,
    /// Listes précédentes où une reprise reste possible.
    old: Vec<Committee>,
    seen: HashMap<String, Seen>,
    released: bool,
}

impl Warden {
    /// Première liste d'un monde (version 1).
    pub fn new(
        root: &SigningKey,
        room: u64,
        standby: Option<String>,
        witnesses: Vec<String>,
        now: u64,
    ) -> Self {
        let mut w = Self {
            room,
            current: Committee::sign(root, room, 1, standby, witnesses),
            old: Vec::new(),
            seen: HashMap::new(),
            released: false,
        };
        w.note_members(now);
        w
    }

    fn note_members(&mut self, now: u64) {
        let members: Vec<String> = self.current.members().collect();
        for m in members {
            self.seen.entry(m).or_insert(Seen {
                first: now,
                ..Default::default()
            });
        }
    }

    /// Gardiens à qui envoyer le battement : ceux de la liste en cours, et ceux des
    /// listes précédentes encore comptées (ils y apprennent la nouvelle).
    pub fn targets(&self) -> Vec<String> {
        let mut out: Vec<String> = self.current.members().collect();
        for c in &self.old {
            for m in c.members() {
                if !out.contains(&m) {
                    out.push(m);
                }
            }
        }
        out
    }

    /// Bail reçu de `who` : il a entendu le battement de l'heure `t` et connaît la liste
    /// `version`.
    pub fn lease(&mut self, who: &str, version: u64, t: u64, now: u64) {
        if t > now || version > self.current.version {
            return;
        }
        let Some(s) = self.seen.get_mut(&who.to_ascii_lowercase()) else {
            return;
        };
        s.lease = Some(s.lease.map_or(t, |l| l.max(t)));
        s.version = s.version.max(version);
        self.retire();
    }

    /// Départ propre de `who` (il a promis de ne jamais voter).
    pub fn resign(&mut self, who: &str) {
        if let Some(s) = self.seen.get_mut(&who.to_ascii_lowercase()) {
            s.resigned = true;
        }
        self.retire();
    }

    /// Une liste précédente ne compte plus quand une reprise y est devenue impossible :
    /// assez de ses témoins ont appris une liste plus récente (ou sont partis proprement)
    /// pour que ceux qui restent, plus les menteurs qui ont fait semblant, ne fassent pas
    /// un quorum.
    fn retire(&mut self) {
        let seen = &self.seen;
        self.old.retain(|c| {
            if !c.can_take_over() {
                return false;
            }
            let moved = c
                .witnesses
                .iter()
                .filter(|m| {
                    seen.get(&m.to_ascii_lowercase())
                        .is_some_and(|s| s.resigned || s.version > c.version)
                })
                .count();
            moved + c.quorum() <= c.witnesses.len() + c.liars()
        });
    }

    /// `who` couvre le relais du monde : un bail encore valable, ou trop récemment nommé
    /// pour avoir pu voter.
    pub fn fresh(&self, who: &str, now: u64) -> bool {
        self.seen.get(&who.to_ascii_lowercase()).is_some_and(|s| {
            !s.resigned && s.lease.unwrap_or(s.first).max(s.first) + LEASE_MS > now
        })
    }

    /// Sans bail depuis `REPLACE_MS` (ou parti proprement) : à remplacer.
    pub fn idle(&self, who: &str, now: u64) -> bool {
        self.seen.get(&who.to_ascii_lowercase()).is_none_or(|s| {
            s.resigned || now.saturating_sub(s.lease.unwrap_or(s.first).max(s.first)) > REPLACE_MS
        })
    }

    /// Un quorum de témoins de cette liste couvre le relais du monde (le relais de secours
    /// ne compte pas).
    fn quorum(&self, c: &Committee, now: u64) -> bool {
        !c.can_take_over()
            || c.witnesses.iter().filter(|w| self.fresh(w, now)).count() >= c.quorum()
    }

    /// Le relais du monde peut-il changer le monde maintenant ? Il lui faut un quorum de
    /// baux dans chaque liste où une reprise reste possible.
    pub fn serving(&self, now: u64) -> bool {
        !self.released
            && self.quorum(&self.current, now)
            && self.old.iter().all(|c| self.quorum(c, now))
    }

    /// Une liste précédente compte encore (changement de gardiens en cours).
    pub fn changing(&self) -> bool {
        !self.old.is_empty()
    }

    /// Nouvelle liste. Faux si rien ne change, si le changement précédent n'est pas fini,
    /// ou si elle ajoute ou retire plus d'un gardien : avec un seul gardien de différence,
    /// deux quorums de deux listes qui se suivent ont toujours un témoin honnête en commun
    /// (qui ne vote qu'une fois) ; deux relais de secours ne peuvent donc pas reprendre le
    /// monde chacun de son côté.
    pub fn reconfigure(
        &mut self,
        root: &SigningKey,
        standby: Option<String>,
        witnesses: Vec<String>,
        now: u64,
    ) -> bool {
        if self.changing() || self.released || witnesses.len() > WITNESSES_MAX {
            return false;
        }
        let next_members: Vec<String> = standby
            .iter()
            .chain(witnesses.iter())
            .map(|p| p.to_ascii_lowercase())
            .collect();
        let mut dedup = next_members.clone();
        dedup.sort();
        dedup.dedup();
        if dedup.len() != next_members.len() {
            return false;
        }
        let added = next_members.iter().filter(|m| !self.current.has(m)).count();
        let removed = self
            .current
            .members()
            .filter(|m| !next_members.contains(m))
            .count();
        let same_roles = self
            .current
            .standby
            .as_ref()
            .map(|s| s.to_ascii_lowercase())
            == standby.as_ref().map(|s| s.to_ascii_lowercase());
        if (added == 0 && removed == 0 && same_roles) || added > 1 || removed > 1 {
            return false;
        }
        let next = Committee::sign(
            root,
            self.room,
            self.current.version + 1,
            standby,
            witnesses,
        );
        let prev = std::mem::replace(&mut self.current, next);
        if prev.can_take_over() {
            self.old.push(prev);
        }
        self.note_members(now);
        self.retire();
        true
    }

    /// Le relais de secours qui pourrait reprendre tout de suite, sans vote : celui de la
    /// liste en cours, s'il est le seul à pouvoir reprendre (les listes encore comptées
    /// nomment le même).
    pub fn releasable(&self) -> Option<String> {
        let s = self.current.standby.clone()?;
        self.old
            .iter()
            .filter(|c| c.can_take_over())
            .all(|c| c.is_standby(&s))
            .then_some(s)
    }

    /// Arrêt (propre, ou pause sans issue) : le monde est en pause pour toujours ici.
    /// Renvoie le relais de secours qui peut reprendre aussitôt (voir `releasable` ;
    /// sinon, les témoins votent, comme pour une chute).
    pub fn release(&mut self) -> Option<String> {
        self.released = true;
        self.releasable()
    }

    pub fn released(&self) -> bool {
        self.released
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn key(n: u8) -> SigningKey {
        SigningKey::from_bytes(&[n; 32])
    }

    fn names(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("W{i}")).collect()
    }

    #[test]
    fn a_committee_is_signed_and_counted() {
        let root = key(1);
        let c = Committee::sign(&root, 7, 1, Some("S".into()), vec!["W".into()]);
        assert!(c.valid(&root.verifying_key()));
        assert!(!c.valid(&key(2).verifying_key()));
        let mut forged = c.clone();
        forged.witnesses.push("Moi".into());
        assert!(!forged.valid(&root.verifying_key()));
        assert!(c.can_take_over());
        // Sans témoin, pas de reprise possible (sauf arrêt signé).
        let alone = Committee::sign(&root, 7, 2, Some("S".into()), vec![]);
        assert!(!alone.can_take_over());
        // Témoins, menteurs tolérés, quorum : deux quorums se recoupent en au moins
        // `menteurs + 1` témoins.
        for (w, liars, quorum) in [
            (1, 0, 1),
            (2, 0, 2),
            (3, 0, 2),
            (4, 1, 3),
            (5, 1, 4),
            (6, 1, 4),
            (7, 2, 5),
        ] {
            let c = Committee::sign(&root, 7, 3, Some("S".into()), names(w));
            assert_eq!((c.liars(), c.quorum()), (liars, quorum), "{w} témoins");
            assert!(2 * c.quorum() > w + c.liars());
        }
        assert_eq!(
            [0, 1, 2, 3, 4, 5, 6, 7, 30].map(witness_target),
            [0, 1, 1, 3, 4, 4, 6, 7, 7]
        );
    }

    #[test]
    fn a_keeper_votes_after_silence_and_never_leases_again() {
        let root = key(1);
        let c = Committee::sign(&root, 7, 1, Some("S".into()), vec!["W".into()]);
        let mut w = Keeper::new(7, "w", root.verifying_key().to_bytes()).unwrap();
        assert_eq!(w.beat(&c, 0), Some(1));
        assert!(!w.tick(VOTE_MS - 1));
        assert_eq!(w.beat(&c, 1000), Some(1));
        assert!(!w.tick(1000 + VOTE_MS - 1));
        assert!(w.tick(1000 + VOTE_MS));
        assert_eq!(w.voted(), Some(1));
        // Il entend de nouveau le relais du monde : plus de bail, jamais.
        assert_eq!(w.beat(&c, 9000), None);
        // Une liste plus récente ne déplace pas son vote.
        let c2 = Committee::sign(&root, 7, 2, Some("S".into()), vec!["W".into()]);
        assert_eq!(w.beat(&c2, 9500), None);
        assert_eq!(w.voted(), Some(1));
        // Liste d'un autre relais : ignorée.
        let other = Committee::sign(&key(3), 7, 5, Some("S".into()), vec!["W".into()]);
        assert!(!w.learn(&other, 9600));
        // Le relais de secours ne vote pas (sa voix ne compterait pas).
        let mut s = Keeper::new(7, "s", root.verifying_key().to_bytes()).unwrap();
        s.beat(&c, 0);
        assert!(!s.tick(10 * VOTE_MS));
    }

    #[test]
    fn the_standby_needs_a_quorum_of_witnesses() {
        let root = key(1);
        let c = Committee::sign(&root, 7, 4, Some("S".into()), names(4));
        // Quatre témoins : il en faut trois (un menteur toléré).
        assert!(!enough_votes(&c, "S", |w| (w == "w0" || w == "w1").then_some(4)));
        assert!(enough_votes(&c, "s", |w| (w != "w3").then_some(4)));
        // Des votes pour une autre liste, ou pour un autre relais de secours : non.
        assert!(!enough_votes(&c, "S", |w| (w != "w3").then_some(3)));
        assert!(!enough_votes(&c, "T", |w| (w != "w3").then_some(4)));
    }

    #[test]
    fn the_world_relay_pauses_without_a_quorum_of_witnesses() {
        let root = key(1);
        let mut r = Warden::new(&root, 7, Some("S".into()), vec!["W".into()], 0);
        // Nommés à l'instant : ils ne peuvent pas avoir voté.
        assert!(r.serving(LEASE_MS - 1));
        assert!(!r.serving(LEASE_MS));
        // Le bail du relais de secours ne compte pas.
        r.lease("S", 1, 1500, 1600);
        assert!(!r.serving(1600));
        r.lease("W", 1, 1500, 1600);
        assert!(r.serving(1500 + LEASE_MS - 1));
        assert!(!r.serving(1500 + LEASE_MS));
        // Un bail qui rappelle un battement du futur (horloge truquée) : ignoré.
        r.lease("W", 1, 9000, 3000);
        assert!(!r.serving(3000));
        assert_eq!(r.release(), Some("S".into()));
        r.lease("W", 1, 3000, 3000);
        assert!(!r.serving(3000), "arrêté pour toujours");
    }

    #[test]
    fn an_old_list_counts_until_too_few_witnesses_remain_for_a_takeover() {
        let root = key(1);
        let mut r = Warden::new(&root, 7, Some("S".into()), names(4), 0);
        for w in names(4) {
            r.lease(&w, 1, 100, 100);
        }
        // W3 parti : remplacé par X.
        let mut next = names(3);
        next.push("X".into());
        assert!(r.reconfigure(&root, Some("S".into()), next.clone(), 200));
        assert!(r.changing());
        assert!(!r.reconfigure(&root, Some("S".into()), names(4), 210));
        // Une autre liste nommerait un autre relais de secours : pas d'arrêt sans vote.
        assert_eq!(r.releasable(), Some("S".into()));
        // Deux témoins de l'ancienne liste ont appris la nouvelle : avec le menteur toléré,
        // une reprise y est encore possible (deux qui restent + un menteur = trois).
        r.lease("W0", 2, 1000, 1000);
        r.lease("W1", 2, 1000, 1000);
        assert!(r.changing());
        // Un troisième : plus possible.
        r.lease("W2", 2, 1000, 1000);
        assert!(!r.changing());
        assert!(r.reconfigure(&root, Some("T".into()), next, 1100));
        // Relais de secours remplacé, ancienne liste encore comptée : l'arrêt sans vote
        // n'est pas permis (deux relais de secours pourraient reprendre).
        assert_eq!(r.releasable(), None);
    }

    #[test]
    fn players_check_the_proof_of_a_takeover() {
        let root = key(1);
        let pk = root.verifying_key().to_bytes();
        let c = Committee::sign(
            &root,
            7,
            3,
            Some("S".into()),
            vec!["A".into(), "B".into(), "C".into()],
        );
        let witness = |n: &str, k: u8, version: u64| {
            let sk = key(k);
            let cert =
                crate::sovereign::Cert::issue(&root, 7, n, sk.verifying_key().to_bytes(), u64::MAX);
            (cert, Vote::sign(&sk, 7, version, n))
        };
        let proof = |votes| Proof::Votes {
            committee: c.clone(),
            votes,
        };
        // Deux témoins sur trois : quorum.
        assert!(proof(vec![witness("A", 2, 3), witness("B", 3, 3)]).check(&pk, 7, "s"));
        // Un seul, ou deux fois le même : non (le relais de secours ne compte pas).
        assert!(!proof(vec![witness("A", 2, 3)]).check(&pk, 7, "S"));
        assert!(!proof(vec![witness("A", 2, 3), witness("A", 2, 3)]).check(&pk, 7, "S"));
        // Un vote pour une autre liste, un témoin qui n'en est pas, une signature d'une
        // autre clé que celle du certificat : non.
        assert!(!proof(vec![witness("A", 2, 3), witness("B", 3, 2)]).check(&pk, 7, "S"));
        assert!(!proof(vec![witness("A", 2, 3), witness("Z", 3, 3)]).check(&pk, 7, "S"));
        let (cert_b, _) = witness("B", 3, 3);
        let forged = Vote::sign(&key(4), 7, 3, "B");
        assert!(!proof(vec![witness("A", 2, 3), (cert_b, forged)]).check(&pk, 7, "S"));
        // Un autre relais de secours que celui de la liste, un autre monde, un autre relais
        // du monde : non.
        assert!(!proof(vec![witness("A", 2, 3), witness("B", 3, 3)]).check(&pk, 7, "T"));
        assert!(!proof(vec![witness("A", 2, 3), witness("B", 3, 3)]).check(&pk, 8, "S"));
        let other = key(9).verifying_key().to_bytes();
        assert!(!proof(vec![witness("A", 2, 3), witness("B", 3, 3)]).check(&other, 7, "S"));
        // L'arrêt, signé par le relais du monde.
        let released = Proof::Released {
            sig: sign_release(&root, 7),
        };
        assert!(released.check(&pk, 7, "S"));
        assert!(!released.check(&pk, 8, "S"));
        assert!(!Proof::Released {
            sig: sign_release(&key(5), 7)
        }
        .check(&pk, 7, "S"));
    }

    #[test]
    fn one_guardian_changes_at_a_time() {
        let root = key(1);
        let mut r = Warden::new(&root, 7, Some("S".into()), vec!["A".into()], 0);
        // Deux de plus d'un coup : refusé ; un à la fois : accepté.
        assert!(!r.reconfigure(
            &root,
            Some("S".into()),
            vec!["A".into(), "B".into(), "C".into()],
            1
        ));
        assert!(r.reconfigure(&root, Some("S".into()), vec!["A".into(), "B".into()], 1));
        r.lease("A", 2, 10, 10);
        r.lease("B", 2, 10, 10);
        assert!(!r.changing());
        // Relais de secours remplacé et témoin remplacé en même temps : refusé.
        assert!(!r.reconfigure(&root, Some("T".into()), vec!["A".into(), "C".into()], 20));
        assert!(r.reconfigure(&root, Some("T".into()), vec!["A".into(), "B".into()], 20));
        // Un même gardien deux fois, ou plus de `WITNESSES_MAX` témoins : refusé.
        r.lease("A", 3, 30, 30);
        r.lease("B", 3, 30, 30);
        assert!(!r.changing());
        assert!(!r.reconfigure(&root, Some("T".into()), vec!["T".into(), "B".into()], 40));
        assert!(!r.reconfigure(&root, Some("T".into()), names(WITNESSES_MAX + 1), 40));
    }

    /// Réseau simulé : relais du monde, relais de secours et témoins (1, 3, 4 ou 7), avec
    /// des délais, des coupures au hasard (relais du monde isolé, gardien coupé de lui
    /// seulement, coupures dans un seul sens), des chutes, des arrêts, des pauses sans issue
    /// rendues au relais de secours, des témoins ajoutés, retirés ou remplacés, des relais
    /// de secours remplacés (les anciens continuent de tourner et entendent tous les
    /// témoins). Une partie sur deux, autant de témoins menteurs que la liste en tolère : ils
    /// donnent leurs baux au relais du monde et votent quand même, tout de suite, pour
    /// n'importe quel relais de secours. À aucun instant le relais du monde sert le monde
    /// pendant qu'un relais de secours a de quoi le reprendre, ni deux relais de secours n'en
    /// ont de quoi ; et sans coupure, le relais du monde ne s'arrête pas.
    #[test]
    fn the_world_never_has_two_relays_serving_it() {
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut rnd = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let mut total = Outcome::default();
        for run in 0..160 {
            total.add(simulate(&mut rnd, run));
        }
        // Les coupures ont bien produit des reprises, des pauses, des arrêts, des relais de
        // secours remplacés, et les menteurs ont voté (l'essai n'est pas vide).
        assert!(total.takeovers > 40, "{total:?}");
        assert!(total.pauses > 40, "{total:?}");
        assert!(total.releases > 5, "{total:?}");
        assert!(total.swaps > 20, "{total:?}");
        assert!(total.lies > 100, "{total:?}");
    }

    /// La même chose, beaucoup plus longtemps (à lancer à la main, en mode optimisé :
    /// `cargo test --release -p minelib-net -- --ignored gardiens_longtemps`). C'est elle
    /// qui trouve la séparation si l'on change plus d'un gardien à la fois.
    #[test]
    #[ignore]
    fn gardiens_longtemps() {
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut rnd = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for run in 0..6000 {
            simulate(&mut rnd, run);
        }
    }

    #[derive(Default, Debug)]
    struct Outcome {
        takeovers: u32,
        pauses: u32,
        releases: u32,
        swaps: u32,
        lies: u32,
    }

    impl Outcome {
        fn add(&mut self, o: Outcome) {
            self.takeovers += o.takeovers;
            self.pauses += o.pauses;
            self.releases += o.releases;
            self.swaps += o.swaps;
            self.lies += o.lies;
        }
    }

    #[derive(Clone)]
    enum Msg {
        Beat(u64, Committee),
        Lease(String, u64, u64),
        /// D'un témoin à un relais de secours : sa liste, et la liste de son vote.
        Report(String, Committee, Option<Committee>),
        Learn(Committee),
        Released,
    }

    /// Une partie simulée.
    fn simulate(rnd: &mut impl FnMut() -> u64, run: u64) -> Outcome {
        let root = key(9);
        let pk = root.verifying_key().to_bytes();
        let room = 42;
        let count = [1usize, 3, 4, 7][(run % 4) as usize];
        let mut witnesses: Vec<String> = (0..count).map(|i| format!("w{i}")).collect();
        let lying = if run % 8 >= 4 { (count - 1) / 3 } else { 0 };
        let liars: HashSet<String> = witnesses.iter().take(lying).cloned().collect();
        // (Avec des menteurs, la liste garde sa taille : ils restent dans ce qu'elle
        // tolère.)
        let resize = liars.is_empty();
        let mut fresh_name = 0;
        let mut standby = "s0".to_string();
        let mut warden = Warden::new(&root, room, Some(standby.clone()), witnesses.clone(), 0);
        // Tous les gardiens, anciens compris (ils tournent toujours), et les listes que
        // chacun a vues.
        let mut keepers: HashMap<String, Keeper> = witnesses
            .iter()
            .chain(std::iter::once(&standby))
            .map(|n| (n.clone(), Keeper::new(room, n, pk).unwrap()))
            .collect();
        let mut seen: HashMap<String, HashMap<u64, Committee>> = HashMap::new();
        let mut standbys = vec![standby.clone()];
        // Votes reçus par chaque relais de secours (version de la liste du vote, par
        // témoin).
        let mut votes: HashMap<String, HashMap<String, u64>> = HashMap::new();
        let mut cut: HashSet<(String, String)> = Default::default();
        let mut queue: Vec<(u64, String, String, Msg)> = Vec::new();
        let mut r_alive = true;
        let mut released_to: Option<String> = None;
        let mut out = Outcome::default();
        let (mut last_beat, mut last_report, mut calm_until) = (0u64, 0u64, 0u64);
        let mut paused_since: Option<u64> = None;
        let mut taken: Option<String> = None;
        let step = 10;
        let mut now = 0;
        while now < 40_000 {
            now += step;
            if now % 500 == 0 {
                let roll = rnd() % 100;
                let all: Vec<String> = keepers.keys().cloned().collect();
                let pick = |r: u64, v: &[String]| v[(r % v.len() as u64) as usize].clone();
                if roll < 8 {
                    // Relais du monde isolé de tous ses gardiens.
                    for a in &all {
                        cut.insert(("r".into(), a.clone()));
                        cut.insert((a.clone(), "r".into()));
                    }
                } else if roll < 22 {
                    // Un gardien coupé du relais du monde seulement.
                    let a = pick(rnd(), &all);
                    cut.insert(("r".into(), a.clone()));
                    cut.insert((a, "r".into()));
                } else if roll < 30 {
                    // Coupure quelconque, parfois dans un seul sens.
                    let mut nodes = all.clone();
                    nodes.push("r".into());
                    let (a, b) = (pick(rnd(), &nodes), pick(rnd(), &nodes));
                    if a != b {
                        cut.insert((a.clone(), b.clone()));
                        if rnd().is_multiple_of(2) {
                            cut.insert((b, a));
                        }
                    }
                } else if roll < 44 {
                    cut.clear();
                    calm_until = now + 6000;
                } else if roll < 45 && r_alive {
                    r_alive = false;
                } else if roll < 46 && r_alive && !warden.released() {
                    // Arrêt propre.
                    if let Some(s) = warden.release() {
                        out.releases += 1;
                        if !cut.contains(&("r".into(), s.clone())) {
                            queue.push((now + 20, "r".into(), s, Msg::Released));
                        }
                    }
                } else if roll < 70 && r_alive && !warden.changing() && !warden.released() {
                    // Changement de gardiens, un seul à la fois : témoin remplacé, ajouté
                    // ou retiré ; relais de secours remplacé ; ou deux d'un coup (refusé).
                    fresh_name += 1;
                    let n = format!("n{fresh_name}");
                    let mut next_w = witnesses.clone();
                    let mut next_s = standby.clone();
                    match rnd() % 5 {
                        0 => {
                            let i = (rnd() % next_w.len() as u64) as usize;
                            next_w[i] = n.clone();
                        }
                        1 if resize && next_w.len() < WITNESSES_MAX => next_w.push(n.clone()),
                        2 if resize && next_w.len() > 1 => {
                            next_w.remove((rnd() % next_w.len() as u64) as usize);
                        }
                        3 => {
                            next_s = n.clone();
                            let i = (rnd() % next_w.len() as u64) as usize;
                            fresh_name += 1;
                            let m = format!("n{fresh_name}");
                            keepers
                                .entry(m.clone())
                                .or_insert_with(|| Keeper::new(room, &m, pk).unwrap());
                            next_w[i] = m;
                        }
                        _ => next_s = n.clone(),
                    }
                    keepers
                        .entry(n.clone())
                        .or_insert_with(|| Keeper::new(room, &n, pk).unwrap());
                    if warden.reconfigure(&root, Some(next_s.clone()), next_w.clone(), now) {
                        if next_s != standby {
                            out.swaps += 1;
                            standbys.push(next_s.clone());
                        }
                        witnesses = next_w;
                        standby = next_s;
                    }
                }
            }
            // Messages arrivés.
            let mut later = Vec::new();
            for (at, from, to, m) in std::mem::take(&mut queue) {
                if at > now {
                    later.push((at, from, to, m));
                    continue;
                }
                match m {
                    Msg::Beat(t, c) => {
                        seen.entry(to.clone())
                            .or_default()
                            .insert(c.version, c.clone());
                        let k = keepers.get_mut(&to).unwrap();
                        // Un menteur donne toujours son bail, même après avoir voté.
                        let lease = if liars.contains(&to) {
                            k.learn(&c, now);
                            k.committee.as_ref().map(|c| c.version)
                        } else {
                            k.beat(&c, now)
                        };
                        if let Some(v) = lease {
                            if !cut.contains(&(to.clone(), "r".into())) {
                                let lat = 5 + rnd() % 150;
                                later.push((
                                    now + lat,
                                    to.clone(),
                                    "r".into(),
                                    Msg::Lease(to, v, t),
                                ));
                            }
                        }
                    }
                    Msg::Lease(who, v, t) => {
                        if r_alive {
                            warden.lease(&who, v, t, now);
                        }
                    }
                    Msg::Report(who, c, vote) => {
                        let k = keepers.get_mut(&to).unwrap();
                        k.learn(&c, now);
                        let known = seen.entry(to.clone()).or_default();
                        known.insert(c.version, c.clone());
                        if let Some(v) = vote {
                            known.insert(v.version, v.clone());
                            votes
                                .entry(to.clone())
                                .or_default()
                                .insert(who.clone(), v.version);
                        }
                        if let Some(mine) = k.committee.clone() {
                            if mine.version > c.version && !cut.contains(&(to.clone(), who.clone()))
                            {
                                later.push((now + 5 + rnd() % 100, to, who, Msg::Learn(mine)));
                            }
                        }
                    }
                    Msg::Learn(c) => {
                        seen.entry(to.clone())
                            .or_default()
                            .insert(c.version, c.clone());
                        keepers.get_mut(&to).unwrap().learn(&c, now);
                    }
                    Msg::Released => {
                        released_to.get_or_insert(to);
                    }
                }
            }
            queue = later;
            // Battements.
            if r_alive && !warden.released() && now - last_beat >= BEAT_MS {
                last_beat = now;
                for to in warden.targets() {
                    if !cut.contains(&("r".into(), to.clone())) {
                        let lat = 5 + rnd() % 150;
                        queue.push((
                            now + lat,
                            "r".into(),
                            to,
                            Msg::Beat(now, warden.current.clone()),
                        ));
                    }
                }
            }
            for k in keepers.values_mut() {
                k.tick(now);
            }
            // Chaque témoin parle à tous les relais de secours (le pire cas : un ancien
            // relais de secours entend tout). Un menteur vote pour chacun, pour la liste
            // la plus récente qu'il connaisse où celui-ci est le relais de secours.
            if now - last_report >= PROBE_MS {
                last_report = now;
                for (n, k) in &keepers {
                    let Some(c) = &k.committee else { continue };
                    for s in &standbys {
                        if s == n || cut.contains(&(n.clone(), s.clone())) {
                            continue;
                        }
                        let vote = if liars.contains(n) {
                            let v = seen
                                .get(n)
                                .and_then(|m| {
                                    m.values()
                                        .filter(|c| c.is_standby(s))
                                        .max_by_key(|c| c.version)
                                })
                                .cloned();
                            if v.is_some() {
                                out.lies += 1;
                            }
                            v
                        } else {
                            k.vote_committee().cloned()
                        };
                        queue.push((
                            now + 5 + rnd() % 100,
                            n.clone(),
                            s.clone(),
                            Msg::Report(n.clone(), c.clone(), vote),
                        ));
                    }
                }
            }
            // Qui a de quoi reprendre le monde : l'arrêt signé reçu, ou un quorum de votes
            // pour une liste qu'il connaît.
            let able: Vec<String> = standbys
                .iter()
                .filter(|s| {
                    released_to.as_ref() == Some(*s)
                        || seen.get(*s).is_some_and(|known| {
                            let got = votes.get(*s);
                            known.values().any(|c| {
                                enough_votes(c, s, |w| got.and_then(|v| v.get(w).copied()))
                            })
                        })
                })
                .cloned()
                .collect();
            if taken.is_none() {
                if let Some(s) = able.first() {
                    taken = Some(s.clone());
                    out.takeovers += 1;
                }
            }
            let serving = r_alive && warden.serving(now);
            assert!(
                !(serving && !able.is_empty()) && able.len() <= 1,
                "partie {run} ({count} témoins, {lying} menteurs), {now} ms : deux relais \
                 servent le monde (relais du monde : {serving}, de quoi reprendre : {able:?})"
            );
            // Pause sans issue : le relais du monde confie le monde au relais de secours,
            // s'il l'entend (et s'il est le seul qui pourrait reprendre).
            if r_alive && !serving && !warden.released() {
                out.pauses += 1;
                let since = *paused_since.get_or_insert(now);
                if now - since >= RELEASE_MS {
                    if let Some(s) = warden.releasable() {
                        if !cut.contains(&("r".into(), s.clone()))
                            && !cut.contains(&(s.clone(), "r".into()))
                        {
                            warden.release();
                            out.releases += 1;
                            queue.push((now + 20, "r".into(), s, Msg::Released));
                        }
                    }
                }
                // Sans coupure depuis 6 s, le relais du monde ne s'arrête que si des
                // témoins honnêtes ont voté.
                if cut.is_empty() && now > calm_until && taken.is_none() {
                    let voted = keepers
                        .iter()
                        .any(|(n, k)| !liars.contains(n) && k.voted().is_some());
                    assert!(voted, "partie {run}, {now} ms : pause sans raison");
                }
            } else {
                paused_since = None;
            }
            if taken.is_some() && now % 2000 == 0 {
                // Repris : la partie continue un peu (pour voir si quelqu'un d'autre
                // reprend), puis s'arrête.
                break;
            }
        }
        out
    }
}
