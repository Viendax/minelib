//! (v261) Le navigateur des écrans sur Edge (WebView2), pour Windows.
//!
//! Chaque onglet est un WebView2 « en composition » : il dessine dans un visuel de
//! Windows (`ContainerVisual`) accroché à une fenêtre cachée de ce programme, et
//! `Windows.Graphics.Capture` en tire les images (sur la carte graphique, copiées ensuite en
//! mémoire pour le jeu). Si aucune image n'arrive ainsi, l'onglet passe à la diffusion
//! d'images du protocole des outils de développement (`Page.startScreencast`, JPEG).
//!
//! La souris passe par `SendMouseInput`, le clavier par le protocole des outils de
//! développement (`Input.insertText`, `Input.dispatchKeyEvent`) : rien n'a besoin du focus de
//! Windows, que le jeu garde. Le filtre est celui de l'ancien navigateur (`filtre.rs`) ; le
//! DNS familial de Cloudflare, qu'Edge ne laisse pas régler, est interrogé ici pour chaque
//! nouvel hôte (sauf les grands sites sûrs), et un hôte qu'il refuse n'est pas chargé.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::c_void;
use std::path::Path;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use webview2_com::Microsoft::Web::WebView2::Win32::*;
use webview2_com::{
    take_pwstr, wait_with_pump, AddScriptToExecuteOnDocumentCreatedCompletedHandler,
    BasicAuthenticationRequestedEventHandler, CallDevToolsProtocolMethodCompletedHandler,
    CoreWebView2EnvironmentOptions, CreateCoreWebView2CompositionControllerCompletedHandler,
    CreateCoreWebView2EnvironmentCompletedHandler, DevToolsProtocolEventReceivedEventHandler,
    DownloadStartingEventHandler, HistoryChangedEventHandler,
    LaunchingExternalUriSchemeEventHandler, NavigationCompletedEventHandler,
    NavigationStartingEventHandler, NewWindowRequestedEventHandler,
    PermissionRequestedEventHandler, ProcessFailedEventHandler, WebMessageReceivedEventHandler,
    WebResourceRequestedEventHandler,
};
use windows::core::{Interface, BOOL, HRESULT, HSTRING, PCWSTR, PWSTR};
use windows_numerics::Vector2;
use windows::Graphics::Capture::{Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::SizeInt32;
use windows::UI::Composition::Desktop::DesktopWindowTarget;
use windows::UI::Composition::{Compositor, ContainerVisual, Visual};
use windows::Win32::Foundation::{HMODULE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Multithread, ID3D11Texture2D,
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAPPED_SUBRESOURCE,
    D3D11_MAP_READ, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryW};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::System::WinRT::Composition::ICompositorDesktopInterop;
use windows::Win32::System::WinRT::Direct3D11::{
    CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
};
use windows::Win32::System::WinRT::{
    CreateDispatcherQueueController, DispatcherQueueOptions, DQTAT_COM_STA, DQTYPE_THREAD_CURRENT,
};
use windows::Win32::UI::Shell::SHCreateMemStream;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, MsgWaitForMultipleObjects,
    PeekMessageW, PostThreadMessageW, RegisterClassW, ShowWindow, TranslateMessage, MSG,
    PM_REMOVE, QS_ALLINPUT, SW_SHOWNOACTIVATE, WM_APP, WM_QUIT, WNDCLASSW, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_POPUP,
};

use super::filtre::{self, Filtre, Refus};
use super::proto::{self, heure, Cmd, Message, Options, Shared};
use super::scripts;
use super::volume;

/// Code de sortie : Edge WebView2 n'est pas installé (le jeu lance son installation).
pub const SANS_WEBVIEW2: i32 = 3;
/// Code de sortie : la session (dossier) n'a pas pu s'ouvrir (le jeu en prend une autre).
pub const SESSION_PRISE: i32 = 2;

/// Le chargeur de WebView2 de Microsoft (`WebView2Loader.dll`, 160 Ko), dans ce programme :
/// écrit à côté de la session et chargé à la demande. Le jeu n'en dépend jamais pour
/// démarrer.
const CHARGEUR: &[u8] = include_bytes!("WebView2Loader.dll");

type CreerEnv = unsafe extern "system" fn(PCWSTR, PCWSTR, *mut c_void, *mut c_void) -> HRESULT;
type VersionEdge = unsafe extern "system" fn(PCWSTR, *mut PWSTR) -> HRESULT;

struct Chargeur {
    creer: CreerEnv,
    version: VersionEdge,
}

impl Chargeur {
    fn charger(dossier: &Path) -> Result<Self, String> {
        let dll = dossier.join("WebView2Loader.dll");
        let a_jour = std::fs::metadata(&dll).is_ok_and(|m| m.len() == CHARGEUR.len() as u64);
        if !a_jour {
            std::fs::create_dir_all(dossier).map_err(|e| format!("dossier {}: {e}", dossier.display()))?;
            let tmp = dossier.join("WebView2Loader.dll.tmp");
            std::fs::write(&tmp, CHARGEUR).map_err(|e| format!("écriture du chargeur : {e}"))?;
            let _ = std::fs::remove_file(&dll);
            std::fs::rename(&tmp, &dll).map_err(|e| format!("chargeur : {e}"))?;
        }
        // SAFETY : bibliothèque de Microsoft, signée, aux fonctions documentées.
        unsafe {
            let h = LoadLibraryW(&HSTRING::from(dll.as_os_str()))
                .map_err(|e| format!("chargement de WebView2Loader.dll : {e}"))?;
            let creer = GetProcAddress(h, windows::core::s!("CreateCoreWebView2EnvironmentWithOptions"))
                .ok_or("CreateCoreWebView2EnvironmentWithOptions absente")?;
            let version = GetProcAddress(h, windows::core::s!("GetAvailableCoreWebView2BrowserVersionString"))
                .ok_or("GetAvailableCoreWebView2BrowserVersionString absente")?;
            Ok(Self {
                creer: std::mem::transmute::<unsafe extern "system" fn() -> isize, CreerEnv>(creer),
                version: std::mem::transmute::<unsafe extern "system" fn() -> isize, VersionEdge>(version),
            })
        }
    }

    /// Version d'Edge WebView2 installée, s'il l'est.
    fn version(&self) -> Option<String> {
        let mut v = PWSTR::null();
        // SAFETY : fonction du chargeur ; la chaîne rendue est libérée par `take_pwstr`.
        let hr = unsafe { (self.version)(PCWSTR::null(), &mut v) };
        if hr.is_err() || v.is_null() {
            return None;
        }
        Some(take_pwstr(v))
    }
}

/// Lit une chaîne rendue par WebView2 (`Uri`, `Source`…).
fn chaine(f: impl FnOnce(*mut PWSTR) -> windows::core::Result<()>) -> String {
    let mut p = PWSTR::null();
    if f(&mut p).is_err() || p.is_null() {
        return String::new();
    }
    take_pwstr(p)
}

fn wstr(s: &str) -> HSTRING {
    HSTRING::from(s)
}

/// Les grands sites sûrs : pas besoin de demander au DNS familial (des dizaines d'hôtes par
/// vidéo, chez YouTube).
const SURS: &[&str] = &[
    "youtube.com",
    "youtu.be",
    "youtube-nocookie.com",
    "ytimg.com",
    "ggpht.com",
    "googlevideo.com",
    "google.com",
    "google.fr",
    "gstatic.com",
    "googleapis.com",
    "googleusercontent.com",
    "googlesyndication.com",
    "googletagservices.com",
    "googleadservices.com",
    "doubleclick.net",
    "2mdn.net",
    "twitch.tv",
    "ttvnw.net",
    "jtvnw.net",
    "twitchcdn.net",
    "twitchsvc.net",
    "live-video.net",
    "vimeo.com",
    "vimeocdn.com",
    "dailymotion.com",
    "dmcdn.net",
    "dm-event.net",
    "microsoft.com",
    "msftconnecttest.com",
];

