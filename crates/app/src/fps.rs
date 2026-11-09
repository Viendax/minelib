//! Mode FPS : 100 points de vie, armes (achat, tir, recul), balles traçantes et impacts.
//!
//! - **Achat** (touche B par défaut) : toutes les armes, avec leur prix. En solo il n'y a pas
//!   d'argent : le solde reste à 0 $ et les achats sont gratuits.
//! - **Tir** : clic gauche (maintenu pour les armes automatiques), R recharge, 1 / 2 arme
//!   principale / pistolet, clic droit : lunette des fusils qui en ont une.
//! - **Recul** : chaque arme suit un motif fixe (montée puis balayage gauche / droite), qui
//!   se retrouve à l'écran (la vue remonte) et dans la direction des balles ; il se remet à
//!   zéro quand on arrête de tirer. Imprécision plus forte en courant et en l'air.
//! - **Balles** : traçante lumineuse du canon jusqu'au point touché, étincelles à l'impact,
//!   impact noir sur le bloc (les 64 derniers).

use glam::{Mat4, Vec3};
use minelib_platform::Input;
use minelib_render::{Ui, VmVertex, NO_TEXTURE};
use minelib_world::raycast::raycast;
use minelib_world::World;

use crate::weapons::{self, Category, WEAPONS};

/// Facteur appliqué aux dégâts de chute en mode FPS.
const FALL_SCALE: f32 = 0.25;
pub const MAX_HP: u32 = 100;
const RANGE: f32 = 250.0;
const BULLET_SPEED: f32 = 300.0;
const TRACER_LEN: f32 = 3.0;
const MAX_HOLES: usize = 64;
/// Champ de vision de la main et de l'arme (comme la main du mode bloc).
const VIEW_FOV: f32 = 70.0;

#[derive(Clone, Copy)]
pub struct Gun {
    pub def: usize,
    pub mag: u32,
    pub reserve: u32,
}

impl Gun {
    fn new(def: usize) -> Self {
        let d = &WEAPONS[def];
        Self {
            def,
            mag: d.magazine,
            reserve: d.reserve,
        }
    }
}

struct Tracer {
    from: Vec3,
    dir: Vec3,
    dist: f32,
    travelled: f32,
}

/// Entrées de tir pour une image.
#[derive(Default)]
pub struct Controls {
    pub trigger_pressed: bool,
    pub trigger_held: bool,
    pub reload: bool,
    pub scope: bool,
    pub slot: Option<usize>,
    /// Vitesse horizontale (m/s) et au sol ou non.
    pub speed: f32,
    pub on_ground: bool,
}

pub struct Fps {
    pub hp: u32,
    /// Solde : 0 en solo (les achats ne le changent pas).
    pub money: u32,
    primary: Option<Gun>,
    secondary: Option<Gun>,
    use_primary: bool,
    cooldown: f32,
    reload: Option<f32>,
    /// Balles tirées dans la rafale en cours (motif de recul), et temps depuis le dernier tir.
    spray_n: u32,
    since_shot: f32,
    /// Recul de la vue (degrés) : vers le haut, vers la droite.
    pub punch: (f32, f32),
    /// Lunette : 0 sans, 1 grossie (une seule position).
    pub scoped: bool,
    tracers: Vec<Tracer>,
    holes: Vec<(Vec3, Vec3)>,
    sparks: Vec<(Vec3, Vec3, f32)>,
    flash: f32,
    kick: f32,
    pub buy_open: bool,
    rng: u32,
}

impl Default for Fps {
    fn default() -> Self {
        Self::new()
    }
}

impl Fps {
    pub fn new() -> Self {
        Self {
            hp: MAX_HP,
            money: 0,
            primary: None,
            secondary: Some(Gun::new(weapons::STARTER)),
            use_primary: false,
            cooldown: 0.0,
            reload: None,
            spray_n: 0,
            since_shot: 10.0,
            punch: (0.0, 0.0),
            scoped: false,
            tracers: Vec::new(),
            holes: Vec::new(),
            sparks: Vec::new(),
            flash: 0.0,
            kick: 0.0,
            buy_open: false,
            rng: 0x9e37_79b9,
        }
    }

