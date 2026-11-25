//! (v309) Les tours de la carte bouclée (`minelib_world::lap`) : « si on se tp plus loin, on
//! fait le tour de la map logique, et tu indiques combien de fois on a fait le tour de la map
//! quand on en fait au moins un, dans quelle direction et quel degré » ; « les coordonnées
//! bouclent, on ne veut pas qu'un mec glitch le jeu avec ça si le chiffre est trop grand en
//! mémoire » (Viendax, 25 novembre).
//!
//! Une place très loin se compte en tours de la carte (entiers de 128 bits, exacts) et en
//! place sur la carte. Les nombres de `/tp` sont lus chiffre à chiffre, sans passer par un
//! flottant : au plus `MAX_DIGITS` chiffres avant la virgule (au-delà, refusé), jamais de
//! dépassement, et le compte des tours ne peut pas déborder (additions vérifiées).

use minelib_world::lap::{split, LAP};

/// Tours de la carte faits par une place : sur x (vers l'est en plus), sur z (vers le sud en
/// plus). (0, 0) : sur la carte, sans en avoir fait le tour.
pub type Laps = [i128; 2];

/// Au plus tant de chiffres avant la virgule dans un nombre de `/tp` (10³⁸ blocs : un entier de
/// 128 bits en tient 1,7 × 10³⁸).
pub const MAX_DIGITS: usize = 38;

/// Un nombre de `/tp` lu exactement : (partie entière, par défaut ; fraction dans [0, 1)).
/// « 12 », « -12.25 » (→ (−13, 0,75)), « +3. », « .5 ». Pas d'exposant, pas de « inf » ni de
/// « NaN », au plus `MAX_DIGITS` chiffres avant la virgule (les zéros de tête ne comptent pas)
/// et 64 signes en tout.
pub fn parse_exact(s: &str) -> Option<(i128, f64)> {
    if s.is_empty() || s.len() > 64 {
        return None;
    }
    let (neg, body) = match s.as_bytes()[0] {
        b'-' => (true, &s[1..]),
        b'+' => (false, &s[1..]),
        _ => (false, s),
    };
    let (int, frac) = match body.split_once('.') {
        Some((i, f)) => (i, f),
        None => (body, ""),
    };
    if int.is_empty() && frac.is_empty() {
        return None;
    }
    if !int.bytes().all(|b| b.is_ascii_digit()) || !frac.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if int.trim_start_matches('0').len() > MAX_DIGITS {
        return None;
    }
    let mut n: i128 = 0;
    for b in int.bytes() {
        n = n.checked_mul(10)?.checked_add((b - b'0') as i128)?;
    }
    // La fraction : ses 15 premiers chiffres suffisent (un millionième de millimètre).
    let f: f64 = if frac.is_empty() {
        0.0
    } else {
        format!("0.{}", &frac[..frac.len().min(15)]).parse().ok()?
    };
    if !neg {
        return Some((n, f));
    }
    if f == 0.0 {
        Some((-n, 0.0))
    } else {
        Some((-n - 1, 1.0 - f))
    }
}

/// (partie entière, fraction) + (partie entière, fraction), exactement ; `None` s'il y a
/// dépassement.
pub fn add_exact(a: (i128, f64), b: (i128, f64)) -> Option<(i128, f64)> {
    let mut n = a.0.checked_add(b.0)?;
    let mut f = a.1 + b.1;
    if f >= 1.0 {
        f -= 1.0;
        n = n.checked_add(1)?;
    }
    Some((n, f.clamp(0.0, 1.0 - f64::EPSILON)))
}

/// Une place exacte (partie entière, fraction) ramenée sur la carte : (tours faits, place).
pub fn on_map(v: (i128, f64)) -> (i128, f64) {
    let (laps, at) = split(v.0);
    (laps, at as f64 + v.1)
}

/// La place exacte d'un point de la carte (`at`) après `laps` tours : `None` si elle ne tient
/// pas en 128 bits (jamais : `laps` vient d'une place qui tenait).
pub fn exact(laps: i128, at: f64) -> Option<(i128, f64)> {
    let floor = at.floor();
    let n = laps.checked_mul(LAP as i128)?.checked_add(floor as i128)?;
    Some((n, at - floor))
}

