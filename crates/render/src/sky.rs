//! (v216) Ciel réaliste qui ne coûte presque rien à dessiner.
//!
//! - **Atmosphère** (`atmosphere`) : la couleur du ciel, de l'aube à la nuit, calculée par
//!   la diffusion de la lumière dans l'air, sur le processeur et dans un fil à part ; elle
//!   arrive dans une petite image cube (`CUBE_N`² × 6) où le ciel et le brouillard lisent
//!   leur couleur en une lecture.
//! - **Nuages** : un volume de bruit traversé pas à pas (`shaders/sky_clouds.wgsl`),
//!   éclairé par le soleil ou la lune. Ce calcul est lourd : il n'est jamais fait pour
//!   l'écran, mais dans une **carte du ciel** (`MAP_N`², tout le ciel vu en œil de poisson),
//!   quelques rangées par image. Une carte est faite d'un seul instantané (caméra, heure,
//!   météo figées pendant qu'on la dessine : aucune couture), puis fondue avec la
//!   précédente. À l'écran, le ciel lit la carte en une ou deux lectures, déplacée du vent
//!   et du chemin de la caméra depuis l'instantané (`Sky::globals`, `clouds` dans
//!   world.wgsl).
//! - **Soleil, lune, étoiles** : dessinés à l'écran, par-dessus (world.wgsl `sky_shade`).
//!
//! Les nuages restent toujours au-dessus de la caméra (`MIN_BELOW`) : ils font partie du
//! ciel, on ne les traverse pas.

use crate::atmosphere::Atmosphere;
use glam::{DVec2, Vec3};
use std::sync::mpsc;

/// Côté d'une face de l'image cube de l'atmosphère.
pub const CUBE_N: u32 = 64;
/// Côté d'une carte des nuages.
pub const MAP_N: u32 = 1536;
/// Côté du volume de bruit des nuages.
const NOISE_N: usize = 64;
/// Rayon de la « planète » des nuages (blocs) : la couche de nuages est une coquille, pour
/// que les nuages de l'horizon soient à une distance finie (environ 3 500 blocs).
pub const PLANET: f32 = 20_000.0;
/// Altitude de la base des nuages (blocs, monde).
pub const CLOUD_BASE: f32 = 420.0;
/// La base reste au moins à cette hauteur au-dessus de la caméra.
const MIN_BELOW: f32 = 110.0;
/// Angle depuis le zénith au bord de la carte (rad) : un peu sous l'horizon.
pub const THETA_MAX: f32 = 1.61;
/// Période du repère des nuages (blocs) : toutes les échelles de bruit la divisent.
const WRAP: f64 = 8192.0;
/// Durée de dessin d'une carte, et de son fondu avec la précédente (s).
/// Durée d'un fondu d'une carte des nuages vers la suivante (s) — et temps donné pour
/// dessiner celle d'après : par temps calme, et quand le ciel change vite (crépuscule,
/// météo qui tourne, vol rapide).
const CYCLE_CALM_S: f32 = 5.0;
const CYCLE_BUSY_S: f32 = 2.0;
/// L'heure ou la météo ont sauté : carte refaite en une demi-seconde, fondu aussi court.
const CYCLE_JUMP_S: f32 = 0.5;
/// Cartes des nuages : deux affichées (fondu continu de l'une vers l'autre) et une en
/// cours de dessin.
pub const MAPS: usize = 3;
const MAP_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
/// Exposition : lumière physique → couleur affichée (1 − exp(−L × exposition)).
const EXPOSURE: f32 = 22.0;

/// Météo vue par le ciel (voir `weather` dans le jeu).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SkyWeather {
    /// Part du ciel couverte de nuages (0 ciel pur … 1 couvert).
    pub cover: f32,
    /// Pluie (0 … 1) : nuages plus épais et plus sombres.
    pub rain: f32,
    /// Orage (0 … 1) : nuages hauts et très sombres.
    pub storm: f32,
    /// Vent (blocs/s, x et z).
    pub wind: [f32; 2],
}

impl Default for SkyWeather {
    fn default() -> Self {
        Self {
            cover: 0.42,
            rain: 0.0,
            storm: 0.0,
            wind: [2.6, 0.9],
        }
    }
}

/// Ce que le jeu donne au ciel à chaque image.
#[derive(Clone, Copy, Debug)]
pub struct SkyInput {
    pub eye: Vec3,
    /// Direction vers le soleil et vers la lune.
    pub sun: Vec3,
    pub moon: Vec3,
    /// Horloge du ciel (s) : arrêtée quand l'heure est arrêtée (les nuages aussi).
    pub sky_time: f32,
    pub weather: SkyWeather,
    /// Éclair : direction (xz, depuis la caméra) et force de la lueur dans les nuages.
    /// (v237) Puis le sinus de sa hauteur dans le ciel, et 0 (tout ce côté du ciel
    /// s'allume) … 1 (une tache autour de lui : la foudre lancée vers le ciel).
    pub flash: [f32; 5],
    /// (v217) Le sol au-delà des chunks chargés, vu sous l'horizon : sa couleur (celle du
    /// dessus des blocs du lointain, linéaire, sans éclairage) et sa hauteur (blocs) ;
    /// `None` : pas connu à cette image (le ciel garde ce qu'il savait).
    pub ground: Option<([f32; 3], f32)>,
    /// (v217) Distance (blocs) au bout de laquelle ce sol lointain a perdu les deux tiers
    /// de sa couleur dans la brume de l'air (plus courte sous la pluie).
    pub haze_len: f32,
}

/// Uniformes du ciel (`Sky` dans world.wgsl, liaison 21).
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub struct SkyGlobals {
    /// rgb = couleur du disque du soleil, w = lumière du soleil qui passe (0 couché).
    pub sun: [f32; 4],
    /// xyz = direction vers la lune, w = son éclat.
    pub moon: [f32; 4],
    /// x = étoiles (0 jour … 1 nuit), y = angle du ciel étoilé (rad), z = voile de nuages
    /// hauts (halo de 22°), w = lueur d'un éclair dans les nuages.
    pub misc: [f32; 4],
    /// rgb = couleur du ciel couvert, w = part de ciel couvert (brouillard).
    pub fog: [f32; 4],
    /// Cartes des nuages : xy = chemin fait depuis leur instantané (vent et caméra),
    /// z = rayon de la caméra à l'instantané, w = poids (0 : pas affichée).
    pub cloud: [[f32; 4]; MAPS],
    /// x = rayon de la caméra maintenant, y = rayon du milieu de la couche, z = 1 / angle
    /// du bord de la carte, w = épaisseur de la couche (blocs).
    pub cloud_geo: [f32; 4],
    /// xy = direction (xz) de l'éclair vu de la caméra. (v237) z = sinus de sa hauteur,
    /// w = 0 : tout ce côté du ciel … 1 : une tache autour de lui.
    pub flash: [f32; 4],
    /// (v217) Sol lointain, sous l'horizon : rgb = sa couleur (linéaire, sans éclairage),
    /// w = hauteur de l'œil au-dessus de lui (blocs).
    pub ground: [f32; 4],
    /// x = 1 / distance de brume du sol lointain (voir `SkyInput::haze_len`), yzw = libres.
    pub haze: [f32; 4],
}

