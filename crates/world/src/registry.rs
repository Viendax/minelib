//! Registre des blocs : forme, textures, propriétés physiques et lumière de chaque bloc.
//!
//! Les identifiants sont **stables** (ils sont enregistrés dans les sauvegardes) : le
//! registre est construit toujours dans le même ordre, les 32 premiers identifiants sont
//! les blocs de base de Minelib, puis viennent les familles (couleurs, bois, pierres,
//! dalles, escaliers, clôtures…). Ne jamais réordonner : seulement ajouter à la fin.
//!
//! Les textures sont désignées par leur nom dans `textures/blocks/` du pack, ou générées
//! (couleur unie légèrement bruitée, texture teintée) quand le pack n'en a pas.

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::BlockId;

/// Source d'une couche de texture.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum TexSrc {
    /// `textures/blocks/<nom>.png` du pack.
    Pack(&'static str),
    /// Couleur unie (sRGB) avec un léger grain.
    Flat([u8; 3]),
    /// Texture du pack multipliée par une couleur (feuillage, lianes…).
    Tint(&'static str, [u8; 3]),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PostKind {
    Fence,
    Wall,
    Pane,
}

/// Forme d'un bloc. Dimensions en seizièmes de bloc.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    Air,
    /// Invisible mais solide (barrière).
    Invisible,
    Cube,
    Torch,
    /// Torche contre un mur, penchée ; le mur est du côté `0` +X, `1` −X, `4` +Z, `5` −Z.
    WallTorch(u8),
    /// Plaque contre une face : `side` = 0 +X, 1 −X, 2 +Y (plafond), 3 −Y (sol), 4 +Z, 5 −Z.
    Panel {
        side: u8,
        thick: u8,
        y0: u8,
        y1: u8,
    },
    /// Escalier : demi-dalle + marche du côté `dir` (0 +X, 1 −X, 2 +Z, 3 −Z) ; `top` = retourné.
    Stairs {
        dir: u8,
        top: bool,
    },
    /// Poteau qui se relie aux voisins (clôture, muret, vitre/barreaux).
    Post(PostKind),
    Box {
        min: [u8; 3],
        max: [u8; 3],
    },
    /// Deux plans en croix (plantes).
    Cross,
}

#[derive(Clone, Debug)]
pub struct BlockDef {
    /// Clé unique (`stone`, `slab:planks_oak:top`…).
    pub key: String,
    /// Nom affiché.
    pub name: String,
    pub shape: Shape,
    /// Couches de texture : dessus, côtés, dessous.
    pub tex: [u16; 3],
    /// Cube plein et opaque : cache ses voisins et arrête la lumière.
    pub full: bool,
    /// Bloque les déplacements.
    pub solid: bool,
    /// Peut être visé (casser, poser contre).
    pub target: bool,
    /// Faces cachées entre deux blocs identiques (verre, eau, feuilles).
    pub cull_same: bool,
    /// Lumière émise (sRGB, intensité 0..255 par canal).
    pub light: Option<[u8; 3]>,
    /// Semi-transparent (eau, glace, verre teinté) : dessiné dans la passe translucide.
    pub translucent: bool,
    /// Reflet (0 = mat … 15 = miroir parfait), pour le raytracing.
    pub reflect: u8,
    /// Métal (fer, or, diamant) : reflet court et flou (voir `mesh::REFLECT_SHORT`).
    pub metal: bool,
}

impl BlockDef {
    /// Boîtes (en seizièmes) du bloc, sans tenir compte des voisins.
    pub fn boxes(&self) -> Vec<([u8; 3], [u8; 3])> {
        match self.shape {
            Shape::Air => vec![],
            Shape::Invisible | Shape::Cube => vec![([0, 0, 0], [16, 16, 16])],
            // Zone visée / boîte de la torche (bâton 8 px + flamme 5 px).
            Shape::Torch => vec![([6, 0, 6], [10, 13, 10])],
            // Mur en +Z dans le repère de base, tourné vers le vrai mur.
            Shape::WallTorch(d) => {
                let (a, b) = ([5u8, 3, 10], [11u8, 15, 16]);
                let (p, q) = (wall_rot(d, [a[0], a[2]]), wall_rot(d, [b[0], b[2]]));
                vec![(
                    [p[0].min(q[0]), a[1], p[1].min(q[1])],
                    [p[0].max(q[0]), b[1], p[1].max(q[1])],
                )]
            }
            Shape::Panel {
                side,
                thick,
                y0,
                y1,
            } => vec![panel_box(side, thick, y0, y1)],
            Shape::Stairs { dir, top } => stairs_boxes(dir, top),
            Shape::Post(k) => vec![post_center(k)],
            Shape::Box { min, max } => vec![(min, max)],
            Shape::Cross => vec![([2, 0, 2], [14, 14, 14])],
        }
    }
}

pub fn panel_box(side: u8, t: u8, y0: u8, y1: u8) -> ([u8; 3], [u8; 3]) {
    match side {
        0 => ([16 - t, y0, 0], [16, y1, 16]),
        1 => ([0, y0, 0], [t, y1, 16]),
        2 => ([0, 16 - t, 0], [16, 16, 16]),
        3 => ([0, 0, 0], [16, t, 16]),
        4 => ([0, y0, 16 - t], [16, y1, 16]),
        _ => ([0, y0, 0], [16, y1, t]),
    }
}

pub fn stairs_boxes(dir: u8, top: bool) -> Vec<([u8; 3], [u8; 3])> {
    let (slab, step_y) = if top {
        (([0, 8, 0], [16, 16, 16]), (0, 8))
    } else {
        (([0, 0, 0], [16, 8, 16]), (8, 16))
    };
    let (y0, y1) = step_y;
    let step = match dir {
        0 => ([8, y0, 0], [16, y1, 16]),
        1 => ([0, y0, 0], [8, y1, 16]),
        2 => ([0, y0, 8], [16, y1, 16]),
        _ => ([0, y0, 0], [16, y1, 8]),
    };
    vec![slab, step]
}

pub fn post_center(k: PostKind) -> ([u8; 3], [u8; 3]) {
    match k {
        PostKind::Fence => ([6, 0, 6], [10, 16, 10]),
        PostKind::Wall => ([4, 0, 4], [12, 16, 12]),
        PostKind::Pane => ([7, 0, 7], [9, 16, 9]),
    }
}

/// Bras d'un poteau vers le côté horizontal `side` (0 +X, 1 −X, 4 +Z, 5 −Z).
pub fn post_arms(k: PostKind, side: u8) -> Vec<([u8; 3], [u8; 3])> {
    let along = |a0: u8, a1: u8, w0: u8, w1: u8, y0: u8, y1: u8| -> ([u8; 3], [u8; 3]) {
        match side {
            0 => ([a1, y0, w0], [16, y1, w1]),
            1 => ([0, y0, w0], [a0, y1, w1]),
            4 => ([w0, y0, a1], [w1, y1, 16]),
            _ => ([w0, y0, 0], [w1, y1, a0]),
        }
    };
    match k {
        PostKind::Fence => vec![along(6, 10, 7, 9, 6, 9), along(6, 10, 7, 9, 12, 15)],
        PostKind::Wall => vec![along(4, 12, 5, 11, 0, 13)],
        PostKind::Pane => vec![along(7, 9, 7, 9, 0, 16)],
    }
}

pub struct Registry {
    pub defs: Vec<BlockDef>,
    pub textures: Vec<TexSrc>,
    keys: HashMap<String, u16>,
    legacy: HashMap<(u16, u8), u16>,
}

impl Registry {
    pub fn get(&self, id: BlockId) -> &BlockDef {
        self.defs.get(id.0 as usize).unwrap_or(&self.defs[0])
    }

    pub fn by_key(&self, key: &str) -> Option<BlockId> {
        self.keys.get(key).map(|&i| BlockId(i))
    }

    /// Bloc correspondant à un ancien identifiant Minecraft (avant 1.13) et sa valeur `meta`.
    pub fn legacy(&self, id: u16, meta: u8) -> Option<BlockId> {
        self.legacy
            .get(&(id, meta))
            .or_else(|| self.legacy.get(&(id, 255)))
            .map(|&i| BlockId(i))
    }

    pub fn len(&self) -> usize {
        self.defs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.defs.is_empty()
    }
}

pub fn registry() -> &'static Registry {
    static R: OnceLock<Registry> = OnceLock::new();
    R.get_or_init(build)
}

