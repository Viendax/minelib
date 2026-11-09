//! Une partie en cours : monde, joueur, inventaire, chargement des chunks, sauvegarde.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use anyhow::Result;
use glam::{Mat4, Vec3};
use minelib_platform::{Input, KeyCode, Mouse};
use minelib_render::{FrameParams, FrameStats, Overlay, Renderer, Ui};
use minelib_world::lod::LodSettings;
use minelib_world::raycast::{raycast, Hit};
use minelib_world::{BlockId, ChunkPos, World, CHUNK_SIZE};

use crate::chat::{self, Chat};
use crate::debug;
use crate::hand::Hand;
use crate::inventory::{self, Inventory, Item};
use crate::jobs::Workers;
use crate::keys::{Action, Bindings};
use crate::player::{MoveInput, Player};
use crate::save::{self, GameMode, GameState, PlayerState, SaveData, WorldMeta, MAX_HEALTH};
use crate::streaming::Streamer;

pub const REACH: f32 = 6.0;
/// Rotation par unité de souris pour une sensibilité de 1 : 0,022°, comme Counter-Strike
/// (« m_yaw » / « m_pitch »).
const CS_DEGREES_PER_COUNT: f32 = 0.022;
pub const FOV: f32 = 75.0;
const ZOOM_FOV: f32 = 18.0;
const AUTOSAVE_S: f32 = 300.0;
/// Deux appuis sur Espace en moins de ce délai : vol activé / coupé.
const DOUBLE_TAP_S: f32 = 0.3;
/// Chute sans dégâts (blocs).
/// Hauteur de chute sans dégâts (blocs) : au-delà, les dégâts commencent.
const SAFE_FALL: f32 = 4.0;
/// Durée d'affichage d'un ping, portée, nombre maximal à l'écran.
const PING_S: f32 = 8.0;
const PING_RANGE: f32 = 400.0;
const MAX_PINGS: usize = 3;

/// Réglages de rendu choisis au lancement.
#[derive(Clone, Copy)]
pub struct SessionConfig {
    pub view_distance: i32,
    pub lod_distance: f32,
    /// Rayon en pleine résolution (chunks), 0 = tout.
    pub full_detail: i32,
}

pub struct Session {
    pub dir: PathBuf,
    pub meta: WorldMeta,
    pub world: World,
    pub player: Player,
    workers: Workers,
    streamer: Streamer,
    view_distance: i32,
    lod_distance: f32,
    lod_enabled: bool,
    full_detail: i32,
    /// Rayon actuel du mode « Détail complet : auto ».
    auto_detail: i32,
    auto_timer: f32,
    dirty: HashSet<ChunkPos>,
    pub inventory: Inventory,
    night: bool,
    daylight: f32,
    break_cooldown: f32,
    place_cooldown: f32,
    pub fov: f32,
    /// Sensibilité de la souris (valeur Counter-Strike).
    pub sensitivity: f32,
    /// Touches choisies dans « Régler les touches ».
    pub keys: Bindings,
    autosave: f32,
    hit: Option<Hit>,
    overlay: Overlay,
    pub chat: Chat,
    /// Mode de jeu, vol autorisé en survie (/fly), santé.
    pub game: GameState,
    /// Point de réapparition (là où le monde a été ouvert).
    spawn: Vec3,
    /// Hauteur d'où le joueur tombe (pour les dégâts de chute).
    fall_from: Option<f32>,
    /// Temps depuis le dernier appui sur Espace (double appui = vol).
    since_space: f32,
    /// Voile rouge après une blessure (s).
    hurt: f32,
    /// Pings (mode FPS) : point visé et âge en secondes.
    pings: Vec<(Vec3, f32)>,
    /// Mode FPS : vie sur 100, armes, tir, menu d'achat.
    pub fps: crate::fps::Fps,
    /// Main et objet tenu (première personne), et leur éclairage.
    hand: Hand,
    hand_light: [f32; 3],
    /// Raytracing : coin de la grille envoyée au GPU, changements depuis, délai.
    vox_origin: Option<[i32; 3]>,
    vox_dirty: bool,
    vox_timer: f32,
    /// Grille du raytracing en cours de calcul (thread de fond).
    vox_job: Option<std::sync::mpsc::Receiver<VoxelGrid>>,
    lights_timer: f32,
    /// Faces visibles des miroirs autour du joueur (raytracing).
    mirror_planes: Vec<MirrorPlane>,
}

/// Distance de rendu maximale réglable (chunks).
pub const MAX_VIEW_DISTANCE: i32 = 256;

/// Distance de rendu que la mémoire vive permet : les chunks détaillés gardés en mémoire
/// (blocs, lumière…) prennent environ 200 Ko chacun ; on s'autorise 60 % de la mémoire
/// disponible au lancement du monde.
pub fn ram_view_limit() -> i32 {
    let (_, available) = minelib_platform::memory_info();
    if available == 0 {
        return MAX_VIEW_DISTANCE;
    }
    let budget = available as f64 * 0.6;
    let chunks = budget / (200.0 * 1024.0);
    // Zone gardée ≈ carré de (2r + 3) chunks de côté.
    let r = ((chunks.sqrt() - 3.0) / 2.0).floor() as i32;
    r.clamp(2, MAX_VIEW_DISTANCE)
}

/// Réglage « Détail complet » automatique : le rayon en pleine résolution s'ajuste au temps
/// GPU mesuré (plus de triangles quand la carte a de la marge, moins quand elle sature).
pub const DETAIL_AUTO: i32 = -1;
/// Bornes et cible du mode automatique.
const AUTO_MIN: i32 = 8;
const AUTO_MAX: i32 = 48;
/// Temps GPU de la scène visé par image (ms). L'anticrénelage et l'affichage (~1 ms, fixes)
/// ne comptent pas : ils ne dépendent pas du nombre de chunks.
const AUTO_TARGET_MS: f32 = 1.5;
/// Attente après un changement (s) : le chargement qu'il déclenche fausse la mesure.
const AUTO_SETTLE_S: f32 = 8.0;

/// Pleine résolution jusqu'à `detail` chunks (0 : jusqu'à la distance de rendu), bornée par
/// la mémoire vive (`ram_limit`, seuls les chunks détaillés en consomment), puis terrain
/// simplifié (LOD) jusqu'à la distance de rendu, ou plus loin si le rendu lointain va
/// au-delà. Sans rendu lointain ni rayon de détail plus court : tout en pleine résolution.
fn lod_settings(
    view_distance: i32,
    ram_limit: i32,
    lod_distance: f32,
    lod_enabled: bool,
    detail: i32,
) -> LodSettings {
    let wanted = if detail > 0 { detail } else { view_distance };
    let detail_chunks = view_distance.min(wanted).min(ram_limit);
    let view = (view_distance * CHUNK_SIZE as i32) as f32;
    let detail = (detail_chunks * CHUNK_SIZE as i32) as f32;
    let max_distance = if lod_enabled {
        lod_distance.max(view)
    } else if detail < view {
        view
    } else {
        0.0
    };
    LodSettings {
        detail,
        max_distance,
    }
}

