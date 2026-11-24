//! (v269) Chair des joueurs et des PNJ : leur corps en petits cubes, pour les morsures de
//! L'Évidé.
//!
//! Demandes de Viendax (24 novembre) : « on peut manger des morceaux de lui, il peut manger des
//! morceaux de nous » ; « ça doit enlever une partie en laissant les traces, la forme reste
//! carrée, et tu fais une version du corps remplie avec des cubes colorés pour situer en gros
//! les os et organes du corps à l'intérieur, et tu affiches les faces de ces cubes utiles pour
//! qu'on les voie combler les parties internes de la morsure » ; « c'est le joueur/PNJ pour
//! les os/organes » (le monstre, lui, est en gelée jusqu'au cœur) ; « on mange toujours par
//! les bords, dans les deux sens » ; « un morceau de corps mangé par L'Évidé réapparaît après
//! 5 minutes » (ce qu'on mange de L'Évidé ne revient pas) ; « on ne mange pas toujours les
//! mêmes endroits ».
//!
//! Chaque pavé du modèle (`player_model::parts`) est une grille de cubes : un cube par pixel
//! de son skin (poitrine 8 × 7 × 4, ventre 6 × 5 × 4, jambe 3 × 12 × 4, bras 4 × 12 × 4, tête
//! 8 × 8 × 8). Une morsure retire une boîte de cubes prise sur un bord de ce qui reste. Le
//! modèle est alors dessiné cube par cube (`player_model::emit_flesh`) : les faces du dehors
//! gardent le pixel du skin, celles qu'une morsure met à nu prennent la couleur de ce qu'il y
//! a là dans le corps (`organ`).

/// Nombre de pavés du modèle (jambe gauche, jambe droite, poitrine, bras gauche, bras droit,
/// tête, ventre : l'ordre de `player_model::parts`).
pub const PARTS: usize = 7;
const LEG_L: usize = 0;
const LEG_R: usize = 1;
const CHEST: usize = 2;
const ARM_L: usize = 3;
const ARM_R: usize = 4;
const HEAD: usize = 5;
const BELLY: usize = 6;

/// Un morceau de corps mangé revient au bout de ce temps (s) : « réapparaît après 5 minutes ».
pub const REGROW_S: f32 = 300.0;

/// Une morsure : la boîte de cubes retirée d'un pavé (coins `lo` compris, `hi` non compris,
/// comptés en cubes : x vers la droite du personnage, y vers le haut, z vers son dos) et son
/// âge (s).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bite {
    pub part: u8,
    pub lo: [u8; 3],
    pub hi: [u8; 3],
    /// (v272) Le cube du bord d'où la morsure est partie : elle ne retire, dans sa boîte, que
    /// ce qu'on atteint depuis lui sans traverser du dur (un os, une veine).
    pub start: [u8; 3],
    pub age: f32,
}

/// (v272) Ce qui ne se mange pas dans un corps : « il ne doit pas pouvoir manger nos os »
/// (`bone`), « on ne doit pas pouvoir manger ses veines » (`evide::vein`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Hard {
    /// Tout se mange (essais).
    #[default]
    Nothing,
    /// Un joueur ou un PNJ : ses os et ses dents restent.
    Bones,
    /// L'Évidé : ses veines restent.
    Veins,
}

impl Hard {
    fn at(self, part: usize, dims: [usize; 3], c: [usize; 3]) -> bool {
        match self {
            Hard::Nothing => false,
            Hard::Bones => bone(part, c, dims),
            Hard::Veins => crate::evide::vein(part, c),
        }
    }
}

/// Ce qui manque à un corps : ses morsures, et pour chaque pavé les cubes qu'elles ont
/// retirés (un bit par cube, tenu à jour).
#[derive(Clone, Debug, Default)]
pub struct Flesh {
    bites: Vec<Bite>,
    gone: [Vec<u64>; PARTS],
    /// (v272) Ce qui ne se mange pas dans ce corps.
    hard: Hard,
}

/// (v299) Le tirage d'un cube (0 … 1, toujours le même) : il fond quand la part fondue du
/// corps le dépasse (`Flesh::melt`).
fn draw_of(part: usize, i: usize) -> f32 {
    let h = (i as u32 ^ (part as u32) << 20).wrapping_mul(0x9e37_79b1);
    ((h ^ h >> 15).wrapping_mul(0x85eb_ca6b) >> 8) as f32 / (1u32 << 24) as f32
}

