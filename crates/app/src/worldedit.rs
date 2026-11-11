//! Édition du monde à la manière de WorldEdit : sélection d'une zone avec la baguette
//! (clic gauche coin 1, clic droit coin 2) puis commandes `//…` dans le tchat.
//!
//! Commandes (comme dans WorldEdit, adaptées à Minelib) :
//! - sélection : `//pos1`, `//pos2` (pieds du joueur), `//hpos1`, `//hpos2` (bloc visé),
//!   `//sel` (efface), `//size`, `//expand`, `//contract`, `//shift`, `//wand` ;
//! - remplissage : `//set`, `//replace`, `//walls`, `//faces` (`//outline`), `//overlay`,
//!   `//center`, `//naturalize`, `//line`, `//drain`, `//replacenear` ;
//! - formes au pied du joueur : `//sphere`, `//hsphere`, `//cyl`, `//hcyl`, `//pyramid`,
//!   `//hpyramid` ;
//! - déplacements : `//move`, `//stack`, `//copy`, `//cut`, `//paste [-a]`, `//rotate`,
//!   `//flip` ;
//! - historique : `//undo`, `//redo` ; informations : `//count`, `//distr`, `//help`.
//!
//! Motifs : `pierre`, `stone`, `50%stone,50%dirt`, numéro de bloc ; noms français ou clés
//! (`planks_oak`, `slab:stone:bottom`…). Directions : `me` (regard, par défaut), `back`,
//! `left`, `right`, `up`, `down`, `north` / `south` / `east` / `west` (ou n, s, e, w, u, d).

use std::collections::{HashMap, HashSet};

use minelib_world::registry::registry;
use minelib_world::{BlockId, ChunkPos, World, CHUNK_HEIGHT};

/// Au plus tant de blocs changés par commande (au-delà, refusé).
const MAX_CHANGES: usize = 4_000_000;
/// Opérations gardées pour `//undo`.
const HISTORY: usize = 30;

type Pos = [i32; 3];

/// Blocs relevés par `//copy`, relatifs aux pieds du joueur au moment de la copie.
struct Clipboard {
    blocks: Vec<(Pos, BlockId)>,
}

#[derive(Default)]
pub struct WorldEdit {
    pub pos1: Option<Pos>,
    pub pos2: Option<Pos>,
    clipboard: Option<Clipboard>,
    undo: Vec<Vec<(Pos, BlockId)>>,
    redo: Vec<Vec<(Pos, BlockId)>>,
}

/// Contexte d'une commande : le monde et la position du joueur.
pub struct Ctx<'a> {
    pub world: &'a mut World,
    /// Bloc des pieds du joueur.
    pub feet: Pos,
    /// Regard (vecteur unitaire).
    pub look: [f32; 3],
    /// Bloc visé (s'il y en a un à portée).
    pub target: Option<Pos>,
    /// Chunks à remailler.
    pub dirty: &'a mut HashSet<ChunkPos>,
}

type Reply = Result<String, String>;

/// Motif : blocs et poids (`50%stone,50%dirt`).
struct Pattern(Vec<(f32, BlockId)>);

impl Pattern {
    fn pick(&self, p: Pos) -> BlockId {
        if self.0.len() == 1 {
            return self.0[0].1;
        }
        let total: f32 = self.0.iter().map(|w| w.0).sum();
        // Tirage déterministe par position (même résultat si on recommence).
        let mut h = (p[0] as u32).wrapping_mul(73_856_093)
            ^ (p[1] as u32).wrapping_mul(19_349_663)
            ^ (p[2] as u32).wrapping_mul(83_492_791);
        h ^= h >> 13;
        h = h.wrapping_mul(0x5bd1_e995);
        h ^= h >> 15;
        let mut r = (h as f32 / u32::MAX as f32) * total;
        for (w, b) in &self.0 {
            if r < *w {
                return *b;
            }
            r -= w;
        }
        self.0[self.0.len() - 1].1
    }
}

/// Minuscules sans accents, espaces et tirets comme `_`.
fn fold(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| match c {
            'à' | 'â' | 'ä' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'î' | 'ï' => 'i',
            'ô' | 'ö' => 'o',
            'ù' | 'û' | 'ü' => 'u',
            'ç' => 'c',
            ' ' | '-' => '_',
            c => c,
        })
        .collect()
}

