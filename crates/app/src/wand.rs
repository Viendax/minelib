//! Baguette de sélection (comme celle de WorldEdit) : le logo de sélection (carré à
//! poignées) en bleu, dessiné pixel par pixel en 16 × 16 comme les autres textures du jeu. Clic gauche : coin 1, clic droit : coin 2.

use glam::{Mat4, Vec3};
use minelib_render::{Ui, VmVertex, NO_TEXTURE};

/// Image 16 × 16 : le logo de sélection seul (`B` bleu, `b` reflet, `-` `|` liens entre
/// les poignées), sans manche. `.` transparent.
const PIXELS: [&str; 16] = [
    "................",
    "................",
    "..BBB.BBB.BBB...",
    "..BbB-BbB-BbB...",
    "..BBB.BBB.BBB...",
    "...|.......|....",
    "..BBB.....BBB...",
    "..BbB.....BbB...",
    "..BBB.....BBB...",
    "...|.......|....",
    "..BBB.BBB.BBB...",
    "..BbB-BbB-BbB...",
    "..BBB.BBB.BBB...",
    "................",
    "................",
    "................",
];

/// Couleur sRGB d'un pixel, s'il est plein.
fn color(c: char) -> Option<[u8; 3]> {
    Some(match c {
        'B' | '-' | '|' => [40, 110, 230],
        'b' => [150, 200, 255],
        _ => return None,
    })
}

fn lin(c: [u8; 3]) -> [f32; 3] {
    c.map(|v| (v as f32 / 255.0).powf(2.2))
}

/// Pixels pleins : (colonne, ligne, couleur linéaire).
fn pixels() -> impl Iterator<Item = (usize, usize, [f32; 3])> {
    PIXELS.iter().enumerate().flat_map(|(y, row)| {
        row.chars()
            .enumerate()
            .filter_map(move |(x, c)| color(c).map(|col| (x, y, lin(col))))
    })
}

/// Icône dans un emplacement d'inventaire : `size` pixels de côté, centrée.
pub fn icon(ui: &mut Ui, cx: f32, cy: f32, size: f32) {
    let p = size / 16.0;
    let (x0, y0) = (cx - size / 2.0, cy - size / 2.0);
    for (x, y, c) in pixels() {
        ui.rect(
            x0 + x as f32 * p,
            y0 + y as f32 * p,
            p,
            p,
            [c[0], c[1], c[2], 1.0],
        );
    }
}

/// Baguette tenue en main : chaque pixel est un petit pavé (objet « en relief » comme les
/// objets plats de Minecraft). `mvp` place l'image 0..1 × 0..1 (y vers le haut).
pub fn model(out: &mut Vec<VmVertex>, mvp: Mat4, light: [f32; 3]) {
    let px = 1.0 / 16.0;
    let depth = px;
    for (x, y, c) in pixels() {
        let x0 = x as f32 * px;
        let y0 = 1.0 - (y + 1) as f32 * px;
        let a = Vec3::new(x0, y0, -depth / 2.0);
        let b = Vec3::new(x0 + px, y0 + px, depth / 2.0);
        let faces: [([Vec3; 4], f32); 6] = [
            (
                [
                    v(a, b, 0, 0, 1),
                    v(a, b, 1, 0, 1),
                    v(a, b, 1, 1, 1),
                    v(a, b, 0, 1, 1),
                ],
                1.0,
            ),
            (
                [
                    v(a, b, 1, 0, 0),
                    v(a, b, 0, 0, 0),
                    v(a, b, 0, 1, 0),
                    v(a, b, 1, 1, 0),
                ],
                0.8,
            ),
            (
                [
                    v(a, b, 0, 1, 0),
                    v(a, b, 0, 1, 1),
                    v(a, b, 1, 1, 1),
                    v(a, b, 1, 1, 0),
                ],
                0.95,
            ),
            (
                [
                    v(a, b, 0, 0, 0),
                    v(a, b, 1, 0, 0),
                    v(a, b, 1, 0, 1),
                    v(a, b, 0, 0, 1),
                ],
                0.6,
            ),
            (
                [
                    v(a, b, 1, 0, 1),
                    v(a, b, 1, 0, 0),
                    v(a, b, 1, 1, 0),
                    v(a, b, 1, 1, 1),
                ],
                0.75,
            ),
            (
                [
                    v(a, b, 0, 0, 0),
                    v(a, b, 0, 0, 1),
                    v(a, b, 0, 1, 1),
                    v(a, b, 0, 1, 0),
                ],
                0.7,
            ),
        ];
        for (q, shade) in faces {
            let col = [
                c[0] * shade * light[0],
                c[1] * shade * light[1],
                c[2] * shade * light[2],
                1.0,
            ];
            for i in [0usize, 1, 2, 2, 3, 0] {
                out.push(VmVertex {
                    clip: (mvp * q[i].extend(1.0)).to_array(),
                    uv: [0.0, 0.0],
                    layer: NO_TEXTURE,
                    _pad: 0,
                    color: col,
                });
            }
        }
    }
}

/// Coin d'un pavé : 0 = bord min, 1 = bord max sur chaque axe.
fn v(a: Vec3, b: Vec3, x: u8, y: u8, z: u8) -> Vec3 {
    Vec3::new(
        if x == 1 { b.x } else { a.x },
        if y == 1 { b.y } else { a.y },
        if z == 1 { b.z } else { a.z },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixel_art_is_16_by_16_blue_logo_only() {
        assert!(PIXELS.iter().all(|r| r.chars().count() == 16));
        let all = pixels().count();
        let blue = pixels().filter(|p| p.2[2] > p.2[0] * 2.0).count();
        assert!(blue > 40 && blue == all, "bleu {blue} sur {all}");
    }
}