/// (v299) Le tirage du cube `c` du pavé `part` (voir `draw_of`).
pub fn melt_draw(part: usize, dims: [usize; 3], c: [usize; 3]) -> f32 {
    draw_of(part, index(dims, c))
}

fn index(dims: [usize; 3], c: [usize; 3]) -> usize {
    (c[2] * dims[1] + c[1]) * dims[0] + c[0]
}

impl Flesh {
    /// (v272) Un corps entier dont ceci ne se mange pas.
    pub fn of(hard: Hard) -> Self {
        Self {
            hard,
            ..Self::default()
        }
    }

    /// (v272) Ses morsures (pour l'enregistrer avec le monde).
    pub fn bites(&self) -> &[Bite] {
        &self.bites
    }

    /// (v272) Un corps relu du disque : ce qui ne s'y mange pas, ses morsures.
    pub fn restored(hard: Hard, bites: Vec<Bite>, dims: &[[usize; 3]; PARTS]) -> Self {
        let mut f = Self::of(hard);
        f.bites = bites
            .into_iter()
            .filter(|b| {
                let d = dims.get(b.part as usize);
                d.is_some_and(|d| (0..3).all(|a| b.lo[a] < b.hi[a] && b.hi[a] as usize <= d[a]))
            })
            .collect();
        f.rebuild(dims);
        f
    }

    /// (v272) Ce cube est-il dur (jamais mangé) ?
    pub fn is_hard(&self, part: usize, dims: [usize; 3], c: [usize; 3]) -> bool {
        self.hard.at(part, dims, c)
    }

    pub fn is_whole(&self) -> bool {
        self.bites.is_empty()
    }

    /// Ce pavé a-t-il été mordu ?
    pub fn bitten(&self, part: usize) -> bool {
        self.bites.iter().any(|b| b.part as usize == part)
    }

    /// (v299) Le corps d'un mort fond : chaque cube encore là, mou (`hard` : dur, ses os ou
    /// ses veines), dont le tirage (un par cube, toujours le même) est sous `part_gone`, s'en
    /// va. Rend les cubes qui viennent de partir : (pavé, cube). À appeler avec une part qui
    /// monte de 0 à 1.
    pub fn melt(
        &mut self,
        hard: bool,
        part_gone: f32,
        dims: &[[usize; 3]; PARTS],
    ) -> Vec<(usize, [usize; 3])> {
        let mut out = Vec::new();
        for (p, d) in dims.iter().enumerate() {
            let cells = d[0] * d[1] * d[2];
            self.gone[p].resize(cells.div_ceil(64), 0);
            for i in 0..cells {
                if self.gone[p][i / 64] >> (i % 64) & 1 == 1 {
                    continue;
                }
                let c = [i % d[0], i / d[0] % d[1], i / (d[0] * d[1])];
                if self.hard.at(p, *d, c) != hard {
                    continue;
                }
                if draw_of(p, i) < part_gone {
                    self.gone[p][i / 64] |= 1 << (i % 64);
                    out.push((p, c));
                }
            }
        }
        out
    }

    /// Le cube `c` du pavé `part` (grille `dims`) a-t-il été mangé ?
    pub fn gone(&self, part: usize, dims: [usize; 3], c: [usize; 3]) -> bool {
        let i = index(dims, c);
        self.gone[part]
            .get(i / 64)
            .is_some_and(|w| w >> (i % 64) & 1 == 1)
    }

    /// Part du corps mangée (0 … 1), `dims` : la grille de chaque pavé. (v290) Pour les
    /// essais : L'Évidé ne meurt plus de la part mangée, mais de nos cinq bouchées.
    #[cfg(test)]
    pub fn eaten(&self, dims: &[[usize; 3]; PARTS]) -> f32 {
        let all: usize = dims.iter().map(|d| d[0] * d[1] * d[2]).sum();
        let gone: u32 = self.gone.iter().flatten().map(|w| w.count_ones()).sum();
        gone as f32 / all.max(1) as f32
    }

