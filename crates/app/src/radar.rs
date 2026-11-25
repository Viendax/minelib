//! Minimap (radar) du mode FPS, en haut à droite, avec les règles de CS : vue de dessus
//! autour du joueur, qui tourne avec lui (son regard toujours en haut, le nord marqué d'un
//! « N »), murs clairs et sol sombre selon la hauteur, sites de
//! bombe, coéquipiers toujours (à leur couleur), adversaires seulement quand ils sont
//! repérés (ceux que l'hôte nous envoie, c'est-à-dire vus), bombe posée ou lâchée.

use glam::Vec3;
use minelib_net::game::FpsMap;
use minelib_render::Ui;
use minelib_world::World;

/// Rayon montré, en blocs (vue de haut : une bonne partie de la carte).
pub const RADIUS: i32 = 48;
/// Rayon gardé en mémoire autour du centre de la grille (plus grand que la vue : on peut
/// s'éloigner du centre sans voir de bord).
const GRID: i32 = 72;
const SIDE: usize = (GRID * 2) as usize;
/// Lignes de la grille recalculées à chaque image (rafraîchissement continu, sans à-coup :
/// blocs qui changent, morceaux de carte chargés après coup).
const ROWS_PER_FRAME: usize = 8;

/// Teintes des cases : vide, sol (4 hauteurs), mur.
const SHADES: [[f32; 4]; 6] = [
    [0.0, 0.0, 0.0, 0.0],
    [0.16, 0.17, 0.19, 0.85],
    [0.22, 0.23, 0.26, 0.85],
    [0.29, 0.30, 0.33, 0.85],
    [0.36, 0.37, 0.41, 0.85],
    [0.80, 0.80, 0.78, 0.92],
];

#[derive(Default)]
pub struct Radar {
    /// Bloc au centre de la grille gardée.
    center: Option<[i32; 3]>,
    /// Teinte (indice dans `SHADES`) de chaque case, ligne par ligne (z puis x).
    cells: Vec<u8>,
    /// Prochaine ligne à recalculer.
    next_row: usize,
    /// Sites de bombe : lettre et centre (x, z).
    sites: Vec<(char, [f32; 2])>,
    /// Relevé de la carte (où l'on peut aller, murs qui bornent) : la minimap ne montre
    /// alors que lui. Sans relevé (pas encore fait) : les blocs autour du joueur.
    survey: Option<Survey>,
    /// Dossier de la carte, et temps avant de regarder de nouveau si le relevé est prêt.
    dir: Option<std::path::PathBuf>,
    poll: f32,
    /// Case des pieds du joueur (hauteur) : les teintes du relevé en dépendent.
    feet: i32,
    /// (v322) Écart (en blocs, x et z) entre l'origine de la partie et celle du relevé (celle
    /// des parties de la carte, `fpsmap::match_origin`) : une case de la partie est celle du
    /// relevé décalée d'autant. Nul pour une partie en ligne, et pour toutes les cartes d'avant.
    shift: [i32; 2],
}

impl Radar {
    /// Sites de bombe de la carte.
    pub fn set_map(&mut self, map: &FpsMap) {
        self.sites = map
            .sites
            .iter()
            .enumerate()
            .filter_map(|(i, z)| {
                z.as_ref().map(|z| {
                    let c = [
                        (z.min[0] + z.max[0]) as f32 / 2.0 + 0.5,
                        (z.min[2] + z.max[2]) as f32 / 2.0 + 0.5,
                    ];
                    ((b'A' + i as u8) as char, c)
                })
            })
            .collect();
    }

    /// Carte jouée : son relevé est lu (ou lancé en tâche de fond s'il manque). (v322) La
    /// partie compte depuis l'origine `origin` (en blocs, `World::origin_blocks`).
    pub fn set_dir(&mut self, dir: &std::path::Path, origin: [i32; 3]) {
        self.dir = Some(dir.to_path_buf());
        self.poll = 0.0;
        let (sx, sz) = crate::fpsmap::match_origin(&crate::fpsmap::load(dir)).origin();
        let w = minelib_world::lap::wrap_block;
        self.shift = [
            w(origin[0] as i64 - sx as i64),
            w(origin[2] as i64 - sz as i64),
        ];
    }

    /// Le relevé de la carte est-il arrivé ? (Regardé une fois par seconde.)
    fn poll_survey(&mut self, dt: f32) {
        if self.survey.is_some() {
            return;
        }
        self.poll -= dt;
        let Some(dir) = self.dir.clone().filter(|_| self.poll <= 0.0) else {
            return;
        };
        self.poll = 1.0;
        match Survey::load(&dir) {
            // Carte sans zones de départ : rien à relever, on garde les blocs autour.
            Some(s) if s.size == [0, 0] => self.dir = None,
            Some(s) => {
                self.survey = Some(s);
                // Tout redessiner d'après le relevé.
                self.center = None;
            }
            None => survey_in_background(&dir),
        }
    }

