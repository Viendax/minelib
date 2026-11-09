//! Couche plateforme : fenêtre, boucle d'événements, entrées, temps.
//! Windows, Linux et macOS passent par `winit`, le reste du moteur
//! ne dépend jamais directement d'un système d'exploitation.
//!
//! Les touches sont des **touches physiques** (`KeyCode`) : `KeyW` est la touche
//! en haut à gauche du pavé de déplacement, c'est-à-dire Z sur un clavier AZERTY.

pub mod sysmon;

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

use winit::application::ApplicationHandler;
use winit::event::{DeviceEvent, DeviceId, ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::PhysicalKey;
use winit::window::{CursorGrabMode, Window, WindowId};

pub use winit::keyboard::KeyCode;
pub use winit::window::Window as PlatformWindow;

/// État des entrées pour l'image en cours.
#[derive(Default)]
pub struct Input {
    held: HashSet<KeyCode>,
    pressed: HashSet<KeyCode>,
    mouse_held: [bool; 3],
    mouse_pressed: [bool; 3],
    /// Déplacement souris brut accumulé depuis l'image précédente (seulement si capturée).
    pub mouse_delta: (f64, f64),
    /// Molette : crans accumulés depuis l'image précédente (positif = vers le haut).
    pub scroll: f32,
    /// Le curseur est-il capturé par le jeu ?
    pub grabbed: bool,
    /// Position du curseur dans la fenêtre (pixels physiques), s'il est dedans.
    pub cursor: Option<(f32, f32)>,
    /// Texte tapé pendant l'image (saisie dans les champs de texte).
    pub text: String,
    /// Caractère réellement produit par chaque touche physique déjà pressée (disposition du
    /// clavier : « Z » pour la touche W d'un clavier AZERTY).
    pub key_labels: std::collections::HashMap<KeyCode, String>,
}

impl Input {
    pub fn held(&self, k: KeyCode) -> bool {
        self.held.contains(&k)
    }
    /// Vrai uniquement sur l'image où la touche a été enfoncée.
    pub fn pressed(&self, k: KeyCode) -> bool {
        self.pressed.contains(&k)
    }
    /// Touches enfoncées pendant cette image.
    pub fn pressed_keys(&self) -> Vec<KeyCode> {
        self.pressed.iter().copied().collect()
    }
    pub fn mouse_held(&self, b: Mouse) -> bool {
        self.mouse_held[b as usize]
    }
    /// Bouton de souris enfoncé pendant cette image, s'il y en a un.
    pub fn mouse_pressed_any(&self) -> Option<Mouse> {
        [Mouse::Left, Mouse::Right, Mouse::Middle]
            .into_iter()
            .find(|b| self.mouse_pressed[*b as usize])
    }
    pub fn mouse_pressed(&self, b: Mouse) -> bool {
        self.mouse_pressed[b as usize]
    }

    fn end_frame(&mut self) {
        self.pressed.clear();
        self.mouse_pressed = [false; 3];
        self.mouse_delta = (0.0, 0.0);
        self.scroll = 0.0;
        self.text.clear();
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Mouse {
    Left = 0,
    Right = 1,
    Middle = 2,
}

/// Ce que le jeu doit implémenter pour être piloté par la plateforme.
pub trait GameLoop {
    fn on_ready(&mut self, window: Arc<Window>);
    fn on_resize(&mut self, width: u32, height: u32);
    /// Appelée à chaque image ; `dt` en secondes (borné à 0,1 s).
    /// Renvoie `false` pour quitter.
    fn on_frame(&mut self, window: &Window, input: &Input, dt: f32) -> bool;
    fn on_exit(&mut self);
    /// Une interface (inventaire…) a-t-elle besoin du curseur libre ?
    /// Tant que oui, la souris n'est pas capturée et les clics vont au jeu.
    fn ui_open(&self) -> bool {
        false
    }
}

pub fn run<G: GameLoop>(title: &str, game: G) -> anyhow::Result<()> {
    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = App {
        title: title.to_owned(),
        game,
        window: None,
        input: Input::default(),
        last: Instant::now(),
        ui_was_open: false,
    };
    event_loop.run_app(&mut app)?;
    Ok(())
}

struct App<G: GameLoop> {
    title: String,
    game: G,
    window: Option<Arc<Window>>,
    input: Input,
    last: Instant,
    ui_was_open: bool,
}

impl<G: GameLoop> App<G> {
    fn set_grab(&mut self, grab: bool) {
        let Some(w) = &self.window else { return };
        if grab {
            // `Locked` n'existe pas partout (Windows) : on se rabat sur `Confined`.
            let ok = w
                .set_cursor_grab(CursorGrabMode::Locked)
                .or_else(|_| w.set_cursor_grab(CursorGrabMode::Confined))
                .is_ok();
            if !ok {
                log::warn!("impossible de capturer le curseur");
            }
            w.set_cursor_visible(false);
        } else {
            let _ = w.set_cursor_grab(CursorGrabMode::None);
            w.set_cursor_visible(true);
        }
        self.input.grabbed = grab;
    }
}

impl<G: GameLoop> ApplicationHandler for App<G> {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title(self.title.clone())
            .with_inner_size(winit::dpi::LogicalSize::new(1280.0, 720.0));
        let window = Arc::new(
            event_loop
                .create_window(attrs)
                .expect("création de la fenêtre"),
        );
        self.game.on_ready(window.clone());
        window.request_redraw();
        self.window = Some(window);
        self.last = Instant::now();
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                self.game.on_exit();
                event_loop.exit();
            }
            WindowEvent::Resized(size) => self.game.on_resize(size.width, size.height),
            WindowEvent::Focused(false) => {
                self.input.held.clear();
                self.input.mouse_held = [false; 3];
                self.set_grab(false);
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let PhysicalKey::Code(code) = event.physical_key else {
                    return;
                };
                match event.state {
                    ElementState::Pressed => {
                        if let Some(t) = &event.text {
                            self.input
                                .text
                                .extend(t.chars().filter(|c| !c.is_control()));
                        }
                        if !event.repeat {
                            self.input.pressed.insert(code);
                        }
                        if let winit::keyboard::Key::Character(c) = &event.logical_key {
                            let label = c.to_uppercase();
                            if !label.trim().is_empty() {
                                self.input.key_labels.insert(code, label);
                            }
                        }
                        self.input.held.insert(code);
                        if code == KeyCode::Escape {
                            self.set_grab(false);
                        }
                    }
                    ElementState::Released => {
                        self.input.held.remove(&code);
                    }
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let idx = match button {
                    MouseButton::Left => 0,
                    MouseButton::Right => 1,
                    MouseButton::Middle => 2,
                    _ => return,
                };
                let down = state == ElementState::Pressed;
                if down && !self.input.grabbed && !self.game.ui_open() {
                    // Premier clic : capture la souris sans déclencher d'action.
                    self.set_grab(true);
                    return;
                }
                if down {
                    self.input.mouse_pressed[idx] = true;
                }
                self.input.mouse_held[idx] = down;
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.input.cursor = Some((position.x as f32, position.y as f32));
            }
            WindowEvent::CursorLeft { .. } => self.input.cursor = None,
            WindowEvent::MouseWheel { delta, .. } => {
                self.input.scroll += match delta {
                    winit::event::MouseScrollDelta::LineDelta(_, y) => y,
                    winit::event::MouseScrollDelta::PixelDelta(p) => (p.y / 40.0) as f32,
                };
            }
            // Les images ne passent plus par le message de dessin de Windows (WM_PAINT, traité
            // en dernier et regroupé) : voir `about_to_wait`.
            WindowEvent::RedrawRequested => {}
            _ => {}
        }
    }

    /// Tous les événements en attente sont traités : on calcule et affiche l'image tout de
    /// suite, en boucle, sans attendre de message de Windows (plus de FPS, moins de latence).
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        let dt = (now - self.last).as_secs_f32().min(0.1);
        self.last = now;
        let Some(w) = self.window.clone() else { return };
        let keep_running = self.game.on_frame(&w, &self.input, dt);
        self.input.end_frame();
        // Ouverture / fermeture d'une interface : libère ou recapture la souris.
        let open = self.game.ui_open();
        if open && self.input.grabbed {
            self.set_grab(false);
        } else if !open && self.ui_was_open {
            self.set_grab(true);
        }
        self.ui_was_open = open;
        if !keep_running {
            self.game.on_exit();
            event_loop.exit();
        }
    }

    fn device_event(&mut self, _el: &ActiveEventLoop, _id: DeviceId, event: DeviceEvent) {
        if let DeviceEvent::MouseMotion { delta } = event {
            if self.input.grabbed {
                self.input.mouse_delta.0 += delta.0;
                self.input.mouse_delta.1 += delta.1;
            }
        }
    }
}

