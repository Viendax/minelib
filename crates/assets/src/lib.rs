//! Fichiers du jeu sur le disque : dossier `.minelib`, packs de ressources, options.
//!
//! - Windows : `%APPDATA%\.minelib` (dossier « Roaming » de l'utilisateur)
//! - Linux / macOS : `~/.minelib`
//!
//! ```text
//! .minelib/
//!   options.txt
//!   resourcepacks/   packs de textures (dossiers ou .zip)
//!   saves/           un dossier par monde
//! ```

use std::collections::HashMap;
use std::fs;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Context, Result};

/// Pack fourni avec le jeu, installé au premier lancement.
pub const DEFAULT_PACK: &str = "VoxelArt_Libre";
const DEFAULT_PACK_ZIP: &[u8] = include_bytes!("../../../assets/VoxelArt_Libre.zip");

#[derive(Clone, Debug)]
pub struct Paths {
    pub root: PathBuf,
    pub resourcepacks: PathBuf,
    pub saves: PathBuf,
    pub options: PathBuf,
}

/// Dossier racine des données du jeu (`MINELIB_HOME` permet de le changer).
pub fn data_dir() -> PathBuf {
    if let Some(p) = std::env::var_os("MINELIB_HOME") {
        return PathBuf::from(p);
    }
    #[cfg(windows)]
    {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            return PathBuf::from(appdata).join(".minelib");
        }
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .unwrap_or_else(|| ".".into());
    PathBuf::from(home).join(".minelib")
}

/// Crée l'arborescence si besoin et installe le pack par défaut.
pub fn init() -> Result<Paths> {
    let root = data_dir();
    let paths = Paths {
        resourcepacks: root.join("resourcepacks"),
        saves: root.join("saves"),
        options: root.join("options.txt"),
        root,
    };
    fs::create_dir_all(&paths.resourcepacks)
        .with_context(|| format!("création de {}", paths.resourcepacks.display()))?;
    fs::create_dir_all(&paths.saves)?;
    let default_zip = paths.resourcepacks.join(format!("{DEFAULT_PACK}.zip"));
    let default_dir = paths.resourcepacks.join(DEFAULT_PACK);
    if !default_zip.exists() && !default_dir.exists() {
        fs::write(&default_zip, DEFAULT_PACK_ZIP)?;
        log::info!("pack par défaut installé : {}", default_zip.display());
    } else if default_zip.exists()
        && fs::read(&default_zip).map_or(true, |old| old != DEFAULT_PACK_ZIP)
    {
        // Nouvelle version du jeu : le pack par défaut installé est remplacé par le sien.
        fs::write(&default_zip, DEFAULT_PACK_ZIP)?;
        log::info!("pack par défaut mis à jour : {}", default_zip.display());
    }
    log::info!("dossier du jeu : {}", paths.root.display());
    Ok(paths)
}

// ---------------------------------------------------------------- options

/// Options persistées (`clé=valeur` par ligne).
#[derive(Clone, Debug)]
pub struct Options {
    pub resource_pack: String,
    /// Distance de rendu, en chunks.
    pub render_distance: i32,
    /// NVIDIA Reflex / AMD Anti-Lag 2 (si la carte le propose) : le pilote fait attendre
    /// le jeu à chaque image. Enregistré sous « reflex » depuis la v129 : l'ancien réglage
    /// (« low_latency ») voulait dire « essayé par le jeu quand il le juge utile », ce qui
    /// coûtait des images même après ; chacun repart de « désactivé ».
    pub low_latency: bool,
    /// Sensibilité de la souris, valeur Counter-Strike (0,022° par unité de souris × valeur).
    pub sensitivity: f32,
    /// Sensibilité en mode FPS (réglée à part de la construction).
    pub sensitivity_fps: f32,
    /// Reflets en raytracing (miroir, métal, eau…).
    pub raytracing: bool,
    /// (v201) Torches en lumières ponctuelles : autour de soi, chaque torche (et chaque bloc
    /// lumineux) éclaire comme une lumière ponctuelle de l'éditeur, au lieu de la lumière
    /// des blocs. Non par défaut.
    pub point_torches: bool,
    /// (v228) Dessiner avec la partie graphique intégrée au processeur plutôt qu'avec la carte
    /// la plus puissante (moins de chaleur et de batterie ; utile aussi quand l'écran est
    /// branché dessus). Non par défaut ; compte au prochain lancement.
    pub igpu: bool,
    /// (v231) Invisible pour ses amis (hors ligne pour eux, pas d'invitations). Non par
    /// défaut.
    pub invisible: bool,
    /// Échelle de rendu du monde (0,5 … 1) : en dessous de 1, le monde est calculé en plus
    /// petit puis reconstruit en pleine résolution par l'anticrénelage (plus de FPS).
    pub render_scale: f32,
    /// Limite d'images par seconde (0 = illimité) : moins de chauffe, FPS stables.
    pub fps_max: u32,
    /// Répartition du processeur : uniforme (tous les cœurs pour les tâches de fond, à basse
    /// priorité) ou réservée (cœurs dédiés au jeu et au pilote).
    pub cpu_uniform: bool,
    /// Plein écran (sans bordure) : Windows affiche l'image directement, sans passer par le
    /// compositeur du bureau (plus de FPS, moins de latence).
    pub fullscreen: bool,
    /// Volume général des sons (0..1).
    pub volume: f32,
    /// Sortie son choisie (nom du périphérique ; vide : celle par défaut du système).
    pub audio_out: String,
    /// Micro choisi (nom du périphérique ; vide : celui par défaut du système).
    pub audio_in: String,
    /// Volume du micro (gain appliqué à ce qu'on envoie : 0..2, 1 = tel quel).
    pub volume_mic: f32,
    /// Volume du son des écrans vidéo (0..1). (v213) 50 % par défaut, enregistré sous
    /// `volume_tele` ; l'ancienne clé `volume_screens` est relue une fois (son ancien
    /// défaut, 100 %, devient 50 %).
    pub volume_screens: f32,
    /// (v179) Menu du jeu ouvert : en solo, les vidéos des écrans se mettent en pause ; en
    /// ligne, leur son est coupé tant que le menu est ouvert. Non par défaut : elles
    /// continuent comme si de rien n'était.
    pub mute_screens_menu: bool,
    /// Volume du tchat vocal entrant (0..2, 1 = tel quel).
    pub volume_voice: f32,
    /// (v234) Volume de la météo : pluie et tonnerre (0..1, × le volume du jeu).
    pub volume_weather: f32,
    /// Adresse du relais (comptes, liste des parties).
    pub relay: String,
    /// Au menu, ce jeu peut servir de garant indépendant pour les parties des autres.
    pub garant: bool,
    /// Quand ce jeu héberge le relais d'une région, d'une province ou un relais de secours,
    /// les joueurs servis le joignent-ils en direct (ils voient son adresse IP) ? (v280)
    /// `hote_direct=true` dans `options.txt`. (v301) Sans la ligne, `auto` ou `false` :
    /// non, l'hôte reste anonyme, joint par des passeurs (`minelib_p2p::passeur`) et, s'il
    /// n'y en a pas, par les serveurs relais publics d'iroh. (La v297 le mettait en direct
    /// d'elle-même quand sa box était ouverte.) (v313) Le réglage vaut aussi pour le nœud du
    /// jeu lui-même (`minelib_p2p::Node::start_game`) : sans `true`, il n'ouvre aucun lien
    /// direct, ni comme joueur ni comme relais, et ne montre donc son adresse IP à personne.
    pub host_direct: Option<bool>,
    /// (v298) Invitation reçue d'un ami : la ligne de son fichier `invitation.txt` (nœud et
    /// clé de son relais). Ce relais est joint directement, sans la liste du DHT.
    pub invitation: String,
    /// (v307) Le joueur partage-t-il son journal réseau (lignes sans pseudo ni adresse)
    /// avec le relais auquel il est connecté ? Rien : pas encore demandé (la question est
    /// posée une fois, au menu d'accueil). `journal_reseau=true` ou `false`.
    pub share_netlog: Option<bool>,
    /// (v307) Quand ce PC est le relais : recevoir les journaux réseau des joueurs qui les
    /// partagent, dans `journaux_reseau.txt` (paramètres avancés). `recevoir_journaux=true`.
    pub receive_netlogs: bool,
    /// (v308) Ce jeu sert-il de passeur (il porte des paquets chiffrés d'autres joueurs, et
    /// montre son adresse IP à ceux à qui il les remet) ? Rien : pas encore demandé (la
    /// question est posée une fois, au menu d'accueil). `passeur=true` ou `false`.
    pub passeur: Option<bool>,
    /// Clé du relais retenue au premier contact (hexadécimal, vide = pas encore).
    pub relay_key: String,
    /// Dernier pseudo utilisé (prérempli à la connexion).
    pub pseudo: String,
    /// Couleur préférée en FPS (0 à 4 : vert, jaune, orange, bleu, violet ; 255 : aucune).
    pub fps_color: u8,
    /// Langue de l'interface (code : « fr », « en »…).
    pub language: String,
}

