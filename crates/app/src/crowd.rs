//! Essai de foule (temporaire, v164) : en solo, des joueurs simulés apparaissent autour du
//! joueur, de 0 à 4 000 en 2 minutes de jeu, et se promènent. Ils sont dessinés exactement
//! comme les joueurs d'un serveur de construction (mêmes modèles, mêmes paliers de détail,
//! mêmes pseudos au-dessus de la tête), pour mesurer la fluidité qu'aurait une vraie foule
//! autour de soi. (v185) Éteint par défaut : `/npc stresstest true` le lance,
//! `/npc stresstest false` les retire.
//!
//! (v226) On peut les frapper (clic gauche à portée) : le PNJ recule d'un bond, rougit et
//! s'enfuit en courant. Un personnage caché, Pistache (skin dessiné pour lui, pseudo de la
//! foule comme les autres), s'y promène : celui qui le trouve et le frappe est prévenu, et
//! il repart se cacher ailleurs.

use glam::Vec3;
use minelib_world::World;

/// Joueurs simulés au plus, et temps (s) pour y arriver depuis zéro.
pub const MAX: usize = 4000;
const RAMP_S: f32 = 120.0;
/// Ils apparaissent entre ces deux distances du joueur (blocs) ; plus loin que `GONE`, ils
/// réapparaissent près de lui.
const NEAR: f32 = 6.0;
const FAR: f32 = 150.0;
const GONE: f32 = 200.0;
/// Le journal note leur nombre toutes les… (s), à côté des bilans d'images par seconde.
const LOG_EVERY: f32 = 10.0;
/// Joueurs dont on reprend l'abri par image (voir `Npc::covered`).
const RECHECK_PER_FRAME: usize = 64;
/// (v226) Coup reçu : recul (blocs/s, à l'horizontale, puis vers le haut), pesanteur
/// (blocs/s²) : un bond d'un peu moins d'un demi-bloc de haut, à 1,5 bloc en arrière.
const KNOCK: f32 = 5.0;
const KNOCK_UP: f32 = 5.5;
const GRAVITY: f32 = 30.0;
/// (v226) Durée du rouge d'un coup reçu, et de la fuite en courant qui suit (s).
pub const HURT_S: f32 = 0.4;
const FLEE_S: f32 = 3.0;
const RUN: f32 = 5.6;
/// (v226) Le personnage caché se cache au moins à cette distance de qui le cherche (blocs).
const HIDE_MIN: f32 = 40.0;
/// (v226) Son nom, dit à qui le trouve.
pub const HIDDEN_NAME: &str = "Pistache";

/// Un joueur simulé.
pub struct Npc {
    pub name: String,
    pub pos: Vec3,
    pub yaw: f32,
    pub pitch: f32,
    /// Vitesse au sol (blocs/s : arrêté, marche ou course).
    speed: f32,
    /// Temps avant de changer de direction ou d'allure.
    turn_in: f32,
    /// Case (x, z) dont la hauteur du sol est connue : on ne la recherche qu'en changeant
    /// de case (des milliers de joueurs : pas de recherche à chaque image).
    cell: (i32, i32),
    /// (v194) Sa marche (balancement des bras et des jambes, pose de l'image d'avant) :
    /// portée par lui, et non cherchée par son pseudo dans une table — à 4 000 joueurs
    /// c'était 8 000 recherches par image, deux fois le hachage de chaque pseudo.
    pub walk: crate::session::Walk,
    /// (v197) Son skin (presque toujours aucun : le modèle par défaut), repris de temps en
    /// temps comme `covered` — et non cherché par son pseudo à chaque image.
    pub skin: Option<std::sync::Arc<crate::skin::Skin>>,
    /// (v191) Le ciel est-il bouché au-dessus de lui ? Sa lumière en découle. Cherché en
    /// changeant de case, et repris pour quelques-uns à chaque image (un toit peut être
    /// construit au-dessus d'eux) : parcourir la colonne de blocs de 4 000 joueurs à
    /// chaque image coûtait près d'une demi-milliseconde.
    pub covered: bool,
    /// (v226) Bond d'un coup reçu : sa vitesse (blocs/s), et depuis combien de temps il est
    /// en l'air (0 : au sol).
    push: Vec3,
    air: f32,
    /// (v226) Temps qui reste du rouge d'un coup reçu (s).
    pub hurt: f32,
    /// (v226) C'est Pistache, le personnage caché (un seul dans la foule).
    pub hidden: bool,
}

