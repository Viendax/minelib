//! (v172) Bots du mode FPS en solo.
//!
//! En passant en mode FPS dans un monde solo, la partie se remplit de bots (« BOT Albert »,
//! « BOT Camille »…) : 5 contre 5 (ou le nombre de joueurs de la carte), la partie démarre
//! et chacun est placé à son départ. Les bots jouent comme des joueurs, sans tricher :
//!
//! - ils passent par l'hôte de la partie (`netgame::Host`) comme des invités : une entrée
//!   de déplacement par pas (avancer, se décaler, sauter, s'accroupir, regard), des tirs et
//!   des achats jugés par les mêmes règles que ceux des joueurs (cadence, arme portée,
//!   imprécision en courant, recul) ;
//! - ils ne voient que ce qu'un joueur verrait (devant eux, pas à travers les murs ni les
//!   fumées) et entendent les tirs et les pas de course ; ils visent avec un temps de
//!   réaction, tournent la vue à vitesse humaine, s'arrêtent pour tirer juste, tirent au
//!   coup par coup de loin et en rafale de près en contrôlant le recul ;
//! - ils trouvent leur chemin sur un relevé de la carte fait en tâche de fond (mêmes règles
//!   de déplacement que la minimap : marche, saut, chute, échelle, voir `radar::Reach`) ;
//! - CT : ils tiennent les sites (regard vers l'arrivée des T), tournent vers le site
//!   attaqué, reprennent le site et désamorcent ; T : ils avancent vers le site choisi
//!   (chacun par son chemin), le porteur pose la bombe derrière ses coéquipiers, puis ils
//!   la protègent ; tout le monde fuit l'explosion quand il est trop tard pour désamorcer.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, VecDeque};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::Arc;

use glam::Vec3;
use minelib_net::game::{
    bomb, phase, FpsMap, NetInput, Zone, CT, ITEM_HELMET, ITEM_KEVLAR, ITEM_KIT, T,
};
use minelib_world::raycast::raycast_with;
use minelib_world::{BlockId, World};

use crate::cs::Match;
use crate::player::{EYE, FPS_SPRINT};
use crate::radar::{Cell, Reach, Step};
use crate::weapons::{Category, WEAPONS};

/// Identifiants des bots (bien au-delà de ceux que donne le relais).
pub const BOT_BASE: u64 = 0x0B07_0000_0000_0000;

/// Prénoms des bots (pseudos « BOT <prénom> » : l'espace les distingue des vrais
/// joueurs, dont le pseudo n'en a jamais).
const NAMES: [&str; 20] = [
    "Albert",
    "Bérénice",
    "Camille",
    "Damien",
    "Elsa",
    "Félix",
    "Gaspard",
    "Hélène",
    "Igor",
    "Jeanne",
    "Karim",
    "Lina",
    "Marius",
    "Nina",
    "Oscar",
    "Pauline",
    "Quentin",
    "Rémi",
    "Sacha",
    "Théa",
];

/// Le bot voit jusqu'à 60° de chaque côté de son regard.
const VIEW_COS: f32 = 0.5;
/// Un regard sur les ennemis tous les 4 pas (64 par seconde), décalé d'un bot à l'autre.
const LOOK_EVERY: u32 = 4;
/// Une décision (où aller) tous les 32 pas (8 par seconde).
const DECIDE_EVERY: u32 = 32;
/// Portées d'écoute (blocs) : tirs, pas de course.
const HEAR_SHOT: f32 = 70.0;
const HEAR_STEP: f32 = 28.0;
/// Ennemi perdu de vue : sa dernière position reste en tête (s).
const MEMORY_S: f32 = 5.0;
/// Distance à la bombe où son explosion ne fait presque plus rien (moins de 20 points).
const BOMB_SAFE: f32 = 38.0;
/// Cases relevées au plus (carte ouverte sur un terrain sans fin : environ 40 Mo).
const NAV_MAX: usize = 400_000;
/// Cases explorées au plus par une recherche de chemin.
const SEARCH_MAX: usize = 400_000;
/// En deçà : on arrose (rafale contrôlée) ; au-delà de `TAP_DIST` : coup par coup.
const SPRAY_DIST: f32 = 12.0;
const TAP_DIST: f32 = 30.0;
/// Portée des armes (blocs), comme pour les joueurs.
const RANGE: f32 = 250.0;

// ---------------------------------------------------------------------------------------
// Relevé de la carte et chemins
// ---------------------------------------------------------------------------------------

/// Relevé de la carte pour les bots : les cases où l'on peut se tenir (celle des pieds),
/// reliées par les déplacements possibles (`radar::Reach` : marche, chute, saut, échelle).
pub struct Nav {
    cells: Vec<Cell>,
    index: HashMap<Cell, u32>,
    first: Vec<u32>,
    links: Vec<Link>,
    /// Plus courts chemins depuis le départ des T, et depuis celui des CT : par où l'on
    /// arrive de chez eux en chaque case.
    from_t: Field,
    from_ct: Field,
    /// Cases de chaque site de bombe (zone élargie de 3 blocs).
    sites: [Vec<u32>; 3],
}

/// Un déplacement possible vers une case voisine (son coût se recalcule : `Nav::cost`).
#[derive(Clone, Copy)]
struct Link {
    to: u32,
    step: Step,
}

/// Plus courts chemins depuis une source : coût depuis la source et case d'avant sur le
/// chemin, pour chaque case (`any` : la source existe).
#[derive(Default)]
struct Field {
    cost: Vec<f32>,
    prev: Vec<u32>,
    any: bool,
}

impl Field {
    /// On arrive en `n` depuis la source (vrai aussi sans source : on ne sait pas).
    fn reaches(&self, n: u32) -> bool {
        !self.any || self.cost.get(n as usize).is_some_and(|c| c.is_finite())
    }
}

/// Centre d'une case, à hauteur des pieds.
fn center(c: Cell) -> Vec3 {
    Vec3::new(c[0] as f32 + 0.5, c[1] as f32, c[2] as f32 + 0.5)
}

fn flat(v: Vec3) -> Vec3 {
    Vec3::new(v.x, 0.0, v.z)
}

fn in_zone(c: Cell, z: &Zone, grow: i32) -> bool {
    (z.min[0] - grow..=z.max[0] + grow).contains(&c[0])
        && (z.min[2] - grow..=z.max[2] + grow).contains(&c[2])
        && (z.min[1] - 2..=z.max[1] + 3).contains(&c[1])
}

/// Coût d'un déplacement : sa longueur, plus un peu pour les sauts, les échelles et les
/// grandes chutes (plus lents ou plus risqués).
fn link_cost(a: Cell, b: Cell, step: Step) -> f32 {
    let dx = (b[0] - a[0]) as f32;
    let dz = (b[2] - a[2]) as f32;
    let dy = (b[1] - a[1]) as f32;
    let flat = (dx * dx + dz * dz).sqrt();
    match step {
        Step::Walk => flat + if dy < -1.5 { -0.3 * dy } else { 0.0 },
        // Monter en diagonale (par-dessus un coin), ou monter en sautant loin, se rate
        // souvent : seulement s'il n'y a pas mieux (une marche, puis un petit saut).
        Step::Jump if dy > 0.0 && dx != 0.0 && dz != 0.0 => flat + 5.0,
        Step::Jump if dy > 0.0 && flat > 1.5 => flat + 4.0,
        Step::Jump => flat + 1.2,
        Step::Ladder => flat + 1.5,
    }
}

impl Nav {
    /// Relève la carte depuis ses zones de départ et ses sites (et `starts`, pour une
    /// carte sans zones).
    pub fn build(world: &World, map: &FpsMap, starts: &[Vec3]) -> Self {
        let r = Reach::new(world);
        let mut index: HashMap<Cell, u32> = HashMap::new();
        let mut cells: Vec<Cell> = Vec::new();
        let mut queue: VecDeque<Cell> = VecDeque::new();
        let mut add = |c: Cell, index: &mut HashMap<Cell, u32>, cells: &mut Vec<Cell>| {
            if let std::collections::hash_map::Entry::Vacant(e) = index.entry(c) {
                e.insert(cells.len() as u32);
                cells.push(c);
                queue.push_back(c);
            }
        };
        for zone in [&map.ct, &map.t]
            .into_iter()
            .chain(map.sites.iter())
            .flatten()
        {
            for x in zone.min[0]..=zone.max[0] {
                for z in zone.min[2]..=zone.max[2] {
                    for y in zone.min[1] - 1..=zone.max[1] + 1 {
                        let c = [x, y, z];
                        if r.stand(c) {
                            add(c, &mut index, &mut cells);
                        }
                    }
                }
            }
        }
        for p in starts {
            let c = [
                p.x.floor() as i32,
                (p.y + 0.05).floor() as i32,
                p.z.floor() as i32,
            ];
            if let Some(l) = r.land(c) {
                add(l, &mut index, &mut cells);
            }
        }
        while let Some(c) = queue.pop_front() {
            if cells.len() >= NAV_MAX {
                break;
            }
            r.neighbors(c, |n, _| {
                if let std::collections::hash_map::Entry::Vacant(e) = index.entry(n) {
                    e.insert(cells.len() as u32);
                    cells.push(n);
                    queue.push_back(n);
                }
            });
        }
        let mut first = Vec::with_capacity(cells.len() + 1);
        let mut links = Vec::new();
        for &c in &cells {
            first.push(links.len() as u32);
            r.neighbors(c, |n, step| {
                if let Some(&to) = index.get(&n) {
                    links.push(Link { to, step });
                }
            });
        }
        first.push(links.len() as u32);
        let mut nav = Self {
            cells,
            index,
            first,
            links,
            from_t: Field::default(),
            from_ct: Field::default(),
            sites: Default::default(),
        };
        nav.from_t = nav.field(map.t.as_ref());
        nav.from_ct = nav.field(map.ct.as_ref());
        for (i, s) in map.sites.iter().enumerate() {
            if let Some(z) = s {
                nav.sites[i] = (0..nav.cells.len() as u32)
                    .filter(|&n| in_zone(nav.cells[n as usize], z, 3))
                    .collect();
            }
        }
        nav
    }

