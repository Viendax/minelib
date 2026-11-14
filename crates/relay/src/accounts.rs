//! Partie privée des comptes, un fichier par compte : preuve de mot de passe (hachée une
//! seconde fois), coffre chiffré par le joueur, secret 2FA chiffré par le relais.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Account {
    pub pseudo: String,
    pub pubkey: [u8; 32],
    /// Argon2id de la preuve de mot de passe (format PHC).
    pub auth_hash: String,
    pub vault: Vec<u8>,
    pub totp: Option<Vec<u8>>,
    pub totp_pending: Option<Vec<u8>>,
    /// Dernière période 2FA utilisée (un code ne sert qu'une fois).
    pub last_totp_step: u64,
    pub created: u64,
}

impl Account {
    pub fn new(pseudo: &str, pubkey: [u8; 32], auth: &[u8; 32], vault: Vec<u8>) -> Result<Self> {
        let salt = SaltString::generate(&mut rand::rngs::OsRng);
        let auth_hash = Argon2::default()
            .hash_password(auth, &salt)
            .map_err(|e| anyhow!("{e}"))?
            .to_string();
        Ok(Self {
            pseudo: pseudo.to_string(),
            pubkey,
            auth_hash,
            vault,
            totp: None,
            totp_pending: None,
            last_totp_step: 0,
            created: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
        })
    }
}

/// Vérifie la preuve de mot de passe ; sans compte, fait le même travail sur un faux
/// hachage (même durée de réponse).
pub fn check_auth(hash: Option<&str>, auth: &[u8; 32]) -> bool {
    // Faux hachage calculé une fois, avec les mêmes réglages que les vrais.
    static DUMMY: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let dummy = DUMMY.get_or_init(|| {
        let salt = SaltString::generate(&mut rand::rngs::OsRng);
        Argon2::default()
            .hash_password(b"minelib", &salt)
            .map(|h| h.to_string())
            .unwrap_or_default()
    });
    let ok = PasswordHash::new(hash.unwrap_or(dummy))
        .map(|h| Argon2::default().verify_password(auth, &h).is_ok())
        .unwrap_or(false);
    ok && hash.is_some()
}

pub fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

pub struct Store {
    dir: PathBuf,
}

impl Store {
    pub fn new(dir: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&dir)?;
        Ok(Self { dir })
    }

    fn path(&self, pseudo: &str) -> PathBuf {
        let h = Sha256::digest(pseudo.to_ascii_lowercase().as_bytes());
        let name: String = h[..16].iter().map(|b| format!("{b:02x}")).collect();
        self.dir.join(format!("{name}.acct"))
    }

    pub fn load(&self, pseudo: &str) -> Option<Account> {
        let bytes = std::fs::read(self.path(pseudo)).ok()?;
        postcard::from_bytes(&bytes).ok()
    }

    pub fn save(&self, a: &Account) -> Result<()> {
        write_atomic(&self.path(&a.pseudo), &postcard::to_allocvec(a)?)
    }

    pub fn remove(&self, pseudo: &str) {
        let _ = std::fs::remove_file(self.path(pseudo));
    }
}