    /// Part d'un pavé mangée (0 … 1).
    pub fn eaten_of(&self, part: usize, dims: [usize; 3]) -> f32 {
        let gone: u32 = self.gone[part].iter().map(|w| w.count_ones()).sum();
        gone as f32 / (dims[0] * dims[1] * dims[2]).max(1) as f32
    }

    fn rebuild(&mut self, dims: &[[usize; 3]; PARTS]) {
        for (p, d) in dims.iter().enumerate() {
            let words = (d[0] * d[1] * d[2]).div_ceil(64);
            self.gone[p].clear();
            self.gone[p].resize(words, 0);
        }
        // (v272) Chaque morsure retire, dans sa boîte, les cubes qu'on atteint depuis son cube
        // de départ de proche en proche (par les faces) sans traverser un cube dur : un os
        // protège ce qu'il y a derrière lui, une veine reste dans le creux.
        let mut todo = Vec::new();
        let hard = self.hard;
        for b in &self.bites {
            let (p, d) = (b.part as usize, dims[b.part as usize]);
            let inside = |c: [usize; 3]| {
                (0..3).all(|a| c[a] >= b.lo[a] as usize && c[a] < (b.hi[a] as usize).min(d[a]))
            };
            let start = b.start.map(|v| v as usize);
            if !inside(start) || hard.at(p, d, start) {
                continue;
            }
            let side = [0, 1, 2].map(|a| (b.hi[a] - b.lo[a]) as usize);
            let local = |c: [usize; 3]| {
                ((c[2] - b.lo[2] as usize) * side[1] + c[1] - b.lo[1] as usize) * side[0] + c[0]
                    - b.lo[0] as usize
            };
            let mut seen = vec![false; side[0] * side[1] * side[2]];
            seen[local(start)] = true;
            todo.clear();
            todo.push(start);
            while let Some(c) = todo.pop() {
                let i = index(d, c);
                self.gone[p][i / 64] |= 1 << (i % 64);
                for a in 0..3 {
                    for step in [-1i32, 1] {
                        let v = c[a] as i32 + step;
                        if v < 0 {
                            continue;
                        }
                        let mut n = c;
                        n[a] = v as usize;
                        if inside(n) && !seen[local(n)] && !hard.at(p, d, n) {
                            seen[local(n)] = true;
                            todo.push(n);
                        }
                    }
                }
            }
        }
        self.drop_loose(dims);
    }

    /// (v277) « Qu'il n'y ait pas de morceau qui vole seul quand le monstre mord » : dans un
    /// pavé mordu, ce qui ne tient plus au dur (nos os, ses veines) de proche en proche, par
    /// les faces, part avec la morsure. (Quand le morceau mangé repousse, il revient avec.)
    fn drop_loose(&mut self, dims: &[[usize; 3]; PARTS]) {
        let hard = self.hard;
        if hard == Hard::Nothing {
            return;
        }
        for (p, d) in dims.iter().enumerate() {
            if !self.bitten(p) {
                continue;
            }
            let count = d[0] * d[1] * d[2];
            let cell = |i: usize| [i % d[0], i / d[0] % d[1], i / (d[0] * d[1])];
            let mut held = vec![false; count];
            let mut todo: Vec<usize> = (0..count).filter(|&i| hard.at(p, *d, cell(i))).collect();
            if todo.is_empty() {
                continue;
            }
            for &i in &todo {
                held[i] = true;
            }
            while let Some(i) = todo.pop() {
                let c = cell(i);
                for a in 0..3 {
                    for step in [-1i32, 1] {
                        let v = c[a] as i32 + step;
                        if v < 0 || v as usize >= d[a] {
                            continue;
                        }
                        let mut n = c;
                        n[a] = v as usize;
                        let j = index(*d, n);
                        if !held[j] && self.gone[p][j / 64] >> (j % 64) & 1 == 0 {
                            held[j] = true;
                            todo.push(j);
                        }
                    }
                }
            }
            for (j, kept) in held.iter().enumerate() {
                if !kept {
                    self.gone[p][j / 64] |= 1 << (j % 64);
                }
            }
        }
    }

    /// Le temps passe : un morceau mangé depuis `regrow` secondes revient (`None` : jamais,
    /// la chair de L'Évidé ne repousse pas).
    pub fn tick(&mut self, dt: f32, regrow: Option<f32>, dims: &[[usize; 3]; PARTS]) {
        if self.bites.is_empty() {
            return;
        }
        for b in &mut self.bites {
            b.age += dt;
        }
        if let Some(limit) = regrow {
            let before = self.bites.len();
            self.bites.retain(|b| b.age < limit);
            if self.bites.len() != before {
                self.rebuild(dims);
            }
        }
    }

