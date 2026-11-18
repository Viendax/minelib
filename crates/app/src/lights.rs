//! Lumières placées par le joueur (éditeur de lumières, touche L) : lumières ponctuelles et
//! projecteurs, enregistrées avec chaque monde (`lumieres.json` dans son dossier), réglages
//! généraux (`.minelib/lumieres.txt`), gobos (motifs fournis et images du dossier
//! `.minelib/gobos`) et lumières automatiques sur les blocs lumineux.
//!
//! Version Minelib d'IRLights (mod Minecraft de qualet, licence MIT) : mêmes réglages et
//! mêmes valeurs par défaut (voir `minelib_render::DynLight` pour le rendu).
//!
//! Les lumières restent chez le joueur qui les place (comme dans IRLights) : sur un serveur
//! de construction, elles sont gardées avec sa copie du monde et les autres ne les voient
//! pas. Elles sont coupées en mode FPS (rien ne doit éclairer un coin sombre pour un seul
//! joueur).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use glam::Vec3;
use minelib_render::{DynKind, DynLight, DynSettings, GoboImage};
use minelib_world::World;
use serde::{Deserialize, Serialize};

/// Fichier des lumières d'un monde (dans son dossier).
pub const SCENE_FILE: &str = "lumieres.json";
/// Réglages généraux (dans `.minelib`).
const SETTINGS_FILE: &str = "lumieres.txt";
/// Date (secondes Unix) de la dernière partie avec des lumières (dans `.minelib`) : leurs
/// shaders sont alors compilés dès le lancement du jeu.
const USED_FILE: &str = "lumieres_utilisees.txt";
/// Au-delà, on ne les prépare plus au lancement.
const USED_FOR_S: u64 = 7 * 24 * 3600;

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Note qu'une partie s'est servie des lumières (voir [`used_recently`]).
pub fn mark_used(root: &Path) {
    if let Err(e) = std::fs::write(root.join(USED_FILE), format!("{}\n", unix_now())) {
        log::warn!("{USED_FILE} : {e}");
    }
}

/// Les lumières ont-elles servi ces sept derniers jours ?
pub fn used_recently(root: &Path) -> bool {
    std::fs::read_to_string(root.join(USED_FILE))
        .ok()
        .and_then(|t| t.trim().parse::<u64>().ok())
        .is_some_and(|t| unix_now().saturating_sub(t) < USED_FOR_S)
}
/// Dossier des images de gobos du joueur (dans `.minelib`).
pub const GOBO_DIR: &str = "gobos";
/// Lumières placées au plus dans un monde.
pub const MAX_PLACED: usize = 192;

/// Sorte de lumière.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    #[default]
    #[serde(rename = "ponctuelle")]
    Point,
    #[serde(rename = "projecteur")]
    Spot,
}

/// Une lumière placée (format du fichier `lumieres.json`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PlacedLight {
    /// Identifiant stable pendant la partie (cache des ombres) ; pas enregistré.
    #[serde(skip)]
    pub id: u64,
    #[serde(rename = "nom")]
    pub name: String,
    #[serde(rename = "sorte")]
    pub kind: Kind,
    /// Allumée (une lumière éteinte reste dans la liste).
    #[serde(rename = "allumee")]
    pub on: bool,
    pub position: [f64; 3],
    /// Direction du projecteur (vers le bas par défaut).
    pub direction: [f32; 3],
    /// Couleur (sRGB, 0..1).
    #[serde(rename = "couleur")]
    pub color: [f32; 3],
    #[serde(rename = "intensite")]
    pub intensity: f32,
    /// Portée d'une lumière ponctuelle (blocs).
    #[serde(rename = "rayon")]
    pub radius: f32,
    /// Portée d'un projecteur (blocs).
    #[serde(rename = "portee")]
    pub range: f32,
    /// Angle du faisceau (degrés, angle total) et douceur du bord (degrés).
    pub angle: f32,
    #[serde(rename = "douceur_bord")]
    pub edge: f32,
    /// Rayons volumétriques : allumés, force, densité, directivité (anisotropie).
    #[serde(rename = "rayons")]
    pub volumetric: bool,
    #[serde(rename = "force_rayons")]
    pub beam: f32,
    #[serde(rename = "densite")]
    pub density: f32,
    #[serde(rename = "directivite")]
    pub anisotropy: f32,
    #[serde(rename = "ombres")]
    pub shadows: bool,
    /// Douceur des ombres : taille de la source (blocs), 0 = réglage général.
    #[serde(rename = "douceur_ombres")]
    pub bulb: f32,
    #[serde(rename = "entites_seulement")]
    pub entities_only: bool,
    #[serde(rename = "blocs_seulement")]
    pub blocks_only: bool,
    /// Gobo (projecteur) : nom du motif ("" : aucun), rotation (degrés), échelle, inversé.
    pub gobo: String,
    pub gobo_rotation: f32,
    pub gobo_echelle: f32,
    pub gobo_inverse: bool,
}

impl Default for PlacedLight {
    fn default() -> Self {
        Self {
            id: 0,
            name: "Lumière".into(),
            kind: Kind::Point,
            on: true,
            position: [0.0; 3],
            direction: [0.0, -1.0, 0.0],
            color: [1.0; 3],
            intensity: 1.0,
            radius: 6.0,
            range: 12.0,
            angle: 35.0,
            edge: 10.0,
            volumetric: true,
            beam: 1.0,
            density: 0.05,
            anisotropy: 0.4,
            shadows: true,
            bulb: 0.0,
            entities_only: false,
            blocks_only: false,
            gobo: String::new(),
            gobo_rotation: 0.0,
            gobo_echelle: 1.0,
            gobo_inverse: false,
        }
    }
}

