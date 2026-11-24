//! (v177) Skins des bots du mode FPS, dessinés ici pixel par pixel pour Minelib (rien n'est
//! repris d'ailleurs). Chaque bot a le sien, tiré de son nom (toujours le même), habillé
//! selon son équipe : tenues de ville et de terrain pour les terroristes (veste, sweat à
//! capuche, t-shirt ; bonnet, cagoule, foulard), tenue d'intervention pour les
//! antiterroristes (bleu, noir, gris ; gilet à poches, casque ou casquette, gants,
//! genouillères). Teintes de peau, cheveux et visages sont tirés de la même façon dans les
//! deux équipes. (v226) Et celui de Pistache, le personnage caché de la foule d'essai.

use std::sync::Arc;

use crate::skin::Skin;

type Rgb = [u8; 3];

/// Tirages (SplitMix64), à partir du nom du bot.
struct Rng(u64);

impl Rng {
    fn of(name: &str, team: u8) -> Self {
        // FNV-1a : le même nom donne toujours le même bot.
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in name.bytes().chain([team]) {
            h = (h ^ b as u64).wrapping_mul(0x0100_0000_01b3);
        }
        Rng(h)
    }

    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn chance(&mut self, p: f32) -> bool {
        ((self.next() >> 40) as f32 / (1u64 << 24) as f32) < p
    }

    fn pick<T: Copy>(&mut self, list: &[T]) -> T {
        list[(self.next() % list.len() as u64) as usize]
    }
}

/// Couleur `c` éclaircie (`k` > 1) ou assombrie (`k` < 1).
fn mul(c: Rgb, k: f32) -> Rgb {
    c.map(|v| (v as f32 * k).round().clamp(0.0, 255.0) as u8)
}

/// Mélange de `a` et `b` (`t` = part de `b`).
fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    [0, 1, 2].map(|i| (a[i] as f32 + (b[i] as f32 - a[i] as f32) * t).round() as u8)
}

#[derive(Clone, Copy, PartialEq)]
enum Hair {
    Short,
    Buzz,
    Long,
    Bald,
}

#[derive(Clone, Copy, PartialEq)]
enum Headwear {
    Bare,
    Beanie(Rgb),
    Balaclava(Rgb),
    /// Foulard sur le bas du visage.
    Bandana(Rgb),
    Cap(Rgb),
    Helmet(Rgb),
}

#[derive(Clone, Copy, PartialEq)]
enum Top {
    /// Veste ouverte sur un t-shirt.
    Jacket,
    Hoodie,
    Tee,
    /// Tenue d'intervention (manches longues), sous un gilet à poches.
    Uniform,
}

/// Tout ce qui fait un bot.
struct Look {
    skin: Rgb,
    hair: Rgb,
    hair_style: Hair,
    eyes: Rgb,
    beard: bool,
    head: Headwear,
    glasses: bool,
    top_kind: Top,
    top: Rgb,
    /// T-shirt sous la veste, motif du t-shirt.
    under: Rgb,
    vest: Option<Rgb>,
    gloves: Option<Rgb>,
    patch: Option<Rgb>,
    pants: Rgb,
    knees: Option<Rgb>,
    boots: Rgb,
    belt: Rgb,
}