    /// Suit le joueur : la grille est recentrée (en gardant ce qui est déjà calculé) quand
    /// il s'éloigne du centre, et quelques lignes sont recalculées à chaque image.
    pub fn update(&mut self, world: &World, feet: Vec3) {
        self.poll_survey(1.0 / 60.0);
        let c = [
            feet.x.floor() as i32,
            feet.y.floor() as i32,
            feet.z.floor() as i32,
        ];
        self.feet = c[1];
        // (v165) Relevé de la carte (fait d'avance) : lu directement au dessin, tout de
        // suite et partout (plus de morceaux qui manquent le temps d'être recalculés).
        if self.survey.is_some() {
            self.center = Some(c);
            return;
        }
        match self.center {
            None => {
                self.center = Some(c);
                self.cells = vec![0; SIDE * SIDE];
                // Première fois : tout de suite en entier.
                for row in 0..SIDE {
                    self.row(world, row);
                }
            }
            Some(o)
                if (o[0] - c[0]).abs() > GRID - RADIUS - 4
                    || (o[2] - c[2]).abs() > GRID - RADIUS - 4
                    || (o[1] - c[1]).abs() >= 3 =>
            {
                // Recentrage : ce qui reste dans la grille est déplacé, pas recalculé.
                let (dx, dz) = (c[0] - o[0], c[2] - o[2]);
                let mut cells = vec![0; SIDE * SIDE];
                for z in 0..SIDE as i32 {
                    for x in 0..SIDE as i32 {
                        let (ox, oz) = (x + dx, z + dz);
                        if (0..SIDE as i32).contains(&ox) && (0..SIDE as i32).contains(&oz) {
                            cells[(z as usize) * SIDE + x as usize] =
                                self.cells[(oz as usize) * SIDE + ox as usize];
                        }
                    }
                }
                self.cells = cells;
                self.center = Some(c);
            }
            _ => {}
        }
        // Quelques lignes par image, toutes à tour de rôle.
        for _ in 0..ROWS_PER_FRAME {
            let row = self.next_row % SIDE;
            self.row(world, row);
            self.next_row = (self.next_row + 1) % SIDE;
        }
    }

    /// Recalcule la ligne `row` de la grille.
    fn row(&mut self, world: &World, row: usize) {
        let Some(c) = self.center else { return };
        let z = c[2] - GRID + row as i32;
        for dx in 0..SIDE {
            let x = c[0] - GRID + dx as i32;
            self.cells[row * SIDE + dx] = match &self.survey {
                Some(s) => s.shade(x + self.shift[0], c[1], z + self.shift[1]),
                None => shade(world, x, c[1], z),
            };
        }
    }

    /// Teinte (`SHADES`) de la colonne (`wx`, `wz`) : d'après le relevé de la carte, sinon
    /// d'après la grille des blocs autour du joueur.
    fn shade_at(&self, c: [i32; 3], wx: i32, wz: i32) -> u8 {
        if let Some(s) = &self.survey {
            return s.shade(wx + self.shift[0], self.feet, wz + self.shift[1]);
        }
        let (col, row) = (wx - (c[0] - GRID), wz - (c[2] - GRID));
        if (0..SIDE as i32).contains(&col) && (0..SIDE as i32).contains(&row) {
            self.cells[row as usize * SIDE + col as usize]
        } else {
            0
        }
    }

