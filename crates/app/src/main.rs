//! Minelib — moteur de jeu voxel / FPS.
//!
//! Écrans : accueil → Solo (liste des mondes, nouveau monde) → jeu ; packs de textures ;
//! menu Échap (reprendre, packs de textures, sauvegarder et quitter).
//!
//! Données du joueur : `%APPDATA%\.minelib` sous Windows, `~/.minelib` sous Linux,
//! avec `resourcepacks/` (packs de textures, dossiers ou .zip) et `saves/` (mondes).
//!
//! Commandes en jeu (touches physiques, valables en AZERTY comme en QWERTY) :
//!   clic           capturer la souris          Échap        menu pause
//!   Z Q S D        se déplacer (W A S D en QWERTY)
//!   Espace         sauter / monter en vol      Maj gauche   descendre en vol
//!   Ctrl gauche    courir                       F            voler / marcher
//!   clic gauche    casser                       clic droit   poser
//!   1 à 0, molette choisir dans la barre        clic molette copier le bloc visé
//!   E              inventaire (10 × 3)          N            jour / nuit
//!   C (maintenu)   zoom                         V            VSync on/off
//!   Page ↑ / ↓     distance de rendu
//!   F3             écran de debug
//!
//! Variables d'environnement :
//!   MINELIB_HOME=<dossier>      remplace le dossier .minelib
//!   MINELIB_WORLD=<nom>         ouvre (ou crée) directement ce monde
//!   MINELIB_SEED=<n>            graine d'un monde créé par MINELIB_WORLD
//!   MINELIB_VIEW=<chunks>       distance de rendu (défaut 10)
//!   MINELIB_VSYNC=1             démarrer avec la VSync (défaut : FPS illimités)
//!   MINELIB_FPS_MAX=<n>         plafond de FPS optionnel
//!   MINELIB_DEBUG=1             F3 ouvert au démarrage
//!   MINELIB_NIGHT=1             commencer de nuit
//!   MINELIB_DEMO=1              nouveau monde : mur + toutes les torches colorées
//!   MINELIB_START=<x>,<y>,<z>[,lacet°,tangage°]  position de départ (vol activé)
//!   MINELIB_EXIT_AFTER=<s>      quitter automatiquement (tests)
//!   MINELIB_SMOKE=<x>,<y>,<z>   mode FPS : fumigène posée là toutes les 18 s (essais)
//!   MINELIB_SMOKE_Q=<n>,<pas>   qualité des fumigènes : pas max et longueur (essais)

// Windows : pas de fenêtre de console à côté du jeu (le journal va dans .minelib/logs).
#![cfg_attr(windows, windows_subsystem = "windows")]

mod audio;
mod chat;
mod debug;
mod equip;
mod fps;
mod gpu_plan;
mod gpu_report;
mod hand;
mod inventory;
mod jobs;
mod keys;
mod mcimport;
mod menu;
mod player;
mod player_model;
mod save;
mod session;
mod streaming;
mod wand;
mod weapons;
mod worldedit;

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use menu::{Ctx, GOLD, GREY, RED, WHITE};
use minelib_assets::{list_packs, Options, PackInfo, Paths, ResourcePack};
use minelib_platform::sysmon::SysMonitor;
use minelib_platform::{GameLoop, Input, KeyCode, PlatformWindow};
use minelib_render::{FrameStats, Renderer, Ui};
use save::WorldSummary;
use session::{Session, SessionConfig};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Screen {
    Title,
    Solo,
    NewWorld,
    Packs,
    Settings,
    Graphics,
    Controls,
    /// Conversion d'un monde Minecraft en cours (une image affichée avant de bloquer).
    Loading,
    Playing,
    Paused,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    Name,
    Seed,
}

struct Game {
    renderer: Option<Renderer>,
    paths: Paths,
    options: Options,
    /// Pack lu au démarrage, en attendant que le rendu soit prêt.
    startup_pack: Option<ResourcePack>,
    screen: Screen,
    /// Où revenir en quittant l'écran des packs.
    packs_return: Screen,
    /// Où revenir en quittant les paramètres.
    settings_return: Screen,
    /// Curseur de réglage tenu à la souris.
    slider_drag: Option<u32>,
    /// Distance de rendu permise par la mémoire vive (mesurée au lancement).
    ram_view_limit: i32,
    /// Plein écran réellement appliqué à la fenêtre.
    applied_fullscreen: Option<bool>,
    /// Écran branché sur la carte qui dessine (pas de recopie Optimus) : le jeu s'y place.
    /// (dernière vérification, nom de l'écran où le jeu a été placé)
    direct_display: (Option<std::time::Instant>, Option<String>),
    /// Champ texte de la sensibilité (en cours de saisie ou non).
    sens_text: String,
    sens_focus: bool,
    /// Touches du jeu, et action en attente d'une nouvelle touche.
    keys: keys::Bindings,
    binding: Option<keys::Action>,
    /// Menu des touches : onglet FPS (sinon Build) et lignes défilées.
    keys_tab_fps: bool,
    keys_scroll: usize,
    /// Barre de défilement du menu des touches tenue à la souris.
    keys_drag: bool,
    session: Option<Session>,
    config: SessionConfig,
    ui: Ui,
    sysmon: SysMonitor,
    time: f32,
    frame_times: VecDeque<f32>,
    last_stats: FrameStats,
    title_timer: f32,
    exit_after: Option<f32>,
    show_debug: bool,
    fps_max: Option<f32>,
    frame_start: Instant,
    // Écran Solo
    worlds: Vec<WorldSummary>,
    selected_world: Option<usize>,
    world_scroll: usize,
    last_row_click: (Option<usize>, f32),
    // Nouveau monde
    new_name: String,
    new_seed: String,
    focus: Field,
    // Packs
    packs: Vec<PackInfo>,
    /// Pack actif, et pack demandé (appliqué en fin d'image, quand le rendu est libre).
    current_pack: String,
    pack_request: Option<PackInfo>,
    message: Option<(String, f32)>,
    /// Écran branché sur une autre carte que celle qui dessine (Optimus) : son nom.
    optimus: Option<String>,
    /// Mémoire vidéo utilisée et budget Windows, relus chaque seconde.
    vram: Option<(minelib_platform::VramMonitor, std::time::Instant, bool)>,
    auto_world: Option<String>,
    pending_open: Option<(PathBuf, u32)>,
    /// Vider tous les maillages GPU (changement de monde) en fin d'image.
    clear_meshes: bool,
    quit: bool,
}

impl Game {
    fn new() -> anyhow::Result<Self> {
        let paths = minelib_assets::init()?;
        let keys = keys::Bindings::load(&paths.root.join("keys.txt"));
        let options = Options::load(&paths.options);
        jobs::set_uniform_sharing(options.cpu_uniform);
        let packs = list_packs(&paths.resourcepacks);
        let pack = packs
            .iter()
            .find(|p| p.name == options.resource_pack)
            .and_then(|p| {
                ResourcePack::open(p)
                    .map_err(|e| log::error!("pack « {} » : {e:#}", p.name))
                    .ok()
            })
            .map_or_else(ResourcePack::builtin, Ok)?;
        let view_distance = env_parse("MINELIB_VIEW")
            .unwrap_or(options.render_distance)
            .clamp(2, session::MAX_VIEW_DISTANCE);
        Ok(Self {
            renderer: None,
            worlds: save::list_worlds(&paths.saves),
            paths,
            options,
            startup_pack: Some(pack),
            screen: Screen::Title,
            packs_return: Screen::Title,
            settings_return: Screen::Title,
            slider_drag: None,
            ram_view_limit: session::ram_view_limit(),
            applied_fullscreen: None,
            direct_display: (None, None),
            sens_text: String::new(),
            sens_focus: false,
            keys,
            binding: None,
            keys_tab_fps: false,
            keys_scroll: 0,
            keys_drag: false,
            session: None,
            config: SessionConfig { view_distance },
            ui: Ui::default(),
            sysmon: SysMonitor::start(),
            time: 0.0,
            frame_times: VecDeque::new(),
            last_stats: FrameStats::default(),
            title_timer: 0.0,
            exit_after: env_parse("MINELIB_EXIT_AFTER"),
            show_debug: env_parse::<u8>("MINELIB_DEBUG").is_some_and(|v| v != 0),
            fps_max: env_parse::<f32>("MINELIB_FPS_MAX").filter(|&f| f > 0.0),
            frame_start: Instant::now(),
            selected_world: None,
            world_scroll: 0,
            last_row_click: (None, 0.0),
            new_name: String::new(),
            new_seed: String::new(),
            focus: Field::Name,
            packs,
            current_pack: String::new(),
            pack_request: None,
            message: None,
            auto_world: std::env::var("MINELIB_WORLD")
                .ok()
                .filter(|s| !s.trim().is_empty()),
            pending_open: None,
            clear_meshes: false,
            quit: false,
            optimus: None,
            vram: None,
        })
    }