/// Le nombre `n` en groupes de trois chiffres (« 1 073 741 760 »).
pub fn grouped(n: u128) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() * 4 / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push('\u{202f}');
        }
        out.push(c);
    }
    out
}

/// La direction (degrés depuis le nord, dans le sens des aiguilles d'une montre, comme le
/// lacet : 90° à l'est) des tours `l`.
pub fn degrees(l: Laps) -> f64 {
    (l[0] as f64).atan2(-(l[1] as f64)).to_degrees().rem_euclid(360.0)
}

/// Le nom de la direction `deg` (huit vents).
fn wind(deg: f64) -> &'static str {
    const NAMES: [&str; 8] = [
        "nord",
        "nord-est",
        "est",
        "sud-est",
        "sud",
        "sud-ouest",
        "ouest",
        "nord-ouest",
    ];
    NAMES[(((deg + 22.5) / 45.0).floor() as usize) % 8]
}

/// Degrés en français : « 90° », « 36,9° ».
fn deg_text(deg: f64) -> String {
    let d = (deg * 10.0).round() / 10.0;
    let d = if d >= 360.0 { 0.0 } else { d };
    if d.fract() == 0.0 {
        format!("{d:.0}°")
    } else {
        format!("{d:.1}°").replace('.', ",")
    }
}

