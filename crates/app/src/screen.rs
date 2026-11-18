//! Écrans vidéo : l'objet « Écran vidéo » qu'on pose sur un mur ou un sol (« utiliser »,
//! clic droit), qu'on touche en le visant (« utiliser » : écran tactile) et qu'on casse
//! comme un bloc (« casser », clic gauche ; l'objet revient, hors créatif). Accroupi,
//! « utiliser » l'ignore et pose le bloc tenu devant.
//!
//! Chaque écran posé a une hitbox : une boîte fine collée à son mur, qu'on vise (contour
//! comme un bloc), qui arrête le joueur et qu'on casse. Les écrans sont enregistrés avec
//! le monde (`screens.txt`).
//!
//! [`Displays`] est ce qu'affichent les écrans : chacun a sa propre page, un onglet du
//! navigateur Chromium (programme `minelib-browser`, lancé à part), ou sa mire si le
//! navigateur n'est pas installé. Il reçoit le survol, l'appui, le relâchement et la
//! molette en coordonnées de l'écran (0..1, origine en haut à gauche) et les transmet à la
//! page. Un clic dans un champ de texte y envoie le clavier (`editing`) tant que le
//! réticule reste sur l'écran ; un clic ailleurs dans la page ou Échap le rend au jeu.

use std::path::Path;

use glam::{Mat4, Vec3};
use minelib_render::{ScreenAnchor, Ui, VmVertex};

/// Taille d'un écran : 16:9, 4 blocs de large.
const WIDTH: f32 = 4.0;
const HEIGHT: f32 = 2.25;
/// Décollé de sa face (pas de scintillement avec le bloc) et épaisseur de sa hitbox.
const GAP: f32 = 0.02;
const THICK: f32 = 0.06;
/// Au plus tant d'écrans par monde.
pub const MAX_SCREENS: usize = 64;
/// Fichier des écrans, dans le dossier du monde.
const FILE: &str = "screens.txt";

/// Un écran posé : le bloc qui le porte, la face où il est posé (normale) et la direction
/// vers laquelle il regarde (horizontale, sur un axe).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Placed {
    pub block: [i32; 3],
    pub normal: [i32; 3],
    pub facing: [i32; 3],
}

impl Placed {
    /// Écran posé sur la face `normal` du bloc `block`. Sur un mur il regarde hors du mur ;
    /// sur un sol ou un plafond il se dresse, tourné vers le joueur (`eye`), sur l'axe le
    /// plus proche (comme les panneaux).
    pub fn new(block: [i32; 3], normal: [i32; 3], eye: Vec3) -> Self {
        let facing = if normal[1] == 0 {
            normal
        } else {
            let face = center_of(block) + ivec(normal) * 0.5;
            let d = eye - face;
            if d.x.abs() >= d.z.abs() {
                [d.x.signum() as i32, 0, 0]
            } else {
                [0, 0, d.z.signum() as i32]
            }
        };
        Self {
            block,
            normal,
            facing,
        }
    }

    /// Où dessiner l'écran.
    pub fn anchor(&self) -> ScreenAnchor {
        let n = ivec(self.normal);
        let f = ivec(self.facing);
        let face = center_of(self.block) + n * 0.5;
        let center = if self.normal[1] == 0 {
            face + n * GAP
        } else {
            face + Vec3::Y * n.y * (HEIGHT * 0.5 + GAP)
        };
        let up = Vec3::Y;
        let right = (-f).cross(up);
        ScreenAnchor {
            center: center.into(),
            right: right.into(),
            up: up.into(),
            size: [WIDTH, HEIGHT],
        }
    }

    /// Numéro de l'écran, tiré de sa place (bloc et face) : le même pour tous les joueurs
    /// et d'une partie à l'autre (FNV-1a).
    pub fn id(&self) -> u32 {
        let mut h: u32 = 0x811c_9dc5;
        for v in self.block.iter().chain(&self.normal) {
            for b in v.to_le_bytes() {
                h = (h ^ u32::from(b)).wrapping_mul(0x0100_0193);
            }
        }
        h
    }

    /// Hitbox : coins min et max (monde). Collée à sa face, `THICK` d'épaisseur.
    pub fn hitbox(&self) -> (Vec3, Vec3) {
        let a = self.anchor();
        let c = Vec3::from(a.center);
        let f = ivec(self.facing);
        let half =
            (Vec3::from(a.right) * WIDTH * 0.5).abs() + (Vec3::from(a.up) * HEIGHT * 0.5).abs();
        // Sur un mur, de la face du bloc jusqu'à un peu devant l'écran ; debout, centrée.
        let (back, front) = if self.normal[1] == 0 {
            (GAP, THICK - GAP)
        } else {
            (THICK * 0.5, THICK * 0.5)
        };
        let p0 = c - f * back - half;
        let p1 = c + f * front + half;
        (p0.min(p1), p0.max(p1))
    }
}

fn ivec(v: [i32; 3]) -> Vec3 {
    Vec3::new(v[0] as f32, v[1] as f32, v[2] as f32)
}

