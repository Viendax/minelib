//! Armes du mode FPS : catalogue (noms, prix, caractéristiques), modèles 3D et recul.
//!
//! Modèles : « Ultimate Gun Pack » de Quaternius (CC0, voir `assets/weapons/CREDITS.txt`),
//! un seul pack pour que toutes les armes aient le même style. Chaque modèle OBJ est
//! intégré à l'exécutable, puis remis à l'échelle et orienté (canon vers l'avant) au
//! premier usage.
//!
//! Caractéristiques et motifs de recul : valeurs de jeu propres à Minelib, choisies pour
//! rappeler le comportement des armes d'un FPS tactique (montée verticale puis balayage
//! gauche / droite en rafale), pas des données copiées d'un autre jeu.

use std::sync::OnceLock;

use glam::Vec3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    Pistol,
    Smg,
    Heavy,
    Rifle,
}

impl Category {
    /// Imprécision ajoutée (degrés) en courant à pleine vitesse : faible au pistolet,
    /// énorme au fusil (il faut s'arrêter pour tirer juste).
    pub fn move_inaccuracy(self) -> f32 {
        match self {
            Category::Pistol => 2.5,
            Category::Smg => 3.5,
            Category::Heavy => 5.0,
            Category::Rifle => 7.0,
        }
    }

    pub const ALL: [Category; 4] = [
        Category::Pistol,
        Category::Smg,
        Category::Heavy,
        Category::Rifle,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Category::Pistol => "Pistolets",
            Category::Smg => "Mitraillettes",
            Category::Heavy => "Lourdes",
            Category::Rifle => "Fusils",
        }
    }
}

/// Caractéristiques d'une arme.
pub struct WeaponDef {
    pub name: &'static str,
    pub category: Category,
    pub price: u32,
    /// Fichier du modèle (sans extension) dans `assets/weapons`.
    model: &'static str,
    /// Longueur de l'arme (blocs ≈ mètres).
    pub length: f32,
    /// Dégâts d'une balle (ou d'un plomb).
    pub damage: u32,
    /// Cadence (coups par minute).
    pub rpm: f32,
    pub magazine: u32,
    pub reserve: u32,
    /// Tir automatique (bouton maintenu).
    pub auto: bool,
    pub reload_s: f32,
    /// Plombs par tir (fusils à pompe).
    pub pellets: u32,
    /// Imprécision de base à l'arrêt (degrés) ; ×4 en courant, ×2,5 en l'air.
    pub spread: f32,
    /// Recul vertical par balle (degrés) et amplitude du balayage latéral.
    pub kick: f32,
    pub sway: f32,
    /// Lunette : grossissements successifs au clic droit (×2 pour l'AUG et le SG 553,
    /// ×2 puis ×4 pour les fusils de précision), puis retour sans lunette.
    pub scope: &'static [f32],
}

use Category::*;

#[allow(clippy::too_many_arguments)]
const fn w(
    name: &'static str,
    category: Category,
    price: u32,
    model: &'static str,
    length: f32,
    (damage, rpm, magazine, reserve): (u32, f32, u32, u32),
    auto: bool,
    reload_s: f32,
    (pellets, spread, kick, sway): (u32, f32, f32, f32),
    scope: &'static [f32],
) -> WeaponDef {
    WeaponDef {
        name,
        category,
        price,
        model,
        length,
        damage,
        rpm,
        magazine,
        reserve,
        auto,
        reload_s,
        pellets,
        spread,
        kick,
        sway,
        scope,
    }
}