const SKINS: [Rgb; 7] = [
    [250, 216, 186],
    [236, 188, 150],
    [218, 166, 122],
    [192, 136, 94],
    [158, 104, 70],
    [118, 76, 50],
    [88, 58, 40],
];
const HAIRS: [Rgb; 13] = [
    [28, 22, 20],
    [28, 22, 20],
    [36, 28, 24],
    [58, 40, 28],
    [64, 44, 30],
    [70, 48, 32],
    [96, 64, 40],
    [104, 70, 42],
    [118, 58, 32],
    [190, 154, 92],
    [204, 172, 110],
    [150, 74, 40],
    [138, 134, 128],
];
const EYES: [Rgb; 8] = [
    [88, 58, 36],
    [88, 58, 36],
    [70, 46, 30],
    [50, 36, 26],
    [50, 36, 26],
    [62, 102, 158],
    [70, 110, 72],
    [102, 112, 120],
];
// Terroristes : couleurs de ville et de terrain (terre, olive, sable, charbon).
const T_TOPS: [Rgb; 8] = [
    [86, 92, 58],
    [160, 136, 98],
    [98, 64, 42],
    [58, 58, 62],
    [100, 46, 40],
    [122, 108, 72],
    [72, 90, 118],
    [132, 88, 52],
];
const UNDERS: [Rgb; 5] = [
    [214, 208, 196],
    [42, 42, 44],
    [122, 122, 120],
    [150, 52, 42],
    [176, 150, 96],
];
const T_PANTS: [Rgb; 6] = [
    [52, 68, 98],
    [44, 56, 82],
    [138, 120, 84],
    [38, 38, 42],
    [78, 82, 52],
    [92, 90, 88],
];
const T_HATS: [Rgb; 5] = [
    [34, 34, 36],
    [70, 74, 48],
    [96, 40, 36],
    [96, 96, 98],
    [58, 46, 34],
];
// Antiterroristes : tenue d'intervention (bleu nuit, noir, gris ardoise).
const CT_UNIFORMS: [Rgb; 5] = [
    [40, 50, 80],
    [34, 36, 42],
    [64, 76, 96],
    [86, 92, 102],
    [40, 62, 80],
];
const CT_VESTS: [Rgb; 5] = [
    [30, 30, 34],
    [60, 66, 52],
    [116, 98, 72],
    [30, 38, 60],
    [52, 56, 62],
];
const PATCHES: [Rgb; 4] = [
    [118, 168, 218],
    [218, 218, 212],
    [204, 172, 84],
    [170, 60, 52],
];

impl Look {
    fn of(name: &str, team: u8) -> Look {
        let mut r = Rng::of(name, team);
        let skin = r.pick(&SKINS);
        let hair = r.pick(&HAIRS);
        let hair_style = r.pick(&[
            Hair::Short,
            Hair::Short,
            Hair::Short,
            Hair::Buzz,
            Hair::Buzz,
            Hair::Long,
            Hair::Bald,
        ]);
        let eyes = r.pick(&EYES);
        let beard = r.chance(0.28);
        if team == minelib_net::game::T {
            let head = match r.next() % 20 {
                0..=6 => Headwear::Bare,
                7..=11 => Headwear::Beanie(r.pick(&T_HATS)),
                12..=15 => Headwear::Balaclava(r.pick(&T_HATS[..2])),
                _ => Headwear::Bandana(r.pick(&[
                    [150, 40, 36],
                    [34, 34, 36],
                    [70, 74, 48],
                    [104, 104, 106],
                ])),
            };
            let top = r.pick(&T_TOPS);
            let mut pants = r.pick(&T_PANTS);
            if pants == top {
                pants = mul(pants, 0.75);
            }
            Look {
                skin,
                hair,
                hair_style,
                eyes,
                beard,
                head,
                glasses: !matches!(head, Headwear::Balaclava(_)) && r.chance(0.22),
                top_kind: r.pick(&[Top::Jacket, Top::Jacket, Top::Hoodie, Top::Tee]),
                top,
                under: r.pick(&UNDERS),
                vest: None,
                gloves: r.chance(0.35).then_some([44, 40, 38]),
                patch: None,
                pants,
                knees: None,
                boots: r.pick(&[[58, 42, 30], [36, 34, 33], [96, 74, 50]]),
                belt: r.pick(&[[52, 36, 24], [30, 28, 28]]),
            }
        } else {
            let uniform = r.pick(&CT_UNIFORMS);
            let mut vest = r.pick(&CT_VESTS);
            if vest == uniform {
                vest = mul(vest, 0.8);
            }
            let head = match r.next() % 4 {
                0 | 1 => Headwear::Helmet(r.pick(&[vest, [32, 32, 36], mul(uniform, 0.85)])),
                2 => Headwear::Cap(r.pick(&[uniform, [30, 30, 34]])),
                _ => Headwear::Bare,
            };
            Look {
                skin,
                hair,
                hair_style,
                eyes,
                beard,
                head,
                glasses: r.chance(0.15),
                top_kind: Top::Uniform,
                top: uniform,
                under: uniform,
                vest: Some(vest),
                gloves: r.chance(0.85).then_some([34, 34, 36]),
                patch: Some(r.pick(&PATCHES)),
                pants: mul(uniform, 0.92),
                knees: r
                    .chance(0.7)
                    .then_some(r.pick(&[mul(vest, 0.9), [30, 30, 32]])),
                boots: r.pick(&[[30, 28, 28], [44, 36, 30]]),
                belt: [30, 30, 30],
            }
        }
    }
}