    /// Dessine le radar (carré de `size` pixels en `x`, `y`), centré exactement sur le
    /// joueur (il glisse avec lui, sans à-coup) et tourné avec lui : la direction de son
    /// regard est toujours en haut. `me` : position, regard et couleur ; `mates` :
    /// coéquipiers en vie (position, couleur) ; `enemies` : adversaires repérés ; `bomb` :
    /// bombe connue.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &self,
        ui: &mut Ui,
        x: f32,
        y: f32,
        size: f32,
        s: f32,
        me: (Vec3, Vec3, [f32; 4]),
        me_bomb: bool,
        mates: &[(Vec3, [f32; 4], bool)],
        enemies: &[(Vec3, bool)],
        bomb: Option<Vec3>,
    ) {
        let Some(c) = self.center else { return };
        ui.rect(
            x - s,
            y - s,
            size + 2.0 * s,
            size + 2.0 * s,
            [0.0, 0.0, 0.0, 0.6],
        );
        let n = (RADIUS * 2) as usize;
        let cell = size / n as f32;
        let (cx, cy) = (x + size / 2.0, y + size / 2.0);
        // Haut de la minimap : la direction du regard ; à droite : la droite du joueur.
        let look = glam::Vec2::new(me.1.x, me.1.z);
        let d = if look.length_squared() > 1e-8 {
            look.normalize()
        } else {
            glam::Vec2::new(0.0, -1.0)
        };
        let r = glam::Vec2::new(-d.y, d.x);
        let (x1, y1) = (x + size, y + size);
        // Cases : chaque carré de l'écran prend la teinte de la colonne du monde qui s'y
        // trouve (carte tournée), par bandes de même teinte.
        for j in 0..n {
            let b = -((j as f32 + 0.5) * cell - size / 2.0) / cell;
            let mut run: Option<(usize, u8)> = None;
            for i in 0..=n {
                let v = if i < n {
                    let a = ((i as f32 + 0.5) * cell - size / 2.0) / cell;
                    let w = r * a + d * b;
                    let wx = (me.0.x + w.x).floor() as i32;
                    let wz = (me.0.z + w.y).floor() as i32;
                    self.shade_at(c, wx, wz)
                } else {
                    u8::MAX
                };
                match run {
                    Some((_, v0)) if v0 == v => {}
                    Some((i0, v0)) => {
                        if v0 != 0 {
                            let left = x + i0 as f32 * cell;
                            let top = y + j as f32 * cell;
                            // Un demi-pixel de plus : pas de fente entre les bandes.
                            ui.rect(
                                left,
                                top,
                                ((i - i0) as f32 * cell + 0.5).min(x1 - left),
                                (cell + 0.5).min(y1 - top),
                                SHADES[v0 as usize],
                            );
                        }
                        run = Some((i, v));
                    }
                    None => run = Some((i, v)),
                }
            }
        }
        // Position monde → radar. Hors du cadre : collé au bord.
        let screen = |p: Vec3| -> (f32, f32) {
            let w = glam::Vec2::new(p.x - me.0.x, p.z - me.0.z);
            (cx + w.dot(r) * cell, cy - w.dot(d) * cell)
        };
        let to = |p: Vec3| -> (f32, f32) {
            let (px, py) = screen(p);
            (px.clamp(x, x1), py.clamp(y, y1))
        };
        // Le nord, au bord du cadre dans sa direction.
        {
            let north = glam::Vec2::new(0.0, -1.0);
            let (dx, dy) = (north.dot(r), -north.dot(d));
            let k = (size / 2.0 - 5.0 * s) / dx.abs().max(dy.abs()).max(1e-3);
            let (px, py) = (cx + dx * k, cy + dy * k);
            let tw = Ui::text_width("N", s);
            ui.text(px - tw / 2.0, py - 4.0 * s, s, [0.95, 0.95, 0.9, 0.85], "N");
        }
        for (letter, at) in &self.sites {
            let (px, py) = to(Vec3::new(at[0], 0.0, at[1]));
            let t = letter.to_string();
            let tw = Ui::text_width(&t, s);
            ui.text(px - tw / 2.0, py - 4.0 * s, s, [1.0, 0.45, 0.35, 0.95], t);
        }
        let dot = |ui: &mut Ui, p: Vec3, col: [f32; 4], r: f32| {
            let (px, py) = to(p);
            ui.rect(
                px - r - s,
                py - r - s,
                2.0 * r + 2.0 * s,
                2.0 * r + 2.0 * s,
                [0.0, 0.0, 0.0, 0.8],
            );
            ui.rect(px - r, py - r, 2.0 * r, 2.0 * r, col);
        };
        // (v209) Logo de bombe (celui qui la porte), à la couleur de son point.
        let bomb_icon = |ui: &mut Ui, (px, py): (f32, f32), col: [f32; 4]| {
            let px_size = s * 0.9;
            let half = 3.5 * px_size;
            ui.rect(
                px - half - s,
                py - half - s,
                2.0 * half + 2.0 * s,
                2.0 * half + 2.0 * s,
                [0.0, 0.0, 0.0, 0.8],
            );
            for (row, line) in ICON_BOMB.iter().enumerate() {
                for (k, ch) in line.chars().enumerate() {
                    if ch == '#' {
                        ui.rect(
                            px - half + k as f32 * px_size,
                            py - half + row as f32 * px_size,
                            px_size + 0.3,
                            px_size + 0.3,
                            col,
                        );
                    }
                }
            }
        };
        if let Some(b) = bomb {
            dot(ui, b, [1.0, 0.25, 0.2, 1.0], 2.0 * s);
        }
        let enemy_col = [0.95, 0.2, 0.2, 1.0];
        for &(e, carries) in enemies {
            if carries {
                bomb_icon(ui, to(e), enemy_col);
            } else {
                dot(ui, e, enemy_col, 1.5 * s);
            }
        }
        for &(p, col, carries) in mates {
            if carries {
                bomb_icon(ui, to(p), col);
            } else {
                dot(ui, p, col, 1.5 * s);
            }
        }
        // Soi : point à sa couleur (cerclé de blanc) au centre, trait vers le haut (le
        // regard).
        for k in 1..=4 {
            let f = k as f32 * 1.6 * s;
            ui.rect(cx - s / 2.0, cy - f - s / 2.0, s, s, [1.0, 1.0, 1.0, 0.9]);
        }
        // (v209) Soi avec la bombe : son logo à sa couleur.
        if me_bomb {
            bomb_icon(ui, (cx, cy), me.2);
            return;
        }
        let r = 2.0 * s;
        ui.rect(
            cx - r - s,
            cy - r - s,
            2.0 * r + 2.0 * s,
            2.0 * r + 2.0 * s,
            [1.0; 4],
        );
        ui.rect(cx - r, cy - r, 2.0 * r, 2.0 * r, me.2);
    }
}

/// (v209) Logo de la bombe sur la minimap (7 × 7 points, dessiné pour Minelib) : une charge
/// avec sa mèche.
const ICON_BOMB: [&str; 7] = [
    "....#..", "...#...", "..###..", ".#####.", "#######", ".#####.", "..###..",
];

/// Teinte d'une colonne : mur s'il y a un bloc plein à hauteur du torse, sinon sol (plus
/// clair quand il est plus haut), vide s'il n'y a rien en dessous.
fn shade(world: &World, x: i32, feet: i32, z: i32) -> u8 {
    let solid = |y: i32| world.get(x, y, z).is_solid();
    if solid(feet + 1) {
        return 5;
    }
    // `feet` est la case des pieds : le sol normal est juste en dessous (d = 1).
    for d in 0..12 {
        if solid(feet - d) {
            return match d {
                0 => 4,
                1 => 3,
                2..=3 => 2,
                _ => 1,
            };
        }
    }
    0
}