impl Session {
    /// Ouvre un monde enregistré.
    pub fn open(dir: PathBuf, cfg: SessionConfig) -> Result<Self> {
        let (data, mut world) = save::load_world(&dir)?;
        let (mut player, fresh) = match data.player {
            Some(p) => {
                let mut pl = Player::new(Vec3::from(p.pos));
                pl.yaw = p.yaw;
                pl.pitch = p.pitch;
                pl.flying = p.flying;
                (pl, false)
            }
            None => (Player::new(Vec3::new(8.5, 0.0, 8.5)), true),
        };
        if let Ok(start) = std::env::var("MINELIB_START") {
            let v: Vec<f32> = start
                .split(',')
                .filter_map(|s| s.trim().parse().ok())
                .collect();
            if v.len() >= 3 {
                player.pos = Vec3::new(v[0], v[1], v[2]);
                player.flying = true;
            }
            if v.len() >= 5 {
                player.yaw = v[3].to_radians();
                player.pitch = v[4].to_radians();
            }
        }
        // Chunks autour du joueur générés tout de suite (sol sous ses pieds).
        let c = ChunkPos::containing(player.pos.x.floor() as i32, player.pos.z.floor() as i32);
        for dz in -1..=1 {
            for dx in -1..=1 {
                world.ensure_chunk(ChunkPos {
                    x: c.x + dx,
                    z: c.z + dz,
                });
            }
        }
        if fresh {
            let ground = world.top_solid(8, 8).unwrap_or(64);
            player.pos.y = ground as f32 + 1.0;
            if std::env::var("MINELIB_DEMO").is_ok_and(|v| v.trim() == "1") {
                build_demo(&mut world);
                player.pos = Vec3::new(8.5, world.top_solid(8, 6).unwrap_or(64) as f32 + 1.0, 6.5);
                player.yaw = std::f32::consts::PI;
                player.pitch = -0.12;
            }
        }
        let game = data.game;
        if game.mode.mortal() && !game.can_fly {
            player.flying = false;
        }
        let spawn = player.pos;
        let night = data.night || std::env::var("MINELIB_NIGHT").is_ok_and(|v| v.trim() == "1");
        let lod_enabled = cfg.lod_distance > 0.0;
        let auto_detail = 24;
        let first = lod_settings(
            cfg.view_distance,
            ram_view_limit(),
            cfg.lod_distance,
            lod_enabled,
            if cfg.full_detail == DETAIL_AUTO {
                auto_detail
            } else {
                cfg.full_detail
            },
        );
        if first.enabled() {
            log::info!(
                "pleine résolution jusqu'à {:.0} chunks, terrain simplifié (LOD) jusqu'à {:.0} chunks",
                first.detail / CHUNK_SIZE as f32,
                first.max_distance / CHUNK_SIZE as f32
            );
        }
        log::info!("monde « {} » (graine {})", data.meta.name, data.meta.seed);
        Ok(Self {
            dir,
            workers: Workers::new(data.meta.seed, world.surface_map(), world.generator),
            streamer: Streamer::new(first),
            meta: data.meta,
            world,
            player,
            view_distance: cfg.view_distance,
            lod_distance: cfg.lod_distance.max(0.0),
            lod_enabled,
            full_detail: cfg.full_detail,
            auto_detail,
            auto_timer: 3.0 - AUTO_SETTLE_S,
            dirty: HashSet::new(),
            inventory: data.inventory.unwrap_or_else(Inventory::starter),
            night,
            daylight: if night { 0.0 } else { 1.0 },
            break_cooldown: 0.0,
            place_cooldown: 0.0,
            fov: FOV,
            sensitivity: minelib_assets::DEFAULT_SENSITIVITY,
            keys: Bindings::default(),
            autosave: AUTOSAVE_S,
            hit: None,
            overlay: Overlay::default(),
            chat: Chat::default(),
            game,
            spawn,
            fall_from: None,
            since_space: 1.0,
            hurt: 0.0,
            fps: crate::fps::Fps::new(),
            pings: Vec::new(),
            hand: Hand::default(),
            hand_light: [1.0; 3],
            vox_origin: None,
            vox_dirty: false,
            vox_timer: 0.0,
            vox_job: None,
            lights_timer: 0.0,
            mirror_planes: Vec::new(),
        })
    }

    pub fn save(&mut self) -> Result<()> {
        self.meta.last_played = save::now();
        let mut inv = Inventory::empty();
        inv.slots = self.inventory.slots;
        inv.selected = self.inventory.selected;
        let data = SaveData {
            game: self.game,
            generator: self.world.generator,
            meta: self.meta.clone(),
            player: Some(PlayerState {
                pos: self.player.pos.into(),
                yaw: self.player.yaw,
                pitch: self.player.pitch,
                flying: self.player.flying,
            }),
            clock: self.world.clock,
            night: self.night,
            inventory: Some(inv),
        };
        save::save_world(&self.dir, &data, &self.world)
    }

    /// Nouveaux réglages graphiques (écran Paramètres), appliqués tout de suite.
    pub fn apply_config(&mut self, cfg: SessionConfig) {
        self.view_distance = cfg.view_distance.clamp(2, MAX_VIEW_DISTANCE);
        self.full_detail = cfg.full_detail;
        self.lod_enabled = cfg.lod_distance > 0.0;
        if self.lod_enabled {
            self.lod_distance = cfg.lod_distance;
        }
        self.settings_changed();
    }

    pub fn config(&self) -> SessionConfig {
        SessionConfig {
            view_distance: self.view_distance,
            lod_distance: if self.lod_enabled {
                self.lod_distance
            } else {
                0.0
            },
            full_detail: self.full_detail,
        }
    }

    /// Rayon en pleine résolution actuel (mode auto compris).
    pub fn detail_radius(&self) -> i32 {
        (self.streamer.settings.detail / CHUNK_SIZE as f32) as i32
    }

    /// Mode « Détail complet : auto » : toutes les 3 s, compare le temps GPU moyen à la cible
    /// et ajuste le rayon en pleine résolution (pas de 4 chunks, entre 8 et 48). Le nombre
    /// de triangles suit à peu près le carré du rayon.
    fn auto_detail_tick(&mut self, dt: f32) {
        if self.full_detail != DETAIL_AUTO {
            return;
        }
        self.auto_timer += dt;
        if self.auto_timer < 3.0 {
            return;
        }
        self.auto_timer = 0.0;
        let Some(gpu) = minelib_render::perf::gpu_scene_recent() else {
            return;
        };
        let cap = AUTO_MAX.min(self.view_distance);
        // Pendant un chargement, les envois au GPU gonflent la mesure : on ne réduit alors
        // que si la carte est vraiment saturée (2 × la cible), et on n'élargit jamais.
        let loading = !self.streamer.idle();
        let too_slow = if loading {
            AUTO_TARGET_MS * 2.0
        } else {
            AUTO_TARGET_MS * 1.2
        };
        let next = if gpu > too_slow && self.auto_detail > AUTO_MIN {
            (self.auto_detail - 4).max(AUTO_MIN)
        } else if gpu < AUTO_TARGET_MS * 0.6 && self.auto_detail < cap && !loading {
            (self.auto_detail + 4).min(cap)
        } else {
            self.auto_detail
        };
        if next != self.auto_detail {
            log::info!(
                "détail complet auto : {} → {next} chunks (scène {gpu:.2} ms GPU par image, cible {AUTO_TARGET_MS} ms)",
                self.auto_detail
            );
            self.auto_detail = next;
            self.auto_timer = 3.0 - AUTO_SETTLE_S;
            self.settings_changed();
        }
    }

    fn settings_changed(&mut self) {
        // Garde-fou : jamais plus de chunks que la mémoire vive ne peut en contenir.
        let limit = ram_view_limit();
        let detail = if self.full_detail == DETAIL_AUTO {
            self.auto_detail
        } else {
            self.full_detail
        };
        let s = lod_settings(
            self.view_distance,
            limit,
            self.lod_distance,
            self.lod_enabled,
            detail,
        );
        let detailed = (s.detail / CHUNK_SIZE as f32) as i32;
        if detailed == limit
            && limit
                < self
                    .view_distance
                    .min(if detail > 0 { detail } else { i32::MAX })
        {
            log::warn!("pleine résolution limitée à {limit} chunks par la mémoire vive disponible");
        }
        log::info!(
            "distance de rendu : {} chunks ({} chunks détaillés environ)",
            self.view_distance,
            (2 * detailed + 1).pow(2)
        );
        if s.enabled() {
            log::info!(
                "pleine résolution jusqu'à {:.0} chunks, terrain simplifié (LOD) jusqu'à {:.0} chunks",
                s.detail / CHUNK_SIZE as f32,
                s.max_distance / CHUNK_SIZE as f32
            );
        }
        self.streamer.set_settings(s);
    }

    fn fog_end(&self) -> f32 {
        let s = &self.streamer.settings;
        if s.enabled() {
            s.max_distance
        } else {
            s.detail
        }
    }

