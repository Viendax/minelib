//! Minelib — moteur de jeu voxel / FPS.
//!
//! Écrans : accueil → Solo (liste des mondes, nouveau monde) → jeu ; packs de textures ;
//! menu Échap (reprendre, packs de textures, sauvegarder et quitter).
//!
//! Données du joueur : `%APPDATA%\.minelib` sous Windows, `~/.minelib` sous Linux,
//! avec `resourcepacks/` (packs de textures, dossiers ou .zip) et `saves/` (mondes).
//!
//! Commandes en jeu (touches physiques, valables en AZERTY comme en QWERTY) :
//!   clic           capturer la souris          Échap        menu pause
//!   Z Q S D        se déplacer (W A S D en QWERTY)
//!   Espace         sauter / monter en vol      Maj gauche   descendre en vol
//!   Ctrl gauche    courir                       F            voler / marcher
//!   clic gauche    casser                       clic droit   poser
//!   1 à 0, molette choisir dans la barre        clic molette copier le bloc visé
//!   E              inventaire (10 × 3)          N            jour / nuit
//!   C (maintenu)   zoom                         V            VSync on/off
//!   Page ↑ / ↓     distance de rendu
//!   F3             écran de debug
//!   L              éditeur de lumières (F : caméra libre ; Suppr : effacer la lumière)
//!
//! Variables d'environnement :
//!   MINELIB_HOME=<dossier>      remplace le dossier .minelib
//!   MINELIB_WORLD=<nom>         ouvre (ou crée) directement ce monde
//!   MINELIB_SEED=<n>            graine d'un monde créé par MINELIB_WORLD
//!   MINELIB_VIEW=<chunks>       distance de rendu (défaut 10)
//!   MINELIB_VSYNC=1             démarrer avec la VSync (défaut : FPS illimités)
//!   MINELIB_FPS_MAX=<n>         plafond de FPS optionnel
//!   MINELIB_DEBUG=1             F3 ouvert au démarrage
//!   MINELIB_NIGHT=1             commencer de nuit
//!   MINELIB_HEURE=<h>           commencer à cette heure (0 … 24), arrêtée (essais du ciel)
//!   MINELIB_LUNE=<jours>        âge de la lune au départ (0 pleine … 4 nouvelle … 8)
//!   MINELIB_METEO=clear|rain|storm  météo imposée et figée dès l'arrivée (essais)
//!   MINELIB_ZEUS=1|2            la Foudre de Zeus en main à l'arrivée (essais ; 2 : sur soi,
//!                               dans la case d'à côté, pas en main) ;
//!                               MINELIB_ZEUS_CHARGES=<n> : avec tant de charges (0 à 10)
//!   MINELIB_THOR=<s>            la foudre tombe là où l'on regarde toutes les <s> s (essais)
//!   MINELIB_F5=1|2              commencer vu de derrière (1) ou de face (2) (essais)
//!   MINELIB_FOUDRE_FIGEE=1      chaque éclair s'arrête à son plus vif (captures d'essai) ;
//!                               un autre nombre : à tant de secondes après le premier arc
//!   MINELIB_FUSION=0            ne fusionne pas les faces de blocs voisines (essais : pour
//!                               comparer l'image et le nombre de triangles)
//!   MINELIB_EN_LIGNE=1          ouvre MINELIB_WORLD en ligne (construction), dès que le
//!                               compte est connecté (voir MINELIB_TEST_ACCOUNT) (essais)
//!   MINELIB_REJOINDRE=<texte>   rejoint le premier serveur de construction dont le nom
//!                               contient ce texte, dès que le compte est connecté (essais)
//!   MINELIB_CMD=<s>:<commande>[|<s>:<commande>…]  tape ces commandes du tchat, chacune
//!                               <s> secondes après l'entrée dans le monde (essais)
//!   MINELIB_DEMO=1              nouveau monde : mur + toutes les torches colorées
//!   MINELIB_START=<x>,<y>,<z>[,lacet°,tangage°]  position de départ (vol activé ; aussi
//!                               dans un monde neuf et dans la démo)
//!   MINELIB_FPS_START=<x>,<y>,<z>,lacet°  partie FPS : départ du joueur local fixé (essais)
//!   MINELIB_EXIT_AFTER=<s>      quitter automatiquement (tests)
//!   MINELIB_TOURNE=<amplitude°>,<période s>  la vue oscille de gauche à droite (essais des
//!                               rayons quand la caméra tourne vite)
//!   MINELIB_TAILLE=<l>x<h>      taille de la fenêtre au lancement (essais ; 1280x720 sinon)
//!   MINELIB_PAS_FIXE=<n>        chaque image fait avancer le jeu de 1/<n> s, quel que soit le
//!                               temps qu'elle a pris (essais : les déplacements d'une image à
//!                               l'autre d'une machine à <n> FPS, sous un rendu lent)
//!   MINELIB_SHOT_AT=<s>         capture d'écran automatique à <s> secondes (tests)
//!   MINELIB_CAPTURES=<s>,<n>    une capture à chaque image, n de suite à partir de <s> s (tests)
//!   MINELIB_UI=lobby            écran FPS avec un lobby d'exemple (essais de l'interface) ;
//!                               (v340) MINELIB_EQUIPE=<n> format choisi, MINELIB_EQUIPE_LISTE=1
//!                               sa liste ouverte, MINELIB_AJOUT=1 « Ajouter une carte » ouvert
//!   MINELIB_UI=lumieres         éditeur de lumières ouvert en entrant dans le monde (essais)
//!   MINELIB_UI=settings|graphics paramètres, ou paramètres graphiques, au lancement (essais)
//!   MINELIB_UI=reglages_lumieres paramètres graphiques → lumières, au lancement (essais)
//!   MINELIB_LUMIERES_LENTES=<s> shaders des lumières prêts <s> secondes plus tard (essais)
//!   MINELIB_SMOKE=<x>,<y>,<z>   mode FPS : fumigène posée là toutes les 18 s (essais)
//!   MINELIB_SMOKE_Q=<n>,<pas>   qualité des fumigènes : pas max et longueur (essais)
//!   MINELIB_FEU=<distance>      mode FPS : feu de molotov devant soi toutes les 9 s (essais)

// Windows : pas de fenêtre de console à côté du jeu (le journal va dans .minelib/logs).
#![cfg_attr(windows, windows_subsystem = "windows")]

mod account;
mod anticheat;
mod audio;
mod bot_skin;
mod bots;
mod bucket;
mod buildnet;
mod chat;
mod cmdhelp;
mod corpse;
mod crowd;
mod cs;
mod daytime;
mod debug;
mod drops;
mod edge;
mod equip;
mod filtres;
mod navigateur;
mod flashlight;
mod evide;
mod flesh;
mod fluids;
mod fps;
mod fpsmap;
mod frames;
mod friends;
mod garant;
mod garant_worker;
mod gpu_plan;
mod gpu_report;
mod hand;
mod heads;
mod i18n;
mod inventory;
mod jobs;
mod keycomp;
mod keylag;
mod keytest;
mod keys;
mod laps;
mod light_editor;
mod lightnet;
mod lightning;
mod lights;
mod mcimport;
mod menu;
mod multiplayer;
mod netgame;
mod online_copies;
mod passkeys;
mod player;
mod player_model;
mod province_host;
mod radar;
mod save;
mod screen;
mod session;
mod skin;
mod solo_maps;
mod spotting;
mod stick;
mod storm;
mod streaming;
mod traces;
mod underwater;
mod update;
mod voice;
mod wand;
mod watch;
mod weapons;
mod weather;
mod worldedit;
mod zeus;

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use menu::{Ctx, GOLD, GREY, RED, WHITE};
use minelib_assets::{list_packs, Options, PackInfo, Paths, ResourcePack};
use minelib_platform::sysmon::SysMonitor;
use minelib_platform::{GameLoop, Input, KeyCode, PlatformWindow};
use minelib_render::{FrameStats, Renderer, Ui};
use save::WorldSummary;
use session::{Session, SessionConfig};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Screen {
    Title,
    Solo,
    NewWorld,
    Packs,
    Settings,
    Graphics,
    /// (v215) Paramètres graphiques → Lumières : réglages de toutes les lumières.
    Lights,
    Controls,
    /// Paramètres → Son : sortie et micro, volumes.
    Sound,
    /// (v307) Paramètres → Paramètres avancés : journal réseau (partage, réception).
    Advanced,
    /// (v311) `/deleteworld` : le créateur confirme la suppression de son monde.
    DeleteWorld,
    /// (v326) Un nouveau monde en ligne, à la limite : confirmer la suppression du plus ancien
    /// monde endormi.
    WorldLimit,
    /// Conversion d'un monde Minecraft en cours (une image affichée avant de bloquer).
    Loading,
    Playing,
    Paused,
    /// Multijoueur : choix du mode, puis liste des parties de ce mode.
    Multiplayer,
    Servers(multiplayer::Kind),
    /// FPS : classement et choix de la carte.
    FpsHome,
    /// Compte : accueil, connexion, inscription, profil, double authentification.
    Account(account::Page),
    /// Choix du monde à ouvrir en construction en ligne.
    HostPick,
    /// (v232) Solo → Cartes FPS : jouer contre des bots sur une carte FPS (de ce PC, ou du
    /// relais, copiée dans ses mondes).
    SoloMaps,
}

