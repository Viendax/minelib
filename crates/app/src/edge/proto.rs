//! (v261) Le protocole entre le jeu et son navigateur : le même que celui de l'ancien
//! navigateur Chromium (`crates/browser`), pour que `screen.rs` n'ait rien à changer.
//!
//! **Entrée** (une commande par ligne, `<id>` = numéro de l'onglet) : `open <id> <adresse>`,
//! `close <id>`, `move|down|up <id> x y`, `wheel <id> x y dy`, `text <id> <texte>`,
//! `key <id> <nom>`, `blur <id>`, `eval <id> <javascript>`, `fps <id> <1..60>`,
//! `cote <id> <largeur> <hauteur>` (avant `open` : la recherche à côté d'un lecteur),
//! `volume 0 <0..100>`, `back <id>`, `forward <id>`, `quit`.
//!
//! **Sortie** : des enregistrements de 16 octets (étiquette, `id`, `a`, `b`) suivis de
//! leurs données : `MLBF` image BGRA `a` × `b`, `MLBK` champ de texte actif, `MLBS` état
//! de la page, `MLBW` défilement possible, `MLBH` pages précédente et suivante, `MLBX` page
//! refusée, `MLBO` ordre de la page.

use std::collections::{HashMap, VecDeque};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use super::filtre::{Mode, Restreint};

/// Images par seconde au plus.
pub const MAX_FPS: i32 = 60;

/// Réglages passés par le jeu en ligne de commande.
#[derive(Debug, Clone)]
pub struct Options {
    pub width: i32,
    pub height: i32,
    /// Dossier de la session du navigateur (cookies, cache).
    pub cache: Option<PathBuf>,
    /// Le jeu veut savoir où la page peut défiler (`MLBW`).
    pub scroll: bool,
    pub fps: i32,
    pub filtre: Mode,
    /// Dossier des listes (`liste_noire.txt`, listes de l'UT1 du jour).
    pub listes: Option<PathBuf>,
    pub restreint: Restreint,
    /// Volume des sons au lancement, en pour cent.
    pub volume: i32,
    /// Solo : le site complet de YouTube, jamais le lecteur embarqué.
    pub site: bool,
}

impl Options {
    pub fn parse(args: impl Iterator<Item = String>) -> Self {
        let mut o = Options {
            width: 1280,
            height: 720,
            cache: None,
            scroll: false,
            fps: 30,
            filtre: Mode::Noire,
            listes: None,
            restreint: Restreint::Non,
            volume: 50,
            site: false,
        };
        let mut args = args.peekable();
        while let Some(a) = args.next() {
            let mut value = || args.next().unwrap_or_default();
            match a.as_str() {
                "--width" => o.width = value().parse().unwrap_or(o.width).clamp(64, 3840),
                "--height" => o.height = value().parse().unwrap_or(o.height).clamp(64, 2160),
                "--cache" => o.cache = Some(PathBuf::from(value())),
                "--scroll" => o.scroll = true,
                "--fps" => o.fps = value().parse().unwrap_or(o.fps).clamp(1, MAX_FPS),
                "--filtre" => o.filtre = Mode::from_name(&value()).unwrap_or(o.filtre),
                "--listes" => o.listes = Some(PathBuf::from(value())),
                "--site-complet" => o.site = true,
                "--volume" => o.volume = value().parse().unwrap_or(o.volume).clamp(0, 100),
                "--restreint" => {
                    o.restreint = Restreint::from_name(&value()).unwrap_or(o.restreint)
                }
                _ => {}
            }
        }
        o
    }
}

/// Commande du jeu.
#[derive(Debug, PartialEq)]
pub enum Cmd {
    Open(u32, String),
    Close(u32),
    Move(u32, i32, i32),
    Down(u32, i32, i32),
    Up(u32, i32, i32),
    Wheel(u32, i32, i32, i32),
    Text(u32, String),
    Key(u32, String),
    Blur(u32),
    Eval(u32, String),
    Fps(u32, i32),
    Cote(u32, i32, i32),
    /// (v293) Largeur de mise en page de l'onglet (pixels) : plus étroite que l'onglet, la
    /// page se met en page dans sa partie gauche (le volet est à côté) ; sa largeur entière
    /// (ou 0) rend la normale.
    Largeur(u32, i32),
    Volume(i32),
    Back(u32),
    Forward(u32),
    Quit,
}

