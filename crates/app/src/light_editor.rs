//! Éditeur de lumières (touche L), version Minelib de l'éditeur d'IRLights : un panneau à
//! droite de l'écran pour ajouter, choisir et régler les lumières (placement, couleur,
//! intensité, portée, cône, rayons volumétriques, ombres, gobo), des guides dans le monde
//! (portée, cône, poignées d'angle et de portée), des flèches pour déplacer la lumière
//! choisie, une poignée pour orienter un projecteur, et une caméra libre (F) pour se placer
//! où l'on veut sans bouger le joueur.
//!
//! Souris : clic sur une lumière pour la choisir ; glisser une flèche (X rouge, Y vert, Z
//! bleu) ou le carré du centre pour la déplacer (Ctrl : par demi-bloc) ; clic droit tenu
//! (ou clic gauche tenu dans le vide) pour tourner la vue ; molette dans la vue : vitesse de
//! la caméra libre. Clavier : Suppr efface la lumière choisie, Échap ferme (Ctrl+D n'est
//! pas pris : Ctrl fait courir la caméra libre, D la déplace).

use glam::{Mat4, Vec3, Vec4};
use minelib_platform::{Input, KeyCode, Mouse};
use minelib_render::{tr, DynState, DynStats, Ui};

use crate::keys::{Action, Bindings};
use crate::lights::{self, Kind, Lights, PlacedLight};
use crate::menu::{fit, GOLD, GREY, RED, WHITE};

const PANEL_BG: [f32; 4] = [0.055, 0.058, 0.07, 0.93];
const ROW_BG: [f32; 4] = [0.12, 0.125, 0.15, 1.0];
const ROW_HOVER: [f32; 4] = [0.18, 0.19, 0.23, 1.0];
const FILL: [f32; 4] = [0.85, 0.62, 0.18, 1.0];
const SELECT: [f32; 4] = [0.85, 0.62, 0.18, 0.28];
const DIM: [f32; 4] = [0.48, 0.5, 0.56, 1.0];
/// Part de la largeur du panneau prise par les libellés des curseurs.
const LABEL: f32 = 0.56;
const AXIS: [[f32; 4]; 3] = [
    [0.95, 0.28, 0.25, 1.0],
    [0.35, 0.9, 0.32, 1.0],
    [0.3, 0.5, 1.0, 1.0],
];

/// Caméra de l'image : projeter les guides, lancer les rayons de la souris.
#[derive(Clone, Copy, Debug)]
pub struct Cam {
    pub pos: Vec3,
    pub look: Vec3,
    pub view_proj: Mat4,
    pub size: (f32, f32),
}

impl Cam {
    /// Point de l'écran (pixels) où se trouve `p`, s'il est devant la caméra.
    pub fn project(&self, p: Vec3) -> Option<(f32, f32)> {
        // Depuis la caméra : les guides ne tremblent pas loin de l'origine.
        let c = minelib_render::eye_relative(self.view_proj) * Vec4::from((p - self.pos, 1.0));
        if c.w < 0.05 {
            return None;
        }
        Some((
            (c.x / c.w * 0.5 + 0.5) * self.size.0,
            (0.5 - c.y / c.w * 0.5) * self.size.1,
        ))
    }

    /// Direction du rayon qui passe par le pixel `px`.
    pub fn ray(&self, px: (f32, f32)) -> Vec3 {
        let ndc = (
            px.0 / self.size.0.max(1.0) * 2.0 - 1.0,
            1.0 - px.1 / self.size.1.max(1.0) * 2.0,
        );
        // Tirée des lignes de la matrice, sans l'inverser (voir `minelib_render::view_ray`).
        minelib_render::view_ray(self.view_proj, [ndc.0, ndc.1]).normalize_or(self.look)
    }
}

/// Caméra libre de l'éditeur : se déplace sans le joueur.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FreeCam {
    pub pos: Vec3,
    pub yaw: f32,
    pub pitch: f32,
    /// Vitesse (blocs par seconde), réglée à la molette.
    pub speed: f32,
}

impl FreeCam {
    pub fn look(&self) -> Vec3 {
        Vec3::new(
            self.yaw.sin() * self.pitch.cos(),
            self.pitch.sin(),
            -self.yaw.cos() * self.pitch.cos(),
        )
    }

    pub fn view(&self) -> Mat4 {
        Mat4::look_to_rh(self.pos, self.look(), Vec3::Y)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tab {
    Placement,
    Base,
    Volume,
    Shadows,
    Gobo,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cat {
    Presets,
    Beams,
    Shadows,
    Surfaces,
    Auto,
    Interface,
}

/// Ce que la souris tient.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Drag {
    /// Curseur ou valeur du panneau (identifiant du réglage).
    Widget(u32),
    /// Flèche du déplacement (axe 0 x, 1 y, 2 z).
    Axis(usize),
    /// Carré du centre : déplacement dans le plan de l'écran.
    Plane,
    /// Poignée de direction d'un projecteur.
    Direction,
    /// Guides : bord extérieur, bord intérieur, portée.
    Outer,
    Inner,
    Range,
    /// Barres de défilement.
    ListBar,
    GoboBar,
}

/// Éditeur de lumières.
pub struct Editor {
    pub open: bool,
    pub selected: Option<usize>,
    tab: Tab,
    /// Onglet des réglages généraux (`settings_screen`).
    cat: Cat,
    pub free_cam: Option<FreeCam>,
    /// La souris tourne la vue (le jeu capture alors le curseur).
    pub looking: bool,
    drag: Option<Drag>,
    drag_mouse: (f32, f32),
    drag_value: [f64; 3],
    list_scroll: usize,
    gobo_scroll: usize,
    settings_scroll: f32,
    /// Saisie du nom de la lumière choisie.
    pub editing_name: bool,
    /// Avertissement des lumières automatiques ouvert.
    warn_auto: bool,
    /// Teinte gardée pour une couleur grise (sa teinte ne se lit plus dans la couleur).
    hue: f32,
    /// Le curseur était au-dessus du panneau (ses clics ne vont pas au monde).
    pub over_panel: bool,
    /// Boutons du gobo : relire le dossier, l'ouvrir.
    gobo_reload: bool,
    gobo_folder: bool,
}

impl Default for Editor {
    fn default() -> Self {
        Self {
            open: false,
            selected: None,
            tab: Tab::Base,
            cat: Cat::Presets,
            free_cam: None,
            looking: false,
            drag: None,
            drag_mouse: (0.0, 0.0),
            drag_value: [0.0; 3],
            list_scroll: 0,
            gobo_scroll: 0,
            settings_scroll: 0.0,
            editing_name: false,
            warn_auto: false,
            hue: 0.0,
            over_panel: false,
            gobo_reload: false,
            gobo_folder: false,
        }
    }
}

/// Couleur donnée par les curseurs Teinte, Saturation et Valeur. Une couleur blanche ou
/// grise n'a pas de teinte visible : bouger la teinte la rend pleine (saturation 100 %),
/// sans passer d'abord par une couleur proposée ; une couleur noire repart à pleine valeur
/// quand on bouge sa teinte ou sa saturation.
fn color_from_sliders(h: f32, s: f32, v: f32, hue_moved: bool, sat_moved: bool) -> [f32; 3] {
    let s = if hue_moved && s < 0.1 { 1.0 } else { s };
    let v = if (hue_moved || sat_moved) && v < 0.05 {
        1.0
    } else {
        v
    };
    hsv_to_rgb(h, s, v)
}

/// RVB (0..1) → teinte (0..1), saturation, valeur.
pub fn rgb_to_hsv(c: [f32; 3]) -> (f32, f32, f32) {
    let (r, g, b) = (c[0], c[1], c[2]);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let h = if d <= 1e-6 {
        0.0
    } else if max == r {
        ((g - b) / d).rem_euclid(6.0) / 6.0
    } else if max == g {
        ((b - r) / d + 2.0) / 6.0
    } else {
        ((r - g) / d + 4.0) / 6.0
    };
    let s = if max <= 1e-6 { 0.0 } else { d / max };
    (h, s, max)
}

pub fn hsv_to_rgb(h: f32, s: f32, v: f32) -> [f32; 3] {
    let h6 = h.rem_euclid(1.0) * 6.0;
    let c = v * s;
    let x = c * (1.0 - (h6 % 2.0 - 1.0).abs());
    let (r, g, b) = match h6 as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    [r + m, g + m, b + m]
}

/// Couleurs proposées d'un clic : blanc, ampoule, bougie, néon, rouge, vert, bleu, violet.
const SWATCHES: [[f32; 3]; 8] = [
    [1.0, 1.0, 1.0],
    [1.0, 0.82, 0.58],
    [1.0, 0.6, 0.28],
    [0.7, 0.85, 1.0],
    [1.0, 0.18, 0.15],
    [0.25, 1.0, 0.35],
    [0.2, 0.4, 1.0],
    [0.75, 0.3, 1.0],
];

/// Contexte du panneau : position courante, mesures, souris.
struct P<'a> {
    ui: &'a mut Ui,
    cursor: Option<(f32, f32)>,
    click: bool,
    held: bool,
    wheel: f32,
    ctrl: bool,
    /// Échelle du texte, hauteur d'une ligne, marge.
    ts: f32,
    row: f32,
    pad: f32,
    x: f32,
    w: f32,
    y: f32,
    /// Bas de la zone visible (les lignes au-delà ne sont pas dessinées).
    clip: (f32, f32),
}

impl P<'_> {
    fn hovered(&self, x: f32, y: f32, w: f32, h: f32) -> bool {
        self.cursor
            .is_some_and(|(cx, cy)| cx >= x && cx < x + w && cy >= y && cy < y + h)
    }

    fn visible(&self, h: f32) -> bool {
        self.y >= self.clip.0 - 0.5 && self.y + h <= self.clip.1 + 0.5
    }

    fn text(&mut self, x: f32, y: f32, color: [f32; 4], t: &str) {
        self.ui.text(x, y, self.ts, color, t);
    }

    fn section(&mut self, t: &str) {
        if self.visible(self.row) {
            let ty = self.y + (self.row - 8.0 * self.ts) / 2.0;
            let x = self.x;
            self.text(x, ty, GOLD, t);
        }
        self.y += self.row;
    }

    fn note(&mut self, t: &str, color: [f32; 4]) {
        // Texte long : coupé en lignes qui tiennent dans la largeur.
        let max = ((self.w / (8.0 * self.ts)).floor() as usize).max(8);
        let mut line = String::new();
        let mut lines = Vec::new();
        for word in minelib_render::tr(t).split(' ') {
            if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > max {
                lines.push(std::mem::take(&mut line));
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
        }
        if !line.is_empty() {
            lines.push(line);
        }
        for l in lines {
            let h = 9.5 * self.ts;
            if self.visible(h) {
                let (x, y) = (self.x, self.y);
                self.ui.text(x, y, self.ts, color, l);
            }
            self.y += h;
        }
        self.y += self.pad * 0.5;
    }

    /// Bouton dans la ligne (x, largeur) ; renvoie vrai s'il vient d'être cliqué.
    fn button_at(&mut self, x: f32, w: f32, label: &str, active: bool, enabled: bool) -> bool {
        let h = self.row - self.pad * 0.5;
        let y = self.y;
        let hover = enabled && self.hovered(x, y, w, h);
        let bg = if active {
            [0.55, 0.4, 0.12, 1.0]
        } else if hover {
            ROW_HOVER
        } else {
            ROW_BG
        };
        self.ui.rect(x, y, w, h, bg);
        let label = fit(label, w - 4.0 * self.ts, self.ts);
        let tw = Ui::text_width(&label, self.ts);
        let color = if enabled { WHITE } else { DIM };
        self.ui.text(
            x + (w - tw) / 2.0,
            y + (h - 8.0 * self.ts) / 2.0,
            self.ts,
            color,
            label,
        );
        hover && self.click
    }

    /// Rangée de boutons ; renvoie l'indice cliqué. Chacun a la largeur de son texte, plus
    /// une part égale de la place qui reste (ou moins s'il n'y en a pas assez).
    fn buttons(&mut self, labels: &[&str], active: Option<usize>) -> Option<usize> {
        let n = labels.len().max(1) as f32;
        let gap = self.pad * 0.5;
        let room = self.w - gap * (n - 1.0);
        let widths: Vec<f32> = labels
            .iter()
            .map(|l| Ui::text_width(l, self.ts) + 4.0 * self.ts)
            .collect();
        let total: f32 = widths.iter().sum();
        let mut hit = None;
        if self.visible(self.row) {
            let mut x = self.x;
            for (i, l) in labels.iter().enumerate() {
                let bw = if total <= room {
                    widths[i] + (room - total) / n
                } else {
                    widths[i] * room / total
                };
                if self.button_at(x, bw, l, active == Some(i), true) {
                    hit = Some(i);
                }
                x += bw + gap;
            }
        }
        self.y += self.row;
        hit
    }
}

/// Valeur affichée : `dec` décimales, puis l'unité.
fn fmt(v: f32, dec: usize, unit: &str) -> String {
    format!("{v:.dec$}{unit}")
}

/// Distance en blocs (unité dans la langue affichée).
fn blocks(v: f32, dec: usize) -> String {
    format!("{v:.dec$} {}", tr("blocs"))
}

impl Editor {
    /// Hauteur d'une ligne et échelle du texte pour une fenêtre de hauteur `h`.
    fn metrics(h: f32) -> (f32, f32, f32) {
        let ts = (h / 500.0).floor().max(1.0);
        (ts, 12.0 * ts, 3.0 * ts)
    }

