//! (v216) Le ciel de la partie : la météo, la foudre, la pluie et la Foudre de Zeus tenue
//! en main. La partie (`session`) lui donne le monde et le joueur à chaque image ; elle en
//! reçoit ce qu'il faut dessiner (segments des éclairs, pluie), éclairer (éclat de la
//! foudre) et subir (dégâts), et le tonnerre comme la pluie se font entendre d'ici.

use glam::{Mat4, Vec3};
use minelib_render::sky::SkyWeather;
use minelib_render::{BoltSeg, DynKind, DynLight, Rain, BOLT_LIGHT, RAIN_TOPS_N};
use minelib_world::{BlockId, ChunkPos, World, CHUNK_SIZE};

use crate::lightning::{self, Arc, HeldBolt, Rng, Strike};
use crate::weather::{self, Relief, Weather};

/// Hauteur de la base des nuages (la même que dans le rendu du ciel) : la foudre en vient.
pub const CLOUD_Y: f32 = 420.0;

/// Portée de `/thor` et de la Foudre de Zeus (blocs).
pub const THOR_RANGE: f32 = 256.0;

/// Temps entre deux coups de la Foudre de Zeus (s).
pub const ZEUS_COOLDOWN: f32 = 0.3;

/// Lumières des coups de foudre proches : identifiants à part de ceux des lumières posées.
const STRIKE_LIGHT_ID: u64 = 0x7f1a_5410_0001_0000;
/// Lumière de la Foudre de Zeus tenue.
const ZEUS_LIGHT_ID: u64 = 0x7f1a_5410_0000_0003;

/// Plus haut bloc de la colonne (`x`, `z`), quel qu'il soit (feuillage, eau, dalle) :
/// `None` si elle est vide ou pas chargée.
pub fn column_top(world: &World, x: i32, z: i32) -> Option<i32> {
    let c = world.chunk(ChunkPos::containing(x, z))?;
    let (lx, lz) = (
        x.rem_euclid(CHUNK_SIZE as i32) as usize,
        z.rem_euclid(CHUNK_SIZE as i32) as usize,
    );
    (c.bottom()..c.top())
        .rev()
        .find(|&y| c.get(lx, y, lz) != BlockId::AIR)
        .map(|y| y as i32)
}

/// Le relief du monde, vu par la foudre.
pub struct WorldRelief<'a>(pub &'a World);

impl Relief for WorldRelief<'_> {
    fn top(&self, x: i32, z: i32) -> Option<i32> {
        column_top(self.0, x, z)
    }

    fn chunk_top(&self, cx: i32, cz: i32) -> Option<i32> {
        let c = self.0.chunk(ChunkPos { x: cx, z: cz })?;
        (c.top() > c.bottom()).then(|| c.top() as i32 - 1)
    }
}

/// Carte des toits pour la pluie : le dessus du plus haut bloc de chaque colonne autour du
/// joueur. Recentrée quand il s'éloigne, relue peu à peu (un bloc posé ou cassé au-dessus
/// de soi change où il pleut).
struct Tops {
    heights: Vec<f32>,
    origin: [i32; 2],
    version: u64,
    row: usize,
    valid: bool,
}

/// Pas de bloc dans la colonne : la pluie passe.
const NO_ROOF: f32 = -1.0e9;

impl Tops {
    fn new() -> Self {
        Self {
            heights: vec![NO_ROOF; RAIN_TOPS_N * RAIN_TOPS_N],
            origin: [0, 0],
            version: 0,
            row: 0,
            valid: false,
        }
    }

    fn read(world: &World, x: i32, z: i32) -> f32 {
        column_top(world, x, z).map_or(NO_ROOF, |y| (y + 1) as f32)
    }

    fn refresh(&mut self, world: &World, eye: Vec3) {
        let n = RAIN_TOPS_N as i32;
        let c = [eye.x.floor() as i32, eye.z.floor() as i32];
        let off = |k: usize| (c[k] - (self.origin[k] + n / 2)).abs();
        if !self.valid || off(0) > 10 || off(1) > 10 {
            // Recentrée : tout est relu d'un coup (rare : il faut avoir fait 10 blocs).
            self.origin = [c[0] - n / 2, c[1] - n / 2];
            for z in 0..n {
                for x in 0..n {
                    self.heights[(z * n + x) as usize] =
                        Self::read(world, self.origin[0] + x, self.origin[1] + z);
                }
            }
            self.valid = true;
            self.version += 1;
            self.row = 0;
            return;
        }
        // Sinon quatre rangées par image : toute la carte en 16 images.
        let mut changed = false;
        for _ in 0..4 {
            let z = self.row as i32;
            for x in 0..n {
                let h = Self::read(world, self.origin[0] + x, self.origin[1] + z);
                let slot = &mut self.heights[(z * n + x) as usize];
                if *slot != h {
                    *slot = h;
                    changed = true;
                }
            }
            self.row = (self.row + 1) % RAIN_TOPS_N;
        }
        if changed {
            self.version += 1;
        }
    }
}