impl PlacedLight {
    /// Valeurs ramenées dans leurs bornes (fichier modifié à la main, ancienne version).
    pub fn sanitize(&mut self) {
        let f = |v: f32, lo: f32, hi: f32, d: f32| if v.is_finite() { v.clamp(lo, hi) } else { d };
        for c in &mut self.color {
            *c = f(*c, 0.0, 1.0, 1.0);
        }
        for p in &mut self.position {
            if !p.is_finite() {
                *p = 0.0;
            }
        }
        let d = Vec3::from(self.direction);
        self.direction = if d.is_finite() && d.length_squared() > 1e-8 {
            d.normalize().to_array()
        } else {
            [0.0, -1.0, 0.0]
        };
        self.intensity = f(self.intensity, 0.0, 20.0, 1.0);
        self.radius = f(self.radius, 0.1, 64.0, 6.0);
        self.range = f(self.range, 0.1, 128.0, 12.0);
        self.angle = f(self.angle, 1.0, 179.0, 35.0);
        self.edge = f(self.edge, 0.0, 60.0, 10.0);
        self.beam = f(self.beam, 0.0, 5.0, 1.0);
        self.density = f(self.density, 0.005, 0.5, 0.05);
        self.anisotropy = f(self.anisotropy, -0.95, 0.95, 0.4);
        self.bulb = f(self.bulb, 0.0, 2.0, 0.0);
        self.gobo_rotation = f(self.gobo_rotation, 0.0, 360.0, 0.0);
        self.gobo_echelle = f(self.gobo_echelle, 0.1, 4.0, 1.0);
        if self.entities_only && self.blocks_only {
            self.blocks_only = false;
        }
        if self.name.trim().is_empty() {
            self.name = "Lumière".into();
        }
        self.name = self.name.chars().take(40).collect();
    }

    /// Réglages visuels par défaut (nom, sorte, position et direction gardés).
    pub fn reset_look(&mut self) {
        let keep = (
            self.id,
            self.name.clone(),
            self.kind,
            self.position,
            self.direction,
            self.on,
        );
        *self = Self::default();
        (
            self.id,
            self.name,
            self.kind,
            self.position,
            self.direction,
            self.on,
        ) = keep;
    }

    pub fn pos(&self) -> Vec3 {
        Vec3::new(
            self.position[0] as f32,
            self.position[1] as f32,
            self.position[2] as f32,
        )
    }

    /// Portée (rayon ou longueur du faisceau).
    pub fn reach(&self) -> f32 {
        match self.kind {
            Kind::Point => self.radius,
            Kind::Spot => self.range,
        }
    }

    /// Angle intérieur du cône (pleine lumière) : l'angle moins la douceur du bord.
    pub fn inner_angle(&self) -> f32 {
        (self.angle - self.edge).max(1.0).min(self.angle)
    }

    /// Lumière pour le rendu ; `gobo` : couche du motif nommé, s'il existe.
    pub fn to_dyn(&self, gobo: Option<u32>) -> DynLight {
        let lin = self.color.map(|c| c.clamp(0.0, 1.0).powf(2.2));
        DynLight {
            id: self.id,
            kind: match self.kind {
                Kind::Point => DynKind::Point,
                Kind::Spot => DynKind::Spot,
            },
            pos: self.pos().to_array(),
            dir: self.direction,
            color: lin,
            intensity: self.intensity,
            range: self.reach(),
            outer_deg: self.angle,
            inner_deg: self.inner_angle(),
            entities_only: self.entities_only,
            blocks_only: self.blocks_only,
            bulb: self.bulb,
            shadows: self.shadows,
            inside_block: false,
            beam: if self.volumetric { self.beam } else { 0.0 },
            density: self.density,
            anisotropy: self.anisotropy,
            gobo: if self.kind == Kind::Spot { gobo } else { None },
            gobo_rotation: self.gobo_rotation.to_radians(),
            gobo_scale: self.gobo_echelle,
            gobo_invert: self.gobo_inverse,
        }
    }
}

/// Contenu de `lumieres.json`.
#[derive(Serialize, Deserialize, Default)]
struct SceneFile {
    version: u32,
    #[serde(rename = "lumieres")]
    lights: Vec<PlacedLight>,
}

/// Lumières d'un monde, relues de son dossier (aucune si le fichier manque ou est abîmé :
/// rien ne passe d'un monde à l'autre).
pub fn load_scene(dir: &Path) -> Vec<PlacedLight> {
    let path = dir.join(SCENE_FILE);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    match serde_json::from_str::<SceneFile>(&text) {
        Ok(f) => {
            let mut v = f.lights;
            v.truncate(MAX_PLACED);
            for l in &mut v {
                l.sanitize();
            }
            log::info!("{} lumière(s) relue(s) pour ce monde", v.len());
            v
        }
        Err(e) => {
            log::warn!("{} illisible ({e}) : aucune lumière", path.display());
            Vec::new()
        }
    }
}

/// Enregistre les lumières d'un monde (fichier supprimé s'il n'y en a plus).
pub fn save_scene(dir: &Path, lights: &[PlacedLight]) -> std::io::Result<()> {
    let path = dir.join(SCENE_FILE);
    if lights.is_empty() {
        return match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    let f = SceneFile {
        version: 1,
        lights: lights.to_vec(),
    };
    let text = serde_json::to_string_pretty(&f).map_err(std::io::Error::other)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, &path)
}

/// Réglages généraux de l'éditeur et du rendu des lumières.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LightSettings {
    pub render: DynSettings,
    /// Guides (portée, cône) dessinés dans le monde quand l'éditeur est ouvert.
    pub guides: bool,
    /// Lumières automatiques sur les blocs lumineux (expérimental).
    pub auto: bool,
    /// Seulement les blocs lumineux qui touchent l'air (les autres ne se voient pas).
    pub auto_culling: bool,
    pub auto_shadows: bool,
    pub auto_intensity: f32,
    /// Portée des lumières automatiques (multiplie celle de chaque bloc).
    pub auto_reach: f32,
    /// Rayon de recherche autour de la caméra (blocs) et nombre maximal.
    pub auto_radius: u32,
    pub auto_max: u32,
    /// L'avertissement « expérimental » a été lu.
    pub auto_warned: bool,
}

