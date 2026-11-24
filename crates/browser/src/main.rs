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
//!   focus), `eval <id> <javascript>`, `fps <id> <1..60>` (images par seconde de l'onglet) ;
//! - `quit`.
//!
//! **Options** : `--width`, `--height`, `--cache <dossier de session>`, `--log <journal>`,
//! `--fps <images par seconde d'un nouvel onglet, 30 par défaut>`, `--scroll`,
//! `--filtre blanche|noire`, `--listes <dossier des listes>`,
//! `--restreint strict|modere|non` (mode restreint de YouTube ; SafeSearch est toujours
//! imposé), `--sans-compte` (v208 : pas de connexion à un compte, cookies effacés ; le jeu
//! ne s'en sert plus depuis la v243),
//! `--volume <0..100>` (v213, volume des sons du navigateur dans Windows ; ensuite la
//! commande `volume 0 <0..100>`).
//!
//! **Filtrage (v205)** : une page qui n'est pas autorisée ne s'ouvre pas du tout — ni dans
//! l'onglet, ni dans un cadre de la page. Voir `filtre`. (v243) Le jeu est interdit aux
//! moins de 18 ans : le web s'ouvre sauf les interdits (liste noire), et le DNS passe par
//! le DNS chiffré familial de Cloudflare, qui ne résout ni les sites pour adultes ni ceux
//! des logiciels malveillants.
//!
//! Aucune fenêtre ne s'ouvre jamais : les liens « nouvel onglet » cliqués s'ouvrent dans
//! l'écran, les fenêtres ouvertes par un script sont bloquées, `alert`/`confirm`/`prompt`
//! et le choix de fichier n'affichent rien, les autorisations (caméra, micro,
//! notifications…) sont refusées.
//!
//! **Sortie** : des enregistrements, chacun un en-tête de 16 octets (étiquette, `id`, `a`,
//! `b` : u32 petit-boutistes) suivi éventuellement de données :
//! - `MLBF` : image de l'onglet, `a` × `b` pixels BGRA (`a * b * 4` octets) ;
//! - `MLBK` : un champ de texte a le focus (`a` = 1) ou ne l'a plus (`a` = 0) ;
//! - `MLBS` : état de la page, `a` octets UTF-8 (`{"u":adresse,"t":temps,"p":pause,
//!   "a":pub,"i":vidéo du lecteur}`) ;
//! - `MLBW` (seulement avec `--scroll`) : la page peut défiler sous la souris vers le haut
//!   (`a` & 1) et vers le bas (`a` & 2) ;
//! - `MLBH` (v213) : il y a une page précédente (`a` & 1), une page suivante (`a` & 2).
//!
//! Ce qu'il écrit sur sa sortie d'erreur (onglets ouverts, première image de chaque onglet,
//! chargements ratés) va dans `logs/navigateur-sortie.log`.
//!
//! Le jeu ne dépend jamais de CEF : sans ce programme et ses fichiers (`libcef.dll`…), ses
//! écrans montrent leur mire. Chromium relance aussi ce programme pour ses sous-processus
//! (rendu, GPU…) : `execute_process` les reconnaît et s'arrête là.
//!
//! C'est un navigateur ordinaire : le site de YouTube s'y affiche comme dans Chrome, avec
//! ses publicités, sans rien télécharger ni récupérer d'un autre navigateur. Sa session
//! (connexion à un compte, cookies) est la sienne, dans le dossier `--cache`.
#![windows_subsystem = "windows"]

mod filtre;
mod volume;

use filtre::{Filtre, Mode, Restreint};
use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::path::PathBuf;
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use cef::*;