    /// Enregistre une capture dans `.minelib/screenshots` (en tâche de fond) et l'annonce
    /// dans le tchat avec un lien (clic, tchat ouvert : copie dans le presse-papier).
    fn save_screenshot(&mut self, shot: minelib_render::Screenshot) {
        let dir = self.paths.root.join("screenshots");
        let _ = std::fs::create_dir_all(&dir);
        let base = stamp();
        let mut path = dir.join(format!("{base}.png"));
        let mut n = 1;
        while path.exists() {
            n += 1;
            path = dir.join(format!("{base}_{n}.png"));
        }
        let name = file_name(&path);
        log::info!("capture d'écran : {}", path.display());
        let target = path.clone();
        std::thread::spawn(move || {
            if let Err(e) = image::save_buffer(
                &target,
                &shot.rgba,
                shot.width,
                shot.height,
                image::ExtendedColorType::Rgba8,
            ) {
                log::error!("capture d'écran {} : {e}", target.display());
            }
        });
        match &mut self.session {
            Some(s) => s.chat.say_link(
                format!("Capture enregistrée : {name} (T puis clic : copier)"),
                chat::YELLOW,
                path,
            ),
            None => self.notify(format!("Capture enregistrée : {name}")),
        }
    }

    fn notify(&mut self, text: impl Into<String>) {
        self.message = Some((text.into(), 4.0));
    }

    fn refresh_worlds(&mut self) {
        self.worlds = save::list_worlds(&self.paths.saves);
        self.selected_world = None;
        self.world_scroll = 0;
    }

    fn play(&mut self, dir: PathBuf) {
        // Nouveau monde : rien de l'ancien ne doit rester à l'écran.
        self.session = None;
        // Le rendu est emprunté pendant l'image : on vide ses maillages en fin d'image.
        self.clear_meshes = true;
        match Session::open(dir, self.config) {
            Ok(mut s) => {
                s.sensitivity = self.options.sensitivity;
                s.volume = self.options.volume;
                s.keys = self.keys.clone();
                self.session = Some(s);
                self.screen = Screen::Playing;
            }
            Err(e) => self.notify(format!("Impossible d'ouvrir le monde : {e:#}")),
        }
    }

    /// Enregistre et ferme le monde en cours.
    fn leave_world(&mut self) {
        if let Some(mut s) = self.session.take() {
            if let Err(e) = s.save() {
                log::error!("sauvegarde impossible : {e:#}");
            }
        }
        // Le rendu est emprunté pendant l'image : on vide ses maillages en fin d'image.
        self.clear_meshes = true;
        self.refresh_worlds();
        self.screen = Screen::Title;
    }

    fn select_pack(&mut self, renderer: &mut Renderer, info: &PackInfo) {
        match ResourcePack::open(info) {
            Ok(p) => {
                renderer.load_pack(&p);
                self.current_pack = p.name.clone();
                self.options.resource_pack = info.name.clone();
                if let Err(e) = self.options.save(&self.paths.options) {
                    log::error!("options : {e:#}");
                }
            }
            Err(e) => self.notify(format!("Pack illisible : {e:#}")),
        }
    }

    fn record_frame_time(&mut self, dt: f32) {
        self.frame_times.push_back(dt);
        let mut total: f32 = self.frame_times.iter().sum();
        while total > 1.0 && self.frame_times.len() > 1 {
            total -= self.frame_times.pop_front().unwrap_or(0.0);
        }
    }

    /// Limite d'images par seconde : variable MINELIB_FPS_MAX, sinon le réglage.
    fn fps_limit(&self) -> Option<f32> {
        self.fps_max
            .or((self.options.fps_max > 0).then_some(self.options.fps_max as f32))
    }

    fn limit_fps(&mut self) {
        // Hors du jeu, les menus n'ont pas besoin de plus de 120 images/s.
        let max = if self.session.is_some() {
            self.fps_limit()
        } else {
            Some(self.fps_limit().unwrap_or(120.0).min(120.0))
        };
        if let Some(max) = max {
            let _s = minelib_render::perf::scope("limiteur de FPS (attente voulue)");
            let target = Duration::from_secs_f32(1.0 / max);
            loop {
                let elapsed = self.frame_start.elapsed();
                if elapsed >= target {
                    break;
                }
                let left = target - elapsed;
                if left > Duration::from_millis(2) {
                    std::thread::sleep(left - Duration::from_millis(1));
                } else {
                    std::hint::spin_loop();
                }
            }
        }
        self.frame_start = Instant::now();
    }

    // ------------------------------------------------------------ écrans

    fn title_screen(&mut self, input: &Input, size: (u32, u32)) {
        let mut c = Ctx::new(&mut self.ui, input, size);
        c.background();
        let s = c.s;
        let logo = s * 4.0;
        c.text_center("MINELIB", c.h * 0.18, logo, GOLD);
        c.text_center("moteur voxel en Rust", c.h * 0.18 + 36.0 * s, s, GREY);
        let cx = c.w / 2.0;
        let mut y = c.h * 0.42;
        let step = 24.0 * s;
        let solo = c.button("Solo", cx, y, 200.0, true);
        y += step;
        let settings = c.button("Paramètres", cx, y, 200.0, true);
        y += step * 1.5;
        let quit = c.button("Quitter le jeu", cx, y, 200.0, true);
        let ds = (s / 2.0).floor().max(1.0);
        c.ui.text(
            4.0 * s,
            c.h - 10.0 * ds - 2.0 * s,
            ds,
            GREY,
            format!("Minelib {}", env!("CARGO_PKG_VERSION")),
        );
        let credit = "Textures : VoxelArt Libre (CC BY 4.0 / CC0) - voir CREDITS.txt";
        let tw = Ui::text_width(credit, ds);
        c.ui.text(
            c.w - tw - 4.0 * s,
            c.h - 10.0 * ds - 2.0 * s,
            ds,
            GREY,
            credit,
        );
        if solo {
            self.refresh_worlds();
            self.screen = Screen::Solo;
        }
        if settings {
            self.settings_return = Screen::Title;
            self.screen = Screen::Settings;
        }
        if quit {
            self.quit = true;
        }
    }

