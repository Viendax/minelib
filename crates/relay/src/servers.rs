//! Serveurs de construction : leurs OP et leurs bannis. Le créateur du serveur est OP
//! d'office (on ne peut ni le bannir ni lui retirer l'OP) ; il nomme les autres OP. Les
//! OP bannissent et débannissent. Gardé par le relais (pas par l'hôte du moment : le
//! serveur peut changer de propriétaire en cours de route), par créateur et par monde.
//! (v238) Un OP nommé peut aussi nommer un OP : un au plus toutes les `OP_WAIT` secondes,
//! et pas avant `OP_WAIT` secondes après l'être devenu ; seul le créateur retire l'OP.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// (v244) La Foudre de Zeus légendaire d'un serveur, tenue par le relais du monde : qui l'a
/// (minuscules ; vide : personne ne l'a prise), ses charges, les coups vers le ciel en
/// attente, et la graine du monde (dite par son créateur : le relais en tire les éclairs des
/// nuages qui la chargent).
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Legend {
    pub holder: String,
    pub charges: u8,
    pub sky_hits: u8,
    pub seed: Option<u32>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Admin {
    /// OP nommés (sans le créateur), en minuscules.
    pub ops: Vec<String>,
    /// Bannis : (pseudo en minuscules, raison).
    pub bans: Vec<(String, String)>,
}

/// Liste blanche d'un serveur : quand elle est active, seuls le créateur, ses OP et les
/// pseudos de la liste entrent. Gardée à part (fichier `….whitelist`) : l'ancien fichier
/// des OP reste lisible.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Whitelist {
    pub on: bool,
    /// Pseudos en minuscules.
    pub names: Vec<String>,
}

/// (v318) Lit un fichier de places en double précision (`None` : absent ou illisible).
fn read_places<T: serde::de::DeserializeOwned>(path: &std::path::Path) -> Option<T> {
    postcard::from_bytes(&std::fs::read(path).ok()?).ok()
}

pub struct Store {
    path: PathBuf,
    servers: HashMap<String, Admin>,
    whitelists: HashMap<String, Whitelist>,
    /// Mode de jeu par défaut de chaque serveur (fichier `….defaults`).
    defaults: HashMap<String, String>,
    /// Dernière position de chaque joueur sur chaque serveur (fichier `….positions`) :
    /// serveur → pseudo (minuscules) → [x, y, z, lacet, tangage]. Le relais en est le
    /// gardien : on revient là où on était, quoi qu'on ait fait ailleurs entre-temps.
    positions: HashMap<String, HashMap<String, [f64; 5]>>,
    /// Positions changées depuis le dernier enregistrement, et ce dernier enregistrement.
    positions_dirty: bool,
    positions_saved: std::time::Instant,
    /// Heure de chaque serveur (fichier `….times`) : (heure à l'instant `since` en
    /// secondes Unix, arrêtée). Elle avance comme dans Minecraft (24 h en 20 minutes).
    times: HashMap<String, (f32, u64, bool)>,
    /// (v229) Météo imposée de chaque serveur (fichier `….weather`) : (temps
    /// `proto::WEATHER_…`, jusqu'à cette heure du système (s), `u64::MAX` : sans fin,
    /// figée). Absente ou passée : la météo des horloges du monde.
    weathers: HashMap<String, (u8, u64, bool)>,
    /// (v223) Point d'apparition de chaque serveur (fichier `….spawns`) : [x, y, z, lacet,
    /// tangage], pour qui y arrive la première fois.
    spawns: HashMap<String, [f64; 5]>,
    /// (v238) Serveur → OP nommé (minuscules) → quand il pourra de nouveau nommer un OP
    /// (`OP_WAIT` après en avoir nommé un, ou après l'être devenu). En mémoire seulement.
    op_ready: HashMap<String, HashMap<String, std::time::Instant>>,
    /// (v244) Foudre de Zeus de chaque serveur (fichier `….foudre`).
    legends: HashMap<String, Legend>,
    /// (v328) Serveurs dont la graine est déverrouillée par un OP (fichier `….graine`) ;
    /// les autres l'ont verrouillée (par défaut).
    seeds_open: std::collections::BTreeSet<String>,
    /// (v330) Graine et clé de terrain de chaque monde (fichier `….graines`), dites par son
    /// hôte en l'ouvrant : elles ne sont plus dans sa carte. La clé va à qui le rejoint, la
    /// graine à ses OP seulement.
    world_seeds: HashMap<String, WorldSeed>,
}

/// (v330) Graine d'un monde (`minelib_net::graine` ; celle d'un monde d'avant la v330 tient
/// sur 32 bits) et clé de son terrain (d'un monde d'avant, la graine elle-même) ; 0 : pas
/// connue.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WorldSeed {
    pub seed: u64,
    pub terrain: u64,
}

/// (v238) Un OP nommé (pas le créateur) nomme un OP au plus une fois toutes les… et pas
/// avant ce temps après l'être devenu (s).
pub const OP_WAIT_S: u64 = 30;
const OP_WAIT: std::time::Duration = std::time::Duration::from_secs(OP_WAIT_S);

/// Positions des joueurs écrites sur le disque au plus toutes les…
const POSITIONS_EVERY: std::time::Duration = std::time::Duration::from_secs(10);

/// Heure d'un monde neuf, et durée d'une journée (s).
pub const START_HOUR: f32 = 8.0;
pub const DAY_S: f32 = 1200.0;

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Clé d'un serveur : son créateur et son monde.
pub fn key(creator: &str, world: &str) -> String {
    format!("{}/{}", creator.to_ascii_lowercase(), world)
}

/// Action d'un OP.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Action {
    Ban,
    Unban,
    Op,
    Deop,
    List,
    /// Tuer un joueur (OP) : rien à garder, juste la permission.
    Kill,
    /// Liste blanche (OP).
    WhitelistOn,
    WhitelistOff,
    WhitelistAdd,
    WhitelistRemove,
    WhitelistList,
    /// Changer de mode de jeu (OP) : `target` = qui, `reason` = le mode.
    Gamemode,
    /// Mode de jeu par défaut à l'arrivée (OP) : `reason` = le mode (vide : aucun).
    DefaultGamemode,
    /// Renommer le serveur (OP) : `reason` = le nom.
    Rename,
    /// Heure (OP) : `reason` = l'heure (0..24) ; l'arrêter ou la relancer (`reason` 1/0).
    TimeSet,
    TimeFix,
    /// (v229) Météo du serveur (OP) : `reason` = voir `proto::SERVER_WEATHER`.
    Weather,
    /// (v223) Téléporter (OP) : `target` = qui (vide : soi-même), `reason` = « x y z » ou
    /// « @pseudo ». Rien à garder : la permission, et le relais envoie la position.
    Teleport,
    /// (v223) Point d'apparition du serveur (OP) : `reason` = « x y z lacet tangage ».
    SetSpawn,
    /// (v328) Verrouiller la graine du monde ou non (OP) : `reason` = « lock » ou « unlock ».
    Seed,
}