/// Couverture des faces du bloc par de la matière qui arrête la lumière des torches :
/// pour chaque face (0 +X, 1 −X, 2 +Y, 3 −Y, 4 +Z, 5 −Z), une grille 4 × 4 de quarts
/// de face (bit = quart couvert). Un cube plein couvre tout ; une dalle du bas couvre
/// son dessous et la moitié basse de ses côtés. La lumière passe entre deux cases tant
/// que leurs couvertures réunies laissent une ouverture.
pub fn light_cover(id: BlockId) -> [u16; 6] {
    static M: OnceLock<Vec<[u16; 6]>> = OnceLock::new();
    let m = M.get_or_init(|| registry().defs.iter().map(def_light_cover).collect());
    m.get(id.0 as usize).copied().unwrap_or([0; 6])
}

/// Faces entièrement couvertes (bit `k` = face `k`).
pub fn light_faces(id: BlockId) -> u8 {
    light_cover(id)
        .iter()
        .enumerate()
        .filter(|(_, &c)| c == u16::MAX)
        .fold(0, |m, (k, _)| m | 1 << k)
}

fn def_light_cover(d: &BlockDef) -> [u16; 6] {
    if d.full {
        return [u16::MAX; 6];
    }
    // Seules les dalles (plaques d'au moins un demi-bloc), escaliers et pavés non
    // transparents arrêtent la lumière ; portes, trappes, tapis, clôtures, vitres et
    // plantes la laissent passer.
    let blocks = match d.shape {
        Shape::Panel { thick, .. } => thick >= 8,
        Shape::Box { .. } | Shape::Stairs { .. } => true,
        _ => false,
    };
    if d.translucent || !blocks {
        return [0; 6];
    }
    let boxes = d.boxes();
    std::array::from_fn(|face| {
        let axis = face / 2;
        let positive = face % 2 == 0;
        let (a, b) = ((axis + 1) % 3, (axis + 2) % 3);
        let mut bits = 0u16;
        for (mn, mx) in &boxes {
            let touches = if positive {
                mx[axis] == 16
            } else {
                mn[axis] == 0
            };
            if !touches {
                continue;
            }
            for u in 0..4u8 {
                for v in 0..4u8 {
                    let inside = mn[a] <= u * 4
                        && mx[a] >= u * 4 + 4
                        && mn[b] <= v * 4
                        && mx[b] >= v * 4 + 4;
                    if inside {
                        bits |= 1 << (u * 4 + v);
                    }
                }
            }
        }
        bits
    })
}

// ---------------------------------------------------------------- données

/// Couleurs dans l'ordre des teintures (méta 0..15) et noms des textures du pack.
pub const DYES: [&str; 16] = [
    "white",
    "orange",
    "magenta",
    "light_blue",
    "yellow",
    "lime",
    "pink",
    "gray",
    "silver",
    "cyan",
    "purple",
    "blue",
    "brown",
    "green",
    "red",
    "black",
];
pub const DYES_FR: [&str; 16] = [
    "blanc",
    "orange",
    "magenta",
    "bleu clair",
    "jaune",
    "vert clair",
    "rose",
    "gris",
    "gris clair",
    "cyan",
    "violet",
    "bleu",
    "marron",
    "vert",
    "rouge",
    "noir",
];
/// Couleur moyenne de chaque teinture (béton), sRGB.
pub const DYE_RGB: [[u8; 3]; 16] = [
    [207, 213, 214],
    [224, 97, 1],
    [169, 48, 159],
    [36, 137, 199],
    [241, 175, 21],
    [94, 169, 24],
    [214, 101, 143],
    [55, 58, 62],
    [125, 125, 115],
    [21, 119, 136],
    [100, 32, 156],
    [45, 47, 143],
    [96, 60, 32],
    [73, 91, 36],
    [142, 33, 33],
    [8, 10, 15],
];

const WOODS: [&str; 6] = ["oak", "spruce", "birch", "jungle", "acacia", "big_oak"];
const WOODS_FR: [&str; 6] = [
    "chêne",
    "sapin",
    "bouleau",
    "acajou",
    "acacia",
    "chêne noir",
];
const PLANKS: [&str; 6] = [
    "planks_oak",
    "planks_spruce",
    "planks_birch",
    "planks_jungle",
    "planks_acacia",
    "planks_big_oak",
];
const LOGS: [(&str, &str); 6] = [
    ("log_oak", "log_oak_top"),
    ("log_spruce", "log_spruce_top"),
    ("log_birch", "log_birch_top"),
    ("log_jungle", "log_jungle_top"),
    ("log_acacia", "log_acacia_top"),
    ("log_big_oak", "log_big_oak_top"),
];
const LEAVES: [&str; 6] = [
    "leaves_oak",
    "leaves_spruce",
    "leaves_birch",
    "leaves_jungle",
    "leaves_acacia",
    "leaves_big_oak",
];
const DOORS: [&str; 7] = [
    "wood", "spruce", "birch", "jungle", "acacia", "dark_oak", "iron",
];

fn mix(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    std::array::from_fn(|i| (a[i] as f32 + (b[i] as f32 - a[i] as f32) * t) as u8)
}

/// Matériau : textures (dessus, côtés, dessous) et nom.
#[derive(Clone)]
struct Mat {
    key: String,
    name: String,
    tex: [TexSrc; 3],
}

fn mat(key: &str, name: &str, top: TexSrc, side: TexSrc, bottom: TexSrc) -> Mat {
    Mat {
        key: key.into(),
        name: name.into(),
        tex: [top, side, bottom],
    }
}

fn mat1(key: &str, name: &str, t: TexSrc) -> Mat {
    mat(key, name, t.clone(), t.clone(), t)
}

struct Builder {
    r: Registry,
    tex_index: HashMap<TexSrc, u16>,
}

#[derive(Clone, Copy)]
struct Props {
    full: bool,
    solid: bool,
    target: bool,
    cull_same: bool,
    light: Option<[u8; 3]>,
    translucent: bool,
}

const SOLID: Props = Props {
    full: true,
    solid: true,
    target: true,
    cull_same: false,
    light: None,
    translucent: false,
};
const PART: Props = Props {
    full: false,
    solid: true,
    target: true,
    cull_same: false,
    light: None,
    translucent: false,
};
const DECOR: Props = Props {
    full: false,
    solid: false,
    target: true,
    cull_same: false,
    light: None,
    translucent: false,
};
/// Feuillage : on voit à travers les trous de la texture, toutes les faces sont dessinées.
const FOLIAGE: Props = Props {
    full: false,
    solid: true,
    target: true,
    cull_same: false,
    light: None,
    translucent: false,
};

impl Builder {
    fn tex(&mut self, t: &TexSrc) -> u16 {
        if let Some(&i) = self.tex_index.get(t) {
            return i;
        }
        let i = self.r.textures.len() as u16;
        self.r.textures.push(t.clone());
        self.tex_index.insert(t.clone(), i);
        i
    }

    fn add(&mut self, key: String, name: String, shape: Shape, tex: &[TexSrc; 3], p: Props) -> u16 {
        if let Some(&i) = self.r.keys.get(&key) {
            return i;
        }
        let t = [self.tex(&tex[0]), self.tex(&tex[1]), self.tex(&tex[2])];
        let id = self.r.defs.len() as u16;
        self.r.defs.push(BlockDef {
            key: key.clone(),
            name,
            shape,
            tex: t,
            full: p.full,
            solid: p.solid,
            target: p.target,
            cull_same: p.cull_same,
            light: p.light,
            translucent: p.translucent,
            reflect: 0,
            metal: false,
        });
        self.r.keys.insert(key, id);
        id
    }

    fn cube(&mut self, m: &Mat, p: Props) -> u16 {
        self.add(m.key.clone(), m.name.clone(), Shape::Cube, &m.tex, p)
    }

    fn slab(&mut self, m: &Mat, top: bool) -> u16 {
        let side = if top { 2 } else { 3 };
        let key = format!("slab:{}:{}", m.key, if top { "top" } else { "bottom" });
        self.add(
            key,
            format!("dalle de {}", m.name),
            Shape::Panel {
                side,
                thick: 8,
                y0: 0,
                y1: 16,
            },
            &m.tex,
            PART,
        )
    }