fn sur(hote: &str) -> bool {
    SURS.iter()
        .any(|d| hote == *d || hote.strip_suffix(d).is_some_and(|r| r.ends_with('.')))
}

/// Le DNS familial de Cloudflare refuse-t-il cet hôte (adultes, logiciels malveillants) ?
/// `None` : pas de réponse (on laisse passer).
fn famille_refuse(hote: &str) -> Option<bool> {
    let url = format!("https://family.cloudflare-dns.com/dns-query?name={hote}&type=A");
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(4)))
        .build()
        .new_agent();
    let mut r = agent.get(&url).header("accept", "application/dns-json").call().ok()?;
    let texte = r.body_mut().with_config().limit(64 * 1024).read_to_string().ok()?;
    let v: serde_json::Value = serde_json::from_str(&texte).ok()?;
    let reponses = v.get("Answer").and_then(|a| a.as_array());
    Some(reponses.is_some_and(|a| {
        a.iter().any(|x| {
            x.get("type").and_then(|t| t.as_u64()) == Some(1)
                && x.get("data").and_then(|d| d.as_str()) == Some("0.0.0.0")
        })
    }))
}

/// Une ressource en attente du verdict du DNS familial.
struct EnAttente {
    deferral: ICoreWebView2Deferral,
    args: ICoreWebView2WebResourceRequestedEventArgs,
    onglet: u32,
    principale: bool,
    url: String,
}

/// Ce qui est partagé entre la boucle et les gestionnaires d'évènements (tous sur ce fil).
#[derive(Default)]
struct Etat {
    /// Onglets dont le WebView2 vient d'être créé.
    crees: Vec<(u32, Option<ICoreWebView2CompositionController>)>,
    /// Onglets dont les scripts sont posés : leur première page peut s'ouvrir.
    prets: Vec<u32>,
    /// Pages à ouvrir hors d'un gestionnaire (renvoi au lecteur, SafeSearch).
    a_ouvrir: Vec<(u32, String)>,
    /// Images de secours (diffusion JPEG) : onglet, image BGRA.
    secours: Vec<(u32, u32, u32, Vec<u8>)>,
    /// DNS familial : verdicts (refusé ?) et ressources en attente, par hôte.
    verdicts: HashMap<String, (bool, Instant)>,
    attente: HashMap<String, Vec<EnAttente>>,
    /// Hôtes à vérifier (pris par les fils du DNS familial).
    a_verifier: Vec<String>,
    /// Une pub joue dans le lecteur de l'onglet.
    pubs: HashMap<u32, bool>,
    /// Dernière adresse de page principale demandée, par onglet.
    principales: HashMap<u32, String>,
    /// Le processus du navigateur est tombé.
    tombe: bool,
}

type Partage = Rc<RefCell<Etat>>;

/// Ce qui sert à copier les images de la carte graphique.
struct Gpu {
    device: ID3D11Device,
    ctx: ID3D11DeviceContext,
    winrt: IDirect3DDevice,
}

impl Gpu {
    fn new() -> windows::core::Result<Self> {
        let mut device = None;
        let mut ctx = None;
        // SAFETY : création documentée ; repli sur WARP (logiciel) sans carte.
        unsafe {
            let r = D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut ctx),
            );
            if r.is_err() {
                D3D11CreateDevice(
                    None,
                    D3D_DRIVER_TYPE_WARP,
                    HMODULE::default(),
                    D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                    None,
                    D3D11_SDK_VERSION,
                    Some(&mut device),
                    None,
                    Some(&mut ctx),
                )?;
            }
        }
        let device: ID3D11Device = device.ok_or_else(windows::core::Error::empty)?;
        let ctx: ID3D11DeviceContext = ctx.ok_or_else(windows::core::Error::empty)?;
        if let Ok(mt) = device.cast::<ID3D11Multithread>() {
            // SAFETY : réglage documenté.
            unsafe {
                let _ = mt.SetMultithreadProtected(true);
            }
        }
        let dxgi: IDXGIDevice = device.cast()?;
        // SAFETY : conversion documentée vers le périphérique WinRT de la capture.
        let winrt: IDirect3DDevice = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi)? }.cast()?;
        Ok(Self { device, ctx, winrt })
    }
}

/// La capture des images d'un onglet.
struct Capture {
    _item: GraphicsCaptureItem,
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    taille: SizeInt32,
    staging: Option<(ID3D11Texture2D, u32, u32)>,
}

impl Capture {
    fn new(gpu: &Gpu, visual: &ContainerVisual, w: i32, h: i32) -> windows::core::Result<Self> {
        let v: Visual = visual.cast()?;
        let item = GraphicsCaptureItem::CreateFromVisual(&v)?;
        let taille = SizeInt32 { Width: w, Height: h };
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &gpu.winrt,
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            2,
            taille,
        )?;
        let session = pool.CreateCaptureSession(&item)?;
        let _ = session.SetIsBorderRequired(false);
        let _ = session.SetIsCursorCaptureEnabled(false);
        session.StartCapture()?;
        Ok(Self {
            _item: item,
            pool,
            session,
            taille,
            staging: None,
        })
    }

    /// La dernière image arrivée, s'il y en a une, copiée en mémoire si `copier`.
    fn prendre(&mut self, gpu: &Gpu, copier: bool) -> Option<Option<(u32, u32, Vec<u8>)>> {
        let mut derniere = None;
        while let Ok(f) = self.pool.TryGetNextFrame() {
            derniere = Some(f);
        }
        let frame = derniere?;
        if let Ok(t) = frame.ContentSize() {
            if t.Width > 0 && t.Height > 0 && (t.Width != self.taille.Width || t.Height != self.taille.Height) {
                // La page a changé de taille : le tampon suit.
                self.taille = t;
                let _ = self.pool.Recreate(&gpu.winrt, DirectXPixelFormat::B8G8R8A8UIntNormalized, 2, t);
            }
        }
        if !copier {
            return Some(None);
        }
        let copie = (|| -> windows::core::Result<(u32, u32, Vec<u8>)> {
            let surface = frame.Surface()?;
            let acces: IDirect3DDxgiInterfaceAccess = surface.cast()?;
            // SAFETY : appels Direct3D 11 documentés, sur la texture de la capture.
            unsafe {
                let tex: ID3D11Texture2D = acces.GetInterface()?;
                let mut desc = D3D11_TEXTURE2D_DESC::default();
                tex.GetDesc(&mut desc);
                let (w, h) = (desc.Width, desc.Height);
                if self.staging.as_ref().is_none_or(|s| s.1 != w || s.2 != h) {
                    let mut sd = desc;
                    sd.Usage = D3D11_USAGE_STAGING;
                    sd.BindFlags = 0;
                    sd.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
                    sd.MiscFlags = 0;
                    sd.MipLevels = 1;
                    sd.ArraySize = 1;
                    let mut st = None;
                    gpu.device.CreateTexture2D(&sd, None, Some(&mut st))?;
                    self.staging = st.map(|t| (t, w, h));
                }
                let Some((st, _, _)) = self.staging.as_ref() else {
                    return Err(windows::core::Error::empty());
                };
                gpu.ctx.CopyResource(st, &tex);
                let mut map = D3D11_MAPPED_SUBRESOURCE::default();
                gpu.ctx.Map(st, 0, D3D11_MAP_READ, 0, Some(&mut map))?;
                let ligne = (w * 4) as usize;
                let mut px = vec![0u8; ligne * h as usize];
                for y in 0..h as usize {
                    let src = std::slice::from_raw_parts(
                        (map.pData as *const u8).add(y * map.RowPitch as usize),
                        ligne,
                    );
                    px[y * ligne..(y + 1) * ligne].copy_from_slice(src);
                }
                gpu.ctx.Unmap(st, 0);
                Ok((w, h, px))
            }
        })();
        match copie {
            Ok(i) => Some(Some(i)),
            Err(e) => {
                eprintln!("{} capture : copie impossible ({e})", heure());
                Some(None)
            }
        }
    }
}

