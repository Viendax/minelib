//! (v240) Clés d'accès (WebAuthn) : ce que le jeu et le relais doivent dire pareil.
//!
//! - Le jeu demande la clé à Windows (Windows Hello, une clé de sécurité, ou un téléphone) pour
//!   `RP_ID`, avec des données client qu'il écrit lui-même (`client_data`) : le relais les
//!   réécrit à l'identique à partir de son défi, et vérifie la signature dessus.
//! - L'identifiant d'utilisateur de la clé (`user_handle`) porte une clé de 32 octets tirée au
//!   hasard par le jeu, et le pseudo : la clé ouvre un coffre gardé par le relais (la clé
//!   privée du compte), que le relais ne peut pas ouvrir. Le relais ne voit jamais cet
//!   identifiant.
//! - Données de l'authentificateur (`parse_auth_data`) : empreinte de `RP_ID`, drapeaux
//!   (présence, vérification de l'utilisateur), compteur, et à la création la clé publique
//!   (COSE : ES256 ou RS256, les seules demandées).

use sha2::{Digest, Sha256};

/// Identifiant de « site » des clés d'accès du jeu (pas un nom de domaine : Windows ne le
/// vérifie pas pour une application).
pub const RP_ID: &str = "minelib";
pub const RP_NAME: &str = "Minelib";

/// Algorithmes COSE demandés : ES256 (P-256), RS256 (Windows Hello sur les machines plus
/// anciennes).
pub const ES256: i32 = -7;
pub const RS256: i32 = -257;

/// Drapeaux des données de l'authentificateur.
pub const FLAG_UP: u8 = 0x01;
pub const FLAG_UV: u8 = 0x04;
pub const FLAG_AT: u8 = 0x40;
pub const FLAG_ED: u8 = 0x80;