    /// Cubes du pavé encore là et au bord de ce qui reste (un voisin dehors ou mangé) : là où
    /// l'on peut mordre.
    fn edge_cells(&self, part: usize, dims: [usize; 3]) -> Vec<[usize; 3]> {
        let mut out = Vec::new();
        for z in 0..dims[2] {
            for y in 0..dims[1] {
                for x in 0..dims[0] {
                    let c = [x, y, z];
                    // (v272) On ne mord pas dans du dur.
                    if self.gone(part, dims, c) || self.is_hard(part, dims, c) {
                        continue;
                    }
                    let open = (0..3).any(|a| {
                        let mut lo = c;
                        let mut hi = c;
                        hi[a] += 1;
                        c[a] == 0
                            || hi[a] >= dims[a]
                            || {
                                lo[a] -= 1;
                                self.gone(part, dims, lo)
                            }
                            || self.gone(part, dims, hi)
                    });
                    if open {
                        out.push(c);
                    }
                }
            }
        }
        out
    }

    /// Mord dans le pavé `part`, autour du cube `at` (un cube du bord, ou le plus proche d'un
    /// bord), une boîte d'au plus `size` cubes de côté : « on mange toujours par les bords ».
    /// Rend la morsure, ou `None` s'il ne reste rien à manger là.
    pub fn bite_at(
        &mut self,
        part: usize,
        at: [usize; 3],
        size: usize,
        dims: &[[usize; 3]; PARTS],
    ) -> Option<Bite> {
        let d = dims[part];
        let edge = self.edge_cells(part, d);
        // Le cube du bord le plus proche de l'endroit visé.
        let dist = |c: &[usize; 3]| (0..3).map(|a| c[a].abs_diff(at[a]).pow(2)).sum::<usize>();
        let c = *edge.iter().min_by_key(|c| dist(c))?;
        // Une boîte de `size` de côté autour de lui, rentrée dans la grille.
        let mut lo = [0u8; 3];
        let mut hi = [0u8; 3];
        for a in 0..3 {
            let s = size.clamp(1, d[a]);
            let start = c[a].saturating_sub(s / 2).min(d[a] - s);
            (lo[a], hi[a]) = (start as u8, (start + s) as u8);
        }
        let bite = Bite {
            part: part as u8,
            lo,
            hi,
            start: c.map(|v| v as u8),
            age: 0.0,
        };
        self.bites.push(bite);
        self.rebuild(dims);
        Some(bite)
    }

    /// (v272) Du sang coule sur ce cube : à combien de cubes au-dessus de lui (1 … `DRIP`)
    /// se trouve un cube mangé de la même colonne ; 0 : aucun.
    pub fn drip(&self, part: usize, dims: [usize; 3], c: [usize; 3]) -> usize {
        if self.bites.is_empty() {
            return 0;
        }
        (1..=DRIP)
            .find(|j| c[1] + j < dims[1] && self.gone(part, dims, [c[0], c[1] + j, c[2]]))
            .unwrap_or(0)
    }

    /// Mord au hasard (`seed`) sur un bord du corps, de préférence un pavé encore peu mangé :
    /// « on ne mange pas toujours les mêmes endroits ». `allowed` : les pavés où mordre.
    pub fn bite_somewhere(
        &mut self,
        seed: u32,
        size: usize,
        allowed: &[usize],
        dims: &[[usize; 3]; PARTS],
    ) -> Option<Bite> {
        let (part, c) = self.pick(seed, allowed, dims)?;
        self.bite_at(part, c, size, dims)
    }