/// Le relais s'arrête : les dernières positions sont écrites.
impl Drop for Store {
    fn drop(&mut self) {
        self.save_positions_now();
    }
}

impl Store {
    pub fn open(path: PathBuf) -> Self {
        let servers = std::fs::read(&path)
            .ok()
            .and_then(|b| postcard::from_bytes(&b).ok())
            .unwrap_or_default();
        let whitelists = std::fs::read(Self::wl_path(&path))
            .ok()
            .and_then(|b| postcard::from_bytes(&b).ok())
            .unwrap_or_default();
        let defaults = std::fs::read(path.with_extension("defaults"))
            .ok()
            .and_then(|b| postcard::from_bytes(&b).ok())
            .unwrap_or_default();
        // (v318) Places en double précision (fichier `….positions64`) ; celui d'avant
        // (simple précision, `….positions`) est lu s'il n'y en a pas encore.
        let positions = read_places(&path.with_extension("positions64"))
            .or_else(|| {
                let old: HashMap<String, HashMap<String, [f32; 5]>> =
                    postcard::from_bytes(&std::fs::read(path.with_extension("positions")).ok()?)
                        .ok()?;
                Some(
                    old.into_iter()
                        .map(|(s, m)| (s, m.into_iter().map(|(p, v)| (p, v.map(f64::from))).collect()))
                        .collect(),
                )
            })
            .unwrap_or_default();
        let times = std::fs::read(path.with_extension("times"))
            .ok()
            .and_then(|b| postcard::from_bytes(&b).ok())
            .unwrap_or_default();
        // (v318) Pareil (`….spawns64`, sinon `….spawns`).
        let spawns = read_places(&path.with_extension("spawns64"))
            .or_else(|| {
                let old: HashMap<String, [f32; 5]> =
                    postcard::from_bytes(&std::fs::read(path.with_extension("spawns")).ok()?)
                        .ok()?;
                Some(old.into_iter().map(|(s, v)| (s, v.map(f64::from))).collect())
            })
            .unwrap_or_default();
        let weathers = std::fs::read(path.with_extension("weather"))
            .ok()
            .and_then(|b| postcard::from_bytes(&b).ok())
            .unwrap_or_default();
        let legends = std::fs::read(path.with_extension("foudre"))
            .ok()
            .and_then(|b| postcard::from_bytes(&b).ok())
            .unwrap_or_default();
        let seeds_open = std::fs::read(path.with_extension("graine"))
            .ok()
            .and_then(|b| postcard::from_bytes(&b).ok())
            .unwrap_or_default();
        let world_seeds = std::fs::read(path.with_extension("graines"))
            .ok()
            .and_then(|b| postcard::from_bytes(&b).ok())
            .unwrap_or_default();
        Self {
            path,
            servers,
            whitelists,
            defaults,
            positions,
            positions_dirty: false,
            positions_saved: std::time::Instant::now()
                .checked_sub(POSITIONS_EVERY)
                .unwrap_or_else(std::time::Instant::now),
            times,
            spawns,
            weathers,
            op_ready: HashMap::new(),
            legends,
            seeds_open,
            world_seeds,
        }
    }

    /// (v330) Graine et clé de terrain du monde `server` (par défaut : pas connues).
    pub fn world_seed(&self, server: &str) -> WorldSeed {
        self.world_seeds.get(server).copied().unwrap_or_default()
    }

    /// (v330) Ce que l'hôte `by` dit de la graine du monde en l'ouvrant ou en le réveillant :
    /// celle de son créateur fait foi ; un autre (qui garde le dernier état) ne la donne que
    /// si le relais ne la connaît pas. Rend ce que le relais en garde.
    pub fn note_world_seed(&mut self, server: &str, creator: &str, by: &str, given: WorldSeed) -> WorldSeed {
        let had = self.world_seed(server);
        let mut now = had;
        let trusted = by.eq_ignore_ascii_case(creator);
        if given.terrain != 0 && (trusted || had.terrain == 0) {
            now.terrain = given.terrain;
            if given.seed != 0 {
                now.seed = given.seed;
            }
        } else if given.seed != 0 && now.seed == 0 && given.terrain == now.terrain {
            now.seed = given.seed;
        }
        if now != had {
            self.world_seeds.insert(server.to_string(), now);
            self.save_world_seeds();
        }
        now
    }

    fn save_world_seeds(&self) {
        if let Ok(b) = postcard::to_allocvec(&self.world_seeds) {
            let _ = crate::accounts::write_atomic(&self.path.with_extension("graines"), &b);
        }
    }

    /// (v328) La graine du monde de ce serveur est-elle verrouillée (par défaut, oui) ?
    pub fn seed_locked(&self, server: &str) -> bool {
        !self.seeds_open.contains(server)
    }

    /// (v328) Ce que `who` peut faire de la graine (la voir, télécharger la carte) : tout si
    /// elle n'est pas verrouillée, ou s'il est OP (le créateur l'est toujours).
    pub fn seed_allowed(&self, server: &str, creator: &str, who: &str) -> bool {
        !self.seed_locked(server) || self.is_op(server, creator, who)
    }

    /// (v328) La verrouiller ou non ; gardé dans le fichier (`save` : relais du monde ; une
    /// province ne fait que suivre ce qu'il lui dit).
    fn set_seed_locked(&mut self, server: &str, locked: bool, save: bool) {
        if locked {
            self.seeds_open.remove(server);
        } else {
            self.seeds_open.insert(server.to_string());
        }
        if save {
            if let Ok(b) = postcard::to_allocvec(&self.seeds_open) {
                let _ = crate::accounts::write_atomic(&self.path.with_extension("graine"), &b);
            }
        }
    }