pub struct Crowd {
    pub npcs: Vec<Npc>,
    t: f32,
    seed: u32,
    log_in: f32,
    /// Numéro du prochain pseudo.
    next: usize,
    /// Prochain joueur dont on reprendra l'abri (voir `Npc::covered`).
    recheck: usize,
    /// (v226) Le personnage caché est-il dans la foule ? (Le premier venu assez loin.)
    hidden_in: bool,
}

/// (v226) Ce qu'a fait un coup (`Crowd::hit`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Hit {
    /// Un PNJ de la foule : il recule et s'enfuit.
    Npc,
    /// Le personnage caché : trouvé, il est reparti se cacher ailleurs.
    Hidden,
}

impl Default for Crowd {
    fn default() -> Self {
        Self::new()
    }
}

impl Crowd {
    pub fn new() -> Self {
        Self {
            npcs: Vec::new(),
            t: 0.0,
            seed: 0x9e37_79b9,
            log_in: LOG_EVERY,
            next: 1,
            recheck: 0,
            hidden_in: false,
        }
    }

    /// Nombre de joueurs voulu après `t` secondes d'essai.
    pub fn wanted(t: f32) -> usize {
        ((t / RAMP_S).clamp(0.0, 1.0) * MAX as f32) as usize
    }

    fn rnd(&mut self) -> f32 {
        self.seed ^= self.seed << 13;
        self.seed ^= self.seed >> 17;
        self.seed ^= self.seed << 5;
        (self.seed >> 8) as f32 / (1u32 << 24) as f32
    }

    /// Une image : de nouveaux joueurs autour de `center` (jusqu'au nombre voulu), et
    /// chacun avance.
    pub fn update(
        &mut self,
        world: &World,
        skins: &crate::skin::Skins,
        center: Vec3,
        dt: f32,
    ) {
        self.t += dt;
        let want = Self::wanted(self.t);
        // Quelques apparitions par image au plus (la recherche du sol coûte).
        let mut budget = 64;
        while self.npcs.len() < want && budget > 0 {
            budget -= 1;
            if let Some(mut n) = self.spawn(world, center) {
                // (v226) Le personnage caché : le premier venu assez loin.
                if !self.hidden_in && n.pos.distance(center) >= HIDE_MIN {
                    self.hidden_in = true;
                    n.hidden = true;
                    n.skin = Some(crate::bot_skin::hidden_skin());
                }
                self.npcs.push(n);
            }
        }
        for i in 0..self.npcs.len() {
            self.step(world, center, i, dt);
        }
        // Leur marche, dans la foulée (voir `Npc::walk`).
        for n in &mut self.npcs {
            let (pos, yaw, pitch) = (n.pos, n.yaw, n.pitch);
            n.walk.update(pos, yaw, pitch, 0.0, dt);
        }
        // Abri repris pour quelques joueurs par image : tous en une minute environ.
        for _ in 0..RECHECK_PER_FRAME.min(self.npcs.len()) {
            self.recheck = (self.recheck + 1) % self.npcs.len();
            let i = self.recheck;
            let skin = skins.of(&self.npcs[i].name);
            let n = &mut self.npcs[i];
            n.covered = covered(world, n.pos);
            // (Le personnage caché garde le sien.)
            if !n.hidden {
                n.skin = skin;
            }
        }
        self.log_in -= dt;
        if self.log_in <= 0.0 {
            self.log_in = LOG_EVERY;
            log::info!(
                "foule d'essai : {} joueurs simulés autour de toi (vers {MAX} en {RAMP_S:.0} s)",
                self.npcs.len()
            );
        }
    }

    /// Un joueur posé sur le sol, à une distance au hasard de `center`.
    fn spawn(&mut self, world: &World, center: Vec3) -> Option<Npc> {
        let a = self.rnd() * std::f32::consts::TAU;
        // Réparti sur la surface du disque (autant de monde loin que près, par bloc²).
        let r = NEAR + (FAR - NEAR) * self.rnd().sqrt();
        let (x, z) = (center.x + a.cos() * r, center.z + a.sin() * r);
        let y = surface(world, x, z, center.y)?;
        let name = format!("Pnj{}", self.next);
        self.next += 1;
        let yaw = self.rnd() * std::f32::consts::TAU;
        let speed = self.pick_speed();
        let turn_in = 1.0 + self.rnd() * 6.0;
        let pos = Vec3::new(x, y, z);
        Some(Npc {
            name,
            walk: crate::session::Walk::new(pos),
            skin: None,
            pos,
            yaw,
            pitch: 0.0,
            speed,
            turn_in,
            cell: (x.floor() as i32, z.floor() as i32),
            covered: covered(world, pos),
            push: Vec3::ZERO,
            air: 0.0,
            hurt: 0.0,
            hidden: false,
        })
    }