    fn active(&self) -> Option<&Gun> {
        if self.use_primary {
            self.primary.as_ref().or(self.secondary.as_ref())
        } else {
            self.secondary.as_ref().or(self.primary.as_ref())
        }
    }

    fn active_mut(&mut self) -> Option<&mut Gun> {
        if self.use_primary && self.primary.is_some() {
            self.primary.as_mut()
        } else if self.secondary.is_some() {
            self.secondary.as_mut()
        } else {
            self.primary.as_mut()
        }
    }

    /// Champ de vision imposé par la lunette, le cas échéant.
    pub fn scope_fov(&self) -> Option<f32> {
        let g = self.active()?;
        WEAPONS[g.def]
            .scope
            .filter(|_| self.scoped && self.reload.is_none())
    }

    /// Achète l'arme `i` (gratuit en solo : le solde n'est pas débité) et la prend en main.
    pub fn buy(&mut self, i: usize) {
        let gun = Some(Gun::new(i));
        if WEAPONS[i].primary() {
            self.primary = gun;
            self.use_primary = true;
        } else {
            self.secondary = gun;
            self.use_primary = false;
        }
        self.reload = None;
        self.scoped = false;
        self.kick = 0.0;
    }

    /// Mort ou nouvelle partie : vie pleine, pistolet de départ.
    pub fn respawn(&mut self) {
        let money = self.money;
        *self = Self::new();
        self.money = money;
    }

    /// Dégâts de chute façon FPS tactique : rien jusqu'à `safe` blocs, mortel vers 3 fois
    /// cette hauteur ; entre les deux, proportionnel à la vitesse d'impact.
    pub fn fall_damage(height: f32, safe: f32) -> u32 {
        if height <= safe {
            return 0;
        }
        let fatal = safe * 3.12;
        let k = (height.sqrt() - safe.sqrt()) / (fatal.sqrt() - safe.sqrt());
        // Dégâts divisés par 4 par rapport à la courbe de base.
        (k * MAX_HP as f32 * FALL_SCALE)
            .ceil()
            .clamp(0.0, MAX_HP as f32) as u32
    }

