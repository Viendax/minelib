//! (v216) Atmosphère : la couleur du ciel calculée par la diffusion de la lumière dans l'air
//! (Rayleigh : le bleu ; Mie : la brume et l'auréole du soleil ; ozone : le bleu profond du
//! crépuscule), avec la diffusion multiple. Tout se calcule sur le processeur, dans un fil à
//! part (voir `sky`), et arrive à la carte graphique dans une petite image cube : le ciel ne
//! coûte rien à dessiner, le brouillard y lit sa couleur en une lecture.
//!
//! Méthode de Sébastien Hillaire (« A Scalable and Production Ready Sky and Atmosphere
//! Rendering Technique », 2020 ; code de référence sous licence MIT) : une table de la
//! lumière qui traverse l'atmosphère (`Atmosphere::trans`), une table de la diffusion
//! multiple (`Atmosphere::multi`), puis une marche le long du regard. Les correspondances
//! de la table de transmission viennent d'Eric Bruneton (licence BSD).
//!
//! Unités : kilomètres. Les couleurs rendues sont linéaires, par unité de lumière du soleil
//! reçue en haut de l'atmosphère.

use glam::Vec3;

/// Rayon de la planète et du haut de l'atmosphère (km).
const RG: f32 = 6360.0;
const RT: f32 = 6460.0;
/// Diffusion de Rayleigh au niveau de la mer (par km, rouge, vert, bleu) ; hauteur
/// d'échelle 8 km.
const RAYLEIGH: Vec3 = Vec3::new(5.802e-3, 13.558e-3, 33.1e-3);
/// Diffusion et extinction de Mie (par km) ; hauteur d'échelle 1,2 km.
const MIE_SCATTER: f32 = 3.996e-3;
const MIE_EXTINCT: f32 = 4.44e-3;
/// Absorption de l'ozone (par km), en tente autour de 25 km.
const OZONE: Vec3 = Vec3::new(0.650e-3, 1.881e-3, 0.085e-3);
/// Part de la lumière que le sol renvoie.
const GROUND_ALBEDO: f32 = 0.3;
/// Altitude de l'observateur (km) : le monde du jeu tient dans quelques centaines de mètres.
const EYE_KM: f32 = 0.2;

const TRANS_W: usize = 256;
const TRANS_H: usize = 64;
const MULTI_N: usize = 32;

/// Densités de l'air à l'altitude `h` (km) : Rayleigh, Mie, ozone.
fn densities(h: f32) -> (f32, f32, f32) {
    let h = h.max(0.0);
    (
        (-h / 8.0).exp(),
        (-h / 1.2).exp(),
        (1.0 - (h - 25.0).abs() / 15.0).max(0.0),
    )
}

/// Diffusion (ce qui est renvoyé dans toutes les directions) et extinction (ce qui est
/// perdu) à l'altitude `h`, par km : (Rayleigh, Mie, extinction totale).
fn medium(h: f32) -> (Vec3, f32, Vec3) {
    let (r, m, o) = densities(h);
    let ray = RAYLEIGH * r;
    let mie = MIE_SCATTER * m;
    (ray, mie, ray + Vec3::splat(MIE_EXTINCT * m) + OZONE * o)
}

/// Distance jusqu'à la sphère de rayon `radius` depuis le rayon `r` dans la direction de
/// cosinus `mu` avec la verticale (on est dedans : toujours une solution).
fn dist_to_sphere(r: f32, mu: f32, radius: f32) -> f32 {
    let disc = r * r * (mu * mu - 1.0) + radius * radius;
    (-r * mu + disc.max(0.0).sqrt()).max(0.0)
}

/// Le rayon (`r`, `mu`) touche-t-il le sol ?
fn hits_ground(r: f32, mu: f32) -> bool {
    mu < 0.0 && r * r * (mu * mu - 1.0) + RG * RG >= 0.0
}

/// Phase de Rayleigh et phase de Mie (Cornette-Shanks, g = 0,8) pour le cosinus `c` de
/// l'angle entre le regard et la lumière.
fn phases(c: f32) -> (f32, f32) {
    let ray = 3.0 / (16.0 * std::f32::consts::PI) * (1.0 + c * c);
    let g = 0.8f32;
    let k = 3.0 / (8.0 * std::f32::consts::PI) * (1.0 - g * g) / (2.0 + g * g);
    let mie = k * (1.0 + c * c) / (1.0 + g * g - 2.0 * g * c).powf(1.5);
    (ray, mie)
}