/// Script injecté dans chaque page : signale quand un champ de texte prend ou perd le
/// focus (`__minelib_focus__0/1`), et l'adresse et l'état de la vidéo
/// (`__minelib_state__{…}` : temps, pause, pub YouTube en cours, vidéo du lecteur), par des
/// messages de la console que ce programme intercepte. (v243) Après chaque clic, il redit
/// son focus même inchangé : le jeu ne donne le clavier à un champ que juste après un clic.
/// (v243) Une vidéo que le lecteur embarqué a renvoyée au site (`&mlb=site` : YouTube ne
/// la permet pas ailleurs) y reste, même si YouTube enlève ce paramètre de l'adresse.
const PAGE_JS: &str = r#"(()=>{if(window.__minelib)return;window.__minelib=1;
const ed=e=>!!e&&(e.isContentEditable||e.tagName==='TEXTAREA'||(e.tagName==='INPUT'&&!/^(button|checkbox|color|file|hidden|image|radio|range|reset|submit)$/i.test(e.type||'')));
const deep=()=>{let a=document.activeElement;while(a&&a.shadowRoot&&a.shadowRoot.activeElement)a=a.shadowRoot.activeElement;return a;};
let f=null;const focus=()=>{const v=ed(deep())?1:0;if(v!==f){f=v;console.log('__minelib_focus__'+v);}};
document.addEventListener('focusin',focus,true);
document.addEventListener('focusout',()=>setTimeout(focus,0),true);
document.addEventListener('mousedown',()=>setTimeout(()=>{f=null;focus();},0),true);
const L='https://viendax.github.io/minelib/lecteur';
const SS=k=>{try{return sessionStorage.getItem(k);}catch(e){return null;}};
const vid=u=>{try{const x=new URL(u,location.href);const h=x.hostname.toLowerCase();let i='';
if(h==='youtu.be')i=x.pathname.split('/')[1]||'';
else if(h==='www.youtube.com'||h==='youtube.com'||h==='m.youtube.com'){
if(x.pathname==='/watch')i=x.searchParams.get('v')||'';
else{const m=x.pathname.match(/^\/(shorts|live)\/([^\/?#]+)/);if(m)i=m[2];}}
if(!/^[A-Za-z0-9_-]{11}$/.test(i))return '';
if(x.searchParams.get('mlb')==='site'){try{sessionStorage.setItem('mlb_site_'+i,'1');}catch(e){}return '';}
if(SS('mlb_site_'+i))return '';
const t=parseInt(x.searchParams.get('t')||x.searchParams.get('start')||'0',10)||0;
return L+'?v='+i+(t>0?'&t='+t:'');}catch(e){return '';}};
if(/(^|\.)youtube\.com$/.test(location.hostname)){
const go=(u,r)=>{const l=vid(u);if(l){if(r)location.replace(l);else location.href=l;return true;}return false;};
document.addEventListener('click',e=>{const a=e.composedPath().find(x=>x&&x.tagName==='A'&&x.href);
if(a&&vid(a.href)){e.preventDefault();e.stopImmediatePropagation();go(a.href,false);}},true);
setInterval(()=>go(location.href,true),250);go(location.href,true);}
let s='';window.addEventListener('pageshow',()=>{s='';});setInterval(()=>{focus();if(window.__mlb_lecteur)return;const mp=document.getElementById('movie_player');
const v=(mp&&mp.querySelector('video'))||document.querySelector('video');
let i='';try{if(mp&&mp.getVideoData)i=mp.getVideoData().video_id||'';}catch(e){}
const st=JSON.stringify({u:location.href,t:v?Math.round(v.currentTime*10)/10:-1,p:v?v.paused:true,a:!!document.querySelector('.ad-showing'),i:i});
if(st!==s){s=st;console.log('__minelib_state__'+st);}},500);})();"#;

/// Script injecté si le jeu le demande (`--scroll`) : signale si la page peut défiler
/// sous la souris, vers le haut (1) et vers le bas (2) (`__minelib_scroll__<0..3>`), pour
/// que la molette n'aille à l'écran que si elle y sert.
const SCROLL_JS: &str = r#"(()=>{if(window.__minelib_scroll)return;window.__minelib_scroll=1;
if(window.__mlb_lecteur){console.log('__minelib_scroll__0');return;}
let mx=-1,my=-1,w=-1;
const sf=()=>{let f=0;const d=document.scrollingElement||document.documentElement;
if(d){if(d.scrollTop>0)f|=1;if(d.scrollTop+window.innerHeight<d.scrollHeight-1)f|=2;}
let e=mx<0?null:document.elementFromPoint(mx,my);
while(e&&e.shadowRoot){const i=e.shadowRoot.elementFromPoint(mx,my);if(!i||i===e)break;e=i;}
for(;e&&e!==d;e=e.parentElement||(e.getRootNode&&e.getRootNode().host)||null){
if(e.tagName==='IFRAME'){f|=3;break;}
const o=getComputedStyle(e).overflowY;
if((o==='auto'||o==='scroll'||o==='overlay')&&e.scrollHeight>e.clientHeight+1){
if(e.scrollTop>0)f|=1;if(e.scrollTop+e.clientHeight<e.scrollHeight-1)f|=2;}}
return f;};
const sr=()=>{const v=sf();if(v!==w){w=v;console.log('__minelib_scroll__'+v);}};
document.addEventListener('mousemove',ev=>{mx=ev.clientX;my=ev.clientY;sr();},true);
document.addEventListener('scroll',()=>setTimeout(sr,0),true);
setInterval(sr,500);sr();})();"#;

/// (v210) Injecté dans le cadre du lecteur officiel (`youtube-nocookie.com/embed`) : dit
/// seulement si une publicité y joue (`__minelib_pub__0|1`), pour que la vidéo partagée ne
/// soit ni lue ni corrigée pendant ce temps. Il lit, il ne touche à rien.
const PUB_JS: &str = r#"(()=>{if(window.__minelib_pub)return;window.__minelib_pub=1;let x=-1;
setInterval(()=>{const a=document.querySelector('.ad-showing')?1:0;
if(a!==x){x=a;console.log('__minelib_pub__'+a);}},400);
let v=null,mt=-1,rt=0,gel=0,perdues=-1;const D=m=>console.log('__minelib_diag__'+m);
const suit=e=>{if(!e||e===v)return;v=e;['waiting','stalled','seeking','emptied','pause','play'].forEach(n=>e.addEventListener(n,()=>D(n+' a '+e.currentTime.toFixed(2))));};
setInterval(()=>{suit(document.querySelector('video'));if(!v)return;const now=performance.now();
if(v.paused||v.ended||v.readyState<3){mt=-1;gel=0;return;}
const t=v.currentTime;if(mt>=0){const dr=now-rt,dm=(t-mt)*1000/(v.playbackRate||1);
if(dm<dr*0.5&&dr>0){gel+=dr;}else{if(gel>=120)D('gel '+Math.round(gel)+' ms a '+t.toFixed(2));gel=0;}
if(dm>dr+250)D('saut '+Math.round(dm-dr)+' ms a '+t.toFixed(2));}
mt=t;rt=now;
const q=v.getVideoPlaybackQuality&&v.getVideoPlaybackQuality();if(q){const p=q.droppedVideoFrames;
if(perdues>=0&&p-perdues>=6)D('images perdues '+(p-perdues));perdues=p;}},100);})();"#;

