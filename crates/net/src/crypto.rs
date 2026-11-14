//! Clés des comptes : tout ce qui touche au mot de passe se fait sur la machine du joueur.
//!
//! - `derive` tire du mot de passe (Argon2id, 64 Mo) deux clés : `auth`, que le relais
//!   vérifie avant de rendre le coffre, et `enc`, qui chiffre le coffre et ne quitte
//!   jamais la machine.
//! - Le coffre contient la clé privée Ed25519 du compte (XChaCha20-Poly1305).
//! - La preuve de travail rend coûteuse la création de comptes en masse.

use anyhow::{anyhow, Result};
use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use ed25519_dalek::SigningKey;
use hkdf::Hkdf;
use rand::RngCore;
use sha2::{Digest, Sha256};

/// Clés tirées du mot de passe.
pub struct Keys {
    pub auth: [u8; 32],
    pub enc: [u8; 32],
}

/// Mémoire d'Argon2id (Kio) : 64 Mo sur le jeu, moins dans les tests.
#[cfg(not(test))]
const ARGON_MEM: u32 = 64 * 1024;
#[cfg(test)]
const ARGON_MEM: u32 = 1024;

/// Clés d'un compte à partir du pseudo (sel, sans tenir compte des majuscules) et du mot
/// de passe.
pub fn derive(pseudo: &str, password: &str) -> Result<Keys> {
    let salt = format!("minelib/compte/{}", pseudo.to_ascii_lowercase());
    let params = Params::new(ARGON_MEM, 3, 1, Some(32)).map_err(|e| anyhow!("{e}"))?;
    let mut master = [0u8; 32];
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(password.as_bytes(), salt.as_bytes(), &mut master)
        .map_err(|e| anyhow!("{e}"))?;
    let hk = Hkdf::<Sha256>::new(None, &master);
    let mut keys = Keys {
        auth: [0; 32],
        enc: [0; 32],
    };
    hk.expand(b"auth", &mut keys.auth)
        .map_err(|e| anyhow!("{e}"))?;
    hk.expand(b"enc", &mut keys.enc)
        .map_err(|e| anyhow!("{e}"))?;
    Ok(keys)
}

/// Nouvelle clé de compte, au hasard.
pub fn new_identity() -> SigningKey {
    SigningKey::generate(&mut rand::rngs::OsRng)
}

/// Chiffre `data` (nonce de 24 octets en tête).
pub fn seal(key: &[u8; 32], data: &[u8]) -> Vec<u8> {
    let cipher = XChaCha20Poly1305::new(key.into());
    let mut nonce = [0u8; 24];
    rand::rngs::OsRng.fill_bytes(&mut nonce);
    let mut out = nonce.to_vec();
    out.extend(
        cipher
            .encrypt(XNonce::from_slice(&nonce), data)
            .expect("chiffrement"),
    );
    out
}

/// Déchiffre ce que `seal` a produit (erreur si la clé est fausse ou les données
/// altérées).
pub fn open(key: &[u8; 32], sealed: &[u8]) -> Result<Vec<u8>> {
    if sealed.len() < 24 + 16 {
        return Err(anyhow!("données trop courtes"));
    }
    let cipher = XChaCha20Poly1305::new(key.into());
    cipher
        .decrypt(XNonce::from_slice(&sealed[..24]), &sealed[24..])
        .map_err(|_| anyhow!("déchiffrement impossible"))
}

/// Coffre : la clé privée du compte chiffrée par la clé `enc`.
pub fn seal_identity(keys: &Keys, id: &SigningKey) -> Vec<u8> {
    seal(&keys.enc, &id.to_bytes())
}

pub fn open_identity(keys: &Keys, vault: &[u8]) -> Result<SigningKey> {
    let raw = open(&keys.enc, vault)?;
    let bytes: [u8; 32] = raw.try_into().map_err(|_| anyhow!("coffre invalide"))?;
    Ok(SigningKey::from_bytes(&bytes))
}

fn pow_hash(pseudo: &str, pubkey: &[u8; 32], nonce: u64) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"minelib/pow");
    h.update(pseudo.to_ascii_lowercase().as_bytes());
    h.update(pubkey);
    h.update(nonce.to_le_bytes());
    h.finalize().into()
}

fn leading_zeros(h: &[u8; 32]) -> u32 {
    let mut n = 0;
    for b in h {
        if *b == 0 {
            n += 8;
        } else {
            n += b.leading_zeros();
            break;
        }
    }
    n
}

/// La preuve de travail `nonce` atteint-elle `bits` bits à zéro ?
pub fn pow_ok(pseudo: &str, pubkey: &[u8; 32], nonce: u64, bits: u8) -> bool {
    leading_zeros(&pow_hash(pseudo, pubkey, nonce)) >= bits as u32
}

/// Cherche une preuve de travail (quelques dixièmes de seconde pour 20 bits).
pub fn pow_solve(pseudo: &str, pubkey: &[u8; 32], bits: u8) -> u64 {
    let mut nonce = rand::random::<u64>() >> 8;
    while !pow_ok(pseudo, pubkey, nonce, bits) {
        nonce = nonce.wrapping_add(1);
    }
    nonce
}

/// Empreinte courte d'une clé publique (affichage, ex. « 7Q3K-M2XD »).
pub fn fingerprint(pubkey: &[u8; 32]) -> String {
    let h: [u8; 32] = Sha256::digest(pubkey).into();
    let s = crate::totp::base32(&h[..5]);
    format!("{}-{}", &s[..4], &s[4..8])
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, Verifier};

    #[test]
    fn vault_opens_only_with_the_right_password() {
        let id = new_identity();
        let k = derive("Viendax", "un bon mot de passe").unwrap();
        let vault = seal_identity(&k, &id);
        let again = derive("viendax", "un bon mot de passe").unwrap();
        assert_eq!(
            k.auth, again.auth,
            "pseudo sans tenir compte des majuscules"
        );
        let back = open_identity(&again, &vault).unwrap();
        assert_eq!(back.to_bytes(), id.to_bytes());
        let wrong = derive("Viendax", "mauvais mot de passe").unwrap();
        assert!(open_identity(&wrong, &vault).is_err());
        assert_ne!(k.auth, k.enc);
        let sig = back.sign(b"defi");
        assert!(id.verifying_key().verify(b"defi", &sig).is_ok());
    }

    #[test]
    fn proof_of_work_is_checked() {
        let pk = [7u8; 32];
        let n = pow_solve("abc", &pk, 10);
        assert!(pow_ok("abc", &pk, n, 10));
        assert!(pow_ok("ABC", &pk, n, 10));
        assert!(!pow_ok("abd", &pk, n, 30));
        assert_eq!(unhex(&hex(&[1, 255, 16])).unwrap(), vec![1, 255, 16]);
        assert_eq!(fingerprint(&pk).len(), 9);
    }
}