fn center_of(b: [i32; 3]) -> Vec3 {
    Vec3::new(b[0] as f32 + 0.5, b[1] as f32 + 0.5, b[2] as f32 + 0.5)
}

/// Écrans enregistrés dans le dossier du monde (aucun si le fichier manque).
pub fn load(dir: &Path) -> Vec<Placed> {
    let Ok(text) = std::fs::read_to_string(dir.join(FILE)) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|l| {
            let n: Vec<i32> = l
                .split_whitespace()
                .map(str::parse)
                .collect::<Result<_, _>>()
                .ok()?;
            (n.len() == 9).then(|| Placed {
                block: [n[0], n[1], n[2]],
                normal: [n[3], n[4], n[5]],
                facing: [n[6], n[7], n[8]],
            })
        })
        .take(MAX_SCREENS)
        .collect()
}

/// Enregistre les écrans dans le dossier du monde.
pub fn save(dir: &Path, screens: &[Placed]) {
    let mut text = String::from("# Écrans vidéo : bloc (x y z), face (normale), direction\n");
    for s in screens {
        let [b, n, f] = [s.block, s.normal, s.facing];
        text += &format!(
            "{} {} {} {} {} {} {} {} {}\n",
            b[0], b[1], b[2], n[0], n[1], n[2], f[0], f[1], f[2]
        );
    }
    if let Err(e) = std::fs::write(dir.join(FILE), text) {
        log::warn!("écrans non enregistrés : {e}");
    }
}

/// Point de l'écran `a` visé par le rayon `eye + t·dir` (0 < t ≤ `limit`) : distance `t`
/// et coordonnées de l'écran (0..1, origine en haut à gauche). `None` si le rayon passe
/// à côté, ou arrive par le dos ou la tranche.
pub fn aim(a: &ScreenAnchor, eye: Vec3, dir: Vec3, limit: f32) -> Option<(f32, [f32; 2])> {
    let c = Vec3::from(a.center);
    let r = Vec3::from(a.right);
    let u = Vec3::from(a.up);
    // Face visible : vers le spectateur.
    let n = r.cross(u);
    let denom = dir.dot(n);
    if denom >= -1e-6 {
        return None;
    }
    let t = (c - eye).dot(n) / denom;
    if t <= 0.0 || t > limit {
        return None;
    }
    let p = eye + dir * t - c;
    let x = p.dot(r) / a.size[0] + 0.5;
    let y = 0.5 - p.dot(u) / a.size[1];
    ((0.0..=1.0).contains(&x) && (0.0..=1.0).contains(&y)).then_some((t, [x, y]))
}

// ----------------------------------------------------------------------------
// L'objet « Écran vidéo » : petite télé en pixel art 16 × 16 (icône et objet en main).
// ----------------------------------------------------------------------------

/// `K` cadre, `S` dalle, `h` reflet, `k` pied. `.` transparent.
const PIXELS: [&str; 16] = [
    "................",
    "................",
    ".KKKKKKKKKKKKKK.",
    ".KSSSSSSSSSSSSK.",
    ".KShhSSSSSSSSSK.",
    ".KShSSSSSSSSSSK.",
    ".KSSSSSSSSSSSSK.",
    ".KSSSSSSSSSSSSK.",
    ".KSSSSSSSSSSSSK.",
    ".KSSSSSSSSSSSSK.",
    ".KKKKKKKKKKKKKK.",
    "......kkkk......",
    ".....kkkkkk.....",
    "................",
    "................",
    "................",
];

/// Pixels pleins : (colonne, ligne, couleur linéaire).
fn pixels() -> impl Iterator<Item = (usize, usize, [f32; 3])> {
    PIXELS.iter().enumerate().flat_map(|(y, row)| {
        row.chars().enumerate().filter_map(move |(x, c)| {
            let srgb: [u8; 3] = match c {
                'K' => [35, 35, 40],
                'S' => [40, 90, 170],
                'h' => [140, 190, 240],
                'k' => [60, 60, 68],
                _ => return None,
            };
            Some((x, y, srgb.map(|v| (v as f32 / 255.0).powf(2.2))))
        })
    })
}

/// Icône dans un emplacement d'inventaire : `size` pixels de côté, centrée.
pub fn icon(ui: &mut Ui, cx: f32, cy: f32, size: f32) {
    crate::wand::pixel_icon(ui, cx, cy, size, pixels());
}

/// Écran tenu en main (objet plat en relief, comme la baguette).
pub fn model(out: &mut Vec<VmVertex>, mvp: Mat4, light: [f32; 3]) {
    crate::wand::pixel_model(out, mvp, light, pixels());
}

// ----------------------------------------------------------------------------
// Ce qu'affichent les écrans : chacun sa page (YouTube), sinon sa mire.
// ----------------------------------------------------------------------------