/// Éclair dans les nuages, au loin (la plupart des éclairs d'un orage ne touchent pas le
/// sol) : une lueur d'un côté du ciel, en deux ou trois sursauts, et son tonnerre plus tard.
#[derive(Clone, Copy, Debug)]
struct Glow {
    age: f32,
    force: f32,
    dir: [f32; 2],
    beats: [f32; 3],
}

impl Glow {
    const LAST_S: f32 = 0.5;

    fn light(&self) -> f32 {
        self.beats
            .iter()
            .enumerate()
            .map(|(k, b)| {
                let t = self.age - k as f32 * 0.11;
                if t < 0.0 {
                    0.0
                } else {
                    b * (-t / 0.06).exp()
                }
            })
            .fold(0.0, f32::max)
            * self.force
    }
}

/// Ce que la partie donne au ciel à chaque image.
pub struct Frame<'a> {
    pub world: &'a World,
    /// Pieds et yeux du joueur.
    pub feet: Vec3,
    pub eye: Vec3,
    /// La météo vit (pas en mode FPS, ni à l'heure « par défaut » des cartes).
    pub active: bool,
    /// Serveur de construction : l'horloge de la météo est l'heure du système.
    pub online: bool,
    /// Le joueur est sous un toit (la pluie s'entend moins).
    pub covered: bool,
}

pub struct Storm {
    pub weather: Weather,
    strikes: Vec<Strike>,
    pub held: HeldBolt,
    /// (v217) Étincelles dans le monde : la gerbe d'une foudre tombée tout près.
    sparks: HeldBolt,
    /// Segments à dessiner cette image : dans le monde, et à la première personne.
    pub bolts: Vec<BoltSeg>,
    pub hand_bolts: Vec<BoltSeg>,
    /// Éclat de la foudre là où est le joueur (0 : rien … 1 : tombée tout près).
    pub flash: f32,
    /// Lueur dans les nuages : côté d'où elle vient (x, z) et force.
    pub sky_flash: [f32; 5],
    tops: Tops,
    glow: Option<Glow>,
    glow_in: f32,
    rng: Rng,
    /// Volume de la pluie envoyé au son (pour ne le renvoyer que s'il change).
    rain_sent: f32,
    /// Temps avant de pouvoir relancer la Foudre de Zeus.
    pub cooldown: f32,
    arcs: Vec<Arc>,
    active: bool,
    /// Essais (MINELIB_ZEUS, MINELIB_THOR) : objet déjà donné, temps avant le prochain coup.
    pub test_given: bool,
    pub test_in: f32,
    /// Essais (`MINELIB_CMD`) : temps passé dans le monde, et commandes déjà tapées.
    pub test_clock: f32,
    pub test_cmds: usize,
    /// (v217) Horloge des éclairs posés au sol.
    clock: f32,
    /// Œil du joueur à la dernière image.
    eye: Vec3,
}

impl Storm {
    pub fn new(seed: u64) -> Self {
        Self {
            weather: Weather::new(seed),
            strikes: Vec::new(),
            held: HeldBolt::default(),
            sparks: HeldBolt::default(),
            bolts: Vec::new(),
            hand_bolts: Vec::new(),
            flash: 0.0,
            sky_flash: [0.0; 5],
            tops: Tops::new(),
            glow: None,
            glow_in: 4.0,
            rng: Rng::new(seed ^ 0x57_0a11),
            rain_sent: 0.0,
            cooldown: 0.0,
            arcs: Vec::new(),
            active: false,
            test_given: false,
            test_in: 0.0,
            test_clock: 0.0,
            test_cmds: 0,
            clock: 0.0,
            eye: Vec3::ZERO,
        }
    }