/// (v215) Sorties son et micros du système, relevés en arrière-plan : le relevé (cpal)
/// prend jusqu'à une seconde sous Windows, l'écran Son s'ouvre tout de suite avec le
/// dernier.
#[derive(Default)]
struct DeviceScan {
    lists: std::sync::Arc<std::sync::Mutex<(Vec<String>, Vec<String>)>>,
    busy: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl DeviceScan {
    /// Premier relevé lancé dès le démarrage du jeu.
    fn started() -> Self {
        let s = Self::default();
        s.refresh();
        s
    }

    /// Refait le relevé en arrière-plan (sauf s'il est déjà en cours).
    fn refresh(&self) {
        use std::sync::atomic::Ordering;
        if self.busy.swap(true, Ordering::AcqRel) {
            return;
        }
        let (lists, busy) = (self.lists.clone(), self.busy.clone());
        let spawned = std::thread::Builder::new()
            .name("relevé du son".into())
            .spawn(move || {
                let found = (
                    crate::audio::output_devices(),
                    crate::voice::input_devices(),
                );
                if let Ok(mut l) = lists.lock() {
                    *l = found;
                }
                busy.store(false, Ordering::Release);
            });
        if spawned.is_err() {
            self.busy.store(false, Ordering::Release);
        }
    }

    /// Le dernier relevé (vide tant que le premier n'est pas fini).
    fn get(&self) -> (Vec<String>, Vec<String>) {
        self.lists.lock().map(|l| l.clone()).unwrap_or_default()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    Name,
    Seed,
}

/// (v325) Ouverture (ou réveil) d'un monde en ligne envoyée au relais, pas encore confirmée.
struct Hosting {
    /// La copie en ligne où l'on joue ; `fresh` : faite pour cette ouverture (rien à garder
    /// si le relais la refuse).
    dir: PathBuf,
    fresh: bool,
    /// Le monde à envoyer au relais une fois ouvert (signature, nom, paquet) : rien ne part
    /// pour une ouverture refusée.
    upload: ([u8; 32], String, Vec<u8>),
    /// (v326) Le monde endormi supprimé pour faire place à celui-ci (sa copie sur ce PC est
    /// effacée une fois celui-ci ouvert).
    replaced: Option<String>,
    /// (v330) Réveil d'un monde dont la copie n'a pas la clé de terrain (gardée par un invité
    /// devenu propriétaire) : la partie ne s'ouvre qu'une fois le monde réveillé, avec la clé
    /// que donne le relais.
    deferred: bool,
}

/// (v326) Comment ouvrir un monde en ligne.
enum Open {
    /// Un nouveau serveur.
    New,
    /// Un nouveau serveur qui fait place en supprimant le monde endormi `world` (confirmé ;
    /// `code` : double authentification).
    Replacing { world: String, code: Option<u32> },
    /// Le monde endormi `world` réveillé, sous son nom `title`.
    Wake { title: String, world: String },
}

/// (v326) Nouveau monde à la limite : ce que le relais demande de confirmer.
struct LimitAsk {
    /// La copie qui attend d'être ouverte, faite pour cette ouverture ou non, et sa carte.
    dir: PathBuf,
    fresh: bool,
    map: String,
    /// Le monde endormi qui ferait place (clé, nom), avec un code de double
    /// authentification (`totp`) ; le code tapé, et s'il vient d'être refusé.
    world: String,
    name: String,
    totp: bool,
    code: String,
    wrong: bool,
    /// Un code est parti avec la dernière confirmation.
    sent_code: bool,
}

struct Game {
    /// Compte du joueur et dialogue avec le relais.
    accounts: account::Accounts,
    /// Écran où aller une fois connecté.
    after_login: Screen,
    /// Parties annoncées par le relais (vide tant que le réseau n'existe pas) et ligne
    /// choisie dans la liste.
    servers: Vec<multiplayer::ServerInfo>,
    server_selected: Option<usize>,
    fps_home: multiplayer::FpsHome,
    fps_home_maps_seen: u64,
    /// (v232) Solo → Cartes FPS : état de l'écran, dernière liste du relais vue, et carte du
    /// relais en cours de téléchargement (signature, nom, à jouer ensuite).
    solo_maps: solo_maps::SoloMaps,
    solo_maps_seen: u64,
    solo_map_pending: Option<([u8; 32], String, bool)>,
    /// Lignes de l'écran (index dans `map_rows`, ce qu'elles montrent), refaites quand la
    /// liste change (pas à chaque image : elles regardent les mondes sur le disque).
    solo_rows: (Vec<usize>, Vec<solo_maps::SoloRow>),
    /// Cartes de ce PC déjà jouées en partie FPS en ligne (hébergées ou rejointes) : on
    /// les renvoie au relais s'il ne les a pas, pour qu'elles restent disponibles à tous.
    /// Les autres mondes du joueur ne sont jamais partagés.
    shared_maps: std::collections::HashSet<[u8; 32]>,
    reshared: std::collections::HashSet<[u8; 32]>,
    maps_rev_seen: u64,
    maps_asked: bool,
    /// Son skin (`.minelib/skin.png`), envoyé au relais à la connexion ; fenêtre « Ouvrir »
    /// en cours ; message de l'écran des paramètres ; rotation de l'aperçu.
    my_skin: Option<std::sync::Arc<skin::Skin>>,
    skin_sent: bool,
    skin_pick: Option<std::sync::mpsc::Receiver<Option<PathBuf>>>,
    skin_msg: Option<(String, bool)>,
    /// Menu déroulant des langues ouvert.
    /// Résultat de « Télécharger la carte » (menu pause d'un serveur de construction).
    download_note: Option<String>,
    /// Menu Échap d'une partie FPS : liste des coéquipiers à exclure ouverte.
    kick_pick: bool,
    /// Amis connus (pour ne refaire leur classement que si la liste change).
    friend_names: Vec<String>,
    /// Lobby : l'hôte du lobby a trouvé cette partie, on la rejoint (après la carte).
    party_go: Option<u64>,
    map_rows: Vec<multiplayer::MapRow>,
    /// (v340) Multijoueur → FPS → « Ajouter une carte » : ses mondes, et la carte en cours
    /// d'envoi au relais (signature, nom).
    add_rows: Vec<multiplayer::AddRow>,
    map_adding: Option<([u8; 32], String)>,
    /// (v340) `/fps certif` envoyé au relais (horloge) : sa réponse, même un refus, est dite
    /// dans le tchat.
    certif_asked: Option<f32>,
    sig_cache: fpsmap::SigCache,
    /// Recherche FPS en cours : carte (signature, nom, dossier s'il est là).
    searching: Option<([u8; 32], String, Option<PathBuf>)>,
    /// Monde de construction rejoint mais pas encore téléchargé : (signature, nom, hôte).
    pending_build: Option<([u8; 32], String, String)>,
    /// (v325) Monde de construction en ligne où l'on est : (relais, clé du monde), dit par le
    /// relais en y entrant (`Event::World`). Propriétaire, sa copie le garde en partant.
    world_key: Option<(String, String)>,
    /// (v325) Ouverture (ou réveil) d'un monde en ligne envoyée, pas encore confirmée.
    hosting: Option<Hosting>,
    /// (v330) Clé de terrain du monde de construction où l'on entre, donnée par le relais
    /// (`Event::WorldTerrain`, avant `Hosted` ou `Joined`) : elle n'est pas dans sa carte.
    world_terrain: Option<u64>,
    /// (v325) Réveil d'un monde par son créateur alors qu'un autre en garde le dernier état :
    /// le deuxième clic (même monde, peu après) le confirme.
    wake_confirm: Option<(String, f32)>,
    /// (v326) Nouveau monde à la limite : la confirmation demandée.
    limit_ask: Option<LimitAsk>,
    /// (v329) Liens directs demandés : la confirmation est à l'écran.
    direct_confirm: bool,
    fps_status: Option<String>,
    /// Mise à jour automatique (GitHub).
    updater: update::Updater,
    /// À l'image d'avant, on jouait (écran de jeu).
    was_playing: bool,
    /// Recherche dans la liste des serveurs, et dans celle des mondes solo.
    server_search: multiplayer::ListSearch,
    world_search: multiplayer::ListSearch,
    /// (v311) Suppression du monde : le code de double authentification en cours de
    /// frappe, et s'il en faut un (le relais l'a dit).
    delete_code: String,
    delete_code_needed: bool,
    /// (v314) Version dont la sortie a déjà été dite dans le tchat du monde où l'on est.
    update_told: u32,
    /// Garant indépendant (au menu : sert ou vérifie la partie d'autres joueurs).
    garant: Option<garant::Garant>,
    /// Garant pendant qu'on joue sa propre partie FPS (deuxième connexion, son propre fil),
    /// et s'il a déjà été tenté pour cette partie.
    garant_worker: Option<garant_worker::GarantWorker>,
    garant_worker_tried: bool,
    /// (v147) La connexion de garant est prêtée à son pays (pas au relais du monde) ; tombée :
    /// prochain essai.
    garant_pays: bool,
    garant_retry: Option<std::time::Instant>,
    /// (v148) Le pays à qui elle est prêtée (province, taille, époque).
    garant_target: Option<([i32; 2], i32, u64)>,
    /// Disponibilité de garant annoncée au relais.
    garant_announced: Option<bool>,
    /// Un monde a été ouvert depuis le lancement (plus de mise à jour automatique sans
    /// demander : bouton sur l'accueil).
    played: bool,
    renderer: Option<Renderer>,
    paths: Paths,
    options: Options,
    /// Pack lu au démarrage, en attendant que le rendu soit prêt.
    startup_pack: Option<ResourcePack>,
    screen: Screen,
    /// Où revenir en quittant l'écran des packs.
    packs_return: Screen,
    /// Où revenir en quittant les paramètres.
    settings_return: Screen,
    /// (v231) Module d'amis (v233 : panneau de l'accueil et du menu Échap) : son état, un
    /// message (invitation envoyée, erreur) et depuis quand il est affiché.
    friends_ui: friends::FriendsUi,
    friends_note: Option<(String, f32)>,
    /// (v271) Ami qu'on vient de demander d'ajouter (panneau, `/ami`, lobby FPS), et quand :
    /// la réponse du relais (refus : pseudo inconnu ; ou la demande envoyée) est dite dans le
    /// panneau. Avant, un refus n'était dit nulle part hors d'un monde en ligne : rien ne se
    /// passait.
    friend_adding: Option<(String, f32)>,
    /// (v271) Pseudo proposé par le relais pour un ami inconnu (celui qui lui ressemble :
    /// « lside » pour « 1side ») : bouton « Ajouter » du panneau, tant que le message y est.
    friends_suggest: Option<String>,
    /// (v231) Partie (salle du relais) où l'on est : un ami qui y est déjà n'a ni « Rejoindre »
    /// ni « Inviter ».
    current_room: Option<u64>,
    /// Curseur de réglage tenu à la souris.
    slider_drag: Option<u32>,
    /// (v251) Paramètres graphiques : lignes défilées, et la barre tenue à la souris.
    graphics_scroll: usize,
    graphics_drag: bool,
    /// Écran Son : sorties et micros du système (lus en l'ouvrant), défilement et barre
    /// tenue de chaque liste.
    sound_devices: (Vec<String>, Vec<String>),
    /// (v215) Relevé des sorties et micros en arrière-plan, et quand il a été demandé.
    sound_scan: DeviceScan,
    sound_scan_at: f32,
    /// (v215) Fréquence de l'écran où est la fenêtre (Hz), relue toutes les 3 s.
    refresh_hz: Option<f32>,
    /// (v215) Paramètres graphiques → Lumières hors d'une partie : les réglages relus de
    /// `.minelib/lumieres.txt` (en jeu, ceux de la partie), et leur fenêtre.
    menu_lights: Option<lights::Lights>,
    menu_light_editor: light_editor::Editor,
    sound_scroll: [usize; 2],
    sound_drag: [bool; 2],
    /// Distance de rendu permise par la mémoire vive (mesurée au lancement).
    ram_view_limit: i32,
    /// Plein écran réellement appliqué à la fenêtre.
    applied_fullscreen: Option<bool>,
    /// Écran branché sur la carte qui dessine (pas de recopie Optimus) : le jeu s'y place.
    /// (dernière vérification, nom de l'écran où le jeu a été placé)
    direct_display: (Option<std::time::Instant>, Option<String>),
    /// Champ texte de la sensibilité (en cours de saisie ou non).
    sens_text: String,
    sens_focus: bool,
    /// Touches du jeu, et action en attente d'une nouvelle touche.
    keys: keys::Bindings,
    binding: Option<keys::Action>,
    /// Menu des touches : onglet FPS (sinon Build) et lignes défilées.
    keys_tab_fps: bool,
    keys_scroll: usize,
    /// Barre de défilement du menu des touches tenue à la souris.
    keys_drag: bool,
    session: Option<Session>,
    config: SessionConfig,
    ui: Ui,
    sysmon: SysMonitor,
    time: f32,
    frame_times: VecDeque<f32>,
    last_stats: FrameStats,
    title_timer: f32,
    exit_after: Option<f32>,
    /// Images dessinées depuis le lancement (la fenêtre apparaît après les premières).
    frames_drawn: u32,
    /// Lancement du jeu.
    started: std::time::Instant,
    /// Capture automatique à ce temps (essais).
    shot_at: Option<f32>,
    /// (v251) Dernière capture (en mémoire, le PNG s'écrit en tâche de fond) et le moment où
    /// elle a été prise : Ctrl+C la copie (en jeu, peu après : `SHOT_COPY_S`).
    last_shot: Option<LastShot>,
    show_debug: bool,
    fps_max: Option<f32>,
    frame_start: Instant,
    // Écran Solo
    worlds: Vec<WorldSummary>,
    selected_world: Option<usize>,
    world_scroll: usize,
    last_row_click: (Option<usize>, f32),
    // Nouveau monde
    new_name: String,
    new_seed: String,
    focus: Field,
    // Packs
    packs: Vec<PackInfo>,
    /// Pack actif, et pack demandé (appliqué en fin d'image, quand le rendu est libre).
    current_pack: String,
    pack_request: Option<PackInfo>,
    message: Option<(String, f32)>,
    /// Écran branché sur une autre carte que celle qui dessine (Optimus) : son nom.
    optimus: Option<String>,
    /// (v228) Partie graphique intégrée voulue au lancement (`Options::igpu`) : le choix
    /// changé depuis ne compte qu'au prochain lancement.
    igpu_at_start: bool,
    /// Shaders des lumières à lancer dès que possible (lumières utilisées ces derniers
    /// jours).
    warm_lights: bool,
    /// Mémoire vidéo utilisée et budget Windows, relus chaque seconde.
    vram: Option<(minelib_platform::VramMonitor, std::time::Instant, bool)>,
    auto_world: Option<String>,
    /// Essais (`MINELIB_EN_LIGNE=1`) : monde à ouvrir en ligne dès que le compte est connecté.
    auto_online: Option<PathBuf>,
    /// (v318) Essais (`MINELIB_REJOINDRE`) : serveur de construction à rejoindre (un morceau
    /// de son nom), et quand sa liste a été demandée.
    auto_join: Option<String>,
    auto_join_asked: f32,
    pending_open: Option<(PathBuf, u32)>,
    /// (v309) La partie doit repartir d'une origine plus proche du joueur (`Session::
    /// wants_rebase`) : faite en fin d'image (`rebase_world`).
    rebase: bool,
    /// (v322) L'origine imposée à la prochaine ouverture (`SessionConfig::origin`) : celle
    /// d'une partie FPS (`Session::take_rebase_origin`).
    next_origin: Option<minelib_world::ChunkPos>,
    /// Vider tous les maillages GPU (changement de monde) en fin d'image.
    clear_meshes: bool,
    quit: bool,
}

impl Game {
    fn new() -> anyhow::Result<Self> {
        let paths = minelib_assets::init()?;
        // (v325) Copies en ligne sans monde, ou à moitié faites.
        online_copies::tidy(&paths.root);
        let keys = keys::Bindings::load(&paths.root.join("keys.txt"));
        let options = Options::load(&paths.options);
        minelib_p2p::set_hosts_direct(options.host_direct);
        // (v329) Les nœuds du jeu prennent une adresse chez un serveur TURN
        // (`relais_turn.txt`) : un chemin de plus pour les tunnels, sans passeur.
        minelib_p2p::enable_turn();
        account::set_invitation(&options.invitation);
        // (v307) Journal réseau : partagé si le joueur l'a accepté ; reçu si ce PC, quand
        // il est le relais, l'a demandé (paramètres avancés).
        minelib_net::netlog::set_share(options.share_netlog == Some(true));
        minelib_relay::set_receive_logs(options.receive_netlogs);
        // (v308) Passeur : seulement si le joueur l'a accepté.
        minelib_net::fast::set_passeur_allowed(options.passeur == Some(true));
        jobs::set_uniform_sharing(options.cpu_uniform);
        crate::audio::set_output(&options.audio_out);
        crate::voice::set_input(&options.audio_in);
        crate::voice::set_mic_gain(options.volume_mic);
        crate::audio::set_master(options.volume);
        crate::audio::set_weather_volume(options.volume_weather);
        let packs = list_packs(&paths.resourcepacks);
        let pack = packs
            .iter()
            .find(|p| p.name == options.resource_pack)
            .and_then(|p| {
                ResourcePack::open(p)
                    .map_err(|e| log::error!("pack « {} » : {e:#}", p.name))
                    .ok()
            })
            .map_or_else(ResourcePack::builtin, Ok)?;
        let view_distance = env_parse("MINELIB_VIEW")
            .unwrap_or(options.render_distance)
            .clamp(2, session::MAX_VIEW_DISTANCE);
        let mut accounts = account::Accounts::new(
            options.relay.clone(),
            &options.relay_key,
            paths.root.join("relay"),
            options.pseudo.clone(),
        );
        // (v231) Invisible pour ses amis : redit au relais à chaque connexion.
        accounts.invisible = options.invisible;
        Ok(Self {
            accounts,
            after_login: Screen::Title,
            servers: Vec::new(),
            server_selected: None,
            fps_home: multiplayer::FpsHome::default(),
            friend_names: Vec::new(),
            my_skin: None,
            skin_sent: false,
            skin_pick: None,
            skin_msg: None,
            download_note: None,
            kick_pick: false,
            party_go: None,
            fps_home_maps_seen: u64::MAX,
            solo_maps: solo_maps::SoloMaps::default(),
            solo_maps_seen: u64::MAX,
            solo_map_pending: None,
            solo_rows: (Vec::new(), Vec::new()),
            shared_maps: Default::default(),
            reshared: Default::default(),
            maps_rev_seen: 0,
            maps_asked: false,
            map_rows: Vec::new(),
            add_rows: Vec::new(),
            map_adding: None,
            certif_asked: None,
            sig_cache: fpsmap::SigCache::default(),
            searching: None,
            pending_build: None,
            world_key: None,
            hosting: None,
            world_terrain: None,
            wake_confirm: None,
            limit_ask: None,
            direct_confirm: false,
            fps_status: None,
            updater: update::Updater::new(),
            was_playing: false,
            server_search: Default::default(),
            world_search: Default::default(),
            delete_code: String::new(),
            delete_code_needed: false,
            update_told: 0,
            garant: None,
            garant_worker: None,
            garant_worker_tried: false,
            garant_pays: false,
            garant_retry: None,
            garant_target: None,
            garant_announced: None,
            played: false,
            renderer: None,
            worlds: save::list_worlds(&paths.saves),
            paths,
            options,
            startup_pack: Some(pack),
            screen: Screen::Account(account::Page::Welcome),
            packs_return: Screen::Title,
            settings_return: Screen::Title,
            friends_ui: friends::FriendsUi::default(),
            friends_note: None,
            friend_adding: None,
            friends_suggest: None,
            current_room: None,
            slider_drag: None,
            graphics_scroll: 0,
            graphics_drag: false,
            sound_devices: (Vec::new(), Vec::new()),
            sound_scan: DeviceScan::started(),
            sound_scan_at: 0.0,
            refresh_hz: None,
            menu_lights: None,
            menu_light_editor: light_editor::Editor::default(),
            sound_scroll: [0; 2],
            sound_drag: [false; 2],
            ram_view_limit: session::ram_view_limit(),
            applied_fullscreen: None,
            direct_display: (None, None),
            sens_text: String::new(),
            sens_focus: false,
            keys,
            binding: None,
            keys_tab_fps: false,
            keys_scroll: 0,
            keys_drag: false,
            session: None,
            config: SessionConfig {
                view_distance,
                far_origin: true,
                origin: None,
                terrain: None,
            },
            ui: Ui::default(),
            sysmon: SysMonitor::start(),
            time: 0.0,
            frame_times: VecDeque::new(),
            last_stats: FrameStats::default(),
            title_timer: 0.0,
            exit_after: env_parse("MINELIB_EXIT_AFTER"),
            shot_at: env_parse("MINELIB_SHOT_AT"),
            last_shot: None,
            frames_drawn: 0,
            started: std::time::Instant::now(),
            show_debug: env_parse::<u8>("MINELIB_DEBUG").is_some_and(|v| v != 0),
            fps_max: env_parse::<f32>("MINELIB_FPS_MAX").filter(|&f| f > 0.0),
            frame_start: Instant::now(),
            selected_world: None,
            world_scroll: 0,
            last_row_click: (None, 0.0),
            new_name: String::new(),
            new_seed: String::new(),
            focus: Field::Name,
            packs,
            current_pack: String::new(),
            pack_request: None,
            message: None,
            auto_world: std::env::var("MINELIB_WORLD")
                .ok()
                .filter(|s| !s.trim().is_empty()),
            auto_online: None,
            auto_join: std::env::var("MINELIB_REJOINDRE")
                .ok()
                .filter(|s| !s.trim().is_empty()),
            auto_join_asked: -10.0,
            pending_open: None,
            rebase: false,
            next_origin: None,
            clear_meshes: false,
            quit: false,
            optimus: None,
            igpu_at_start: false,
            warm_lights: false,
            vram: None,
        })
        .map(|mut g| {
            minelib_render::set_translator(i18n::translate);
            i18n::set(&g.options.language);
            // Connexion au compte dès le démarrage, pendant que le lanceur est encore affiché
            // et que le jeu se charge (fenêtre cachée) : on arrive dans le jeu déjà connecté.
            g.accounts.poll();
            g.shared_maps = load_shared(&g.paths.root);
            // Partie FPS en ligne laissée en cours : on garde cette version pour la finir.
            g.updater.hold = update::match_lock(&g.paths.root);
            g.my_skin = std::fs::read(g.paths.root.join("skin.png"))
                .ok()
                .and_then(|b| skin::Skin::from_png(&b).ok());
            if std::env::var("MINELIB_UI").is_ok_and(|v| v == "settings") {
                g.screen = Screen::Settings;
            }
            // (v201) Paramètres graphiques ouverts au lancement (essais de l'interface).
            if std::env::var("MINELIB_UI").is_ok_and(|v| v == "graphics") {
                g.screen = Screen::Graphics;
            }
            // (v215) Paramètres graphiques → Lumières au lancement (essais de l'interface).
            if std::env::var("MINELIB_UI").is_ok_and(|v| v == "reglages_lumieres") {
                g.screen = Screen::Lights;
            }
            // Essais de l'interface : écran FPS avec un lobby d'exemple.
            if std::env::var("MINELIB_UI").is_ok_and(|v| v == "lobby") {
                g.screen = Screen::FpsHome;
                g.accounts.friends = vec![
                    ("Alice".into(), true),
                    ("Bastien_le_tres_long_pseudo".into(), true),
                    ("Chloé".into(), false),
                    ("Dimitri".into(), true),
                    ("Emma".into(), false),
                ];
                g.accounts.party = (7, vec!["Alice".into(), "Moi".into(), "Dimitri".into()]);
                g.accounts.invites = vec!["Zoé".into()];
                g.map_rows = vec![multiplayer::MapRow {
                    sig: [1; 32],
                    name: "Dust de blocs".into(),
                    author: "Alice".into(),
                    local: None,
                    waiting: 3,
                    plays: 12,
                    configured: true,
                    players: 10,
                    stored: true,
                    certified: false,
                }];
                // (v340) Une carte certifiée, et deux d'autres formats (« Équipe »).
                for (k, (name, players, plays, certified)) in [
                    ("Mirage de pierre", 10u8, 4u32, true),
                    ("Petit duel", 2, 7, false),
                    ("Arène 2v2", 4, 30, true),
                ]
                .into_iter()
                .enumerate()
                {
                    g.map_rows.push(multiplayer::MapRow {
                        sig: [k as u8 + 2; 32],
                        name: name.into(),
                        author: "Fox".into(),
                        local: None,
                        waiting: 0,
                        plays,
                        configured: true,
                        players,
                        stored: true,
                        certified,
                    });
                }
                multiplayer::sort_rows(&mut g.map_rows);
                // Lignes d'exemple gardées tant que le relais n'a pas donné sa liste.
                g.fps_home_maps_seen = g.accounts.maps_rev;
                if let Ok(n) = std::env::var("MINELIB_EQUIPE") {
                    g.fps_home.team = n.parse().unwrap_or(5);
                }
                g.fps_home.team_open = std::env::var("MINELIB_EQUIPE_LISTE").is_ok();
                if std::env::var("MINELIB_AJOUT").is_ok() {
                    g.fps_home.adding = true;
                    g.add_rows = vec![
                        multiplayer::AddRow {
                            dir: PathBuf::from("a"),
                            sig: [9; 32],
                            name: "Mon Dust".into(),
                            players: 10,
                            configured: true,
                            listed: false,
                        },
                        multiplayer::AddRow {
                            dir: PathBuf::from("b"),
                            sig: [10; 32],
                            name: "Arène 2v2".into(),
                            players: 4,
                            configured: true,
                            listed: true,
                        },
                        multiplayer::AddRow {
                            dir: PathBuf::from("c"),
                            sig: [11; 32],
                            name: "Nouveau monde 3".into(),
                            players: 10,
                            configured: false,
                            listed: false,
                        },
                    ];
                }
            }
            g
        })
    }

    /// Enregistre une capture dans `.minelib/screenshots` (en tâche de fond) et l'annonce
    /// dans le tchat avec un lien (clic, tchat ouvert : copie dans le presse-papier).
    fn save_screenshot(&mut self, shot: minelib_render::Screenshot) {
        let dir = self.paths.root.join("screenshots");
        let _ = std::fs::create_dir_all(&dir);
        let base = stamp();
        let mut path = dir.join(format!("{base}.png"));
        let mut n = 1;
        while path.exists() {
            n += 1;
            path = dir.join(format!("{base}_{n}.png"));
        }
        let name = file_name(&path);
        log::info!("capture d'écran : {}", path.display());
        let target = path.clone();
        let rgba = std::sync::Arc::new(shot.rgba);
        let pixels = rgba.clone();
        let (width, height) = (shot.width, shot.height);
        std::thread::spawn(move || {
            if let Err(e) = image::save_buffer(
                &target,
                &pixels,
                width,
                height,
                image::ExtendedColorType::Rgba8,
            ) {
                log::error!("capture d'écran {} : {e}", target.display());
            }
        });
        self.last_shot = Some(LastShot {
            path: path.clone(),
            rgba,
            width,
            height,
            at: self.time,
        });
        match &mut self.session {
            Some(s) => s.chat.say_link(
                format!("Capture enregistrée : {name} (Ctrl+C, ou T puis clic : copier)"),
                chat::YELLOW,
                path,
            ),
            None => self.notify(format!("Capture enregistrée : {name} (Ctrl+C : copier)")),
        }
    }

    /// (v251) « Ctrl+C dans le jeu pour copier rapidement mon screen après l'avoir pris, avec
    /// le même message que si on clique » : la dernière capture, prise il y a moins de
    /// `SHOT_COPY_S`, copiée dans le presse-papier (depuis la mémoire : son PNG est peut-être
    /// encore en train de s'écrire). (v262) Dans les menus et le tchat ouvert, à tout moment ;
    /// pas avec du texte sélectionné dans le tchat (Ctrl+C copie ce texte), ni en écrivant
    /// dans la page d'un écran, ni en réglant une touche. « Le C de ce Ctrl+C ne
    /// déclenche ni le zoom ni la radio ni rien qui pourrait être bind et interférer » : la
    /// touche C est ignorée par les réglages du jeu et de la partie jusqu'à ce qu'elle soit
    /// relâchée. Rend vrai si la capture a été copiée (ou a essayé).
    fn copy_last_shot(&mut self, input: &Input) -> bool {
        let ctrl = input.held(KeyCode::ControlLeft) || input.held(KeyCode::ControlRight);
        if !ctrl || !input.pressed(KeyCode::KeyC) || self.binding.is_some() {
            return false;
        }
        // (v262) « Je dois pouvoir faire Ctrl+C partout dans le jeu avec mon screen » : la
        // dernière capture, dans tous les écrans et menus, tchat ouvert compris (sauf du
        // texte sélectionné dans le tchat : c'est lui qu'on copie), à tout moment. En jeu
        // (curseur capturé), seulement dans la minute qui suit (`SHOT_COPY_S`) : Ctrl et C y
        // servent aussi (courir et zoomer, s'accroupir et la radio). Pas en écrivant dans la
        // page d'un écran (Ctrl+C y est à la page).
        let Some(shot) = self
            .last_shot
            .as_ref()
            .filter(|s| !input.grabbed || self.time - s.at < SHOT_COPY_S)
        else {
            return false;
        };
        if self
            .session
            .as_ref()
            .is_some_and(|s| s.chat.has_selection() || s.keyboard_on_screen())
        {
            return false;
        }
        let name = file_name(&shot.path);
        let (text, color) = match copy_rgba_to_clipboard(shot.width, shot.height, &shot.rgba) {
            Ok(()) => (
                format!("Capture copiée dans le presse-papier : {name}"),
                chat::GREY,
            ),
            Err(e) => {
                log::warn!("presse-papier : {e:#}");
                (format!("Copie impossible : {e}"), chat::RED)
            }
        };
        // Le C de Ctrl+C ne compte pour rien d'autre tant qu'il est tenu.
        self.keys.muted_key = Some(KeyCode::KeyC);
        match &mut self.session {
            Some(s) => {
                s.chat.say(text, color);
                s.keys.muted_key = Some(KeyCode::KeyC);
            }
            None => self.notify(text),
        }
        true
    }

    fn notify(&mut self, text: impl Into<String>) {
        self.message = Some((text.into(), 4.0));
    }

    fn refresh_worlds(&mut self) {
        self.worlds = save::list_worlds(&self.paths.saves);
        self.selected_world = None;
        self.world_scroll = 0;
    }

    fn play(&mut self, dir: PathBuf) {
        self.world_key = None;
        self.play_with(dir, true);
    }

    /// (v309) Ouvre le monde `dir` ; `far_origin` faux : partie FPS en ligne, tout est compté
    /// depuis l'origine de sa carte (v322 ; avant, depuis le centre du monde : voir
    /// `SessionConfig::far_origin`).
    fn play_with(&mut self, dir: PathBuf, far_origin: bool) {
        self.play_with_terrain(dir, far_origin, None);
    }

    /// (v330) `play_with`, avec la clé de terrain donnée par le relais d'un serveur de
    /// construction (sa copie du monde ne l'a pas).
    fn play_with_terrain(&mut self, dir: PathBuf, far_origin: bool, terrain: Option<u64>) {
        // Nouveau monde : rien de l'ancien ne doit rester à l'écran.
        self.session = None;
        self.played = true;
        // On joue : plus garant (le garant en attente ou un joueur reprend sans coupure).
        if self.garant.take().is_some() {
            self.accounts.room(minelib_net::client::Cmd::Leave);
        }
        self.announce_garant(false);
        // Le rendu est emprunté pendant l'image : on vide ses maillages en fin d'image.
        self.clear_meshes = true;
        let config = SessionConfig {
            far_origin,
            // (v322) Rouverte pour une partie FPS : depuis son origine (`rebase_world`).
            origin: self.next_origin.take(),
            terrain,
            ..self.config
        };
        match Session::open(dir, config) {
            Ok(mut s) => {
                s.voice_volumes = load_volumes(&self.paths.root.join("voice.txt"));
                s.sensitivity = self.options.sensitivity;
                s.sensitivity_fps = self.options.sensitivity_fps;
                s.fps_color = self.options.fps_color;
                s.skins.set_mine(self.my_skin.clone());
                s.pseudo = self.accounts.pseudo();
                s.volume = self.options.volume;
                s.voice_volume = self.options.volume_voice;
                s.set_screens_volume(self.options.volume_screens);
                s.keys = self.keys.clone();
                // Essais de l'interface : éditeur de lumières ouvert, première lumière choisie.
                if std::env::var("MINELIB_UI").is_ok_and(|v| v == "lumieres") {
                    s.light_editor.open = true;
                    s.light_editor.selected = (!s.lights.scene.is_empty()).then_some(0);
                }
                self.session = Some(s);
                self.screen = Screen::Playing;
            }
            Err(e) => self.notify(format!("Impossible d'ouvrir le monde : {e:#}")),
        }
    }

    /// (v233) Panneau des amis (accueil, menu Échap) : la liste redemandée au relais toutes
    /// les `friends::REFRESH_S` s tant qu'il est affiché.
    fn friends_refresh(&mut self) {
        if !self.accounts.logged_in() {
            self.friends_ui.asked = None;
            return;
        }
        if self
            .friends_ui
            .asked
            .is_none_or(|t| t.elapsed().as_secs_f32() >= friends::REFRESH_S)
        {
            self.friends_ui.asked = Some(std::time::Instant::now());
            self.accounts.room(minelib_net::client::Cmd::Friend {
                pseudo: String::new(),
                add: false,
            });
        }
    }

    /// (v233) Dessine le panneau des amis dans ce rectangle (après `friends_refresh`).
    #[allow(clippy::too_many_arguments)]
    fn friends_panel(
        c: &mut Ctx,
        input: &Input,
        st: &mut friends::FriendsUi,
        accounts: &account::Accounts,
        in_world: Option<u64>,
        note: Option<(&str, Option<&str>)>,
        rect: (f32, f32, f32, f32),
        time: f32,
    ) -> friends::FriendsAction {
        let view = friends::FriendsView {
            logged_in: accounts.logged_in(),
            friends: &accounts.friends,
            places: &accounts.friend_places,
            invites: &accounts.world_invites,
            requests: &accounts.friend_requests,
            invisible: accounts.invisible,
            in_world,
            note: note.map(|n| n.0),
            suggest: note.and_then(|n| n.1),
        };
        let (x, y, w, h) = rect;
        friends::panel(c, input, st, &view, x, y, w, h, time)
    }

    /// (v233) Monde de construction en ligne où l'on est (on peut y inviter), et le message
    /// du panneau des amis s'il est récent. (v271) Avec le pseudo proposé pour un ami inconnu
    /// (bouton « Ajouter ») : le message reste alors 30 s, le temps de le lire.
    fn friends_context(&self) -> (Option<u64>, Option<(String, Option<String>)>) {
        let in_world = self
            .session
            .as_ref()
            .filter(|s| s.build.is_some())
            .and(self.current_room);
        let keep = if self.friends_suggest.is_some() { 30.0 } else { 6.0 };
        let note = self
            .friends_note
            .as_ref()
            .filter(|(_, t)| self.time - t < keep)
            .map(|(n, _)| (n.clone(), self.friends_suggest.clone()));
        (in_world, note)
    }

    /// (v231) Ce qui a été demandé dans le panneau des amis : rejoindre un monde (en quittant
    /// celui où l'on est), inviter, ajouter, accepter ou refuser une demande, retirer, devenir
    /// invisible, se connecter.
    fn friends_act(&mut self, action: friends::FriendsAction) {
        use minelib_net::client::Cmd;
        match action {
            friends::FriendsAction::None => {}
            friends::FriendsAction::Login => {
                self.after_login = self.screen;
                self.goto_account(account::Page::Welcome);
            }
            friends::FriendsAction::Add(p) => {
                // Accepter une demande, c'est l'ajouter aussi.
                self.accounts
                    .friend_requests
                    .retain(|r| !r.eq_ignore_ascii_case(&p));
                // (v271) Sa réponse sera dite dans le panneau (voir `friend_adding`).
                self.friend_adding = Some((p.clone(), self.time));
                self.friends_suggest = None;
                self.accounts.room(Cmd::Friend { pseudo: p, add: true });
            }
            friends::FriendsAction::Decline(p) => {
                self.accounts
                    .friend_requests
                    .retain(|r| !r.eq_ignore_ascii_case(&p));
                self.accounts.room(Cmd::FriendDecline(p));
            }
            friends::FriendsAction::Remove(p) => {
                self.accounts.room(Cmd::Friend { pseudo: p, add: false });
            }
            friends::FriendsAction::Invite(p) => self.accounts.room(Cmd::Invite(p)),
            friends::FriendsAction::Invisible(on) => {
                self.accounts.invisible = on;
                self.options.invisible = on;
                if let Err(e) = self.options.save(&self.paths.options) {
                    log::error!("options : {e:#}");
                }
                self.accounts.room(Cmd::Presence(on));
            }
            // (v242) Accepter (on rejoint son monde), refuser (il le sait), ignorer (il n'en
            // sait rien) : le relais la retire.
            friends::FriendsAction::Answer(i, reply) => {
                if i >= self.accounts.world_invites.len() {
                    return;
                }
                let inv = self.accounts.world_invites.remove(i);
                self.accounts.room(Cmd::InviteReply {
                    from: inv.from.clone(),
                    reply,
                });
                match reply {
                    minelib_net::proto::INVITE_ACCEPT if inv.open => {
                        self.friends_act(friends::FriendsAction::Join(inv.room));
                    }
                    minelib_net::proto::INVITE_DECLINE => {
                        self.friends_note =
                            Some((format!("Invitation de {} refusée.", inv.from), self.time));
                    }
                    _ => {}
                }
            }
            friends::FriendsAction::Join(room) => {
                // (v242) Les invitations dans ce monde sont acceptées (le relais les retire).
                let (done, keep): (Vec<_>, Vec<_>) = std::mem::take(&mut self.accounts.world_invites)
                    .into_iter()
                    .partition(|i| i.open && i.room == room);
                self.accounts.world_invites = keep;
                for inv in done {
                    self.accounts.room(Cmd::InviteReply {
                        from: inv.from,
                        reply: minelib_net::proto::INVITE_ACCEPT,
                    });
                }
                // Dans un monde : on le quitte d'abord (enregistré s'il est à nous).
                if self.session.is_some() {
                    self.leave_world();
                }
                self.accounts.room(Cmd::Join(room));
                self.friends_note = Some(("Connexion au monde…".into(), self.time));
            }
        }
    }

    /// (v309) « Aucun problème jusqu'à 16,7 millions » : le joueur s'est éloigné de l'origine
    /// de sa partie (`Session::wants_rebase`) ; le monde est enregistré et rouvert, compté
    /// depuis une origine proche de lui (`session::origin_near`). L'écran de chargement le temps
    /// de recharger autour de lui ; le tchat et sa vitesse suivent.
    fn rebase_world(&mut self) {
        let Some(mut s) = self.session.take() else {
            return;
        };
        if let Err(e) = s.save() {
            log::error!("sauvegarde impossible avant de changer d'origine : {e:#}");
            self.session = Some(s);
            return;
        }
        let dir = s.dir.clone();
        let place = s.exact_place();
        // (v322) Une partie FPS demande son origine (celle de sa carte, ou du jeu de l'hôte de
        // la partie FPS du serveur) : la partie rouverte compte depuis elle.
        self.next_origin = s.take_rebase_origin();
        // (v318) Le tchat, la vitesse, et en ligne la connexion au serveur et ce qu'on en
        // sait : la partie rouverte les reprend (on ne quitte pas le serveur).
        let carry = s.take_carry();
        let online = carry.online();
        // (v330) La clé de terrain que le relais avait donnée (pas dans la copie du monde).
        let terrain = s.terrain_given;
        drop(s);
        log::info!(
            "loin de l'origine de la partie : rouverte depuis une origine plus proche (joueur en {:.1} / {:.1} / {:.1}){}",
            place[0],
            place[1],
            place[2],
            if online { ", sans quitter le serveur" } else { "" }
        );
        self.play_with_terrain(dir, true, terrain);
        if let Some(n) = &mut self.session {
            n.put_carry(carry);
            // (v322) Rouverte pour la partie FPS du serveur : on y entre.
            n.after_rebase();
        }
    }

    /// Enregistre et ferme le monde en cours.
    fn leave_world(&mut self) {
        self.current_room = None;
        if let Some(mut s) = self.session.take() {
            // Partie en ligne : on la quitte ; un invité n'enregistre pas la carte de l'hôte.
            // Partie FPS en ligne : la carte ne change pas (sa signature non plus).
            // (La partie solo contre des bots n'est pas en ligne : le monde s'enregistre.)
            // (v243) La partie FPS d'un serveur de construction non plus : son monde à soi
            // s'enregistre.
            let online = s.ranked_match();
            let guest_build = s.build.as_ref().is_some_and(|b| !b.is_owner());
            if online || s.build.is_some() {
                self.accounts.room(minelib_net::client::Cmd::Leave);
            }
            self.accounts.forget_standby();
            s.flush_reports();
            if !online && !guest_build {
                if let Err(e) = s.save() {
                    log::error!("sauvegarde impossible : {e:#}");
                }
                if s.build.is_some() {
                    // (Fermé d'abord : sa copie peut changer de dossier.)
                    let dir = s.dir.clone();
                    drop(s);
                    self.keep_online_copy(&dir);
                }
            }
        }
        self.world_key = None;
        self.hosting = None;
        // Le rendu est emprunté pendant l'image : on vide ses maillages en fin d'image.
        self.clear_meshes = true;
        self.refresh_worlds();
        self.screen = Screen::Title;
    }

    fn select_pack(&mut self, renderer: &mut Renderer, info: &PackInfo) {
        match ResourcePack::open(info) {
            Ok(p) => {
                renderer.load_pack(&p);
                self.current_pack = p.name.clone();
                self.options.resource_pack = info.name.clone();
                if let Err(e) = self.options.save(&self.paths.options) {
                    log::error!("options : {e:#}");
                }
            }
            Err(e) => self.notify(format!("Pack illisible : {e:#}")),
        }
    }

    fn record_frame_time(&mut self, dt: f32) {
        self.frame_times.push_back(dt);
        let mut total: f32 = self.frame_times.iter().sum();
        while total > 1.0 && self.frame_times.len() > 1 {
            total -= self.frame_times.pop_front().unwrap_or(0.0);
        }
    }

    /// Limite d'images par seconde : variable MINELIB_FPS_MAX, sinon le réglage.
    fn fps_limit(&self) -> Option<f32> {
        self.fps_max
            .or((self.options.fps_max > 0).then_some(self.options.fps_max as f32))
    }

    fn limit_fps(&mut self) {
        // (v215) À l'accueil et dans le menu Échap (et tout ce qu'on ouvre depuis lui) : pas
        // plus d'images par seconde que l'écran n'en affiche (avant : 120 hors du jeu, et
        // aucune limite dans le menu Échap). 120 si l'écran ne dit pas sa fréquence.
        let menu = self.session.is_none() || self.screen != Screen::Playing;
        let max = if menu {
            let screen = self.refresh_hz.filter(|h| *h >= 30.0).unwrap_or(120.0);
            Some(self.fps_limit().map_or(screen, |l| l.min(screen)))
        } else {
            self.fps_limit()
        };
        if let Some(max) = max {
            let _s = minelib_render::perf::scope("limiteur de FPS (attente voulue)");
            let target = Duration::from_secs_f32(1.0 / max);
            loop {
                let elapsed = self.frame_start.elapsed();
                if elapsed >= target {
                    break;
                }
                let left = target - elapsed;
                if left > Duration::from_millis(2) {
                    std::thread::sleep(left - Duration::from_millis(1));
                } else {
                    std::hint::spin_loop();
                }
            }
        }
        self.frame_start = Instant::now();
    }

    // ------------------------------------------------------------ écrans

    fn title_screen(&mut self, input: &Input, size: (u32, u32)) {
        self.friends_refresh();
        let (in_world, note) = self.friends_context();
        let time = self.time;
        let mut c = Ctx::new(&mut self.ui, input, size);
        c.background();
        let s = c.s;
        // (v233) Module d'amis à droite (comme celui du lobby FPS), sous le bouton du compte ;
        // le menu reste au milieu tant qu'il a la place.
        let pw = friends::panel_width(&c);
        let px = c.w - pw - 6.0 * s;
        let rect = (px, 30.0 * s, pw, c.h - 48.0 * s);
        let friends_action = Self::friends_panel(
            &mut c,
            input,
            &mut self.friends_ui,
            &self.accounts,
            in_world,
            note.as_ref().map(|(n, p)| (n.as_str(), p.as_deref())),
            rect,
            time,
        );
        let cx = (c.w / 2.0).min(px - 112.0 * s);
        let logo = s * 4.0;
        for (t, y, scale, color) in [
            ("MINELIB", c.h * 0.18, logo, GOLD),
            ("moteur voxel en Rust", c.h * 0.18 + 36.0 * s, s, GREY),
        ] {
            let tw = Ui::text_width(t, scale);
            c.ui.text(cx - tw / 2.0, y, scale, color, t);
        }
        let mut y = c.h * 0.42;
        let step = 24.0 * s;
        let solo = c.button("Solo", cx, y, 200.0, true);
        y += step;
        let multi = c.button("Multijoueur", cx, y, 200.0, true);
        // Compte, en haut à droite : pseudo connecté ou invitation à se connecter.
        let account_label = match &self.accounts.account {
            Some(a) => format!("Compte : {}", a.pseudo),
            None => "Hors ligne - Se connecter".to_string(),
        };
        let aw = (Ui::text_width(&account_label, s) / s + 16.0).max(100.0);
        let account = c.button(
            &account_label,
            c.w - (aw / 2.0 + 6.0) * s,
            6.0 * s,
            aw,
            true,
        );
        y += step;
        let settings = c.button("Paramètres", cx, y, 200.0, true);
        y += step * 1.5;
        let quit = c.button("Quitter le jeu", cx, y, 200.0, true);
        // (v319) Le bas du dernier bouton : les questions d'en bas à gauche restent dessous.
        let buttons_bottom = y + 20.0 * s;
        let ds = (s / 2.0).floor().max(1.0);
        // Version et état (à jour, mise à jour disponible…), bien lisible.
        let (line, color) = self.updater.line();
        c.ui.text(4.0 * s, c.h - 12.0 * s, s, color, line);
        // Nouvelle version publiée : bouton tout en bas à gauche, au-dessus de la version
        // (pas pendant une partie en cours dans cette version).
        let mut do_update = false;
        if let (Some(r), None) = (&self.updater.available, self.updater.hold) {
            let label = format!("Mettre à jour (v{})", r.version);
            do_update = c.button(
                &label,
                4.0 * s + 70.0 * s,
                c.h - 38.0 * s,
                140.0,
                !self.updater.busy(),
            );
        }
        if let Some(t) = &self.updater.status {
            c.ui.text(4.0 * s, c.h - 50.0 * s, ds, update::ORANGE, t.clone());
        }
        // (v319) Les deux questions qui suivent (une seule fois chacune) : en bas à gauche,
        // entre le dernier bouton et la version (« plus bas le message à gauche » : à 110 fois
        // l'échelle du bas, le titre passait sur « Paramètres » dès que la fenêtre fait moins
        // de 376 fois l'échelle de haut, 900 pixels compris), ses lignes espacées. Pas pendant
        // qu'une mise à jour est proposée : son bouton est à cette place (la question revient
        // après).
        let ask = self.updater.available.is_none() && self.updater.status.is_none();
        let question = |c: &mut Ctx<'_>, title: &str, lines: &[&str]| -> (bool, bool) {
            let lines_y = 11.0 * s;
            let buttons_y = lines_y + lines.len() as f32 * 10.0 * ds + 6.0 * s;
            let height = buttons_y + 20.0 * s;
            let (top, bottom) = (buttons_bottom, c.h - 12.0 * s);
            // Au pixel près : un texte posé entre deux pixels prend un trait de la lettre
            // voisine dans l'image des lettres.
            let y0 = if bottom - top >= height {
                top + (bottom - top - height) / 2.0
            } else {
                bottom - height - 2.0 * s
            }
            .round();
            c.ui.text(4.0 * s, y0, s, WHITE, title);
            for (i, line) in lines.iter().enumerate() {
                c.ui
                    .text(4.0 * s, y0 + lines_y + i as f32 * 10.0 * ds, ds, GREY, *line);
            }
            let yes = c.button("Oui", 4.0 * s + 33.0 * s, y0 + buttons_y, 66.0, true);
            let no = c.button("Non", 4.0 * s + 105.0 * s, y0 + buttons_y, 66.0, true);
            (yes, no)
        };
        // (v307) Une seule fois, en bas à gauche : partager son journal réseau ? La
        // réponse se change ensuite dans Paramètres → Paramètres avancés.
        let mut share_answer = None;
        if ask && self.options.share_netlog.is_none() {
            let (yes, no) = question(
                &mut c,
                "Aider à corriger le réseau du jeu ?",
                &[
                    "Partager des lignes sur l'état de ton réseau avec le relais :",
                    "ni pseudo, ni adresse. Se change dans Paramètres avancés.",
                ],
            );
            if yes {
                share_answer = Some(true);
            }
            if no {
                share_answer = Some(false);
            }
        }
        // (v308) Puis, une seule fois aussi : servir de passeur ? C'est un échange : ton
        // jeu porte les paquets chiffrés d'autres joueurs, les leurs portent les tiens, et
        // personne ne voit l'adresse de celui à qui il parle. Mais celui à qui un passeur
        // remet un paquet voit l'adresse du passeur : on ne le devient pas sans l'accepter.
        let mut passeur_answer = None;
        if ask && self.options.share_netlog.is_some() && self.options.passeur.is_none() {
            let (yes, no) = question(
                &mut c,
                "Relayer pour les autres joueurs ?",
                &[
                    "Ton jeu fait passer des paquets scellés d'autres parties, sans les lire.",
                    "C'est ce qui donne le 256 tick en FPS (sans assez de relais : 128).",
                    "Ceux à qui ton jeu remet un paquet voient ton adresse IP, pas ton pseudo.",
                    "Qui relaie est servi en premier. Se change dans Paramètres avancés.",
                ],
            );
            if yes {
                passeur_answer = Some(true);
            }
            if no {
                passeur_answer = Some(false);
            }
        }
        let credit = "Textures : VoxelArt Libre (CC BY 4.0 / CC0) - voir CREDITS.txt";
        let tw = Ui::text_width(credit, ds);
        c.ui.text(
            c.w - tw - 4.0 * s,
            c.h - 10.0 * ds - 2.0 * s,
            ds,
            GREY,
            credit,
        );
        if let Some(yes) = share_answer {
            self.options.share_netlog = Some(yes);
            minelib_net::netlog::set_share(yes);
            if let Err(e) = self.options.save(&self.paths.options) {
                log::error!("options : {e:#}");
            }
        }
        if let Some(yes) = passeur_answer {
            self.options.passeur = Some(yes);
            minelib_net::fast::set_passeur_allowed(yes);
            if let Err(e) = self.options.save(&self.paths.options) {
                log::error!("options : {e:#}");
            }
        }
        if solo {
            self.refresh_worlds();
            self.screen = Screen::Solo;
        }
        if multi {
            if self.accounts.logged_in() {
                self.screen = Screen::Multiplayer;
            } else {
                self.after_login = Screen::Multiplayer;
                self.goto_account(account::Page::Welcome);
            }
        }
        self.friends_act(friends_action);
        if account {
            if self.accounts.logged_in() {
                self.after_login = Screen::Title;
                self.goto_account(account::Page::Profile);
            } else {
                self.after_login = Screen::Title;
                self.goto_account(account::Page::Welcome);
            }
        }
        if settings {
            self.settings_return = Screen::Title;
            self.screen = Screen::Settings;
        }
        if do_update {
            self.updater.start();
        }
        if quit {
            self.quit = true;
        }
    }

    /// Choix de la carte (un monde de `saves`) pour héberger une partie FPS.
    fn host_pick_screen(&mut self, input: &Input, size: (u32, u32)) {
        let mut c = Ctx::new(&mut self.ui, input, size);
        c.background();
        let s = c.s;
        c.text_center(
            "Ouvrir un monde en construction en ligne",
            12.0 * s,
            s * 2.0,
            WHITE,
        );
        let list_w = 280.0 * s;
        let list_x = (c.w - list_w) / 2.0;
        let list_y = 36.0 * s;
        let row_h = 24.0 * s;
        let list_h = (c.h - 64.0 * s - list_y).max(row_h);
        c.frame(list_x, list_y, list_w, list_h);
        let ds = (s / 2.0).floor().max(1.0);
        c.text_center(
            "Le monde est envoyé au relais : les joueurs le téléchargent en rejoignant.",
            list_y + list_h + 4.0 * s,
            ds,
            GREY,
        );
        // (v325) Chaque ouverture : un nouveau serveur, sur une copie à part.
        c.text_center(
            "Un nouveau serveur, sur une copie à part : ton monde solo ne change pas.",
            list_y + list_h + 4.0 * s + 10.0 * ds,
            ds,
            GREY,
        );
        let visible = ((list_h - 4.0 * s) / row_h).floor() as usize;
        let mut chosen = None;
        for (i, w) in self
            .worlds
            .iter()
            .filter(|w| !w.minecraft)
            .take(visible)
            .enumerate()
        {
            let y = list_y + 2.0 * s + i as f32 * row_h;
            let detail = format!("graine {}", w.meta.seed_label());
            if c.list_row(
                list_x + 2.0 * s,
                y,
                list_w - 4.0 * s,
                row_h - s,
                &w.meta.name,
                &detail,
                false,
            ) {
                chosen = Some(w.dir.clone());
            }
        }
        let back = c.button("Retour", c.w / 2.0, c.h - 28.0 * s, 150.0, true);
        if let Some(dir) = chosen {
            self.host_build(dir);
        } else if back || input.pressed(KeyCode::Escape) {
            self.screen = Screen::Servers(multiplayer::Kind::Build);
        }
    }

    fn goto_account(&mut self, page: account::Page) {
        self.accounts.enter(page);
        self.screen = Screen::Account(page);
    }

    fn account_screen(&mut self, input: &Input, size: (u32, u32), page: account::Page) {
        let time = self.time;
        let mut c = Ctx::new(&mut self.ui, input, size);
        match self.accounts.screen(&mut c, input, page, time) {
            account::Outcome::None => {}
            account::Outcome::Goto(p) => self.goto_account(p),
            account::Outcome::Leave => {
                // Hors ligne : seulement le solo.
                let target = if self.accounts.logged_in() {
                    self.after_login
                } else {
                    Screen::Title
                };
                self.after_login = Screen::Title;
                self.screen = target;
                if let Screen::Servers(kind) = target {
                    self.accounts.refresh_servers(kind as u8);
                }
            }
        }
    }

    /// Événements de partie en ligne reçus du relais.
    fn handle_game_events(&mut self) {
        use minelib_net::client::{Cmd, Event};
        // (v231) Demandes d'ami pas encore annoncées (reçues en jeu, ou hors ligne).
        let new = std::mem::take(&mut self.accounts.new_requests);
        if !new.is_empty() {
            let t = match (new.as_slice(), self.session.is_some()) {
                ([one], true) => format!("{one} veut être ton ami (Échap : Accepter)."),
                ([one], false) => format!("{one} veut être ton ami."),
                (more, true) => format!("{} demandes d'ami (Échap).", more.len()),
                (more, false) => format!("{} demandes d'ami.", more.len()),
            };
            if let Some(s) = &mut self.session {
                s.chat.say(t.clone(), chat::YELLOW);
            }
            self.notify(t);
        }
        for e in std::mem::take(&mut self.accounts.game_events) {
            // Garant indépendant (au menu) : ses événements de partie.
            if self.session.is_none() && self.garant.is_some() {
                match &e {
                    Event::PeerJoined { .. }
                    | Event::PeerLeft(_)
                    | Event::FromPeer { .. }
                    | Event::Witness { .. }
                    | Event::FromHost(_)
                    | Event::Fast(_)
                    | Event::Promoted { .. }
                    | Event::HostChanged(_)
                    | Event::BackupIs(_) => {
                        if let Some(g) = &mut self.garant {
                            g.event(&e);
                        }
                        continue;
                    }
                    Event::Evidence {
                        case,
                        part,
                        of,
                        data,
                    } => {
                        if let Some(g) = &mut self.garant {
                            g.evidence(*case, *part, *of, data);
                        }
                        continue;
                    }
                    Event::RoomClosed => {
                        self.garant = None;
                        continue;
                    }
                    _ => {}
                }
            }
            // Troisième garant : arbitre entre un vérificateur et un hôte (au menu seulement ;
            // sinon le relais fera sans lui).
            if let Event::Arbitrate { case, map_sig, map } = &e {
                if self.session.is_none() && self.searching.is_none() && self.garant.is_none() {
                    if let Some(link) = self.accounts.link.clone() {
                        let dir = self.local_map(map_sig);
                        if dir.is_none() {
                            self.accounts.room(Cmd::DownloadMap(*map_sig));
                        }
                        self.garant = Some(garant::Garant::arbiter(
                            *case,
                            *map_sig,
                            map.clone(),
                            link,
                            dir,
                        ));
                    }
                }
                continue;
            }
            if let Event::GarantTask {
                room,
                map_sig,
                map,
                members,
                standby,
            } = &e
            {
                if self.session.is_some() || self.searching.is_some() {
                    self.accounts.room(Cmd::Leave);
                    continue;
                }
                let Some(link) = self.accounts.link.clone() else {
                    continue;
                };
                let dir = self.local_map(map_sig);
                if dir.is_none() {
                    self.accounts.room(Cmd::DownloadMap(*map_sig));
                }
                self.garant = Some(garant::Garant::new(
                    *room,
                    *map_sig,
                    map.clone(),
                    members.clone(),
                    *standby,
                    link,
                    self.accounts.pseudo(),
                    dir,
                ));
                continue;
            }
            match &e {
                // (v330) La clé de terrain du monde où l'on entre (pas dans sa carte).
                Event::WorldTerrain(t) => self.world_terrain = (*t != 0).then_some(*t),
                // (v325) Le monde de construction où l'on entre : celui qu'on ouvre ou réveille
                // est gardé, dès maintenant, par la copie où l'on joue.
                Event::World(key) => {
                    let relay = self.accounts.relay_hex();
                    if let Some(h) = &self.hosting {
                        online_copies::keep(&self.paths.root, &relay, key, &h.dir);
                    }
                    self.world_key = Some((relay, key.clone()));
                }
                // (v325) Monde pas ouvert (ou pas réveillé) : on n'y reste pas ; une copie
                // toute neuve, où l'on n'a rien fait, est oubliée.
                Event::HostRefused(m) => {
                    self.limit_ask = None;
                    if let Some(h) = self.hosting.take() {
                        if self.session.as_ref().is_some_and(|s| s.dir == h.dir) {
                            self.session = None;
                            self.clear_meshes = true;
                            self.current_room = None;
                            self.world_key = None;
                            self.screen = Screen::Servers(multiplayer::Kind::Build);
                            self.accounts.refresh_servers(multiplayer::Kind::Build as u8);
                        }
                        if h.fresh && online_copies::is_copy(&self.paths.root, &h.dir) {
                            let _ = std::fs::remove_dir_all(&h.dir);
                        }
                    }
                    self.notify(m.clone());
                }
                // (v326) Ce nouveau monde ne s'ouvre qu'en supprimant le plus ancien endormi :
                // on n'y reste pas, la copie attend la confirmation du joueur.
                Event::WorldLimit { world, name, totp } => {
                    if let Some(h) = self.hosting.take() {
                        if self.session.as_ref().is_some_and(|s| s.dir == h.dir) {
                            self.session = None;
                            self.clear_meshes = true;
                            self.current_room = None;
                            self.world_key = None;
                        }
                        let again = self.limit_ask.take();
                        let wrong = again
                            .as_ref()
                            .is_some_and(|a| a.sent_code && a.world == *world);
                        self.limit_ask = Some(LimitAsk {
                            dir: h.dir,
                            fresh: h.fresh,
                            map: h.upload.1,
                            world: world.clone(),
                            name: name.clone(),
                            totp: *totp,
                            code: String::new(),
                            wrong,
                            sent_code: false,
                        });
                        self.screen = Screen::WorldLimit;
                    }
                }
                Event::Hosted(room) => {
                    self.current_room = Some(*room);
                    // (v326) Le monde supprimé pour faire place : sa copie sur ce PC aussi.
                    if let Some(old) = self.hosting.as_ref().and_then(|h| h.replaced.clone()) {
                        online_copies::forget(&self.paths.root, &self.accounts.relay_hex(), &old);
                    }
                    self.limit_ask = None;
                    // (v325) Monde ouvert (ou réveillé) : sa météo (celle qu'un OP y a
                    // imposée, gardée par le relais), et le monde pour ceux qui le rejoignent.
                    let mut hosting = self.hosting.take();
                    // (v330) Réveillé depuis une copie sans clé de terrain : la partie s'ouvre
                    // maintenant, avec celle du relais ; s'il ne l'a pas, on n'y reste pas.
                    if let Some(h) = hosting.as_ref().filter(|h| h.deferred) {
                        let link = self.accounts.link.clone();
                        if let (Some(t), Some(link)) = (self.world_terrain, link) {
                            self.play_with_terrain(h.dir.clone(), true, Some(t));
                            self.attach_host_net(link);
                        }
                        if self.session.is_none() {
                            hosting = None;
                            self.current_room = None;
                            self.world_key = None;
                            self.accounts.room(Cmd::Leave);
                            self.notify(
                                "Le serveur ne connaît pas la graine de ce monde : son créateur \
                                 doit le réveiller."
                                    .to_string(),
                            );
                        }
                    }
                    if let Some(h) = hosting {
                        self.accounts.room(Cmd::ServerAdmin {
                            action: minelib_net::proto::SERVER_WEATHER,
                            pseudo: String::new(),
                            reason: String::new(),
                        });
                        // (v328) Et ce qui vaut pour sa graine (son créateur la voit toujours).
                        self.accounts.room(Cmd::ServerAdmin {
                            action: minelib_net::proto::SERVER_SEED,
                            pseudo: String::new(),
                            reason: String::new(),
                        });
                        let (sig, name, bytes) = h.upload;
                        self.accounts.room(Cmd::UploadMap {
                            sig,
                            name,
                            bytes,
                            fps: false,
                        });
                    }
                    if let Some((sig, name, Some(dir))) = self.searching.take() {
                        self.fps_status = None;
                        self.mark_shared(sig);
                        self.host_world(dir.clone(), sig);
                        // La carte part au relais (s'il ne l'a pas) pour les suivants.
                        if let Ok(p) = fpsmap::package(&dir) {
                            self.accounts.room(Cmd::UploadMap {
                                sig,
                                name,
                                bytes: p.to_bytes(),
                                fps: true,
                            });
                        }
                        log::info!("partie FPS {room} ouverte, en attente de joueurs");
                    } else if let Some(s) = &mut self.session {
                        s.chat.say(
                            format!("Monde ouvert en ligne (partie n° {room})."),
                            chat::YELLOW,
                        );
                    }
                }
                Event::Joined {
                    map,
                    host,
                    map_sig,
                    room,
                } => {
                    self.current_room = Some(*room);
                    let fps = self.searching.is_some();
                    let searched = self.searching.take().and_then(|s| s.2);
                    let dir = searched.or_else(|| self.local_map(map_sig));
                    match dir {
                        Some(dir) if fps => {
                            self.fps_status = None;
                            self.mark_shared(*map_sig);
                            self.join_world(dir, *map_sig, host.clone());
                        }
                        Some(dir) => self.join_build(dir, host.clone()),
                        None => {
                            // Monde pas encore sur ce PC : on le télécharge d'abord.
                            self.fps_status = Some(format!("Téléchargement de « {map} »…"));
                            self.pending_build = Some((*map_sig, map.clone(), host.clone()));
                            self.accounts.room(Cmd::DownloadMap(*map_sig));
                        }
                    }
                }
                Event::PartyGo { room, map, map_sig } => {
                    // L'hôte du lobby lance : on le suit (même partie, même équipe).
                    if self.session.is_some() {
                        self.leave_world();
                    }
                    self.garant = None;
                    let dir = self.local_map(map_sig);
                    self.searching = Some((*map_sig, map.clone(), dir.clone()));
                    self.screen = Screen::FpsHome;
                    if dir.is_some() {
                        self.fps_status = Some("On rejoint le lobby…".into());
                        self.accounts.room(Cmd::Join(*room));
                    } else {
                        self.party_go = Some(*room);
                        self.fps_status = Some("Téléchargement de la carte…".into());
                        self.accounts.room(Cmd::DownloadMap(*map_sig));
                    }
                }
                Event::Skin { pseudo, png } => {
                    if let Some(s) = &mut self.session {
                        s.skins.received(pseudo, png);
                    }
                }
                // (v165) Ping avec le relais du monde (construction : montré aux autres).
                Event::Rtt(ms) => {
                    if let Some(b) = self.session.as_mut().and_then(|s| s.build.as_mut()) {
                        b.set_world_ping(*ms);
                    }
                }
                Event::SkinChanged(pseudo) => {
                    // En construction seulement (en FPS, le skin du début reste).
                    if let Some(s) = self.session.as_mut().filter(|s| s.build.is_some()) {
                        s.skins.forget(pseudo);
                    }
                }
                // (v231) Invitation dans le monde d'un ami : notification, et dans le tchat.
                Event::Invited { from, name, .. } => {
                    // (v233) Le module d'amis est sur l'accueil et dans le menu Échap.
                    let t = if self.session.is_some() {
                        format!("{from} t'invite dans « {name} » (Échap : Accepter).")
                    } else {
                        format!("{from} t'invite dans « {name} ».")
                    };
                    if let Some(s) = &mut self.session {
                        s.chat.say(t.clone(), chat::YELLOW);
                    }
                    self.notify(t);
                }
                Event::InviteSent(to) => {
                    // (v232) Hors ligne (ou invisible) : le relais la lui garde.
                    let online = self
                        .accounts
                        .friends
                        .iter()
                        .any(|(p, on)| *on && p.eq_ignore_ascii_case(to));
                    let t = if online {
                        format!("Invitation envoyée à {to}.")
                    } else {
                        format!("{to} n'est pas en ligne : l'invitation l'attendra.")
                    };
                    self.friends_note = Some((t, self.time));
                }
                // (v242) Une de nos invitations refusée.
                Event::InviteDeclined { by, name } => {
                    let t = format!("{by} a refusé ton invitation dans « {name} ».");
                    if let Some(s) = &mut self.session {
                        s.chat.say(t.clone(), chat::YELLOW);
                    }
                    self.notify(t);
                }
                // (v231) Demande d'ami acceptée.
                Event::FriendAdded { from, .. } => {
                    let t = format!("{from} t'a ajouté aussi : vous êtes amis.");
                    if let Some(s) = &mut self.session {
                        s.chat.say(t.clone(), chat::YELLOW);
                    }
                    self.notify(t);
                }
                Event::PartyInvite(from) => {
                    let t = format!("{from} t'invite dans son lobby (Multijoueur → FPS).");
                    if let Some(s) = &mut self.session {
                        s.chat.say(t.clone(), chat::YELLOW);
                    }
                    self.notify(t);
                }
                Event::MapProgress(_, f, up) => {
                    if self.session.is_none() {
                        let what = if *up { "Envoi" } else { "Téléchargement" };
                        self.fps_status = Some(format!("{what} de la carte : {:.0} %", f * 100.0));
                    }
                }
                Event::MapUploaded(sig) => {
                    log::info!("carte envoyée au relais");
                    // (v340) Carte ajoutée par « Ajouter une carte » : dans la liste de tous.
                    if let Some((_, name)) = self.map_adding.take_if(|(s, _)| s == sig) {
                        self.fps_status = Some(format!(
                            "« {name} » est dans la liste des cartes FPS de tous les joueurs."
                        ));
                        self.accounts.room(Cmd::Maps);
                    }
                }
                // (v340) Réponse du relais à `/fps certif`.
                Event::MapCertified { on, .. } => {
                    self.certif_asked = None;
                    if let Some(s) = &mut self.session {
                        s.chat.say(
                            if *on {
                                "Carte certifiée : elle passe en tête de la liste FPS de tous \
                                 les joueurs."
                            } else {
                                "Certification retirée : la carte reprend sa place parmi les \
                                 plus jouées."
                            },
                            chat::LIGHT_PURPLE,
                        );
                    }
                    self.accounts.room(Cmd::Maps);
                }
                Event::MapDownloaded { sig, bytes } => {
                    let name = self
                        .map_rows
                        .iter()
                        .find(|m| m.sig == *sig)
                        .map(|m| m.name.clone())
                        .or_else(|| self.pending_build.as_ref().map(|p| p.1.clone()))
                        .unwrap_or_else(|| "Carte".into());
                    let for_garant = self
                        .garant
                        .as_ref()
                        .is_some_and(|g| g.needs_map() && g.sig == *sig);
                    let name = if for_garant {
                        self.garant.as_ref().map_or(name, |g| g.map.clone())
                    } else {
                        name
                    };
                    match fpsmap::unpack(&self.paths.root.join("maps"), &name, bytes) {
                        Ok(dir) if for_garant => {
                            if let Some(g) = &mut self.garant {
                                g.set_dir(dir);
                            }
                        }
                        Ok(dir) => {
                            if let Some((_, _, host)) = self.pending_build.take() {
                                self.fps_status = None;
                                self.join_build(dir, host);
                            } else if self.solo_map_pending.as_ref().is_some_and(|p| p.0 == *sig)
                            {
                                // (v232) Solo → Cartes FPS : copiée dans ses mondes (jouée).
                                if let Some((sig, name, play)) = self.solo_map_pending.take() {
                                    self.solo_map_copied(&dir, &name, &sig, play);
                                }
                            } else if let Some(search) = &mut self.searching {
                                let players = fpsmap::load(&dir).players();
                                search.2 = Some(dir);
                                if let Some(room) = self.party_go.take() {
                                    self.fps_status = Some("On rejoint le lobby…".into());
                                    self.accounts.room(Cmd::Join(room));
                                } else {
                                    self.fps_status = Some("Recherche d'une partie…".into());
                                    self.accounts.room(Cmd::Search {
                                        map_sig: *sig,
                                        map: name,
                                        players,
                                    });
                                }
                            }
                        }
                        Err(e) => self.fps_status = Some(format!("Carte illisible : {e:#}")),
                    }
                }
                Event::MapFailed(sig, m) => {
                    self.searching = None;
                    self.solo_map_pending = None;
                    // (v340) Carte de « Ajouter une carte » refusée (trop grande…) : plus
                    // attendue.
                    if self.map_adding.as_ref().is_some_and(|(s, _)| s == sig) {
                        self.map_adding = None;
                    }
                    if self.pending_build.take().is_some() {
                        self.accounts.room(Cmd::Leave);
                    }
                    self.fps_status = Some(format!("Carte : {m}"));
                    self.notify(format!("Carte : {m}"));
                }
                Event::Reported(p) => {
                    if let Some(s) = &mut self.session {
                        s.chat.say(
                            format!("{p} est signalé : tu ne joueras plus avec ce joueur."),
                            chat::YELLOW,
                        );
                    }
                }
                Event::ServerAdmin(text) => {
                    if let Some(s) = self.session.as_mut().filter(|_| !text.is_empty()) {
                        // Liste des OP demandée pour ouvrir l'éditeur de lumières : elle
                        // n'arrive qu'aux OP, on ne l'affiche pas.
                        if !(text.starts_with("OP :") && s.light_op_answer(true)) {
                            s.chat.say(text, chat::YELLOW);
                        }
                    }
                }
                Event::Kicked(reason) => {
                    let world = self.world_key.clone();
                    if self.session.as_ref().is_some_and(|s| s.build.is_some()) {
                        self.leave_world();
                    }
                    // (v325) Monde supprimé par son créateur : sa copie sur ce PC aussi.
                    if let Some((relay, key)) = world.filter(|_| reason.contains("supprimé")) {
                        online_copies::forget(&self.paths.root, &relay, &key);
                    }
                    self.notify(reason);
                }
                // (v311) Suppression du monde : le compte a la double authentification,
                // le relais veut un code (ou celui donné était faux).
                Event::DeleteWorldCode => {
                    if self.session.as_ref().is_some_and(|s| s.build.is_some()) {
                        self.delete_code.clear();
                        self.delete_code_needed = true;
                        self.screen = Screen::DeleteWorld;
                    }
                }
                Event::BanDone(p) => {
                    if let Some(s) = &mut self.session {
                        s.chat.say(
                            format!(
                                "{p} est banni du relais pour triche : compte à vie, \
                                 machines pour 3 mois."
                            ),
                            chat::YELLOW,
                        );
                    }
                }
                Event::Unreported(p) => {
                    if let Some(s) = &mut self.session {
                        s.chat
                            .say(format!("Signalement de {p} annulé."), chat::YELLOW);
                    }
                }
                Event::Friends(list) => {
                    // (v271) L'ami demandé est dans la liste : la demande est partie (il la
                    // verra même s'il n'est pas connecté), ou vous êtes amis.
                    if self
                        .friend_adding
                        .as_ref()
                        .is_some_and(|(_, t)| self.time - t >= 10.0)
                    {
                        self.friend_adding = None;
                    }
                    let asked = self.friend_adding.as_ref().and_then(|(p, _)| {
                        list.iter()
                            .position(|(f, _)| f.eq_ignore_ascii_case(p))
                    });
                    if let Some(i) = asked {
                        self.friend_adding = None;
                        let name = &list[i].0;
                        let pending = self
                            .accounts
                            .friend_places
                            .get(i)
                            .is_none_or(|w| w.kind == minelib_net::proto::FRIEND_PENDING);
                        let t = if pending {
                            format!(
                                "Demande d'ami envoyée à {name} : il la verra dans ses \
                                 demandes, même s'il n'est pas connecté."
                            )
                        } else {
                            format!("{name} et toi êtes amis.")
                        };
                        self.friends_note = Some((t.clone(), self.time));
                        self.friends_suggest = None;
                        if let Some(s) = &mut self.session {
                            s.chat.say(t, chat::YELLOW);
                        }
                    }
                    // (v231) Dans le tchat seulement pour `/amis` (l'écran des amis redemande
                    // la liste toutes les quelques secondes).
                    if let Some(s) = self.session.as_mut().filter(|s| s.friends_said) {
                        s.friends_said = false;
                        let t = if list.is_empty() {
                            "Aucun ami pour l'instant (/ami <pseudo>).".to_string()
                        } else {
                            let places = &self.accounts.friend_places;
                            let names: Vec<String> = list
                                .iter()
                                .enumerate()
                                .map(|(i, (p, on))| {
                                    let pending = places.get(i).is_some_and(|w| {
                                        w.kind == minelib_net::proto::FRIEND_PENDING
                                    });
                                    if pending {
                                        format!("{p} (demande envoyée)")
                                    } else if *on {
                                        format!("{p} (en ligne)")
                                    } else {
                                        p.clone()
                                    }
                                })
                                .collect();
                            format!("Amis : {}", names.join(", "))
                        };
                        s.chat.say(t, chat::YELLOW);
                    }
                    // Classement des amis à refaire si la liste a changé (pas pour un
                    // simple changement « en ligne »).
                    let names: Vec<String> = list.iter().map(|f| f.0.clone()).collect();
                    if self.screen == Screen::FpsHome && names != self.friend_names {
                        self.friend_names = names;
                        if !self.fps_home.world_tab {
                            self.accounts.room(Cmd::Leaderboard { friends: true });
                        }
                    }
                }
                Event::RoomClosed => {
                    let online = self
                        .session
                        .as_ref()
                        .is_some_and(|s| s.online_match() || s.build.is_some());
                    if online {
                        self.leave_world();
                        self.notify("L'hôte a fermé la partie.".to_string());
                    }
                }
                // Monde en pause : son relais n'entend plus assez de gardiens (rien n'y change,
                // les modifications du joueur attendent) ; ou il repart.
                Event::Paused(p) => {
                    let p = *p;
                    if let Some(s) = &mut self.session {
                        if let Some(b) = &mut s.build {
                            if b.set_paused(p) {
                                let (m, c) = if p {
                                    (
                                        "Monde en pause : son relais ne joint plus ses gardiens. \
                                         Reprise en cours…",
                                        chat::YELLOW,
                                    )
                                } else {
                                    ("Le monde repart.", chat::YELLOW)
                                };
                                s.chat.say(m.to_string(), c);
                            }
                        }
                    }
                }
                // (v146) Veille : la connexion au relais du monde est fermée ; la connexion de
                // garant aussi (sauf si elle tient un relais), elle reviendra avec nous.
                Event::Dozed { .. } => {
                    if let Some(b) = self.session.as_mut().and_then(|s| s.build.as_mut()) {
                        b.set_dozing();
                    }
                    // (Celle prêtée à son pays reste : elle ne passe pas par le relais du monde.)
                    if !self.garant_pays
                        && !self.garant_worker.as_ref().is_some_and(|w| w.hosting())
                    {
                        self.garant_worker = None;
                        self.garant_worker_tried = false;
                    }
                }
                // (v146) De retour au relais du monde (le même) : la partie continue telle
                // quelle, par la nouvelle connexion.
                Event::TakenOver { .. }
                    if self
                        .session
                        .as_ref()
                        .and_then(|s| s.build.as_ref())
                        .is_some_and(|b| b.is_dozing()) =>
                {
                    if let (Some(b), Some(link)) = (
                        self.session.as_mut().and_then(|s| s.build.as_mut()),
                        self.accounts.link.clone(),
                    ) {
                        b.woke(link);
                    }
                }
                // Le relais du monde est tombé, son relais de secours a repris le monde : la
                // partie continue, par la nouvelle connexion.
                Event::TakenOver { host, .. } => {
                    // La connexion de garant suit le monde chez son nouveau relais (sauf si
                    // c'est elle qui le tient, ou si elle est prêtée à son pays) : il pourra s'y
                    // choisir un relais de secours.
                    if !self.garant_pays
                        && !self.garant_worker.as_ref().is_some_and(|w| w.hosting())
                    {
                        self.garant_worker = None;
                        self.garant_worker_tried = false;
                    }
                    let me = self.accounts.pseudo();
                    let link = self.accounts.link.clone();
                    if let (Some(s), Some(link)) = (&mut self.session, link) {
                        if let Some(b) = &mut s.build {
                            b.set_link(link.clone());
                            if host.eq_ignore_ascii_case(&me) {
                                b.set_owner();
                            } else {
                                b.clear_owner();
                            }
                            s.voice = Some(voice::Voice::new(link));
                            s.chat.say(
                                "Monde repris par son relais de secours : la partie continue."
                                    .to_string(),
                                chat::YELLOW,
                            );
                        }
                    }
                }
                Event::Error(m) => {
                    if self.searching.take().is_some() || self.screen == Screen::FpsHome {
                        self.fps_status = Some(m.clone());
                    }
                    // (v271) Réponse à un ami demandé (« Joueur « 1side » inconnu. Voulais-tu
                    // dire « lside »… ») : dans le panneau des amis, avec un bouton pour le
                    // pseudo proposé, et dans le tchat même hors d'un monde en ligne.
                    let adding = self
                        .friend_adding
                        .take()
                        .is_some_and(|(_, t)| self.time - t < 10.0);
                    if adding {
                        self.friends_note = Some((m.clone(), self.time));
                        self.friends_suggest =
                            minelib_net::proto::suggested_player(&m).map(str::to_string);
                    }
                    // (v340) Refus d'une certification (`/fps certif`) : dit aussi en solo.
                    let certif = self
                        .certif_asked
                        .take()
                        .is_some_and(|t| self.time - t < 10.0);
                    // (v340) Envoi refusé d'une carte de « Ajouter une carte » : plus attendu.
                    self.map_adding = None;
                    if let Some(s) = &mut self.session {
                        // Refus de la liste des OP demandée pour l'éditeur de lumières : son
                        // propre message à la place.
                        let editor = m.contains("Réservé aux OP") && s.light_op_answer(false);
                        if !editor && (adding || certif || s.net.is_some() || s.build.is_some()) {
                            s.chat.say(m.clone(), chat::RED);
                        }
                    }
                }
                _ => {
                    if let Some(s) = &mut self.session {
                        s.net_event(&e);
                    }
                }
            }
        }
        if let Some(s) = &mut self.session {
            // (v340) Compte connecté après l'ouverture du monde (connexion automatique plus
            // lente que le monde) : la partie le connaît aussi (`/fps certif`…).
            if s.pseudo.is_empty() && self.accounts.logged_in() {
                s.pseudo = self.accounts.pseudo();
            }
            if let Some(started) = s.room_started.take() {
                self.accounts.room(Cmd::RoomStarted(started));
            }
            if let Some(paused) = s.room_paused.take() {
                self.accounts.room(Cmd::RoomPaused(paused));
            }
            // Résultat de la partie : chaque joueur l'envoie, le relais les compare.
            if let Some(summary) = s.match_result.take() {
                self.accounts
                    .room(Cmd::MatchResult(minelib_net::game::encode(&summary)));
            }
            for cmd in std::mem::take(&mut s.relay_cmds) {
                // (v340) `/fps certif` : sa réponse aussi est dite (tchat), même en solo.
                if matches!(cmd, Cmd::MapCertify { .. }) {
                    self.certif_asked = Some(self.time);
                }
                // (v271) `/ami <pseudo>` : sa réponse aussi est dite (tchat et panneau).
                if let Cmd::Friend { pseudo, add: true } = &cmd {
                    if !pseudo.is_empty() {
                        self.friend_adding = Some((pseudo.clone(), self.time));
                        self.friends_suggest = None;
                    }
                }
                self.accounts.room(cmd);
            }
            if std::mem::take(&mut s.voice_dirty) {
                save_volumes(&self.paths.root.join("voice.txt"), &s.voice_volumes);
            }
        }
        // Exclu par les règles (trois coéquipiers tués) : on quitte la partie.
        if let Some(reason) = self.session.as_mut().and_then(|s| s.kicked.take()) {
            self.leave_world();
            self.notify(reason);
        }
    }

    /// Garant indépendant : simulation, disponibilité annoncée au relais.
    fn garant_update(&mut self, dt: f32) {
        let free = self.session.is_none()
            && self.accounts.logged_in()
            && self.searching.is_none()
            && self.options.garant;
        self.announce_garant(free);
        // En pleine partie FPS en ligne (ou sur un serveur de construction) : une deuxième
        // connexion peut servir de garant à d'autres joueurs (le relais la préfère quand la
        // partie vient de commencer ; les mondes de construction prennent leurs garants
        // parmi les joueurs d'autres mondes).
        let playing = self.options.garant
            && self.accounts.logged_in()
            && self.session.as_ref().is_some_and(|s| {
                (s.online_match() && s.game.mode == save::GameMode::Fps) || s.build.is_some()
            });
        // (v146) En veille, pas de connexion de garant au relais du monde (elle a été fermée
        // avec la nôtre, sauf si elle tenait un relais). (v147) Si son pays appelle ce joueur,
        // elle va chez lui : il choisit lui-même les hôtes de ses provinces.
        if playing && self.accounts.dozing() {
            if self.garant_worker.as_ref().is_some_and(|w| !w.alive()) {
                self.garant_worker = None;
                self.garant_pays = false;
                self.garant_retry = Some(std::time::Instant::now() + Duration::from_secs(30));
            }
            let now = std::time::Instant::now();
            let me = self.accounts.pseudo();
            let call = self
                .session
                .as_ref()
                .and_then(|s| s.build.as_ref())
                .and_then(|b| b.garant_call().cloned())
                .filter(|c| c.picks(&me));
            let target = call
                .as_ref()
                .map(|c| (c.at.province, c.at.size, c.at.epoch));
            // (v148) Appelé par un autre pays (plus bas, ou repris par son relais de secours) :
            // la connexion de garant y va, si elle ne tient rien.
            if self.garant_pays
                && target.is_some()
                && target != self.garant_target
                && self
                    .garant_worker
                    .as_ref()
                    .is_some_and(|w| !w.busy() && !w.hosting())
            {
                self.garant_worker = None;
                self.garant_pays = false;
            }
            if self.garant_worker.is_none() && self.garant_retry.is_none_or(|t| now >= t) {
                if let Some(login) = call.and_then(|c| self.accounts.pays_garant_login(&c)) {
                    let maps = self
                        .map_rows
                        .iter()
                        .filter_map(|m| Some((m.sig, m.local.clone()?)))
                        .collect();
                    log::info!("garant en jeu : appelé par son pays, on lui prête notre connexion");
                    self.garant_worker = Some(garant_worker::GarantWorker::start(
                        login,
                        maps,
                        self.paths.root.join("maps"),
                    ));
                    self.garant_pays = true;
                    self.garant_target = target;
                }
            }
            self.garant_worker_tried = false;
            return;
        }
        if playing {
            if self.garant_worker.is_none() && !self.garant_worker_tried {
                self.garant_worker_tried = true;
                if let Some(login) = self.accounts.garant_login() {
                    let maps = self
                        .map_rows
                        .iter()
                        .filter_map(|m| Some((m.sig, m.local.clone()?)))
                        .collect();
                    self.garant_worker = Some(garant_worker::GarantWorker::start(
                        login,
                        maps,
                        self.paths.root.join("maps"),
                    ));
                    self.garant_pays = false;
                }
            }
            if self.garant_worker.as_ref().is_some_and(|w| !w.alive()) {
                self.garant_worker = None;
                self.garant_pays = false;
            }
        } else {
            // Partie finie (ou quittée) : la partie servie passe au garant en attente.
            self.garant_worker = None;
            self.garant_worker_tried = false;
            self.garant_pays = false;
        }
        if self.session.is_some() {
            return;
        }
        if let Some(g) = &mut self.garant {
            for cmd in g.update(dt) {
                self.accounts.room(cmd);
            }
            // Arbitre : verdict rendu, il redevient libre.
            if g.done {
                self.garant = None;
            }
        }
    }

    fn announce_garant(&mut self, on: bool) {
        if self.garant_announced != Some(on) && self.accounts.logged_in() {
            self.garant_announced = Some(on);
            self.accounts
                .room(minelib_net::client::Cmd::GarantAvailable(on));
        }
    }

    /// Dossier d'une carte de ce PC, par sa signature.
    fn local_map(&mut self, sig: &[u8; 32]) -> Option<PathBuf> {
        if let Some(d) = self
            .map_rows
            .iter()
            .find(|m| m.sig == *sig)
            .and_then(|m| m.local.clone())
        {
            return Some(d);
        }
        let dir = fpsmap::downloaded_dir(&self.paths.root.join("maps"), sig);
        if dir.join("level.txt").exists() {
            return Some(dir);
        }
        let worlds = self.local_worlds();
        worlds
            .into_iter()
            .map(|(d, _)| d)
            .find(|d| self.sig_cache.signature(d).as_ref() == Some(sig))
    }

    /// Mondes de ce PC utilisables comme cartes : `saves` et cartes téléchargées.
    fn local_worlds(&self) -> Vec<(PathBuf, String)> {
        let mut v: Vec<(PathBuf, String)> = save::list_worlds(&self.paths.saves)
            .into_iter()
            .filter(|w| !w.minecraft)
            .map(|w| (w.dir, w.meta.name))
            .collect();
        v.extend(
            save::list_worlds(&self.paths.root.join("maps"))
                .into_iter()
                .map(|w| (w.dir, w.meta.name)),
        );
        v
    }

    /// (v232) Solo → Cartes FPS, liste à jour.
    fn open_solo_maps(&mut self) {
        if self.accounts.logged_in() {
            self.accounts.room(minelib_net::client::Cmd::Maps);
        }
        if self.solo_map_pending.is_none() {
            self.fps_status = None;
        }
        self.solo_maps.polled = self.time;
        self.solo_maps_seen = self.accounts.maps_rev;
        self.refresh_solo_rows();
        self.screen = Screen::SoloMaps;
    }

    /// (v232) Lignes de Solo → Cartes FPS : les cartes du relais, celles téléchargées, et
    /// celles de ses mondes dont les départs sont réglés (index dans `map_rows`).
    fn refresh_solo_rows(&mut self) {
        self.rebuild_map_rows();
        let saves = &self.paths.saves;
        let copies = fpsmap::solo_copies(saves);
        let mut idx = Vec::new();
        let mut rows = Vec::new();
        for (i, m) in self.map_rows.iter().enumerate() {
            let in_saves = m.local.as_ref().is_some_and(|d| d.starts_with(saves));
            if in_saves && !m.configured {
                continue;
            }
            let downloaded = m.local.is_some() && !in_saves;
            let copied = !in_saves && copies.contains(&m.sig);
            idx.push(i);
            rows.push(solo_maps::SoloRow {
                name: m.name.clone(),
                detail: solo_maps::detail(
                    in_saves || copied,
                    downloaded,
                    m.stored,
                    &m.author,
                    m.configured,
                    m.plays,
                ),
                players: m.players,
                can_play: m.local.is_some() || copied || m.stored,
                can_download: !in_saves && !copied && (m.local.is_some() || m.stored),
            });
        }
        self.solo_rows = (idx, rows);
    }

    fn solo_maps_screen(&mut self, input: &Input, size: (u32, u32)) {
        use minelib_net::client::Cmd;
        if self.accounts.maps_rev != self.solo_maps_seen {
            self.solo_maps_seen = self.accounts.maps_rev;
            self.refresh_solo_rows();
        }
        let time = self.time;
        let logged = self.accounts.logged_in();
        let busy = self.solo_map_pending.is_some();
        let mut c = Ctx::new(&mut self.ui, input, size);
        c.background();
        let action = solo_maps::screen(
            &mut c,
            input,
            &mut self.solo_maps,
            &self.solo_rows.1,
            self.fps_status.as_deref(),
            logged,
            busy,
            time,
        );
        match action {
            solo_maps::SoloMapsAction::None => {}
            solo_maps::SoloMapsAction::Back => {
                self.refresh_worlds();
                self.screen = Screen::Solo;
            }
            solo_maps::SoloMapsAction::Poll => self.accounts.room(Cmd::Maps),
            solo_maps::SoloMapsAction::Refresh => {
                self.sig_cache = fpsmap::SigCache::default();
                self.open_solo_maps();
            }
            solo_maps::SoloMapsAction::Play(k) => {
                if let Some(&i) = self.solo_rows.0.get(k) {
                    self.solo_map_go(i, true);
                }
            }
            solo_maps::SoloMapsAction::Download(k) => {
                if let Some(&i) = self.solo_rows.0.get(k) {
                    self.solo_map_go(i, false);
                }
            }
        }
    }

    /// (v232) Carte `i` de `map_rows` : jouée contre des bots (`play`) ou copiée dans ses
    /// mondes. Du relais : téléchargée d'abord (suite dans `Event::MapDownloaded`).
    fn solo_map_go(&mut self, i: usize, play: bool) {
        use minelib_net::client::Cmd;
        let Some(m) = self.map_rows.get(i).cloned() else {
            return;
        };
        let saves = self.paths.saves.clone();
        let mine = m
            .local
            .clone()
            .filter(|d| d.starts_with(&saves))
            .or_else(|| fpsmap::solo_copy(&saves, &m.sig));
        if let Some(dir) = mine {
            if play {
                self.play_vs_bots(dir);
            } else {
                self.fps_status = Some(format!("« {} » est déjà dans tes mondes.", m.name));
            }
            return;
        }
        if let Some(src) = &m.local {
            self.solo_map_copied(src, &m.name, &m.sig, play);
            return;
        }
        if !self.accounts.logged_in() {
            self.fps_status = Some("Connecte-toi pour télécharger les cartes du relais.".into());
            return;
        }
        if !m.stored {
            self.fps_status = Some("Cette carte est encore en cours d'envoi au relais.".into());
            return;
        }
        self.solo_map_pending = Some((m.sig, m.name.clone(), play));
        self.fps_status = Some("Téléchargement de la carte…".into());
        self.accounts.room(Cmd::DownloadMap(m.sig));
    }

    /// (v232) Carte téléchargée (`src`, dans `maps`) : copiée dans ses mondes, puis jouée
    /// contre des bots (`play`).
    fn solo_map_copied(&mut self, src: &std::path::Path, name: &str, sig: &[u8; 32], play: bool) {
        match fpsmap::copy_to_saves(src, &self.paths.saves, name, sig) {
            Ok(dir) => {
                log::info!("carte copiée dans {}", dir.display());
                self.refresh_worlds();
                self.refresh_solo_rows();
                if play {
                    self.play_vs_bots(dir);
                } else {
                    self.fps_status = Some(format!("« {name} » est maintenant dans tes mondes."));
                }
            }
            Err(e) => self.fps_status = Some(format!("Copie impossible : {e:#}")),
        }
    }

    /// (v232) Ouvre ce monde en mode FPS : partie contre des bots. Un monde qui n'était pas
    /// en mode FPS garde le sien dans son fichier.
    fn play_vs_bots(&mut self, dir: PathBuf) {
        self.fps_status = None;
        // (v322) En solo : compté depuis près du joueur, la partie contre les bots se place
        // d'elle-même près des départs de la carte (`Session::solo_fps_far`).
        self.play_with(dir, true);
        if let Some(s) = &mut self.session {
            if s.game.mode != save::GameMode::Fps {
                s.saved_mode = Some(s.game.mode);
                s.set_mode(save::GameMode::Fps);
            }
        }
    }

    /// Écran FPS : classement et cartes à jour.
    fn open_fps_home(&mut self) {
        use minelib_net::client::Cmd;
        self.fps_status = None;
        self.searching = None;
        self.accounts.room(Cmd::Maps);
        self.accounts.room(Cmd::Leaderboard {
            friends: !self.fps_home.world_tab,
        });
        // (v340) Le format choisi la dernière fois (« Équipe »).
        self.fps_home.team = multiplayer::team_or_default(self.options.fps_team);
        self.fps_home.team_open = false;
        self.fps_home.adding = false;
        self.rebuild_map_rows();
        self.screen = Screen::FpsHome;
    }

    /// Cartes de ce PC et du relais, chacune une seule fois (signature).
    fn rebuild_map_rows(&mut self) {
        let mut rows: Vec<multiplayer::MapRow> = Vec::new();
        for (dir, name) in self.local_worlds() {
            let Some(sig) = self.sig_cache.signature(&dir) else {
                continue;
            };
            if rows.iter().any(|r| r.sig == sig) {
                continue;
            }
            let fps = fpsmap::load(&dir);
            rows.push(multiplayer::MapRow {
                sig,
                name,
                author: self.accounts.pseudo(),
                local: Some(dir),
                waiting: 0,
                plays: 0,
                configured: fps.ct.is_some() && fps.t.is_some(),
                players: fps.players(),
                stored: true,
                certified: false,
            });
        }
        for m in &self.accounts.maps {
            match rows.iter_mut().find(|r| r.sig == m.sig) {
                Some(r) => {
                    r.waiting = m.waiting;
                    r.plays = m.plays;
                    r.author = m.author.clone();
                    r.certified = m.certified;
                }
                // Une carte dont la recherche vient de commencer apparaît tout de suite,
                // même si elle n'est pas encore arrivée entière au relais.
                None => rows.push(multiplayer::MapRow {
                    sig: m.sig,
                    name: m.name.clone(),
                    author: m.author.clone(),
                    local: None,
                    waiting: m.waiting,
                    plays: m.plays,
                    configured: true,
                    players: m.players,
                    stored: m.stored,
                    certified: m.certified,
                }),
            }
        }
        // Anciennes versions d'une carte jamais jouées jusqu'au bout en ligne : retirées.
        multiplayer::hide_old_versions(&mut rows);
        // (v340) Les certifiées d'abord, puis les plus jouées.
        multiplayer::sort_rows(&mut rows);
        // La carte choisie reste choisie (les lignes ont pu changer de place).
        let picked = self
            .fps_home
            .selected
            .and_then(|i| self.map_rows.get(i))
            .map(|m| m.sig);
        self.fps_home.selected = picked.and_then(|s| rows.iter().position(|r| r.sig == s));
        self.map_rows = rows;
        if self.fps_home.adding {
            self.rebuild_add_rows();
        }
    }

    /// (v340) Multijoueur → FPS → « Ajouter une carte » : ses mondes (pas les cartes
    /// téléchargées), ceux qu'on peut ajouter d'abord (départs réglés, pas encore dans la
    /// liste FPS du relais), puis ceux qui y sont déjà, puis les autres.
    fn rebuild_add_rows(&mut self) {
        let picked = self
            .fps_home
            .add_selected
            .and_then(|i| self.add_rows.get(i))
            .map(|a| a.sig);
        let mut rows: Vec<multiplayer::AddRow> = Vec::new();
        for w in save::list_worlds(&self.paths.saves) {
            if w.minecraft {
                continue;
            }
            let Some(sig) = self.sig_cache.signature(&w.dir) else {
                continue;
            };
            let fps = fpsmap::load(&w.dir);
            rows.push(multiplayer::AddRow {
                listed: self.accounts.maps.iter().any(|m| m.sig == sig && m.stored),
                configured: fps.ct.is_some() && fps.t.is_some(),
                players: fps.players(),
                dir: w.dir,
                sig,
                name: w.meta.name,
            });
        }
        rows.sort_by(|a, b| {
            let rank = |r: &multiplayer::AddRow| (!r.can_add(), !r.listed);
            rank(a).cmp(&rank(b)).then(a.name.cmp(&b.name))
        });
        self.fps_home.add_selected = picked.and_then(|s| rows.iter().position(|r| r.sig == s));
        self.add_rows = rows;
    }

    /// (v340) Ce monde (de « Ajouter une carte ») part au relais : une carte FPS proposée à
    /// tous, comme une carte lancée une fois en ligne (et renvoyée par ce PC si le relais
    /// la perd, `maps_upkeep`).
    fn add_map(&mut self, i: usize) {
        use minelib_net::client::Cmd;
        let Some(a) = self.add_rows.get(i).cloned() else {
            return;
        };
        if !self.accounts.logged_in() {
            self.fps_status = Some("Connecte-toi pour ajouter une carte.".into());
            return;
        }
        match fpsmap::package(&a.dir) {
            Ok(p) if p.signature() == a.sig => {
                self.mark_shared(a.sig);
                self.map_adding = Some((a.sig, a.name.clone()));
                self.fps_status = Some(format!("Envoi de « {} » au relais…", a.name));
                self.accounts.room(Cmd::UploadMap {
                    sig: a.sig,
                    name: a.name,
                    bytes: p.to_bytes(),
                    fps: true,
                });
            }
            Ok(_) => {
                // Le monde a changé depuis la liste : on la refait.
                self.sig_cache = fpsmap::SigCache::default();
                self.rebuild_add_rows();
                self.fps_status = Some(format!("« {} » a changé : choisis-le de nouveau.", a.name));
                self.fps_home.adding = true;
            }
            Err(e) => self.fps_status = Some(format!("Carte illisible : {e:#}")),
        }
    }

    fn fps_home_screen(&mut self, input: &Input, size: (u32, u32)) {
        use minelib_net::client::Cmd;
        // Liste du relais reçue : on refait la liste des cartes.
        let relay_maps = self.accounts.maps_rev;
        if relay_maps != self.fps_home_maps_seen {
            self.fps_home_maps_seen = relay_maps;
            self.rebuild_map_rows();
        }
        let time = self.time;
        let busy = self.searching.is_some();
        let me = self.accounts.pseudo();
        let mut c = Ctx::new(&mut self.ui, input, size);
        let lobby = multiplayer::LobbyView {
            me: &me,
            friends: &self.accounts.friends,
            members: &self.accounts.party.1,
            invites: &self.accounts.invites,
            color: self.options.fps_color,
        };
        let action = multiplayer::fps_screen(
            &mut c,
            input,
            &mut self.fps_home,
            self.accounts.leaderboard.as_ref(),
            &self.map_rows,
            &self.add_rows,
            self.fps_status.as_deref(),
            busy,
            time,
            lobby,
        );
        // (v315) Classement mondial entier : la suite arrive quand on défile vers la fin.
        if self.fps_home.world_tab {
            self.accounts.leaderboard_more(self.fps_home.scroll);
        }
        match action {
            multiplayer::FpsAction::Back => {
                self.searching = None;
                self.screen = Screen::Multiplayer;
            }
            multiplayer::FpsAction::Poll => {
                self.accounts.room(Cmd::Maps);
                // Amis en ligne (pour les inviter).
                self.accounts.room(Cmd::Friend {
                    pseudo: String::new(),
                    add: false,
                });
            }
            multiplayer::FpsAction::Color(c) => {
                self.options.fps_color = c;
                let _ = self.options.save(&self.paths.options);
            }
            // (v340) Format choisi : gardé pour la prochaine fois.
            multiplayer::FpsAction::Team(n) => {
                if self.options.fps_team != n {
                    self.options.fps_team = n;
                    let _ = self.options.save(&self.paths.options);
                }
            }
            multiplayer::FpsAction::OpenAdd => {
                self.fps_status = None;
                self.rebuild_add_rows();
                self.accounts.room(Cmd::Maps);
            }
            multiplayer::FpsAction::AddMap(i) => self.add_map(i),
            multiplayer::FpsAction::Party(action, pseudo) => {
                use minelib_net::proto;
                if action == proto::PARTY_ACCEPT || action == proto::PARTY_DECLINE {
                    self.accounts
                        .invites
                        .retain(|p| !p.eq_ignore_ascii_case(&pseudo));
                }
                self.accounts.room(Cmd::Party { action, pseudo });
            }
            multiplayer::FpsAction::Refresh => {
                self.sig_cache = fpsmap::SigCache::default();
                self.open_fps_home();
            }
            multiplayer::FpsAction::Tab(friends) => {
                self.accounts.room(Cmd::Leaderboard { friends });
            }
            multiplayer::FpsAction::AddFriend(p) => {
                self.accounts.room(Cmd::Friend {
                    pseudo: p,
                    add: true,
                });
            }
            // En ligne, on joue dans la dernière version (une partie en cours se finit dans
            // la sienne).
            multiplayer::FpsAction::Search(_)
                if self.updater.available.is_some() && self.updater.hold.is_none() =>
            {
                let v = self.updater.available.as_ref().map_or(0, |r| r.version);
                self.fps_status = Some(format!(
                    "Version {v} disponible : mets le jeu à jour (accueil) pour jouer en ligne."
                ));
            }
            multiplayer::FpsAction::Search(i) => {
                // On va jouer : plus garant (le relais fait reprendre la partie servie).
                self.garant = None;
                if let Some(m) = self.map_rows.get(i).cloned() {
                    self.searching = Some((m.sig, m.name.clone(), m.local.clone()));
                    if m.local.is_some() {
                        self.fps_status = Some("Recherche d'une partie…".into());
                        self.accounts.room(Cmd::Search {
                            map_sig: m.sig,
                            map: m.name,
                            players: m.players,
                        });
                    } else {
                        self.fps_status = Some("Téléchargement de la carte…".into());
                        self.accounts.room(Cmd::DownloadMap(m.sig));
                    }
                }
            }
            multiplayer::FpsAction::None => {}
        }
    }

    /// Skins : le sien envoyé au relais à chaque connexion, celui choisi dans la fenêtre
    /// « Ouvrir » vérifié puis gardé, ceux des autres joueurs demandés au relais.
    fn skin_upkeep(&mut self) {
        use minelib_net::client::Cmd;
        if !self.accounts.logged_in() {
            self.skin_sent = false;
        } else if !self.skin_sent {
            self.skin_sent = true;
            let png = std::fs::read(self.paths.root.join("skin.png")).unwrap_or_default();
            self.accounts.room(Cmd::SkinPut(png));
        }
        if let Some(rx) = &self.skin_pick {
            match rx.try_recv() {
                Ok(Some(path)) => {
                    self.skin_pick = None;
                    self.use_skin_file(&path);
                }
                Ok(None) | Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.skin_pick = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        if let Some(s) = &mut self.session {
            // (v146) Son skin, déposé chez chaque province où l'on entre ; en veille, ceux des
            // autres sont demandés aux provinces (le relais du monde n'est plus joint).
            if let Some(b) = s.build.as_mut().filter(|b| !b.has_skin()) {
                b.set_skin(std::fs::read(self.paths.root.join("skin.png")).unwrap_or_default());
            }
            // (v157) Têtes de joueurs : leur skin, tenu à jour.
            s.update_head_skins(&self.accounts.pseudo());
            let dozing = self.accounts.dozing();
            for p in s.skins.take_wanted() {
                match s.build.as_ref().filter(|_| dozing) {
                    Some(b) => b.ask_skin(&p),
                    None => self.accounts.room(Cmd::SkinGet(p)),
                }
            }
            if let Some(b) = s.build.as_mut() {
                for (p, png) in b.take_skins() {
                    s.skins.received(&p, &png);
                }
            }
        }
    }

    /// (v146) Au fond d'un pays : la connexion au relais du monde se ferme (veille), et se
    /// rouvre quand il le faut ; les messages des OP arrivés pendant ce temps par les
    /// provinces sont traités comme ceux du relais du monde.
    fn doze_upkeep(&mut self) {
        // (v317) Dit au réseau, pour les tunnels : en veille, l'un d'eux qui doit s'ouvrir
        // (une région de plus en vue) demande le retour au relais du monde, seul à pouvoir
        // lui donner une route par des passeurs. (Dit aussi hors d'un monde : la veille
        // finie, plus aucun tunnel n'attend.)
        minelib_net::fast::set_dozing(self.accounts.dozing());
        let Some(b) = self.session.as_mut().and_then(|s| s.build.as_mut()) else {
            return;
        };
        if self.accounts.dozing() {
            if b.wake_needed() || minelib_net::fast::take_route_wanted() {
                self.accounts.wake();
            }
        } else if b.doze_ready() {
            if let Some(room) = b.room() {
                self.accounts.doze(room);
            }
        }
        let notices = b.take_notices();
        self.accounts.game_events.extend(notices);
    }

    /// Carte jouée en partie FPS en ligne : on la gardera disponible pour les autres.
    fn mark_shared(&mut self, sig: [u8; 32]) {
        if self.shared_maps.insert(sig) {
            let text: String = self
                .shared_maps
                .iter()
                .map(|s| format!("{}\n", minelib_net::crypto::hex(s)))
                .collect();
            let path = self.paths.root.join("maps").join("partagees.txt");
            if let Some(d) = path.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            let _ = std::fs::write(path, text);
        }
    }

    /// Cartes FPS de ce PC que le relais n'a pas (relais changé, PC-relais éteint) : on
    /// les lui renvoie, une ou deux à la fois. Seulement celles déjà jouées en ligne, et
    /// (v340) celles ajoutées par « Ajouter une carte ».
    fn maps_upkeep(&mut self) {
        use minelib_net::client::Cmd;
        if !self.accounts.logged_in() {
            self.maps_asked = false;
            self.reshared.clear();
            return;
        }
        if !self.maps_asked {
            self.maps_asked = true;
            self.accounts.room(Cmd::Maps);
            return;
        }
        if self.accounts.maps_rev == self.maps_rev_seen {
            return;
        }
        self.maps_rev_seen = self.accounts.maps_rev;
        let missing: Vec<[u8; 32]> = self
            .shared_maps
            .iter()
            .filter(|s| !self.reshared.contains(*s))
            .filter(|s| !self.accounts.maps.iter().any(|m| m.sig == **s && m.stored))
            .copied()
            .take(2)
            .collect();
        for sig in missing {
            self.reshared.insert(sig);
            let Some(dir) = self.local_map(&sig) else {
                continue;
            };
            let Ok(p) = fpsmap::package(&dir) else {
                continue;
            };
            // Le monde a pu changer depuis la partie : seulement s'il est identique.
            if p.signature() != sig {
                continue;
            }
            let name = self
                .local_worlds()
                .into_iter()
                .find(|(d, _)| *d == dir)
                .map_or_else(|| "Carte".into(), |(_, n)| n);
            log::info!(
                "carte {} renvoyée au relais",
                minelib_net::crypto::hex(&sig[..6])
            );
            self.accounts.room(Cmd::UploadMap {
                sig,
                name,
                bytes: p.to_bytes(),
                fps: true,
            });
        }
    }

    /// Prend ce fichier comme skin (vérifié : PNG de 64 × 64 ou 64 × 32).
    fn use_skin_file(&mut self, path: &std::path::Path) {
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                self.skin_msg = Some((format!("Lecture impossible : {e}"), false));
                return;
            }
        };
        match skin::Skin::from_png(&bytes) {
            Ok(sk) => {
                if let Err(e) = std::fs::write(self.paths.root.join("skin.png"), &bytes) {
                    self.skin_msg = Some((format!("Enregistrement impossible : {e}"), false));
                    return;
                }
                self.my_skin = Some(sk.clone());
                if let Some(s) = &mut self.session {
                    s.skins.set_mine(Some(sk));
                }
                // Envoyé au relais (les autres le verront).
                self.skin_sent = false;
                self.skin_msg = Some(("Skin changé.".into(), true));
            }
            Err(e) => self.skin_msg = Some((format!("Skin refusé : {e:#}"), false)),
        }
    }

    /// Ouvre la fenêtre « Ouvrir » (Windows) pour choisir un skin, sans bloquer le jeu.
    fn pick_skin(&mut self) {
        #[cfg(windows)]
        {
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let file = rfd::FileDialog::new()
                    .set_title("Choisir un skin (PNG 64 × 64)")
                    .add_filter("Skin", &["png"])
                    .pick_file();
                let _ = tx.send(file);
            });
            self.skin_pick = Some(rx);
        }
        #[cfg(not(windows))]
        {
            let path = self.paths.root.join("skin.png");
            self.skin_msg = Some((format!("Dépose ton skin ici : {}", path.display()), true));
            if path.exists() {
                self.use_skin_file(&path);
            }
        }
    }

    /// Ouvre la partie FPS trouvée par la recherche, dont ce jeu est l'hôte (seul en
    /// attendant les autres).
    fn host_world(&mut self, dir: PathBuf, sig: [u8; 32]) {
        let Some(link) = self.accounts.link.clone() else {
            self.notify("Connecte-toi d'abord à ton compte.".to_string());
            return;
        };
        self.play_with(dir, false);
        let pseudo = self.accounts.pseudo();
        if let Some(s) = &mut self.session {
            // (v322) Comptée depuis l'origine de la carte (celle de la partie, chez tous).
            let map = s.fps_map();
            s.game.mode = save::GameMode::Fps;
            // Pas de vol en partie en ligne.
            s.game.can_fly = false;
            s.player.flying = false;
            s.voice = Some(voice::Voice::new(link.clone()));
            let mut net = netgame::NetGame::host(link, pseudo, map, sig, s.spawn_pos());
            // Ses amis du lobby le rejoindront dans son équipe.
            net.set_own_party(self.accounts.party.0);
            s.net = Some(net);
            s.chat.say(
                "Aucune partie en attente : tu es dans la tienne. Elle commence à 10 joueurs \
                 (échauffement en attendant).",
                chat::YELLOW,
            );
        }
    }

    /// Partie FPS rejointe : même carte (signature vérifiée par l'hôte).
    fn join_world(&mut self, dir: PathBuf, sig: [u8; 32], host: String) {
        let Some(link) = self.accounts.link.clone() else {
            return;
        };
        self.play_with(dir, false);
        if let Some(s) = &mut self.session {
            s.game.mode = save::GameMode::Fps;
            // Pas de vol en partie en ligne.
            s.game.can_fly = false;
            s.player.flying = false;
            s.voice = Some(voice::Voice::new(link.clone()));
            // (v220) Présenté à l'hôte seulement une fois son monde chargé (voir
            // `Session::loading`) : personne ne le voit apparaître avant.
            s.net = Some(netgame::NetGame::guest_loading(link, sig));
            s.chat.say(
                if host.is_empty() {
                    "Partie rejointe.".to_string()
                } else {
                    format!("Partie de {host} rejointe.")
                },
                chat::YELLOW,
            );
        }
    }

    /// Ouvre en ligne le monde solo `solo` (ce jeu en est le propriétaire). (v325) Un
    /// nouveau serveur, joué dans une copie à part (`online_copies`) : le monde solo n'en
    /// sait plus rien, et l'ouvrir de nouveau en ligne fait un autre serveur.
    fn host_build(&mut self, solo: PathBuf) {
        if self.accounts.link.is_none() {
            self.notify("Connecte-toi d'abord à ton compte.".to_string());
            return;
        }
        let name = save::list_worlds(&self.paths.saves)
            .into_iter()
            .find(|w| w.dir == solo)
            .map_or_else(|| "Monde".into(), |w| w.meta.name);
        let dir = match online_copies::fork(&self.paths.root, &solo) {
            Ok(d) => d,
            Err(e) => {
                self.notify(format!("Copie du monde impossible : {e:#}"));
                return;
            }
        };
        self.host_online(dir, true, name, Open::New);
    }

    /// (v325) Réveille le monde endormi `e` (son entrée dans la liste), joué dans sa copie :
    /// celle que ce PC en garde, sinon (endormi avant la v325) là où il se jouait alors.
    fn wake_world(&mut self, e: &minelib_net::proto::ServerEntry) {
        if self.accounts.link.is_none() {
            self.notify("Connecte-toi d'abord à ton compte.".to_string());
            return;
        }
        let me = self.accounts.pseudo();
        let creator = e.creator.eq_ignore_ascii_case(&me) || e.host.eq_ignore_ascii_case(&me);
        let keeper = !e.keeper.is_empty() && e.keeper.eq_ignore_ascii_case(&me);
        if e.elsewhere() {
            self.notify("Ce monde dort sur un autre relais : réveille-le depuis ce relais.");
            return;
        }
        if !creator && !keeper {
            let who = if e.keeper.is_empty() || e.keeper.eq_ignore_ascii_case(&e.host) {
                e.host.clone()
            } else {
                format!("{} ou {}", e.host, e.keeper)
            };
            self.notify(format!("Ce monde dort : seul {who} peut le réveiller."));
            return;
        }
        // Son créateur, alors qu'un autre en garde le dernier état : sa copie à lui est plus
        // ancienne. Un deuxième clic le confirme.
        if !keeper && !e.keeper.is_empty() {
            let again = self
                .wake_confirm
                .take()
                .is_some_and(|(k, t)| k == e.world && self.time - t < 8.0);
            if !again {
                self.wake_confirm = Some((e.world.clone(), self.time));
                self.notify(format!(
                    "{} garde le dernier état de ce monde. « Réveiller » encore : avec ta \
                     copie, plus ancienne.",
                    e.keeper
                ));
                return;
            }
        }
        let root = self.paths.root.clone();
        let relay = self.accounts.relay_hex();
        let found = online_copies::find(&root, &relay, &e.world)
            .or_else(|| online_copies::before_v325(&root, &self.paths.saves, &e.map, creator));
        let Some(src) = found else {
            let who = if keeper { e.host.clone() } else { e.keeper.clone() };
            self.notify(format!(
                "Pas de copie de ce monde sur ce PC : {who} doit le réveiller depuis le sien."
            ));
            return;
        };
        // Sa copie à lui, ou une copie à part de là où il se jouait (jamais le monde solo ni
        // la carte téléchargée eux-mêmes).
        let (dir, fresh) = if online_copies::is_copy(&root, &src) {
            (src, false)
        } else {
            match online_copies::fork(&root, &src) {
                Ok(d) => (d, true),
                Err(err) => {
                    self.notify(format!("Copie du monde impossible : {err:#}"));
                    return;
                }
            }
        };
        log::info!("réveil du monde {} depuis {}", e.world, dir.display());
        self.host_online(
            dir,
            fresh,
            e.map.clone(),
            Open::Wake {
                title: e.name.clone(),
                world: e.world.clone(),
            },
        );
    }

    /// (v325) Propriétaire d'un monde en ligne qui s'en va : sa copie (`dir`) en garde le
    /// dernier état. Jamais ouvert en ligne (le relais n'a pas répondu) : la copie où l'on a
    /// joué devient un monde solo, plutôt que de rester où personne ne la verrait.
    fn keep_online_copy(&mut self, dir: &std::path::Path) {
        let root = self.paths.root.clone();
        match self.world_key.clone() {
            Some((relay, key)) => online_copies::keep(&root, &relay, &key, dir),
            None if online_copies::is_copy(&root, dir) => {
                match online_copies::to_solo(&root, &self.paths.saves, dir) {
                    Ok(d) => log::info!("monde jamais ouvert en ligne : dans les mondes solo ({})", d.display()),
                    Err(e) => log::warn!("copie d'un monde jamais ouvert en ligne : {e:#}"),
                }
            }
            None => {}
        }
    }

    /// (v325) Ouvre en ligne le monde joué dans `dir` (sa copie en ligne ; `fresh` : faite
    /// à l'instant), sous le nom de carte `name` : un nouveau serveur, ou (`how`) le monde
    /// endormi qu'on réveille, ou (v326) un nouveau serveur qui fait place à la limite.
    fn host_online(&mut self, dir: PathBuf, fresh: bool, name: String, how: Open) {
        // (Rien d'ouvert : une copie faite pour cette ouverture ne sert à rien.)
        let drop_fresh = |root: &std::path::Path, dir: &std::path::Path| {
            if fresh && online_copies::is_copy(root, dir) {
                let _ = std::fs::remove_dir_all(dir);
            }
        };
        let Some(link) = self.accounts.link.clone() else {
            drop_fresh(&self.paths.root, &dir);
            self.notify("Connecte-toi d'abord à ton compte.".to_string());
            return;
        };
        let Ok(p) = fpsmap::package(&dir) else {
            drop_fresh(&self.paths.root, &dir);
            self.notify("Monde illisible.".to_string());
            return;
        };
        // (v330) La carte part sans la graine ni la clé de terrain : le relais les garde (il
        // ne donne la clé qu'aux joueurs, et la graine qu'aux OP du serveur).
        let (p, seed, terrain) = fpsmap::without_seed(p, &dir);
        let sig = p.signature();
        self.world_key = None;
        self.wake_confirm = None;
        self.world_terrain = None;
        // (v330) Une copie sans clé de terrain (gardée par un invité devenu propriétaire) : la
        // partie ne s'ouvre qu'une fois le monde réveillé, avec la clé du relais
        // (`Event::Hosted`).
        let deferred = terrain == 0 && matches!(how, Open::Wake { .. });
        if !deferred {
            self.play_with(dir.clone(), true);
            if self.session.is_none() {
                // (Monde illisible : rien n'est ouvert en ligne.)
                drop_fresh(&self.paths.root, &dir);
                return;
            }
        } else {
            // (Ce que `play_with` fait d'abord : plus garant, avant d'entrer dans le monde.)
            if self.garant.take().is_some() {
                self.accounts.room(minelib_net::client::Cmd::Leave);
            }
            self.announce_garant(false);
            self.session = None;
            self.clear_meshes = true;
        }
        self.hosting = Some(Hosting {
            dir,
            fresh,
            upload: (sig, name.clone(), p.to_bytes()),
            replaced: match &how {
                Open::Replacing { world, .. } => Some(world.clone()),
                _ => None,
            },
            deferred,
        });
        let pseudo = self.accounts.pseudo();
        if !deferred {
            self.attach_host_net(link);
        }
        self.accounts.room(match how {
            Open::Wake { title, world } => minelib_net::client::Cmd::WakeWorld {
                name: title,
                map: name.clone(),
                map_sig: sig,
                world,
                seed,
                terrain,
            },
            Open::New => minelib_net::client::Cmd::Host {
                kind: 1,
                name: format!("{name} ({pseudo})"),
                map: name.clone(),
                map_sig: sig,
                max_players: 0,
                seed,
                terrain,
            },
            Open::Replacing { world, code } => minelib_net::client::Cmd::HostReplacing {
                name: format!("{name} ({pseudo})"),
                map: name.clone(),
                map_sig: sig,
                replace: world,
                code,
                seed,
                terrain,
            },
        });
        // (v229) La météo du serveur, et le monde (pour ceux qui le rejoignent, pas pour la
        // liste des cartes) : une fois ouvert (`Event::Hosted`).
    }

    /// (v325) La partie ouverte (ou réveillée) en ligne dont on est l'hôte : son mode, la voix
    /// et le réseau du monde de construction.
    fn attach_host_net(&mut self, link: std::sync::mpsc::Sender<minelib_net::transport::Inbound>) {
        let pseudo = self.accounts.pseudo();
        if let Some(s) = &mut self.session {
            if s.game.mode == save::GameMode::Fps {
                s.game.mode = save::GameMode::Creative;
            }
            s.voice = Some(voice::Voice::new(link.clone()));
            let mut b = buildnet::BuildNet::new(link, pseudo, true);
            b.set_node_dir(self.accounts.relay_dir());
            b.set_identity(self.accounts.identity.clone());
            // (v318) La partie compte depuis cette origine (loin du centre, proche du joueur).
            b.set_origin(s.world.origin_blocks());
            s.build = Some(b);
        }
    }

    /// Monde de construction rejoint.
    fn join_build(&mut self, dir: PathBuf, host: String) {
        let Some(link) = self.accounts.link.clone() else {
            return;
        };
        // Jamais dans un monde solo de ce PC (même identique) : le serveur a sa copie à
        // part, et ce qu'on y fait ne touche pas au monde solo (ni l'inverse).
        let maps = self.paths.root.join("maps");
        let dir = if dir.starts_with(&maps) {
            dir
        } else {
            let name = save::list_worlds(&self.paths.saves)
                .into_iter()
                .find(|w| w.dir == dir)
                .map_or_else(|| "Monde".into(), |w| w.meta.name);
            match fpsmap::package(&dir).and_then(|p| fpsmap::unpack(&maps, &name, &p.to_bytes())) {
                Ok(d) => d,
                Err(e) => {
                    log::warn!("copie du monde du serveur : {e:#}");
                    return;
                }
            }
        };
        // (v330) La clé de terrain que le relais vient de donner (pas dans la carte).
        self.play_with_terrain(dir, true, self.world_terrain);
        let pseudo = self.accounts.pseudo();
        if let Some(s) = &mut self.session {
            s.game.mode = save::GameMode::Creative;
            s.voice = Some(voice::Voice::new(link.clone()));
            let mut b = buildnet::BuildNet::new(link, pseudo, false);
            b.set_node_dir(self.accounts.relay_dir());
            b.set_identity(self.accounts.identity.clone());
            // (v318) La partie compte depuis cette origine (loin du centre, proche du joueur).
            b.set_origin(s.world.origin_blocks());
            s.build = Some(b);
            s.chat
                .say(format!("Monde de {host} rejoint."), chat::YELLOW);
        }
        // Mode de jeu par défaut du serveur (si l'on n'en est pas OP).
        self.accounts.room(minelib_net::client::Cmd::ServerAdmin {
            action: minelib_net::proto::SERVER_GAMEMODE_QUERY,
            pseudo: String::new(),
            reason: String::new(),
        });
        // (v328) Sa graine : verrouillée ou non, et ce qui vaut pour nous.
        self.accounts.room(minelib_net::client::Cmd::ServerAdmin {
            action: minelib_net::proto::SERVER_SEED,
            pseudo: String::new(),
            reason: String::new(),
        });
    }

    /// Réponses du relais, et options à enregistrer (clé du relais, dernier pseudo).
    fn poll_accounts(&mut self) {
        let relayed = self
            .session
            .as_mut()
            .and_then(|s| s.build.as_mut())
            .is_some_and(|b| b.take_relayed_beat());
        if relayed {
            self.accounts.relayed_beat();
        }
        // (v320) Au menu, sans partie : le lien avec le relais peut changer de connexion
        // (par des passeurs). Changé, ce qu'on avait dit à l'ancienne est redit.
        self.accounts.set_idle(
            self.session.is_none()
                && self.searching.is_none()
                && self.garant.is_none()
                && self.garant_worker.is_none(),
        );
        self.accounts.poll();
        if self.accounts.take_link_moved() {
            self.garant_announced = None;
        }
        let mut changed = false;
        if let Some(k) = self.accounts.new_relay_key.take() {
            self.options.relay_key = k;
            changed = true;
        }
        if let Some(p) = self.accounts.new_pseudo.take() {
            if self.options.pseudo != p {
                self.options.pseudo = p;
                changed = true;
            }
        }
        if changed {
            if let Err(e) = self.options.save(&self.paths.options) {
                log::error!("options : {e:#}");
            }
        }
    }

    fn multiplayer_screen(&mut self, input: &Input, size: (u32, u32)) {
        let mut c = Ctx::new(&mut self.ui, input, size);
        match multiplayer::mode_screen(&mut c) {
            multiplayer::Action::Open(multiplayer::Kind::Fps) => self.open_fps_home(),
            multiplayer::Action::Open(kind) => {
                self.server_selected = None;
                self.accounts.refresh_servers(kind as u8);
                self.screen = Screen::Servers(kind);
            }
            multiplayer::Action::Back => self.screen = Screen::Title,
            _ => {
                if input.pressed(KeyCode::Escape) {
                    self.screen = Screen::Title;
                }
            }
        }
    }

    fn servers_screen(&mut self, input: &Input, size: (u32, u32), kind: multiplayer::Kind) {
        self.servers = self
            .accounts
            .servers
            .iter()
            .map(|e| multiplayer::ServerInfo {
                name: e.name.clone(),
                map: e.map.clone(),
                players: e.players,
                max_players: e.max_players,
                ping: 0,
                region: e.region.clone(),
                peak: e.peak,
                active: e.active,
                asleep: e.asleep,
                keeper: e.keeper.clone(),
                can_wake: {
                    let me = self.accounts.pseudo();
                    !me.is_empty()
                        && (e.creator.eq_ignore_ascii_case(&me)
                            || e.host.eq_ignore_ascii_case(&me)
                            || e.keeper.eq_ignore_ascii_case(&me))
                },
            })
            .collect();
        let time = self.time;
        let mut c = Ctx::new(&mut self.ui, input, size);
        let action = multiplayer::list_screen(
            &mut c,
            input,
            kind,
            &self.servers,
            &mut self.server_selected,
            &mut self.server_search,
            time,
        );
        match action {
            multiplayer::Action::Refresh => self.accounts.refresh_servers(kind as u8),
            multiplayer::Action::Join(i) => {
                // (v325) Un monde endormi : « Réveiller ».
                if let Some(e) = self.accounts.servers.get(i).cloned().filter(|e| e.asleep) {
                    self.wake_world(&e);
                } else if let Some(e) = self.accounts.servers.get(i).cloned() {
                    if e.elsewhere() {
                        self.accounts.join_elsewhere(&e);
                    } else {
                        self.accounts.room(minelib_net::client::Cmd::Join(e.id));
                    }
                }
            }
            multiplayer::Action::Create => {
                self.refresh_worlds();
                self.screen = Screen::HostPick;
            }
            _ => {}
        }
        if matches!(action, multiplayer::Action::Back) || input.pressed(KeyCode::Escape) {
            self.screen = Screen::Multiplayer;
        }
    }

    fn solo_screen(&mut self, input: &Input, size: (u32, u32)) {
        let time = self.time;
        let mut c = Ctx::new(&mut self.ui, input, size);
        c.background();
        let s = c.s;
        c.text_center("Mondes", 12.0 * s, s * 2.0, WHITE);
        let list_w = 260.0 * s;
        let list_x = (c.w - list_w) / 2.0;
        // Recherche par nom, en haut de la liste.
        if c.text_field(
            "",
            &self.world_search.text,
            "rechercher un monde",
            list_x + list_w / 2.0,
            30.0 * s,
            list_w / s,
            self.world_search.focus,
            time,
        ) {
            self.world_search.focus = true;
        } else if c.click {
            self.world_search.focus = false;
        }
        if self.world_search.focus {
            menu::edit_text(&mut self.world_search.text, input, 32);
        }
        let query = self.world_search.text.trim().to_lowercase();
        let shown: Vec<usize> = (0..self.worlds.len())
            .filter(|&i| {
                query.is_empty() || self.worlds[i].meta.name.to_lowercase().contains(&query)
            })
            .collect();
        if self.selected_world.is_some_and(|k| !shown.contains(&k)) {
            self.selected_world = None;
        }
        let list_y = 50.0 * s;
        let row_h = 26.0 * s;
        let bottom = c.h - 54.0 * s;
        let list_h = (bottom - list_y).max(row_h);
        let visible = ((list_h - 4.0 * s) / row_h).floor().max(1.0) as usize;
        c.frame(list_x, list_y, list_w, list_h);

        if self.worlds.is_empty() {
            let small = (s / 2.0).floor().max(1.0);
            c.text_center(
                "Aucun monde pour l'instant : crée-en un !",
                list_y + 20.0 * s,
                small,
                GREY,
            );
        }
        self.world_scroll = self.world_scroll.min(shown.len().saturating_sub(visible));
        if input.scroll.abs() >= 1.0 && shown.len() > visible {
            let max = shown.len() - visible;
            let d = -input.scroll.signum() as i32;
            self.world_scroll = (self.world_scroll as i32 + d).clamp(0, max as i32) as usize;
        }
        let mut clicked = None;
        for (row, &k) in shown
            .iter()
            .enumerate()
            .skip(self.world_scroll)
            .take(visible)
        {
            let w = &self.worlds[k];
            let y = list_y + 2.0 * s + (row - self.world_scroll) as f32 * row_h;
            let detail = if w.minecraft {
                "Monde Minecraft   -   converti au lancement (originaux conservés)".to_string()
            } else {
                format!(
                    "graine {}   -   dernière partie {}",
                    w.meta.seed_label(),
                    save::format_date(w.meta.last_played)
                )
            };
            if c.list_row(
                list_x + 2.0 * s,
                y,
                list_w - 4.0 * s,
                row_h - s,
                &w.meta.name,
                &detail,
                self.selected_world == Some(k),
            ) {
                clicked = Some(k);
            }
        }
        let cx = c.w / 2.0;
        let by = c.h - 48.0 * s;
        let play = c.button(
            "Jouer",
            cx - 105.0 * s,
            by,
            150.0,
            self.selected_world.is_some(),
        );
        let new = c.button("Nouveau monde", cx + 80.0 * s, by, 150.0, true);
        // (v232) Cartes FPS (de ce PC et du relais) : contre des bots.
        let fps_maps = c.button("Cartes FPS", cx - 105.0 * s, by + 24.0 * s, 150.0, true);
        let back = c.button("Retour", cx + 80.0 * s, by + 24.0 * s, 150.0, true);

        let mut launch = None;
        if let Some(k) = clicked {
            // Double clic : jouer directement.
            if self.last_row_click.0 == Some(k) && time - self.last_row_click.1 < 0.4 {
                launch = Some(k);
            }
            self.selected_world = Some(k);
            self.last_row_click = (Some(k), time);
        }
        if play {
            launch = self.selected_world;
        }
        if let Some(k) = launch {
            let dir = self.worlds[k].dir.clone();
            if self.worlds[k].minecraft {
                self.pending_open = Some((dir, 0));
                self.screen = Screen::Loading;
            } else {
                self.play(dir);
            }
        }
        if fps_maps {
            self.open_solo_maps();
            return;
        }
        if new {
            self.new_name = format!("Nouveau monde {}", self.worlds.len() + 1);
            self.new_seed.clear();
            self.focus = Field::Name;
            self.screen = Screen::NewWorld;
        }
        if back || input.pressed(KeyCode::Escape) {
            self.screen = Screen::Title;
        }
    }

    fn new_world_screen(&mut self, input: &Input, size: (u32, u32)) {
        match self.focus {
            Field::Name => menu::edit_text(&mut self.new_name, input, 32),
            Field::Seed => menu::edit_text(&mut self.new_seed, input, 24),
        }
        if input.pressed(KeyCode::Tab) {
            self.focus = if self.focus == Field::Name {
                Field::Seed
            } else {
                Field::Name
            };
        }
        let time = self.time;
        let mut c = Ctx::new(&mut self.ui, input, size);
        c.background();
        let s = c.s;
        c.text_center("Nouveau monde", 12.0 * s, s * 2.0, WHITE);
        let cx = c.w / 2.0;
        let mut y = c.h * 0.3;
        if c.text_field(
            "Nom du monde",
            &self.new_name,
            "Mon monde",
            cx,
            y,
            220.0,
            self.focus == Field::Name,
            time,
        ) {
            self.focus = Field::Name;
        }
        y += 40.0 * s;
        if c.text_field(
            "Graine",
            &self.new_seed,
            "vide = aléatoire",
            cx,
            y,
            220.0,
            self.focus == Field::Seed,
            time,
        ) {
            self.focus = Field::Seed;
        }
        let by = c.h - 48.0 * s;
        let ok = !self.new_name.trim().is_empty();
        let create = c.button("Créer le monde", cx - 80.0 * s, by, 150.0, ok);
        let cancel = c.button("Annuler", cx + 80.0 * s, by, 150.0, true);
        if create || (ok && input.pressed(KeyCode::Enter)) {
            let seed = save::parse_seed(&self.new_seed);
            match save::create_world(&self.paths.saves, &self.new_name, seed) {
                Ok(dir) => self.play(dir),
                Err(e) => self.notify(format!("Création impossible : {e:#}")),
            }
        }
        if cancel || input.pressed(KeyCode::Escape) {
            self.screen = Screen::Solo;
        }
    }

    fn packs_screen(&mut self, input: &Input, size: (u32, u32), over_game: bool) {
        let current = self.current_pack.clone();
        let mut c = Ctx::new(&mut self.ui, input, size);
        if over_game {
            c.dim(0.7);
        } else {
            c.background();
        }
        let s = c.s;
        c.text_center("Textures", 12.0 * s, s * 2.0, WHITE);
        let list_w = 260.0 * s;
        let list_x = (c.w - list_w) / 2.0;
        let list_y = 36.0 * s;
        let row_h = 26.0 * s;
        let list_h = (c.h - 60.0 * s - list_y).max(row_h);
        c.frame(list_x, list_y, list_w, list_h);
        let visible = ((list_h - 4.0 * s) / row_h).floor().max(1.0) as usize;
        let mut chosen = None;
        for (k, p) in self.packs.iter().enumerate().take(visible) {
            let y = list_y + 2.0 * s + k as f32 * row_h;
            let active = p.name == current;
            let kind = if p.path.is_dir() {
                "dossier"
            } else {
                "archive .zip"
            };
            let detail = if active {
                format!("{kind}   -   actif")
            } else {
                format!("{kind}   -   cliquer pour l'utiliser")
            };
            if c.list_row(
                list_x + 2.0 * s,
                y,
                list_w - 4.0 * s,
                row_h - s,
                &p.name,
                &detail,
                active,
            ) && !active
            {
                chosen = Some(k);
            }
        }
        let cx = c.w / 2.0;
        let by = c.h - 48.0 * s;
        let open = c.button("Ouvrir le dossier", cx - 80.0 * s, by, 150.0, true);
        let refresh = c.button("Actualiser", cx + 80.0 * s, by, 150.0, true);
        let done = c.button("Terminé", cx, by + 24.0 * s, 150.0, true);
        if let Some(k) = chosen {
            self.pack_request = Some(self.packs[k].clone());
        }
        if open {
            open_folder(&self.paths.resourcepacks);
        }
        if refresh {
            self.packs = list_packs(&self.paths.resourcepacks);
        }
        if done || input.pressed(KeyCode::Escape) {
            self.screen = self.packs_return;
        }
    }

    fn loading_screen(&mut self, input: &Input, size: (u32, u32)) {
        let mut c = Ctx::new(&mut self.ui, input, size);
        c.background();
        let s = c.s;
        c.text_center(
            "Conversion du monde Minecraft...",
            c.h * 0.42,
            s * 2.0,
            WHITE,
        );
        let small = (s / 2.0).floor().max(1.0);
        c.text_center("Les fichiers d'origine ne sont pas modifiés ; la sauvegarde se fera au format Minelib.", c.h * 0.42 + 26.0 * s, small, GREY);
    }

    fn pause_menu(&mut self, input: &Input, size: (u32, u32)) {
        self.friends_refresh();
        let (in_world, note) = self.friends_context();
        let time = self.time;
        let mut c = Ctx::new(&mut self.ui, input, size);
        c.dim(0.55);
        let s = c.s;
        // (v233) Module d'amis à droite (comme celui du lobby FPS) : les inviter dans ce
        // monde, en rejoindre un ; le menu reste au milieu tant qu'il a la place.
        let pw = friends::panel_width(&c);
        let px = c.w - pw - 6.0 * s;
        let rect = (px, 8.0 * s, pw, c.h - 16.0 * s);
        let friends_action = Self::friends_panel(
            &mut c,
            input,
            &mut self.friends_ui,
            &self.accounts,
            in_world,
            note.as_ref().map(|(n, p)| (n.as_str(), p.as_deref())),
            rect,
            time,
        );
        let cx = (c.w / 2.0).min(px - 126.0 * s);
        let title = "Jeu en pause";
        let tw = Ui::text_width(title, s * 2.0);
        c.ui.text(cx - tw / 2.0, c.h * 0.2, s * 2.0, WHITE, title);
        let mut y = c.h * 0.36;
        let resume = c.button("Reprendre", cx, y, 200.0, true);
        y += 24.0 * s;
        let settings = c.button("Paramètres", cx, y, 200.0, true);
        // Serveur de construction : une copie du monde tel qu'il est, dans ses mondes solo.
        // (v328) Pas quand ses OP ont verrouillé la graine du monde (par défaut : seuls eux).
        let on_build = self.session.as_ref().is_some_and(|s| s.build.is_some());
        let may_download = self.session.as_ref().is_some_and(|s| s.seed_allowed());
        let download = on_build && {
            y += 24.0 * s;
            c.button("Télécharger la carte", cx, y, 200.0, may_download)
        };
        let locked_note = (on_build && !may_download)
            .then(|| "Carte verrouillée par les OP du serveur (/seed lock).".to_string());
        if let Some(t) = locked_note
            .as_ref()
            .or(self.download_note.as_ref())
            .filter(|_| on_build)
        {
            // (v328) Sous le bouton (haut de 20), plus sur son bas.
            let tw = Ui::text_width(t, s);
            c.ui.text(cx - tw / 2.0, y + 23.0 * s, s, GREY, t.clone());
        }
        // Partie FPS en ligne : votes d'équipe (comme dans CS).
        let choices = self.session.as_ref().and_then(|s| s.vote_choices());
        let mut vote: Option<(u8, u64)> = None;
        if let Some((mates, timeouts, can_pause)) = &choices {
            y += 24.0 * s;
            let label = format!("Proposer une pause tactique ({timeouts} / 3)");
            if c.button(&label, cx, y, 240.0, *can_pause && *timeouts > 0) {
                vote = Some((minelib_net::game::VOTE_PAUSE, u64::MAX));
            }
            y += 24.0 * s;
            if c.button(
                "Proposer d'exclure un coéquipier",
                cx,
                y,
                240.0,
                !mates.is_empty(),
            ) {
                self.kick_pick = !self.kick_pick;
            }
            if self.kick_pick {
                for (id, name) in mates {
                    y += 22.0 * s;
                    if c.button(&format!("Exclure {name}"), cx, y, 160.0, true) {
                        vote = Some((minelib_net::game::VOTE_KICK, *id));
                    }
                }
            }
            // Abandon : toute l'équipe doit dire oui (l'autre équipe gagne).
            y += 24.0 * s;
            if c.button("Proposer d'abandonner la partie", cx, y, 240.0, *can_pause) {
                vote = Some((minelib_net::game::VOTE_SURRENDER, u64::MAX));
            }
        }
        let online = self
            .session
            .as_ref()
            .is_some_and(|s| s.ranked_match() || s.build.as_ref().is_some_and(|b| !b.is_owner()));
        let label = if online {
            "Quitter la partie"
        } else {
            "Sauvegarder et quitter"
        };
        let quit = c.button(label, cx, (c.h * 0.72).max(y + 40.0 * s), 200.0, true);
        let (line, color) = self.updater.line();
        c.ui.text(4.0 * s, c.h - 12.0 * s, s, color, line);
        if let Some((kind, target)) = vote {
            self.kick_pick = false;
            if let Some(sess) = &mut self.session {
                sess.call_vote(kind, target);
                sess.keys.mouse_muted = true;
            }
            self.screen = Screen::Playing;
        }
        if resume {
            self.kick_pick = false;
            self.screen = Screen::Playing;
            if let Some(sess) = &mut self.session {
                sess.keys.mouse_muted = true;
            }
        }
        if settings {
            self.settings_return = Screen::Paused;
            self.screen = Screen::Settings;
        }
        self.friends_act(friends_action);
        if download {
            let saves = self.paths.saves.clone();
            if let Some(sess) = &mut self.session {
                self.download_note = Some(match sess.download_copy(&saves) {
                    Ok(dir) => {
                        log::info!("carte copiée dans {}", dir.display());
                        "Copie enregistrée dans tes mondes solo.".into()
                    }
                    Err(e) => format!("Copie impossible : {e:#}"),
                });
            }
        }
        if quit {
            self.download_note = None;
            self.leave_world();
        }
    }

    fn settings_screen(&mut self, input: &Input, size: (u32, u32), over_game: bool) {
        let mut c = Ctx::new(&mut self.ui, input, size);
        if over_game {
            c.dim(0.7);
        } else {
            c.background();
        }
        let s = c.s;
        c.text_center("Paramètres", c.h * 0.18, s * 2.0, WHITE);
        let cx = c.w / 2.0;
        let mut y = c.h * 0.36;
        let graphics = c.button("Paramètres graphiques", cx, y, 200.0, true);
        y += 24.0 * s;
        let controls = c.button("Régler les touches", cx, y, 200.0, true);
        y += 24.0 * s;
        let packs = c.button("Textures", cx, y, 200.0, true);
        y += 24.0 * s;
        // Langue : « < Français > » ; les flèches (ou un clic au milieu) passent d'une langue
        // à l'autre. Chaque langue est écrite dans sa langue (jamais traduite).
        let n = i18n::LANGS.len();
        let cur = i18n::current().min(n.saturating_sub(1));
        let lang = i18n::LANGS.get(cur).map_or("Français", |l| l.1);
        let prev = c.button("<", cx - 100.0 * s + 11.0 * s, y, 22.0, n > 1);
        let mid = c.button(&format!("Langue : {lang}"), cx, y, 152.0, n > 1);
        let next = c.button(">", cx + 100.0 * s - 11.0 * s, y, 22.0, n > 1);
        let mut picked = None;
        if n > 1 && (prev || next || mid) {
            let k = if prev {
                (cur + n - 1) % n
            } else {
                (cur + 1) % n
            };
            picked = Some(i18n::LANGS[k].0);
        }
        y += 24.0 * s;
        // Son : sortie, micro et volumes.
        let sound = c.button("Son", cx, y, 200.0, true);
        y += 24.0 * s;
        // (v307) Journal réseau : partage et réception.
        let advanced = c.button("Paramètres avancés", cx, y, 200.0, true);
        y += 30.0 * s;
        let done = c.button("Terminé", cx, (c.h * 0.72).max(y), 200.0, true);
        // Skin : aperçu en 3D qui tourne, à droite, et le bouton pour en changer.
        let mut want_pick = false;
        let px = (c.w * 0.82).max(cx + 130.0 * s);
        if px + 40.0 * s < c.w {
            let ph = (c.h * 0.42).min(150.0 * s);
            let py = c.h * 0.22;
            c.frame(px - 45.0 * s, py - 6.0 * s, 90.0 * s, ph + 60.0 * s);
            let yaw = self.time * 0.8;
            c.ui.model(skin::preview(
                self.my_skin.clone(),
                px,
                py + ph / 2.0,
                ph,
                yaw,
            ));
            let busy = self.skin_pick.is_some();
            want_pick = c.button("Changer de skin", px, py + ph + 8.0 * s, 84.0, !busy);
            if self.my_skin.is_some()
                && c.button("Retirer le skin", px, py + ph + 30.0 * s, 84.0, !busy)
            {
                let _ = std::fs::remove_file(self.paths.root.join("skin.png"));
                self.my_skin = None;
                if let Some(sess) = &mut self.session {
                    sess.skins.set_mine(None);
                }
                self.skin_sent = false;
            }
            if let Some((m, ok)) = &self.skin_msg {
                let col = if *ok { GOLD } else { [1.0, 0.45, 0.4, 1.0] };
                let t = crate::menu::fit(m, 120.0 * s, (s / 2.0).floor().max(1.0));
                let tw = Ui::text_width(&t, (s / 2.0).floor().max(1.0));
                c.ui.text(
                    px - tw / 2.0,
                    py + ph + 52.0 * s,
                    (s / 2.0).floor().max(1.0),
                    col,
                    t,
                );
            }
        }
        if want_pick {
            self.pick_skin();
        }
        if let Some(code) = picked {
            i18n::set(code);
            self.options.language = code.to_string();
            let _ = self.options.save(&self.paths.options);
            return;
        }
        if graphics {
            self.screen = Screen::Graphics;
        }
        if advanced {
            self.screen = Screen::Advanced;
            self.direct_confirm = false;
        }
        if sound {
            // (v215) Le dernier relevé tout de suite (le refaire prend jusqu'à une seconde
            // sous Windows : l'écran s'ouvrait après), et un neuf en arrière-plan.
            self.sound_devices = self.sound_scan.get();
            self.sound_scan.refresh();
            self.sound_scan_at = self.time;
            self.sound_scroll = [0; 2];
            self.screen = Screen::Sound;
        }
        if controls {
            self.sens_text = format_sens(if self.keys_tab_fps {
                self.options.sensitivity_fps
            } else {
                self.options.sensitivity
            });
            self.sens_focus = false;
            self.screen = Screen::Controls;
        }
        if packs {
            self.packs = list_packs(&self.paths.resourcepacks);
            self.packs_return = Screen::Settings;
            self.screen = Screen::Packs;
        }
        if done || input.pressed(KeyCode::Escape) {
            self.slider_drag = None;
            if let Err(e) = self.options.save(&self.paths.options) {
                log::error!("options : {e:#}");
            }
            self.screen = self.settings_return;
        }
    }

    /// (v311) `/deleteworld` : le créateur d'un monde en ligne confirme d'un clic qu'il le
    /// supprime ; si son compte a la double authentification, le relais demande un code,
    /// tapé ici. Le relais vérifie que c'est bien le créateur.
    fn delete_world_screen(&mut self, input: &Input, size: (u32, u32)) {
        let time = self.time;
        if self.delete_code_needed {
            menu::edit_text(&mut self.delete_code, input, 6);
            self.delete_code.retain(|c| c.is_ascii_digit());
        }
        let mut c = Ctx::new(&mut self.ui, input, size);
        c.dim(0.7);
        let s = c.s;
        let ds = (s / 2.0).floor().max(1.0);
        c.text_center("Supprimer ce monde ?", c.h * 0.2, s * 2.0, WHITE);
        let cx = c.w / 2.0;
        let mut y = c.h * 0.32;
        // (v326) La copie en ligne de ceux qui y sont est effacée (v325) : dit ici.
        for line in [
            "Tous ses joueurs en sortent, et le relais oublie ses OP, ses bannis, sa liste blanche",
            "et les positions de ses joueurs. C'est définitif. La copie en ligne de ceux qui y sont",
            "(la tienne aussi) est effacée ; ton monde solo d'origine n'est pas touché. Pour en",
            "garder une copie dans tes mondes solo : « Télécharger la carte » (menu Échap), avant.",
        ] {
            c.text_center(line, y, ds, GREY);
            y += 7.0 * ds;
        }
        y += 22.0 * s;
        if self.delete_code_needed {
            c.text_field(
                "Code de double authentification",
                &self.delete_code,
                "6 chiffres",
                cx,
                y,
                120.0,
                true,
                time,
            );
            y += 30.0 * s;
        }
        let ready = !self.delete_code_needed || self.delete_code.len() == 6;
        let confirm = c.button("Supprimer définitivement", cx, y, 200.0, ready);
        y += 24.0 * s;
        let cancel = c.button("Annuler", cx, y, 200.0, true);
        if confirm {
            let code = self
                .delete_code
                .parse::<u32>()
                .ok()
                .filter(|_| self.delete_code_needed);
            self.accounts
                .room(minelib_net::client::Cmd::DeleteWorld(code));
            self.delete_code.clear();
            self.screen = Screen::Playing;
        } else if cancel || input.pressed(KeyCode::Escape) {
            self.delete_code.clear();
            self.screen = Screen::Playing;
        }
    }

    /// (v326) Nouveau monde en ligne alors que le joueur en a déjà le nombre permis : il
    /// confirme la suppression de son plus ancien monde endormi, avec la même protection que
    /// `/deleteworld` (un code si son compte a la double authentification). Le relais vérifie.
    fn world_limit_screen(&mut self, input: &Input, size: (u32, u32)) {
        let Some(ask) = &mut self.limit_ask else {
            self.screen = Screen::Servers(multiplayer::Kind::Build);
            return;
        };
        let time = self.time;
        if ask.totp {
            menu::edit_text(&mut ask.code, input, 6);
            ask.code.retain(|c| c.is_ascii_digit());
        }
        let mut c = Ctx::new(&mut self.ui, input, size);
        c.background();
        let s = c.s;
        let ds = (s / 2.0).floor().max(1.0);
        let title = format!(
            "Tu as déjà {} mondes en ligne",
            minelib_net::proto::WORLDS_PER_CREATOR
        );
        c.text_center(&title, c.h * 0.2, s * 2.0, WHITE);
        let cx = c.w / 2.0;
        let mut y = c.h * 0.32;
        let first = format!(
            "Pour ouvrir celui-ci, ton plus ancien monde endormi, « {} », est supprimé :",
            ask.name
        );
        for line in [
            first.as_str(),
            "le relais oublie ses OP, ses bannis, sa liste blanche et les positions de ses joueurs,",
            "et sa copie en ligne sur ce PC est effacée. C'est définitif. Ton monde solo d'origine",
            "n'est pas touché. (Pour garder ce monde-là, réveille-le plutôt, et ferme-en un autre.)",
        ] {
            c.text_center(line, y, ds, GREY);
            y += 7.0 * ds;
        }
        y += 22.0 * s;
        if ask.totp {
            c.text_field(
                "Code de double authentification",
                &ask.code,
                "6 chiffres",
                cx,
                y,
                120.0,
                true,
                time,
            );
            if ask.wrong {
                c.text_center("Code refusé : réessaie.", y + 14.0 * s, ds, RED);
            }
            y += 30.0 * s;
        }
        let ready = !ask.totp || ask.code.len() == 6;
        let label = format!("Supprimer « {} » et ouvrir", ask.name);
        let confirm = c.button(&label, cx, y, 260.0, ready);
        y += 24.0 * s;
        let cancel = c.button("Annuler", cx, y, 260.0, true);
        if confirm {
            let code = ask.code.parse::<u32>().ok().filter(|_| ask.totp);
            ask.sent_code = code.is_some();
            ask.code.clear();
            let (dir, fresh, map, world) =
                (ask.dir.clone(), ask.fresh, ask.map.clone(), ask.world.clone());
            self.host_online(dir, fresh, map, Open::Replacing { world, code });
        } else if cancel || input.pressed(KeyCode::Escape) {
            if let Some(ask) = self.limit_ask.take() {
                if ask.fresh && online_copies::is_copy(&self.paths.root, &ask.dir) {
                    let _ = std::fs::remove_dir_all(&ask.dir);
                }
            }
            self.screen = Screen::Servers(multiplayer::Kind::Build);
            self.accounts.refresh_servers(multiplayer::Kind::Build as u8);
        }
    }

    /// (v307) Paramètres → Paramètres avancés. Deux réglages du journal réseau : le
    /// partager avec le relais (la question posée une fois au menu d'accueil), et, quand ce
    /// PC est le relais, recevoir ceux des joueurs qui les partagent.
    fn advanced_screen(&mut self, input: &Input, size: (u32, u32), over_game: bool) {
        if self.direct_confirm {
            self.direct_confirm_screen(input, size, over_game);
            return;
        }
        let mut c = Ctx::new(&mut self.ui, input, size);
        if over_game {
            c.dim(0.7);
        } else {
            c.background();
        }
        let s = c.s;
        let ds = (s / 2.0).floor().max(1.0);
        c.text_center("Paramètres avancés", c.h * 0.18, s * 2.0, WHITE);
        let cx = c.w / 2.0;
        let mut y = c.h * 0.28;
        let share = self.options.share_netlog == Some(true);
        let label = if share {
            "Partager mon journal réseau : oui"
        } else {
            "Partager mon journal réseau : non"
        };
        let flip_share = c.button(label, cx, y, 300.0, true);
        y += 23.0 * s;
        for line in [
            "Des lignes sur l'état de ton réseau (débits, retards, tunnels), sans pseudo ni adresse,",
            "envoyées au relais auquel tu es connecté, si son hôte a demandé à les recevoir.",
        ] {
            c.text_center(line, y, ds, GREY);
            y += 7.0 * ds;
        }
        y += 12.0 * s;
        let recv = self.options.receive_netlogs;
        let label = if recv {
            "Recevoir les journaux réseau des joueurs : oui"
        } else {
            "Recevoir les journaux réseau des joueurs : non"
        };
        let flip_recv = c.button(label, cx, y, 300.0, true);
        y += 23.0 * s;
        for line in [
            "Quand ce PC est le relais : les journaux des joueurs qui les partagent, le relevé du relais",
            "et ses alertes (tentatives de le faire tomber) sont rangés dans journaux_reseau.txt.",
        ] {
            c.text_center(line, y, ds, GREY);
            y += 7.0 * ds;
        }
        y += 12.0 * s;
        // (v308) Passeur.
        let pass = self.options.passeur == Some(true);
        let label = if pass {
            "Relayer pour les autres joueurs : oui"
        } else {
            "Relayer pour les autres joueurs : non"
        };
        let flip_pass = c.button(label, cx, y, 300.0, true);
        y += 23.0 * s;
        for line in [
            "Ton jeu fait passer des paquets scellés de parties d'autres joueurs, sans pouvoir les lire.",
            "C'est ce qui donne le 256 tick en FPS ; sans assez de joueurs qui relaient, souvent 128.",
            "Ceux à qui ton jeu remet un paquet voient ton adresse IP, jamais ton pseudo.",
            "Qui relaie est servi en premier. Ta box doit ouvrir un port : le jeu le lui demande tout seul.",
        ] {
            c.text_center(line, y, ds, GREY);
            y += 7.0 * ds;
        }
        y += 12.0 * s;
        // (v327) Liens directs (`hote_direct`), avec ce qu'ils montrent.
        let direct = self.options.host_direct == Some(true);
        let label = if direct {
            "Liens directs avec les autres joueurs : oui"
        } else {
            "Liens directs avec les autres joueurs : non"
        };
        let flip_direct = c.button(label, cx, y, 300.0, true);
        y += 23.0 * s;
        for line in [
            "Oui : ton jeu se relie en direct aux joueurs qui l'ont mis à oui eux aussi. Vous voyez alors",
            "vos adresses IP ; si ton PC sert une région ou un relais, ils savent que c'est toi.",
            "Pas besoin pour le 256 tick : ce sont les joueurs qui relaient qui le donnent. Pour tous les",
            "autres, rien ne change. Pris en compte au prochain lancement du jeu.",
        ] {
            c.text_center(line, y, ds, if direct { WHITE } else { GREY });
            y += 7.0 * ds;
        }
        y += 14.0 * s;
        let done = c.button("Terminé", cx, (c.h * 0.78).max(y), 200.0, true);
        // (v329) Les mettre à oui se confirme (ce que ça montre, redit) ; les ôter, non.
        if flip_direct && !direct {
            self.direct_confirm = true;
            return;
        }
        if flip_direct {
            self.options.host_direct = Some(false);
            minelib_p2p::set_hosts_direct(Some(false));
        }
        if flip_pass {
            self.options.passeur = Some(!pass);
            minelib_net::fast::set_passeur_allowed(!pass);
        }
        if flip_share {
            self.options.share_netlog = Some(!share);
            minelib_net::netlog::set_share(!share);
        }
        if flip_recv {
            self.options.receive_netlogs = !recv;
            minelib_relay::set_receive_logs(!recv);
        }
        if flip_share
            || flip_recv
            || flip_pass
            || flip_direct
            || done
            || input.pressed(KeyCode::Escape)
        {
            if let Err(e) = self.options.save(&self.paths.options) {
                log::error!("options : {e:#}");
            }
        }
        if done || input.pressed(KeyCode::Escape) {
            self.screen = Screen::Settings;
        }
    }

    /// (v329) Confirmation des liens directs : ce qu'ils montrent, puis « Activer » ou
    /// « Annuler » (Échap aussi). Rien ne change sans « Activer ».
    fn direct_confirm_screen(&mut self, input: &Input, size: (u32, u32), over_game: bool) {
        let mut c = Ctx::new(&mut self.ui, input, size);
        if over_game {
            c.dim(0.8);
        } else {
            c.background();
        }
        let s = c.s;
        let ds = (s / 2.0).floor().max(1.0);
        let cx = c.w / 2.0;
        c.text_center("Activer les liens directs ?", c.h * 0.22, s * 2.0, WHITE);
        let mut y = c.h * 0.34;
        for line in [
            "Ton jeu se reliera en direct aux joueurs qui les ont activés eux aussi.",
            "Eux verront ton adresse IP, et toi la leur : elle dit à peu près où tu habites (ville,",
            "fournisseur d'accès), et permet de viser ta connexion (par exemple pour la saturer).",
            "Quand ton PC sert une région, une province ou un relais, ceux qui y jouent en direct",
            "sauront que c'est toi. Pour tous les autres joueurs, rien ne change.",
            "Pris en compte au prochain lancement du jeu ; tu peux les retirer ici à tout moment.",
        ] {
            c.text_center(line, y, ds, GREY);
            y += 7.0 * ds;
        }
        y += 20.0 * s;
        let yes = c.button("Activer les liens directs", cx, y, 240.0, true);
        let no = c.button("Annuler", cx, y + 24.0 * s, 240.0, true);
        if yes {
            self.options.host_direct = Some(true);
            minelib_p2p::set_hosts_direct(Some(true));
            if let Err(e) = self.options.save(&self.paths.options) {
                log::error!("options : {e:#}");
            }
            self.direct_confirm = false;
        } else if no || input.pressed(KeyCode::Escape) {
            self.direct_confirm = false;
        }
    }

    /// Paramètres → Son. En haut, deux listes : la sortie son et le micro (« Périphérique
    /// par défaut » d'abord, puis ceux du système). Sous la sortie, le volume du jeu ; sous
    /// le micro, son volume. En dessous, le volume des écrans vidéo et, à sa droite, celui
    /// du tchat vocal entrant.
    fn sound_screen(&mut self, input: &Input, size: (u32, u32), over_game: bool) {
        // (v215) Listes tenues à jour en arrière-plan (un casque branché apparaît), sans
        // jamais faire attendre l'écran.
        if self.time - self.sound_scan_at > 2.0 {
            self.sound_scan.refresh();
            self.sound_scan_at = self.time;
        }
        self.sound_devices = self.sound_scan.get();
        let mut c = Ctx::new(&mut self.ui, input, size);
        if over_game {
            c.dim(0.7);
        } else {
            c.background();
        }
        let s = c.s;
        c.text_center("Son", c.h * 0.08, s * 2.0, WHITE);
        let cx = c.w / 2.0;
        // Deux colonnes de 200 (pixels de texture), 10 d'écart.
        let col = [cx - 105.0 * s, cx + 105.0 * s];
        let list_w = 200.0 * s;
        let row_h = 13.0 * s;
        let rows_shown = 5;
        let top = c.h * 0.08 + 30.0 * s;
        let list_h = row_h * rows_shown as f32 + 4.0 * s;
        let mut chosen: [Option<String>; 2] = [None, None];
        for k in 0..2 {
            let (title, devices, current) = if k == 0 {
                (
                    "Sortie (haut-parleurs, casque)",
                    &self.sound_devices.0,
                    &self.options.audio_out,
                )
            } else {
                (
                    "Entrée (micro)",
                    &self.sound_devices.1,
                    &self.options.audio_in,
                )
            };
            let x = col[k] - list_w / 2.0;
            c.ui.text(x, top - 11.0 * s, s, GREY, title);
            c.frame(x, top, list_w, list_h);
            // « Périphérique par défaut » (nom vide), puis ceux du système.
            let names: Vec<&str> = std::iter::once("")
                .chain(devices.iter().map(String::as_str))
                .collect();
            let inner_w = list_w - c.bar_w() - 6.0 * s;
            let over = c.hovered(x, top, list_w, list_h);
            Ctx::wheel(
                input,
                over,
                names.len(),
                rows_shown,
                &mut self.sound_scroll[k],
            );
            for (i, name) in names
                .iter()
                .enumerate()
                .skip(self.sound_scroll[k])
                .take(rows_shown)
            {
                let ry = top + 2.0 * s + (i - self.sound_scroll[k]) as f32 * row_h;
                let rx = x + 2.0 * s;
                let selected = *name == current.as_str();
                let hover = c.hovered(rx, ry, inner_w, row_h);
                if selected {
                    c.ui.rect(rx, ry, inner_w, row_h, [0.85, 0.85, 0.9, 0.25]);
                } else if hover {
                    c.ui.rect(rx, ry, inner_w, row_h, [1.0, 1.0, 1.0, 0.08]);
                }
                let label = if name.is_empty() {
                    "Périphérique par défaut".to_string()
                } else {
                    name.to_string()
                };
                let color = if selected { GOLD } else { WHITE };
                let text = crate::menu::fit(&label, inner_w - 6.0 * s, s);
                c.ui.text(rx + 3.0 * s, ry + (row_h - 8.0 * s) / 2.0, s, color, text);
                if hover && c.click && !selected {
                    chosen[k] = Some(name.to_string());
                }
            }
            let bx = x + list_w - c.bar_w() - 2.0 * s;
            let mut drag = self.sound_drag[k];
            c.scrollbar(
                bx,
                top + 2.0 * s,
                list_h - 4.0 * s,
                names.len(),
                rows_shown,
                &mut self.sound_scroll[k],
                &mut drag,
            );
            self.sound_drag[k] = drag;
        }
        // Sous la sortie : le volume du jeu ; sous l'entrée : celui du micro.
        let y = top + list_h + 10.0 * s;
        let vol = self.options.volume;
        let label = format!("Volume du jeu : {} %", (vol * 100.0).round());
        let game_vol = c.slider(7, &mut self.slider_drag, &label, vol, col[0], y, 200.0);
        let mic = self.options.volume_mic;
        let label = format!("Volume du micro : {} %", (mic * 100.0).round());
        let mic_vol = c.slider(
            20,
            &mut self.slider_drag,
            &label,
            mic / 2.0,
            col[1],
            y,
            200.0,
        );
        // En dessous : les écrans vidéo, et à droite le tchat vocal entrant.
        let y2 = y + 30.0 * s;
        let scr = self.options.volume_screens;
        let label = format!("Volume des écrans : {} %", (scr * 100.0).round());
        let screens_vol = c.slider(21, &mut self.slider_drag, &label, scr, col[0], y2, 200.0);
        let vc = self.options.volume_voice;
        let label = format!("Tchat vocal entrant : {} %", (vc * 100.0).round());
        let voice_vol = c.slider(
            22,
            &mut self.slider_drag,
            &label,
            vc / 2.0,
            col[1],
            y2,
            200.0,
        );
        // (v234) En dessous, à gauche : la météo (pluie et tonnerre).
        let y3 = y2 + 30.0 * s;
        let wv = self.options.volume_weather;
        let label = format!("Volume de la météo : {} %", (wv * 100.0).round());
        let weather_vol = c.slider(23, &mut self.slider_drag, &label, wv, col[0], y3, 200.0);
        // (v179) En dessous : couper le son des écrans quand on ouvre le menu du jeu.
        let y3 = y3 + 30.0 * s;
        let mute_label = format!(
            "Couper le son des écrans dans le menu : {}",
            if self.options.mute_screens_menu {
                "oui"
            } else {
                "non"
            }
        );
        let mute_click = c.button(&mute_label, cx, y3, 410.0, true);
        c.text_center(
            "Oui : en solo, les vidéos se mettent en pause ; en ligne, le son est coupé tant que le menu est ouvert.",
            y3 + 24.0 * s,
            (s / 2.0).floor().max(1.0),
            GREY,
        );
        let done = c.button("Terminé", cx, (y3 + 40.0 * s).max(c.h * 0.84), 200.0, true);
        if mute_click {
            self.options.mute_screens_menu = !self.options.mute_screens_menu;
        }
        if let Some(name) = chosen[0].take() {
            crate::audio::set_output(&name);
            self.options.audio_out = name;
            crate::audio::play(crate::audio::Sfx::Dry, 0.0, 1.0);
        }
        if let Some(name) = chosen[1].take() {
            crate::voice::set_input(&name);
            self.options.audio_in = name;
        }
        if let Some(f) = game_vol {
            let v = (f * 20.0).round() / 20.0;
            if v != self.options.volume {
                self.options.volume = v;
                if let Some(sess) = &mut self.session {
                    sess.volume = v;
                }
                // (v215) Tout de suite, sons en cours compris.
                crate::audio::set_master(v);
                crate::audio::play(crate::audio::Sfx::Dry, 0.0, 1.0);
            }
        }
        if let Some(f) = mic_vol {
            let v = (f * 2.0 * 20.0).round() / 20.0;
            if v != self.options.volume_mic {
                self.options.volume_mic = v;
                crate::voice::set_mic_gain(v);
            }
        }
        if let Some(f) = screens_vol {
            let v = (f * 20.0).round() / 20.0;
            if v != self.options.volume_screens {
                self.options.volume_screens = v;
                if let Some(sess) = &mut self.session {
                    sess.set_screens_volume(v);
                }
            }
        }
        if let Some(f) = weather_vol {
            let v = (f * 20.0).round() / 20.0;
            if v != self.options.volume_weather {
                self.options.volume_weather = v;
                // Tout de suite, la pluie en cours comprise.
                crate::audio::set_weather_volume(v);
            }
        }
        if let Some(f) = voice_vol {
            let v = (f * 2.0 * 20.0).round() / 20.0;
            if v != self.options.volume_voice {
                self.options.volume_voice = v;
                if let Some(sess) = &mut self.session {
                    sess.voice_volume = v;
                }
            }
        }
        if done || input.pressed(KeyCode::Escape) {
            self.slider_drag = None;
            self.sound_drag = [false; 2];
            if let Err(e) = self.options.save(&self.paths.options) {
                log::error!("options : {e:#}");
            }
            self.screen = Screen::Settings;
        }
    }

    /// Réglage des commandes : sensibilité de la souris (valeurs Counter-Strike), par curseur
    /// ou en tapant la valeur dans le champ à droite.
    fn controls_screen(&mut self, input: &Input, size: (u32, u32), over_game: bool) {
        let time = self.time;
        // Sensibilité de l'onglet affiché : construction ou FPS, chacune à part.
        let fps_sens = self.keys_tab_fps;
        let mut sens = if fps_sens {
            self.options.sensitivity_fps
        } else {
            self.options.sensitivity
        };
        let mut c = Ctx::new(&mut self.ui, input, size);
        if over_game {
            c.dim(0.7);
        } else {
            c.background();
        }
        let s = c.s;
        c.text_center("Régler les touches", c.h * 0.06, s * 2.0, WHITE);
        let cx = c.w / 2.0;
        // Deux onglets : Build (modes de construction) et FPS (touches de CS:GO).
        let tab_y = c.h * 0.06 + 22.0 * s;
        let fps_tab = self.keys_tab_fps;
        let (build_label, fps_label) = if fps_tab {
            ("Build", "> FPS <")
        } else {
            ("> Build <", "FPS")
        };
        if c.button(build_label, cx - 52.0 * s, tab_y, 100.0, true) {
            self.keys_tab_fps = false;
            self.keys_scroll = 0;
            self.binding = None;
            self.sens_text = format_sens(self.options.sensitivity);
            self.sens_focus = false;
        }
        if c.button(fps_label, cx + 52.0 * s, tab_y, 100.0, true) {
            self.keys_tab_fps = true;
            self.keys_scroll = 0;
            self.binding = None;
            self.sens_text = format_sens(self.options.sensitivity_fps);
            self.sens_focus = false;
        }
        let y = tab_y + 26.0 * s;
        // Curseur 0,1 à 8 (plage du jeu Counter-Strike) ; le champ accepte 0,01 à 20.
        let (lo, hi) = (0.1f32, 8.0f32);
        let label = format!(
            "Sensibilité souris ({}) : {}",
            if fps_sens { "FPS" } else { "construction" },
            format_sens(sens)
        );
        let frac = (sens.clamp(lo, hi) - lo) / (hi - lo);
        if let Some(f) = c.slider(
            3,
            &mut self.slider_drag,
            &label,
            frac,
            cx - 34.0 * s,
            y,
            230.0,
        ) {
            sens = ((lo + f * (hi - lo)) * 100.0).round() / 100.0;
            self.sens_text = format_sens(sens);
            self.sens_focus = false;
        }
        let field_clicked = c.text_field(
            "",
            &self.sens_text,
            "1.25",
            cx + 116.0 * s,
            y,
            60.0,
            self.sens_focus,
            time,
        );
        if field_clicked {
            self.sens_focus = true;
            self.binding = None;
        } else if c.click && self.sens_focus {
            self.sens_focus = false; // clic ailleurs : on valide
        }
        let ds = (s / 2.0).floor().max(1.0);
        c.text_center(
            "Valeurs de Counter-Strike : 0,022° par unité de souris × sensibilité.",
            y + 23.0 * s,
            ds,
            GREY,
        );

        // En attente (depuis une image précédente) : la prochaine touche ou le prochain clic
        // est assigné (Échap annule). Un clic ainsi pris n'active aucun bouton.
        let mut cancel_escape = false;
        if let Some(action) = self.binding {
            if input.pressed(KeyCode::Escape) {
                self.binding = None;
                cancel_escape = true;
            } else if let Some(k) = input
                .pressed_keys()
                .into_iter()
                .find(|k| keys::is_bindable(*k))
            {
                self.keys.bind(action, keys::Bind::Key(k));
                self.binding = None;
            } else if let Some(b) = input.mouse_pressed_any() {
                self.keys.bind(action, keys::Bind::Mouse(b));
                self.binding = None;
                c.click = false;
            }
        }

        // Onglet FPS : marcher et s'accroupir en « maintenir » ou « bascule ».
        let mut top = y + 34.0 * s;
        if self.keys_tab_fps {
            let mode = |t: bool| if t { "bascule" } else { "maintenir" };
            let walk = format!("Marcher : {}", mode(self.keys.walk_toggle));
            let duck = format!("S'accroupir : {}", mode(self.keys.duck_toggle));
            if c.button(&walk, cx - 124.0 * s, top, 240.0, true) {
                self.keys.walk_toggle = !self.keys.walk_toggle;
            }
            if c.button(&duck, cx + 124.0 * s, top, 240.0, true) {
                self.keys.duck_toggle = !self.keys.duck_toggle;
            }
            top += 24.0 * s;
        }

        // Une ligne par action : nom, bouton de la touche (cliquer puis appuyer sur la
        // nouvelle touche ou cliquer), bouton X pour la retirer. Deux colonnes ; la molette
        // fait défiler quand tout ne tient pas.
        let actions = keys::Action::tab(self.keys_tab_fps);
        let row_h = 20.0 * s;
        let col_w = 244.0 * s;
        let done_y = c.h * 0.91;
        let rows = actions.len().div_ceil(2);
        let visible = (((done_y - 14.0 * s - top) / row_h).floor() as usize).max(1);
        let max_scroll = rows.saturating_sub(visible);
        if input.scroll.abs() >= 1.0 {
            self.keys_scroll = if input.scroll > 0.0 {
                self.keys_scroll.saturating_sub(1)
            } else {
                self.keys_scroll + 1
            };
        }
        // Barre de défilement (comme dans l'inventaire créatif) : on attrape le curseur et
        // la liste suit la souris ; un clic dans la barre y saute.
        if max_scroll > 0 {
            let (bx, by, bw, bh) = (
                cx + 256.0 * s,
                top,
                8.0 * s,
                visible as f32 * row_h - 4.0 * s,
            );
            if let Some((mx, my)) = c.cursor {
                if c.click
                    && mx >= bx - 3.0 * s
                    && mx <= bx + bw + 3.0 * s
                    && my >= by
                    && my <= by + bh
                {
                    self.keys_drag = true;
                    c.click = false;
                }
                if self.keys_drag {
                    let frac = ((my - by) / bh).clamp(0.0, 1.0);
                    let first = frac * rows as f32 - visible as f32 / 2.0;
                    self.keys_scroll = first.round().max(0.0) as usize;
                }
            }
            if !c.held {
                self.keys_drag = false;
            }
            self.keys_scroll = self.keys_scroll.min(max_scroll);
            let shown = (visible as f32 / rows as f32).min(1.0);
            let start = self.keys_scroll as f32 / rows as f32;
            c.ui.rect(bx, by, bw, bh, [0.041, 0.041, 0.05, 1.0]);
            c.ui.rect(
                bx + s,
                by + bh * start,
                bw - 2.0 * s,
                (bh * shown).max(4.0 * s),
                [0.578, 0.578, 0.64, 1.0],
            );
        } else {
            self.keys_drag = false;
        }
        self.keys_scroll = self.keys_scroll.min(max_scroll);
        for (i, action) in actions.into_iter().enumerate() {
            let (row, col) = (i / 2, i % 2);
            if row < self.keys_scroll || row >= self.keys_scroll + visible {
                continue;
            }
            let x0 = cx - col_w + col as f32 * (col_w + 6.0 * s);
            let ry = top + (row - self.keys_scroll) as f32 * row_h;
            let grey = action.coming_soon();
            c.ui.text(
                x0,
                ry + 6.0 * s,
                s,
                if grey { GREY } else { WHITE },
                action.label(),
            );
            let waiting = self.binding == Some(action);
            let key = self.keys.get(action);
            let text = if waiting {
                "> ? <".to_string()
            } else {
                key.map_or("aucune".into(), |k| k.name(input))
            };
            if c.button(&text, x0 + 178.0 * s, ry, 92.0, true) {
                self.binding = Some(action);
                self.sens_focus = false;
            }
            if c.button("X", x0 + 236.0 * s, ry, 16.0, key.is_some()) {
                self.keys.unbind(action);
                if self.binding == Some(action) {
                    self.binding = None;
                }
            }
        }
        if max_scroll > 0 || self.keys_tab_fps {
            let mut hint = String::new();
            if max_scroll > 0 {
                hint = format!(
                    "Molette : faire défiler ({}-{} sur {} lignes)",
                    self.keys_scroll + 1,
                    (self.keys_scroll + visible).min(rows),
                    rows
                );
            }
            if self.keys_tab_fps {
                if !hint.is_empty() {
                    hint += "   ";
                }
                hint += "En gris : pas encore dans le jeu (la touche est gardée).";
            }
            c.text_center(&hint, done_y - 11.0 * s, ds, GREY);
        }
        let done = c.button("Terminé", cx, c.h * 0.91, 200.0, true);
        if self.sens_focus {
            for ch in input
                .text
                .chars()
                .filter(|ch| ch.is_ascii_digit() || *ch == '.' || *ch == ',')
            {
                if self.sens_text.len() < 6 {
                    self.sens_text.push(if ch == ',' { '.' } else { ch });
                }
            }
            if input.pressed(KeyCode::Backspace) {
                self.sens_text.pop();
            }
            if input.pressed(KeyCode::Enter) || input.pressed(KeyCode::NumpadEnter) {
                self.sens_focus = false;
            }
        }
        // Valeur tapée : appliquée dès qu'elle est lisible.
        if let Ok(v) = self.sens_text.trim().parse::<f32>() {
            if (minelib_assets::MIN_SENSITIVITY..=minelib_assets::MAX_SENSITIVITY).contains(&v) {
                sens = v;
            }
        }
        if !self.sens_focus {
            self.sens_text = format_sens(sens);
        }
        if fps_sens && self.keys_tab_fps && sens != self.options.sensitivity_fps {
            self.options.sensitivity_fps = sens;
            if let Some(sess) = &mut self.session {
                sess.sensitivity_fps = sens;
            }
        } else if !fps_sens && !self.keys_tab_fps && sens != self.options.sensitivity {
            self.options.sensitivity = sens;
            if let Some(sess) = &mut self.session {
                sess.sensitivity = sens;
            }
        }
        if let Some(sess) = &mut self.session {
            sess.keys = self.keys.clone();
        }
        if done || (input.pressed(KeyCode::Escape) && !cancel_escape) {
            self.slider_drag = None;
            self.sens_focus = false;
            self.binding = None;
            if let Err(e) = self.options.save(&self.paths.options) {
                log::error!("options : {e:#}");
            }
            if let Err(e) = self.keys.save(&self.paths.root.join("keys.txt")) {
                log::error!("touches : {e}");
            }
            self.screen = Screen::Settings;
        }
    }

    fn graphics_screen(
        &mut self,
        input: &Input,
        size: (u32, u32),
        over_game: bool,
        renderer: &mut Renderer,
    ) {
        if let Some(sess) = &self.session {
            self.config = sess.config();
        }
        let mut cfg = self.config;
        let mut low_latency = renderer.low_latency();
        let mut raytracing = renderer.raytracing();
        let mut scale = renderer.render_scale();
        let mut c = Ctx::new(&mut self.ui, input, size);
        if over_game {
            c.dim(0.7);
        } else {
            c.background();
        }
        let s = c.s;
        c.text_center("Paramètres graphiques", c.h * 0.14, s * 2.0, WHITE);
        let cx = c.w / 2.0;
        let step = 21.5 * s;
        let ds = (s / 2.0).floor().max(1.0);
        // (v251) « Dans les paramètres graphiques, deux par ligne les boutons et curseurs si c'est
        // trop grand pour l'écran, et la barre de défilement à droite, comme celle de
        // l'inventaire créatif » : les réglages se rangent sur une colonne s'ils tiennent entre
        // le titre et les notes du bas, sinon sur deux ; s'ils ne tiennent toujours pas, ils
        // défilent (molette, barre à droite).
        // (v271) « 2 par colonne les boutons des paramètres graphiques » : toujours deux par
        // ligne. Dans une fenêtre trop étroite pour deux réglages de 300, ils rétrécissent
        // (leur texte aussi) ; une seule colonne seulement sous 150 chacun.
        let several = gpu_report::several_cards();
        let n = 7 + usize::from(several) + usize::from(low_latency.is_some());
        let notes = 1 + usize::from(self.options.point_torches) + usize::from(self.optimus.is_some());
        let top = c.h * 0.21;
        let bottom = c.h * 0.78 - 14.0 * s - notes as f32 * ds * 10.0 - 4.0 * s;
        let avail = (bottom - top).max(step);
        let fit = ((avail / step).floor() as usize).max(1);
        let half = ((c.w / s - 24.0) / 2.0 - 10.0).min(300.0).floor();
        let cols = if half >= 150.0 { 2 } else { 1 };
        let bw = if cols == 2 { half } else { 300.0 };
        let rows = n.div_ceil(cols);
        let vis = fit.min(rows);
        let col_w = if cols == 2 { (bw + 10.0) * s } else { 0.0 };
        let area_w = (bw + 10.0) * s * cols as f32;
        let over = c.hovered(cx - area_w / 2.0, top, area_w, vis as f32 * step);
        Ctx::wheel(input, over, rows, vis, &mut self.graphics_scroll);
        if rows > vis {
            let bar_x = cx + area_w / 2.0 + 4.0 * s;
            c.scrollbar(
                bar_x,
                top - 2.0 * s,
                vis as f32 * step,
                rows,
                vis,
                &mut self.graphics_scroll,
                &mut self.graphics_drag,
            );
        } else {
            self.graphics_scroll = 0;
        }
        let scroll = self.graphics_scroll;
        // Place du réglage n° `k` (de gauche à droite, puis de haut en bas), s'il est visible.
        let slot = |k: usize| -> Option<(f32, f32)> {
            let (r, col) = (k / cols, k % cols);
            if r < scroll || r >= scroll + vis {
                return None;
            }
            let x = cx + (col as f32 - (cols as f32 - 1.0) / 2.0) * col_w;
            Some((x, top + (r - scroll) as f32 * step))
        };
        let mut k = 0;
        let mut next = || {
            k += 1;
            slot(k - 1)
        };
        // Distance de rendu : 2 à 64 chunks (curseur plus fin aux petites valeurs).
        let max = session::MAX_VIEW_DISTANCE;
        let ram = self.ram_view_limit;
        let label = if cfg.view_distance > ram {
            format!(
                "Distance de rendu : {} chunks (mémoire : {ram} max)",
                cfg.view_distance
            )
        } else {
            format!("Distance de rendu : {} chunks", cfg.view_distance)
        };
        if let Some((x, y)) = next() {
            let frac = ((cfg.view_distance - 2) as f32 / (max - 2) as f32).sqrt();
            if let Some(f) = c.slider(1, &mut self.slider_drag, &label, frac, x, y, bw) {
                cfg.view_distance = 2 + (f * f * (max - 2) as f32).round() as i32;
            }
        }
        // Échelle de rendu : 50 à 100 % (par pas de 5 %), reconstruite par l'anticrénelage.
        if let Some((x, y)) = next() {
            let label = format!("Échelle de rendu : {} %", (scale * 100.0).round() as i32);
            if let Some(f) = c.slider(
                3,
                &mut self.slider_drag,
                &label,
                (scale - 0.5) / 0.5,
                x,
                y,
                bw,
            ) {
                scale = ((0.5 + f * 0.5) * 20.0).round() / 20.0;
            }
        }
        // Limite de FPS : 30 à 500 par pas de 10, tout à droite = illimité. Moins d'images,
        // moins de chauffe : les FPS restent stables au lieu de baisser quand le PC chauffe.
        if let Some((x, y)) = next() {
            let cur = self.options.fps_max;
            let label = if cur == 0 {
                "Limite de FPS : illimité".to_string()
            } else {
                format!("Limite de FPS : {cur}")
            };
            let frac = if cur == 0 {
                1.0
            } else {
                (cur.clamp(30, 500) - 30) as f32 / 470.0 * 0.985
            };
            if let Some(f) = c.slider(6, &mut self.slider_drag, &label, frac, x, y, bw) {
                self.options.fps_max = if f > 0.985 {
                    0
                } else {
                    (30.0 + f / 0.985 * 470.0).round() as u32 / 10 * 10
                };
            }
        }
        // Raytracing des reflets (écrit pour Minelib : marche sur toutes les cartes).
        if let Some((x, y)) = next() {
            let rt_label = if raytracing {
                "Raytracing (reflets) : activé"
            } else {
                "Raytracing (reflets) : désactivé"
            };
            if c.button(rt_label, x, y, bw, true) {
                raytracing = !raytracing;
            }
        }
        // (v201) Torches en lumières ponctuelles (non par défaut) : autour de soi, chaque
        // torche ou bloc lumineux éclaire comme une lumière ponctuelle de l'éditeur.
        if let Some((x, y)) = next() {
            let pt_label = if self.options.point_torches {
                "Torches ponctuelles : activé"
            } else {
                "Torches ponctuelles : désactivé"
            };
            if c.button(pt_label, x, y, bw, true) {
                self.options.point_torches = !self.options.point_torches;
            }
        }
        if let Some((x, y)) = next() {
            let fs_label = if self.options.fullscreen {
                "Plein écran : oui (F11)"
            } else {
                "Plein écran : non (F11)"
            };
            if c.button(fs_label, x, y, bw, true) {
                self.options.fullscreen = !self.options.fullscreen;
            }
        }
        // (v228) Carte graphique : la plus puissante ou la partie intégrée au processeur (moins
        // de chaleur et de batterie), quand la machine en a plusieurs ; compte au prochain
        // lancement.
        if several {
            if let Some((x, y)) = next() {
                let mut gpu_label = if self.options.igpu {
                    "Carte graphique : partie intégrée".to_string()
                } else {
                    "Carte graphique : la plus puissante".to_string()
                };
                if self.options.igpu != self.igpu_at_start {
                    gpu_label.push_str(" (au prochain lancement)");
                }
                if c.button(&gpu_label, x, y, bw, true) {
                    self.options.igpu = !self.options.igpu;
                }
            }
        }
        // Répartition du processeur (prise en compte au prochain monde chargé).
        if let Some((x, y)) = next() {
            let cpu_label = if self.options.cpu_uniform {
                "Processeur : tous les coeurs"
            } else {
                "Processeur : coeurs réservés"
            };
            if c.button(cpu_label, x, y, bw, true) {
                self.options.cpu_uniform = !self.options.cpu_uniform;
                jobs::set_uniform_sharing(self.options.cpu_uniform);
            }
        }
        // Mode basse latence : seulement si la carte le propose (NVIDIA Reflex, AMD Anti-Lag 2).
        if let Some((kind, on)) = &mut low_latency {
            if let Some((x, y)) = next() {
                let label = format!(
                    "{} : {}",
                    kind.label(),
                    if *on { "activé" } else { "désactivé" }
                );
                if c.button(&label, x, y, bw, true) {
                    *on = !*on;
                }
            }
        }
        let y = top + vis as f32 * step;
        c.text_center(
            "Échelle sous 100 % : monde calculé en plus petit puis reconstruit (plus de FPS).",
            y + 4.0 * s,
            ds,
            GREY,
        );
        let mut hint_y = y + 4.0 * s + ds * 10.0;
        if self.options.point_torches {
            c.text_center(
                "Torches ponctuelles : réglages dans « Lumières », onglet Auto.",
                hint_y,
                ds,
                GREY,
            );
            hint_y += ds * 10.0;
        }
        if let Some(shown) = &self.optimus {
            c.text_center(
                &format!(
                    "Écran sur « {shown} » : active le mode GPU dédié (MUX) pour plus de FPS."
                ),
                hint_y,
                ds,
                GREY,
            );
        }
        // (v215) Les réglages de toutes les lumières (avant : bouton « Réglages » de
        // l'éditeur de lumières), à côté de « Terminé ».
        let to_lights = c.button("Lumières", cx - 76.0 * s, c.h * 0.78, 148.0, true);
        let done = c.button("Terminé", cx + 76.0 * s, c.h * 0.78, 148.0, true);
        let leave = done || input.pressed(KeyCode::Escape);
        if to_lights && !leave {
            self.slider_drag = None;
            self.screen = Screen::Lights;
        }

        if cfg.view_distance != self.config.view_distance {
            self.config = cfg;
            if let Some(sess) = &mut self.session {
                sess.apply_config(cfg);
            }
        }
        if raytracing != renderer.raytracing() {
            renderer.set_raytracing(raytracing);
        }
        if (scale - renderer.render_scale()).abs() > 1e-3 {
            renderer.set_render_scale(scale);
        }
        if let Some((_, on)) = low_latency {
            if renderer.low_latency().is_some_and(|(_, cur)| cur != on) {
                renderer.set_low_latency(on);
            }
        }
        if leave {
            self.slider_drag = None;
            self.options.render_distance = self.config.view_distance;
            if let Some((_, on)) = renderer.low_latency() {
                self.options.low_latency = on;
            }
            self.options.raytracing = renderer.raytracing();
            self.options.render_scale = renderer.render_scale();
            if let Err(e) = self.options.save(&self.paths.options) {
                log::error!("options : {e:#}");
            }
            self.screen = Screen::Settings;
        }
    }

    /// (v215) Paramètres graphiques → Lumières : les réglages de toutes les lumières. En jeu,
    /// ceux de la partie, appliqués tout de suite ; hors d'une partie, relus de
    /// `.minelib/lumieres.txt` et enregistrés en sortant.
    fn lights_screen(&mut self, input: &Input, size: (u32, u32), over_game: bool) {
        let sizef = (size.0 as f32, size.1 as f32);
        {
            let mut c = Ctx::new(&mut self.ui, input, size);
            if over_game {
                c.dim(0.7);
            } else {
                c.background();
            }
        }
        let done = match &mut self.session {
            Some(sess) => sess.light_settings_ui(&mut self.ui, sizef, input),
            None => {
                let lights = self.menu_lights.get_or_insert_with(|| {
                    let root = minelib_assets::data_dir();
                    lights::Lights::load(&root, &root)
                });
                self.menu_light_editor
                    .settings_screen(&mut self.ui, input, lights, sizef)
            }
        };
        if done {
            if let Some(mut l) = self.menu_lights.take() {
                l.save_now();
            }
            self.screen = Screen::Graphics;
        }
    }

    fn draw_message(&mut self, size: (u32, u32), dt: f32) {
        let Some((text, t)) = &mut self.message else {
            return;
        };
        *t -= dt;
        if *t <= 0.0 {
            self.message = None;
            return;
        }
        let s = (size.1 as f32 / 300.0).floor().max(1.0);
        let tw = Ui::text_width(text, s);
        let x = (size.0 as f32 - tw) / 2.0;
        self.ui.rect(
            x - 6.0 * s,
            4.0 * s,
            tw + 12.0 * s,
            14.0 * s,
            [0.0, 0.0, 0.0, 0.8],
        );
        self.ui.text(x, 7.0 * s, s, RED, text.clone());
    }
}