/// Résolution de la mire (16:9).
const W: u32 = 640;
const H: u32 = 360;
/// Taille des pages du navigateur (16:9).
const PAGE_W: u32 = 1280;
const PAGE_H: u32 = 720;
/// Page ouverte sur un nouvel écran.
pub const HOME_URL: &str = "https://www.youtube.com/";
/// Durée de l'onde qui montre un appui (secondes).
const RIPPLE_S: f32 = 0.35;
/// Durée d'un tour de la barre de progression de la mire (secondes).
const LOOP_S: f32 = 20.0;

/// Adresse à ouvrir pour `/yt <texte>` : rien → l'accueil de YouTube ; une adresse web →
/// elle-même ; sinon une recherche YouTube.
pub fn youtube_url(query: &str) -> String {
    let q = query.trim();
    if q.is_empty() {
        return HOME_URL.to_string();
    }
    if q.starts_with("https://") || q.starts_with("http://") {
        return q.to_string();
    }
    let mut enc = String::new();
    for b in q.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                enc.push(b as char)
            }
            b' ' => enc.push('+'),
            _ => enc.push_str(&format!("%{b:02X}")),
        }
    }
    format!("https://www.youtube.com/results?search_query={enc}")
}

/// Point de la page (pixels) sous le point `uv` de l'écran.
fn page_point(uv: [f32; 2]) -> (i32, i32) {
    let x = (uv[0].clamp(0.0, 1.0) * PAGE_W as f32) as i32;
    let y = (uv[1].clamp(0.0, 1.0) * PAGE_H as f32) as i32;
    (x.min(PAGE_W as i32 - 1), y.min(PAGE_H as i32 - 1))
}

/// Répétition d'une touche tenue (comme dans le tchat) : vrai à l'appui, puis toutes les
/// 35 ms après 0,4 s. `state` : touche répétée et temps écoulé.
pub fn key_repeat(
    state: &mut Option<(minelib_platform::KeyCode, f32)>,
    input: &minelib_platform::Input,
    k: minelib_platform::KeyCode,
    dt: f32,
) -> bool {
    if input.pressed(k) {
        *state = Some((k, -0.4));
        return true;
    }
    match state {
        Some((rk, t)) if *rk == k && input.held(k) => {
            *t += dt;
            if *t >= 0.035 {
                *t = 0.0;
                true
            } else {
                false
            }
        }
        _ => false,
    }
}

/// Ce qu'affichent les écrans : chacun a son onglet dans le navigateur (programme
/// `minelib-browser`, Chromium) s'il est installé, sinon sa mire animée. Tactiles.
pub struct Displays {
    /// Navigateur lancé ; `browser_tried` : lancement déjà tenté (pas de relance en boucle).
    browser: Option<Browser>,
    browser_tried: bool,
    /// Un onglet par écran, par numéro d'écran.
    tabs: std::collections::HashMap<u32, Tab>,
    /// États rapportés par les pages depuis le dernier `take_states` (regarder ensemble).
    states: Vec<(u32, String)>,
}

impl Displays {
    pub fn new() -> Self {
        Self {
            browser: None,
            browser_tried: false,
            tabs: std::collections::HashMap::new(),
            states: Vec::new(),
        }
    }

    /// Les écrans posés (leurs numéros) : un onglet pour chaque nouveau, fermé pour chaque
    /// écran retiré. Le navigateur est lancé avec le premier écran, arrêté avec le dernier.
    pub fn sync(&mut self, ids: &[u32]) {
        if ids.is_empty() {
            if !self.tabs.is_empty() || self.browser.is_some() {
                self.tabs.clear();
                self.browser = None;
            }
            self.browser_tried = false;
            return;
        }
        if !self.browser_tried {
            self.browser_tried = true;
            self.browser = Browser::start();
            if self.browser.is_none() {
                log::info!("écrans vidéo : navigateur absent (dossier « cef »), mires affichées");
            }
            // Les onglets déjà là (navigateur relancé) rouvrent leur page.
            for (id, tab) in &self.tabs {
                if let Some(b) = self.browser.as_mut() {
                    b.send(&format!("open {id} {}", tab.url));
                }
            }
        }
        let gone: Vec<u32> = self
            .tabs
            .keys()
            .copied()
            .filter(|id| !ids.contains(id))
            .collect();
        for id in gone {
            self.tabs.remove(&id);
            if let Some(b) = self.browser.as_mut() {
                b.send(&format!("close {id}"));
            }
        }
        for &id in ids {
            if !self.tabs.contains_key(&id) {
                self.tabs.insert(id, Tab::new(HOME_URL));
                if let Some(b) = self.browser.as_mut() {
                    b.send(&format!("open {id} {HOME_URL}"));
                }
            }
        }
    }

    /// Ouvre `url` sur l'écran `id`. `false` : navigateur absent.
    pub fn open(&mut self, id: u32, url: &str) -> bool {
        if let Some(tab) = self.tabs.get_mut(&id) {
            tab.url = url.to_string();
        }
        match self.browser.as_mut() {
            Some(b) => b.send(&format!("open {id} {url}")),
            None => false,
        }
    }