/// Sensibilité par défaut de Counter-Strike 2.
pub const DEFAULT_SENSITIVITY: f32 = 1.25;
pub const MIN_SENSITIVITY: f32 = 0.01;
pub const MAX_SENSITIVITY: f32 = 20.0;

impl Default for Options {
    fn default() -> Self {
        Self {
            resource_pack: DEFAULT_PACK.into(),
            render_distance: 10,
            low_latency: false,
            sensitivity: DEFAULT_SENSITIVITY,
            sensitivity_fps: DEFAULT_SENSITIVITY,
            fps_color: 255,
            language: "fr".into(),
            raytracing: true,
            point_torches: false,
            igpu: false,
            invisible: false,
            render_scale: 1.0,
            fps_max: 0,
            cpu_uniform: true,
            fullscreen: false,
            volume: 0.6,
            audio_out: String::new(),
            audio_in: String::new(),
            volume_mic: 1.0,
            volume_screens: 0.5,
            mute_screens_menu: false,
            volume_voice: 1.0,
            volume_weather: 1.0,
            relay: "auto".into(),
            garant: true,
            host_direct: None,
            invitation: String::new(),
            share_netlog: None,
            receive_netlogs: false,
            passeur: None,
            relay_key: String::new(),
            pseudo: String::new(),
        }
    }
}

