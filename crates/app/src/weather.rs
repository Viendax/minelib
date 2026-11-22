//! (v216) Météo : beau temps, pluie, orage.
//!
//! Le rythme est celui de Minecraft (20 tics par seconde), avec ses deux horloges
//! indépendantes :
//! - la pluie : une averse de 10 à 20 min (12 000 à 24 000 tics), puis 10 min à 2 h 30 de
//!   temps sec (12 000 à 180 000 tics) ;
//! - le tonnerre : 3 à 13 min (3 600 à 15 600 tics), puis 10 min à 2 h 30 sans.
//!
//! Il y a orage quand il pleut et qu'il tonne à la fois. Pendant un orage la foudre tombe
//! comme dans Minecraft : une chance sur 100 000 par chunk et par tic, soit, sur les
//! 441 chunks autour d'un joueur, un éclair toutes les 11 s en moyenne dans un carré de
//! 336 blocs de côté.
//!
//! La météo est une fonction de la graine du monde et d'une horloge : tout le monde y lit
//! le même temps au même moment, sans rien s'échanger. En solo l'horloge est le temps joué
//! dans ce monde (enregistré avec lui) ; sur un serveur de construction c'est l'heure du
//! système, la même pour tous les joueurs. `/weather` impose un temps pour une durée tirée
//! comme dans Minecraft, `/weather fix` le fige. (v229) Sur un serveur de construction, c'est
//! un OP qui l'impose, pour tous ses joueurs : le relais le dit à chacun (`impose`), et à
//! ceux qui arrivent.
//!
//! Ce que voit le rendu est lissé : les nuages s'épaississent en une minute, la pluie
//! commence quand le ciel est couvert, l'orage s'assombrit ensuite ; dans l'autre sens la
//! pluie cesse d'abord, puis le ciel se dégage.

use glam::Vec3;
use minelib_render::sky::SkyWeather;

/// Le temps qu'il fait.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Clear,
    Rain,
    Storm,
}

impl Kind {
    /// Mot de la commande `/weather` (français ou anglais).
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "clear" | "sun" | "sunny" | "beau" | "soleil" | "degage" | "dégagé" => {
                Some(Kind::Clear)
            }
            "rain" | "pluie" => Some(Kind::Rain),
            "storm" | "thunder" | "orage" | "tonnerre" => Some(Kind::Storm),
            _ => None,
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Kind::Clear => "clear",
            Kind::Rain => "rain",
            Kind::Storm => "storm",
        }
    }

    /// (v229) Sur le réseau (`proto::WEATHER_…`).
    pub fn from_byte(b: u8) -> Option<Self> {
        use minelib_net::proto::{WEATHER_CLEAR, WEATHER_RAIN, WEATHER_STORM};
        match b {
            WEATHER_CLEAR => Some(Kind::Clear),
            WEATHER_RAIN => Some(Kind::Rain),
            WEATHER_STORM => Some(Kind::Storm),
            _ => None,
        }
    }

    /// Nom montré au joueur.
    pub fn label(self) -> &'static str {
        match self {
            Kind::Clear => "beau temps",
            Kind::Rain => "pluie",
            Kind::Storm => "orage",
        }
    }
}

/// Durées de Minecraft, en secondes (tics / 20) : averse, temps sec, tonnerre, sans
/// tonnerre.
pub const RAIN_ON_S: (f64, f64) = (600.0, 1200.0);
pub const RAIN_OFF_S: (f64, f64) = (600.0, 9000.0);
pub const THUNDER_ON_S: (f64, f64) = (180.0, 780.0);
pub const THUNDER_OFF_S: (f64, f64) = (600.0, 9000.0);

/// Foudre de Minecraft : une chance sur 100 000 par chunk (16 × 16 blocs) et par tic
/// (20 par seconde). Par bloc carré et par seconde :
pub const STRIKES_PER_BLOCK_S: f64 = 20.0 / (100_000.0 * 256.0);
/// Elle tombe sur les chunks actifs autour du joueur : à 10 chunks à la ronde, un carré
/// de 21 chunks de côté.
pub const STRIKE_REACH: f32 = 168.0;
/// Cases du monde (blocs) dans lesquelles la foudre est tirée, seconde par seconde.
const CELL: i32 = 48;

/// En ligne, l'horloge de la météo compte depuis le 1er janvier 2026 (UTC).
const ONLINE_EPOCH_S: f64 = 1_767_225_600.0;

fn mix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

/// Nombre de 0 à 1 (exclu) tiré de `seed` et de deux rangs.
fn unit(seed: u64, a: u64, b: u64) -> f64 {
    let h = mix64(mix64(seed ^ a.wrapping_mul(0x9e37_79b9_7f4a_7c15)) ^ b);
    (h >> 11) as f64 / (1u64 << 53) as f64
}

/// Une des deux horloges : des périodes « sans » puis « avec », l'une après l'autre,
/// chacune d'une durée tirée de la graine. Elle garde où elle en est : la suivre d'une
/// image à l'autre ne coûte rien.
#[derive(Clone, Debug)]
struct Cycle {
    seed: u64,
    on: (f64, f64),
    off: (f64, f64),
    /// Rang de la période en cours (paire : sans, impaire : avec) et son début.
    k: u64,
    start: f64,
}

