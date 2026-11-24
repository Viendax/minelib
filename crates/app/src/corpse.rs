//! (v299) Les corps qui tombent : « L'Évidé, comme le joueur ou un PNJ, tombe par rapport aux
//! blocs autour de lui ; fais une physique pour ça, qui se fige assez rapidement ». Un mort
//! s'effondre là où il est : un pantin de quatorze points (tête, cou, épaules, coudes, mains,
//! hanches, genoux, pieds) reliés par des tiges de longueur fixe. Chaque point tombe, s'appuie
//! sur les blocs (leur forme réelle : dalles, escaliers, clôtures) et y frotte ; le corps se
//! couche sur le sol, pend d'un rebord, s'adosse à un mur. Dès que plus rien ne bouge (ou au
//! bout de `MAX_S` secondes), il est figé : plus aucun calcul.
//!
//! Le corps est dessiné avec les pavés de son modèle : chaque morceau rigide (le haut et le
//! bas de chaque membre, le tronc, la tête) garde, dans le repère de ses points, la place qu'il
//! avait au moment de tomber.

use glam::{Mat4, Vec3};
use minelib_world::World;

/// Les pavés d'un corps, dans l'ordre du modèle du joueur : jambe gauche, jambe droite,
/// poitrine, bras gauche, bras droit, tête, ventre.
pub const PARTS: usize = 7;

/// La posture d'un corps au moment où il tombe : pour chaque pavé, son repère (pixels →
/// monde) et ses coins (pixels) ; pour un membre, le repère de sa moitié du bas (sous le
/// genou, sous le coude).
pub struct Stance {
    pub parts: [(Mat4, [f32; 3], [f32; 3]); PARTS],
    pub lower: [Option<Mat4>; PARTS],
}

/// Ce qui coule d'un corps : du sang, la gelée de L'Évidé, ce que portent ses veines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Liquid {
    Blood,
    Jelly,
    Vein,
}

const N: usize = 14;
const HEAD: usize = 0;
const NECK: usize = 1;
const SH_L: usize = 2;
const SH_R: usize = 3;
const EL_L: usize = 4;
const EL_R: usize = 5;
const HA_L: usize = 6;
const HA_R: usize = 7;
const HIP_L: usize = 8;
const HIP_R: usize = 9;
const KN_L: usize = 10;
const KN_R: usize = 11;
const FT_L: usize = 12;
const FT_R: usize = 13;

/// Pas du calcul (s), pesanteur (blocs/s²), ce que chaque point garde de sa vitesse d'un pas
/// au suivant, demi-épaisseur d'un point (blocs), part de sa vitesse le long d'un bloc qu'il
/// perd à chaque pas en le touchant.
const STEP: f32 = 1.0 / 90.0;
const GRAVITY: f32 = 24.0;
const KEEP: f32 = 0.992;
const THICK: f32 = 0.1;
const RUB: f32 = 0.35;
/// Figé quand aucun point ne dépasse `CALM` blocs/s pendant `CALM_S` secondes, ou au bout de
/// `MAX_S` secondes.
const CALM: f32 = 0.3;
const CALM_S: f32 = 0.25;
pub const MAX_S: f32 = 3.0;

/// Tiges : le tronc (hanches, épaules, cou : un seul bloc rigide), la tête, les membres.
const LINKS: [(usize, usize); 22] = [
    (HIP_L, HIP_R),
    (SH_L, SH_R),
    (HIP_L, SH_L),
    (HIP_R, SH_R),
    (HIP_L, SH_R),
    (HIP_R, SH_L),
    (NECK, SH_L),
    (NECK, SH_R),
    (NECK, HIP_L),
    (NECK, HIP_R),
    (NECK, HEAD),
    (HEAD, SH_L),
    (HEAD, SH_R),
    (HEAD, HIP_L),
    (SH_L, EL_L),
    (EL_L, HA_L),
    (SH_R, EL_R),
    (EL_R, HA_R),
    (HIP_L, KN_L),
    (KN_L, FT_L),
    (HIP_R, KN_R),
    (KN_R, FT_R),
];
/// Un corps ne se replie pas en tas : les deux bouts d'un membre restent écartés d'au moins
/// cette part de sa longueur (les bras se plient bien, les genoux moins) ; de même l'épaule
/// et le genou d'un même côté (il ne se plie guère aux hanches) : il bascule et s'allonge.
const SPREAD: [(usize, usize, usize, f32); 6] = [
    (SH_L, EL_L, HA_L, 0.45),
    (SH_R, EL_R, HA_R, 0.45),
    (HIP_L, KN_L, FT_L, 0.8),
    (HIP_R, KN_R, FT_R, 0.8),
    (SH_L, HIP_L, KN_L, 0.86),
    (SH_R, HIP_R, KN_R, 0.86),
];
/// Poussé de moins que ça (blocs/s, à plat), il s'effondre quand même d'un côté, de tant.
const PUSH_MIN: f32 = 1.0;
const PUSH_OWN: f32 = 1.3;

