//! Écrans vidéo : l'objet « Écran vidéo » qu'on pose sur un mur ou un sol (clic droit),
//! qu'on touche en le visant (clic gauche : écran tactile) et qu'on casse comme un objet
//! du monde (accroupi + clic gauche ; l'objet revient, hors créatif).
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
// Ce qu'affichent les écrans.
// ----------------------------------------------------------------------------

/// Résolution de l'image (16:9).
const W: u32 = 640;
const H: u32 = 360;
/// Durée de l'onde qui montre un appui (secondes).
const RIPPLE_S: f32 = 0.35;
/// Durée d'un tour de la barre de progression (secondes).
const LOOP_S: f32 = 20.0;

/// Contenu de test des écrans : mire animée, tactile.
pub struct ScreenContent {
    pixels: Vec<u8>,
    /// Temps de « lecture » (n'avance pas en pause).
    time: f32,
    paused: bool,
    /// Point visé (curseur), s'il y en a un.
    cursor: Option<[f32; 2]>,
    /// Dernier appui : point et âge (secondes).
    ripple: Option<([f32; 2], f32)>,
    /// Change à chaque nouvelle image (le rendu ne renvoie que les nouvelles).
    version: u64,
}

impl ScreenContent {
    pub fn new() -> Self {
        let mut s = Self {
            pixels: vec![0; (W * H * 4) as usize],
            time: 0.0,
            paused: false,
            cursor: None,
            ripple: None,
            version: 0,
        };
        s.draw();
        s
    }

    pub fn size(&self) -> (u32, u32) {
        (W, H)
    }

    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    /// Le réticule survole ce point de l'écran (`None` : il n'est pas sur un écran).
    pub fn hover(&mut self, uv: Option<[f32; 2]>) {
        self.cursor = uv;
    }

    /// Appui sur l'écran : pause / lecture, comme un clic sur un lecteur vidéo.
    pub fn press(&mut self, uv: [f32; 2]) {
        self.paused = !self.paused;
        self.ripple = Some((uv, 0.0));
    }

    /// Fin de l'appui (`uv` : `None` si le réticule a quitté l'écran entre-temps).
    pub fn release(&mut self, _uv: Option<[f32; 2]>) {}

    /// Avance d'une image et redessine.
    pub fn update(&mut self, dt: f32) {
        if !self.paused {
            self.time += dt;
        }
        if let Some((_, age)) = &mut self.ripple {
            *age += dt;
        }
        if self.ripple.is_some_and(|r| r.1 > RIPPLE_S) {
            self.ripple = None;
        }
        self.draw();
    }

    fn draw(&mut self) {
        let (w, h) = (W as i32, H as i32);
        let scroll = (self.time * 60.0) as i32;
        let bar = ((self.time / LOOP_S).fract() * w as f32) as i32;
        let dim: u8 = if self.paused { 2 } else { 1 };
        // Fond : dégradé animé (figé et assombri en pause), barre de progression en haut.
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
        // En pause : grand triangle « lecture » au centre.
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
        // Onde de l'appui : cercle qui s'agrandit et s'efface.
        if let Some((uv, age)) = self.ripple {
            let k = age / RIPPLE_S;
            let radius = 8.0 + 40.0 * k;
            let shade = (255.0 * (1.0 - k)) as u8;
            self.ring(uv, radius, 3.0, [shade, shade, shade]);
        }
        // Curseur : petit anneau là où vise le réticule.
        if let Some(uv) = self.cursor {
            self.ring(uv, 7.0, 2.0, [255, 255, 255]);
        }
        self.version += 1;
    }

    /// Anneau de rayon `radius` (pixels) et d'épaisseur `width` centré en `uv`.
    fn ring(&mut self, uv: [f32; 2], radius: f32, width: f32, bgr: [u8; 3]) {
        let (cx, cy) = (uv[0] * W as f32, uv[1] * H as f32);
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
        if x < 0 || y < 0 || x >= W as i32 || y >= H as i32 {
            return;
        }
        let i = ((y as u32 * W + x as u32) * 4) as usize;
        self.pixels[i..i + 4].copy_from_slice(&[bgr[0], bgr[1], bgr[2], 255]);
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
    fn icone_16_par_16() {
        assert!(PIXELS.iter().all(|r| r.chars().count() == 16));
        assert!(pixels().count() > 100);
    }
}