/// Toutes les armes, par catégorie.
pub static WEAPONS: [WeaponDef; 34] = [
    // Pistolets
    w(
        "Glock-18",
        Pistol,
        200,
        "Pistol_5",
        0.2,
        (30, 400.0, 20, 120),
        false,
        2.2,
        (1, 0.35, 0.9, 0.4),
        &[],
    ),
    w(
        "USP-S",
        Pistol,
        200,
        "Pistol_3",
        0.22,
        (35, 352.0, 12, 24),
        false,
        2.2,
        (1, 0.25, 1.0, 0.4),
        &[],
    ),
    w(
        "P2000",
        Pistol,
        200,
        "Pistol_4",
        0.2,
        (35, 352.0, 13, 52),
        false,
        2.2,
        (1, 0.3, 1.0, 0.4),
        &[],
    ),
    w(
        "Dual Berettas",
        Pistol,
        300,
        "Pistol_1",
        0.2,
        (38, 500.0, 30, 120),
        false,
        3.8,
        (1, 0.45, 0.9, 0.6),
        &[],
    ),
    w(
        "P250",
        Pistol,
        300,
        "Pistol_2",
        0.2,
        (38, 400.0, 13, 26),
        false,
        2.2,
        (1, 0.35, 1.2, 0.5),
        &[],
    ),
    w(
        "Five-SeveN",
        Pistol,
        500,
        "Pistol_4",
        0.21,
        (32, 400.0, 20, 100),
        false,
        2.2,
        (1, 0.3, 1.1, 0.5),
        &[],
    ),
    w(
        "Tec-9",
        Pistol,
        500,
        "SubmachineGun_4",
        0.26,
        (33, 500.0, 18, 90),
        false,
        2.5,
        (1, 0.45, 1.1, 0.6),
        &[],
    ),
    w(
        "CZ75-Auto",
        Pistol,
        500,
        "Pistol_1",
        0.2,
        (31, 600.0, 12, 12),
        true,
        2.7,
        (1, 0.5, 1.3, 0.8),
        &[],
    ),
    w(
        "Desert Eagle",
        Pistol,
        700,
        "Pistol_6",
        0.27,
        (53, 267.0, 7, 35),
        false,
        2.2,
        (1, 0.4, 2.6, 0.8),
        &[],
    ),
    w(
        "R8 Revolver",
        Pistol,
        600,
        "Revolver_1",
        0.3,
        (86, 120.0, 8, 8),
        false,
        2.3,
        (1, 0.35, 3.0, 0.6),
        &[],
    ),
    // Mitraillettes
    w(
        "MAC-10",
        Smg,
        1050,
        "SubmachineGun_1",
        0.3,
        (29, 800.0, 30, 100),
        true,
        2.6,
        (1, 0.7, 0.6, 1.0),
        &[],
    ),
    w(
        "MP9",
        Smg,
        1250,
        "SubmachineGun_2",
        0.4,
        (26, 857.0, 30, 120),
        true,
        2.1,
        (1, 0.6, 0.55, 0.9),
        &[],
    ),
    w(
        "MP7",
        Smg,
        1500,
        "SubmachineGun_5",
        0.45,
        (29, 750.0, 30, 120),
        true,
        3.1,
        (1, 0.5, 0.55, 0.8),
        &[],
    ),
    w(
        "MP5-SD",
        Smg,
        1500,
        "SubmachineGun_3",
        0.5,
        (27, 750.0, 30, 120),
        true,
        2.6,
        (1, 0.5, 0.5, 0.7),
        &[],
    ),
    w(
        "UMP-45",
        Smg,
        1200,
        "AssaultRifle_1",
        0.55,
        (35, 667.0, 25, 100),
        true,
        3.5,
        (1, 0.55, 0.7, 0.8),
        &[],
    ),
    w(
        "P90",
        Smg,
        2350,
        "Bullpup_3",
        0.55,
        (26, 857.0, 50, 100),
        true,
        3.3,
        (1, 0.6, 0.45, 0.7),
        &[],
    ),
    w(
        "PP-Bizon",
        Smg,
        1400,
        "AssaultRifle_4",
        0.6,
        (27, 750.0, 64, 120),
        true,
        2.4,
        (1, 0.6, 0.45, 0.7),
        &[],
    ),
    // Armes lourdes
    w(
        "Nova",
        Heavy,
        1050,
        "Shotgun_1",
        0.95,
        (26, 68.0, 8, 32),
        false,
        4.0,
        (9, 3.5, 3.0, 0.5),
        &[],
    ),
    w(
        "XM1014",
        Heavy,
        2000,
        "Shotgun_4",
        0.95,
        (20, 171.0, 7, 32),
        true,
        4.0,
        (6, 3.8, 2.0, 0.6),
        &[],
    ),
    w(
        "Sawed-Off",
        Heavy,
        1100,
        "Shotgun_SawedOff",
        0.6,
        (32, 71.0, 7, 32),
        false,
        4.0,
        (8, 5.0, 3.0, 0.6),
        &[],
    ),
    w(
        "MAG-7",
        Heavy,
        1300,
        "Shotgun_ShortStock",
        0.75,
        (30, 71.0, 5, 32),
        false,
        2.5,
        (8, 3.0, 3.2, 0.6),
        &[],
    ),
    w(
        "M249",
        Heavy,
        5200,
        "AssaultRifle2_4",
        0.95,
        (32, 750.0, 100, 200),
        true,
        5.7,
        (1, 0.8, 0.6, 1.1),
        &[],
    ),
    w(
        "Negev",
        Heavy,
        1700,
        "AssaultRifle2_3",
        0.95,
        (35, 800.0, 150, 300),
        true,
        5.7,
        (1, 0.9, 0.5, 1.2),
        &[],
    ),
    // Fusils
    w(
        "Galil AR",
        Rifle,
        1800,
        "AssaultRifle_5",
        0.85,
        (30, 666.0, 35, 90),
        true,
        3.0,
        (1, 0.4, 0.75, 1.0),
        &[],
    ),
    w(
        "FAMAS",
        Rifle,
        2050,
        "Bullpup_2",
        0.7,
        (30, 666.0, 25, 90),
        true,
        3.3,
        (1, 0.35, 0.7, 0.9),
        &[],
    ),
    w(
        "AK-47",
        Rifle,
        2700,
        "AssaultRifle_3",
        0.88,
        (36, 600.0, 30, 90),
        true,
        2.5,
        (1, 0.3, 1.0, 1.3),
        &[],
    ),
    w(
        "M4A4",
        Rifle,
        3100,
        "AssaultRifle2_1",
        0.85,
        (33, 666.0, 30, 90),
        true,
        3.1,
        (1, 0.28, 0.8, 1.0),
        &[],
    ),
    w(
        "M4A1-S",
        Rifle,
        2900,
        "AssaultRifle2_2",
        0.9,
        (38, 600.0, 20, 80),
        true,
        3.1,
        (1, 0.25, 0.7, 0.8),
        &[],
    ),
    w(
        "SG 553",
        Rifle,
        3000,
        "AssaultRifle_2",
        0.85,
        (30, 545.0, 30, 90),
        true,
        2.8,
        (1, 0.3, 0.9, 1.0),
        &[2.0],
    ),
    w(
        "AUG",
        Rifle,
        3300,
        "Bullpup_1",
        0.75,
        (28, 600.0, 30, 90),
        true,
        3.8,
        (1, 0.28, 0.8, 0.9),
        &[2.0],
    ),
    w(
        "SSG 08",
        Rifle,
        1700,
        "SniperRifle_1",
        1.05,
        (88, 48.0, 10, 90),
        false,
        3.7,
        (1, 0.15, 2.5, 0.3),
        &[2.0, 4.0],
    ),
    w(
        "AWP",
        Rifle,
        4750,
        "SniperRifle_3",
        1.15,
        // Réarmement 50 % plus rapide que dans CS (41 → 61,5 coups par minute).
        (115, 61.5, 5, 30),
        false,
        3.7,
        (1, 0.1, 4.0, 0.4),
        &[2.0, 4.0],
    ),
    w(
        "G3SG1",
        Rifle,
        5000,
        "SniperRifle_4",
        1.1,
        (80, 240.0, 20, 90),
        true,
        4.7,
        (1, 0.2, 1.8, 0.5),
        &[2.0, 4.0],
    ),
    w(
        "SCAR-20",
        Rifle,
        5000,
        "SniperRifle_2",
        1.1,
        (80, 240.0, 20, 90),
        true,
        3.1,
        (1, 0.2, 1.8, 0.5),
        &[2.0, 4.0],
    ),
];