    /// (v226) PNJ dont la boîte (0,6 × 1,8 × 0,6 bloc, comme un joueur) est touchée par le
    /// rayon `eye` + t · `dir` à moins de `reach` blocs : le plus proche, et sa distance.
    pub fn aimed(&self, eye: Vec3, dir: Vec3, reach: f32) -> Option<(usize, f32)> {
        let h = crate::player::HALF_WIDTH;
        let mut best: Option<(usize, f32)> = None;
        for (i, n) in self.npcs.iter().enumerate() {
            // Loin du rayon : écarté sans calcul (des milliers par image, clic tenu).
            if (n.pos - eye).length_squared() > (reach + 2.0) * (reach + 2.0) {
                continue;
            }
            let min = n.pos - Vec3::new(h, 0.0, h);
            let max = n.pos + Vec3::new(h, crate::player::HEIGHT, h);
            if let Some(t) = ray_box(eye, dir, min, max) {
                if t <= reach && best.is_none_or(|b| t < b.1) {
                    best = Some((i, t));
                }
            }
        }
        best
    }

    /// (v226) Coup de celui dont les yeux sont en `from` au PNJ `i` : il recule d'un bond loin
    /// de lui, rougit, puis s'enfuit en courant. Le personnage caché, lui, est trouvé : il
    /// repart se cacher ailleurs dans la foule (autour de `center`, loin de `from`), sous un
    /// autre pseudo.
    pub fn hit(&mut self, world: &World, i: usize, from: Vec3, center: Vec3) -> Hit {
        if self.npcs[i].hidden {
            self.hide(world, i, center, from);
            return Hit::Hidden;
        }
        let n = &mut self.npcs[i];
        let mut away = Vec3::new(n.pos.x - from.x, 0.0, n.pos.z - from.z).normalize_or_zero();
        if away == Vec3::ZERO {
            away = Vec3::X;
        }
        n.push = away * KNOCK + Vec3::Y * KNOCK_UP;
        n.air = n.air.max(1e-3);
        n.hurt = HURT_S;
        // Puis il s'enfuit, droit devant lui, loin du coup.
        n.yaw = away.x.atan2(-away.z).rem_euclid(std::f32::consts::TAU);
        n.speed = RUN;
        n.turn_in = FLEE_S;
        Hit::Npc
    }

    /// (v226) Le personnage caché `i` part se cacher ailleurs : à un endroit de la foule
    /// (autour de `center`) à au moins `HIDE_MIN` blocs de `away_from`, sous un pseudo neuf.
    /// Reste où il est s'il n'en trouve pas (chunks pas chargés).
    fn hide(&mut self, world: &World, i: usize, center: Vec3, away_from: Vec3) {
        for _ in 0..32 {
            let Some(m) = self.spawn(world, center) else {
                continue;
            };
            if m.pos.distance(away_from) < HIDE_MIN {
                continue;
            }
            let skin = self.npcs[i].skin.clone();
            self.npcs[i] = Npc {
                hidden: true,
                skin,
                ..m
            };
            return;
        }
    }

    /// Arrêté (un sur cinq), à la marche ou en courant.
    fn pick_speed(&mut self) -> f32 {
        let r = self.rnd();
        if r < 0.2 {
            0.0
        } else if r < 0.75 {
            4.3
        } else {
            5.6
        }
    }