pub fn parse_cmd(line: &str) -> Option<Cmd> {
    let line = line.trim_end_matches(['\r', '\n']);
    if line.trim() == "quit" {
        return Some(Cmd::Quit);
    }
    let (word, rest) = line.split_once(' ')?;
    let (id, rest) = match rest.split_once(' ') {
        Some((id, rest)) => (id, rest),
        None => (rest, ""),
    };
    let id: u32 = id.parse().ok()?;
    let mut nums = rest.split_whitespace().map(|n| n.parse::<i32>().ok());
    let mut num = || nums.next().flatten();
    Some(match word {
        "open" => Cmd::Open(id, rest.trim().to_string()),
        "close" => Cmd::Close(id),
        "move" => Cmd::Move(id, num()?, num()?),
        "down" => Cmd::Down(id, num()?, num()?),
        "up" => Cmd::Up(id, num()?, num()?),
        "wheel" => Cmd::Wheel(id, num()?, num()?, num()?),
        "text" => Cmd::Text(id, rest.to_string()),
        "key" => Cmd::Key(id, rest.trim().to_string()),
        "blur" => Cmd::Blur(id),
        "eval" => Cmd::Eval(id, rest.to_string()),
        "fps" => Cmd::Fps(id, num()?),
        "cote" => Cmd::Cote(id, num()?, num()?),
        "largeur" => Cmd::Largeur(id, num()?),
        "volume" => Cmd::Volume(num()?),
        "back" => Cmd::Back(id),
        "forward" => Cmd::Forward(id),
        _ => return None,
    })
}

/// Une touche nommée en évènement du protocole des outils de développement de Chromium
/// (`Input.dispatchKeyEvent`) : code de touche Windows, nom, code, texte produit, et la
/// commande d'édition qui va avec un raccourci (`selectAll`, `paste`…). Le clavier passe
/// par là : il marche sans que la fenêtre du navigateur ait le focus de Windows (c'est le
/// jeu qui l'a).
pub fn key_event(name: &str) -> Option<(i32, &'static str, &'static str, Option<&'static str>, Option<&'static str>)> {
    Some(match name {
        "back" => (0x08, "Backspace", "Backspace", None, None),
        "tab" => (0x09, "Tab", "Tab", None, None),
        "enter" => (0x0D, "Enter", "Enter", Some("\r"), None),
        "end" => (0x23, "End", "End", None, None),
        "home" => (0x24, "Home", "Home", None, None),
        "left" => (0x25, "ArrowLeft", "ArrowLeft", None, None),
        "up" => (0x26, "ArrowUp", "ArrowUp", None, None),
        "right" => (0x27, "ArrowRight", "ArrowRight", None, None),
        "down" => (0x28, "ArrowDown", "ArrowDown", None, None),
        "delete" => (0x2E, "Delete", "Delete", None, None),
        "ctrl+a" => (0x41, "a", "KeyA", None, Some("selectAll")),
        "ctrl+c" => (0x43, "c", "KeyC", None, Some("copy")),
        "ctrl+v" => (0x56, "v", "KeyV", None, Some("paste")),
        "ctrl+x" => (0x58, "x", "KeyX", None, Some("cut")),
        "ctrl+y" => (0x59, "y", "KeyY", None, Some("redo")),
        "ctrl+z" => (0x5A, "z", "KeyZ", None, Some("undo")),
        _ => return None,
    })
}

/// Les deux appels `Input.dispatchKeyEvent` (appui, relâchement) d'une touche nommée, en
/// JSON.
pub fn key_json(name: &str) -> Option<[String; 2]> {
    let (vk, key, code, text, command) = key_event(name)?;
    let modifiers = if command.is_some() { 2 } else { 0 };
    let esc = |s: &str| serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into());
    let mut down = format!(
        "{{\"type\":\"{}\",\"windowsVirtualKeyCode\":{vk},\"nativeVirtualKeyCode\":{vk},\"key\":{},\"code\":{},\"modifiers\":{modifiers}",
        if text.is_some() { "keyDown" } else { "rawKeyDown" },
        esc(key),
        esc(code)
    );
    if let Some(t) = text {
        down.push_str(&format!(",\"text\":{0},\"unmodifiedText\":{0}", esc(t)));
    }
    if let Some(c) = command {
        down.push_str(&format!(",\"commands\":[{}]", esc(c)));
    }
    down.push('}');
    let up = format!(
        "{{\"type\":\"keyUp\",\"windowsVirtualKeyCode\":{vk},\"nativeVirtualKeyCode\":{vk},\"key\":{},\"code\":{},\"modifiers\":{modifiers}}}",
        esc(key),
        esc(code)
    );
    Some([down, up])
}