/// Arme de départ du mode FPS (pistolet).
pub const STARTER: usize = 0;

impl WeaponDef {
    /// Délai entre deux tirs (s).
    pub fn interval(&self) -> f32 {
        60.0 / self.rpm
    }

    /// Arme principale (fusil, mitraillette, lourde) ou secondaire (pistolet).
    pub fn primary(&self) -> bool {
        self.category != Category::Pistol
    }

    /// Camp qui peut l'acheter (`game::CT` ou `game::T`), `None` pour les deux (CS2).
    pub fn side(&self) -> Option<u8> {
        use minelib_net::game::{CT, T};
        match self.name {
            "Glock-18" | "Tec-9" | "MAC-10" | "Sawed-Off" | "Galil AR" | "AK-47" | "SG 553"
            | "G3SG1" => Some(T),
            "USP-S" | "P2000" | "Five-SeveN" | "MP9" | "MAG-7" | "FAMAS" | "M4A4" | "M4A1-S"
            | "AUG" | "SCAR-20" => Some(CT),
            _ => None,
        }
    }

    /// Part des dégâts qui traverse le gilet pare-balles (« armor penetration » de CS2,
    /// arme par arme).
    pub fn armor_pen(&self) -> f32 {
        match self.name {
            "Glock-18" => 0.47,
            "USP-S" | "P2000" => 0.505,
            "Dual Berettas" => 0.575,
            "P250" => 0.64,
            "Five-SeveN" => 0.9115,
            "Tec-9" => 0.906,
            "CZ75-Auto" => 0.7765,
            "Desert Eagle" | "R8 Revolver" => 0.932,
            "MAC-10" | "PP-Bizon" => 0.575,
            "MP9" => 0.6,
            "MP7" | "MP5-SD" => 0.625,
            "UMP-45" => 0.65,
            "P90" => 0.69,
            "Nova" => 0.5,
            "XM1014" => 0.8,
            "Sawed-Off" | "MAG-7" => 0.75,
            "M249" => 0.8,
            "Negev" => 0.71,
            "Galil AR" | "AK-47" => 0.775,
            "FAMAS" | "M4A4" | "M4A1-S" => 0.7,
            "SG 553" => 1.0,
            "AUG" => 0.9,
            "SSG 08" => 0.85,
            "AWP" => 0.975,
            "G3SG1" | "SCAR-20" => 0.825,
            _ => 0.6,
        }
    }