    fn solo_screen(&mut self, input: &Input, size: (u32, u32)) {
        let time = self.time;
        let mut c = Ctx::new(&mut self.ui, input, size);
        c.background();
        let s = c.s;
        c.text_center("Mondes", 12.0 * s, s * 2.0, WHITE);
        let list_w = 260.0 * s;
        let list_x = (c.w - list_w) / 2.0;
        let list_y = 36.0 * s;
        let row_h = 26.0 * s;
        let bottom = c.h - 54.0 * s;
        let list_h = (bottom - list_y).max(row_h);
        let visible = ((list_h - 4.0 * s) / row_h).floor().max(1.0) as usize;
        c.frame(list_x, list_y, list_w, list_h);

        if self.worlds.is_empty() {
            let small = (s / 2.0).floor().max(1.0);
            c.text_center(
                "Aucun monde pour l'instant : crée-en un !",
                list_y + 20.0 * s,
                small,
                GREY,
            );
        }
        if input.scroll.abs() >= 1.0 && self.worlds.len() > visible {
            let max = self.worlds.len() - visible;
            let d = -input.scroll.signum() as i32;
            self.world_scroll = (self.world_scroll as i32 + d).clamp(0, max as i32) as usize;
        }
        let mut clicked = None;
        for (k, w) in self
            .worlds
            .iter()
            .enumerate()
            .skip(self.world_scroll)
            .take(visible)
        {
            let y = list_y + 2.0 * s + (k - self.world_scroll) as f32 * row_h;
            let detail = if w.minecraft {
                "Monde Minecraft   -   converti au lancement (originaux conservés)".to_string()
            } else {
                format!(
                    "graine {}   -   dernière partie {}",
                    w.meta.seed,
                    save::format_date(w.meta.last_played)
                )
            };
            if c.list_row(
                list_x + 2.0 * s,
                y,
                list_w - 4.0 * s,
                row_h - s,
                &w.meta.name,
                &detail,
                self.selected_world == Some(k),
            ) {
                clicked = Some(k);
            }
        }
        let cx = c.w / 2.0;
        let by = c.h - 48.0 * s;
        let play = c.button(
            "Jouer",
            cx - 105.0 * s,
            by,
            150.0,
            self.selected_world.is_some(),
        );
        let new = c.button("Nouveau monde", cx + 80.0 * s, by, 150.0, true);
        let back = c.button("Retour", cx, by + 24.0 * s, 150.0, true);

        let mut launch = None;
        if let Some(k) = clicked {
            // Double clic : jouer directement.
            if self.last_row_click.0 == Some(k) && time - self.last_row_click.1 < 0.4 {
                launch = Some(k);
            }
            self.selected_world = Some(k);
            self.last_row_click = (Some(k), time);
        }
        if play {
            launch = self.selected_world;
        }
        if let Some(k) = launch {
            let dir = self.worlds[k].dir.clone();
            if self.worlds[k].minecraft {
                self.pending_open = Some((dir, 0));
                self.screen = Screen::Loading;
            } else {
                self.play(dir);
            }
        }
        if new {
            self.new_name = format!("Nouveau monde {}", self.worlds.len() + 1);
            self.new_seed.clear();
            self.focus = Field::Name;
            self.screen = Screen::NewWorld;
        }
        if back || input.pressed(KeyCode::Escape) {
            self.screen = Screen::Title;
        }
    }

    fn new_world_screen(&mut self, input: &Input, size: (u32, u32)) {
        match self.focus {
            Field::Name => menu::edit_text(&mut self.new_name, input, 32),
            Field::Seed => menu::edit_text(&mut self.new_seed, input, 24),
        }
        if input.pressed(KeyCode::Tab) {
            self.focus = if self.focus == Field::Name {
                Field::Seed
            } else {
                Field::Name
            };
        }
        let time = self.time;
        let mut c = Ctx::new(&mut self.ui, input, size);
        c.background();
        let s = c.s;
        c.text_center("Nouveau monde", 12.0 * s, s * 2.0, WHITE);
        let cx = c.w / 2.0;
        let mut y = c.h * 0.3;
        if c.text_field(
            "Nom du monde",
            &self.new_name,
            "Mon monde",
            cx,
            y,
            220.0,
            self.focus == Field::Name,
            time,
        ) {
            self.focus = Field::Name;
        }
        y += 40.0 * s;
        if c.text_field(
            "Graine",
            &self.new_seed,
            "vide = aléatoire",
            cx,
            y,
            220.0,
            self.focus == Field::Seed,
            time,
        ) {
            self.focus = Field::Seed;
        }
        let by = c.h - 48.0 * s;
        let ok = !self.new_name.trim().is_empty();
        let create = c.button("Créer le monde", cx - 80.0 * s, by, 150.0, ok);
        let cancel = c.button("Annuler", cx + 80.0 * s, by, 150.0, true);
        if create || (ok && input.pressed(KeyCode::Enter)) {
            let seed = save::parse_seed(&self.new_seed);
            match save::create_world(&self.paths.saves, &self.new_name, seed) {
                Ok(dir) => self.play(dir),
                Err(e) => self.notify(format!("Création impossible : {e:#}")),
            }
        }
        if cancel || input.pressed(KeyCode::Escape) {
            self.screen = Screen::Solo;
        }
    }

    fn packs_screen(&mut self, input: &Input, size: (u32, u32), over_game: bool) {
        let current = self.current_pack.clone();
        let mut c = Ctx::new(&mut self.ui, input, size);
        if over_game {
            c.dim(0.7);
        } else {
            c.background();
        }
        let s = c.s;
        c.text_center("Packs de textures", 12.0 * s, s * 2.0, WHITE);
        let list_w = 260.0 * s;
        let list_x = (c.w - list_w) / 2.0;
        let list_y = 36.0 * s;
        let row_h = 26.0 * s;
        let list_h = (c.h - 60.0 * s - list_y).max(row_h);
        c.frame(list_x, list_y, list_w, list_h);
        let visible = ((list_h - 4.0 * s) / row_h).floor().max(1.0) as usize;
        let mut chosen = None;
        for (k, p) in self.packs.iter().enumerate().take(visible) {
            let y = list_y + 2.0 * s + k as f32 * row_h;
            let active = p.name == current;
            let kind = if p.path.is_dir() {
                "dossier"
            } else {
                "archive .zip"
            };
            let detail = if active {
                format!("{kind}   -   actif")
            } else {
                format!("{kind}   -   cliquer pour l'utiliser")
            };
            if c.list_row(
                list_x + 2.0 * s,
                y,
                list_w - 4.0 * s,
                row_h - s,
                &p.name,
                &detail,
                active,
            ) && !active
            {
                chosen = Some(k);
            }
        }
        let cx = c.w / 2.0;
        let by = c.h - 48.0 * s;
        let open = c.button("Ouvrir le dossier", cx - 80.0 * s, by, 150.0, true);
        let refresh = c.button("Actualiser", cx + 80.0 * s, by, 150.0, true);
        let done = c.button("Terminé", cx, by + 24.0 * s, 150.0, true);
        if let Some(k) = chosen {
            self.pack_request = Some(self.packs[k].clone());
        }
        if open {
            open_folder(&self.paths.resourcepacks);
        }
        if refresh {
            self.packs = list_packs(&self.paths.resourcepacks);
        }
        if done || input.pressed(KeyCode::Escape) {
            self.screen = self.packs_return;
        }
    }

    fn loading_screen(&mut self, input: &Input, size: (u32, u32)) {
        let mut c = Ctx::new(&mut self.ui, input, size);
        c.background();
        let s = c.s;
        c.text_center(
            "Conversion du monde Minecraft...",
            c.h * 0.42,
            s * 2.0,
            WHITE,
        );
        let small = (s / 2.0).floor().max(1.0);
        c.text_center("Les fichiers d'origine ne sont pas modifiés ; la sauvegarde se fera au format Minelib.", c.h * 0.42 + 26.0 * s, small, GREY);
    }

