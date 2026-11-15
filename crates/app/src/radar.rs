//! Minimap (radar) du mode FPS, en haut à droite, avec les règles de CS : vue de dessus
//! autour du joueur (le nord en haut), murs clairs et sol sombre selon la hauteur, sites de
//! bombe, coéquipiers toujours (à leur couleur), adversaires seulement quand ils sont
//! repérés (ceux que l'hôte nous envoie, c'est-à-dire vus), bombe posée ou lâchée.

use glam::Vec3;
use minelib_net::game::FpsMap;
use minelib_render::Ui;
use minelib_world::World;

/// Rayon montré, en blocs.
pub const RADIUS: i32 = 32;
const SIDE: usize = (RADIUS * 2) as usize;

/// Teintes des cases : vide, sol (4 hauteurs), mur.
const SHADES: [[f32; 4]; 6] = [
    [0.0, 0.0, 0.0, 0.0],
    [0.16, 0.17, 0.19, 0.85],
    [0.22, 0.23, 0.26, 0.85],
    [0.29, 0.30, 0.33, 0.85],
    [0.36, 0.37, 0.41, 0.85],
    [0.80, 0.80, 0.78, 0.92],
];

#[derive(Default)]
pub struct Radar {
    /// Bloc au centre de la grille calculée.
    center: Option<[i32; 3]>,
    /// Teinte (indice dans `SHADES`) de chaque case, ligne par ligne (z puis x).
    cells: Vec<u8>,
    /// Sites de bombe : lettre et centre (x, z).
    sites: Vec<(char, [f32; 2])>,
}

impl Radar {
    /// Sites de bombe de la carte.
    pub fn set_map(&mut self, map: &FpsMap) {
        self.sites = map
            .sites
            .iter()
            .enumerate()
            .filter_map(|(i, z)| {
                z.as_ref().map(|z| {
                    let c = [
                        (z.min[0] + z.max[0]) as f32 / 2.0 + 0.5,
                        (z.min[2] + z.max[2]) as f32 / 2.0 + 0.5,
                    ];
                    ((b'A' + i as u8) as char, c)
                })
            })
            .collect();
    }

    /// Recalcule la grille quand le joueur s'est éloigné de plus de 4 blocs.
    pub fn update(&mut self, world: &World, feet: Vec3) {
        let c = [
            feet.x.floor() as i32,
            feet.y.floor() as i32,
            feet.z.floor() as i32,
        ];
        let near = self.center.is_some_and(|o| {
            (o[0] - c[0]).abs() < 4 && (o[1] - c[1]).abs() < 3 && (o[2] - c[2]).abs() < 4
        });
        if near {
            return;
        }
        self.center = Some(c);
        self.cells = vec![0; SIDE * SIDE];
        for dz in 0..SIDE as i32 {
            for dx in 0..SIDE as i32 {
                let (x, z) = (c[0] - RADIUS + dx, c[2] - RADIUS + dz);
                self.cells[(dz as usize) * SIDE + dx as usize] = shade(world, x, c[1], z);
            }
        }
    }