impl GameLoop for Game {
    fn on_ready(&mut self, window: Arc<PlatformWindow>) {
        let pack = self
            .startup_pack
            .take()
            .unwrap_or_else(|| ResourcePack::builtin().expect("pack intégré"));
        // (v228) La carte choisie dans les paramètres graphiques.
        minelib_render::prefer_integrated(self.options.igpu);
        self.igpu_at_start = self.options.igpu;
        match Renderer::new(window, &pack) {
            Ok(mut r) => {
                r.set_low_latency(self.options.low_latency);
                r.set_raytracing(self.options.raytracing);
                r.set_render_scale(self.options.render_scale);
                // Répartition du travail selon la carte (et réglages du 1er lancement).
                let first_launch = !self.paths.options.exists();
                gpu_plan::apply(&mut r, &mut self.options, first_launch);
                // Anticrénelage temporel : toujours actif (MINELIB_TAA=0 seulement pour les tests).
                r.set_antialiasing(std::env::var("MINELIB_TAA").map_or(true, |v| v != "0"));
                // Lumières utilisées ces derniers jours : leurs shaders (25 s de compilation
                // sous DirectX) commencent dès que possible, pendant les menus.
                self.warm_lights = lights::used_recently(&self.paths.root) && !r.warm_dyn_lights();
                // Essais : MINELIB_SMOKE_Q=<pas max>,<longueur d'un pas> (qualité des fumigènes).
                if let Ok(q) = std::env::var("MINELIB_SMOKE_Q") {
                    let v: Vec<f32> = q.split(',').filter_map(|s| s.trim().parse().ok()).collect();
                    if v.len() == 2 {
                        r.set_smoke_quality(minelib_render::SmokeQuality {
                            max_steps: v[0] as u32,
                            step: v[1],
                        });
                    }
                }
                self.sysmon.set_gpu_name(&r.adapter_name);
                // Mémoire vidéo : 80 % de la mémoire dédiée de la carte, moins 512 Mo pour
                // l'image, l'anticrénelage et les textures.
                self.vram = minelib_platform::VramMonitor::new(&r.adapter_name)
                    .map(|m| (m, std::time::Instant::now(), false));
                let adapters = minelib_platform::gpu_adapters();
                // (v228) Où est l'écran (écran de debug), combien de cartes (paramètres).
                gpu_report::note(&adapters, &r.adapter_name);
                if let Some(a) = adapters
                    .iter()
                    .find(|a| r.adapter_name.contains(&a.name) || a.name.contains(&r.adapter_name))
                {
                    if a.dedicated > 1 << 30 {
                        r.set_vram_budget((a.dedicated as f64 * 0.8) as u64 - (512 << 20));
                    }
                    // Optimus : l'écran est sur une autre carte, chaque image y est recopiée.
                    // Le basculement (MUX) ne se fait que par le logiciel du portable ou le
                    // BIOS, avec redémarrage : on prévient le joueur.
                    if !a.has_display {
                        if let Some(d) = adapters.iter().find(|d| d.has_display) {
                            self.optimus = Some(d.name.clone());
                            log::info!(
                                "écran sur « {} » : le mode GPU dédié (MUX) donnerait plus de FPS",
                                d.name
                            );
                        }
                    }
                }
                // Cartes graphiques, écran, partie intégrée : bilan dans le journal.
                gpu_report::spawn(r.adapter_name.clone());
                self.current_pack = r.pack_name.clone();
                self.renderer = Some(r);
            }
            Err(e) => log::error!("initialisation du rendu impossible : {e:#}"),
        }
        // Lancement direct dans un monde (tests, raccourci).
        if self.auto_world.is_some() {
            // Essais : l'image attendue dès la première (pas de ciel seul).
            if let Some(r) = &self.renderer {
                r.wait_world_ready();
            }
        }
        if let Some(name) = self.auto_world.take() {
            let existing = self
                .worlds
                .iter()
                .find(|w| w.meta.name == name)
                .map(|w| w.dir.clone());
            let dir = existing.or_else(|| {
                let seed = std::env::var("MINELIB_SEED")
                    .map(|s| save::parse_seed(&s))
                    .unwrap_or(20_261_108);
                save::create_world(&self.paths.saves, &name, seed).ok()
            });
            if let Some(dir) = dir {
                if std::env::var("MINELIB_EN_LIGNE").is_ok_and(|v| v.trim() == "1") {
                    self.auto_online = Some(dir);
                } else {
                    self.play(dir);
                }
            }
        }
    }