impl Default for LightSettings {
    fn default() -> Self {
        Self {
            render: DynSettings::default(),
            guides: true,
            auto: false,
            auto_culling: true,
            auto_shadows: false,
            auto_intensity: 1.0,
            auto_reach: 1.0,
            auto_radius: 48,
            auto_max: 200,
            auto_warned: false,
        }
    }
}

impl LightSettings {
    pub fn load(root: &Path) -> Self {
        let text = std::fs::read_to_string(root.join(SETTINGS_FILE)).unwrap_or_default();
        Self::parse(&text)
    }

    fn parse(text: &str) -> Self {
        let kv = minelib_assets::parse_kv(text);
        let mut s = Self::default();
        let b = |k: &str, d: bool| kv.get(k).map_or(d, |v| v == "true" || v == "1");
        let f = |k: &str, d: f32, lo: f32, hi: f32| {
            kv.get(k)
                .and_then(|v| v.parse::<f32>().ok())
                .filter(|v| v.is_finite())
                .map_or(d, |v| v.clamp(lo, hi))
        };
        let u = |k: &str, d: u32, lo: u32, hi: u32| {
            kv.get(k)
                .and_then(|v| v.parse::<u32>().ok())
                .map_or(d, |v| v.clamp(lo, hi))
        };
        let r = &mut s.render;
        let d = DynSettings::default();
        r.intensity = f("intensite", d.intensity, 0.0, 4.0);
        r.specular = f("reflets", d.specular, 0.0, 4.0);
        r.toon = b("bandes", d.toon);
        r.toon_bands = u("bandes_nombre", d.toon_bands, 2, 8);
        r.toon_smooth = f("bandes_douceur", d.toon_smooth, 0.0, 0.5);
        r.shadows = b("ombres", d.shadows);
        r.block_shadows = b("ombres_blocs", d.block_shadows);
        r.entity_shadows = b("ombres_joueurs", d.entity_shadows);
        r.shadow_quality = u("ombres_qualite", d.shadow_quality as u32, 0, 3) as u8;
        r.shadow_cache = b("ombres_cache", d.shadow_cache);
        r.softness = f("ombres_douceur", d.softness, 0.0, 0.8);
        r.bake_budget = u("ombres_budget", d.bake_budget, 1, 64);
        r.volumetric = b("rayons", d.volumetric);
        r.vl_intensity = f("rayons_intensite", d.vl_intensity, 0.0, 5.0);
        r.vl_steps = u("rayons_pas", d.vl_steps, 8, 96);
        r.vl_scale = if u("rayons_resolution", d.vl_scale, 2, 4) >= 3 {
            4
        } else {
            2
        };
        r.vl_max_dist = f("rayons_distance", d.vl_max_dist, 8.0, 256.0);
        r.vl_tip_boost = f("rayons_lueur", d.vl_tip_boost, 0.0, 4.0);
        r.vl_tip_radius = f("rayons_lueur_rayon", d.vl_tip_radius, 0.1, 4.0);
        r.vl_shadows = b("rayons_ombres", d.vl_shadows);
        r.vl_shadow_stride = u("rayons_ombres_pas", d.vl_shadow_stride, 1, 8);
        r.vl_noise = b("rayons_bruit", d.vl_noise);
        r.vl_noise_amount = f("rayons_bruit_part", d.vl_noise_amount, 0.2, 1.0);
        r.vl_noise_scale = f("rayons_bruit_taille", d.vl_noise_scale, 0.25, 6.0);
        r.vl_noise_speed = f("rayons_bruit_vitesse", d.vl_noise_speed, 0.0, 3.0);
        r.vl_noise_morph = f("rayons_bruit_metamorphose", d.vl_noise_morph, 0.0, 3.0);
        r.vl_noise_stride = u("rayons_bruit_pas", d.vl_noise_stride, 1, 8);
        r.vl_dither_temporal = b("rayons_tramage", d.vl_dither_temporal);
        let dd = Self::default();
        s.guides = b("guides", dd.guides);
        s.auto = b("auto", dd.auto);
        s.auto_culling = b("auto_cachees", dd.auto_culling);
        s.auto_shadows = b("auto_ombres", dd.auto_shadows);
        s.auto_intensity = f("auto_intensite", dd.auto_intensity, 0.0, 4.0);
        s.auto_reach = f("auto_portee", dd.auto_reach, 0.25, 3.0);
        s.auto_radius = u("auto_rayon", dd.auto_radius, 8, 128);
        s.auto_max = u("auto_max", dd.auto_max, 1, 240);
        s.auto_warned = b("auto_lu", dd.auto_warned);
        s
    }