/// Fichier du relevé d'une carte.
const SURVEY_FILE: &str = "radar.bin";

/// Révision de la méthode de relevé : la changer refait les relevés déjà enregistrés.
const SURVEY_REV: u64 = 2;

/// Empreinte attendue d'un relevé à jour de la carte `dir`.
fn survey_stamp(dir: &std::path::Path) -> u64 {
    crate::fpsmap::stamp(dir) ^ SURVEY_REV.wrapping_mul(0x9E37_79B9_7F4A_7C15)
}

/// Relevé d'une carte FPS : toutes les cases où un joueur peut se tenir en partant des
/// zones de départ (marche, saut, chute, échelle), et les murs qui bornent ces cases. Fait
/// une fois quand la carte est signée, puis gardé avec elle : la minimap est la même pour
/// tous et ne montre aucun mur inutile (décor hors d'atteinte, dessous de la carte).
#[derive(serde::Serialize, serde::Deserialize, Clone, Default, Debug, PartialEq)]
pub struct Survey {
    /// Empreinte des fichiers de la carte au moment du relevé (refait si elle change).
    stamp: u64,
    /// Coin (x, z) et taille (en x, en z) du rectangle relevé.
    min: [i32; 2],
    size: [u32; 2],
    /// Par colonne (z puis x) : hauteurs des cases où l'on peut se tenir, et hauteurs où
    /// la colonne fait mur pour une case voisine atteignable.
    floors: Vec<Vec<i16>>,
    walls: Vec<Vec<i16>>,
}

/// Plus longue chute qui fait encore partie de la carte (blocs).
const SURVEY_FALL: i32 = 7;

/// Cases explorées au plus (carte immense ou ouverte sur un terrain sans fin).
const SURVEY_MAX: usize = 3_000_000;

impl Survey {
    fn col(&self, x: i32, z: i32) -> Option<usize> {
        let (dx, dz) = (x - self.min[0], z - self.min[1]);
        ((0..self.size[0] as i32).contains(&dx) && (0..self.size[1] as i32).contains(&dz))
            .then(|| dz as usize * self.size[0] as usize + dx as usize)
    }

    /// Teinte de la colonne (`SHADES`) pour un joueur dont les pieds sont à la hauteur
    /// `feet` : le sol atteignable le plus proche de sa hauteur, sinon un mur à sa
    /// hauteur, sinon rien.
    fn shade(&self, x: i32, feet: i32, z: i32) -> u8 {
        let Some(c) = self.col(x, z) else { return 0 };
        let near = self.floors[c]
            .iter()
            .map(|&f| feet - f as i32)
            .filter(|d| (-3..=6).contains(d))
            .min_by_key(|d| d.abs());
        if let Some(d) = near {
            return match d {
                i32::MIN..=-1 => 4,
                0 => 3,
                1..=2 => 2,
                _ => 1,
            };
        }
        let wall = self.walls[c]
            .iter()
            .any(|&w| (-3..=3).contains(&(w as i32 - feet)));
        if wall {
            5
        } else {
            0
        }
    }

    /// Relevé enregistré avec la carte `dir`, s'il est à jour.
    pub fn load(dir: &std::path::Path) -> Option<Self> {
        let s: Self = minelib_net::game::decode(&std::fs::read(dir.join(SURVEY_FILE)).ok()?)?;
        let cols = s.size[0] as usize * s.size[1] as usize;
        (s.stamp == survey_stamp(dir) && s.floors.len() == cols && s.walls.len() == cols)
            .then_some(s)
    }