    /// Perte de dégâts avec la distance (« range modifier » de CS2) : les dégâts sont
    /// multipliés par ce nombre tous les 500 unités de CS ([`RANGE_STEP`] blocs).
    pub fn range_mod(&self) -> f32 {
        match self.name {
            "Glock-18" | "Desert Eagle" | "MP7" | "MP5-SD" | "CZ75-Auto" => 0.85,
            "USP-S" | "P2000" => 0.91,
            "Dual Berettas" | "Tec-9" => 0.79,
            "P250" => 0.9,
            "Five-SeveN" => 0.81,
            "R8 Revolver" | "M4A1-S" => 0.94,
            "MAC-10" | "PP-Bizon" => 0.8,
            "MP9" => 0.87,
            "UMP-45" => 0.75,
            "P90" => 0.86,
            "Nova" | "XM1014" => 0.7,
            "Sawed-Off" | "MAG-7" => 0.45,
            "M249" | "Negev" | "M4A4" => 0.97,
            "FAMAS" => 0.96,
            "AWP" => 0.99,
            _ => 0.98,
        }
    }

    /// Dégâts d'une balle à `dist` blocs, avant la partie du corps et le gilet.
    pub fn damage_at(&self, dist: f32) -> f32 {
        self.damage as f32 * self.range_mod().powf(dist.max(0.0) / RANGE_STEP)
    }