/// (v210) La page du lecteur des écrans, servie par le navigateur à `filtre::LECTEUR`.
const LECTEUR_HTML: &str = include_str!("lecteur.html");

/// Le cadre du lecteur officiel (où injecter `PUB_JS`).
fn cadre_du_lecteur(url: &str) -> bool {
    filtre::hote(url).is_some_and(|h| h == "www.youtube-nocookie.com" || h == "www.youtube.com")
        && url.split_once("://").is_some_and(|(_, r)| {
            r.split_once('/').is_some_and(|(_, chemin)| chemin.starts_with("embed/"))
        })
}

/// Le champ de texte qui a le focus le perd.
const BLUR_JS: &str = "(()=>{let a=document.activeElement;\
while(a&&a.shadowRoot&&a.shadowRoot.activeElement)a=a.shadowRoot.activeElement;\
if(a&&a.blur)a.blur();})()";

/// Images par seconde au plus (rendu hors écran de Chromium).
const MAX_FPS: i32 = 60;

/// Réglages passés par le jeu en ligne de commande.
struct Options {
    width: i32,
    height: i32,
    /// Dossier de la session du navigateur (cookies, cache).
    cache: Option<String>,
    /// Journal de Chromium.
    log: Option<String>,
    /// Le jeu veut savoir où la page peut défiler (`MLBW`).
    scroll: bool,
    /// Images par seconde d'un nouvel onglet (le jeu les règle ensuite par onglet).
    fps: i32,
    /// (v205) Liste blanche (le défaut : rien que YouTube et Google) ou liste noire.
    filtre: Mode,
    /// Dossier des fichiers `liste_blanche.txt` et `liste_noire.txt`.
    listes: Option<PathBuf>,
    /// Mode restreint de YouTube et SafeSearch (strict par défaut).
    restreint: Restreint,
    /// (v208) En ligne : pas de page de connexion, cookies effacés au lancement.
    sans_compte: bool,
    /// (v213) Volume des sons du navigateur au lancement, en pour cent.
    volume: i32,
}