/// Base64url sans remplissage (le défi dans les données client).
pub fn b64url(data: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(data.len() * 4 / 3 + 3);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16
            | (c.get(1).copied().unwrap_or(0) as u32) << 8
            | c.get(2).copied().unwrap_or(0) as u32;
        let k = c.len() + 1;
        for i in 0..k {
            out.push(A[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

/// Données client signées par la clé : création (`create`) ou connexion, pour ce défi. Le
/// relais les réécrit à l'identique.
pub fn client_data(create: bool, challenge: &[u8; 32]) -> Vec<u8> {
    let kind = if create {
        "webauthn.create"
    } else {
        "webauthn.get"
    };
    format!(
        "{{\"type\":\"{kind}\",\"challenge\":\"{}\",\"origin\":\"app:{RP_ID}\",\"crossOrigin\":false}}",
        b64url(challenge)
    )
    .into_bytes()
}

/// Identifiant d'utilisateur d'une clé : la clé du coffre (32 octets) puis le pseudo.
pub fn user_handle(key: &[u8; 32], pseudo: &str) -> Vec<u8> {
    let mut h = key.to_vec();
    h.extend_from_slice(pseudo.as_bytes());
    h
}

/// Clé du coffre et pseudo d'un identifiant d'utilisateur (`user_handle`).
pub fn parse_user_handle(h: &[u8]) -> Option<([u8; 32], String)> {
    if h.len() <= 32 || h.len() > 64 {
        return None;
    }
    let key: [u8; 32] = h[..32].try_into().ok()?;
    let pseudo = String::from_utf8(h[32..].to_vec()).ok()?;
    Some((key, pseudo))
}

/// Clé publique d'une clé d'accès.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CoseKey {
    Es256 { x: [u8; 32], y: [u8; 32] },
    Rs256 { n: Vec<u8>, e: Vec<u8> },
}

impl CoseKey {
    /// Pour l'enregistrer : algorithme, puis les nombres.
    pub fn to_bytes(&self) -> Vec<u8> {
        match self {
            CoseKey::Es256 { x, y } => {
                let mut v = vec![1u8];
                v.extend_from_slice(x);
                v.extend_from_slice(y);
                v
            }
            CoseKey::Rs256 { n, e } => {
                let mut v = vec![2u8];
                v.extend_from_slice(&(n.len() as u16).to_be_bytes());
                v.extend_from_slice(n);
                v.extend_from_slice(e);
                v
            }
        }
    }

    pub fn from_bytes(b: &[u8]) -> Option<Self> {
        match b.first()? {
            1 if b.len() == 65 => Some(CoseKey::Es256 {
                x: b[1..33].try_into().ok()?,
                y: b[33..65].try_into().ok()?,
            }),
            2 => {
                let len = u16::from_be_bytes(b.get(1..3)?.try_into().ok()?) as usize;
                let n = b.get(3..3 + len)?.to_vec();
                let e = b.get(3 + len..)?.to_vec();
                (!n.is_empty() && !e.is_empty()).then_some(CoseKey::Rs256 { n, e })
            }
            _ => None,
        }
    }

    /// La signature `sig` (ES256 : DER ; RS256 : PKCS#1 v1.5) de `msg` est-elle bonne ?
    pub fn verify(&self, msg: &[u8], sig: &[u8]) -> bool {
        use ring::signature;
        match self {
            CoseKey::Es256 { x, y } => {
                let mut pt = vec![4u8];
                pt.extend_from_slice(x);
                pt.extend_from_slice(y);
                signature::UnparsedPublicKey::new(&signature::ECDSA_P256_SHA256_ASN1, &pt)
                    .verify(msg, sig)
                    .is_ok()
            }
            CoseKey::Rs256 { n, e } => signature::RsaPublicKeyComponents { n, e }
                .verify(&signature::RSA_PKCS1_2048_8192_SHA256, msg, sig)
                .is_ok(),
        }
    }
}

/// Données de l'authentificateur, lues.
#[derive(Clone, Debug, PartialEq)]
pub struct AuthData {
    pub rp_id_hash: [u8; 32],
    pub flags: u8,
    pub sign_count: u32,
    /// À la création : identifiant de la clé et sa clé publique.
    pub credential: Option<(Vec<u8>, CoseKey)>,
}

/// Lit les données de l'authentificateur.
pub fn parse_auth_data(b: &[u8]) -> Result<AuthData, &'static str> {
    if b.len() < 37 {
        return Err("données trop courtes");
    }
    let rp_id_hash: [u8; 32] = b[..32].try_into().map_err(|_| "rp")?;
    let flags = b[32];
    let sign_count = u32::from_be_bytes(b[33..37].try_into().map_err(|_| "compteur")?);
    let mut credential = None;
    if flags & FLAG_AT != 0 {
        // AAGUID (16), longueur de l'identifiant (2), identifiant, clé COSE.
        let len = u16::from_be_bytes(b.get(53..55).ok_or("identifiant")?.try_into().unwrap())
            as usize;
        let id = b.get(55..55 + len).ok_or("identifiant")?.to_vec();
        let rest = b.get(55 + len..).ok_or("clé")?;
        let (item, _) = cbor(rest, 0)?;
        credential = Some((id, cose_key(&item)?));
    }
    Ok(AuthData {
        rp_id_hash,
        flags,
        sign_count,
        credential,
    })
}

/// Empreinte de `RP_ID` (le début des données de l'authentificateur).
pub fn rp_id_hash() -> [u8; 32] {
    Sha256::digest(RP_ID.as_bytes()).into()
}

/// Vérifie une création de clé (sans attestation : le compte est déjà connecté) : bon
/// « site », présence et vérification de l'utilisateur, clé publique lisible, même
/// identifiant que celui annoncé. Rend la clé publique et le compteur.
pub fn check_creation(auth_data: &[u8], id: &[u8]) -> Result<(CoseKey, u32), &'static str> {
    let a = parse_auth_data(auth_data)?;
    if a.rp_id_hash != rp_id_hash() {
        return Err("clé d'un autre site");
    }
    if a.flags & FLAG_UP == 0 || a.flags & FLAG_UV == 0 {
        return Err("utilisateur pas vérifié");
    }
    let Some((cid, key)) = a.credential else {
        return Err("pas de clé publique");
    };
    if cid != id || id.is_empty() || id.len() > 1023 {
        return Err("identifiant de clé");
    }
    Ok((key, a.sign_count))
}

/// Vérifie une connexion par clé d'accès : bon « site », présence et vérification de
/// l'utilisateur, signature des données de l'authentificateur et de l'empreinte des données
/// client (`client_data` pour ce défi), compteur qui avance (s'il est tenu). Rend le
/// nouveau compteur.
pub fn check_assertion(
    key: &CoseKey,
    last_count: u32,
    auth_data: &[u8],
    challenge: &[u8; 32],
    signature: &[u8],
) -> Result<u32, &'static str> {
    let a = parse_auth_data(auth_data)?;
    if a.rp_id_hash != rp_id_hash() {
        return Err("clé d'un autre site");
    }
    if a.flags & FLAG_UP == 0 || a.flags & FLAG_UV == 0 {
        return Err("utilisateur pas vérifié");
    }
    let mut msg = auth_data.to_vec();
    msg.extend_from_slice(&Sha256::digest(client_data(false, challenge)));
    if !key.verify(&msg, signature) {
        return Err("signature");
    }
    // Compteur : une clé qui en tient un ne revient jamais en arrière (clé copiée).
    if (a.sign_count != 0 || last_count != 0) && a.sign_count <= last_count {
        return Err("compteur de la clé");
    }
    Ok(a.sign_count)
}