/// Les deux tables de l'atmosphère (calculées une fois, en quelques dizaines de ms).
pub struct Atmosphere {
    /// Part de la lumière qui arrive du haut de l'atmosphère jusqu'au point de rayon r dans
    /// la direction mu (correspondance de Bruneton, `TRANS_W` × `TRANS_H`).
    trans: Vec<Vec3>,
    /// Lumière diffusée plusieurs fois, par unité de lumière du soleil, selon la hauteur du
    /// soleil et l'altitude (`MULTI_N`²).
    multi: Vec<Vec3>,
}

impl Atmosphere {
    pub fn new() -> Self {
        let mut a = Self {
            trans: vec![Vec3::ZERO; TRANS_W * TRANS_H],
            multi: vec![Vec3::ZERO; MULTI_N * MULTI_N],
        };
        let h = (RT * RT - RG * RG).sqrt();
        for j in 0..TRANS_H {
            for i in 0..TRANS_W {
                let (u, v) = (
                    i as f32 / (TRANS_W - 1) as f32,
                    j as f32 / (TRANS_H - 1) as f32,
                );
                let rho = h * v;
                let r = (rho * rho + RG * RG).sqrt();
                let (d_min, d_max) = (RT - r, rho + h);
                let d = d_min + u * (d_max - d_min);
                let mu = if d == 0.0 {
                    1.0
                } else {
                    ((h * h - rho * rho - d * d) / (2.0 * r * d)).clamp(-1.0, 1.0)
                };
                a.trans[j * TRANS_W + i] = Self::integrate_transmittance(r, mu);
            }
        }
        a.build_multi();
        a
    }

    /// Transmission du point (`r`, `mu`) jusqu'en haut de l'atmosphère, par intégration.
    fn integrate_transmittance(r: f32, mu: f32) -> Vec3 {
        const STEPS: usize = 40;
        let len = dist_to_sphere(r, mu, RT);
        let dt = len / STEPS as f32;
        let mut depth = Vec3::ZERO;
        for k in 0..STEPS {
            let t = (k as f32 + 0.5) * dt;
            let rr = (r * r + t * t + 2.0 * r * mu * t).sqrt();
            depth += medium(rr - RG).2 * dt;
        }
        Vec3::new((-depth.x).exp(), (-depth.y).exp(), (-depth.z).exp())
    }

    /// Part de la lumière du soleil qui arrive au point de rayon `r` quand le soleil y est
    /// à la hauteur de cosinus `mu` (0 derrière la planète).
    pub fn transmittance(&self, r: f32, mu: f32) -> Vec3 {
        let r = r.clamp(RG + 1e-3, RT);
        if hits_ground(r, mu) {
            return Vec3::ZERO;
        }
        let h = (RT * RT - RG * RG).sqrt();
        let rho = (r * r - RG * RG).max(0.0).sqrt();
        let d = dist_to_sphere(r, mu, RT);
        let (d_min, d_max) = (RT - r, rho + h);
        let u = ((d - d_min) / (d_max - d_min)).clamp(0.0, 1.0);
        let v = (rho / h).clamp(0.0, 1.0);
        bilinear(&self.trans, TRANS_W, TRANS_H, u, v)
    }