    /// Rectangle du panneau (x, y, largeur, hauteur).
    pub fn panel_rect(size: (f32, f32)) -> (f32, f32, f32, f32) {
        let (ts, _, pad) = Self::metrics(size.1);
        // Environ 34 caractères de large (les libellés tiennent sans être coupés).
        let w = (280.0 * ts).min(size.0 * 0.34);
        (size.0 - w - pad * 2.0, pad * 2.0, w, size.1 - pad * 4.0)
    }

    pub fn close(&mut self) {
        self.open = false;
        self.free_cam = None;
        self.looking = false;
        self.drag = None;
        self.editing_name = false;
        self.warn_auto = false;
    }

    /// Échap : arrête la saisie du nom, ferme l'avertissement, sinon ferme l'éditeur.
    pub fn escape(&mut self) {
        if self.editing_name {
            self.editing_name = false;
        } else if self.warn_auto {
            self.warn_auto = false;
        } else {
            self.close();
        }
    }

    /// Caméra libre : allumée à la place de la caméra (`eye`, regard `yaw`, `pitch`), ou
    /// éteinte.
    pub fn toggle_free_cam(&mut self, eye: Vec3, yaw: f32, pitch: f32) {
        self.free_cam = match self.free_cam {
            Some(_) => None,
            None => Some(FreeCam {
                pos: eye,
                yaw,
                pitch,
                speed: 8.0,
            }),
        };
    }

    /// Déplace la caméra libre (touches de déplacement du joueur ; courir : 3 fois plus
    /// vite) et la tourne (`turn` : radians de lacet et de tangage).
    pub fn move_free_cam(&mut self, input: &Input, keys: &Bindings, dt: f32, turn: (f32, f32)) {
        let typing = self.editing_name;
        let Some(c) = &mut self.free_cam else { return };
        c.yaw += turn.0;
        c.pitch = (c.pitch - turn.1).clamp(-1.55, 1.55);
        if typing {
            return;
        }
        let axis = |pos: Action, neg: Action| {
            keys.held(input, pos) as i32 as f32 - keys.held(input, neg) as i32 as f32
        };
        let fwd = c.look();
        let right = Vec3::new(c.yaw.cos(), 0.0, c.yaw.sin());
        let mut v = fwd * axis(Action::Forward, Action::Back)
            + right * axis(Action::Right, Action::Left)
            + Vec3::Y * axis(Action::Jump, Action::Sneak);
        if v.length_squared() > 0.0 {
            v = v.normalize();
        }
        let boost = if keys.held(input, Action::Sprint) {
            3.0
        } else {
            1.0
        };
        c.pos += v * c.speed * boost * dt.min(0.1);
    }

    /// Nom libre « Lumière N » / « Projecteur N » (le plus petit N pas encore pris).
    fn next_name(lights: &Lights, kind: Kind) -> String {
        let base = tr(match kind {
            Kind::Point => "Lumière",
            Kind::Spot => "Projecteur",
        });
        let mut n = 1;
        while lights.scene.iter().any(|l| l.name == format!("{base} {n}")) {
            n += 1;
        }
        format!("{base} {n}")
    }

    fn add(&mut self, lights: &mut Lights, kind: Kind, cam: &Cam) {
        if lights.scene.len() >= lights::MAX_PLACED {
            return;
        }
        // Un peu devant la caméra : on la voit tout de suite.
        let p = cam.pos + cam.look * 2.5;
        let mut l = PlacedLight {
            kind,
            name: Self::next_name(lights, kind),
            position: [p.x as f64, p.y as f64, p.z as f64],
            ..Default::default()
        };
        if kind == Kind::Spot {
            // Projecteur : éclaire là où l'on regarde.
            l.direction = cam.look.normalize_or(Vec3::NEG_Y).to_array();
        }
        l.id = lights.new_id();
        lights.scene.push(l);
        self.selected = Some(lights.scene.len() - 1);
        lights.touch_scene();
    }

    fn duplicate(&mut self, lights: &mut Lights) {
        let Some(i) = self.selected.filter(|&i| i < lights.scene.len()) else {
            return;
        };
        if lights.scene.len() >= lights::MAX_PLACED {
            return;
        }
        let mut l = lights.scene[i].clone();
        l.id = lights.new_id();
        l.name = format!("{} {}", l.name, tr("copie"))
            .chars()
            .take(40)
            .collect();
        lights.scene.insert(i + 1, l);
        self.selected = Some(i + 1);
        lights.touch_scene();
    }

    fn delete(&mut self, lights: &mut Lights) {
        let Some(i) = self.selected.filter(|&i| i < lights.scene.len()) else {
            return;
        };
        lights.scene.remove(i);
        self.selected = if lights.scene.is_empty() {
            None
        } else {
            Some(i.min(lights.scene.len() - 1))
        };
        self.editing_name = false;
        lights.touch_scene();
    }

    /// Interface de l'éditeur pour une image : guides et poignées dans le monde, puis le
    /// panneau. `looking` passe à vrai quand la souris doit tourner la vue (clic droit, ou
    /// clic gauche tenu dans le vide).
    #[allow(clippy::too_many_arguments)]
    pub fn ui(
        &mut self,
        ui: &mut Ui,
        input: &Input,
        lights: &mut Lights,
        cam: &Cam,
        stats: DynStats,
        root_hint: &str,
    ) {
        if !self.open {
            return;
        }
        let size = cam.size;
        if self.selected.is_some_and(|i| i >= lights.scene.len()) {
            self.selected = None;
        }
        let (px, py, pw, ph) = Self::panel_rect(size);
        let over = input
            .cursor
            .is_some_and(|(x, y)| x >= px && x < px + pw && y >= py && y < py + ph);
        self.over_panel = over || self.warn_auto;
        let held_l = input.mouse_held(Mouse::Left);
        let held_r = input.mouse_held(Mouse::Right);
        if !held_l && !held_r {
            self.drag = None;
        }
        // Suppr : efface la lumière choisie (sans saisie en cours).
        if !self.editing_name && !input.grabbed && input.pressed(KeyCode::Delete) {
            self.delete(lights);
        }
        // 1. Le monde : guides, lumières, poignées.
        let world_click = !self.over_panel && input.mouse_pressed(Mouse::Left) && !input.grabbed;
        let consumed = self.world_overlay(ui, input, lights, cam, world_click);
        // Tourner la vue : clic droit tenu hors du panneau, ou clic gauche tenu dans le
        // vide (rien attrapé).
        if self.looking {
            self.looking = (held_r || held_l) && self.drag.is_none();
        } else if !self.over_panel
            && self.drag.is_none()
            && !input.grabbed
            && (input.mouse_pressed(Mouse::Right) || (world_click && !consumed))
        {
            self.looking = true;
        }
        // Vitesse de la caméra libre : molette dans la vue.
        if !self.over_panel && input.scroll.abs() > 0.0 {
            if let Some(c) = &mut self.free_cam {
                c.speed = (c.speed * 1.2f32.powf(input.scroll)).clamp(1.0, 64.0);
            }
        }
        // 2. Le panneau.
        self.panel(ui, input, lights, cam, stats, (px, py, pw, ph));
        // 3. Bandeau d'aide en bas à gauche.
        let (ts, row, pad) = Self::metrics(size.1);
        let mut hints = Vec::new();
        if let Some(c) = self.free_cam {
            hints.push(format!(
                "{} : {:.1}",
                tr("Vitesse de la caméra (molette)"),
                c.speed
            ));
            hints.push("F : revenir au joueur".to_string());
        } else {
            hints.push("F : caméra libre".to_string());
        }
        hints.push("Clic droit tenu : tourner la vue".into());
        hints.push(format!(
            "{} / {root_hint} : {}",
            tr("Échap"),
            tr("fermer l'éditeur")
        ));
        let mut y = size.1 - pad * 2.0 - row * hints.len() as f32;
        for h in hints {
            let tw = Ui::text_width(&h, ts);
            ui.rect(
                pad * 2.0,
                y,
                tw + pad * 2.0,
                row - pad * 0.3,
                [0.0, 0.0, 0.0, 0.5],
            );
            ui.text(
                pad * 3.0,
                y + (row - 8.0 * ts) / 2.0 - pad * 0.15,
                ts,
                WHITE,
                h,
            );
            y += row;
        }
        // 4. Avertissement des lumières automatiques (par-dessus tout).
        if self.warn_auto {
            self.auto_warning(ui, input, lights, size);
        }
    }

    // ------------------------------------------------------------ le monde

    /// Guides, marques des lumières et poignées de la lumière choisie. Renvoie vrai si le
    /// clic a servi (lumière choisie, poignée attrapée).
    fn world_overlay(
        &mut self,
        ui: &mut Ui,
        input: &Input,
        lights: &mut Lights,
        cam: &Cam,
        click: bool,
    ) -> bool {
        let (ts, _, _) = Self::metrics(cam.size.1);
        let line = (1.6 * ts).max(1.5);
        let mouse = input.cursor;
        let mut consumed = false;
        // Poignées de la lumière choisie : en premier (elles passent devant les marques).
        let sel = self.selected.filter(|&i| i < lights.scene.len());
        if let (Some(i), Some(m)) = (sel, mouse) {
            if self.drag.is_none() && click {
                if let Some(d) = self.pick_handle(&lights.scene[i], cam, m) {
                    self.start_drag(d, m, &lights.scene[i]);
                    consumed = true;
                }
            }
            if let Some(d) = self.drag {
                if self.apply_drag(d, &mut lights.scene[i], cam, m, input) {
                    lights.touch_scene();
                }
            }
        }
        // Guides de toutes les lumières (plus marqués pour la choisie).
        for (k, l) in lights.scene.iter().enumerate() {
            let chosen = sel == Some(k);
            if lights.settings.guides || chosen {
                Self::draw_guides(ui, cam, l, chosen, line);
            }
        }
        // Marques : un losange de la couleur de la lumière ; clic pour la choisir.
        let mut best: Option<(usize, f32)> = None;
        for (k, l) in lights.scene.iter().enumerate() {
            let Some((sx, sy)) = cam.project(l.pos()) else {
                continue;
            };
            let chosen = sel == Some(k);
            let r = if chosen { 7.0 } else { 5.0 } * ts;
            let c = [
                l.color[0],
                l.color[1],
                l.color[2],
                if l.on { 1.0 } else { 0.35 },
            ];
            let edge = if chosen { WHITE } else { [0.0, 0.0, 0.0, 0.8] };
            for (a, b) in [
                ((0.0, -r), (r, 0.0)),
                ((r, 0.0), (0.0, r)),
                ((0.0, r), (-r, 0.0)),
                ((-r, 0.0), (0.0, -r)),
            ] {
                ui.line(sx + a.0, sy + a.1, sx + b.0, sy + b.1, line * 2.2, edge);
            }
            ui.rect(sx - r * 0.45, sy - r * 0.45, r * 0.9, r * 0.9, c);
            if l.kind == Kind::Spot {
                // Petit trait dans la direction du faisceau.
                if let Some((dx, dy)) = cam.project(l.pos() + Vec3::from(l.direction) * 0.8) {
                    ui.line(sx, sy, dx, dy, line, edge);
                }
            }
            if chosen {
                ui.text(sx + r + 3.0 * ts, sy - 4.0 * ts, ts, WHITE, l.name.clone());
            }
            if let Some((mx, my)) = mouse {
                let d = ((mx - sx).powi(2) + (my - sy).powi(2)).sqrt();
                if d < r + 4.0 * ts && best.is_none_or(|b| d < b.1) {
                    best = Some((k, d));
                }
            }
        }
        if click && !consumed && self.drag.is_none() {
            if let Some((k, _)) = best {
                self.selected = Some(k);
                self.editing_name = false;
                consumed = true;
                // Attraper la marque déplace la lumière dans le plan de l'écran.
                if let Some(m) = mouse {
                    self.start_drag(Drag::Plane, m, &lights.scene[k]);
                }
            }
        }
        // Flèches de déplacement et poignée de direction de la lumière choisie.
        if let Some(i) = self.selected.filter(|&i| i < lights.scene.len()) {
            self.draw_gizmo(ui, cam, &lights.scene[i], mouse, line);
        }
        consumed
    }

