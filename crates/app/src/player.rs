//! Joueur : caméra à la première personne, déplacements, gravité et collisions
//! boîte englobante / voxels (résolues axe par axe).

use glam::{Mat4, Vec3};
use minelib_world::registry::{fluid, post_arms, post_connects, Fluid, Shape};
use minelib_world::World;

pub const HALF_WIDTH: f32 = 0.3;

/// Le joueur dont les pieds sont en `feet` est dans l'eau (pieds ou corps) : elle amortit
/// toute chute.
pub fn in_water(world: &World, feet: Vec3) -> bool {
    in_fluid(world, feet) == Some(Fluid::Water)
}

/// Fluide (eau ou lave, source ou qui coule) où baigne le joueur (pieds ou corps).
pub fn in_fluid(world: &World, feet: Vec3) -> Option<Fluid> {
    let (x, z) = (feet.x.floor() as i32, feet.z.floor() as i32);
    [0.05, 0.9].iter().find_map(|dy| {
        let y = feet.y + dy;
        let id = world.get(x, y.floor() as i32, z);
        let (f, _) = fluid(id)?;
        // Fluide bas (qui coule) : seulement sous sa surface.
        let h = match id.def().shape {
            Shape::Fluid(h) => h as f32 / 16.0,
            _ => 1.0,
        };
        (y - y.floor() <= h).then_some(f)
    })
}

/// Nage (comme dans Minecraft) : lent dans l'eau, très lent dans la lave ; on coule
/// doucement, Saut fait remonter.
const SWIM_WATER: f32 = 2.2;
const SWIM_LAVA: f32 = 1.0;
const SWIM_UP: f32 = 2.4;
const SINK_WATER: f32 = -1.2;
const SINK_LAVA: f32 = -0.6;
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
/// Freinage quand aucune touche de déplacement n'est tenue (par seconde) : en vol, au sol
/// et en l'air (hors mode FPS, qui suit la friction de CS).
const FLY_BRAKE: f32 = 45.0;
const GROUND_BRAKE: f32 = 30.0;
const AIR_BRAKE: f32 = 9.0;
const FLY: f32 = 12.0;
const FLY_SPRINT: f32 = 30.0;
/// Tolérance de contact (erreurs d'arrondi) : en deçà, deux faces se touchent sans se
/// recouvrir.
const TOUCH: f32 = 1e-4;
/// Hauteur franchie sans sauter (dalles, marches d'escalier).
const STEP_HEIGHT: f32 = 0.6;
/// Vitesse sur une échelle : celle de CS (`MAX_CLIMB_SPEED` 200 u/s, soit 5,08 blocs/s,
/// dans les mêmes unités que la gravité et le saut).
const LADDER_SPEED: f32 = 200.0 * cs::UNIT;
/// Temps pour atteindre la pleine vitesse sur une échelle (s) : presque immédiat, comme
/// dans CS.
const LADDER_RAMP: f32 = 0.15;
/// Vitesse accroupi sur une échelle (blocs/s) : bien sous le seuil du bruit.
const LADDER_SNEAK_SPEED: f32 = 1.3;
/// Vitesse verticale gardée en quittant l'échelle par le haut (presque pas de saut).
const LADDER_TOP_POP: f32 = 1.0;

/// Vitesse verticale sur une échelle à partir de laquelle elle fait du bruit (à fond).
pub const LADDER_NOISY: f32 = LADDER_SPEED * 0.95;
/// Hauteur grimpée entre deux bruits d'échelle (un barreau sur deux).
pub const LADDER_RUNG: f32 = 0.5;

#[derive(Clone)]
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
    /// Mode FPS : vitesse permise par ce qu'on tient, en unités de CS (250 : couteau,
    /// 215 : AK-47, 200 : AWP, 100 : AWP à la lunette…). 0 : 250.
    pub carry: u8,
    /// Les autres joueurs, solides : on ne les traverse pas, on peut monter sur une tête.
    /// À remplir avant chaque pas (vide : personne).
    pub others: Vec<Other>,
    /// Atterri sur un joueur déjà perché sur un autre : on glisse de sa tête (son centre).
    slide: Option<Vec3>,
    /// Accroché à une échelle (pas de chute : on y monte et descend).
    pub on_ladder: bool,
    /// Fluide où l'on nage (eau, lave).
    pub swimming: Option<Fluid>,
}

/// Un autre joueur, vu comme un obstacle.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Other {
    /// Ses pieds.
    pub feet: Vec3,
    /// Il est lui-même debout sur la tête d'un joueur : personne ne tient sur la sienne.
    pub stacked: bool,
}

/// Obstacles que forment les joueurs aux pieds `feet` (les autres que soi, vivants).
pub fn others_from(feet: &[Vec3]) -> Vec<Other> {
    feet.iter()
        .enumerate()
        .map(|(i, &f)| Other {
            feet: f,
            stacked: feet.iter().enumerate().any(|(j, &g)| {
                j != i
                    && (f.y - (g.y + HEIGHT)).abs() < 0.05
                    && (f.x - g.x).abs() < 2.0 * HALF_WIDTH
                    && (f.z - g.z).abs() < 2.0 * HALF_WIDTH
            }),
        })
        .collect()
}