impl Cycle {
    fn new(seed: u64, on: (f64, f64), off: (f64, f64)) -> Self {
        Self {
            seed,
            on,
            off,
            k: 0,
            start: 0.0,
        }
    }

    fn length(&self, k: u64) -> f64 {
        let (lo, hi) = if k % 2 == 1 { self.on } else { self.off };
        lo + (hi - lo) * unit(self.seed, k, 0x5eed)
    }

    /// À l'instant `t` (s) : « avec » ou non, et le temps qu'il reste à cette période.
    fn at(&mut self, t: f64) -> (bool, f64) {
        let t = t.max(0.0);
        if t < self.start {
            self.k = 0;
            self.start = 0.0;
        }
        loop {
            let len = self.length(self.k);
            if t < self.start + len {
                return (self.k % 2 == 1, self.start + len - t);
            }
            self.start += len;
            self.k += 1;
        }
    }
}

/// Bruit lisse à une dimension (0 … 1).
fn noise1(seed: u64, x: f64) -> f32 {
    let i = x.floor();
    let f = (x - i) as f32;
    let u = f * f * (3.0 - 2.0 * f);
    let a = unit(seed, i as i64 as u64, 0xc10d) as f32;
    let b = unit(seed, (i as i64 + 1) as u64, 0xc10d) as f32;
    a + (b - a) * u
}

fn smoothstep(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// (v241) Éclairs qui courent dans les nuages au-dessus d'une zone pendant l'orage (ce sont
/// eux qui chargent la Foudre de Zeus de celui qui la porte en dessous). Tirés de la graine
/// et de l'horloge, par zone de `CLOUD_ZONE` blocs de côté et par créneau de `CLOUD_SLOT_S`
/// secondes : les mêmes pour tous, **quel que soit le nombre de joueurs dans la zone**.
pub const CLOUD_ZONE: i32 = 256;
pub const CLOUD_SLOT_S: f64 = 15.0;
/// Chance par zone et par créneau, orage installé : un toutes les deux minutes en moyenne
/// au-dessus d'une zone (« ça doit quand même arriver un peu »).
pub const CLOUD_P: f64 = 1.0 / 8.0;
/// Autour de celui qui porte la Foudre de Zeus (à moins de `zeus::CHARGE_REACH` blocs de
/// lui) : trois fois plus souvent.
pub const CLOUD_BOOST: f64 = 3.0;
/// On les tire jusqu'à cette distance de la caméra (blocs) : une dizaine de zones, soit
/// cinq éclairs par minute environ (et autant des éclairs des nuages d'avant, tirés au
/// hasard chez chacun : voir `Storm::update`).
const CLOUD_REACH: f32 = 448.0;

/// Un éclair que le ciel envoie : la colonne visée (avant que le relief ne l'attire), le
/// moment (s après l'appel) et de quoi tirer sa forme.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spot {
    pub x: f32,
    pub z: f32,
    pub delay: f32,
    pub seed: u32,
}

pub struct Weather {
    seed: u64,
    rain: Cycle,
    thunder: Cycle,
    /// Horloge de la météo (s).
    pub clock: f64,
    /// `/weather` : temps imposé jusqu'à cette heure de l'horloge (infinie : figé).
    forced: Option<(Kind, f64)>,
    /// `/weather fix` : le temps ne change plus tout seul.
    pub fixed: bool,
    /// Ce que montre le ciel, lissé : nuages (part du ciel), pluie et orage (0 … 1).
    pub cover: f32,
    pub rain_amount: f32,
    pub storm_amount: f32,
    /// Faux tant que les valeurs lissées n'ont pas été posées (monde qu'on vient d'ouvrir :
    /// pas de transition, le temps est déjà là).
    settled: bool,
    /// Dernière seconde dont la foudre a été tirée.
    last_slot: Option<i64>,
    /// (v241) Dernier créneau des éclairs des nuages déjà tiré.
    last_cloud_slot: Option<i64>,
    /// (v229) Ce que le serveur vient d'imposer (`impose`), posé à la prochaine `update`
    /// (l'horloge y est celle du moment).
    imposed: Option<(Option<Kind>, Option<f64>, bool)>,
}

impl Weather {
    pub fn new(seed: u64) -> Self {
        Self {
            seed,
            rain: Cycle::new(mix64(seed ^ 0x7261_696e), RAIN_ON_S, RAIN_OFF_S),
            thunder: Cycle::new(mix64(seed ^ 0x7468_756e), THUNDER_ON_S, THUNDER_OFF_S),
            clock: 0.0,
            forced: None,
            fixed: false,
            cover: 0.3,
            rain_amount: 0.0,
            storm_amount: 0.0,
            settled: false,
            last_slot: None,
            last_cloud_slot: None,
            imposed: None,
        }
    }

    /// Le temps que donnent les deux horloges à l'instant `t`, et combien il dure encore.
    pub fn natural_at(&mut self, t: f64) -> (Kind, f64) {
        let (rain, rain_left) = self.rain.at(t);
        let (thunder, thunder_left) = self.thunder.at(t);
        match (rain, thunder) {
            (true, true) => (Kind::Storm, rain_left.min(thunder_left)),
            (true, false) => (Kind::Rain, rain_left.min(thunder_left)),
            (false, _) => (Kind::Clear, rain_left),
        }
    }

    /// Le temps qu'il fait maintenant (imposé, sinon celui des horloges).
    pub fn kind(&mut self) -> Kind {
        if let Some((k, until)) = self.forced {
            if self.clock < until {
                return k;
            }
            self.forced = None;
        }
        self.natural_at(self.clock).0
    }