    fn random(&mut self) -> f32 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 17;
        self.rng ^= self.rng << 5;
        (self.rng as f32 / u32::MAX as f32) * 2.0 - 1.0
    }

    /// Une image de jeu : tir, rechargement, recul, effets. `eye` / `yaw` / `pitch` : vue
    /// du joueur (sans le recul).
    pub fn update(
        &mut self,
        dt: f32,
        c: &Controls,
        world: &World,
        eye: Vec3,
        yaw: f32,
        pitch: f32,
    ) {
        // Peut devenir négatif : à faible nombre d'images par seconde, plusieurs balles
        // partent dans la même image et la cadence de l'arme est tenue.
        self.cooldown -= dt;
        self.since_shot += dt;
        self.flash = (self.flash - dt).max(0.0);
        self.kick = (self.kick - dt * 6.0).max(0.0);
        if let Some(i) = c.slot {
            let want_primary = i == 0;
            if want_primary != self.use_primary {
                self.use_primary = want_primary;
                self.reload = None;
                self.scoped = false;
            }
        }
        // Rechargement.
        if let Some(t) = &mut self.reload {
            *t -= dt;
            if *t <= 0.0 {
                self.reload = None;
                if let Some(g) = self.active_mut() {
                    let need = WEAPONS[g.def].magazine - g.mag;
                    let n = need.min(g.reserve);
                    g.mag += n;
                    g.reserve -= n;
                }
            }
        }
        let Some(gun) = self.active().copied() else {
            return;
        };
        let def = &WEAPONS[gun.def];
        if c.reload && self.reload.is_none() && gun.mag < def.magazine && gun.reserve > 0 {
            self.reload = Some(def.reload_s);
            self.scoped = false;
        }
        if c.scope && def.scope.is_some() {
            self.scoped = !self.scoped;
        }
        // Rafale finie : le motif repart du début, la vue revient doucement.
        if self.since_shot > def.interval() * 1.8 + 0.12 {
            self.spray_n = 0;
        }
        let back = (dt * 9.0).min(1.0);
        if self.since_shot > def.interval() * 1.2 {
            self.punch.0 -= self.punch.0 * back;
            self.punch.1 -= self.punch.1 * back;
        }

        let wants = if def.auto {
            c.trigger_held
        } else {
            c.trigger_pressed
        };
        if !wants || self.reload.is_some() {
            self.cooldown = self.cooldown.max(0.0);
            return;
        }
        // Au plus quelques balles par image (si l'image a été très longue).
        let mut shots = 0;
        while self.cooldown <= 0.0 && shots < 8 {
            let mag = self.active().map_or(0, |g| g.mag);
            if mag == 0 {
                if self.active().is_some_and(|g| g.reserve > 0) {
                    self.reload = Some(def.reload_s);
                    self.scoped = false;
                }
                self.cooldown = self.cooldown.max(0.0);
                return;
            }
            self.fire(c, world, eye, yaw, pitch);
            self.cooldown += def.interval();
            shots += 1;
            if !def.auto {
                break;
            }
        }
        self.cooldown = self.cooldown.max(-def.interval());
    }

    /// Un tir : munition, recul (motif), imprécision, balles traçantes, impacts.
    fn fire(&mut self, c: &Controls, world: &World, eye: Vec3, yaw: f32, pitch: f32) {
        let Some(gun) = self.active().copied() else {
            return;
        };
        let def = &WEAPONS[gun.def];
        if let Some(g) = self.active_mut() {
            g.mag -= 1;
        }
        self.since_shot = 0.0;
        self.flash = 0.05;
        self.kick = 1.0;
        // Recul : la balle suit le motif ; la vue en montre environ la moitié.
        let (up, side) = def.spray(self.spray_n);
        self.spray_n += 1;
        let (nup, nside) = def.spray(self.spray_n);
        self.punch = (nup * 0.5, nside * 0.5);
        // Imprécision : plus forte en courant et en l'air ; la lunette la réduit.
        let mut spread = def.spread * (1.0 + (c.speed / crate::player::FPS_WALK).min(1.5) * 2.0);
        if !c.on_ground {
            spread *= 2.5;
        }
        if self.scoped {
            spread *= 0.25;
        }
        let muzzle = self.muzzle_world(eye, yaw, pitch);
        for _ in 0..def.pellets.max(1) {
            let r = spread.to_radians() * self.random().abs().sqrt();
            let a = self.random() * std::f32::consts::PI;
            let p = pitch + up.to_radians() + r * a.sin();
            let y = yaw + side.to_radians() + r * a.cos();
            let dir = Vec3::new(y.sin() * p.cos(), p.sin(), -y.cos() * p.cos());
            let (dist, hit) = match raycast(world, eye.into(), dir.into(), RANGE) {
                Some(h) => (h.distance, Some(h)),
                None => (RANGE, None),
            };
            let target = eye + dir * dist;
            let tdir = (target - muzzle).normalize_or_zero();
            self.tracers.push(Tracer {
                from: muzzle,
                dir: tdir,
                dist: (target - muzzle).length(),
                travelled: 0.0,
            });
            if let Some(h) = hit {
                let n = Vec3::new(h.normal[0] as f32, h.normal[1] as f32, h.normal[2] as f32);
                if self.holes.len() >= MAX_HOLES {
                    self.holes.remove(0);
                }
                self.holes.push((target + n * 0.004, n));
                for _ in 0..5 {
                    let v = n * 2.0 + Vec3::new(self.random(), self.random(), self.random()) * 2.5;
                    self.sparks.push((target + n * 0.02, v, 0.0));
                }
            }
        }
    }

    /// Avance les balles et les étincelles (appelé à chaque image).
    pub fn tick_effects(&mut self, dt: f32) {
        for t in &mut self.tracers {
            t.travelled += BULLET_SPEED * dt;
        }
        self.tracers.retain(|t| t.travelled - TRACER_LEN < t.dist);
        for s in &mut self.sparks {
            s.2 += dt;
            s.1.y -= 12.0 * dt;
            s.0 += s.1 * dt;
        }
        self.sparks.retain(|s| s.2 < 0.35);
    }

    /// Position du bout du canon dans le monde (départ des balles traçantes).
    fn muzzle_world(&self, eye: Vec3, yaw: f32, pitch: f32) -> Vec3 {
        let Some(g) = self.active() else { return eye };
        let m = weapons::model(g.def);
        let cam = Mat4::from_rotation_y(-yaw) * Mat4::from_rotation_x(pitch);
        let local = self.gun_place(g.def) * m.muzzle.extend(1.0);
        eye + cam.transform_vector3(local.truncate())
    }

    /// Placement de l'arme dans le repère de la vue (droite, bas, devant).
    fn gun_place(&self, def: usize) -> Mat4 {
        let d = &WEAPONS[def];
        let reload = self
            .reload
            .map_or(0.0, |t| (t / d.reload_s).clamp(0.0, 1.0));
        let dip = (reload * std::f32::consts::PI).sin();
        let k = self.kick;
        let len = d.length;
        let scoped = self.scope_fov().is_some();
        // Arme en bas à droite, l'arrière à ~25 cm de l'œil, le canon vers le centre.
        let (x, y) = if scoped {
            (0.0, -0.08)
        } else {
            (0.2 + len * 0.04, -0.2 - len * 0.03)
        };
        Mat4::from_translation(Vec3::new(x, y - dip * 0.12, -(0.26 + len * 0.5) + k * 0.03))
            * Mat4::from_rotation_x(k * 0.09 - dip * 0.6)
            * Mat4::from_rotation_y(if scoped { 0.0 } else { 0.04 })
    }

    /// Arme en main (triangles projetés), éclairée par `light` (couleur linéaire).
    pub fn viewmodel(&self, aspect: f32, light: [f32; 3]) -> Vec<VmVertex> {
        let mut out = Vec::new();
        let Some(g) = self.active() else { return out };
        // Lunette : l'arme disparaît (on regarde dans la lunette).
        if self.scope_fov().is_some() {
            return out;
        }
        let proj = Mat4::perspective_infinite_reverse_rh(VIEW_FOV.to_radians(), aspect, 0.01);
        let m = self.gun_place(g.def);
        let mvp = proj * m;
        let sun = Vec3::new(0.3, 0.9, 0.4).normalize();
        for t in &weapons::model(g.def).tris {
            let n = (t.p[1] - t.p[0]).cross(t.p[2] - t.p[0]).normalize_or_zero();
            let n = m.transform_vector3(n);
            let shade = 0.45 + 0.55 * n.dot(sun).abs();
            // Couleurs très sombres du pack (métal noir) : un peu relevées pour rester lisibles.
            let c = t.color.map(|v| (v * 2.2 + 0.015) * shade);
            for p in t.p {
                out.push(VmVertex {
                    clip: (mvp * p.extend(1.0)).to_array(),
                    uv: [0.0, 0.0],
                    layer: NO_TEXTURE,
                    _pad: 0,
                    color: [c[0] * light[0], c[1] * light[1], c[2] * light[2], 1.0],
                });
            }
        }
        // Flamme du canon, juste après un tir.
        if self.flash > 0.0 {
            let tip = m.transform_point3(weapons::model(g.def).muzzle);
            // Étoile de deux carrés croisés, jaune orangé.
            let s = 0.018 + 0.012 * (self.flash / 0.05);
            let c = [2.4, 1.5, 0.4, 1.0];
            let d = std::f32::consts::FRAC_1_SQRT_2;
            quad(&mut out, proj, tip, Vec3::X * s, Vec3::Y * s, c);
            quad(
                &mut out,
                proj,
                tip,
                Vec3::new(d, d, 0.0) * s * 1.4,
                Vec3::new(-d, d, 0.0) * s * 0.5,
                c,
            );
        }
        out
    }

    /// Balles traçantes, étincelles et impacts (dans le monde) : triangles projetés avec la
    /// caméra du jeu. Les impacts cachés par un bloc (vérifié par un rayon) ne sont pas dessinés.
    pub fn world_fx(&self, view_proj: Mat4, eye: Vec3, world: &World) -> Vec<VmVertex> {
        let mut out = Vec::new();
        let cam_right = |p: Vec3, d: Vec3| d.cross(p - eye).normalize_or_zero();
        for t in &self.tracers {
            let head = t.travelled.min(t.dist);
            let tail = (t.travelled - TRACER_LEN).max(0.0);
            if head <= tail {
                continue;
            }
            let a = t.from + t.dir * tail;
            let b = t.from + t.dir * head;
            let w = cam_right((a + b) * 0.5, t.dir) * 0.012;
            push_tri(
                &mut out,
                view_proj,
                [a - w, b - w, b + w],
                [3.0, 2.4, 1.2, 1.0],
            );
            push_tri(
                &mut out,
                view_proj,
                [b + w, a + w, a - w],
                [3.0, 2.4, 1.2, 1.0],
            );
        }
        for (p, v, age) in &self.sparks {
            let s = 0.025 * (1.0 - age / 0.35);
            let d = v.normalize_or_zero();
            let w = cam_right(*p, d) * s;
            let tail = *p - d * s * 3.0;
            push_tri(
                &mut out,
                view_proj,
                [tail, *p - w, *p + w],
                [4.0, 2.6, 0.8, 1.0],
            );
        }
        for (p, n) in &self.holes {
            // Caché derrière un bloc : on ne le dessine pas (il serait vu à travers).
            let to = *p - eye;
            let dist = to.length();
            if dist > 64.0 {
                continue;
            }
            if let Some(h) = raycast(world, eye.into(), (to / dist).into(), dist - 0.05) {
                if h.distance < dist - 0.06 {
                    continue;
                }
            }
            let u = if n.y.abs() > 0.5 { Vec3::X } else { Vec3::Y };
            let v = n.cross(u);
            let s = 0.045;
            let c = [0.01, 0.01, 0.01, 1.0];
            let (a, b, cc, d) = (
                *p - u * s - v * s,
                *p + u * s - v * s,
                *p + u * s + v * s,
                *p - u * s + v * s,
            );
            push_tri(&mut out, view_proj, [a, b, cc], c);
            push_tri(&mut out, view_proj, [cc, d, a], c);
        }
        out
    }

    // ------------------------------------------------------------ interface

    /// Vie, munitions, solde ; menu d'achat s'il est ouvert.
    pub fn hud(&self, ui: &mut Ui, size: (f32, f32), cursor: Option<(f32, f32)>) {
        let s = (size.1 / 360.0).floor().max(1.0);
        let col = |c: [u8; 3], a: f32| {
            let l = |v: u8| (v as f32 / 255.0).powf(2.2);
            [l(c[0]), l(c[1]), l(c[2]), a]
        };
        // Vie : en bas à gauche, « + 100 » et une barre.
        let (x, y) = (8.0 * s, size.1 - 22.0 * s);
        ui.rect(
            x - 2.0 * s,
            y - 2.0 * s,
            92.0 * s,
            18.0 * s,
            [0.0, 0.0, 0.0, 0.45],
        );
        let hpc = if self.hp > 50 {
            [235, 235, 225]
        } else if self.hp > 20 {
            [240, 200, 80]
        } else {
            [240, 70, 60]
        };
        ui.text(x, y, s * 1.5, col(hpc, 1.0), format!("+ {}", self.hp));
        let bw = 40.0 * s;
        ui.rect(
            x + 46.0 * s,
            y + 4.0 * s,
            bw,
            5.0 * s,
            [0.05, 0.05, 0.05, 0.8],
        );
        ui.rect(
            x + 46.0 * s,
            y + 4.0 * s,
            bw * self.hp as f32 / MAX_HP as f32,
            5.0 * s,
            col(hpc, 1.0),
        );
        // Munitions : en bas à droite.
        if let Some(g) = self.active() {
            let d = &WEAPONS[g.def];
            let t = if self.reload.is_some() {
                format!("{}   recharge...", d.name)
            } else {
                format!("{}   {} / {}", d.name, g.mag, g.reserve)
            };
            let w = Ui::text_width(&t, s * 1.5);
            ui.rect(
                size.0 - w - 12.0 * s,
                y - 2.0 * s,
                w + 8.0 * s,
                18.0 * s,
                [0.0, 0.0, 0.0, 0.45],
            );
            let c = if g.mag == 0 {
                [240, 70, 60]
            } else {
                [235, 235, 225]
            };
            ui.text(size.0 - w - 8.0 * s, y, s * 1.5, col(c, 1.0), t);
        }
        // Solde : en haut à gauche.
        ui.text(
            8.0 * s,
            8.0 * s,
            s * 1.5,
            col([120, 220, 120], 1.0),
            format!("{} $", self.money),
        );
        // Lunette : cache noir autour d'un cercle (approché par des bandes).
        if self.scope_fov().is_some() {
            let r = size.1 * 0.46;
            let (cx, cy) = (size.0 / 2.0, size.1 / 2.0);
            ui.rect(0.0, 0.0, cx - r, size.1, [0.0, 0.0, 0.0, 1.0]);
            ui.rect(cx + r, 0.0, size.0 - cx - r, size.1, [0.0, 0.0, 0.0, 1.0]);
            let steps = 48;
            for i in 0..steps {
                let y0 = cy - r + 2.0 * r * i as f32 / steps as f32;
                let y1 = y0 + 2.0 * r / steps as f32;
                let ym = ((y0 + y1) / 2.0 - cy) / r;
                let half = r * (1.0 - ym * ym).max(0.0).sqrt();
                ui.rect(cx - r, y0, r - half, y1 - y0, [0.0, 0.0, 0.0, 1.0]);
                ui.rect(cx + half, y0, r - half, y1 - y0, [0.0, 0.0, 0.0, 1.0]);
            }
            ui.rect(0.0, cy - 0.5 * s, size.0, s, [0.0, 0.0, 0.0, 1.0]);
            ui.rect(cx - 0.5 * s, 0.0, s, size.1, [0.0, 0.0, 0.0, 1.0]);
        }
        if self.buy_open {
            self.draw_buy(ui, size, cursor, s, &col);
        }
    }

    fn buy_layout(size: (f32, f32)) -> (f32, f32, f32, f32, f32) {
        let s = (size.1 / 360.0).floor().max(1.0);
        let cw = 150.0 * s;
        let rh = 14.0 * s;
        let w = cw * 4.0 + 16.0 * s;
        let h = 30.0 * s + rh * 10.0 + 10.0 * s;
        ((size.0 - w) / 2.0, (size.1 - h) / 2.0, w, h, s)
    }

    /// Arme du menu d'achat sous le curseur.
    fn buy_at(size: (f32, f32), x: f32, y: f32) -> Option<usize> {
        let (px, py, _, _, s) = Self::buy_layout(size);
        let (cw, rh) = (150.0 * s, 14.0 * s);
        for (ci, cat) in Category::ALL.into_iter().enumerate() {
            for (k, (i, _)) in WEAPONS
                .iter()
                .enumerate()
                .filter(|(_, d)| d.category == cat)
                .enumerate()
            {
                let (bx, by) = (px + 8.0 * s + ci as f32 * cw, py + 28.0 * s + k as f32 * rh);
                if x >= bx && x < bx + cw - 4.0 * s && y >= by && y < by + rh - 2.0 * s {
                    return Some(i);
                }
            }
        }
        None
    }

    /// Clic dans le menu d'achat.
    pub fn buy_click(&mut self, size: (f32, f32), cursor: (f32, f32)) {
        if let Some(i) = Self::buy_at(size, cursor.0, cursor.1) {
            // Solo : prix affiché, mais rien n'est débité (le solde reste à 0).
            self.buy(i);
            self.buy_open = false;
        }
    }

    fn draw_buy(
        &self,
        ui: &mut Ui,
        size: (f32, f32),
        cursor: Option<(f32, f32)>,
        s: f32,
        col: &dyn Fn([u8; 3], f32) -> [f32; 4],
    ) {
        let (px, py, w, h, _) = Self::buy_layout(size);
        let (cw, rh) = (150.0 * s, 14.0 * s);
        ui.rect(0.0, 0.0, size.0, size.1, [0.0, 0.0, 0.0, 0.35]);
        ui.rect(px, py, w, h, [0.02, 0.025, 0.03, 0.92]);
        ui.text(
            px + 8.0 * s,
            py + 6.0 * s,
            s * 1.5,
            col([240, 240, 240], 1.0),
            "Menu d'achat",
        );
        let solde = format!("Solde : {} $   (solo : achats gratuits)", self.money);
        let tw = Ui::text_width(&solde, s);
        ui.text(
            px + w - tw - 8.0 * s,
            py + 9.0 * s,
            s,
            col([120, 220, 120], 1.0),
            solde,
        );
        let hover = cursor.and_then(|(x, y)| Self::buy_at(size, x, y));
        for (ci, cat) in Category::ALL.into_iter().enumerate() {
            let x = px + 8.0 * s + ci as f32 * cw;
            ui.text(x, py + 19.0 * s, s, col([200, 170, 90], 1.0), cat.label());
            for (k, (i, d)) in WEAPONS
                .iter()
                .enumerate()
                .filter(|(_, d)| d.category == cat)
                .enumerate()
            {
                let y = py + 28.0 * s + k as f32 * rh;
                let owned = self.primary.is_some_and(|g| g.def == i)
                    || self.secondary.is_some_and(|g| g.def == i);
                let bg = if hover == Some(i) {
                    col([70, 90, 120], 0.95)
                } else if owned {
                    col([40, 70, 45], 0.9)
                } else {
                    col([30, 34, 40], 0.9)
                };
                ui.rect(x, y, cw - 4.0 * s, rh - 2.0 * s, bg);
                ui.text(
                    x + 3.0 * s,
                    y + 3.0 * s,
                    s,
                    col([235, 235, 235], 1.0),
                    d.name,
                );
                let p = format!("{} $", d.price);
                let pw = Ui::text_width(&p, s);
                ui.text(
                    x + cw - 8.0 * s - pw,
                    y + 3.0 * s,
                    s,
                    col([150, 210, 150], 1.0),
                    p,
                );
            }
        }
        if let Some(i) = hover {
            let d = &WEAPONS[i];
            let info = format!(
                "{} : {} dégâts, {} coups/min, chargeur {}{}",
                d.name,
                d.damage * d.pellets.max(1),
                d.rpm as u32,
                d.magazine,
                if d.auto { ", automatique" } else { "" }
            );
            ui.text(
                px + 8.0 * s,
                py + h - 9.0 * s,
                s,
                col([200, 200, 210], 1.0),
                info,
            );
        }
    }

    /// Entrées propres au menu d'achat ouvert (clic), à appeler avec le curseur libre.
    pub fn buy_input(&mut self, input: &Input, size: (f32, f32)) {
        if !self.buy_open {
            return;
        }
        if input.mouse_pressed(minelib_platform::Mouse::Left) {
            if let Some(c) = input.cursor {
                self.buy_click(size, c);
            }
        }
    }
}