/// Ce qui attend d'être envoyé au jeu : la dernière image de chaque onglet (les images
/// arrivées pendant un envoi remplacent la précédente) et les évènements, dans l'ordre.
#[derive(Default)]
pub struct Outbox {
    pub frames: HashMap<u32, (u32, u32, Vec<u8>)>,
    pub events: VecDeque<([u8; 4], u32, u32, Vec<u8>)>,
    pub closed: bool,
    /// Images par seconde envoyées au plus, par onglet.
    pub fps: HashMap<u32, i32>,
}

pub type Shared = Arc<(Mutex<Outbox>, Condvar)>;

pub fn push_event(out: &Shared, tag: [u8; 4], id: u32, a: u32, data: Vec<u8>) {
    let (lock, cv) = &**out;
    if let Ok(mut o) = lock.lock() {
        o.events.push_back((tag, id, a, data));
        cv.notify_one();
    }
}

pub fn push_frame(out: &Shared, id: u32, w: u32, h: u32, px: Vec<u8>) {
    let (lock, cv) = &**out;
    if let Ok(mut o) = lock.lock() {
        o.frames.insert(id, (w, h, px));
        cv.notify_one();
    }
}

/// L'onglet `id` voudrait-il une image maintenant ? (Pas plus de ses images par seconde :
/// inutile de copier une image de la carte graphique pour rien.)
pub fn frame_due(out: &Shared, id: u32, last: Option<Instant>) -> bool {
    let fps = out
        .0
        .lock()
        .ok()
        .and_then(|o| o.fps.get(&id).copied())
        .unwrap_or(MAX_FPS)
        .clamp(1, MAX_FPS);
    let gap = Duration::from_secs_f64(1.0 / f64::from(fps)).saturating_sub(Duration::from_micros(1500));
    last.is_none_or(|t| t.elapsed() >= gap)
}

/// Envoie au jeu les évènements puis les images, au fur et à mesure, chaque onglet à ses
/// images par seconde.
pub fn send_out(out: Shared) {
    let mut w = std::io::stdout().lock();
    let (lock, cv) = &*out;
    let header = |w: &mut std::io::StdoutLock, tag: &[u8; 4], id: u32, a: u32, b: u32| {
        w.write_all(tag)?;
        w.write_all(&id.to_le_bytes())?;
        w.write_all(&a.to_le_bytes())?;
        w.write_all(&b.to_le_bytes())
    };
    let mut last: HashMap<u32, Instant> = HashMap::new();
    loop {
        let (events, frames) = {
            let Ok(mut o) = lock.lock() else { return };
            let due = |o: &Outbox, last: &HashMap<u32, Instant>| -> (Vec<u32>, Option<Duration>) {
                let now = Instant::now();
                let mut ids = Vec::new();
                let mut wait: Option<Duration> = None;
                for id in o.frames.keys() {
                    let fps = o.fps.get(id).copied().unwrap_or(MAX_FPS).clamp(1, MAX_FPS);
                    let gap = Duration::from_secs_f64(1.0 / f64::from(fps))
                        .saturating_sub(Duration::from_micros(1500));
                    match last.get(id).map(|t| *t + gap) {
                        Some(next) if next > now => {
                            let w = next - now;
                            wait = Some(wait.map_or(w, |x| x.min(w)));
                        }
                        _ => ids.push(*id),
                    }
                }
                (ids, wait)
            };
            let ids = loop {
                if o.closed {
                    return;
                }
                let (ids, wait) = due(&o, &last);
                if !ids.is_empty() || !o.events.is_empty() {
                    break ids;
                }
                o = match wait {
                    Some(w) => match cv.wait_timeout(o, w) {
                        Ok((o, _)) => o,
                        Err(_) => return,
                    },
                    None => match cv.wait(o) {
                        Ok(o) => o,
                        Err(_) => return,
                    },
                };
            };
            let frames: Vec<(u32, (u32, u32, Vec<u8>))> = ids
                .into_iter()
                .filter_map(|id| o.frames.remove(&id).map(|f| (id, f)))
                .collect();
            (std::mem::take(&mut o.events), frames)
        };
        let now = Instant::now();
        for (id, _) in &frames {
            last.insert(*id, now);
        }
        last.retain(|_, t| now.duration_since(*t) < Duration::from_secs(10));
        let mut sent: std::io::Result<()> = Ok(());
        for (tag, id, a, data) in events {
            sent = sent
                .and_then(|_| header(&mut w, &tag, id, a, 0))
                .and_then(|_| w.write_all(&data));
        }
        for (id, (fw, fh, pixels)) in frames {
            sent = sent
                .and_then(|_| header(&mut w, b"MLBF", id, fw, fh))
                .and_then(|_| w.write_all(&pixels));
        }
        if sent.and_then(|_| w.flush()).is_err() {
            // Le jeu ne lit plus : il est fermé.
            std::process::exit(0);
        }
    }
}

