//! Minimap (radar) du mode FPS, en haut à droite, avec les règles de CS : vue de dessus
//! autour du joueur (le nord en haut), murs clairs et sol sombre selon la hauteur, sites de
//! bombe, coéquipiers toujours (à leur couleur), adversaires seulement quand ils sont
//! repérés (ceux que l'hôte nous envoie, c'est-à-dire vus), bombe posée ou lâchée.

use glam::Vec3;
use minelib_net::game::FpsMap;
use minelib_render::Ui;
use minelib_world::World;

/// Rayon montré, en blocs (vue de haut : une bonne partie de la carte).
pub const RADIUS: i32 = 48;
/// Rayon gardé en mémoire autour du centre de la grille (plus grand que la vue : on peut
/// s'éloigner du centre sans voir de bord).
const GRID: i32 = 72;
const SIDE: usize = (GRID * 2) as usize;
/// Lignes de la grille recalculées à chaque image (rafraîchissement continu, sans à-coup :
/// blocs qui changent, morceaux de carte chargés après coup).
const ROWS_PER_FRAME: usize = 8;

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
    /// Bloc au centre de la grille gardée.
    center: Option<[i32; 3]>,
    /// Teinte (indice dans `SHADES`) de chaque case, ligne par ligne (z puis x).
    cells: Vec<u8>,
    /// Prochaine ligne à recalculer.
    next_row: usize,
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

    /// Suit le joueur : la grille est recentrée (en gardant ce qui est déjà calculé) quand
    /// il s'éloigne du centre, et quelques lignes sont recalculées à chaque image.
    pub fn update(&mut self, world: &World, feet: Vec3) {
        let c = [
            feet.x.floor() as i32,
            feet.y.floor() as i32,
            feet.z.floor() as i32,
        ];
        match self.center {
            None => {
                self.center = Some(c);
                self.cells = vec![0; SIDE * SIDE];
                // Première fois : tout de suite en entier.
                for row in 0..SIDE {
                    self.row(world, row);
                }
            }
            Some(o)
                if (o[0] - c[0]).abs() > GRID - RADIUS - 4
                    || (o[2] - c[2]).abs() > GRID - RADIUS - 4
                    || (o[1] - c[1]).abs() >= 3 =>
            {
                // Recentrage : ce qui reste dans la grille est déplacé, pas recalculé.
                let (dx, dz) = (c[0] - o[0], c[2] - o[2]);
                let mut cells = vec![0; SIDE * SIDE];
                for z in 0..SIDE as i32 {
                    for x in 0..SIDE as i32 {
                        let (ox, oz) = (x + dx, z + dz);
                        if (0..SIDE as i32).contains(&ox) && (0..SIDE as i32).contains(&oz) {
                            cells[(z as usize) * SIDE + x as usize] =
                                self.cells[(oz as usize) * SIDE + ox as usize];
                        }
                    }
                }
                self.cells = cells;
                self.center = Some(c);
            }
            _ => {}
        }
        // Quelques lignes par image, toutes à tour de rôle.
        for _ in 0..ROWS_PER_FRAME {
            let row = self.next_row % SIDE;
            self.row(world, row);
            self.next_row = (self.next_row + 1) % SIDE;
        }
    }

    /// Recalcule la ligne `row` de la grille.
    fn row(&mut self, world: &World, row: usize) {
        let Some(c) = self.center else { return };
        let z = c[2] - GRID + row as i32;
        for dx in 0..SIDE {
            let x = c[0] - GRID + dx as i32;
            self.cells[row * SIDE + dx] = shade(world, x, c[1], z);
        }
    }

    /// Dessine le radar (carré de `size` pixels en `x`, `y`), centré exactement sur le
    /// joueur (il glisse avec lui, sans à-coup). `me` : position, regard et couleur ; `mates` :
    /// coéquipiers en vie (position, couleur) ; `enemies` : adversaires repérés ; `bomb` :
    /// bombe connue.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &self,
        ui: &mut Ui,
        x: f32,
        y: f32,
        size: f32,
        s: f32,
        me: (Vec3, Vec3, [f32; 4]),
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
        let cell = size / (RADIUS * 2) as f32;
        let (cx, cy) = (x + size / 2.0, y + size / 2.0);
        // Coin haut gauche de la case (wx, wz) à l'écran.
        let sx = |wx: f32| cx + (wx - me.0.x) * cell;
        let sy = |wz: f32| cy + (wz - me.0.z) * cell;
        let (x1, y1) = (x + size, y + size);
        // Cases visibles, par bandes de même teinte, coupées au bord du cadre.
        for row in 0..SIDE {
            let wz = (c[2] - GRID + row as i32) as f32;
            let (top, bottom) = (sy(wz).max(y), sy(wz + 1.0).min(y1));
            if bottom <= top {
                continue;
            }
            let mut start = 0;
            while start < SIDE {
                let v = self.cells[row * SIDE + start];
                let mut end = start + 1;
                while end < SIDE && self.cells[row * SIDE + end] == v {
                    end += 1;
                }
                if v != 0 {
                    let wx0 = (c[0] - GRID + start as i32) as f32;
                    let wx1 = (c[0] - GRID + end as i32) as f32;
                    let (left, right) = (sx(wx0).max(x), sx(wx1).min(x1));
                    if right > left {
                        // Un demi-pixel de plus : pas de fente entre les bandes.
                        ui.rect(
                            left,
                            top,
                            (right - left + 0.5).min(x1 - left),
                            (bottom - top + 0.5).min(y1 - top),
                            SHADES[v as usize],
                        );
                    }
                }
                start = end;
            }
        }
        // Position monde → radar (le nord, -z, en haut). Hors du cadre : collé au bord.
        let to = |p: Vec3| -> (f32, f32) { (sx(p.x).clamp(x, x1), sy(p.z).clamp(y, y1)) };
        for (letter, at) in &self.sites {
            let (px, py) = to(Vec3::new(at[0], 0.0, at[1]));
            let t = letter.to_string();
            let tw = Ui::text_width(&t, s);
            ui.text(px - tw / 2.0, py - 4.0 * s, s, [1.0, 0.45, 0.35, 0.95], t);
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
        // Soi : point à sa couleur (cerclé de blanc) au centre, trait dans la direction du
        // regard.
        let dir = Vec3::new(me.1.x, 0.0, me.1.z).normalize_or_zero();
        for k in 1..=4 {
            let f = k as f32 * 1.6 * s;
            ui.rect(
                cx + dir.x * f - s / 2.0,
                cy + dir.z * f - s / 2.0,
                s,
                s,
                [1.0, 1.0, 1.0, 0.9],
            );
        }
        let r = 2.0 * s;
        ui.rect(
            cx - r - s,
            cy - r - s,
            2.0 * r + 2.0 * s,
            2.0 * r + 2.0 * s,
            [1.0; 4],
        );
        ui.rect(cx - r, cy - r, 2.0 * r, 2.0 * r, me.2);
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
        let at = |r: &Radar, dx: i32, dz: i32| {
            r.cells[((dz + GRID) as usize) * SIDE + (dx + GRID) as usize]
        };
        assert_eq!(at(&r, 3, 0), 5);
        assert_eq!(at(&r, 0, 0), 3);
        // Le joueur s'éloigne : la grille est recentrée sur lui, ce qui était connu suit
        // (le mur est toujours au même endroit du monde).
        r.update(&w, Vec3::new(-30.5, 11.0, 0.5));
        assert_eq!(r.center, Some([-31, 11, 0]));
        assert_eq!(at(&r, 34, 0), 5, "le mur, déplacé avec la grille");
    }
}
