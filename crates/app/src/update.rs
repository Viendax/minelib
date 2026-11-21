//! Mise à jour automatique depuis les versions publiées sur GitHub (« Releases » de
//! `Viendax/minelib`, étiquette `vNN`, avec l'exécutable du jeu en pièce jointe).
//!
//! - Au lancement : s'il existe une version plus récente, le jeu la télécharge, prend sa
//!   place et redémarre dessus avant même qu'on ait joué.
//! - Pendant le jeu : vérification toutes les 10 minutes ; une version trouvée fait
//!   apparaître le bouton « Mettre à jour » sur l'accueil.
//!
//! Remplacement sous Windows : l'exécutable en cours est renommé (`….old`), le nouveau
//! prend son nom, il est lancé, et l'ancien est effacé au démarrage suivant.
//! `MINELIB_NO_UPDATE=1` désactive tout.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use sha2::{Digest, Sha256};

/// Orange des messages « mise à jour disponible ».
pub const ORANGE: [f32; 4] = [1.0, 0.55, 0.1, 1.0];

/// Fichier tenu pendant une partie FPS en ligne (dans le dossier du jeu) : version et
/// début. Le jeu ne se met pas à jour tant qu'il est là, le lanceur non plus (une partie
/// commencée dans une version se finit dans cette version).
pub const MATCH_LOCK: &str = "partie_en_cours.txt";
/// Au-delà, le fichier est oublié (partie abandonnée).
pub const MATCH_LOCK_TTL: u64 = 3 * 3600;

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Partie en cours notée dans `root` : sa version.
pub fn match_lock(root: &std::path::Path) -> Option<u32> {
    let text = std::fs::read_to_string(root.join(MATCH_LOCK)).ok()?;
    let get = |k: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(k)?.strip_prefix('='))
            .and_then(|v| v.trim().parse::<u64>().ok())
    };
    let (v, t) = (get("version")?, get("debut")?);
    (unix_now().saturating_sub(t) < MATCH_LOCK_TTL).then_some(v as u32)
}

pub fn write_match_lock(root: &std::path::Path) {
    let _ = std::fs::write(
        root.join(MATCH_LOCK),
        format!("version={VERSION}\ndebut={}\n", unix_now()),
    );
}

pub fn clear_match_lock(root: &std::path::Path) {
    let _ = std::fs::remove_file(root.join(MATCH_LOCK));
}

/// Numéro de cette version (l'étiquette GitHub est `v` suivi de ce nombre).
pub const VERSION: u32 = 212;
const REPO: &str = "https://api.github.com/repos/Viendax/minelib/releases";
const EVERY: Duration = Duration::from_secs(120);

#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub version: u32,
    pub url: String,
    pub size: u64,
    /// Empreinte SHA-256 publiée avec l'exécutable (fichier `.sha256`), si elle existe.
    pub sha256_url: Option<String>,
}

/// Client HTTPS : certificats du système (Windows), pour passer aussi derrière un
/// antivirus ou un réseau d'entreprise qui inspecte les connexions (sinon « certificat
/// inconnu » et le téléchargement échoue).
pub(crate) fn http() -> ureq::Agent {
    use ureq::tls::{RootCerts, TlsConfig};
    ureq::Agent::config_builder()
        .tls_config(
            TlsConfig::builder()
                .root_certs(RootCerts::PlatformVerifier)
                .build(),
        )
        .build()
        .into()
}

/// Version annoncée par une étiquette (`v63`, `v63.0`, `63`).
pub fn parse_tag(tag: &str) -> Option<u32> {
    let t = tag.trim().trim_start_matches(['v', 'V']);
    t.split(['.', '-']).next()?.parse().ok()
}

