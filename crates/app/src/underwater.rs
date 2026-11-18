//! (v194) Hauteur de l'eau au-dessus des yeux : le rendu bleute ce que la caméra voit sous la
//! surface de l'eau, seulement sous la ligne de l'eau (voir `underwater.wgsl` dans le rendu).

use glam::Vec3;
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
}