    /// Une image : la météo avance, la foudre tombe, les éclairs en cours se déroulent.
    /// Renvoie les dégâts que le joueur prend (foudre tombée sur lui).
    pub fn update(&mut self, dt: f32, f: &Frame) -> u8 {
        self.bolts.clear();
        self.hand_bolts.clear();
        self.active = f.active;
        self.cooldown = (self.cooldown - dt).max(0.0);
        self.clock += dt;
        self.eye = f.eye;
        if f.active {
            self.weather.update(dt, f.online.then(weather::unix_now));
            // La foudre du ciel : elle vise une colonne, le relief l'attire.
            for spot in self.weather.strikes(f.feet) {
                let (x, z) = (spot.x.floor() as i32, spot.z.floor() as i32);
                if let Some((x, z, top)) = weather::attach(x, z, &WorldRelief(f.world)) {
                    let pos = Vec3::new(x as f32 + 0.5, (top + 1) as f32, z as f32 + 0.5);
                    self.strikes
                        .push(Strike::new(pos, spot.seed, cloud_y(pos), spot.delay));
                    self.maybe_cloud_bolt(pos, spot.delay);
                }
            }
        }
        let mut hurt = 0u8;
        for s in &mut self.strikes {
            if s.update(dt) {
                match s.kind {
                    // (Le tonnerre d'un éclair des nuages part avec sa lueur.)
                    lightning::Kind::Cloud => {}
                    // Celle qui part de la main claque tout près.
                    lightning::Kind::Sky => crate::audio::thunder(25.0, s.seed),
                    lightning::Kind::Ground => {
                        let d = (s.pos - f.eye).length();
                        crate::audio::thunder(d, s.seed);
                        // (v217) Une gerbe d'étincelles jaillit du point frappé : dense tout
                        // près, et encore visible de loin (elles volent alors plus loin,
                        // pour rester une gerbe à l'écran).
                        if d < 420.0 {
                            let far = (d / 45.0).max(1.0);
                            let n = if d < 70.0 { 60 } else { 34 };
                            self.sparks
                                .burst(s.pos + Vec3::Y * 0.1, Vec3::Y, n, 9.0 * far);
                        }
                    }
                }
                if s.hurts(f.feet) {
                    hurt = hurt.saturating_add(lightning::DAMAGE);
                }
            }
        }
        self.strikes.retain(|s| !s.done());
        self.sparks.update(dt, Mat4::IDENTITY, false);
        let mut arcs = std::mem::take(&mut self.arcs);
        arcs.clear();
        self.sparks.sparks(&mut arcs);
        lightning::in_world(&arcs, Mat4::IDENTITY, &mut self.bolts);
        self.arcs = arcs;
        // Éclat sur le monde (il faiblit avec la distance) et lueur dans les nuages, du
        // côté de l'éclair le plus fort.
        let mut best = (0.0f32, [0.0f32; 2], [0.0f32; 2]);
        let mut world = 0.0f32;
        for s in &self.strikes {
            s.segments(f.eye, &mut self.bolts);
            let l = s.light();
            // (L'éclat d'un éclair des nuages vient de sa lueur, plus bas.)
            if l <= 0.0 || s.kind == lightning::Kind::Cloud {
                continue;
            }
            let d = Vec3::new(s.pos.x - f.eye.x, 0.0, s.pos.z - f.eye.z);
            let level = l / (1.0 + (d.length() / 150.0).powi(2));
            // (v237) Celle qu'on lance vers le ciel : « le spot lumineux doit venir d'où je
            // tape, pas d'au-dessus de moi ». Les nuages s'allument en une tache là où elle
            // entre dans leur couche (avant : tout ce côté du ciel, jusqu'au-dessus de la
            // tête), et le paysage reçoit bien moins d'éclat du ciel (cet éclat-là tombe
            // d'en haut, comme le jour) : il est éclairé par les lumières de son canal, du
            // côté où elle part (`lights`).
            let up = s.kind == lightning::Kind::Sky;
            world = world.max(if up { level * SKY_STRIKE_FLASH } else { level });
            if level > best.0 {
                best = if up {
                    let to = (s.cloud_entry() - f.eye).normalize_or(Vec3::Y);
                    let flat = Vec3::new(to.x, 0.0, to.z).normalize_or_zero();
                    (level, [flat.x, flat.z], [to.y, 1.0])
                } else {
                    let n = d.normalize_or_zero();
                    (level, [n.x, n.z], [0.0, 0.0])
                };
            }
        }
        self.flash = world;
        let mut sky = (best.0, best.1, best.2);
        // Éclairs dans les nuages, au loin.
        let thundering = f.active && self.weather.thundering();
        if let Some(g) = &mut self.glow {
            g.age += dt;
            if g.age > Glow::LAST_S {
                self.glow = None;
            }
        }
        if thundering {
            self.glow_in -= dt;
            if self.glow_in <= 0.0 && self.glow.is_none() {
                self.glow_in = self.rng.range(3.0, 11.0);
                let a = self.rng.range(0.0, std::f32::consts::TAU);
                self.glow = Some(Glow {
                    age: 0.0,
                    force: self.rng.range(0.25, 0.65),
                    dir: [a.cos(), a.sin()],
                    beats: [1.0, self.rng.range(0.3, 1.0), self.rng.range(0.0, 0.8)],
                });
                let far = self.rng.range(450.0, 1500.0);
                crate::audio::thunder(far, self.rng.next() as u32);
                // (v217) Deux fois sur trois on voit l'éclair lui-même : il court sous la
                // base des nuages et dedans, de ce côté-là du ciel, sans tomber.
                if self.rng.unit() < 0.67 {
                    self.cloud_bolt(f.eye, [a.cos(), a.sin()]);
                }
            }
        }
        if let Some(g) = &self.glow {
            let l = g.light();
            // (La lueur des nuages éclaire un peu le sol aussi.)
            self.flash = self.flash.max(l * 0.22);
            if l > sky.0 {
                sky = (l, g.dir, [0.0, 0.0]);
            }
        }
        self.sky_flash = [sky.1[0], sky.1[1], sky.0.min(1.0), sky.2[0], sky.2[1]];
        // Pluie : la carte des toits, et son bruit (assourdi sous un toit).
        let rain = if f.active {
            self.weather.rain_amount
        } else {
            0.0
        };
        if rain > 0.002 {
            self.tops.refresh(f.world, f.eye);
        }
        // (v234) « Le son de la pluie 25 % moins fort en général » : × 0,75.
        let volume = if rain > 0.01 {
            rain.powf(1.3)
                * (0.45 + 0.2 * self.weather.storm_amount)
                * RAIN_VOLUME
                * if f.covered { 0.4 } else { 1.0 }
        } else {
            0.0
        };
        if (volume - self.rain_sent).abs() > 0.01 || (volume == 0.0) != (self.rain_sent == 0.0) {
            crate::audio::rain(volume);
            self.rain_sent = volume;
        }
        hurt
    }