/// Caractère de la touche physique `k` selon la disposition du clavier (Windows), ex. « Z »
/// pour `KeyW` en AZERTY. `None` si inconnu (ailleurs : appris quand la touche est pressée).
pub fn layout_label(k: KeyCode) -> Option<String> {
    #[cfg(windows)]
    {
        use winit::platform::scancode::PhysicalKeyExtScancode;
        #[link(name = "user32")]
        extern "system" {
            fn MapVirtualKeyW(code: u32, map_type: u32) -> u32;
        }
        let scan = winit::keyboard::PhysicalKey::Code(k).to_scancode()?;
        // MAPVK_VSC_TO_VK_EX = 3, MAPVK_VK_TO_CHAR = 2
        let vk = unsafe { MapVirtualKeyW(scan, 3) };
        if vk == 0 {
            return None;
        }
        let ch = unsafe { MapVirtualKeyW(vk, 2) } & 0x7fff_ffff; // bit haut : touche morte
        let c = char::from_u32(ch).filter(|c| !c.is_control() && !c.is_whitespace())?;
        Some(c.to_uppercase().to_string())
    }
    #[cfg(not(windows))]
    {
        let _ = k;
        None
    }
}

/// Plantages hors Rust (pilote graphique, accès mémoire invalide…) : sous Windows, le code
/// d'exception, l'adresse et la pile d'appels sont ajoutés au fichier `path` avant la
/// fermeture du jeu.
pub fn install_native_crash_handler(path: std::path::PathBuf) {
    #[cfg(windows)]
    native_crash::install(path);
    #[cfg(not(windows))]
    let _ = path;
}