    pub fn len(&self) -> usize {
        self.cells.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    /// Essais : ce que couvre le relevé (étendue, cases de chaque site, chemins des
    /// départs vers les sites).
    #[cfg(test)]
    pub fn report(&self, map: &FpsMap) -> String {
        use std::fmt::Write;
        let mut out = String::new();
        let (mut lo, mut hi) = ([i32::MAX; 3], [i32::MIN; 3]);
        for c in &self.cells {
            for k in 0..3 {
                lo[k] = lo[k].min(c[k]);
                hi[k] = hi[k].max(c[k]);
            }
        }
        let _ = writeln!(out, "  {} cases, de {lo:?} à {hi:?}", self.cells.len());
        let spawn = |z: Option<Zone>| {
            z.and_then(|z| {
                (0..self.cells.len() as u32).find(|&n| in_zone(self.cells[n as usize], &z, 0))
            })
        };
        let (t, ct) = (spawn(map.t), spawn(map.ct));
        let _ = writeln!(out, "  départ T : {t:?}, départ CT : {ct:?}");
        for (i, pool) in self.sites.iter().enumerate() {
            if map.sites[i].is_none() {
                continue;
            }
            let site = pool.first().copied();
            let reach = |from: Option<u32>| match (from, site) {
                (Some(a), Some(b)) => self.path_steps(a, b).map(|p| p.len()),
                _ => None,
            };
            let _ = writeln!(
                out,
                "  site {} : {} cases ; depuis T {:?} pas, depuis CT {:?} pas",
                i,
                pool.len(),
                reach(t),
                reach(ct)
            );
        }
        out
    }

    /// Essais : comment on passe d'une case à l'autre sur le plus court chemin.
    #[cfg(test)]
    pub fn path_steps(&self, from: u32, to: u32) -> Option<Vec<Step>> {
        let mut search = Search::default();
        let p = search.path(self, from, To::Node(to), &|i, l: &Link| self.cost(i, l))?;
        Some(p.iter().skip(1).map(|x| x.1).collect())
    }

    /// Coût du lien `l` qui part de la case `from`.
    fn cost(&self, from: u32, l: &Link) -> f32 {
        link_cost(self.cells[from as usize], self.cells[l.to as usize], l.step)
    }

    fn links(&self, i: u32) -> &[Link] {
        let a = self.first[i as usize] as usize;
        let b = self.first[i as usize + 1] as usize;
        &self.links[a..b]
    }

    pub fn center(&self, i: u32) -> Vec3 {
        center(self.cells[i as usize])
    }

    /// Case du relevé où se tient (ou presque) un joueur dont les pieds sont en `p`.
    pub fn near(&self, p: Vec3) -> Option<u32> {
        let base = [
            p.x.floor() as i32,
            (p.y + 0.05).floor() as i32,
            p.z.floor() as i32,
        ];
        if let Some(&i) = self.index.get(&base) {
            return Some(i);
        }
        let mut best: Option<(f32, u32)> = None;
        for dy in [0, -1, 1, -2, 2] {
            for dz in -1..=1 {
                for dx in -1..=1 {
                    let c = [base[0] + dx, base[1] + dy, base[2] + dz];
                    if let Some(&i) = self.index.get(&c) {
                        let d = center(c).distance(p);
                        if best.is_none_or(|b| d < b.0) {
                            best = Some((d, i));
                        }
                    }
                }
            }
            if best.is_some() {
                break;
            }
        }
        best.map(|b| b.1)
    }

    /// Plus courts chemins depuis une zone (toutes ses cases à la fois).
    fn field(&self, zone: Option<&Zone>) -> Field {
        let n = self.cells.len();
        let mut cost = vec![f32::INFINITY; n];
        let mut prev = vec![u32::MAX; n];
        let mut heap: BinaryHeap<Reverse<(u32, u32)>> = BinaryHeap::new();
        let mut any = false;
        if let Some(z) = zone {
            for (i, &c) in self.cells.iter().enumerate() {
                if in_zone(c, z, 0) {
                    cost[i] = 0.0;
                    any = true;
                    heap.push(Reverse((0f32.to_bits(), i as u32)));
                }
            }
        }
        while let Some(Reverse((k, i))) = heap.pop() {
            let c = f32::from_bits(k);
            if c > cost[i as usize] {
                continue;
            }
            for l in self.links(i) {
                let nc = c + self.cost(i, l);
                if nc < cost[l.to as usize] {
                    cost[l.to as usize] = nc;
                    prev[l.to as usize] = i;
                    heap.push(Reverse((nc.to_bits(), l.to)));
                }
            }
        }
        Field { cost, prev, any }
    }

    /// Case `k` pas en arrière sur le chemin qui vient de la source du champ `f` (par où
    /// arrivent ceux qui en partent).
    fn back(&self, f: &Field, mut i: u32, k: usize) -> u32 {
        for _ in 0..k {
            match f.prev.get(i as usize) {
                Some(&p) if p != u32::MAX => i = p,
                _ => break,
            }
        }
        i
    }
}

/// But d'une recherche de chemin.
enum To<'a> {
    Node(u32),
    Where(&'a dyn Fn(u32) -> bool),
}

/// Mémoire de travail des recherches de chemin (réutilisée d'une recherche à l'autre).
#[derive(Default)]
struct Search {
    g: Vec<f32>,
    from: Vec<(u32, Step)>,
    mark: Vec<u32>,
    gen: u32,
    heap: BinaryHeap<Reverse<(u32, u32)>>,
}

impl Search {
    /// Chemin le moins coûteux de `start` au but (A* vers une case, sinon Dijkstra),
    /// `cost` donnant le coût de chaque lien emprunté. Le chemin commence par `start`.
    fn path(
        &mut self,
        nav: &Nav,
        start: u32,
        goal: To,
        cost: &dyn Fn(u32, &Link) -> f32,
    ) -> Option<Vec<(u32, Step)>> {
        let n = nav.cells.len();
        if self.mark.len() != n {
            self.g = vec![0.0; n];
            self.from = vec![(u32::MAX, Step::Walk); n];
            self.mark = vec![0; n];
            self.gen = 0;
        }
        self.gen = self.gen.wrapping_add(1);
        if self.gen == 0 {
            self.mark.fill(0);
            self.gen = 1;
        }
        let gen = self.gen;
        self.heap.clear();
        let target = match goal {
            To::Node(t) => Some(t),
            To::Where(_) => None,
        };
        let h = |i: u32| target.map_or(0.0, |t| flat(nav.center(i) - nav.center(t)).length());
        let s = start as usize;
        self.mark[s] = gen;
        self.g[s] = 0.0;
        self.from[s] = (u32::MAX, Step::Walk);
        self.heap.push(Reverse((h(start).to_bits(), start)));
        let mut found = None;
        let mut pops = 0;
        while let Some(Reverse((key, i))) = self.heap.pop() {
            let gi = self.g[i as usize];
            if f32::from_bits(key) > gi + h(i) + 1e-3 {
                continue;
            }
            let reached = match goal {
                To::Node(t) => i == t,
                To::Where(f) => f(i),
            };
            if reached {
                found = Some(i);
                break;
            }
            pops += 1;
            if pops > SEARCH_MAX {
                break;
            }
            for l in nav.links(i) {
                let ng = gi + cost(i, l);
                let j = l.to as usize;
                if self.mark[j] != gen || ng < self.g[j] {
                    self.mark[j] = gen;
                    self.g[j] = ng;
                    self.from[j] = (i, l.step);
                    self.heap.push(Reverse(((ng + h(l.to)).to_bits(), l.to)));
                }
            }
        }
        let end = found?;
        let mut out = vec![(end, Step::Walk)];
        let mut at = end;
        while at != start {
            let (p, step) = self.from[at as usize];
            if let Some(last) = out.last_mut() {
                last.1 = step;
            }
            out.push((p, Step::Walk));
            at = p;
        }
        out.reverse();
        Some(out)
    }
}

/// Relève la carte d'un monde enregistré (`dir`) en tâche de fond, pour les bots.
pub fn nav_in_background(dir: std::path::PathBuf, map: FpsMap, starts: Vec<Vec3>) -> Receiver<Nav> {
    let (tx, rx) = std::sync::mpsc::channel();
    let _ = std::thread::Builder::new()
        .name("releve-bots".into())
        .spawn(move || {
            let t = std::time::Instant::now();
            let Some(world) = crate::radar::load_map_world(&dir, &map) else {
                log::warn!("bots : carte illisible, ils resteront sur place");
                return;
            };
            let nav = Nav::build(&world, &map, &starts);
            if nav.is_empty() {
                log::warn!("bots : aucune case où se tenir aux départs, ils resteront sur place");
            }
            log::info!(
                "bots : relevé de la carte, {} cases en {:.1} s",
                nav.len(),
                t.elapsed().as_secs_f32()
            );
            let _ = tx.send(nav);
        });
    rx
}

// ---------------------------------------------------------------------------------------
// Ce que l'hôte montre aux bots, et ce qu'ils lui demandent
// ---------------------------------------------------------------------------------------

/// Un joueur de la partie, tel que l'hôte le connaît à ce pas.
#[derive(Clone, Copy, Debug)]
pub struct Body {
    pub id: u64,
    pub team: u8,
    pub alive: bool,
    pub feet: Vec3,
    pub vel: Vec3,
    pub crouch: f32,
    pub on_ground: bool,
    pub on_ladder: bool,
    pub yaw: f32,
    /// Vitesse permise par ce qu'il tient (unités de CS).
    pub carry: u8,
}

impl Body {
    fn eye(&self) -> Vec3 {
        self.feet + Vec3::Y * (EYE - 0.3 * self.crouch)
    }

    /// Milieu de la tête (zone de la tête : les 0,3 bloc du haut de la boîte).
    fn head(&self) -> Vec3 {
        self.feet + Vec3::Y * (1.65 - 0.3 * self.crouch)
    }

    fn chest(&self) -> Vec3 {
        self.feet + Vec3::Y * (1.15 - 0.15 * self.crouch)
    }

    fn speed(&self) -> f32 {
        flat(self.vel).length()
    }
}

/// La partie vue par les bots à un pas.
pub struct Scene<'a> {
    pub tick: u32,
    pub m: &'a Match,
    pub bodies: Vec<Body>,
    /// Fumées en cours (centre, rayon).
    pub smokes: &'a [(Vec3, f32)],
    /// Dernier tir de chaque joueur (pas).
    pub last_shot: &'a HashMap<u64, u32>,
}

impl Scene<'_> {
    fn body(&self, id: u64) -> Option<&Body> {
        self.bodies.iter().find(|b| b.id == id)
    }
}

/// Ce qu'un bot demande à l'hôte (comme un invité le lui enverrait).
#[derive(Debug)]
pub enum Act {
    /// Entrées d'un pas.
    Input(u64, NetInput),
    Fire {
        bot: u64,
        weapon: u8,
        origin: Vec3,
        dirs: Vec<Vec3>,
    },
    /// Touches poser / désamorcer maintenues.
    Keys {
        bot: u64,
        plant: bool,
        defuse: bool,
    },
    Buy {
        bot: u64,
        item: u16,
    },
    /// Arme en main (vue par les autres).
    Hold {
        bot: u64,
        weapon: u8,
    },
    /// Ouvrir (ou fermer) la porte de cette case, poussée de ce côté (`netgame::use_door`).
    Door {
        bot: u64,
        pos: [i32; 3],
        side: u8,
    },
}

// ---------------------------------------------------------------------------------------
// Les bots
// ---------------------------------------------------------------------------------------

/// Ennemi vu ou entendu par un bot, connu de toute son équipe (comme sur la minimap).
#[derive(Clone, Copy)]
struct Intel {
    id: u64,
    pos: Vec3,
    t: f32,
}

/// Plan de la manche.
#[derive(Default)]
struct Plan {
    /// Site attaqué par les T.
    site: Option<usize>,
    /// Site gardé par chaque CT.
    guard: HashMap<u64, usize>,
    /// Case visée par chaque bot (pour ne pas s'y empiler).
    spots: HashMap<u64, u32>,
    /// Case où le porteur pose la bombe.
    plant: Option<u32>,
    /// Tireur d'élite de chaque équipe (s'il a de quoi acheter l'AWP).
    awper: [Option<u64>; 2],
    /// Début de la manche (fin du temps d'achat) et du temps d'achat.
    live_at: f32,
    freeze_at: f32,
}

fn team_index(team: u8) -> usize {
    (team == T) as usize
}

pub struct Bots {
    nav: Option<Arc<Nav>>,
    building: Option<Receiver<Nav>>,
    brains: Vec<Brain>,
    rng: u32,
    time: f32,
    phase: u8,
    played: u32,
    intel: [Vec<Intel>; 2],
    plan: Plan,
    search: Search,
    /// Essais : bots qui ne voient ni n'entendent personne (ils ne font que l'objectif).
    calm: bool,
}

