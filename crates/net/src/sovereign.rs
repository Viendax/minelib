//! Régions souveraines : les blocs d'une région peuplée sont décidés par le PC d'un joueur
//! (l'hôte principal de la région), surveillé par les autres hôtes de la région.
//!
//! Ce module contient ce qu'il faut pour que personne n'ait à croire l'hôte principal sur
//! parole :
//!
//! - **Chaque modification est signée par son auteur** (`Edit`), avec la clé de son compte.
//!   Le relais du monde certifie cette clé (`Cert`, dans le billet du joueur). Un hôte ne
//!   peut donc ni inventer une modification, ni l'attribuer à quelqu'un d'autre. Le relais
//!   du monde peut aussi signer à la place d'un joueur (`Edit::by_root`) : une demande qui
//!   lui est arrivée directement, ou une modification en masse d'un OP.
//! - **L'hôte principal signe ce qu'il publie** (`Batch`) : des lots de modifications
//!   numérotées, enchaînés par une empreinte (chaque lot contient l'empreinte de tout ce qui
//!   précède). Ce qu'il a signé l'engage.
//! - **Les autres hôtes revérifient tout** (`Checker`) avec les mêmes règles, et le relais du
//!   monde n'archive un lot que lorsqu'un d'eux l'a confirmé. Une faute se prouve avec les
//!   lots signés eux-mêmes (`judge`) : le relais du monde n'a à croire personne, il refait
//!   la vérification.
//!
//! Les règles, vérifiables par n'importe qui à partir des lots seuls :
//! - la signature de l'auteur (ou du relais du monde) et son certificat ;
//! - la case est dans la région, à une hauteur valide, et à moins de `VIEW` chunks de là
//!   où l'auteur dit se trouver ;
//! - les modifications d'un même auteur ont des dates (signées) strictement croissantes,
//!   pas plus anciennes que le début de l'époque (pas de modification rejouée), et pas plus
//!   de `EDIT_RATE` par seconde (`FLUID_RATE` pour l'écoulement des fluides) ; le relais
//!   du monde, qui limite lui-même chacun, n'a pas de cadence (modification en masse) ;
//! - deux lots signés pour les mêmes numéros sont identiques, et chacun suit le précédent.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Modifications acceptées par seconde et par auteur (dates signées).
pub const EDIT_RATE: usize = 20;
/// Changements dus à l'écoulement de l'eau et de la lave, par seconde et par auteur.
pub const FLUID_RATE: usize = 400;
/// Distance (chunks) entre une case modifiée et l'endroit où son auteur dit se trouver.
pub const VIEW: i32 = 12;
/// Avance d'horloge tolérée par l'hôte principal (ms) pour une modification datée du futur.
pub const SKEW_MS: u64 = 30_000;
/// Hauteur d'un monde.
const HEIGHT: i32 = 256;

/// Région (32 × 32 chunks) d'une case.
pub fn region_of_pos(pos: [i32; 3]) -> [i32; 2] {
    crate::proto::region_of([pos[0].div_euclid(16), pos[2].div_euclid(16)])
}

fn key_of(bytes: &[u8; 32]) -> Option<VerifyingKey> {
    VerifyingKey::from_bytes(bytes).ok()
}

fn sig_of(bytes: &[u8]) -> Option<Signature> {
    let b: [u8; 64] = bytes.try_into().ok()?;
    Some(Signature::from_bytes(&b))
}

/// Certificat d'un compte : le relais du monde atteste que `key` est la clé de `pseudo`
/// dans le monde `room`, jusqu'à `expires` (secondes depuis 1970).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Cert {
    pub pseudo: String,
    pub key: [u8; 32],
    pub expires: u64,
    pub sig: Vec<u8>,
}

fn cert_bytes(room: u64, pseudo: &str, key: &[u8; 32], expires: u64) -> Vec<u8> {
    let mut m = b"minelib/cert/v1".to_vec();
    m.extend(room.to_le_bytes());
    m.extend(pseudo.to_ascii_lowercase().as_bytes());
    m.push(0);
    m.extend(key);
    m.extend(expires.to_le_bytes());
    m
}

impl Cert {
    /// Certificat signé par le relais du monde.
    pub fn issue(root: &SigningKey, room: u64, pseudo: &str, key: [u8; 32], expires: u64) -> Self {
        let sig = root
            .sign(&cert_bytes(room, pseudo, &key, expires))
            .to_bytes()
            .to_vec();
        Self {
            pseudo: pseudo.to_string(),
            key,
            expires,
            sig,
        }
    }