    /// Longueur des flèches (blocs) : la même taille à l'écran quelle que soit la distance.
    fn gizmo_len(cam: &Cam, p: Vec3) -> f32 {
        (p - cam.pos).length().max(0.5) * 0.14
    }

    fn draw_gizmo(
        &self,
        ui: &mut Ui,
        cam: &Cam,
        l: &PlacedLight,
        mouse: Option<(f32, f32)>,
        line: f32,
    ) {
        let p = l.pos();
        let Some(o) = cam.project(p) else { return };
        let len = Self::gizmo_len(cam, p);
        let hot = mouse.and_then(|m| self.pick_handle(l, cam, m));
        for (a, &c) in AXIS.iter().enumerate() {
            let mut dir = Vec3::ZERO;
            dir[a] = 1.0;
            if let Some(e) = cam.project(p + dir * len) {
                let on = self.drag == Some(Drag::Axis(a)) || hot == Some(Drag::Axis(a));
                let w = if on { line * 2.6 } else { line * 1.7 };
                ui.line(o.0, o.1, e.0, e.1, w, c);
                let s = w * 2.2;
                ui.rect(e.0 - s / 2.0, e.1 - s / 2.0, s, s, c);
            }
        }
        // Carré du centre.
        let on = self.drag == Some(Drag::Plane) || hot == Some(Drag::Plane);
        let s = line * if on { 5.0 } else { 4.0 };
        ui.rect(
            o.0 - s / 2.0,
            o.1 - s / 2.0,
            s,
            s,
            [1.0, 1.0, 1.0, if on { 0.9 } else { 0.55 }],
        );
        if l.kind == Kind::Spot {
            let h = p + Vec3::from(l.direction) * len * 1.7;
            if let Some(e) = cam.project(h) {
                let on = self.drag == Some(Drag::Direction) || hot == Some(Drag::Direction);
                ui.line(o.0, o.1, e.0, e.1, line, [1.0, 0.85, 0.3, 0.9]);
                let r = line * if on { 4.5 } else { 3.2 };
                for (a, b) in [
                    ((0.0, -r), (r, 0.0)),
                    ((r, 0.0), (0.0, r)),
                    ((0.0, r), (-r, 0.0)),
                    ((-r, 0.0), (0.0, -r)),
                ] {
                    ui.line(e.0 + a.0, e.1 + a.1, e.0 + b.0, e.1 + b.1, line * 2.0, GOLD);
                }
            }
        }
    }

    /// Points d'un cercle (centre, deux axes, rayon) projetés à l'écran (`None` derrière).
    fn ring(cam: &Cam, c: Vec3, u: Vec3, v: Vec3, r: f32) -> Vec<Option<(f32, f32)>> {
        (0..=48)
            .map(|k| {
                let a = k as f32 / 48.0 * std::f32::consts::TAU;
                cam.project(c + (u * a.cos() + v * a.sin()) * r)
            })
            .collect()
    }

    fn polyline(ui: &mut Ui, pts: &[Option<(f32, f32)>], w: f32, color: [f32; 4]) {
        for p in pts.windows(2) {
            if let (Some(a), Some(b)) = (p[0], p[1]) {
                ui.line(a.0, a.1, b.0, b.1, w, color);
            }
        }
    }