/// Ce que les bots partagent pendant qu'un d'eux réfléchit.
struct Ctx<'a> {
    nav: Option<&'a Nav>,
    intel: &'a mut [Vec<Intel>; 2],
    plan: &'a mut Plan,
    rng: &'a mut u32,
    search: &'a mut Search,
    time: f32,
    slot: u32,
    calm: bool,
}

impl Ctx<'_> {
    fn rand(&mut self) -> u32 {
        xorshift(self.rng)
    }

    /// Hasard entre 0 et 1.
    fn unit(&mut self) -> f32 {
        (self.rand() >> 8) as f32 / (1u32 << 24) as f32
    }
}

fn xorshift(s: &mut u32) -> u32 {
    *s ^= *s << 13;
    *s ^= *s >> 17;
    *s ^= *s << 5;
    *s
}

impl Bots {
    pub fn new(seed: u32) -> Self {
        Self {
            nav: None,
            building: None,
            brains: Vec::new(),
            rng: seed | 1,
            time: 0.0,
            phase: u8::MAX,
            played: u32::MAX,
            intel: Default::default(),
            plan: Plan::default(),
            search: Search::default(),
            calm: false,
        }
    }

    /// Essais : bots qui ne se battent pas (seulement l'objectif).
    #[cfg(test)]
    pub fn set_calm(&mut self, calm: bool) {
        self.calm = calm;
    }

    /// Essais : où en est chaque bot (but, chemin, arme).
    #[cfg(test)]
    pub fn debug(&self) -> Vec<String> {
        self.brains
            .iter()
            .map(|b| {
                let next = self.nav.as_ref().and_then(|nav| {
                    let (n, step) = *b.path.get(b.at)?;
                    let prev = b.path.get(b.at.checked_sub(1)?)?.0;
                    Some((nav.cells[prev as usize], nav.cells[n as usize], step))
                });
                format!(
                    "{:x} {:?} chemin {}/{} arme {} cible {:?} coincé {} vers {:?}",
                    b.id & 0xff,
                    b.goal,
                    b.at,
                    b.path.len(),
                    b.weapon,
                    b.target.map(|t| t & 0xff),
                    b.stuck_n,
                    next
                )
            })
            .collect()
    }

    /// Essais : fois où un bot est resté coincé (passage évité ensuite).
    #[cfg(test)]
    pub fn stuck_events(&self) -> u32 {
        self.brains.iter().map(|b| b.stuck_events).sum()
    }

    /// Pseudos de `n` bots (« BOT <prénom> », tous différents).
    pub fn names(n: usize, seed: u32) -> Vec<String> {
        let mut order: Vec<usize> = (0..NAMES.len()).collect();
        let mut s = seed | 1;
        for i in (1..order.len()).rev() {
            let j = xorshift(&mut s) as usize % (i + 1);
            order.swap(i, j);
        }
        (0..n)
            .map(|i| {
                let name = NAMES[order[i % NAMES.len()]];
                if i < NAMES.len() {
                    format!("BOT {name}")
                } else {
                    format!("BOT {name} {}", i / NAMES.len() + 1)
                }
            })
            .collect()
    }

    /// Un bot de plus (identifiant donné par l'hôte).
    pub fn add(&mut self, id: u64) {
        let seed = xorshift(&mut self.rng);
        // De bons joueurs, pas tous égaux.
        let skill = 0.72 + (seed % 1000) as f32 / 1000.0 * 0.23;
        self.brains.push(Brain::new(id, skill, seed));
    }

    /// Relevé de la carte en cours de préparation (voir `nav_in_background`).
    pub fn set_building(&mut self, rx: Receiver<Nav>) {
        self.building = Some(rx);
    }

    pub fn set_nav(&mut self, nav: Nav) {
        self.nav = Some(Arc::new(nav));
        self.building = None;
    }

    /// Un pas de la partie : ce que chaque bot fait.
    pub fn think(&mut self, s: &Scene, world: &World, dt: f32) -> Vec<Act> {
        self.time += dt;
        if let Some(rx) = &self.building {
            match rx.try_recv() {
                Ok(nav) => self.set_nav(nav),
                Err(TryRecvError::Disconnected) => self.building = None,
                Err(TryRecvError::Empty) => {}
            }
        }
        let ph = s.m.phase;
        if ph != self.phase || (ph == phase::FREEZE && s.m.played != self.played) {
            self.phase_changed(ph, s);
            self.phase = ph;
            self.played = s.m.played;
        }
        let now = self.time;
        for v in &mut self.intel {
            v.retain(|i| now - i.t < MEMORY_S * 2.0);
        }
        let nav = self.nav.clone();
        let mut acts = Vec::new();
        let mut brains = std::mem::take(&mut self.brains);
        for (k, b) in brains.iter_mut().enumerate() {
            let Some(me) = s.body(b.id).copied() else {
                continue;
            };
            let mut cx = Ctx {
                nav: nav.as_deref(),
                intel: &mut self.intel,
                plan: &mut self.plan,
                rng: &mut self.rng,
                search: &mut self.search,
                time: now,
                slot: k as u32,
                calm: self.calm,
            };
            b.step(&mut cx, s, &me, world, dt, &mut acts);
        }
        self.brains = brains;
        acts
    }

