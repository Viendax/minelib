//! (v216) Foudre : la forme d'un éclair (un canal brisé et ses branches), son déroulé (un
//! traceur descend du nuage, puis des arcs en retour rallument le canal plusieurs fois),
//! et la Foudre de Zeus : un éclair tenu en main, avec ses arcs et ses étincelles.
//!
//! Tout est fait de segments lumineux (`minelib_render::BoltSeg`), de la même teinte : la
//! foudre du ciel, l'éclair tenu en main et ses étincelles ont la même lumière.

use glam::{Mat4, Vec3};
use minelib_render::{BoltSeg, BOLT, BOLT_CLOUD_GLOW, BOLT_TINT};

/// Dégâts de la foudre, comme dans Minecraft : 5 points de vie (deux cœurs et demi) à ce
/// qui se trouve dans la boîte de l'éclair : 3 blocs autour du point frappé, de 3 blocs
/// en dessous à 9 au-dessus.
pub const DAMAGE: u8 = 5;
pub const DAMAGE_REACH: f32 = 3.0;
pub const DAMAGE_BELOW: f32 = 3.0;
pub const DAMAGE_ABOVE: f32 = 9.0;

/// Longueur de l'éclair tenu en main (blocs). (v217) 40 % plus grand qu'avant (0,9, la
/// moitié de la taille du joueur) : tout l'éclair grandit d'autant (`HELD_SCALE`), ses
/// arcs, son halo et ses étincelles compris.
pub const HELD_SCALE: f32 = 1.4;
pub const HELD_LEN: f32 = 0.9 * HELD_SCALE;
/// Son halo ne grossit pas autant que lui : il resterait un trait fin.
const HELD_HALO: f32 = 1.15;

/// (v217) Teinte du halo d'un éclair : le bleu de la foudre, tiré vers le violet de `k`
/// (0 … 1). Le cœur reste blanc : ce ne sont que des touches, d'un morceau à l'autre.
fn tint(k: f32) -> [f32; 3] {
    const VIOLET: [f32; 3] = [0.68, 0.4, 1.0];
    let k = k.clamp(0.0, 1.0);
    [0, 1, 2].map(|i| BOLT_TINT[i] + (VIOLET[i] - BOLT_TINT[i]) * k)
}

/// Tirage fixe (0 … 1) d'un morceau d'éclair, pour sa teinte.
fn shade(n: usize, seed: u32) -> f32 {
    let mut h = (n as u32).wrapping_mul(0x9e37_79b9) ^ seed.wrapping_mul(0x85eb_ca6b);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2c1b_3c6d);
    ((h >> 8) & 0xffff) as f32 / 65535.0
}

/// (v217) Demi-largeur du halo d'un éclair au plus, en part de sa distance à l'œil (soit à
/// l'écran) : de près, la foudre reste un trait fin et vif, comme l'éclair tenu en main.
const NEAR_WIDTH: f32 = 0.03;
/// En deçà (blocs), le canal a aussi ses filaments et ses arcs courts.
const DETAIL_REACH: f32 = 70.0;
/// (v237) Vu de près, un morceau droit de l'éclair est brisé plus fin pour l'image
/// (`fine_levels`). Un éclair tombé à quelques blocs était fait de trois ou quatre longs
/// traits droits (ses morceaux font 3 blocs) ; il garde maintenant le dessin qu'il a aux
/// distances moyennes, où chaque morceau tient 5 à 10 % de sa distance à l'œil.
/// `FINE_FROM` : en deçà de cette part de sa distance à l'œil, un morceau est dessiné tel
/// quel (à plus de 30 blocs pour un morceau de 3 : l'éclair des distances moyennes et
/// lointaines ne change pas du tout). Au-delà, il est coupé jusqu'à ce que chaque bout tienne
/// au plus `FINE` fois sa distance.
const FINE_FROM: f32 = 0.1;
const FINE: f32 = 0.06;
/// Au plus 2^6 bouts par morceau.
const FINE_LEVELS: u32 = 6;
/// Écart des zigzags ajoutés, en part de la longueur de chaque bout (comme le canal).
const FINE_ROUGH: f32 = 0.34;

/// (v237) Combien de fois couper en deux le morceau `a`–`b`, vu de `eye` (0 : il est assez
/// loin, on le dessine tel quel).
fn fine_levels(a: Vec3, b: Vec3, eye: Vec3) -> u32 {
    let ab = b - a;
    let len2 = ab.length_squared();
    if len2 < 1e-6 {
        return 0;
    }
    // Distance de l'œil au morceau.
    let t = ((eye - a).dot(ab) / len2).clamp(0.0, 1.0);
    let d = (a + ab * t - eye).length().max(0.25);
    let len = len2.sqrt();
    if len <= FINE_FROM * d {
        0
    } else {
        ((len / (FINE * d)).log2().ceil() as u32).clamp(1, FINE_LEVELS)
    }
}

/// Tirages au sort reproductibles (la forme d'un éclair ne dépend que de sa graine).
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ 0x2545_f491_4f6c_dd1d)
    }

    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut x = self.0;
        x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        x ^ (x >> 31)
    }

    /// 0 … 1 (exclu).
    pub fn unit(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }

    pub fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.unit()
    }

    /// −1 … 1, plus souvent près de 0.
    pub fn bell(&mut self) -> f32 {
        (self.unit() + self.unit() + self.unit()) * 2.0 / 3.0 - 1.0
    }

    /// Direction au hasard (unitaire).
    pub fn dir(&mut self) -> Vec3 {
        loop {
            let v = Vec3::new(self.bell(), self.bell(), self.bell());
            if v.length_squared() > 1e-4 {
                return v.normalize();
            }
        }
    }
}

/// Ligne brisée de `a` à `b` : chaque morceau est coupé en deux `levels` fois, son milieu
/// écarté de côté d'une part `rough` de sa longueur. Des zigzags à toutes les échelles,
/// comme un vrai canal de foudre. `2^levels + 1` points.
pub fn jagged(a: Vec3, b: Vec3, levels: u32, rough: f32, rng: &mut Rng) -> Vec<Vec3> {
    let mut pts = vec![a, b];
    for _ in 0..levels {
        let mut next = Vec::with_capacity(pts.len() * 2 - 1);
        for w in pts.windows(2) {
            let (p, q) = (w[0], w[1]);
            let d = q - p;
            let len = d.length();
            // Écart perpendiculaire au morceau.
            let r = rng.dir();
            let side = (r - d * (r.dot(d) / d.length_squared().max(1e-12))).normalize_or_zero();
            next.push(p);
            next.push((p + q) * 0.5 + side * (len * rough * rng.bell()));
        }
        next.push(*pts.last().unwrap_or(&b));
        pts = next;
    }
    pts
}

/// Un morceau du canal ou d'une branche.
#[derive(Clone, Copy, Debug)]
struct Piece {
    a: Vec3,
    b: Vec3,
    /// Chemin parcouru depuis le nuage (part de la longueur du canal) en `a` et en `b` :
    /// le traceur y arrive dans cet ordre.
    sa: f32,
    sb: f32,
    /// 0 : canal ; 1 : branche ; 2 : rameau.
    level: u8,
    /// Le long d'une branche : 1 à sa naissance … 0 à sa pointe.
    ta: f32,
    tb: f32,
    /// Largeur du halo en `a` et en `b`, par rapport à celle d'un éclair lointain (un
    /// éclair qui part de la main est fin près d'elle).
    wa: f32,
    wb: f32,
}

/// (v217) Sortes d'éclairs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Du nuage au sol : la foudre qui tombe.
    Ground,
    /// De la main vers le ciel (Foudre de Zeus, `/thor` en visant le ciel) : il monte,
    /// entre dans les nuages et les traverse.
    Sky,
    /// Dans les nuages pendant l'orage : il court sous leur base et dedans, sans tomber.
    Cloud,
    /// (v241) Arc court de la pointe de la Foudre de Zeus sans charge jusqu'à ce qu'on vise
    /// (8 blocs au plus) : il ne passe pas par le ciel.
    Arc,
}

/// Un coup de foudre.
#[derive(Clone, Debug)]
pub struct Strike {
    pub kind: Kind,
    /// Altitude (monde) de la base des nuages là où il passe : au-dessus, il éclaire le
    /// nuage de l'intérieur.
    cloud_base: f32,
    /// Point frappé (`Ground`), d'où il part (`Sky`), ou son milieu (`Cloud`).
    pub pos: Vec3,
    /// Temps avant qu'il parte (s), puis son âge.
    pub delay: f32,
    pub age: f32,
    pieces: Vec<Piece>,
    /// Durée de la descente du traceur (s).
    leader_s: f32,
    /// Arcs en retour : moment (s) et force (1 le premier, puis de moins en moins).
    strokes: Vec<(f32, f32)>,
    /// Le premier arc a touché : dégâts, tonnerre et lumière ont été donnés.
    pub landed: bool,
    pub seed: u32,
}