impl Options {
    fn parse() -> Self {
        let mut o = Options {
            width: 1280,
            height: 720,
            cache: None,
            log: None,
            scroll: false,
            fps: 30,
            filtre: Mode::Blanche,
            listes: None,
            restreint: Restreint::Strict,
            sans_compte: false,
            volume: 50,
        };
        let mut args = std::env::args().skip(1);
        while let Some(a) = args.next() {
            let mut value = || args.next().unwrap_or_default();
            match a.as_str() {
                "--width" => o.width = value().parse().unwrap_or(o.width).clamp(64, 3840),
                "--height" => o.height = value().parse().unwrap_or(o.height).clamp(64, 2160),
                "--cache" => o.cache = Some(value()),
                "--log" => o.log = Some(value()),
                "--scroll" => o.scroll = true,
                "--fps" => o.fps = value().parse().unwrap_or(o.fps).clamp(1, MAX_FPS),
                "--filtre" => o.filtre = Mode::from_name(&value()).unwrap_or(o.filtre),
                "--listes" => o.listes = Some(PathBuf::from(value())),
                "--sans-compte" => o.sans_compte = true,
                "--volume" => o.volume = value().parse().unwrap_or(o.volume).clamp(0, 100),
                "--restreint" => o.restreint = Restreint::from_name(&value()).unwrap_or(o.restreint),
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
    /// Images par seconde de l'onglet (1 à 60).
    Fps(u32, i32),
    /// (v213) Volume de tous les sons du navigateur, en pour cent (l'onglet est ignoré).
    Volume(i32),
    /// (v213) Page précédente, page suivante (boutons retour et suivant de l'écran).
    Back(u32),
    Forward(u32),
    /// (v214) Chromium demande à faire son travail dans tant de millisecondes (pas du jeu :
    /// voir `Pompe`).
    Pump(i64),
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
        "fps" => Cmd::Fps(id, num()?),
        "volume" => Cmd::Volume(num()?),
        "back" => Cmd::Back(id),
        "forward" => Cmd::Forward(id),
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
    /// Images par seconde envoyées au plus, par onglet (Chromium en peint souvent plus :
    /// deux fois la fréquence demandée).
    fps: HashMap<u32, i32>,
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
        // Première image déjà signalée (journal).
        painted: Arc<AtomicBool>,
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
            if !self.painted.swap(true, Ordering::Relaxed) {
                eprintln!("onglet {} : première image ({width} × {height})", self.id);
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
        // (v210) Une pub joue dans le lecteur officiel de la page.
        pub_: Arc<AtomicBool>,
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
            } else if let Some(v) = text.strip_prefix("__minelib_scroll__") {
                let flags = v.trim().parse::<u32>().unwrap_or(0) & 3;
                push_event(&self.out, *b"MLBW", self.id, flags, Vec::new());
                1
            } else if let Some(d) = text.strip_prefix("__minelib_diag__") {
                // (v214) Ce que voit le lecteur : gel du temps de la vidéo (le son s'arrête
                // avec), saut, mise en mémoire, images perdues. Pour trouver les coupures.
                eprintln!("{} onglet {} : lecteur : {d}", heure(), self.id);
                1
            } else if let Some(v) = text.strip_prefix("__minelib_pub__") {
                self.pub_.store(v.trim() == "1", Ordering::Relaxed);
                1
            } else if let Some(state) = text.strip_prefix("__minelib_state__") {
                // (v210) Une pub dans le cadre du lecteur : la page le dit au jeu.
                let state = if self.pub_.load(Ordering::Relaxed) {
                    state.replace("\"a\":false", "\"a\":true")
                } else {
                    state.to_string()
                };
                let data = state.into_bytes();
                push_event(&self.out, *b"MLBS", self.id, data.len() as u32, data);
                1
            } else {
                0
            }
        }
    }
}

wrap_load_handler! {
    struct Loader {
        id: u32,
        scroll: bool,
        out: Shared,
    }

    impl LoadHandler {
        // (v213) Les boutons retour et suivant de l'écran sont grisés sans page où aller.
        fn on_loading_state_change(
            &self,
            _browser: Option<&mut Browser>,
            _is_loading: ::std::os::raw::c_int,
            can_go_back: ::std::os::raw::c_int,
            can_go_forward: ::std::os::raw::c_int,
        ) {
            let bits = u32::from(can_go_back != 0) | u32::from(can_go_forward != 0) << 1;
            push_event(&self.out, *b"MLBH", self.id, bits, Vec::new());
        }

        fn on_load_error(
            &self,
            _browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            error_code: Errorcode,
            error_text: Option<&CefString>,
            failed_url: Option<&CefString>,
        ) {
            // (Une navigation interrompue par une autre n'est pas une erreur.)
            let main = frame.is_some_and(|f| f.is_main() != 0);
            let aborted = *error_code.as_ref() == sys::cef_errorcode_t::ERR_ABORTED;
            if main && !aborted {
                eprintln!(
                    "onglet {} : chargement raté ({:?}, {}) : {}",
                    self.id,
                    error_code,
                    error_text.map(|t| t.to_string()).unwrap_or_default(),
                    failed_url.map(|u| u.to_string()).unwrap_or_default()
                );
            }
        }

        fn on_load_end(
            &self,
            _browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            _http_status_code: ::std::os::raw::c_int,
        ) {
            if let Some(frame) = frame {
                if frame.is_main() == 0 {
                    let url = CefString::from(&frame.url()).to_string();
                    if cadre_du_lecteur(&url) {
                        frame.execute_java_script(Some(&CefString::from(PUB_JS)), None, 0);
                    }
                } else {
                    frame.execute_java_script(Some(&CefString::from(PAGE_JS)), None, 0);
                    if self.scroll {
                        frame.execute_java_script(Some(&CefString::from(SCROLL_JS)), None, 0);
                    }
                }
            }
        }
    }
}

wrap_life_span_handler! {
    struct Popups {
        id: u32,
    }

    impl LifeSpanHandler {
        /// Jamais de nouvelle fenêtre. Un lien qu'on clique pour l'ouvrir dans un nouvel
        /// onglet s'ouvre dans cet écran ; le reste (fenêtres ouvertes par un script, pubs)
        /// est bloqué.
        fn on_before_popup(
            &self,
            browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _popup_id: ::std::os::raw::c_int,
            target_url: Option<&CefString>,
            _target_frame_name: Option<&CefString>,
            target_disposition: WindowOpenDisposition,
            user_gesture: ::std::os::raw::c_int,
            _popup_features: Option<&PopupFeatures>,
            _window_info: Option<&mut WindowInfo>,
            _client: Option<&mut Option<Client>>,
            _settings: Option<&mut BrowserSettings>,
            _extra_info: Option<&mut Option<DictionaryValue>>,
            _no_javascript_access: Option<&mut ::std::os::raw::c_int>,
        ) -> ::std::os::raw::c_int {
            let url = target_url.map(|u| u.to_string()).unwrap_or_default();
            let web = url.starts_with("https://") || url.starts_with("http://");
            let link =
                user_gesture != 0 && web && target_disposition != WindowOpenDisposition::NEW_POPUP;
            if link {
                if let Some(frame) = browser.and_then(|b| b.main_frame()) {
                    frame.load_url(Some(&CefString::from(url.as_str())));
                }
            }
            let what = if link { "ouverte dans l'écran" } else { "bloquée" };
            eprintln!("onglet {} : fenêtre {what} ({url})", self.id);
            1
        }
    }
}

wrap_jsdialog_handler! {
    struct Dialogs;

    impl JsdialogHandler {
        /// `alert`, `confirm`, `prompt` : aucune boîte (comme si on l'avait fermée).
        fn on_jsdialog(
            &self,
            _browser: Option<&mut Browser>,
            _origin_url: Option<&CefString>,
            _dialog_type: JsdialogType,
            _message_text: Option<&CefString>,
            _default_prompt_text: Option<&CefString>,
            _callback: Option<&mut JsdialogCallback>,
            suppress_message: Option<&mut ::std::os::raw::c_int>,
        ) -> ::std::os::raw::c_int {
            if let Some(s) = suppress_message {
                *s = 1;
            }
            0
        }

        /// « Quitter la page ? » : oui, sans boîte.
        fn on_before_unload_dialog(
            &self,
            _browser: Option<&mut Browser>,
            _message_text: Option<&CefString>,
            _is_reload: ::std::os::raw::c_int,
            callback: Option<&mut JsdialogCallback>,
        ) -> ::std::os::raw::c_int {
            if let Some(c) = callback {
                c.cont(1, None);
            }
            1
        }
    }
}

wrap_dialog_handler! {
    struct Files;

    impl DialogHandler {
        /// Choix d'un fichier : jamais (pas de fenêtre, rien du disque).
        fn on_file_dialog(
            &self,
            _browser: Option<&mut Browser>,
            _mode: FileDialogMode,
            _title: Option<&CefString>,
            _default_file_path: Option<&CefString>,
            _accept_filters: Option<&mut CefStringList>,
            _accept_extensions: Option<&mut CefStringList>,
            _accept_descriptions: Option<&mut CefStringList>,
            callback: Option<&mut FileDialogCallback>,
        ) -> ::std::os::raw::c_int {
            if let Some(c) = callback {
                c.cancel();
            }
            1
        }
    }
}

wrap_permission_handler! {
    struct Permissions;

    impl PermissionHandler {
        /// Caméra, micro, partage d'écran : refusés, sans demander.
        fn on_request_media_access_permission(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _requesting_origin: Option<&CefString>,
            _requested_permissions: u32,
            callback: Option<&mut MediaAccessCallback>,
        ) -> ::std::os::raw::c_int {
            if let Some(c) = callback {
                c.cancel();
            }
            1
        }

        /// Notifications, position… : refusées, sans demander.
        fn on_show_permission_prompt(
            &self,
            _browser: Option<&mut Browser>,
            _prompt_id: u64,
            _requesting_origin: Option<&CefString>,
            _requested_permissions: u32,
            callback: Option<&mut PermissionPromptCallback>,
        ) -> ::std::os::raw::c_int {
            if let Some(c) = callback {
                c.cont(PermissionRequestResult::DENY);
            }
            1
        }
    }
}

wrap_client! {
    struct TabClient {
        painter: RenderHandler,
        console: DisplayHandler,
        loader: LoadHandler,
        popups: LifeSpanHandler,
        dialogs: JsdialogHandler,
        files: DialogHandler,
        permissions: PermissionHandler,
        requests: RequestHandler,
    }

    impl Client {
        fn life_span_handler(&self) -> Option<LifeSpanHandler> {
            Some(self.popups.clone())
        }

        fn jsdialog_handler(&self) -> Option<JsdialogHandler> {
            Some(self.dialogs.clone())
        }

        fn dialog_handler(&self) -> Option<DialogHandler> {
            Some(self.files.clone())
        }

        fn permission_handler(&self) -> Option<PermissionHandler> {
            Some(self.permissions.clone())
        }

        fn render_handler(&self) -> Option<RenderHandler> {
            Some(self.painter.clone())
        }

        fn display_handler(&self) -> Option<DisplayHandler> {
            Some(self.console.clone())
        }

        fn load_handler(&self) -> Option<LoadHandler> {
            Some(self.loader.clone())
        }

        // (v205) Le filtrage des pages.
        fn request_handler(&self) -> Option<RequestHandler> {
            Some(self.requests.clone())
        }
    }
}

// (v205) Le garde d'un onglet : il refuse une page avant qu'elle ne s'ouvre, refuse de
// même les ressources d'une page refusée (un cadre, une vidéo, une image), met le mode
// restreint de YouTube et SafeSearch sur les recherches Google, et dit au jeu ce qu'il a
// refusé pour qu'il l'écrive au joueur.
wrap_request_handler! {
    struct Garde {
        id: u32,
        filtre: Arc<Filtre>,
        restreint: Restreint,
        out: Shared,
    }

    impl RequestHandler {
        fn on_before_browse(
            &self,
            _browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            request: Option<&mut Request>,
            _user_gesture: ::std::os::raw::c_int,
            _is_redirect: ::std::os::raw::c_int,
        ) -> ::std::os::raw::c_int {
            let url = request.map(url_de).unwrap_or_default();
            match self.filtre.juge(&url) {
                Ok(()) => 0,
                Err(refus) => {
                    eprintln!(
                        "onglet {} : page refusée ({}) : {url}",
                        self.id,
                        refus.raison()
                    );
                    // (v210) Seulement ce que le joueur voit : une page refusée dans un cadre
                    // caché (la vérification de connexion de YouTube) ne lui dit rien.
                    if frame.is_some_and(|f| f.is_main() != 0) {
                        push_event(&self.out, *b"MLBX", self.id, refus as u32, Vec::new());
                    }
                    1
                }
            }
        }

        fn resource_request_handler(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _request: Option<&mut Request>,
            _is_navigation: ::std::os::raw::c_int,
            _is_download: ::std::os::raw::c_int,
            _request_initiator: Option<&CefString>,
            _disable_default_handling: Option<&mut ::std::os::raw::c_int>,
        ) -> Option<ResourceRequestHandler> {
            Some(Entetes::new(self.filtre.clone(), self.restreint))
        }
    }
}

wrap_resource_request_handler! {
    struct Entetes {
        filtre: Arc<Filtre>,
        restreint: Restreint,
    }

    impl ResourceRequestHandler {
        fn on_before_resource_load(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            request: Option<&mut Request>,
            _callback: Option<&mut Callback>,
        ) -> ReturnValue {
            let Some(req) = request else {
                return ReturnValue::CONTINUE;
            };
            let url = url_de(req);
            if self.filtre.juge(&url).is_err() {
                return ReturnValue::CANCEL;
            }
            // (v210) Une vidéo du site YouTube ouverte dans l'onglet : le lecteur officiel.
            if req.resource_type() == ResourceType::MAIN_FRAME {
                if let Some(lecteur) = filtre::vers_lecteur(&url) {
                    req.set_url(Some(&CefString::from(lecteur.as_str())));
                    return ReturnValue::CONTINUE;
                }
            }
            // Mode restreint de YouTube : l'en-tête que Google documente pour les réseaux
            // encadrés, sur les seuls noms qu'il indique. (v243) Coupé par défaut.
            if let Some(valeur) = self.restreint.entete() {
                if filtre::hote(&url).is_some_and(|h| filtre::youtube(&h)) {
                    req.set_header_by_name(
                        Some(&CefString::from("YouTube-Restrict")),
                        Some(&CefString::from(valeur)),
                        1,
                    );
                }
            }
            // SafeSearch : une recherche Google part toujours avec `safe=active`. (v243)
            // Gardé pour un jeu interdit aux moins de 18 ans : les images d'une recherche
            // viennent des serveurs de Google, que la liste noire ne voit pas.
            if filtre::recherche_google(&url) {
                if let Some(neuf) = filtre::avec_safe(&url) {
                    req.set_url(Some(&CefString::from(neuf.as_str())));
                }
            }
            ReturnValue::CONTINUE
        }

        fn resource_handler(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            request: Option<&mut Request>,
        ) -> Option<ResourceHandler> {
            let url = request.map(url_de)?;
            filtre::est_lecteur(&url).then(|| PageLecteur::new(Arc::new(Mutex::new(0))))
        }
    }
}

wrap_resource_handler! {
    // (v210) La page du lecteur, servie sans réseau.
    struct PageLecteur {
        lu: Arc<Mutex<usize>>,
    }

    impl ResourceHandler {
        fn open(
            &self,
            _request: Option<&mut Request>,
            handle_request: Option<&mut ::std::os::raw::c_int>,
            _callback: Option<&mut Callback>,
        ) -> ::std::os::raw::c_int {
            if let Some(h) = handle_request {
                *h = 1;
            }
            1
        }

        fn response_headers(
            &self,
            response: Option<&mut Response>,
            response_length: Option<&mut i64>,
            _redirect_url: Option<&mut CefString>,
        ) {
            if let Some(r) = response {
                r.set_status(200);
                r.set_mime_type(Some(&CefString::from("text/html")));
                r.set_charset(Some(&CefString::from("utf-8")));
                r.set_header_by_name(
                    Some(&CefString::from("Cache-Control")),
                    Some(&CefString::from("no-store")),
                    1,
                );
            }
            if let Some(n) = response_length {
                *n = LECTEUR_HTML.len() as i64;
            }
        }

        fn read(
            &self,
            data_out: *mut u8,
            bytes_to_read: ::std::os::raw::c_int,
            bytes_read: Option<&mut ::std::os::raw::c_int>,
            _callback: Option<&mut ResourceReadCallback>,
        ) -> ::std::os::raw::c_int {
            let page = LECTEUR_HTML.as_bytes();
            let mut lu = self.lu.lock().unwrap_or_else(|e| e.into_inner());
            let n = (page.len() - *lu).min(bytes_to_read.max(0) as usize);
            if n == 0 || data_out.is_null() {
                if let Some(b) = bytes_read {
                    *b = 0;
                }
                return 0;
            }
            // SAFETY : CEF fournit `bytes_to_read` octets à écrire en `data_out`.
            unsafe { std::ptr::copy_nonoverlapping(page[*lu..].as_ptr(), data_out, n) };
            *lu += n;
            if let Some(b) = bytes_read {
                *b = n as i32;
            }
            1
        }
    }
}

/// (v214) L'heure (UTC, à la milliseconde), comme le journal du jeu, pour comparer les deux.
fn heure() -> String {
    let d = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let ms = d.as_millis() % 86_400_000;
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        ms / 3_600_000,
        ms / 60_000 % 60,
        ms / 1000 % 60,
        ms % 1000
    )
}