/// Face d'un pavé (comme `Skin::color`) : 0 devant, 1 derrière, 2 côté droit du
/// personnage, 3 côté gauche, 4 dessus, 5 dessous.
const FRONT: usize = 0;
const BACK: usize = 1;
const TOP: usize = 4;
const BOTTOM: usize = 5;

/// Rectangle (x, y, largeur, hauteur) de la face `face` d'un pavé de taille `w` × `h` × `d`
/// rangé en `origin` dans l'image du skin.
fn face_rect(origin: (u32, u32), [w, h, d]: [u32; 3], face: usize) -> (u32, u32, u32, u32) {
    let (ox, oy) = origin;
    match face {
        FRONT => (ox + d, oy + d, w, h),
        BACK => (ox + 2 * d + w, oy + d, w, h),
        2 => (ox, oy + d, d, h),
        3 => (ox + d + w, oy + d, d, h),
        TOP => (ox + d, oy, w, d),
        _ => (ox + d + w, oy, w, d),
    }
}

/// Pixel d'une face, avec ce qu'il faut savoir pour le dessiner.
#[derive(Clone, Copy)]
struct At {
    face: usize,
    u: u32,
    v: u32,
    /// Côté (faces 2 et 3) : distance au bord de devant (0 : le bord de devant).
    from_front: u32,
    /// Bras, jambe : face tournée vers l'extérieur du corps.
    outer: bool,
}

/// Dessine les six faces d'un pavé : `paint` donne la couleur de chaque pixel ; un léger
/// grain (tiré de la position) rend le tissu et la peau moins plats.
fn paint(
    px: &mut [[u8; 4]],
    origin: (u32, u32),
    size: [u32; 3],
    outer_face: usize,
    seed: u64,
    color: impl Fn(At) -> Rgb,
) {
    for face in 0..6 {
        let (x0, y0, fw, fh) = face_rect(origin, size, face);
        for v in 0..fh {
            for u in 0..fw {
                let from_front = match face {
                    2 => fw - 1 - u,
                    3 => u,
                    _ => 0,
                };
                let c = color(At {
                    face,
                    u,
                    v,
                    from_front,
                    outer: face == outer_face,
                });
                let (x, y) = (x0 + u, y0 + v);
                let h = (x as u64 * 0x9e37_79b9 + y as u64 * 0x85eb_ca6b + seed)
                    .wrapping_mul(0xc2b2_ae3d_27d4_eb4f);
                let grain = 1.0 + ((h >> 40) as f32 / (1u64 << 24) as f32 - 0.5) * 0.09;
                let c = mul(c, grain);
                px[(y * 64 + x) as usize] = [c[0], c[1], c[2], 255];
            }
        }
    }
}

/// Visage (devant de la tête, 8 × 8).
fn face_px(l: &Look, u: u32, v: u32) -> Rgb {
    let s = l.skin;
    let brow = if l.hair_style == Hair::Bald {
        mul(mix(l.hair, s, 0.3), 0.8)
    } else {
        mul(l.hair, 0.85)
    };
    if v == 3 && matches!(u, 1 | 2 | 5 | 6) {
        return brow;
    }
    if v == 4 {
        if l.glasses && (1..=6).contains(&u) {
            return if u == 1 || u == 5 {
                [70, 74, 80]
            } else {
                [24, 24, 28]
            };
        }
        return match u {
            1 | 6 => [236, 234, 228],
            2 | 5 => l.eyes,
            _ => s,
        };
    }
    let beard = mul(l.hair, 0.95);
    if l.beard && ((v >= 6 && !(v == 6 && (3..=4).contains(&u))) || (v == 5 && (u == 0 || u == 7)))
    {
        return beard;
    }
    if v == 5 && (3..=4).contains(&u) {
        return mul(s, 0.93);
    }
    if v == 6 && (3..=4).contains(&u) {
        return mix(s, [96, 42, 38], 0.5);
    }
    s
}

