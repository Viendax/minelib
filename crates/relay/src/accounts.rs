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
    /// Appareils de confiance (empreintes de leur jeton) : la 2FA ne leur est plus
    /// redemandée. Huit au plus, les plus récents.
    pub devices: Vec<[u8; 32]>,
    /// (v240) Clés d'accès (`PASSKEYS_MAX` au plus).
    pub passkeys: Vec<Passkey>,
}

/// (v240) Clés d'accès d'un compte, au plus.
pub const PASSKEYS_MAX: usize = 10;

/// (v240) Clé d'accès d'un compte.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Passkey {
    /// Identifiant donné par l'authentificateur.
    pub id: Vec<u8>,
    /// Clé publique (`minelib_net::passkey::CoseKey::to_bytes`).
    pub key: Vec<u8>,
    /// Compteur de signatures de l'authentificateur (0 : il n'en tient pas).
    pub count: u32,
    /// La clé privée du compte, chiffrée par une clé que seul l'authentificateur garde
    /// (dans l'identifiant d'utilisateur) : le relais ne peut pas l'ouvrir.
    pub vault: Vec<u8>,
    pub name: String,
    pub created: u64,
    pub used: u64,
}

/// Compte enregistré avant les clés d'accès (relu, puis réenregistré au nouveau format).
#[derive(Deserialize)]
struct AccountV2 {
    pseudo: String,
    pubkey: [u8; 32],
    auth_hash: String,
    vault: Vec<u8>,
    totp: Option<Vec<u8>>,
    totp_pending: Option<Vec<u8>>,
    last_totp_step: u64,
    created: u64,
    devices: Vec<[u8; 32]>,
}

/// Compte enregistré avant les appareils de confiance (relu, puis réenregistré au nouveau
/// format).
#[derive(Deserialize)]
struct AccountV1 {
    pseudo: String,
    pubkey: [u8; 32],
    auth_hash: String,
    vault: Vec<u8>,
    totp: Option<Vec<u8>>,
    totp_pending: Option<Vec<u8>>,
    last_totp_step: u64,
    created: u64,
}

impl Account {
    /// Jeton d'appareil de confiance : `true` s'il est connu.
    pub fn trusts(&self, token: &[u8; 32]) -> bool {
        let h: [u8; 32] = Sha256::digest(token).into();
        self.devices.contains(&h)
    }

    /// Nouvel appareil de confiance : renvoie son jeton (seule son empreinte est gardée).
    pub fn trust_new_device(&mut self) -> [u8; 32] {
        use rand::RngCore;
        let mut token = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut token);
        self.devices.push(Sha256::digest(token).into());
        if self.devices.len() > 8 {
            self.devices.remove(0);
        }
        token
    }

    /// Compte neuf ; `auth_hash` : sa preuve de mot de passe hachée (`hash_auth`, v276 :
    /// calculée hors du verrou du relais).
    pub fn with_hash(pseudo: &str, pubkey: [u8; 32], auth_hash: String, vault: Vec<u8>) -> Self {
        Self {
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
            devices: Vec::new(),
            passkeys: Vec::new(),
        }
    }
}

/// Argon2id de la preuve de mot de passe (format PHC). Plusieurs dizaines de millisecondes
/// de calcul : jamais en tenant l'état du relais (voir `prepare` dans lib.rs).
pub fn hash_auth(auth: &[u8; 32]) -> Result<String> {
    let salt = SaltString::generate(&mut rand::rngs::OsRng);
    Ok(Argon2::default()
        .hash_password(auth, &salt)
        .map_err(|e| anyhow!("{e}"))?
        .to_string())
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
        postcard::from_bytes(&bytes)
            .ok()
            .or_else(|| {
                let o: AccountV2 = postcard::from_bytes(&bytes).ok()?;
                Some(Account {
                    pseudo: o.pseudo,
                    pubkey: o.pubkey,
                    auth_hash: o.auth_hash,
                    vault: o.vault,
                    totp: o.totp,
                    totp_pending: o.totp_pending,
                    last_totp_step: o.last_totp_step,
                    created: o.created,
                    devices: o.devices,
                    passkeys: Vec::new(),
                })
            })
            .or_else(|| {
                let o: AccountV1 = postcard::from_bytes(&bytes).ok()?;
                Some(Account {
                    pseudo: o.pseudo,
                    pubkey: o.pubkey,
                    auth_hash: o.auth_hash,
                    vault: o.vault,
                    totp: o.totp,
                    totp_pending: o.totp_pending,
                    last_totp_step: o.last_totp_step,
                    created: o.created,
                    devices: Vec::new(),
                    passkeys: Vec::new(),
                })
            })
    }

    pub fn save(&self, a: &Account) -> Result<()> {
        write_atomic(&self.path(&a.pseudo), &postcard::to_allocvec(a)?)
    }

    pub fn remove(&self, pseudo: &str) {
        let _ = std::fs::remove_file(self.path(pseudo));
    }
}