    /// Nouvelle phase de la partie : nouveau plan à chaque manche.
    fn phase_changed(&mut self, ph: u8, s: &Scene) {
        let now = self.time;
        match ph {
            phase::FREEZE | phase::WARMUP => {
                let sites: Vec<usize> = (0..3).filter(|&i| s.m.map.sites[i].is_some()).collect();
                let pick = xorshift(&mut self.rng) as usize;
                let mut plan = Plan {
                    site: (!sites.is_empty()).then(|| sites[pick % sites.len()]),
                    freeze_at: now,
                    live_at: now + s.m.time.max(0.0),
                    ..Default::default()
                };
                // CT : répartis entre les sites (le plus nombreux change d'une manche à
                // l'autre).
                let mut cts: Vec<u64> = self
                    .brains
                    .iter()
                    .map(|b| b.id)
                    .filter(|id| s.m.players.get(id).is_some_and(|p| p.team == CT))
                    .collect();
                for i in (1..cts.len()).rev() {
                    let j = xorshift(&mut self.rng) as usize % (i + 1);
                    cts.swap(i, j);
                }
                if !sites.is_empty() {
                    let shift = xorshift(&mut self.rng) as usize;
                    for (i, id) in cts.iter().enumerate() {
                        plan.guard.insert(*id, sites[(i + shift) % sites.len()]);
                    }
                }
                // Un tireur d'élite par équipe (le plus adroit qui a de quoi).
                for team in [CT, T] {
                    plan.awper[team_index(team)] = self
                        .brains
                        .iter()
                        .filter(|b| {
                            s.m.players
                                .get(&b.id)
                                .is_some_and(|p| p.team == team && p.money >= 5_600)
                        })
                        .max_by(|a, b| a.skill.total_cmp(&b.skill))
                        .map(|b| b.id);
                }
                self.plan = plan;
                self.intel = Default::default();
                for b in &mut self.brains {
                    let r = xorshift(&mut self.rng);
                    b.new_round(r);
                }
            }
            phase::LIVE => self.plan.live_at = now,
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------------------
// Un bot
// ---------------------------------------------------------------------------------------

/// Où va un bot.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Goal {
    /// Rester là (regard sur `look`).
    Wait,
    /// Aller à une case et y tenir l'angle.
    Go(u32),
    /// Porteur : aller poser la bombe sur cette case.
    Plant(u32),
    /// CT : désamorcer la bombe (on est dessus).
    Defuse,
    /// Fuir l'explosion vers cette case.
    Flee(u32),
}

struct Brain {
    id: u64,
    skill: f32,
    seed: u32,
    alive: bool,
    last_feet: Vec3,
    // Chemin.
    path: Vec<(u32, Step)>,
    at: usize,
    dest: Option<u32>,
    /// Pas de chemin trouvé : on ne recherche pas avant ce moment (s) ; détour d'un pas au
    /// hasard (coincé) : on ne recherche pas avant sa fin.
    retry_at: f32,
    detour_until: f32,
    stuck_t: f32,
    /// Case du chemin visée au dernier contrôle (pas de progrès : coincé).
    stuck_at: usize,
    stuck_n: u32,
    /// Liens où il est resté coincé (évités ensuite), et combien de fois c'est arrivé.
    blocked: Vec<(u32, u32)>,
    stuck_events: u32,
    jump_cd: f32,
    unstick: f32,
    unstick_dir: i8,
    /// Porte devant lui à ouvrir (coincé contre elle).
    door: Option<[i32; 3]>,
    door_cd: f32,
    /// Recul pour reprendre de l'élan avant un saut (s).
    backoff: f32,
    // But.
    goal: Goal,
    look: Option<Vec3>,
    look_t: f32,
    /// T : temps à attendre au départ (tous ne partent pas en même temps).
    delay: f32,
    // Combat.
    target: Option<u64>,
    react: f32,
    seen: HashMap<u64, (Vec3, f32)>,
    heard: Option<(Vec3, f32)>,
    head_aim: bool,
    aim_err: (f32, f32),
    yaw: f32,
    pitch: f32,
    // Arme.
    weapon: u8,
    deploy: f32,
    cooldown: f32,
    since_shot: f32,
    spray_n: u32,
    burst: u32,
    ammo: HashMap<u8, (u32, u32)>,
    reload: f32,
    scoped: bool,
    still_t: f32,
    // Déplacements en combat.
    strafe: i8,
    strafe_t: f32,
    duck_fight: bool,
    keys: (bool, bool),
    // Achats.
    bought: bool,
    buy_at: f32,
}

/// Vue (lacet, tangage) qui regarde dans la direction `d`.
fn angles(d: Vec3) -> (f32, f32) {
    let yaw = d.x.atan2(-d.z).rem_euclid(std::f32::consts::TAU);
    let pitch = d.y.atan2(flat(d).length()).clamp(-1.5, 1.5);
    (yaw, pitch)
}

fn look_dir(yaw: f32, pitch: f32) -> Vec3 {
    let (sy, cy) = yaw.sin_cos();
    let (sp, cp) = pitch.sin_cos();
    Vec3::new(sy * cp, sp, -cy * cp)
}

/// Écart d'angle ramené entre −π et π.
fn wrap(a: f32) -> f32 {
    (a + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI
}

/// Touches (avancer, se décaler) pour aller dans la direction `dir` en regardant vers
/// `yaw` (huit directions, comme au clavier).
fn keys_for(dir: Vec3, yaw: f32) -> (i8, i8) {
    let d = flat(dir);
    if d.length_squared() < 1e-6 {
        return (0, 0);
    }
    let (sy, cy) = yaw.sin_cos();
    let f = d.x * sy - d.z * cy;
    let r = d.x * cy + d.z * sy;
    let sector = (r.atan2(f) / std::f32::consts::FRAC_PI_4).round() as i32;
    match sector.rem_euclid(8) {
        0 => (1, 0),
        1 => (1, 1),
        2 => (0, 1),
        3 => (-1, 1),
        4 => (-1, 0),
        5 => (-1, -1),
        6 => (0, -1),
        _ => (1, -1),
    }
}

/// Le bloc cache la vue (pas les vitres, les barrières invisibles, les échelles).
fn blocks_sight(id: BlockId) -> bool {
    let d = id.def();
    d.solid
        && !matches!(
            d.shape,
            minelib_world::registry::Shape::Invisible
                | minelib_world::registry::Shape::Panel { thick: 1, .. }
        )
        && !d.key.contains("glass")
}

/// On voit `to` depuis `from` (aucun bloc opaque entre les deux).
fn clear_sight(world: &World, from: Vec3, to: Vec3) -> bool {
    let d = to - from;
    let len = d.length();
    if len < 1e-3 {
        return true;
    }
    raycast_with(world, from.into(), (d / len).into(), len, blocks_sight)
        .is_none_or(|h| h.distance >= len - 0.05)
}

/// Le segment passe dans une fumée.
fn smoked(smokes: &[(Vec3, f32)], a: Vec3, b: Vec3) -> bool {
    smokes.iter().any(|&(c, r)| {
        let ab = b - a;
        let t = ((c - a).dot(ab) / ab.length_squared().max(1e-6)).clamp(0.0, 1.0);
        (a + ab * t).distance(c) < r * 0.9
    })
}

/// Indice d'une arme par son nom.
fn gun(name: &str) -> u8 {
    WEAPONS
        .iter()
        .position(|w| w.name == name)
        .map_or(255, |i| i as u8)
}

/// Porte (ou trappe) juste devant les pieds ou la tête, dans la direction `dir`.
fn door_ahead(world: &World, feet: Vec3, dir: Vec3) -> Option<[i32; 3]> {
    for d in [0.5, 1.0] {
        for dy in [0.2, 1.2] {
            let p = feet + dir * d + Vec3::Y * dy;
            let c = [p.x.floor() as i32, p.y.floor() as i32, p.z.floor() as i32];
            if crate::session::hinged(world.get(c[0], c[1], c[2])).is_some() {
                return Some(c);
            }
        }
    }
    None
}

/// Côté (0 +X, 1 −X, 4 +Z, 5 −Z) vers lequel on pousse en allant dans la direction `v`.
fn side_toward(v: Vec3) -> u8 {
    if v.x.abs() > v.z.abs() {
        if v.x > 0.0 {
            0
        } else {
            1
        }
    } else if v.z > 0.0 {
        4
    } else {
        5
    }
}

/// Côté du mur d'une échelle (direction vers le mur), si `c` en est une.
fn ladder_wall(world: &World, c: Cell) -> Option<Vec3> {
    let key = &world.get(c[0], c[1], c[2]).def().key;
    let side: u8 = key.strip_prefix("ladder:")?.parse().ok()?;
    Some(match side {
        0 => Vec3::X,
        1 => -Vec3::X,
        4 => Vec3::Z,
        _ => -Vec3::Z,
    })
}

impl Brain {
    fn new(id: u64, skill: f32, seed: u32) -> Self {
        Self {
            id,
            skill,
            seed,
            alive: false,
            last_feet: Vec3::ZERO,
            path: Vec::new(),
            at: 0,
            dest: None,
            retry_at: 0.0,
            detour_until: 0.0,
            stuck_t: 0.0,
            stuck_at: usize::MAX,
            stuck_n: 0,
            blocked: Vec::new(),
            stuck_events: 0,
            jump_cd: 0.0,
            unstick: 0.0,
            unstick_dir: 1,
            door: None,
            door_cd: 0.0,
            backoff: 0.0,
            goal: Goal::Wait,
            look: None,
            look_t: 0.0,
            delay: 0.0,
            target: None,
            react: 0.0,
            seen: HashMap::new(),
            heard: None,
            head_aim: true,
            aim_err: (0.0, 0.0),
            yaw: 0.0,
            pitch: 0.0,
            weapon: 255,
            deploy: 0.0,
            cooldown: 0.0,
            since_shot: 10.0,
            spray_n: 0,
            burst: 0,
            ammo: HashMap::new(),
            reload: 0.0,
            scoped: false,
            still_t: 0.0,
            strafe: 1,
            strafe_t: 0.0,
            duck_fight: false,
            keys: (false, false),
            bought: false,
            buy_at: 1.0,
        }
    }

    fn new_round(&mut self, r: u32) {
        self.path.clear();
        self.at = 0;
        self.dest = None;
        self.goal = Goal::Wait;
        self.look = None;
        self.target = None;
        self.seen.clear();
        self.heard = None;
        self.blocked.clear();
        self.stuck_n = 0;
        // Munitions refaites à chaque manche (comme dans CS).
        self.ammo.clear();
        self.reload = 0.0;
        self.bought = false;
        self.buy_at = 0.6 + (r % 1000) as f32 / 1000.0 * 2.8;
        // T : certains partent tout de suite, d'autres un peu après.
        self.delay = ((r >> 10) % 1000) as f32 / 1000.0 * 7.0;
        self.scoped = false;
    }

    /// Mort, ou partie finie : on oublie tout ce qui tenait à cette vie.
    fn clear_life(&mut self) {
        self.path.clear();
        self.dest = None;
        self.target = None;
        self.goal = Goal::Wait;
        self.scoped = false;
        self.reload = 0.0;
    }

    fn reaction(&self, cx: &mut Ctx) -> f32 {
        // Entre 0,17 et 0,32 s environ selon l'adresse, avec un peu de hasard.
        0.34 - 0.2 * self.skill + cx.unit() * 0.08
    }

    #[allow(clippy::too_many_arguments)]
    fn step(
        &mut self,
        cx: &mut Ctx,
        s: &Scene,
        me: &Body,
        world: &World,
        dt: f32,
        acts: &mut Vec<Act>,
    ) {
        let m = s.m;
        if !me.alive {
            if self.alive {
                self.alive = false;
                self.clear_life();
            }
            self.set_keys(false, false, acts);
            return;
        }
        if !self.alive || me.feet.distance(self.last_feet) > 4.0 {
            // Réapparition (ou replacé) : on repart de la vue donnée par l'hôte.
            self.alive = true;
            self.yaw = me.yaw;
            self.pitch = 0.0;
            self.path.clear();
            self.dest = None;
            self.goal = Goal::Wait;
            self.stuck_n = 0;
        }
        self.last_feet = me.feet;
        let Some(mp) = m.players.get(&me.id) else {
            return;
        };
        // Minuteries.
        self.deploy -= dt;
        self.cooldown -= dt;
        self.since_shot += dt;
        self.react -= dt;
        self.strafe_t -= dt;
        self.jump_cd -= dt;
        self.unstick -= dt;
        self.door_cd -= dt;
        self.backoff -= dt;
        if self.reload > 0.0 {
            self.reload -= dt;
            if self.reload <= 0.0 {
                self.finish_reload();
            }
        }
        if me.speed() < 0.2 && me.on_ground {
            self.still_t += dt;
        } else {
            self.still_t = 0.0;
        }
        // Achats.
        let freeze = matches!(m.phase, phase::FREEZE | phase::WARMUP);
        if freeze && !self.bought && cx.time - cx.plan.freeze_at >= self.buy_at {
            self.bought = true;
            for item in self.shopping(cx, s, me) {
                acts.push(Act::Buy { bot: me.id, item });
            }
        }
        // Ce qu'il voit et entend.
        if (s.tick + cx.slot).is_multiple_of(LOOK_EVERY) && !cx.calm {
            self.perceive(cx, s, me, world);
        }
        // Où aller.
        if (s.tick + cx.slot * 3).is_multiple_of(DECIDE_EVERY) {
            self.decide(cx, s, me, world);
        }
        // Qui viser.
        let target = self.update_target(cx, s, me);
        // Arme en main.
        let fighting = target.is_some() || self.recent_threat(cx.time);
        self.choose_weapon(cx, mp.guns, fighting, me, acts);
        // Déplacement.
        let mut mv = self.navigate(cx, s, me, world, dt);
        // Poser, désamorcer : immobile, touche tenue.
        let (mut plant, mut defuse) = (false, false);
        match self.goal {
            Goal::Plant(n) if m.carrier == Some(me.id) && m.phase == phase::LIVE => {
                let here = cx
                    .nav
                    .and_then(|nav| nav.near(me.feet))
                    .is_some_and(|h| h == n || self.at >= self.path.len());
                let in_site = m
                    .map
                    .sites
                    .iter()
                    .flatten()
                    .any(|z| z.holds(me.feet.to_array()))
                    || m.map.sites.iter().all(|z| z.is_none());
                if here && in_site && me.on_ground {
                    mv.dir = Vec3::ZERO;
                    mv.stop = true;
                    plant = me.speed() < 0.4 && (target.is_none() || m.busy(me.id));
                } else if here && !in_site && self.at >= self.path.len() && !self.path.is_empty() {
                    // Arrivé à côté du site (glissé, ou case au bord) : une autre case.
                    cx.plan.plant = None;
                    self.dest = None;
                    self.path.clear();
                }
            }
            Goal::Defuse => {
                mv.dir = Vec3::ZERO;
                mv.stop = true;
                let need = if mp.kit {
                    crate::cs::DEFUSE_KIT_S
                } else {
                    crate::cs::DEFUSE_S
                };
                let left = m
                    .defusing()
                    .filter(|d| d.0 == me.id)
                    .map_or(need, |d| need - d.1);
                // Un ennemi en vue et encore long à finir : on se défend d'abord.
                defuse = me.on_ground && (target.is_none() || left < 1.5);
            }
            _ => {}
        }
        // Combat : s'arrêter pour tirer, se décaler entre deux rafales.
        let mut duck = mv.duck;
        let busy = plant || defuse;
        if let Some(t) = target.filter(|_| self.react <= 0.0 && !busy) {
            let fight = self.fight_moves(cx, me, &t);
            mv.dir = fight.0;
            mv.stop = fight.1;
            duck = fight.2;
        }
        self.set_keys(plant, defuse, acts);
        if let Some(pos) = self.door.take() {
            self.door_cd = 2.0;
            acts.push(Act::Door {
                bot: me.id,
                pos,
                side: side_toward(mv.dir),
            });
        }
        // Regard.
        self.update_look(cx, me, target.as_ref(), &mv, dt);
        // Tir.
        if let Some(t) = target.filter(|_| self.react <= 0.0 && !busy) {
            self.try_fire(cx, s, me, &t, acts);
        }
        // Entrées du pas.
        let (mut forward, mut strafe) = keys_for(mv.dir, self.yaw);
        if mv.stop {
            // Contre-pas : la touche opposée à l'élan, pour s'arrêter net (comme dans CS).
            let (sy, cy) = self.yaw.sin_cos();
            let vf = me.vel.x * sy - me.vel.z * cy;
            let vr = me.vel.x * cy + me.vel.z * sy;
            forward = if vf > 0.6 {
                -1
            } else if vf < -0.6 {
                1
            } else {
                0
            };
            strafe = if vr > 0.6 {
                -1
            } else if vr < -0.6 {
                1
            } else {
                0
            };
        }
        if mv.ladder_fwd {
            forward = 1;
            strafe = 0;
        }
        let def = WEAPONS.get(self.weapon as usize);
        let carry = def.map_or(250, |d| d.speed(self.scoped));
        let input = NetInput {
            forward,
            strafe,
            jump: mv.jump,
            jump_pressed: mv.jump,
            walk: mv.walk,
            duck,
            yaw: self.yaw,
            pitch: self.pitch,
            carry,
        };
        acts.push(Act::Input(me.id, input));
    }

    fn set_keys(&mut self, plant: bool, defuse: bool, acts: &mut Vec<Act>) {
        if self.keys != (plant, defuse) {
            self.keys = (plant, defuse);
            acts.push(Act::Keys {
                bot: self.id,
                plant,
                defuse,
            });
        }
    }

    fn recent_threat(&self, now: f32) -> bool {
        self.seen.values().any(|(_, t)| now - t < 3.0)
            || self.heard.is_some_and(|(_, t)| now - t < 3.0)
    }

    // -----------------------------------------------------------------------------------
    // Perception
    // -----------------------------------------------------------------------------------

    fn perceive(&mut self, cx: &mut Ctx, s: &Scene, me: &Body, world: &World) {
        let eye = me.eye();
        let look = look_dir(self.yaw, self.pitch);
        for e in s.bodies.iter().filter(|e| e.alive && e.team != me.team) {
            let head = e.head();
            let d = head - eye;
            let dist = d.length();
            if dist > RANGE {
                continue;
            }
            // Devant lui (ou tout près : on le sent).
            let in_view = dist < 2.0 || d.normalize_or_zero().dot(look) > VIEW_COS;
            if in_view {
                let parts = [head, e.chest(), e.feet + Vec3::Y * 0.5];
                let visible = parts
                    .iter()
                    .any(|&p| clear_sight(world, eye, p) && !smoked(s.smokes, eye, p));
                if visible {
                    self.seen.insert(e.id, (e.feet, cx.time));
                    let team = &mut cx.intel[team_index(me.team)];
                    team.retain(|i| i.id != e.id);
                    team.push(Intel {
                        id: e.id,
                        pos: e.feet,
                        t: cx.time,
                    });
                    continue;
                }
            }
            // Entendu : un tir, des pas de course.
            let shot = s
                .last_shot
                .get(&e.id)
                .is_some_and(|&t| s.tick.wrapping_sub(t) <= LOOK_EVERY && dist < HEAR_SHOT);
            let run =
                e.on_ground && e.speed() > crate::netgame::step_speed(e.carry) && dist < HEAR_STEP;
            if shot || run {
                // Position à peu près (on entend d'où ça vient, pas au bloc près).
                let fuzz = Vec3::new(cx.unit() - 0.5, 0.0, cx.unit() - 0.5) * (dist * 0.1);
                self.heard = Some((e.feet + fuzz, cx.time));
                if shot {
                    let team = &mut cx.intel[team_index(me.team)];
                    if !team.iter().any(|i| i.id == e.id && cx.time - i.t < 1.0) {
                        team.push(Intel {
                            id: e.id,
                            pos: e.feet + fuzz,
                            t: cx.time,
                        });
                    }
                }
            }
        }
        let now = cx.time;
        self.seen.retain(|_, (_, t)| now - *t < MEMORY_S);
    }

    /// Ennemi visé : celui qu'on voit (le même tant qu'on le voit, sinon le plus proche).
    fn update_target(&mut self, cx: &mut Ctx, s: &Scene, me: &Body) -> Option<Body> {
        let now = cx.time;
        let fresh = |id: &u64, seen: &HashMap<u64, (Vec3, f32)>| {
            seen.get(id).is_some_and(|(_, t)| now - t < 0.06)
        };
        let alive = |id: u64| s.body(id).filter(|b| b.alive).copied();
        if let Some(t) = self.target {
            if fresh(&t, &self.seen) {
                if let Some(b) = alive(t) {
                    return Some(b);
                }
            }
        }
        let best = s
            .bodies
            .iter()
            .filter(|e| e.alive && e.team != me.team && fresh(&e.id, &self.seen))
            .min_by(|a, b| {
                a.feet
                    .distance(me.feet)
                    .total_cmp(&b.feet.distance(me.feet))
            })
            .copied();
        match best {
            Some(b) => {
                if self.target != Some(b.id) {
                    // Nouvel ennemi : le temps de réagir, plus court si on l'attendait là.
                    let expected = self.look.is_some_and(|l| {
                        let d1 = (b.head() - me.eye()).normalize_or_zero();
                        let d2 = (l - me.eye()).normalize_or_zero();
                        d1.dot(d2) > 0.97
                    });
                    let base = self.reaction(cx);
                    self.react = if expected { base * 0.7 } else { base };
                    self.target = Some(b.id);
                    self.head_aim = cx.unit() < 0.35 + 0.45 * self.skill;
                    self.duck_fight = cx.unit() < 0.25;
                    self.burst = 0;
                    // Premier coup d'œil un peu à côté (plus ou moins selon l'adresse).
                    let (ty, tp) = angles(b.head() - me.eye());
                    let dy = wrap(ty - self.yaw);
                    let dp = tp - self.pitch;
                    let k = (1.0 - self.skill) * 0.35;
                    self.aim_err = (
                        dy * k * (cx.unit() * 2.0 - 1.0),
                        dp * k * (cx.unit() * 2.0 - 1.0) + 0.01 * (cx.unit() - 0.5),
                    );
                }
                Some(b)
            }
            None => {
                self.target = None;
                None
            }
        }
    }

    // -----------------------------------------------------------------------------------
    // Décisions
    // -----------------------------------------------------------------------------------

    fn decide(&mut self, cx: &mut Ctx, s: &Scene, me: &Body, world: &World) {
        let m = s.m;
        let Some(nav) = cx.nav else {
            self.goal = Goal::Wait;
            return;
        };
        if !matches!(
            m.phase,
            phase::FREEZE | phase::LIVE | phase::PLANTED | phase::ROUND_END
        ) {
            self.goal = Goal::Wait;
            return;
        }
        let goal = if me.team == T {
            self.decide_t(cx, s, me, nav, world)
        } else {
            self.decide_ct(cx, s, me, nav)
        };
        self.goal = goal;
        let dest = match goal {
            Goal::Go(n) | Goal::Plant(n) | Goal::Flee(n) => Some(n),
            Goal::Wait | Goal::Defuse => None,
        };
        let retry = dest.is_some() && self.path.is_empty() && cx.time >= self.retry_at;
        let detour = cx.time < self.detour_until && self.at < self.path.len();
        if (dest != self.dest || retry) && !detour {
            self.route(cx, nav, me, dest);
            if dest.is_some() && self.path.is_empty() {
                // Injoignable d'ici : une autre case la prochaine fois (pas de recherche à
                // chaque décision).
                self.retry_at = cx.time + 1.0;
                cx.plan.spots.remove(&me.id);
                if matches!(goal, Goal::Plant(_)) {
                    cx.plan.plant = None;
                }
            }
        }
    }

    /// Le bot doit fuir la bombe maintenant pour ne pas mourir de l'explosion.
    fn must_flee(&self, m: &Match, me: &Body) -> bool {
        if m.bomb != bomb::PLANTED {
            return false;
        }
        let d = (me.feet + Vec3::Y - m.bomb_pos).length();
        if d >= BOMB_SAFE {
            return false;
        }
        let need = (BOMB_SAFE - d) * 1.5 / FPS_SPRINT + 1.5;
        m.time < need
    }

    fn flee_node(&self, cx: &mut Ctx, nav: &Nav, me: &Body, bomb: Vec3) -> Option<u32> {
        if let Goal::Flee(n) = self.goal {
            return Some(n);
        }
        let start = nav.near(me.feet)?;
        let far = |i: u32| (nav.center(i) + Vec3::Y - bomb).length() >= BOMB_SAFE + 2.0;
        let p = cx
            .search
            .path(nav, start, To::Where(&far), &|i, l: &Link| nav.cost(i, l))?;
        p.last().map(|x| x.0)
    }

    /// Une case libre parmi `pool` (loin des cases déjà prises par les coéquipiers), où
    /// l'on arrive depuis le départ de son équipe (pas sur une caisse inaccessible).
    fn pick_spot(&self, cx: &mut Ctx, nav: &Nav, pool: &[u32], me: &Body) -> Option<u32> {
        if let Some(&n) = cx.plan.spots.get(&me.id) {
            if pool.contains(&n) {
                return Some(n);
            }
        }
        let field = if me.team == T {
            &nav.from_t
        } else {
            &nav.from_ct
        };
        let pool: Vec<u32> = pool.iter().copied().filter(|&n| field.reaches(n)).collect();
        if pool.is_empty() {
            return None;
        }
        let taken: Vec<Vec3> = cx
            .plan
            .spots
            .iter()
            .filter(|(id, _)| **id != me.id)
            .map(|(_, &n)| nav.center(n))
            .collect();
        let mut best = None;
        for _ in 0..24 {
            let n = pool[cx.rand() as usize % pool.len()];
            let c = nav.center(n);
            let gap = taken
                .iter()
                .map(|t| t.distance(c))
                .fold(f32::INFINITY, f32::min);
            if gap >= 3.0 {
                best = Some(n);
                break;
            }
            if best.is_none() {
                best = Some(n);
            }
        }
        if let Some(n) = best {
            cx.plan.spots.insert(me.id, n);
        }
        best
    }

    fn decide_t(&mut self, cx: &mut Ctx, s: &Scene, me: &Body, nav: &Nav, world: &World) -> Goal {
        let m = s.m;
        let live = cx.time - cx.plan.live_at;
        // Bombe posée : la garder, puis fuir l'explosion.
        if m.bomb == bomb::PLANTED {
            if self.must_flee(m, me) {
                return self
                    .flee_node(cx, nav, me, m.bomb_pos)
                    .map_or(Goal::Wait, Goal::Flee);
            }
            let bomb_node = nav.near(m.bomb_pos);
            // Un CT désamorce : on y va.
            let defusing = s
                .bodies
                .iter()
                .any(|b| b.alive && b.team == CT && m.busy(b.id));
            if defusing {
                if let Some(n) = bomb_node {
                    return Goal::Go(n);
                }
            }
            if let Some(n) = self.guard_spot(cx, nav, me, m.bomb_pos, world) {
                self.look = Some(self.approach(nav, n, false));
                return Goal::Go(n);
            }
            return Goal::Wait;
        }
        // Bombe au sol : le T le plus proche va la ramasser.
        if m.bomb == bomb::DROPPED {
            let mine = me.feet.distance(m.bomb_pos);
            let closer = s.bodies.iter().any(|b| {
                b.alive && b.team == T && b.id != me.id && b.feet.distance(m.bomb_pos) < mine
            });
            if !closer {
                if let Some(n) = nav.near(m.bomb_pos) {
                    return Goal::Go(n);
                }
            }
        }
        let Some(site) = cx.plan.site else {
            // Carte sans site : chasse vers le départ adverse.
            return self.hunt(cx, nav, me, m);
        };
        let pool = &nav.sites[site];
        if m.carrier == Some(me.id) {
            let plant = match cx.plan.plant {
                Some(n) => n,
                None => {
                    let z = m.map.sites[site].unwrap_or(Zone::new([0; 3], [0; 3]));
                    let mid = Vec3::from(z.center());
                    // (Des cases où l'on arrive depuis le départ des T : pas le dessus d'une
                    // caisse sans accès.)
                    let inside: Vec<u32> = pool
                        .iter()
                        .copied()
                        .filter(|&n| z.holds(nav.center(n).to_array()))
                        .filter(|&n| nav.from_t.reaches(n))
                        .collect();
                    // Vers le milieu du site, pas toujours au même endroit.
                    let pick = inside
                        .iter()
                        .map(|&n| (nav.center(n).distance(mid) + (cx.rand() % 4) as f32, n))
                        .min_by(|a, b| a.0.total_cmp(&b.0))
                        .map(|x| x.1)
                        .or_else(|| pool.first().copied());
                    let Some(n) = pick else {
                        return Goal::Wait;
                    };
                    cx.plan.plant = Some(n);
                    n
                }
            };
            // Le porteur passe derrière : il attend qu'un coéquipier soit devant lui (au
            // plus 25 s).
            let mid = nav.center(plant);
            let mine = me.feet.distance(mid);
            let mates = s
                .bodies
                .iter()
                .filter(|b| b.alive && b.team == T && b.id != me.id);
            let (mut any, mut ahead) = (false, false);
            for b in mates {
                any = true;
                if b.feet.distance(mid) < mine - 2.0 {
                    ahead = true;
                }
            }
            if any && !ahead && live < 25.0 && mine > 12.0 && m.time > 30.0 {
                self.look = Some(mid + Vec3::Y * EYE);
                return Goal::Wait;
            }
            return Goal::Plant(plant);
        }
        if m.phase == phase::LIVE && live < self.delay && m.time > 40.0 {
            if let Some(&n) = pool.first() {
                self.look = Some(nav.center(n) + Vec3::Y * EYE);
            }
            return Goal::Wait;
        }
        match self.pick_spot(cx, nav, pool, me) {
            Some(n) => {
                self.look = Some(self.approach(nav, n, false));
                Goal::Go(n)
            }
            None => Goal::Wait,
        }
    }

    fn decide_ct(&mut self, cx: &mut Ctx, s: &Scene, me: &Body, nav: &Nav) -> Goal {
        let m = s.m;
        if m.bomb == bomb::PLANTED {
            let bomb = m.bomb_pos;
            let d = me.feet.distance(bomb);
            let kit = m.players.get(&me.id).is_some_and(|p| p.kit);
            let need = if kit {
                crate::cs::DEFUSE_KIT_S
            } else {
                crate::cs::DEFUSE_S
            };
            let defusing_now = self.goal == Goal::Defuse && m.busy(me.id);
            if defusing_now && m.time > 0.3 {
                // Commencé à temps (sinon on ne l'aurait pas commencé) : on finit.
                return Goal::Defuse;
            }
            let eta = d / (FPS_SPRINT * 0.85) + 0.8;
            if m.time < need + eta + 0.2 {
                // Trop tard pour désamorcer : se mettre à l'abri.
                if d < BOMB_SAFE {
                    return self
                        .flee_node(cx, nav, me, bomb)
                        .map_or(Goal::Wait, Goal::Flee);
                }
                return Goal::Wait;
            }
            // Un coéquipier plus proche (ou avec une pince) désamorce : on le couvre.
            let other_defusing = s
                .bodies
                .iter()
                .any(|b| b.alive && b.team == CT && b.id != me.id && m.busy(b.id));
            if d < 1.4 && me.on_ground && !other_defusing {
                return Goal::Defuse;
            }
            if other_defusing && d < 10.0 {
                self.look = self.heard.map(|h| h.0 + Vec3::Y * EYE);
                return Goal::Wait;
            }
            return nav.near(bomb).map_or(Goal::Wait, Goal::Go);
        }
        let Some(mut site) = cx.plan.guard.get(&me.id).copied().or(cx.plan.site) else {
            return self.hunt(cx, nav, me, m);
        };
        // Site attaqué (des T vus près d'un site) : on va aider.
        let attacked = (0..3)
            .filter(|&i| m.map.sites[i].is_some())
            .max_by_key(|&i| {
                let z = m.map.sites[i].expect("site");
                let c = Vec3::from(z.center());
                cx.intel[team_index(CT)]
                    .iter()
                    .filter(|x| cx.time - x.t < 6.0 && flat(x.pos - c).length() < 16.0)
                    .count()
            });
        if let Some(a) = attacked {
            let z = m.map.sites[a].expect("site");
            let c = Vec3::from(z.center());
            let n = cx.intel[team_index(CT)]
                .iter()
                .filter(|x| cx.time - x.t < 6.0 && flat(x.pos - c).length() < 16.0)
                .count();
            if n >= 2 && a != site {
                // Tous sauf un sur l'autre site (s'ils sont au moins deux).
                let others_here = cx
                    .plan
                    .guard
                    .iter()
                    .filter(|(id, g)| **id != me.id && **g == site)
                    .count();
                if others_here >= 1 {
                    cx.plan.guard.insert(me.id, a);
                    cx.plan.spots.remove(&me.id);
                    site = a;
                }
            }
        }
        match self.pick_spot(cx, nav, &nav.sites[site], me) {
            Some(n) => {
                if self.look.is_none() || self.dest != Some(n) {
                    self.look = Some(self.approach(nav, n, true));
                }
                Goal::Go(n)
            }
            None => Goal::Wait,
        }
    }

    /// Carte sans site : on va vers le départ adverse (ou le dernier ennemi connu).
    fn hunt(&mut self, cx: &mut Ctx, nav: &Nav, me: &Body, m: &Match) -> Goal {
        let known = cx.intel[team_index(me.team)]
            .iter()
            .max_by(|a, b| a.t.total_cmp(&b.t))
            .map(|i| i.pos);
        let to = known.or_else(|| {
            let z = if me.team == T { m.map.ct } else { m.map.t };
            z.map(|z| Vec3::from(z.center()))
                .or_else(|| m.fallback_spawns.map(|f| f[team_index(me.team) ^ 1]))
        });
        to.and_then(|p| nav.near(p)).map_or(Goal::Wait, Goal::Go)
    }

    /// Où regarder depuis la case `n` : vers l'arrivée des T (`ct` vrai), sinon des CT.
    fn approach(&self, nav: &Nav, n: u32, ct: bool) -> Vec3 {
        let f = if ct { &nav.from_t } else { &nav.from_ct };
        let from = nav.back(f, n, 14);
        let p = nav.center(from);
        if p.distance(nav.center(n)) < 2.0 {
            // Rien en vue (source injoignable) : droit devant.
            return nav.center(n) + Vec3::new(0.0, EYE, -4.0);
        }
        p + Vec3::Y * EYE
    }

    /// Case de garde autour de la bombe posée (vue sur elle).
    fn guard_spot(
        &mut self,
        cx: &mut Ctx,
        nav: &Nav,
        me: &Body,
        bomb: Vec3,
        world: &World,
    ) -> Option<u32> {
        if let Some(&n) = cx.plan.spots.get(&me.id) {
            let d = nav.center(n).distance(bomb);
            if (2.5..14.0).contains(&d) {
                return Some(n);
            }
        }
        let site = (0..3).find(|&i| {
            nav.sites[i]
                .iter()
                .any(|&n| nav.center(n).distance(bomb) < 6.0)
        });
        let field = if me.team == T {
            &nav.from_t
        } else {
            &nav.from_ct
        };
        let pool: Vec<u32> = match site {
            Some(i) => nav.sites[i]
                .iter()
                .copied()
                .filter(|&n| field.reaches(n))
                .collect(),
            None => {
                let b = nav.near(bomb)?;
                vec![b]
            }
        };
        if pool.is_empty() {
            return nav.near(bomb);
        }
        let taken: Vec<Vec3> = cx
            .plan
            .spots
            .iter()
            .filter(|(id, _)| **id != me.id)
            .map(|(_, &n)| nav.center(n))
            .collect();
        for _ in 0..30 {
            let n = pool[cx.rand() as usize % pool.len()];
            let c = nav.center(n);
            let d = c.distance(bomb);
            if !(3.0..12.0).contains(&d) || taken.iter().any(|t| t.distance(c) < 2.5) {
                continue;
            }
            if clear_sight(world, c + Vec3::Y * EYE, bomb + Vec3::Y * 0.3) {
                cx.plan.spots.insert(me.id, n);
                return Some(n);
            }
        }
        let n = nav.near(bomb)?;
        cx.plan.spots.insert(me.id, n);
        Some(n)
    }

    /// Calcule le chemin vers `dest` (chaque bot a ses préférences de route : les T ne
    /// passent pas tous par le même couloir).
    fn route(&mut self, cx: &mut Ctx, nav: &Nav, me: &Body, dest: Option<u32>) {
        self.dest = dest;
        self.path.clear();
        self.at = 0;
        let Some(dest) = dest else { return };
        let Some(start) = nav.near(me.feet) else {
            return;
        };
        let seed = self.seed;
        let blocked = self.blocked.clone();
        let noise = |n: u32| {
            let c = nav.cells[n as usize];
            let mut h = (c[0].div_euclid(6) as u32).wrapping_mul(0x9E37_79B1)
                ^ (c[2].div_euclid(6) as u32).wrapping_mul(0x85EB_CA77)
                ^ seed;
            h ^= h >> 15;
            h = h.wrapping_mul(0x2C1B_3C6D);
            h ^= h >> 12;
            (h % 1000) as f32 / 1000.0
        };
        let cost = |from: u32, l: &Link| {
            let pen = if blocked.contains(&(from, l.to)) {
                60.0
            } else {
                0.0
            };
            nav.cost(from, l) * (1.0 + 0.7 * noise(l.to)) + pen
        };
        if let Some(p) = cx.search.path(nav, start, To::Node(dest), &cost) {
            self.path = p;
            self.at = 1.min(self.path.len());
        }
    }

    // -----------------------------------------------------------------------------------
    // Déplacements
    // -----------------------------------------------------------------------------------

    fn navigate(&mut self, cx: &mut Ctx, s: &Scene, me: &Body, world: &World, dt: f32) -> Move {
        let mut mv = Move::default();
        let m = s.m;
        if matches!(m.phase, phase::WARMUP | phase::GAME_OVER) {
            return mv;
        }
        let Some(nav) = cx.nav else {
            return mv;
        };
        if self.path.is_empty() || self.at >= self.path.len() {
            return mv;
        }
        // Cases déjà atteintes (ou dépassées : coin coupé, chute).
        let feet = me.feet;
        let reached = |n: u32, step: Step| {
            let c = nav.center(n);
            let d = flat(c - feet).length();
            let dy = feet.y - c.y;
            match step {
                Step::Ladder => d < 0.7 && dy.abs() < 0.35,
                _ => d < 0.45 && dy > -0.7 && dy < 1.2,
            }
        };
        let mut look_ahead = self.at;
        for k in self.at..(self.at + 4).min(self.path.len()) {
            if reached(self.path[k].0, self.path[k].1) {
                look_ahead = k + 1;
            }
        }
        self.at = look_ahead;
        // Coin coupé : la case suivante est sur le même palier, à deux pas en marchant.
        while self.at + 1 < self.path.len() {
            let (n, step) = self.path[self.at];
            let (n2, step2) = self.path[self.at + 1];
            let c = nav.center(n);
            let c2 = nav.center(n2);
            let same = (c.y - feet.y).abs() < 0.5 && (c2.y - c.y).abs() < 0.1;
            if step == Step::Walk && step2 == Step::Walk && same && flat(c - feet).length() < 0.9 {
                self.at += 1;
            } else {
                break;
            }
        }
        if self.at >= self.path.len() {
            return mv;
        }
        let (n, step) = self.path[self.at];
        let wp = nav.center(n);
        let to = flat(wp - feet);
        mv.dir = to.normalize_or_zero();
        let dist = to.length();
        // Sauts : à la bonne distance du bord. Pour monter d'un bloc, en courant on saute
        // tôt (à l'arrivée sur le bord on est déjà plus haut que lui) ; arrêté contre lui, on
        // saute de là (on passe dessus en l'air). Entre deux sauts, le temps de reprendre
        // son souffle (comme dans CS, des sauts répétés montent de moins en moins haut).
        if step == Step::Jump && me.on_ground && self.jump_cd <= 0.0 && self.backoff <= 0.0 {
            let prev = self.path[self.at.saturating_sub(1)].0;
            let from = nav.center(prev);
            let span = flat(wp - from).length();
            let rise = wp.y - from.y;
            let go = if rise > 0.5 && span <= 1.5 {
                if me.speed() > 3.0 {
                    dist <= 1.8
                } else {
                    dist <= 1.25
                }
            } else if span <= 1.5 {
                dist < 1.3
            } else {
                dist <= span - 0.25
            };
            if go {
                mv.jump = true;
                self.jump_cd = 0.7;
            }
        }
        // Échelles : face au mur, on monte en regardant devant, on descend en regardant
        // en bas.
        let here = [
            feet.x.floor() as i32,
            (feet.y + 0.05).floor() as i32,
            feet.z.floor() as i32,
        ];
        let wall = ladder_wall(world, here)
            .or_else(|| ladder_wall(world, nav.cells[n as usize]))
            .filter(|_| step == Step::Ladder || me.on_ladder);
        if let Some(w) = wall {
            let (yaw, _) = angles(w);
            if wp.y > feet.y + 0.2 {
                mv.ladder = Some((yaw, 0.25));
                mv.ladder_fwd = true;
            } else if wp.y < feet.y - 0.2 {
                mv.ladder = Some((yaw, -0.9));
                mv.ladder_fwd = true;
            }
        }
        // Coincé (la case suivante du chemin n'est pas atteinte en une demi-seconde, alors
        // qu'elles sont à un bloc l'une de l'autre) : ouvrir une porte, sauter ou reprendre de
        // l'élan, se décaler, puis éviter ce passage. (Pas pendant un combat : on s'arrête
        // exprès pour tirer.)
        // (Ni derrière un coéquipier qui passe devant : on attend qu'il avance.)
        let mate_ahead = s.bodies.iter().any(|b| {
            let rel = flat(b.feet - feet);
            b.alive
                && b.id != me.id
                && b.team == me.team
                && rel.length() < 1.6
                && rel.dot(mv.dir) > 0.3
                && (b.feet.y - feet.y).abs() < 1.5
        });
        if self.target.is_some() || m.frozen() || mate_ahead {
            self.stuck_t = 0.0;
            self.stuck_n = 0;
            self.stuck_at = usize::MAX;
        }
        self.stuck_t += dt;
        if self.stuck_t >= 0.5 {
            self.stuck_t = 0.0;
            if self.at == self.stuck_at {
                self.stuck_n += 1;
            } else {
                self.stuck_n = 0;
            }
            self.stuck_at = self.at;
            match self.stuck_n {
                // Une porte fermée devant : on l'ouvre (si la carte le permet).
                1 if m.map.doors() && self.door_cd <= 0.0 => {
                    self.door = door_ahead(world, feet, mv.dir);
                }
                // Un saut raté : on recule un peu pour reprendre de l'élan.
                2 if step == Step::Jump => {
                    self.backoff = 0.35;
                }
                2 => {
                    if me.on_ground {
                        mv.jump = true;
                    }
                }
                3 => {
                    self.unstick = 0.45;
                    self.unstick_dir = if cx.rand() & 1 == 0 { 1 } else { -1 };
                }
                4 | 6 => {
                    self.stuck_events += 1;
                    if self.at >= 1 {
                        let edge = (self.path[self.at - 1].0, n);
                        if !self.blocked.contains(&edge) {
                            self.blocked.push(edge);
                        }
                    }
                    let dest = self.dest;
                    self.route(cx, nav, me, dest);
                }
                8.. => {
                    // Toujours coincé : un pas vers une case voisine au hasard, puis on
                    // cherche de nouveau le chemin de là.
                    self.stuck_n = 0;
                    if let Some(start) = nav.near(feet) {
                        let links = nav.links(start);
                        if !links.is_empty() {
                            let l = links[cx.rand() as usize % links.len()];
                            self.path = vec![(start, Step::Walk), (l.to, l.step)];
                            self.at = 1;
                            self.dest = None;
                            self.detour_until = cx.time + 1.0;
                        }
                    }
                }
                _ => {}
            }
        }
        if self.unstick > 0.0 {
            let side = Vec3::new(-mv.dir.z, 0.0, mv.dir.x) * self.unstick_dir as f32;
            mv.dir = (mv.dir * 0.3 + side).normalize_or_zero();
        }
        if self.backoff > 0.0 {
            mv.dir = -mv.dir;
            mv.jump = false;
        }
        // Coéquipier juste devant : on le contourne.
        for b in s.bodies.iter().filter(|b| b.alive && b.id != me.id) {
            let rel = flat(b.feet - feet);
            let d = rel.length();
            if d < 1.0 && d > 1e-3 && rel.dot(mv.dir) > 0.5 * d && (b.feet.y - feet.y).abs() < 1.5 {
                let side = Vec3::new(-rel.z, 0.0, rel.x) / d;
                let sign = if (self.seed & 1) == 0 { 1.0 } else { -1.0 };
                mv.dir = (mv.dir + side * sign * 0.8).normalize_or_zero();
            }
        }
        let ahead = (self.at + 3).min(self.path.len().saturating_sub(1));
        mv.ahead = self.path.get(ahead).map(|x| x.0);
        mv
    }

    /// Combat : (direction, s'arrêter, accroupi).
    fn fight_moves(&mut self, cx: &mut Ctx, me: &Body, t: &Body) -> (Vec3, bool, bool) {
        let to = flat(t.feet - me.feet);
        let dist = to.length();
        let side = Vec3::new(-to.z, 0.0, to.x).normalize_or_zero();
        let def = WEAPONS.get(self.weapon as usize);
        let close = dist < 6.0;
        // Rechargement : on recule.
        if self.reload > 0.0 || self.weapon == 255 {
            return (-to.normalize_or_zero(), false, false);
        }
        // Entre deux rafales : un pas de côté (on se décale comme un joueur), puis on
        // s'arrête pour tirer.
        let auto = def.is_some_and(|d| d.auto);
        if self.strafe_t > 0.0 {
            return (side * self.strafe as f32, false, false);
        }
        let between = auto && self.burst == 0 && self.since_shot < 0.5 && self.spray_n > 0;
        if between && !close && cx.unit() < 0.04 {
            self.strafe = -self.strafe;
            self.strafe_t = 0.18 + cx.unit() * 0.25;
            return (side * self.strafe as f32, false, false);
        }
        let duck = self.duck_fight && dist < 18.0 && auto;
        // Pistolet ou mitraillette tout près : on tire en bougeant.
        let run_gun =
            close && def.is_some_and(|d| matches!(d.category, Category::Pistol | Category::Smg));
        if run_gun {
            return (side * self.strafe as f32, false, duck);
        }
        (Vec3::ZERO, true, duck)
    }

    // -----------------------------------------------------------------------------------
    // Regard et tir
    // -----------------------------------------------------------------------------------

    fn update_look(&mut self, cx: &mut Ctx, me: &Body, target: Option<&Body>, mv: &Move, dt: f32) {
        let eye = me.eye();
        let now = cx.time;
        // Où regarder, par ordre d'importance.
        let mut combat = false;
        let point = if let Some(t) = target {
            if self.react > 0.0 {
                // Pas encore réagi : la vue ne bouge pas encore.
                return;
            }
            combat = true;
            Some(if self.head_aim { t.head() } else { t.chest() })
        } else if let Some((yaw, pitch)) = mv.ladder {
            let k = (dt * 12.0).min(1.0);
            self.yaw += wrap(yaw - self.yaw) * k;
            self.pitch += (pitch - self.pitch) * k;
            self.yaw = self.yaw.rem_euclid(std::f32::consts::TAU);
            return;
        } else if let Some((p, _)) = self
            .seen
            .values()
            .filter(|(_, t)| now - t < 2.5)
            .max_by(|a, b| a.1.total_cmp(&b.1))
        {
            // Perdu de vue : on garde l'angle là où il était.
            Some(*p + Vec3::Y * 1.55)
        } else if let Some((p, _)) = self.heard.filter(|(_, t)| now - t < 2.0) {
            Some(p + Vec3::Y * 1.55)
        } else if self.at >= self.path.len() || self.path.is_empty() {
            // Arrivé (ou en attente) : l'angle à tenir, qui change de temps en temps.
            self.look_t -= dt;
            if self.look_t <= 0.0 {
                self.look_t = 3.0 + cx.unit() * 5.0;
            }
            self.look
        } else {
            // En chemin : vers où l'on va (un peu plus loin que la case suivante).
            mv.ahead
                .and_then(|n| cx.nav.map(|nav| nav.center(n) + Vec3::Y * EYE))
        };
        let Some(p) = point else {
            return;
        };
        let (mut ty, mut tp) = angles(p - eye);
        if combat {
            // Erreur du premier coup d'œil, qui se corrige.
            ty += self.aim_err.0;
            tp += self.aim_err.1;
            let decay = (dt * (3.0 + 4.0 * self.skill)).min(1.0);
            self.aim_err.0 -= self.aim_err.0 * decay;
            self.aim_err.1 -= self.aim_err.1 * decay;
            // Léger tremblement de la main.
            let w = (1.0 - self.skill) * 0.004;
            let t = now * 2.3 + self.seed as f32 * 1e-6;
            ty += w * t.sin();
            tp += w * (t * 1.7).cos();
        } else {
            // Balayage lent de l'angle tenu (sinon immobile comme une statue).
            let t = now * 0.6 + (self.seed % 100) as f32;
            ty += 0.05 * t.sin();
        }
        // Vitesse de rotation humaine : rapide en combat, tranquille sinon.
        let (rate, max) = if combat {
            (10.0 + 8.0 * self.skill, 9.0 + 6.0 * self.skill)
        } else {
            (5.0, 4.5)
        };
        let k = (dt * rate).min(1.0);
        let dy = (wrap(ty - self.yaw) * k).clamp(-max * dt, max * dt);
        let dp = ((tp - self.pitch) * k).clamp(-max * dt, max * dt);
        self.yaw = (self.yaw + dy).rem_euclid(std::f32::consts::TAU);
        self.pitch = (self.pitch + dp).clamp(-1.5, 1.5);
    }

    fn try_fire(&mut self, cx: &mut Ctx, s: &Scene, me: &Body, t: &Body, acts: &mut Vec<Act>) {
        let w = self.weapon;
        let Some(def) = WEAPONS.get(w as usize) else {
            return;
        };
        if self.deploy > 0.0 || self.reload > 0.0 || self.cooldown > 0.0 {
            return;
        }
        // Fin de rafale : le motif repart du début.
        if self.since_shot > def.interval() * 1.8 + 0.12 {
            self.spray_n = 0;
        }
        let (mag, _) = self.ammo_of(w);
        if mag == 0 {
            self.start_reload();
            return;
        }
        let eye = me.eye();
        let aim = if self.head_aim { t.head() } else { t.chest() };
        let to = aim - eye;
        let dist = to.length();
        // Sur la cible ?
        let look = look_dir(self.yaw, self.pitch);
        let err = look.angle_between(to / dist.max(1e-3));
        let half = if self.head_aim { 0.11 } else { 0.2 };
        let tol = (half / dist.max(0.5)).atan() + 0.004 * (1.0 - self.skill);
        if err > tol {
            return;
        }
        // Précision : pas de tir en courant (sauf pistolet ou mitraillette de près).
        let speed = me.speed();
        let slow = speed < FPS_SPRINT * 0.34;
        let moving_ok = slow
            || (def.category == Category::Pistol && dist < 10.0)
            || (def.category == Category::Smg && dist < 12.0)
            || dist < 4.0;
        if !moving_ok || (!me.on_ground && dist > 4.0) {
            return;
        }
        // Un coéquipier dans la ligne de tir : on ne tire pas.
        let dir = to / dist.max(1e-3);
        let in_line = s.bodies.iter().any(|b| {
            if !b.alive || b.team != me.team || b.id == me.id {
                return false;
            }
            let c = b.chest();
            let along = (c - eye).dot(dir);
            along > 0.0 && along < dist && (eye + dir * along).distance(c) < 0.7
        });
        if in_line {
            return;
        }
        if def.auto {
            if self.burst == 0 {
                // Nouvelle rafale une fois le recul retombé.
                if self.spray_n > 0 {
                    return;
                }
                self.burst = if dist > TAP_DIST {
                    1
                } else if dist > SPRAY_DIST {
                    2 + cx.rand() % 3
                } else {
                    30
                };
            }
        } else if self.since_shot < def.interval() + 0.05 + (1.0 - self.skill) * 0.15 {
            return;
        }
        let snipe = def.scope.len() > 1;
        if snipe && !self.scoped && dist > 6.0 {
            self.scoped = true;
            // Le temps de mettre l'œil à la lunette.
            self.cooldown = 0.2;
            return;
        }
        let settled = self.still_t >= crate::fps::SETTLE_S;
        let spread = crate::fps::spread_deg(def, speed, me.on_ground, self.scoped, settled);
        let (up, side) = def.spray(self.spray_n);
        // Recul : contrôlé en bonne partie (on tire la souris vers le bas).
        let comp = 0.55 + 0.4 * self.skill;
        let up = up * (1.0 - comp);
        let side = side * (1.0 - comp);
        let mut dirs = Vec::new();
        for _ in 0..def.pellets.max(1) {
            let r = spread.to_radians() * cx.unit().sqrt();
            let a = (cx.unit() * 2.0 - 1.0) * std::f32::consts::PI;
            let p = self.pitch + up.to_radians() + r * a.sin();
            let y = self.yaw + side.to_radians() + r * a.cos();
            dirs.push(Vec3::new(y.sin() * p.cos(), p.sin(), -y.cos() * p.cos()));
        }
        acts.push(Act::Fire {
            bot: me.id,
            weapon: w,
            origin: eye,
            dirs,
        });
        self.cooldown = def.interval();
        self.since_shot = 0.0;
        self.spray_n += 1;
        self.burst = self.burst.saturating_sub(1);
        if let Some(a) = self.ammo.get_mut(&w) {
            a.0 = a.0.saturating_sub(1);
            if a.0 == 0 {
                self.start_reload();
            }
        }
        if snipe {
            // Après le coup : la lunette se rabaisse le temps de réarmer.
            self.scoped = false;
        }
    }

    // -----------------------------------------------------------------------------------
    // Armes et achats
    // -----------------------------------------------------------------------------------

    fn ammo_of(&mut self, w: u8) -> (u32, u32) {
        let def = &WEAPONS[w as usize];
        *self.ammo.entry(w).or_insert((def.magazine, def.reserve))
    }

    fn start_reload(&mut self) {
        let Some(def) = WEAPONS.get(self.weapon as usize) else {
            return;
        };
        let (mag, reserve) = self.ammo_of(self.weapon);
        if mag < def.magazine && reserve > 0 && self.reload <= 0.0 {
            self.reload = def.reload_s;
            self.scoped = false;
        }
    }

    fn finish_reload(&mut self) {
        let Some(def) = WEAPONS.get(self.weapon as usize) else {
            return;
        };
        if let Some(a) = self.ammo.get_mut(&self.weapon) {
            let need = def.magazine.saturating_sub(a.0).min(a.1);
            a.0 += need;
            a.1 -= need;
        }
    }

    fn choose_weapon(
        &mut self,
        cx: &mut Ctx,
        guns: (u8, u8),
        fighting: bool,
        me: &Body,
        acts: &mut Vec<Act>,
    ) {
        let usable = |b: &mut Self, w: u8| {
            w != 255 && WEAPONS.get(w as usize).is_some() && {
                let (m, r) = b.ammo_of(w);
                m + r > 0
            }
        };
        let gun = if usable(self, guns.0) {
            guns.0
        } else if usable(self, guns.1) {
            guns.1
        } else {
            255
        };
        // Long trajet sans ennemi en vue : au couteau, plus rapide (comme les joueurs).
        let far = self.path.len().saturating_sub(self.at) > 30;
        let quiet = !fighting
            && !cx.intel[team_index(me.team)]
                .iter()
                .any(|i| cx.time - i.t < 6.0 && i.pos.distance(me.feet) < 40.0);
        // (Pas près des sites : on y arrive l'arme sortie.)
        let knife = far
            && quiet
            && self.reload <= 0.0
            && matches!(self.goal, Goal::Go(_))
            && cx.time - cx.plan.live_at < 14.0;
        let want = if knife { 255 } else { gun };
        if want != self.weapon {
            self.weapon = want;
            self.deploy = crate::fps::DEPLOY_S;
            self.scoped = false;
            self.reload = 0.0;
            self.spray_n = 0;
            self.burst = 0;
            acts.push(Act::Hold {
                bot: me.id,
                weapon: want,
            });
        }
        // Rechargement au calme.
        if !fighting && self.reload <= 0.0 && want != 255 {
            let def = &WEAPONS[want as usize];
            let (mag, _) = self.ammo_of(want);
            if mag * 2 < def.magazine {
                self.start_reload();
            }
        }
    }

    /// Achats de la manche, comme un joueur : pistolet et gilet à la première manche,
    /// fusil et casque quand l'argent suit, achat forcé ou économie sinon.
    fn shopping(&self, cx: &mut Ctx, s: &Scene, me: &Body) -> Vec<u16> {
        let m = s.m;
        let Some(p) = m.players.get(&me.id) else {
            return Vec::new();
        };
        let warmup = !m.started();
        let mut left = if warmup { 16_000 } else { p.money as i64 };
        let mut list = Vec::new();
        let take = |item: u16, price: u32, left: &mut i64, list: &mut Vec<u16>| {
            if *left >= price as i64 {
                *left -= price as i64;
                list.push(item);
                true
            } else {
                false
            }
        };
        let r = cx.rand();
        let pistol_round = !warmup && m.played.is_multiple_of(12);
        let team = p.team;
        if pistol_round {
            if r.is_multiple_of(3) {
                let w = gun("P250");
                take(w as u16, WEAPONS[w as usize].price, &mut left, &mut list);
            } else {
                take(ITEM_KEVLAR, crate::cs::KEVLAR_PRICE, &mut left, &mut list);
            }
            if team == CT && !p.kit {
                take(ITEM_KIT, crate::cs::KIT_PRICE, &mut left, &mut list);
            }
            return list;
        }
        if p.guns.0 == 255 {
            let rifle = if team == T {
                gun("AK-47")
            } else if r & 4 == 0 {
                gun("M4A4")
            } else {
                gun("M4A1-S")
            };
            let awp = gun("AWP");
            let price = |w: u8| WEAPONS[w as usize].price as i64;
            let awper = cx.plan.awper[team_index(team)] == Some(me.id);
            if awper && left >= price(awp) + 1_000 {
                take(awp as u16, price(awp) as u32, &mut left, &mut list);
            } else if left >= price(rifle) + 1_000 {
                take(rifle as u16, price(rifle) as u32, &mut left, &mut list);
            } else if left >= 2_400 {
                // Achat forcé.
                let w = match (team == T, r & 8 == 0) {
                    (true, true) => gun("Galil AR"),
                    (true, false) => gun("MAC-10"),
                    (false, true) => gun("FAMAS"),
                    (false, false) => gun("MP9"),
                };
                take(w as u16, price(w) as u32, &mut left, &mut list);
            } else if left >= 1_000 && r.is_multiple_of(4) && p.guns.1 == crate::cs::starter(team) {
                // Économie : au mieux un meilleur pistolet.
                let w = if r & 16 == 0 {
                    gun("P250")
                } else {
                    gun("Desert Eagle")
                };
                take(w as u16, price(w) as u32, &mut left, &mut list);
                return list;
            } else if left < 2_400 {
                return list;
            }
        }
        // Gilet et casque, pince pour les CT.
        if p.armor < 100 || !p.helmet {
            let helmet = if p.armor >= 100 {
                crate::cs::HELMET_PRICE - crate::cs::KEVLAR_PRICE
            } else {
                crate::cs::HELMET_PRICE
            };
            if !take(ITEM_HELMET, helmet, &mut left, &mut list) && p.armor < 100 {
                take(ITEM_KEVLAR, crate::cs::KEVLAR_PRICE, &mut left, &mut list);
            }
        }
        if team == CT && !p.kit {
            take(ITEM_KIT, crate::cs::KIT_PRICE, &mut left, &mut list);
        }
        list
    }
}

/// Déplacement voulu à ce pas.
#[derive(Default)]
struct Move {
    dir: Vec3,
    jump: bool,
    duck: bool,
    walk: bool,
    /// S'arrêter (contre-pas).
    stop: bool,
    /// Sur une échelle : vue imposée (lacet, tangage) et avancer.
    ladder: Option<(f32, f32)>,
    ladder_fwd: bool,
    /// Case un peu plus loin sur le chemin (où regarder en marchant).
    ahead: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_follow_the_wanted_direction_whatever_the_view() {
        // Regard vers −Z (lacet 0) : avancer = −Z, droite = +X.
        assert_eq!(keys_for(Vec3::new(0.0, 0.0, -1.0), 0.0), (1, 0));
        assert_eq!(keys_for(Vec3::new(1.0, 0.0, 0.0), 0.0), (0, 1));
        assert_eq!(keys_for(Vec3::new(-1.0, 0.0, 1.0), 0.0), (-1, -1));
        assert_eq!(keys_for(Vec3::ZERO, 1.0), (0, 0));
        // Regard vers +X (lacet π/2) : aller vers +X, c'est avancer.
        let yaw = std::f32::consts::FRAC_PI_2;
        assert_eq!(keys_for(Vec3::X, yaw), (1, 0));
        assert_eq!(keys_for(Vec3::Z, yaw), (0, 1));
    }

    #[test]
    fn angles_and_look_dir_agree() {
        for d in [
            Vec3::new(1.0, 0.2, -3.0),
            Vec3::new(-2.0, -1.0, 0.5),
            Vec3::new(0.0, 0.0, 1.0),
        ] {
            let (yaw, pitch) = angles(d);
            let back = look_dir(yaw, pitch);
            assert!(back.distance(d.normalize()) < 1e-4, "{d:?} -> {back:?}");
        }
        // (−π et π : le même angle.)
        assert!((wrap(3.0 * std::f32::consts::PI).abs() - std::f32::consts::PI).abs() < 1e-4);
        assert!((wrap(0.1 + std::f32::consts::TAU) - 0.1).abs() < 1e-5);
    }

    #[test]
    fn bot_names_are_all_different() {
        let names = Bots::names(25, 9);
        let set: std::collections::HashSet<&String> = names.iter().collect();
        assert_eq!(set.len(), 25);
        assert!(names.iter().all(|n| n.starts_with("BOT ")));
    }

    /// Bombe posée : on ne fuit qu'au dernier moment, assez tôt pour s'en sortir.
    #[test]
    fn bots_flee_the_bomb_only_when_it_is_about_to_blow() {
        let mut m = Match::new(FpsMap::default(), [0; 32], 1);
        m.bomb = bomb::PLANTED;
        m.bomb_pos = Vec3::new(0.0, 10.0, 0.0);
        let b = Brain::new(1, 0.8, 1);
        let me = Body {
            id: 1,
            team: T,
            alive: true,
            feet: Vec3::new(5.0, 10.0, 0.0),
            vel: Vec3::ZERO,
            crouch: 0.0,
            on_ground: true,
            on_ladder: false,
            yaw: 0.0,
            carry: 250,
        };
        m.time = 30.0;
        assert!(!b.must_flee(&m, &me));
        // 33 blocs à faire à 5 blocs/s, avec de la marge.
        m.time = 9.0;
        assert!(b.must_flee(&m, &me));
        let far = Body {
            feet: Vec3::new(60.0, 10.0, 0.0),
            ..me
        };
        assert!(!b.must_flee(&m, &far));
    }
}