impl Options {
    pub fn load(path: &Path) -> Self {
        let mut o = Self::default();
        if let Ok(text) = fs::read_to_string(path) {
            let kv = parse_kv(&text);
            if let Some(p) = kv.get("resource_pack") {
                o.resource_pack = p.clone();
            }
            if let Some(v) = kv
                .get("render_distance")
                .and_then(|v| v.parse::<i32>().ok())
            {
                o.render_distance = v.clamp(2, 64);
            }
            if let Some(v) = kv.get("reflex") {
                o.low_latency = v == "true";
            }
            if let Some(v) = kv.get("raytracing") {
                o.raytracing = v != "false";
            }
            if let Some(v) = kv.get("torches_ponctuelles") {
                o.point_torches = v == "true";
            }
            if let Some(v) = kv.get("carte_integree") {
                o.igpu = v == "true";
            }
            if let Some(v) = kv.get("invisible") {
                o.invisible = v == "true";
            }
            if let Some(v) = kv.get("fullscreen") {
                o.fullscreen = v == "true";
            }
            if let Some(v) = kv.get("cpu_sharing") {
                o.cpu_uniform = v != "reserved";
            }
            if let Some(v) = kv.get("fps_max").and_then(|v| v.parse::<u32>().ok()) {
                o.fps_max = if v == 0 { 0 } else { v.clamp(30, 1000) };
            }
            if let Some(v) = kv.get("render_scale").and_then(|v| v.parse::<f32>().ok()) {
                o.render_scale = v.clamp(0.5, 1.0);
            }
            if let Some(v) = kv.get("relay").filter(|v| !v.trim().is_empty()) {
                o.relay = v.trim().to_string();
            }
            if let Some(v) = kv.get("garant") {
                o.garant = v != "false";
            }
            if let Some(v) = kv.get("hote_direct") {
                o.host_direct = match v.as_str() {
                    "true" => Some(true),
                    "false" => Some(false),
                    _ => None,
                };
            }
            if let Some(v) = kv.get("invitation") {
                o.invitation = v.trim().to_string();
            }
            if let Some(v) = kv.get("journal_reseau") {
                o.share_netlog = match v.as_str() {
                    "true" => Some(true),
                    "false" => Some(false),
                    _ => None,
                };
            }
            if let Some(v) = kv.get("recevoir_journaux") {
                o.receive_netlogs = v == "true";
            }
            if let Some(v) = kv.get("passeur") {
                o.passeur = match v.as_str() {
                    "true" => Some(true),
                    "false" => Some(false),
                    _ => None,
                };
            }
            if let Some(v) = kv.get("relay_key") {
                o.relay_key = v.trim().to_string();
            }
            if let Some(v) = kv.get("pseudo") {
                o.pseudo = v.trim().to_string();
            }
            if let Some(v) = kv.get("volume").and_then(|v| v.parse::<f32>().ok()) {
                o.volume = v.clamp(0.0, 1.0);
            }
            if let Some(v) = kv.get("audio_out") {
                o.audio_out = v.trim().to_string();
            }
            if let Some(v) = kv.get("audio_in") {
                o.audio_in = v.trim().to_string();
            }
            if let Some(v) = kv.get("volume_mic").and_then(|v| v.parse::<f32>().ok()) {
                o.volume_mic = v.clamp(0.0, 2.0);
            }
            if let Some(v) = kv.get("volume_tele").and_then(|v| v.parse::<f32>().ok()) {
                o.volume_screens = v.clamp(0.0, 1.0);
            } else if let Some(v) = kv.get("volume_screens").and_then(|v| v.parse::<f32>().ok()) {
                // (v213) Avant : 100 % par défaut. Un réglage laissé au défaut passe au
                // nouveau défaut ; un réglage choisi est gardé.
                o.volume_screens = if v >= 0.999 { 0.5 } else { v.clamp(0.0, 1.0) };
            }
            if let Some(v) = kv.get("mute_screens_menu") {
                o.mute_screens_menu = v == "true";
            }
            if let Some(v) = kv.get("volume_voice").and_then(|v| v.parse::<f32>().ok()) {
                o.volume_voice = v.clamp(0.0, 2.0);
            }
            if let Some(v) = kv.get("volume_meteo").and_then(|v| v.parse::<f32>().ok()) {
                o.volume_weather = v.clamp(0.0, 1.0);
            }
            if let Some(v) = kv.get("sensitivity").and_then(|v| v.parse::<f32>().ok()) {
                o.sensitivity = v.clamp(MIN_SENSITIVITY, MAX_SENSITIVITY);
                o.sensitivity_fps = o.sensitivity;
            }
            if let Some(v) = kv
                .get("sensitivity_fps")
                .and_then(|v| v.parse::<f32>().ok())
            {
                o.sensitivity_fps = v.clamp(MIN_SENSITIVITY, MAX_SENSITIVITY);
            }
            if let Some(v) = kv.get("fps_color").and_then(|v| v.parse::<u8>().ok()) {
                o.fps_color = if v < 5 { v } else { 255 };
            }
            if let Some(v) = kv.get("language") {
                let v = v.trim();
                if !v.is_empty() && v.len() <= 8 {
                    o.language = v.to_string();
                }
            }
        }
        o
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        fs::write(
            path,
            format!(
                "resource_pack={}\nrender_distance={}\nreflex={}\nsensitivity={}\nraytracing={}\nrender_scale={}\ncpu_sharing={}\nfullscreen={}\nvolume={}\naudio_out={}\naudio_in={}\nvolume_mic={}\nvolume_tele={}\nvolume_voice={}\nfps_max={}\nrelay={}\nrelay_key={}\npseudo={}\ngarant={}\nsensitivity_fps={}\nfps_color={}\nlanguage={}\nmute_screens_menu={}\ntorches_ponctuelles={}\ncarte_integree={}\ninvisible={}\nvolume_meteo={}\nhote_direct={}\ninvitation={}\njournal_reseau={}\nrecevoir_journaux={}\npasseur={}\n",
                self.resource_pack, self.render_distance, self.low_latency, self.sensitivity, self.raytracing, self.render_scale, if self.cpu_uniform { "uniform" } else { "reserved" }, self.fullscreen,
                self.volume,
                self.audio_out.replace('\n', " "),
                self.audio_in.replace('\n', " "),
                self.volume_mic,
                self.volume_screens,
                self.volume_voice,
                self.fps_max,
                self.relay,
                self.relay_key,
                self.pseudo,
                self.garant,
                self.sensitivity_fps,
                self.fps_color,
                self.language,
                self.mute_screens_menu,
                self.point_torches,
                self.igpu,
                self.invisible,
                self.volume_weather,
                match self.host_direct {
                    Some(true) => "true",
                    Some(false) => "false",
                    None => "auto",
                },
                self.invitation.replace('\n', " "),
                match self.share_netlog {
                    Some(true) => "true",
                    Some(false) => "false",
                    None => "demander",
                },
                self.receive_netlogs,
                match self.passeur {
                    Some(true) => "true",
                    Some(false) => "false",
                    None => "demander",
                }
            ),
        )?;
        Ok(())
    }
}

/// Lit un fichier `clé=valeur` (les lignes vides et `#` sont ignorées).
pub fn parse_kv(text: &str) -> HashMap<String, String> {
    text.lines()
        .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
        .collect()
}

// ---------------------------------------------------------------- packs

#[derive(Clone, Debug, PartialEq)]
pub struct PackInfo {
    /// Nom affiché et enregistré dans les options (nom du dossier ou du zip sans extension).
    pub name: String,
    pub path: PathBuf,
}

/// Packs présents dans `resourcepacks/` (dossiers et .zip), triés par nom.
pub fn list_packs(dir: &Path) -> Vec<PackInfo> {
    let mut out = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for e in entries.flatten() {
            let path = e.path();
            let file = e.file_name().to_string_lossy().to_string();
            if path.is_dir() {
                out.push(PackInfo { name: file, path });
            } else if file.to_lowercase().ends_with(".zip") {
                out.push(PackInfo {
                    name: file[..file.len() - 4].to_owned(),
                    path,
                });
            }
        }
    }
    out.sort_by_key(|p| p.name.to_lowercase());
    out
}

