//! Joueur : caméra à la première personne, déplacements, gravité et collisions
//! boîte englobante / voxels (résolues axe par axe).

use glam::{Mat4, Vec3};
use minelib_world::registry::{post_arms, Shape};
use minelib_world::World;

pub const HALF_WIDTH: f32 = 0.3;
pub const HEIGHT: f32 = 1.8;
pub const EYE: f32 = 1.62;

const GRAVITY: f32 = 28.0;
const JUMP_SPEED: f32 = 8.6;
const WALK: f32 = 4.4;
const SPRINT: f32 = 6.6;
/// Accroupi : marche lente, on ne tombe pas des bords.
const SNEAK: f32 = 1.3;
/// Les yeux descendent de 30 cm accroupi.
const CROUCH_DROP: f32 = 0.3;
/// Mode FPS : marche réduite de 50 %, sprint de 25 %.
pub const FPS_WALK: f32 = WALK * 0.5;
pub const FPS_SPRINT: f32 = SPRINT * 0.75;
const FLY: f32 = 12.0;
const FLY_SPRINT: f32 = 30.0;
/// Tolérance de contact (erreurs d'arrondi) : en deçà, deux faces se touchent sans se
/// recouvrir.
const TOUCH: f32 = 1e-4;
/// Hauteur franchie sans sauter (dalles, marches d'escalier).
const STEP_HEIGHT: f32 = 0.6;

pub struct Player {
    /// Position des pieds (centre de la base de la boîte).
    pub pos: Vec3,
    pub vel: Vec3,
    pub yaw: f32,
    pub pitch: f32,
    pub flying: bool,
    pub on_ground: bool,
    /// 0 = debout, 1 = accroupi (transition douce).
    pub crouch: f32,
    /// Spectateur : traverse les blocs.
    pub noclip: bool,
}

/// Intention de déplacement pour une image (indépendante du clavier).
#[derive(Default)]
pub struct MoveInput {
    pub forward: f32, // -1..1
    pub strafe: f32,  // -1..1 (positif = droite)
    pub up: f32,      // vol : -1..1
    pub jump: bool,
    pub sprint: bool,
    pub sneak: bool,
    /// Mode FPS : vitesses réduites et vitesse horizontale plafonnée (le saut
    /// ne fait pas aller plus vite).
    pub fps: bool,
}

impl Player {
    pub fn new(pos: Vec3) -> Self {
        Self {
            pos,
            vel: Vec3::ZERO,
            yaw: 0.0,
            pitch: -0.25,
            flying: false,
            on_ground: false,
            crouch: 0.0,
            noclip: false,
        }
    }

    pub fn eye(&self) -> Vec3 {
        self.pos + Vec3::Y * (EYE - CROUCH_DROP * self.crouch)
    }

    pub fn look_dir(&self) -> Vec3 {
        let (sy, cy) = self.yaw.sin_cos();
        let (sp, cp) = self.pitch.sin_cos();
        Vec3::new(sy * cp, sp, -cy * cp)
    }

    pub fn view(&self) -> Mat4 {
        Mat4::look_to_rh(self.eye(), self.look_dir(), Vec3::Y)
    }

    pub fn rotate(&mut self, dx: f32, dy: f32) {
        self.yaw = (self.yaw + dx).rem_euclid(std::f32::consts::TAU);
        self.pitch = (self.pitch - dy).clamp(-1.55, 1.55);
    }

    /// La boîte du joueur recouvre-t-elle le bloc (x, y, z) ?
    pub fn overlaps_block(&self, b: [i32; 3]) -> bool {
        let (min, max) = self.bounds();
        let bmin = Vec3::new(b[0] as f32, b[1] as f32, b[2] as f32);
        let bmax = bmin + Vec3::ONE;
        min.x < bmax.x
            && max.x > bmin.x
            && min.y < bmax.y
            && max.y > bmin.y
            && min.z < bmax.z
            && max.z > bmin.z
    }

    fn bounds(&self) -> (Vec3, Vec3) {
        let h = Vec3::new(HALF_WIDTH, 0.0, HALF_WIDTH);
        (self.pos - h, self.pos + h + Vec3::Y * HEIGHT)
    }

