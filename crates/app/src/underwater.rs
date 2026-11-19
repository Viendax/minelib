//! (v194) Hauteur de l'eau au-dessus des yeux : le rendu bleute ce que la caméra voit sous la
//! surface de l'eau, seulement sous la ligne de l'eau (voir `underwater.wgsl` dans le rendu).
//! (v200) Et l'eau qu'on voit d'au-dessus, où se reflètent les joueurs (`SeenWater`).

use glam::{Mat4, Vec3};
use minelib_world::registry::Fluid;
use minelib_world::World;

/// Surface de l'eau de la colonne de la caméra `eye`, quand la caméra est sous l'eau ou
/// tout près au-dessus (son plan proche, à 0,05 bloc, peut passer dessous : la ligne de l'eau
/// traverse alors l'image) ; `None` hors de l'eau.
pub fn eye_water_level(world: &World, eye: Vec3) -> Option<f32> {
    let (x, z) = (eye.x.floor() as i32, eye.z.floor() as i32);
    let y0 = eye.y.floor() as i32;
    let water = |y: i32| match crate::fluids::surface(world, [x, y, z]) {
        Some((Fluid::Water, h)) => Some(h),
        _ => None,
    };
    // La case des yeux, sinon celle du dessous (yeux juste au-dessus d'une eau basse).
    let mut y = [y0, y0 - 1].into_iter().find(|&y| water(y).is_some())?;
    let mut h = water(y)?;
    // Haut de la colonne d'eau.
    for _ in 0..256 {
        match water(y + 1) {
            Some(h2) => {
                y += 1;
                h = h2;
            }
            None => break,
        }
    }
    let level = y as f32 + h;
    (level > eye.y - 0.1).then_some(level)
}

/// Surface de la colonne d'eau qui contient la case `c` (le haut de la dernière case d'eau
/// au-dessus), si `c` est de l'eau.
fn column_surface(world: &World, c: [i32; 3]) -> Option<f32> {
    let water = |y: i32| match crate::fluids::surface(world, [c[0], y, c[2]]) {
        Some((Fluid::Water, h)) => Some(h),
        _ => None,
    };
    let mut y = c[1];
    let mut h = water(y)?;
    for _ in 0..256 {
        match water(y + 1) {
            Some(h2) => {
                y += 1;
                h = h2;
            }
            None => break,
        }
    }
    Some(y as f32 + h)
}

/// Portée des rayons qui cherchent l'eau en vue (blocs) : celle des reflets.
pub const SEEN_RANGE: f32 = 64.0;

/// Première eau le long du rayon `o` + t `d` (unitaire), avant un bloc plein, à moins de
/// `SEEN_RANGE` blocs : la surface de sa colonne.
fn water_on_ray(world: &World, o: Vec3, d: Vec3) -> Option<f32> {
    let mut cell = [o.x.floor() as i32, o.y.floor() as i32, o.z.floor() as i32];
    let (o, d) = (o.to_array(), d.to_array());
    let step: [i32; 3] = std::array::from_fn(|i| if d[i] > 0.0 { 1 } else { -1 });
    let delta: [f32; 3] = std::array::from_fn(|i| {
        if d[i] != 0.0 {
            1.0 / d[i].abs()
        } else {
            f32::INFINITY
        }
    });
    let mut next: [f32; 3] = std::array::from_fn(|i| {
        if d[i] == 0.0 {
            f32::INFINITY
        } else if d[i] > 0.0 {
            (cell[i] as f32 + 1.0 - o[i]) / d[i]
        } else {
            (cell[i] as f32 - o[i]) / d[i]
        }
    });
    let mut t = 0.0;
    while t <= SEEN_RANGE {
        let id = world.get(cell[0], cell[1], cell[2]);
        if matches!(minelib_world::registry::fluid(id), Some((Fluid::Water, _))) {
            return column_surface(world, cell);
        }
        if id.is_opaque() {
            return None;
        }
        let axis = if next[0] < next[1] {
            if next[0] < next[2] {
                0
            } else {
                2
            }
        } else if next[1] < next[2] {
            1
        } else {
            2
        };
        t = next[axis];
        next[axis] += delta[axis];
        cell[axis] += step[axis];
    }
    None
}

/// (v200) L'eau que la caméra voit d'au-dessus, où se reflètent les joueurs et les objets
/// (essai, `water_mirror` dans le rendu) : une seule surface par image, celle que touchent
/// le plus de rayons lancés sur une grille de l'image. (v202) Une ligne de la grille par
/// image (le coût reste faible et égal d'une image à l'autre), les autres gardées de leur
/// dernier passage.
#[derive(Default)]
pub struct SeenWater {
    /// Surfaces touchées par les rayons de chaque ligne (en neuvièmes de bloc).
    rows: Vec<Vec<i32>>,
    next: u32,
    level: Option<f32>,
}

