//! Gardiens d'un monde : ce qui l'empêche de se séparer en deux.
//!
//! Un monde de construction qui a un relais de secours a aussi des témoins : des joueurs
//! (même d'autres mondes), par leur connexion de garant. Le relais du monde, le relais de
//! secours et les témoins sont les gardiens du monde. Les règles :
//!
//! - **Le relais du monde ne change le monde que s'il entend une majorité des gardiens**
//!   (lui compris). Il leur envoie un battement toutes les `BEAT_MS` ; chacun de ceux qui
//!   l'entendent répond par un bail qui rappelle l'heure du battement. Un bail vaut
//!   `LEASE_MS` à compter de l'envoi du battement, sur l'horloge du relais du monde. Sans
//!   assez de baux, le monde est en pause : rien n'y change (`Warden::serving`).
//! - **Un gardien qui n'entend plus le relais du monde depuis `VOTE_MS` vote pour la
//!   reprise**, et ne lui donnera plus jamais de bail (`Keeper::tick`). Comme `VOTE_MS`
//!   dépasse `LEASE_MS`, son dernier bail a expiré quand il vote.
//! - **Le relais de secours ne reprend le monde qu'avec les votes d'une majorité** (lui
//!   compris, `may_take_over`). Deux majorités ont toujours un gardien en commun, qui ne
//!   peut pas à la fois avoir donné un bail encore valable et voté : le relais du monde et
//!   le relais de secours ne servent jamais le monde en même temps.
//! - **La liste des gardiens est numérotée et signée par le relais du monde**
//!   (`Committee`). Un vote vaut pour la liste que le gardien avait en votant, et seulement
//!   pour elle. Quand la liste change (un gardien parti, un autre ajouté), le relais du
//!   monde doit encore entendre une majorité de l'ancienne, tant qu'une majorité de ses
//!   membres n'a pas appris la nouvelle : une reprise y reste possible jusque-là.
//! - **Arrêt propre du relais du monde** : il se met en pause pour toujours et le dit au
//!   relais de secours, qui reprend aussitôt (`Warden::release`).
//!
//! Le prix, inévitable (théorème CAP) : un relais du monde coupé de la majorité de ses
//! gardiens, mais encore joint par des joueurs, met le monde en pause pour eux le temps que
//! le relais de secours reprenne. Continuer seul, ce serait la séparation.

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
/// Un gardien qui n'entend plus le relais du monde depuis ce temps vote pour la reprise.
/// La marge sur `LEASE_MS` couvre l'écart de marche des horloges (très largement).
pub const VOTE_MS: u64 = 1250;
/// Un gardien sans bail depuis ce temps est remplacé (parti, coupé, ou il a voté).
pub const REPLACE_MS: u64 = 2000;
/// Le relais du monde dit aux joueurs où en est la pause au moins aussi souvent : un jeu
/// qui n'en entend plus parler depuis `SILENT_MS` cherche le relais de secours.
pub const HEARTBEAT_MS: u64 = 500;
pub const SILENT_MS: u64 = 1500;
/// Un jeu qui cherche le relais de secours réessaie à ce rythme (et un témoin lui dit où
/// il en est à ce rythme).
pub const PROBE_MS: u64 = 250;
/// Témoins voulus par monde quand il y a assez de garants (sinon un seul).
pub const WITNESSES: usize = 3;

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
    let mut m = b"minelib/gardiens/v1".to_vec();
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

    /// Gardiens, relais du monde compris.
    pub fn size(&self) -> usize {
        1 + usize::from(self.standby.is_some()) + self.witnesses.len()
    }

    pub fn majority(&self) -> usize {
        self.size() / 2 + 1
    }

    /// Une reprise est-elle possible avec cette liste : un relais de secours, et assez de
    /// gardiens pour une majorité sans le relais du monde ?
    pub fn can_take_over(&self) -> bool {
        self.standby.is_some() && self.size() > self.majority()
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

/// Un gardien (relais de secours ou témoin), de son côté : il répond aux battements tant
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

    /// Gardien dans la dernière liste connue.
    pub fn member(&self) -> bool {
        self.committee.as_ref().is_some_and(|c| c.has(&self.me))
    }

    /// À appeler régulièrement : vote quand le relais du monde se tait depuis `VOTE_MS`.
    /// Vrai au moment du vote.
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

/// Arrêt propre du relais du monde, signé par lui : il ne servira plus jamais ce monde.
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
/// l'arrêt propre signé par le relais du monde, ou la liste des gardiens (signée par lui)
/// avec les votes signés d'assez de témoins pour une majorité (le relais de secours
/// compte pour lui-même : le joueur le joint par la clé que le relais du monde lui a
/// donnée).
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
                1 + yes.len() >= committee.majority()
            }
        }
    }
}