    /// Fait le relevé du monde `world` (chunks déjà chargés) pour la carte `map`.
    pub fn of(world: &World, map: &FpsMap) -> Self {
        use std::collections::{HashSet, VecDeque};
        let r = Reach::new(world);
        let mut seen: HashSet<Cell> = HashSet::new();
        let mut queue: VecDeque<Cell> = VecDeque::new();
        // Départs : toutes les cases des deux zones où l'on peut se tenir.
        for zone in [&map.ct, &map.t].into_iter().flatten() {
            for x in zone.min[0]..=zone.max[0] {
                for z in zone.min[2]..=zone.max[2] {
                    for y in zone.min[1] - 1..=zone.max[1] + 1 {
                        let c = [x, y, z];
                        if r.stand(c) && seen.insert(c) {
                            queue.push_back(c);
                        }
                    }
                }
            }
        }
        // Débogage (`MINELIB_SURVEY_TRACE=x,z`) : par où l'on arrive dans cette colonne.
        let trace: Option<[i32; 2]> = std::env::var("MINELIB_SURVEY_TRACE").ok().and_then(|v| {
            let mut it = v.split(',').filter_map(|t| t.trim().parse().ok());
            Some([it.next()?, it.next()?])
        });
        let mut from: std::collections::HashMap<Cell, Cell> = Default::default();
        while let Some(c) = queue.pop_front() {
            if seen.len() > SURVEY_MAX {
                break;
            }
            r.neighbors(c, |n, _| {
                if seen.insert(n) {
                    if trace.is_some() {
                        from.insert(n, c);
                    }
                    queue.push_back(n);
                }
            });
        }
        let full = |c: Cell| r.full(c);
        let up = |c: Cell, d: i32| [c[0], c[1] + d, c[2]];
        if let Some(t) = trace {
            let mut at = seen.iter().copied().find(|c| c[0] == t[0] && c[2] == t[1]);
            while let Some(c) = at {
                let key = |c: Cell| world.get(c[0], c[1], c[2]).def().key.clone();
                eprintln!("TRACE {c:?} dans {} sur {}", key(c), key(up(c, -1)));
                at = from.get(&c).copied();
            }
        }
        // Rectangle relevé : les cases atteintes, plus une de marge pour leurs murs.
        if seen.is_empty() {
            return Self::default();
        }
        let lo = |k: usize| seen.iter().map(|c| c[k]).min().unwrap_or(0) - 1;
        let hi = |k: usize| seen.iter().map(|c| c[k]).max().unwrap_or(0) + 1;
        let min = [lo(0), lo(2)];
        let size = [(hi(0) - min[0] + 1) as u32, (hi(2) - min[1] + 1) as u32];
        let mut s = Self {
            stamp: 0,
            min,
            size,
            floors: vec![Vec::new(); (size[0] * size[1]) as usize],
            walls: vec![Vec::new(); (size[0] * size[1]) as usize],
        };
        for c in &seen {
            if let Some(i) = s.col(c[0], c[2]) {
                s.floors[i].push(c[1] as i16);
            }
        }
        // Murs : la colonne voisine d'une case atteinte, quand elle la bloque vraiment
        // (pas un bord de vide) et qu'on ne peut pas s'y tenir à cette hauteur.
        for c in &seen {
            for dir in &DIRS[..4] {
                let n = [c[0] + dir[0], c[1], c[2] + dir[1]];
                let blocked = full(n) || full(up(n, 1));
                let walkable = (-1..=1).any(|d| seen.contains(&up(n, d)));
                if blocked && !walkable {
                    if let Some(i) = s.col(n[0], n[2]) {
                        s.walls[i].push(c[1] as i16);
                    }
                }
            }
        }
        for v in s.floors.iter_mut().chain(s.walls.iter_mut()) {
            v.sort_unstable();
            v.dedup();
        }
        s
    }
}

/// Une case du monde (x, y, z) : celle où sont les pieds.
pub type Cell = [i32; 3];

/// Façon de passer d'une case à une voisine (voir `Reach::neighbors`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Marcher (et tomber d'un bord).
    Walk,
    /// Sauter (monter d'une case, ou franchir un trou).
    Jump,
    /// Monter ou descendre une échelle (ou en sortir par le haut).
    Ladder,
}

/// Directions horizontales : les quatre côtés, puis les diagonales.
pub const DIRS: [[i32; 2]; 8] = [
    [1, 0],
    [-1, 0],
    [0, 1],
    [0, -1],
    [1, 1],
    [1, -1],
    [-1, 1],
    [-1, -1],
];

/// Où l'on peut aller dans un monde, et comment (marche, saut, chute, échelle) : les
/// règles du relevé de la minimap, et celles que suivent les bots du mode FPS pour trouver
/// leur chemin.
pub struct Reach<'a> {
    world: &'a World,
}

impl<'a> Reach<'a> {
    pub fn new(world: &'a World) -> Self {
        Self { world }
    }

    /// Hauteur de collision d'une case (16es de bloc ; 0 : on passe au travers). Les
    /// portes comptent comme ouvertes : un passage.
    fn top(&self, c: Cell) -> u8 {
        if c[1] < 0 || c[1] >= minelib_world::CHUNK_HEIGHT as i32 {
            return 0;
        }
        let def = self.world.get(c[0], c[1], c[2]).def();
        if !def.solid || def.key.starts_with("door_") || def.key.starts_with("ladder") {
            return 0;
        }
        // Clôtures, murets, vitres : des barrières, on ne monte pas dessus.
        if matches!(def.shape, minelib_world::registry::Shape::Post(_)) {
            return 24;
        }
        def.boxes().iter().map(|b| b.1[1]).max().unwrap_or(16)
    }

    fn loaded(&self, c: Cell) -> bool {
        self.world
            .contains(minelib_world::ChunkPos::containing(c[0], c[2]))
    }

    /// On traverse la case (rien, ou un obstacle bas : dalle, tapis).
    pub fn pass(&self, c: Cell) -> bool {
        self.top(c) <= 9
    }

    /// La case bloque.
    pub fn full(&self, c: Cell) -> bool {
        self.top(c) > 9
    }

    pub fn ladder(&self, c: Cell) -> bool {
        self.world
            .get(c[0], c[1], c[2])
            .def()
            .key
            .starts_with("ladder")
    }

    /// Place pour le corps (deux cases de haut).
    pub fn room(&self, c: Cell) -> bool {
        self.loaded(c) && self.pass(c) && self.pass(up(c, 1))
    }

    /// On peut s'y tenir.
    pub fn stand(&self, c: Cell) -> bool {
        self.room(c) && (self.full(up(c, -1)) || self.top(c) > 0 || self.ladder(c))
    }