#[cfg(windows)]
mod native_crash {
    use std::io::Write;
    use std::path::PathBuf;
    use std::sync::OnceLock;

    use windows::Win32::System::Diagnostics::Debug::{
        SetUnhandledExceptionFilter, EXCEPTION_POINTERS,
    };
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;

    static PATH: OnceLock<PathBuf> = OnceLock::new();

    pub fn install(path: PathBuf) {
        let _ = PATH.set(path);
        unsafe {
            SetUnhandledExceptionFilter(Some(filter));
        }
    }

    unsafe extern "system" fn filter(info: *const EXCEPTION_POINTERS) -> i32 {
        let mut msg = String::from("\n=== PLANTAGE (exception Windows) ===\n");
        if let Some(rec) = info.as_ref().and_then(|i| i.ExceptionRecord.as_ref()) {
            let code = rec.ExceptionCode.0 as u32;
            let name = match code {
                0xC0000005 => "accès mémoire invalide",
                0xC00000FD => "dépassement de pile",
                0xC000001D => "instruction illégale",
                0xC0000094 => "division entière par zéro",
                0xC0000409 => "arrêt forcé (fail fast / pile corrompue)",
                0xE06D7363 => "exception C++ (pilote ou bibliothèque)",
                _ => "autre",
            };
            let addr = rec.ExceptionAddress as usize;
            let base = GetModuleHandleW(None).map(|h| h.0 as usize).unwrap_or(0);
            msg += &format!(
                "code 0x{code:08X} ({name})\nadresse 0x{addr:X} (minelib.exe + 0x{:X})\n",
                addr.wrapping_sub(base)
            );
            if code == 0xC0000005 && rec.NumberParameters >= 2 {
                let op = match rec.ExceptionInformation[0] {
                    0 => "lecture",
                    1 => "écriture",
                    8 => "exécution",
                    _ => "?",
                };
                msg += &format!("{op} à l'adresse 0x{:X}\n", rec.ExceptionInformation[1]);
            }
        }
        msg += &format!("thread : {:?}\n", std::thread::current().name());
        msg += &format!("{}\n", std::backtrace::Backtrace::force_capture());
        if let Some(p) = PATH.get() {
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(p)
            {
                let _ = f.write_all(msg.as_bytes());
            }
        }
        let _ = std::io::stderr().write_all(msg.as_bytes());
        0 // EXCEPTION_CONTINUE_SEARCH : fermeture normale après le rapport
    }
}