    fn pause_menu(&mut self, input: &Input, size: (u32, u32)) {
        let mut c = Ctx::new(&mut self.ui, input, size);
        c.dim(0.55);
        let s = c.s;
        c.text_center("Jeu en pause", c.h * 0.2, s * 2.0, WHITE);
        let cx = c.w / 2.0;
        let mut y = c.h * 0.36;
        let resume = c.button("Reprendre", cx, y, 200.0, true);
        y += 24.0 * s;
        let settings = c.button("Paramètres", cx, y, 200.0, true);
        let quit = c.button("Sauvegarder et quitter", cx, c.h * 0.72, 200.0, true);
        if resume {
            self.screen = Screen::Playing;
        }
        if settings {
            self.settings_return = Screen::Paused;
            self.screen = Screen::Settings;
        }
        if quit {
            self.leave_world();
        }
    }

    fn settings_screen(&mut self, input: &Input, size: (u32, u32), over_game: bool) {
        let mut c = Ctx::new(&mut self.ui, input, size);
        if over_game {
            c.dim(0.7);
        } else {
            c.background();
        }
        let s = c.s;
        c.text_center("Paramètres", c.h * 0.18, s * 2.0, WHITE);
        let cx = c.w / 2.0;
        let mut y = c.h * 0.36;
        let graphics = c.button("Paramètres graphiques", cx, y, 200.0, true);
        y += 24.0 * s;
        let controls = c.button("Régler les touches", cx, y, 200.0, true);
        y += 24.0 * s;
        let packs = c.button("Packs de textures", cx, y, 200.0, true);
        y += 24.0 * s;
        // Volume des sons (tirs, rechargements, lunette).
        let vol = self.options.volume;
        let label = format!("Volume : {} %", (vol * 100.0).round());
        if let Some(f) = c.slider(7, &mut self.slider_drag, &label, vol, cx, y, 200.0) {
            let v = (f * 20.0).round() / 20.0;
            if v != self.options.volume {
                self.options.volume = v;
                if let Some(sess) = &mut self.session {
                    sess.volume = v;
                }
                crate::audio::play(crate::audio::Sfx::Dry, 0.0, v);
            }
        }
        let done = c.button("Terminé", cx, c.h * 0.72, 200.0, true);
        if graphics {
            self.screen = Screen::Graphics;
        }
        if controls {
            self.sens_text = format_sens(self.options.sensitivity);
            self.sens_focus = false;
            self.screen = Screen::Controls;
        }
        if packs {
            self.packs = list_packs(&self.paths.resourcepacks);
            self.packs_return = Screen::Settings;
            self.screen = Screen::Packs;
        }
        if done || input.pressed(KeyCode::Escape) {
            self.slider_drag = None;
            if let Err(e) = self.options.save(&self.paths.options) {
                log::error!("options : {e:#}");
            }
            self.screen = self.settings_return;
        }
    }

    /// Réglage des commandes : sensibilité de la souris (valeurs Counter-Strike), par curseur
    /// ou en tapant la valeur dans le champ à droite.
    fn controls_screen(&mut self, input: &Input, size: (u32, u32), over_game: bool) {
        let time = self.time;
        let mut sens = self.options.sensitivity;
        let mut c = Ctx::new(&mut self.ui, input, size);
        if over_game {
            c.dim(0.7);
        } else {
            c.background();
        }
        let s = c.s;
        c.text_center("Régler les touches", c.h * 0.06, s * 2.0, WHITE);
        let cx = c.w / 2.0;
        // Deux onglets : Build (modes de construction) et FPS (touches de CS:GO).
        let tab_y = c.h * 0.06 + 22.0 * s;
        let fps_tab = self.keys_tab_fps;
        let (build_label, fps_label) = if fps_tab {
            ("Build", "> FPS <")
        } else {
            ("> Build <", "FPS")
        };
        if c.button(build_label, cx - 52.0 * s, tab_y, 100.0, true) {
            self.keys_tab_fps = false;
            self.keys_scroll = 0;
            self.binding = None;
        }
        if c.button(fps_label, cx + 52.0 * s, tab_y, 100.0, true) {
            self.keys_tab_fps = true;
            self.keys_scroll = 0;
            self.binding = None;
        }
        let y = tab_y + 26.0 * s;
        // Curseur 0,1 à 8 (plage du jeu Counter-Strike) ; le champ accepte 0,01 à 20.
        let (lo, hi) = (0.1f32, 8.0f32);
        let label = format!("Sensibilité souris : {}", format_sens(sens));
        let frac = (sens.clamp(lo, hi) - lo) / (hi - lo);
        if let Some(f) = c.slider(
            3,
            &mut self.slider_drag,
            &label,
            frac,
            cx - 34.0 * s,
            y,
            230.0,
        ) {
            sens = ((lo + f * (hi - lo)) * 100.0).round() / 100.0;
            self.sens_text = format_sens(sens);
            self.sens_focus = false;
        }
        let field_clicked = c.text_field(
            "",
            &self.sens_text,
            "1.25",
            cx + 116.0 * s,
            y,
            60.0,
            self.sens_focus,
            time,
        );
        if field_clicked {
            self.sens_focus = true;
            self.binding = None;
        } else if c.click && self.sens_focus {
            self.sens_focus = false; // clic ailleurs : on valide
        }
        let ds = (s / 2.0).floor().max(1.0);
        c.text_center(
            "Valeurs de Counter-Strike : 0,022° par unité de souris × sensibilité.",
            y + 23.0 * s,
            ds,
            GREY,
        );

        // En attente (depuis une image précédente) : la prochaine touche ou le prochain clic
        // est assigné (Échap annule). Un clic ainsi pris n'active aucun bouton.
        let mut cancel_escape = false;
        if let Some(action) = self.binding {
            if input.pressed(KeyCode::Escape) {
                self.binding = None;
                cancel_escape = true;
            } else if let Some(k) = input
                .pressed_keys()
                .into_iter()
                .find(|k| keys::is_bindable(*k))
            {
                self.keys.bind(action, keys::Bind::Key(k));
                self.binding = None;
            } else if let Some(b) = input.mouse_pressed_any() {
                self.keys.bind(action, keys::Bind::Mouse(b));
                self.binding = None;
                c.click = false;
            }
        }

        // Onglet FPS : marcher et s'accroupir en « maintenir » ou « bascule ».
        let mut top = y + 34.0 * s;
        if self.keys_tab_fps {
            let mode = |t: bool| if t { "bascule" } else { "maintenir" };
            let walk = format!("Marcher : {}", mode(self.keys.walk_toggle));
            let duck = format!("S'accroupir : {}", mode(self.keys.duck_toggle));
            if c.button(&walk, cx - 124.0 * s, top, 240.0, true) {
                self.keys.walk_toggle = !self.keys.walk_toggle;
            }
            if c.button(&duck, cx + 124.0 * s, top, 240.0, true) {
                self.keys.duck_toggle = !self.keys.duck_toggle;
            }
            top += 24.0 * s;
        }

        // Une ligne par action : nom, bouton de la touche (cliquer puis appuyer sur la
        // nouvelle touche ou cliquer), bouton X pour la retirer. Deux colonnes ; la molette
        // fait défiler quand tout ne tient pas.
        let actions = keys::Action::tab(self.keys_tab_fps);
        let row_h = 20.0 * s;
        let col_w = 244.0 * s;
        let done_y = c.h * 0.91;
        let rows = actions.len().div_ceil(2);
        let visible = (((done_y - 14.0 * s - top) / row_h).floor() as usize).max(1);
        let max_scroll = rows.saturating_sub(visible);
        if input.scroll.abs() >= 1.0 {
            self.keys_scroll = if input.scroll > 0.0 {
                self.keys_scroll.saturating_sub(1)
            } else {
                self.keys_scroll + 1
            };
        }
        // Barre de défilement (comme dans l'inventaire créatif) : on attrape le curseur et
        // la liste suit la souris ; un clic dans la barre y saute.
        if max_scroll > 0 {
            let (bx, by, bw, bh) = (
                cx + 256.0 * s,
                top,
                8.0 * s,
                visible as f32 * row_h - 4.0 * s,
            );
            if let Some((mx, my)) = c.cursor {
                if c.click
                    && mx >= bx - 3.0 * s
                    && mx <= bx + bw + 3.0 * s
                    && my >= by
                    && my <= by + bh
                {
                    self.keys_drag = true;
                    c.click = false;
                }
                if self.keys_drag {
                    let frac = ((my - by) / bh).clamp(0.0, 1.0);
                    let first = frac * rows as f32 - visible as f32 / 2.0;
                    self.keys_scroll = first.round().max(0.0) as usize;
                }
            }
            if !c.held {
                self.keys_drag = false;
            }
            self.keys_scroll = self.keys_scroll.min(max_scroll);
            let shown = (visible as f32 / rows as f32).min(1.0);
            let start = self.keys_scroll as f32 / rows as f32;
            c.ui.rect(bx, by, bw, bh, [0.041, 0.041, 0.05, 1.0]);
            c.ui.rect(
                bx + s,
                by + bh * start,
                bw - 2.0 * s,
                (bh * shown).max(4.0 * s),
                [0.578, 0.578, 0.64, 1.0],
            );
        } else {
            self.keys_drag = false;
        }
        self.keys_scroll = self.keys_scroll.min(max_scroll);
        for (i, action) in actions.into_iter().enumerate() {
            let (row, col) = (i / 2, i % 2);
            if row < self.keys_scroll || row >= self.keys_scroll + visible {
                continue;
            }
            let x0 = cx - col_w + col as f32 * (col_w + 6.0 * s);
            let ry = top + (row - self.keys_scroll) as f32 * row_h;
            let grey = action.coming_soon();
            c.ui.text(
                x0,
                ry + 6.0 * s,
                s,
                if grey { GREY } else { WHITE },
                action.label(),
            );
            let waiting = self.binding == Some(action);
            let key = self.keys.get(action);
            let text = if waiting {
                "> ? <".to_string()
            } else {
                key.map_or("aucune".into(), |k| k.name(input))
            };
            if c.button(&text, x0 + 178.0 * s, ry, 92.0, true) {
                self.binding = Some(action);
                self.sens_focus = false;
            }
            if c.button("X", x0 + 236.0 * s, ry, 16.0, key.is_some()) {
                self.keys.unbind(action);
                if self.binding == Some(action) {
                    self.binding = None;
                }
            }
        }
        if max_scroll > 0 || self.keys_tab_fps {
            let mut hint = String::new();
            if max_scroll > 0 {
                hint = format!(
                    "Molette : faire défiler ({}-{} sur {} lignes)",
                    self.keys_scroll + 1,
                    (self.keys_scroll + visible).min(rows),
                    rows
                );
            }
            if self.keys_tab_fps {
                if !hint.is_empty() {
                    hint += "   ";
                }
                hint += "En gris : pas encore dans le jeu (la touche est gardée).";
            }
            c.text_center(&hint, done_y - 11.0 * s, ds, GREY);
        }
        let done = c.button("Terminé", cx, c.h * 0.91, 200.0, true);
        if self.sens_focus {
            for ch in input
                .text
                .chars()
                .filter(|ch| ch.is_ascii_digit() || *ch == '.' || *ch == ',')
            {
                if self.sens_text.len() < 6 {
                    self.sens_text.push(if ch == ',' { '.' } else { ch });
                }
            }
            if input.pressed(KeyCode::Backspace) {
                self.sens_text.pop();
            }
            if input.pressed(KeyCode::Enter) || input.pressed(KeyCode::NumpadEnter) {
                self.sens_focus = false;
            }
        }
        // Valeur tapée : appliquée dès qu'elle est lisible.
        if let Ok(v) = self.sens_text.trim().parse::<f32>() {
            if (minelib_assets::MIN_SENSITIVITY..=minelib_assets::MAX_SENSITIVITY).contains(&v) {
                sens = v;
            }
        }
        if !self.sens_focus {
            self.sens_text = format_sens(sens);
        }
        if sens != self.options.sensitivity {
            self.options.sensitivity = sens;
            if let Some(sess) = &mut self.session {
                sess.sensitivity = sens;
            }
        }
        if let Some(sess) = &mut self.session {
            sess.keys = self.keys.clone();
        }
        if done || (input.pressed(KeyCode::Escape) && !cancel_escape) {
            self.slider_drag = None;
            self.sens_focus = false;
            self.binding = None;
            if let Err(e) = self.options.save(&self.paths.options) {
                log::error!("options : {e:#}");
            }
            if let Err(e) = self.keys.save(&self.paths.root.join("keys.txt")) {
                log::error!("touches : {e}");
            }
            self.screen = Screen::Settings;
        }
    }

