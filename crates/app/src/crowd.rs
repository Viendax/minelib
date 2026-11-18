//! Essai de foule (temporaire, v164) : en solo, des joueurs simulés apparaissent autour du
//! joueur, de 0 à 4 000 en 2 minutes de jeu, et se promènent. Ils sont dessinés exactement
//! comme les joueurs d'un serveur de construction (mêmes modèles, mêmes paliers de détail,
//! mêmes pseudos au-dessus de la tête), pour mesurer la fluidité qu'aurait une vraie foule
//! autour de soi. (v185) Éteint par défaut : `/npc stresstest true` le lance,
//! `/npc stresstest false` les retire.

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
    /// (v191) Le ciel est-il bouché au-dessus de lui ? Sa lumière en découle. Cherché en
    /// changeant de case, et repris pour quelques-uns à chaque image (un toit peut être
    /// construit au-dessus d'eux) : parcourir la colonne de blocs de 4 000 joueurs à
    /// chaque image coûtait près d'une demi-milliseconde.
    pub covered: bool,
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
    pub fn update(&mut self, world: &World, center: Vec3, dt: f32) {
        self.t += dt;
        let want = Self::wanted(self.t);
        // Quelques apparitions par image au plus (la recherche du sol coûte).
        let mut budget = 64;
        while self.npcs.len() < want && budget > 0 {
            budget -= 1;
            if let Some(n) = self.spawn(world, center) {
                self.npcs.push(n);
            }
        }
        for i in 0..self.npcs.len() {
            self.step(world, center, i, dt);
        }
        // Abri repris pour quelques joueurs par image : tous en une minute environ.
        for _ in 0..RECHECK_PER_FRAME.min(self.npcs.len()) {
            self.recheck = (self.recheck + 1) % self.npcs.len();
            let n = &mut self.npcs[self.recheck];
            n.covered = covered(world, n.pos);
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
            pos,
            yaw,
            pitch: 0.0,
            speed,
            turn_in,
            cell: (x.floor() as i32, z.floor() as i32),
            covered: covered(world, pos),
        })
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
        n.turn_in -= dt;
        let turn = n.turn_in <= 0.0;
        if turn {
            n.turn_in = 1.0 + r1 * 6.0;
            n.yaw = (n.yaw + (r2 - 0.5) * 2.5).rem_euclid(std::f32::consts::TAU);
        }
        let far = (n.pos - center).length() > GONE;
        if far {
            // Trop loin : il revient près du joueur.
            if let Some(m) = self.spawn(world, center) {
                let name = std::mem::take(&mut self.npcs[i].name);
                self.npcs[i] = Npc { name, ..m };
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

    #[test]
    fn the_crowd_grows_to_four_thousand_in_two_minutes_and_walks_on_the_ground() {
        assert_eq!(Crowd::wanted(0.0), 0);
        assert_eq!(Crowd::wanted(60.0), 2000);
        assert_eq!(Crowd::wanted(120.0), MAX);
        assert_eq!(Crowd::wanted(500.0), MAX);
        // Un sol plat de 32 × 32 chunks (sol en y = 10).
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
        let mut c = Crowd::new();
        let center = Vec3::new(0.0, 11.0, 0.0);
        for _ in 0..(30.0 / 0.05) as usize {
            c.update(&w, center, 0.05);
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
