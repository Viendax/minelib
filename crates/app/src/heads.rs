//! Têtes dessinées pixel par pixel avec leur skin, comme la tête du modèle du joueur :
//!
//! - les blocs « tête » (posés par le joueur, ou venus d'une carte Minecraft convertie), au
//!   sol (16 directions, comme Minecraft) ou contre un mur ;
//! - les têtes portées par des entités des cartes (porte-armures, entités d'affichage,
//!   cadres), à leur place exacte, tournées et mises à l'échelle comme dans Minecraft.
//!
//! Skin : celui du jeu par défaut, une texture de Minecraft (téléchargée une fois depuis
//! textures.minecraft.net, gardée dans `.minelib/tetes/`), ou celui d'un joueur de Minelib
//! (demandé au relais, tenu à jour). Les têtes sont éclairées comme les blocs autour d'elles
//! (ciel, ombre des toits et des grottes, torches, jour et nuit) : la lumière vient du
//! maillage des chunks (`MeshData::heads`, `MeshData::probes`).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, LazyLock, Mutex, RwLock};

use glam::{Mat4, Quat, Vec3, Vec4};
use minelib_render::{EntityVertex, VmVertex};
use minelib_world::registry::skull_pose;
use minelib_world::{BlockId, ChunkPos};

use crate::skin::Skin;

/// Distance maximale des têtes dessinées (blocs).
const RANGE: f32 = 200.0;
/// Téléchargements en même temps.
const PARALLEL: usize = 4;
/// Têtes posées par le joueur (à côté de `tetes.txt`, que réécrit une nouvelle conversion
/// de la carte) : mêmes lignes.
pub const PLACED_FILE: &str = "tetes_posees.txt";

// ------------------------------------------------------------------------------- skins

/// Skin d'une tête.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum HeadSkin {
    /// Le skin par défaut du jeu.
    Default,
    /// Texture de Minecraft : empreinte sur textures.minecraft.net (minuscules).
    Mc(String),
    /// Joueur de Minelib (pseudo) : son skin du moment.
    Player(String),
}

impl HeadSkin {
    /// Lit le champ d'une ligne (`-`, empreinte hexadécimale, `j:pseudo`).
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        if s == "-" {
            return Some(Self::Default);
        }
        if let Some(p) = s.strip_prefix("j:") {
            return minelib_net::proto::valid_pseudo(p).then(|| Self::Player(p.to_string()));
        }
        ((32..=80).contains(&s.len()) && s.chars().all(|c| c.is_ascii_hexdigit()))
            .then(|| Self::Mc(s.to_ascii_lowercase()))
    }

    /// Champ d'une ligne (voir `parse`).
    pub fn field(&self) -> String {
        match self {
            Self::Default => "-".into(),
            Self::Mc(h) => h.clone(),
            Self::Player(p) => format!("j:{p}"),
        }
    }

    /// Nom de l'objet « tête » qui porte ce skin.
    pub fn item_name(&self) -> String {
        match self {
            Self::Default => "Tête".into(),
            Self::Mc(_) => "Tête (skin de Minecraft)".into(),
            Self::Player(p) => format!("Tête de {p}"),
        }
    }
}

/// Skins des têtes de l'inventaire : `Item::Head(n)` porte le skin n° n de cette table.
static TABLE: RwLock<Vec<HeadSkin>> = RwLock::new(Vec::new());

/// Numéro d'un skin de tête pour un objet de l'inventaire (`Item::Head`).
pub fn intern(skin: &HeadSkin) -> u32 {
    if let Some(i) = TABLE
        .read()
        .ok()
        .and_then(|t| t.iter().position(|s| s == skin))
    {
        return i as u32;
    }
    let mut t = TABLE.write().unwrap_or_else(|e| e.into_inner());
    if let Some(i) = t.iter().position(|s| s == skin) {
        return i as u32;
    }
    t.push(skin.clone());
    (t.len() - 1) as u32
}

/// Skin de l'objet `Item::Head(n)`.
pub fn skin_of(n: u32) -> HeadSkin {
    TABLE
        .read()
        .ok()
        .and_then(|t| t.get(n as usize).cloned())
        .unwrap_or(HeadSkin::Default)
}

/// Skins chargés (textures de Minecraft téléchargées, skins de joueurs reçus), pour les
/// têtes du monde et les icônes de l'inventaire.
static LOADED: LazyLock<RwLock<HashMap<HeadSkin, Arc<Skin>>>> = LazyLock::new(Default::default);
/// Skins demandés par l'interface (icônes) pas encore chargés.
static WANTED: Mutex<Vec<HeadSkin>> = Mutex::new(Vec::new());

/// Skin chargé ; sinon `None`, et il est demandé (téléchargé, ou demandé au relais).
pub fn loaded(skin: &HeadSkin) -> Option<Arc<Skin>> {
    if *skin == HeadSkin::Default {
        return None;
    }
    let got = LOADED.read().ok().and_then(|m| m.get(skin).cloned());
    if got.is_none() {
        if let Ok(mut w) = WANTED.lock() {
            if !w.contains(skin) && w.len() < 256 {
                w.push(skin.clone());
            }
        }
    }
    got
}

/// Garde un skin chargé ; `true` s'il est nouveau ou a changé.
fn store(skin: HeadSkin, s: Arc<Skin>) -> bool {
    let mut m = LOADED.write().unwrap_or_else(|e| e.into_inner());
    match m.get(&skin) {
        // Le même skin redemandé (toutes les deux minutes) : rien à refaire.
        Some(old) if Arc::ptr_eq(old, &s) || **old == *s => false,
        _ => {
            m.insert(skin, s);
            true
        }
    }
}

fn take_wanted() -> Vec<HeadSkin> {
    WANTED
        .lock()
        .map(|mut w| std::mem::take(&mut *w))
        .unwrap_or_default()
}