/// Un onglet (un écran du jeu, ou la recherche à côté d'un lecteur).
struct Onglet {
    controleur: ICoreWebView2CompositionController,
    ctrl: ICoreWebView2Controller,
    vue: ICoreWebView2,
    visuel: ContainerVisual,
    capture: Option<Capture>,
    /// Images reçues de la capture, et depuis quand la page est ouverte.
    images: u64,
    ouvert: Instant,
    derniere: Option<Instant>,
    /// La diffusion JPEG de secours est lancée.
    secours: bool,
    bilan: bool,
}

/// Un onglet en cours de création.
struct Attente {
    url: String,
    taille: (i32, i32),
    cote: bool,
    cmds: Vec<Cmd>,
}

struct Navigateur {
    opts: Options,
    env: ICoreWebView2Environment,
    fenetre: HWND,
    compositeur: Compositor,
    racine: ContainerVisual,
    _cible: DesktopWindowTarget,
    gpu: Option<Gpu>,
    filtre: Rc<Filtre>,
    out: Shared,
    etat: Partage,
    onglets: HashMap<u32, Onglet>,
    attente: HashMap<u32, Attente>,
    cotes: HashMap<u32, (i32, i32)>,
    volume: volume::Volume,
    /// Première adresse d'un onglet, ouverte quand ses scripts sont posés.
    attente_url: HashMap<u32, String>,
}

/// Le gestionnaire « rien à faire » d'un appel au protocole des outils de développement.
fn sans_reponse() -> ICoreWebView2CallDevToolsProtocolMethodCompletedHandler {
    CallDevToolsProtocolMethodCompletedHandler::create(Box::new(|_, _| Ok(())))
}

fn cdp(vue: &ICoreWebView2, methode: &str, json: &str) {
    // SAFETY : appel WebView2 documenté.
    unsafe {
        let _ = vue.CallDevToolsProtocolMethod(&wstr(methode), &wstr(json), &sans_reponse());
    }
}

fn script(vue: &ICoreWebView2, js: &str) {
    // SAFETY : appel WebView2 documenté.
    unsafe {
        let _ = vue.ExecuteScript(&wstr(js), None);
    }
}

impl Navigateur {
    fn ouvrir(&mut self, id: u32, url: String) {
        if let Err(refus) = self.filtre.juge(&url) {
            eprintln!("onglet {id} : ouverture refusée ({}) : {url}", refus.raison());
            proto::push_event(&self.out, *b"MLBX", id, refus as u32, Vec::new());
            return;
        }
        // Onglet là mais scripts pas encore posés : c'est cette adresse qui s'ouvrira.
        if let Some(u) = self.attente_url.get_mut(&id) {
            *u = url;
            return;
        }
        if let Some(o) = self.onglets.get(&id) {
            self.etat.borrow_mut().principales.insert(id, url.clone());
            // SAFETY : appel WebView2 documenté.
            unsafe {
                let _ = o.vue.Navigate(&wstr(&url));
            }
            return;
        }
        if let Some(a) = self.attente.get_mut(&id) {
            a.url = url;
            return;
        }
        let cote = self.cotes.get(&id).copied();
        let taille = cote.unwrap_or((self.opts.width, self.opts.height));
        if let Ok(mut o) = self.out.0.lock() {
            o.fps.insert(id, self.opts.fps);
        }
        let etat = self.etat.clone();
        let handler = CreateCoreWebView2CompositionControllerCompletedHandler::create(Box::new(
            move |r, c| {
                if let Err(e) = &r {
                    eprintln!("onglet {id} : création impossible ({e})");
                }
                etat.borrow_mut().crees.push((id, c));
                Ok(())
            },
        ));
        let env3: windows::core::Result<ICoreWebView2Environment3> = self.env.cast();
        match env3 {
            Ok(env3) => {
                // SAFETY : appel WebView2 documenté.
                let r = unsafe { env3.CreateCoreWebView2CompositionController(self.fenetre, &handler) };
                if let Err(e) = r {
                    eprintln!("onglet {id} : création impossible ({e})");
                    return;
                }
                self.attente.insert(
                    id,
                    Attente {
                        url,
                        taille,
                        cote: cote.is_some(),
                        cmds: Vec::new(),
                    },
                );
            }
            Err(e) => eprintln!("onglet {id} : Edge trop ancien pour la composition ({e})"),
        }
    }

