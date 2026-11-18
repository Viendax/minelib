//! Navigateur des écrans vidéo de Minelib : Chromium (CEF) dessiné hors écran.
//!
//! Le jeu lance ce programme (un seul pour tous ses écrans) et lui parle par ses entrées
//! et sorties standard :
//! - **entrée** : une commande par ligne, coordonnées en pixels de la page :
//!   `move x y`, `down x y`, `up x y`, `wheel x y dy`, `url <adresse>`, `quit` ;
//! - **sortie** : les images de la page, chacune précédée de `MLBF`, de la largeur et de
//!   la hauteur (u32 petit-boutistes), suivies de largeur × hauteur × 4 octets BGRA.
//!
//! Le jeu ne dépend jamais de CEF : sans ce programme et ses fichiers (`libcef.dll`…), ses
//! écrans montrent leur mire. Chromium relance aussi ce programme pour ses sous-processus
//! (rendu, GPU…) : `execute_process` les reconnaît et s'arrête là.
//!
//! C'est un navigateur ordinaire : le site de YouTube s'y affiche comme dans Chrome, avec
//! ses publicités, sans rien télécharger ni récupérer d'un autre navigateur. Sa session
//! (connexion à un compte, cookies) est la sienne, dans le dossier `--cache`.
#![windows_subsystem = "windows"]

use std::io::{BufRead, Write};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use cef::*;

/// Réglages passés par le jeu en ligne de commande.
struct Options {
    width: i32,
    height: i32,
    url: String,
    /// Dossier de la session du navigateur (cookies, cache).
    cache: Option<String>,
    /// Journal de Chromium.
    log: Option<String>,
}

impl Options {
    fn parse() -> Self {
        let mut o = Options {
            width: 1280,
            height: 720,
            url: "https://www.youtube.com/".into(),
            cache: None,
            log: None,
        };
        let mut args = std::env::args().skip(1);
        while let Some(a) = args.next() {
            let mut value = || args.next().unwrap_or_default();
            match a.as_str() {
                "--width" => o.width = value().parse().unwrap_or(o.width).clamp(64, 3840),
                "--height" => o.height = value().parse().unwrap_or(o.height).clamp(64, 2160),
                "--url" => o.url = value(),
                "--cache" => o.cache = Some(value()),
                "--log" => o.log = Some(value()),
                _ => {}
            }
        }
        o
    }
}

/// Commande du jeu.
enum Cmd {
    Move(i32, i32),
    Down(i32, i32),
    Up(i32, i32),
    Wheel(i32, i32, i32),
    Url(String),
    Quit,
}

fn parse_cmd(line: &str) -> Option<Cmd> {
    let mut it = line.split_whitespace();
    let word = it.next()?;
    let mut num = || it.next()?.parse::<i32>().ok();
    Some(match word {
        "move" => Cmd::Move(num()?, num()?),
        "down" => Cmd::Down(num()?, num()?),
        "up" => Cmd::Up(num()?, num()?),
        "wheel" => Cmd::Wheel(num()?, num()?, num()?),
        "url" => Cmd::Url(line.trim_start()[3..].trim().to_string()),
        "quit" => Cmd::Quit,
        _ => return None,
    })
}

/// Dernière image peinte, en attente d'être envoyée au jeu (les images peintes pendant un
/// envoi remplacent la précédente : le jeu reçoit toujours la plus récente).
#[derive(Default)]
struct Latest {
    frame: Option<(u32, u32, Vec<u8>)>,
    closed: bool,
}

type Shared = Arc<(Mutex<Latest>, Condvar)>;