    /// (v272) Là où la prochaine morsure tombera : un cube du bord de ce qui reste, tiré de
    /// `seed` parmi les pavés `allowed`, de préférence un pavé encore peu mangé. Le monstre
    /// le vise avant de mordre : (pavé, cube).
    pub fn pick(
        &self,
        seed: u32,
        allowed: &[usize],
        dims: &[[usize; 3]; PARTS],
    ) -> Option<(usize, [usize; 3])> {
        let mut rng = seed.wrapping_mul(0x9e37_79b9) | 1;
        let mut next = || {
            rng ^= rng << 13;
            rng ^= rng >> 17;
            rng ^= rng << 5;
            rng
        };
        // Deux pavés tirés : on garde le moins mangé.
        let pick = |n: u32| allowed[(n as usize) % allowed.len().max(1)];
        if allowed.is_empty() {
            return None;
        }
        let (a, b) = (pick(next()), pick(next() >> 7));
        let part = if self.eaten_of(a, dims[a]) <= self.eaten_of(b, dims[b]) {
            a
        } else {
            b
        };
        let edge = self.edge_cells(part, dims[part]);
        if edge.is_empty() {
            // Ce pavé est fini : un autre, s'il en reste.
            let other = allowed
                .iter()
                .copied()
                .find(|&p| !self.edge_cells(p, dims[p]).is_empty())?;
            let e = self.edge_cells(other, dims[other]);
            return Some((other, e[next() as usize % e.len()]));
        }
        Some((part, edge[next() as usize % edge.len()]))
    }
}

/// (v272) Un os ou une dent : ce que L'Évidé ne peut pas manger (la moelle, enfermée dans
/// l'os, et le cerveau, dans le crâne, sont donc à l'abri aussi).
pub fn bone(part: usize, c: [usize; 3], dims: [usize; 3]) -> bool {
    matches!(organ(part, c, dims), BONE | TEETH)
}

/// (v272) Le sang coule au plus sur tant de cubes sous une morsure.
pub const DRIP: usize = 5;
/// (v272) Couleur du sang frais sur la peau (sRGB).
pub const BLOOD: u32 = 0x7c0a0a;

// Couleurs de l'intérieur du corps (sRGB).
const MUSCLE: u32 = 0xa83a34;
const MUSCLE_DARK: u32 = 0x8c2c2a;
const FAT: u32 = 0xe2b98a;
const BONE: u32 = 0xece4d2;
const BRAIN: u32 = 0xd9a2a6;
const EYE: u32 = 0xf4f4f0;
const TEETH: u32 = 0xfaf6ea;
const MOUTH: u32 = 0x5c1418;
const TONGUE: u32 = 0xc8566a;
const THROAT: u32 = 0x8a2a36;
const LUNG: u32 = 0xdf9a9c;
const HEART: u32 = 0x8e1420;
const LIVER: u32 = 0x6e2c1e;
const STOMACH: u32 = 0xcf8672;
const GUT: u32 = 0xdcaa84;
const KIDNEY: u32 = 0x7a2a30;
const BLADDER: u32 = 0xd8c074;
/// La gelée de L'Évidé, du bord vers le cœur.
const JELLY: [u32; 3] = [0xf2efe2, 0xe9e5d0, 0xdcd8bc];