    /// `/thor`, ou un coup de la Foudre de Zeus : la foudre tombe en `pos`, tout de suite.
    pub fn strike(&mut self, pos: Vec3) {
        let seed = self.rng.next() as u32;
        self.strike_seeded(pos, seed);
    }

    /// (v229) La même, de forme tirée de `seed` : sur un serveur de construction, l'arbitre
    /// la publie avec sa graine, et tous ceux qui la voient voient le même éclair.
    pub fn strike_seeded(&mut self, pos: Vec3, seed: u32) {
        self.strikes.push(Strike::new(pos, seed, cloud_y(pos), 0.0));
        self.maybe_cloud_bolt(pos, 0.0);
    }

    /// (v220) Près de trois fois sur dix, un éclair court aussi dans les nuages au moment
    /// où la foudre part vers `pos` (foudre du ciel, Foudre de Zeus ou `/thor`, par tous
    /// les temps), de ce côté-là du ciel.
    fn maybe_cloud_bolt(&mut self, pos: Vec3, delay: f32) {
        if self.rng.unit() >= 0.3 {
            return;
        }
        let eye = self.eye;
        let to = Vec3::new(pos.x - eye.x, 0.0, pos.z - eye.z);
        let a = self.rng.range(0.0, std::f32::consts::TAU);
        let side = to.normalize_or(Vec3::new(a.cos(), 0.0, a.sin()));
        let away = (to.length() + self.rng.range(-60.0, 160.0)).clamp(80.0, 900.0);
        let before = self.strikes.len();
        self.cloud_bolt_at(eye, [side.x, side.z], away);
        if let Some(s) = self.strikes.get_mut(before) {
            s.delay = delay;
        }
    }

    /// (v217) En visant le ciel : la foudre ne tombe pas, elle part de `from` (la pointe
    /// de l'éclair tenu, ou la main) dans la direction `dir`, monte et traverse les
    /// nuages. `eye_y` : hauteur de l'œil (la base des nuages en dépend).
    pub fn strike_up(&mut self, from: Vec3, dir: Vec3, eye_y: f32) {
        let seed = self.rng.next() as u32;
        self.strike_up_seeded(from, dir, eye_y, seed);
        self.held.surge(from, dir);
    }

    /// (v229) La même, de forme tirée de `seed` (voir `strike_seeded`), sans le sursaut de
    /// l'éclair tenu (c'est peut-être celle d'un autre joueur).
    pub fn strike_up_seeded(&mut self, from: Vec3, dir: Vec3, eye_y: f32, seed: u32) {
        let base = minelib_render::sky::cloud_base_y(eye_y);
        let thick = minelib_render::sky::thickness(&self.sky());
        self.strikes.push(Strike::up(from, dir, seed, base, thick));
        self.maybe_cloud_bolt(from + dir * 400.0, 0.0);
    }

    /// (v217) Éclair d'orage dans les nuages, du côté `side` (x, z) du ciel vu de `eye` : il
    /// part de la couche, descend sous sa base (on l'y voit en entier), puis y remonte ou
    /// se perd dans l'air.
    fn cloud_bolt(&mut self, eye: Vec3, side: [f32; 2]) {
        let away = self.rng.range(260.0, 950.0);
        self.cloud_bolt_at(eye, side, away);
    }

    /// Le même, à `away` blocs (à plat) de `eye`.
    fn cloud_bolt_at(&mut self, eye: Vec3, side: [f32; 2], away: f32) {
        use minelib_render::sky::{cloud_base_y, layer_y, thickness};
        let thick = thickness(&self.sky());
        let side = Vec3::new(side[0], 0.0, side[1]);
        let across = Vec3::new(-side.z, 0.0, side.x);
        let half = self.rng.range(150.0, 420.0);
        let turn = self.rng.range(-0.7, 0.7);
        let along = (across * turn.cos() + side * turn.sin()).normalize();
        let centre = eye + side * away;
        let rng = &mut self.rng;
        let at = |k: f32, above: f32, rng: &mut Rng| {
            let p = centre + along * (half * k) + side * rng.range(-60.0, 60.0);
            let flat = Vec3::new(p.x - eye.x, 0.0, p.z - eye.z).length();
            Vec3::new(p.x, layer_y(eye.y, flat, above), p.z)
        };
        let a = at(-1.0, thick * rng.range(0.04, 0.3), rng);
        let dip = at(rng.range(-0.25, 0.25), -rng.range(10.0, 55.0), rng);
        // Une fois sur trois, il finit dans l'air sous les nuages au lieu d'y remonter.
        let b = if rng.unit() < 0.33 {
            at(1.0, -rng.range(30.0, 110.0), rng)
        } else {
            at(1.0, thick * rng.range(0.04, 0.3), rng)
        };
        let seed = rng.next() as u32;
        self.strikes
            .push(Strike::cloud(a, dip, b, seed, cloud_base_y(eye.y)));
    }