impl Strike {
    /// Éclair qui frappe `pos`, venu de la base des nuages à la hauteur `cloud_y`.
    pub fn new(pos: Vec3, seed: u32, cloud_y: f32, delay: f32) -> Self {
        let mut rng = Rng::new(seed as u64 | 0x51_0000_0000);
        let height = (cloud_y - pos.y).max(70.0);
        // Le canal arrive de biais.
        let lean = rng.dir() * Vec3::new(1.0, 0.0, 1.0) * (height * rng.range(0.1, 0.4));
        let top = pos + Vec3::new(lean.x, height, lean.z);
        let main = jagged(top, pos, 7, 0.34, &mut rng);
        Self::build(Kind::Ground, main, pos, pos.y + height, seed, delay, rng)
    }

    /// (v217) Éclair qui part de `from` (la main) dans la direction `dir` et monte dans
    /// les nuages : il entre par leur base (à l'altitude `cloud_base` au-dessus de lui) et
    /// traverse la couche, épaisse de `thick` blocs.
    pub fn up(from: Vec3, dir: Vec3, seed: u32, cloud_base: f32, thick: f32) -> Self {
        let mut rng = Rng::new(seed as u64 | 0x52_0000_0000);
        let dir = dir.normalize_or(Vec3::Y);
        // Jusqu'au haut de la couche ; presque à l'horizontale, il se perd au loin.
        let rise = (cloud_base - from.y).max(40.0) + thick * rng.range(0.7, 0.95);
        let len = (rise / dir.y.max(0.05)).min(1600.0);
        // Par tronçons de plus en plus longs : vu de son départ, le canal doit être brisé
        // fin tout près (sinon son premier morceau barrerait l'écran d'un trait droit) et
        // rester du côté où l'on vise ; il s'écarte davantage en montant.
        let side = |rng: &mut Rng, along: f32| {
            let r = rng.dir();
            (r - dir * r.dot(dir)) * (along * 0.07)
        };
        let mut main = vec![from];
        let mut at = 0.0;
        for (reach, levels) in [(3.0f32, 4u32), (20.0, 5), (110.0, 5), (len.max(130.0), 6)] {
            let reach = reach.min(len);
            if reach <= at {
                continue;
            }
            let a = main[main.len() - 1];
            let b = from + dir * reach + side(&mut rng, reach);
            main.pop();
            main.extend(jagged(a, b, levels, 0.2, &mut rng));
            at = reach;
        }
        Self::build(Kind::Sky, main, from, cloud_base, seed, 0.0, rng)
    }

    /// (v217) Éclair d'orage qui ne tombe pas : il court de `a` à `b` dans la couche de
    /// nuages en passant par `dip`, plus bas (sous leur base, on l'y voit en entier).
    pub fn cloud(a: Vec3, dip: Vec3, b: Vec3, seed: u32, cloud_base: f32) -> Self {
        let mut rng = Rng::new(seed as u64 | 0x53_0000_0000);
        let mut main = jagged(a, dip, 6, 0.36, &mut rng);
        main.pop();
        main.extend(jagged(dip, b, 6, 0.36, &mut rng));
        Self::build(Kind::Cloud, main, dip, cloud_base, seed, 0.0, rng)
    }

    /// (v241) Arc de la Foudre de Zeus sans charge : de sa pointe `from` à ce qu'on vise,
    /// `to`, tout droit (il ne passe pas par le ciel), avec quelques rameaux.
    pub fn arc(from: Vec3, to: Vec3, seed: u32) -> Self {
        let mut rng = Rng::new(seed as u64 | 0x54_0000_0000);
        let main = jagged(from, to, 5, 0.2, &mut rng);
        Self::build(Kind::Arc, main, to, f32::MAX, seed, 0.0, rng)
    }

    /// Le canal `main` (du départ du traceur à son arrivée), ses branches et son déroulé.
    fn build(
        kind: Kind,
        main: Vec<Vec3>,
        pos: Vec3,
        cloud_base: f32,
        seed: u32,
        delay: f32,
        mut rng: Rng,
    ) -> Self {
        let (start, end) = (main[0], main[main.len() - 1]);
        // Largeur du halo à la distance `d` du départ : un éclair parti de la main est fin
        // près d'elle (sinon son halo remplirait l'écran), puis prend sa largeur.
        let wide = |p: Vec3| match kind {
            Kind::Sky => ((p - start).length() * 0.035 / 1.7).clamp(0.03, 1.0),
            // (v241) L'arc court est fin d'un bout à l'autre.
            Kind::Arc => 0.14,
            _ => 1.0,
        };
        let lengths: Vec<f32> = main.windows(2).map(|w| (w[1] - w[0]).length()).collect();
        let total: f32 = lengths.iter().sum::<f32>().max(1.0);
        let mut pieces = Vec::with_capacity(700);
        let mut s = 0.0;
        let mut along = Vec::with_capacity(main.len());
        for (w, len) in main.windows(2).zip(&lengths) {
            along.push(s);
            let e = s + len / total;
            pieces.push(Piece {
                a: w[0],
                b: w[1],
                sa: s,
                sb: e,
                level: 0,
                ta: 1.0,
                tb: 1.0,
                wa: wide(w[0]),
                wb: wide(w[1]),
            });
            s = e;
        }
        // Branches : elles partent du canal dans le sens où il avance (vers le bas pour la
        // foudre qui tombe, vers le haut pour celle qui monte), de côté, et se ramifient
        // parfois.
        let n = main.len() - 1;
        let often = if kind == Kind::Cloud { 0.06 } else { 0.085 };
        for i in 6..n.saturating_sub(6) {
            if rng.unit() > often {
                continue;
            }
            let ahead = (main[(i + 4).min(n)] - main[i]).normalize_or(Vec3::NEG_Y);
            let room = match kind {
                Kind::Ground => (main[i].y - end.y).max(8.0),
                // (v241) Les rameaux de l'arc court sont à sa mesure.
                Kind::Arc => (end - main[i]).length(),
                _ => (end - main[i]).length().clamp(8.0, 320.0),
            };
            let mut len = room * rng.range(0.14, 0.45);
            if kind == Kind::Sky {
                // Près de la main, pas de grande branche : vue de si près, elle barrerait
                // l'écran.
                let gone = (main[i] - start).length();
                if gone < 30.0 {
                    continue;
                }
                len = len.min(gone * 0.5);
            }
            let tip = main[i] + tilt(ahead, rng.range(0.32, 0.9), &mut rng) * len;
            let branch = jagged(main[i], tip, 5, 0.36, &mut rng);
            let from = pieces.len();
            push_branch(&mut pieces, &branch, along[i], total, 1, wide(main[i]));
            // Rameaux.
            let made = pieces.len() - from;
            for k in 4..made.saturating_sub(4) {
                if rng.unit() > 0.07 {
                    continue;
                }
                let p = pieces[from + k];
                let dir = (p.b - p.a).normalize_or(Vec3::NEG_Y);
                let tip =
                    p.a + tilt(dir, rng.range(0.35, 1.0), &mut rng) * (len * rng.range(0.15, 0.4));
                let twig = jagged(p.a, tip, 3, 0.36, &mut rng);
                push_branch(&mut pieces, &twig, p.sa, total, 2, p.wa);
            }
        }
        // Le traceur avance, puis deux à quatre arcs en retour, de moins en moins forts.
        let leader_s = match kind {
            Kind::Ground => rng.range(0.045, 0.075),
            Kind::Sky => rng.range(0.06, 0.1),
            Kind::Cloud => rng.range(0.09, 0.17),
            Kind::Arc => rng.range(0.012, 0.022),
        };
        let mut strokes = vec![(leader_s, 1.0)];
        let mut t = leader_s;
        let mut force = 1.0;
        for _ in 0..1 + (rng.next() % 3) {
            t += rng.range(0.05, 0.11);
            force *= rng.range(0.6, 0.85);
            strokes.push((t, force));
        }
        Self {
            kind,
            cloud_base,
            pos,
            delay,
            age: 0.0,
            pieces,
            leader_s,
            strokes,
            landed: false,
            seed,
        }
    }