    fn stairs(&mut self, m: &Mat, dir: u8, top: bool) -> u16 {
        let key = format!(
            "stairs:{}:{dir}:{}",
            m.key,
            if top { "top" } else { "bottom" }
        );
        self.add(
            key,
            format!("escalier de {}", m.name),
            Shape::Stairs { dir, top },
            &m.tex,
            PART,
        )
    }

    /// Dalles + escaliers (8 orientations) d'un matériau.
    fn shapes(&mut self, m: &Mat) {
        self.slab(m, false);
        self.slab(m, true);
        for dir in 0..4 {
            for top in [false, true] {
                self.stairs(m, dir, top);
            }
        }
    }

    fn legacy(&mut self, id: u16, meta: u8, block: u16) {
        self.r.legacy.insert((id, meta), block);
    }

    /// Même bloc pour toutes les valeurs `meta`.
    fn legacy_all(&mut self, id: u16, block: u16) {
        self.r.legacy.insert((id, 255), block);
    }
}

fn t3(t: TexSrc) -> [TexSrc; 3] {
    [t.clone(), t.clone(), t]
}

fn p(n: &'static str) -> TexSrc {
    TexSrc::Pack(n)
}

fn build() -> Registry {
    let mut b = Builder {
        r: Registry {
            defs: Vec::new(),
            textures: Vec::new(),
            keys: HashMap::new(),
            legacy: HashMap::new(),
        },
        tex_index: HashMap::new(),
    };
    // Couche 0 : texture manquante.
    b.tex(&p("missing"));

    // ---- 0..31 : blocs de base de Minelib (identifiants historiques, ne pas changer).
    let air = TexSrc::Flat([0, 0, 0]);
    let none = Props {
        full: false,
        solid: false,
        target: false,
        cull_same: false,
        light: None,
        translucent: false,
    };
    b.add(
        "air".into(),
        "air".into(),
        Shape::Air,
        &[air.clone(), air.clone(), air.clone()],
        none,
    );
    let stone = mat1("stone", "pierre", p("stone"));
    let base: Vec<(Mat, Props)> = vec![
        (stone.clone(), SOLID),
        (mat1("dirt", "terre", p("dirt")), SOLID),
        (
            mat("grass", "herbe", p("grass_top"), p("grass_side"), p("dirt")),
            SOLID,
        ),
        (mat1("sand", "sable", p("sand")), SOLID),
        (
            mat(
                "log_oak",
                "bois",
                p("log_oak_top"),
                p("log_oak"),
                p("log_oak_top"),
            ),
            SOLID,
        ),
        (mat1("leaves_oak", "feuilles", p("leaves_oak")), FOLIAGE),
        (mat1("snow", "neige", p("snow")), SOLID),
        (mat1("bedrock", "socle", p("bedrock")), SOLID),
        (mat1("planks_oak", "planches", p("planks_oak")), SOLID),
        (mat1("brick", "briques", p("brick")), SOLID),
    ];
    for (m, pr) in &base {
        b.cube(m, *pr);
    }
    // Dessous du bâton : bois uni (la texture de la torche n'a rien de bon à cet endroit).
    let torch_tex = [p("torch_on"), p("torch_on"), TexSrc::Flat([92, 66, 38])];
    b.add(
        "torch".into(),
        "torche".into(),
        Shape::Torch,
        &torch_tex,
        DECOR,
    ); // 11
    b.add(
        "flame".into(),
        "flamme".into(),
        Shape::Air,
        &torch_tex,
        none,
    ); // 12
    let more: Vec<(Mat, Props)> = vec![
        (
            mat1("cobblestone", "pierre taillée", p("cobblestone")),
            SOLID,
        ),
        (mat1("gravel", "gravier", p("gravel")), SOLID),
        (
            mat1("water", "eau", p("water_still")),
            Props {
                full: false,
                solid: false,
                target: false,
                cull_same: true,
                light: None,
                translucent: true,
            },
        ),
        (
            mat1("lava", "lave", p("lava_still")),
            Props {
                full: true,
                solid: false,
                target: false,
                cull_same: true,
                light: Some([255, 120, 40]),
                translucent: false,
            },
        ),
        (
            mat(
                "sandstone",
                "grès",
                p("sandstone_top"),
                p("sandstone_normal"),
                p("sandstone_bottom"),
            ),
            SOLID,
        ),
        (mat1("clay", "argile", p("clay")), SOLID),
        (mat1("coal_ore", "minerai de charbon", p("coal_ore")), SOLID),
        (mat1("iron_ore", "minerai de fer", p("iron_ore")), SOLID),
        (mat1("gold_ore", "minerai d'or", p("gold_ore")), SOLID),
        (
            mat1("diamond_ore", "minerai de diamant", p("diamond_ore")),
            SOLID,
        ),
        (
            mat1("glass", "verre", p("glass")),
            Props {
                full: false,
                solid: true,
                target: true,
                cull_same: true,
                light: None,
                translucent: false,
            },
        ),
        (
            mat1("ice", "glace", p("ice")),
            Props {
                full: false,
                solid: true,
                target: true,
                cull_same: true,
                light: None,
                translucent: true,
            },
        ),
        (mat1("obsidian", "obsidienne", p("obsidian")), SOLID),
        (
            mat1("stonebrick", "briques de pierre", p("stonebrick")),
            SOLID,
        ),
        (
            mat1(
                "cobblestone_mossy",
                "pierre moussue",
                p("cobblestone_mossy"),
            ),
            SOLID,
        ),
        (
            mat1("hardened_clay", "terre cuite", p("hardened_clay")),
            SOLID,
        ),
        (
            mat1("wool_white", "laine blanche", p("wool_colored_white")),
            SOLID,
        ),
    ];
    for (m, pr) in &more {
        b.cube(m, *pr);
    }
    // 30 : barrière (invisible, solide) ; 31 : réservé.
    b.add(
        "barrier".into(),
        "barrière".into(),
        Shape::Invisible,
        &[air.clone(), air.clone(), air.clone()],
        PART,
    );
    b.add(
        "reserved31".into(),
        "réservé".into(),
        Shape::Air,
        &[air.clone(), air.clone(), air],
        none,
    );
    debug_assert_eq!(b.r.defs.len(), 32);

    // ---- Matériaux supplémentaires (cubes) + dalles et escaliers.
    let mut mats: Vec<Mat> = vec![
        mat1("stone_granite", "granite", p("stone_granite")),
        mat1(
            "stone_granite_smooth",
            "granite poli",
            p("stone_granite_smooth"),
        ),
        mat1("stone_diorite", "diorite", p("stone_diorite")),
        mat1(
            "stone_diorite_smooth",
            "diorite polie",
            p("stone_diorite_smooth"),
        ),
        mat1("stone_andesite", "andésite", p("stone_andesite")),
        mat1(
            "stone_andesite_smooth",
            "andésite polie",
            p("stone_andesite_smooth"),
        ),
        mat(
            "smooth_stone",
            "pierre lisse",
            p("stone_slab_top"),
            p("stone_slab_side"),
            p("stone_slab_top"),
        ),
        mat(
            "sandstone_carved",
            "grès sculpté",
            p("sandstone_top"),
            p("sandstone_carved"),
            p("sandstone_top"),
        ),
        mat(
            "sandstone_smooth",
            "grès lisse",
            p("sandstone_top"),
            p("sandstone_smooth"),
            p("sandstone_top"),
        ),
        mat(
            "red_sandstone",
            "grès rouge",
            p("red_sandstone_top"),
            p("red_sandstone_normal"),
            p("red_sandstone_bottom"),
        ),
        mat(
            "red_sandstone_carved",
            "grès rouge sculpté",
            p("red_sandstone_top"),
            p("red_sandstone_carved"),
            p("red_sandstone_top"),
        ),
        mat(
            "red_sandstone_smooth",
            "grès rouge lisse",
            p("red_sandstone_top"),
            p("red_sandstone_smooth"),
            p("red_sandstone_top"),
        ),
        mat1("red_sand", "sable rouge", p("red_sand")),
        mat(
            "quartz",
            "quartz",
            p("quartz_block_top"),
            p("quartz_block_side"),
            p("quartz_block_bottom"),
        ),
        mat(
            "quartz_chiseled",
            "quartz sculpté",
            p("quartz_block_chiseled_top"),
            p("quartz_block_chiseled"),
            p("quartz_block_chiseled_top"),
        ),
        mat(
            "quartz_pillar",
            "pilier de quartz",
            p("quartz_block_lines_top"),
            p("quartz_block_lines"),
            p("quartz_block_lines_top"),
        ),
        mat1(
            "stonebrick_mossy",
            "briques de pierre moussues",
            p("stonebrick_mossy"),
        ),
        mat1(
            "stonebrick_cracked",
            "briques de pierre fissurées",
            p("stonebrick_cracked"),
        ),
        mat1(
            "stonebrick_carved",
            "briques de pierre sculptées",
            p("stonebrick_carved"),
        ),
        mat1("prismarine", "prismarine", p("prismarine_rough")),
        mat1(
            "prismarine_bricks",
            "briques de prismarine",
            p("prismarine_bricks"),
        ),
        mat1("prismarine_dark", "prismarine sombre", p("prismarine_dark")),
        mat1(
            "nether_brick",
            "briques infernales",
            TexSrc::Tint("hellrock_brick", [120, 60, 70]),
        ),
        mat1(
            "end_bricks",
            "briques de l'End",
            TexSrc::Flat([226, 230, 172]),
        ),
        mat1("purpur", "purpur", TexSrc::Flat([170, 125, 170])),
        mat1("iron_block", "bloc de fer", p("iron_block")),
        mat1("gold_block", "bloc d'or", p("gold_block")),
        mat1("diamond_block", "bloc de diamant", p("diamond_block")),
        mat1("emerald_block", "bloc d'émeraude", p("emerald_block")),
        mat1("lapis_block", "bloc de lapis", p("lapis_block")),
        mat1("coal_block", "bloc de charbon", p("coal_block")),
        mat1("packed_ice", "glace compacte", p("packed_ice")),
        mat(
            "hay",
            "botte de foin",
            p("hay_block_top"),
            p("hay_block_side"),
            p("hay_block_top"),
        ),
        mat(
            "bookshelf",
            "bibliothèque",
            p("planks_oak"),
            p("bookshelf"),
            p("planks_oak"),
        ),
        mat("tnt", "TNT", p("tnt_top"), p("tnt_side"), p("tnt_bottom")),
        mat1("netherrack", "roche infernale", p("hellrock")),
        mat1("soul_sand", "sable des âmes", p("ash_sand")),
        mat1("sponge", "éponge", p("sponge")),
        mat1("mycelium", "mycélium", p("mycelium_top")),
        mat(
            "podzol",
            "podzol",
            p("dirt_podzol_top"),
            p("dirt_podzol_side"),
            p("dirt"),
        ),
        mat1("coarse_dirt", "terre stérile", p("coarse_dirt")),
        mat(
            "crafting_table",
            "établi",
            p("crafting_table_top"),
            p("crafting_table_side"),
            p("planks_oak"),
        ),
        mat(
            "furnace",
            "four",
            p("furnace_top"),
            p("furnace_side"),
            p("furnace_top"),
        ),
        mat(
            "pumpkin",
            "citrouille",
            p("pumpkin_top"),
            p("pumpkin_side"),
            p("pumpkin_top"),
        ),
        mat(
            "melon",
            "pastèque",
            p("melon_top"),
            p("melon_side"),
            p("melon_top"),
        ),
        mat(
            "piston",
            "piston",
            p("piston_top_normal"),
            p("piston_side"),
            p("piston_bottom"),
        ),
        mat(
            "cauldron",
            "chaudron",
            p("cauldron_top"),
            p("cauldron_side"),
            p("cauldron_bottom"),
        ),
        mat1(
            "command_block",
            "bloc de commande",
            TexSrc::Flat([196, 125, 80]),
        ),
        mat1("slime", "bloc de slime", p("slime")),
        mat(
            "jukebox",
            "juke-box",
            p("jukebox_top"),
            p("jukebox_side"),
            p("jukebox_side"),
        ),
        mat1("noteblock", "bloc musical", p("noteblock")),
        mat1("obsidian_crying", "obsidienne pleureuse", p("obsidian")),
        mat1("magma", "magma", TexSrc::Flat([140, 60, 25])),
        mat1("bone_block", "bloc d'os", TexSrc::Flat([225, 220, 200])),
        mat1(
            "red_nether_brick",
            "briques infernales rouges",
            TexSrc::Tint("hellrock_brick", [150, 40, 40]),
        ),
        mat1(
            "nether_wart_block",
            "bloc de verrue",
            TexSrc::Flat([115, 10, 10]),
        ),
        mat1("observer", "observateur", TexSrc::Flat([90, 90, 90])),
        mat1("dispenser", "distributeur", p("dispenser_front_horizontal")),
        mat1("redstone_block", "bloc d'énergie", p("energy_block")),
    ];
    for (i, w) in PLANKS.iter().enumerate() {
        mats.push(mat1(w, &format!("planches de {}", WOODS_FR[i]), p(w)));
    }
    for (i, (side, top)) in LOGS.iter().enumerate() {
        mats.push(mat(
            side,
            &format!("bûche de {}", WOODS_FR[i]),
            p(top),
            p(side),
            p(top),
        ));
    }
    for (i, l) in LEAVES.iter().enumerate() {
        mats.push(mat1(
            &format!("leaves_{}", WOODS[i]),
            &format!("feuilles de {}", WOODS_FR[i]),
            p(l),
        ));
    }
    let dye_tex = |prefix: &'static str| -> Vec<TexSrc> {
        // Les noms du pack sont construits à partir de constantes : 'static via leak, une fois.
        DYES.iter()
            .map(|d| TexSrc::Pack(Box::leak(format!("{prefix}{d}").into_boxed_str())))
            .collect()
    };
    let wool = dye_tex("wool_colored_");
    let clay = dye_tex("hardened_clay_stained_");
    let glass = dye_tex("glass_");
    let pane = dye_tex("glass_pane_top_");
    for i in 0..16 {
        let c = DYES_FR[i];
        mats.push(mat1(
            &format!("wool_{}", DYES[i]),
            &format!("laine {c}"),
            wool[i].clone(),
        ));
        mats.push(mat1(
            &format!("clay_{}", DYES[i]),
            &format!("terre cuite {c}"),
            clay[i].clone(),
        ));
        mats.push(mat1(
            &format!("concrete_{}", DYES[i]),
            &format!("béton {c}"),
            TexSrc::Flat(DYE_RGB[i]),
        ));
        mats.push(mat1(
            &format!("concrete_powder_{}", DYES[i]),
            &format!("béton en poudre {c}"),
            TexSrc::Flat(mix(DYE_RGB[i], [255, 255, 255], 0.18)),
        ));
        mats.push(mat1(
            &format!("glazed_{}", DYES[i]),
            &format!("terre cuite émaillée {c}"),
            clay[i].clone(),
        ));
        mats.push(mat1(
            &format!("shulker_{}", DYES[i]),
            &format!("boîte de Shulker {c}"),
            TexSrc::Flat(mix(DYE_RGB[i], [0, 0, 0], 0.1)),
        ));
    }
    // Cubes pour tous les matériaux, puis leurs dalles et escaliers (ordre fixe).
    let all: Vec<Mat> = base
        .iter()
        .map(|(m, _)| m.clone())
        .chain(more.iter().map(|(m, _)| m.clone()))
        .chain(mats.iter().cloned())
        .collect();
    for m in &mats {
        let props = if m.key.starts_with("leaves_") {
            FOLIAGE
        } else {
            SOLID
        };
        b.cube(m, props);
    }
    for m in &all {
        b.shapes(m);
    }

    // Verre teinté et vitres.
    let glass_p = Props {
        full: false,
        solid: true,
        target: true,
        cull_same: true,
        light: None,
        translucent: true,
    };
    for i in 0..16 {
        b.add(
            format!("glass_{}", DYES[i]),
            format!("verre {}", DYES_FR[i]),
            Shape::Cube,
            &[glass[i].clone(), glass[i].clone(), glass[i].clone()],
            glass_p,
        );
        b.add(
            format!("pane_{}", DYES[i]),
            format!("vitre {}", DYES_FR[i]),
            Shape::Post(PostKind::Pane),
            &[pane[i].clone(), glass[i].clone(), pane[i].clone()],
            Props {
                translucent: true,
                ..PART
            },
        );
        b.add(
            format!("carpet_{}", DYES[i]),
            format!("tapis {}", DYES_FR[i]),
            Shape::Panel {
                side: 3,
                thick: 1,
                y0: 0,
                y1: 16,
            },
            &[wool[i].clone(), wool[i].clone(), wool[i].clone()],
            DECOR,
        );
        for side in [0u8, 1, 4, 5] {
            b.add(
                format!("banner_{}:{side}", DYES[i]),
                format!("bannière {}", DYES_FR[i]),
                Shape::Panel {
                    side,
                    thick: 1,
                    y0: 0,
                    y1: 16,
                },
                &[wool[i].clone(), wool[i].clone(), wool[i].clone()],
                DECOR,
            );
        }
    }
    b.add(
        "pane".into(),
        "vitre".into(),
        Shape::Post(PostKind::Pane),
        &[p("glass_pane_top"), p("glass"), p("glass_pane_top")],
        PART,
    );
    b.add(
        "iron_bars".into(),
        "barreaux".into(),
        Shape::Post(PostKind::Pane),
        &[p("iron_bars"), p("iron_bars"), p("iron_bars")],
        PART,
    );

    // Clôtures, murets.
    for (i, w) in PLANKS.iter().enumerate() {
        b.add(
            format!("fence_{}", WOODS[i]),
            format!("clôture de {}", WOODS_FR[i]),
            Shape::Post(PostKind::Fence),
            &[p(w), p(w), p(w)],
            PART,
        );
    }
    b.add(
        "fence_nether".into(),
        "clôture infernale".into(),
        Shape::Post(PostKind::Fence),
        &t3(TexSrc::Tint("hellrock_brick", [120, 60, 70])),
        PART,
    );
    for (k, t, n) in [
        ("wall_cobblestone", "cobblestone", "muret de pierre"),
        (
            "wall_cobblestone_mossy",
            "cobblestone_mossy",
            "muret moussu",
        ),
    ] {
        b.add(
            k.into(),
            n.into(),
            Shape::Post(PostKind::Wall),
            &[p(t), p(t), p(t)],
            PART,
        );
    }

    // Portes (bas/haut, 4 orientations, ouverte/fermée), trappes.
    for d in DOORS {
        let low = TexSrc::Pack(Box::leak(format!("door_{d}_lower").into_boxed_str()));
        let up = TexSrc::Pack(Box::leak(format!("door_{d}_upper").into_boxed_str()));
        for side in [0u8, 1, 4, 5] {
            for (half, t) in [("lower", &low), ("upper", &up)] {
                let tex = [t.clone(), t.clone(), t.clone()];
                b.add(
                    format!("door_{d}:{half}:{side}"),
                    "porte".into(),
                    Shape::Panel {
                        side,
                        thick: 3,
                        y0: 0,
                        y1: 16,
                    },
                    &tex,
                    PART,
                );
            }
        }
    }
    for (k, t) in [("trapdoor", "trapdoor"), ("iron_trapdoor", "iron_trapdoor")] {
        for side in 0u8..6 {
            b.add(
                format!("{k}:{side}"),
                "trappe".into(),
                Shape::Panel {
                    side,
                    thick: 3,
                    y0: 0,
                    y1: 16,
                },
                &[p(t), p(t), p(t)],
                PART,
            );
        }
    }

    // Plaques murales (échelles, panneaux), boutons, petits objets.
    for side in [0u8, 1, 4, 5] {
        b.add(
            format!("ladder:{side}"),
            "échelle".into(),
            Shape::Panel {
                side,
                thick: 1,
                y0: 0,
                y1: 16,
            },
            &[p("ladder"), p("ladder"), p("ladder")],
            DECOR,
        );
        b.add(
            format!("wall_sign:{side}"),
            "panneau".into(),
            Shape::Panel {
                side,
                thick: 2,
                y0: 4,
                y1: 12,
            },
            &[p("planks_oak"), p("planks_oak"), p("planks_oak")],
            DECOR,
        );
        b.add(
            format!("vine:{side}"),
            "liane".into(),
            Shape::Panel {
                side,
                thick: 1,
                y0: 0,
                y1: 16,
            },
            &t3(TexSrc::Tint("vine", [90, 150, 50])),
            DECOR,
        );
        let (min, max) = match side {
            0 => ([14, 6, 5], [16, 10, 11]),
            1 => ([0, 6, 5], [2, 10, 11]),
            4 => ([5, 6, 14], [11, 10, 16]),
            _ => ([5, 6, 0], [11, 10, 2]),
        };
        b.add(
            format!("button_stone:{side}"),
            "bouton".into(),
            Shape::Box { min, max },
            &[p("stone"), p("stone"), p("stone")],
            DECOR,
        );
        b.add(
            format!("button_wood:{side}"),
            "bouton".into(),
            Shape::Box { min, max },
            &[p("planks_oak"), p("planks_oak"), p("planks_oak")],
            DECOR,
        );
        let (min, max) = match side {
            0 => ([14, 2, 6], [16, 10, 10]),
            1 => ([0, 2, 6], [2, 10, 10]),
            4 => ([6, 2, 14], [10, 10, 16]),
            _ => ([6, 2, 0], [10, 10, 2]),
        };
        b.add(
            format!("tripwire_hook:{side}"),
            "crochet".into(),
            Shape::Box { min, max },
            &[p("planks_oak"), p("planks_oak"), p("planks_oak")],
            DECOR,
        );
    }
    for (k, n, min, max, t) in [
        (
            "button_stone:floor",
            "bouton",
            [5u8, 0, 6],
            [11u8, 2, 10],
            p("stone"),
        ),
        (
            "button_stone:ceiling",
            "bouton",
            [5, 14, 6],
            [11, 16, 10],
            p("stone"),
        ),
        (
            "button_wood:floor",
            "bouton",
            [5, 0, 6],
            [11, 2, 10],
            p("planks_oak"),
        ),
        (
            "button_wood:ceiling",
            "bouton",
            [5, 14, 6],
            [11, 16, 10],
            p("planks_oak"),
        ),
        (
            "pressure_plate_wood",
            "plaque de pression",
            [1, 0, 1],
            [15, 1, 15],
            p("planks_oak"),
        ),
        (
            "pressure_plate_stone",
            "plaque de pression",
            [1, 0, 1],
            [15, 1, 15],
            p("stone"),
        ),
        ("sign", "panneau", [2, 6, 7], [14, 14, 9], p("planks_oak")),
        (
            "flower_pot",
            "pot de fleurs",
            [5, 0, 5],
            [11, 6, 11],
            p("flower_pot"),
        ),
        ("chest", "coffre", [1, 0, 1], [15, 14, 15], p("planks_oak")),
        (
            "skull",
            "tête",
            [4, 0, 4],
            [12, 8, 12],
            TexSrc::Flat([200, 200, 190]),
        ),
        ("cake", "gâteau", [1, 0, 1], [15, 8, 15], p("cake_side")),
        (
            "enchanting_table",
            "table d'enchantement",
            [0, 0, 0],
            [16, 12, 16],
            p("enchanting_table_side"),
        ),
        ("anvil", "enclume", [2, 0, 2], [14, 16, 14], p("anvil_base")),
        (
            "brewing_stand",
            "alambic",
            [7, 0, 7],
            [9, 14, 9],
            p("brewing_stand"),
        ),
        (
            "hopper",
            "entonnoir",
            [0, 6, 0],
            [16, 16, 16],
            p("hopper_outside"),
        ),
        (
            "end_rod",
            "barre de l'End",
            [7, 0, 7],
            [9, 16, 9],
            TexSrc::Flat([240, 236, 230]),
        ),
        (
            "cactus",
            "cactus",
            [1, 0, 1],
            [15, 16, 15],
            p("cactus_side"),
        ),
        (
            "bed",
            "lit",
            [0, 0, 0],
            [16, 9, 16],
            TexSrc::Flat([160, 40, 40]),
        ),
        (
            "daylight_detector",
            "capteur de lumière",
            [0, 0, 0],
            [16, 6, 16],
            p("daylight_detector_top"),
        ),
        (
            "farmland",
            "terre labourée",
            [0, 0, 0],
            [16, 15, 16],
            p("farmland_dry"),
        ),
        (
            "grass_path",
            "chemin",
            [0, 0, 0],
            [16, 15, 16],
            p("grass_path_top"),
        ),
        (
            "repeater",
            "répéteur",
            [0, 0, 0],
            [16, 2, 16],
            p("repeater_off"),
        ),
        (
            "comparator",
            "comparateur",
            [0, 0, 0],
            [16, 2, 16],
            p("comparator_off"),
        ),
        ("lever", "levier", [5, 0, 4], [11, 3, 12], p("cobblestone")),
        ("rail", "rail", [0, 0, 0], [16, 1, 16], p("rail_normal")),
        (
            "lily_pad",
            "nénuphar",
            [0, 0, 0],
            [16, 1, 16],
            TexSrc::Tint("waterlily", [80, 140, 50]),
        ),
    ] {
        let props = if matches!(
            k,
            "chest"
                | "cactus"
                | "enchanting_table"
                | "anvil"
                | "hopper"
                | "bed"
                | "farmland"
                | "grass_path"
                | "daylight_detector"
                | "cake"
        ) {
            PART
        } else {
            DECOR
        };
        let light = if k == "end_rod" {
            Some([230, 225, 255])
        } else {
            None
        };
        b.add(
            k.into(),
            n.into(),
            Shape::Box { min, max },
            &[t.clone(), t.clone(), t],
            Props { light, ..props },
        );
    }
    for h in 1u8..=8 {
        b.add(
            format!("snow_layer:{h}"),
            "neige".into(),
            Shape::Panel {
                side: 3,
                thick: h * 2,
                y0: 0,
                y1: 16,
            },
            &[p("snow"), p("snow"), p("snow")],
            if h > 1 { PART } else { DECOR },
        );
    }

    // Blocs lumineux.
    let light = |c: [u8; 3]| Props {
        light: Some(c),
        ..SOLID
    };
    b.add(
        "glowstone".into(),
        "pierre lumineuse".into(),
        Shape::Cube,
        &[p("glowstone"), p("glowstone"), p("glowstone")],
        light([255, 205, 140]),
    );
    b.add(
        "sea_lantern".into(),
        "lanterne marine".into(),
        Shape::Cube,
        &[p("sea_lantern"), p("sea_lantern"), p("sea_lantern")],
        light([190, 235, 255]),
    );
    b.add(
        "lamp_on".into(),
        "lampe allumée".into(),
        Shape::Cube,
        &[
            p("energy_lamp_on"),
            p("energy_lamp_on"),
            p("energy_lamp_on"),
        ],
        light([255, 190, 110]),
    );
    b.add(
        "lamp_off".into(),
        "lampe".into(),
        Shape::Cube,
        &[
            p("energy_lamp_off"),
            p("energy_lamp_off"),
            p("energy_lamp_off"),
        ],
        SOLID,
    );
    b.add(
        "jack_o_lantern".into(),
        "citrouille-lanterne".into(),
        Shape::Cube,
        &[p("pumpkin_top"), p("pumpkin_face_on"), p("pumpkin_top")],
        light([255, 170, 70]),
    );

    // Plantes (croix).
    for (k, t) in [
        ("tallgrass", p("tallgrass")),
        ("fern", p("fern")),
        ("deadbush", p("deadbush")),
        ("dandelion", p("flower_dandelion")),
        ("rose", p("flower_rose")),
        ("blue_orchid", p("flower_blue_orchid")),
        ("allium", p("flower_allium")),
        ("houstonia", p("flower_houstonia")),
        ("tulip_red", p("flower_tulip_red")),
        ("tulip_orange", p("flower_tulip_orange")),
        ("tulip_white", p("flower_tulip_white")),
        ("tulip_pink", p("flower_tulip_pink")),
        ("oxeye_daisy", p("flower_oxeye_daisy")),
        ("sapling", p("sapling_oak")),
        ("mushroom_brown", p("mushroom_brown")),
        ("mushroom_red", p("mushroom_red")),
        ("reeds", p("reeds")),
        ("wheat", p("wheat_stage_7")),
        ("web", p("web")),
        ("double_grass_lower", p("double_plant_grass_bottom")),
        ("double_grass_upper", p("double_plant_grass_top")),
        ("double_fern_lower", p("double_plant_fern_bottom")),
        ("double_fern_upper", p("double_plant_fern_top")),
        ("sunflower_lower", p("double_plant_sunflower_bottom")),
        ("sunflower_upper", p("double_plant_sunflower_front")),
        ("lilac_lower", p("double_plant_syringa_bottom")),
        ("lilac_upper", p("double_plant_syringa_top")),
        ("rose_bush_lower", p("double_plant_rose_bottom")),
        ("rose_bush_upper", p("double_plant_rose_top")),
        ("peony_lower", p("double_plant_paeonia_bottom")),
        ("peony_upper", p("double_plant_paeonia_top")),
    ] {
        b.add(
            format!("plant:{k}"),
            "plante".into(),
            Shape::Cross,
            &[t.clone(), t.clone(), t],
            DECOR,
        );
    }

    // Torches murales (ajoutées à la fin : les identifiants existants ne bougent pas).
    for d in [0u8, 1, 4, 5] {
        b.add(
            format!("wall_torch:{d}"),
            "torche".into(),
            Shape::WallTorch(d),
            &torch_tex,
            DECOR,
        );
    }
    // Miroir (raytracing) : verre argenté.
    let mirror = TexSrc::Flat([214, 220, 228]);
    let id = b.add(
        "mirror".into(),
        "miroir".into(),
        Shape::Cube,
        &[mirror.clone(), mirror.clone(), mirror],
        SOLID,
    );
    b.r.defs[id as usize].reflect = 15;
    for (key, v) in [
        // Métaux : reflet discret (le fer reflétait trop avec le raytracing).
        ("iron_block", 2u8),
        ("gold_block", 2),
        ("diamond_block", 2),
        ("ice", 2),
        ("packed_ice", 2),
        ("water", 3),
    ] {
        if let Some(&i) = b.r.keys.get(key) {
            b.r.defs[i as usize].reflect = v;
            b.r.defs[i as usize].metal = key.ends_with("_block");
        }
    }

    legacy_table(&mut b);
    b.r
}