    fn step(&mut self, world: &World, center: Vec3, i: usize, dt: f32) {
        let r1 = self.rnd();
        let r2 = self.rnd();
        let n = &mut self.npcs[i];
        n.hurt = (n.hurt - dt).max(0.0);
        // (v226) En l'air après un coup : il retombe, puis s'enfuit.
        if n.air > 0.0 {
            fly(world, n, dt);
            return;
        }
        n.turn_in -= dt;
        let turn = n.turn_in <= 0.0;
        if turn {
            n.turn_in = 1.0 + r1 * 6.0;
            n.yaw = (n.yaw + (r2 - 0.5) * 2.5).rem_euclid(std::f32::consts::TAU);
        }
        let far = (n.pos - center).length() > GONE;
        if far && n.hidden {
            // (v226) Le personnage caché revient aussi, mais pas tout près.
            self.hide(world, i, center, center);
            return;
        }
        if far {
            // Trop loin : il revient près du joueur.
            if let Some(m) = self.spawn(world, center) {
                let name = std::mem::take(&mut self.npcs[i].name);
                self.npcs[i] = Npc { name, ..m };
                // (v194) Il repart d'ailleurs : sa marche aussi, sinon il arriverait en
                // courant (et laisserait une traînée d'anticrénelage sur tout le chemin).
                // (Le pseudo pris par `spawn` n'est pas utilisé.)
                self.next -= 1;
            }
            return;
        }
        if turn {
            let s = self.pick_speed();
            self.npcs[i].speed = s;
        }
        let n = &mut self.npcs[i];
        if n.speed <= 0.0 {
            return;
        }
        let (sy, cy) = n.yaw.sin_cos();
        let next = n.pos + Vec3::new(sy, 0.0, -cy) * n.speed * dt;
        let cell = (next.x.floor() as i32, next.z.floor() as i32);
        if cell == n.cell {
            n.pos = next;
            return;
        }
        // Nouvelle case : son sol, à une marche près ; sinon (mur, trou), demi-tour.
        match step_ground(world, next.x, next.z, n.pos.y) {
            Some(y) => {
                n.pos = Vec3::new(next.x, y, next.z);
                n.cell = cell;
                n.covered = covered(world, n.pos);
            }
            None => {
                n.yaw = (n.yaw + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU);
            }
        }
    }
}

/// (v226) Un pas du bond d'un coup reçu : il recule (un mur l'arrête), monte, retombe sur le
/// sol (au plus 2 s en l'air : au-delà, posé sur le sol de sa colonne). Posé, il s'enfuit à la
/// vitesse décidée par le coup.
fn fly(world: &World, n: &mut Npc, dt: f32) {
    n.air += dt;
    n.push.y -= GRAVITY * dt;
    let mut next = n.pos + n.push * dt;
    let solid = |x: f32, y: f32, z: f32| {
        world
            .get(x.floor() as i32, y.floor() as i32, z.floor() as i32)
            .is_solid()
    };
    // Mur devant lui (le bord de sa boîte du côté où il recule, aux pieds, au milieu ou à la
    // tête) : plus de recul.
    let half = crate::player::HALF_WIDTH;
    let (ex, ez) = (
        next.x + half * n.push.x.signum() * f32::from(n.push.x != 0.0),
        next.z + half * n.push.z.signum() * f32::from(n.push.z != 0.0),
    );
    if [0.1, 0.9, 1.7]
        .iter()
        .any(|&h| solid(ex, n.pos.y + h, next.z) || solid(next.x, n.pos.y + h, ez))
    {
        next.x = n.pos.x;
        next.z = n.pos.z;
        n.push.x = 0.0;
        n.push.z = 0.0;
    }
    // Plafond : il ne monte plus.
    if n.push.y > 0.0 && solid(next.x, next.y + 1.8, next.z) {
        next.y = n.pos.y;
        n.push.y = 0.0;
    }
    let mut land = None;
    if n.push.y <= 0.0 {
        // Sol : le haut du bloc plein sous ses pieds, s'il l'atteint à cette image.
        let floor = next.y.floor();
        if solid(next.x, floor - 0.5, next.z) && next.y <= floor + 0.001 + n.push.y.abs() * dt {
            land = Some(floor);
        } else if solid(next.x, next.y, next.z) {
            land = Some(next.y.floor() + 1.0);
        }
    }
    if land.is_none() && n.air > 2.0 {
        land = surface(world, next.x, next.z, next.y).or(Some(n.pos.y));
    }
    match land {
        Some(y) => {
            n.pos = Vec3::new(next.x, y, next.z);
            n.push = Vec3::ZERO;
            n.air = 0.0;
            n.cell = (next.x.floor() as i32, next.z.floor() as i32);
            n.covered = covered(world, n.pos);
        }
        None => n.pos = next,
    }
}

/// (v226) Distance du rayon `o` + t · `d` à la boîte (`min`, `max`), s'il la touche.
fn ray_box(o: Vec3, d: Vec3, min: Vec3, max: Vec3) -> Option<f32> {
    let inv = d.recip();
    let (a, b) = ((min - o) * inv, (max - o) * inv);
    let near = a.min(b).max_element();
    let far = a.max(b).min_element();
    (far >= near.max(0.0)).then_some(near.max(0.0))
}