/// Élément CBOR (ce qu'il faut pour une clé COSE ; le reste est lu pour être sauté).
#[derive(Clone, Debug, PartialEq)]
#[allow(dead_code)]
enum Cbor {
    Int(i64),
    Bytes(Vec<u8>),
    Text(String),
    Array(Vec<Cbor>),
    Map(Vec<(Cbor, Cbor)>),
    Simple(u8),
}

/// Lit un élément CBOR ; rend l'élément et sa taille.
fn cbor(b: &[u8], depth: u32) -> Result<(Cbor, usize), &'static str> {
    if depth > 8 {
        return Err("CBOR trop profond");
    }
    let first = *b.first().ok_or("CBOR vide")?;
    let major = first >> 5;
    let info = first & 31;
    let (arg, mut at): (u64, usize) = match info {
        0..=23 => (info as u64, 1),
        24 => (*b.get(1).ok_or("CBOR")? as u64, 2),
        25 => (u16::from_be_bytes(b.get(1..3).ok_or("CBOR")?.try_into().unwrap()) as u64, 3),
        26 => (u32::from_be_bytes(b.get(1..5).ok_or("CBOR")?.try_into().unwrap()) as u64, 5),
        27 => (u64::from_be_bytes(b.get(1..9).ok_or("CBOR")?.try_into().unwrap()), 9),
        _ => return Err("CBOR : longueur indéfinie"),
    };
    let item = match major {
        0 => Cbor::Int(i64::try_from(arg).map_err(|_| "CBOR : entier")?),
        1 => Cbor::Int(-1 - i64::try_from(arg).map_err(|_| "CBOR : entier")?),
        2 | 3 => {
            let n = usize::try_from(arg).map_err(|_| "CBOR")?;
            let data = b.get(at..at.checked_add(n).ok_or("CBOR")?).ok_or("CBOR coupé")?;
            at += n;
            if major == 2 {
                Cbor::Bytes(data.to_vec())
            } else {
                Cbor::Text(String::from_utf8(data.to_vec()).map_err(|_| "CBOR : texte")?)
            }
        }
        4 => {
            let n = usize::try_from(arg).map_err(|_| "CBOR")?.min(64);
            let mut v = Vec::new();
            for _ in 0..n {
                let (it, len) = cbor(b.get(at..).ok_or("CBOR coupé")?, depth + 1)?;
                at += len;
                v.push(it);
            }
            Cbor::Array(v)
        }
        5 => {
            let n = usize::try_from(arg).map_err(|_| "CBOR")?.min(64);
            let mut v = Vec::new();
            for _ in 0..n {
                let (k, len) = cbor(b.get(at..).ok_or("CBOR coupé")?, depth + 1)?;
                at += len;
                let (val, len) = cbor(b.get(at..).ok_or("CBOR coupé")?, depth + 1)?;
                at += len;
                v.push((k, val));
            }
            Cbor::Map(v)
        }
        7 => Cbor::Simple(info),
        _ => return Err("CBOR : type"),
    };
    Ok((item, at))
}