    fn build_multi(&mut self) {
        const DIRS: usize = 8;
        const STEPS: usize = 20;
        let iso = 1.0 / (4.0 * std::f32::consts::PI);
        let mut out = vec![Vec3::ZERO; MULTI_N * MULTI_N];
        for j in 0..MULTI_N {
            for i in 0..MULTI_N {
                let mu_s = (i as f32 + 0.5) / MULTI_N as f32 * 2.0 - 1.0;
                let r = RG + 0.01 + (j as f32 + 0.5) / MULTI_N as f32 * (RT - RG - 0.02);
                let sun = Vec3::new((1.0 - mu_s * mu_s).max(0.0).sqrt(), mu_s, 0.0);
                let pos = Vec3::new(0.0, r, 0.0);
                let (mut lum, mut fms) = (Vec3::ZERO, Vec3::ZERO);
                for a in 0..DIRS {
                    for b in 0..DIRS {
                        let phi = std::f32::consts::TAU * (a as f32 + 0.5) / DIRS as f32;
                        let ct = 1.0 - 2.0 * (b as f32 + 0.5) / DIRS as f32;
                        let st = (1.0 - ct * ct).max(0.0).sqrt();
                        let dir = Vec3::new(st * phi.cos(), ct, st * phi.sin());
                        let ground = hits_ground(r, dir.y);
                        let len = if ground {
                            dist_to_sphere_near(r, dir.y, RG)
                        } else {
                            dist_to_sphere(r, dir.y, RT)
                        };
                        let dt = len / STEPS as f32;
                        let mut through = Vec3::ONE;
                        for k in 0..STEPS {
                            let p = pos + dir * ((k as f32 + 0.3) * dt);
                            let pr = p.length();
                            let (ray, mie, ext) = medium(pr - RG);
                            let scat = ray + Vec3::splat(mie);
                            let t_sun = self.transmittance(pr, p.dot(sun) / pr);
                            let step = (-ext * dt).exp();
                            let s = scat * t_sun * iso;
                            lum += through * (s - s * step) / ext;
                            fms += through * (scat - scat * step) / ext;
                            through *= step;
                        }
                        if ground {
                            let p = pos + dir * len;
                            let n = p.normalize();
                            let t_sun = self.transmittance(RG + 1e-3, n.dot(sun));
                            lum += through
                                * t_sun
                                * n.dot(sun).max(0.0)
                                * (GROUND_ALBEDO / std::f32::consts::PI);
                        }
                    }
                }
                let n = (DIRS * DIRS) as f32;
                let (lum, fms) = (lum / n, fms / n);
                out[j * MULTI_N + i] = lum / (Vec3::ONE - fms);
            }
        }
        self.multi = out;
    }

    fn multi_scatter(&self, r: f32, mu_s: f32) -> Vec3 {
        let u = (mu_s * 0.5 + 0.5).clamp(0.0, 1.0);
        let v = ((r - RG) / (RT - RG)).clamp(0.0, 1.0);
        // (Centres des cases : même grille qu'au calcul.)
        let (u, v) = (
            (u * MULTI_N as f32 - 0.5) / (MULTI_N - 1) as f32,
            (v * MULTI_N as f32 - 0.5) / (MULTI_N - 1) as f32,
        );
        bilinear(
            &self.multi,
            MULTI_N,
            MULTI_N,
            u.clamp(0.0, 1.0),
            v.clamp(0.0, 1.0),
        )
    }

    /// Lumière du ciel dans la direction `dir` (unitaire, y vers le haut) pour une source
    /// dans la direction `sun`, par unité de sa lumière en haut de l'atmosphère. Sans le
    /// disque de la source. Sous l'horizon : le sol éclairé, vu à travers l'air.
    pub fn sky(&self, dir: Vec3, sun: Vec3) -> Vec3 {
        const STEPS: usize = 24;
        let r0 = RG + EYE_KM;
        let pos = Vec3::new(0.0, r0, 0.0);
        let ground = hits_ground(r0, dir.y);
        let len = if ground {
            dist_to_sphere_near(r0, dir.y, RG)
        } else {
            dist_to_sphere(r0, dir.y, RT)
        };
        let (p_ray, p_mie) = phases(dir.dot(sun));
        let mut lum = Vec3::ZERO;
        let mut through = Vec3::ONE;
        let mut t0 = 0.0;
        for k in 0..STEPS {
            // Pas plus serrés près de l'œil, où l'air est dense.
            let f = (k as f32 + 1.0) / STEPS as f32;
            let t1 = len * f * f;
            let dt = t1 - t0;
            let p = pos + dir * (t0 + dt * 0.3);
            t0 = t1;
            let pr = p.length();
            let (ray, mie, ext) = medium(pr - RG);
            let mu_s = p.dot(sun) / pr;
            let t_sun = self.transmittance(pr, mu_s);
            let s = (ray * p_ray + Vec3::splat(mie * p_mie)) * t_sun
                + self.multi_scatter(pr, mu_s) * (ray + Vec3::splat(mie));
            let step = (-ext * dt).exp();
            lum += through * (s - s * step) / ext;
            through *= step;
        }
        if ground {
            let p = pos + dir * len;
            let n = p.normalize();
            let t_sun = self.transmittance(RG + 1e-3, n.dot(sun));
            lum += through * t_sun * n.dot(sun).max(0.0) * (GROUND_ALBEDO / std::f32::consts::PI);
        }
        lum
    }

    /// Part de la lumière d'une source dans la direction `sun` qui arrive jusqu'à
    /// l'observateur (sa couleur : blanche en haut du ciel, orangée puis rouge à l'horizon,
    /// nulle derrière la planète).
    pub fn sun_light(&self, sun: Vec3) -> Vec3 {
        self.transmittance(RG + EYE_KM, sun.y)
    }
}