    /// Lâché dans la case `c` : où l'on atterrit (ou rien : le vide, une trop longue
    /// chute).
    pub fn land(&self, mut c: Cell) -> Option<Cell> {
        if !self.room(c) {
            return None;
        }
        let from = c[1];
        while !self.stand(c) {
            c = up(c, -1);
            // Une chute de plus de `SURVEY_FALL` blocs ne fait pas partie de la carte
            // (on en meurt, ou c'est le dehors) ; de même le vide.
            if c[1] < 0 || from - c[1] > SURVEY_FALL || !self.pass(c) {
                return None;
            }
        }
        Some(c)
    }

    /// Les cases où l'on peut aller depuis `c` (où l'on se tient), et comment.
    pub fn neighbors(&self, c: Cell, mut go: impl FnMut(Cell, Step)) {
        // Échelle : monter et descendre.
        if self.ladder(c) {
            for d in [1, -1] {
                let n = up(c, d);
                if self.room(n) && (self.ladder(n) || self.stand(n)) {
                    go(n, Step::Ladder);
                } else if d == 1 && self.room(n) {
                    // Haut de l'échelle : on en sort vers le rebord d'à côté.
                    for dir in &DIRS[..4] {
                        if let Some(l) = self.land([n[0] + dir[0], n[1], n[2] + dir[1]]) {
                            go(l, Step::Ladder);
                        }
                    }
                }
            }
        }
        for dir in DIRS {
            let diag = dir[0] != 0 && dir[1] != 0;
            // En diagonale, le corps (0,6 bloc de large) ne passe pas entre deux blocs
            // qui se touchent par un coin, ni en rasant un seul : les deux cases de
            // côté doivent être libres à chaque pas.
            let open = |from: Cell| {
                !diag
                    || (self.room([from[0] + dir[0], from[1], from[2]])
                        && self.room([from[0], from[1], from[2] + dir[1]]))
            };
            // Marcher (et tomber du bord).
            if open(c) {
                if let Some(l) = self.land([c[0] + dir[0], c[1], c[2] + dir[1]]) {
                    go(l, Step::Walk);
                }
            }
            // Sauter : une case plus haut, jusqu'à 3 cases plus loin (2 pour monter).
            if !self.pass(up(c, 2)) {
                continue;
            }
            let mut at = up(c, 1);
            for dist in 1..=3 {
                let p = [at[0] + dir[0], at[1], at[2] + dir[1]];
                if !self.room(p) || !open(at) {
                    break;
                }
                at = p;
                match self.land(p) {
                    Some(l) if l[1] > c[1] && dist == 3 => {}
                    Some(l) => go(l, Step::Jump),
                    None => {}
                }
            }
        }
    }
}

fn up(c: Cell, d: i32) -> Cell {
    [c[0], c[1] + d, c[2]]
}

/// Monde enregistré de la carte `dir` (pour un relevé) : tous ses chunks enregistrés, et
/// ceux autour de ses zones (monde généré). (v322) Compté depuis l'origine `origin` d'une
/// partie (`map` aussi) : celle de la carte pour le relevé de la minimap
/// (`fpsmap::load_for_match`), celle de la partie pour les bots.
pub fn load_map_world(
    dir: &std::path::Path,
    map: &FpsMap,
    origin: minelib_world::ChunkPos,
) -> Option<World> {
    let (_, mut world) = crate::save::load_world(dir).ok()?;
    world.set_origin(origin);
    let bases: Vec<minelib_world::ChunkPos> = world.bases().map(|b| b.0).collect();
    let mut need: std::collections::HashSet<minelib_world::ChunkPos> =
        bases.into_iter().map(|b| world.local_chunk(b)).collect();
    for z in [&map.ct, &map.t]
        .into_iter()
        .chain(map.sites.iter())
        .flatten()
    {
        let c = |v: i32| v.div_euclid(16);
        for x in c(z.min[0]) - 6..=c(z.max[0]) + 6 {
            for cz in c(z.min[2]) - 6..=c(z.max[2]) + 6 {
                need.insert(minelib_world::ChunkPos { x, z: cz });
            }
        }
    }
    for c in need {
        world.ensure_chunk(c);
    }
    Some(world)
}