    /// Prime d'élimination de CS2.
    pub fn kill_reward(&self) -> u32 {
        match self.name {
            "AWP" => 100,
            "P90" => 300,
            "XM1014" => 600,
            _ if self.pellets > 1 => 900,
            _ if self.category == Category::Smg => 600,
            _ => 300,
        }
    }

    /// Décalage de visée (tangage vers le haut, lacet vers la droite, en degrés) de la
    /// balle n° `n` d'une rafale : les premières montent presque droit, puis l'arme balaie
    /// à droite puis à gauche en continuant à monter un peu (motif fixe, apprenable).
    pub fn spray(&self, n: u32) -> (f32, f32) {
        let n = n as f32;
        let climb = 9.0_f32;
        let up = if n < climb {
            self.kick * n * (1.0 - n / (climb * 3.0))
        } else {
            self.kick * climb * (2.0 / 3.0) + self.kick * 0.15 * (n - climb)
        };
        // Léger départ vers la droite pendant la montée, puis balayage.
        let side = if n < climb {
            self.sway * 0.08 * n
        } else {
            self.sway * 0.72 + self.sway * 2.2 * ((n - climb) * 0.33).sin()
        };
        (up, side)
    }
}

/// Triangle coloré d'un modèle (coordonnées : canon vers −Z, haut +Y, origine à la poignée).
#[derive(Clone, Copy)]
pub struct Tri {
    pub p: [Vec3; 3],
    /// Couleur linéaire (Kd du matériau).
    pub color: [f32; 3],
}

pub struct GunModel {
    pub tris: Vec<Tri>,
    /// Bout du canon (pour la flamme et le départ des balles).
    pub muzzle: Vec3,
}

macro_rules! models {
    ($($n:literal),* $(,)?) => {
        &[$(($n,
            include_str!(concat!("../assets/weapons/", $n, ".obj")),
            include_str!(concat!("../assets/weapons/", $n, ".mtl")))),*]
    };
}

static FILES: &[(&str, &str, &str)] = models!(
    "Pistol_1",
    "Pistol_2",
    "Pistol_3",
    "Pistol_4",
    "Pistol_5",
    "Pistol_6",
    "Revolver_1",
    "SubmachineGun_1",
    "SubmachineGun_2",
    "SubmachineGun_3",
    "SubmachineGun_4",
    "SubmachineGun_5",
    "AssaultRifle_1",
    "AssaultRifle_2",
    "AssaultRifle_3",
    "AssaultRifle_4",
    "AssaultRifle_5",
    "AssaultRifle2_1",
    "AssaultRifle2_2",
    "AssaultRifle2_3",
    "AssaultRifle2_4",
    "Bullpup_1",
    "Bullpup_2",
    "Bullpup_3",
    "Shotgun_1",
    "Shotgun_4",
    "Shotgun_SawedOff",
    "Shotgun_ShortStock",
    "SniperRifle_1",
    "SniperRifle_2",
    "SniperRifle_3",
    "SniperRifle_4",
    "Silencer_1",
    "Silencer_Short",
    "Silencer_long",
    "Bayonet",
);

/// Modèle de l'arme `i` (chargé une fois).
pub fn model(i: usize) -> &'static GunModel {
    static MODELS: OnceLock<Vec<GunModel>> = OnceLock::new();
    &MODELS.get_or_init(|| {
        WEAPONS
            .iter()
            .map(|d| {
                let (_, obj, mtl) = FILES
                    .iter()
                    .find(|f| f.0 == d.model)
                    .copied()
                    .unwrap_or(FILES[0]);
                let mut m = load_obj(obj, mtl, d.length);
                dress(d.name, &mut m);
                m
            })
            .collect()
    })[i]
}

