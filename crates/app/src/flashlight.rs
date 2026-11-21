//! (v201) Lampe torche : un objet qu'on tient en main et qui éclaire comme un projecteur de
//! l'éditeur de lumières (cône de lumière, arrêté par les murs, rayons dans l'air selon les
//! réglages des lumières), droit devant, là où l'on regarde. Clic droit : l'allumer ou
//! l'éteindre. Comme les autres lumières de l'éditeur, elle n'éclaire que chez celui qui la
//! tient ; (v222) sauf en construction en ligne, où les autres la voient dans sa main et en
//! reçoivent la lumière (voir `REMOTE_LAMPS`).

use glam::{Mat4, Vec3};
use minelib_render::{DynKind, DynLight, VmVertex};

/// Lumière de la lampe pour le rendu : identifiant fixe (sa carte d'ombre est gardée d'une
/// image à l'autre), à part de ceux des lumières posées et automatiques.
pub const LIGHT_ID: u64 = 0x7f1a_5410_0000_0001;

/// Numéro des morceaux du joueur qui tient la lampe (`ShadowBox::owner`) : ils ne font pas
/// d'ombre à sa lumière.
pub const HOLDER: u32 = 1;

/// (v202) Bit en plus du morceau du joueur tout contre la flamme de la torche qu'il tient
/// (sa main) : seul écarté de la lumière de la torche, le reste du joueur y fait son ombre.
pub const NEAR_FLAME: u32 = 2;

/// (v222) Lampes des autres joueurs (construction en ligne) qui éclairent ici : au plus tant,
/// les plus proches de la caméra, à moins de `REMOTE_RANGE` blocs. Chacune a son bit de
/// porteur, `REMOTE_HOLDER << k` : les morceaux de celui qui la tient ne la cachent pas.
pub const REMOTE_LAMPS: usize = 8;
pub const REMOTE_HOLDER: u32 = 4;
pub const REMOTE_RANGE: f32 = 96.0;

/// Couleurs (sRGB) : corps, bagues, interrupteur, tête, verre allumé et éteint.
pub const BODY: [u8; 3] = [40, 40, 44];
const RING: [u8; 3] = [24, 24, 27];
const SWITCH: [u8; 3] = [200, 45, 35];
pub const HEAD: [u8; 3] = [160, 162, 170];
pub const LENS_ON: [u8; 3] = [255, 244, 214];
pub const LENS_OFF: [u8; 3] = [80, 92, 104];

/// Un pavé de la lampe : coins (seizièmes de bloc), couleur sRGB, lumineux.
pub type Part = ([f32; 3], [f32; 3], [u8; 3], bool);

/// Pavés de la lampe debout (verre en haut, sur +y).
pub fn parts(on: bool) -> [Part; 6] {
    [
        // Corps (manche), deux bagues pour la prise, l'interrupteur sur le côté.
        ([6.0, 0.0, 6.0], [10.0, 9.0, 10.0], BODY, false),
        ([5.8, 1.6, 5.8], [10.2, 2.4, 10.2], RING, false),
        ([5.8, 4.6, 5.8], [10.2, 5.4, 10.2], RING, false),
        ([7.3, 6.0, 9.9], [8.7, 7.6, 10.5], SWITCH, false),
        // Tête en métal, puis le verre.
        ([5.0, 9.0, 5.0], [11.0, 13.0, 11.0], HEAD, false),
        (
            [5.6, 13.0, 5.6],
            [10.4, 13.4, 10.4],
            if on { LENS_ON } else { LENS_OFF },
            on,
        ),
    ]
}

fn lin(c: [u8; 3]) -> [f32; 3] {
    c.map(|v| (v as f32 / 255.0).powf(2.2))
}

/// Lampe en triangles de la vue à la première personne (`mvp` : du repère du bloc,
/// 0..1, lampe debout sur +y, vers l'écran).
pub fn model(out: &mut Vec<VmVertex>, mvp: Mat4, light: [f32; 3], on: bool) {
    for (a, b, c, glow) in parts(on) {
        crate::hand::push_box(
            out,
            mvp,
            Vec3::from(a) / 16.0,
            Vec3::from(b) / 16.0,
            None,
            lin(c),
            light,
            glow,
        );
    }
}