    fn on_resize(&mut self, width: u32, height: u32) {
        if let Some(r) = &mut self.renderer {
            r.resize(width, height);
        }
    }

    fn on_frame(&mut self, window: &PlatformWindow, input: &Input, dt: f32) -> bool {
        // (v251) Essais : MINELIB_PAS_FIXE=<n> (voir plus haut).
        static FIXED: std::sync::OnceLock<Option<f32>> = std::sync::OnceLock::new();
        let dt = FIXED
            .get_or_init(|| {
                std::env::var("MINELIB_PAS_FIXE")
                    .ok()
                    .and_then(|v| v.trim().parse::<f32>().ok())
                    .filter(|n| n.is_finite() && *n >= 1.0)
            })
            .map_or(dt, |n| 1.0 / n);
        self.time += dt;
        self.record_frame_time(dt);
        // (v251) Ctrl+C peu après une capture : copiée. Avant toute autre touche : le C de ce
        // Ctrl+C ne compte pour rien d'autre tant qu'il est tenu (`muted_key`).
        if self.keys.muted_key.is_some_and(|k| !input.held(k)) {
            self.keys.muted_key = None;
        }
        self.copy_last_shot(input);
        if self.auto_online.is_some() && self.accounts.link.is_some() {
            if let Some(dir) = self.auto_online.take() {
                self.host_build(dir);
            }
        }
        // (v318) Essais : rejoindre un serveur de construction (`MINELIB_REJOINDRE`).
        if self.session.is_none() && self.accounts.link.is_some() {
            if let Some(want) = self.auto_join.clone() {
                let found = self
                    .accounts
                    .servers
                    .iter()
                    .find(|e| e.name.contains(want.as_str()))
                    .map(|e| e.id);
                match found {
                    Some(room) => {
                        self.auto_join = None;
                        log::info!("essai : serveur « {want} » rejoint");
                        self.accounts.room(minelib_net::client::Cmd::Join(room));
                    }
                    None if self.time - self.auto_join_asked > 2.0 => {
                        self.auto_join_asked = self.time;
                        // (Un monde ouvert en ligne pour la première fois est sous liste
                        // blanche : `/whitelist off` dans ce monde pour qu'il paraisse.)
                        log::info!(
                            "essai : « {want} » pas encore dans la liste ({} serveur(s) : {:?})",
                            self.accounts.servers.len(),
                            self.accounts
                                .servers
                                .iter()
                                .map(|e| e.name.as_str())
                                .collect::<Vec<_>>(),
                        );
                        self.accounts.refresh_servers(1);
                    }
                    None => {}
                }
            }
        }
        if let Some(limit) = self.exit_after {
            if self.time > limit {
                log::info!("arrêt automatique après {limit} s");
                return false;
            }
        }
        // Plein écran : F11, ou le bouton des paramètres graphiques.
        if input.pressed(KeyCode::F11) {
            self.options.fullscreen = !self.options.fullscreen;
            if let Err(e) = self.options.save(&self.paths.options) {
                log::error!("options : {e:#}");
            }
        }
        // Le plus proche d'un MUX sans MUX : si un écran est branché sur une prise reliée à la
        // carte qui dessine (souvent la HDMI sur les portables Optimus), le jeu s'y affiche
        // tout seul (vérifié toutes les 3 s : écran branché en cours de partie). L'image n'est
        // alors plus recopiée vers la puce Intel.
        let check = self
            .direct_display
            .0
            .is_none_or(|t| t.elapsed().as_secs_f32() >= 3.0);
        let mut target: Option<winit::monitor::MonitorHandle> = None;
        if check {
            self.direct_display.0 = Some(std::time::Instant::now());
            // (v215) Fréquence de l'écran où est la fenêtre : limite des menus.
            self.refresh_hz = window
                .current_monitor()
                .and_then(|m| m.refresh_rate_millihertz())
                .map(|mhz| mhz as f32 / 1000.0);
            if let Some(r) = &self.renderer {
                let outputs = minelib_platform::gpu_output_names(&r.adapter_name);
                target = window
                    .available_monitors()
                    .find(|m| m.name().is_some_and(|n| outputs.contains(&n)));
            }
            let name = target.as_ref().and_then(|m| m.name());
            if name != self.direct_display.1 {
                if let (Some(m), Some(n)) = (&target, &name) {
                    log::info!("écran {n} branché sur la carte qui dessine : le jeu s'y affiche (pas de recopie entre cartes)");
                    self.optimus = None;
                    gpu_report::now_direct();
                    if !self.options.fullscreen {
                        window.set_outer_position(m.position());
                    }
                }
                self.direct_display.1 = name;
                self.applied_fullscreen = None; // replacer le plein écran sur le bon écran
            }
        }
        if self.applied_fullscreen != Some(self.options.fullscreen) {
            let monitor = self.direct_display.1.as_ref().and_then(|n| {
                window
                    .available_monitors()
                    .find(|m| m.name().as_ref() == Some(n))
            });
            window.set_fullscreen(
                self.options
                    .fullscreen
                    .then_some(winit::window::Fullscreen::Borderless(monitor)),
            );
            self.applied_fullscreen = Some(self.options.fullscreen);
            log::info!(
                "plein écran : {}",
                if self.options.fullscreen {
                    "oui"
                } else {
                    "non"
                }
            );
        }
        let Some(mut renderer) = self.renderer.take() else {
            return false;
        };
        // Voir `on_ready` : la compilation des lumières attendait les shaders du monde.
        if self.warm_lights {
            self.warm_lights = !renderer.warm_dyn_lights();
        }
        let size = renderer.size();
        let sizef = (size.0 as f32, size.1 as f32);
        self.ui.clear();

        if self.keys.pressed(input, keys::Action::Debug)
            && !self.session.as_ref().is_some_and(|s| s.chat.open)
        {
            self.show_debug = !self.show_debug;
        }
        // F2 : capture d'écran (interface comprise), comme dans Minecraft, même tchat ouvert.
        let mut auto_shot = self.shot_at.is_some_and(|t| self.time >= t);
        if auto_shot {
            self.shot_at = None;
        }
        // (v305) Essais : MINELIB_CAPTURES=<s>,<n> : une capture à chaque image, n images de
        // suite à partir de <s> secondes (ce qui change d'une image à l'autre).
        static SERIES: std::sync::Mutex<Option<(f32, u32)>> = std::sync::Mutex::new(None);
        static SERIES_INIT: std::sync::Once = std::sync::Once::new();
        SERIES_INIT.call_once(|| {
            let v = std::env::var("MINELIB_CAPTURES").unwrap_or_default();
            if let Some((a, b)) = v.split_once(',') {
                if let (Ok(t), Ok(n)) = (a.trim().parse::<f32>(), b.trim().parse::<u32>()) {
                    if let Ok(mut g) = SERIES.lock() {
                        *g = Some((t, n));
                    }
                }
            }
        });
        if let Ok(mut g) = SERIES.lock() {
            if let Some((t, n)) = g.as_mut() {
                if self.time >= *t && *n > 0 {
                    *n -= 1;
                    auto_shot = true;
                }
            }
        }
        // Mode FPS : la touche de CS (F5) ; sinon celle de Minecraft (F2).
        let fps_mode = self
            .session
            .as_ref()
            .is_some_and(|s| s.game.mode == save::GameMode::Fps);
        let shot_key = if fps_mode {
            keys::Action::FpsScreenshot
        } else {
            keys::Action::Screenshot
        };
        if self.keys.pressed(input, shot_key) || auto_shot {
            renderer.request_screenshot();
        }
        if let Some(shot) = renderer.take_screenshot() {
            self.save_screenshot(shot);
        }
        // Clic sur le nom d'une capture dans le tchat : image copiée dans le presse-papier.
        if let Some(path) = self.session.as_mut().and_then(|s| s.chat.clicked.take()) {
            let text = match copy_image_to_clipboard(&path) {
                Ok(()) => (
                    format!(
                        "Capture copiée dans le presse-papier : {}",
                        file_name(&path)
                    ),
                    chat::GREY,
                ),
                Err(e) => {
                    log::warn!("presse-papier : {e:#}");
                    (format!("Copie impossible : {e}"), chat::RED)
                }
            };
            if let Some(s) = &mut self.session {
                s.chat.say(text.0, text.1);
            }
        }

        // Relais : réponses, partie en ligne.
        self.poll_accounts();
        self.doze_upkeep();
        self.handle_game_events();
        self.skin_upkeep();
        self.maps_upkeep();
        self.updater.poll(!self.played);
        // (v314) Une version est sortie pendant qu'on joue : dit une fois dans le tchat.
        // Le joueur dont le PC sert de relais ne passe pas par l'accueil, où est le bouton ;
        // or les joueurs déjà à jour ne peuvent plus le rejoindre si le protocole a changé.
        if let Some(v) = self.updater.available.as_ref().map(|r| r.version) {
            if self.update_told != v {
                if let Some(s) = &mut self.session {
                    self.update_told = v;
                    s.chat.say(
                        format!(
                            "Version {v} du jeu disponible : relance le jeu quand tu peux. Si \
                             elle change le réseau, les joueurs à jour ne rejoignent plus \
                             ceux qui ne le sont pas."
                        ),
                        chat::YELLOW,
                    );
                }
            }
        }
        // Partie FPS en ligne commencée : notée (pas de mise à jour, ni par le jeu ni par
        // le lanceur, avant sa fin) ; finie : oubliée.
        let net = self.session.as_ref().and_then(|s| s.net.as_ref());
        if let Some(n) = net.filter(|n| !n.is_solo()) {
            if n.in_progress() && self.updater.hold.is_none() {
                update::write_match_lock(&self.paths.root);
                self.updater.hold = Some(update::VERSION);
            } else if n.phase() == minelib_net::game::phase::GAME_OVER
                && self.updater.hold.is_some()
            {
                update::clear_match_lock(&self.paths.root);
                self.updater.hold = None;
            }
        }
        self.garant_update(dt);

        // Échap : pause / reprise / fermeture de l'inventaire. (v179) Avec « Couper le son
        // des écrans dans le menu » (Paramètres → Son), en solo, la pause met aussi en pause
        // les vidéos des écrans (elles ne repartent pas à la reprise).
        if input.pressed(KeyCode::Escape) {
            match self.screen {
                Screen::Playing => match &mut self.session {
                    Some(s) if s.chat.open => s.chat.close(),
                    Some(s) if s.inventory.open => s.inventory.close(),
                    Some(s) if s.light_editor.open => s.light_editor.escape(),
                    Some(s) if s.fps.buy_open => s.fps.buy_open = false,
                    // On écrit dans la page d'un écran : Échap rend le clavier au jeu.
                    Some(s) if s.keyboard_on_screen() => s.screen_escape(),
                    Some(s) => {
                        if self.options.mute_screens_menu {
                            s.pause_videos();
                        }
                        self.screen = Screen::Paused;
                    }
                    None => self.screen = Screen::Paused,
                },
                Screen::Paused => self.screen = Screen::Playing,
                _ => {}
            }
        }
        // (v179) En ligne, avec la même option : le son des écrans est coupé tant qu'un menu
        // est ouvert par-dessus la partie (pause, paramètres…), et revient ensuite.
        // (v201) Réglage vidéo des torches en lumières ponctuelles.
        if let Some(s) = &mut self.session {
            s.lights.point_torches = self.options.point_torches;
        }
        if let Some(s) = &mut self.session {
            let mute =
                self.options.mute_screens_menu && self.screen != Screen::Playing && s.online();
            s.set_screens_volume(if mute {
                0.0
            } else {
                self.options.volume_screens
            });
        }
        // (v234) « Coupe le son de la météo dans Échap » : la pluie et le tonnerre se taisent
        // tant qu'un menu est ouvert par-dessus la partie (Échap, paramètres…).
        crate::audio::weather_hush(self.session.is_some() && self.screen != Screen::Playing);

        // Mémoire vidéo : relevé chaque seconde (bilan des 10 s), et plus de nouvelles pages
        // pour les chunks au-delà de 92 % du budget Windows (reprise sous 85 %).
        if let Some((mon, last, pressure)) = &mut self.vram {
            if last.elapsed().as_secs_f32() >= 1.0 {
                *last = std::time::Instant::now();
                if let Some(t) = minelib_render::thermal::gpu_temp() {
                    minelib_render::perf::gauge("carte graphique (°C)", t);
                }
                if let Some((used, budget)) = mon.query() {
                    let mb = |b: u64| (b / (1024 * 1024)) as f32;
                    minelib_render::perf::gauge("mémoire vidéo utilisée (Mo)", mb(used));
                    minelib_render::perf::gauge("budget mémoire vidéo de Windows (Mo)", mb(budget));
                    let ratio = used as f64 / budget.max(1) as f64;
                    let next = if *pressure {
                        ratio > 0.85
                    } else {
                        ratio > 0.92
                    };
                    if next != *pressure {
                        *pressure = next;
                        renderer.set_vram_pressure(next);
                        if next {
                            log::warn!(
                                "mémoire vidéo presque pleine ({:.0} / {:.0} Mo) : plus de nouveaux chunks en mémoire vidéo",
                                mb(used),
                                mb(budget)
                            );
                        } else {
                            log::info!(
                                "mémoire vidéo de nouveau disponible ({:.0} / {:.0} Mo)",
                                mb(used),
                                mb(budget)
                            );
                        }
                    }
                }
            }
        }

        // Arrivée en jeu (Jouer, Reprendre, rejoindre un serveur…) : le clic qui y mène ne
        // pose ni ne casse de bloc, la souris compte de nouveau une fois relâchée.
        let playing = self.screen == Screen::Playing;
        if playing && !self.was_playing {
            if let Some(s) = &mut self.session {
                s.keys.mouse_muted = true;
            }
        }
        self.was_playing = playing;
        let in_world = self.session.is_some()
            && matches!(
                self.screen,
                Screen::Playing
                    | Screen::Paused
                    | Screen::Packs
                    | Screen::Settings
                    | Screen::Graphics
                    | Screen::Lights
                    | Screen::Controls
                    | Screen::Sound
                    | Screen::Advanced
                    | Screen::DeleteWorld
            );
        if in_world {
            let active = self.screen == Screen::Playing;
            let frames = debug::frame_summary(&self.frame_times);
            let stats = self.last_stats;
            let sys = self.sysmon.latest();
            let fps_max = self.fps_limit();
            // (v220) Monde pas encore chargé : un écran de chargement le cache, sans
            // l'interface du jeu (voir `Session::loading`).
            let mut loading = None;
            let mut hud_off = false;
            if let Some(s) = &mut self.session {
                // (v273) Tab au tchat : ses amis aussi parmi les pseudos proposés.
                if s.chat.open && input.pressed(KeyCode::Tab) {
                    s.chat.known = self.accounts.friends.iter().map(|f| f.0.clone()).collect();
                }
                s.update(input, dt, &mut renderer, active);
                self.rebase |= s.wants_rebase();
                let _ui = minelib_render::perf::scope("interface");
                loading = s.loading_progress();
                // (v220) Interface masquée (F1, construction) : pas d'écran F3 non plus.
                hud_off = s.hud_off();
                if self.show_debug && loading.is_none() && !hud_off {
                    s.build_debug(frames, sys, &renderer, stats, fps_max);
                }
                if active && loading.is_none() {
                    s.draw_hud(&mut self.ui, sizef, input, self.show_debug);
                    s.light_editor_ui(&mut self.ui, sizef, input, renderer.dyn_stats());
                }
            }
            if let Some((part, shaders)) = loading {
                world_loading_screen(&mut self.ui, input, size, part, shaders);
            }
            // (v311) `/deleteworld` vient d'être tapé : l'écran de confirmation.
            if self
                .session
                .as_mut()
                .is_some_and(|s| std::mem::take(&mut s.delete_world_asked))
            {
                self.delete_code.clear();
                self.delete_code_needed = false;
                self.screen = Screen::DeleteWorld;
            }
            match self.screen {
                Screen::DeleteWorld => self.delete_world_screen(input, size),
                Screen::Paused => self.pause_menu(input, size),
                Screen::Packs => self.packs_screen(input, size, true),
                Screen::Settings => self.settings_screen(input, size, true),
                Screen::Graphics => self.graphics_screen(input, size, true, &mut renderer),
                Screen::Lights => self.lights_screen(input, size, true),
                Screen::Controls => self.controls_screen(input, size, true),
                Screen::Sound => self.sound_screen(input, size, true),
                Screen::Advanced => self.advanced_screen(input, size, true),
                _ => {}
            }
            // Monde ouvert dans les toutes premières secondes du jeu : les shaders lourds
            // finissent de se compiler en tâche de fond.
            if !renderer.world_ready() && loading.is_none() {
                let s = (sizef.1 / 300.0).floor().max(1.0);
                let t = "Préparation des shaders…";
                let w = Ui::text_width(t, s);
                self.ui.text((sizef.0 - w) / 2.0, sizef.1 * 0.4, s, GOLD, t);
            }
            self.draw_message(size, dt);
            if let Some(s) = &self.session {
                self.last_stats = s.render(
                    &mut renderer,
                    &self.ui,
                    self.show_debug && loading.is_none() && !hud_off,
                    self.time,
                    input.grabbed,
                );
            } else {
                renderer.render_ui(&self.ui);
            }
            if let Some(s) = &self.session {
                self.title_timer += dt;
                if self.title_timer >= 0.5 {
                    self.title_timer = 0.0;
                    let fps = debug::frame_summary(&self.frame_times).fps;
                    window.set_title(&format!("Minelib - {} - {fps:.0} FPS", s.meta.name));
                }
            }
        } else {
            self.poll_accounts();
            match self.screen {
                Screen::Title => self.title_screen(input, size),
                Screen::Solo => self.solo_screen(input, size),
                Screen::NewWorld => self.new_world_screen(input, size),
                Screen::Packs => self.packs_screen(input, size, false),
                Screen::Settings => self.settings_screen(input, size, false),
                Screen::Graphics => self.graphics_screen(input, size, false, &mut renderer),
                Screen::Lights => self.lights_screen(input, size, false),
                Screen::Controls => self.controls_screen(input, size, false),
                Screen::Sound => self.sound_screen(input, size, false),
                Screen::Advanced => self.advanced_screen(input, size, false),
                // (Seulement dans un monde : hors d'un monde, retour à l'accueil.)
                Screen::DeleteWorld => self.screen = Screen::Title,
                Screen::Loading => self.loading_screen(input, size),
                Screen::Multiplayer => self.multiplayer_screen(input, size),
                Screen::Servers(kind) => self.servers_screen(input, size, kind),
                Screen::Account(page) => self.account_screen(input, size, page),
                Screen::HostPick => self.host_pick_screen(input, size),
                Screen::WorldLimit => self.world_limit_screen(input, size),
                Screen::FpsHome => self.fps_home_screen(input, size),
                Screen::SoloMaps => self.solo_maps_screen(input, size),
                Screen::Playing | Screen::Paused => self.screen = Screen::Title,
            }
            self.draw_message(size, dt);
            renderer.render_ui(&self.ui);
            window.set_title("Minelib");
        }
        // Conversion après avoir affiché au moins une image de l'écran d'attente.
        if self.screen == Screen::Loading {
            if let Some((dir, frames)) = self.pending_open.take() {
                if frames >= 2 {
                    self.play(dir);
                    if self.session.is_none() {
                        self.screen = Screen::Solo;
                    }
                } else {
                    self.pending_open = Some((dir, frames + 1));
                }
            } else {
                self.screen = Screen::Solo;
            }
        }
        if std::mem::take(&mut self.rebase) {
            self.rebase_world();
        }
        if std::mem::take(&mut self.clear_meshes) {
            renderer.clear_meshes();
        }
        if let Some(info) = self.pack_request.take() {
            self.select_pack(&mut renderer, &info);
        }
        self.renderer = Some(renderer);
        self.frames_drawn = self.frames_drawn.saturating_add(1);
        self.limit_fps();
        // (v204) Tout ce qui fait attendre (pilote, file de la carte, image de la fenêtre)
        // se passe ici, juste avant que la fenêtre relise le clavier et la souris : l'image
        // suivante part avec les entrées les plus fraîches.
        if let Some(r) = &mut self.renderer {
            r.next_frame();
        }
        minelib_render::perf::frame_end();
        !self.quit
    }

