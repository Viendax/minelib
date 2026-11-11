//! Inventaire : barre de 10 emplacements utilisable (touches 1 à 0, molette)
//! + inventaire de 10 × 3 ouvert avec E. Info-bulles au survol.
//!
//! En créatif, E ouvre le catalogue : tous les blocs et toutes les torches (une entrée par
//! famille : un escalier, une dalle… par matériau), recherche au clavier, molette pour
//! défiler, et une poubelle à côté de la barre.

use minelib_render::Ui;
use minelib_world::flame::{format_duration, CHARGE_G, CHEMICALS, WOOD_FLAME};
use minelib_world::BlockId;

pub const HOTBAR: usize = 10;
pub const COLS: usize = 10;
pub const ROWS: usize = 3;
pub const SLOTS: usize = HOTBAR + COLS * ROWS;
pub const MAX_STACK: u32 = 64;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Item {
    Block(BlockId),
    /// Torche, éventuellement chargée de 5 g d'un sel (index dans `CHEMICALS`).
    Torch(Option<usize>),
    /// Baguette de sélection (WorldEdit) : clic gauche coin 1, clic droit coin 2.
    Wand,
}

impl Item {
    pub fn name(self) -> String {
        match self {
            Item::Block(b) => capitalize(b.name()),
            Item::Wand => "Baguette de sélection".into(),
            Item::Torch(None) => "Torche".into(),
            Item::Torch(Some(i)) => {
                let c = &CHEMICALS[i];
                let el = c.element.to_lowercase();
                let art = if el.starts_with(['a', 'e', 'é', 'i', 'o', 'u']) {
                    "à l'"
                } else {
                    "au "
                };
                format!("Torche {art}{el} ({})", c.formula)
            }
        }
    }

    /// Couleur de flamme (sRGB) pour les torches.
    pub fn flame(self) -> Option<[u8; 3]> {
        match self {
            Item::Torch(None) => Some(WOOD_FLAME),
            Item::Torch(Some(i)) => Some(CHEMICALS[i].rgb),
            Item::Block(_) | Item::Wand => None,
        }
    }

    /// Lignes de l'info-bulle : (texte, couleur sRGB).
    pub fn tooltip(self) -> Vec<(String, [u8; 3])> {
        const WHITE: [u8; 3] = [240, 240, 240];
        const GREY: [u8; 3] = [170, 175, 185];
        let mut lines = vec![(self.name(), self.flame().map_or(WHITE, brighten))];
        match self {
            Item::Block(_) => lines.push(("Bloc de construction".into(), GREY)),
            Item::Wand => {
                lines.push(("Clic gauche : coin 1, clic droit : coin 2".into(), WHITE));
                lines.push(("Clic molette : effacer la sélection".into(), WHITE));
                lines.push(("Commandes //set, //copy, //paste… (//help)".into(), GREY));
            }
            Item::Torch(None) => {
                lines.push(("Flamme orange (bois)".into(), WHITE));
                lines.push(("Lumière permanente".into(), GREY));
            }
            Item::Torch(Some(i)) => {
                let c = &CHEMICALS[i];
                lines.push((
                    format!("{} g de {} ({})", fr(CHARGE_G, 0), c.salt, c.formula),
                    WHITE,
                ));
                lines.push((format!("Flamme {} - {}", c.color_name, c.emission), WHITE));
                lines.push((
                    format!(
                        "Couleur pendant {} ({} g/min)",
                        format_duration(c.burn_time_s()),
                        fr(c.burn_rate_g_per_min(), 2)
                    ),
                    [255, 220, 120],
                ));
                lines.push(("puis flamme normale (bois)".into(), GREY));
            }
        }
        lines
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
        .unwrap_or_default()
}

/// Nombre à la française (virgule décimale).
fn fr(v: f32, decimals: usize) -> String {
    format!("{v:.decimals$}").replace('.', ",")
}

fn brighten(c: [u8; 3]) -> [u8; 3] {
    c.map(|v| (v as u16 + (255 - v as u16) / 3) as u8)
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Stack {
    pub item: Item,
    pub count: u32,
}

pub struct Inventory {
    pub slots: [Option<Stack>; SLOTS],
    pub selected: usize,
    pub open: bool,
    held: Option<Stack>,
    /// Durée d'affichage restante du nom de l'objet choisi (s).
    name_timer: f32,
    /// Mode créatif (catalogue à la place de l'inventaire), mis à jour par la partie.
    pub creative: bool,
    /// Recherche dans le catalogue créatif.
    pub search: String,
    /// Première ligne affichée du catalogue.
    scroll_row: usize,
    /// Barre de défilement du catalogue tenue à la souris.
    dragging: bool,
}

/// Catalogue créatif : torches (toutes les flammes), puis un bloc par famille.
pub fn catalogue() -> &'static [Item] {
    static CAT: std::sync::OnceLock<Vec<Item>> = std::sync::OnceLock::new();
    CAT.get_or_init(|| {
        use minelib_world::registry::{registry, Shape};
        let mut items: Vec<Item> = [Item::Wand, Item::Torch(None)]
            .into_iter()
            .chain((0..CHEMICALS.len()).map(|i| Item::Torch(Some(i))))
            .collect();
        let mut seen = std::collections::HashSet::new();
        for (i, d) in registry().defs.iter().enumerate() {
            if matches!(
                d.shape,
                Shape::Air | Shape::Invisible | Shape::Torch | Shape::WallTorch(_)
            ) || d.key.starts_with("reserved")
                || d.key.ends_with("_upper")
                || d.key.contains(":upper")
            {
                continue;
            }
            let parts: Vec<&str> = d.key.split(':').collect();
            // Une entrée par famille : orientation et moitié haute regroupées.
            let family = match parts[0] {
                "stairs" | "slab" => parts[..2.min(parts.len())].join(":"),
                "plant" => d.key.clone(),
                _ => parts[0].to_string(),
            };
            if seen.insert(family) {
                items.push(Item::Block(BlockId(i as u16)));
            }
        }
        items
    })
}

/// Recherche sans tenir compte des majuscules ni des accents.
fn fold(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| match c {
            'à' | 'â' | 'ä' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'î' | 'ï' => 'i',
            'ô' | 'ö' => 'o',
            'ù' | 'û' | 'ü' => 'u',
            'ç' => 'c',
            c => c,
        })
        .collect()
}