    fn graphics_screen(
        &mut self,
        input: &Input,
        size: (u32, u32),
        over_game: bool,
        renderer: &mut Renderer,
    ) {
        if let Some(sess) = &self.session {
            self.config = sess.config();
        }
        let mut cfg = self.config;
        let mut low_latency = renderer.low_latency();
        let mut raytracing = renderer.raytracing();
        let mut scale = renderer.render_scale();
        let mut c = Ctx::new(&mut self.ui, input, size);
        if over_game {
            c.dim(0.7);
        } else {
            c.background();
        }
        let s = c.s;
        c.text_center("Paramètres graphiques", c.h * 0.14, s * 2.0, WHITE);
        let cx = c.w / 2.0;
        let mut y = c.h * 0.21;
        let step = 21.5 * s;
        // Distance de rendu : 2 à 64 chunks (curseur plus fin aux petites valeurs).
        let max = session::MAX_VIEW_DISTANCE;
        let ram = self.ram_view_limit;
        let label = if cfg.view_distance > ram {
            format!(
                "Distance de rendu : {} chunks (mémoire : {ram} max)",
                cfg.view_distance
            )
        } else {
            format!("Distance de rendu : {} chunks", cfg.view_distance)
        };
        let frac = ((cfg.view_distance - 2) as f32 / (max - 2) as f32).sqrt();
        if let Some(f) = c.slider(1, &mut self.slider_drag, &label, frac, cx, y, 300.0) {
            cfg.view_distance = 2 + (f * f * (max - 2) as f32).round() as i32;
        }
        y += step;
        // Échelle de rendu : 50 à 100 % (par pas de 5 %), reconstruite par l'anticrénelage.
        let label = format!("Échelle de rendu : {} %", (scale * 100.0).round() as i32);
        if let Some(f) = c.slider(
            3,
            &mut self.slider_drag,
            &label,
            (scale - 0.5) / 0.5,
            cx,
            y,
            300.0,
        ) {
            scale = ((0.5 + f * 0.5) * 20.0).round() / 20.0;
        }
        y += step;
        // Limite de FPS : 30 à 500 par pas de 10, tout à droite = illimité. Moins d'images,
        // moins de chauffe : les FPS restent stables au lieu de baisser quand le PC chauffe.
        let cur = self.options.fps_max;
        let label = if cur == 0 {
            "Limite de FPS : illimité".to_string()
        } else {
            format!("Limite de FPS : {cur}")
        };
        let frac = if cur == 0 {
            1.0
        } else {
            (cur.clamp(30, 500) - 30) as f32 / 470.0 * 0.985
        };
        if let Some(f) = c.slider(6, &mut self.slider_drag, &label, frac, cx, y, 300.0) {
            self.options.fps_max = if f > 0.985 {
                0
            } else {
                (30.0 + f / 0.985 * 470.0).round() as u32 / 10 * 10
            };
        }
        y += step;
        let ds = (s / 2.0).floor().max(1.0);
        // Raytracing des reflets (écrit pour Minelib : marche sur toutes les cartes).
        let rt_label = if raytracing {
            "Raytracing (reflets) : activé"
        } else {
            "Raytracing (reflets) : désactivé"
        };
        if c.button(rt_label, cx, y, 300.0, true) {
            raytracing = !raytracing;
        }
        y += step;
        let fs_label = if self.options.fullscreen {
            "Plein écran : oui (F11)"
        } else {
            "Plein écran : non (F11)"
        };
        if c.button(fs_label, cx, y, 300.0, true) {
            self.options.fullscreen = !self.options.fullscreen;
        }
        y += step;
        // Répartition du processeur (prise en compte au prochain monde chargé).
        let cpu_label = if self.options.cpu_uniform {
            "Processeur : tous les coeurs"
        } else {
            "Processeur : coeurs réservés"
        };
        if c.button(cpu_label, cx, y, 300.0, true) {
            self.options.cpu_uniform = !self.options.cpu_uniform;
            jobs::set_uniform_sharing(self.options.cpu_uniform);
        }
        y += step;
        // Mode basse latence : seulement si la carte le propose (NVIDIA Reflex, AMD Anti-Lag 2).
        if let Some((kind, on)) = &mut low_latency {
            let label = format!(
                "{} : {}",
                kind.label(),
                if *on { "activé" } else { "désactivé" }
            );
            if c.button(&label, cx, y, 300.0, true) {
                *on = !*on;
            }
            y += step;
        }
        c.text_center(
            "Échelle sous 100 % : monde calculé en plus petit puis reconstruit (plus de FPS).",
            y + 4.0 * s,
            ds,
            GREY,
        );
        if let Some(shown) = &self.optimus {
            c.text_center(
                &format!(
                    "Écran sur « {shown} » : active le mode GPU dédié (MUX) pour plus de FPS."
                ),
                y + 4.0 * s + ds * 10.0,
                ds,
                GREY,
            );
        }
        let done = c.button("Terminé", cx, c.h * 0.78, 200.0, true);
        let leave = done || input.pressed(KeyCode::Escape);

        if cfg.view_distance != self.config.view_distance {
            self.config = cfg;
            if let Some(sess) = &mut self.session {
                sess.apply_config(cfg);
            }
        }
        if raytracing != renderer.raytracing() {
            renderer.set_raytracing(raytracing);
        }
        if (scale - renderer.render_scale()).abs() > 1e-3 {
            renderer.set_render_scale(scale);
        }
        if let Some((_, on)) = low_latency {
            if renderer.low_latency().is_some_and(|(_, cur)| cur != on) {
                renderer.set_low_latency(on);
            }
        }
        if leave {
            self.slider_drag = None;
            self.options.render_distance = self.config.view_distance;
            if let Some((_, on)) = renderer.low_latency() {
                self.options.low_latency = on;
            }
            self.options.raytracing = renderer.raytracing();
            self.options.render_scale = renderer.render_scale();
            if let Err(e) = self.options.save(&self.paths.options) {
                log::error!("options : {e:#}");
            }
            self.screen = Screen::Settings;
        }
    }

