//! Écrans vidéo : l'objet « Écran vidéo » qu'on pose sur un mur ou un sol (« utiliser »,
//! clic droit), qu'on touche en le visant (« utiliser » : écran tactile) et qu'on casse
//! comme un bloc (« casser », clic gauche ; l'objet revient, hors créatif). Accroupi,
//! « utiliser » l'ignore et pose le bloc tenu devant.
//!
//! Chaque écran posé a une hitbox : une boîte fine collée à son mur, qu'on vise (contour
//! comme un bloc), qui arrête le joueur et qu'on casse. Les écrans sont enregistrés avec
//! le monde (`screens.txt`).
//!
//! [`ScreenContent`] est ce qu'affichent les écrans : il reçoit le survol (`hover`),
//! l'appui (`press`) et le relâchement (`release`) en coordonnées de l'écran (0..1,
//! origine en haut à gauche) et produit des images BGRA. Aujourd'hui c'est une mire de
//! test (un appui met en pause ou relance) ; demain le navigateur CEF qui lit YouTube,
//! avec les mêmes méthodes (survol → souris, appui / relâchement → clic dans la page).

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
// Ce qu'affichent les écrans : la page du navigateur (YouTube), sinon une mire.
// ----------------------------------------------------------------------------

/// Résolution de la mire (16:9).
const W: u32 = 640;
const H: u32 = 360;
/// Taille de la page du navigateur (16:9).
const PAGE_W: u32 = 1280;
const PAGE_H: u32 = 720;
/// Page ouverte au lancement du navigateur.
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

/// Ce qu'affichent les écrans : la page du navigateur (programme `minelib-browser`,
/// Chromium) s'il est installé, sinon une mire animée. Tactile dans les deux cas.
pub struct ScreenContent {
    /// Image affichée (BGRA), de taille `size`.
    pixels: Vec<u8>,
    size: (u32, u32),
    /// Navigateur lancé ; `browser_tried` : lancement déjà tenté (pas de relance en boucle).
    browser: Option<Browser>,
    browser_tried: bool,
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
    /// À redessiner (nouvelle image de la page, curseur déplacé…).
    dirty: bool,
    /// Change à chaque nouvelle image (le rendu ne renvoie que les nouvelles).
    version: u64,
}

impl ScreenContent {
    pub fn new() -> Self {
        let mut s = Self {
            pixels: Vec::new(),
            size: (W, H),
            browser: None,
            browser_tried: false,
            page: Vec::new(),
            page_size: (0, 0),
            time: 0.0,
            paused: false,
            cursor: None,
            sent: None,
            ripple: None,
            dirty: true,
            version: 0,
        };
        s.draw();
        s
    }

    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    /// Lance le navigateur sur l'accueil de YouTube (une seule tentative) : appelé quand
    /// un écran est posé.
    pub fn start_browser(&mut self) {
        if !self.browser_tried {
            self.launch(HOME_URL);
        }
    }

    /// Arrête le navigateur (plus aucun écran) ; il sera relancé au prochain écran.
    pub fn stop_browser(&mut self) {
        if self.browser.take().is_some() {
            self.page.clear();
            self.page_size = (0, 0);
            self.dirty = true;
        }
        self.browser_tried = false;
    }

    /// Ouvre `url` dans le navigateur (le lance si besoin). `false` : navigateur absent.
    pub fn open(&mut self, url: &str) -> bool {
        match self.browser.as_mut() {
            Some(b) => b.send(&format!("url {url}")),
            None => {
                self.launch(url);
                self.browser.is_some()
            }
        }
    }

    fn launch(&mut self, url: &str) {
        self.browser_tried = true;
        self.browser = Browser::start(url);
        if self.browser.is_none() {
            log::info!("écrans vidéo : navigateur absent (dossier « cef »), mire affichée");
        }
    }

    /// Le réticule survole ce point de l'écran (`None` : il n'est pas sur un écran).
    pub fn hover(&mut self, uv: Option<[f32; 2]>) {
        if uv != self.cursor {
            self.cursor = uv;
            self.dirty = true;
        }
        let Some(p) = uv.map(page_point) else { return };
        if self.sent == Some(p) {
            return;
        }
        if let Some(b) = self.browser.as_mut() {
            b.send(&format!("move {} {}", p.0, p.1));
            self.sent = Some(p);
        }
    }

    /// Appui sur l'écran : clic dans la page ; sur la mire, pause / lecture.
    pub fn press(&mut self, uv: [f32; 2]) {
        self.ripple = Some((uv, 0.0));
        self.dirty = true;
        let p = page_point(uv);
        match self.browser.as_mut() {
            Some(b) => {
                b.send(&format!("move {} {}", p.0, p.1));
                b.send(&format!("down {} {}", p.0, p.1));
                self.sent = Some(p);
            }
            None => self.paused = !self.paused,
        }
    }

    /// Fin de l'appui (`uv` : `None` si le réticule a quitté l'écran entre-temps).
    pub fn release(&mut self, uv: Option<[f32; 2]>) {
        let Some(p) = uv.map(page_point).or(self.sent) else {
            return;
        };
        if let Some(b) = self.browser.as_mut() {
            b.send(&format!("up {} {}", p.0, p.1));
        }
    }