/// Géométrie du catalogue créatif, en pixels.
struct Creative {
    s: f32,
    slot: f32,
    panel: (f32, f32, f32, f32),
    search: (f32, f32, f32, f32),
    grid: (f32, f32),
    scrollbar: (f32, f32, f32, f32),
    hotbar: (f32, f32),
    trash: (f32, f32),
}

const CAT_COLS: usize = 10;
const CAT_ROWS: usize = 6;

impl Creative {
    fn new(w: f32, h: f32) -> Self {
        let s = (h / 360.0).floor().max(1.0);
        let slot = 20.0 * s;
        let pad = 6.0 * s;
        let title = 14.0 * s;
        let gap = 6.0 * s;
        let pw = slot * (CAT_COLS + 1) as f32 + 2.0 * pad + 4.0 * s;
        let ph = pad + title + slot * CAT_ROWS as f32 + gap + slot + pad;
        let (px, py) = ((w - pw) / 2.0, (h - ph) / 2.0);
        let grid = (px + pad, py + pad + title);
        let right = grid.0 + slot * CAT_COLS as f32 + 4.0 * s;
        let hotbar = (grid.0, grid.1 + slot * CAT_ROWS as f32 + gap);
        Self {
            s,
            slot,
            panel: (px, py, pw, ph),
            search: (
                px + pad + 56.0 * s,
                py + pad - s,
                pw - 2.0 * pad - 56.0 * s,
                11.0 * s,
            ),
            grid,
            scrollbar: (right + 6.0 * s, grid.1, 8.0 * s, slot * CAT_ROWS as f32),
            hotbar,
            trash: (right, hotbar.1),
        }
    }

    fn inside(r: (f32, f32), size: f32, x: f32, y: f32) -> bool {
        x >= r.0 && x < r.0 + size && y >= r.1 && y < r.1 + size
    }

    /// Case du catalogue sous le curseur (index affiché, 0..COLS×ROWS).
    fn cell_at(&self, x: f32, y: f32) -> Option<usize> {
        (0..CAT_COLS * CAT_ROWS).find(|&k| Self::inside(self.cell_pos(k), self.slot, x, y))
    }

    fn cell_pos(&self, k: usize) -> (f32, f32) {
        (
            self.grid.0 + (k % CAT_COLS) as f32 * self.slot,
            self.grid.1 + (k / CAT_COLS) as f32 * self.slot,
        )
    }

    fn hotbar_pos(&self, i: usize) -> (f32, f32) {
        (self.hotbar.0 + i as f32 * self.slot, self.hotbar.1)
    }

    fn hotbar_at(&self, x: f32, y: f32) -> Option<usize> {
        (0..HOTBAR).find(|&i| Self::inside(self.hotbar_pos(i), self.slot, x, y))
    }
}

/// Géométrie de l'interface, en pixels.
struct Layout {
    s: f32,
    slot: f32,
    hotbar: (f32, f32),
    panel: (f32, f32, f32, f32),
    grid: (f32, f32),
    panel_hotbar: (f32, f32),
}

