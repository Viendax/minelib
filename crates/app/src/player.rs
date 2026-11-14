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
/// Mode FPS : marche (touche « Marcher ») à 62,5 % de la marche du mode Build (réduite de
/// 50 % puis augmentée de 25 %), course à 75 % du sprint.
pub const FPS_WALK: f32 = WALK * 0.5 * 1.25;
pub const FPS_SPRINT: f32 = SPRINT * 0.75;

/// Déplacements du mode FPS, façon Counter-Strike (moteur Source). Les valeurs sont les
/// réglages publics par défaut de CS:GO (`sv_accelerate`, `sv_friction`…) exprimés en
/// unités du moteur (1 unité = 1 pouce = 2,54 cm) puis converties en blocs (1 bloc = 1 m).
/// Les formules (friction, accélération au sol et en l'air) sont celles documentées par
/// la communauté (physique Quake/Source) ; le code est le nôtre.
pub mod cs {
    /// Blocs par unité Source.
    pub const UNIT: f32 = 0.0254;
    /// `sv_gravity` 800 u/s².
    pub const GRAVITY: f32 = 800.0 * UNIT;
    /// `sv_jump_impulse` 301,993 u/s : saut de 57 unités (1,45 bloc).
    pub const JUMP_IMPULSE: f32 = 301.993 * UNIT;
    /// `sv_accelerate` 5,5 et `sv_airaccelerate` 12.
    pub const ACCELERATE: f32 = 5.5;
    pub const AIR_ACCELERATE: f32 = 12.0;
    /// `sv_friction` 5,2 et `sv_stopspeed` 80 u/s. Nos vitesses de course étant plus
    /// basses que les 250 u/s de CS, `sv_stopspeed` garde la même proportion (32 % de la
    /// course) : sinon la friction l'emporterait sur l'accélération accroupi et on
    /// n'avancerait plus.
    pub const FRICTION: f32 = 5.2;
    pub const STOP_SPEED: f32 = super::FPS_SPRINT * 80.0 / 250.0;
    /// En l'air, la vitesse voulue est plafonnée à 30 u/s : c'est ce qui permet de
    /// gagner de la vitesse en tournant la souris du côté où l'on se décale (air strafe,
    /// bunny hop).
    pub const AIR_WISH_CAP: f32 = 30.0 * UNIT;
    /// Accroupi : 34 % de la vitesse de course.
    pub const DUCK_MULT: f32 = 0.34;
    /// Fatigue (`sv_staminamax` 80, `sv_staminajumpcost` 0,08, `sv_staminalandcost`
    /// 0,05, `sv_staminarecoveryrate` 60/s). Coûts proportionnels à la vitesse verticale
    /// en u/s ; la fatigue réduit le saut et ralentit au sol de `fatigue` %.
    pub const STAMINA_MAX: f32 = 80.0;
    pub const STAMINA_JUMP_COST: f32 = 0.08;
    pub const STAMINA_LAND_COST: f32 = 0.05;
    pub const STAMINA_RECOVERY: f32 = 60.0;
    /// Vitesse d'accroupissement : 8 au repos, −2 à chaque nouvel appui (accroupis
    /// répétés), reprise de 3 par seconde ; plus elle est basse, plus on s'accroupit
    /// lentement.
    pub const DUCK_SPEED_MAX: f32 = 8.0;
    pub const DUCK_SPEED_COST: f32 = 2.0;
    pub const DUCK_SPEED_MIN: f32 = 1.5;
    pub const DUCK_SPEED_RECOVERY: f32 = 3.0;
    /// Pas de temps du serveur de CS (64 ticks/s) : la fatigue au sol s'applique par tick.
    pub const TICK: f32 = 1.0 / 64.0;
}
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
    /// Mode FPS : fatigue des jambes (0..80, voir [`cs`]).
    pub stamina: f32,
    /// Mode FPS : vitesse d'accroupissement (8 = reposé).
    pub duck_speed: f32,
    /// Mode FPS : appui sur saut encore valable (s) ; il faut réappuyer pour ressauter.
    jump_buffer: f32,
    sneak_was_held: bool,
}