wrap_render_handler! {
    struct Painter {
        size: (i32, i32),
        latest: Shared,
    }

    impl RenderHandler {
        fn view_rect(&self, _browser: Option<&mut Browser>, rect: Option<&mut Rect>) {
            if let Some(rect) = rect {
                *rect = Rect {
                    x: 0,
                    y: 0,
                    width: self.size.0,
                    height: self.size.1,
                };
            }
        }

        fn on_paint(
            &self,
            _browser: Option<&mut Browser>,
            type_: PaintElementType,
            _dirty_rects: Option<&[Rect]>,
            buffer: *const u8,
            width: ::std::os::raw::c_int,
            height: ::std::os::raw::c_int,
        ) {
            // Les menus déroulants (popups) ne sont pas encore dessinés.
            if type_ != PaintElementType::VIEW || buffer.is_null() || width <= 0 || height <= 0 {
                return;
            }
            let n = width as usize * height as usize * 4;
            // SAFETY : CEF fournit `width * height` pixels BGRA, valides pendant l'appel.
            let pixels = unsafe { std::slice::from_raw_parts(buffer, n) }.to_vec();
            let (lock, cv) = &*self.latest;
            if let Ok(mut l) = lock.lock() {
                l.frame = Some((width as u32, height as u32, pixels));
                cv.notify_one();
            }
        }
    }
}

wrap_client! {
    struct Browsing {
        painter: RenderHandler,
    }

    impl Client {
        fn render_handler(&self) -> Option<RenderHandler> {
            Some(self.painter.clone())
        }
    }
}

wrap_app! {
    struct BrowserApp;

    impl App {
        fn on_before_command_line_processing(
            &self,
            _process_type: Option<&CefString>,
            command_line: Option<&mut CommandLine>,
        ) {
            if let Some(cl) = command_line {
                // Une vidéo choisie sur l'écran se lance sans second clic.
                cl.append_switch_with_value(
                    Some(&CefString::from("autoplay-policy")),
                    Some(&CefString::from("no-user-gesture-required")),
                );
            }
        }
    }
}

fn mouse(x: i32, y: i32) -> MouseEvent {
    MouseEvent { x, y, modifiers: 0 }
}

/// Envoie les images au jeu, la plus récente à chaque fois.
fn send_frames(latest: Shared) {
    let mut out = std::io::stdout().lock();
    let (lock, cv) = &*latest;
    loop {
        let (w, h, pixels) = {
            let Ok(mut l) = lock.lock() else { return };
            while l.frame.is_none() && !l.closed {
                l = match cv.wait(l) {
                    Ok(l) => l,
                    Err(_) => return,
                };
            }
            if l.closed {
                return;
            }
            l.frame.take().expect("image présente")
        };
        let sent = out
            .write_all(b"MLBF")
            .and_then(|_| out.write_all(&w.to_le_bytes()))
            .and_then(|_| out.write_all(&h.to_le_bytes()))
            .and_then(|_| out.write_all(&pixels))
            .and_then(|_| out.flush());
        if sent.is_err() {
            return; // le jeu est fermé
        }
    }
}

