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
    /// Segments à dessiner cette image : dans le monde, et à la première personne.
    pub bolts: Vec<BoltSeg>,
    pub hand_bolts: Vec<BoltSeg>,
    /// Éclat de la foudre là où est le joueur (0 : rien … 1 : tombée tout près).
    pub flash: f32,
    /// Lueur dans les nuages : côté d'où elle vient (x, z) et force.
    pub sky_flash: [f32; 3],
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
}

impl Storm {
    pub fn new(seed: u64) -> Self {
        Self {
            weather: Weather::new(seed),
            strikes: Vec::new(),
            held: HeldBolt::default(),
            bolts: Vec::new(),
            hand_bolts: Vec::new(),
            flash: 0.0,
            sky_flash: [0.0; 3],
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
        }
    }

    /// Une image : la météo avance, la foudre tombe, les éclairs en cours se déroulent.
    /// Renvoie les dégâts que le joueur prend (foudre tombée sur lui).
    pub fn update(&mut self, dt: f32, f: &Frame) -> u8 {
        self.bolts.clear();
        self.hand_bolts.clear();
        self.active = f.active;
        self.cooldown = (self.cooldown - dt).max(0.0);
        if f.active {
            self.weather.update(dt, f.online.then(weather::unix_now));
            // La foudre du ciel : elle vise une colonne, le relief l'attire.
            for spot in self.weather.strikes(f.feet) {
                let (x, z) = (spot.x.floor() as i32, spot.z.floor() as i32);
                if let Some((x, z, top)) = weather::attach(x, z, &WorldRelief(f.world)) {
                    let pos = Vec3::new(x as f32 + 0.5, (top + 1) as f32, z as f32 + 0.5);
                    self.strikes
                        .push(Strike::new(pos, spot.seed, cloud_y(pos), spot.delay));
                }
            }
        }
        let mut hurt = 0u8;
        for s in &mut self.strikes {
            if s.update(dt) {
                crate::audio::thunder((s.pos - f.eye).length(), s.seed);
                if s.hurts(f.feet) {
                    hurt = hurt.saturating_add(lightning::DAMAGE);
                }
            }
        }
        self.strikes.retain(|s| !s.done());
        // Éclat sur le monde (il faiblit avec la distance) et lueur dans les nuages, du
        // côté de l'éclair le plus fort.
        let mut best = (0.0f32, [0.0f32; 2]);
        for s in &self.strikes {
            s.segments(&mut self.bolts);
            let l = s.light();
            if l <= 0.0 {
                continue;
            }
            let d = Vec3::new(s.pos.x - f.eye.x, 0.0, s.pos.z - f.eye.z);
            let level = l / (1.0 + (d.length() / 150.0).powi(2));
            if level > best.0 {
                let n = d.normalize_or_zero();
                best = (level, [n.x, n.z]);
            }
        }
        self.flash = best.0;
        let mut sky = (best.0, best.1);
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
            }
        }
        if let Some(g) = &self.glow {
            let l = g.light();
            // (La lueur des nuages éclaire un peu le sol aussi.)
            self.flash = self.flash.max(l * 0.22);
            if l > sky.0 {
                sky = (l, g.dir);
            }
        }
        self.sky_flash = [sky.1[0], sky.1[1], sky.0.min(1.0)];
        // Pluie : la carte des toits, et son bruit (assourdi sous un toit).
        let rain = if f.active {
            self.weather.rain_amount
        } else {
            0.0
        };
        if rain > 0.002 {
            self.tops.refresh(f.world, f.eye);
        }
        let volume = if rain > 0.01 {
            rain.powf(1.3)
                * (0.45 + 0.2 * self.weather.storm_amount)
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
        self.strikes.push(Strike::new(pos, seed, cloud_y(pos), 0.0));
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
            if l < 0.03 || (s.pos - eye).length() > 110.0 {
                continue;
            }
            out.push(DynLight {
                id: STRIKE_LIGHT_ID + (k as u64 & 0xff),
                kind: DynKind::Point,
                pos: (s.pos + Vec3::Y * 2.5).to_array(),
                color: BOLT_LIGHT,
                intensity: 16.0 * l,
                range: 26.0,
                // (Pas d'ombres : un éclair dure un tiers de seconde, leur calcul ferait
                // sauter une image.)
                shadows: false,
                beam: 0.0,
                ..Default::default()
            });
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
            intensity: 2.4 * self.held.glow(),
            range: 12.0,
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

/// D'où part un éclair qui frappe `pos` : la base des nuages, toujours bien au-dessus.
fn cloud_y(pos: Vec3) -> f32 {
    CLOUD_Y.max(pos.y + 160.0)
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
            assert!(l.iter().all(|l| !l.shadows && l.intensity <= 16.0));
        }
        assert_eq!(hurt, lightning::DAMAGE as u32);
        assert!(
            flash > 0.9 && lights == 1 && segs > 150,
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
            for k in &s.strikes {
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