/// Un corps qui tombe (voir le haut du fichier).
#[derive(Clone, Debug)]
pub struct Ragdoll {
    p: [Vec3; N],
    q: [Vec3; N],
    rest: [f32; 22],
    spread: [f32; 6],
    /// Pour chaque pavé, la place de son haut et de son bas dans le repère de leurs points,
    /// et l'axe de côté de ce repère (gardé d'un pas au suivant : un membre ne vrille pas
    /// d'un coup).
    local: [[Mat4; 2]; PARTS],
    side: [[Vec3; 2]; PARTS],
    age: f32,
    calm: f32,
    carry: f32,
    still: bool,
}

/// Repère d'origine `o`, d'axe y vers `up`, d'axe x au plus près de `side`.
fn frame(o: Vec3, up: Vec3, side: Vec3) -> (Mat4, Vec3) {
    let y = up.normalize_or(Vec3::Y);
    let mut x = side - y * side.dot(y);
    if x.length_squared() < 1e-6 {
        x = y.any_orthonormal_vector();
    }
    let x = x.normalize();
    let z = x.cross(y);
    (
        Mat4::from_cols(x.extend(0.0), y.extend(0.0), z.extend(0.0), o.extend(1.0)),
        x,
    )
}

impl Ragdoll {
    /// Le corps dans la posture `s`, lancé à la vitesse `vel` (celle qu'il avait), poussé de
    /// `push` (blocs/s, ce qui l'a tué : le haut du corps part plus fort que le bas) ; `seed` :
    /// un rien de hasard, pour qu'un corps bien droit ne reste pas debout.
    pub fn new(s: &Stance, vel: Vec3, push: Vec3, seed: u32) -> Self {
        let at = |i: usize, low: bool, y: f32| {
            let (xf, _, _) = s.parts[i];
            let xf = if low { s.lower[i].unwrap_or(xf) } else { xf };
            xf.transform_point3(Vec3::new(0.0, y, 0.0))
        };
        let mid = |i: usize| (s.parts[i].1[1] + s.parts[i].2[1]) / 2.0;
        let (lo, hi) = (|i: usize| s.parts[i].1[1], |i: usize| s.parts[i].2[1]);
        let mut p = [Vec3::ZERO; N];
        // Jambes : la hanche est l'origine du pavé, le pied son bas.
        for (part, hip, knee, foot) in [(0, HIP_L, KN_L, FT_L), (1, HIP_R, KN_R, FT_R)] {
            p[hip] = at(part, false, hi(part));
            p[knee] = at(part, false, mid(part));
            p[foot] = at(part, true, lo(part));
        }
        // Bras : l'épaule est l'origine du pavé (il dépasse un peu au-dessus).
        for (part, sh, el, ha) in [(3, SH_L, EL_L, HA_L), (4, SH_R, EL_R, HA_R)] {
            p[sh] = at(part, false, 0.0);
            p[el] = at(part, false, mid(part));
            p[ha] = at(part, true, lo(part));
        }
        p[NECK] = at(5, false, lo(5));
        p[HEAD] = at(5, false, hi(5));
        let mut rng = seed | 1;
        let mut unit = || {
            rng ^= rng << 13;
            rng ^= rng >> 17;
            rng ^= rng << 5;
            (rng >> 8) as f32 / (1u32 << 24) as f32 - 0.5
        };
        // Rien ne l'a poussé : il s'effondre quand même d'un côté (un corps bien droit ne
        // reste pas debout).
        let push = if Vec3::new(push.x, 0.0, push.z).length() < PUSH_MIN {
            let a = unit() * std::f32::consts::TAU;
            push + Vec3::new(a.cos(), 0.0, a.sin()) * PUSH_OWN
        } else {
            push
        };
        // Le haut du corps part plus fort : il bascule.
        let top = p[HEAD].y.max(p[NECK].y);
        let bottom = p[FT_L].y.min(p[FT_R].y);
        let mut q = p;
        for i in 0..N {
            let h = ((p[i].y - bottom) / (top - bottom).max(0.1)).clamp(0.0, 1.0);
            let jitter = Vec3::new(unit(), 0.0, unit()) * 0.5;
            let v = vel + push * (0.3 + 0.9 * h) + jitter;
            q[i] = p[i] - v * STEP;
        }
        let mut rag = Self {
            p,
            q,
            rest: [0.0; 22],
            spread: [0.0; 6],
            local: [[Mat4::IDENTITY; 2]; PARTS],
            side: [[Vec3::X; 2]; PARTS],
            age: 0.0,
            calm: 0.0,
            carry: 0.0,
            still: false,
        };
        for (k, (a, b)) in LINKS.into_iter().enumerate() {
            rag.rest[k] = (p[a] - p[b]).length();
        }
        for (k, (a, m, b, part)) in SPREAD.into_iter().enumerate() {
            let len = (p[a] - p[m]).length() + (p[m] - p[b]).length();
            rag.spread[k] = (len * part).min((p[a] - p[b]).length());
        }
        // La place de chaque morceau dans le repère de ses points, tel qu'il est maintenant.
        for i in 0..PARTS {
            let (xf, _, _) = s.parts[i];
            for (k, m) in [xf, s.lower[i].unwrap_or(xf)].into_iter().enumerate() {
                rag.side[i][k] = m.x_axis.truncate().normalize_or(Vec3::X);
                let (basis, side) = rag.basis(i, k == 1);
                rag.side[i][k] = side;
                rag.local[i][k] = basis.inverse() * m;
            }
        }
        rag
    }