/// Vitesse à laquelle on glisse de la tête d'un joueur déjà perché (blocs/s).
const SLIDE: f32 = 3.5;

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
            others: Vec::new(),
            slide: None,
            on_ladder: false,
            swimming: None,
            carry: 250,
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
            carry: self.carry,
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
        if s.carry != 0 {
            self.carry = s.carry;
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

    /// Échelle (ou liane, même physique) touchée (côté du mur où elle est accrochée : 0 +X, 1 −X, 4 +Z, 5 −Z) : il
    /// faut être contre sa planche, pas seulement dans sa case.
    /// Renvoie aussi si l'on touche sa planche (on n'y monte que là) ; sinon on est
    /// seulement dans la moitié de sa case côté mur, où l'on ne tombe pas.
    fn ladder(&self, world: &World, fps: bool) -> Option<(u8, bool)> {
        let (min0, max0) = self.bounds();
        let mut held = None;
        // Moitié de la case côté mur (on y reste accroché, on peut y descendre).
        let lo = min0.floor().as_ivec3();
        let hi = (max0 - Vec3::splat(TOUCH)).floor().as_ivec3();
        for y in lo.y..=hi.y {
            for z in lo.z..=hi.z {
                for x in lo.x..=hi.x {
                    let def = world.get(x, y, z).def();
                    let Some(side) = climbable(&def.key, fps) else {
                        continue;
                    };
                    let Ok(side) = side.parse::<u8>() else {
                        continue;
                    };
                    let o = Vec3::new(x as f32, y as f32, z as f32);
                    let (a, b) = match side {
                        0 => (Vec3::new(0.5, 0.0, 0.0), Vec3::ONE),
                        1 => (Vec3::ZERO, Vec3::new(0.5, 1.0, 1.0)),
                        4 => (Vec3::new(0.0, 0.0, 0.5), Vec3::ONE),
                        _ => (Vec3::ZERO, Vec3::new(1.0, 1.0, 0.5)),
                    };
                    let (a, b) = (o + a, o + b);
                    if min0.x < b.x
                        && max0.x > a.x
                        && min0.y < b.y
                        && max0.y > a.y
                        && min0.z < b.z
                        && max0.z > a.z
                    {
                        held = Some(side);
                    }
                }
            }
        }
        let (min, max) = (min0, max0);
        let pad = Vec3::new(0.02, 0.0, 0.02);
        let (min, max) = (min - pad, max + pad);
        let lo = min.floor().as_ivec3();
        let hi = (max - Vec3::splat(TOUCH)).floor().as_ivec3();
        for y in lo.y..=hi.y {
            for z in lo.z..=hi.z {
                for x in lo.x..=hi.x {
                    let def = world.get(x, y, z).def();
                    let Some(side) = climbable(&def.key, fps) else {
                        continue;
                    };
                    let o = Vec3::new(x as f32, y as f32, z as f32);
                    let touches = def.boxes().iter().any(|(a, b)| {
                        let a = o + Vec3::new(a[0] as f32, a[1] as f32, a[2] as f32) / 16.0;
                        let b = o + Vec3::new(b[0] as f32, b[1] as f32, b[2] as f32) / 16.0;
                        min.x < b.x
                            && max.x > a.x
                            && min.y < b.y
                            && max.y > a.y
                            && min.z < b.z
                            && max.z > a.z
                    });
                    if touches {
                        return side.parse().ok().map(|s| (s, true));
                    }
                }
            }
        }
        held.map(|s| (s, false))
    }

    pub fn update(&mut self, world: &World, m: &MoveInput, dt: f32) {
        // Vitesse verticale d'avant ce pas (sur une échelle, la gravité n'y est pas).
        let vy0 = self.vel.y;
        let v0 = self.vel;
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
        let crouch_before = self.crouch;
        self.crouch += (target - self.crouch) * (dt * duck_rate).min(1.0);
        // Saut accroupi (mode FPS) : s'accroupir en l'air replie les jambes, les pieds
        // montent d'autant que la vue descend (la vue ne bouge pas) ; se relever en l'air
        // les redescend. Saut (1,45 bloc) + jambes repliées : on atteint 1,5 bloc.
        if fps && !self.on_ground {
            let lift = CROUCH_DROP * (self.crouch - crouch_before);
            self.move_axis(world, 1, lift);
            self.slide = None;
        }

        if self.flying {
            let speed = if m.sprint { FLY_SPRINT } else { FLY };
            let target = wish * speed + Vec3::Y * m.up * speed;
            // On prend de la vitesse en douceur, mais on s'arrête net : touche relâchée,
            // le vol ne continue pas sur son élan (à 30 blocs/s il glissait 3 blocs).
            for a in 0..3 {
                let k = if target[a] == 0.0 { FLY_BRAKE } else { 10.0 };
                self.vel[a] += (target[a] - self.vel[a]) * (dt * k).min(1.0);
                if target[a] == 0.0 && self.vel[a].abs() < 0.05 {
                    self.vel[a] = 0.0;
                }
            }
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
            // Touches relâchées : on s'arrête net (au sol comme en l'air), sans continuer
            // sur son élan.
            let idle = wish.length_squared() == 0.0;
            let control = match (self.on_ground, idle) {
                (true, false) => 14.0,
                (true, true) => GROUND_BRAKE,
                (false, false) => 3.0,
                (false, true) => AIR_BRAKE,
            };
            let k = (dt * control).min(1.0);
            self.vel.x += (target.x - self.vel.x) * k;
            self.vel.z += (target.z - self.vel.z) * k;
            if idle && self.vel.x.hypot(self.vel.z) < 0.05 {
                self.vel.x = 0.0;
                self.vel.z = 0.0;
            }
            self.vel.y -= GRAVITY * dt;
            self.vel.y = self.vel.y.max(-60.0);
            if m.jump && self.on_ground {
                self.vel.y = JUMP_SPEED;
            }
        }
        // Dans l'eau ou la lave : on nage.
        self.swimming = None;
        if !self.flying && !self.noclip {
            if let Some(f) = in_fluid(world, self.pos) {
                self.swimming = Some(f);
                let (cap, sink) = match f {
                    Fluid::Water => (SWIM_WATER * if m.sprint { 1.4 } else { 1.0 }, SINK_WATER),
                    Fluid::Lava => (SWIM_LAVA, SINK_LAVA),
                };
                let k = (dt * 6.0).min(1.0);
                let target = wish * cap;
                // Depuis la vitesse d'avant ce pas (sans l'accélération de la marche).
                self.vel.x = v0.x + (target.x - v0.x) * k;
                self.vel.z = v0.z + (target.z - v0.z) * k;
                let up = if m.jump { SWIM_UP } else { sink };
                // On garde l'élan d'un saut hors de l'eau quelques instants, mais une chute
                // est freinée aussitôt.
                // La gravité ne compte pas dans un fluide : on part de la vitesse d'avant.
                let k = (dt * if vy0 < up { 8.0 } else { 3.0 }).min(1.0);
                self.vel.y = vy0 + (up - vy0) * k;
            }
        }
        // Échelle (comme dans CS) : vers l'avant on monte, ou on descend en regardant vers
        // le bas ; sans rien, on reste accroché ; un saut nous en décroche.
        let was_on_ladder = self.on_ladder;
        self.on_ladder = false;
        if !self.flying && !self.noclip {
            let wall_of = |side: u8| match side {
                0 => Vec3::X,
                1 => -Vec3::X,
                4 => Vec3::Z,
                _ => -Vec3::Z,
            };
            // Sur l'échelle la vitesse verticale est nulle au repos : le sol se cherche juste
            // sous les pieds.
            let grounded = self.on_ground || self.support_within(world, 0.02);
            // Projeté loin de l'échelle (on vient d'en sauter) : elle ne nous rattrape pas.
            let ladder = self
                .ladder(world, fps)
                .filter(|&(side, _)| grounded || v0.dot(-wall_of(side)) < 1.5);
            if let Some((side, touching)) = ladder {
                let wall = wall_of(side);
                if m.jump_pressed {
                    // Sauter de l'échelle : projeté en arrière avec la force d'un saut (un peu
                    // vers le haut), rien ne nous retient.
                    let jump = if fps { cs::JUMP_IMPULSE } else { JUMP_SPEED };
                    self.vel = -wall * jump + Vec3::Y * jump * 0.35;
                } else {
                    self.on_ladder = true;
                    let down = self.pitch < -0.35;
                    let climb = if m.forward == 0.0 {
                        0.0
                    } else if down {
                        -m.forward.signum()
                    } else {
                        m.forward.signum()
                    };
                    // On ne monte que contre la planche ; à côté, on peut descendre.
                    let climb = if touching { climb } else { climb.min(0.0) };
                    // S'éloigner du mur (dos à l'échelle, on avance) : on descend en restant
                    // accroché jusqu'au sol, on ne se décolle pas de l'échelle en l'air.
                    // Au sol, on s'en va librement : on ne remonte pas l'échelle en
                    // s'en éloignant.
                    let away = wish.dot(-wall);
                    let leaving = away > 0.3;
                    let climb = match (leaving, grounded) {
                        (true, false) => -1.0,
                        (true, true) => 0.0,
                        _ => climb,
                    };
                    let free = grounded && climb <= 0.0;
                    // Accroupi : plus lent (et silencieux, voir `ladder_noise`).
                    let cap = if m.sneak {
                        LADDER_SNEAK_SPEED
                    } else {
                        LADDER_SPEED
                    };
                    let target = if climb < 0.0 && grounded {
                        0.0
                    } else {
                        climb * cap
                    };
                    // On prend de la vitesse : la pleine vitesse en LADDER_RAMP secondes.
                    let step = LADDER_SPEED / LADDER_RAMP * dt;
                    // En arrivant sur l'échelle en tombant : arrêté net ; en sautant dessus avec
                    // de l'élan : l'élan passe dans la montée.
                    let from = if was_on_ladder {
                        vy0.clamp(-cap, cap)
                    } else if !grounded && vy0 > 0.0 {
                        let run = Vec3::new(v0.x, 0.0, v0.z).length();
                        vy0.max(run).clamp(0.0, cap)
                    } else {
                        vy0.clamp(0.0, cap)
                    };
                    self.vel.y = from + (target - from).clamp(-step, step);
                    // Au sol : appuyé dessus (le pas suivant sait qu'on y est, et on y marche
                    // avec l'adhérence du sol).
                    if free {
                        self.vel.y = self.vel.y.min(-0.5);
                    }
                    // À l'horizontale, on suit les touches comme au sol (sans glisser : immobile
                    // sur l'échelle, on ne bouge pas), sans jamais s'écarter du mur en l'air.
                    let mut flat_wish = wish * cap;
                    if free {
                        // Au sol sans monter : on marche comme d'habitude (vitesse comprise).
                        flat_wish = Vec3::new(self.vel.x, 0.0, self.vel.z);
                    }
                    if !grounded {
                        flat_wish -= -wall * flat_wish.dot(-wall).max(0.0);
                    }
                    let k = (dt * 14.0).min(1.0);
                    self.vel.x += (flat_wish.x - self.vel.x) * k;
                    self.vel.z += (flat_wish.z - self.vel.z) * k;
                    if !grounded {
                        let out = Vec3::new(self.vel.x, 0.0, self.vel.z).dot(-wall).max(0.0);
                        self.vel -= -wall * out;
                    }
                    let flat = Vec3::new(self.vel.x, 0.0, self.vel.z);
                    if !free && flat.length() > cap {
                        let f = flat.normalize() * cap;
                        self.vel.x = f.x;
                        self.vel.z = f.z;
                    }
                }
            }
        }
        // Sortie par le haut de l'échelle : on arrive sur le palier sans être projeté en
        // l'air par la vitesse de montée.
        if was_on_ladder && !self.on_ladder && !m.jump_pressed && self.swimming.is_none() {
            self.vel.y = self.vel.y.min(LADDER_TOP_POP);
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
        // Mode FPS : en descendant d'une dalle (une marche d'au plus STEP_HEIGHT), on reste
        // collé au sol, sans chute ni atterrissage (sauf si on vient de sauter).
        if fps && was_on_ground && !self.on_ground && !self.flying && self.vel.y <= 0.0 {
            let saved = self.pos;
            if self.move_axis(world, 1, -STEP_HEIGHT) && self.slide.is_none() {
                self.on_ground = true;
                self.vel.y = 0.0;
            } else {
                self.pos = saved;
                self.slide = None;
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
        // Arme en main : même rapport que dans CS (vitesse de l'arme / 250).
        let carry = if self.carry == 0 {
            250.0
        } else {
            self.carry as f32
        } / 250.0;
        let max_speed = carry
            * if m.sneak {
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
                match self.slide.take() {
                    // Tête d'un joueur déjà perché : on n'y tient pas, on glisse du côté
                    // le plus proche du bord.
                    Some(c) => {
                        let off = self.pos - c;
                        let ax = if off.x.abs() >= off.z.abs() { 0 } else { 2 };
                        self.vel[ax] = if off[ax] < 0.0 { -SLIDE } else { SLIDE };
                    }
                    None => self.on_ground = true,
                }
            }
            self.vel.y = 0.0;
            return;
        }
        // Marche automatique : on remonte d'au plus STEP_HEIGHT (dalles, escaliers).
        if (was_on_ground || self.on_ground) && !self.flying {
            let blocked = self.pos;
            self.pos = before;
            // On monte autant que le plafond le permet (une marche basse passe sous un
            // plafond bas : il suffit qu'on tienne debout dessus).
            self.move_axis(world, 1, STEP_HEIGHT);
            let raised = self.pos.y - before.y;
            if raised > 1e-4 {
                self.move_axis(world, axis, d);
                if (self.pos[axis] - before[axis]).abs()
                    > (blocked[axis] - before[axis]).abs() + 1e-4
                {
                    self.move_axis(world, 1, -raised);
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
        self.support_within(world, 0.6)
    }

    /// Sol à moins de `d` sous les pieds.
    fn support_within(&mut self, world: &World, d: f32) -> bool {
        let saved = self.pos;
        let hit = self.move_axis(world, 1, -d);
        self.pos = saved;
        self.slide = None;
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
        // Les autres joueurs : mêmes règles que les blocs (on ne s'arrête que contre un
        // joueur qui était devant ; deux joueurs déjà l'un dans l'autre peuvent se séparer).
        self.slide = None;
        let mut on_player: Option<(f32, Other)> = None;
        for i in 0..self.others.len() {
            let o = self.others[i];
            let h = Vec3::new(HALF_WIDTH, 0.0, HALF_WIDTH);
            let (bmin, bmax) = (o.feet - h, o.feet + h + Vec3::Y * HEIGHT);
            let (pmin, pmax) = self.bounds();
            let side = (0..3)
                .all(|k| k == axis || (pmin[k] < bmax[k] - TOUCH && pmax[k] > bmin[k] + TOUCH));
            if !side {
                continue;
            }
            if d > 0.0 {
                if bmin[axis] >= bmax0[axis] - TOUCH && pmax[axis] > bmin[axis] {
                    self.pos[axis] = self.pos[axis].min(bmin[axis] - above);
                    hit = true;
                }
            } else if bmax[axis] <= bmin0[axis] + TOUCH && pmin[axis] < bmax[axis] {
                self.pos[axis] = self.pos[axis].max(bmax[axis] + below);
                hit = true;
                if axis == 1 && on_player.is_none_or(|(top, _)| bmax.y > top) {
                    on_player = Some((bmax.y, o));
                }
            }
        }
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
        // Posé sur la tête d'un joueur (et pas sur un bloc plus haut) qui est lui-même perché.
        if let Some((top, o)) = on_player {
            if o.stacked && (self.pos.y - top).abs() < TOUCH {
                self.slide = Some(o.feet);
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
    let mut boxes = minelib_world::registry::placed_boxes(world.get(x, y, z), |dx, dy, dz| {
        world.get(x + dx, y + dy, z + dz)
    });
    if let Shape::Post(kind) = def.shape {
        for (side, dx, dz) in [(0u8, 1, 0), (1, -1, 0), (4, 0, 1), (5, 0, -1)] {
            if post_connects(kind, world.get(x + dx, y, z + dz)) {
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

/// Échelle ou liane (`ladder:<côté>`, `vine:<côté>`) : côté du mur où elle est
/// accrochée. Les lianes se grimpent comme des échelles (hors mode FPS).
fn climbable(key: &str, fps: bool) -> Option<&str> {
    key.strip_prefix("ladder:")
        .or_else(|| key.strip_prefix("vine:").filter(|_| !fps))
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
    fn swims_in_water_and_lava_like_minecraft() {
        let mut w = flat_world();
        for y in 11..16 {
            for z in 0..16 {
                for x in 0..16 {
                    w.set(x, y, z, BlockId::WATER);
                }
            }
        }
        let dt = 1.0 / 64.0;
        // On tombe dedans : freiné, puis on coule doucement.
        let mut p = Player::new(Vec3::new(8.5, 15.0, 8.5));
        p.vel.y = -15.0;
        let none = MoveInput::default();
        for _ in 0..32 {
            p.update(&w, &none, dt);
        }
        assert_eq!(p.swimming, Some(Fluid::Water));
        assert!(
            p.vel.y > -1.5 && p.vel.y < 0.0,
            "coule doucement : {}",
            p.vel.y
        );
        // Saut tenu : on remonte.
        let y = p.pos.y;
        let jump = MoveInput {
            jump: true,
            ..Default::default()
        };
        for _ in 0..32 {
            p.update(&w, &jump, dt);
        }
        assert!(p.pos.y > y + 0.5, "remonte : {} → {}", y, p.pos.y);
        // Nage lente à l'horizontale.
        let fwd = MoveInput {
            forward: 1.0,
            ..Default::default()
        };
        for _ in 0..64 {
            p.update(&w, &fwd, dt);
        }
        let flat = Vec3::new(p.vel.x, 0.0, p.vel.z).length();
        assert!(flat < SWIM_WATER + 0.01, "{flat}");
        // Lave : plus lent encore.
        let lava = minelib_world::registry::registry().by_key("lava").unwrap();
        w.set(8, 11, 8, lava);
        w.set(8, 12, 8, lava);
        let mut p = Player::new(Vec3::new(8.5, 11.0, 8.5));
        p.update(&w, &none, dt);
        assert_eq!(p.swimming, Some(Fluid::Lava));
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
    fn fps_steps_down_half_blocks_without_falling() {
        // Mode FPS : en descendant une dalle, les pieds suivent le sol aussitôt (pas une
        // image en l'air) ; d'un bloc entier, on tombe.
        let reg = minelib_world::registry::registry();
        let slab = reg.by_key("slab:stone:bottom").unwrap();
        let mut w = flat_world();
        for z in 0..16 {
            w.set(8, 11, z, slab);
            w.set(9, 11, z, slab);
        }
        let mut p = Player::new(Vec3::new(8.5, 11.5, 8.5));
        p.yaw = std::f32::consts::FRAC_PI_2; // vers +X
        let m = MoveInput {
            forward: 1.0,
            sprint: true,
            fps: true,
            ..Default::default()
        };
        let dt = 1.0 / 128.0;
        for _ in 0..30 {
            p.update(
                &w,
                &MoveInput {
                    fps: true,
                    ..Default::default()
                },
                dt,
            );
        }
        assert!(p.on_ground && p.pos.y == 11.5);
        let mut stepped = false;
        for _ in 0..200 {
            p.update(&w, &m, dt);
            assert!(p.on_ground, "en l'air en x = {} (y = {})", p.pos.x, p.pos.y);
            if p.pos.x > 10.5 {
                stepped = true;
                break;
            }
        }
        assert!(stepped);
        assert_eq!(p.pos.y, 11.0, "descendu de la dalle");
    }

    #[test]
    fn steps_onto_a_low_step_under_a_low_ceiling() {
        // Plafond à 2 blocs du sol, marche basse et solide (≤ 3/16) à droite : il reste
        // assez de place pour un joueur de 1,8 : on y passe en marchant, sans sauter.
        let reg = minelib_world::registry::registry();
        let (low, top) = reg
            .defs
            .iter()
            .enumerate()
            .filter(|(_, d)| d.solid && !d.full)
            .filter_map(|(i, d)| {
                let b = d.boxes();
                let full = b
                    .iter()
                    .any(|(a, c)| a[0] == 0 && a[2] == 0 && c[0] == 16 && c[2] == 16);
                let h = b.iter().map(|(_, c)| c[1]).max()?;
                (full && b.iter().all(|(a, _)| a[1] == 0) && (1..=3).contains(&h))
                    .then_some((BlockId(i as u16), h as f32 / 16.0))
            })
            .next()
            .expect("une marche basse et solide");
        let mut w = flat_world();
        for z in 0..16 {
            for x in 0..16 {
                w.set(x, 13, z, BlockId::STONE);
            }
            for x in 10..16 {
                w.set(x, 11, z, low);
            }
        }
        let mut p = Player::new(Vec3::new(8.5, 11.0, 8.5));
        p.yaw = std::f32::consts::FRAC_PI_2; // vers +X
        let m = MoveInput {
            forward: 1.0,
            ..Default::default()
        };
        let dt = 1.0 / 144.0;
        for _ in 0..(2.0 / dt) as usize {
            p.update(&w, &m, dt);
            if p.pos.x > 12.0 {
                break;
            }
        }
        assert!(
            p.pos.x > 12.0,
            "bloqué en x = {} (y = {})",
            p.pos.x,
            p.pos.y
        );
        assert!((p.pos.y - (11.0 + top)).abs() < 1e-3, "y = {}", p.pos.y);
    }

    #[test]
    fn climbs_and_descends_ladders_like_cs() {
        let reg = minelib_world::registry::registry();
        let ladder = reg.by_key("ladder:0").unwrap();
        let mut w = flat_world();
        for y in 11..18 {
            for z in 0..16 {
                w.set(10, y, z, BlockId::STONE); // le mur
            }
            w.set(9, y, 8, ladder);
        }
        for fps in [false, true] {
            // Dans la moitié de la case côté mur, sans toucher la planche : on ne tombe pas,
            // et en tombant on s'y rattrape net.
            let mut p = Player::new(Vec3::new(9.25, 15.0, 8.5));
            p.vel.y = -12.0;
            let none = MoveInput {
                fps,
                ..Default::default()
            };
            p.update(&w, &none, 1.0 / 64.0);
            assert!(
                p.on_ladder && p.vel.y == 0.0,
                "rattrapé net (fps {fps}) : {}",
                p.vel.y
            );
            let y = p.pos.y;
            for _ in 0..64 {
                p.update(&w, &none, 1.0 / 64.0);
            }
            assert!((p.pos.y - y).abs() < 1e-3, "tenu sans toucher (fps {fps})");
            let mut p = Player::new(Vec3::new(9.4, 11.0, 8.5));
            p.yaw = std::f32::consts::FRAC_PI_2; // vers +X (le mur)
            let dt = 1.0 / 64.0;
            let still = MoveInput {
                fps,
                ..Default::default()
            };
            // Hors de la moitié de la case côté mur : pas accroché.
            let mut far = Player::new(Vec3::new(9.1, 11.0, 8.5));
            far.update(&w, &still, dt);
            assert!(!far.on_ladder, "trop loin (fps {fps})");
            let up = MoveInput {
                forward: 1.0,
                fps,
                ..Default::default()
            };
            // On prend de la vitesse en 0,15 s environ.
            for _ in 0..64 {
                p.update(&w, &up, dt);
                if p.vel.y > 0.0 {
                    break;
                }
            }
            p.update(&w, &up, dt);
            assert!(p.on_ladder, "fps {fps}");
            assert!(
                p.vel.y > 0.0 && p.vel.y < LADDER_NOISY,
                "pas tout de suite à fond : {}",
                p.vel.y
            );
            for _ in 0..26 {
                p.update(&w, &up, dt);
            }
            assert!(p.vel.y >= LADDER_NOISY, "à fond (fps {fps}) : {}", p.vel.y);
            assert!(p.pos.y > 12.2, "monte (fps {fps}) : y = {}", p.pos.y);
            // Rien : on ralentit puis on reste accroché.
            for _ in 0..40 {
                p.update(&w, &still, dt);
            }
            let y = p.pos.y;
            for _ in 0..64 {
                p.update(&w, &still, dt);
            }
            assert!((p.pos.y - y).abs() < 0.01, "accroché (fps {fps})");
            // Accroupi : on monte, mais sous la vitesse qui fait du bruit.
            let sneak = MoveInput {
                forward: 1.0,
                sneak: true,
                fps,
                ..Default::default()
            };
            let mut top = 0.0f32;
            for _ in 0..64 {
                p.update(&w, &sneak, dt);
                top = top.max(p.vel.y);
            }
            assert!(
                top > 0.5 && top < LADDER_NOISY,
                "accroupi (fps {fps}) : {top}"
            );
            // En regardant vers le bas : on descend jusqu'au sol, sans dégâts de chute.
            p.pitch = -1.0;
            for _ in 0..400 {
                p.update(&w, &up, dt);
            }
            assert!(
                (p.pos.y - 11.0).abs() < 1e-3,
                "descend (fps {fps}) : y = {}",
                p.pos.y
            );
            // Dos à l'échelle, on avance : on descend accroché jusqu'au sol, puis on part.
            let mut p = Player::new(Vec3::new(9.4, 15.0, 8.5));
            p.yaw = -std::f32::consts::FRAC_PI_2; // vers −X (dos au mur)
            p.update(&w, &still, dt);
            let mut left_in_air = false;
            for _ in 0..600 {
                p.update(&w, &up, dt);
                if p.pos.y > 11.001 && !p.on_ladder {
                    left_in_air = true;
                }
            }
            assert!(!left_in_air, "décollé de l'échelle en l'air (fps {fps})");
            assert!(
                p.pos.x < 9.0,
                "parti une fois au sol (fps {fps}) : x = {}",
                p.pos.x
            );
            // En haut de l'échelle à pleine vitesse : on arrive sur le mur sans s'envoler.
            let mut p = Player::new(Vec3::new(9.4, 14.0, 8.5));
            p.yaw = std::f32::consts::FRAC_PI_2;
            let mut top = 0.0f32;
            for _ in 0..400 {
                p.update(&w, &up, dt);
                top = top.max(p.pos.y);
            }
            assert!(
                p.pos.x > 10.0 && p.on_ground,
                "sur le mur (fps {fps}) : {:?}",
                p.pos
            );
            assert!(
                top < 18.2,
                "projeté en haut de l'échelle (fps {fps}) : {top}"
            );
            // Au sol contre l'échelle : on s'en éloigne sans effort, à vitesse normale.
            let mut p = Player::new(Vec3::new(9.4, 11.0, 8.5));
            p.yaw = -std::f32::consts::FRAC_PI_2;
            p.update(&w, &still, dt);
            for _ in 0..32 {
                p.update(&w, &up, dt);
            }
            assert!(p.on_ground && p.pos.y < 11.01, "reste au sol (fps {fps})");
            // Comme sans échelle (à 10 cm près).
            let mut q = Player::new(Vec3::new(5.4, 11.0, 4.5));
            q.yaw = -std::f32::consts::FRAC_PI_2;
            q.update(&w, &still, dt);
            for _ in 0..32 {
                q.update(&w, &up, dt);
            }
            assert!(
                (9.4 - p.pos.x) > (5.4 - q.pos.x) - 0.1,
                "s'éloigne (fps {fps}) : {} au lieu de {}",
                9.4 - p.pos.x,
                5.4 - q.pos.x
            );
            // Saut sur l'échelle avec de l'élan : l'élan passe dans la montée.
            let mut p = Player::new(Vec3::new(9.35, 13.0, 8.5));
            p.yaw = std::f32::consts::FRAC_PI_2;
            p.vel = Vec3::new(4.0, 2.0, 0.0);
            p.update(&w, &up, dt);
            assert!(
                p.on_ladder && p.vel.y > 2.5,
                "élan transféré (fps {fps}) : {}",
                p.vel.y
            );
            // Saut depuis l'échelle : projeté en arrière, à la vitesse d'un saut.
            let jump = MoveInput {
                jump_pressed: true,
                fps,
                ..Default::default()
            };
            p.update(&w, &jump, dt);
            assert!(
                !p.on_ladder && p.vel.x < -5.0 && p.vel.y > 0.0 && p.vel.y < -p.vel.x,
                "saut arrière (fps {fps}) : {:?}",
                p.vel
            );
            // Rien ne nous retient : l'échelle ne nous rattrape pas pendant qu'on s'en va.
            let x0 = p.pos.x;
            for _ in 0..10 {
                p.update(&w, &still, dt);
                assert!(!p.on_ladder, "repris par l'échelle (fps {fps})");
            }
            assert!(
                p.pos.x < x0 - 0.5,
                "projeté en arrière (fps {fps}) : {}",
                p.pos.x
            );
            // Immobile sur l'échelle : aucune glissade de côté.
            let mut p = Player::new(Vec3::new(9.4, 14.0, 8.5));
            p.update(&w, &still, dt);
            p.vel.z = 3.0;
            for _ in 0..64 {
                p.update(&w, &still, dt);
            }
            let z = p.pos.z;
            for _ in 0..64 {
                p.update(&w, &still, dt);
            }
            assert!((p.pos.z - z).abs() < 1e-3, "glisse de côté (fps {fps})");
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

    const TICK: f32 = crate::session::SIM_TICK;

    /// `n` pas de 1/128 s, convertis en pas du jeu (la durée reste la même).
    fn ticks(n: usize) -> usize {
        (n as f32 / 128.0 / TICK).round() as usize
    }

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
        for _ in 0..ticks(400) {
            p.update(w, &fps_input(), TICK);
            peak = peak.max(p.pos.y - y0);
            if p.on_ground {
                break;
            }
        }
        peak
    }

    /// Saut accroupi : saut, puis accroupi maintenu en l'air. Les pieds passent 1,5 bloc
    /// (pas sans s'accroupir), et on se pose sur un obstacle de 1,5 bloc.
    #[test]
    fn crouch_jump_reaches_one_and_a_half_blocks() {
        let w = flat_world();
        let mut p = Player::new(Vec3::new(8.5, 11.0, 8.5));
        for _ in 0..ticks(64) {
            p.update(&w, &fps_input(), TICK);
        }
        let plain = jump_peak(&mut p, &w);
        assert!(plain < 1.5, "saut simple : {plain}");
        for _ in 0..ticks(512) {
            p.update(&w, &fps_input(), TICK);
        }
        let y0 = p.pos.y;
        let jump = MoveInput {
            jump: true,
            jump_pressed: true,
            ..fps_input()
        };
        let duck = MoveInput {
            sneak: true,
            jump: true,
            ..fps_input()
        };
        p.update(&w, &jump, TICK);
        let mut peak: f32 = 0.0;
        for _ in 0..ticks(400) {
            p.update(&w, &duck, TICK);
            peak = peak.max(p.pos.y - y0);
            if p.on_ground {
                break;
            }
        }
        assert!(peak > 1.55 && peak < 1.85, "saut accroupi : {peak}");
        assert!((p.pos.y - y0).abs() < 1e-3, "retombé au sol");
        // Sur un mur de 1,5 bloc (un bloc et une dalle), en avançant.
        let mut w = flat_world();
        for z in 6..11 {
            w.set(11, 11, z, BlockId::STONE);
            let slab = minelib_world::registry::registry()
                .by_key("slab:stone:bottom")
                .unwrap();
            w.set(11, 12, z, slab);
        }
        let mut p = Player::new(Vec3::new(10.69, 11.0, 8.5));
        p.yaw = std::f32::consts::FRAC_PI_2;
        for _ in 0..ticks(64) {
            p.update(&w, &fps_input(), TICK);
        }
        let fwd = |m: MoveInput| MoveInput { forward: 1.0, ..m };
        p.update(&w, &fwd(jump), TICK);
        let mut on_wall = false;
        for _ in 0..ticks(300) {
            p.update(&w, &fwd(duck), TICK);
            on_wall |= p.on_ground && (p.pos.y - 12.5).abs() < 1e-3;
        }
        assert!(on_wall, "monté sur le mur : {:?}", p.pos);
    }

    /// Les joueurs sont solides entre eux : on ne se traverse pas, on monte sur une tête,
    /// mais un troisième glisse de la tête de celui qui est déjà perché.
    #[test]
    fn players_collide_stack_and_the_third_slides_off() {
        let w = flat_world();
        let a = Vec3::new(8.5, 11.0, 8.5);
        // On marche vers A : arrêté contre lui.
        let mut p = Player::new(Vec3::new(6.5, 11.0, 8.5));
        p.yaw = std::f32::consts::FRAC_PI_2;
        p.others = others_from(&[a]);
        let run = MoveInput {
            forward: 1.0,
            sprint: true,
            ..fps_input()
        };
        for _ in 0..ticks(512) {
            p.update(&w, &run, TICK);
        }
        assert!(
            (p.pos.x - (a.x - 2.0 * HALF_WIDTH)).abs() < 1e-3,
            "{:?}",
            p.pos
        );
        // B tombe sur la tête de A : il y tient.
        let mut b = Player::new(a + Vec3::Y * 3.0);
        b.others = others_from(&[a]);
        for _ in 0..ticks(256) {
            b.update(&w, &fps_input(), TICK);
        }
        assert!(
            b.on_ground && (b.pos.y - (a.y + HEIGHT)).abs() < 1e-3,
            "{:?}",
            b.pos
        );
        // C tombe sur la tête de B (perché sur A), un peu décalé : il glisse et finit au sol,
        // du côté où il était.
        let mut c = Player::new(b.pos + Vec3::new(0.1, 3.0, 0.0));
        c.others = others_from(&[a, b.pos]);
        assert!(c.others[1].stacked && !c.others[0].stacked);
        for _ in 0..ticks(512) {
            c.update(&w, &fps_input(), TICK);
        }
        assert!(
            c.on_ground && (c.pos.y - 11.0).abs() < 1e-3,
            "au sol : {:?}",
            c.pos
        );
        assert!(
            c.pos.x > a.x + 2.0 * HALF_WIDTH - 1e-3,
            "du côté le plus proche"
        );
        // Deux joueurs l'un dans l'autre (même point d'apparition) peuvent se séparer.
        let mut d = Player::new(a + Vec3::new(0.1, 0.0, 0.0));
        d.yaw = std::f32::consts::FRAC_PI_2;
        d.others = others_from(&[a]);
        for _ in 0..ticks(256) {
            d.update(&w, &run, TICK);
        }
        assert!(d.pos.x > a.x + 1.0, "{:?}", d.pos);
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
        for _ in 0..ticks(512) {
            p.update(&w, &run, TICK);
            p.pos.x = 8.5;
            p.pos.z = 8.5;
            assert!(p.ground_speed() <= FPS_SPRINT + 1e-3);
        }
        assert!(p.ground_speed() > FPS_SPRINT * 0.99, "{}", p.ground_speed());
        // Friction : arrêt en moins d'une demi-seconde.
        for _ in 0..ticks(64) {
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
        for _ in 0..ticks(256) {
            p.update(&w, &hold, TICK);
            assert!(p.on_ground, "saut sans nouvel appui");
        }
    }

    #[test]
    fn tired_legs_jump_lower_and_recover() {
        let w = flat_world();
        let mut p = Player::new(Vec3::new(8.5, 11.0, 8.5));
        for _ in 0..ticks(8) {
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
        for _ in 0..ticks(256) {
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
        for _ in 0..ticks(512) {
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
        for _ in 0..ticks(200) {
            p.update(&w, &fps_input(), TICK);
            if p.on_ground {
                break;
            }
            p.pos.x = 8.5;
            p.pos.z = 8.5;
        }
        for _ in 0..ticks(16) {
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
        for _ in 0..ticks(256) {
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
        for _ in 0..ticks(256) {
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
        for _ in 0..ticks(256) {
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
            for _ in 0..ticks(4) {
                p.update(&w, &m, TICK);
            }
        }
        assert!(p.duck_speed < cs::DUCK_SPEED_MAX - 5.0, "{}", p.duck_speed);
        for _ in 0..ticks(384) {
            p.update(&w, &fps_input(), TICK);
        }
        assert_eq!(p.duck_speed, cs::DUCK_SPEED_MAX);
    }
}

#[cfg(test)]
mod stop_tests {
    use super::*;

    /// Touche d'avance tenue longtemps puis relâchée : le joueur s'arrête tout de suite
    /// (moins d'un demi-bloc de glisse, à l'arrêt en moins de 0,4 s), en FPS comme en
    /// construction.
    #[test]
    fn flight_and_build_walking_stop_dead_when_the_keys_are_released() {
        let mut w = World::new(0);
        for x in -2..2 {
            for z in -80..4 {
                w.insert_empty(minelib_world::ChunkPos { x, z });
            }
        }
        for x in -20..20 {
            for z in -1200..40 {
                w.set(x, 60, z, minelib_world::BlockId::STONE);
            }
        }
        let dt = crate::session::SIM_TICK;
        // (vol, saut tenu, glissade permise en blocs)
        for (flying, jump, slack) in [(true, false, 1.0), (false, false, 0.3), (false, true, 1.0)] {
            let mut p = Player::new(Vec3::new(0.5, if flying { 70.0 } else { 61.0 }, 0.5));
            p.pitch = 0.0;
            p.flying = flying;
            let go = MoveInput {
                forward: 1.0,
                sprint: true,
                jump,
                ..Default::default()
            };
            let stop = MoveInput {
                jump,
                ..Default::default()
            };
            for _ in 0..(256 * 30) {
                p.update(&w, &go, dt);
            }
            let (at, speed) = (p.pos, p.ground_speed());
            assert!(
                speed > if flying { 29.0 } else { 6.0 },
                "pleine vitesse : {speed}"
            );
            let mut stopped = None;
            for i in 0..256 {
                p.update(&w, &stop, dt);
                if stopped.is_none() && p.ground_speed() < 0.05 {
                    stopped = Some(i as f32 * dt);
                }
            }
            let slid = Vec3::new(p.pos.x - at.x, 0.0, p.pos.z - at.z).length();
            assert!(
                slid < slack && stopped.is_some_and(|t| t < 0.6),
                "vol {flying}, saut {jump} : glisse {slid} blocs, arrêt {stopped:?}"
            );
        }
    }

    #[test]
    fn the_player_stops_right_after_a_long_run() {
        let mut w = World::new(0);
        // (Le joueur regarde vers −Z.)
        for x in -2..2 {
            for z in -40..4 {
                w.insert_empty(minelib_world::ChunkPos { x, z });
            }
        }
        for x in -20..20 {
            for z in -600..40 {
                w.set(x, 60, z, minelib_world::BlockId::STONE);
            }
        }
        for fps in [true, false] {
            let mut p = Player::new(Vec3::new(0.5, 61.0, 0.5));
            p.pitch = 0.0;
            let dt = 1.0 / 128.0;
            let go = MoveInput {
                forward: 1.0,
                fps,
                ..Default::default()
            };
            let stop = MoveInput {
                fps,
                ..Default::default()
            };
            for _ in 0..(128 * 20) {
                p.update(&w, &go, dt);
            }
            let at = p.pos;
            let mut stopped = None;
            for i in 0..(128 * 3) {
                p.update(&w, &stop, dt);
                if stopped.is_none() && Vec3::new(p.vel.x, 0.0, p.vel.z).length() < 0.05 {
                    stopped = Some(i as f32 * dt);
                }
            }
            assert!(p.on_ground && at.z < -40.0, "il a bien couru : {at:?}");
            assert!(stopped.is_some_and(|t| t < 0.4), "fps {fps} : {stopped:?}");
            assert!((p.pos - at).length() < 0.5, "fps {fps}");
        }
    }
}