    /// (v217) La Foudre de Zeus posée au sol en `at` (objet lâché) : le même éclair qu'en
    /// main, debout. (v235) De la même taille qu'en main (il était plus de deux fois plus
    /// petit). `phase` : chaque objet vit à son rythme.
    pub fn ground_item(&mut self, at: Vec3, phase: f32) {
        let mut arcs = std::mem::take(&mut self.arcs);
        arcs.clear();
        HeldBolt::arcs_at(self.clock + phase, 1.0, &mut arcs);
        let scale = GROUND_ITEM / lightning::HELD_LEN;
        let place = Mat4::from_translation(at + Vec3::Y * (GROUND_ITEM * 0.5 + 0.03))
            * Mat4::from_scale(Vec3::splat(scale));
        lightning::in_world_scaled(&arcs, place, scale, &mut self.bolts);
        self.arcs = arcs;
    }

    /// La Foudre de Zeus tenue en main, pour cette image. `local` : du repère de l'éclair
    /// (le poing à l'origine, l'éclair le long de +y) vers le monde ; `screen` : à la
    /// première personne, (projection de la main × repère de l'éclair dans la vue,
    /// projection de la main × vue) : l'éclair et ses étincelles sont alors dessinés par-
    /// dessus la main ; sinon dans le monde (vue de derrière). `on` : il est tenu.
    pub fn hold(&mut self, dt: f32, local: Mat4, screen: Option<(Mat4, Mat4)>, on: bool) {
        self.held.update(dt, local, on);
        let mut arcs = std::mem::take(&mut self.arcs);
        arcs.clear();
        if on {
            self.held.arcs(&mut arcs);
            match screen {
                Some((bolt, _)) => lightning::on_screen(&arcs, bolt, &mut self.hand_bolts),
                None => lightning::in_world(&arcs, local, &mut self.bolts),
            }
        }
        // Les étincelles volent dans le monde, même une fois l'éclair rangé.
        arcs.clear();
        self.held.sparks(&mut arcs);
        match screen {
            Some((_, world)) => lightning::on_screen(&arcs, world, &mut self.hand_bolts),
            None => lightning::in_world(&arcs, Mat4::IDENTITY, &mut self.bolts),
        }
        self.arcs = arcs;
    }

    /// Lumières des coups de foudre proches de `eye` : le sol et les murs autour du point
    /// frappé s'illuminent.
    pub fn lights(&self, eye: Vec3, out: &mut Vec<DynLight>) {
        for (k, s) in self.strikes.iter().enumerate() {
            let l = s.light();
            if l < 0.03
                || s.kind == lightning::Kind::Cloud
                || (s.pos - eye).length() > 110.0
            {
                continue;
            }
            // (v220) Deux lumières sur le canal lui-même : tout près du point frappé, et
            // plus haut le long de l'éclair (elle éclaire de là où il passe). (v235) Bien
            // moins fortes (16 et 20 avant), et la première un peu plus haut : l'éclat d'une
            // lumière vaut jusqu'au bout de sa portée ou presque, et à 16 et 20 tout le sol
            // devenait blanc sur des dizaines de blocs : on ne voyait plus l'éclair qui
            // venait de tomber, ni rien d'autre.
            let [low, high] = s.light_points();
            // (v237) Celle qu'on lance vers le ciel : ses lumières sont plus loin le long
            // d'elle (voir `light_points`), elles portent donc plus loin.
            let reach = if s.kind == lightning::Kind::Sky {
                [34.0, 90.0]
            } else {
                [22.0, 44.0]
            };
            for (n, (pos, intensity, range)) in [(low, 2.0, reach[0]), (high, 1.2, reach[1])]
                .into_iter()
                .enumerate()
            {
                out.push(DynLight {
                    id: STRIKE_LIGHT_ID + ((k as u64 & 0x7f) << 1) + n as u64,
                    kind: DynKind::Point,
                    pos: pos.to_array(),
                    color: BOLT_LIGHT,
                    intensity: intensity * l,
                    range,
                    // (Pas d'ombres : un éclair dure un tiers de seconde, leur calcul
                    // ferait sauter une image.)
                    shadows: false,
                    beam: 0.0,
                    ..Default::default()
                });
            }
        }
    }

