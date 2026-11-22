//! (v240) Clés d'accès, côté jeu : la fenêtre de Windows (Windows Hello, clé de sécurité,
//! téléphone), appelée dans un fil de fond (elle attend le joueur) ; ou, pour les essais, une
//! clé logicielle gardée dans le fichier `MINELIB_CLE_ESSAI` (sur n'importe quel système : ce
//! n'est pas une vraie clé d'accès).
//!
//! Ce que fait une clé d'accès pour un compte :
//! - à sa création (compte connecté), le jeu tire une clé de 32 octets, la met avec le pseudo
//!   dans l'identifiant d'utilisateur de la clé d'accès, et envoie au relais un coffre : les
//!   clés du compte (celles que donne le mot de passe) chiffrées par cette clé ;
//! - pour se connecter, Windows rend l'identifiant d'utilisateur (la clé du coffre, le pseudo)
//!   et la signature du défi du relais ; le relais vérifie, rend le coffre et fait de ce PC un
//!   appareil de confiance (pas de 2FA) ; le jeu ouvre le coffre et se connecte avec les clés
//!   du compte, comme avec le mot de passe (gardées ensuite pour la connexion automatique).

use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver};

use minelib_net::passkey;
use minelib_platform::passkey::{Assertion, Created};

/// Fichier de la clé logicielle des essais, s'il est demandé.
fn soft_file() -> Option<PathBuf> {
    std::env::var_os("MINELIB_CLE_ESSAI").map(PathBuf::from)
}

/// Peut-on se servir de clés d'accès ici ?
pub fn available() -> bool {
    soft_file().is_some() || minelib_platform::passkey::available()
}

/// Ce que rend le fil de fond.
pub enum Done {
    /// Clé créée (ou refusée), avec la clé de son coffre et les clés du compte à y mettre.
    Created {
        result: Result<Created, String>,
        key: [u8; 32],
        keys: Vec<u8>,
    },
    /// Clé donnée pour une connexion.
    Got(Result<Assertion, String>),
}

/// Crée une clé d'accès pour ce compte (dans un fil de fond) : les clés du compte viennent du
/// mot de passe gardé (ou sont déjà là, après une connexion par clé d'accès).
pub fn start_create(pseudo: String, password: String) -> Receiver<Done> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        use rand::RngCore;
        let mut key = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut key);
        let keys = match minelib_net::crypto::keys_from_password(&password) {
            Some(k) => Ok(k),
            None => minelib_net::crypto::derive(&pseudo, &password).map_err(|e| format!("{e:#}")),
        };
        let keys = match keys {
            Ok(k) => minelib_net::crypto::keys_to_bytes(&k),
            Err(e) => {
                let _ = tx.send(Done::Created {
                    result: Err(e),
                    key,
                    keys: Vec::new(),
                });
                return;
            }
        };
        let mut challenge = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut challenge);
        let handle = passkey::user_handle(&key, &pseudo);
        let result = match soft_file() {
            Some(f) => soft_create(&f, &handle),
            None => minelib_platform::passkey::create(
                passkey::RP_ID,
                passkey::RP_NAME,
                &handle,
                &pseudo,
                &passkey::client_data(true, &challenge),
                &[passkey::ES256, passkey::RS256],
            ),
        };
        let _ = tx.send(Done::Created { result, key, keys });
    });
    rx
}

/// Demande une clé d'accès pour ce défi du relais (dans un fil de fond).
pub fn start_get(challenge: [u8; 32]) -> Receiver<Done> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let r = match soft_file() {
            Some(f) => soft_get(&f, &challenge),
            None => minelib_platform::passkey::get(
                passkey::RP_ID,
                &passkey::client_data(false, &challenge),
            ),
        };
        let _ = tx.send(Done::Got(r));
    });
    rx
}

fn soft_create(file: &std::path::Path, handle: &[u8]) -> Result<Created, String> {
    let (k, authenticator_data) = passkey::SoftKey::create(handle);
    std::fs::write(file, k.to_bytes()).map_err(|e| format!("clé d'essai : {e}"))?;
    log::warn!("clé d'accès d'ESSAI (logicielle) créée dans {}", file.display());
    Ok(Created {
        id: k.id.clone(),
        authenticator_data,
    })
}

fn soft_get(file: &std::path::Path, challenge: &[u8; 32]) -> Result<Assertion, String> {
    let bytes = std::fs::read(file).map_err(|_| "Aucune clé d'accès de Minelib sur cet appareil.")?;
    let mut k = passkey::SoftKey::from_bytes(&bytes).ok_or("clé d'essai illisible")?;
    let (authenticator_data, signature) = k.sign(challenge);
    std::fs::write(file, k.to_bytes()).map_err(|e| format!("clé d'essai : {e}"))?;
    Ok(Assertion {
        id: k.id.clone(),
        authenticator_data,
        signature,
        user_handle: k.user_handle.clone(),
    })
}

/// Nom d'une nouvelle clé : le PC et le jour.
pub fn new_name() -> String {
    let pc = ["COMPUTERNAME", "HOSTNAME"]
        .iter()
        .find_map(|v| std::env::var(v).ok().filter(|s| !s.trim().is_empty()))
        .unwrap_or_else(|| "ce PC".into());
    format!("{} ({})", pc.trim(), crate::save::format_date(crate::save::now()))
}