    /// Le repère des points du morceau (`low` : le bas d'un membre) et son axe de côté.
    fn basis(&self, part: usize, low: bool) -> (Mat4, Vec3) {
        let p = &self.p;
        let side = self.side[part][low as usize];
        let limb = |a: usize, m: usize, b: usize| {
            if low {
                frame(p[m], p[m] - p[b], side)
            } else {
                frame(p[a], p[a] - p[m], side)
            }
        };
        match part {
            0 => limb(HIP_L, KN_L, FT_L),
            1 => limb(HIP_R, KN_R, FT_R),
            3 => limb(SH_L, EL_L, HA_L),
            4 => limb(SH_R, EL_R, HA_R),
            5 => frame(p[NECK], p[HEAD] - p[NECK], side),
            _ => {
                let (hips, sh) = ((p[HIP_L] + p[HIP_R]) / 2.0, (p[SH_L] + p[SH_R]) / 2.0);
                frame(hips, sh - hips, (p[HIP_R] - p[HIP_L]) + (p[SH_R] - p[SH_L]))
            }
        }
    }

    /// Les repères (pixels → monde) du haut et du bas de chaque pavé, là où le corps est.
    pub fn frames(&self) -> [[Mat4; 2]; PARTS] {
        let mut out = [[Mat4::IDENTITY; 2]; PARTS];
        for (i, pair) in out.iter_mut().enumerate() {
            for (k, m) in pair.iter_mut().enumerate() {
                *m = self.basis(i, k == 1).0 * self.local[i][k];
            }
        }
        out
    }

    /// Il ne bouge plus.
    pub fn still(&self) -> bool {
        self.still
    }

    /// Le milieu du corps (entre les hanches et les épaules).
    pub fn middle(&self) -> Vec3 {
        (self.p[HIP_L] + self.p[HIP_R] + self.p[SH_L] + self.p[SH_R]) / 4.0
    }

    /// Un pas de sa chute. Figé : rien.
    pub fn update(&mut self, world: &World, dt: f32) {
        if self.still || dt <= 0.0 {
            return;
        }
        self.age += dt;
        self.carry += dt.min(0.1);
        while self.carry >= STEP {
            self.carry -= STEP;
            self.step(world);
        }
        for i in 0..PARTS {
            for k in 0..2 {
                self.side[i][k] = self.basis(i, k == 1).1;
            }
        }
        if self.calm >= CALM_S || self.age >= MAX_S {
            self.still = true;
        }
    }