/// Ce que le ciel ajoute au groupe de liaisons du monde (liaisons 16 à 21, voir le bloc
/// « CIEL » de world.wgsl) : l'image cube de l'atmosphère, les trois cartes des nuages,
/// leur échantillonneur et les uniformes du ciel. Les shaders des entités et des joueurs
/// les lisent aussi (leur brouillard va vers le même ciel) : toute pipeline qui les dessine
/// a ces liaisons dans son groupe, même hors écran (`entity::draw_offscreen`).
pub(crate) fn layout_entries() -> [wgpu::BindGroupLayoutEntry; 6] {
    let texture = |binding, view_dimension| wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension,
            multisampled: false,
        },
        count: None,
    };
    [
        texture(16, wgpu::TextureViewDimension::Cube),
        texture(17, wgpu::TextureViewDimension::D2),
        texture(18, wgpu::TextureViewDimension::D2),
        texture(19, wgpu::TextureViewDimension::D2),
        wgpu::BindGroupLayoutEntry {
            binding: 20,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            count: None,
        },
        wgpu::BindGroupLayoutEntry {
            binding: 21,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        },
    ]
}

/// Couleurs tirées de l'atmosphère pour un soleil et une lune donnés.
#[derive(Clone, Copy, Debug, Default)]
struct Lighting {
    /// Lumière directe qui arrive (soleil, lune), en couleur affichée.
    sun: Vec3,
    moon: Vec3,
    /// Ciel au zénith et à l'horizon (moyenne), couleur affichée.
    zenith: Vec3,
    horizon: Vec3,
}

struct Request {
    sun: Vec3,
    moon: Vec3,
    moon_lit: f32,
}

enum Baked {
    /// Le volume de bruit, et pour chaque part de ciel couverte (0, 1/32 … 1) le seuil du
    /// bruit qui la donne.
    Noise(Vec<u8>, [f32; 33]),
    Sky {
        sun: Vec3,
        cube: Vec<u16>,
        light: Lighting,
    },
}

/// Instantané d'une carte des nuages.
#[derive(Clone, Copy, Debug, Default)]
struct Snapshot {
    valid: bool,
    /// Caméra dans le repère des nuages (qui dérive avec le vent), en blocs.
    cam: DVec2,
    /// Rayon de la caméra (centre de la planète des nuages).
    r_cam: f32,
    /// Soleil, horloge du ciel et météo à l'instantané : la carte est à refaire quand
    /// ils ont changé (vite, si l'heure ou la météo ont sauté).
    sun: Vec3,
    sky_time: f32,
    weather: Option<SkyWeather>,
}

/// Carte en cours de dessin : rangées déjà faites, reste de rangée dû (les images vont
/// plus vite que les rangées), et temps donné pour la finir (s).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Build {
    target: usize,
    row: u32,
    due: f32,
    span: f32,
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct CloudParams {
    /// xz = caméra dans le repère des nuages (modulo `WRAP`), y = évolution (s), w = libre.
    origin: [f32; 4],
    /// xyz = direction vers la lumière (soleil ou lune).
    light_dir: [f32; 4],
    /// rgb = lumière directe au niveau des nuages.
    light_col: [f32; 4],
    amb_top: [f32; 4],
    amb_bot: [f32; 4],
    /// x = couverture, y = densité, z = épaisseur (blocs), w = orage (0 … 1).
    shape: [f32; 4],
    /// x = rayon de la caméra, y = rayon de la base, z = angle du bord, w = côté de la carte.
    geo: [f32; 4],
    /// rgb = gris du ciel couvert, w = ciel couvert (0 … 1).
    haze: [f32; 4],
}

pub struct Sky {
    cube: wgpu::Texture,
    pub cube_view: wgpu::TextureView,
    pub map_views: [wgpu::TextureView; MAPS],
    noise: wgpu::Texture,
    /// Uniformes du ciel (`SkyGlobals`), lus par le shader du monde et ceux des entités.
    pub uniform: wgpu::Buffer,
    /// Le bruit en volume des nuages (qui se répète) : les flammes s'en servent aussi.
    pub noise_view: wgpu::TextureView,
    pub noise_sampler: wgpu::Sampler,
    pub sampler: wgpu::Sampler,
    pipeline: wgpu::RenderPipeline,
    params: wgpu::Buffer,
    group: wgpu::BindGroup,
    to_worker: mpsc::Sender<Request>,
    from_worker: mpsc::Receiver<Baked>,
    /// Demande en cours chez le fil de l'atmosphère, et soleil de la dernière image reçue.
    pending: bool,
    baked_sun: Option<Vec3>,
    baked_moon: Vec3,
    light: Lighting,
    /// Seuils du bruit par part de ciel couverte (`None` : bruit pas encore calculé).
    thresholds: Option<[f32; 33]>,
    /// Rangées de la carte à dessiner à cette image (carte, de, à), voir `encode`.
    draw: Option<(usize, u32, u32)>,
    snaps: [Snapshot; MAPS],
    /// Cartes affichées : fondu de `from` vers `to` (`t` : 0 … 1, en `blend_s` secondes).
    from: Option<usize>,
    to: Option<usize>,
    t: f32,
    blend_s: f32,
    /// Carte en cours de dessin, et carte finie qui attend la fin du fondu en cours.
    build: Option<Build>,
    ready: Option<(usize, f32)>,
    /// Dérive du repère des nuages (vent accumulé), et horloge du ciel d'avant.
    drift: DVec2,
    last_sky_time: Option<f32>,
    last_frame: std::time::Instant,
    /// Dernière entrée (pour `globals`).
    input: Option<SkyInput>,
    /// (v217) Sol lointain (couleur, hauteur), lissé : il change sans à-coup quand on
    /// passe d'une forêt à la mer.
    ground: Option<([f32; 3], f32)>,
}