/// Thread de fond (génération, maillages) : priorité sous celle du rendu, pour que le jeu
/// reste fluide même quand tous les cœurs travaillent.
pub fn lower_thread_priority() {
    #[cfg(windows)]
    unsafe {
        use windows::Win32::System::Threading::{
            GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL,
        };
        let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL);
    }
}

/// Thread de fond en répartition uniforme : priorité la plus basse, il ne prend que le temps
/// que le jeu et le rendu laissent libre sur chaque cœur.
pub fn lowest_thread_priority() {
    #[cfg(windows)]
    unsafe {
        use windows::Win32::System::Threading::{
            GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_LOWEST,
        };
        let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_LOWEST);
    }
}

/// Thread du jeu et du rendu : passe avant les tâches de fond sur n'importe quel cœur.
pub fn raise_thread_priority() {
    #[cfg(windows)]
    unsafe {
        use windows::Win32::System::Threading::{
            GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_ABOVE_NORMAL,
        };
        let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_ABOVE_NORMAL);
    }
}

/// Carte graphique vue par Windows (DXGI).
#[derive(Clone, Debug)]
pub struct GpuAdapter {
    pub name: String,
    /// Mémoire vidéo dédiée (octets) : 0 ou presque pour une partie graphique intégrée.
    pub dedicated: u64,
    /// Mémoire système partagée utilisable (octets).
    pub shared: u64,
    /// Un écran est branché sur cette carte (sinon ses images sont recopiées vers une autre).
    pub has_display: bool,
}

/// Cartes graphiques de la machine et écrans branchés dessus (Windows ; vide ailleurs).
pub fn gpu_adapters() -> Vec<GpuAdapter> {
    #[cfg(windows)]
    unsafe {
        use windows::Win32::Graphics::Dxgi::{
            CreateDXGIFactory1, IDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE,
        };
        let mut out = Vec::new();
        let Ok(factory) = CreateDXGIFactory1::<IDXGIFactory1>() else {
            return out;
        };
        for i in 0.. {
            let Ok(a) = factory.EnumAdapters1(i) else {
                break;
            };
            let Ok(desc) = a.GetDesc1() else { continue };
            if desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0 {
                continue;
            }
            let len = desc
                .Description
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(desc.Description.len());
            out.push(GpuAdapter {
                name: String::from_utf16_lossy(&desc.Description[..len])
                    .trim()
                    .to_string(),
                dedicated: desc.DedicatedVideoMemory as u64,
                shared: desc.SharedSystemMemory as u64,
                has_display: a.EnumOutputs(0).is_ok(),
            });
        }
        out
    }
    #[cfg(not(windows))]
    Vec::new()
}