    /// Avance de `dt`. Vrai : le premier arc en retour vient de toucher le sol.
    pub fn update(&mut self, dt: f32) -> bool {
        if self.delay > 0.0 {
            self.delay -= dt;
            return false;
        }
        self.age += dt;
        // Essais : MINELIB_FOUDRE_FIGEE=1 arrête chaque éclair à son plus vif (captures) ;
        // un autre nombre : à tant de secondes après que le premier arc a touché le sol
        // (négatif : avant, pendant la descente du traceur).
        static FROZEN: std::sync::OnceLock<Option<f32>> = std::sync::OnceLock::new();
        let frozen = FROZEN.get_or_init(|| {
            let v = std::env::var("MINELIB_FOUDRE_FIGEE").ok()?;
            match v.trim() {
                "1" => Some(0.004),
                v => v.parse().ok(),
            }
        });
        if let Some(at) = frozen {
            self.age = self.age.min((self.leader_s + at).max(0.0));
        }
        if !self.landed && self.age >= self.leader_s {
            self.landed = true;
            return true;
        }
        false
    }

    /// L'éclair est fini.
    pub fn done(&self) -> bool {
        self.age > self.strokes.last().map_or(0.0, |s| s.0) + 0.45
    }

    /// Lumière qu'il jette à cet instant (0 … 1 ; 1 au premier arc).
    pub fn light(&self) -> f32 {
        if self.delay > 0.0 {
            return 0.0;
        }
        let arcs: f32 = self
            .strokes
            .iter()
            .filter(|(t, _)| self.age >= *t)
            .map(|(t, f)| f * (-(self.age - t) / 0.06).exp())
            .sum();
        // (Le traceur éclaire à peine.)
        let leader = if self.age < self.leader_s {
            0.05 * self.age / self.leader_s
        } else {
            0.0
        };
        (arcs + leader).min(1.0)
    }

    /// Ses segments à cet instant, vus de `eye`. (v217) Tout près, le canal garde la
    /// finesse de l'éclair tenu en main (son halo ne dépasse pas une largeur à l'écran : à
    /// quelques blocs, c'était une colonne blanche qui mangeait l'image), et il en prend
    /// les animations : des filaments qui courent autour de lui, de courts arcs sur les
    /// côtés (`detail`).
    pub fn segments(&self, eye: Vec3, out: &mut Vec<BoltSeg>) {
        if self.delay > 0.0 {
            return;
        }
        // Demi-largeur du halo au plus, à la distance `d` de l'œil.
        let thin = |p: Vec3| NEAR_WIDTH * (p - eye).length() + 0.012;
        let reach = self.age / self.leader_s;
        // Canal : rallumé par chaque arc, puis une lueur qui traîne.
        let since = self.age - self.leader_s;
        let (main, branch) = if since >= 0.0 {
            let arcs: f32 = self
                .strokes
                .iter()
                .filter(|(t, _)| self.age >= *t)
                .map(|(t, f)| f * (-(self.age - t) / 0.05).exp())
                .sum();
            (
                arcs * 3.4 + 0.3 * (-since / 0.22).exp(),
                // Les branches ne se rallument pas : seul le premier arc les vide.
                1.5 * (-since / 0.035).exp() + 0.12 * (-since / 0.15).exp(),
            )
        } else {
            (0.0, 0.0)
        };
        // (v237) De près, chaque morceau est brisé plus fin (`fine_levels`) : mêmes zigzags
        // à chaque image (ils ne dépendent que de la graine et du morceau), et ceux d'un
        // niveau restent quand on en ajoute un en s'approchant.
        let mut parts: Vec<Piece> = Vec::with_capacity(1 << FINE_LEVELS);
        for (n, whole) in self.pieces.iter().enumerate() {
            if since < 0.0 && whole.sa >= reach {
                continue;
            }
            parts.clear();
            let levels = fine_levels(whole.a, whole.b, eye);
            if levels == 0 {
                parts.push(*whole);
            } else {
                let mut rng = Rng::new((self.seed as u64) << 24 ^ n as u64 ^ 0xf1e0_0000_0000_0000);
                let pts = jagged(whole.a, whole.b, levels, FINE_ROUGH, &mut rng);
                let m = (pts.len() - 1) as f32;
                let mix = |x: f32, y: f32, u: f32| x + (y - x) * u;
                for (k, w) in pts.windows(2).enumerate() {
                    let (u, v) = (k as f32 / m, (k + 1) as f32 / m);
                    parts.push(Piece {
                        a: w[0],
                        b: w[1],
                        sa: mix(whole.sa, whole.sb, u),
                        sb: mix(whole.sa, whole.sb, v),
                        level: whole.level,
                        ta: mix(whole.ta, whole.tb, u),
                        tb: mix(whole.ta, whole.tb, v),
                        wa: mix(whole.wa, whole.wb, u),
                        wb: mix(whole.wa, whole.wb, v),
                    });
                }
            }
          for p in &parts {
            let (mut b, mut sb, mut tb) = (p.b, p.sb, p.tb);
            let mut tip = 0.0;
            if since < 0.0 {
                // Le traceur n'est pas encore arrivé là.
                if p.sa >= reach {
                    continue;
                }
                if sb > reach {
                    let k = (reach - p.sa) / (sb - p.sa).max(1e-6);
                    b = p.a + (p.b - p.a) * k;
                    tb = p.ta + (p.tb - p.ta) * k;
                    sb = reach;
                }
                // Sa pointe brille, le reste du chemin est à peine visible.
                tip = 1.0;
            }
            let force = |s: f32, t: f32| {
                // Le haut de la foudre qui tombe se perd dans le nuage ; celle qui monte
                // se perd à son bout (dans la couche, ce sont les nuages qui la cachent) ;
                // celle des nuages, à ses deux bouts.
                let fade = match self.kind {
                    Kind::Ground => smooth(0.0, 0.07, s),
                    Kind::Sky => 1.0 - smooth(0.88, 1.0, s),
                    Kind::Cloud => smooth(0.0, 0.06, s) * (1.0 - smooth(0.94, 1.0, s)),
                    Kind::Arc => 1.0,
                };
                let base = match p.level {
                    0 => main,
                    _ => branch * t.powf(0.6),
                };
                let lead = tip * (0.22 + 1.1 * (-(reach - s).max(0.0) * 26.0).exp()) * t;
                (base + lead) * fade
            };
            let (fa, fb) = (force(p.sa, p.ta), force(sb, tb));
            if fa.max(fb) < 0.012 {
                continue;
            }
            let width = |t: f32| match p.level {
                0 => 1.7,
                1 => 0.5 + 0.7 * t,
                _ => 0.35 + 0.35 * t,
            };
            out.push(BoltSeg {
                a: [p.a.x, p.a.y, p.a.z, BOLT],
                b: [b.x, b.y, b.z, 1.0],
                // Des touches de violet : à peine sur le canal, davantage sur les branches.
                color: tint(shade(n / 16, self.seed) * if p.level == 0 { 0.35 } else { 0.9 }),
                force_a: fa,
                force_b: fb,
                width_a: (width(p.ta) * p.wa).min(thin(p.a)),
                width_b: (width(tb) * p.wb).min(thin(b)),
                screen: 0.0,
            });
          }
        }
        if since >= 0.0 && main > 0.05 {
            self.detail(eye, main, out);
        }
        // (v217) Dans la couche de nuages, le canal éclaire le nuage de l'intérieur : une
        // lueur large et douce le long de lui, par tronçons de huit morceaux (le rendu ne la
        // montre que là où il y a du nuage, voir bolt.wgsl).
        if matches!(self.kind, Kind::Ground | Kind::Arc) {
            return;
        }
        let level = if since >= 0.0 { main } else { 0.25 };
        let main_pieces = self.pieces.iter().take_while(|p| p.level == 0);
        let mut run: Option<(Vec3, Vec3, f32)> = None;
        for (k, p) in main_pieces.enumerate() {
            let inside = p.b.y > self.cloud_base - 20.0 && (since >= 0.0 || p.sb <= reach);
            if inside {
                run = Some(match run {
                    Some((a, _, s)) => (a, p.b, s),
                    None => (p.a, p.b, p.sa),
                });
            }
            if (k % 8 == 7 || !inside) && run.is_some() {
                let (a, b, s) = run.take().unwrap_or((p.a, p.b, p.sa));
                let fade = match self.kind {
                    Kind::Sky => 1.0 - smooth(0.9, 1.0, s),
                    _ => 1.0,
                };
                let force = (level * 0.11).min(0.6) * fade;
                if force > 0.004 {
                    out.push(BoltSeg {
                        a: [a.x, a.y, a.z, BOLT_CLOUD_GLOW],
                        b: [b.x, b.y, b.z, 1.0],
                        color: BOLT_TINT,
                        force_a: force,
                        force_b: force,
                        width_a: 60.0,
                        width_b: 60.0,
                        screen: 0.0,
                    });
                }
            }
        }
    }