    /// Le réticule survole le point `uv` de l'écran `id` (`None` : d'aucun écran).
    pub fn hover(&mut self, aim: Option<(u32, [f32; 2])>) {
        for (id, tab) in self.tabs.iter_mut() {
            let uv = aim.filter(|a| a.0 == *id).map(|a| a.1);
            if uv != tab.cursor {
                tab.cursor = uv;
                tab.dirty = true;
            }
            let Some(p) = uv.map(page_point) else {
                continue;
            };
            if tab.sent != Some(p) {
                if let Some(b) = self.browser.as_mut() {
                    b.send(&format!("move {id} {} {}", p.0, p.1));
                    tab.sent = Some(p);
                }
            }
        }
    }

    /// Appui sur l'écran `id` : clic dans sa page ; sur sa mire, pause / lecture.
    pub fn press(&mut self, id: u32, uv: [f32; 2]) {
        let Some(tab) = self.tabs.get_mut(&id) else {
            return;
        };
        tab.ripple = Some((uv, 0.0));
        tab.dirty = true;
        let p = page_point(uv);
        match self.browser.as_mut() {
            Some(b) => {
                b.send(&format!("move {id} {} {}", p.0, p.1));
                b.send(&format!("down {id} {} {}", p.0, p.1));
                tab.sent = Some(p);
            }
            None => tab.paused = !tab.paused,
        }
    }

    /// Fin de l'appui (`uv` : `None` si le réticule a quitté l'écran entre-temps).
    pub fn release(&mut self, id: u32, uv: Option<[f32; 2]>) {
        let Some(tab) = self.tabs.get(&id) else {
            return;
        };
        let Some(p) = uv.map(page_point).or(tab.sent) else {
            return;
        };
        if let Some(b) = self.browser.as_mut() {
            b.send(&format!("up {id} {} {}", p.0, p.1));
        }
    }

    /// Molette sur l'écran `id` : fait défiler sa page (`dy` : crans × 120, positif vers
    /// le haut).
    pub fn wheel(&mut self, id: u32, uv: [f32; 2], dy: i32) {
        let p = page_point(uv);
        if let Some(b) = self.browser.as_mut() {
            b.send(&format!("wheel {id} {} {} {dy}", p.0, p.1));
        }
    }

    /// Un champ de texte de la page de l'écran `id` a-t-il le focus (le clavier y va) ?
    pub fn editing(&self, id: u32) -> bool {
        self.browser.is_some() && self.tabs.get(&id).is_some_and(|t| t.editing)
    }

    /// Texte tapé dans le champ de texte de l'écran `id`.
    pub fn text(&mut self, id: u32, text: &str) {
        if let Some(b) = self.browser.as_mut() {
            b.send(&format!("text {id} {text}"));
        }
    }

    /// Touche spéciale (`back`, `enter`, `ctrl+v`…) pour l'écran `id`.
    pub fn key(&mut self, id: u32, name: &str) {
        if let Some(b) = self.browser.as_mut() {
            b.send(&format!("key {id} {name}"));
        }
    }

    /// Le champ de texte de l'écran `id` perd le focus : le clavier revient au jeu.
    pub fn blur(&mut self, id: u32) {
        if let Some(tab) = self.tabs.get_mut(&id) {
            tab.editing = false;
        }
        if let Some(b) = self.browser.as_mut() {
            b.send(&format!("blur {id}"));
        }
    }

    /// Lance le script `js` dans la page de l'écran `id` (regarder ensemble : temps et
    /// pause de la vidéo).
    pub fn eval(&mut self, id: u32, js: &str) {
        if !self.tabs.contains_key(&id) {
            return;
        }
        if let Some(b) = self.browser.as_mut() {
            b.send(&format!("eval {id} {js}"));
        }
    }

    /// États rapportés par les pages depuis le dernier appel (adresse, temps de la vidéo,
    /// pause, pub ; `{"u":…,"t":…,"p":…,"a":…,"i":…}`), dans l'ordre.
    pub fn take_states(&mut self) -> Vec<(u32, String)> {
        std::mem::take(&mut self.states)
    }

    /// Image actuelle de l'écran `id` : version, largeur, hauteur, pixels BGRA.
    pub fn frame(&self, id: u32) -> Option<(u64, u32, u32, &[u8])> {
        let t = self.tabs.get(&id)?;
        Some((t.version, t.size.0, t.size.1, &t.pixels))
    }

    /// Avance d'une image : reçoit pages et évènements du navigateur, redessine.
    pub fn update(&mut self, dt: f32) {
        if let Some(b) = self.browser.as_mut() {
            if !b.alive() {
                log::warn!("écrans vidéo : le navigateur s'est arrêté (voir logs/navigateur*.log)");
                // `browser_tried` reste vrai : pas de relance en boucle.
                self.browser = None;
                for tab in self.tabs.values_mut() {
                    tab.page.clear();
                    tab.editing = false;
                    tab.dirty = true;
                }
            } else {
                let (frames, events) = b.take();
                for (id, (w, h, px)) in frames {
                    if let Some(tab) = self.tabs.get_mut(&id) {
                        tab.page = px;
                        tab.page_size = (w, h);
                        tab.dirty = true;
                    }
                }
                for (id, ev) in events {
                    let Some(tab) = self.tabs.get_mut(&id) else {
                        continue;
                    };
                    match ev {
                        Event::Editing(on) => tab.editing = on,
                        Event::State(s) => self.states.push((id, s)),
                    }
                }
            }
        }
        for tab in self.tabs.values_mut() {
            tab.update(dt);
        }
    }
}

