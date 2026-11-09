//! Bruit de valeur (value noise) 2D/3D avec interpolation lissée, et fBm.
//! Entièrement déterministe à partir d'une graine.

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
    let l0 = lerp(lerp(v(0, 0, 0), v(1, 0, 0), tx), lerp(v(0, 0, 1), v(1, 0, 1), tx), tz);
    let l1 = lerp(lerp(v(0, 1, 0), v(1, 1, 0), tx), lerp(v(0, 1, 1), v(1, 1, 1), tx), tz);
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

#[cfg(test)]
mod tests {
    use super::*;

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