    /// Signé par ce relais du monde, pour ce monde ?
    pub fn valid(&self, root: &VerifyingKey, room: u64) -> bool {
        sig_of(&self.sig).is_some_and(|s| {
            root.verify(&cert_bytes(room, &self.pseudo, &self.key, self.expires), &s)
                .is_ok()
        })
    }
}

/// Une modification de bloc, signée par son auteur (ou par le relais du monde à sa place).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Edit {
    pub pseudo: String,
    pub pos: [i32; 3],
    pub block: u16,
    /// Changement dû à l'écoulement d'un fluide (budget à part).
    pub fluid: bool,
    /// Chunk où l'auteur dit se trouver.
    pub at: [i32; 2],
    /// Date (ms depuis 1970, horloge de l'auteur), strictement croissante pour un auteur.
    pub t: u64,
    /// Signée par le relais du monde (demande qui lui est arrivée directement), pas par
    /// l'auteur.
    pub by_root: bool,
    pub sig: Vec<u8>,
}

impl Edit {
    fn message(&self, room: u64) -> Vec<u8> {
        let mut m = b"minelib/bloc/v1".to_vec();
        m.push(self.by_root as u8);
        m.extend(room.to_le_bytes());
        m.extend(self.pseudo.to_ascii_lowercase().as_bytes());
        m.push(0);
        for v in self.pos {
            m.extend(v.to_le_bytes());
        }
        m.extend(self.block.to_le_bytes());
        m.push(self.fluid as u8);
        for v in self.at {
            m.extend(v.to_le_bytes());
        }
        m.extend(self.t.to_le_bytes());
        m
    }

    /// Modification signée par son auteur (`key` : la clé de son compte) ou, si `by_root`,
    /// par le relais du monde (`key` : la sienne).
    #[allow(clippy::too_many_arguments)]
    pub fn sign(
        key: &SigningKey,
        room: u64,
        pseudo: &str,
        pos: [i32; 3],
        block: u16,
        fluid: bool,
        at: [i32; 2],
        t: u64,
        by_root: bool,
    ) -> Self {
        let mut e = Self {
            pseudo: pseudo.to_string(),
            pos,
            block,
            fluid,
            at,
            t,
            by_root,
            sig: Vec::new(),
        };
        e.sig = key.sign(&e.message(room)).to_bytes().to_vec();
        e
    }

    pub fn verify(&self, room: u64, key: &VerifyingKey) -> bool {
        sig_of(&self.sig).is_some_and(|s| key.verify(&self.message(room), &s).is_ok())
    }

    /// Auteur, en minuscules, et sorte de signature (les dates du relais du monde et
    /// celles de l'auteur ne viennent pas de la même horloge : suivies à part).
    fn author(&self) -> (String, bool) {
        (self.pseudo.to_ascii_lowercase(), self.by_root)
    }
}

/// Une modification acceptée, à son rang dans la région.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub seq: u64,
    pub edit: Edit,
}

/// Lot de modifications publié par l'hôte principal d'une région, signé par lui.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Batch {
    pub room: u64,
    pub region: [i32; 2],
    pub epoch: u64,
    /// Rang de la première modification ; les suivantes se suivent.
    pub first: u64,
    /// Empreinte de tout ce qui précède dans l'époque.
    pub prev: [u8; 32],
    pub records: Vec<Record>,
    /// Certificats des auteurs du lot (pas dans l'empreinte : vérifiés à part).
    pub certs: Vec<Cert>,
    pub sig: Vec<u8>,
}

/// Empreinte de départ d'une époque.
pub fn genesis(room: u64, region: [i32; 2], epoch: u64, first: u64) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"minelib/genese/v1");
    h.update(room.to_le_bytes());
    h.update(region[0].to_le_bytes());
    h.update(region[1].to_le_bytes());
    h.update(epoch.to_le_bytes());
    h.update(first.to_le_bytes());
    h.finalize().into()
}

impl Batch {
    /// Rang qui suit le dernier de ce lot.
    pub fn end(&self) -> u64 {
        self.first + self.records.len() as u64
    }

    /// Empreinte après ce lot.
    pub fn chain(&self) -> [u8; 32] {
        let mut h = Sha256::new();
        h.update(b"minelib/chaine/v1");
        h.update(self.prev);
        h.update(self.epoch.to_le_bytes());
        h.update(self.first.to_le_bytes());
        h.update(postcard::to_allocvec(&self.records).unwrap_or_default());
        h.finalize().into()
    }