/// Lit la réponse de l'API GitHub : version et exécutable du jeu (pas celui du relais),
/// avec son empreinte si elle est publiée.
pub fn parse_release(json: &str) -> Option<Release> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let version = parse_tag(v.get("tag_name")?.as_str()?)?;
    let assets = v.get("assets")?.as_array()?;
    let name_of = |a: &serde_json::Value| {
        a.get("name")
            .and_then(|n| n.as_str())
            .unwrap_or("")
            .to_lowercase()
    };
    let asset = assets.iter().find(|a| {
        let name = name_of(a);
        name.ends_with(".exe")
            && name.contains("minelib")
            && !name.contains("relay")
            && !name.contains("launcher")
    })?;
    let exe_name = name_of(asset);
    let sha256_url = assets
        .iter()
        .find(|a| name_of(a) == format!("{exe_name}.sha256"))
        .and_then(|a| a.get("browser_download_url")?.as_str())
        .map(str::to_string);
    Some(Release {
        version,
        url: asset.get("browser_download_url")?.as_str()?.to_string(),
        size: asset.get("size").and_then(|s| s.as_u64()).unwrap_or(0),
        sha256_url,
    })
}

/// Ce que dit GitHub de la dernière version.
enum Latest {
    Newer(Release),
    UpToDate,
    /// Aucune version publiée (le dépôt n'a pas encore de Release).
    NoRelease,
}

fn get(url: &str) -> std::result::Result<String, ureq::Error> {
    let mut resp = http()
        .get(url)
        .header("User-Agent", "minelib-updater")
        .header("Accept", "application/vnd.github+json")
        .call()?;
    resp.body_mut().read_to_string()
}

fn fetch_latest() -> Result<Latest> {
    match get(&format!("{REPO}/latest")) {
        Ok(text) => Ok(match parse_release(&text) {
            Some(r) if r.version > VERSION => Latest::Newer(r),
            _ => Latest::UpToDate,
        }),
        Err(ureq::Error::StatusCode(404)) => Ok(Latest::NoRelease),
        Err(ureq::Error::StatusCode(403)) => {
            Err(anyhow!("GitHub limite les demandes, réessai plus tard"))
        }
        Err(e) => Err(anyhow!(e)).context("GitHub injoignable"),
    }
}

/// Empreinte SHA-256 publiée (64 chiffres hexadécimaux au début du fichier).
fn fetch_sha256(url: &str) -> Result<[u8; 32]> {
    let text = get(url).context("empreinte")?;
    parse_sha256(&text).ok_or_else(|| anyhow!("empreinte illisible"))
}

pub fn parse_sha256(text: &str) -> Option<[u8; 32]> {
    let hex = text.split_whitespace().next()?;
    if hex.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(hex.get(2 * i..2 * i + 2)?, 16).ok()?;
    }
    Some(out)
}

/// Télécharge l'exécutable en comptant les octets reçus (`done`), puis le vérifie : un
/// exécutable Windows, de la taille annoncée, et de l'empreinte publiée quand il y en a une.
fn download(r: &Release, done: &AtomicU64) -> Result<Vec<u8>> {
    use std::io::Read;
    let expected = match &r.sha256_url {
        Some(u) => Some(fetch_sha256(u)?),
        None => None,
    };
    let mut resp = http()
        .get(&r.url)
        .header("User-Agent", "minelib-updater")
        .call()
        .context("téléchargement")?;
    let mut reader = resp
        .body_mut()
        .with_config()
        .limit(512 * 1024 * 1024)
        .reader();
    let mut bytes = Vec::with_capacity(r.size as usize);
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut buf).context("téléchargement interrompu")?;
        if n == 0 {
            break;
        }
        bytes.extend_from_slice(&buf[..n]);
        done.store(bytes.len() as u64, Ordering::Relaxed);
    }
    if bytes.len() < 1024 || &bytes[..2] != b"MZ" || (r.size > 0 && bytes.len() as u64 != r.size) {
        return Err(anyhow!("fichier reçu invalide"));
    }
    if let Some(want) = expected {
        let got: [u8; 32] = Sha256::digest(&bytes).into();
        if got != want {
            return Err(anyhow!(
                "empreinte différente de celle publiée : fichier refusé"
            ));
        }
    }
    Ok(bytes)
}