    fn loaded(&self) -> bool {
        // Rendu prêt et quelques images déjà dessinées (textures envoyées à la carte), et
        // connexion automatique au compte terminée (12 s au plus : relais lent ou absent).
        self.renderer.is_some()
            && self.frames_drawn >= 3
            && (self.accounts.auto_settled() || self.started.elapsed().as_secs() >= 8)
    }

    fn on_shown(&mut self) {
        log::info!(
            "fenêtre affichée après {} images, {:.1} s ({})",
            self.frames_drawn,
            self.started.elapsed().as_secs_f32(),
            if self.accounts.logged_in() {
                "compte connecté"
            } else {
                "pas connecté"
            }
        );
        // Le lanceur attend ce fichier pour se fermer.
        if let Some(path) = std::env::var_os("MINELIB_READY_FILE") {
            let _ = std::fs::write(path, b"ok");
        }
    }

    fn ui_open(&self) -> bool {
        match self.screen {
            Screen::Playing => self.session.as_ref().is_some_and(|s| {
                s.inventory.open
                    || s.chat.open
                    || s.fps.buy_open
                    || s.cursor_free
                    // Éditeur de lumières : souris libre, sauf pendant qu'elle tourne la vue.
                    || (s.light_editor.open && !s.light_editor.looking)
            }),
            _ => true,
        }
    }

