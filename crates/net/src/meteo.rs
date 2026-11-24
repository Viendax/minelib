//! (v244) Horloges de la météo, communes au jeu et au relais.
//!
//! La météo d'un monde est une fonction de sa graine et d'une horloge (voir
//! `app::weather`) : tout le monde y lit le même temps au même moment, sans rien s'échanger.
//! Les éclairs qui courent dans les nuages au-dessus des zones en sont tirés aussi, les mêmes
//! pour tous. (v282) Pendant l'orage, la Foudre de Zeus de celui qui la porte gagne une
//! charge toutes les `CHARGE_EVERY_S` secondes de cette horloge (avant : quand un de ces
//! éclairs passait au-dessus de lui) ; en ligne, c'est le relais qui accorde ces charges
//! (principe du projet : il valide tout), aux mêmes instants (`charge_marks`).

/// Durées de Minecraft, en secondes (tics / 20) : averse, temps sec, tonnerre, sans
/// tonnerre.
pub const RAIN_ON_S: (f64, f64) = (600.0, 1200.0);
pub const RAIN_OFF_S: (f64, f64) = (600.0, 9000.0);
pub const THUNDER_ON_S: (f64, f64) = (180.0, 780.0);
pub const THUNDER_OFF_S: (f64, f64) = (600.0, 9000.0);

/// En ligne, l'horloge de la météo compte depuis le 1er janvier 2026 (UTC).
pub const ONLINE_EPOCH_S: f64 = 1_767_225_600.0;

/// Éclairs qui courent dans les nuages au-dessus d'une zone pendant l'orage : tirés de la
/// graine et de l'horloge, par zone de `CLOUD_ZONE` blocs de côté et par créneau de
/// `CLOUD_SLOT_S` secondes. Les mêmes pour tous, **quel que soit le nombre de joueurs dans la
/// zone**.
pub const CLOUD_ZONE: i32 = 256;
pub const CLOUD_SLOT_S: f64 = 15.0;
/// Chance par zone et par créneau, orage installé : un toutes les deux minutes en moyenne
/// au-dessus d'une zone (« ça doit quand même arriver un peu »).
pub const CLOUD_P: f64 = 1.0 / 8.0;
/// Autour de celui qui porte la Foudre de Zeus (à moins de `CHARGE_REACH` blocs de lui) :
/// trois fois plus souvent.
pub const CLOUD_BOOST: f64 = 3.0;
/// Autour de celui qui porte la Foudre de Zeus (à moins de tant de blocs, à plat), les
/// éclairs des nuages sont plus fréquents (`CLOUD_BOOST`). (Avant la v282, ils la
/// chargeaient.)
pub const CHARGE_REACH: f32 = 160.0;
/// (v282) « Quand il y a la foudre, au lieu que les éclairs dans le ciel chargent l'éclair,
/// charge-le d'autant qu'un éclair naturel toutes les 10 secondes » : pendant l'orage, celui
/// qui porte la Foudre de Zeus gagne une charge à chaque multiple de tant de secondes de
/// l'horloge de la météo (les mêmes instants pour son jeu et pour le relais).
pub const CHARGE_EVERY_S: f64 = 10.0;

/// (v282) Instants de charge (multiples de `CHARGE_EVERY_S`) entre `from` (exclu) et `to`
/// (inclus) de l'horloge de la météo.
pub fn charge_marks(from: f64, to: f64) -> impl Iterator<Item = f64> {
    let first = (from / CHARGE_EVERY_S).floor() as i64 + 1;
    let last = (to / CHARGE_EVERY_S).floor() as i64;
    (first..=last).map(|k| k as f64 * CHARGE_EVERY_S)
}

pub fn mix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

/// Nombre de 0 à 1 (exclu) tiré de `seed` et de deux rangs.
pub fn unit(seed: u64, a: u64, b: u64) -> f64 {
    let h = mix64(mix64(seed ^ a.wrapping_mul(0x9e37_79b9_7f4a_7c15)) ^ b);
    (h >> 11) as f64 / (1u64 << 53) as f64
}

/// Une des deux horloges : des périodes « sans » puis « avec », l'une après l'autre,
/// chacune d'une durée tirée de la graine. Elle garde où elle en est : la suivre d'une
/// image à l'autre ne coûte rien.
#[derive(Clone, Debug)]
pub struct Cycle {
    seed: u64,
    on: (f64, f64),
    off: (f64, f64),
    /// Rang de la période en cours (paire : sans, impaire : avec) et son début.
    k: u64,
    start: f64,
}