    /// `/weather <temps>` : imposé pour une durée tirée comme dans Minecraft (une averse,
    /// un orage ou une période de beau temps de longueur ordinaire) ; sans fin si le temps
    /// est figé.
    pub fn force(&mut self, kind: Kind) {
        let (lo, hi) = match kind {
            Kind::Clear => RAIN_OFF_S,
            Kind::Rain => RAIN_ON_S,
            Kind::Storm => THUNDER_ON_S,
        };
        let len = lo + (hi - lo) * unit(self.seed, self.clock.to_bits(), 0xf0_4ce);
        let until = if self.fixed {
            f64::INFINITY
        } else {
            self.clock + len
        };
        self.forced = Some((kind, until));
    }

    /// (v229) Le serveur de construction dit sa météo (à l'arrivée, et quand un OP la
    /// change) : `kind` imposé pour encore `left` secondes (`None` : sans fin), ou `None` :
    /// celle des horloges du monde. Remplace ce qui était imposé ici.
    pub fn impose(&mut self, kind: Option<Kind>, left: Option<f64>, fixed: bool) {
        self.imposed = Some((kind, left.filter(|l| l.is_finite()), fixed && kind.is_some()));
    }

    /// `/weather fix` : figer le temps qu'il fait (ou le laisser changer de nouveau).
    pub fn fix(&mut self, on: bool) {
        if on {
            let k = self.kind();
            self.forced = Some((k, f64::INFINITY));
        } else if self.fixed {
            self.forced = None;
        }
        self.fixed = on;
    }

    /// Avance la météo de `dt` secondes. `unix` : l'heure du système (s), en ligne (elle
    /// sert alors d'horloge, la même pour tous les joueurs) ; sinon l'horloge du monde
    /// avance de `dt`.
    pub fn update(&mut self, dt: f32, unix: Option<f64>) {
        match unix {
            Some(u) => self.clock = (u - ONLINE_EPOCH_S).max(0.0),
            None => self.clock += dt as f64,
        }
        if let Some((kind, left, fixed)) = self.imposed.take() {
            self.fixed = fixed;
            self.forced = kind.map(|k| (k, left.map_or(f64::INFINITY, |l| self.clock + l.max(0.0))));
        }
        let kind = self.kind();
        // Par beau temps : plus ou moins de cumulus, d'une heure à l'autre.
        let fair = 0.14 + 0.32 * noise1(self.seed, self.clock / 600.0);
        let (cover, rain, storm) = match kind {
            Kind::Clear => (fair, 0.0, 0.0),
            Kind::Rain => (1.0, 1.0, 0.0),
            Kind::Storm => (1.0, 1.0, 1.0),
        };
        if !self.settled {
            self.settled = true;
            (self.cover, self.rain_amount, self.storm_amount) = (cover, rain, storm);
            return;
        }
        let toward = |v: &mut f32, target: f32, step: f32| {
            *v = if *v < target {
                (*v + step).min(target)
            } else {
                (*v - step).max(target)
            };
        };
        // Le ciel se couvre en une minute ; il ne se dégage qu'une fois la pluie finie.
        if cover > self.cover {
            toward(&mut self.cover, cover, dt / 60.0);
        } else if self.rain_amount < 0.2 {
            toward(&mut self.cover, cover, dt / 95.0);
        }
        // La pluie commence sous un ciel couvert.
        if rain > self.rain_amount {
            if self.cover > 0.82 {
                toward(&mut self.rain_amount, rain, dt / 22.0);
            }
        } else {
            toward(&mut self.rain_amount, rain, dt / 28.0);
        }
        // L'orage vient avec la pluie.
        if storm > self.storm_amount {
            if self.rain_amount > 0.45 {
                toward(&mut self.storm_amount, storm, dt / 40.0);
            }
        } else {
            toward(&mut self.storm_amount, storm, dt / 50.0);
        }
    }

    /// Ciel couvert (0 beau temps … 1 couvert) : la même mesure que le rendu du ciel.
    pub fn overcast(&self) -> f32 {
        smoothstep(0.55, 0.95, self.cover)
    }

    /// Ce que le rendu du ciel doit savoir.
    pub fn sky(&self) -> SkyWeather {
        // Le vent forcit sous la pluie et dans l'orage.
        let k = 1.0 + 0.5 * self.rain_amount + 1.3 * self.storm_amount;
        SkyWeather {
            cover: self.cover,
            rain: self.rain_amount,
            storm: self.storm_amount,
            wind: [2.6 * k, 0.9 * k],
        }
    }

    /// Part de la lumière du jour qui arrive au sol (1 par beau temps).
    pub fn light(&self) -> f32 {
        1.0 - 0.26 * self.overcast() - 0.12 * self.rain_amount - 0.24 * self.storm_amount
    }

    /// Distance de vue dans la pluie (1 par beau temps).
    pub fn visibility(&self) -> f32 {
        1.0 - 0.3 * self.rain_amount - 0.15 * self.storm_amount
    }

    /// La foudre tombe-t-elle (orage installé) ?
    pub fn thundering(&self) -> bool {
        self.storm_amount > 0.55
    }