impl Layout {
    fn new(w: f32, h: f32) -> Self {
        let s = (h / 360.0).floor().max(1.0);
        let slot = 20.0 * s;
        let pad = 6.0 * s;
        let hotbar = ((w - slot * HOTBAR as f32) / 2.0, h - slot - 4.0 * s);
        let pw = slot * COLS as f32 + 2.0 * pad;
        let title = 12.0 * s;
        let gap = 6.0 * s;
        let ph = pad + title + slot * ROWS as f32 + gap + slot + pad;
        let (px, py) = ((w - pw) / 2.0, (h - ph) / 2.0);
        let grid = (px + pad, py + pad + title);
        let panel_hotbar = (px + pad, grid.1 + slot * ROWS as f32 + gap);
        Self {
            s,
            slot,
            hotbar,
            panel: (px, py, pw, ph),
            grid,
            panel_hotbar,
        }
    }

    /// Coin haut-gauche de l'emplacement `i` (inventaire ouvert ou non).
    fn slot_pos(&self, i: usize, open: bool) -> (f32, f32) {
        if i < HOTBAR {
            let (x, y) = if open { self.panel_hotbar } else { self.hotbar };
            (x + i as f32 * self.slot, y)
        } else {
            let k = i - HOTBAR;
            (
                self.grid.0 + (k % COLS) as f32 * self.slot,
                self.grid.1 + (k / COLS) as f32 * self.slot,
            )
        }
    }

    fn slot_at(&self, x: f32, y: f32, open: bool) -> Option<usize> {
        let n = if open { SLOTS } else { HOTBAR };
        (0..n).find(|&i| {
            let (sx, sy) = self.slot_pos(i, open);
            x >= sx && x < sx + self.slot && y >= sy && y < sy + self.slot
        })
    }
}

/// Couleur d'interface : sRGB → linéaire (la surface est sRGB).
fn col(c: [u8; 3], a: f32) -> [f32; 4] {
    let l = |v: u8| (v as f32 / 255.0).powf(2.2);
    [l(c[0]), l(c[1]), l(c[2]), a]
}

fn shade(c: [u8; 3], k: f32) -> [u8; 3] {
    c.map(|v| (v as f32 * k).clamp(0.0, 255.0) as u8)
}

impl Inventory {
    pub fn empty() -> Self {
        Self {
            slots: [None; SLOTS],
            selected: 0,
            open: false,
            held: None,
            name_timer: 0.0,
            creative: false,
            search: String::new(),
            scroll_row: 0,
            dragging: false,
        }
    }

    /// Souris dans le catalogue créatif ouvert (à chaque image) : clic sur la barre de
    /// défilement puis glisser, la liste suit le curseur ; un clic dans la barre y saute.
    pub fn pointer(
        &mut self,
        size: (f32, f32),
        cursor: Option<(f32, f32)>,
        pressed: bool,
        held: bool,
    ) {
        if !(self.open && self.creative) {
            self.dragging = false;
            return;
        }
        let c = Creative::new(size.0, size.1);
        let (bx, by, bw, bh) = c.scrollbar;
        if let Some((x, y)) = cursor {
            // Zone de prise un peu plus large que la barre (facile à attraper).
            if pressed && x >= bx - 3.0 * c.s && x <= bx + bw + 3.0 * c.s && y >= by && y <= by + bh
            {
                self.dragging = true;
            }
            if self.dragging {
                let rows = self.filtered().len().div_ceil(CAT_COLS);
                let max = rows.saturating_sub(CAT_ROWS);
                let frac = ((y - by) / bh).clamp(0.0, 1.0);
                // Le milieu du curseur de la barre suit la souris.
                let first = frac * rows as f32 - CAT_ROWS as f32 / 2.0;
                self.scroll_row = (first.round().max(0.0) as usize).min(max);
            }
        }
        if !held {
            self.dragging = false;
        }
    }

    /// Objets du catalogue qui correspondent à la recherche.
    pub fn filtered(&self) -> Vec<Item> {
        let q = fold(self.search.trim());
        catalogue()
            .iter()
            .copied()
            .filter(|it| {
                q.is_empty() || fold(&it.name()).contains(&q) || {
                    match it {
                        Item::Block(b) => fold(&b.def().key).contains(&q),
                        Item::Torch(_) => "torche".contains(&q),
                        Item::Wand => "baguette wand worldedit".contains(&q),
                    }
                }
            })
            .collect()
    }

    /// Saisie dans le catalogue créatif ouvert : texte de recherche, effacement, molette.
    pub fn search_input(&mut self, text: &str, backspace: bool, scroll: f32) {
        let before = self.search.clone();
        for c in text.chars().filter(|c| !c.is_control()) {
            if self.search.chars().count() < 30 {
                self.search.push(c);
            }
        }
        if backspace {
            self.search.pop();
        }
        if self.search != before {
            self.scroll_row = 0;
        }
        if scroll.abs() >= 1.0 {
            let rows = self.filtered().len().div_ceil(CAT_COLS);
            let max = rows.saturating_sub(CAT_ROWS);
            let next = self.scroll_row as i32 - scroll.signum() as i32;
            self.scroll_row = next.clamp(0, max as i32) as usize;
        }
    }