impl Cycle {
    pub fn new(seed: u64, on: (f64, f64), off: (f64, f64)) -> Self {
        Self {
            seed,
            on,
            off,
            k: 0,
            start: 0.0,
        }
    }

    fn length(&self, k: u64) -> f64 {
        let (lo, hi) = if k % 2 == 1 { self.on } else { self.off };
        lo + (hi - lo) * unit(self.seed, k, 0x5eed)
    }

    /// À l'instant `t` (s) : « avec » ou non, et le temps qu'il reste à cette période.
    pub fn at(&mut self, t: f64) -> (bool, f64) {
        let t = t.max(0.0);
        if t < self.start {
            self.k = 0;
            self.start = 0.0;
        }
        loop {
            let len = self.length(self.k);
            if t < self.start + len {
                return (self.k % 2 == 1, self.start + len - t);
            }
            self.start += len;
            self.k += 1;
        }
    }
}

/// Les deux horloges d'un monde de graine `seed` : la pluie et le tonnerre.
pub fn clocks(seed: u64) -> (Cycle, Cycle) {
    (
        Cycle::new(mix64(seed ^ 0x7261_696e), RAIN_ON_S, RAIN_OFF_S),
        Cycle::new(mix64(seed ^ 0x7468_756e), THUNDER_ON_S, THUNDER_OFF_S),
    )
}

/// Le temps que donnent les deux horloges à l'instant `t` (`proto::WEATHER_…`), et combien
/// il dure encore. (Ce que montre le jeu est lissé : il le suit de près, pas à la seconde.)
pub fn natural_at(rain: &mut Cycle, thunder: &mut Cycle, t: f64) -> (u8, f64) {
    use crate::proto::{WEATHER_CLEAR, WEATHER_RAIN, WEATHER_STORM};
    let (wet, rain_left) = rain.at(t);
    let (loud, thunder_left) = thunder.at(t);
    match (wet, loud) {
        (true, true) => (WEATHER_STORM, rain_left.min(thunder_left)),
        (true, false) => (WEATHER_RAIN, rain_left.min(thunder_left)),
        (false, _) => (WEATHER_CLEAR, rain_left),
    }
}

/// Horloge de la météo en ligne à l'heure du système `unix` (s depuis 1970).
pub fn online_clock(unix: f64) -> f64 {
    (unix - ONLINE_EPOCH_S).max(0.0)
}

/// Un éclair des nuages : au-dessus de (`x`, `z`), `delay` secondes après le début de son
/// créneau, et de quoi tirer sa forme.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CloudSpot {
    pub x: f32,
    pub z: f32,
    pub delay: f32,
    pub seed: u32,
}

/// Zone (`CLOUD_ZONE` blocs de côté) qui contient la coordonnée `v`.
pub fn cloud_zone(v: f32) -> i32 {
    (v / CLOUD_ZONE as f32).floor() as i32
}

/// L'éclair des nuages de la zone (`cx`, `cz`) dans le créneau `slot` d'un monde de graine
/// `seed`, s'il y en a un : il y en a trois fois plus souvent quand il passe à moins de
/// `CHARGE_REACH` blocs de `holder` (celui qui porte la Foudre de Zeus, x et z). Sa place
/// et son moment ne dépendent pas de `holder` : un porteur en ajoute, il n'en change aucun.
pub fn cloud_spot(
    seed: u64,
    slot: i64,
    cx: i32,
    cz: i32,
    holder: Option<(f32, f32)>,
) -> Option<CloudSpot> {
    let key = (cx as u32 as u64) << 32 | cz as u32 as u64;
    let s = slot as u64;
    let x = (cx * CLOUD_ZONE) as f32 + unit(seed ^ 0xc1, s, key) as f32 * CLOUD_ZONE as f32;
    let z = (cz * CLOUD_ZONE) as f32 + unit(seed ^ 0xc2, s, key) as f32 * CLOUD_ZONE as f32;
    let near = holder.is_some_and(|(hx, hz)| (hx - x).hypot(hz - z) <= CHARGE_REACH);
    let p = if near { CLOUD_P * CLOUD_BOOST } else { CLOUD_P };
    if unit(seed ^ 0xc10d, s, key) >= p {
        return None;
    }
    Some(CloudSpot {
        x,
        z,
        delay: (unit(seed ^ 0xc3, s, key) * CLOUD_SLOT_S) as f32,
        seed: mix64(seed ^ key ^ s << 24 ^ 0xc10d) as u32,
    })
}

