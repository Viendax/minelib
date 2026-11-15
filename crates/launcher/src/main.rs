//! Lanceur de Minelib. Il ne contient pas le jeu : il demande à GitHub la dernière version
//! publiée (Releases de `Viendax/minelib`), la télécharge avec une barre de progression si
//! elle est plus récente que celle installée, vérifie son empreinte SHA-256 publiée, puis
//! lance le jeu et se ferme.
//!
//! Le jeu est rangé dans `%APPDATA%\.minelib\bin` (`MINELIB_HOME` pour un autre dossier),
//! avec `version.txt`. GitHub injoignable : la version déjà installée est lancée.

#![cfg_attr(windows, windows_subsystem = "windows")]

use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{anyhow, bail, Context, Result};
use sha2::{Digest, Sha256};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::window::{Window, WindowId};

const API: &str = "https://api.github.com/repos/Viendax/minelib/releases/latest";
const PAGE: &str = "https://github.com/Viendax/minelib/releases";

/// Une version publiée : numéro, exécutable du jeu, sa taille et son empreinte.
#[derive(Clone, Debug, PartialEq)]
struct Release {
    version: u32,
    url: String,
    size: u64,
    sha256_url: Option<String>,
}

/// Client HTTPS : certificats du système (Windows), pour passer aussi derrière un
/// antivirus ou un réseau d'entreprise qui inspecte les connexions (sinon « certificat
/// inconnu » et le téléchargement échoue).
fn http() -> ureq::Agent {
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

fn parse_tag(tag: &str) -> Option<u32> {
    let t = tag.trim().trim_start_matches(['v', 'V']);
    t.split(['.', '-']).next()?.parse().ok()
}

/// L'exécutable du jeu dans une Release (ni le relais, ni ce lanceur) et son empreinte.
fn parse_release(json: &str) -> Option<Release> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let version = parse_tag(v.get("tag_name")?.as_str()?)?;
    let assets = v.get("assets")?.as_array()?;
    let name_of = |a: &serde_json::Value| {
        a.get("name")
            .and_then(|n| n.as_str())
            .unwrap_or("")
            .to_lowercase()
    };
    let game = assets.iter().find(|a| {
        let n = name_of(a);
        n.ends_with(".exe") && n.contains("minelib") && !n.contains("relay") && !n.contains("launcher")
    })?;
    let exe = name_of(game);
    let sha256_url = assets
        .iter()
        .find(|a| name_of(a) == format!("{exe}.sha256"))
        .and_then(|a| a.get("browser_download_url")?.as_str())
        .map(str::to_string);
    Some(Release {
        version,
        url: game.get("browser_download_url")?.as_str()?.to_string(),
        size: game.get("size").and_then(|s| s.as_u64()).unwrap_or(0),
        sha256_url,
    })
}

fn parse_sha256(text: &str) -> Option<[u8; 32]> {
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

/// Ce que dit GitHub.
enum Latest {
    Found(Release),
    /// Aucune version publiée.
    None,
}

/// Que faire, selon la version installée et la réponse de GitHub.
#[derive(Debug, PartialEq)]
enum Plan {
    Download(Release),
    Launch,
    Fail(String),
}

fn plan(installed: Option<u32>, latest: std::result::Result<Latest, String>) -> Plan {
    match (installed, latest) {
        (Some(v), Ok(Latest::Found(r))) if r.version <= v => Plan::Launch,
        (_, Ok(Latest::Found(r))) => Plan::Download(r),
        (Some(_), _) => Plan::Launch,
        (None, Ok(Latest::None)) => Plan::Fail(
            "Aucune version du jeu n'est encore publiée sur GitHub.".into(),
        ),
        (None, Err(e)) => Plan::Fail(format!("GitHub injoignable : {e}")),
    }
}

fn get(url: &str) -> std::result::Result<String, ureq::Error> {
    http().get(url)
        .header("User-Agent", "minelib-launcher")
        .header("Accept", "application/vnd.github+json")
        .call()?
        .body_mut()
        .read_to_string()
}

fn fetch_latest() -> std::result::Result<Latest, String> {
    // Essais : une autre adresse que GitHub (`MINELIB_RELEASES_API`).
    let api = std::env::var("MINELIB_RELEASES_API").unwrap_or_else(|_| API.to_string());
    match get(&api) {
        Ok(text) => parse_release(&text)
            .map(Latest::Found)
            .ok_or_else(|| "réponse de GitHub illisible".to_string()),
        Err(ureq::Error::StatusCode(404)) => Ok(Latest::None),
        Err(ureq::Error::StatusCode(403)) => Err("trop de demandes, réessaie dans un moment".into()),
        Err(e) => Err(e.to_string()),
    }
}

/// Dossier du jeu installé.
fn bin_dir() -> PathBuf {
    let home = std::env::var_os("MINELIB_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join(".minelib")))
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".minelib")))
        .unwrap_or_else(|| PathBuf::from(".minelib"));
    home.join("bin")
}

