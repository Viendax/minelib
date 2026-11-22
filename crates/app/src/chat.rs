//! Tchat façon Minecraft : `T` ou `/` pour écrire, Entrée pour envoyer, Échap pour fermer,
//! flèches haut/bas pour l'historique. Les messages restent affichés quelques secondes.
//! Un message peut porter un lien (capture d'écran) : tchat ouvert, un clic dessus le
//! signale (`clicked`).
//!
//! (v236) Tchat ouvert : la molette remonte dans les messages (les 100 derniers) ; on y
//! sélectionne du texte à la souris (glisser), Ctrl+C le copie ; un clic dans la saisie y
//! place le curseur, glisser y sélectionne (aussi Maj + flèches, Ctrl+A), Ctrl+C / Ctrl+X /
//! Ctrl+V copient, coupent et collent.

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

/// (v236) Lignes de messages par cran de molette.
const WHEEL_ROWS: i64 = 3;

/// (v236) Une ligne de message (un message coupé en lignes), à la dernière image où le tchat
/// était ouvert.
#[derive(Clone, Debug, Default)]
struct Row {
    /// Rang en partant du bas (0 : la ligne la plus récente).
    rank: usize,
    /// Message d'où elle vient (dans `lines`).
    msg: usize,
    text: String,
    /// Coin du texte ; `None` : pas à l'écran (molette).
    at: Option<(f32, f32)>,
}

/// (v236) Ce que montrait le tchat ouvert à la dernière image : pour trouver ce qui est sous
/// la souris et copier.
#[derive(Clone, Debug, Default)]
struct Layout {
    /// Toutes les lignes, de la plus ancienne à la plus récente.
    rows: Vec<Row>,
    /// Lignes qui tiennent à l'écran.
    shown: usize,
    s: f32,
    line_h: f32,
    /// Saisie : début du texte (x), haut et hauteur de la boîte, premier caractère montré.
    input: (f32, f32, f32, usize),
}

/// (v236) Ce que la souris tient (bouton gauche enfoncé).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum Drag {
    #[default]
    None,
    Rows,
    Input,
}

/// (v236) Place dans les messages : (rang de la ligne en partant du bas, caractère).
type Spot = (usize, usize);

/// (v195) Oui ou non dans une commande, au choix : `on`/`off`, `true`/`false`, `1`/`0` ou
/// `oui`/`non`, en majuscules ou non. `None` : autre chose.
pub fn switch(word: &str) -> Option<bool> {
    match word.to_lowercase().as_str() {
        "on" | "true" | "1" | "oui" => Some(true),
        "off" | "false" | "0" | "non" => Some(false),
        _ => None,
    }
}

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
    /// Position du curseur dans la saisie (en caractères), et touche tenue qui se répète
    /// (gauche, droite, effacer) avec son temps.
    cursor: usize,
    repeat: Option<(KeyCode, f32)>,
    /// (v236) Lignes remontées avec la molette (0 : en bas).
    scroll: usize,
    /// (v236) Sélection dans les messages : (début, fin) dans l'ordre où on l'a faite.
    sel: Option<(Spot, Spot)>,
    /// (v236) Sélection dans la saisie : son autre bout (l'un est le curseur).
    anchor: Option<usize>,
    drag: Drag,
    /// (v236) Ce que montrait la dernière image (`draw`).
    layout: RefCell<Layout>,
    /// (v236) Moment de la dernière copie (« Copié » s'affiche un instant).
    copied_at: Option<f32>,
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
            cursor: 0,
            repeat: None,
            scroll: 0,
            sel: None,
            anchor: None,
            drag: Drag::None,
            layout: RefCell::new(Layout::default()),
            copied_at: None,
        }
    }
}

/// (v236) Ordre de lecture de deux places dans les messages (de haut en bas, puis de gauche
/// à droite).
fn reading(a: Spot, b: Spot) -> (Spot, Spot) {
    let key = |p: Spot| (usize::MAX - p.0, p.1);
    if key(a) <= key(b) {
        (a, b)
    } else {
        (b, a)
    }
}

/// (v236) Copie `text` dans le presse-papier du système.
fn copy_text(text: &str) {
    if cfg!(test) {
        return;
    }
    if let Err(e) = arboard::Clipboard::new().and_then(|mut c| c.set_text(text.to_string())) {
        log::warn!("presse-papier : {e}");
    }
}