    fn on_exit(&mut self) {
        // Fermeture de la fenêtre en pleine partie : on enregistre quand même.
        if let Some(mut s) = self.session.take() {
            s.flush_reports();
            if let Err(e) = s.save() {
                log::error!("sauvegarde impossible : {e:#}");
            }
            // (v325) Propriétaire d'un monde en ligne : sa copie le garde (fermé d'abord : elle
            // peut changer de dossier).
            if s.build.as_ref().is_some_and(|b| b.is_owner()) {
                let dir = s.dir.clone();
                drop(s);
                self.keep_online_copy(&dir);
            }
        }
        // Relais intégré, ou monde repris tenu par la connexion de garant : ses mondes
        // passent tout de suite à leur relais de secours.
        if let Some(w) = self.garant_worker.take() {
            w.finish();
        }
        self.accounts.release_relay();
        self.renderer = None; // libère le GPU avant la fermeture de la fenêtre
    }
}

/// Cartes déjà jouées en ligne (`maps/partagees.txt`, une signature par ligne).
/// (v220) Écran de chargement d'un monde, par-dessus le jeu (voir `Session::loading`) :
/// fond des menus, ce qu'on attend et une barre (`part`, 0 à 1).
fn world_loading_screen(ui: &mut Ui, input: &Input, size: (u32, u32), part: f32, shaders: bool) {
    let mut c = Ctx::new(ui, input, size);
    c.background();
    let s = c.s;
    let title = if shaders {
        "Chargement du monde…"
    } else {
        "Préparation des shaders…"
    };
    c.text_center(title, c.h * 0.42, s * 2.0, WHITE);
    let (w, h) = (160.0 * s, 4.0 * s);
    let (x, y) = ((c.w - w) / 2.0, c.h * 0.42 + 26.0 * s);
    c.ui.rect(x, y, w, h, [0.0, 0.0, 0.0, 0.6]);
    c.ui.rect(x, y, w * part.clamp(0.0, 1.0), h, [0.35, 0.85, 0.4, 1.0]);
}