/// Cheveux sur ce pixel de la tête ?
fn hair_at(style: Hair, a: At) -> bool {
    let back = 7 - a.from_front.min(7);
    match (style, a.face) {
        (_, BOTTOM) => false,
        (Hair::Bald, TOP) => false,
        (_, TOP) => true,
        (Hair::Short, FRONT) => a.v <= 1 || (a.v == 2 && (a.u == 0 || a.u == 7)),
        (Hair::Short, BACK) => a.v <= 5,
        (Hair::Short, _) => a.v <= 2 || (a.v <= 4 && back <= 1),
        (Hair::Buzz, FRONT) => a.v == 0,
        (Hair::Buzz, BACK) => a.v <= 3,
        (Hair::Buzz, _) => a.v <= 1 || (a.v == 2 && back <= 2),
        (Hair::Long, FRONT) => a.v <= 1 || ((a.u == 0 || a.u == 7) && a.v <= 5),
        (Hair::Long, BACK) => true,
        (Hair::Long, _) => a.v <= 2 || a.from_front >= 2,
        (Hair::Bald, FRONT) => false,
        (Hair::Bald, BACK) => (3..=5).contains(&a.v),
        (Hair::Bald, _) => (3..=4).contains(&a.v) && back <= 2,
    }
}

fn head_px(l: &Look, a: At) -> Rgb {
    let back = 7 - a.from_front.min(7);
    match l.head {
        Headwear::Balaclava(c) => {
            // Fente des yeux.
            if a.face == FRONT && (3..=4).contains(&a.v) && (1..=6).contains(&a.u) {
                return face_px(l, a.u, a.v);
            }
            // Maille : un rang sur deux un peu plus sombre.
            return mul(c, if a.v.is_multiple_of(2) { 1.0 } else { 0.9 });
        }
        Headwear::Helmet(c) => {
            let covered = match a.face {
                TOP => true,
                FRONT => a.v <= 2,
                BACK => a.v <= 5,
                BOTTOM => false,
                _ => a.v <= 2 || (a.v <= 5 && back <= 4),
            };
            if covered {
                let rim = match a.face {
                    FRONT => a.v == 2,
                    BACK => a.v == 5,
                    TOP => false,
                    _ => a.v == 5 || (a.v == 2 && back > 4),
                };
                return if rim { mul(c, 0.72) } else { c };
            }
            // Jugulaire.
            if (a.face == 2 || a.face == 3) && a.v >= 6 && a.from_front == 2 {
                return [26, 26, 28];
            }
        }
        Headwear::Cap(c) => match a.face {
            TOP => return if a.u == 3 && a.v == 1 { mul(c, 0.7) } else { c },
            FRONT if a.v <= 1 => {
                return if a.v == 1 && (3..=4).contains(&a.u) {
                    [214, 208, 160]
                } else {
                    c
                }
            }
            FRONT if a.v == 2 => return mul(c, 0.62),
            BACK if a.v <= 1 || (a.v == 2 && !(3..=4).contains(&a.u)) => return c,
            2 | 3 if a.v <= 1 => return c,
            _ => {}
        },
        Headwear::Beanie(c) => {
            let covered = match a.face {
                TOP => true,
                BOTTOM => false,
                BACK => a.v <= 3,
                _ => a.v <= 2,
            };
            if covered {
                // Revers plus clair, côtes verticales.
                let fold = a.v >= 2 && a.face != TOP;
                let rib = if a.u.is_multiple_of(2) { 1.0 } else { 0.9 };
                return mul(c, if fold { 1.12 * rib } else { rib });
            }
        }
        Headwear::Bandana(c) => {
            let dot = (a.u + a.v).is_multiple_of(3);
            let k = if dot { 1.25 } else { 1.0 };
            match a.face {
                FRONT if a.v >= 5 => return mul(c, k),
                2 | 3 if (5..=6).contains(&a.v) => return mul(c, k),
                BACK if a.v == 5 || (a.v >= 6 && (3..=4).contains(&a.u)) => return mul(c, 0.9),
                _ => {}
            }
        }
        Headwear::Bare => {}
    }
    if hair_at(l.hair_style, a) {
        let c = match l.hair_style {
            // Cheveux ras : on voit la peau entre les cheveux.
            Hair::Buzz => mix(
                l.hair,
                l.skin,
                if (a.u + a.v).is_multiple_of(2) {
                    0.3
                } else {
                    0.15
                },
            ),
            _ => l.hair,
        };
        // Mèches : un peu de relief.
        let k = if (a.u * 3 + a.v * 5).is_multiple_of(7) {
            1.15
        } else {
            1.0
        };
        return mul(c, k);
    }
    match a.face {
        FRONT => face_px(l, a.u, a.v),
        // Oreille.
        2 | 3 if (4..=5).contains(&a.v) && (3..=4).contains(&a.from_front) => mul(l.skin, 0.86),
        BOTTOM => {
            if l.beard {
                mul(l.hair, 0.9)
            } else {
                mul(l.skin, 0.8)
            }
        }
        // Crâne chauve : un reflet.
        TOP if a.u == 2 && a.v == 5 => mul(l.skin, 1.08),
        _ => l.skin,
    }
}