    fn to_text(self) -> String {
        let r = &self.render;
        let mut out = String::from(
            "# Réglages des lumières (éditeur de lumières, touche L). Supprimer ce fichier \
             remet les valeurs par défaut.\n",
        );
        let mut kv = |k: &str, v: String| {
            out.push_str(k);
            out.push('=');
            out.push_str(&v);
            out.push('\n');
        };
        kv("intensite", r.intensity.to_string());
        kv("reflets", r.specular.to_string());
        kv("bandes", r.toon.to_string());
        kv("bandes_nombre", r.toon_bands.to_string());
        kv("bandes_douceur", r.toon_smooth.to_string());
        kv("ombres", r.shadows.to_string());
        kv("ombres_blocs", r.block_shadows.to_string());
        kv("ombres_joueurs", r.entity_shadows.to_string());
        kv("ombres_qualite", r.shadow_quality.to_string());
        kv("ombres_cache", r.shadow_cache.to_string());
        kv("ombres_douceur", r.softness.to_string());
        kv("ombres_budget", r.bake_budget.to_string());
        kv("rayons", r.volumetric.to_string());
        kv("rayons_intensite", r.vl_intensity.to_string());
        kv("rayons_pas", r.vl_steps.to_string());
        kv("rayons_resolution", r.vl_scale.to_string());
        kv("rayons_distance", r.vl_max_dist.to_string());
        kv("rayons_lueur", r.vl_tip_boost.to_string());
        kv("rayons_lueur_rayon", r.vl_tip_radius.to_string());
        kv("rayons_ombres", r.vl_shadows.to_string());
        kv("rayons_ombres_pas", r.vl_shadow_stride.to_string());
        kv("rayons_bruit", r.vl_noise.to_string());
        kv("rayons_bruit_part", r.vl_noise_amount.to_string());
        kv("rayons_bruit_taille", r.vl_noise_scale.to_string());
        kv("rayons_bruit_vitesse", r.vl_noise_speed.to_string());
        kv("rayons_bruit_metamorphose", r.vl_noise_morph.to_string());
        kv("rayons_bruit_pas", r.vl_noise_stride.to_string());
        kv("rayons_tramage", r.vl_dither_temporal.to_string());
        kv("guides", self.guides.to_string());
        kv("auto", self.auto.to_string());
        kv("auto_cachees", self.auto_culling.to_string());
        kv("auto_ombres", self.auto_shadows.to_string());
        kv("auto_intensite", self.auto_intensity.to_string());
        kv("auto_portee", self.auto_reach.to_string());
        kv("auto_rayon", self.auto_radius.to_string());
        kv("auto_max", self.auto_max.to_string());
        kv("auto_lu", self.auto_warned.to_string());
        out
    }

    pub fn save(&self, root: &Path) {
        if let Err(e) = std::fs::write(root.join(SETTINGS_FILE), self.to_text()) {
            log::warn!("réglages des lumières non enregistrés : {e}");
        }
    }
}

/// Préréglages de qualité : (pas des rayons, distance des rayons, pas entre deux ombres,
/// pas entre deux bruits, ombres des rayons, qualité des ombres, ombres des blocs) — les
/// mêmes qu'IRLights.
pub const QUALITY_NAMES: [&str; 4] = ["Rapide", "Équilibré", "Qualité", "Ultra"];
const QUALITY: [(u32, f32, u32, u32, bool, u8, bool); 4] = [
    (16, 48.0, 4, 4, false, 0, false),
    (48, 96.0, 2, 2, true, 1, true),
    (56, 128.0, 1, 2, true, 2, true),
    (64, 192.0, 1, 1, true, 2, true),
];
/// Styles des rayons : (bruit, part, taille, vitesse, lueur à la source, rayon de la lueur).
pub const STYLE_NAMES: [&str; 3] = ["Net", "Poussiéreux", "Enfumé"];
const STYLE: [(bool, f32, f32, f32, f32, f32); 3] = [
    (false, 0.6, 2.0, 0.25, 1.0, 1.5),
    (true, 0.6, 2.0, 0.25, 1.5, 1.5),
    (true, 1.0, 0.5, 3.0, 2.0, 2.0),
];

/// Préréglage de qualité qui correspond aux réglages (`None` : réglages personnels).
pub fn quality_preset(r: &DynSettings) -> Option<usize> {
    QUALITY.iter().position(|q| {
        r.vl_steps == q.0
            && r.vl_max_dist == q.1
            && r.vl_shadow_stride == q.2
            && r.vl_noise_stride == q.3
            && r.vl_shadows == q.4
            && r.shadow_quality == q.5
            && r.block_shadows == q.6
    })
}

pub fn apply_quality(r: &mut DynSettings, i: usize) {
    if let Some(q) = QUALITY.get(i) {
        r.vl_steps = q.0;
        r.vl_max_dist = q.1;
        r.vl_shadow_stride = q.2;
        r.vl_noise_stride = q.3;
        r.vl_shadows = q.4;
        r.shadow_quality = q.5;
        r.block_shadows = q.6;
    }
}

/// Style des rayons qui correspond aux réglages (sans bruit, sa forme ne compte pas).
pub fn style_preset(r: &DynSettings) -> Option<usize> {
    STYLE.iter().position(|s| {
        r.vl_noise == s.0
            && (!r.vl_noise
                || (r.vl_noise_amount == s.1 && r.vl_noise_scale == s.2 && r.vl_noise_speed == s.3))
            && r.vl_tip_boost == s.4
            && r.vl_tip_radius == s.5
    })
}

pub fn apply_style(r: &mut DynSettings, i: usize) {
    if let Some(s) = STYLE.get(i) {
        r.vl_noise = s.0;
        r.vl_noise_amount = s.1;
        r.vl_noise_scale = s.2;
        r.vl_noise_speed = s.3;
        r.vl_tip_boost = s.4;
        r.vl_tip_radius = s.5;
    }
}

// ---------------------------------------------------------------- gobos

/// Un motif de gobo : fourni par le jeu ou image du dossier du joueur.
pub struct Gobo {
    pub name: String,
    pub builtin: bool,
    pub image: GoboImage,
}

/// Motifs fournis : (nom, dessin).
const BUILTIN: [&str; 10] = [
    "Fenêtre",
    "Stores",
    "Feuillage",
    "Grille",
    "Barreaux",
    "Étoile",
    "Anneaux",
    "Points",
    "Vitrail",
    "Rond doux",
];