/// Le relais de secours (`standby`, qui a voté) peut-il reprendre le monde ? Il lui faut,
/// avec lui, une majorité de la liste pour laquelle il a voté, chaque témoin ayant voté
/// pour cette même liste (`votes` : version de la liste de son vote, par pseudo).
pub fn may_take_over(standby: &Keeper, votes: impl Fn(&str) -> Option<u64>) -> bool {
    let Some(c) = standby.vote_committee() else {
        return false;
    };
    if !c.is_standby(&standby.me) || !c.can_take_over() {
        return false;
    }
    let yes = c
        .witnesses
        .iter()
        .filter(|w| votes(&w.to_ascii_lowercase()) == Some(c.version))
        .count();
    1 + yes >= c.majority()
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
    /// Listes précédentes où une reprise reste possible (pas encore apprises par une
    /// majorité de leurs membres).
    old: Vec<Committee>,
    seen: HashMap<String, Seen>,
    released: bool,
}

impl Warden {
    /// Premières listes d'un monde (version 1).
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

    /// Les listes précédentes que la majorité a quittées ne comptent plus.
    fn retire(&mut self) {
        let seen = &self.seen;
        self.old.retain(|c| {
            if !c.can_take_over() {
                return false;
            }
            let moved = c
                .members()
                .filter(|m| {
                    seen.get(m)
                        .is_some_and(|s| s.resigned || s.version > c.version)
                })
                .count();
            1 + moved < c.majority()
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

    fn quorum(&self, c: &Committee, now: u64) -> bool {
        !c.can_take_over() || 1 + c.members().filter(|m| self.fresh(m, now)).count() >= c.majority()
    }

    /// Le relais du monde peut-il changer le monde maintenant ? Il lui faut une majorité
    /// de baux dans chaque liste où une reprise reste possible.
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
    /// ou si elle ajoute ou retire plus d'un gardien : une reprise exige une majorité de sa
    /// liste, et avec un seul gardien de différence, deux majorités de deux listes qui se
    /// suivent ont toujours un gardien en commun (qui ne vote qu'une fois) ; deux relais de
    /// secours ne peuvent donc pas reprendre le monde chacun de son côté.
    pub fn reconfigure(
        &mut self,
        root: &SigningKey,
        standby: Option<String>,
        witnesses: Vec<String>,
        now: u64,
    ) -> bool {
        if self.changing() || self.released {
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

    /// Arrêt propre : le monde est en pause pour toujours ici. Renvoie le relais de
    /// secours qui peut reprendre aussitôt, sans vote : celui de la liste en cours, s'il
    /// est le seul à pouvoir reprendre (pas de changement de gardiens en cours ; sinon,
    /// les gardiens votent, comme pour une chute).
    pub fn release(&mut self) -> Option<String> {
        self.released = true;
        if self.changing() {
            return None;
        }
        self.current.standby.clone()
    }

    pub fn released(&self) -> bool {
        self.released
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(n: u8) -> SigningKey {
        SigningKey::from_bytes(&[n; 32])
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
        assert_eq!((c.size(), c.majority()), (3, 2));
        assert!(c.can_take_over());
        // Sans témoin, le relais de secours ne peut pas avoir de majorité sans le relais
        // du monde : pas de reprise possible.
        let alone = Committee::sign(&root, 7, 2, Some("S".into()), vec![]);
        assert!(!alone.can_take_over());
        let five = Committee::sign(
            &root,
            7,
            3,
            Some("S".into()),
            vec!["A".into(), "B".into(), "C".into()],
        );
        assert_eq!((five.size(), five.majority()), (5, 3));
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
    }

    #[test]
    fn the_standby_needs_a_majority_of_its_own_list() {
        let root = key(1);
        let c = Committee::sign(
            &root,
            7,
            4,
            Some("S".into()),
            vec!["A".into(), "B".into(), "C".into()],
        );
        let mut s = Keeper::new(7, "S", root.verifying_key().to_bytes()).unwrap();
        s.beat(&c, 0);
        assert!(!may_take_over(&s, |_| Some(4)), "il n'a pas voté");
        assert!(s.tick(VOTE_MS));
        assert!(!may_take_over(&s, |w| (w == "a").then_some(4)));
        assert!(may_take_over(&s, |w| (w != "c").then_some(4)));
        // Des votes pour une autre liste ne comptent pas.
        assert!(!may_take_over(&s, |w| (w != "c").then_some(3)));
    }

    #[test]
    fn the_world_relay_pauses_without_a_majority_of_leases() {
        let root = key(1);
        let mut r = Warden::new(&root, 7, Some("S".into()), vec!["W".into()], 0);
        // Nommés à l'instant : ils ne peuvent pas avoir voté.
        assert!(r.serving(LEASE_MS - 1));
        assert!(!r.serving(LEASE_MS));
        r.lease("W", 1, 1500, 1600);
        assert!(r.serving(1500 + LEASE_MS - 1));
        assert!(!r.serving(1500 + LEASE_MS));
        // Un bail qui rappelle un battement du futur (horloge truquée) : ignoré.
        r.lease("S", 1, 9000, 3000);
        assert!(!r.serving(3000));
        r.release();
        r.lease("S", 1, 3000, 3000);
        assert!(!r.serving(3000), "arrêté pour toujours");
    }

    #[test]
    fn an_old_list_counts_until_a_majority_learns_the_new_one() {
        let root = key(1);
        let mut r = Warden::new(&root, 7, Some("S".into()), vec!["W".into()], 0);
        r.lease("S", 1, 100, 100);
        r.lease("W", 1, 100, 100);
        // W parti : remplacé par X.
        assert!(r.reconfigure(&root, Some("S".into()), vec!["X".into()], 200));
        assert!(r.changing());
        assert!(!r.reconfigure(&root, Some("S".into()), vec!["Y".into()], 210));
        // X nommé à l'instant couvre la nouvelle liste, le bail de S l'ancienne.
        assert!(r.serving(1000));
        // S apprend la nouvelle liste : l'ancienne ne compte plus.
        r.lease("S", 2, 1000, 1000);
        assert!(!r.changing());
        assert!(r.reconfigure(&root, Some("S".into()), vec!["Y".into()], 1100));
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
        // Deux témoins sur trois (et le relais de secours) : majorité.
        assert!(proof(vec![witness("A", 2, 3), witness("B", 3, 3)]).check(&pk, 7, "s"));
        // Un seul, ou deux fois le même : non.
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
        // L'arrêt propre, signé par le relais du monde.
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
        r.lease("S", 2, 10, 10);
        r.lease("A", 2, 10, 10);
        assert!(!r.changing());
        // Relais de secours remplacé et témoin remplacé en même temps : refusé.
        assert!(!r.reconfigure(&root, Some("T".into()), vec!["A".into(), "C".into()], 20));
        assert!(r.reconfigure(&root, Some("T".into()), vec!["A".into(), "B".into()], 20));
        // Un même gardien deux fois : refusé.
        r.lease("A", 3, 30, 30);
        r.lease("B", 3, 30, 30);
        assert!(!r.changing());
        assert!(!r.reconfigure(&root, Some("T".into()), vec!["T".into(), "B".into()], 40));
    }

    /// Réseau simulé : relais du monde, relais de secours et témoins, avec des délais, des
    /// coupures au hasard (relais du monde isolé, gardien coupé de lui seulement, coupures
    /// dans un seul sens), des chutes, des arrêts propres, des témoins ajoutés, retirés ou
    /// remplacés, et des relais de secours remplacés (l'ancien continue de tourner, et
    /// reçoit, comme le nouveau, tout ce que disent les témoins). À aucun instant deux
    /// relais ne servent le monde en même temps ; et sans coupure, le relais du monde ne
    /// s'arrête pas.
    #[test]
    fn the_world_never_has_two_relays_serving_it() {
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut rnd = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let (mut takeovers, mut pauses, mut swaps) = (0, 0, 0);
        for run in 0..160 {
            let (t, p, s) = simulate(&mut rnd, run);
            takeovers += t;
            pauses += p;
            swaps += s;
        }
        // Les coupures ont bien produit des reprises, des pauses et des relais de secours
        // remplacés (l'essai n'est pas vide).
        assert!(takeovers > 40, "{takeovers} reprises");
        assert!(pauses > 40, "{pauses} pauses");
        assert!(swaps > 20, "{swaps} relais de secours remplacés");
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

    #[derive(Clone)]
    enum Msg {
        Beat(u64, Committee),
        Lease(String, u64, u64),
        Report(String, Committee, Option<u64>),
        Learn(Committee),
        Released,
    }

    /// Une partie simulée ; renvoie (reprises, instants de pause, relais de secours
    /// remplacés).
    fn simulate(rnd: &mut impl FnMut() -> u64, run: u64) -> (u32, u32, u32) {
        let root = key(9);
        let pk = root.verifying_key().to_bytes();
        let room = 42;
        let count = if run.is_multiple_of(2) { 1 } else { 3 };
        let mut witnesses: Vec<String> = (0..count).map(|i| format!("w{i}")).collect();
        let mut fresh_name = 0;
        let mut standby = "s0".to_string();
        let mut warden = Warden::new(&root, room, Some(standby.clone()), witnesses.clone(), 0);
        // Tous les gardiens, anciens compris (ils tournent toujours).
        let mut keepers: HashMap<String, Keeper> = witnesses
            .iter()
            .chain(std::iter::once(&standby))
            .map(|n| (n.clone(), Keeper::new(room, n, pk).unwrap()))
            .collect();
        let mut standbys = vec![standby.clone()];
        // Votes reçus par chaque relais de secours.
        let mut votes: HashMap<String, HashMap<String, Option<u64>>> = HashMap::new();
        let mut cut: std::collections::HashSet<(String, String)> = Default::default();
        let mut queue: Vec<(u64, String, String, Msg)> = Vec::new();
        let mut r_alive = true;
        let mut active: Option<String> = None;
        let (mut takeovers, mut pauses, mut swaps) = (0, 0, 0);
        let (mut last_beat, mut last_report, mut calm_until) = (0u64, 0u64, 0u64);
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
                } else if roll < 20 {
                    // Un gardien coupé du relais du monde seulement.
                    let a = pick(rnd(), &all);
                    cut.insert(("r".into(), a.clone()));
                    cut.insert((a, "r".into()));
                } else if roll < 28 {
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
                } else if roll < 46 && r_alive {
                    // Arrêt propre.
                    if let Some(s) = warden.release() {
                        if !cut.contains(&("r".into(), s.clone())) {
                            queue.push((now + 20, "r".into(), s, Msg::Released));
                        }
                    }
                } else if roll < 70 && r_alive && !warden.changing() {
                    // Changement de gardiens, un seul à la fois : témoin remplacé, ajouté
                    // ou retiré ; relais de secours remplacé.
                    fresh_name += 1;
                    let n = format!("n{fresh_name}");
                    let mut next_w = witnesses.clone();
                    let mut next_s = standby.clone();
                    match rnd() % 5 {
                        0 if !next_w.is_empty() => {
                            let i = (rnd() % next_w.len() as u64) as usize;
                            next_w[i] = n.clone();
                        }
                        1 if next_w.len() < 4 => next_w.push(n.clone()),
                        2 if next_w.len() > 1 => {
                            next_w.remove((rnd() % next_w.len() as u64) as usize);
                        }
                        // Deux changements d'un coup : refusés (voir `reconfigure`).
                        3 if !next_w.is_empty() => {
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
                            swaps += 1;
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
                        let k = keepers.get_mut(&to).unwrap();
                        if let Some(v) = k.beat(&c, now) {
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
                        votes
                            .entry(to.clone())
                            .or_default()
                            .insert(who.clone(), vote);
                        if let Some(mine) = k.committee.clone() {
                            if mine.version > c.version && !cut.contains(&(to.clone(), who.clone()))
                            {
                                later.push((now + 5 + rnd() % 100, to, who, Msg::Learn(mine)));
                            }
                        }
                    }
                    Msg::Learn(c) => {
                        keepers.get_mut(&to).unwrap().learn(&c, now);
                    }
                    Msg::Released => {
                        if active.is_none() {
                            active = Some(to);
                            takeovers += 1;
                        }
                    }
                }
            }
            queue = later;
            // Battements.
            if r_alive && now - last_beat >= BEAT_MS {
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
            // Chaque témoin parle à tous les relais de secours qu'il connaît (le pire cas :
            // un ancien relais de secours entend tout).
            if now - last_report >= PROBE_MS {
                last_report = now;
                for (n, k) in &keepers {
                    let Some(c) = &k.committee else { continue };
                    for s in &standbys {
                        if s != n && !cut.contains(&(n.clone(), s.clone())) {
                            queue.push((
                                now + 5 + rnd() % 100,
                                n.clone(),
                                s.clone(),
                                Msg::Report(n.clone(), c.clone(), k.voted()),
                            ));
                        }
                    }
                }
            }
            for s in &standbys {
                let got = votes.get(s);
                if active.is_none()
                    && may_take_over(&keepers[s], |w| {
                        got.and_then(|v| v.get(w).copied().flatten())
                    })
                {
                    active = Some(s.clone());
                    takeovers += 1;
                }
            }
            let serving = r_alive && warden.serving(now);
            let others: Vec<&String> = standbys
                .iter()
                .filter(|s| Some(*s) != active.as_ref())
                .filter(|s| {
                    let got = votes.get(*s);
                    may_take_over(&keepers[*s], |w| {
                        got.and_then(|v| v.get(w).copied().flatten())
                    })
                })
                .collect();
            assert!(
                !(serving && active.is_some()) && (active.is_none() || others.is_empty()),
                "partie {run}, {now} ms : deux relais servent le monde \
                 (relais du monde : {serving}, repris par {active:?}, et aussi {others:?})"
            );
            if r_alive && !serving && !warden.released() {
                pauses += 1;
                // Sans coupure depuis 6 s, le relais du monde ne s'arrête que si des
                // gardiens ont voté (il les remplace alors, un à la fois).
                if cut.is_empty() && now > calm_until && active.is_none() {
                    let voted = keepers.values().any(|k| k.voted().is_some());
                    assert!(voted, "partie {run}, {now} ms : pause sans raison");
                }
            }
            if active.is_some() && now % 2000 == 0 {
                // Repris : la partie continue un peu (pour voir si quelqu'un d'autre
                // reprend), puis s'arrête.
                break;
            }
        }
        (takeovers, pauses, swaps)
    }
}