fn main() {
    // Version de l'API de CEF dont ce programme a été compilé : avant tout autre appel.
    let _ = api_hash(sys::CEF_API_VERSION_LAST, 0);
    let args = args::Args::new();
    let mut app = BrowserApp::new();

    // Sous-processus de Chromium (rendu, GPU…) : il fait son travail puis s'arrête ici.
    let code = execute_process(
        Some(args.as_main_args()),
        Some(&mut app),
        std::ptr::null_mut(),
    );
    if code >= 0 {
        std::process::exit(code);
    }

    let opts = Options::parse();
    let mut settings = Settings {
        no_sandbox: 1,
        windowless_rendering_enabled: 1,
        persist_session_cookies: 1,
        locale: CefString::from("fr"),
        log_severity: LogSeverity::from(sys::cef_log_severity_t::LOGSEVERITY_WARNING),
        ..Default::default()
    };
    if let Some(cache) = &opts.cache {
        settings.root_cache_path = CefString::from(cache.as_str());
        settings.cache_path = CefString::from(cache.as_str());
    }
    if let Some(log) = &opts.log {
        settings.log_file = CefString::from(log.as_str());
    }
    if initialize(
        Some(args.as_main_args()),
        Some(&settings),
        Some(&mut app),
        std::ptr::null_mut(),
    ) != 1
    {
        eprintln!("minelib-browser : initialisation de CEF impossible");
        std::process::exit(2);
    }

    let latest: Shared = Arc::new((Mutex::new(Latest::default()), Condvar::new()));
    let painter = Painter::new((opts.width, opts.height), latest.clone());
    let mut client = Browsing::new(painter);
    let window_info = WindowInfo::default().set_as_windowless(sys::HWND::default());
    let browser_settings = BrowserSettings {
        windowless_frame_rate: 30,
        // Fond opaque : pas de transparence à gérer côté jeu.
        background_color: 0xff00_0000,
        ..Default::default()
    };
    let browser = browser_host_create_browser_sync(
        Some(&window_info),
        Some(&mut client),
        Some(&CefString::from(opts.url.as_str())),
        Some(&browser_settings),
        None,
        None,
    );
    let host = browser.as_ref().and_then(|b| b.host());
    if let Some(h) = &host {
        h.set_focus(1);
    }

    // Images vers le jeu.
    let writer = {
        let latest = latest.clone();
        std::thread::spawn(move || send_frames(latest))
    };
    // Commandes du jeu (fin de l'entrée : le jeu est fermé).
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if let Some(cmd) = parse_cmd(&line) {
                if tx.send(cmd).is_err() {
                    return;
                }
            }
        }
        let _ = tx.send(Cmd::Quit);
    });

    // Boucle de Chromium sur ce fil (le fil « UI » de CEF) : les clics y sont envoyés.
    'run: loop {
        do_message_loop_work();
        while let Ok(cmd) = rx.try_recv() {
            let Some(h) = &host else {
                if matches!(cmd, Cmd::Quit) {
                    break 'run;
                }
                continue;
            };
            let left = MouseButtonType::from(sys::cef_mouse_button_type_t::MBT_LEFT);
            match cmd {
                Cmd::Move(x, y) => h.send_mouse_move_event(Some(&mouse(x, y)), 0),
                Cmd::Down(x, y) => h.send_mouse_click_event(Some(&mouse(x, y)), left, 0, 1),
                Cmd::Up(x, y) => h.send_mouse_click_event(Some(&mouse(x, y)), left, 1, 1),
                Cmd::Wheel(x, y, dy) => h.send_mouse_wheel_event(Some(&mouse(x, y)), 0, dy),
                Cmd::Url(url) => {
                    if let Some(frame) = browser.as_ref().and_then(|b| b.main_frame()) {
                        frame.load_url(Some(&CefString::from(url.as_str())));
                    }
                }
                Cmd::Quit => break 'run,
            }
        }
        std::thread::sleep(Duration::from_millis(4));
    }

    // Fermeture : le navigateur se ferme, puis CEF.
    if let Some(h) = &host {
        h.close_browser(1);
    }
    let end = Instant::now() + Duration::from_millis(500);
    while Instant::now() < end {
        do_message_loop_work();
        std::thread::sleep(Duration::from_millis(10));
    }
    {
        let (lock, cv) = &*latest;
        if let Ok(mut l) = lock.lock() {
            l.closed = true;
        }
        cv.notify_one();
    }
    let _ = writer.join();
    drop(host);
    drop(browser);
    drop(client);
    shutdown();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lit_les_commandes() {
        assert!(matches!(parse_cmd("move 10 20"), Some(Cmd::Move(10, 20))));
        assert!(matches!(parse_cmd("down 1 2"), Some(Cmd::Down(1, 2))));
        assert!(matches!(
            parse_cmd("wheel 5 6 -120"),
            Some(Cmd::Wheel(5, 6, -120))
        ));
        match parse_cmd("url https://www.youtube.com/watch?v=x") {
            Some(Cmd::Url(u)) => assert_eq!(u, "https://www.youtube.com/watch?v=x"),
            _ => panic!("url"),
        }
        assert!(parse_cmd("move 1").is_none());
        assert!(parse_cmd("inconnu").is_none());
    }
}