/// Dessine le motif fourni `k` (blanc : la lumière passe).
fn builtin_gobo(k: usize) -> GoboImage {
    const S: usize = 256;
    let mut luma = vec![0u8; S * S];
    let hash = |x: i32, y: i32| -> f32 {
        let mut h = (x as u32).wrapping_mul(0x9e37_79b9) ^ (y as u32).wrapping_mul(0x85eb_ca6b);
        h = (h ^ (h >> 15)).wrapping_mul(0x2c1b_3c6d);
        h ^= h >> 12;
        (h & 0xffff) as f32 / 65535.0
    };
    let noise = |x: f32, y: f32| -> f32 {
        let (ix, iy) = (x.floor() as i32, y.floor() as i32);
        let (fx, fy) = (x - x.floor(), y - y.floor());
        let s = |t: f32| t * t * (3.0 - 2.0 * t);
        let (ux, uy) = (s(fx), s(fy));
        let a = hash(ix, iy) + (hash(ix + 1, iy) - hash(ix, iy)) * ux;
        let b = hash(ix, iy + 1) + (hash(ix + 1, iy + 1) - hash(ix, iy + 1)) * ux;
        a + (b - a) * uy
    };
    // Cellules du vitrail : 18 germes.
    let seeds: Vec<(f32, f32)> = (0..18)
        .map(|i| (hash(i, 7) * S as f32, hash(i, 13) * S as f32))
        .collect();
    for y in 0..S {
        for x in 0..S {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let (u, v) = (fx / S as f32 * 2.0 - 1.0, fy / S as f32 * 2.0 - 1.0);
            let r = (u * u + v * v).sqrt();
            let a = v.atan2(u);
            let on = |b: bool| if b { 1.0 } else { 0.0 };
            let val: f32 = match k {
                // Fenêtre : quatre carreaux dans un cadre.
                0 => {
                    let m = 22.0;
                    let bar = 10.0;
                    let inside = fx > m && fx < S as f32 - m && fy > m && fy < S as f32 - m;
                    let mid = (fx - S as f32 / 2.0).abs() < bar / 2.0
                        || (fy - S as f32 / 2.0).abs() < bar / 2.0;
                    on(inside && !mid)
                }
                // Stores : lames horizontales.
                1 => on((fy / 24.0).fract() > 0.38),
                // Feuillage : taches de lumière entre les feuilles.
                2 => {
                    let n = noise(fx / 22.0, fy / 22.0) * 0.6
                        + noise(fx / 9.0 + 17.0, fy / 9.0 + 3.0) * 0.4;
                    ((n - 0.52) * 9.0).clamp(0.0, 1.0)
                }
                // Grille : carreaux séparés par des traits.
                3 => on((fx / 32.0).fract() > 0.14 && (fy / 32.0).fract() > 0.14),
                // Barreaux : traits verticaux.
                4 => on((fx / 36.0).fract() > 0.22),
                // Étoile à cinq branches.
                5 => {
                    let k5 = (a + std::f32::consts::FRAC_PI_2)
                        .rem_euclid(std::f32::consts::TAU / 5.0)
                        / (std::f32::consts::TAU / 5.0);
                    let edge = 0.42 + 0.48 * (1.0 - (k5 * 2.0 - 1.0).abs());
                    on(r < edge * 0.95)
                }
                // Anneaux concentriques.
                6 => on((r * 6.0).fract() > 0.45 && r < 1.0),
                // Points.
                7 => {
                    let (cx, cy) = ((fx / 32.0).fract() - 0.5, (fy / 32.0).fract() - 0.5);
                    on(cx * cx + cy * cy < 0.09)
                }
                // Vitrail : cellules séparées par des plombs, teintes de gris différentes.
                8 => {
                    let mut d1 = f32::MAX;
                    let mut d2 = f32::MAX;
                    let mut best = 0;
                    for (i, (sx, sy)) in seeds.iter().enumerate() {
                        let d = ((fx - sx).powi(2) + (fy - sy).powi(2)).sqrt();
                        if d < d1 {
                            d2 = d1;
                            d1 = d;
                            best = i;
                        } else if d < d2 {
                            d2 = d;
                        }
                    }
                    if d2 - d1 < 4.0 || r > 0.98 {
                        0.0
                    } else {
                        0.45 + 0.55 * hash(best as i32, 3)
                    }
                }
                // Rond doux : un faisceau rond au bord estompé.
                _ => (1.0 - ((r - 0.55) / 0.4).clamp(0.0, 1.0)).powf(1.5),
            };
            luma[y * S + x] = (val.clamp(0.0, 1.0) * 255.0).round() as u8;
        }
    }
    GoboImage {
        width: S as u32,
        height: S as u32,
        luma,
    }
}

/// Motifs disponibles : ceux du jeu, puis les images PNG du dossier `gobos`.
pub fn load_gobos(root: &Path) -> Vec<Gobo> {
    let mut out: Vec<Gobo> = BUILTIN
        .iter()
        .enumerate()
        .map(|(k, name)| Gobo {
            name: (*name).into(),
            builtin: true,
            image: builtin_gobo(k),
        })
        .collect();
    let dir = root.join(GOBO_DIR);
    let _ = std::fs::create_dir_all(&dir);
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("png"))
        })
        .collect();
    files.sort();
    for p in files {
        if out.len() >= minelib_render::MAX_GOBOS {
            break;
        }
        match image::open(&p) {
            Ok(img) => {
                let g = img.to_luma8();
                out.push(Gobo {
                    name: p
                        .file_name()
                        .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
                    builtin: false,
                    image: GoboImage {
                        width: g.width(),
                        height: g.height(),
                        luma: g.into_raw(),
                    },
                });
            }
            Err(e) => log::warn!("gobo {} illisible : {e}", p.display()),
        }
    }
    out
}

// ---------------------------------------------------------------- lumières automatiques

/// Lumière posée automatiquement sur un bloc lumineux.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AutoLight {
    pub pos: Vec3,
    /// Couleur (sRGB, la plus forte composante à 255).
    pub rgb: [u8; 3],
    /// Force propre du bloc (0..1 : sa composante la plus forte).
    pub strength: f32,
    pub radius: f32,
    /// Au centre d'un bloc plein (sa case ne lui fait pas d'ombre).
    pub inside: bool,
}