/// Image RGBA 8 bits.
#[derive(Clone, Debug)]
pub struct Rgba {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl Rgba {
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * self.width + x) * 4) as usize;
        [
            self.data[i],
            self.data[i + 1],
            self.data[i + 2],
            self.data[i + 3],
        ]
    }

    /// Agrandit/réduit au plus proche voisin (textures pixel art).
    pub fn resize_nearest(&self, w: u32, h: u32) -> Rgba {
        let mut data = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                data.extend_from_slice(&self.pixel(x * self.width / w, y * self.height / h));
            }
        }
        Rgba {
            width: w,
            height: h,
            data,
        }
    }

    /// Damier magenta/noir : texture manquante.
    pub fn missing(size: u32) -> Rgba {
        let mut data = Vec::with_capacity((size * size * 4) as usize);
        for y in 0..size {
            for x in 0..size {
                let on = (x * 2 / size + y * 2 / size).is_multiple_of(2);
                data.extend_from_slice(if on {
                    &[255, 0, 255, 255]
                } else {
                    &[0, 0, 0, 255]
                });
            }
        }
        Rgba {
            width: size,
            height: size,
            data,
        }
    }
}

enum Source {
    Dir(PathBuf),
    Zip(HashMap<String, Vec<u8>>),
}

/// Pack de ressources ouvert : accès aux fichiers par chemin relatif (`textures/blocks/stone.png`).
/// Un fichier absent du pack est pris dans le pack par défaut (un pack peut ne remplacer
/// que quelques textures).
pub struct ResourcePack {
    pub name: String,
    source: Source,
    fallback: bool,
}

/// Pack par défaut décodé une seule fois.
fn default_pack() -> Option<&'static ResourcePack> {
    static PACK: OnceLock<Option<ResourcePack>> = OnceLock::new();
    PACK.get_or_init(|| {
        read_zip(DEFAULT_PACK_ZIP).ok().map(|f| ResourcePack {
            name: DEFAULT_PACK.into(),
            source: Source::Zip(f),
            fallback: false,
        })
    })
    .as_ref()
}

impl ResourcePack {
    pub fn open(info: &PackInfo) -> Result<Self> {
        let source = if info.path.is_dir() {
            // Pack « à plat » ou avec un sous-dossier unique contenant textures/.
            let mut root = info.path.clone();
            if !root.join("textures").exists() {
                if let Some(sub) = fs::read_dir(&root)?
                    .flatten()
                    .map(|e| e.path())
                    .find(|p| p.join("textures").is_dir())
                {
                    root = sub;
                }
            }
            Source::Dir(root)
        } else {
            let bytes = fs::read(&info.path)
                .with_context(|| format!("lecture de {}", info.path.display()))?;
            Source::Zip(read_zip(&bytes)?)
        };
        Ok(Self {
            name: info.name.clone(),
            source,
            fallback: true,
        })
    }

    /// Le pack fourni avec le jeu, lu directement depuis l'exécutable.
    pub fn builtin() -> Result<Self> {
        Ok(Self {
            name: DEFAULT_PACK.into(),
            source: Source::Zip(read_zip(DEFAULT_PACK_ZIP)?),
            fallback: false,
        })
    }

    pub fn read(&self, rel: &str) -> Option<Vec<u8>> {
        self.read_own(rel).or_else(|| {
            if self.fallback {
                default_pack()?.read(rel)
            } else {
                None
            }
        })
    }

    /// Fichier du pack lui-même, sans repli.
    fn read_own(&self, rel: &str) -> Option<Vec<u8>> {
        match &self.source {
            Source::Dir(root) => fs::read(root.join(rel)).ok(),
            Source::Zip(files) => files.get(rel).cloned(),
        }
    }

    fn decode(&self, rel: &str, bytes: Vec<u8>) -> Option<Rgba> {
        match image::load_from_memory_with_format(&bytes, image::ImageFormat::Png) {
            Ok(img) => {
                let img = img.to_rgba8();
                Some(Rgba {
                    width: img.width(),
                    height: img.height(),
                    data: img.into_raw(),
                })
            }
            Err(e) => {
                log::warn!("{rel} : image illisible ({e})");
                None
            }
        }
    }

    fn image_own(&self, rel: &str) -> Option<Rgba> {
        let bytes = self.read_own(rel)?;
        self.decode(rel, bytes)
    }

    /// Texture de bloc par son nom Minelib (`planks_oak`…). Cherche dans le pack :
    /// `textures/blocks/<nom>` (packs à l'ancienne, Minecraft ≤ 1.12), puis le nom moderne
    /// `textures/block/<nom>` (Minecraft 1.13+, ex. `oak_planks`), en teintant les textures
    /// grises de Minecraft (herbe, feuillage, eau) ; à défaut, le pack par défaut.
    pub fn block_image(&self, name: &str) -> Option<Rgba> {
        if let Some(mut img) = self.image_own(&format!("textures/blocks/{name}.png")) {
            // Feuillage, herbe, eau en gris dans le pack : teinte de Minecraft.
            if let Some(color) = biome_tint(name) {
                if is_grayscale(&img) {
                    tint_gray(&mut img, color);
                }
            }
            return Some(img);
        }
        for modern in modern_names(name) {
            if let Some(img) = self.modern_block(name, &modern) {
                return Some(img);
            }
        }
        if self.fallback {
            return default_pack()?.block_image(name);
        }
        None
    }

    /// (v165) Image d'un objet (pas un bloc) par son nom de Minecraft : `textures/items/<nom>`
    /// (packs à l'ancienne), puis `textures/item/<nom>` (1.13+) ; à défaut, le pack par
    /// défaut.
    pub fn item_image(&self, name: &str) -> Option<Rgba> {
        let own = self
            .image_own(&format!("textures/items/{name}.png"))
            .or_else(|| self.image_own(&format!("textures/item/{name}.png")));
        match own {
            Some(img) => Some(img),
            None if self.fallback => default_pack()?.item_image(name),
            None => None,
        }
    }