    fn draw_message(&mut self, size: (u32, u32), dt: f32) {
        let Some((text, t)) = &mut self.message else {
            return;
        };
        *t -= dt;
        if *t <= 0.0 {
            self.message = None;
            return;
        }
        let s = (size.1 as f32 / 300.0).floor().max(1.0);
        let tw = Ui::text_width(text, s);
        let x = (size.0 as f32 - tw) / 2.0;
        self.ui.rect(
            x - 6.0 * s,
            4.0 * s,
            tw + 12.0 * s,
            14.0 * s,
            [0.0, 0.0, 0.0, 0.8],
        );
        self.ui.text(x, 7.0 * s, s, RED, text.clone());
    }
}

impl GameLoop for Game {
    fn on_ready(&mut self, window: Arc<PlatformWindow>) {
        let pack = self
            .startup_pack
            .take()
            .unwrap_or_else(|| ResourcePack::builtin().expect("pack intégré"));
        match Renderer::new(window, &pack) {
            Ok(mut r) => {
                r.set_low_latency(self.options.low_latency);
                r.set_raytracing(self.options.raytracing);
                r.set_render_scale(self.options.render_scale);
                // Répartition du travail selon la carte (et réglages du 1er lancement).
                let first_launch = !self.paths.options.exists();
                gpu_plan::apply(&mut r, &mut self.options, first_launch);
                // Anticrénelage temporel : toujours actif (MINELIB_TAA=0 seulement pour les tests).
                r.set_antialiasing(std::env::var("MINELIB_TAA").map_or(true, |v| v != "0"));
                // Essais : MINELIB_SMOKE_Q=<pas max>,<longueur d'un pas> (qualité des fumigènes).
                if let Ok(q) = std::env::var("MINELIB_SMOKE_Q") {
                    let v: Vec<f32> = q.split(',').filter_map(|s| s.trim().parse().ok()).collect();
                    if v.len() == 2 {
                        r.set_smoke_quality(minelib_render::SmokeQuality {
                            max_steps: v[0] as u32,
                            step: v[1],
                        });
                    }
                }
                self.sysmon.set_gpu_name(&r.adapter_name);
                // Mémoire vidéo : 80 % de la mémoire dédiée de la carte, moins 512 Mo pour
                // l'image, l'anticrénelage et les textures.
                self.vram = minelib_platform::VramMonitor::new(&r.adapter_name)
                    .map(|m| (m, std::time::Instant::now(), false));
                let adapters = minelib_platform::gpu_adapters();
                if let Some(a) = adapters
                    .iter()
                    .find(|a| r.adapter_name.contains(&a.name) || a.name.contains(&r.adapter_name))
                {
                    if a.dedicated > 1 << 30 {
                        r.set_vram_budget((a.dedicated as f64 * 0.8) as u64 - (512 << 20));
                    }
                    // Optimus : l'écran est sur une autre carte, chaque image y est recopiée.
                    // Le basculement (MUX) ne se fait que par le logiciel du portable ou le
                    // BIOS, avec redémarrage : on prévient le joueur.
                    if !a.has_display {
                        if let Some(d) = adapters.iter().find(|d| d.has_display) {
                            self.optimus = Some(d.name.clone());
                            log::info!(
                                "écran sur « {} » : le mode GPU dédié (MUX) donnerait plus de FPS",
                                d.name
                            );
                        }
                    }
                }
                // Cartes graphiques, écran, partie intégrée : bilan dans le journal.
                gpu_report::spawn(r.adapter_name.clone());
                self.current_pack = r.pack_name.clone();
                self.renderer = Some(r);
            }
            Err(e) => log::error!("initialisation du rendu impossible : {e:#}"),
        }
        // Lancement direct dans un monde (tests, raccourci).
        if let Some(name) = self.auto_world.take() {
            let existing = self
                .worlds
                .iter()
                .find(|w| w.meta.name == name)
                .map(|w| w.dir.clone());
            let dir = existing.or_else(|| {
                let seed = std::env::var("MINELIB_SEED")
                    .map(|s| save::parse_seed(&s))
                    .unwrap_or(20_261_108);
                save::create_world(&self.paths.saves, &name, seed).ok()
            });
            if let Some(dir) = dir {
                self.play(dir);
            }
        }
    }

    fn on_resize(&mut self, width: u32, height: u32) {
        if let Some(r) = &mut self.renderer {
            r.resize(width, height);
        }
    }