/// Clé publique d'une clé COSE (ES256 sur P-256, ou RS256).
fn cose_key(item: &Cbor) -> Result<CoseKey, &'static str> {
    let Cbor::Map(m) = item else {
        return Err("clé COSE");
    };
    let get = |k: i64| {
        m.iter()
            .find(|(key, _)| *key == Cbor::Int(k))
            .map(|(_, v)| v)
    };
    let int = |k: i64| match get(k) {
        Some(Cbor::Int(v)) => Some(*v),
        _ => None,
    };
    let bytes = |k: i64| match get(k) {
        Some(Cbor::Bytes(v)) => Some(v.clone()),
        _ => None,
    };
    match (int(1), int(3)) {
        // EC2, ES256, courbe P-256.
        (Some(2), Some(a)) if a == ES256 as i64 => {
            if int(-1) != Some(1) {
                return Err("courbe");
            }
            let x: [u8; 32] = bytes(-2).and_then(|v| v.try_into().ok()).ok_or("x")?;
            let y: [u8; 32] = bytes(-3).and_then(|v| v.try_into().ok()).ok_or("y")?;
            Ok(CoseKey::Es256 { x, y })
        }
        // RSA, RS256.
        (Some(3), Some(a)) if a == RS256 as i64 => {
            let n = bytes(-1).ok_or("n")?;
            let e = bytes(-2).ok_or("e")?;
            if n.len() < 256 || n.len() > 1024 || e.is_empty() || e.len() > 8 {
                return Err("clé RSA");
            }
            Ok(CoseKey::Rs256 { n, e })
        }
        _ => Err("algorithme de clé"),
    }
}

/// Authentificateur logiciel (essais, et jeu sans Windows avec `MINELIB_CLE_ESSAI`) : une
/// clé ES256 gardée en clair. Ce n'est pas une vraie clé d'accès.
pub struct SoftKey {
    pkcs8: Vec<u8>,
    pub id: Vec<u8>,
    pub user_handle: Vec<u8>,
    pub count: u32,
}