    /// Molette sur l'écran : fait défiler la page (`dy` : crans × 120, positif vers le haut).
    pub fn wheel(&mut self, uv: [f32; 2], dy: i32) {
        let p = page_point(uv);
        if let Some(b) = self.browser.as_mut() {
            b.send(&format!("wheel {} {} {dy}", p.0, p.1));
        }
    }

    /// Avance d'une image : reçoit la page du navigateur et redessine si besoin.
    pub fn update(&mut self, dt: f32) {
        if let Some((_, age)) = &mut self.ripple {
            *age += dt;
            self.dirty = true;
        }
        if self.ripple.is_some_and(|r| r.1 > RIPPLE_S) {
            self.ripple = None;
        }
        if let Some(b) = self.browser.as_mut() {
            if !b.alive() {
                log::warn!("écrans vidéo : le navigateur s'est arrêté (voir logs/navigateur*.log)");
                // `browser_tried` reste vrai : pas de relance en boucle.
                self.browser = None;
                self.page.clear();
                self.page_size = (0, 0);
                self.dirty = true;
            } else if let Some((w, h, px)) = b.take_frame() {
                self.page = px;
                self.page_size = (w, h);
                self.dirty = true;
            }
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
        // Curseur : petit anneau là où vise le réticule.
        if let Some(uv) = self.cursor {
            self.ring(uv, 7.0 * k, 2.0 * k, [255, 255, 255]);
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

/// Dernière image reçue du navigateur : largeur, hauteur, pixels BGRA.
type Frame = (u32, u32, Vec<u8>);

/// Navigateur lancé à part : le jeu ne dépend jamais de CEF (sans le programme, les
/// écrans montrent leur mire) et un plantage de Chromium ne fait pas tomber le jeu.
/// On lui écrit des commandes (une par ligne) et il renvoie les images de la page.
struct Browser {
    child: Option<std::process::Child>,
    stdin: Option<std::process::ChildStdin>,
    frame: std::sync::Arc<std::sync::Mutex<Option<Frame>>>,
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

    fn start(url: &str) -> Option<Self> {
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
            .arg("--url")
            .arg(url)
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
        let frame = std::sync::Arc::new(std::sync::Mutex::new(None));
        let slot = frame.clone();
        let reader = std::thread::Builder::new()
            .name("navigateur".into())
            .spawn(move || read_frames(stdout, slot));
        if reader.is_err() {
            let _ = child.kill();
            return None;
        }
        log::info!("écrans vidéo : navigateur lancé ({})", exe.display());
        Some(Self {
            child: Some(child),
            stdin,
            frame,
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

    /// Dernière image reçue depuis l'appel précédent.
    fn take_frame(&self) -> Option<Frame> {
        self.frame.lock().ok()?.take()
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

/// Lit les images du navigateur : `MLBF`, largeur, hauteur (u32 petit-boutistes), puis
/// les pixels BGRA. S'arrête à la fin du flux (navigateur fermé).
fn read_frames(mut out: impl std::io::Read, slot: std::sync::Arc<std::sync::Mutex<Option<Frame>>>) {
    let mut head = [0u8; 12];
    loop {
        if out.read_exact(&mut head).is_err() {
            return;
        }
        if &head[..4] != b"MLBF" {
            log::warn!("écrans vidéo : flux d'images du navigateur invalide");
            return;
        }
        let w = u32::from_le_bytes([head[4], head[5], head[6], head[7]]);
        let h = u32::from_le_bytes([head[8], head[9], head[10], head[11]]);
        if w == 0 || h == 0 || w > 4096 || h > 4096 {
            log::warn!("écrans vidéo : image du navigateur de taille invalide ({w} × {h})");
            return;
        }
        let mut px = vec![0u8; (w * h * 4) as usize];
        if out.read_exact(&mut px).is_err() {
            return;
        }
        if let Ok(mut s) = slot.lock() {
            *s = Some((w, h, px));
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
    fn un_appui_met_en_pause_et_relance() {
        let mut s = ScreenContent::new();
        let v = s.version();
        s.press([0.5, 0.5]);
        s.update(0.1);
        assert!(s.paused && s.version() > v);
        let t = s.time;
        s.update(0.1);
        assert_eq!(s.time, t, "le temps n'avance pas en pause");
        s.press([0.2, 0.2]);
        s.update(0.1);
        assert!(!s.paused && s.time > t);
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
    fn lit_les_images_du_navigateur() {
        let mut data = b"MLBF".to_vec();
        data.extend(2u32.to_le_bytes());
        data.extend(1u32.to_le_bytes());
        data.extend([1, 2, 3, 4, 5, 6, 7, 8]);
        let slot = std::sync::Arc::new(std::sync::Mutex::new(None));
        read_frames(&data[..], slot.clone());
        assert_eq!(
            slot.lock().unwrap().take(),
            Some((2, 1, vec![1, 2, 3, 4, 5, 6, 7, 8]))
        );
    }

    #[test]
    fn icone_16_par_16() {
        assert!(PIXELS.iter().all(|r| r.chars().count() == 16));
        assert!(pixels().count() > 100);
    }
}