/// Ce qu'il y a dans le corps d'un joueur ou d'un PNJ au cube `c` du pavé `part` (grille
/// `dims`) : sa couleur. « En gros » : la peau et le gras au bord, le muscle dessous, les os
/// (crâne, côtes, colonne, bassin, os longs des membres) et les organes à leur place (cerveau,
/// yeux, dents, poumons, cœur à gauche, foie à droite, estomac, intestins, reins, vessie).
/// Le personnage regarde vers −z (z = 0 : devant) ; x = 0 est sa gauche.
pub fn organ(part: usize, c: [usize; 3], dims: [usize; 3]) -> u32 {
    let [x, y, z] = c;
    let [nx, ny, nz] = dims;
    // Profondeur sous la peau : 0 au bord.
    let depth = x.min(nx - 1 - x).min(y.min(ny - 1 - y)).min(z.min(nz - 1 - z));
    // Du haut vers le bas, et la droite du personnage (x grand) ramenée à gauche.
    let top = ny - 1 - y;
    let mid = |v: usize, n: usize| v == (n - 1) / 2 || v == n / 2;
    match part {
        HEAD => {
            // Crâne : une coque d'os sous la peau, le cerveau dedans ; en bas, la mâchoire,
            // les dents devant, la bouche, la langue, la gorge ; les yeux derrière la face.
            let jaw = top >= 5;
            if depth == 0 {
                return if z == 0 && jaw && top == 6 && (2..nx - 2).contains(&x) {
                    TEETH
                } else {
                    FAT
                };
            }
            if !jaw {
                if z == 1 && top == 4 && (x == 2 || x == nx - 3) {
                    return EYE;
                }
                return if depth == 1 { BONE } else { BRAIN };
            }
            if z == 1 && top <= 6 {
                return TEETH;
            }
            if depth == 1 {
                return if top == 7 || z >= nz - 2 { BONE } else { MUSCLE };
            }
            if mid(x, nx) && z >= nz / 2 {
                return if z == nz - 3 { BONE } else { THROAT };
            }
            if top == 6 {
                TONGUE
            } else {
                MOUTH
            }
        }
        CHEST => {
            // Devant : le sternum au milieu et les côtes une rangée sur deux ; derrière : la
            // colonne et les côtes ; dedans : les deux poumons, le cœur un peu à gauche.
            if z == 0 || z == nz - 1 {
                if mid(x, nx) {
                    return if z == 0 && top == 0 { MUSCLE } else { BONE };
                }
                // (v277) Les côtes font le tour : devant, sur les flancs, derrière, d'un seul
                // tenant avec le sternum et la colonne.
                if x == 0 || x == nx - 1 {
                    return if top % 2 == 0 { BONE } else { MUSCLE };
                }
                return if top % 2 == 0 { BONE } else { MUSCLE_DARK };
            }
            if top == 0 {
                // Clavicules et haut des épaules.
                return if mid(x, nx) { THROAT } else { BONE };
            }
            if x == 0 || x == nx - 1 {
                return if top % 2 == 0 { BONE } else { MUSCLE };
            }
            // Le cœur : à gauche du milieu (x petit : sa gauche), vers l'avant.
            if (2..=4).contains(&top) && (x == nx / 2 - 1 || x == nx / 2 - 2) && z == 1 {
                return HEART;
            }
            if mid(x, nx) {
                return if z == nz - 2 { BONE } else { THROAT };
            }
            if top == ny - 1 {
                // Le diaphragme, sous les poumons.
                return MUSCLE_DARK;
            }
            LUNG
        }
        BELLY => {
            // Derrière au milieu : la colonne ; en bas : le bassin ; dedans : le foie en haut
            // à droite, l'estomac en haut à gauche, les reins derrière, les intestins, la
            // vessie en bas.
            // (v277) En bas du dos, le bassin rejoint la colonne (un seul squelette).
            if z == nz - 1 {
                return if mid(x, nx) || top == ny - 1 { BONE } else { MUSCLE_DARK };
            }
            if top == ny - 1 {
                return if mid(x, nx) && z > 0 { BLADDER } else { BONE };
            }
            if z == 0 {
                return if x == 0 || x == nx - 1 { MUSCLE } else { FAT };
            }
            if x == 0 || x == nx - 1 {
                return if top == ny - 2 { BONE } else { MUSCLE };
            }
            if top == 0 {
                return if x >= nx / 2 { LIVER } else { STOMACH };
            }
            if top == 1 && z == nz - 2 && !mid(x, nx) {
                return KIDNEY;
            }
            if top == 1 && x >= nx / 2 + 1 {
                return LIVER;
            }
            GUT
        }
        ARM_L | ARM_R | LEG_L | LEG_R => {
            // Un os long au milieu (avec sa moelle), le muscle autour, le gras sous la peau ;
            // l'articulation (coude, genou) à mi-hauteur ; la main, le pied : des petits os.
            let core_x = if nx % 2 == 1 { x == nx / 2 } else { mid(x, nx) };
            let core_z = mid(z, nz);
            let joint = top == ny / 2 || top == ny / 2 - 1;
            let end = top >= ny - 2;
            // (v277) « Des os tout le long du corps » : l'os long est de l'os d'un bout à
            // l'autre du membre (avant : sa moelle, que L'Évidé mangeait, sauf aux
            // articulations).
            if core_x && core_z {
                return BONE;
            }
            // (v277) La main, le pied : des os en croix, qui tiennent à l'os long (avant : un
            // damier de petits os séparés, dont un pouvait rester en l'air avec sa chair).
            if end {
                return if core_x || core_z { BONE } else { MUSCLE };
            }
            if joint && (core_x || core_z) {
                return BONE;
            }
            if depth == 0 && !core_x && !core_z {
                return FAT;
            }
            if (top + x) % 3 == 0 {
                MUSCLE_DARK
            } else {
                MUSCLE
            }
        }
        _ => MUSCLE,
    }
}