/// (v261) Un message d'une page (`chrome.webview.postMessage`) : ce qu'il faut dire au jeu.
#[derive(Debug, PartialEq)]
pub enum Message {
    Focus(bool),
    Scroll(u32),
    Diag(String),
    Ordre(String),
    Pub(bool),
    State(String),
}

pub fn parse_message(text: &str) -> Option<Message> {
    if let Some(v) = text.strip_prefix("__minelib_focus__") {
        Some(Message::Focus(v == "1"))
    } else if let Some(v) = text.strip_prefix("__minelib_scroll__") {
        Some(Message::Scroll(v.trim().parse::<u32>().unwrap_or(0) & 3))
    } else if let Some(d) = text.strip_prefix("__minelib_diag__") {
        Some(Message::Diag(d.chars().take(300).collect()))
    } else if let Some(o) = text.strip_prefix("__minelib_ordre__") {
        (o.len() <= 512).then(|| Message::Ordre(o.to_string()))
    } else if let Some(v) = text.strip_prefix("__minelib_pub__") {
        Some(Message::Pub(v.trim() == "1"))
    } else if let Some(s) = text.strip_prefix("__minelib_state__") {
        (s.len() <= 1 << 16).then(|| Message::State(s.to_string()))
    } else {
        None
    }
}

/// L'heure UTC, pour le journal.
pub fn heure() -> String {
    let s = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    let ms = s % 1000;
    let sec = (s / 1000) % 60;
    let min = (s / 60_000) % 60;
    let h = (s / 3_600_000) % 24;
    format!("{h:02}:{min:02}:{sec:02}.{ms:03}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lit_les_commandes_du_jeu() {
        assert_eq!(parse_cmd("move 3 10 20"), Some(Cmd::Move(3, 10, 20)));
        assert_eq!(parse_cmd("wheel 3 5 6 -120"), Some(Cmd::Wheel(3, 5, 6, -120)));
        assert_eq!(parse_cmd("cote 9 464 720"), Some(Cmd::Cote(9, 464, 720)));
        assert_eq!(parse_cmd("largeur 9 928"), Some(Cmd::Largeur(9, 928)));
        assert_eq!(
            parse_cmd("open 7 https://www.youtube.com/watch?v=x"),
            Some(Cmd::Open(7, "https://www.youtube.com/watch?v=x".into()))
        );
        assert_eq!(parse_cmd("text 7 chat  qui joue"), Some(Cmd::Text(7, "chat  qui joue".into())));
        assert_eq!(parse_cmd("volume 0 40"), Some(Cmd::Volume(40)));
        assert_eq!(parse_cmd("quit"), Some(Cmd::Quit));
        assert_eq!(parse_cmd("move 3 1"), None);
        assert_eq!(parse_cmd("inconnu 1"), None);
    }

    #[test]
    fn les_touches_deviennent_des_evenements_du_protocole() {
        let [down, up] = key_json("ctrl+v").unwrap();
        let d: serde_json::Value = serde_json::from_str(&down).unwrap();
        assert_eq!(d["type"], "rawKeyDown");
        assert_eq!(d["modifiers"], 2);
        assert_eq!(d["commands"][0], "paste");
        let u: serde_json::Value = serde_json::from_str(&up).unwrap();
        assert_eq!(u["type"], "keyUp");
        let [enter, _] = key_json("enter").unwrap();
        let e: serde_json::Value = serde_json::from_str(&enter).unwrap();
        assert_eq!((e["type"].as_str(), e["text"].as_str()), (Some("keyDown"), Some("\r")));
        assert!(key_json("f5").is_none());
    }

    #[test]
    fn lit_les_messages_des_pages() {
        assert_eq!(parse_message("__minelib_focus__1"), Some(Message::Focus(true)));
        assert_eq!(parse_message("__minelib_scroll__7"), Some(Message::Scroll(3)));
        assert_eq!(
            parse_message(r#"__minelib_ordre__{"clic":"abc"}"#),
            Some(Message::Ordre(r#"{"clic":"abc"}"#.into()))
        );
        assert_eq!(parse_message(&format!("__minelib_ordre__{}", "x".repeat(600))), None);
        assert_eq!(parse_message("bonjour"), None);
    }

    #[test]
    fn lit_les_options() {
        let o = Options::parse(
            ["--width", "1280", "--cache", "C:/x", "--scroll", "--site-complet", "--volume", "30"]
                .iter()
                .map(|s| s.to_string()),
        );
        assert!(o.scroll && o.site);
        assert_eq!(o.volume, 30);
        assert_eq!(o.cache, Some(PathBuf::from("C:/x")));
    }
}