    fn modern_block(&self, name: &str, modern: &str) -> Option<Rgba> {
        // Noms modernes dans `textures/block/` (Minecraft 1.13+) ou dans `textures/blocks/`
        // (packs libres qui gardent l'ancien dossier, comme VoxelArt Libre v3).
        let mut img = self
            .image_own(&format!("textures/block/{modern}.png"))
            .or_else(|| self.image_own(&format!("textures/blocks/{modern}.png")))?;
        // Côté de l'herbe : la bande verte est une surcouche grise à teinter.
        if modern == "grass_block_side" {
            let overlay = self
                .image_own("textures/block/grass_block_side_overlay.png")
                .or_else(|| self.image_own("textures/blocks/grass_block_side_overlay.png"));
            if let Some(mut over) = overlay {
                tint(&mut over, GRASS_TINT);
                if over.width == img.width && over.height == img.height {
                    blend_over(&mut img, &over);
                }
            }
            return Some(img);
        }
        if let Some(color) = biome_tint(name) {
            if is_grayscale(&img) {
                tint_gray(&mut img, color);
            }
        }
        Some(img)
    }

    /// Image PNG du pack (ou du pack par défaut), décodée en RGBA.
    pub fn image(&self, rel: &str) -> Option<Rgba> {
        let bytes = self.read(rel)?;
        self.decode(rel, bytes)
    }

    /// Image d'interface (`textures/gui/<nom>.png`) ; pour un pack Minecraft 1.20+, les
    /// équivalents de `gui/sprites/` (cœurs…) ; à défaut, celle du pack par défaut.
    pub fn gui_image(&self, name: &str) -> Option<Rgba> {
        if let Some(img) = self.image_own(&format!("textures/gui/{name}.png")) {
            return Some(img);
        }
        let modern = match name {
            "icons/health_full" => Some("sprites/hud/heart/full"),
            "icons/health_half" => Some("sprites/hud/heart/half"),
            "icons/health_empty" => Some("sprites/hud/heart/container"),
            "crosshair" => Some("sprites/hud/crosshair"),
            "hotbar_selected" => Some("sprites/hud/hotbar_selection"),
            "button" => Some("sprites/widget/button"),
            "button_hover" => Some("sprites/widget/button_highlighted"),
            "button_disabled" => Some("sprites/widget/button_disabled"),
            _ => None,
        };
        if let Some(img) = modern.and_then(|m| self.image_own(&format!("textures/gui/{m}.png"))) {
            return Some(img);
        }
        self.image(&format!("textures/gui/{name}.png"))
    }

    /// Nombre de textures de blocs que le pack fournit lui-même (diagnostic).
    pub fn own_block_textures(&self, names: &[&str]) -> usize {
        names
            .iter()
            .filter(|n| {
                self.read_own(&format!("textures/blocks/{n}.png")).is_some()
                    || modern_names(n).iter().any(|m| {
                        self.read_own(&format!("textures/block/{m}.png")).is_some()
                            || self.read_own(&format!("textures/blocks/{m}.png")).is_some()
                    })
            })
            .count()
    }
}

// ---------------------------------------------------------------- packs Minecraft modernes

/// Couleurs de biome par défaut (plaines) pour les textures grises de Minecraft.
const GRASS_TINT: [u8; 3] = [145, 189, 89];
const FOLIAGE_TINT: [u8; 3] = [119, 171, 47];
const WATER_TINT: [u8; 3] = [63, 118, 228];

fn biome_tint(name: &str) -> Option<[u8; 3]> {
    Some(match name {
        "grass_top"
        | "tallgrass"
        | "fern"
        | "double_plant_grass_bottom"
        | "double_plant_grass_top"
        | "double_plant_fern_bottom"
        | "double_plant_fern_top" => GRASS_TINT,
        "leaves_spruce" | "leaves_spruce_opaque" => [97, 153, 97],
        "leaves_birch" | "leaves_birch_opaque" => [128, 167, 85],
        "water_still" | "water_flow" => WATER_TINT,
        // (v179) Feuilles de palétuvier : grises dans les packs, teintées par le jeu.
        "mangrove_leaves" => [141, 177, 39],
        n if n.starts_with("leaves_") || n == "vine" || n == "waterlily" => FOLIAGE_TINT,
        _ => return None,
    })
}

/// Texture surtout grise (à teinter comme Minecraft) : au moins 60 % des pixels visibles
/// sans couleur. Les feuilles de jungle ont des fruits jaunes (≈ 12 % des pixels) : la
/// moyenne seule les faisait passer pour colorées et elles restaient grises.
/// Pixel « sans couleur » : gris pur, ou gris chaud peu saturé (base sépia de Pixel
/// Perfection, utilisée par VoxelArt Libre v3), à teinter comme du gris.
fn gray_px(px: &[u8]) -> bool {
    let (hi, lo) = (
        px[0].max(px[1]).max(px[2]) as u32,
        px[0].min(px[1]).min(px[2]) as u32,
    );
    hi - lo < 12 || (hi - lo) * 100 < hi * 25
}

fn is_grayscale(img: &Rgba) -> bool {
    let (mut gray, mut n) = (0u64, 0u64);
    for px in img.data.chunks_exact(4).filter(|p| p[3] > 0) {
        if gray_px(px) {
            gray += 1;
        }
        n += 1;
    }
    n > 0 && gray * 10 >= n * 6
}

/// Teinte seulement les pixels gris : les détails déjà colorés du pack (fruits jaunes des
/// feuilles de jungle, fleurs…) gardent leur couleur.
fn tint_gray(img: &mut Rgba, c: [u8; 3]) {
    for px in img.data.chunks_exact_mut(4) {
        if gray_px(px) {
            // Luminosité du pixel × teinte (une base sépia ne doit pas brunir le vert).
            let l = (px[0] as u32 + px[1] as u32 + px[2] as u32) / 3;
            for k in 0..3 {
                px[k] = (l * c[k] as u32 / 255) as u8;
            }
        }
    }
}