/// Blocs lumineux autour de `cam` (dans `radius` blocs) : torches (à leur flamme) et blocs
/// lumineux (au centre). Les blocs pleins entourés d'autres blocs pleins ne se voient pas
/// (`culling`) ; un amas de blocs pleins de même couleur (lave, pierre lumineuse) donne une
/// seule lumière par cube de 4 blocs. Triés du plus proche au plus loin. Renvoie aussi le
/// nombre de blocs lumineux trouvés.
pub fn scan_auto(world: &World, cam: Vec3, radius: f32, culling: bool) -> (Vec<AutoLight>, usize) {
    use minelib_world::{ChunkPos, CHUNK_SIZE};
    let mut out = Vec::new();
    let mut found = 0;
    let near = |p: Vec3| p.distance_squared(cam) < radius * radius;
    let mut seen = std::collections::HashSet::new();
    for (p, t) in world.torches() {
        let id = world.get(p[0], p[1], p[2]);
        let pos = Vec3::from(crate::session::flame_center(id, p));
        if !near(pos) {
            continue;
        }
        seen.insert(p);
        found += 1;
        let c = world.torch_color(&t);
        let m = c.iter().copied().max().unwrap_or(1).max(1) as u32;
        out.push(AutoLight {
            pos,
            rgb: c.map(|v| (v as u32 * 255 / m) as u8),
            strength: 1.0,
            radius: 10.0,
            inside: false,
        });
    }
    // Amas : (cube de 4, couleur) → (somme des positions, nombre).
    let mut clusters: HashMap<([i32; 3], [u8; 3]), (Vec3, u32)> = HashMap::new();
    let r = (radius as i32 + CHUNK_SIZE as i32 - 1) / CHUNK_SIZE as i32;
    let c0 = ChunkPos::containing(cam.x.floor() as i32, cam.z.floor() as i32);
    for cz in c0.z - r..=c0.z + r {
        for cx in c0.x - r..=c0.x + r {
            let Some(ch) = world.chunk(ChunkPos { x: cx, z: cz }) else {
                continue;
            };
            let (ox, oz) = (cx * CHUNK_SIZE as i32, cz * CHUNK_SIZE as i32);
            for ([x, y, z], id) in ch.emitters() {
                let p = [ox + x as i32, y as i32, oz + z as i32];
                let Some(rgb) = id.def().light else { continue };
                if seen.contains(&p) {
                    continue;
                }
                let def = id.def();
                let torch = id.is_torch();
                let pos = if torch {
                    Vec3::from(crate::session::flame_center(id, p))
                } else {
                    Vec3::new(p[0] as f32 + 0.5, p[1] as f32 + 0.5, p[2] as f32 + 0.5)
                };
                if !near(pos) {
                    continue;
                }
                found += 1;
                if culling && def.full {
                    let open = [
                        [1, 0, 0],
                        [-1, 0, 0],
                        [0, 1, 0],
                        [0, -1, 0],
                        [0, 0, 1],
                        [0, 0, -1],
                    ]
                    .iter()
                    .any(|d| {
                        let n = world.get(p[0] + d[0], p[1] + d[1], p[2] + d[2]);
                        !n.def().full
                    });
                    if !open {
                        continue;
                    }
                }
                let m = rgb.iter().copied().max().unwrap_or(1).max(1) as u32;
                let norm = rgb.map(|v| (v as u32 * 255 / m) as u8);
                if def.full && !torch {
                    let cell = [p[0].div_euclid(4), p[1].div_euclid(4), p[2].div_euclid(4)];
                    let e = clusters.entry((cell, norm)).or_insert((Vec3::ZERO, 0));
                    e.0 += pos;
                    e.1 += 1;
                } else {
                    out.push(AutoLight {
                        pos,
                        rgb: norm,
                        strength: m as f32 / 255.0,
                        radius: 10.0,
                        inside: !torch,
                    });
                }
            }
        }
    }
    for ((_, rgb), (sum, n)) in clusters {
        let pos = sum / n as f32;
        // Plusieurs blocs : une seule lumière un peu plus forte et plus large, au milieu.
        out.push(AutoLight {
            pos,
            rgb,
            strength: (1.0 + 0.1 * (n.min(8) as f32 - 1.0)).min(1.6),
            radius: 12.0 + if n > 1 { 3.0 } else { 0.0 },
            inside: n == 1,
        });
    }
    out.sort_by(|a, b| {
        a.pos
            .distance_squared(cam)
            .total_cmp(&b.pos.distance_squared(cam))
    });
    (out, found)
}

/// Identifiant stable d'une lumière automatique (d'après sa position).
fn auto_id(p: Vec3) -> u64 {
    let q = (p * 16.0).round().as_ivec3();
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for v in [q.x, q.y, q.z] {
        h = (h ^ v as u32 as u64).wrapping_mul(0x0100_0000_01b3);
    }
    h | 1 << 63
}

/// Lumières du monde et réglages, tenus par la partie.
pub struct Lights {
    pub scene: Vec<PlacedLight>,
    pub settings: LightSettings,
    pub gobos: Vec<Gobo>,
    root: PathBuf,
    dir: PathBuf,
    next_id: u64,
    /// Changements pas encore enregistrés (enregistrés une seconde après le dernier).
    scene_dirty: bool,
    settings_dirty: bool,
    save_in: f32,
    /// Lumières automatiques trouvées à la dernière recherche, et combien de blocs.
    auto: Vec<AutoLight>,
    pub auto_found: usize,
    auto_timer: f32,
    /// Les gobos sont à envoyer à la carte graphique.
    pub gobos_dirty: bool,
}