/// Remplace l'exécutable en cours par `bytes` et lance la nouvelle version.
fn install_and_restart(bytes: &[u8], version: u32) -> Result<()> {
    let exe = std::env::current_exe()?;
    let new = exe.with_extension("new");
    let old = exe.with_extension("old");
    std::fs::write(&new, bytes).context("écriture de la nouvelle version")?;
    let _ = std::fs::remove_file(&old);
    std::fs::rename(&exe, &old).context("mise de côté de l'ancienne version")?;
    if let Err(e) = std::fs::rename(&new, &exe) {
        let _ = std::fs::rename(&old, &exe);
        return Err(e).context("mise en place de la nouvelle version");
    }
    // Jeu installé par le lanceur : il lit la version dans `version.txt`, à côté.
    if let Some(v) = exe
        .parent()
        .map(|d| d.join("version.txt"))
        .filter(|v| v.exists())
    {
        let _ = std::fs::write(v, version.to_string());
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::Command::new(&exe)
        .args(args)
        .spawn()
        .context("lancement de la nouvelle version")?;
    log::info!("mise à jour installée, redémarrage");
    std::process::exit(0);
}

/// Efface l'ancienne version laissée par une mise à jour.
pub fn cleanup() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::fs::remove_file(exe.with_extension("old"));
        let _ = std::fs::remove_file(exe.with_extension("new"));
    }
}

/// L'exécutable en cours est-il celui publié pour cette version ? Une vérification
/// honnête : elle repère un fichier abîmé ou modifié, elle n'empêche pas un tricheur
/// décidé de mentir (c'est le rôle des garants, qui rejouent les parties).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Integrity {
    Unknown,
    /// Identique à la version officielle.
    Official,
    /// Différent de la version officielle (modifié, abîmé, ou compilé soi-même).
    Modified,
    /// Pas d'empreinte publiée pour cette version.
    Unpublished,
}

fn self_check() -> Integrity {
    let Ok(exe) = std::env::current_exe() else {
        return Integrity::Unknown;
    };
    let Ok(bytes) = std::fs::read(&exe) else {
        return Integrity::Unknown;
    };
    let mine: [u8; 32] = Sha256::digest(&bytes).into();
    let Ok(text) = get(&format!("{REPO}/tags/v{VERSION}")) else {
        return Integrity::Unpublished;
    };
    let Some(url) = parse_release(&text).and_then(|r| r.sha256_url) else {
        return Integrity::Unpublished;
    };
    match fetch_sha256(&url) {
        Ok(h) if h == mine => Integrity::Official,
        Ok(_) => Integrity::Modified,
        Err(_) => Integrity::Unknown,
    }
}

enum Msg {
    Found(Release),
    UpToDate,
    NoRelease,
    Downloaded(Vec<u8>),
    Failed(String),
    Integrity(Integrity),
}

/// État de la mise à jour, interrogé à chaque image.
pub struct Updater {
    enabled: bool,
    rx: Option<Receiver<Msg>>,
    /// Vérification de l'exécutable (fil à part).
    integrity_rx: Option<Receiver<Msg>>,
    next_check: Instant,
    pub available: Option<Release>,
    /// Message à afficher (téléchargement, erreur).
    pub status: Option<String>,
    downloading: bool,
    /// Octets reçus du téléchargement en cours.
    done: Arc<AtomicU64>,
    /// Résultat de la dernière vérification.
    pub check: Check,
    pub integrity: Integrity,
    /// Partie FPS en ligne en cours dans cette version : pas de mise à jour avant sa fin.
    pub hold: Option<u32>,
}

/// Ce que l'on sait de la version : pas encore vérifiée, à jour, ou impossible à vérifier.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Check {
    Pending,
    UpToDate,
    Unreachable,
    /// Aucune version publiée sur GitHub.
    NoRelease,
    /// Mises à jour coupées (`MINELIB_NO_UPDATE`, ou pas sous Windows).
    Off,
    /// Lancé par le lanceur, qui l'a mis à jour juste avant.
    Launcher,
}