    /// (v217) Près de l'œil, le canal allumé a les animations de l'éclair tenu en main :
    /// deux ou trois filaments fins courent autour de lui d'un point à un autre, et de
    /// courts arcs partent sur les côtés et s'éteignent en pointe ; ils changent plusieurs
    /// dizaines de fois par seconde. `level` : éclat du canal.
    fn detail(&self, eye: Vec3, level: f32, out: &mut Vec<BoltSeg>) {
        // Les points du canal à moins de 70 blocs de l'œil.
        let near: Vec<Vec3> = self
            .pieces
            .iter()
            .take_while(|p| p.level == 0)
            .filter(|p| (p.a - eye).length_squared() < DETAIL_REACH * DETAIL_REACH)
            .map(|p| p.a)
            .collect();
        if near.len() < 8 {
            return;
        }
        let force = (level * 0.3).min(1.3);
        let mut push = |a: Vec3, b: Vec3, fa: f32, fb: f32, wide: f32| {
            let w = |p: Vec3| wide * (NEAR_WIDTH * (p - eye).length() + 0.012);
            let color = tint(0.25 + 0.7 * fa.fract());
            let mut one = |a: Vec3, b: Vec3, fa: f32, fb: f32| {
                out.push(BoltSeg {
                    a: [a.x, a.y, a.z, BOLT],
                    b: [b.x, b.y, b.z, 1.0],
                    color,
                    force_a: fa,
                    force_b: fb,
                    width_a: w(a),
                    width_b: w(b),
                    screen: 0.0,
                });
            };
            // (v237) De près, brisés plus fin eux aussi : un filament court d'un point du
            // canal à un autre, des dizaines de blocs plus loin, par bouts de plusieurs
            // blocs ; à quelques blocs de l'œil c'étaient de longs traits droits et fins
            // en travers de l'image.
            let levels = fine_levels(a, b, eye);
            if levels == 0 {
                one(a, b, fa, fb);
                return;
            }
            let mut rng = Rng::new((a.x.to_bits() as u64) << 32 ^ (b.y.to_bits() as u64) ^ 0xf11e);
            let pts = jagged(a, b, levels, FINE_ROUGH, &mut rng);
            let m = (pts.len() - 1) as f32;
            for (k, s) in pts.windows(2).enumerate() {
                let (u, v) = (k as f32 / m, (k + 1) as f32 / m);
                one(s[0], s[1], fa + (fb - fa) * u, fa + (fb - fa) * v);
            }
        };
        // Filaments (31 fois par seconde).
        let mut rng = Rng::new((self.age * 31.0) as u64 ^ (self.seed as u64) << 8 ^ 0xf11a);
        for _ in 0..3 {
            let i = (rng.next() as usize) % (near.len() - 6);
            let j = (i + 4 + (rng.next() as usize) % 14).min(near.len() - 1);
            let f = force * rng.range(0.4, 0.9);
            for w in jagged(near[i], near[j], 4, 0.3, &mut rng).windows(2) {
                push(w[0], w[1], f, f, 0.45);
            }
        }
        // Arcs courts (23 fois par seconde).
        let mut rng = Rng::new((self.age * 23.0) as u64 ^ (self.seed as u64) << 8 ^ 0xa4c5);
        for _ in 0..7 {
            if rng.unit() < 0.3 {
                continue;
            }
            let i = 1 + (rng.next() as usize) % (near.len() - 2);
            let along = (near[i + 1] - near[i - 1]).normalize_or(Vec3::Y)
                * if rng.unit() < 0.5 { 1.0 } else { -1.0 };
            let len = ((near[i] - eye).length() * rng.range(0.05, 0.2)).min(7.0);
            let tip = near[i] + tilt(along, rng.range(0.5, 1.25), &mut rng) * len;
            let path = jagged(near[i], tip, 3, 0.28, &mut rng);
            let n = (path.len() - 1) as f32;
            let f = force * rng.range(0.7, 1.3);
            for (k, w) in path.windows(2).enumerate() {
                let (ta, tb) = (1.0 - k as f32 / n, 1.0 - (k + 1) as f32 / n);
                push(w[0], w[1], f * ta, f * tb, 0.3 + 0.35 * ta);
            }
        }
    }

    /// (v220) D'où part la lumière qu'il jette : deux points de son canal même, à 3,5 et à
    /// une vingtaine de blocs du point frappé (ou de la main, pour celui qui monte). La
    /// lumière et les ombres viennent ainsi de là où l'on voit l'éclair, pas d'à côté.
    /// (v235) 3,5 blocs et non 1,5 : collée au sol, elle le blanchissait tout autour.
    /// (v237) Celle qu'on lance vers le ciel : à 12 et à 45 blocs de la main. Tout près de
    /// la main, la lumière semblait venir d'au-dessus de soi, pas de là où l'on tape.
    pub fn light_points(&self) -> [Vec3; 2] {
        let from = self.pos;
        let [low, high] = if self.kind == Kind::Sky {
            [12.0, 45.0]
        } else {
            [3.5, 20.0]
        };
        let mut out = [from + Vec3::Y * low, from + Vec3::Y * high];
        let (mut near, mut far) = (f32::MAX, f32::MAX);
        for p in self.pieces.iter().take_while(|p| p.level == 0) {
            let d = (p.a - from).length();
            if (d - low).abs() < near {
                near = (d - low).abs();
                out[0] = p.a;
            }
            if (d - high).abs() < far {
                far = (d - high).abs();
                out[1] = p.a;
            }
        }
        out
    }

    /// (v237) Où il allume les nuages : le milieu de ce que son canal parcourt dans leur
    /// couche (celui qui monte y entre par la base) ; à défaut, son bout.
    pub fn cloud_entry(&self) -> Vec3 {
        let mut end = self.pos;
        let mut entry = None;
        for p in self.pieces.iter().take_while(|p| p.level == 0) {
            end = p.b;
            if entry.is_none() && p.b.y >= self.cloud_base {
                entry = Some(p.b);
            }
        }
        entry.map_or(end, |e| (e + end) * 0.5)
    }

    /// Ce coup de foudre blesse-t-il ce qui se tient en `feet` (ses pieds) ? (Seule la
    /// foudre qui tombe blesse.)
    pub fn hurts(&self, feet: Vec3) -> bool {
        self.kind == Kind::Ground && hurts_at(self.pos, feet)
    }
}

/// (v238) La foudre tombée en `at` blesse-t-elle ce qui se tient en `feet` (ses pieds) ? Pour
/// le joueur (`Strike::hurts`) comme pour les PNJ de la foule.
pub fn hurts_at(at: Vec3, feet: Vec3) -> bool {
    let d = feet - at;
    d.x.abs() <= DAMAGE_REACH
        && d.z.abs() <= DAMAGE_REACH
        && d.y >= -DAMAGE_BELOW
        && d.y <= DAMAGE_ABOVE
}