fn body_px(l: &Look, a: At) -> Rgb {
    let t = l.top;
    // Ceinture.
    if a.v == 11 && a.face != TOP && a.face != BOTTOM {
        return if a.face == FRONT && (3..=4).contains(&a.u) {
            [150, 150, 146]
        } else {
            l.belt
        };
    }
    match a.face {
        BOTTOM => return l.pants,
        TOP => {
            if let Some(v) = l.vest {
                if matches!(a.u, 1 | 2 | 5 | 6) {
                    return v;
                }
            }
            return t;
        }
        _ => {}
    }
    if let Some(vest) = l.vest {
        let on = match a.face {
            FRONT => {
                (1..=10).contains(&a.v) && !(a.v <= 1 && (3..=4).contains(&a.u))
                    || (a.v == 0 && matches!(a.u, 1 | 2 | 5 | 6))
            }
            BACK => a.v <= 10,
            _ => (2..=9).contains(&a.v),
        };
        if on {
            if a.face == FRONT {
                // Poches du bas, plaque nominative, radio.
                let pocket = (7..=9).contains(&a.v) && a.u != 2 && a.u != 5;
                if pocket {
                    return if a.v == 7 {
                        mul(vest, 0.78)
                    } else {
                        mul(vest, 1.12)
                    };
                }
                if a.v == 3 && (1..=2).contains(&a.u) {
                    return mix(vest, [230, 230, 222], 0.55);
                }
                if (2..=4).contains(&a.v) && a.u == 6 {
                    return [26, 26, 28];
                }
            }
            if a.face == BACK {
                if a.v == 1 && (2..=5).contains(&a.u) {
                    return mul(vest, 0.7);
                }
                if (5..=6).contains(&a.v) && (1..=6).contains(&a.u) {
                    return mix(vest, [230, 230, 222], 0.35);
                }
            }
            return vest;
        }
        return if a.face == FRONT && a.v == 0 {
            mul(t, 1.1)
        } else {
            t
        };
    }
    match l.top_kind {
        Top::Jacket => {
            if a.face == FRONT {
                if (3..=4).contains(&a.u) {
                    return l.under;
                }
                if a.v == 0 && (a.u == 2 || a.u == 5) {
                    return mul(t, 1.18);
                }
                if a.v == 8 && a.u != 2 && a.u != 5 {
                    return mul(t, 0.8);
                }
            }
            if a.face == BACK && a.v == 0 {
                return mul(t, 1.12);
            }
            t
        }
        Top::Hoodie => match a.face {
            FRONT if (1..=3).contains(&a.v) && (3..=4).contains(&a.u) => [214, 208, 196],
            FRONT if a.v == 7 && (2..=5).contains(&a.u) => mul(t, 0.75),
            FRONT if (8..=9).contains(&a.v) && (2..=5).contains(&a.u) => mul(t, 0.88),
            BACK if a.v <= 2 => mul(t, 0.84),
            BACK if a.v == 3 && (2..=5).contains(&a.u) => mul(t, 0.76),
            _ => t,
        },
        _ => {
            // T-shirt : une bande sur la poitrine, col.
            if a.face == FRONT && a.v == 0 && (3..=4).contains(&a.u) {
                return l.skin;
            }
            if a.face == FRONT && (4..=5).contains(&a.v) && (1..=6).contains(&a.u) {
                return l.under;
            }
            t
        }
    }
}