fn push_tri(out: &mut Vec<VmVertex>, vp: Mat4, p: [Vec3; 3], color: [f32; 4]) {
    for q in p {
        let c = vp * q.extend(1.0);
        // Derrière la caméra : on saute le triangle entier.
        if c.w <= 0.01 {
            out.truncate(out.len() - out.len() % 3);
            return;
        }
        out.push(VmVertex {
            clip: c.to_array(),
            uv: [0.0, 0.0],
            layer: NO_TEXTURE,
            _pad: 0,
            color,
        });
    }
}

fn quad(out: &mut Vec<VmVertex>, vp: Mat4, c: Vec3, u: Vec3, v: Vec3, color: [f32; 4]) {
    let (a, b, cc, d) = (c - u - v, c + u - v, c + u + v, c - u + v);
    push_tri(out, vp, [a, b, cc], color);
    push_tri(out, vp, [cc, d, a], color);
}

#[cfg(test)]
mod tests {
    use super::*;
    use minelib_world::{BlockId, ChunkPos};

    #[test]
    fn fall_damage_curve() {
        assert_eq!(Fps::fall_damage(4.0, 4.0), 0);
        assert!(Fps::fall_damage(6.0, 4.0) > 0);
        assert!(Fps::fall_damage(8.0, 4.0) < 25);
        assert!((24..=30).contains(&Fps::fall_damage(13.0, 4.0)));
        assert!(Fps::fall_damage(50.0, 4.0) < 100);
    }