    /// Éclairs que le ciel envoie autour de `centre` depuis le dernier appel. Tirés seconde
    /// par seconde et case par case de la graine : les mêmes pour tous les joueurs d'un
    /// même monde.
    pub fn strikes(&mut self, centre: Vec3) -> Vec<Spot> {
        let slot = self.clock.floor() as i64;
        let last = self.last_slot.replace(slot);
        let mut out = Vec::new();
        let Some(last) = last else {
            return out;
        };
        if !self.thundering() || slot <= last {
            return out;
        }
        let p = STRIKES_PER_BLOCK_S * (CELL * CELL) as f64;
        let cell = |v: f32| (v / CELL as f32).floor() as i32;
        // (Au plus les trois dernières secondes : après une longue image, pas de rafale.)
        for s in (last + 1).max(slot - 2)..=slot {
            for cz in cell(centre.z - STRIKE_REACH)..=cell(centre.z + STRIKE_REACH) {
                for cx in cell(centre.x - STRIKE_REACH)..=cell(centre.x + STRIKE_REACH) {
                    let key = (cx as u32 as u64) << 32 | cz as u32 as u64;
                    if unit(self.seed ^ 0xb017, s as u64, key) >= p {
                        continue;
                    }
                    let x = (cx * CELL) as f32
                        + unit(self.seed ^ 1, s as u64, key) as f32 * CELL as f32;
                    let z = (cz * CELL) as f32
                        + unit(self.seed ^ 2, s as u64, key) as f32 * CELL as f32;
                    if (x - centre.x).abs() > STRIKE_REACH || (z - centre.z).abs() > STRIKE_REACH {
                        continue;
                    }
                    out.push(Spot {
                        x,
                        z,
                        delay: unit(self.seed ^ 3, s as u64, key) as f32,
                        seed: mix64(self.seed ^ key ^ (s as u64) << 20) as u32,
                    });
                }
            }
        }
        out
    }

    /// Pour la sauvegarde du monde : horloge, temps imposé (et le temps qu'il lui reste),
    /// figé ou non.
    pub fn save(&self) -> String {
        let forced = match self.forced {
            Some((k, until)) if until.is_infinite() => format!("{};inf", k.key()),
            Some((k, until)) if until > self.clock => {
                format!("{};{:.0}", k.key(), until - self.clock)
            }
            _ => "-;0".into(),
        };
        format!("{:.1};{forced};{}", self.clock, u8::from(self.fixed))
    }

    /// Relit ce qu'a écrit `save` (une valeur illisible : laissée telle quelle).
    pub fn load(&mut self, s: &str) {
        let mut f = s.split(';');
        if let Some(c) = f.next().and_then(|v| v.parse::<f64>().ok()) {
            if c.is_finite() && c >= 0.0 {
                self.clock = c;
            }
        }
        let kind = f.next().and_then(Kind::parse);
        let left = f.next().unwrap_or("0");
        self.fixed = f.next() == Some("1");
        self.forced = kind.and_then(|k| {
            if left == "inf" {
                Some((k, f64::INFINITY))
            } else {
                let l = left
                    .parse::<f64>()
                    .ok()
                    .filter(|l| l.is_finite() && *l > 0.0)?;
                Some((k, self.clock + l))
            }
        });
        self.settled = false;
        self.last_slot = None;
        self.last_cloud_slot = None;
    }

    /// (v241) Éclairs qui courent dans les nuages autour de `centre` pendant le créneau qui
    /// commence (rien entre deux créneaux) : où (au-dessus de quelle colonne), quand (s après
    /// l'appel) et de quoi tirer leur forme. `holder` : celui qui porte la Foudre de Zeus,
    /// s'il y en a un ; près de lui ils sont trois fois plus fréquents. Les éclairs d'une
    /// zone sans porteur sont aussi ceux qu'elle a avec lui : il en ajoute, il n'en change
    /// aucun.
    pub fn cloud_bolts(&mut self, centre: Vec3, holder: Option<Vec3>) -> Vec<Spot> {
        let slot = (self.clock / CLOUD_SLOT_S).floor() as i64;
        let last = self.last_cloud_slot.replace(slot);
        let mut out = Vec::new();
        if !self.thundering() || last.is_none_or(|l| slot <= l) {
            return out;
        }
        let cell = |v: f32| (v / CLOUD_ZONE as f32).floor() as i32;
        for cz in cell(centre.z - CLOUD_REACH)..=cell(centre.z + CLOUD_REACH) {
            for cx in cell(centre.x - CLOUD_REACH)..=cell(centre.x + CLOUD_REACH) {
                let key = (cx as u32 as u64) << 32 | cz as u32 as u64;
                let s = slot as u64;
                let x = (cx * CLOUD_ZONE) as f32
                    + unit(self.seed ^ 0xc1, s, key) as f32 * CLOUD_ZONE as f32;
                let z = (cz * CLOUD_ZONE) as f32
                    + unit(self.seed ^ 0xc2, s, key) as f32 * CLOUD_ZONE as f32;
                let near = holder.is_some_and(|h| {
                    (h.x - x).hypot(h.z - z) <= crate::zeus::CHARGE_REACH
                });
                let p = if near { CLOUD_P * CLOUD_BOOST } else { CLOUD_P };
                if unit(self.seed ^ 0xc10d, s, key) >= p {
                    continue;
                }
                out.push(Spot {
                    x,
                    z,
                    delay: (unit(self.seed ^ 0xc3, s, key) * CLOUD_SLOT_S) as f32,
                    seed: mix64(self.seed ^ key ^ s << 24 ^ 0xc10d) as u32,
                });
            }
        }
        out
    }
}