/// Rotation (en seizièmes, autour du centre) du repère de base « mur en +Z » vers le mur `d`.
pub fn wall_rot(d: u8, p: [u8; 2]) -> [u8; 2] {
    let (x, z) = (p[0], p[1]);
    match d {
        0 => [z, 16 - x],
        1 => [16 - z, x],
        5 => [16 - x, 16 - z],
        _ => [x, z],
    }
}

/// Correspondance des identifiants numériques Minecraft (1.12 et avant) → blocs.
fn legacy_table(b: &mut Builder) {
    let k = |b: &Builder, key: &str| -> u16 {
        *b.r.keys
            .get(key)
            .unwrap_or_else(|| panic!("clé de bloc inconnue : {key}"))
    };
    // Torches murales (meta 1 à 4 : penchée vers est, ouest, sud, nord).
    for (meta, wall) in [(1u8, 1u8), (2, 0), (3, 5), (4, 4)] {
        let id = k(b, &format!("wall_torch:{wall}"));
        b.legacy(50, meta, id);
    }
    let simple: &[(u16, &str)] = &[
        (0, "air"),
        (2, "grass"),
        (4, "cobblestone"),
        (7, "bedrock"),
        (8, "water"),
        (9, "water"),
        (10, "lava"),
        (11, "lava"),
        (13, "gravel"),
        (14, "gold_ore"),
        (15, "iron_ore"),
        (16, "coal_ore"),
        (19, "sponge"),
        (20, "glass"),
        (21, "diamond_ore"),
        (22, "lapis_block"),
        (23, "dispenser"),
        (25, "noteblock"),
        (29, "piston"),
        (33, "piston"),
        (34, "piston"),
        (41, "gold_block"),
        (42, "iron_block"),
        (45, "brick"),
        (46, "tnt"),
        (47, "bookshelf"),
        (48, "cobblestone_mossy"),
        (49, "obsidian"),
        (50, "torch"),
        (52, "iron_bars"),
        (54, "chest"),
        (56, "diamond_ore"),
        (57, "diamond_block"),
        (58, "crafting_table"),
        (61, "furnace"),
        (62, "furnace"),
        (63, "sign"),
        (70, "pressure_plate_stone"),
        (72, "pressure_plate_wood"),
        (73, "diamond_ore"),
        (74, "diamond_ore"),
        (79, "ice"),
        (80, "snow"),
        (81, "cactus"),
        (82, "clay"),
        (83, "plant:reeds"),
        (84, "jukebox"),
        (86, "pumpkin"),
        (87, "netherrack"),
        (88, "soul_sand"),
        (89, "glowstone"),
        (91, "jack_o_lantern"),
        (92, "cake"),
        (93, "repeater"),
        (94, "repeater"),
        (97, "stone"),
        (101, "iron_bars"),
        (102, "pane"),
        (103, "melon"),
        (110, "mycelium"),
        (111, "lily_pad"),
        (112, "nether_brick"),
        (113, "fence_nether"),
        (116, "enchanting_table"),
        (117, "brewing_stand"),
        (118, "cauldron"),
        (121, "end_bricks"),
        (123, "lamp_off"),
        (124, "lamp_on"),
        (129, "diamond_ore"),
        (130, "chest"),
        (133, "emerald_block"),
        (137, "command_block"),
        (140, "flower_pot"),
        (144, "skull"),
        (145, "anvil"),
        (146, "chest"),
        (147, "pressure_plate_stone"),
        (148, "pressure_plate_stone"),
        (149, "comparator"),
        (150, "comparator"),
        (151, "daylight_detector"),
        (152, "redstone_block"),
        (153, "diamond_ore"),
        (154, "hopper"),
        (158, "dispenser"),
        (165, "slime"),
        (166, "barrier"),
        (169, "sea_lantern"),
        (170, "hay"),
        (172, "hardened_clay"),
        (173, "coal_block"),
        (174, "packed_ice"),
        (178, "daylight_detector"),
        (198, "end_rod"),
        (201, "purpur"),
        (202, "purpur"),
        (204, "purpur"),
        (206, "end_bricks"),
        (208, "grass_path"),
        (210, "command_block"),
        (211, "command_block"),
        (212, "ice"),
        (213, "magma"),
        (214, "nether_wart_block"),
        (215, "red_nether_brick"),
        (216, "bone_block"),
        (217, "air"),
        (218, "observer"),
        (255, "command_block"),
        (60, "farmland"),
        (66, "rail"),
        (27, "rail"),
        (28, "rail"),
        (157, "rail"),
        (69, "lever"),
        (30, "plant:web"),
        (32, "plant:deadbush"),
        (37, "plant:dandelion"),
        (39, "plant:mushroom_brown"),
        (40, "plant:mushroom_red"),
        (59, "plant:wheat"),
        (141, "plant:wheat"),
        (142, "plant:wheat"),
        (207, "plant:wheat"),
        (115, "plant:wheat"),
        (6, "plant:sapling"),
        (26, "bed"),
    ];
    for &(id, key) in simple {
        let v = k(b, key);
        b.legacy_all(id, v);
    }
    // Sans équivalent : fil, feu, portails, redstone… → rien.
    for id in [
        36u16, 51, 55, 75, 76, 90, 104, 105, 119, 120, 122, 127, 132, 138, 199, 200, 209,
    ] {
        b.legacy_all(id, 0);
    }
    let stone_variants = [
        "stone",
        "stone_granite",
        "stone_granite_smooth",
        "stone_diorite",
        "stone_diorite_smooth",
        "stone_andesite",
        "stone_andesite_smooth",
    ];
    for (m, key) in stone_variants.iter().enumerate() {
        let v = k(b, key);
        b.legacy(1, m as u8, v);
    }
    for (m, key) in ["dirt", "coarse_dirt", "podzol"].iter().enumerate() {
        let v = k(b, key);
        b.legacy(3, m as u8, v);
    }
    for (m, w) in PLANKS.iter().enumerate() {
        let v = k(b, w);
        b.legacy(5, m as u8, v);
        let v = k(b, &format!("slab:{w}:bottom"));
        b.legacy(126, m as u8, v);
        let v = k(b, &format!("slab:{w}:top"));
        b.legacy(126, m as u8 | 8, v);
        let v = k(b, w);
        b.legacy(125, m as u8, v);
        b.legacy(125, m as u8 | 8, v);
    }
    for (m, key) in ["sand", "red_sand"].iter().enumerate() {
        let v = k(b, key);
        b.legacy(12, m as u8, v);
    }
    // Bûches et feuilles (méta & 3 = essence, le reste = axe / décomposition).
    for meta in 0u8..16 {
        let (w1, w2) = ((meta & 3) as usize, 4 + (meta & 1) as usize);
        let v = k(b, LOGS[w1].0);
        b.legacy(17, meta, v);
        let v = k(b, LOGS[w2.min(5)].0);
        b.legacy(162, meta, v);
        let v = k(b, &format!("leaves_{}", WOODS[w1]));
        b.legacy(18, meta, v);
        let v = k(b, &format!("leaves_{}", WOODS[w2.min(5)]));
        b.legacy(161, meta, v);
    }
    for (m, key) in ["sandstone", "sandstone_carved", "sandstone_smooth"]
        .iter()
        .enumerate()
    {
        let v = k(b, key);
        b.legacy(24, m as u8, v);
    }
    for (m, key) in [
        "red_sandstone",
        "red_sandstone_carved",
        "red_sandstone_smooth",
    ]
    .iter()
    .enumerate()
    {
        let v = k(b, key);
        b.legacy(179, m as u8, v);
    }
    for meta in 0u8..16 {
        let i = meta as usize;
        let pairs = [
            (35u16, format!("wool_{}", DYES[i])),
            (159, format!("clay_{}", DYES[i])),
            (95, format!("glass_{}", DYES[i])),
            (160, format!("pane_{}", DYES[i])),
            (171, format!("carpet_{}", DYES[i])),
            (251, format!("concrete_{}", DYES[i])),
            (252, format!("concrete_powder_{}", DYES[i])),
        ];
        for (id, key) in pairs {
            let v = k(b, &key);
            b.legacy(id, meta, v);
        }
    }
    for i in 0..16u16 {
        let v = k(b, &format!("shulker_{}", DYES[i as usize]));
        b.legacy_all(219 + i, v);
        let v = k(b, &format!("glazed_{}", DYES[i as usize]));
        b.legacy_all(235 + i, v);
    }
    // Dalles de pierre (méta & 7 = matériau, & 8 = moitié haute) et doubles dalles.
    let slab_mats = [
        "smooth_stone",
        "sandstone",
        "planks_oak",
        "cobblestone",
        "brick",
        "stonebrick",
        "nether_brick",
        "quartz",
    ];
    for (m, mat) in slab_mats.iter().enumerate() {
        let v = k(b, &format!("slab:{mat}:bottom"));
        b.legacy(44, m as u8, v);
        let v = k(b, &format!("slab:{mat}:top"));
        b.legacy(44, m as u8 | 8, v);
        let v = k(b, mat);
        b.legacy(43, m as u8, v);
    }
    for (meta, key) in [
        (8u8, "smooth_stone"),
        (9, "sandstone_smooth"),
        (15, "quartz"),
    ] {
        let v = k(b, key);
        b.legacy(43, meta, v);
    }
    for meta in 0u8..16 {
        let half = if meta & 8 != 0 { "top" } else { "bottom" };
        let v = k(b, &format!("slab:red_sandstone:{half}"));
        b.legacy(182, meta, v);
        let v = k(b, &format!("slab:purpur:{half}"));
        b.legacy(205, meta, v);
        let v = k(b, "red_sandstone");
        b.legacy(181, meta, v);
    }
    // Escaliers : méta & 3 = 0 est, 1 ouest, 2 sud, 3 nord ; & 4 = retourné.
    let stairs: &[(u16, &str)] = &[
        (53, "planks_oak"),
        (67, "cobblestone"),
        (108, "brick"),
        (109, "stonebrick"),
        (114, "nether_brick"),
        (128, "sandstone"),
        (134, "planks_spruce"),
        (135, "planks_birch"),
        (136, "planks_jungle"),
        (156, "quartz"),
        (163, "planks_acacia"),
        (164, "planks_big_oak"),
        (180, "red_sandstone"),
        (203, "purpur"),
    ];
    for &(id, mat) in stairs {
        for meta in 0u8..16 {
            let dir = [0u8, 1, 2, 3][(meta & 3) as usize];
            let top = meta & 4 != 0;
            let v = k(
                b,
                &format!("stairs:{mat}:{dir}:{}", if top { "top" } else { "bottom" }),
            );
            b.legacy(id, meta, v);
        }
    }
    for (m, key) in [
        "stonebrick",
        "stonebrick_mossy",
        "stonebrick_cracked",
        "stonebrick_carved",
    ]
    .iter()
    .enumerate()
    {
        let v = k(b, key);
        b.legacy(98, m as u8, v);
    }
    for meta in 0u8..16 {
        let key = match meta {
            1 => "quartz_chiseled",
            2..=4 => "quartz_pillar",
            _ => "quartz",
        };
        let v = k(b, key);
        b.legacy(155, meta, v);
        let v = k(
            b,
            ["prismarine", "prismarine_bricks", "prismarine_dark"][(meta as usize).min(2)],
        );
        b.legacy(168, meta, v);
        let v = k(
            b,
            if meta & 1 == 1 {
                "wall_cobblestone_mossy"
            } else {
                "wall_cobblestone"
            },
        );
        b.legacy(139, meta, v);
    }
    let fences: &[(u16, &str)] = &[
        (85, "fence_oak"),
        (188, "fence_spruce"),
        (189, "fence_birch"),
        (190, "fence_jungle"),
        (191, "fence_big_oak"),
        (192, "fence_acacia"),
    ];
    for &(id, key) in fences {
        let v = k(b, key);
        b.legacy_all(id, v);
    }
    // Portillons : comme une clôture.
    for (id, key) in [
        (107u16, "fence_oak"),
        (183, "fence_spruce"),
        (184, "fence_birch"),
        (185, "fence_jungle"),
        (186, "fence_big_oak"),
        (187, "fence_acacia"),
    ] {
        let v = k(b, key);
        b.legacy_all(id, v);
    }
    // Côté contre lequel est posé un objet mural, d'après sa direction (2 nord, 3 sud, 4 ouest, 5 est).
    let wall_side = |facing: u8| -> u8 {
        match facing {
            2 => 4, // tourné vers le nord : contre la face sud (+Z)
            3 => 5,
            4 => 0,
            _ => 1,
        }
    };
    for meta in 0u8..16 {
        let side = wall_side(meta & 7);
        for (id, key) in [
            (65u16, format!("ladder:{side}")),
            (68, format!("wall_sign:{side}")),
            (177, format!("banner_white:{side}")),
        ] {
            let v = k(b, &key);
            b.legacy(id, meta, v);
        }
        let v = k(b, "banner_white:5");
        b.legacy(176, meta, v);
        // Boutons : 0 plafond, 1 est (contre −X), 2 ouest, 3 sud, 4 nord, 5 sol.
        for (id, mat) in [(77u16, "button_stone"), (143, "button_wood")] {
            let key = match meta & 7 {
                0 => format!("{mat}:ceiling"),
                1 => format!("{mat}:1"),
                2 => format!("{mat}:0"),
                3 => format!("{mat}:5"),
                4 => format!("{mat}:4"),
                _ => format!("{mat}:floor"),
            };
            let v = k(b, &key);
            b.legacy(id, meta, v);
        }
        // Crochet : 0 sud, 1 ouest, 2 nord, 3 est (direction) → contre la face opposée.
        let side = [5u8, 0, 4, 1][(meta & 3) as usize];
        let v = k(b, &format!("tripwire_hook:{side}"));
        b.legacy(131, meta, v);
        // Lianes : bits 1 sud, 2 ouest, 4 nord, 8 est.
        let side = if meta & 1 != 0 {
            4
        } else if meta & 2 != 0 {
            1
        } else if meta & 4 != 0 {
            5
        } else {
            0
        };
        let v = k(b, &format!("vine:{side}"));
        b.legacy(106, meta, v);
        // Trappes : méta & 3 = 0 nord, 1 sud, 2 ouest, 3 est ; & 4 ouverte ; & 8 moitié haute.
        for (id, kind) in [(96u16, "trapdoor"), (167, "iron_trapdoor")] {
            let side = if meta & 4 != 0 {
                [4u8, 5, 0, 1][(meta & 3) as usize]
            } else if meta & 8 != 0 {
                2
            } else {
                3
            };
            let v = k(b, &format!("{kind}:{side}"));
            b.legacy(id, meta, v);
        }
        // Neige en couches.
        let v = k(b, &format!("snow_layer:{}", (meta & 7) + 1));
        b.legacy(78, meta, v);
    }
    // Herbes, fleurs, plantes doubles.
    for (meta, key) in [
        (0u8, "plant:deadbush"),
        (1, "plant:tallgrass"),
        (2, "plant:fern"),
    ] {
        let v = k(b, key);
        b.legacy(31, meta, v);
    }
    let flowers = [
        "rose",
        "blue_orchid",
        "allium",
        "houstonia",
        "tulip_red",
        "tulip_orange",
        "tulip_white",
        "tulip_pink",
        "oxeye_daisy",
    ];
    for (meta, f) in flowers.iter().enumerate() {
        let v = k(b, &format!("plant:{f}"));
        b.legacy(38, meta as u8, v);
    }
    let doubles = [
        "sunflower",
        "lilac",
        "double_grass",
        "double_fern",
        "rose_bush",
        "peony",
    ];
    for (meta, d) in doubles.iter().enumerate() {
        let v = k(b, &format!("plant:{d}_lower"));
        b.legacy(175, meta as u8, v);
    }
    // Moitié haute (méta ≥ 8) : l'espèce est dans le bloc du dessous ; herbe haute par défaut,
    // corrigée lors de la conversion.
    for meta in 8u8..16 {
        let v = k(b, "plant:double_grass_upper");
        b.legacy(175, meta, v);
    }
    // Portes : bas (méta < 8) & 3 = 0 est, 1 sud, 2 ouest, 3 nord, & 4 ouverte ; haut : voir conversion.
    for (id, d) in [
        (64u16, "wood"),
        (71, "iron"),
        (193, "spruce"),
        (194, "birch"),
        (195, "jungle"),
        (196, "acacia"),
        (197, "dark_oak"),
    ] {
        for meta in 0u8..8 {
            let v = k(
                b,
                &format!("door_{d}:lower:{}", door_side(meta & 3, meta & 4 != 0)),
            );
            b.legacy(id, meta, v);
        }
        for meta in 8u8..16 {
            let v = k(b, &format!("door_{d}:upper:1"));
            b.legacy(id, meta, v);
        }
    }
}

