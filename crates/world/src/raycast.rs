//! Lancer de rayon dans la grille de voxels (algorithme DDA d'Amanatides & Woo).

use crate::{BlockId, World};

#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Hit {
    /// Bloc touché.
    pub block: [i32; 3],
    /// Normale de la face touchée (la case vide devant est `block + normal`).
    pub normal: [i32; 3],
    pub id: BlockId,
    pub distance: f32,
}

impl Hit {
    pub fn adjacent(&self) -> [i32; 3] {
        [self.block[0] + self.normal[0], self.block[1] + self.normal[1], self.block[2] + self.normal[2]]
    }
}

/// Premier bloc solide le long du rayon, à au plus `max_dist` blocs.
pub fn raycast(world: &World, origin: [f32; 3], dir: [f32; 3], max_dist: f32) -> Option<Hit> {
    let len = (dir[0] * dir[0] + dir[1] * dir[1] + dir[2] * dir[2]).sqrt();
    if len < 1e-6 {
        return None;
    }
    let d = dir.map(|c| c / len);
    let mut cell = origin.map(|c| c.floor() as i32);
    let step = d.map(|c| if c > 0.0 { 1 } else { -1 });
    let mut t_max = [0.0f32; 3];
    let mut t_delta = [f32::INFINITY; 3];
    for i in 0..3 {
        if d[i] != 0.0 {
            t_delta[i] = 1.0 / d[i].abs();
            let boundary = if d[i] > 0.0 { cell[i] as f32 + 1.0 } else { cell[i] as f32 };
            t_max[i] = (boundary - origin[i]) / d[i];
        } else {
            t_max[i] = f32::INFINITY;
        }
    }
    let mut normal = [0; 3];
    let mut t = 0.0;
    while t <= max_dist {
        let id = world.get(cell[0], cell[1], cell[2]);
        if id.is_targetable() && cell[1] >= 0 {
            let def = id.def();
            if def.full {
                return Some(Hit { block: cell, normal, id, distance: t });
            }
            // Bloc partiel (torche, dalle, clôture…) : il faut toucher sa vraie forme.
            if let Some((dist, n)) = hit_boxes(origin, d, cell, &def.boxes()) {
                if dist <= max_dist {
                    return Some(Hit { block: cell, normal: n, id, distance: dist });
                }
            }
        }
        let axis = if t_max[0] < t_max[1] {
            if t_max[0] < t_max[2] { 0 } else { 2 }
        } else if t_max[1] < t_max[2] {
            1
        } else {
            2
        };
        t = t_max[axis];
        t_max[axis] += t_delta[axis];
        cell[axis] += step[axis];
        normal = [0; 3];
        normal[axis] = -step[axis];
    }
    None
}

/// Boîte (coins min, max, en coordonnées monde) visée pour un bloc : l'union de ses boîtes.
pub fn selection_box(world: &World, block: [i32; 3]) -> ([f32; 3], [f32; 3]) {
    let boxes = world.get(block[0], block[1], block[2]).def().boxes();
    let (mut lo, mut hi) = ([16u8; 3], [0u8; 3]);
    for (a, b) in &boxes {
        for k in 0..3 {
            lo[k] = lo[k].min(a[k]);
            hi[k] = hi[k].max(b[k]);
        }
    }
    if boxes.is_empty() {
        (lo, hi) = ([0; 3], [16; 3]);
    }
    let o = block.map(|v| v as f32);
    (
        [o[0] + lo[0] as f32 / 16.0, o[1] + lo[1] as f32 / 16.0, o[2] + lo[2] as f32 / 16.0],
        [o[0] + hi[0] as f32 / 16.0, o[1] + hi[1] as f32 / 16.0, o[2] + hi[2] as f32 / 16.0],
    )
}

/// Premier contact du rayon avec l'une des boîtes (en seizièmes de bloc) d'une case.
fn hit_boxes(origin: [f32; 3], d: [f32; 3], cell: [i32; 3], boxes: &[([u8; 3], [u8; 3])]) -> Option<(f32, [i32; 3])> {
    let mut best: Option<(f32, [i32; 3])> = None;
    for (a, b) in boxes {
        let (mut t0, mut t1) = (0.0f32, f32::INFINITY);
        let mut n = [0; 3];
        let mut ok = true;
        for k in 0..3 {
            let lo = cell[k] as f32 + a[k] as f32 / 16.0;
            let hi = cell[k] as f32 + b[k] as f32 / 16.0;
            if d[k].abs() < 1e-8 {
                if origin[k] < lo || origin[k] > hi {
                    ok = false;
                    break;
                }
                continue;
            }
            let (mut ta, mut tb) = ((lo - origin[k]) / d[k], (hi - origin[k]) / d[k]);
            if ta > tb {
                std::mem::swap(&mut ta, &mut tb);
            }
            if ta > t0 {
                t0 = ta;
                n = [0; 3];
                n[k] = if d[k] > 0.0 { -1 } else { 1 };
            }
            t1 = t1.min(tb);
            if t0 > t1 {
                ok = false;
                break;
            }
        }
        if ok && best.is_none_or(|(bt, _)| t0 < bt) {
            best = Some((t0, n));
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ChunkPos;

    #[test]
    fn torch_is_hit_only_on_its_shape() {
        let mut w = World::new(0);
        w.insert_empty(ChunkPos { x: 0, z: 0 });
        w.set(5, 100, 2, BlockId::TORCH);
        w.set(7, 100, 2, BlockId::STONE);
        // À travers le haut de la case (au-dessus de la torche, 13 px) : on touche la pierre.
        let h = raycast(&w, [1.5, 100.9, 2.5], [1.0, 0.0, 0.0], 10.0).unwrap();
        assert_eq!(h.block, [7, 100, 2]);
        // Au milieu : la torche, sur sa face à 6/16.
        let h = raycast(&w, [1.5, 100.3, 2.5], [1.0, 0.0, 0.0], 10.0).unwrap();
        assert_eq!((h.block, h.normal), ([5, 100, 2], [-1, 0, 0]));
        assert!((h.distance - 3.875).abs() < 1e-4, "{}", h.distance);
        let (lo, hi) = selection_box(&w, [5, 100, 2]);
        assert_eq!((lo, hi), ([5.375, 100.0, 2.375], [5.625, 100.8125, 2.625]));
    }

    #[test]
    fn hits_block_and_reports_face() {
        let mut w = World::new(0);
        w.insert_empty(ChunkPos { x: 0, z: 0 });
        w.set(5, 100, 2, BlockId::STONE);
        let h = raycast(&w, [1.5, 100.5, 2.5], [1.0, 0.0, 0.0], 10.0).unwrap();
        assert_eq!(h.block, [5, 100, 2]);
        assert_eq!(h.normal, [-1, 0, 0]);
        assert_eq!(h.adjacent(), [4, 100, 2]);
        assert!((h.distance - 3.5).abs() < 1e-4);
        assert!(raycast(&w, [1.5, 100.5, 2.5], [1.0, 0.0, 0.0], 3.0).is_none());
        let down = raycast(&w, [5.5, 105.0, 2.5], [0.0, -1.0, 0.0], 10.0).unwrap();
        assert_eq!(down.normal, [0, 1, 0]);
    }
}