/// L'adresse d'une requête, en texte.
fn url_de(request: &mut Request) -> String {
    CefString::from(&request.url()).to_string()
}

/// (v214) Où Chromium demande à faire son travail (voir `Pompe`) : le fil principal.
static POMPE: std::sync::OnceLock<mpsc::Sender<Cmd>> = std::sync::OnceLock::new();

wrap_browser_process_handler! {
    // (v214) Chromium dit lui-même quand il a du travail (`external_message_pump`), au lieu
    // d'être servi à un rythme fixe. Avant, la boucle le servait puis dormait 4 ms ; sous
    // Windows, une attente de 4 ms dure souvent 15,6 ms (résolution par défaut de
    // l'horloge), et l'image d'un écran tombait alors presque à zéro par moments (essai :
    // 1806 images en 46 s avec 4 ms, 10 avec 16 ms) : les micro-coupures.
    //
    // (v243) C'est aussi là que le DNS chiffré familial de Cloudflare est mis en place, une
    // fois Chromium prêt (`dns_doh`).
    struct Pompe;

    impl BrowserProcessHandler {
        fn on_context_initialized(&self) {
            dns_doh();
        }

        fn on_schedule_message_pump_work(&self, delay_ms: i64) {
            if let Some(tx) = POMPE.get() {
                let _ = tx.send(Cmd::Pump(delay_ms));
            }
        }
    }
}