/// Intention de déplacement pour une image (indépendante du clavier).
#[derive(Default, Clone, Copy)]
pub struct MoveInput {
    pub forward: f32, // -1..1
    pub strafe: f32,  // -1..1 (positif = droite)
    pub up: f32,      // vol : -1..1
    pub jump: bool,
    pub sprint: bool,
    pub sneak: bool,
    /// Saut appuyé à cette image (pas seulement maintenu) : le mode FPS demande un nouvel
    /// appui pour chaque saut (espace ou molette, comme dans CS).
    pub jump_pressed: bool,
    /// Mode FPS : déplacements façon Counter-Strike (voir [`cs`]).
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
            stamina: 0.0,
            duck_speed: cs::DUCK_SPEED_MAX,
            jump_buffer: 0.0,
            sneak_was_held: false,
        }
    }

    /// État du déplacement pour le réseau (de quoi reprendre la simulation à l'identique).
    pub fn net_state(&self) -> minelib_net::game::SimState {
        minelib_net::game::SimState {
            pos: self.pos.to_array(),
            vel: self.vel.to_array(),
            on_ground: self.on_ground,
            crouch: self.crouch,
            stamina: self.stamina,
            duck_speed: self.duck_speed,
            jump_buffer: self.jump_buffer,
            sneak_held: self.sneak_was_held,
        }
    }

    pub fn set_net_state(&mut self, s: &minelib_net::game::SimState) {
        self.pos = Vec3::from(s.pos);
        self.vel = Vec3::from(s.vel);
        self.on_ground = s.on_ground;
        self.crouch = s.crouch;
        self.stamina = s.stamina;
        self.duck_speed = s.duck_speed;
        self.jump_buffer = s.jump_buffer;
        self.sneak_was_held = s.sneak_held;
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
        let fps = m.fps && !self.flying && !self.noclip;
        let duck_rate = if fps { self.duck_speed * 1.5 } else { 12.0 };
        let target = if crouching { 1.0 } else { 0.0 };
        self.crouch += (target - self.crouch) * (dt * duck_rate).min(1.0);

        if self.flying {
            let speed = if m.sprint { FLY_SPRINT } else { FLY };
            let target = wish * speed + Vec3::Y * m.up * speed;
            // Accélération/freinage doux en vol.
            self.vel = self.vel.lerp(target, (dt * 10.0).min(1.0));
        } else if fps {
            self.cs_move(m, wish, dt);
        } else {
            let speed = if crouching {
                SNEAK
            } else if m.sprint {
                SPRINT
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
        }
        let fall_speed = -self.vel.y;
        let crouching = crouching && !fps;

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
        // Atterrissage en mode FPS : les jambes encaissent selon la vitesse de chute.
        if fps && self.on_ground && !was_on_ground && fall_speed > 0.0 {
            self.stamina =
                (self.stamina + cs::STAMINA_LAND_COST * fall_speed / cs::UNIT).min(cs::STAMINA_MAX);
        }
    }

    /// Vitesse horizontale (blocs/s).
    pub fn ground_speed(&self) -> f32 {
        Vec3::new(self.vel.x, 0.0, self.vel.z).length()
    }

    /// Une image de déplacement façon Counter-Strike : saut (nouvel appui exigé),
    /// friction au sol sauf l'image du saut (bunny hop), accélération au sol, contrôle
    /// en l'air avec la vitesse voulue plafonnée (air strafe), fatigue des jambes.
    fn cs_move(&mut self, m: &MoveInput, wish: Vec3, dt: f32) {
        // Fatigue : récupération ; accroupis répétés.
        self.stamina = (self.stamina - cs::STAMINA_RECOVERY * dt).max(0.0);
        if m.sneak && !self.sneak_was_held {
            self.duck_speed = (self.duck_speed - cs::DUCK_SPEED_COST).max(cs::DUCK_SPEED_MIN);
        }
        self.sneak_was_held = m.sneak;
        self.duck_speed = (self.duck_speed + cs::DUCK_SPEED_RECOVERY * dt).min(cs::DUCK_SPEED_MAX);
        // Un appui sur saut reste valable un tick de CS (1/64 s) : sauter à l'atterrissage.
        if m.jump_pressed {
            self.jump_buffer = cs::TICK;
        } else {
            self.jump_buffer = (self.jump_buffer - dt).max(0.0);
        }

        // Accroupi : vitesse d'accroupi tout de suite (dès l'appui, sans attendre que
        // le corps soit descendu).
        let max_speed = if m.sneak {
            FPS_SPRINT * cs::DUCK_MULT
        } else if m.sprint {
            FPS_SPRINT
        } else {
            FPS_WALK
        };
        let wish_len = wish.length();
        let wish_dir = if wish_len > 1e-4 {
            wish / wish_len
        } else {
            Vec3::ZERO
        };
        let wish_speed = max_speed * wish_len.min(1.0);
        let rested = 1.0 - self.stamina / 100.0;

        if self.on_ground {
            if self.jump_buffer > 0.0 {
                // Saut : pas de friction à cette image (c'est ce qui permet le bunny hop).
                self.jump_buffer = 0.0;
                self.vel.y = cs::JUMP_IMPULSE * rested;
                self.stamina = (self.stamina + cs::STAMINA_JUMP_COST * cs::JUMP_IMPULSE / cs::UNIT)
                    .min(cs::STAMINA_MAX);
                self.on_ground = false;
                self.accelerate(
                    wish_dir,
                    wish_speed,
                    cs::AIR_ACCELERATE,
                    Some(cs::AIR_WISH_CAP),
                    dt,
                );
            } else {
                self.friction(dt);
                // Accroupi au sol : ralenti net à la vitesse d'accroupi.
                let speed = self.ground_speed();
                if m.sneak && speed > max_speed {
                    let k = max_speed / speed;
                    self.vel.x *= k;
                    self.vel.z *= k;
                }
                // Jambes fatiguées : ralenti au sol, tick après tick.
                if self.stamina > 0.0 {
                    let k = rested.powf(dt / cs::TICK);
                    self.vel.x *= k;
                    self.vel.z *= k;
                }
                self.accelerate(wish_dir, wish_speed * rested, cs::ACCELERATE, None, dt);
            }
        } else {
            self.accelerate(
                wish_dir,
                wish_speed,
                cs::AIR_ACCELERATE,
                Some(cs::AIR_WISH_CAP),
                dt,
            );
        }
        self.vel.y = (self.vel.y - cs::GRAVITY * dt).max(-3500.0 * cs::UNIT);
    }

    /// Friction au sol : au moins `sv_stopspeed` est retiré, pour s'arrêter net.
    fn friction(&mut self, dt: f32) {
        let speed = self.ground_speed();
        if speed < 0.1 * cs::UNIT {
            self.vel.x = 0.0;
            self.vel.z = 0.0;
            return;
        }
        let drop = speed.max(cs::STOP_SPEED) * cs::FRICTION * dt;
        let k = (speed - drop).max(0.0) / speed;
        self.vel.x *= k;
        self.vel.z *= k;
    }

    /// Accélération vers `dir` : seul ce qui manque pour atteindre la vitesse voulue
    /// (plafonnée à `cap` en l'air) dans cette direction est ajouté.
    fn accelerate(&mut self, dir: Vec3, wish_speed: f32, accel: f32, cap: Option<f32>, dt: f32) {
        if wish_speed <= 0.0 {
            return;
        }
        let target = cap.map_or(wish_speed, |c| wish_speed.min(c));
        let current = self.vel.x * dir.x + self.vel.z * dir.z;
        let add = target - current;
        if add <= 0.0 {
            return;
        }
        let gain = (accel * wish_speed * dt).min(add);
        self.vel.x += gain * dir.x;
        self.vel.z += gain * dir.z;
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

    const TICK: f32 = 1.0 / 128.0;

    fn fps_input() -> MoveInput {
        MoveInput {
            fps: true,
            ..Default::default()
        }
    }

    /// Saute (nouvel appui) et renvoie la hauteur atteinte.
    fn jump_peak(p: &mut Player, w: &World) -> f32 {
        let y0 = p.pos.y;
        let jump = MoveInput {
            jump: true,
            jump_pressed: true,
            ..fps_input()
        };
        p.update(w, &jump, TICK);
        let mut peak: f32 = 0.0;
        for _ in 0..400 {
            p.update(w, &fps_input(), TICK);
            peak = peak.max(p.pos.y - y0);
            if p.on_ground {
                break;
            }
        }
        peak
    }

    #[test]
    fn fps_ground_speed_and_jump_like_cs() {
        let w = flat_world();
        let mut p = Player::new(Vec3::new(8.5, 11.0, 8.5));
        let run = MoveInput {
            forward: 1.0,
            sprint: true,
            ..fps_input()
        };
        for _ in 0..512 {
            p.update(&w, &run, TICK);
            p.pos.x = 8.5;
            p.pos.z = 8.5;
            assert!(p.ground_speed() <= FPS_SPRINT + 1e-3);
        }
        assert!(p.ground_speed() > FPS_SPRINT * 0.99, "{}", p.ground_speed());
        // Friction : arrêt en moins d'une demi-seconde.
        for _ in 0..64 {
            p.update(&w, &fps_input(), TICK);
        }
        assert_eq!(p.ground_speed(), 0.0);
        // Saut de 57 unités (≈ 1,45 bloc) reposé.
        let h = jump_peak(&mut p, &w);
        assert!((1.40..1.50).contains(&h), "saut = {h}");
        // Garder la touche enfoncée ne ressaute pas : il faut réappuyer.
        let hold = MoveInput {
            jump: true,
            ..fps_input()
        };
        for _ in 0..256 {
            p.update(&w, &hold, TICK);
            assert!(p.on_ground, "saut sans nouvel appui");
        }
    }

    #[test]
    fn tired_legs_jump_lower_and_recover() {
        let w = flat_world();
        let mut p = Player::new(Vec3::new(8.5, 11.0, 8.5));
        for _ in 0..8 {
            p.update(&w, &fps_input(), TICK);
        }
        let fresh = jump_peak(&mut p, &w);
        // Ressauter tout de suite à l'atterrissage : plus bas, de plus en plus.
        let second = jump_peak(&mut p, &w);
        let third = jump_peak(&mut p, &w);
        assert!(second < fresh * 0.95, "{fresh} puis {second}");
        assert!(third < fresh * 0.95, "{fresh} puis {third}");
        assert!(p.stamina > 0.0);
        // Deux secondes de repos : les jambes récupèrent.
        for _ in 0..256 {
            p.update(&w, &fps_input(), TICK);
        }
        assert_eq!(p.stamina, 0.0);
        let rested = jump_peak(&mut p, &w);
        assert!((rested - fresh).abs() < 0.02, "{fresh} / {rested}");
    }

    #[test]
    fn bunny_hop_with_air_strafe_gains_speed_but_landing_slows() {
        let w = flat_world();
        let mut p = Player::new(Vec3::new(8.5, 11.0, 8.5));
        let run = MoveInput {
            forward: 1.0,
            sprint: true,
            ..fps_input()
        };
        for _ in 0..512 {
            p.update(&w, &run, TICK);
            p.pos.x = 8.5;
            p.pos.z = 8.5;
        }
        let start = p.ground_speed();
        assert!(start > FPS_SPRINT * 0.99);
        // Bunny hop : saut à chaque atterrissage, décalage à droite en tournant la souris
        // pour garder la direction voulue perpendiculaire à la vitesse.
        for _ in 0..(128 * 4) {
            p.yaw = p.vel.x.atan2(-p.vel.z);
            let m = MoveInput {
                strafe: 1.0,
                sprint: true,
                jump: true,
                jump_pressed: p.on_ground,
                ..fps_input()
            };
            p.update(&w, &m, TICK);
            // Large monde plat : on revient au centre pour rester sur le sol chargé.
            p.pos.x = 8.5;
            p.pos.z = 8.5;
        }
        let hopped = p.ground_speed();
        assert!(hopped > start * 1.3, "bunny hop : {start} → {hopped}");
        // Atterrir sans ressauter : fatigue + friction, la vitesse s'effondre.
        for _ in 0..200 {
            p.update(&w, &fps_input(), TICK);
            if p.on_ground {
                break;
            }
            p.pos.x = 8.5;
            p.pos.z = 8.5;
        }
        for _ in 0..16 {
            p.update(&w, &fps_input(), TICK);
            p.pos.x = 8.5;
            p.pos.z = 8.5;
        }
        assert!(p.ground_speed() < hopped * 0.5);
    }

    #[test]
    fn crouching_slows_down_at_once() {
        let w = flat_world();
        let mut p = Player::new(Vec3::new(8.5, 11.0, 8.5));
        let run = MoveInput {
            forward: 1.0,
            sprint: true,
            ..fps_input()
        };
        for _ in 0..256 {
            p.update(&w, &run, TICK);
            p.pos.x = 8.5;
            p.pos.z = 8.5;
        }
        let duck = MoveInput { sneak: true, ..run };
        p.update(&w, &duck, TICK);
        assert!(
            p.ground_speed() <= FPS_SPRINT * cs::DUCK_MULT + 1e-3,
            "{}",
            p.ground_speed()
        );
        // On continue d'avancer accroupi, à la vitesse d'accroupi.
        for _ in 0..256 {
            p.update(&w, &duck, TICK);
            p.pos.x = 8.5;
            p.pos.z = 8.5;
        }
        assert!(
            p.ground_speed() > FPS_SPRINT * cs::DUCK_MULT * 0.95,
            "accroupi immobile : {}",
            p.ground_speed()
        );
        // Et en repartant de l'arrêt.
        let mut q = Player::new(Vec3::new(8.5, 11.0, 8.5));
        for _ in 0..256 {
            q.update(&w, &duck, TICK);
            q.pos.x = 8.5;
            q.pos.z = 8.5;
        }
        assert!(q.ground_speed() > FPS_SPRINT * cs::DUCK_MULT * 0.95);
    }

    #[test]
    fn crouch_spam_tires_the_legs() {
        let w = flat_world();
        let mut p = Player::new(Vec3::new(8.5, 11.0, 8.5));
        for i in 0..8 {
            let m = MoveInput {
                sneak: i % 2 == 0,
                ..fps_input()
            };
            for _ in 0..4 {
                p.update(&w, &m, TICK);
            }
        }
        assert!(p.duck_speed < cs::DUCK_SPEED_MAX - 5.0, "{}", p.duck_speed);
        for _ in 0..(128 * 3) {
            p.update(&w, &fps_input(), TICK);
        }
        assert_eq!(p.duck_speed, cs::DUCK_SPEED_MAX);
    }
}