/// Sol lointain tant qu'on ne sait rien du monde autour : un vert sombre de prairie.
const GROUND_DEFAULT: [f32; 3] = [0.09, 0.13, 0.06];

/// Altitude (monde) de la base des nuages au-dessus d'un œil à la hauteur `eye_y`, comme
/// le ciel la dessine : 420, ou plus haut si l'on monte (elle reste au-dessus de soi).
pub fn cloud_base_y(eye_y: f32) -> f32 {
    eye_y + (CLOUD_BASE - eye_y).max(MIN_BELOW)
}

/// Altitude (monde) d'un point de la couche de nuages, `above` blocs au-dessus de sa base,
/// à la distance `away` (blocs, à plat) d'un œil à la hauteur `eye_y`. La couche est une
/// coquille : au loin, elle descend vers l'horizon.
pub fn layer_y(eye_y: f32, away: f32, above: f32) -> f32 {
    let r = PLANET + CLOUD_BASE + above;
    eye_y + (r * r - away * away).max(0.0).sqrt() - cam_radius(eye_y)
}

/// Rayon de la caméra pour le ciel : sous la base des nuages, d'au moins `MIN_BELOW`.
fn cam_radius(eye_y: f32) -> f32 {
    PLANET + CLOUD_BASE - (CLOUD_BASE - eye_y).max(MIN_BELOW)
}

/// Épaisseur de la couche de nuages (blocs) selon la météo.
pub fn thickness(w: &SkyWeather) -> f32 {
    170.0 + 90.0 * w.rain + 220.0 * w.storm
}