    /// Une image de jeu. `active` : faux quand le jeu est en pause (temps figé, pas d'entrées).
    pub fn update(&mut self, input: &Input, dt: f32, renderer: &mut Renderer, active: bool) {
        use minelib_render::perf;
        let logic = perf::scope("jeu : logique et physique");
        self.auto_detail_tick(dt);
        crate::jobs::throttle(perf::last_unmeasured_ms(), !self.streamer.idle(), dt);
        if active {
            // Tchat : saisie, puis ouverture par `t` ou `/` (jamais par-dessus l'inventaire).
            let was_open = self.chat.open;
            let mut sent = self.chat.update(input, dt);
            if !was_open && !self.inventory.open {
                let by_chat = self.keys.pressed(input, Action::Chat);
                let by_command = self.keys.pressed(input, Action::Command);
                sent = sent.or_else(|| self.chat.try_open(input, by_chat, by_command));
            }
            // Image où le tchat était ouvert, s'est ouvert ou a été envoyé : les touches
            // tapées sont du texte, pas des commandes de jeu.
            let typing = was_open || self.chat.open || sent.is_some();
            if let Some(text) = sent {
                self.submit(&text);
            }
            self.hurt = (self.hurt - dt).max(0.0);
            for p in &mut self.pings {
                p.1 += dt;
            }
            self.pings.retain(|p| p.1 < PING_S);
            self.since_space += dt;
            self.inventory.creative = self.game.mode == GameMode::Creative;
            if self.inventory.open && self.inventory.creative && !typing {
                // Catalogue créatif ouvert : le clavier sert à la recherche ; E ferme
                // seulement quand la recherche est vide (sinon c'est une lettre).
                if self.keys.pressed(input, Action::Inventory) && self.inventory.search.is_empty() {
                    self.inventory.close();
                } else {
                    self.inventory.search_input(
                        &input.text,
                        input.pressed(KeyCode::Backspace),
                        input.scroll,
                    );
                }
            } else if !typing {
                self.handle_keys(input);
                self.handle_hotbar(input);
            }
            self.inventory.update(dt);
            self.tick_torches(dt);
            let target_day = if self.night { 0.0 } else { 1.0 };
            self.daylight += (target_day - self.daylight) * (dt * 1.5).min(1.0);

            let k = &self.keys;
            let zooming = !typing && k.held(input, Action::Zoom) && !self.inventory.open;
            let sprinting =
                !typing && k.held(input, Action::Sprint) && self.player.vel.length() > 5.0;
            let target_fov = if zooming {
                ZOOM_FOV
            } else if sprinting {
                FOV * 1.1
            } else {
                FOV
            };
            self.fov += (target_fov - self.fov) * (dt * 14.0).min(1.0);

            if input.grabbed {
                let (dx, dy) = input.mouse_delta;
                let k = (CS_DEGREES_PER_COUNT * self.sensitivity).to_radians() * (self.fov / FOV);
                self.player.rotate(dx as f32 * k, dy as f32 * k);
            }
            let keys = &self.keys;
            let axis = |pos: Action, neg: Action| {
                keys.held(input, pos) as i32 as f32 - keys.held(input, neg) as i32 as f32
            };
            let mut m = MoveInput {
                forward: axis(Action::Forward, Action::Back),
                strafe: axis(Action::Right, Action::Left),
                up: axis(Action::Jump, Action::Sneak),
                jump: keys.held(input, Action::Jump),
                sprint: keys.held(input, Action::Sprint),
                sneak: keys.held(input, Action::Sneak),
                fps: self.game.mode == GameMode::Fps,
            };
            if self.inventory.open || typing {
                m = MoveInput::default();
            }
            // Spectateur : vol forcé, à travers les blocs.
            let spectator = self.game.mode == GameMode::Spectator;
            self.player.noclip = spectator;
            if spectator {
                self.player.flying = true;
            }
            let here = ChunkPos::containing(
                self.player.pos.x.floor() as i32,
                self.player.pos.z.floor() as i32,
            );
            if self.player.flying || self.world.contains(here) {
                self.player.update(&self.world, &m, dt);
            }
            self.check_fall();
            if self.game.mode == GameMode::Fps {
                self.update_fps(input, dt, renderer, typing);
            } else {
                self.fps.buy_open = false;
            }
            let held = self.held_item();
            let speed = if self.player.on_ground && !self.player.flying {
                Vec3::new(self.player.vel.x, 0.0, self.player.vel.z).length()
            } else {
                0.0
            };
            let swung = input.grabbed
                && !typing
                && !self.inventory.open
                && (self.keys.pressed(input, Action::Attack)
                    || self.keys.pressed(input, Action::Use));
            self.hand.update(dt, speed, swung, held);
            self.hand_light = self.light_at_player(held);
            self.handle_blocks(input, dt);
            if self.inventory.open {
                let (w, h) = renderer.size();
                self.inventory.pointer(
                    (w as f32, h as f32),
                    input.cursor,
                    input.mouse_pressed(Mouse::Left),
                    input.mouse_held(Mouse::Left),
                );
                if let Some(c) = input.cursor {
                    for (button, right) in [(Mouse::Left, false), (Mouse::Right, true)] {
                        if input.mouse_pressed(button) {
                            let shift =
                                input.held(KeyCode::ShiftLeft) || input.held(KeyCode::ShiftRight);
                            self.inventory.click((w as f32, h as f32), c, right, shift);
                        }
                    }
                }
            }
            self.autosave -= dt;
            if self.autosave <= 0.0 {
                self.autosave = AUTOSAVE_S;
                let _s = perf::scope("sauvegarde automatique");
                perf::event(|| "sauvegarde automatique".into());
                if let Err(e) = self.save() {
                    log::error!("sauvegarde automatique impossible : {e:#}");
                }
            }
        }

        drop(logic);
        if !self.dirty.is_empty() {
            self.vox_dirty = true;
            let n = self.dirty.len();
            perf::event(|| format!("{n} chunks à remailler (bloc ou lumière modifiés)"));
        }
        for pos in std::mem::take(&mut self.dirty) {
            self.streamer
                .remesh_now(&self.world, &mut self.workers, pos);
        }
        {
            let _s = perf::scope("grille raytracing");
            self.update_voxels(renderer, dt);
        }
        {
            let _s = perf::scope("lumières proches");
            self.update_lights(renderer, dt);
        }
        self.streamer.update(
            &mut self.world,
            renderer,
            &mut self.workers,
            self.player.pos.x,
            self.player.pos.z,
        );
        let _s = perf::scope("visée");
        self.hit = raycast(
            &self.world,
            self.player.eye().into(),
            self.player.look_dir().into(),
            REACH,
        );
    }

    fn handle_keys(&mut self, input: &Input) {
        // Mode FPS : pas d'inventaire.
        if self.keys.pressed(input, Action::Inventory) && self.game.mode.has_inventory() {
            self.inventory.toggle();
        }
        if self.keys.pressed(input, Action::DayNight) {
            self.night = !self.night;
        }
        // Double appui sur Espace : vol (toujours en créatif, en survie après /fly).
        if self.keys.pressed(input, Action::Jump) && !self.inventory.open {
            if self.since_space < DOUBLE_TAP_S && self.fly_allowed() {
                self.player.flying = !self.player.flying;
                self.player.vel = Vec3::ZERO;
                self.since_space = 1.0; // pas de triple appui
            } else {
                self.since_space = 0.0;
            }
        }
        if self.keys.pressed(input, Action::Lod) {
            self.lod_enabled = !self.lod_enabled;
            if self.lod_distance <= 0.0 {
                self.lod_distance = 4096.0;
            }
            self.settings_changed();
        }
        let mut dv = 0;
        if input.pressed(KeyCode::PageUp) {
            dv += 2;
        }
        if input.pressed(KeyCode::PageDown) {
            dv -= 2;
        }
        if dv != 0 {
            self.view_distance = (self.view_distance + dv).clamp(2, MAX_VIEW_DISTANCE);
            self.settings_changed();
        }
    }

    fn handle_hotbar(&mut self, input: &Input) {
        let keys = [
            KeyCode::Digit1,
            KeyCode::Digit2,
            KeyCode::Digit3,
            KeyCode::Digit4,
            KeyCode::Digit5,
            KeyCode::Digit6,
            KeyCode::Digit7,
            KeyCode::Digit8,
            KeyCode::Digit9,
            KeyCode::Digit0,
        ];
        for (i, key) in keys.into_iter().enumerate() {
            if input.pressed(key) {
                self.inventory.select(i);
            }
        }
        if !self.inventory.open && input.scroll.abs() >= 1.0 {
            self.inventory.scroll(-input.scroll.signum() as i32);
        }
    }