fn arm_px(l: &Look, a: At) -> Rgb {
    let sleeve_end = if l.top_kind == Top::Tee { 3 } else { 9 };
    let hand = l.gloves.unwrap_or(l.skin);
    match a.face {
        TOP => return l.top,
        BOTTOM => return hand,
        _ => {}
    }
    // Gants (mitaines sous une veste) : jusqu'au poignet.
    if a.v >= 10 || (l.gloves.is_some() && a.v >= 9 && l.top_kind != Top::Tee) {
        return hand;
    }
    if a.v > sleeve_end {
        return if l.gloves.is_some() && a.v >= 9 {
            hand
        } else {
            l.skin
        };
    }
    if let Some(p) = l.patch {
        if a.outer && (1..=2).contains(&a.v) && (1..=2).contains(&a.u) {
            return p;
        }
    }
    if a.v == sleeve_end {
        return mul(l.top, 0.82);
    }
    if l.top_kind == Top::Jacket && a.v == 6 {
        return mul(l.top, 0.9);
    }
    l.top
}

fn leg_px(l: &Look, a: At) -> Rgb {
    let p = l.pants;
    match a.face {
        TOP => return p,
        BOTTOM => return mul(l.boots, 0.55),
        _ => {}
    }
    if a.v >= 9 {
        return match a.v {
            9 => mul(l.boots, 1.2),
            11 => mul(l.boots, 0.62),
            _ => l.boots,
        };
    }
    if let Some(k) = l.knees {
        if a.face == FRONT && (4..=5).contains(&a.v) {
            return if a.v == 4 { mul(k, 1.15) } else { k };
        }
    }
    // Poche cargo sur le côté.
    if a.outer && (3..=6).contains(&a.v) && (1..=2).contains(&a.u) {
        return if a.v == 3 { mul(p, 0.74) } else { mul(p, 0.9) };
    }
    if a.v == 8 {
        return mul(p, 0.88);
    }
    p
}

/// Skin du bot `name` dans l'équipe `team` (`CT` ou `T`).
pub fn bot_skin(name: &str, team: u8) -> Arc<Skin> {
    let l = Look::of(name, team);
    let seed = Rng::of(name, team).next();
    paint_look(&l, seed)
}

/// (v226) Skin de Pistache, le personnage caché dans la foule d'essai (`crowd`), dessiné ici
/// pour Minelib : reconnaissable de loin à qui le cherche (sweat vert pistache, casquette
/// jaune, pantalon violet, baskets blanches), sans rien qui le distingue de près des autres
/// (il a un pseudo de la foule). Toujours le même.
pub fn hidden_skin() -> Arc<Skin> {
    static SKIN: std::sync::OnceLock<Arc<Skin>> = std::sync::OnceLock::new();
    SKIN.get_or_init(|| {
        let l = Look {
            skin: [232, 190, 152],
            hair: [168, 86, 38],
            hair_style: Hair::Short,
            eyes: [62, 112, 58],
            beard: false,
            head: Headwear::Cap([244, 200, 36]),
            glasses: false,
            top_kind: Top::Hoodie,
            top: [150, 198, 98],
            under: [246, 236, 206],
            vest: None,
            gloves: None,
            patch: None,
            pants: [112, 62, 152],
            knees: None,
            boots: [236, 236, 228],
            belt: [64, 42, 30],
        };
        paint_look(&l, 0x5157_ac4e_0000_0226)
    })
    .clone()
}

