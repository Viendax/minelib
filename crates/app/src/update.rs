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

use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};

/// Numéro de cette version (l'étiquette GitHub est `v` suivi de ce nombre).
pub const VERSION: u32 = 69;
const API: &str = "https://api.github.com/repos/Viendax/minelib/releases/latest";
const EVERY: Duration = Duration::from_secs(600);

#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub version: u32,
    pub url: String,
    pub size: u64,
}

/// Version annoncée par une étiquette (`v63`, `v63.0`, `63`).
pub fn parse_tag(tag: &str) -> Option<u32> {
    let t = tag.trim().trim_start_matches(['v', 'V']);
    t.split(['.', '-']).next()?.parse().ok()
}

/// Lit la réponse de l'API GitHub : version et exécutable du jeu (pas celui du relais).
pub fn parse_release(json: &str) -> Option<Release> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let version = parse_tag(v.get("tag_name")?.as_str()?)?;
    let asset = v.get("assets")?.as_array()?.iter().find(|a| {
        let name = a
            .get("name")
            .and_then(|n| n.as_str())
            .unwrap_or("")
            .to_lowercase();
        name.ends_with(".exe") && name.contains("minelib") && !name.contains("relay")
    })?;
    Some(Release {
        version,
        url: asset.get("browser_download_url")?.as_str()?.to_string(),
        size: asset.get("size").and_then(|s| s.as_u64()).unwrap_or(0),
    })
}

fn fetch_latest() -> Result<Option<Release>> {
    let mut resp = ureq::get(API)
        .header("User-Agent", "minelib-updater")
        .header("Accept", "application/vnd.github+json")
        .call()
        .context("GitHub injoignable")?;
    let text = resp.body_mut().read_to_string()?;
    Ok(parse_release(&text).filter(|r| r.version > VERSION))
}

fn download(r: &Release) -> Result<Vec<u8>> {
    let mut resp = ureq::get(&r.url)
        .header("User-Agent", "minelib-updater")
        .call()
        .context("téléchargement")?;
    let bytes = resp
        .body_mut()
        .with_config()
        .limit(512 * 1024 * 1024)
        .read_to_vec()?;
    // Un exécutable Windows commence par « MZ ».
    if bytes.len() < 1024 || &bytes[..2] != b"MZ" || (r.size > 0 && bytes.len() as u64 != r.size) {
        return Err(anyhow!("fichier reçu invalide"));
    }
    Ok(bytes)
}

/// Remplace l'exécutable en cours par `bytes` et lance la nouvelle version.
fn install_and_restart(bytes: &[u8]) -> Result<()> {
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

enum Msg {
    Found(Release),
    Downloaded(Vec<u8>),
    Failed(String),
}

/// État de la mise à jour, interrogé à chaque image.
pub struct Updater {
    enabled: bool,
    rx: Option<Receiver<Msg>>,
    next_check: Instant,
    pub available: Option<Release>,
    /// Message à afficher (téléchargement, erreur).
    pub status: Option<String>,
    downloading: bool,
}

impl Updater {
    pub fn new() -> Self {
        let enabled = cfg!(windows) && std::env::var_os("MINELIB_NO_UPDATE").is_none();
        if enabled {
            cleanup();
        }
        Self {
            enabled,
            rx: None,
            next_check: Instant::now(),
            available: None,
            status: None,
            downloading: false,
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
        if self.rx.is_none() && !self.downloading && Instant::now() >= self.next_check {
            self.next_check = Instant::now() + EVERY;
            self.spawn(|| match fetch_latest() {
                Ok(Some(r)) => Msg::Found(r),
                Ok(None) => Msg::Failed(String::new()),
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
                if fresh {
                    self.start();
                }
            }
            Msg::Downloaded(bytes) => {
                if let Err(e) = install_and_restart(&bytes) {
                    self.downloading = false;
                    self.status = Some(format!("Mise à jour impossible : {e:#}"));
                }
            }
            Msg::Failed(e) => {
                self.downloading = false;
                if !e.is_empty() {
                    log::info!("mise à jour : {e}");
                    if self.available.is_some() {
                        self.status = Some(format!("Mise à jour impossible : {e}"));
                    }
                }
            }
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
        self.downloading = true;
        self.status = Some(format!("Mise à jour vers la version {}…", r.version));
        self.spawn(move || match download(&r) {
            Ok(b) => Msg::Downloaded(b),
            Err(e) => Msg::Failed(format!("{e:#}")),
        });
    }

    pub fn busy(&self) -> bool {
        self.downloading
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
    }
}