/// Couteau (baïonnette du pack), lame vers −Z, 30 cm.
pub fn knife_model() -> &'static GunModel {
    static KNIFE: OnceLock<GunModel> = OnceLock::new();
    KNIFE.get_or_init(|| file_model("Bayonet", 0.3))
}

/// Modèle d'un fichier du pack, à la longueur donnée.
fn file_model(name: &str, length: f32) -> GunModel {
    let (_, obj, mtl) = FILES
        .iter()
        .find(|f| f.0 == name)
        .copied()
        .unwrap_or(FILES[0]);
    load_obj(obj, mtl, length)
}

/// Détails propres à certaines armes, avec les pièces du même pack : silencieux vissé au
/// bout du canon (USP-S, M4A1-S, MP5-SD), deux pistolets côte à côte (Dual Berettas),
/// teinte sable (Five-SeveN, pour le distinguer du P2000 qui a le même modèle).
fn dress(name: &str, m: &mut GunModel) {
    let silencer = match name {
        "USP-S" => Some(("Silencer_Short", 0.11)),
        "M4A1-S" => Some(("Silencer_long", 0.22)),
        "MP5-SD" => Some(("Silencer_1", 0.16)),
        _ => None,
    };
    if let Some((file, len)) = silencer {
        let s = file_model(file, len);
        // Bout du canon : centre des sommets les plus en avant.
        let pts = m.tris.iter().flat_map(|t| t.p);
        let zmin = pts.clone().map(|p| p.z).fold(f32::MAX, f32::min);
        let (lo, hi) = pts.filter(|p| p.z < zmin + 0.008).fold(
            (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)),
            |(lo, hi), p| (lo.min(p), hi.max(p)),
        );
        let tip = Vec3::new((lo.x + hi.x) * 0.5, (lo.y + hi.y) * 0.5, zmin);
        m.muzzle = tip;
        // Le silencieux (centré) prolonge le canon : son arrière touche la bouche.
        let at = m.muzzle - Vec3::new(0.0, 0.0, len * 0.5);
        m.tris.extend(s.tris.iter().map(|t| Tri {
            p: t.p.map(|p| p + at),
            color: t.color,
        }));
        m.muzzle.z -= len;
    }
    match name {
        "Dual Berettas" => {
            let copy: Vec<Tri> = m
                .tris
                .iter()
                .map(|t| Tri {
                    p: t.p.map(|p| p + Vec3::new(-0.13, 0.0, 0.02)),
                    color: t.color,
                })
                .collect();
            m.tris.extend(copy);
        }
        "Five-SeveN" => {
            for t in &mut m.tris {
                // Gris foncé → vert-sable (polymère du Five-seveN), le métal reste gris.
                let l = (t.color[0] + t.color[1] + t.color[2]) / 3.0;
                if l < 0.2 {
                    t.color = [l * 1.6 + 0.08, l * 1.6 + 0.07, l * 1.1 + 0.03];
                }
            }
        }
        _ => {}
    }
}