fn tint(img: &mut Rgba, c: [u8; 3]) {
    for px in img.data.chunks_exact_mut(4) {
        for k in 0..3 {
            px[k] = (px[k] as u32 * c[k] as u32 / 255) as u8;
        }
    }
}

fn blend_over(dst: &mut Rgba, src: &Rgba) {
    for (d, s) in dst.data.chunks_exact_mut(4).zip(src.data.chunks_exact(4)) {
        let a = s[3] as u32;
        for k in 0..3 {
            d[k] = ((s[k] as u32 * a + d[k] as u32 * (255 - a)) / 255) as u8;
        }
    }
}

const COLORS: [&str; 16] = [
    "white",
    "orange",
    "magenta",
    "light_blue",
    "yellow",
    "lime",
    "pink",
    "gray",
    "silver",
    "cyan",
    "purple",
    "blue",
    "brown",
    "green",
    "red",
    "black",
];

fn modern_color(c: &str) -> &str {
    if c == "silver" {
        "light_gray"
    } else {
        c
    }
}

fn modern_wood(w: &str) -> &str {
    if w == "big_oak" {
        "dark_oak"
    } else {
        w
    }
}

/// Noms possibles d'une texture Minelib/1.12 dans un pack Minecraft 1.13+ (du plus probable
/// au moins probable). Le nom identique est toujours essayé en dernier.
pub fn modern_names(name: &str) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    let n = name.strip_suffix("_opaque").unwrap_or(name);
    let fixed: &[(&str, &[&str])] = &[
        ("grass_top", &["grass_block_top"]),
        ("grass_side", &["grass_block_side"]),
        ("stone_granite", &["granite"]),
        ("stone_granite_smooth", &["polished_granite"]),
        ("stone_diorite", &["diorite"]),
        ("stone_diorite_smooth", &["polished_diorite"]),
        ("stone_andesite", &["andesite"]),
        ("stone_andesite_smooth", &["polished_andesite"]),
        ("stonebrick", &["stone_bricks"]),
        ("stonebrick_mossy", &["mossy_stone_bricks"]),
        ("stonebrick_cracked", &["cracked_stone_bricks"]),
        ("stonebrick_carved", &["chiseled_stone_bricks"]),
        ("cobblestone_mossy", &["mossy_cobblestone"]),
        ("brick", &["bricks"]),
        ("hardened_clay", &["terracotta"]),
        ("sandstone_normal", &["sandstone"]),
        ("sandstone_carved", &["chiseled_sandstone"]),
        ("sandstone_smooth", &["cut_sandstone"]),
        ("red_sandstone_normal", &["red_sandstone"]),
        ("red_sandstone_carved", &["chiseled_red_sandstone"]),
        ("red_sandstone_smooth", &["cut_red_sandstone"]),
        (
            "quartz_block_lines",
            &["quartz_pillar_side", "quartz_pillar"],
        ),
        ("quartz_block_lines_top", &["quartz_pillar_top"]),
        ("quartz_block_chiseled", &["chiseled_quartz_block"]),
        ("quartz_block_chiseled_top", &["chiseled_quartz_block_top"]),
        ("stone_slab_top", &["smooth_stone"]),
        ("stone_slab_side", &["smooth_stone_slab_side"]),
        ("prismarine_rough", &["prismarine"]),
        ("prismarine_dark", &["dark_prismarine"]),
        ("hellrock", &["netherrack"]),
        ("hellrock_brick", &["nether_bricks", "hell_bricks"]),
        ("ash_sand", &["soul_sand"]),
        ("energy_block", &["redstone_block"]),
        ("energy_lamp_on", &["redstone_lamp_on"]),
        ("energy_lamp_off", &["redstone_lamp", "energy_lamp"]),
        ("dirt_podzol_top", &["podzol_top"]),
        ("dirt_podzol_side", &["podzol_side"]),
        ("torch_on", &["torch"]),
        ("tallgrass", &["short_grass", "grass"]),
        ("deadbush", &["dead_bush"]),
        ("flower_rose", &["poppy"]),
        ("flower_dandelion", &["dandelion"]),
        ("flower_houstonia", &["azure_bluet"]),
        ("flower_blue_orchid", &["blue_orchid"]),
        ("flower_allium", &["allium"]),
        ("flower_oxeye_daisy", &["oxeye_daisy"]),
        ("flower_tulip_red", &["red_tulip"]),
        ("flower_tulip_orange", &["orange_tulip"]),
        ("flower_tulip_white", &["white_tulip"]),
        ("flower_tulip_pink", &["pink_tulip"]),
        ("double_plant_grass_bottom", &["tall_grass_bottom"]),
        ("double_plant_grass_top", &["tall_grass_top"]),
        ("double_plant_fern_bottom", &["large_fern_bottom"]),
        ("double_plant_fern_top", &["large_fern_top"]),
        ("double_plant_syringa_bottom", &["lilac_bottom"]),
        ("double_plant_syringa_top", &["lilac_top"]),
        ("double_plant_paeonia_bottom", &["peony_bottom"]),
        ("double_plant_paeonia_top", &["peony_top"]),
        ("double_plant_rose_bottom", &["rose_bush_bottom"]),
        ("double_plant_rose_top", &["rose_bush_top"]),
        ("double_plant_sunflower_bottom", &["sunflower_bottom"]),
        ("double_plant_sunflower_front", &["sunflower_front"]),
        ("mushroom_red", &["red_mushroom"]),
        ("mushroom_brown", &["brown_mushroom"]),
        ("reeds", &["sugar_cane"]),
        ("wheat_stage_7", &["wheat_stage7"]),
        ("waterlily", &["lily_pad"]),
        ("web", &["cobweb"]),
        ("trapdoor", &["oak_trapdoor"]),
        ("door_wood_lower", &["oak_door_bottom"]),
        ("door_wood_upper", &["oak_door_top"]),
        ("hay_block_side", &["hay_block_side"]),
        ("pumpkin_face_on", &["jack_o_lantern"]),
        ("noteblock", &["note_block"]),
        ("slime", &["slime_block"]),
        ("dispenser_front_horizontal", &["dispenser_front"]),
        ("rail_normal", &["rail"]),
        ("repeater_off", &["repeater"]),
        ("comparator_off", &["comparator"]),
        ("anvil_base", &["anvil"]),
        ("farmland_dry", &["farmland"]),
        ("grass_path_top", &["dirt_path_top"]),
        ("mycelium_top", &["mycelium_top"]),
        ("crafting_table_side", &["crafting_table_side"]),
        ("furnace_side", &["furnace_side"]),
        ("piston_top_normal", &["piston_top"]),
    ];
    if let Some((_, list)) = fixed.iter().find(|(k, _)| *k == n) {
        v.extend(list.iter().map(|s| s.to_string()));
    }
    if let Some(w) = n.strip_prefix("planks_") {
        v.push(format!("{}_planks", modern_wood(w)));
    }
    if let Some(w) = n.strip_prefix("log_") {
        match w.strip_suffix("_top") {
            Some(w) => v.push(format!("{}_log_top", modern_wood(w))),
            None => v.push(format!("{}_log", modern_wood(w))),
        }
    }
    if let Some(w) = n.strip_prefix("leaves_") {
        v.push(format!("{}_leaves", modern_wood(w)));
    }
    if let Some(w) = n.strip_prefix("sapling_") {
        v.push(format!("{}_sapling", modern_wood(w)));
    }
    if let Some(rest) = n.strip_prefix("door_") {
        if let Some((w, half)) = rest.rsplit_once('_') {
            let half = if half == "lower" { "bottom" } else { "top" };
            v.push(format!(
                "{}_door_{half}",
                if w == "wood" { "oak" } else { w }
            ));
        }
    }
    for c in COLORS {
        let m = modern_color(c);
        if n == format!("wool_colored_{c}") {
            v.push(format!("{m}_wool"));
        }
        if n == format!("hardened_clay_stained_{c}") {
            v.push(format!("{m}_terracotta"));
        }
        if n == format!("glass_{c}") {
            v.push(format!("{m}_stained_glass"));
        }
        if n == format!("glass_pane_top_{c}") {
            v.push(format!("{m}_stained_glass_pane_top"));
        }
    }
    v.push(n.to_string());
    v.dedup();
    v
}