    /// Distance (pixels) du point `m` à la ligne brisée.
    fn dist_polyline(pts: &[Option<(f32, f32)>], m: (f32, f32)) -> f32 {
        let mut best = f32::MAX;
        for p in pts.windows(2) {
            if let (Some(a), Some(b)) = (p[0], p[1]) {
                let (dx, dy) = (b.0 - a.0, b.1 - a.1);
                let len2 = dx * dx + dy * dy;
                let t = if len2 > 0.0 {
                    (((m.0 - a.0) * dx + (m.1 - a.1) * dy) / len2).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                let (qx, qy) = (a.0 + dx * t, a.1 + dy * t);
                best = best.min(((m.0 - qx).powi(2) + (m.1 - qy).powi(2)).sqrt());
            }
        }
        best
    }

    /// Repère du cône d'un projecteur : (direction, deux axes perpendiculaires).
    fn spot_frame(l: &PlacedLight) -> (Vec3, Vec3, Vec3) {
        let d = Vec3::from(l.direction).normalize_or(Vec3::NEG_Y);
        let up = if d.y.abs() > 0.99 { Vec3::Z } else { Vec3::Y };
        let s = d.cross(up).normalize();
        (d, s, s.cross(d))
    }

    /// Rayon du cercle du cône (demi-angle en degrés) au bout de sa portée.
    fn cap_radius(range: f32, half_deg: f32) -> f32 {
        range * half_deg.clamp(0.5, 80.0).to_radians().tan()
    }

    fn draw_guides(ui: &mut Ui, cam: &Cam, l: &PlacedLight, chosen: bool, line: f32) {
        // Discrets : ils ne doivent pas cacher la lumière qu'on règle.
        let a = if chosen { 0.7 } else { 0.3 };
        let tint = |k: f32| {
            [
                (0.35 + 0.65 * l.color[0]) * k,
                (0.35 + 0.65 * l.color[1]) * k,
                (0.35 + 0.65 * l.color[2]) * k,
                a,
            ]
        };
        let w = if chosen { line * 1.3 } else { line };
        let p = l.pos();
        match l.kind {
            Kind::Point => {
                let r = l.radius;
                for (u, v) in [(Vec3::X, Vec3::Y), (Vec3::X, Vec3::Z), (Vec3::Y, Vec3::Z)] {
                    Self::polyline(ui, &Self::ring(cam, p, u, v, r), w, tint(1.0));
                }
            }
            Kind::Spot => {
                let (d, s, u) = Self::spot_frame(l);
                let c = p + d * l.range;
                let outer = Self::cap_radius(l.range, l.angle * 0.5);
                let inner = Self::cap_radius(l.range, l.inner_angle() * 0.5);
                Self::polyline(ui, &Self::ring(cam, c, s, u, outer), w, tint(1.0));
                Self::polyline(ui, &Self::ring(cam, c, s, u, inner), w * 0.8, tint(0.7));
                // Axe et quatre arêtes du cône.
                if let (Some(o), Some(e)) = (cam.project(p), cam.project(c)) {
                    ui.line(o.0, o.1, e.0, e.1, w * 0.6, tint(0.8));
                    let r = 3.0 * line;
                    ui.rect(e.0 - r, e.1 - r, 2.0 * r, 2.0 * r, tint(1.0));
                }
                for k in 0..4 {
                    let ang = k as f32 * std::f32::consts::FRAC_PI_2;
                    let q = c + (s * ang.cos() + u * ang.sin()) * outer;
                    if let (Some(o), Some(e)) = (cam.project(p), cam.project(q)) {
                        ui.line(o.0, o.1, e.0, e.1, w * 0.6, tint(0.85));
                    }
                }
            }
        }
    }

    /// Poignée sous la souris : flèches, centre, direction, puis guides (bords, portée).
    fn pick_handle(&self, l: &PlacedLight, cam: &Cam, m: (f32, f32)) -> Option<Drag> {
        let (ts, _, _) = Self::metrics(cam.size.1);
        let tol = 7.0 * ts;
        let p = l.pos();
        let o = cam.project(p)?;
        let len = Self::gizmo_len(cam, p);
        let near = |q: (f32, f32), r: f32| ((m.0 - q.0).powi(2) + (m.1 - q.1).powi(2)).sqrt() < r;
        if l.kind == Kind::Spot {
            if let Some(e) = cam.project(p + Vec3::from(l.direction) * len * 1.7) {
                if near(e, tol * 1.2) {
                    return Some(Drag::Direction);
                }
            }
        }
        for a in 0..3 {
            let mut dir = Vec3::ZERO;
            dir[a] = 1.0;
            if let Some(e) = cam.project(p + dir * len) {
                if near(e, tol) || Self::dist_polyline(&[Some(o), Some(e)], m) < tol * 0.5 {
                    // Le centre passe avant les flèches qui en partent.
                    if !near(o, tol * 0.8) {
                        return Some(Drag::Axis(a));
                    }
                }
            }
        }
        if near(o, tol) {
            return Some(Drag::Plane);
        }
        match l.kind {
            Kind::Point => {
                let rings: Vec<_> = [(Vec3::X, Vec3::Y), (Vec3::X, Vec3::Z), (Vec3::Y, Vec3::Z)]
                    .iter()
                    .map(|(u, v)| Self::ring(cam, p, *u, *v, l.radius))
                    .collect();
                if rings.iter().any(|r| Self::dist_polyline(r, m) < tol * 0.8) {
                    return Some(Drag::Range);
                }
            }
            Kind::Spot => {
                let (d, s, u) = Self::spot_frame(l);
                let c = p + d * l.range;
                if let Some(e) = cam.project(c) {
                    if near(e, tol * 1.2) {
                        return Some(Drag::Range);
                    }
                }
                let outer = Self::ring(cam, c, s, u, Self::cap_radius(l.range, l.angle * 0.5));
                if Self::dist_polyline(&outer, m) < tol * 0.8 {
                    return Some(Drag::Outer);
                }
                let inner = Self::ring(
                    cam,
                    c,
                    s,
                    u,
                    Self::cap_radius(l.range, l.inner_angle() * 0.5),
                );
                if Self::dist_polyline(&inner, m) < tol * 0.8 {
                    return Some(Drag::Inner);
                }
            }
        }
        None
    }

    fn start_drag(&mut self, d: Drag, m: (f32, f32), l: &PlacedLight) {
        self.drag = Some(d);
        self.drag_mouse = m;
        self.drag_value = l.position;
    }

    /// Applique le geste en cours à la lumière ; renvoie vrai si elle a changé.
    fn apply_drag(
        &mut self,
        d: Drag,
        l: &mut PlacedLight,
        cam: &Cam,
        m: (f32, f32),
        input: &Input,
    ) -> bool {
        let snap = input.held(KeyCode::ControlLeft) || input.held(KeyCode::ControlRight);
        let start = Vec3::new(
            self.drag_value[0] as f32,
            self.drag_value[1] as f32,
            self.drag_value[2] as f32,
        );
        let ray = cam.ray(m);
        let before = l.clone();
        match d {
            Drag::Axis(a) => {
                let len = Self::gizmo_len(cam, start);
                let mut dir = Vec3::ZERO;
                dir[a] = 1.0;
                let (Some(o), Some(e)) = (cam.project(start), cam.project(start + dir * len))
                else {
                    return false;
                };
                let (ax, ay) = (e.0 - o.0, e.1 - o.1);
                let px = (ax * ax + ay * ay).sqrt();
                if px < 2.0 {
                    return false;
                }
                let along = ((m.0 - self.drag_mouse.0) * ax + (m.1 - self.drag_mouse.1) * ay) / px;
                let mut delta = along / px * len;
                if snap {
                    delta = (delta * 2.0).round() / 2.0;
                }
                let mut pos = self.drag_value;
                pos[a] += delta as f64;
                if snap {
                    pos[a] = (pos[a] * 2.0).round() / 2.0;
                }
                l.position = pos;
            }
            Drag::Plane => {
                // Plan face à la caméra, passant par la lumière.
                let n = cam.look;
                let denom = ray.dot(n);
                if denom.abs() < 1e-4 {
                    return false;
                }
                let t = (start - cam.pos).dot(n) / denom;
                if t <= 0.0 {
                    return false;
                }
                let start_ray = cam.ray(self.drag_mouse);
                let t0 = (start - cam.pos).dot(n) / start_ray.dot(n);
                let delta = (cam.pos + ray * t) - (cam.pos + start_ray * t0);
                let mut pos = [
                    self.drag_value[0] + delta.x as f64,
                    self.drag_value[1] + delta.y as f64,
                    self.drag_value[2] + delta.z as f64,
                ];
                if snap {
                    for v in &mut pos {
                        *v = (*v * 2.0).round() / 2.0;
                    }
                }
                l.position = pos;
            }
            Drag::Direction => {
                // La poignée suit la souris sur une sphère autour de la lumière.
                let r = Self::gizmo_len(cam, start) * 1.7;
                let oc = cam.pos - start;
                let b = oc.dot(ray);
                let c = oc.dot(oc) - r * r;
                let disc = b * b - c;
                let hit = if disc >= 0.0 {
                    cam.pos + ray * (-b - disc.sqrt())
                } else {
                    // À côté de la sphère : le point du rayon le plus proche.
                    cam.pos + ray * (-b).max(0.0)
                };
                let dir = (hit - start).normalize_or(Vec3::NEG_Y);
                l.direction = dir.to_array();
            }
            Drag::Outer | Drag::Inner if l.kind == Kind::Spot => {
                let (dd, _, _) = Self::spot_frame(l);
                let c = start + dd * l.range;
                let denom = ray.dot(dd);
                if denom.abs() < 1e-4 {
                    return false;
                }
                let t = (c - cam.pos).dot(dd) / denom;
                if t <= 0.0 {
                    return false;
                }
                let q = cam.pos + ray * t;
                let half = ((q - c).length() / l.range.max(0.01)).atan().to_degrees();
                let full = (half * 2.0).clamp(1.0, 179.0).round();
                if d == Drag::Outer {
                    let inner = l.inner_angle();
                    l.angle = full.max(1.0);
                    l.edge = (l.angle - inner.min(l.angle)).clamp(0.0, 60.0);
                } else {
                    l.edge = (l.angle - full.min(l.angle)).clamp(0.0, 60.0).round();
                }
            }
            Drag::Range => {
                if l.kind == Kind::Spot {
                    // Point de l'axe le plus proche du rayon de la souris.
                    let (dd, _, _) = Self::spot_frame(l);
                    let w0 = start - cam.pos;
                    let a = dd.dot(dd);
                    let b = dd.dot(ray);
                    let c = ray.dot(ray);
                    let dn = a * c - b * b;
                    if dn.abs() < 1e-6 {
                        return false;
                    }
                    let s = (b * ray.dot(w0) - c * dd.dot(w0)) / dn;
                    l.range = (s.clamp(0.1, 128.0) * 4.0).round() / 4.0;
                } else {
                    // Rayon : distance de la lumière au rayon de la souris.
                    let w0 = start - cam.pos;
                    let t = w0.dot(ray).max(0.0);
                    let r = (cam.pos + ray * t - start).length();
                    l.radius = (r.clamp(0.1, 64.0) * 4.0).round() / 4.0;
                }
            }
            _ => return false,
        }
        *l != before
    }

    // ------------------------------------------------------------ le panneau

    #[allow(clippy::too_many_arguments)]
    fn panel(
        &mut self,
        ui: &mut Ui,
        input: &Input,
        lights: &mut Lights,
        cam: &Cam,
        stats: DynStats,
        (px, py, pw, ph): (f32, f32, f32, f32),
    ) {
        let (ts, row, pad) = Self::metrics(cam.size.1);
        ui.rect(px, py, pw, ph, PANEL_BG);
        ui.rect(px, py, pw, 2.0 * ts, FILL);
        let cursor = if self.warn_auto { None } else { input.cursor };
        let mut p = P {
            ui,
            cursor,
            click: input.mouse_pressed(Mouse::Left) && !input.grabbed,
            held: input.mouse_held(Mouse::Left),
            wheel: if cursor.is_some() { input.scroll } else { 0.0 },
            ctrl: input.held(KeyCode::ControlLeft) || input.held(KeyCode::ControlRight),
            ts,
            row,
            pad,
            x: px + pad * 2.0,
            w: pw - pad * 4.0,
            y: py + pad * 2.0,
            clip: (py, py + ph),
        };
        // Titre, fermer. (v215) Les réglages de toutes les lumières sont dans Paramètres
        // graphiques → Lumières (`settings_screen`) : ici, chaque lumière.
        {
            let y = p.y;
            p.text(p.x, y + (row - 8.0 * ts) / 2.0, WHITE, "LUMIÈRES");
            let bw = 18.0 * ts;
            let close_x = p.x + p.w - bw;
            if p.button_at(close_x, bw, "x", false, true) {
                self.close();
                return;
            }
            p.y += row + pad;
        }
        // Liste des lumières.
        p.section(&format!("{} ({})", tr("SOURCES"), lights.scene.len()));
        let visible = 5usize;
        let list_y = p.y;
        let list_h = row * visible as f32;
        let bar_w = 4.0 * ts;
        p.ui.rect(p.x, list_y, p.w, list_h, [0.0, 0.0, 0.0, 0.35]);
        let rows = lights.scene.len();
        if p.hovered(p.x, list_y, p.w, list_h) && p.wheel.abs() >= 1.0 {
            self.list_scroll = if p.wheel > 0.0 {
                self.list_scroll.saturating_sub(1)
            } else {
                self.list_scroll + 1
            };
        }
        self.list_scroll = self.list_scroll.min(rows.saturating_sub(visible));
        if rows == 0 {
            p.text(p.x + pad, list_y + pad, DIM, "Aucune lumière.");
            p.text(
                p.x + pad,
                list_y + pad + row,
                DIM,
                "Ajoute-en une ci-dessous.",
            );
        }
        let mut toggle_on = None;
        for (k, l) in lights
            .scene
            .iter()
            .enumerate()
            .skip(self.list_scroll)
            .take(visible)
        {
            let y = list_y + (k - self.list_scroll) as f32 * row;
            let w = p.w - bar_w - pad;
            let hover = p.hovered(p.x, y, w, row);
            if self.selected == Some(k) {
                p.ui.rect(p.x, y, w, row, SELECT);
            } else if hover {
                p.ui.rect(p.x, y, w, row, [1.0, 1.0, 1.0, 0.06]);
            }
            // Pastille de couleur, sorte, nom, interrupteur.
            let sw = 6.0 * ts;
            let c = [
                l.color[0],
                l.color[1],
                l.color[2],
                if l.on { 1.0 } else { 0.3 },
            ];
            p.ui.rect(p.x + pad, y + (row - sw) / 2.0, sw, sw, c);
            let icon = match l.kind {
                Kind::Point => "o",
                Kind::Spot => "v",
            };
            let ty = y + (row - 8.0 * ts) / 2.0;
            p.ui.text(p.x + pad * 2.0 + sw, ty, ts, GREY, icon);
            let name_x = p.x + pad * 3.0 + sw + 8.0 * ts;
            let eye_w = 12.0 * ts;
            let name = fit(&l.name, w - (name_x - p.x) - eye_w - pad, ts);
            p.ui.text(name_x, ty, ts, if l.on { WHITE } else { DIM }, name);
            let ex = p.x + w - eye_w;
            let eye_hover = p.hovered(ex, y, eye_w, row);
            p.ui.text(
                ex + (eye_w - 8.0 * ts) / 2.0,
                ty,
                ts,
                if eye_hover {
                    GOLD
                } else if l.on {
                    WHITE
                } else {
                    DIM
                },
                if l.on { "*" } else { "-" },
            );
            if p.click && eye_hover {
                toggle_on = Some(k);
            } else if p.click && hover {
                self.selected = Some(k);
                self.editing_name = false;
            }
        }
        if let Some(k) = toggle_on {
            lights.scene[k].on = !lights.scene[k].on;
            lights.touch_scene();
        }
        // Barre de défilement.
        if rows > visible {
            let bx = p.x + p.w - bar_w;
            p.ui.rect(bx, list_y, bar_w, list_h, [0.04, 0.04, 0.05, 1.0]);
            let shown = visible as f32 / rows as f32;
            let start = self.list_scroll as f32 / rows as f32;
            p.ui.rect(
                bx,
                list_y + list_h * start,
                bar_w,
                (list_h * shown).max(4.0 * ts),
                [0.6, 0.6, 0.66, 1.0],
            );
            if p.click && p.hovered(bx - 2.0 * ts, list_y, bar_w + 4.0 * ts, list_h) {
                self.drag = Some(Drag::ListBar);
            }
            if self.drag == Some(Drag::ListBar) && p.held {
                if let Some((_, my)) = p.cursor {
                    let f = ((my - list_y) / list_h).clamp(0.0, 1.0);
                    self.list_scroll = ((f * rows as f32 - visible as f32 / 2.0).round().max(0.0)
                        as usize)
                        .min(rows - visible);
                }
            }
        }
        p.y = list_y + list_h + pad;
        match p.buttons(&["+ Ponctuelle", "+ Projecteur"], None) {
            Some(0) => self.add(lights, Kind::Point, cam),
            Some(1) => self.add(lights, Kind::Spot, cam),
            _ => {}
        }
        let has_sel = self.selected.is_some();
        {
            let gap = pad * 0.5;
            let bw = (p.w - gap) / 2.0;
            let x0 = p.x;
            if p.button_at(x0, bw, "Dupliquer", false, has_sel) {
                self.duplicate(lights);
            }
            if p.button_at(x0 + bw + gap, bw, "Supprimer", false, has_sel) {
                self.delete(lights);
            }
            p.y += row;
        }
        p.y += pad;
        p.ui.rect(p.x, p.y, p.w, ts, [1.0, 1.0, 1.0, 0.08]);
        p.y += pad * 2.0;
        // Bas du panneau : état des ombres.
        let foot = 2.0 * row;
        let body_end = py + ph - foot - pad;
        if let Some(i) = self.selected.filter(|&i| i < lights.scene.len()) {
            self.light_body(&mut p, input, lights, i, cam, body_end);
        } else {
            p.note(
                "Choisis une lumière dans la liste, ou clique sur sa marque dans le monde.",
                DIM,
            );
        }
        // Pied : ombres et lumières automatiques.
        let fy = py + ph - foot;
        p.ui.rect(px, fy - pad * 0.5, pw, ts, [1.0, 1.0, 1.0, 0.06]);
        // Ombres prêtes / ombres voulues (les autres attendent leur calcul). Leurs shaders
        // sont compilés la première fois qu'une lumière est posée : quelques secondes.
        let (line1, color1) = match stats.state {
            DynState::Compiling => (tr("Préparation des lumières…"), GOLD),
            DynState::Failed => (tr("Lumières indisponibles"), RED),
            _ => (
                format!(
                    "{} : {}   {} : {}/{}",
                    tr("Lumières"),
                    stats.lights,
                    tr("Ombres"),
                    stats.shadowed,
                    stats.shadowed + stats.waiting
                ),
                GREY,
            ),
        };
        let line2 = if stats.state == DynState::Failed {
            "Carte graphique : voir le journal".to_string()
        } else if stats.tiles_total > 0 {
            format!(
                "{} : {} / {}{}",
                tr("Cases d'ombre"),
                stats.tiles_used,
                stats.tiles_total,
                if lights.settings.auto || lights.point_torches {
                    format!("   auto : {}", lights.auto_active())
                } else {
                    String::new()
                }
            )
        } else if lights.settings.auto || lights.point_torches {
            format!("Lumières automatiques : {}", lights.auto_active())
        } else {
            "Minelib — d'après IRLights (qualet)".to_string()
        };
        let x = p.x;
        p.ui.text(x, fy + pad * 0.5, ts, color1, fit(&line1, p.w, ts));
        p.ui.text(x, fy + pad * 0.5 + row * 0.9, ts, DIM, fit(&line2, p.w, ts));
    }

    /// Curseur (ligne « libellé … valeur ») ; `curve` > 1 : plus de précision pour les
    /// petites valeurs. Renvoie la nouvelle valeur si elle a changé.
    #[allow(clippy::too_many_arguments)]
    fn slider(
        &mut self,
        p: &mut P,
        id: u32,
        label: &str,
        v: f32,
        lo: f32,
        hi: f32,
        curve: f32,
        text: &str,
    ) -> Option<f32> {
        let mut out = None;
        if p.visible(p.row) {
            let lw = p.w * LABEL;
            let (bx, by, bw, bh) = (p.x + lw, p.y + p.pad * 0.25, p.w - lw, p.row - p.pad);
            let ty = p.y + (p.row - 8.0 * p.ts) / 2.0;
            let x = p.x;
            p.text(x, ty, GREY, &fit(label, lw - p.pad, p.ts));
            let hover = p.hovered(bx, by, bw, bh);
            let active = self.drag == Some(Drag::Widget(id));
            p.ui.rect(
                bx,
                by,
                bw,
                bh,
                if hover || active { ROW_HOVER } else { ROW_BG },
            );
            let frac = ((v - lo) / (hi - lo)).clamp(0.0, 1.0).powf(1.0 / curve);
            p.ui.rect(bx, by, bw * frac, bh, [FILL[0], FILL[1], FILL[2], 0.55]);
            let text = fit(text, bw - 2.0 * p.ts, p.ts);
            let tw = Ui::text_width(&text, p.ts);
            p.ui.text(bx + (bw - tw) / 2.0, ty, p.ts, WHITE, text);
            if hover && p.click {
                self.drag = Some(Drag::Widget(id));
            }
            if (self.drag == Some(Drag::Widget(id))) && p.held {
                if let Some((mx, _)) = p.cursor {
                    let f = ((mx - bx) / bw).clamp(0.0, 1.0);
                    let nv = lo + (hi - lo) * f.powf(curve);
                    if (nv - v).abs() > f32::EPSILON {
                        out = Some(nv);
                    }
                }
            } else if hover && p.wheel.abs() >= 1.0 {
                // Molette : un centième de la course (Ctrl : un millième).
                let step = (hi - lo) / if p.ctrl { 1000.0 } else { 100.0 };
                out = Some((v + step * p.wheel.signum()).clamp(lo, hi));
            }
        }
        p.y += p.row;
        out
    }

    /// Case à cocher (ligne entière cliquable).
    fn check(&mut self, p: &mut P, label: &str, v: &mut bool) -> bool {
        let mut changed = false;
        if p.visible(p.row) {
            let hover = p.hovered(p.x, p.y, p.w, p.row);
            if hover {
                p.ui.rect(p.x, p.y, p.w, p.row, [1.0, 1.0, 1.0, 0.05]);
            }
            let ty = p.y + (p.row - 8.0 * p.ts) / 2.0;
            let x = p.x;
            p.text(
                x,
                ty,
                if *v { WHITE } else { GREY },
                &fit(label, p.w - 12.0 * p.ts, p.ts),
            );
            let b = 7.0 * p.ts;
            let bx = p.x + p.w - b - p.pad;
            let by = p.y + (p.row - b) / 2.0;
            p.ui.rect(bx, by, b, b, ROW_BG);
            if *v {
                p.ui.rect(
                    bx + p.ts * 1.5,
                    by + p.ts * 1.5,
                    b - p.ts * 3.0,
                    b - p.ts * 3.0,
                    FILL,
                );
            }
            if hover && p.click {
                *v = !*v;
                changed = true;
            }
        }
        p.y += p.row;
        changed
    }

    /// Valeur qu'on règle en glissant de gauche à droite (position) ; `speed` : unités par
    /// pixel. La molette ajoute ±0,1 (Ctrl : ±1).
    fn drag_value(&mut self, p: &mut P, id: u32, label: &str, v: f64, speed: f64) -> Option<f64> {
        let mut out = None;
        if p.visible(p.row) {
            let lw = 14.0 * p.ts;
            let (bx, by, bw, bh) = (p.x + lw, p.y + p.pad * 0.25, p.w - lw, p.row - p.pad);
            let ty = p.y + (p.row - 8.0 * p.ts) / 2.0;
            let x = p.x;
            p.text(x, ty, GREY, label);
            let hover = p.hovered(bx, by, bw, bh);
            let active = self.drag == Some(Drag::Widget(id));
            p.ui.rect(
                bx,
                by,
                bw,
                bh,
                if hover || active { ROW_HOVER } else { ROW_BG },
            );
            let t = format!("{v:.2}");
            let tw = Ui::text_width(&t, p.ts);
            p.ui.text(bx + (bw - tw) / 2.0, ty, p.ts, WHITE, t);
            p.ui.text(bx + p.pad, ty, p.ts, DIM, "<");
            p.ui.text(bx + bw - p.pad - 8.0 * p.ts, ty, p.ts, DIM, ">");
            if hover && p.click {
                self.drag = Some(Drag::Widget(id));
                if let Some(m) = p.cursor {
                    self.drag_mouse = m;
                }
                self.drag_value[0] = v;
            }
            if self.drag == Some(Drag::Widget(id)) && p.held {
                if let Some((mx, _)) = p.cursor {
                    let k = if p.ctrl { 0.1 } else { 1.0 };
                    let nv = self.drag_value[0] + (mx - self.drag_mouse.0) as f64 * speed * k;
                    if (nv - v).abs() > 1e-9 {
                        out = Some(nv);
                    }
                }
            } else if hover && p.wheel.abs() >= 1.0 {
                let step = if p.ctrl { 1.0 } else { 0.1 };
                out = Some(v + step * p.wheel.signum() as f64);
            }
        }
        p.y += p.row;
        out
    }

    #[allow(clippy::too_many_arguments)]
    fn light_body(
        &mut self,
        p: &mut P,
        input: &Input,
        lights: &mut Lights,
        i: usize,
        cam: &Cam,
        end: f32,
    ) {
        let ts = p.ts;
        let row = p.row;
        // En-tête : nom (clic pour le changer) et « Réinitialiser ».
        {
            let rw = Ui::text_width("Réinitialiser", ts) + 6.0 * ts;
            let nw = p.w - rw - p.pad;
            let (x, y) = (p.x, p.y);
            let hover = p.hovered(x, y, nw, row);
            p.ui.rect(
                x,
                y,
                nw,
                row - p.pad * 0.5,
                if self.editing_name {
                    ROW_HOVER
                } else {
                    [0.0; 4]
                },
            );
            if self.editing_name {
                crate::menu::edit_text(&mut lights.scene[i].name, input, 40);
                if input.pressed(KeyCode::Enter) || input.pressed(KeyCode::NumpadEnter) {
                    self.editing_name = false;
                }
                lights.touch_scene();
            }
            let name = lights.scene[i].name.clone();
            let shown = if self.editing_name {
                format!("{name}_")
            } else {
                name
            };
            p.ui.text(
                x + p.pad,
                y + (row - 8.0 * ts) / 2.0,
                ts,
                GOLD,
                fit(&shown, nw - 2.0 * p.pad, ts),
            );
            if hover && p.click {
                self.editing_name = true;
            } else if p.click && !hover {
                self.editing_name = false;
            }
            if p.button_at(x + nw + p.pad, rw, "Réinitialiser", false, true) {
                lights.scene[i].reset_look();
                lights.touch_scene();
            }
            p.y += row;
        }
        // Onglets.
        let spot = lights.scene[i].kind == Kind::Spot;
        let tabs: &[(&str, Tab)] = if spot {
            &[
                ("Placement", Tab::Placement),
                ("Base", Tab::Base),
                ("Rayons", Tab::Volume),
                ("Ombres", Tab::Shadows),
                ("Gobo", Tab::Gobo),
            ]
        } else {
            &[
                ("Placement", Tab::Placement),
                ("Base", Tab::Base),
                ("Rayons", Tab::Volume),
                ("Ombres", Tab::Shadows),
            ]
        };
        if !spot && self.tab == Tab::Gobo {
            self.tab = Tab::Base;
        }
        let labels: Vec<&str> = tabs.iter().map(|t| t.0).collect();
        let cur = tabs.iter().position(|t| t.1 == self.tab);
        if let Some(k) = p.buttons(&labels, cur) {
            self.tab = tabs[k].1;
        }
        p.y += p.pad;
        p.clip.1 = end;
        let mut changed = false;
        let l = &mut lights.scene[i];
        match self.tab {
            Tab::Placement => {
                for (a, name) in ["X", "Y", "Z"].iter().enumerate() {
                    if let Some(v) = self.drag_value(p, 100 + a as u32, name, l.position[a], 0.02) {
                        l.position[a] = v;
                        changed = true;
                    }
                }
                if p.buttons(&["Amener ici"], None).is_some() {
                    l.position = [cam.pos.x as f64, cam.pos.y as f64, cam.pos.z as f64];
                    changed = true;
                }
                if spot {
                    let d = Vec3::from(l.direction);
                    p.note(
                        &format!("Direction : {:.2} / {:.2} / {:.2}", d.x, d.y, d.z),
                        DIM,
                    );
                    // Direction en lacet / tangage (degrés).
                    let yaw = d.x.atan2(-d.z).to_degrees().rem_euclid(360.0);
                    let pitch = d.y.clamp(-1.0, 1.0).asin().to_degrees();
                    let mut dir_changed = None;
                    if let Some(v) =
                        self.slider(p, 110, "Cap", yaw, 0.0, 360.0, 1.0, &fmt(yaw, 0, "°"))
                    {
                        dir_changed = Some((v, pitch));
                    }
                    if let Some(v) = self.slider(
                        p,
                        111,
                        "Inclinaison",
                        pitch,
                        -90.0,
                        90.0,
                        1.0,
                        &fmt(pitch, 0, "°"),
                    ) {
                        dir_changed = Some((yaw, v));
                    }
                    if let Some((y, pt)) = dir_changed {
                        let (y, pt) = (y.to_radians(), pt.clamp(-89.9, 89.9).to_radians());
                        l.direction = [y.sin() * pt.cos(), pt.sin(), -y.cos() * pt.cos()];
                        changed = true;
                    }
                    if p.buttons(&["Viser où je regarde"], None).is_some() {
                        l.direction = cam.look.normalize_or(Vec3::NEG_Y).to_array();
                        changed = true;
                    }
                }
                p.note(
                    "Glisse les flèches dans le monde pour déplacer la lumière (Ctrl : par demi-bloc).",
                    DIM,
                );
            }
            Tab::Base => {
                // Couleur : aperçu, couleurs proposées, teinte, saturation, valeur.
                if p.visible(row) {
                    let ty = p.y + (row - 8.0 * ts) / 2.0;
                    let x = p.x;
                    p.text(x, ty, GREY, "Couleur");
                    let sw = (p.w * (1.0 - LABEL) - p.pad * 7.0) / 8.0;
                    let x0 = p.x + p.w * LABEL;
                    for (k, c) in SWATCHES.iter().enumerate() {
                        let sx = x0 + k as f32 * (sw + p.pad);
                        let sy = p.y + p.pad * 0.5;
                        let hover = p.hovered(sx, sy, sw, row - p.pad);
                        p.ui.rect(sx, sy, sw, row - p.pad, [c[0], c[1], c[2], 1.0]);
                        if hover {
                            p.ui.rect(sx, sy + row - p.pad - ts, sw, ts, WHITE);
                        }
                        if hover && p.click {
                            l.color = *c;
                            changed = true;
                        }
                    }
                }
                p.y += row;
                let (h0, s0, v0) = rgb_to_hsv(l.color);
                if s0 > 0.01 && v0 > 0.01 {
                    self.hue = h0;
                }
                let (mut h, mut s, mut v) = (self.hue, s0, v0);
                let (mut hue_moved, mut sat_moved, mut val_moved) = (false, false, false);
                // Teinte : la barre montre l'arc-en-ciel (dessiné par-dessus le curseur), et
                // un trait blanc marque la teinte choisie.
                {
                    let y = p.y;
                    let shown = p.visible(row);
                    if let Some(nh) = self.slider(p, 120, "Teinte", h, 0.0, 1.0, 1.0, "") {
                        h = nh;
                        self.hue = nh;
                        hue_moved = true;
                    }
                    if shown {
                        let lw = p.w * LABEL;
                        let (bx, bw) = (p.x + lw, p.w - lw);
                        let (by, bh) = (y + p.pad * 0.25, row - p.pad);
                        let n = 24;
                        for k in 0..n {
                            let c = hsv_to_rgb(k as f32 / n as f32, 1.0, 1.0);
                            p.ui.rect(
                                bx + bw * k as f32 / n as f32,
                                by,
                                bw / n as f32 + 0.5,
                                bh,
                                [c[0], c[1], c[2], 1.0],
                            );
                        }
                        let mx = bx + bw * h.clamp(0.0, 1.0);
                        p.ui.rect(mx - ts, by - ts, 2.0 * ts, bh + 2.0 * ts, WHITE);
                    }
                }
                if let Some(ns) = self.slider(
                    p,
                    121,
                    "Saturation",
                    s,
                    0.0,
                    1.0,
                    1.0,
                    &fmt(s * 100.0, 0, " %"),
                ) {
                    s = ns;
                    sat_moved = true;
                }
                if let Some(nv) =
                    self.slider(p, 122, "Valeur", v, 0.0, 1.0, 1.0, &fmt(v * 100.0, 0, " %"))
                {
                    v = nv;
                    val_moved = true;
                }
                if hue_moved || sat_moved || val_moved {
                    l.color = color_from_sliders(h, s, v, hue_moved, sat_moved);
                    changed = true;
                }
                if let Some(v) = self.slider(
                    p,
                    123,
                    "Intensité",
                    l.intensity,
                    0.0,
                    20.0,
                    2.0,
                    &fmt(l.intensity, 2, ""),
                ) {
                    l.intensity = v;
                    changed = true;
                }
                if spot {
                    if let Some(v) = self.slider(
                        p,
                        124,
                        "Portée",
                        l.range,
                        0.1,
                        128.0,
                        2.0,
                        &blocks(l.range, 1),
                    ) {
                        l.range = v;
                        changed = true;
                    }
                    if let Some(v) = self.slider(
                        p,
                        125,
                        "Angle du faisceau",
                        l.angle,
                        1.0,
                        179.0,
                        1.0,
                        &fmt(l.angle, 0, "°"),
                    ) {
                        l.angle = v.round();
                        changed = true;
                    }
                    if let Some(v) = self.slider(
                        p,
                        126,
                        "Douceur du bord",
                        l.edge,
                        0.0,
                        60.0,
                        1.0,
                        &fmt(l.edge, 0, "°"),
                    ) {
                        l.edge = v.round();
                        changed = true;
                    }
                } else if let Some(v) = self.slider(
                    p,
                    127,
                    "Rayon",
                    l.radius,
                    0.1,
                    64.0,
                    2.0,
                    &blocks(l.radius, 1),
                ) {
                    l.radius = v;
                    changed = true;
                }
            }
            Tab::Volume => {
                changed |= self.check(p, "Rayons volumétriques", &mut l.volumetric);
                if l.volumetric {
                    if let Some(v) = self.slider(
                        p,
                        130,
                        "Force des rayons",
                        l.beam,
                        0.0,
                        5.0,
                        1.0,
                        &fmt(l.beam, 2, ""),
                    ) {
                        l.beam = v;
                        changed = true;
                    }
                    p.note("Réglage fin", DIM);
                    if let Some(v) = self.slider(
                        p,
                        131,
                        "Densité",
                        l.density,
                        0.005,
                        0.5,
                        2.0,
                        &fmt(l.density, 3, ""),
                    ) {
                        l.density = v;
                        changed = true;
                    }
                    if let Some(v) = self.slider(
                        p,
                        132,
                        "Directivité",
                        l.anisotropy,
                        -0.95,
                        0.95,
                        1.0,
                        &fmt(l.anisotropy, 2, ""),
                    ) {
                        l.anisotropy = v;
                        changed = true;
                    }
                    p.note(
                        "Directivité : positive, les rayons brillent face à la lumière ; négative, dos à elle.",
                        DIM,
                    );
                }
            }
            Tab::Shadows => {
                changed |= self.check(p, "Ombres", &mut l.shadows);
                if let Some(v) = self.slider(
                    p,
                    140,
                    "Douceur des ombres",
                    l.bulb,
                    0.0,
                    2.0,
                    1.5,
                    &if l.bulb <= 0.0 {
                        tr("réglage général")
                    } else {
                        fmt(l.bulb, 2, "")
                    },
                ) {
                    l.bulb = v;
                    changed = true;
                }
                if self.check(p, "Entités seulement", &mut l.entities_only) {
                    changed = true;
                    if l.entities_only {
                        l.blocks_only = false;
                    }
                }
                if self.check(p, "Blocs seulement", &mut l.blocks_only) {
                    changed = true;
                    if l.blocks_only {
                        l.entities_only = false;
                    }
                }
                p.note(
                    "Les ombres des blocs sont calculées dans la grille autour de la caméra (64 blocs de haut).",
                    DIM,
                );
            }
            Tab::Gobo => {
                changed |= self.gobo_tab(p, l, &lights.gobos);
                let bw = (p.w - p.pad * 0.5) / 2.0;
                let x0 = p.x;
                if p.visible(row) {
                    if p.button_at(x0, bw, "Actualiser", false, true) {
                        self.gobo_reload = true;
                    }
                    if p.button_at(x0 + bw + p.pad * 0.5, bw, "Dossier", false, true) {
                        self.gobo_folder = true;
                    }
                }
                p.y += row;
                let l = &mut lights.scene[i];
                let has = !l.gobo.is_empty();
                if has {
                    if let Some(v) = self.slider(
                        p,
                        150,
                        "Rotation",
                        l.gobo_rotation,
                        0.0,
                        360.0,
                        1.0,
                        &fmt(l.gobo_rotation, 0, "°"),
                    ) {
                        l.gobo_rotation = v.round();
                        changed = true;
                    }
                    if let Some(v) = self.slider(
                        p,
                        151,
                        "Échelle",
                        l.gobo_echelle,
                        0.1,
                        4.0,
                        1.5,
                        &fmt(l.gobo_echelle, 2, ""),
                    ) {
                        l.gobo_echelle = v;
                        changed = true;
                    }
                    changed |= self.check(p, "Inverser", &mut l.gobo_inverse);
                }
                p.note(
                    "Images PNG du dossier gobos (blanc : la lumière passe, noir : elle est arrêtée).",
                    DIM,
                );
            }
        }
        if changed {
            lights.scene[i].sanitize();
            lights.touch_scene();
        }
        if self.gobo_reload {
            self.gobo_reload = false;
            lights.reload_gobos();
        }
        if self.gobo_folder {
            self.gobo_folder = false;
            crate::open_folder(&lights.gobo_dir());
        }
    }

    /// Liste des gobos (« aucun » puis les motifs) ; renvoie vrai si le choix a changé.
    fn gobo_tab(&mut self, p: &mut P, l: &mut PlacedLight, gobos: &[lights::Gobo]) -> bool {
        let visible = 6usize;
        let rows = gobos.len() + 1;
        let (ts, row) = (p.ts, p.row);
        let y0 = p.y;
        let h = row * visible as f32;
        let mut changed = false;
        p.ui.rect(p.x, y0, p.w, h, [0.0, 0.0, 0.0, 0.35]);
        if p.hovered(p.x, y0, p.w, h) && p.wheel.abs() >= 1.0 {
            self.gobo_scroll = if p.wheel > 0.0 {
                self.gobo_scroll.saturating_sub(1)
            } else {
                self.gobo_scroll + 1
            };
        }
        self.gobo_scroll = self.gobo_scroll.min(rows.saturating_sub(visible));
        let bar_w = 4.0 * ts;
        for k in self.gobo_scroll..(self.gobo_scroll + visible).min(rows) {
            let y = y0 + (k - self.gobo_scroll) as f32 * row;
            let (name, label) = if k == 0 {
                (String::new(), "(aucun)".to_string())
            } else {
                let g = &gobos[k - 1];
                (
                    g.name.clone(),
                    if g.builtin {
                        tr(&g.name)
                    } else {
                        format!("{} ({})", g.name, tr("dossier"))
                    },
                )
            };
            let w = p.w - bar_w - p.pad;
            let hover = p.hovered(p.x, y, w, row);
            if l.gobo == name {
                p.ui.rect(p.x, y, w, row, SELECT);
            } else if hover {
                p.ui.rect(p.x, y, w, row, [1.0, 1.0, 1.0, 0.06]);
            }
            p.ui.text(
                p.x + p.pad,
                y + (row - 8.0 * ts) / 2.0,
                ts,
                WHITE,
                fit(&label, w - 2.0 * p.pad, ts),
            );
            if hover && p.click && l.gobo != name {
                l.gobo = name;
                changed = true;
            }
        }
        if rows > visible {
            let bx = p.x + p.w - bar_w;
            p.ui.rect(bx, y0, bar_w, h, [0.04, 0.04, 0.05, 1.0]);
            let shown = visible as f32 / rows as f32;
            let start = self.gobo_scroll as f32 / rows as f32;
            p.ui.rect(
                bx,
                y0 + h * start,
                bar_w,
                (h * shown).max(4.0 * ts),
                [0.6, 0.6, 0.66, 1.0],
            );
            if p.click && p.hovered(bx - 2.0 * ts, y0, bar_w + 4.0 * ts, h) {
                self.drag = Some(Drag::GoboBar);
            }
            if self.drag == Some(Drag::GoboBar) && p.held {
                if let Some((_, my)) = p.cursor {
                    let f = ((my - y0) / h).clamp(0.0, 1.0);
                    self.gobo_scroll = ((f * rows as f32 - visible as f32 / 2.0).round().max(0.0)
                        as usize)
                        .min(rows - visible);
                }
            }
        }
        p.y = y0 + h + p.pad;
        changed
    }

    /// (v215) Réglages de toutes les lumières (préréglages, rayons, ombres, surfaces,
    /// lumières automatiques, interface), seuls dans une fenêtre au milieu de l'écran :
    /// Échap → Paramètres graphiques → Lumières. Le panneau de l'éditeur (L) ne règle plus
    /// que chaque lumière. Échap ferme d'abord l'avertissement des lumières automatiques.
    /// Renvoie vrai quand la fenêtre se ferme (« Terminé », « x » ou Échap).
    pub fn settings_screen(
        &mut self,
        ui: &mut Ui,
        input: &Input,
        lights: &mut Lights,
        size: (f32, f32),
    ) -> bool {
        let (ts, row, pad) = Self::metrics(size.1);
        let w = (300.0 * ts).min(size.0 - pad * 4.0);
        let h = size.1 - row * 4.0;
        let (px, py) = ((size.0 - w) / 2.0, row * 2.0);
        if !input.mouse_held(Mouse::Left) {
            self.drag = None;
        }
        let mut done = false;
        if input.pressed(KeyCode::Escape) {
            if self.warn_auto {
                self.warn_auto = false;
            } else {
                done = true;
            }
        }
        ui.rect(px, py, w, h, PANEL_BG);
        ui.rect(px, py, w, 2.0 * ts, FILL);
        let cursor = if self.warn_auto { None } else { input.cursor };
        let mut p = P {
            ui,
            cursor,
            click: input.mouse_pressed(Mouse::Left) && !input.grabbed,
            held: input.mouse_held(Mouse::Left),
            wheel: if cursor.is_some() { input.scroll } else { 0.0 },
            ctrl: input.held(KeyCode::ControlLeft) || input.held(KeyCode::ControlRight),
            ts,
            row,
            pad,
            x: px + pad * 2.0,
            w: w - pad * 4.0,
            y: py + pad * 2.0,
            clip: (py, py + h),
        };
        {
            let y = p.y;
            p.text(
                p.x,
                y + (row - 8.0 * ts) / 2.0,
                WHITE,
                "Réglages des lumières",
            );
            let bw = 18.0 * ts;
            if p.button_at(p.x + p.w - bw, bw, "x", false, true) {
                done = true;
            }
            p.y += row + pad;
        }
        let foot_y = py + h - row - pad;
        self.settings_body(&mut p, input, lights, foot_y - pad);
        p.clip = (py, py + h);
        p.y = foot_y;
        let bw = (Ui::text_width("Terminé", ts) + 24.0 * ts).min(p.w);
        if p.button_at(px + (w - bw) / 2.0, bw, "Terminé", false, true) {
            done = true;
        }
        if self.warn_auto {
            self.auto_warning(ui, input, lights, size);
        }
        if done {
            self.warn_auto = false;
            self.drag = None;
        }
        done
    }

    /// Réglages généraux (préréglages, rayons, ombres, surfaces, lumières automatiques,
    /// interface).
    fn settings_body(&mut self, p: &mut P, input: &Input, lights: &mut Lights, end: f32) {
        let _ = input;
        let cats = [
            ("Préréglages", Cat::Presets),
            ("Rayons", Cat::Beams),
            ("Ombres", Cat::Shadows),
        ];
        let cats2 = [
            ("Surfaces", Cat::Surfaces),
            ("Auto", Cat::Auto),
            ("Interface", Cat::Interface),
        ];
        for set in [&cats, &cats2] {
            let labels: Vec<&str> = set.iter().map(|c| c.0).collect();
            let cur = set.iter().position(|c| c.1 == self.cat);
            if let Some(k) = p.buttons(&labels, cur) {
                self.cat = set[k].1;
                self.settings_scroll = 0.0;
            }
        }
        p.y += p.pad;
        // Défilement à la molette (le contenu peut dépasser).
        let top = p.y;
        if p.hovered(p.x, top, p.w, end - top) && p.wheel.abs() > 0.0 && self.drag.is_none() {
            self.settings_scroll = (self.settings_scroll - p.wheel * p.row).max(0.0);
        }
        p.y -= self.settings_scroll;
        p.clip = (top, end);
        let mut s = lights.settings;
        let mut r = s.render;
        let before = s;
        match self.cat {
            Cat::Presets => {
                p.section("Qualité");
                let q = lights::quality_preset(&r);
                if let Some(k) = p.buttons(&lights::QUALITY_NAMES, q) {
                    lights::apply_quality(&mut r, k);
                }
                if q.is_none() {
                    p.note("Réglages personnels.", DIM);
                }
                p.section("Style des rayons");
                let st = lights::style_preset(&r);
                if let Some(k) = p.buttons(&lights::STYLE_NAMES, st) {
                    lights::apply_style(&mut r, k);
                }
                if st.is_none() {
                    p.note("Réglages personnels.", DIM);
                }
                p.note(
                    "Les préréglages changent plusieurs réglages des rayons et des ombres d'un coup ; chacun reste modifiable dans son onglet.",
                    DIM,
                );
            }
            Cat::Beams => {
                self.check(p, "Rayons volumétriques", &mut r.volumetric);
                if let Some(v) = self.slider(
                    p,
                    200,
                    "Intensité",
                    r.vl_intensity,
                    0.0,
                    5.0,
                    1.5,
                    &fmt(r.vl_intensity, 2, ""),
                ) {
                    r.vl_intensity = v;
                }
                if let Some(v) = self.slider(
                    p,
                    201,
                    "Pas de la marche",
                    r.vl_steps as f32,
                    8.0,
                    96.0,
                    1.0,
                    &r.vl_steps.to_string(),
                ) {
                    r.vl_steps = v.round() as u32;
                }
                p.section("Résolution des rayons");
                if let Some(k) = p.buttons(&["Moitié", "Quart"], Some((r.vl_scale >= 4) as usize))
                {
                    r.vl_scale = if k == 1 { 4 } else { 2 };
                }
                p.note(
                    "Au quart, quatre fois moins de calcul pour une image presque identique (les rayons sont flous par nature).",
                    DIM,
                );
                if let Some(v) = self.slider(
                    p,
                    202,
                    "Distance max",
                    r.vl_max_dist,
                    8.0,
                    256.0,
                    1.0,
                    &blocks(r.vl_max_dist, 0),
                ) {
                    r.vl_max_dist = v.round();
                }
                if let Some(v) = self.slider(
                    p,
                    203,
                    "Lueur à la source",
                    r.vl_tip_boost,
                    0.0,
                    4.0,
                    1.0,
                    &fmt(r.vl_tip_boost, 2, ""),
                ) {
                    r.vl_tip_boost = v;
                }
                if let Some(v) = self.slider(
                    p,
                    204,
                    "Rayon de la lueur",
                    r.vl_tip_radius,
                    0.1,
                    4.0,
                    1.0,
                    &blocks(r.vl_tip_radius, 2),
                ) {
                    r.vl_tip_radius = v;
                }
                self.check(p, "Ombres dans les rayons", &mut r.vl_shadows);
                if let Some(v) = self.slider(
                    p,
                    205,
                    "Pas des ombres",
                    r.vl_shadow_stride as f32,
                    1.0,
                    8.0,
                    1.0,
                    &r.vl_shadow_stride.to_string(),
                ) {
                    r.vl_shadow_stride = v.round() as u32;
                }
                self.check(p, "Bruit de densité", &mut r.vl_noise);
                if let Some(v) = self.slider(
                    p,
                    206,
                    "Part du bruit",
                    r.vl_noise_amount,
                    0.2,
                    1.0,
                    1.0,
                    &fmt(r.vl_noise_amount, 2, ""),
                ) {
                    r.vl_noise_amount = v;
                }
                if let Some(v) = self.slider(
                    p,
                    207,
                    "Taille du bruit",
                    r.vl_noise_scale,
                    0.25,
                    6.0,
                    1.0,
                    &blocks(r.vl_noise_scale, 2),
                ) {
                    r.vl_noise_scale = v;
                }
                if let Some(v) = self.slider(
                    p,
                    208,
                    "Vitesse de dérive",
                    r.vl_noise_speed,
                    0.0,
                    3.0,
                    1.0,
                    &fmt(r.vl_noise_speed, 2, ""),
                ) {
                    r.vl_noise_speed = (v * 4.0).round() / 4.0;
                }
                if let Some(v) = self.slider(
                    p,
                    209,
                    "Métamorphose",
                    r.vl_noise_morph,
                    0.0,
                    3.0,
                    1.0,
                    &fmt(r.vl_noise_morph, 2, ""),
                ) {
                    r.vl_noise_morph = (v * 4.0).round() / 4.0;
                }
                if let Some(v) = self.slider(
                    p,
                    210,
                    "Pas du bruit",
                    r.vl_noise_stride as f32,
                    1.0,
                    8.0,
                    1.0,
                    &r.vl_noise_stride.to_string(),
                ) {
                    r.vl_noise_stride = v.round() as u32;
                }
                self.check(p, "Tramage tournant", &mut r.vl_dither_temporal);
                p.note(
                    "Les vitesses vont par quart. Le tramage tournant est lissé par l'anticrénelage : sans lui, l'image grésille un peu.",
                    DIM,
                );
            }
            Cat::Shadows => {
                self.check(p, "Ombres des lumières", &mut r.shadows);
                p.section("Qualité des ombres");
                if let Some(k) = p.buttons(
                    &minelib_render::SHADOW_QUALITY_NAMES,
                    Some(r.shadow_quality as usize),
                ) {
                    r.shadow_quality = k as u8;
                }
                let (atlas, tile) = minelib_render::SHADOW_QUALITY[r.shadow_quality as usize];
                p.note(&format!("Taille d'une carte d'ombre : {tile} px"), DIM);
                p.note(
                    &format!(
                        "Mémoire vidéo : {} {}",
                        atlas as u64 * atlas as u64 * 4 / (1024 * 1024),
                        tr("Mo")
                    ),
                    DIM,
                );
                self.check(p, "Ombres des blocs", &mut r.block_shadows);
                p.note(
                    "Les murs arrêtent toujours la lumière ; décochées, les ombres des blocs sont nettes et bien moins coûteuses.",
                    DIM,
                );
                self.check(p, "Ombres des joueurs", &mut r.entity_shadows);
                // (v217) Jusqu'où les joueurs et les PNJ font leur ombre (soleil compris).
                if let Some(v) = self.slider(
                    p,
                    222,
                    "Ombres des joueurs jusqu'à",
                    s.player_shadow_distance,
                    lights::PLAYER_SHADOW_MIN,
                    lights::PLAYER_SHADOW_MAX,
                    1.0,
                    &blocks(s.player_shadow_distance, 0),
                ) {
                    s.player_shadow_distance = v.round();
                }
                p.note(
                    "Au soleil comme aux lumières, tous les joueurs et les PNJ jusqu'à cette distance font leur ombre.",
                    DIM,
                );
                self.check(p, "Cache des ombres", &mut r.shadow_cache);

                if let Some(v) = self.slider(
                    p,
                    220,
                    "Douceur (défaut)",
                    r.softness,
                    0.0,
                    0.8,
                    1.0,
                    &fmt(r.softness, 2, ""),
                ) {
                    r.softness = v;
                }
                if let Some(v) = self.slider(
                    p,
                    221,
                    "Recalculs par image",
                    r.bake_budget as f32,
                    1.0,
                    64.0,
                    1.5,
                    &r.bake_budget.to_string(),
                ) {
                    r.bake_budget = v.round() as u32;
                }
                p.note(
                    "Avec le cache, une carte d'ombre n'est recalculée que si sa lumière bouge ou si un bloc change près d'elle.",
                    DIM,
                );
            }
            Cat::Surfaces => {
                if let Some(v) = self.slider(
                    p,
                    230,
                    "Intensité générale",
                    r.intensity,
                    0.0,
                    4.0,
                    1.5,
                    &fmt(r.intensity, 2, ""),
                ) {
                    r.intensity = v;
                }
                if let Some(v) = self.slider(
                    p,
                    231,
                    "Brillance",
                    r.specular,
                    0.0,
                    4.0,
                    1.5,
                    &fmt(r.specular, 2, ""),
                ) {
                    r.specular = v;
                }
                self.check(p, "Éclairage en demi-résolution", &mut r.half_lighting);
                p.note(
                    "Un pixel sur quatre calculé, les autres repris de leurs voisins sauf aux bords des objets, des ombres et des reflets (calculés exactement) : même image, moins de calcul.",
                    DIM,
                );
                self.check(p, "Éclairage en bandes", &mut r.toon);
                if r.toon {
                    if let Some(v) = self.slider(
                        p,
                        232,
                        "Nombre de bandes",
                        r.toon_bands as f32,
                        2.0,
                        8.0,
                        1.0,
                        &r.toon_bands.to_string(),
                    ) {
                        r.toon_bands = v.round() as u32;
                    }
                    if let Some(v) = self.slider(
                        p,
                        233,
                        "Douceur des bandes",
                        r.toon_smooth,
                        0.0,
                        0.5,
                        1.0,
                        &fmt(r.toon_smooth, 2, ""),
                    ) {
                        r.toon_smooth = v;
                    }
                }
            }
            Cat::Auto => {
                let mut on = s.auto;
                if self.check(p, "Lumières automatiques", &mut on) {
                    if on && !s.auto_warned {
                        self.warn_auto = true;
                    }
                    s.auto = on;
                }
                p.note(
                    "Expérimental : une vraie lumière sur chaque torche, lanterne ou bloc lumineux proche.",
                    [1.0, 0.75, 0.3, 1.0],
                );
                // (v201) Le réglage vidéo les allume aussi, avec ces réglages.
                if lights.point_torches {
                    p.note(
                        "Allumées par « Torches ponctuelles » (paramètres graphiques).",
                        DIM,
                    );
                }
                self.check(p, "Ignorer les blocs enterrés", &mut s.auto_culling);
                self.check(p, "Ombres (les 8 plus proches)", &mut s.auto_shadows);
                if let Some(v) = self.slider(
                    p,
                    240,
                    "Luminosité",
                    s.auto_intensity,
                    0.0,
                    4.0,
                    1.5,
                    &fmt(s.auto_intensity, 2, ""),
                ) {
                    s.auto_intensity = v;
                }
                if let Some(v) = self.slider(
                    p,
                    241,
                    "Portée",
                    s.auto_reach,
                    0.25,
                    3.0,
                    1.0,
                    &fmt(s.auto_reach, 2, ""),
                ) {
                    s.auto_reach = v;
                }
                if let Some(v) = self.slider(
                    p,
                    242,
                    "Rayon de recherche",
                    s.auto_radius as f32,
                    8.0,
                    128.0,
                    1.0,
                    &blocks(s.auto_radius as f32, 0),
                ) {
                    s.auto_radius = v.round() as u32;
                }
                if let Some(v) = self.slider(
                    p,
                    243,
                    "Nombre maximal",
                    s.auto_max as f32,
                    1.0,
                    240.0,
                    1.5,
                    &s.auto_max.to_string(),
                ) {
                    s.auto_max = v.round() as u32;
                }
                p.note(
                    &format!(
                        "Actives / trouvées : {} / {}",
                        lights.auto_active(),
                        lights.auto_found
                    ),
                    GREY,
                );
            }
            Cat::Interface => {
                self.check(p, "Guides de toutes les lumières", &mut s.guides);
                p.note(
                    "Sans guides, seuls ceux de la lumière choisie sont dessinés.",
                    DIM,
                );
                if p.buttons(&["Dossier des gobos"], None).is_some() {
                    crate::open_folder(&lights.gobo_dir());
                }
            }
        }
        s.render = r;
        if s != before {
            lights.settings = s;
            lights.touch_settings();
        }
        // Le contenu ne défile pas au-delà de sa fin.
        let content = p.y + self.settings_scroll - top;
        let room = end - top;
        self.settings_scroll = self.settings_scroll.min((content - room).max(0.0));
    }

    /// Fenêtre « expérimental » à la première activation des lumières automatiques.
    fn auto_warning(&mut self, ui: &mut Ui, input: &Input, lights: &mut Lights, size: (f32, f32)) {
        let (ts, row, pad) = Self::metrics(size.1);
        let w = (260.0 * ts).min(size.0 * 0.8);
        let h = row * 6.0;
        let (x, y) = ((size.0 - w) / 2.0, (size.1 - h) / 2.0);
        ui.rect(0.0, 0.0, size.0, size.1, [0.0, 0.0, 0.0, 0.45]);
        ui.rect(x, y, w, h, PANEL_BG);
        ui.rect(x, y, w, 2.0 * ts, [1.0, 0.75, 0.3, 1.0]);
        let mut p = P {
            ui,
            cursor: input.cursor,
            click: input.mouse_pressed(Mouse::Left),
            held: input.mouse_held(Mouse::Left),
            wheel: 0.0,
            ctrl: false,
            ts,
            row,
            pad,
            x: x + pad * 3.0,
            w: w - pad * 6.0,
            y: y + pad * 3.0,
            clip: (0.0, size.1),
        };
        p.section("Expérimental !");
        p.note(
            "Les lumières automatiques sont expérimentales. Les performances ne sont pas garanties, surtout là où il y a beaucoup de blocs lumineux et avec leurs ombres.",
            WHITE,
        );
        if p.buttons(&["Compris"], None).is_some() {
            self.warn_auto = false;
            lights.settings.auto_warned = true;
            lights.touch_settings();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cam() -> Cam {
        let pos = Vec3::new(0.0, 70.0, 0.0);
        let look = Vec3::NEG_Z;
        let size = (1600.0, 900.0);
        let vp = Mat4::perspective_infinite_reverse_rh(1.2, size.0 / size.1, 0.05)
            * Mat4::look_to_rh(pos, look, Vec3::Y);
        Cam {
            pos,
            look,
            view_proj: vp,
            size,
        }
    }

    #[test]
    fn moving_the_hue_of_a_white_light_colors_it() {
        // Lumière neuve : blanche. La teinte seule la colore, en plein.
        let (_, s, v) = rgb_to_hsv([1.0, 1.0, 1.0]);
        let c = color_from_sliders(0.6, s, v, true, false);
        let (h, s, v) = rgb_to_hsv(c);
        assert!((h - 0.6).abs() < 1e-3 && s > 0.99 && v > 0.99, "{c:?}");
        // Une couleur pâle choisie exprès garde sa saturation.
        let c = color_from_sliders(0.3, 0.4, 1.0, true, false);
        assert!((rgb_to_hsv(c).1 - 0.4).abs() < 1e-3, "{c:?}");
        // Noire : la teinte ou la saturation la rallument ; la valeur seule, non.
        assert!(rgb_to_hsv(color_from_sliders(0.1, 0.5, 0.0, false, true)).2 > 0.99);
        assert_eq!(color_from_sliders(0.1, 0.5, 0.0, false, false), [0.0; 3]);
    }

    #[test]
    fn colors_go_to_hsv_and_back() {
        for c in [
            [1.0, 0.5, 0.25],
            [0.2, 0.9, 0.4],
            [0.3, 0.3, 0.9],
            [1.0, 1.0, 1.0],
            [0.0, 0.0, 0.0],
        ] {
            let (h, s, v) = rgb_to_hsv(c);
            let back = hsv_to_rgb(h, s, v);
            for k in 0..3 {
                assert!((back[k] - c[k]).abs() < 1e-4, "{c:?} → {back:?}");
            }
        }
    }

    #[test]
    fn the_mouse_ray_goes_through_what_it_points_at() {
        let c = cam();
        let p = Vec3::new(3.0, 72.0, -15.0);
        let s = c.project(p).unwrap();
        let r = c.ray(s);
        let want = (p - c.pos).normalize();
        assert!(r.distance(want) < 1e-3, "{r:?} {want:?}");
        // Derrière la caméra : pas de point à l'écran.
        assert!(c.project(Vec3::new(0.0, 70.0, 10.0)).is_none());
    }

    #[test]
    fn dragging_an_arrow_moves_the_light_along_its_axis_only() {
        let c = cam();
        let mut ed = Editor {
            open: true,
            selected: Some(0),
            ..Default::default()
        };
        let mut l = PlacedLight {
            position: [0.0, 70.0, -10.0],
            ..Default::default()
        };
        let o = c.project(l.pos()).unwrap();
        ed.start_drag(Drag::Axis(0), o, &l);
        // Souris 100 pixels vers la droite : la lumière va vers +X, et seulement en X.
        let input = Input::default();
        assert!(ed.apply_drag(Drag::Axis(0), &mut l, &c, (o.0 + 100.0, o.1), &input));
        assert!(l.position[0] > 0.5, "{:?}", l.position);
        assert_eq!((l.position[1], l.position[2]), (70.0, -10.0));
        // Le point sous la souris est bien là où la lumière est allée.
        let now = c.project(l.pos()).unwrap();
        assert!((now.0 - (o.0 + 100.0)).abs() < 2.0, "{now:?}");
    }

    #[test]
    fn dragging_the_center_keeps_the_light_under_the_mouse() {
        let c = cam();
        let mut ed = Editor::default();
        let mut l = PlacedLight {
            position: [2.0, 71.0, -12.0],
            ..Default::default()
        };
        let o = c.project(l.pos()).unwrap();
        ed.start_drag(Drag::Plane, o, &l);
        let target = (o.0 - 140.0, o.1 + 60.0);
        assert!(ed.apply_drag(Drag::Plane, &mut l, &c, target, &Input::default()));
        let now = c.project(l.pos()).unwrap();
        assert!((now.0 - target.0).abs() < 1.0 && (now.1 - target.1).abs() < 1.0);
        // Même distance au plan de la caméra.
        assert!((l.pos().z - -12.0).abs() < 1e-3);
    }

    #[test]
    fn spot_handles_change_angle_and_range() {
        let c = cam();
        let mut ed = Editor::default();
        // Projecteur vu de côté (un axe vers la caméra ne se tire pas : il se confond avec
        // le rayon de la souris).
        let mut l = PlacedLight {
            kind: Kind::Spot,
            position: [-6.0, 70.0, -14.0],
            direction: [1.0, 0.0, 0.0],
            range: 10.0,
            angle: 40.0,
            edge: 10.0,
            ..Default::default()
        };
        // Portée : la poignée du bout de l'axe, tirée plus loin le long de l'axe.
        let far = c.project(Vec3::new(-6.0 + 14.0, 70.0, -14.0)).unwrap();
        ed.start_drag(Drag::Range, far, &l);
        assert!(ed.apply_drag(Drag::Range, &mut l, &c, far, &Input::default()));
        assert!((l.range - 14.0).abs() < 0.3, "{}", l.range);
        // Bord extérieur : un point du plan du bout à 5 blocs de l'axe → 2·atan(5/14).
        let edge = c.project(Vec3::new(-6.0 + l.range, 75.0, -14.0)).unwrap();
        ed.start_drag(Drag::Outer, edge, &l);
        assert!(ed.apply_drag(Drag::Outer, &mut l, &c, edge, &Input::default()));
        let want = (2.0 * (5.0f32 / l.range).atan().to_degrees()).round();
        assert!((l.angle - want).abs() <= 1.0, "{} {want}", l.angle);
        // L'intérieur suit (même largeur de bord, jamais plus large que l'extérieur).
        assert!(l.inner_angle() <= l.angle);
        // Bord intérieur : 3 blocs de l'axe → douceur = extérieur − 2·atan(3/14).
        let inner = c.project(Vec3::new(-6.0 + l.range, 73.0, -14.0)).unwrap();
        ed.start_drag(Drag::Inner, inner, &l);
        assert!(ed.apply_drag(Drag::Inner, &mut l, &c, inner, &Input::default()));
        let want_inner = 2.0 * (3.0f32 / l.range).atan().to_degrees();
        assert!(
            (l.inner_angle() - want_inner).abs() <= 1.5,
            "{} {want_inner}",
            l.inner_angle()
        );
        // Direction : la poignée suit la souris autour de la lumière.
        let up = c.project(l.pos() + Vec3::Y * 3.0).unwrap();
        ed.start_drag(Drag::Direction, up, &l);
        assert!(ed.apply_drag(Drag::Direction, &mut l, &c, up, &Input::default()));
        assert!(Vec3::from(l.direction).y > 0.6, "{:?}", l.direction);
    }

    #[test]
    fn new_lights_are_named_and_placed_in_front() {
        let dir = std::env::temp_dir().join(format!("minelib-editeur-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut lights = Lights::load(&dir, &dir);
        let c = cam();
        let mut ed = Editor::default();
        ed.add(&mut lights, Kind::Point, &c);
        ed.add(&mut lights, Kind::Spot, &c);
        ed.add(&mut lights, Kind::Point, &c);
        let names: Vec<&str> = lights.scene.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, ["Lumière 1", "Projecteur 1", "Lumière 2"]);
        assert!(lights.scene[0].pos().distance(c.pos + c.look * 2.5) < 1e-3);
        assert_eq!(lights.scene[1].direction, [0.0, 0.0, -1.0]);
        // Dupliquer puis supprimer : la sélection suit.
        ed.selected = Some(0);
        ed.duplicate(&mut lights);
        assert_eq!(lights.scene[1].name, "Lumière 1 copie");
        assert_ne!(lights.scene[1].id, lights.scene[0].id);
        assert_eq!(ed.selected, Some(1));
        ed.delete(&mut lights);
        assert_eq!(lights.scene.len(), 3);
        assert_eq!(ed.selected, Some(1));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