    fn signed_bytes(&self) -> Vec<u8> {
        let mut m = b"minelib/lot/v1".to_vec();
        m.extend(self.room.to_le_bytes());
        m.extend(self.region[0].to_le_bytes());
        m.extend(self.region[1].to_le_bytes());
        m.extend(self.epoch.to_le_bytes());
        m.extend(self.first.to_le_bytes());
        m.extend(self.prev);
        m.extend(self.chain());
        m
    }

    pub fn sign(&mut self, key: &SigningKey) {
        self.sig = key.sign(&self.signed_bytes()).to_bytes().to_vec();
    }

    /// Signé par cet hôte, et les rangs se suivent ?
    pub fn verify(&self, primary: &VerifyingKey) -> bool {
        self.numbered()
            && sig_of(&self.sig).is_some_and(|s| primary.verify(&self.signed_bytes(), &s).is_ok())
    }

    /// Les rangs se suivent à partir de `first` (sans rien vérifier de signé).
    pub fn numbered(&self) -> bool {
        self.records
            .iter()
            .enumerate()
            .all(|(i, r)| r.seq == self.first + i as u64)
    }

    /// Vérification d'un lot pris seul, par n'importe qui (le relais du monde, ou l'hôte
    /// d'une autre région à qui il le confie) : signé par l'hôte principal `primary`, et
    /// chaque modification valable (signature, certificat, case, dates dans le lot).
    /// Renvoie la faute, ou `None`. Un lot mal signé n'est une faute prouvable que pour
    /// qui l'a reçu de l'hôte lui-même (le relais du monde) : `judge` l'ignore.
    pub fn audit(&self, rules: &Rules, epoch: u64, primary: &VerifyingKey) -> Option<String> {
        if self.room != rules.room || self.region != rules.region || self.epoch != epoch {
            return Some("lot d'une autre région ou d'une autre époque".into());
        }
        if !self.verify(primary) {
            return Some("lot mal signé par l'hôte principal".into());
        }
        judge(rules, epoch, primary, std::slice::from_ref(self))
    }
}

/// Ce qui est vérifiable sans rien connaître d'autre : la région, la clé du relais du
/// monde, la date la plus ancienne acceptée dans l'époque.
#[derive(Clone, Debug)]
pub struct Rules {
    pub room: u64,
    pub region: [i32; 2],
    pub root: [u8; 32],
    /// Date (ms) en dessous de laquelle une modification est périmée dans l'époque.
    pub floor_t: u64,
}

impl Rules {
    /// Faute d'une modification prise seule (signature, certificat, case), ou `None`.
    pub fn fault(&self, edit: &Edit, certs: &HashMap<String, Cert>) -> Option<String> {
        let room = self.room;
        if edit.by_root {
            let ok = key_of(&self.root).is_some_and(|k| edit.verify(room, &k));
            if !ok {
                return Some(format!(
                    "signature du relais du monde fausse ({})",
                    edit.pseudo
                ));
            }
        } else {
            let Some(cert) = certs.get(&edit.pseudo.to_ascii_lowercase()) else {
                return Some(format!("pas de certificat pour {}", edit.pseudo));
            };
            let root_ok = key_of(&self.root).is_some_and(|k| cert.valid(&k, room));
            if !root_ok || edit.t > cert.expires.saturating_mul(1000) {
                return Some(format!("certificat de {} faux ou périmé", edit.pseudo));
            }
            if !key_of(&cert.key).is_some_and(|k| edit.verify(room, &k)) {
                return Some(format!("signature de {} fausse", edit.pseudo));
            }
        }
        let chunk = [edit.pos[0].div_euclid(16), edit.pos[2].div_euclid(16)];
        if crate::proto::region_of(chunk) != self.region {
            return Some(format!("case {:?} hors de la région", edit.pos));
        }
        if !(0..HEIGHT).contains(&edit.pos[1]) {
            return Some(format!("hauteur {} hors du monde", edit.pos[1]));
        }
        if (edit.at[0] - chunk[0]).abs() > VIEW || (edit.at[1] - chunk[1]).abs() > VIEW {
            return Some(format!("case {:?} hors de vue de son auteur", edit.pos));
        }
        if edit.t < self.floor_t {
            return Some(format!(
                "modification de {} périmée (antérieure à l'époque)",
                edit.pseudo
            ));
        }
        None
    }
}