    pub fn starter() -> Self {
        let mut inv = Self::empty();
        let find = |f: &str| CHEMICALS.iter().position(|c| c.formula == f);
        let hotbar = [
            Item::Block(BlockId::STONE),
            Item::Block(BlockId::PLANKS),
            Item::Block(BlockId::BRICK),
            Item::Torch(None),
            Item::Torch(find("SrCl2")),
            Item::Torch(find("NaCl")),
            Item::Torch(find("H3BO3")),
            Item::Torch(find("CuCl2")),
            Item::Torch(find("CsCl")),
            Item::Torch(find("KCl")),
        ];
        for (i, item) in hotbar.into_iter().enumerate() {
            inv.slots[i] = Some(Stack {
                item,
                count: MAX_STACK,
            });
        }
        // Le reste : toutes les autres torches puis les blocs.
        let mut rest: Vec<Item> = (0..CHEMICALS.len()).map(|i| Item::Torch(Some(i))).collect();
        rest.extend(BlockId::BUILDING.iter().map(|&b| Item::Block(b)));
        for item in rest {
            if !hotbar.contains(&item) {
                inv.add(item, MAX_STACK);
            }
        }
        inv
    }

    pub fn selected_item(&self) -> Option<Item> {
        self.slots[self.selected].map(|s| s.item)
    }

    /// Retire un objet de l'emplacement choisi.
    pub fn take_selected(&mut self) -> Option<Item> {
        let slot = &mut self.slots[self.selected];
        let stack = slot.as_mut()?;
        let item = stack.item;
        stack.count -= 1;
        if stack.count == 0 {
            *slot = None;
        }
        Some(item)
    }

    /// Ajoute des objets (piles existantes d'abord, barre puis inventaire). Renvoie le surplus.
    pub fn add(&mut self, item: Item, mut count: u32) -> u32 {
        for slot in self.slots.iter_mut().flatten() {
            if slot.item == item && slot.count < MAX_STACK {
                let n = (MAX_STACK - slot.count).min(count);
                slot.count += n;
                count -= n;
                if count == 0 {
                    return 0;
                }
            }
        }
        for slot in self.slots.iter_mut() {
            if slot.is_none() {
                let n = count.min(MAX_STACK);
                *slot = Some(Stack { item, count: n });
                count -= n;
                if count == 0 {
                    return 0;
                }
            }
        }
        count
    }

    pub fn select(&mut self, i: usize) {
        if i < HOTBAR && i != self.selected {
            self.selected = i;
            self.name_timer = 2.0;
        }
    }

    pub fn scroll(&mut self, steps: i32) {
        let i = (self.selected as i32 + steps).rem_euclid(HOTBAR as i32) as usize;
        self.select(i);
    }

    pub fn toggle(&mut self) {
        if self.open {
            self.close();
        } else {
            self.open = true;
        }
    }

    pub fn close(&mut self) {
        self.open = false;
        self.search.clear();
        self.scroll_row = 0;
        if let Some(h) = self.held.take() {
            self.add(h.item, h.count);
        }
    }

    pub fn update(&mut self, dt: f32) {
        self.name_timer = (self.name_timer - dt).max(0.0);
    }

    /// Clic dans l'inventaire ouvert. `right` : clic droit (moitié / un seul objet),
    /// `shift` : Maj enfoncée (catalogue : pile directement dans la barre ; poubelle : tout
    /// vider).
    pub fn click(&mut self, size: (f32, f32), cursor: (f32, f32), right: bool, shift: bool) {
        if self.creative {
            self.click_creative(size, cursor, right, shift);
            return;
        }
        let layout = Layout::new(size.0, size.1);
        let Some(i) = layout.slot_at(cursor.0, cursor.1, true) else {
            return;
        };
        self.click_slot(i, right);
    }