/// Fait (ou refait) le relevé de la carte `dir` en tâche de fond, s'il manque ou n'est
/// plus à jour, et l'enregistre avec elle. Un seul à la fois par carte.
pub fn survey_in_background(dir: &std::path::Path) {
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock};
    static BUSY: OnceLock<Mutex<HashSet<std::path::PathBuf>>> = OnceLock::new();
    let busy = BUSY.get_or_init(Default::default);
    if Survey::load(dir).is_some() || !busy.lock().unwrap().insert(dir.to_path_buf()) {
        return;
    }
    let job = dir.to_path_buf();
    let spawned = std::thread::Builder::new()
        .name("releve-carte".into())
        .spawn(move || {
            let dir = job;
            let t = std::time::Instant::now();
            let stamp = survey_stamp(&dir);
            // (v322) Compté depuis l'origine des parties de la carte (`Radar::set_origin`).
            let (map, origin) = crate::fpsmap::load_for_match(&dir);
            if let Some(world) = load_map_world(&dir, &map, origin) {
                let mut s = Survey::of(&world, &map);
                s.stamp = stamp;
                let cells: usize = s.floors.iter().map(Vec::len).sum();
                let ok = std::fs::write(dir.join(SURVEY_FILE), minelib_net::game::encode(&s));
                log::info!(
                    "relevé de la carte pour la minimap : {cells} cases atteignables depuis \
                     les départs, {} × {} blocs, en {:.1} s{}",
                    s.size[0],
                    s.size[1],
                    t.elapsed().as_secs_f32(),
                    if ok.is_ok() { "" } else { " (non enregistré)" }
                );
            }
            // (La carte reste « en cours » si le relevé n'a pas pu être écrit : pas de
            // nouvel essai en boucle.)
            if Survey::load(&dir).is_some() {
                busy.lock().unwrap().remove(&dir);
            }
        });
    if spawned.is_err() {
        busy.lock().unwrap().remove(dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use minelib_world::{BlockId, ChunkPos};

    /// Débogage : blocs d'une région d'une vraie carte, et nombre de barrières.
    #[test]
    #[ignore]
    fn dump_region() {
        let dir = std::path::PathBuf::from(std::env::var("MINELIB_SAVE").unwrap());
        let (_, mut world) = crate::save::load_world(&dir).unwrap();
        let all: Vec<_> = world.bases().map(|b| b.0).collect();
        for c in &all {
            world.ensure_chunk(*c);
        }
        let mut count = std::collections::HashMap::new();
        for c in &all {
            for y in 0..256 {
                for z in 0..16 {
                    for x in 0..16 {
                        let k = world.get(c.x * 16 + x, y, c.z * 16 + z);
                        let key = &k.def().key;
                        if key.contains("barrier") || key == "air" {
                            *count.entry(key.clone()).or_insert(0u64) += 1;
                        }
                    }
                }
            }
        }
        println!("{count:?}");
        for y in (150..=157).rev() {
            println!("y={y}");
            for z in 117..=124 {
                let row: Vec<String> = (-4..=5)
                    .map(|x| {
                        let k = world.get(x, y, z).def().key.clone();
                        format!("{:>10.10}", k)
                    })
                    .collect();
                println!("z{z} {}", row.join(" "));
            }
        }
    }

    /// Débogage : relevé d'une vraie carte (`MINELIB_SAVE`), écrit en image PGM.
    #[test]
    #[ignore]
    fn survey_of_a_real_map() {
        let dir = std::path::PathBuf::from(std::env::var("MINELIB_SAVE").unwrap());
        let _ = std::fs::remove_file(dir.join(SURVEY_FILE));
        let t = std::time::Instant::now();
        survey_in_background(&dir);
        let s = loop {
            std::thread::sleep(std::time::Duration::from_millis(200));
            if let Some(s) = Survey::load(&dir) {
                break s;
            }
            assert!(t.elapsed().as_secs() < 300);
        };
        let cells: usize = s.floors.iter().map(Vec::len).sum();
        println!(
            "{} cases, {:?} {:?}, {:.1} s",
            cells,
            s.min,
            s.size,
            t.elapsed().as_secs_f32()
        );
        let feet: i32 = std::env::var("FEET")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(150);
        let mut out = format!("P2\n{} {}\n5\n", s.size[0], s.size[1]);
        for z in 0..s.size[1] as i32 {
            for x in 0..s.size[0] as i32 {
                out += &format!("{} ", s.shade(s.min[0] + x, feet, s.min[1] + z));
            }
            out.push('\n');
        }
        std::fs::write("/tmp/claude-0/survey.pgm", out).unwrap();
    }

    #[test]
    fn the_survey_does_not_squeeze_between_two_blocks_touching_by_a_corner() {
        use minelib_net::game::Zone;
        let mut w = World::new(0);
        for dz in -1..=2 {
            for dx in -1..=2 {
                w.insert_empty(ChunkPos { x: dx, z: dz });
            }
        }
        let stone = BlockId::STONE;
        // Un grand sol, et une salle dont un mur fait une marche : z = 9 pour x ≤ 6,
        // z = 10 pour x ≥ 7. Les deux pans ne se touchent que par l'arête.
        for x in -8..=24 {
            for z in -8..=24 {
                w.set(x, 10, z, stone);
            }
        }
        for y in 11..17 {
            for x in 0..=13 {
                w.set(x, y, 0, stone);
                w.set(x, y, if x <= 6 { 9 } else { 10 }, stone);
            }
            for z in 0..=10 {
                w.set(0, y, z, stone);
                w.set(13, y, z, stone);
            }
        }
        let map = FpsMap {
            ct: Some(Zone::new([2, 11, 2], [4, 12, 4])),
            ..Default::default()
        };
        let s = Survey::of(&w, &map);
        let floors = |x: i32, z: i32| s.col(x, z).map(|c| s.floors[c].clone()).unwrap_or_default();
        assert_eq!(
            floors(7, 9),
            vec![11],
            "le renfoncement fait partie de la salle"
        );
        assert!(
            floors(6, 10).is_empty(),
            "passé en diagonale entre deux blocs"
        );
        assert!(
            floors(6, 14).is_empty() && floors(-4, 4).is_empty(),
            "dehors"
        );
    }

    #[test]
    fn the_survey_keeps_only_where_players_can_go_and_the_walls_around() {
        use minelib_net::game::Zone;
        let mut w = World::new(0);
        for dz in -1..=2 {
            for dx in -1..=2 {
                w.insert_empty(ChunkPos { x: dx, z: dz });
            }
        }
        let stone = BlockId::STONE;
        // Une salle de 12 × 8 fermée par des murs de 4 de haut (sol y = 10).
        for x in 0..=13 {
            for z in 0..=9 {
                w.set(x, 10, z, stone);
                if x == 0 || x == 13 || z == 0 || z == 9 {
                    for y in 11..15 {
                        w.set(x, y, z, stone);
                    }
                }
            }
        }
        // Une caisse d'un bloc (on saute dessus), une colonne de 2 (on n'y monte pas).
        w.set(4, 11, 4, stone);
        w.set(8, 11, 4, stone);
        w.set(8, 12, 4, stone);
        // Un décor hors d'atteinte, derrière le mur : il ne doit pas apparaître.
        for y in 11..14 {
            w.set(20, y, 4, stone);
        }
        w.set(20, 10, 4, stone);
        let zone = Zone::new([1, 11, 1], [3, 12, 3]);
        let map = FpsMap {
            ct: Some(zone),
            ..Default::default()
        };
        let s = Survey::of(&w, &map);
        let floors = |x: i32, z: i32| s.col(x, z).map(|c| s.floors[c].clone()).unwrap_or_default();
        let walls = |x: i32, z: i32| s.col(x, z).map(|c| s.walls[c].clone()).unwrap_or_default();
        // Tout le sol de la salle, le dessus de la caisse, pas celui de la colonne.
        assert_eq!(floors(6, 6), vec![11]);
        assert_eq!(floors(12, 8), vec![11]);
        assert_eq!(floors(4, 4), vec![12]);
        assert!(floors(8, 4).is_empty());
        // Les murs de la salle et la colonne sont des murs ; la caisse non (on y monte).
        assert_eq!(walls(0, 5), vec![11]);
        assert_eq!(walls(13, 5), vec![11]);
        assert_eq!(walls(8, 4), vec![11]);
        assert!(walls(4, 4).is_empty());
        // Rien hors de la salle : ni le dessus des murs, ni le décor derrière.
        assert!(floors(0, 5).is_empty());
        assert!(floors(20, 4).is_empty() && walls(20, 4).is_empty());
        assert!(s.col(20, 4).is_none());
        // Teintes de la minimap, pieds au sol de la salle (y = 11).
        assert_eq!(s.shade(6, 11, 6), 3, "sol");
        assert_eq!(s.shade(4, 11, 4), 4, "caisse, plus haute");
        assert_eq!(s.shade(0, 11, 5), 5, "mur");
        assert_eq!(s.shade(8, 11, 4), 5, "colonne");
        assert_eq!(s.shade(20, 11, 4), 0, "décor inutile");
        // Une porte dans le mur (un trou de 2 de haut) : le couloir derrière est relevé,
        // et une échelle mène à une plateforme haute.
        w.set(13, 11, 5, BlockId::AIR);
        w.set(13, 12, 5, BlockId::AIR);
        for x in 14..=18 {
            w.set(x, 10, 5, stone);
        }
        let ladder = minelib_world::registry::registry()
            .by_key("ladder:0")
            .or_else(|| minelib_world::registry::registry().by_key("ladder"));
        if let Some(l) = ladder {
            for y in 11..16 {
                w.set(18, y, 5, l);
                w.set(19, y, 5, stone);
            }
            w.set(19, 10, 5, stone);
        }
        let s = Survey::of(&w, &map);
        let floors = |x: i32, z: i32| s.col(x, z).map(|c| s.floors[c].clone()).unwrap_or_default();
        assert_eq!(floors(13, 5), vec![11]);
        assert_eq!(floors(16, 5), vec![11]);
        if ladder.is_some() {
            assert!(floors(19, 5).contains(&16), "{:?}", floors(19, 5));
        }
        // Sans zone de départ : rien.
        assert_eq!(Survey::of(&w, &FpsMap::default()), Survey::default());
    }

    #[test]
    fn floor_walls_and_void() {
        let mut w = World::new(0);
        for dz in -1..=1 {
            for dx in -1..=1 {
                w.insert_empty(ChunkPos { x: dx, z: dz });
            }
        }
        for z in -8..8 {
            for x in -8..8 {
                w.set(x, 10, z, BlockId::STONE);
            }
        }
        // Un mur de 3 de haut en x = 3.
        for y in 11..14 {
            w.set(3, y, 0, BlockId::STONE);
        }
        // Pieds dans la case y = 11 (debout sur y = 10).
        assert_eq!(shade(&w, 0, 11, 0), 3, "sol");
        assert_eq!(shade(&w, 3, 11, 0), 5, "mur");
        assert_eq!(shade(&w, 12, 11, 12), 0, "vide");
        let mut r = Radar::default();
        r.update(&w, Vec3::new(0.5, 11.0, 0.5));
        assert_eq!(r.cells.len(), SIDE * SIDE);
        let at = |r: &Radar, dx: i32, dz: i32| {
            r.cells[((dz + GRID) as usize) * SIDE + (dx + GRID) as usize]
        };
        assert_eq!(at(&r, 3, 0), 5);
        assert_eq!(at(&r, 0, 0), 3);
        // Le joueur s'éloigne : la grille est recentrée sur lui, ce qui était connu suit
        // (le mur est toujours au même endroit du monde).
        r.update(&w, Vec3::new(-30.5, 11.0, 0.5));
        assert_eq!(r.center, Some([-31, 11, 0]));
        assert_eq!(at(&r, 34, 0), 5, "le mur, déplacé avec la grille");
    }
}