/// Lit un OBJ (triangulé en éventail) avec les couleurs de son MTL, puis l'oriente :
/// l'axe le plus long devient l'avant (−Z), le canon du côté le plus fin, haut = +Y,
/// longueur ramenée à `length`.
fn load_obj(obj: &str, mtl: &str, length: f32) -> GunModel {
    let mut colors = std::collections::HashMap::new();
    let mut cur = String::new();
    for line in mtl.lines() {
        let t: Vec<&str> = line.split_whitespace().collect();
        match t.as_slice() {
            ["newmtl", name, ..] => cur = name.to_string(),
            ["Kd", r, g, b, ..] => {
                let p = |s: &str| s.parse::<f32>().unwrap_or(0.5);
                colors.insert(cur.clone(), [p(r), p(g), p(b)]);
            }
            _ => {}
        }
    }
    let mut v: Vec<Vec3> = Vec::new();
    let mut tris = Vec::new();
    let mut color = [0.5; 3];
    for line in obj.lines() {
        let t: Vec<&str> = line.split_whitespace().collect();
        match t.first() {
            Some(&"v") if t.len() >= 4 => {
                let p = |s: &str| s.parse::<f32>().unwrap_or(0.0);
                v.push(Vec3::new(p(t[1]), p(t[2]), p(t[3])));
            }
            Some(&"usemtl") if t.len() >= 2 => {
                color = colors.get(t[1]).copied().unwrap_or([0.5; 3])
            }
            Some(&"f") if t.len() >= 4 => {
                let idx: Vec<usize> = t[1..]
                    .iter()
                    .filter_map(|s| s.split('/').next()?.parse::<i64>().ok())
                    .map(|i| {
                        if i < 0 {
                            (v.len() as i64 + i) as usize
                        } else {
                            (i - 1) as usize
                        }
                    })
                    .collect();
                for k in 1..idx.len().saturating_sub(1) {
                    if let (Some(&a), Some(&b), Some(&c)) =
                        (v.get(idx[0]), v.get(idx[k]), v.get(idx[k + 1]))
                    {
                        tris.push(Tri {
                            p: [a, b, c],
                            color,
                        });
                    }
                }
            }
            _ => {}
        }
    }
    if v.is_empty() {
        return GunModel {
            tris,
            muzzle: Vec3::new(0.0, 0.0, -length),
        };
    }
    let mn = v.iter().fold(Vec3::splat(f32::MAX), |a, &p| a.min(p));
    let mx = v.iter().fold(Vec3::splat(f32::MIN), |a, &p| a.max(p));
    let ext = mx - mn;
    // Axe de la longueur (x ou z), haut = y (export Blender, Y en haut).
    let ax = if ext.x >= ext.z { 0 } else { 2 };
    // Côté du canon : l'extrémité la moins haute (la crosse et la poignée sont plus hautes).
    let band = ext[ax] * 0.06;
    let spread = |at_max: bool| {
        let (lo, hi) = v
            .iter()
            .filter(|p| {
                if at_max {
                    p[ax] > mx[ax] - band
                } else {
                    p[ax] < mn[ax] + band
                }
            })
            .fold((f32::MAX, f32::MIN), |(lo, hi), p| {
                (lo.min(p.y), hi.max(p.y))
            });
        hi - lo
    };
    let muzzle_at_max = spread(true) < spread(false);
    let scale = length / ext[ax].max(1e-6);
    let center = (mn + mx) * 0.5;
    // Rotation autour de Y (sans miroir : le sens des triangles est gardé) qui amène le
    // canon vers −Z.
    let map = |p: Vec3| -> Vec3 {
        let d = p - center;
        let r = match (ax, muzzle_at_max) {
            (0, true) => Vec3::new(d.z, d.y, -d.x),
            (0, false) => Vec3::new(-d.z, d.y, d.x),
            (_, true) => Vec3::new(-d.x, d.y, -d.z),
            (_, false) => d,
        };
        r * scale
    };
    for t in &mut tris {
        t.p = t.p.map(map);
    }
    let muzzle = Vec3::new(0.0, (mx.y - center.y) * scale * 0.55, -length * 0.5);
    GunModel { tris, muzzle }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_weapon_has_a_model_and_sane_stats() {
        for (i, d) in WEAPONS.iter().enumerate() {
            let m = model(i);
            assert!(m.tris.len() > 100, "{} : modèle vide", d.name);
            let zmin = m
                .tris
                .iter()
                .flat_map(|t| t.p)
                .map(|p| p.z)
                .fold(f32::MAX, f32::min);
            let zmax = m
                .tris
                .iter()
                .flat_map(|t| t.p)
                .map(|p| p.z)
                .fold(f32::MIN, f32::max);
            // Longueur de l'arme, silencieux en plus pour les armes « -S » / « -SD ».
            let extra = if d.name.ends_with("-S") || d.name.ends_with("-SD") {
                0.25
            } else if d.name.starts_with("Dual") {
                0.03
            } else {
                0.0
            };
            let l = zmax - zmin;
            assert!(
                l > d.length - 0.02 && l < d.length + extra + 0.02,
                "{} : longueur {l}",
                d.name
            );
            assert!(d.magazine > 0 && d.rpm > 0.0 && d.damage > 0);
        }
        // Une arme de chaque catégorie au moins.
        for c in Category::ALL {
            assert!(WEAPONS.iter().any(|d| d.category == c));
        }
    }

    #[test]
    fn spray_climbs_then_sweeps() {
        let ak = WEAPONS.iter().find(|d| d.name == "AK-47").unwrap();
        let (up1, _) = ak.spray(1);
        let (up8, _) = ak.spray(8);
        assert!(up8 > up1 * 4.0, "monte pendant les premières balles");
        let sides: Vec<f32> = (10..30).map(|n| ak.spray(n).1).collect();
        assert!(
            sides.iter().any(|&s| s > 1.0) && sides.iter().any(|&s| s < 0.0),
            "balaie des deux côtés"
        );
        assert_eq!(ak.spray(0), (0.0, 0.0), "première balle droite");
    }
}