    fn step(&mut self, world: &World) {
        let fall = Vec3::NEG_Y * (GRAVITY * STEP * STEP);
        for i in 0..N {
            let v = (self.p[i] - self.q[i]) * KEEP;
            self.q[i] = self.p[i];
            self.p[i] += v + fall;
        }
        for _ in 0..6 {
            for (k, (a, b)) in LINKS.into_iter().enumerate() {
                let d = self.p[b] - self.p[a];
                let len = d.length();
                if len > 1e-5 {
                    let fix = d * ((len - self.rest[k]) / len * 0.5);
                    self.p[a] += fix;
                    self.p[b] -= fix;
                }
            }
            for (k, (a, _, b, _)) in SPREAD.into_iter().enumerate() {
                let d = self.p[b] - self.p[a];
                let len = d.length();
                if len > 1e-5 && len < self.spread[k] {
                    let fix = d * ((len - self.spread[k]) / len * 0.5);
                    self.p[a] += fix;
                    self.p[b] -= fix;
                }
            }
            for i in 0..N {
                let (mut p, mut q) = (self.p[i], self.q[i]);
                push_out(world, &mut p, &mut q);
                (self.p[i], self.q[i]) = (p, q);
            }
        }
        let fastest = (0..N)
            .map(|i| (self.p[i] - self.q[i]).length() / STEP)
            .fold(0.0f32, f32::max);
        if fastest < CALM {
            self.calm += STEP;
        } else {
            self.calm = 0.0;
        }
    }
}

/// (v299) « Quand un joueur ou un PNJ meurt, pareil : il tombe au sol, son corps se transforme
/// en sang au sol ; seuls ses ossements restent au sol 5 minutes, en plus du sang » (en
/// construction). Le corps tombe (`Ragdoll`) ; figé (ou au bout de `BLOOD_WAIT` secondes), sa
/// chair fond en `BLOOD_S` secondes, chaque cube en gouttes de sang qui tachent le sol
/// dessous ; ses os restent `BONES_S` secondes.
pub struct Fallen {
    rag: Ragdoll,
    flesh: crate::flesh::Flesh,
    skin: Option<std::sync::Arc<crate::skin::Skin>>,
    age: f32,
    melt: f32,
    /// Son maillage, gardé tant que rien ne change (il ne bouge plus, il a fini de fondre) :
    /// son numéro de lot et sa version pour la carte graphique.
    id: u64,
    version: u64,
    mesh: Vec<minelib_render::EntityVertex>,
    lit: [f32; 3],
    dirty: bool,
}

pub const BONES_S: f32 = 300.0;
const BLOOD_WAIT: f32 = 1.6;
const BLOOD_S: f32 = 6.0;
/// Au plus tant de gouttes par image pendant que la chair fond.
const BLOOD_DROPS: usize = 6;

impl Fallen {
    /// Le corps dans la posture `stance` (voir `Ragdoll::new`), avec ce qu'il lui reste de
    /// chair (`flesh` : ce que L'Évidé lui avait mangé manque déjà) et son skin ; `id` : son
    /// numéro de lot parmi les entités fixes.
    pub fn new(
        stance: &Stance,
        vel: Vec3,
        push: Vec3,
        seed: u32,
        flesh: crate::flesh::Flesh,
        skin: Option<std::sync::Arc<crate::skin::Skin>>,
        id: u64,
    ) -> Self {
        Self {
            rag: Ragdoll::new(stance, vel, push, seed),
            flesh,
            skin,
            age: 0.0,
            melt: 0.0,
            id,
            version: 0,
            mesh: Vec::new(),
            lit: [-1.0; 3],
            dirty: true,
        }
    }

    /// Un pas : il tombe, puis sa chair fond. Rend d'où une goutte de sang part à ce pas.
    pub fn update(&mut self, world: &World, dt: f32) -> Vec<Vec3> {
        self.age += dt;
        if !self.rag.still() {
            self.rag.update(world, dt);
            self.dirty = true;
        }
        let mut drops = Vec::new();
        if self.melt < BLOOD_S && (self.age >= BLOOD_WAIT || self.rag.still()) {
            self.melt += dt;
            let dims = crate::player_model::flesh_grids();
            let gone = self.flesh.melt(false, (self.melt / BLOOD_S).min(1.0), &dims);
            // (Ses os se montrent au même rythme, et tous à la fin.)
            self.dirty |= self.melt >= BLOOD_S;
            if !gone.is_empty() {
                self.dirty = true;
                let frames = self.rag.frames();
                let every = gone.len().div_ceil(BLOOD_DROPS).max(1);
                for (part, cell) in gone.into_iter().step_by(every) {
                    drops.push(crate::player_model::fallen_point(&frames, part, cell));
                }
            }
        }
        drops
    }