// ------------------------------------------------------------------------------- poses

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Pose {
    /// Au sol, tournée de 0..15 seizièmes de tour (comme Minecraft : 0 regarde le nord, 4
    /// l'est, 8 le sud, 12 l'ouest).
    Floor(u8),
    /// Contre un mur (côté du mur : 0 +X, 1 −X, 4 +Z, 5 −Z), le visage à l'opposé.
    Wall(u8),
    /// Portée par une entité de la carte (porte-armure, entité d'affichage, cadre) :
    /// passage des pixels du modèle de la tête au monde (3 × 4, par colonnes).
    Free([f32; 12]),
    /// Porte-armure visible (son bâti de bois ; la tête qu'il porte est une ligne `Free`) :
    /// passage de ses pixels au monde, et options (1 sans socle, 2 avec bras).
    Stand([f32; 12], u8),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Head {
    /// Case de la tête (bloc « tête », ou case où est le centre d'une tête d'entité).
    pub pos: [i32; 3],
    pub pose: Pose,
    pub skin: HeadSkin,
}

/// Les 12 nombres d'une transformation affine (3 × 4, par colonnes) d'une ligne.
pub fn pack(m: Mat4) -> [f32; 12] {
    let c = m.to_cols_array();
    [
        c[0], c[1], c[2], c[4], c[5], c[6], c[8], c[9], c[10], c[12], c[13], c[14],
    ]
}

fn unpack(a: &[f32; 12]) -> Mat4 {
    Mat4::from_cols(
        Vec4::new(a[0], a[1], a[2], 0.0),
        Vec4::new(a[3], a[4], a[5], 0.0),
        Vec4::new(a[6], a[7], a[8], 0.0),
        Vec4::new(a[9], a[10], a[11], 1.0),
    )
}

fn floats(s: &str) -> Option<Vec<f32>> {
    s.split(' ')
        .map(|v| v.parse::<f32>().ok().filter(|f| f.is_finite()))
        .collect()
}

/// Lit une ligne `x,y,z,pose,skin` de `tetes.txt` (ou de `PLACED_FILE`). Pose : `0`..`15`
/// au sol, `w0`/`w1`/`w4`/`w5` contre un mur, `m` + 12 nombres (tête d'entité), `a` + 13
/// nombres (porte-armure) ; les cadres (`f…`) sont lus ailleurs.
pub fn parse(line: &str) -> Option<Head> {
    let v: Vec<&str> = line.trim().split(',').collect();
    let [x, y, z, pose, skin] = v.as_slice() else {
        return None;
    };
    let pose = if let Some(s) = pose.strip_prefix('w') {
        Pose::Wall(s.parse().ok().filter(|s| matches!(s, 0 | 1 | 4 | 5))?)
    } else if let Some(m) = pose.strip_prefix('m') {
        let f: [f32; 12] = floats(m)?.try_into().ok()?;
        Pose::Free(f)
    } else if let Some(m) = pose.strip_prefix('a') {
        let f = floats(m)?;
        let (m, flags) = (f.get(..12)?, *f.get(12)?);
        if f.len() != 13 {
            return None;
        }
        Pose::Stand(m.try_into().ok()?, flags as u8)
    } else {
        Pose::Floor(pose.parse::<u8>().ok()? & 15)
    };
    Some(Head {
        pos: [x.parse().ok()?, y.parse().ok()?, z.parse().ok()?],
        pose,
        skin: HeadSkin::parse(skin)?,
    })
}

/// Ligne d'une tête (voir `parse`).
pub fn line(h: &Head) -> String {
    let join = |a: &[f32]| {
        a.iter()
            .map(|f| f.to_string())
            .collect::<Vec<_>>()
            .join(" ")
    };
    let pose = match h.pose {
        Pose::Floor(r) => r.to_string(),
        Pose::Wall(s) => format!("w{s}"),
        Pose::Free(m) => format!("m{}", join(&m)),
        Pose::Stand(m, flags) => format!("a{} {flags}", join(&m)),
    };
    let [x, y, z] = h.pos;
    format!("{x},{y},{z},{pose},{}", h.skin.field())
}

/// Passage des pixels du modèle de la tête (-4..4 × 0..8 × -4..4, visage vers −z) au
/// monde, pour une tête de bloc posée en `pos` (ou une tête d'entité).
pub fn transform_at(pos: [i32; 3], pose: Pose) -> Mat4 {
    let [x, y, z] = pos.map(|v| v as f32);
    let px = Mat4::from_scale(Vec3::splat(1.0 / 16.0));
    match pose {
        // Minecraft tourne le modèle de la tête de r × 22,5° : à 0 le visage regarde le
        // nord (−z), à 4 l'est (+x).
        Pose::Floor(r) => {
            let t = r as f32 * std::f32::consts::TAU / 16.0;
            Mat4::from_translation(Vec3::new(x + 0.5, y, z + 0.5)) * Mat4::from_rotation_y(-t) * px
        }
        Pose::Wall(s) => {
            let wall = match s {
                0 => Vec3::X,
                1 => -Vec3::X,
                4 => Vec3::Z,
                _ => -Vec3::Z,
            };
            let face = -wall;
            let yaw = (-face.x).atan2(-face.z);
            Mat4::from_translation(Vec3::new(x + 0.5, y + 0.25, z + 0.5) + wall * 0.25)
                * Mat4::from_quat(Quat::from_rotation_y(yaw))
                * px
        }
        Pose::Free(m) | Pose::Stand(m, _) => unpack(&m),
    }
}

pub fn transform(h: &Head) -> Mat4 {
    transform_at(h.pos, h.pose)
}

/// Le repère du modèle de la tête de Minecraft (y vers le bas) vu depuis le nôtre (y vers
/// le haut) : un demi-tour autour de z (pas un miroir : le skin reste à l'endroit).
fn mc() -> Mat4 {
    Mat4::from_scale(Vec3::new(-1.0, -1.0, 1.0))
}

/// Rotation d'une partie de modèle de Minecraft (degrés, appliqués z, y, x).
fn part_rotation(deg: [f32; 3]) -> Mat4 {
    let [x, y, z] = deg.map(f32::to_radians);
    Mat4::from_rotation_z(z) * Mat4::from_rotation_y(y) * Mat4::from_rotation_x(x)
}

/// Tête portée par un porte-armure en `pos` (pieds), tourné de `yaw` degrés (Minecraft :
/// 0 regarde le sud), tête tournée de `head` (degrés, `Pose.Head`), petit ou non. Comme
/// Minecraft : la tête (agrandie de 1,1875) posée sur le pivot du cou, à 23 pixels.
pub fn armor_stand_head(pos: Vec3, yaw: f32, head: [f32; 3], small: bool) -> Mat4 {
    let k = if small { 0.5 } else { 1.0 };
    Mat4::from_translation(pos)
        * Mat4::from_rotation_y((180.0 - yaw).to_radians())
        * mc()
        * Mat4::from_scale(Vec3::splat(k))
        * Mat4::from_translation(Vec3::new(0.0, -1.501 + 1.0 / 16.0, 0.0))
        * part_rotation(head)
        * mc()
        * Mat4::from_scale(Vec3::splat(1.1875 / 16.0))
}

/// Bâti d'un porte-armure (voir `armor_stand_head`) : pixels (y vers le haut depuis les
/// pieds, devant vers −z) → monde.
pub fn armor_stand_body(pos: Vec3, yaw: f32, small: bool) -> Mat4 {
    let k = if small { 0.5 } else { 1.0 };
    Mat4::from_translation(pos)
        * Mat4::from_rotation_y((180.0 - yaw).to_radians())
        * Mat4::from_scale(Vec3::splat(k / 16.0))
}

/// Repère d'une entité d'affichage en `pos`, tournée de `yaw`, `pitch` (degrés, comme
/// Minecraft), avec sa `transformation` (translation, rotation gauche, échelle, rotation
/// droite).
pub fn display_frame(pos: Vec3, yaw: f32, pitch: f32, transformation: Mat4) -> Mat4 {
    Mat4::from_translation(pos)
        * Mat4::from_rotation_y((-yaw).to_radians())
        * Mat4::from_rotation_x(pitch.to_radians())
        * transformation
}

/// Tête montrée par une entité d'affichage d'objet (`item_display`, mode `none`) dont le
/// repère est `frame` (voir `display_frame`) : Minecraft la tourne d'un demi-tour, puis
/// dessine l'objet « tête » centré (la tête, posée sur le bas de sa case, finit sous
/// l'origine de l'entité, le visage vers −z).
pub fn display_head(frame: Mat4) -> Mat4 {
    let half = Mat4::from_rotation_y(std::f32::consts::PI);
    frame
        * half
        * Mat4::from_translation(Vec3::new(0.0, -0.5, 0.0))
        * half
        * Mat4::from_scale(Vec3::splat(1.0 / 16.0))
}

/// Tête dans un cadre : case du cadre `cell`, côté vers lequel il regarde (`facing` comme
/// Minecraft : 0 bas, 1 haut, 2 nord, 3 sud, 4 ouest, 5 est), rotation de l'objet (8es de
/// tour), cadre invisible ou non. L'objet est dessiné à demi-taille contre le mur, le visage
/// vers l'extérieur.
pub fn frame_head(cell: [i32; 3], facing: u8, rotation: u8, invisible: bool) -> Mat4 {
    let center = Vec3::new(
        cell[0] as f32 + 0.5,
        cell[1] as f32 + 0.5,
        cell[2] as f32 + 0.5,
    );
    // Rotation de l'entité (comme Minecraft : y = 90° × direction, x = ∓90° au sol / au
    // plafond).
    let (x_rot, y_rot) = match facing {
        0 => (90.0f32, 0.0f32),
        1 => (-90.0, 0.0),
        2 => (0.0, 180.0),
        3 => (0.0, 0.0),
        4 => (0.0, 90.0),
        _ => (0.0, 270.0),
    };
    let depth = if invisible { 0.5 } else { 0.4375 };
    Mat4::from_translation(center)
        * Mat4::from_rotation_x(x_rot.to_radians())
        * Mat4::from_rotation_y((180.0 - y_rot).to_radians())
        * Mat4::from_translation(Vec3::new(0.0, 0.0, depth))
        * Mat4::from_rotation_z((rotation as f32 * 45.0).to_radians())
        * Mat4::from_scale(Vec3::splat(0.5))
        * Mat4::from_translation(Vec3::new(0.0, -0.25, 0.0))
        * Mat4::from_scale(Vec3::splat(1.0 / 16.0))
}

/// Case qui contient le centre d'une tête de transformation `m`.
pub fn cell_of(m: Mat4) -> [i32; 3] {
    let c = m.transform_point3(Vec3::new(0.0, 4.0, 0.0));
    [c.x.floor() as i32, c.y.floor() as i32, c.z.floor() as i32]
}

// ------------------------------------------------------------------------------- lumière

/// Lumière reçue (`[r, g, b, ciel]` du maillage) → éclairage d'une tête, comme celui des
/// blocs (ciel selon l'heure et l'abri, torches au carré) ; `day` : 1 plein jour, 0 nuit.
/// L'ombrage des faces est déjà dans la couleur des pixels (dessus plus clair).
pub fn lit(l: [u8; 4], day: f32) -> [f32; 3] {
    let s = (l[3].min(15)) as f32 / 15.0;
    let day = day.clamp(0.0, 1.0);
    let sky = ((0.48 + 0.52 * 0.75) * day + 0.04 * (1.0 - day)) * s * s + 0.012;
    let torch = |c: u8| {
        let c = (c >> 2) as f32 / 63.0;
        c * c * 1.25
    };
    [sky + torch(l[0]), sky + torch(l[1]), sky + torch(l[2])]
}

// ------------------------------------------------------------------------------- têtes

/// Tête d'un bloc lue dans les fichiers : sa pose (utile seulement pour les blocs `…:tex`
/// des cartes converties avant la v157) et son skin.
#[derive(Clone, Debug)]
struct Entry {
    pose: Pose,
    skin: HeadSkin,
}

pub struct Heads {
    /// Têtes des blocs, par position.
    on_blocks: HashMap<[i32; 3], Entry>,
    /// Têtes et porte-armures des entités des cartes.
    free: Vec<Head>,
    cache: PathBuf,
    /// Téléchargements demandés (en cours ou ratés) : pas redemandés.
    asked: HashSet<String>,
    running: usize,
    tx: Sender<(String, Option<Vec<u8>>)>,
    rx: Receiver<(String, Option<Vec<u8>>)>,
    /// Textures à télécharger d'abord (demandées par l'inventaire).
    queue: Vec<String>,
    /// Joueurs dont les têtes du monde ou de l'inventaire montrent le skin.
    players: HashSet<String>,
    /// Têtes par chunk : leurs triangles sont faits une fois et gardés sur la carte
    /// graphique (rien n'est refait quand on se déplace).
    cells: HashMap<(i32, i32), Cell>,
    next_gen: u64,
    /// Images depuis la dernière recherche de textures à télécharger.
    frames: u32,
}

/// Têtes d'un chunk et leurs triangles.
#[derive(Default)]
struct Cell {
    /// Blocs « tête » vus par le maillage : position, bloc, lumière reçue.
    spots: Vec<([i32; 3], BlockId, [u8; 4])>,
    /// Têtes d'entités (indices dans `free`) et leur lumière (points de mesure du chunk,
    /// même ordre ; vide : pas encore mesurée).
    free: Vec<usize>,
    free_light: Vec<[u8; 4]>,
    verts: Vec<EntityVertex>,
    /// Version des triangles (change quand ils sont refaits) ; 0 : pas encore faits.
    gen: u64,
    /// Lumière du jour lors de la dernière construction.
    day: f32,
    /// Finesse de la dernière construction (1 de près, 2 ou 4 de loin).
    lod: usize,
    /// À refaire (skin arrivé, bloc posé ou cassé…).
    dirty: bool,
}

/// Finesse voulue à cette distance ; `current` : celle en place (on n'en change qu'une
/// fois la limite franchement passée, pour ne pas refaire la zone sans arrêt).
fn lod_for(d: f32, current: usize) -> usize {
    let want = if d < 40.0 {
        1
    } else if d < 90.0 {
        2
    } else {
        4
    };
    let keep = match current {
        1 => d < 48.0,
        2 => (32.0..104.0).contains(&d),
        4 => d >= 76.0,
        _ => false,
    };
    if keep {
        current
    } else {
        want
    }
}

impl Heads {
    /// Têtes du monde `dir` ; textures gardées dans `cache`.
    pub fn load(dir: &Path, cache: PathBuf) -> Self {
        let (tx, rx) = channel();
        let mut h = Self {
            on_blocks: HashMap::new(),
            free: Vec::new(),
            cache,
            asked: HashSet::new(),
            running: 0,
            tx,
            rx,
            queue: Vec::new(),
            players: HashSet::new(),
            cells: HashMap::new(),
            next_gen: 0,
            frames: u32::MAX,
        };
        // Les têtes posées par le joueur passent après celles de la carte.
        for file in [crate::mcimport::HEADS_FILE, PLACED_FILE] {
            let text = std::fs::read_to_string(dir.join(file)).unwrap_or_default();
            for head in text.lines().filter_map(parse) {
                h.add(head);
            }
        }
        h
    }

    fn add(&mut self, head: Head) {
        if let HeadSkin::Player(p) = &head.skin {
            self.players.insert(p.clone());
        }
        match head.pose {
            Pose::Floor(_) | Pose::Wall(_) => {
                self.on_blocks.insert(
                    head.pos,
                    Entry {
                        pose: head.pose,
                        skin: head.skin,
                    },
                );
            }
            Pose::Free(_) | Pose::Stand(..) => {
                let key = (head.pos[0] >> 4, head.pos[2] >> 4);
                let i = self.free.len();
                self.free.push(head);
                let c = self.cells.entry(key).or_default();
                c.free.push(i);
                c.free_light.clear();
                c.dirty = true;
            }
        }
    }

    /// Points de mesure de la lumière à donner au monde (`World::set_probes`) : les cases
    /// des têtes d'entités, par chunk.
    pub fn probes(&self) -> Vec<(ChunkPos, Vec<[i32; 3]>)> {
        self.cells
            .iter()
            .filter(|(_, c)| !c.free.is_empty())
            .map(|(&(x, z), c)| {
                (
                    ChunkPos { x, z },
                    c.free.iter().map(|&i| self.free[i].pos).collect(),
                )
            })
            .collect()
    }

    /// Le chunk `pos` vient d'être maillé : ses blocs « tête » (avec leur lumière) et la
    /// lumière à ses points de mesure.
    pub fn chunk_meshed(
        &mut self,
        pos: ChunkPos,
        spots: Vec<([i32; 3], BlockId, [u8; 4])>,
        probes: Vec<[u8; 4]>,
    ) {
        let key = (pos.x, pos.z);
        if spots.is_empty() && !self.cells.contains_key(&key) {
            return;
        }
        let c = self.cells.entry(key).or_default();
        if c.spots != spots {
            c.spots = spots;
            c.dirty = true;
        }
        if probes.len() == c.free.len() && c.free_light != probes {
            c.free_light = probes;
            c.dirty = true;
        }
    }

    /// Skin de la tête du bloc en `pos` (celui par défaut s'il n'est pas dans les
    /// fichiers).
    pub fn skin_at(&self, pos: [i32; 3]) -> HeadSkin {
        self.on_blocks
            .get(&pos)
            .map_or(HeadSkin::Default, |e| e.skin.clone())
    }

    /// Une tête de ce skin vient d'être posée en `pos` (bloc déjà posé) : gardée dans
    /// `PLACED_FILE` du monde `dir` (rien à garder pour le skin par défaut).
    pub fn place(&mut self, pos: [i32; 3], pose: Pose, skin: HeadSkin, dir: &Path) {
        let had = self.on_blocks.remove(&pos).is_some();
        if skin != HeadSkin::Default {
            if let HeadSkin::Player(p) = &skin {
                self.players.insert(p.clone());
            }
            self.on_blocks.insert(pos, Entry { pose, skin });
        }
        if had || self.on_blocks.contains_key(&pos) {
            self.save_placed(pos, dir);
        }
        self.touch(pos);
    }

    /// Le bloc « tête » en `pos` a été cassé : sa tête est oubliée (fichiers du monde `dir`
    /// compris).
    pub fn remove_at(&mut self, pos: [i32; 3], dir: &Path) {
        if self.on_blocks.remove(&pos).is_none() {
            return;
        }
        let prefix = format!("{},{},{},", pos[0], pos[1], pos[2]);
        let path = dir.join(crate::mcimport::HEADS_FILE);
        if let Ok(text) = std::fs::read_to_string(&path) {
            // Lignes de ce bloc (pas celles des cadres ni des têtes d'entités de la case).
            let mine = |l: &str| {
                l.starts_with(&prefix)
                    && l.split(',')
                        .nth(3)
                        .is_some_and(|p| !p.starts_with(['f', 'i', 'm', 'a']))
            };
            if text.lines().any(mine) {
                write_lines(&path, text.lines().filter(|l| !mine(l)));
            }
        }
        self.save_placed(pos, dir);
        self.touch(pos);
    }

    /// Réécrit la ligne de `pos` dans `PLACED_FILE`.
    fn save_placed(&self, pos: [i32; 3], dir: &Path) {
        let path = dir.join(PLACED_FILE);
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let prefix = format!("{},{},{},", pos[0], pos[1], pos[2]);
        let mut lines: Vec<String> = text
            .lines()
            .filter(|l| !l.starts_with(&prefix))
            .map(str::to_string)
            .collect();
        if let Some(e) = self.on_blocks.get(&pos) {
            lines.push(line(&Head {
                pos,
                pose: e.pose,
                skin: e.skin.clone(),
            }));
        }
        write_lines(&path, lines.iter().map(String::as_str));
    }

    /// La zone de `pos` est à refaire.
    fn touch(&mut self, pos: [i32; 3]) {
        if let Some(c) = self.cells.get_mut(&(pos[0] >> 4, pos[2] >> 4)) {
            c.dirty = true;
        }
    }

    /// Zones dont une tête montre ce skin : à refaire.
    fn skin_changed(&mut self, skin: &HeadSkin) {
        for c in self.cells.values_mut() {
            let uses = c
                .spots
                .iter()
                .any(|(p, _, _)| self.on_blocks.get(p).is_some_and(|e| e.skin == *skin))
                || c.free.iter().any(|&i| self.free[i].skin == *skin);
            if uses {
                c.dirty = true;
            }
        }
    }

    /// Joueurs dont des têtes (du monde, ou de l'inventaire) montrent le skin : le jeu les
    /// demande au relais et donne leur skin (`set_player`).
    pub fn players(&self) -> impl Iterator<Item = &str> {
        self.players.iter().map(String::as_str)
    }

    /// Skin (reçu, ou changé) d'un joueur.
    pub fn set_player(&mut self, pseudo: &str, skin: Arc<Skin>) {
        let s = HeadSkin::Player(pseudo.to_string());
        if store(s.clone(), skin) {
            self.skin_changed(&s);
        }
    }

    fn file(&self, hash: &str) -> PathBuf {
        self.cache.join(format!("{hash}.png"))
    }

    /// Textures reçues, nouvelles demandes (celles de l'inventaire, puis les têtes proches).
    pub fn update(&mut self, cam: Vec3) {
        while let Ok((hash, bytes)) = self.rx.try_recv() {
            self.running = self.running.saturating_sub(1);
            if let Some(skin) = bytes.as_deref().and_then(|b| Skin::from_png(b).ok()) {
                if let Some(b) = &bytes {
                    let _ = std::fs::create_dir_all(&self.cache);
                    let _ = std::fs::write(self.file(&hash), b);
                }
                let s = HeadSkin::Mc(hash);
                if store(s.clone(), skin) {
                    self.skin_changed(&s);
                }
            }
        }
        for s in take_wanted() {
            match s {
                HeadSkin::Mc(h) if !self.asked.contains(&h) && !self.queue.contains(&h) => {
                    self.queue.push(h)
                }
                HeadSkin::Player(p) => {
                    self.players.insert(p);
                }
                _ => {}
            }
        }
        if self.running >= PARALLEL {
            return;
        }
        let mut wanted: Vec<String> = Vec::new();
        while wanted.len() + self.running < PARALLEL && !self.queue.is_empty() {
            let h = self.queue.remove(0);
            if !self.asked.contains(&h) {
                wanted.push(h);
            }
        }
        // Têtes proches : cherchées quatre fois par seconde environ (pas à chaque image).
        self.frames = self.frames.saturating_add(1);
        if wanted.len() + self.running < PARALLEL && self.frames >= 15 {
            self.frames = 0;
            let loaded = LOADED.read().ok();
            let missing = |s: &HeadSkin| match s {
                HeadSkin::Mc(h) => {
                    !self.asked.contains(h)
                        && !wanted.contains(h)
                        && !loaded.as_ref().is_some_and(|m| m.contains_key(s))
                }
                _ => false,
            };
            let mut near: Vec<(f32, &str)> = self
                .on_blocks
                .iter()
                .map(|(p, e)| (p, &e.skin))
                .chain(self.free.iter().map(|h| (&h.pos, &h.skin)))
                .filter(|(_, s)| missing(s))
                .map(|(p, s)| {
                    let c = Vec3::new(p[0] as f32 + 0.5, p[1] as f32 + 0.5, p[2] as f32 + 0.5);
                    let HeadSkin::Mc(h) = s else { unreachable!() };
                    (c.distance(cam), h.as_str())
                })
                .filter(|(d, _)| *d < RANGE * 1.5)
                .collect();
            near.sort_by(|a, b| a.0.total_cmp(&b.0));
            for (_, h) in near {
                if wanted.len() + self.running >= PARALLEL {
                    break;
                }
                if !wanted.iter().any(|w| w == h) {
                    wanted.push(h.to_string());
                }
            }
        }
        for hash in wanted {
            self.asked.insert(hash.clone());
            // Déjà sur le disque.
            if let Ok(b) = std::fs::read(self.file(&hash)) {
                if let Ok(skin) = Skin::from_png(&b) {
                    let s = HeadSkin::Mc(hash);
                    if store(s.clone(), skin) {
                        self.skin_changed(&s);
                    }
                    continue;
                }
            }
            self.running += 1;
            let tx = self.tx.clone();
            let _ = std::thread::Builder::new()
                .name("tête".into())
                .spawn(move || {
                    let bytes = download(&hash)
                        .map_err(|e| log::info!("texture de tête {hash} : {e:#}"))
                        .ok();
                    let _ = tx.send((hash, bytes));
                });
        }
    }

    /// Zone proche de la caméra à (re)faire (pas encore construite, changée, finesse ou
    /// lumière du jour à revoir) : une seule par appel (jamais d'à-coup). `day` : lumière
    /// du jour (1 plein jour, 0 nuit).
    pub fn refresh(&mut self, cam: Vec3, day: f32) {
        let mut best: Option<((i32, i32), f32)> = None;
        for (&key, c) in &self.cells {
            if c.spots.is_empty() && c.free.is_empty() && c.verts.is_empty() {
                continue;
            }
            let d = cell_distance(key, cam);
            if d > RANGE + 16.0 {
                continue;
            }
            let todo =
                c.dirty || c.gen == 0 || lod_for(d, c.lod) != c.lod || (c.day - day).abs() > 0.02;
            if todo && best.is_none_or(|b| d < b.1) {
                best = Some((key, d));
            }
        }
        let Some((key, d)) = best else { return };
        let lod = lod_for(d, self.cells[&key].lod);
        let verts = self.build(&self.cells[&key], day, lod);
        self.next_gen += 1;
        let c = self.cells.get_mut(&key).expect("zone");
        c.verts = verts;
        c.gen = self.next_gen;
        c.day = day;
        c.lod = lod;
        c.dirty = false;
    }

    /// Triangles des têtes d'une zone.
    fn build(&self, c: &Cell, day: f32, lod: usize) -> Vec<EntityVertex> {
        let mut out = Vec::new();
        for &(pos, id, light) in &c.spots {
            let Some((wall, n)) = skull_pose(id) else {
                continue;
            };
            let entry = self.on_blocks.get(&pos);
            // Blocs d'avant la v157 : la pose est dans le fichier.
            let pose = match entry {
                Some(e) if id.def().key.ends_with(":tex") => e.pose,
                _ if wall => Pose::Wall(n),
                _ => Pose::Floor(n),
            };
            let skin = entry.map_or(&HeadSkin::Default, |e| &e.skin);
            emit_head(
                &mut out,
                skin,
                transform_at(pos, pose),
                lit(light, day),
                lod,
            );
        }
        for (k, &i) in c.free.iter().enumerate() {
            let h = &self.free[i];
            // Pas encore mesurée : comme au grand air.
            let light = c.free_light.get(k).copied().unwrap_or([0, 0, 0, 15]);
            let l = lit(light, day);
            match h.pose {
                Pose::Stand(m, flags) => emit_stand(&mut out, unpack(&m), flags, l),
                _ => emit_head(&mut out, &h.skin, transform(h), l, lod),
            }
        }
        out
    }

    /// Tête d'entité visée par le rayon `eye` + t `dir` (t ≤ `max`) : la plus proche, avec
    /// son skin (les têtes des blocs sont visées avec leur bloc).
    pub fn aimed(&self, eye: Vec3, dir: Vec3, max: f32) -> Option<(f32, HeadSkin)> {
        let mut best: Option<(f32, HeadSkin)> = None;
        for (&key, c) in &self.cells {
            if c.free.is_empty() || cell_distance(key, eye) > max + 24.0 {
                continue;
            }
            for &i in &c.free {
                let h = &self.free[i];
                if !matches!(h.pose, Pose::Free(_)) {
                    continue;
                }
                let inv = transform(h).inverse();
                let (o, d) = (inv.transform_point3(eye), inv.transform_vector3(dir));
                if let Some(t) = ray_box(o, d, Vec3::new(-4.0, 0.0, -4.0), Vec3::new(4.0, 8.0, 4.0))
                {
                    if t <= max && best.as_ref().is_none_or(|b| t < b.0) {
                        best = Some((t, h.skin.clone()));
                    }
                }
            }
        }
        best
    }

    /// Zones à dessiner : (identifiant, triangles, version).
    pub fn visible(&self, cam: Vec3) -> Vec<(u64, &[EntityVertex], u64)> {
        self.cells
            .iter()
            .filter(|(&k, c)| {
                c.gen != 0 && !c.verts.is_empty() && cell_distance(k, cam) < RANGE + 8.0
            })
            .map(|(&(x, z), c)| {
                (
                    (x as u32 as u64) << 32 | z as u32 as u64,
                    c.verts.as_slice(),
                    c.gen,
                )
            })
            .collect()
    }
}

/// Réécrit un fichier de lignes (supprimé s'il n'en reste aucune).
fn write_lines<'a>(path: &Path, lines: impl Iterator<Item = &'a str>) {
    let mut t = lines.collect::<Vec<_>>().join("\n");
    if t.is_empty() {
        let _ = std::fs::remove_file(path);
        return;
    }
    t.push('\n');
    let _ = std::fs::write(path, t);
}