    fn handle_blocks(&mut self, input: &Input, dt: f32) {
        self.break_cooldown -= dt;
        self.place_cooldown -= dt;
        if !input.grabbed || self.inventory.open {
            return;
        }
        let keys = &self.keys;
        let pick = keys.pressed(input, Action::Pick);
        // Spectateur : on regarde seulement.
        if self.game.mode == GameMode::Spectator {
            return;
        }
        // Mode FPS : ni cassé ni posé ; la touche « Copier / ping » marque l'endroit visé.
        if self.game.mode == GameMode::Fps {
            if pick {
                let eye = self.player.eye();
                let dir = self.player.look_dir();
                if let Some(h) = raycast(&self.world, eye.into(), dir.into(), PING_RANGE) {
                    if self.pings.len() >= MAX_PINGS {
                        self.pings.remove(0);
                    }
                    self.pings.push((eye + dir * h.distance, 0.0));
                }
            }
            return;
        }
        let breaking = keys.pressed(input, Action::Attack)
            || (keys.held(input, Action::Attack) && self.break_cooldown <= 0.0);
        let placing = keys.pressed(input, Action::Use)
            || (keys.held(input, Action::Use) && self.place_cooldown <= 0.0);
        let hit = raycast(
            &self.world,
            self.player.eye().into(),
            self.player.look_dir().into(),
            REACH,
        );
        let Some(hit) = hit else { return };

        if breaking && hit.id.is_breakable() {
            let [x, y, z] = hit.block;
            // Une torche cassée rend une torche ordinaire : le sel restant est perdu.
            let item = if hit.id.is_torch() {
                Item::Torch(None)
            } else {
                Item::Block(hit.id)
            };
            self.dirty.extend(self.world.set(x, y, z, BlockId::AIR));
            self.inventory.add(item, 1);
            self.break_cooldown = 0.22;
        }

        if placing {
            let [x, y, z] = hit.adjacent();
            let free = !self.world.get(x, y, z).is_targetable();
            match self.inventory.selected_item() {
                Some(Item::Block(b)) if free && !self.player.overlaps_block([x, y, z]) => {
                    self.dirty.extend(self.world.set(x, y, z, b));
                    self.inventory.take_selected();
                }
                // Torche : au sol sur le dessus d'un bloc, murale contre un côté, pas au plafond.
                Some(Item::Torch(chem)) if free && hit.normal[1] >= 0 => {
                    let block = match hit.normal {
                        [-1, 0, 0] => BlockId::wall_torch(0),
                        [1, 0, 0] => BlockId::wall_torch(1),
                        [0, 0, -1] => BlockId::wall_torch(4),
                        [0, 0, 1] => BlockId::wall_torch(5),
                        _ => BlockId::TORCH,
                    };
                    self.dirty
                        .extend(self.world.place_torch_block(x, y, z, block, chem));
                    // En créatif, une torche colorée ne s'épuise jamais.
                    if self.game.mode == GameMode::Creative && chem.is_some() {
                        self.world.set_torch_infinite([x, y, z]);
                    }
                    self.inventory.take_selected();
                }
                _ => {}
            }
            self.place_cooldown = 0.22;
        }
        // Copier le bloc visé : il passe dans la main. En créatif il est créé s'il n'est pas
        // dans la barre ; en survie on ne fait que choisir un emplacement qui l'a déjà.
        if pick {
            let wanted = if hit.id.is_torch() {
                Item::Torch(self.world.torch(hit.block).and_then(|t| t.chemical))
            } else {
                Item::Block(hit.id)
            };
            if let Some(i) = self.inventory.slots[..inventory::HOTBAR]
                .iter()
                .position(|s| s.is_some_and(|s| s.item == wanted))
            {
                self.inventory.select(i);
            } else if self.game.mode == GameMode::Creative {
                // Emplacement vide de la barre s'il y en a un, sinon celui en main.
                let slot = self.inventory.slots[..inventory::HOTBAR]
                    .iter()
                    .position(|s| s.is_none())
                    .unwrap_or(self.inventory.selected);
                self.inventory.slots[slot] = Some(inventory::Stack {
                    item: wanted,
                    count: 64,
                });
                self.inventory.select(slot);
            }
        }
    }

    /// Sources de lumière proches (flammes des torches, blocs lumineux), pour leurs reflets
    /// sur les surfaces brillantes ; mises à jour 10 fois par seconde.
    fn update_lights(&mut self, renderer: &mut Renderer, dt: f32) {
        self.lights_timer -= dt;
        if self.lights_timer > 0.0 {
            return;
        }
        self.lights_timer = 0.1;
        let lights = collect_lights(&self.world, self.player.eye());
        let planes: &[MirrorPlane] = if renderer.raytracing() {
            &self.mirror_planes
        } else {
            &[]
        };
        renderer.set_mirror_lights(&mirror_lights(&lights, planes, self.player.eye()));
        renderer.set_lights(&lights);
    }