impl SoftKey {
    /// Nouvelle clé pour cet identifiant d'utilisateur : la clé, et les données de
    /// l'authentificateur de sa création.
    pub fn create(user_handle: &[u8]) -> (Self, Vec<u8>) {
        use ring::rand::SecureRandom;
        use ring::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_ASN1_SIGNING};
        let rng = ring::rand::SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &rng)
            .expect("clé P-256")
            .as_ref()
            .to_vec();
        let pair = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &pkcs8, &rng)
            .expect("clé P-256");
        let mut id = vec![0u8; 16];
        rng.fill(&mut id).expect("hasard");
        let pt = pair.public_key().as_ref();
        let mut ad = rp_id_hash().to_vec();
        ad.push(FLAG_UP | FLAG_UV | FLAG_AT);
        ad.extend_from_slice(&1u32.to_be_bytes());
        ad.extend_from_slice(&[0u8; 16]);
        ad.extend_from_slice(&(id.len() as u16).to_be_bytes());
        ad.extend_from_slice(&id);
        // {1: 2, 3: -7, -1: 1, -2: x, -3: y}
        ad.extend_from_slice(&[0xA5, 0x01, 0x02, 0x03, 0x26, 0x20, 0x01, 0x21, 0x58, 0x20]);
        ad.extend_from_slice(&pt[1..33]);
        ad.extend_from_slice(&[0x22, 0x58, 0x20]);
        ad.extend_from_slice(&pt[33..65]);
        (
            Self {
                pkcs8,
                id,
                user_handle: user_handle.to_vec(),
                count: 1,
            },
            ad,
        )
    }

    /// Connexion : données de l'authentificateur et signature pour ce défi.
    pub fn sign(&mut self, challenge: &[u8; 32]) -> (Vec<u8>, Vec<u8>) {
        use ring::signature::{EcdsaKeyPair, ECDSA_P256_SHA256_ASN1_SIGNING};
        let rng = ring::rand::SystemRandom::new();
        let pair = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &self.pkcs8, &rng)
            .expect("clé P-256");
        self.count += 1;
        let mut ad = rp_id_hash().to_vec();
        ad.push(FLAG_UP | FLAG_UV);
        ad.extend_from_slice(&self.count.to_be_bytes());
        let mut msg = ad.clone();
        msg.extend_from_slice(&Sha256::digest(client_data(false, challenge)));
        let sig = pair.sign(&rng, &msg).expect("signature").as_ref().to_vec();
        (ad, sig)
    }

    /// Gardée dans un fichier (essais seulement) : pkcs8, identifiant, utilisateur, compteur.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut v = Vec::new();
        for part in [&self.pkcs8, &self.id, &self.user_handle] {
            v.extend_from_slice(&(part.len() as u16).to_be_bytes());
            v.extend_from_slice(part);
        }
        v.extend_from_slice(&self.count.to_be_bytes());
        v
    }

    pub fn from_bytes(b: &[u8]) -> Option<Self> {
        let mut at = 0;
        let mut part = || {
            let n = u16::from_be_bytes(b.get(at..at + 2)?.try_into().ok()?) as usize;
            let p = b.get(at + 2..at + 2 + n)?.to_vec();
            at += 2 + n;
            Some(p)
        };
        let pkcs8 = part()?;
        let id = part()?;
        let user_handle = part()?;
        let count = u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?);
        Some(Self {
            pkcs8,
            id,
            user_handle,
            count,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn b64url_matches_the_spec() {
        assert_eq!(b64url(b""), "");
        assert_eq!(b64url(b"f"), "Zg");
        assert_eq!(b64url(b"fo"), "Zm8");
        assert_eq!(b64url(b"foo"), "Zm9v");
        assert_eq!(b64url(b"foob"), "Zm9vYg");
        assert_eq!(b64url(&[0xfb, 0xff]), "-_8");
    }

    #[test]
    fn a_soft_key_is_created_then_signs_in() {
        let handle = user_handle(&[7; 32], "Viendax");
        assert_eq!(parse_user_handle(&handle), Some(([7; 32], "Viendax".into())));
        let (mut k, ad) = SoftKey::create(&handle);
        let (key, count) = check_creation(&ad, &k.id).unwrap();
        assert_eq!(count, 1);
        assert_eq!(CoseKey::from_bytes(&key.to_bytes()), Some(key.clone()));
        let challenge = [3u8; 32];
        let (ad, sig) = k.sign(&challenge);
        let count = check_assertion(&key, 1, &ad, &challenge, &sig).unwrap();
        assert_eq!(count, 2);
        // Autre défi, compteur rejoué, signature altérée : refusés.
        assert!(check_assertion(&key, 1, &ad, &[4; 32], &sig).is_err());
        assert!(check_assertion(&key, 2, &ad, &challenge, &sig).is_err());
        let mut bad = sig.clone();
        let last = bad.len() - 1;
        bad[last] ^= 1;
        assert!(check_assertion(&key, 1, &ad, &challenge, &bad).is_err());
        // Pas vérifié (drapeau UV) : refusé.
        let mut ad2 = ad.clone();
        ad2[32] &= !FLAG_UV;
        assert!(check_assertion(&key, 1, &ad2, &challenge, &sig).is_err());
        // Gardée puis relue.
        let back = SoftKey::from_bytes(&k.to_bytes()).unwrap();
        assert_eq!((back.id, back.count), (k.id.clone(), 2));
    }

    #[test]
    fn a_creation_for_another_site_is_refused() {
        let (k, mut ad) = SoftKey::create(&user_handle(&[1; 32], "A"));
        ad[0] ^= 1;
        assert!(check_creation(&ad, &k.id).is_err());
    }

    #[test]
    fn an_rs256_key_is_read() {
        // {1: 3, 3: -257, -1: n (256 octets), -2: e}
        let mut item = vec![0xA4, 0x01, 0x03, 0x03, 0x39, 0x01, 0x00, 0x20, 0x59, 0x01, 0x00];
        item.extend_from_slice(&[0xAB; 256]);
        item.extend_from_slice(&[0x21, 0x43, 0x01, 0x00, 0x01]);
        let (c, len) = cbor(&item, 0).unwrap();
        assert_eq!(len, item.len());
        assert_eq!(
            cose_key(&c).unwrap(),
            CoseKey::Rs256 {
                n: vec![0xAB; 256],
                e: vec![1, 0, 1]
            }
        );
    }
}
