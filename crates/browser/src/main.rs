//! Navigateur des écrans vidéo de Minelib : Chromium (CEF) dessiné hors écran, un onglet
//! par écran.
//!
//! Le jeu lance ce programme (un seul pour tous ses écrans) et lui parle par ses entrées
//! et sorties standard.
//!
//! **Entrée** : une commande par ligne ; `<id>` est le numéro de l'écran, les coordonnées
//! sont en pixels de la page :
//! - `open <id> <adresse>` (crée l'onglet, ou y charge l'adresse), `close <id>` ;
//! - `move <id> x y`, `down <id> x y`, `up <id> x y`, `wheel <id> x y dy` ;
//! - `text <id> <texte>` (caractères tapés), `key <id> <nom>` (`back`, `delete`,
//!   `enter`, `tab`, `left`, `right`, `up`, `down`, `home`, `end`, `ctrl+a`, `ctrl+c`,
//!   `ctrl+v`, `ctrl+x`, `ctrl+y`, `ctrl+z`), `blur <id>` (le champ de texte perd le
//!   focus), `eval <id> <javascript>` ;
//! - `quit`.
//!
//! **Sortie** : des enregistrements, chacun un en-tête de 16 octets (étiquette, `id`, `a`,
//! `b` : u32 petit-boutistes) suivi éventuellement de données :
//! - `MLBF` : image de l'onglet, `a` × `b` pixels BGRA (`a * b * 4` octets) ;
//! - `MLBK` : un champ de texte a le focus (`a` = 1) ou ne l'a plus (`a` = 0) ;
//! - `MLBS` : état de la page, `a` octets UTF-8 (`{"u":adresse,"t":temps,"p":pause}`).
//!
//! Le jeu ne dépend jamais de CEF : sans ce programme et ses fichiers (`libcef.dll`…), ses
//! écrans montrent leur mire. Chromium relance aussi ce programme pour ses sous-processus
//! (rendu, GPU…) : `execute_process` les reconnaît et s'arrête là.
//!
//! C'est un navigateur ordinaire : le site de YouTube s'y affiche comme dans Chrome, avec
//! ses publicités, sans rien télécharger ni récupérer d'un autre navigateur. Sa session
//! (connexion à un compte, cookies) est la sienne, dans le dossier `--cache`.
#![windows_subsystem = "windows"]

use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, Write};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use cef::*;

/// Script injecté dans chaque page : signale quand un champ de texte prend ou perd le
/// focus (`__minelib_focus__0/1`), et l'adresse et l'état de la vidéo
/// (`__minelib_state__{…}` : temps, pause, pub YouTube en cours, vidéo du lecteur), par des
/// messages de la console que ce programme intercepte.
const PAGE_JS: &str = r#"(()=>{if(window.__minelib)return;window.__minelib=1;
const ed=e=>!!e&&(e.isContentEditable||e.tagName==='TEXTAREA'||(e.tagName==='INPUT'&&!/^(button|checkbox|color|file|hidden|image|radio|range|reset|submit)$/i.test(e.type||'')));
const deep=()=>{let a=document.activeElement;while(a&&a.shadowRoot&&a.shadowRoot.activeElement)a=a.shadowRoot.activeElement;return a;};
let f=null;const focus=()=>{const v=ed(deep())?1:0;if(v!==f){f=v;console.log('__minelib_focus__'+v);}};
document.addEventListener('focusin',focus,true);
document.addEventListener('focusout',()=>setTimeout(focus,0),true);
let s='';setInterval(()=>{focus();const mp=document.getElementById('movie_player');
const v=(mp&&mp.querySelector('video'))||document.querySelector('video');
let i='';try{if(mp&&mp.getVideoData)i=mp.getVideoData().video_id||'';}catch(e){}
const st=JSON.stringify({u:location.href,t:v?Math.round(v.currentTime*10)/10:-1,p:v?v.paused:true,a:!!document.querySelector('.ad-showing'),i:i});
if(st!==s){s=st;console.log('__minelib_state__'+st);}},500);})();"#;

/// Le champ de texte qui a le focus le perd.
const BLUR_JS: &str = "(()=>{let a=document.activeElement;\
while(a&&a.shadowRoot&&a.shadowRoot.activeElement)a=a.shadowRoot.activeElement;\
if(a&&a.blur)a.blur();})()";