    /// (v244) La Foudre de Zeus du serveur (personne ne l'a : la valeur par défaut).
    pub fn legend(&self, server: &str) -> Legend {
        self.legends.get(server).cloned().unwrap_or_default()
    }

    /// (v244) Elle change : gardée dans le fichier.
    pub fn set_legend(&mut self, server: &str, legend: Legend) {
        self.legends.insert(server.to_string(), legend);
        if let Ok(b) = postcard::to_allocvec(&self.legends) {
            let _ = crate::accounts::write_atomic(&self.path.with_extension("foudre"), &b);
        }
    }

    /// (v238) Secondes (arrondies au-dessus) avant que l'OP nommé `who` puisse nommer un OP ;
    /// 0 : il le peut (le créateur toujours).
    pub fn op_wait_left(&self, server: &str, creator: &str, who: &str) -> u64 {
        if who.eq_ignore_ascii_case(creator) {
            return 0;
        }
        let now = std::time::Instant::now();
        self.op_ready
            .get(server)
            .and_then(|m| m.get(&who.to_ascii_lowercase()))
            .filter(|at| **at > now)
            .map_or(0, |at| (*at - now).as_secs_f32().ceil().max(1.0) as u64)
    }

    /// Heure du serveur maintenant, et si elle est arrêtée.
    pub fn time(&self, server: &str) -> (f32, bool) {
        let (h, since, fixed) =
            self.times
                .get(server)
                .copied()
                .unwrap_or((START_HOUR, unix_now(), false));
        if fixed || h < 0.0 {
            return (h, true);
        }
        let dt = unix_now().saturating_sub(since) as f32;
        ((h + dt * 24.0 / DAY_S).rem_euclid(24.0), false)
    }

    fn set_time(&mut self, server: &str, h: f32, fixed: bool) {
        self.times.insert(
            server.to_string(),
            (
                if h < 0.0 { -1.0 } else { h.rem_euclid(24.0) },
                unix_now(),
                fixed,
            ),
        );
        if let Ok(b) = postcard::to_allocvec(&self.times) {
            let _ = crate::accounts::write_atomic(&self.path.with_extension("times"), &b);
        }
    }

    /// (v229) Météo du serveur, comme dans `Response::SetWeather` : (temps, secondes qu'il
    /// lui reste, négatif : sans fin, figée) ; `WEATHER_NATURAL` si rien n'est imposé (ou
    /// plus : sa durée est passée).
    pub fn weather(&self, server: &str) -> (u8, f32, bool) {
        use minelib_net::proto::WEATHER_NATURAL;
        match self.weathers.get(server).copied() {
            Some((kind, u64::MAX, fixed)) => (kind, -1.0, fixed),
            Some((kind, until, _)) if until > unix_now() => {
                (kind, (until - unix_now()) as f32, false)
            }
            _ => (WEATHER_NATURAL, 0.0, false),
        }
    }

    /// (v229) `/weather` d'un OP (`reason` : voir `proto::SERVER_WEATHER`). Renvoie le
    /// message à lui afficher. `roll` : nombre de 0 à 1 dont la durée est tirée.
    fn set_weather(&mut self, server: &str, reason: &str, roll: f32) -> Result<String, String> {
        use minelib_net::proto::{self, WEATHER_CLEAR, WEATHER_RAIN, WEATHER_STORM};
        let kind_of = |w: &str| match w {
            "clear" => Some(WEATHER_CLEAR),
            "rain" => Some(WEATHER_RAIN),
            "storm" => Some(WEATHER_STORM),
            _ => None,
        };
        let label = |k: u8| match k {
            WEATHER_CLEAR => "beau temps",
            WEATHER_RAIN => "pluie",
            _ => "orage",
        };
        let bad = || "Usage : /weather clear|rain|storm, /weather fix [on|off]".to_string();
        let mut words = reason.split_whitespace();
        let fixed = self.weather(server).2;
        let text = match (words.next(), words.next()) {
            (Some("auto"), None) => {
                self.weathers.remove(server);
                "La météo change de nouveau toute seule, pour tout le serveur.".to_string()
            }
            (Some("fix"), Some(w)) => {
                let kind = kind_of(w).ok_or_else(bad)?;
                self.weathers
                    .insert(server.to_string(), (kind, u64::MAX, true));
                format!("Météo figée pour tout le serveur : {}.", label(kind))
            }
            (Some(w), None) => {
                let kind = kind_of(w).ok_or_else(bad)?;
                let (lo, hi) = proto::weather_span(kind).ok_or_else(bad)?;
                let until = if fixed {
                    u64::MAX
                } else {
                    unix_now() + (lo + (hi - lo) * roll.clamp(0.0, 1.0)) as u64
                };
                self.weathers
                    .insert(server.to_string(), (kind, until, fixed));
                format!(
                    "{} pour tout le serveur{}.",
                    match kind {
                        WEATHER_CLEAR => "Le ciel se dégage",
                        WEATHER_RAIN => "La pluie arrive",
                        _ => "L'orage arrive",
                    },
                    if fixed { " (météo figée)" } else { "" }
                )
            }
            _ => return Err(bad()),
        };
        // (Les météos passées ne restent pas dans le fichier.)
        let now = unix_now();
        self.weathers.retain(|_, w| w.1 > now);
        if let Ok(b) = postcard::to_allocvec(&self.weathers) {
            let _ = crate::accounts::write_atomic(&self.path.with_extension("weather"), &b);
        }
        Ok(text)
    }

    /// Dernière position connue de `pseudo` sur ce serveur.
    pub fn position(&self, server: &str, pseudo: &str) -> Option<[f64; 5]> {
        self.positions
            .get(server)?
            .get(&pseudo.to_ascii_lowercase())
            .copied()
    }

    /// Position actuelle de `pseudo` sur ce serveur (gardée en mémoire ; écrite par
    /// `save_positions`).
    pub fn set_position(&mut self, server: &str, pseudo: &str, p: [f64; 5]) {
        if p.iter().all(|v| v.is_finite()) {
            self.positions
                .entry(server.to_string())
                .or_default()
                .insert(pseudo.to_ascii_lowercase(), p);
            self.positions_dirty = true;
        }
    }

    /// Écrit les positions tout de suite si elles ont changé (le relais s'arrête).
    pub fn save_positions_now(&mut self) {
        if std::mem::take(&mut self.positions_dirty) {
            if let Ok(b) = postcard::to_allocvec(&self.positions) {
                let _ =
                    crate::accounts::write_atomic(&self.path.with_extension("positions64"), &b);
            }
        }
    }