fn smoothstep(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Lumière physique → couleur affichée (linéaire, 0 … 1), un peu plus saturée (les
/// appareils photo et l'œil renforcent le bleu du ciel).
fn expose(l: Vec3, exposure: f32) -> Vec3 {
    let c = Vec3::ONE - (-l * exposure).exp();
    let y = c.dot(Vec3::new(0.2126, 0.7152, 0.0722));
    (Vec3::splat(y) + (c - Vec3::splat(y)) * 1.25).max(Vec3::ZERO)
}

/// Exposition selon la hauteur du soleil (sinus) : au crépuscule, le ciel réel est cent
/// fois plus sombre qu'en plein jour et l'œil s'y fait ; on l'éclaircit de même.
fn exposure_for(sun_y: f32) -> f32 {
    // (Tard : soleil encore au-dessus de l'horizon, le ciel autour de lui reste orangé au
    // lieu de blanchir, et son disque s'y détache.)
    let s = smoothstep(0.12, -0.12, sun_y);
    EXPOSURE * (1.0 + 9.0 * s * s * s)
}

/// Part de nuit (0 jour … 1 nuit noire) selon la hauteur du soleil.
pub fn night_of(sun_y: f32) -> f32 {
    smoothstep(0.02, -0.16, sun_y)
}

/// Direction du texel (`i`, `j`) de la face `face` d'une image cube (ordre +X, −X, +Y, −Y,
/// +Z, −Z, convention des cartes graphiques).
fn cube_dir(face: usize, i: u32, j: u32, n: u32) -> Vec3 {
    let u = (i as f32 + 0.5) / n as f32 * 2.0 - 1.0;
    let v = (j as f32 + 0.5) / n as f32 * 2.0 - 1.0;
    match face {
        0 => Vec3::new(1.0, -v, -u),
        1 => Vec3::new(-1.0, -v, u),
        2 => Vec3::new(u, 1.0, v),
        3 => Vec3::new(u, -1.0, -v),
        4 => Vec3::new(u, -v, 1.0),
        _ => Vec3::new(-u, -v, -1.0),
    }
    .normalize()
}

/// Couleur affichée du ciel dans la direction `d` (sans nuages ni astres).
fn sky_display(atm: &Atmosphere, d: Vec3, r: &Request) -> Vec3 {
    // Sous l'horizon : la couleur de l'horizon de ce côté, assombrie (brume), comme avant
    // la v216 ; le brouillard des blocs lointains s'y fond.
    let up = Vec3::new(d.x, d.y.max(0.012), d.z).normalize();
    let mut c = Vec3::ZERO;
    if r.sun.y > -0.33 {
        c += expose(atm.sky(up, r.sun), exposure_for(r.sun.y));
    }
    if r.moon.y > -0.2 && r.moon_lit > 0.01 {
        // (Clair de lune : 400 000 fois plus faible que le jour en vrai ; ici ce que l'œil
        // habitué au noir en voit, un bleu profond. (v221) Quatre fois plus sombre : le
        // ciel de la nuit était trop clair, les étoiles s'y perdaient.)
        c += expose(atm.sky(up, r.moon) * (0.008 * r.moon_lit), EXPOSURE)
            * Vec3::new(0.75, 0.85, 1.0);
    }
    // Nuit : bleu très sombre (lueur du ciel, étoiles trop faibles pour être vues une à une).
    let night = night_of(r.sun.y);
    let t = up.y.clamp(0.0, 1.0);
    // (v221) Presque noir au zénith sans lune (trois à quatre fois plus sombre qu'avant).
    c += Vec3::new(0.003, 0.0045, 0.010).lerp(Vec3::new(0.0005, 0.0011, 0.0038), t) * night;
    // Sous l'horizon : le lointain dans la brume, plus sombre et plus terne que le ciel.
    let below = (-d.y * 3.5).clamp(0.0, 1.0);
    let y = c.dot(Vec3::new(0.2126, 0.7152, 0.0722));
    c.lerp(Vec3::splat(y), 0.35 * below) * (1.0 - 0.5 * below)
}

fn bake(atm: &Atmosphere, r: &Request) -> Baked {
    let n = CUBE_N;
    let mut cube = Vec::with_capacity((6 * n * n * 4) as usize);
    for face in 0..6 {
        for j in 0..n {
            for i in 0..n {
                let c = sky_display(atm, cube_dir(face, i, j, n), r);
                cube.extend_from_slice(&[
                    f16_bits(c.x),
                    f16_bits(c.y),
                    f16_bits(c.z),
                    f16_bits(1.0),
                ]);
            }
        }
    }
    // Lumière directe : celle qui arrive aux nuages, un peu plus haut que le sol (ils sont
    // encore éclairés quelques instants après le coucher du soleil).
    let lift = |d: Vec3| Vec3::new(d.x, d.y + 0.035, d.z).normalize();
    let warm = 1.0 + 0.6 * smoothstep(0.10, -0.03, r.sun.y);
    let sun = atm.sun_light(lift(r.sun)) * warm;
    // (v221) Les nuages au clair de lune : moitié moins clairs, avec le ciel de la nuit.
    let moon = atm.sun_light(lift(r.moon)) * Vec3::new(0.055, 0.065, 0.085) * 0.26 * r.moon_lit;
    let zenith = sky_display(atm, Vec3::Y, r);
    let mut horizon = Vec3::ZERO;
    for k in 0..8 {
        let a = k as f32 / 8.0 * std::f32::consts::TAU;
        horizon += sky_display(atm, Vec3::new(a.cos(), 0.06, a.sin()).normalize(), r) / 8.0;
    }
    Baked::Sky {
        sun: r.sun,
        cube,
        light: Lighting {
            sun,
            moon,
            zenith,
            horizon,
        },
    }
}

/// Nombre à virgule de 32 bits → 16 bits (demi-précision), pour les images `Rgba16Float`.
fn f16_bits(v: f32) -> u16 {
    let v = v.clamp(0.0, 65000.0);
    let b = v.to_bits();
    let exp = ((b >> 23) & 0xff) as i32 - 127 + 15;
    let man = b & 0x7f_ffff;
    if v == 0.0 || exp <= 0 {
        // Trop petit : dénormalisé ou zéro.
        if exp < -10 || v == 0.0 {
            return 0;
        }
        let m = (man | 0x80_0000) >> (1 - exp + 13);
        return m as u16;
    }
    ((exp as u32) << 10 | (man >> 13)) as u16
}

// ------------------------------------------------------------------ bruit des nuages

fn hash(x: i32, y: i32, z: i32, seed: u32) -> u32 {
    let mut h = (x as u32).wrapping_mul(0x8da6_b343)
        ^ (y as u32).wrapping_mul(0xd816_3841)
        ^ (z as u32).wrapping_mul(0xcb1a_b31f)
        ^ seed.wrapping_mul(0x9e37_79b9);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2c1b_3c6d);
    h ^= h >> 12;
    h = h.wrapping_mul(0x297a_2d39);
    h ^ (h >> 15)
}

fn unit(h: u32) -> f32 {
    (h >> 8) as f32 / (1u32 << 24) as f32
}

/// Bruit de gradient (Perlin) qui se répète tous les `period` (entier), dans −1 … 1.
fn perlin(p: [f32; 3], period: i32, seed: u32) -> f32 {
    let f = |v: f32| v.floor();
    let (xi, yi, zi) = (f(p[0]) as i32, f(p[1]) as i32, f(p[2]) as i32);
    let (xf, yf, zf) = (p[0] - f(p[0]), p[1] - f(p[1]), p[2] - f(p[2]));
    let fade = |t: f32| t * t * t * (t * (t * 6.0 - 15.0) + 10.0);
    let (u, v, w) = (fade(xf), fade(yf), fade(zf));
    let grad = |dx: i32, dy: i32, dz: i32| {
        let h = hash(
            (xi + dx).rem_euclid(period),
            (yi + dy).rem_euclid(period),
            (zi + dz).rem_euclid(period),
            seed,
        );
        // Douze directions (arêtes du cube).
        let (x, y, z) = (xf - dx as f32, yf - dy as f32, zf - dz as f32);
        match h % 12 {
            0 => x + y,
            1 => -x + y,
            2 => x - y,
            3 => -x - y,
            4 => x + z,
            5 => -x + z,
            6 => x - z,
            7 => -x - z,
            8 => y + z,
            9 => -y + z,
            10 => y - z,
            _ => -y - z,
        }
    };
    let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t;
    lerp(
        lerp(
            lerp(grad(0, 0, 0), grad(1, 0, 0), u),
            lerp(grad(0, 1, 0), grad(1, 1, 0), u),
            v,
        ),
        lerp(
            lerp(grad(0, 0, 1), grad(1, 0, 1), u),
            lerp(grad(0, 1, 1), grad(1, 1, 1), u),
            v,
        ),
        w,
    )
}

/// Bruit cellulaire (Worley) qui se répète tous les `cells` : 1 au centre d'une cellule,
/// 0 loin de tout point.
fn worley(p: [f32; 3], cells: i32, seed: u32) -> f32 {
    let (xi, yi, zi) = (
        p[0].floor() as i32,
        p[1].floor() as i32,
        p[2].floor() as i32,
    );
    let mut best = f32::MAX;
    for dz in -1..=1 {
        for dy in -1..=1 {
            for dx in -1..=1 {
                let (cx, cy, cz) = (xi + dx, yi + dy, zi + dz);
                let (wx, wy, wz) = (
                    cx.rem_euclid(cells),
                    cy.rem_euclid(cells),
                    cz.rem_euclid(cells),
                );
                let fx = cx as f32 + unit(hash(wx, wy, wz, seed));
                let fy = cy as f32 + unit(hash(wx, wy, wz, seed ^ 0x5bd1_e995));
                let fz = cz as f32 + unit(hash(wx, wy, wz, seed ^ 0x1b87_3593));
                let d = (fx - p[0]).powi(2) + (fy - p[1]).powi(2) + (fz - p[2]).powi(2);
                best = best.min(d);
            }
        }
    }
    1.0 - best.sqrt().min(1.0)
}

/// Volume de bruit des nuages (`NOISE_N`³, RGBA 8 bits, qui se répète) :
/// R = formes (Perlin gonflé de Worley, la « choucroute » des cumulus), G et B = Worley de
/// plus en plus fin (bords effilochés), A = Perlin seul (variations à grande échelle).
fn cloud_noise() -> Vec<u8> {
    let n = NOISE_N;
    let mut out = vec![0u8; n * n * n * 4];
    for z in 0..n {
        for y in 0..n {
            for x in 0..n {
                let u = [
                    (x as f32 + 0.5) / n as f32,
                    (y as f32 + 0.5) / n as f32,
                    (z as f32 + 0.5) / n as f32,
                ];
                let at = |f: f32| [u[0] * f, u[1] * f, u[2] * f];
                let mut pf = 0.0;
                let mut amp = 1.0;
                for o in 0..4 {
                    let f = (4 << o) as f32;
                    pf += perlin(at(f), 4 << o, 11 + o as u32) * amp;
                    amp *= 0.5;
                }
                let pf = (0.5 + pf * 0.62).clamp(0.0, 1.0);
                let w4 = worley(at(4.0), 4, 21);
                let w8 = worley(at(8.0), 8, 22);
                let w16 = worley(at(16.0), 16, 23);
                let low = w4 * 0.625 + w8 * 0.25 + w16 * 0.125;
                let r = low + pf * (1.0 - low);
                // Bourgeons : d'autres tirages que les formes (sinon ils les suivraient),
                // étalés sur toute la plage 0 … 1.
                let v4 = worley(at(4.0), 4, 31);
                let v8 = worley(at(8.0), 8, 32);
                let v16 = worley(at(16.0), 16, 33);
                let g = ((v4 * 0.5 + v8 * 0.3 + v16 * 0.2) - 0.24) / 0.5;
                let b =
                    ((worley(at(8.0), 8, 34) * 0.6 + worley(at(16.0), 16, 35) * 0.4) - 0.2) / 0.56;
                let i = ((z * n + y) * n + x) * 4;
                let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                out[i] = q(r);
                out[i + 1] = q(g);
                out[i + 2] = q(b);
                out[i + 3] = q(pf);
            }
        }
    }
    out
}

/// Pour chaque part de ciel couverte (0, 1/32 … 1), le seuil du canal des formes au-dessus
/// duquel se trouve cette part du volume.
fn thresholds(noise: &[u8]) -> [f32; 33] {
    let mut hist = [0u32; 256];
    for t in noise.chunks_exact(4) {
        hist[t[0] as usize] += 1;
    }
    let total = (noise.len() / 4) as f32;
    let mut out = [0.0; 33];
    for (k, o) in out.iter_mut().enumerate() {
        let want = k as f32 / 32.0 * total;
        // Du plus fort au plus faible, jusqu'à avoir la part voulue.
        let mut above = 0.0;
        let mut level = 255usize;
        while level > 0 && above + (hist[level] as f32) <= want {
            above += hist[level] as f32;
            level -= 1;
        }
        *o = (level as f32 + 0.5) / 255.0;
    }
    // Ciel tout couvert : tout le volume est nuage.
    out[32] = 0.0;
    out
}

// ------------------------------------------------------------------ côté carte graphique

impl Sky {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let cube = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("ciel : atmosphère"),
            size: wgpu::Extent3d {
                width: CUBE_N,
                height: CUBE_N,
                depth_or_array_layers: 6,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let cube_view = cube.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::Cube),
            ..Default::default()
        });
        let map = |label| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width: MAP_N,
                    height: MAP_N,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                // Couleurs déjà prêtes à afficher (0 … 1) : 8 bits sRGB suffisent, et
                // se lisent plus vite que 16 bits flottants.
                format: MAP_FORMAT,
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            })
        };
        let map_views = [
            map("ciel : nuages (0)").create_view(&Default::default()),
            map("ciel : nuages (1)").create_view(&Default::default()),
            map("ciel : nuages (2)").create_view(&Default::default()),
        ];
        let noise = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("ciel : bruit des nuages"),
            size: wgpu::Extent3d {
                width: NOISE_N as u32,
                height: NOISE_N as u32,
                depth_or_array_layers: NOISE_N as u32,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D3,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let noise_view = noise.create_view(&Default::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("ciel"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let noise_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("ciel : bruit"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            address_mode_w: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ciel : nuages"),
            size: std::mem::size_of::<CloudParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let tex = |binding, view_dimension| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension,
                multisampled: false,
            },
            count: None,
        };
        let samp = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ciel : nuages"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                tex(1, wgpu::TextureViewDimension::D3),
                samp(2),
                tex(3, wgpu::TextureViewDimension::Cube),
                samp(4),
            ],
        });
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ciel : nuages"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: params.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&noise_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&noise_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&cube_view),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("ciel : nuages"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/sky_clouds.wgsl").into()),
        });
        let pipe_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("ciel : nuages"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("ciel : nuages"),
            layout: Some(&pipe_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_full"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_clouds"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: MAP_FORMAT,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview: None,
            cache: None,
        });
        // Bruit et atmosphère : dans un fil à part (0,1 à 0,2 s de calcul au lancement,
        // puis 60 ms à chaque fois que le soleil a tourné d'un quart de degré).
        let (to_worker, requests) = mpsc::channel::<Request>();
        let (results, from_worker) = mpsc::channel::<Baked>();
        let spawned = std::thread::Builder::new()
            .name("ciel".into())
            .spawn(move || {
                let noise = cloud_noise();
                let table = thresholds(&noise);
                if results.send(Baked::Noise(noise, table)).is_err() {
                    return;
                }
                let atm = Atmosphere::new();
                while let Ok(mut r) = requests.recv() {
                    // La plus récente seulement.
                    while let Ok(newer) = requests.try_recv() {
                        r = newer;
                    }
                    if results.send(bake(&atm, &r)).is_err() {
                        return;
                    }
                }
            });
        if let Err(e) = spawned {
            log::warn!("ciel : fil de calcul refusé ({e}) : ciel sans atmosphère");
        }
        // Tant que rien n'est calculé : cartes vides (ciel sans nuages, noir), jamais lues
        // pleines de restes.
        let zeros = vec![0u8; (CUBE_N * CUBE_N * 8) as usize];
        for layer in 0..6 {
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &cube,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: 0,
                        y: 0,
                        z: layer,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                &zeros,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(CUBE_N * 8),
                    rows_per_image: Some(CUBE_N),
                },
                wgpu::Extent3d {
                    width: CUBE_N,
                    height: CUBE_N,
                    depth_or_array_layers: 1,
                },
            );
        }
        Self {
            cube,
            cube_view,
            map_views,
            noise,
            uniform: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("ciel : uniformes"),
                size: std::mem::size_of::<SkyGlobals>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            noise_view,
            noise_sampler,
            sampler,
            pipeline,
            params,
            group,
            to_worker,
            from_worker,
            pending: false,
            baked_sun: None,
            baked_moon: Vec3::NEG_Y,
            light: Lighting::default(),
            thresholds: None,
            draw: None,
            snaps: [Snapshot::default(); MAPS],
            from: None,
            to: None,
            t: 1.0,
            blend_s: CYCLE_JUMP_S,
            build: None,
            ready: None,
            drift: DVec2::ZERO,
            last_sky_time: None,
            last_frame: std::time::Instant::now(),
            input: None,
            ground: None,
        }
    }

    /// Part éclairée de la lune (0 nouvelle … 1 pleine) vue d'ici.
    fn moon_lit(sun: Vec3, moon: Vec3) -> f32 {
        (0.5 - 0.5 * sun.dot(moon)).clamp(0.0, 1.0)
    }

    /// Reçoit ce que le fil a calculé, et lui demande la suite si le soleil a tourné.
    fn pump(&mut self, queue: &wgpu::Queue, i: &SkyInput) {
        while let Ok(b) = self.from_worker.try_recv() {
            match b {
                Baked::Noise(data, table) => {
                    queue.write_texture(
                        wgpu::TexelCopyTextureInfo {
                            texture: &self.noise,
                            mip_level: 0,
                            origin: wgpu::Origin3d::ZERO,
                            aspect: wgpu::TextureAspect::All,
                        },
                        &data,
                        wgpu::TexelCopyBufferLayout {
                            offset: 0,
                            bytes_per_row: Some(NOISE_N as u32 * 4),
                            rows_per_image: Some(NOISE_N as u32),
                        },
                        wgpu::Extent3d {
                            width: NOISE_N as u32,
                            height: NOISE_N as u32,
                            depth_or_array_layers: NOISE_N as u32,
                        },
                    );
                    self.thresholds = Some(table);
                }
                Baked::Sky { sun, cube, light } => {
                    let per = (CUBE_N * CUBE_N * 4) as usize;
                    for layer in 0..6usize {
                        queue.write_texture(
                            wgpu::TexelCopyTextureInfo {
                                texture: &self.cube,
                                mip_level: 0,
                                origin: wgpu::Origin3d {
                                    x: 0,
                                    y: 0,
                                    z: layer as u32,
                                },
                                aspect: wgpu::TextureAspect::All,
                            },
                            bytemuck::cast_slice(&cube[layer * per..(layer + 1) * per]),
                            wgpu::TexelCopyBufferLayout {
                                offset: 0,
                                bytes_per_row: Some(CUBE_N * 8),
                                rows_per_image: Some(CUBE_N),
                            },
                            wgpu::Extent3d {
                                width: CUBE_N,
                                height: CUBE_N,
                                depth_or_array_layers: 1,
                            },
                        );
                    }
                    self.baked_sun = Some(sun);
                    self.light = light;
                    self.pending = false;
                }
            }
        }
        // Un quart de degré de plus (ou l'heure a sauté) : nouvelle image de l'atmosphère.
        let moved = self
            .baked_sun
            .is_none_or(|s| s.dot(i.sun) < 0.99999 || self.baked_moon.dot(i.moon) < 0.9999);
        if moved && !self.pending {
            let sent = self.to_worker.send(Request {
                sun: i.sun,
                moon: i.moon,
                moon_lit: Self::moon_lit(i.sun, i.moon),
            });
            self.pending = sent.is_ok();
            self.baked_moon = i.moon;
        }
    }

    /// À chaque image, avant de remplir les uniformes : reçoit l'atmosphère, fait avancer le
    /// vent, et choisit les rangées de la carte des nuages à dessiner (`encode`).
    pub fn prepare(&mut self, queue: &wgpu::Queue, i: &SkyInput) {
        self.pump(queue, i);
        self.draw = None;
        let now = std::time::Instant::now();
        let dt = now.duration_since(self.last_frame).as_secs_f32().min(0.25);
        self.last_frame = now;
        // Le vent pousse le repère des nuages, tant que l'heure avance.
        let d_sky = self
            .last_sky_time
            .map_or(0.0, |t| (i.sky_time - t).clamp(0.0, 0.5));
        self.last_sky_time = Some(i.sky_time);
        self.drift += DVec2::new(i.weather.wind[0] as f64, i.weather.wind[1] as f64) * d_sky as f64;
        self.input = Some(*i);
        // (v217) Le sol lointain suit ce que le jeu relève, en une seconde environ.
        if let Some((rgb, y)) = i.ground {
            self.ground = Some(match self.ground {
                Some((old, old_y)) if (old_y - y).abs() < 96.0 => {
                    let k = 1.0 - (-dt / 0.9).exp();
                    (
                        [0, 1, 2].map(|c| old[c] + (rgb[c] - old[c]) * k),
                        old_y + (y - old_y) * k,
                    )
                }
                _ => (rgb, y),
            });
        }
        if self.thresholds.is_none() || self.baked_sun.is_none() {
            return;
        }
        // 1. Le fondu en cours avance (la première carte apparaît en fondu aussi).
        if self.to.is_some() && self.t < 1.0 {
            self.t = (self.t + dt / self.blend_s).min(1.0);
        }
        // 2. Fondu fini et carte suivante prête : on enchaîne, sans temps mort (le ciel
        //    change ainsi de façon continue, jamais par à-coups).
        if self.t >= 1.0 {
            if let Some((k, span)) = self.ready.take() {
                self.from = self.to;
                self.to = Some(k);
                self.t = 0.0;
                self.blend_s = span;
            }
        }
        // 3. Rien en cours ni en attente, et le ciel a changé depuis la dernière carte :
        //    on commence la suivante. (Heure arrêtée et joueur immobile : rien à refaire,
        //    le ciel ne coûte alors plus rien.)
        if self.build.is_none() && self.ready.is_none() {
            let span = match self.to {
                None => Some(CYCLE_JUMP_S),
                Some(k) => self.stale(k, i),
            };
            if let Some(span) = span {
                let shown = [self.to, self.from.filter(|_| self.t < 1.0)];
                let target = (0..MAPS).find(|k| !shown.contains(&Some(*k))).unwrap_or(0);
                self.snapshot(queue, target, i);
                self.build = Some(Build {
                    target,
                    row: 0,
                    due: 0.0,
                    span,
                });
            }
        }
        // 4. Quelques rangées de la carte en cours : toute la carte dans le temps donné,
        //    quel que soit le nombre d'images par seconde.
        if let Some(mut b) = self.build {
            b.due += MAP_N as f32 * dt / (b.span * 0.9);
            let rows = (b.due as u32).min(MAP_N / 8).min(MAP_N - b.row);
            b.due -= rows as f32;
            if rows > 0 {
                self.draw = Some((b.target, b.row, b.row + rows));
                b.row += rows;
            }
            if b.row >= MAP_N {
                self.snaps[b.target].valid = true;
                self.ready = Some((b.target, b.span));
                self.build = None;
            } else {
                self.build = Some(b);
            }
        }
    }

    /// Dessine les rangées choisies par `prepare` dans la carte en cours.
    pub fn encode(&mut self, encoder: &mut wgpu::CommandEncoder) {
        let Some((target, row, end)) = self.draw.take() else {
            return;
        };
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("ciel : nuages"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &self.map_views[target],
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.group, &[]);
        pass.set_scissor_rect(0, row, MAP_N, end - row);
        pass.draw(0..3, 0..1);
    }

    /// Le ciel a-t-il changé depuis l'instantané de la carte `k` ? Si oui : le temps à
    /// donner à la carte suivante (s).
    fn stale(&self, k: usize, i: &SkyInput) -> Option<f32> {
        let s = &self.snaps[k];
        let w = s.weather.unwrap_or(i.weather);
        let cam = DVec2::new(i.eye.x as f64, i.eye.z as f64) - self.drift;
        let moved = (cam - s.cam).length() as f32;
        let climbed = (cam_radius(i.eye.y) - s.r_cam).abs();
        let turned = s.sun.dot(i.sun);
        let d_cover = (w.cover - i.weather.cover).abs();
        let d_rain = (w.rain - i.weather.rain)
            .abs()
            .max((w.storm - i.weather.storm).abs());
        // L'heure ou la météo ont sauté (/timeset, /weather, téléportation).
        if turned < 0.996 || d_cover > 0.25 || d_rain > 0.4 || moved > 500.0 || climbed > 200.0 {
            return Some(CYCLE_JUMP_S);
        }
        let running = i.sky_time != s.sky_time;
        if !running && turned > 0.99999 && d_cover + d_rain < 1e-4 && moved < 40.0 && climbed < 12.0
        {
            return None;
        }
        // Crépuscule (les couleurs tournent en quelques secondes), météo qui change ou
        // déplacement rapide : cartes plus rapprochées.
        let busy =
            i.sun.y.abs() < 0.25 || d_cover + d_rain > 0.004 || moved > 60.0 || climbed > 20.0;
        Some(if busy { CYCLE_BUSY_S } else { CYCLE_CALM_S })
    }

    /// Fige l'instantané de la carte `target` : caméra, lumière et météo de maintenant.
    fn snapshot(&mut self, queue: &wgpu::Queue, target: usize, i: &SkyInput) {
        let cam = DVec2::new(i.eye.x as f64, i.eye.z as f64) - self.drift;
        let r_cam = cam_radius(i.eye.y);
        let w = i.weather;
        // La lumière la plus forte éclaire les nuages : le soleil, ou la lune la nuit.
        let lum = |c: Vec3| c.dot(Vec3::new(0.2126, 0.7152, 0.0722));
        let (dir, col) = if lum(self.light.sun) >= lum(self.light.moon) {
            (i.sun, self.light.sun)
        } else {
            (i.moon, self.light.moon)
        };
        // Sous la pluie, la lumière directe se perd dans la couche (son épaisseur fait
        // l'essentiel) ; la lumière du ciel n'est plus bleue mais grise.
        let direct = 1.0 - 0.25 * w.rain - 0.25 * w.storm;
        let dark = 1.0 - 0.3 * w.rain - 0.35 * w.storm;
        let (gray, over) = self.overcast(&w);
        // (v235) Sous un ciel couvert, la lumière du ciel prend la teinte du temps qu'il
        // fait (gris à peine bleu, gris-bleu de la pluie, ardoise de l'orage).
        let tint = weather_tint(&w);
        let neutral = |c: Vec3| c.lerp(tint * lum(c), over);
        let p = CloudParams {
            origin: [
                cam.x.rem_euclid(WRAP) as f32,
                i.sky_time,
                cam.y.rem_euclid(WRAP) as f32,
                0.0,
            ],
            light_dir: [dir.x, dir.y, dir.z, 0.0],
            light_col: (col * 3.0 * direct.max(0.1)).extend(0.0).to_array(),
            amb_top: (neutral(self.light.zenith) * 1.1 * dark + Vec3::splat(0.004))
                .extend(0.0)
                .to_array(),
            amb_bot: (neutral(self.light.horizon) * 0.9 * dark + Vec3::splat(0.003))
                .extend(0.0)
                .to_array(),
            shape: [
                self.threshold(w.cover),
                0.10 + 0.035 * w.rain + 0.03 * w.storm,
                thickness(&w),
                w.storm.clamp(0.0, 1.0),
            ],
            geo: [r_cam, PLANET + CLOUD_BASE, THETA_MAX, MAP_N as f32],
            haze: gray.extend(over).to_array(),
        };
        queue.write_buffer(&self.params, 0, bytemuck::bytes_of(&p));
        self.snaps[target] = Snapshot {
            valid: false,
            cam,
            r_cam,
            sun: i.sun,
            sky_time: i.sky_time,
            weather: Some(w),
        };
    }

    /// Gris du ciel couvert (la brume et le lointain y tendent), et à quel point le ciel
    /// l'est (0 beau temps … 1 couvert).
    /// (v235) Avec la teinte du temps qu'il fait (`weather_tint`), et un reste de la couleur
    /// du ciel dégagé à cette heure : un soir de pluie garde un peu de son couchant.
    fn overcast(&self, w: &SkyWeather) -> (Vec3, f32) {
        let over = smoothstep(0.55, 0.95, w.cover);
        let clear = self.light.horizon * 0.55 + self.light.zenith * 0.25;
        let y = clear.dot(LUMA);
        let gray = (weather_tint(w) * y).lerp(clear, 0.16) * (1.0 - 0.3 * w.rain - 0.4 * w.storm)
            + Vec3::splat(0.003);
        (gray, over)
    }

    /// Seuil du bruit qui couvre la part `cover` du ciel.
    fn threshold(&self, cover: f32) -> f32 {
        let Some(t) = &self.thresholds else {
            return 1.0;
        };
        let x = cover.clamp(0.0, 1.0) * 32.0;
        let k = (x as usize).min(31);
        t[k] + (t[k + 1] - t[k]) * (x - k as f32)
    }

    /// Envoie les uniformes du ciel de l'image en cours (après `prepare`).
    pub fn upload(&self, queue: &wgpu::Queue) {
        queue.write_buffer(&self.uniform, 0, bytemuck::bytes_of(&self.globals()));
    }

    /// Uniformes du ciel pour l'image en cours (après `prepare`).
    pub fn globals(&self) -> SkyGlobals {
        let Some(i) = &self.input else {
            return SkyGlobals::default();
        };
        let cam = DVec2::new(i.eye.x as f64, i.eye.z as f64) - self.drift;
        let mut weights = [0.0f32; MAPS];
        match (self.from, self.to) {
            (Some(a), Some(b)) if a != b && self.t < 1.0 => {
                weights[a] = 1.0 - self.t;
                weights[b] = self.t;
            }
            // Première carte : elle apparaît en fondu.
            (None, Some(b)) => weights[b] = self.t,
            (_, Some(b)) => weights[b] = 1.0,
            _ => {}
        }
        let cloud = |k: usize| {
            let s = &self.snaps[k];
            let d = cam - s.cam;
            [d.x as f32, d.y as f32, s.r_cam, weights[k]]
        };
        let w = i.weather;
        let night = night_of(i.sun.y);
        let lit = Self::moon_lit(i.sun, i.moon);
        // Disque du soleil : sa lumière qui arrive jusqu'ici (orangée à l'horizon).
        let through = self.light.sun.max_element().min(1.0);
        let (gray, over) = self.overcast(&w);
        SkyGlobals {
            sun: (self.light.sun * 6.0).extend(through).to_array(),
            moon: [
                i.moon.x,
                i.moon.y,
                i.moon.z,
                smoothstep(-0.06, 0.03, i.moon.y) * (0.25 + 0.75 * lit),
            ],
            misc: [
                night * (1.0 - 0.9 * over),
                star_angle(i.sun),
                // Voile de nuages hauts : quand le temps se couvre, avant la pluie.
                smoothstep(0.5, 0.75, w.cover) * (1.0 - smoothstep(0.8, 1.0, w.cover)),
                i.flash[2],
            ],
            fog: gray.extend(over).to_array(),
            cloud: [cloud(0), cloud(1), cloud(2)],
            cloud_geo: [
                cam_radius(i.eye.y),
                PLANET + CLOUD_BASE + 0.4 * thickness(&w),
                1.0 / THETA_MAX,
                thickness(&w),
            ],
            flash: [i.flash[0], i.flash[1], i.flash[3], i.flash[4]],
            ground: {
                let (rgb, y) = self.ground.unwrap_or((GROUND_DEFAULT, i.eye.y - 24.0));
                [rgb[0], rgb[1], rgb[2], (i.eye.y - y).max(1.5)]
            },
            haze: [1.0 / i.haze_len.max(16.0), 0.0, 0.0, 0.0],
        }
    }
}