/// Écrans branchés directement sur la carte nommée `name` (noms Windows, « \\.\DISPLAY2 »).
/// Sur un portable Optimus sans MUX, l'écran intégré est sur la puce Intel ; une prise
/// (souvent la HDMI) peut être reliée à la carte dédiée : un écran branché là n'a pas de
/// recopie entre les cartes.
pub fn gpu_output_names(name: &str) -> Vec<String> {
    #[cfg(windows)]
    unsafe {
        use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1};
        let mut out = Vec::new();
        let Ok(factory) = CreateDXGIFactory1::<IDXGIFactory1>() else {
            return out;
        };
        for i in 0.. {
            let Ok(a) = factory.EnumAdapters1(i) else {
                break;
            };
            let Ok(desc) = a.GetDesc1() else { continue };
            let len = desc
                .Description
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(desc.Description.len());
            let n = String::from_utf16_lossy(&desc.Description[..len])
                .trim()
                .to_string();
            if n.is_empty() || !(name.contains(&n) || n.contains(name)) {
                continue;
            }
            for j in 0.. {
                let Ok(o) = a.EnumOutputs(j) else { break };
                if let Ok(d) = o.GetDesc() {
                    let len = d
                        .DeviceName
                        .iter()
                        .position(|&c| c == 0)
                        .unwrap_or(d.DeviceName.len());
                    out.push(String::from_utf16_lossy(&d.DeviceName[..len]));
                }
            }
        }
        out
    }
    #[cfg(not(windows))]
    {
        let _ = name;
        Vec::new()
    }
}

/// Mémoire vidéo réellement utilisée par le jeu sur une carte, et budget que Windows lui
/// accorde (DXGI). Au-delà du budget, Windows déplace des données vers la mémoire vive :
/// la carte se bloque alors pendant des dizaines de millisecondes.
pub struct VramMonitor {
    #[cfg(windows)]
    adapter: windows::Win32::Graphics::Dxgi::IDXGIAdapter3,
}

impl VramMonitor {
    /// Carte dont le nom ressemble à `name` (celle qui dessine).
    pub fn new(name: &str) -> Option<Self> {
        #[cfg(windows)]
        unsafe {
            use windows::core::Interface;
            use windows::Win32::Graphics::Dxgi::{
                CreateDXGIFactory1, IDXGIAdapter3, IDXGIFactory1,
            };
            let factory = CreateDXGIFactory1::<IDXGIFactory1>().ok()?;
            for i in 0.. {
                let a = factory.EnumAdapters1(i).ok()?;
                let Ok(desc) = a.GetDesc1() else { continue };
                let len = desc
                    .Description
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(desc.Description.len());
                let n = String::from_utf16_lossy(&desc.Description[..len])
                    .trim()
                    .to_string();
                if !n.is_empty() && (name.contains(&n) || n.contains(name)) {
                    return a
                        .cast::<IDXGIAdapter3>()
                        .ok()
                        .map(|adapter| Self { adapter });
                }
            }
            None
        }
        #[cfg(not(windows))]
        {
            let _ = name;
            None
        }
    }

    /// (utilisée, budget) en octets.
    pub fn query(&self) -> Option<(u64, u64)> {
        #[cfg(windows)]
        unsafe {
            use windows::Win32::Graphics::Dxgi::{
                DXGI_MEMORY_SEGMENT_GROUP_LOCAL, DXGI_QUERY_VIDEO_MEMORY_INFO,
            };
            let mut info = DXGI_QUERY_VIDEO_MEMORY_INFO::default();
            self.adapter
                .QueryVideoMemoryInfo(0, DXGI_MEMORY_SEGMENT_GROUP_LOCAL, &mut info)
                .ok()?;
            Some((info.CurrentUsage, info.Budget))
        }
        #[cfg(not(windows))]
        None
    }
}

/// Mémoire vive (octets) : (totale, disponible).
pub fn memory_info() -> (u64, u64) {
    let mut sys = sysinfo::System::new_with_specifics(
        sysinfo::RefreshKind::nothing().with_memory(sysinfo::MemoryRefreshKind::everything()),
    );
    sys.refresh_memory();
    (sys.total_memory(), sys.available_memory())
}