    #[test]
    fn shooting_uses_ammo_makes_holes_and_recoils() {
        let mut w = World::new(0);
        w.insert_empty(ChunkPos { x: 0, z: 0 });
        for y in 0..16 {
            for x in 0..16 {
                w.set(x, y, 4, BlockId::STONE); // mur au nord (z = 4)
            }
        }
        let mut f = Fps::new();
        let ak = WEAPONS.iter().position(|d| d.name == "AK-47").unwrap();
        f.buy(ak);
        assert_eq!(f.money, 0, "solo : achat gratuit, solde inchangé");
        let eye = Vec3::new(8.5, 8.0, 12.5);
        let c = Controls {
            trigger_pressed: true,
            trigger_held: true,
            on_ground: true,
            ..Default::default()
        };
        for _ in 0..120 {
            f.update(1.0 / 60.0, &c, &w, eye, 0.0, 0.0);
            f.tick_effects(1.0 / 60.0);
        }
        let g = f.active().unwrap();
        assert!(g.mag < 30, "des balles tirées");
        assert!(!f.holes.is_empty(), "impacts sur le mur");
        assert!(f.punch.0 > 0.5, "la vue remonte");
        // Relâché : le motif et la vue reviennent.
        let idle = Controls {
            on_ground: true,
            ..Default::default()
        };
        for _ in 0..120 {
            f.update(1.0 / 60.0, &idle, &w, eye, 0.0, 0.0);
        }
        assert_eq!(f.spray_n, 0);
        assert!(f.punch.0 < 0.05);
        // Rechargement.
        let r = Controls {
            reload: true,
            on_ground: true,
            ..Default::default()
        };
        f.update(1.0 / 60.0, &r, &w, eye, 0.0, 0.0);
        for _ in 0..200 {
            f.update(1.0 / 60.0, &idle, &w, eye, 0.0, 0.0);
        }
        assert_eq!(f.active().unwrap().mag, 30);
    }
}