    fn on_frame(&mut self, window: &PlatformWindow, input: &Input, dt: f32) -> bool {
        self.time += dt;
        self.record_frame_time(dt);
        if let Some(limit) = self.exit_after {
            if self.time > limit {
                log::info!("arrêt automatique après {limit} s");
                return false;
            }
        }
        // Plein écran : F11, ou le bouton des paramètres graphiques.
        if input.pressed(KeyCode::F11) {
            self.options.fullscreen = !self.options.fullscreen;
            if let Err(e) = self.options.save(&self.paths.options) {
                log::error!("options : {e:#}");
            }
        }
        // Le plus proche d'un MUX sans MUX : si un écran est branché sur une prise reliée à la
        // carte qui dessine (souvent la HDMI sur les portables Optimus), le jeu s'y affiche
        // tout seul (vérifié toutes les 3 s : écran branché en cours de partie). L'image n'est
        // alors plus recopiée vers la puce Intel.
        let check = self
            .direct_display
            .0
            .is_none_or(|t| t.elapsed().as_secs_f32() >= 3.0);
        let mut target: Option<winit::monitor::MonitorHandle> = None;
        if check {
            self.direct_display.0 = Some(std::time::Instant::now());
            if let Some(r) = &self.renderer {
                let outputs = minelib_platform::gpu_output_names(&r.adapter_name);
                target = window
                    .available_monitors()
                    .find(|m| m.name().is_some_and(|n| outputs.contains(&n)));
            }
            let name = target.as_ref().and_then(|m| m.name());
            if name != self.direct_display.1 {
                if let (Some(m), Some(n)) = (&target, &name) {
                    log::info!("écran {n} branché sur la carte qui dessine : le jeu s'y affiche (pas de recopie entre cartes)");
                    self.optimus = None;
                    if !self.options.fullscreen {
                        window.set_outer_position(m.position());
                    }
                }
                self.direct_display.1 = name;
                self.applied_fullscreen = None; // replacer le plein écran sur le bon écran
            }
        }
        if self.applied_fullscreen != Some(self.options.fullscreen) {
            let monitor = self.direct_display.1.as_ref().and_then(|n| {
                window
                    .available_monitors()
                    .find(|m| m.name().as_ref() == Some(n))
            });
            window.set_fullscreen(
                self.options
                    .fullscreen
                    .then_some(winit::window::Fullscreen::Borderless(monitor)),
            );
            self.applied_fullscreen = Some(self.options.fullscreen);
            log::info!(
                "plein écran : {}",
                if self.options.fullscreen {
                    "oui"
                } else {
                    "non"
                }
            );
        }
        let Some(mut renderer) = self.renderer.take() else {
            return false;
        };
        // Reflex / Anti-Lag 2 : attente pilotée par le pilote avant la simulation.
        {
            let _r = minelib_render::perf::scope("Reflex / Anti-Lag (attente du pilote)");
            renderer.frame_begin();
        }
        let size = renderer.size();
        let sizef = (size.0 as f32, size.1 as f32);
        self.ui.clear();

        if self.keys.pressed(input, keys::Action::Debug)
            && !self.session.as_ref().is_some_and(|s| s.chat.open)
        {
            self.show_debug = !self.show_debug;
        }
        // F2 : capture d'écran (interface comprise), comme dans Minecraft, même tchat ouvert.
        if self.keys.pressed(input, keys::Action::Screenshot) {
            renderer.request_screenshot();
        }
        if let Some(shot) = renderer.take_screenshot() {
            self.save_screenshot(shot);
        }
        // Clic sur le nom d'une capture dans le tchat : image copiée dans le presse-papier.
        if let Some(path) = self.session.as_mut().and_then(|s| s.chat.clicked.take()) {
            let text = match copy_image_to_clipboard(&path) {
                Ok(()) => (
                    format!(
                        "Capture copiée dans le presse-papier : {}",
                        file_name(&path)
                    ),
                    chat::GREY,
                ),
                Err(e) => {
                    log::warn!("presse-papier : {e:#}");
                    (format!("Copie impossible : {e}"), chat::RED)
                }
            };
            if let Some(s) = &mut self.session {
                s.chat.say(text.0, text.1);
            }
        }

        // Échap : pause / reprise / fermeture de l'inventaire.
        if input.pressed(KeyCode::Escape) {
            match self.screen {
                Screen::Playing => match &mut self.session {
                    Some(s) if s.chat.open => s.chat.close(),
                    Some(s) if s.inventory.open => s.inventory.close(),
                    Some(s) if s.fps.buy_open => s.fps.buy_open = false,
                    _ => self.screen = Screen::Paused,
                },
                Screen::Paused => self.screen = Screen::Playing,
                _ => {}
            }
        }

        // Mémoire vidéo : relevé chaque seconde (bilan des 10 s), et plus de nouvelles pages
        // pour les chunks au-delà de 92 % du budget Windows (reprise sous 85 %).
        if let Some((mon, last, pressure)) = &mut self.vram {
            if last.elapsed().as_secs_f32() >= 1.0 {
                *last = std::time::Instant::now();
                if let Some((used, budget)) = mon.query() {
                    let mb = |b: u64| (b / (1024 * 1024)) as f32;
                    minelib_render::perf::gauge("mémoire vidéo utilisée (Mo)", mb(used));
                    minelib_render::perf::gauge("budget mémoire vidéo de Windows (Mo)", mb(budget));
                    let ratio = used as f64 / budget.max(1) as f64;
                    let next = if *pressure {
                        ratio > 0.85
                    } else {
                        ratio > 0.92
                    };
                    if next != *pressure {
                        *pressure = next;
                        renderer.set_vram_pressure(next);
                        if next {
                            log::warn!(
                                "mémoire vidéo presque pleine ({:.0} / {:.0} Mo) : plus de nouveaux chunks en mémoire vidéo",
                                mb(used),
                                mb(budget)
                            );
                        } else {
                            log::info!(
                                "mémoire vidéo de nouveau disponible ({:.0} / {:.0} Mo)",
                                mb(used),
                                mb(budget)
                            );
                        }
                    }
                }
            }
        }

        let in_world = self.session.is_some()
            && matches!(
                self.screen,
                Screen::Playing
                    | Screen::Paused
                    | Screen::Packs
                    | Screen::Settings
                    | Screen::Graphics
                    | Screen::Controls
            );
        if in_world {
            let active = self.screen == Screen::Playing;
            let frames = debug::frame_summary(&self.frame_times);
            let stats = self.last_stats;
            let sys = self.sysmon.latest();
            let fps_max = self.fps_limit();
            if let Some(s) = &mut self.session {
                s.update(input, dt, &mut renderer, active);
                let _ui = minelib_render::perf::scope("interface");
                if self.show_debug {
                    s.build_debug(frames, sys, &renderer, stats, fps_max);
                }
                if active {
                    s.draw_hud(&mut self.ui, sizef, input, self.show_debug);
                }
            }
            match self.screen {
                Screen::Paused => self.pause_menu(input, size),
                Screen::Packs => self.packs_screen(input, size, true),
                Screen::Settings => self.settings_screen(input, size, true),
                Screen::Graphics => self.graphics_screen(input, size, true, &mut renderer),
                Screen::Controls => self.controls_screen(input, size, true),
                _ => {}
            }
            self.draw_message(size, dt);
            if let Some(s) = &self.session {
                self.last_stats = s.render(
                    &mut renderer,
                    &self.ui,
                    self.show_debug,
                    self.time,
                    input.grabbed,
                );
            } else {
                renderer.render_ui(&self.ui);
            }
            if let Some(s) = &self.session {
                self.title_timer += dt;
                if self.title_timer >= 0.5 {
                    self.title_timer = 0.0;
                    let fps = debug::frame_summary(&self.frame_times).fps;
                    window.set_title(&format!("Minelib - {} - {fps:.0} FPS", s.meta.name));
                }
            }
        } else {
            match self.screen {
                Screen::Title => self.title_screen(input, size),
                Screen::Solo => self.solo_screen(input, size),
                Screen::NewWorld => self.new_world_screen(input, size),
                Screen::Packs => self.packs_screen(input, size, false),
                Screen::Settings => self.settings_screen(input, size, false),
                Screen::Graphics => self.graphics_screen(input, size, false, &mut renderer),
                Screen::Controls => self.controls_screen(input, size, false),
                Screen::Loading => self.loading_screen(input, size),
                Screen::Playing | Screen::Paused => self.screen = Screen::Title,
            }
            self.draw_message(size, dt);
            renderer.render_ui(&self.ui);
            window.set_title("Minelib");
        }
        // Conversion après avoir affiché au moins une image de l'écran d'attente.
        if self.screen == Screen::Loading {
            if let Some((dir, frames)) = self.pending_open.take() {
                if frames >= 2 {
                    self.play(dir);
                    if self.session.is_none() {
                        self.screen = Screen::Solo;
                    }
                } else {
                    self.pending_open = Some((dir, frames + 1));
                }
            } else {
                self.screen = Screen::Solo;
            }
        }
        if std::mem::take(&mut self.clear_meshes) {
            renderer.clear_meshes();
        }
        if let Some(info) = self.pack_request.take() {
            self.select_pack(&mut renderer, &info);
        }
        self.renderer = Some(renderer);
        self.limit_fps();
        minelib_render::perf::frame_end();
        !self.quit
    }