/// Le ciel est-il bouché au-dessus de ses yeux ? (La même lumière que pour les joueurs.)
fn covered(world: &World, pos: Vec3) -> bool {
    crate::session::covered_above(world, crate::session::eye_of(pos, 0.0))
}

/// Sol de la colonne (`x`, `z`) le plus haut entre 40 blocs sous `near_y` et 40 au-dessus :
/// un bloc plein avec deux cases libres au-dessus. `None` : rien (chunk absent, vide).
fn surface(world: &World, x: f32, z: f32, near_y: f32) -> Option<f32> {
    let (bx, bz) = (x.floor() as i32, z.floor() as i32);
    let y0 = near_y.floor() as i32;
    ((y0 - 40)..=(y0 + 40))
        .rev()
        .find(|&y| {
            world.get(bx, y, bz).is_solid()
                && !world.get(bx, y + 1, bz).is_solid()
                && !world.get(bx, y + 2, bz).is_solid()
        })
        .map(|y| (y + 1) as f32)
}

/// Sol de la case où l'on arrive, au plus une marche plus haut ou trois plus bas que les
/// pieds `feet_y` (on ne grimpe pas aux murs, on ne saute pas dans un trou).
fn step_ground(world: &World, x: f32, z: f32, feet_y: f32) -> Option<f32> {
    let (bx, bz) = (x.floor() as i32, z.floor() as i32);
    let y0 = feet_y.floor() as i32;
    ((y0 - 4)..=y0)
        .rev()
        .find(|&y| {
            world.get(bx, y, bz).is_solid()
                && !world.get(bx, y + 1, bz).is_solid()
                && !world.get(bx, y + 2, bz).is_solid()
        })
        .map(|y| (y + 1) as f32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use minelib_world::{BlockId, ChunkPos};

    /// Un sol plat de 32 × 32 chunks (sol en y = 10, pieds en y = 11).
    fn flat() -> World {
        let mut w = World::new(0);
        for cz in -16..16 {
            for cx in -16..16 {
                w.insert_empty(ChunkPos { x: cx, z: cz });
            }
        }
        for z in -200..200 {
            for x in -200..200 {
                w.set(x, 10, z, BlockId::STONE);
            }
        }
        w
    }

    /// Une foule d'un seul PNJ, immobile, en `at`.
    fn one_at(w: &World, at: Vec3) -> Crowd {
        let mut c = Crowd::new();
        let mut n = c.spawn(w, at).unwrap();
        n.pos = at;
        n.cell = (at.x.floor() as i32, at.z.floor() as i32);
        n.speed = 0.0;
        n.turn_in = 100.0;
        c.npcs.push(n);
        c
    }

    /// (v226) Frappé, un PNJ recule d'un bond, rougit, retombe sur le sol, puis s'enfuit en
    /// courant loin du coup ; un mur arrête son recul. Clic dans le vide ou trop loin : rien.
    #[test]
    fn a_hit_npc_jumps_back_and_runs_away() {
        let w = flat();
        let skins = crate::skin::Skins::default();
        let eye = Vec3::new(0.5, 12.62, 0.5);
        let mut c = one_at(&w, Vec3::new(3.5, 11.0, 0.5));
        assert!(c.aimed(eye, Vec3::Z, 4.5).is_none(), "à côté");
        assert!(c.aimed(eye, Vec3::X, 2.5).is_none(), "trop loin");
        let (i, t) = c.aimed(eye, Vec3::X, 4.5).unwrap();
        assert!((t - 2.7).abs() < 1e-4, "{t}");
        assert_eq!(c.hit(&w, i, eye, eye), Hit::Npc);
        assert!(c.npcs[0].hurt > 0.0);
        let mut top = 0.0f32;
        for _ in 0..12 {
            c.update(&w, &skins, eye, 1.0 / 60.0);
            top = top.max(c.npcs[0].pos.y);
        }
        assert!(top > 11.2 && top < 11.8, "bond : {top}");
        for _ in 0..30 {
            c.update(&w, &skins, eye, 1.0 / 60.0);
        }
        let n = &c.npcs[0];
        assert_eq!(n.pos.y, 11.0, "retombé sur le sol");
        assert!(n.pos.x > 4.5, "reculé : {}", n.pos.x);
        assert_eq!(n.hurt, 0.0, "plus rouge");
        let x = n.pos.x;
        for _ in 0..60 {
            c.update(&w, &skins, eye, 1.0 / 60.0);
        }
        assert!(c.npcs[0].pos.x > x + 4.0, "s'enfuit : {}", c.npcs[0].pos.x);
        assert!(c.npcs[0].pos.z.abs() < 1.0, "droit devant lui");

        // Un mur juste derrière lui : il n'y entre pas.
        let mut w = flat();
        for y in 11..14 {
            for z in -3..4 {
                w.set(4, y, z, BlockId::STONE);
            }
        }
        let mut c = one_at(&w, Vec3::new(3.5, 11.0, 0.5));
        let (i, _) = c.aimed(eye, Vec3::X, 4.5).unwrap();
        c.hit(&w, i, eye, eye);
        for _ in 0..120 {
            c.update(&w, &skins, eye, 1.0 / 60.0);
            let n = &c.npcs[0];
            // En l'air, sa boîte s'arrête au mur ; puis, comme quand il marche, il fait
            // demi-tour en y arrivant.
            let wall = if n.air > 0.0 { 4.0 - crate::player::HALF_WIDTH } else { 4.0 };
            assert!(n.pos.x <= wall + 1e-4, "dans le mur : {}", n.pos.x);
        }
        assert_eq!(c.npcs[0].pos.y, 11.0);
    }

    /// (v226) Un seul personnage caché dans la foule, loin du joueur, avec son skin ;
    /// trouvé (frappé), il repart se cacher loin de qui l'a trouvé, sous un autre pseudo.
    #[test]
    fn the_hidden_character_hides_again_when_found() {
        let w = flat();
        let skins = crate::skin::Skins::default();
        let center = Vec3::new(0.0, 11.0, 0.0);
        let mut c = Crowd::new();
        for _ in 0..(20.0 / 0.05) as usize {
            c.update(&w, &skins, center, 0.05);
        }
        let hidden: Vec<usize> = (0..c.npcs.len()).filter(|&i| c.npcs[i].hidden).collect();
        assert_eq!(hidden.len(), 1);
        let i = hidden[0];
        let skin = crate::bot_skin::hidden_skin();
        assert!(c.npcs[i]
            .skin
            .as_ref()
            .is_some_and(|s| std::sync::Arc::ptr_eq(s, &skin)));
        // Il se cache (et marche) dans la foule ; son pseudo est un pseudo de la foule.
        assert!(c.npcs[i].name.starts_with("Pnj"));
        // Trouvé : ailleurs, loin de qui l'a trouvé, sous un autre pseudo, toujours seul.
        let (name, at) = (c.npcs[i].name.clone(), c.npcs[i].pos);
        let finder = at + Vec3::new(-2.0, 1.62, 0.0);
        assert_eq!(c.hit(&w, i, finder, center), Hit::Hidden);
        let n = &c.npcs[i];
        assert!(n.hidden);
        assert_ne!(n.name, name);
        assert!(n.pos.distance(finder) >= HIDE_MIN, "{}", n.pos.distance(finder));
        assert_eq!(n.pos.y, 11.0);
        assert_eq!(c.npcs.iter().filter(|n| n.hidden).count(), 1);
        // Les skins repris de temps en temps ne lui enlèvent pas le sien.
        for _ in 0..200 {
            c.update(&w, &skins, center, 0.05);
        }
        let n = c.npcs.iter().find(|n| n.hidden).unwrap();
        assert!(n.skin.as_ref().is_some_and(|s| std::sync::Arc::ptr_eq(s, &skin)));
    }

    #[test]
    fn the_crowd_grows_to_four_thousand_in_two_minutes_and_walks_on_the_ground() {
        assert_eq!(Crowd::wanted(0.0), 0);
        assert_eq!(Crowd::wanted(60.0), 2000);
        assert_eq!(Crowd::wanted(120.0), MAX);
        assert_eq!(Crowd::wanted(500.0), MAX);
        let w = flat();
        let mut c = Crowd::new();
        let skins = crate::skin::Skins::default();
        let center = Vec3::new(0.0, 11.0, 0.0);
        for _ in 0..(30.0 / 0.05) as usize {
            c.update(&w, &skins, center, 0.05);
        }
        assert!(
            (Crowd::wanted(30.0) as i64 - c.npcs.len() as i64).abs() <= 64,
            "{} joueurs après 30 s",
            c.npcs.len()
        );
        for n in &c.npcs {
            assert_eq!(n.pos.y, 11.0, "{} : sur le sol", n.name);
            assert!((n.pos - center).length() <= GONE + 1.0);
        }
        // Des pseudos tous différents.
        let names: std::collections::HashSet<&str> =
            c.npcs.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names.len(), c.npcs.len());
    }
}