/// Distance du rayon `o` + t `d` à la boîte [`lo`, `hi`] (t ≥ 0), s'il la touche.
fn ray_box(o: Vec3, d: Vec3, lo: Vec3, hi: Vec3) -> Option<f32> {
    let (mut t0, mut t1) = (0.0f32, f32::MAX);
    for i in 0..3 {
        if d[i].abs() < 1e-9 {
            if o[i] < lo[i] || o[i] > hi[i] {
                return None;
            }
            continue;
        }
        let (a, b) = ((lo[i] - o[i]) / d[i], (hi[i] - o[i]) / d[i]);
        t0 = t0.max(a.min(b));
        t1 = t1.min(a.max(b));
        if t0 > t1 {
            return None;
        }
    }
    Some(t0)
}

/// Une tête avec son skin (celui par défaut tant que le sien n'est pas chargé).
fn emit_head(out: &mut Vec<EntityVertex>, skin: &HeadSkin, xf: Mat4, light: [f32; 3], lod: usize) {
    match loaded(skin) {
        Some(s) => out.extend(crate::player_model::head(&s, xf, light, lod)),
        None => out.extend(crate::player_model::default_head(xf, light, lod)),
    }
}

/// Bâti de bois d'un porte-armure (pixels, y vers le haut depuis les pieds, comme celui de
/// Minecraft) : socle, jambes, hanches, deux montants, épaules, cou, bras.
fn emit_stand(out: &mut Vec<EntityVertex>, xf: Mat4, flags: u8, light: [f32; 3]) {
    const WOOD: [f32; 3] = [0.36, 0.22, 0.1];
    const STONE: [f32; 3] = [0.3, 0.3, 0.3];
    let mut boxes: Vec<([f32; 3], [f32; 3], [f32; 3])> = vec![
        ([-2.9, 1.0, -1.0], [-0.9, 12.0, 1.0], WOOD),
        ([0.9, 1.0, -1.0], [2.9, 12.0, 1.0], WOOD),
        ([-4.0, 12.0, -1.0], [4.0, 14.0, 1.0], WOOD),
        ([-3.0, 14.0, -1.0], [-1.0, 21.0, 1.0], WOOD),
        ([1.0, 14.0, -1.0], [3.0, 21.0, 1.0], WOOD),
        ([-6.0, 21.0, -1.5], [6.0, 24.0, 1.5], WOOD),
        ([-1.0, 23.0, -1.0], [1.0, 30.0, 1.0], WOOD),
    ];
    if flags & 1 == 0 {
        boxes.push(([-6.0, 0.0, -6.0], [6.0, 1.0, 6.0], STONE));
    }
    if flags & 2 != 0 {
        boxes.push(([-7.0, 12.0, -1.0], [-5.0, 24.0, 1.0], WOOD));
        boxes.push(([5.0, 12.0, -1.0], [7.0, 24.0, 1.0], WOOD));
    }
    let lit = EntityVertex::light(light, true);
    for (lo, hi, rgb) in boxes {
        let (lo, hi) = (Vec3::from(lo), Vec3::from(hi));
        for f in 0..6 {
            let (axis, side) = (f / 2, f % 2);
            let mut n = Vec3::ZERO;
            n[axis] = if side == 0 { 1.0 } else { -1.0 };
            let (u, v) = ((axis + 1) % 3, (axis + 2) % 3);
            let corner = |a: f32, b: f32| {
                let mut p = Vec3::ZERO;
                p[axis] = if side == 0 { hi[axis] } else { lo[axis] };
                p[u] = if a == 0.0 { lo[u] } else { hi[u] };
                p[v] = if b == 0.0 { lo[v] } else { hi[v] };
                xf.transform_point3(p)
            };
            let wn = xf.transform_vector3(n).normalize_or_zero();
            let shade = (0.78 + 0.22 * wn.y + 0.06 * wn.x).clamp(0.45, 1.0);
            let color = rgb.map(|c| c * shade);
            let q = [
                corner(0.0, 0.0),
                corner(1.0, 0.0),
                corner(1.0, 1.0),
                corner(0.0, 1.0),
            ];
            for i in [0usize, 1, 2, 2, 3, 0] {
                out.push(EntityVertex {
                    pos: q[i].to_array(),
                    color,
                    uv: [0.0; 2],
                    layer: -1,
                    light: lit,
                });
            }
        }
    }
}