    fn click_creative(&mut self, size: (f32, f32), cursor: (f32, f32), right: bool, shift: bool) {
        let c = Creative::new(size.0, size.1);
        let (x, y) = cursor;
        // Poubelle : détruit l'objet tenu ; Maj + clic : vide tout l'inventaire.
        if Creative::inside(c.trash, c.slot, x, y) {
            if self.held.take().is_none() && shift {
                self.slots = [None; SLOTS];
            }
            return;
        }
        if let Some(k) = c.cell_at(x, y) {
            let list = self.filtered();
            let Some(&item) = list.get(self.scroll_row * CAT_COLS + k) else {
                return;
            };
            // Objet tenu relâché sur le catalogue : il disparaît (comme dans Minecraft).
            if self.held.take().is_some() {
                return;
            }
            let count = if right { 1 } else { MAX_STACK };
            if shift {
                let slot = (0..HOTBAR)
                    .find(|&i| self.slots[i].is_none())
                    .unwrap_or(self.selected);
                self.slots[slot] = Some(Stack {
                    item,
                    count: MAX_STACK,
                });
            } else {
                self.held = Some(Stack { item, count });
            }
            return;
        }
        if let Some(i) = c.hotbar_at(x, y) {
            self.click_slot(i, right);
        }
    }

    /// Clic sur l'emplacement `i` : prendre, poser, échanger, fusionner.
    fn click_slot(&mut self, i: usize, right: bool) {
        let slot = self.slots[i];
        match (self.held, slot, right) {
            (None, Some(s), false) => {
                self.held = Some(s);
                self.slots[i] = None;
            }
            (None, Some(s), true) => {
                let take = s.count.div_ceil(2);
                self.held = Some(Stack {
                    item: s.item,
                    count: take,
                });
                self.slots[i] = (s.count > take).then_some(Stack {
                    item: s.item,
                    count: s.count - take,
                });
            }
            (Some(h), None, false) => {
                self.slots[i] = Some(h);
                self.held = None;
            }
            (Some(h), None, true) => {
                self.slots[i] = Some(Stack {
                    item: h.item,
                    count: 1,
                });
                self.held = (h.count > 1).then_some(Stack {
                    item: h.item,
                    count: h.count - 1,
                });
            }
            (Some(h), Some(s), _) if h.item == s.item => {
                let n = if right { 1 } else { h.count }.min(MAX_STACK - s.count);
                self.slots[i] = Some(Stack {
                    item: s.item,
                    count: s.count + n,
                });
                self.held = (h.count > n).then_some(Stack {
                    item: h.item,
                    count: h.count - n,
                });
            }
            (Some(h), Some(s), false) => {
                self.slots[i] = Some(h);
                self.held = Some(s);
            }
            _ => {}
        }
    }

    // ------------------------------------------------------------ dessin

    pub fn draw(&self, ui: &mut Ui, size: (f32, f32), cursor: Option<(f32, f32)>) {
        if self.open && self.creative {
            self.draw_creative(ui, size, cursor);
            return;
        }
        let l = Layout::new(size.0, size.1);
        let s = l.s;
        let hover = cursor
            .filter(|_| self.open)
            .and_then(|(x, y)| l.slot_at(x, y, true));

        if self.open {
            ui.rect(0.0, 0.0, size.0, size.1, [0.0, 0.0, 0.0, 0.45]);
            let (px, py, pw, ph) = l.panel;
            ui.nine_slice("gui/panel", px, py, pw, ph, 4.0, s);
            ui.text_flat(
                l.grid.0,
                py + 4.0 * s,
                s,
                col([55, 55, 62], 1.0),
                "Inventaire",
            );
            for i in 0..SLOTS {
                let (x, y) = l.slot_pos(i, true);
                self.draw_slot(
                    ui,
                    &l,
                    x,
                    y,
                    self.slots[i],
                    i == self.selected,
                    hover == Some(i),
                );
            }
        } else {
            for i in 0..HOTBAR {
                let (x, y) = l.slot_pos(i, false);
                self.draw_slot(ui, &l, x, y, self.slots[i], false, false);
            }
            // Cadre de l'emplacement choisi, par-dessus.
            let (x, y) = l.slot_pos(self.selected, false);
            ui.image(
                "gui/hotbar_selected",
                x - 2.0 * s,
                y - 2.0 * s,
                24.0 * s,
                24.0 * s,
                [1.0; 4],
            );
            // Nom de l'objet choisi, quelques secondes au-dessus de la barre.
            if self.name_timer > 0.0 {
                if let Some(item) = self.selected_item() {
                    let name = item.name();
                    let w = Ui::text_width(&name, s);
                    let a = self.name_timer.min(1.0);
                    let c = item.flame().map_or([240, 240, 240], brighten);
                    ui.text(
                        (size.0 - w) / 2.0,
                        l.hotbar.1 - 12.0 * s,
                        s,
                        col(c, a),
                        name,
                    );
                }
            }
        }

        if let Some((cx, cy)) = cursor.filter(|_| self.open) {
            if let Some(h) = self.held {
                let half = l.slot / 2.0;
                draw_icon(ui, cx - half, cy - half, l.slot, s, h);
            } else if let Some(stack) = hover.and_then(|i| self.slots[i]) {
                self.draw_tooltip(ui, size, (cx, cy), s, stack.item);
            }
        }
    }