impl Updater {
    pub fn new() -> Self {
        // Lancé par le lanceur : c'est lui qui tient le jeu à jour depuis GitHub.
        let by_launcher = std::env::var_os("MINELIB_LAUNCHER").is_some();
        // Le lanceur met le jeu à jour avant de le lancer ; ensuite, comme sans lanceur, le
        // jeu vérifie toutes les 10 minutes et propose « Mettre à jour » sur l'accueil.
        let enabled = cfg!(windows) && std::env::var_os("MINELIB_NO_UPDATE").is_none();
        let mut integrity_rx = None;
        if enabled {
            cleanup();
            let (tx, rx) = channel();
            integrity_rx = Some(rx);
            let _ = std::thread::Builder::new()
                .name("vérification".into())
                .spawn(move || {
                    let _ = tx.send(Msg::Integrity(self_check()));
                });
        }
        Self {
            enabled,
            rx: None,
            integrity_rx,
            // Tout juste mis à jour par le lanceur : première vérification un peu plus tard.
            next_check: Instant::now()
                + if by_launcher {
                    Duration::from_secs(60)
                } else {
                    Duration::ZERO
                },
            available: None,
            status: None,
            downloading: false,
            done: Arc::new(AtomicU64::new(0)),
            check: if by_launcher {
                Check::Launcher
            } else if enabled {
                Check::Pending
            } else {
                Check::Off
            },
            integrity: Integrity::Unknown,
            hold: None,
        }
    }

    fn spawn(&mut self, f: impl FnOnce() -> Msg + Send + 'static) {
        let (tx, rx) = channel();
        self.rx = Some(rx);
        let _ = std::thread::Builder::new()
            .name("mise à jour".into())
            .spawn(move || {
                let _ = tx.send(f());
            });
    }

    /// À chaque image. `fresh` : le joueur n'a encore rien lancé (mise à jour immédiate).
    pub fn poll(&mut self, fresh: bool) {
        if !self.enabled {
            return;
        }
        if let Some(rx) = &self.integrity_rx {
            if let Ok(Msg::Integrity(i)) = rx.try_recv() {
                self.integrity = i;
                self.integrity_rx = None;
                if i == Integrity::Modified {
                    log::warn!("cet exécutable n'est pas celui publié pour la version {VERSION}");
                }
            }
        }
        if self.rx.is_none() && !self.downloading && Instant::now() >= self.next_check {
            self.next_check = Instant::now() + EVERY;
            self.spawn(|| match fetch_latest() {
                Ok(Latest::Newer(r)) => Msg::Found(r),
                Ok(Latest::UpToDate) => Msg::UpToDate,
                Ok(Latest::NoRelease) => Msg::NoRelease,
                Err(e) => Msg::Failed(format!("{e:#}")),
            });
        }
        let Some(rx) = &self.rx else { return };
        let Ok(msg) = rx.try_recv() else { return };
        self.rx = None;
        match msg {
            Msg::Found(r) => {
                log::info!("version {} disponible (ici {VERSION})", r.version);
                self.available = Some(r);
                if fresh && self.hold.is_none() {
                    self.start();
                }
            }
            Msg::UpToDate => {
                static SAID: std::sync::atomic::AtomicBool =
                    std::sync::atomic::AtomicBool::new(false);
                if !SAID.swap(true, Ordering::Relaxed) {
                    log::info!("mises à jour : version {VERSION} à jour");
                }
                if self.check != Check::Launcher {
                    self.check = Check::UpToDate;
                }
            }
            Msg::NoRelease => self.check = Check::NoRelease,
            Msg::Downloaded(bytes) => {
                let version = self.available.as_ref().map_or(VERSION, |r| r.version);
                if let Err(e) = install_and_restart(&bytes, version) {
                    self.downloading = false;
                    self.status = Some(format!("Mise à jour impossible : {e:#}"));
                }
            }
            Msg::Failed(e) => {
                if !self.downloading && self.available.is_none() && self.check != Check::Launcher {
                    self.check = Check::Unreachable;
                }
                let was_downloading = self.downloading;
                self.downloading = false;
                log::info!("mise à jour : {e}");
                if was_downloading || self.available.is_some() {
                    self.status = Some(format!("Mise à jour impossible : {e}"));
                }
            }
            Msg::Integrity(_) => {}
        }
    }