/// Ce qu'affiche un écran.
struct Tab {
    /// Adresse ouverte (rouverte si le navigateur est relancé).
    url: String,
    /// Image affichée (BGRA), de taille `size`.
    pixels: Vec<u8>,
    size: (u32, u32),
    /// Dernière image de la page (BGRA) et sa taille ; vide tant qu'il n'y en a pas.
    page: Vec<u8>,
    page_size: (u32, u32),
    /// Mire : temps de « lecture » (n'avance pas en pause) et pause.
    time: f32,
    paused: bool,
    /// Point visé (curseur), s'il y en a un, et dernier point envoyé au navigateur.
    cursor: Option<[f32; 2]>,
    sent: Option<(i32, i32)>,
    /// Dernier appui : point et âge (secondes).
    ripple: Option<([f32; 2], f32)>,
    /// Un champ de texte de la page a le focus.
    editing: bool,
    /// À redessiner (nouvelle image de la page, curseur déplacé…).
    dirty: bool,
    /// Change à chaque nouvelle image (le rendu ne renvoie que les nouvelles).
    version: u64,
}

impl Tab {
    fn new(url: &str) -> Self {
        let mut t = Self {
            url: url.to_string(),
            pixels: Vec::new(),
            size: (W, H),
            page: Vec::new(),
            page_size: (0, 0),
            time: 0.0,
            paused: false,
            cursor: None,
            sent: None,
            ripple: None,
            editing: false,
            dirty: true,
            version: 0,
        };
        t.draw();
        t
    }

    fn update(&mut self, dt: f32) {
        if let Some((_, age)) = &mut self.ripple {
            *age += dt;
            self.dirty = true;
        }
        if self.ripple.is_some_and(|r| r.1 > RIPPLE_S) {
            self.ripple = None;
        }
        if self.page.is_empty() {
            // Mire (aussi le temps que la première page arrive).
            if !self.paused {
                self.time += dt;
            }
            self.dirty = true;
        }
        if self.dirty {
            self.draw();
        }
    }

    fn draw(&mut self) {
        if self.page.is_empty() {
            self.draw_mire();
        } else {
            self.size = self.page_size;
            self.pixels.clear();
            self.pixels.extend_from_slice(&self.page);
        }
        // Les anneaux gardent la même taille à l'écran quelle que soit la résolution.
        let k = self.size.0 as f32 / W as f32;
        // Onde de l'appui : cercle qui s'agrandit et s'efface.
        if let Some((uv, age)) = self.ripple {
            let t = age / RIPPLE_S;
            let shade = (255.0 * (1.0 - t)) as u8;
            self.ring(uv, (8.0 + 40.0 * t) * k, 3.0 * k, [shade, shade, shade]);
        }
        // Curseur : petit anneau là où vise le réticule (bleu quand le clavier va dans
        // la page).
        if let Some(uv) = self.cursor {
            let c = if self.editing {
                [255, 170, 60]
            } else {
                [255, 255, 255]
            };
            self.ring(uv, 7.0 * k, 2.0 * k, c);
        }
        self.dirty = false;
        self.version += 1;
    }

    /// Mire de test : dégradé animé (figé et assombri en pause), barre de progression, et
    /// en pause un grand triangle « lecture ».
    fn draw_mire(&mut self) {
        self.size = (W, H);
        self.pixels.resize((W * H * 4) as usize, 0);
        let (w, h) = (W as i32, H as i32);
        let scroll = (self.time * 60.0) as i32;
        let bar = ((self.time / LOOP_S).fract() * w as f32) as i32;
        let dim: u8 = if self.paused { 2 } else { 1 };
        for y in 0..h {
            for x in 0..w {
                let (mut b, mut g, mut r) =
                    (30u8, (((x + y + scroll) / 3) % 256) as u8 / 2 + 20, 40u8);
                if y < 6 {
                    (b, g, r) = if x < bar {
                        (230, 230, 230)
                    } else {
                        (70, 70, 70)
                    };
                }
                self.put(x, y, [b / dim, g / dim, r / dim]);
            }
        }
        if self.paused {
            let (cx, cy) = (w / 2, h / 2);
            let tri = h / 5;
            for y in cy - tri / 2..cy + tri / 2 {
                for x in cx - tri / 3..cx + 2 * tri / 3 {
                    let dx = x - (cx - tri / 3);
                    if (y - cy).abs() < (tri - dx) / 2 {
                        self.put(x, y, [240, 240, 240]);
                    }
                }
            }
        }
    }