    /// Lumière de la Foudre de Zeus tenue, en `pos` : elle éclaire les murs et les
    /// personnages autour, avec leurs ombres, et scintille avec l'éclair.
    pub fn held_light(&self, pos: Vec3) -> DynLight {
        DynLight {
            id: ZEUS_LIGHT_ID,
            kind: DynKind::Point,
            pos: pos.to_array(),
            color: BOLT_LIGHT,
            intensity: 3.0 * self.held.glow(),
            range: 14.0,
            shadows: true,
            // (Comme la torche tenue : l'ombre du joueur, sauf sa main tout contre.)
            owner: crate::flashlight::NEAR_FLAME,
            beam: 0.0,
            ..Default::default()
        }
    }

    /// Ce que le rendu du ciel doit savoir (beau temps fixe quand la météo ne vit pas).
    pub fn sky(&self) -> SkyWeather {
        if self.active {
            self.weather.sky()
        } else {
            SkyWeather::default()
        }
    }

    /// Part de la lumière du jour qui arrive au sol.
    pub fn light(&self) -> f32 {
        if self.active {
            self.weather.light()
        } else {
            1.0
        }
    }

    /// Distance de vue (1 par beau temps).
    pub fn visibility(&self) -> f32 {
        if self.active {
            self.weather.visibility()
        } else {
            1.0
        }
    }

    /// La pluie à dessiner.
    pub fn rain(&self, daylight: f32) -> Rain<'_> {
        let amount = if self.active && self.tops.valid {
            self.weather.rain_amount
        } else {
            0.0
        };
        Rain {
            amount,
            wind: self.weather.sky().wind,
            tops: self.tops.valid.then_some((
                &self.tops.heights[..],
                self.tops.origin,
                self.tops.version,
            )),
            daylight,
        }
    }
}

/// (v237) Part de l'éclat du ciel (celui qui éclaire tout le paysage d'en haut) que donne
/// la foudre lancée vers le ciel.
const SKY_STRIKE_FLASH: f32 = 0.3;

/// (v234) Volume propre de la pluie (avant : 1 ; « 25 % moins fort en général »). Le
/// curseur « Volume de la météo » (Paramètres → Son) s'y ajoute.
const RAIN_VOLUME: f32 = 0.75;

/// (v217) Hauteur de la Foudre de Zeus posée au sol (blocs). (v235) Celle de l'éclair tenu
/// en main (elle valait 0,55).
const GROUND_ITEM: f32 = lightning::HELD_LEN;

/// (v217) Quitter la partie fait taire la pluie et le tonnerre (ils continuaient à
/// l'accueil : la boucle de la pluie ne s'arrêtait qu'avec la météo de la partie).
impl Drop for Storm {
    fn drop(&mut self) {
        crate::audio::weather_off();
    }
}

/// (v217) Le relief au loin, pour le sol que le ciel dessine sous l'horizon au-delà du
/// monde chargé : couleur moyenne du dessus des blocs (`color` : celle d'une image de bloc)
/// et hauteur moyenne, relevées sur deux cercles autour de `eye` (à 55 % et 80 % de
/// `reach`, la distance de vue). `None` : rien de chargé là-bas.
pub fn far_ground(
    world: &World,
    eye: Vec3,
    reach: f32,
    color: &dyn Fn(u16) -> [f32; 3],
) -> Option<([f32; 3], f32)> {
    let (mut sum, mut height, mut n) = ([0.0f32; 3], 0.0f32, 0u32);
    for k in 0..16 {
        let a = k as f32 * std::f32::consts::TAU / 16.0;
        let r = reach * if k % 2 == 0 { 0.8 } else { 0.55 };
        let (x, z) = (
            (eye.x + a.cos() * r).floor() as i32,
            (eye.z + a.sin() * r).floor() as i32,
        );
        let Some(y) = column_top(world, x, z) else {
            continue;
        };
        let c = color(world.get(x, y, z).def().tex[0]);
        for i in 0..3 {
            sum[i] += c[i];
        }
        height += (y + 1) as f32;
        n += 1;
    }
    (n >= 3).then(|| (sum.map(|v| v / n as f32), height / n as f32))
}

/// D'où part un éclair qui frappe `pos` : la base des nuages, toujours bien au-dessus.
fn cloud_y(pos: Vec3) -> f32 {
    CLOUD_Y.max(pos.y + 160.0)
}