/// Les six pavés d'un skin, dessinés d'après `l`.
fn paint_look(l: &Look, seed: u64) -> Arc<Skin> {
    let mut px = vec![[0u8; 4]; 64 * 64];
    paint(&mut px, (0, 0), [8, 8, 8], usize::MAX, seed, |a| head_px(l, a));
    paint(&mut px, (16, 16), [8, 12, 4], usize::MAX, seed, |a| body_px(l, a));
    // Bras droit (côté droit dehors) et gauche ; jambes de même.
    paint(&mut px, (40, 16), [4, 12, 4], 2, seed, |a| arm_px(l, a));
    paint(&mut px, (32, 48), [4, 12, 4], 3, seed, |a| arm_px(l, a));
    paint(&mut px, (0, 16), [4, 12, 4], 2, seed, |a| leg_px(l, a));
    paint(&mut px, (16, 48), [4, 12, 4], 3, seed, |a| leg_px(l, a));
    Skin::from_pixels(px)
}

#[cfg(test)]
mod tests {
    use super::*;
    use minelib_net::game::{CT, T};

    #[test]
    fn every_bot_has_its_own_full_skin() {
        let names = [
            "BOT Hélène",
            "BOT Karim",
            "BOT Jeanne",
            "BOT Albert",
            "BOT Sacha",
        ];
        let mut seen = Vec::new();
        for name in names {
            for team in [CT, T] {
                let s = bot_skin(name, team);
                // Toujours le même pour le même bot.
                assert!(*s == *bot_skin(name, team));
                // Chaque pixel de chaque face est opaque (rien du modèle par défaut).
                for part in 0..6 {
                    let size = match part {
                        5 => [8, 8, 8],
                        2 => [8, 12, 4],
                        _ => [4, 12, 4],
                    };
                    for face in 0..6 {
                        let (w, h) = match face {
                            0 | 1 => (size[0], size[1]),
                            2 | 3 => (size[2], size[1]),
                            _ => (size[0], size[2]),
                        };
                        for v in 0..h {
                            for u in 0..w {
                                assert!(s.layer_color(part, face, u, v, size, false).is_some());
                                // Calque du dessus vide : le dessin est celui du dessous.
                                assert!(s.layer_color(part, face, u, v, size, true).is_none());
                            }
                        }
                    }
                }
                seen.push(s.rgba());
            }
        }
        // Tous différents.
        for i in 0..seen.len() {
            for j in 0..i {
                assert!(seen[i] != seen[j], "{i} et {j} identiques");
            }
        }
    }