    pub fn update(&mut self, world: &World, m: &MoveInput, dt: f32) {
        let (sy, cy) = self.yaw.sin_cos();
        let fwd = Vec3::new(sy, 0.0, -cy);
        let right = Vec3::new(cy, 0.0, sy);
        let mut wish = fwd * m.forward + right * m.strafe;
        if wish.length_squared() > 1.0 {
            wish = wish.normalize();
        }

        let crouching = m.sneak && !self.flying;
        let target = if crouching { 1.0 } else { 0.0 };
        self.crouch += (target - self.crouch) * (dt * 12.0).min(1.0);

        if self.flying {
            let speed = if m.sprint { FLY_SPRINT } else { FLY };
            let target = wish * speed + Vec3::Y * m.up * speed;
            // Accélération/freinage doux en vol.
            self.vel = self.vel.lerp(target, (dt * 10.0).min(1.0));
        } else {
            let speed = if crouching {
                SNEAK
            } else if m.sprint {
                if m.fps {
                    FPS_SPRINT
                } else {
                    SPRINT
                }
            } else if m.fps {
                FPS_WALK
            } else {
                WALK
            };
            let target = wish * speed;
            let control = if self.on_ground { 14.0 } else { 3.0 };
            let k = (dt * control).min(1.0);
            self.vel.x += (target.x - self.vel.x) * k;
            self.vel.z += (target.z - self.vel.z) * k;
            self.vel.y -= GRAVITY * dt;
            self.vel.y = self.vel.y.max(-60.0);
            if m.jump && self.on_ground {
                self.vel.y = JUMP_SPEED;
            }
            if m.fps {
                // Plafond strict : ni le saut ni l'élan ne dépassent la vitesse permise.
                let h = Vec3::new(self.vel.x, 0.0, self.vel.z);
                let len = h.length();
                if len > speed {
                    let h = h * (speed / len);
                    self.vel.x = h.x;
                    self.vel.z = h.z;
                }
            }
        }

        let delta = self.vel * dt;
        if self.noclip {
            self.pos += delta;
            self.on_ground = false;
            return;
        }
        // Petits pas pour ne jamais traverser un bloc à grande vitesse.
        let steps = (delta.abs().max_element() / 0.4).ceil().max(1.0) as usize;
        let step = delta / steps as f32;
        let was_on_ground = self.on_ground;
        self.on_ground = false;
        for _ in 0..steps {
            for axis in [1, 0, 2] {
                let before = self.pos;
                self.step_axis(world, axis, step[axis], was_on_ground);
                // Accroupi au sol : on ne descend pas d'un bord (comme dans Minecraft).
                if axis != 1
                    && crouching
                    && was_on_ground
                    && self.vel.y <= 0.0
                    && !self.supported(world)
                {
                    self.pos[axis] = before[axis];
                    self.vel[axis] = 0.0;
                }
            }
        }
    }

    /// Déplacement sur un axe, avec atterrissage et montée automatique des marches.
    fn step_axis(&mut self, world: &World, axis: usize, d: f32, was_on_ground: bool) {
        let before = self.pos;
        if !self.move_axis(world, axis, d) {
            return;
        }
        if axis == 1 {
            if d < 0.0 {
                self.on_ground = true;
            }
            self.vel.y = 0.0;
            return;
        }
        // Marche automatique : on remonte d'au plus STEP_HEIGHT (dalles, escaliers).
        if (was_on_ground || self.on_ground) && !self.flying {
            let blocked = self.pos;
            self.pos = before;
            if !self.move_axis(world, 1, STEP_HEIGHT) {
                self.move_axis(world, axis, d);
                if (self.pos[axis] - before[axis]).abs()
                    > (blocked[axis] - before[axis]).abs() + 1e-4
                {
                    self.move_axis(world, 1, -STEP_HEIGHT);
                    self.on_ground = true;
                    return;
                }
            }
            self.pos = blocked;
        }
        self.vel[axis] = 0.0;
    }

    /// Y a-t-il un appui sous les pieds (à moins de 0,6 bloc) ?
    fn supported(&mut self, world: &World) -> bool {
        let saved = self.pos;
        let hit = self.move_axis(world, 1, -0.6);
        self.pos = saved;
        hit
    }