impl Lights {
    /// Lumières du monde `dir` (et réglages de `root`, le dossier `.minelib`).
    pub fn load(root: &Path, dir: &Path) -> Self {
        let mut s = Self {
            scene: load_scene(dir),
            settings: LightSettings::load(root),
            gobos: load_gobos(root),
            root: root.to_path_buf(),
            dir: dir.to_path_buf(),
            next_id: 1,
            scene_dirty: false,
            settings_dirty: false,
            save_in: 0.0,
            auto: Vec::new(),
            auto_found: 0,
            auto_timer: 0.0,
            gobos_dirty: true,
        };
        for i in 0..s.scene.len() {
            s.scene[i].id = s.new_id();
        }
        s
    }

    pub fn new_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    /// Les lumières ont changé : enregistrées bientôt.
    pub fn touch_scene(&mut self) {
        self.scene_dirty = true;
        self.save_in = 1.0;
    }

    pub fn touch_settings(&mut self) {
        self.settings_dirty = true;
        self.save_in = 1.0;
    }

    /// Relit le dossier des gobos (bouton « Actualiser »).
    pub fn reload_gobos(&mut self) {
        self.gobos = load_gobos(&self.root);
        self.gobos_dirty = true;
    }

    pub fn gobo_dir(&self) -> PathBuf {
        self.root.join(GOBO_DIR)
    }

    /// Couche du gobo nommé.
    pub fn gobo_layer(&self, name: &str) -> Option<u32> {
        if name.is_empty() {
            return None;
        }
        self.gobos
            .iter()
            .position(|g| g.name == name)
            .map(|i| i as u32)
    }

    /// Enregistre tout de suite ce qui a changé.
    pub fn save_now(&mut self) {
        if self.scene_dirty {
            self.scene_dirty = false;
            if let Err(e) = save_scene(&self.dir, &self.scene) {
                log::warn!("lumières non enregistrées : {e}");
            }
        }
        if self.settings_dirty {
            self.settings_dirty = false;
            self.settings.save(&self.root);
        }
    }

    /// Une image : enregistrement différé, recherche des lumières automatiques (4 fois par
    /// seconde).
    /// Enregistrement différé, et recherche des lumières automatiques (4 fois par seconde)
    /// si elles sont activées et que les lumières servent (`active` : pas en mode FPS).
    pub fn update(&mut self, dt: f32, world: &World, cam: Vec3, active: bool) {
        if self.scene_dirty || self.settings_dirty {
            self.save_in -= dt;
            if self.save_in <= 0.0 {
                self.save_now();
            }
        }
        if self.settings.auto && active {
            self.auto_timer -= dt;
            if self.auto_timer <= 0.0 {
                self.auto_timer = 0.25;
                let (v, found) = scan_auto(
                    world,
                    cam,
                    self.settings.auto_radius as f32,
                    self.settings.auto_culling,
                );
                self.auto = v;
                self.auto_found = found;
            }
        } else if !self.auto.is_empty() {
            self.auto.clear();
            self.auto_found = 0;
        }
    }

    /// Nombre de lumières automatiques actives.
    pub fn auto_active(&self) -> usize {
        self.auto
            .len()
            .min(self.settings.auto_max as usize)
            .min(minelib_render::MAX_DYN_LIGHTS.saturating_sub(self.scene.len()))
    }