fn smooth(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// `dir` écarté de `angle` (rad) vers un côté au hasard.
fn tilt(dir: Vec3, angle: f32, rng: &mut Rng) -> Vec3 {
    let r = rng.dir();
    let side = (r - dir * r.dot(dir)).normalize_or(Vec3::X);
    (dir * angle.cos() + side * angle.sin()).normalize()
}

fn push_branch(out: &mut Vec<Piece>, pts: &[Vec3], s0: f32, total: f32, level: u8, wide: f32) {
    let len: f32 = pts
        .windows(2)
        .map(|w| (w[1] - w[0]).length())
        .sum::<f32>()
        .max(1e-3);
    let mut done = 0.0;
    for w in pts.windows(2) {
        let l = (w[1] - w[0]).length();
        out.push(Piece {
            a: w[0],
            b: w[1],
            sa: s0 + done / total,
            sb: s0 + (done + l) / total,
            level,
            ta: 1.0 - done / len,
            tb: 1.0 - (done + l) / len,
            wa: wide,
            wb: wide,
        });
        done += l;
    }
}

// ---------------------------------------------------------------- Foudre de Zeus

/// Un morceau de l'éclair tenu, dans son repère : en blocs, l'éclair le long de +y, le
/// poing à l'origine.
#[derive(Clone, Copy, Debug)]
pub struct Arc {
    pub a: Vec3,
    pub b: Vec3,
    pub force_a: f32,
    pub force_b: f32,
    /// Demi-largeur du halo (blocs).
    pub width_a: f32,
    pub width_b: f32,
    /// (v217) Part de violet dans son halo (0 : le bleu de la foudre).
    pub violet: f32,
}

/// Une étincelle : un point incandescent parti de l'éclair, qui file, ralentit et tombe.
/// Elle est dessinée comme un trait le long de son chemin récent.
#[derive(Clone, Copy, Debug)]
struct Spark {
    pos: Vec3,
    vel: Vec3,
    age: f32,
    life: f32,
    /// Points par où elle vient de passer (du plus récent au plus ancien), et le temps
    /// depuis le dernier relevé : la longueur de son trait.
    trail: [Vec3; 3],
    since: f32,
    every: f32,
    bright: f32,
}

/// L'éclair tenu en main : sa forme change sans cesse (le canal du milieu bouge par
/// saccades, des filaments courent autour, de courts arcs partent sur les côtés), et il
/// jette des étincelles.
pub struct HeldBolt {
    t: f32,
    sparks: Vec<Spark>,
    owed: f32,
    rng: Rng,
    /// (v217) Sursaut : l'éclair flambe un instant quand la foudre part (1 → 0).
    surge: f32,
}

impl Default for HeldBolt {
    fn default() -> Self {
        Self {
            t: 0.0,
            sparks: Vec::new(),
            owed: 0.0,
            rng: Rng::new(0x2e05),
            surge: 0.0,
        }
    }
}

/// Étincelles par seconde, et au plus autant en vol.
const SPARKS_PER_S: f32 = 46.0;
const MAX_SPARKS: usize = 96;
/// (v217) Avec les gerbes (foudre lancée, foudre tombée tout près).
const MAX_SPARKS_BURST: usize = 220;

impl HeldBolt {
    /// Le canal du milieu à l'instant `t` : du bas (−y) au haut de l'éclair, en passant
    /// par le poing. Il change 17 fois par seconde.
    fn core(t: f32) -> Vec<Vec3> {
        let mut rng = Rng::new((t * 17.0) as u64 ^ 0xc0_4e);
        let half = HELD_LEN * 0.5;
        // Dans le poing, il ne bouge presque pas.
        let grip = Vec3::new(rng.bell() * 0.008, -0.03, rng.bell() * 0.008);
        let low = jagged(Vec3::new(0.0, -half, 0.0), grip, 4, 0.3, &mut rng);
        let mut high = jagged(grip, Vec3::new(0.0, half, 0.0), 4, 0.3, &mut rng);
        let mut pts = low;
        pts.append(&mut high.split_off(1));
        pts
    }

    /// Avance de `dt` : les étincelles volent, de nouvelles partent de l'éclair. `place` :
    /// du repère de l'éclair vers le monde. `on` : l'éclair est tenu (sinon les étincelles
    /// déjà parties finissent leur course).
    pub fn update(&mut self, dt: f32, place: Mat4, on: bool) {
        self.t += dt;
        self.surge = (self.surge - dt / 0.22).max(0.0);
        // Par pas de 6 ms au plus : les traits ont la même longueur à toutes les cadences
        // (sinon une image longue les étirerait).
        let steps = (dt / 0.006).ceil().clamp(1.0, 40.0);
        let h = dt / steps;
        for s in &mut self.sparks {
            for _ in 0..steps as usize {
                s.age += h;
                s.since += h;
                if s.since >= s.every {
                    s.since = 0.0;
                    s.trail = [s.pos, s.trail[0], s.trail[1]];
                }
                // Légère, freinée par l'air, elle retombe.
                s.vel += Vec3::new(0.0, -6.5, 0.0) * h;
                s.vel *= (-3.2 * h).exp();
                s.pos += s.vel * h;
            }
        }
        self.sparks.retain(|s| s.age < s.life);
        if !on {
            self.owed = 0.0;
            return;
        }
        // Par bouffées : l'éclair crépite.
        let burst = 0.45 + 1.1 * ((self.t * 9.0).sin() * 0.5 + 0.5).powi(3);
        self.owed += SPARKS_PER_S * burst * dt;
        let core = Self::core(self.t);
        while self.owed >= 1.0 {
            self.owed -= 1.0;
            if self.sparks.len() >= MAX_SPARKS {
                continue;
            }
            let rng = &mut self.rng;
            // Elle part d'un point de l'éclair, dans le sens du morceau d'où elle vient
            // (elle prolonge sa courbure), un peu vers l'extérieur.
            let i = 1 + (rng.next() as usize) % (core.len() - 2);
            let along = (core[i + 1] - core[i - 1]).normalize_or(Vec3::Y)
                * if rng.unit() < 0.5 { 1.0 } else { -1.0 };
            let out = (core[i] * Vec3::new(1.0, 0.0, 1.0)).normalize_or(rng.dir());
            let local = (along * rng.range(0.8, 4.2) + out * rng.range(0.4, 2.6) + rng.dir() * 0.9)
                * (0.5 + 0.5 * HELD_SCALE);
            let pos = place.transform_point3(core[i]);
            let life = rng.range(0.1, 0.55);
            self.sparks.push(Spark {
                pos,
                vel: place.transform_vector3(local),
                age: 0.0,
                life,
                trail: [pos; 3],
                since: 0.0,
                // (Relevés plus ou moins espacés : des traits plus ou moins longs.)
                every: rng.range(0.004, 0.02),
                bright: rng.range(0.6, 1.5),
            });
        }
    }

    /// (v217) La foudre part : l'éclair flambe un instant et jette une gerbe d'étincelles
    /// depuis `tip` (monde), dans la direction `dir`.
    pub fn surge(&mut self, tip: Vec3, dir: Vec3) {
        self.surge = 1.0;
        self.burst(tip, dir, 26, 7.0);
    }

    /// (v217) Gerbe de `n` étincelles depuis `pos` (monde), surtout vers `dir`, à `speed`
    /// blocs par seconde environ : la foudre lancée, ou tombée tout près.
    pub fn burst(&mut self, pos: Vec3, dir: Vec3, n: usize, speed: f32) {
        for _ in 0..n {
            if self.sparks.len() >= MAX_SPARKS_BURST {
                break;
            }
            let rng = &mut self.rng;
            let vel = (dir * rng.range(0.2, 1.0) + rng.dir() * rng.range(0.4, 1.0)) * speed;
            self.sparks.push(Spark {
                pos,
                vel,
                age: 0.0,
                life: rng.range(0.15, 0.75),
                trail: [pos; 3],
                since: 0.0,
                every: rng.range(0.004, 0.02),
                bright: rng.range(0.7, 1.6),
            });
        }
    }

    /// L'éclair lui-même, dans son repère.
    pub fn arcs(&self, out: &mut Vec<Arc>) {
        Self::arcs_at(self.t, 1.0 + 0.9 * self.surge, out);
    }

    /// L'éclair à l'instant `t`, dans son repère (`boost` : son éclat, 1 d'ordinaire). Le
    /// même partout où on le voit : en main, (v217) en icône dans l'inventaire et posé au
    /// sol.
    pub fn arcs_at(t: f32, boost: f32, out: &mut Vec<Arc>) {
        let half = HELD_LEN * 0.5;
        // Scintillement : l'éclat saute 40 fois par seconde.
        let mut fl = Rng::new((t * 40.0) as u64 ^ 0xf1_1c);
        let flicker = fl.range(0.72, 1.3) * boost;
        // S'effile vers les deux pointes.
        let taper = |y: f32| (1.0 - (y.abs() / half).powi(3)).clamp(0.0, 1.0);
        let core = Self::core(t);
        for w in core.windows(2) {
            out.push(Arc {
                a: w[0],
                b: w[1],
                force_a: 1.9 * flicker * (0.35 + 0.65 * taper(w[0].y)),
                force_b: 1.9 * flicker * (0.35 + 0.65 * taper(w[1].y)),
                width_a: (0.03 + 0.05 * taper(w[0].y)) * HELD_HALO,
                width_b: (0.03 + 0.05 * taper(w[1].y)) * HELD_HALO,
                violet: 0.15,
            });
        }
        // Deux filaments courent autour du canal, plus fins, plus nerveux (31 fois par
        // seconde).
        let mut rng = Rng::new((t * 31.0) as u64 ^ 0xf11a);
        for _ in 0..2 {
            let (i, j) = (
                (rng.next() as usize) % (core.len() / 2),
                core.len() / 2 + (rng.next() as usize) % (core.len() / 2),
            );
            let path = jagged(core[i], core[j], 4, 0.3, &mut rng);
            let force = rng.range(0.35, 0.8) * flicker;
            for w in path.windows(2) {
                out.push(Arc {
                    a: w[0],
                    b: w[1],
                    force_a: force,
                    force_b: force,
                    width_a: 0.034 * HELD_HALO,
                    width_b: 0.034 * HELD_HALO,
                    violet: 0.8,
                });
            }
        }
        // De courts arcs partent sur les côtés et s'éteignent en pointe (23 fois par
        // seconde).
        let mut rng = Rng::new((t * 23.0) as u64 ^ 0xa4c5);
        for _ in 0..5 {
            if rng.unit() < 0.35 {
                continue;
            }
            let i = 1 + (rng.next() as usize) % (core.len() - 2);
            let along = (core[i + 1] - core[i - 1]).normalize_or(Vec3::Y)
                * if rng.unit() < 0.5 { 1.0 } else { -1.0 };
            let end = core[i]
                + tilt(along, rng.range(0.5, 1.25), &mut rng) * (rng.range(0.07, 0.3) * HELD_SCALE);
            let path = jagged(core[i], end, 3, 0.28, &mut rng);
            let n = (path.len() - 1) as f32;
            let force = rng.range(0.7, 1.3) * flicker;
            for (k, w) in path.windows(2).enumerate() {
                let (ta, tb) = (1.0 - k as f32 / n, 1.0 - (k + 1) as f32 / n);
                out.push(Arc {
                    a: w[0],
                    b: w[1],
                    force_a: force * ta,
                    force_b: force * tb,
                    width_a: (0.016 + 0.024 * ta) * HELD_HALO,
                    width_b: (0.016 + 0.024 * tb) * HELD_HALO,
                    violet: force.fract(),
                });
            }
        }
    }

    /// Les étincelles en vol, dans le monde : un trait le long du chemin récent de chacune.
    pub fn sparks(&self, out: &mut Vec<Arc>) {
        for s in &self.sparks {
            let k = 1.0 - s.age / s.life;
            // Vive au départ, elle refroidit.
            let force = s.bright * k.powf(0.7) * 1.3;
            let pts = [s.pos, s.trail[0], s.trail[1], s.trail[2]];
            for (i, w) in pts.windows(2).enumerate() {
                if (w[0] - w[1]).length_squared() < 1e-8 {
                    continue;
                }
                let (fa, fb) = (1.0 - i as f32 / 3.0, 1.0 - (i + 1) as f32 / 3.0);
                out.push(Arc {
                    a: w[0],
                    b: w[1],
                    force_a: force * fa,
                    force_b: force * fb,
                    width_a: 0.011 + 0.012 * fa,
                    width_b: 0.011 + 0.012 * fb,
                    violet: (s.bright * 3.7).fract() * 0.7,
                });
            }
        }
    }

    /// Éclat de l'éclair à cet instant (autour de 1) : sa lumière sur les murs scintille
    /// avec lui, et monte quand il crépite.
    /// (v222) Des étincelles volent encore (l'éclair rangé, elles finissent leur course).
    pub fn has_sparks(&self) -> bool {
        !self.sparks.is_empty()
    }

    pub fn glow(&self) -> f32 {
        let mut fl = Rng::new((self.t * 40.0) as u64 ^ 0xf1_1c);
        let flicker = fl.range(0.72, 1.3);
        flicker
            * (0.85 + 0.3 * (self.sparks.len() as f32 / MAX_SPARKS as f32).min(1.0))
            * (1.0 + 1.5 * self.surge)
    }
}

/// Segments à dessiner dans le monde : `arcs` placés par `place` (du repère des arcs vers
/// le monde ; l'identité pour des arcs déjà dans le monde).
pub fn in_world(arcs: &[Arc], place: Mat4, out: &mut Vec<BoltSeg>) {
    in_world_scaled(arcs, place, 1.0, out);
}

/// Comme `in_world`, pour un repère `place` qui agrandit ou réduit de `scale` (les halos
/// suivent) : (v217) la Foudre de Zeus posée au sol est le même éclair ((v235) de la même
/// taille qu'en main).
pub fn in_world_scaled(arcs: &[Arc], place: Mat4, scale: f32, out: &mut Vec<BoltSeg>) {
    for s in arcs {
        let (a, b) = (place.transform_point3(s.a), place.transform_point3(s.b));
        out.push(BoltSeg {
            a: [a.x, a.y, a.z, BOLT],
            b: [b.x, b.y, b.z, 1.0],
            color: tint(s.violet),
            force_a: s.force_a,
            force_b: s.force_b,
            width_a: s.width_a * scale,
            width_b: s.width_b * scale,
            screen: 0.0,
        });
    }
}

/// (v236) L'éclair en icône, penché en diagonale : un peu plus long que droit.
const ICON_LONGER: f32 = 1.2;

/// (v217) Horloge des icônes (s) : la même pour toutes, elle tourne toujours.
pub fn clock() -> f32 {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    START
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_secs_f32()
}

/// (v217) L'éclair tenu en main, en icône : centré en (`cx`, `cy`) (pixels), long de
/// `size` pixels, à l'instant `t` (il vit comme en main). Pour `Ui::bolts`. (v236) « En
/// diagonale, de en bas à gauche vers en haut à droite de sa case, plus droit » : penché de
/// 45°, et 20 % plus long (la diagonale de la case l'est de 41 %).
pub fn icon(t: f32, cx: f32, cy: f32, size: f32) -> Vec<BoltSeg> {
    let mut arcs = Vec::new();
    HeldBolt::arcs_at(t, 1.0, &mut arcs);
    let k = size * ICON_LONGER / HELD_LEN;
    // Le haut de l'éclair (+y) vers le haut à droite de l'écran (y vers le bas).
    let c = std::f32::consts::FRAC_1_SQRT_2;
    let at = |p: Vec3| [cx + (p.x + p.y) * c * k, cy - (p.y - p.x) * c * k, 0.0, 1.0];
    arcs.iter()
        .map(|s| BoltSeg {
            a: at(s.a),
            b: at(s.b),
            color: tint(s.violet),
            force_a: s.force_a,
            force_b: s.force_b,
            // (Un peu plus large qu'à l'échelle : une icône doit se lire.)
            width_a: (s.width_a * k * 1.5).max(1.6),
            width_b: (s.width_b * k * 1.5).max(1.6),
            screen: 1.0,
        })
        .collect()
}

/// Segments à dessiner à la première personne : `arcs` passés à l'écran par `mvp` (la
/// projection de la main comprise). La largeur des halos suit la distance à l'œil.
pub fn on_screen(arcs: &[Arc], mvp: Mat4, out: &mut Vec<BoltSeg>) {
    // Taille à l'écran d'un bloc à la distance 1 (moitié de la hauteur de l'écran = 1).
    let scale = Vec3::new(mvp.x_axis.y, mvp.y_axis.y, mvp.z_axis.y).length();
    for s in arcs {
        let (a, b) = (mvp * s.a.extend(1.0), mvp * s.b.extend(1.0));
        if a.w <= 1e-3 || b.w <= 1e-3 {
            continue;
        }
        out.push(BoltSeg {
            a: a.to_array(),
            b: b.to_array(),
            color: tint(s.violet),
            force_a: s.force_a,
            force_b: s.force_b,
            // (En part de la hauteur de l'écran.)
            width_a: s.width_a * scale / a.w * 0.5,
            width_b: s.width_b * scale / b.w * 0.5,
            screen: 1.0,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// (v236) L'icône de la Foudre de Zeus est penchée : du bas à gauche au haut à droite.
    #[test]
    fn the_bolt_icon_goes_from_bottom_left_to_top_right() {
        for t in [0.0, 0.3, 1.7] {
            let segs = icon(t, 50.0, 50.0, 16.0);
            // Corrélation entre « vers la droite » et « vers le haut » (y de l'écran vers
            // le bas) : forte et positive, du bas à gauche au haut à droite.
            let (mut xy, mut xx, mut yy) = (0.0, 0.0, 0.0);
            for p in segs.iter().flat_map(|s| [s.a, s.b]) {
                let (dx, dy) = (p[0] - 50.0, 50.0 - p[1]);
                xy += dx * dy;
                xx += dx * dx;
                yy += dy * dy;
            }
            let r = xy / (xx * yy).sqrt().max(1e-6);
            assert!(r > 0.8, "en diagonale : corrélation {r}");
        }
    }

    /// Un éclair relie le nuage au point frappé d'un seul tenant, ne dépend que de sa
    /// graine, et ses branches partent du canal vers le bas.
    #[test]
    fn a_strike_is_one_channel_from_the_cloud_to_the_ground() {
        let pos = Vec3::new(12.0, 70.0, -40.0);
        let s = Strike::new(pos, 77, 420.0, 0.0);
        let main: Vec<&Piece> = s.pieces.iter().filter(|p| p.level == 0).collect();
        assert_eq!(main.len(), 128);
        assert!(
            (main[0].a.y - 420.0).abs() < 1e-3,
            "part de la base des nuages"
        );
        assert!(
            (main[127].b - pos).length() < 1e-3,
            "arrive au point frappé"
        );
        for w in main.windows(2) {
            assert!((w[0].b - w[1].a).length() < 1e-4, "d'un seul tenant");
            assert!(w[1].sa >= w[0].sa);
        }
        assert!((main[127].sb - 1.0).abs() < 1e-3);
        // Ni trop droit, ni emmêlé : 15 à 80 % plus long que la ligne droite.
        let straight = (main[0].a - pos).length();
        let len: f32 = main.iter().map(|p| (p.b - p.a).length()).sum();
        assert!(
            len > straight * 1.15 && len < straight * 1.8,
            "{len} pour {straight}"
        );
        // Des branches, attachées au canal, qui descendent.
        let branches: Vec<&Piece> = s.pieces.iter().filter(|p| p.level == 1).collect();
        assert!(
            branches.len() >= 3 * 32,
            "{} morceaux de branche",
            branches.len()
        );
        let roots: Vec<&&Piece> = branches.iter().filter(|p| p.ta == 1.0).collect();
        for r in &roots {
            assert!(
                main.iter().any(|m| (m.a - r.a).length() < 1e-4),
                "une branche part du canal"
            );
        }
        let down = branches.iter().filter(|p| p.b.y < p.a.y).count();
        assert!(down * 10 > branches.len() * 6, "elles descendent");
        // Même graine : même éclair ; autre graine : un autre.
        let again = Strike::new(pos, 77, 420.0, 0.0);
        assert_eq!(again.pieces.len(), s.pieces.len());
        assert!(again.pieces.iter().zip(&s.pieces).all(|(a, b)| a.b == b.b));
        let other = Strike::new(pos, 78, 420.0, 0.0);
        assert!(other.pieces.iter().zip(&s.pieces).any(|(a, b)| a.b != b.b));
    }

    /// Déroulé : le traceur descend (rien n'arrive au sol avant lui), l'arc en retour
    /// illumine tout d'un coup, le canal se rallume à chaque arc, puis tout s'éteint.
    #[test]
    fn the_leader_comes_down_then_the_return_strokes_flash() {
        let pos = Vec3::new(0.0, 64.0, 0.0);
        let mut s = Strike::new(pos, 5, 400.0, 0.2);
        let mut segs = Vec::new();
        s.segments(Vec3::new(900.0, 64.0, 0.0), &mut segs);
        assert!(segs.is_empty() && s.light() == 0.0, "pas encore parti");
        assert!(!s.update(0.21));
        // Traceur à mi-chemin : rien près du sol, sa lumière est faible.
        s.age = s.leader_s * 0.5;
        s.segments(Vec3::new(900.0, 64.0, 0.0), &mut segs);
        assert!(!segs.is_empty());
        let lowest = segs.iter().map(|g| g.b[1]).fold(f32::MAX, f32::min);
        assert!(lowest > pos.y + 60.0, "le traceur n'est qu'à {lowest}");
        assert!(s.light() < 0.06);
        let leader_peak = segs.iter().map(|g| g.force_a).fold(0.0, f32::max);
        // Il touche : le premier arc.
        let mut landed = 0;
        let mut peak = 0.0f32;
        let mut relit = 0;
        let mut last = 0.0;
        for _ in 0..400 {
            landed += usize::from(s.update(0.002));
            let l = s.light();
            peak = peak.max(l);
            // (Une remontée franche de la lumière : un nouvel arc.)
            if l > last + 0.15 {
                relit += 1;
            }
            last = l;
        }
        assert_eq!(landed, 1, "il ne touche qu'une fois");
        assert!(peak > 0.9);
        assert!((2..=4).contains(&relit), "{relit} arcs");
        assert_eq!(relit, s.strokes.len());
        segs.clear();
        s.age = s.leader_s + 0.004;
        s.segments(Vec3::new(900.0, 64.0, 0.0), &mut segs);
        let main_peak = segs.iter().map(|g| g.force_a).fold(0.0, f32::max);
        assert!(main_peak > 2.5 && main_peak > leader_peak * 2.0);
        assert!(
            segs.iter().any(|g| (g.b[1] - pos.y).abs() < 1e-3),
            "jusqu'au sol"
        );
        // Fini : plus rien.
        s.age = 5.0;
        assert!(s.done() && s.light() < 1e-3);
        segs.clear();
        s.segments(Vec3::new(900.0, 64.0, 0.0), &mut segs);
        assert!(segs.is_empty());
    }

    /// (v237) Vu de près, l'éclair garde le dessin qu'il a aux distances moyennes : ses
    /// morceaux droits de plusieurs blocs sont brisés plus fin (aucun ne barre l'image d'un
    /// long trait), il touche toujours le même point, il ne tremble pas d'une image à
    /// l'autre, et de loin rien ne change.
    #[test]
    fn a_strike_seen_from_close_is_broken_finer_and_the_same_from_afar() {
        let pos = Vec3::new(0.0, 64.0, 0.0);
        let mut s = Strike::new(pos, 21, 420.0, 0.0);
        s.age = s.leader_s + 0.004;
        let seen = |eye: Vec3| {
            let mut segs = Vec::new();
            s.segments(eye, &mut segs);
            segs
        };
        let ends = |g: &BoltSeg| (Vec3::from_slice(&g.a[..3]), Vec3::from_slice(&g.b[..3]));
        // Le plus long morceau à moins de 30 blocs de l'œil, en part de sa distance à lui.
        let longest = |eye: Vec3, pieces: &[(Vec3, Vec3)]| {
            pieces
                .iter()
                .filter_map(|&(a, b)| {
                    let ab = b - a;
                    let t = ((eye - a).dot(ab) / ab.length_squared().max(1e-9)).clamp(0.0, 1.0);
                    let d = (a + ab * t - eye).length();
                    (3.0..30.0).contains(&d).then(|| ab.length() / d)
                })
                .fold(0.0, f32::max)
        };
        // De loin : ses morceaux tels quels, d'où qu'on le voie.
        let far = seen(Vec3::new(600.0, 64.0, 0.0));
        let farther = seen(Vec3::new(3000.0, 64.0, 0.0));
        assert_eq!(far.len(), farther.len());
        assert!(far.iter().zip(&farther).all(|(a, b)| ends(a) == ends(b)));
        // À cinq blocs du point frappé.
        let eye = pos + Vec3::new(5.0, 1.6, 0.0);
        let raw: Vec<_> = s.pieces.iter().map(|p| (p.a, p.b)).collect();
        assert!(longest(eye, &raw) > 0.4, "{}", longest(eye, &raw));
        let near = seen(eye);
        let drawn: Vec<_> = near.iter().map(ends).collect();
        assert!(near.len() > far.len() + 60, "{} {}", near.len(), far.len());
        assert!(longest(eye, &drawn) < 0.3, "{}", longest(eye, &drawn));
        assert!(drawn.iter().any(|(_, b)| (*b - pos).length() < 1e-3), "jusqu'au sol");
        // (Les filaments et les arcs courts changent des dizaines de fois par seconde, pas
        // le canal ni ses branches : au même instant, la même image.)
        let again = seen(eye);
        assert_eq!(again.len(), near.len());
        assert!(again.iter().zip(&near).all(|(a, b)| ends(a) == ends(b)));
        // En s'approchant, les zigzags déjà là restent : seuls de plus fins s'ajoutent.
        let (a, b) = (Vec3::new(0.0, 0.0, 0.0), Vec3::new(4.0, 0.0, 0.0));
        let coarse = jagged(a, b, 2, FINE_ROUGH, &mut Rng::new(9));
        let fine = jagged(a, b, 4, FINE_ROUGH, &mut Rng::new(9));
        assert!(coarse.iter().enumerate().all(|(i, p)| fine[i * 4] == *p));
        assert_eq!(fine_levels(a, b, Vec3::new(2.0, 0.0, 41.0)), 0);
        assert_eq!(fine_levels(a, b, Vec3::new(2.0, 0.0, 39.0)), 1);
        assert_eq!(fine_levels(a, b, Vec3::new(2.0, 0.0, 20.0)), 2);
        assert_eq!(fine_levels(a, b, Vec3::new(2.0, 0.0, 0.1)), FINE_LEVELS);
    }

    /// (v237) Celle qu'on lance vers le ciel : ses lumières sont le long d'elle, loin de la
    /// main, et l'endroit où elle allume les nuages est du côté où on la lance.
    #[test]
    fn a_bolt_thrown_at_the_sky_lights_from_where_it_goes() {
        let from = Vec3::new(0.0, 66.0, 0.0);
        let dir = Vec3::new(0.6, 0.5, -0.3).normalize();
        let s = Strike::up(from, dir, 8, 420.0, 180.0);
        let [low, high] = s.light_points();
        let along = |p: Vec3| (p - from).dot(dir);
        assert!((along(low) - 12.0).abs() < 4.0, "{}", along(low));
        assert!((along(high) - 45.0).abs() < 8.0, "{}", along(high));
        let glow = s.cloud_entry();
        assert!(glow.y > 420.0 && glow.y < 600.0, "{glow}");
        assert!((glow - from).normalize().dot(dir) > 0.97);
        // La foudre qui tombe garde les siennes près du point frappé.
        let g = Strike::new(from, 8, 420.0, 0.0);
        let [low, high] = g.light_points();
        assert!((low - from).length() < 5.0 && (high - from).length() < 28.0);
    }

    /// (v241) L'arc de la Foudre de Zeus sans charge : de la pointe à la cible, tout droit
    /// (il ne monte pas au ciel), fin, bref, et il ne blesse pas comme la foudre qui tombe.
    #[test]
    fn the_short_arc_goes_straight_from_the_tip_to_the_target() {
        let (from, to) = (Vec3::new(0.0, 66.0, 0.0), Vec3::new(6.0, 65.5, 2.0));
        let mut s = Strike::arc(from, to, 3);
        assert_eq!(s.kind, Kind::Arc);
        let main: Vec<_> = s.pieces.iter().take_while(|p| p.level == 0).collect();
        assert!(main[0].a == from && main[main.len() - 1].b == to);
        let span = (to - from).length();
        // Aucun morceau (canal, rameaux) ne s'écarte de plus de la moitié de sa longueur.
        for p in &s.pieces {
            let t = ((p.b - from).dot(to - from) / (span * span)).clamp(0.0, 1.0);
            let off = (from + (to - from) * t - p.b).length();
            assert!(off < span * 0.5, "{off}");
            assert!(p.wa <= 0.2 && p.wb <= 0.2, "fin d'un bout à l'autre");
        }
        assert!(!s.hurts(to) && !s.hurts(to - Vec3::Y));
        let mut landed = 0;
        let mut segs = Vec::new();
        for _ in 0..600 {
            landed += usize::from(s.update(0.002));
            s.segments(from + Vec3::new(0.0, 0.0, 3.0), &mut segs);
        }
        assert_eq!(landed, 1);
        assert!(!segs.is_empty() && s.done());
        let top = segs.iter().map(|g| g.a[1].max(g.b[1])).fold(f32::MIN, f32::max);
        assert!(top < 72.0, "il ne passe pas par le ciel : {top}");
        // (Aucune lueur dans les nuages.)
        assert!(segs.iter().all(|g| g.a[3] == BOLT));
    }

    /// Dégâts comme dans Minecraft : dans la boîte de 3 blocs autour du point frappé.
    #[test]
    fn lightning_hurts_what_stands_close() {
        let s = Strike::new(Vec3::new(10.0, 64.0, 10.0), 1, 400.0, 0.0);
        assert_eq!(DAMAGE, 5);
        assert!(s.hurts(Vec3::new(10.5, 64.0, 9.0)));
        assert!(s.hurts(Vec3::new(12.9, 64.0, 7.1)));
        assert!(s.hurts(Vec3::new(10.0, 61.5, 10.0)) && s.hurts(Vec3::new(10.0, 72.0, 10.0)));
        assert!(!s.hurts(Vec3::new(13.2, 64.0, 10.0)));
        assert!(!s.hurts(Vec3::new(10.0, 60.0, 10.0)) && !s.hurts(Vec3::new(10.0, 74.0, 10.0)));
    }

    /// L'éclair tenu fait la moitié de la taille du joueur, passe par le poing, change de
    /// forme sans cesse, et ses arcs de côté restent près de lui.
    #[test]
    fn the_held_bolt_is_half_a_player_tall_and_alive() {
        let mut h = HeldBolt::default();
        let mut shapes = std::collections::HashSet::new();
        let mut flickers = Vec::new();
        for _ in 0..120 {
            h.update(1.0 / 120.0, Mat4::IDENTITY, true);
            let core = HeldBolt::core(h.t);
            assert_eq!(core.len(), 33);
            let half = HELD_LEN * 0.5;
            assert!((core[0].y + half).abs() < 1e-5 && (core[32].y - half).abs() < 1e-5);
            assert!((core[32].y - core[0].y - HELD_LEN).abs() < 1e-5);
            // (v217) 40 % de plus que la moitié de 1,8 bloc.
            assert!((HELD_LEN - 0.9 * 1.4).abs() < 1e-6);
            assert!(core[16].length() < 0.05, "passe par le poing");
            assert!(core.iter().all(|p| Vec3::new(p.x, 0.0, p.z).length() < 0.42));
            shapes.insert(core[8].x.to_bits());
            let mut arcs = Vec::new();
            h.arcs(&mut arcs);
            assert!(arcs.len() >= 32);
            for a in &arcs {
                assert!(a.a.length() < 1.15 && a.b.length() < 1.15, "près de l'éclair");
                assert!(a.force_a >= 0.0 && a.force_a < 3.0 && a.width_a > 0.0);
            }
            flickers.push(h.glow());
        }
        assert!(shapes.len() >= 12, "{} formes en une seconde", shapes.len());
        let (lo, hi) = flickers
            .iter()
            .fold((f32::MAX, 0.0f32), |(l, h), v| (l.min(*v), h.max(*v)));
        assert!(hi > lo * 1.3, "il scintille");
    }

    /// Les étincelles partent de l'éclair, plus ou moins vite et plus ou moins loin, dans
    /// le sens du morceau d'où elles viennent ; elles ralentissent, retombent et
    /// s'éteignent ; leurs traits sont plus ou moins longs. Lâché, l'éclair n'en fait plus.
    #[test]
    fn sparks_fly_off_the_bolt_and_die() {
        let place = Mat4::from_translation(Vec3::new(100.0, 70.0, -20.0));
        let origin = place.transform_point3(Vec3::ZERO);
        let mut h = HeldBolt::default();
        let mut born = 0;
        let (mut far, mut lengths) = (0.0f32, Vec::new());
        for _ in 0..600 {
            let before = h.sparks.len();
            h.update(1.0 / 240.0, place, true);
            for s in h.sparks.iter().skip(before) {
                assert!((s.pos - origin).length() < 0.95, "née sur l'éclair");
                born += 1;
            }
            for s in &h.sparks {
                far = far.max((s.pos - origin).length());
                assert!(s.age < s.life && s.life <= 0.55);
            }
            let mut arcs = Vec::new();
            h.sparks(&mut arcs);
            lengths.extend(arcs.iter().map(|a| (a.a - a.b).length()));
            assert!(h.sparks.len() <= MAX_SPARKS);
        }
        // 2,5 s : une centaine d'étincelles, restées autour de la main.
        assert!((60..220).contains(&born), "{born} étincelles");
        assert!(far > 0.5 && far < 3.0, "la plus lointaine à {far}");
        let (short, long) = lengths
            .iter()
            .fold((f32::MAX, 0.0f32), |(s, l), v| (s.min(*v), l.max(*v)));
        assert!(
            long > 0.05 && long > short * 8.0,
            "traits de {short} à {long}"
        );
        // Lâché : elles finissent leur course, plus aucune ne part.
        for _ in 0..200 {
            let before = h.sparks.len();
            h.update(1.0 / 240.0, place, false);
            assert!(h.sparks.len() <= before);
        }
        assert!(h.sparks.is_empty());
    }

    /// À l'écran ou dans le monde : les mêmes arcs, la même teinte que la foudre du ciel.
    #[test]
    fn held_arcs_keep_the_colour_of_lightning() {
        let mut h = HeldBolt::default();
        h.update(0.01, Mat4::IDENTITY, true);
        let mut arcs = Vec::new();
        h.arcs(&mut arcs);
        let place = Mat4::from_translation(Vec3::new(3.0, 4.0, 5.0));
        let (mut world, mut screen) = (Vec::new(), Vec::new());
        in_world(&arcs, place, &mut world);
        let proj = Mat4::perspective_infinite_reverse_rh(70f32.to_radians(), 16.0 / 9.0, 0.01);
        let view = Mat4::from_translation(Vec3::new(0.3, -0.3, -0.9));
        on_screen(&arcs, proj * view, &mut screen);
        assert_eq!(world.len(), arcs.len());
        assert_eq!(screen.len(), arcs.len());
        let mut sky = Vec::new();
        let mut s = Strike::new(Vec3::ZERO, 3, 300.0, 0.0);
        s.age = s.leader_s + 0.01;
        s.segments(Vec3::new(900.0, 64.0, 0.0), &mut sky);
        // (v217) Le bleu de la foudre, avec au plus une touche de violet : jamais d'autre
        // teinte, et le bleu reste entier.
        assert!(world.iter().chain(&screen).chain(&sky).all(|g| {
            g.color[2] == 1.0
                && (BOLT_TINT[0]..=0.68 + 1e-4).contains(&g.color[0])
                && (0.4 - 1e-4..=BOLT_TINT[1]).contains(&g.color[1])
        }));
        assert!((world[0].a[0] - arcs[0].a.x - 3.0).abs() < 1e-5 && world[0].screen == 0.0);
        // À l'écran : devant l'œil, et un halo de quelques centièmes de la hauteur.
        for g in &screen {
            assert!(g.screen == 1.0 && g.a[3] > 0.3 && g.a[3] < 1.6);
            assert!(g.width_a > 0.004 && g.width_a < 0.12, "{}", g.width_a);
        }
    }
}