/// Angle du ciel étoilé (rad) : les étoiles tournent avec le soleil autour de l'axe du
/// monde (z), elles se lèvent et se couchent.
fn star_angle(sun: Vec3) -> f32 {
    sun.y.atan2(sun.x)
}

const LUMA: Vec3 = Vec3::new(0.2126, 0.7152, 0.0722);

/// (v235) Teinte d'un ciel couvert selon le temps qu'il fait, de luminance 1 : un gris à
/// peine bleu sous de simples nuages, le gris-bleu froid de la pluie, l'ardoise sombre (un
/// rien de vert) de l'orage. Avant, les trois étaient du même gris neutre, plus ou moins
/// foncé : l'horizon et les nuages d'un orage avaient la couleur d'une pluie sombre.
pub fn weather_tint(w: &SkyWeather) -> Vec3 {
    let unit = |c: Vec3| c / c.dot(LUMA);
    let cloudy = unit(Vec3::new(0.93, 0.96, 1.0));
    let rain = unit(Vec3::new(0.78, 0.89, 1.0));
    let storm = unit(Vec3::new(0.60, 0.80, 0.98));
    cloudy
        .lerp(rain, w.rain.clamp(0.0, 1.0))
        .lerp(storm, w.storm.clamp(0.0, 1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_floats_round_trip() {
        for v in [0.0f32, 1.0, 0.5, 0.001, 0.1234, 6.0, 100.0, 1e-5] {
            let b = f16_bits(v);
            let (e, m) = ((b >> 10) & 0x1f, b & 0x3ff);
            let back = if e == 0 {
                m as f32 * 2f32.powi(-24)
            } else {
                (1.0 + m as f32 / 1024.0) * 2f32.powi(e as i32 - 15)
            };
            assert!((back - v).abs() <= v * 0.001 + 1e-7, "{v} → {back}");
        }
    }

    /// Le bruit se répète sans couture et couvre bien 0 … 1.
    #[test]
    fn the_cloud_noise_tiles_and_spans_its_range() {
        for (a, b) in [(0.02f32, 4.02f32), (1.3, 5.3)] {
            let p = perlin([a, 0.4, 2.7], 4, 3);
            let q = perlin([b, 0.4, 2.7], 4, 3);
            assert!((p - q).abs() < 1e-5, "{p} {q}");
            let p = worley([a, 0.4, 2.7], 4, 3);
            let q = worley([b, 0.4, 2.7], 4, 3);
            assert!((p - q).abs() < 1e-5, "{p} {q}");
        }
        let n = cloud_noise();
        for c in 0..4 {
            let (lo, hi) = n
                .chunks_exact(4)
                .fold((255u8, 0u8), |(lo, hi), t| (lo.min(t[c]), hi.max(t[c])));
            assert!(lo < 60 && hi > 200, "canal {c} : {lo} … {hi}");
        }
    }

    /// Plus le ciel est couvert, plus le seuil est bas ; rien n'est nuage sans couverture.
    #[test]
    fn more_cover_means_a_lower_threshold() {
        let t = thresholds(&cloud_noise());
        assert!(t[0] > 0.97, "{t:?}");
        assert_eq!(t[32], 0.0);
        assert!(t.windows(2).all(|w| w[1] <= w[0]), "{t:?}");
        assert!(t[16] > 0.3 && t[16] < 0.9, "{t:?}");
    }

    /// La caméra reste toujours sous les nuages, et les faces du cube couvrent toutes les
    /// directions (la face du haut regarde vers le haut).
    #[test]
    fn the_camera_stays_below_the_clouds() {
        assert_eq!(cam_radius(130.0), PLANET + 130.0);
        assert_eq!(
            cam_radius(CLOUD_BASE + 300.0),
            PLANET + CLOUD_BASE - MIN_BELOW
        );
        assert!(cube_dir(2, 32, 32, 64).y > 0.99);
        assert!(cube_dir(3, 32, 32, 64).y < -0.99);
        assert!(cube_dir(0, 32, 32, 64).x > 0.99);
        assert!(cube_dir(5, 32, 32, 64).z < -0.99);
    }
}