/// Côté occupé par une porte fermée selon sa direction (0 est, 1 sud, 2 ouest, 3 nord),
/// ou par une porte ouverte (tournée d'un quart de tour).
pub fn door_side(facing: u8, open: bool) -> u8 {
    let closed = [1u8, 5, 0, 4][facing as usize & 3];
    if open {
        [5u8, 0, 4, 1][facing as usize & 3]
    } else {
        closed
    }
}

/// Matériau d'un nom d'essence moderne (`oak`, `dark_oak`, `mangrove`…) → index dans WOODS.
pub fn wood_index(name: &str) -> usize {
    match name {
        "spruce" => 1,
        "birch" => 2,
        "jungle" | "bamboo" => 3,
        "acacia" | "mangrove" | "cherry" | "crimson" => 4,
        "dark_oak" | "warped" | "pale_oak" => 5,
        _ => 0,
    }
}

pub fn wood_key(i: usize) -> &'static str {
    WOODS[i.min(5)]
}

pub fn planks_key(i: usize) -> &'static str {
    PLANKS[i.min(5)]
}

pub fn log_key(i: usize) -> &'static str {
    LOGS[i.min(5)].0
}

pub fn door_key(i: usize) -> &'static str {
    DOORS[i.min(6)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_ids_are_stable() {
        let r = registry();
        assert_eq!(r.by_key("air"), Some(BlockId::AIR));
        assert_eq!(r.by_key("stone"), Some(BlockId::STONE));
        assert_eq!(r.by_key("torch"), Some(BlockId::TORCH));
        assert_eq!(r.by_key("wool_white"), Some(BlockId::WOOL));
        assert_eq!(r.by_key("barrier"), Some(BlockId(30)));
        println!("{} blocs, {} textures", r.len(), r.textures.len());
        assert!(r.len() > 500 && r.len() < 4000, "{} blocs", r.len());
        assert!(r.textures.len() < 1024);
    }

    #[test]
    fn legacy_lookup() {
        let r = registry();
        let name = |id: u16, meta: u8| r.get(r.legacy(id, meta).unwrap()).key.clone();
        assert_eq!(name(251, 14), "concrete_red");
        assert_eq!(name(44, 9), "slab:sandstone:top");
        assert_eq!(name(128, 2), "stairs:sandstone:2:bottom");
        assert_eq!(name(5, 5), "planks_big_oak");
        assert_eq!(name(166, 0), "barrier");
        assert_eq!(name(1, 4), "stone_diorite_smooth");
        assert_eq!(name(188, 0), "fence_spruce");
        assert!(r.get(r.legacy(166, 0).unwrap()).solid);
        assert!(!r.get(r.legacy(31, 1).unwrap()).solid);
    }
}