/// Les éclairs des nuages du créneau `slot` qui passent à moins de `CHARGE_REACH` blocs du
/// porteur en (`x`, `z`) (ceux qui le chargeaient avant la v282).
pub fn charging_spots(seed: u64, slot: i64, x: f32, z: f32) -> Vec<CloudSpot> {
    let reach = CHARGE_REACH;
    let mut out = Vec::new();
    for cz in cloud_zone(z - reach)..=cloud_zone(z + reach) {
        for cx in cloud_zone(x - reach)..=cloud_zone(x + reach) {
            if let Some(s) = cloud_spot(seed, slot, cx, cz, Some((x, z))) {
                if (s.x - x).hypot(s.z - z) <= reach {
                    out.push(s);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Les éclairs des nuages : les mêmes pour tous (graine, créneau, zone), trois fois plus
    /// nombreux près du porteur, qui n'en change aucun ailleurs ; ceux qui le chargent sont
    /// à sa portée.
    #[test]
    fn cloud_spots_depend_on_the_seed_and_three_times_more_near_the_holder() {
        let (mut alone, mut near, mut charging) = (0, 0, 0);
        let (hx, hz) = (300.0f32, -120.0f32);
        for slot in 0..4000i64 {
            let a = cloud_spot(7, slot, 1, -1, None);
            assert_eq!(a, cloud_spot(7, slot, 1, -1, None));
            let b = cloud_spot(7, slot, 1, -1, Some((hx, hz)));
            if let Some(a) = a {
                assert_eq!(Some(a), b, "un porteur n'en change aucun");
                alone += usize::from((a.x - hx).hypot(a.z - hz) <= CHARGE_REACH);
            }
            if let Some(b) = b {
                assert!(b.delay >= 0.0 && b.delay < CLOUD_SLOT_S as f32);
                near += usize::from((b.x - hx).hypot(b.z - hz) <= CHARGE_REACH);
            }
            for s in charging_spots(7, slot, hx, hz) {
                assert!((s.x - hx).hypot(s.z - hz) <= CHARGE_REACH);
                charging += 1;
            }
        }
        let ratio = near as f32 / alone.max(1) as f32;
        assert!(alone > 20 && (2.3..=3.7).contains(&ratio), "{alone} {near}");
        assert!(charging >= near, "{charging} {near}");
        // Une autre graine, d'autres éclairs.
        assert!((0..200i64).any(|s| cloud_spot(7, s, 0, 0, None) != cloud_spot(8, s, 0, 0, None)));
    }

    /// (v282) Une charge toutes les 10 secondes, aux multiples de 10 de l'horloge : comptée
    /// une fois, même découpée en morceaux.
    #[test]
    fn charges_come_every_ten_seconds_of_the_clock() {
        assert_eq!(charge_marks(0.0, 60.0).collect::<Vec<_>>(), [10.0, 20.0, 30.0, 40.0, 50.0, 60.0]);
        assert_eq!(charge_marks(10.0, 19.9).count(), 0);
        assert_eq!(charge_marks(9.99, 10.0).count(), 1);
        assert_eq!(charge_marks(100.0, 3700.0).count(), 360);
        let mut n = 0;
        let mut t = 1234.5;
        while t < 4834.5 {
            n += charge_marks(t, t + 0.37).count();
            t += 0.37;
        }
        assert_eq!(n, charge_marks(1234.5, t).count());
        assert!((359..=361).contains(&n), "{n}");
    }

    /// Les horloges : la même météo pour la même graine et le même instant, d'où qu'on la
    /// lise (même en revenant en arrière) ; il y a des orages.
    #[test]
    fn the_natural_weather_is_a_function_of_the_seed_and_the_clock() {
        use crate::proto::WEATHER_STORM;
        let (mut r, mut t) = clocks(42);
        let (mut r2, mut t2) = clocks(42);
        let mut storms = 0;
        for k in 0..20_000u32 {
            let at = k as f64 * 97.0;
            let a = natural_at(&mut r, &mut t, at);
            storms += usize::from(a.0 == WEATHER_STORM);
            if k % 50 == 0 {
                assert_eq!(a, natural_at(&mut r2, &mut t2, at));
            }
        }
        assert!(storms > 100, "{storms}");
        assert_eq!(online_clock(ONLINE_EPOCH_S + 10.0), 10.0);
        assert_eq!(online_clock(0.0), 0.0);
    }
}