/// (v243) Le DNS du navigateur passe, chiffré, par le DNS familial de Cloudflare
/// (« 1.1.1.1 for Families », filtre logiciels malveillants et contenus pour adultes) : un
/// site pour adultes ne se résout pas, donc ne s'ouvre pas, même absent des listes. Mode
/// « secure » : pas de repli sur le DNS de la machine. Les règles de SafeSearch
/// (`host-resolver-rules`) passent avant. Limite : derrière un proxy, c'est le proxy qui
/// résout les noms, et ce filtre-ci ne joue pas (la liste noire, si).
fn dns_doh() {
    let Some(prefs) = preference_manager_get_global() else {
        eprintln!("DNS familial : préférences de Chromium introuvables");
        return;
    };
    let mut ok = true;
    for (nom, valeur) in [
        ("dns_over_https.mode", "secure"),
        ("dns_over_https.templates", DOH_FAMILLE),
    ] {
        let Some(mut v) = value_create() else {
            ok = false;
            continue;
        };
        v.set_string(Some(&CefString::from(valeur)));
        // Une chaîne vide, pas `default()` : celle-ci passe un pointeur nul, et CEF refuse
        // alors tout sans rien dire (vu le 24/11/2026).
        let mut erreur = CefString::from("");
        if prefs.set_preference(Some(&CefString::from(nom)), Some(&mut v), Some(&mut erreur)) != 1 {
            eprintln!("DNS familial : {nom} non réglé ({erreur})");
            ok = false;
        }
    }
    let mode = prefs
        .preference(Some(&CefString::from("dns_over_https.mode")))
        .map(|v| CefString::from(&v.string()).to_string())
        .unwrap_or_default();
    eprintln!(
        "DNS familial de Cloudflare : {} (mode {mode})",
        if ok { "en place" } else { "PAS en place" }
    );
}