fn game_path(dir: &Path) -> PathBuf {
    dir.join(if cfg!(windows) { "minelib.exe" } else { "minelib" })
}

fn installed(dir: &Path) -> Option<u32> {
    if !game_path(dir).exists() {
        return None;
    }
    std::fs::read_to_string(dir.join("version.txt"))
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// Télécharge et vérifie la nouvelle version, puis la met en place (l'ancienne reste
/// jusqu'au dernier moment : un téléchargement raté ne casse rien).
fn install(dir: &Path, r: &Release, state: &Shared) -> Result<()> {
    use std::io::Read;
    let want = match &r.sha256_url {
        Some(u) => Some(
            parse_sha256(&get(u).map_err(|e| anyhow!("empreinte : {e}"))?)
                .ok_or_else(|| anyhow!("empreinte publiée illisible"))?,
        ),
        None => None,
    };
    let mut resp = http().get(&r.url)
        .header("User-Agent", "minelib-launcher")
        .call()
        .map_err(|e| anyhow!("téléchargement : {e}"))?;
    let mut reader = resp
        .body_mut()
        .with_config()
        .limit(1024 * 1024 * 1024)
        .reader();
    let mut bytes = Vec::with_capacity(r.size as usize);
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut buf).context("téléchargement interrompu")?;
        if n == 0 {
            break;
        }
        bytes.extend_from_slice(&buf[..n]);
        state.lock().unwrap().phase = Phase::Downloading {
            version: r.version,
            done: bytes.len() as u64,
            total: r.size,
        };
    }
    if bytes.len() < 1024 || &bytes[..2] != b"MZ" || (r.size > 0 && bytes.len() as u64 != r.size) {
        bail!("fichier reçu invalide");
    }
    if let Some(want) = want {
        let got: [u8; 32] = Sha256::digest(&bytes).into();
        if got != want {
            bail!("l'empreinte ne correspond pas à celle publiée : fichier refusé");
        }
    }
    std::fs::create_dir_all(dir).context("dossier du jeu")?;
    let new = dir.join("minelib.new");
    std::fs::write(&new, &bytes).context("écriture du jeu")?;
    let exe = game_path(dir);
    let _ = std::fs::remove_file(&exe);
    std::fs::rename(&new, &exe).context("mise en place du jeu")?;
    std::fs::write(dir.join("version.txt"), r.version.to_string()).context("version.txt")?;
    Ok(())
}

/// Fichier que le jeu écrit quand sa fenêtre apparaît, entièrement chargée.
fn ready_path(dir: &Path) -> PathBuf {
    dir.join("pret")
}