/// Faute de l'hôte principal, avec de quoi la prouver (ses lots signés).
#[derive(Clone, Debug)]
pub struct Fault {
    pub why: String,
    pub evidence: Vec<Arc<Batch>>,
}

/// Dernières modifications d'un auteur : date, fluide, lot qui la contient.
#[derive(Default)]
struct Author {
    last: Option<(u64, Arc<Batch>)>,
    window: VecDeque<(u64, bool, Arc<Batch>)>,
}

/// Vérification de ce que publie l'hôte principal d'une région, dans une époque : chaque
/// lot doit suivre le précédent et respecter les règles. Sert aux autres hôtes de la
/// région ; l'hôte principal s'en sert aussi pour lui-même (`admit`).
pub struct Checker {
    pub rules: Rules,
    pub epoch: u64,
    /// Rang de la prochaine modification attendue.
    pub next: u64,
    /// Empreinte après le dernier lot vérifié.
    pub chain: [u8; 32],
    certs: HashMap<String, Cert>,
    authors: HashMap<(String, bool), Author>,
    /// Dernier lot vérifié (preuve d'une rupture de chaîne).
    last: Option<Arc<Batch>>,
}

impl Checker {
    /// Époque qui commence au rang `next`, après l'empreinte `chain`.
    pub fn new(rules: Rules, epoch: u64, next: u64, chain: [u8; 32]) -> Self {
        Self {
            rules,
            epoch,
            next,
            chain,
            certs: HashMap::new(),
            authors: HashMap::new(),
            last: None,
        }
    }

    /// Retient un certificat (s'il est valide).
    pub fn learn(&mut self, cert: &Cert) {
        let valid = key_of(&self.rules.root).is_some_and(|k| cert.valid(&k, self.rules.room));
        if valid {
            self.certs
                .insert(cert.pseudo.to_ascii_lowercase(), cert.clone());
        }
    }

    /// Certificat connu d'un auteur.
    pub fn cert(&self, pseudo: &str) -> Option<&Cert> {
        self.certs.get(&pseudo.to_ascii_lowercase())
    }

    /// Date et cadence d'un auteur (l'enregistrement est dans `batch`). Renvoie la faute,
    /// avec les lots qui la prouvent.
    fn pace(&mut self, edit: &Edit, batch: &Arc<Batch>) -> Result<(), Fault> {
        let a = self.authors.entry(edit.author()).or_default();
        if let Some((t, b)) = &a.last {
            if edit.t <= *t {
                return Err(Fault {
                    why: format!(
                        "modification de {} rejouée ou datée en arrière ({} après {})",
                        edit.pseudo, edit.t, t
                    ),
                    evidence: vec![b.clone(), batch.clone()],
                });
            }
        }
        // (Signée par le relais du monde : il limite lui-même la cadence de chacun, et
        // une modification en masse d'un OP passe par lui.)
        if edit.by_root {
            a.last = Some((edit.t, batch.clone()));
            return Ok(());
        }
        while a.window.front().is_some_and(|w| w.0 + 1000 <= edit.t) {
            a.window.pop_front();
        }
        let (max, fluid) = if edit.fluid {
            (FLUID_RATE, true)
        } else {
            (EDIT_RATE, false)
        };
        let same: Vec<&Arc<Batch>> = a
            .window
            .iter()
            .filter(|w| w.1 == fluid)
            .map(|w| &w.2)
            .collect();
        if same.len() >= max {
            let mut evidence: Vec<Arc<Batch>> = Vec::new();
            for b in same.into_iter().chain(std::iter::once(batch)) {
                if !evidence.iter().any(|e| Arc::ptr_eq(e, b)) {
                    evidence.push(b.clone());
                }
            }
            return Err(Fault {
                why: format!(
                    "plus de {max} modifications de {} en une seconde",
                    edit.pseudo
                ),
                evidence,
            });
        }
        a.window.push_back((edit.t, fluid, batch.clone()));
        a.last = Some((edit.t, batch.clone()));
        Ok(())
    }