/// Bloc par clé, nom (français, sans accents) ou numéro.
pub fn parse_block(name: &str) -> Option<BlockId> {
    let reg = registry();
    let n = name.trim().trim_start_matches("minecraft:");
    if n.is_empty() {
        return None;
    }
    if matches!(n, "air" | "0" | "vide") {
        return Some(BlockId::AIR);
    }
    if let Ok(i) = n.parse::<u16>() {
        return ((i as usize) < reg.defs.len()).then_some(BlockId(i));
    }
    if let Some(b) = reg.by_key(n) {
        return Some(b);
    }
    let f = fold(n);
    reg.defs
        .iter()
        .position(|d| fold(&d.name) == f || fold(&d.key) == f)
        .map(|i| BlockId(i as u16))
}

fn parse_pattern(s: &str) -> Result<Pattern, String> {
    let mut v = Vec::new();
    for part in s.split(',') {
        let (w, name) = match part.split_once('%') {
            Some((w, n)) => (
                w.parse::<f32>()
                    .map_err(|_| format!("pourcentage illisible : {part}"))?,
                n,
            ),
            None => (1.0, part),
        };
        let b = parse_block(name).ok_or_else(|| format!("bloc inconnu : {name}"))?;
        v.push((w.max(0.0), b));
    }
    if v.is_empty() {
        return Err("motif vide".into());
    }
    Ok(Pattern(v))
}

fn parse_mask(s: &str) -> Result<HashSet<BlockId>, String> {
    s.split(',')
        .map(|n| parse_block(n).ok_or_else(|| format!("bloc inconnu : {n}")))
        .collect()
}

/// Direction : relative au regard (`me`, `back`, `left`, `right`) ou absolue.
fn parse_dir(s: Option<&str>, look: [f32; 3]) -> Result<Pos, String> {
    let facing = || -> Pos {
        let [x, y, z] = look;
        if y.abs() > 0.75 {
            [0, y.signum() as i32, 0]
        } else if x.abs() > z.abs() {
            [x.signum() as i32, 0, 0]
        } else {
            [0, 0, z.signum() as i32]
        }
    };
    let f = facing();
    let horiz = || -> Pos {
        let [x, _, z] = look;
        if x.abs() > z.abs() {
            [x.signum() as i32, 0, 0]
        } else {
            [0, 0, z.signum() as i32]
        }
    };
    Ok(match s.map(fold).as_deref() {
        None | Some("me") | Some("forward") | Some("f") | Some("avant") => f,
        Some("back") | Some("b") | Some("arriere") => [-f[0], -f[1], -f[2]],
        Some("left") | Some("l") | Some("gauche") => {
            let h = horiz();
            [h[2], 0, -h[0]]
        }
        Some("right") | Some("r") | Some("droite") => {
            let h = horiz();
            [-h[2], 0, h[0]]
        }
        Some("up") | Some("u") | Some("haut") => [0, 1, 0],
        Some("down") | Some("d") | Some("bas") => [0, -1, 0],
        Some("north") | Some("n") | Some("nord") => [0, 0, -1],
        Some("south") | Some("s") | Some("sud") => [0, 0, 1],
        Some("east") | Some("e") | Some("est") => [1, 0, 0],
        Some("west") | Some("w") | Some("ouest") | Some("o") => [-1, 0, 0],
        Some(o) => return Err(format!("direction inconnue : {o}")),
    })
}

fn num(s: Option<&str>, default: i32) -> Result<i32, String> {
    match s {
        None => Ok(default),
        Some(t) => t.parse().map_err(|_| format!("nombre attendu : {t}")),
    }
}