    /// Lumières à dessiner : celles du joueur d'abord (elles passent avant pour les ombres),
    /// puis les automatiques les plus proches.
    pub fn frame_lights(&self) -> Vec<DynLight> {
        let mut out: Vec<DynLight> = self
            .scene
            .iter()
            .filter(|l| l.on)
            .map(|l| l.to_dyn(self.gobo_layer(&l.gobo)))
            .collect();
        let s = &self.settings;
        if s.auto {
            // Ombres : les 8 plus proches au plus (les cartes coûtent cher).
            for (k, a) in self.auto.iter().take(self.auto_active()).enumerate() {
                let lin = a.rgb.map(|v| (v as f32 / 255.0).powf(2.2));
                out.push(DynLight {
                    id: auto_id(a.pos),
                    kind: DynKind::Point,
                    pos: a.pos.to_array(),
                    color: lin,
                    intensity: 1.2 * a.strength * s.auto_intensity,
                    range: a.radius * s.auto_reach,
                    shadows: s.auto_shadows && k < 8,
                    inside_block: a.inside,
                    beam: 0.0,
                    ..Default::default()
                });
            }
        }
        out.truncate(minelib_render::MAX_DYN_LIGHTS);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scene_survives_saving_and_bad_values_are_tamed() {
        let dir = std::env::temp_dir().join(format!("minelib-lumieres-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut a = PlacedLight {
            name: "Scène".into(),
            kind: Kind::Spot,
            position: [250_000.25, 70.5, -12.125],
            direction: [0.0, -1.0, 0.0],
            color: [1.0, 0.2, 0.5],
            gobo: "Fenêtre".into(),
            gobo_rotation: 45.0,
            ..Default::default()
        };
        a.id = 99;
        let b = PlacedLight::default();
        save_scene(&dir, &[a.clone(), b.clone()]).unwrap();
        let back = load_scene(&dir);
        assert_eq!(back.len(), 2);
        // L'identifiant n'est pas enregistré : le reste revient à l'identique.
        let mut want = a.clone();
        want.id = 0;
        assert_eq!(back[0], want);
        assert_eq!(back[1], b);
        // Un fichier plus ancien (champs manquants) ou modifié à la main.
        std::fs::write(
            dir.join(SCENE_FILE),
            r#"{"version":1,"lumieres":[{"nom":"","sorte":"projecteur","intensite":99,
            "angle":-5,"direction":[0,0,0],"entites_seulement":true,"blocs_seulement":true}]}"#,
        )
        .unwrap();
        let l = &load_scene(&dir)[0];
        assert_eq!(l.kind, Kind::Spot);
        assert_eq!(l.intensity, 20.0);
        assert_eq!(l.angle, 1.0);
        assert_eq!(l.direction, [0.0, -1.0, 0.0]);
        assert!(l.entities_only && !l.blocks_only);
        assert_eq!(l.name, "Lumière");
        assert_eq!(l.radius, 6.0, "valeur par défaut");
        // Fichier abîmé : aucune lumière, pas de plantage ; plus de lumière : plus de fichier.
        std::fs::write(dir.join(SCENE_FILE), "{ pas du json").unwrap();
        assert!(load_scene(&dir).is_empty());
        save_scene(&dir, &[]).unwrap();
        assert!(!dir.join(SCENE_FILE).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn settings_round_trip_and_presets_are_recognised() {
        let mut s = LightSettings::default();
        // Les valeurs par défaut sont « Équilibré » et « Poussiéreux », comme IRLights.
        assert_eq!(quality_preset(&s.render), Some(1));
        assert_eq!(style_preset(&s.render), Some(1));
        apply_quality(&mut s.render, 0);
        apply_style(&mut s.render, 2);
        s.auto = true;
        s.auto_max = 37;
        s.render.vl_noise_morph = 1.25;
        let back = LightSettings::parse(&s.to_text());
        assert_eq!(back, s);
        assert_eq!(quality_preset(&back.render), Some(0));
        assert_eq!(style_preset(&back.render), Some(2));
        // Une valeur changée à la main : réglages personnels.
        let mut c = back;
        c.render.vl_steps = 33;
        assert_eq!(quality_preset(&c.render), None);
        // Valeurs absurdes ramenées dans leurs bornes.
        let w = LightSettings::parse("rayons_pas=100000\nombres_qualite=9\nintensite=nan\n");
        assert_eq!(w.render.vl_steps, 96);
        assert_eq!(w.render.shadow_quality, 3);
        assert_eq!(w.render.intensity, 1.0);
    }

    #[test]
    fn placed_lights_become_render_lights() {
        let mut l = PlacedLight {
            kind: Kind::Spot,
            angle: 40.0,
            edge: 15.0,
            range: 20.0,
            radius: 3.0,
            color: [1.0, 0.5, 0.0],
            volumetric: false,
            gobo: "x".into(),
            ..Default::default()
        };
        let d = l.to_dyn(Some(4));
        assert_eq!(d.kind, DynKind::Spot);
        assert_eq!(
            d.range, 20.0,
            "un projecteur porte à sa portée, pas à son rayon"
        );
        assert_eq!((d.outer_deg, d.inner_deg), (40.0, 25.0));
        assert_eq!(d.beam, 0.0, "rayons coupés");
        assert_eq!(d.gobo, Some(4));
        assert!((d.color[1] - 0.5f32.powf(2.2)).abs() < 1e-6);
        // Bord plus doux que l'angle : l'intérieur garde 1°.
        l.edge = 60.0;
        assert_eq!(l.inner_angle(), 1.0);
        // Ponctuelle : son rayon, pas de gobo.
        l.kind = Kind::Point;
        let p = l.to_dyn(Some(4));
        assert_eq!((p.range, p.gobo), (3.0, None));
    }

    #[test]
    fn builtin_gobos_have_light_and_shadow() {
        for (k, name) in BUILTIN.iter().enumerate() {
            let g = builtin_gobo(k);
            assert_eq!(g.luma.len(), (g.width * g.height) as usize);
            let lit = g.luma.iter().filter(|&&v| v > 128).count() as f32 / g.luma.len() as f32;
            assert!((0.05..0.95).contains(&lit), "{name} : {lit} de lumière");
        }
    }

    #[test]
    fn auto_lights_skip_buried_blocks_and_merge_lava_lakes() {
        use minelib_world::{BlockId, ChunkPos};
        let _ = BlockId::AIR;
        let mut w = World::new(0);
        for cz in -2..=2 {
            for cx in -2..=2 {
                w.ensure_chunk(ChunkPos { x: cx, z: cz });
            }
        }
        let glow = minelib_world::registry::registry()
            .by_key("glowstone")
            .unwrap();
        let stone = BlockId::STONE;
        // Un bloc lumineux enterré dans la pierre, un autre à l'air libre.
        let y = 120;
        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    w.set(10 + dx, y + dy, 10 + dz, stone);
                }
            }
        }
        w.set(10, y, 10, glow);
        w.set(-10, y, 4, glow);
        let cam = Vec3::new(0.0, y as f32, 0.0);
        let (v, found) = scan_auto(&w, cam, 40.0, true);
        assert_eq!(found, 2);
        assert_eq!(v.len(), 1, "le bloc enterré ne compte pas");
        assert!(v[0].pos.distance(Vec3::new(-9.5, y as f32 + 0.5, 4.5)) < 1e-4);
        assert!(v[0].inside);
        let (all, _) = scan_auto(&w, cam, 40.0, false);
        assert_eq!(all.len(), 2, "sans tri, tous");
        // Un amas de 8 blocs dans un même cube de 4 : une seule lumière, au milieu.
        let mut w2 = World::new(0);
        w2.ensure_chunk(ChunkPos { x: 0, z: 0 });
        for x in 4..6 {
            for z in 4..6 {
                for yy in y..y + 2 {
                    w2.set(x, yy, z, glow);
                }
            }
        }
        let (m, found) = scan_auto(&w2, Vec3::new(8.0, y as f32, 8.0), 40.0, false);
        assert_eq!(found, 8);
        assert_eq!(m.len(), 1);
        assert!(m[0].pos.distance(Vec3::new(5.0, y as f32 + 1.0, 5.0)) < 1e-4);
        assert!(m[0].radius > 12.0 && !m[0].inside);
    }
}