/// Tête pour l'interface ou la main : pixels du modèle de la tête → écran (`mvp`), couleurs
/// multipliées par `light`.
pub fn head_vm(skin: Option<&Skin>, mvp: Mat4, light: [f32; 3]) -> Vec<VmVertex> {
    let verts = match skin {
        Some(s) => crate::player_model::head(s, Mat4::IDENTITY, [1.0; 3], 1),
        None => crate::player_model::default_head(Mat4::IDENTITY, [1.0; 3], 1),
    };
    verts
        .into_iter()
        .map(|v| {
            let clip = mvp * Vec3::from(v.pos).extend(1.0);
            VmVertex {
                clip: clip.to_array(),
                uv: [0.0; 2],
                layer: minelib_render::NO_TEXTURE,
                _pad: 0,
                color: [
                    v.color[0] * light[0],
                    v.color[1] * light[1],
                    v.color[2] * light[2],
                    1.0,
                ],
            }
        })
        .collect()
}

/// Icône 3D d'une tête (vue de trois quarts, le visage devant à gauche), centrée en
/// (`cx`, `cy`), large de `size` pixels, comme celle des blocs.
pub fn icon(cx: f32, cy: f32, size: f32, skin: &HeadSkin) -> Vec<VmVertex> {
    let s = loaded(skin);
    // Même vue que les blocs (cube de côté 1 vu à 45° et 30°), la tête un peu plus grande
    // qu'un demi-bloc pour remplir la case.
    let k = size / 1.62;
    let m = Mat4::from_translation(Vec3::new(cx, cy, 0.5))
        * Mat4::from_scale(Vec3::new(k, -k, 0.02))
        * Mat4::from_rotation_x(std::f32::consts::FRAC_PI_6)
        * Mat4::from_rotation_y(-std::f32::consts::FRAC_PI_4)
        * Mat4::from_rotation_y(std::f32::consts::PI)
        * Mat4::from_scale(Vec3::splat(0.9 / 8.0))
        * Mat4::from_translation(Vec3::new(0.0, -4.0, 0.0));
    head_vm(s.as_deref(), m, [1.0; 3])
}