fn add(a: Pos, b: Pos) -> Pos {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn scale(a: Pos, k: i32) -> Pos {
    [a[0] * k, a[1] * k, a[2] * k]
}

const HELP: &str = "//pos1 //pos2 //hpos1 //hpos2 //sel //size //wand | //set //replace //walls \
//faces //overlay //center //naturalize //line //drain //replacenear | //sphere //hsphere //cyl \
//hcyl //pyramid //hpyramid | //move //stack //shift //expand //contract | //copy //cut \
//paste [-a] //rotate //flip | //undo //redo //count //distr";

impl WorldEdit {
    /// Zone sélectionnée (coins min et max inclus).
    pub fn region(&self) -> Option<(Pos, Pos)> {
        let (a, b) = (self.pos1?, self.pos2?);
        Some((
            [a[0].min(b[0]), a[1].min(b[1]), a[2].min(b[2])],
            [a[0].max(b[0]), a[1].max(b[1]), a[2].max(b[2])],
        ))
    }

    fn need_region(&self) -> Result<(Pos, Pos), String> {
        self.region()
            .ok_or_else(|| "Sélection incomplète : //pos1 et //pos2 (ou la baguette)".into())
    }

    fn volume((a, b): (Pos, Pos)) -> usize {
        ((b[0] - a[0] + 1) as usize) * ((b[1] - a[1] + 1) as usize) * ((b[2] - a[2] + 1) as usize)
    }

    /// Change les blocs donnés (en gardant les anciens pour `//undo`).
    fn apply(&mut self, ctx: &mut Ctx, changes: Vec<(Pos, BlockId)>) -> Result<usize, String> {
        if changes.len() > MAX_CHANGES {
            return Err(format!(
                "Trop de blocs ({}) : au plus {MAX_CHANGES} par commande",
                changes.len()
            ));
        }
        let mut old = Vec::with_capacity(changes.len());
        let mut n = 0;
        for (p, b) in changes {
            if !(0..CHUNK_HEIGHT as i32).contains(&p[1]) {
                continue;
            }
            let before = ctx.world.get(p[0], p[1], p[2]);
            if before == b || !ctx.world.contains(ChunkPos::containing(p[0], p[2])) {
                continue;
            }
            old.push((p, before));
            let d = if b.is_torch() {
                ctx.world.place_torch_block(p[0], p[1], p[2], b, None)
            } else {
                ctx.world.set(p[0], p[1], p[2], b)
            };
            ctx.dirty.extend(d);
            n += 1;
        }
        if !old.is_empty() {
            self.undo.push(old);
            if self.undo.len() > HISTORY {
                self.undo.remove(0);
            }
            self.redo.clear();
        }
        Ok(n)
    }

    fn cells((a, b): (Pos, Pos)) -> impl Iterator<Item = Pos> {
        (a[1]..=b[1]).flat_map(move |y| {
            (a[2]..=b[2]).flat_map(move |z| (a[0]..=b[0]).map(move |x| [x, y, z]))
        })
    }

    /// Exécute la commande `name` (sans les `//`) avec ses arguments.
    pub fn command(&mut self, name: &str, args: &[&str], ctx: &mut Ctx) -> Reply {
        let changed = |n: usize| Ok(format!("{n} bloc(s) modifié(s)"));
        match name {
            "help" | "?" => Ok(HELP.into()),
            "pos1" => {
                self.pos1 = Some(ctx.feet);
                Ok(format!("Coin 1 : {:?}", ctx.feet))
            }
            "pos2" => {
                self.pos2 = Some(ctx.feet);
                Ok(format!("Coin 2 : {:?}", ctx.feet))
            }
            "hpos1" | "hpos2" => {
                let t = ctx.target.ok_or("Aucun bloc visé")?;
                if name == "hpos1" {
                    self.pos1 = Some(t);
                } else {
                    self.pos2 = Some(t);
                }
                Ok(format!("Coin {} : {t:?}", &name[4..]))
            }
            "sel" | "desel" | "deselect" => {
                self.pos1 = None;
                self.pos2 = None;
                Ok("Sélection effacée".into())
            }
            "size" => {
                let r = self.need_region()?;
                let s = [
                    r.1[0] - r.0[0] + 1,
                    r.1[1] - r.0[1] + 1,
                    r.1[2] - r.0[2] + 1,
                ];
                Ok(format!(
                    "Taille {} × {} × {} = {} blocs, de {:?} à {:?}",
                    s[0],
                    s[1],
                    s[2],
                    Self::volume(r),
                    r.0,
                    r.1
                ))
            }
            "count" => {
                let mask = parse_mask(args.first().ok_or("Usage : //count <blocs>")?)?;
                let r = self.need_region()?;
                let n = Self::cells(r)
                    .filter(|p| mask.contains(&ctx.world.get(p[0], p[1], p[2])))
                    .count();
                Ok(format!("{n} bloc(s) trouvé(s)"))
            }
            "distr" => {
                let r = self.need_region()?;
                let mut m: HashMap<BlockId, usize> = HashMap::new();
                for p in Self::cells(r) {
                    *m.entry(ctx.world.get(p[0], p[1], p[2])).or_default() += 1;
                }
                let total = Self::volume(r) as f32;
                let mut v: Vec<_> = m.into_iter().collect();
                v.sort_by_key(|e| std::cmp::Reverse(e.1));
                Ok(v.iter()
                    .take(8)
                    .map(|(b, n)| format!("{} {:.1} %", b.name(), *n as f32 * 100.0 / total))
                    .collect::<Vec<_>>()
                    .join(", "))
            }
            "set" => {
                let pat = parse_pattern(args.first().ok_or("Usage : //set <motif>")?)?;
                let r = self.need_region()?;
                let ch = Self::cells(r).map(|p| (p, pat.pick(p))).collect();
                changed(self.apply(ctx, ch)?)
            }
            "replace" | "rep" => {
                let (mask, pat) = match args {
                    [to] => (None, parse_pattern(to)?),
                    [from, to, ..] => (Some(parse_mask(from)?), parse_pattern(to)?),
                    _ => return Err("Usage : //replace [<de>] <vers>".into()),
                };
                let r = self.need_region()?;
                let ch = Self::cells(r)
                    .filter(|p| {
                        let b = ctx.world.get(p[0], p[1], p[2]);
                        match &mask {
                            Some(m) => m.contains(&b),
                            None => b != BlockId::AIR,
                        }
                    })
                    .map(|p| (p, pat.pick(p)))
                    .collect();
                changed(self.apply(ctx, ch)?)
            }
            "walls" | "faces" | "outline" => {
                let pat = parse_pattern(args.first().ok_or("Usage : //walls <motif>")?)?;
                let (a, b) = self.need_region()?;
                let all = name != "walls";
                let ch = Self::cells((a, b))
                    .filter(|p| {
                        p[0] == a[0]
                            || p[0] == b[0]
                            || p[2] == a[2]
                            || p[2] == b[2]
                            || (all && (p[1] == a[1] || p[1] == b[1]))
                    })
                    .map(|p| (p, pat.pick(p)))
                    .collect();
                changed(self.apply(ctx, ch)?)
            }
            "overlay" => {
                let pat = parse_pattern(args.first().ok_or("Usage : //overlay <motif>")?)?;
                let (a, b) = self.need_region()?;
                let mut ch = Vec::new();
                for z in a[2]..=b[2] {
                    for x in a[0]..=b[0] {
                        // Premier bloc plein en descendant : on pose dessus.
                        if let Some(y) = (a[1]..=b[1])
                            .rev()
                            .find(|&y| ctx.world.get(x, y, z) != BlockId::AIR)
                        {
                            ch.push(([x, y + 1, z], pat.pick([x, y + 1, z])));
                        }
                    }
                }
                changed(self.apply(ctx, ch)?)
            }
            "center" | "middle" => {
                let pat = parse_pattern(args.first().ok_or("Usage : //center <motif>")?)?;
                let (a, b) = self.need_region()?;
                let lo = |i: usize| (a[i] + b[i]).div_euclid(2);
                let hi = |i: usize| (a[i] + b[i] + 1).div_euclid(2);
                let ch = Self::cells(([lo(0), lo(1), lo(2)], [hi(0), hi(1), hi(2)]))
                    .map(|p| (p, pat.pick(p)))
                    .collect();
                changed(self.apply(ctx, ch)?)
            }
            "naturalize" => {
                // Dessus en herbe, 3 blocs de terre, puis pierre (sur les blocs non vides).
                let (a, b) = self.need_region()?;
                let (grass, dirt) = (
                    parse_block("grass").unwrap_or(BlockId::STONE),
                    parse_block("dirt").unwrap_or(BlockId::STONE),
                );
                let mut ch = Vec::new();
                for z in a[2]..=b[2] {
                    for x in a[0]..=b[0] {
                        let mut depth = 0;
                        for y in (a[1]..=b[1]).rev() {
                            if ctx.world.get(x, y, z) == BlockId::AIR {
                                depth = 0;
                                continue;
                            }
                            let nb = match depth {
                                0 => grass,
                                1..=3 => dirt,
                                _ => BlockId::STONE,
                            };
                            ch.push(([x, y, z], nb));
                            depth += 1;
                        }
                    }
                }
                changed(self.apply(ctx, ch)?)
            }
            "line" => {
                let pat = parse_pattern(args.first().ok_or("Usage : //line <motif>")?)?;
                let (p1, p2) = (
                    self.pos1.ok_or("//pos1 manquant")?,
                    self.pos2.ok_or("//pos2 manquant")?,
                );
                let n = (0..3)
                    .map(|i| (p2[i] - p1[i]).abs())
                    .max()
                    .unwrap_or(0)
                    .max(1);
                let ch = (0..=n)
                    .map(|k| {
                        let t = k as f32 / n as f32;
                        let p = std::array::from_fn(|i| {
                            (p1[i] as f32 + (p2[i] - p1[i]) as f32 * t).round() as i32
                        });
                        (p, pat.pick(p))
                    })
                    .collect();
                changed(self.apply(ctx, ch)?)
            }
            "drain" => {
                let r = num(args.first().copied(), 8)?.clamp(1, 64);
                let water: HashSet<BlockId> = registry()
                    .defs
                    .iter()
                    .enumerate()
                    .filter(|(_, d)| d.key.contains("water") || d.key.contains("lava"))
                    .map(|(i, _)| BlockId(i as u16))
                    .collect();
                let c = ctx.feet;
                let ch = Self::cells((
                    [c[0] - r, c[1] - r, c[2] - r],
                    [c[0] + r, c[1] + r, c[2] + r],
                ))
                .filter(|p| water.contains(&ctx.world.get(p[0], p[1], p[2])))
                .map(|p| (p, BlockId::AIR))
                .collect();
                changed(self.apply(ctx, ch)?)
            }
            "replacenear" => {
                let [r, from, to, ..] = args else {
                    return Err("Usage : //replacenear <rayon> <de> <vers>".into());
                };
                let r = num(Some(r), 8)?.clamp(1, 64);
                let (mask, pat) = (parse_mask(from)?, parse_pattern(to)?);
                let c = ctx.feet;
                let ch = Self::cells((
                    [c[0] - r, c[1] - r, c[2] - r],
                    [c[0] + r, c[1] + r, c[2] + r],
                ))
                .filter(|p| mask.contains(&ctx.world.get(p[0], p[1], p[2])))
                .map(|p| (p, pat.pick(p)))
                .collect();
                changed(self.apply(ctx, ch)?)
            }
            "sphere" | "hsphere" => {
                let [pat, r, ..] = args else {
                    return Err(format!("Usage : //{name} <motif> <rayon>"));
                };
                let pat = parse_pattern(pat)?;
                let r: f32 = r.parse().map_err(|_| "rayon illisible")?;
                let hollow = name == "hsphere";
                let c = ctx.feet;
                let ri = r.ceil() as i32;
                let ch = Self::cells((
                    [c[0] - ri, c[1] - ri, c[2] - ri],
                    [c[0] + ri, c[1] + ri, c[2] + ri],
                ))
                .filter(|p| {
                    let d = (((p[0] - c[0]).pow(2) + (p[1] - c[1]).pow(2) + (p[2] - c[2]).pow(2))
                        as f32)
                        .sqrt();
                    d <= r + 0.5 && (!hollow || d > r - 0.5)
                })
                .map(|p| (p, pat.pick(p)))
                .collect();
                changed(self.apply(ctx, ch)?)
            }
            "cyl" | "hcyl" => {
                let [pat, r, rest @ ..] = args else {
                    return Err(format!("Usage : //{name} <motif> <rayon> [hauteur]"));
                };
                let pat = parse_pattern(pat)?;
                let r: f32 = r.parse().map_err(|_| "rayon illisible")?;
                let h = num(rest.first().copied(), 1)?.max(1);
                let hollow = name == "hcyl";
                let c = ctx.feet;
                let ri = r.ceil() as i32;
                let ch = Self::cells((
                    [c[0] - ri, c[1], c[2] - ri],
                    [c[0] + ri, c[1] + h - 1, c[2] + ri],
                ))
                .filter(|p| {
                    let d = (((p[0] - c[0]).pow(2) + (p[2] - c[2]).pow(2)) as f32).sqrt();
                    d <= r + 0.5 && (!hollow || d > r - 0.5)
                })
                .map(|p| (p, pat.pick(p)))
                .collect();
                changed(self.apply(ctx, ch)?)
            }
            "pyramid" | "hpyramid" => {
                let [pat, size, ..] = args else {
                    return Err(format!("Usage : //{name} <motif> <taille>"));
                };
                let pat = parse_pattern(pat)?;
                let n: i32 = size.parse().map_err(|_| "taille illisible")?;
                let hollow = name == "hpyramid";
                let c = ctx.feet;
                let mut ch = Vec::new();
                for dy in 0..n {
                    let w = n - 1 - dy;
                    for dz in -w..=w {
                        for dx in -w..=w {
                            if hollow && dx.abs() != w && dz.abs() != w && dy != 0 {
                                continue;
                            }
                            let p = [c[0] + dx, c[1] + dy, c[2] + dz];
                            ch.push((p, pat.pick(p)));
                        }
                    }
                }
                changed(self.apply(ctx, ch)?)
            }
            "expand" | "contract" | "shift" => {
                let n = num(args.first().copied(), 1)?;
                let (mut a, mut b) = self.need_region()?;
                if name == "expand" && args.get(1).map(|s| fold(s)).as_deref() == Some("vert") {
                    a[1] = 0;
                    b[1] = CHUNK_HEIGHT as i32 - 1;
                } else {
                    let d = parse_dir(args.get(1).copied(), ctx.look)?;
                    for i in 0..3 {
                        let k = d[i] * n;
                        match name {
                            "shift" => {
                                a[i] += k;
                                b[i] += k;
                            }
                            "expand" if k > 0 => b[i] += k,
                            "expand" => a[i] += k,
                            "contract" if k > 0 => a[i] = (a[i] + k).min(b[i]),
                            _ => b[i] = (b[i] + k).max(a[i]),
                        }
                    }
                }
                self.pos1 = Some(a);
                self.pos2 = Some(b);
                Ok(format!(
                    "Sélection : {a:?} à {b:?} ({} blocs)",
                    Self::volume((a, b))
                ))
            }
            "move" | "stack" => {
                let n = num(args.first().copied(), 1)?.max(1);
                let d = parse_dir(args.get(1).copied(), ctx.look)?;
                let r = self.need_region()?;
                let size = [
                    r.1[0] - r.0[0] + 1,
                    r.1[1] - r.0[1] + 1,
                    r.1[2] - r.0[2] + 1,
                ];
                let src: Vec<(Pos, BlockId)> = Self::cells(r)
                    .map(|p| (p, ctx.world.get(p[0], p[1], p[2])))
                    .collect();
                let mut ch: Vec<(Pos, BlockId)> = Vec::new();
                if name == "move" {
                    let off = scale(d, n);
                    let dest: HashSet<Pos> = src.iter().map(|(p, _)| add(*p, off)).collect();
                    ch.extend(
                        src.iter()
                            .filter(|(p, _)| !dest.contains(p))
                            .map(|(p, _)| (*p, BlockId::AIR)),
                    );
                    ch.extend(src.iter().map(|(p, b)| (add(*p, off), *b)));
                    self.pos1 = self.pos1.map(|p| add(p, off));
                    self.pos2 = self.pos2.map(|p| add(p, off));
                } else {
                    let step = [d[0] * size[0], d[1] * size[1], d[2] * size[2]];
                    for k in 1..=n {
                        ch.extend(src.iter().map(|(p, b)| (add(*p, scale(step, k)), *b)));
                    }
                }
                changed(self.apply(ctx, ch)?)
            }
            "copy" | "cut" => {
                let r = self.need_region()?;
                let f = ctx.feet;
                let blocks: Vec<(Pos, BlockId)> = Self::cells(r)
                    .map(|p| {
                        (
                            [p[0] - f[0], p[1] - f[1], p[2] - f[2]],
                            ctx.world.get(p[0], p[1], p[2]),
                        )
                    })
                    .collect();
                let n = blocks.len();
                self.clipboard = Some(Clipboard { blocks });
                if name == "cut" {
                    let ch = Self::cells(r).map(|p| (p, BlockId::AIR)).collect();
                    self.apply(ctx, ch)?;
                }
                Ok(format!("{n} bloc(s) dans le presse-papiers"))
            }
            "paste" => {
                let skip_air = args.contains(&"-a");
                let clip = self
                    .clipboard
                    .as_ref()
                    .ok_or("Presse-papiers vide : //copy")?;
                let f = ctx.feet;
                let ch = clip
                    .blocks
                    .iter()
                    .filter(|(_, b)| !(skip_air && *b == BlockId::AIR))
                    .map(|(p, b)| (add(f, *p), *b))
                    .collect();
                changed(self.apply(ctx, ch)?)
            }
            "rotate" => {
                let deg = num(args.first().copied(), 90)?;
                if deg % 90 != 0 {
                    return Err("Rotation par multiples de 90°".into());
                }
                let clip = self
                    .clipboard
                    .as_mut()
                    .ok_or("Presse-papiers vide : //copy")?;
                let turns = (deg / 90).rem_euclid(4);
                for (p, _) in &mut clip.blocks {
                    for _ in 0..turns {
                        // Sens des aiguilles d'une montre vu du dessus.
                        *p = [-p[2], p[1], p[0]];
                    }
                }
                Ok(format!("Presse-papiers tourné de {deg}°"))
            }
            "flip" => {
                let d = parse_dir(args.first().copied(), ctx.look)?;
                let clip = self
                    .clipboard
                    .as_mut()
                    .ok_or("Presse-papiers vide : //copy")?;
                let axis = (0..3).find(|&i| d[i] != 0).unwrap_or(0);
                for (p, _) in &mut clip.blocks {
                    p[axis] = -p[axis];
                }
                Ok("Presse-papiers retourné".into())
            }
            "undo" | "redo" => {
                let n = num(args.first().copied(), 1)?.max(1);
                let mut done = 0;
                for _ in 0..n {
                    let ops = if name == "undo" {
                        self.undo.pop()
                    } else {
                        self.redo.pop()
                    };
                    let Some(ops) = ops else { break };
                    let mut back = Vec::with_capacity(ops.len());
                    for (p, b) in ops.into_iter().rev() {
                        back.push((p, ctx.world.get(p[0], p[1], p[2])));
                        let d = if b.is_torch() {
                            ctx.world.place_torch_block(p[0], p[1], p[2], b, None)
                        } else {
                            ctx.world.set(p[0], p[1], p[2], b)
                        };
                        ctx.dirty.extend(d);
                    }
                    if name == "undo" {
                        self.redo.push(back);
                    } else {
                        self.undo.push(back);
                    }
                    done += 1;
                }
                if done == 0 {
                    Err(format!(
                        "Rien à {}",
                        if name == "undo" { "annuler" } else { "refaire" }
                    ))
                } else {
                    Ok(format!(
                        "{done} opération(s) {}",
                        if name == "undo" {
                            "annulée(s)"
                        } else {
                            "refaite(s)"
                        }
                    ))
                }
            }
            _ => Err(format!("Commande inconnue : //{name} (//help)")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn world() -> World {
        let mut w = World::new(0);
        for dz in -1..=1 {
            for dx in -1..=1 {
                w.insert_empty(ChunkPos { x: dx, z: dz });
            }
        }
        w
    }

    fn run(we: &mut WorldEdit, w: &mut World, line: &str) -> Reply {
        let mut dirty = HashSet::new();
        let mut parts = line.split_whitespace();
        let name = parts.next().unwrap().trim_start_matches('/');
        let args: Vec<&str> = parts.collect();
        let mut ctx = Ctx {
            world: w,
            feet: [8, 100, 8],
            look: [0.0, 0.0, -1.0],
            target: Some([8, 99, 4]),
            dirty: &mut dirty,
        };
        we.command(name, &args, &mut ctx)
    }

    #[test]
    fn set_replace_undo_redo() {
        let (mut we, mut w) = (WorldEdit::default(), world());
        we.pos1 = Some([0, 100, 0]);
        we.pos2 = Some([3, 101, 2]);
        assert!(run(&mut we, &mut w, "//size").unwrap().contains("24 blocs"));
        run(&mut we, &mut w, "//set stone").unwrap();
        assert_eq!(w.get(3, 101, 2), BlockId::STONE);
        run(&mut we, &mut w, "//replace stone brick").unwrap();
        assert_eq!(w.get(0, 100, 0), BlockId::BRICK);
        assert!(run(&mut we, &mut w, "//count brick")
            .unwrap()
            .starts_with("24"));
        run(&mut we, &mut w, "//undo").unwrap();
        assert_eq!(w.get(0, 100, 0), BlockId::STONE);
        run(&mut we, &mut w, "//undo").unwrap();
        assert_eq!(w.get(0, 100, 0), BlockId::AIR);
        run(&mut we, &mut w, "//redo 2").unwrap();
        assert_eq!(w.get(0, 100, 0), BlockId::BRICK);
        assert!(run(&mut we, &mut w, "//set nimporte").is_err());
    }

    #[test]
    fn copy_paste_rotate_move_stack() {
        let (mut we, mut w) = (WorldEdit::default(), world());
        w.set(9, 100, 8, BlockId::STONE);
        w.set(10, 100, 8, BlockId::BRICK);
        we.pos1 = Some([9, 100, 8]);
        we.pos2 = Some([10, 100, 8]);
        run(&mut we, &mut w, "//copy").unwrap();
        run(&mut we, &mut w, "//rotate 90").unwrap();
        // Joueur en (8, 100, 8) : le bloc à +1 en x passe à +1 en z.
        we.pos1 = None;
        let mut dirty = HashSet::new();
        let mut ctx = Ctx {
            world: &mut w,
            feet: [8, 105, 8],
            look: [0.0, 0.0, -1.0],
            target: None,
            dirty: &mut dirty,
        };
        we.command("paste", &[], &mut ctx).unwrap();
        assert_eq!(w.get(8, 105, 9), BlockId::STONE);
        assert_eq!(w.get(8, 105, 10), BlockId::BRICK);
        we.pos1 = Some([9, 100, 8]);
        we.pos2 = Some([10, 100, 8]);
        run(&mut we, &mut w, "//stack 2 east").unwrap();
        assert_eq!(w.get(13, 100, 8), BlockId::STONE);
        assert_eq!(w.get(14, 100, 8), BlockId::BRICK);
        run(&mut we, &mut w, "//move 3 up").unwrap();
        assert_eq!(w.get(9, 100, 8), BlockId::AIR);
        assert_eq!(w.get(9, 103, 8), BlockId::STONE);
        assert_eq!(we.pos1, Some([9, 103, 8]));
    }

    #[test]
    fn shapes_and_patterns() {
        let (mut we, mut w) = (WorldEdit::default(), world());
        run(&mut we, &mut w, "//sphere stone 3").unwrap();
        assert_eq!(w.get(8, 103, 8), BlockId::STONE);
        assert_eq!(w.get(11, 103, 11), BlockId::AIR);
        run(&mut we, &mut w, "//undo").unwrap();
        run(&mut we, &mut w, "//hcyl brick 4 3").unwrap();
        assert_eq!(w.get(12, 102, 8), BlockId::BRICK);
        assert_eq!(w.get(8, 101, 8), BlockId::AIR, "creux");
        run(&mut we, &mut w, "//pyramid stone 3").unwrap();
        assert_eq!(w.get(8, 102, 8), BlockId::STONE);
        we.pos1 = Some([0, 120, 0]);
        we.pos2 = Some([15, 120, 15]);
        run(&mut we, &mut w, "//set 50%stone,50%brick").unwrap();
        let stone = (0..16)
            .flat_map(|x| (0..16).map(move |z| (x, z)))
            .filter(|&(x, z)| w.get(x, 120, z) == BlockId::STONE)
            .count();
        assert!((80..176).contains(&stone), "{stone}");
        assert_eq!(parse_block("pierre"), Some(BlockId::STONE));
        assert_eq!(parse_block("minecraft:stone"), Some(BlockId::STONE));
    }
}