    /// Bouton « Mettre à jour » (ou lancement) : télécharge puis redémarre.
    pub fn start(&mut self) {
        let Some(r) = self.available.clone() else {
            return;
        };
        if self.downloading {
            return;
        }
        if let Some(v) = self.hold {
            self.status = Some(format!(
                "Partie en cours en version {v} : mise à jour après la fin de la partie."
            ));
            return;
        }
        self.downloading = true;
        self.done.store(0, Ordering::Relaxed);
        self.status = Some(format!("Mise à jour vers la version {}…", r.version));
        let done = self.done.clone();
        self.spawn(move || match download(&r, &done) {
            Ok(b) => Msg::Downloaded(b),
            Err(e) => Msg::Failed(format!("{e:#}")),
        });
    }

    pub fn busy(&self) -> bool {
        self.downloading
    }

    /// Ligne « version » à afficher (accueil, pause) et sa couleur.
    pub fn line(&self) -> (String, [f32; 4]) {
        if let Some(v) = self.hold.filter(|&v| v == VERSION) {
            return (
                format!("Minelib v{v} - bonne version pour finir ta partie en cours"),
                [0.4, 1.0, 0.4, 1.0],
            );
        }
        let (state, color) = match (&self.available, self.check) {
            (Some(r), _) => (
                format!("mise à jour disponible : version {}", r.version),
                ORANGE,
            ),
            (None, Check::UpToDate) => ("à jour".to_string(), [0.5, 1.0, 0.5, 1.0]),
            (None, Check::Pending) => ("vérification…".to_string(), [0.7, 0.7, 0.7, 1.0]),
            (None, Check::Unreachable) => (
                "version non vérifiée (GitHub injoignable)".to_string(),
                [1.0, 0.55, 0.4, 1.0],
            ),
            (None, Check::NoRelease) => (
                "aucune version publiée sur GitHub".to_string(),
                [0.8, 0.8, 0.6, 1.0],
            ),
            (None, Check::Off) => ("mises à jour désactivées".to_string(), [0.7, 0.7, 0.7, 1.0]),
            (None, Check::Launcher) => ("à jour (lanceur)".to_string(), [0.5, 1.0, 0.5, 1.0]),
        };
        let files = match self.integrity {
            Integrity::Official => " - fichiers vérifiés",
            Integrity::Modified => " - exécutable non officiel",
            Integrity::Unknown | Integrity::Unpublished => "",
        };
        let color = if self.integrity == Integrity::Modified {
            [1.0, 0.55, 0.4, 1.0]
        } else {
            color
        };
        (format!("Minelib v{VERSION} - {state}{files}"), color)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_github_release() {
        assert_eq!(parse_tag("v63"), Some(63));
        assert_eq!(parse_tag("V70.1"), Some(70));
        let json = r#"{"tag_name":"v63","assets":[
            {"name":"minelib-relay.exe","browser_download_url":"https://x/relay.exe","size":10},
            {"name":"minelib_v63.exe","browser_download_url":"https://x/game.exe","size":2048}]}"#;
        let r = parse_release(json).unwrap();
        assert_eq!(r.version, 63);
        assert_eq!(r.url, "https://x/game.exe");
        assert_eq!(r.size, 2048);
        assert_eq!(r.sha256_url, None);
        // Avec son empreinte publiée.
        let json = r#"{"tag_name":"v64","assets":[
            {"name":"minelib_v64.exe","browser_download_url":"https://x/g.exe","size":4096},
            {"name":"minelib_v64.exe.sha256","browser_download_url":"https://x/g.sha","size":65}]}"#;
        let r = parse_release(json).unwrap();
        assert_eq!(r.sha256_url.as_deref(), Some("https://x/g.sha"));
        let hex = "00".repeat(31) + "ff";
        assert_eq!(
            parse_sha256(&format!("{hex}  minelib.exe\n")).unwrap()[31],
            0xff
        );
        assert!(parse_sha256("pas une empreinte").is_none());
        assert!(parse_sha256(&"zz".repeat(32)).is_none());
    }
}
