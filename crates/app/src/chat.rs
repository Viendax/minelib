//! Tchat façon Minecraft : `T` ou `/` pour écrire, Entrée pour envoyer, Échap pour fermer,
//! flèches haut/bas pour l'historique. Les messages restent affichés quelques secondes.
//! Un message peut porter un lien (capture d'écran) : tchat ouvert, un clic dessus le
//! signale (`clicked`).

use std::cell::RefCell;
use std::path::PathBuf;

use minelib_platform::{Input, KeyCode, Mouse};
use minelib_render::Ui;

const SHOWN_S: f32 = 10.0;
const FADE_S: f32 = 1.5;
const MAX_LINES: usize = 100;
const MAX_INPUT: usize = 100;

/// Zone cliquable : (x, y, largeur, hauteur, lien).
type Hit = (f32, f32, f32, f32, PathBuf);

pub struct Chat {
    pub open: bool,
    input: String,
    /// (texte, couleur, âge en secondes, lien éventuel)
    lines: Vec<(String, [f32; 4], f32, Option<PathBuf>)>,
    /// Zones cliquables affichées à la dernière image : (x, y, largeur, hauteur, lien).
    hits: RefCell<Vec<Hit>>,
    /// Lien cliqué pendant cette image.
    pub clicked: Option<PathBuf>,
    history: Vec<String>,
    history_pos: Option<usize>,
    time: f32,
}

pub const WHITE: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
pub const YELLOW: [f32; 4] = [1.0, 0.9, 0.35, 1.0];
pub const RED: [f32; 4] = [1.0, 0.45, 0.4, 1.0];
pub const GREY: [f32; 4] = [0.7, 0.72, 0.78, 1.0];

impl Default for Chat {
    fn default() -> Self {
        Self {
            open: false,
            input: String::new(),
            lines: Vec::new(),
            hits: RefCell::new(Vec::new()),
            clicked: None,
            history: Vec::new(),
            history_pos: None,
            time: 0.0,
        }
    }
}

impl Chat {
    pub fn say(&mut self, text: impl Into<String>, color: [f32; 4]) {
        self.push(text.into(), color, None);
    }

    /// Message avec un lien (clic, tchat ouvert : voir `clicked`).
    pub fn say_link(&mut self, text: impl Into<String>, color: [f32; 4], link: PathBuf) {
        self.push(text.into(), color, Some(link));
    }

    fn push(&mut self, text: String, color: [f32; 4], link: Option<PathBuf>) {
        self.lines.push((text, color, 0.0, link));
        if self.lines.len() > MAX_LINES {
            self.lines.remove(0);
        }
    }

    pub fn close(&mut self) {
        self.open = false;
        self.input.clear();
        self.history_pos = None;
    }

    /// Ouverture par la touche « Tchat » ou « Commande » (ou le caractère « / » tapé, quelle
    /// que soit la disposition du clavier). Le caractère produit par la touche d'ouverture
    /// n'est pas gardé ; ce qui est tapé ensuite dans la même image l'est.
    /// Renvoie le message si Entrée a aussi été pressée dans la même image (frappe rapide).
    pub fn try_open(&mut self, input: &Input, by_chat: bool, by_command: bool) -> Option<String> {
        let slash = input.text.find('/');
        if !by_chat && !by_command && slash.is_none() {
            return None;
        }
        self.open = true;
        let command = by_command || (slash.is_some() && !by_chat);
        let rest = match slash {
            Some(i) if command => &input.text[i + 1..],
            _ => {
                let mut c = input.text.chars();
                c.next();
                c.as_str()
            }
        };
        self.input = if command { "/".into() } else { String::new() };
        self.input.extend(rest.chars().take(MAX_INPUT));
        self.history_pos = None;
        if input.pressed(KeyCode::Enter) || input.pressed(KeyCode::NumpadEnter) {
            return self.send();
        }
        None
    }

    fn send(&mut self) -> Option<String> {
        let text = self.input.trim().to_string();
        self.close();
        if text.is_empty() {
            return None;
        }
        if self.history.last() != Some(&text) {
            self.history.push(text.clone());
        }
        Some(text)
    }