/// Icône d'inventaire : la lampe en 3D, penchée, verre allumé, centrée en (`cx`, `cy`),
/// haute d'environ `size` pixels.
pub fn icon(cx: f32, cy: f32, size: f32) -> Vec<VmVertex> {
    let k = size / 0.95;
    let m = Mat4::from_translation(Vec3::new(cx, cy, 0.6))
        * Mat4::from_scale(Vec3::new(k, -k, 0.02))
        * Mat4::from_rotation_z(-0.7)
        * Mat4::from_rotation_x(0.45)
        * Mat4::from_rotation_y(-std::f32::consts::FRAC_PI_4)
        * Mat4::from_translation(Vec3::new(-0.5, -0.42, -0.5));
    let mut out = Vec::new();
    model(&mut out, m, [1.0; 3], true);
    out
}

/// (v215) Réglage du faisceau : 0 large, 1 serré ; la première fois, au milieu. (v217) Sur
/// la course raccourcie (voir `WIDEST`) : un autre fichier que celui de la v215.
const FOCUS_FILE: &str = "torche_faisceau";
/// (v215) Le réglage de la v215, sur l'ancienne course (repris une fois, converti).
const OLD_FOCUS_FILE: &str = "torche_reglage";

/// (v217) La molette vers le bas s'arrête là de l'ancienne course (0 large … 1 serré) :
/// « les 40 derniers % du scroll vers le bas sont de trop » (Viendax). Le réglage va de
/// celui-là (0) au plus serré (1).
pub const WIDEST: f32 = 0.4;
/// (v217) Intensité de la lampe : 30 % de moins que celle de la v215 (Viendax).
const INTENSITY: f32 = 0.7;
/// (v217) Crans de la molette d'un bout à l'autre.
pub const STEPS: f32 = 10.0;

fn read(file: &str) -> Option<f32> {
    std::fs::read_to_string(minelib_assets::data_dir().join(file))
        .ok()
        .and_then(|s| s.trim().parse::<f32>().ok())
        .filter(|f| f.is_finite())
}

/// (v215) Réglage du faisceau gardé (`.minelib/torche_faisceau`), sinon celui de la v215
/// ramené à la nouvelle course (le même faisceau, ou le plus large qui reste), sinon 0,5.
pub fn saved_focus() -> f32 {
    read(FOCUS_FILE)
        .or_else(|| read(OLD_FOCUS_FILE).map(|t| (t - WIDEST) / (1.0 - WIDEST)))
        .map_or(0.5, |f| f.clamp(0.0, 1.0))
}

pub fn save_focus(f: f32) {
    if let Err(e) = std::fs::write(
        minelib_assets::data_dir().join(FOCUS_FILE),
        format!("{:.2}", f.clamp(0.0, 1.0)),
    ) {
        log::warn!("lampe torche : réglage pas enregistré ({e})");
    }
}

/// (v215) Faisceau pour un réglage `focus` (0 large … 1 serré) : (angle du cône en degrés,
/// portée en blocs, intensité). Chaque valeur est la position de son curseur dans
/// l'éditeur de lumières (angle 1 à 179°, portée 0,1 à 128 blocs et intensité 0 à 20, ces
/// deux-là au carré de la position), comme demandé : serré, intensité et portée à 100 % de
/// leur curseur, angle à 25 % ; large, angle à 100 %, portée à 25 % et intensité à 50 %.
/// (v217) Le réglage ne parcourt plus que la fin de cette course, de `WIDEST` (angle à
/// 70 %, portée à 55 %, intensité à 70 % de leur curseur) au plus serré, et l'intensité est
/// 30 % plus basse partout.
pub fn beam(focus: f32) -> (f32, f32, f32) {
    let t = WIDEST + (1.0 - WIDEST) * focus.clamp(0.0, 1.0);
    let angle = 1.0 + 178.0 * (1.0 - 0.75 * t);
    let range = 0.1 + 127.9 * (0.25 + 0.75 * t).powi(2);
    let intensity = INTENSITY * 20.0 * (0.5 + 0.5 * t).powi(2);
    (angle, range, intensity)
}

/// Projecteur de la lampe : part de `pos` (l'ampoule), réglé par `focus` (voir `beam`).
/// (v215) Il vise le centre de l'écran à mi-portée : le point à `range / 2` devant les yeux
/// (`eye`, regard `look`), d'où qu'il parte.
pub fn light(pos: Vec3, eye: Vec3, look: Vec3, focus: f32) -> DynLight {
    let (angle, range, intensity) = beam(focus);
    let aim = eye + look.normalize_or_zero() * range * 0.5;
    let dir = (aim - pos).normalize_or(look);
    DynLight {
        id: LIGHT_ID,
        kind: DynKind::Spot,
        pos: pos.to_array(),
        dir: dir.to_array(),
        color: lin([255, 242, 220]),
        intensity,
        range,
        outer_deg: angle,
        inner_deg: angle * 0.4,
        shadows: true,
        owner: HOLDER,
        beam: 0.35,
        density: 0.04,
        anisotropy: 0.6,
        ..Default::default()
    }
}