/// (v222) Lumière de la Foudre de Zeus tenue par un autre joueur (le `k`-ième proche), en
/// `pos` : comme la sienne (`Storm::held_light`), avec le scintillement (`glow`) de son
/// éclair. Son bit de porteur est mis par l'appelant (ses morceaux ne la cachent pas).
pub fn held_light_at(pos: Vec3, glow: f32, k: usize) -> DynLight {
    DynLight {
        id: ZEUS_LIGHT_ID + 1 + k as u64,
        kind: DynKind::Point,
        pos: pos.to_array(),
        color: BOLT_LIGHT,
        intensity: 3.0 * glow,
        range: 14.0,
        shadows: true,
        beam: 0.0,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(top: i32) -> World {
        let mut w = World::new(0);
        for cz in -2..=2 {
            for cx in -2..=2 {
                w.ensure_chunk(ChunkPos { x: cx, z: cz });
            }
        }
        for z in -32..48 {
            for x in -32..48 {
                w.set(x, top, z, BlockId::STONE);
            }
        }
        w
    }

    fn frame(world: &World, feet: Vec3) -> Frame<'_> {
        Frame {
            world,
            feet,
            eye: feet + Vec3::Y * 1.6,
            active: true,
            online: false,
            covered: false,
        }
    }

    /// `/thor` : la foudre tombe où on la demande, blesse qui s'y trouve (5 points de vie,
    /// une seule fois), éclaire le monde autour, puis s'éteint.
    #[test]
    fn a_commanded_strike_lands_hurts_once_and_lights_up() {
        let world = flat(64);
        let mut s = Storm::new(1);
        let feet = Vec3::new(4.5, 65.0, 4.5);
        s.strike(Vec3::new(5.5, 65.0, 5.0));
        let (mut hurt, mut flash, mut lights, mut segs) = (0u32, 0.0f32, 0usize, 0usize);
        for _ in 0..400 {
            hurt += s.update(1.0 / 200.0, &frame(&world, feet)) as u32;
            flash = flash.max(s.flash);
            segs = segs.max(s.bolts.len());
            let mut l = Vec::new();
            s.lights(feet, &mut l);
            lights = lights.max(l.len());
            assert!(l.iter().all(|l| !l.shadows && l.intensity <= 20.0));
            // (v220) Ses lumières sont sur son canal : au-dessus du point frappé, l'une
            // tout près de lui, l'autre une vingtaine de blocs plus haut.
            if let [low, high] = &l[..] {
                let at = |l: &DynLight| Vec3::from(l.pos) - Vec3::new(5.5, 65.0, 5.0);
                assert!(at(low).length() < 4.0, "{:?}", at(low));
                assert!(at(high).y > 12.0 && at(high).length() < 28.0, "{:?}", at(high));
                assert_ne!(low.id, high.id);
            }
        }
        assert_eq!(hurt, lightning::DAMAGE as u32);
        assert!(
            flash > 0.9 && lights == 2 && segs > 150,
            "{flash} {lights} {segs}"
        );
        assert!(s.strikes.is_empty() && s.flash == 0.0 && s.bolts.is_empty());
        // Loin du joueur : pas de dégâts, un éclat plus faible.
        s.strike(Vec3::new(4.5 + 300.0, 65.0, 4.5));
        let (mut hurt, mut flash) = (0u32, 0.0f32);
        for _ in 0..400 {
            hurt += s.update(1.0 / 200.0, &frame(&world, feet)) as u32;
            flash = flash.max(s.flash);
        }
        assert!(hurt == 0 && flash > 0.1 && flash < 0.3, "{flash}");
    }

    /// (v237) « Quand je tape vers le ciel, le spot lumineux doit venir d'où je tape, pas
    /// d'au-dessus de moi » : les nuages s'allument en une tache du côté où part la foudre
    /// (et non tout un côté du ciel), le paysage reçoit peu d'éclat du ciel, et ses lumières
    /// sont le long d'elle, loin de la main.
    #[test]
    fn a_bolt_thrown_at_the_sky_lights_the_clouds_where_it_goes() {
        let world = flat(64);
        let mut s = Storm::new(3);
        let feet = Vec3::new(0.5, 65.0, 0.5);
        let eye = feet + Vec3::Y * 1.6;
        let dir = Vec3::new(0.6, 0.5, -0.3).normalize();
        s.strike_up(eye + dir * 0.9, dir, eye.y);
        let (mut flash, mut glow, mut lit) = (0.0f32, 0.0f32, 0);
        for _ in 0..300 {
            s.update(1.0 / 200.0, &frame(&world, feet));
            flash = flash.max(s.flash);
            let [x, z, level, rise, focus] = s.sky_flash;
            if level > 0.0 {
                glow = glow.max(level);
                let flat = (1.0 - rise * rise).max(0.0).sqrt();
                let at = Vec3::new(x * flat, rise, z * flat);
                assert!(focus == 1.0 && at.dot(dir) > 0.97, "{:?}", s.sky_flash);
            }
            let mut l = Vec::new();
            s.lights(eye, &mut l);
            if let [low, high] = &l[..] {
                lit += 1;
                let along = |l: &DynLight| (Vec3::from(l.pos) - eye).dot(dir);
                assert!(along(low) > 8.0 && along(high) > 30.0);
                assert!(low.range > 30.0 && high.range > 80.0);
            }
        }
        assert!(glow > 0.9 && lit > 0, "{glow} {lit}");
        assert!(flash > 0.1 && flash <= SKY_STRIKE_FLASH + 1e-3, "{flash}");
        // La foudre qui tombe : tout son côté du ciel, comme avant.
        s.strike(Vec3::new(60.5, 65.0, 0.5));
        let mut wide = false;
        for _ in 0..300 {
            s.update(1.0 / 200.0, &frame(&world, feet));
            let [x, _, level, rise, focus] = s.sky_flash;
            if level > 0.0 {
                wide = true;
                assert!(focus == 0.0 && rise == 0.0 && x > 0.99, "{:?}", s.sky_flash);
            }
        }
        assert!(wide);
    }

    /// Orage : la foudre tombe d'elle-même autour du joueur, sur le dessus du relief, et
    /// frappe la tour plutôt que le sol à côté. Pas de météo en mode FPS.
    #[test]
    fn storms_strike_the_ground_and_prefer_the_tower() {
        let mut world = flat(64);
        for y in 65..95 {
            world.set(8, y, 8, BlockId::STONE);
        }
        assert_eq!(column_top(&world, 8, 8), Some(94));
        assert_eq!(column_top(&world, 0, 0), Some(64));
        assert_eq!(column_top(&world, 5000, 0), None);
        let relief = WorldRelief(&world);
        assert_eq!(relief.chunk_top(0, 0), Some(94));
        assert_eq!(relief.chunk_top(1, 1), Some(64));
        assert_eq!(weather::attach(20, 12, &relief), Some((8, 8, 94)));
        let mut s = Storm::new(7);
        let feet = Vec3::new(8.5, 65.0, 0.5);
        s.update(0.05, &frame(&world, feet));
        s.weather.force(weather::Kind::Storm);
        s.weather.fix(true);
        (
            s.weather.cover,
            s.weather.rain_amount,
            s.weather.storm_amount,
        ) = (1.0, 1.0, 1.0);
        let (mut on_tower, mut on_ground, mut seen) = (0, 0, std::collections::HashSet::new());
        for _ in 0..40_000 {
            s.update(0.05, &frame(&world, feet));
            // (Les éclairs des nuages ne tombent pas : on ne compte que la foudre.)
            for k in s.strikes.iter().filter(|k| k.kind == lightning::Kind::Ground) {
                if seen.insert(k.seed) {
                    if k.pos == Vec3::new(8.5, 95.0, 8.5) {
                        on_tower += 1;
                    } else {
                        assert_eq!(k.pos.y, 65.0, "sur le dessus du sol");
                        on_ground += 1;
                    }
                }
            }
        }
        // Le monde d'essai fait 80 blocs de côté (le reste n'est pas chargé : rien n'y
        // tombe) ; la tour de 30 blocs capte ce qui vise à moins de 90 blocs d'elle.
        assert!(
            on_tower >= 5 && on_ground == 0,
            "{on_tower} sur la tour, {on_ground} au sol"
        );
        assert!(s.rain(1.0).amount == 1.0 && s.rain(1.0).tops.is_some());
        // Mode FPS : beau temps fixe, pas de pluie, pas de foudre.
        let mut calm = Storm::new(7);
        calm.weather.force(weather::Kind::Storm);
        let mut f = frame(&world, feet);
        f.active = false;
        for _ in 0..2000 {
            assert_eq!(calm.update(0.05, &f), 0);
        }
        assert!(calm.strikes.is_empty() && calm.rain(1.0).amount == 0.0);
        assert_eq!(calm.sky().cover, SkyWeather::default().cover);
        assert_eq!((calm.light(), calm.visibility()), (1.0, 1.0));
    }

    /// La carte des toits suit le joueur et les blocs posés au-dessus de lui.
    #[test]
    fn the_roof_map_follows_the_player_and_the_blocks() {
        let mut world = flat(64);
        let mut t = Tops::new();
        let at = |t: &Tops, x: i32, z: i32| {
            t.heights[((z - t.origin[1]) * RAIN_TOPS_N as i32 + x - t.origin[0]) as usize]
        };
        t.refresh(&world, Vec3::new(0.5, 70.0, 0.5));
        assert!(t.valid && t.origin == [-32, -32]);
        assert_eq!(at(&t, 3, 3), 65.0);
        assert_eq!(at(&t, -32, -32), 65.0);
        let v = t.version;
        // Rien ne change : pas de nouvel envoi.
        for _ in 0..40 {
            t.refresh(&world, Vec3::new(4.5, 70.0, 2.5));
        }
        assert_eq!(t.version, v);
        // Un toit posé : vu en 16 images au plus.
        world.set(3, 80, 3, BlockId::STONE);
        for _ in 0..16 {
            t.refresh(&world, Vec3::new(4.5, 70.0, 2.5));
        }
        assert_eq!(at(&t, 3, 3), 81.0);
        assert!(t.version > v);
        // Parti à 11 blocs : recentrée d'un coup.
        t.refresh(&world, Vec3::new(11.5, 70.0, 0.5));
        assert_eq!(t.origin, [-21, -32]);
        assert_eq!(at(&t, 3, 3), 81.0);
        assert_eq!(at(&t, 42, 0), 65.0);
    }
}