/// (v236) Texte du presse-papier, sur une ligne (sauts de ligne et tabulations : espaces).
fn paste_text() -> String {
    if cfg!(test) {
        return String::new();
    }
    arboard::Clipboard::new()
        .and_then(|mut c| c.get_text())
        .map(|t| {
            t.chars()
                .map(|c| if c == '\n' || c == '\t' { ' ' } else { c })
                .filter(|c| !c.is_control())
                .collect()
        })
        .unwrap_or_default()
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
        self.cursor = 0;
        self.history_pos = None;
        self.scroll = 0;
        self.sel = None;
        self.anchor = None;
        self.drag = Drag::None;
    }

    /// (v236) La sélection de la saisie (début, fin en caractères), si elle n'est pas vide.
    fn input_selection(&self) -> Option<(usize, usize)> {
        let a = self.anchor?;
        let len = self.input.chars().count();
        let (lo, hi) = (a.min(self.cursor).min(len), a.max(self.cursor).min(len));
        (lo < hi).then_some((lo, hi))
    }

    /// (v236) Retire la sélection de la saisie (le curseur va à sa place) ; vrai s'il y en
    /// avait une.
    fn delete_input_selection(&mut self) -> bool {
        let Some((lo, hi)) = self.input_selection() else {
            self.anchor = None;
            return false;
        };
        let byte = |s: &str, i: usize| s.char_indices().nth(i).map_or(s.len(), |(b, _)| b);
        let (a, b) = (byte(&self.input, lo), byte(&self.input, hi));
        self.input.replace_range(a..b, "");
        self.cursor = lo;
        self.anchor = None;
        true
    }

    /// (v236) Texte sélectionné dans les messages : les lignes d'un même message jointes par
    /// une espace, deux messages par un saut de ligne.
    fn selected_text(&self) -> Option<String> {
        let (a, b) = self.sel?;
        let (lo, hi) = reading(a, b);
        if lo == hi {
            return None;
        }
        let layout = self.layout.borrow();
        let mut out = String::new();
        let mut last_msg = None;
        for row in layout
            .rows
            .iter()
            .filter(|r| r.rank <= lo.0 && r.rank >= hi.0)
        {
            let chars: Vec<char> = row.text.chars().collect();
            let from = if row.rank == lo.0 {
                lo.1.min(chars.len())
            } else {
                0
            };
            let to = if row.rank == hi.0 {
                hi.1.min(chars.len())
            } else {
                chars.len()
            };
            if let Some(m) = last_msg {
                out.push(if m == row.msg { ' ' } else { '\n' });
            }
            out.extend(&chars[from.min(to)..to]);
            last_msg = Some(row.msg);
        }
        (!out.trim().is_empty()).then_some(out)
    }

    /// (v236) Place (ligne, caractère) des messages sous le point (`cx`, `cy`) de l'écran.
    fn row_spot_at(&self, cx: f32, cy: f32, clamp_rows: bool) -> Option<Spot> {
        let layout = self.layout.borrow();
        let shown: Vec<&Row> = layout.rows.iter().filter(|r| r.at.is_some()).collect();
        let (first, last) = (shown.first()?, shown.last()?);
        let row = shown
            .iter()
            .find(|r| {
                let (_, y) = r.at.unwrap_or_default();
                cy >= y - layout.s && cy < y - layout.s + layout.line_h
            })
            .copied()
            .or_else(|| {
                if !clamp_rows {
                    return None;
                }
                let top = first.at.unwrap_or_default().1;
                Some(if cy < top { *first } else { *last })
            })?;
        let (x, _) = row.at.unwrap_or_default();
        Some((row.rank, char_at(&row.text, cx - x, layout.s)))
    }

    /// (v236) Caractère de la saisie sous l'abscisse `cx` (et, si `strict`, seulement dans
    /// la boîte de saisie, à la hauteur `cy`).
    fn input_char_at(&self, cx: f32, cy: f32, strict: bool) -> Option<usize> {
        let layout = self.layout.borrow();
        let (x, y, h, start) = layout.input;
        if h <= 0.0 || (strict && (cy < y || cy > y + h)) {
            return None;
        }
        let shown: String = self.input.chars().skip(start).collect();
        Some(start + char_at(&shown, cx - x, layout.s))
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
        self.cursor = self.input.chars().count();
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
        self.cursor = self.input.chars().count();
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
        let ctrl = input.held(KeyCode::ControlLeft) || input.held(KeyCode::ControlRight);
        let shift = input.held(KeyCode::ShiftLeft) || input.held(KeyCode::ShiftRight);
        // (v236) Molette : remonter dans les messages (et redescendre).
        if input.scroll.abs() >= 1.0 {
            let max = {
                let l = self.layout.borrow();
                l.rows.len().saturating_sub(l.shown)
            };
            let d = input.scroll.round() as i64 * WHEEL_ROWS;
            self.scroll = (self.scroll as i64 + d).clamp(0, max as i64) as usize;
        }
        if input.mouse_pressed(Mouse::Left) {
            if let Some((cx, cy)) = input.cursor {
                self.clicked = self
                    .hits
                    .borrow()
                    .iter()
                    .find(|(x, y, w, h, _)| cx >= *x && cx <= x + w && cy >= *y && cy <= y + h)
                    .map(|h| h.4.clone());
                // (v236) Sinon : le curseur dans la saisie, ou le début d'une sélection
                // dans les messages.
                if self.clicked.is_none() {
                    if let Some(c) = self.input_char_at(cx, cy, true) {
                        if !shift {
                            self.anchor = Some(c);
                        } else if self.anchor.is_none() {
                            self.anchor = Some(self.cursor);
                        }
                        self.cursor = c;
                        self.sel = None;
                        self.drag = Drag::Input;
                    } else if let Some(p) = self.row_spot_at(cx, cy, false) {
                        self.sel = Some((p, p));
                        self.anchor = None;
                        self.drag = Drag::Rows;
                    } else {
                        self.sel = None;
                    }
                }
            }
        } else if input.mouse_held(Mouse::Left) {
            if let Some((cx, cy)) = input.cursor {
                match self.drag {
                    Drag::Input => {
                        if let Some(c) = self.input_char_at(cx, cy, false) {
                            self.cursor = c;
                        }
                    }
                    Drag::Rows => {
                        if let (Some(p), Some((a, _))) = (self.row_spot_at(cx, cy, true), self.sel)
                        {
                            self.sel = Some((a, p));
                        }
                    }
                    Drag::None => {}
                }
            }
        } else {
            self.drag = Drag::None;
        }
        // (v236) Copier, couper, coller, tout sélectionner.
        if ctrl && input.pressed(KeyCode::KeyC) {
            let text = self.selected_text().or_else(|| {
                let (lo, hi) = self.input_selection()?;
                Some(self.input.chars().skip(lo).take(hi - lo).collect())
            });
            if let Some(t) = text {
                copy_text(&t);
                self.copied_at = Some(self.time);
            }
        }
        if ctrl && input.pressed(KeyCode::KeyX) {
            if let Some((lo, hi)) = self.input_selection() {
                let t: String = self.input.chars().skip(lo).take(hi - lo).collect();
                copy_text(&t);
                self.copied_at = Some(self.time);
                self.delete_input_selection();
            }
        }
        if ctrl && input.pressed(KeyCode::KeyA) {
            self.anchor = Some(0);
            self.cursor = self.input.chars().count();
        }
        let pasted = if ctrl && input.pressed(KeyCode::KeyV) {
            paste_text()
        } else {
            String::new()
        };
        // Tout se fait à la place du curseur (flèches gauche et droite, Début, Fin).
        let byte = |s: &str, i: usize| s.char_indices().nth(i).map_or(s.len(), |(b, _)| b);
        let typed: String = input.text.chars().chain(pasted.chars()).collect();
        if !typed.is_empty() {
            // (v236) Ce qu'on tape remplace la sélection.
            self.delete_input_selection();
        }
        for c in typed.chars() {
            if self.input.chars().count() < MAX_INPUT {
                let at = byte(&self.input, self.cursor);
                self.input.insert(at, c);
                self.cursor += 1;
            }
        }
        // Gauche, droite, effacer : une fois, puis à répétition si la touche reste tenue.
        let mut act = |k: KeyCode| -> bool {
            if input.pressed(k) {
                self.repeat = Some((k, -0.4));
                return true;
            }
            match &mut self.repeat {
                Some((rk, t)) if *rk == k && input.held(k) => {
                    *t += dt;
                    if *t >= 0.035 {
                        *t = 0.0;
                        return true;
                    }
                    false
                }
                _ => false,
            }
        };
        let (left, right) = (act(KeyCode::ArrowLeft), act(KeyCode::ArrowRight));
        let (back, del) = (act(KeyCode::Backspace), act(KeyCode::Delete));
        let (home, end) = (input.pressed(KeyCode::Home), input.pressed(KeyCode::End));
        let len = self.input.chars().count();
        self.cursor = self.cursor.min(len);
        // (v236) Maj + flèches, Début, Fin : la sélection s'étend ; sans Maj, elle s'en va.
        if left || right || home || end {
            if shift {
                self.anchor.get_or_insert(self.cursor);
            } else {
                self.anchor = None;
            }
        }
        if left {
            self.cursor = self.cursor.saturating_sub(1);
        }
        if right {
            self.cursor = (self.cursor + 1).min(len);
        }
        if home {
            self.cursor = 0;
        }
        if end {
            self.cursor = len;
        }
        let cut = (back || del) && self.delete_input_selection();
        if back && !cut && self.cursor > 0 {
            let at = byte(&self.input, self.cursor - 1);
            self.input.remove(at);
            self.cursor -= 1;
        }
        if del && !cut && self.cursor < self.input.chars().count() {
            let at = byte(&self.input, self.cursor);
            self.input.remove(at);
        }
        if input.pressed(KeyCode::Tab) {
            self.complete();
        } else if !input.text.is_empty() || back || del {
            self.completing = None;
        }
        if input.pressed(KeyCode::ArrowUp) && !self.history.is_empty() {
            let i = self
                .history_pos
                .map_or(self.history.len() - 1, |i| i.saturating_sub(1));
            self.history_pos = Some(i);
            self.input = self.history[i].clone();
            self.cursor = self.input.chars().count();
            self.anchor = None;
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
                self.cursor = self.input.chars().count();
                self.anchor = None;
            }
        }
        if input.pressed(KeyCode::Enter) || input.pressed(KeyCode::NumpadEnter) {
            return self.send();
        }
        None
    }

    /// (v236) Lignes du tchat ouvert : tous les messages coupés en lignes (les plus anciens
    /// d'abord), et la place à l'écran de celles qu'on voit (molette : `scroll`).
    fn build_layout(&self, size: (f32, f32)) -> Layout {
        let s = (size.1 / 360.0).floor().max(1.0);
        let line_h = 10.0 * s;
        let x = 4.0 * s;
        let width = (320.0 * s).min(size.0 - 8.0 * s);
        let bottom = size.1 - 38.0 * s;
        let mut rows = Vec::new();
        for (msg, (text, ..)) in self.lines.iter().enumerate() {
            for piece in wrap(text, s, width - 4.0 * s) {
                rows.push(Row {
                    rank: 0,
                    msg,
                    text: piece,
                    at: None,
                });
            }
        }
        let n = rows.len();
        let shown = 12usize;
        let scroll = self.scroll.min(n.saturating_sub(shown));
        for (i, row) in rows.iter_mut().enumerate() {
            row.rank = n - 1 - i;
            if row.rank >= scroll && row.rank < scroll + shown {
                let y = bottom - (row.rank - scroll + 1) as f32 * line_h;
                row.at = Some((x + 2.0 * s, y + s));
            }
        }
        // Saisie : la partie autour du curseur si elle est plus longue que la boîte.
        let chars: Vec<char> = self.input.chars().collect();
        let cur = self.cursor.min(chars.len());
        let room = size.0 - 12.0 * s - Ui::text_width("_", s);
        let w = |a: usize, b: usize| Ui::text_width(&chars[a..b].iter().collect::<String>(), s);
        let mut start = 0;
        while start < cur && w(start, cur) > room {
            start += 1;
        }
        let iy = size.1 - 13.0 * s;
        Layout {
            rows,
            shown,
            s,
            line_h,
            input: (x + 2.0 * s, iy, line_h + 2.0 * s, start),
        }
    }

    pub fn draw(&self, ui: &mut Ui, size: (f32, f32)) {
        let s = (size.1 / 360.0).floor().max(1.0);
        let line_h = 10.0 * s;
        let x = 4.0 * s;
        let width = (320.0 * s).min(size.0 - 8.0 * s);
        // Au-dessus de la barre d'objets (et des cœurs).
        let bottom = size.1 - 38.0 * s;
        let mut hits = self.hits.borrow_mut();
        hits.clear();
        if self.open {
            // (v236) Tchat ouvert : toutes les lignes, remontées avec la molette ; la
            // sélection en clair derrière le texte.
            let layout = self.build_layout(size);
            let sel = self.sel.map(|(a, b)| reading(a, b));
            for row in &layout.rows {
                let Some((tx, ty)) = row.at else { continue };
                let (_, color, _, link) = &self.lines[row.msg];
                let y = ty - s;
                ui.rect(x, y, width, line_h, [0.0, 0.0, 0.0, 0.45]);
                if let Some((lo, hi)) = sel.filter(|(lo, hi)| lo != hi) {
                    if row.rank <= lo.0 && row.rank >= hi.0 {
                        let chars: Vec<char> = row.text.chars().collect();
                        let from = if row.rank == lo.0 {
                            lo.1.min(chars.len())
                        } else {
                            0
                        };
                        let to = if row.rank == hi.0 {
                            hi.1.min(chars.len())
                        } else {
                            chars.len()
                        };
                        let w =
                            |n: usize| Ui::text_width(&chars[..n].iter().collect::<String>(), s);
                        if to > from {
                            ui.rect(
                                tx + w(from),
                                y,
                                w(to) - w(from),
                                line_h,
                                [0.45, 0.6, 1.0, 0.55],
                            );
                        }
                    }
                }
                ui.text(tx, ty, s, *color, row.text.clone());
                if link.is_some() {
                    let w = Ui::text_width(&row.text, s);
                    ui.rect(
                        tx,
                        y + line_h - 1.5 * s,
                        w,
                        s * 0.5,
                        [color[0], color[1], color[2], 0.8],
                    );
                    hits.push((x, y, width, line_h, link.clone().unwrap_or_default()));
                }
            }
            // Ascenseur : où l'on est dans les messages.
            let n = layout.rows.len();
            if n > layout.shown {
                let h = layout.shown as f32 * line_h;
                let top = bottom - h;
                let scroll = self.scroll.min(n - layout.shown);
                let bar = (h * layout.shown as f32 / n as f32).max(4.0 * s);
                let y = top + (h - bar) * (1.0 - scroll as f32 / (n - layout.shown) as f32);
                ui.rect(x + width, top, 2.0 * s, h, [1.0, 1.0, 1.0, 0.12]);
                ui.rect(x + width, y, 2.0 * s, bar, [1.0, 1.0, 1.0, 0.6]);
            }
            let y = size.1 - 13.0 * s;
            ui.rect(
                x,
                y,
                size.0 - 8.0 * s,
                line_h + 2.0 * s,
                [0.0, 0.0, 0.0, 0.6],
            );
            let chars: Vec<char> = self.input.chars().collect();
            let cur = self.cursor.min(chars.len());
            let start = layout.input.3.min(cur);
            let room = size.0 - 12.0 * s - Ui::text_width("_", s);
            let width =
                |a: usize, b: usize| Ui::text_width(&chars[a..b].iter().collect::<String>(), s);
            let mut end = chars.len();
            while end > cur && width(start, end) > room {
                end -= 1;
            }
            // (v236) Sélection de la saisie, en clair.
            if let Some((lo, hi)) = self.input_selection() {
                let (lo, hi) = (lo.clamp(start, end), hi.clamp(start, end));
                if hi > lo {
                    ui.rect(
                        x + 2.0 * s + width(start, lo),
                        y + 1.0 * s,
                        width(lo, hi),
                        line_h,
                        [0.45, 0.6, 1.0, 0.55],
                    );
                }
            }
            let shown: String = chars[start..end].iter().collect();
            ui.text(x + 2.0 * s, y + 2.0 * s, s, WHITE, shown);
            // Curseur : une barre clignotante entre deux lettres.
            if (self.time * 2.0).fract() < 0.5 {
                let cx = x + 2.0 * s + width(start, cur);
                ui.rect(cx, y + 1.0 * s, s.max(1.0), line_h, WHITE);
            }
            // (v236) « Copié » un instant, au bout de la saisie.
            if self.copied_at.is_some_and(|t| self.time - t < 1.2) {
                let label = "Copié";
                let w = Ui::text_width(label, s);
                ui.text(size.0 - 8.0 * s - w, y + 2.0 * s, s, YELLOW, label);
            }
            *self.layout.borrow_mut() = layout;
            return;
        }
        let recent: Vec<_> = self.lines.iter().rev().filter(|l| l.2 < SHOWN_S).collect();
        let max_lines = 8;
        // Du plus récent (en bas) au plus ancien ; un message trop long passe à la ligne.
        let mut row = 0usize;
        for (text, color, age, link) in recent {
            if row >= max_lines {
                break;
            }
            let a = ((SHOWN_S - age) / FADE_S).clamp(0.0, 1.0);
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
                // Lien : souligné.
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
            row += n;
        }
    }
}