    /// Ses os ont fait leur temps : à retirer.
    pub fn done(&self) -> bool {
        self.age >= BONES_S
    }

    pub fn middle(&self) -> Vec3 {
        self.rag.middle()
    }

    /// Refait son maillage s'il a bougé ou fondu, ou si la lumière du lieu (`light`) a
    /// changé.
    pub fn refresh(&mut self, light: [f32; 3]) {
        let relit = (0..3).any(|k| (light[k] - self.lit[k]).abs() > 0.04);
        if self.dirty || relit {
            let frames = self.rag.frames();
            let bare = (self.melt / BLOOD_S).min(1.0);
            let skin = self.skin.as_deref();
            self.mesh = crate::player_model::fallen_model(&frames, &self.flesh, skin, light, bare);
            self.version += 1;
            self.lit = light;
            self.dirty = false;
        }
    }

    /// Son lot d'entités fixes : (numéro, sommets, version).
    pub fn lot(&self) -> (u64, &[minelib_render::EntityVertex], u64) {
        (self.id, &self.mesh, self.version)
    }
}

/// Sort le point `p` (un petit cube de `THICK` de demi-côté) des blocs qu'il touche, par le
/// côté le plus proche ; il perd sa vitesse vers le bloc et une part de celle le long de lui
/// (`q` : sa place au pas d'avant).
fn push_out(world: &World, p: &mut Vec3, q: &mut Vec3) {
    let (min, max) = (*p - Vec3::splat(THICK), *p + Vec3::splat(THICK));
    let (lo, hi) = (min.floor().as_ivec3(), max.floor().as_ivec3());
    for y in lo.y..=hi.y {
        for z in lo.z..=hi.z {
            for x in lo.x..=hi.x {
                for (a, b) in crate::player::collision_boxes(world, x, y, z) {
                    let (min, max) = (*p - Vec3::splat(THICK), *p + Vec3::splat(THICK));
                    if !(0..3).all(|k| min[k] < b[k] && max[k] > a[k]) {
                        continue;
                    }
                    // Le côté par où il y a le moins à sortir.
                    let mut best = (f32::MAX, 0, 0.0f32);
                    for k in 0..3 {
                        let (up, down) = (b[k] - min[k], max[k] - a[k]);
                        if up < best.0 {
                            best = (up, k, 1.0);
                        }
                        if down < best.0 {
                            best = (down, k, -1.0);
                        }
                    }
                    let (depth, k, way) = best;
                    p[k] += way * depth;
                    q[k] = p[k];
                    for o in [(k + 1) % 3, (k + 2) % 3] {
                        q[o] += (p[o] - q[o]) * RUB;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use minelib_world::{BlockId, ChunkPos};

    fn flat() -> World {
        let mut w = World::new(0);
        for cx in -1..=1 {
            for cz in -1..=1 {
                w.insert_empty(ChunkPos { x: cx, z: cz });
            }
        }
        for x in -16..32 {
            for z in -16..32 {
                w.set(x, 10, z, BlockId::STONE);
            }
        }
        w
    }

    fn standing(feet: Vec3) -> Stance {
        let pose = crate::player_model::Pose {
            feet,
            yaw: 0.4,
            pitch: 0.0,
            walk: 0.0,
            swing: 0.0,
            crouch: 0.0,
            light: [1.0; 3],
            held: None,
            skin: None,
            bow: 0.0,
        };
        crate::player_model::stance(&pose)
    }

    /// Debout sur un sol plat, poussé un peu : il tombe, se couche et se fige en moins de
    /// trois secondes, sans passer sous le sol ; ses morceaux gardent leur longueur ; au
    /// départ, ils sont exactement là où le modèle les dessinait.
    #[test]
    fn a_body_falls_lies_down_and_freezes() {
        let w = flat();
        let s = standing(Vec3::new(4.5, 11.0, 4.5));
        let mut r = Ragdoll::new(&s, Vec3::ZERO, Vec3::new(1.2, 0.0, 0.4), 7);
        let first = r.frames();
        for i in 0..PARTS {
            let lower = s.lower[i].unwrap_or(s.parts[i].0);
            for (got, want) in [(first[i][0], s.parts[i].0), (first[i][1], lower)] {
                let off = (0..4)
                    .map(|c| (got.col(c) - want.col(c)).abs().max_element())
                    .fold(0.0f32, f32::max);
                assert!(off < 1e-3, "pavé {i} : écart {off}");
            }
        }
        let rest = r.rest;
        let mut time = 0.0;
        while !r.still() {
            r.update(&w, 1.0 / 60.0);
            time += 1.0 / 60.0;
            assert!(time < MAX_S + 0.1, "jamais figé");
        }
        let low = r.p.iter().map(|p| p.y).fold(f32::MAX, f32::min);
        let high = r.p.iter().map(|p| p.y).fold(f32::MIN, f32::max);
        assert!(low > 11.0 + THICK - 0.03, "un point à {low}");
        assert!(high < 11.9, "encore debout : {high}");
        for (k, (a, b)) in LINKS.into_iter().enumerate() {
            let len = (r.p[a] - r.p[b]).length();
            assert!((len - rest[k]).abs() < 0.05, "tige {k} : {len} au lieu de {}", rest[k]);
        }
        // Figé : plus rien ne change.
        let before = r.frames();
        r.update(&w, 0.5);
        assert_eq!(before, r.frames());
    }

    /// Un joueur mort : il tombe, sa chair fond en sang (des gouttes partent du corps), il ne
    /// reste que ses os, cinq minutes ; son maillage n'est refait que quand il change.
    #[test]
    fn a_dead_player_melts_into_blood_and_leaves_his_bones() {
        let w = flat();
        let s = standing(Vec3::new(4.5, 11.0, 4.5));
        let flesh = crate::flesh::Flesh::of(crate::flesh::Hard::Bones);
        let mut f = Fallen::new(&s, Vec3::ZERO, Vec3::new(1.0, 0.0, 0.0), 9, flesh, None, 77);
        f.refresh([1.0; 3]);
        let whole = f.lot().1.len();
        let (mut drops, mut t) = (0, 0.0);
        while t < 10.0 {
            let d = f.update(&w, 1.0 / 60.0);
            for at in &d {
                assert!(at.y > 10.9 && at.y < 13.0 && (at.x - 4.5).abs() < 3.0, "{at}");
            }
            drops += d.len();
            f.refresh([1.0; 3]);
            t += 1.0 / 60.0;
        }
        assert!(drops > 150, "{drops} gouttes");
        let (id, bones, version) = f.lot();
        assert_eq!(id, 77);
        // Les os : bien moins de sommets que le corps entier, mais il en reste, au sol.
        assert!(bones.len() > 300 && bones.len() < whole, "{} puis {}", whole, bones.len());
        assert!(bones.iter().all(|v| v.pos[1] > 10.95 && v.pos[1] < 12.0));
        // Plus rien ne change : le maillage n'est plus refait.
        for _ in 0..60 {
            assert!(f.update(&w, 1.0 / 60.0).is_empty());
            f.refresh([1.0; 3]);
        }
        assert_eq!(f.lot().2, version);
        assert!(!f.done());
        f.update(&w, BONES_S);
        assert!(f.done());
    }

    /// Au bord d'un trou : il y tombe (il ne flotte pas à sa hauteur de départ) ; contre un
    /// mur, il ne le traverse pas.
    #[test]
    fn it_falls_into_holes_and_stops_at_walls() {
        let mut w = flat();
        for x in 6..9 {
            for z in 0..10 {
                w.set(x, 10, z, BlockId::AIR);
                w.set(x, 6, z, BlockId::STONE);
            }
        }
        for y in 11..15 {
            for z in 0..10 {
                w.set(2, y, z, BlockId::STONE);
            }
        }
        // Poussé vers le trou (à +x).
        let s = standing(Vec3::new(5.6, 11.0, 4.5));
        let mut r = Ragdoll::new(&s, Vec3::ZERO, Vec3::new(3.0, 0.0, 0.0), 3);
        for _ in 0..240 {
            r.update(&w, 1.0 / 60.0);
        }
        let low = r.p.iter().map(|p| p.y).fold(f32::MAX, f32::min);
        assert!(low < 10.5, "resté au bord : {low}");
        // Poussé vers le mur (à −x) : aucun point ne passe derrière sa face (x = 3).
        let s = standing(Vec3::new(3.6, 11.0, 4.5));
        let mut r = Ragdoll::new(&s, Vec3::ZERO, Vec3::new(-4.0, 0.0, 0.0), 5);
        for _ in 0..240 {
            r.update(&w, 1.0 / 60.0);
            let back = r.p.iter().map(|p| p.x).fold(f32::MAX, f32::min);
            assert!(back > 3.0 + THICK - 0.03, "dans le mur : {back}");
        }
        assert!(r.still());
    }
}