    /// Écrit les positions si elles ont changé, au plus toutes les `POSITIONS_EVERY` (v140 :
    /// dans un monde de 100 000 joueurs, tout le fichier était réécrit toutes les 2 s).
    pub fn save_positions(&mut self) {
        if !self.positions_dirty || self.positions_saved.elapsed() < POSITIONS_EVERY {
            return;
        }
        self.positions_dirty = false;
        self.positions_saved = std::time::Instant::now();
        if let Ok(b) = postcard::to_allocvec(&self.positions) {
            let _ = crate::accounts::write_atomic(&self.path.with_extension("positions64"), &b);
        }
    }

    /// Mode de jeu par défaut du serveur (donné aux joueurs qui ne sont pas OP à leur
    /// arrivée).
    pub fn default_gamemode(&self, server: &str) -> Option<String> {
        self.defaults.get(server).cloned()
    }

    /// (v223) Point d'apparition du serveur (`/setspawn`), s'il en a un.
    pub fn spawn(&self, server: &str) -> Option<[f64; 5]> {
        self.spawns.get(server).copied()
    }

    fn wl_path(path: &std::path::Path) -> PathBuf {
        path.with_extension("whitelist")
    }

    /// (v311) Le monde `server` est supprimé par son créateur : ses OP, ses bannis, sa
    /// liste blanche, son mode par défaut, son point d'apparition et les positions de ses
    /// joueurs sont oubliés. (Son heure, sa météo et sa Foudre de Zeus le sont en mémoire ;
    /// leurs fichiers le seront à leur prochain enregistrement.)
    pub fn forget(&mut self, server: &str) {
        self.servers.remove(server);
        self.whitelists.remove(server);
        self.defaults.remove(server);
        self.spawns.remove(server);
        self.times.remove(server);
        self.weathers.remove(server);
        self.legends.remove(server);
        self.op_ready.remove(server);
        // (v330) Sa graine aussi, verrouillée ou non.
        if self.world_seeds.remove(server).is_some() {
            self.save_world_seeds();
        }
        if self.seeds_open.remove(server) {
            if let Ok(b) = postcard::to_allocvec(&self.seeds_open) {
                let _ = crate::accounts::write_atomic(&self.path.with_extension("graine"), &b);
            }
        }
        if self.positions.remove(server).is_some() {
            self.positions_dirty = true;
            self.save_positions_now();
        }
        self.save();
    }

    fn save(&self) {
        if let Ok(b) = postcard::to_allocvec(&self.servers) {
            let _ = crate::accounts::write_atomic(&self.path, &b);
        }
        if let Ok(b) = postcard::to_allocvec(&self.whitelists) {
            let _ = crate::accounts::write_atomic(&Self::wl_path(&self.path), &b);
        }
        if let Ok(b) = postcard::to_allocvec(&self.defaults) {
            let _ = crate::accounts::write_atomic(&self.path.with_extension("defaults"), &b);
        }
        if let Ok(b) = postcard::to_allocvec(&self.spawns) {
            let _ = crate::accounts::write_atomic(&self.path.with_extension("spawns64"), &b);
        }
    }

    /// (v251) « Quand un joueur ouvre sa carte sur un serveur de construction, le serveur doit
    /// être fermé par défaut (liste blanche active, invisible dans la liste des serveurs) ; dis-le
    /// à la première connexion : /whitelist off pour que tout le monde puisse rejoindre » : son
    /// créateur ouvre son monde et rien n'est encore réglé : la liste blanche est notée active
    /// (gardée : il la règle ensuite à sa guise). Rend vrai cette première fois.
    pub fn whitelist_first(&mut self, server: &str) -> bool {
        if self.whitelists.contains_key(server) {
            return false;
        }
        self.whitelists.insert(
            server.to_string(),
            Whitelist {
                on: true,
                names: Vec::new(),
            },
        );
        self.save();
        true
    }

    /// (v251) Un ami invité par le créateur ou un OP entre, même liste blanche active : il y
    /// est ajouté (rien si elle est inactive).
    pub fn whitelist_invited(&mut self, server: &str, creator: &str, by: &str, who: &str) {
        if !self.is_op(server, creator, by) {
            return;
        }
        let Some(w) = self.whitelists.get_mut(server).filter(|w| w.on) else {
            return;
        };
        let low = who.to_ascii_lowercase();
        if !w.names.contains(&low) {
            w.names.push(low);
            self.save();
        }
    }

    /// Liste blanche active sur ce serveur.
    pub fn whitelist_on(&self, server: &str) -> bool {
        self.whitelists.get(server).is_some_and(|w| w.on)
    }

    /// Liste blanche active et `who` n'y est pas (ni créateur, ni OP) : entrée refusée.
    pub fn whitelisted_out(&self, server: &str, creator: &str, who: &str) -> bool {
        self.whitelists.get(server).is_some_and(|w| {
            w.on && !self.is_op(server, creator, who)
                && !w.names.iter().any(|n| n.eq_ignore_ascii_case(who))
        })
    }

    pub fn is_op(&self, server: &str, creator: &str, who: &str) -> bool {
        who.eq_ignore_ascii_case(creator)
            || self
                .servers
                .get(server)
                .is_some_and(|a| a.ops.iter().any(|o| o.eq_ignore_ascii_case(who)))
    }

    pub fn banned(&self, server: &str, who: &str) -> Option<String> {
        self.servers.get(server).and_then(|a| {
            a.bans
                .iter()
                .find(|b| b.0.eq_ignore_ascii_case(who))
                .map(|b| b.1.clone())
        })
    }

    /// OP nommés de ce serveur (sans le créateur), en minuscules.
    pub fn ops(&self, server: &str) -> Vec<String> {
        self.servers
            .get(server)
            .map(|a| a.ops.clone())
            .unwrap_or_default()
    }