    /// Anneau de rayon `radius` (pixels) et d'épaisseur `width` centré en `uv`.
    fn ring(&mut self, uv: [f32; 2], radius: f32, width: f32, bgr: [u8; 3]) {
        let (cx, cy) = (uv[0] * self.size.0 as f32, uv[1] * self.size.1 as f32);
        let reach = (radius + width) as i32 + 1;
        for y in cy as i32 - reach..=cy as i32 + reach {
            for x in cx as i32 - reach..=cx as i32 + reach {
                let d = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt();
                if (d - radius).abs() <= width * 0.5 {
                    self.put(x, y, bgr);
                }
            }
        }
    }

    /// Écrit un pixel (BGR, opaque) ; ignoré hors de l'image.
    fn put(&mut self, x: i32, y: i32, bgr: [u8; 3]) {
        let (w, h) = (self.size.0 as i32, self.size.1 as i32);
        if x < 0 || y < 0 || x >= w || y >= h {
            return;
        }
        let i = ((y * w + x) * 4) as usize;
        if let Some(px) = self.pixels.get_mut(i..i + 4) {
            px.copy_from_slice(&[bgr[0], bgr[1], bgr[2], 255]);
        }
    }
}

// ----------------------------------------------------------------------------
// Le navigateur : programme `minelib-browser` (Chromium, CEF), lancé à part.
// ----------------------------------------------------------------------------

/// Image reçue du navigateur : largeur, hauteur, pixels BGRA.
type Frame = (u32, u32, Vec<u8>);

/// Évènement d'un onglet du navigateur.
#[derive(Debug, PartialEq)]
enum Event {
    /// Un champ de texte a le focus (vrai) ou ne l'a plus (faux).
    Editing(bool),
    /// État de la page : `{"u":adresse,"t":temps,"p":pause}`.
    State(String),
}

/// Ce que le navigateur a envoyé depuis la dernière lecture : la dernière image de chaque
/// onglet et les évènements, dans l'ordre.
#[derive(Default)]
struct Inbox {
    frames: std::collections::HashMap<u32, Frame>,
    events: Vec<(u32, Event)>,
}

/// Navigateur lancé à part : le jeu ne dépend jamais de CEF (sans le programme, les
/// écrans montrent leur mire) et un plantage de Chromium ne fait pas tomber le jeu.
/// On lui écrit des commandes (une par ligne) et il renvoie images et évènements.
struct Browser {
    child: Option<std::process::Child>,
    stdin: Option<std::process::ChildStdin>,
    inbox: std::sync::Arc<std::sync::Mutex<Inbox>>,
}

impl Browser {
    /// Programme du navigateur : dossier `cef` à côté du jeu, sinon dans ses données.
    fn find() -> Option<std::path::PathBuf> {
        let name = if cfg!(windows) {
            "minelib-browser.exe"
        } else {
            "minelib-browser"
        };
        let near_exe = std::env::current_exe()
            .ok()
            .and_then(|e| Some(e.parent()?.join("cef").join(name)));
        let in_data = Some(minelib_assets::data_dir().join("cef").join(name));
        [near_exe, in_data]
            .into_iter()
            .flatten()
            .find(|p| p.is_file())
    }

    fn start() -> Option<Self> {
        use std::process::{Command, Stdio};
        let exe = Self::find()?;
        let data = minelib_assets::data_dir();
        let logs = data.join("logs");
        let _ = std::fs::create_dir_all(&logs);
        let stderr = std::fs::File::create(logs.join("navigateur-sortie.log"))
            .map(Stdio::from)
            .unwrap_or_else(|_| Stdio::null());
        let mut cmd = Command::new(&exe);
        cmd.arg("--width")
            .arg(PAGE_W.to_string())
            .arg("--height")
            .arg(PAGE_H.to_string())
            // Session du navigateur (connexion à YouTube, cookies) : la sienne, ici.
            .arg("--cache")
            .arg(data.join("cef-cache"))
            .arg("--log")
            .arg(logs.join("navigateur.log"))
            .current_dir(exe.parent()?)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(stderr);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // Pas de fenêtre de console.
            cmd.creation_flags(0x0800_0000);
        }
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                log::warn!(
                    "écrans vidéo : navigateur non lancé ({}) : {e}",
                    exe.display()
                );
                return None;
            }
        };
        let stdin = child.stdin.take();
        let stdout = child.stdout.take()?;
        let inbox = std::sync::Arc::new(std::sync::Mutex::new(Inbox::default()));
        let slot = inbox.clone();
        let reader = std::thread::Builder::new()
            .name("navigateur".into())
            .spawn(move || read_records(stdout, slot));
        if reader.is_err() {
            let _ = child.kill();
            return None;
        }
        log::info!("écrans vidéo : navigateur lancé ({})", exe.display());
        Some(Self {
            child: Some(child),
            stdin,
            inbox,
        })
    }

    /// Envoie une commande ; `false` si le navigateur ne l'a pas reçue.
    fn send(&mut self, line: &str) -> bool {
        use std::io::Write;
        let Some(stdin) = self.stdin.as_mut() else {
            return false;
        };
        writeln!(stdin, "{line}")
            .and_then(|_| stdin.flush())
            .is_ok()
    }

    /// Ce qui est arrivé depuis l'appel précédent.
    fn take(&self) -> (std::collections::HashMap<u32, Frame>, Vec<(u32, Event)>) {
        match self.inbox.lock() {
            Ok(mut i) => (std::mem::take(&mut i.frames), std::mem::take(&mut i.events)),
            Err(_) => Default::default(),
        }
    }

    fn alive(&mut self) -> bool {
        self.child
            .as_mut()
            .is_some_and(|c| matches!(c.try_wait(), Ok(None)))
    }
}

