//! Petits éléments de menu en mode immédiat, dessinés avec les images du pack
//! (`gui/button`, `gui/panel_dark`, `gui/menu_background`…).

use minelib_platform::{Input, Mouse};
use minelib_render::Ui;

pub const WHITE: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
pub const GREY: [f32; 4] = [0.62, 0.64, 0.70, 1.0];
pub const GOLD: [f32; 4] = [1.0, 0.78, 0.25, 1.0];
pub const RED: [f32; 4] = [1.0, 0.45, 0.40, 1.0];

pub struct Ctx<'a> {
    pub ui: &'a mut Ui,
    pub cursor: Option<(f32, f32)>,
    pub click: bool,
    pub held: bool,
    pub w: f32,
    pub h: f32,
    /// Échelle de l'interface (pixels écran par pixel de texture).
    pub s: f32,
}

impl<'a> Ctx<'a> {
    pub fn new(ui: &'a mut Ui, input: &Input, size: (u32, u32)) -> Self {
        let (w, h) = (size.0 as f32, size.1 as f32);
        Self {
            ui,
            cursor: input.cursor,
            click: input.mouse_pressed(Mouse::Left),
            held: input.mouse_held(Mouse::Left),
            w,
            h,
            s: (h / 300.0).floor().clamp(1.0, 6.0),
        }
    }

    pub fn hovered(&self, x: f32, y: f32, w: f32, h: f32) -> bool {
        self.cursor.is_some_and(|(cx, cy)| cx >= x && cx < x + w && cy >= y && cy < y + h)
    }

    /// Fond de menu du pack, en mosaïque.
    pub fn background(&mut self) {
        let s = self.s * 2.0;
        self.ui.tiled("gui/menu_background", 0.0, 0.0, self.w, self.h, s, WHITE);
    }

    /// Voile sombre (menu par-dessus le jeu).
    pub fn dim(&mut self, alpha: f32) {
        self.ui.rect(0.0, 0.0, self.w, self.h, [0.0, 0.0, 0.0, alpha]);
    }

    pub fn text_center(&mut self, text: &str, y: f32, scale: f32, color: [f32; 4]) {
        let tw = Ui::text_width(text, scale);
        self.ui.text((self.w - tw) / 2.0, y, scale, color, text);
    }

    /// Bouton centré horizontalement en `cx`. Largeur en pixels de texture (×`s`).
    /// Renvoie vrai s'il vient d'être cliqué.
    pub fn button(&mut self, label: &str, cx: f32, y: f32, width: f32, enabled: bool) -> bool {
        let s = self.s;
        let (w, h) = (width * s, 20.0 * s);
        let x = cx - w / 2.0;
        let hover = enabled && self.hovered(x, y, w, h);
        let sprite = if !enabled {
            "gui/button_disabled"
        } else if hover {
            "gui/button_hover"
        } else {
            "gui/button"
        };
        self.ui.nine_slice(sprite, x, y, w, h, 3.0, s);
        let ts = s;
        let tw = Ui::text_width(label, ts);
        let color = if enabled { WHITE } else { GREY };
        self.ui.text(x + (w - tw) / 2.0, y + (h - 8.0 * ts) / 2.0, ts, color, label);
        hover && self.click
    }