    /// Aperçu (lancé à la main) : `BOT_SKIN_PNG=fichier.png cargo test preview_bots --
    /// --ignored` dessine six bots (trois T, trois CT) avec ce qu'ils peuvent tenir, le
    /// premier avec la bombe sur le dos.
    #[test]
    #[ignore]
    fn preview_bots() {
        use crate::player_model::{build, Held, Pose};
        use glam::{Mat4, Vec3};
        let Ok(path) = std::env::var("BOT_SKIN_PNG") else {
            return;
        };
        let w = |n: &str| {
            crate::weapons::WEAPONS
                .iter()
                .position(|d| d.name == n)
                .unwrap() as u8
        };
        let gear = [
            w("AK-47"),
            w("AWP"),
            w("Glock-18"),
            crate::cs::W_KNIFE,
            minelib_net::game::ITEM_NADE as u8,
            crate::cs::W_BOMB,
        ];
        let names = [
            "BOT Hélène",
            "BOT Karim",
            "BOT Jeanne",
            "BOT Albert",
            "BOT Sacha",
            "BOT Inès",
        ];
        let turn: f32 = std::env::var("BOT_SKIN_TURN")
            .ok()
            .and_then(|t| t.parse().ok())
            .unwrap_or(0.5);
        let mut verts = Vec::new();
        for (i, name) in names.iter().enumerate() {
            let team = if i < 3 { T } else { CT };
            let pose = Pose {
                feet: Vec3::new(i as f32 * 1.1 - 2.75, 0.0, 0.0),
                // Face à la caméra (regard vers +z), un peu tourné.
                yaw: std::f32::consts::PI + turn,
                pitch: 0.0,
                walk: 0.0,
                swing: 0.0,
                crouch: 0.0,
                light: [1.0; 3],
                held: Some(Held::Gear(gear[i])),
                skin: Some(bot_skin(name, team)),
                bow: 0.0,
                air: 0.0,
                dir: 0.0,
                run: 0.0,
            };
            verts.extend(build(&pose));
            // Le premier porte la bombe sur le dos.
            if i == 0 {
                verts.extend(crate::player_model::bomb_on_back(&pose));
            }
        }
        let size = 1024;
        let cam: Vec<f32> = std::env::var("BOT_SKIN_CAM")
            .unwrap_or_else(|_| "0,1.4,6.5,0,0.95,0".into())
            .split(',')
            .filter_map(|v| v.parse().ok())
            .collect();
        let eye = Vec3::new(cam[0], cam[1], cam[2]);
        let vp = Mat4::perspective_infinite_reverse_rh(45f32.to_radians(), 1.0, 0.05)
            * Mat4::look_at_rh(eye, Vec3::new(cam[3], cam[4], cam[5]), Vec3::Y);
        let Some([cpu, _]) = minelib_render::draw_offscreen(vp, size, &verts, &[], &[], &[]) else {
            eprintln!("pas de carte graphique");
            return;
        };
        let mut img = image::RgbImage::new(size, size);
        for (i, p) in cpu.iter().enumerate() {
            let bg = [0.42, 0.47, 0.52];
            let c = [0, 1, 2].map(|k| {
                let lin = if p[3] > 0.5 { p[k] } else { bg[k] };
                (lin.clamp(0.0, 1.0).powf(1.0 / 2.2) * 255.0) as u8
            });
            img.put_pixel(i as u32 % size, i as u32 / size, image::Rgb(c));
        }
        img.save(&path).unwrap();
    }

    /// Les deux équipes se reconnaissent : la tenue des CT (corps et bras) est froide (plus
    /// de bleu que de rouge) ou sombre, celle des T jamais bleu nuit.
    #[test]
    fn teams_wear_their_own_colors() {
        for i in 0..40 {
            let name = format!("BOT Essai {i}");
            let ct = Look::of(&name, CT);
            assert!(ct.vest.is_some() && ct.top_kind == Top::Uniform);
            let [r, _, b] = ct.top;
            assert!(b >= r || (r as u32 + b as u32) < 100, "{:?}", ct.top);
            let t = Look::of(&name, T);
            assert!(t.vest.is_none() && t.top_kind != Top::Uniform);
            assert!(!CT_UNIFORMS.contains(&t.top));
        }
    }

    /// (v226) Pistache, le personnage caché de la foule d'essai : un skin complet, toujours le
    /// même (une seule image pour tout le jeu), le sweat vert pistache, que n'a aucun bot.
    #[test]
    fn pistache_has_his_own_full_skin() {
        let s = hidden_skin();
        assert!(Arc::ptr_eq(&s, &hidden_skin()));
        for part in 0..6 {
            let size = match part {
                5 => [8, 8, 8],
                2 => [8, 12, 4],
                _ => [4, 12, 4],
            };
            for face in 0..6 {
                let (w, h) = match face {
                    0 | 1 => (size[0], size[1]),
                    2 | 3 => (size[2], size[1]),
                    _ => (size[0], size[2]),
                };
                for v in 0..h {
                    for u in 0..w {
                        assert!(s.layer_color(part, face, u, v, size, false).is_some());
                    }
                }
            }
        }
        for i in 0..40 {
            for team in [CT, T] {
                let l = Look::of(&format!("BOT Essai {i}"), team);
                assert_ne!(l.top, [150, 198, 98]);
                assert!(*bot_skin(&format!("BOT Essai {i}"), team) != *s);
            }
        }
    }
}