    /// Raytracing : grille de blocs autour du joueur (128 × 64 × 128), refaite quand il
    /// s'est déplacé de 8 blocs ou qu'un bloc a changé (au plus 4 fois par seconde).
    fn update_voxels(&mut self, renderer: &mut Renderer, dt: f32) {
        use minelib_render::rt::VOX;
        if !renderer.raytracing() {
            self.vox_origin = None;
            self.vox_job = None;
            self.mirror_planes.clear();
            return;
        }
        // Grille calculée en arrière-plan : on l'envoie au GPU dès qu'elle est prête.
        if let Some(rx) = &self.vox_job {
            match rx.try_recv() {
                Ok(grid) => {
                    let _p = minelib_render::perf::scope("grille raytracing (envoi)");
                    renderer.set_voxels(grid.origin, &grid.ids);
                    self.mirror_planes = grid.planes;
                    self.lights_timer = 0.0;
                    self.vox_job = None;
                    minelib_render::perf::event(|| {
                        format!(
                            "grille du raytracing reçue ({:.1} ms en arrière-plan)",
                            grid.ms
                        )
                    });
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => return,
                Err(_) => self.vox_job = None,
            }
        }
        self.vox_timer -= dt;
        let e = self.player.eye();
        let snap = |v: f32, half: i32| ((v.floor() as i32 - half).div_euclid(8)) * 8;
        let origin = [
            snap(e.x, VOX[0] as i32 / 2),
            snap(e.y, VOX[1] as i32 / 2),
            snap(e.z, VOX[2] as i32 / 2),
        ];
        let moved = self.vox_origin != Some(origin);
        if !moved && !(self.vox_dirty && self.vox_timer <= 0.0) {
            return;
        }
        // Copie légère (pointeurs partagés) des chunks couverts par la grille.
        let c0 = ChunkPos::containing(origin[0], origin[2]);
        let c1 = ChunkPos::containing(origin[0] + VOX[0] as i32 - 1, origin[2] + VOX[2] as i32 - 1);
        let mut chunks = HashMap::new();
        for cz in c0.z..=c1.z {
            for cx in c0.x..=c1.x {
                let p = ChunkPos { x: cx, z: cz };
                if let Some(c) = self.world.chunk_arc(p) {
                    chunks.insert(p, c);
                }
            }
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("minelib-raytracing".into())
            .spawn(move || {
                minelib_platform::lower_thread_priority();
                let t = std::time::Instant::now();
                let (ids, planes) = build_voxel_grid(&chunks, origin);
                let _ = tx.send(VoxelGrid {
                    origin,
                    ids,
                    planes,
                    ms: t.elapsed().as_secs_f32() * 1000.0,
                });
            });
        if spawned.is_ok() {
            self.vox_job = Some(rx);
        }
        self.vox_origin = Some(origin);
        self.vox_dirty = false;
        self.vox_timer = 0.25;
    }

    /// Objet en main (aucun en mode FPS : pas d'inventaire).
    fn held_item(&self) -> Option<Item> {
        self.game
            .mode
            .has_inventory()
            .then(|| self.inventory.selected_item())
            .flatten()
    }

    /// Éclairage approximatif de la main : ciel (abrité ou non), jour/nuit, torche tenue.
    fn light_at_player(&self, held: Option<Item>) -> [f32; 3] {
        let e = self.player.eye();
        let covered = self
            .world
            .top_solid(e.x.floor() as i32, e.z.floor() as i32)
            .is_some_and(|t| t as f32 > e.y);
        let sky = if covered { 0.35 } else { 1.0 } * (0.18 + 0.82 * self.daylight);
        let mut l = [sky; 3];
        if let Some(Item::Torch(_)) = held {
            for (c, w) in l.iter_mut().zip([1.0, 0.78, 0.5]) {
                *c = c.max(w);
            }
        }
        l
    }

    fn fly_allowed(&self) -> bool {
        (self.game.mode == GameMode::Creative || self.game.can_fly)
            && self.game.mode != GameMode::Spectator
    }

    /// Dégâts de chute (survie) : un demi-cœur par bloc au-delà de 3.
    fn check_fall(&mut self) {
        let p = &self.player;
        if self.game.mode.mortal() && p.pos.y < -40.0 {
            self.die();
            return;
        }
        if !p.flying && !p.on_ground {
            let y = p.pos.y;
            self.fall_from = Some(self.fall_from.map_or(y, |f| f.max(y)));
        } else if let Some(from) = self.fall_from.take() {
            let height = from - p.pos.y;
            if self.game.mode == GameMode::Fps {
                // Mode FPS : 100 points de vie, dégâts selon la vitesse d'impact.
                let dmg = crate::fps::Fps::fall_damage(height, SAFE_FALL);
                if !p.flying && dmg > 0 {
                    self.fps.hp = self.fps.hp.saturating_sub(dmg);
                    self.hurt = 0.35;
                    if self.fps.hp == 0 {
                        self.die();
                    }
                }
            } else if !p.flying && height > SAFE_FALL {
                self.damage((height - SAFE_FALL).ceil() as u8);
            }
        }
    }

    fn damage(&mut self, amount: u8) {
        if !self.game.mode.mortal() || amount == 0 {
            return;
        }
        self.game.health = self.game.health.saturating_sub(amount);
        self.hurt = 0.35;
        if self.game.health == 0 {
            self.die();
        }
    }

    fn die(&mut self) {
        self.chat
            .say("Vous êtes mort ! Retour au point d'apparition.", chat::RED);
        self.player.pos = self.spawn;
        // Point d'apparition en l'air (monde enregistré en volant) : on réapparaît posé sur
        // le sol en dessous, sinon on retomberait et on mourrait en boucle.
        let (x, z) = (self.spawn.x.floor() as i32, self.spawn.z.floor() as i32);
        if let Some(top) = self.world.top_solid(x, z) {
            if (top as f32 + 1.0) < self.spawn.y {
                self.player.pos.y = top as f32 + 1.0;
            }
        }
        self.player.flying = false;
        self.player.vel = Vec3::ZERO;
        self.game.health = MAX_HEALTH;
        self.fall_from = None;
        self.fps.respawn();
    }

    /// Mode FPS, à chaque image : menu d'achat, tir, rechargement, choix de l'arme.
    fn update_fps(&mut self, input: &Input, dt: f32, renderer: &Renderer, typing: bool) {
        let (w, h) = renderer.size();
        let size = (w as f32, h as f32);
        if !typing && self.keys.pressed(input, Action::Buy) {
            self.fps.buy_open = !self.fps.buy_open;
        } else {
            self.fps.buy_input(input, size);
        }
        let free = input.grabbed && !typing && !self.fps.buy_open;
        let k = &self.keys;
        let c = crate::fps::Controls {
            trigger_pressed: free && k.pressed(input, Action::Attack),
            trigger_held: free && k.held(input, Action::Attack),
            reload: free && k.pressed(input, Action::Reload),
            scope: free && k.pressed(input, Action::Use),
            slot: if !free {
                None
            } else if input.pressed(KeyCode::Digit1) {
                Some(0)
            } else if input.pressed(KeyCode::Digit2) {
                Some(1)
            } else {
                None
            },
            speed: Vec3::new(self.player.vel.x, 0.0, self.player.vel.z).length(),
            on_ground: self.player.on_ground || self.player.flying,
        };
        let eye = self.player.eye();
        self.fps
            .update(dt, &c, &self.world, eye, self.player.yaw, self.player.pitch);
        self.fps.tick_effects(dt);
    }

    /// Vue du joueur avec le recul du mode FPS (la vue remonte pendant une rafale).
    fn view_with_punch(&self) -> Mat4 {
        if self.game.mode != GameMode::Fps {
            return self.player.view();
        }
        let p = (self.player.pitch + self.fps.punch.0.to_radians()).clamp(-1.55, 1.55);
        let y = self.player.yaw + self.fps.punch.1.to_radians();
        let dir = Vec3::new(y.sin() * p.cos(), p.sin(), -y.cos() * p.cos());
        Mat4::look_to_rh(self.player.eye(), dir, Vec3::Y)
    }

    /// Message ou commande envoyé dans le tchat.
    fn submit(&mut self, text: &str) {
        let Some(cmd) = text.strip_prefix('/') else {
            self.chat.say(format!("<Joueur> {text}"), chat::WHITE);
            return;
        };
        let mut parts = cmd.split_whitespace();
        let name = parts.next().unwrap_or("").to_lowercase();
        let arg = parts.next();
        match name.as_str() {
            "fly" => {
                if self.game.mode == GameMode::Creative {
                    self.chat.say(
                        "En créatif le vol est toujours permis : appuyez deux fois sur Espace.",
                        chat::GREY,
                    );
                } else {
                    self.game.can_fly = !self.game.can_fly;
                    if !self.game.can_fly && self.player.flying {
                        self.player.flying = false;
                    }
                    self.chat.say(
                        if self.game.can_fly {
                            "Vol autorisé : appuyez deux fois sur Espace pour voler."
                        } else {
                            "Vol désactivé."
                        },
                        chat::YELLOW,
                    );
                }
            }
            "gamemode" | "gm" => match arg.and_then(GameMode::parse) {
                Some(mode) => {
                    self.game.mode = mode;
                    if mode.mortal() && !self.game.can_fly {
                        self.player.flying = false;
                    }
                    if mode == GameMode::Creative {
                        self.game.health = MAX_HEALTH;
                    }
                    if !mode.has_inventory() {
                        self.inventory.close();
                    }
                    if mode == GameMode::Spectator {
                        self.player.flying = true;
                    } else if self.player.noclip {
                        // Sortie du mode spectateur : vol gardé si permis, sinon on tombe.
                        self.player.noclip = false;
                        self.player.flying = mode == GameMode::Creative || self.game.can_fly;
                    }
                    self.fall_from = None;
                    self.chat
                        .say(format!("Mode de jeu : {}", mode.label()), chat::YELLOW);
                }
                None => self.chat.say(
                    "Utilisation : /gm <0|1|2|3|survival|creative|fps|spectator>",
                    chat::RED,
                ),
            },
            "give" => {
                if !self.game.mode.has_inventory() {
                    self.chat
                        .say("Pas d'inventaire dans ce mode de jeu.", chat::RED);
                    return;
                }
                let reg = minelib_world::registry::registry();
                let want = arg.unwrap_or("").to_lowercase();
                let count: u32 = parts
                    .next()
                    .and_then(|n| n.parse().ok())
                    .unwrap_or(64)
                    .clamp(1, 64);
                let found = reg.by_key(&want).or_else(|| {
                    // Sinon par le nom affiché (« miroir », « bloc de fer »…).
                    (1..reg.len())
                        .map(|i| BlockId(i as u16))
                        .find(|b| b.def().name.to_lowercase() == want)
                });
                match found {
                    _ if want == "torch" || want == "torche" => {
                        self.inventory.add(Item::Torch(None), count);
                        self.chat
                            .say(format!("{count} torche(s) données."), chat::YELLOW);
                    }
                    Some(b) if b != BlockId::AIR => {
                        self.inventory.add(Item::Block(b), count);
                        self.chat.say(
                            format!("{count} × {} donné(s).", b.def().name),
                            chat::YELLOW,
                        );
                    }
                    _ => self.chat.say(
                        format!("Bloc inconnu : {want} (ex. /give mirror, /give iron_block)"),
                        chat::RED,
                    ),
                }
            }
            "heal" => {
                self.game.health = MAX_HEALTH;
                self.chat.say("Vous êtes soigné.", chat::YELLOW);
            }
            "help" | "aide" | "?" => {
                for l in [
                    "/gm (ou /gamemode) <0|1|2|3> : survie, créatif, fps, spectateur",
                    "/fly : autorise ou interdit le vol en survie",
                    "/heal : rend tous les coeurs",
                    "/give <bloc> [nombre] : ex. /give mirror, /give iron_block",
                    "Vol : deux appuis rapides sur Espace",
                    "Mode FPS : clic molette = ping (pas d'inventaire ni de blocs)",
                    "Spectateur (/gm 3) : vol forcé, on traverse les blocs",
                ] {
                    self.chat.say(l, chat::GREY);
                }
            }
            _ => self.chat.say(
                format!("Commande inconnue : /{name}. Tapez /help."),
                chat::RED,
            ),
        }
    }

    /// Avance l'horloge du monde ; les torches dont le sel est épuisé repassent en flamme normale.
    fn tick_torches(&mut self, dt: f32) {
        let t0 = self.world.clock;
        self.world.clock += dt as f64;
        for p in self.world.torches_expiring(t0, self.world.clock) {
            self.dirty.extend(World::chunks_in_light_radius(p[0], p[2]));
        }
    }

    /// Barre d'objets / inventaire (et réticule du pack).
    pub fn draw_hud(&self, ui: &mut Ui, size: (f32, f32), input: &Input) {
        let s = (size.1 / 360.0).floor().max(1.0);
        if self.hurt > 0.0 {
            ui.rect(0.0, 0.0, size.0, size.1, [0.7, 0.0, 0.0, self.hurt * 0.6]);
        }
        if !self.inventory.open {
            let c = 15.0 * s;
            ui.image(
                "gui/crosshair",
                (size.0 - c) / 2.0,
                (size.1 - c) / 2.0,
                c,
                c,
                [1.0, 1.0, 1.0, 0.9],
            );
            // Mode FPS : vie sur 100, munitions, solde, menu d'achat.
            if self.game.mode == GameMode::Fps {
                self.fps.hud(ui, size, input.cursor);
            }
            // Cœurs (survie) : au-dessus de la barre d'objets, à gauche, images du pack.
            if self.game.mode == GameMode::Survival {
                let slot = 20.0 * s;
                let x0 = (size.0 - slot * inventory::HOTBAR as f32) / 2.0;
                let y = size.1 - slot - 4.0 * s - 10.0 * s;
                let h = self.game.health;
                for i in 0..(MAX_HEALTH / 2) {
                    let sprite = match h.saturating_sub(i * 2) {
                        0 => "gui/icons/health_empty",
                        1 => "gui/icons/health_half",
                        _ => "gui/icons/health_full",
                    };
                    // Peu de vie : les cœurs tremblent légèrement.
                    let dy = if h <= 4 {
                        ((i as f32 * 2.3 + self.world.clock as f32 * 25.0).sin() * s).round()
                    } else {
                        0.0
                    };
                    ui.image(
                        sprite,
                        x0 + i as f32 * 8.0 * s,
                        y + dy,
                        9.0 * s,
                        9.0 * s,
                        [1.0; 4],
                    );
                }
            }
        }
        if self.game.mode.has_inventory() {
            self.inventory.draw(ui, size, input.cursor);
        }
        self.draw_pings(ui, size);
        self.chat.draw(ui, size);
    }

    /// Pings : losange jaune et distance, sur le bord de l'écran s'ils sont hors champ.
    fn draw_pings(&self, ui: &mut Ui, size: (f32, f32)) {
        if self.pings.is_empty() {
            return;
        }
        let s = (size.1 / 360.0).floor().max(1.0);
        let proj = glam::Mat4::perspective_infinite_reverse_rh(
            self.fov.to_radians(),
            size.0 / size.1,
            0.05,
        );
        let vp = proj * self.player.view();
        let eye = self.player.eye();
        for (p, age) in &self.pings {
            let a = ((PING_S - age) / 1.0).clamp(0.0, 1.0);
            let clip = vp * p.extend(1.0);
            let (mut nx, mut ny) = if clip.w.abs() > 1e-4 {
                (clip.x / clip.w, clip.y / clip.w)
            } else {
                (0.0, 0.0)
            };
            if clip.w < 0.0 {
                // Derrière le joueur : on l'envoie sur le bord opposé.
                nx = -nx * 100.0;
                ny = -ny * 100.0;
            }
            let m = 0.92;
            let k = (nx.abs() / m).max(ny.abs() / m).max(1.0);
            let (nx, ny) = (nx / k, ny / k);
            let x = (nx * 0.5 + 0.5) * size.0;
            let y = (0.5 - ny * 0.5) * size.1;
            let gold = [1.0, 0.8, 0.2, a];
            let dark = [0.0, 0.0, 0.0, 0.6 * a];
            // Losange en escalier (5 lignes de pixels agrandis).
            for (row, half) in [(0.0, 1.0), (1.0, 2.0), (2.0, 3.0), (3.0, 2.0), (4.0, 1.0)] {
                let w = half * 2.0 * s;
                ui.rect(
                    x - w / 2.0 - s,
                    y + (row - 2.5) * 2.0 * s - s,
                    w + 2.0 * s,
                    2.0 * s + 2.0 * s,
                    dark,
                );
                ui.rect(x - w / 2.0, y + (row - 2.5) * 2.0 * s, w, 2.0 * s, gold);
            }
            let d = (*p - eye).length();
            let label = format!("{d:.0} m");
            let tw = Ui::text_width(&label, s);
            ui.text(x - tw / 2.0, y + 7.0 * s, s, gold, label);
        }
    }

    /// Construit l'écran F3.
    pub fn build_debug(
        &mut self,
        frames: debug::FrameSummary,
        sys: minelib_platform::sysmon::SysStats,
        renderer: &Renderer,
        stats: FrameStats,
        fps_max: Option<f32>,
    ) {
        let hit = self.hit;
        self.overlay = debug::build(&debug::DebugInfo {
            frames,
            player: &self.player,
            hit,
            seed: self.world.seed,
            chunks_loaded: self.world.len(),
            stats,
            held: self
                .inventory
                .selected_item()
                .map_or("rien".into(), |i| i.name()),
            torch_info: hit.filter(|h| h.id.is_torch()).and_then(|h| {
                let t = self.world.torch(h.block)?;
                let flame = minelib_world::flame::CHEMICALS;
                Some(match (t.chemical, self.world.torch_remaining(&t)) {
                    (Some(i), Some(left)) if left.is_infinite() => {
                        format!("flamme {} : durée infinie", flame[i].color_name)
                    }
                    (Some(i), Some(left)) => format!(
                        "flamme {} : encore {}",
                        flame[i].color_name,
                        minelib_world::flame::format_duration(left)
                    ),
                    (Some(_), None) => "sel épuisé, flamme normale : durée infinie".into(),
                    _ => "flamme normale : durée infinie".into(),
                })
            }),
            torches: self.world.torch_count(),
            sys,
            renderer,
            engine: debug::EngineInfo {
                view_distance: self.view_distance,
                lod: self.streamer.settings,
                stream: self.streamer.stats,
                workers: self.workers.count(),
                fps_max,
                fov: self.fov,
                world_name: self.meta.name.clone(),
            },
        });
    }

    pub fn render(
        &self,
        renderer: &mut Renderer,
        ui: &Ui,
        show_debug: bool,
        time: f32,
        grabbed: bool,
    ) -> FrameStats {
        let fog_end = self.fog_end();
        let fps_mode = self.game.mode == GameMode::Fps;
        let fov = if fps_mode {
            self.fps.scope_fov().unwrap_or(self.fov)
        } else {
            self.fov
        };
        let view = self.view_with_punch();
        let hand = if fps_mode {
            // Arme en main, puis balles traçantes, étincelles et impacts dans le monde.
            let mut v = self.fps.viewmodel(renderer.aspect(), self.hand_light);
            let proj =
                Mat4::perspective_infinite_reverse_rh(fov.to_radians(), renderer.aspect(), 0.05);
            v.extend(
                self.fps
                    .world_fx(proj * view, self.player.eye(), &self.world),
            );
            v
        } else {
            self.hand.build(
                self.held_item(),
                renderer.aspect(),
                self.hand_light,
                self.game.mode != GameMode::Spectator,
            )
        };
        let params = FrameParams {
            viewmodel: &hand,
            eye: self.player.eye(),
            view,
            fov_y: fov.to_radians(),
            fog_start: fog_end * 0.45,
            fog_end,
            held_light: match self.inventory.selected_item() {
                Some(Item::Torch(chem)) if self.game.mode.has_inventory() => {
                    let rgb = chem
                        .and_then(|i| minelib_world::flame::CHEMICALS.get(i))
                        .map_or(minelib_world::flame::WOOD_FLAME, |c| c.rgb);
                    Some((rgb, 10.0))
                }
                _ => None,
            },
            selected: if grabbed {
                self.hit
                    .map(|h| minelib_world::raycast::selection_box(&self.world, h.block))
            } else {
                None
            },
            time,
            daylight: self.daylight,
            ui: Some(ui),
            overlay: show_debug.then_some(&self.overlay),
        };
        renderer.render(&params)
    }
}

/// Scène de démonstration : un mur de briques et toutes les torches colorées devant.
fn build_demo(world: &mut World) {
    let z_wall = 14;
    let n = minelib_world::flame::CHEMICALS.len() as i32 + 1;
    let x0 = 8 - (n - 1) * 3 / 2;
    for i in 0..n {
        let x = x0 + i * 3;
        let ground = world.top_solid(x, z_wall - 2).unwrap_or(64);
        let chem = if i == 0 { None } else { Some((i - 1) as usize) };
        world.place_torch(x, ground + 1, z_wall - 2, chem);
    }
    for x in x0 - 3..=x0 + n * 3 {
        let ground = world.top_solid(x, z_wall).unwrap_or(64);
        for y in ground + 1..ground + 7 {
            world.set(x, y, z_wall, BlockId::BRICK);
        }
    }
    // Raytracing : terrain aplani autour du joueur, mur miroir à l'ouest, blocs de fer à l'est.
    let reg = minelib_world::registry::registry();
    if let (Some(mirror), Some(iron)) = (reg.by_key("mirror"), reg.by_key("iron_block")) {
        let g0 = world.top_solid(8, 6).unwrap_or(64);
        for z in 0..=12 {
            for x in 1..=15 {
                for y in g0 + 1..g0 + 8 {
                    world.set(x, y, z, BlockId::AIR);
                }
                world.set(x, g0, z, BlockId::GRASS);
            }
        }
        for z in 2..=10 {
            for y in g0 + 1..g0 + 5 {
                world.set(2, y, z, mirror);
            }
        }
        // Grotte fermée sous le sol (x 20..27, z 0..7) avec un miroir et une torche.
        for x in 19..=28 {
            for z in -1..=8 {
                for y in g0 - 8..=g0 - 2 {
                    let wall =
                        x == 19 || x == 28 || z == -1 || z == 8 || y == g0 - 8 || y == g0 - 2;
                    world.set(x, y, z, if wall { BlockId::STONE } else { BlockId::AIR });
                }
            }
        }
        for z in 1..=6 {
            for y in g0 - 7..g0 - 3 {
                world.set(20, y, z, mirror);
            }
        }
        world.place_torch(26, g0 - 7, 6, None);
        // Miroir parallèle en face : reflets à l'infini ; torche collée au premier miroir.
        for z in 2..=10 {
            for y in g0 + 1..g0 + 5 {
                world.set(12, y, z, mirror);
            }
        }
        world.place_torch(3, g0 + 1, 3, Some(4));
        // Second miroir, perpendiculaire : miroir dans le miroir.
        for x in 5..=10 {
            for y in g0 + 1..g0 + 4 {
                world.set(x, y, 11, mirror);
            }
        }
        for z in 3..=8 {
            world.set(14, g0 + 1, z, iron);
            world.set(14, g0 + 2, z, iron);
        }
        world.place_torch(6, g0 + 1, 9, Some(3));
        world.set(5, g0 + 1, 4, BlockId::BRICK);
        world.set(5, g0 + 2, 4, BlockId::BRICK);
        // Torche murale contre la colonne, torches colorées près du miroir et du fer.
        world.place_torch_block(6, g0 + 2, 4, BlockId::wall_torch(1), Some(5));
        world.place_torch(4, g0 + 1, 7, Some(0));
        world.place_torch(11, g0 + 1, 5, Some(8));
    }
}

/// Distance maximale (blocs) des sources de lumière envoyées au rendu.
const LIGHTS_RANGE: f32 = 40.0;

/// Centre de la flamme d'une torche (droite ou murale) posée en `p`.
fn flame_center(id: BlockId, p: [i32; 3]) -> [f32; 3] {
    let local = match id.def().shape {
        minelib_world::registry::Shape::WallTorch(d) => {
            minelib_world::mesh::wall_torch_xf(d, [0.5, 0.656, 0.5])
        }
        _ => [0.5, 0.656, 0.5],
    };
    [
        p[0] as f32 + local[0],
        p[1] as f32 + local[1],
        p[2] as f32 + local[2],
    ]
}

/// Les sources de lumière les plus proches de `eye` (au plus `MAX_LIGHTS`).
pub fn collect_lights(world: &World, eye: Vec3) -> Vec<minelib_render::PointLight> {
    use minelib_render::{PointLight, MAX_LIGHTS};
    use minelib_world::LIGHT_RADIUS;
    let reach = LIGHT_RADIUS as f32;
    let near = |p: [f32; 3]| Vec3::from(p).distance(eye) < LIGHTS_RANGE;
    let mut out: Vec<PointLight> = Vec::new();
    let mut seen = HashSet::new();
    for (p, t) in world.torches() {
        let id = world.get(p[0], p[1], p[2]);
        let pos = flame_center(id, p);
        if !near(pos) {
            continue;
        }
        seen.insert(p);
        // Teinte conservée, canal le plus fort à 255 (comme l'éclairage des blocs).
        let c = world.torch_color(&t);
        let m = c.iter().copied().max().unwrap_or(1).max(1) as u32;
        out.push(PointLight {
            pos,
            size: 0.12,
            rgb: c.map(|v| (v as u32 * 255 / m) as u8),
            reach,
        });
    }
    let r = (LIGHTS_RANGE as i32 + CHUNK_SIZE as i32 - 1) / CHUNK_SIZE as i32;
    let c0 = ChunkPos::containing(eye.x.floor() as i32, eye.z.floor() as i32);
    for cz in c0.z - r..=c0.z + r {
        for cx in c0.x - r..=c0.x + r {
            let Some(ch) = world.chunk(ChunkPos { x: cx, z: cz }) else {
                continue;
            };
            let (ox, oz) = (cx * CHUNK_SIZE as i32, cz * CHUNK_SIZE as i32);
            for ([x, y, z], id) in ch.emitters() {
                let p = [ox + x as i32, y as i32, oz + z as i32];
                let Some(rgb) = id.def().light else { continue };
                if seen.contains(&p) {
                    continue;
                }
                let (pos, size) = if id.is_torch() {
                    (flame_center(id, p), 0.12)
                } else {
                    (
                        [p[0] as f32 + 0.5, p[1] as f32 + 0.5, p[2] as f32 + 0.5],
                        0.0,
                    )
                };
                if near(pos) {
                    out.push(PointLight {
                        pos,
                        size,
                        rgb,
                        reach,
                    });
                }
            }
        }
    }
    let d = |l: &PointLight| Vec3::from(l.pos).distance_squared(eye);
    out.sort_by(|a, b| d(a).total_cmp(&d(b)));
    out.truncate(MAX_LIGHTS);
    out
}

/// Plan de faces de miroir : axe, coordonnée, côté visible, étendue sur les deux autres axes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MirrorPlane {
    pub axis: u8,
    pub coord: i32,
    pub side: i32,
    pub lo: [i32; 2],
    pub hi: [i32; 2],
}

/// Images des sources de lumière derrière les miroirs qu'elles éclairent (les plus proches
/// du joueur d'abord).
pub fn mirror_lights(
    lights: &[minelib_render::PointLight],
    planes: &[MirrorPlane],
    eye: Vec3,
) -> Vec<minelib_render::MirrorLight> {
    use minelib_render::{MirrorLight, MAX_MIRROR_LIGHTS};
    let mut out = Vec::new();
    for l in lights {
        for p in planes {
            let ax = p.axis as usize;
            let (a, b) = ((ax + 1) % 3, (ax + 2) % 3);
            let dist = (l.pos[ax] - p.coord as f32) * p.side as f32;
            if dist <= 0.0 || dist >= l.reach {
                continue;
            }
            // Distance de la source au rectangle des faces de miroir.
            let da = (p.lo[0] as f32 - l.pos[a])
                .max(l.pos[a] - p.hi[0] as f32)
                .max(0.0);
            let db = (p.lo[1] as f32 - l.pos[b])
                .max(l.pos[b] - p.hi[1] as f32)
                .max(0.0);
            if (dist * dist + da * da + db * db).sqrt() >= l.reach {
                continue;
            }
            let mut pos = l.pos;
            pos[ax] = 2.0 * p.coord as f32 - l.pos[ax];
            out.push(MirrorLight {
                pos,
                rgb: l.rgb,
                reach: l.reach,
                axis: p.axis,
                coord: p.coord as f32,
                side: p.side as f32,
            });
        }
    }
    let d = |m: &MirrorLight| Vec3::from(m.pos).distance_squared(eye);
    out.sort_by(|x, y| d(x).total_cmp(&d(y)));
    out.truncate(MAX_MIRROR_LIGHTS);
    out
}

/// Lumière du ciel (0..15) dans les bits 12..15 de chaque case de la grille du raytracing :
/// 15 sous le ciel ouvert, puis −1 par case en se diffusant sous les toits et dans les
/// grottes, comme l'éclairage des blocs du monde.
fn sky_light_grid(ids: &mut [u16], solid: &[bool], covered: &[bool], [vx, vy, vz]: [i32; 3]) {
    let idx = |x: i32, y: i32, z: i32| (x + vx * (y + vy * z)) as usize;
    let mut sky = vec![0u8; ids.len()];
    let mut queue = std::collections::VecDeque::new();
    for z in 0..vz {
        for x in 0..vx {
            if covered[(x + vx * z) as usize] {
                continue;
            }
            for y in (0..vy).rev() {
                let i = idx(x, y, z);
                if solid[i] {
                    break;
                }
                sky[i] = 15;
                queue.push_back((x, y, z));
            }
        }
    }
    let dirs = [
        [1, 0, 0],
        [-1, 0, 0],
        [0, 1, 0],
        [0, -1, 0],
        [0, 0, 1],
        [0, 0, -1],
    ];
    while let Some((x, y, z)) = queue.pop_front() {
        let l = sky[idx(x, y, z)];
        if l <= 1 {
            continue;
        }
        for d in dirs {
            let (a, b, c) = (x + d[0], y + d[1], z + d[2]);
            if a < 0 || b < 0 || c < 0 || a >= vx || b >= vy || c >= vz {
                continue;
            }
            let j = idx(a, b, c);
            if !solid[j] && sky[j] < l - 1 {
                sky[j] = l - 1;
                queue.push_back((a, b, c));
            }
        }
    }
    for (v, s) in ids.iter_mut().zip(sky) {
        *v = (*v & 0x0fff) | (s as u16) << 12;
    }
}

/// Grille du raytracing calculée en arrière-plan.
struct VoxelGrid {
    origin: [i32; 3],
    ids: Vec<u16>,
    planes: Vec<MirrorPlane>,
    ms: f32,
}

/// Grille de blocs autour de `origin` (identifiants + lumière du ciel) et faces visibles
/// des miroirs, à partir des chunks donnés.
fn build_voxel_grid(
    chunks: &HashMap<ChunkPos, std::sync::Arc<minelib_world::Chunk>>,
    origin: [i32; 3],
) -> (Vec<u16>, Vec<MirrorPlane>) {
    use minelib_render::rt::{solid, traced, VOX};
    let [vx, vy, vz] = VOX.map(|v| v as i32);
    let mut ids = vec![0u16; (vx * vy * vz) as usize];
    let reg = minelib_world::registry::registry();
    let keep: Vec<bool> = reg.defs.iter().map(traced).collect();
    let opaque: Vec<bool> = reg.defs.iter().map(solid).collect();
    let is_mirror: Vec<bool> = reg.defs.iter().map(|d| d.reflect >= 15).collect();
    let full: Vec<bool> = reg.defs.iter().map(|d| d.full).collect();
    let mut mirrors = Vec::new();
    // Lumière du ciel dans la grille (comme le maillage) : blocs pleins et colonnes
    // couvertes au-dessus de la grille.
    let mut solid_cells = vec![false; ids.len()];
    let mut covered = vec![true; (vx * vz) as usize];
    let c0 = ChunkPos::containing(origin[0], origin[2]);
    let c1 = ChunkPos::containing(origin[0] + vx - 1, origin[2] + vz - 1);
    for cz in c0.z..=c1.z {
        for cx in c0.x..=c1.x {
            let Some(chunk) = chunks.get(&ChunkPos { x: cx, z: cz }) else {
                continue;
            };
            for lz in 0..CHUNK_SIZE as i32 {
                let z = cz * CHUNK_SIZE as i32 + lz - origin[2];
                if !(0..vz).contains(&z) {
                    continue;
                }
                for lx in 0..CHUNK_SIZE as i32 {
                    let x = cx * CHUNK_SIZE as i32 + lx - origin[0];
                    if !(0..vx).contains(&x) {
                        continue;
                    }
                    let above = (origin[1] + vy).max(0) as usize;
                    covered[(x + vx * z) as usize] = (above..chunk.top()).any(|wy| {
                        full.get(chunk.get(lx as usize, wy, lz as usize).0 as usize)
                            .copied()
                            .unwrap_or(false)
                    });
                    // Seulement les couches où le chunk a des blocs : les tranches vides
                    // (sous le plus bas bloc, au-dessus du plus haut) restent de l'air.
                    let ylo = (chunk.bottom() as i32 - origin[1]).max(0);
                    let yhi = (chunk.top() as i32 - origin[1]).min(vy);
                    for y in ylo..yhi {
                        let wy = origin[1] + y;
                        let id = chunk.get(lx as usize, wy as usize, lz as usize);
                        if full.get(id.0 as usize).copied().unwrap_or(false) {
                            solid_cells[(x + vx * (y + vy * z)) as usize] = true;
                        }
                        if id.0 != 0 && keep.get(id.0 as usize).copied().unwrap_or(false) {
                            ids[(x + vx * (y + vy * z)) as usize] = id.0;
                            if is_mirror.get(id.0 as usize).copied().unwrap_or(false) {
                                mirrors.push([x, y, z]);
                            }
                        }
                    }
                }
            }
        }
    }
    sky_light_grid(&mut ids, &solid_cells, &covered, [vx, vy, vz]);
    // Faces de miroir non masquées, regroupées par plan.
    let at = |p: [i32; 3]| -> bool {
        if (0..3).any(|i| p[i] < 0 || p[i] >= VOX[i] as i32) {
            return false;
        }
        let id = ids[(p[0] + vx * (p[1] + vy * p[2])) as usize] & 0x0fff;
        id != 0 && opaque.get(id as usize).copied().unwrap_or(false)
    };
    let mut planes: Vec<MirrorPlane> = Vec::new();
    for m in mirrors {
        for axis in 0..3 {
            for side in [-1i32, 1] {
                let mut nb = m;
                nb[axis] += side;
                if at(nb) {
                    continue;
                }
                let coord = origin[axis] + m[axis] + (side + 1) / 2;
                let (a, b) = ((axis + 1) % 3, (axis + 2) % 3);
                let (u, v) = (origin[a] + m[a], origin[b] + m[b]);
                match planes
                    .iter_mut()
                    .find(|p| p.axis == axis as u8 && p.coord == coord && p.side == side)
                {
                    Some(p) => {
                        p.lo = [p.lo[0].min(u), p.lo[1].min(v)];
                        p.hi = [p.hi[0].max(u + 1), p.hi[1].max(v + 1)];
                    }
                    None => planes.push(MirrorPlane {
                        axis: axis as u8,
                        coord,
                        side,
                        lo: [u, v],
                        hi: [u + 1, v + 1],
                    }),
                }
            }
        }
    }
    (ids, planes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detail_radius_and_far_terrain() {
        let c = CHUNK_SIZE as f32;
        // 64 chunks voulus, détail 24 : pleine résolution 24, simplifié jusqu'au rendu lointain.
        let s = lod_settings(64, 200, 4096.0, true, 24);
        assert_eq!((s.detail, s.max_distance), (24.0 * c, 4096.0));
        // Mémoire vive limitée à 16 chunks détaillés : la distance reste 64 en simplifié.
        let s = lod_settings(64, 16, 0.0, false, 0);
        assert_eq!((s.detail, s.max_distance), (16.0 * c, 64.0 * c));
        // Rendu lointain désactivé et détail partout : tout en pleine résolution.
        let s = lod_settings(10, 200, 0.0, false, 0);
        assert!(!s.enabled());
        assert_eq!(s.detail, 10.0 * c);
        // Détail plus grand que la distance de rendu : borné par elle.
        let s = lod_settings(10, 200, 0.0, false, 24);
        assert!(!s.enabled());
    }
}