    /// Vérifie un lot de l'hôte principal (clé `primary`) et le prend en compte. Un lot
    /// d'une autre époque, ou qui ne suit pas (lot manquant), est écarté sans faute
    /// (`Ok(false)`) ; `Err` : faute prouvée par les lots joints.
    pub fn check(&mut self, batch: Batch, primary: &VerifyingKey) -> Result<bool, Fault> {
        let rules = &self.rules;
        if batch.room != rules.room || batch.region != rules.region || batch.epoch != self.epoch {
            return Ok(false);
        }
        if !batch.verify(primary) {
            // Pas signé par l'hôte principal : rien ne l'engage (message abîmé, ou venu
            // d'ailleurs).
            return Ok(false);
        }
        if batch.first != self.next {
            return Ok(false);
        }
        let batch = Arc::new(batch);
        if batch.prev != self.chain {
            let mut evidence = vec![batch.clone()];
            if let Some(l) = &self.last {
                evidence.insert(0, l.clone());
            }
            return Err(Fault {
                why: "lot qui ne suit pas le précédent (empreinte)".into(),
                evidence,
            });
        }
        for c in &batch.certs {
            self.learn(c);
        }
        for r in &batch.records {
            if let Some(why) = self.rules.fault(&r.edit, &self.certs) {
                return Err(Fault {
                    why,
                    evidence: vec![batch.clone()],
                });
            }
            self.pace(&r.edit, &batch)?;
        }
        self.next = batch.end();
        self.chain = batch.chain();
        self.last = Some(batch);
        Ok(true)
    }

    /// L'hôte principal : cette modification peut-elle être acceptée maintenant
    /// (`now_ms` : son horloge) ? Mêmes règles que pour ceux qui le surveillent, plus une
    /// date pas trop dans le futur. En cas de oui, elle compte pour la cadence de son
    /// auteur.
    pub fn admit(&mut self, edit: &Edit, cert: Option<&Cert>, now_ms: u64) -> Result<(), String> {
        if let Some(c) = cert {
            self.learn(c);
        }
        if let Some(why) = self.rules.fault(edit, &self.certs) {
            return Err(why);
        }
        if edit.t > now_ms + SKEW_MS {
            return Err(format!("modification de {} datée du futur", edit.pseudo));
        }
        // (Le lot n'existe pas encore : un lot vide tient sa place pour la cadence.)
        let placeholder = Arc::new(Batch {
            room: self.rules.room,
            region: self.rules.region,
            epoch: self.epoch,
            first: 0,
            prev: [0; 32],
            records: Vec::new(),
            certs: Vec::new(),
            sig: Vec::new(),
        });
        self.pace(edit, &placeholder).map_err(|f| f.why)
    }

    /// L'hôte principal scelle un lot de modifications déjà admises : rangs, empreinte,
    /// certificats de leurs auteurs, signature.
    pub fn seal(&mut self, edits: Vec<Edit>, key: &SigningKey) -> Batch {
        let first = self.next;
        let records: Vec<Record> = edits
            .into_iter()
            .enumerate()
            .map(|(i, edit)| Record {
                seq: first + i as u64,
                edit,
            })
            .collect();
        let mut certs: Vec<Cert> = Vec::new();
        for r in &records {
            if r.edit.by_root
                || certs
                    .iter()
                    .any(|c| c.pseudo.eq_ignore_ascii_case(&r.edit.pseudo))
            {
                continue;
            }
            if let Some(c) = self.cert(&r.edit.pseudo) {
                certs.push(c.clone());
            }
        }
        let mut batch = Batch {
            room: self.rules.room,
            region: self.rules.region,
            epoch: self.epoch,
            first,
            prev: self.chain,
            records,
            certs,
            sig: Vec::new(),
        };
        batch.sign(key);
        self.next = batch.end();
        self.chain = batch.chain();
        let batch_arc = Arc::new(batch.clone());
        // (Les lots qui tenaient la place des vrais dans la cadence sont remplacés par
        // celui-ci : c'est lui qui servira de preuve à ceux qui surveillent.)
        for r in &batch.records {
            if let Some(a) = self.authors.get_mut(&r.edit.author()) {
                for w in a.window.iter_mut().filter(|w| w.0 == r.edit.t) {
                    w.2 = batch_arc.clone();
                }
                if a.last.as_ref().is_some_and(|l| l.0 == r.edit.t) {
                    a.last = Some((r.edit.t, batch_arc.clone()));
                }
            }
        }
        self.last = Some(batch_arc);
        batch
    }
}