impl SeenWater {
    /// Surface à refléter pour la caméra `eye` (vue `view`, champ vertical `fov_y` en
    /// radians, rapport largeur / hauteur `aspect`).
    pub fn update(
        &mut self,
        world: &World,
        eye: Vec3,
        view: Mat4,
        fov_y: f32,
        aspect: f32,
    ) -> Option<f32> {
        if self.rows.len() != GRID.1 as usize {
            self.rows = vec![Vec::new(); GRID.1 as usize];
        }
        let j = self.next % GRID.1;
        self.next = self.next.wrapping_add(1);
        let row = &mut self.rows[j as usize];
        row.clear();
        row.extend(row_levels(world, eye, view, fov_y, aspect, j));
        self.level = choose_level(self.rows.iter().flatten().copied(), self.level, eye);
        self.level
    }
}

/// Grille de rayons : colonnes et lignes de l'image.
const GRID: (u32, u32) = (16, 9);

/// Surfaces d'eau (en neuvièmes de bloc) que touchent les rayons de la ligne `j` de la
/// grille, vues d'au-dessus.
fn row_levels(
    world: &World,
    eye: Vec3,
    view: Mat4,
    fov_y: f32,
    aspect: f32,
    j: u32,
) -> impl Iterator<Item = i32> + '_ {
    // Repère de la caméra : lignes de la vue (droite, haut, arrière).
    let right = view.row(0).truncate();
    let up = view.row(1).truncate();
    let forward = -view.row(2).truncate();
    let ty = (fov_y * 0.5).tan();
    let tx = ty * aspect;
    (0..GRID.0).filter_map(move |i| {
        let x = ((i as f32 + 0.5) / GRID.0 as f32) * 2.0 - 1.0;
        let y = ((j as f32 + 0.5) / GRID.1 as f32) * 2.0 - 1.0;
        let d = (forward + right * (x * tx) + up * (y * ty)).normalize();
        water_on_ray(world, eye, d)
            .filter(|&h| eye.y > h + 0.05)
            .map(|h| (h * 9.0).round() as i32)
    })
}

/// Case (hauteur entière) d'une surface d'eau en neuvièmes de bloc : celle du bloc d'eau.
fn level_cell(key: i32) -> i32 {
    (key - 1).div_euclid(9)
}

/// Surface à refléter parmi celles touchées (`keys`, en neuvièmes de bloc) : (v202) la
/// nappe d'eau (même case) la plus touchée, et dans celle-ci la hauteur la plus touchée.
/// Une eau qui s'étale a toutes les hauteurs (1/9 à 8/9) dans la même case : le rendu
/// prend toute la case (`water_mirror`). `prev`, celle d'avant, est gardée tant que sa
/// nappe et sa hauteur ont encore au moins la moitié des rayons des plus touchées.
fn choose_level(keys: impl Iterator<Item = i32>, prev: Option<f32>, eye: Vec3) -> Option<f32> {
    // (Hauteur, nombre de rayons.)
    let mut votes: Vec<(i32, u32)> = Vec::new();
    for key in keys.filter(|&k| eye.y > k as f32 / 9.0 + 0.05) {
        match votes.iter_mut().find(|v| v.0 == key) {
            Some(v) => v.1 += 1,
            None => votes.push((key, 1)),
        }
    }
    let in_cell = |cell: i32| votes.iter().filter(move |v| level_cell(v.0) == cell);
    let cell_votes = |cell: i32| in_cell(cell).map(|v| v.1).sum::<u32>();
    let best_cell = votes
        .iter()
        .map(|v| level_cell(v.0))
        .max_by_key(|&c| (cell_votes(c), c))?;
    let best_key = in_cell(best_cell).max_by_key(|v| (v.1, v.0))?;
    let keep = prev.and_then(|p| {
        let key = (p * 9.0).round() as i32;
        let cell = level_cell(key);
        let own = votes.iter().find(|v| v.0 == key)?.1;
        let strong = cell_votes(cell) * 2 >= cell_votes(best_cell)
            && in_cell(cell)
                .map(|v| v.1)
                .max()
                .is_some_and(|m| own * 2 >= m);
        strong.then_some(p)
    });
    keep.or(Some(best_key.0 as f32 / 9.0))
}

/// Surface d'eau vue d'au-dessus que touchent le plus de rayons de toute la grille (voir
/// `SeenWater`, `choose_level`), `prev` gardée si elle en a encore assez (essais).
#[cfg(test)]
fn seen_water_level(
    world: &World,
    eye: Vec3,
    view: Mat4,
    fov_y: f32,
    aspect: f32,
    prev: Option<f32>,
) -> Option<f32> {
    let keys: Vec<i32> = (0..GRID.1)
        .flat_map(|j| row_levels(world, eye, view, fov_y, aspect, j))
        .collect();
    choose_level(keys.into_iter(), prev, eye)
}

#[cfg(test)]
mod tests {
    use super::*;
    use minelib_world::{BlockId, ChunkPos};

