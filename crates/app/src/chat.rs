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
const MAX_INPUT: usize = 256;

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
    /// Pseudos pour compléter le dernier mot avec Tab (joueurs de la partie).
    pub names: Vec<String>,
    /// Complétion en cours : (début tapé, rang de la proposition).
    completing: Option<(String, usize)>,
}

pub const WHITE: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
pub const YELLOW: [f32; 4] = [1.0, 0.9, 0.35, 1.0];
pub const RED: [f32; 4] = [1.0, 0.45, 0.4, 1.0];
pub const GREY: [f32; 4] = [0.7, 0.72, 0.78, 1.0];
/// Messages de WorldEdit (comme son violet clair).
pub const LIGHT_PURPLE: [f32; 4] = [0.85, 0.55, 1.0, 1.0];

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
            names: Vec::new(),
            completing: None,
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

    /// Tab : complète le dernier mot avec un pseudo ; Tab encore, le suivant.
    fn complete(&mut self) {
        let start = self.input.rfind(' ').map_or(0, |i| i + 1);
        let (typed, rank) = match &self.completing {
            Some((t, r)) => (t.clone(), r + 1),
            None => (self.input[start..].trim_start_matches('/').to_string(), 0),
        };
        let low = typed.to_lowercase();
        let mut found: Vec<&String> = self
            .names
            .iter()
            .filter(|n| n.to_lowercase().starts_with(&low))
            .collect();
        found.sort();
        found.dedup();
        if found.is_empty() || start == 0 {
            return;
        }
        let pick = found[rank % found.len()].clone();
        self.input.truncate(start);
        self.input.push_str(&pick);
        self.completing = Some((typed, rank));
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
        if input.pressed(KeyCode::Tab) {
            self.complete();
        } else if !input.text.is_empty() || input.pressed(KeyCode::Backspace) {
            self.completing = None;
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
        let (max_lines, recent): (usize, Vec<_>) = if self.open {
            (12, self.lines.iter().rev().collect())
        } else {
            (
                8,
                self.lines.iter().rev().filter(|l| l.2 < SHOWN_S).collect(),
            )
        };
        let mut hits = self.hits.borrow_mut();
        hits.clear();
        // Du plus récent (en bas) au plus ancien ; un message trop long passe à la ligne.
        let mut row = 0usize;
        for (text, color, age, link) in recent {
            if row >= max_lines {
                break;
            }
            let a = if self.open {
                1.0
            } else {
                ((SHOWN_S - age) / FADE_S).clamp(0.0, 1.0)
            };
            let pieces = wrap(text, s, width - 4.0 * s);
            let n = pieces.len().min(max_lines - row);
            // Dernières lignes du message en bas ; s'il ne tient pas, on garde sa fin.
            let shown = &pieces[pieces.len() - n..];
            let top = bottom - (row + n) as f32 * line_h;
            for (k, piece) in shown.iter().enumerate() {
                let y = top + k as f32 * line_h;
                ui.rect(x, y, width, line_h, [0.0, 0.0, 0.0, 0.45 * a]);
                ui.text(
                    x + 2.0 * s,
                    y + s,
                    s,
                    [color[0], color[1], color[2], a],
                    piece.clone(),
                );
                // Lien : souligné, cliquable quand le tchat est ouvert.
                if link.is_some() {
                    let w = Ui::text_width(piece, s);
                    ui.rect(
                        x + 2.0 * s,
                        y + line_h - 1.5 * s,
                        w,
                        s * 0.5,
                        [color[0], color[1], color[2], a * 0.8],
                    );
                }
            }
            if let (Some(path), true) = (link, self.open) {
                hits.push((x, top, width, n as f32 * line_h, path.clone()));
            }
            row += n;
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
            // Saisie plus longue que la boîte : on montre la fin (là où l'on écrit).
            let room = size.0 - 12.0 * s - Ui::text_width("_", s);
            let mut shown = self.input.as_str();
            while Ui::text_width(shown, s) > room {
                let mut c = shown.chars();
                c.next();
                shown = c.as_str();
            }
            ui.text(
                x + 2.0 * s,
                y + 2.0 * s,
                s,
                WHITE,
                format!("{shown}{caret}"),
            );
        }
    }
}

/// Coupe `text` en lignes d'au plus `max_w` pixels (à l'échelle `s`), entre les mots
/// quand c'est possible, sinon au milieu d'un mot trop long.
fn wrap(text: &str, s: f32, max_w: f32) -> Vec<String> {
    let mut out = Vec::new();
    let mut line = String::new();
    for word in text.split(' ') {
        let candidate = if line.is_empty() {
            word.to_string()
        } else {
            format!("{line} {word}")
        };
        if Ui::text_width(&candidate, s) <= max_w {
            line = candidate;
            continue;
        }
        if !line.is_empty() {
            out.push(std::mem::take(&mut line));
        }
        // Mot plus long qu'une ligne : coupé caractère par caractère.
        for c in word.chars() {
            line.push(c);
            if Ui::text_width(&line, s) > max_w && line.chars().count() > 1 {
                let last = line.pop().unwrap_or(' ');
                out.push(std::mem::take(&mut line));
                line.push(last);
            }
        }
    }
    if !line.is_empty() || out.is_empty() {
        out.push(line);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_messages_wrap() {
        let s = 1.0;
        let text = "un message vraiment très long qui ne tient pas sur une seule ligne du tchat";
        let lines = wrap(text, s, 120.0);
        assert!(lines.len() > 1);
        assert!(lines.iter().all(|l| Ui::text_width(l, s) <= 120.0));
        assert_eq!(lines.join(" "), text);
        let long = "a".repeat(200);
        assert!(wrap(&long, s, 120.0)
            .iter()
            .all(|l| Ui::text_width(l, s) <= 120.0));
        assert_eq!(wrap("court", s, 120.0), vec!["court".to_string()]);
    }
}

#[cfg(test)]
mod completion_tests {
    use super::*;

    #[test]
    fn tab_completes_the_last_word_with_a_pseudo() {
        let mut c = Chat {
            names: vec!["Viendax".into(), "Victor".into(), "Bob".into()],
            input: "/report vi".into(),
            ..Default::default()
        };
        c.complete();
        assert_eq!(c.input, "/report Victor");
        c.complete();
        assert_eq!(c.input, "/report Viendax", "Tab encore : le suivant");
    }
}