    /// Le WebView2 de l'onglet `id` est créé : réglages, évènements, scripts, capture.
    fn creer(&mut self, id: u32, controleur: ICoreWebView2CompositionController) -> windows::core::Result<()> {
        let Some(a) = self.attente.remove(&id) else {
            // Fermé entre-temps.
            // SAFETY : appel WebView2 documenté.
            unsafe {
                if let Ok(c) = controleur.cast::<ICoreWebView2Controller>() {
                    let _ = c.Close();
                }
            }
            return Ok(());
        };
        let (w, h) = a.taille;
        let ctrl: ICoreWebView2Controller = controleur.cast()?;
        let visuel = self.compositeur.CreateContainerVisual()?;
        visuel.SetSize(Vector2 { X: w as f32, Y: h as f32 })?;
        self.racine.Children()?.InsertAtTop(&visuel)?;
        // SAFETY : appels WebView2 documentés, sur ce fil (celui de l'interface).
        let vue = unsafe {
            controleur.SetRootVisualTarget(&visuel.cast::<windows::core::IUnknown>()?)?;
            if let Ok(c3) = ctrl.cast::<ICoreWebView2Controller3>() {
                let _ = c3.SetBoundsMode(COREWEBVIEW2_BOUNDS_MODE_USE_RAW_PIXELS);
                let _ = c3.SetShouldDetectMonitorScaleChanges(false);
                let _ = c3.SetRasterizationScale(1.0);
            }
            ctrl.SetBounds(RECT { left: 0, top: 0, right: w, bottom: h })?;
            ctrl.SetIsVisible(true)?;
            if let Ok(c2) = ctrl.cast::<ICoreWebView2Controller2>() {
                let _ = c2.SetDefaultBackgroundColor(COREWEBVIEW2_COLOR { A: 255, R: 255, G: 255, B: 255 });
            }
            ctrl.CoreWebView2()?
        };
        self.regler(&vue)?;
        self.ecouter(id, &vue, a.cote)?;
        // Les scripts avant la première page : elle s'ouvre quand ils sont tous posés.
        let mut js: Vec<String> = Vec::new();
        if self.opts.site {
            js.push("window.__mlb_site=1;".into());
        }
        if a.cote {
            js.push("window.__mlb_cote=1;".into());
        }
        js.push(scripts::PAGE_JS.into());
        if self.opts.scroll {
            js.push(scripts::SCROLL_JS.into());
        }
        js.push(scripts::PUB_JS.into());
        let reste = Rc::new(Cell::new(js.len()));
        for s in &js {
            let etat = self.etat.clone();
            let reste = reste.clone();
            let handler = AddScriptToExecuteOnDocumentCreatedCompletedHandler::create(Box::new(
                move |_, _| {
                    reste.set(reste.get().saturating_sub(1));
                    if reste.get() == 0 {
                        etat.borrow_mut().prets.push(id);
                    }
                    Ok(())
                },
            ));
            // SAFETY : appel WebView2 documenté.
            unsafe {
                vue.AddScriptToExecuteOnDocumentCreated(&wstr(s), &handler)?;
            }
        }
        cdp(&vue, "Page.enable", "{}");
        cdp(&vue, "Page.setInterceptFileChooserDialog", r#"{"enabled":true}"#);
        cdp(&vue, "Emulation.setFocusEmulationEnabled", r#"{"enabled":true}"#);
        let capture = match self.gpu.as_ref() {
            Some(gpu) => match Capture::new(gpu, &visuel, w, h) {
                Ok(c) => Some(c),
                Err(e) => {
                    eprintln!("onglet {id} : capture impossible ({e}), diffusion JPEG à la place");
                    None
                }
            },
            None => None,
        };
        let secours = capture.is_none();
        self.etat.borrow_mut().principales.insert(id, a.url.clone());
        self.onglets.insert(
            id,
            Onglet {
                controleur,
                ctrl,
                vue: vue.clone(),
                visuel,
                capture,
                images: 0,
                ouvert: Instant::now(),
                derniere: None,
                secours: false,
                bilan: false,
            },
        );
        if secours {
            self.diffuser(id);
        }
        // L'adresse est ouverte quand les scripts sont posés (`prets`).
        self.attente_url.insert(id, a.url);
        for c in a.cmds {
            self.commande(c);
        }
        eprintln!(
            "onglet {id} : ouvert ({w} × {h}){}",
            if a.cote { ", recherche à côté" } else { "" }
        );
        Ok(())
    }

    /// Réglages d'une page : rien qui ouvre une fenêtre ou un menu de Windows.
    fn regler(&self, vue: &ICoreWebView2) -> windows::core::Result<()> {
        // SAFETY : appels WebView2 documentés.
        unsafe {
            let s = vue.Settings()?;
            let _ = s.SetAreDevToolsEnabled(false);
            let _ = s.SetAreDefaultContextMenusEnabled(false);
            let _ = s.SetAreDefaultScriptDialogsEnabled(false);
            let _ = s.SetIsStatusBarEnabled(false);
            let _ = s.SetIsZoomControlEnabled(false);
            let _ = s.SetIsWebMessageEnabled(true);
            let _ = s.SetIsScriptEnabled(true);
            if let Ok(s3) = s.cast::<ICoreWebView2Settings3>() {
                let _ = s3.SetAreBrowserAcceleratorKeysEnabled(false);
            }
            if let Ok(s4) = s.cast::<ICoreWebView2Settings4>() {
                let _ = s4.SetIsPasswordAutosaveEnabled(false);
                let _ = s4.SetIsGeneralAutofillEnabled(false);
            }
            if let Ok(s5) = s.cast::<ICoreWebView2Settings5>() {
                let _ = s5.SetIsPinchZoomEnabled(false);
            }
            if let Ok(s6) = s.cast::<ICoreWebView2Settings6>() {
                let _ = s6.SetIsSwipeNavigationEnabled(false);
            }
        }
        Ok(())
    }

    /// Les évènements d'une page : filtre, renvois, messages des scripts, historique,
    /// fenêtres, téléchargements, permissions.
    fn ecouter(&self, id: u32, vue: &ICoreWebView2, cote: bool) -> windows::core::Result<()> {
        let mut jeton = Default::default();
        let site = self.opts.site;
        // SAFETY : appels WebView2 documentés ; les gestionnaires tournent sur ce fil.
        unsafe {
            // Navigation de la page : filtre, lecteur (en ligne), SafeSearch.
            {
                let filtre = self.filtre.clone();
                let out = self.out.clone();
                let etat = self.etat.clone();
                vue.add_NavigationStarting(
                    &NavigationStartingEventHandler::create(Box::new(move |_, args| {
                        let Some(args) = args else { return Ok(()) };
                        let url = chaine(|p| args.Uri(p));
                        if let Err(refus) = filtre.juge(&url) {
                            eprintln!("onglet {id} : page refusée ({}) : {url}", refus.raison());
                            let _ = args.SetCancel(true);
                            proto::push_event(&out, *b"MLBX", id, refus as u32, Vec::new());
                            return Ok(());
                        }
                        // (v210) Une vidéo du site : le lecteur officiel ; (v246) en ligne seulement.
                        let neuf = if site { None } else { filtre::vers_lecteur(&url) }
                            .or_else(|| filtre::recherche_google(&url).then(|| filtre::avec_safe(&url)).flatten());
                        if let Some(neuf) = neuf.filter(|n| *n != url) {
                            let _ = args.SetCancel(true);
                            etat.borrow_mut().a_ouvrir.push((id, neuf));
                            return Ok(());
                        }
                        etat.borrow_mut().principales.insert(id, url);
                        Ok(())
                    })),
                    &mut jeton,
                )?;
            }
            // Les cadres : filtre seul.
            {
                let filtre = self.filtre.clone();
                vue.add_FrameNavigationStarting(
                    &NavigationStartingEventHandler::create(Box::new(move |_, args| {
                        let Some(args) = args else { return Ok(()) };
                        let url = chaine(|p| args.Uri(p));
                        if let Err(refus) = filtre.juge(&url) {
                            eprintln!("onglet {id} : cadre refusé ({}) : {url}", refus.raison());
                            let _ = args.SetCancel(true);
                        }
                        Ok(())
                    })),
                    &mut jeton,
                )?;
            }
            // Tout ce que la page charge : filtre, page du lecteur servie d'ici, DNS familial.
            {
                let filtre = self.filtre.clone();
                let etat = self.etat.clone();
                let env = self.env.clone();
                let out = self.out.clone();
                let filtre_ajoute = match vue.cast::<ICoreWebView2_22>() {
                    Ok(v22) => v22.AddWebResourceRequestedFilterWithRequestSourceKinds(
                        &wstr("*"),
                        COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL,
                        COREWEBVIEW2_WEB_RESOURCE_REQUEST_SOURCE_KINDS_ALL,
                    ),
                    Err(_) => vue.AddWebResourceRequestedFilter(&wstr("*"), COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL),
                };
                if let Err(e) = filtre_ajoute {
                    eprintln!("onglet {id} : filtre des ressources impossible ({e})");
                }
                vue.add_WebResourceRequested(
                    &WebResourceRequestedEventHandler::create(Box::new(move |_, args| {
                        let Some(args) = args else { return Ok(()) };
                        ressource(id, &args, &filtre, &etat, &env, &out);
                        Ok(())
                    })),
                    &mut jeton,
                )?;
            }
            // Les messages des scripts de la page.
            {
                let out = self.out.clone();
                let etat = self.etat.clone();
                vue.add_WebMessageReceived(
                    &WebMessageReceivedEventHandler::create(Box::new(move |_, args| {
                        let Some(args) = args else { return Ok(()) };
                        let texte = chaine(|p| args.TryGetWebMessageAsString(p));
                        let source = chaine(|p| args.Source(p));
                        message(id, &texte, &source, &out, &etat);
                        Ok(())
                    })),
                    &mut jeton,
                )?;
            }
            // (v213) Pages précédente et suivante.
            {
                let out = self.out.clone();
                vue.add_HistoryChanged(
                    &HistoryChangedEventHandler::create(Box::new(move |sender, _| {
                        let Some(v) = sender else { return Ok(()) };
                        let (mut b, mut f) = (BOOL(0), BOOL(0));
                        let _ = v.CanGoBack(&mut b);
                        let _ = v.CanGoForward(&mut f);
                        let bits = u32::from(b.as_bool()) | (u32::from(f.as_bool()) << 1);
                        proto::push_event(&out, *b"MLBH", id, bits, Vec::new());
                        Ok(())
                    })),
                    &mut jeton,
                )?;
            }
            // Chargement raté : au journal.
            vue.add_NavigationCompleted(
                &NavigationCompletedEventHandler::create(Box::new(move |_, args| {
                    let Some(args) = args else { return Ok(()) };
                    let mut ok = BOOL(0);
                    let _ = args.IsSuccess(&mut ok);
                    if !ok.as_bool() {
                        let mut statut = COREWEBVIEW2_WEB_ERROR_STATUS::default();
                        let _ = args.WebErrorStatus(&mut statut);
                        if statut != COREWEBVIEW2_WEB_ERROR_STATUS_OPERATION_CANCELED {
                            eprintln!("onglet {id} : chargement raté (erreur {})", statut.0);
                        }
                    }
                    Ok(())
                })),
                &mut jeton,
            )?;
            // Pas de fenêtre : un lien « nouvel onglet » cliqué s'ouvre ici, une fenêtre
            // ouverte par un script ne s'ouvre pas.
            {
                let etat = self.etat.clone();
                vue.add_NewWindowRequested(
                    &NewWindowRequestedEventHandler::create(Box::new(move |_, args| {
                        let Some(args) = args else { return Ok(()) };
                        let _ = args.SetHandled(true);
                        let mut clic = BOOL(0);
                        let _ = args.IsUserInitiated(&mut clic);
                        if clic.as_bool() {
                            let url = chaine(|p| args.Uri(p));
                            etat.borrow_mut().a_ouvrir.push((id, url));
                        }
                        Ok(())
                    })),
                    &mut jeton,
                )?;
            }
            // Pas d'autorisation (caméra, micro, notifications, position…).
            vue.add_PermissionRequested(
                &PermissionRequestedEventHandler::create(Box::new(|_, args| {
                    if let Some(args) = args {
                        let _ = args.SetState(COREWEBVIEW2_PERMISSION_STATE_DENY);
                    }
                    Ok(())
                })),
                &mut jeton,
            )?;
            // Pas de téléchargement.
            if let Ok(v4) = vue.cast::<ICoreWebView2_4>() {
                v4.add_DownloadStarting(
                    &DownloadStartingEventHandler::create(Box::new(move |_, args| {
                        if let Some(args) = args {
                            let _ = args.SetCancel(true);
                            let _ = args.SetHandled(true);
                            eprintln!("onglet {id} : téléchargement refusé");
                        }
                        Ok(())
                    })),
                    &mut jeton,
                )?;
            }
            // Pas d'autre programme (mailto:, liens d'applications…).
            if let Ok(v18) = vue.cast::<ICoreWebView2_18>() {
                v18.add_LaunchingExternalUriScheme(
                    &LaunchingExternalUriSchemeEventHandler::create(Box::new(|_, args| {
                        if let Some(args) = args {
                            let _ = args.SetCancel(true);
                        }
                        Ok(())
                    })),
                    &mut jeton,
                )?;
            }
            // Pas de mot de passe demandé par une fenêtre.
            if let Ok(v10) = vue.cast::<ICoreWebView2_10>() {
                v10.add_BasicAuthenticationRequested(
                    &BasicAuthenticationRequestedEventHandler::create(Box::new(|_, args| {
                        if let Some(args) = args {
                            let _ = args.SetCancel(true);
                        }
                        Ok(())
                    })),
                    &mut jeton,
                )?;
            }
            // Le navigateur tombé : le jeu le relance.
            {
                let etat = self.etat.clone();
                vue.add_ProcessFailed(
                    &ProcessFailedEventHandler::create(Box::new(move |_, args| {
                        let Some(args) = args else { return Ok(()) };
                        let mut genre = COREWEBVIEW2_PROCESS_FAILED_KIND::default();
                        let _ = args.ProcessFailedKind(&mut genre);
                        eprintln!("onglet {id} : processus d'Edge tombé (genre {})", genre.0);
                        if genre == COREWEBVIEW2_PROCESS_FAILED_KIND_BROWSER_PROCESS_EXITED {
                            etat.borrow_mut().tombe = true;
                        }
                        Ok(())
                    })),
                    &mut jeton,
                )?;
            }
        }
        let _ = cote;
        Ok(())
    }

    /// La diffusion d'images de secours (JPEG du protocole des outils de développement),
    /// quand la capture ne donne rien.
    fn diffuser(&mut self, id: u32) {
        let Some(o) = self.onglets.get_mut(&id) else { return };
        if o.secours {
            return;
        }
        o.secours = true;
        let vue = o.vue.clone();
        let etat = self.etat.clone();
        // SAFETY : appels WebView2 documentés.
        unsafe {
            let Ok(recepteur) = vue.GetDevToolsProtocolEventReceiver(&wstr("Page.screencastFrame")) else {
                eprintln!("onglet {id} : diffusion d'images impossible");
                return;
            };
            let vue2 = vue.clone();
            let mut jeton = Default::default();
            let _ = recepteur.add_DevToolsProtocolEventReceived(
                &DevToolsProtocolEventReceivedEventHandler::create(Box::new(move |_, args| {
                    let Some(args) = args else { return Ok(()) };
                    let json = chaine(|p| args.ParameterObjectAsJson(p));
                    let Ok(v) = serde_json::from_str::<serde_json::Value>(&json) else { return Ok(()) };
                    if let Some(s) = v.get("sessionId").and_then(|s| s.as_i64()) {
                        cdp(&vue2, "Page.screencastFrameAck", &format!("{{\"sessionId\":{s}}}"));
                    }
                    let Some(data) = v.get("data").and_then(|d| d.as_str()) else { return Ok(()) };
                    if let Some((w, h, px)) = jpeg_bgra(data) {
                        etat.borrow_mut().secours.push((id, w, h, px));
                    }
                    Ok(())
                })),
                &mut jeton,
            );
        }
        let (w, h) = self
            .cotes
            .get(&id)
            .copied()
            .unwrap_or((self.opts.width, self.opts.height));
        cdp(
            &vue,
            "Page.startScreencast",
            &format!("{{\"format\":\"jpeg\",\"quality\":85,\"maxWidth\":{w},\"maxHeight\":{h},\"everyNthFrame\":1}}"),
        );
        eprintln!("onglet {id} : diffusion d'images JPEG (secours)");
    }

    fn fermer(&mut self, id: u32) {
        self.attente.remove(&id);
        self.cotes.remove(&id);
        self.attente_url.remove(&id);
        if let Some(o) = self.onglets.remove(&id) {
            if let Some(c) = &o.capture {
                let _ = c.session.Close();
                let _ = c.pool.Close();
            }
            // SAFETY : appel WebView2 documenté.
            unsafe {
                let _ = o.ctrl.Close();
            }
            if let Ok(enfants) = self.racine.Children() {
                let _ = enfants.Remove(&o.visuel);
            }
            let _ = &o.controleur;
        }
        if let Ok(mut o) = self.out.0.lock() {
            o.frames.remove(&id);
            o.fps.remove(&id);
        }
        let mut e = self.etat.borrow_mut();
        e.pubs.remove(&id);
        e.principales.remove(&id);
    }

    fn commande(&mut self, cmd: Cmd) {
        // Une commande pour un onglet en cours de création attend qu'il soit là.
        let cible = match &cmd {
            Cmd::Move(id, ..)
            | Cmd::Down(id, ..)
            | Cmd::Up(id, ..)
            | Cmd::Wheel(id, ..)
            | Cmd::Text(id, _)
            | Cmd::Key(id, _)
            | Cmd::Blur(id)
            | Cmd::Eval(id, _)
            | Cmd::Back(id)
            | Cmd::Forward(id) => Some(*id),
            _ => None,
        };
        if let Some(id) = cible {
            if let Some(a) = self.attente.get_mut(&id) {
                a.cmds.push(cmd);
                return;
            }
        }
        let point = |x: i32, y: i32| POINT { x, y };
        // SAFETY : appels WebView2 documentés.
        unsafe {
            match cmd {
                Cmd::Quit => {}
                Cmd::Open(id, url) => self.ouvrir(id, url),
                Cmd::Close(id) => self.fermer(id),
                Cmd::Cote(id, w, h) => {
                    self.cotes.insert(id, (w.clamp(64, 3840), h.clamp(64, 2160)));
                }
                Cmd::Move(id, x, y) => {
                    if let Some(o) = self.onglets.get(&id) {
                        let _ = o.controleur.SendMouseInput(
                            COREWEBVIEW2_MOUSE_EVENT_KIND_MOVE,
                            COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_NONE,
                            0,
                            point(x, y),
                        );
                    }
                }
                Cmd::Down(id, x, y) => {
                    if let Some(o) = self.onglets.get(&id) {
                        let _ = o.controleur.SendMouseInput(
                            COREWEBVIEW2_MOUSE_EVENT_KIND_LEFT_BUTTON_DOWN,
                            COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_LEFT_BUTTON,
                            0,
                            point(x, y),
                        );
                    }
                }
                Cmd::Up(id, x, y) => {
                    if let Some(o) = self.onglets.get(&id) {
                        let _ = o.controleur.SendMouseInput(
                            COREWEBVIEW2_MOUSE_EVENT_KIND_LEFT_BUTTON_UP,
                            COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_NONE,
                            0,
                            point(x, y),
                        );
                    }
                }
                Cmd::Wheel(id, x, y, dy) => {
                    if let Some(o) = self.onglets.get(&id) {
                        let _ = o.controleur.SendMouseInput(
                            COREWEBVIEW2_MOUSE_EVENT_KIND_WHEEL,
                            COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_NONE,
                            dy as u32,
                            point(x, y),
                        );
                    }
                }
                Cmd::Text(id, texte) => {
                    if let Some(o) = self.onglets.get(&id) {
                        let json = serde_json::json!({ "text": texte }).to_string();
                        cdp(&o.vue, "Input.insertText", &json);
                    }
                }
                Cmd::Key(id, nom) => {
                    if let (Some(o), Some([bas, haut])) = (self.onglets.get(&id), proto::key_json(&nom)) {
                        cdp(&o.vue, "Input.dispatchKeyEvent", &bas);
                        cdp(&o.vue, "Input.dispatchKeyEvent", &haut);
                    }
                }
                Cmd::Blur(id) => {
                    if let Some(o) = self.onglets.get(&id) {
                        script(&o.vue, scripts::BLUR_JS);
                    }
                }
                Cmd::Eval(id, js) => {
                    if let Some(o) = self.onglets.get(&id) {
                        script(&o.vue, &js);
                    }
                }
                Cmd::Fps(id, fps) => {
                    if let Ok(mut o) = self.out.0.lock() {
                        o.fps.insert(id, fps.clamp(1, proto::MAX_FPS));
                    }
                }
                Cmd::Volume(p) => self.volume.set(volume::from_pct(p)),
                Cmd::Back(id) => {
                    if let Some(o) = self.onglets.get(&id) {
                        let _ = o.vue.GoBack();
                    }
                }
                Cmd::Forward(id) => {
                    if let Some(o) = self.onglets.get(&id) {
                        let _ = o.vue.GoForward();
                    }
                }
            }
        }
    }

    /// Les images des onglets : capture (la dernière de chacun, copiée à ses images par
    /// seconde), ou diffusion de secours.
    fn images(&mut self) {
        let Some(gpu) = self.gpu.as_ref() else {
            return;
        };
        let mut a_secourir = Vec::new();
        for (id, o) in self.onglets.iter_mut() {
            if let Some(cap) = o.capture.as_mut() {
                let du = proto::frame_due(&self.out, *id, o.derniere);
                if let Some(image) = cap.prendre(gpu, du) {
                    o.images += 1;
                    if o.images == 1 {
                        eprintln!(
                            "onglet {id} : première image capturée ({:.1} s après l'ouverture)",
                            o.ouvert.elapsed().as_secs_f32()
                        );
                    }
                    if let Some((w, h, px)) = image {
                        o.derniere = Some(Instant::now());
                        proto::push_frame(&self.out, *id, w, h, px);
                    }
                }
            }
            // Rien de la capture au bout de 4 s : la diffusion de secours.
            if o.images == 0 && !o.secours && o.ouvert.elapsed() > Duration::from_secs(4) {
                a_secourir.push(*id);
            }
            if !o.bilan && o.ouvert.elapsed() > Duration::from_secs(8) {
                o.bilan = true;
                eprintln!(
                    "onglet {id} : {} images capturées en 8 s{}",
                    o.images,
                    if o.secours { " (diffusion de secours lancée)" } else { "" }
                );
            }
        }
        for id in a_secourir {
            eprintln!("onglet {id} : aucune image de la capture, diffusion de secours");
            self.diffuser(id);
        }
    }

    /// Ce que les gestionnaires ont laissé à faire.
    fn suites(&mut self) {
        let (crees, prets, a_ouvrir, secours) = {
            let mut e = self.etat.borrow_mut();
            (
                std::mem::take(&mut e.crees),
                std::mem::take(&mut e.prets),
                std::mem::take(&mut e.a_ouvrir),
                std::mem::take(&mut e.secours),
            )
        };
        for (id, c) in crees {
            match c {
                Some(c) => {
                    if let Err(e) = self.creer(id, c) {
                        eprintln!("onglet {id} : mise en place impossible ({e})");
                        self.fermer(id);
                    }
                }
                None => {
                    self.attente.remove(&id);
                }
            }
        }
        for id in prets {
            if let (Some(url), Some(o)) = (self.attente_url.remove(&id), self.onglets.get(&id)) {
                // SAFETY : appel WebView2 documenté.
                unsafe {
                    let _ = o.vue.Navigate(&wstr(&url));
                }
            }
        }
        for (id, url) in a_ouvrir {
            self.ouvrir(id, url);
        }
        for (id, w, h, px) in secours {
            let du = self
                .onglets
                .get(&id)
                .is_some_and(|o| proto::frame_due(&self.out, id, o.derniere));
            if du {
                if let Some(o) = self.onglets.get_mut(&id) {
                    o.derniere = Some(Instant::now());
                }
                proto::push_frame(&self.out, id, w, h, px);
            }
        }
    }
}

/// Une image JPEG (base64) en BGRA.
fn jpeg_bgra(b64: &str) -> Option<(u32, u32, Vec<u8>)> {
    use base64_lite::decode;
    let octets = decode(b64)?;
    let img = image::load_from_memory_with_format(&octets, image::ImageFormat::Jpeg).ok()?;
    let rgba = img.to_rgba8();
    let (w, h) = (rgba.width(), rgba.height());
    let mut px = rgba.into_raw();
    for p in px.chunks_exact_mut(4) {
        p.swap(0, 2);
        p[3] = 255;
    }
    Some((w, h, px))
}

/// Base64 standard, sans dépendance de plus.
mod base64_lite {
    pub fn decode(s: &str) -> Option<Vec<u8>> {
        let val = |c: u8| -> Option<u32> {
            Some(match c {
                b'A'..=b'Z' => (c - b'A') as u32,
                b'a'..=b'z' => (c - b'a' + 26) as u32,
                b'0'..=b'9' => (c - b'0' + 52) as u32,
                b'+' => 62,
                b'/' => 63,
                _ => return None,
            })
        };
        let octets: Vec<u8> = s.bytes().filter(|c| !c.is_ascii_whitespace() && *c != b'=').collect();
        let mut out = Vec::with_capacity(octets.len() * 3 / 4);
        for bloc in octets.chunks(4) {
            let mut n = 0u32;
            for (i, c) in bloc.iter().enumerate() {
                n |= val(*c)? << (18 - 6 * i);
            }
            out.push((n >> 16) as u8);
            if bloc.len() > 2 {
                out.push((n >> 8) as u8);
            }
            if bloc.len() > 3 {
                out.push(n as u8);
            }
        }
        Some(out)
    }
}

/// Une ressource demandée par une page.
fn ressource(
    id: u32,
    args: &ICoreWebView2WebResourceRequestedEventArgs,
    filtre: &Filtre,
    etat: &Partage,
    env: &ICoreWebView2Environment,
    out: &Shared,
) {
    // SAFETY : appels WebView2 documentés.
    unsafe {
        let Ok(req) = args.Request() else { return };
        let url = chaine(|p| req.Uri(p));
        // (v210) La page du lecteur, servie sans réseau.
        if filtre::est_lecteur(&url) {
            let stream = SHCreateMemStream(Some(scripts::LECTEUR_HTML.as_bytes()));
            if let Ok(r) = env.CreateWebResourceResponse(
                stream.as_ref(),
                200,
                &wstr("OK"),
                &wstr("Content-Type: text/html; charset=utf-8\r\nCache-Control: no-store"),
            ) {
                let _ = args.SetResponse(&r);
            }
            return;
        }
        if filtre.juge(&url).is_err() {
            refuser(args, env);
            return;
        }
        // SafeSearch : une recherche Google part toujours avec `safe=active`.
        if filtre::recherche_google(&url) {
            if let Some(neuf) = filtre::avec_safe(&url) {
                let _ = req.SetUri(&wstr(&neuf));
            }
        }
        let Some(hote) = filtre::hote(&url) else { return };
        if sur(&hote) {
            return;
        }
        let principale = etat.borrow().principales.get(&id).is_some_and(|u| *u == url);
        let verdict = {
            let mut e = etat.borrow_mut();
            e.verdicts.retain(|_, (_, t)| t.elapsed() < Duration::from_secs(3600));
            e.verdicts.get(&hote).map(|v| v.0)
        };
        match verdict {
            Some(false) => {}
            Some(true) => {
                refuser(args, env);
                if principale {
                    eprintln!("onglet {id} : page refusée (DNS familial) : {url}");
                    proto::push_event(out, *b"MLBX", id, Refus::Famille as u32, Vec::new());
                }
            }
            None => {
                let Ok(deferral) = args.GetDeferral() else { return };
                let mut e = etat.borrow_mut();
                let nouveau = !e.attente.contains_key(&hote);
                e.attente.entry(hote.clone()).or_default().push(EnAttente {
                    deferral,
                    args: args.clone(),
                    onglet: id,
                    principale,
                    url,
                });
                if nouveau {
                    e.a_verifier.push(hote);
                }
            }
        }
    }
}

/// Répond « refusé » à une ressource (une petite page pour un document).
fn refuser(args: &ICoreWebView2WebResourceRequestedEventArgs, env: &ICoreWebView2Environment) {
    const PAGE: &str = "<!doctype html><meta charset=utf-8><body style=\"background:#111;color:#bbb;\
font:20px sans-serif;display:flex;align-items:center;justify-content:center;height:90vh\">\
Ce site ne s'ouvre pas sur les écrans.</body>";
    // SAFETY : appels WebView2 documentés.
    unsafe {
        let stream = SHCreateMemStream(Some(PAGE.as_bytes()));
        if let Ok(r) = env.CreateWebResourceResponse(
            stream.as_ref(),
            403,
            &wstr("Forbidden"),
            &wstr("Content-Type: text/html; charset=utf-8"),
        ) {
            let _ = args.SetResponse(&r);
        }
    }
}

/// Un message des scripts d'une page.
fn message(id: u32, texte: &str, source: &str, out: &Shared, etat: &Partage) {
    let Some(m) = proto::parse_message(texte) else { return };
    match m {
        Message::Focus(on) => proto::push_event(out, *b"MLBK", id, u32::from(on), Vec::new()),
        Message::Scroll(f) => proto::push_event(out, *b"MLBW", id, f, Vec::new()),
        Message::Diag(d) => eprintln!("{} onglet {id} : lecteur : {d}", heure()),
        Message::Ordre(o) => {
            // (v253) Seulement de la page du lecteur ou du site de YouTube.
            // (v265) Et de Vimeo, de Dailymotion (recherche à côté, clic sur une vidéo).
            let de_confiance = filtre::est_lecteur(source)
                || filtre::hote(source).is_some_and(|h| {
                    matches!(
                        h.as_str(),
                        "www.youtube.com"
                            | "youtube.com"
                            | "m.youtube.com"
                            | "vimeo.com"
                            | "www.vimeo.com"
                            | "www.dailymotion.com"
                            | "dailymotion.com"
                    )
                });
            if de_confiance {
                proto::push_event(out, *b"MLBO", id, o.len() as u32, o.into_bytes());
            }
        }
        Message::Pub(on) => {
            if filtre::est_lecteur(source) {
                etat.borrow_mut().pubs.insert(id, on);
            }
        }
        Message::State(s) => {
            // (v210) Une pub dans le cadre du lecteur : la page le dit au jeu.
            let s = if etat.borrow().pubs.get(&id).copied().unwrap_or(false) {
                s.replace("\"a\":false", "\"a\":true")
            } else {
                s
            };
            proto::push_event(out, *b"MLBS", id, s.len() as u32, s.into_bytes());
        }
    }
}

unsafe extern "system" fn fenetre_proc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    // SAFETY : procédure par défaut de Windows.
    unsafe { DefWindowProcW(h, m, w, l) }
}

/// La fenêtre (cachée) où vivent les visuels des onglets : hors de tout écran, sans
/// barre des tâches, qui ne prend jamais le focus.
fn fenetre() -> windows::core::Result<HWND> {
    let classe = windows::core::w!("minelib-ecrans-edge");
    // SAFETY : création de fenêtre documentée.
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let wc = WNDCLASSW {
            lpfnWndProc: Some(fenetre_proc),
            hInstance: instance.into(),
            lpszClassName: classe,
            ..Default::default()
        };
        RegisterClassW(&wc);
        // Pour essayer : MINELIB_EDGE_FENETRE=visible la met en haut à gauche de l'écran.
        let visible = std::env::var("MINELIB_EDGE_FENETRE").is_ok_and(|v| v == "visible");
        let (x, y) = if visible { (0, 0) } else { (-24000, -24000) };
        let h = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            classe,
            windows::core::w!("minelib - écrans"),
            WS_POPUP,
            x,
            y,
            1280,
            720,
            None,
            None,
            Some(instance.into()),
            None,
        )?;
        let _ = ShowWindow(h, SW_SHOWNOACTIVATE);
        Ok(h)
    }
}