    #[test]
    fn the_water_level_is_the_top_of_the_column() {
        let mut w = World::new(0);
        w.insert_empty(ChunkPos { x: 0, z: 0 });
        // Trois blocs d'eau de haut (10 à 12) : la surface est aux 8/9 du dernier.
        for y in 10..13 {
            w.set(4, y, 4, BlockId::WATER);
        }
        let top = 12.0 + 8.0 / 9.0;
        let at = |y: f32| eye_water_level(&w, Vec3::new(4.5, y, 4.5));
        // Yeux au fond, au milieu, juste sous la surface : la même surface.
        for y in [10.2, 11.5, top - 0.01] {
            assert!((at(y).unwrap() - top).abs() < 1e-5, "{y}");
        }
        // Juste au-dessus (le plan proche peut toucher l'eau) : encore là.
        assert!(at(top + 0.03).is_some());
        // Nettement au-dessus, ou à côté de l'eau : rien.
        assert!(at(top + 0.5).is_none());
        assert!(eye_water_level(&w, Vec3::new(6.5, 11.5, 4.5)).is_none());
    }

    /// Une piscine vue d'au-dessus : sa surface est l'eau à refléter. De dessous (yeux
    /// sous la surface), ou en regardant ailleurs, rien.
    #[test]
    fn the_seen_water_is_the_pool_below_the_camera() {
        let mut w = World::new(0);
        w.insert_empty(ChunkPos { x: 0, z: 0 });
        for x in 2..14 {
            for z in 2..14 {
                w.set(x, 9, z, BlockId::STONE);
                w.set(x, 10, z, BlockId::WATER);
                w.set(x, 11, z, BlockId::WATER);
            }
        }
        let top = 11.0 + 8.0 / 9.0;
        let look = |eye: Vec3, at: Vec3| Mat4::look_at_rh(eye, at, Vec3::Y);
        let fov = 70f32.to_radians();
        let eye = Vec3::new(8.0, 14.0, 8.0);
        let down = look(eye, Vec3::new(8.0, 0.0, 9.0));
        let h = seen_water_level(&w, eye, down, fov, 16.0 / 9.0, None).expect("eau en vue");
        assert!((h - top).abs() < 1e-4, "{h}");
        // Vers le ciel : pas d'eau en vue.
        let sky = look(eye, Vec3::new(8.0, 30.0, 9.0));
        assert!(seen_water_level(&w, eye, sky, fov, 16.0 / 9.0, None).is_none());
        // Sous la surface : pas de reflet vu d'au-dessus.
        let under = Vec3::new(8.0, 11.2, 8.0);
        let v = look(under, Vec3::new(8.0, 0.0, 9.0));
        assert!(seen_water_level(&w, under, v, fov, 16.0 / 9.0, None).is_none());
        // Une ligne de la grille par image ; oubliée dès qu'on passe dessous.
        let mut seen = SeenWater::default();
        let mut got = None;
        for _ in 0..GRID.1 {
            got = seen.update(&w, eye, down, fov, 16.0 / 9.0);
        }
        assert_eq!(got, Some(h));
        assert_eq!(seen.update(&w, under, down, fov, 16.0 / 9.0), None);
    }

    /// (v202) Une eau qui s'étale (hauteurs de 1/9 à 8/9 dans la même case) : c'est sa case
    /// qui compte, plus touchée qu'une piscine à côté, et la hauteur la plus vue.
    #[test]
    fn spreading_water_is_one_sheet() {
        let eye = Vec3::new(0.0, 140.0, 0.0);
        let k = |h: f32| (h * 9.0).round() as i32;
        // Flaque de la case 131 à hauteurs mêlées (6 rayons), piscine de la case 120 (4).
        let keys = [
            131.0 + 1.0 / 9.0,
            131.0 + 1.0 / 9.0,
            131.0 + 4.0 / 9.0,
            131.0 + 5.0 / 9.0,
            131.0 + 1.0 / 9.0,
            131.0 + 8.0 / 9.0,
            120.0 + 8.0 / 9.0,
            120.0 + 8.0 / 9.0,
            120.0 + 8.0 / 9.0,
            120.0 + 8.0 / 9.0,
        ];
        let got = choose_level(keys.iter().map(|&h| k(h)), None, eye).expect("eau");
        assert!((got - (131.0 + 1.0 / 9.0)).abs() < 1e-4, "{got}");
        assert_eq!(level_cell(k(got)), 131);
        assert_eq!(level_cell(k(131.0 + 8.0 / 9.0)), 131);
        assert_eq!(level_cell(k(132.0)), 131);
        // Celle d'avant reste tant qu'elle est encore bien vue.
        let prev = Some(131.0 + 4.0 / 9.0);
        let more = [131.0 + 4.0 / 9.0, 131.0 + 1.0 / 9.0, 131.0 + 1.0 / 9.0];
        let kept = choose_level(more.iter().map(|&h| k(h)), prev, eye);
        assert_eq!(kept, prev);
    }
}