impl Drop for Browser {
    /// Fermeture sans bloquer le jeu : on demande au navigateur de quitter, puis on
    /// l'arrête s'il traîne.
    fn drop(&mut self) {
        let _ = self.send("quit");
        let stdin = self.stdin.take();
        let Some(mut child) = self.child.take() else {
            return;
        };
        let _ = std::thread::Builder::new()
            .name("navigateur (fin)".into())
            .spawn(move || {
                drop(stdin);
                for _ in 0..40 {
                    if !matches!(child.try_wait(), Ok(None)) {
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                let _ = child.kill();
                let _ = child.wait();
            });
    }
}

/// Lit ce que le navigateur envoie : des enregistrements de 16 octets (étiquette, onglet,
/// `a`, `b` : u32 petit-boutistes) suivis de leurs données — `MLBF` image `a` × `b` BGRA,
/// `MLBK` champ de texte actif (`a`), `MLBS` état de la page (`a` octets). S'arrête à la
/// fin du flux (navigateur fermé).
fn read_records(mut out: impl std::io::Read, inbox: std::sync::Arc<std::sync::Mutex<Inbox>>) {
    let mut head = [0u8; 16];
    let u = |b: &[u8]| u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
    loop {
        if out.read_exact(&mut head).is_err() {
            return;
        }
        let (id, a, b) = (u(&head[4..8]), u(&head[8..12]), u(&head[12..16]));
        match &head[..4] {
            b"MLBF" => {
                if a == 0 || b == 0 || a > 4096 || b > 4096 {
                    log::warn!("écrans vidéo : image du navigateur de taille invalide ({a} × {b})");
                    return;
                }
                let mut px = vec![0u8; (a * b * 4) as usize];
                if out.read_exact(&mut px).is_err() {
                    return;
                }
                if let Ok(mut i) = inbox.lock() {
                    i.frames.insert(id, (a, b, px));
                }
            }
            b"MLBK" => {
                if let Ok(mut i) = inbox.lock() {
                    i.events.push((id, Event::Editing(a != 0)));
                }
            }
            b"MLBS" => {
                if a > 1 << 16 {
                    log::warn!("écrans vidéo : état de page trop long ({a} octets)");
                    return;
                }
                let mut data = vec![0u8; a as usize];
                if out.read_exact(&mut data).is_err() {
                    return;
                }
                if let Ok(mut i) = inbox.lock() {
                    i.events.push((
                        id,
                        Event::State(String::from_utf8_lossy(&data).into_owned()),
                    ));
                }
            }
            _ => {
                log::warn!("écrans vidéo : flux du navigateur invalide");
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Écran sur la face +Z du bloc (0, 1, 0) : regarde vers +Z.
    fn wall() -> Placed {
        Placed::new([0, 1, 0], [0, 0, 1], Vec3::new(0.5, 1.5, 6.0))
    }

    #[test]
    fn vise_le_centre_et_les_coins() {
        let a = wall().anchor();
        let c = Vec3::from(a.center);
        let eye = c + Vec3::Z * 5.0;
        let (t, uv) = aim(&a, eye, Vec3::NEG_Z, 32.0).unwrap();
        assert!((t - 5.0).abs() < 1e-4);
        assert!((uv[0] - 0.5).abs() < 1e-5 && (uv[1] - 0.5).abs() < 1e-5);
        // Vers le coin haut-gauche : (0, 0).
        let corner = c + Vec3::new(-1.99, 1.12, 0.0);
        let (_, tl) = aim(&a, eye, (corner - eye).normalize(), 32.0).unwrap();
        assert!(tl[0] < 0.01 && tl[1] < 0.01, "{tl:?}");
    }

    #[test]
    fn rate_par_le_dos_trop_loin_ou_a_cote() {
        let a = wall().anchor();
        let c = Vec3::from(a.center);
        assert!(aim(&a, c - Vec3::Z * 5.0, Vec3::Z, 32.0).is_none());
        assert!(aim(&a, c + Vec3::Z * 5.0, Vec3::NEG_Z, 3.0).is_none());
        assert!(aim(&a, c + Vec3::new(5.0, 0.0, 5.0), Vec3::NEG_Z, 32.0).is_none());
    }

    #[test]
    fn hitbox_collee_au_mur_et_de_la_taille_de_l_ecran() {
        let (mn, mx) = wall().hitbox();
        // Face +Z du bloc (0,1,0) : z = 1. La hitbox part de la face, épaisseur THICK.
        assert!((mn.z - 1.0).abs() < 1e-5 && (mx.z - (1.0 + THICK)).abs() < 1e-5);
        assert!((mx.x - mn.x - WIDTH).abs() < 1e-5 && (mx.y - mn.y - HEIGHT).abs() < 1e-5);
    }

    #[test]
    fn debout_sur_un_sol_face_au_joueur_sur_un_axe() {
        // Sol (face +Y du bloc (0,0,0)), joueur surtout vers -X : l'écran regarde -X.
        let s = Placed::new([0, 0, 0], [0, 1, 0], Vec3::new(-6.0, 2.0, 1.0));
        assert_eq!(s.facing, [-1, 0, 0]);
        let (mn, _) = s.hitbox();
        assert!((mn.y - 1.0).abs() < 0.05, "posé sur le sol : {mn:?}");
    }

    #[test]
    fn enregistre_et_relit() {
        let dir = std::env::temp_dir().join(format!("minelib-ecrans-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let list = vec![
            wall(),
            Placed::new([3, 0, 2], [0, 1, 0], Vec3::new(3.5, 2.0, 9.0)),
        ];
        save(&dir, &list);
        assert_eq!(load(&dir), list);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn un_appui_met_la_mire_en_pause_et_la_relance() {
        let mut d = Displays::new();
        d.tabs.insert(1, Tab::new(HOME_URL));
        let v = d.frame(1).unwrap().0;
        d.press(1, [0.5, 0.5]);
        d.update(0.1);
        let tab = &d.tabs[&1];
        assert!(tab.paused && d.frame(1).unwrap().0 > v);
        let t = tab.time;
        d.update(0.1);
        assert_eq!(d.tabs[&1].time, t, "le temps n'avance pas en pause");
        d.press(1, [0.2, 0.2]);
        d.update(0.1);
        assert!(!d.tabs[&1].paused && d.tabs[&1].time > t);
    }

    #[test]
    fn chaque_ecran_sa_page() {
        let mut d = Displays::new();
        d.tabs.insert(1, Tab::new(HOME_URL));
        d.tabs.insert(2, Tab::new(HOME_URL));
        d.press(1, [0.5, 0.5]);
        d.update(0.1);
        assert!(d.tabs[&1].paused && !d.tabs[&2].paused);
        // Le curseur n'est que sur l'écran visé.
        d.hover(Some((2, [0.3, 0.3])));
        assert!(d.tabs[&1].cursor.is_none() && d.tabs[&2].cursor.is_some());
    }

    #[test]
    fn numero_stable_et_distinct() {
        let a = wall();
        assert_eq!(a.id(), Placed::new([0, 1, 0], [0, 0, 1], Vec3::ZERO).id());
        assert_ne!(a.id(), Placed::new([0, 1, 0], [1, 0, 0], Vec3::ZERO).id());
        assert_ne!(a.id(), Placed::new([1, 1, 0], [0, 0, 1], Vec3::ZERO).id());
    }

    #[test]
    fn adresse_youtube() {
        assert_eq!(youtube_url(""), HOME_URL);
        assert_eq!(youtube_url("  https://youtu.be/x "), "https://youtu.be/x");
        assert_eq!(
            youtube_url("chat piano"),
            "https://www.youtube.com/results?search_query=chat+piano"
        );
        assert_eq!(
            youtube_url("été & co"),
            "https://www.youtube.com/results?search_query=%C3%A9t%C3%A9+%26+co"
        );
    }

    #[test]
    fn point_de_la_page() {
        assert_eq!(page_point([0.0, 0.0]), (0, 0));
        assert_eq!(page_point([0.5, 0.5]), (640, 360));
        assert_eq!(page_point([1.0, 1.0]), (1279, 719));
    }

    #[test]
    fn lit_les_enregistrements_du_navigateur() {
        let rec = |tag: &[u8; 4], id: u32, a: u32, b: u32, data: &[u8]| {
            let mut v = tag.to_vec();
            for n in [id, a, b] {
                v.extend(n.to_le_bytes());
            }
            v.extend(data);
            v
        };
        let mut data = rec(b"MLBF", 7, 2, 1, &[1, 2, 3, 4, 5, 6, 7, 8]);
        data.extend(rec(b"MLBK", 7, 1, 0, &[]));
        data.extend(rec(b"MLBS", 7, 2, 0, b"{}"));
        let inbox = std::sync::Arc::new(std::sync::Mutex::new(Inbox::default()));
        read_records(&data[..], inbox.clone());
        let i = inbox.lock().unwrap();
        assert_eq!(i.frames[&7], (2, 1, vec![1, 2, 3, 4, 5, 6, 7, 8]));
        assert_eq!(
            i.events,
            vec![(7, Event::Editing(true)), (7, Event::State("{}".into()))]
        );
    }

    #[test]
    fn icone_16_par_16() {
        assert!(PIXELS.iter().all(|r| r.chars().count() == 16));
        assert!(pixels().count() > 100);
    }
}