    /// Relais de province (v139) : réglages du serveur tels que les tient le relais du
    /// monde (OP, bannis, mode par défaut). Rien n'est écrit sur le disque ici.
    pub fn mirror(&mut self, server: &str, s: &minelib_net::proto::ProvinceSettings) {
        // (v328) La graine, comme le relais du monde la tient (rien d'écrit ici).
        self.set_seed_locked(server, !s.seed_open, false);
        let a = self.servers.entry(server.to_string()).or_default();
        a.ops = s.ops.iter().map(|o| o.to_ascii_lowercase()).collect();
        a.bans = s
            .bans
            .iter()
            .map(|b| (b.to_ascii_lowercase(), String::new()))
            .collect();
        match &s.default_mode {
            Some(m) => {
                self.defaults.insert(server.to_string(), m.clone());
            }
            None => {
                self.defaults.remove(server);
            }
        }
    }

    /// Pseudos bannis de ce serveur.
    pub fn bans(&self, server: &str) -> Vec<String> {
        self.servers
            .get(server)
            .map(|a| a.bans.iter().map(|b| b.0.clone()).collect())
            .unwrap_or_default()
    }

    /// `by` fait `action` sur `target` dans le serveur `server` de `creator`. Renvoie le
    /// message à lui afficher, ou pourquoi c'est refusé.
    pub fn act(
        &mut self,
        server: &str,
        creator: &str,
        by: &str,
        action: Action,
        target: &str,
        reason: &str,
    ) -> Result<String, String> {
        if !self.is_op(server, creator, by) {
            return Err("Réservé aux OP de ce serveur.".into());
        }
        let mode: String = reason.chars().take(16).collect();
        match action {
            Action::TimeSet => {
                let h: f32 = reason
                    .trim()
                    .parse()
                    .map_err(|_| "Heure illisible.".to_string())?;
                // −1 : le soleil de Dust2, immobile.
                if h == -1.0 {
                    self.set_time(server, h, true);
                    return Ok("Soleil de Dust2.".into());
                }
                if !(0.0..24.0).contains(&h) {
                    return Err("Heure entre 0 et 24.".into());
                }
                let fixed = self.time(server).1;
                self.set_time(server, h, fixed);
                return Ok("Heure changée.".into());
            }
            Action::TimeFix => {
                let fixed = matches!(reason.trim(), "1" | "true" | "oui" | "on");
                let (h, _) = self.time(server);
                self.set_time(server, h, fixed);
                return Ok(if fixed {
                    "Heure arrêtée.".into()
                } else {
                    "Le temps passe de nouveau.".into()
                });
            }
            Action::Rename => {
                return Ok(format!("Serveur renommé : {}.", reason.trim()));
            }
            Action::Weather => return self.set_weather(server, reason, rand::random::<f32>()),
            Action::Seed => {
                let locked = match reason.trim() {
                    "lock" => true,
                    "unlock" => false,
                    _ => return Err("Usage : /seed lock true|false.".into()),
                };
                self.set_seed_locked(server, locked, true);
                return Ok(if locked {
                    "Graine du monde verrouillée : seuls les OP la voient (/seed) et peuvent \
                     télécharger la carte."
                        .into()
                } else {
                    "Graine du monde déverrouillée : tous les joueurs du serveur peuvent la voir \
                     (/seed) et télécharger la carte (menu Échap)."
                        .into()
                });
            }
            Action::Teleport => {
                return Ok(if target.is_empty() || target.eq_ignore_ascii_case(by) {
                    "Téléporté.".into()
                } else {
                    format!("{target} est téléporté.")
                });
            }
            Action::SetSpawn => {
                // (v318) En double précision, ramené sur la carte bouclée.
                let v: Vec<f64> = reason
                    .split_whitespace()
                    .filter_map(|t| t.parse().ok())
                    .collect();
                let [x, y, z, yaw, pitch] = v[..] else {
                    return Err("Position illisible.".into());
                };
                if !(x.is_finite() && z.is_finite()) {
                    return Err("Position illisible.".into());
                }
                let (x, z) = (
                    minelib_net::proto::wrap_place(x),
                    minelib_net::proto::wrap_place(z),
                );
                let at = [x, y, z, yaw, pitch];
                if !at.iter().all(|c| c.is_finite()) || y.abs() > 4096.0 {
                    return Err("Position illisible.".into());
                }
                self.spawns.insert(server.to_string(), at);
                self.save();
                return Ok(format!(
                    "Point d'apparition du serveur : {:.0} {:.0} {:.0} (pour qui arrive la première fois).",
                    x, y, z
                ));
            }
            Action::Gamemode => {
                return Ok(if target.is_empty() || target.eq_ignore_ascii_case(by) {
                    format!("Mode de jeu : {mode}.")
                } else {
                    format!("{target} passe en mode {mode}.")
                })
            }
            Action::DefaultGamemode => {
                let msg = if mode.is_empty() {
                    self.defaults.remove(server);
                    "Plus de mode de jeu par défaut : chacun garde le sien.".to_string()
                } else {
                    self.defaults.insert(server.to_string(), mode.clone());
                    format!("Mode de jeu par défaut : {mode} (pour qui n'est pas OP, à l'arrivée).")
                };
                self.save();
                return Ok(msg);
            }
            _ => {}
        }
        let low = target.to_ascii_lowercase();
        let is_creator = target.eq_ignore_ascii_case(creator);
        let wl = self.whitelists.entry(server.to_string()).or_default();
        let msg = match action {
            Action::WhitelistList => {
                return Ok(format!(
                    "Liste blanche {} : {}.",
                    if wl.on { "active" } else { "inactive" },
                    if wl.names.is_empty() {
                        "personne".into()
                    } else {
                        wl.names.join(", ")
                    }
                ));
            }
            Action::WhitelistOn => {
                wl.on = true;
                Some(
                    "Liste blanche active : seuls les OP et les pseudos de la liste entrent."
                        .to_string(),
                )
            }
            Action::WhitelistOff => {
                wl.on = false;
                Some("Liste blanche désactivée : tout le monde peut entrer.".to_string())
            }
            Action::WhitelistAdd | Action::WhitelistRemove if target.is_empty() => {
                return Err("Pseudo manquant.".into())
            }
            Action::WhitelistAdd => {
                if !wl.names.contains(&low) {
                    wl.names.push(low.clone());
                }
                Some(format!("{target} est sur la liste blanche."))
            }
            Action::WhitelistRemove => {
                let before = wl.names.len();
                wl.names.retain(|n| *n != low);
                if wl.names.len() == before {
                    return Err(format!("{target} n'est pas sur la liste blanche."));
                }
                Some(format!("{target} n'est plus sur la liste blanche."))
            }
            _ => None,
        };
        if let Some(msg) = msg {
            self.save();
            return Ok(msg);
        }
        let a = self.servers.entry(server.to_string()).or_default();
        let msg = match action {
            Action::List => {
                let mut ops = vec![format!("{creator} (créateur)")];
                ops.extend(a.ops.iter().cloned());
                let bans: Vec<String> = a.bans.iter().map(|b| b.0.clone()).collect();
                let mut text = format!(
                    "OP : {}. Bannis : {}.",
                    ops.join(", "),
                    if bans.is_empty() {
                        "aucun".into()
                    } else {
                        bans.join(", ")
                    }
                );
                // (v238) Un OP nommé qui attend encore : le temps qui lui reste.
                let left = self.op_wait_left(server, creator, by);
                if left > 0 {
                    text += &format!(" Tu pourras nommer un OP dans {left} s.");
                }
                return Ok(text);
            }
            _ if target.is_empty() => return Err("Pseudo manquant.".into()),
            Action::Kill => return Ok(format!("{target} est tué.")),
            Action::WhitelistOn
            | Action::WhitelistOff
            | Action::WhitelistAdd
            | Action::WhitelistRemove
            | Action::WhitelistList
            | Action::Gamemode
            | Action::Rename
            | Action::TimeSet
            | Action::TimeFix
            | Action::Weather
            | Action::Teleport
            | Action::SetSpawn
            | Action::Seed
            | Action::DefaultGamemode => unreachable!("traité plus haut"),
            Action::Ban if is_creator => return Err("On ne bannit pas le créateur.".into()),
            Action::Ban if by.eq_ignore_ascii_case(target) => {
                return Err("On ne se bannit pas soi-même.".into())
            }
            Action::Ban => {
                // Un OP banni perd l'OP.
                a.ops.retain(|o| *o != low);
                a.bans.retain(|b| b.0 != low);
                a.bans.push((low, reason.chars().take(120).collect()));
                format!("{target} est banni de ce serveur.")
            }
            Action::Unban => {
                let before = a.bans.len();
                a.bans.retain(|b| b.0 != low);
                if a.bans.len() == before {
                    return Err(format!("{target} n'est pas banni."));
                }
                format!("{target} n'est plus banni.")
            }
            Action::Deop if !by.eq_ignore_ascii_case(creator) => {
                return Err("Seul le créateur du serveur retire l'OP.".into())
            }
            Action::Op if is_creator => return Err("Le créateur est déjà OP.".into()),
            Action::Op if a.ops.contains(&low) => return Err(format!("{target} est déjà OP.")),
            Action::Op => {
                // (v238) Un OP nommé en nomme un au plus toutes les `OP_WAIT_S` secondes, et
                // pas avant `OP_WAIT_S` secondes après l'être devenu ; le créateur, quand il veut.
                let now = std::time::Instant::now();
                let ready = self.op_ready.entry(server.to_string()).or_default();
                ready.retain(|_, at| *at > now);
                let by_creator = by.eq_ignore_ascii_case(creator);
                if !by_creator {
                    let me = by.to_ascii_lowercase();
                    if let Some(&at) = ready.get(&me) {
                        let left = (at - now).as_secs_f32().ceil().max(1.0) as u64;
                        return Err(format!(
                            "Tu pourras nommer un OP dans {left} s (un OP nommé en nomme un \
                             toutes les {OP_WAIT_S} s au plus, et {OP_WAIT_S} s après l'être \
                             devenu)."
                        ));
                    }
                    ready.insert(me, now + OP_WAIT);
                }
                // Le nouvel OP attend avant d'en nommer un à son tour.
                ready.insert(low.clone(), now + OP_WAIT);
                a.ops.push(low);
                a.bans.retain(|b| !b.0.eq_ignore_ascii_case(target));
                if by_creator {
                    format!(
                        "{target} est OP de ce serveur ({target} pourra nommer un OP dans \
                         {OP_WAIT_S} s)."
                    )
                } else {
                    format!(
                        "{target} est OP de ce serveur. Tu pourras nommer un autre OP dans \
                         {OP_WAIT_S} s."
                    )
                }
            }
            Action::Deop if is_creator => {
                return Err("Le créateur reste OP.".into());
            }
            Action::Deop => {
                a.ops.retain(|o| *o != low);
                format!("{target} n'est plus OP.")
            }
        };
        self.save();
        Ok(msg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whitelist_lets_in_only_ops_and_listed_players() {
        let path = std::env::temp_dir().join(format!("minelib-wl-{}", rand::random::<u32>()));
        let mut s = Store::open(path.clone());
        let k = key("Createur", "monde");
        let act = |s: &mut Store, by: &str, a: Action, t: &str| s.act(&k, "Createur", by, a, t, "");
        assert!(
            !s.whitelisted_out(&k, "Createur", "Eve"),
            "inactive : ouvert"
        );
        assert!(
            act(&mut s, "Eve", Action::WhitelistOn, "").is_err(),
            "pas OP"
        );
        act(&mut s, "Createur", Action::Op, "Bob").unwrap();
        act(&mut s, "Bob", Action::WhitelistOn, "").unwrap();
        assert!(s.whitelisted_out(&k, "Createur", "Eve"));
        assert!(!s.whitelisted_out(&k, "Createur", "bob"), "OP");
        assert!(!s.whitelisted_out(&k, "Createur", "createur"), "créateur");
        act(&mut s, "Bob", Action::WhitelistAdd, "Eve").unwrap();
        assert!(!s.whitelisted_out(&k, "Createur", "EVE"));
        assert!(act(&mut s, "Bob", Action::WhitelistList, "")
            .unwrap()
            .contains("eve"));
        // Gardé sur disque, et l'ancien fichier des OP reste lisible.
        let mut s2 = Store::open(path.clone());
        assert!(!s2.whitelisted_out(&k, "Createur", "eve"));
        assert!(s2.whitelisted_out(&k, "Createur", "Zoe"));
        assert!(s2.is_op(&k, "Createur", "bob"));
        act(&mut s2, "Bob", Action::WhitelistRemove, "eve").unwrap();
        assert!(s2.whitelisted_out(&k, "Createur", "eve"));
        act(&mut s2, "Bob", Action::WhitelistOff, "").unwrap();
        assert!(!s2.whitelisted_out(&k, "Createur", "eve"));
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("whitelist"));
    }

    /// (v328) La graine du monde : verrouillée par défaut (seuls les OP et le créateur la
    /// voient), déverrouillée ou verrouillée par un OP seulement, gardée sur le disque ; une
    /// province suit ce que lui dit le relais du monde (sans rien écrire).
    #[test]
    fn the_seed_is_locked_by_default_and_ops_unlock_it() {
        let path = std::env::temp_dir().join(format!("minelib-graine-{}", rand::random::<u32>()));
        let mut s = Store::open(path.clone());
        let k = key("Createur", "monde");
        let other = key("Createur", "autre");
        assert!(s.seed_locked(&k), "verrouillée par défaut");
        assert!(!s.seed_allowed(&k, "Createur", "Eve"));
        assert!(s.seed_allowed(&k, "Createur", "createur"), "le créateur la voit");
        assert!(s
            .act(&k, "Createur", "Eve", Action::Seed, "", "unlock")
            .is_err());
        assert!(s.seed_locked(&k), "pas OP : rien ne change");
        s.act(&k, "Createur", "Createur", Action::Op, "Bob", "")
            .unwrap();
        assert!(s.seed_allowed(&k, "Createur", "BOB"), "un OP la voit");
        assert!(s
            .act(&k, "Createur", "Bob", Action::Seed, "", "ouvrir")
            .is_err());
        s.act(&k, "Createur", "Bob", Action::Seed, "", "unlock")
            .unwrap();
        assert!(!s.seed_locked(&k) && s.seed_allowed(&k, "Createur", "Eve"));
        assert!(s.seed_locked(&other), "chaque serveur la sienne");
        // Gardée sur le disque.
        let mut again = Store::open(path.clone());
        assert!(!again.seed_locked(&k) && again.seed_locked(&other));
        again
            .act(&k, "Createur", "Createur", Action::Seed, "", "lock")
            .unwrap();
        assert!(Store::open(path.clone()).seed_locked(&k));
        // Une province suit le relais du monde.
        let ppath = std::env::temp_dir().join(format!("minelib-graine-p-{}", rand::random::<u32>()));
        let mut province = Store::open(ppath.clone());
        let mut set = minelib_net::proto::ProvinceSettings {
            seed_open: true,
            ..Default::default()
        };
        province.mirror(&k, &set);
        assert!(!province.seed_locked(&k));
        set.seed_open = false;
        province.mirror(&k, &set);
        assert!(province.seed_locked(&k));
        assert!(!ppath.with_extension("graine").exists(), "rien d'écrit chez la province");
        let _ = std::fs::remove_file(&path);
        for ext in ["graine", "whitelist"] {
            let _ = std::fs::remove_file(path.with_extension(ext));
        }
    }

    /// (v330) Graine et clé de terrain d'un monde : celles du créateur font foi ; celui qui
    /// garde le dernier état (sans elles) ne les efface pas, ni n'en impose d'autres ; gardées
    /// sur le disque ; oubliées avec le monde.
    #[test]
    fn the_relay_keeps_the_seed_its_creator_gives() {
        let path = std::env::temp_dir().join(format!("minelib-graines-{}", rand::random::<u32>()));
        let mut s = Store::open(path.clone());
        let k = key("Createur", "monde");
        let ws = |seed, terrain| WorldSeed { seed, terrain };
        assert_eq!(s.world_seed(&k), ws(0, 0));
        // Un gardien sans rien, avant tout : rien.
        assert_eq!(s.note_world_seed(&k, "Createur", "Gardien", ws(0, 0)), ws(0, 0));
        assert_eq!(s.note_world_seed(&k, "Createur", "createur", ws(77, 99)), ws(77, 99));
        // Le gardien (copie sans graine) : ce que garde le relais.
        assert_eq!(s.note_world_seed(&k, "Createur", "Gardien", ws(0, 0)), ws(77, 99));
        // Un autre n'en impose pas d'autres.
        assert_eq!(s.note_world_seed(&k, "Createur", "Gardien", ws(5, 6)), ws(77, 99));
        // Gardées sur le disque.
        let mut again = Store::open(path.clone());
        assert_eq!(again.world_seed(&k), ws(77, 99));
        // Le créateur fait foi.
        assert_eq!(again.note_world_seed(&k, "Createur", "Createur", ws(8, 9)), ws(8, 9));
        again.forget(&k);
        assert_eq!(Store::open(path.clone()).world_seed(&k), ws(0, 0));
        for ext in ["graines", "graine", "whitelist"] {
            let _ = std::fs::remove_file(path.with_extension(ext));
        }
        let _ = std::fs::remove_file(&path);
    }

    /// (v229) La météo imposée : par un OP seulement, pour une durée ordinaire tirée par le
    /// relais ; figée, elle n'a pas de fin ; gardée sur le disque ; passée, oubliée.
    #[test]
    fn the_weather_of_a_server_is_set_by_ops_kept_and_ends() {
        use minelib_net::proto::{WEATHER_CLEAR, WEATHER_NATURAL, WEATHER_RAIN, WEATHER_STORM};
        let path = std::env::temp_dir().join(format!("minelib-meteo-{}", rand::random::<u32>()));
        let mut s = Store::open(path.clone());
        let k = key("Createur", "monde");
        assert_eq!(s.weather(&k).0, WEATHER_NATURAL);
        assert!(s
            .act(&k, "Createur", "Eve", Action::Weather, "", "storm")
            .is_err());
        assert_eq!(s.weather(&k).0, WEATHER_NATURAL, "pas OP : rien ne change");
        // Durées ordinaires : aux deux bouts de ce que tire le relais.
        for (word, kind, roll, want) in [
            ("storm", WEATHER_STORM, 0.0, 180.0),
            ("storm", WEATHER_STORM, 1.0, 780.0),
            ("rain", WEATHER_RAIN, 0.5, 900.0),
            ("clear", WEATHER_CLEAR, 1.0, 9000.0),
        ] {
            s.set_weather(&k, word, roll).unwrap();
            let (got, left, fixed) = s.weather(&k);
            assert_eq!((got, fixed), (kind, false));
            assert!((left - want).abs() <= 2.0, "{word} {left}");
        }
        assert!(s.set_weather(&k, "neige", 0.5).is_err());
        assert!(s.set_weather(&k, "fix", 0.5).is_err());
        assert!(s.set_weather(&k, "fix neige", 0.5).is_err());
        assert_eq!(s.weather(&k).0, WEATHER_CLEAR, "refusé : la météo reste");
        // Figée : sans fin, et le temps qu'on impose ensuite le reste.
        s.act(&k, "Createur", "Createur", Action::Weather, "", "fix rain")
            .unwrap();
        assert_eq!(s.weather(&k), (WEATHER_RAIN, -1.0, true));
        s.set_weather(&k, "storm", 0.5).unwrap();
        assert_eq!(s.weather(&k), (WEATHER_STORM, -1.0, true));
        // Gardée sur le disque.
        let mut again = Store::open(path.clone());
        assert_eq!(again.weather(&k), (WEATHER_STORM, -1.0, true));
        // De nouveau toute seule.
        again.set_weather(&k, "auto", 0.5).unwrap();
        assert_eq!(again.weather(&k).0, WEATHER_NATURAL);
        assert_eq!(Store::open(path.clone()).weather(&k).0, WEATHER_NATURAL);
        // Passée : oubliée.
        again.weathers.insert(k.clone(), (WEATHER_RAIN, unix_now() - 1, false));
        assert_eq!(again.weather(&k).0, WEATHER_NATURAL);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("weather"));
    }