    /// Déplace le joueur sur un axe et le replace contre le premier bloc solide rencontré.
    ///
    /// Collision « balayée » : un bloc n'arrête le joueur que s'il était devant lui avant le
    /// déplacement et qu'il serait traversé après. Le joueur se pose alors **exactement**
    /// contre lui (pieds pile à la hauteur du sol, sans marge) : au repos, la gravité le
    /// ramène chaque image sur la même hauteur et le contact est détecté à chaque image
    /// (plus de Y qui oscille entre ,000 et ,001, ni d'images « en l'air » sur une dalle).
    fn move_axis(&mut self, world: &World, axis: usize, d: f32) -> bool {
        if d == 0.0 {
            return false;
        }
        let (bmin0, bmax0) = self.bounds();
        self.pos[axis] += d;
        let (min, max) = self.bounds();
        // Cases couvertes par le trajet (avant et après) sur cet axe.
        let lo = min.min(bmin0).floor().as_ivec3();
        let hi = (max.max(bmax0) - Vec3::splat(TOUCH)).floor().as_ivec3();
        let (below, above) = if axis == 1 {
            (0.0, HEIGHT)
        } else {
            (HALF_WIDTH, HALF_WIDTH)
        };
        let mut hit = false;
        for y in lo.y..=hi.y {
            for z in lo.z..=hi.z {
                for x in lo.x..=hi.x {
                    for (bmin, bmax) in collision_boxes(world, x, y, z) {
                        let (pmin, pmax) = self.bounds();
                        // Recouvrement sur les deux autres axes (un contact pile ne compte pas :
                        // on glisse le long d'un mur ou sur le sol sans s'y accrocher).
                        let side = (0..3).all(|k| {
                            k == axis || (pmin[k] < bmax[k] - TOUCH && pmax[k] > bmin[k] + TOUCH)
                        });
                        if !side {
                            continue;
                        }
                        if d > 0.0 {
                            // Bloc devant (au-dessus / plus loin) avant le pas, et atteint.
                            if bmin[axis] >= bmax0[axis] - TOUCH && pmax[axis] > bmin[axis] {
                                self.pos[axis] = self.pos[axis].min(bmin[axis] - above);
                                hit = true;
                            }
                        } else if bmax[axis] <= bmin0[axis] + TOUCH && pmin[axis] < bmax[axis] {
                            self.pos[axis] = self.pos[axis].max(bmax[axis] + below);
                            hit = true;
                        }
                    }
                }
            }
        }
        hit
    }
}