/// Décompresse les fichiers utiles d'un zip en mémoire. Si tout le contenu est dans un
/// dossier racine unique (`MonPack/textures/...`), ce préfixe est retiré.
fn read_zip(bytes: &[u8]) -> Result<HashMap<String, Vec<u8>>> {
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).context("archive zip invalide")?;
    let names: Vec<String> = zip.file_names().map(str::to_owned).collect();
    // Racine du pack : celle de Minecraft (`…/assets/minecraft/`) en priorité — un pack peut
    // aussi contenir d'autres espaces de noms (fabric, forge…) — sinon le dossier qui
    // contient `textures/blocks` ou `textures/block`, sinon le premier `textures/`.
    let find = |pat: &str, keep: usize| {
        names
            .iter()
            .find_map(|n| n.find(pat).map(|i| n[..i + keep].to_owned()))
    };
    let prefix = find("assets/minecraft/textures/", "assets/minecraft/".len())
        .or_else(|| find("textures/blocks/", 0))
        .or_else(|| find("textures/block/", 0))
        .or_else(|| find("textures/", 0))
        .unwrap_or_default();
    let mut files = HashMap::new();
    for i in 0..zip.len() {
        let mut f = zip.by_index(i)?;
        if f.is_dir() {
            continue;
        }
        let name = f.name().to_owned();
        let Some(rel) = name.strip_prefix(&prefix) else {
            continue;
        };
        // Les sons et musiques ne sont pas encore utilisés : on ne garde que le reste.
        if rel.starts_with("sounds/") {
            continue;
        }
        let mut buf = Vec::with_capacity(f.size() as usize);
        f.read_to_end(&mut buf)?;
        files.insert(rel.to_owned(), buf);
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// (v213) Le son des écrans : 50 % par défaut ; l'ancien défaut (100 %) y passe, un
    /// réglage choisi reste, et la nouvelle clé est relue telle quelle.
    /// (v298) `hote_direct` a trois valeurs (rien ou `auto`, `true`, `false`) et
    /// l'invitation d'un ami est gardée telle quelle quand les options sont réécrites.
    #[test]
    fn l_hote_direct_et_l_invitation_se_relisent() {
        let dir = std::env::temp_dir().join(format!("minelib-hote-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let f = dir.join("options.txt");
        assert_eq!(Options::load(&dir.join("absent.txt")).host_direct, None);
        let invite = format!("{} {} Relais d'un ami", "ab".repeat(32), "cd".repeat(32));
        for (texte, voulu) in [
            ("hote_direct=true\n", Some(true)),
            ("hote_direct=false\n", Some(false)),
            ("hote_direct=auto\n", None),
            ("volume=1\n", None),
        ] {
            fs::write(&f, format!("{texte}invitation={invite}\n")).unwrap();
            let o = Options::load(&f);
            assert_eq!(o.host_direct, voulu, "{texte}");
            assert_eq!(o.invitation, invite);
            o.save(&f).unwrap();
            let again = Options::load(&f);
            assert_eq!((again.host_direct, again.invitation.as_str()), (voulu, invite.as_str()));
            // (v307) Le partage du journal réseau : pas encore demandé, puis oui ou non ;
            // la réception, non sans la ligne.
            assert_eq!((again.share_netlog, again.receive_netlogs), (None, false));
            assert_eq!(again.passeur, None);
            let mut o = again;
            o.share_netlog = Some(voulu != Some(false));
            o.receive_netlogs = true;
            o.save(&f).unwrap();
            let back = Options::load(&f);
            assert_eq!(back.share_netlog, Some(voulu != Some(false)));
            assert!(back.receive_netlogs);
            let mut o = back;
            o.passeur = Some(true);
            o.save(&f).unwrap();
            assert_eq!(Options::load(&f).passeur, Some(true));
        }
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn le_son_des_ecrans_est_a_50_par_defaut() {
        let dir = std::env::temp_dir().join(format!("minelib-tele-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let f = dir.join("options.txt");
        assert_eq!(Options::load(&dir.join("absent.txt")).volume_screens, 0.5);
        for (texte, voulu) in [
            ("volume_screens=1\n", 0.5),
            ("volume_screens=0.3\n", 0.3),
            ("volume_tele=1\n", 1.0),
            ("volume_tele=0.8\nvolume_screens=1\n", 0.8),
        ] {
            fs::write(&f, texte).unwrap();
            assert_eq!(Options::load(&f).volume_screens, voulu, "{texte}");
        }
        let mut o = Options::load(&f);
        o.volume_screens = 1.0;
        o.save(&f).unwrap();
        assert_eq!(Options::load(&f).volume_screens, 1.0);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn builtin_pack_has_textures() {
        let p = ResourcePack::builtin().unwrap();
        let stone = p.image("textures/blocks/stone.png").unwrap();
        assert_eq!((stone.width, stone.height), (16, 16));
        assert!(p.image("textures/gui/button.png").is_some());
        assert!(p.read("CREDITS.txt").is_some());
    }

    #[test]
    fn partial_pack_falls_back_to_default() {
        let dir = std::env::temp_dir().join(format!("minelib-pack-{}", std::process::id()));
        fs::create_dir_all(dir.join("textures/blocks")).unwrap();
        let img = image::RgbaImage::from_pixel(16, 16, image::Rgba([255, 0, 0, 255]));
        img.save(dir.join("textures/blocks/stone.png")).unwrap();
        let p = ResourcePack::open(&PackInfo {
            name: "rouge".into(),
            path: dir.clone(),
        })
        .unwrap();
        assert_eq!(
            p.image("textures/blocks/stone.png").unwrap().pixel(0, 0),
            [255, 0, 0, 255]
        );
        assert!(
            p.image("textures/blocks/dirt.png").is_some(),
            "repli sur le pack par défaut"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn only_gray_pixels_are_tinted() {
        // Feuille grise avec un fruit jaune : le gris devient vert, le jaune reste jaune.
        let mut img = Rgba {
            width: 2,
            height: 1,
            data: vec![120, 120, 120, 255, 230, 200, 40, 255],
        };
        tint_gray(&mut img, FOLIAGE_TINT);
        assert!(
            img.data[1] > img.data[0] && img.data[1] > img.data[2],
            "gris teinté en vert"
        );
        assert_eq!(&img.data[4..8], &[230, 200, 40, 255], "jaune inchangé");
    }

    #[test]
    fn minecraft_style_pack_is_found_and_tinted() {
        use std::io::Write;
        // Zip façon pack Minecraft moderne, avec un autre espace de noms en premier.
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut z = zip::ZipWriter::new(&mut buf);
            let opt = zip::write::SimpleFileOptions::default();
            let png = |c: [u8; 4]| {
                let mut out = Vec::new();
                image::RgbaImage::from_pixel(32, 32, image::Rgba(c))
                    .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
                    .unwrap();
                out
            };
            z.start_file("assets/fabric/textures/gui/x.png", opt)
                .unwrap();
            z.write_all(&png([1, 2, 3, 255])).unwrap();
            z.start_file("assets/minecraft/textures/block/oak_planks.png", opt)
                .unwrap();
            z.write_all(&png([200, 100, 50, 255])).unwrap();
            z.start_file("assets/minecraft/textures/block/grass_block_top.png", opt)
                .unwrap();
            z.write_all(&png([200, 200, 200, 255])).unwrap();
            z.finish().unwrap();
        }
        let dir = std::env::temp_dir().join(format!("minelib-mcpack-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("Faithful.zip");
        fs::write(&path, buf.into_inner()).unwrap();
        let p = ResourcePack::open(&PackInfo {
            name: "Faithful".into(),
            path,
        })
        .unwrap();
        let planks = p.block_image("planks_oak").unwrap();
        assert_eq!(
            (planks.width, planks.pixel(0, 0)),
            (32, [200, 100, 50, 255])
        );
        let grass = p.block_image("grass_top").unwrap();
        assert!(
            grass.pixel(0, 0)[1] > grass.pixel(0, 0)[0] + 20,
            "herbe teintée en vert : {:?}",
            grass.pixel(0, 0)
        );
        // Absent du pack : pack par défaut.
        assert_eq!(p.block_image("stone").unwrap().width, 16);
        assert_eq!(
            p.own_block_textures(&["planks_oak", "grass_top", "stone"]),
            2
        );
        assert_eq!(modern_names("leaves_big_oak_opaque")[0], "dark_oak_leaves");
        assert_eq!(modern_names("door_spruce_lower")[0], "spruce_door_bottom");
        assert_eq!(modern_names("wool_colored_silver")[0], "light_gray_wool");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn init_creates_layout_and_installs_default_pack() {
        let dir = std::env::temp_dir().join(format!("minelib-test-{}", std::process::id()));
        std::env::set_var("MINELIB_HOME", &dir);
        let paths = init().unwrap();
        assert!(paths.saves.is_dir());
        let packs = list_packs(&paths.resourcepacks);
        assert_eq!(packs.len(), 1);
        assert_eq!(packs[0].name, DEFAULT_PACK);
        let pack = ResourcePack::open(&packs[0]).unwrap();
        assert!(pack.block_image("grass_top").is_some());
        let mut o = Options::load(&paths.options);
        // (v201) Torches en lumières ponctuelles : non par défaut, gardé ensuite. (v228)
        // Pareil pour la partie graphique intégrée.
        assert!(!o.point_torches && !o.igpu && !o.invisible);
        // (v234) Volume de la météo : 100 % par défaut, gardé ensuite.
        assert_eq!(o.volume_weather, 1.0);
        o.resource_pack = "Autre".into();
        o.point_torches = true;
        o.igpu = true;
        o.invisible = true;
        o.volume_weather = 0.35;
        o.save(&paths.options).unwrap();
        let back = Options::load(&paths.options);
        assert_eq!(back.resource_pack, "Autre");
        assert!(back.point_torches && back.igpu && back.invisible);
        assert_eq!(back.volume_weather, 0.35);
        let _ = fs::remove_dir_all(dir);
    }
}