    fn ui_open(&self) -> bool {
        match self.screen {
            Screen::Playing => self
                .session
                .as_ref()
                .is_some_and(|s| s.inventory.open || s.chat.open || s.fps.buy_open),
            _ => true,
        }
    }

    fn on_exit(&mut self) {
        // Fermeture de la fenêtre en pleine partie : on enregistre quand même.
        if let Some(mut s) = self.session.take() {
            if let Err(e) = s.save() {
                log::error!("sauvegarde impossible : {e:#}");
            }
        }
        self.renderer = None; // libère le GPU avant la fermeture de la fenêtre
    }
}

/// Ouvre un dossier dans l'explorateur de fichiers du système.
fn open_folder(path: &std::path::Path) {
    #[cfg(windows)]
    let cmd = "explorer";
    #[cfg(target_os = "macos")]
    let cmd = "open";
    #[cfg(all(unix, not(target_os = "macos")))]
    let cmd = "xdg-open";
    if let Err(e) = std::process::Command::new(cmd).arg(path).spawn() {
        log::warn!("impossible d'ouvrir {} : {e}", path.display());
    }
}

/// Sensibilité affichée : jusqu'à 2 décimales, sans zéros inutiles (1.25, 2, 0.8).
fn format_sens(v: f32) -> String {
    let t = format!("{v:.2}");
    t.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn env_parse<T: std::str::FromStr>(name: &str) -> Option<T> {
    std::env::var(name).ok()?.trim().parse().ok()
}

/// Journal écrit à la fois dans la console, dans le journal du lancement et dans
/// `minelib.log` (dernier lancement), pour retrouver la cause d'un plantage.
struct Tee(Option<std::fs::File>, Option<std::fs::File>);

impl std::io::Write for Tee {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let _ = std::io::stderr().write_all(buf);
        for f in [&mut self.0, &mut self.1].into_iter().flatten() {
            let _ = f.write_all(buf);
        }
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        for f in [&mut self.0, &mut self.1].into_iter().flatten() {
            let _ = f.flush();
        }
        Ok(())
    }
}

/// Date et heure UTC lisibles (pour les noms de fichiers du journal).
fn file_name(p: &std::path::Path) -> String {
    p.file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
}

/// Copie une image (fichier PNG) dans le presse-papier.
fn copy_image_to_clipboard(path: &std::path::Path) -> anyhow::Result<()> {
    let img = image::open(path)?.to_rgba8();
    let (width, height) = img.dimensions();
    let mut cb = arboard::Clipboard::new()?;
    cb.set_image(arboard::ImageData {
        width: width as usize,
        height: height as usize,
        bytes: std::borrow::Cow::Owned(img.into_raw()),
    })?;
    Ok(())
}

fn stamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs()) as i64;
    let (days, rem) = (secs.div_euclid(86400), secs.rem_euclid(86400));
    // Jours depuis 1970 → date (algorithme civil de H. Hinnant, domaine public).
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + (m <= 2) as i64;
    format!(
        "{y:04}-{m:02}-{d:02}_{:02}-{:02}-{:02}",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// Garde les `keep` fichiers les plus récents de `dir` commençant par `prefix`.
fn prune(dir: &std::path::Path, prefix: &str, keep: usize) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<_> = rd
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with(prefix))
        .map(|e| e.path())
        .collect();
    files.sort();
    let n = files.len().saturating_sub(keep);
    for p in &files[..n] {
        let _ = std::fs::remove_file(p);
    }
}

/// Portables à deux cartes : demande aux pilotes NVIDIA (Optimus) et AMD (PowerXpress) de
/// lancer le jeu sur la carte dédiée (exportés par `build.rs`).
#[cfg(windows)]
#[no_mangle]
#[used]
pub static NvOptimusEnablement: u32 = 1;
#[cfg(windows)]
#[no_mangle]
#[used]
pub static AmdPowerXpressRequestHighPerformance: u32 = 1;

fn main() -> anyhow::Result<()> {
    // Processus de bilan des cartes graphiques (lancé par le jeu, voir `gpu_report`).
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).is_some_and(|a| a == "--bilan-gpu") {
        let logs = minelib_assets::data_dir().join("logs");
        let _ = std::fs::create_dir_all(&logs);
        let file = std::fs::File::create(logs.join("bilan_gpu.txt")).ok();
        env_logger::Builder::new()
            .filter_level(log::LevelFilter::Info)
            .filter_module("wgpu_core", log::LevelFilter::Error)
            .filter_module("wgpu_hal", log::LevelFilter::Error)
            .filter_module("naga", log::LevelFilter::Error)
            .target(env_logger::Target::Pipe(Box::new(Tee(file, None))))
            .init();
        gpu_report::run_process(args.get(2).map_or("", |s| s.as_str()));
        return Ok(());
    }
    // Journaux : `.minelib/logs/minelib_<date>.log` (un par lancement, 20 gardés),
    // plantages dans `.minelib/logs/crash_<date>.txt`, et `.minelib/minelib.log` = dernier.
    let dir = minelib_assets::data_dir();
    let logs = dir.join("logs");
    let _ = std::fs::create_dir_all(&logs);
    let t = stamp();
    let log_path = logs.join(format!("minelib_{t}.log"));
    let crash_path = logs.join(format!("crash_{t}.txt"));
    prune(&logs, "minelib_", 19);
    prune(&logs, "crash_", 19);
    let file = std::fs::File::create(&log_path).ok();
    let latest = std::fs::File::create(dir.join("minelib.log")).ok();
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("info,wgpu_core=warn,wgpu_hal=warn,naga=warn"),
    )
    .target(env_logger::Target::Pipe(Box::new(Tee(file, latest))))
    .init();
    log::info!(
        "Minelib {} — {} {} — journal : {}",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        log_path.display()
    );
    // Plantage Rust : message, endroit et pile d'appels, dans le journal et un rapport.
    {
        let crash_path = crash_path.clone();
        std::panic::set_hook(Box::new(move |info| {
            let bt = std::backtrace::Backtrace::force_capture();
            let thread = std::thread::current().name().unwrap_or("?").to_string();
            let msg = format!("PLANTAGE (thread {thread}) : {info}\n{bt}");
            log::error!("{msg}");
            let _ = std::fs::write(&crash_path, &msg);
        }));
    }
    // Thread du jeu et du rendu : priorité haute, il passe avant les tâches de fond.
    minelib_platform::raise_thread_priority();
    // Plantage hors Rust (pilote…) : rapport ajouté au journal.
    minelib_platform::install_native_crash_handler(log_path.clone());
    let result = Game::new().and_then(|g| minelib_platform::run("Minelib", g));
    match &result {
        Ok(()) => log::info!("fermeture normale"),
        Err(e) => {
            log::error!("erreur fatale : {e:?}");
            let _ = std::fs::write(&crash_path, format!("erreur fatale : {e:?}\n"));
        }
    }
    result
}