/// L'intérieur de L'Évidé : de la gelée jusqu'au cœur, un peu plus dense vers le milieu.
pub fn jelly(c: [usize; 3], dims: [usize; 3]) -> u32 {
    let [x, y, z] = c;
    let [nx, ny, nz] = dims;
    let depth = x.min(nx - 1 - x).min(y.min(ny - 1 - y)).min(z.min(nz - 1 - z));
    JELLY[depth.min(2)]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// (v277) « Les os, il doit y en avoir tout le long du corps, qu'il n'y ait pas de
    /// morceau qui vole seul quand le monstre mord » : chaque rangée des membres, de la
    /// poitrine et du ventre a de l'os, d'un seul tenant dans chaque pavé ; mordu autant qu'on
    /// veut, les os sont tous là et tout ce qui reste de chair tient à un os.
    #[test]
    fn bones_run_all_along_the_body_and_nothing_is_left_floating() {
        let grids = crate::player_model::flesh_grids();
        let cells = |d: [usize; 3]| {
            (0..d[0] * d[1] * d[2]).map(move |i| [i % d[0], i / d[0] % d[1], i / (d[0] * d[1])])
        };
        for (p, d) in grids.iter().enumerate() {
            // (La tête n'est jamais mordue : « c'est fait d'os ».)
            if p == HEAD {
                continue;
            }
            let bones: Vec<[usize; 3]> = cells(*d).filter(|c| bone(p, *c, *d)).collect();
            for y in 0..d[1] {
                assert!(bones.iter().any(|c| c[1] == y), "pavé {p} : pas d'os à la rangée {y}");
            }
            let mut seen = std::collections::HashSet::from([bones[0]]);
            let mut todo = vec![bones[0]];
            while let Some(c) = todo.pop() {
                for n in &bones {
                    if (0..3).map(|a| c[a].abs_diff(n[a])).sum::<usize>() == 1 && seen.insert(*n) {
                        todo.push(*n);
                    }
                }
            }
            assert_eq!(seen.len(), bones.len(), "pavé {p} : squelette en plusieurs morceaux");
        }
        let mut f = Flesh::of(Hard::Bones);
        for k in 0..80u32 {
            f.bite_somewhere(k * 131 + 9, 3 + (k as usize % 2), &[0, 1, 2, 3, 4, 6], &grids);
            if k % 20 != 19 {
                continue;
            }
            for (p, d) in grids.iter().enumerate() {
                // Ce qui reste, et ce qui tient à un os de proche en proche.
                let left: Vec<[usize; 3]> = cells(*d).filter(|c| !f.gone(p, *d, *c)).collect();
                let mut held: std::collections::HashSet<[usize; 3]> =
                    left.iter().copied().filter(|c| bone(p, *c, *d)).collect();
                assert_eq!(held.len(), cells(*d).filter(|c| bone(p, *c, *d)).count(), "os mangé");
                let mut todo: Vec<[usize; 3]> = held.iter().copied().collect();
                while let Some(c) = todo.pop() {
                    for n in &left {
                        if (0..3).map(|a| c[a].abs_diff(n[a])).sum::<usize>() == 1 && held.insert(*n) {
                            todo.push(*n);
                        }
                    }
                }
                assert_eq!(held.len(), left.len(), "pavé {p} : un morceau vole seul");
            }
        }
        assert!(f.eaten(&grids) > 0.3, "{}", f.eaten(&grids));
    }

    const DIMS: [[usize; 3]; PARTS] = [
        [3, 12, 4],
        [3, 12, 4],
        [8, 7, 4],
        [4, 12, 4],
        [4, 12, 4],
        [8, 8, 8],
        [6, 5, 4],
    ];

    /// « On mange toujours par les bords » : chaque morsure touche le bord de ce qui reste ;
    /// « on ne mange pas toujours les mêmes endroits » ; le morceau revient après 5 minutes
    /// (celui de L'Évidé jamais) ; à force, il ne reste rien.
    #[test]
    fn bites_take_the_edges_and_grow_back() {
        let mut f = Flesh::default();
        assert!(f.is_whole() && f.eaten(&DIMS) == 0.0);
        let all = [0, 1, 2, 3, 4, 5, 6];
        let mut places = std::collections::HashSet::new();
        for k in 0..12u32 {
            // Avant de mordre : la boîte doit toucher un cube du bord de ce qui reste.
            let before = f.clone();
            let b = f.bite_somewhere(k * 7 + 1, 2, &all, &DIMS).expect("morsure");
            let d = DIMS[b.part as usize];
            let edge = before.edge_cells(b.part as usize, d);
            let touches = edge.iter().any(|c| {
                (0..3).all(|a| c[a] >= b.lo[a] as usize && c[a] < b.hi[a] as usize)
            });
            assert!(touches, "morsure {k} : pas sur un bord");
            assert!((0..3).all(|a| b.hi[a] > b.lo[a] && b.hi[a] as usize <= d[a]));
            places.insert((b.part, b.lo));
        }
        assert!(places.len() >= 8, "{} endroits seulement", places.len());
        let eaten = f.eaten(&DIMS);
        assert!(eaten > 0.02 && eaten < 0.2, "{eaten}");
        // Le temps passe : rien ne revient avant 5 minutes, tout après (une par une).
        let mut jelly = f.clone();
        f.tick(REGROW_S - 1.0, Some(REGROW_S), &DIMS);
        assert_eq!(f.bites.len(), 12);
        f.tick(2.0, Some(REGROW_S), &DIMS);
        assert!(f.is_whole() && f.eaten(&DIMS) == 0.0);
        jelly.tick(10_000.0, None, &DIMS);
        assert_eq!(jelly.bites.len(), 12);
        // Viser un cube : la morsure est là. Manger jusqu'au bout : plus rien à mordre.
        let mut g = Flesh::default();
        let b = g.bite_at(HEAD, [7, 7, 0], 3, &DIMS).unwrap();
        assert_eq!((b.lo, b.hi), ([5, 5, 0], [8, 8, 3]));
        assert!(g.gone(HEAD, DIMS[HEAD], [7, 7, 0]) && !g.gone(HEAD, DIMS[HEAD], [4, 7, 0]));
        let mut n = 0;
        while g.bite_somewhere(n, 3, &[BELLY], &DIMS).is_some() {
            n += 1;
            assert!(n < 500);
        }
        assert!((g.eaten_of(BELLY, DIMS[BELLY]) - 1.0).abs() < 1e-6);
    }

    /// L'intérieur « en gros » : le crâne autour du cerveau, les yeux, les dents, les côtes et
    /// la colonne, le cœur à gauche, les poumons, le foie à droite, l'os au milieu des membres.
    #[test]
    fn the_inside_has_bones_and_organs_where_they_belong() {
        let count = |part: usize, colour: u32| {
            let d = DIMS[part];
            let mut n = 0;
            for z in 0..d[2] {
                for y in 0..d[1] {
                    for x in 0..d[0] {
                        n += (organ(part, [x, y, z], d) == colour) as usize;
                    }
                }
            }
            n
        };
        assert!(count(HEAD, BRAIN) >= 40 && count(HEAD, BONE) >= 60);
        assert_eq!(count(HEAD, EYE), 2);
        assert!(count(HEAD, TEETH) >= 4);
        assert!(count(CHEST, LUNG) >= 20 && count(CHEST, BONE) >= 20);
        assert!(count(CHEST, HEART) >= 4);
        assert!(count(BELLY, GUT) >= 10 && count(BELLY, LIVER) >= 3 && count(BELLY, KIDNEY) == 2);
        for limb in [ARM_L, ARM_R, LEG_L, LEG_R] {
            assert!(count(limb, BONE) >= 12, "{limb}");
            assert!(count(limb, MUSCLE) + count(limb, MUSCLE_DARK) >= 30, "{limb}");
        }
        // Le cœur est à gauche du milieu (x petit : la gauche du personnage).
        let d = DIMS[CHEST];
        for y in 0..d[1] {
            for x in d[0] / 2..d[0] {
                for z in 0..d[2] {
                    assert_ne!(organ(CHEST, [x, y, z], d), HEART);
                }
            }
        }
        // La gelée : plus dense au cœur.
        assert_ne!(jelly([0, 0, 0], [8, 8, 8]), jelly([3, 3, 3], [8, 8, 8]));
    }
}