/// Réglages passés par le jeu en ligne de commande.
struct Options {
    width: i32,
    height: i32,
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
            cache: None,
            log: None,
        };
        let mut args = std::env::args().skip(1);
        while let Some(a) = args.next() {
            let mut value = || args.next().unwrap_or_default();
            match a.as_str() {
                "--width" => o.width = value().parse().unwrap_or(o.width).clamp(64, 3840),
                "--height" => o.height = value().parse().unwrap_or(o.height).clamp(64, 2160),
                "--cache" => o.cache = Some(value()),
                "--log" => o.log = Some(value()),
                _ => {}
            }
        }
        o
    }
}

/// Commande du jeu.
#[derive(Debug, PartialEq)]
enum Cmd {
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
    Quit,
}

fn parse_cmd(line: &str) -> Option<Cmd> {
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
        _ => return None,
    })
}

/// Touche nommée : code de touche Windows (VK_*), modificateurs, caractère produit.
fn named_key(name: &str) -> Option<(i32, u32, Option<u16>)> {
    // EVENTFLAG_CONTROL_DOWN.
    let ctrl = 1 << 2;
    Some(match name {
        "back" => (0x08, 0, None),
        "tab" => (0x09, 0, Some(b'\t' as u16)),
        "enter" => (0x0D, 0, Some(b'\r' as u16)),
        "end" => (0x23, 0, None),
        "home" => (0x24, 0, None),
        "left" => (0x25, 0, None),
        "up" => (0x26, 0, None),
        "right" => (0x27, 0, None),
        "down" => (0x28, 0, None),
        "delete" => (0x2E, 0, None),
        "ctrl+a" => (0x41, ctrl, None),
        "ctrl+c" => (0x43, ctrl, None),
        "ctrl+v" => (0x56, ctrl, None),
        "ctrl+x" => (0x58, ctrl, None),
        "ctrl+y" => (0x59, ctrl, None),
        "ctrl+z" => (0x5A, ctrl, None),
        _ => return None,
    })
}

/// Ce qui attend d'être envoyé au jeu : la dernière image de chaque onglet (les images
/// peintes pendant un envoi remplacent la précédente) et les évènements, dans l'ordre.
#[derive(Default)]
struct Outbox {
    frames: HashMap<u32, (u32, u32, Vec<u8>)>,
    events: VecDeque<([u8; 4], u32, u32, Vec<u8>)>,
    closed: bool,
}

type Shared = Arc<(Mutex<Outbox>, Condvar)>;

fn push_event(out: &Shared, tag: [u8; 4], id: u32, a: u32, data: Vec<u8>) {
    let (lock, cv) = &**out;
    if let Ok(mut o) = lock.lock() {
        o.events.push_back((tag, id, a, data));
        cv.notify_one();
    }
}

wrap_render_handler! {
    struct Painter {
        id: u32,
        size: (i32, i32),
        out: Shared,
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
            let (lock, cv) = &*self.out;
            if let Ok(mut o) = lock.lock() {
                o.frames.insert(self.id, (width as u32, height as u32, pixels));
                cv.notify_one();
            }
        }
    }
}

wrap_display_handler! {
    struct Console {
        id: u32,
        out: Shared,
    }

    impl DisplayHandler {
        fn on_console_message(
            &self,
            _browser: Option<&mut Browser>,
            _level: LogSeverity,
            message: Option<&CefString>,
            _source: Option<&CefString>,
            _line: ::std::os::raw::c_int,
        ) -> ::std::os::raw::c_int {
            let Some(message) = message else { return 0 };
            let text = message.to_string();
            if let Some(v) = text.strip_prefix("__minelib_focus__") {
                push_event(&self.out, *b"MLBK", self.id, u32::from(v == "1"), Vec::new());
                1
            } else if let Some(state) = text.strip_prefix("__minelib_state__") {
                let data = state.as_bytes().to_vec();
                push_event(&self.out, *b"MLBS", self.id, data.len() as u32, data);
                1
            } else {
                0
            }
        }
    }
}

wrap_load_handler! {
    struct Loader;

    impl LoadHandler {
        fn on_load_end(
            &self,
            _browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            _http_status_code: ::std::os::raw::c_int,
        ) {
            if let Some(frame) = frame {
                if frame.is_main() != 0 {
                    frame.execute_java_script(Some(&CefString::from(PAGE_JS)), None, 0);
                }
            }
        }
    }
}