/// (v236) Caractère de `text` (à l'échelle `s`) le plus proche de l'abscisse `dx` comptée
/// depuis son début : la place du curseur entre deux lettres.
fn char_at(text: &str, dx: f32, s: f32) -> usize {
    let mut prev = 0.0;
    let mut acc = String::new();
    for (i, c) in text.chars().enumerate() {
        acc.push(c);
        let w = Ui::text_width(&acc, s);
        if dx < (prev + w) / 2.0 {
            return i;
        }
        prev = w;
    }
    text.chars().count()
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
mod switch_tests {
    /// (v195) Toutes les façons d'écrire oui ou non, et rien d'autre.
    #[test]
    fn on_off_true_false_one_zero() {
        for w in ["on", "ON", "true", "True", "1", "oui"] {
            assert_eq!(super::switch(w), Some(true), "{w}");
        }
        for w in ["off", "Off", "false", "FALSE", "0", "non"] {
            assert_eq!(super::switch(w), Some(false), "{w}");
        }
        for w in ["", "2", "auto", "stop", "facile", "onn"] {
            assert_eq!(super::switch(w), None, "{w}");
        }
    }
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
mod cursor_tests {
    use super::*;

    #[test]
    fn arrows_move_the_cursor_and_typing_goes_there() {
        let mut c = Chat {
            open: true,
            ..Default::default()
        };
        let mut i = Input::default();
        i.text = "bonour".into();
        c.update(&i, 0.016);
        for _ in 0..3 {
            i.next_frame();
            i.press(KeyCode::ArrowLeft);
            c.update(&i, 0.016);
        }
        i.next_frame();
        i.text = "j".into();
        c.update(&i, 0.016);
        assert_eq!(c.input, "bonjour");
        i.next_frame();
        i.press(KeyCode::Home);
        c.update(&i, 0.016);
        i.next_frame();
        i.press(KeyCode::Delete);
        c.update(&i, 0.016);
        assert_eq!(c.input, "onjour");
        i.next_frame();
        i.press(KeyCode::End);
        c.update(&i, 0.016);
        i.next_frame();
        i.press(KeyCode::Backspace);
        c.update(&i, 0.016);
        assert_eq!(c.input, "onjou");
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

#[cfg(test)]
mod mouse_tests {
    use super::*;

    const SIZE: (f32, f32) = (1280.0, 720.0);

    fn open_with(lines: &[&str]) -> Chat {
        let mut c = Chat {
            open: true,
            ..Default::default()
        };
        for l in lines {
            c.say(*l, WHITE);
        }
        *c.layout.borrow_mut() = c.build_layout(SIZE);
        c
    }

    /// Point de l'écran du caractère `k` de la ligne de rang `rank` (0 : en bas).
    fn spot(c: &Chat, rank: usize, k: usize) -> (f32, f32) {
        let l = c.layout.borrow();
        let row = l.rows.iter().find(|r| r.rank == rank).unwrap();
        let (x, y) = row.at.unwrap();
        let before: String = row.text.chars().take(k).collect();
        (x + Ui::text_width(&before, l.s) + 0.1, y + l.s)
    }

    /// (v236) La molette remonte dans les messages, trois lignes par cran, sans dépasser le
    /// premier ; refermer le tchat revient en bas.
    #[test]
    fn the_wheel_scrolls_back_through_the_messages() {
        let msgs: Vec<String> = (0..40).map(|i| format!("message {i}")).collect();
        let refs: Vec<&str> = msgs.iter().map(String::as_str).collect();
        let mut c = open_with(&refs);
        let mut i = Input::default();
        i.scroll = 1.0;
        c.update(&i, 0.016);
        assert_eq!(c.scroll, 3);
        *c.layout.borrow_mut() = c.build_layout(SIZE);
        let top = c
            .layout
            .borrow()
            .rows
            .iter()
            .find(|r| r.at.is_some())
            .map(|r| r.text.clone());
        assert_eq!(
            top.as_deref(),
            Some("message 25"),
            "la plus haute des lignes vues"
        );
        i.scroll = 100.0;
        c.update(&i, 0.016);
        assert_eq!(c.scroll, 40 - 12, "pas plus haut que le premier message");
        c.close();
        assert_eq!(c.scroll, 0);
    }

    /// (v236) Glisser sur les messages sélectionne du texte (sur deux lignes d'un même
    /// message ou sur deux messages) ; Ctrl+C le copie.
    #[test]
    fn dragging_over_messages_selects_their_text() {
        let mut c = open_with(&["bonjour tout le monde", "deuxième message"]);
        let mut i = Input::default();
        i.cursor = Some(spot(&c, 1, 0));
        i.press_mouse(Mouse::Left);
        c.update(&i, 0.016);
        i.next_frame();
        i.cursor = Some(spot(&c, 1, 12));
        c.update(&i, 0.016);
        assert_eq!(c.selected_text().as_deref(), Some("bonjour tout"));
        // Jusque dans le message suivant (en bas), à l'envers aussi.
        i.next_frame();
        i.cursor = Some(spot(&c, 0, 8));
        c.update(&i, 0.016);
        assert_eq!(
            c.selected_text().as_deref(),
            Some("bonjour tout le monde\ndeuxième")
        );
        i.next_frame();
        i.release_mouse(Mouse::Left);
        i.press(KeyCode::ControlLeft);
        i.press(KeyCode::KeyC);
        c.update(&i, 0.016);
        assert!(c.copied_at.is_some(), "copié");
        // Un clic ailleurs dans les messages : plus de sélection.
        i.next_frame();
        i.cursor = Some(spot(&c, 0, 3));
        i.press_mouse(Mouse::Left);
        c.update(&i, 0.016);
        assert_eq!(c.selected_text(), None);
    }

    /// (v236) Un clic dans la saisie y place le curseur ; glisser sélectionne, ce qu'on
    /// tape remplace la sélection ; Maj + flèches sélectionnent aussi ; Ctrl+A tout.
    #[test]
    fn the_mouse_places_the_cursor_in_the_input() {
        let mut c = open_with(&[]);
        let mut i = Input::default();
        i.text = "salut toi".into();
        c.update(&i, 0.016);
        *c.layout.borrow_mut() = c.build_layout(SIZE);
        let (x0, y0, h, _) = c.layout.borrow().input;
        let s = c.layout.borrow().s;
        let at = |k: usize| x0 + Ui::text_width(&"salut toi"[..k], s) + 0.1;
        i.next_frame();
        i.cursor = Some((at(5), y0 + h / 2.0));
        i.press_mouse(Mouse::Left);
        c.update(&i, 0.016);
        assert_eq!(c.cursor, 5, "curseur entre « salut » et « toi »");
        i.next_frame();
        i.cursor = Some((at(9), y0 + h / 2.0));
        c.update(&i, 0.016);
        assert_eq!(c.input_selection(), Some((5, 9)));
        i.next_frame();
        i.release_mouse(Mouse::Left);
        i.text = "!".into();
        c.update(&i, 0.016);
        assert_eq!(c.input, "salut!");
        // Maj + Début : de là au début ; Retour arrière l'efface.
        i.next_frame();
        i.press(KeyCode::ShiftLeft);
        i.press(KeyCode::Home);
        c.update(&i, 0.016);
        assert_eq!(c.input_selection(), Some((0, 6)));
        i = Input::default();
        i.press(KeyCode::Backspace);
        c.update(&i, 0.016);
        assert_eq!(c.input, "");
        // Ctrl+A puis une lettre : tout est remplacé.
        i = Input::default();
        i.text = "abc".into();
        c.update(&i, 0.016);
        i.next_frame();
        i.press(KeyCode::ControlLeft);
        i.press(KeyCode::KeyA);
        c.update(&i, 0.016);
        i = Input::default();
        i.text = "z".into();
        c.update(&i, 0.016);
        assert_eq!(c.input, "z");
    }
}