/// Boîtes de collision (coordonnées monde) du bloc en (x, y, z) : la forme réelle
/// (dalle, escalier, clôture reliée à ses voisins…), pas un cube entier.
fn collision_boxes(world: &World, x: i32, y: i32, z: i32) -> Vec<(Vec3, Vec3)> {
    let def = world.get(x, y, z).def();
    if !def.solid {
        return Vec::new();
    }
    let mut boxes = def.boxes();
    if let Shape::Post(kind) = def.shape {
        for (side, dx, dz) in [(0u8, 1, 0), (1, -1, 0), (4, 0, 1), (5, 0, -1)] {
            let n = world.get(x + dx, y, z + dz).def();
            if n.full || matches!(n.shape, Shape::Post(k) if k == kind) {
                // Bras sur toute la hauteur : on ne passe pas entre les barreaux.
                boxes.extend(post_arms(kind, side).into_iter().map(|(mut a, mut b)| {
                    a[1] = 0;
                    b[1] = 16;
                    (a, b)
                }));
            }
        }
    }
    let o = Vec3::new(x as f32, y as f32, z as f32);
    boxes
        .into_iter()
        .map(|(a, b)| {
            (
                o + Vec3::new(a[0] as f32, a[1] as f32, a[2] as f32) / 16.0,
                o + Vec3::new(b[0] as f32, b[1] as f32, b[2] as f32) / 16.0,
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use minelib_world::{BlockId, ChunkPos};

    #[test]
    fn spectator_flies_through_blocks() {
        let w = flat_world();
        let mut p = Player::new(Vec3::new(8.0, 12.0, 8.0));
        p.flying = true;
        p.noclip = true;
        p.pitch = 0.0;
        let down = MoveInput {
            up: -1.0,
            ..Default::default()
        };
        for _ in 0..60 {
            p.update(&w, &down, 1.0 / 30.0);
        }
        assert!(
            p.pos.y < 10.0,
            "doit traverser le sol de pierre : {}",
            p.pos.y
        );
        // Sans le mode spectateur, le sol arrête le joueur.
        let mut q = Player::new(Vec3::new(8.0, 12.0, 8.0));
        q.flying = true;
        for _ in 0..60 {
            q.update(&w, &down, 1.0 / 30.0);
        }
        assert!(q.pos.y >= 11.0 - 1e-3, "{}", q.pos.y);
    }

    fn flat_world() -> World {
        let mut w = World::new(0);
        w.insert_empty(ChunkPos { x: 0, z: 0 });
        for z in 0..16 {
            for x in 0..16 {
                w.set(x, 10, z, BlockId::STONE);
            }
        }
        w
    }

    #[test]
    fn falls_and_lands_on_ground() {
        let w = flat_world();
        let mut p = Player::new(Vec3::new(8.5, 15.0, 8.5));
        for _ in 0..200 {
            p.update(&w, &MoveInput::default(), 1.0 / 60.0);
        }
        assert!(p.on_ground);
        assert!((p.pos.y - 11.0).abs() < 0.01, "y = {}", p.pos.y);
    }

    #[test]
    fn wall_blocks_movement() {
        let mut w = flat_world();
        w.set(10, 11, 8, BlockId::STONE);
        w.set(10, 12, 8, BlockId::STONE);
        let mut p = Player::new(Vec3::new(8.5, 11.0, 8.5));
        p.yaw = std::f32::consts::FRAC_PI_2; // regarde vers +X
        let m = MoveInput {
            forward: 1.0,
            ..Default::default()
        };
        for _ in 0..120 {
            p.update(&w, &m, 1.0 / 60.0);
        }
        assert!(p.pos.x <= 10.0 - HALF_WIDTH + 0.01, "x = {}", p.pos.x);
        assert!(p.pos.x > 9.5);
    }

    #[test]
    fn walks_up_slabs_but_not_full_blocks() {
        let reg = minelib_world::registry::registry();
        let slab = reg.by_key("slab:stone:bottom").unwrap();
        let mut w = flat_world();
        w.set(10, 11, 8, slab);
        w.set(10, 11, 9, slab);
        let mut p = Player::new(Vec3::new(8.5, 11.0, 8.6));
        p.yaw = std::f32::consts::FRAC_PI_2; // vers +X
        let m = MoveInput {
            forward: 1.0,
            ..Default::default()
        };
        for _ in 0..90 {
            p.update(&w, &m, 1.0 / 60.0);
        }
        assert!(
            p.pos.x > 10.5,
            "la dalle doit se franchir : x = {}",
            p.pos.x
        );
        assert!(
            (p.pos.y - 11.5).abs() < 0.05 || p.pos.x > 11.3,
            "y = {}",
            p.pos.y
        );
        // Une clôture ne se traverse pas (bras sur toute la hauteur entre deux poteaux).
        let fence = reg.by_key("fence_oak").unwrap();
        let mut w = flat_world();
        for z in 0..16 {
            w.set(10, 11, z, fence);
        }
        let mut p = Player::new(Vec3::new(8.5, 11.0, 8.5));
        p.yaw = std::f32::consts::FRAC_PI_2;
        for _ in 0..90 {
            p.update(&w, &m, 1.0 / 60.0);
        }
        assert!(p.pos.x < 10.4, "x = {}", p.pos.x);
    }

    #[test]
    fn standing_height_is_exact_and_stable() {
        // À l'arrêt, à n'importe quel FPS : pieds pile sur le sol, au sol à chaque image.
        let reg = minelib_world::registry::registry();
        let slab = reg.by_key("slab:stone:bottom").unwrap();
        let mut w = flat_world();
        w.set(8, 11, 8, slab);
        for (start, ground) in [
            (Vec3::new(4.5, 12.0, 4.5), 11.0),
            (Vec3::new(8.5, 12.0, 8.5), 11.5),
        ] {
            for dt in [1.0 / 30.0, 1.0 / 144.0, 1.0 / 1000.0, 1.0 / 3000.0] {
                let mut p = Player::new(start);
                for _ in 0..(3.0 / dt) as usize {
                    p.update(&w, &MoveInput::default(), dt);
                }
                for i in 0..500 {
                    p.update(&w, &MoveInput::default(), dt);
                    assert_eq!(p.pos.y, ground, "dt = {dt}, image {i}");
                    assert!(p.on_ground, "dt = {dt}, image {i}");
                }
            }
        }
    }

    #[test]
    fn always_steps_up_slabs_at_any_fps() {
        // Une rangée de dalles puis une rangée de blocs pleins posés dessus (escalier de
        // demi-blocs) : montée sans sauter, à 30 comme à 3000 images/s.
        let reg = minelib_world::registry::registry();
        let slab = reg.by_key("slab:stone:bottom").unwrap();
        for dt in [1.0 / 30.0, 1.0 / 144.0, 1.0 / 1000.0, 1.0 / 3000.0] {
            let mut w = flat_world();
            for z in 0..16 {
                w.set(10, 11, z, slab);
                w.set(11, 11, z, BlockId::STONE);
                w.set(12, 11, z, BlockId::STONE);
                w.set(12, 12, z, slab);
            }
            let mut p = Player::new(Vec3::new(8.5, 11.0, 8.5));
            p.yaw = std::f32::consts::FRAC_PI_2; // vers +X
            let m = MoveInput {
                forward: 1.0,
                ..Default::default()
            };
            for _ in 0..(2.0 / dt) as usize {
                p.update(&w, &m, dt);
                if p.pos.x > 12.6 {
                    break;
                }
            }
            assert!(
                p.pos.x > 12.6,
                "dt = {dt} : bloqué en x = {} (y = {})",
                p.pos.x,
                p.pos.y
            );
            // Encore un peu sur la dernière marche, puis à l'arrêt : pile sur la dalle.
            for _ in 0..(0.5 / dt) as usize {
                p.update(&w, &MoveInput::default(), dt);
            }
            assert_eq!(p.pos.y, 12.5, "dt = {dt}");
        }
    }

    #[test]
    fn sneaking_does_not_walk_off_edges() {
        let w = flat_world(); // sol en y = 10 sur x, z dans 0..16
        let mut p = Player::new(Vec3::new(14.5, 11.0, 8.5));
        p.update(&w, &MoveInput::default(), 1.0 / 60.0);
        p.yaw = std::f32::consts::FRAC_PI_2; // vers +X : le bord est en x = 16
        let m = MoveInput {
            forward: 1.0,
            sneak: true,
            ..Default::default()
        };
        for _ in 0..240 {
            p.update(&w, &m, 1.0 / 60.0);
        }
        assert!(
            p.on_ground && (p.pos.y - 11.0).abs() < 0.01,
            "tombé : {:?}",
            p.pos
        );
        assert!(
            p.pos.x > 15.5 && p.pos.x < 16.0 + HALF_WIDTH,
            "x = {}",
            p.pos.x
        );
        assert!(p.eye().y < 11.0 + EYE - 0.25);
        // Sans s'accroupir, on tombe.
        let m = MoveInput {
            forward: 1.0,
            ..Default::default()
        };
        for _ in 0..120 {
            p.update(&w, &m, 1.0 / 60.0);
        }
        assert!(p.pos.y < 10.5, "y = {}", p.pos.y);
    }

    #[test]
    fn jump_reaches_one_block() {
        let w = flat_world();
        let mut p = Player::new(Vec3::new(8.5, 11.0, 8.5));
        p.update(&w, &MoveInput::default(), 1.0 / 60.0);
        let mut peak: f32 = 0.0;
        let jump = MoveInput {
            jump: true,
            ..Default::default()
        };
        p.update(&w, &jump, 1.0 / 60.0);
        for _ in 0..60 {
            p.update(&w, &MoveInput::default(), 1.0 / 60.0);
            peak = peak.max(p.pos.y - 11.0);
        }
        assert!(peak > 1.05 && peak < 1.6, "hauteur de saut = {peak}");
    }

    #[test]
    fn fps_speed_is_capped_even_when_jumping() {
        let w = flat_world();
        let mut p = Player::new(Vec3::new(8.5, 11.0, 8.5));
        p.update(&w, &MoveInput::default(), 1.0 / 60.0);
        let mut max: f32 = 0.0;
        for i in 0..240 {
            let m = MoveInput {
                forward: 1.0,
                strafe: 1.0,
                sprint: i < 120,
                jump: true,
                fps: true,
                ..Default::default()
            };
            p.update(&w, &m, 1.0 / 60.0);
            let h = Vec3::new(p.vel.x, 0.0, p.vel.z).length();
            let cap = if i < 120 { FPS_SPRINT } else { FPS_WALK };
            assert!(h <= cap + 1e-3, "vitesse {h} > {cap} (image {i})");
            max = max.max(h);
        }
        assert!(max > FPS_SPRINT * 0.95, "le sprint FPS doit être atteint");
    }
}