    /// (v238) « Un OP classique peut OP une autre personne toutes les 30 secondes max, et on
    /// doit attendre 30 s pour pouvoir OP quand on le devient ; indique le temps restant. »
    #[test]
    fn a_named_op_names_an_op_every_30_seconds() {
        let path = std::env::temp_dir().join(format!("minelib-opwait-{}", rand::random::<u32>()));
        let mut s = Store::open(path.clone());
        let k = key("Createur", "monde");
        let act = |s: &mut Store, by: &str, a: Action, t: &str| s.act(&k, "Createur", by, a, t, "");
        // Le temps passe (`secs` secondes) pour les attentes.
        let later = |s: &mut Store, secs: u64| {
            for at in s.op_ready.values_mut().flat_map(|m| m.values_mut()) {
                *at = at.checked_sub(std::time::Duration::from_secs(secs)).unwrap();
            }
        };
        act(&mut s, "Createur", Action::Op, "Bob").unwrap();
        // Tout juste OP : il attend 30 s, et le refus dit combien il en reste.
        let e = act(&mut s, "Bob", Action::Op, "Eve").unwrap_err();
        assert!(e.contains("dans 30 s"), "{e}");
        assert!(!s.is_op(&k, "Createur", "eve"));
        assert_eq!(s.op_wait_left(&k, "Createur", "bob"), 30);
        let list = act(&mut s, "Bob", Action::List, "").unwrap();
        assert!(list.starts_with("OP :") && list.contains("dans 30 s"), "{list}");
        later(&mut s, 12);
        let e = act(&mut s, "Bob", Action::Op, "Eve").unwrap_err();
        assert!(e.contains("dans 18 s"), "{e}");
        later(&mut s, 18);
        // 30 s après : il nomme Eve, puis attend de nouveau 30 s.
        let ok = act(&mut s, "Bob", Action::Op, "Eve").unwrap();
        assert!(ok.contains("dans 30 s"), "{ok}");
        assert!(s.is_op(&k, "Createur", "eve"));
        let e = act(&mut s, "Bob", Action::Op, "Zoe").unwrap_err();
        assert!(e.contains("dans 30 s"), "{e}");
        // Eve, tout juste OP, attend aussi ; nommer quelqu'un qui l'est déjà ne compte pas.
        let e = act(&mut s, "Eve", Action::Op, "Zoe").unwrap_err();
        assert!(e.contains("dans 30 s"), "{e}");
        let e = act(&mut s, "Createur", Action::Op, "EVE").unwrap_err();
        assert!(e.contains("déjà OP"), "{e}");
        // Le créateur en nomme quand il veut, plusieurs de suite.
        act(&mut s, "Createur", Action::Op, "Zoe").unwrap();
        act(&mut s, "Createur", Action::Op, "Max").unwrap();
        assert_eq!(s.op_wait_left(&k, "Createur", "createur"), 0);
        assert!(!act(&mut s, "Createur", Action::List, "")
            .unwrap()
            .contains("Tu pourras"));
        // Seul le créateur retire l'OP.
        let e = act(&mut s, "Bob", Action::Deop, "Eve").unwrap_err();
        assert!(e.contains("créateur"), "{e}");
        later(&mut s, 30);
        act(&mut s, "Bob", Action::Op, "Lea").unwrap();
        act(&mut s, "Eve", Action::Op, "Noe").unwrap();
        act(&mut s, "Createur", Action::Deop, "Eve").unwrap();
        assert!(!s.is_op(&k, "Createur", "eve"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn the_creator_is_op_and_names_the_others() {
        let path = std::env::temp_dir().join(format!("minelib-ops-{}", rand::random::<u32>()));
        let mut s = Store::open(path.clone());
        let k = key("Createur", "monde");
        let act = |s: &mut Store, by: &str, a: Action, t: &str| s.act(&k, "Createur", by, a, t, "");
        assert!(
            act(&mut s, "Bob", Action::Ban, "Eve").is_err(),
            "Bob n'est pas OP"
        );
        assert!(act(&mut s, "createur", Action::Op, "Bob").is_ok());
        // (v238) Tout juste OP : il attend 30 s avant d'en nommer un.
        let e = act(&mut s, "Bob", Action::Op, "Eve").unwrap_err();
        assert!(e.contains("Tu pourras nommer un OP dans"), "{e}");
        assert!(act(&mut s, "Bob", Action::Ban, "Eve").is_ok());
        assert!(s.banned(&k, "EVE").is_some());
        assert!(act(&mut s, "Bob", Action::Ban, "Createur").is_err());
        assert!(act(&mut s, "Createur", Action::Deop, "Createur").is_err());
        assert!(act(&mut s, "Bob", Action::Unban, "eve").is_ok());
        assert!(s.banned(&k, "Eve").is_none());
        act(&mut s, "Bob", Action::Ban, "Eve").unwrap();
        // Gardé sur disque.
        let s2 = Store::open(path.clone());
        assert!(s2.banned(&k, "eve").is_some());
        assert!(s2.is_op(&k, "Createur", "BOB"));
        assert!(s2.banned(&key("Createur", "autre"), "eve").is_none());
        let _ = std::fs::remove_file(path);
    }
}
