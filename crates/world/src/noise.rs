//! Bruit de valeur (value noise) 2D/3D avec interpolation lissée, et fBm.
//! Entièrement déterministe à partir d'une graine.

use crate::lap::wrap_cell;

#[inline]
fn hash(mut x: u32) -> u32 {
    // Mélangeur entier rapide (type « xorshift-multiply »).
    x ^= x >> 16;
    x = x.wrapping_mul(0x7feb_352d);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846c_a68b);
    x ^= x >> 16;
    x
}

/// Hache des coordonnées entières + graine en un entier pseudo-aléatoire.
#[inline]
pub fn hash3(seed: u32, x: i32, y: i32, z: i32) -> u32 {
    let mut h = hash(seed ^ 0x9e37_79b9);
    h = hash(h ^ x as u32);
    h = hash(h ^ (y as u32).wrapping_mul(0x85eb_ca6b));
    hash(h ^ (z as u32).wrapping_mul(0xc2b2_ae35))
}

/// Valeur pseudo-aléatoire dans [0, 1).
#[inline]
pub fn rand01(seed: u32, x: i32, y: i32, z: i32) -> f32 {
    (hash3(seed, x, y, z) >> 8) as f32 / (1u32 << 24) as f32
}

#[inline]
fn smooth(t: f32) -> f32 {
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

#[inline]
fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Bruit 2D dans [-1, 1].
pub fn value2(seed: u32, x: f32, z: f32) -> f32 {
    let (x0, z0) = (x.floor(), z.floor());
    let (tx, tz) = (smooth(x - x0), smooth(z - z0));
    let (ix, iz) = (x0 as i32, z0 as i32);
    let v = |dx, dz| rand01(seed, ix + dx, 0, iz + dz);
    let a = lerp(v(0, 0), v(1, 0), tx);
    let b = lerp(v(0, 1), v(1, 1), tx);
    lerp(a, b, tz) * 2.0 - 1.0
}

/// Bruit 3D dans [-1, 1].
pub fn value3(seed: u32, x: f32, y: f32, z: f32) -> f32 {
    let (x0, y0, z0) = (x.floor(), y.floor(), z.floor());
    let (tx, ty, tz) = (smooth(x - x0), smooth(y - y0), smooth(z - z0));
    let (ix, iy, iz) = (x0 as i32, y0 as i32, z0 as i32);
    let v = |dx, dy, dz| rand01(seed, ix + dx, iy + dy, iz + dz);
    let l0 = lerp(
        lerp(v(0, 0, 0), v(1, 0, 0), tx),
        lerp(v(0, 0, 1), v(1, 0, 1), tx),
        tz,
    );
    let l1 = lerp(
        lerp(v(0, 1, 0), v(1, 1, 0), tx),
        lerp(v(0, 1, 1), v(1, 1, 1), tx),
        tz,
    );
    lerp(l0, l1, ty) * 2.0 - 1.0
}

/// Somme fractale (fBm) de `octaves` couches de bruit 2D, normalisée dans [-1, 1].
pub fn fbm2(seed: u32, x: f32, z: f32, octaves: u32) -> f32 {
    let (mut sum, mut amp, mut freq, mut norm) = (0.0, 1.0, 1.0, 0.0);
    for o in 0..octaves {
        sum += value2(seed.wrapping_add(o * 1013), x * freq, z * freq) * amp;
        norm += amp;
        amp *= 0.5;
        freq *= 2.0;
    }
    sum / norm
}

// (v309) Les mêmes en double précision, pour le relief très loin du centre du monde
// (`gen::FAR`) : en f32, `x / 70` ne vaut plus qu'au 1/8 de case de bruit vers 8 millions de
// blocs, et la colonne elle-même (`wx as f32`) n'est plus exacte au-delà de 16 777 216 ; le
// relief y tombait en marches et en bandes. Mêmes tirages (`rand01`), même lissage.

#[inline]
fn smooth64(t: f64) -> f64 {
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

#[inline]
fn lerp64(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

/// Bruit 2D dans [-1, 1], en double précision.
pub fn value2_64(seed: u32, x: f64, z: f64) -> f64 {
    let (x0, z0) = (x.floor(), z.floor());
    let (tx, tz) = (smooth64(x - x0), smooth64(z - z0));
    let (ix, iz) = (x0 as i32, z0 as i32);
    let v = |dx: i32, dz: i32| rand01(seed, ix.wrapping_add(dx), 0, iz.wrapping_add(dz)) as f64;
    let a = lerp64(v(0, 0), v(1, 0), tx);
    let b = lerp64(v(0, 1), v(1, 1), tx);
    lerp64(a, b, tz) * 2.0 - 1.0
}

/// Bruit 3D dans [-1, 1], en double précision.
pub fn value3_64(seed: u32, x: f64, y: f64, z: f64) -> f64 {
    let (x0, y0, z0) = (x.floor(), y.floor(), z.floor());
    let (tx, ty, tz) = (smooth64(x - x0), smooth64(y - y0), smooth64(z - z0));
    let (ix, iy, iz) = (x0 as i32, y0 as i32, z0 as i32);
    let v = |dx: i32, dy: i32, dz: i32| {
        rand01(seed, ix.wrapping_add(dx), iy.wrapping_add(dy), iz.wrapping_add(dz)) as f64
    };
    let l0 = lerp64(
        lerp64(v(0, 0, 0), v(1, 0, 0), tx),
        lerp64(v(0, 0, 1), v(1, 0, 1), tx),
        tz,
    );
    let l1 = lerp64(
        lerp64(v(0, 1, 0), v(1, 1, 0), tx),
        lerp64(v(0, 1, 1), v(1, 1, 1), tx),
        tz,
    );
    lerp64(l0, l1, ty) * 2.0 - 1.0
}

/// `fbm2` en double précision.
pub fn fbm2_64(seed: u32, x: f64, z: f64, octaves: u32) -> f64 {
    let (mut sum, mut amp, mut freq, mut norm) = (0.0, 1.0, 1.0, 0.0);
    for o in 0..octaves {
        sum += value2_64(seed.wrapping_add(o * 1013), x * freq, z * freq) * amp;
        norm += amp;
        amp *= 0.5;
        freq *= 2.0;
    }
    sum / norm
}

// (v309) Les mêmes sur la carte bouclée (`lap`) : le numéro des cases de bruit boucle,
// `cells` cases par tour (sur x et sur z ; un nombre pair). Dans la carte, les mêmes numéros,
// donc exactement le même bruit que `value2_64`, `value3_64` et `fbm2_64` ; au-delà de la
// limite, celui de l'autre bord : le bruit se répète à chaque tour, sans raccord.

/// `value2_64` sur la carte bouclée.
pub fn value2_lap(seed: u32, x: f64, z: f64, cells: i64) -> f64 {
    let (x0, z0) = (x.floor(), z.floor());
    let (tx, tz) = (smooth64(x - x0), smooth64(z - z0));
    let (ix, iz) = (x0 as i64, z0 as i64);
    let v = |dx: i64, dz: i64| {
        rand01(seed, wrap_cell(ix + dx, cells), 0, wrap_cell(iz + dz, cells)) as f64
    };
    let a = lerp64(v(0, 0), v(1, 0), tx);
    let b = lerp64(v(0, 1), v(1, 1), tx);
    lerp64(a, b, tz) * 2.0 - 1.0
}

/// `value3_64` sur la carte bouclée (x et z bouclent, pas y).
pub fn value3_lap(seed: u32, x: f64, y: f64, z: f64, cells: i64) -> f64 {
    let (x0, y0, z0) = (x.floor(), y.floor(), z.floor());
    let (tx, ty, tz) = (smooth64(x - x0), smooth64(y - y0), smooth64(z - z0));
    let (ix, iy, iz) = (x0 as i64, y0 as i32, z0 as i64);
    let v = |dx: i64, dy: i32, dz: i64| {
        rand01(
            seed,
            wrap_cell(ix + dx, cells),
            iy.wrapping_add(dy),
            wrap_cell(iz + dz, cells),
        ) as f64
    };
    let l0 = lerp64(
        lerp64(v(0, 0, 0), v(1, 0, 0), tx),
        lerp64(v(0, 0, 1), v(1, 0, 1), tx),
        tz,
    );
    let l1 = lerp64(
        lerp64(v(0, 1, 0), v(1, 1, 0), tx),
        lerp64(v(0, 1, 1), v(1, 1, 1), tx),
        tz,
    );
    lerp64(l0, l1, ty) * 2.0 - 1.0
}

/// `fbm2_64` sur la carte bouclée : `cells` cases de la première couche par tour (chaque couche
/// en a deux fois plus que la précédente).
pub fn fbm2_lap(seed: u32, x: f64, z: f64, octaves: u32, cells: i64) -> f64 {
    let (mut sum, mut amp, mut freq, mut norm) = (0.0, 1.0, 1.0, 0.0);
    for o in 0..octaves {
        let n = cells << o;
        sum += value2_lap(seed.wrapping_add(o * 1013), x * freq, z * freq, n) * amp;
        norm += amp;
        amp *= 0.5;
        freq *= 2.0;
    }
    sum / norm
}

#[cfg(test)]
mod tests {
    use super::*;

    /// (v309) Près du centre, la double précision donne le même bruit (à l'arrondi près).
    #[test]
    fn the_double_precision_noise_is_the_same_noise() {
        for i in 0..2000 {
            let x = i as f32 * 0.37 - 300.0;
            let z = i as f32 * 0.11 + 7.0;
            let a = fbm2(42, x, z, 5);
            let b = fbm2_64(42, x as f64, z as f64, 5);
            assert!((a as f64 - b).abs() < 1e-4, "{x} {z} : {a} {b}");
            let c = value3(9, x, z, x * 0.5);
            let d = value3_64(9, x as f64, z as f64, x as f64 * 0.5);
            assert!((c as f64 - d).abs() < 1e-4);
        }
    }

    /// (v309) Dans la carte bouclée, le bruit qui boucle est exactement le même ; passé la
    /// limite, c'est celui de l'autre bord, à l'arrondi près des places.
    #[test]
    fn the_lapping_noise_is_the_same_noise_on_the_map() {
        let cells = 30_678_336i64;
        let half = (cells / 2) as f64;
        for i in 0..3000 {
            let x = -half + 0.5 + i as f64 * 10_227.3;
            let z = (i as f64 * 0.37).sin() * (half - 2.0);
            assert_eq!(fbm2_64(42, x, z, 5), fbm2_lap(42, x, z, 5, cells));
            assert_eq!(value3_64(9, x, 0.25, z), value3_lap(9, x, 0.25, z, cells));
        }
        for i in 0..500 {
            let d = i as f64 * 0.013;
            let a = value2_lap(7, half + d, 3.5, cells);
            let b = value2_lap(7, -half + d, 3.5, cells);
            assert!((a - b).abs() < 1e-6, "{d} : {a} {b}");
        }
    }

    #[test]
    fn deterministic_and_bounded() {
        for i in 0..1000 {
            let x = i as f32 * 0.37 - 100.0;
            let z = i as f32 * 0.11 + 7.0;
            let a = fbm2(42, x, z, 5);
            assert_eq!(a, fbm2(42, x, z, 5));
            assert!((-1.0..=1.0).contains(&a));
            let b = value3(42, x, z, x * 0.5);
            assert!((-1.0..=1.0).contains(&b));
        }
    }
}