/// Où part la lumière d'une lampe tenue par un joueur dont les yeux sont en `eye` et qui
/// regarde vers `look` : au verre, dans la main droite, un peu devant et en dessous des
/// yeux.
pub fn lens_position(eye: Vec3, look: Vec3) -> Vec3 {
    let right = look.cross(Vec3::Y).normalize_or_zero();
    let up = right.cross(look).normalize_or_zero();
    eye + look * 0.45 + right * 0.28 - up * 0.3
}

#[cfg(test)]
mod tests {
    use super::*;

    /// La lampe éclaire depuis la main droite (un peu devant les yeux, à droite, en
    /// dessous), quel que soit le regard ; (v215) son faisceau passe par le centre de l'écran
    /// à mi-portée, quel que soit le réglage ; son verre ne brille qu'allumée.
    #[test]
    fn the_flashlight_shines_ahead_from_the_right_hand() {
        let eye = Vec3::new(10.0, 70.0, -4.0);
        for look in [
            Vec3::new(0.0, 0.0, -1.0),
            Vec3::new(1.0, -0.6, 0.2).normalize(),
            Vec3::new(-0.3, 0.8, 0.5).normalize(),
        ] {
            let p = lens_position(eye, look);
            let d = p - eye;
            assert!(d.dot(look) > 0.4, "devant : {d}");
            assert!(
                d.dot(look.cross(Vec3::Y).normalize()) > 0.2,
                "à droite : {d}"
            );
            assert!(d.length() < 0.7, "dans la main : {d}");
            for focus in [0.0, 0.5, 1.0] {
                let l = light(p, eye, look, focus);
                assert_eq!(l.kind, DynKind::Spot);
                let mid = eye + look * l.range * 0.5;
                let hit = p + Vec3::from(l.dir) * (mid - p).length();
                assert!(
                    (hit - mid).length() < 1e-3,
                    "réglage {focus} : le faisceau passe en {hit}, pas au centre de l'écran {mid}"
                );
                assert!(l.shadows, "arrêtée par les murs");
                assert_eq!(l.owner, HOLDER, "pas l'ombre du bras qui la tient");
            }
        }
        assert!(parts(true).iter().any(|p| p.3));
        assert!(!parts(false).iter().any(|p| p.3));
    }

    /// (v215) Molette : serré, angle à 25 % de son curseur, portée et intensité à 100 % ;
    /// large, angle à 100 %, portée à 25 % et intensité à 50 % (positions des curseurs de
    /// l'éditeur de lumières : angle linéaire, portée et intensité au carré). (v217) Sans
    /// les 40 % les plus larges : au plus large, angle à 70 %, portée à 55 %, intensité à
    /// 70 % ; intensité 30 % plus basse partout.
    #[test]
    fn the_flashlight_focus_moves_the_light_editor_sliders() {
        let close = |a: f32, b: f32| (a - b).abs() < 1e-3;
        let (angle, range, intensity) = beam(1.0);
        assert!(close(angle, 1.0 + 178.0 * 0.25), "angle serré {angle}");
        assert!(close(range, 128.0), "portée serrée {range}");
        assert!(close(intensity, 0.7 * 20.0), "intensité serrée {intensity}");
        let (angle, range, intensity) = beam(0.0);
        assert!(close(angle, 1.0 + 178.0 * 0.7), "angle large {angle}");
        assert!(
            close(range, 0.1 + 127.9 * 0.55 * 0.55),
            "portée large {range}"
        );
        assert!(
            close(intensity, 0.7 * 20.0 * 0.7 * 0.7),
            "intensité large {intensity}"
        );
        // Le milieu de la nouvelle course (réglage de départ) : 70 % de l'ancienne.
        let (angle, _, _) = beam(0.5);
        assert!(
            close(angle, 1.0 + 178.0 * (1.0 - 0.75 * 0.7)),
            "angle au milieu {angle}"
        );
        // Entre les deux, tout suit le réglage sans à-coup.
        let mut last = beam(0.0);
        for k in 1..=20 {
            let b = beam(k as f32 / 20.0);
            assert!(b.0 < last.0 && b.1 > last.1 && b.2 > last.2);
            last = b;
        }
    }
}
