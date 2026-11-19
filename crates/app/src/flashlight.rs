//! (v201) Lampe torche : un objet qu'on tient en main et qui éclaire comme un projecteur de
//! l'éditeur de lumières (cône de lumière, arrêté par les murs, rayons dans l'air selon les
//! réglages des lumières), droit devant, là où l'on regarde. Clic droit : l'allumer ou
//! l'éteindre. Comme les autres lumières de l'éditeur, elle n'éclaire que chez celui qui la
//! tient.

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

/// Projecteur de la lampe : part de `pos` (le verre), éclaire vers `dir`.
pub fn light(pos: Vec3, dir: Vec3) -> DynLight {
    DynLight {
        id: LIGHT_ID,
        kind: DynKind::Spot,
        pos: pos.to_array(),
        dir: dir.normalize_or_zero().to_array(),
        color: lin([255, 242, 220]),
        intensity: 4.0,
        range: 28.0,
        outer_deg: 40.0,
        inner_deg: 16.0,
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

    /// La lampe éclaire droit devant, depuis la main droite (un peu devant les yeux, à
    /// droite, en dessous), quel que soit le regard ; son verre ne brille qu'allumée.
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
            let l = light(p, look * 3.0);
            assert_eq!(l.kind, DynKind::Spot);
            assert!((Vec3::from(l.dir) - look).length() < 1e-5);
            assert!(
                l.shadows && l.range > 20.0,
                "arrêtée par les murs, porte loin"
            );
            assert_eq!(l.owner, HOLDER, "pas l'ombre du bras qui la tient");
        }
        assert!(parts(true).iter().any(|p| p.3));
        assert!(!parts(false).iter().any(|p| p.3));
    }
}
