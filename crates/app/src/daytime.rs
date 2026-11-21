//! Heure du jour et soleil.
//!
//! - Construction : cycle de Minecraft, une journée en 20 minutes (le soleil se lève à
//!   l'est à 6 h, passe au zénith à midi, se couche à l'ouest à 18 h). `/timeset` règle
//!   l'heure, `/timefix` l'arrête.
//! - Cartes FPS : heure fixe (`/fps set time`) ; par défaut le soleil de Dust2 dans CS2.
//!
//! Heures écrites `11`, `11.20`, `11:20` ou `11h20` (11 h 20).

/// Durée d'une journée complète (s), comme dans Minecraft.
pub const DAY_S: f32 = 1200.0;

/// Heure « par défaut » (`/timeset default`) : le soleil de Dust2, immobile.
pub const DUST2: f32 = -1.0;

/// Heure d'un monde neuf.
pub const START_HOUR: f32 = 8.0;

/// Direction vers le soleil de Dust2 dans CS2 (son `light_environment` : inclinaison
/// 54,18°, lacet 87,80° ; lumière qui va vers +Y de Source, le nord), dans le repère du
/// jeu (+X est, +Y haut, +Z sud) : haut dans le ciel, presque plein sud.
pub fn dust2_sun() -> [f32; 3] {
    let (pitch, yaw) = (54.179_23_f32.to_radians(), 87.803_26_f32.to_radians());
    // Sens de la lumière dans Source (x est, y nord, z haut), puis vers le soleil.
    let (fx, fy, fz) = (
        pitch.cos() * yaw.cos(),
        pitch.cos() * yaw.sin(),
        -pitch.sin(),
    );
    let to_sun = [-fx, -fz, fy];
    let l = (to_sun[0].powi(2) + to_sun[1].powi(2) + to_sun[2].powi(2)).sqrt();
    to_sun.map(|v| v / l)
}

/// Direction vers le soleil à l'heure `h` (cycle de Minecraft, d'est en ouest).
pub fn sun_dir(h: f32) -> [f32; 3] {
    let a = (h - 6.0) / 24.0 * std::f32::consts::TAU;
    // Un léger penchant vers le sud : les faces nord et sud ne restent pas toujours
    // dans la même ombre.
    let (x, y, z) = (a.cos(), a.sin(), 0.25);
    let l = (x * x + y * y + z * z).sqrt();
    [x / l, y / l, z / l]
}

/// (v216) Jours d'un cycle de la lune, comme dans Minecraft.
pub const MOON_DAYS: f32 = 8.0;

/// (v216) Direction vers la lune à l'heure `h`, `age` jours après la pleine lune. Elle suit
/// le chemin du soleil avec un retard qui grandit d'un tour par cycle : pleine lune à
/// l'opposé du soleil (elle se lève quand il se couche), puis chaque nuit plus tard,
/// nouvelle lune près du soleil au bout de quatre jours. Ses phases viennent de là : le
/// rendu l'éclaire avec le vrai soleil.
pub fn moon_dir(h: f32, age: f32) -> [f32; 3] {
    use std::f32::consts::{PI, TAU};
    let a = (h - 6.0) / 24.0 * TAU - PI - age / MOON_DAYS * TAU;
    // (Pas tout à fait le chemin du soleil : pas d'éclipse à chaque nouvelle lune.)
    let (x, y, z) = (a.cos(), a.sin(), 0.08);
    let l = (x * x + y * y + z * z).sqrt();
    [x / l, y / l, z / l]
}

/// Lune d'un soleil immobile (cartes FPS, soleil de Dust2) : à l'opposé.
pub fn moon_opposite(sun: [f32; 3]) -> [f32; 3] {
    [-sun[0], -sun[1], sun[2]]
}

/// Lumière du jour (0 nuit … 1 plein jour) selon la hauteur du soleil.
pub fn daylight(h: f32) -> f32 {
    let e = ((h - 6.0) / 24.0 * std::f32::consts::TAU).sin();
    let t = ((e + 0.1) / 0.35).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Heure qui avance de `dt` secondes.
pub fn advance(h: f32, dt: f32) -> f32 {
    (h + dt * 24.0 / DAY_S).rem_euclid(24.0)
}

/// Lit une heure : `11`, `11.20`, `11:20`, `11h20`, `11h` (minutes 0..59).
pub fn parse(text: &str) -> Option<f32> {
    let t = text.trim().to_lowercase();
    let (h, m) = match t.find([':', '.', 'h']) {
        Some(i) => (&t[..i], &t[i + 1..]),
        None => (t.as_str(), ""),
    };
    let h: u32 = h.trim().parse().ok()?;
    let m: u32 = if m.trim().is_empty() {
        0
    } else {
        m.trim().parse().ok()?
    };
    (h < 24 && m < 60).then_some(h as f32 + m as f32 / 60.0)
}

/// `11:20`.
pub fn format(h: f32) -> String {
    if h < 0.0 {
        return "soleil de Dust2".into();
    }
    let total = (h.rem_euclid(24.0) * 60.0).round() as u32 % (24 * 60);
    format!("{}:{:02}", total / 60, total % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_times_and_moves_the_sun_like_minecraft() {
        assert_eq!(parse("11"), Some(11.0));
        assert_eq!(parse("11.20"), Some(11.0 + 20.0 / 60.0));
        assert_eq!(parse("11:20"), parse("11h20"));
        assert_eq!(parse("7h"), Some(7.0));
        assert_eq!(parse("24"), None);
        assert_eq!(parse("10:75"), None);
        assert_eq!(format(11.0 + 20.0 / 60.0), "11:20");
        // Lever à l'est, midi en haut, coucher à l'ouest, nuit noire à minuit.
        assert!(sun_dir(6.0)[0] > 0.9 && sun_dir(18.0)[0] < -0.9);
        assert!(sun_dir(12.0)[1] > 0.9);
        assert_eq!(daylight(12.0), 1.0);
        assert_eq!(daylight(0.0), 0.0);
        // Une journée en 20 minutes.
        assert!((advance(23.0, DAY_S / 24.0) - 0.0).abs() < 1e-4);
        // Pleine lune : à l'opposé du soleil ; quatre jours plus tard, nouvelle lune, du même
        // côté que lui ; à mi-chemin, un quart de tour derrière.
        let dot = |a: [f32; 3], b: [f32; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
        assert!(dot(sun_dir(0.0), moon_dir(0.0, 0.0)) < -0.9);
        assert!(moon_dir(0.0, 0.0)[1] > 0.9, "pleine lune haute à minuit");
        assert!(dot(sun_dir(15.0), moon_dir(15.0, 4.0)) > 0.9);
        assert!(dot(sun_dir(9.0), moon_dir(9.0, 2.0)).abs() < 0.2);
        assert!(dot(moon_dir(3.0, 1.0), moon_dir(3.0, 9.0)) > 0.999);
        // Soleil de Dust2 : haut (≈ 54°) et vers le sud.
        let s = dust2_sun();
        assert!((s[1] - 54.18_f32.to_radians().sin()).abs() < 1e-3);
        assert!(s[2] > 0.5 && s[0].abs() < 0.05, "{s:?}");
    }
}