/// Les arguments de Chromium pour Edge : lecture sans clic (le jeu décide quel écran joue),
/// rien de ralenti pour une fenêtre cachée, SafeSearch par le nom de Google.
fn arguments() -> String {
    let mut a = String::from(
        "--autoplay-policy=no-user-gesture-required \
         --disable-features=CalculateNativeWinOcclusion,msSmartScreenProtection \
         --disable-background-timer-throttling --disable-renderer-backgrounding \
         --disable-backgrounding-occluded-windows \
         --host-resolver-rules=\"MAP www.google.com forcesafesearch.google.com\"",
    );
    // (v248) Diagnostic à la demande : `logs\journal-reseau.demande`.
    let logs = minelib_assets::data_dir().join("logs");
    if logs.join("journal-reseau.demande").is_file() {
        a.push_str(&format!(
            " --log-net-log=\"{}\" --net-log-capture-mode=Default",
            logs.join("journal-reseau.json").display()
        ));
    }
    a
}

/// Le navigateur : `minelib.exe --navigateur-edge <options>`. Rend son code de sortie.
pub fn main(opts: Options) -> i32 {
    eprintln!("{} navigateur Edge : lancement", heure());
    // SAFETY : COM en appartement pour ce fil (celui de l'interface), comme WebView2 le veut.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let _ = windows::Win32::Media::timeBeginPeriod(1);
    }
    let dossier = minelib_assets::data_dir().join("edge");
    let chargeur = match Chargeur::charger(&dossier) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("navigateur Edge : {e}");
            return 1;
        }
    };
    let Some(version) = chargeur.version() else {
        eprintln!("navigateur Edge : Edge WebView2 n'est pas installé");
        return SANS_WEBVIEW2;
    };
    eprintln!("navigateur Edge : WebView2 {version}");
    // La file de messages de WinRT de ce fil (le compositeur en a besoin).
    // SAFETY : appel documenté ; le contrôleur vit jusqu'à la fin.
    let _file = unsafe {
        CreateDispatcherQueueController(DispatcherQueueOptions {
            dwSize: std::mem::size_of::<DispatcherQueueOptions>() as u32,
            threadType: DQTYPE_THREAD_CURRENT,
            apartmentType: DQTAT_COM_STA,
        })
    };
    if let Err(e) = &_file {
        eprintln!("navigateur Edge : file de messages WinRT impossible ({e})");
    }
    let fenetre = match fenetre() {
        Ok(h) => h,
        Err(e) => {
            eprintln!("navigateur Edge : fenêtre impossible ({e})");
            return 1;
        }
    };
    let compo = (|| -> windows::core::Result<(Compositor, DesktopWindowTarget, ContainerVisual)> {
        let compositeur = Compositor::new()?;
        let interop: ICompositorDesktopInterop = compositeur.cast()?;
        // SAFETY : appel documenté sur notre fenêtre.
        let cible = unsafe { interop.CreateDesktopWindowTarget(fenetre, false)? };
        let racine = compositeur.CreateContainerVisual()?;
        racine.SetSize(Vector2 { X: 4096.0, Y: 4096.0 })?;
        cible.SetRoot(&racine)?;
        Ok((compositeur, cible, racine))
    })();
    let (compositeur, cible, racine) = match compo {
        Ok(c) => c,
        Err(e) => {
            eprintln!("navigateur Edge : composition impossible ({e})");
            return 1;
        }
    };
    let gpu = match Gpu::new() {
        Ok(g) => Some(g),
        Err(e) => {
            eprintln!("navigateur Edge : Direct3D 11 impossible ({e}), diffusion JPEG");
            None
        }
    };
    if !GraphicsCaptureSession::IsSupported().unwrap_or(false) {
        eprintln!("navigateur Edge : capture de Windows indisponible, diffusion JPEG");
    }
    // L'environnement d'Edge : sa session (cookies, compte) dans `--cache`.
    let session = opts
        .cache
        .clone()
        .unwrap_or_else(|| minelib_assets::data_dir().join("ecrans-edge"));
    let _ = std::fs::create_dir_all(&session);
    let options = CoreWebView2EnvironmentOptions::default();
    // SAFETY : réglages de l'objet d'options, avant la création.
    unsafe {
        options.set_additional_browser_arguments(arguments());
        options.set_language("fr".to_string());
        options.set_are_browser_extensions_enabled(false);
    }
    let options: ICoreWebView2EnvironmentOptions = options.into();
    let (tx, rx) = mpsc::channel();
    let handler = CreateCoreWebView2EnvironmentCompletedHandler::create(Box::new(move |r, env| {
        let _ = tx.send((r, env));
        Ok(())
    }));
    let dossier_session = HSTRING::from(session.as_os_str());
    // SAFETY : fonction du chargeur, objets COM vivants jusqu'au rappel.
    let hr = unsafe {
        (chargeur.creer)(
            PCWSTR::null(),
            PCWSTR(dossier_session.as_ptr()),
            options.as_raw(),
            handler.as_raw(),
        )
    };
    if hr.is_err() {
        eprintln!("navigateur Edge : environnement impossible ({hr:?})");
        return SESSION_PRISE;
    }
    let env = match wait_with_pump(rx) {
        Ok((Ok(()), Some(env))) => env,
        Ok((r, _)) => {
            eprintln!("navigateur Edge : environnement impossible ({r:?})");
            return SESSION_PRISE;
        }
        Err(e) => {
            eprintln!("navigateur Edge : environnement impossible ({e:?})");
            return SESSION_PRISE;
        }
    };
    eprintln!("minelib-browser : prêt (Edge {version}, session {})", session.display());

    let (filtre, mot) = Filtre::charge(opts.filtre, opts.listes.as_deref());
    eprintln!("filtrage des pages : {mot} ; DNS familial de Cloudflare vérifié par le navigateur");
    let volume = volume::Volume::new(volume::from_pct(opts.volume));
    volume.start();

    let out: Shared = std::sync::Arc::new((std::sync::Mutex::new(proto::Outbox::default()), std::sync::Condvar::new()));
    let ecrivain = {
        let out = out.clone();
        std::thread::spawn(move || proto::send_out(out))
    };
    // SAFETY : identifiant de ce fil.
    let fil = unsafe { GetCurrentThreadId() };
    let (tx_cmd, rx_cmd) = mpsc::channel::<Cmd>();
    std::thread::spawn(move || {
        use std::io::BufRead;
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if let Some(cmd) = proto::parse_cmd(&line) {
                if tx_cmd.send(cmd).is_err() {
                    return;
                }
                // SAFETY : réveille la boucle de ce programme.
                unsafe {
                    let _ = PostThreadMessageW(fil, WM_APP, WPARAM(0), LPARAM(0));
                }
            }
        }
        let _ = tx_cmd.send(Cmd::Quit);
        // SAFETY : idem.
        unsafe {
            let _ = PostThreadMessageW(fil, WM_APP, WPARAM(0), LPARAM(0));
        }
    });
    // Les fils du DNS familial.
    let (tx_hote, rx_hote) = mpsc::channel::<String>();
    let rx_hote = std::sync::Arc::new(std::sync::Mutex::new(rx_hote));
    let (tx_verdict, rx_verdict) = mpsc::channel::<(String, Option<bool>)>();
    for _ in 0..4 {
        let rx = rx_hote.clone();
        let tx = tx_verdict.clone();
        std::thread::spawn(move || loop {
            let hote = match rx.lock().map(|r| r.recv()) {
                Ok(Ok(h)) => h,
                _ => return,
            };
            let v = famille_refuse(&hote);
            if tx.send((hote, v)).is_err() {
                return;
            }
            // SAFETY : réveille la boucle.
            unsafe {
                let _ = PostThreadMessageW(fil, WM_APP, WPARAM(0), LPARAM(0));
            }
        });
    }

    let mut nav = Navigateur {
        opts,
        env,
        fenetre,
        compositeur,
        racine,
        _cible: cible,
        gpu,
        filtre: Rc::new(filtre),
        out: out.clone(),
        etat: Rc::new(RefCell::new(Etat::default())),
        onglets: HashMap::new(),
        attente: HashMap::new(),
        cotes: HashMap::new(),
        volume,
        attente_url: HashMap::new(),
    };
    let mut msg = MSG::default();
    let mut avertis = std::collections::HashSet::new();
    'boucle: loop {
        // SAFETY : attente et pompe de messages de Windows, sur ce fil.
        unsafe {
            let attente = if nav.onglets.is_empty() { 50 } else { 3 };
            let _ = MsgWaitForMultipleObjects(None, false, attente, QS_ALLINPUT);
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                if msg.message == WM_QUIT {
                    break 'boucle;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        while let Ok(cmd) = rx_cmd.try_recv() {
            if cmd == Cmd::Quit {
                break 'boucle;
            }
            nav.commande(cmd);
        }
        nav.suites();
        // Hôtes à vérifier, verdicts arrivés.
        let a_verifier = std::mem::take(&mut nav.etat.borrow_mut().a_verifier);
        for h in a_verifier {
            let _ = tx_hote.send(h);
        }
        while let Ok((hote, v)) = rx_verdict.try_recv() {
            if v.is_none() && avertis.insert(hote.clone()) {
                eprintln!("DNS familial : pas de réponse pour {hote} (laissé passer)");
            }
            let refuse = v.unwrap_or(false);
            let attente = {
                let mut e = nav.etat.borrow_mut();
                if v.is_some() {
                    e.verdicts.insert(hote.clone(), (refuse, Instant::now()));
                }
                e.attente.remove(&hote).unwrap_or_default()
            };
            for a in attente {
                if refuse {
                    refuser(&a.args, &nav.env);
                    if a.principale {
                        eprintln!("onglet {} : page refusée (DNS familial) : {}", a.onglet, a.url);
                        proto::push_event(&nav.out, *b"MLBX", a.onglet, Refus::Famille as u32, Vec::new());
                    }
                }
                // SAFETY : appel WebView2 documenté, sur ce fil.
                unsafe {
                    let _ = a.deferral.Complete();
                }
            }
        }
        nav.images();
        if nav.etat.borrow().tombe {
            eprintln!("navigateur Edge : le processus d'Edge est tombé, arrêt (le jeu relance)");
            std::process::exit(4);
        }
    }
    let ids: Vec<u32> = nav.onglets.keys().copied().collect();
    for id in ids {
        nav.fermer(id);
    }
    {
        let (lock, cv) = &*out;
        if let Ok(mut o) = lock.lock() {
            o.closed = true;
        }
        cv.notify_one();
    }
    let _ = ecrivain.join();
    0
}