fn load_shared(root: &std::path::Path) -> std::collections::HashSet<[u8; 32]> {
    std::fs::read_to_string(root.join("maps").join("partagees.txt"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let l = l.trim();
            if l.len() != 64 {
                return None;
            }
            let mut out = [0u8; 32];
            for (i, b) in out.iter_mut().enumerate() {
                *b = u8::from_str_radix(l.get(2 * i..2 * i + 2)?, 16).ok()?;
            }
            Some(out)
        })
        .collect()
}

/// Ouvre un dossier dans l'explorateur de fichiers du système.
fn open_folder(path: &std::path::Path) {
    #[cfg(windows)]
    let cmd = "explorer";
    #[cfg(target_os = "macos")]
    let cmd = "open";
    #[cfg(all(unix, not(target_os = "macos")))]
    let cmd = "xdg-open";
    if let Err(e) = std::process::Command::new(cmd).arg(path).spawn() {
        log::warn!("impossible d'ouvrir {} : {e}", path.display());
    }
}

/// Sensibilité affichée : jusqu'à 2 décimales, sans zéros inutiles (1.25, 2, 0.8).
fn format_sens(v: f32) -> String {
    let t = format!("{v:.2}");
    t.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn env_parse<T: std::str::FromStr>(name: &str) -> Option<T> {
    std::env::var(name).ok()?.trim().parse().ok()
}

/// Journal écrit à la fois dans la console, dans le journal du lancement et dans
/// `minelib.log` (dernier lancement), pour retrouver la cause d'un plantage.
struct Tee(Option<std::fs::File>, Option<std::fs::File>);

impl std::io::Write for Tee {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let _ = std::io::stderr().write_all(buf);
        // (v307) Les lignes réseau du journal (sans pseudo ni adresse) sont aussi gardées
        // pour le relais, si le joueur a accepté de les partager (`netlog`).
        if minelib_net::netlog::sharing() {
            if let Ok(text) = std::str::from_utf8(buf) {
                for line in text.lines() {
                    // (Après l'en-tête « [date NIVEAU module] ».)
                    let msg = line.split_once("] ").map_or(line, |(_, m)| m);
                    minelib_net::netlog::offer(msg);
                }
            }
        }
        for f in [&mut self.0, &mut self.1].into_iter().flatten() {
            let _ = f.write_all(buf);
        }
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        for f in [&mut self.0, &mut self.1].into_iter().flatten() {
            let _ = f.flush();
        }
        Ok(())
    }
}

