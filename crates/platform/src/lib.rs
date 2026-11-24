//! Couche plateforme : fenêtre, boucle d'événements, entrées, temps.
//! Windows, Linux et macOS passent par `winit`, le reste du moteur
//! ne dépend jamais directement d'un système d'exploitation.
//!
//! Les touches sont des **touches physiques** (`KeyCode`) : `KeyW` est la touche
//! en haut à gauche du pavé de déplacement, c'est-à-dire Z sur un clavier AZERTY.

pub mod passkey;
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

/// (v250) Ctrl est tenu sans Alt : la touche appuyée est un raccourci (Ctrl+V…), pas du
/// texte. Alt Gr arrive sous Windows comme Ctrl + Alt droit : c'est du texte.
fn raccourci(held: &HashSet<KeyCode>) -> bool {
    (held.contains(&KeyCode::ControlLeft) || held.contains(&KeyCode::ControlRight))
        && !held.contains(&KeyCode::AltRight)
        && !held.contains(&KeyCode::AltLeft)
}

/// État des entrées pour l'image en cours.
#[derive(Default)]
pub struct Input {
    held: HashSet<KeyCode>,
    /// Moment de l'appui de chaque touche tenue (journal des touches rattrapées).
    pressed_at: std::collections::HashMap<KeyCode, Instant>,
    pressed: HashSet<KeyCode>,
    /// Touches physiquement enfoncées d'après le clavier lui-même (saisie brute), qui
    /// arrive même quand un logiciel (macro, superposition, pilote) intercepte ou avale
    /// les messages de Windows.
    raw_down: HashSet<KeyCode>,
    /// Touches relâchées d'après le clavier, à comparer à ce qu'en dit Windows.
    raw_released: Vec<(KeyCode, Instant)>,
    /// Répétitions automatiques reçues pour chaque touche tenue (nombre, dernière) : au
    /// relâchement d'une touche tenue longtemps, le journal dit ce que Windows et le
    /// clavier en disaient (pour trouver d'où vient une touche qui « reste enfoncée »).
    repeats: std::collections::HashMap<KeyCode, (u32, Instant)>,
    /// Dernier appui (neuf) d'une touche, quelle qu'elle soit : Windows ne répète que la
    /// dernière touche enfoncée.
    last_press: Option<Instant>,
    /// Touches relâchées parce que leurs répétitions se sont arrêtées (voir
    /// `release_silent_keys`) : une répétition qui revient les remet.
    soft: HashSet<KeyCode>,
    /// Retard du dernier message de clavier traité (ms) : temps entre le moment où
    /// Windows l'a daté et celui où le jeu l'a reçu ; et retards déjà signalés.
    msg_lag: u32,
    lag_warned: u32,
    /// Appareils d'où viennent les touches (saisie brute) : nom → numéro donné au journal.
    /// Sans nom : aucune touche physique derrière, c'est un logiciel qui tape (macro,
    /// script) ; `injected` le retient pour prévenir le joueur.
    key_devices: std::collections::HashMap<Option<String>, usize>,
    last_device: usize,
    injected: bool,
    /// La saisie brute du clavier arrive-t-elle (notée une fois) ?
    raw_seen: bool,
    /// La fenêtre a le clavier (les appuis ne sont pris que dans ce cas).
    focused: bool,
    /// Touches enfoncées puis relâchées dans la même image : gardées tenues jusqu'à la fin
    /// de l'image, pour que le jeu les voie au moins une fois (un appui très bref, ou un
    /// appui pendant une image longue, n'est jamais perdu).
    release_after_frame: Vec<KeyCode>,
    /// Appuis vus par le clavier lui-même (saisie brute) ou par l'état du système avant le
    /// message de Windows : quand, et par qui (pour mesurer le retard du message).
    early_press: std::collections::HashMap<KeyCode, (Instant, &'static str)>,
    /// Bilan des appuis (journal toutes les 5 minutes).
    press_stats: PressStats,
    /// Touches déjà signalées au journal (une fois chacune).
    #[cfg_attr(not(windows), allow(dead_code))]
    raw_warned: HashSet<KeyCode>,
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

/// Bilan des appuis de touches : qui les a vus en premier, et le retard du message de
/// Windows sur le clavier lui-même.
#[derive(Default)]
struct PressStats {
    total: u32,
    /// Vus d'abord par la saisie brute, ou par l'état du système.
    by_raw: u32,
    by_state: u32,
    /// Message de Windows arrivé après coup : nombre, retard cumulé et pire (ms).
    late: u32,
    late_sum: f32,
    late_max: f32,
    /// Message d'appui de Windows qui n'est jamais arrivé (touche relâchée avant).
    missing: u32,
    /// Appuis que Windows donnait pour des répétitions (sa touche lui paraissait encore
    /// enfoncée) : pris comme de vrais appuis.
    as_repeat: u32,
    /// Touches enfoncées et relâchées dans la même image (gardées une image).
    taps: u32,
    since: Option<Instant>,
}

impl Input {
    /// Essais : appuie sur une touche pour cette image (et la tient).
    pub fn press(&mut self, k: KeyCode) {
        self.held.insert(k);
        self.pressed.insert(k);
    }

    /// Essais : début d'une nouvelle image (touches toujours tenues, plus « pressées »).
    pub fn next_frame(&mut self) {
        self.pressed.clear();
        self.text.clear();
        self.mouse_pressed = [false; 3];
        self.scroll = 0.0;
    }

    /// (v236) Essais : enfonce un bouton de la souris pour cette image (et le tient).
    pub fn press_mouse(&mut self, b: Mouse) {
        self.mouse_pressed[b as usize] = true;
        self.mouse_held[b as usize] = true;
    }

    /// (v236) Essais : relâche un bouton de la souris.
    pub fn release_mouse(&mut self, b: Mouse) {
        self.mouse_held[b as usize] = false;
    }

    pub fn held(&self, k: KeyCode) -> bool {
        self.held.contains(&k)
    }

    /// Une touche vient d'être enfoncée d'après `source` : tenue et « pressée » pour cette
    /// image, quel que soit le chemin qui l'a vue en premier (le clavier lui-même, l'état du
    /// système, ou le message de Windows).
    fn press_now(&mut self, code: KeyCode, source: &'static str) {
        let now = Instant::now();
        self.last_press = Some(now);
        self.soft.remove(&code);
        self.repeats.remove(&code);
        self.pressed.insert(code);
        self.held.insert(code);
        self.pressed_at.insert(code, now);
        self.release_after_frame.retain(|k| *k != code);
        self.press_stats.total += 1;
        match source {
            "message de Windows" => {}
            "clavier (saisie brute)" => {
                self.press_stats.by_raw += 1;
                self.early_press.insert(code, (now, source));
            }
            _ => {
                self.press_stats.by_state += 1;
                self.early_press.insert(code, (now, source));
            }
        }
    }

    /// Le message de Windows d'un appui déjà vu ailleurs arrive : son retard est noté.
    fn confirm_press(&mut self, code: KeyCode) {
        if let Some((at, source)) = self.early_press.remove(&code) {
            let ms = at.elapsed().as_secs_f32() * 1000.0;
            let st = &mut self.press_stats;
            st.late += 1;
            st.late_sum += ms;
            if ms > st.late_max {
                st.late_max = ms;
            }
            if ms >= 30.0 {
                log::info!(
                    "touche {code:?} : appui vu par {source} {ms:.0} ms avant le message de \
                     Windows (pris dès le premier)"
                );
            }
        }
    }

    /// Une touche tenue vient d'être relâchée : enlevée tout de suite, sauf si elle a été
    /// enfoncée dans cette même image (gardée jusqu'à la fin de l'image : le jeu la voit
    /// au moins une fois). `false` : elle n'était pas tenue.
    fn release_now(&mut self, code: KeyCode) -> bool {
        if !self.held.contains(&code) {
            return false;
        }
        if let Some((_, source)) = self.early_press.remove(&code) {
            self.press_stats.missing += 1;
            log::info!(
                "touche {code:?} : appui vu par {source}, relâchée sans que Windows ait envoyé \
                 son appui (pris quand même)"
            );
        }
        if self.pressed.contains(&code) {
            if !self.release_after_frame.contains(&code) {
                self.release_after_frame.push(code);
                self.press_stats.taps += 1;
            }
        } else {
            self.held.remove(&code);
        }
        true
    }

    /// Bilan des appuis, au journal toutes les 5 minutes (et à la fermeture : `force`).
    fn report_presses(&mut self, force: bool) {
        let st = &mut self.press_stats;
        let since = *st.since.get_or_insert_with(Instant::now);
        if st.total == 0 || (!force && since.elapsed().as_secs() < 300) {
            return;
        }
        if let Some(f) = filter_keys() {
            log::warn!("clavier : {f}");
        }
        log::info!(
            "clavier : {} appuis en {:.0} s ; vus d'abord par le clavier lui-même {} fois, par \
             l'état du système {} fois ; message de Windows en retard {} fois (moyenne {:.1} \
             ms, pire {:.0} ms), jamais arrivé {} fois ; donnés pour des répétitions par \
             Windows {} fois ; appuis plus brefs qu'une image {} (gardés une image)",
            st.total,
            since.elapsed().as_secs_f32(),
            st.by_raw,
            st.by_state,
            st.late,
            if st.late > 0 { st.late_sum / st.late as f32 } else { 0.0 },
            st.late_max,
            st.missing,
            st.as_repeat,
            st.taps
        );
        *st = PressStats {
            since: Some(Instant::now()),
            ..PressStats::default()
        };
    }

    /// Touche tenue longtemps dont Windows a cessé d'envoyer les répétitions : elle est
    /// relâchée, même si ni le message de relâchement ni l'état du système ne le disent
    /// (un logiciel qui avale le relâchement laisse Windows croire la touche enfoncée, et
    /// le joueur continuait d'avancer). Windows répète la dernière touche enfoncée au
    /// moins 2,5 fois par seconde tant qu'elle l'est : 0,7 s sans répétition, sans autre
    /// touche enfoncée depuis (qui aurait pris les répétitions pour elle), et c'est fini.
    /// Une répétition qui revient malgré tout la remet aussitôt.
    fn release_silent_keys(&mut self) {
        // Quand le clavier lui-même nous parle (saisie brute), c'est lui qui fait foi :
        // cette règle ne sert que s'il ne dit rien.
        if self.raw_seen {
            return;
        }
        let last_press = self.last_press;
        let silent: Vec<KeyCode> = self
            .held
            .iter()
            .copied()
            .filter(|code| {
                self.repeats.get(code).is_some_and(|&(n, last)| {
                    n >= 3
                        && last.elapsed().as_secs_f32() > 0.7
                        && last_press.is_none_or(|p| p <= last)
                })
            })
            .collect();
        for code in silent {
            self.held.remove(&code);
            self.note_release(code, "répétitions de Windows arrêtées");
            self.soft.insert(code);
        }
    }

    /// Une touche tenue vient d'être relâchée (d'après `source`) : tenue plus de 2 s,
    /// le journal garde de quoi comprendre une touche restée enfoncée trop longtemps
    /// (Windows envoyait-il encore des répétitions ? le clavier la disait-il enfoncée ?).
    fn note_release(&mut self, code: KeyCode, source: &str) {
        let held = self
            .pressed_at
            .get(&code)
            .map_or(0.0, |t| t.elapsed().as_secs_f32());
        let reps = self.repeats.remove(&code);
        if held < 2.0 && source != "répétitions de Windows arrêtées" {
            return;
        }
        let (n, last) = reps.map_or((0, f32::NAN), |r| (r.0, r.1.elapsed().as_secs_f32()));
        log::info!(
            "touche {code:?} relâchée à {:.3} s après {held:.2} s (vu par : {source}, message \
             reçu {} ms après sa date, clavier n° {}) ; {n} répétitions de Windows, la dernière \
             il y a {last:.2} s ; clavier brut : {}",
            uptime(),
            self.msg_lag,
            self.last_device,
            if !self.raw_seen {
                "pas de saisie brute"
            } else if self.raw_down.contains(&code) {
                "encore enfoncée"
            } else {
                "relâchée"
            }
        );
    }
    /// Vrai uniquement sur l'image où la touche a été enfoncée.
    pub fn pressed(&self, k: KeyCode) -> bool {
        self.pressed.contains(&k)
    }
    /// Touches tenues en ce moment (diagnostic).
    /// Les mêmes entrées sans aucune touche ni clic (la souris tourne toujours la vue) :
    /// pour le jeu pendant un test des touches, où le joueur tient des touches qui ne
    /// doivent rien faire.
    pub fn silenced(&self) -> Input {
        Input {
            mouse_delta: self.mouse_delta,
            grabbed: self.grabbed,
            cursor: self.cursor,
            focused: self.focused,
            raw_seen: self.raw_seen,
            key_labels: self.key_labels.clone(),
            ..Input::default()
        }
    }

    pub fn held_keys(&self) -> Vec<KeyCode> {
        self.held.iter().copied().collect()
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

    /// Un message de clavier vient d'arriver : note son retard (date de Windows contre
    /// maintenant). Un retard net veut dire que les touches arrivent au jeu après coup
    /// (file de messages encombrée, ou logiciel qui retient le clavier) : relâcher une
    /// touche n'est alors vu que plus tard. Noté au journal (quelques fois).
    fn note_lag(&mut self, what: &str) {
        self.msg_lag = message_lag_ms();
        if self.msg_lag >= 80 && self.lag_warned < 20 {
            self.lag_warned += 1;
            log::warn!(
                "clavier : message « {what} » reçu {} ms après sa date par Windows (les touches \
                 arrivent au jeu en retard)",
                self.msg_lag
            );
        }
    }

    /// Des touches arrivent-elles d'un logiciel et non d'un clavier (frappes injectées :
    /// macro, script, logiciel qui réécrit les touches) ? C'est alors lui qui décide quand
    /// une touche est relâchée, pas le clavier.
    pub fn injected_keys(&self) -> bool {
        self.injected
    }

    /// Note de quel appareil vient la touche brute en cours (une ligne au journal par
    /// appareil).
    fn note_device(&mut self, name: Option<String>) {
        let next = self.key_devices.len() + 1;
        let fresh = !self.key_devices.contains_key(&name);
        let n = *self.key_devices.entry(name.clone()).or_insert(next);
        self.last_device = n;
        if fresh {
            match &name {
                Some(d) => log::info!("clavier n° {n} : {d}"),
                None => {
                    self.injected = true;
                    log::warn!(
                        "clavier n° {n} : touches sans appareil — elles sont tapées par un \
                         logiciel (macro, script, logiciel qui réécrit le clavier), qui décide \
                         aussi du moment où elles sont relâchées"
                    );
                }
            }
        }
    }

    fn end_frame(&mut self) {
        for code in std::mem::take(&mut self.release_after_frame) {
            self.held.remove(&code);
        }
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
    /// Le jeu est-il prêt à être montré ? La fenêtre reste cachée jusque-là (pas de fenêtre
    /// noire ou à moitié dessinée pendant le chargement).
    fn loaded(&self) -> bool {
        true
    }
    /// La fenêtre vient d'apparaître.
    fn on_shown(&mut self) {}
}

/// Au plus ce temps avant de montrer la fenêtre, prête ou non.
const SHOW_AFTER: std::time::Duration = std::time::Duration::from_secs(25);

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
        shown: false,
        created: Instant::now(),
    };
    // (Réglage du clavier laissé par un plantage : remis d'abord.)
    key_repeat::restore_leftover();
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
    /// Fenêtre montrée (elle est créée cachée), et quand elle a été créée.
    shown: bool,
    created: Instant,
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
        // (v251) Essais : MINELIB_TAILLE=<largeur>x<hauteur> (sinon 1280 × 720).
        let (w, h) = std::env::var("MINELIB_TAILLE")
            .ok()
            .and_then(|v| {
                let (w, h) = v.trim().split_once('x')?;
                Some((w.parse::<f64>().ok()?, h.parse::<f64>().ok()?))
            })
            .filter(|&(w, h)| w >= 320.0 && h >= 200.0)
            .unwrap_or((1280.0, 720.0));
        let attrs = Window::default_attributes()
            .with_title(self.title.clone())
            .with_inner_size(winit::dpi::LogicalSize::new(w, h))
            .with_visible(false);
        let window = Arc::new(
            event_loop
                .create_window(attrs)
                .expect("création de la fenêtre"),
        );
        if let Some(f) = filter_keys() {
            log::warn!("clavier : {f}");
        }
        self.game.on_ready(window.clone());
        window.request_redraw();
        self.window = Some(window);
        self.last = Instant::now();
        self.created = Instant::now();
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                self.input.report_presses(true);
                self.game.on_exit();
                event_loop.exit();
            }
            WindowEvent::Resized(size) => self.game.on_resize(size.width, size.height),
            WindowEvent::Focused(true) => {
                // (v215) Au journal : à rapprocher de ce que le joueur entend ou voit quand
                // il quitte le jeu et y revient.
                log::info!("fenêtre : au premier plan");
                self.input.focused = true;
            }
            WindowEvent::Focused(false) => {
                log::info!("fenêtre : plus au premier plan");
                self.input.focused = false;
                self.input.held.clear();
                self.input.raw_down.clear();
                self.input.raw_released.clear();
                self.input.release_after_frame.clear();
                self.input.early_press.clear();
                self.input.mouse_held = [false; 3];
                self.set_grab(false);
            }
            WindowEvent::KeyboardInput { event, .. } => {
                self.input.note_lag("touche");
                // (Même une touche inconnue prend les répétitions pour elle.)
                if event.state == ElementState::Pressed && !event.repeat {
                    self.input.last_press = Some(Instant::now());
                }
                let PhysicalKey::Code(code) = event.physical_key else {
                    log::info!(
                        "touche sans code reconnu : {:?} ({:?})",
                        event.physical_key,
                        event.state
                    );
                    return;
                };
                match event.state {
                    ElementState::Pressed => {
                        // (v250) Ctrl + touche est un raccourci, pas du texte : winit donne
                        // quand même la lettre (« v » pour Ctrl+V), qui s'écrivait avant ce
                        // qu'on collait. Alt Gr (Ctrl + Alt pour Windows) tape bien du texte
                        // (« @ », « € »…).
                        if let (Some(t), false) = (&event.text, raccourci(&self.input.held)) {
                            self.input
                                .text
                                .extend(t.chars().filter(|c| !c.is_control()));
                        }
                        // Un appui déjà annoncé par le clavier lui-même ou par l'état du
                        // système n'est pas annoncé une deuxième fois (une touche qui ouvre
                        // et ferme un menu ne doit pas basculer deux fois).
                        if !event.repeat && !self.input.early_press.contains_key(&code) {
                            self.input.pressed.insert(code);
                        }
                        if let winit::keyboard::Key::Character(c) = &event.logical_key {
                            let label = c.to_uppercase();
                            if !label.trim().is_empty() {
                                self.input.key_labels.insert(code, label);
                            }
                        }
                        // Les répétitions automatiques (touche gardée longtemps) ne
                        // remettent pas une touche que le système dit relâchée : en
                        // retard dans la file, elles faisaient continuer d'avancer.
                        // Exception : Windows croit la touche encore enfoncée depuis la
                        // fois d'avant (son relâchement a été avalé par un logiciel), donc
                        // ce nouvel appui lui paraît une répétition ; le clavier, lui, dit
                        // qu'elle vient d'être enfoncée : c'est un vrai appui.
                        if event.repeat {
                            let r = self
                                .input
                                .repeats
                                .entry(code)
                                .or_insert((0, Instant::now()));
                            *r = (r.0 + 1, Instant::now());
                        }
                        // Relâchée à tort (les répétitions avaient seulement du retard) :
                        // elle est toujours enfoncée, on la remet.
                        if event.repeat && self.input.soft.remove(&code) {
                            self.input.held.insert(code);
                        }
                        // Déjà vue par le clavier lui-même ou par l'état du système : ce
                        // message confirme l'appui (son retard est noté).
                        let held = self.input.held.contains(&code);
                        if held && !event.repeat {
                            self.input.confirm_press(code);
                        } else if held {
                            // Windows l'a vue, mais en répétition : pas de retard à noter.
                            self.input.early_press.remove(&code);
                        }
                        // Un appui que Windows donne pour une répétition (il croyait la
                        // touche encore enfoncée) est un vrai appui si la touche l'est
                        // vraiment : d'après le clavier, ou d'après l'état du système. Une
                        // répétition en retard d'une touche déjà relâchée, elle, ne remet
                        // rien (elle faisait continuer d'avancer).
                        let fresh = !held
                            && (!event.repeat
                                || self.input.raw_down.contains(&code)
                                || key_down_now(code));
                        if fresh {
                            if event.repeat {
                                self.input.press_stats.as_repeat += 1;
                            }
                            self.input.press_now(code, "message de Windows");
                        }
                        if code == KeyCode::Escape {
                            self.set_grab(false);
                        }
                    }
                    ElementState::Released => {
                        self.input.early_press.remove(&code);
                        if self.input.release_now(code) {
                            self.input.note_release(code, "message de Windows");
                        }
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
        release_stale_keys(&mut self.input);
        self.input.release_silent_keys();
        // La fenêtre a-t-elle le clavier ? (aussi lu ici : le message de prise du clavier
        // peut manquer au premier affichage de la fenêtre).
        if let Some(w) = &self.window {
            self.input.focused = w.has_focus();
        }
        catch_missed_presses(&mut self.input);
        self.input.report_presses(false);
        let now = Instant::now();
        let dt = (now - self.last).as_secs_f32().min(0.1);
        self.last = now;
        let Some(w) = self.window.clone() else { return };
        let keep_running = self.game.on_frame(&w, &self.input, dt);
        self.input.end_frame();
        // Chargé (ou trop long) : la fenêtre apparaît, déjà dessinée.
        if !self.shown && (self.game.loaded() || self.created.elapsed() > SHOW_AFTER) {
            self.shown = true;
            // Au centre de l'écran, sauf en plein écran (déjà posé sur l'écran).
            if w.fullscreen().is_none() {
                center(&w);
            }
            w.set_visible(true);
            w.focus_window();
            self.game.on_shown();
        }
        // Ouverture / fermeture d'une interface : libère ou recapture la souris.
        let open = self.game.ui_open();
        if open && self.input.grabbed {
            self.set_grab(false);
        } else if !open && self.ui_was_open {
            self.set_grab(true);
        }
        self.ui_was_open = open;
        if !keep_running {
            self.input.report_presses(true);
            self.game.on_exit();
            event_loop.exit();
        }
    }

    fn device_event(&mut self, _el: &ActiveEventLoop, id: DeviceId, event: DeviceEvent) {
        match event {
            DeviceEvent::MouseMotion { delta } => {
                if self.input.grabbed {
                    self.input.mouse_delta.0 += delta.0;
                    self.input.mouse_delta.1 += delta.1;
                }
            }
            // Le clavier lui-même : son relâchement fait foi, même si le message de
            // Windows n'arrive jamais (touche qui « reste enfoncée » et fait continuer
            // d'avancer).
            DeviceEvent::Key(raw) => {
                self.input.note_lag("clavier brut");
                self.input.note_device(device_name(&id));
                let PhysicalKey::Code(code) = raw.physical_key else {
                    return;
                };
                match raw.state {
                    ElementState::Pressed => {
                        if !self.input.raw_seen {
                            self.input.raw_seen = true;
                            log::info!("clavier : la saisie brute arrive");
                        }
                        self.input.raw_down.insert(code);
                        // Le clavier lui-même dit la touche enfoncée, et Windows aussi (son
                        // état des touches) : appui pris tout de suite, sans attendre son
                        // message (qui peut arriver en retard, ou pas du tout). Sans l'accord
                        // de Windows, rien : un logiciel qui change les touches (remappage)
                        // a pu avaler celle-ci, son message dira la bonne.
                        if self.input.focused
                            && !self.input.held.contains(&code)
                            && key_down_now(code)
                        {
                            self.input.press_now(code, "clavier (saisie brute)");
                        }
                    }
                    ElementState::Released => {
                        self.input.raw_down.remove(&code);
                        if self.input.release_now(code) {
                            self.input.note_release(code, "clavier (saisie brute)");
                            self.input.raw_released.push((code, Instant::now()));
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// Place la fenêtre au centre de l'écran où elle s'ouvre (l'écran principal sinon).
fn center(w: &Window) {
    let Some(m) = w.current_monitor().or_else(|| w.primary_monitor()) else {
        return;
    };
    let (mp, ms) = (m.position(), m.size());
    let ws = w.outer_size();
    let x = mp.x + (ms.width as i32 - ws.width as i32) / 2;
    let y = mp.y + (ms.height as i32 - ws.height as i32) / 2;
    w.set_outer_position(winit::dpi::PhysicalPosition::new(x.max(mp.x), y.max(mp.y)));
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

/// (v215) Le son du jeu n'est plus baissé par Windows pendant une communication (appel,
/// tchat vocal d'un autre logiciel : « réduire le volume des autres sons de 80 % ») : cette
/// baisse se fait lentement, et le son remonte lentement ensuite. Vaut pour la session son
/// du jeu sur la sortie par défaut ; à appeler une fois la sortie ouverte. Rend l'erreur de
/// Windows le cas échéant.
pub fn audio_no_ducking() -> Result<(), String> {
    #[cfg(windows)]
    unsafe {
        use windows::core::Interface;
        use windows::Win32::Media::Audio::{
            eConsole, eRender, IAudioSessionControl2, IAudioSessionManager2, IMMDeviceEnumerator,
            MMDeviceEnumerator,
        };
        use windows::Win32::System::Com::{
            CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED,
        };
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let run = || -> windows::core::Result<()> {
            let en: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
            let dev = en.GetDefaultAudioEndpoint(eRender, eConsole)?;
            let mgr: IAudioSessionManager2 = dev.Activate(CLSCTX_ALL, None)?;
            let ctl: IAudioSessionControl2 = mgr.GetAudioSessionControl(None, 0)?.cast()?;
            ctl.SetDuckingPreference(true)
        };
        run().map_err(|e| e.to_string())
    }
    #[cfg(not(windows))]
    Ok(())
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

/// Touches tenues que le système dit relâchées : le message de relâchement a été perdu ou
/// arrive en retard (file des messages encombrée) ; sans ce contrôle, le joueur continuait
/// d'avancer après avoir lâché la touche. Chaque image, l'état réel du clavier fait foi.
#[cfg(windows)]
fn release_stale_keys(input: &mut Input) {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, MapVirtualKeyW, MAPVK_VSC_TO_VK_EX,
    };
    use winit::platform::scancode::PhysicalKeyExtScancode;
    let down = |code: KeyCode| {
        let Some(sc) = PhysicalKey::Code(code).to_scancode() else {
            return false;
        };
        // SAFETY : simples lectures de l'état du clavier.
        let vk = unsafe { MapVirtualKeyW(sc, MAPVK_VSC_TO_VK_EX) };
        vk != 0 && unsafe { GetAsyncKeyState(vk as i32) } as u16 & 0x8000 != 0
    };
    // Touche relâchée d'après le clavier il y a plus d'un dixième de seconde, et que
    // Windows voit toujours enfoncée : un logiciel a avalé son relâchement. Le jeu l'a
    // déjà relâchée ; on le note une fois (c'est la cause du « ça continue d'avancer »).
    let mut waiting = std::mem::take(&mut input.raw_released);
    waiting.retain(|&(code, at)| {
        if at.elapsed().as_secs_f32() < 0.1 {
            return true;
        }
        if down(code) && !input.raw_down.contains(&code) && input.raw_warned.insert(code) {
            log::warn!(
                "touche {code:?} : le clavier l'a relâchée mais Windows la voit encore \
                 enfoncée (un logiciel intercepte le clavier : macro, superposition, \
                 pilote) ; le jeu suit le clavier"
            );
        }
        false
    });
    input.raw_released = waiting;
    let stale: Vec<KeyCode> = input
        .held
        .iter()
        .copied()
        .filter(|&code| {
            let Some(sc) = PhysicalKey::Code(code).to_scancode() else {
                return false;
            };
            // SAFETY : simples lectures de l'état du clavier.
            let vk = unsafe { MapVirtualKeyW(sc, MAPVK_VSC_TO_VK_EX) };
            vk != 0 && unsafe { GetAsyncKeyState(vk as i32) } as u16 & 0x8000 == 0
        })
        .collect();
    for code in stale {
        let since = input
            .pressed_at
            .get(&code)
            .map_or(0.0, |t| t.elapsed().as_secs_f32());
        log::info!(
            "touche {code:?} relâchée sans message du système : rattrapée ({since:.2} s après l'appui)"
        );
        input.release_now(code);
        input.note_release(code, "état du système");
    }
}

/// Touches filtrées de Windows (accessibilité) : quand elles sont actives, Windows attend
/// avant d'accepter un appui, ou ignore les appuis rapprochés. `None` : inactives.
/// (Elles s'activent aussi en tenant Maj droite 8 s, ce qui arrive en courant.)
#[cfg(windows)]
pub fn filter_keys() -> Option<String> {
    use windows::Win32::UI::WindowsAndMessaging::{
        SystemParametersInfoW, SPI_GETFILTERKEYS, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
    };
    #[repr(C)]
    #[derive(Default)]
    struct FilterKeys {
        size: u32,
        flags: u32,
        wait_ms: u32,
        delay_ms: u32,
        repeat_ms: u32,
        bounce_ms: u32,
    }
    let mut fk = FilterKeys {
        size: std::mem::size_of::<FilterKeys>() as u32,
        ..FilterKeys::default()
    };
    // SAFETY : lecture d'un réglage dans une structure de la taille annoncée.
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_GETFILTERKEYS,
            fk.size,
            Some(&mut fk as *mut FilterKeys as *mut _),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    };
    (ok.is_ok() && fk.flags & 1 != 0).then(|| {
        format!(
            "touches filtrées de Windows ACTIVES : appui accepté après {} ms, appuis \
             rapprochés ignorés pendant {} ms (Paramètres > Accessibilité > Clavier)",
            fk.wait_ms, fk.bounce_ms
        )
    })
}

#[cfg(not(windows))]
pub fn filter_keys() -> Option<String> {
    None
}

/// La touche `code` est-elle enfoncée en ce moment d'après l'état du système (mis à jour
/// avant que le message de Windows n'arrive dans la file du jeu) ?
#[cfg(windows)]
fn key_down_now(code: KeyCode) -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, MapVirtualKeyW, MAPVK_VSC_TO_VK_EX,
    };
    use winit::platform::scancode::PhysicalKeyExtScancode;
    let Some(sc) = PhysicalKey::Code(code).to_scancode() else {
        return false;
    };
    // SAFETY : simples lectures de l'état du clavier.
    let vk = unsafe { MapVirtualKeyW(sc, MAPVK_VSC_TO_VK_EX) };
    vk != 0 && unsafe { GetAsyncKeyState(vk as i32) } as u16 & 0x8000 != 0
}

#[cfg(not(windows))]
fn key_down_now(_code: KeyCode) -> bool {
    false
}

/// Touches de jeu regardées à chaque image (lettres, chiffres, espace, modificateurs,
/// flèches) : un appui sans message de Windows est quand même vu.
const WATCHED: &[KeyCode] = &[
    KeyCode::KeyA, KeyCode::KeyB, KeyCode::KeyC, KeyCode::KeyD, KeyCode::KeyE,
    KeyCode::KeyF, KeyCode::KeyG, KeyCode::KeyH, KeyCode::KeyI, KeyCode::KeyJ,
    KeyCode::KeyK, KeyCode::KeyL, KeyCode::KeyM, KeyCode::KeyN, KeyCode::KeyO,
    KeyCode::KeyP, KeyCode::KeyQ, KeyCode::KeyR, KeyCode::KeyS, KeyCode::KeyT,
    KeyCode::KeyU, KeyCode::KeyV, KeyCode::KeyW, KeyCode::KeyX, KeyCode::KeyY,
    KeyCode::KeyZ, KeyCode::Digit0, KeyCode::Digit1, KeyCode::Digit2, KeyCode::Digit3,
    KeyCode::Digit4, KeyCode::Digit5, KeyCode::Digit6, KeyCode::Digit7, KeyCode::Digit8,
    KeyCode::Digit9, KeyCode::Space, KeyCode::ShiftLeft, KeyCode::ShiftRight,
    KeyCode::ControlLeft, KeyCode::ControlRight, KeyCode::AltLeft, KeyCode::Tab,
    KeyCode::ArrowUp, KeyCode::ArrowDown, KeyCode::ArrowLeft, KeyCode::ArrowRight,
];

/// Appui qu'aucun message n'a annoncé : la touche est enfoncée d'après l'état du système,
/// pas encore pour le jeu. Avec la saisie brute, seulement si le clavier lui-même la dit
/// enfoncée aussi (l'état du système peut garder enfoncée une touche dont un logiciel a
/// avalé le relâchement) ; sans elle, parmi les touches de jeu courantes. Jamais une touche
/// relâchée faute de répétitions (`soft`), pour la même raison.
fn catch_missed_presses(input: &mut Input) {
    if !input.focused {
        return;
    }
    let candidates: Vec<KeyCode> = if input.raw_seen {
        input.raw_down.iter().copied().collect()
    } else {
        WATCHED.to_vec()
    };
    let missed: Vec<KeyCode> = candidates
        .into_iter()
        .filter(|k| !input.held.contains(k) && !input.soft.contains(k) && key_down_now(*k))
        .collect();
    for code in missed {
        input.press_now(code, "état du système");
    }
}

#[cfg(not(windows))]
fn release_stale_keys(_input: &mut Input) {}

/// Secondes écoulées depuis le lancement (pour dater les touches au millième).
fn uptime() -> f64 {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f64()
}

/// Nom de l'appareil d'un événement brut (`None` : aucun, la frappe vient d'un logiciel).
#[cfg(windows)]
fn device_name(id: &DeviceId) -> Option<String> {
    use winit::platform::windows::DeviceIdExtWindows;
    id.persistent_identifier()
}

#[cfg(not(windows))]
fn device_name(_id: &DeviceId) -> Option<String> {
    Some("clavier".into())
}

/// Temps écoulé (ms) depuis la date que Windows a donnée au message en cours de
/// traitement.
#[cfg(windows)]
fn message_lag_ms() -> u32 {
    use windows::Win32::System::SystemInformation::GetTickCount;
    use windows::Win32::UI::WindowsAndMessaging::GetMessageTime;
    // SAFETY : simples lectures (heure du système, date du dernier message de ce fil).
    let (now, at) = unsafe { (GetTickCount(), GetMessageTime() as u32) };
    let lag = now.wrapping_sub(at);
    // (Horloges à 16 ms près ; une date « dans le futur » compte pour zéro.)
    if lag > u32::MAX / 2 {
        0
    } else {
        lag
    }
}

#[cfg(not(windows))]
fn message_lag_ms() -> u32 {
    0
}

/// Répétition automatique du clavier : la version 116 la réduisait pendant le jeu (sans
/// effet prouvé). Il ne reste que la remise du réglage du joueur si cette version s'était
/// arrêtée brutalement.
mod key_repeat {
    #[cfg(windows)]
    pub fn restore_leftover() {
        use windows::Win32::UI::WindowsAndMessaging::{
            SystemParametersInfoW, SPI_SETKEYBOARDDELAY, SPI_SETKEYBOARDSPEED,
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
        };
        let file = std::env::temp_dir().join("minelib_repetition_clavier.txt");
        let Ok(text) = std::fs::read_to_string(&file) else {
            return;
        };
        let mut n = text
            .split_whitespace()
            .filter_map(|v| v.parse::<u32>().ok());
        if let (Some(speed), Some(delay)) = (n.next(), n.next()) {
            let none = SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0);
            // SAFETY : deux réglages passés par valeur, sans écriture dans le profil.
            unsafe {
                let _ = SystemParametersInfoW(SPI_SETKEYBOARDSPEED, speed.min(31), None, none);
                let _ = SystemParametersInfoW(SPI_SETKEYBOARDDELAY, delay.min(3), None, none);
            }
            log::info!("clavier : répétition automatique du joueur remise");
        }
        let _ = std::fs::remove_file(file);
    }

    #[cfg(not(windows))]
    pub fn restore_leftover() {}
}

#[cfg(test)]
mod tests {

    /// (v250) Ctrl+V colle sans écrire « v » ; Alt Gr tape toujours « @ ».
    #[test]
    fn ctrl_plus_une_lettre_n_ecrit_rien_mais_alt_gr_si() {
        let h = |ks: &[KeyCode]| ks.iter().copied().collect::<HashSet<KeyCode>>();
        assert!(raccourci(&h(&[KeyCode::ControlLeft])));
        assert!(raccourci(&h(&[KeyCode::ControlRight, KeyCode::ShiftLeft])));
        assert!(!raccourci(&h(&[KeyCode::ControlLeft, KeyCode::AltRight])));
        assert!(!raccourci(&h(&[KeyCode::ControlLeft, KeyCode::AltLeft])));
        assert!(!raccourci(&h(&[KeyCode::ShiftLeft])));
        assert!(!raccourci(&h(&[])));
    }
    use super::*;

    #[test]
    fn un_appui_plus_bref_qu_une_image_est_vu_une_image() {
        let mut i = Input {
            focused: true,
            ..Input::default()
        };
        i.press_now(KeyCode::KeyW, "message de Windows");
        assert!(i.release_now(KeyCode::KeyW));
        // Relâchée dans la même image : encore tenue pour cette image.
        assert!(i.held(KeyCode::KeyW) && i.pressed(KeyCode::KeyW));
        i.end_frame();
        assert!(!i.held(KeyCode::KeyW));
        assert_eq!(i.press_stats.taps, 1);
        // Tenue sur deux images : relâchée tout de suite.
        i.press_now(KeyCode::KeyW, "message de Windows");
        i.end_frame();
        assert!(i.release_now(KeyCode::KeyW));
        assert!(!i.held(KeyCode::KeyW));
        assert!(!i.release_now(KeyCode::KeyW));
    }

    #[test]
    fn appui_vu_par_le_clavier_puis_confirme_par_windows() {
        let mut i = Input {
            focused: true,
            ..Input::default()
        };
        i.press_now(KeyCode::KeyW, "clavier (saisie brute)");
        assert!(i.held(KeyCode::KeyW) && i.pressed(KeyCode::KeyW));
        std::thread::sleep(std::time::Duration::from_millis(5));
        i.confirm_press(KeyCode::KeyW);
        assert_eq!((i.press_stats.by_raw, i.press_stats.late), (1, 1));
        assert!(i.press_stats.late_max >= 4.0);
        // Un appui jamais confirmé par Windows (relâché avant) est compté.
        i.end_frame();
        i.press_now(KeyCode::KeyA, "clavier (saisie brute)");
        i.end_frame();
        assert!(i.release_now(KeyCode::KeyA));
        assert_eq!(i.press_stats.missing, 1);
        assert_eq!(i.press_stats.total, 2);
    }

    #[test]
    fn un_appui_annonce_par_le_clavier_ne_l_est_pas_deux_fois() {
        let mut i = Input {
            focused: true,
            ..Input::default()
        };
        // Vu par le clavier à l'image 1, message de Windows à l'image 2.
        i.press_now(KeyCode::KeyE, "clavier (saisie brute)");
        assert!(i.pressed(KeyCode::KeyE));
        i.end_frame();
        assert!(i.early_press.contains_key(&KeyCode::KeyE));
        assert!(i.held(KeyCode::KeyE) && !i.pressed(KeyCode::KeyE));
    }
}