/// Distance horizontale de la caméra au centre d'une zone.
fn cell_distance(key: (i32, i32), cam: Vec3) -> f32 {
    let c = Vec3::new(key.0 as f32 * 16.0 + 8.0, cam.y, key.1 as f32 * 16.0 + 8.0);
    c.distance(cam)
}

/// Texture d'une tête (PNG 64 × 64) depuis les serveurs de textures de Minecraft.
fn download(hash: &str) -> anyhow::Result<Vec<u8>> {
    use ureq::tls::{RootCerts, TlsConfig};
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .tls_config(
            TlsConfig::builder()
                .root_certs(RootCerts::PlatformVerifier)
                .build(),
        )
        .timeout_global(Some(std::time::Duration::from_secs(20)))
        .build()
        .into();
    let mut resp = agent
        .get(&format!("https://textures.minecraft.net/texture/{hash}"))
        .header("User-Agent", "minelib")
        .call()?;
    let bytes = resp
        .body_mut()
        .with_config()
        .limit(256 * 1024)
        .read_to_vec()?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HASH: &str = "31a7973f25cbbb078f595c04349fc541ac1df9a44b8c1a16c528581aa4170";

    fn face(m: Mat4) -> Vec3 {
        m.transform_vector3(-Vec3::Z).normalize()
    }

    #[test]
    fn reads_and_writes_every_kind_of_head_line() {
        let h = parse(&format!("6,168,-298,8,{HASH}")).unwrap();
        assert_eq!(h.pose, Pose::Floor(8));
        assert_eq!(h.skin, HeadSkin::Mc(HASH.into()));
        assert!(
            parse("1,2,3,w2,abcdefabcdefabcdefabcdefabcdefab").is_none(),
            "mur inconnu"
        );
        assert!(parse("1,2,3,0,zzz").is_none());
        assert!(
            parse("1,2,3,f4,bloc:stone").is_none(),
            "cadre : lu ailleurs"
        );
        assert_eq!(parse("1,2,3,w5,-").unwrap().skin, HeadSkin::Default);
        assert_eq!(
            parse("1,2,3,0,j:Viendax").unwrap().skin,
            HeadSkin::Player("Viendax".into())
        );
        assert!(parse("1,2,3,0,j:a b").is_none(), "pseudo invalide");
        let m = armor_stand_head(Vec3::new(1.5, 64.0, -2.5), 30.0, [10.0, 0.0, 0.0], false);
        for h in [
            Head {
                pos: [1, 65, -3],
                pose: Pose::Free(pack(m)),
                skin: HeadSkin::Mc(HASH.into()),
            },
            Head {
                pos: [1, 64, -3],
                pose: Pose::Stand(pack(armor_stand_body(Vec3::ZERO, 30.0, false)), 2),
                skin: HeadSkin::Default,
            },
            Head {
                pos: [-7, 3, 9],
                pose: Pose::Floor(13),
                skin: HeadSkin::Player("Bob_42".into()),
            },
        ] {
            let l = line(&h);
            assert_eq!(parse(&l), Some(h.clone()), "{l}");
        }
        assert!(transform_at(
            [0; 3],
            parse(&line(&Head {
                pos: [0; 3],
                pose: Pose::Free(pack(m)),
                skin: HeadSkin::Default,
            }))
            .unwrap()
            .pose
        )
        .abs_diff_eq(m, 1e-5));
    }

    #[test]
    fn floor_heads_turn_like_minecraft_and_walls_face_out() {
        let at = |p: Pose| transform_at([0, 0, 0], p);
        // Minecraft : 0 regarde le nord, 4 l'est, 8 le sud, 12 l'ouest ; 2 le nord-est.
        assert!(face(at(Pose::Floor(0))).distance(-Vec3::Z) < 1e-4);
        assert!(face(at(Pose::Floor(4))).distance(Vec3::X) < 1e-4);
        assert!(face(at(Pose::Floor(8))).distance(Vec3::Z) < 1e-4);
        assert!(face(at(Pose::Floor(12))).distance(-Vec3::X) < 1e-4);
        let ne = Vec3::new(1.0, 0.0, -1.0).normalize();
        assert!(face(at(Pose::Floor(2))).distance(ne) < 1e-4);
        // Au sol : posée au milieu du bloc, un demi-bloc de haut.
        let m = at(Pose::Floor(0));
        let (a, b) = (
            m.transform_point3(Vec3::new(-4.0, 0.0, -4.0)),
            m.transform_point3(Vec3::new(4.0, 8.0, 4.0)),
        );
        assert!(a.min(b).distance(Vec3::new(0.25, 0.0, 0.25)) < 1e-4);
        assert!(a.max(b).distance(Vec3::new(0.75, 0.5, 0.75)) < 1e-4);
        // Contre le mur nord (côté 5, −z) : regarde le sud, collée au mur.
        assert!(face(at(Pose::Wall(5))).distance(Vec3::Z) < 1e-4);
        let c = at(Pose::Wall(5)).transform_point3(Vec3::new(0.0, 4.0, 0.0));
        assert!((c.z - 0.25).abs() < 1e-3 && (c.y - 0.5).abs() < 0.01, "{c}");
        // Les blocs de la v157 portent leur pose.
        assert_eq!(
            skull_pose(minelib_world::registry::skull_block(false, 7)),
            Some((false, 7))
        );
        assert_eq!(
            skull_pose(minelib_world::registry::skull_block(true, 4)),
            Some((true, 4))
        );
    }

    #[test]
    fn heads_carried_by_entities_sit_where_minecraft_draws_them() {
        // Porte-armure regardant le sud : tête sur le cou (bas à 1,44 bloc), agrandie.
        let m = armor_stand_head(Vec3::new(0.5, 10.0, 0.5), 0.0, [0.0; 3], false);
        assert!(face(m).distance(Vec3::Z) < 1e-4);
        let bottom = m.transform_point3(Vec3::ZERO);
        assert!((bottom.y - 11.438).abs() < 0.01, "{bottom}");
        let top = m.transform_point3(Vec3::new(0.0, 8.0, 0.0));
        assert!((top.y - bottom.y - 0.59375).abs() < 1e-3);
        // Tourné de −90° (Minecraft : vers l'est).
        let m = armor_stand_head(Vec3::ZERO, -90.0, [0.0; 3], false);
        assert!(face(m).distance(Vec3::X) < 1e-4);
        // Tête penchée en avant (x > 0) : le visage descend.
        let m = armor_stand_head(Vec3::ZERO, 0.0, [30.0, 0.0, 0.0], false);
        assert!(face(m).y < -0.3, "{}", face(m));
        // Entité d'affichage sans transformation : la tête sous son origine, vers −z.
        let f = display_frame(Vec3::new(2.0, 5.0, 2.0), 0.0, 0.0, Mat4::IDENTITY);
        let m = display_head(f);
        assert!(face(m).distance(-Vec3::Z) < 1e-4);
        let (a, b) = (
            m.transform_point3(Vec3::new(-4.0, 0.0, -4.0)),
            m.transform_point3(Vec3::new(4.0, 8.0, 4.0)),
        );
        assert!(
            a.min(b).distance(Vec3::new(1.75, 4.5, 1.75)) < 1e-4,
            "{}",
            a.min(b)
        );
        assert!(a.max(b).distance(Vec3::new(2.25, 5.0, 2.25)) < 1e-4);
        // Tournée d'un quart de tour (yaw 90 : Minecraft regarde l'ouest… le visage suit).
        let f = display_frame(Vec3::ZERO, 90.0, 0.0, Mat4::IDENTITY);
        assert!(
            face(display_head(f)).distance(Vec3::X) < 1e-4,
            "{}",
            face(display_head(f))
        );
        // Cadre invisible sur un mur à l'est de sa case (il regarde l'ouest) : la tête, à
        // demi-taille, à moitié dans le mur, le visage vers l'ouest.
        let m = frame_head([-48, 89, 73], 4, 0, true);
        assert!(face(m).distance(-Vec3::X) < 1e-4, "{}", face(m));
        let c = m.transform_point3(Vec3::new(0.0, 4.0, 0.0));
        assert!(c.distance(Vec3::new(-47.0, 89.5, 73.5)) < 1e-3, "{c}");
        assert_eq!(cell_of(m), [-47, 89, 73]);
    }

    #[test]
    fn heads_are_lit_like_the_blocks_around_them() {
        let noon = lit([0, 0, 0, 15], 1.0);
        let night = lit([0, 0, 0, 15], 0.0);
        let cave = lit([0, 0, 0, 0], 1.0);
        let torch = lit([255, 200, 120, 0], 0.0);
        assert!(noon[0] > 0.8 && noon[0] < 1.0, "{noon:?}");
        assert!(night[0] < 0.06, "{night:?}");
        assert!(cave[0] < 0.02, "{cave:?}");
        assert!(torch[0] > 1.0 && torch[2] < torch[0], "{torch:?}");
        // Sous un toit (ciel 10/15) : bien plus sombre qu'en plein soleil.
        assert!(lit([0, 0, 0, 10], 1.0)[0] < noon[0] * 0.5);
    }

    #[test]
    fn heads_come_and_go_with_their_blocks() {
        let dir = std::env::temp_dir().join(format!("minelib_tetes_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(crate::mcimport::HEADS_FILE),
            format!("3,64,5,w4,{HASH}\n3,64,5,f4,bloc:stone\n"),
        )
        .unwrap();
        let mut h = Heads::load(&dir, dir.join("cache"));
        assert_eq!(h.skin_at([3, 64, 5]), HeadSkin::Mc(HASH.into()));
        let wall = minelib_world::registry::skull_block(true, 4);
        let floor = minelib_world::registry::skull_block(false, 3);
        h.chunk_meshed(
            ChunkPos { x: 0, z: 0 },
            vec![
                ([3, 64, 5], wall, [0, 0, 0, 15]),
                ([4, 64, 5], floor, [0, 0, 0, 15]),
            ],
            Vec::new(),
        );
        let cam = Vec3::new(3.0, 65.0, 8.0);
        h.refresh(cam, 1.0);
        let v = h.visible(cam);
        assert_eq!(v.len(), 1);
        // Deux têtes (texture pas chargée : celle du jeu par défaut), en entier.
        assert_eq!(v[0].1.len(), 2 * 6 * 64 * 6);
        let gen = v[0].2;
        // Une tête posée par le joueur, au skin d'un joueur : gardée à part.
        h.place(
            [4, 64, 5],
            Pose::Floor(3),
            HeadSkin::Player("Viendax".into()),
            &dir,
        );
        let placed = std::fs::read_to_string(dir.join(PLACED_FILE)).unwrap();
        assert_eq!(placed, "4,64,5,3,j:Viendax\n");
        assert!(h.players().any(|p| p == "Viendax"));
        // Cassée : oubliée, le cadre de la même case reste.
        h.remove_at([3, 64, 5], &dir);
        let text = std::fs::read_to_string(dir.join(crate::mcimport::HEADS_FILE)).unwrap();
        assert_eq!(text, "3,64,5,f4,bloc:stone\n");
        h.chunk_meshed(
            ChunkPos { x: 0, z: 0 },
            vec![([4, 64, 5], floor, [0, 0, 0, 15])],
            Vec::new(),
        );
        h.refresh(cam, 1.0);
        let v = h.visible(cam);
        assert!(v[0].2 > gen);
        assert_eq!(v[0].1.len(), 6 * 64 * 6);
        h.remove_at([4, 64, 5], &dir);
        assert!(!dir.join(PLACED_FILE).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_head_carried_by_an_entity_can_be_aimed_at() {
        let dir = std::env::temp_dir().join(format!("minelib_tetes_vise_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let m = armor_stand_head(Vec3::new(0.5, 64.0, 0.5), 0.0, [0.0; 3], false);
        let head = Head {
            pos: cell_of(m),
            pose: Pose::Free(pack(m)),
            skin: HeadSkin::Mc(HASH.into()),
        };
        std::fs::write(dir.join(crate::mcimport::HEADS_FILE), line(&head) + "\n").unwrap();
        let h = Heads::load(&dir, dir.join("cache"));
        assert_eq!(
            h.probes(),
            vec![(ChunkPos { x: 0, z: 0 }, vec![[0, 65, 0]])]
        );
        let eye = Vec3::new(0.5, 65.7, 4.0);
        let (t, skin) = h.aimed(eye, -Vec3::Z, 6.0).expect("tête visée");
        assert!((t - (4.0 - 0.5 - 0.297)).abs() < 0.01, "{t}");
        assert_eq!(skin, HeadSkin::Mc(HASH.into()));
        assert!(h.aimed(eye, Vec3::Z, 6.0).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn inventory_heads_keep_their_skin() {
        let a = intern(&HeadSkin::Player("Alice".into()));
        let b = intern(&HeadSkin::Mc(HASH.into()));
        assert_ne!(a, b);
        assert_eq!(intern(&HeadSkin::Player("Alice".into())), a);
        assert_eq!(skin_of(a), HeadSkin::Player("Alice".into()));
        assert_eq!(skin_of(b).item_name(), "Tête (skin de Minecraft)");
    }
}