/// Date et heure UTC lisibles (pour les noms de fichiers du journal).
fn file_name(p: &std::path::Path) -> String {
    p.file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
}

/// Copie une image (fichier PNG) dans le presse-papier.
fn copy_image_to_clipboard(path: &std::path::Path) -> anyhow::Result<()> {
    let img = image::open(path)?.to_rgba8();
    let (width, height) = img.dimensions();
    copy_rgba_to_clipboard(width, height, img.as_raw())
}

/// (v251) Copie une image (RGBA, 8 bits) dans le presse-papier.
fn copy_rgba_to_clipboard(width: u32, height: u32, rgba: &[u8]) -> anyhow::Result<()> {
    let mut cb = arboard::Clipboard::new()?;
    cb.set_image(arboard::ImageData {
        width: width as usize,
        height: height as usize,
        bytes: std::borrow::Cow::Borrowed(rgba),
    })?;
    Ok(())
}

/// (v251) Dernière capture d'écran, gardée en mémoire pour Ctrl+C.
struct LastShot {
    path: std::path::PathBuf,
    rgba: std::sync::Arc<Vec<u8>>,
    width: u32,
    height: u32,
    /// Moment de la capture (`Game::time`, s).
    at: f32,
}

/// (v251) En jeu (curseur capturé), Ctrl+C copie la dernière capture pendant ce temps après
/// l'avoir prise (s), pas plus : Ctrl et C servent aussi en jeu (courir et zoomer,
/// s'accroupir et la radio). (v262) Dans les menus et le tchat ouvert : toujours.
const SHOT_COPY_S: f32 = 60.0;

fn stamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs()) as i64;
    let (days, rem) = (secs.div_euclid(86400), secs.rem_euclid(86400));
    // Jours depuis 1970 → date (algorithme civil de H. Hinnant, domaine public).
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + (m <= 2) as i64;
    format!(
        "{y:04}-{m:02}-{d:02}_{:02}-{:02}-{:02}",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// Garde les `keep` fichiers les plus récents de `dir` commençant par `prefix`.
fn prune(dir: &std::path::Path, prefix: &str, keep: usize) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<_> = rd
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with(prefix))
        .map(|e| e.path())
        .collect();
    files.sort();
    let n = files.len().saturating_sub(keep);
    for p in &files[..n] {
        let _ = std::fs::remove_file(p);
    }
}

/// Portables à deux cartes : demande aux pilotes NVIDIA (Optimus) et AMD (PowerXpress) de
/// lancer le jeu sur la carte dédiée (exportés par `build.rs`).
#[cfg(windows)]
#[no_mangle]
#[used]
pub static NvOptimusEnablement: u32 = 1;
#[cfg(windows)]
#[no_mangle]
#[used]
pub static AmdPowerXpressRequestHighPerformance: u32 = 1;

fn main() -> anyhow::Result<()> {
    // (v261) Le navigateur des écrans (Edge), lancé par le jeu : rien d'autre du jeu.
    {
        let mut args = std::env::args();
        if args.nth(1).is_some_and(|a| a == edge::ARG) {
            std::process::exit(edge::main(args.collect()));
        }
    }
    // Version annoncée au relais (parties FPS entre jeux de la même version).
    minelib_net::client::GAME_VERSION.store(update::VERSION, std::sync::atomic::Ordering::Relaxed);
    // Essais : MINELIB_FUSION=0, pas de fusion des faces voisines (voir `mesh::merge_flat_faces`).
    if std::env::var("MINELIB_FUSION").is_ok_and(|v| v.trim() == "0") {
        minelib_world::mesh::MERGE_FACES.store(false, std::sync::atomic::Ordering::Relaxed);
    }
    // Processus de bilan des cartes graphiques (lancé par le jeu, voir `gpu_report`).
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).is_some_and(|a| a == "--bilan-gpu") {
        let logs = minelib_assets::data_dir().join("logs");
        let _ = std::fs::create_dir_all(&logs);
        let file = std::fs::File::create(logs.join("bilan_gpu.txt")).ok();
        env_logger::Builder::new()
            .filter_level(log::LevelFilter::Info)
            .filter_module("wgpu_core", log::LevelFilter::Error)
            .filter_module("wgpu_hal", log::LevelFilter::Error)
            .filter_module("naga", log::LevelFilter::Error)
            .target(env_logger::Target::Pipe(Box::new(Tee(file, None))))
            .init();
        gpu_report::run_process(args.get(2).map_or("", |s| s.as_str()));
        return Ok(());
    }
    // Journaux : `.minelib/logs/minelib_<date>.log` (un par lancement, 20 gardés),
    // plantages dans `.minelib/logs/crash_<date>.txt`, et `.minelib/minelib.log` = dernier.
    let dir = minelib_assets::data_dir();
    let logs = dir.join("logs");
    let _ = std::fs::create_dir_all(&logs);
    let t = stamp();
    let log_path = logs.join(format!("minelib_{t}.log"));
    let crash_path = logs.join(format!("crash_{t}.txt"));
    prune(&logs, "minelib_", 19);
    prune(&logs, "crash_", 19);
    let file = std::fs::File::create(&log_path).ok();
    let latest = std::fs::File::create(dir.join("minelib.log")).ok();
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(
        // DHT (mainline) : une ligne par nœud injoignable, des centaines par seconde. Iroh
        // (tunnels) : une ligne par paquet envoyé (« poll_send », « tx »), écrite pendant
        // l'envoi : des centaines par seconde en partie, qui ralentissaient le réseau.
        "info,wgpu_core=warn,wgpu_hal=warn,naga=warn,mainline=error,iroh=warn,iroh_relay=warn,\
         iroh_quinn=warn,iroh_quinn_proto=warn,quinn=warn,quinn_proto=warn,tracing=warn,\
         portmapper=warn,netwatch=warn,n0_watcher=warn,hickory_proto=warn,hickory_resolver=warn,\
         swarm_discovery=warn,pkarr=warn",
    ))
    .target(env_logger::Target::Pipe(Box::new(Tee(file, latest))))
    .init();
    log::info!(
        "Minelib v{} ({}) — {} {} — journal : {}",
        update::VERSION,
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        log_path.display()
    );
    // Voie rapide des parties FPS : quand ce jeu sert de passeur, il demande à la box
    // d'ouvrir son port (elle a jusqu'à 10 s pour répondre).
    minelib_net::fast::set_mapper(Box::new(|port| {
        minelib_p2p::portmap::map_udp(port, std::time::Duration::from_secs(10))
    }));
    // Plantage Rust : message, endroit et pile d'appels, dans le journal et un rapport.
    {
        let crash_path = crash_path.clone();
        std::panic::set_hook(Box::new(move |info| {
            let bt = std::backtrace::Backtrace::force_capture();
            let thread = std::thread::current().name().unwrap_or("?").to_string();
            let msg = format!("PLANTAGE (thread {thread}) : {info}\n{bt}");
            log::error!("{msg}");
            let _ = std::fs::write(&crash_path, &msg);
        }));
    }
    // Thread du jeu et du rendu : priorité haute, il passe avant les tâches de fond.
    minelib_platform::raise_thread_priority();
    // Plantage hors Rust (pilote…) : rapport ajouté au journal.
    minelib_platform::install_native_crash_handler(log_path.clone());
    let result = Game::new().and_then(|g| minelib_platform::run("Minelib", g));
    match &result {
        Ok(()) => log::info!("fermeture normale"),
        Err(e) => {
            log::error!("erreur fatale : {e:?}");
            let _ = std::fs::write(&crash_path, format!("erreur fatale : {e:?}\n"));
        }
    }
    result
}

/// Volumes du chat vocal choisis pour chaque joueur (`pseudo=volume`, une ligne chacun).
fn load_volumes(path: &std::path::Path) -> std::collections::HashMap<String, f32> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let (k, v) = l.split_once('=')?;
            Some((k.trim().to_lowercase(), v.trim().parse().ok()?))
        })
        .collect()
}

fn save_volumes(path: &std::path::Path, v: &std::collections::HashMap<String, f32>) {
    let mut lines: Vec<String> = v.iter().map(|(k, x)| format!("{k}={x:.2}")).collect();
    lines.sort();
    let _ = std::fs::write(path, lines.join("\n") + "\n");
}