/// Le relais du monde juge une accusation : les lots joints (signés par l'hôte principal
/// accusé, clé `primary`, tous de l'époque `epoch` aux règles `rules`) prouvent-ils une
/// faute ? Renvoie laquelle. Il refait la vérification lui-même : il n'a à croire ni
/// l'accusateur ni l'accusé.
pub fn judge(
    rules: &Rules,
    epoch: u64,
    primary: &VerifyingKey,
    batches: &[Batch],
) -> Option<String> {
    if batches.is_empty() || batches.len() > 64 {
        return None;
    }
    // Tout doit être signé par l'accusé, dans cette époque, pour cette région.
    let signed = batches.iter().all(|b| {
        b.room == rules.room && b.region == rules.region && b.epoch == epoch && b.verify(primary)
    });
    if !signed {
        return None;
    }
    // Deux lots différents pour les mêmes rangs, ou un lot qui ne suit pas le précédent.
    for a in batches {
        for b in batches {
            if a.first == b.first && a.chain() != b.chain() {
                return Some("deux lots différents signés pour les mêmes rangs".into());
            }
            if b.first == a.end() && b.prev != a.chain() {
                return Some("lot qui ne suit pas le précédent (empreinte)".into());
            }
        }
    }
    // Chaque modification prise seule.
    let mut certs = HashMap::new();
    for b in batches {
        for c in &b.certs {
            let valid = key_of(&rules.root).is_some_and(|k| c.valid(&k, rules.room));
            if valid {
                certs.insert(c.pseudo.to_ascii_lowercase(), c.clone());
            }
        }
    }
    let mut records: Vec<&Record> = batches.iter().flat_map(|b| &b.records).collect();
    records.sort_by_key(|r| r.seq);
    records.dedup_by_key(|r| r.seq);
    for r in &records {
        if let Some(why) = rules.fault(&r.edit, &certs) {
            return Some(why);
        }
    }
    // Dates et cadence de chaque auteur, dans l'ordre des rangs.
    let mut by_author: HashMap<(String, bool), Vec<&Edit>> = HashMap::new();
    for r in &records {
        by_author.entry(r.edit.author()).or_default().push(&r.edit);
    }
    for ((_, by_root), edits) in &by_author {
        for w in edits.windows(2) {
            if w[1].t <= w[0].t {
                return Some(format!(
                    "modification de {} rejouée ou datée en arrière",
                    w[1].pseudo
                ));
            }
        }
        for fluid in [false, true] {
            if *by_root {
                break;
            }
            let max = if fluid { FLUID_RATE } else { EDIT_RATE };
            let ts: Vec<u64> = edits
                .iter()
                .filter(|e| e.fluid == fluid)
                .map(|e| e.t)
                .collect();
            for (i, t) in ts.iter().enumerate() {
                let n = ts[i..].iter().take_while(|u| **u < t + 1000).count();
                if n > max {
                    return Some(format!(
                        "plus de {max} modifications de {} en une seconde",
                        edits[0].pseudo
                    ));
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOM: u64 = 77;
    const REGION: [i32; 2] = [1, -2];

    fn key(n: u8) -> SigningKey {
        SigningKey::from_bytes(&[n; 32])
    }

    fn rules(root: &SigningKey) -> Rules {
        Rules {
            room: ROOM,
            region: REGION,
            root: root.verifying_key().to_bytes(),
            floor_t: 1_000_000,
        }
    }

    /// Une case de la région, et le chunk de son auteur.
    fn spot(i: i32) -> ([i32; 3], [i32; 2]) {
        let pos = [32 * 16 + 5 + i, 64, -64 * 16 + 7];
        (pos, [pos[0].div_euclid(16), pos[2].div_euclid(16)])
    }

    struct World {
        root: SigningKey,
        primary: SigningKey,
        players: Vec<(String, SigningKey, Cert)>,
    }

    fn world() -> World {
        let root = key(1);
        let players = (0..3)
            .map(|i| {
                let k = key(10 + i);
                let name = format!("Joueur{i}");
                let c = Cert::issue(&root, ROOM, &name, k.verifying_key().to_bytes(), 9_999_999);
                (name, k, c)
            })
            .collect();
        World {
            root,
            primary: key(2),
            players,
        }
    }

    fn edit(w: &World, p: usize, i: i32, t: u64) -> Edit {
        let (pos, at) = spot(i);
        let (name, k, _) = &w.players[p];
        Edit::sign(k, ROOM, name, pos, 3, false, at, t, false)
    }

    /// L'hôte principal admet et scelle ; ceux qui surveillent vérifient les mêmes lots.
    #[test]
    fn an_honest_primary_is_followed_by_its_witnesses() {
        let w = world();
        let g = genesis(ROOM, REGION, 5, 1000);
        let mut primary = Checker::new(rules(&w.root), 5, 1000, g);
        let mut witness = Checker::new(rules(&w.root), 5, 1000, g);
        let pk = w.primary.verifying_key();
        let mut t = 1_000_100;
        for round in 0..5 {
            let mut edits = Vec::new();
            for p in 0..3 {
                t += 60;
                let e = edit(&w, p, round * 3 + p as i32, t);
                primary.admit(&e, Some(&w.players[p].2), t).expect("admise");
                edits.push(e);
            }
            // Le relais du monde signe aussi pour un joueur (demande arrivée chez lui).
            let (pos, at) = spot(40 + round);
            let r = Edit::sign(&w.root, ROOM, "Joueur0", pos, 0, false, at, t, true);
            primary.admit(&r, None, t).expect("admise");
            edits.push(r);
            let batch = primary.seal(edits, &w.primary);
            assert!(matches!(witness.check(batch, &pk), Ok(true)));
        }
        assert_eq!((witness.next, witness.chain), (primary.next, primary.chain));
        assert_eq!(witness.next, 1000 + 5 * 4);
    }

    fn sealed(w: &World, c: &mut Checker, edits: Vec<Edit>) -> Batch {
        // (Un hôte malhonnête scelle sans rien vérifier.)
        for p in &w.players {
            c.learn(&p.2);
        }
        c.seal(edits, &w.primary)
    }

    fn accused(w: &World, bad: Vec<Batch>, before: Vec<Batch>) -> (String, Option<String>) {
        let g = genesis(ROOM, REGION, 5, 1000);
        let mut witness = Checker::new(rules(&w.root), 5, 1000, g);
        let pk = w.primary.verifying_key();
        for b in before {
            assert!(witness.check(b, &pk).expect("lot honnête"));
        }
        let mut fault = None;
        for b in bad {
            if let Err(f) = witness.check(b, &pk) {
                fault = Some(f);
                break;
            }
        }
        let fault = fault.expect("faute vue par celui qui surveille");
        let evidence: Vec<Batch> = fault.evidence.iter().map(|b| (**b).clone()).collect();
        // Le relais du monde refait la vérification avec les seuls lots joints.
        (fault.why, judge(&rules(&w.root), 5, &pk, &evidence))
    }

    #[test]
    fn a_forged_or_misplaced_edit_is_caught_and_proven() {
        let w = world();
        let g = genesis(ROOM, REGION, 5, 1000);
        // Une modification au nom d'un autre, signée par l'hôte avec sa propre clé.
        let mut c = Checker::new(rules(&w.root), 5, 1000, g);
        let (pos, at) = spot(0);
        let forged = Edit::sign(
            &w.primary, ROOM, "Joueur1", pos, 9, false, at, 1_000_500, false,
        );
        let b = sealed(&w, &mut c, vec![forged]);
        let (why, proven) = accused(&w, vec![b], vec![]);
        assert!(why.contains("signature"), "{why}");
        assert!(proven.is_some());
        // Au nom du relais du monde, sans sa clé.
        let mut c = Checker::new(rules(&w.root), 5, 1000, g);
        let fake_root = Edit::sign(
            &w.primary, ROOM, "Joueur1", pos, 9, false, at, 1_000_500, true,
        );
        let b = sealed(&w, &mut c, vec![fake_root]);
        assert!(accused(&w, vec![b], vec![]).1.is_some());
        // Hors de la région, hors de vue, sous le monde, ou d'avant l'époque : signées
        // par leur auteur, mais refusées quand même.
        let (name, k, _) = &w.players[0];
        for (pos, at, t) in [
            ([0, 64, 0], [0, 0], 1_000_500),
            (spot(0).0, [0, 0], 1_000_500),
            ([spot(0).0[0], -1, spot(0).0[2]], spot(0).1, 1_000_500),
            (spot(0).0, spot(0).1, 999_000),
        ] {
            let mut c = Checker::new(rules(&w.root), 5, 1000, g);
            let e = Edit::sign(k, ROOM, name, pos, 1, false, at, t, false);
            let b = sealed(&w, &mut c, vec![e]);
            assert!(
                accused(&w, vec![b], vec![]).1.is_some(),
                "{pos:?} {at:?} {t}"
            );
        }
    }

    #[test]
    fn a_replayed_edit_or_a_flood_is_caught_and_proven() {
        let w = world();
        let g = genesis(ROOM, REGION, 5, 1000);
        // Rejouée : la même modification, deux lots plus loin.
        let mut c = Checker::new(rules(&w.root), 5, 1000, g);
        let e = edit(&w, 0, 0, 1_000_500);
        let b1 = sealed(&w, &mut c, vec![e.clone()]);
        let b2 = sealed(&w, &mut c, vec![edit(&w, 1, 1, 1_000_600)]);
        let b3 = sealed(&w, &mut c, vec![e]);
        let (why, proven) = accused(&w, vec![b3], vec![b1, b2]);
        assert!(why.contains("rejouée"), "{why}");
        assert!(proven.is_some());
        // 21 modifications d'un même joueur en une seconde (dates signées).
        let mut c = Checker::new(rules(&w.root), 5, 1000, g);
        let mut batches = Vec::new();
        for i in 0..21 {
            batches.push(sealed(
                &w,
                &mut c,
                vec![edit(&w, 2, i, 1_000_500 + 40 * i as u64)],
            ));
        }
        let (why, proven) = accused(&w, batches, vec![]);
        assert!(why.contains("en une seconde"), "{why}");
        assert!(proven.is_some(), "prouvé avec les lots joints");
        // 20 seulement : rien à redire.
        let mut c = Checker::new(rules(&w.root), 5, 1000, g);
        let mut witness = Checker::new(rules(&w.root), 5, 1000, g);
        for i in 0..20 {
            let b = sealed(&w, &mut c, vec![edit(&w, 2, i, 1_000_500 + 40 * i as u64)]);
            assert_eq!(
                witness.check(b, &w.primary.verifying_key()).ok(),
                Some(true)
            );
        }
    }

    #[test]
    fn a_rewritten_history_is_caught_and_proven() {
        let w = world();
        let g = genesis(ROOM, REGION, 5, 1000);
        let mut c = Checker::new(rules(&w.root), 5, 1000, g);
        let b1 = sealed(&w, &mut c, vec![edit(&w, 0, 0, 1_000_500)]);
        // Le lot suivant prétend suivre autre chose.
        let mut b2 = sealed(&w, &mut c, vec![edit(&w, 1, 1, 1_000_600)]);
        b2.prev = [9; 32];
        b2.sign(&w.primary);
        let (why, proven) = accused(&w, vec![b2], vec![b1.clone()]);
        assert!(why.contains("empreinte"), "{why}");
        assert!(proven.is_some());
        // Deux versions signées des mêmes rangs (à deux surveillants différents).
        let mut other = Checker::new(rules(&w.root), 5, 1000, g);
        let b1_other = sealed(&w, &mut other, vec![edit(&w, 2, 5, 1_000_500)]);
        let pk = w.primary.verifying_key();
        assert!(judge(&rules(&w.root), 5, &pk, &[b1, b1_other]).is_some());
    }

    #[test]
    fn an_innocent_primary_is_never_condemned() {
        let w = world();
        let g = genesis(ROOM, REGION, 5, 1000);
        let mut c = Checker::new(rules(&w.root), 5, 1000, g);
        let pk = w.primary.verifying_key();
        let mut batches = Vec::new();
        let mut t = 1_000_500;
        let mut last_of_0 = None;
        for i in 0..30 {
            let mut edits = Vec::new();
            for p in 0..3 {
                t += 17;
                let e = edit(&w, p, i * 3 + p as i32, t);
                c.admit(&e, Some(&w.players[p].2), t).expect("admise");
                if p == 0 {
                    last_of_0 = Some(e.clone());
                }
                edits.push(e);
            }
            batches.push(c.seal(edits, &w.primary));
        }
        assert_eq!(judge(&rules(&w.root), 5, &pk, &batches[..20]), None);
        assert_eq!(judge(&rules(&w.root), 5, &pk, &batches[10..]), None);
        // Un accusateur qui trafique un lot (ou en invente) ne prouve rien : ce n'est
        // plus la signature de l'accusé.
        let mut tampered = batches[3].clone();
        tampered.records[0].edit.block = 99;
        assert_eq!(judge(&rules(&w.root), 5, &pk, &[tampered]), None);
        let mut invented = batches[4].clone();
        invented.sign(&key(66));
        assert_eq!(judge(&rules(&w.root), 5, &pk, &[invented]), None);
        // Un lot d'une autre époque ou d'un autre monde : rien non plus.
        assert_eq!(judge(&rules(&w.root), 6, &pk, &batches[..2]), None);
        // L'hôte principal honnête refuse lui-même ce qui serait une faute.
        let replay = last_of_0.expect("modification");
        let why = c.admit(&replay, None, t).expect_err("rejouée");
        assert!(why.contains("rejouée"), "{why}");
        let far = edit(&w, 0, 1, t + 10 * SKEW_MS);
        assert!(c.admit(&far, None, t).is_err(), "datée du futur");
    }
}