/// Ce qu'on dit des tours `l` : « 1 tour de la carte vers l'est (90°) », « 2 tours de la carte
/// vers l'est et 3 vers le nord : direction 33,7° (nord-est) » ; `None` sans tour.
pub fn text(l: Laps) -> Option<String> {
    let [x, z] = l;
    let part = |n: i128, plus: &'static str, minus: &'static str| {
        (n != 0).then(|| (n.unsigned_abs(), if n > 0 { plus } else { minus }))
    };
    let parts: Vec<(u128, &str)> = [
        part(x, "vers l'est", "vers l'ouest"),
        part(z, "vers le sud", "vers le nord"),
    ]
    .into_iter()
    .flatten()
    .collect();
    let deg = degrees(l);
    match parts[..] {
        [] => None,
        [(n, dir)] => Some(format!(
            "{} tour{} de la carte {dir} ({})",
            grouped(n),
            if n > 1 { "s" } else { "" },
            deg_text(deg)
        )),
        [(a, da), (b, db)] => Some(format!(
            "{} tour{} de la carte {da} et {} {db} : direction {} ({})",
            grouped(a),
            if a > 1 { "s" } else { "" },
            grouped(b),
            deg_text(deg),
            wind(deg)
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use minelib_world::lap::HALF;

    /// (v309) « On ne veut pas qu'un mec glitch le jeu avec ça si le chiffre est trop grand en
    /// mémoire » : les nombres de `/tp` lus exactement, jusqu'à 38 chiffres ; au-delà, ou ce
    /// qui n'est pas un nombre, refusé ; les additions ne débordent jamais.
    #[test]
    fn tp_numbers_are_read_exactly_or_refused() {
        assert_eq!(parse_exact("12"), Some((12, 0.0)));
        assert_eq!(parse_exact("-12.25"), Some((-13, 0.75)));
        assert_eq!(parse_exact("+3."), Some((3, 0.0)));
        assert_eq!(parse_exact(".5"), Some((0, 0.5)));
        assert_eq!(parse_exact("-0"), Some((0, 0.0)));
        let big = "9".repeat(38);
        assert_eq!(parse_exact(&big).unwrap().0, 10i128.pow(38) - 1);
        assert_eq!(parse_exact(&format!("-{big}")).unwrap().0, -(10i128.pow(38) - 1));
        assert_eq!(parse_exact(&format!("000{big}")).unwrap().0, 10i128.pow(38) - 1);
        for bad in [
            "",
            "-",
            ".",
            "1e9",
            "inf",
            "NaN",
            "infinity",
            "12a",
            "1.2.3",
            "--4",
            " 5",
            &"9".repeat(39),
            &"1".repeat(65),
        ] {
            assert_eq!(parse_exact(bad), None, "{bad:?}");
        }
        // Une place à 38 chiffres plus un « ~ » d'autant : pas de dépassement, refusé
        // proprement si ça ne tient pas.
        let max = parse_exact(&big).unwrap();
        assert_eq!(add_exact(max, (1, 0.5)), Some((10i128.pow(38), 0.5)));
        assert_eq!(add_exact(max, max), None, "2 × 10³⁸ ne tient pas : refusé");
        assert_eq!(add_exact((i128::MAX, 0.5), (0, 0.75)), None);
        assert_eq!(add_exact((3, 0.5), (1, 0.75)), Some((5, 0.25)));
    }

    /// (v309) Très loin, une place est des tours de la carte et une place sur la carte, sans
    /// rien perdre : 10³⁸ − 1 blocs à l'est.
    #[test]
    fn far_places_are_laps_and_a_place_on_the_map() {
        let v = parse_exact(&"9".repeat(38)).unwrap();
        let (laps, at) = on_map(v);
        assert!((-(HALF as f64)..HALF as f64).contains(&at));
        assert_eq!(exact(laps, at), Some(v));
        assert_eq!(on_map((HALF as i128, 0.5)), (1, -(HALF as f64) + 0.5));
        assert_eq!(on_map((-(HALF as i128) - 1, 0.25)), (-1, HALF as f64 - 0.75));
        assert_eq!(on_map((7, 0.0)), (0, 7.0));
    }

    /// (v318) Le jeu et le relais parlent de la même carte bouclée (`minelib_net::proto` ne
    /// dépend pas du monde : son tour est écrit deux fois).
    #[test]
    fn the_net_and_the_world_share_one_map() {
        use minelib_net::proto;
        use minelib_world::lap;
        assert_eq!(lap::LAP, proto::LAP_BLOCKS);
        assert_eq!(lap::LAP_CHUNKS, proto::LAP_CHUNKS);
        assert_eq!(lap::HALF as f64, proto::MAP_HALF);
        for v in [0.0, 1.5, -1e9, 1_073_741_759.5, -1_073_741_760.0, 1_073_741_760.0, 5e9, -7.25e12] {
            assert_eq!(lap::wrap_f64(v), proto::wrap_place(v), "{v}");
        }
        for v in [0i64, -1, 1_073_741_759, 1_073_741_760, -1_073_741_761, 9_999_999_999] {
            assert_eq!(lap::wrap_block(v), proto::wrap_block(v), "{v}");
        }
        for c in [0i64, 67_108_859, 67_108_860, -67_108_861, 1 << 40] {
            assert_eq!(lap::wrap_chunk(c), proto::wrap_chunk(c), "{c}");
        }
    }

    #[test]
    fn laps_are_told_with_their_direction() {
        assert_eq!(text([0, 0]), None);
        assert_eq!(text([1, 0]).unwrap(), "1 tour de la carte vers l'est (90°)");
        assert_eq!(text([-3, 0]).unwrap(), "3 tours de la carte vers l'ouest (270°)");
        assert_eq!(text([0, 2]).unwrap(), "2 tours de la carte vers le sud (180°)");
        assert_eq!(text([0, -1]).unwrap(), "1 tour de la carte vers le nord (0°)");
        assert_eq!(
            text([2, -3]).unwrap(),
            "2 tours de la carte vers l'est et 3 vers le nord : direction 33,7° (nord-est)"
        );
        assert_eq!(
            text([1_000_000, 0]).unwrap(),
            "1\u{202f}000\u{202f}000 tours de la carte vers l'est (90°)"
        );
        let huge = 10i128.pow(29);
        assert!(text([huge, -huge]).unwrap().contains("direction 45° (nord-est)"));
        assert_eq!(grouped(1_073_741_760), "1\u{202f}073\u{202f}741\u{202f}760");
        assert_eq!(grouped(12), "12");
    }
}