/// (v243) Le DNS chiffré familial de Cloudflare (adresse documentée par Cloudflare).
const DOH_FAMILLE: &str = "https://family.cloudflare-dns.com/dns-query";

wrap_app! {
    struct BrowserApp {
        restreint: Restreint,
    }

    impl App {
        fn browser_process_handler(&self) -> Option<BrowserProcessHandler> {
            Some(Pompe::new())
        }

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
                // (v205) Mode restreint de YouTube et SafeSearch aussi par le DNS, la
                // seconde méthode que Google documente : les noms qu'il indique (et aucun
                // autre) pointent vers son hôte restreint. Deux barrières valent mieux
                // qu'une, et celle-ci tient même si une page contourne l'en-tête. (v243)
                // SafeSearch toujours, le mode restreint de YouTube seulement s'il est
                // demandé. Ces règles passent avant le DNS, chiffré ou non (voir `Pompe`).
                let mut regles: Vec<String> = match self.restreint.hote_dns() {
                    Some(cible) => filtre::HOTES_YOUTUBE
                        .iter()
                        .map(|h| format!("MAP {h} {cible}"))
                        .collect(),
                    None => Vec::new(),
                };
                regles.push("MAP www.google.com forcesafesearch.google.com".into());
                cl.append_switch_with_value(
                    Some(&CefString::from("host-resolver-rules")),
                    Some(&CefString::from(regles.join(",").as_str())),
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
    fn open(id: u32, url: &str, opts: &Options, filtre: &Arc<Filtre>, out: &Shared) -> Option<Self> {
        let painted = Arc::new(AtomicBool::new(false));
        let painter = Painter::new(id, (opts.width, opts.height), out.clone(), painted);
        let console = Console::new(id, out.clone(), Arc::new(AtomicBool::new(false)));
        let mut client = TabClient::new(
            painter,
            console,
            Loader::new(id, opts.scroll, out.clone()),
            Popups::new(id),
            Dialogs::new(),
            Files::new(),
            Permissions::new(),
            Garde::new(id, filtre.clone(), opts.restreint, out.clone()),
        );
        let window_info = WindowInfo::default().set_as_windowless(sys::HWND::default());
        let settings = BrowserSettings {
            windowless_frame_rate: opts.fps,
            // Fond opaque : pas de transparence à gérer côté jeu.
            background_color: 0xff00_0000,
            ..Default::default()
        };
        let Some(browser) = browser_host_create_browser_sync(
            Some(&window_info),
            Some(&mut client),
            Some(&CefString::from(url)),
            Some(&settings),
            None,
            None,
        ) else {
            eprintln!("onglet {id} : création impossible");
            return None;
        };
        let host = browser.host()?;
        eprintln!("onglet {id} : ouvert ({url})");
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
    // Dernier envoi d'une image de chaque onglet.
    let mut last: HashMap<u32, Instant> = HashMap::new();
    loop {
        let (events, frames) = {
            let Ok(mut o) = lock.lock() else { return };
            // Images à envoyer : celles des onglets dont la dernière est assez ancienne
            // (les autres attendent, remplacées par les suivantes).
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
            return; // le jeu est fermé
        }
    }
}

fn main() {
    // Version de l'API de CEF dont ce programme a été compilé : avant tout autre appel.
    let _ = api_hash(sys::CEF_API_VERSION_LAST, 0);
    let args = args::Args::new();
    let opts = Options::parse();
    let mut app = BrowserApp::new(opts.restreint);

    // Sous-processus de Chromium (rendu, GPU…) : il fait son travail puis s'arrête ici.
    let code = execute_process(
        Some(args.as_main_args()),
        Some(&mut app),
        std::ptr::null_mut(),
    );
    if code >= 0 {
        std::process::exit(code);
    }

    // (v214) Les commandes du jeu et les demandes de Chromium arrivent au fil principal par
    // le même canal (voir `Pompe`) : créé avant que Chromium démarre.
    let (tx, rx) = mpsc::channel();
    let _ = POMPE.set(tx.clone());
    // (v214) Horloge de Windows à la milliseconde pour ce programme : une attente demandée
    // dure ce qu'elle doit.
    #[cfg(windows)]
    // SAFETY : appel Windows sans état partagé ; rendu par la fin du programme.
    unsafe {
        let _ = windows::Win32::Media::timeBeginPeriod(1);
    }

    let mut settings = Settings {
        no_sandbox: 1,
        windowless_rendering_enabled: 1,
        external_message_pump: 1,
        persist_session_cookies: if opts.sans_compte { 0 } else { 1 },
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
        eprintln!(
            "minelib-browser : initialisation de CEF impossible (session déjà prise par un \
             autre navigateur ?)"
        );
        std::process::exit(2);
    }
    eprintln!("minelib-browser : prêt");

    let out: Shared = Arc::new((Mutex::new(Outbox::default()), Condvar::new()));
    let writer = {
        let out = out.clone();
        std::thread::spawn(move || send_out(out))
    };
    // Commandes du jeu (fin de l'entrée : le jeu est fermé).
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

    // (v205) Le filtre, partagé par tous les onglets.
    let (filtre, mot) = Filtre::charge(opts.filtre, opts.listes.as_deref());
    let filtre = if opts.sans_compte {
        // (v208) En ligne : on part sans aucune session (personne n'y regarde avec le compte
        // d'un autre), et aucune page de connexion ne s'ouvrira.
        if let Some(cookies) = cookie_manager_get_global_manager(None) {
            cookies.delete_cookies(None, None, None);
            cookies.flush_store(None);
        }
        filtre.sans_compte()
    } else {
        filtre
    };
    let filtre = Arc::new(filtre);
    eprintln!(
        "filtrage des pages : {mot}{}",
        if opts.sans_compte { " ; en ligne, sans compte" } else { "" }
    );
    // (v213) Volume des sons du navigateur dans Windows (le lecteur de YouTube reste à fond).
    let volume = volume::Volume::new(volume::from_pct(opts.volume));
    volume.start();
    // Boucle de Chromium sur ce fil (le fil « UI » de CEF) : les commandes y sont jouées.
    let mut tabs: HashMap<u32, Tab> = HashMap::new();
    let left = MouseButtonType::from(sys::cef_mouse_button_type_t::MBT_LEFT);
    // (v214) Prochain moment où Chromium veut travailler. Filet : au moins toutes les
    // `FILET` même sans demande (ce que CEF conseille).
    const FILET: Duration = Duration::from_millis(33);
    let mut next: Option<Instant> = Some(Instant::now());
    'run: loop {
        let wait = next
            .map_or(FILET, |n| n.saturating_duration_since(Instant::now()))
            .min(FILET);
        let first = match rx.recv_timeout(wait) {
            Ok(cmd) => Some(cmd),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(mpsc::RecvTimeoutError::Disconnected) => break 'run,
        };
        if first.is_none() && next.is_none() {
            next = Some(Instant::now());
        }
        for cmd in first.into_iter().chain(std::iter::from_fn(|| rx.try_recv().ok())) {
            match cmd {
                Cmd::Quit => break 'run,
                Cmd::Pump(ms) => {
                    let at = Instant::now() + Duration::from_millis(ms.max(0) as u64);
                    next = Some(next.map_or(at, |n| n.min(at)));
                }
                // (v205) Une adresse refusée n'arrive même pas à Chromium : le jeu le
                // sait tout de suite, l'onglet ne bouge pas, et un onglet neuf ne s'ouvre
                // pas du tout.
                Cmd::Open(id, url) => match filtre.juge(&url) {
                    Err(refus) => {
                        eprintln!("onglet {id} : ouverture refusée ({}) : {url}", refus.raison());
                        push_event(&out, *b"MLBX", id, refus as u32, Vec::new());
                    }
                    Ok(()) => match tabs.get(&id) {
                        Some(t) => {
                            if let Some(frame) = t.browser.main_frame() {
                                frame.load_url(Some(&CefString::from(url.as_str())));
                            }
                        }
                        None => {
                            if let Ok(mut o) = out.0.lock() {
                                o.fps.insert(id, opts.fps);
                            }
                            if let Some(t) = Tab::open(id, &url, &opts, &filtre, &out) {
                                tabs.insert(id, t);
                            }
                        }
                    },
                },
                Cmd::Close(id) => {
                    if let Some(t) = tabs.remove(&id) {
                        t.host.close_browser(1);
                    }
                    if let Ok(mut o) = out.0.lock() {
                        o.frames.remove(&id);
                        o.fps.remove(&id);
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
                Cmd::Fps(id, fps) => {
                    let fps = fps.clamp(1, MAX_FPS);
                    eprintln!("{} onglet {id} : {fps} images/s", heure());
                    if let Some(t) = tabs.get(&id) {
                        t.host.set_windowless_frame_rate(fps);
                    }
                    if let Ok(mut o) = out.0.lock() {
                        o.fps.insert(id, fps);
                    }
                }
                Cmd::Eval(id, code) => {
                    if let Some(t) = tabs.get(&id) {
                        t.js(&code);
                    }
                }
                Cmd::Volume(pct) => volume.set(volume::from_pct(pct)),
                // (Par la page : `go_back` de CEF ne faisait rien sur ces onglets hors écran.)
                Cmd::Back(id) => {
                    if let Some(t) = tabs.get(&id) {
                        t.js("history.back()");
                    }
                }
                Cmd::Forward(id) => {
                    if let Some(t) = tabs.get(&id) {
                        t.js("history.forward()");
                    }
                }
            }
        }
        if next.is_some_and(|n| n <= Instant::now()) {
            next = None;
            do_message_loop_work();
        }
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
        assert_eq!(parse_cmd("fps 3 60"), Some(Cmd::Fps(3, 60)));
        assert_eq!(parse_cmd("fps 3"), None);
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