/// Lance le jeu (qui sait alors qu'il est tenu à jour par le lanceur), puis attend qu'il
/// soit chargé : sa fenêtre n'apparaît qu'une fois prête, le lanceur reste affiché d'ici là.
fn launch(dir: &Path) -> Result<()> {
    let ready = ready_path(dir);
    let _ = std::fs::remove_file(&ready);
    let mut child = std::process::Command::new(game_path(dir))
        .current_dir(dir)
        .env("MINELIB_LAUNCHER", "1")
        .env("MINELIB_READY_FILE", &ready)
        .spawn()
        .context("lancement du jeu")?;
    let start = Instant::now();
    loop {
        if ready.exists() {
            let _ = std::fs::remove_file(&ready);
            return Ok(());
        }
        if let Ok(Some(status)) = child.try_wait() {
            bail!("le jeu s'est fermé pendant son chargement ({status}) : voir minelib.log");
        }
        // Très long : le jeu se montrera de lui-même, on n'attend plus.
        if start.elapsed().as_secs() > 120 {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

#[derive(Clone, Debug)]
enum Phase {
    Checking,
    Downloading { version: u32, done: u64, total: u64 },
    Launching,
    Failed(String),
}

type Shared = Arc<Mutex<State>>;

struct State {
    phase: Phase,
    done: bool,
}

/// Le travail du lanceur (fil à part) : GitHub, téléchargement, lancement.
fn work(state: Shared, proxy: EventLoopProxy<()>) {
    let dir = bin_dir();
    let have = installed(&dir);
    let result = match plan(have, fetch_latest()) {
        Plan::Download(r) => {
            state.lock().unwrap().phase = Phase::Downloading {
                version: r.version,
                done: 0,
                total: r.size,
            };
            match install(&dir, &r, &state) {
                Ok(()) => Ok(()),
                // Raté mais une version est déjà là : on joue avec.
                Err(_) if have.is_some() => Ok(()),
                Err(e) => Err(format!("{e:#}")),
            }
        }
        Plan::Launch => Ok(()),
        Plan::Fail(e) => Err(e),
    };
    if result.is_ok() {
        state.lock().unwrap().phase = Phase::Launching;
    }
    let next = match result.and_then(|()| launch(&dir).map_err(|e| format!("{e:#}"))) {
        Ok(()) => {
            state.lock().unwrap().done = true;
            Phase::Launching
        }
        Err(e) => Phase::Failed(e),
    };
    state.lock().unwrap().phase = next;
    let _ = proxy.send_event(());
}

struct App {
    state: Shared,
    proxy: EventLoopProxy<()>,
    window: Option<Arc<Window>>,
    surface: Option<softbuffer::Surface<Arc<Window>, Arc<Window>>>,
    started: Instant,
}

impl App {
    fn start_work(&self) {
        let (s, p) = (self.state.clone(), self.proxy.clone());
        std::thread::spawn(move || work(s, p));
    }

    fn draw(&mut self) {
        let (Some(window), Some(surface)) = (&self.window, &mut self.surface) else {
            return;
        };
        let size = window.inner_size();
        let (Some(w), Some(h)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
        else {
            return;
        };
        if surface.resize(w, h).is_err() {
            return;
        }
        let Ok(mut buf) = surface.buffer_mut() else {
            return;
        };
        let mut c = Canvas {
            px: &mut buf,
            w: size.width as usize,
            h: size.height as usize,
        };
        let scale = (size.height as usize / 100).max(1);
        c.fill(0, 0, c.w, c.h, 0x1d1e22);
        // Damier discret, comme le fond du jeu.
        let tile = 8 * scale;
        for ty in 0..c.h / tile + 1 {
            for tx in 0..c.w / tile + 1 {
                if (tx + ty) % 2 == 0 {
                    c.fill(tx * tile, ty * tile, tile, tile, 0x212227);
                }
            }
        }
        let cx = c.w / 2;
        c.text_center("MINELIB", cx, c.h / 6, scale * 3, 0xffd966);
        let phase = self.state.lock().unwrap().phase.clone();
        let (title, frac): (String, Option<f32>) = match &phase {
            Phase::Checking => ("Recherche de la dernière version…".into(), None),
            Phase::Downloading { version, done, total } => {
                let f = if *total > 0 {
                    (*done as f32 / *total as f32).min(1.0)
                } else {
                    0.0
                };
                let mo = |b: u64| format!("{:.1}", b as f64 / 1_048_576.0).replace('.', ",");
                (
                    format!(
                        "Version {version} : {} % ({} / {} Mo)",
                        (f * 100.0).round(),
                        mo(*done),
                        mo(*total)
                    ),
                    Some(f),
                )
            }
            Phase::Launching => ("Chargement du jeu et connexion au compte…".into(), None),
            Phase::Failed(e) => (e.clone(), None),
        };
        let failed = matches!(phase, Phase::Failed(_));
        let ts = scale.max(2) / 2 + usize::from(scale >= 3);
        let title = fit(&title, c.w.saturating_sub(16), ts);
        c.text_center(&title, cx, c.h / 2 - 12 * ts, ts, if failed { 0xff8a70 } else { 0xffffff });
        let (bw, bh) = (c.w * 3 / 4, 6 * scale);
        let (bx, by) = (cx - bw / 2, c.h / 2 + 4 * scale);
        c.fill(bx.saturating_sub(1), by.saturating_sub(1), bw + 2, bh + 2, 0x000000);
        c.fill(bx, by, bw, bh, 0x2c2d33);
        match frac {
            Some(f) => c.fill(bx, by, (bw as f32 * f) as usize, bh, 0x59cc66),
            None if !failed => {
                let t = (self.started.elapsed().as_secs_f32() * 1.5).sin() * 0.5 + 0.5;
                let seg = bw / 5;
                c.fill(bx + ((bw - seg) as f32 * t) as usize, by, seg, bh, 0x9999b3);
            }
            None => {}
        }
        if failed {
            c.text_center("Clic : réessayer", cx, by + bh + 8 * scale, ts, 0xbbbbbb);
            c.text_center(PAGE, cx, c.h - 12 * ts, ts.max(1), 0x777788);
        }
        let _ = buf.present();
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("Minelib")
            .with_inner_size(LogicalSize::new(560.0, 260.0))
            .with_resizable(false)
            .with_visible(false);
        let Ok(window) = el.create_window(attrs) else {
            el.exit();
            return;
        };
        let window = Arc::new(window);
        // Au centre de l'écran, puis montrée.
        if let Some(m) = window.current_monitor().or_else(|| el.primary_monitor()) {
            let (mp, ms, ws) = (m.position(), m.size(), window.outer_size());
            let x = mp.x + (ms.width as i32 - ws.width as i32) / 2;
            let y = mp.y + (ms.height as i32 - ws.height as i32) / 2;
            window.set_outer_position(winit::dpi::PhysicalPosition::new(x.max(mp.x), y.max(mp.y)));
        }
        window.set_visible(true);
        let surface = softbuffer::Context::new(window.clone())
            .and_then(|ctx| softbuffer::Surface::new(&ctx, window.clone()))
            .ok();
        self.window = Some(window);
        self.surface = surface;
        self.start_work();
    }

    fn user_event(&mut self, _el: &ActiveEventLoop, _e: ()) {
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => el.exit(),
            WindowEvent::RedrawRequested => self.draw(),
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let failed = matches!(self.state.lock().unwrap().phase, Phase::Failed(_));
                if failed {
                    self.state.lock().unwrap().phase = Phase::Checking;
                    self.start_work();
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        // Jeu chargé et affiché : le lanceur se ferme.
        if self.state.lock().unwrap().done {
            el.exit();
            return;
        }
        if let Some(w) = &self.window {
            w.request_redraw();
        }
        el.set_control_flow(ControlFlow::WaitUntil(
            Instant::now() + std::time::Duration::from_millis(33),
        ));
    }
}

/// Image de la fenêtre, en 0x00RRGGBB.
struct Canvas<'a> {
    px: &'a mut [u32],
    w: usize,
    h: usize,
}

impl Canvas<'_> {
    fn fill(&mut self, x: usize, y: usize, w: usize, h: usize, rgb: u32) {
        for yy in y.min(self.h)..(y + h).min(self.h) {
            let row = yy * self.w;
            for xx in x.min(self.w)..(x + w).min(self.w) {
                self.px[row + xx] = rgb;
            }
        }
    }

    /// Texte en police 8 × 8 (Latin-1), agrandi `s` fois, centré sur `cx`.
    fn text_center(&mut self, text: &str, cx: usize, y: usize, s: usize, rgb: u32) {
        let width = text.chars().count() * 8 * s;
        let mut x = cx.saturating_sub(width / 2);
        for ch in text.chars() {
            let g = glyph(ch);
            for (row, bits) in g.iter().enumerate() {
                for col in 0..8 {
                    if bits >> col & 1 == 1 {
                        // Ombre puis lettre.
                        self.fill(x + col * s + s / 2 + 1, y + row * s + s / 2 + 1, s, s, 0x0a0a0c);
                        self.fill(x + col * s, y + row * s, s, s, rgb);
                    }
                }
            }
            x += 8 * s;
        }
    }
}

fn glyph(ch: char) -> [u8; 8] {
    use font8x8::legacy::{BASIC_LEGACY, LATIN_LEGACY};
    match ch {
        '…' => [0, 0, 0, 0, 0, 0, 0b0100_1001, 0],
        c if (c as u32) < 128 => BASIC_LEGACY[c as usize],
        c if (0xA0..0x100).contains(&(c as u32)) => LATIN_LEGACY[c as usize - 0xA0],
        _ => BASIC_LEGACY['?' as usize],
    }
}

/// Coupe un texte trop long pour la fenêtre.
fn fit(text: &str, max_px: usize, s: usize) -> String {
    let max = (max_px / (8 * s)).max(4);
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut t: String = text.chars().take(max - 1).collect();
    t.push('…');
    t
}

fn main() {
    let Ok(event_loop) = EventLoop::<()>::with_user_event().build() else {
        return;
    };
    let mut app = App {
        state: Arc::new(Mutex::new(State {
            phase: Phase::Checking,
            done: false,
        })),
        proxy: event_loop.create_proxy(),
        window: None,
        surface: None,
        started: Instant::now(),
    };
    let _ = event_loop.run_app(&mut app);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rel(v: u32) -> Release {
        Release {
            version: v,
            url: "u".into(),
            size: 1,
            sha256_url: None,
        }
    }

    #[test]
    fn picks_the_game_not_the_relay_or_the_launcher() {
        let json = r#"{"tag_name":"v80","assets":[
            {"name":"minelib-launcher.exe","browser_download_url":"https://x/l.exe","size":1},
            {"name":"minelib-relay_v80.exe","browser_download_url":"https://x/r.exe","size":1},
            {"name":"minelib_v80.exe","browser_download_url":"https://x/g.exe","size":9},
            {"name":"minelib_v80.exe.sha256","browser_download_url":"https://x/g.sha","size":65}]}"#;
        let r = parse_release(json).unwrap();
        assert_eq!((r.version, r.url.as_str(), r.size), (80, "https://x/g.exe", 9));
        assert_eq!(r.sha256_url.as_deref(), Some("https://x/g.sha"));
        assert!(parse_sha256(&"ab".repeat(32)).is_some());
        assert!(parse_sha256("court").is_none());
    }

    #[test]
    fn decides_what_to_do() {
        assert_eq!(plan(None, Ok(Latest::Found(rel(80)))), Plan::Download(rel(80)));
        assert_eq!(plan(Some(79), Ok(Latest::Found(rel(80)))), Plan::Download(rel(80)));
        assert_eq!(plan(Some(80), Ok(Latest::Found(rel(80)))), Plan::Launch);
        // GitHub injoignable ou rien de publié : on joue avec ce qu'on a.
        assert_eq!(plan(Some(80), Err("x".into())), Plan::Launch);
        assert_eq!(plan(Some(80), Ok(Latest::None)), Plan::Launch);
        assert!(matches!(plan(None, Ok(Latest::None)), Plan::Fail(_)));
        assert!(matches!(plan(None, Err("x".into())), Plan::Fail(_)));
    }

    #[test]
    fn long_texts_are_cut() {
        assert_eq!(fit("court", 800, 1), "court");
        assert!(fit(&"x".repeat(200), 160, 1).chars().count() <= 20);
    }
}
