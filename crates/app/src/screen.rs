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

/// Taille d'un écran normal : 16:9, 4 blocs de large.
const WIDTH: f32 = 4.0;
const HEIGHT: f32 = 2.25;
/// Tailles d'écran (un objet chacune), en fois la taille normale : petit (2 × 1,125
/// blocs), normal (4 × 2,25), grand (8 × 4,5), géant (32 × 18).
pub const SIZES: [f32; 4] = [0.5, 1.0, 2.0, 8.0];
/// Indice de la taille normale dans `SIZES`.
pub const NORMAL: u8 = 1;

/// Nom de l'objet écran de taille `size` (indice dans `SIZES`).
pub fn size_name(size: u8) -> &'static str {
    match size {
        0 => "Petit écran vidéo",
        2 => "Grand écran vidéo",
        3 => "Écran vidéo géant",
        _ => "Écran vidéo",
    }
}

/// Largeur et hauteur (blocs) d'un écran de taille `size`.
pub fn dims(size: u8) -> (f32, f32) {
    let k = SIZES[usize::from(size.min(3))];
    (WIDTH * k, HEIGHT * k)
}
/// Décollé de sa face (pas de scintillement avec le bloc) et épaisseur de sa hitbox.
const GAP: f32 = 0.02;
const THICK: f32 = 0.06;
/// Au plus tant d'écrans par monde.
pub const MAX_SCREENS: usize = 64;
/// Fichier des écrans, dans le dossier du monde.
const FILE: &str = "screens.txt";

/// Un écran posé : le bloc qui le porte, la face où il est posé (normale), la direction
/// vers laquelle il regarde (horizontale, sur un axe) et sa taille (indice dans `SIZES`).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Placed {
    pub block: [i32; 3],
    pub normal: [i32; 3],
    pub facing: [i32; 3],
    pub size: u8,
}

impl Placed {
    /// Écran de taille `size` posé sur la face `normal` du bloc `block`. Sur un mur il
    /// regarde hors du mur ; sur un sol ou un plafond il se dresse, tourné vers le joueur
    /// (`eye`), sur l'axe le plus proche (comme les panneaux).
    pub fn new(block: [i32; 3], normal: [i32; 3], eye: Vec3, size: u8) -> Self {
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
            size: size.min(3),
        }
    }

    /// Où dessiner l'écran. Le bas de l'écran est au point posé : sur un mur, au bas du
    /// bloc visé (centré sur lui en largeur) ; sur un sol, sur le sol. Sous un plafond,
    /// il pend (son haut au plafond).
    pub fn anchor(&self) -> ScreenAnchor {
        let (width, height) = dims(self.size);
        let n = ivec(self.normal);
        let f = ivec(self.facing);
        let face = center_of(self.block) + n * 0.5;
        let center = if self.normal[1] == 0 {
            face + n * GAP + Vec3::Y * (height * 0.5 - 0.5)
        } else {
            face + Vec3::Y * n.y * (height * 0.5 + GAP)
        };
        let up = Vec3::Y;
        let right = (-f).cross(up);
        ScreenAnchor {
            center: center.into(),
            right: right.into(),
            up: up.into(),
            size: [width, height],
            // Les boutons, le curseur et l'onde : mis par la session.
            reveal: [0.0; 2],
            cursor: [0.0; 4],
            ripple: [0.0, 0.0, -1.0, 0.0],
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
        let (width, height) = (a.size[0], a.size[1]);
        let c = Vec3::from(a.center);
        let f = ivec(self.facing);
        let half =
            (Vec3::from(a.right) * width * 0.5).abs() + (Vec3::from(a.up) * height * 0.5).abs();
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

/// Écrans enregistrés dans le dossier du monde (aucun si le fichier manque). Une ligne
/// par écran : bloc, face, direction, taille (absente avant la v160 : normale).
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
            (n.len() == 9 || n.len() == 10).then(|| Placed {
                block: [n[0], n[1], n[2]],
                normal: [n[3], n[4], n[5]],
                facing: [n[6], n[7], n[8]],
                size: n.get(9).map_or(NORMAL, |&k| k.clamp(0, 3) as u8),
            })
        })
        .take(MAX_SCREENS)
        .collect()
}

/// Enregistre les écrans dans le dossier du monde.
pub fn save(dir: &Path, screens: &[Placed]) {
    let mut text =
        String::from("# Écrans vidéo : bloc (x y z), face (normale), direction, taille\n");
    for s in screens {
        let [b, n, f] = [s.block, s.normal, s.facing];
        text += &format!(
            "{} {} {} {} {} {} {} {} {} {}\n",
            b[0], b[1], b[2], n[0], n[1], n[2], f[0], f[1], f[2], s.size
        );
    }
    if let Err(e) = std::fs::write(dir.join(FILE), text) {
        log::warn!("écrans non enregistrés : {e}");
    }
}

/// Point de l'écran `a` visé par le rayon `eye + t·dir` (0 < t ≤ `limit`) : distance `t`
/// et coordonnées de l'écran (0..1, origine en haut à gauche). `None` si le rayon passe
/// à côté, ou arrive par le dos ou la tranche.
/// (v213) Volume des écrans en pour cent, pour le navigateur.
fn volume_pct(v: f32) -> u32 {
    (v.clamp(0.0, 1.0) * 100.0).round() as u32
}

/// (v210) L'adresse d'un état de page (`{"u":…}`).
fn page_address(json: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    v.get("u")?.as_str().map(|u| u.chars().take(2048).collect())
}

/// (v243) Une vidéo joue-t-elle dans la page (temps connu, pas en pause) ?
/// (v253) La page du lecteur montre-t-elle sa file d'attente (`o`) et (v257) la recherche à
/// côté (`r`), (v265) sur quel site (`"yt"`, `"dm"`, `"vm"`, ou rien) ? `None` : pas dit.
fn page_panel(json: &str) -> Option<(bool, &'static str)> {
    let v = serde_json::from_str::<serde_json::Value>(json).ok()?;
    let o = v.get("o")?.as_bool()?;
    let r = match v.get("r") {
        Some(serde_json::Value::Bool(true)) => "yt",
        Some(serde_json::Value::String(s)) => site_recherche(s).unwrap_or(""),
        _ => "",
    };
    Some((o, r))
}

/// (v265) Les sites où chercher une vidéo depuis la file (un bouton chacun) : leur code et
/// la page où commencer.
pub const SITES_RECHERCHE: [(&str, &str); 3] = [
    ("yt", RECHERCHE_VIDEO),
    ("dm", "https://www.dailymotion.com/"),
    // (v284) Vimeo a fermé la recherche de son site en Europe : une page à nous, qui
    // cherche par Google (voir `edge::scripts::RECHERCHE_VIMEO`).
    ("vm", crate::edge::scripts::RECHERCHE_VIMEO),
];

/// (v265) Le code d'un site où chercher (`yt`, `dm`, `vm`, ou leur nom en entier).
fn site_recherche(s: &str) -> Option<&'static str> {
    Some(match s {
        "yt" | "youtube" => "yt",
        "dm" | "dailymotion" => "dm",
        "vm" | "vimeo" => "vm",
        _ => return None,
    })
}

/// (v257) Un ordre de la page du lecteur pour la recherche à côté
/// (`{"recherche":true|false}`, (v265) ou le site : `"yt"`, `"dm"`, `"vm"`) ; `None` : un
/// autre ordre (pour regarder ensemble). `Some(None)` : la fermer.
fn ordre_recherche(json: &str) -> Option<Option<&'static str>> {
    let v = serde_json::from_str::<serde_json::Value>(json).ok()?;
    match v.get("recherche")? {
        serde_json::Value::Bool(true) => Some(Some("yt")),
        serde_json::Value::Bool(false) => Some(None),
        serde_json::Value::String(s) => Some(site_recherche(s)),
        _ => None,
    }
}

/// (v289) Un bouton du haut du volet : Google, la page précédente, la suivante.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Nav {
    Google,
    Retour,
    Suivant,
}

/// (v289) L'ordre d'un bouton du haut du volet (`{"nav":"google"|"retour"|"suivant"}`).
fn ordre_nav(json: &str) -> Option<Nav> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    match v.get("nav")?.as_str()? {
        "google" => Some(Nav::Google),
        "retour" => Some(Nav::Retour),
        "suivant" => Some(Nav::Suivant),
        _ => None,
    }
}

/// (v289) Ce que le volet d'un écran doit montrer (`__mlb.etat` de `panneau.html`) : la file
/// (`f`, numéros vérifiés), les pages où aller (`h` : 1 retour, 2 suivant), la recherche
/// ouverte (`r` : son site, ou rien) et si l'on est en ligne (`l`).
fn etat_volet(t: &Tab) -> String {
    let file: Vec<&String> = t.file.iter().filter(|v| crate::watch::valid_id(v)).collect();
    let r = t.cote.as_ref().filter(|c| c.voulue).map_or("", |c| c.site);
    serde_json::json!({"f": file, "h": t.history, "r": r, "l": t.en_ligne}).to_string()
}

fn page_playing(json: &str) -> bool {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return false;
    };
    let t = v.get("t").and_then(|t| t.as_f64()).unwrap_or(-1.0);
    let p = v.get("p").and_then(|p| p.as_bool()).unwrap_or(true);
    t >= 0.0 && !p
}

/// (v210) Part de l'écran visible à l'image (0 à 1), à travers `view_proj` : la part de ses
/// points (une grille de 9 × 9) qui tombent dans l'image, devant la caméra. Ce qui le cache
/// (un mur, un joueur) n'est pas compté.
pub fn visible_part(a: &ScreenAnchor, view_proj: glam::Mat4) -> f32 {
    const N: usize = 9;
    let c = Vec3::from(a.center);
    let r = Vec3::from(a.right) * a.size[0];
    let u = Vec3::from(a.up) * a.size[1];
    let mut dedans = 0;
    for i in 0..N {
        for j in 0..N {
            let x = i as f32 / (N - 1) as f32 - 0.5;
            let y = j as f32 / (N - 1) as f32 - 0.5;
            let p = view_proj * (c + r * x + u * y).extend(1.0);
            if p.w > 1e-4 && p.x.abs() <= p.w && p.y.abs() <= p.w {
                dedans += 1;
            }
        }
    }
    dedans as f32 / (N * N) as f32
}

/// (v220) Distance où le rayon (`eye`, `dir`) entre dans la boîte (`min`, `max`), jusqu'à
/// `limit` : la hitbox d'un écran touchée par derrière ou par le côté.
pub fn aim_box(min: Vec3, max: Vec3, eye: Vec3, dir: Vec3, limit: f32) -> Option<f32> {
    let mut near = 0.0f32;
    let mut far = limit;
    for k in 0..3 {
        if dir[k].abs() < 1e-9 {
            if eye[k] < min[k] || eye[k] > max[k] {
                return None;
            }
            continue;
        }
        let (a, b) = ((min[k] - eye[k]) / dir[k], (max[k] - eye[k]) / dir[k]);
        near = near.max(a.min(b));
        far = far.min(a.max(b));
        if near > far {
            return None;
        }
    }
    Some(near)
}

/// (v243) Où le regard coupe le plan de l'écran (devant lui, côté image), à toute distance
/// et même hors de l'écran : `x`, `y` en dehors de 0..1. `None` s'il ne le coupe pas.
pub fn ray_uv(a: &ScreenAnchor, eye: Vec3, dir: Vec3) -> Option<[f32; 2]> {
    let c = Vec3::from(a.center);
    let r = Vec3::from(a.right);
    let u = Vec3::from(a.up);
    let n = r.cross(u);
    let denom = dir.dot(n);
    if denom >= -1e-6 {
        return None;
    }
    let t = (c - eye).dot(n) / denom;
    if t <= 0.0 {
        return None;
    }
    let p = eye + dir * t - c;
    Some([p.dot(r) / a.size[0] + 0.5, 0.5 - p.dot(u) / a.size[1]])
}