impl Default for Atmosphere {
    fn default() -> Self {
        Self::new()
    }
}

/// Distance jusqu'au premier point de la sphère de rayon `radius`, vue de l'extérieur.
fn dist_to_sphere_near(r: f32, mu: f32, radius: f32) -> f32 {
    let disc = r * r * (mu * mu - 1.0) + radius * radius;
    (-r * mu - disc.max(0.0).sqrt()).max(0.0)
}

fn bilinear(t: &[Vec3], w: usize, h: usize, u: f32, v: f32) -> Vec3 {
    let (x, y) = (u * (w - 1) as f32, v * (h - 1) as f32);
    let (x0, y0) = ((x as usize).min(w - 1), (y as usize).min(h - 1));
    let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
    let (fx, fy) = (x - x0 as f32, y - y0 as f32);
    let a = t[y0 * w + x0].lerp(t[y0 * w + x1], fx);
    let b = t[y1 * w + x0].lerp(t[y1 * w + x1], fx);
    a.lerp(b, fy)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(elev_deg: f32, az_deg: f32) -> Vec3 {
        let (e, a) = (elev_deg.to_radians(), az_deg.to_radians());
        Vec3::new(e.cos() * a.cos(), e.sin(), e.cos() * a.sin())
    }

    /// Le ciel de midi est bleu (plus de bleu que de vert, plus de vert que de rouge), plus
    /// clair à l'horizon qu'au zénith ; au coucher, l'horizon du côté du soleil est orangé
    /// et bien plus rouge que le côté opposé ; la nuit astronomique est noire.
    #[test]
    fn the_sky_is_blue_at_noon_and_orange_at_sunset() {
        let a = Atmosphere::new();
        let noon = dir(70.0, 0.0);
        let zenith = a.sky(Vec3::Y, noon);
        assert!(
            zenith.z > zenith.y * 1.5 && zenith.y > zenith.x * 1.5,
            "{zenith}"
        );
        let horizon = a.sky(dir(2.0, 90.0), noon);
        assert!(
            horizon.dot(Vec3::ONE) > zenith.dot(Vec3::ONE) * 1.5,
            "{horizon} {zenith}"
        );
        // Soleil sur l'horizon.
        let low = dir(1.0, 0.0);
        let toward = a.sky(dir(2.0, 8.0), low);
        let away = a.sky(dir(2.0, 180.0), low);
        assert!(toward.x > toward.z, "vers le soleil : {toward}");
        assert!(toward.x > away.x * 3.0, "{toward} {away}");
        // Sa lumière : blanche en haut, rouge en bas, nulle sous l'horizon.
        let high = a.sun_light(noon);
        assert!(high.x > 0.8 && high.z > 0.6, "{high}");
        let red = a.sun_light(low);
        assert!(red.x > red.z * 4.0 && red.x > 0.1, "{red}");
        assert_eq!(a.sun_light(dir(-10.0, 0.0)), Vec3::ZERO);
        // Soleil 20° sous l'horizon : plus rien.
        let night = a.sky(Vec3::Y, dir(-20.0, 0.0));
        assert!(night.max_element() < 1e-4, "{night}");
        // Crépuscule (soleil 3° sous l'horizon) : encore un peu de lumière au zénith, cent
        // fois moins qu'en plein jour, et une lueur rouge du côté du soleil.
        let below = dir(-3.0, 0.0);
        let dusk = a.sky(Vec3::Y, below);
        assert!(dusk.max_element() > 1e-5, "{dusk}");
        assert!(dusk.z < zenith.z * 0.05, "{dusk} {zenith}");
        let glow = a.sky(dir(3.0, 10.0), below);
        assert!(
            glow.x > glow.z * 3.0 && glow.x > dusk.x * 5.0,
            "{glow} {dusk}"
        );
    }

    /// Tout reste fini et positif, dans toutes les directions et pour tous les soleils.
    #[test]
    fn the_sky_is_finite_everywhere() {
        let a = Atmosphere::new();
        for se in [-30.0, -6.0, -1.0, 0.0, 0.5, 5.0, 45.0, 90.0] {
            for e in [-90.0, -30.0, -1.0, 0.0, 0.3, 10.0, 60.0, 90.0] {
                for az in [0.0, 37.0, 90.0, 180.0] {
                    let c = a.sky(dir(e, az), dir(se, 0.0));
                    assert!(
                        c.is_finite() && c.min_element() >= 0.0,
                        "soleil {se}°, regard {e}° / {az}° : {c}"
                    );
                }
            }
        }
    }
}