wrap_client! {
    struct TabClient {
        painter: RenderHandler,
        console: DisplayHandler,
        loader: LoadHandler,
    }

    impl Client {
        fn render_handler(&self) -> Option<RenderHandler> {
            Some(self.painter.clone())
        }

        fn display_handler(&self) -> Option<DisplayHandler> {
            Some(self.console.clone())
        }

        fn load_handler(&self) -> Option<LoadHandler> {
            Some(self.loader.clone())
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

/// Un onglet (un écran du jeu).
struct Tab {
    browser: Browser,
    host: BrowserHost,
    _client: Client,
}

impl Tab {
    fn open(id: u32, url: &str, opts: &Options, out: &Shared) -> Option<Self> {
        let painter = Painter::new(id, (opts.width, opts.height), out.clone());
        let console = Console::new(id, out.clone());
        let mut client = TabClient::new(painter, console, Loader::new());
        let window_info = WindowInfo::default().set_as_windowless(sys::HWND::default());
        let settings = BrowserSettings {
            windowless_frame_rate: 30,
            // Fond opaque : pas de transparence à gérer côté jeu.
            background_color: 0xff00_0000,
            ..Default::default()
        };
        let browser = browser_host_create_browser_sync(
            Some(&window_info),
            Some(&mut client),
            Some(&CefString::from(url)),
            Some(&settings),
            None,
            None,
        )?;
        let host = browser.host()?;
        host.set_focus(1);
        Some(Tab {
            browser,
            host,
            _client: client,
        })
    }

    fn js(&self, code: &str) {
        if let Some(frame) = self.browser.main_frame() {
            frame.execute_java_script(Some(&CefString::from(code)), None, 0);
        }
    }

    fn key(&self, vk: i32, modifiers: u32, ch: Option<u16>) {
        let mut ev = KeyEvent {
            type_: KeyEventType::RAWKEYDOWN,
            modifiers,
            windows_key_code: vk,
            ..Default::default()
        };
        self.host.send_key_event(Some(&ev));
        if let Some(c) = ch {
            ev.type_ = KeyEventType::CHAR;
            ev.windows_key_code = i32::from(c);
            ev.character = c;
            ev.unmodified_character = c;
            self.host.send_key_event(Some(&ev));
        }
        ev.type_ = KeyEventType::KEYUP;
        ev.windows_key_code = vk;
        ev.character = 0;
        ev.unmodified_character = 0;
        self.host.send_key_event(Some(&ev));
    }

    fn text(&self, text: &str) {
        for c in text.encode_utf16() {
            let ev = KeyEvent {
                type_: KeyEventType::CHAR,
                windows_key_code: i32::from(c),
                character: c,
                unmodified_character: c,
                ..Default::default()
            };
            self.host.send_key_event(Some(&ev));
        }
    }
}

fn mouse(x: i32, y: i32) -> MouseEvent {
    MouseEvent { x, y, modifiers: 0 }
}

/// Envoie au jeu les évènements puis les images, au fur et à mesure.
fn send_out(out: Shared) {
    let mut w = std::io::stdout().lock();
    let (lock, cv) = &*out;
    let header = |w: &mut std::io::StdoutLock, tag: &[u8; 4], id: u32, a: u32, b: u32| {
        w.write_all(tag)?;
        w.write_all(&id.to_le_bytes())?;
        w.write_all(&a.to_le_bytes())?;
        w.write_all(&b.to_le_bytes())
    };
    loop {
        let (events, frames) = {
            let Ok(mut o) = lock.lock() else { return };
            while o.frames.is_empty() && o.events.is_empty() && !o.closed {
                o = match cv.wait(o) {
                    Ok(o) => o,
                    Err(_) => return,
                };
            }
            if o.closed {
                return;
            }
            (std::mem::take(&mut o.events), std::mem::take(&mut o.frames))
        };
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

    let out: Shared = Arc::new((Mutex::new(Outbox::default()), Condvar::new()));
    let writer = {
        let out = out.clone();
        std::thread::spawn(move || send_out(out))
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

    // Boucle de Chromium sur ce fil (le fil « UI » de CEF) : les commandes y sont jouées.
    let mut tabs: HashMap<u32, Tab> = HashMap::new();
    let left = MouseButtonType::from(sys::cef_mouse_button_type_t::MBT_LEFT);
    'run: loop {
        do_message_loop_work();
        while let Ok(cmd) = rx.try_recv() {
            match cmd {
                Cmd::Quit => break 'run,
                Cmd::Open(id, url) => match tabs.get(&id) {
                    Some(t) => {
                        if let Some(frame) = t.browser.main_frame() {
                            frame.load_url(Some(&CefString::from(url.as_str())));
                        }
                    }
                    None => {
                        if let Some(t) = Tab::open(id, &url, &opts, &out) {
                            tabs.insert(id, t);
                        }
                    }
                },
                Cmd::Close(id) => {
                    if let Some(t) = tabs.remove(&id) {
                        t.host.close_browser(1);
                    }
                    if let Ok(mut o) = out.0.lock() {
                        o.frames.remove(&id);
                    }
                }
                Cmd::Move(id, x, y) => {
                    if let Some(t) = tabs.get(&id) {
                        t.host.send_mouse_move_event(Some(&mouse(x, y)), 0);
                    }
                }
                Cmd::Down(id, x, y) => {
                    if let Some(t) = tabs.get(&id) {
                        t.host
                            .send_mouse_click_event(Some(&mouse(x, y)), left, 0, 1);
                    }
                }
                Cmd::Up(id, x, y) => {
                    if let Some(t) = tabs.get(&id) {
                        t.host
                            .send_mouse_click_event(Some(&mouse(x, y)), left, 1, 1);
                    }
                }
                Cmd::Wheel(id, x, y, dy) => {
                    if let Some(t) = tabs.get(&id) {
                        t.host.send_mouse_wheel_event(Some(&mouse(x, y)), 0, dy);
                    }
                }
                Cmd::Text(id, text) => {
                    if let Some(t) = tabs.get(&id) {
                        t.text(&text);
                    }
                }
                Cmd::Key(id, name) => {
                    if let (Some(t), Some((vk, mods, ch))) = (tabs.get(&id), named_key(&name)) {
                        t.key(vk, mods, ch);
                    }
                }
                Cmd::Blur(id) => {
                    if let Some(t) = tabs.get(&id) {
                        t.js(BLUR_JS);
                    }
                }
                Cmd::Eval(id, code) => {
                    if let Some(t) = tabs.get(&id) {
                        t.js(&code);
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_millis(4));
    }

    // Fermeture : les onglets se ferment, puis CEF.
    for t in tabs.values() {
        t.host.close_browser(1);
    }
    let end = Instant::now() + Duration::from_millis(500);
    while Instant::now() < end {
        do_message_loop_work();
        std::thread::sleep(Duration::from_millis(10));
    }
    {
        let (lock, cv) = &*out;
        if let Ok(mut o) = lock.lock() {
            o.closed = true;
        }
        cv.notify_one();
    }
    let _ = writer.join();
    drop(tabs);
    shutdown();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lit_les_commandes() {
        assert_eq!(parse_cmd("move 3 10 20"), Some(Cmd::Move(3, 10, 20)));
        assert_eq!(parse_cmd("down 3 1 2"), Some(Cmd::Down(3, 1, 2)));
        assert_eq!(
            parse_cmd("wheel 3 5 6 -120"),
            Some(Cmd::Wheel(3, 5, 6, -120))
        );
        assert_eq!(
            parse_cmd("open 7 https://www.youtube.com/watch?v=x"),
            Some(Cmd::Open(7, "https://www.youtube.com/watch?v=x".into()))
        );
        // Le texte garde ses espaces.
        assert_eq!(
            parse_cmd("text 7 chat  qui joue"),
            Some(Cmd::Text(7, "chat  qui joue".into()))
        );
        assert_eq!(parse_cmd("text 7  "), Some(Cmd::Text(7, " ".into())));
        assert_eq!(
            parse_cmd("key 7 ctrl+v"),
            Some(Cmd::Key(7, "ctrl+v".into()))
        );
        assert_eq!(parse_cmd("close 7"), Some(Cmd::Close(7)));
        assert_eq!(parse_cmd("blur 7"), Some(Cmd::Blur(7)));
        assert_eq!(parse_cmd("quit"), Some(Cmd::Quit));
        assert_eq!(parse_cmd("move 3 1"), None);
        assert_eq!(parse_cmd("move x 1 2"), None);
        assert_eq!(parse_cmd("inconnu 1"), None);
    }

    #[test]
    fn touches_nommees() {
        assert_eq!(named_key("enter"), Some((0x0D, 0, Some(13))));
        assert!(named_key("ctrl+v").is_some_and(|k| k.1 != 0));
        assert_eq!(named_key("f13"), None);
    }
}