/// (v257) Distance de l'œil au point de l'écran le plus proche (blocs).
pub fn screen_dist(a: &ScreenAnchor, eye: Vec3) -> f32 {
    let c = Vec3::from(a.center);
    let r = Vec3::from(a.right);
    let u = Vec3::from(a.up);
    let d = eye - c;
    let x = d.dot(r).clamp(-a.size[0] * 0.5, a.size[0] * 0.5);
    let y = d.dot(u).clamp(-a.size[1] * 0.5, a.size[1] * 0.5);
    (d - r * x - u * y).length()
}

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
/// Icône de l'objet écran de taille `screen` : la télé, plus ou moins grande dans la case.
pub fn icon(ui: &mut Ui, cx: f32, cy: f32, size: f32, screen: u8) {
    let k = [0.55, 0.75, 0.9, 1.0][usize::from(screen.min(3))];
    crate::wand::pixel_icon(ui, cx, cy, size * k, pixels());
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
/// Script qui met en pause les vidéos et les sons d'une page.
const PAUSE_JS: &str = "(()=>{document.querySelectorAll('video,audio').forEach(m=>{if(!m.paused)m.pause();});\
if(window.__mlb)window.__mlb.pauseVideo();})()";
/// (v210) Lecture automatique permise sur la page du lecteur de l'écran regardé.
/// (v246) Et sur un Short du site, en ligne : le Short de l'écran regardé se lance.
const REGARDE_JS: &str =
    "(()=>{if(window.__mlb)window.__mlb.regarde();else if(window.__mlb_short)window.__mlb_short();})()";
/// (v210) Un écran est « regardé » (sa vidéo démarre seule) s'il est visé et visible à
/// plus de moitié ; l'accord lui est redonné tant qu'il l'est (une page qui se recharge).
const REGARDE_PART: f32 = 0.5;
const REGARDE_S: f32 = 1.0;
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

/// Le navigateur reste lancé tant de secondes sans écran (des écrans qui disparaissent
/// puis reviennent, comme ceux des autres joueurs, ne le relancent pas).
const IDLE_STOP_S: f32 = 120.0;
/// Attente avant de relancer un navigateur arrêté, et lancements ratés de suite au plus.
const RETRY_S: f32 = 2.0;
const MAX_FAILURES: u32 = 4;
/// Sessions du navigateur essayées au plus (`cef-cache`, `cef-cache-2`…) : une session
/// ne sert qu'à un navigateur à la fois, et un autre jeu ouvert peut tenir la première.
const MAX_CACHES: u32 = 4;

/// Pages ouvertes en même temps au plus (les écrans les plus en vue) : chaque page est un
/// onglet de Chromium (une centaine de Mo de mémoire, du calcul). Les autres écrans
/// gardent leur dernière image, assombrie (en veille), sans son.
pub const MAX_PAGES: usize = 8;
/// Images par seconde des pages, selon ce que voit le joueur (voir `Displays::schedule`) :
/// l'écran regardé (ou, sans écran regardé, le plus proche à moins de `NEAR_60` blocs) ;
/// jusqu'à `MAX_30` autres écrans en vue à moins de `NEAR_30` blocs ; les autres en vue ;
/// ceux qu'on ne voit pas (derrière soi, de dos), qui gardent leur son.
/// (v243) Après un clic dans une page, temps pendant lequel un champ de texte peut prendre
/// le clavier (son focus arrive du navigateur jusqu'à une demi-seconde après).
const TYPING_AFTER_CLICK: f32 = 1.5;
/// (v243) Après Entrée dans un champ, il perd le focus au bout de ce temps (la touche est
/// arrivée à la page avant).
const BLUR_AFTER_ENTER: f32 = 0.25;

pub const FPS_LOOKED: i32 = 60;
pub const FPS_NEAR: i32 = 30;
pub const FPS_SEEN: i32 = 10;
pub const FPS_HIDDEN: i32 = 2;
const NEAR_60: f32 = 16.0;
/// (v243) Écrans qui jouent une vidéo et qu'on voit, gardés à 60 images/s sans être visés.
const MAX_PLAYING_60: usize = 2;
/// (v243) Une baisse d'images par seconde n'est appliquée qu'au bout de ce temps.
const LOWER_AFTER: f32 = 1.5;
const NEAR_30: f32 = 32.0;
const MAX_30: usize = 3;
/// Images par seconde d'un onglet tant que `schedule` ne l'a pas réglé.
const FPS_DEFAULT: i32 = 30;

/// Un écran tel que le voit le joueur (pour `Displays::schedule`).
#[derive(Clone, Copy, Debug)]
pub struct Seen {
    pub id: u32,
    /// Distance de l'œil au centre de l'écran (blocs).
    pub dist: f32,
    /// Devant le joueur et vu de face.
    pub visible: bool,
}

/// Navigateurs arrêtés qui se ferment encore. Un nouveau n'est lancé qu'après eux : il
/// prendrait leur session (`cef-cache`), encore tenue, et ne démarrerait pas.
static CLOSING: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Ce qu'affichent les écrans : chacun a son onglet dans le navigateur (programme
/// `minelib-browser`, Chromium) s'il est installé, sinon sa mire animée. Tactiles.
///
/// Le navigateur est lancé avec le premier écran et reste lancé 2 minutes après le
/// dernier. Un navigateur arrêté (plantage, flux abîmé) est relancé ; ses pages sont
/// rouvertes. Pendant le chargement, les écrans montrent qu'ils chargent.
pub struct Displays {
    /// Navigateur lancé.
    browser: Option<Browser>,
    /// Pas de navigateur installé (dossier « cef ») : mires, sans le chercher de nouveau.
    absent: bool,
    /// Lancements ratés de suite, et attente avant le prochain essai (secondes).
    failures: u32,
    retry_in: f32,
    /// Session du navigateur : 0 pour `cef-cache`, n pour `cef-cache-<n+1>`.
    cache: u32,
    /// Depuis combien de temps il n'y a plus d'écran (secondes).
    idle: f32,
    /// Un onglet par écran, par numéro d'écran.
    tabs: std::collections::HashMap<u32, Tab>,
    /// États rapportés par les pages depuis le dernier `take_states` (regarder ensemble).
    states: Vec<(u32, String)>,
    /// Volume du son des écrans (Paramètres → Son), donné au navigateur.
    volume: f32,
    /// (v205) Filtrage des pages : `blanche` (YouTube, Google et les sites pour enfants),
    /// `noire` (tout sauf les sites filtrés) ou, en ligne, `en_ligne` (v208 : la liste
    /// blanche, sans connexion à un compte, dans une session à part dont les cookies sont
    /// effacés à chaque lancement). Changer de mode relance le navigateur.
    filtre: &'static str,
    /// Pages refusées depuis le dernier `take_blocked` : (écran, raison).
    blocked: Vec<(u32, u8)>,
    /// (v253) Ordres des pages depuis le dernier `take_orders` : (écran, JSON, (v257) venu
    /// de la recherche à côté).
    orders: Vec<(u32, String, bool)>,
    /// (v210) Écran regardé (lecture automatique permise, un seul à la fois), et temps
    /// avant de lui redonner l'accord.
    regarde: Option<u32>,
    regarde_in: f32,
}

impl Displays {
    pub fn new() -> Self {
        Self {
            browser: None,
            absent: false,
            failures: 0,
            retry_in: 0.0,
            cache: 0,
            idle: 0.0,
            tabs: std::collections::HashMap::new(),
            states: Vec::new(),
            volume: 0.5,
            filtre: "blanche",
            blocked: Vec::new(),
            orders: Vec::new(),
            regarde: None,
            regarde_in: 0.0,
        }
    }

    /// (v210) L'écran regardé : visé et visible à plus de moitié (`part`, voir
    /// [`visible_part`]). Seule la page du lecteur de cet écran-là peut lancer sa vidéo
    /// d'elle-même ; les autres attendent un clic dans leur lecteur. Les règles du lecteur
    /// officiel de YouTube : un seul lecteur en lecture automatique à la fois, et seulement
    /// s'il est visible à plus de moitié.
    pub fn look(&mut self, aimed: Option<(u32, f32)>, dt: f32) {
        let id = aimed.filter(|a| a.1 > REGARDE_PART).map(|a| a.0);
        if id != self.regarde {
            self.regarde = id;
            self.regarde_in = 0.0;
        }
        let Some(id) = id else {
            return;
        };
        self.regarde_in -= dt;
        if self.regarde_in > 0.0 {
            return;
        }
        self.regarde_in = REGARDE_S;
        if self
            .tabs
            .get(&id)
            .is_some_and(|t| crate::watch::is_player(&t.addr) || crate::watch::is_short(&t.addr))
        {
            self.eval(id, REGARDE_JS);
        }
    }

    /// (v205) Le mode de filtrage des pages (`blanche` ou `noire`). Le navigateur est
    /// relancé s'il change, le filtre étant chargé à son lancement.
    pub fn set_filter(&mut self, mode: &'static str) {
        if self.filtre == mode {
            return;
        }
        log::info!("écrans vidéo : filtrage des pages en liste {mode}");
        self.filtre = mode;
        // Son `Drop` lui demande de quitter.
        self.browser = None;
        for tab in self.tabs.values_mut() {
            tab.opened = false;
            if let Some(c) = tab.cote.as_mut() {
                c.opened = false;
            }
            if let Some(v) = tab.volet.as_mut() {
                v.opened = false;
            }
        }
    }

    /// (v243) Le curseur et l'onde d'un appui de l'écran `id`, pour le shader (voir
    /// `ScreenAnchor::cursor` et `ripple`).
    pub fn marks(&self, id: u32) -> ([f32; 4], [f32; 4]) {
        let Some(t) = self.tabs.get(&id) else {
            return ([0.0; 4], [0.0, 0.0, -1.0, 0.0]);
        };
        let editing = t.editing || t.cote.as_ref().is_some_and(|c| c.editing);
        let cursor = t.cursor.map_or([0.0; 4], |c| {
            [c[0], c[1], if editing { 2.0 } else { 1.0 }, 0.0]
        });
        let ripple = t.ripple.map_or([0.0, 0.0, -1.0, 0.0], |(uv, age)| {
            [uv[0], uv[1], (age / RIPPLE_S).clamp(0.0, 1.0), 0.0]
        });
        (cursor, ripple)
    }

    /// (v213) La page précédente de cet écran (bouton retour, v289 : du volet), ou la
    /// suivante (`forward`). Un bouton grisé ne fait rien.
    pub fn history(&mut self, id: u32, forward: bool) {
        let bit = if forward { 2 } else { 1 };
        if !self.tabs.get(&id).is_some_and(|t| t.opened && t.history & bit != 0) {
            return;
        }
        if let Some(b) = self.browser.as_mut() {
            b.send(&format!("{} {id}", if forward { "forward" } else { "back" }));
        }
    }

    /// Les pages refusées depuis la dernière fois : (écran, raison) — voir `filtre::Refus`
    /// du navigateur.
    pub fn take_blocked(&mut self) -> Vec<(u32, u8)> {
        std::mem::take(&mut self.blocked)
    }

    /// (v253) Les ordres des pages depuis la dernière fois : (écran, JSON, venu de la
    /// recherche à côté) — vidéo cliquée sur le site de YouTube, vidéo retirée de la file
    /// d'attente (voir `watch::Party::order`).
    pub fn take_orders(&mut self) -> Vec<(u32, String, bool)> {
        std::mem::take(&mut self.orders)
    }

    /// (v257) Ouvre (`on`) ou ferme la recherche à côté du lecteur de l'écran `id` (le bouton
    /// de la file). Fermée, elle rentre sous la file, puis son onglet est fermé ; rouverte,
    /// elle revient là où elle en était.
    /// (v265) `site` : où chercher (`yt`, `dm`, `vm`) ; `None` : la fermer. Passer d'un site
    /// à l'autre garde la page où on en était sur chacun.
    pub fn recherche(&mut self, id: u32, site: Option<&'static str>) {
        let bid = self.cote_bid(id);
        let Some(tab) = self.tabs.get_mut(&id) else {
            return;
        };
        let depart = |tab: &Tab, s: &str| {
            tab.cote_urls.get(s).cloned().unwrap_or_else(|| {
                SITES_RECHERCHE
                    .iter()
                    .find(|(c, _)| *c == s)
                    .map_or(RECHERCHE_VIDEO, |(_, u)| *u)
                    .to_string()
            })
        };
        match (site, tab.cote.is_some()) {
            (None, true) => {
                if let Some(c) = tab.cote.as_mut() {
                    c.voulue = false;
                    c.fermee = 0.0;
                }
            }
            (None, false) => return,
            (Some(s), false) => {
                let url = depart(tab, s);
                tab.cote = Some(Cote::new(bid, s, &url));
            }
            (Some(s), true) => {
                let ancien = tab.cote.as_ref().map(|c| (c.site, c.url.clone()));
                if let Some((a, u)) = ancien.filter(|(a, _)| *a != s) {
                    tab.cote_urls.insert(a, u);
                    let url = depart(tab, s);
                    if let Some(c) = tab.cote.as_mut() {
                        c.site = s;
                        c.url = url.clone();
                        c.change = Some(url);
                    }
                }
                if let Some(c) = tab.cote.as_mut() {
                    c.voulue = true;
                    c.fermee = 0.0;
                }
            }
        }
        log::info!(
            "écrans vidéo : écran {id}, recherche à côté {}",
            site.map_or("fermée".to_string(), |s| format!("ouverte ({s})"))
        );
    }

    /// (v257) Un numéro d'onglet libre pour la recherche de l'écran `id`.
    fn cote_bid(&self, id: u32) -> u32 {
        self.bid_libre(id, COTE_BID)
    }

    /// (v289) Un numéro d'onglet libre pour un onglet à part de l'écran `id` (sa recherche,
    /// son volet) : celui de l'écran mêlé à `mele`, ou le suivant libre.
    fn bid_libre(&self, id: u32, mele: u32) -> u32 {
        let pris = |b: u32| {
            self.tabs.contains_key(&b)
                || self.tabs.values().any(|t| {
                    t.cote.as_ref().is_some_and(|c| c.bid == b)
                        || t.volet.as_ref().is_some_and(|v| v.bid == b)
                })
        };
        let mut b = id ^ mele;
        while pris(b) {
            b = b.wrapping_add(1);
        }
        b
    }

    /// (v289) La file d'attente de chaque écran (`file`, regarder ensemble) et si l'on est
    /// en ligne : le volet les montre.
    pub fn set_files<'a>(&mut self, en_ligne: bool, file: impl Fn(u32) -> &'a [String]) {
        for (id, tab) in self.tabs.iter_mut() {
            let f = file(*id);
            if tab.file != f {
                tab.file = f.to_vec();
            }
            tab.en_ligne = en_ligne;
        }
    }

    /// Volume du son de tous les écrans (0 à 1). (v213) Il ne touche plus aux pages : le
    /// navigateur règle le volume de ses propres sons dans Windows (`volume`), et le lecteur
    /// de YouTube reste à fond. Un script toutes les deux secondes sur chaque page, qui
    /// redonnait aussi son volume au lecteur, faisait de petites coupures.
    pub fn set_volume(&mut self, v: f32) {
        let v = v.clamp(0.0, 1.0);
        if (v - self.volume).abs() > 1e-4 {
            self.volume = v;
            if let Some(b) = self.browser.as_mut() {
                b.send(&format!("volume 0 {}", volume_pct(v)));
            }
        }
    }

    /// Les écrans montrés (leurs numéros) : un onglet pour chaque nouveau, fermé pour
    /// chaque écran retiré. Le navigateur est lancé s'il y a des écrans.
    pub fn sync(&mut self, ids: &[u32]) {
        if !ids.is_empty() {
            self.idle = 0.0;
            self.launch();
        }
        let gone: Vec<u32> = self
            .tabs
            .keys()
            .copied()
            .filter(|id| !ids.contains(id))
            .collect();
        for id in gone {
            if let Some(tab) = self.tabs.remove(&id) {
                if let (true, Some(b)) = (tab.opened, self.browser.as_mut()) {
                    b.send(&format!("close {id}"));
                }
                if let (Some(c), Some(b)) = (tab.cote.as_ref(), self.browser.as_mut()) {
                    if c.opened {
                        b.send(&format!("close {}", c.bid));
                    }
                }
                if let (Some(v), Some(b)) = (tab.volet.as_ref(), self.browser.as_mut()) {
                    if v.opened {
                        b.send(&format!("close {}", v.bid));
                    }
                }
            }
        }
        for &id in ids {
            if !self.tabs.contains_key(&id) {
                // Sa page s'ouvre au prochain `update` (si l'écran est assez en vue).
                let awake = self.tabs.values().filter(|t| t.want_awake).count() < MAX_PAGES;
                let mut tab = Tab::new(HOME_URL);
                tab.want_awake = awake;
                self.tabs.insert(id, tab);
            }
        }
    }

    /// Ce que voit le joueur : chaque écran montré (`seen`) et celui sous le réticule
    /// (`looked`). Les `MAX_PAGES` écrans les plus en vue (le regardé, puis ceux en vue,
    /// puis les autres, du plus proche au plus loin) gardent leur page ouverte ; les autres
    /// passent en veille. Images par seconde : `FPS_LOOKED` pour l'écran regardé à moins
    /// de 48 blocs (sans écran regardé, le plus proche en vue à moins de `NEAR_60`), puis
    /// `FPS_NEAR` pour `MAX_30` écrans en vue à moins de `NEAR_30`, `FPS_SEEN` pour les
    /// autres en vue, `FPS_HIDDEN` pour ceux qu'on ne voit pas.
    pub fn schedule(&mut self, seen: &[Seen], looked: Option<u32>) {
        let mut order: Vec<&Seen> = seen.iter().filter(|s| self.tabs.contains_key(&s.id)).collect();
        order.sort_by(|a, b| {
            let key = |s: &Seen| (Some(s.id) != looked, !s.visible);
            key(a).cmp(&key(b)).then(a.dist.total_cmp(&b.dist))
        });
        let mut top = looked.filter(|l| order.iter().any(|s| s.id == *l && s.dist <= 48.0));
        if top.is_none() {
            top = order
                .iter()
                .find(|s| s.visible && s.dist <= NEAR_60)
                .map(|s| s.id);
        }
        let mut near = 0;
        let mut playing = 0;
        for (rank, s) in order.iter().enumerate() {
            // (v243) Une vidéo qui joue sur un écran qu'on voit : 60 images/s même sans
            // le viser (à 30, l'image d'une vidéo saccadait).
            let joue = s.visible
                && s.dist <= 48.0
                && playing < MAX_PLAYING_60
                && self.tabs.get(&s.id).is_some_and(|t| t.playing);
            if joue && Some(s.id) != top {
                playing += 1;
            }
            let fps = if Some(s.id) == top || joue {
                FPS_LOOKED
            } else if s.visible && s.dist <= NEAR_30 && near < MAX_30 {
                near += 1;
                FPS_NEAR
            } else if s.visible {
                FPS_SEEN
            } else {
                FPS_HIDDEN
            };
            if let Some(tab) = self.tabs.get_mut(&s.id) {
                let awake = rank < MAX_PAGES;
                if tab.want_awake != awake {
                    tab.want_awake = awake;
                    tab.dirty = true;
                }
                tab.want_fps = fps;
            }
        }
    }

    /// Amène le navigateur à ce que veulent les onglets : pages ouvertes ou fermées
    /// (veille), images par seconde.
    fn apply(&mut self) {
        let Some(b) = self.browser.as_mut() else {
            return;
        };
        for (id, tab) in self.tabs.iter_mut() {
            if tab.want_awake && !tab.opened {
                b.send(&format!("open {id} {}", tab.url));
                tab.opened = true;
                tab.fps_sent = FPS_DEFAULT;
                tab.sent = None;
            } else if !tab.want_awake && tab.opened {
                b.send(&format!("close {id}"));
                tab.opened = false;
                tab.editing = false;
                tab.scroll = 0;
            }
            // (v243) Une hausse tout de suite, une baisse après `LOWER_AFTER`.
            let lower = tab.want_fps < tab.fps_sent && tab.lower_for < LOWER_AFTER;
            if tab.opened && tab.want_fps != tab.fps_sent && !lower {
                b.send(&format!("fps {id} {}", tab.want_fps));
                tab.fps_sent = tab.want_fps;
            }
            // (v257) La recherche à côté vit avec la page de l'écran (en veille avec elle), à
            // sa taille ; cachée (la file partie), elle ne se redessine presque plus.
            if let Some(c) = tab.cote.as_mut() {
                if tab.opened && !c.opened {
                    b.send(&format!("cote {} {COTE_W} {PAGE_H}", c.bid));
                    b.send(&format!("open {} {}", c.bid, c.url));
                    c.change = None;
                    c.opened = true;
                    c.fps_sent = FPS_DEFAULT;
                    c.sent = None;
                } else if !tab.opened && c.opened {
                    b.send(&format!("close {}", c.bid));
                    c.opened = false;
                    c.editing = false;
                    c.scroll = 0;
                }
                // (v265) Un autre site demandé : son onglet y va.
                if let Some(url) = c.change.take().filter(|_| c.opened) {
                    b.send(&format!("open {} {url}", c.bid));
                }
                let fps = if tab.panel_open { tab.fps_sent.max(FPS_NEAR) } else { FPS_HIDDEN };
                if c.opened && fps != c.fps_sent {
                    b.send(&format!("fps {} {fps}", c.bid));
                    c.fps_sent = fps;
                }
            }
            // (v289) Le volet aussi vit avec la page de l'écran, à sa taille.
            if let Some(v) = tab.volet.as_mut() {
                if tab.opened && !v.opened {
                    b.send(&format!("cote {} {VOLET_W} {PAGE_H}", v.bid));
                    b.send(&format!("open {} {}", v.bid, crate::edge::scripts::VOLET));
                    v.opened = true;
                    v.fps_sent = FPS_DEFAULT;
                    v.sent = None;
                    v.etat = None;
                } else if !tab.opened && v.opened {
                    b.send(&format!("close {}", v.bid));
                    v.opened = false;
                    v.scroll = 0;
                }
                let fps = if tab.panel_open { tab.fps_sent.max(FPS_NEAR) } else { FPS_HIDDEN };
                if v.opened && fps != v.fps_sent {
                    b.send(&format!("fps {} {fps}", v.bid));
                    v.fps_sent = fps;
                }
            }
        }
    }

    /// Lance le navigateur s'il ne l'est pas (après la fermeture de celui d'avant, et pas
    /// tout de suite après un lancement raté). Les pages des écrans y sont ouvertes.
    fn launch(&mut self) {
        if self.browser.is_some()
            || !self.available()
            || self.retry_in > 0.0
            || CLOSING.load(std::sync::atomic::Ordering::Acquire) > 0
        {
            return;
        }
        // (v247) Le navigateur des écrans s'installait ou se mettait à jour tout seul (voir
        // `navigateur`). (v261) Sous Windows, c'est Edge (WebView2), dans ce programme :
        // rien à télécharger ; seul WebView2 peut manquer (installé par Microsoft, en
        // attendant les écrans gardent leur mire).
        if cfg!(windows) {
            if crate::navigateur::webview2_en_cours() {
                self.retry_in = 1.0;
                return;
            }
        } else {
            let dossier = minelib_assets::data_dir().join("cef");
            if crate::navigateur::lancer(dossier, crate::update::VERSION) == crate::navigateur::Etat::EnCours {
                self.retry_in = 1.0;
                return;
            }
        }
        match Browser::start(self.cache, self.filtre, self.volume) {
            Err(Launch::Absent) => {
                log::info!("écrans vidéo : navigateur absent (dossier « cef »), mires affichées");
                self.absent = true;
            }
            Err(Launch::Failed) => self.failed(),
            Ok(b) => {
                // Ses pages s'ouvrent dans `apply`.
                for tab in self.tabs.values_mut() {
                    tab.opened = false;
                }
                self.browser = Some(b);
            }
        }
    }

    /// Un navigateur ne s'est pas lancé, ou s'est arrêté trop vite : nouvel essai dans
    /// `RETRY_S`, abandon après `MAX_FAILURES`.
    fn failed(&mut self) {
        self.failures += 1;
        self.retry_in = RETRY_S;
        if self.failures >= MAX_FAILURES {
            log::warn!(
                "écrans vidéo : le navigateur ne démarre pas ({} essais), mires affichées \
                 (voir logs/navigateur*.log)",
                self.failures
            );
        }
    }

    /// Un navigateur est lancé, ou le sera.
    fn available(&self) -> bool {
        !self.absent && self.failures < MAX_FAILURES
    }

    /// Ouvre `url` sur l'écran `id` (tout de suite, ou dès que le navigateur est lancé).
    /// `false` : pas de navigateur.
    pub fn open(&mut self, id: u32, url: &str) -> bool {
        if let Some(tab) = self.tabs.get_mut(&id) {
            tab.url = url.to_string();
            // (La nouvelle page dira elle-même si elle défile.)
            tab.scroll = 0;
            // Page en veille : l'adresse est ouverte à son réveil.
            if let (true, Some(b)) = (tab.opened, self.browser.as_mut()) {
                b.send(&format!("open {id} {url}"));
            }
        }
        self.available()
    }

    /// Le réticule survole le point `uv` de l'écran `id` (`None` : d'aucun écran).
    /// (v243) Où le regard coupe le plan de chaque écran, à toute distance (pour savoir si
    /// le réticule arrive sur un écran par son bord). Les écrans absents : nulle part.
    /// (v257) Avec, pour chaque écran, la distance de l'œil à son point le plus proche (voir
    /// [`screen_dist`]) : la file et sa recherche ouverte ne s'en vont qu'en s'éloignant.
    pub fn rays(&mut self, rays: &[(u32, Option<[f32; 2]>, f32)]) {
        for (id, tab) in self.tabs.iter_mut() {
            let r = rays.iter().find(|r| r.0 == *id);
            tab.ray = r.and_then(|r| r.1);
            let d = r.map_or(f32::MAX, |r| r.2);
            // Un peu de marge pour repartir : pas de va-et-vient au bord de la portée.
            let portee = crate::session::REACH + if tab.pres { 1.0 } else { 0.0 };
            tab.pres = d <= portee;
        }
    }

    pub fn hover(&mut self, aim: Option<(u32, [f32; 2])>) {
        for (id, tab) in self.tabs.iter_mut() {
            let uv = aim.filter(|a| a.0 == *id).map(|a| a.1);
            if uv != tab.cursor {
                tab.cursor = uv;
            }
            // (v257) Dans la recherche à côté (v289 : ou le volet) : la souris est à eux.
            let Some(uv) = uv else { continue };
            let (cible, q) = tab.cible(uv);
            let (bid, opened, sent) = match cible {
                Cible::Page => (*id, true, &mut tab.sent),
                Cible::Cote => match tab.cote.as_mut() {
                    Some(c) => (c.bid, c.opened, &mut c.sent),
                    None => continue,
                },
                Cible::Volet => match tab.volet.as_mut() {
                    Some(v) => (v.bid, v.opened, &mut v.sent),
                    None => continue,
                },
            };
            if opened && *sent != Some(q) {
                if let Some(b) = self.browser.as_mut() {
                    b.send(&format!("move {bid} {} {}", q.0, q.1));
                    *sent = Some(q);
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
        let (cible, q) = tab.cible(uv);
        tab.appui = cible;
        // (v289) Dans le volet : le clic est à lui ; il n'a pas de champ de texte, la page et
        // la recherche perdent le leur.
        if cible == Cible::Volet {
            tab.typing_ok = 0.0;
            let blur_page = std::mem::take(&mut tab.editing);
            let blur_cote = tab.cote.as_mut().and_then(|c| {
                c.typing_ok = 0.0;
                std::mem::take(&mut c.editing).then_some(c.bid)
            });
            let Some(v) = tab.volet.as_mut() else { return };
            if let Some(b) = self.browser.as_mut() {
                if blur_page {
                    b.send(&format!("blur {id}"));
                }
                if let Some(bid) = blur_cote {
                    b.send(&format!("blur {bid}"));
                }
                if v.opened {
                    b.send(&format!("move {} {} {}", v.bid, q.0, q.1));
                    b.send(&format!("down {} {} {}", v.bid, q.0, q.1));
                    v.sent = Some(q);
                }
            }
            return;
        }
        // (v257) Dans la recherche à côté : le clic est à elle, et le clavier ne peut plus
        // aller qu'à elle (la page du lecteur perd son champ de texte, et inversement).
        if cible == Cible::Cote {
            tab.typing_ok = 0.0;
            let blur_page = std::mem::take(&mut tab.editing);
            let Some(c) = tab.cote.as_mut() else { return };
            c.typing_ok = TYPING_AFTER_CLICK;
            c.blur_in = None;
            if let Some(b) = self.browser.as_mut() {
                if blur_page {
                    b.send(&format!("blur {id}"));
                }
                if c.opened {
                    b.send(&format!("move {} {} {}", c.bid, q.0, q.1));
                    b.send(&format!("down {} {} {}", c.bid, q.0, q.1));
                    c.sent = Some(q);
                }
            }
            return;
        }
        if let Some(c) = tab.cote.as_mut() {
            c.typing_ok = 0.0;
            if std::mem::take(&mut c.editing) {
                if let Some(b) = self.browser.as_mut() {
                    b.send(&format!("blur {}", c.bid));
                }
            }
        }
        // (v243) Un clic dans la page : un champ de texte peut prendre le clavier (son focus
        // arrive du navigateur un peu après).
        tab.typing_ok = TYPING_AFTER_CLICK;
        tab.blur_in = None;
        let p = q;
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
        // (v257) Un appui dans la recherche à côté (v289 : ou le volet) finit chez elle.
        let ici = uv.map(|uv| tab.cible(uv)).filter(|c| c.0 == tab.appui).map(|c| c.1);
        let autre = match tab.appui {
            Cible::Page => None,
            Cible::Cote => tab.cote.as_ref().map(|c| (c.bid, c.opened, c.sent)),
            Cible::Volet => tab.volet.as_ref().map(|v| (v.bid, v.opened, v.sent)),
        };
        if tab.appui != Cible::Page {
            let Some((bid, opened, sent)) = autre else { return };
            if let (Some(q), Some(b), true) = (ici.or(sent), self.browser.as_mut(), opened) {
                b.send(&format!("up {bid} {} {}", q.0, q.1));
            }
            return;
        }
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
        // (v257) Dans la recherche à côté (v289 : ou le volet) : elle défile.
        let Some(t) = self.tabs.get(&id) else { return };
        let (cible, p) = t.cible(uv);
        let bid = match cible {
            Cible::Page => Some(id),
            Cible::Cote => t.cote.as_ref().map(|c| c.bid),
            Cible::Volet => t.volet.as_ref().map(|v| v.bid),
        };
        let Some(bid) = bid else { return };
        if let Some(b) = self.browser.as_mut() {
            b.send(&format!("wheel {bid} {} {} {dy}", p.0, p.1));
        }
    }

    /// La page de l'écran `id` peut-elle défiler sous le curseur, vers le haut (`up`) ou
    /// vers le bas ? Sinon la molette reste au jeu (barre d'objets).
    pub fn page_scrolls(&self, id: u32, up: bool) -> bool {
        let bit = if up { 1 } else { 2 };
        self.browser.is_some()
            && self.tabs.get(&id).is_some_and(|t| {
                let cible = t.cursor.map_or(Cible::Page, |uv| t.cible(uv).0);
                let flags = match cible {
                    Cible::Page => t.scroll,
                    Cible::Cote => t.cote.as_ref().map_or(0, |c| c.scroll),
                    Cible::Volet => t.volet.as_ref().map_or(0, |v| v.scroll),
                };
                flags & bit != 0
            })
    }

    /// (v257) Où va le clavier de l'écran `id` : l'onglet de sa recherche à côté si un de
    /// ses champs de texte l'a, sinon sa page.
    fn clavier(&self, id: u32) -> u32 {
        self.tabs
            .get(&id)
            .and_then(|t| t.cote.as_ref())
            .filter(|c| c.opened && c.editing)
            .map_or(id, |c| c.bid)
    }

    /// Un champ de texte de la page de l'écran `id` a-t-il le focus (le clavier y va) ?
    pub fn editing(&self, id: u32) -> bool {
        self.browser.is_some()
            && self.tabs.get(&id).is_some_and(|t| {
                t.opened && t.editing || t.cote.as_ref().is_some_and(|c| c.opened && c.editing)
            })
    }

    /// Texte tapé dans le champ de texte de l'écran `id` (v254 : ou de sa recherche à côté).
    pub fn text(&mut self, id: u32, text: &str) {
        let to = self.clavier(id);
        if let Some(b) = self.browser.as_mut() {
            b.send(&format!("text {to} {text}"));
        }
    }

    /// Touche spéciale (`back`, `enter`, `ctrl+v`…) pour l'écran `id`.
    ///
    /// (v243) Entrée rend le clavier au jeu tout de suite (avant, il attendait que la page
    /// dise que son champ avait perdu le focus : les touches de déplacement tapées pendant
    /// le chargement partaient dans la barre de recherche, qui rouvrait sa liste par-dessus
    /// les résultats), et le champ perd le focus un instant après, une fois la recherche
    /// partie.
    pub fn key(&mut self, id: u32, name: &str) {
        let to = self.clavier(id);
        if let Some(b) = self.browser.as_mut() {
            b.send(&format!("key {to} {name}"));
        }
        if name == "enter" {
            if let Some(tab) = self.tabs.get_mut(&id) {
                match tab.cote.as_mut().filter(|c| c.bid == to && to != id) {
                    Some(c) => {
                        c.editing = false;
                        c.typing_ok = 0.0;
                        c.blur_in = Some(BLUR_AFTER_ENTER);
                    }
                    None => {
                        tab.editing = false;
                        tab.typing_ok = 0.0;
                        tab.blur_in = Some(BLUR_AFTER_ENTER);
                    }
                }
            }
        }
    }

    /// Le champ de texte de l'écran `id` (v254 : et de sa recherche à côté) perd le focus :
    /// le clavier revient au jeu.
    pub fn blur(&mut self, id: u32) {
        let mut cote = None;
        if let Some(tab) = self.tabs.get_mut(&id) {
            tab.editing = false;
            if let Some(c) = tab.cote.as_mut().filter(|c| c.opened) {
                c.editing = false;
                cote = Some(c.bid);
            }
        }
        if let Some(b) = self.browser.as_mut() {
            b.send(&format!("blur {id}"));
            if let Some(bid) = cote {
                b.send(&format!("blur {bid}"));
            }
        }
    }

    /// Lance le script `js` dans la page de l'écran `id` (regarder ensemble : temps et
    /// pause de la vidéo).
    pub fn eval(&mut self, id: u32, js: &str) {
        if !self.tabs.get(&id).is_some_and(|t| t.opened) {
            return;
        }
        if let Some(b) = self.browser.as_mut() {
            b.send(&format!("eval {id} {js}"));
        }
    }

    /// Met en pause les vidéos et les sons en cours sur tous les écrans (sans rien
    /// relancer ensuite).
    pub fn pause_all(&mut self) {
        let ids: Vec<u32> = self
            .tabs
            .iter()
            .filter(|(_, t)| t.opened)
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            self.eval(id, PAUSE_JS);
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

    /// Avance d'une image : reçoit pages et évènements du navigateur, redessine ; arrête
    /// le navigateur après `IDLE_STOP_S` sans écran, le relance s'il s'est arrêté.
    pub fn update(&mut self, dt: f32) {
        self.retry_in = (self.retry_in - dt).max(0.0);
        // (v243) Fenêtre de saisie après un clic, et champ qui perd le focus après Entrée ;
        // temps depuis lequel un onglet voudrait moins d'images par seconde.
        let mut blur = Vec::new();
        for (id, tab) in self.tabs.iter_mut() {
            tab.lower_for = if tab.want_fps < tab.fps_sent {
                tab.lower_for + dt
            } else {
                0.0
            };
            tab.typing_ok = (tab.typing_ok - dt).max(0.0);
            if let Some(t) = tab.blur_in.as_mut() {
                *t -= dt;
                if *t <= 0.0 {
                    tab.blur_in = None;
                    blur.push(*id);
                }
            }
        }
        // (v257) La recherche à côté : saisie, perte du focus après Entrée, et fermeture
        // (son onglet se ferme une fois qu'elle est rentrée sous la file).
        let mut fermees = Vec::new();
        for tab in self.tabs.values_mut() {
            let Some(c) = tab.cote.as_mut() else { continue };
            c.typing_ok = (c.typing_ok - dt).max(0.0);
            if let Some(t) = c.blur_in.as_mut() {
                *t -= dt;
                if *t <= 0.0 {
                    c.blur_in = None;
                    blur.push(c.bid);
                }
            }
            c.fermee = if c.voulue { 0.0 } else { c.fermee + dt };
            if !c.voulue && c.fermee >= COTE_FERME_S {
                if c.opened {
                    fermees.push(c.bid);
                }
                tab.cote_urls.insert(c.site, c.url.clone());
                tab.cote = None;
                tab.dirty = true;
            }
        }
        for id in blur {
            if let Some(b) = self.browser.as_mut() {
                b.send(&format!("blur {id}"));
            }
        }
        for bid in fermees {
            if let Some(b) = self.browser.as_mut() {
                b.send(&format!("close {bid}"));
            }
        }
        // (v253) La file d'attente du lecteur s'ouvre ou se ferme (la page la fait glisser) ;
        // redit au plus une fois par seconde à une page qui ne l'a pas encore fait. (v257) Et
        // la recherche à côté.
        let mut panneaux = Vec::new();
        for (id, tab) in self.tabs.iter_mut() {
            let sur_lecteur = crate::watch::is_player(&tab.addr);
            let voulu = (
                tab.panel_open,
                tab.cote.as_ref().filter(|c| c.voulue).map_or("", |c| c.site),
            );
            if !tab.opened || !sur_lecteur || tab.panel_page == Some(voulu) {
                continue;
            }
            if tab.panel_sent != Some(voulu) || tab.panel_ago >= 1.0 {
                tab.panel_sent = Some(voulu);
                tab.panel_ago = 0.0;
                panneaux.push((*id, voulu));
            }
        }
        for (id, (o, r)) in panneaux {
            self.eval(
                id,
                &format!(
                    "(()=>{{if(window.__mlb&&window.__mlb.panneau)window.__mlb.panneau({o},'{r}');}})()"
                ),
            );
        }
        // (v289) Le volet d'un écran est créé la première fois qu'on le fait venir ; tant
        // qu'il est sorti, il reçoit son état (file, historique, recherche ouverte, en ligne)
        // dès qu'il change, et chaque seconde (sa page a pu se recharger).
        let nouveaux: Vec<u32> = self
            .tabs
            .iter()
            .filter(|(_, t)| t.panel_open && t.volet.is_none())
            .map(|(id, _)| *id)
            .collect();
        for id in nouveaux {
            let bid = self.bid_libre(id, VOLET_BID);
            if let Some(t) = self.tabs.get_mut(&id) {
                t.volet = Some(Volet::new(bid));
                log::info!("écrans vidéo : écran {id}, volet ouvert (onglet {bid})");
            }
        }
        let mut etats = Vec::new();
        for tab in self.tabs.values_mut() {
            let etat = etat_volet(tab);
            let sorti = tab.o > 0.0;
            let Some(v) = tab.volet.as_mut().filter(|v| v.opened) else { continue };
            v.etat_ago += dt;
            if !sorti {
                v.etat = None;
                continue;
            }
            if v.etat.as_deref() != Some(etat.as_str()) || v.etat_ago >= 1.0 {
                v.etat_ago = 0.0;
                etats.push((v.bid, etat.clone()));
                v.etat = Some(etat);
            }
        }
        if let Some(b) = self.browser.as_mut() {
            for (bid, etat) in etats {
                b.send(&format!(
                    "eval {bid} (()=>{{if(window.__mlb&&window.__mlb.etat)window.__mlb.etat({etat});}})()"
                ));
            }
        }
        if self.tabs.is_empty() {
            self.idle += dt;
            if self.idle >= IDLE_STOP_S && self.browser.is_some() {
                log::info!("écrans vidéo : plus d'écran, navigateur arrêté");
                self.browser = None;
            }
        }
        if let Some(b) = self.browser.as_mut() {
            if let Some((code, ran)) = b.ended() {
                self.browser = None;
                for tab in self.tabs.values_mut() {
                    tab.page.clear();
                    tab.editing = false;
                    tab.scroll = 0;
                    tab.opened = false;
                    tab.dirty = true;
                    if let Some(c) = tab.cote.as_mut() {
                        c.page.clear();
                        c.editing = false;
                        c.scroll = 0;
                        c.opened = false;
                    }
                    if let Some(v) = tab.volet.as_mut() {
                        v.page.clear();
                        v.scroll = 0;
                        v.opened = false;
                    }
                }
                if code == Some(crate::edge::SANS_WEBVIEW2) {
                    // (v261) Edge WebView2 manque sur ce PC : son installateur officiel, et
                    // un nouvel essai une fois installé.
                    log::warn!("écrans vidéo : Edge WebView2 n'est pas installé");
                    if crate::navigateur::installer_webview2() {
                        self.retry_in = 2.0;
                    } else {
                        self.failed();
                    }
                } else if code == Some(2) && ran < 10.0 && self.cache + 1 < MAX_CACHES {
                    // Sa session est tenue par un autre navigateur (un autre jeu ouvert) :
                    // on en prend une autre, tout de suite.
                    self.cache += 1;
                    log::warn!(
                        "écrans vidéo : session du navigateur déjà prise (un autre jeu \
                         ouvert ?), essai avec cef-cache-{}",
                        self.cache + 1
                    );
                } else {
                    log::warn!(
                        "écrans vidéo : le navigateur s'est arrêté après {ran:.1} s \
                         (code {code:?}), relancé (voir logs/navigateur*.log)"
                    );
                    self.failed();
                }
            } else {
                let (frames, events) = b.take();
                // Une page est arrivée : ce navigateur marche, les ratés sont oubliés.
                if !frames.is_empty() {
                    self.failures = 0;
                }
                for (id, (w, h, px)) in frames {
                    if let Some(tab) = self.tabs.get_mut(&id) {
                        tab.page = px;
                        tab.page_size = (w, h);
                        tab.dirty = true;
                    } else if let Some(tab) = self
                        .tabs
                        .values_mut()
                        .find(|t| t.cote.as_ref().is_some_and(|c| c.bid == id))
                    {
                        // (v257) La recherche à côté : recollée dans l'image de l'écran.
                        if let Some(c) = tab.cote.as_mut() {
                            c.page = px;
                            c.page_size = (w, h);
                        }
                        tab.dirty |= tab.o > 0.0 && tab.s > 0.0;
                    } else if let Some(tab) = self
                        .tabs
                        .values_mut()
                        .find(|t| t.volet.as_ref().is_some_and(|v| v.bid == id))
                    {
                        // (v289) Le volet : recollé dans l'image de l'écran s'il est sorti.
                        if let Some(v) = tab.volet.as_mut() {
                            v.page = px;
                            v.page_size = (w, h);
                        }
                        tab.dirty |= tab.o > 0.0;
                    }
                }
                let mut recherches = Vec::new();
                let mut navs = Vec::new();
                for (id, ev) in events {
                    // (v289) Un évènement du volet d'un écran : ses ordres (Google, retour,
                    // suivant, recherche, vidéo retirée de la file) et son défilement.
                    if let Some((sid, tab)) = self
                        .tabs
                        .iter_mut()
                        .find(|(_, t)| t.volet.as_ref().is_some_and(|v| v.bid == id))
                    {
                        match ev {
                            Event::Scroll(f) => {
                                if let Some(v) = tab.volet.as_mut() {
                                    v.scroll = f;
                                }
                            }
                            Event::Blocked(raison) => self.blocked.push((*sid, raison)),
                            Event::Ordre(o) => match (ordre_recherche(&o), ordre_nav(&o)) {
                                (Some(site), _) => recherches.push((*sid, site)),
                                (None, Some(nav)) => navs.push((*sid, nav)),
                                (None, None) => self.orders.push((*sid, o, false)),
                            },
                            Event::Editing(_) | Event::State(_) | Event::History(_) => {}
                        }
                        continue;
                    }
                    let Some(tab) = self.tabs.get_mut(&id) else {
                        // (v257) Un évènement de la recherche à côté d'un écran.
                        let Some((sid, tab)) = self
                            .tabs
                            .iter_mut()
                            .find(|(_, t)| t.cote.as_ref().is_some_and(|c| c.bid == id))
                        else {
                            continue;
                        };
                        let Some(c) = tab.cote.as_mut() else { continue };
                        match ev {
                            Event::Editing(on) => c.editing = on && c.typing_ok > 0.0,
                            Event::State(st) => {
                                let u = page_address(&st).unwrap_or_default();
                                if crate::watch::is_player(&u) {
                                    // Une vidéo ouverte là malgré tout (le lecteur) : dans la
                                    // file, et la recherche revient où elle en était.
                                    if let Some(v) = crate::watch::video_of(&u) {
                                        self.orders.push((*sid, format!(r#"{{"clic":"{v}"}}"#), true));
                                    }
                                    b.send(&format!("open {id} {}", c.url));
                                } else if !u.is_empty() {
                                    c.url = u;
                                }
                            }
                            Event::Scroll(f) => c.scroll = f,
                            Event::History(_) => {}
                            Event::Blocked(raison) => self.blocked.push((*sid, raison)),
                            Event::Ordre(o) => self.orders.push((*sid, o, true)),
                        }
                        continue;
                    };
                    match ev {
                        // (v243) Un champ ne prend le clavier que juste après un clic.
                        Event::Editing(on) => tab.editing = on && tab.typing_ok > 0.0,
                        Event::State(s) => {
                            if let Some(u) = page_address(&s) {
                                tab.addr = u;
                            }
                            tab.playing = page_playing(&s);
                            tab.panel_page = page_panel(&s);
                            self.states.push((id, s))
                        }
                        Event::Scroll(f) => tab.scroll = f,
                        Event::History(h) => tab.history = h,
                        Event::Blocked(raison) => self.blocked.push((id, raison)),
                        // (v257) Le bouton de la file : la recherche à côté, ici même.
                        Event::Ordre(o) => match ordre_recherche(&o) {
                            Some(site) => recherches.push((id, site)),
                            None => self.orders.push((id, o, false)),
                        },
                    }
                }
                for (id, site) in recherches {
                    self.recherche(id, site);
                }
                for (id, nav) in navs {
                    match nav {
                        Nav::Google => {
                            self.open(id, RECHERCHE);
                        }
                        Nav::Retour => self.history(id, false),
                        Nav::Suivant => self.history(id, true),
                    }
                }
            }
        }
        if !self.tabs.is_empty() {
            self.launch();
        }
        self.apply();
        let loading = self.available();
        for tab in self.tabs.values_mut() {
            if tab.loading != loading {
                tab.loading = loading;
                tab.dirty = true;
            }
            tab.update(dt);
        }
    }
}

/// L'adresse où mène le bouton Google du volet (SafeSearch est imposé par le navigateur).
/// (v289) Les boutons qui étaient en haut à gauche de l'écran (loupe, vidéo, retour,
/// suivant) sont partis, demande du propriétaire : Google, retour et suivant sont en haut
/// du volet de gauche, et la recherche de YouTube y a son bouton parmi les sites.
pub const RECHERCHE: &str = "https://www.google.com/";
/// (v211) La recherche de YouTube.
pub const RECHERCHE_VIDEO: &str = "https://www.youtube.com/";

/// (v253) La file d'attente du lecteur YouTube, sur la droite de l'écran (largeur
/// `PANNEAU_U`..1, 352 px de 1280 dans la page). Comme les boutons, et pour soi seul : elle
/// ne vient que si le regard **entre sur l'écran par son bord droit** et reste sur elle ; elle
/// s'en va dès qu'on regarde ailleurs sur l'écran (et il faut revenir par le bord), ou qu'on
/// s'éloigne (hors de portée de main ; elle revient en se rapprochant sans quitter la
/// zone). La page la fait glisser depuis la droite, et la vidéo se réduit à côté (jamais
/// par-dessus).
///
/// (v289) Le volet est maintenant **à gauche**, sur **toutes les pages** (demande du
/// propriétaire) : il vient en entrant sur l'écran par son bord gauche, sur sa zone
/// (`0..PANNEAU_U`), et repart vers la gauche ; mêmes règles qu'avant, en miroir. C'est une
/// page à part (`panneau.html`, son propre onglet du navigateur, voir [`Volet`]) que le jeu
/// colle sur la gauche de la page de l'écran ; sur le lecteur, la vidéo se décale vers la
/// droite d'autant (jamais par-dessus), ailleurs le volet passe devant la page.
const PANNEAU_U: f32 = VOLET_W as f32 / PAGE_W as f32;
/// (v253) Un peu de marge avant de la refermer en allant vers la vidéo.
const PANNEAU_MARGE: f32 = 0.02;

/// (v257) La recherche à côté du lecteur : largeur de son onglet (pixels de page ; la
/// largeur du trou de `lecteur.html`), et temps qu'il reste ouvert après « Fermer la
/// recherche », le temps qu'elle rentre sous la file.
const COTE_W: u32 = 464;
const COTE_FERME_S: f32 = 0.5;
/// (v257) Numéro de l'onglet de recherche d'un écran : celui de l'écran mêlé à ceci.
const COTE_BID: u32 = 0x5EA2_C4E5;
/// (v289) Le volet : largeur de son onglet (pixels de page), numéro de son onglet (celui de
/// l'écran mêlé à ceci), et temps pour glisser (comme la vidéo du lecteur, `lecteur.html`).
const VOLET_W: u32 = 352;
const VOLET_BID: u32 = 0x7C1E_A5E7;
const GLISSE_S: f32 = 0.28;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Panneau {
    /// Entré par le bord gauche (v289 ; le droit avant), sur la zone du volet, et pas
    /// reparti ailleurs depuis.
    armed: bool,
    /// Où le regard coupait le plan de l'écran à l'image d'avant.
    prev: Option<[f32; 2]>,
}

impl Panneau {
    /// La file doit-elle être ouverte ? (v257) `etendu` : la recherche est ouverte à côté
    /// (demande du joueur) : l'écran entier est à elle, et ni regarder la vidéo ni regarder
    /// ailleurs ne referme rien ; seul s'éloigner (`pres` faux : hors de portée de main) fait
    /// tout glisser vers la gauche, et se rapprocher le rouvre tel quel. Fermer la recherche
    /// remet les règles de la file seule (glisser sur la vidéo la referme).
    fn target(
        &mut self,
        cursor: Option<[f32; 2]>,
        ray: Option<[f32; 2]>,
        etendu: bool,
        pres: bool,
    ) -> bool {
        let prev = std::mem::replace(&mut self.prev, ray);
        if etendu && self.armed {
            return pres;
        }
        let Some(p) = ray.filter(|p| on_screen(*p)) else {
            self.armed = false;
            return false;
        };
        if !prev.is_some_and(on_screen) {
            self.armed = prev.is_some_and(|q| q[0] < 0.0) && p[0] <= PANNEAU_U;
        }
        if !self.armed || cursor.is_none() {
            return false;
        }
        if p[0] > PANNEAU_U + PANNEAU_MARGE {
            self.armed = false;
            return false;
        }
        true
    }
}

/// (v257) La recherche de YouTube à côté du lecteur d'un écran (demande du joueur) : un
/// deuxième onglet du navigateur, à elle (`COTE_W` × `PAGE_H`). (v289) Le jeu la colle à
/// droite du volet, d'où elle sort en glissant (voir `Tab::coller`). Pour soi seul : rien
/// n'en part aux autres joueurs, sauf les vidéos qu'on y clique (dans la file).
struct Cote {
    /// Numéro de son onglet dans le navigateur.
    bid: u32,
    /// (v265) Le site où l'on cherche (`yt`, `dm`, `vm`), et une page à ouvrir dans son onglet
    /// (on vient d'en changer).
    site: &'static str,
    change: Option<String>,
    /// Voulue ouverte (« Fermer la recherche » la met à faux), et depuis quand elle ne
    /// l'est plus.
    voulue: bool,
    fermee: f32,
    /// Adresse où elle en est (rouverte là), telle que la page la dit.
    url: String,
    /// Dernière image de son onglet (BGRA) et sa taille ; vide tant qu'il n'y en a pas.
    page: Vec<u8>,
    page_size: (u32, u32),
    /// Onglet ouvert dans le navigateur, et images par seconde demandées.
    opened: bool,
    fps_sent: i32,
    /// Dernier point envoyé au navigateur.
    sent: Option<(i32, i32)>,
    /// Comme pour la page de l'écran : champ de texte actif, saisie permise après un clic,
    /// perte du focus après Entrée, défilement sous la souris.
    editing: bool,
    typing_ok: f32,
    blur_in: Option<f32>,
    scroll: u8,
}

impl Cote {
    fn new(bid: u32, site: &'static str, url: &str) -> Self {
        Self {
            bid,
            site,
            change: None,
            voulue: true,
            fermee: 0.0,
            url: url.to_string(),
            page: Vec::new(),
            page_size: (0, 0),
            opened: false,
            fps_sent: 0,
            sent: None,
            editing: false,
            typing_ok: 0.0,
            blur_in: None,
            scroll: 0,
        }
    }
}

/// (v289) Le volet d'un écran : la page `panneau.html` dans son propre onglet du navigateur
/// (`VOLET_W` × `PAGE_H`), ouvert la première fois qu'on le fait venir et gardé tant que
/// la page de l'écran l'est. Le jeu lui dit la file, l'historique et la recherche ouverte
/// (`__mlb.etat`), elle répond par des ordres (voir `Displays::ordre_volet`).
struct Volet {
    /// Numéro de son onglet dans le navigateur.
    bid: u32,
    /// Dernière image de son onglet (BGRA) et sa taille ; vide tant qu'il n'y en a pas.
    page: Vec<u8>,
    page_size: (u32, u32),
    /// Onglet ouvert dans le navigateur, et images par seconde demandées.
    opened: bool,
    fps_sent: i32,
    /// Dernier point envoyé au navigateur.
    sent: Option<(i32, i32)>,
    /// Sa liste peut défiler sous la souris : vers le haut (1), vers le bas (2).
    scroll: u8,
    /// Dernier état donné à la page, et depuis quand (redit chaque seconde tant qu'il est
    /// visible : la page peut avoir été rechargée).
    etat: Option<String>,
    etat_ago: f32,
}

impl Volet {
    fn new(bid: u32) -> Self {
        Self {
            bid,
            page: Vec::new(),
            page_size: (0, 0),
            opened: false,
            fps_sent: 0,
            sent: None,
            scroll: 0,
            etat: None,
            etat_ago: 0.0,
        }
    }
}

/// (v289) Ce que vise un point de l'écran : la page, la recherche à côté ou le volet.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum Cible {
    #[default]
    Page,
    Cote,
    Volet,
}

/// (v289) Un glissement de 0 à 1 en douceur (accélère puis ralentit).
fn doux(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

/// (v289) Colle l'image `src` (`sw` × `sh`, celle d'un onglet de `PAGE_H` pixels de page de
/// haut) dans `dst` (`w` × `h`, l'image de la page de l'écran, `PAGE_W` pixels de page de
/// large) : le bord gauche de `src` à `gauche`, en pixels de page (négatif : sa gauche est
/// hors de l'écran), et seulement dans les colonnes `[x0, x1)` (pixels de page). Ce qui
/// dépasse de `src` (ou tout, sans image) est sombre.
#[allow(clippy::too_many_arguments)]
fn colle(dst: &mut [u8], w: usize, h: usize, gauche: f32, x0: f32, x1: f32, src: &[u8], sw: usize, sh: usize) {
    if w == 0 || h == 0 || dst.len() < w * h * 4 {
        return;
    }
    const SOMBRE: [u8; 4] = [15, 15, 15, 255];
    let k = w as f32 / PAGE_W as f32;
    let a = ((x0.max(0.0) * k).round() as usize).min(w);
    let b = ((x1.min(PAGE_W as f32) * k).round() as usize).min(w);
    if a >= b {
        return;
    }
    let vide = sw == 0 || sh == 0 || src.len() < sw * sh * 4;
    // Pixels de `src` par pixel de page.
    let sk = if vide { 1.0 } else { sh as f32 / PAGE_H as f32 };
    let colonnes: Vec<Option<usize>> = (a..b)
        .map(|x| {
            let px = (x as f32 + 0.5) / k - gauche;
            let sx = (px * sk).floor();
            (!vide && sx >= 0.0 && (sx as usize) < sw).then_some(sx as usize)
        })
        .collect();
    // Une suite de colonnes qui se suivent (même échelle) : copiée d'un bloc.
    let suite = colonnes.iter().flatten().count() == colonnes.len()
        && colonnes.windows(2).all(|c| c[1] == c[0].map(|x| x + 1));
    for y in 0..h {
        let ligne = &mut dst[(y * w + a) * 4..(y * w + b) * 4];
        if vide {
            for p in ligne.chunks_exact_mut(4) {
                p.copy_from_slice(&SOMBRE);
            }
            continue;
        }
        let sy = (y * sh / h).min(sh - 1);
        if suite {
            let s0 = colonnes[0].unwrap_or(0);
            let n = colonnes.len();
            ligne.copy_from_slice(&src[(sy * sw + s0) * 4..(sy * sw + s0 + n) * 4]);
            continue;
        }
        for (p, c) in ligne.chunks_exact_mut(4).zip(&colonnes) {
            match c {
                Some(sx) => {
                    let i = (sy * sw + sx) * 4;
                    p.copy_from_slice(&src[i..i + 4]);
                }
                None => p.copy_from_slice(&SOMBRE),
            }
        }
    }
}

/// Le point est-il sur l'écran ?
fn on_screen(p: [f32; 2]) -> bool {
    (0.0..=1.0).contains(&p[0]) && (0.0..=1.0).contains(&p[1])
}

/// Ce qu'affiche un écran.
struct Tab {
    /// Adresse ouverte (rouverte si le navigateur est relancé).
    url: String,
    /// (v210) Adresse de la page, telle qu'elle la rapporte.
    addr: String,
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
    /// (v243) Où le regard coupe le plan de l'écran, à toute distance (voir `Panneau`).
    ray: Option<[f32; 2]>,
    /// (v257) L'œil est à portée de main de l'écran (avec une marge pour repartir).
    pres: bool,
    sent: Option<(i32, i32)>,
    /// Dernier appui : point et âge (secondes).
    ripple: Option<([f32; 2], f32)>,
    /// Un champ de texte de la page a le focus.
    editing: bool,
    /// (v243) Temps (s) pendant lequel un champ de texte peut prendre le clavier : il ne le
    /// prend qu'après un clic du joueur dans la page, jamais de lui-même (une page qui met
    /// le focus dans sa barre de recherche en se chargeant ne vole pas les touches).
    typing_ok: f32,
    /// (v243) Après Entrée : le champ de texte perd le focus dans tant de secondes (le
    /// temps que la recherche parte), le clavier est déjà revenu au jeu.
    blur_in: Option<f32>,
    /// Un navigateur est lancé ou va l'être : sans page, l'écran montre qu'il charge
    /// (sinon, sa mire).
    loading: bool,
    /// La page peut défiler sous le curseur : vers le haut (1), vers le bas (2).
    scroll: u8,
    /// (v253) La file d'attente (v289 : le volet, bord gauche, toutes les pages) : voulue
    /// ouverte ; sur le lecteur, ce que la page dit montrer (la vidéo décalée), dernier
    /// ordre envoyé et depuis quand.
    panneau: Panneau,
    panel_open: bool,
    panel_page: Option<(bool, &'static str)>,
    panel_sent: Option<(bool, &'static str)>,
    panel_ago: f32,
    /// (v257) La recherche à côté, si elle est ouverte (ou en train de rentrer), et où elle
    /// en était quand elle a été fermée (rouverte là).
    cote: Option<Cote>,
    cote_urls: std::collections::HashMap<&'static str, String>,
    /// (v289) Le volet (son onglet), créé la première fois qu'on le fait venir.
    volet: Option<Volet>,
    /// (v289) Où en sont les glissements, de 0 à 1 (sans la douceur, voir [`doux`]) : le
    /// volet (`o`) et la recherche à côté (`s`).
    o: f32,
    s: f32,
    /// (v289) La file d'attente de l'écran (regarder ensemble), et si l'on est en ligne
    /// (en solo, pas de file : une vidéo cliquée s'ouvre).
    file: Vec<String>,
    en_ligne: bool,
    /// (v257) Où est allé le dernier appui (v289 : la page, la recherche ou le volet).
    appui: Cible,
    /// (v213) Il y a une page précédente (1), une page suivante (2) : sans elles, les
    /// boutons retour et suivant sont grisés et ne font rien.
    history: u8,
    /// Page voulue ouverte (sinon en veille : dernière image assombrie) et images par
    /// seconde voulues (voir `Displays::schedule`) ; page ouverte dans le navigateur et
    /// images par seconde qui lui ont été demandées.
    want_awake: bool,
    want_fps: i32,
    /// (v243) Une vidéo joue dans la page (d'après son état) : vue, elle garde 60 images/s.
    playing: bool,
    /// (v243) Depuis combien de temps l'onglet voudrait moins d'images par seconde : une
    /// baisse n'est appliquée qu'au bout de `LOWER_AFTER` (un coup d'œil ailleurs ne fait
    /// pas chuter l'image d'une vidéo).
    lower_for: f32,
    opened: bool,
    fps_sent: i32,
    /// À redessiner (nouvelle image de la page, curseur déplacé…).
    dirty: bool,
    /// Change à chaque nouvelle image (le rendu ne renvoie que les nouvelles).
    version: u64,
}

impl Tab {
    fn new(url: &str) -> Self {
        let mut t = Self {
            url: url.to_string(),
            addr: url.to_string(),
            pixels: Vec::new(),
            size: (W, H),
            page: Vec::new(),
            page_size: (0, 0),
            time: 0.0,
            paused: false,
            cursor: None,
            ray: None,
            pres: false,
            sent: None,
            ripple: None,
            editing: false,
            typing_ok: 0.0,
            blur_in: None,
            loading: false,
            scroll: 0,
            panneau: Panneau::default(),
            panel_open: false,
            panel_page: None,
            panel_sent: None,
            panel_ago: 0.0,
            cote: None,
            cote_urls: std::collections::HashMap::new(),
            volet: None,
            o: 0.0,
            s: 0.0,
            file: Vec::new(),
            en_ligne: false,
            appui: Cible::Page,
            history: 0,
            want_awake: true,
            want_fps: FPS_DEFAULT,
            playing: false,
            lower_for: 0.0,
            opened: false,
            fps_sent: 0,
            dirty: true,
            version: 0,
        };
        t.draw();
        t
    }

    fn update(&mut self, dt: f32) {
        // (v253) La file d'attente ; (v257) avec la recherche à côté, tout l'écran la garde
        // ouverte ; (v289) sur toutes les pages, depuis le bord gauche.
        let etendu = self.cote.as_ref().is_some_and(|c| c.voulue);
        self.panel_open = self.panneau.target(self.cursor, self.ray, etendu, self.pres);
        self.panel_ago += dt;
        // (v289) Le volet et la recherche glissent ; l'image est recollée à chaque pas.
        let pas = dt / GLISSE_S;
        let vers = |x: f32, v: bool| if v { (x + pas).min(1.0) } else { (x - pas).max(0.0) };
        let (o, s) = (vers(self.o, self.panel_open), vers(self.s, etendu));
        if (o, s) != (self.o, self.s) {
            (self.o, self.s) = (o, s);
            self.dirty = true;
        }
        // (v243) Le curseur et l'onde sont dessinés par le shader : la page
        // n'est pas redessinée (ni renvoyée à la carte graphique) quand ils bougent.
        if let Some((_, age)) = &mut self.ripple {
            *age += dt;
        }
        if self.ripple.is_some_and(|r| r.1 > RIPPLE_S) {
            self.ripple = None;
        }
        // Mire ou chargement animés (pas la veille, qui ne bouge pas).
        if self.page.is_empty() && !(self.loading && !self.want_awake) {
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
        let asleep = !self.want_awake && self.loading;
        if self.page.is_empty() && asleep {
            self.draw_asleep();
        } else if self.page.is_empty() && self.loading {
            self.draw_loading();
        } else if self.page.is_empty() {
            self.draw_mire();
        } else {
            self.size = self.page_size;
            self.pixels.clear();
            self.pixels.extend_from_slice(&self.page);
            if !asleep {
                self.coller();
            }
            if asleep {
                // En veille : sa dernière image, assombrie.
                for px in self.pixels.chunks_exact_mut(4) {
                    px[0] /= 3;
                    px[1] /= 3;
                    px[2] /= 3;
                }
            }
        }
        // (v243) Le curseur et l'onde d'un appui ne sont plus dessinés ici mais par le
        // shader, au-dessus des boutons (voir `Displays::marks`).
        self.dirty = false;
        self.version += 1;
    }

    /// (v289) Le bloc du volet et de la recherche, en pixels de page : son bord droit `r`
    /// (il sort de la gauche jusque-là), sa largeur `b` (le volet, plus la recherche sortie)
    /// et ce qui est sorti de la recherche. `None` : rentré.
    fn bloc(&self) -> Option<(f32, f32, f32)> {
        let o = doux(self.o);
        if o <= 0.001 {
            return None;
        }
        let s = if self.cote.is_some() { doux(self.s) } else { 0.0 };
        let sorti = COTE_W as f32 * s;
        let b = VOLET_W as f32 + sorti;
        Some((o * b, b, sorti))
    }

    /// (v289) Colle le volet sur la gauche de l'image de la page, et la recherche à côté à
    /// sa droite (elle sort de sous lui en glissant : on en voit d'abord la droite). Sans
    /// image d'eux encore : sombre.
    fn coller(&mut self) {
        let Some((r, b, sorti)) = self.bloc() else { return };
        let (w, h) = (self.size.0 as usize, self.size.1 as usize);
        if sorti > 0.5 {
            if let Some(c) = self.cote.as_ref() {
                let (sw, sh) = (c.page_size.0 as usize, c.page_size.1 as usize);
                colle(&mut self.pixels, w, h, r - COTE_W as f32, r - sorti, r, &c.page, sw, sh);
            }
        }
        let g = r - b;
        let (src, sw, sh) = self.volet.as_ref().map_or((&[][..], 0, 0), |v| {
            (&v.page[..], v.page_size.0 as usize, v.page_size.1 as usize)
        });
        colle(&mut self.pixels, w, h, g, g, g + VOLET_W as f32, src, sw, sh);
    }

    /// (v289) Ce que vise le point `uv` de l'écran (la page, la recherche à côté, le volet),
    /// et son point dans la page de celui-là.
    fn cible(&self, uv: [f32; 2]) -> (Cible, (i32, i32)) {
        let p = page_point(uv);
        let Some((r, b, sorti)) = self.bloc() else {
            return (Cible::Page, p);
        };
        let x = uv[0].clamp(0.0, 1.0) * PAGE_W as f32;
        // Le point dans un onglet (`larg` pixels de page de large) dont la gauche est à `g`.
        let dans = |g: f32, larg: u32, taille: (u32, u32)| {
            let (sw, sh) = if taille.1 > 0 { taille } else { (larg, PAGE_H) };
            let k = sh as f32 / PAGE_H as f32;
            let sx = (((x - g) * k) as i32).clamp(0, sw as i32 - 1);
            let sy = ((p.1 as f32 * k) as i32).clamp(0, sh as i32 - 1);
            (sx, sy)
        };
        let g = r - b;
        if let Some(v) = self.volet.as_ref() {
            if x < g + VOLET_W as f32 {
                return (Cible::Volet, dans(g, VOLET_W, v.page_size));
            }
        }
        if let Some(c) = self.cote.as_ref().filter(|_| sorti > 0.5) {
            if x >= r - sorti && x < r {
                return (Cible::Cote, dans(r - COTE_W as f32, COTE_W, c.page_size));
            }
        }
        (Cible::Page, p)
    }

    /// Écran en veille sans image : fond sombre et deux barres « pause ».
    fn draw_asleep(&mut self) {
        self.size = (W, H);
        self.pixels.resize((W * H * 4) as usize, 0);
        for px in self.pixels.chunks_exact_mut(4) {
            px.copy_from_slice(&[26, 22, 20, 255]);
        }
        let (cx, cy, h) = (W as i32 / 2, H as i32 / 2, H as i32 / 8);
        for y in cy - h / 2..cy + h / 2 {
            for x in (cx - h / 3..cx - h / 8).chain(cx + h / 8..cx + h / 3) {
                self.put(x, y, [110, 110, 110]);
            }
        }
    }

    /// Page en cours de chargement : fond sombre et arc qui tourne.
    fn draw_loading(&mut self) {
        self.size = (W, H);
        let n = (W * H * 4) as usize;
        self.pixels.resize(n, 0);
        for px in self.pixels.chunks_exact_mut(4) {
            px.copy_from_slice(&[26, 22, 20, 255]);
        }
        let (cx, cy) = (W as f32 / 2.0, H as f32 / 2.0);
        let (radius, width) = (H as f32 / 9.0, H as f32 / 45.0);
        let start = self.time * 5.0;
        let reach = (radius + width) as i32 + 1;
        let tau = std::f32::consts::TAU;
        for y in cy as i32 - reach..=cy as i32 + reach {
            for x in cx as i32 - reach..=cx as i32 + reach {
                let (dx, dy) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
                if ((dx * dx + dy * dy).sqrt() - radius).abs() > width * 0.5 {
                    continue;
                }
                // Trois quarts de tour, plus clair vers la tête de l'arc.
                let a = (dy.atan2(dx) - start).rem_euclid(tau) / tau;
                if a < 0.75 {
                    let v = (90.0 + 165.0 * a / 0.75) as u8;
                    self.put(x, y, [v, v, v]);
                }
            }
        }
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
    /// État de la page : `{"u":adresse,"t":temps,"p":pause,"a":pub,"i":vidéo}`.
    State(String),
    /// La page peut défiler sous la souris : vers le haut (1), vers le bas (2).
    Scroll(u8),
    /// (v205) Une page a été refusée par le filtre, avec sa raison (voir `filtre::Refus`
    /// du navigateur).
    Blocked(u8),
    /// (v213) Il y a une page précédente (1), une page suivante (2).
    History(u8),
    /// (v253) Un ordre de la page pour le jeu (JSON : vidéo cliquée, vidéo retirée de la
    /// file d'attente).
    Ordre(String),
}

/// Ce que le navigateur a envoyé depuis la dernière lecture : la dernière image de chaque
/// onglet et les évènements, dans l'ordre.
#[derive(Default)]
struct Inbox {
    frames: std::collections::HashMap<u32, Frame>,
    events: Vec<(u32, Event)>,
    /// Le flux du navigateur est fini (fermé, ou abîmé : on n'y lit plus rien).
    closed: bool,
}

/// Navigateur lancé à part : le jeu ne dépend jamais de CEF (sans le programme, les
/// écrans montrent leur mire) et un plantage de Chromium ne fait pas tomber le jeu.
/// On lui écrit des commandes (une par ligne) et il renvoie images et évènements.
struct Browser {
    child: Option<std::process::Child>,
    stdin: Option<std::process::ChildStdin>,
    inbox: std::sync::Arc<std::sync::Mutex<Inbox>>,
    /// Lancé à cet instant.
    started: std::time::Instant,
}

/// Pourquoi le navigateur n'est pas lancé.
enum Launch {
    /// Pas installé.
    Absent,
    /// Le programme n'a pas pu être lancé.
    Failed,
}

impl Browser {
    /// Programme du navigateur : (v261) sous Windows, ce programme même (Edge, lancé avec
    /// `--navigateur-edge`) ; ailleurs, l'ancien navigateur Chromium, dossier `cef` à côté
    /// du jeu, sinon dans ses données.
    fn find() -> Option<std::path::PathBuf> {
        if cfg!(windows) {
            return std::env::current_exe().ok();
        }
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

    /// Lance le navigateur avec la session `cache` (0 : `cef-cache`, n : `cef-cache-<n+1>`).
    fn start(cache: u32, filtre: &str, volume: f32) -> Result<Self, Launch> {
        use std::io::Write;
        use std::process::{Command, Stdio};
        let exe = Self::find().ok_or(Launch::Absent)?;
        let data = minelib_assets::data_dir();
        let logs = data.join("logs");
        let _ = std::fs::create_dir_all(&logs);
        // (v208) En ligne, une session à part : celle du solo (et son compte) n'y était pas.
        // (v243) On peut s'y connecter à un compte (jeu interdit aux moins de 18 ans).
        // (v251) Une seule session, en solo comme en ligne (demande du propriétaire) : on
        // reste connecté à YouTube en passant de l'un à l'autre. Rien n'en sort : chaque
        // joueur a son navigateur, et « regarder ensemble » ne partage qu'un numéro de
        // vidéo ; le compte (et le Premium) ne sert qu'à celui à qui il est.
        let en_ligne = filtre == "en_ligne";
        // (v261) La session d'Edge, à part de celle de l'ancien Chromium.
        let base = if cfg!(windows) { "ecrans-edge" } else { "cef-cache" };
        let session = match cache {
            0 => base.to_string(),
            n => format!("{base}-{}", n + 1),
        };
        // (v208) Les listes de l'UT1 (sites pour enfants, liste noire), rafraîchies une fois
        // par jour.
        crate::filtres::rafraichir(data.join("filtres"));
        // Sa sortie d'erreur, ajoutée au journal (vidé au-delà de 256 Ko).
        let path = logs.join("navigateur-sortie.log");
        let big = std::fs::metadata(&path).is_ok_and(|m| m.len() > 256 * 1024);
        let stderr = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .append(!big)
            .truncate(big)
            .open(&path)
            .map(|mut f| {
                let _ = writeln!(f, "=== lancement du navigateur (session {session}) ===");
                Stdio::from(f)
            })
            .unwrap_or_else(|_| Stdio::null());
        let mut cmd = Command::new(&exe);
        // (v261) Le jeu lui-même, en navigateur des écrans (Edge).
        if cfg!(windows) {
            cmd.arg(crate::edge::ARG);
        }
        cmd.arg("--width")
            .arg(PAGE_W.to_string())
            .arg("--height")
            .arg(PAGE_H.to_string())
            // Session du navigateur (connexion à YouTube, cookies) : la sienne, ici.
            .arg("--cache")
            .arg(data.join(&session))
            .arg("--log")
            .arg(logs.join("navigateur.log"))
            // Qu'il dise où la page peut défiler (la molette n'y va que si elle y sert).
            .arg("--scroll")
            // (v205) Filtrage des pages et SafeSearch. (v243) Le jeu est interdit aux moins
            // de 18 ans : la liste noire partout (le web sauf les interdits), sans mode
            // restreint de YouTube, connexion à un compte permise. Les listes sont dans
            // `.minelib\\filtres`.
            .arg("--filtre")
            .arg(if en_ligne { "noire" } else { filtre })
            .arg("--listes")
            .arg(data.join("filtres"))
            .arg("--restreint")
            .arg("non")
            // (v246) En solo, le site complet de YouTube, pas le lecteur embarqué (demande
            // du propriétaire).
            .args((!en_ligne).then_some("--site-complet"))
            // (v213) Le volume des écrans dès le lancement, sans un éclat de son avant.
            .arg("--volume")
            .arg(volume_pct(volume).to_string())
            .current_dir(exe.parent().ok_or(Launch::Absent)?)
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
                return Err(Launch::Failed);
            }
        };
        let stdin = child.stdin.take();
        let Some(stdout) = child.stdout.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Launch::Failed);
        };
        let inbox = std::sync::Arc::new(std::sync::Mutex::new(Inbox::default()));
        let slot = inbox.clone();
        let reader = std::thread::Builder::new()
            .name("navigateur".into())
            .spawn(move || {
                read_records(stdout, slot.clone());
                if let Ok(mut i) = slot.lock() {
                    i.closed = true;
                }
            });
        if reader.is_err() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Launch::Failed);
        }
        log::info!(
            "écrans vidéo : navigateur lancé ({}, session {session})",
            exe.display()
        );
        Ok(Self {
            child: Some(child),
            stdin,
            inbox,
            started: std::time::Instant::now(),
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

    /// Le navigateur s'est-il arrêté (ou son flux est-il fini, abîmé) ? Son code de sortie
    /// (2 : CEF n'a pas démarré, sa session est sans doute tenue par un autre) et depuis
    /// combien de temps il tournait (secondes). Un navigateur dont le flux est abîmé est
    /// arrêté.
    fn ended(&mut self) -> Option<(Option<i32>, f32)> {
        let ran = self.started.elapsed().as_secs_f32();
        let child = self.child.as_mut()?;
        if let Ok(Some(status)) = child.try_wait() {
            return Some((status.code(), ran));
        }
        let closed = self.inbox.lock().map_or(true, |i| i.closed);
        if !closed {
            return None;
        }
        // Flux fini mais programme vivant : on attend un peu qu'il sorte, puis on l'arrête.
        for _ in 0..10 {
            std::thread::sleep(std::time::Duration::from_millis(20));
            if let Ok(Some(status)) = child.try_wait() {
                return Some((status.code(), ran));
            }
        }
        let _ = child.kill();
        let _ = child.wait();
        Some((None, ran))
    }
}

impl Drop for Browser {
    /// Fermeture sans bloquer le jeu : on demande au navigateur de quitter, puis on
    /// l'arrête s'il traîne.
    ///
    /// Tant qu'il se ferme, `CLOSING` le compte : aucun autre n'est lancé avant (il
    /// prendrait sa session, encore tenue).
    fn drop(&mut self) {
        use std::sync::atomic::Ordering;
        let _ = self.send("quit");
        let stdin = self.stdin.take();
        let Some(mut child) = self.child.take() else {
            return;
        };
        let close = move || {
            drop(stdin);
            let mut gone = false;
            for _ in 0..60 {
                if !matches!(child.try_wait(), Ok(None)) {
                    gone = true;
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            if !gone {
                let _ = child.kill();
                let _ = child.wait();
            }
            // Ses sous-processus lâchent la session juste après lui.
            std::thread::sleep(std::time::Duration::from_millis(300));
            CLOSING.fetch_sub(1, Ordering::AcqRel);
        };
        CLOSING.fetch_add(1, Ordering::AcqRel);
        let spawned = std::thread::Builder::new()
            .name("navigateur (fin)".into())
            .spawn(close);
        if spawned.is_err() {
            // (Fil impossible : on n'attend pas, il est compté fermé.)
            CLOSING.fetch_sub(1, Ordering::AcqRel);
        }
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
            b"MLBW" => {
                if let Ok(mut i) = inbox.lock() {
                    i.events.push((id, Event::Scroll((a & 3) as u8)));
                }
            }
            // (v213) Page précédente (`a` & 1), page suivante (`a` & 2).
            b"MLBH" => {
                if let Ok(mut i) = inbox.lock() {
                    i.events.push((id, Event::History((a & 3) as u8)));
                }
            }
            // (v205) Page refusée : `a` est la raison. L'adresse reste au journal du
            // navigateur (`navigateur-sortie.log`) : le jeu n'a pas à l'afficher.
            b"MLBX" => {
                if let Ok(mut i) = inbox.lock() {
                    i.events.push((id, Event::Blocked((a & 0xff) as u8)));
                }
            }
            b"MLBO" => {
                if a > 512 {
                    log::warn!("écrans vidéo : ordre de page trop long ({a} octets)");
                    return;
                }
                let mut data = vec![0u8; a as usize];
                if out.read_exact(&mut data).is_err() {
                    return;
                }
                if let Ok(mut i) = inbox.lock() {
                    i.events.push((id, Event::Ordre(String::from_utf8_lossy(&data).into_owned())));
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

    #[test]
    fn on_sait_quelle_part_de_l_ecran_est_a_l_image() {
        use glam::Mat4;
        let a = ScreenAnchor {
            center: [0.0, 0.0, -4.0],
            right: [1.0, 0.0, 0.0],
            up: [0.0, 1.0, 0.0],
            size: [2.0, 1.125],
            reveal: [0.0; 2],
            cursor: [0.0; 4],
            ripple: [0.0, 0.0, -1.0, 0.0],
        };
        let proj = Mat4::perspective_infinite_reverse_rh(70f32.to_radians(), 16.0 / 9.0, 0.05);
        let face = proj * Mat4::look_to_rh(Vec3::ZERO, -Vec3::Z, Vec3::Y);
        assert_eq!(visible_part(&a, face), 1.0);
        let dos = proj * Mat4::look_to_rh(Vec3::ZERO, Vec3::Z, Vec3::Y);
        assert_eq!(visible_part(&a, dos), 0.0);
        // Tout près, décalé : le bord droit de l'image passe au milieu de l'écran (à 1 bloc,
        // l'image fait 2 × 1,245 de large), donc à peu près sa moitié est à l'image.
        let pres = ScreenAnchor { center: [0.0, 0.0, -1.0], ..a };
        let bord = proj * Mat4::look_to_rh(Vec3::new(-1.245, 0.0, 0.0), -Vec3::Z, Vec3::Y);
        let part = visible_part(&pres, bord);
        assert!((0.4..0.65).contains(&part), "{part}");
    }

    #[test]
    fn la_page_dit_son_adresse() {
        let u = format!("{}?v=dQw4w9WgXcQ", crate::watch::LECTEUR);
        let json = format!(r#"{{"u":"{u}","t":1,"p":false,"a":false,"i":"dQw4w9WgXcQ"}}"#);
        assert_eq!(page_address(&json), Some(u));
        assert_eq!(page_address("{}"), None);
        assert_eq!(page_address("pas du json"), None);
    }

    #[test]
    fn seul_l_ecran_regarde_a_plus_de_moitie_demarre_seul() {
        let mut d = Displays::new();
        d.look(Some((7, 0.4)), 0.1);
        assert_eq!(d.regarde, None);
        d.look(Some((7, 0.8)), 0.1);
        assert_eq!(d.regarde, Some(7));
        d.look(Some((9, 0.9)), 0.1);
        assert_eq!(d.regarde, Some(9));
        d.look(None, 0.1);
        assert_eq!(d.regarde, None);
    }

    use super::*;

    /// Écran sur la face +Z du bloc (0, 1, 0) : regarde vers +Z.
    fn wall() -> Placed {
        Placed::new([0, 1, 0], [0, 0, 1], Vec3::new(0.5, 1.5, 6.0), NORMAL)
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

    /// (v220) Un écran debout (posé au sol) se touche aussi par derrière et par le côté
    /// (sa hitbox) ; sa page, seulement de face.
    #[test]
    fn la_hitbox_se_touche_aussi_par_derriere() {
        // Sol : face +Y du bloc (0, 0, 0), le joueur en (0,5 ; 2 ; 9) : l'écran le regarde
        // (vers +Z).
        let f = Placed::new([0, 0, 0], [0, 1, 0], Vec3::new(0.5, 2.0, 9.0), NORMAL);
        let (mn, mx) = f.hitbox();
        let mid = (mn + mx) * 0.5;
        let a = f.anchor();
        // De face : la page (point visé dans 0..1).
        let eye = mid + Vec3::Z * 3.0;
        assert!(aim(&a, eye, -Vec3::Z, 10.0).is_some());
        // Par derrière : pas la page, mais la hitbox, à la bonne distance.
        let eye = mid - Vec3::Z * 3.0;
        assert!(aim(&a, eye, Vec3::Z, 10.0).is_none());
        let t = aim_box(mn, mx, eye, Vec3::Z, 10.0).expect("touchée par derrière");
        assert!((t - (3.0 - (mx.z - mn.z) * 0.5)).abs() < 1e-3, "{t}");
        // Par le côté, et pas au-delà de la portée ni à côté.
        assert!(aim_box(mn, mx, mid + Vec3::X * 4.0, -Vec3::X, 10.0).is_some());
        assert!(aim_box(mn, mx, mid + Vec3::X * 4.0, -Vec3::X, 1.0).is_none());
        assert!(aim_box(mn, mx, mid + Vec3::new(4.0, 5.0, 0.0), -Vec3::X, 10.0).is_none());
    }

    #[test]
    fn quatre_tailles_le_bas_au_point_pose() {
        for (size, k) in SIZES.iter().enumerate() {
            let size = size as u8;
            // Mur : face +Z du bloc (0, 5, 0). Bas de l'écran au bas du bloc (y = 5),
            // centré sur le bloc en largeur.
            let w = Placed::new([0, 5, 0], [0, 0, 1], Vec3::new(0.5, 6.0, 9.0), size);
            let (mn, mx) = w.hitbox();
            assert!((mn.y - 5.0).abs() < 1e-4, "taille {size} : bas {mn:?}");
            assert!((mx.y - mn.y - HEIGHT * k).abs() < 1e-4);
            assert!((mx.x - mn.x - WIDTH * k).abs() < 1e-4);
            assert!(((mn.x + mx.x) * 0.5 - 0.5).abs() < 1e-4);
            // Sol : face +Y du bloc (0, 0, 0). Bas de l'écran sur le sol (y = 1).
            let f = Placed::new([0, 0, 0], [0, 1, 0], Vec3::new(0.5, 2.0, 9.0), size);
            let (mn, mx) = f.hitbox();
            assert!((mn.y - 1.0).abs() < 0.05, "taille {size} : sol {mn:?}");
            assert!((mx.y - mn.y - HEIGHT * k).abs() < 1e-4);
        }
        assert_eq!(dims(0), (2.0, 1.125));
        assert_eq!(dims(3), (32.0, 18.0));
    }

    #[test]
    fn debout_sur_un_sol_face_au_joueur_sur_un_axe() {
        // Sol (face +Y du bloc (0,0,0)), joueur surtout vers -X : l'écran regarde -X.
        let s = Placed::new([0, 0, 0], [0, 1, 0], Vec3::new(-6.0, 2.0, 1.0), NORMAL);
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
            Placed::new([3, 0, 2], [0, 1, 0], Vec3::new(3.5, 2.0, 9.0), 3),
            Placed::new([5, 0, 2], [1, 0, 0], Vec3::new(9.0, 2.0, 2.0), 0),
        ];
        save(&dir, &list);
        assert_eq!(load(&dir), list);
        // Fichier d'avant les tailles (9 nombres) : écrans normaux.
        std::fs::write(dir.join(FILE), "0 1 0 0 0 1 0 0 1\n").unwrap();
        assert_eq!(load(&dir), vec![wall()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn un_appui_met_la_mire_en_pause_et_la_relance() {
        let mut d = Displays::new();
        // (Pas de navigateur : la mire.)
        d.absent = true;
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

    /// (v253) La file d'attente vient en entrant par le bord (v289 : **gauche**) sur sa
    /// zone, s'en va en regardant la page (il faut revenir par le bord), ou hors de portée
    /// (elle revient en se rapprochant). S'approcher en la visant déjà ne l'ouvre pas.
    #[test]
    fn le_volet_vient_du_bord_gauche_et_repart() {
        let mut p = Panneau::default();
        let z = [0.1, 0.4];
        // Le regard arrive de la gauche.
        assert!(!p.target(None, Some([-0.1, 0.4]), false, true));
        assert!(p.target(Some(z), Some(z), false, true));
        assert!(p.target(Some([0.2, 0.6]), Some([0.2, 0.6]), false, true));
        // Hors de portée : fermé, mais il revient en se rapprochant.
        assert!(!p.target(None, Some(z), false, true));
        assert!(p.target(Some(z), Some(z), false, true));
        // Le regard va sur la page : fermé, et revenir sur la zone ne le rouvre pas.
        assert!(!p.target(Some([0.5, 0.4]), Some([0.5, 0.4]), false, true));
        assert!(!p.target(Some(z), Some(z), false, true));
        // Ressortir à gauche puis revenir : rouvert.
        assert!(!p.target(None, Some([-0.05, 0.4]), false, true));
        assert!(p.target(Some(z), Some(z), false, true));
        // Regard déjà sur l'écran, entrée par le haut, ou par la droite (l'ancien côté) : non.
        let mut p = Panneau::default();
        assert!(!p.target(None, Some(z), false, true));
        assert!(!p.target(Some(z), Some(z), false, true));
        let mut p = Panneau::default();
        p.target(None, Some([0.1, -0.1]), false, true);
        assert!(!p.target(Some(z), Some(z), false, true));
        let mut p = Panneau::default();
        p.target(None, Some([1.1, 0.4]), false, true);
        assert!(!p.target(Some([0.9, 0.4]), Some([0.9, 0.4]), false, true));
        // Par le bord gauche mais pour aller sur la page (pas dans la zone) : non plus.
        let mut p = Panneau::default();
        p.target(None, Some([-0.1, 0.4]), false, true);
        assert!(!p.target(Some([0.4, 0.4]), Some([0.4, 0.4]), false, true));
    }

    /// (v257) Avec la recherche ouverte à côté, regarder la vidéo ou ailleurs ne referme
    /// rien : seulement s'éloigner (tout revient en se rapprochant), ou fermer la recherche
    /// puis glisser sur la vidéo.
    #[test]
    fn avec_la_recherche_tout_l_ecran_garde_la_file() {
        let mut p = Panneau::default();
        let z = [0.1, 0.4];
        let v = [0.8, 0.5];
        p.target(None, Some([-0.1, 0.4]), false, true);
        assert!(p.target(Some(z), Some(z), false, true));
        // La recherche ouverte : la vidéo, ou ailleurs que l'écran, ne referment rien.
        assert!(p.target(Some(v), Some(v), true, true), "la vidéo, à droite");
        assert!(p.target(None, Some([1.3, 0.5]), true, true), "regard ailleurs");
        assert!(p.target(None, None, true, true), "dos tourné");
        // S'éloigner : tout part ; se rapprocher : tout revient, même sans viser l'écran.
        assert!(!p.target(None, None, true, false), "loin");
        assert!(p.target(None, None, true, true), "revenu");
        // On revient sur la file et on ferme la recherche : la file seule, que glisser sur
        // la vidéo referme.
        assert!(p.target(Some(z), Some(z), true, true));
        assert!(p.target(Some(z), Some(z), false, true));
        assert!(!p.target(Some(v), Some(v), false, true));
        assert!(!p.target(Some(z), Some(z), false, true), "il faut revenir par le bord");
    }

    /// Le regard entre par le bord gauche et reste sur le volet (`n` images de 16 ms).
    fn entre_par_la_gauche(t: &mut Tab, n: usize) {
        t.cursor = None;
        t.ray = Some([-0.05, 0.4]);
        t.pres = true;
        t.update(0.016);
        for _ in 0..n {
            t.cursor = Some([0.1, 0.4]);
            t.ray = t.cursor;
            t.update(0.016);
        }
    }

    /// (v289) Le volet vient sur **toutes les pages** (pas seulement le lecteur), glisse
    /// depuis la gauche, et la recherche sort à sa droite ; le jeu colle leurs images sur la
    /// page, et chaque clic va à qui est dessous.
    #[test]
    fn le_volet_glisse_sur_toutes_les_pages_et_prend_son_clic() {
        let (w, h) = (PAGE_W as usize, PAGE_H as usize);
        let mut t = Tab::new(HOME_URL);
        t.page = vec![200u8; w * h * 4];
        t.page_size = (w as u32, h as u32);
        t.loading = true;
        entre_par_la_gauche(&mut t, 4);
        assert!(t.panel_open, "sur une page qui n'est pas le lecteur");
        assert!(t.o > 0.0 && t.o < 1.0, "il glisse : {}", t.o);
        entre_par_la_gauche(&mut t, 30);
        assert_eq!(t.o, 1.0);
        let mut v = Volet::new(77);
        v.page = vec![9u8; VOLET_W as usize * h * 4];
        v.page_size = (VOLET_W, h as u32);
        t.volet = Some(v);
        t.draw();
        let at = |t: &Tab, x: usize, y: usize| t.pixels[(y * w + x) * 4];
        assert_eq!(at(&t, 0, 10), 9, "le volet, collé contre le bord gauche");
        assert_eq!(at(&t, 351, 700), 9);
        assert_eq!(at(&t, 352, 10), 200, "la page, à sa droite");
        assert_eq!(t.cible([0.1, 0.5]), (Cible::Volet, (128, 360)));
        assert_eq!(t.cible([0.5, 0.5]).0, Cible::Page);
        // La recherche ouverte sort de sous le volet, vers la droite.
        let mut c = Cote::new(78, "yt", RECHERCHE_VIDEO);
        c.page = vec![5u8; COTE_W as usize * h * 4];
        c.page_size = (COTE_W, h as u32);
        t.cote = Some(c);
        for _ in 0..30 {
            t.cursor = Some([0.1, 0.4]);
            t.ray = t.cursor;
            t.update(0.016);
        }
        assert_eq!(t.s, 1.0);
        assert_eq!(at(&t, 352, 10), 5, "la recherche, à droite du volet");
        assert_eq!(at(&t, 815, 10), 5);
        assert_eq!(at(&t, 816, 10), 200);
        let x = 400.0 / PAGE_W as f32;
        assert_eq!(t.cible([x, 0.5]), (Cible::Cote, (48, 360)));
        // Hors de portée : tout rentre vers la gauche, d'un bloc.
        t.pres = false;
        t.cursor = None;
        for _ in 0..6 {
            t.update(0.016);
        }
        let (r, b, _) = t.bloc().unwrap();
        assert!(r < b && r > 0.0, "à mi-chemin : {r} / {b}");
        assert_eq!(at(&t, (r - 1.0) as usize, 10), 5, "la droite de la recherche sort en dernier");
        assert_eq!(at(&t, r as usize + 2, 10), 200);
        for _ in 0..30 {
            t.update(0.016);
        }
        assert_eq!(t.o, 0.0);
        assert!(t.bloc().is_none());
        assert_eq!(at(&t, 10, 10), 200, "rentré : la page entière");
        assert_eq!(t.cible([0.1, 0.5]).0, Cible::Page);
    }

    /// (v289) Le collage suit l'échelle des images (page ou onglet plus petits) et met du
    /// sombre là où l'onglet n'a pas d'image.
    #[test]
    fn le_collage_suit_l_echelle_et_le_sombre_comble() {
        let (w, h) = (64usize, 36usize);
        let mut dst = vec![0u8; w * h * 4];
        // Un onglet de 352 px de page, en image de 176 × 360 (moitié).
        let src: Vec<u8> = (0..176 * 360).flat_map(|i| [(i % 176) as u8, 0, 0, 255]).collect();
        colle(&mut dst, w, h, 0.0, 0.0, 352.0, &src, 176, 360);
        // 352 px de page sur 1280 = 17,6 colonnes de 64.
        assert_eq!(dst[0], 5, "colonne 0 : 10 px de page, pixel 5 de l'onglet");
        assert_eq!(dst[(5 * w + 10) * 4], ((10.5 * 20.0) / 2.0) as u8);
        assert_eq!(dst[(5 * w + 20) * 4 + 3], 0, "au-delà du volet : rien de touché");
        // Sa gauche hors de l'écran : on en voit la droite.
        let mut dst = vec![0u8; w * h * 4];
        colle(&mut dst, w, h, -100.0, 0.0, 252.0, &src, 176, 360);
        assert_eq!(dst[0], ((0.5 * 20.0 + 100.0) / 2.0) as u8);
        // Sans image : sombre.
        let mut dst = vec![0u8; w * h * 4];
        colle(&mut dst, w, h, 0.0, 0.0, 352.0, &[], 0, 0);
        assert_eq!(&dst[..4], &[15, 15, 15, 255]);
    }

    /// (v257) Un bouton de la file ouvre la recherche à côté (un onglet à elle) ; un clic et
    /// le clavier y vont quand le curseur est dessus ; « Fermer la recherche » la fait
    /// rentrer puis ferme son onglet. (v289) Le volet prend ses clics, et ses boutons du
    /// haut (Google, retour, suivant) sont des ordres à lui.
    #[test]
    fn la_recherche_a_cote_s_ouvre_prend_le_clic_et_se_ferme() {
        let mut d = Displays::new();
        let url = crate::watch::watch_url("dQw4w9WgXcQ", 0.0);
        let mut t = Tab::new(&url);
        t.addr = url;
        d.tabs.insert(1, t);
        d.recherche(1, Some("yt"));
        let bid = d.tabs[&1].cote.as_ref().unwrap().bid;
        assert_ne!(bid, 1);
        let vbid = d.bid_libre(1, VOLET_BID);
        assert!(vbid != 1 && vbid != bid);
        let t = d.tabs.get_mut(&1).unwrap();
        (t.o, t.s) = (1.0, 1.0);
        t.cote.as_mut().unwrap().opened = true;
        let mut v = Volet::new(vbid);
        v.opened = true;
        t.volet = Some(v);
        // Clic sur la recherche (à droite du volet) : à elle, qui peut prendre le clavier.
        d.press(1, [0.5, 0.5]);
        let t = &d.tabs[&1];
        assert!(t.appui == Cible::Cote && t.cote.as_ref().unwrap().typing_ok > 0.0 && t.typing_ok == 0.0);
        d.tabs.get_mut(&1).unwrap().cote.as_mut().unwrap().editing = true;
        assert_eq!(d.clavier(1), bid, "le clavier va à la recherche");
        d.key(1, "enter");
        let c = d.tabs[&1].cote.as_ref().unwrap();
        assert!(!c.editing && c.blur_in.is_some());
        assert_eq!(d.clavier(1), 1);
        // Clic sur le volet : à lui ; la recherche perd son champ.
        d.tabs.get_mut(&1).unwrap().cote.as_mut().unwrap().editing = true;
        d.press(1, [0.1, 0.5]);
        let t = &d.tabs[&1];
        assert!(t.appui == Cible::Volet && !t.cote.as_ref().unwrap().editing && t.typing_ok == 0.0);
        assert_eq!(d.clavier(1), 1);
        // Clic sur la vidéo (à droite) : à la page.
        d.press(1, [0.9, 0.5]);
        assert!(d.tabs[&1].appui == Cible::Page && d.tabs[&1].typing_ok > 0.0);
        assert_eq!(d.clavier(1), 1);
        // Fermée : elle rentre, puis son onglet est fermé ; rouverte là où elle en était.
        d.tabs.get_mut(&1).unwrap().cote.as_mut().unwrap().url = "https://www.youtube.com/results?search_query=x".into();
        d.recherche(1, None);
        d.update(0.2);
        assert!(d.tabs[&1].cote.is_some());
        d.update(COTE_FERME_S);
        assert!(d.tabs[&1].cote.is_none());
        d.recherche(1, Some("yt"));
        assert_eq!(d.tabs[&1].cote.as_ref().unwrap().url, "https://www.youtube.com/results?search_query=x");
        assert_eq!(ordre_recherche(r#"{"recherche":true}"#), Some(Some("yt")));
        assert_eq!(ordre_recherche(r#"{"recherche":"dm"}"#), Some(Some("dm")));
        assert_eq!(ordre_recherche(r#"{"recherche":false}"#), Some(None));
        assert_eq!(ordre_recherche(r#"{"clic":"dQw4w9WgXcQ"}"#), None);
        assert_eq!(ordre_nav(r#"{"nav":"google"}"#), Some(Nav::Google));
        assert_eq!(ordre_nav(r#"{"nav":"retour"}"#), Some(Nav::Retour));
        assert_eq!(ordre_nav(r#"{"nav":"suivant"}"#), Some(Nav::Suivant));
        assert_eq!(ordre_nav(r#"{"nav":"ailleurs"}"#), None);
        assert_eq!(ordre_nav(r#"{"retire":0,"v":"dQw4w9WgXcQ"}"#), None);
        assert_eq!(page_panel(r#"{"o":true,"r":true}"#), Some((true, "yt")));
        assert_eq!(page_panel(r#"{"o":true,"r":"vm"}"#), Some((true, "vm")));
        assert_eq!(page_panel(r#"{"o":true}"#), Some((true, "")));
    }

    /// (v289) Le volet est créé la première fois qu'on le fait venir, et reçoit ce qu'il
    /// doit montrer : la file (numéros vérifiés), l'historique, la recherche, en ligne.
    #[test]
    fn le_volet_se_cree_et_recoit_son_etat() {
        let mut d = Displays::new();
        d.tabs.insert(4, Tab::new(HOME_URL));
        let file = vec!["dQw4w9WgXcQ".to_string(), "<script>".to_string(), "dm:x8abcd".to_string()];
        d.set_files(true, |id| if id == 4 { &file[..] } else { &[] });
        d.tabs.get_mut(&4).unwrap().panel_open = true;
        d.update(0.016);
        let t = &d.tabs[&4];
        let v = t.volet.as_ref().expect("volet créé");
        assert_ne!(v.bid, 4);
        let mut t = Tab::new(HOME_URL);
        t.file = file.clone();
        t.en_ligne = true;
        t.history = 1;
        let e: serde_json::Value = serde_json::from_str(&etat_volet(&t)).unwrap();
        assert_eq!(e["f"], serde_json::json!(["dQw4w9WgXcQ", "dm:x8abcd"]));
        assert_eq!(e["h"], 1);
        assert_eq!(e["r"], "");
        assert_eq!(e["l"], true);
        t.cote = Some(Cote::new(5, "vm", RECHERCHE_VIDEO));
        let e: serde_json::Value = serde_json::from_str(&etat_volet(&t)).unwrap();
        assert_eq!(e["r"], "vm");
    }

    /// (v243) Entrée rend le clavier au jeu tout de suite, et le champ perd le focus un
    /// instant après. Un champ qui prend le focus sans clic récent (la page qui le remet
    /// en chargeant) n'a pas le clavier.
    #[test]
    fn entree_rend_le_clavier_et_la_page_ne_le_reprend_pas_seule() {
        let mut d = Displays::new();
        d.tabs.insert(1, Tab::new(HOME_URL));
        d.press(1, [0.5, 0.1]);
        assert!(d.tabs[&1].typing_ok > 0.0, "un clic ouvre la fenêtre de saisie");
        d.tabs.get_mut(&1).unwrap().editing = true;
        d.key(1, "enter");
        let t = &d.tabs[&1];
        assert!(!t.editing && t.typing_ok == 0.0 && t.blur_in.is_some());
        d.update(BLUR_AFTER_ENTER + 0.01);
        assert!(d.tabs[&1].blur_in.is_none());
        // Longtemps après le clic, la fenêtre de saisie est fermée.
        d.press(1, [0.5, 0.1]);
        d.update(TYPING_AFTER_CLICK + 0.1);
        assert_eq!(d.tabs[&1].typing_ok, 0.0);
    }

    /// (v243) Une vidéo qui joue sur un écran en vue reste à 60 images/s même sans le
    /// viser, et une baisse n'est demandée qu'après `LOWER_AFTER`.
    #[test]
    fn une_video_qui_joue_garde_ses_60_images() {
        let mut d = Displays::new();
        for id in 1..=4 {
            d.tabs.insert(id, Tab::new(HOME_URL));
        }
        for id in [2, 3, 4] {
            d.tabs.get_mut(&id).unwrap().playing = true;
        }
        let seen: Vec<Seen> = (1..=4)
            .map(|id| Seen { id, dist: 5.0 + id as f32, visible: true })
            .collect();
        d.schedule(&seen, Some(1));
        let fps: Vec<i32> = (1..=4).map(|id| d.tabs[&id].want_fps).collect();
        // Le regardé, puis deux vidéos qui jouent ; la troisième redescend à 30.
        assert_eq!(fps, vec![FPS_LOOKED, FPS_LOOKED, FPS_LOOKED, FPS_NEAR]);
        // Une baisse voulue s'accumule avant d'être envoyée.
        let t = d.tabs.get_mut(&4).unwrap();
        t.fps_sent = FPS_LOOKED;
        d.update(0.5);
        d.update(0.5);
        assert!((d.tabs[&4].lower_for - 1.0).abs() < 1e-4);
        d.tabs.get_mut(&4).unwrap().want_fps = FPS_LOOKED;
        d.update(0.1);
        assert_eq!(d.tabs[&4].lower_for, 0.0);
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
        assert_eq!(a.id(), Placed::new([0, 1, 0], [0, 0, 1], Vec3::ZERO, NORMAL).id());
        assert_ne!(a.id(), Placed::new([0, 1, 0], [1, 0, 0], Vec3::ZERO, NORMAL).id());
        assert_ne!(a.id(), Placed::new([1, 1, 0], [0, 0, 1], Vec3::ZERO, NORMAL).id());
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
        data.extend(rec(b"MLBW", 7, 2, 0, &[]));
        data.extend(rec(b"MLBH", 7, 3, 0, &[]));
        let inbox = std::sync::Arc::new(std::sync::Mutex::new(Inbox::default()));
        read_records(&data[..], inbox.clone());
        let i = inbox.lock().unwrap();
        assert_eq!(i.frames[&7], (2, 1, vec![1, 2, 3, 4, 5, 6, 7, 8]));
        assert_eq!(
            i.events,
            vec![
                (7, Event::Editing(true)),
                (7, Event::State("{}".into())),
                (7, Event::Scroll(2)),
                (7, Event::History(3))
            ]
        );
    }

    #[test]
    fn icone_16_par_16() {
        assert!(PIXELS.iter().all(|r| r.chars().count() == 16));
        assert!(pixels().count() > 100);
    }
}