/// 500 unités de CS, en blocs (une unité = 2,54 cm).
pub const RANGE_STEP: f32 = 12.7;

/// Trajet d'une balle : elle traverse un mur d'**un bloc** d'épaisseur (pas deux), puis
/// s'arrête au mur suivant. Même calcul chez le tireur (impacts), l'hôte et le
/// vérificateur (dégâts).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BulletPath {
    /// Distance du premier mur (ou la portée) : avant, pleins dégâts.
    pub first: f32,
    /// Mur traversé : distance de la sortie, puis de l'arrêt final. Entre les deux, les
    /// dégâts sont réduits ([`THROUGH_WALL`]).
    pub through: Option<(f32, f32)>,
    /// Impacts à dessiner : entrée, sortie, arrivée (point, normale de la face).
    pub impacts: Vec<(glam::Vec3, glam::Vec3)>,
}

/// Part des dégâts qui restent après un mur.
pub const THROUGH_WALL: f32 = 0.5;

pub fn bullet_path(
    world: &minelib_world::World,
    origin: glam::Vec3,
    dir: glam::Vec3,
    range: f32,
) -> BulletPath {
    use glam::Vec3;
    use minelib_world::raycast::raycast;
    let normal = |n: [i32; 3]| Vec3::new(n[0] as f32, n[1] as f32, n[2] as f32);
    let Some(h) = raycast(world, origin.into(), dir.into(), range) else {
        return BulletPath {
            first: range,
            ..Default::default()
        };
    };
    let mut path = BulletPath {
        first: h.distance,
        through: None,
        impacts: vec![(origin + dir * h.distance, normal(h.normal))],
    };
    // On avance dans le mur : la balle ressort si la case suivante sur sa route est vide.
    let solid = |p: Vec3| {
        let c = p.floor().as_ivec3();
        (world.get(c.x, c.y, c.z).def().solid, c)
    };
    let (_, entry) = solid(origin + dir * (h.distance + 0.01));
    let mut t = h.distance + 0.01;
    let exit = loop {
        t += 0.02;
        if t > h.distance + 1.8 || t >= range {
            return path;
        }
        let (full, cell) = solid(origin + dir * t);
        if cell != entry {
            if full {
                return path;
            }
            break t;
        }
    };
    let out = origin + dir * (exit + 0.01);
    // Face de sortie : vue depuis l'autre côté.
    if let Some(back) = raycast(world, out.into(), (-dir).into(), 0.2) {
        path.impacts
            .push((out - dir * back.distance, normal(back.normal)));
    }
    let last = match raycast(world, out.into(), dir.into(), range - exit) {
        Some(h2) => {
            path.impacts
                .push((out + dir * h2.distance, normal(h2.normal)));
            exit + 0.01 + h2.distance
        }
        None => range,
    };
    path.through = Some((exit, last));
    path
}