    /// Dessine le radar (carré de `size` pixels en `x`, `y`). `me` : position et regard ;
    /// `mates` : coéquipiers en vie (position, couleur) ; `enemies` : adversaires repérés ;
    /// `bomb` : bombe connue.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &self,
        ui: &mut Ui,
        x: f32,
        y: f32,
        size: f32,
        s: f32,
        me: (Vec3, Vec3),
        mates: &[(Vec3, [f32; 4])],
        enemies: &[Vec3],
        bomb: Option<Vec3>,
    ) {
        let Some(c) = self.center else { return };
        ui.rect(
            x - s,
            y - s,
            size + 2.0 * s,
            size + 2.0 * s,
            [0.0, 0.0, 0.0, 0.6],
        );
        let cell = size / SIDE as f32;
        // Cases, par bandes de même teinte (moins de rectangles).
        for row in 0..SIDE {
            let mut start = 0;
            while start < SIDE {
                let v = self.cells[row * SIDE + start];
                let mut end = start + 1;
                while end < SIDE && self.cells[row * SIDE + end] == v {
                    end += 1;
                }
                if v != 0 {
                    ui.rect(
                        x + start as f32 * cell,
                        y + row as f32 * cell,
                        (end - start) as f32 * cell + 0.5,
                        cell + 0.5,
                        SHADES[v as usize],
                    );
                }
                start = end;
            }
        }
        // Position monde → radar (le nord, -z, en haut). Hors du cadre : collé au bord.
        let origin = Vec3::new((c[0] - RADIUS) as f32, 0.0, (c[2] - RADIUS) as f32);
        let to = |p: Vec3| -> (f32, f32) {
            let u = ((p.x - origin.x) * cell).clamp(0.0, size);
            let v = ((p.z - origin.z) * cell).clamp(0.0, size);
            (x + u, y + v)
        };
        for (letter, at) in &self.sites {
            let (sx, sy) = to(Vec3::new(at[0], 0.0, at[1]));
            let t = letter.to_string();
            let tw = Ui::text_width(&t, s);
            ui.text(sx - tw / 2.0, sy - 4.0 * s, s, [1.0, 0.45, 0.35, 0.95], t);
        }
        let dot = |ui: &mut Ui, p: Vec3, col: [f32; 4], r: f32| {
            let (px, py) = to(p);
            ui.rect(
                px - r - s,
                py - r - s,
                2.0 * r + 2.0 * s,
                2.0 * r + 2.0 * s,
                [0.0, 0.0, 0.0, 0.8],
            );
            ui.rect(px - r, py - r, 2.0 * r, 2.0 * r, col);
        };
        if let Some(b) = bomb {
            dot(ui, b, [1.0, 0.25, 0.2, 1.0], 2.0 * s);
        }
        for e in enemies {
            dot(ui, *e, [0.95, 0.2, 0.2, 1.0], 1.5 * s);
        }
        for (p, col) in mates {
            dot(ui, *p, *col, 1.5 * s);
        }
        // Soi : point blanc et trait dans la direction du regard.
        let (px, py) = to(me.0);
        let dir = Vec3::new(me.1.x, 0.0, me.1.z).normalize_or_zero();
        for k in 1..=4 {
            let f = k as f32 * 1.6 * s;
            ui.rect(
                px + dir.x * f - s / 2.0,
                py + dir.z * f - s / 2.0,
                s,
                s,
                [1.0, 1.0, 1.0, 0.9],
            );
        }
        dot(ui, me.0, [1.0, 1.0, 1.0, 1.0], 2.0 * s);
    }
}

/// Teinte d'une colonne : mur s'il y a un bloc plein à hauteur du torse, sinon sol (plus
/// clair quand il est plus haut), vide s'il n'y a rien en dessous.
fn shade(world: &World, x: i32, feet: i32, z: i32) -> u8 {
    let solid = |y: i32| world.get(x, y, z).is_solid();
    if solid(feet + 1) {
        return 5;
    }
    // `feet` est la case des pieds : le sol normal est juste en dessous (d = 1).
    for d in 0..12 {
        if solid(feet - d) {
            return match d {
                0 => 4,
                1 => 3,
                2..=3 => 2,
                _ => 1,
            };
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use minelib_world::{BlockId, ChunkPos};

    #[test]
    fn floor_walls_and_void() {
        let mut w = World::new(0);
        for dz in -1..=1 {
            for dx in -1..=1 {
                w.insert_empty(ChunkPos { x: dx, z: dz });
            }
        }
        for z in -8..8 {
            for x in -8..8 {
                w.set(x, 10, z, BlockId::STONE);
            }
        }
        // Un mur de 3 de haut en x = 3.
        for y in 11..14 {
            w.set(3, y, 0, BlockId::STONE);
        }
        // Pieds dans la case y = 11 (debout sur y = 10).
        assert_eq!(shade(&w, 0, 11, 0), 3, "sol");
        assert_eq!(shade(&w, 3, 11, 0), 5, "mur");
        assert_eq!(shade(&w, 12, 11, 12), 0, "vide");
        let mut r = Radar::default();
        r.update(&w, Vec3::new(0.5, 11.0, 0.5));
        assert_eq!(r.cells.len(), SIDE * SIDE);
        let at =
            |dx: i32, dz: i32| r.cells[((dz + RADIUS) as usize) * SIDE + (dx + RADIUS) as usize];
        assert_eq!(at(3, 0), 5);
        assert_eq!(at(0, 0), 3);
    }
}