    /// Curseur horizontal centré en `cx`. `frac` = position actuelle (0..1).
    /// `drag` mémorise le curseur tenu d'une image à l'autre (identifiant `id`).
    /// Renvoie la nouvelle position quand l'utilisateur le déplace.
    #[allow(clippy::too_many_arguments)]
    pub fn slider(&mut self, id: u32, drag: &mut Option<u32>, label: &str, frac: f32, cx: f32, y: f32, width: f32) -> Option<f32> {
        let s = self.s;
        let (w, h) = (width * s, 20.0 * s);
        let x = cx - w / 2.0;
        let hover = self.hovered(x, y, w, h);
        if hover && self.click {
            *drag = Some(id);
        }
        if !self.held && *drag == Some(id) {
            *drag = None;
        }
        let active = *drag == Some(id);
        self.ui.nine_slice("gui/button_disabled", x, y, w, h, 3.0, s);
        let knob = 8.0 * s;
        let mut f = frac.clamp(0.0, 1.0);
        let mut changed = None;
        if active {
            if let Some((mx, _)) = self.cursor {
                let nf = ((mx - x - knob / 2.0) / (w - knob)).clamp(0.0, 1.0);
                if (nf - f).abs() > 1e-6 {
                    f = nf;
                    changed = Some(nf);
                }
            }
        }
        let kx = x + f * (w - knob);
        let sprite = if hover || active { "gui/button_hover" } else { "gui/button" };
        self.ui.nine_slice(sprite, kx, y, knob, h, 3.0, s);
        let tw = Ui::text_width(label, s);
        self.ui.text(x + (w - tw) / 2.0, y + (h - 8.0 * s) / 2.0, s, WHITE, label);
        changed
    }

    /// Cadre sombre (listes, champs).
    pub fn frame(&mut self, x: f32, y: f32, w: f32, h: f32) {
        self.ui.nine_slice("gui/panel_dark", x, y, w, h, 4.0, self.s);
    }

    /// Champ de texte. Renvoie vrai s'il vient d'être cliqué (prise du focus).
    #[allow(clippy::too_many_arguments)]
    pub fn text_field(&mut self, label: &str, value: &str, placeholder: &str, cx: f32, y: f32, width: f32, focused: bool, time: f32) -> bool {
        let s = self.s;
        let (w, h) = (width * s, 20.0 * s);
        let x = cx - w / 2.0;
        self.ui.text(x, y - 11.0 * s, s, GREY, label);
        self.frame(x, y, w, h);
        if focused {
            self.ui.rect(x, y + h - s, w, s, GOLD);
        }
        let ty = y + (h - 8.0 * s) / 2.0;
        if value.is_empty() && !focused {
            self.ui.text(x + 5.0 * s, ty, s, GREY, placeholder);
        } else {
            self.ui.text(x + 5.0 * s, ty, s, WHITE, value);
            if focused && (time * 2.0).fract() < 0.5 {
                let cx = x + 5.0 * s + Ui::text_width(value, s);
                self.ui.rect(cx + s, ty, s, 8.0 * s, WHITE);
            }
        }
        self.hovered(x, y, w, h) && self.click
    }

    /// Ligne cliquable d'une liste. Renvoie vrai si cliquée.
    #[allow(clippy::too_many_arguments)]
    pub fn list_row(&mut self, x: f32, y: f32, w: f32, h: f32, title: &str, detail: &str, selected: bool) -> bool {
        let s = self.s;
        let hover = self.hovered(x, y, w, h);
        if selected {
            self.ui.rect(x, y, w, h, [0.85, 0.85, 0.9, 0.9]);
            self.ui.rect(x + s, y + s, w - 2.0 * s, h - 2.0 * s, [0.05, 0.05, 0.07, 0.95]);
        } else if hover {
            self.ui.rect(x, y, w, h, [1.0, 1.0, 1.0, 0.08]);
        }
        self.ui.text(x + 6.0 * s, y + 4.0 * s, s, WHITE, title);
        let ds = (s / 2.0).floor().max(1.0);
        self.ui.text(x + 6.0 * s, y + 15.0 * s, ds, GREY, detail);
        hover && self.click
    }
}

/// Applique la saisie clavier à un champ de texte (caractères tapés + effacement).
pub fn edit_text(value: &mut String, input: &Input, max_len: usize) {
    for c in input.text.chars() {
        if value.chars().count() < max_len {
            value.push(c);
        }
    }
    if input.pressed(minelib_platform::KeyCode::Backspace) {
        value.pop();
    }
}