    /// Saisie quand le tchat est ouvert. Renvoie le texte envoyé (Entrée).
    pub fn update(&mut self, input: &Input, dt: f32) -> Option<String> {
        self.time += dt;
        for l in &mut self.lines {
            l.2 += dt;
        }
        self.clicked = None;
        if !self.open {
            return None;
        }
        if input.mouse_pressed(Mouse::Left) {
            if let Some((cx, cy)) = input.cursor {
                self.clicked = self
                    .hits
                    .borrow()
                    .iter()
                    .find(|(x, y, w, h, _)| cx >= *x && cx <= x + w && cy >= *y && cy <= y + h)
                    .map(|h| h.4.clone());
            }
        }
        for c in input.text.chars() {
            if self.input.chars().count() < MAX_INPUT {
                self.input.push(c);
            }
        }
        if input.pressed(KeyCode::Backspace) {
            self.input.pop();
        }
        if input.pressed(KeyCode::ArrowUp) && !self.history.is_empty() {
            let i = self
                .history_pos
                .map_or(self.history.len() - 1, |i| i.saturating_sub(1));
            self.history_pos = Some(i);
            self.input = self.history[i].clone();
        }
        if input.pressed(KeyCode::ArrowDown) {
            if let Some(i) = self.history_pos {
                if i + 1 < self.history.len() {
                    self.history_pos = Some(i + 1);
                    self.input = self.history[i + 1].clone();
                } else {
                    self.history_pos = None;
                    self.input.clear();
                }
            }
        }
        if input.pressed(KeyCode::Enter) || input.pressed(KeyCode::NumpadEnter) {
            return self.send();
        }
        None
    }

    pub fn draw(&self, ui: &mut Ui, size: (f32, f32)) {
        let s = (size.1 / 360.0).floor().max(1.0);
        let line_h = 10.0 * s;
        let x = 4.0 * s;
        let width = (320.0 * s).min(size.0 - 8.0 * s);
        // Au-dessus de la barre d'objets (et des cœurs).
        let bottom = size.1 - 38.0 * s;
        let visible: Vec<_> = if self.open {
            self.lines.iter().rev().take(12).collect()
        } else {
            self.lines
                .iter()
                .rev()
                .filter(|l| l.2 < SHOWN_S)
                .take(8)
                .collect()
        };
        let mut hits = self.hits.borrow_mut();
        hits.clear();
        for (k, (text, color, age, link)) in visible.iter().enumerate() {
            let a = if self.open {
                1.0
            } else {
                ((SHOWN_S - age) / FADE_S).clamp(0.0, 1.0)
            };
            let y = bottom - (k + 1) as f32 * line_h;
            ui.rect(x, y, width, line_h, [0.0, 0.0, 0.0, 0.45 * a]);
            ui.text(
                x + 2.0 * s,
                y + s,
                s,
                [color[0], color[1], color[2], a],
                text.clone(),
            );
            // Lien : souligné, cliquable quand le tchat est ouvert.
            if let Some(path) = link {
                let w = minelib_render::Ui::text_width(text, s).min(width - 4.0 * s);
                ui.rect(
                    x + 2.0 * s,
                    y + line_h - 1.5 * s,
                    w,
                    s * 0.5,
                    [color[0], color[1], color[2], a * 0.8],
                );
                if self.open {
                    hits.push((x, y, width, line_h, path.clone()));
                }
            }
        }
        if self.open {
            let y = size.1 - 13.0 * s;
            ui.rect(
                x,
                y,
                size.0 - 8.0 * s,
                line_h + 2.0 * s,
                [0.0, 0.0, 0.0, 0.6],
            );
            let caret = if (self.time * 2.0).fract() < 0.5 {
                "_"
            } else {
                ""
            };
            ui.text(
                x + 2.0 * s,
                y + 2.0 * s,
                s,
                WHITE,
                format!("{}{caret}", self.input),
            );
        }
    }
}