/// Heure du système (s depuis 1970), pour l'horloge de la météo en ligne.
pub fn unix_now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

/// Le relief vu par la foudre : hauteur du plus haut bloc d'une colonne, et du plus haut
/// bloc d'un chunk entier (`None` : pas chargé).
pub trait Relief {
    fn top(&self, x: i32, z: i32) -> Option<i32>;
    fn chunk_top(&self, cx: i32, cz: i32) -> Option<i32>;
}

/// Où tombe un éclair visant la colonne (`x`, `z`) : la foudre s'attache au point haut qui
/// la capte. Un point qui dépasse le point visé de `h` blocs capte ce qui serait tombé à
/// moins de 3 `h` de lui (l'aire de capture des normes de protection contre la foudre,
/// CEI 62305 : une pente de 1 pour 3 depuis le sommet) ; un sommet deux fois plus haut
/// reçoit donc quatre fois plus d'éclairs, comme en vrai. Renvoie la colonne frappée et sa
/// hauteur (`None` : la colonne visée n'est pas chargée).
///
/// Toutes les colonnes à moins de 96 blocs comptent, même une antenne d'un seul bloc ;
/// mais un chunk dont le plus haut bloc ne peut pas l'emporter n'est pas parcouru : sur un
/// terrain sans relief, presque rien n'est regardé.
pub fn attach(x: i32, z: i32, relief: &dyn Relief) -> Option<(i32, i32, i32)> {
    const REACH: i32 = 96;
    const C: i32 = minelib_world::CHUNK_SIZE as i32;
    let here = relief.top(x, z)?;
    // Chunks à portée, avec ce que leur plus haut bloc pourrait donner au mieux (s'il
    // était au plus près) : les plus prometteurs d'abord.
    let mut chunks: Vec<(f32, i32, i32)> = Vec::new();
    for cz in (z - REACH).div_euclid(C)..=(z + REACH).div_euclid(C) {
        for cx in (x - REACH).div_euclid(C)..=(x + REACH).div_euclid(C) {
            let Some(t) = relief.chunk_top(cx, cz) else {
                continue;
            };
            let dx = (cx * C - x).max(x - (cx * C + C - 1)).max(0);
            let dz = (cz * C - z).max(z - (cz * C + C - 1)).max(0);
            let near = ((dx * dx + dz * dz) as f32).sqrt();
            let bound = (t - here) as f32 - near / 3.0;
            if bound > 0.0 && near <= REACH as f32 {
                chunks.push((bound, cx, cz));
            }
        }
    }
    chunks.sort_by(|a, b| b.0.total_cmp(&a.0));
    let mut best = (x, z, here);
    let mut best_over = 0.0f32;
    for (bound, cx, cz) in chunks {
        if bound <= best_over {
            break;
        }
        for lz in 0..C {
            for lx in 0..C {
                let (wx, wz) = (cx * C + lx, cz * C + lz);
                let (dx, dz) = (wx - x, wz - z);
                let d2 = dx * dx + dz * dz;
                if d2 == 0 || d2 > REACH * REACH {
                    continue;
                }
                let Some(h) = relief.top(wx, wz) else {
                    continue;
                };
                let over = (h - here) as f32 - (d2 as f32).sqrt() / 3.0;
                if over > best_over {
                    best_over = over;
                    best = (wx, wz, h);
                }
            }
        }
    }
    Some(best)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// (v241) Les éclairs des nuages d'une zone : tirés de la graine et de l'horloge (les
    /// mêmes pour tous, où qu'on soit et combien qu'on soit), une fois par créneau, pendant
    /// l'orage seulement ; trois fois plus fréquents près de celui qui porte la Foudre de
    /// Zeus, qui en ajoute sans changer les autres.
    #[test]
    fn cloud_bolts_come_per_zone_and_three_times_more_near_the_holder() {
        let here = Vec3::new(100.0, 70.0, -40.0);
        let count = |holder: Option<Vec3>, from: Vec3| {
            let mut w = Weather::new(42);
            w.fix(true);
            w.force(Kind::Storm);
            let mut all = Vec::new();
            // Une heure d'orage.
            for _ in 0..3600 {
                w.update(1.0, None);
                all.extend(w.cloud_bolts(from, holder));
            }
            all
        };
        let alone = count(None, here);
        // Près d'ici (à portée de charge) : sans porteur, avec lui.
        let near = |all: &[Spot]| {
            all.iter()
                .filter(|s| (s.x - here.x).hypot(s.z - here.z) <= crate::zeus::CHARGE_REACH)
                .count()
        };
        let held = count(Some(here), here);
        let (n0, n1) = (near(&alone), near(&held));
        assert!(n0 >= 12, "il en vient un peu sans porteur : {n0} en une heure");
        let ratio = n1 as f32 / n0 as f32;
        assert!((2.2..=3.8).contains(&ratio), "{n1} avec lui, {n0} sans : {ratio}");
        // Il en ajoute près de lui, il ne change rien ailleurs ni aux autres.
        assert!(alone.iter().all(|s| held.contains(s)));
        assert_eq!(alone.len() - n0, held.len() - n1);
        // Les mêmes vus d'un peu plus loin (un autre joueur de la zone) : rien de plus.
        let other = count(None, here + Vec3::new(90.0, 0.0, 60.0));
        assert!(alone
            .iter()
            .filter(|s| (s.x - here.x).abs() < 300.0 && (s.z - here.z).abs() < 300.0)
            .all(|s| other.iter().any(|o| o.x == s.x && o.z == s.z && o.seed == s.seed)));
        // Sans orage : aucun.
        let mut calm = Weather::new(42);
        calm.fix(true);
        calm.force(Kind::Clear);
        let mut n = 0;
        for _ in 0..600 {
            calm.update(1.0, None);
            n += calm.cloud_bolts(here, Some(here)).len();
        }
        assert_eq!(n, 0);
    }

    /// Les deux horloges donnent les parts de temps de Minecraft : il pleut 16 % du temps
    /// (15 min toutes les 95 min en moyenne), il tonne 9 % du temps, et il y a orage quand
    /// les deux tombent ensemble (1,4 % du temps).
    #[test]
    fn the_weather_keeps_minecraft_s_odds() {
        let mut w = Weather::new(0x5eed_1234);
        let (mut rain, mut storm, mut n) = (0u32, 0u32, 0u32);
        let mut t = 0.0;
        while t < 4.0e7 {
            match w.natural_at(t).0 {
                Kind::Clear => {}
                Kind::Rain => rain += 1,
                Kind::Storm => {
                    rain += 1;
                    storm += 1;
                }
            }
            n += 1;
            t += 37.0;
        }
        let (rain, storm) = (rain as f64 / n as f64, storm as f64 / n as f64);
        // Averse : 900 s en moyenne ; temps sec : 4800 s.
        assert!((rain - 900.0 / 5700.0).abs() < 0.01, "pluie {rain}");
        // Tonnerre : 480 s sur 5280 ; orage = les deux à la fois.
        let both = 900.0 / 5700.0 * 480.0 / 5280.0;
        assert!(
            (storm - both).abs() < 0.004,
            "orage {storm} (attendu {both})"
        );
    }

    /// Les périodes ont les durées de Minecraft, et la météo est la même pour tous ceux
    /// qui ont la même graine, d'où qu'ils la regardent (en avançant ou d'un coup).
    #[test]
    fn periods_last_as_long_as_minecraft_s_and_depend_only_on_the_seed() {
        let mut c = Cycle::new(77, RAIN_ON_S, RAIN_OFF_S);
        let mut t = 0.0;
        for k in 0..400 {
            let (on, left) = c.at(t);
            assert_eq!(on, k % 2 == 1);
            let (lo, hi) = if on { RAIN_ON_S } else { RAIN_OFF_S };
            assert!(left >= lo && left <= hi, "{left} hors de {lo} … {hi}");
            t += left + 1e-6;
        }
        let mut a = Weather::new(42);
        let mut b = Weather::new(42);
        let mut other = Weather::new(43);
        let mut differs = false;
        for i in 0..2000 {
            let t = i as f64 * 613.0;
            let x = a.natural_at(t);
            // (b : interrogé seulement de temps en temps, et une fois en arrière.)
            if i % 7 == 0 {
                assert_eq!(b.natural_at(t), x);
                assert_eq!(b.natural_at(t * 0.5), a.clone_at(t * 0.5));
            }
            differs |= other.natural_at(t).0 != x.0;
        }
        assert!(differs, "une autre graine : un autre temps");
    }

    impl Weather {
        fn clone_at(&self, t: f64) -> (Kind, f64) {
            Weather::new(self.seed).natural_at(t)
        }
    }

    /// `/weather` impose un temps pour une durée ordinaire puis rend la main ; figé, il
    /// dure ; et tout cela revient tel quel de la sauvegarde.
    #[test]
    fn forced_weather_ends_unless_fixed_and_is_saved() {
        let mut w = Weather::new(9);
        w.clock = 100.0;
        assert_eq!(w.kind(), Kind::Clear);
        w.force(Kind::Storm);
        assert_eq!(w.kind(), Kind::Storm);
        let mut back = Weather::new(9);
        back.load(&w.save());
        assert_eq!(back.kind(), Kind::Storm);
        assert!((back.clock - 100.0).abs() < 0.1);
        // Un orage imposé dure de 3 à 13 min.
        w.clock += 179.0;
        assert_eq!(w.kind(), Kind::Storm);
        w.clock += 602.0;
        assert_eq!(w.kind(), Kind::Clear);
        // Figé : il dure.
        w.force(Kind::Rain);
        w.fix(true);
        w.clock += 1.0e6;
        assert_eq!(w.kind(), Kind::Rain);
        w.force(Kind::Storm);
        w.clock += 1.0e6;
        assert_eq!(w.kind(), Kind::Storm, "changé à la main, toujours figé");
        let mut back = Weather::new(9);
        back.load(&w.save());
        assert!(back.fixed);
        back.clock += 1.0e6;
        assert_eq!(back.kind(), Kind::Storm);
        w.fix(false);
        assert!(!w.fixed);
        let natural = w.clone_at(w.clock).0;
        assert_eq!(w.kind(), natural);
        // Une sauvegarde abîmée ne casse rien.
        let mut odd = Weather::new(9);
        odd.load("n'importe quoi;;;");
        assert_eq!(odd.kind(), Kind::Clear);
    }

    /// (v229) Ce qu'impose le serveur de construction : compté sur l'horloge en ligne (l'heure
    /// du système) du moment où on l'apprend, même reçu avant la première image ; déjà là à
    /// l'arrivée (pas de transition) ; « aucun » efface ce qui était imposé ici.
    #[test]
    fn the_server_weather_replaces_ours_and_ends_on_the_online_clock() {
        let unix = ONLINE_EPOCH_S + 5.0e6;
        // Un monde qu'on avait figé chez soi sur l'orage, puis ouvert en ligne.
        let mut w = Weather::new(9);
        w.force(Kind::Storm);
        w.fix(true);
        w.impose(None, None, false);
        w.update(0.016, Some(unix));
        assert!(!w.fixed);
        let natural = w.clone_at(w.clock).0;
        assert_eq!(w.kind(), natural, "le serveur n'impose rien : les horloges");
        // À l'arrivée : la pluie pour encore 300 s, reçue avant la première image.
        let mut w = Weather::new(9);
        w.impose(Kind::from_byte(1), Some(300.0), false);
        w.update(0.016, Some(unix));
        assert_eq!(w.kind(), Kind::Rain);
        assert_eq!((w.cover, w.rain_amount), (1.0, 1.0), "déjà là, sans transition");
        w.update(0.016, Some(unix + 299.0));
        assert_eq!(w.kind(), Kind::Rain);
        w.update(0.016, Some(unix + 301.0));
        let natural = w.clone_at(w.clock).0;
        assert_eq!(w.kind(), natural, "sa durée passée : les horloges");
        // Un OP fige l'orage : sans fin, et il arrive peu à peu.
        w.impose(Some(Kind::Storm), None, true);
        w.update(0.016, Some(unix + 302.0));
        assert!(w.fixed && w.storm_amount < 0.1);
        w.update(0.016, Some(unix + 1.0e7));
        assert_eq!(w.kind(), Kind::Storm);
        // Un temps inconnu : comme « aucun ».
        assert_eq!(Kind::from_byte(7), None);
        for k in [Kind::Clear, Kind::Rain, Kind::Storm] {
            let b = match k {
                Kind::Clear => minelib_net::proto::WEATHER_CLEAR,
                Kind::Rain => minelib_net::proto::WEATHER_RAIN,
                Kind::Storm => minelib_net::proto::WEATHER_STORM,
            };
            assert_eq!(Kind::from_byte(b), Some(k));
            // (Le relais tire les mêmes durées que `force`.)
            let span = minelib_net::proto::weather_span(b).unwrap();
            let ours = match k {
                Kind::Clear => RAIN_OFF_S,
                Kind::Rain => RAIN_ON_S,
                Kind::Storm => THUNDER_ON_S,
            };
            assert_eq!((span.0 as f64, span.1 as f64), ours);
        }
    }

    /// De beau temps à l'orage : les nuages d'abord, puis la pluie, puis l'orage, sans
    /// saut ; et dans l'autre sens la pluie cesse avant que le ciel ne se dégage.
    #[test]
    fn the_sky_changes_in_order_and_without_jumps() {
        let mut w = Weather::new(3);
        w.update(0.1, None);
        assert!(w.cover < 0.5 && w.rain_amount == 0.0);
        w.force(Kind::Storm);
        let (mut rain_at, mut storm_at, mut covered_at) = (None, None, None);
        let mut prev = (w.cover, w.rain_amount, w.storm_amount);
        for i in 0..4000 {
            w.update(0.05, None);
            let now = (w.cover, w.rain_amount, w.storm_amount);
            assert!((now.0 - prev.0).abs() < 0.002 && (now.1 - prev.1).abs() < 0.004);
            assert!(now.0 >= prev.0 - 1e-6, "les nuages ne reculent pas");
            prev = now;
            let t = i as f32 * 0.05;
            if w.cover > 0.99 && covered_at.is_none() {
                covered_at = Some(t);
            }
            if w.rain_amount > 0.01 && rain_at.is_none() {
                rain_at = Some(t);
                assert!(w.cover > 0.8, "la pluie attend le ciel couvert");
            }
            if w.storm_amount > 0.01 && storm_at.is_none() {
                storm_at = Some(t);
                assert!(w.rain_amount > 0.4);
            }
        }
        assert!(w.thundering() && w.light() < 0.45);
        let (c, r, s) = (covered_at.unwrap(), rain_at.unwrap(), storm_at.unwrap());
        assert!(r < c && r < s && c < 60.0 && s < 80.0, "{c} {r} {s}");
        // Et retour : la pluie cesse, puis le ciel se dégage.
        w.force(Kind::Clear);
        let mut cleared_rain = None;
        for i in 0..4000 {
            let before = w.cover;
            w.update(0.05, None);
            if w.rain_amount >= 0.2 {
                assert_eq!(w.cover, before, "couvert tant qu'il pleut");
            } else if cleared_rain.is_none() {
                cleared_rain = Some(i);
            }
        }
        assert!(cleared_rain.is_some() && w.cover < 0.5 && w.light() > 0.99);
    }

    /// La foudre tombe au rythme de Minecraft (un éclair toutes les 11,3 s dans le carré de
    /// 336 blocs), partout dans ce carré, et aux mêmes endroits pour deux joueurs voisins.
    #[test]
    fn lightning_falls_at_minecraft_s_rate_and_is_shared() {
        let make = || {
            let mut w = Weather::new(0xabc);
            w.update(0.1, None);
            w.force(Kind::Storm);
            w.fix(true);
            (w.cover, w.rain_amount, w.storm_amount) = (1.0, 1.0, 1.0);
            w
        };
        let (mut a, mut b) = (make(), make());
        let (pa, pb) = (Vec3::new(10.0, 70.0, -30.0), Vec3::new(60.0, 90.0, 20.0));
        let (mut n, mut shared, mut seen_b) = (0usize, 0usize, Vec::new());
        let secs = 40_000;
        let mut far = 0.0f32;
        for _ in 0..secs {
            a.update(1.0, None);
            b.update(1.0, None);
            let sa = a.strikes(pa);
            seen_b.extend(b.strikes(pb));
            for s in &sa {
                assert!((0.0..1.0).contains(&s.delay));
                far = far.max((s.x - pa.x).abs()).max((s.z - pa.z).abs());
                // Dans le carré des deux joueurs : le second l'a tiré aussi.
                if (s.x - pb.x).abs() < STRIKE_REACH - 1.0
                    && (s.z - pb.z).abs() < STRIKE_REACH - 1.0
                {
                    shared += 1;
                    assert!(seen_b.contains(s), "{s:?} pas vu du voisin");
                }
            }
            n += sa.len();
        }
        let expected = secs as f64 * STRIKES_PER_BLOCK_S * (2.0 * STRIKE_REACH as f64).powi(2);
        assert!(
            (n as f64 - expected).abs() < expected * 0.08,
            "{n} éclairs, {expected:.0} attendus"
        );
        assert!((secs as f64 / n as f64 - 11.3).abs() < 1.0);
        assert!(far > STRIKE_REACH * 0.97 && shared > n / 3);
        // Pas d'orage : pas de foudre.
        let mut calm = Weather::new(0xabc);
        calm.update(0.1, None);
        for _ in 0..500 {
            calm.update(1.0, None);
            assert!(calm.strikes(pa).is_empty());
        }
    }

    /// Relief d'essai : une fonction qui donne la hauteur de chaque colonne.
    struct Land<F: Fn(i32, i32) -> Option<i32>>(F);

    impl<F: Fn(i32, i32) -> Option<i32>> Relief for Land<F> {
        fn top(&self, x: i32, z: i32) -> Option<i32> {
            (self.0)(x, z)
        }

        fn chunk_top(&self, cx: i32, cz: i32) -> Option<i32> {
            (0..256)
                .filter_map(|i| (self.0)(cx * 16 + i % 16, cz * 16 + i / 16))
                .max()
        }
    }

    /// La foudre s'attache aux points hauts, en proportion de leur hauteur : une tour de
    /// 30 blocs capte ce qui tombe à moins de 90 blocs d'elle, une tour deux fois plus
    /// haute quatre fois plus d'éclairs — même large d'un seul bloc ; sur un terrain plat
    /// elle tombe où elle vise.
    #[test]
    fn lightning_strikes_high_points_in_proportion() {
        assert_eq!(attach(5, -7, &Land(|_, _| Some(64))), Some((5, -7, 64)));
        assert_eq!(attach(5, -7, &Land(|_, _| None)), None);
        let hits = |h: i32| {
            let t = Land(move |x: i32, z: i32| Some(if (x, z) == (3, -2) { 64 + h } else { 64 }));
            let mut n = 0;
            for z in (-200..=200).step_by(5) {
                for x in (-200..=200).step_by(5) {
                    n += usize::from(attach(x, z, &t) == Some((3, -2, 64 + h)));
                }
            }
            n as f32
        };
        let (h10, h20) = (hits(10), hits(20));
        // Aire de capture : π (3 h)² (comptée sur une grille de 5 blocs).
        let area = |h: f32| std::f32::consts::PI * (3.0 * h) * (3.0 * h) / 25.0;
        assert!(
            (h10 / area(10.0) - 1.0).abs() < 0.12,
            "{h10} pour {}",
            area(10.0)
        );
        assert!(
            (h20 / area(20.0) - 1.0).abs() < 0.12,
            "{h20} pour {}",
            area(20.0)
        );
        assert!((h20 / h10 - 4.0).abs() < 0.4);
        // Deux sommets : celui qui dépasse le plus la pente de 1 pour 3 l'emporte (le
        // traceur qui descend arrive d'abord à portée de celui-là).
        let two = Land(|x: i32, z: i32| {
            Some(match (x, z) {
                (0, 0) => 84,
                (80, 0) => 100,
                _ => 64,
            })
        });
        assert_eq!(attach(2, 0, &two), Some((0, 0, 84)), "tout près du petit");
        assert_eq!(attach(60, 0, &two), Some((80, 0, 100)));
        assert_eq!(
            attach(36, 0, &two),
            Some((80, 0, 100)),
            "le grand capte de plus loin"
        );
        // Une colline : la foudre vise son flanc, elle frappe plus haut.
        let hill = Land(|x: i32, z: i32| {
            let d = ((x * x + z * z) as f32).sqrt();
            Some(64 + (40.0 - d * 0.8).max(0.0) as i32)
        });
        let (hx, hz, hy) = attach(30, 0, &hill).unwrap();
        assert!(
            hy > 64 + 16 && hx * hx + hz * hz < 30 * 30,
            "({hx}, {hz}, {hy})"
        );
    }
}