    fn draw_creative(&self, ui: &mut Ui, size: (f32, f32), cursor: Option<(f32, f32)>) {
        let c = Creative::new(size.0, size.1);
        let s = c.s;
        // Mêmes cadres d'emplacement que l'inventaire (Layout ne sert qu'à la taille).
        let l = Layout::new(size.0, size.1);
        ui.rect(0.0, 0.0, size.0, size.1, [0.0, 0.0, 0.0, 0.45]);
        let (px, py, pw, ph) = c.panel;
        ui.nine_slice("gui/panel", px, py, pw, ph, 4.0, s);
        ui.text_flat(c.grid.0, py + 6.0 * s, s, col([55, 55, 62], 1.0), "Créatif");
        // Champ de recherche.
        let (sx, sy, sw, sh) = c.search;
        ui.rect(sx, sy, sw, sh, col([20, 20, 24], 1.0));
        let (txt, tc) = if self.search.is_empty() {
            (
                "Rechercher (tapez)...".to_string(),
                col([140, 140, 150], 1.0),
            )
        } else {
            (format!("{}_", self.search), col([255, 255, 255], 1.0))
        };
        ui.text(sx + 2.0 * s, sy + 2.0 * s, s, tc, txt);

        let list = self.filtered();
        let (mx, my) = cursor.unwrap_or((-1.0, -1.0));
        let mut hover_item: Option<Item> = None;
        for k in 0..CAT_COLS * CAT_ROWS {
            let (x, y) = c.cell_pos(k);
            let item = list.get(self.scroll_row * CAT_COLS + k).copied();
            let hover = Creative::inside((x, y), c.slot, mx, my);
            if hover {
                hover_item = item;
            }
            self.draw_slot(
                ui,
                &l,
                x,
                y,
                item.map(|item| Stack { item, count: 1 }),
                false,
                hover,
            );
        }
        if list.is_empty() {
            ui.text(
                c.grid.0 + 4.0 * s,
                c.grid.1 + 6.0 * s,
                s,
                col([90, 90, 100], 1.0),
                "Aucun résultat",
            );
        }
        // Barre de défilement.
        let (bx, by, bw, bh) = c.scrollbar;
        ui.rect(bx, by, bw, bh, col([60, 60, 66], 1.0));
        let rows = list.len().div_ceil(CAT_COLS).max(1);
        let shown = (CAT_ROWS as f32 / rows as f32).min(1.0);
        let start = self.scroll_row as f32 / rows as f32;
        ui.rect(
            bx + s,
            by + bh * start,
            bw - 2.0 * s,
            (bh * shown).max(4.0 * s),
            col([200, 200, 210], 1.0),
        );
        // Barre d'objets.
        for i in 0..HOTBAR {
            let (x, y) = c.hotbar_pos(i);
            let hover = Creative::inside((x, y), c.slot, mx, my);
            if hover {
                hover_item = self.slots[i].map(|st| st.item);
            }
            self.draw_slot(ui, &l, x, y, self.slots[i], i == self.selected, hover);
        }
        // Poubelle.
        let (tx, ty) = c.trash;
        let over_trash = Creative::inside(c.trash, c.slot, mx, my);
        ui.image(
            if over_trash {
                "gui/slot_highlight"
            } else {
                "gui/slot"
            },
            tx + s,
            ty + s,
            18.0 * s,
            18.0 * s,
            [1.0; 4],
        );
        let red = col([200, 60, 55], 1.0);
        let dark = col([120, 30, 30], 1.0);
        ui.rect(tx + 5.0 * s, ty + 6.0 * s, 10.0 * s, 1.5 * s, red); // couvercle
        ui.rect(tx + 8.5 * s, ty + 4.5 * s, 3.0 * s, 1.5 * s, red); // poignée
        ui.rect(tx + 6.0 * s, ty + 8.0 * s, 8.0 * s, 8.0 * s, red); // cuve
        for k in 0..3 {
            ui.rect(
                tx + (7.5 + k as f32 * 2.0) * s,
                ty + 9.5 * s,
                s,
                5.0 * s,
                dark,
            );
        }

        if let Some((cx, cy)) = cursor {
            if let Some(h) = self.held {
                let half = c.slot / 2.0;
                draw_icon(ui, cx - half, cy - half, c.slot, s, h);
            } else if over_trash {
                let ts = (s / 2.0).floor().max(1.0);
                let lines = [
                    "Poubelle : déposez un objet pour le détruire",
                    "Maj + clic : vider tout l'inventaire",
                ];
                let w = lines
                    .iter()
                    .map(|t| Ui::text_width(t, ts))
                    .fold(0.0, f32::max)
                    + 8.0 * ts;
                let x = (cx - w - 8.0 * s).max(0.0);
                ui.rect(x, cy, w, 24.0 * ts, col([16, 10, 22], 0.96));
                for (k, t) in lines.iter().enumerate() {
                    ui.text(
                        x + 4.0 * ts,
                        cy + 4.0 * ts + k as f32 * 10.0 * ts,
                        ts,
                        col([240, 240, 240], 1.0),
                        *t,
                    );
                }
            } else if let Some(item) = hover_item {
                self.draw_tooltip(ui, size, (cx, cy), s, item);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_slot(
        &self,
        ui: &mut Ui,
        l: &Layout,
        x: f32,
        y: f32,
        stack: Option<Stack>,
        selected: bool,
        hover: bool,
    ) {
        let s = l.s;
        let name = if hover {
            "gui/slot_highlight"
        } else {
            "gui/slot"
        };
        ui.image(name, x + s, y + s, 18.0 * s, 18.0 * s, [1.0; 4]);
        if selected {
            ui.image(
                "gui/hotbar_selected",
                x - 2.0 * s,
                y - 2.0 * s,
                24.0 * s,
                24.0 * s,
                [1.0; 4],
            );
        }
        if let Some(st) = stack {
            draw_icon(ui, x, y, l.slot, s, st);
        }
    }

    fn draw_tooltip(&self, ui: &mut Ui, size: (f32, f32), cursor: (f32, f32), s: f32, item: Item) {
        let lines = item.tooltip();
        let ts = (s / 2.0).floor().max(1.0);
        let line_h = 10.0 * ts;
        let pad = 4.0 * ts;
        let w = lines
            .iter()
            .map(|(t, _)| Ui::text_width(t, ts))
            .fold(0.0, f32::max)
            + 2.0 * pad;
        let h = lines.len() as f32 * line_h + 2.0 * pad;
        let mut x = cursor.0 + 12.0 * s;
        let mut y = cursor.1 - 4.0 * s;
        if x + w > size.0 {
            x = (cursor.0 - 12.0 * s - w).max(0.0);
        }
        if y + h > size.1 {
            y = size.1 - h;
        }
        let edge = item.flame().map_or([90, 60, 160], |c| shade(c, 0.8));
        ui.rect(x - ts, y - ts, w + 2.0 * ts, h + 2.0 * ts, col(edge, 0.95));
        ui.rect(x, y, w, h, col([16, 10, 22], 0.96));
        for (k, (t, c)) in lines.iter().enumerate() {
            ui.text(
                x + pad,
                y + pad + k as f32 * line_h,
                ts,
                col(*c, 1.0),
                t.clone(),
            );
        }
    }
}

/// Icône 16 × 16 (×`s`) centrée dans un emplacement, avec le nombre d'objets.
fn draw_icon(ui: &mut Ui, x: f32, y: f32, slot: f32, s: f32, st: Stack) {
    match st.item {
        // Torche : même modèle 3D animé que posée.
        Item::Torch(chem) => ui.model(crate::hand::torch_icon(
            x + slot / 2.0,
            y + slot / 2.0,
            15.0 * s,
            chem,
        )),
        // Baguette : image en pixels.
        Item::Wand => crate::wand::icon(ui, x + slot / 2.0, y + slot / 2.0, 16.0 * s),
        // Bloc : vrai modèle 3D, net à toutes les résolutions.
        Item::Block(b) => ui.model(crate::hand::block_icon(
            x + slot / 2.0,
            y + slot / 2.0,
            16.0 * s,
            b,
        )),
    }
    let ts = (s / 2.0).floor().max(1.0);
    if let Item::Torch(Some(i)) = st.item {
        let c = brighten(CHEMICALS[i].rgb);
        ui.text(x + s + ts, y + s + ts, ts, col(c, 1.0), CHEMICALS[i].symbol);
    }
    if st.count > 1 {
        let t = st.count.to_string();
        let w = Ui::text_width(&t, ts);
        ui.text(
            x + slot - w - 2.0 * ts,
            y + slot - 9.0 * ts,
            ts,
            col([255, 255, 255], 1.0),
            t,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starter_has_all_torches_and_fits() {
        let inv = Inventory::starter();
        let items: Vec<Item> = inv.slots.iter().flatten().map(|s| s.item).collect();
        assert!(items.contains(&Item::Torch(None)));
        for i in 0..CHEMICALS.len() {
            assert!(
                items.contains(&Item::Torch(Some(i))),
                "torche {i} manquante"
            );
        }
        assert!(items.len() <= SLOTS);
    }

    #[test]
    fn add_take_and_merge() {
        let mut inv = Inventory::empty();
        assert_eq!(inv.add(Item::Block(BlockId::STONE), 70), 0);
        assert_eq!(inv.slots[0].unwrap().count, 64);
        assert_eq!(inv.slots[1].unwrap().count, 6);
        assert_eq!(inv.take_selected(), Some(Item::Block(BlockId::STONE)));
        assert_eq!(inv.slots[0].unwrap().count, 63);
    }

    #[test]
    fn click_moves_stacks() {
        let mut inv = Inventory::starter();
        inv.open = true;
        let size = (1280.0, 720.0);
        let l = Layout::new(size.0, size.1);
        let center = |i: usize| {
            let (x, y) = l.slot_pos(i, true);
            (x + l.slot / 2.0, y + l.slot / 2.0)
        };
        let first = inv.slots[0];
        let empty = (0..SLOTS).rev().find(|&i| inv.slots[i].is_none()).unwrap();
        inv.click(size, center(0), false, false);
        assert!(inv.slots[0].is_none());
        inv.click(size, center(empty), false, false);
        assert_eq!(inv.slots[empty], first);
        // Clic droit : prend la moitié.
        inv.click(size, center(empty), true, false);
        assert_eq!(inv.slots[empty].unwrap().count, 32);
        inv.close();
        let total: u32 = inv
            .slots
            .iter()
            .flatten()
            .filter(|s| s.item == first.unwrap().item)
            .map(|s| s.count)
            .sum();
        assert_eq!(total, 64);
    }

    #[test]
    fn creative_catalogue_search_and_trash() {
        let cat = catalogue();
        // Toutes les torches, et une seule entrée par famille (pas 8 escaliers par matériau).
        assert!(cat.contains(&Item::Torch(None)));
        let stairs = cat
            .iter()
            .filter(|i| matches!(i, Item::Block(b) if b.def().key.starts_with("stairs:planks_oak")))
            .count();
        assert_eq!(stairs, 1);
        assert!(!cat
            .iter()
            .any(|i| matches!(i, Item::Block(b) if b.def().key.starts_with("wall_torch"))));
        let mut inv = Inventory::empty();
        inv.creative = true;
        inv.open = true;
        inv.search_input("escalier", false, 0.0);
        assert!(!inv.filtered().is_empty());
        assert!(inv
            .filtered()
            .iter()
            .all(|i| fold(&i.name()).contains("escalier")));
        inv.search.clear();
        inv.search_input("ESCALIER planche", false, 0.0);
        inv.search.clear();
        // Prendre une pile dans le catalogue, la poser dans la barre, puis la jeter.
        let size = (1280.0, 720.0);
        let c = Creative::new(size.0, size.1);
        let mid = |p: (f32, f32)| (p.0 + c.slot / 2.0, p.1 + c.slot / 2.0);
        inv.click(size, mid(c.cell_pos(0)), false, false);
        assert_eq!(inv.held.map(|h| h.count), Some(MAX_STACK));
        inv.click(size, mid(c.hotbar_pos(3)), false, false);
        assert!(inv.slots[3].is_some() && inv.held.is_none());
        inv.click(size, mid(c.hotbar_pos(3)), false, false);
        inv.click(size, mid(c.trash), false, false);
        assert!(inv.held.is_none() && inv.slots[3].is_none());
        // Barre de défilement : glisser jusqu'en bas affiche la fin de la liste.
        let (bx, by, bw, bh) = c.scrollbar;
        inv.pointer(size, Some((bx + bw / 2.0, by + 2.0)), true, true);
        assert_eq!(inv.scroll_row, 0);
        inv.pointer(size, Some((bx + bw / 2.0, by + bh)), false, true);
        let rows = inv.filtered().len().div_ceil(CAT_COLS);
        assert_eq!(inv.scroll_row, rows - CAT_ROWS);
        inv.pointer(size, Some((0.0, 0.0)), false, false);
        assert!(!inv.dragging);
        // Maj + clic sur la poubelle : tout vider.
        inv.click(size, mid(c.cell_pos(1)), false, true);
        assert!(inv.slots.iter().any(|s| s.is_some()));
        inv.click(size, mid(c.trash), false, true);
        assert!(inv.slots.iter().all(|s| s.is_none()));
    }

    #[test]
    fn torch_tooltip_mentions_salt_and_duration() {
        let li = CHEMICALS.iter().position(|c| c.symbol == "Li").unwrap();
        let t = Item::Torch(Some(li)).tooltip();
        let all: String = t
            .iter()
            .map(|(s, _)| s.as_str())
            .collect::<Vec<_>>()
            .join(" | ");
        assert!(all.contains("5 g de chlorure de lithium (LiCl)"), "{all}");
        assert!(all.contains("min"), "{all}");
        assert_eq!(Item::Torch(Some(li)).name(), "Torche au lithium (LiCl)");
        let ind = CHEMICALS.iter().position(|c| c.symbol == "In").unwrap();
        assert_eq!(Item::Torch(Some(ind)).name(), "Torche à l'indium (InCl3)");
    }
}
