//! Une partie en cours : monde, joueur, inventaire, chargement des chunks, sauvegarde.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use anyhow::Result;
use glam::{Mat4, Vec3};
use minelib_platform::{Input, KeyCode, Mouse};

use minelib_render::{FrameParams, FrameStats, Overlay, Renderer, ShadowBox, Ui};

/// Menu Échap d'une partie FPS : coéquipiers (identifiant, pseudo), pauses tactiques qui
/// restent, vote possible maintenant.
pub type VoteChoices = (Vec<(u64, String)>, u8, bool);
use minelib_world::raycast::{raycast, Hit};
use minelib_world::{BlockId, ChunkPos, World, CHUNK_SIZE};

use crate::chat::{self, Chat};
use crate::debug;
use crate::hand::Hand;
use crate::inventory::{self, Inventory, Item};
use crate::jobs::Workers;
use crate::keys::{Action, Bindings};
use crate::player::{MoveInput, Player};
use crate::save::{self, GameMode, GameState, PlayerState, SaveData, WorldMeta, MAX_HEALTH};
use crate::streaming::Streamer;

pub const REACH: f32 = 6.0;
/// (v226) Portée d'un coup à un PNJ de la foule d'essai (blocs, depuis les yeux).
const NPC_REACH: f32 = 4.5;
/// (v227) Foule d'essai partagée en ligne : on entend crier un PNJ frappé par un autre à
/// moins de… (blocs) ; on apprend que le personnage caché a été trouvé à moins de… (blocs).
const NPC_CRY_HEAR: f32 = 24.0;
const FOUND_NEAR: f32 = 64.0;

/// (v227) Volume du cri d'un PNJ frappé à `d` blocs (`None` : trop loin pour l'entendre).
fn npc_cry_gain(d: f32) -> Option<f32> {
    let k = 1.0 - d / NPC_CRY_HEAR;
    (k > 0.0).then_some(0.55 * k * k)
}

/// (v269) Une morsure de L'Évidé : points de vie perdus (sur 20) ; temps entre deux de nos
/// bouchées (s) ; au-delà de cette distance (blocs) il n'est plus dessiné. (v290) « Faire 2x
/// moins mal quand il croque, arrondis un truc cool » : 2 (un cœur) au lieu de 3, la moitié
/// de ce que lui retire une de nos bouchées (`evide::OUR_BITE_HURTS`).
const EVIDE_BITE: u8 = 2;
/// (v312) En ligne : les Évidés qu'on mène sont dits au relais tous les tant de secondes ;
/// les règles que le relais surveille sont celles du jeu.
const EVIDE_EVERY: f32 = 0.05;
const _: () = assert!(EVIDE_BITE == minelib_net::evide::BITE_HURTS);
/// (v272) Une morsure saigne : gouttes du premier jet, puis gouttes par seconde au début,
/// qui s'espacent (divisées par e toutes les `BLEED_FADE_S` secondes), pendant `BLEED_S`.
const BLEED_SPURT: usize = 14;
const BLEED_RATE: f32 = 9.0;
const BLEED_FADE_S: f32 = 3.5;
const BLEED_S: f32 = 12.0;
/// (v272) « En build le sang reste 5 minutes au sol » (hors partie FPS).
const BLOOD_ON_GROUND_S: f32 = 300.0;

/// (v272) Où sont l'endroit visé par L'Évidé et les morsures qui saignent, dans la pose
/// dessinée : (l'endroit visé, (pavé, cube, position) de chaque morsure).
/// (v290) Et le milieu du membre où est l'endroit visé.
type BitePoints = (Option<Vec3>, Vec<(usize, [usize; 3], Vec3)>, Option<Vec3>);

/// (v272) Les pavés du joueur que L'Évidé peut mordre : tout le tour du corps (jambes,
/// poitrine, bras, ventre ; pas la tête), ou les jambes seulement quand il rampe.
fn bite_parts(low: bool) -> &'static [usize] {
    if low {
        &[0, 1]
    } else {
        &[0, 1, 2, 3, 4, 6]
    }
}
/// (v272) Notre bouchée : on mord à `EAT_IN` s, le geste dure `EAT_S` s ; vitesse à laquelle
/// on se plie (rad/s). (v290) « Je n'arrive plus à manger le monstre […] que ça fonctionne
/// quand je clique » : notre bouche n'a plus à arriver sur le morceau (elle n'y arrivait
/// presque jamais : il est deux fois plus grand que nous, et elle ne fait que descendre en arc
/// autour de nos hanches) ; il suffit qu'au moment de mordre il soit encore à portée de nos
/// yeux, à `EAT_SLACK` bloc près.
const EAT_IN: f32 = 0.2;
const EAT_S: f32 = 0.38;
const EAT_SLACK: f32 = 0.4;
const BOW_SPEED: f32 = 9.0;
/// (v290) Vitesse à laquelle notre tête se tourne vers le milieu du membre qu'on mord, et en
/// revient (rad/s) ; elle ne tourne pas de plus de tant (rad).
const EAT_TURN_SPEED: f32 = 6.0;
const EAT_TURN_MAX: f32 = 0.7;
/// (v290) La main vide à la première personne : temps pour se tendre quand un geste commence,
/// pour retomber après (s) ; l'angle dont le bras tendu est baissé autour de l'épaule quand il
/// part du corps ou y revient (rad).
const HAND_UP_S: f32 = 0.07;
const HAND_BACK_S: f32 = 0.22;
const HAND_DOWN: f32 = 1.35;
const EVIDE_DRAWN: f32 = 160.0;
/// (v299) Au plus tant de morts (joueur, PNJ) laissés au sol à la fois.
const FALLEN_MAX: usize = 8;

/// (v249) Durée de l'éblouissement par la foudre qu'on prend (s) : l'écran reste blanc
/// pendant le premier sixième (les arcs en retour), puis la vue revient peu à peu. Une
/// seconde en tout (« celui qui porte l'éclair peut en jeter dix d'affilée quand même »).
const DAZZLE_S: f32 = 1.0;

/// (v249) Part de blanc à l'écran pour un éblouissement `d` (1 au coup de foudre … 0).
fn dazzle_alpha(d: f32) -> f32 {
    (d.clamp(0.0, 1.0) / 0.84).min(1.0).powf(1.7)
}

/// (v227) PNJ visé : dans notre foule d'essai, ou dans celle que mène un autre OP (son pseudo).
#[derive(Clone, Debug)]
enum NpcTarget {
    Mine(usize),
    Theirs(String, usize),
}

/// (v244) Un coup de la Foudre de Zeus demandé au relais du monde, pas encore publié par
/// l'arbitre du chunk : ce qu'on lui enverra avec le laissez-passer, et le PNJ visé par le
/// petit éclair (blessé quand il part).
#[derive(Clone, Debug)]
struct ZeusPending {
    seq: u32,
    act: u8,
    chunk: [i32; 2],
    pos: Vec3,
    to: Vec3,
    seed: u32,
    npc: Option<NpcTarget>,
    /// (v281) Le petit éclair touche quelqu'un : le coup sec partira avec lui.
    hit: bool,
    at: std::time::Instant,
}

/// (v244) Le petit éclair de la Foudre de Zeus touche un joueur ou un PNJ à mi-corps : à tant
/// de blocs au-dessus de ses pieds.
const ARC_BODY: f32 = 0.9;

/// (v281) Celui que vise `Session::aimed_person`.
#[derive(Clone, Debug)]
struct Aimed {
    /// Ses pieds : la foudre chargée y tombe.
    feet: Vec3,
    /// Où le petit éclair le touche : à mi-corps (joueur, PNJ) ; L'Évidé (3,5 blocs debout, un à
    /// plat ventre), là où l'on vise son corps.
    body: Vec3,
    /// Le PNJ, si c'en est un.
    npc: Option<NpcTarget>,
    /// Sa distance à l'œil (blocs).
    t: f32,
}
/// (v281) « Quand on casse un bloc en survie, il doit apparaître en objet au sol à l'endroit
/// du bloc cassé, pas dans mon inventaire directement » : ce qu'on vient de casser part de `at`
/// (le milieu de sa case, d'un écran) et tombe sur le sol sous lui ; on le ramasse en passant
/// dessus (`Items::pick_up`). Avant, il allait dans l'inventaire (perdu s'il était plein). Une
/// tête à skin y va encore : au sol, elle perdrait son skin (le code d'un objet ne le porte
/// pas, voir `Item::code`).
fn drop_broken(
    world: &World,
    inventory: &mut inventory::Inventory,
    items: &mut crate::drops::Items,
    at: Vec3,
    item: Item,
    count: u8,
) {
    if matches!(item, Item::Head(_)) {
        inventory.add(item, count as u32);
        return;
    }
    let (x, z) = (at.x.floor() as i32, at.z.floor() as i32);
    let mut y = at.y.floor() as i32;
    while y > 0 && !world.get(x, y - 1, z).is_solid() {
        y -= 1;
    }
    // (v283) Posé sur le dessus de ce qui le porte (un tapis : un seizième au-dessus de sa
    // case, pas un bloc plus haut).
    let top = minelib_world::registry::shape_boxes(world.get(x, y - 1, z), |dx, dy, dz| {
        world.get(x + dx, y - 1 + dy, z + dz)
    })
    .iter()
    .map(|b| b.1[1])
    .max()
    .unwrap_or(16);
    let rest = (y - 1) as f32 + top as f32 / 16.0;
    let pile = minelib_net::items::Pile {
        item: item.code(),
        count: count.max(1),
    };
    items.drop_broken(at, Vec3::new(at.x, rest, at.z), pile);
}

/// (v283) Ce que rend une pile de neige fine ou de tapis cassée : une couche de neige par
/// couche, un tapis de sa sorte par couche (avant de la casser : ses couleurs sont dans le
/// monde). `None` : pas une pile.
fn layer_drops(world: &World, at: [i32; 3], id: BlockId) -> Option<Vec<(Item, u8)>> {
    use minelib_world::registry::Layers;
    match Layers::of(id)? {
        Layers::Snow(h) => Some(vec![(Item::Block(Layers::Snow(1).block()?), h)]),
        Layers::Carpet { .. } => {
            let mut out: Vec<(Item, u8)> = Vec::new();
            for kind in world.carpet_layers(at)? {
                let item = Item::Block(Layers::Carpet { n: 1, kind }.block()?);
                match out.iter_mut().find(|e| e.0 == item) {
                    Some(e) => e.1 += 1,
                    None => out.push((item, 1)),
                }
            }
            Some(out)
        }
    }
}

/// (v283) Neige fine, tapis : où va la couche que l'on pose et ce que devient sa case.
/// Cliquée sur le dessus d'une pile de la même matière qui a encore de la place, ou dans la
/// case d'une telle pile (clic sur le côté d'un bloc voisin) : une couche de plus (sa sorte
/// de tapis en haut). Sinon une première couche, dans la case libre contre la face visée, sur
/// un bloc qui la porte (la neige : sur un dessus plein). `None` : l'objet n'est pas une
/// couche ; `Some(None)` : rien à poser là.
#[allow(clippy::type_complexity)]
fn layer_placement(
    world: &World,
    hit: &Hit,
    item: BlockId,
) -> Option<Option<([i32; 3], BlockId, Option<Vec<u8>>)>> {
    use minelib_world::registry::Layers;
    let new = Layers::of(item)?;
    let kind = match new {
        Layers::Carpet { kind, .. } => kind,
        Layers::Snow(_) => 0,
    };
    let grow = |at: [i32; 3], id: BlockId| {
        let l = Layers::of(id)?;
        if !l.same_matter(new) {
            return None;
        }
        let more = l.plus_one(kind)?;
        let layers = match l {
            Layers::Carpet { .. } => {
                let mut v = world.carpet_layers(at)?;
                v.push(kind);
                Some(v)
            }
            Layers::Snow(_) => None,
        };
        Some((at, more.block()?, layers))
    };
    if hit.normal == [0, 1, 0] {
        if let Some(r) = grow(hit.block, hit.id) {
            return Some(Some(r));
        }
    }
    let p = hit.adjacent();
    let there = world.get(p[0], p[1], p[2]);
    if let Some(r) = grow(p, there) {
        return Some(Some(r));
    }
    if there.is_targetable() {
        return Some(None);
    }
    // Une première couche : sur un bloc qui la porte.
    let below = world.get(p[0], p[1] - 1, p[2]);
    let top = minelib_world::registry::shape_boxes(below, |dx, dy, dz| {
        world.get(p[0] + dx, p[1] - 1 + dy, p[2] + dz)
    })
    .iter()
    .map(|b| b.1[1])
    .max()
    .unwrap_or(0);
    let holds = match new {
        Layers::Snow(_) => below.is_solid() && top == 16,
        Layers::Carpet { .. } => below.is_solid(),
    };
    Some(holds.then_some((p, item, None)))
}

/// (v281) Seau nommé dans `/give` : `seau` (vide), `seau_eau`, `seau_lave` (aussi en anglais,
/// avec accents, tirets ou espaces). `Some(None)` : le seau vide.
fn bucket_named(want: &str) -> Option<Option<minelib_world::registry::Fluid>> {
    use minelib_world::registry::Fluid;
    let w: String = want
        .to_lowercase()
        .replace(['é', 'è'], "e")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    match w.as_str() {
        "seau" | "bucket" | "seauvide" | "emptybucket" => Some(None),
        "seaueau" | "seaudeau" | "waterbucket" | "bucketwater" => Some(Some(Fluid::Water)),
        "seaulave" | "seaudelave" | "lavabucket" | "bucketlava" => Some(Some(Fluid::Lava)),
        _ => None,
    }
}

/// Taille d'écran nommée dans `/give` : `ecran` (normal), `petit_ecran`, `grand_ecran`,
/// `ecran_geant` (aussi en anglais et avec accents, tirets ou espaces).
fn screen_size(want: &str) -> Option<u8> {
    let w: String = want
        .to_lowercase()
        .replace('é', "e")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    match w.as_str() {
        "screen" | "ecran" | "ecranvideo" => Some(crate::screen::NORMAL),
        "petitecran" | "smallscreen" | "ecranpetit" => Some(0),
        "grandecran" | "largescreen" | "bigscreen" | "ecrangrand" => Some(2),
        "ecrangeant" | "geantecran" | "giantscreen" | "hugescreen" => Some(3),
        _ => None,
    }
}

/// Écran regardé de si loin au plus (pour lui donner le plus d'images par seconde). On
/// n'agit sur un écran (toucher, molette, clavier, casser) qu'à portée de main (`REACH`).
const SCREEN_LOOK: f32 = 64.0;
/// Rotation par unité de souris pour une sensibilité de 1 : 0,022°, comme Counter-Strike
/// (« m_yaw » / « m_pitch »).
const CS_DEGREES_PER_COUNT: f32 = 0.022;
pub const FOV: f32 = 75.0;
const ZOOM_FOV: f32 = 18.0;
const AUTOSAVE_S: f32 = 300.0;
/// Deux appuis sur Espace en moins de ce délai : vol activé / coupé.
const DOUBLE_TAP_S: f32 = 0.3;
/// Chute sans dégâts (blocs).
/// Hauteur de chute sans dégâts (blocs) : au-delà, les dégâts commencent.
pub const SAFE_FALL: f32 = 4.0;
/// Durée d'affichage d'un ping, portée, nombre maximal à l'écran.
const PING_S: f32 = 8.0;
const PING_RANGE: f32 = 400.0;
const MAX_PINGS: usize = 3;

/// (v222) On voit l'éclair tenu par un autre joueur jusqu'à tant de blocs.
const ZEUS_SEEN: f32 = 64.0;
/// (v238) Volume du clic de la lampe torche qu'on allume ou éteint (« réduis le son
/// d'allumage/extinction de la lampe torche de 50 % » : 0,6 avant).
const FLASHLIGHT_CLICK: f32 = 0.3;

/// (v222) Numéro du crépitement de l'éclair d'un autre joueur (jamais celui du sien, 1).
fn crackle_id(pseudo: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in pseudo.bytes() {
        h = (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
    }
    h | 2
}

/// (v220) Survie : temps (s) de clic tenu pour casser un bloc, le même pour tous les blocs
/// pour l'instant. En créatif, un bloc se casse tout de suite.
const BREAK_S: f32 = 0.5;

/// (v220) À l'arrivée dans un monde, on n'y apparaît (chez soi comme chez les autres)
/// qu'une fois tout chargé (`loading_done`). Au moins `LOAD_MIN_S` : en ligne, ce que le
/// relais a de changé autour de soi arrive avant.
///
/// (v283) **RÈGLE DE VIENDAX** (24 novembre, 18 h 55 : « je suis encore en jeu alors que le
/// jeu n'a pas fini d'être chargé : ça ne doit pas être possible ; corrige, note-le bien dans
/// le projet, fais en sorte que dans le code on ne puisse pas changer ça »). Avant, on entrait
/// une fois affichés les chunks à 4 chunks de soi (le reste de la distance de rendu
/// apparaissait sous ses yeux), et de force au bout de 30 s. Maintenant : **toute** la
/// distance de rendu, aucune sortie au bout d'un temps, une seule porte (`loading_done`,
/// appelée par `check_loaded`, le seul endroit qui ferme l'écran de chargement). Les essais
/// `nothing_is_entered_before_everything_is_loaded` échouent si cela change.
const LOAD_MIN_S: f32 = 0.5;

/// (v220) Monde en cours de chargement (voir `Session::loading`).
struct Loading {
    since: std::time::Instant,
    /// Chunks affichés autour de soi, et combien il en faut.
    shown: (usize, usize),
    /// Shaders du monde prêts, et (v283) ceux des lumières si le monde en a (sinon : vrai).
    shaders: bool,
    /// (v295) Part de la barre de chargement (0 à 1) : le temps déjà attendu sur le temps
    /// d'attente estimé (`load_bar`). Elle ne recule jamais.
    bar: f32,
}

impl Loading {
    fn new() -> Self {
        Self {
            since: std::time::Instant::now(),
            shown: (0, 1),
            shaders: false,
            bar: 0.0,
        }
    }
}

/// (v295) Vitesse des chunks (par seconde) supposée au début d'un chargement, tant qu'on en a
/// vu arriver peu (chez Viendax : 40 à 80 par seconde).
const LOAD_CHUNKS_PRIOR: f32 = 40.0;

/// (v295) Secondes qui restent, estimées, avant d'entrer dans le monde, `elapsed` s après le
/// début de l'écran de chargement : la plus longue des attentes de `loading_done` (elles vont
/// en même temps). Les chunks `shown` (affichés, voulus) à la vitesse vue jusqu'ici, les
/// shaders d'après leur durée habituelle (`Renderer::shaders_left`), la météo du serveur
/// (`weather` : arrivée), l'écran au moins `LOAD_MIN_S`.
fn load_left(elapsed: f32, shown: (usize, usize), shaders_left: f32, weather: bool) -> f32 {
    let missing = shown.1.saturating_sub(shown.0) as f32;
    // Une seconde de « vitesse supposée », vite noyée dans ce qu'on voit.
    let rate = (shown.0 as f32 + LOAD_CHUNKS_PRIOR) / (elapsed.max(0.0) + 1.0);
    let chunks = missing / rate.max(1.0);
    let weather = if weather { 0.0 } else { 1.0 };
    let min = (LOAD_MIN_S - elapsed).max(0.0);
    chunks.max(shaders_left).max(weather).max(min)
}

/// (v295) Barre de chargement : le temps déjà attendu sur le temps d'attente estimé (déjà
/// attendu + `left`), jamais pleine avant l'entrée. « La barre de chargement quand on rejoint
/// un monde : le temps ne correspond pas au temps qu'on attend » (Viendax, 24 novembre,
/// 21 h 03) : elle suivait les chunks seuls, pleine en une vingtaine de secondes, puis on
/// attendait encore les shaders du monde (une minute au lancement du jeu, chez lui).
fn load_bar(elapsed: f32, left: f32) -> f32 {
    let e = elapsed.max(0.0);
    (e / (e + left.max(0.0)).max(1e-3)).min(0.99)
}

/// (v283) La seule porte d'entrée dans le monde (voir `LOAD_MIN_S`) : chaque chunk de toute la
/// distance de rendu affiché (maillé et envoyé à la carte graphique ; un chunk vide ou refusé
/// faute de mémoire vidéo compte aussi), les shaders du monde prêts (et ceux des lumières dans
/// un monde qui en a), en ligne la météo du serveur arrivée (`weather`), et l'écran montré au
/// moins `LOAD_MIN_S`. Aucune condition de
/// temps ne l'ouvre : un monde qui ne se charge pas garde son écran (Échap reste possible).
fn loading_done(l: &Loading, weather: bool) -> bool {
    l.shaders
        && weather
        && l.shown.1 > 0
        && l.shown.0 >= l.shown.1
        && l.since.elapsed().as_secs_f32() >= LOAD_MIN_S
}

/// (v295) Le clic gauche est-il pour la Foudre de Zeus (tenue en main) ? Appuyé à cette image
/// **ou** tenu : un clic bref, appuyé et relâché dans la même image (une image lente), n'est
/// pas « tenu », et il cassait alors le bloc visé (« j'ai réussi à casser un bloc avec
/// l'éclair, ça doit être impossible »).
fn zeus_click(selected: Option<Item>, pressed: bool, held: bool) -> bool {
    selected == Some(Item::Zeus) && (pressed || held)
}

/// Réglages de rendu choisis au lancement.
#[derive(Clone, Copy)]
pub struct SessionConfig {
    pub view_distance: i32,
    /// (v309) Partie solo : loin du centre du monde, elle compte depuis une origine proche du
    /// joueur (`World::origin`, voir `origin_near`). Faux en partie FPS en ligne : tout y est
    /// compté depuis l'origine de sa carte (`fpsmap::match_origin`, v322), comme chez les
    /// autres joueurs. (v318) Vrai aussi en construction en ligne : ce qui part au réseau est
    /// en places absolues (`BuildNet`).
    pub far_origin: bool,
    /// (v322) Origine imposée à l'ouverture : celle d'une partie FPS (de la carte, ou de la
    /// partie FPS du serveur de construction), quand le passage en mode FPS rouvre la partie
    /// depuis elle (`Session::wants_rebase`, `Game::rebase_world`).
    pub origin: Option<ChunkPos>,
    /// (v330) Clé de terrain donnée par le relais d'un serveur de construction (graine d'un
    /// monde d'avant la v330, ou `minelib_net::graine::terrain_key`) : la copie du monde ne
    /// l'a pas. Gardée en mémoire seulement (jamais écrite dans ses fichiers).
    pub terrain: Option<u64>,
}

/// (v330) Le terrain (et la graine publique : météo, éclairs) d'un monde de construction dont
/// la copie n'a pas la graine, à partir de la clé que donne le relais : d'un monde
/// `procedural2`, sa clé de terrain ; d'un monde d'avant la v330, sa graine.
pub(crate) fn give_terrain(world: &mut World, key: u64) {
    if world.generator == minelib_world::gen::Generator::Procedural2 {
        world.terrain = minelib_world::gen::Terrain::v2(key);
        world.seed = minelib_net::graine::public_seed(key);
    } else {
        world.seed = key as u32;
        world.terrain = minelib_world::gen::Terrain::v1(key as u32);
    }
}

/// (v309) « Aucun problème jusqu'à 16,7 millions » : à moins de tant de blocs du centre du
/// monde (sur x ou sur z), l'origine reste le centre : les mondes et les cartes d'avant ne
/// changent pas (un f32 y vaut encore au demi-millimètre près, et la v254 y garde l'image et
/// les pas exacts).
const ORIGIN_FREE: f64 = 32_768.0;
/// (v309) L'origine est un multiple de tant de blocs (256 chunks, une zone du relais).
const ORIGIN_STEP: f64 = 4_096.0;
/// (v309) Plus loin que ça de l'origine (sur x ou sur z), la partie est rechargée avec une
/// origine plus proche (`Session::take_rebase`) : un f32 y vaut encore au millimètre près.
const REBASE_AT: f32 = 24_576.0;

/// (v309) Origine (en chunks) des coordonnées locales d'une partie solo dont le joueur est
/// en `abs` (place absolue) : le centre du monde à moins de `ORIGIN_FREE`, sinon le multiple
/// de `ORIGIN_STEP` le plus proche, sur chaque axe.
pub fn origin_near(abs: [f64; 3]) -> ChunkPos {
    let axis = |a: f64| -> i32 {
        if !a.is_finite() || a.abs() < ORIGIN_FREE {
            return 0;
        }
        let blocks = (a / ORIGIN_STEP).round() * ORIGIN_STEP;
        (blocks.clamp(-2.0e9, 2.0e9) as i64 / CHUNK_SIZE as i64) as i32
    };
    // (v309) Ramenée sur la carte bouclée (près de la limite, le multiple le plus proche peut
    // être juste de l'autre côté).
    ChunkPos {
        x: minelib_world::lap::wrap_chunk(axis(abs[0]) as i64),
        z: minelib_world::lap::wrap_chunk(axis(abs[2]) as i64),
    }
}

/// (v309) Le point d'une partie dont l'origine est `ob` (blocs) de la place absolue `a` : la
/// plus proche de l'origine, sur la carte bouclée.
fn local_from(ob: [i32; 3], a: [f64; 3]) -> Vec3 {
    let wrap = minelib_world::lap::wrap_f64;
    Vec3::new(
        wrap(a[0] - ob[0] as f64) as f32,
        a[1] as f32,
        wrap(a[2] - ob[2] as f64) as f32,
    )
}

/// (v309) Ce que le tchat dit des tours de la carte bouclée `l` du joueur.
fn laps_said(l: crate::laps::Laps) -> String {
    match crate::laps::text(l) {
        Some(t) => format!("Tour de la carte ! Tu en es à {t}."),
        None => "De retour sur la carte de départ : aucun tour.".to_string(),
    }
}

/// Pas de la simulation du joueur : 256 fois par seconde, quelle que soit la cadence
/// d'images (même résultat sur toutes les machines, base du multijoueur).
pub const SIM_TICK: f32 = 1.0 / 256.0;
/// Au plus tant de pas par image (125 ms) : après un gel, on ne rattrape pas indéfiniment.
const MAX_TICKS: u32 = 32;

/// État simulé du joueur et état montré (interpolé entre les deux derniers pas).
#[derive(Clone, Copy)]
struct SimState {
    pos: Vec3,
    crouch: f32,
    shown_pos: Vec3,
    shown_crouch: f32,
}

/// Arrêt en cours de mesure (journal) : où et à quelle vitesse était le joueur quand la
/// première touche de la course a été lâchée, depuis combien de temps, longueur de la
/// course, et quelle touche.
#[derive(Clone, Copy)]
struct StopMeas {
    at: Vec3,
    /// Sens de la course (à l'horizontale).
    dir: Vec3,
    speed: f32,
    t: f32,
    run: f32,
    first: &'static str,
    /// « Avancer » a-t-elle été vue relâchée depuis le début de la mesure ?
    fwd_up: bool,
    /// Pendant l'arrêt : une autre touche de déplacement tenue (le joueur continue de
    /// côté, la distance ne dit rien de l'arrêt), ou les pieds en l'air (saut, rebord :
    /// le freinage y est plus doux).
    other: bool,
    air: bool,
}

/// (v318) Ce qu'une partie garde quand elle repart d'une origine plus proche du joueur
/// (`Session::take_carry`, `Game::rebase_world`).
pub struct Carry {
    chat: Chat,
    vel: Vec3,
    /// L'origine (blocs) depuis laquelle comptaient les places gardées.
    from: [i32; 3],
    online: Option<Box<Online>>,
}

impl Carry {
    /// En ligne (construction) : la partie rouverte reprend la connexion au serveur.
    pub fn online(&self) -> bool {
        self.online.is_some()
    }
}

/// (v318) En construction en ligne : la connexion au serveur et ce qu'on en sait.
struct Online {
    build: crate::buildnet::BuildNet,
    voice: Option<crate::voice::Voice>,
    skins: crate::skin::Skins,
    items: crate::drops::Items,
    items_ready: bool,
    items_seeded: bool,
    /// Modifications du joueur pas encore parties (places absolues).
    journal: Vec<([i32; 3], BlockId, BlockId)>,
    thor_seq: u32,
    thor_refused: Option<std::time::Instant>,
    zeus_online: crate::zeus::Online,
    zeus_pending: Vec<ZeusPending>,
    relay_cmds: Vec<minelib_net::client::Cmd>,
    room_started: Option<bool>,
    room_paused: Option<bool>,
    build_match: Option<String>,
    build_match_origin: ChunkPos,
    weather_synced: bool,
    game: GameState,
    saved_mode: Option<GameMode>,
    friends_said: bool,
    light_op: (bool, Option<std::time::Instant>),
    seed_lock: Option<(bool, bool)>,
    relay_seed: Option<u64>,
}

pub struct Session {
    /// Essais : temps avant la prochaine fumigène de MINELIB_SMOKE.
    test_smoke_timer: f32,
    test_fire_timer: f32,
    test_traces_timer: f32,
    /// Objets au sol encore en vol (lancés en cloche) : temps depuis leur apparition.
    ground_fly: std::collections::HashMap<u32, f32>,
    ground_seen: std::collections::HashSet<u32>,
    /// Bombe lâchée en vol : départ, arrivée, temps écoulé ; et dernier état connu
    /// (état, position du porteur).
    bomb_fly: Option<(Vec3, Vec3, f32)>,
    bomb_last: (u8, Option<Vec3>),
    /// (v209) Adversaires repérés par l'équipe (minimap).
    spotting: crate::spotting::Spotting,
    /// (v177) Bombe posée : temps avant le prochain bip, et voyant allumé (s).
    bomb_beep: f32,
    bomb_flash: f32,
    /// Caméra adoucie sur les marches (montée ou descente d'une dalle) : décalage de la vue
    /// qui revient à 0 en un dixième de seconde, et hauteur au sol de l'image d'avant.
    step_offset: f32,
    step_prev: Option<f32>,
    /// Plus haut point atteint en l'air (bruit de réception).
    fall_top: Option<f32>,
    /// Bruits de ses propres pas : distance parcourue depuis le dernier.
    stride: f32,
    /// Temps pas encore simulé (moins d'un pas).
    tick_acc: f32,
    /// Position au pas précédent (interpolation de l'affichage).
    prev_pos: Vec3,
    prev_crouch: f32,
    /// Vraie position simulée pendant que `player` montre la position interpolée.
    sim: Option<SimState>,
    /// Appui sur saut arrivé entre deux pas (pris par le pas suivant).
    pending_jump: bool,
    /// Partie en ligne (hôte ou invité).
    pub net: Option<crate::netgame::NetGame>,
    /// Invité mort, en attente de réapparition.
    net_dead: bool,
    /// Hôte : la partie commence (elle n'est plus proposée par la recherche).
    pub room_started: Option<bool>,
    /// Pause de la partie FPS commencée / finie (hôte : à dire au relais).
    pub room_paused: Option<bool>,
    /// (v243) Serveur de construction : partie FPS ouverte par un OP (son pseudo), et ouverture
    /// demandée par ce joueur (`/start`, `/bot` : le relais vérifie qu'il est OP).
    build_match: Option<String>,
    /// (v322) L'origine (en chunks) de cette partie FPS : celle du jeu de son hôte, que le
    /// relais dit avec elle (`Response::BuildMatch`). On y entre compté depuis elle : les
    /// places de la partie sont les mêmes chez tous.
    build_match_origin: ChunkPos,
    match_opening: bool,
    /// Résultat de la partie à envoyer au relais (classement).
    pub match_result: Option<minelib_net::game::MatchSummary>,
    /// Tableau de fin de partie affiché (résultat, temps depuis la fin).
    pub game_over: Option<(minelib_net::game::MatchSummary, f32)>,
    /// Bandeau de fin de manche (texte, couleur, temps restant).
    banner: Option<(String, [f32; 4], f32)>,
    /// Exclu de la partie (raison) : le jeu la quitte.
    pub kicked: Option<String>,
    /// Fil des éliminations (en haut à droite) : élimination, âge (s).
    feed: Vec<(crate::netgame::Kill, f32)>,
    /// Demandes pour le relais : signaler, amis.
    pub relay_cmds: Vec<minelib_net::client::Cmd>,
    /// Construction en ligne.
    pub build: Option<crate::buildnet::BuildNet>,
    /// Images par seconde moyennes (capacités de garant).
    fps_avg: f32,
    /// Chat vocal (partie en ligne).
    pub voice: Option<crate::voice::Voice>,
    /// Volume choisi pour chaque joueur (pseudo en minuscules → 0..2), et à enregistrer.
    pub voice_volumes: HashMap<String, f32>,
    pub voice_dirty: bool,
    /// Tab + clic droit : souris libre pour régler les volumes.
    pub cursor_free: bool,
    /// Taille de la fenêtre (pour les curseurs de volume).
    screen_size: (f32, f32),
    /// Mort en ligne : coéquipier suivi (pseudo) et rang dans la liste.
    spectating: Option<String>,
    /// (v209) Spectateur : l'écran du joueur regardé (vie, armes, arme en main), montré
    /// comme si c'était soi qui jouait (`Fps::mirror`).
    spec_fps: crate::fps::Fps,
    spectate_idx: usize,
    pub dir: PathBuf,
    pub meta: WorldMeta,
    pub world: World,
    pub player: Player,
    workers: Workers,
    streamer: Streamer,
    view_distance: i32,
    dirty: HashSet<ChunkPos>,
    /// Sélection et commandes `//…` façon WorldEdit.
    we: crate::worldedit::WorldEdit,
    /// Dernier refus de WorldEdit affiché (un seul message par commande).
    bulk_refused: std::time::Instant,
    /// Hauteur grimpée à pleine vitesse sur une échelle depuis le dernier bruit.
    ladder_climb: f32,
    /// (v209) Monde en ligne : échelles des autres joueurs (hauteur à l'image d'avant,
    /// hauteur grimpée depuis le dernier bruit, image où on l'a vu).
    ladders: HashMap<String, (f32, f32, u32)>,
    pub inventory: Inventory,
    /// Heure du jour (0..24) ; arrêtée (`/timefix`).
    time_h: f32,
    time_fixed: bool,
    /// Heure fixe de la carte FPS (`/fps set time`, aucune : soleil de Dust2), lue une fois.
    fps_time: Option<Option<f32>>,
    /// Direction vers le soleil.
    sun: [f32; 3],
    /// (v216) Direction vers la lune, et son âge (jours depuis la pleine lune, 0 … 8).
    moon: [f32; 3],
    moon_age: f32,
    /// (v216) Météo, foudre, pluie, Foudre de Zeus tenue en main.
    storm: crate::storm::Storm,
    /// (v216) Lumière du jour montrée : celle de l'heure (`daylight`), moins sous un ciel
    /// couvert, plus l'éclat d'un éclair.
    daylight_shown: f32,
    /// (v229) Foudre sur un serveur de construction : numéro de notre dernière demande à
    /// l'arbitre (`BuildMsg::ThorAsk`), et quand on a dit pour la dernière fois qu'elle est
    /// réservée aux OP.
    thor_seq: u32,
    thor_refused: Option<std::time::Instant>,
    /// (v241) La Foudre de Zeus de la carte, objet légendaire : prise ou non, ses charges
    /// (`crate::zeus`). Gardée avec la carte.
    legend: crate::zeus::Legend,
    /// (v244) Sur un serveur de construction : ce que le relais du monde dit de l'éclair du
    /// serveur, et nos coups demandés, pas encore publiés.
    zeus_online: crate::zeus::Online,
    zeus_pending: Vec<ZeusPending>,
    daylight: f32,
    break_cooldown: f32,
    /// (v247) Le clic tenu vient de frapper un PNJ : il ne casse rien tant qu'il reste tenu.
    punch_latch: bool,
    /// (v247) Construction en ligne : la météo du serveur est arrivée (le chargement l'attend).
    weather_synced: bool,
    /// (v251) Saisie du tchat pour laquelle l'aide des commandes a été faite.
    hint_for: String,
    place_cooldown: f32,
    /// (v220) Survie : bloc qu'on est en train de casser, et depuis combien de temps (voir
    /// `BREAK_S`).
    dig: Option<([i32; 3], f32)>,
    /// (v220) Geste de la main décidé en cassant ou en posant (`handle_blocks`), joué à
    /// l'image suivante (`Hand::update`).
    gesture: crate::hand::Gesture,
    /// (v220) Interface masquée (touche F1, construction) : voir `hud_off`.
    hud_hidden: bool,
    /// (v222) Éclairs tenus par les autres joueurs (voir `update_remote_bolts`).
    remote_bolts: HashMap<String, crate::lightning::HeldBolt>,
    /// (v220) Un éclair tenu crépitait à l'image d'avant (voir `update_crackles`).
    crackling: bool,
    pub fov: f32,
    /// Sensibilité de la souris (valeur Counter-Strike).
    pub sensitivity: f32,
    /// Sensibilité en mode FPS (réglée à part).
    pub sensitivity_fps: f32,
    /// Skins : le sien et ceux des autres joueurs.
    pub skins: crate::skin::Skins,
    /// Minimap du mode FPS (sites chargés une fois).
    radar: crate::radar::Radar,
    radar_map: bool,
    /// Couleur préférée en FPS (255 : aucune) et dernière envoyée à l'hôte.
    pub fps_color: u8,
    color_sent: Option<u8>,
    /// Volume général des sons (0..1).
    pub volume: f32,
    /// Volume du tchat vocal entrant (Paramètres → Son ; 1 : tel quel).
    pub voice_volume: f32,
    /// Touches choisies dans « Régler les touches ».
    pub keys: Bindings,
    /// Écoulement de l'eau et de la lave.
    fluids: crate::fluids::Fluids,
    /// Temps avant la prochaine brûlure dans la lave.
    lava_burn: f32,
    /// Têtes (blocs « tête », têtes des entités des cartes ; chargées au premier pas), et
    /// leurs triangles.
    heads: Option<crate::heads::Heads>,
    /// Dernière fois que les skins des têtes de joueurs ont été redemandés au relais.
    head_skins_at: Option<std::time::Instant>,
    /// (v269) Ce que L'Évidé a mangé de chacun (par pseudo ; vide : le joueur local) : les
    /// morceaux qui manquent à son corps, qui reviennent en cinq minutes (`flesh::Flesh`).
    flesh: HashMap<String, crate::flesh::Flesh>,
    /// (v269) Les Évidés du monde (`evide`). (v272) En ligne, dans un monde de construction :
    /// ceux de ce joueur (les autres ne les voient pas) ; jamais en partie FPS.
    evides: crate::evide::Horde,
    /// (v312) En ligne : temps depuis le dernier envoi au relais des Évidés qu'on mène (s).
    evide_out: f32,
    /// (v272) Notre bouchée en cours sur un Évidé : (son numéro, pavé, cube visés, temps écoulé
    /// en s, déjà mordu). On se plie vers le morceau, on le mange si notre bouche le touche.
    eating: Option<(u32, usize, [usize; 3], f32, bool)>,
    /// (v272) De combien le haut de notre corps est plié en avant pour manger (rad).
    eat_bow: f32,
    /// (v290) La main vide à la première personne : 0, elle pend le long du corps ; 1, le bras
    /// est tendu devant, le temps d'un geste (frapper, casser, poser).
    hand_raise: f32,
    /// (v290) De combien notre tête est tournée (lacet, tangage, rad) vers le milieu du
    /// membre de L'Évidé qu'on mord, en plus de notre regard : la caméra à la première
    /// personne et notre modèle ; revient à zéro après la morsure.
    eat_turn: (f32, f32),
    /// (v272) L'endroit de notre corps que L'Évidé mordra ensuite (pavé, cube) : il le vise,
    /// se penche jusqu'à lui et le suit s'il bouge.
    evide_spot: Option<(usize, [usize; 3])>,
    /// (v272) Nos morsures qui saignent : (pavé, cube d'où le sang goutte, âge en s, gouttes
    /// dues).
    bleeding: Vec<(usize, [usize; 3], f32, f32)>,
    /// (v272) Où sont, dans la pose dessinée, l'endroit visé et les morsures qui saignent
    /// (écrit par `render`, lu à l'image suivante : la pose exacte, objet tenu compris, n'est
    /// connue que là).
    bite_points: std::cell::RefCell<BitePoints>,
    /// (v290) Pour l'anticrénelage (« le monstre a de la rémanence ») : les repères des pavés
    /// de chaque Évidé dessiné à l'image d'avant (par numéro), et la pose du joueur local
    /// dessiné en F5 à l'image d'avant (avec sa révérence), voir `entities_moved`.
    evide_seen: std::cell::RefCell<HashMap<u32, Vec<Mat4>>>,
    /// (v299) Les morts (joueur, PNJ) tombés là, en construction : leur corps fond en sang,
    /// leurs os restent 5 minutes (`corpse::Fallen`) ; le numéro du prochain.
    fallen: Vec<crate::corpse::Fallen>,
    fallen_next: u64,
    own_seen: std::cell::Cell<Option<(crate::player_model::Prev, f32)>>,
    /// (v305) La vue de la caméra F5 à l'image d'avant : la coupe de son corps par l'écran
    /// (`player_model::cut`) suit l'écran, son mouvement aussi.
    cut_view: std::cell::Cell<Option<Mat4>>,
    /// (v264) Plaies des joueurs de la partie FPS (« un impact dans le joueur ») : par pseudo
    /// (vide : le joueur local), jusqu'à sa mort ou la fin de la manche, comme les traces sur
    /// les blocs. Dessinées sur son modèle (`player_model::wound_model`).
    wounds: HashMap<String, Vec<crate::player_model::Wound>>,
    /// (v256) Tenue d'équipe du joueur local en partie FPS (équipe, quand elle a été
    /// cherchée, skin), relue de temps en temps (`worn_skin`).
    #[allow(clippy::type_complexity)]
    team_skin:
        std::cell::RefCell<Option<(u8, std::time::Instant, std::sync::Arc<crate::skin::Skin>)>>,
    /// Points de mesure de la lumière de chaque chunk : combien sont aux têtes d'entités
    /// (les suivants sont aux cadres).
    probe_split: HashMap<ChunkPos, usize>,
    /// (v255) `/fps showelements` : portes, sites et départs de la carte FPS montrés à travers
    /// les murs, hors partie (`None` : masqués).
    fps_elements: Option<FpsElements>,
    /// Cadres des murs (cartes importées).
    frames: Option<crate::frames::Frames>,
    /// Temps passé à avancer sans touche tenue (diagnostic), et déjà noté au journal.
    coast: f32,
    coast_logged: bool,
    /// Touche « avancer » qui remonte mal : règle apprise pour ce joueur.
    stick: crate::stick::Guard,
    fwd_was: bool,
    run_was: bool,
    /// Double appui sur « avancer » (comme dans Minecraft) : temps restant pour le second
    /// appui, et course lancée ainsi (jusqu'à ce qu'« avancer » soit lâchée).
    fwd_tap: f32,
    tap_sprint: bool,
    /// Mesure des arrêts, pour le journal : d'où la course est partie, et l'arrêt en
    /// cours de mesure (depuis la première des deux touches lâchée).
    run_from: Option<Vec3>,
    stop_meas: Option<StopMeas>,
    coast_air: f32,
    /// Ligne de l'écran de débogage : touches tenues et déplacement demandé.
    move_debug: String,
    /// Le joueur a été prévenu qu'un logiciel tape ses touches.
    injected_warned: bool,
    /// Touches filtrées de Windows : regardées une fois par partie, dans ce temps (s).
    filter_keys_in: Option<f32>,
    /// Bilan des retards de la touche avancer (journal toutes les 5 minutes).
    lag_book: crate::keylag::LagBook,
    /// Test des touches en cours (`/testtouche`) : le jeu ne reçoit ni touches ni clics.
    key_test: Option<crate::keytest::KeyTest>,
    /// Retard mesuré de la touche « avancer » (`/testtouche`) et sa compensation.
    key_lag: Option<crate::keycomp::Setting>,
    key_comp: crate::keycomp::Comp,
    /// « Avancer » et « courir » tenues d'après le clavier (à la dernière image).
    fwd_key: bool,
    run_key: bool,
    /// Mode FPS : il tire ou vise (le chemin dû n'est alors pas rendu, voir `keycomp`).
    fps_aiming: bool,
    autosave: f32,
    hit: Option<Hit>,
    /// Cadre visé (son rang dans `frames`).
    frame_aim: Option<usize>,
    /// Objets au sol et coffres (construction).
    items: crate::drops::Items,
    /// `items` est-il prêt (sauvegarde relue, ou remise au relais par le propriétaire) ?
    items_ready: bool,
    /// (v309) Origine (blocs, x et z) depuis laquelle les places de `drops::FILE` du monde
    /// sont comptées : celle de la partie une fois les objets enregistrés depuis elle.
    items_origin: [i32; 2],
    /// (v309) Partie qui peut compter depuis une origine proche du joueur (solo, pas une
    /// carte FPS : `SessionConfig::far_origin`).
    far_allowed: bool,
    /// (v309) Téléporté loin de l'origine : sa place absolue exacte, enregistrée telle quelle
    /// (la partie est rouverte depuis une origine proche d'elle à la fin de l'image).
    teleport_to: Option<[f64; 3]>,
    /// (v322) Une partie FPS qui se jouera loin de l'origine de la partie (la partie contre les
    /// bots autour des départs d'une carte lointaine), ou depuis une autre origine (la partie
    /// FPS d'un serveur de construction) : l'origine depuis laquelle rouvrir la partie d'abord
    /// (`wants_rebase`, `Game::rebase_world` : `SessionConfig::origin`).
    rebase_origin: Option<ChunkPos>,
    /// (v309) Tours de la carte bouclée de l'origine de la partie (`World::origin`, sur la
    /// carte) : ceux du joueur, moins la limite qu'il a passée depuis elle (`laps_at`).
    lap_base: crate::laps::Laps,
    /// (v309) Les tours du joueur à l'image d'avant : un changement est dit au tchat
    /// (`watch_laps`).
    laps_seen: crate::laps::Laps,
    /// (v309) Ceux d'un `/tp` loin de l'origine (avec `teleport_to`), enregistrés tels quels.
    teleport_laps: Option<crate::laps::Laps>,
    /// (v318) Notre dernier `/tp` en ligne : ses tours, la place sur la carte, quand (ils
    /// comptent quand le relais nous y place, `BuildSpawn`).
    tp_laps_online: Option<(crate::laps::Laps, [f64; 3], std::time::Instant)>,
    /// (v220) Monde pas encore chargé (voir `LOAD_MIN_S` et `loading_done`, la seule porte) :
    /// on n'y apparaît pas. Ni touches ni physique (le joueur reste où il arrive), sa position
    /// ne part pas aux autres (construction en ligne) et il ne se présente pas à l'hôte (partie
    /// FPS) ; l'écran de chargement cache le monde. (v283) Revient après un téléport ou une
    /// réapparition là où le monde n'est pas chargé (`arrive_elsewhere`).
    loading: Option<Loading>,
    items_seeded: bool,
    /// (v311) `/deleteworld` vient d'être tapé : le jeu ouvre l'écran de confirmation.
    pub delete_world_asked: bool,
    /// Horloge du ciel : n'avance qu'avec l'heure (nuages immobiles à heure fixe).
    sky_clock: f32,
    overlay: Overlay,
    pub chat: Chat,
    /// Mode de jeu, vol autorisé en survie (/fly), santé.
    pub game: GameState,
    /// Point de réapparition (là où le monde a été ouvert, ou (v223) `/setspawn`).
    spawn: Vec3,
    /// (v223) `spawn` vient de `/setspawn` : enregistré avec le monde.
    spawn_set: bool,
    /// Hauteur d'où le joueur tombe (pour les dégâts de chute).
    fall_from: Option<f32>,
    /// Temps depuis le dernier appui sur Espace (double appui = vol).
    since_space: f32,
    /// Voile rouge après une blessure (s).
    hurt: f32,
    /// (v249) Ébloui par la foudre qu'on vient de prendre (1 : l'écran est blanc … 0).
    dazzle: f32,
    /// Pings (mode FPS) : point visé et âge en secondes.
    pings: Vec<(Vec3, f32)>,
    /// Mode FPS : marcher / s'accroupir actifs (maintenus ou basculés).
    fps_walk_on: bool,
    fps_duck_on: bool,
    /// Mode FPS : vie sur 100, armes, tir, menu d'achat.
    pub fps: crate::fps::Fps,
    /// Main et objet tenu (première personne), et leur éclairage.
    hand: Hand,
    hand_light: [f32; 3],
    /// (v201) Lampe torche allumée, vaut pour toutes les lampes qu'on tient. (v215) Clic
    /// molette pour l'allumer ou l'éteindre ; éteinte quand on arrive dessus dans la barre.
    flashlight_on: bool,
    /// (v215) Réglage du faisceau de la lampe (0 : large, 1 : serré ; molette, lampe
    /// allumée), gardé d'une partie à l'autre (`flashlight::saved_focus`).
    flashlight_focus: f32,
    /// Vue (F5) : 0 première personne, 1 troisième personne de dos, 2 de face.
    perspective: u8,
    /// Écrans vidéo posés dans le monde (objet « Écran vidéo »), chargés à la première
    /// image depuis le dossier du monde.
    screens: Vec<crate::screen::Placed>,
    screens_loaded: bool,
    /// Écrans montrés : les siens (`screens`), puis ceux des joueurs proches avec leur
    /// propriétaire (regarder ensemble, en construction en ligne). Refaits à chaque image.
    shown: Vec<(crate::screen::Placed, Option<String>)>,
    /// Regarder ensemble : écrans des autres et vidéos partagées.
    party: crate::watch::Party,
    /// Ce qu'affichent les écrans (une page chacun).
    displays: crate::screen::Displays,
    /// Dernier écran regardé ou touché (cible de `/yt`).
    screen_last: Option<u32>,
    /// Touche répétée dans le champ de texte d'un écran (effacer, flèches…).
    screen_repeat: Option<(KeyCode, f32)>,
    /// Écran visé par le réticule : indice (dans `shown`), distance et point visé dessus
    /// (0..1).
    screen_aim: Option<(usize, f32, [f32; 2])>,
    /// Écran touché dont la touche « utiliser » n'est pas encore relâchée.
    screen_touch: Option<u32>,
    /// Un écran vient d'être cassé : rien d'autre n'est cassé avant que la touche
    /// « casser » soit relâchée.
    screen_broke: bool,
    /// Marche du modèle du joueur : phase (radians) et ampleur 0..1.
    walk_phase: f32,
    /// Marche des autres joueurs (par pseudo) : comme la sienne en F5, calculée sur leur
    /// déplacement affiché (fluide, quel que soit le rythme des messages).
    walks: HashMap<String, Walk>,
    /// (v205) Mode de filtrage des écrans en solo, retenu une fois pour la session : la
    /// liste noire demande un fichier installé, et on ne va pas le chercher à chaque image.
    solo_filtre: Option<&'static str>,
    /// (v261) Refus des écrans déjà dits dans le tchat, et quand (pas deux fois de suite).
    refus_dit: std::collections::HashMap<u8, std::time::Instant>,
    /// (v191) Numéro de l'image en cours, pour oublier les joueurs partis (voir `Walk`).
    walk_stamp: u32,
    /// (v172) Skins des autres joueurs envoyés à la carte (ils sont dessinés par elle) ;
    /// tenus à jour pendant le dessin de l'image.
    skin_layers: std::cell::RefCell<crate::player_model::SkinLayers>,
    /// (v197) Tableaux réutilisés d'une image à l'autre : avec 4 000 joueurs, les allouer
    /// et les libérer à chaque image, c'était près d'un mégaoctet recopié 230 fois par
    /// seconde pour rien.
    scratch: std::cell::RefCell<Scratch>,
    /// (v197) « Le soleil atteint-il ce point ? », gardé par demi-bloc : un rayon de
    /// 64 blocs par pavé d'ombre et par image (jusqu'à 48) revenait à refaire le même
    /// calcul des dizaines de fois de suite pour un joueur qui marche.
    sun_seen: std::cell::RefCell<SunSeen>,
    /// (v165, essai temporaire) Joueurs simulés autour de soi en solo (voir `crowd`), et
    /// essai voulu (v185 : `/npc stresstest true|false`, éteint par défaut).
    crowd: Option<crate::crowd::Crowd>,
    crowd_on: bool,
    /// (v227) Foule d'essai menée en construction en ligne (`Some` : on la mène, le relais
    /// nous a dit OP) : ce qu'il reste à en dire aux joueurs proches.
    crowd_out: Option<crate::crowd::Outbox>,
    /// (v227) `/npc stresstest on` en ligne : réponse du relais attendue (OP ou non).
    crowd_probe: Option<std::time::Instant>,
    /// (v227) Foules d'essai menées par d'autres joueurs (OP), par pseudo de celui qui la mène.
    remote_crowds: HashMap<String, crate::crowd::RemoteCrowd>,
    /// (v231) `/amis` vient d'être tapé : la liste reçue va dans le tchat.
    pub friends_said: bool,
    /// (v232) Monde ouvert en mode FPS par « Cartes FPS → Jouer contre des bots » : le mode
    /// qu'il avait, enregistré à sa place (il le retrouve en l'ouvrant depuis « Mondes »).
    pub saved_mode: Option<GameMode>,
    /// (v200, essai) Joueurs et objets reflétés dans l'eau sans raytracing (`/npc reflets
    /// on|off`, allumé par défaut), et l'eau qu'on voit (cherchée toutes les quelques
    /// images, pendant le rendu).
    water_mirror_on: bool,
    seen_water: std::cell::RefCell<crate::underwater::SeenWater>,
    /// (v172) Pseudo du joueur (compte connecté ; vide sinon), et partie contre des bots
    /// voulue en mode FPS dans un monde solo (`/bots off` l'arrête).
    pub pseudo: String,
    bots_auto: bool,
    walk_amp: f32,
    /// (v303) Le reste de l'allure de notre modèle (`player_model::Pose`) : en l'air,
    /// direction du déplacement par rapport au regard, course ; et accroupi en vol qui
    /// descend (0 … 1).
    anim: (f32, f32, f32),
    fly_crouch: f32,
    /// Portes et trappes ouvertes pendant la partie : bloc fermé d'origine (pour refermer).
    opened: HashMap<[i32; 3], BlockId>,
    /// Raytracing : coin de la grille envoyée au GPU, changements depuis, délai.
    vox_origin: Option<[i32; 3]>,
    vox_dirty: bool,
    vox_timer: f32,
    /// Grille du raytracing en cours de calcul (thread de fond).
    vox_job: Option<std::sync::mpsc::Receiver<VoxelGrid>>,
    lights_timer: f32,
    /// Faces visibles des miroirs autour du joueur (raytracing).
    mirror_planes: Vec<MirrorPlane>,
    /// Lumières placées dans ce monde (éditeur de lumières, touche L) et leurs réglages.
    pub lights: crate::lights::Lights,
    pub light_editor: crate::light_editor::Editor,
    /// Construction en ligne : l'éditeur de lumières est réservé aux OP du serveur. Demande
    /// envoyée au relais (la liste des OP ne répond qu'à eux), en attente de sa réponse.
    light_op_probe: Option<std::time::Instant>,
    /// (v224) Lumières de l'éditeur partagées sur un serveur de construction (voir
    /// `lightnet`) ; dernier refus dit au joueur ; le relais a répondu qu'on est OP (pour
    /// `/lumieres effacer`), et un `/lumieres effacer` attend cette réponse.
    light_sync: Option<crate::lightnet::LightSync>,
    /// (v224) Messages des lumières mis de côté par `update_items` (qui vide les états
    /// publiés reçus) pour `update_light_sync`.
    light_inbox: Vec<minelib_net::game::BuildMsg>,
    light_denied_said: Option<std::time::Instant>,
    light_op_ok: bool,
    light_clear_asked: bool,
    /// (v328) Sur un serveur de construction, la graine du monde : (verrouillée, ce joueur peut
    /// la voir et télécharger la carte), comme le relais le dit (`Event::SeedLock`). `None` :
    /// pas encore dit (rien de permis d'ici là).
    seed_lock: Option<(bool, bool)>,
    /// (v328) `/seed lock` vient d'être demandé au relais (sa réponse se lit au tchat : pas
    /// deux fois le même message).
    seed_lock_asked: Option<std::time::Instant>,
    /// (v330) La graine du monde, dite par le relais à qui peut la voir (OP, ou graine
    /// déverrouillée) : elle n'est plus dans la copie du monde d'un serveur de construction.
    relay_seed: Option<u64>,
    /// (v330) Clé de terrain d'un monde `procedural2` (de sa sauvegarde, ou donnée par le
    /// relais) : « Télécharger la carte » la donne à la copie. `None` : monde d'avant.
    terrain_key: Option<u64>,
    /// (v330) Clé de terrain donnée par le relais à l'ouverture (`SessionConfig::terrain`) :
    /// la partie rouverte depuis une autre origine la reprend.
    pub terrain_given: Option<u64>,
    /// Lumières à afficher cette image, et état de leurs shaders (« Préparation des
    /// lumières… » tant qu'ils compilent).
    lights_wanted: bool,
    /// (v202) Lumières de l'image où les joueurs font leur ombre (place, portée) : un joueur
    /// tout près d'elles fait une ombre géante, il passe avant ceux qui sont seulement près
    /// de la caméra.
    shadow_lights: Vec<(Vec3, f32)>,
    /// (v222) Lampes allumées des autres joueurs qui éclairent à cette image : leur pseudo
    /// et leur bit de porteur (voir `remote_lamp_lights`).
    remote_lamps: Vec<(String, u32)>,
    lights_state: minelib_render::DynState,
    /// Déjà fait dans cette partie : lumières notées comme utilisées, refus dit au joueur.
    lights_marked: bool,
    lights_failed_said: bool,
}

/// Distance de rendu maximale réglable (chunks).
pub const MAX_VIEW_DISTANCE: i32 = 64;

/// Distance de rendu que la mémoire vive permet : les chunks détaillés gardés en mémoire
/// (blocs, lumière…) prennent environ 200 Ko chacun ; on s'autorise 60 % de la mémoire
/// disponible au lancement du monde.
pub fn ram_view_limit() -> i32 {
    let (_, available) = minelib_platform::memory_info();
    if available == 0 {
        return MAX_VIEW_DISTANCE;
    }
    let budget = available as f64 * 0.6;
    let chunks = budget / (330.0 * 1024.0);
    // Zone gardée ≈ carré de (2r + 3) chunks de côté.
    let r = ((chunks.sqrt() - 3.0) / 2.0).floor() as i32;
    r.clamp(2, MAX_VIEW_DISTANCE)
}

/// (v322) Réglages FPS du monde `dir` comptés depuis l'origine de la partie `world`
/// (`Session::fps_map`, pris champ par champ là où la partie FPS est empruntée).
fn fps_map_of(dir: &std::path::Path, world: &World) -> minelib_net::game::FpsMap {
    crate::fpsmap::local(&crate::fpsmap::load(dir), world.origin_blocks())
}

/// (v322) Origine des coordonnées locales d'une partie qui s'ouvre (en chunks) : celle qui est
/// imposée (`SessionConfig::origin` : celle d'une partie FPS, quand le passage en mode FPS
/// rouvre la partie) ; dans une partie FPS en ligne (`far_origin` faux), celle de sa carte
/// (`fps` : `fpsmap::origin`, la même chez tous les joueurs de la partie, le garant et le
/// vérificateur ; sans zones, le centre du monde) ; sinon près du joueur en `abs`
/// (`origin_near`, v309), en solo (mode FPS compris : la partie contre les bots se place
/// d'elle-même, `Session::solo_fps_far`) comme en construction en ligne (v318). Avant la v322,
/// le mode FPS et les cartes FPS comptaient toujours depuis le centre du monde (refusés loin
/// de lui).
fn opening_origin(cfg: &SessionConfig, fps: Option<ChunkPos>, abs: [f64; 3]) -> ChunkPos {
    if let Some(o) = cfg.origin {
        return o;
    }
    if !cfg.far_origin {
        return fps.unwrap_or(ChunkPos { x: 0, z: 0 });
    }
    origin_near(abs)
}

impl Session {
    /// Ouvre un monde enregistré.
    pub fn open(dir: PathBuf, cfg: SessionConfig) -> Result<Self> {
        let (data, mut world) = save::load_world(&dir)?;
        // (v330) Serveur de construction : le terrain de la clé que donne le relais (la copie
        // du monde ne l'a pas) ; d'un monde d'avant la v330, c'est sa graine.
        let terrain_given = cfg.terrain.filter(|k| *k != 0);
        if let Some(k) = terrain_given {
            give_terrain(&mut world, k);
        }
        let terrain_key = (world.generator == minelib_world::gen::Generator::Procedural2)
            .then(|| terrain_given.or(data.meta.key))
            .flatten();
        let dir_for_lights = dir.clone();
        // (v309) Place absolue du joueur (enregistrée en double précision).
        let (mut abs, mut player, fresh) = match data.player {
            Some(p) => {
                let mut pl = Player::new(Vec3::ZERO);
                pl.yaw = p.yaw;
                pl.pitch = p.pitch;
                pl.flying = p.flying;
                (p.pos, pl, false)
            }
            None => ([8.5, 0.0, 8.5], Player::new(Vec3::ZERO), true),
        };
        // (v198) Position de départ donnée (essais) : gardée aussi dans un monde neuf. (v309)
        // Place absolue (jusqu'à 16,7 millions de blocs et au-delà).
        let mut start_given = false;
        let mut demo: Option<f64> = None;
        // (v309) À la première ouverture seulement : une partie rouverte depuis une autre
        // origine (`Game::rebase_world`) reprend la place enregistrée, pas celle des essais.
        static START_USED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        let start = std::env::var("MINELIB_START")
            .ok()
            .filter(|_| !START_USED.swap(true, std::sync::atomic::Ordering::Relaxed));
        if let Some(start) = start {
            let v: Vec<f64> = start
                .split(',')
                .filter_map(|s| s.trim().parse().ok())
                .collect();
            if v.len() >= 3 && v[..3].iter().all(|a| a.is_finite()) {
                abs = [v[0], v[1], v[2]];
                player.flying = true;
                start_given = true;
            }
            if v.len() >= 5 {
                player.yaw = (v[3] as f32).to_radians();
                player.pitch = (v[4] as f32).to_radians();
            }
        }
        // (v309) Origine des coordonnées locales : près du joueur, loin du centre du monde
        // (`origin_near`, `World`). Tout ce que la partie compte (joueur, chunks chargés,
        // objets, lumières…) l'est depuis elle ; ce qui est enregistré reste absolu. (v322)
        // Partout sauf dans une partie FPS en ligne (comptée depuis l'origine de sa carte) :
        // en solo, en mode FPS aussi, et en construction en ligne (v318).
        let far = cfg.far_origin;
        // (v309) Sur la carte bouclée (`laps`) : une place enregistrée ou donnée hors de la
        // carte (`MINELIB_START`, une partie d'avant) y est ramenée, ses tours comptés.
        let mut laps = if far { data.laps } else { [0, 0] };
        if far {
            for (k, i) in [(0usize, 0usize), (1, 2)] {
                if let Some(e) = crate::laps::exact(0, abs[i]) {
                    let (l, at) = crate::laps::on_map(e);
                    laps[k] = laps[k].saturating_add(l);
                    abs[i] = at;
                }
            }
        }
        let origin = opening_origin(
            &cfg,
            crate::fpsmap::origin(&crate::fpsmap::load(&dir)),
            abs,
        );
        world.set_origin(origin);
        if origin != (ChunkPos { x: 0, z: 0 }) {
            let o = world.origin_blocks();
            log::info!(
                "loin du centre du monde : la partie compte depuis l'origine {} / {} (place du joueur : {:.1} / {:.1} / {:.1})",
                o[0], o[2], abs[0], abs[1], abs[2]
            );
        }
        let ob = world.origin_blocks();
        player.pos = local_from(ob, abs);
        // (v309) Les tours de l'origine : ceux du joueur, moins la limite qu'il a passée depuis
        // elle (près de la limite, l'origine peut être de l'autre côté).
        let lap_base = {
            let cross = |o: i32, v: f32| minelib_world::lap::split(o as i128 + v.floor() as i128).0;
            [
                laps[0].saturating_sub(cross(ob[0], player.pos.x)),
                laps[1].saturating_sub(cross(ob[2], player.pos.z)),
            ]
        };
        // (v272) Les Évidés enregistrés avec ce monde. (v309) Comptés depuis l'origine.
        let evides_saved = crate::evide::Horde::load(&dir, ob);
        // Chunks autour du joueur générés tout de suite (sol sous ses pieds).
        let c = ChunkPos::containing(player.pos.x.floor() as i32, player.pos.z.floor() as i32);
        for dz in -1..=1 {
            for dx in -1..=1 {
                world.ensure_chunk(ChunkPos {
                    x: c.x + dx,
                    z: c.z + dz,
                });
            }
        }
        if fresh {
            // (v198) MINELIB_START garde la main, même dans un monde neuf et la démo (essais :
            // se placer où l'on veut).
            let ground = world.top_solid(8, 8).unwrap_or(64);
            if !start_given {
                player.pos.y = ground as f32 + 1.0;
            }
            if std::env::var("MINELIB_DEMO").is_ok_and(|v| v.trim() == "1") {
                build_demo(&mut world);
                demo = Some(world.top_solid(8, 6).unwrap_or(64) as f64);
                if !start_given {
                    player.pos =
                        Vec3::new(8.5, world.top_solid(8, 6).unwrap_or(64) as f32 + 1.0, 6.5);
                    player.yaw = std::f32::consts::PI;
                    player.pitch = -0.12;
                }
            }
        }
        let game = data.game;
        if game.mode.mortal() && !game.can_fly {
            player.flying = false;
        }
        // (v223) Point d'apparition choisi par `/setspawn`, sinon là où l'on ouvre le monde.
        // (v309) Enregistré à sa place absolue.
        let spawn_set = data.spawn.is_some();
        let spawn = data.spawn.map_or(player.pos, |a| local_from(ob, a));
        // (v216) Météo du monde. MINELIB_METEO=clear|rain|storm : imposée et figée (essais).
        let mut storm = crate::storm::Storm::new(u64::from(world.seed));
        storm.weather.load(&data.weather);
        if let Some(k) = std::env::var("MINELIB_METEO")
            .ok()
            .and_then(|v| crate::weather::Kind::parse(v.trim()))
        {
            storm.weather.fix(true);
            storm.weather.force(k);
        }
        // (v216) MINELIB_LUNE=<jours> : âge de la lune au départ (essais du ciel).
        let test_moon = std::env::var("MINELIB_LUNE")
            .ok()
            .and_then(|v| v.trim().parse::<f32>().ok())
            .map_or(0.0, |a| a.rem_euclid(crate::daytime::MOON_DAYS));
        // (v216) MINELIB_HEURE=<h> : commencer à cette heure, arrêtée (essais du ciel).
        let test_hour = std::env::var("MINELIB_HEURE")
            .ok()
            .and_then(|v| v.trim().parse::<f32>().ok())
            .map(|h| h.rem_euclid(24.0));
        let time_h = if let Some(h) = test_hour {
            h
        } else if std::env::var("MINELIB_NIGHT").is_ok_and(|v| v.trim() == "1") {
            0.0
        } else {
            data.time
        };
        let radius = cfg
            .view_distance
            .clamp(2, MAX_VIEW_DISTANCE)
            .min(ram_view_limit());
        log::info!("monde « {} » (graine {})", data.meta.name, data.meta.seed);
        Ok(Self {
            test_smoke_timer: 0.0,
            test_fire_timer: 0.0,
            test_traces_timer: 0.0,
            stride: 0.0,
            fall_top: None,
            step_offset: 0.0,
            step_prev: None,
            ground_fly: std::collections::HashMap::new(),
            ground_seen: std::collections::HashSet::new(),
            bomb_fly: None,
            bomb_last: (0, None),
            spotting: Default::default(),
            bomb_beep: 0.0,
            bomb_flash: 0.0,
            tick_acc: 0.0,
            prev_pos: Vec3::ZERO,
            prev_crouch: 0.0,
            sim: None,
            pending_jump: false,
            net: None,
            net_dead: false,
            match_result: None,
            game_over: None,
            banner: None,
            feed: Vec::new(),
            kicked: None,
            relay_cmds: Vec::new(),
            build: None,
            fps_avg: 0.0,
            spectating: None,
            spec_fps: crate::fps::Fps::new(),
            spectate_idx: 0,
            voice: None,
            voice_volumes: HashMap::new(),
            voice_dirty: false,
            cursor_free: false,
            screen_size: (1280.0, 720.0),
            room_started: None,
            room_paused: None,
            build_match: None,
            build_match_origin: ChunkPos { x: 0, z: 0 },
            match_opening: false,
            dir,
            workers: Workers::new(world.terrain, world.generator, world.origin()),
            streamer: Streamer::new(radius),
            meta: data.meta,
            world,
            player,
            view_distance: cfg.view_distance.clamp(2, MAX_VIEW_DISTANCE),
            dirty: HashSet::new(),
            we: crate::worldedit::WorldEdit::default(),
            bulk_refused: std::time::Instant::now() - std::time::Duration::from_secs(10),
            ladder_climb: 0.0,
            ladders: HashMap::new(),
            inventory: data.inventory.unwrap_or_else(Inventory::starter),
            time_h,
            time_fixed: data.time_fixed || test_hour.is_some(),
            fps_time: None,
            sun: crate::daytime::sun_dir(time_h),
            moon: crate::daytime::moon_dir(time_h, test_moon),
            moon_age: test_moon,
            storm,
            daylight_shown: crate::daytime::daylight(time_h),
            thor_seq: 0,
            thor_refused: None,
            legend: crate::zeus::Legend::load(&data.zeus),
            zeus_online: crate::zeus::Online::default(),
            zeus_pending: Vec::new(),
            daylight: crate::daytime::daylight(time_h),
            break_cooldown: 0.0,
            punch_latch: false,
            weather_synced: false,
            hint_for: String::new(),
            dig: None,
            gesture: crate::hand::Gesture::None,
            hud_hidden: false,
            remote_bolts: HashMap::new(),
            crackling: false,
            place_cooldown: 0.0,
            fov: FOV,
            sensitivity: minelib_assets::DEFAULT_SENSITIVITY,
            sensitivity_fps: minelib_assets::DEFAULT_SENSITIVITY,
            fps_color: 255,
            color_sent: None,
            radar: crate::radar::Radar::default(),
            skins: crate::skin::Skins::default(),
            radar_map: false,
            volume: 0.6,
            voice_volume: 1.0,
            keys: Bindings::default(),
            fluids: Default::default(),
            lava_burn: 0.0,
            heads: None,
            head_skins_at: None,
            team_skin: Default::default(),
            eating: None,
            eat_bow: 0.0,
            hand_raise: 0.0,
            eat_turn: (0.0, 0.0),
            evide_spot: None,
            bleeding: Vec::new(),
            bite_points: Default::default(),
            evide_seen: Default::default(),
            fallen: Vec::new(),
            fallen_next: 0,
            own_seen: Default::default(),
            cut_view: Default::default(),
            // Essais : MINELIB_EVIDE="x,y,z,lacet°[,posture]" pose d'avance un Évidé (figé dans
            // la posture nommée : debout, court, rampe, mord, mange ; vivant sans posture) ;
            // plusieurs séparés par « ; ».
            evide_out: 0.0,
            evides: {
                let mut horde = evides_saved;
                for one in std::env::var("MINELIB_EVIDE").unwrap_or_default().split(';') {
                    let f: Vec<&str> = one.split(',').map(str::trim).collect();
                    let n: Vec<f32> = f.iter().take(4).filter_map(|v| v.parse().ok()).collect();
                    if n.len() < 4 {
                        continue;
                    }
                    let (at, yaw) = (Vec3::new(n[0], n[1], n[2]), n[3].to_radians());
                    match f.get(4) {
                        Some(pose) => horde.list.push(crate::evide::Evide::posed(at, yaw, pose)),
                        None => {
                            horde.spawn(at, yaw);
                        }
                    }
                }
                horde
            },
            // Essais : MINELIB_MORSURES=<n> mord d'avance n fois le joueur local.
            flesh: {
                let mut map = HashMap::new();
                let n = std::env::var("MINELIB_MORSURES")
                    .ok()
                    .and_then(|v| v.parse::<u32>().ok())
                    .unwrap_or(0);
                if n > 0 {
                    let grids = crate::player_model::flesh_grids();
                    let mut f = crate::flesh::Flesh::of(crate::flesh::Hard::Bones);
                    for k in 0..n {
                        f.bite_somewhere(k * 31 + 7, 2 + (k % 2) as usize, &[0, 1, 2, 3, 5, 6], &grids);
                    }
                    map.insert(String::new(), f);
                }
                map
            },
            // Essais : MINELIB_PLAIES=1 pose d'avance des plaies sur le joueur local.
            wounds: if std::env::var_os("MINELIB_PLAIES").is_some() {
                HashMap::from([(String::new(), crate::player_model::test_wounds())])
            } else {
                HashMap::new()
            },
            probe_split: HashMap::new(),
            fps_elements: None,
            frames: None,
            coast: 0.0,
            coast_logged: false,
            stick: crate::stick::Guard::restored(),
            fwd_was: false,
            run_was: false,
            fwd_tap: 0.0,
            tap_sprint: false,
            run_from: None,
            stop_meas: None,
            coast_air: 0.0,
            move_debug: String::new(),
            injected_warned: false,
            filter_keys_in: Some(3.0),
            lag_book: Default::default(),
            key_test: None,
            key_lag: crate::keycomp::load(),
            key_comp: Default::default(),
            fwd_key: false,
            run_key: false,
            fps_aiming: false,
            autosave: AUTOSAVE_S,
            hit: None,
            frame_aim: None,
            items: crate::drops::Items::new(true),
            items_ready: false,
            items_origin: data.objects_origin,
            far_allowed: far,
            teleport_to: None,
            rebase_origin: None,
            lap_base,
            laps_seen: laps,
            teleport_laps: None,
            tp_laps_online: None,
            loading: Some(Loading::new()),
            items_seeded: false,
            delete_world_asked: false,
            sky_clock: 0.0,
            overlay: Overlay::default(),
            chat: Chat::default(),
            game,
            spawn,
            spawn_set,
            fall_from: None,
            since_space: 1.0,
            hurt: 0.0,
            dazzle: 0.0,
            fps: crate::fps::Fps::new(),
            pings: Vec::new(),
            fps_walk_on: false,
            fps_duck_on: false,
            hand: Hand::default(),
            hand_light: [1.0; 3],
            flashlight_on: false,
            flashlight_focus: crate::flashlight::saved_focus(),
            // Essais : MINELIB_F5=1 (vu de derrière) ou 2 (de face).
            perspective: std::env::var("MINELIB_F5")
                .ok()
                .and_then(|v| v.trim().parse::<u8>().ok())
                .map_or(0, |v| v % 3),
            screens: Vec::new(),
            screens_loaded: false,
            shown: Vec::new(),
            party: crate::watch::Party::new(),
            displays: crate::screen::Displays::new(),
            screen_last: None,
            screen_repeat: None,
            screen_aim: None,
            screen_touch: None,
            screen_broke: false,
            walk_phase: 0.0,
            walks: HashMap::new(),
            walk_stamp: 0,
            solo_filtre: None,
            refus_dit: std::collections::HashMap::new(),
            skin_layers: Default::default(),
            scratch: Default::default(),
            sun_seen: Default::default(),
            crowd: None,
            crowd_on: false,
            crowd_out: None,
            crowd_probe: None,
            remote_crowds: HashMap::new(),
            friends_said: false,
            saved_mode: None,
            water_mirror_on: true,
            seen_water: Default::default(),
            pseudo: String::new(),
            bots_auto: true,
            walk_amp: 0.0,
            anim: (0.0, 0.0, 0.0),
            fly_crouch: 0.0,
            opened: HashMap::new(),
            vox_origin: None,
            vox_dirty: false,
            vox_timer: 0.0,
            vox_job: None,
            lights_timer: 0.0,
            mirror_planes: Vec::new(),
            lights: {
                let mut l =
                    crate::lights::Lights::load(&minelib_assets::data_dir(), &dir_for_lights);
                // (v309) Comptées depuis l'origine de la partie.
                l.set_origin(ob);
                if let Some(ground) = demo {
                    demo_lights(&mut l, ground);
                }
                l
            },
            light_editor: Default::default(),
            light_op_probe: None,
            light_sync: None,
            light_inbox: Vec::new(),
            light_denied_said: None,
            light_op_ok: false,
            seed_lock: None,
            seed_lock_asked: None,
            relay_seed: None,
            terrain_key,
            terrain_given,
            light_clear_asked: false,
            lights_wanted: false,
            shadow_lights: Vec::new(),
            remote_lamps: Vec::new(),
            lights_state: minelib_render::DynState::Off,
            lights_marked: false,
            lights_failed_said: false,
        })
    }

    /// Une image du test des touches (`/testtouche`) : messages au tchat et au journal ;
    /// Échap, la pause ou une minute sans rien le terminent, avec le bilan.
    fn run_key_test(&mut self, input: &Input, dt: f32, active: bool) {
        use crate::keytest::Tone;
        let Some(test) = self.key_test.as_mut() else {
            return;
        };
        let label = |k: KeyCode| {
            input
                .key_labels
                .get(&k)
                .cloned()
                .or_else(|| minelib_platform::layout_label(k))
                .unwrap_or_else(|| format!("{k:?}"))
        };
        let mouse = [Mouse::Left, Mouse::Right, Mouse::Middle]
            .iter()
            .any(|&b| input.mouse_held(b));
        let mut said = test.step(&input.held_keys(), mouse, std::time::Instant::now(), dt, &label);
        if !active || input.pressed(KeyCode::Escape) || test.finished {
            said.extend(test.summary(&label));
            // La touche « avancer » testée : son retard mesuré est compensé (ou ne l'est
            // plus, si elle remonte maintenant normalement).
            if let Some(crate::keys::Bind::Key(k)) = self.keys.get(Action::Forward) {
                let long = test.trials().iter().filter(|t| t.key == k && t.held >= 1.5).count();
                match crate::keycomp::estimate(test.trials(), k) {
                    Some(lag) => {
                        let s = crate::keycomp::Setting { key: k, lag, on: true };
                        crate::keycomp::save(&s);
                        self.key_lag = Some(s);
                        said.push((
                            format!(
                                "Le jeu compense maintenant ce retard ({:.0} ms) : quand la \
                                 touche est enfin relâchée, tu reviens là où tu as lâché. \
                                 (/compense off pour l'enlever.)",
                                lag * 1000.0
                            ),
                            Tone::Good,
                        ));
                    }
                    None if long >= 3 && self.key_lag.is_some_and(|s| s.key == k && s.on) => {
                        let s = crate::keycomp::Setting {
                            on: false,
                            ..self.key_lag.unwrap()
                        };
                        crate::keycomp::save(&s);
                        self.key_lag = Some(s);
                        said.push((
                            "La touche remonte normalement : le jeu ne compense plus rien."
                                .to_string(),
                            Tone::Good,
                        ));
                    }
                    None => {}
                }
            }
            self.key_test = None;
        }
        for (text, tone) in said {
            log::info!("test des touches : {text}");
            let color = match tone {
                Tone::Info => chat::YELLOW,
                Tone::Good => [0.45, 1.0, 0.5, 1.0],
                Tone::Bad => chat::RED,
            };
            self.chat.say(text, color);
        }
    }

    /// Fin de partie : les bilans en cours vont au journal.
    pub fn flush_reports(&mut self) {
        if let Some(line) = self.lag_book.report(true) {
            log::info!("{line}");
        }
    }

    pub fn save(&mut self) -> Result<()> {
        self.meta.last_played = save::now();
        let mut inv = Inventory::empty();
        inv.slots = self.inventory.slots;
        inv.selected = self.inventory.selected;
        let mut game = self.game;
        if let Some(m) = self.saved_mode {
            game.mode = m;
        }
        // (v309) Places absolues (la partie compte depuis `World::origin`) ; les objets au sol
        // gardent la leur depuis l'origine (`objects_origin`).
        let o = self.world.origin_blocks();
        // Objets au sol et coffres : en solo, ou chez le propriétaire d'un monde en ligne.
        // (v309) Réécrits depuis l'origine de la partie si le fichier en avait une autre ; le
        // monde garde celle depuis laquelle le fichier compte vraiment.
        if self.items_ready && self.build.as_ref().is_none_or(|b| b.is_owner()) {
            let here = [o[0], o[2]];
            if self.items_origin != here {
                self.items.dirty = true;
            }
            if self.items.save(&self.dir) {
                self.items_origin = here;
            }
        }
        let data = SaveData {
            game,
            generator: self.world.generator,
            meta: self.meta.clone(),
            player: Some(PlayerState {
                pos: self.exact_place(),
                yaw: self.player.yaw,
                pitch: self.player.pitch,
                flying: self.player.flying,
            }),
            clock: self.world.clock,
            time: self.time_h,
            time_fixed: self.time_fixed,
            weather: self.storm.weather.save(),
            zeus: self.legend_saved(),
            inventory: Some(inv),
            spawn: self.spawn_set.then(|| self.abs_of(self.spawn)),
            objects_origin: self.items_origin,
            laps: self.teleport_laps.unwrap_or_else(|| self.laps()),
        };
        // (v272) Les Évidés : avec le monde (en ligne : avec la copie de ce joueur, ce sont
        // les siens).
        if self.net.is_none() {
            self.evides.save(&self.dir, self.world.origin_blocks());
        }
        // Lumières de l'éditeur : avec la copie du monde de ce joueur (même en ligne).
        self.lights.save_now();
        save::save_world(&self.dir, &data, &self.world)
    }

    /// (v328) Ce joueur peut-il voir la graine du monde (`/seed`, écran F3) et télécharger une
    /// copie de la carte (menu Échap) ? En solo, oui ; sur un serveur de construction,
    /// seulement si ses OP ne l'ont pas verrouillée (par défaut, elle l'est) ou s'il est OP
    /// (le relais le dit : `Event::SeedLock` ; rien de permis avant).
    pub fn seed_allowed(&self) -> bool {
        self.build.is_none() || self.seed_lock.is_some_and(|(_, allowed)| allowed)
    }

    /// (v330) La graine du monde, si on la connaît : celle de sa sauvegarde (son monde à soi),
    /// sinon celle que le relais nous a dite (OP, ou graine déverrouillée). À ne montrer que
    /// si `seed_allowed`.
    pub fn known_seed(&self) -> Option<u64> {
        self.meta
            .shown_seed(self.world.generator)
            .or(self.relay_seed)
    }

    /// Copie du monde tel qu'il est maintenant (serveur de construction compris) dans ses
    /// mondes solo, sous un nouveau nom. Renvoie son dossier. (v328) Refusée quand la graine
    /// est verrouillée pour ce joueur (`seed_allowed`).
    pub fn download_copy(&mut self, saves: &std::path::Path) -> Result<std::path::PathBuf> {
        if !self.seed_allowed() {
            anyhow::bail!("carte verrouillée par les OP du serveur (/seed lock)");
        }
        let name = format!("{} (copie)", self.meta.name);
        // (v330) La copie a tout ce qu'il faut pour son terrain : la graine (d'un monde d'avant
        // la v330 : celle de la partie, que le relais a donnée), la graine de 64 bits si on la
        // connaît, la clé de terrain.
        let seeds = save::WorldSeeds {
            seed: self.world.seed,
            graine: self
                .meta
                .graine
                .or(self.relay_seed.filter(|_| self.terrain_key.is_some())),
            key: self.terrain_key,
        };
        let dir = save::create_world_as(saves, &name, seeds, self.world.generator)?;
        let mut meta = self.meta.clone();
        meta.name = name;
        meta.seed = seeds.seed;
        meta.graine = seeds.graine;
        meta.key = seeds.key;
        meta.created = save::now();
        meta.last_played = save::now();
        let mut game = self.game;
        if game.mode == GameMode::Fps {
            game.mode = GameMode::Creative;
        }
        let data = SaveData {
            game,
            generator: self.world.generator,
            meta,
            // (v309) Places absolues, comme dans `save`.
            player: Some(PlayerState {
                pos: self.abs_of(self.player.pos),
                yaw: self.player.yaw,
                pitch: self.player.pitch,
                flying: self.player.flying,
            }),
            clock: self.world.clock,
            time: self.time_h,
            time_fixed: self.time_fixed,
            weather: self.storm.weather.save(),
            zeus: self.legend_saved(),
            inventory: None,
            spawn: self.spawn_set.then(|| self.abs_of(self.spawn)),
            // (La copie n'emporte pas les objets au sol.)
            objects_origin: [0, 0],
            laps: self.laps(),
        };
        save::save_world(&dir, &data, &self.world)?;
        Ok(dir)
    }

    /// Nouveaux réglages graphiques (écran Paramètres), appliqués tout de suite.
    pub fn apply_config(&mut self, cfg: SessionConfig) {
        self.view_distance = cfg.view_distance.clamp(2, MAX_VIEW_DISTANCE);
        self.settings_changed();
    }

    pub fn config(&self) -> SessionConfig {
        SessionConfig {
            view_distance: self.view_distance,
            // (Le choix d'une partie, pas un réglage : `Game::play_with` le donne à chaque
            // ouverture.)
            far_origin: true,
            origin: None,
            terrain: None,
        }
    }

    /// (v309) Place absolue (dans le monde, en double précision) du point `v` de la partie
    /// (compté depuis `World::origin`).
    pub fn abs_of(&self, v: Vec3) -> [f64; 3] {
        let o = self.world.origin_blocks();
        // (v309) Sur la carte bouclée (ses tours : `laps_at`).
        let wrap = minelib_world::lap::wrap_f64;
        [
            wrap(v.x as f64 + o[0] as f64),
            v.y as f64,
            wrap(v.z as f64 + o[2] as f64),
        ]
    }

    /// (v322) L'origine depuis laquelle la partie doit être rouverte (une partie FPS la demande),
    /// prise par `Game::rebase_world` (`SessionConfig::origin`).
    pub fn take_rebase_origin(&mut self) -> Option<ChunkPos> {
        self.rebase_origin.take()
    }

    /// (v322) Rouverte depuis l'origine de la partie FPS du serveur de construction (en mode
    /// FPS, `join_build_match`) : on y entre maintenant. (La partie contre les bots repart
    /// d'elle-même, `bots_auto`.)
    pub fn after_rebase(&mut self) {
        if self.game.mode != GameMode::Fps || self.build.is_none() || self.in_build_match() {
            return;
        }
        if let Some(host) = self
            .build_match
            .clone()
            .filter(|h| !h.eq_ignore_ascii_case(&self.pseudo))
        {
            self.join_build_match(&host);
        }
    }

    /// (v322) Réglages FPS du monde comptés depuis l'origine de la partie (`fpsmap::local`) :
    /// ce que prennent la partie, ses bots, son radar et `/fps showelements`. (`fps.txt` les
    /// garde en places absolues.)
    pub fn fps_map(&self) -> minelib_net::game::FpsMap {
        fps_map_of(&self.dir, &self.world)
    }

    /// (v322) Enregistre les réglages FPS `m` (comptés depuis l'origine de la partie) en places
    /// absolues.
    fn save_fps_map(&self, m: &minelib_net::game::FpsMap) -> anyhow::Result<()> {
        crate::fpsmap::save(
            &self.dir,
            &crate::fpsmap::absolute(m, self.world.origin_blocks()),
        )
    }

    /// (v322) La partie contre les bots se joue autour des départs de la carte (sans zones :
    /// du point de départ du monde, où elle place les joueurs). Loin de l'origine de la partie
    /// (au-delà de `REBASE_AT` : un monde ouvert ailleurs, une carte construite au loin), elle
    /// doit d'abord repartir d'une origine proche d'eux : laquelle (celle de la carte,
    /// `fpsmap::origin`), et la place absolue où mettre le joueur (au-dessus du départ CT, la
    /// partie l'y placera de toute façon).
    fn solo_fps_far(&self) -> Option<(ChunkPos, [f64; 3])> {
        let abs = crate::fpsmap::load(&self.dir);
        let start = [&abs.ct, &abs.t]
            .into_iter()
            .chain(abs.sites.iter())
            .flatten()
            .next();
        let at = match start {
            Some(z) => [
                (z.min[0] as f64 + z.max[0] as f64 + 1.0) / 2.0,
                z.max[1] as f64 + 1.0,
                (z.min[2] as f64 + z.max[2] as f64 + 1.0) / 2.0,
            ],
            None => self.abs_of(self.spawn),
        };
        let local = self.local_of(at);
        if local.x.abs() <= REBASE_AT && local.z.abs() <= REBASE_AT {
            return None;
        }
        let origin = crate::fpsmap::origin(&abs).unwrap_or_else(|| origin_near(at));
        Some((origin, at))
    }

    /// (v318) Place absolue exacte du joueur : celle d'un `/tp` lointain pas encore fait
    /// (`teleport_to`, avant que la partie ne change d'origine), sinon la sienne.
    pub fn exact_place(&self) -> [f64; 3] {
        self.teleport_to.unwrap_or_else(|| self.abs_of(self.player.pos))
    }

    /// (v309) Tours de la carte bouclée du point `v` de la partie : ceux de l'origine, plus
    /// la limite qu'il a passée depuis elle.
    fn laps_at(&self, v: Vec3) -> crate::laps::Laps {
        let o = self.world.origin_blocks();
        let cross = |o: i32, v: f32| minelib_world::lap::split(o as i128 + v.floor() as i128).0;
        [
            self.lap_base[0].saturating_add(cross(o[0], v.x)),
            self.lap_base[1].saturating_add(cross(o[2], v.z)),
        ]
    }

    /// (v309) Tours de la carte bouclée faits par le joueur.
    pub fn laps(&self) -> crate::laps::Laps {
        self.laps_at(self.player.pos)
    }

    /// (v309) Le joueur en est à `target` tours (après un `/tp`) : l'origine en compte
    /// d'autant.
    fn set_laps(&mut self, target: crate::laps::Laps) {
        let here = self.laps();
        for k in 0..2 {
            self.lap_base[k] = self.lap_base[k].saturating_add(target[k].saturating_sub(here[k]));
        }
        self.laps_seen = target;
    }

    /// (v309) « Tu indiques combien de fois on a fait le tour de la map quand on en fait au
    /// moins un, dans quelle direction et quel degré » : passé la limite à pied (ou en
    /// volant), le tchat le dit.
    fn watch_laps(&mut self) {
        if self.teleport_to.is_some() || !self.far_allowed {
            return;
        }
        let now = self.laps();
        if now != self.laps_seen {
            self.laps_seen = now;
            self.chat.say(laps_said(now), chat::YELLOW);
        }
    }

    /// (v318) Ce que la partie garde quand elle repart d'une origine plus proche
    /// (`Game::rebase_world`) : ce qui ne s'enregistre pas avec le monde. En ligne, la
    /// connexion au serveur et ce qu'on en sait (relais, joueurs proches, voix, skins, objets
    /// au sol, foudre demandée…), en places absolues ou décalé jusqu'à la nouvelle origine
    /// (`put_carry`) : on ne quitte pas le serveur.
    pub fn take_carry(&mut self) -> Carry {
        let from = self.world.origin_blocks();
        let vel = self.player.vel;
        let online = self.build.take().map(|mut build| {
            // Ce que la partie n'a pas encore traité ou envoyé repart à `build`, en places
            // absolues : lumières reçues, demandes d'objets.
            build.untake_items(std::mem::take(&mut self.light_inbox));
            for (chunk, req) in std::mem::take(&mut self.items.outbox) {
                build.send_item(chunk, &req);
            }
            // Notre foule d'essai s'arrête (ses PNJ comptaient depuis l'ancienne origine).
            if self.crowd_out.take().is_some() {
                let stop = crate::crowd::stop_packet(self.player.pos);
                build.send_crowd(minelib_net::crowd::pack(&stop));
            }
            // Les modifications du joueur pas encore parties (monde en pause), en places
            // absolues.
            let journal = self
                .world
                .journal
                .take()
                .unwrap_or_default()
                .into_iter()
                .map(|(p, b, old)| (self.world.abs_block(p), b, old))
                .collect();
            Box::new(Online {
                build,
                voice: self.voice.take(),
                skins: std::mem::take(&mut self.skins),
                items: std::mem::replace(&mut self.items, crate::drops::Items::new(true)),
                items_ready: self.items_ready,
                items_seeded: self.items_seeded,
                journal,
                thor_seq: self.thor_seq,
                thor_refused: self.thor_refused,
                zeus_online: std::mem::take(&mut self.zeus_online),
                zeus_pending: std::mem::take(&mut self.zeus_pending),
                relay_cmds: std::mem::take(&mut self.relay_cmds),
                room_started: self.room_started,
                room_paused: self.room_paused,
                build_match: self.build_match.take(),
                build_match_origin: self.build_match_origin,
                weather_synced: self.weather_synced,
                game: self.game,
                saved_mode: self.saved_mode,
                friends_said: self.friends_said,
                light_op: (self.light_op_ok, self.light_op_probe),
                seed_lock: self.seed_lock,
                relay_seed: self.relay_seed,
            })
        });
        Carry {
            chat: std::mem::take(&mut self.chat),
            vel,
            from,
            online,
        }
    }

    /// (v318) La partie rouverte depuis une origine plus proche reprend ce qu'elle gardait
    /// (`take_carry`) : ses places, décalées de l'ancienne origine à la nouvelle.
    pub fn put_carry(&mut self, c: Carry) {
        self.chat = c.chat;
        self.player.vel = c.vel;
        let Some(o) = c.online else {
            return;
        };
        let Online {
            mut build,
            voice,
            skins,
            mut items,
            items_ready,
            items_seeded,
            journal,
            thor_seq,
            thor_refused,
            zeus_online,
            mut zeus_pending,
            relay_cmds,
            room_started,
            room_paused,
            build_match,
            build_match_origin,
            weather_synced,
            game,
            saved_mode,
            friends_said,
            light_op,
            seed_lock,
            relay_seed,
        } = *o;
        self.seed_lock = seed_lock;
        self.relay_seed = relay_seed;
        let to = self.world.origin_blocks();
        // L'écart le plus court entre les deux origines, sur la carte bouclée.
        let w = minelib_world::lap::wrap_block;
        let (dx, dz) = (
            w(c.from[0] as i64 - to[0] as i64) as i64,
            w(c.from[2] as i64 - to[2] as i64) as i64,
        );
        build.set_origin(to);
        items.rebase(dx, dz);
        let v = Vec3::new(dx as f32, 0.0, dz as f32);
        for p in &mut zeus_pending {
            p.pos += v;
            p.to += v;
        }
        self.world.journal = Some(
            journal
                .into_iter()
                .map(|(p, b, old)| (self.world.local_block(p), b, old))
                .collect(),
        );
        self.build = Some(build);
        self.voice = voice;
        // (Le sien, donné par le jeu à l'ouverture, reste.)
        let mine = self.skins.mine();
        self.skins = skins;
        self.skins.set_mine(mine);
        self.items = items;
        self.items_ready = items_ready;
        self.items_seeded = items_seeded;
        self.thor_seq = thor_seq;
        self.thor_refused = thor_refused;
        self.zeus_online = zeus_online;
        self.zeus_pending = zeus_pending;
        self.relay_cmds = relay_cmds;
        self.room_started = room_started;
        self.room_paused = room_paused;
        self.build_match = build_match;
        self.build_match_origin = build_match_origin;
        self.weather_synced = weather_synced;
        self.game = game;
        self.saved_mode = saved_mode;
        self.friends_said = friends_said;
        (self.light_op_ok, self.light_op_probe) = light_op;
    }

    /// (v309) La partie doit-elle repartir d'une origine plus proche ? En solo, quand le joueur
    /// s'est éloigné de plus de `REBASE_AT` blocs de l'origine (sur x ou sur z) et qu'une autre
    /// lui conviendrait (`origin_near`). Le jeu enregistre alors la partie et la rouvre
    /// (`Game::rebase`) : tout ce qui est compté depuis l'origine (chunks, maillages, objets,
    /// lumières…) repart de la nouvelle, sans rien perdre.
    pub fn wants_rebase(&self) -> bool {
        // (Même pendant un chargement : un `/tp` au loin repart tout de suite de la bonne
        // origine, au lieu de charger d'abord autour d'une place arrondie.)
        // (v322) Une partie FPS demande une autre origine (`rebase_origin`) : tout de suite.
        if self.rebase_origin.is_some() {
            return true;
        }
        // (v318) En construction en ligne aussi (sans quitter le serveur : `take_carry`) ; pas
        // dans une partie FPS (leurs places sont celles de la partie, partagée). (v322) En mode
        // FPS hors d'une partie, oui (avant : jamais en mode FPS, qui ne se jouait qu'au centre
        // du monde ; un joueur en mode FPS téléporté au loin restait dans les terres lointaines).
        if !self.far_allowed || self.net.is_some() {
            return false;
        }
        let p = self.player.pos;
        if !(p.x.abs() > REBASE_AT || p.z.abs() > REBASE_AT) {
            return false;
        }
        origin_near(self.abs_of(p)) != self.world.origin()
    }

    /// (v309) Point de la partie (compté depuis `World::origin`) de la place absolue `a`.
    pub fn local_of(&self, a: [f64; 3]) -> Vec3 {
        let o = self.world.origin_blocks();
        // (v309) La place la plus proche de l'origine, sur la carte bouclée.
        let wrap = minelib_world::lap::wrap_f64;
        Vec3::new(
            wrap(a[0] - o[0] as f64) as f32,
            a[1] as f32,
            wrap(a[2] - o[2] as f64) as f32,
        )
    }

    fn settings_changed(&mut self) {
        // Garde-fou : jamais plus de chunks que la mémoire vive ne peut en contenir.
        let limit = ram_view_limit();
        let radius = self.view_distance.min(limit);
        if radius < self.view_distance {
            log::warn!("distance de rendu limitée à {limit} chunks par la mémoire vive disponible");
        }
        log::info!(
            "distance de rendu : {radius} chunks ({} chunks environ)",
            (2 * radius + 1).pow(2)
        );
        self.streamer.set_radius(radius);
    }

    fn fog_end(&self) -> f32 {
        (self.streamer.radius * CHUNK_SIZE as i32) as f32
    }

    /// Une image de jeu. `active` : faux quand le jeu est en pause (temps figé, pas d'entrées).
    pub fn update(&mut self, input: &Input, dt: f32, renderer: &mut Renderer, active: bool) {
        let (sw, sh) = renderer.size();
        self.screen_size = (sw as f32, sh as f32);
        use minelib_render::perf;
        let logic = perf::scope("jeu : logique et physique");
        if self.keys.mouse_muted
            && ![Mouse::Left, Mouse::Right, Mouse::Middle]
                .iter()
                .any(|&b| input.mouse_held(b))
        {
            self.keys.mouse_muted = false;
        }
        // (v251) Le C de Ctrl+C (capture copiée) compte de nouveau une fois relâché.
        if self.keys.muted_key.is_some_and(|k| !input.held(k)) {
            self.keys.muted_key = None;
        }
        // (v283) Derrière l'écran de chargement : tous les threads de fond, et les maillages
        // envoyés au GPU par plus grosses fournées (« qu'on puisse rejoindre plus vite »).
        if self.loading.is_some() {
            crate::jobs::full_speed();
        } else {
            crate::jobs::throttle(perf::last_unmeasured_ms(), !self.streamer.idle(), dt);
        }
        self.streamer.set_loading(self.loading.is_some());
        // (v220) Monde pas encore chargé : on n'y apparaît pas (voir `loading`).
        self.check_loaded(renderer);
        // (v309) Passé la limite de la carte bouclée : dit au tchat.
        self.watch_laps();
        // Test des touches : il lit le clavier et la souris, le jeu ne reçoit rien.
        let quiet;
        let input = if self.key_test.is_some() {
            self.run_key_test(input, dt, active);
            quiet = input.silenced();
            &quiet
        } else if self.loading.is_some() {
            // (v220) Pendant le chargement : ni touches, ni souris.
            let mut q = input.silenced();
            q.mouse_delta = (0.0, 0.0);
            quiet = q;
            &quiet
        } else {
            input
        };
        // (v172) Mode FPS dans un monde solo : partie contre des bots.
        if self.bots_auto
            && self.loading.is_none()
            && self.net.is_none()
            && self.build.is_none()
            && self.game.mode == GameMode::Fps
        {
            self.start_bots();
        }
        // Minimap : grille autour du joueur, sites de la carte.
        if self.net.is_some() {
            if !self.radar_map {
                self.radar.set_map(&fps_map_of(&self.dir, &self.world));
                self.radar.set_dir(&self.dir, self.world.origin_blocks());
                self.radar_map = true;
            }
            self.radar.update(&self.world, self.player.pos);
        }
        // Couleur préférée : envoyée une fois qu'on est dans la partie.
        if let Some(n) = &mut self.net {
            let me = n.me();
            if self.color_sent != Some(self.fps_color) && n.rows().iter().any(|r| r.id == me) {
                n.choose_color(self.fps_color);
                self.color_sent = Some(self.fps_color);
            }
        }
        // Menu, pause ou souris libre : le clic qui fait reprendre ne tire pas.
        if !active || !input.grabbed {
            self.fps.block_fire();
        }
        if active {
            // Tchat : saisie, puis ouverture par `t` ou `/` (jamais par-dessus l'inventaire).
            // Clavier dans la page d'un écran (champ de texte visé) : ni tchat ni commandes.
            let screen_keys = self.keyboard_on_screen();
            let was_open = self.chat.open;
            // (v273) Tab au tchat : les pseudos des joueurs présents, pris quand il sert.
            if self.chat.open && input.pressed(KeyCode::Tab) {
                self.chat.names = self.present_pseudos();
            }
            let mut sent = self.chat.update(input, dt);
            if !was_open && !self.inventory.open && !screen_keys && !self.light_editor.open {
                let by_chat = if self.game.mode == GameMode::Fps {
                    self.keys.pressed(input, Action::FpsChat)
                        || self.keys.pressed(input, Action::FpsTeamChat)
                } else {
                    self.keys.pressed(input, Action::Chat)
                };
                let by_command = self.keys.pressed(input, Action::Command);
                sent = sent.or_else(|| self.chat.try_open(input, by_chat, by_command));
            }
            // Image où le tchat était ouvert, s'est ouvert ou a été envoyé : les touches
            // tapées sont du texte, pas des commandes de jeu.
            let chat_typing = was_open || self.chat.open || sent.is_some();
            let typing = chat_typing || screen_keys;
            if let Some(text) = sent {
                self.submit(&text);
            }
            // (v251) Aide de la commande qu'on tape (refaite quand la saisie change).
            let typed = if self.chat.open {
                self.chat.typed().to_string()
            } else {
                String::new()
            };
            if typed != self.hint_for {
                // (v262) Toutes ses lignes (« … et 17 lignes de plus » avant) : elles défilent.
                // (v340) `/fps certif` dans l'aide de /fps : pour le compte qui certifie seulement.
                let certifier = self.certifier();
                let hint = crate::cmdhelp::lines(&typed, usize::MAX, certifier, |name| {
                    self.command_state(name)
                });
                self.chat.set_hint(hint);
                self.hint_for = typed;
            }
            // Éditeur de lumières (touche L) et sa caméra libre (F) ; pas en partie FPS en
            // ligne (rien ne doit éclairer un coin sombre pour un seul joueur). En FPS solo, si.
            let fps_online = !self.lights_allowed();
            if fps_online && self.light_editor.open {
                self.light_editor.close();
            }
            let naming = self.light_editor.editing_name;
            if !typing && !fps_online && !naming && self.keys.pressed(input, Action::LightEditor) {
                if self.light_editor.open {
                    self.light_editor.close();
                } else {
                    self.open_light_editor();
                }
            }
            if self.light_editor.open
                && !typing
                && !naming
                && self.keys.pressed(input, Action::FreeCamera)
            {
                self.light_editor.toggle_free_cam(
                    self.player.eye(),
                    self.player.yaw,
                    self.player.pitch,
                );
            }
            let editing = self.light_editor.open;
            self.hurt = (self.hurt - dt).max(0.0);
            self.dazzle = (self.dazzle - dt / DAZZLE_S).max(0.0);
            // (v269) Les morceaux mangés par L'Évidé reviennent en cinq minutes.
            if !self.flesh.is_empty() {
                let grids = crate::player_model::flesh_grids();
                for f in self.flesh.values_mut() {
                    f.tick(dt, Some(crate::flesh::REGROW_S), &grids);
                }
                self.flesh.retain(|_, f| !f.is_whole());
            }
            // (v272) « Fais que du sang en coule » : chaque morsure saigne `BLEED_S` secondes,
            // des gouttes de plus en plus rares qui tombent de l'endroit mordu (là où il est
            // dans la pose dessinée) et tachent le sol. « En build le sang reste 5 minutes au
            // sol » : hors partie FPS, les taches de sang s'en vont après ce temps-là.
            if !self.bleeding.is_empty() {
                let vel = self.player.vel;
                let mut drops = Vec::new();
                {
                    let seen = self.bite_points.borrow();
                    for b in &mut self.bleeding {
                        b.2 += dt;
                        b.3 += dt * BLEED_RATE * (-b.2 / BLEED_FADE_S).exp();
                        let n = b.3 as usize;
                        b.3 -= n as f32;
                        let at = seen.1.iter().find(|p| (p.0, p.1) == (b.0, b.1));
                        if let (true, Some(at)) = (n > 0, at) {
                            drops.push((at.2, n));
                        }
                    }
                }
                for (at, n) in drops {
                    self.fps.bleed(at, vel, n, false);
                }
                self.bleeding.retain(|b| b.2 < BLEED_S);
            }
            if self.game.mode != GameMode::Fps {
                self.fps.tick_blood(dt, &self.world, BLOOD_ON_GROUND_S);
                // (v299) Les morts : ils tombent, fondent en sang (des gouttes qui tachent le
                // sol dessous), leurs os restent.
                let dead: Vec<_> = self
                    .crowd
                    .as_mut()
                    .map(|c| std::mem::take(&mut c.fallen))
                    .unwrap_or_default();
                for (pos, yaw, skin, from) in dead {
                    let pose = crate::player_model::Pose {
                        feet: pos,
                        yaw,
                        pitch: 0.0,
                        walk: 0.0,
                        swing: 0.0,
                        crouch: 0.0,
                        light: [1.0; 3],
                        held: None,
                        skin: None,
                        bow: 0.0,
                        air: 0.0,
                        dir: 0.0,
                        run: 0.0,
                    };
                    let away = pos - from;
                    let push = Vec3::new(away.x, 0.0, away.z).normalize_or_zero() * 2.2;
                    self.fall_body(&pose, Vec3::ZERO, push, None, skin);
                }
                // Essais : MINELIB_MORT=x,y,z,lacet° : un joueur meurt là, debout, au début.
                static TRIED: std::sync::Once = std::sync::Once::new();
                TRIED.call_once(|| {
                    let n: Vec<f32> = std::env::var("MINELIB_MORT")
                        .unwrap_or_default()
                        .split(',')
                        .filter_map(|v| v.trim().parse().ok())
                        .collect();
                    if n.len() == 4 {
                        let pose = crate::player_model::Pose {
                            feet: Vec3::new(n[0], n[1], n[2]),
                            yaw: n[3].to_radians(),
                            ..self.body_pose()
                        };
                        let skin = self.worn_skin();
                        self.fall_body(&pose, Vec3::ZERO, Vec3::new(0.9, 0.0, 0.5), None, skin);
                    }
                });
                let mut drops = Vec::new();
                for f in &mut self.fallen {
                    drops.extend(f.update(&self.world, dt));
                    let light = sky_light(&self.world, f.middle(), self.daylight_shown);
                    f.refresh(light);
                }
                for at in drops {
                    self.fps.bleed(at, Vec3::ZERO, 1, false);
                }
                self.fallen.retain(|f| !f.done());
            } else if !self.fallen.is_empty() {
                self.fallen.clear();
            }
            // (v272) Notre bouchée : on se plie vers le morceau visé, là où il est maintenant ;
            // au bout de `EAT_IN` secondes on le mange si notre bouche le touche, sinon rien
            // (« si la bouche se décolle avant, ça ne mange pas, dans les deux sens ») ; puis
            // on se redresse.
            let mut bow = 0.0;
            let mut turn = (0.0f32, 0.0f32);
            if let Some((id, part, cell, t, done)) = self.eating {
                // (Le morceau, et le milieu du membre où il est.)
                let target = self
                    .evides
                    .by_id(id)
                    .filter(|e| !e.is_dying())
                    .map(|e| (e.cell_point(part, cell), e.part_point(part)));
                self.eating = None;
                if let Some((target, limb)) = target {
                    let mut pose = self.body_pose();
                    crate::player_model::bow_to(&mut pose, target);
                    bow = pose.bow;
                    // (v290) « Le joueur se penche à la bonne hauteur et tourne la tête vers
                    // le centre du membre attaqué, et revient à sa position initiale à la fin
                    // de la morsure » : de combien tourner (lacet, tangage) pour regarder le
                    // milieu de ce membre depuis notre bouche ; la caméra à la première
                    // personne et notre modèle (F5, ombre) tournent d'autant, puis reviennent.
                    let to = limb - crate::player_model::mouth_point(&pose);
                    let flat = Vec3::new(to.x, 0.0, to.z).length();
                    if flat > 0.1 {
                        let yaw = to.x.atan2(-to.z) - self.player.yaw;
                        turn.0 = yaw.sin().atan2(yaw.cos()).clamp(-EAT_TURN_MAX, EAT_TURN_MAX);
                    }
                    let pitch = to.y.atan2(flat.max(0.1)).clamp(-1.3, 1.3) - self.player.pitch;
                    turn.1 = pitch.clamp(-EAT_TURN_MAX, EAT_TURN_MAX);
                    let (t, mut done) = (t + dt, done);
                    if t >= EAT_IN && !done {
                        done = true;
                        let far = crate::evide::EAT_REACH + EAT_SLACK;
                        if self.player.eye().distance(target) <= far {
                            let eye = self.player.eye();
                            if let Some(b) = &self.build {
                                // (v312) En ligne : demandée au relais, qui vérifie qu'on est
                                // à portée et que le morceau est encore là ; elle arrive
                                // quand il la publie (`evides_online`).
                                let me = b.pseudo().to_string();
                                let whose = self.evides.by_id(id).map(|e| {
                                    e.lead.clone().unwrap_or((me, e.id as u16))
                                });
                                if let Some((owner, id)) = whose {
                                    b.send_evide(minelib_net::game::BuildMsg::EvideEat {
                                        owner,
                                        id,
                                        part: part as u8,
                                        cell: cell.map(|c| c as u8),
                                        by: String::new(),
                                        health: 0,
                                    });
                                }
                            } else {
                                let eaten = self
                                    .evides
                                    .by_id(id)
                                    .and_then(|e| e.eaten_at(part, cell, eye));
                                if let Some(dead) = eaten {
                                    crate::audio::jelly_bite(0.0);
                                    if dead {
                                        crate::audio::evide_voice(0.0);
                                    }
                                }
                            }
                        }
                    }
                    if t < EAT_S {
                        self.eating = Some((id, part, cell, t, done));
                    }
                }
            }
            self.eat_bow += (bow - self.eat_bow).clamp(-BOW_SPEED * dt, BOW_SPEED * dt);
            let step = EAT_TURN_SPEED * dt;
            self.eat_turn.0 += (turn.0 - self.eat_turn.0).clamp(-step, step);
            self.eat_turn.1 += (turn.1 - self.eat_turn.1).clamp(-step, step);
            // (v269) Les Évidés. (v272) En solo et dans un monde de construction en ligne (là,
            // ceux de ce joueur seulement : rien n'est partagé, le relais n'a rien à valider) ;
            // pas en partie FPS. La nuit, il en vient tout seuls, rarement
            // (`evide::SPAWN_EVERY_S`).
            if self.net.is_none() {
                let mut preys = Vec::new();
                if self.game.mode != GameMode::Spectator && self.spectating.is_none() {
                    preys.push(crate::evide::Prey {
                        feet: self.player.pos,
                        eye: self.player.eye(),
                        spot: self.evide_spot.and(self.bite_points.borrow().0),
                        limb: self.evide_spot.and(self.bite_points.borrow().2),
                        tag: 0,
                    });
                }
                // (v312) En ligne, L'Évidé est partagé : ceux qu'on mène s'en prennent au
                // joueur le plus proche d'eux, nous ou un autre (`names` : leurs pseudos,
                // dans l'ordre des `Prey::tag`).
                let mut names: Vec<String> = Vec::new();
                if let Some(b) = &self.build {
                    for o in b.others.values() {
                        names.push(o.pseudo.clone());
                        preys.push(crate::evide::Prey {
                            feet: o.pos,
                            eye: eye_of(o.pos, o.crouch),
                            spot: None,
                            limb: None,
                            tag: names.len() as u16,
                        });
                    }
                }
                let night = self.game.mode != GameMode::Fps
                    && self.time_h >= 0.0
                    && crate::daytime::daylight(self.time_h) < 0.15;
                for ev in self.evides.update(&self.world, dt, &preys, night) {
                    self.evide_event(ev, &names);
                }
                self.evides_online(dt);
            }
            for p in &mut self.pings {
                p.1 += dt;
            }
            self.pings.retain(|p| p.1 < PING_S);
            self.since_space += dt;
            self.inventory.creative = self.game.mode == GameMode::Creative;
            self.sync_legend();
            if self.inventory.open && self.inventory.chest.is_some() && !typing {
                // Coffre ouvert : E (ou Échap) le referme.
                if self.keys.pressed(input, Action::Inventory) {
                    self.inventory.close();
                }
            } else if self.inventory.open && self.inventory.creative && !typing {
                // Catalogue créatif ouvert : le clavier sert à la recherche ; E ferme
                // seulement quand la recherche est vide (sinon c'est une lettre).
                if self.keys.pressed(input, Action::Inventory) && self.inventory.search.is_empty() {
                    self.inventory.close();
                } else {
                    self.inventory.search_input(
                        &input.text,
                        input.pressed(KeyCode::Backspace),
                        input.scroll,
                    );
                }
            } else if !typing && !editing && self.game.mode != GameMode::Fps {
                self.handle_keys(input);
                self.handle_hotbar(input);
            }
            // Éditeur de lumières ouvert : les écrans ne se touchent pas.
            self.update_screens(input, dt, chat_typing || editing);
            self.inventory.update(dt);
            self.tick_torches(dt);
            self.update_sun(dt);

            let k = &self.keys;
            let fps_mode = self.game.mode == GameMode::Fps;
            let zooming =
                !fps_mode && !typing && k.held(input, Action::Zoom) && !self.inventory.open;
            let sprinting = !fps_mode
                && !typing
                && k.held(input, Action::Sprint)
                && self.player.vel.length() > 5.0;
            let target_fov = if zooming {
                ZOOM_FOV
            } else if sprinting {
                FOV * 1.1
            } else {
                FOV
            };
            self.fov += (target_fov - self.fov) * (dt * 14.0).min(1.0);

            let mut free_turn = (0.0, 0.0);
            if input.grabbed {
                let (dx, dy) = input.mouse_delta;
                // Lunette : sensibilité réduite comme le champ de vision (CS :
                // zoom_sensitivity_ratio 1).
                let fov = if fps_mode {
                    self.fps.scope_fov().unwrap_or(self.fov)
                } else {
                    self.fov
                };
                let sens = if fps_mode {
                    self.sensitivity_fps
                } else {
                    self.sensitivity
                };
                let k = (CS_DEGREES_PER_COUNT * sens).to_radians() * (fov / FOV);
                // Éditeur de lumières avec la caméra libre : c'est elle qui tourne.
                if editing && self.light_editor.free_cam.is_some() {
                    free_turn = (dx as f32 * k, dy as f32 * k);
                } else {
                    self.player.rotate(dx as f32 * k, dy as f32 * k);
                }
            }
            if editing {
                self.light_editor
                    .move_free_cam(input, &self.keys, dt, free_turn);
            }
            // Mode FPS : touches de l'onglet FPS ; marcher et s'accroupir peuvent basculer.
            if fps_mode && !typing && !self.inventory.open {
                if self.keys.walk_toggle {
                    if self.keys.pressed(input, Action::FpsWalk) {
                        self.fps_walk_on = !self.fps_walk_on;
                    }
                } else {
                    self.fps_walk_on = self.keys.held(input, Action::FpsWalk);
                }
                if self.keys.duck_toggle {
                    if self.keys.pressed(input, Action::FpsDuck) {
                        self.fps_duck_on = !self.fps_duck_on;
                    }
                } else {
                    self.fps_duck_on = self.keys.held(input, Action::FpsDuck);
                }
            }
            // Double appui sur « avancer » : on court tant qu'« avancer » reste tenue (mode
            // construction, comme dans Minecraft ; en FPS, on court déjà par défaut).
            if fps_mode || typing || self.inventory.open {
                self.tap_sprint = false;
                self.fwd_tap = 0.0;
            } else {
                if self.keys.pressed(input, Action::Forward) {
                    if self.fwd_tap > 0.0 {
                        self.tap_sprint = true;
                        self.fwd_tap = 0.0;
                    } else {
                        self.fwd_tap = SPRINT_TAP_S;
                    }
                } else {
                    self.fwd_tap = (self.fwd_tap - dt).max(0.0);
                }
                if !self.keys.held(input, Action::Forward) {
                    self.tap_sprint = false;
                }
            }
            let tap_sprint = self.tap_sprint;
            self.fwd_key = self.keys.held(
                input,
                if fps_mode {
                    Action::FpsForward
                } else {
                    Action::Forward
                },
            );
            // (Mode FPS : « courir » n'y est pas tenue — on ne court pas avec Maj —, elle ne
            // sert donc pas de repère ; et le ralenti ne passerait pas le réseau.)
            self.run_key = !fps_mode && self.keys.held(input, Action::Sprint);
            self.fps_aiming = fps_mode
                && (self.keys.held(input, Action::FpsFire)
                    || self.keys.held(input, Action::FpsScope));
            let keys = &self.keys;
            let axis = |pos: Action, neg: Action| {
                keys.held(input, pos) as i32 as f32 - keys.held(input, neg) as i32 as f32
            };
            let mut m = if fps_mode {
                MoveInput {
                    forward: axis(Action::FpsForward, Action::FpsBack),
                    strafe: axis(Action::FpsRight, Action::FpsLeft),
                    up: 0.0,
                    jump: keys.held(input, Action::FpsJump),
                    // Comme dans CS : on court par défaut, « Marcher » ralentit.
                    sprint: !self.fps_walk_on,
                    sneak: self.fps_duck_on,
                    // La molette change d'arme (comme dans CS) ; on saute avec la touche.
                    jump_pressed: keys.pressed(input, Action::FpsJump),
                    fps: true,
                }
            } else {
                MoveInput {
                    forward: axis(Action::Forward, Action::Back),
                    strafe: axis(Action::Right, Action::Left),
                    up: axis(Action::Jump, Action::Sneak),
                    jump: keys.held(input, Action::Jump),
                    sprint: keys.held(input, Action::Sprint) || tap_sprint,
                    sneak: keys.held(input, Action::Sneak),
                    jump_pressed: keys.pressed(input, Action::Jump),
                    fps: false,
                }
            };
            if test_pose().is_some_and(|p| p.2) {
                // (En créatif on vole dès qu'on est placé en l'air : posé, pour s'accroupir.)
                m.sneak = true;
                self.player.flying = false;
            }
            // Touche « avancer » qui remonte mal (voir `stick`) : chez un joueur dont elle
            // traîne après chaque course, lâcher « courir » arrête d'avancer tout de suite.
            // (Pas en mode FPS : « marcher » relâchée y veut dire courir, pas s'arrêter.)
            if !fps_mode {
                let fwd_held = keys.held(input, Action::Forward);
                let run_held = keys.held(input, Action::Sprint);
                // Pour le journal : ce qu'il en était juste avant cette image.
                let (held_for, since_run) = (self.stick.forward_held(), self.stick.since_sprint());
                if self
                    .stick
                    .update(dt, fwd_held, keys.pressed(input, Action::Forward), run_held)
                    && fwd_held
                {
                    m.forward -= 1.0;
                }
                let released = self.fwd_was && !fwd_held;
                self.fwd_was = fwd_held;
                // (Texte du journal : seulement quand une ligne est écrite.)
                let hot = || {
                    minelib_render::thermal::gpu_temp()
                        .map(|t| format!(", carte graphique à {t:.0} °C"))
                        .unwrap_or_default()
                };
                if let Some(s) = self.stick.stop.take() {
                    self.lag_book.add(s.gap, s.held, minelib_render::thermal::gpu_temp());
                    let hot = hot();
                    log::info!(
                        "fin de course : avancer relâchée {:.0} ms après courir (course de \
                         {:.1} s, avancer tenue {:.1} s{hot}){}",
                        s.gap * 1000.0,
                        s.ran,
                        s.held,
                        if !s.stopped {
                            String::new()
                        } else if s.resumed > 0.0 {
                            format!(
                                " ; arrêté dès courir relâchée, mais la touche a traîné plus \
                                 que prévu ({:.2} s) : la marche avait repris {:.0} ms",
                                s.patience,
                                s.resumed * 1000.0
                            )
                        } else {
                            " ; arrêt fait dès courir relâchée".to_string()
                        }
                    );
                } else if released && self.stick.active() && held_for > 0.25 && !run_held {
                    // Avancer relâchée sans « courir » : rien ne protège ce cas (la
                    // touche a pu traîner) ; le journal note dans quelles conditions.
                    let v = Vec3::new(self.player.vel.x, 0.0, self.player.vel.z).length();
                    let hot = hot();
                    log::info!(
                        "touche avancer : relâchée seule après {held_for:.2} s d'appui, à \
                         {v:.1} blocs/s ({}, courir relâchée depuis {}{hot}) : aucun moyen \
                         de savoir quand le doigt s'est levé",
                        if self.player.flying {
                            "en vol"
                        } else {
                            "au sol"
                        },
                        if since_run > 30.0 {
                            "longtemps".to_string()
                        } else {
                            format!("{since_run:.2} s")
                        }
                    );
                }
                // (Autre touche de déplacement : de côté, en arrière, monter ou descendre.)
                let other = m.strafe != 0.0 || m.up != 0.0 || m.forward < 0.0;
                self.measure_stop(dt, fwd_held, run_held, released, other);
            }
            self.stick.remember();
            if let Some(line) = self.lag_book.report(false) {
                log::info!("{line}");
            }
            if let Some(note) = self.stick.note.take() {
                log::info!("touche avancer : {note}");
            }
            if std::mem::take(&mut self.stick.just_learned) {
                log::warn!(
                    "la touche avancer se relâche après la touche courir, trois fois de suite : \
                     elle remonte mal ; désormais, lâcher courir arrête d'avancer"
                );
                self.chat.say(
                    "Ta touche avancer se relâche en retard (elle remonte mal). Le jeu s'arrête \
                     maintenant dès que tu lâches courir (/touchelente off pour l'enlever)."
                        .to_string(),
                    chat::YELLOW,
                );
            }
            // Inventaire, tchat, éditeur de lumières : le joueur ne bouge pas (la caméra
            // libre de l'éditeur, si); il ne casse ni ne pose rien.
            if self.inventory.open || typing || editing {
                m = MoveInput::default();
            }
            // Diagnostic : le joueur avance encore alors qu'aucune touche de déplacement
            // n'est tenue (au sol, hors eau et échelle) : noté une fois au journal, avec
            // tout ce qui peut l'expliquer.
            let flat = Vec3::new(self.player.vel.x, 0.0, self.player.vel.z).length();
            let idle = m.forward == 0.0 && m.strafe == 0.0;
            // Écran de débogage (F3) : ce que le jeu lit du clavier, pour voir d'où vient
            // un déplacement qu'on n'a pas demandé.
            self.move_debug = format!(
                "Touches tenues : {:?}   demandé : avant {:+.0} côté {:+.0}{}",
                input.held_keys(),
                m.forward,
                m.strafe,
                if idle && flat > 0.3 {
                    "   (avance sans touche)"
                } else if self.stick.active() {
                    "   (touche avancer lente : arrêt dès que courir est lâchée)"
                } else if input.injected_keys() {
                    "   (touches tapées par un logiciel)"
                } else {
                    ""
                }
            );
            // Un logiciel tape les touches à la place du clavier (macro, script) : c'est lui
            // qui les relâche, parfois en retard. Le joueur est prévenu une fois.
            // Touches filtrées de Windows (accessibilité, aussi activées en tenant Maj
            // droite 8 s) : Windows retarde ou ignore des appuis. Dit une fois au joueur.
            if let Some(t) = self.filter_keys_in.as_mut() {
                *t -= dt;
            }
            if self.filter_keys_in.is_some_and(|t| t <= 0.0) {
                self.filter_keys_in = None;
                if let Some(f) = minelib_platform::filter_keys() {
                    log::warn!("clavier : {f}");
                    self.chat.say(
                        "Les « touches filtrées » de Windows sont actives : Windows retarde ou \
                         ignore des appuis. Coupe-les dans Paramètres > Accessibilité > Clavier."
                            .to_string(),
                        chat::YELLOW,
                    );
                }
            }
            if input.injected_keys() && !self.injected_warned {
                self.injected_warned = true;
                self.chat.say(
                    "Un logiciel tape ou réécrit tes touches (macro, script) : une touche peut \
                     rester enfoncée après que tu l'as lâchée. Ferme-le pour jouer."
                        .to_string(),
                    chat::YELLOW,
                );
            }
            // En l'air, la vitesse se garde : seulement après 2 s (plus long qu'un saut).
            self.coast_air = if idle && flat > 1.0 && !self.player.on_ground && !self.player.flying
            {
                self.coast_air + dt
            } else {
                0.0
            };
            let coasting = idle
                && ((flat > 0.3 && (self.player.on_ground || self.player.flying))
                    || self.coast_air > 2.0)
                && !self.player.on_ladder
                && self.player.swimming.is_none();
            self.coast = if coasting { self.coast + dt } else { 0.0 };
            if self.coast > 0.3 && !self.coast_logged {
                self.coast_logged = true;
                log::warn!(
                    "le joueur avance sans touche depuis {:.1} s : vitesse {:?}, mode {:?}, \
                     sol {}, vol {}, touches tenues {:?}, saisie {}, inventaire {}",
                    self.coast,
                    self.player.vel,
                    self.game.mode,
                    self.player.on_ground,
                    self.player.flying,
                    input.held_keys(),
                    typing,
                    self.inventory.open
                );
            }
            if !coasting {
                self.coast_logged = false;
            }
            // Spectateur : vol forcé, à travers les blocs.
            let spectator = self.game.mode == GameMode::Spectator;
            self.player.noclip = spectator;
            if spectator {
                self.player.flying = true;
            }
            let here = ChunkPos::containing(
                self.player.pos.x.floor() as i32,
                self.player.pos.z.floor() as i32,
            );
            if self.loading.is_some() {
                // (v220) Pas encore chargé : le joueur reste où il arrive (ni chute, ni
                // poussée).
            } else if self.player.flying || self.world.contains(here) {
                self.simulate(&m, dt);
            } else {
                self.check_fall();
            }
            self.smooth_steps(dt);
            if self.game.mode == GameMode::Fps {
                self.update_fps(input, dt, renderer, typing);
            } else {
                self.fps.buy_open = false;
                self.build_splash();
                // (v209) Échelles : les siennes et celles des autres, dans tous les modes.
                self.own_ladder_noise(dt, true);
                // Sons des autres modes (portes…) : joués tout de suite (sinon ils
                // s'accumulaient et partaient tous d'un coup en passant en mode FPS).
                for (sfx, delay, gain) in self.fps.sounds.drain(..) {
                    crate::audio::play(sfx, delay, gain);
                }
            }
            // Partie en ligne : envoi des entrées et des tirs, marqueur de touche.
            let shots = std::mem::take(&mut self.fps.shots);
            let mut net_locals = Vec::new();
            self.fps.hit_marker.0 = (self.fps.hit_marker.0 - dt).max(0.0);
            // Têtes (blocs « tête », têtes des entités des cartes) : textures téléchargées,
            // têtes proches.
            if self.heads.is_none() {
                // (v309) Comptés depuis l'origine de la partie.
                self.heads = Some(crate::heads::Heads::load(
                    &self.dir,
                    minelib_assets::data_dir().join("tetes"),
                    self.world.origin_blocks(),
                ));
                self.frames = Some(crate::frames::Frames::load(
                    &self.dir,
                    self.world.origin_blocks(),
                ));
                // Lumière des têtes d'entités et des cadres : mesurée par le maillage de
                // leurs chunks.
                self.set_probes(None);
            }
            // Têtes et cadres : triangles gardés sur la carte graphique, refaits (une zone
            // à la fois) seulement quand ils changent ; éclairés comme les blocs.
            if let Some(heads) = self.heads.as_mut() {
                let eye = self.player.eye();
                heads.update(eye);
                heads.refresh(eye, self.daylight);
            }
            if let Some(f) = self.frames.as_mut() {
                f.refresh(self.daylight);
            }
            // Eau et lave qui coulent (hors parties FPS en ligne : la carte y est figée).
            let fluids_on = self.net.is_none() && self.game.mode != GameMode::Fps;
            if fluids_on {
                if self.world.touched.is_none() {
                    self.world.touched = Some(Vec::new());
                }
                // (v185) En solo, dans chaque chunk qui arrive (et la première fois dans
                // ceux déjà là), ce que le joueur a changé (fluide posé ou qui a coulé, bloc
                // retiré) réveille les fluides : rechargée pendant que l'eau séchait, une
                // partie ne la garde plus pour toujours. En ligne, chacun ne fait couler que
                // ce qu'il a changé lui-même.
                if self.build.is_none() {
                    let fresh = match self.world.fresh.take() {
                        Some(f) => f,
                        None => self.world.loaded().collect(),
                    };
                    self.world.fresh = Some(Vec::new());
                    for pos in fresh {
                        for (p, id) in self.world.edits_in(pos) {
                            if id == BlockId::AIR || minelib_world::registry::fluid(id).is_some() {
                                self.fluids.notify(&self.world, p);
                            }
                        }
                    }
                } else {
                    self.world.fresh = None;
                }
                let touched = self
                    .world
                    .touched
                    .as_mut()
                    .map(std::mem::take)
                    .unwrap_or_default();
                for p in touched {
                    self.fluids.notify(&self.world, p);
                }
                let dirty = self.fluids.update(&mut self.world, dt);
                self.dirty.extend(dirty);
                // (v185) Ce que l'eau emporte (torche, redstone, fleur) tombe en objet, en
                // solo ; en ligne, le bloc part seulement (chacun ferait tomber le sien).
                for (p, old) in std::mem::take(&mut self.fluids.washed) {
                    let Some(item) = crate::fluids::washed_item(old) else {
                        continue;
                    };
                    if !self.items.is_local() {
                        continue;
                    }
                    let from = Vec3::new(p[0] as f32 + 0.5, p[1] as f32 + 0.5, p[2] as f32 + 0.5);
                    let mut y = p[1];
                    while y > 0 && !self.world.get(p[0], y - 1, p[2]).is_solid() {
                        y -= 1;
                    }
                    let to = Vec3::new(from.x, y as f32, from.z);
                    let pile = minelib_net::items::Pile {
                        item: item.code(),
                        count: 1,
                    };
                    self.items.drop_item(from, to, pile);
                }
                // (v185) En solo, les objets dans l'eau flottent et suivent le courant.
                // (v247) Un objet tombé dans la lave brûle : son grésillement, selon la
                // distance.
                let eye = self.player.eye();
                for at in self.items.drift(&self.world, dt) {
                    let d = at.distance(eye);
                    if d < 32.0 {
                        crate::audio::burn(1.0 / (1.0 + d / 6.0));
                    }
                }
            } else {
                self.world.touched = None;
                self.world.fresh = None;
            }
            // Construction en ligne : blocs changés envoyés au garant, position aux voisins.
            if dt > 0.0 {
                self.fps_avg += (1.0 / dt - self.fps_avg) * 0.02;
            }
            // (v222) Ce qu'on tient et que les autres voient, envoyé avec la position : la
            // lampe torche (allumée ou non, son réglage) ou la Foudre de Zeus.
            let held = match self.held_item() {
                Some(Item::Flashlight) => Some(minelib_net::poses::Held::Lamp(
                    minelib_net::poses::Lamp {
                        on: self.flashlight_on,
                        step: (self.flashlight_focus
                            * f32::from(minelib_net::poses::Lamp::STEPS))
                        .round() as u8,
                    },
                )),
                Some(Item::Zeus) => Some(minelib_net::poses::Held::Zeus),
                _ => None,
            };
            if let Some(b) = &mut self.build {
                if self.world.journal.is_none() {
                    self.world.journal = Some(Vec::new());
                }
                b.held = held;
                // (v241) La Foudre de Zeus sur soi sans la tenir : les autres la voient dans
                // notre dos.
                b.back = self.game.mode.has_inventory()
                    && self.inventory.has_zeus()
                    && held != Some(minelib_net::poses::Held::Zeus);
                let speed = Vec3::new(self.player.vel.x, 0.0, self.player.vel.z).length();
                let (caps, dirty) = b.update(
                    &mut self.world,
                    self.player.pos,
                    (self.player.yaw, self.player.pitch),
                    // (v303) Accroupi aussi pour les autres quand on descend en vol.
                    self.player.crouch.max(self.fly_crouch),
                    speed,
                    self.fps_avg,
                    dt,
                );
                if let Some(caps) = caps {
                    self.relay_cmds.push(minelib_net::client::Cmd::Caps(caps));
                }
                self.dirty.extend(dirty);
            }
            if let Some(n) = &mut self.net {
                let weapon = self.fps.held_weapon();
                let nades = crate::netgame::NadeOut {
                    throws: std::mem::take(&mut self.fps.throws),
                    blasts: std::mem::take(&mut self.fps.blasts),
                    fires: if n.is_host() {
                        self.fps.fire_cells()
                    } else {
                        Vec::new()
                    },
                };
                n.set_sight(self.fps.is_blind(), self.fps.smoke_spheres());
                let mut locals = n.end_frame(&self.world, shots, weapon, nades, dt);
                for item in std::mem::take(&mut self.fps.requests) {
                    locals.extend(n.buy(item));
                }
                // (v177) Reventes (clic droit dans le menu d'achat).
                for item in std::mem::take(&mut self.fps.sells) {
                    locals.extend(n.sell(item));
                }
                // Armes : lâchées (G), ramassées (E près d'une arme au sol). Ce que chacun
                // porte, c'est l'hôte qui le sait (achats, ramassages).
                // Lancé en cloche devant soi : il retombe à environ 4 blocs.
                let look = self.player.look_dir();
                for w in std::mem::take(&mut self.fps.dropped) {
                    let at = crate::equip::toss_landing(&self.world, self.player.eye(), look);
                    n.drop_weapon(w, at);
                }
                let use_pressed = input.grabbed
                    && !typing
                    && !self.net_dead
                    && self.keys.pressed(input, Action::FpsUse);
                if use_pressed {
                    let feet = self.player.pos;
                    let near = n
                        .ground()
                        .into_iter()
                        .filter(|g| (Vec3::from(g.pos) - feet).length() < 1.6)
                        .min_by(|a, b| {
                            let da = (Vec3::from(a.pos) - feet).length();
                            let db = (Vec3::from(b.pos) - feet).length();
                            da.total_cmp(&db)
                        });
                    if let Some(g) = near {
                        locals.extend(n.pickup(g.id));
                    }
                }
                // Objets lâchés : on anime leur chute en cloche (une fois chacun).
                let ground = n.ground();
                for g in &ground {
                    let fresh = !self.ground_seen.contains(&g.id);
                    if fresh {
                        self.ground_seen.insert(g.id);
                        if Vec3::from(g.from) != Vec3::from(g.pos) {
                            self.ground_fly.insert(g.id, 0.0);
                        }
                    }
                }
                self.ground_fly.retain(|id, t| {
                    *t += dt;
                    *t < TOSS_FLIGHT && ground.iter().any(|g| g.id == *id)
                });
                if self.ground_seen.len() > 512 {
                    self.ground_seen
                        .retain(|id| ground.iter().any(|g| g.id == *id));
                }
                {
                    use minelib_net::game::{bomb, FLAG_BOMB};
                    let hud = n.hud();
                    let carrier = if hud.carrying {
                        Some(self.player.pos)
                    } else {
                        n.views()
                            .iter()
                            .find(|v| v.alive && v.flags & FLAG_BOMB != 0)
                            .map(|v| v.pos)
                    };
                    if hud.bomb == bomb::DROPPED && self.bomb_last.0 == bomb::CARRIED {
                        if let Some(from) = self.bomb_last.1 {
                            let to = Vec3::from(hud.bomb_pos);
                            if (to - from).length() > 0.5 {
                                self.bomb_fly = Some((from + Vec3::Y * 1.3, to, 0.0));
                            }
                        }
                    }
                    if let Some(f) = &mut self.bomb_fly {
                        f.2 += dt;
                        if f.2 >= TOSS_FLIGHT || hud.bomb != bomb::DROPPED {
                            self.bomb_fly = None;
                        }
                    }
                    // (v177) Bombe posée : un bip de plus en plus rapproché (une fois par
                    // seconde au début, moins de sept à la fin, pas plus), le voyant
                    // s'allume à chaque bip ; puis l'explosion, qu'on entend de loin.
                    let at = Vec3::from(hud.bomb_pos);
                    let eye = self.player.eye();
                    if hud.bomb == bomb::PLANTED {
                        self.bomb_beep -= dt;
                        if self.bomb_beep <= 0.0 {
                            self.bomb_beep = bomb_beep_gap(hud.time);
                            self.bomb_flash = 0.12;
                            let d = (at - eye).length();
                            self.fps.sounds.push((
                                crate::audio::Sfx::BombBeep,
                                0.0,
                                1.0 / (1.0 + d / 12.0),
                            ));
                        }
                    } else {
                        self.bomb_beep = 0.0;
                    }
                    self.bomb_flash = (self.bomb_flash - dt).max(0.0);
                    if hud.bomb == bomb::EXPLODED && self.bomb_last.0 == bomb::PLANTED {
                        self.fps.bomb_blast(at, eye, &self.world);
                    }
                    self.bomb_last = (
                        hud.bomb,
                        carrier.or(self.bomb_last.1.filter(|_| hud.bomb == bomb::CARRIED)),
                    );
                }
                // (v209) Minimap : qui de l'équipe voit quel adversaire (voir `spotting`).
                {
                    let hud = n.hud();
                    let views = n.views();
                    let me_alive = !self.net_dead && self.spectating.is_none();
                    let mut observers: Vec<crate::spotting::Observer> = views
                        .iter()
                        .filter(|v| v.alive && v.team == hud.team)
                        .map(|v| crate::spotting::Observer {
                            pseudo: &v.pseudo,
                            eye: eye_of(v.pos, v.crouch),
                            look: {
                                let (sy, cy) = v.yaw.sin_cos();
                                let (sp, cp) = v.pitch.sin_cos();
                                Vec3::new(sy * cp, sp, -cy * cp)
                            },
                            own_blind: None,
                        })
                        .collect();
                    if me_alive {
                        observers.push(crate::spotting::Observer {
                            pseudo: "",
                            eye: self.player.eye(),
                            look: self.player.look_dir(),
                            own_blind: Some(self.fps.blind_amount()),
                        });
                    }
                    for at in self.fps.take_flashes() {
                        self.spotting.flash(at, &self.world, &observers);
                    }
                    let targets: Vec<crate::spotting::Target> = views
                        .iter()
                        .filter(|v| v.alive && v.team != hud.team)
                        .map(|v| crate::spotting::Target {
                            pseudo: &v.pseudo,
                            feet: v.pos,
                            crouch: v.crouch,
                        })
                        .collect();
                    let smokes = self.fps.smoke_spheres();
                    // (v247) La bombe posée ou lâchée : les CT ne la voient sur la minimap qu'aux
                    // conditions d'un ennemi (`Spotting::bomb_shown`).
                    use minelib_net::game::bomb as bs;
                    let bomb_at = matches!(hud.bomb, bs::PLANTED | bs::DROPPED)
                        .then(|| Vec3::from(hud.bomb_pos));
                    self.spotting
                        .update(dt, &self.world, &observers, &targets, &smokes, bomb_at);
                }
                // Mort : on suit un coéquipier (clic : le suivant).
                if self.net_dead {
                    let hud = n.hud();
                    let mates: Vec<crate::netgame::View> = n
                        .views()
                        .into_iter()
                        .filter(|v| v.alive && v.team == hud.team)
                        .collect();
                    if mates.is_empty() {
                        self.spectating = None;
                    } else {
                        if input.grabbed && self.keys.pressed(input, Action::FpsFire) {
                            self.spectate_idx += 1;
                        }
                        // (v174) Clic droit (lunette) : le précédent.
                        if input.grabbed && self.keys.pressed(input, Action::FpsScope) {
                            let n = mates.len();
                            self.spectate_idx = (self.spectate_idx % n + n - 1) % n;
                        }
                        let v = &mates[self.spectate_idx % mates.len()];
                        self.player.pos = v.pos;
                        self.player.yaw = v.yaw;
                        self.player.pitch = v.pitch;
                        self.player.crouch = v.crouch;
                        self.player.vel = Vec3::ZERO;
                        // (v209) Son écran comme si c'était soi : vie, équipement, arme en
                        // main (une autre personne regardée : tout repart de zéro).
                        if self.spectating.as_deref() != Some(v.pseudo.as_str()) {
                            self.spec_fps = crate::fps::Fps::new();
                        }
                        if let Some(w) = n.watched(&v.pseudo) {
                            self.spec_fps.mirror(&w, dt);
                        }
                        self.spectating = Some(v.pseudo.clone());
                        // (v209) Partie solo : touche Utiliser, on prend la place du bot.
                        if input.grabbed
                            && !typing
                            && self.keys.pressed(input, Action::FpsUse)
                            && n.is_bot(&v.pseudo)
                        {
                            // (Ce qui en découle arrive à la fin de l'image : `TakeOver`.)
                            let pseudo = v.pseudo.clone();
                            n.take_over(&pseudo);
                        }
                    }
                } else {
                    self.spectating = None;
                }
                // Poser (touche Utiliser ou Bombe maintenue) / désamorcer (Utiliser).
                let free = input.grabbed && !typing && !self.net_dead;
                let use_key = free && self.keys.held(input, Action::FpsUse);
                let bomb_key = free
                    && (self.keys.held(input, Action::FpsBomb)
                        || (self.fps.holding_bomb() && self.keys.held(input, Action::FpsFire)));
                n.keys(use_key || bomb_key, use_key);
                // Argent et équipement venus de l'hôte.
                let hud = n.hud();
                self.fps.online = Some(crate::fps::Online {
                    team: hud.team,
                    money: hud.money,
                    armor: hud.armor,
                    helmet: hud.helmet,
                    kit: hud.kit,
                    bomb: hud.carrying,
                    can_buy: hud.can_buy,
                    warmup: hud.phase == minelib_net::game::phase::WARMUP,
                });
                net_locals = locals;
                for f in &mut self.feed {
                    f.1 += dt;
                }
                self.feed.retain(|f| f.1 < 6.0);
                if let Some(b) = &mut self.banner {
                    b.2 -= dt;
                    if b.2 <= 0.0 {
                        self.banner = None;
                    }
                }
                if let Some(g) = &mut self.game_over {
                    g.1 += dt;
                }
            }
            self.apply_net(net_locals);
            self.update_voice(input, dt, typing);
            let held = self.held_item();
            let speed = if self.player.on_ground && !self.player.flying {
                Vec3::new(self.player.vel.x, 0.0, self.player.vel.z).length()
            } else {
                0.0
            };
            // (v220) Geste de la main : décidé en cassant, en frappant ou en posant (voir
            // `handle_blocks`, à l'image d'avant).
            let gesture = if !fps_mode && !typing && !editing && !self.inventory.open {
                std::mem::take(&mut self.gesture)
            } else {
                self.gesture = crate::hand::Gesture::None;
                crate::hand::Gesture::None
            };
            self.hand.update(dt, speed, gesture, held);
            // (v290) La main vide se tend le temps d'un geste (vite), puis retombe.
            let raise = if self.hand.busy() { dt / HAND_UP_S } else { -dt / HAND_BACK_S };
            self.hand_raise = (self.hand_raise + raise).clamp(0.0, 1.0);
            // Balancement des bras et des jambes du modèle (vue F5), et des autres joueurs.
            self.walk_phase = (self.walk_phase + stride_rate(speed) * dt) % std::f32::consts::TAU;
            // (v165, essai) Foule simulée en solo, hors mode FPS. (v185) Seulement si on l'a
            // demandée (`/npc stresstest true`), et sans message à l'arrivée.
            // (v227) En construction en ligne, un OP la mène aussi pour les joueurs proches.
            let solo = self.build.is_none() && self.net.is_none();
            let lead = self.build.is_some() && self.crowd_out.is_some();
            if self.crowd_on && (solo || lead) && self.game.mode != GameMode::Fps {
                let c = self.crowd.get_or_insert_with(crate::crowd::Crowd::new);
                c.update(&self.world, &self.skins, self.player.pos, dt);
            } else {
                self.crowd = None;
            }
            self.update_shared_crowds(dt);
            // (Avec leur pose : l'anticrénelage temporel a besoin de celle de l'image
            // d'avant, voir `Walk`.)
            // (v191) Des milliers de joueurs : leur pseudo n'est plus recopié à chaque
            // image, et chaque marche porte le numéro de l'image où on l'a vue — celles
            // qui ne l'ont pas sont parties. Avant, 4 000 chaînes allouées et un ensemble
            // de tous les pseudos, par image.
            self.walk_stamp = self.walk_stamp.wrapping_add(1);
            let (stamp, walks) = (self.walk_stamp, &mut self.walks);
            // (v303) (Les autres joueurs : pas encore de saut, il faudrait que leur jeu le
            // dise ; leur direction et leur course se voient à leur déplacement.)
            let mut mark = |pseudo: &str, pos: Vec3, yaw: f32, pitch: f32, crouch: f32| {
                let w = match walks.get_mut(pseudo) {
                    Some(w) => w,
                    None => walks.entry(pseudo.to_string()).or_insert(Walk::new(pos)),
                };
                w.update(pos, yaw, pitch, crouch, false, dt);
                w.stamp = stamp;
            };
            if let Some(n) = &self.net {
                for v in n.views() {
                    mark(&v.pseudo, v.pos, v.yaw, v.pitch, v.crouch);
                }
            }
            if let Some(b) = &self.build {
                for o in b.others.values() {
                    mark(&o.pseudo, o.pos, o.yaw, o.pitch, o.crouch);
                }
            }
            drop(mark);
            self.walks.retain(|_, w| w.stamp == stamp);
            self.others_ladder_noise(dt);
            let target = (speed / 4.3).min(1.0);
            self.walk_amp += (target - self.walk_amp) * (dt * 10.0).min(1.0);
            // (v303) Le reste de l'allure de notre modèle : le saut, la direction du
            // déplacement par rapport au regard, la course ; « et qu'on se sneak quand on
            // descend en vol » : en vol, tant qu'on descend, il se met accroupi (lui
            // seulement : ni la caméra ni notre boîte ne bougent).
            {
                let p = &self.player;
                let free = !p.flying && !p.on_ladder && p.swimming.is_none() && !p.noclip;
                let air = crate::player_model::air_of(!p.on_ground && free, p.vel.y);
                self.anim.0 += (air - self.anim.0) * (dt * 12.0).min(1.0);
                let flat = Vec3::new(p.vel.x, 0.0, p.vel.z);
                if flat.length() > 0.3 {
                    let to = crate::player_model::heading(flat, p.yaw);
                    self.anim.1 = crate::player_model::turn_toward(self.anim.1, to, dt * 10.0);
                }
                let run = crate::player_model::run_of(flat.length());
                self.anim.2 += (run - self.anim.2) * (dt * 8.0).min(1.0);
                let sinking = p.flying && !p.noclip && p.vel.y < -0.5;
                let step = if sinking { dt / FLY_CROUCH_S } else { -dt / FLY_CROUCH_S };
                self.fly_crouch = (self.fly_crouch + step).clamp(0.0, 1.0);
                if let Some(fixed) = test_gait() {
                    self.anim = fixed;
                }
            }
            self.hand_light = self.light_at_player(held);
            if !editing {
                self.handle_blocks(input, dt);
                self.update_items(input, typing);
            }
            if self.inventory.open {
                let (w, h) = renderer.size();
                if self.inventory.chest.is_none() {
                    self.inventory.pointer(
                        (w as f32, h as f32),
                        input.cursor,
                        input.mouse_pressed(Mouse::Left),
                        input.mouse_held(Mouse::Left),
                        input.mouse_held(Mouse::Right),
                    );
                }
                if let Some(c) = input.cursor {
                    for (button, right) in [(Mouse::Left, false), (Mouse::Right, true)] {
                        if input.mouse_pressed(button) {
                            let shift =
                                input.held(KeyCode::ShiftLeft) || input.held(KeyCode::ShiftRight);
                            self.inventory.click((w as f32, h as f32), c, right, shift);
                        }
                    }
                    // (v295) En créatif, clic molette sur un objet : une pile de cet objet.
                    if input.mouse_pressed(Mouse::Middle) {
                        self.inventory.middle_click((w as f32, h as f32), c);
                    }
                }
            }
            self.autosave -= dt;
            if self.autosave <= 0.0 {
                self.autosave = AUTOSAVE_S;
                let _s = perf::scope("sauvegarde automatique");
                perf::event(|| "sauvegarde automatique".into());
                if let Err(e) = self.save() {
                    log::error!("sauvegarde automatique impossible : {e:#}");
                }
            }
        }

        // (v220) Éclair tenu : son crépitement (arrêté en pause).
        self.update_crackles(active);
        drop(logic);
        if !self.dirty.is_empty() {
            self.vox_dirty = true;
            let n = self.dirty.len();
            perf::event(|| format!("{n} chunks à remailler (bloc ou lumière modifiés)"));
        }
        for pos in std::mem::take(&mut self.dirty) {
            self.streamer
                .remesh_now(&self.world, &mut self.workers, pos);
        }
        // (v216) Météo, foudre, pluie, Foudre de Zeus tenue.
        {
            let _s = perf::scope("météo et foudre");
            self.update_storm(dt);
        }
        // Lumières de l'éditeur : lumières automatiques, enregistrement, envoi au rendu
        // (avant la grille de blocs : leurs ombres en ont besoin).
        {
            let _s = perf::scope("lumières de l'éditeur");
            self.update_light_sync(dt);
            let cam = self.camera_pos();
            let allowed = self.lights_allowed();
            self.lights.update(dt, &self.world, cam, allowed);
            if std::mem::take(&mut self.lights.gobos_dirty) {
                let imgs: Vec<minelib_render::GoboImage> =
                    self.lights.gobos.iter().map(|g| g.image.clone()).collect();
                renderer.set_gobos(&imgs);
            }
            let mut list = if allowed {
                self.lights.frame_lights()
            } else {
                Vec::new()
            };
            // (v201) Lampe torche allumée en main, ou torche tenue avec les torches en
            // lumières ponctuelles : la première (sa carte d'ombre, à refaire dès qu'on
            // bouge, passe avant celles des autres).
            // (v216) De même la Foudre de Zeus tenue ; et les coups de foudre proches
            // éclairent le sol et les murs autour d'eux.
            let mut at = 0;
            if let Some(l) = self
                .flashlight_light()
                .or_else(|| self.held_torch_light())
                .or_else(|| self.zeus_light())
            {
                list.insert(0, l);
                at = 1;
            }
            // (v222) Lampes allumées et éclairs des autres joueurs, les plus proches : juste
            // après la sienne (portées, leurs cartes d'ombre suivent chaque image).
            let remote = if allowed {
                self.remote_held_lights(cam)
            } else {
                Vec::new()
            };
            self.remote_lamps.clear();
            for (k, (pseudo, l)) in remote.into_iter().enumerate() {
                self.remote_lamps.push((pseudo, l.owner));
                list.insert((at + k).min(list.len()), l);
            }
            if allowed {
                self.storm.lights(cam, self.daylight * self.storm.light(), &mut list);
            }
            list.truncate(minelib_render::MAX_DYN_LIGHTS);
            // (v247) « Augmente la distance à laquelle on voit les rayons de lumière, à distance
            // égale avec celle où elle éclaire » : une lumière éclaire tant que son chunk est
            // chargé (distance de rendu) ; ses rayons se voient jusque-là aussi (avant : 48 à
            // 192 blocs selon la qualité). La marche ne coûte que là où passent les lumières.
            let mut st = self.lights.settings.render;
            st.vl_max_dist = st.vl_max_dist.max((self.view_distance + 1) as f32 * 16.0);
            renderer.set_dyn_lights(&list, &st);
            self.shadow_lights.clear();
            self.shadow_lights.extend(
                list.iter()
                    .filter(|l| l.shadows && !l.blocks_only)
                    .map(|l| (Vec3::from(l.pos), l.range)),
            );
            renderer.set_torch_swap(self.lights.torch_swap());
            self.lights_wanted = !list.is_empty();
            self.lights_state = renderer.dyn_stats().state;
            if self.lights_wanted && !self.lights_marked {
                self.lights_marked = true;
                crate::lights::mark_used(&minelib_assets::data_dir());
            }
            if self.lights_wanted
                && self.lights_state == minelib_render::DynState::Failed
                && !self.lights_failed_said
            {
                self.lights_failed_said = true;
                self.chat.say(
                    "Lumières indisponibles sur cette carte graphique (voir le journal).",
                    chat::RED,
                );
            }
        }
        {
            let _s = perf::scope("grille raytracing");
            self.update_voxels(renderer, dt);
        }
        {
            let _s = perf::scope("lumières proches");
            self.update_lights(renderer, dt);
        }
        // (v209) Partie FPS servie ici (contre les bots, ou hôte d'une partie en ligne) : le
        // sol sous chaque joueur reste chargé, même loin de soi (petite distance de vue, carte
        // plus grande qu'elle) ; sinon sa simulation l'attend (`netgame::step`) au lieu de le
        // faire tomber à travers le sol, et lui avec (il réapparaissait sous la carte).
        if let Some(n) = self.net.as_ref().filter(|n| n.is_host()) {
            let mut set = std::collections::HashSet::new();
            for v in n.views() {
                let c = ChunkPos::containing(v.pos.x.floor() as i32, v.pos.z.floor() as i32);
                for dz in -1..=1 {
                    for dx in -1..=1 {
                        set.insert(ChunkPos {
                            x: c.x + dx,
                            z: c.z + dz,
                        });
                    }
                }
            }
            // Au plus 12 par image (un chunk importé se décode vite, un chunk généré moins) :
            // au départ d'une manche, tout est là avant la fin du temps d'achat.
            for c in self.streamer.pin(&self.world, set).into_iter().take(12) {
                self.world.ensure_chunk(c);
            }
        } else {
            self.streamer
                .pin(&self.world, std::collections::HashSet::new());
        }
        self.streamer.update(
            &mut self.world,
            renderer,
            &mut self.workers,
            self.player.pos.x,
            self.player.pos.z,
        );
        // (v255) `/fps showelements` : zones relues, portes cherchées.
        self.update_fps_elements(dt);
        // Têtes des chunks maillés : blocs « tête » et lumière reçue (têtes d'entités, puis
        // cadres : voir `set_probes`).
        if self.heads.is_some() {
            for (pos, spots, probes) in self.streamer.take_heads() {
                let n = self.probe_split.get(&pos).copied().unwrap_or(0);
                let (hp, fp) = probes.split_at(n.min(probes.len()));
                if let Some(h) = self.heads.as_mut() {
                    h.chunk_meshed(pos, spots, hp.to_vec());
                }
                if let Some(f) = self.frames.as_mut().filter(|_| !fp.is_empty()) {
                    f.set_light(pos, fp);
                }
            }
        }
        let _s = perf::scope("visée");
        // (v278) Depuis la caméra (elle avance accroupi).
        self.hit = raycast(
            &self.world,
            self.aim_eye().into(),
            self.player.look_dir().into(),
            REACH,
        );
    }

    fn handle_keys(&mut self, input: &Input) {
        // Mode FPS : pas d'inventaire.
        if self.keys.pressed(input, Action::Inventory) && self.game.mode.has_inventory() {
            self.inventory.toggle();
        }
        // F5 : 1re personne → 3e de dos → 3e de face (pas en mode FPS).
        if self.keys.pressed(input, Action::Perspective) && self.game.mode != GameMode::Fps {
            self.perspective = (self.perspective + 1) % 3;
        }
        // (v220) F1 : plus d'interface, seulement le jeu et les pseudos (voir `hud_off`) ;
        // un second appui la rend.
        if self.keys.pressed(input, Action::HideHud) && self.game.mode != GameMode::Fps {
            self.hud_hidden = !self.hud_hidden;
        }
        // Double appui sur Espace : vol (toujours en créatif, en survie après /fly).
        if self.keys.pressed(input, Action::Jump) && !self.inventory.open {
            if self.since_space < DOUBLE_TAP_S && self.fly_allowed() {
                self.player.flying = !self.player.flying;
                self.player.vel = Vec3::ZERO;
                self.since_space = 1.0; // pas de triple appui
            } else {
                self.since_space = 0.0;
            }
        }
        let mut dv = 0;
        if input.pressed(KeyCode::PageUp) {
            dv += 2;
        }
        if input.pressed(KeyCode::PageDown) {
            dv -= 2;
        }
        if dv != 0 {
            self.view_distance = (self.view_distance + dv).clamp(2, MAX_VIEW_DISTANCE);
            self.settings_changed();
        }
    }

    fn handle_hotbar(&mut self, input: &Input) {
        let keys = [
            KeyCode::Digit1,
            KeyCode::Digit2,
            KeyCode::Digit3,
            KeyCode::Digit4,
            KeyCode::Digit5,
            KeyCode::Digit6,
            KeyCode::Digit7,
            KeyCode::Digit8,
            KeyCode::Digit9,
            KeyCode::Digit0,
        ];
        let before = self.inventory.selected_item();
        for (i, key) in keys.into_iter().enumerate() {
            if input.pressed(key) {
                self.inventory.select(i);
            }
        }
        // En mode FPS la molette sert à sauter. Face à un écran vidéo, sa hitbox à portée,
        // elle ne change jamais l'objet tenu (v176) : elle fait défiler la page si celle-ci
        // peut défiler, sinon rien (voir `update_screens`).
        if !self.inventory.open
            && input.scroll.abs() >= 1.0
            && self.game.mode != GameMode::Fps
            && self.screen_aim.is_none()
        {
            // (v215) Lampe torche allumée en main : la molette règle son faisceau au lieu de
            // changer d'objet (vers le haut : plus serré, plus fort et plus loin ; vers le
            // bas : plus large, moins fort et moins loin), gardé pour la suite.
            if self.flashlight_on && before == Some(Item::Flashlight) {
                let step = input.scroll.signum() / crate::flashlight::STEPS;
                let f = (self.flashlight_focus + step).clamp(0.0, 1.0);
                if (f - self.flashlight_focus).abs() > 1e-4 {
                    self.flashlight_focus = f;
                    crate::flashlight::save_focus(f);
                }
            } else {
                self.inventory.scroll(-input.scroll.signum() as i32);
            }
        }
        // (v215) On arrive sur la lampe torche (molette, chiffre) : elle est éteinte.
        let now = self.inventory.selected_item();
        if now != before && now == Some(Item::Flashlight) {
            self.flashlight_on = false;
        }
    }

    /// `/yt` : ouvre YouTube (accueil, recherche ou adresse) sur les écrans vidéo.
    fn youtube(&mut self, query: &str) {
        if self.shown.is_empty() {
            self.chat.say(
                "Pose d'abord un écran vidéo (objet « Écran vidéo », /give ecran).",
                chat::RED,
            );
            return;
        }
        // L'écran regardé en dernier, sinon le premier.
        let id = self
            .screen_last
            .filter(|l| self.shown.iter().any(|s| s.0.id() == *l))
            .unwrap_or_else(|| self.shown[0].0.id());
        let url = crate::screen::youtube_url(query);
        if self.displays.open(id, &url) {
            self.chat.say(format!("Écran : {url}"), chat::YELLOW);
        } else {
            self.chat.say(
                "Navigateur absent : il faut le dossier « cef » (minelib-browser) dans .minelib.",
                chat::RED,
            );
        }
    }

    /// Écrans vidéo : chargement (première image), une page par écran et point visé par le
    /// réticule. On n'agit sur un écran qu'en le visant, sa hitbox à portée de main
    /// (`REACH`). Sur un écran, « utiliser » le touche (tactile), « casser » le casse
    /// (l'objet revient, hors créatif) et « copier le bloc » met l'objet en main ; accroupi,
    /// « utiliser » l'ignore et pose le bloc tenu devant (voir `handle_blocks`). En mode
    /// FPS, la touche « utiliser » le touche ; on ne le casse pas. Un clic dans un champ de
    /// texte de la page y envoie le clavier tant que le réticule reste sur l'écran.
    ///
    /// En construction en ligne, les écrans des joueurs proches sont montrés aussi, et les
    /// vidéos YouTube partagées (voir `crate::watch`) ; seul leur propriétaire les casse.
    fn update_screens(&mut self, input: &Input, dt: f32, typing: bool) {
        if !self.screens_loaded {
            self.screens = crate::screen::load(&self.dir, self.world.origin_blocks());
            self.screens_loaded = true;
        }
        if self.build.is_none() {
            self.party.clear();
        }
        // (v318) Ses écrans partent à leur place absolue (voir `watch::Party::set_origin`).
        self.party.set_origin(self.world.origin_blocks());
        self.shown = self.screens.iter().map(|s| (*s, None)).collect();
        self.shown.extend(
            self.party
                .screens(&self.screens)
                .into_iter()
                .map(|(s, owner)| (s, Some(owner))),
        );
        // Hitbox pour la physique.
        self.player.solids = self.shown.iter().map(|s| s.0.hitbox()).collect();
        // (v243) Le jeu est interdit aux moins de 18 ans : le web sauf les interdits (liste
        // noire), en solo comme en ligne. En ligne, une session du navigateur à part (son
        // propre compte, s'il s'y connecte) ; chacun voit ses pubs.
        let en_ligne = self.build.is_some() || self.net.is_some();
        let mode = if en_ligne { "en_ligne" } else { self.solo_filter() };
        self.displays.set_filter(mode);
        // Une page par écran : onglets ouverts et fermés avec les écrans.
        let ids: Vec<u32> = self.shown.iter().map(|s| s.0.id()).collect();
        self.displays.sync(&ids);
        let fps = self.game.mode == GameMode::Fps;
        let use_key = if fps { Action::FpsUse } else { Action::Use };
        // Écran regardé (même de loin : il a le plus d'images par seconde) ; on n'agit
        // dessus que si sa hitbox est à portée de main.
        let look = if self.shown.is_empty() {
            None
        } else {
            self.aimed_screen(SCREEN_LOOK)
        };
        self.screen_aim = None;
        if input.grabbed
            && !typing
            && !self.inventory.open
            && self.game.mode != GameMode::Spectator
        {
            self.screen_aim = look.filter(|a| a.1 <= REACH);
        }
        self.schedule_screens(look.map(|a| ids[a.0]));
        // (v210) L'écran regardé, et quelle part de lui est à l'image : seul celui-là peut
        // lancer sa vidéo de lui-même (règles du lecteur officiel de YouTube).
        let regarde = look.and_then(|(i, _, _)| {
            let s = self.shown.get(i)?;
            let (_, view, _) = self.view_camera();
            let proj = Mat4::perspective_infinite_reverse_rh(
                self.fov.to_radians(),
                self.screen_size.0 / self.screen_size.1.max(1.0),
                0.05,
            );
            Some((ids[i], crate::screen::visible_part(&s.0.anchor(), proj * view)))
        });
        self.displays.look(regarde, dt);
        // (v220) Seulement de face : par derrière, on ne touche que sa hitbox (la casser, la
        // copier), pas sa page.
        let aimed = self
            .screen_aim
            .filter(|a| a.2[0] >= 0.0)
            .map(|(i, _, uv)| (ids[i], uv));
        let editing = aimed.is_some_and(|a| self.displays.editing(a.0));
        if let Some((id, _)) = aimed {
            self.screen_last = Some(id);
        }
        // (v247) « Quand je tape l'écran avec l'éclair, ça ne doit pas le casser, et bien jeter
        // le sort » : la Foudre de Zeus en main, le clic gauche est pour elle (`handle_blocks`).
        let zeus_held = self.inventory.selected_item() == Some(Item::Zeus);
        if let Some((i, _, uv)) = self.screen_aim {
            let near = !fps;
            if near && self.keys.pressed(input, Action::Attack) && !zeus_held {
                match self.shown.get(i).and_then(|s| s.1.clone()) {
                    Some(owner) => self.chat.say(
                        format!("Cet écran est à {owner} : lui seul peut le retirer."),
                        chat::YELLOW,
                    ),
                    None => self.break_screen(i),
                }
                self.screen_broke = true;
            } else if near
                && self.keys.pressed(input, Action::Pick)
                && self.inventory.selected_item() != Some(Item::Flashlight)
            {
                let size = self.shown.get(i).map_or(crate::screen::NORMAL, |s| s.0.size);
                self.give_picked(Item::Screen(size));
            } else if self.keys.pressed(input, use_key)
                && uv[0] >= 0.0
                && (fps || !self.keys.held(input, Action::Sneak))
                // En mode FPS, « utiliser » est une lettre : elle va dans le champ de texte.
                && !(fps && editing)
            {
                // (v289) Plus de boutons en haut à gauche : Google, retour et suivant sont
                // dans le volet de gauche (un clic comme un autre, voir `Displays::press`).
                {
                    self.displays.press(ids[i], uv);
                    self.screen_touch = Some(ids[i]);
                }
            }
        }
        // Molette sur un écran dont la page peut défiler dans ce sens sous le réticule :
        // elle fait défiler la page. (Face à l'écran, elle ne va jamais à la barre
        // d'objets, que la page défile ou non : voir `handle_hotbar`.)
        if let Some((id, uv)) = aimed {
            if input.scroll.abs() >= 1.0 && self.displays.page_scrolls(id, input.scroll > 0.0) {
                self.displays.wheel(id, uv, (input.scroll * 120.0) as i32);
            }
        }
        // Clavier dans le champ de texte de l'écran visé.
        if let Some((id, _)) = aimed.filter(|_| editing) {
            self.screen_keyboard(input, dt, id);
        }
        if let Some(id) = self.screen_touch {
            if !self.keys.held(input, use_key) {
                let uv = aimed.filter(|a| a.0 == id).map(|a| a.1);
                self.displays.release(id, uv);
                self.screen_touch = None;
            }
        }
        if self.screen_broke && !self.keys.held(input, Action::Attack) {
            self.screen_broke = false;
        }
        // (v243) Où le regard coupe chaque écran, à toute distance : les boutons d'un écran
        // n'apparaissent que si le regard franchit son bord, pas en marchant vers lui.
        {
            let eye = self.player.eye();
            let dir = self.player.look_dir();
            // (v257) Et la distance à chacun : la file d'un écran et sa recherche ouverte ne
            // s'en vont qu'en s'éloignant.
            let rays: Vec<(u32, Option<[f32; 2]>, f32)> = self
                .shown
                .iter()
                .map(|(s, _)| {
                    let a = s.anchor();
                    (s.id(), crate::screen::ray_uv(&a, eye, dir), crate::screen::screen_dist(&a, eye))
                })
                .collect();
            self.displays.rays(&rays);
        }
        self.displays.hover(aimed);
        self.displays.update(dt);
        // (v247) L'installation du navigateur des écrans : un mot au joueur.
        for mot in crate::navigateur::mots() {
            self.chat.say(format!("Écrans : {mot}"), chat::YELLOW);
        }
        // Pages refusées par le filtre : un mot au joueur, une fois par raison.
        // (v261) Le même refus plusieurs fois de suite (une page qui insiste, un clic
        // redirigé) n'est dit qu'une fois en 5 secondes.
        self.refus_dit.retain(|_, t| t.elapsed().as_secs_f32() < 5.0);
        for (_, raison) in self.displays.take_blocked() {
            let quoi = match raison {
                1 => "ce genre d'adresse ne s'ouvre pas sur un écran",
                2 => "pas de machine du réseau local sur un écran",
                4 => "site filtré",
                5 => "pas de connexion à un compte sur les écrans en ligne",
                6 => "Twitch et Vimeo ne se lisent pas sur les écrans (leurs vidéos demandent \
                      des codecs que le navigateur n'a pas)",
                7 => "site pour adultes ou dangereux",
                _ => "site hors de la liste autorisée",
            };
            if self.refus_dit.contains_key(&raison) {
                continue;
            }
            self.refus_dit.insert(raison, std::time::Instant::now());
            self.chat.say(format!("Écran : {quoi}."), chat::YELLOW);
        }
        self.watch_together(dt);
    }

    /// Regarder ensemble (construction en ligne) : ce que rapportent les pages, messages
    /// aux joueurs proches (par le canal de la voix), pages amenées à la vidéo partagée.
    /// (v205) En solo : la liste noire si son fichier est installé, sinon la liste
    /// blanche. (v243) Le jeu est interdit aux moins de 18 ans : la liste noire toujours
    /// (le navigateur en a une copie intégrée), en solo comme en ligne.
    fn solo_filter(&mut self) -> &'static str {
        if let Some(m) = self.solo_filtre {
            return m;
        }
        let m = "noire";
        log::info!("écrans vidéo : filtrage en solo, liste {m}");
        self.solo_filtre = Some(m);
        m
    }

    fn watch_together(&mut self, dt: f32) {
        let states = self.displays.take_states();
        let orders = self.displays.take_orders();
        // (v289) Le volet de chaque écran montre sa file (en ligne seulement).
        let en_ligne = self.build.is_some();
        self.displays.set_files(en_ligne, |id| self.party.liste(id));
        let Some(b) = &self.build else {
            // (v253) Sans regarder ensemble, une vidéo cliquée s'ouvre, tout simplement.
            for (id, json, cote) in orders {
                if let Some(crate::watch::Act::Open(id, url)) =
                    crate::watch::Party::new().order(id, &json, "", cote)
                {
                    self.displays.open(id, &url);
                }
            }
            return;
        };
        let me = b.pseudo().to_string();
        // (v253) Ordres des pages : vidéo cliquée (file d'attente), vidéo retirée. (v257) Une
        // vidéo cliquée dans la recherche à côté du lecteur va dans la file sans rien rouvrir.
        for (id, json, cote) in orders {
            match self.party.order(id, &json, &me, cote) {
                Some(crate::watch::Act::Open(id, url)) => {
                    self.displays.open(id, &url);
                }
                Some(crate::watch::Act::Eval(id, js)) => self.displays.eval(id, &js),
                None => {}
            }
        }
        let players: Vec<(String, Vec3)> = b
            .others
            .values()
            .map(|o| (o.pseudo.clone(), o.pos))
            .collect();
        for (id, json) in states {
            self.party.observe(id, &json, &me);
        }
        for act in self.party.update(dt, &self.screens, &players) {
            match act {
                crate::watch::Act::Open(id, url) => {
                    self.displays.open(id, &url);
                }
                crate::watch::Act::Eval(id, js) => self.displays.eval(id, &js),
            }
        }
        let out = self.party.take_out();
        if let Some(b) = &self.build {
            for (to, data) in out {
                b.send_voice(to, data);
            }
        }
    }

    /// Clavier dans le champ de texte de l'écran `id` : texte tapé et touches spéciales
    /// (effacer, flèches… répétées si elles restent tenues ; Ctrl+A/C/V/X/Y/Z).
    fn screen_keyboard(&mut self, input: &Input, dt: f32, id: u32) {
        let text: String = input.text.chars().filter(|c| !c.is_control()).collect();
        if !text.is_empty() {
            self.displays.text(id, &text);
        }
        for (k, name) in [
            (KeyCode::Enter, "enter"),
            (KeyCode::NumpadEnter, "enter"),
            (KeyCode::Tab, "tab"),
            (KeyCode::Home, "home"),
            (KeyCode::End, "end"),
        ] {
            if input.pressed(k) {
                self.displays.key(id, name);
            }
        }
        for (k, name) in [
            (KeyCode::Backspace, "back"),
            (KeyCode::Delete, "delete"),
            (KeyCode::ArrowLeft, "left"),
            (KeyCode::ArrowRight, "right"),
            (KeyCode::ArrowUp, "up"),
            (KeyCode::ArrowDown, "down"),
        ] {
            if crate::screen::key_repeat(&mut self.screen_repeat, input, k, dt) {
                self.displays.key(id, name);
            }
        }
        // Ctrl + lettre (Alt Gr, qui tape « @ », « € »…, n'est pas un raccourci).
        let ctrl = (input.held(KeyCode::ControlLeft) || input.held(KeyCode::ControlRight))
            && !input.held(KeyCode::AltRight);
        if ctrl {
            for k in input.pressed_keys() {
                let Some(label) = input.key_labels.get(&k) else {
                    continue;
                };
                let l = label.to_lowercase();
                if matches!(l.as_str(), "a" | "c" | "v" | "x" | "y" | "z") {
                    self.displays.key(id, &format!("ctrl+{l}"));
                }
            }
        }
    }

    /// Menu pause ouvert (Échap) : en solo, les vidéos des écrans se mettent en pause
    /// (elles ne repartent pas toutes seules à la reprise). En ligne, le monde continue :
    /// rien ne change (une pause partirait chez les autres, voir `crate::watch`). (v179)
    /// Seulement si l'option « Couper le son des écrans dans le menu » est choisie.
    pub fn pause_videos(&mut self) {
        if !self.online() {
            self.displays.pause_all();
        }
    }

    /// (v179) Partie partagée avec d'autres (construction ou FPS en ligne) : le monde
    /// continue pendant le menu.
    pub fn online(&self) -> bool {
        self.build.is_some() || self.online_match()
    }

    /// Le clavier va-t-il dans la page d'un écran (un champ de texte de l'écran visé a le
    /// focus) ? Le jeu ne lit alors plus ses touches.
    pub fn keyboard_on_screen(&self) -> bool {
        self.screen_aim
            .filter(|a| a.2[0] >= 0.0)
            .and_then(|a| self.shown.get(a.0))
            .is_some_and(|s| self.displays.editing(s.0.id()))
    }

    /// Échap pendant qu'on écrit dans la page d'un écran : le champ de texte perd le focus
    /// et le clavier revient au jeu.
    pub fn screen_escape(&mut self) {
        let id = self
            .screen_aim
            .and_then(|a| self.shown.get(a.0))
            .map(|s| s.0.id());
        if let Some(id) = id {
            self.displays.blur(id);
        }
    }

    /// Images par seconde et pages ouvertes des écrans selon ce que voit le joueur (voir
    /// `Displays::schedule`) : `looked`, l'écran sous le réticule.
    fn schedule_screens(&mut self, looked: Option<u32>) {
        let eye = self.player.eye();
        // (v215) Vu dès qu'un bout de sa hitbox est dans l'image, de face comme de dos
        // (avant : vu de face seulement, et le centre devant soi dans un cône large ; vu de
        // dos, l'écran passait à 2 images par seconde). Avec la caméra de l'image (F5,
        // caméra libre de l'éditeur) et son champ de vision.
        let (cam, view, _) = self.view_camera();
        let fov = if self.game.mode == GameMode::Fps {
            self.fps.scope_fov().unwrap_or(self.fov)
        } else {
            self.fov
        };
        let aspect = (self.screen_size.0 / self.screen_size.1.max(1.0)).max(0.1);
        let view_proj = Mat4::perspective_rh(fov.to_radians(), aspect, 0.05, 4096.0) * view;
        let seen: Vec<crate::screen::Seen> = self
            .shown
            .iter()
            .map(|(s, _)| {
                let a = s.anchor();
                let to = Vec3::from(a.center) - eye;
                let dist = to.length();
                let (mn, mx) = s.hitbox();
                let visible =
                    (Vec3::from(a.center) - cam).length() < 1.5 || box_in_view(view_proj, mn, mx);
                crate::screen::Seen {
                    id: s.id(),
                    dist,
                    visible,
                }
            })
            .collect();
        self.displays.schedule(&seen, looked);
    }

    /// Écran visé par le réticule jusqu'à `range` (le plus proche, s'il n'est pas caché
    /// par un bloc) : indice, distance et point visé dessus (0..1). (v220) Sa hitbox se
    /// touche aussi par derrière ou par le côté : point visé (−1, −1), on peut le casser ou
    /// le copier, pas cliquer dans sa page.
    fn aimed_screen(&self, range: f32) -> Option<(usize, f32, [f32; 2])> {
        let eye = self.aim_eye();
        let dir = self.player.look_dir();
        // Jusqu'au premier bloc visé, un rien au-delà : l'écran est collé à son mur.
        let seen = minelib_world::raycast::raycast(&self.world, eye.into(), dir.into(), range);
        let limit = seen.map_or(range, |h| h.distance + 0.1);
        let mut best: Option<(usize, f32, [f32; 2])> = None;
        for (i, (s, _)) in self.shown.iter().enumerate() {
            let hit = crate::screen::aim(&s.anchor(), eye, dir, limit).or_else(|| {
                let (min, max) = s.hitbox();
                crate::screen::aim_box(min, max, eye, dir, limit).map(|t| (t, [-1.0, -1.0]))
            });
            if let Some((t, uv)) = hit {
                if best.is_none_or(|b| t < b.1) {
                    best = Some((i, t, uv));
                }
            }
        }
        best
    }

    /// Pose l'écran vidéo tenu, de taille `size`, sur la face visée `hit` (clic droit).
    /// Refusé s'il serait dans le joueur ou s'il y en a déjà un (le sien ou celui d'un
    /// autre).
    fn place_screen(&mut self, hit: minelib_world::raycast::Hit, size: u8) {
        let s = crate::screen::Placed::new(hit.block, hit.normal, self.player.eye(), size);
        let (min, max) = s.hitbox();
        let (pmin, pmax) = self.player.bounds();
        let inside = (0..3).all(|k| pmin[k] < max[k] && pmax[k] > min[k]);
        // (Ni sur un des siens, ni sur celui d'un autre joueur.)
        let taken = self.shown.iter().any(|x| x.0.id() == s.id());
        if inside || taken || self.screens.len() >= crate::screen::MAX_SCREENS {
            return;
        }
        if self.game.mode != GameMode::Creative {
            self.inventory.take_selected();
        }
        self.screens.push(s);
        self.screens_changed(true);
    }

    /// Casse l'écran `i` (un des siens, les premiers de `shown`) : l'objet revient dans
    /// l'inventaire (hors créatif). (v281) Il tombe au sol, de son milieu, comme un bloc cassé
    /// (`drop_broken`).
    fn break_screen(&mut self, i: usize) {
        if i >= self.screens.len() {
            return;
        }
        let screen = self.screens.remove(i);
        self.screen_aim = None;
        if self.game.mode != GameMode::Creative {
            let at = Vec3::from(screen.anchor().center);
            let item = Item::Screen(screen.size);
            drop_broken(&self.world, &mut self.inventory, &mut self.items, at, item, 1);
        }
        self.screens_changed(true);
    }


    /// Ses écrans ont changé : hitbox pour la physique du joueur (refaite aussi à chaque
    /// image), enregistrement, et les joueurs proches le savent tout de suite.
    fn screens_changed(&mut self, save: bool) {
        self.player.solids = self.screens.iter().map(|s| s.hitbox()).collect();
        if save {
            crate::screen::save(&self.dir, &self.screens, self.world.origin_blocks());
        }
        self.party.screens_changed();
    }

    /// Cadre (carte importée) que le joueur vise, devant tout bloc visible.
    fn aimed_frame(&self) -> Option<usize> {
        let frames = self.frames.as_ref().filter(|f| !f.is_empty())?;
        let eye = self.aim_eye();
        let dir = self.player.look_dir();
        // Jusqu'au premier bloc visible : une barrière invisible (la collision que la
        // carte met autour du cadre) ne le protège pas.
        let seen = minelib_world::raycast::raycast_with(
            &self.world,
            eye.into(),
            dir.into(),
            REACH,
            |id| {
                let d = id.def();
                d.target && !matches!(d.shape, minelib_world::registry::Shape::Invisible)
            },
        );
        frames.hit(eye, dir, REACH, seen.map(|h| (h.block, h.distance + 0.01)))
    }

    /// Objets au sol et coffres (construction) : lâcher l'objet en main, ramasser ce qui
    /// traîne, écran du coffre, et échanges avec l'arbitre (le relais en ligne, ce jeu
    /// en solo). Voir `crate::drops` et `minelib_net::items`.
    fn update_items(&mut self, input: &Input, typing: bool) {
        use crate::drops::Items;
        use crate::inventory::{ChestAction, Stack, CHEST_SLOTS};
        use minelib_net::items::Pile;
        let online = self.build.is_some();
        // Première image, ou passage du solo au serveur : on repart de la bonne source.
        if !self.items_ready || online == self.items.is_local() {
            self.items = Items::new(!online);
            self.items_ready = true;
            self.items_seeded = false;
            if !online && self.net.is_none() {
                // (v309) Comptés depuis l'origine de la partie (les trop lointains restent
                // garés, voir `drops::PARK`).
                let o = self.world.origin_blocks();
                let saved = Items::load_saved(&self.dir, self.items_origin);
                if self.items.restore_saved(saved, [o[0], o[2]]) {
                    self.items.dirty = true;
                }
                // (v241) La Foudre de Zeus est unique : une carte d'avant peut en avoir des
                // piles de 64, au sol et dans des coffres aussi. Il n'en reste qu'une, celle
                // de l'inventaire d'abord. Aucune nulle part : elle peut de nouveau être
                // prise (rien ne la supprime).
                let mine = self.inventory.keep_one_zeus(true);
                let elsewhere = self.items.keep_one_legend(!mine);
                if mine || elsewhere {
                    self.legend.taken = true;
                } else {
                    self.legend = crate::zeus::Legend::default();
                }
            }
        }
        // Propriétaire d'un monde en ligne : sa sauvegarde est remise au relais, une fois.
        if let Some(b) = &self.build {
            if b.is_owner() && !self.items_seeded {
                self.items_seeded = true;
                // (v309) En ligne, tout est compté depuis le centre du monde.
                b.seed_items(&Items::saved_at(&self.dir, self.items_origin, [0, 0]));
            }
        }
        let me = self
            .build
            .as_ref()
            .map_or_else(|| "moi".to_string(), |b| b.pseudo().to_string());
        // Réponses et états publiés par le relais.
        if let Some(b) = self.build.as_mut() {
            // (v318) Comptés depuis l'origine de la partie (`BuildNet::take_items`).
            for m in b.take_items() {
                // (v224) Les lumières de l'éditeur arrivent par le même chemin : pour
                // `update_light_sync`.
                if matches!(
                    m,
                    minelib_net::game::BuildMsg::Light { .. }
                        | minelib_net::game::BuildMsg::LightDenied { .. }
                ) {
                    self.light_inbox.push(m);
                    continue;
                }
                // L'objet part de la main de celui qui l'a lâché, si on le voit.
                let thrower = match &m {
                    minelib_net::game::BuildMsg::Item { to, .. } => {
                        let key: String = to.chars().take(16).collect();
                        b.others.get(&key).map(|o| o.pos + Vec3::Y * 1.3)
                    }
                    _ => None,
                };
                self.items.hear(&m, &me, thrower);
            }
        }
        let playing = self.game.mode.has_inventory() && self.game.mode != GameMode::Fps;
        if playing {
            // Lâcher l'objet en main (Ctrl : toute la pile), comme dans Minecraft. (v222)
            // Lancé selon le regard, comme les armes en FPS : vers le haut, en cloche et plus
            // loin ; vers le bas, au sol devant ses pieds.
            if input.grabbed
                && !typing
                && !self.inventory.open
                && self.keys.pressed(input, Action::Drop)
            {
                // (v247) « Maj + lâcher : toute la pile d'un coup », comme Ctrl.
                let all = input.held(KeyCode::ControlLeft)
                    || input.held(KeyCode::ControlRight)
                    || input.held(KeyCode::ShiftLeft)
                    || input.held(KeyCode::ShiftRight);
                let n = if all { crate::inventory::MAX_STACK } else { 1 };
                // (v241) La Foudre de Zeus ne se jette pas dans le vide (rien ne la
                // supprime) : sans sol où tomber, elle reste en main.
                let path = crate::equip::toss_path(
                    &self.world,
                    self.player.eye(),
                    self.player.look_dir(),
                );
                let lost = path.last().is_none_or(|p| p.y < 1.0);
                let zeus = self.legend_on() && self.inventory.selected_item() == Some(Item::Zeus);
                if zeus && self.build.is_some() {
                    // (v244) Sur un serveur, elle reste avec celui qui l'a prise (le relais
                    // refuserait de la poser).
                    self.chat.say(crate::inventory::LEGEND_BOUND, chat::GREY);
                } else if lost && zeus {
                    self.chat.say(
                        "La Foudre de Zeus ne peut pas être jetée dans le vide.",
                        chat::GREY,
                    );
                } else if let Some(st) = self.inventory.take_from(self.inventory.selected, n) {
                    let pile = Pile {
                        item: st.item.code(),
                        count: st.count.min(64) as u8,
                    };
                    self.items.throw_item(path, pile);
                    // (v247) « Mets un son quand on lâche les objets aussi. »
                    crate::audio::toss();
                }
            }
            // Ramasser ce qui est à portée, s'il y a de la place ; (v220) pas avant d'être
            // arrivé.
            let inv = &self.inventory;
            if self.loading.is_none() {
                self.items.pick_up(self.player.pos, |p| {
                    crate::inventory::Item::from_code(p.item)
                        .is_some_and(|item| inv.has_room(item, p.count as u32))
                });
            }
        }
        // Écran du coffre ouvert.
        if let Some(pos) = self.inventory.chest {
            let here = Vec3::new(
                pos[0] as f32 + 0.5,
                pos[1] as f32 + 0.5,
                pos[2] as f32 + 0.5,
            );
            let still = self.world.get(pos[0], pos[1], pos[2]).def().key == "chest"
                && here.distance(self.player.eye()) < REACH + 2.0
                && playing;
            if !still {
                self.inventory.close();
            } else {
                let chest = self.items.view.chest(pos);
                let shown = |p: Pile| {
                    crate::inventory::Item::from_code(p.item).map(|item| Stack {
                        item,
                        count: p.count as u32,
                    })
                };
                for (i, slot) in chest.iter().enumerate().take(CHEST_SLOTS) {
                    self.inventory.chest_slots[i] = slot.and_then(shown);
                }
                match self.inventory.chest_action.take() {
                    // Prendre : versé à l'inventaire quand l'arbitre répond.
                    Some(ChestAction::Take { slot, all }) => {
                        if let Some(p) = chest[slot] {
                            let n = if all { p.count } else { 1 };
                            let fits = shown(p)
                                .is_some_and(|st| self.inventory.has_room(st.item, n as u32));
                            if fits {
                                self.items.chest_take(pos, slot as u8, n);
                            }
                        }
                    }
                    // Poser : la pile quitte l'inventaire tout de suite (rendue si le
                    // coffre la refuse), sur une pile du même objet ou une case libre.
                    Some(ChestAction::Put { slot, all }) => {
                        // (v244) Sur un serveur, la Foudre de Zeus ne se range pas.
                        let bound = self.build.is_some()
                            && self.legend_on()
                            && self.inventory.slots[slot].is_some_and(|s| s.item == Item::Zeus);
                        if bound {
                            self.chat.say(crate::inventory::LEGEND_BOUND, chat::GREY);
                        } else if let Some(st) = self.inventory.slots[slot] {
                            let n = if all { st.count.min(64) } else { 1 };
                            let code = st.item.code();
                            let target = (0..CHEST_SLOTS)
                                .find(|&i| {
                                    chest[i]
                                        .is_some_and(|p| p.item == code && p.count as u32 + n <= 64)
                                })
                                .or_else(|| (0..CHEST_SLOTS).find(|&i| chest[i].is_none()));
                            if let Some(t) = target {
                                if let Some(out) = self.inventory.take_from(slot, n) {
                                    let pile = Pile {
                                        item: code,
                                        count: out.count as u8,
                                    };
                                    self.items.chest_put(pos, t as u8, pile);
                                }
                            }
                        }
                    }
                    None => {}
                }
            }
        }
        // Solo : un coffre garni dont le bloc a disparu (cassé, remplacé) se vide au sol.
        if self.items.is_local() {
            let gone: Vec<[i32; 3]> = self
                .items
                .view
                .chests
                .keys()
                .copied()
                .filter(|p| {
                    self.world.contains(ChunkPos::containing(p[0], p[2]))
                        && self.world.get(p[0], p[1], p[2]).def().key != "chest"
                })
                .collect();
            for pos in gone {
                self.items.block_changed(pos, &me);
            }
        }
        self.items.arbitrate(&me);
        if let Some(b) = &mut self.build {
            for (chunk, req) in std::mem::take(&mut self.items.outbox) {
                b.send_item(chunk, &req);
            }
        }
        // (v219) Objet ramassé au sol : un « pop », plus ou moins aigu.
        if std::mem::take(&mut self.items.picked) > 0 {
            crate::audio::pickup();
        }
        // Ce que l'arbitre accorde entre dans l'inventaire ; ce qui n'y tient plus retombe
        // aux pieds du joueur.
        for p in std::mem::take(&mut self.items.credits) {
            let Some(item) = crate::inventory::Item::from_code(p.item) else {
                continue;
            };
            let left = self.inventory.add(item, p.count as u32);
            if left > 0 {
                let feet = self.player.pos;
                let pile = Pile {
                    item: p.item,
                    count: left.min(64) as u8,
                };
                self.items.drop_item(feet + Vec3::Y, feet, pile);
            }
        }
    }

    /// (v226) PNJ de la foule d'essai visé à portée de coup, et sa distance. (v227) Le plus
    /// proche, dans la nôtre ou dans celle d'un autre OP.
    fn npc_aimed(&self) -> Option<(NpcTarget, f32)> {
        let (eye, dir) = (self.player.eye(), self.player.look_dir());
        let mut best = self
            .crowd
            .as_ref()
            .and_then(|c| c.aimed(eye, dir, NPC_REACH))
            .map(|(i, t)| (NpcTarget::Mine(i), t));
        for (owner, rc) in &self.remote_crowds {
            if let Some((i, t)) = rc.aimed(eye, dir, NPC_REACH) {
                if best.as_ref().is_none_or(|b| t < b.1) {
                    best = Some((NpcTarget::Theirs(owner.clone(), i), t));
                }
            }
        }
        best
    }

    /// (v226) Coup à un PNJ de la foule d'essai : il recule et crie (un des cris de douleur,
    /// plus ou moins aigu à chaque fois). Si c'est le personnage caché : un petit air (jamais
    /// tout à fait le même) et un message ; il est reparti se cacher. (v227) PNJ de la foule
    /// d'un autre OP : il recule tout de suite ici, le coup part au jeu de l'OP (qui dit,
    /// pour le personnage caché, qui l'a trouvé) ; dans la nôtre menée en ligne, les joueurs
    /// proches sauront que nous l'avons trouvé.
    fn hit_npc(&mut self, target: NpcTarget) {
        self.hit_npc_for(target, crate::crowd::PUNCH);
    }

    /// (v241) Le même coup, qui ôte `amount` points de vie au PNJ de notre foule (l'arc court
    /// de la Foudre de Zeus : `zeus::ARC_DAMAGE`). (Celui de la foule d'un autre OP : son
    /// jeu en décide.)
    fn hit_npc_for(&mut self, target: NpcTarget, amount: f32) {
        let (eye, center) = (self.player.eye(), self.player.pos);
        match target {
            NpcTarget::Mine(i) => {
                let Some(c) = self.crowd.as_mut() else {
                    return;
                };
                let at = c.npcs[i].pos;
                match c.hit_for(&self.world, i, eye, center, amount) {
                    crate::crowd::Hit::Npc => {
                        self.fps.sounds.push((crate::audio::Sfx::Pain, 0.0, 0.55));
                    }
                    // (v238) Ce coup l'a tué : un cri plus fort ; il est reparti d'ailleurs.
                    crate::crowd::Hit::Killed => {
                        self.fps.sounds.push((crate::audio::Sfx::Pain, 0.0, 0.85));
                    }
                    crate::crowd::Hit::Hidden => {
                        if let (Some(out), Some(b)) = (self.crowd_out.as_mut(), &self.build) {
                            out.found = Some((b.pseudo().to_string(), at.to_array()));
                        }
                        self.say_found();
                    }
                }
            }
            NpcTarget::Theirs(owner, i) => {
                let (Some(rc), Some(b)) = (self.remote_crowds.get_mut(&owner), &self.build) else {
                    return;
                };
                let Some(id) = rc.id_of(i) else {
                    return;
                };
                rc.hit(i, eye);
                b.send_crowd_hit(&owner, id, eye.to_array());
                if !rc.npcs[i].hidden {
                    self.fps.sounds.push((crate::audio::Sfx::Pain, 0.0, 0.55));
                }
            }
        }
    }

    /// (v226) Nous avons trouvé le personnage caché : son petit air, et le message.
    fn say_found(&mut self) {
        crate::audio::found();
        self.chat.say(
            format!(
                "Tu as trouvé {} ! Il est reparti se cacher dans la foule.",
                crate::crowd::HIDDEN_NAME
            ),
            chat::YELLOW,
        );
    }

    /// (v227) Foules d'essai en construction en ligne : la nôtre (OP), dite aux joueurs proches
    /// avec les coups qu'ils donnent à ses PNJ ; celles des autres OP, reçues, qui avancent
    /// ici entre deux nouvelles. Le personnage caché trouvé : celui qui l'a trouvé et les
    /// joueurs à moins de `FOUND_NEAR` blocs de là le savent.
    fn update_shared_crowds(&mut self, dt: f32) {
        if self.crowd_out.is_none() {
            // (Seul le jeu qui mène la foule en ligne dit ce qui y change.)
            if let Some(c) = self.crowd.as_mut() {
                c.changed.clear();
            }
        }
        let Some(b) = self.build.as_mut() else {
            self.remote_crowds.clear();
            self.crowd_out = None;
            self.crowd_probe = None;
            return;
        };
        if std::mem::take(&mut b.crowd_denied) && self.crowd_out.take().is_some() {
            self.crowd_on = false;
            self.crowd = None;
            self.chat.say(
                "Essai de foule arrêté : réservé aux OP de ce serveur.",
                chat::RED,
            );
        }
        let me = self.player.pos;
        let hits = b.take_crowd_hits();
        if let (Some(c), Some(out)) = (self.crowd.as_mut(), self.crowd_out.as_mut()) {
            for (by, id, from) in hits {
                let i = id as usize;
                let from = Vec3::from(from);
                // Un coup venu de trop loin (ou d'un PNJ qui n'est plus là) : ignoré.
                let Some(at) = c.npcs.get(i).map(|n| n.pos) else {
                    continue;
                };
                if !from.is_finite() || at.distance(from) > NPC_REACH + 4.0 {
                    continue;
                }
                match c.hit(&self.world, i, from, me) {
                    crate::crowd::Hit::Hidden => {
                        out.found = Some((by.clone(), at.to_array()));
                        if at.distance(me) <= FOUND_NEAR {
                            self.chat.say(
                                format!("{by} a trouvé {} !", crate::crowd::HIDDEN_NAME),
                                chat::YELLOW,
                            );
                        }
                    }
                    crate::crowd::Hit::Npc | crate::crowd::Hit::Killed => {
                        if let Some(g) = npc_cry_gain(at.distance(me)) {
                            self.fps.sounds.push((crate::audio::Sfx::Pain, 0.0, g));
                        }
                    }
                }
            }
            if let Some(p) = out.take(c, me, dt) {
                b.send_crowd(minelib_net::crowd::pack(&p));
            }
        }
        for (owner, data) in b.take_crowds() {
            let Some(p) = minelib_net::crowd::unpack(&data) else {
                continue;
            };
            if p.stop {
                self.remote_crowds.remove(&owner);
                continue;
            }
            let rc = self.remote_crowds.entry(owner).or_default();
            for at in rc.apply(&self.world, &p) {
                if let Some(g) = npc_cry_gain(at.distance(me)) {
                    self.fps.sounds.push((crate::audio::Sfx::Pain, 0.0, g));
                }
            }
            if let Some((by, at)) = p.found {
                if by.eq_ignore_ascii_case(b.pseudo()) {
                    crate::audio::found();
                    self.chat.say(
                        format!(
                            "Tu as trouvé {} ! Il est reparti se cacher dans la foule.",
                            crate::crowd::HIDDEN_NAME
                        ),
                        chat::YELLOW,
                    );
                } else if Vec3::from(at.map(|v| v as f32)).distance(me) <= FOUND_NEAR {
                    self.chat.say(
                        format!("{by} a trouvé {} !", crate::crowd::HIDDEN_NAME),
                        chat::YELLOW,
                    );
                }
            }
        }
        for rc in self.remote_crowds.values_mut() {
            rc.update(&self.world, dt);
        }
        self.remote_crowds
            .retain(|_, rc| rc.heard < crate::crowd::REMOTE_TIMEOUT);
    }

    /// (v227) `/npc stresstest on` en construction en ligne, le relais nous a dit OP : la
    /// foule apparaît autour de nous, pour les joueurs proches aussi.
    fn start_crowd_online(&mut self) {
        self.crowd_on = true;
        self.crowd = None;
        self.crowd_out = Some(crate::crowd::Outbox::new());
        self.chat.say(
            format!(
                "Essai de foule : jusqu'à {} joueurs simulés autour de toi en 2 minutes, vus \
                 aussi par les joueurs proches. Un clic gauche les frappe ; {} se cache parmi \
                 eux.",
                crate::crowd::MAX,
                crate::crowd::HIDDEN_NAME
            ),
            chat::YELLOW,
        );
    }

    /// (v227) Arrête notre foule d'essai ; menée en ligne, les joueurs proches la voient
    /// partir.
    fn stop_crowd(&mut self) {
        if let (Some(_), Some(b)) = (self.crowd_out.take(), &self.build) {
            let p = crate::crowd::stop_packet(self.player.pos);
            b.send_crowd(minelib_net::crowd::pack(&p));
        }
        self.crowd_on = false;
        self.crowd = None;
    }

    fn handle_blocks(&mut self, input: &Input, dt: f32) {
        use crate::hand::Gesture;
        self.break_cooldown -= dt;
        self.place_cooldown -= dt;
        self.frame_aim = None;
        // (v220) Bloc en train d'être cassé (survie) : oublié si on ne le casse plus à cette
        // image (voir plus bas).
        let dig = self.dig.take();
        // (Essais : avec MINELIB_CLIC, sans la souris prise non plus.)
        let free = !input.grabbed && std::env::var_os("MINELIB_CLIC").is_none();
        if free || self.inventory.open {
            return;
        }
        // (v220) Clic gauche ou droit : la main frappe (arc), sauf si un bloc se casse (coup
        // vers le bas, plus bas).
        if self.game.mode != GameMode::Fps
            && self.game.mode != GameMode::Spectator
            && (self.keys.pressed(input, Action::Attack) || self.keys.pressed(input, Action::Use))
        {
            self.gesture = Gesture::Swing;
        }
        // Écran vidéo visé à portée (ou touche encore tenue après l'avoir touché ou cassé) :
        // ses touches sont pour lui (voir `update_screens`), rien n'est cassé ni posé
        // derrière. Accroupi, « utiliser » l'ignore : le bloc tenu est posé devant, comme
        // contre une porte ou un coffre.
        if self.screen_touch.is_some() || self.screen_broke {
            return;
        }
        if self.screen_aim.is_some_and(|a| a.1 <= REACH) {
            let place_through = self.game.mode != GameMode::Fps
                && self.keys.held(input, Action::Sneak)
                && !self.keys.held(input, Action::Attack);
            // (v247) La Foudre de Zeus en main : le clic gauche la lance, même sur un écran.
            // (v295) Un clic bref (appuyé et relâché dans la même image) aussi.
            let zeus_blow = self.game.mode != GameMode::Fps
                && zeus_click(
                    self.inventory.selected_item(),
                    self.keys.pressed(input, Action::Attack),
                    self.keys.held(input, Action::Attack),
                );
            if !place_through && !zeus_blow {
                return;
            }
        }
        let keys = &self.keys;
        let pick = if self.game.mode == GameMode::Fps {
            keys.pressed(input, Action::FpsPing)
        } else {
            keys.pressed(input, Action::Pick)
        };
        let door_key = self.game.mode == GameMode::Fps && keys.pressed(input, Action::FpsUse);
        // Spectateur : on regarde seulement.
        if self.game.mode == GameMode::Spectator {
            return;
        }
        // Mode FPS : ni cassé ni posé ; la touche « Copier / ping » marque l'endroit visé.
        if self.game.mode == GameMode::Fps {
            // Touche Utiliser sur une porte ou une trappe (si la carte le permet).
            if door_key {
                self.fps_door();
            }
            if pick {
                let eye = self.player.eye();
                let dir = self.player.look_dir();
                if let Some(h) = raycast(&self.world, eye.into(), dir.into(), PING_RANGE) {
                    if self.pings.len() >= MAX_PINGS {
                        self.pings.remove(0);
                    }
                    self.pings.push((eye + dir * h.distance, 0.0));
                }
            }
            return;
        }
        // Tout de suite (créatif, cadres, écrans) ; en survie, un bloc demande `BREAK_S` de
        // clic tenu (voir plus bas).
        let breaking = keys.pressed(input, Action::Attack)
            || (keys.held(input, Action::Attack) && self.break_cooldown <= 0.0);
        let placing = keys.pressed(input, Action::Use)
            || (keys.held(input, Action::Use) && self.place_cooldown <= 0.0);
        // (v278) Depuis la caméra (elle avance accroupi).
        let hit = raycast(
            &self.world,
            self.aim_eye().into(),
            self.player.look_dir().into(),
            REACH,
        );
        // (v226) PNJ de la foule d'essai visé avant le bloc : le clic le frappe ; clic tenu,
        // rien n'est cassé derrière lui.
        // (v235) Sauf avec la Foudre de Zeus en main : le clic lance la foudre, qui tombe sur
        // lui (plus bas).
        let clicked = test_click(dt);
        if clicked {
            // (Essais : le geste de la main part avec le clic simulé.)
            self.gesture = crate::hand::Gesture::Swing;
        }
        let attack = keys.pressed(input, Action::Attack) || clicked;
        let zeus = self.inventory.selected_item() == Some(Item::Zeus);
        // (v269) Un Évidé visé de près : le clic en mange un morceau, quel que soit l'objet tenu
        // (« les épées ne lui font rien : il faut le manger vivant, de près ») ; clic tenu,
        // rien n'est cassé derrière lui.
        if attack || keys.held(input, Action::Attack) {
            let aim = self
                .evides
                .aimed(self.player.eye(), self.player.look_dir(), crate::evide::EAT_REACH)
                .filter(|a| hit.as_ref().is_none_or(|b| a.1 <= b.distance));
            if let Some((id, _, part, cell)) = aim {
                // (v272) « On doit se voir se pencher pour manger le morceau qu'on vise si on
                // est assez près de lui » : la bouchée est menée image après image plus bas
                // (`eating`), le haut du corps plié vers ce morceau.
                // (v290) « Je n'arrive plus à manger le monstre […] que ça fonctionne quand je
                // clique » : le clic suffit, que notre bouche puisse y arriver ou non.
                if attack && self.eating.is_none() {
                    self.eating = Some((id, part, cell, 0.0, false));
                }
                self.punch_latch = true;
                return;
            }
        }
        if (attack || keys.held(input, Action::Attack)) && !zeus {
            let before_block = |t: f32| hit.as_ref().is_none_or(|b| t <= b.distance);
            if let Some((target, _)) = self.npc_aimed().filter(|(_, t)| before_block(*t)) {
                if attack {
                    self.hit_npc(target);
                    self.punch_latch = true;
                }
                return;
            }
        }
        // (v247) « Quand je tape Pistache, ça casse le bloc derrière » : frappé, le PNJ bondit
        // hors de la vue, et le clic encore tenu cassait le bloc qu'il cachait. Tant que le
        // clic qui l'a frappé reste tenu, rien n'est cassé.
        if !keys.held(input, Action::Attack) {
            self.punch_latch = false;
        }
        let breaking = breaking && !self.punch_latch;
        // Cadre accroché au mur (carte importée) devant le bloc visé : il se casse. (v247) Pas
        // avec la Foudre de Zeus en main : son clic lance la foudre.
        self.frame_aim = self.aimed_frame();
        if breaking && !zeus {
            if let (Some(i), Some(frames)) = (self.frame_aim.take(), self.frames.as_mut()) {
                let at = frames.pos(i);
                frames.remove(i, &self.dir);
                if let Some(p) = at {
                    self.set_probes(Some(ChunkPos::containing(p[0], p[2])));
                }
                self.break_cooldown = 0.22;
                return;
            }
        }
        // (v215) Lampe torche tenue : la touche « Copier le bloc » (clic molette) l'allume
        // ou l'éteint, même sans bloc visé ; elle ne copie rien tant qu'on la tient.
        if pick && self.inventory.selected_item() == Some(Item::Flashlight) {
            self.flashlight_on = !self.flashlight_on;
            // (v238) Son de l'interrupteur deux fois moins fort (0,6 avant).
            self.fps.sounds.push((crate::audio::Sfx::Dry, 0.0, FLASHLIGHT_CLICK));
            return;
        }
        // « Copier le bloc » sur une tête portée par une entité de la carte (porte-armure,
        // affichage, cadre) devant le bloc visé : la tête, avec son skin, vient en main.
        if pick && self.inventory.selected_item() != Some(Item::Wand) {
            let eye = self.player.eye();
            let aimed = self
                .heads
                .as_ref()
                .and_then(|h| h.aimed(eye, self.player.look_dir(), REACH))
                .filter(|(t, _)| hit.as_ref().is_none_or(|b| *t <= b.distance + 0.01));
            if let Some((_, skin)) = aimed {
                self.give_picked(head_item(skin));
                return;
            }
        }
        // « Copier le bloc » sur un cadre : le bloc qu'il montre vient en main.
        if pick && self.inventory.selected_item() != Some(Item::Wand) {
            let framed = self.frame_aim.and_then(|i| self.frames.as_ref()?.block(i));
            if let Some(b) = framed {
                self.give_picked(Item::Block(b));
                return;
            }
        }
        // Baguette : clic molette (touche « Copier le bloc ») efface la sélection, même
        // sans bloc visé.
        if self.inventory.selected_item() == Some(Item::Wand) && pick {
            if self.we.pos1.is_some() || self.we.pos2.is_some() {
                self.we.pos1 = None;
                self.we.pos2 = None;
                self.chat.say("Sélection effacée", chat::LIGHT_PURPLE);
            }
            return;
        }
        // (v216) Foudre de Zeus tenue : clic gauche, la foudre tombe là où l'on vise, comme
        // avec /thor (rien n'est cassé, même en créatif). (v295) « J'ai réussi à casser un
        // bloc avec l'éclair, ça doit être impossible » : un clic bref, appuyé et relâché dans
        // la même image (une image lente), n'était pas « tenu » : la foudre ne partait pas et
        // le clic cassait le bloc visé. Appuyé ou tenu, le clic est pour elle (`zeus_click`).
        if zeus_click(
            self.inventory.selected_item(),
            keys.pressed(input, Action::Attack),
            keys.held(input, Action::Attack),
        ) {
            // (v241) Objet légendaire : chargé ou non, le coup n'est pas le même.
            if self.legend_on() {
                if keys.pressed(input, Action::Attack) && self.storm.cooldown <= 0.0 {
                    self.zeus_blow();
                }
                return;
            }
            if keys.pressed(input, Action::Attack) && self.storm.cooldown <= 0.0 {
                // (v235) Un PNJ visé : la foudre tombe sur lui (`aimed`). (v238) Elle le blesse
                // en tombant, comme un joueur, et le fait sauter (`Crowd::struck`) : plus de
                // coup de poing en plus.
                if let Some(aim) = self.aimed() {
                    self.cast_lightning(aim);
                    self.storm.cooldown = crate::storm::ZEUS_COOLDOWN;
                }
            }
            return;
        }
        // (v281) Seau tenu : clic droit, il prend la source visée ou la repose (voir
        // `use_bucket`) ; le clic gauche casse comme d'habitude.
        if let Some(Item::Bucket(content)) = self.inventory.selected_item() {
            // (Une porte, une trappe ou un coffre visés s'ouvrent, comme avec autre chose en
            // main ; accroupi, le seau sert quand même.)
            let opens = hit
                .as_ref()
                .is_some_and(|h| hinged(h.id).is_some() || h.id.def().key == "chest")
                && !keys.held(input, Action::Sneak);
            if keys.pressed(input, Action::Use) && !opens {
                self.use_bucket(content);
                self.place_cooldown = 0.22;
                return;
            }
        }
        // (v215) Lampe torche tenue : le clic droit n'allume plus rien (c'est le clic
        // molette) ; il ouvre les portes, trappes et coffres comme d'habitude.
        let Some(hit) = hit else { return };

        // (v269) Œuf de L'Évidé tenu : clic droit, il sort sur la face visée (aucun bloc posé).
        if self.inventory.selected_item() == Some(Item::Egg) {
            if keys.pressed(input, Action::Use) {
                self.hatch_evide(hit);
                self.place_cooldown = 0.22;
                return;
            }
            if !breaking {
                return;
            }
        }

        // Écran vidéo tenu : clic droit le pose sur la face visée (aucun bloc posé).
        if let Some(Item::Screen(size)) = self.inventory.selected_item() {
            if keys.pressed(input, Action::Use) {
                self.place_screen(hit, size);
                self.place_cooldown = 0.22;
                return;
            }
            if !breaking {
                return;
            }
        }

        // Baguette de sélection : clic gauche coin 1, clic droit coin 2 (rien n'est cassé
        // ni posé).
        if self.inventory.selected_item() == Some(Item::Wand) {
            if keys.pressed(input, Action::Attack) {
                self.we.pos1 = Some(hit.block);
                if !self.snap_door_selection() {
                    self.say_selection(1, hit.block);
                }
            } else if keys.pressed(input, Action::Use) {
                self.we.pos2 = Some(hit.block);
                if !self.snap_door_selection() {
                    self.say_selection(2, hit.block);
                }
            }
            return;
        }

        // Clic droit sur une porte ou une trappe : elle s'ouvre ou se ferme (accroupi : on
        // pose le bloc tenu contre elle, comme dans Minecraft).
        if keys.pressed(input, Action::Use)
            && hinged(hit.id).is_some()
            && !keys.held(input, Action::Sneak)
        {
            let look = self.player.look_dir();
            let changes = toggle_hinged(&self.world, &mut self.opened, hit.block, look);
            if !changes.is_empty() {
                let sfx = crate::audio::Sfx::Door(door_opened(&changes));
                self.fps.sounds.push((sfx, 0.0, 0.9));
            }
            for (p, b) in changes {
                self.dirty.extend(self.world.set(p[0], p[1], p[2], b));
            }
            self.place_cooldown = 0.22;
            return;
        }
        // Clic droit sur un coffre : il s'ouvre (accroupi : on pose le bloc tenu contre lui).
        if keys.pressed(input, Action::Use)
            && hit.id.def().key == "chest"
            && !keys.held(input, Action::Sneak)
            && self.game.mode.has_inventory()
        {
            self.inventory.open_chest(hit.block);
            self.place_cooldown = 0.22;
            return;
        }
        // Clic droit sur un pot de fleurs : la plante tenue y est mise ; sur un pot garni,
        // la plante est enlevée (rendue hors créatif).
        if keys.pressed(input, Action::Use)
            && hit.id.def().key.starts_with("flower_pot")
            && !keys.held(input, Action::Sneak)
        {
            let reg = minelib_world::registry::registry();
            let [x, y, z] = hit.block;
            let change = match minelib_world::registry::potted_plant(hit.id) {
                Some(plant) => {
                    if self.game.mode != GameMode::Creative {
                        self.inventory.add(Item::Block(plant), 1);
                    }
                    reg.by_key("flower_pot")
                }
                None => match self.inventory.selected_item() {
                    Some(Item::Block(plant))
                        if minelib_world::registry::pottable(&plant.def().key) =>
                    {
                        let pot = reg.by_key(&format!("flower_pot:{}", plant.def().key));
                        if pot.is_some() && self.game.mode != GameMode::Creative {
                            self.inventory.take_selected();
                        }
                        pot
                    }
                    _ => None,
                },
            };
            if let Some(b) = change {
                self.dirty.extend(self.world.set(x, y, z, b));
                self.place_cooldown = 0.22;
                return;
            }
        }
        // En créatif, comme dans Minecraft : même la bedrock et les barrières se cassent.
        // (v295) Jamais avec la Foudre de Zeus en main (voir `zeus_click`).
        let breakable = (hit.id.is_breakable()
            || (self.game.mode == GameMode::Creative && hit.id.is_targetable()))
            && !zeus;
        // (v220) En créatif, le bloc part tout de suite, la main fait un petit coup vers le
        // bas. En survie, il faut tenir le clic `BREAK_S` sur le même bloc, et la main frappe
        // en boucle pendant ce temps (plus lentement, une pause entre deux coups).
        let breaking = if self.game.mode == GameMode::Creative {
            if breaking && breakable {
                self.gesture = Gesture::Dip;
            }
            breaking
        } else if keys.held(input, Action::Attack) && breakable && !self.punch_latch {
            let t = match dig {
                Some((b, t)) if b == hit.block => t + dt,
                _ => 0.0,
            };
            self.gesture = Gesture::Dig;
            if t >= BREAK_S {
                true
            } else {
                self.dig = Some((hit.block, t));
                false
            }
        } else {
            false
        };
        if breaking && breakable {
            self.opened.remove(&hit.block);
            // Porte : les deux moitiés partent ensemble.
            if let Some(Hinged::Door { upper, .. }) = hinged(hit.id) {
                let [x, y, z] = hit.block;
                let other = if upper { y - 1 } else { y + 1 };
                if matches!(
                    hinged(self.world.get(x, other, z)),
                    Some(Hinged::Door { .. })
                ) {
                    self.dirty.extend(self.world.set(x, other, z, BlockId::AIR));
                    self.opened.remove(&[x, other, z]);
                }
            }
            let [x, y, z] = hit.block;
            // Tête : elle part avec le bloc (et rend une tête avec son skin).
            let mut head = None;
            if minelib_world::registry::skull_pose(hit.id).is_some() {
                if let Some(h) = self.heads.as_mut() {
                    head = Some(head_item(h.skin_at(hit.block)));
                    h.remove_at(hit.block, &self.dir);
                }
            }
            // Une torche cassée rend une torche ordinaire : le sel restant est perdu.
            let item = if hit.id.is_torch() {
                Item::Torch(None)
            } else {
                head.unwrap_or(Item::Block(hit.id))
            };
            // (v283) Neige fine, tapis : la pile part entière et rend chacune de ses couches.
            let drops = layer_drops(&self.world, hit.block, hit.id).unwrap_or_else(|| vec![(item, 1)]);
            self.dirty.extend(self.world.set(x, y, z, BlockId::AIR));
            // En créatif, comme dans Minecraft : le bloc cassé ne s'ajoute pas à l'inventaire.
            // (v281) En survie, il tombe en objet au sol, de sa case (`drop_broken`).
            if self.game.mode != GameMode::Creative {
                let at = Vec3::new(x as f32 + 0.5, y as f32 + 0.5, z as f32 + 0.5);
                for (item, count) in drops {
                    drop_broken(&self.world, &mut self.inventory, &mut self.items, at, item, count);
                }
            }
            self.break_cooldown = 0.22;
        }

        if placing {
            // Tête : au sol tournée vers soi (16 directions, comme Minecraft), ou contre le
            // mur visé ; pas au plafond. Son skin est gardé avec elle.
            if let Some(skin) = self.inventory.selected_item().and_then(Item::head_skin) {
                let [x, y, z] = hit.adjacent();
                let free = !self.world.get(x, y, z).is_targetable()
                    && !self.player.overlaps_block([x, y, z]);
                let look = self.player.look_dir();
                let pose = match hit.normal {
                    [1, 0, 0] => Some(crate::heads::Pose::Wall(1)),
                    [-1, 0, 0] => Some(crate::heads::Pose::Wall(0)),
                    [0, 0, 1] => Some(crate::heads::Pose::Wall(5)),
                    [0, 0, -1] => Some(crate::heads::Pose::Wall(4)),
                    [0, 1, 0] => Some(crate::heads::Pose::Floor(floor_rotation(look))),
                    _ => None,
                };
                if let (Some(pose), true) = (pose, free) {
                    let block = match pose {
                        crate::heads::Pose::Wall(s) => {
                            minelib_world::registry::skull_block(true, s)
                        }
                        crate::heads::Pose::Floor(r) => {
                            minelib_world::registry::skull_block(false, r)
                        }
                        _ => BlockId::AIR,
                    };
                    if block != BlockId::AIR {
                        self.dirty.extend(self.world.set(x, y, z, block));
                        if let Some(h) = self.heads.as_mut() {
                            h.place([x, y, z], pose, skin, &self.dir);
                        }
                        self.use_selected();
                    }
                }
                self.place_cooldown = 0.22;
                return;
            }
            // Portes (2 blocs, au bord le plus proche du joueur) et trappes (en haut ou en
            // bas de la case selon l'endroit visé).
            if let Some(Item::Block(b)) = self.inventory.selected_item() {
                let point = self.player.eye() + self.player.look_dir() * hit.distance;
                if let Some(cells) = hinged_placement(&hit, b, self.player.look_dir(), point) {
                    let free = cells.iter().all(|(p, _)| {
                        !self.world.get(p[0], p[1], p[2]).is_targetable()
                            && !self.player.overlaps_block(*p)
                    });
                    if free {
                        for (p, id) in cells {
                            self.dirty.extend(self.world.set(p[0], p[1], p[2], id));
                        }
                        self.use_selected();
                    }
                    self.place_cooldown = 0.22;
                    return;
                }
            }
            // Escaliers : le bas tourné vers soi ; à l'envers au plafond ou sur la moitié
            // haute d'un mur (comme dans Minecraft).
            if let Some(Item::Block(b)) = self.inventory.selected_item() {
                let point = self.player.eye() + self.player.look_dir() * hit.distance;
                if let Some(id) = stairs_placement(&hit, b, self.player.look_dir(), point) {
                    let [x, y, z] = hit.adjacent();
                    if !self.world.get(x, y, z).is_targetable()
                        && !self.player.overlaps_block([x, y, z])
                    {
                        self.dirty.extend(self.world.set(x, y, z, id));
                        self.use_selected();
                    }
                    self.place_cooldown = 0.22;
                    return;
                }
            }
            // (v283) Neige fine, tapis : sur une pile de la même matière, une couche de plus
            // (« elle s'empile ») ; les tapis gardent la couleur de chacune.
            if let Some(Item::Block(b)) = self.inventory.selected_item() {
                if let Some(r) = layer_placement(&self.world, &hit, b) {
                    if let Some((p, id, layers)) = r {
                        if !self.player.overlaps_block(p) || !id.is_solid() {
                            self.dirty.extend(self.world.set(p[0], p[1], p[2], id));
                            if let Some(l) = layers {
                                self.world.set_carpet_layers(p, &l);
                                // (v323) En construction en ligne, les autres joueurs voient
                                // aussi la couleur de chaque couche (le bloc ne dit que celle
                                // du dessus) : à l'arbitre de son chunk, après le bloc.
                                if let Some(b) = self.build.as_mut() {
                                    b.send_carpet(p, l);
                                }
                            }
                            self.use_selected();
                        }
                    }
                    self.place_cooldown = 0.22;
                    return;
                }
            }
            // Dalles : orientation selon la face visée, et deux dalles du même matériau dans
            // une case font un bloc complet (comme dans Minecraft).
            if let Some(Item::Block(b)) = self.inventory.selected_item() {
                let sneaking = keys.held(input, Action::Sneak);
                let point = self.player.eye() + self.player.look_dir() * hit.distance;
                if let Some((p, id)) = slab_placement(&self.world, &hit, b, point, sneaking) {
                    let changes = self.world.get(p[0], p[1], p[2]) != id;
                    if changes && (!self.player.overlaps_block(p) || !id.is_solid()) {
                        self.dirty.extend(self.world.set(p[0], p[1], p[2], id));
                        self.use_selected();
                    }
                    self.place_cooldown = 0.22;
                    return;
                }
            }
            // (v283) Une seule couche de neige : le bloc posé dessus la remplace.
            let thin_snow = minelib_world::registry::Layers::of(hit.id)
                == Some(minelib_world::registry::Layers::Snow(1));
            let [x, y, z] = if thin_snow { hit.block } else { hit.adjacent() };
            let free = thin_snow || !self.world.get(x, y, z).is_targetable();
            // Bûche contre un mur : couchée, dans l'axe de la face visée (comme Minecraft).
            if let Some(Item::Block(b)) = self.inventory.selected_item() {
                let key = &b.def().key;
                if key.starts_with("log_") && hit.normal[1] == 0 {
                    let base = key.split(':').next().unwrap_or(key);
                    let axis = if hit.normal[0] != 0 { "x" } else { "z" };
                    let reg = minelib_world::registry::registry();
                    if let Some(id) = reg.by_key(&format!("{base}:{axis}")) {
                        if free && !self.player.overlaps_block([x, y, z]) {
                            self.dirty.extend(self.world.set(x, y, z, id));
                            self.use_selected();
                        }
                        self.place_cooldown = 0.22;
                        return;
                    }
                }
            }
            // Bannière, échelle, liane : toujours contre le mur visé (posée sur le sol ou au
            // plafond : contre le côté de la case opposé au joueur).
            if let Some(Item::Block(b)) = self.inventory.selected_item() {
                // Échelle, liane : pareil, contre le mur visé.
                let key = &b.def().key;
                let family = if key.starts_with("ladder:") {
                    Some("ladder".to_string())
                } else if key.starts_with("vine:") {
                    Some("vine".to_string())
                } else if key.starts_with("tripwire_hook") {
                    // (v223) Crochet (fil de déclenchement) : accroché au mur cliqué ; il ne
                    // se pose ni sur le sol ni au plafond.
                    if hit.normal[1] != 0 {
                        self.place_cooldown = 0.22;
                        return;
                    }
                    Some("tripwire_hook".to_string())
                } else {
                    key.strip_prefix("banner_")
                        .and_then(|r| r.split(':').next())
                        .map(|c| format!("banner_{c}"))
                };
                if let Some(family) = family {
                    let side = match hit.normal {
                        [-1, 0, 0] => 0,
                        [1, 0, 0] => 1,
                        [0, 0, -1] => 4,
                        [0, 0, 1] => 5,
                        _ => side_toward(self.player.look_dir()),
                    };
                    let reg = minelib_world::registry::registry();
                    if let Some(id) = reg.by_key(&format!("{family}:{side}")) {
                        if free && !self.player.overlaps_block([x, y, z]) {
                            self.dirty.extend(self.world.set(x, y, z, id));
                            self.use_selected();
                        }
                    }
                    self.place_cooldown = 0.22;
                    return;
                }
            }
            match self.inventory.selected_item() {
                Some(Item::Block(b)) if free && !self.player.overlaps_block([x, y, z]) => {
                    // Lanterne posée sous un bloc visible (clic sur son dessous), ou sans
                    // rien dessous mais avec un bloc au-dessus : suspendue.
                    let b = {
                        let key = &b.def().key;
                        let lantern = key == "lantern" || key == "soul_lantern";
                        let held = |dy: i32| {
                            let d = self.world.get(x, y + dy, z).def();
                            use minelib_world::registry::Shape;
                            // (Un bloc invisible ne retient rien.)
                            d.target && !matches!(d.shape, Shape::Cross | Shape::Invisible)
                        };
                        if lantern && held(1) && (hit.normal == [0, -1, 0] || !held(-1)) {
                            minelib_world::registry::registry()
                                .by_key(&format!("{key}:hanging"))
                                .unwrap_or(b)
                        } else {
                            b
                        }
                    };
                    self.dirty.extend(self.world.set(x, y, z, b));
                    self.use_selected();
                }
                // Torche : au sol sur le dessus d'un bloc, murale contre un côté, pas au plafond.
                Some(Item::Torch(chem)) if free && hit.normal[1] >= 0 => {
                    let block = match hit.normal {
                        [-1, 0, 0] => BlockId::wall_torch(0),
                        [1, 0, 0] => BlockId::wall_torch(1),
                        [0, 0, -1] => BlockId::wall_torch(4),
                        [0, 0, 1] => BlockId::wall_torch(5),
                        _ => BlockId::TORCH,
                    };
                    self.dirty
                        .extend(self.world.place_torch_block(x, y, z, block, chem));
                    // En créatif, une torche colorée ne s'épuise jamais.
                    if self.game.mode == GameMode::Creative && chem.is_some() {
                        self.world.set_torch_infinite([x, y, z]);
                    }
                    self.use_selected();
                }
                _ => {}
            }
            self.place_cooldown = 0.22;
        }
        // Copier le bloc visé : il passe dans la main. En créatif il est créé s'il n'est pas
        // dans la barre ; en survie on ne fait que choisir un emplacement qui l'a déjà.
        if pick {
            let wanted = if hit.id.is_torch() {
                Item::Torch(self.world.torch(hit.block).and_then(|t| t.chemical))
            } else if minelib_world::registry::skull_pose(hit.id).is_some() {
                // Tête : avec son skin (texture de Minecraft, joueur), à reposer ailleurs.
                head_item(
                    self.heads
                        .as_ref()
                        .map_or(crate::heads::HeadSkin::Default, |h| h.skin_at(hit.block)),
                )
            } else if let Some(l) = minelib_world::registry::Layers::of(hit.id) {
                // (v283) Neige fine : une couche ; pile de tapis : un tapis de la sorte du dessus.
                use minelib_world::registry::Layers;
                let one = match l {
                    Layers::Snow(_) => Layers::Snow(1),
                    Layers::Carpet { kind, .. } => Layers::Carpet { n: 1, kind },
                };
                Item::Block(one.block().unwrap_or(hit.id))
            } else {
                // (v281) Une dalle ou un escalier retiré (déjà posé) : son bloc plein.
                Item::Block(minelib_world::registry::retired_shape_base(hit.id).unwrap_or(hit.id))
            };
            self.give_picked(wanted);
        }
    }

    /// Points de mesure de la lumière (têtes d'entités, puis cadres) donnés au monde, chunk
    /// par chunk ; `only` : ce chunk seulement (un cadre cassé). Les chunks concernés sont
    /// remaillés : leur maillage mesure la lumière à ces points.
    fn set_probes(&mut self, only: Option<ChunkPos>) {
        let mut by: HashMap<ChunkPos, (Vec<[i32; 3]>, usize)> = HashMap::new();
        if let Some(h) = &self.heads {
            for (pos, pts) in h.probes() {
                let e = by.entry(pos).or_default();
                e.1 = pts.len();
                e.0.extend(pts);
            }
        }
        if let Some(f) = &self.frames {
            for (pos, pts) in f.probes() {
                by.entry(pos).or_default().0.extend(pts);
            }
        }
        if let Some(o) = only {
            by.retain(|p, _| *p == o);
            by.entry(o).or_default();
        }
        for (pos, (pts, n)) in by {
            self.probe_split.insert(pos, n);
            self.world.set_probes(pos, pts);
            self.streamer
                .remesh_now(&self.world, &mut self.workers, pos);
        }
    }

    /// Skins des têtes de joueurs (posées, ou dans l'inventaire) : demandés au relais, et
    /// redemandés toutes les deux minutes (un joueur a pu changer de skin) ; celui du joueur
    /// `me` (le compte) est connu ici.
    pub fn update_head_skins(&mut self, me: &str) {
        let Some(h) = self.heads.as_mut() else {
            return;
        };
        let again = self
            .head_skins_at
            .is_none_or(|t| t.elapsed() > std::time::Duration::from_secs(120));
        if again {
            self.head_skins_at = Some(std::time::Instant::now());
        }
        let pseudos: Vec<String> = h.players().map(str::to_string).collect();
        for p in pseudos {
            let skin = if !me.is_empty() && p.eq_ignore_ascii_case(me) {
                self.skins.mine()
            } else {
                if again {
                    self.skins.refresh(&p);
                }
                self.skins.of(&p)
            };
            if let Some(s) = skin {
                h.set_player(&p, s);
            }
        }
    }

    /// « Copier le bloc » : l'objet visé vient en main (déjà dans la barre : on le
    /// choisit ; sinon, en créatif, une pile neuve).
    /// (v281) Seau tenu, clic droit (voir `bucket`) : le vide prend la première source d'eau ou
    /// de lave visée, le plein pose la sienne sur la face visée. En survie, il change dans la
    /// main (`turn_selected_into`) ; en créatif, il ne change pas : le vide enlève les sources
    /// sans se remplir, le plein ne se vide jamais. Ce que la source posée remplace (plante,
    /// torche) tombe en objet en solo, comme quand le fluide coule dessus.
    fn use_bucket(&mut self, content: Option<minelib_world::registry::Fluid>) {
        use minelib_world::registry::{Fluid, FLUID_SOURCE};
        let (eye, dir) = (self.aim_eye().to_array(), self.player.look_dir().to_array());
        let now = match content {
            None => {
                let Some((at, f)) = crate::bucket::source_aimed(&self.world, eye, dir, REACH)
                else {
                    return;
                };
                let changed = self.world.set(at[0], at[1], at[2], BlockId::AIR);
                if changed.is_empty() {
                    return;
                }
                self.dirty.extend(changed);
                crate::audio::bucket(true, f == Fluid::Lava);
                Some(f)
            }
            Some(f) => {
                let Some(at) = crate::bucket::place_aimed(&self.world, eye, dir, REACH) else {
                    return;
                };
                let old = self.world.get(at[0], at[1], at[2]);
                let changed = self.world.set(at[0], at[1], at[2], f.block(FLUID_SOURCE));
                if changed.is_empty() {
                    return;
                }
                self.dirty.extend(changed);
                self.fluids.washed.push((at, old));
                crate::audio::bucket(false, f == Fluid::Lava);
                None
            }
        };
        self.gesture = crate::hand::Gesture::Swing;
        if self.game.mode != GameMode::Creative {
            self.turn_selected_into(Item::Bucket(now));
        }
    }

    /// (v281) L'objet en main devient `item` : seul, il est remplacé ; d'une pile, elle en perd
    /// un et `item` va dans l'inventaire (à nos pieds s'il est plein).
    fn turn_selected_into(&mut self, item: Item) {
        let i = self.inventory.selected;
        let Some(stack) = self.inventory.slots[i].as_mut() else {
            return;
        };
        if stack.count <= 1 {
            stack.item = item;
            return;
        }
        stack.count -= 1;
        if self.inventory.add(item, 1) > 0 {
            let feet = self.player.pos;
            let pile = minelib_net::items::Pile {
                item: item.code(),
                count: 1,
            };
            self.items.drop_item(feet + Vec3::Y, feet, pile);
        }
    }

    fn give_picked(&mut self, wanted: Item) {
        if let Some(i) = self.inventory.slots[..inventory::HOTBAR]
            .iter()
            .position(|s| s.is_some_and(|s| s.item == wanted))
        {
            self.inventory.select(i);
        } else if self.game.mode == GameMode::Creative {
            // Emplacement vide de la barre s'il y en a un, sinon celui en main. (v247) Jamais
            // par-dessus la Foudre de Zeus (« j'ai réussi à supprimer la foudre en copiant un
            // bloc avec le clic molette ») : la case suivante de la barre qui ne la tient pas.
            let inv = &self.inventory;
            let Some(slot) = (0..inventory::HOTBAR)
                .find(|&i| inv.slots[i].is_none())
                .or_else(|| inv.replaceable_slot())
            else {
                return;
            };
            self.inventory.slots[slot] = Some(inventory::Stack {
                item: wanted,
                count: 64,
            });
            self.inventory.select(slot);
        }
    }

    /// Objet posé : retiré de la main, sauf en créatif (réserve illimitée).
    fn use_selected(&mut self) {
        if self.game.mode != GameMode::Creative {
            self.inventory.take_selected();
        }
    }

    /// Sources de lumière proches (flammes des torches, blocs lumineux), pour leurs reflets
    /// sur les surfaces brillantes ; mises à jour 10 fois par seconde.
    fn update_lights(&mut self, renderer: &mut Renderer, dt: f32) {
        self.lights_timer -= dt;
        if self.lights_timer > 0.0 {
            return;
        }
        self.lights_timer = 0.1;
        let lights = collect_lights(&self.world, self.camera_pos());
        let planes: &[MirrorPlane] = if renderer.raytracing() {
            &self.mirror_planes
        } else {
            &[]
        };
        renderer.set_mirror_lights(&mirror_lights(&lights, planes, self.player.eye()));
        renderer.set_lights(&lights);
    }

    /// Raytracing : grille de blocs autour du joueur (128 × 64 × 128), refaite quand il
    /// s'est déplacé de 8 blocs ou qu'un bloc a changé (au plus 4 fois par seconde).
    fn update_voxels(&mut self, renderer: &mut Renderer, dt: f32) {
        use minelib_render::rt::VOX;
        // La grille sert aux reflets et aux ombres des lumières de l'éditeur.
        if !renderer.raytracing() && !renderer.dyn_lights_want_voxels() {
            self.vox_origin = None;
            self.vox_job = None;
            self.mirror_planes.clear();
            return;
        }
        // Grille calculée en arrière-plan : on l'envoie au GPU dès qu'elle est prête.
        if let Some(rx) = &self.vox_job {
            match rx.try_recv() {
                Ok(grid) => {
                    let _p = minelib_render::perf::scope("grille raytracing (envoi)");
                    renderer.set_voxels(grid.origin, &grid.ids, grid.bricks);
                    self.mirror_planes = grid.planes;
                    self.lights_timer = 0.0;
                    self.vox_job = None;
                    minelib_render::perf::event(|| {
                        format!(
                            "grille du raytracing reçue ({:.1} ms en arrière-plan)",
                            grid.ms
                        )
                    });
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => return,
                Err(_) => self.vox_job = None,
            }
        }
        self.vox_timer -= dt;
        // Autour de la caméra (la caméra libre de l'éditeur peut être loin du joueur).
        let e = self.camera_pos();
        let snap = |v: f32, half: i32| ((v.floor() as i32 - half).div_euclid(8)) * 8;
        let origin = [
            snap(e.x, VOX[0] as i32 / 2),
            snap(e.y, VOX[1] as i32 / 2),
            snap(e.z, VOX[2] as i32 / 2),
        ];
        let moved = self.vox_origin != Some(origin);
        if !moved && !(self.vox_dirty && self.vox_timer <= 0.0) {
            return;
        }
        // Copie légère (pointeurs partagés) des chunks couverts par la grille.
        let c0 = ChunkPos::containing(origin[0], origin[2]);
        let c1 = ChunkPos::containing(origin[0] + VOX[0] as i32 - 1, origin[2] + VOX[2] as i32 - 1);
        let mut chunks = HashMap::new();
        for cz in c0.z..=c1.z {
            for cx in c0.x..=c1.x {
                let p = ChunkPos { x: cx, z: cz };
                if let Some(c) = self.world.chunk_arc(p) {
                    chunks.insert(p, c);
                }
            }
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("minelib-raytracing".into())
            .spawn(move || {
                minelib_platform::lower_thread_priority();
                let t = std::time::Instant::now();
                let (ids, planes) = build_voxel_grid(&chunks, origin);
                // (v203) Ses empreintes aussi, ici plutôt qu'à son arrivée dans l'image.
                let bricks = minelib_render::VoxelBricks::new(origin, &ids);
                let _ = tx.send(VoxelGrid {
                    origin,
                    ids,
                    bricks,
                    planes,
                    ms: t.elapsed().as_secs_f32() * 1000.0,
                });
            });
        if spawned.is_ok() {
            self.vox_job = Some(rx);
        }
        self.vox_origin = Some(origin);
        self.vox_dirty = false;
        self.vox_timer = 0.25;
    }

    /// Ouvre l'éditeur de lumières (touche L, `/lumieres`). Pas en mode FPS. En
    /// construction en ligne, il est réservé aux OP du serveur : on demande d'abord la liste
    /// des OP au relais, qui ne la donne qu'à eux, et l'éditeur s'ouvre à sa réponse (voir
    /// `light_op_answer`). La caméra libre de l'éditeur ne sert donc pas à espionner.
    /// Lumières de l'éditeur (projecteurs…) : partout sauf en partie FPS en ligne (elles ne
    /// sont pas envoyées aux autres joueurs : un coin sombre éclairé pour un seul). En FPS
    /// solo, si.
    fn lights_allowed(&self) -> bool {
        // (v172) Contre les bots (partie solo), aussi. (v243) Et dans la partie d'un serveur de
        // construction : ses lumières sont celles du serveur, pour tous.
        !(self.game.mode == GameMode::Fps && self.ranked_match())
    }

    /// (v201) Lumière de la lampe torche tenue et allumée : un projecteur au verre de la
    /// lampe (devant les yeux à la première personne, dans la main du modèle à la troisième),
    /// pointé où l'on regarde.
    fn flashlight_light(&self) -> Option<minelib_render::DynLight> {
        let held = self.game.mode.has_inventory()
            && self.flashlight_on
            && self.held_item() == Some(Item::Flashlight)
            && self.lights_allowed();
        if !held {
            return None;
        }
        let eye = self.player.eye();
        let look = self.player.look_dir();
        // (v215) Toujours l'ampoule de la lampe que tient le modèle du joueur (celui des
        // miroirs, de la troisième personne et des autres joueurs), même à la première
        // personne, où la lampe en main est dessinée à cette même place : la lumière part de
        // la lampe qu'on voit, de face comme dans un miroir.
        let held = crate::player_model::Held::Flashlight(true);
        let bulb = crate::player_model::flashlight_bulb(&self.own_pose(held));
        let mut pos = bulb.unwrap_or_else(|| crate::flashlight::lens_position(eye, look));
        // Collé à un mur, le verre reste de ce côté-ci (sinon le mur arrête toute la
        // lumière).
        let to = pos - eye;
        let len = to.length();
        if len > 1e-4 {
            if let Some(h) = raycast(&self.world, eye.into(), (to / len).into(), len) {
                pos = eye + to / len * (h.distance - 0.05).max(0.0);
            }
        }
        Some(crate::flashlight::light(
            pos,
            eye,
            look,
            self.flashlight_focus,
        ))
    }

    /// (v222) Pose du modèle d'un autre joueur (construction en ligne) tenant `held`.
    fn other_pose(
        &self,
        o: &crate::buildnet::Other,
        held: Option<crate::player_model::Held>,
    ) -> crate::player_model::Pose {
        let w = self.walks.get(&o.pseudo);
        crate::player_model::Pose {
            feet: o.pos,
            yaw: o.yaw,
            pitch: o.pitch,
            walk: w.map_or(0.0, |w| w.phase),
            swing: w.map_or(0.0, |w| w.amp),
            crouch: o.crouch,
            light: [1.0; 3],
            held,
            skin: None,
            bow: 0.0,
            air: w.map_or(0.0, |w| w.air),
            dir: w.map_or(0.0, |w| w.dir),
            run: w.map_or(0.0, |w| w.run),
        }
    }

    /// (v222) Lumières de ce que tiennent les autres joueurs (construction en ligne) : leur
    /// lampe torche allumée (depuis l'ampoule de la lampe que tient leur modèle, vers leur
    /// regard, à leur réglage) ou leur Foudre de Zeus (depuis leur poing, avec son
    /// scintillement). Les `flashlight::REMOTE_LAMPS` plus proches de la caméra, à moins de
    /// `flashlight::REMOTE_RANGE` ; chacune a son bit de porteur : les morceaux de celui qui
    /// la tient ne la cachent pas (voir `lamp_owner`).
    fn remote_held_lights(&self, cam: Vec3) -> Vec<(String, minelib_render::DynLight)> {
        use minelib_net::poses::Held;
        let Some(b) = &self.build else {
            return Vec::new();
        };
        let range2 = crate::flashlight::REMOTE_RANGE * crate::flashlight::REMOTE_RANGE;
        let mut near: Vec<(f32, &crate::buildnet::Other)> = b
            .others
            .values()
            .filter(|o| match o.held {
                Some(Held::Lamp(l)) => l.on,
                Some(Held::Zeus) => true,
                None => false,
            })
            .map(|o| (o.pos.distance_squared(cam), o))
            .filter(|(d2, _)| *d2 < range2)
            .collect();
        near.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut out = Vec::new();
        for (k, (_, o)) in near
            .into_iter()
            .take(crate::flashlight::REMOTE_LAMPS)
            .enumerate()
        {
            let mut l = match o.held {
                Some(Held::Lamp(lamp)) => {
                    let pose =
                        self.other_pose(o, Some(crate::player_model::Held::Flashlight(true)));
                    let Some(bulb) = crate::player_model::flashlight_bulb(&pose) else {
                        continue;
                    };
                    let (sy, cy) = o.yaw.sin_cos();
                    let (sp, cp) = o.pitch.sin_cos();
                    let look = Vec3::new(sy * cp, sp, -cy * cp);
                    let focus = f32::from(lamp.step) / f32::from(minelib_net::poses::Lamp::STEPS);
                    let mut l =
                        crate::flashlight::light(bulb, eye_of(o.pos, o.crouch), look, focus);
                    l.id = crate::flashlight::LIGHT_ID + 1 + k as u64;
                    l
                }
                Some(Held::Zeus) => {
                    let pose = self.other_pose(o, Some(crate::player_model::Held::Zeus));
                    let Some(grip) = crate::player_model::zeus_frame(&pose) else {
                        continue;
                    };
                    let glow = self.remote_bolts.get(&o.pseudo).map_or(1.0, |h| h.glow());
                    // Comme la sienne : de l'éclair lui-même, au-dessus du poing.
                    let at = grip.transform_point3(Vec3::Y * (crate::lightning::HELD_LEN * 0.42));
                    crate::storm::held_light_at(at, glow, k)
                }
                None => continue,
            };
            l.owner = crate::flashlight::REMOTE_HOLDER << k;
            out.push((o.pseudo.clone(), l));
        }
        out
    }

    /// (v222) Éclairs tenus par les autres joueurs (construction en ligne) à moins de
    /// `ZEUS_SEEN` blocs de la caméra : chacun son éclair qui crépite et ses étincelles,
    /// dans sa main (comme on voit le sien à la troisième personne).
    fn update_remote_bolts(&mut self, dt: f32, cam: Vec3) {
        use minelib_net::poses::Held;
        let holders: Vec<(String, Mat4)> = self
            .build
            .as_ref()
            .map(|b| {
                b.others
                    .values()
                    .filter(|o| {
                        o.held == Some(Held::Zeus)
                            && o.pos.distance_squared(cam) < ZEUS_SEEN * ZEUS_SEEN
                    })
                    .filter_map(|o| {
                        let pose = self.other_pose(o, Some(crate::player_model::Held::Zeus));
                        Some((o.pseudo.clone(), crate::player_model::zeus_frame(&pose)?))
                    })
                    .collect()
            })
            .unwrap_or_default();
        // (v241) Ceux qui l'ont sur eux sans la tenir : dans leur dos.
        let backs: Vec<(Mat4, f32)> = self
            .build
            .as_ref()
            .map(|b| {
                b.others
                    .values()
                    .filter(|o| o.back && o.pos.distance_squared(cam) < ZEUS_SEEN * ZEUS_SEEN)
                    .filter_map(|o| {
                        let pose = self.other_pose(o, None);
                        let phase =
                            o.pseudo.bytes().map(f32::from).sum::<f32>() % std::f32::consts::TAU;
                        Some((crate::player_model::zeus_back_frame(&pose)?, phase))
                    })
                    .collect()
            })
            .unwrap_or_default();
        for (place, phase) in backs {
            self.storm.back_item(place, phase);
        }
        // (Ceux qui l'ont rangé : leurs étincelles finissent leur course, puis on les oublie.)
        let mut arcs = Vec::new();
        for (pseudo, bolt) in &mut self.remote_bolts {
            let place = holders.iter().find(|(p, _)| p == pseudo).map(|h| h.1);
            bolt.update(dt, place.unwrap_or(Mat4::IDENTITY), place.is_some());
            arcs.clear();
            if let Some(place) = place {
                bolt.arcs(&mut arcs);
                crate::lightning::in_world(&arcs, place, &mut self.storm.bolts);
            }
            arcs.clear();
            bolt.sparks(&mut arcs);
            crate::lightning::in_world(&arcs, Mat4::IDENTITY, &mut self.storm.bolts);
        }
        self.remote_bolts
            .retain(|p, b| holders.iter().any(|h| &h.0 == p) || b.has_sparks());
        for (pseudo, _) in holders {
            self.remote_bolts.entry(pseudo).or_default();
        }
    }

    /// (v222) Bit de porteur de la lampe allumée de l'autre joueur `pseudo` (voir
    /// `remote_lamp_lights`), s'il en a une qui éclaire ici.
    fn lamp_owner(&self, pseudo: &str) -> Option<u32> {
        self.remote_lamps
            .iter()
            .find(|(p, _)| p == pseudo)
            .map(|(_, bit)| *bit)
    }

    /// (v201) Torche tenue, avec les torches en lumières ponctuelles : une lumière
    /// ponctuelle à sa flamme, comme les torches posées (au lieu de la lumière simple de la
    /// torche tenue).
    fn held_torch_light(&self) -> Option<minelib_render::DynLight> {
        if !self.lights.point_torches || !self.lights_allowed() {
            return None;
        }
        let Some(Item::Torch(chem)) = self.held_item() else {
            return None;
        };
        let rgb = chem
            .and_then(|i| minelib_world::flame::CHEMICALS.get(i))
            .map_or(minelib_world::flame::WOOD_FLAME, |c| c.rgb);
        let pose = self.own_pose(crate::player_model::Held::Torch(rgb));
        let tip = crate::player_model::torch_tip(&pose)?;
        Some(crate::lights::held_torch(tip, rgb, &self.lights.settings))
    }

    /// (v216) La Foudre de Zeus est-elle en main ?
    fn holding_zeus(&self) -> bool {
        self.held_item() == Some(Item::Zeus)
            && self.spectating.is_none()
            && self.game.mode != GameMode::Spectator
    }

    /// (v241) A-t-on la Foudre de Zeus sur soi sans la tenir (on la voit alors dans son dos) ?
    fn zeus_on_back(&self) -> bool {
        self.game.mode.has_inventory()
            && self.spectating.is_none()
            && self.game.mode != GameMode::Spectator
            && self.inventory.has_zeus()
            && !self.holding_zeus()
    }

    /// (v241) Les règles de l'objet légendaire valent-elles ici ? En solo. (v244) Sur un
    /// serveur de construction aussi : le relais du monde les y tient (`zeus_online`).
    fn legend_on(&self) -> bool {
        self.net.is_none() && self.game.mode != GameMode::Fps
    }

    /// (v244) Notre pseudo sur le serveur de construction, si l'éclair du serveur est à nous
    /// d'après le relais du monde.
    fn zeus_mine_online(&self) -> bool {
        self.build
            .as_ref()
            .is_some_and(|b| self.zeus_online.mine(b.pseudo()))
    }

    /// (v244) Une demande au relais du monde pour l'éclair du serveur (`Request::Zeus`) :
    /// son état, le prendre, dire qu'on l'a déjà. La graine du monde l'accompagne (le relais
    /// ne la retient que de son créateur : il en tire la météo, dont l'orage qui la charge).
    fn zeus_cmd(&mut self, act: u8, charges: u8) {
        self.thor_seq = self.thor_seq.wrapping_add(1);
        let chunk = self.net_chunk(self.player.pos);
        self.relay_cmds.push(minelib_net::client::Cmd::Zeus {
            seq: self.thor_seq,
            act,
            chunk,
            seed: self.world.seed,
            charges,
        });
    }

    /// (v318) Le chunk absolu (sur la carte bouclée) du point `p` de la partie : celui que
    /// les relais connaissent.
    fn net_chunk(&self, p: Vec3) -> [i32; 2] {
        let a = self.abs_of(p);
        minelib_net::proto::chunk_at(a[0], a[2])
    }

    /// (v244) Un coup demandé au relais du monde : `act` (`proto::ZEUS_SKY`, `ZEUS_STRIKE`,
    /// `ZEUS_ARC`), de `pos` vers `to` (voir `BuildMsg::ZeusAsk`). S'il l'accorde, son
    /// laissez-passer part à l'arbitre du chunk (`on_zeus`), qui publie le coup à tous ceux
    /// qui sont près, nous compris (`update_thor`).
    fn zeus_ask(&mut self, act: u8, pos: Vec3, to: Vec3, npc: Option<NpcTarget>, hit: bool) {
        self.thor_seq = self.thor_seq.wrapping_add(1);
        let seq = self.thor_seq;
        // (v318) Le chunk absolu : le laissez-passer du relais du monde est pour lui, et
        // l'arbitre le compare à celui du coup (`BuildNet::send_thor`, en place absolue).
        let chunk = self.net_chunk(pos);
        self.zeus_pending
            .retain(|p| p.at.elapsed().as_secs_f32() < 15.0);
        if self.zeus_pending.len() >= 8 {
            self.zeus_pending.remove(0);
        }
        self.zeus_pending.push(ZeusPending {
            seq,
            act,
            chunk,
            pos,
            to,
            seed: rand::random::<u32>(),
            npc,
            hit,
            at: std::time::Instant::now(),
        });
        self.relay_cmds.push(minelib_net::client::Cmd::Zeus {
            seq,
            act,
            chunk,
            seed: 0,
            charges: 0,
        });
    }

    /// (v244) Ce que dit le relais du monde de l'éclair du serveur (`Response::Zeus`) : qui
    /// l'a, ses charges, et sa réponse à nos demandes.
    #[allow(clippy::too_many_arguments)]
    fn on_zeus(
        &mut self,
        seq: u32,
        act: u8,
        why: u8,
        holder: String,
        charges: u8,
        sky_hits: u8,
        expires: u64,
        permit: Vec<u8>,
    ) {
        use minelib_net::proto::{
            ZEUS_ARC, ZEUS_CLAIM, ZEUS_CLOUD, ZEUS_EMPTY, ZEUS_HOLD, ZEUS_NOT_CREATIVE,
            ZEUS_NOT_HOLDER, ZEUS_OK, ZEUS_RETURN, ZEUS_SKY, ZEUS_STRIKE, ZEUS_TAKEN,
        };
        if self.build.is_none() {
            return;
        }
        let before = self.zeus_online.holder.replace(holder.clone());
        let mine = self.zeus_mine_online();
        // Ce qu'on garde avec la carte suit le relais.
        self.legend.taken = !holder.is_empty();
        self.legend.elsewhere = self.legend.taken && !mine;
        if !mine {
            (self.legend.charges, self.legend.sky_hits) = (0, 0);
        } else if seq != 0 || act == ZEUS_CLOUD {
            // (Une charge donnée par l'orage se lit sur l'objet, pas dans le tchat : règle 17
            // de `claude/foudre-de-zeus.md`.)
            self.legend.charges = charges.min(crate::zeus::MAX_CHARGES);
            self.legend.sky_hits = sky_hits.min(crate::zeus::SKY_HITS - 1);
        }
        match act {
            ZEUS_CLAIM if seq != 0 => {
                self.zeus_online.claiming = false;
                match why {
                    ZEUS_OK => self
                        .chat
                        .say("La Foudre de Zeus de ce serveur est à toi.", chat::YELLOW),
                    ZEUS_TAKEN => self.chat.say(
                        format!("La Foudre de Zeus de ce serveur a déjà été prise par {holder}."),
                        chat::RED,
                    ),
                    ZEUS_NOT_CREATIVE => self
                        .chat
                        .say("La Foudre de Zeus se prend en créatif.", chat::RED),
                    _ => self.chat.say(
                        "Le relais n'a pas donné la Foudre de Zeus : réessaie.",
                        chat::GREY,
                    ),
                }
            }
            ZEUS_HOLD if seq != 0 => self.zeus_online.holding = false,
            // (v247) Rendue au menu créatif ; refusée (plus en créatif…), elle nous est remise
            // (`sync_legend_online`).
            ZEUS_RETURN if seq != 0 => {
                self.zeus_online.returning = false;
                if why == ZEUS_NOT_CREATIVE {
                    self.chat.say(
                        "La Foudre de Zeus se rend au menu créatif en créatif seulement.",
                        chat::RED,
                    );
                }
            }
            ZEUS_SKY | ZEUS_STRIKE | ZEUS_ARC if seq != 0 => {
                let Some(i) = self.zeus_pending.iter().position(|p| p.seq == seq) else {
                    return;
                };
                if why == ZEUS_OK {
                    let p = &self.zeus_pending[i];
                    let req = minelib_net::game::BuildMsg::ZeusAsk {
                        seq,
                        chunk: p.chunk,
                        act: p.act,
                        // (v318) Comptés depuis l'origine de la partie : `BuildNet::send_thor`
                        // les met en places absolues.
                        pos: p.pos.to_array().map(f64::from),
                        to: p.to.to_array().map(f64::from),
                        seed: p.seed,
                        expires,
                        permit,
                    };
                    if let Some(b) = self.build.as_mut() {
                        b.send_thor(&req);
                    }
                    return;
                }
                self.zeus_pending.remove(i);
                // (Sans charge d'après le relais : rien à dire, le prochain coup sera le
                // petit éclair.)
                let quiet = why == ZEUS_EMPTY
                    || self
                        .thor_refused
                        .is_some_and(|t| t.elapsed().as_secs_f32() < 3.0);
                if !quiet {
                    self.thor_refused = Some(std::time::Instant::now());
                    self.chat.say(
                        if why == ZEUS_NOT_HOLDER {
                            "Cette Foudre de Zeus n'est pas la tienne."
                        } else {
                            "La foudre n'est pas partie (relais pas prêt, ou trop de demandes) : réessaie."
                        },
                        chat::GREY,
                    );
                }
            }
            // Une nouvelle : quelqu'un d'autre vient de la prendre.
            _ => {
                let news = seq == 0 && act != ZEUS_CLOUD && before.as_deref() != Some(holder.as_str());
                if news && !holder.is_empty() && !mine {
                    self.chat.say(
                        format!("{holder} a pris la Foudre de Zeus de ce serveur."),
                        chat::GREY,
                    );
                }
            }
        }
    }

    /// (v244) Sur un serveur de construction, chaque image : l'état de l'éclair est demandé
    /// à l'arrivée ; le catalogue le demande au relais ; il est sur nous s'il est à nous, et
    /// seulement alors (celui qu'on avait pris en solo sur cette carte : on le dit au
    /// relais, qui ne l'entend que du créateur du monde).
    fn sync_legend_online(&mut self) {
        use minelib_net::proto::{ZEUS_CLAIM, ZEUS_HOLD, ZEUS_RETURN, ZEUS_SYNC};
        if !self.zeus_online.asked {
            self.zeus_online.asked = true;
            self.zeus_cmd(ZEUS_SYNC, 0);
        }
        let mine = self.zeus_mine_online();
        self.inventory.legend_took = false;
        if std::mem::take(&mut self.inventory.legend_wanted) && !mine && !self.zeus_online.claiming
        {
            self.zeus_online.claiming = true;
            self.zeus_cmd(ZEUS_CLAIM, 0);
        }
        // (v247) Supprimée dans le menu créatif : elle y retourne, pour tout le serveur (le
        // relais du monde la rend ; jusque-là, elle ne nous est pas remise).
        if std::mem::take(&mut self.inventory.legend_returned)
            && mine
            && !self.inventory.has_zeus()
            && !self.zeus_online.returning
        {
            self.zeus_online.returning = true;
            self.zeus_cmd(ZEUS_RETURN, 0);
        }
        self.inventory.legend_free = self.zeus_online.free() && !self.zeus_online.claiming;
        if self.zeus_online.holder.is_none() {
            return;
        }
        if mine {
            if !self.inventory.has_zeus() && !self.zeus_online.returning {
                self.inventory.add(Item::Zeus, 1);
            }
            self.inventory.keep_one_zeus(true);
        } else if self.inventory.has_zeus() && !self.zeus_online.holding {
            let kept = self.legend.taken && !self.legend.elsewhere;
            if self.zeus_online.free() && kept && !self.zeus_online.hold_asked {
                self.zeus_online.hold_asked = true;
                self.zeus_online.holding = true;
                self.zeus_cmd(ZEUS_HOLD, self.legend.charges);
            } else {
                self.inventory.keep_one_zeus(false);
                self.chat.say(
                    "Sur ce serveur, la Foudre de Zeus est légendaire : une seule, à prendre dans le menu créatif. La tienne est retirée.",
                    chat::GREY,
                );
            }
        }
    }

    /// (v241) Ce qu'on garde de l'éclair avec la carte (rien tant qu'il n'a pas été pris).
    fn legend_saved(&self) -> String {
        if self.legend.taken {
            self.legend.save()
        } else {
            String::new()
        }
    }

    /// (v241) Chaque image : l'inventaire sait si l'éclair de la carte peut encore être pris,
    /// la partie note qu'il vient de l'être, les refus sont dits, et l'icône montre ses
    /// charges.
    fn sync_legend(&mut self) {
        let on = self.legend_on();
        let online = on && self.build.is_some();
        self.inventory.legend = on;
        self.inventory.legend_ask = online;
        if online {
            self.sync_legend_online();
        } else {
            if std::mem::take(&mut self.inventory.legend_took) && on {
                self.legend.taken = true;
                self.legend.elsewhere = false;
            }
            // (v247) Supprimé dans le menu créatif : il y retourne, personne ne l'a plus (ses
            // charges restent les siennes).
            if std::mem::take(&mut self.inventory.legend_returned)
                && on
                && !self.inventory.has_zeus()
            {
                self.legend.taken = false;
            }
            self.inventory.legend_free = on && !self.legend.taken;
        }
        if let Some(text) = self.inventory.notice.take() {
            self.chat.say(text, chat::GREY);
        }
        let mine = !online || self.zeus_mine_online();
        crate::zeus::show(on.then_some(if mine { self.legend.charges } else { 0 }));
    }

    /// (v241) Un coup de la Foudre de Zeus, objet légendaire. Vers le ciel : la foudre monte
    /// de sa pointe, et deux coups font une charge. Chargée : la foudre tombe du ciel sur le
    /// bloc, le joueur ou le PNJ visé à moins de 128 blocs (une charge de moins), même tout
    /// près. Sans charge : un arc part de sa pointe jusqu'à ce qu'on vise, 8 blocs au plus ;
    /// il part même si l'on ne vise rien (il finit alors en l'air). Rend vrai si un coup est
    /// parti.
    fn zeus_blow(&mut self) -> bool {
        use crate::zeus::{Blow, ARC_REACH};
        use minelib_net::proto::{ZEUS_ARC, ZEUS_SKY, ZEUS_STRIKE};
        // (v244) Sur un serveur : c'est le relais du monde qui accorde chaque coup, à celui
        // qui a l'éclair du serveur (sinon l'objet nous est retiré : `sync_legend_online`).
        let online = self.build.is_some();
        if online && !self.zeus_mine_online() {
            return false;
        }
        // (v281) Visé depuis le viseur (`aim_eye`), comme `aimed` et `aimed_person`.
        let (eye, dir) = (self.aim_eye(), self.player.look_dir());
        // (v244) « Enlève la distance max pour taper avec la foudre, c'est comme le /thor » :
        // ce qu'on vise est ce que vise `/thor` (`aimed`).
        let aim = self.aimed();
        let blow = self.legend.blow(matches!(aim, Some(Aim::Sky(_))));
        match (blow, aim) {
            (Blow::Sky, Some(Aim::Sky(dir))) => {
                if online {
                    let from = self.sky_origin(dir);
                    self.zeus_ask(ZEUS_SKY, from, dir.normalize_or(Vec3::Y), None, false);
                } else {
                    self.throw_lightning(Aim::Sky(dir));
                    self.legend.sky_hit();
                }
            }
            // (La foudre blesse en tombant ceux qu'elle touche, PNJ compris : `storm.landed`.)
            (Blow::Strike, Some(Aim::Ground(pos))) => {
                if online {
                    self.zeus_ask(ZEUS_STRIKE, pos, pos, None, false);
                } else {
                    self.legend.spend();
                    self.throw_lightning(Aim::Ground(pos));
                }
            }
            (Blow::Arc, _) => {
                // (v244) Il part du bout de l'éclair, et sa portée se mesure de là : jusqu'à
                // ce qu'on vise (« si on vise un bloc précis, ça va jusqu'au bloc » ;
                // quelqu'un : à mi-corps, pas à ses pieds) s'il est à 8 blocs de la pointe
                // ou moins, sinon 8 blocs plus loin, en l'air (« il doit vraiment faire les
                // 8 blocs de trajet »).
                let tip = self.zeus_tip();
                let far = ARC_REACH + 2.0;
                let block = raycast(&self.world, eye.into(), dir.into(), far);
                let limit = block.as_ref().map_or(far, |h| h.distance);
                let person = self.aimed_person(limit);
                let reached = |t: f32| (eye + dir * t - tip).length() <= ARC_REACH;
                // (v281) `hit` : il touche quelqu'un (le coup sec part avec lui).
                let (to, npc, hit) = match (person, &block) {
                    (Some(p), _) if reached(p.t) => (p.body, p.npc, true),
                    (None, Some(h)) if reached(h.distance) => (eye + dir * h.distance, None, false),
                    _ => (crate::zeus::arc_end(eye, dir, tip, ARC_REACH), None, false),
                };
                if online {
                    self.zeus_ask(ZEUS_ARC, tip, to, npc, hit);
                } else {
                    self.storm.strike_arc(tip, to, hit);
                    if let Some(target) = npc {
                        self.hit_npc_for(target, f32::from(crate::zeus::ARC_DAMAGE));
                    }
                }
            }
            // Chargée, rien à viser (rien de chargé par là) : rien ne part.
            _ => return false,
        }
        self.storm.cooldown = crate::storm::ZEUS_COOLDOWN;
        true
    }

    /// (v216) Lumière de la Foudre de Zeus tenue : au poing, avec les ombres (celle du
    /// joueur comprise, sauf sa main), scintillante comme l'éclair.
    fn zeus_light(&self) -> Option<minelib_render::DynLight> {
        if !self.holding_zeus() || !self.lights_allowed() {
            return None;
        }
        // (v217) Elle part de l'éclair lui-même, au-dessus du poing du modèle, à la première
        // personne comme vu de derrière : loin de la tête et du bras, tout le joueur fait
        // son ombre (avant, devant les yeux, sa tête et son bras n'en faisaient pas, et
        // son ombre n'était pas la même en F5).
        let pose = self.own_pose(crate::player_model::Held::Zeus);
        let at = crate::player_model::zeus_frame(&pose).map_or_else(
            || crate::flashlight::lens_position(self.player.eye(), self.player.look_dir()),
            |m| m.transform_point3(Vec3::Y * (crate::lightning::HELD_LEN * 0.42)),
        );
        Some(self.storm.held_light(at))
    }

    /// (v216) Météo, foudre et pluie de l'image, et la Foudre de Zeus tenue en main.
    fn update_storm(&mut self, dt: f32) {
        let fps = self.game.mode == GameMode::Fps;
        // Pas de météo sur les cartes FPS, ni à l'heure « par défaut » (soleil immobile).
        let active = !fps && self.time_h >= 0.0;
        let eye = self.camera_pos();
        // (v324) La foudre et les éclairs des nuages tirés en places absolues : les mêmes pour
        // tous, quelle que soit l'origine de la partie de chacun.
        let o = self.world.origin_blocks();
        self.storm.weather.origin = [o[0] as i64, o[2] as i64];
        let mine = self.legend_on() && self.inventory.has_zeus();
        // (v324) Celui qui porte la Foudre de Zeus du serveur, si ce n'est pas nous et qu'on le
        // voit : au milieu de son chunk (comme le sien chez lui) ; les éclairs des nuages sont
        // plus fréquents au-dessus de lui chez nous aussi, les mêmes que chez lui. Avant, seul
        // le porteur les voyait.
        let other_holder = self
            .build
            .as_ref()
            .filter(|_| !mine)
            .and_then(|b| {
                let who = self.zeus_online.holder.as_deref().filter(|h| !h.is_empty())?;
                let key: String = who.chars().take(16).collect();
                let p = b
                    .others
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case(&key))
                    .map(|(_, o)| o.pos)?;
                Some(Vec3::new(
                    (p.x / 16.0).floor() * 16.0 + 8.0,
                    p.y,
                    (p.z / 16.0).floor() * 16.0 + 8.0,
                ))
            });
        // (v324) Qui la porte ne se met pas en veille au fond d'une province : le relais du
        // monde lui accorde les charges de l'orage, il doit l'entendre.
        if let Some(b) = self.build.as_mut() {
            b.set_keep_root(mine);
        }
        let hurt = self.storm.update(
            dt,
            &crate::storm::Frame {
                world: &self.world,
                feet: self.player.pos,
                eye,
                active,
                online: self.build.is_some(),
                covered: covered_above(&self.world, eye),
                // (v241) Celui qui porte l'éclair légendaire : le joueur, s'il l'a sur lui.
                // (v244) Sur un serveur : au milieu de son chunk, là où le relais du monde
                // (qui accorde les charges) le sait.
                holder: mine.then(|| {
                    let p = self.player.pos;
                    if self.build.is_some() {
                        Vec3::new(
                            (p.x / 16.0).floor() * 16.0 + 8.0,
                            p.y,
                            (p.z / 16.0).floor() * 16.0 + 8.0,
                        )
                    } else {
                        p
                    }
                }),
                other_holder,
            },
        );
        if hurt > 0 {
            // (v249) « On n'est pas assez ébloui quand on prend un éclair, comme dans sa
            // première version » : la foudre qui nous touche nous éblouit (dans tous les
            // modes de jeu), comme la première version blanchissait l'image. Celle qui tombe
            // à côté garde son réglage (on voit l'éclair).
            self.dazzle = 1.0;
            self.damage(hurt);
        }
        // (v238) La foudre blesse aussi les PNJ de notre foule, comme un joueur (et les fait
        // sauter) ; ceux de la foule d'un autre OP sautent tout de suite (son jeu, où elle
        // tombe aussi, leur ôte la vie). Le plus proche crie.
        // (v247) La nôtre (Foudre de Zeus, `/thor`) : c'est comme si nous les avions frappés.
        // Le personnage caché qu'elle touche est trouvé (« Pistache peut être trouvé en tapant
        // avec la foudre ») ; dans la foule d'un autre OP, le coup part à son jeu, depuis là
        // où elle est tombée.
        let me = self.player.eye();
        let center = self.player.pos;
        for (at, mine) in std::mem::take(&mut self.storm.landed) {
            let mut hit = Vec::new();
            let mut found = false;
            if let Some(c) = self.crowd.as_mut() {
                let (h, f) = c.struck(&self.world, at, center, mine);
                hit.extend(h);
                found |= f;
            }
            let mut sends = Vec::new();
            for (owner, rc) in self.remote_crowds.iter_mut() {
                let (h, hidden) = rc.struck(at, mine);
                hit.extend(h);
                if let Some(id) = hidden {
                    sends.push((owner.clone(), id));
                }
            }
            // (v281) « Joue-le quand on touche un joueur, un PNJ ou un monstre avec l'éclair
            // chargé » : elle a touché quelqu'un (un PNJ, caché compris, nous, un autre joueur,
            // L'Évidé, qu'elle ne blesse pas) : le coup sec, juste après son claquement ; la
            // nôtre en entier, où qu'elle tombe.
            let hurts = |feet: Vec3| crate::lightning::hurts_at(at, feet);
            let touched = !hit.is_empty()
                || found
                || !sends.is_empty()
                || hurts(self.player.pos)
                || self
                    .build
                    .as_ref()
                    .is_some_and(|b| b.others.values().any(|o| hurts(o.pos)))
                || self.evides.list.iter().any(|e| !e.is_dying() && hurts(e.pos));
            if touched {
                let d = if mine { 0.0 } else { at.distance(me) };
                crate::audio::zeus_hit(d, crate::storm::HIT_AFTER);
            }
            if let Some(b) = &self.build {
                for (owner, id) in sends {
                    b.send_crowd_hit(&owner, id, at.to_array());
                }
            }
            if found {
                if let (Some(out), Some(b)) = (self.crowd_out.as_mut(), &self.build) {
                    out.found = Some((b.pseudo().to_string(), at.to_array()));
                }
                self.say_found();
            }
            let near = hit.iter().map(|p| p.distance(me)).fold(f32::INFINITY, f32::min);
            if let Some(g) = npc_cry_gain(near) {
                self.fps.sounds.push((crate::audio::Sfx::Pain, 0.0, g));
            }
        }
        // (v241) Une charge de plus : (v282) toutes les 10 secondes d'orage (avant : un éclair
        // qui courait dans les nuages au-dessus de lui).
        // (v244) Sur un serveur, c'est le relais du monde qui l'accorde (`on_zeus`). (v247)
        // Sans le dire dans le tchat (« n'écris pas que les éclairs chargent notre foudre ») :
        // l'objet le dit, et son icône montre ses charges.
        let charged = if self.build.is_some() {
            0
        } else {
            self.storm.charged
        };
        for _ in 0..charged {
            self.legend.charge();
        }
        // Essais : MINELIB_ZEUS=1 (la Foudre de Zeus en main à l'arrivée), MINELIB_THOR=<s>
        // (la foudre tombe là où l'on regarde, toutes les <s> secondes).
        static TESTS: std::sync::OnceLock<(u8, Option<f32>)> = std::sync::OnceLock::new();
        let (zeus, thor) = *TESTS.get_or_init(|| {
            (
                // (v241) 2 : sur soi sans la tenir (dans la case d'à côté).
                std::env::var("MINELIB_ZEUS")
                    .ok()
                    .and_then(|v| v.trim().parse::<u8>().ok())
                    .filter(|v| (1..=2).contains(v))
                    .unwrap_or(0),
                std::env::var("MINELIB_THOR")
                    .ok()
                    .and_then(|v| v.trim().parse::<f32>().ok())
                    .filter(|s| *s > 0.0),
            )
        });
        if zeus > 0 && !std::mem::replace(&mut self.storm.test_given, true) {
            self.inventory.keep_one_zeus(false);
            self.items.keep_one_legend(false);
            let slot = (self.inventory.selected + usize::from(zeus - 1)) % inventory::HOTBAR;
            self.inventory.slots[slot] = Some(inventory::Stack {
                item: Item::Zeus,
                count: 1,
            });
            // (v241) MINELIB_ZEUS_CHARGES=<n> : avec tant de charges.
            self.legend = crate::zeus::Legend {
                taken: true,
                charges: std::env::var("MINELIB_ZEUS_CHARGES")
                    .ok()
                    .and_then(|v| v.trim().parse::<u8>().ok())
                    .unwrap_or(0)
                    .min(crate::zeus::MAX_CHARGES),
                sky_hits: 0,
                elsewhere: false,
            };
        }
        if let Some(every) = thor {
            self.storm.test_in -= dt;
            if self.storm.test_in <= 0.0 {
                // (v241) L'éclair légendaire en main : un coup de lui (chargé ou non), pas
                // la foudre de `/thor`.
                if zeus == 1 && self.legend_on() && self.holding_zeus() {
                    if self.zeus_blow() {
                        self.storm.test_in = every;
                    }
                } else if let Some(aim) = self.aimed() {
                    self.storm.test_in = every;
                    self.cast_lightning(aim);
                }
            }
        }
        // Essais : MINELIB_CMD=<s>:<commande>|… (commandes du tchat tapées à heure dite).
        static CMDS: std::sync::OnceLock<Vec<(f32, String)>> = std::sync::OnceLock::new();
        let cmds = CMDS.get_or_init(|| {
            std::env::var("MINELIB_CMD")
                .unwrap_or_default()
                .split('|')
                .filter_map(|c| {
                    let (t, cmd) = c.split_once(':')?;
                    Some((t.trim().parse().ok()?, cmd.trim().to_string()))
                })
                .collect()
        });
        if !cmds.is_empty() {
            self.storm.test_clock += dt;
            while let Some((at, cmd)) = cmds.get(self.storm.test_cmds) {
                if self.storm.test_clock < *at {
                    break;
                }
                self.storm.test_cmds += 1;
                self.submit(cmd);
            }
        }
        // (v263) Essais : MINELIB_TOURNE=<amplitude°>,<période s> : la vue oscille de gauche à
        // droite autour de sa direction de départ (caméra qui tourne vite : rayons repris d'une
        // image à l'autre, marge préparée hors de l'écran).
        static TURN: std::sync::OnceLock<Option<(f32, f32)>> = std::sync::OnceLock::new();
        static TURN_AT: std::sync::Mutex<Option<(f32, f32)>> = std::sync::Mutex::new(None);
        let turn = TURN.get_or_init(|| {
            let v = std::env::var("MINELIB_TOURNE").ok()?;
            let (a, p) = v.split_once(',')?;
            Some((a.trim().parse().ok()?, p.trim().parse().ok()?))
        });
        if let (Some((amp, period)), Ok(mut at)) = (*turn, TURN_AT.lock()) {
            let (base, clock) = at.get_or_insert((self.player.yaw, 0.0));
            *clock += dt;
            let phase = *clock * std::f32::consts::TAU / period.max(0.05);
            self.player.yaw = *base + amp.to_radians() * phase.sin();
        }
        // (v305) Essais : MINELIB_GLISSE=<amplitude blocs>,<période s> : le joueur glisse de
        // gauche à droite autour de sa place de départ (passer vite à côté d'un bloc : « pas net
        // quand je passe rapidement à côté »), sans tourner.
        static SLIDE: std::sync::OnceLock<Option<(f32, f32)>> = std::sync::OnceLock::new();
        static SLIDE_AT: std::sync::Mutex<Option<(Vec3, f32)>> = std::sync::Mutex::new(None);
        let slide = SLIDE.get_or_init(|| {
            let v = std::env::var("MINELIB_GLISSE").ok()?;
            let (a, p) = v.split_once(',')?;
            Some((a.trim().parse().ok()?, p.trim().parse().ok()?))
        });
        if let (Some((amp, period)), Ok(mut at)) = (*slide, SLIDE_AT.lock()) {
            if self.loading.is_none() {
                let (base, clock) = at.get_or_insert((self.player.pos, 0.0));
                *clock += dt;
                let phase = *clock * std::f32::consts::TAU / period.max(0.05);
                let right = Vec3::new(self.player.yaw.cos(), 0.0, self.player.yaw.sin());
                self.player.pos = *base + right * (amp * phase.sin());
                self.player.vel = Vec3::ZERO;
            }
        }
        // (v229) Sur un serveur de construction : les foudres que publient les arbitres.
        self.update_thor();
        // L'éclair tenu : vu de derrière (F5, caméra libre), dans le monde, au poing du
        // modèle. (v240) À la première personne aussi (« pareil, ceux du personnage, à la
        // même place qu'en F5 ») : au poing du modèle, avec les gestes de la main, dessiné
        // dans la passe de la main avec la projection du monde (le bras le cache, les murs
        // non). Avant, la main de la première personne le tenait à sa place à elle.
        let on = self.holding_zeus();
        let third = if fps { 0 } else { self.perspective };
        if third == 0 && self.light_editor.free_cam.is_none() {
            let (_, view, _) = self.view_camera();
            let aspect = self.screen_size.0 / self.screen_size.1.max(1.0);
            let proj =
                Mat4::perspective_infinite_reverse_rh(self.fov.to_radians(), aspect, 0.05);
            let place = self.first_person_zeus(view);
            self.storm
                .hold(dt, place, Some((proj * view * place, proj * view)), on);
        } else {
            let pose = self.own_pose(crate::player_model::Held::Zeus);
            let place = crate::player_model::zeus_frame(&pose)
                .unwrap_or_else(|| Mat4::from_translation(self.player.eye()));
            self.storm.hold(dt, place, None, on);
        }
        // (v244) « Il doit toujours partir du bout » : le petit éclair parti de l'éclair
        // qu'on tient suit sa pointe (elle bouge, et la main fait son geste).
        if on {
            let tip = self.zeus_tip();
            self.storm.follow_arcs(tip);
        }
        // (v241) « On voit l'éclair dans le dos du joueur, comme la bombe en FPS, quand il
        // ne l'a pas en main mais dans son inventaire » : le sien, vu de derrière (à la
        // première personne on ne voit pas son dos).
        if !on && self.zeus_on_back() && (third != 0 || self.light_editor.free_cam.is_some()) {
            let mut pose = self.own_pose(crate::player_model::Held::Zeus);
            pose.held = None;
            if let Some(place) = crate::player_model::zeus_back_frame(&pose) {
                self.storm.back_item(place, 0.37);
            }
        }
        // (v217) Les Foudres de Zeus posées au sol : le même éclair, (v235) de la même taille.
        for (at, phase) in self.items.bolts(&self.world, eye) {
            self.storm.ground_item(at, phase);
        }
        // (v222) Ceux des autres joueurs.
        let cam = self.camera_pos();
        self.update_remote_bolts(dt, cam);
        // (v251) Boules de lumière des lumières ponctuelles (l'éclat de la source, dessiné
        // comme un éclair : par-dessus l'image, caché par ce qui est devant).
        if self.lights_allowed() {
            for ball in self.lights.balls() {
                let at = ball.at;
                self.storm.bolts.push(minelib_render::BoltSeg {
                    a: [at[0], at[1], at[2], minelib_render::BOLT_BALL],
                    b: [at[0], at[1], at[2], minelib_render::BOLT_BALL],
                    color: ball.color,
                    force_a: ball.force,
                    // (v258) Pour une boule : le fondu de ses bords (`BOLT_BALL`).
                    force_b: ball.edge,
                    width_a: ball.radius,
                    width_b: ball.radius,
                    screen: 0.0,
                });
            }
        }
        // Lumière du jour montrée : moins sous un ciel couvert, plus l'éclat des éclairs.
        // (v235) L'éclat d'un éclair compte d'autant moins qu'il fait déjà clair : en plein
        // soleil, la foudre tombée tout près doublait la lumière du jour, et le sol clair
        // (la neige surtout) devenait une tache blanche où l'on ne voyait plus l'éclair.
        // La nuit et sous l'orage, rien ne change ou presque.
        let day = self.daylight * self.storm.light();
        self.daylight_shown =
            (day + self.storm.flash * 1.5 * (1.0 - 0.8 * day.clamp(0.0, 1.0))).min(2.2);
    }

    /// (v216) Ce que vise le regard, pour la foudre : le premier bloc touché à moins de
    /// 256 blocs ; sinon, (v217) le ciel si l'on regarde au-dessus de l'horizon (la foudre
    /// y monte au lieu de tomber) ; sinon le sol, 120 blocs plus loin dans la direction du
    /// regard (`None` : rien de chargé là-bas).
    fn aimed(&self) -> Option<Aim> {
        // (v281) Depuis le viseur (la caméra avance accroupi, v274).
        let (eye, dir) = (self.aim_eye(), self.player.look_dir());
        let block = raycast(
            &self.world,
            eye.into(),
            dir.into(),
            crate::storm::THOR_RANGE,
        );
        // (v235) Un joueur ou un PNJ visé, devant le premier bloc : la foudre tombe sur lui.
        let limit = block.as_ref().map_or(crate::storm::THOR_RANGE, |h| h.distance);
        if let Some(p) = self.aimed_person(limit) {
            return Some(Aim::Ground(p.feet));
        }
        if let Some(h) = block {
            return Some(Aim::Ground(eye + dir * h.distance));
        }
        if dir.y > 0.1 {
            return Some(Aim::Sky(dir));
        }
        let flat = Vec3::new(dir.x, 0.0, dir.z).normalize_or_zero();
        let p = eye + flat * 120.0;
        let (x, z) = (p.x.floor() as i32, p.z.floor() as i32);
        crate::storm::column_top(&self.world, x, z)
            .map(|top| Aim::Ground(Vec3::new(x as f32 + 0.5, (top + 1) as f32, z as f32 + 0.5)))
    }

    /// (v235) Joueur ou PNJ (foule d'essai, la nôtre ou celle d'un autre OP) visé à moins de
    /// `limit` blocs : le plus proche. (v281) L'Évidé aussi (« quand on touche un joueur, un
    /// PNJ ou un monstre ») : la foudre ne lui fait rien (il ne meurt qu'en étant mangé), mais
    /// elle s'arrête sur lui (avant, elle le traversait). Visé depuis le viseur (`aim_eye`).
    fn aimed_person(&self, limit: f32) -> Option<Aimed> {
        let (eye, dir) = (self.aim_eye(), self.player.look_dir());
        let mut best: Option<Aimed> = None;
        let mut keep = |feet: Vec3, body: Vec3, npc: Option<NpcTarget>, t: f32| {
            if t <= limit && best.as_ref().is_none_or(|b| t < b.t) {
                best = Some(Aimed { feet, body, npc, t });
            }
        };
        let mid = Vec3::Y * ARC_BODY;
        if let Some((i, t)) = self.crowd.as_ref().and_then(|c| c.aimed(eye, dir, limit)) {
            if let Some(c) = &self.crowd {
                let feet = c.npcs[i].pos;
                keep(feet, feet + mid, Some(NpcTarget::Mine(i)), t);
            }
        }
        for (owner, rc) in &self.remote_crowds {
            if let Some((i, t)) = rc.aimed(eye, dir, limit) {
                let feet = rc.npcs[i].pos;
                keep(feet, feet + mid, Some(NpcTarget::Theirs(owner.clone(), i)), t);
            }
        }
        if let Some(b) = &self.build {
            let h = crate::player::HALF_WIDTH;
            for o in b.others.values() {
                let min = o.pos - Vec3::new(h, 0.0, h);
                let max = o.pos + Vec3::new(h, crate::player::HEIGHT, h);
                if let Some(t) = crate::screen::aim_box(min, max, eye, dir, limit) {
                    keep(o.pos, o.pos + mid, None, t);
                }
            }
        }
        if let Some((id, t, _, _)) = self.evides.aimed(eye, dir, limit) {
            if let Some(e) = self.evides.list.iter().find(|e| e.id == id) {
                keep(e.pos, eye + dir * t, None, t);
            }
        }
        best
    }

    /// (v281) Quelqu'un est-il à mi-corps en `at` (à moins d'un bloc) : un autre joueur, un PNJ
    /// (le petit éclair d'un autre joueur qui s'arrête là l'a touché) ?
    fn someone_at(&self, at: Vec3) -> bool {
        let near = |feet: Vec3| (at - (feet + Vec3::Y * ARC_BODY)).length() < 1.0;
        self.crowd
            .as_ref()
            .is_some_and(|c| c.npcs.iter().any(|n| !n.hidden && near(n.pos)))
            || self
                .remote_crowds
                .values()
                .any(|rc| rc.npcs.iter().any(|n| !n.hidden && near(n.pos)))
            || self
                .build
                .as_ref()
                .is_some_and(|b| b.others.values().any(|o| near(o.pos)))
    }

    /// (v216) `/thor` : la foudre tombe là où l'on vise. En solo, celui qui joue ; sur un
    /// serveur de construction, les OP (v227 : l'arbitre du chunk visé le vérifie et la
    /// montre à tous les joueurs proches, voir `cast_lightning`).
    fn thor_command(&mut self) {
        if self.game.mode == GameMode::Fps {
            self.chat.say("Pas de foudre en partie FPS.", chat::RED);
            return;
        }
        let Some(aim) = self.aimed() else {
            self.chat.say("Rien à viser par là.", chat::RED);
            return;
        };
        self.cast_lightning(aim);
    }

    /// (v229) `/thor` ou un coup de la Foudre de Zeus vers `aim`. En solo : la foudre part
    /// tout de suite. Sur un serveur de construction : demandée à l'arbitre du chunk où
    /// elle tombe (le relais du monde ou celui de sa province), qui vérifie qu'on est OP et
    /// la publie à tous les joueurs proches, nous compris : elle part chez chacun à la
    /// réception (`update_thor`), le même éclair pour tous.
    fn cast_lightning(&mut self, aim: Aim) {
        if self.build.is_none() {
            self.throw_lightning(aim);
            return;
        }
        let (up, pos, dir) = match aim {
            Aim::Ground(pos) => (false, pos, Vec3::NEG_Y),
            Aim::Sky(dir) => (true, self.sky_origin(dir), dir.normalize_or(Vec3::Y)),
        };
        self.thor_seq = self.thor_seq.wrapping_add(1);
        let req = minelib_net::game::BuildMsg::ThorAsk {
            seq: self.thor_seq,
            chunk: [
                (pos.x.floor() as i32).div_euclid(16),
                (pos.z.floor() as i32).div_euclid(16),
            ],
            up,
            // (v318) Comptée depuis l'origine de la partie : `BuildNet::send_thor` la met en
            // place absolue.
            pos: pos.to_array().map(f64::from),
            dir: dir.to_array(),
            seed: rand::random::<u32>(),
        };
        if let Some(b) = self.build.as_mut() {
            b.send_thor(&req);
        }
    }

    /// (v229) Ce que les arbitres publient (`BuildMsg::Thor` : la foudre d'un OP, la nôtre
    /// comprise) part ici ; un refus de notre demande est dit dans le tchat.
    fn update_thor(&mut self) {
        use minelib_net::game::BuildMsg;
        let Some(b) = self.build.as_mut() else {
            return;
        };
        // (v318) Comptées depuis l'origine de la partie (`BuildNet::take_thor`).
        let inbox = b.take_thor();
        if inbox.is_empty() {
            return;
        }
        let me = b.pseudo().to_string();
        for m in inbox {
            match m {
                BuildMsg::Thor {
                    up,
                    pos,
                    dir,
                    seed,
                    by,
                    seq,
                    ..
                } => {
                    let (pos, dir) = (Vec3::from(pos.map(|v| v as f32)), Vec3::from(dir));
                    if !pos.is_finite() || !dir.is_finite() {
                        continue;
                    }
                    // La nôtre : l'éclair tenu sursaute vers elle. (v247) Ce qu'elle touche,
                    // nous l'avons frappé.
                    let mine = by.eq_ignore_ascii_case(&me) && seq == self.thor_seq;
                    let ours = mine && self.holding_zeus();
                    if up {
                        let eye_y = self.camera_pos().y;
                        self.storm.strike_up_seeded(pos, dir, eye_y, seed);
                        if ours {
                            self.storm.held.surge(pos, dir);
                        }
                    } else {
                        self.storm.strike_seeded(pos, seed);
                        if mine {
                            self.storm.mark_mine();
                        }
                        if ours {
                            let tip = self.zeus_tip();
                            self.storm
                                .held
                                .surge(tip, (pos - tip).normalize_or(Vec3::Y));
                        }
                    }
                }
                // (v244) Un coup de la Foudre de Zeus (le nôtre ou celui d'un autre), accordé
                // par le relais du monde et publié par l'arbitre du chunk.
                BuildMsg::Zeus {
                    act,
                    pos,
                    to,
                    seed,
                    by,
                    seq,
                    ..
                } => {
                    use minelib_net::proto::{ZEUS_ARC, ZEUS_SKY, ZEUS_STRIKE};
                    let f = |a: [f64; 3]| Vec3::from(a.map(|v| v as f32));
                    let (pos, to) = (f(pos), f(to));
                    if !pos.is_finite() || !to.is_finite() {
                        continue;
                    }
                    let own = by.eq_ignore_ascii_case(&me);
                    let asked = own
                        .then(|| self.zeus_pending.iter().position(|p| p.seq == seq))
                        .flatten()
                        .map(|i| self.zeus_pending.remove(i));
                    let ours = own && self.holding_zeus();
                    match act {
                        ZEUS_SKY => {
                            let eye_y = self.camera_pos().y;
                            self.storm.strike_up_seeded(pos, to, eye_y, seed);
                            if ours {
                                self.storm.held.surge(pos, to);
                            }
                        }
                        ZEUS_STRIKE => {
                            self.storm.strike_seeded(pos, seed);
                            // (v247) La nôtre : ce qu'elle touche, nous l'avons frappé
                            // (« Pistache peut être trouvé en tapant avec la foudre »).
                            if own {
                                self.storm.mark_mine();
                            }
                            if ours {
                                let tip = self.zeus_tip();
                                self.storm
                                    .held
                                    .surge(tip, (pos - tip).normalize_or(Vec3::Y));
                            }
                        }
                        ZEUS_ARC => {
                            // Le nôtre part de la pointe de l'éclair, là où elle est maintenant.
                            let from = if ours { self.zeus_tip() } else { pos };
                            // Celui d'un autre nous touche (à mi-corps).
                            let on_me = !own
                                && (to - (self.player.pos + Vec3::Y * ARC_BODY)).length() < 1.0;
                            // (v281) Touche-t-il quelqu'un (le coup sec part avec lui) ? Le
                            // nôtre : ce qu'on visait ; celui d'un autre : nous, ou quelqu'un à
                            // mi-corps à son bout.
                            let hit = match &asked {
                                Some(p) => p.hit,
                                None => on_me || self.someone_at(to),
                            };
                            self.storm.strike_arc_seeded(from, to, seed, ours, hit);
                            if let Some(target) = asked.and_then(|p| p.npc) {
                                self.hit_npc_for(target, f32::from(crate::zeus::ARC_DAMAGE));
                            } else if on_me {
                                // (v249) Il éblouit, moins que la foudre du ciel.
                                self.dazzle = self.dazzle.max(0.6);
                                self.damage(crate::zeus::ARC_DAMAGE);
                            }
                        }
                        _ => {}
                    }
                }
                BuildMsg::ThorDenied { why, seq } => {
                    self.zeus_pending.retain(|p| p.seq != seq);
                    // (Dit au plus toutes les 3 s : on clique vite avec l'éclair en main.)
                    let said = self
                        .thor_refused
                        .is_some_and(|t| t.elapsed().as_secs_f32() < 3.0);
                    if !said {
                        self.thor_refused = Some(std::time::Instant::now());
                        if why == minelib_net::game::THOR_NOT_OP {
                            self.chat
                                .say("La foudre est réservée aux OP de ce serveur.", chat::RED);
                        } else {
                            self.chat.say(
                                "La foudre n'est pas partie (relais pas prêt, ou trop de demandes) : réessaie.",
                                chat::GREY,
                            );
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// (v217) D'où part la foudre qui monte vers le ciel : la pointe de l'éclair tenu, ou
    /// la main sans lui.
    fn sky_origin(&self, dir: Vec3) -> Vec3 {
        if self.holding_zeus() {
            self.zeus_tip()
        } else {
            self.player.eye() + dir * 0.9 - Vec3::Y * 0.25
        }
    }

    /// (v217) La foudre part : elle tombe sur le point visé, ou, en visant le ciel, monte
    /// de la pointe de l'éclair tenu (de la main, sans lui) et traverse les nuages.
    fn throw_lightning(&mut self, aim: Aim) {
        match aim {
            Aim::Ground(pos) => {
                self.storm.strike(pos);
                self.storm.mark_mine();
                if self.holding_zeus() {
                    let tip = self.zeus_tip();
                    self.storm.held.surge(tip, (pos - tip).normalize_or(Vec3::Y));
                }
            }
            Aim::Sky(dir) => {
                let from = self.sky_origin(dir);
                self.storm.strike_up(from, dir, self.camera_pos().y);
            }
        }
    }

    /// (v217) Pointe de l'éclair tenu, dans le monde, là où on la voit : au poing du modèle
    /// ((v240) à la première personne aussi, avec les gestes de la main).
    fn zeus_tip(&self) -> Vec3 {
        // (v244) Son bout, là où il est à cette image (il bouge avec l'éclair).
        let top = self.storm.held.tip();
        let third = if self.game.mode == GameMode::Fps {
            0
        } else {
            self.perspective
        };
        if third == 0 && self.light_editor.free_cam.is_none() {
            let (_, view, _) = self.view_camera();
            self.first_person_zeus(view).transform_point3(top)
        } else {
            let pose = self.own_pose(crate::player_model::Held::Zeus);
            crate::player_model::zeus_frame(&pose)
                .map_or_else(|| self.player.eye(), |m| m.transform_point3(top))
        }
    }

    /// (v216) `/weather` : le temps qu'il fait ; `/weather clear|rain|storm` : un temps
    /// imposé (il arrive peu à peu : les nuages, puis la pluie, puis l'orage) pour une
    /// durée ordinaire ; `/weather fix [on|off]` : le temps ne change plus tout seul.
    fn weather_command(&mut self, arg: Option<&str>, next: Option<&str>) {
        use crate::weather::Kind;
        if self.game.mode == GameMode::Fps || self.time_h < 0.0 {
            self.chat.say("Pas de météo sur les cartes FPS.", chat::RED);
            return;
        }
        // (v229) Sur un serveur de construction : la météo est celle du serveur. Un OP la
        // change pour tous ses joueurs ; le relais le vérifie, répond (son texte s'affiche)
        // et la dit à chacun (`Event::SetWeather`).
        let online = self.build.is_some();
        let w = &mut self.storm.weather;
        let mut ask: Option<String> = None;
        let text = match arg.map(str::to_lowercase).as_deref() {
            None => format!(
                "Météo : {}{}. /weather clear|rain|storm, /weather fix [on|off]",
                w.kind().label(),
                if w.fixed { " (figée)" } else { "" }
            ),
            Some("fix" | "fixe" | "fige" | "figer" | "figée" | "lock") => {
                let on = match next.map(str::to_lowercase).as_deref() {
                    Some("on" | "oui" | "true" | "1") => true,
                    Some("off" | "non" | "false" | "0") => false,
                    _ => !w.fixed,
                };
                if online {
                    // (Le temps qu'on voit : le relais ne connaît pas celui des horloges.)
                    ask = Some(if on {
                        format!("fix {}", w.kind().key())
                    } else {
                        "auto".into()
                    });
                    String::new()
                } else {
                    w.fix(on);
                    if on {
                        format!("Météo figée : {}.", w.kind().label())
                    } else {
                        "La météo change de nouveau toute seule.".to_string()
                    }
                }
            }
            Some(word) => match Kind::parse(word) {
                Some(k) if online => {
                    ask = Some(k.key().into());
                    String::new()
                }
                Some(k) => {
                    w.force(k);
                    let what = match k {
                        Kind::Clear => "Le ciel se dégage",
                        Kind::Rain => "La pluie arrive",
                        Kind::Storm => "L'orage arrive",
                    };
                    format!("{what}{}.", if w.fixed { " (météo figée)" } else { "" })
                }
                None => {
                    self.chat.say(
                        "Usage : /weather clear|rain|storm, /weather fix [on|off]",
                        chat::RED,
                    );
                    return;
                }
            },
        };
        if let Some(reason) = ask {
            self.relay_cmds.push(minelib_net::client::Cmd::ServerAdmin {
                action: minelib_net::proto::SERVER_WEATHER,
                pseudo: String::new(),
                reason,
            });
            return;
        }
        self.chat.say(text, chat::YELLOW);
    }

    /// (v273) Pseudos des joueurs présents, pour compléter un pseudo avec Tab au tchat : ceux
    /// de la partie FPS, ceux du monde en ligne, et soi.
    fn present_pseudos(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(n) = &self.net {
            out.extend(n.pseudos());
        }
        if let Some(b) = &self.build {
            out.extend(b.others.values().map(|o| o.pseudo.clone()));
        }
        if !self.pseudo.is_empty() {
            out.push(self.pseudo.clone());
        }
        out
    }

    /// (v201) Pose du joueur qui tient `held`, pour placer la lumière de ce qu'il tient.
    /// (v240) Le modèle à la première personne : sa pose (pieds remontés comme la caméra sur
    /// une marche), et les gestes de la main (coup vers le bas, arc, objet qui remonte)
    /// autour de l'épaule de son bras droit, dans le monde (`view` : vue de cette image).
    /// L'objet tenu et le bras y sont dessinés comme en F5.
    fn first_person_hold(
        &self,
        held: crate::player_model::Held,
        view: Mat4,
    ) -> (crate::player_model::Pose, Mat4) {
        let mut p = self.own_pose(held);
        p.feet += Vec3::Y * self.step_offset;
        let gesture = crate::player_model::right_arm_matrix(&p).map_or(Mat4::IDENTITY, |arm| {
            // L'épaule : le bout z = 1 de la boîte du bras.
            let pivot = view.transform_point3(arm.transform_point3(Vec3::new(0.5, 0.5, 1.0)));
            view.inverse() * self.hand.gesture_about(pivot) * view
        });
        (p, gesture)
    }

    /// (v240) Repère de l'éclair tenu à la première personne (monde) : au poing du modèle,
    /// comme en F5, avec les gestes de la main.
    fn first_person_zeus(&self, view: Mat4) -> Mat4 {
        let (p, gesture) = self.first_person_hold(crate::player_model::Held::Zeus, view);
        gesture
            * crate::player_model::zeus_frame(&p)
                .unwrap_or_else(|| Mat4::from_translation(self.player.eye()))
    }

    /// (v272) Notre corps tel qu'il est (sans l'objet tenu), plié de `eat_bow`.
    /// Où en est notre marche : (moment du pas, ampleur du balancement).
    fn stride(&self) -> (f32, f32) {
        test_pose().map_or((self.walk_phase, self.walk_amp), |p| (p.0, p.1))
    }

    /// (v303) L'accroupi de notre modèle : le nôtre, ou celui du vol qui descend.
    fn pose_crouch(&self) -> f32 {
        self.player.crouch.max(self.fly_crouch)
    }

    fn body_pose(&self) -> crate::player_model::Pose {
        let (walk, swing) = self.stride();
        crate::player_model::Pose {
            feet: self.player.pos,
            yaw: self.player.yaw + self.eat_turn.0,
            pitch: self.player.pitch + self.eat_turn.1,
            walk,
            swing,
            crouch: self.pose_crouch(),
            light: self.hand_light,
            // (v281) Le bras tel qu'il est dessiné. (v290) La main vide pend.
            held: self.model_held(),
            skin: None,
            bow: self.eat_bow,
            air: self.anim.0,
            dir: self.anim.1,
            run: self.anim.2,
        }
    }

    /// Ce que le modèle du joueur tient (F5, miroirs, ombre, première personne). (v281) Sorti
    /// de `render` : `body_pose` (où L'Évidé vise, la tête pliée) a le même bras.
    fn model_held(&self) -> Option<crate::player_model::Held> {
        use crate::player_model::Held;
        if self.game.mode == GameMode::Fps {
            // (v177) Ce qu'on a en main, avec son vrai modèle (reflets, ombre : les deux bras
            // tendus avec une arme à feu, comme les autres nous voient).
            Some(Held::Gear(self.fps.held_weapon()))
        } else if self.game.mode.has_inventory() {
            match self.held_item() {
                Some(Item::Block(b)) => Some(Held::Block(b)),
                Some(Item::Torch(chem)) => Some(Held::Torch(
                    chem.and_then(|i| minelib_world::flame::CHEMICALS.get(i))
                        .map_or(minelib_world::flame::WOOD_FLAME, |c| c.rgb),
                )),
                Some(Item::Wand) => Some(Held::Wand([70, 60, 90])),
                Some(Item::Screen(_)) => Some(Held::Wand([30, 30, 36])),
                Some(Item::Egg) => Some(Held::Wand(crate::evide::EGG_RGB)),
                Some(Item::Bucket(_)) => Some(Held::Wand(crate::bucket::RGB)),
                // (v201) Lampe torche, verre allumé ou non.
                Some(Item::Flashlight) => Some(Held::Flashlight(self.flashlight_on)),
                Some(Item::Zeus) => Some(Held::Zeus),
                // Tête tenue (vue en 3e personne) : comme le bloc « tête ».
                Some(Item::Head(_)) => {
                    Some(Held::Block(minelib_world::registry::skull_block(false, 0)))
                }
                // (v281) « Comme quand je tiens un item, mon bras en F5, je dois le voir dans la
                // même position qu'à la première personne, aussi quand je ne tiens pas d'item » :
                // la main vide était tendue comme si elle tenait un bloc, en F5, dans les
                // miroirs et à la première personne (v247). (v290) « Arrête de pencher mon bras
                // en avant quand je n'ai pas d'item dans les mains, à la première personne et en
                // F5 » : la main vide pend le long du corps, partout pareil ; à la première
                // personne, elle ne se tend que le temps d'un geste (`hand_raise`).
                None => None,
            }
        } else {
            None
        }
    }

    fn own_pose(&self, held: crate::player_model::Held) -> crate::player_model::Pose {
        let (walk, swing) = self.stride();
        crate::player_model::Pose {
            feet: self.player.pos,
            yaw: self.player.yaw + self.eat_turn.0,
            pitch: self.player.pitch + self.eat_turn.1,
            walk,
            swing,
            crouch: self.pose_crouch(),
            light: self.hand_light,
            held: Some(held),
            skin: None,
            bow: self.eat_bow,
            air: self.anim.0,
            dir: self.anim.1,
            run: self.anim.2,
        }
    }

    fn open_light_editor(&mut self) {
        if !self.lights_allowed() {
            self.chat
                .say("Pas de lumières en partie FPS en ligne.", chat::RED);
            return;
        }
        if self.build.is_some() {
            // Déjà demandé (réponse en route) : pas une deuxième fois.
            if self
                .light_op_probe
                .is_some_and(|t| t.elapsed().as_secs_f32() < 5.0)
            {
                return;
            }
            self.light_op_probe = Some(std::time::Instant::now());
            self.relay_cmds.push(minelib_net::client::Cmd::ServerAdmin {
                action: minelib_net::proto::SERVER_LIST,
                pseudo: String::new(),
                reason: String::new(),
            });
            return;
        }
        self.light_editor.open = true;
        self.inventory.close();
    }

    /// (v224) Lumières de l'éditeur sur un serveur de construction (voir `lightnet`) : ce
    /// que publie l'arbitre de chaque chunk arrive dans la scène (les autres OP), ce que
    /// l'éditeur y change part en demandes. À chaque image, même éditeur ouvert.
    fn update_light_sync(&mut self, dt: f32) {
        use minelib_net::game::BuildMsg;
        let Some(b) = self.build.as_mut() else {
            self.light_sync = None;
            self.light_inbox.clear();
            return;
        };
        let scene = &self.lights.scene;
        let sync = self
            .light_sync
            .get_or_insert_with(|| crate::lightnet::LightSync::new(b.pseudo(), scene));
        // (Celles que `update_items` a mises de côté d'abord : elles sont arrivées avant.)
        // (v318) Comptées depuis l'origine de la partie (`BuildNet::take_items`).
        let (more, rest): (Vec<BuildMsg>, Vec<BuildMsg>) = b
            .take_items()
            .into_iter()
            .partition(|m| matches!(m, BuildMsg::Light { .. } | BuildMsg::LightDenied { .. }));
        b.untake_items(rest);
        let mut msgs = std::mem::take(&mut self.light_inbox);
        msgs.extend(more);
        let mut heard = crate::lightnet::Heard::default();
        for m in &msgs {
            let h = self
                .lights
                .with_scene(|scene, new_id| sync.hear(m, scene, new_id));
            heard.changed |= h.changed;
            heard.denied |= h.denied;
            heard.removed.extend(h.removed);
        }
        if heard.changed {
            self.lights.touch_scene();
            let sel = crate::lightnet::follow_selection(self.light_editor.selected, &heard.removed);
            if sel != self.light_editor.selected {
                self.light_editor.selected = sel;
                self.light_editor.editing_name = false;
            }
        }
        if heard.denied
            && self
                .light_denied_said
                .is_none_or(|t| t.elapsed().as_secs_f32() > 3.0)
        {
            self.light_denied_said = Some(std::time::Instant::now());
            self.chat.say(
                "Lumières : changement refusé par le serveur (réservé à ses OP) ; elles reviennent comme les autres les ont.",
                chat::RED,
            );
        }
        for r in sync.outgoing(&self.lights.scene, self.lights.scene_gen, dt) {
            b.send_light(&r);
        }
    }

    /// (v224) `/lumieres effacer` : toutes les lumières du monde retirées (en ligne, pour tout
    /// le monde : réservé aux OP).
    fn clear_lights(&mut self) {
        let n = self.lights.scene.len();
        self.lights.scene.clear();
        self.light_editor.selected = None;
        self.light_editor.editing_name = false;
        self.lights.touch_scene();
        self.chat.say(
            format!("{n} lumière(s) retirée(s) de ce monde."),
            chat::YELLOW,
        );
    }

    /// Réponse du relais à la demande de `open_light_editor` (`op` : on est OP du serveur).
    /// Renvoie vrai si c'était bien elle (son texte n'est alors pas affiché tel quel).
    pub fn light_op_answer(&mut self, op: bool) -> bool {
        // (v227) Et à `/npc stresstest on` en ligne.
        if let Some(asked) = self.crowd_probe.take() {
            if asked.elapsed().as_secs_f32() <= 10.0 {
                if op {
                    self.start_crowd_online();
                } else {
                    self.chat
                        .say("/npc stresstest : réservé aux OP de ce serveur.", chat::RED);
                }
                return true;
            }
        }
        let Some(asked) = self.light_op_probe.take() else {
            return false;
        };
        if asked.elapsed().as_secs_f32() > 10.0 {
            self.light_clear_asked = false;
            return false;
        }
        self.light_op_ok = op;
        // (v224) `/lumieres effacer` en attente de la réponse.
        if std::mem::take(&mut self.light_clear_asked) {
            if op {
                self.clear_lights();
            } else {
                self.chat
                    .say("/lumieres effacer : réservé aux OP de ce serveur.", chat::RED);
            }
            return true;
        }
        if !op {
            self.chat.say(
                "Éditeur de lumières : réservé aux OP de ce serveur.",
                chat::RED,
            );
        } else if self.build.is_some() && self.game.mode != GameMode::Fps {
            self.light_editor.open = true;
            self.inventory.close();
        }
        true
    }

    /// Position de la caméra libre de l'éditeur de lumières, sinon les yeux du joueur.
    fn camera_pos(&self) -> Vec3 {
        self.light_editor
            .free_cam
            .map_or_else(|| self.player.eye(), |c| c.pos)
    }

    /// Caméra de l'image : (position, vue, direction du regard). Caméra libre de l'éditeur,
    /// sinon les yeux du joueur, ou derrière lui (F5).
    /// (v278) La caméra à la première personne hors mode FPS : les yeux, la marche montée en
    /// douceur (`step_offset`), la tête pliée pour manger (`bow_shift`, v272) et, accroupi, la
    /// tête qui avance : « quand je sneak, c'est pas mon corps qui doit reculer, c'est ma caméra
    /// qui doit avancer proportionnellement à la tête » (`first_person_ahead`). Jamais dans un
    /// bloc devant soi : elle s'arrête à 0,1 bloc de lui.
    fn first_person_eye(&self) -> Vec3 {
        let mut eye = self.player.eye() + Vec3::Y * self.step_offset;
        if self.eat_bow != 0.0 {
            eye += crate::player_model::bow_shift(&self.body_pose());
        }
        let ahead = crate::player_model::first_person_ahead(self.player.yaw, self.player.crouch);
        let len = ahead.length();
        if len > 1e-4 {
            let dir = ahead / len;
            let room = raycast(&self.world, eye.into(), dir.into(), len + 0.1)
                .map_or(len, |h| (h.distance - 0.1).max(0.0));
            eye += dir * len.min(room);
        }
        eye
    }

    /// (v278) D'où l'on vise (blocs, écrans, cadres) : la caméra à la première personne hors
    /// mode FPS (elle avance accroupi : on vise ce qu'on voit au viseur), les yeux du joueur
    /// sinon (troisième personne, mode FPS, caméra libre).
    fn aim_eye(&self) -> Vec3 {
        let first = self.perspective == 0 && self.light_editor.free_cam.is_none();
        if first && self.game.mode != GameMode::Fps {
            self.first_person_eye()
        } else {
            self.player.eye()
        }
    }

    fn view_camera(&self) -> (Vec3, Mat4, Vec3) {
        if let Some(c) = self.light_editor.free_cam {
            return (c.pos, c.view(), c.look());
        }
        let fps_mode = self.game.mode == GameMode::Fps;
        let third = if fps_mode { 0 } else { self.perspective };
        if third == 0 {
            // (v268) Avec la marche montée ou descendue en douceur (`smooth_steps`), comme la
            // vue : depuis la v254, l'image est dessinée depuis cette position (matrices
            // comptées depuis la caméra, sans la translation de la vue) ; sans elle, la caméra
            // sautait d'un coup sur une dalle, et la vue (tri des chunks, lumières) ne tombait
            // plus juste avec l'image.
            // (v272) Plié pour manger : la caméra plonge avec la tête. (v278) Accroupi, elle
            // avance avec elle (voir `first_person_eye`) ; en mode FPS, les yeux du joueur.
            let eye = if fps_mode {
                self.player.eye() + Vec3::Y * self.step_offset
            } else {
                self.first_person_eye()
            };
            // (v290) En mordant L'Évidé, le regard tourne vers le milieu du membre mordu, puis
            // revient (`eat_turn`).
            if self.eat_turn != (0.0, 0.0) {
                let y = self.player.yaw + self.eat_turn.0;
                let p = (self.player.pitch + self.eat_turn.1).clamp(-1.55, 1.55);
                let dir = Vec3::new(y.sin() * p.cos(), p.sin(), -y.cos() * p.cos());
                return (eye, Mat4::look_to_rh(eye, dir, Vec3::Y), dir);
            }
            (eye, self.view_with_punch(), self.player.look_dir())
        } else {
            let eye = self.player.eye();
            let look = self.player.look_dir();
            // (v305) « Je dois pouvoir descendre ma caméra jusqu'au sol en F5 » : plus de limite
            // (la v295 la gardait à 55° au plus sous les yeux). Ce que l'écran coupe alors du
            // corps, dans ses jambes, est dessiné plein (`player_model::cut`).
            let back = if third == 1 { -look } else { look };
            let dist = raycast(&self.world, eye.into(), back.into(), THIRD_PERSON_DIST)
                .map_or(THIRD_PERSON_DIST, |h| (h.distance - 0.25).max(0.3));
            let cam = eye + back * dist;
            (cam, Mat4::look_to_rh(cam, -back, Vec3::Y), -back)
        }
    }

    /// (v215) Paramètres graphiques → Lumières : les réglages de toutes les lumières de la
    /// partie, appliqués tout de suite (voir `light_editor::Editor::settings_screen`).
    /// Renvoie vrai quand la fenêtre se ferme.
    pub fn light_settings_ui(&mut self, ui: &mut Ui, size: (f32, f32), input: &Input) -> bool {
        self.light_editor
            .settings_screen(ui, input, &mut self.lights, size)
    }

    /// Éditeur de lumières (s'il est ouvert) : guides et poignées dans le monde, puis son
    /// panneau, par-dessus le HUD.
    pub fn light_editor_ui(
        &mut self,
        ui: &mut Ui,
        size: (f32, f32),
        input: &Input,
        stats: minelib_render::DynStats,
    ) {
        // (v220) Interface masquée (F1) : le panneau aussi.
        if !self.light_editor.open || self.hud_off() {
            return;
        }
        let (pos, view, look) = self.view_camera();
        let proj = Mat4::perspective_infinite_reverse_rh(
            self.fov.to_radians(),
            size.0 / size.1.max(1.0),
            0.05,
        );
        let cam = crate::light_editor::Cam {
            pos,
            look,
            view_proj: proj * view,
            size,
        };
        let hint = self
            .keys
            .get(Action::LightEditor)
            .map_or_else(|| "L".to_string(), |b| b.name(input));
        self.light_editor
            .ui(ui, input, &mut self.lights, &cam, stats, &hint);
    }

    /// Objet en main (aucun en mode FPS : pas d'inventaire).
    fn held_item(&self) -> Option<Item> {
        self.game
            .mode
            .has_inventory()
            .then(|| self.inventory.selected_item())
            .flatten()
    }

    /// Éclairage approximatif de la main : ciel (abrité ou non), jour/nuit, torche tenue.
    fn light_at_player(&self, held: Option<Item>) -> [f32; 3] {
        let mut l = sky_light(&self.world, self.player.eye(), self.daylight_shown);
        if let Some(Item::Torch(_)) = held {
            for (c, w) in l.iter_mut().zip([1.0, 0.78, 0.5]) {
                *c = c.max(w);
            }
        }
        l
    }

    fn fly_allowed(&self) -> bool {
        (self.game.mode == GameMode::Creative || self.game.can_fly)
            && self.game.mode != GameMode::Spectator
    }

    /// Dégâts de chute (survie) : un demi-cœur par bloc au-delà de 3.
    /// Journal des arrêts : à chaque fin de course (« courir » ou « avancer » lâchée),
    /// mesure le chemin encore parcouru dans le sens de la course jusqu'à l'arrêt, et dit
    /// s'il y a un mur devant. C'est ce que le joueur voit (« ça continue d'avancer ») ;
    /// les touches seules ne le disent pas.
    fn measure_stop(&mut self, dt: f32, fwd: bool, run: bool, fwd_released: bool, other: bool) {
        let pos = self.player.pos;
        let flat = Vec3::new(self.player.vel.x, 0.0, self.player.vel.z);
        let both = fwd && run;
        if both && self.run_from.is_none() {
            // Nouvelle course : la mesure d'avant n'a plus de sens.
            self.run_from = Some(pos);
            self.stop_meas = None;
        }
        let run_released = self.run_was && !run && fwd;
        self.run_was = run;
        if let Some(from) = self.run_from.filter(|_| !both) {
            self.run_from = None;
            if (run_released || fwd_released) && flat.length() > 2.0 {
                self.stop_meas = Some(StopMeas {
                    at: pos,
                    dir: flat.normalize(),
                    speed: flat.length(),
                    t: 0.0,
                    run: (pos - from).length(),
                    first: if run_released { "courir" } else { "avancer" },
                    fwd_up: !fwd,
                    other: false,
                    air: false,
                });
            }
        }
        let Some(mut s) = self.stop_meas.take() else {
            return;
        };
        s.t += dt;
        s.other |= other;
        s.air |= !self.player.flying && !self.player.on_ground;
        // « Avancer » lâchée puis renfoncée : il repart, ce n'est plus un arrêt.
        let again = s.fwd_up && fwd;
        s.fwd_up |= !fwd;
        let along = flat.dot(s.dir);
        let stopped = along < 0.05;
        if !stopped && !again && s.t < 1.5 {
            self.stop_meas = Some(s);
            return;
        }
        let wall = match self.player.obstacle_ahead(&self.world, 8.0) {
            Some(d) if d < 0.01 => " ; contre un mur".to_string(),
            Some(d) => format!(" ; mur devant à {d:.2} bloc"),
            None => String::new(),
        };
        // Ce qui allonge un arrêt sans que les touches y soient pour rien.
        let why = match (s.other, s.air) {
            (true, true) => " (autre touche de déplacement tenue, et en l'air)",
            (true, false) => " (autre touche de déplacement tenue)",
            (false, true) => " (en l'air pendant l'arrêt)",
            (false, false) => "",
        };
        log::info!(
            "arrêt {} : {:.2} bloc parcouru après « {} » lâchée{}, parti de {:.1} blocs/s \
             après une course de {:.1} blocs{wall}{why}",
            if self.player.flying {
                "en vol"
            } else {
                "au sol"
            },
            (pos - s.at).dot(s.dir),
            s.first,
            if stopped {
                format!(", arrêté en {:.2} s", s.t)
            } else if again {
                format!(", avancer renfoncée après {:.2} s", s.t)
            } else {
                format!(
                    ", encore à {along:.1} blocs/s après {:.1} s (il a continué)",
                    s.t
                )
            },
            s.speed,
            s.run,
        );
    }

    /// Avance la simulation du joueur par pas fixes de `SIM_TICK`, puis montre une
    /// position interpolée entre les deux derniers pas (mouvement fluide à toute cadence).
    fn simulate(&mut self, m: &MoveInput, dt: f32) {
        // Reprend la vraie position, sauf si quelqu'un a déplacé le joueur depuis
        // (téléportation, réapparition) : on part alors de là.
        if let Some(s) = self.sim.take() {
            if self.player.pos == s.shown_pos && self.player.crouch == s.shown_crouch {
                self.player.pos = s.pos;
                self.player.crouch = s.crouch;
            } else {
                self.prev_pos = self.player.pos;
                self.prev_crouch = self.player.crouch;
            }
        } else {
            self.prev_pos = self.player.pos;
            self.prev_crouch = self.player.crouch;
        }
        if let Some(n) = &mut self.net {
            n.reconcile(&mut self.player, &self.world, SIM_TICK);
        }
        let dead_input = MoveInput {
            fps: true,
            ..Default::default()
        };
        let frozen_input;
        let m = if self.net_dead {
            &dead_input
        } else if self.net.as_ref().is_some_and(|n| n.frozen()) {
            frozen_input = crate::netgame::still(m);
            &frozen_input
        } else {
            m
        };
        self.pending_jump |= m.jump_pressed;
        self.tick_acc = (self.tick_acc + dt).min(SIM_TICK * MAX_TICKS as f32);
        // Retard mesuré de la touche « avancer » (0 : rien à compenser).
        let fps_mode = self.game.mode == GameMode::Fps;
        // (v185) Mode FPS en ligne : l'hôte simule le joueur à partir des entrées, le chemin
        // dû est donc rendu par les entrées (voir `keycomp`), et il n'y a pas de ralenti
        // d'avance (il ne passerait pas le réseau). (v209) Contre des bots (ou seul), c'est ce
        // jeu qui fait foi pour son joueur (`NetGame::on_tick` prend sa position) : il est
        // replacé en douceur comme en construction, au lieu de reculer à pied — un recul que
        // le journal montrait (« le joueur avance sans touche depuis 0,3 s », juste après
        // chaque retour) et qui, en tournant la souris, le faisait tourner en rond.
        let by_input = self.online_match();
        let fwd_action = if fps_mode {
            Action::FpsForward
        } else {
            Action::Forward
        };
        let key_lag = self
            .key_lag
            .filter(|s| s.on && self.keys.get(fwd_action) == Some(crate::keys::Bind::Key(s.key)))
            .map_or(0.0, |s| s.lag);
        // Le joueur peut-il bouger (ni mort, ni figé au départ d'une manche) ?
        let can_move = !self.net_dead && !self.net.as_ref().is_some_and(|n| n.frozen());
        while self.tick_acc >= SIM_TICK {
            self.prev_pos = self.player.pos;
            self.prev_crouch = self.player.crouch;
            let mut mt = *m;
            mt.jump_pressed = std::mem::take(&mut self.pending_jump);
            // Mode FPS : on court moins vite avec une arme lourde (comme dans CS).
            self.player.carry = if self.game.mode == GameMode::Fps {
                self.fps.carry()
            } else {
                250
            };
            // En ligne (FPS) : les autres joueurs sont solides.
            self.player.others = self.net.as_ref().map_or_else(Vec::new, |n| n.obstacles());
            // « Courir » vient d'être lâchée après un long appui sur « avancer » : ralenti
            // (jamais arrêté) le temps de savoir si « avancer » suit (voir `keycomp`).
            if key_lag > 0.0 && !by_input && mt.forward > 0.0 {
                let f = self.key_comp.factor();
                if f < 1.0 {
                    mt.forward *= f;
                    // (Sauf s'il court encore, par un double appui sur « avancer ».)
                    if !mt.sprint {
                        self.player.brake_forward(f, SIM_TICK);
                    }
                }
            }
            // Mode FPS en ligne : le chemin dû après le relâchement annoncé en retard est rendu
            // en demandant de reculer, comme si le joueur l'avait fait — l'hôte reçoit la même
            // entrée et arrive au même endroit. Sa visée et ses touches passent devant.
            // (v209) Droit vers là où le doigt s'est levé, quelle que soit la vue (la
            // direction de l'entrée la plus proche, parmi huit) ; lâché assez tôt pour que la
            // glissade s'arrête juste là ; jamais en l'air, ni vers un bord d'où il tomberait.
            if key_lag > 0.0 && by_input && self.key_comp.back_input() {
                let (owed, dir) = self.key_comp.owed();
                let speed = self.player.vel.x * dir.x + self.player.vel.z * dir.z;
                let stop = crate::player::Player::fps_stop_distance(speed, SIM_TICK);
                let behind = self.player.pos + dir * 0.35;
                let free = can_move
                    && mt.forward == 0.0
                    && mt.strafe == 0.0
                    && !mt.jump
                    && !self.fps_aiming
                    && self.player.on_ground;
                if free && owed > stop && self.player.supported_at(&self.world, behind) {
                    let (f, s) = crate::keycomp::input_towards(dir, self.player.yaw);
                    mt.forward = f;
                    mt.strafe = s;
                } else {
                    self.key_comp.drop_owed();
                }
            }
            let before = self.player.pos;
            self.player.update(&self.world, &mt, SIM_TICK);
            // Touche « avancer » annoncée relâchée en retard : ce qui a été avancé pendant
            // le retard est rendu (voir `crate::keycomp`).
            let nudge = self.key_comp.tick(
                SIM_TICK,
                key_lag,
                by_input,
                // (v185) La touche elle-même, pas l'entrée : en FPS on s'arrête net en
                // appuyant sur « reculer » sans lâcher « avancer » (contre-strafe), et le
                // chemin dû ne doit pas être rendu une deuxième fois. Seul le chemin fait
                // vers l'avant compte de toute façon.
                self.fwd_key,
                self.run_key,
                self.player.yaw,
                self.player.pos - before,
                self.player.pos.y,
                self.player.on_ground,
                !self.player.flying && !self.player.on_ground && self.player.vel.y < 0.0,
            );
            if nudge != crate::keycomp::Nudge::default() {
                self.player.nudge(&self.world, nudge.back, nudge.lift);
            }
            if let Some((secs, blocks)) = self.key_comp.last.take() {
                log::info!(
                    "touche avancer : relâchement annoncé en retard, {:.0} ms repris \
                     ({blocks:.2} bloc)",
                    secs * 1000.0
                );
            }
            if let Some(n) = &mut self.net {
                n.on_tick(&self.world, &self.player, &mt, SIM_TICK);
            }
            self.tick_acc -= SIM_TICK;
            self.check_fall();
            // Dans la lave (comme dans Minecraft) : 4 points de vie toutes les demi-secondes.
            if self.player.swimming == Some(minelib_world::registry::Fluid::Lava) {
                self.lava_burn -= SIM_TICK;
                if self.lava_burn <= 0.0 {
                    self.lava_burn = 0.5;
                    self.damage(4);
                }
            } else {
                self.lava_burn = 0.0;
            }
        }
        let a = self.tick_acc / SIM_TICK;
        let s = SimState {
            pos: self.player.pos,
            crouch: self.player.crouch,
            shown_pos: self.prev_pos.lerp(self.player.pos, a),
            shown_crouch: self.prev_crouch + (self.player.crouch - self.prev_crouch) * a,
        };
        self.player.pos = s.shown_pos;
        self.player.crouch = s.shown_crouch;
        self.sim = Some(s);
    }

    /// Change le mode de jeu du joueur.
    pub fn set_mode(&mut self, mode: GameMode) {
        // (v322) Le mode FPS se joue partout, loin du centre du monde aussi (v309 : refusé
        // au-delà de 32 768 blocs) : la partie contre les bots repart d'abord d'une origine
        // proche de ses départs s'il le faut (`solo_fps_far`), la partie FPS d'un serveur se
        // joue comptée depuis l'origine du jeu de son hôte (`join_build_match`).
        // (v232) Un mode choisi en jeu (autre que FPS) est celui que le monde garde.
        if mode != GameMode::Fps && mode != GameMode::Spectator {
            self.saved_mode = None;
        }
        // (v243) Serveur de construction : en mode FPS, on entre dans la partie FPS qu'un OP y
        // mène ; en en sortant, on la quitte (celui qui la mène la ferme).
        let mut join = None;
        if self.build.is_some() {
            if mode == GameMode::Fps {
                if !self.in_build_match() {
                    join = self
                        .build_match
                        .clone()
                        .filter(|h| !h.eq_ignore_ascii_case(&self.pseudo));
                }
            } else if self.in_build_match() {
                let host = self.net.as_ref().is_some_and(|n| n.is_host());
                self.relay_cmds.push(minelib_net::client::Cmd::ServerAdmin {
                    action: minelib_net::proto::SERVER_MATCH,
                    pseudo: String::new(),
                    reason: if host { "close" } else { "leave" }.into(),
                });
                self.end_build_match();
                self.chat.say(
                    if host {
                        "Partie FPS du serveur fermée."
                    } else {
                        "Tu as quitté la partie FPS du serveur."
                    },
                    chat::YELLOW,
                );
            }
        }
        // (v172) Mode FPS en solo : partie contre des bots (lancée à l'image suivante) ;
        // on la quitte en changeant de mode. (v174) Sauf en spectateur : on la regarde,
        // les bots continuent entre eux, et on y revient en repassant en mode FPS (à la
        // manche suivante).
        let solo = self.net.as_ref().is_some_and(|n| n.is_solo());
        if mode == GameMode::Fps {
            self.bots_auto = true;
            if let Some(n) = self.net.as_mut().filter(|_| solo) {
                if !n.set_away(false) && !self.net_dead {
                    // Revenu en pleine manche : mort jusqu'à la suivante.
                    self.net_dead = true;
                    self.fps.hp = 0;
                    self.fps.buy_open = false;
                }
            }
        } else if mode == GameMode::Spectator && solo {
            if let Some(n) = self.net.as_mut() {
                n.set_away(true);
            }
            self.spectating = None;
            self.fps.buy_open = false;
            self.chat.say(
                "Les bots continuent la partie. (gm fps : la reprendre là où tu es.)",
                chat::YELLOW,
            );
        } else {
            self.stop_bots();
        }
        self.game.mode = mode;
        if mode.mortal() && !self.game.can_fly {
            self.player.flying = false;
        }
        if mode == GameMode::Creative {
            self.game.health = MAX_HEALTH;
        }
        if !mode.has_inventory() {
            self.inventory.close();
        }
        if mode == GameMode::Spectator {
            self.player.flying = true;
        } else if self.player.noclip {
            // Sortie du mode spectateur : vol gardé si permis, sinon on tombe.
            self.player.noclip = false;
            self.player.flying = mode == GameMode::Creative || self.game.can_fly;
        }
        self.fall_from = None;
        self.chat
            .say(format!("Mode de jeu : {}", mode.label()), chat::YELLOW);
        if let Some(host) = join {
            self.join_build_match(&host);
        }
    }

    /// (v243) La partie FPS du serveur de construction s'ouvre (menée par `host`) ou se ferme :
    /// celle qu'on a demandée démarre chez soi (on en est l'hôte) ; celle d'un autre, on y entre
    /// si l'on est en mode FPS ; fermée, on en sort.
    /// (v322) `origin` : l'origine (en chunks) du jeu de son hôte, depuis laquelle la partie
    /// compte (on y entre compté depuis elle).
    fn build_match_event(&mut self, host: &str, open: bool, origin: [i32; 2]) {
        if open {
            let mine = !self.pseudo.is_empty() && host.eq_ignore_ascii_case(&self.pseudo);
            self.build_match = Some(host.to_string());
            self.build_match_origin = ChunkPos {
                x: origin[0],
                z: origin[1],
            };
            if mine {
                if !self.in_build_match() {
                    if self.match_opening && self.game.mode == GameMode::Fps {
                        self.start_build_match();
                    } else {
                        // Sorti du mode FPS entre-temps, ou une partie à son nom que ce jeu
                        // ne mène plus (reconnexion) : elle se referme.
                        self.relay_cmds.push(minelib_net::client::Cmd::ServerAdmin {
                            action: minelib_net::proto::SERVER_MATCH,
                            pseudo: String::new(),
                            reason: "close".into(),
                        });
                    }
                }
                self.match_opening = false;
            } else if self.game.mode == GameMode::Fps {
                if !self.in_build_match() {
                    self.join_build_match(host);
                }
            } else {
                self.chat.say(
                    format!(
                        "{host} mène une partie FPS sur ce serveur : les joueurs en mode FPS la \
                         rejoignent."
                    ),
                    chat::YELLOW,
                );
            }
        } else {
            self.build_match = None;
            if self.in_build_match() {
                self.end_build_match();
                self.chat
                    .say("La partie FPS du serveur est finie.", chat::YELLOW);
            }
        }
    }

    /// (v209) `/bot` : sans partie, en lance une ; sinon la complète jusqu'au nombre de
    /// joueurs choisi (`/bot maxbot`, sinon celui de la carte), sans la recommencer.
    fn fill_bots(&mut self) {
        let Some(n) = self.net.as_mut().filter(|n| n.is_solo()) else {
            if self.online_match() {
                return;
            }
            // Nouvelle partie (commencée à l'image suivante).
            self.bots_auto = true;
            return;
        };
        // Partie finie : on en refait une.
        if n.phase() == minelib_net::game::phase::GAME_OVER {
            self.stop_bots();
            self.bots_auto = true;
            return;
        }
        let players = crate::bots::saved_players()
            .unwrap_or_else(|| fps_map_of(&self.dir, &self.world).players() as usize);
        let before = n.bot_count();
        let now = n.set_bot_count(players.saturating_sub(1));
        let t = if now == before {
            format!("Déjà {now} bots dans la partie (/bot maxbot <2-20> pour en changer).")
        } else {
            format!("{now} bots dans la partie.")
        };
        self.chat.say(t, chat::YELLOW);
    }

    /// Partie FPS en ligne (pas la partie solo contre des bots).
    /// (v220) Éclairs tenus qui crépitent (voir `audio::crackles`) : le sien, depuis sa
    /// main, entendu de là où est la caméra (à droite et devant à la première personne).
    fn update_crackles(&mut self, active: bool) {
        let mut list = Vec::new();
        let mine = active
            && self.loading.is_none()
            && self.game.mode.has_inventory()
            && self.game.mode != GameMode::Fps
            && self.held_item() == Some(Item::Zeus);
        let (cam, view, _) = self.view_camera();
        if mine {
            // (v247) « Si un objet fait un bruit comme l'éclair, il le fait aussi en F5 » : le
            // bruit part toujours de l'objet tenu là où il est dans le monde (sa pointe, au
            // poing du modèle : `zeus_tip`, à la première personne comme en F5), entendu de la
            // caméra. Avant : d'un point devant les yeux, qu'en F5 la caméra laissait loin
            // devant elle.
            let hand = self.zeus_tip();
            let v = view.transform_point3(hand);
            list.push(crate::audio::CrackleAt {
                id: 1,
                dir: v.normalize_or(Vec3::NEG_Z).to_array(),
                gain: crate::audio::crackle_gain(hand.distance(cam)),
            });
        }
        // (v222) Ceux des autres joueurs (construction en ligne) à moins de `ZEUS_HEAR` blocs,
        // depuis leur poing : les huit plus proches.
        if active && self.loading.is_none() {
            let mut near: Vec<(f32, u64, Vec3)> = self
                .build
                .iter()
                .flat_map(|b| b.others.values())
                .filter(|o| o.held == Some(minelib_net::poses::Held::Zeus))
                .filter_map(|o| {
                    let pose = self.other_pose(o, Some(crate::player_model::Held::Zeus));
                    let p = crate::player_model::zeus_frame(&pose)?.transform_point3(Vec3::ZERO);
                    let d = p.distance(cam);
                    (d < crate::audio::ZEUS_HEAR).then(|| (d, crackle_id(&o.pseudo), p))
                })
                .collect();
            near.sort_by(|a, b| a.0.total_cmp(&b.0));
            for (d, id, p) in near.into_iter().take(8) {
                let v = view.transform_point3(p);
                list.push(crate::audio::CrackleAt {
                    id,
                    dir: v.normalize_or(Vec3::NEG_Z).to_array(),
                    gain: crate::audio::crackle_gain(d),
                });
            }
        }
        if !list.is_empty() || self.crackling {
            self.crackling = !list.is_empty();
            crate::audio::crackles(list);
        }
    }

    /// (v220) Chargement du monde ; fini (`loading_done` : la seule porte, voir `LOAD_MIN_S`),
    /// on apparaît : chez les autres en construction en ligne (sa position part), à l'hôte
    /// d'une partie FPS (il se présente).
    fn check_loaded(&mut self, renderer: &mut Renderer) {
        // (v283) Un monde qui a des lumières de l'éditeur : leurs shaders aussi (lancés tout
        // de suite, ils se compilent pendant que le monde se charge).
        let lights = self.loading.is_some()
            && !self.lights.scene.is_empty()
            && !(self.net.is_some() && self.game.mode == GameMode::Fps);
        if lights {
            renderer.warm_dyn_lights();
        }
        if let Some(l) = &mut self.loading {
            let p = self.player.pos;
            // (v283) Toute la distance de rendu (avant : 4 chunks autour de soi).
            l.shown = self.streamer.shown_within(p.x, p.z, self.streamer.radius);
            l.shaders = renderer.world_ready() && (!lights || renderer.dyn_lights_ready());
            // (v247) « On ne doit pas pouvoir rejoindre tant que tout n'est pas chargé (il y
            // avait un orage et les nuages étaient mal chargés) » : en ligne, aussi la météo du
            // serveur (elle arrive par le relais, juste après l'entrée).
            let weather = self.build.is_none() || self.weather_synced;
            // (v295) La barre avance au rythme de toute l'attente, shaders compris.
            let elapsed = l.since.elapsed().as_secs_f32();
            let shaders_left = if l.shaders {
                0.0
            } else {
                renderer.shaders_left(lights)
            };
            let left = load_left(elapsed, l.shown, shaders_left, weather);
            l.bar = l.bar.max(load_bar(elapsed, left));
            if loading_done(l, weather) {
                log::info!(
                    "monde chargé en {:.1} s ({} chunks affichés : toute la distance de rendu)",
                    l.since.elapsed().as_secs_f32(),
                    l.shown.1
                );
                self.loading = None;
                if let Some(n) = &mut self.net {
                    n.loaded();
                    // (v258) Les portes posées pour la partie de la carte (copies de la partie).
                    n.set_map_doors(fps_map_of(&self.dir, &self.world).door_list);
                }
            }
        }
        if let Some(b) = &mut self.build {
            b.hidden = self.loading.is_some();
        }
    }

    /// (v283) Arrivé ailleurs (téléporté, réapparu loin) : si le monde n'y est pas tout chargé,
    /// l'écran de chargement revient jusqu'à ce qu'il le soit (`loading_done`). Pas en partie
    /// FPS (les départs sont dans la carte, déjà chargée).
    fn arrive_elsewhere(&mut self) {
        if self.net.is_some() || self.loading.is_some() {
            return;
        }
        let p = self.player.pos;
        let (shown, all) = self.streamer.shown_within(p.x, p.z, self.streamer.radius);
        if shown < all {
            self.loading = Some(Loading::new());
        }
    }

    /// (v220) Monde en cours de chargement : part déjà faite (0 à 1), et shaders prêts.
    /// (v295) La part est celle du temps d'attente estimé (`load_bar`), plus celle des chunks.
    pub fn loading_progress(&self) -> Option<(f32, bool)> {
        self.loading.as_ref().map(|l| (l.bar.clamp(0.0, 1.0), l.shaders))
    }

    pub fn online_match(&self) -> bool {
        self.net.as_ref().is_some_and(|n| !n.is_solo())
    }

    /// (v243) Partie FPS en ligne du matchmaking (classée) : pas la partie solo contre des
    /// bots, ni celle d'un serveur de construction (le relais n'en sait que les messages, le
    /// monde reste celui du serveur).
    pub fn ranked_match(&self) -> bool {
        self.online_match() && self.build.is_none()
    }

    /// (v243) Dans la partie FPS du serveur de construction (son hôte ou un de ses joueurs).
    fn in_build_match(&self) -> bool {
        self.build.is_some() && self.online_match()
    }

    /// (v243) Ouvre la partie FPS du serveur de construction dont ce jeu est l'hôte (le relais
    /// vient de l'accepter : ce joueur est OP) : autour de lui (départs à 10 blocs de part et
    /// d'autre si le monde n'a pas de zones), complétée par des bots, commencée tout de suite.
    /// Les bots relèvent une copie des chunks chargés autour.
    fn start_build_match(&mut self) {
        let Some(link) = self.build.as_ref().map(|b| b.world_link()) else {
            return;
        };
        let map = fps_map_of(&self.dir, &self.world);
        let spawn = self.player.pos;
        let mut starts = vec![spawn, spawn - Vec3::X * 10.0, spawn + Vec3::X * 10.0];
        for z in [&map.ct, &map.t].into_iter().chain(map.sites.iter()).flatten() {
            let c = |i: usize| (z.min[i] + z.max[i]) as f32 * 0.5;
            starts.push(Vec3::new(c(0), c(1), c(2)));
        }
        // Copie des chunks chargés à 8 chunks au plus des départs et des zones.
        let mut copy = World::new(0);
        for pos in self.world.loaded().collect::<Vec<_>>() {
            let mid = Vec3::new(pos.x as f32 * 16.0 + 8.0, 0.0, pos.z as f32 * 16.0 + 8.0);
            let near = starts
                .iter()
                .any(|s| Vec3::new(s.x, 0.0, s.z).distance(mid) <= 8.0 * 16.0);
            if near {
                if let Some(c) = self.world.chunk_arc(pos) {
                    copy.insert((*c).clone());
                }
            }
        }
        let nav = crate::bots::nav_from_world(copy, map.clone(), starts);
        let players = crate::bots::saved_players().unwrap_or(map.players() as usize);
        let level = crate::bots::saved_level();
        let pseudo = if self.pseudo.is_empty() {
            "Joueur".to_string()
        } else {
            self.pseudo.clone()
        };
        self.net = Some(crate::netgame::NetGame::build_host(
            link,
            pseudo,
            map,
            spawn,
            Some(nav),
            level,
            players,
        ));
        self.net_dead = false;
        self.player.flying = false;
        self.radar_map = false;
        self.game_over = None;
        log::info!(
            "partie FPS du serveur ouverte : {players} joueurs, bots {}",
            crate::bots::LEVELS[level as usize]
        );
        self.chat.say(
            format!(
                "Partie FPS ouverte sur le serveur ({players} joueurs, bots {}) : les joueurs en \
                 mode FPS la rejoignent. /restart : la recommencer, /bot maxbot <2-20> : joueurs \
                 de la partie, /bot facile|normal|difficile : leur niveau, /bot off : la fermer.",
                crate::bots::LEVELS[level as usize]
            ),
            chat::YELLOW,
        );
    }

    /// (v243) Entre dans la partie FPS du serveur de construction ouverte par `host` (ce joueur
    /// est en mode FPS) : le relais le dit à son hôte, et ce jeu en devient un invité.
    fn join_build_match(&mut self, host: &str) {
        let Some(link) = self.build.as_ref().map(|b| b.world_link()) else {
            return;
        };
        // (v322) La partie compte depuis l'origine du jeu de son hôte : la partie est d'abord
        // rouverte depuis elle, sans quitter le serveur (`wants_rebase`), puis on y entre
        // (`after_rebase`). Les places de la partie sont alors les mêmes chez tous.
        if self.world.origin() != self.build_match_origin {
            log::info!(
                "partie FPS du serveur : rouverte depuis l'origine de son hôte ({} / {}) avant d'y \
                 entrer",
                self.build_match_origin.x * 16,
                self.build_match_origin.z * 16
            );
            self.rebase_origin = Some(self.build_match_origin);
            return;
        }
        self.relay_cmds.push(minelib_net::client::Cmd::ServerAdmin {
            action: minelib_net::proto::SERVER_MATCH,
            pseudo: String::new(),
            reason: "join".into(),
        });
        let mut n = crate::netgame::NetGame::guest_loading(link, [0; 32]);
        n.loaded();
        n.set_map_doors(fps_map_of(&self.dir, &self.world).door_list);
        self.net = Some(n);
        self.net_dead = false;
        self.player.flying = false;
        self.radar_map = false;
        self.game_over = None;
        log::info!("partie FPS du serveur rejointe (hôte : {host})");
        self.chat
            .say(format!("Partie FPS de {host} rejointe."), chat::YELLOW);
    }

    /// (v243) Hors de la partie FPS du serveur de construction (fermée, quittée) : seul en mode
    /// FPS dans le monde, comme avant.
    fn end_build_match(&mut self) {
        log::info!("partie FPS du serveur quittée");
        self.net = None;
        self.net_dead = false;
        self.fps.online = None;
        // (v264) La partie quittée : plus de plaies.
        self.wounds.clear();
        self.fps.hp = 100;
        self.feed.clear();
        self.banner = None;
        self.game_over = None;
        self.spectating = None;
    }

    /// (v243) `/start`, `/bot …`, `/restart` sur un serveur de construction : la partie FPS du
    /// serveur, menée par l'OP qui l'ouvre (le relais refuse les autres) ; ses réglages
    /// changent chez son hôte seulement.
    fn build_match_command(&mut self, cmd: &str, arg: Option<&str>, count: Option<&str>) {
        use minelib_net::client::Cmd;
        use minelib_net::proto::SERVER_MATCH;
        if self.game.mode != GameMode::Fps {
            self.chat.say(
                "La partie se joue en mode FPS : /gm fps (OP).",
                chat::RED,
            );
            return;
        }
        let ask = |what: &str| Cmd::ServerAdmin {
            action: SERVER_MATCH,
            pseudo: String::new(),
            reason: what.into(),
        };
        // (v322) L'ouverture dit l'origine (en chunks) depuis laquelle ce jeu compte : ses
        // joueurs y entreront comptés depuis elle (`Response::BuildMatch`).
        let open = format!("open {} {}", self.world.origin().x, self.world.origin().z);
        let host_me = self
            .net
            .as_ref()
            .is_some_and(|n| n.is_build() && n.is_host());
        let level = arg.and_then(crate::bots::level_named);
        let off = arg.is_some_and(|w| w == "stop" || chat::switch(w) == Some(false));
        let maxbot = arg.is_some_and(|w| {
            matches!(w, "maxbot" | "maxbots" | "max" | "nombre" | "joueurs")
        });
        if !host_me {
            if let Some(host) = self.build_match.clone() {
                if !host.eq_ignore_ascii_case(&self.pseudo) {
                    self.chat.say(
                        format!("La partie est menée par {host} : c'est lui qui la règle."),
                        chat::RED,
                    );
                    return;
                }
            }
        }
        if let Some(l) = level {
            crate::bots::save_level(l);
            self.chat.say(
                format!("Bots : niveau {}.", crate::bots::LEVELS[l as usize]),
                chat::YELLOW,
            );
            if let Some(n) = self.net.as_mut().filter(|_| host_me) {
                n.set_bot_level(l);
            }
            return;
        }
        if maxbot {
            let range = crate::bots::MIN_PLAYERS..=crate::bots::MAX_PLAYERS;
            match count.and_then(|n| n.parse::<usize>().ok()) {
                Some(n) if range.contains(&n) => {
                    crate::bots::save_players(n);
                    let now = self.net.as_mut().filter(|_| host_me).map(|g| {
                        let humans = g.human_count();
                        (humans, g.set_bot_count(n.saturating_sub(humans)))
                    });
                    let t = match now {
                        Some((h, b)) => format!(
                            "Partie à {n} joueurs : {h} joueur{} et {b} bot{} maintenant.",
                            if h > 1 { "s" } else { "" },
                            if b > 1 { "s" } else { "" }
                        ),
                        None => format!("Parties du serveur : {n} joueurs, bots compris."),
                    };
                    self.chat.say(t, chat::YELLOW);
                }
                _ => self.chat.say(
                    format!(
                        "Utilisation : /bot maxbot <{}-{}> (joueurs de la partie ; les bots \
                         complètent)",
                        range.start(),
                        range.end()
                    ),
                    chat::RED,
                ),
            }
            return;
        }
        if off {
            if host_me {
                self.relay_cmds.push(ask("close"));
                self.end_build_match();
                self.chat.say("Partie FPS du serveur fermée.", chat::YELLOW);
            }
            return;
        }
        if cmd == "restart" {
            if host_me {
                // Fermée puis rouverte : ses joueurs la rejoignent de nouveau.
                self.relay_cmds.push(ask("close"));
                self.end_build_match();
                self.relay_cmds.push(ask(&open));
                self.match_opening = true;
                self.chat.say("La partie recommence.", chat::YELLOW);
            } else {
                self.chat
                    .say("Aucune partie à recommencer : /start.", chat::RED);
            }
            return;
        }
        if host_me {
            let n = self.net.as_mut().expect("partie");
            let t = if cmd == "start" {
                if n.start_now() {
                    "La partie commence.".to_string()
                } else {
                    "La partie est déjà commencée (/restart : la recommencer).".to_string()
                }
            } else {
                let players = crate::bots::saved_players()
                    .unwrap_or_else(|| fps_map_of(&self.dir, &self.world).players() as usize);
                let before = n.bot_count();
                let now = n.set_bot_count(players.saturating_sub(n.human_count()));
                if now == before {
                    format!("Déjà {now} bots dans la partie (/bot maxbot <2-20> pour en changer).")
                } else {
                    format!("{now} bots dans la partie.")
                }
            };
            self.chat.say(t, chat::YELLOW);
            return;
        }
        // Pas encore de partie : le relais vérifie qu'on est OP, puis l'ouvre (voir
        // `Event::BuildMatch`).
        self.relay_cmds.push(ask(&open));
        self.match_opening = true;
    }

    /// (v172) Partie contre des bots (mode FPS dans un monde solo) : la carte se remplit de
    /// bots, la partie démarre, chacun est placé à son départ.
    fn start_bots(&mut self) {
        // (v322) Ses départs loin de l'origine de la partie : elle repart d'abord d'une origine
        // proche d'eux, le joueur au départ (puis les bots démarrent : `bots_auto`).
        if let Some((origin, at)) = self.solo_fps_far() {
            log::info!(
                "partie contre les bots loin de l'origine de la partie : rouverte depuis la \
                 carte (départ en {:.1} / {:.1} / {:.1})",
                at[0],
                at[1],
                at[2]
            );
            self.rebase_origin = Some(origin);
            self.teleport_to = Some(at);
            return;
        }
        let map = fps_map_of(&self.dir, &self.world);
        let pseudo = if self.pseudo.is_empty() {
            "Joueur".to_string()
        } else {
            self.pseudo.clone()
        };
        let spawn = self.spawn_pos();
        // Les bots relèvent la carte enregistrée : elle doit être à jour.
        if let Err(e) = self.save() {
            log::warn!("bots : sauvegarde avant le relevé de la carte impossible : {e:#}");
        }
        let nav = crate::bots::nav_in_background(
            self.dir.clone(),
            map.clone(),
            vec![spawn],
            self.world.origin(),
        );
        let level = crate::bots::saved_level();
        self.net = Some(crate::netgame::NetGame::solo(
            pseudo,
            map,
            spawn,
            Some(nav),
            level,
        ));
        self.net_dead = false;
        self.player.flying = false;
        self.radar_map = false;
        self.game_over = None;
        self.chat.say(
            format!(
                "Partie contre des bots ({}) : elle commence. (/restart : la recommencer, \
                 /bot maxbot <2-20> : joueurs de la partie, /bot facile|normal|difficile : \
                 leur niveau, /bot off : les retirer.)",
                crate::bots::LEVELS[level as usize]
            ),
            chat::YELLOW,
        );
    }

    /// (v172) Fin de la partie contre des bots : seul sur la carte, en mode FPS.
    fn stop_bots(&mut self) {
        if !self.net.as_ref().is_some_and(|n| n.is_solo()) {
            return;
        }
        self.net = None;
        self.net_dead = false;
        self.fps.online = None;
        // (v264) La partie quittée : plus de plaies.
        self.wounds.clear();
        self.fps.hp = 100;
        self.feed.clear();
        self.banner = None;
        self.game_over = None;
        self.spectating = None;
    }

    /// Mode FPS : ouvre ou ferme la porte / trappe visée (l'hôte en décide en ligne).
    fn fps_door(&mut self) {
        let eye = self.player.eye();
        let dir = self.player.look_dir();
        let reach = crate::netgame::DOOR_REACH;
        let Some(h) = raycast(&self.world, eye.into(), dir.into(), reach) else {
            return;
        };
        if hinged(h.id).is_none() {
            return;
        }
        let side = side_toward(dir);
        if let Some(n) = &mut self.net {
            if !self.net_dead {
                let l = n.use_door(&self.world, h.block, side);
                self.apply_net(l);
            }
            return;
        }
        // Hors ligne (essai de sa carte) : selon le réglage de la carte ; sur un serveur de
        // construction, toujours (les blocs changés partent comme les autres).
        let map = fps_map_of(&self.dir, &self.world);
        if self.build.is_none() && !map.door_usable(h.block) {
            return;
        }
        let world = &self.world;
        let changes = toggle_door_pair_with(
            &|c| world.get(c[0], c[1], c[2]),
            &mut self.opened,
            h.block,
            dir,
            map.door_sync(),
        );
        if !changes.is_empty() {
            let sfx = crate::audio::Sfx::Door(door_opened(&changes));
            self.fps.sounds.push((sfx, 0.0, 0.9));
        }
        for (p, b) in changes {
            self.dirty.extend(self.world.set(p[0], p[1], p[2], b));
        }
    }

    fn check_fall(&mut self) {
        let p = &self.player;
        if self.game.mode.mortal() && p.pos.y < -40.0 {
            if let Some(n) = &mut self.net {
                if !self.net_dead {
                    let l = n.local_hurt(1000);
                    self.apply_net(l);
                }
                return;
            }
            self.die();
            return;
        }
        // L'eau amortit la chute : aucun dégât (en survie comme en FPS). Sur une échelle,
        // on ne tombe pas.
        if p.on_ladder || crate::player::in_water(&self.world, p.pos) {
            self.fall_from = None;
            return;
        }
        if !p.flying && !p.on_ground {
            let y = p.pos.y;
            self.fall_from = Some(self.fall_from.map_or(y, |f| f.max(y)));
        } else if let Some(from) = self.fall_from.take() {
            let height = from - p.pos.y;
            if self.game.mode == GameMode::Fps {
                // Mode FPS : 100 points de vie, dégâts selon la vitesse d'impact.
                // Un bloc de plus qu'en survie avant les premiers dégâts.
                let dmg = crate::fps::Fps::fall_damage(height, SAFE_FALL + 1.0);
                // En ligne, l'hôte fait foi : l'invité attend ses points de vie.
                if let Some(n) = &mut self.net {
                    if dmg > 0 && !p.flying {
                        let l = n.local_hurt(dmg);
                        self.hurt = 0.35;
                        self.apply_net(l);
                    }
                    return;
                }
                if !p.flying && dmg > 0 {
                    self.fps.hp = self.fps.hp.saturating_sub(dmg);
                    self.hurt = 0.35;
                    if self.fps.hp == 0 {
                        self.die();
                    }
                }
            } else if !p.flying && !self.game.can_fly && height > SAFE_FALL {
                // (v247) « Je ne dois pas prendre de dégâts de chute en /fly » : le vol permis,
                // aucune chute ne blesse (même vol coupé en l'air).
                self.damage((height - SAFE_FALL).ceil() as u8);
            }
        }
    }

    /// (v269) Œuf de L'Évidé : il sort là où l'on a cliqué, tourné vers nous.
    fn hatch_evide(&mut self, hit: minelib_world::raycast::Hit) {
        if self.net.is_some() || self.game.mode == GameMode::Fps {
            self.chat
                .say("L'Évidé ne vient pas en partie FPS.", chat::RED);
            return;
        }
        let [x, y, z] = hit.adjacent();
        let at = Vec3::new(x as f32 + 0.5, y as f32, z as f32 + 0.5);
        let to = self.player.pos - at;
        if !self.evides.spawn(at, to.x.atan2(-to.z)) {
            self.chat.say("Il y a déjà trop d'Évidés ici.", chat::RED);
            return;
        }
        if self.game.mode != GameMode::Creative {
            self.inventory.take_selected();
        }
    }

    /// (v269) Ce que vient de faire un Évidé : sa voix (grognement ou rire, d'où il est), sa
    /// morsure (« il retire un cube de notre corps, depuis les bords » : un morceau de notre
    /// modèle part, qui reviendra en cinq minutes, et on perd des points de vie).
    /// `names` : en ligne, les pseudos des autres joueurs (`evide::Prey::tag` moins un).
    fn evide_event(&mut self, ev: crate::evide::Event, names: &[String]) {
        let ear = self.player.eye();
        match ev {
            crate::evide::Event::Voice(at) => crate::audio::evide_voice(at.distance(ear)),
            // (v272) Il choisit où mordre : partout sur le tour de notre corps (les bras, le
            // torse, le ventre, les jambes ; pas la tête), les jambes seulement s'il rampe.
            // `render` dira où est cet endroit dans notre pose, image après image.
            // (v299) La gelée qui coule de ce qu'on lui mange, puis de son corps qui fond, et
            // ce que portent ses veines : des gouttes qui tachent le sol, comme le sang.
            crate::evide::Event::Drips {
                at,
                liquid,
                count,
                spurt,
            } => self.fps.ooze(liquid, at, Vec3::ZERO, usize::from(count), spurt),
            // (v312) Il vise un autre joueur : rien à suivre chez nous.
            crate::evide::Event::Aims { victim, .. } if victim != 0 => {}
            crate::evide::Event::Aims { low, seed, .. } => {
                let grids = crate::player_model::flesh_grids();
                self.evide_spot = self
                    .flesh
                    .entry(String::new())
                    .or_insert_with(|| crate::flesh::Flesh::of(crate::flesh::Hard::Bones))
                    .pick(seed, bite_parts(low), &grids);
                self.bite_points.borrow_mut().0 = None;
            }
            crate::evide::Event::Bites {
                low,
                at,
                seed,
                who,
                victim,
                aim,
            } => {
                // (v312) En ligne : la morsure est demandée au relais, qui ne la publie que
                // si le monstre peut mordre maintenant un joueur qu'il voit à sa portée ; elle
                // arrive (à la victime, à nous, aux autres) quand il la publie.
                if let Some(b) = &self.build {
                    let victim = match victim {
                        0 => b.pseudo().to_string(),
                        k => match names.get(usize::from(k) - 1) {
                            Some(n) => n.clone(),
                            None => return,
                        },
                    };
                    b.send_evide(minelib_net::game::BuildMsg::EvideBite {
                        owner: b.pseudo().to_string(),
                        id: who as u16,
                        victim,
                        low,
                        aim,
                        seed,
                    });
                    return;
                }
                self.evide_bitten(low, at, seed);
            }
        }
    }

    /// (v312) Sorti de `evide_event` : la morsure d'un Évidé tombe sur nous (son bruit, le
    /// morceau retiré là où il visait, le sang, le cri, les points de vie).
    fn evide_bitten(&mut self, low: bool, at: Vec3, seed: u32) {
        let ear = self.player.eye();
        crate::audio::jelly_bite(at.distance(ear));
        if self.game.mode == GameMode::Spectator {
            return;
        }
        // (v272) « La morsure n'est pas assez grande » : un cube de 3 ou 4 de côté
        // (2 ou 3 en v269), là où il visait.
        let size = 3 + (seed >> 9) as usize % 2;
        let grids = crate::player_model::flesh_grids();
        let flesh = self
            .flesh
            .entry(String::new())
            .or_insert_with(|| crate::flesh::Flesh::of(crate::flesh::Hard::Bones));
        let bite = match self.evide_spot.take() {
            Some((part, cell)) => flesh.bite_at(part, cell, size, &grids),
            None => flesh.bite_somewhere(seed, size, bite_parts(low), &grids),
        };
        if let Some(b) = bite {
            // Le sang part du bas de la morsure : un jet tout de suite, puis des
            // gouttes.
            let mid = |a: usize| ((b.lo[a] + b.hi[a]) / 2) as usize;
            let cell = [mid(0), b.lo[1] as usize, mid(2)];
            let from = self.bite_points.borrow().0.unwrap_or(at);
            self.fps.bleed(from, self.player.vel, BLEED_SPURT, true);
            self.bleeding.push((b.part as usize, cell, 0.0, 0.0));
        }
        // (v290) « Notre joueur doit faire le bruit d'avoir mal habituel aussi à une
        // morsure » : le cri des coups reçus (celui des PNJ frappés, des joueurs
        // touchés en partie FPS).
        self.fps.sounds.push((crate::audio::Sfx::Pain, 0.0, 0.7));
        self.damage(EVIDE_BITE);
    }

    /// (v312) L'Évidé partagé en construction en ligne (`minelib_net::evide`). Ceux qu'on mène
    /// sont dits au relais vingt fois par seconde ; ceux des autres arrivent publiés par lui
    /// (leurs pantins : `Horde::remote`) ; une morsure ou une bouchée n'a lieu que quand le
    /// relais la publie.
    fn evides_online(&mut self, dt: f32) {
        use minelib_net::game::BuildMsg;
        let Some(b) = self.build.as_mut() else {
            return;
        };
        let me = b.pseudo().to_string();
        self.evide_out += dt;
        if self.evide_out >= EVIDE_EVERY {
            self.evide_out = 0.0;
            let mobs = self.evides.wire(self.player.pos);
            if !mobs.is_empty() {
                b.send_evide(BuildMsg::Evide {
                    owner: me.clone(),
                    data: minelib_net::evide::pack(&mobs),
                });
            }
        }
        // (v318) Comptés depuis l'origine de la partie (`BuildNet::take_evides`).
        for (owner, data) in b.take_evides() {
            if let Some(mobs) = minelib_net::evide::unpack(&data) {
                self.evides.remote(&owner, mobs);
            }
        }
        let bites = std::mem::take(&mut b.evide_bites);
        let eats = std::mem::take(&mut b.evide_eats);
        // Où sont les autres joueurs (leurs pieds), par pseudo.
        let others: Vec<(String, Vec3)> =
            b.others.values().map(|o| (o.pseudo.clone(), o.pos)).collect();
        let place = |who: &str| {
            others
                .iter()
                .find(|(p, _)| p.eq_ignore_ascii_case(who))
                .map(|(_, at)| *at)
        };
        let ear = self.player.eye();
        for (owner, id, victim, low, aim, seed) in bites {
            // La bouche du monstre : le nôtre, ou le pantin de celui d'un autre.
            let mine = owner.eq_ignore_ascii_case(&me);
            let mouth = self
                .evides
                .list
                .iter()
                .find(|e| match &e.lead {
                    None => mine && e.id as u16 == id,
                    Some((o, n)) => !mine && *n == id && o.eq_ignore_ascii_case(&owner),
                })
                .map(crate::evide::Evide::eye);
            if victim.eq_ignore_ascii_case(&me) {
                // Mené par un autre : l'endroit visé se tire comme chez lui.
                if self.evide_spot.is_none() {
                    let grids = crate::player_model::flesh_grids();
                    self.evide_spot = self
                        .flesh
                        .entry(String::new())
                        .or_insert_with(|| crate::flesh::Flesh::of(crate::flesh::Hard::Bones))
                        .pick(aim, bite_parts(low), &grids);
                }
                let chest = self.player.pos + Vec3::Y * 1.1;
                self.evide_bitten(low, mouth.unwrap_or(chest), seed);
            } else if let Some(at) = place(&victim) {
                // Un autre joueur est mordu : le bruit, et son sang.
                let chest = at + Vec3::Y * 1.1;
                crate::audio::jelly_bite(chest.distance(ear));
                self.fps.bleed(chest, Vec3::ZERO, BLEED_SPURT, true);
            }
        }
        for (owner, id, part, cell, by, _health) in eats {
            let from = if by.eq_ignore_ascii_case(&me) {
                Some(ear)
            } else {
                place(&by).map(|at| at + Vec3::Y * 1.5)
            };
            if owner.eq_ignore_ascii_case(&me) {
                // Le nôtre : c'est ici qu'il perd le morceau (et meurt à la cinquième bouchée).
                let Some(e) = self.evides.own(id) else {
                    continue;
                };
                let (at, from) = (e.pos, from.unwrap_or(e.pos + Vec3::X));
                let eaten = e.eaten_at(usize::from(part), cell.map(usize::from), from);
                if let Some(dead) = eaten {
                    crate::audio::jelly_bite(at.distance(ear));
                    if dead {
                        crate::audio::evide_voice(at.distance(ear));
                    }
                }
            } else {
                // Celui d'un autre : son pantin perd le morceau avec l'état que le relais
                // publie ; ici, le bruit.
                let at = self
                    .evides
                    .list
                    .iter()
                    .find(|e| {
                        e.lead
                            .as_ref()
                            .is_some_and(|(o, n)| *n == id && o.eq_ignore_ascii_case(&owner))
                    })
                    .map(|e| e.pos);
                if let Some(at) = at {
                    crate::audio::jelly_bite(at.distance(ear));
                }
            }
        }
    }

    fn damage(&mut self, amount: u8) {
        if !self.game.mode.mortal() || amount == 0 {
            return;
        }
        self.game.health = self.game.health.saturating_sub(amount);
        self.hurt = 0.35;
        if self.game.health == 0 {
            self.die();
        }
    }

    /// (v299) Un mort tombe là, dans la posture `pose` (en construction seulement) : son corps
    /// (`flesh` : ce qu'il lui en restait ; rien : entier) s'effondre selon les blocs autour de
    /// lui, fond en sang, laisse ses os 5 minutes. Au plus `FALLEN_MAX` à la fois (le plus
    /// vieux s'en va).
    fn fall_body(
        &mut self,
        pose: &crate::player_model::Pose,
        vel: Vec3,
        push: Vec3,
        flesh: Option<crate::flesh::Flesh>,
        skin: Option<std::sync::Arc<crate::skin::Skin>>,
    ) {
        if self.game.mode == GameMode::Fps {
            return;
        }
        if self.fallen.len() >= FALLEN_MAX {
            self.fallen.remove(0);
        }
        self.fallen_next += 1;
        let flesh =
            flesh.unwrap_or_else(|| crate::flesh::Flesh::of(crate::flesh::Hard::Bones));
        self.fallen.push(crate::corpse::Fallen::new(
            &crate::player_model::stance(pose),
            vel,
            push,
            self.fallen_next as u32 * 7919 + 13,
            flesh,
            skin,
            // (Les lots des têtes sont des numéros de zone, celui des cadres u64::MAX.)
            (1 << 62) + self.fallen_next,
        ));
    }

    fn die(&mut self) {
        // En ligne, la mort et la réapparition viennent de l'hôte.
        if self.net.is_some() {
            return;
        }
        self.chat
            .say("Vous êtes mort ! Retour au point d'apparition.", chat::RED);
        // (v299) Son corps reste où il est mort : il tombe, fond en sang, laisse ses os.
        {
            let pose = self.body_pose();
            let (vel, flesh, skin) = (self.player.vel, self.flesh.remove(""), self.worn_skin());
            let back = -self.player.look_dir();
            let push = Vec3::new(back.x, 0.0, back.z).normalize_or_zero() * 1.2;
            self.fall_body(&pose, vel, push, flesh, skin);
        }
        self.player.pos = self.spawn;
        // Point d'apparition en l'air (monde enregistré en volant) : on réapparaît posé sur
        // le sol en dessous, sinon on retomberait et on mourrait en boucle.
        let (x, z) = (self.spawn.x.floor() as i32, self.spawn.z.floor() as i32);
        if let Some(top) = self.world.top_solid(x, z) {
            if (top as f32 + 1.0) < self.spawn.y {
                self.player.pos.y = top as f32 + 1.0;
            }
        }
        self.player.flying = false;
        self.player.vel = Vec3::ZERO;
        self.game.health = MAX_HEALTH;
        self.fall_from = None;
        // (v269) Un corps neuf : ce que L'Évidé avait mangé est revenu.
        self.flesh.remove("");
        self.bleeding.clear();
        self.evide_spot = None;
        self.eating = None;
        self.eat_bow = 0.0;
        self.eat_turn = (0.0, 0.0);
        self.fps.respawn();
        // (v283) Réapparu loin : pas en jeu tant que le monde n'y est pas chargé.
        self.arrive_elsewhere();
    }

    /// Point d'apparition du monde.
    pub fn spawn_pos(&self) -> Vec3 {
        self.spawn
    }

    /// (v223) `/tp` : `x y z` (« ~ » : par rapport à soi), `pseudo` (là où il est), `pseudo x
    /// y z`, `pseudo pseudo2`. En solo, soi seulement ; sur un serveur de construction,
    /// réservé aux OP : le relais vérifie, puis place celui qui part (`BuildSpawn`).
    fn tp_command(&mut self, args: &[&str]) {
        let usage = "Utilisation : /tp <x> <y> <z> (« ~ » : par rapport à toi), /tp <pseudo>, \
                     /tp <pseudo> <x> <y> <z>, /tp <pseudo> <pseudo2>";
        // (Qui part, et où : trois coordonnées ou un pseudo.)
        let (who, at, other): (&str, Option<[&str; 3]>, Option<&str>) = match args {
            [x, y, z] => ("", Some([x, y, z]), None),
            [name] => ("", None, Some(name)),
            [name, x, y, z] => (name, Some([x, y, z]), None),
            [name, other] => (name, None, Some(other)),
            _ => return self.chat.say(usage, chat::RED),
        };
        if self.build.is_some() {
            // En ligne : le relais vérifie, puis place celui qui part (`BuildSpawn`). (v318)
            // Places absolues lues exactement et ramenées sur la carte bouclée, comme en solo
            // (`tp_target`) ; les tours d'un `/tp` de soi-même comptent à l'arrivée.
            let to = match (at, other) {
                (Some(a), _) => {
                    let Some((laps, p)) = self.tp_target(&a) else {
                        return self.chat.say(
                            format!(
                                "{usage}. Coordonnées : des nombres (« 12 », « -3.5 »), {}                                  chiffres au plus avant la virgule ; hauteur entre -4096 et 4096.",
                                crate::laps::MAX_DIGITS
                            ),
                            chat::RED,
                        );
                    };
                    let me = who.is_empty() || who.eq_ignore_ascii_case(&self.pseudo);
                    self.tp_laps_online = me.then(|| (laps, p, std::time::Instant::now()));
                    format!("{} {} {}", p[0], p[1], p[2])
                }
                (None, Some(o)) => format!("@{o}"),
                (None, None) => return self.chat.say(usage, chat::RED),
            };
            self.relay_cmds.push(minelib_net::client::Cmd::ServerAdmin {
                action: minelib_net::proto::SERVER_TELEPORT,
                pseudo: who.to_string(),
                reason: to,
            });
            return;
        }
        // Solo : soi seulement (« /tp <son pseudo> x y z » aussi).
        let me = !who.is_empty() && who.eq_ignore_ascii_case(&self.pseudo);
        let Some(a) = at.filter(|_| who.is_empty() || me) else {
            self.chat
                .say("En solo, on ne téléporte que soi, vers des coordonnées.", chat::RED);
            return;
        };
        // (v309) La place visée, lue exactement (`laps::parse_exact`) et ramenée sur la carte
        // bouclée : aussi loin qu'on veut (38 chiffres), sans rien perdre ni déborder.
        let Some((laps, p)) = self.tp_target(&a) else {
            self.chat.say(
                format!(
                    "{usage}. Coordonnées : des nombres (« 12 », « -3.5 »), {} chiffres au plus \
                     avant la virgule ; hauteur entre -4096 et 4096.",
                    crate::laps::MAX_DIGITS
                ),
                chat::RED,
            );
            return;
        };
        let before = self.laps();
        self.teleport_abs(p, None);
        if self.teleport_to.is_some() {
            self.teleport_laps = Some(laps);
            self.laps_seen = laps;
        } else {
            self.set_laps(laps);
        }
        self.chat.say(
            format!("Téléporté en {:.1} {:.1} {:.1}.", p[0], p[1], p[2]),
            chat::YELLOW,
        );
        if laps != before {
            self.chat.say(laps_said(laps), chat::YELLOW);
        }
    }

    /// (v309) La place visée par `/tp x y z` en solo : x et z lus exactement (partie entière
    /// sur 128 bits), par rapport à la place exacte du joueur avec « ~ » (ses tours compris),
    /// puis ramenés sur la carte bouclée ; y en double précision, entre −4 096 et 4 096.
    /// `None` : pas un nombre, trop de chiffres, ou une somme qui déborderait.
    fn tp_target(&self, a: &[&str; 3]) -> Option<(crate::laps::Laps, [f64; 3])> {
        use crate::laps::{add_exact, exact, on_map, parse_exact};
        let here = self.abs_of(self.player.pos);
        let now = self.laps();
        let mut out = [0.0f64; 3];
        let mut laps = [0i128; 2];
        for (k, t) in a.iter().enumerate() {
            let (rel, rest) = match t.strip_prefix('~') {
                Some(r) => (true, r),
                None => (false, *t),
            };
            if k == 1 {
                let v: f64 = if rest.is_empty() { 0.0 } else { rest.parse().ok()? };
                let y = if rel { here[1] + v } else { v };
                if !(y.is_finite() && y.abs() <= 4096.0) {
                    return None;
                }
                out[1] = y;
                continue;
            }
            let axis = k / 2;
            let v = if rest.is_empty() { (0, 0.0) } else { parse_exact(rest)? };
            let v = if rel { add_exact(exact(now[axis], here[k])?, v)? } else { v };
            let (l, at) = on_map(v);
            laps[axis] = l;
            out[k] = at;
        }
        Some((laps, out))
    }

    /// (v309) `/tp` vers la place absolue `a`. Loin de l'origine de la partie (solo), la place
    /// exacte est gardée pour l'enregistrement : la partie repart aussitôt d'une origine proche
    /// d'elle (`wants_rebase`), sans l'arrondi d'un f32 loin de l'origine.
    /// (v318) En ligne aussi (arrivée sur un serveur de construction, `/tp` d'un OP) : la
    /// partie repart d'une origine proche sans quitter le serveur (`Game::rebase_world`).
    fn teleport_abs(&mut self, a: [f64; 3], look: Option<(f32, f32)>) {
        let local = self.local_of(a);
        let far = self.far_allowed && (local.x.abs() > REBASE_AT || local.z.abs() > REBASE_AT);
        // (v322) Loin de la partie FPS en cours (contre les bots, ou celle du serveur) : on la
        // quitte, en restant en mode FPS, et la partie repart d'une origine proche du point
        // d'arrivée (avant : la partie FPS empêchait ce départ, le joueur restait dans les
        // terres lointaines).
        if far && self.net.is_some() {
            self.leave_match_far();
        }
        self.teleport_to = far.then_some(a);
        self.teleport(local, look);
    }

    /// (v322) Téléporté loin de la partie FPS en cours : on la quitte (en restant en mode FPS).
    fn leave_match_far(&mut self) {
        if self.net.as_ref().is_some_and(|n| n.is_solo()) {
            self.stop_bots();
            self.bots_auto = false;
            self.chat.say(
                "Partie contre les bots arrêtée : tu es parti loin d'elle (/bot : en relancer une).",
                chat::YELLOW,
            );
        } else if self.in_build_match() {
            let host = self.net.as_ref().is_some_and(|n| n.is_host());
            self.relay_cmds.push(minelib_net::client::Cmd::ServerAdmin {
                action: minelib_net::proto::SERVER_MATCH,
                pseudo: String::new(),
                reason: if host { "close" } else { "leave" }.into(),
            });
            self.end_build_match();
            self.chat.say(
                if host {
                    "Partie FPS du serveur fermée : tu es parti loin d'elle."
                } else {
                    "Tu as quitté la partie FPS du serveur : tu es parti loin d'elle."
                },
                chat::YELLOW,
            );
        }
    }

    /// (v223) Placé ailleurs (`/tp`, arrivée sur un serveur) : sans vitesse ni chute en cours.
    fn teleport(&mut self, pos: Vec3, look: Option<(f32, f32)>) {
        self.player.pos = pos;
        self.player.vel = Vec3::ZERO;
        if let Some((yaw, pitch)) = look {
            self.player.yaw = yaw;
            self.player.pitch = pitch;
        }
        self.fall_from = None;
        self.fall_top = None;
        // (v283) Pas en jeu tant que le monde n'y est pas chargé.
        self.arrive_elsewhere();
    }

    /// (v223) `/setspawn` : le point d'apparition là où l'on est. En solo, enregistré avec le
    /// monde (on y réapparaît) ; sur un serveur de construction, réservé aux OP : ceux qui y
    /// arrivent pour la première fois y apparaissent.
    fn setspawn(&mut self) {
        let p = self.player.pos;
        if self.build.is_some() {
            // (v318) À sa place absolue (en double précision).
            let a = self.abs_of(p);
            self.relay_cmds.push(minelib_net::client::Cmd::ServerAdmin {
                action: minelib_net::proto::SERVER_SETSPAWN,
                pseudo: String::new(),
                reason: format!(
                    "{} {} {} {} {}",
                    a[0], a[1], a[2], self.player.yaw, self.player.pitch
                ),
            });
            return;
        }
        self.spawn = p;
        self.spawn_set = true;
        // (v309) À sa place absolue.
        let a = self.abs_of(p);
        self.chat.say(
            format!("Point d'apparition : {:.0} {:.0} {:.0}.", a[0], a[1], a[2]),
            chat::YELLOW,
        );
    }

    /// Événement du relais pour la partie en ligne.
    pub fn net_event(&mut self, e: &minelib_net::client::Event) {
        use minelib_net::client::Event;
        if let Event::VoiceFrom { from, data } = e {
            self.voice_from(from, data, false);
            return;
        }
        if let Event::Voice { from, data } = e {
            let (name, dist) = self.speaker(*from);
            let mut gain = self.volume.max(0.05) * 1.6 * self.voice_volume;
            gain *= self
                .voice_volumes
                .get(&name.to_lowercase())
                .copied()
                .unwrap_or(1.0);
            // Construction : plus bas avec la distance, muet au-delà de 48 blocs.
            if let Some(d) = dist {
                gain *= (1.0 - d / VOICE_RANGE).clamp(0.0, 1.0);
            }
            if let Some(v) = &mut self.voice {
                v.receive(*from, None, data, gain);
            }
            return;
        }
        // (v243) Partie FPS du serveur de construction : son ouverture, ses joueurs (chez son
        // hôte), ses messages.
        if self.build.is_some() {
            match e {
                Event::BuildMatch { host, open, origin } => {
                    self.build_match_event(host, *open, *origin);
                    return;
                }
                Event::MatchPeer {
                    peer,
                    pseudo,
                    joined,
                } => {
                    if let Some(n) = self.net.as_mut().filter(|n| n.is_build()) {
                        if *joined {
                            n.peer_joined(*peer, pseudo.clone());
                        } else {
                            n.peer_left(*peer);
                        }
                    }
                    return;
                }
                Event::FromPeer { peer, data } => {
                    let locals = match self.net.as_mut().filter(|n| n.is_build()) {
                        Some(n) => n.receive(Some(*peer), data, &mut self.world),
                        None => return,
                    };
                    self.apply_net(locals);
                    return;
                }
                Event::FromHost(data) => {
                    let locals = match self.net.as_mut().filter(|n| !n.is_solo()) {
                        Some(n) => n.receive(None, data, &mut self.world),
                        None => return,
                    };
                    self.apply_net(locals);
                    return;
                }
                _ => {}
            }
        }
        if let Some(b) = &mut self.build {
            match e {
                // Le fonctionnement côté serveur (propriétaire, zones) ne s'affiche pas.
                Event::Promoted { .. } => b.set_owner(),
                Event::HostChanged(host) => {
                    log::info!("monde gardé par {host}");
                    b.clear_owner();
                }
                Event::Zones { gained, lost } => {
                    let _ = b.zones_changed(gained, lost);
                }
                // Relais des régions peuplées, et billet pour y entrer.
                Event::Shards(list) => b.set_shards(list.clone()),
                Event::Provinces(list) => b.set_provinces(list.clone()),
                Event::ShardTicket {
                    room,
                    pseudo,
                    expires,
                    sig,
                    cert,
                    root,
                } => {
                    b.set_ticket(*room, pseudo.clone(), *expires, sig.clone(), cert.clone());
                    b.set_root(*root);
                }
                // WorldEdit refusé (pas OP) : les blocs reviennent.
                // Arrivée sur le serveur : là où l'on était (gardé par le relais).
                Event::BuildSpawn { pos, yaw, pitch } => {
                    // (À l'arrivée : sa dernière position ici ; (v223) `/tp`.)
                    let look = (yaw.is_finite() && pitch.is_finite()).then_some((*yaw, *pitch));
                    // (v318) Place absolue, en double précision : loin de l'origine de la
                    // partie, elle en change (sans quitter le serveur).
                    if pos.iter().all(|v| v.is_finite()) {
                        let at = [
                            minelib_net::proto::wrap_place(pos[0]),
                            pos[1],
                            minelib_net::proto::wrap_place(pos[2]),
                        ];
                        // (v318) Notre `/tp` (récent, vers cette place) : ses tours comptent.
                        let laps = self.tp_laps_online.take().and_then(|(l, p, t)| {
                            let close = (minelib_net::proto::place_gap(p[0], at[0]).abs() < 1.0)
                                && (minelib_net::proto::place_gap(p[2], at[2]).abs() < 1.0);
                            (close && t.elapsed().as_secs_f32() < 15.0).then_some(l)
                        });
                        let before = self.laps();
                        self.teleport_abs(at, look);
                        // (Placé par le relais, à l'arrivée ou par un OP : pas un tour de la
                        // carte, même de l'autre côté de sa limite.)
                        let target = laps.unwrap_or(before);
                        if self.teleport_to.is_some() {
                            self.teleport_laps = Some(target);
                            self.laps_seen = target;
                        } else {
                            self.set_laps(target);
                        }
                        if target != before {
                            self.chat.say(laps_said(target), chat::YELLOW);
                        }
                    }
                    return;
                }
                Event::BulkDone { id, error } => {
                    let dirty = b.bulk_done(*id, error, &mut self.world);
                    self.dirty.extend(dirty);
                    if !error.is_empty() && self.bulk_refused.elapsed().as_secs_f32() > 2.0 {
                        self.bulk_refused = std::time::Instant::now();
                        self.chat.say(error.clone(), chat::RED);
                    }
                    return;
                }
                // Heure du serveur (à l'arrivée, ou changée par un OP).
                Event::SetTime { hours, fixed } => {
                    self.time_h = *hours;
                    self.time_fixed = *fixed;
                    return;
                }
                // (v229) Météo du serveur (à l'arrivée, ou changée par un OP) : le temps
                // imposé et ce qu'il lui reste, ou celui des horloges du monde.
                Event::Zeus {
                    seq,
                    act,
                    why,
                    holder,
                    charges,
                    sky_hits,
                    expires,
                    permit,
                } => self.on_zeus(
                    *seq,
                    *act,
                    *why,
                    holder.clone(),
                    *charges,
                    *sky_hits,
                    *expires,
                    permit.clone(),
                ),
                // (v328) La graine du monde : ce qui vaut pour nous (à l'arrivée, quand un OP
                // la verrouille ou non, ou qu'on devient OP ou qu'on ne l'est plus).
                Event::SeedLock {
                    locked,
                    allowed,
                    seed,
                } => {
                    let before = self.seed_lock.replace((*locked, *allowed));
                    // (v330) La graine elle-même, à qui peut la voir ; plus permise : oubliée.
                    // (Une province dit ce qui change sans la graine : on garde celle qu'on
                    // a, ou on la redemande au relais du monde.)
                    if !*allowed {
                        self.relay_seed = None;
                    } else if *seed != 0 {
                        self.relay_seed = Some(*seed);
                    } else if self.relay_seed.is_none() && self.known_seed().is_none() {
                        self.relay_cmds.push(minelib_net::client::Cmd::ServerAdmin {
                            action: minelib_net::proto::SERVER_SEED,
                            pseudo: String::new(),
                            reason: String::new(),
                        });
                    }
                    let mine = self
                        .seed_lock_asked
                        .take()
                        .is_some_and(|t| t.elapsed().as_secs_f32() < 10.0);
                    if before.is_some_and(|(was, _)| was != *locked) && !mine {
                        self.chat.say(
                            if *locked {
                                "Un OP a verrouillé la graine du monde (/seed, téléchargement de la carte)."
                            } else {
                                "Un OP a déverrouillé la graine du monde : /seed, et « Télécharger la carte » dans le menu Échap."
                            },
                            chat::YELLOW,
                        );
                    }
                    return;
                }
                Event::SetWeather { kind, left, fixed } => {
                    self.storm.weather.impose(
                        crate::weather::Kind::from_byte(*kind),
                        (*left >= 0.0).then_some(*left as f64),
                        *fixed,
                    );
                    // (v247) À l'arrivée, le temps du serveur est là d'un coup (avant : le ciel
                    // passait du beau temps à l'orage en arrivant, nuages « mal chargés »), et
                    // le chargement l'attend (`check_loaded`).
                    if !std::mem::replace(&mut self.weather_synced, true) {
                        self.storm.weather.resettle();
                    }
                    return;
                }
                // Mode de jeu donné par un OP (ou par défaut du serveur, à l'arrivée).
                Event::SetGamemode { mode, by } => {
                    if let Some(m) = GameMode::parse(mode) {
                        if !by.is_empty() {
                            self.chat
                                .say(format!("Mode de jeu donné par {by}."), chat::YELLOW);
                        }
                        self.set_mode(m);
                    }
                    return;
                }
                // Un OP du serveur a tué ce joueur (/kill).
                Event::Slain(by) => {
                    self.chat.say(format!("Tué par {by} (/kill)."), chat::RED);
                    self.die();
                }
                Event::FromZone { peer, chunk, data } => {
                    let dirty = b.receive(*peer, *chunk, data, &mut self.world);
                    self.dirty.extend(dirty);
                }
                // (v333) Voie directe des voisins : routes données par le relais du monde,
                // et positions venues par elles.
                Event::Near(near) => b.set_near(near.clone()),
                Event::NearPos { peer, data } => b.near_pose(*peer, data),
                _ => {}
            }
            return;
        }
        let Some(n) = &mut self.net else { return };
        match e {
            Event::Promoted { members, old_host } => {
                let me = n.me();
                let pseudo = n
                    .rows()
                    .into_iter()
                    .find(|r| r.id == me)
                    .map_or_else(String::new, |r| r.pseudo);
                let map = fps_map_of(&self.dir, &self.world);
                let resumed = n.promote(pseudo, members, &self.player, map, *old_host);
                if let Some((guns, nades)) = n.my_guns() {
                    self.fps.set_guns(guns, nades);
                }
                let t = if resumed && old_host.is_some() {
                    "Ta connexion est la meilleure : tu sers maintenant la partie."
                } else if resumed {
                    "L'hôte est parti : tu étais garant de secours, tu reprends la partie."
                } else {
                    "L'hôte est parti : tu ouvres une nouvelle partie sur la même carte."
                };
                self.chat.say(t, chat::YELLOW);
                return;
            }
            Event::HostChanged(host) => {
                let was_host = n.is_host();
                if was_host {
                    // Un garant indépendant prend la partie : on devient un joueur comme
                    // les autres (il fait foi, plus nous).
                    n.demote();
                    self.chat.say(
                        format!("{host} sert maintenant la partie (connexion plus sûre)."),
                        chat::YELLOW,
                    );
                } else {
                    n.host_changed();
                    self.chat.say(
                        format!("Changement de serveur : {host} sert la partie."),
                        chat::YELLOW,
                    );
                }
                return;
            }
            Event::BackupIs(peer) => {
                n.designate_backup(*peer);
                return;
            }
            Event::Judge {
                conflict,
                seq,
                host_pos,
                audit_pos,
            } => {
                // Notre propre prédiction tranche : la position la plus proche a raison.
                if let Some(mine) = n.predicted_at(*seq) {
                    let d = |p: &[f32; 3]| (Vec3::from(*p) - Vec3::from(mine)).length();
                    self.relay_cmds.push(minelib_net::client::Cmd::Vote {
                        conflict: *conflict,
                        auditor_right: d(audit_pos) < d(host_pos),
                    });
                }
                return;
            }
            _ => {}
        }
        let locals = match e {
            Event::PeerJoined {
                peer,
                pseudo,
                party,
            } => {
                n.peer_joined_party(*peer, pseudo.clone(), *party);
                Vec::new()
            }
            Event::PeerLeft(peer) => {
                n.peer_left(*peer);
                Vec::new()
            }
            Event::FromPeer { peer, data } => n.receive(Some(*peer), data, &mut self.world),
            Event::FromHost(data) => n.receive(None, data, &mut self.world),
            // Voie rapide : le bout donné par le relais pour cette partie.
            Event::Fast(end) => {
                n.set_fast(end.clone());
                Vec::new()
            }
            _ => Vec::new(),
        };
        self.apply_net(locals);
    }

    /// Menu Échap d'une partie FPS en ligne : propose un vote à son équipe.
    pub fn call_vote(&mut self, kind: u8, target: u64) {
        let locals = match &mut self.net {
            Some(n) => n.call_vote(kind, target),
            None => return,
        };
        self.apply_net(locals);
    }

    /// Coéquipiers (identifiant, pseudo) qu'on peut proposer d'exclure, et pauses
    /// tactiques qui restent ; `None` hors partie FPS en ligne.
    pub fn vote_choices(&self) -> Option<VoteChoices> {
        let n = self.net.as_ref()?;
        let hud = n.hud();
        let me = n.me();
        let mates = n
            .rows()
            .into_iter()
            .filter(|r| r.team == hud.team && r.id != me)
            .map(|r| (r.id, r.pseudo))
            .collect();
        let started = hud.phase != minelib_net::game::phase::WARMUP;
        Some((mates, hud.timeouts, started && hud.vote.is_none()))
    }

    fn apply_net(&mut self, locals: Vec<crate::netgame::Local>) {
        use crate::netgame::Local;
        use minelib_net::game::{reason, CT};
        // (Partie solo contre des bots : rien à dire au relais.) (v243) Ni de la partie d'un
        // serveur de construction : le serveur continue, elle n'est ni classée ni en pause pour
        // lui.
        let online = self.ranked_match();
        let build_match = self.in_build_match();
        // (v264) Les plaies d'un joueur mort (ou parti) s'en vont avec lui.
        if let (false, Some(n)) = (self.wounds.is_empty(), &self.net) {
            let views = n.views();
            self.wounds
                .retain(|k, _| k.is_empty() || views.iter().any(|v| v.alive && v.pseudo == *k));
        }
        for l in locals {
            match l {
                Local::SetHp(hp) => {
                    if hp < self.fps.hp {
                        self.hurt = 0.35;
                    }
                    if hp == 0 {
                        self.wounds.remove("");
                    }
                    self.fps.hp = hp;
                    if hp == 0 && !self.net_dead {
                        self.net_dead = true;
                        self.fps.buy_open = false;
                        self.chat.say("Tu es mort.", chat::RED);
                    }
                }
                Local::Respawn { state, reset, team } => {
                    self.player.set_net_state(&state);
                    self.player.vel = Vec3::ZERO;
                    self.sim = None;
                    self.prev_pos = self.player.pos;
                    self.fall_from = None;
                    // Échauffement : on réapparaît seul, les fumées des autres restent.
                    let warmup = self
                        .net
                        .as_ref()
                        .is_some_and(|n| n.hud().phase == minelib_net::game::phase::WARMUP);
                    self.fps.new_round(reset, team, !warmup);
                    // (v264) Ses plaies s'en vont avec lui ; nouvelle manche : celles de tous.
                    if warmup {
                        self.wounds.remove("");
                    } else {
                        self.wounds.clear();
                    }
                    // (v209) Nouvelle manche : plus aucun adversaire repéré.
                    self.spotting.clear();
                    self.net_dead = false;
                }
                // (v174) Retour dans la partie solo après le mode spectateur, là où l'on
                // est (rien n'est effacé dans le monde : fumées, grenades en vol).
                Local::Resume { fresh, reset, team } => {
                    if fresh {
                        self.fps.new_round(reset, team, false);
                    }
                    self.net_dead = false;
                    self.spectating = None;
                    self.chat.say("Tu reprends la partie.", chat::YELLOW);
                }
                // (v209) Partie solo, mort : on prend la place du bot regardé, là où il est,
                // avec ce qu'il avait en main (vie et armes viennent d'arriver).
                Local::TakeOver {
                    state,
                    bot,
                    held,
                    ammo,
                } => {
                    self.player.set_net_state(&state);
                    self.player.vel = Vec3::ZERO;
                    self.sim = None;
                    self.prev_pos = self.player.pos;
                    self.fall_from = None;
                    self.net_dead = false;
                    self.spectating = None;
                    self.spec_fps = crate::fps::Fps::new();
                    self.fps.take_over(held, &ammo);
                    self.chat
                        .say(format!("Tu prends la place de {bot}."), chat::YELLOW);
                }
                Local::Chat(t) => self.chat.say(t, chat::WHITE),
                Local::Notice(t) => self.chat.say(t, chat::YELLOW),
                Local::Hit { head } => self.fps.hit_marker = (0.25, head),
                Local::Give(item) => self.fps.give(item),
                Local::Kicked(reason) => {
                    if online {
                        self.kicked = Some(reason);
                    } else if build_match {
                        // (v243) Exclu de la partie du serveur : on y reste, seul en mode FPS.
                        self.relay_cmds.push(minelib_net::client::Cmd::ServerAdmin {
                            action: minelib_net::proto::SERVER_MATCH,
                            pseudo: String::new(),
                            reason: "leave".into(),
                        });
                        self.end_build_match();
                        self.chat.say(reason, chat::RED);
                    } else {
                        // Partie solo contre des bots : elle s'arrête, on reste sur la carte.
                        self.stop_bots();
                        self.bots_auto = false;
                        self.chat.say(reason, chat::RED);
                    }
                }
                Local::Paused(on) => {
                    if online {
                        self.room_paused = Some(on);
                    }
                }
                Local::Kill(k) => {
                    self.feed.push((k, 0.0));
                    if self.feed.len() > 6 {
                        self.feed.remove(0);
                    }
                }
                Local::Shot {
                    weapon,
                    pos,
                    heard,
                    hits,
                    dirs,
                } => {
                    let eye = self.player.eye();
                    // (v209) Spectateur : le tir du joueur regardé se voit dans ses mains.
                    if self.spectating.is_some() && pos.distance(eye) < 1.2 {
                        self.spec_fps.mirror_fire();
                    }
                    if heard {
                        self.fps.heard_shot(weapon, pos, eye);
                        // (v249) Ses impacts sur les blocs (les nôtres sont posés au tir).
                        self.fps.shot_seen(&self.world, weapon, pos, &dirs);
                    }
                    let knife = weapon >= crate::netgame::KNIFE_HEAVY;
                    for h in hits {
                        self.fps.blood_at(h, pos, knife, eye, &self.world);
                        self.wound_at(h, pos, knife);
                    }
                }
                Local::Step(pos, kind) => {
                    let eye = self.player.eye();
                    self.fps.heard_step(pos, eye, kind);
                }
                Local::Doors { cells, at } => {
                    let changes: Vec<([i32; 3], BlockId)> =
                        cells.into_iter().map(|(c, b)| (c, BlockId(b))).collect();
                    for &(c, b) in &changes {
                        self.dirty.extend(self.world.set(c[0], c[1], c[2], b));
                    }
                    if let Some(at) = at {
                        let d = (at - self.player.eye()).length();
                        let sfx = crate::audio::Sfx::Door(door_opened(&changes));
                        self.fps.sounds.push((sfx, 0.0, 0.9 / (1.0 + d / 8.0)));
                    }
                }
                Local::Guns { guns, nades } => self.fps.set_guns(guns, nades),
                Local::RoundEnd {
                    winner,
                    reason: why,
                } => {
                    let side = if winner == CT {
                        "Les antiterroristes gagnent"
                    } else {
                        "Les terroristes gagnent"
                    };
                    let how = match why {
                        reason::EXPLODED => " : la bombe a explosé",
                        reason::DEFUSED => " : bombe désamorcée",
                        reason::TIME => " : temps écoulé",
                        _ => "",
                    };
                    let col = if winner == CT {
                        [0.45, 0.65, 1.0, 1.0]
                    } else {
                        [1.0, 0.75, 0.3, 1.0]
                    };
                    self.banner = Some((format!("{side}{how}"), col, 5.0));
                }
                Local::GameOver(summary) => {
                    if online {
                        self.match_result = Some(summary.clone());
                    } else if build_match {
                        self.chat.say(
                            "Partie finie. (L'OP qui la mène : /restart pour en refaire une, /bot \
                             off pour la fermer.)",
                            chat::YELLOW,
                        );
                    } else {
                        self.chat.say(
                            "Partie finie. /restart pour en refaire une, /bot off pour rester \
                             seul sur la carte.",
                            chat::YELLOW,
                        );
                    }
                    self.game_over = Some((summary, 0.0));
                }
                Local::Thrown {
                    owner,
                    nade,
                    pos,
                    vel,
                } => self.fps.add_thrown(owner, nade, pos, vel),
                Local::Started => {
                    if online {
                        self.room_started = Some(true);
                    }
                    self.game_over = None;
                }
            }
        }
    }

    /// Mode FPS, à chaque image : menu d'achat, tir, rechargement, choix de l'arme.
    /// Heure et soleil : le cycle de Minecraft (sauf `/timefix`), ou l'heure fixe de la
    /// carte en mode FPS (par défaut le soleil de Dust2).
    fn update_sun(&mut self, dt: f32) {
        use crate::daytime;
        if self.game.mode == GameMode::Fps {
            let t = *self
                .fps_time
                .get_or_insert_with(|| crate::fpsmap::load_time(&self.dir));
            match t {
                Some(h) => {
                    self.sun = daytime::sun_dir(h);
                    self.moon = daytime::moon_dir(h, 0.0);
                    self.daylight = daytime::daylight(h);
                }
                None => {
                    self.sun = daytime::dust2_sun();
                    self.moon = daytime::moon_opposite(self.sun);
                    self.daylight = 1.0;
                }
            }
            return;
        }
        // Heure « par défaut » : le soleil de Dust2, immobile.
        if self.time_h < 0.0 {
            self.sun = daytime::dust2_sun();
            self.moon = daytime::moon_opposite(self.sun);
            self.daylight = 1.0;
            return;
        }
        if !self.time_fixed {
            self.time_h = daytime::advance(self.time_h, dt);
            // Le ciel ne bouge (nuages) que si l'heure avance.
            self.sky_clock += dt;
            // (v216) La lune prend un jour de retard par jour : un cycle en huit jours.
            self.moon_age = (self.moon_age + dt / daytime::DAY_S).rem_euclid(daytime::MOON_DAYS);
        }
        self.sun = daytime::sun_dir(self.time_h);
        self.moon = daytime::moon_dir(self.time_h, self.moon_age);
        self.daylight = daytime::daylight(self.time_h);
    }

    /// Marche montée ou descendue (dalle, demi-bloc) : les pieds changent de hauteur d'un
    /// coup, la caméra suit en douceur (tous les modes de jeu).
    fn smooth_steps(&mut self, dt: f32) {
        let grounded = self.player.on_ground && !self.player.flying;
        if let (true, Some(prev)) = (grounded, self.step_prev) {
            let dy = self.player.pos.y - prev;
            if dy.abs() > 0.01 && dy.abs() < 0.65 {
                self.step_offset = (self.step_offset - dy).clamp(-0.65, 0.65);
            }
        }
        self.step_prev = grounded.then_some(self.player.pos.y);
        self.step_offset *= (-dt * 14.0).exp();
        if self.step_offset.abs() < 1e-3 {
            self.step_offset = 0.0;
        }
    }

    /// Volume du son des écrans vidéo (Paramètres → Son).
    pub fn set_screens_volume(&mut self, v: f32) {
        self.displays.set_volume(v);
    }

    /// Mode Build : plongeon dans l'eau après une chute d'au moins `SPLASH_HEIGHT` (le même
    /// bruit qu'en mode FPS). Pas en volant : on entre alors doucement dans l'eau.
    fn build_splash(&mut self) {
        if crate::player::in_water(&self.world, self.player.pos) {
            if let Some(top) = self.fall_top.take() {
                if top - self.player.pos.y >= crate::netgame::SPLASH_HEIGHT {
                    self.fps.sounds.push((crate::audio::Sfx::Splash, 0.0, 0.9));
                }
            }
        } else if self.player.on_ladder || self.player.on_ground || self.player.flying {
            self.fall_top = None;
        } else {
            // Plus haut point atteint en l'air.
            let y = self.player.pos.y;
            self.fall_top = Some(self.fall_top.map_or(y, |t| t.max(y)));
        }
    }

    fn update_fps(&mut self, input: &Input, dt: f32, renderer: &Renderer, typing: bool) {
        let (w, h) = renderer.size();
        let size = (w as f32, h as f32);
        if !typing && self.keys.pressed(input, Action::FpsBuy) {
            self.fps.buy_open = !self.fps.buy_open;
        } else {
            self.fps.buy_input(input, size);
        }
        // Vote de son équipe : F1 oui, F2 non (comme dans CS).
        if !typing {
            let yes = self.keys.pressed(input, Action::FpsVoteYes);
            let no = self.keys.pressed(input, Action::FpsVoteNo);
            if yes || no {
                let locals = match &mut self.net {
                    Some(n) if n.hud().vote.is_some_and(|v| v.can_vote) => n.ballot(yes),
                    _ => Vec::new(),
                };
                self.apply_net(locals);
            }
        }
        // Éditeur de lumières ouvert (FPS solo) : ni tir, ni arme, même en tournant la vue.
        // (v174) Mort (en partie) : plus de tir ni d'arme ; le clic change de joueur suivi.
        let free = input.grabbed
            && !typing
            && !self.fps.buy_open
            && !self.light_editor.open
            && !self.net_dead;
        // (v177) Figé au départ d'une manche (pauses comprises), à la mi-temps, après la
        // fin : on ne joue pas, on ne tire pas (ni grenade). Acheter, changer d'arme : oui.
        let still = self.net.as_ref().is_some_and(|n| {
            use minelib_net::game::phase;
            matches!(
                n.hud().phase,
                phase::FREEZE | phase::HALFTIME | phase::GAME_OVER
            )
        });
        let fire = free && !still;
        let k = &self.keys;
        let c = crate::fps::Controls {
            trigger_pressed: fire && k.pressed(input, Action::FpsFire),
            trigger_held: fire && k.held(input, Action::FpsFire),
            trigger_down: k.held(input, Action::FpsFire),
            reload: free && k.pressed(input, Action::FpsReload),
            scope: free && k.pressed(input, Action::FpsScope),
            slot: if !free {
                None
            } else if k.pressed(input, Action::FpsPrimary) {
                Some(0)
            } else if k.pressed(input, Action::FpsSecondary) {
                Some(1)
            } else if k.pressed(input, Action::FpsKnife) {
                Some(2)
            } else if k.pressed(input, Action::FpsGrenade) {
                Some(3)
            } else if k.pressed(input, Action::FpsBomb) {
                Some(4)
            } else {
                None
            },
            last_weapon: free && k.pressed(input, Action::FpsLastWeapon),
            next_weapon: free && k.pressed(input, Action::FpsNextWeapon),
            // Molette (comme dans CS) : vers le bas arme suivante, vers le haut précédente.
            wheel: if !free {
                0
            } else if k.pressed(input, Action::FpsPrevWeapon) || input.scroll >= 1.0 {
                -1
            } else if input.scroll <= -1.0 {
                1
            } else {
                0
            },
            nade_key: if !free {
                None
            } else {
                [
                    (Action::FpsHe, crate::equip::Nade::He),
                    (Action::FpsFlash, crate::equip::Nade::Flash),
                    (Action::FpsSmoke, crate::equip::Nade::Smoke),
                    (Action::FpsDecoy, crate::equip::Nade::Decoy),
                    (Action::FpsMolotov, crate::equip::Nade::Molotov),
                ]
                .into_iter()
                .find(|(a, _)| k.pressed(input, *a))
                .map(|(_, n)| n)
            },
            vel: self.player.vel,
            drop: free && k.pressed(input, Action::FpsDrop),
            rebuy: !typing
                && (k.pressed(input, Action::FpsRebuy) || k.pressed(input, Action::FpsAutoBuy)),
            speed: Vec3::new(self.player.vel.x, 0.0, self.player.vel.z).length(),
            on_ground: self.player.on_ground || self.player.flying,
        };
        let eye = self.player.eye();
        // Essais de performances : MINELIB_SMOKE=x,y,z pose une fumigène 6 s après le
        // départ, puis toutes les 18 s.
        if let Ok(v) = std::env::var("MINELIB_SMOKE") {
            let mut v: Vec<f32> = v.split(',').filter_map(|s| s.trim().parse().ok()).collect();
            // Un seul nombre : à cette distance devant le joueur.
            if v.len() == 1 {
                let (sy, cy) = self.player.yaw.sin_cos();
                let p = eye + Vec3::new(sy, 0.0, -cy) * v[0];
                v = vec![p.x, p.y + 1.0, p.z];
            }
            self.test_smoke_timer += dt;
            if v.len() == 3 && self.test_smoke_timer > 6.0 {
                self.test_smoke_timer = 6.0 - 18.0;
                self.fps
                    .spawn_test_smoke(Vec3::new(v[0], v[1], v[2]), &self.world);
            }
        }
        // (v216) De même MINELIB_FEU=<distance> : un feu de molotov devant soi, 4 s après
        // le départ, puis toutes les 9 s.
        if let Some(d) = std::env::var("MINELIB_FEU")
            .ok()
            .and_then(|v| v.trim().parse::<f32>().ok())
        {
            self.test_fire_timer += dt;
            if self.test_fire_timer > 4.0 {
                self.test_fire_timer = 4.0 - 9.0;
                let (sy, cy) = self.player.yaw.sin_cos();
                let p = eye + Vec3::new(sy, 0.0, -cy) * d;
                self.fps.spawn_test_fire(p, &self.world);
            }
        }
        // (v249) De même MINELIB_SANG=<distance> : du sang, des impacts et de la suie devant
        // soi (voir `Fps::spawn_test_traces`), 3 s après le départ, une fois.
        if let Some(d) = std::env::var("MINELIB_SANG")
            .ok()
            .and_then(|v| v.trim().parse::<f32>().ok())
        {
            if self.test_traces_timer < 3.0 {
                self.test_traces_timer += dt;
                if self.test_traces_timer >= 3.0 {
                    let dir = self.player.look_dir();
                    self.fps.spawn_test_traces(&self.world, eye, dir, d);
                }
            }
        }
        self.fps
            .update(dt, &c, &self.world, eye, self.player.yaw, self.player.pitch);
        self.fps.tick_effects(dt, &self.world);
        // Ses propres pas : en courant (les autres les entendent aussi, voir l'hôte), plus
        // doux en marchant ou accroupi (eux ne les entendent pas).
        let speed = Vec3::new(self.player.vel.x, 0.0, self.player.vel.z).length();
        let alive = !(self.net.is_some() && self.net_dead);
        self.own_ladder_noise(dt, alive);
        // Plongeon dans l'eau (les autres l'entendent aussi), sinon réception d'une chute
        // d'au moins 1,3 bloc.
        if crate::player::in_water(&self.world, self.player.pos) {
            if let Some(top) = self.fall_top.take() {
                if alive && top - self.player.pos.y >= crate::netgame::SPLASH_HEIGHT {
                    self.fps.sounds.push((crate::audio::Sfx::Splash, 0.0, 0.9));
                }
            }
        } else if self.player.on_ladder {
            self.fall_top = None;
        } else if self.player.on_ground || self.player.flying {
            if let Some(top) = self.fall_top.take() {
                if alive && top - self.player.pos.y >= crate::netgame::LAND_HEIGHT {
                    self.fps.sounds.push((crate::audio::Sfx::Land, 0.0, 0.9));
                }
            }
        } else {
            // Plus haut point atteint en l'air (un saut normal monte à ~1,45 bloc).
            let y = self.player.pos.y;
            self.fall_top = Some(self.fall_top.map_or(y, |t| t.max(y)));
        }
        // Course : ses pas ne s'entendent (pour lui comme pour les autres) qu'à partir de
        // 80 % de sa vitesse de course ; en marchant ou accroupi, il entend les siens, plus
        // doux.
        let run = speed > crate::netgame::step_speed(self.player.carry);
        let quiet = self.fps_walk_on || self.fps_duck_on;
        if alive && self.player.on_ground && !self.player.flying && speed > 0.6 && (run || quiet) {
            self.stride += speed * dt;
            let every = if run {
                crate::netgame::STRIDE
            } else {
                crate::netgame::STRIDE * 0.8
            };
            if self.stride >= every {
                self.stride -= every;
                let gain = if run { 0.75 } else { 0.3 };
                self.fps.sounds.push((crate::audio::Sfx::Step, 0.0, gain));
            }
        } else {
            self.stride = crate::netgame::STRIDE * 0.7;
        }
        for (sfx, delay, gain) in self.fps.sounds.drain(..) {
            crate::audio::play(sfx, delay, gain);
        }
        // Explosion, feu : mort à 0 point de vie (en ligne, l'hôte en décide).
        if self.fps.hp == 0 && self.net.is_none() {
            self.die();
        }
    }

    /// Échelle à pleine vitesse (pas accroupi) : ses barreaux, un bruit par bloc (v236 ; les
    /// autres l'entendent aussi : l'hôte en partie FPS, chacun d'eux dans le monde en
    /// ligne, voir `others_ladder_noise`). (v209) Dans tous les modes de jeu.
    fn own_ladder_noise(&mut self, dt: f32, alive: bool) {
        let vy = self.player.vel.y.abs();
        if alive && self.player.on_ladder && vy >= crate::player::LADDER_NOISY {
            self.ladder_climb += vy * dt;
            if self.ladder_climb >= crate::player::LADDER_RUNG {
                self.ladder_climb -= crate::player::LADDER_RUNG;
                self.fps.sounds.push((crate::audio::Sfx::Ladder, 0.0, 0.6));
            }
        } else {
            self.ladder_climb = crate::player::LADDER_RUNG * 0.8;
        }
    }

    /// (v209) Monde en ligne : les échelles grimpées à pleine vitesse par les joueurs
    /// proches s'entendent, comme en partie FPS. On ne connaît que leur position : sur une
    /// échelle et montant ou descendant vite, un bruit par bloc grimpé (v236).
    fn others_ladder_noise(&mut self, dt: f32) {
        let Some(b) = &self.build else {
            self.ladders.clear();
            return;
        };
        let eye = self.player.eye();
        let stamp = self.walk_stamp;
        let rung = crate::player::LADDER_RUNG;
        for o in b.others.values() {
            // (Pseudo recopié seulement à la première rencontre : voir `walk_stamp`.)
            if !self.ladders.contains_key(&o.pseudo) {
                self.ladders
                    .insert(o.pseudo.clone(), (o.pos.y, rung * 0.8, stamp));
            }
            let Some(c) = self.ladders.get_mut(&o.pseudo) else {
                continue;
            };
            let dy = (o.pos.y - c.0).abs();
            c.0 = o.pos.y;
            c.2 = stamp;
            let d = (o.pos - eye).length();
            // (Position adoucie à l'arrivée : un peu sous la vitesse de l'échelle.)
            let fast = dy / dt.max(1e-4) >= crate::player::LADDER_NOISY * 0.85 && dy < 1.0;
            if fast
                && d < crate::netgame::HEARD_STEPS
                && crate::player::ladder_at(&self.world, o.pos, o.crouch)
            {
                c.1 += dy;
                if c.1 >= rung {
                    c.1 -= rung;
                    self.fps
                        .sounds
                        .push((crate::audio::Sfx::Ladder, 0.0, 0.9 / (1.0 + d / 8.0)));
                }
            } else {
                c.1 = rung * 0.8;
            }
        }
        self.ladders.retain(|_, c| c.2 == stamp);
    }

    /// Coin de sélection posé avec la baguette.
    fn say_selection(&mut self, n: u8, p: [i32; 3]) {
        let size = self
            .we
            .region()
            .map(|(a, b)| {
                format!(
                    " ({} blocs)",
                    (b[0] - a[0] + 1) as i64 * (b[1] - a[1] + 1) as i64 * (b[2] - a[2] + 1) as i64
                )
            })
            .unwrap_or_default();
        // (v309) À sa place absolue.
        let o = self.world.origin_blocks();
        self.chat.say(
            format!(
                "Coin {n} : {} {} {}{size}",
                p[0].wrapping_add(o[0]),
                p[1],
                p[2].wrapping_add(o[2])
            ),
            chat::LIGHT_PURPLE,
        );
    }

    /// Commande WorldEdit (`//set stone`…).
    fn worldedit(&mut self, line: &str) {
        if self.game.mode != GameMode::Creative {
            self.chat.say(
                "Les commandes // sont réservées au mode créatif.",
                chat::RED,
            );
            return;
        }
        let mut parts = line.split_whitespace();
        let name = parts.next().unwrap_or("").to_lowercase();
        let args: Vec<&str> = parts.collect();
        if name == "wand" {
            // Baguette dans la main (emplacement actuel de la barre). (v247) Jamais par-dessus la
            // Foudre de Zeus : la case suivante.
            let Some(i) = self.inventory.replaceable_slot() else {
                return;
            };
            self.inventory.select(i);
            self.inventory.slots[i] = Some(inventory::Stack {
                item: Item::Wand,
                count: 1,
            });
            self.chat.say(
                "Baguette : clic gauche coin 1, clic droit coin 2, clic molette efface.",
                chat::LIGHT_PURPLE,
            );
            return;
        }
        let look = self.player.look_dir();
        let feet = self.player.pos.floor().as_ivec3().to_array();
        let target = self.hit.map(|h| h.block);
        // En ligne : ce que la commande change part au relais (réservé aux OP : refusé, tout
        // est remis comme avant), pas aux garants bloc par bloc.
        let before = self.world.journal.as_ref().map_or(0, Vec::len);
        let mut ctx = crate::worldedit::Ctx {
            world: &mut self.world,
            feet,
            look: look.to_array(),
            target,
            dirty: &mut self.dirty,
        };
        let r = self.we.command(&name, &args, &mut ctx);
        let snapped = r.is_ok()
            && matches!(name.as_str(), "pos1" | "pos2" | "hpos1" | "hpos2")
            && self.snap_door_selection();
        if let (Some(b), Some(j)) = (&mut self.build, &mut self.world.journal) {
            let cells: Vec<_> = j.drain(before.min(j.len())..).collect();
            if !cells.is_empty() {
                b.bulk(cells);
            }
        }
        match r {
            // (v258) Le coin a pris toute la porte : `snap_door_selection` l'a dit.
            Ok(_) if snapped => {}
            Ok(msg) => self.chat.say(msg, chat::LIGHT_PURPLE),
            Err(e) => self.chat.say(e, chat::RED),
        }
    }

    /// (v258) « Même si on sélectionne qu'un bloc, la sélection change quand c'est une porte et
    /// passe aux deux blocs de la porte, et aux quatre des deux portes » : une sélection tout
    /// entière dans une porte (un seul de ses blocs, ou un battant d'une double porte) devient
    /// toute la porte, ou toute la double porte, et le dit. `false` : rien de changé.
    fn snap_door_selection(&mut self) -> bool {
        let Some((a, b)) = self.we.region() else {
            return false;
        };
        let world = &self.world;
        let Some(d) = door_box(&|c| world.get(c[0], c[1], c[2]), a) else {
            return false;
        };
        let inside = (0..3).all(|k| a[k] >= d.min[k] && b[k] <= d.max[k]);
        if !inside || (a, b) == (d.min, d.max) {
            return false;
        }
        self.we.pos1 = Some(d.min);
        self.we.pos2 = Some(d.max);
        self.chat.say(
            if d.double {
                "Sélection : la double porte (4 blocs)."
            } else {
                "Sélection : la porte (2 blocs)."
            },
            chat::LIGHT_PURPLE,
        );
        true
    }

    /// (v258) `/fps set door` (`on`) ou `/fps clear door` : la porte visée, ou celles de la
    /// sélection, ajoutées à la carte (une boîte par porte, quatre blocs pour une double porte ;
    /// ce qui la chevauchait est remplacé) ou retirées.
    fn set_listed_doors(&mut self, map: &mut minelib_net::game::FpsMap, on: bool) {
        let doors = match self.doors_targeted() {
            Ok(d) => d,
            Err(e) => {
                self.chat.say(e, chat::RED);
                return;
            }
        };
        if doors.is_empty() {
            self.chat
                .say("Aucune porte dans la sélection.", chat::RED);
            return;
        }
        let overlaps = |z: &minelib_net::game::Zone, d: &DoorBox| {
            (0..3).all(|k| z.min[k] <= d.max[k] && z.max[k] >= d.min[k])
        };
        let mut full = 0;
        let origin = self.world.origin_blocks();
        for d in &doors {
            map.door_list.retain(|z| !overlaps(z, d));
            let zone = minelib_net::game::Zone::new(d.min, d.max);
            // (v322) Pas à cheval sur la limite du monde (`fps.txt` la garde par ses coins).
            if on && !crate::fpsmap::straddles(&zone, origin) {
                if map.door_list.len() < crate::fpsmap::MAX_DOORS {
                    map.door_list.push(zone);
                } else {
                    full += 1;
                }
            }
        }
        if full > 0 {
            self.chat.say(
                format!(
                    "{full} porte(s) non posée(s) : {} portes au plus par carte.",
                    crate::fpsmap::MAX_DOORS
                ),
                chat::RED,
            );
        }
        let doubles = doors.iter().filter(|d| d.double).count();
        let text = match (on, doors.len()) {
            (true, 1) if doubles == 1 => {
                "Double porte posée pour la partie (vert foncé) : elle s'ouvre en partie.".to_string()
            }
            (true, 1) => "Porte posée pour la partie (vert clair) : elle s'ouvre en partie.".to_string(),
            (true, n) => format!(
                "{n} portes posées pour la partie ({doubles} doubles) : elles s'ouvrent en partie."
            ),
            (false, 1) => "Porte retirée de la partie : elle reste fixe.".to_string(),
            (false, n) => format!("{n} portes retirées de la partie."),
        };
        match self.save_fps_map(map) {
            Ok(()) => {
                self.chat.say(
                    format!("{text} Nouvelle signature : c'est une nouvelle carte."),
                    chat::LIGHT_PURPLE,
                );
                // Les éléments affichés le voient tout de suite.
                if let Some(e) = self.fps_elements.as_mut() {
                    e.map = map.clone();
                }
            }
            Err(e) => self.chat.say(format!("fps.txt : {e:#}"), chat::RED),
        }
    }

    /// (v258) Portes que vise `/fps set door` ou `/fps clear door` : celles de la sélection de
    /// la baguette, ou celle qu'on vise (« qu'on puisse juste viser les portes pour les
    /// poser ») si elle n'est pas dans la sélection. `Err` : rien de visé ni de sélectionné, ou
    /// une sélection trop grande.
    fn doors_targeted(&self) -> Result<Vec<DoorBox>, &'static str> {
        let world = &self.world;
        let get = |c: [i32; 3]| world.get(c[0], c[1], c[2]);
        let region = self.we.region();
        let aimed = self.hit.and_then(|h| door_box(&get, h.block));
        if let Some(d) = aimed {
            let in_region = region.is_some_and(|(a, b)| {
                (0..3).all(|k| d.min[k] <= b[k] && d.max[k] >= a[k])
            });
            if !in_region {
                return Ok(vec![d]);
            }
        }
        let Some((a, b)) = region else {
            return Err("Vise une porte, ou sélectionne-la avec la baguette (un seul de ses blocs suffit).");
        };
        let volume = (0..3).map(|k| (b[k] - a[k] + 1) as u64).product::<u64>();
        if volume > 2_000_000 {
            return Err("Sélection trop grande pour y chercher des portes (2 millions de blocs au plus).");
        }
        let mut out: Vec<DoorBox> = Vec::new();
        for y in a[1]..=b[1] {
            for z in a[2]..=b[2] {
                for x in a[0]..=b[0] {
                    if let Some(d) = door_box(&get, [x, y, z]) {
                        if !out.iter().any(|o| o.min == d.min) {
                            out.push(d);
                        }
                    }
                }
            }
        }
        Ok(out)
    }

    /// Vue du joueur avec le recul du mode FPS (la vue remonte pendant une rafale).
    fn view_with_punch(&self) -> Mat4 {
        if self.game.mode != GameMode::Fps {
            // Marche montée ou descendue : la caméra suit en douceur (voir `smooth_steps`).
            // (v278) La même place que `view_camera` (la tête pliée pour manger, accroupie) :
            // la vue (tri des chunks, cases des lumières) tombe juste avec l'image.
            let eye = self.first_person_eye();
            return Mat4::look_to_rh(eye, self.player.look_dir(), Vec3::Y);
        }
        let p = (self.player.pitch + self.fps.punch.0.to_radians()).clamp(-1.55, 1.55);
        let y = self.player.yaw + self.fps.punch.1.to_radians();
        let dir = Vec3::new(y.sin() * p.cos(), p.sin(), -y.cos() * p.cos());
        Mat4::look_to_rh(self.player.eye() + Vec3::Y * self.step_offset, dir, Vec3::Y)
    }

    /// Message ou commande envoyé dans le tchat.
    /// Commandes de la partie en ligne et des cartes FPS. Renvoie vrai si traitée.
    fn online_command(&mut self, cmd: &str) -> bool {
        let words: Vec<&str> = cmd.split_whitespace().collect();
        let Some(first) = words.first() else {
            return false;
        };
        match first.to_lowercase().as_str() {
            // (v311) Supprimer le monde en ligne où l'on est (son créateur seulement : le
            // relais le vérifie). Un écran fait confirmer d'un clic, et demande le code de
            // double authentification si le compte en a une.
            "deleteworld" | "supprimermonde" => {
                if self.build.is_none() {
                    self.chat.say(
                        "/deleteworld supprime un monde en ligne, depuis ce monde, par son créateur.",
                        chat::RED,
                    );
                    return true;
                }
                self.delete_world_asked = true;
                true
            }
            // (v307) Signaler la carte de la partie FPS en cours.
            "reportmap" | "signalercarte" => {
                if self.net.is_none() {
                    self.chat.say(
                        "On ne signale une carte que pendant une partie FPS en ligne sur cette carte.",
                        chat::RED,
                    );
                    return true;
                }
                self.relay_cmds.push(minelib_net::client::Cmd::MapReport);
                self.chat.say(
                    "Carte signalée (une fois par compte). Si un quart de ceux qui y ont joué \
                     la signalent, elle ne compte plus pour le classement.",
                    chat::GREY,
                );
                true
            }
            "report" | "signaler" => {
                let Some(target) = words.get(1) else {
                    self.chat.say(
                        "Usage : /report <pseudo> (Tab complète le pseudo).",
                        chat::GREY,
                    );
                    return true;
                };
                let in_game = self
                    .net
                    .as_ref()
                    .is_some_and(|n| n.pseudos().iter().any(|p| p.eq_ignore_ascii_case(target)));
                if !in_game {
                    self.chat.say(
                        "On ne peut signaler qu'un joueur de sa partie, pendant la partie.",
                        chat::RED,
                    );
                    return true;
                }
                self.relay_cmds
                    .push(minelib_net::client::Cmd::Report(target.to_string()));
                true
            }
            // Se tuer (n'importe où hors partie FPS), ou tuer un joueur du serveur (OP).
            "kill" | "tuer" => {
                if self.net.is_some() {
                    self.chat
                        .say("/kill n'existe pas en partie FPS.", chat::RED);
                    return true;
                }
                match words.get(1) {
                    None => {
                        self.game.health = 0;
                        self.die();
                    }
                    Some(_) if self.build.is_none() => {
                        self.chat.say(
                            "Tuer un autre joueur : seulement sur un serveur en ligne (OP).",
                            chat::RED,
                        );
                    }
                    Some(target) => self.relay_cmds.push(minelib_net::client::Cmd::ServerAdmin {
                        action: minelib_net::proto::SERVER_KILL,
                        pseudo: target.to_string(),
                        reason: String::new(),
                    }),
                }
                true
            }
            // OP d'un serveur de construction (le créateur l'est d'office).
            "ban" | "bannir" | "unban" | "debannir" | "débannir" | "op" | "deop" => {
                use minelib_net::proto;
                let name = first.to_lowercase();
                if self.build.is_none() {
                    self.chat.say(
                        format!(
                            "/{name} : seulement sur un serveur de construction en ligne, \
                             pour ses OP."
                        ),
                        chat::RED,
                    );
                    return true;
                }
                let action = match name.as_str() {
                    "ban" | "bannir" => proto::SERVER_BAN,
                    "unban" | "debannir" | "débannir" => proto::SERVER_UNBAN,
                    "op" if words.len() < 2 => proto::SERVER_LIST,
                    "op" => proto::SERVER_OP,
                    _ => proto::SERVER_DEOP,
                };
                let target = words.get(1).map_or(String::new(), |t| t.to_string());
                if target.is_empty() && action != proto::SERVER_LIST {
                    self.chat.say(
                        match action {
                            proto::SERVER_BAN => "Usage : /ban <pseudo> [raison] (OP du serveur).",
                            proto::SERVER_UNBAN => "Usage : /unban <pseudo> (OP du serveur).",
                            _ => "Usage : /deop <pseudo> (créateur du serveur).",
                        },
                        chat::GREY,
                    );
                    return true;
                }
                self.relay_cmds.push(minelib_net::client::Cmd::ServerAdmin {
                    action,
                    pseudo: target,
                    reason: words.get(2..).map_or(String::new(), |w| w.join(" ")),
                });
                true
            }
            // (v328) « /seed (commande commune) et /seed lock true/false (commande op) pour
            // activer qu'on puisse avoir la seed de la map ou non ; si c'est locké on ne peut pas
            // télécharger la map dans Échap et on ne peut pas voir la seed avec /seed ; locké par
            // défaut » (Viendax, 25 novembre). En solo, la graine est au joueur.
            "seed" | "graine" => {
                // (Pas en partie FPS en ligne : ses commandes y sont comptées.)
                if self.ranked_match() {
                    return false;
                }
                let sub = words.get(1).map(|w| w.to_lowercase()).unwrap_or_default();
                if sub.is_empty() {
                    // (v330) Celle de la sauvegarde, ou que le relais nous a dite.
                    let known = self.seed_allowed().then(|| self.known_seed()).flatten();
                    if let Some(seed) = known {
                        self.chat
                            .say(format!("Graine du monde : {seed}"), chat::YELLOW);
                    } else if self.seed_allowed() || self.seed_lock.is_none() {
                        self.chat.say(
                            "Graine du monde : le serveur ne l'a pas encore dit, réessaie dans un instant.",
                            chat::GREY,
                        );
                    } else {
                        self.chat.say(
                            "La graine de ce monde est verrouillée par ses OP (eux seuls la voient).",
                            chat::RED,
                        );
                    }
                    return true;
                }
                let lock = match (sub.as_str(), words.get(2).and_then(|w| chat::switch(w))) {
                    ("lock" | "verrou" | "verrouiller", Some(on)) => Some(on),
                    ("lock" | "verrou" | "verrouiller", None) if words.len() == 2 => Some(true),
                    ("unlock" | "deverrouiller" | "déverrouiller", None) => Some(false),
                    _ => None,
                };
                let Some(lock) = lock else {
                    self.chat.say(
                        "Usage : /seed (la graine du monde), /seed lock true|false (la verrouiller ou non : OP d'un serveur de construction).",
                        chat::GREY,
                    );
                    return true;
                };
                if self.build.is_none() {
                    self.chat.say(
                        "/seed lock : sur un serveur de construction en ligne, pour ses OP (en solo, la graine est à toi).",
                        chat::RED,
                    );
                    return true;
                }
                self.seed_lock_asked = Some(std::time::Instant::now());
                self.relay_cmds.push(minelib_net::client::Cmd::ServerAdmin {
                    action: minelib_net::proto::SERVER_SEED,
                    pseudo: String::new(),
                    reason: if lock { "lock" } else { "unlock" }.into(),
                });
                true
            }
            // Liste blanche d'un serveur de construction (OP) : comme dans Minecraft.
            "whitelist" | "withlist" | "whitlist" | "whithlist" | "wl" | "listeblanche" => {
                use minelib_net::proto;
                if self.build.is_none() {
                    self.chat.say(
                        "/whitelist : seulement sur un serveur de construction en ligne, pour ses OP.",
                        chat::RED,
                    );
                    return true;
                }
                let sub = words.get(1).map(|w| w.to_lowercase()).unwrap_or_default();
                // (v197) on/off, true/false, 1/0, oui/non (`chat::switch`).
                let action = match (sub.as_str(), chat::switch(&sub)) {
                    ("activer", _) | (_, Some(true)) => proto::SERVER_WL_ON,
                    ("desactiver" | "désactiver", _) | (_, Some(false)) => proto::SERVER_WL_OFF,
                    ("add" | "ajouter", _) => proto::SERVER_WL_ADD,
                    ("remove" | "retirer" | "del", _) => proto::SERVER_WL_REMOVE,
                    ("list" | "liste" | "", _) => proto::SERVER_WL_LIST,
                    _ => {
                        self.chat.say(
                            "Usage : /whitelist on | off | add <pseudo> | remove <pseudo> | list",
                            chat::GREY,
                        );
                        return true;
                    }
                };
                let target = words.get(2).map_or(String::new(), |t| t.to_string());
                if target.is_empty()
                    && (action == proto::SERVER_WL_ADD || action == proto::SERVER_WL_REMOVE)
                {
                    self.chat
                        .say(format!("Usage : /whitelist {sub} <pseudo>"), chat::GREY);
                    return true;
                }
                self.relay_cmds.push(minelib_net::client::Cmd::ServerAdmin {
                    action,
                    pseudo: target,
                    reason: String::new(),
                });
                true
            }
            // Triche seulement : banni de tout le relais (administrateurs, admins.txt).
            // Sinon on n'est banni que d'un serveur (/ban de ses OP).
            "bantriche" => {
                let (Some(target), true) = (words.get(1), words.len() > 2) else {
                    self.chat.say(
                        "Usage : /bantriche <pseudo> <preuve de la triche> (administrateurs \
                         du relais). Pour le reste, /ban sur un serveur.",
                        chat::GREY,
                    );
                    return true;
                };
                let reason = words[2..].join(" ");
                self.relay_cmds.push(minelib_net::client::Cmd::Ban {
                    pseudo: target.to_string(),
                    reason,
                });
                true
            }
            "unreport" | "designaler" => {
                let Some(target) = words.get(1) else {
                    self.chat.say("Usage : /unreport <pseudo>.", chat::GREY);
                    return true;
                };
                self.relay_cmds
                    .push(minelib_net::client::Cmd::Unreport(target.to_string()));
                true
            }
            "ami" | "amis" | "friend" | "friends" => {
                // Dans n'importe quel ordre : /ami add Bob, /ami Bob add, /friend Bob.
                let verb = |w: &str| {
                    let w = w.to_lowercase();
                    match w.as_str() {
                        "retirer" | "remove" | "enlever" | "del" => Some(false),
                        "ajouter" | "add" => Some(true),
                        _ => None,
                    }
                };
                let rest = &words[1..];
                let add = rest.iter().find_map(|w| verb(w)).unwrap_or(true);
                let pseudo = rest
                    .iter()
                    .find(|w| verb(w).is_none())
                    .map_or(String::new(), |p| p.to_string());
                self.relay_cmds
                    .push(minelib_net::client::Cmd::Friend { pseudo, add });
                self.friends_said = true;
                true
            }
            "equipe" | "équipe" | "team" => {
                self.chat.say(
                    "On ne change pas d'équipe : elles sont faites au lancement de la partie.",
                    chat::GREY,
                );
                true
            }
            "fps" => {
                self.fps_command(&words[1..]);
                true
            }
            _ => false,
        }
    }

    /// (v251) Ce qui est enregistré pour une commande (par son premier nom), montré dans son
    /// aide pendant qu'on la tape (`cmdhelp`).
    fn command_state(&mut self, name: &str) -> Vec<String> {
        let on = |b: bool| if b { "oui" } else { "non" };
        match name {
            "fps" => {
                let map = fps_map_of(&self.dir, &self.world);
                let zone = |z: &Option<minelib_net::game::Zone>| if z.is_some() { "posé" } else { "—" };
                let sites: Vec<String> = map
                    .sites
                    .iter()
                    .enumerate()
                    .map(|(k, z)| format!("site {} {}", crate::fpsmap::SITE_NAMES[k], zone(z)))
                    .collect();
                vec![
                    format!(
                        "joueurs : {} ({} contre {})",
                        map.players(),
                        map.players() / 2,
                        map.players() / 2
                    ),
                    format!(
                        "départ CT {}, départ T {}, {}",
                        zone(&map.ct),
                        zone(&map.t),
                        sites.join(", ")
                    ),
                    format!(
                        "portes : {} ; posées : {} ; doubles portes : {}",
                        if map.doors() { "toutes utilisables" } else { "fixes" },
                        map.door_list.len(),
                        if map.door_sync() { "ensemble" } else { "séparées" }
                    ),
                    format!(
                        "éléments affichés à travers les murs : {}",
                        on(self.fps_elements.is_some())
                    ),
                ]
            }
            "bot" | "start" | "restart" => {
                let mut v = vec![format!(
                    "niveau : {} ; joueurs par partie : {}",
                    crate::bots::LEVELS[crate::bots::saved_level() as usize],
                    crate::bots::saved_players()
                        .map_or_else(|| "ceux de la carte".to_string(), |n| n.to_string())
                )];
                if let Some(n) = &self.net {
                    if n.is_solo() || n.is_build() {
                        v.push(format!(
                            "partie en cours : {} joueur(s) et {} bot(s)",
                            n.human_count(),
                            n.bot_count()
                        ));
                    }
                }
                v
            }
            "gm" => vec![format!("mode : {}", self.game.mode.label())],
            "fly" => vec![format!("vol permis : {}", on(self.game.can_fly))],
            "weather" => vec![format!(
                "météo : {}{}",
                self.storm.weather.kind().label(),
                if self.storm.weather.fixed { " (figée)" } else { "" }
            )],
            "timeset" | "timefix" => vec![if self.time_h < 0.0 {
                "heure : le soleil de Dust2, immobile".to_string()
            } else {
                format!(
                    "il est {}{}",
                    crate::daytime::format(self.time_h),
                    if self.time_fixed { " (temps arrêté)" } else { "" }
                )
            }],
            "lumieres" => vec![format!("lumières dans ce monde : {}", self.lights.scene.len())],
            "npc" => vec![format!("foule d'essai : {}", on(self.crowd_on))],
            "worldname" => vec![format!("nom : {}", self.meta.name)],
            _ => Vec::new(),
        }
    }

    /// `/fps set ct|t spawn`, `/fps set a|b|c site` (zone sélectionnée à la baguette),
    /// `/fps clear …`, `/fps info`.
    /// (v340) Ce joueur est-il le compte qui certifie les cartes FPS (`proto::CERTIFIER`) ?
    /// Lui seul voit et peut taper `/fps certif` (le relais le vérifie aussi).
    pub fn certifier(&self) -> bool {
        minelib_net::proto::can_certify(&self.pseudo)
    }

    fn fps_command(&mut self, args: &[&str]) {
        use minelib_net::game::Zone;
        // (v322) Partout, loin du centre du monde aussi (v309 : refusé au-delà de 32 768
        // blocs) : `fps.txt` garde les zones en places absolues (`save_fps_map`), la partie
        // les compte depuis son origine (`fps_map`).
        const USAGE: &str = "Usage : /fps set spawn ct | spawn t | site a | site b | site c \
                             (sélection à la baguette), /fps set maxplayer 2|4|6|8|10|20, /fps set door on|off, /fps set doorsync on|off, /fps clear …, \
                             /fps showelements on|off, /fps info";
        // (v255) « Un /fps showelements true/false pour voir les portes, sites et spawns à
        // travers les murs (comme une sélection de la baguette) en build » : un affichage,
        // permis aussi pendant une partie de bots (il ne se voit qu'hors mode FPS).
        if matches!(
            args.first().map(|a| a.to_lowercase()).as_deref(),
            Some("showelements" | "showelement" | "elements" | "éléments")
        ) {
            let on = match args.get(1) {
                Some(w) => match chat::switch(w) {
                    Some(on) => on,
                    None => {
                        self.chat
                            .say("Usage : /fps showelements on|off (ou true|false).", chat::GREY);
                        return;
                    }
                },
                None => self.fps_elements.is_none(),
            };
            if on {
                self.fps_elements = Some(FpsElements {
                    map: fps_map_of(&self.dir, &self.world),
                    ..Default::default()
                });
                self.chat.say(
                    "Éléments de la carte FPS montrés à travers les murs (hors partie) : portes \
                     (posées avec /fps set door, ou toutes avec /fps set door on) en vert clair, \
                     doubles portes en vert foncé, sites en rouge, départ CT en bleu, départ T en \
                     jaune. /fps showelements off pour les masquer.",
                    chat::LIGHT_PURPLE,
                );
            } else {
                self.fps_elements = None;
                self.chat
                    .say("Éléments de la carte FPS masqués.", chat::LIGHT_PURPLE);
            }
            return;
        }
        // (v340) `/fps certif true|false` : le compte `proto::CERTIFIER` seulement (pour les
        // autres, la commande n'existe pas). La carte de ce monde, telle qu'elle est
        // enregistrée (sa signature), passe en tête de la liste FPS de tous les joueurs ; aussi
        // pendant une partie sur elle.
        if self.certifier()
            && matches!(
                args.first().map(|a| a.to_lowercase()).as_deref(),
                Some("certif" | "certify" | "certifier")
            )
        {
            let Some(on) = args.get(1).and_then(|w| chat::switch(w)) else {
                self.chat.say(
                    "Usage : /fps certif true|false (la carte de ce monde en tête de la liste \
                     FPS, ou plus).",
                    chat::GREY,
                );
                return;
            };
            match crate::fpsmap::package(&self.dir) {
                Ok(p) => {
                    self.relay_cmds.push(minelib_net::client::Cmd::MapCertify {
                        sig: p.signature(),
                        on,
                    });
                }
                Err(e) => self
                    .chat
                    .say(format!("Carte de ce monde illisible : {e:#}"), chat::RED),
            }
            return;
        }
        if self.net.is_some() {
            let msg = if self.online_match() {
                "Les réglages de carte se font hors partie en ligne."
            } else {
                "Les réglages de carte se font sans les bots : /bots off d'abord."
            };
            self.chat.say(msg, chat::RED);
            return;
        }
        let mut map = fps_map_of(&self.dir, &self.world);
        let lower: Vec<String> = args.iter().map(|a| a.to_lowercase()).collect();
        let what = |l: &[String]| -> Option<usize> {
            // Dans les deux ordres : « spawn ct » ou « ct spawn », « site a » ou « a site ».
            let pair = (l.first().map(String::as_str), l.get(1).map(String::as_str));
            let (a, b) = match pair {
                (Some(x @ ("spawn" | "site" | "depart" | "départ")), Some(y)) => (y, x),
                (Some(y), Some(x)) => (y, x),
                _ => return None,
            };
            match (a, b) {
                ("ct", "spawn" | "depart" | "départ") => Some(0),
                ("t", "spawn" | "depart" | "départ") => Some(1),
                ("a", "site") => Some(2),
                ("b", "site") => Some(3),
                ("c", "site") => Some(4),
                _ => None,
            }
        };
        let name = |k: usize| match k {
            0 => "départ CT".to_string(),
            1 => "départ T".to_string(),
            k => format!("site {}", crate::fpsmap::SITE_NAMES[k - 2]),
        };
        fn slot(map: &mut minelib_net::game::FpsMap, k: usize) -> &mut Option<Zone> {
            match k {
                0 => &mut map.ct,
                1 => &mut map.t,
                k => &mut map.sites[k - 2],
            }
        }
        // Nombre de joueurs de la carte (5 contre 5 par défaut ; seule la 5v5 est classée).
        if lower.first().map(String::as_str) == Some("set")
            && matches!(
                lower.get(1).map(String::as_str),
                Some("maxplayer" | "maxplayers" | "joueurs")
            )
        {
            // (v340) Les formats de « Équipe » (Multijoueur → FPS) seulement : 2, 4, 6, 8, 10
            // ou 20 joueurs (aussi écrits « 2v2 »…). Une carte d'un autre nombre n'y serait
            // dans aucun.
            let n = lower.get(2).and_then(|n| match n.split_once('v') {
                Some((a, b)) if a == b => a.parse::<u8>().ok().map(|t| t.saturating_mul(2)),
                _ => n.parse::<u8>().ok(),
            });
            match n {
                Some(n)
                    if n <= proto_max()
                        && n % 2 == 0
                        && crate::multiplayer::TEAM_SIZES.contains(&(n / 2)) =>
                {
                    map.set_players(n);
                    match self.save_fps_map(&map) {
                        Ok(()) => self.chat.say(
                            format!(
                                "Carte pour {n} joueurs ({} contre {}){}. Nouvelle signature : \
                                 c'est une nouvelle carte dans la liste FPS.",
                                n / 2,
                                n / 2,
                                if n == 10 {
                                    ""
                                } else {
                                    ", hors classement (seul le 5 contre 5 compte)"
                                }
                            ),
                            chat::LIGHT_PURPLE,
                        ),
                        Err(e) => self.chat.say(format!("fps.txt : {e:#}"), chat::RED),
                    }
                }
                _ => self.chat.say(
                    format!(
                        "Usage : /fps set maxplayer {} (joueurs de la carte : {}).",
                        crate::multiplayer::TEAM_SIZES
                            .iter()
                            .map(|t| (2 * t).to_string())
                            .collect::<Vec<_>>()
                            .join(" | "),
                        crate::multiplayer::TEAM_SIZES
                            .iter()
                            .map(|&t| crate::multiplayer::team_label(t))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    chat::GREY,
                ),
            }
            return;
        }
        // Heure de la carte : l'heure actuelle, une heure donnée, ou « default » (le soleil
        // de Dust2 dans CS2).
        if lower.first().map(String::as_str) == Some("set")
            && matches!(lower.get(1).map(String::as_str), Some("time" | "heure"))
        {
            let time = match lower.get(2).map(String::as_str) {
                None => (self.time_h >= 0.0).then_some(self.time_h),
                Some("default" | "defaut" | "défaut") => None,
                Some(t) => match crate::daytime::parse(t) {
                    Some(h) => Some(h),
                    None => {
                        self.chat.say(
                            "Usage : /fps set time [heure | default] (11, 11:20, 11.20, 11h20).",
                            chat::GREY,
                        );
                        return;
                    }
                },
            };
            match crate::fpsmap::save_time(&self.dir, time) {
                Ok(()) => {
                    self.fps_time = Some(time);
                    self.chat.say(
                        match time {
                            Some(h) => format!(
                                "Heure de la carte : {}. Nouvelle signature : c'est une nouvelle carte.",
                                crate::daytime::format(h)
                            ),
                            None => "Heure de la carte : soleil de Dust2 (par défaut). Nouvelle \
                                     signature : c'est une nouvelle carte."
                                .to_string(),
                        },
                        chat::LIGHT_PURPLE,
                    );
                }
                Err(e) => self.chat.say(format!("heure.txt : {e:#}"), chat::RED),
            }
            return;
        }
        // Double porte : les deux battants s'ouvrent ensemble.
        if lower.first().map(String::as_str) == Some("set")
            && matches!(
                lower.get(1).map(String::as_str),
                Some("doorsync" | "doublesdoors" | "doubledoor" | "portesdoubles")
            )
        {
            let Some(on) = lower.get(2).map(String::as_str).and_then(chat::switch) else {
                self.chat.say(
                    format!(
                        "Usage : /fps set doorsync on|off (actuellement : {}).",
                        on_off(map.door_sync())
                    ),
                    chat::GREY,
                );
                return;
            };
            map.set_door_sync(on);
            match self.save_fps_map(&map) {
                Ok(()) => self.chat.say(
                    if on {
                        "Doubles portes : ouvrir un battant ouvre aussi l'autre en partie. \
                         Nouvelle signature : c'est une nouvelle carte."
                    } else {
                        "Doubles portes : chaque battant s'ouvre seul en partie. Nouvelle \
                         signature : c'est une nouvelle carte."
                    },
                    chat::LIGHT_PURPLE,
                ),
                Err(e) => self.chat.say(format!("fps.txt : {e:#}"), chat::RED),
            }
            return;
        }
        // Portes et trappes utilisables pendant la partie.
        if lower.first().map(String::as_str) == Some("set")
            && matches!(
                lower.get(1).map(String::as_str),
                Some("door" | "doors" | "porte" | "portes")
            )
        {
            // (v258) Sans on/off : la porte visée, ou celles de la sélection, posées pour la
            // partie (« les portes, c'est avec la baguette aussi qu'on les pose »).
            if lower.get(2).is_none() {
                self.set_listed_doors(&mut map, true);
                return;
            }
            let Some(on) = lower.get(2).map(String::as_str).and_then(chat::switch) else {
                self.chat.say(
                    format!(
                        "Usage : /fps set door (la porte visée ou sélectionnée), /fps set door \
                         on|off (toutes les portes ; actuellement : {}).",
                        on_off(map.doors())
                    ),
                    chat::GREY,
                );
                return;
            };
            map.set_doors(on);
            match self.save_fps_map(&map) {
                Ok(()) => self.chat.say(
                    if on {
                        "Toutes les portes et trappes s'ouvrent et se ferment en partie (touche \
                         Utiliser). Nouvelle signature : c'est une nouvelle carte."
                    } else {
                        "Portes et trappes fixes en partie, sauf celles posées avec /fps set door. \
                         Nouvelle signature : c'est une nouvelle carte."
                    },
                    chat::LIGHT_PURPLE,
                ),
                Err(e) => self.chat.say(format!("fps.txt : {e:#}"), chat::RED),
            }
            return;
        }
        match lower.first().map(String::as_str) {
            Some("set") => {
                let Some(k) = what(&lower[1..]) else {
                    self.chat.say(USAGE, chat::GREY);
                    return;
                };
                let Some((a, b)) = self.we.region() else {
                    self.chat.say(
                        "Sélectionne d'abord la zone avec la baguette (clic gauche, clic droit).",
                        chat::RED,
                    );
                    return;
                };
                let zone = Zone::new(a, b);
                // (v322) D'un seul côté de la limite du monde (carte bouclée) : `fps.txt` la
                // garde par ses deux coins absolus.
                if crate::fpsmap::straddles(&zone, self.world.origin_blocks()) {
                    self.chat.say(
                        "Une zone de la carte FPS ne peut pas être à cheval sur la limite du \
                         monde : choisis-la d'un côté ou de l'autre.",
                        chat::RED,
                    );
                    return;
                }
                *slot(&mut map, k) = Some(zone);
            }
            Some("clear")
                if matches!(
                    lower.get(1).map(String::as_str),
                    Some("door" | "doors" | "porte" | "portes")
                ) =>
            {
                if matches!(lower.get(2).map(String::as_str), Some("all" | "tout" | "toutes")) {
                    let n = map.door_list.len();
                    map.door_list.clear();
                    match self.save_fps_map(&map) {
                        Ok(()) => self.chat.say(
                            format!("{n} porte(s) retirée(s) de la partie."),
                            chat::LIGHT_PURPLE,
                        ),
                        Err(e) => self.chat.say(format!("fps.txt : {e:#}"), chat::RED),
                    }
                } else {
                    self.set_listed_doors(&mut map, false);
                }
                return;
            }
            Some("clear") => {
                let Some(k) = what(&lower[1..]) else {
                    self.chat.say(USAGE, chat::GREY);
                    return;
                };
                *slot(&mut map, k) = None;
            }
            Some("info") | None => {
                self.chat
                    .say(format!("joueurs : {}", map.players()), chat::LIGHT_PURPLE);
                self.chat.say(
                    format!(
                        "portes : {} ; posées pour la partie : {}",
                        if map.doors() { "toutes utilisables" } else { "fixes" },
                        map.door_list.len()
                    ),
                    chat::LIGHT_PURPLE,
                );
                self.chat.say(
                    format!(
                        "doubles portes : {}",
                        if map.door_sync() {
                            "ensemble"
                        } else {
                            "séparées"
                        }
                    ),
                    chat::LIGHT_PURPLE,
                );
                // (v322) Les zones en places absolues (celles des blocs, de F3).
                let mut shown = crate::fpsmap::absolute(&map, self.world.origin_blocks());
                for k in 0..5 {
                    let z = *slot(&mut shown, k);
                    let t = match z {
                        Some(z) => format!("{} : {:?} → {:?}", name(k), z.min, z.max),
                        None => format!("{} : pas défini", name(k)),
                    };
                    self.chat.say(t, chat::LIGHT_PURPLE);
                }
                return;
            }
            _ => {
                self.chat.say(USAGE, chat::GREY);
                return;
            }
        }
        match self.save_fps_map(&map) {
            Ok(()) => {
                // (v258) Les éléments affichés le voient tout de suite (la sélection passe à
                // sa couleur).
                if let Some(e) = self.fps_elements.as_mut() {
                    e.map = map.clone();
                }
                let k = what(&lower[1..]).unwrap_or(0);
                let verb = if lower[0] == "set" {
                    "défini"
                } else {
                    "effacé"
                };
                self.chat.say(
                    format!(
                        "{} {verb}. La carte change de signature : c'est une nouvelle carte \
                         dans la liste FPS.",
                        name(k)
                    ),
                    chat::LIGHT_PURPLE,
                );
            }
            Err(e) => self.chat.say(format!("fps.txt : {e:#}"), chat::RED),
        }
    }

    fn submit(&mut self, text: &str) {
        let Some(cmd) = text.strip_prefix('/') else {
            if let Some(n) = &mut self.net {
                n.chat(text);
                if n.is_host() {
                    self.chat.say(format!("<moi> {text}"), chat::WHITE);
                }
                return;
            }
            self.chat.say(format!("<Joueur> {text}"), chat::WHITE);
            return;
        };
        // `/yt [recherche | adresse]` : YouTube sur les écrans vidéo (navigateur local,
        // permis aussi en ligne).
        if cmd == "yt" || cmd.starts_with("yt ") {
            self.youtube(&cmd[2..]);
            return;
        }
        // (v230) `/fusion [on|off]` : fusion des faces de blocs voisines que rien ne
        // distingue (moins de triangles : voir `mesh::merge_flat_faces`). Réglage du rendu,
        // chez soi : permis aussi en ligne. Pour comparer l'image et les images par seconde.
        if cmd == "fusion" || cmd.starts_with("fusion ") {
            use std::sync::atomic::Ordering::Relaxed;
            let flag = &minelib_world::mesh::MERGE_FACES;
            let on = match chat::switch(cmd[6..].trim()) {
                Some(v) => v,
                None if cmd[6..].trim().is_empty() => !flag.load(Relaxed),
                None => {
                    self.chat
                        .say("Utilisation : /fusion [on|off] (seul : bascule).", chat::RED);
                    return;
                }
            };
            flag.store(on, Relaxed);
            let n = self.streamer.remesh_all(&self.world, &mut self.workers);
            self.chat.say(
                format!(
                    "Fusion des faces voisines : {} ({n} chunks remaillés).",
                    if on { "activée" } else { "désactivée" }
                ),
                chat::YELLOW,
            );
            return;
        }
        if self.online_command(cmd) {
            return;
        }
        // Partie FPS en ligne : aucune autre commande (pas de triche : mode de jeu, vol,
        // objets, santé, WorldEdit…). Construction en ligne : pas de WorldEdit (les blocs
        // passent un par un par le garant). (Contre des bots en solo : tout est permis.)
        // (v243) La partie d'un serveur de construction garde les commandes du serveur (le
        // relais y vérifie les OP).
        if self.ranked_match() {
            self.chat.say(
                "Commande interdite en partie en ligne. Permises : /report, /unreport, /ami, \
                 /equipe.",
                chat::RED,
            );
            return;
        }

        // `//…` : commandes WorldEdit (mode créatif).
        if let Some(we) = cmd.strip_prefix('/') {
            self.worldedit(we);
            return;
        }
        let mut parts = cmd.split_whitespace();
        let name = parts.next().unwrap_or("").to_lowercase();
        let arg = parts.next();
        match name.as_str() {
            // Vol en survie : (v197) /fly on|off, ou /fly seul pour basculer.
            "fly" => {
                let want = arg.map(chat::switch);
                if self.game.mode == GameMode::Creative {
                    self.chat.say(
                        "En créatif le vol est toujours permis : appuyez deux fois sur Espace.",
                        chat::GREY,
                    );
                } else if want == Some(None) {
                    self.chat
                        .say("Utilisation : /fly [on|off] (seul : bascule).", chat::RED);
                } else {
                    self.game.can_fly = want.flatten().unwrap_or(!self.game.can_fly);
                    if !self.game.can_fly && self.player.flying {
                        self.player.flying = false;
                    }
                    self.chat.say(
                        if self.game.can_fly {
                            "Vol autorisé : appuyez deux fois sur Espace pour voler."
                        } else {
                            "Vol désactivé."
                        },
                        chat::YELLOW,
                    );
                }
            }
            // Serveur de construction en ligne : seuls ses OP changent de mode de jeu (le
            // relais vérifie, puis renvoie le mode au joueur visé).
            "gamemode" | "gm" | "defaultgamemode" | "dgm" if self.build.is_some() => {
                let default = matches!(name.as_str(), "defaultgamemode" | "dgm");
                let who = parts.next().map_or(String::new(), str::to_string);
                let none = default && matches!(arg, Some("none" | "aucun" | "off"));
                let mode = arg.and_then(GameMode::parse);
                if mode.is_some() || none {
                    self.relay_cmds.push(minelib_net::client::Cmd::ServerAdmin {
                        action: if default {
                            minelib_net::proto::SERVER_DEFAULT_GAMEMODE
                        } else {
                            minelib_net::proto::SERVER_GAMEMODE
                        },
                        pseudo: who,
                        reason: mode.map_or(String::new(), |m| m.key().to_string()),
                    });
                } else {
                    self.chat.say(
                        if default {
                            "Utilisation (OP) : /defaultgamemode <survival|creative|fps|spectator|none>"
                        } else {
                            "Utilisation (OP) : /gm <survival|creative|fps|spectator> [pseudo]"
                        },
                        chat::RED,
                    );
                }
            }
            // (v165, essai) Foule simulée en solo : (v185) `/npc stresstest true|false`,
            // éteinte par défaut ; (v197) aussi on/off, 1/0, oui/non. (v200, essai)
            // `/npc reflets on|off` : les joueurs reflétés dans l'eau, allumés par défaut.
            "npc" => {
                let sub = arg.map(str::to_lowercase);
                let value = parts.next().and_then(chat::switch);
                match (sub.as_deref(), value) {
                    (Some("stresstest"), Some(false)) => {
                        self.stop_crowd();
                        self.chat.say("Essai de foule arrêté.", chat::YELLOW);
                    }
                    // (v227) En construction en ligne : seulement un OP (le relais le dit, comme
                    // pour `/thor`), pour les joueurs proches aussi.
                    (Some("stresstest"), Some(true)) if self.build.is_some() => {
                        if !self
                            .crowd_probe
                            .is_some_and(|t| t.elapsed().as_secs_f32() < 5.0)
                        {
                            self.crowd_probe = Some(std::time::Instant::now());
                            self.relay_cmds.push(minelib_net::client::Cmd::ServerAdmin {
                                action: minelib_net::proto::SERVER_LIST,
                                pseudo: String::new(),
                                reason: String::new(),
                            });
                        }
                    }
                    (Some("stresstest"), Some(true)) => {
                        self.crowd_on = true;
                        self.crowd = None;
                        // (v226) On peut les frapper, et le personnage caché est parmi eux.
                        self.chat.say(
                            format!(
                                "Essai de foule : jusqu'à {} joueurs simulés autour de toi en \
                                 2 minutes, en solo hors mode FPS. Un clic gauche les frappe ; \
                                 {} se cache parmi eux.",
                                crate::crowd::MAX,
                                crate::crowd::HIDDEN_NAME
                            ),
                            chat::YELLOW,
                        );
                    }
                    (Some("reflets" | "reflet" | "reflections"), Some(on)) => {
                        self.water_mirror_on = on;
                        self.chat.say(
                            if on {
                                "Reflets des joueurs dans l'eau (essai, sans raytracing) : \
                                 allumés."
                            } else {
                                "Reflets des joueurs dans l'eau : éteints."
                            },
                            chat::YELLOW,
                        );
                    }
                    _ => self.chat.say(
                        "Utilisation : /npc stresstest <on|off>, /npc reflets <on|off>",
                        chat::RED,
                    ),
                }
            }
            // Compensation du retard mesuré de la touche « avancer » (voir `keycomp`).
            "compense" | "compensation" => {
                let want = arg.and_then(chat::switch);
                let text = match (self.key_lag, want) {
                    (None, _) => "Aucun retard mesuré : fais d'abord /testtouche avec ta touche \
                                  avancer."
                        .to_string(),
                    (Some(s), Some(on)) => {
                        let s = crate::keycomp::Setting { on, ..s };
                        crate::keycomp::save(&s);
                        self.key_lag = Some(s);
                        if on {
                            format!(
                                "Retard de la touche avancer compensé ({:.0} ms).",
                                s.lag * 1000.0
                            )
                        } else {
                            "Retard de la touche avancer plus compensé.".to_string()
                        }
                    }
                    (Some(s), None) => format!(
                        "Retard mesuré de la touche avancer : {:.0} ms, {}. Utilisation : \
                         /compense <on|off>",
                        s.lag * 1000.0,
                        if s.on { "compensé" } else { "pas compensé" }
                    ),
                };
                self.chat.say(text, chat::YELLOW);
            }
            // Test des touches : une touche annonce-t-elle son relâchement en retard ?
            "testtouche" | "testtouches" | "keytest" => {
                self.key_test = Some(crate::keytest::KeyTest::new());
                log::info!("test des touches : commencé");
                for line in [
                    "Test des touches : tiens UNE touche (avancer, par exemple) et le clic \
                     gauche en même temps pendant 10 secondes.",
                    "Quand le jeu te le dit, lâche les deux d'un seul coup. Fais-le 3 fois, \
                     puis avec d'autres touches pour comparer.",
                    "Pendant le test tu ne bouges pas. Échap : fin du test et bilan.",
                ] {
                    self.chat.say(line.to_string(), chat::YELLOW);
                }
            }
            // Touche « avancer » qui remonte mal : règle forcée, refusée ou apprise.
            "touchelente" | "slowkey" => {
                use crate::stick::Mode;
                let mode = match arg.map(str::to_lowercase).as_deref() {
                    Some("auto") => Some(Mode::Auto),
                    Some(w) => chat::switch(w).map(|on| if on { Mode::On } else { Mode::Off }),
                    None => None,
                };
                match mode {
                    Some(m) => {
                        self.stick.set_mode(m);
                        self.stick.remember();
                        self.chat.say(
                            match m {
                                Mode::On => "Lâcher courir arrête d'avancer tout de suite.",
                                Mode::Off => "Lâcher courir n'arrête plus d'avancer.",
                                Mode::Auto => {
                                    "Le jeu décide seul, selon le retard de ta touche avancer."
                                }
                            },
                            chat::YELLOW,
                        );
                    }
                    None => self
                        .chat
                        .say("Utilisation : /touchelente <on|off|auto>", chat::RED),
                }
            }
            // (v172) Partie contre des bots (mode FPS en solo) : retirer, refaire. (v197) Arrêt :
            // off, false, 0, non ou stop ; lancement : rien, on, true, 1 ou oui. Les niveaux
            // se donnent par leur nom (0 et 1 veulent dire arrêt et lancement). (v209) `/bot`
            // ne recommence plus la partie : il la complète (`/restart` pour la refaire) ;
            // `/bot maxbot <2-20>` : joueurs de la partie, soi compris.
            // (v243) Sur un serveur de construction : la partie FPS du serveur (OP).
            "bots" | "bot" | "start" | "restart" if self.build.is_some() => {
                let a = arg.map(str::to_lowercase);
                let count = parts.next();
                self.build_match_command(name.as_str(), a.as_deref(), count);
            }
            "bots" | "bot" => match arg.map(str::to_lowercase).as_deref() {
                Some(w) if w == "stop" || chat::switch(w) == Some(false) => {
                    self.bots_auto = false;
                    if self.net.as_ref().is_some_and(|n| n.is_solo()) {
                        self.stop_bots();
                        self.chat
                            .say("Bots retirés : tu es seul sur la carte.", chat::YELLOW);
                    }
                }
                // (v174) Niveau des bots, gardé pour les parties suivantes ; (v209) ceux de
                // la partie en cours le prennent tout de suite.
                Some(l) if crate::bots::level_named(l).is_some() => {
                    let level = crate::bots::level_named(l).unwrap_or(0);
                    crate::bots::save_level(level);
                    self.chat.say(
                        format!("Bots : niveau {}.", crate::bots::LEVELS[level as usize]),
                        chat::YELLOW,
                    );
                    if let Some(n) = self.net.as_mut().filter(|n| n.is_solo()) {
                        n.set_bot_level(level);
                    }
                }
                Some("maxbot" | "maxbots" | "max" | "nombre" | "joueurs") => {
                    let range = crate::bots::MIN_PLAYERS..=crate::bots::MAX_PLAYERS;
                    match parts.next().and_then(|n| n.parse::<usize>().ok()) {
                        Some(n) if range.contains(&n) => {
                            crate::bots::save_players(n);
                            let bots = n - 1;
                            let now = self
                                .net
                                .as_mut()
                                .filter(|n| n.is_solo())
                                .map(|g| g.set_bot_count(bots));
                            let t = match now {
                                Some(b) => format!(
                                    "Partie à {n} joueurs, toi compris : {b} bot{} maintenant.",
                                    if b > 1 { "s" } else { "" }
                                ),
                                None => {
                                    format!("Parties contre les bots : {n} joueurs, toi compris.")
                                }
                            };
                            self.chat.say(t, chat::YELLOW);
                        }
                        _ => self.chat.say(
                            format!(
                                "Utilisation : /bot maxbot <{}-{}> (joueurs de la partie, toi \
                                 compris ; les bots complètent)",
                                range.start(),
                                range.end()
                            ),
                            chat::RED,
                        ),
                    }
                }
                Some(w) if chat::switch(w) != Some(true) => self.chat.say(
                    "Utilisation : /bot [on|off|facile|normal|difficile|maxbot <2-20>]",
                    chat::RED,
                ),
                _ => {
                    if self.game.mode != GameMode::Fps {
                        self.chat
                            .say("Les bots jouent en mode FPS : /gm fps.", chat::RED);
                        return;
                    }
                    self.fill_bots();
                }
            },
            // (v209) Partie contre les bots : la lancer (ou la faire commencer tout de suite),
            // la recommencer du début.
            "start" => {
                if self.game.mode != GameMode::Fps {
                    self.chat
                        .say("La partie se joue en mode FPS : /gm fps.", chat::RED);
                    return;
                }
                match self.net.as_mut().filter(|n| n.is_solo()) {
                    Some(n) => {
                        let t = if n.start_now() {
                            "La partie commence."
                        } else {
                            "La partie est déjà commencée (/restart : la recommencer)."
                        };
                        self.chat.say(t, chat::YELLOW);
                    }
                    None => self.fill_bots(),
                }
            }
            "restart" => {
                if self.game.mode != GameMode::Fps {
                    self.chat
                        .say("La partie se joue en mode FPS : /gm fps.", chat::RED);
                    return;
                }
                // Nouvelle partie (commencée à l'image suivante).
                self.stop_bots();
                self.bots_auto = true;
                self.chat.say("La partie recommence.", chat::YELLOW);
            }
            "gamemode" | "gm" => match arg.and_then(GameMode::parse) {
                Some(mode) => self.set_mode(mode),
                None => self.chat.say(
                    "Utilisation : /gm <0|1|2|3|survival|creative|fps|spectator>",
                    chat::RED,
                ),
            },
            "give" => {
                if !self.game.mode.has_inventory() {
                    self.chat
                        .say("Pas d'inventaire dans ce mode de jeu.", chat::RED);
                    return;
                }
                let reg = minelib_world::registry::registry();
                let want = arg.unwrap_or("").to_lowercase();
                let mut next = parts.next();
                // Tête d'un joueur du jeu : /give tête <pseudo> [nombre] (son skin, tenu à
                // jour, posée comme dans Minecraft).
                if matches!(
                    want.as_str(),
                    "skull" | "tete" | "tête" | "head" | "player_head"
                ) {
                    if let Some(who) = next.filter(|w| w.parse::<u32>().is_err()) {
                        next = parts.next();
                        let count: u32 =
                            next.and_then(|n| n.parse().ok()).unwrap_or(64).clamp(1, 64);
                        if !minelib_net::proto::valid_pseudo(who) {
                            self.chat.say(format!("Pseudo invalide : {who}"), chat::RED);
                            return;
                        }
                        let skin = crate::heads::HeadSkin::Player(who.to_string());
                        self.inventory
                            .add(Item::Head(crate::heads::intern(&skin)), count);
                        self.chat
                            .say(format!("{count} tête(s) de {who} donnée(s)."), chat::YELLOW);
                        return;
                    }
                }
                let count: u32 = next.and_then(|n| n.parse().ok()).unwrap_or(64).clamp(1, 64);
                let found = reg
                    .by_key(&want)
                    .or_else(|| {
                        // Sinon par le nom affiché (« miroir », « bloc de fer »…).
                        (1..reg.len())
                            .map(|i| BlockId(i as u16))
                            .find(|b| b.def().name.to_lowercase() == want)
                    })
                    // (v281) Pas les dalles et escaliers retirés (TNT, eau, lave, minerais…).
                    // (v283) Ni les piles de tapis (elles se font en empilant des tapis).
                    .filter(|b| {
                        minelib_world::registry::retired_shape_base(*b).is_none()
                            && !b.def().key.starts_with("carpet_stack:")
                    });
                match found {
                    _ if screen_size(&want).is_some() => {
                        let size = screen_size(&want).unwrap_or(crate::screen::NORMAL);
                        self.inventory.add(Item::Screen(size), count);
                        let name = crate::screen::size_name(size).to_lowercase();
                        self.chat
                            .say(format!("{count} × {name} donné(s)."), chat::YELLOW);
                    }
                    _ if want == "torch" || want == "torche" => {
                        self.inventory.add(Item::Torch(None), count);
                        self.chat
                            .say(format!("{count} torche(s) données."), chat::YELLOW);
                    }
                    // (v216) Foudre de Zeus (« /give foudre », « /give zeus »).
                    _ if matches!(
                        want.as_str(),
                        "foudre" | "foudre_de_zeus" | "zeus" | "eclair" | "éclair" | "lightning"
                    ) =>
                    {
                        // (v244) Sur un serveur : demandé au relais du monde, qui le donne.
                        if self.legend_on() && self.build.is_some() {
                            if self.zeus_mine_online() {
                                self.chat.say(
                                    "Tu as déjà la Foudre de Zeus de ce serveur.",
                                    chat::GREY,
                                );
                            } else if !self.zeus_online.free() {
                                self.chat
                                    .say(crate::inventory::LEGEND_GONE_ONLINE, chat::RED);
                            } else if !self.zeus_online.claiming {
                                self.zeus_online.claiming = true;
                                self.zeus_cmd(minelib_net::proto::ZEUS_CLAIM, 0);
                            }
                        // (v241) Objet légendaire : un seul par carte, une seule fois.
                        } else if self.legend_on() && self.legend.taken {
                            self.chat.say(crate::inventory::LEGEND_GONE, chat::RED);
                        } else if self.inventory.add(Item::Zeus, 1) > 0 {
                            self.chat.say("Pas de place dans l'inventaire.", chat::RED);
                        } else {
                            self.legend.taken |= self.legend_on();
                            self.chat.say("Foudre de Zeus donnée.", chat::YELLOW);
                        }
                    }
                    // (v281) Seaux : vide, d'eau, de lave.
                    _ if bucket_named(&want).is_some() => {
                        let content = bucket_named(&want).flatten();
                        let item = Item::Bucket(content);
                        self.inventory.add(item, count);
                        self.chat.say(
                            format!("{count} × {} donné(s).", item.name().to_lowercase()),
                            chat::YELLOW,
                        );
                    }
                    // (v269) Œuf de L'Évidé.
                    _ if matches!(want.as_str(), "oeuf" | "oeuf_evide" | "evide" | "egg") => {
                        let n: u32 = next.and_then(|n| n.parse().ok()).unwrap_or(1).clamp(1, 64);
                        self.inventory.add(Item::Egg, n);
                        self.chat
                            .say(format!("{n} œuf(s) de L'Évidé donné(s)."), chat::YELLOW);
                    }
                    // (v201) Lampe torche (« /give lampe torche » aussi) : une seule, sauf
                    // nombre donné.
                    _ if matches!(
                        want.as_str(),
                        "lampe" | "lampe_torche" | "lampetorche" | "flashlight"
                    ) =>
                    {
                        let n: u32 = next.and_then(|n| n.parse().ok()).unwrap_or(1).clamp(1, 64);
                        self.inventory.add(Item::Flashlight, n);
                        self.chat
                            .say(format!("{n} lampe(s) torche(s) donnée(s)."), chat::YELLOW);
                    }
                    Some(b) if b != BlockId::AIR => {
                        self.inventory.add(Item::Block(b), count);
                        self.chat.say(
                            format!("{count} × {} donné(s).", b.def().name),
                            chat::YELLOW,
                        );
                    }
                    _ => self.chat.say(
                        format!("Bloc inconnu : {want} (ex. /give mirror, /give iron_block)"),
                        chat::RED,
                    ),
                }
            }
            // (v216) Météo : /weather clear|rain|storm, /weather fix [on|off].
            "weather" | "meteo" | "météo" => {
                let next = parts.next();
                self.weather_command(arg, next);
            }
            // (v216) La foudre tombe là où l'on vise.
            "thor" | "foudre" => self.thor_command(),
            // Heure du jour (sur un serveur de construction : OP, pour tout le monde).
            "timeset" | "heure" => {
                let default = matches!(
                    arg.map(str::to_lowercase).as_deref(),
                    Some("default" | "defaut" | "défaut")
                );
                // Carte FPS : l'heure par défaut est le soleil de Dust2 (`/fps set time`).
                if default && self.game.mode == GameMode::Fps {
                    self.fps_command(&["set", "time", "default"]);
                    return;
                }
                // Construction : le soleil de Dust2, immobile (heure « -1 »).
                if default {
                    if self.build.is_some() {
                        self.relay_cmds.push(minelib_net::client::Cmd::ServerAdmin {
                            action: minelib_net::proto::SERVER_TIMESET,
                            pseudo: String::new(),
                            reason: "-1".into(),
                        });
                    } else {
                        self.time_h = crate::daytime::DUST2;
                        self.time_fixed = true;
                        self.chat.say("Soleil de Dust2, immobile.", chat::YELLOW);
                    }
                    return;
                }
                let Some(h) = arg.and_then(crate::daytime::parse) else {
                    self.chat.say(
                        format!(
                            "Utilisation : /timeset <heure | default> (11, 11:20, 11.20, 11h20) ; il est {}.",
                            crate::daytime::format(self.time_h)
                        ),
                        chat::RED,
                    );
                    return;
                };
                if self.build.is_some() {
                    self.relay_cmds.push(minelib_net::client::Cmd::ServerAdmin {
                        action: minelib_net::proto::SERVER_TIMESET,
                        pseudo: String::new(),
                        reason: h.to_string(),
                    });
                } else {
                    self.time_h = h;
                    self.chat.say(
                        format!("Il est {}.", crate::daytime::format(h)),
                        chat::YELLOW,
                    );
                }
            }
            // Arrête ou relance le temps qui passe.
            "timefix" => {
                let Some(fixed) = arg.and_then(chat::switch) else {
                    self.chat.say(
                        format!(
                            "Utilisation : /timefix on|off (actuellement : {}).",
                            on_off(self.time_fixed)
                        ),
                        chat::RED,
                    );
                    return;
                };
                if self.build.is_some() {
                    self.relay_cmds.push(minelib_net::client::Cmd::ServerAdmin {
                        action: minelib_net::proto::SERVER_TIMEFIX,
                        pseudo: String::new(),
                        reason: (fixed as u8).to_string(),
                    });
                } else {
                    self.time_fixed = fixed;
                    self.chat.say(
                        if fixed {
                            "Heure arrêtée."
                        } else {
                            "Le temps passe de nouveau."
                        },
                        chat::YELLOW,
                    );
                }
            }
            // Renomme le monde (liste des mondes) ; sur un serveur de construction (OP),
            // aussi son nom dans la liste des serveurs.
            "worldname" | "nommonde" => {
                let new_name: String = cmd
                    .split_once(char::is_whitespace)
                    .map_or("", |(_, r)| r)
                    .trim()
                    .chars()
                    .take(32)
                    .collect();
                if new_name.is_empty() {
                    self.chat.say(
                        format!(
                            "Utilisation : /worldname <nom> (actuel : {})",
                            self.meta.name
                        ),
                        chat::RED,
                    );
                    return;
                }
                if self.build.is_some() {
                    self.relay_cmds.push(minelib_net::client::Cmd::ServerAdmin {
                        action: minelib_net::proto::SERVER_RENAME,
                        pseudo: String::new(),
                        reason: new_name.clone(),
                    });
                }
                let guest = self.build.as_ref().is_some_and(|b| !b.is_owner());
                if !self.ranked_match() && !guest {
                    self.meta.name = new_name.clone();
                    match self.save() {
                        Ok(()) => self
                            .chat
                            .say(format!("Monde renommé : {new_name}."), chat::YELLOW),
                        Err(e) => self.chat.say(format!("Enregistrement : {e:#}"), chat::RED),
                    }
                }
            }
            "heal" => {
                self.game.health = MAX_HEALTH;
                self.chat.say("Vous êtes soigné.", chat::YELLOW);
            }
            // (v223) Téléportation et point d'apparition (voir `tp_command`, `setspawn`).
            "tp" | "teleport" => {
                let args: Vec<&str> = cmd.split_whitespace().skip(1).collect();
                self.tp_command(&args);
            }
            "setspawn" | "spawnpoint" => self.setspawn(),
            // Éditeur de lumières (touche L) ; `/lumieres effacer` les retire toutes.
            "lumieres" | "lumières" | "lights" => match arg.map(|a| a.to_lowercase()) {
                // (v224) En ligne, pour tout le monde : seulement un OP (on le demande au
                // relais, comme pour ouvrir l'éditeur).
                Some(a) if a == "effacer" || a == "clear" => {
                    if self.build.is_none() || self.light_op_ok {
                        self.clear_lights();
                    } else if self
                        .light_op_probe
                        .is_none_or(|t| t.elapsed().as_secs_f32() >= 5.0)
                    {
                        self.light_op_probe = Some(std::time::Instant::now());
                        self.light_clear_asked = true;
                        self.relay_cmds.push(minelib_net::client::Cmd::ServerAdmin {
                            action: minelib_net::proto::SERVER_LIST,
                            pseudo: String::new(),
                            reason: String::new(),
                        });
                    }
                }
                _ => self.open_light_editor(),
            },
            "help" | "aide" | "?" => {
                for l in [
                    "/gm (ou /gamemode) <0|1|2|3> : survie, créatif, fps, spectateur",
                    "/fly [on|off] : autorise ou interdit le vol en survie",
                    "/heal : rend tous les coeurs",
                    "/tp <x y z | pseudo> ; /tp <pseudo> <x y z | pseudo> : téléporter (~ : par rapport à toi ; en ligne, OP)",
                    "/setspawn : point d'apparition là où tu es (en ligne, OP : pour qui arrive)",
                    "/worldname <nom> : renomme le monde",
                    "/seed : la graine du monde ; /seed lock true|false : la verrouiller ou non (en ligne, OP ; verrouillée par défaut : ni /seed ni « Télécharger la carte » pour qui n'est pas OP)",
                    "/timeset <heure | default> : règle l'heure (11, 11:20, 11.20, 11h20)",
                    "/timefix on|off : arrête ou relance le temps",
                    "/weather clear|rain|storm : le temps qu'il fait ; /weather fix [on|off] : le fige (en ligne, OP : pour tout le serveur)",
                    "/thor : la foudre tombe là où tu vises (en ligne, OP : tous la voient) ; /give foudre : la Foudre de Zeus",
                    "/fusion [on|off] : fusionne les faces de blocs voisines (moins de triangles, plus d'images par seconde)",
                    "/give <bloc> [nombre] : ex. /give mirror, /give iron_block, /give ecran",
                    "/give petit_ecran | grand_ecran | ecran_geant : les autres tailles d'écran",
                    "/give tête <pseudo> [nombre] : la tête d'un joueur, avec son skin",
                    "/give lampe : une lampe torche (clic droit : l'allumer ou l'éteindre)",
                    "/yt [recherche | adresse] : YouTube sur les écrans vidéo",
                    "/lumieres : éditeur de lumières (touche L) ; /lumieres effacer : les retire",
                    "/npc reflets on|off : les joueurs reflétés dans l'eau (essai, allumé)",
                    "Vol : deux appuis rapides sur Espace",
                    "Mode FPS : clic molette = ping (pas d'inventaire ni de blocs)",
                    "Spectateur (/gm 3) : vol forcé, on traverse les blocs",
                    "Oui ou non dans une commande : on/off, true/false, 1/0 ou oui/non, au choix",
                ] {
                    self.chat.say(l, chat::GREY);
                }
            }
            _ => self.chat.say(
                format!("Commande inconnue : /{name}. Tapez /help."),
                chat::RED,
            ),
        }
    }

    /// Avance l'horloge du monde ; les torches dont le sel est épuisé repassent en flamme normale.
    fn tick_torches(&mut self, dt: f32) {
        let t0 = self.world.clock;
        self.world.clock += dt as f64;
        for p in self.world.torches_expiring(t0, self.world.clock) {
            self.dirty.extend(World::chunks_in_light_radius(p[0], p[2]));
        }
    }

    /// Barre d'objets / inventaire (et réticule du pack).
    /// Partie en ligne : pseudos au-dessus des joueurs visibles, scores (Tab maintenu).
    fn draw_net(&self, ui: &mut Ui, size: (f32, f32), input: &Input, s: f32) {
        use minelib_net::game::{bomb, phase, CT};
        let Some(n) = &self.net else { return };
        let hud = n.hud();
        let my_team = hud.team;
        let eye = self.player.eye();
        let proj = Mat4::perspective_rh(
            self.fov.to_radians(),
            size.0 / size.1.max(1.0),
            0.05,
            1000.0,
        );
        let vp = proj * self.view_with_punch();
        let to_screen = |p: Vec3| -> Option<(f32, f32)> {
            let c = vp * p.extend(1.0);
            (c.w > 0.0).then(|| {
                (
                    (c.x / c.w * 0.5 + 0.5) * size.0,
                    (0.5 - c.y / c.w * 0.5) * size.1,
                )
            })
        };
        // Coéquipiers : un triangle à leur couleur au-dessus d'eux, même à travers les murs,
        // et leur pseudo quand on les voit. Rien au-dessus des adversaires.
        let views = n.views();
        let rows = n.rows();
        let mut mates: Vec<&crate::netgame::View> = views
            .iter()
            .filter(|v| v.alive && v.team == my_team)
            .collect();
        mates.sort_by(|a, b| a.pseudo.cmp(&b.pseudo));
        for (k, v) in mates.iter().enumerate() {
            // Couleur donnée par l'hôte (préférences), sinon dans l'ordre.
            let k = rows
                .iter()
                .find(|r| r.pseudo == v.pseudo)
                .map_or(k, |r| r.color as usize);
            let head = v.pos + Vec3::Y * 2.05;
            let to = head - Vec3::Y * 0.4 - eye;
            let dist = to.length();
            let Some((x, y)) = to_screen(head) else {
                continue;
            };
            let col = mate_color(k);
            // Triangle pointe en bas.
            let t = 4.0 * s;
            for row in 0..(t as i32) {
                let half = (t - row as f32) * 0.9;
                ui.rect(x - half, y + row as f32, half * 2.0, 1.0, col);
            }
            // (v251) À travers une vitre ou du verre, on le voit (son pseudo aussi).
            let seen =
                crate::fps::raycast_view(&self.world, eye.into(), (to / dist.max(1e-3)).into(), dist)
                    .is_none_or(|h| h.distance >= dist - 0.3);
            if !seen || dist > 60.0 {
                continue;
            }
            // (v209) Plus de « [bombe] » après le pseudo de celui qui la porte (la minimap
            // la montre).
            let name = v.pseudo.clone();
            let tw = Ui::text_width(&name, s);
            let ny = y - 11.0 * s;
            ui.rect(
                x - tw / 2.0 - s,
                ny - s,
                tw + 2.0 * s,
                10.0 * s,
                [0.0, 0.0, 0.0, NAME_BACK],
            );
            ui.text(x - tw / 2.0, ny, s, col, name);
        }
        // Bombe lâchée (connue de l'équipe) : repère à l'écran. (v209) Posée : plus rien
        // d'écrit au-dessus (la minimap et le compte à rebours la montrent).
        if hud.bomb == bomb::DROPPED {
            if let Some((x, y)) = to_screen(Vec3::from(hud.bomb_pos) + Vec3::Y * 0.4) {
                let t = "bombe lâchée";
                let tw = Ui::text_width(t, s);
                ui.text(x - tw / 2.0, y, s, [1.0, 0.3, 0.25, 1.0], t);
            }
        }
        // Haut de l'écran : score, temps, joueurs en vie.
        let (cx, top) = (size.0 / 2.0, 4.0 * s);
        let time = hud.time.ceil() as u32;
        let clock = format!("{}:{:02}", time / 60, time % 60);
        let (mid, mid_col) = match hud.phase {
            _ if hud.pause != 0 => {
                let t = hud.pause_left.ceil() as u32;
                let kind = if hud.pause == minelib_net::game::PAUSE_TECHNICAL {
                    "Pause technique"
                } else {
                    "Pause tactique"
                };
                (
                    format!("{kind} {}:{:02}", t / 60, t % 60),
                    [1.0, 0.75, 0.3, 1.0],
                )
            }
            phase::WARMUP => {
                if hud.time > 0.0 {
                    (format!("Début dans {time} s"), [1.0, 0.9, 0.35, 1.0])
                } else {
                    (
                        format!("Échauffement {} / 10", hud.players),
                        [0.85, 0.85, 0.85, 1.0],
                    )
                }
            }
            phase::PLANTED => ("BOMBE".to_string(), [1.0, 0.3, 0.25, 1.0]),
            phase::HALFTIME => ("Mi-temps".to_string(), [1.0, 0.9, 0.35, 1.0]),
            phase::GAME_OVER => ("Fin".to_string(), [1.0, 0.9, 0.35, 1.0]),
            phase::FREEZE => (clock, [0.6, 0.85, 1.0, 1.0]),
            _ => (clock, [1.0, 1.0, 1.0, 1.0]),
        };
        let mw = Ui::text_width(&mid, s * 1.5);
        let w = (150.0 * s).max(mw + 60.0 * s);
        ui.rect(cx - w / 2.0, top, w, 22.0 * s, [0.0, 0.0, 0.0, 0.5]);
        // Bandeau des têtes : CT à gauche, T à droite ; ses coéquipiers à leur couleur, les
        // morts assombris.
        // Plus petites au-delà de 5 par équipe (cartes jusqu'à 10 contre 10).
        let most = [CT, minelib_net::game::T]
            .map(|t| rows.iter().filter(|r| r.team == t).count())
            .into_iter()
            .max()
            .unwrap_or(0);
        let head = if most > 5 { 10.0 * s } else { 14.0 * s };
        for (side, sign) in [(CT, -1.0f32), (minelib_net::game::T, 1.0)] {
            let mut team: Vec<&minelib_net::game::ScoreRow> =
                rows.iter().filter(|r| r.team == side).collect();
            team.sort_by_key(|r| (r.color, r.id));
            for (k, r) in team.iter().enumerate() {
                let off = w / 2.0 + 4.0 * s + k as f32 * (head + 2.0 * s);
                let x = if sign < 0.0 {
                    cx - off - head
                } else {
                    cx + off
                };
                // (v209) Chacun sa couleur, adversaires compris (avant : celle de leur
                // équipe pour tous).
                let col = mate_color(r.color as usize);
                // (v209) Mort : cadre, couleur et visage passés au gris (avant : seulement
                // assombris, la couleur restait).
                let (dim, gray) = if r.alive { (1.0, 0.0) } else { (0.45, 0.85) };
                let grayed = |c: [f32; 4]| {
                    let l = 0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2];
                    let m = |v: f32| (v + (l - v) * gray) * dim;
                    [m(c[0]), m(c[1]), m(c[2]), c[3]]
                };
                let c = grayed([col[0], col[1], col[2], 0.95]);
                ui.rect(x, top, head, head + 4.0 * s, [0.0, 0.0, 0.0, 0.55]);
                ui.rect(x + s, top + s, head - 2.0 * s, head - 2.0 * s, c);
                // Tête : le visage du skin (pas pendant l'échauffement, anonyme), sinon
                // l'initiale du pseudo.
                let skin = if r.id == n.me() {
                    self.skins.mine()
                } else if r.pseudo.starts_with("Joueur ") {
                    None
                } else {
                    // (v177) Les bots ont leur skin (leur visage, ici).
                    self.skins.in_match(&r.pseudo, r.team)
                };
                if let Some(sk) = skin {
                    let inner = head - 4.0 * s;
                    crate::skin::draw_face_gray(
                        ui,
                        &sk.face(),
                        x + 2.0 * s,
                        top + 2.0 * s,
                        inner,
                        dim,
                        gray,
                    );
                } else {
                    let ch: String = r.pseudo.chars().take(1).collect::<String>().to_uppercase();
                    let cw = Ui::text_width(&ch, s);
                    ui.text(
                        x + (head - cw) / 2.0,
                        top + 3.0 * s,
                        s,
                        [0.0, 0.0, 0.0, 0.8 * dim + 0.2],
                        ch,
                    );
                }
                // (v177) Bande du bas : la couleur du joueur (bots compris). (v209) Celle de
                // chaque adversaire aussi, grisée s'il est mort.
                let band = grayed(col);
                ui.rect(x + s, top + head, head - 2.0 * s, 3.0 * s, band);
                // (v209) Mort : une croix rouge, aussi fine que l'ancienne barre.
                if !r.alive {
                    let (x0, y0) = (x + 2.0 * s, top + 2.0 * s);
                    let (x1, y1) = (x + head - 2.0 * s, top + head - 2.0 * s);
                    let red = [0.9, 0.2, 0.2, 0.9];
                    ui.line(x0, y0, x1, y1, s, red);
                    ui.line(x1, y0, x0, y1, s, red);
                }
            }
        }
        ui.text(cx - mw / 2.0, top + 3.0 * s, s * 1.5, mid_col, mid.clone());
        if hud.phase != phase::WARMUP {
            let l = format!("{}", hud.score[0]);
            let r = format!("{}", hud.score[1]);
            ui.text(
                cx - w / 2.0 + 6.0 * s,
                top + 3.0 * s,
                s * 1.5,
                team_color(CT),
                l,
            );
            let rw = Ui::text_width(&r, s * 1.5);
            ui.text(
                cx + w / 2.0 - 6.0 * s - rw,
                top + 3.0 * s,
                s * 1.5,
                team_color(minelib_net::game::T),
                r,
            );
            let alive = format!(
                "{} en vie  manche {}  {} en vie",
                hud.alive[0], hud.round, hud.alive[1]
            );
            let aw = Ui::text_width(&alive, s);
            ui.text(
                cx - aw / 2.0,
                top + 15.0 * s,
                s,
                [0.8, 0.8, 0.8, 1.0],
                alive,
            );
        }
        // Équipe, bombe, achats.
        let mut line = format!(
            "Équipe : {}",
            if my_team == CT {
                "antiterroristes (CT)"
            } else {
                "terroristes (T)"
            }
        );
        if hud.carrying {
            line += "   Tu as la bombe : 5 pour la prendre, tir ou E maintenu sur un site, G pour la lâcher";
        }
        if hud.can_buy && hud.phase != phase::WARMUP {
            line += "   Achats ouverts (B)";
        }
        ui.text(
            cx - Ui::text_width(&line, s) / 2.0,
            top + 26.0 * s,
            s,
            team_color(my_team),
            line,
        );
        // Vote de son équipe : à gauche, comme dans CS (F1 oui, F2 non).
        if let Some(v) = hud.vote {
            let name = |id: u64| {
                rows.iter()
                    .find(|r| r.id == id)
                    .map_or_else(|| "?".to_string(), |r| r.pseudo.clone())
            };
            let title = if v.kind == minelib_net::game::VOTE_KICK {
                format!("Exclure {} ?", name(v.target))
            } else if v.kind == minelib_net::game::VOTE_SURRENDER {
                "Abandonner la partie ?".to_string()
            } else {
                "Pause tactique ?".to_string()
            };
            let by = format!("Proposé par {}", name(v.caller));
            let count = format!(
                "Oui {}   Non {}   ({} oui nécessaires)  {} s",
                v.yes,
                v.no,
                v.need,
                v.left.ceil() as u32
            );
            let keys = if v.can_vote {
                format!(
                    "{} : oui   {} : non",
                    self.keys
                        .get(Action::FpsVoteYes)
                        .map_or_else(|| "-".into(), |b| b.name(input)),
                    self.keys
                        .get(Action::FpsVoteNo)
                        .map_or_else(|| "-".into(), |b| b.name(input))
                )
            } else {
                "Vote enregistré".to_string()
            };
            let lines = [title, by, count, keys];
            let w = lines
                .iter()
                .map(|l| Ui::text_width(l, s))
                .fold(0.0, f32::max)
                + 10.0 * s;
            let (x, y) = (6.0 * s, size.1 * 0.35);
            ui.rect(x, y, w, 50.0 * s, [0.0, 0.0, 0.0, 0.6]);
            for (i, l) in lines.into_iter().enumerate() {
                let col = if i == 0 {
                    [1.0, 0.85, 0.35, 1.0]
                } else {
                    [0.9, 0.9, 0.9, 1.0]
                };
                ui.text(x + 5.0 * s, y + 4.0 * s + i as f32 * 11.0 * s, s, col, l);
            }
        }
        // Mort : qui on suit.
        if let Some(name) = &self.spectating {
            // (v209) Partie solo : on peut prendre la place du bot regardé.
            let take = if n.is_bot(name) {
                ", E : prendre sa place"
            } else {
                ""
            };
            let t = format!("Tu regardes {name} (clic : suivant, clic droit : précédent{take})");
            ui.text(
                cx - Ui::text_width(&t, s) / 2.0,
                size.1 * 0.8,
                s,
                [1.0, 1.0, 1.0, 1.0],
                t,
            );
        }
        // Arme au sol à portée.
        if self.spectating.is_none() && !self.net_dead {
            let feet = self.player.pos;
            if let Some(g) = n
                .ground()
                .into_iter()
                .find(|g| (Vec3::from(g.pos) - feet).length() < 1.6)
            {
                let name = match crate::cs::nade_code(g.weapon) {
                    Some(n) => crate::equip::Nade::ALL[n].name(),
                    None => crate::weapons::WEAPONS
                        .get(g.weapon as usize)
                        .map_or("?", |w| w.name),
                };
                let t = format!("E : ramasser {name}");
                ui.text(
                    cx - Ui::text_width(&t, s) / 2.0,
                    size.1 * 0.66,
                    s,
                    [1.0, 1.0, 1.0, 1.0],
                    t,
                );
            }
        }
        // Barre de pose / désamorçage.
        if hud.progress > 0.0 {
            let (bw, bh) = (120.0 * s, 6.0 * s);
            let (bx, by) = (cx - bw / 2.0, size.1 * 0.62);
            let label = if my_team == CT {
                if hud.kit {
                    "Désamorçage (kit)"
                } else {
                    "Désamorçage"
                }
            } else {
                "Pose de la bombe"
            };
            ui.text(
                cx - Ui::text_width(label, s) / 2.0,
                by - 10.0 * s,
                s,
                [1.0, 1.0, 1.0, 1.0],
                label,
            );
            ui.rect(bx, by, bw, bh, [0.0, 0.0, 0.0, 0.6]);
            ui.rect(bx, by, bw * hud.progress, bh, [0.95, 0.75, 0.2, 0.95]);
        } else if my_team == CT
            && hud.bomb == bomb::PLANTED
            && (Vec3::from(hud.bomb_pos) - self.player.pos).length() < 1.8
        {
            let t = "Maintiens E pour désamorcer";
            ui.text(
                cx - Ui::text_width(t, s) / 2.0,
                size.1 * 0.62,
                s,
                [1.0, 1.0, 1.0, 1.0],
                t,
            );
        }
        // Minimap en haut à droite (règles de CS : coéquipiers toujours, adversaires
        // repérés seulement, bombe connue de son équipe).
        let radar_size = (72.0 * s).min(size.1 * 0.3);
        let radar_x = size.0 - radar_size - 6.0 * s;
        {
            use minelib_net::game::FLAG_BOMB;
            // (v209) Celui qui porte la bombe : un logo de bombe à sa couleur au lieu du
            // point (les adversaires : en rouge, s'ils sont repérés).
            let mates: Vec<(Vec3, [f32; 4], bool)> = views
                .iter()
                .filter(|v| v.alive && v.team == my_team)
                .map(|v| {
                    let k = rows
                        .iter()
                        .find(|r| r.pseudo == v.pseudo)
                        .map_or(0, |r| r.color as usize);
                    (v.pos, mate_color(k), v.flags & FLAG_BOMB != 0)
                })
                .collect();
            // (v209) Adversaires : seulement ceux que l'équipe voit ou vient de voir (3,5 s,
            // à leur dernière position vue). Avant : tous, tout le temps.
            let enemies: Vec<(Vec3, bool)> = self
                .spotting
                .shown()
                .map(|(pseudo, at)| {
                    let bomb = views
                        .iter()
                        .any(|v| v.pseudo == pseudo && v.alive && v.flags & FLAG_BOMB != 0);
                    (at, bomb)
                })
                .collect();
            // (v247) « La bombe ne peut être vue que sous les mêmes conditions qu'un ennemi pour
            // les CT, et toujours pour les T. »
            let bomb_at = if my_team == CT {
                self.spotting.bomb_shown()
            } else {
                matches!(hud.bomb, bomb::PLANTED | bomb::DROPPED).then(|| Vec3::from(hud.bomb_pos))
            };
            // Son point à sa couleur dans l'équipe, comme les coéquipiers.
            let me_id = n.me();
            let my_color = rows
                .iter()
                .find(|r| r.id == me_id)
                .map_or([1.0; 4], |r| mate_color(r.color as usize));
            self.radar.draw(
                ui,
                radar_x,
                6.0 * s,
                radar_size,
                s,
                (self.player.pos, self.player.look_dir(), my_color),
                hud.carrying && !self.net_dead,
                &mates,
                &enemies,
                bomb_at,
            );
        }
        // Fil des éliminations, en haut à droite (sous la minimap), comme dans CS : tueur,
        // arme, marques (aveuglé, tir en tête, à travers un mur, à travers une fumée), mort.
        let mut fy = 6.0 * s + radar_size + 6.0 * s;
        for (k, age) in &self.feed {
            let alpha = (6.0 - age).clamp(0.0, 1.0);
            let fade = |c: [f32; 4]| [c[0], c[1], c[2], c[3] * alpha];
            let weapon = crate::netgame::weapon_name(k.weapon);
            use minelib_net::game::{KILL_BLIND, KILL_SMOKE, KILL_WALL};
            let icon = 9.0 * s;
            let mut marks: Vec<&[&str; 7]> = Vec::new();
            if k.flags & KILL_WALL != 0 {
                marks.push(&ICON_WALL);
            }
            if k.flags & KILL_SMOKE != 0 {
                marks.push(&ICON_SMOKE);
            }
            if k.head {
                marks.push(&ICON_HEAD);
            }
            let blind = k.flags & KILL_BLIND != 0;
            let gap = 4.0 * s;
            let (kw, ww, vw) = (
                Ui::text_width(&k.killer, s),
                Ui::text_width(weapon, s),
                Ui::text_width(&k.victim, s),
            );
            let total = kw
                + if k.killer.is_empty() { 0.0 } else { gap }
                + if blind { icon + gap } else { 0.0 }
                + ww
                + gap
                + marks.len() as f32 * (icon + gap)
                + vw;
            let mut x = size.0 - 6.0 * s - total;
            ui.rect(
                x - 3.0 * s,
                fy - 2.0 * s,
                total + 6.0 * s,
                12.0 * s,
                fade([0.0, 0.0, 0.0, 0.5]),
            );
            if !k.killer.is_empty() {
                ui.text(x, fy, s, fade(team_color(k.teams[0])), k.killer.clone());
                x += kw + gap;
            }
            if blind {
                draw_icon(ui, x, fy, s, &ICON_BLIND, fade([1.0, 1.0, 1.0, 1.0]));
                x += icon + gap;
            }
            ui.text(x, fy, s, fade([0.9, 0.9, 0.9, 1.0]), weapon);
            x += ww + gap;
            for m in marks {
                let col = if std::ptr::eq(m, &ICON_HEAD) {
                    [1.0, 0.35, 0.3, 1.0]
                } else {
                    [1.0, 1.0, 1.0, 1.0]
                };
                draw_icon(ui, x, fy, s, m, fade(col));
                x += icon + gap;
            }
            ui.text(x, fy, s, fade(team_color(k.teams[1])), k.victim.clone());
            fy += 14.0 * s;
        }
        // Bandeau de fin de manche.
        if let Some((text, col, _)) = &self.banner {
            let tw = Ui::text_width(text, s * 2.0);
            ui.rect(
                cx - tw / 2.0 - 8.0 * s,
                size.1 * 0.22 - 4.0 * s,
                tw + 16.0 * s,
                24.0 * s,
                [0.0, 0.0, 0.0, 0.55],
            );
            ui.text(cx - tw / 2.0, size.1 * 0.22, s * 2.0, *col, text.clone());
        }
        // Tableau des scores : Tab maintenu, ou fin de partie.
        let over = self.game_over.as_ref();
        if over.is_some() || (!self.chat.open && input.held(KeyCode::Tab)) {
            self.draw_scoreboard(ui, size, s, n, over.map(|o| &o.0));
        }
    }

    /// Pseudo d'un joueur qui parle, et sa distance (construction seulement).
    fn speaker(&self, id: u64) -> (String, Option<f32>) {
        if let Some(b) = &self.build {
            return b
                .others
                .values()
                .find(|o| crate::voice::key_of(&o.pseudo) == id)
                .map_or((String::new(), Some(VOICE_RANGE)), |o| {
                    (o.pseudo.clone(), Some((o.pos - self.player.pos).length()))
                });
        }
        let name = self
            .net
            .as_ref()
            .and_then(|n| n.rows().into_iter().find(|r| r.id == id))
            .map_or_else(String::new, |r| r.pseudo);
        (name, None)
    }

    /// Construction (v137) : voix reçue (par le relais du monde, ou par un relais de région
    /// : `relayed`), jouée au volume choisi pour ce joueur, plus bas avec la distance.
    fn voice_from(&mut self, from: &str, data: &[u8], relayed: bool) {
        // Regarder ensemble : ses messages passent par le canal de la voix.
        if data.starts_with(&crate::watch::MAGIC) {
            let pos = self.build.as_ref().and_then(|b| {
                b.others
                    .values()
                    .find(|o| o.pseudo.eq_ignore_ascii_case(from))
                    .map(|o| o.pos)
            });
            self.party.receive(from, data, pos, &self.screens);
            return;
        }
        let key = crate::voice::key_of(from);
        let dist = self.build.as_ref().and_then(|b| {
            b.others
                .values()
                .find(|o| o.pseudo.eq_ignore_ascii_case(from))
                .map(|o| (o.pos - self.player.pos).length())
        });
        let mut gain = self.volume.max(0.05) * 1.6 * self.voice_volume;
        gain *= self
            .voice_volumes
            .get(&from.to_lowercase())
            .copied()
            .unwrap_or(1.0);
        gain *= (1.0 - dist.unwrap_or(VOICE_RANGE) / VOICE_RANGE).clamp(0.0, 1.0);
        if let Some(v) = &mut self.voice {
            v.receive(key, Some((from, relayed)), data, gain);
        }
    }

    /// Construction (v137) : ce que la voix a à envoyer part par pseudo (par le relais de
    /// région de là où l'on est, sinon par le relais du monde) ; ce qu'ont apporté les
    /// relais de région est joué. Clés de signature et de vérification des bonjours tenues
    /// à jour.
    fn route_voice(&mut self) {
        let Some(b) = &mut self.build else { return };
        let incoming = b.take_voice();
        if let (Some(v), Some((key, cert, room, root))) = (&mut self.voice, b.voice_keys()) {
            v.set_verify(root, room);
            v.set_signer(crate::voice::Signer { key, cert, room });
        }
        if let Some(v) = &mut self.voice {
            let out = v.take_out();
            if !out.is_empty() {
                let names: HashMap<u64, String> = b
                    .others
                    .values()
                    .map(|o| (crate::voice::key_of(&o.pseudo), o.pseudo.clone()))
                    .collect();
                for (keys, data) in out {
                    let to: Vec<String> =
                        keys.iter().filter_map(|k| names.get(k).cloned()).collect();
                    b.send_voice(to, data);
                }
            }
        }
        for (from, data) in incoming {
            self.voice_from(&from, &data, true);
        }
    }

    /// Joueurs de la partie (pour le vocal) : tous, et ceux qui doivent m'entendre.
    fn voice_peers(&self) -> (Vec<u64>, Vec<u64>) {
        if let Some(n) = &self.net {
            let me = n.me();
            let rows = n.rows();
            let team = n.hud().team;
            let all: Vec<u64> = rows.iter().filter(|r| r.id != me).map(|r| r.id).collect();
            let mates = rows
                .iter()
                .filter(|r| r.id != me && r.team == team)
                .map(|r| r.id)
                .collect();
            return (all, mates);
        }
        if let Some(b) = &self.build {
            // Dans une foule : les 256 joueurs les plus proches entendent (le relais n'en
            // sert pas plus par message), et les clés ne s'échangent qu'avec ceux qui
            // sont à portée ou près de l'être.
            let me = self.player.pos;
            let mut by_dist: Vec<(f32, u64)> = b
                .others
                .values()
                .map(|o| ((o.pos - me).length(), crate::voice::key_of(&o.pseudo)))
                .filter(|(d, _)| *d < VOICE_RANGE * 1.5)
                .collect();
            by_dist.sort_by(|x, y| x.0.total_cmp(&y.0));
            by_dist.truncate(VOICE_LISTENERS);
            let all: Vec<u64> = by_dist.iter().map(|x| x.1).collect();
            let near = by_dist
                .iter()
                .filter(|x| x.0 < VOICE_RANGE)
                .map(|x| x.1)
                .collect();
            return (all, near);
        }
        (Vec::new(), Vec::new())
    }

    /// (v215) Touche Parler du mode : celle de l'onglet FPS des touches en FPS, celle de
    /// l'onglet Build ailleurs (réglées à part, K toutes deux par défaut).
    fn talk_action(&self) -> Action {
        if self.game.mode == GameMode::Fps {
            Action::FpsVoice
        } else {
            Action::Talk
        }
    }

    /// Micro (touche maintenue), volumes au curseur (Tab + clic droit).
    fn update_voice(&mut self, input: &Input, dt: f32, typing: bool) {
        if self.voice.is_none() {
            return;
        }
        let ptt = !typing && self.keys.held(input, self.talk_action());
        let (all, listen) = self.voice_peers();
        let build = self.build.is_some();
        if let Some(v) = &mut self.voice {
            // Construction : la voix part par pseudo (voir `route_voice`).
            if build {
                v.route_by_pseudo();
            }
            v.update(dt, &all, &listen, ptt);
        }
        self.route_voice();
        // Tab maintenu : clic droit libère la souris ; Tab lâché : on rejoue.
        let tab = !self.chat.open && input.held(KeyCode::Tab);
        if tab && input.mouse_pressed(minelib_platform::Mouse::Right) {
            self.cursor_free = true;
        }
        if !tab {
            self.cursor_free = false;
        }
        if self.cursor_free && input.mouse_held(minelib_platform::Mouse::Left) {
            if let Some((cx, cy)) = input.cursor {
                let size = self.screen_size;
                for (name, x, y, w, h) in self.volume_sliders(size) {
                    if cx >= x - 4.0 && cx <= x + w + 4.0 && cy >= y && cy <= y + h {
                        let v = ((cx - x) / w).clamp(0.0, 1.0) * 2.0;
                        self.voice_volumes.insert(name.to_lowercase(), v);
                        self.voice_dirty = true;
                    }
                }
            }
        }
    }

    /// Curseurs de volume : (pseudo, x, y, largeur, hauteur), à droite du tableau des
    /// scores (FPS) ou dans la liste des joueurs proches (construction).
    fn volume_sliders(&self, size: (f32, f32)) -> Vec<(String, f32, f32, f32, f32)> {
        let s = (size.1 / 360.0).floor().max(1.0);
        let mut out = Vec::new();
        if let Some(n) = &self.net {
            let me = n.me();
            for (row, y) in scoreboard_lines(size, &n.rows()) {
                if row.id != me {
                    let (x, _, w) = scoreboard_frame(size);
                    out.push((row.pseudo.clone(), x + w + 6.0 * s, y, 60.0 * s, 8.0 * s));
                }
            }
        } else if let Some(b) = &self.build {
            for (i, (pseudo, _, _)) in self.build_list(b).into_iter().enumerate() {
                // (Sous sa propre ligne, la première.)
                let (x, y) = (
                    size.0 / 2.0 + 40.0 * s,
                    size.1 * 0.18 + 12.0 * s * (i as f32 + 2.5),
                );
                out.push((pseudo, x, y, 60.0 * s, 8.0 * s));
            }
        }
        out
    }

    /// Joueurs proches en construction, du plus près au plus loin : pseudo, distance, ping.
    fn build_list(&self, b: &crate::buildnet::BuildNet) -> Vec<(String, f32, Option<u16>)> {
        let mut v: Vec<(String, f32, Option<u16>)> = b
            .others
            .values()
            .map(|o| (o.pseudo.clone(), (o.pos - self.player.pos).length(), o.ping))
            .collect();
        v.sort_by(|a, b| a.1.total_cmp(&b.1));
        v.truncate(20);
        v
    }

    /// (v215) Haut-parleur en bas à gauche tant qu'on tient la touche Parler : blanc quand
    /// la voix part, rouge sans micro utilisable, gris hors ligne (personne ne l'entend).
    /// En FPS, au-dessus de la vie.
    fn draw_talk_icon(&self, ui: &mut Ui, size: (f32, f32), input: &Input, s: f32) {
        let held = !self.chat.open
            && !self.keyboard_on_screen()
            && self.keys.held(input, self.talk_action());
        let (color, note) = match &self.voice {
            Some(v) if v.talking => ([1.0, 1.0, 1.0, 1.0], None),
            Some(_) if held => ([1.0, 0.35, 0.3, 1.0], Some("Aucun micro")),
            None if held => ([0.6, 0.6, 0.6, 1.0], Some("Hors ligne")),
            _ => return,
        };
        let x = 8.0 * s;
        let y = if self.game.mode == GameMode::Fps {
            size.1 - 42.0 * s
        } else {
            size.1 - 20.0 * s
        };
        let w = note.map_or(0.0, |t| Ui::text_width(t, s) + 4.0 * s);
        ui.rect(
            x - 2.0 * s,
            y - 2.0 * s,
            16.0 * s + w,
            16.0 * s,
            [0.0, 0.0, 0.0, 0.45],
        );
        // Corps, pavillon qui s'évase, deux ondes (en pixels de 1 × s).
        for (px, py, pw, ph) in [
            (0.0, 4.0, 3.0, 4.0),
            (3.0, 3.0, 1.0, 6.0),
            (4.0, 2.0, 1.0, 8.0),
            (5.0, 1.0, 1.0, 10.0),
            (6.0, 0.0, 1.0, 12.0),
            (8.0, 3.0, 1.0, 1.0),
            (9.0, 4.0, 1.0, 4.0),
            (8.0, 8.0, 1.0, 1.0),
            (10.0, 1.0, 1.0, 1.0),
            (11.0, 2.0, 1.0, 8.0),
            (10.0, 10.0, 1.0, 1.0),
        ] {
            ui.rect(x + px * s, y + py * s, pw * s, ph * s, color);
        }
        if let Some(t) = note {
            ui.text(x + 15.0 * s, y + 2.0 * s, s, color, t);
        }
    }

    /// Curseurs de volume et qui parle.
    fn draw_voice(&self, ui: &mut Ui, size: (f32, f32), input: &Input, s: f32) {
        self.draw_talk_icon(ui, size, input, s);
        let Some(v) = &self.voice else { return };
        // Qui parle, en haut à gauche (soi : le haut-parleur en bas à gauche).
        let mut y = 40.0 * s;
        for id in v.speaking.keys() {
            let (name, _) = self.speaker(*id);
            ui.text(8.0 * s, y, s, [0.6, 1.0, 0.6, 1.0], format!("> {name}"));
            y += 11.0 * s;
        }
        let tab = !self.chat.open && input.held(KeyCode::Tab);
        if !tab {
            return;
        }
        // Construction : liste des joueurs proches.
        if let Some(b) = &self.build {
            let list = self.build_list(b);
            let (x, y0) = (size.0 / 2.0 - 120.0 * s, size.1 * 0.18);
            let h = 12.0 * s * (list.len() as f32 + 3.0);
            ui.rect(x, y0, 240.0 * s, h, [0.0, 0.0, 0.0, 0.7]);
            ui.text(
                x + 6.0 * s,
                y0 + 4.0 * s,
                s,
                [1.0, 0.9, 0.35, 1.0],
                "Joueurs proches",
            );
            // (v165) Ping de chacun à gauche de son pseudo ; (v172) soi en premier, avec
            // son ping comme les autres.
            let ping_w = Ui::text_width("9999 ms", s) + 6.0 * s;
            let me = if self.pseudo.is_empty() {
                "Toi".to_string()
            } else {
                self.pseudo.clone()
            };
            let mine = (b.ping() > 0).then(|| b.ping());
            let rows = std::iter::once((me, None, mine, true)).chain(
                list.iter()
                    .map(|(name, d, ping)| (name.clone(), Some(*d), *ping, false)),
            );
            for (i, (name, d, ping, own)) in rows.enumerate() {
                let ry = y0 + 12.0 * s * (i as f32 + 1.5);
                let (text, color) = ping_label(ping);
                ui.text(x + 6.0 * s, ry, s, color, text);
                let line = match d {
                    Some(d) => format!("{name}  {d:.0} m"),
                    None => name,
                };
                let c = if own {
                    [1.0, 0.95, 0.6, 1.0]
                } else {
                    [1.0, 1.0, 1.0, 1.0]
                };
                ui.text(x + 6.0 * s + ping_w, ry, s, c, line);
            }
        }
        for (name, x, y, w, h) in self.volume_sliders(size) {
            let vol = self
                .voice_volumes
                .get(&name.to_lowercase())
                .copied()
                .unwrap_or(1.0);
            ui.rect(x, y + h * 0.35, w, h * 0.3, [0.25, 0.25, 0.3, 0.9]);
            ui.rect(
                x,
                y + h * 0.35,
                w * vol / 2.0,
                h * 0.3,
                [0.5, 0.8, 0.5, 0.95],
            );
            ui.rect(x + w * vol / 2.0 - s, y, 2.0 * s, h, [1.0, 1.0, 1.0, 1.0]);
            ui.text(
                x + w + 3.0 * s,
                y,
                s,
                [0.85, 0.85, 0.85, 1.0],
                format!("{:.0} %", vol * 100.0),
            );
        }
        if !self.cursor_free {
            let t = "Clic droit : régler le volume de chaque joueur";
            ui.text(
                (size.0 - Ui::text_width(t, s)) / 2.0,
                size.1 - 60.0 * s,
                s,
                [0.85, 0.85, 0.85, 1.0],
                t,
            );
        }
    }

    /// (v255) `/fps showelements` : zones de la carte relues toutes les secondes, et portes
    /// cherchées dans quelques chunks chargés par image (les plus proches d'abord, puis tous de
    /// nouveau, en boucle) ; celles des chunks déchargés oubliées.
    fn update_fps_elements(&mut self, dt: f32) {
        let Some(e) = self.fps_elements.as_mut() else {
            return;
        };
        e.map_age += dt;
        if e.map_age >= 1.0 {
            e.map_age = 0.0;
            e.map = fps_map_of(&self.dir, &self.world);
        }
        if e.queue.is_empty() {
            let me = ChunkPos::containing(
                self.player.pos.x.floor() as i32,
                self.player.pos.z.floor() as i32,
            );
            let mut loaded: Vec<ChunkPos> = self.world.loaded().collect();
            let keep: HashSet<ChunkPos> = loaded.iter().copied().collect();
            e.doors.retain(|c, _| keep.contains(c));
            // Les plus proches à la fin : `pop` les prend d'abord.
            loaded.sort_by_key(|c| std::cmp::Reverse((c.x - me.x).pow(2) + (c.z - me.z).pow(2)));
            e.queue = loaded;
        }
        // (v258) Les portes trouvées ne se montrent que si toutes s'ouvrent (`/fps set door
        // on`) ; sinon, seulement celles posées pour la partie (dans la carte).
        if !e.map.doors() {
            e.doors.clear();
            e.queue.clear();
            return;
        }
        for _ in 0..ELEMENT_CHUNKS_PER_FRAME {
            let Some(c) = e.queue.pop() else { break };
            let found = chunk_doors(&self.world, c);
            if found.is_empty() {
                e.doors.remove(&c);
            } else {
                e.doors.insert(c, found);
            }
        }
    }

    /// (v255) `/fps showelements` : le nom de chaque zone de la carte (« Site A », « Départ CT »…)
    /// au-dessus de sa boîte, de sa couleur, à travers les murs.
    fn draw_element_names(&self, ui: &mut Ui, size: (f32, f32), s: f32) {
        let Some(e) = &self.fps_elements else { return };
        if self.game.mode == GameMode::Fps {
            return;
        }
        let proj = Mat4::perspective_rh(
            self.fov.to_radians(),
            size.0 / size.1.max(1.0),
            0.05,
            1000.0,
        );
        let vp = proj * self.view_with_punch();
        for (zone, name, color) in element_zones(&e.map) {
            let top = Vec3::new(
                (zone.min[0] + zone.max[0] + 1) as f32 * 0.5,
                zone.max[1] as f32 + 1.6,
                (zone.min[2] + zone.max[2] + 1) as f32 * 0.5,
            );
            let c = vp * top.extend(1.0);
            if c.w <= 0.0 {
                continue;
            }
            let (x, y) = (
                (c.x / c.w * 0.5 + 0.5) * size.0,
                (0.5 - c.y / c.w * 0.5) * size.1,
            );
            let w = Ui::text_width(&name, s);
            ui.rect(x - w * 0.5 - s, y - s, w + 2.0 * s, 9.0 * s, [0.0, 0.0, 0.0, 0.45]);
            let shown = [
                color[0].min(1.0),
                color[1].min(1.0),
                color[2].min(1.0),
                1.0,
            ];
            ui.text(x - w * 0.5, y, s, shown, name);
        }
    }

    /// Construction en ligne : pseudos des joueurs proches. (v243) Pas ceux de la partie FPS du
    /// serveur quand on y est (on ne voit pas ses adversaires à travers les murs).
    fn draw_build_names(&self, ui: &mut Ui, size: (f32, f32), s: f32) {
        let mut near: Vec<(&str, Vec3)> = Vec::new();
        let in_match: std::collections::HashSet<String> = match &self.net {
            Some(n) if self.in_build_match() => {
                n.views().iter().map(|v| v.pseudo.to_lowercase()).collect()
            }
            _ => Default::default(),
        };
        if let Some(b) = &self.build {
            near.extend(
                b.others
                    .values()
                    .filter(|o| !in_match.contains(&o.pseudo.to_lowercase()))
                    .map(|o| (o.pseudo.as_str(), o.pos)),
            );
        }
        if let Some(c) = &self.crowd {
            near.extend(c.npcs.iter().map(|n| (n.name.as_str(), n.pos)));
        }
        // (v227) Et ceux des foules des autres OP.
        for rc in self.remote_crowds.values() {
            near.extend(rc.npcs.iter().map(|n| (n.name.as_str(), n.pos)));
        }
        if near.is_empty() {
            return;
        }
        let proj = Mat4::perspective_rh(
            self.fov.to_radians(),
            size.0 / size.1.max(1.0),
            0.05,
            1000.0,
        );
        let vp = proj * self.view_with_punch();
        let cam = self.camera_pos();
        // (v189) Les plus loin d'abord : les pseudos proches passent devant.
        let mut labels: Vec<(f32, &str, Vec3)> = near
            .into_iter()
            .map(|(pseudo, pos)| {
                let top = pos + Vec3::Y * 2.1;
                ((top - cam).length(), pseudo, top)
            })
            .filter(|&(d, _, _)| d < NAME_RANGE)
            .collect();
        labels.sort_by(|a, b| b.0.total_cmp(&a.0));
        for (d, pseudo, top) in labels {
            let c = vp * top.extend(1.0);
            if c.w <= 0.0 {
                continue;
            }
            let Some((ts, alpha)) = name_label(d, s) else {
                continue;
            };
            let (x, y) = (
                (c.x / c.w * 0.5 + 0.5) * size.0,
                (0.5 - c.y / c.w * 0.5) * size.1,
            );
            let tw = Ui::text_width(pseudo, ts);
            // Hors de l'écran : rien à dessiner.
            if x + tw / 2.0 + ts < 0.0
                || x - tw / 2.0 - ts > size.0
                || y + 9.0 * ts < 0.0
                || y - ts > size.1
            {
                continue;
            }
            // (v217) Fond presque opaque : deux pseudos qui se chevauchent restent lisibles
            // (le plus proche cache celui de derrière ; avant, on lisait les deux mêlés).
            ui.rect(
                x - tw / 2.0 - ts,
                y - ts,
                tw + 2.0 * ts,
                10.0 * ts,
                [0.0, 0.0, 0.0, NAME_BACK * alpha],
            );
            ui.text(
                x - tw / 2.0,
                y,
                ts,
                [1.0, 1.0, 1.0, alpha],
                pseudo.to_string(),
            );
        }
    }

    /// Tableau des scores façon CS : une partie par équipe, argent des coéquipiers,
    /// éliminations, passes, morts, étoiles d'homme du match, % de tirs à la tête, dégâts
    /// moyens par manche, score ; manches gagnées en haut. En fin de partie : victoire ou
    /// défaite.
    fn draw_scoreboard(
        &self,
        ui: &mut Ui,
        size: (f32, f32),
        s: f32,
        n: &crate::netgame::NetGame,
        over: Option<&minelib_net::game::MatchSummary>,
    ) {
        use minelib_net::game::{CT, T};
        let rows = n.rows();
        let hud = n.hud();
        let me = n.me();
        let rounds = n.round_history();
        let played = rounds.len().max(1) as f32;
        let row = 11.0 * s;
        let (x, mut y, w) = scoreboard_frame(size);
        let header = 30.0 * s;
        let count = |t: u8| rows.iter().filter(|r| r.team == t).count().max(1) as f32;
        let h = header + (count(CT) + count(T) + 2.0) * row + 16.0 * s;
        ui.rect(x, y, w, h, [0.02, 0.02, 0.03, 0.82]);
        // Titre : résultat ou score.
        let title = match over {
            Some(sum) => {
                let mine = rows.iter().find(|r| r.id == me).map(|r| r.pseudo.clone());
                let start = mine
                    .and_then(|p| sum.players.iter().find(|q| q.pseudo == p))
                    .map_or(0, |q| q.start_team as usize);
                let (a, b) = (sum.score[start], sum.score[1 - start]);
                let verdict = if a > b {
                    "VICTOIRE"
                } else if a < b {
                    "DÉFAITE"
                } else {
                    "ÉGALITÉ"
                };
                format!("{verdict}  {a} - {b}   (Échap : quitter la partie)")
            }
            None if hud.round == 0 => "Échauffement".to_string(),
            None => format!(
                "CT {}  -  {} T      manche {}",
                hud.score[0], hud.score[1], hud.round
            ),
        };
        ui.text(
            x + 6.0 * s,
            y + 4.0 * s,
            s * 1.5,
            [1.0, 0.9, 0.35, 1.0],
            title,
        );
        // Manches : un carré par manche, couleur du côté qui l'a gagnée.
        let sq = 5.0 * s;
        for (i, (start_winner, _)) in rounds.iter().enumerate() {
            let side = side_of_start(*start_winner, i as u32);
            let rx = x + 6.0 * s + i as f32 * (sq + s);
            if rx + sq > x + w {
                break;
            }
            ui.rect(rx, y + 18.0 * s, sq, sq, team_color(side));
        }
        y += header;
        let ping_w = Ui::text_width("9999 ms", s) + 6.0 * s;
        let cols: [(&str, f32); 8] = [
            ("$", 0.36),
            ("K", 0.47),
            ("A", 0.53),
            ("D", 0.59),
            ("MVP", 0.65),
            ("HS%", 0.73),
            ("ADR", 0.82),
            ("Score", 0.91),
        ];
        for team in [CT, T] {
            let col = team_color(team);
            ui.text(
                x + 6.0 * s,
                y,
                s,
                col,
                if team == CT {
                    "Antiterroristes"
                } else {
                    "Terroristes"
                },
            );
            for (name, fx) in cols {
                ui.text(x + w * fx, y, s, [0.7, 0.7, 0.75, 1.0], name);
            }
            y += row;
            for r in rows.iter().filter(|r| r.team == team) {
                let bright = if r.alive || over.is_some() { 1.0 } else { 0.5 };
                let c = [col[0] * bright, col[1] * bright, col[2] * bright, 1.0];
                if r.id == me {
                    ui.rect(x + 2.0 * s, y - s, w - 4.0 * s, row, [1.0, 1.0, 1.0, 0.08]);
                }
                // (v172) Ping à gauche du pseudo (l'hôte de la partie : 0) ; les bots de la
                // partie solo : « BOT » à sa place.
                let (ping, pc) = if r.pseudo.starts_with("BOT ") {
                    ("BOT".to_string(), [0.6, 0.6, 0.65, 1.0])
                } else {
                    ping_label((r.ping > 0).then_some(r.ping))
                };
                ui.text(x + 6.0 * s, y, s, pc, ping);
                ui.text(x + 6.0 * s + ping_w, y, s, c, r.pseudo.clone());
                let hs = if r.kills > 0 {
                    r.headshots as f32 * 100.0 / r.kills as f32
                } else {
                    0.0
                };
                let vals = [
                    if team == hud.team || over.is_some() {
                        format!("{}", r.money)
                    } else {
                        String::new()
                    },
                    format!("{}", r.kills),
                    format!("{}", r.assists),
                    format!("{}", r.deaths),
                    if r.mvps > 0 {
                        format!("*{}", r.mvps)
                    } else {
                        String::new()
                    },
                    format!("{hs:.0}"),
                    format!("{:.0}", r.damage as f32 / played),
                    format!("{}", r.score),
                ];
                for ((_, fx), v) in cols.iter().zip(vals) {
                    ui.text(x + w * fx, y, s, [0.95, 0.95, 0.95, bright], v);
                }
                y += row;
            }
            y += 4.0 * s;
        }
    }

    /// (v220) Interface masquée (F1, hors mode FPS) : ni barre d'objets, ni viseur, ni
    /// tchat, ni écran F3, ni main ou objet tenu à la première personne, ni contour du bloc
    /// visé ; il reste le jeu et les pseudos des joueurs et des PNJ.
    pub fn hud_off(&self) -> bool {
        self.hud_hidden && self.game.mode != GameMode::Fps
    }

    pub fn draw_hud(&self, ui: &mut Ui, size: (f32, f32), input: &Input, debug: bool) {
        let s = (size.1 / 360.0).floor().max(1.0);
        if self.hud_off() {
            self.draw_build_names(ui, size, s);
            return;
        }
        self.draw_net(ui, size, input, s);
        self.draw_build_names(ui, size, s);
        self.draw_element_names(ui, size, s);
        self.draw_voice(ui, size, input, s);
        if self.hurt > 0.0 {
            ui.rect(0.0, 0.0, size.0, size.1, [0.7, 0.0, 0.0, self.hurt * 0.6]);
        }
        if !self.inventory.open {
            let c = 15.0 * s;
            // Mode FPS : viseur façon CS dessiné par le HUD FPS. Éditeur de lumières : la
            // souris est libre, pas de viseur.
            if self.game.mode != GameMode::Fps && !self.light_editor.open {
                ui.image(
                    "gui/crosshair",
                    (size.0 - c) / 2.0,
                    (size.1 - c) / 2.0,
                    c,
                    c,
                    [1.0, 1.0, 1.0, 0.9],
                );
            }
            // (v249) Ébloui par la foudre : l'écran blanc (un peu bleuté), puis la vue revient.
            if self.dazzle > 0.0 {
                let a = dazzle_alpha(self.dazzle);
                ui.rect(0.0, 0.0, size.0, size.1, [0.93, 0.96, 1.0, a]);
            }
            // Mode FPS : vie sur 100, munitions, solde, menu d'achat (pas sous l'éditeur de
            // lumières, en FPS solo : son panneau prend la place).
            if self.game.mode == GameMode::Fps {
                self.fps.screen_fx(ui, size, self.player.eye());
                if !self.light_editor.open {
                    // (v209) Spectateur : l'écran du joueur regardé, comme si c'était soi.
                    if self.spectating.is_some() {
                        self.spec_fps.hud(ui, size, None, debug);
                    } else {
                        self.fps.hud(ui, size, input.cursor, debug);
                    }
                }
            }
            // Cœurs (survie) : au-dessus de la barre d'objets, à gauche, images du pack.
            if self.game.mode == GameMode::Survival {
                let slot = 20.0 * s;
                let x0 = (size.0 - slot * inventory::HOTBAR as f32) / 2.0;
                let y = size.1 - slot - 4.0 * s - 10.0 * s;
                let h = self.game.health;
                for i in 0..(MAX_HEALTH / 2) {
                    let sprite = match h.saturating_sub(i * 2) {
                        0 => "gui/icons/health_empty",
                        1 => "gui/icons/health_half",
                        _ => "gui/icons/health_full",
                    };
                    // Peu de vie : les cœurs tremblent légèrement.
                    let dy = if h <= 4 {
                        ((i as f32 * 2.3 + self.world.clock as f32 * 25.0).sin() * s).round()
                    } else {
                        0.0
                    };
                    ui.image(
                        sprite,
                        x0 + i as f32 * 8.0 * s,
                        y + dy,
                        9.0 * s,
                        9.0 * s,
                        [1.0; 4],
                    );
                }
            }
        }
        if self.game.mode.has_inventory() {
            self.inventory.draw(ui, size, input.cursor);
        }
        self.draw_pings(ui, size);
        // Shaders des lumières en cours de compilation (au lancement du jeu) : les
        // lumières arrivent dans quelques secondes. L'éditeur le dit lui-même.
        if self.lights_wanted
            && self.lights_state == minelib_render::DynState::Compiling
            && !self.light_editor.open
        {
            let t = minelib_render::tr("Préparation des lumières…");
            let tw = Ui::text_width(&t, s);
            let (x, y) = ((size.0 - tw) / 2.0, 6.0 * s);
            ui.rect(
                x - 3.0 * s,
                y - 2.0 * s,
                tw + 6.0 * s,
                12.0 * s,
                [0.0, 0.0, 0.0, 0.45],
            );
            ui.text(x, y, s, [1.0, 0.85, 0.35, 1.0], t);
        }
        self.chat.draw(ui, size);
    }

    /// Pings : losange jaune et distance, sur le bord de l'écran s'ils sont hors champ.
    fn draw_pings(&self, ui: &mut Ui, size: (f32, f32)) {
        if self.pings.is_empty() {
            return;
        }
        let s = (size.1 / 360.0).floor().max(1.0);
        let proj = glam::Mat4::perspective_infinite_reverse_rh(
            self.fov.to_radians(),
            size.0 / size.1,
            0.05,
        );
        let vp = proj * self.player.view();
        let eye = self.player.eye();
        for (p, age) in &self.pings {
            let a = ((PING_S - age) / 1.0).clamp(0.0, 1.0);
            let clip = vp * p.extend(1.0);
            let (mut nx, mut ny) = if clip.w.abs() > 1e-4 {
                (clip.x / clip.w, clip.y / clip.w)
            } else {
                (0.0, 0.0)
            };
            if clip.w < 0.0 {
                // Derrière le joueur : on l'envoie sur le bord opposé.
                nx = -nx * 100.0;
                ny = -ny * 100.0;
            }
            let m = 0.92;
            let k = (nx.abs() / m).max(ny.abs() / m).max(1.0);
            let (nx, ny) = (nx / k, ny / k);
            let x = (nx * 0.5 + 0.5) * size.0;
            let y = (0.5 - ny * 0.5) * size.1;
            let gold = [1.0, 0.8, 0.2, a];
            let dark = [0.0, 0.0, 0.0, 0.6 * a];
            // Losange en escalier (5 lignes de pixels agrandis).
            for (row, half) in [(0.0, 1.0), (1.0, 2.0), (2.0, 3.0), (3.0, 2.0), (4.0, 1.0)] {
                let w = half * 2.0 * s;
                ui.rect(
                    x - w / 2.0 - s,
                    y + (row - 2.5) * 2.0 * s - s,
                    w + 2.0 * s,
                    2.0 * s + 2.0 * s,
                    dark,
                );
                ui.rect(x - w / 2.0, y + (row - 2.5) * 2.0 * s, w, 2.0 * s, gold);
            }
            let d = (*p - eye).length();
            let label = format!("{d:.0} m");
            let tw = Ui::text_width(&label, s);
            ui.text(x - tw / 2.0, y + 7.0 * s, s, gold, label);
        }
    }

    /// Construit l'écran F3.
    pub fn build_debug(
        &mut self,
        frames: debug::FrameSummary,
        sys: minelib_platform::sysmon::SysStats,
        renderer: &Renderer,
        stats: FrameStats,
        fps_max: Option<f32>,
    ) {
        let hit = self.hit;
        self.overlay = debug::build(&debug::DebugInfo {
            frames,
            player: &self.player,
            origin: self.world.origin_blocks(),
            laps: self.laps(),
            hit,
            // (v328) Verrouillée par les OP du serveur : pas à l'écran F3 non plus.
            seed: self.seed_allowed().then(|| self.known_seed()).flatten(),
            seed_locked: !self.seed_allowed(),
            chunks_loaded: self.world.len(),
            stats,
            held: self
                .inventory
                .selected_item()
                .map_or("rien".into(), |i| i.name()),
            keys: self.move_debug.clone(),
            torch_info: hit.filter(|h| h.id.is_torch()).and_then(|h| {
                let t = self.world.torch(h.block)?;
                let flame = minelib_world::flame::CHEMICALS;
                Some(match (t.chemical, self.world.torch_remaining(&t)) {
                    (Some(i), Some(left)) if left.is_infinite() => {
                        format!("flamme {} : durée infinie", flame[i].color_name)
                    }
                    (Some(i), Some(left)) => format!(
                        "flamme {} : encore {}",
                        flame[i].color_name,
                        minelib_world::flame::format_duration(left)
                    ),
                    (Some(_), None) => "sel épuisé, flamme normale : durée infinie".into(),
                    _ => "flamme normale : durée infinie".into(),
                })
            }),
            fps: self.game.mode == GameMode::Fps,
            torches: self.world.torch_count(),
            sys,
            renderer,
            engine: debug::EngineInfo {
                view_distance: self.view_distance,
                stream: self.streamer.stats,
                workers: self.workers.count(),
                fps_max,
                fov: self.fov,
                world_name: self.meta.name.clone(),
            },
        });
    }

    /// (v264) « Un impact dans le joueur » : le coup qui touche en `hit` (monde), tiré de
    /// `from`, laisse une plaie sur le corps du joueur de la partie le plus proche de ce point
    /// (le joueur local compris), sur le pavé de son modèle qui s'y trouve. Douze au plus par
    /// joueur (les plus vieilles s'en vont).
    fn wound_at(&mut self, hit: Vec3, from: Vec3, knife: bool) {
        use crate::player_model::{Held, Pose};
        let Some(n) = &self.net else { return };
        let pose = |feet: Vec3, yaw: f32, pitch: f32, crouch: f32, weapon: u8| Pose {
            feet,
            yaw,
            pitch,
            walk: 0.0,
            swing: 0.0,
            crouch,
            light: [1.0; 3],
            held: Some(Held::Gear(weapon)),
            skin: None,
            bow: 0.0,
            air: 0.0,
            dir: 0.0,
            run: 0.0,
        };
        let near = |feet: Vec3| (feet + Vec3::Y * 0.9).distance_squared(hit);
        let mut best: Option<(f32, String, Pose)> = None;
        let me = &self.player;
        // Pas le tireur lui-même (le coup part de ses yeux).
        if self.game.mode == GameMode::Fps && me.eye().distance(from) > 0.8 {
            let p = pose(me.pos, me.yaw, me.pitch, me.crouch, self.fps.held_weapon());
            best = Some((near(me.pos), String::new(), p));
        }
        for v in n.views().iter().filter(|v| v.alive) {
            let d = near(v.pos);
            let shooter = (v.pos + Vec3::Y * 1.5).distance(from) < 0.8;
            if !shooter && best.as_ref().is_none_or(|b| d < b.0) {
                let p = pose(v.pos, v.yaw, v.pitch, v.crouch, v.weapon);
                best = Some((d, v.pseudo.clone(), p));
            }
        }
        let Some((d, who, pose)) = best else { return };
        if d > 2.0 * 2.0 {
            return;
        }
        let seed = (hit.x * 977.0 + hit.y * 613.0 + hit.z * 331.0).to_bits() ^ 0x5bd1_e995;
        if let Some(w) = crate::player_model::wound_at(&pose, hit, from, knife, seed) {
            let list = self.wounds.entry(who).or_default();
            if list.len() >= 12 {
                list.remove(0);
            }
            list.push(w);
        }
    }

    /// (v256) Skin que porte le joueur local : le sien ; en partie FPS, la tenue de son
    /// équipe, la même que celle que les autres lui voient (`Skins::worn`).
    fn worn_skin(&self) -> Option<std::sync::Arc<crate::skin::Skin>> {
        let team = match (&self.net, self.fps.online) {
            (Some(_), Some(o)) if self.game.mode == GameMode::Fps => o.team,
            _ => return self.skins.mine(),
        };
        if team != minelib_net::game::CT && team != minelib_net::game::T {
            return self.skins.mine();
        }
        // Son pseudo dans la partie (anonyme pendant l'échauffement, puis le sien) : relu
        // toutes les deux secondes, pas à chaque image.
        let mut kept = self.team_skin.borrow_mut();
        if kept
            .as_ref()
            .is_none_or(|(t, at, _)| *t != team || at.elapsed().as_secs_f32() > 2.0)
        {
            let n = self.net.as_ref()?;
            let me = n.me();
            let pseudo = n
                .rows()
                .into_iter()
                .find(|r| r.id == me)
                .map(|r| r.pseudo)
                .unwrap_or_default();
            let skin = self.skins.worn(&pseudo, team)?;
            *kept = Some((team, std::time::Instant::now(), skin));
        }
        kept.as_ref().map(|(_, _, s)| s.clone())
    }

    pub fn render(
        &self,
        renderer: &mut Renderer,
        ui: &Ui,
        show_debug: bool,
        time: f32,
        grabbed: bool,
    ) -> FrameStats {
        let fog_end = self.fog_end();
        let fps_mode = self.game.mode == GameMode::Fps;
        let fov = if fps_mode && self.spectating.is_some() {
            // (v209) Spectateur : la lunette du joueur regardé.
            self.spec_fps.scope_fov().unwrap_or(self.fov)
        } else if fps_mode {
            self.fps.scope_fov().unwrap_or(self.fov)
        } else {
            self.fov
        };
        // Vue F5 : caméra à 4 blocs derrière (ou devant) les yeux, comme dans Minecraft,
        // rapprochée si un bloc est sur le chemin. Pas en mode FPS. Caméra libre de
        // l'éditeur de lumières : le joueur reste où il est, et se voit.
        let free = self.light_editor.free_cam.is_some();
        let third = if fps_mode {
            0
        } else if free {
            1
        } else {
            self.perspective
        };
        let (cam, view, cam_look) = self.view_camera();
        use crate::player_model::Held;
        let held = self.model_held();
        let (walk, swing) = self.stride();
        let pose = crate::player_model::Pose {
            feet: self.player.pos,
            yaw: self.player.yaw + self.eat_turn.0,
            pitch: self.player.pitch + self.eat_turn.1,
            walk,
            swing,
            crouch: self.pose_crouch(),
            light: self.hand_light,
            held,
            skin: self.worn_skin(),
            bow: self.eat_bow,
            air: self.anim.0,
            dir: self.anim.1,
            run: self.anim.2,
        };
        // (v272) Où sont, dans cette pose, l'endroit que L'Évidé vise et nos morsures qui
        // saignent : lus à l'image suivante (`Prey::spot`, gouttes de sang).
        if self.evide_spot.is_some() || !self.bleeding.is_empty() {
            let mut seen = self.bite_points.borrow_mut();
            seen.0 = self
                .evide_spot
                .map(|(p, c)| crate::player_model::cell_point(&pose, p, c));
            seen.2 = self
                .evide_spot
                .map(|(p, _)| crate::player_model::part_point(&pose, p));
            seen.1 = self
                .bleeding
                .iter()
                .map(|b| (b.0, b.1, crate::player_model::cell_point(&pose, b.0, b.1)))
                .collect();
        }
        let visible = self.game.mode != GameMode::Spectator;
        // (v290) Ce qui dit son mouvement à l'anticrénelage (sa place à l'image d'avant,
        // sommet par sommet) : le corps du joueur local en F5, les Évidés. Mis à la fin des
        // entités de l'image (`entities_moved`).
        let mut moved: Vec<minelib_render::EntityVertex> = Vec::new();
        let mut moved_prev: Vec<[f32; 3]> = Vec::new();
        if third != 0 && visible {
            let body = |p: &crate::player_model::Pose| {
                // (v269) Avec ce que L'Évidé lui a mangé.
                let mut m = crate::player_model::build_bitten(p, self.flesh.get(""));
                // (v264) Ses plaies (partie FPS), sur son corps.
                if let Some(w) = self.wounds.get("") {
                    m.extend(crate::player_model::wound_model(p, w));
                }
                // (v305) Les genoux et les coudes pleins : vus d'en dessous (la caméra
                // descendue au sol), plus de fente.
                m.extend(crate::player_model::joint_sections(p, self.flesh.get("")));
                m
            };
            let m = body(&pose);
            // (v290) « De la rémanence sur l'intérieur d'un joueur » : son corps n'avait aucun
            // mouvement (« il suit la caméra »), alors qu'il balance bras et jambes, se penche,
            // s'accroupit, et que la caméra tourne autour de lui en F5 ; les cubes rouges de
            // l'intérieur d'un corps mordu traînaient. Le même corps dans la pose de l'image
            // d'avant donne la place d'avant de chaque sommet (même skin, même objet, mêmes
            // morsures : les mêmes sommets dans le même ordre ; sinon, immobile).
            let before = self
                .own_seen
                .replace(Some((crate::player_model::Prev::still(&pose), pose.bow)));
            let old = before
                .map(|(p, bow)| {
                    body(&crate::player_model::Pose {
                        feet: p.feet,
                        yaw: p.yaw,
                        pitch: p.pitch,
                        walk: p.walk,
                        swing: p.swing,
                        crouch: p.crouch,
                        light: pose.light,
                        held: pose.held,
                        skin: pose.skin.clone(),
                        bow,
                        air: p.air,
                        dir: p.dir,
                        run: p.run,
                    })
                })
                .filter(|o| o.len() == m.len());
            moved_prev.extend(
                m.iter()
                    .enumerate()
                    .map(|(i, v)| old.as_ref().map_or(v.pos, |o| o[i].pos)),
            );
            moved.extend(m);
            // (v305) « L'intérieur visible, tu le textures de la texture visible de sa jambe au
            // niveau de la caméra » : ce que l'écran coupe de son corps (la caméra descendue
            // au sol, dans ses jambes) est plein, à la couleur de son skin à cet endroit. La
            // coupe suit l'écran : à l'image d'avant, elle était à la même place devant la
            // caméra d'avant. (v323) Ce qu'il tient aussi (le bloc, à sa texture).
            let cut = crate::player_model::cut(&pose, self.flesh.get(""), cam, cam_look, CUT_AT);
            let back = self.cut_view.replace(Some(view)).map(|v| v.inverse() * view);
            moved_prev.extend(cut.iter().map(|v| {
                back.map_or(v.pos, |b| b.transform_point3(Vec3::from(v.pos)).to_array())
            }));
            moved.extend(cut);
        } else {
            self.own_seen.set(None);
            self.cut_view.set(None);
        }
        let mut model: Vec<minelib_render::EntityVertex> = Vec::new();
        // (Avant la v290, le corps du joueur local était en tête, sans mouvement : « il suit
        // la caméra ».) Le reste est fixé dans le monde.
        let follow = 0;
        // (v197) Tableaux de l'image repris à l'image d'avant (voir `Scratch`).
        let mut scratch = std::mem::take(&mut *self.scratch.borrow_mut());
        let mut shadows = std::mem::take(&mut scratch.shadows);
        shadows.clear();
        if visible && self.spectating.is_none() {
            // (v273) « Mon ombre au soleil n'est plus la bonne par rapport à mon joueur » : à la
            // première personne (hors mode FPS), son corps est dessiné reculé
            // (`first_person_feet`, v267) ; son ombre l'est aussi, sous les pieds qu'il voit.
            let mut own = if third == 0 && !free && !fps_mode {
                crate::player_model::shadow_boxes(
                    &pose.at(crate::player_model::first_person_feet(&pose)),
                )
            } else {
                crate::player_model::shadow_boxes(&pose)
            };
            // (v201) Sa lampe torche n'a pas l'ombre de son bras ni de sa tête. (v202) Sa
            // torche tenue, seulement pas celle du morceau tout contre la flamme (sa main).
            // (v237) La Foudre de Zeus : « l'ombre de mon bras qui tient l'éclair ne doit pas
            // pouvoir disparaître ». Sa lumière est sur l'éclair, une demi-longueur au-dessus
            // du poing : hors du bras. La marge de 0,2 bloc autour de la boîte droite du bras
            // (bien plus grande que lui quand il est penché) l'écartait selon la pose : son
            // ombre allait et venait en marchant. N'est écarté que le morceau où la lumière
            // se trouverait vraiment (tout serait alors dans l'ombre).
            let torch = self.held_torch_light().map(|l| Vec3::from(l.pos));
            let zeus = match torch {
                Some(_) => None,
                None => self.zeus_light().map(|l| Vec3::from(l.pos)),
            };
            for b in &mut own {
                b.owner = crate::flashlight::HOLDER;
                if torch.is_some_and(|f| near_box(b, f, 0.2))
                    || zeus.is_some_and(|f| inside_box(b, f))
                {
                    b.owner |= crate::flashlight::NEAR_FLAME;
                }
            }
            shadows.push((self.player.pos, own, None));
        }
        // (v217) Ombres des joueurs jusqu'à la distance réglée (Paramètres graphiques →
        // Lumières → Ombres) : boîtes exactes pour les plus proches (de la caméra ou d'une
        // lumière), carte d'ombre du soleil pour tous les autres (`sun_casters`).
        let shadow_d = self.lights.settings.player_shadow_distance;
        let shadow_d2 = shadow_d * shadow_d;
        let mut sun_casters = std::mem::take(&mut scratch.sun_casters);
        sun_casters.clear();
        // (v220) Et tous, jusqu'à cette distance, pour l'ombre aux lumières dynamiques (atlas
        // des joueurs).
        let mut light_casters = std::mem::take(&mut scratch.light_casters);
        light_casters.clear();
        // (v172) Les autres joueurs : dessinés par la carte graphique, tous d'un coup (un
        // modèle commun, une instance chacun), comme soi en F5 : leur skin pixel par pixel,
        // la lumière de là où ils sont (lumières dynamiques comprises). Seul l'objet tenu
        // reste fait ici.
        let mut layers = self.skin_layers.borrow_mut();
        layers.begin(renderer);
        let mut players = std::mem::take(&mut scratch.players);
        players.clear();
        // (v174) Mort en FPS : on regarde par les yeux d'un coéquipier ; son corps (et ce
        // qu'il tient, la bombe sur son dos) n'est pas dessiné autour de la caméra. Son
        // ombre reste, comme la sienne.
        let inside = if free {
            None
        } else {
            self.spectating.as_deref()
        };
        // (v215) Les autres joueurs (et ce qu'ils tiennent) sont dans le brouillard comme le
        // relief (`entity_fog`) : à `fog_end` de la caméra, tout entiers (le modèle tient
        // dans 2 blocs autour de ses pieds), on ne les voit plus. Plus loin, ils ne sont pas
        // dessinés : ni de face, ni dans un reflet, ni par leur ombre. Ce n'est pas le tri
        // du champ de vision (refusé en v192) : où que regarde la caméra, la même distance.
        let hidden2 = (fog_end + 2.0) * (fog_end + 2.0);
        // Partie en ligne : les autres joueurs.
        if let Some(n) = &self.net {
            for v in n
                .views()
                .iter()
                .filter(|v| v.alive && v.pos.distance_squared(cam) <= hidden2)
            {
                // (v177) Ce qu'il a en main, avec son vrai modèle : arme, couteau (255),
                // grenade ou bombe.
                let held = Some(Held::Gear(v.weapon));
                let w = self.walks.get(&v.pseudo).copied().unwrap_or_default();
                let other = crate::player_model::Pose {
                    feet: v.pos,
                    yaw: v.yaw,
                    pitch: v.pitch,
                    walk: w.phase,
                    swing: w.amp,
                    crouch: v.crouch,
                    light: sky_light(&self.world, eye_of(v.pos, v.crouch), self.daylight_shown),
                    held,
                    // (v177) Les bots aussi ont un skin, dessiné pour eux. (v256) Les
                    // joueurs portent la même tenue d'équipe qu'eux.
                    skin: self.skins.worn(&v.pseudo, v.team),
                    bow: 0.0,
                    air: w.air,
                    dir: w.dir,
                    run: w.run,
                };
                let layer = layers.layer(other.skin.as_ref(), renderer);
                let prev = Walk::prev_of(&self.walks, &v.pseudo, &other);
                let inst = crate::player_model::instance(&other, layer, &prev);
                // (v217) Jusqu'à la distance réglée ; au-delà des boîtes exactes, dans la
                // carte du soleil (à 10 contre 10, les derniers n'avaient pas d'ombre).
                // (v273) Dans l'atlas des joueurs (lumières dynamiques) seulement s'il n'a pas
                // ses boîtes exactes (voir plus bas, quand elles ne tiennent plus).
                if v.pos.distance_squared(cam) <= shadow_d2 {
                    shadows.push((v.pos, crate::player_model::shadow_boxes(&other), Some(inst)));
                }
                if inside == Some(v.pseudo.as_str()) {
                    continue;
                }
                players.push(inst);
                // (v177) Le modèle de son arme de près, sa silhouette de loin.
                let near = v.pos.distance_squared(cam) < GEAR_DETAIL * GEAR_DETAIL;
                model.extend(crate::player_model::held_model(&other, near));
                // (v264) Ses plaies, sur son corps (pas de trop loin : 40 blocs).
                if let Some(w) = self.wounds.get(&v.pseudo) {
                    if v.pos.distance_squared(cam) < 40.0 * 40.0 {
                        model.extend(crate::player_model::wound_model(&other, w));
                    }
                }
                // (v179) La bombe sur le dos de celui qui la porte (pas dans ses mains),
                // debout contre son dos.
                if v.flags & minelib_net::game::FLAG_BOMB != 0 && v.weapon != crate::cs::W_BOMB {
                    model.extend(crate::player_model::bomb_on_back(&other));
                }
            }
            // Armes et grenades au sol, avec le modèle tenu en main (plus simple au loin) ;
            // celles qui viennent d'être lâchées retombent en cloche.
            for g in n.ground() {
                let rest = Vec3::from(g.pos);
                let fly = self.ground_fly.get(&g.id).copied();
                let (pos, spin) = match fly {
                    Some(t) => (toss_arc(Vec3::from(g.from), rest, t), t * 11.0),
                    None => (rest, 0.0),
                };
                let turn = g.id as f32 * 1.7;
                let far = (pos - cam).length();
                if far > 64.0 {
                    continue;
                }
                let nade = crate::cs::nade_code(g.weapon);
                // (v172) Éclairés là où ils sont, comme les joueurs.
                let light = sky_light(&self.world, pos, self.daylight_shown);
                if far > 24.0 && fly.is_none() {
                    let len = crate::weapons::WEAPONS
                        .get(g.weapon as usize)
                        .map_or(0.18, |w| w.length);
                    model.extend(crate::player_model::gun_on_ground(pos, len, turn, light));
                } else if let Some(k) = nade {
                    let tris = crate::equip::nade_model(crate::equip::Nade::ALL[k]);
                    crate::player_model::item_on_ground(
                        &mut model, &tris, pos, turn, true, spin, light,
                    );
                } else if (g.weapon as usize) < crate::weapons::WEAPONS.len() {
                    crate::player_model::item_on_ground(
                        &mut model,
                        &crate::weapons::model(g.weapon as usize).tris,
                        pos,
                        turn,
                        true,
                        spin,
                        light,
                    );
                }
            }
            // Bombe posée ou lâchée : le voyant clignote, de plus en plus vite.
            let hud = n.hud();
            use minelib_net::game::bomb;
            if matches!(hud.bomb, bomb::PLANTED | bomb::DROPPED) {
                // (v177) Le voyant s'allume à chaque bip.
                let blink = hud.bomb == bomb::PLANTED && self.bomb_flash > 0.0;
                let at = match self.bomb_fly {
                    Some((from, to, t)) if hud.bomb == bomb::DROPPED => toss_arc(from, to, t),
                    _ => Vec3::from(hud.bomb_pos),
                };
                let light = sky_light(&self.world, at, self.daylight_shown);
                model.extend(crate::player_model::bomb(at, blink, light));
            }
        }
        // (v269) Les Évidés : leur corps cube par cube, leur ombre.
        let evides_before = self.evide_seen.borrow();
        let mut evides_now = HashMap::new();
        for e in &self.evides.list {
            if e.pos.distance(self.player.pos) > EVIDE_DRAWN {
                continue;
            }
            let light = sky_light(&self.world, e.eye(), self.daylight_shown);
            // (v290) Avec sa place à l'image d'avant (voir `mesh_moved`).
            let frames = e.mesh_moved(
                light,
                evides_before.get(&e.id).map(Vec::as_slice),
                &mut moved,
                &mut moved_prev,
            );
            evides_now.insert(e.id, frames);
            if !e.is_dying() {
                shadows.push((e.pos, e.shadow_boxes(), None));
            }
        }
        drop(evides_before);
        self.evide_seen.replace(evides_now);
        // Construction en ligne (et, en solo, la foule d'essai : dessinée pareil).
        // (v197) Sa place d'un coup : 4 000 joueurs, c'était une douzaine
        // d'agrandissements et autant de recopies par image.
        let mut others: Vec<Other> = Vec::with_capacity(
            self.build.as_ref().map_or(0, |b| b.others.len())
                + self.crowd.as_ref().map_or(0, |c| c.npcs.len())
                + self
                    .remote_crowds
                    .values()
                    .map(|rc| rc.npcs.len())
                    .sum::<usize>(),
        );
        // (v243) Dans la partie FPS du serveur : ses joueurs sont ceux de la partie (dessinés
        // plus haut, là où l'hôte les voit), pas leur position de construction.
        let in_match: std::collections::HashSet<String> = match &self.net {
            Some(n) if self.in_build_match() => {
                n.views().iter().map(|v| v.pseudo.to_lowercase()).collect()
            }
            _ => Default::default(),
        };
        if let Some(b) = &self.build {
            let shown = |o: &&crate::buildnet::Other| {
                in_match.is_empty() || !in_match.contains(&o.pseudo.to_lowercase())
            };
            others.extend(b.others.values().filter(shown).map(|o| Other {
                pseudo: &o.pseudo,
                pos: o.pos,
                yaw: o.yaw,
                pitch: o.pitch,
                crouch: o.crouch,
                walk: None,
                skin: self.skins.of(&o.pseudo),
                covered: None,
                held: o.held,
                hurt: 0.0,
            }));
        }
        if let Some(c) = &self.crowd {
            others.extend(c.npcs.iter().map(|n| Other {
                pseudo: &n.name,
                pos: n.pos,
                yaw: n.yaw,
                pitch: n.pitch,
                crouch: 0.0,
                walk: Some(&n.walk),
                // (v197) Son skin est gardé par lui (repris de temps en temps) : 4 000
                // recherches par pseudo et par image pour une réponse qui ne change
                // jamais.
                skin: n.skin.clone(),
                covered: Some(n.covered),
                held: None,
                hurt: n.hurt,
            }));
        }
        // (v227) Les foules d'essai des autres OP, dessinées pareil.
        for rc in self.remote_crowds.values() {
            others.extend(rc.npcs.iter().map(|n| Other {
                pseudo: &n.name,
                pos: n.pos,
                yaw: n.yaw,
                pitch: n.pitch,
                crouch: 0.0,
                walk: Some(&n.walk),
                skin: n.skin.clone(),
                covered: Some(n.covered),
                held: None,
                hurt: n.hurt,
            }));
        }
        if !others.is_empty() {
            let _p = minelib_render::perf::scope("joueurs : poses");
            // Les plus proches d'abord (ombres, et place limitée sur la carte : au-delà de
            // `MAX_PLAYERS`, les plus lointains en trois pavés).
            // (v191) Des milliers de joueurs : la distance de chacun est calculée une fois
            // (le tri la reprenait à chaque comparaison, des dizaines de milliers de fois).
            // (v192) Et ils sont **tous** préparés, où que regarde la caméra : la v191
            // écartait ceux qui n'étaient pas dans son cône, on les voyait apparaître en
            // tournant la tête. Jamais de joueur qui attend qu'on le regarde.
            let mut order = std::mem::take(&mut scratch.order);
            order.clear();
            order.extend(
                others
                    .iter()
                    .enumerate()
                    .map(|(i, o)| (o.pos.distance_squared(cam), i as u32)),
            );
            order.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
            for (rank, &(d2, i)) in order.iter().enumerate() {
                // (v215) Au bout de la vue, ils disparaissent dans le brouillard comme le
                // relief (distance hauteur comprise) : plus loin, plus rien à dessiner. Vu
                // de haut, ils restaient tous dessinés, nets dans le brouillard (et des
                // milliers dans une foule).
                if d2 > hidden2 {
                    break;
                }
                let o = &others[i as usize];
                // (v202) Ou tout près d'une lumière : son ombre y est géante, on la voit de
                // loin (avant, elle disparaissait dès qu'on s'éloignait un peu).
                // (v236) Les 48 plus proches (avant : 24), triés ensuite par l'axe de vue.
                let shadow = (rank < 48 && d2 < 48.0 * 48.0)
                    || light_shadow_score(&self.shadow_lights, o.pos) < SHADOW_LIGHT_NEAR;
                let w = o.walk.or_else(|| self.walks.get(o.pseudo));
                let other = crate::player_model::Pose {
                    feet: o.pos,
                    yaw: o.yaw,
                    pitch: o.pitch,
                    walk: w.map_or(0.0, |w| w.phase),
                    swing: w.map_or(0.0, |w| w.amp),
                    crouch: o.crouch,
                    light: hurt_tint(
                        match o.covered {
                            Some(c) => sky_light_covered(c, self.daylight_shown),
                            None => sky_light(
                                &self.world,
                                eye_of(o.pos, o.crouch),
                                self.daylight_shown,
                            ),
                        },
                        o.hurt,
                    ),
                    // (v222) Ce qu'il tient : sa lampe torche (bras tendu vers son regard,
                    // comme on la voit soi-même dans un miroir) ou la Foudre de Zeus (l'éclair
                    // est dessiné à part : voir `update_remote_bolts`).
                    held: o.held.map(|h| match h {
                        minelib_net::poses::Held::Lamp(l) => Held::Flashlight(l.on),
                        minelib_net::poses::Held::Zeus => Held::Zeus,
                    }),
                    skin: o.skin.clone(),
                    bow: 0.0,
                    air: w.map_or(0.0, |w| w.air),
                    dir: w.map_or(0.0, |w| w.dir),
                    run: w.map_or(0.0, |w| w.run),
                };
                if other.held.is_some() {
                    model.extend(crate::player_model::held_model(&other, true));
                }
                // (v222) Sa propre lampe n'est pas cachée par lui (voir `remote_lamps`).
                let own_lamp = self.lamp_owner(o.pseudo);
                let boxes_of = |other: &crate::player_model::Pose| {
                    let mut boxes = crate::player_model::shadow_boxes(other);
                    if let Some(bit) = own_lamp {
                        for b in &mut boxes {
                            b.owner |= bit;
                        }
                    }
                    boxes
                };
                if players.len() < minelib_render::MAX_PLAYERS {
                    let layer = layers.layer(other.skin.as_ref(), renderer);
                    let prev = w.map_or_else(
                        || crate::player_model::Prev::still(&other),
                        |w| w.prev,
                    );
                    let inst = crate::player_model::instance(&other, layer, &prev);
                    // (v217) Candidat aux boîtes exactes, sinon dans la carte du soleil,
                    // jusqu'à la distance réglée.
                    // (v273) L'atlas des joueurs (lumières dynamiques) aussi, seulement sans
                    // boîtes exactes : un joueur dans les deux avait l'ombre de l'atlas (moins
                    // fine) par-dessus la sienne, l'écart entre ses jambes bouché.
                    if d2 <= shadow_d2 {
                        if shadow {
                            shadows.push((o.pos, boxes_of(&other), Some(inst)));
                        } else {
                            sun_casters.push(inst);
                            light_casters.push(inst);
                        }
                    }
                    players.push(inst);
                } else {
                    if shadow && d2 <= shadow_d2 {
                        shadows.push((o.pos, boxes_of(&other), None));
                    }
                    model.extend(crate::player_model::build_tiny(&other));
                }
            }
            scratch.order = order;
        }
        // (v190) À la première personne, seuls les miroirs montrent le joueur : son modèle
        // (avec son skin) et ce qu'il tient. (En F5, il est déjà dans la scène.)
        let mirror_me = third == 0 && visible && self.spectating.is_none();
        let mirror_self = mirror_me.then(|| {
            let layer = layers.layer(pose.skin.as_ref(), renderer);
            crate::player_model::instance(&pose, layer, &crate::player_model::Prev::still(&pose))
        });
        let mirror_held = if mirror_me {
            crate::player_model::held_model(&pose, true)
        } else {
            Vec::new()
        };
        drop(layers);
        // Objets au sol : ils flottent et tournent. (v185) Leur vrai modèle, éclairés comme
        // les joueurs (lumière du ciel à leur place).
        if self.game.mode != GameMode::Fps {
            let (world, daylight) = (&self.world, self.daylight_shown);
            self.items
                .build(world, cam, &|p| sky_light(world, p, daylight), &mut model);
        }
        // Ombres des joueurs : les plus proches de la caméra (place limitée), (v202) ou d'une
        // lumière (ombre géante, voir `light_shadow_score`). (v236) « Fais la précision des
        // ombres normalement pour les joueurs les plus proches de moi ou de mon axe de
        // vue » : la distance compte double de côté, triple derrière (`shadow_view_score`).
        let lights = &self.shadow_lights;
        let look = view
            .inverse()
            .transform_vector3(-Vec3::Z)
            .normalize_or_zero();
        shadows.sort_by_cached_key(|s| {
            let score = shadow_view_score(cam, look, s.0).min(light_shadow_score(lights, s.0));
            (score * 64.0) as u32
        });
        let mut shadow_boxes = std::mem::take(&mut scratch.boxes);
        shadow_boxes.clear();
        // (v217) Joueur par joueur, tant que toutes ses boîtes tiennent ; les suivants dans la
        // carte du soleil (avant, les boîtes en trop étaient perdues, et un joueur pouvait
        // n'avoir que la moitié de son ombre).
        for (_, boxes, inst) in shadows.iter() {
            if shadow_boxes.len() + boxes.len() <= minelib_render::MAX_SHADOW_BOXES {
                shadow_boxes.extend_from_slice(boxes);
            } else if let Some(i) = inst {
                sun_casters.push(*i);
                // (v273) Ses boîtes ne tiennent plus : l'atlas des joueurs pour les lumières
                // dynamiques (avant, tous y étaient, boîtes exactes ou non).
                light_casters.push(*i);
            }
        }
        // Morceau de joueur que des blocs cachent du soleil : pas d'ombre du soleil (on ne
        // voit pas son ombre à l'abri d'un toit, d'un arbre…).
        {
            let seen = &mut *self.sun_seen.borrow_mut();
            for b in &mut shadow_boxes {
                b.sunlit = sun_reaches_seen(seen, &self.world, b, self.sun);
            }
        }
        // Le joueur se voit dans les miroirs (et l'eau, le métal), même à la 1re personne.
        // Avec son vrai skin (envoyé à la carte quand il change). (v256) En partie FPS : la
        // tenue de son équipe.
        let skin = self.worn_skin();
        let key = skin
            .as_ref()
            .map_or(0, |s| std::sync::Arc::as_ptr(s) as usize);
        renderer.set_skin(key, || skin.as_ref().map(|s| s.rgba()));
        let ray_boxes = if visible {
            crate::player_model::ray_boxes(&pose)
        } else {
            Vec::new()
        };
        // (v215) Lampe torche en main, à la première personne : la lampe et le bras du modèle
        // (pieds remontés comme la caméra sur une marche), dessinés dans la vue du monde.
        // (v239) Tout objet tenu en construction (bloc, tête, torche, baguette, écran) aussi :
        // le bras du modèle et l'objet là où il le tient, comme en F5. (v240) La Foudre de
        // Zeus aussi (son éclair est dessiné avec l'orage, au poing du modèle). Pas la main
        // vide.
        let world_proj =
            Mat4::perspective_infinite_reverse_rh(fov.to_radians(), renderer.aspect(), 0.05);
        // (v220) Interface masquée (F1) : ni main ni objet tenu à la première personne.
        let hud_off = self.hud_off();
        // (v247) « La main par défaut : change-la par celle qu'on calcule comme avec un objet en
        // main » (et « accroupi à la première personne, mon bras doit bouger avec moi comme en
        // F5 ») : la main vide est le bras du modèle, tendu comme s'il tenait un bloc, sans
        // rien dedans (avant : une main dessinée à part, à l'écran, qui ne suivait pas
        // l'accroupi).
        // (v290) La main vide pend le long du corps (le bras droit du modèle, dessiné avec le
        // corps : `first_person_body`) ; le temps d'un geste (frapper, casser, poser), le bras
        // se tend comme s'il tenait un bloc (un bloc d'air : rien dedans), monté depuis l'épaule
        // (`hand_raise`), puis retombe.
        let empty_hand = pose.held.is_none() && !fps_mode;
        let first_hold = match pose.held {
            Some(
                h @ (Held::Flashlight(_)
                | Held::Block(_)
                | Held::Torch(_)
                | Held::Wand(_)
                | Held::Zeus),
            ) => Some((h, true)),
            None if empty_hand && self.hand_raise > 0.0 => {
                Some((Held::Block(minelib_world::BlockId::AIR), false))
            }
            _ => None,
        };
        let first_person_lamp = (third == 0
            && !fps_mode
            && !hud_off
            && self.spectating.is_none()
            && self.game.mode != GameMode::Spectator)
            .then_some(first_hold)
            .flatten()
            .and_then(|(held, item)| {
                // (v220) Le coup vers le bas de la main (bloc cassé). (v222) La lampe et le bras
                // tournent ensemble autour de l'épaule (la base du bras, bout z = 1 de sa
                // boîte), comme la main dessinée autour de la sienne : seul le bout descend.
                // (v239) Et l'arc en posant ou en frappant, l'objet qui remonte en changeant.
                let (p, dip) = self.first_person_hold(held, view);
                let arm = crate::player_model::right_arm_matrix(&p)?;
                let lamp = crate::player_model::held_item_matrix(&p).filter(|_| item);
                // (v290) La main vide qui se tend pour un geste, ou retombe après : tournée
                // vers le bas autour de l'épaule de ce qu'il lui reste à monter.
                let lift = if empty_hand && self.hand_raise < 1.0 {
                    let pivot =
                        view.transform_point3(arm.transform_point3(Vec3::new(0.5, 0.5, 1.0)));
                    view.inverse()
                        * Mat4::from_translation(pivot)
                        * Mat4::from_rotation_x(-HAND_DOWN * (1.0 - self.hand_raise))
                        * Mat4::from_translation(-pivot)
                        * view
                } else {
                    Mat4::IDENTITY
                };
                Some((lamp.map(|m| dip * m), lift * dip * arm))
            });
        let hand = if self.spectating.is_some() && fps_mode {
            // (v209) Spectateur : l'arme du joueur regardé dans ses mains, et ce qui est dans
            // le monde (balles traçantes, grenades en vol, impacts) comme en jouant.
            let mut v = self.spec_fps.viewmodel(renderer.aspect(), self.hand_light);
            let proj =
                Mat4::perspective_infinite_reverse_rh(fov.to_radians(), renderer.aspect(), 0.05);
            v.extend(
                self.fps
                    .world_fx(proj * view, self.player.eye(), &self.world, self.daylight),
            );
            v
        } else if self.spectating.is_some() {
            Vec::new()
        } else if fps_mode {
            // Arme en main, puis balles traçantes, étincelles et impacts dans le monde.
            let mut v = self.fps.viewmodel(renderer.aspect(), self.hand_light);
            let proj =
                Mat4::perspective_infinite_reverse_rh(fov.to_radians(), renderer.aspect(), 0.05);
            v.extend(
                self.fps
                    .world_fx(proj * view, self.player.eye(), &self.world, self.daylight),
            );
            v
        } else {
            // À la 3e personne : pas de main à l'écran (le modèle est dans le monde).
            let mut v = if let Some((Some(item), _)) = first_person_lamp {
                // (v215) Lampe torche : dessinée là où le modèle la tient (vue du monde), là
                // où la lumière part et où la montrent les miroirs. (v239) Tout objet tenu.
                crate::hand::item_mesh(
                    self.held_item(),
                    world_proj * view * item,
                    self.hand_light,
                    self.flashlight_on,
                )
            } else {
                // (La Foudre de Zeus est dessinée avec l'orage ; la main vide n'a que le bras.)
                Vec::new()
            };
            // Sélection WorldEdit : arêtes de la zone et coins (en créatif, baguette en main).
            if self.game.mode == GameMode::Creative
                && self.inventory.selected_item() == Some(Item::Wand)
                && !hud_off
            {
                let proj = Mat4::perspective_infinite_reverse_rh(
                    fov.to_radians(),
                    renderer.aspect(),
                    0.05,
                );
                // (v258) `/fps showelements` : une sélection qui est un élément de la carte en
                // prend la couleur (« si je viens de poser le départ T, je vois la sélection
                // passer en jaune »).
                let map = self
                    .fps_elements
                    .as_ref()
                    .filter(|_| self.game.mode != GameMode::Fps)
                    .map(|e| &e.map);
                v.extend(selection_lines(&self.we, map, proj * view, cam));
            }
            // (v272) Les gouttes de sang de nos morsures, en vol (dessinées jusqu'ici en
            // partie FPS seulement).
            if self.fps.has_drops() {
                let proj = Mat4::perspective_infinite_reverse_rh(
                    fov.to_radians(),
                    renderer.aspect(),
                    0.05,
                );
                v.extend(self.fps.world_fx(proj * view, cam, &self.world, self.daylight));
            }
            // (v255) `/fps showelements` : portes, sites et départs, à travers les murs.
            if let Some(e) = &self.fps_elements {
                if self.game.mode != GameMode::Fps && !hud_off {
                    let proj = Mat4::perspective_infinite_reverse_rh(
                        fov.to_radians(),
                        renderer.aspect(),
                        0.05,
                    );
                    v.extend(element_lines(e, proj * view, cam));
                }
            }
            v
        };
        // Bras à la première personne (pas en mode FPS, qui a ses armes, ni en spectateur) :
        // éclairé comme le modèle du joueur en F5.
        let mut arm = if let Some((_, arm)) = first_person_lamp {
            // (v215) Lampe torche : le bras droit du modèle, tendu, qui la tient. (v239) Tout
            // objet tenu en construction.
            crate::hand::Hand::arm_world(world_proj * view, arm, self.hand_light, skin.as_deref())
        } else if third == 0
            && !fps_mode
            && !hud_off
            && !empty_hand
            && self.spectating.is_none()
            && self.game.mode != GameMode::Spectator
        {
            self.hand.arm(
                renderer.aspect(),
                fov,
                self.hand_light,
                skin.as_deref(),
                view.inverse(),
            )
        } else {
            Vec::new()
        };
        // (v267) « On doit voir notre corps dans le jeu […] pas en mode FPS » : son propre
        // corps à la première personne, quand le regard est assez baissé pour qu'il entre
        // dans l'image (avec ses plaies). Ni spectateur, ni interface masquée (F1).
        if third == 0
            && !fps_mode
            && !hud_off
            && visible
            && self.spectating.is_none()
            && self.player.pitch < crate::player_model::FIRST_PERSON_PITCH
        {
            let wounds = self.wounds.get("").map_or(&[][..], Vec::as_slice);
            // (v273) Pieds remontés comme la caméra et le bras sur une marche (`step_offset`) :
            // le corps ne saute plus dans l'image quand la caméra monte la marche en douceur.
            arm.extend(crate::player_model::first_person_body(
                &pose.at(pose.feet + Vec3::Y * self.step_offset),
                wounds,
                self.flesh.get(""),
                world_proj * view,
                // (v290) La main vide au repos : le bras droit pend, avec le corps.
                empty_hand && self.hand_raise <= 0.0,
            ));
        }
        let smokes = self.fps.smoke_volumes();
        let flames = self.fps.flames();
        // Têtes (par zone) et cadres des cartes importées.
        let mut statics = self
            .heads
            .as_ref()
            .map_or_else(Vec::new, |h| h.visible(cam));
        if let Some(f) = self.frames.as_ref().filter(|f| !f.is_empty()) {
            let (v, gen) = f.current();
            statics.push((u64::MAX, v, gen));
        }
        // (v299) Les morts tombés là : leur maillage n'est renvoyé à la carte que quand il
        // change (ils tombent, ils fondent).
        if !fps_mode {
            statics.extend(self.fallen.iter().map(|f| f.lot()));
        }
        let screens: Vec<(u32, minelib_render::ScreenAnchor)> = self
            .shown
            .iter()
            .map(|s| {
                let id = s.0.id();
                // (v289) Les boutons en haut à gauche sont partis dans le volet : le shader ne
                // les dessine plus (apparition nulle).
                let reveal = [0.0, 0.0];
                // (v243) Le curseur et l'onde d'un appui, dessinés au-dessus des boutons.
                let (cursor, ripple) = self.displays.marks(id);
                (
                    id,
                    minelib_render::ScreenAnchor {
                        reveal,
                        cursor,
                        ripple,
                        ..s.0.anchor()
                    },
                )
            })
            .collect();
        renderer.set_screens(&screens);
        for (id, _) in &screens {
            if let Some((version, w, h, pixels)) = self.displays.frame(*id) {
                renderer.set_screen_frame(*id, version, w, h, pixels);
            }
        }
        // (v217) Le relief au loin, pour le sol dessiné sous l'horizon au-delà du monde
        // chargé.
        let far_ground =
            crate::storm::far_ground(&self.world, cam, fog_end, &|layer| renderer.layer_mean(layer));
        // (v290) Ce qui dit son mouvement : à la fin des entités.
        model.extend(moved);
        let params = FrameParams {
            ray_boxes: &ray_boxes,
            smokes: &smokes,
            held_light_pos: crate::player_model::torch_tip(&pose),
            ray_light: self.hand_light,
            shadow_boxes: &shadow_boxes,
            sun_casters: &sun_casters,
            light_casters: &light_casters,
            player_shadow_distance: shadow_d,
            viewmodel: &hand,
            arm: &arm,
            entities: &model,
            entities_follow: follow,
            entities_moved: &moved_prev,
            static_entities: &statics,
            players: &players,
            eye: cam,
            water_level: crate::underwater::eye_water_level(&self.world, cam),
            water_mirror: if self.water_mirror_on {
                self.seen_water.borrow_mut().update(
                    &self.world,
                    cam,
                    view,
                    fov.to_radians(),
                    renderer.aspect(),
                )
            } else {
                None
            },
            player_eye: self.player.eye(),
            view,
            fov_y: fov.to_radians(),
            // (v215) Bande de brouillard au bord de la distance de vue deux fois plus
            // étroite (avant : de 45 % à 100 % de la distance, maintenant de 72,5 %).
            // (v216) Sous la pluie, le lointain se voile de plus près.
            fog_start: fog_end * 0.725 * self.storm.visibility().powi(2),
            fog_end,
            held_light: match self.inventory.selected_item() {
                // (v201) Torches en lumières ponctuelles : la torche tenue a la sienne.
                Some(Item::Torch(_)) if self.lights.point_torches && renderer.torch_swap_on() => {
                    None
                }
                Some(Item::Torch(chem)) if self.game.mode.has_inventory() => {
                    let rgb = chem
                        .and_then(|i| minelib_world::flame::CHEMICALS.get(i))
                        .map_or(minelib_world::flame::WOOD_FLAME, |c| c.rgb);
                    Some((rgb, 10.0))
                }
                // (v216) Foudre de Zeus : sa lumière simple, tant que les lumières
                // dynamiques ne sont pas dessinées (sinon c'est la sienne, avec ombres).
                Some(Item::Zeus) if self.holding_zeus() && !renderer.dyn_lights_drawn() => {
                    Some(([170, 205, 255], 11.0))
                }
                _ => None,
            },
            // Pas de contour du bloc visé en mode FPS (ni dans l'éditeur de lumières, ni
            // (v220) l'interface masquée).
            selected: if grabbed
                && self.game.mode != GameMode::Fps
                && !self.light_editor.open
                && !self.hud_off()
            {
                // Hitbox de l'écran visé (à portée de main), sinon cadre, sinon bloc.
                self.screen_aim
                    .filter(|a| a.1 <= REACH)
                    .and_then(|a| self.shown.get(a.0))
                    .map(|s| {
                        let (a, b) = s.0.hitbox();
                        (a.to_array(), b.to_array())
                    })
                    .or_else(|| self.frame_aim.and_then(|i| self.frames.as_ref()?.bounds(i)))
                    .or_else(|| {
                        self.hit
                            .map(|h| minelib_world::raycast::selection_box(&self.world, h.block))
                    })
            } else {
                None
            },
            time,
            sky_time: self.sky_clock,
            daylight: self.daylight_shown,
            sun: self.sun,
            moon: self.moon,
            weather: self.storm.sky(),
            sky_flash: self.storm.sky_flash,
            far_ground,
            // Par beau temps le sol lointain se voit loin ; sous la pluie et l'orage il se
            // perd vite dans le rideau de pluie : l'horizon est bouché.
            haze_len: {
                let w = self.storm.sky();
                3.5 * fog_end * self.storm.visibility().powi(2) / (1.0 + 3.0 * w.rain + 3.0 * w.storm)
            },
            bolts: &self.storm.bolts,
            hand_bolts: &self.storm.hand_bolts,
            rain: self.storm.rain(self.daylight_shown.min(1.0)),
            flames: &flames,
            decals: self.fps.decals(),
            ui: Some(ui),
            overlay: show_debug.then_some(&self.overlay),
            mirror_self,
            mirror_held: &mirror_held,
        };
        let stats = renderer.render(&params);
        // (v197) Les tableaux retournent au tas de l'image suivante, avec leur place déjà
        // réservée (voir `Scratch`).
        drop(params);
        scratch.players = players;
        scratch.shadows = shadows;
        scratch.boxes = shadow_boxes;
        scratch.sun_casters = sun_casters;
        scratch.light_casters = light_casters;
        *self.scratch.borrow_mut() = scratch;
        stats
    }
}

/// Une porte ou une trappe vient de s'ouvrir (sinon, de se fermer) : `changes`, les blocs
/// qu'elle devient.
fn door_opened(changes: &[([i32; 3], BlockId)]) -> bool {
    changes.iter().any(|(_, b)| match hinged(*b) {
        Some(Hinged::Door { .. }) => {
            let k = &b.def().key;
            k.ends_with(":o") || k.ends_with(":m")
        }
        Some(Hinged::Trapdoor { side, .. }) => side != 2 && side != 3,
        None => false,
    })
}

/// Porte ou trappe.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Hinged {
    Door {
        wood: &'static str,
        upper: bool,
        side: u8,
    },
    /// `side` : 2 plafond, 3 dessous (fermée), sinon côté contre lequel elle est relevée ;
    /// `hinge` : fermée, côté où elle se relève (choisi à la pose), `None` : trappe
    /// d'avant (se relève à l'opposé du joueur).
    Trapdoor {
        kind: &'static str,
        side: u8,
        hinge: Option<u8>,
    },
}

pub(crate) fn hinged(id: BlockId) -> Option<Hinged> {
    let key: &'static str = &id.def().key;
    if let Some(rest) = key.strip_prefix("door_") {
        let mut it = rest.split(':');
        let wood = it.next()?;
        let half = it.next()?;
        let side = it.next()?.parse().ok()?;
        return Some(Hinged::Door {
            wood,
            upper: half == "upper",
            side,
        });
    }
    let mut it = key.split(':');
    let kind = it.next()?;
    if kind == "trapdoor" || kind == "iron_trapdoor" {
        let side = it.next()?.parse().ok()?;
        let hinge = match it.next() {
            Some(h) => Some(h.parse().ok()?),
            None => None,
        };
        return Some(Hinged::Trapdoor { kind, side, hinge });
    }
    None
}

fn hinged_block(h: Hinged) -> Option<BlockId> {
    let reg = minelib_world::registry::registry();
    match h {
        Hinged::Door { wood, upper, side } => reg.by_key(&format!(
            "door_{wood}:{}:{side}",
            if upper { "upper" } else { "lower" }
        )),
        Hinged::Trapdoor {
            kind,
            side,
            hinge: Some(h),
        } if side == 2 || side == 3 => reg.by_key(&format!("{kind}:{side}:{h}")),
        Hinged::Trapdoor { kind, side, .. } => reg.by_key(&format!("{kind}:{side}")),
    }
}

/// Même porte ou même trappe (essence, moitié), orientation mise à part.
fn same_family(a: BlockId, b: BlockId) -> bool {
    match (hinged(a), hinged(b)) {
        (
            Some(Hinged::Door {
                wood: w1,
                upper: u1,
                ..
            }),
            Some(Hinged::Door {
                wood: w2,
                upper: u2,
                ..
            }),
        ) => w1 == w2 && u1 == u2,
        (Some(Hinged::Trapdoor { kind: k1, .. }), Some(Hinged::Trapdoor { kind: k2, .. })) => {
            k1 == k2
        }
        _ => false,
    }
}

/// (v197) Un réglage oui/non tel que les commandes l'écrivent.
fn on_off(b: bool) -> &'static str {
    if b {
        "on"
    } else {
        "off"
    }
}

/// (v172) Distance (blocs) où les pseudos des autres joueurs ont leur taille normale.
/// (v189) 8 au lieu de 6,5 : un peu plus grands à toute distance.
const NAME_SIZE_AT: f32 = 8.0;

/// (v189) Pseudos des autres joueurs (et des NPC) affichés jusqu'à cette distance (blocs),
/// effacés peu à peu sur les [`NAME_FADE`] derniers blocs. Avant : jusqu'à ce que le texte
/// passe sous 7 pixels de haut, 15 à 22 blocs selon la taille de l'écran.
const NAME_RANGE: f32 = 32.0;
/// (v217) Opacité du fond des pseudos au-dessus des joueurs (0,45 avant) : « j'ai du mal à
/// lire les pseudos quand ils se superposent » (Viendax).
const NAME_BACK: f32 = 0.82;
const NAME_FADE: f32 = 4.0;

/// (v189) Pseudo à `d` blocs de la caméra (`s` : échelle de l'interface) : taille du texte
/// et opacité, `None` s'il n'est pas affiché. Taille comme celle d'un objet du monde (v172),
/// plus petite plus loin, plus grande plus près (4 fois au plus), mais jamais sous celle de
/// la police (8 pixels de haut) : lisible jusqu'au bout.
fn name_label(d: f32, s: f32) -> Option<(f32, f32)> {
    let alpha = ((NAME_RANGE - d) / NAME_FADE).clamp(0.0, 1.0);
    let ts = (s * (NAME_SIZE_AT / d.max(0.1)).min(4.0)).max(1.0);
    (alpha > 0.0).then_some((ts, alpha))
}

/// (v165) Ping montré dans la liste des joueurs : texte et couleur (vert, jaune, rouge
/// selon sa valeur ; gris s'il n'est pas connu).
fn ping_label(ms: Option<u16>) -> (String, [f32; 4]) {
    match ms {
        None => ("-".into(), [0.55, 0.55, 0.6, 1.0]),
        Some(ms) => {
            let c = match ms {
                0..=59 => [0.55, 0.95, 0.55, 1.0],
                60..=129 => [1.0, 0.85, 0.35, 1.0],
                _ => [1.0, 0.45, 0.4, 1.0],
            };
            (format!("{ms} ms"), c)
        }
    }
}

/// Objet d'une tête : simple (skin par défaut du jeu) ou avec son skin.
fn head_item(skin: crate::heads::HeadSkin) -> Item {
    match skin {
        crate::heads::HeadSkin::Default => {
            Item::Block(minelib_world::registry::skull_block(false, 0))
        }
        s => Item::Head(crate::heads::intern(&s)),
    }
}

/// Rotation (seizièmes de tour, comme Minecraft : 0 visage au nord, 4 à l'est) d'une tête
/// posée au sol par un joueur qui regarde dans la direction `look` : elle le regarde.
fn floor_rotation(look: Vec3) -> u8 {
    let face = -Vec3::new(look.x, 0.0, look.z);
    if face.length_squared() < 1e-8 {
        return 0;
    }
    let t = face.x.atan2(-face.z);
    ((t / (std::f32::consts::TAU / 16.0)).round() as i32).rem_euclid(16) as u8
}

/// Côté vertical (0 +X, 1 −X, 4 +Z, 5 −Z) dans la direction horizontale dominante de `v`.
/// (v215) Une boîte (coins `mn`, `mx`) a-t-elle un bout dans l'image de `view_proj` ?
/// Prudent : écartée seulement si ses huit coins sont tous du même côté d'un des plans
/// du champ (gauche, droite, bas, haut, devant).
fn box_in_view(view_proj: Mat4, mn: Vec3, mx: Vec3) -> bool {
    let corners: Vec<glam::Vec4> = (0..8)
        .map(|i| {
            let p = Vec3::new(
                if i & 1 == 0 { mn.x } else { mx.x },
                if i & 2 == 0 { mn.y } else { mx.y },
                if i & 4 == 0 { mn.z } else { mx.z },
            );
            view_proj * p.extend(1.0)
        })
        .collect();
    let out = |f: &dyn Fn(&glam::Vec4) -> bool| corners.iter().all(f);
    !(out(&|c| c.x < -c.w)
        || out(&|c| c.x > c.w)
        || out(&|c| c.y < -c.w)
        || out(&|c| c.y > c.w)
        || out(&|c| c.w <= 0.0)
        || out(&|c| c.z > c.w))
}

fn side_toward(v: Vec3) -> u8 {
    if v.x.abs() > v.z.abs() {
        if v.x > 0.0 {
            0
        } else {
            1
        }
    } else if v.z > 0.0 {
        4
    } else {
        5
    }
}

/// Porte : quart de tour (même sens que les portes de Minecraft importées).
fn door_turn(side: u8) -> u8 {
    match side {
        1 => 5,
        5 => 0,
        0 => 4,
        _ => 1,
    }
}

/// Case voisine dans la direction du côté `side` (0 +X, 1 −X, 4 +Z, 5 −Z).
fn side_step(c: [i32; 3], side: u8) -> [i32; 3] {
    match side {
        0 => [c[0] + 1, c[1], c[2]],
        1 => [c[0] - 1, c[1], c[2]],
        4 => [c[0], c[1], c[2] + 1],
        _ => [c[0], c[1], c[2] - 1],
    }
}

fn opposite_side(side: u8) -> u8 {
    match side {
        0 => 1,
        1 => 0,
        4 => 5,
        _ => 4,
    }
}

/// Bloc fermé d'une porte ouverte (`…:o`, `…:m` : le côté qu'elle fermait se lit dans
/// son nom) ; `None` pour une porte fermée ou un autre bloc.
fn door_closed_block(id: BlockId) -> Option<BlockId> {
    let key: &str = &id.def().key;
    let flipped = match key.rsplit(':').next() {
        Some("o") => false,
        Some("m") => true,
        _ => return None,
    };
    let Hinged::Door { wood, upper, side } = hinged(id)? else {
        return None;
    };
    // Inverse de `door_turn` (après le demi-tour d'une porte à gonds inversés).
    let turned = if flipped { opposite_side(side) } else { side };
    let closed = match turned {
        5 => 1,
        0 => 5,
        4 => 0,
        _ => 4,
    };
    hinged_block(Hinged::Door {
        wood,
        upper,
        side: closed,
    })
}

/// Porte fermée en `c` (son état d'origine si elle est ouverte en ce moment).
fn closed_door(
    get: &dyn Fn([i32; 3]) -> BlockId,
    opened: &HashMap<[i32; 3], BlockId>,
    c: [i32; 3],
) -> Option<(&'static str, bool, u8)> {
    let id = opened.get(&c).copied().unwrap_or_else(|| get(c));
    let id = door_closed_block(id).unwrap_or(id);
    match hinged(id)? {
        Hinged::Door { wood, upper, side } => Some((wood, upper, side)),
        Hinged::Trapdoor { .. } => None,
    }
}

/// Double porte : une porte fermée a ses gonds du côté `door_turn` ; si une porte jumelle
/// (même essence, même moitié, même côté) est collée de ce côté-là, ses gonds passent de
/// l'autre côté, comme dans Minecraft : les deux battants s'ouvrent en miroir, gonds à
/// l'extérieur. `Some(true)` : gonds inversés.
fn door_flipped(
    get: &dyn Fn([i32; 3]) -> BlockId,
    opened: &HashMap<[i32; 3], BlockId>,
    c: [i32; 3],
) -> Option<bool> {
    let (wood, upper, side) = closed_door(get, opened, c)?;
    let n = side_step(c, door_turn(side));
    Some(closed_door(get, opened, n) == Some((wood, upper, side)))
}

/// Côté où se range le battant ouvert de la porte en `c` (fermée).
fn door_open_side(
    get: &dyn Fn([i32; 3]) -> BlockId,
    opened: &HashMap<[i32; 3], BlockId>,
    c: [i32; 3],
) -> Option<u8> {
    let (_, _, side) = closed_door(get, opened, c)?;
    let turn = door_turn(side);
    Some(if door_flipped(get, opened, c)? {
        opposite_side(turn)
    } else {
        turn
    })
}

/// Autre battant de la double porte dont fait partie la porte en `c`, s'il y en a un.
pub(crate) fn door_partner(
    get: &dyn Fn([i32; 3]) -> BlockId,
    opened: &HashMap<[i32; 3], BlockId>,
    c: [i32; 3],
) -> Option<[i32; 3]> {
    let (wood, upper, side) = closed_door(get, opened, c)?;
    let turn = door_turn(side);
    if door_flipped(get, opened, c)? {
        return Some(side_step(c, turn));
    }
    let n = side_step(c, opposite_side(turn));
    (closed_door(get, opened, n) == Some((wood, upper, side))
        && door_flipped(get, opened, n) == Some(true))
    .then_some(n)
}

/// Comme `toggle_hinged_with` ; avec `pair`, l'autre battant d'une double porte suit
/// (s'il était dans le même état : les deux s'ouvrent ou se ferment ensemble).
pub(crate) fn toggle_door_pair_with(
    get: &dyn Fn([i32; 3]) -> BlockId,
    opened: &mut HashMap<[i32; 3], BlockId>,
    p: [i32; 3],
    look: Vec3,
    pair: bool,
) -> Vec<([i32; 3], BlockId)> {
    let partner = pair.then(|| door_partner(get, opened, p)).flatten();
    let state = |o: &HashMap<[i32; 3], BlockId>, c: [i32; 3]| o.contains_key(&c);
    let same = partner.is_some_and(|q| state(opened, p) == state(opened, q));
    let mut out = toggle_hinged_with(get, opened, p, look);
    if let Some(q) = partner.filter(|_| same && !out.is_empty()) {
        out.extend(toggle_hinged_with(get, opened, q, look));
    }
    out
}

/// Ouvre ou ferme la porte / trappe en `p` ; renvoie les blocs à changer. Une porte
/// ouverte pendant la partie retrouve exactement sa place en se refermant ; une trappe se
/// relève contre le côté opposé au joueur.
fn toggle_hinged(
    world: &World,
    opened: &mut HashMap<[i32; 3], BlockId>,
    p: [i32; 3],
    look: Vec3,
) -> Vec<([i32; 3], BlockId)> {
    toggle_hinged_with(&|c| world.get(c[0], c[1], c[2]), opened, p, look)
}

/// Comme `toggle_hinged`, en lisant les blocs avec `get` (monde vu par une partie FPS, avec
/// ses portes déjà changées).
pub(crate) fn toggle_hinged_with(
    get: &dyn Fn([i32; 3]) -> BlockId,
    opened: &mut HashMap<[i32; 3], BlockId>,
    p: [i32; 3],
    look: Vec3,
) -> Vec<([i32; 3], BlockId)> {
    let id = get(p);
    let Some(h) = hinged(id) else {
        return Vec::new();
    };
    match h {
        Hinged::Door { upper, .. } => {
            let other = [p[0], if upper { p[1] - 1 } else { p[1] + 1 }, p[2]];
            let mut cells = vec![p];
            if matches!(hinged(get(other)), Some(Hinged::Door { .. })) {
                cells.push(other);
            }
            // Sens d'ouverture de chaque moitié, calculé avant de toucher à `opened`.
            let turns: Vec<Option<u8>> = cells
                .iter()
                .map(|&c| door_open_side(get, opened, c))
                .collect();
            let mut out = Vec::new();
            for (c, turn) in cells.into_iter().zip(turns) {
                let cur = get(c);
                if let Some(closed) = opened.remove(&c).filter(|&b| same_family(b, cur)) {
                    out.push((c, closed));
                } else if let Some(closed) = door_closed_block(cur) {
                    // Ouverte depuis toujours (carte importée, monde rechargé) : elle se
                    // referme du côté que dit son nom.
                    out.push((c, closed));
                } else if let Some(Hinged::Door { wood, upper, side }) = hinged(cur) {
                    let open = Hinged::Door {
                        wood,
                        upper,
                        side: turn.unwrap_or_else(|| door_turn(side)),
                    };
                    // Bloc d'une porte ouverte : `…:o`, ou `…:m` si ses gonds sont de l'autre
                    // côté (battant droit d'une double porte) ; le maillage y lit où sont
                    // les gonds.
                    let flipped = turn.is_some_and(|t| t != door_turn(side));
                    let block = hinged_block(open).and_then(|b| {
                        let state = if flipped { "m" } else { "o" };
                        minelib_world::registry::registry()
                            .by_key(&format!("{}:{state}", b.def().key))
                    });
                    if let Some(b) = block {
                        opened.insert(c, cur);
                        out.push((c, b));
                    }
                }
            }
            out
        }
        Hinged::Trapdoor { kind, side, hinge } => {
            if let Some(closed) = opened.remove(&p).filter(|&b| same_family(b, id)) {
                return vec![(p, closed)];
            }
            let closed = side == 2 || side == 3;
            // Fermée : se relève du côté choisi à la pose (trappe d'avant : à l'opposé du
            // joueur) ; ouverte : se rabat en dessous, en gardant ce côté.
            let (new_side, new_hinge) = if closed {
                (hinge.unwrap_or_else(|| side_toward(look)), None)
            } else {
                (3, Some(side))
            };
            match hinged_block(Hinged::Trapdoor {
                kind,
                side: new_side,
                hinge: new_hinge,
            }) {
                Some(b) => {
                    if side == 2 || side == 3 {
                        opened.insert(p, id);
                    }
                    vec![(p, b)]
                }
                None => Vec::new(),
            }
        }
    }
}

/// Pose d'une porte (moitiés basse et haute, au bord de la case le plus proche du joueur)
/// ou d'une trappe (dessous / plafond / moitié visée sur un côté). `None` sinon.
fn hinged_placement(
    hit: &Hit,
    item: BlockId,
    look: Vec3,
    point: Vec3,
) -> Option<Vec<([i32; 3], BlockId)>> {
    let p = hit.adjacent();
    match hinged(item)? {
        Hinged::Door { wood, .. } => {
            // Bord le plus proche du joueur : à l'opposé du regard.
            let side = side_toward(-look);
            let lower = hinged_block(Hinged::Door {
                wood,
                upper: false,
                side,
            })?;
            let upper = hinged_block(Hinged::Door {
                wood,
                upper: true,
                side,
            })?;
            Some(vec![(p, lower), ([p[0], p[1] + 1, p[2]], upper)])
        }
        Hinged::Trapdoor { kind, .. } => {
            let side = match hit.normal {
                [0, 1, 0] => 3,
                [0, -1, 0] => 2,
                _ if point.y - point.y.floor() > 0.5 => 2,
                _ => 3,
            };
            // Côté où elle se relèvera, comme dans Minecraft : contre le mur visé, sinon
            // le bord le plus proche du joueur.
            let hinge = match hit.normal {
                [0, _, 0] => side_toward(-look),
                [x, _, z] => side_toward(-Vec3::new(x as f32, 0.0, z as f32)),
            };
            Some(vec![(
                p,
                hinged_block(Hinged::Trapdoor {
                    kind,
                    side,
                    hinge: Some(hinge),
                })?,
            )])
        }
    }
}

/// Dalle : matériau et côté occupé (0 +X, 1 −X, 2 haut, 3 bas, 4 +Z, 5 −Z).
fn slab_parts(id: BlockId) -> Option<(&'static str, u8)> {
    let rest = id.def().key.strip_prefix("slab:")?;
    let (mat, half) = rest.rsplit_once(':')?;
    let side = match half {
        "bottom" => 3,
        "top" => 2,
        h => h.parse().ok()?,
    };
    Some((mat, side))
}

fn slab_of(mat: &str, side: u8) -> Option<BlockId> {
    let half = match side {
        2 => "top".to_string(),
        3 => "bottom".to_string(),
        s => s.to_string(),
    };
    minelib_world::registry::registry().by_key(&format!("slab:{mat}:{half}"))
}

/// Normale du côté `side` d'une case.
fn side_dir(side: u8) -> [i32; 3] {
    match side {
        0 => [1, 0, 0],
        1 => [-1, 0, 0],
        2 => [0, 1, 0],
        3 => [0, -1, 0],
        4 => [0, 0, 1],
        _ => [0, 0, -1],
    }
}

/// Où et quoi poser quand on pose la dalle `item` sur le bloc visé `hit` (`point` : point
/// touché) : `None` si `item` n'est pas une dalle.
/// - sur la moitié libre d'une dalle du même matériau, ou dans une case qui en contient
///   une : bloc complet ;
/// - debout, comme dans Minecraft : sur le dessus d'un bloc, dalle du bas ; au plafond,
///   dalle du haut ; contre un mur, du haut ou du bas selon la hauteur visée ;
/// - accroupi, toujours verticale (quatre positions) : contre un mur, collée au mur ; au
///   sol ou au plafond, du côté du bord le plus proche de l'endroit visé.
fn slab_placement(
    world: &World,
    hit: &Hit,
    item: BlockId,
    point: Vec3,
    sneaking: bool,
) -> Option<([i32; 3], BlockId)> {
    let (mat, _) = slab_parts(item)?;
    let reg = minelib_world::registry::registry();
    let full = reg
        .by_key(mat)
        .filter(|b| matches!(b.def().shape, minelib_world::registry::Shape::Cube));
    // Dalle visée du même matériau, cliquée sur sa face intérieure : on la complète.
    if let (Some((m, side)), Some(full)) = (slab_parts(hit.id), full) {
        let d = side_dir(side);
        if m == mat && hit.normal == [-d[0], -d[1], -d[2]] {
            return Some((hit.block, full));
        }
    }
    let p = hit.adjacent();
    let there = world.get(p[0], p[1], p[2]);
    if let (Some((m, _)), Some(full)) = (slab_parts(there), full) {
        if m == mat {
            return Some((p, full));
        }
    }
    if there.is_targetable() {
        return Some((p, there)); // case occupée : rien ne change
    }
    let side = if sneaking {
        match hit.normal {
            // Contre un mur : collée à la face visée (le mur est du côté −normale).
            [1, 0, 0] => 1,
            [-1, 0, 0] => 0,
            [0, 0, 1] => 5,
            [0, 0, -1] => 4,
            // Au sol ou au plafond : du côté du bord le plus proche du point visé.
            _ => {
                let (fx, fz) = (point.x - point.x.floor(), point.z - point.z.floor());
                [(1.0 - fx, 0u8), (fx, 1), (1.0 - fz, 4), (fz, 5)]
                    .into_iter()
                    .min_by(|a, b| a.0.total_cmp(&b.0))
                    .map_or(4, |e| e.1)
            }
        }
    } else {
        match hit.normal {
            [0, 1, 0] => 3,
            [0, -1, 0] => 2,
            _ if point.y - point.y.floor() > 0.5 => 2,
            _ => 3,
        }
    };
    Some((p, slab_of(mat, side).unwrap_or(item)))
}

/// Escalier à poser : la marche haute du côté où l'on regarde (le bas vers soi), à
/// l'envers posé au plafond ou sur la moitié haute d'un côté. `None` : pas un escalier.
fn stairs_placement(hit: &Hit, item: BlockId, look: Vec3, point: Vec3) -> Option<BlockId> {
    let key = &item.def().key;
    let rest = key.strip_prefix("stairs:")?;
    let mat = rest.split(':').next()?;
    let dir = if look.x.abs() >= look.z.abs() {
        if look.x >= 0.0 {
            0
        } else {
            1
        }
    } else if look.z >= 0.0 {
        2
    } else {
        3
    };
    let top = match hit.normal {
        [0, -1, 0] => true,
        [0, 1, 0] => false,
        _ => point.y - point.y.floor() > 0.5,
    };
    let half = if top { "top" } else { "bottom" };
    minelib_world::registry::registry().by_key(&format!("stairs:{mat}:{dir}:{half}"))
}

/// (v197) Tableaux de l'image, réutilisés (voir `Session::scratch`).
#[derive(Default)]
struct Scratch {
    players: Vec<minelib_render::PlayerInstance>,
    order: Vec<(f32, u32)>,
    /// Candidats aux ombres exactes : pieds, boîtes, et (v217) leur instance, pour la carte
    /// du soleil s'ils n'y ont pas de place.
    shadows: Vec<(Vec3, Vec<ShadowBox>, Option<minelib_render::PlayerInstance>)>,
    boxes: Vec<ShadowBox>,
    /// (v217) Joueurs de la carte d'ombre du soleil.
    sun_casters: Vec<minelib_render::PlayerInstance>,
    /// (v220) Joueurs de l'atlas des ombres des joueurs (lumières dynamiques).
    light_casters: Vec<minelib_render::PlayerInstance>,
}

/// (v197) Réponses de `sun_reaches` gardées par demi-bloc (voir `Session::sun_seen`).
struct SunSeen {
    sun: [f32; 3],
    since: std::time::Instant,
    seen: HashMap<[i32; 3], bool>,
}

impl Default for SunSeen {
    fn default() -> Self {
        SunSeen {
            sun: [0.0; 3],
            since: std::time::Instant::now(),
            seen: HashMap::new(),
        }
    }
}

/// (v236) Importance de l'ombre exacte d'un joueur (pieds en `feet`) pour la caméra en `cam`
/// qui regarde vers `look` (unitaire) : plus petit, plus important. Sa distance, comptée
/// une fois dans l'axe de vue, deux fois de côté, trois fois derrière.
fn shadow_view_score(cam: Vec3, look: Vec3, feet: Vec3) -> f32 {
    let to = feet + Vec3::Y * 0.9 - cam;
    let d = to.length();
    let c = if d > 1e-3 { to.dot(look) / d } else { 1.0 };
    d * (2.0 - c)
}

/// (v202) Distance (blocs) en deçà de laquelle un joueur près d'une lumière fait son ombre
/// même loin de la caméra (score de `light_shadow_score`).
const SHADOW_LIGHT_NEAR: f32 = 48.0;

/// (v202) Importance de l'ombre d'un joueur (pieds en `feet`) aux lumières `lights` (place,
/// portée), comparable à une distance à la caméra : plus il est près d'une lumière, plus son
/// ombre est grande (elle grandit comme l'inverse de la distance) : 4 fois sa distance à la
/// plus proche dont il est à portée ; infini sinon.
fn light_shadow_score(lights: &[(Vec3, f32)], feet: Vec3) -> f32 {
    let body = feet + Vec3::Y * 0.9;
    lights
        .iter()
        .filter_map(|&(c, r)| {
            let d = body.distance(c);
            (d < r).then_some(d * 4.0)
        })
        .fold(f32::INFINITY, f32::min)
}

/// (v202) Le point `p` est-il dans la boîte `b` (alignée sur les axes) agrandie de `margin` ?
/// (v290) Essais : MINELIB_CLIC=<s>[,<s>…] : un clic d'attaque à ces secondes de jeu (manger
/// L'Évidé qu'on vise, geste de la main, sans souris).
fn test_click(dt: f32) -> bool {
    static AT: std::sync::OnceLock<Vec<f32>> = std::sync::OnceLock::new();
    static CLOCK: std::sync::Mutex<f32> = std::sync::Mutex::new(0.0);
    let at = AT.get_or_init(|| {
        std::env::var("MINELIB_CLIC")
            .unwrap_or_default()
            .split(',')
            .filter_map(|v| v.trim().parse().ok())
            .collect()
    });
    if at.is_empty() {
        return false;
    }
    let Ok(mut clock) = CLOCK.lock() else {
        return false;
    };
    let before = *clock;
    *clock += dt;
    at.iter().any(|t| before < *t && *t <= *clock)
}

/// (v290) Essais : MINELIB_POSE=<moment du pas, rad>,<ampleur 0 à 1>[,1] : notre corps figé à
/// ce moment de la marche, sans avancer (captures des genoux et des coudes) ; « ,1 » : la
/// touche « s'accroupir » comptée tenue.
fn test_pose() -> Option<(f32, f32, bool)> {
    static POSE: std::sync::OnceLock<Option<(f32, f32, bool)>> = std::sync::OnceLock::new();
    *POSE.get_or_init(|| {
        let v = std::env::var("MINELIB_POSE").ok()?;
        let mut it = v.split(',').map(str::trim);
        let walk = it.next()?.parse().ok()?;
        let swing = it.next()?.parse().ok()?;
        Some((walk, swing, it.next() == Some("1")))
    })
}

/// (v303) Essais : MINELIB_ALLURE=<en l'air −1 à 1>,<direction en degrés>,<course 0 à 1> :
/// l'allure de notre modèle figée là (captures du saut, du pas de côté, de la course).
fn test_gait() -> Option<(f32, f32, f32)> {
    static GAIT: std::sync::OnceLock<Option<(f32, f32, f32)>> = std::sync::OnceLock::new();
    *GAIT.get_or_init(|| {
        let v = std::env::var("MINELIB_ALLURE").ok()?;
        let mut it = v.split(',').map(|x| x.trim().parse::<f32>());
        Some((it.next()?.ok()?, it.next()?.ok()?.to_radians(), it.next()?.ok()?))
    })
}

fn near_box(b: &ShadowBox, p: Vec3, margin: f32) -> bool {
    let (mn, mx) = (Vec3::from(b.min) - margin, Vec3::from(b.max) + margin);
    p.cmpge(mn).all() && p.cmple(mx).all()
}

/// (v237) Le point `p` est-il dans le morceau `b` lui-même (sa boîte exacte, penchée comme
/// lui ; 5 % de marge) ?
fn inside_box(b: &ShadowBox, p: Vec3) -> bool {
    b.to_unit.iter().all(|row| {
        let u = Vec3::new(row[0], row[1], row[2]).dot(p) + row[3];
        (-0.05..=1.05).contains(&u)
    })
}

/// (v217) Ce que vise la foudre lancée (`/thor`, Foudre de Zeus).
#[derive(Clone, Copy, Debug)]
enum Aim {
    /// Ce point du monde : elle y tombe.
    Ground(Vec3),
    /// Le ciel, dans cette direction : elle y monte et traverse les nuages.
    Sky(Vec3),
}

/// Le soleil (direction `sun`) atteint-il le milieu de la boîte ? La réponse de
/// `sun_reaches`, gardée par demi-bloc : oubliée quand le soleil a tourné ou au bout de
/// deux secondes (un bloc a pu être posé au-dessus).
fn sun_reaches_seen(seen: &mut SunSeen, world: &World, b: &ShadowBox, sun: [f32; 3]) -> bool {
    let moved = [0, 1, 2].iter().any(|&k| (seen.sun[k] - sun[k]).abs() > 1e-4);
    if moved || seen.since.elapsed().as_secs_f32() > 2.0 || seen.seen.len() > 8192 {
        seen.sun = sun;
        seen.since = std::time::Instant::now();
        seen.seen.clear();
    }
    let key = [0, 1, 2].map(|k| ((b.min[k] + b.max[k]) * 1.0).round() as i32);
    match seen.seen.get(&key) {
        Some(&v) => v,
        None => {
            let v = sun_reaches(world, b, sun);
            seen.seen.insert(key, v);
            v
        }
    }
}

/// Un autre joueur à dessiner : la construction en ligne, ou la foule d'essai.
struct Other<'a> {
    pseudo: &'a str,
    pos: Vec3,
    yaw: f32,
    pitch: f32,
    crouch: f32,
    /// (v194) Sa marche quand il la porte lui-même (la foule), sinon cherchée par son
    /// pseudo.
    walk: Option<&'a Walk>,
    /// (v197) Son skin, déjà trouvé (voir `Skins::of`).
    skin: Option<std::sync::Arc<crate::skin::Skin>>,
    /// (v191) Abri déjà connu (la foule le garde en mémoire), sinon cherché ici : voir
    /// `sky_light`.
    covered: Option<bool>,
    /// (v222) Ce qu'il tient en main (construction en ligne) : lampe torche, Foudre de Zeus.
    held: Option<minelib_net::poses::Held>,
    /// (v226) Temps qui reste du rouge d'un coup reçu (PNJ de la foule d'essai), en s.
    hurt: f32,
}

/// (v226) Lumière d'un PNJ qui vient de recevoir un coup (`hurt` secondes de rouge qui
/// restent) : il rougit, de moins en moins.
fn hurt_tint(light: [f32; 3], hurt: f32) -> [f32; 3] {
    let k = (hurt / crate::crowd::HURT_S).clamp(0.0, 1.0);
    if k <= 0.0 {
        return light;
    }
    let lum = light.iter().fold(0.0f32, |m, &c| m.max(c)).max(0.35);
    [
        light[0] + (lum * 1.3 - light[0]) * k,
        light[1] * (1.0 - 0.65 * k),
        light[2] * (1.0 - 0.65 * k),
    ]
}

/// Marche d'un autre joueur : même balancement que le sien en F5, d'après la vitesse
/// de son déplacement à l'écran (lissée : pas d'à-coups).
#[derive(Clone, Copy, Default)]
pub struct Walk {
    pub phase: f32,
    pub amp: f32,
    /// (v191) Image où on l'a vu pour la dernière fois (voir `walk_stamp`).
    stamp: u32,
    /// (v189) Sa pose à l'image d'avant (anticrénelage temporel : voir `PlayerInstance`).
    prev: crate::player_model::Prev,
    last: Vec3,
    speed: f32,
    /// (v303) Le reste de son allure (`player_model::Pose`) : en l'air, la direction de son
    /// déplacement par rapport à son regard, sa course.
    pub air: f32,
    pub dir: f32,
    pub run: f32,
}

impl Walk {
    pub fn new(pos: Vec3) -> Self {
        Walk {
            prev: crate::player_model::Prev {
                feet: pos,
                ..Default::default()
            },
            last: pos,
            ..Default::default()
        }
    }

    /// Sa pose à l'image d'avant, ou celle d'aujourd'hui s'il vient d'apparaître.
    fn prev_of(walks: &HashMap<String, Walk>, pseudo: &str, pose: &crate::player_model::Pose) -> crate::player_model::Prev {
        walks
            .get(pseudo)
            .map(|w| w.prev)
            .unwrap_or_else(|| crate::player_model::Prev::still(pose))
    }

    /// `airborne` : il saute ou tombe (pas en vol).
    pub fn update(&mut self, pos: Vec3, yaw: f32, pitch: f32, crouch: f32, airborne: bool, dt: f32) {
        let d = pos - self.last;
        self.prev = crate::player_model::Prev {
            feet: self.last,
            yaw,
            pitch,
            walk: self.phase,
            swing: self.amp,
            crouch,
            air: self.air,
            dir: self.dir,
            run: self.run,
        };
        self.last = pos;
        let v = if dt > 0.0 {
            Vec3::new(d.x, 0.0, d.z).length() / dt
        } else {
            0.0
        };
        // Téléportation (réapparition) : pas un pas de géant.
        let v = if v > 20.0 { 0.0 } else { v };
        self.speed += (v - self.speed) * (dt * 12.0).min(1.0);
        self.phase = (self.phase + stride_rate(self.speed) * dt) % std::f32::consts::TAU;
        let target = (self.speed / 4.3).min(1.0);
        self.amp += (target - self.amp) * (dt * 10.0).min(1.0);
        // (v303) Dans quelle direction il va par rapport à son regard, s'il court, s'il saute.
        if v > 0.3 {
            let to = crate::player_model::heading(Vec3::new(d.x, 0.0, d.z), yaw);
            self.dir = crate::player_model::turn_toward(self.dir, to, dt * 10.0);
        }
        let run = crate::player_model::run_of(self.speed);
        self.run += (run - self.run) * (dt * 8.0).min(1.0);
        let vy = if dt > 0.0 && d.y.abs() < 3.0 { d.y / dt } else { 0.0 };
        let air = crate::player_model::air_of(airborne, vy);
        self.air += (air - self.air) * (dt * 12.0).min(1.0);
    }
}

/// (v247) Cadence du balancement des bras et des jambes (radians par seconde) pour une
/// vitesse au sol (blocs/s), la même pour soi, les autres joueurs et les PNJ : 2,6 par bloc/s
/// jusqu'à la marche (4,4 blocs/s), plus au-delà. « Les mouvements des bras quand je cours
/// sont deux fois trop rapides, quand je tiens un objet aussi » : en courant (6,6 blocs/s),
/// 17,2 avant ; maintenant celle de la marche (11,4), des pas plus grands.
pub fn stride_rate(speed: f32) -> f32 {
    2.6 * speed.clamp(0.0, STRIDE_TOP)
}

/// (v303) En vol, le temps (s) de se mettre accroupi quand on descend, et d'en revenir.
const FLY_CROUCH_S: f32 = 0.15;

/// (v247) Vitesse (blocs/s) au-delà de laquelle la cadence ne monte plus : la marche.
const STRIDE_TOP: f32 = 4.4;

/// Le soleil (direction `sun`) atteint-il le milieu de la boîte ? Non si un bloc plein et
/// opaque est sur le chemin, à moins de 64 blocs (comme l'ombre du joueur dans le shader).
fn sun_reaches(world: &World, b: &ShadowBox, sun: [f32; 3]) -> bool {
    if sun[1] <= 0.0 {
        return false;
    }
    let c = [0, 1, 2].map(|k| (b.min[k] + b.max[k]) * 0.5);
    minelib_world::raycast::raycast_with(world, c, sun, 64.0, |id| {
        let d = id.def();
        d.full && !d.translucent
    })
    .is_none()
}

/// Recul de la caméra à la 3e personne (F5), en blocs, comme dans Minecraft.
const THIRD_PERSON_DIST: f32 = 4.0;

/// (v305) La coupe du corps par l'écran (`player_model::cut`) : à peine au-delà du plan proche
/// de l'image, où l'écran coupe ce qui est plus près.
const CUT_AT: f32 = minelib_render::NEAR * 1.04;

/// Scène de démonstration : un mur de briques et toutes les torches colorées devant.
/// (v198) Démo : un projecteur au-dessus du sol (`ground`) devant le miroir d'en face
/// (lumières de l'éditeur : sa tache se voit au sol et dans les miroirs).
fn demo_lights(lights: &mut crate::lights::Lights, ground: f64) {
    let id = lights.new_id();
    lights.scene.push(crate::lights::PlacedLight {
        id,
        name: "Démo".into(),
        kind: crate::lights::Kind::Spot,
        position: [8.5, ground + 5.5, 8.5],
        direction: [0.0, -1.0, 0.3],
        color: [1.0, 0.85, 0.6],
        intensity: 6.0,
        range: 14.0,
        angle: 30.0,
        volumetric: false,
        ..Default::default()
    });
}

fn build_demo(world: &mut World) {
    let z_wall = 14;
    let n = minelib_world::flame::CHEMICALS.len() as i32 + 1;
    let x0 = 8 - (n - 1) * 3 / 2;
    for i in 0..n {
        let x = x0 + i * 3;
        let ground = world.top_solid(x, z_wall - 2).unwrap_or(64);
        let chem = if i == 0 { None } else { Some((i - 1) as usize) };
        world.place_torch(x, ground + 1, z_wall - 2, chem);
    }
    for x in x0 - 3..=x0 + n * 3 {
        let ground = world.top_solid(x, z_wall).unwrap_or(64);
        for y in ground + 1..ground + 7 {
            world.set(x, y, z_wall, BlockId::BRICK);
        }
    }
    // Raytracing : terrain aplani autour du joueur, mur miroir à l'ouest, blocs de fer à l'est.
    let reg = minelib_world::registry::registry();
    if let (Some(mirror), Some(iron)) = (reg.by_key("mirror"), reg.by_key("iron_block")) {
        let g0 = world.top_solid(8, 6).unwrap_or(64);
        for z in 0..=12 {
            for x in 1..=15 {
                for y in g0 + 1..g0 + 8 {
                    world.set(x, y, z, BlockId::AIR);
                }
                world.set(x, g0, z, BlockId::GRASS);
            }
        }
        for z in 2..=10 {
            for y in g0 + 1..g0 + 5 {
                world.set(2, y, z, mirror);
            }
        }
        // Grotte fermée sous le sol (x 20..27, z 0..7) avec un miroir et une torche.
        for x in 19..=28 {
            for z in -1..=8 {
                for y in g0 - 8..=g0 - 2 {
                    let wall =
                        x == 19 || x == 28 || z == -1 || z == 8 || y == g0 - 8 || y == g0 - 2;
                    world.set(x, y, z, if wall { BlockId::STONE } else { BlockId::AIR });
                }
            }
        }
        for z in 1..=6 {
            for y in g0 - 7..g0 - 3 {
                world.set(20, y, z, mirror);
            }
        }
        world.place_torch(26, g0 - 7, 6, None);
        // Miroir parallèle en face : reflets à l'infini ; torche collée au premier miroir.
        for z in 2..=10 {
            for y in g0 + 1..g0 + 5 {
                world.set(12, y, z, mirror);
            }
        }
        world.place_torch(3, g0 + 1, 3, Some(4));
        // Second miroir, perpendiculaire : miroir dans le miroir.
        for x in 5..=10 {
            for y in g0 + 1..g0 + 4 {
                world.set(x, y, 11, mirror);
            }
        }
        for z in 3..=8 {
            world.set(14, g0 + 1, z, iron);
            world.set(14, g0 + 2, z, iron);
        }
        world.place_torch(6, g0 + 1, 9, Some(3));
        world.set(5, g0 + 1, 4, BlockId::BRICK);
        world.set(5, g0 + 2, 4, BlockId::BRICK);
        // Torche murale contre la colonne, torches colorées près du miroir et du fer.
        world.place_torch_block(6, g0 + 2, 4, BlockId::wall_torch(1), Some(5));
        world.place_torch(4, g0 + 1, 7, Some(0));
        world.place_torch(11, g0 + 1, 5, Some(8));
    }
}

/// Distance maximale (blocs) des sources de lumière envoyées au rendu.
const LIGHTS_RANGE: f32 = 40.0;

/// Centre de la flamme d'une torche (droite ou murale) posée en `p`.
/// Centre de la flamme d'une torche (droite ou murale) en `p`.
pub(crate) fn flame_center(id: BlockId, p: [i32; 3]) -> [f32; 3] {
    let local = match id.def().shape {
        minelib_world::registry::Shape::WallTorch(d) => {
            minelib_world::mesh::wall_torch_xf(d, [0.5, 0.656, 0.5])
        }
        _ => [0.5, 0.656, 0.5],
    };
    [
        p[0] as f32 + local[0],
        p[1] as f32 + local[1],
        p[2] as f32 + local[2],
    ]
}

/// Les sources de lumière les plus proches de `eye` (au plus `MAX_LIGHTS`).
pub fn collect_lights(world: &World, eye: Vec3) -> Vec<minelib_render::PointLight> {
    use minelib_render::{PointLight, MAX_LIGHTS};
    use minelib_world::LIGHT_RADIUS;
    let reach = LIGHT_RADIUS as f32;
    let near = |p: [f32; 3]| Vec3::from(p).distance(eye) < LIGHTS_RANGE;
    let mut out: Vec<PointLight> = Vec::new();
    let mut seen = HashSet::new();
    for (p, t) in world.torches() {
        let id = world.get(p[0], p[1], p[2]);
        let pos = flame_center(id, p);
        if !near(pos) {
            continue;
        }
        seen.insert(p);
        // Teinte conservée, canal le plus fort à 255 (comme l'éclairage des blocs).
        let c = world.torch_color(&t);
        let m = c.iter().copied().max().unwrap_or(1).max(1) as u32;
        out.push(PointLight {
            pos,
            size: 0.12,
            rgb: c.map(|v| (v as u32 * 255 / m) as u8),
            reach,
        });
    }
    let r = (LIGHTS_RANGE as i32 + CHUNK_SIZE as i32 - 1) / CHUNK_SIZE as i32;
    let c0 = ChunkPos::containing(eye.x.floor() as i32, eye.z.floor() as i32);
    for cz in c0.z - r..=c0.z + r {
        for cx in c0.x - r..=c0.x + r {
            let Some(ch) = world.chunk(ChunkPos { x: cx, z: cz }) else {
                continue;
            };
            let (ox, oz) = (cx * CHUNK_SIZE as i32, cz * CHUNK_SIZE as i32);
            for ([x, y, z], id) in ch.emitters() {
                let p = [ox + x as i32, y as i32, oz + z as i32];
                let Some(rgb) = id.def().light else { continue };
                if seen.contains(&p) {
                    continue;
                }
                let (pos, size) = if id.is_torch() {
                    (flame_center(id, p), 0.12)
                } else {
                    (
                        [p[0] as f32 + 0.5, p[1] as f32 + 0.5, p[2] as f32 + 0.5],
                        0.0,
                    )
                };
                if near(pos) {
                    out.push(PointLight {
                        pos,
                        size,
                        rgb,
                        reach,
                    });
                }
            }
        }
    }
    let d = |l: &PointLight| Vec3::from(l.pos).distance_squared(eye);
    out.sort_by(|a, b| d(a).total_cmp(&d(b)));
    out.truncate(MAX_LIGHTS);
    out
}

/// Plan de faces de miroir : axe, coordonnée, côté visible, étendue sur les deux autres axes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MirrorPlane {
    pub axis: u8,
    pub coord: i32,
    pub side: i32,
    pub lo: [i32; 2],
    pub hi: [i32; 2],
}

/// Images des sources de lumière derrière les miroirs qu'elles éclairent (les plus proches
/// du joueur d'abord).
pub fn mirror_lights(
    lights: &[minelib_render::PointLight],
    planes: &[MirrorPlane],
    eye: Vec3,
) -> Vec<minelib_render::MirrorLight> {
    use minelib_render::{MirrorLight, MAX_MIRROR_LIGHTS};
    let mut out = Vec::new();
    for l in lights {
        for p in planes {
            let ax = p.axis as usize;
            let (a, b) = ((ax + 1) % 3, (ax + 2) % 3);
            let dist = (l.pos[ax] - p.coord as f32) * p.side as f32;
            if dist <= 0.0 || dist >= l.reach {
                continue;
            }
            // Distance de la source au rectangle des faces de miroir.
            let da = (p.lo[0] as f32 - l.pos[a])
                .max(l.pos[a] - p.hi[0] as f32)
                .max(0.0);
            let db = (p.lo[1] as f32 - l.pos[b])
                .max(l.pos[b] - p.hi[1] as f32)
                .max(0.0);
            if (dist * dist + da * da + db * db).sqrt() >= l.reach {
                continue;
            }
            let mut pos = l.pos;
            pos[ax] = 2.0 * p.coord as f32 - l.pos[ax];
            out.push(MirrorLight {
                pos,
                rgb: l.rgb,
                reach: l.reach,
                axis: p.axis,
                coord: p.coord as f32,
                side: p.side as f32,
            });
        }
    }
    let d = |m: &MirrorLight| Vec3::from(m.pos).distance_squared(eye);
    out.sort_by(|x, y| d(x).total_cmp(&d(y)));
    out.truncate(MAX_MIRROR_LIGHTS);
    out
}

/// Lumière du ciel (0..15) dans les bits 12..15 de chaque case de la grille du raytracing :
/// 15 sous le ciel ouvert, puis −1 par case en se diffusant sous les toits et dans les
/// grottes, comme l'éclairage des blocs du monde.
fn sky_light_grid(ids: &mut [u16], solid: &[bool], covered: &[bool], [vx, vy, vz]: [i32; 3]) {
    let idx = |x: i32, y: i32, z: i32| (x + vx * (y + vy * z)) as usize;
    let mut sky = vec![0u8; ids.len()];
    let mut queue = std::collections::VecDeque::new();
    for z in 0..vz {
        for x in 0..vx {
            if covered[(x + vx * z) as usize] {
                continue;
            }
            for y in (0..vy).rev() {
                let i = idx(x, y, z);
                if solid[i] {
                    break;
                }
                sky[i] = 15;
                queue.push_back((x, y, z));
            }
        }
    }
    let dirs = [
        [1, 0, 0],
        [-1, 0, 0],
        [0, 1, 0],
        [0, -1, 0],
        [0, 0, 1],
        [0, 0, -1],
    ];
    while let Some((x, y, z)) = queue.pop_front() {
        let l = sky[idx(x, y, z)];
        if l <= 1 {
            continue;
        }
        for d in dirs {
            let (a, b, c) = (x + d[0], y + d[1], z + d[2]);
            if a < 0 || b < 0 || c < 0 || a >= vx || b >= vy || c >= vz {
                continue;
            }
            let j = idx(a, b, c);
            if !solid[j] && sky[j] < l - 1 {
                sky[j] = l - 1;
                queue.push_back((a, b, c));
            }
        }
    }
    for (v, s) in ids.iter_mut().zip(sky) {
        *v = (*v & 0x0fff) | (s as u16) << 12;
    }
}

/// Grille du raytracing calculée en arrière-plan.
struct VoxelGrid {
    origin: [i32; 3],
    ids: Vec<u16>,
    bricks: minelib_render::VoxelBricks,
    planes: Vec<MirrorPlane>,
    ms: f32,
}

/// Grille de blocs autour de `origin` (identifiants + lumière du ciel) et faces visibles
/// des miroirs, à partir des chunks donnés.
fn build_voxel_grid(
    chunks: &HashMap<ChunkPos, std::sync::Arc<minelib_world::Chunk>>,
    origin: [i32; 3],
) -> (Vec<u16>, Vec<MirrorPlane>) {
    use minelib_render::rt::{solid, traced, VOX};
    let [vx, vy, vz] = VOX.map(|v| v as i32);
    let mut ids = vec![0u16; (vx * vy * vz) as usize];
    let reg = minelib_world::registry::registry();
    let keep: Vec<bool> = reg.defs.iter().map(traced).collect();
    let opaque: Vec<bool> = reg.defs.iter().map(solid).collect();
    let is_mirror: Vec<bool> = reg.defs.iter().map(|d| d.reflect >= 15).collect();
    let full: Vec<bool> = reg.defs.iter().map(|d| d.full).collect();
    let mut mirrors = Vec::new();
    // Lumière du ciel dans la grille (comme le maillage) : blocs pleins et colonnes
    // couvertes au-dessus de la grille.
    let mut solid_cells = vec![false; ids.len()];
    let mut covered = vec![true; (vx * vz) as usize];
    let c0 = ChunkPos::containing(origin[0], origin[2]);
    let c1 = ChunkPos::containing(origin[0] + vx - 1, origin[2] + vz - 1);
    for cz in c0.z..=c1.z {
        for cx in c0.x..=c1.x {
            let Some(chunk) = chunks.get(&ChunkPos { x: cx, z: cz }) else {
                continue;
            };
            for lz in 0..CHUNK_SIZE as i32 {
                let z = cz * CHUNK_SIZE as i32 + lz - origin[2];
                if !(0..vz).contains(&z) {
                    continue;
                }
                for lx in 0..CHUNK_SIZE as i32 {
                    let x = cx * CHUNK_SIZE as i32 + lx - origin[0];
                    if !(0..vx).contains(&x) {
                        continue;
                    }
                    let above = (origin[1] + vy).max(0) as usize;
                    covered[(x + vx * z) as usize] = (above..chunk.top()).any(|wy| {
                        full.get(chunk.get(lx as usize, wy, lz as usize).0 as usize)
                            .copied()
                            .unwrap_or(false)
                    });
                    // Seulement les couches où le chunk a des blocs : les tranches vides
                    // (sous le plus bas bloc, au-dessus du plus haut) restent de l'air.
                    let ylo = (chunk.bottom() as i32 - origin[1]).max(0);
                    let yhi = (chunk.top() as i32 - origin[1]).min(vy);
                    for y in ylo..yhi {
                        let wy = origin[1] + y;
                        let id = chunk.get(lx as usize, wy as usize, lz as usize);
                        if full.get(id.0 as usize).copied().unwrap_or(false) {
                            solid_cells[(x + vx * (y + vy * z)) as usize] = true;
                        }
                        if id.0 != 0 && keep.get(id.0 as usize).copied().unwrap_or(false) {
                            // (v283) Numéro sur 12 bits : un bloc au-delà de 4096 (les
                            // dernières piles de tapis) y est un bloc de la même forme.
                            ids[(x + vx * (y + vy * z)) as usize] =
                                minelib_world::registry::rt_stand_in(id).0;
                            if is_mirror.get(id.0 as usize).copied().unwrap_or(false) {
                                mirrors.push([x, y, z]);
                            }
                        }
                    }
                }
            }
        }
    }
    sky_light_grid(&mut ids, &solid_cells, &covered, [vx, vy, vz]);
    // Faces de miroir non masquées, regroupées par plan.
    let at = |p: [i32; 3]| -> bool {
        if (0..3).any(|i| p[i] < 0 || p[i] >= VOX[i] as i32) {
            return false;
        }
        let id = ids[(p[0] + vx * (p[1] + vy * p[2])) as usize] & 0x0fff;
        id != 0 && opaque.get(id as usize).copied().unwrap_or(false)
    };
    let mut planes: Vec<MirrorPlane> = Vec::new();
    for m in mirrors {
        for axis in 0..3 {
            for side in [-1i32, 1] {
                let mut nb = m;
                nb[axis] += side;
                if at(nb) {
                    continue;
                }
                let coord = origin[axis] + m[axis] + (side + 1) / 2;
                let (a, b) = ((axis + 1) % 3, (axis + 2) % 3);
                let (u, v) = (origin[a] + m[a], origin[b] + m[b]);
                match planes
                    .iter_mut()
                    .find(|p| p.axis == axis as u8 && p.coord == coord && p.side == side)
                {
                    Some(p) => {
                        p.lo = [p.lo[0].min(u), p.lo[1].min(v)];
                        p.hi = [p.hi[0].max(u + 1), p.hi[1].max(v + 1)];
                    }
                    None => planes.push(MirrorPlane {
                        axis: axis as u8,
                        coord,
                        side,
                        lo: [u, v],
                        hi: [u + 1, v + 1],
                    }),
                }
            }
        }
    }
    (ids, planes)
}

/// Arêtes de la sélection WorldEdit (bleu), coin 1 (rouge) et coin 2 (bleu clair) : traits
/// fins dessinés avec la main (toujours visibles, comme l'affichage de WorldEdit).
/// (v258) `map` : les éléments de la carte FPS affichés (`/fps showelements`) ; une sélection
/// qui en est un en prend la couleur, sans ses coins.
fn selection_lines(
    we: &crate::worldedit::WorldEdit,
    map: Option<&minelib_net::game::FpsMap>,
    vp: Mat4,
    eye: Vec3,
) -> Vec<minelib_render::VmVertex> {
    let mut out = Vec::new();
    let f = |p: [i32; 3]| Vec3::new(p[0] as f32, p[1] as f32, p[2] as f32);
    if let Some((a, b)) = we.region() {
        let element = map.and_then(|m| element_color(m, minelib_net::game::Zone::new(a, b)));
        push_box(
            &mut out,
            vp,
            eye,
            f(a) - Vec3::splat(0.01),
            f(b) + Vec3::splat(1.01),
            element.unwrap_or([0.1, 0.45, 1.6, 1.0]),
        );
        if element.is_some() {
            return out;
        }
    }
    if let Some(p) = we.pos1 {
        push_box(
            &mut out,
            vp,
            eye,
            f(p) - Vec3::splat(0.02),
            f(p) + Vec3::splat(1.02),
            [1.6, 0.15, 0.1, 1.0],
        );
    }
    if let Some(p) = we.pos2 {
        push_box(
            &mut out,
            vp,
            eye,
            f(p) - Vec3::splat(0.02),
            f(p) + Vec3::splat(1.02),
            [0.3, 0.8, 1.8, 1.0],
        );
    }
    out
}

/// Un trait de `a` à `b` (couleur `c`), dessiné avec la main : toujours visible, à travers
/// les murs. Découpé en morceaux (épaisseur adaptée à la distance de chacun), et chaque
/// morceau coupé au ras de la caméra : quand on est dans la boîte, les arêtes qui passent
/// derrière nous restent visibles devant.
fn push_line(
    out: &mut Vec<minelib_render::VmVertex>,
    vp: Mat4,
    eye: Vec3,
    a: Vec3,
    b: Vec3,
    c: [f32; 4],
) {
    // Profondeur devant la caméra (w du clip, linéaire le long d'un segment).
    let depth = |p: Vec3| (vp * p.extend(1.0)).w;
    const NEAR: f32 = 0.06;
    let pieces = ((b - a).length() / 1.5).ceil().max(1.0) as usize;
    for k in 0..pieces {
        let mut p0 = a.lerp(b, k as f32 / pieces as f32);
        let mut p1 = a.lerp(b, (k + 1) as f32 / pieces as f32);
        let (w0, w1) = (depth(p0), depth(p1));
        if w0 < NEAR && w1 < NEAR {
            continue;
        }
        if w0 < NEAR {
            p0 = p0.lerp(p1, (NEAR - w0) / (w1 - w0));
        } else if w1 < NEAR {
            p1 = p1.lerp(p0, (NEAR - w1) / (w0 - w1));
        }
        let mid = (p0 + p1) * 0.5;
        let w = (p1 - p0).cross(mid - eye).normalize_or_zero()
            * (0.012 * (mid - eye).length().max(1.0)).min(0.08);
        for p in [p0 - w, p1 - w, p1 + w, p1 + w, p0 + w, p0 - w] {
            let q = vp * p.extend(1.0);
            out.push(minelib_render::VmVertex {
                clip: [q.x, q.y, q.z, q.w.max(1e-3)],
                uv: [0.0, 0.0],
                layer: minelib_render::NO_TEXTURE,
                _pad: 0,
                color: c,
            });
        }
    }
}

/// Les 12 arêtes de la boîte de coins `lo` et `hi` (voir `push_line`).
fn push_box(
    out: &mut Vec<minelib_render::VmVertex>,
    vp: Mat4,
    eye: Vec3,
    lo: Vec3,
    hi: Vec3,
    c: [f32; 4],
) {
    let v = |x: bool, y: bool, z: bool| {
        Vec3::new(
            if x { hi.x } else { lo.x },
            if y { hi.y } else { lo.y },
            if z { hi.z } else { lo.z },
        )
    };
    for a in [false, true] {
        for b in [false, true] {
            push_line(out, vp, eye, v(false, a, b), v(true, a, b), c);
            push_line(out, vp, eye, v(a, false, b), v(a, true, b), c);
            push_line(out, vp, eye, v(a, b, false), v(a, b, true), c);
        }
    }
}

/// (v255) Couleurs de `/fps showelements` (au-delà de 1 : elles brillent, comme la sélection
/// de la baguette) : porte simple vert clair, double porte vert foncé, sites rouges, départ
/// CT bleu, départ T jaune.
const ELEMENT_DOOR: [f32; 4] = [0.45, 1.7, 0.45, 1.0];
const ELEMENT_DOUBLE_DOOR: [f32; 4] = [0.0, 0.28, 0.03, 1.0];
const ELEMENT_SITE: [f32; 4] = [1.7, 0.08, 0.08, 1.0];
const ELEMENT_CT: [f32; 4] = [0.15, 0.4, 1.9, 1.0];
const ELEMENT_T: [f32; 4] = [1.7, 1.45, 0.05, 1.0];

/// (v255) Une porte vue par `/fps showelements` : sa boîte (coins inclus, deux blocs de haut ;
/// les deux battants pour une double porte).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DoorBox {
    pub min: [i32; 3],
    pub max: [i32; 3],
    pub double: bool,
}

/// (v255) Portes du chunk `pos` : le bas de chaque porte, une boîte par porte simple et une
/// pour les deux battants d'une double porte (rangée avec le battant le plus à l'ouest, puis
/// au nord). Seulement les couches où le chunk a des blocs.
pub(crate) fn chunk_doors(world: &World, pos: ChunkPos) -> Vec<DoorBox> {
    let Some(chunk) = world.chunk(pos) else {
        return Vec::new();
    };
    let get = |c: [i32; 3]| world.get(c[0], c[1], c[2]);
    let none = HashMap::new();
    let (x0, z0) = (pos.x * CHUNK_SIZE as i32, pos.z * CHUNK_SIZE as i32);
    let mut out = Vec::new();
    for y in chunk.bottom()..chunk.top() {
        for z in 0..CHUNK_SIZE {
            for x in 0..CHUNK_SIZE {
                let id = chunk.get(x, y, z);
                if !matches!(hinged(id), Some(Hinged::Door { upper: false, .. })) {
                    continue;
                }
                let c = [x0 + x as i32, y as i32, z0 + z as i32];
                // L'autre battant d'une double porte la range (une seule boîte pour les deux).
                if door_partner(&get, &none, c).is_some_and(|q| (q[0], q[2]) < (c[0], c[2])) {
                    continue;
                }
                if let Some(d) = door_box(&get, c) {
                    out.push(d);
                }
            }
        }
    }
    out
}

/// (v258) Boîte de la porte dont fait partie le bloc `c` : ses deux blocs, ou les quatre des
/// deux battants d'une double porte. `None` : pas une porte.
pub(crate) fn door_box(get: &dyn Fn([i32; 3]) -> BlockId, c: [i32; 3]) -> Option<DoorBox> {
    let upper = match hinged(get(c))? {
        Hinged::Door { upper, .. } => upper,
        Hinged::Trapdoor { .. } => return None,
    };
    let low = if upper { [c[0], c[1] - 1, c[2]] } else { c };
    let none = HashMap::new();
    Some(match door_partner(get, &none, low) {
        Some(q) => DoorBox {
            min: [low[0].min(q[0]), low[1], low[2].min(q[2])],
            max: [low[0].max(q[0]), low[1] + 1, low[2].max(q[2])],
            double: true,
        },
        None => DoorBox {
            min: low,
            max: [low[0], low[1] + 1, low[2]],
            double: false,
        },
    })
}

/// (v255) Ce que montre `/fps showelements` : les zones de la carte FPS (relues toutes les
/// secondes : un `/fps set` se voit tout de suite) et les portes des chunks chargés, cherchées
/// quelques chunks par image (les plus proches d'abord), puis de nouveau en boucle (une porte
/// posée ou cassée se voit au tour suivant).
#[derive(Default)]
pub(crate) struct FpsElements {
    pub map: minelib_net::game::FpsMap,
    pub map_age: f32,
    pub doors: HashMap<ChunkPos, Vec<DoorBox>>,
    pub queue: Vec<ChunkPos>,
}

/// (v255) Chunks fouillés par image pour les portes de `/fps showelements`.
const ELEMENT_CHUNKS_PER_FRAME: usize = 3;

/// (v255) Zones posées de la carte FPS, avec leur nom et leur couleur.
fn element_zones(
    map: &minelib_net::game::FpsMap,
) -> Vec<(minelib_net::game::Zone, String, [f32; 4])> {
    let mut out = Vec::new();
    if let Some(z) = map.ct {
        out.push((z, "Départ CT".to_string(), ELEMENT_CT));
    }
    if let Some(z) = map.t {
        out.push((z, "Départ T".to_string(), ELEMENT_T));
    }
    for (k, z) in map.sites.iter().enumerate() {
        if let Some(z) = z {
            out.push((
                *z,
                format!("Site {}", crate::fpsmap::SITE_NAMES[k]),
                ELEMENT_SITE,
            ));
        }
    }
    out
}

/// (v258) Couleur de l'élément de la carte FPS qu'est exactement la zone `z` (départ, site, ou
/// porte posée pour la partie), s'il y en a un.
fn element_color(
    map: &minelib_net::game::FpsMap,
    z: minelib_net::game::Zone,
) -> Option<[f32; 4]> {
    if let Some((_, _, c)) = element_zones(map).into_iter().find(|(e, _, _)| *e == z) {
        return Some(c);
    }
    map.door_list.contains(&z).then(|| door_color(z))
}

/// (v258) Couleur d'une porte posée pour la partie : vert clair, vert foncé pour une double
/// porte (deux battants côte à côte).
fn door_color(z: minelib_net::game::Zone) -> [f32; 4] {
    if z.max[0] > z.min[0] || z.max[2] > z.min[2] {
        ELEMENT_DOUBLE_DOOR
    } else {
        ELEMENT_DOOR
    }
}

/// (v255) Traits de `/fps showelements` (voir `push_box`) : la boîte de chaque zone de la
/// carte, et de chaque porte (à 192 blocs au plus). (v258) Les portes posées pour la partie
/// (`/fps set door`) ; toutes celles trouvées dans les chunks quand toutes s'ouvrent
/// (`/fps set door on`).
fn element_lines(e: &FpsElements, vp: Mat4, eye: Vec3) -> Vec<minelib_render::VmVertex> {
    let mut out = Vec::new();
    let f = |p: [i32; 3]| Vec3::new(p[0] as f32, p[1] as f32, p[2] as f32);
    for (z, _, c) in element_zones(&e.map) {
        push_box(
            &mut out,
            vp,
            eye,
            f(z.min) - Vec3::splat(0.015),
            f(z.max) + Vec3::splat(1.015),
            c,
        );
    }
    for z in &e.map.door_list {
        let (lo, hi) = (f(z.min), f(z.max) + Vec3::ONE);
        if ((lo + hi) * 0.5 - eye).length() > 192.0 {
            continue;
        }
        push_box(
            &mut out,
            vp,
            eye,
            lo - Vec3::splat(0.03),
            hi + Vec3::splat(0.03),
            door_color(*z),
        );
    }
    let all = e.map.doors();
    for d in e.doors.values().flatten().filter(|_| all) {
        if e.map.door_listed(d.min) {
            continue;
        }
        let (lo, hi) = (f(d.min), f(d.max) + Vec3::ONE);
        if ((lo + hi) * 0.5 - eye).length() > 192.0 {
            continue;
        }
        let c = if d.double {
            ELEMENT_DOUBLE_DOOR
        } else {
            ELEMENT_DOOR
        };
        push_box(
            &mut out,
            vp,
            eye,
            lo - Vec3::splat(0.03),
            hi + Vec3::splat(0.03),
            c,
        );
    }
    out
}

/// Portée du chat vocal de proximité (construction), en blocs.
const VOICE_RANGE: f32 = 48.0;
/// Deux appuis sur « avancer » à moins de ce temps (s) l'un de l'autre font courir (sept
/// ticks de Minecraft).
const SPRINT_TAP_S: f32 = 0.35;
/// Auditeurs d'un message vocal en construction, au plus (les plus proches).
const VOICE_LISTENERS: usize = 256;

/// Cadre du tableau des scores : (x, y, largeur).
fn scoreboard_frame(size: (f32, f32)) -> (f32, f32, f32) {
    let s = (size.1 / 360.0).floor().max(1.0);
    let w = (420.0 * s).min(size.0 - 80.0 * s);
    ((size.0 - w) / 2.0 - 35.0 * s, size.1 * 0.12, w)
}

/// Lignes des joueurs du tableau des scores et leur hauteur à l'écran (même ordre que
/// le dessin : CT puis T).
fn scoreboard_lines(
    size: (f32, f32),
    rows: &[minelib_net::game::ScoreRow],
) -> Vec<(minelib_net::game::ScoreRow, f32)> {
    use minelib_net::game::{CT, T};
    let s = (size.1 / 360.0).floor().max(1.0);
    let row = 11.0 * s;
    let (_, mut y, _) = scoreboard_frame(size);
    y += 30.0 * s;
    let mut out = Vec::new();
    for team in [CT, T] {
        y += row;
        for r in rows.iter().filter(|r| r.team == team) {
            out.push((r.clone(), y));
            y += row;
        }
        y += 4.0 * s;
    }
    out
}

/// Couleur d'un coéquipier (comme dans CS : vert, jaune, orange, bleu, violet, puis cinq
/// de plus pour les cartes à 10 contre 10).
pub fn mate_color(k: usize) -> [f32; 4] {
    let c = &crate::multiplayer::PLAYER_COLORS;
    c[k % c.len()]
}

/// Joueurs au plus d'une carte FPS.
fn proto_max() -> u8 {
    minelib_net::proto::FPS_MAX_PLAYERS as u8
}

/// Petites images du fil des éliminations (7 × 7 points, dessinées pour Minelib).
const ICON_HEAD: [&str; 7] = [
    "..###..", ".#####.", ".#.#.#.", ".#####.", "..###..", "..#.#..", ".......",
];
const ICON_WALL: [&str; 7] = [
    "#######", "#..#..#", "#######", ".#..#..", "#######", "#..#..#", "#######",
];
const ICON_SMOKE: [&str; 7] = [
    "..##...", ".####.#", "######.", "#######", ".#####.", ".......", ".......",
];
const ICON_BLIND: [&str; 7] = [
    "......#", ".###.#.", "#..##.#", "#.#.#.#", "#.##..#", ".#.###.", "#......",
];

fn draw_icon(ui: &mut Ui, x: f32, y: f32, s: f32, icon: &[&str; 7], color: [f32; 4]) {
    let px = 9.0 * s / 7.0;
    for (j, row) in icon.iter().enumerate() {
        for (i, c) in row.bytes().enumerate() {
            if c == b'#' {
                ui.rect(x + i as f32 * px, y + j as f32 * px, px, px, color);
            }
        }
    }
}

/// Couleur d'une équipe (CT bleu, T orangé).
fn team_color(team: u8) -> [f32; 4] {
    if team == minelib_net::game::CT {
        [0.45, 0.65, 1.0, 1.0]
    } else {
        [1.0, 0.72, 0.3, 1.0]
    }
}

/// Côté qu'occupait l'équipe de départ `start` à la manche `i` (0 = première) : les
/// équipes changent de côté après la manche 12, puis toutes les 3 manches en
/// prolongations.
fn side_of_start(start: u8, i: u32) -> u8 {
    let swaps = (1..=i)
        .filter(|&n| n == 12 || (n >= 24 && (n - 24) % 3 == 0))
        .count();
    if swaps % 2 == 0 {
        start
    } else {
        1 - start
    }
}

#[cfg(test)]
mod loading_tests {
    use super::*;

    /// (v283) RÈGLE DE VIENDAX : « je suis encore en jeu alors que le jeu n'a pas fini d'être
    /// chargé : ça ne doit pas être possible ; fais en sorte que dans le code on ne puisse pas
    /// changer ça ». On n'entre qu'une fois chaque chunk de toute la distance de rendu affiché,
    /// les shaders prêts et (en ligne) la météo du serveur arrivée, même une heure après ; et
    /// le code n'a qu'une porte : si quelqu'un ajoute une sortie au bout d'un temps, ferme
    /// l'écran de chargement ailleurs ou ne compte qu'une partie de la distance de rendu, cet
    /// essai échoue.
    #[test]
    fn nothing_is_entered_before_everything_is_loaded() {
        let long_ago = std::time::Instant::now() - std::time::Duration::from_secs(3600);
        let l = |shown, all, shaders| Loading {
            since: long_ago,
            shown: (shown, all),
            shaders,
            bar: 0.0,
        };
        assert!(!loading_done(&l(4224, 4225, true), true), "un chunk manque");
        assert!(!loading_done(&l(4225, 4225, false), true), "shaders pas prêts");
        assert!(!loading_done(&l(4225, 4225, true), false), "météo du serveur pas arrivée");
        assert!(!loading_done(&l(0, 0, true), true));
        assert!(loading_done(&l(4225, 4225, true), true));
        let fresh = Loading {
            since: std::time::Instant::now(),
            shown: (81, 81),
            shaders: true,
            bar: 0.0,
        };
        assert!(!loading_done(&fresh, true), "l'écran se montre au moins LOAD_MIN_S");
        // Une seule porte, dans `check_loaded`, qui passe par `loading_done` et compte toute
        // la distance de rendu.
        let src = include_str!("session.rs");
        let close = concat!("self.loading", " = None");
        assert_eq!(src.matches(close).count(), 1, "l'écran de chargement se ferme ailleurs");
        assert!(!src.contains(concat!("self.loading", ".take()")));
        assert!(!src.contains(concat!("LOAD_", "MAX_S")), "une durée maximale est revenue");
        let check = &src[src.find("fn check_loaded(").unwrap()..];
        let check = &check[..check.find("\n    }\n").unwrap()];
        assert!(check.contains(close) && check.contains("loading_done(l, weather)"));
        assert!(check.contains("shown_within(p.x, p.z, self.streamer.radius)"));
        let gate = &src[src.find("fn loading_done(").unwrap()..];
        let gate = &gate[..gate.find("\n}\n").unwrap()];
        assert!(!gate.contains("||"), "une autre façon d'entrer : {gate}");
    }

    /// (v295) « La barre de chargement quand on rejoint un monde : le temps ne correspond pas
    /// au temps qu'on attend ». Son journal de 20 h 01 : monde ouvert 45 s après le lancement
    /// du jeu, ses 806 chunks affichés en une vingtaine de secondes, entrée 49 s après, à la
    /// fin de la compilation des shaders du monde (66 s depuis le lancement). La barre doit
    /// avancer au rythme de toute l'attente : pas pleine quand les chunks le sont.
    #[test]
    fn the_loading_bar_follows_the_whole_wait() {
        // Toute l'attente, image par image (une toutes les 0,1 s) : les chunks arrivent à 40
        // par seconde ; la compilation des shaders du monde, lancée 17 s avant l'ouverture du
        // monde, prend 66,2 s (comme aux lancements précédents) : elle finit 49,2 s après.
        let (all, done_at) = (806usize, 49.2f32);
        let mut bar = 0.0f32;
        let mut t = 0.0f32;
        while t < done_at {
            let shown = ((t * 40.0) as usize).min(all);
            let shaders_left = minelib_render::compile_time::left(t + 17.0, 66.2);
            let left = load_left(t, (shown, all), shaders_left, true);
            let b = load_bar(t, left);
            assert!(b < 1.0, "pleine avant l'entrée, à {t} s");
            bar = bar.max(b);
            // Avant : les chunks seuls (pleine dès 20 s).
            let before = shown as f32 / all as f32;
            if t > 21.0 {
                assert_eq!(before, 1.0);
                assert!(bar < 0.99 && bar < t / done_at + 0.05, "{t} s : barre {bar}");
            }
            // La barre suit le temps : à ± 8 % près de la part de l'attente déjà faite.
            if t > 2.0 {
                assert!((bar - t / done_at).abs() < 0.08, "{t} s : barre {bar}");
            }
            t += 0.1;
        }
        // Shaders déjà prêts (monde rouvert) : la barre suit les chunks, comme avant.
        let left = load_left(5.0, (400, 800), 0.0, true);
        assert!((load_bar(5.0, left) - 0.5).abs() < 0.06);
        // Rien n'arrive (chunks à 0, longtemps) : la barre ne se remplit pas.
        let left = load_left(60.0, (0, 800), 0.0, true);
        assert!(load_bar(60.0, left) < 0.1);
        // En ligne, la météo du serveur pas encore là : pas pleine non plus.
        assert!(load_left(10.0, (800, 800), 0.0, false) > 0.0);
    }
}

#[cfg(test)]
mod far_origin_tests {
    use super::*;

    /// (v322) L'origine d'une partie qui s'ouvre : imposée (le passage en mode FPS la rouvre
    /// depuis l'origine de la partie FPS) ; celle de la carte dans une partie FPS en ligne (le
    /// centre du monde pour une carte sans zones, et toutes les cartes d'avant) ; près du joueur
    /// ailleurs : en solo (mode FPS compris) et en construction en ligne (la copie du monde d'un
    /// serveur chez un invité dit « fps » : v318).
    #[test]
    fn a_game_opens_from_the_right_origin() {
        let cfg = |far_origin, origin| SessionConfig {
            view_distance: 4,
            far_origin,
            origin,
            terrain: None,
        };
        let player = [1.0e9, 70.0, -5.0e8];
        let near_player = origin_near(player);
        assert_ne!(near_player, ChunkPos { x: 0, z: 0 });
        let map_origin = ChunkPos { x: 62_500, z: 4 };
        let map = Some(map_origin);
        let forced = ChunkPos { x: 7, z: -3 };
        assert_eq!(opening_origin(&cfg(true, Some(forced)), map, player), forced);
        assert_eq!(opening_origin(&cfg(true, Some(forced)), None, player), forced);
        // Partie FPS en ligne : la carte, sinon le centre du monde.
        assert_eq!(opening_origin(&cfg(false, None), map, player), map_origin);
        assert_eq!(
            opening_origin(&cfg(false, None), None, player),
            ChunkPos { x: 0, z: 0 }
        );
        // Solo (mode FPS compris) et construction en ligne : près du joueur, carte FPS ou pas.
        assert_eq!(opening_origin(&cfg(true, None), map, player), near_player);
        assert_eq!(opening_origin(&cfg(true, None), None, player), near_player);
    }

    /// (v309) L'origine des coordonnées d'une partie solo : le centre du monde à moins de
    /// 32 768 blocs (rien ne change pour les mondes d'avant), sinon le multiple de 4 096 blocs
    /// le plus proche du joueur, sur chaque axe. Rouverte depuis elle, la partie a le joueur à
    /// moins de 2 048 blocs de son origine : bien en deçà du seuil où elle repart d'une autre
    /// (pas d'aller-retour).
    #[test]
    fn a_far_game_counts_from_an_origin_close_to_the_player() {
        let c = |x: i32, z: i32| ChunkPos { x, z };
        assert_eq!(origin_near([0.0, 70.0, 0.0]), c(0, 0));
        assert_eq!(origin_near([32_767.9, 70.0, -32_767.9]), c(0, 0));
        assert_eq!(origin_near([32_768.0, 70.0, 0.0]), c(2_048, 0));
        assert_eq!(
            origin_near([16_777_216.5, 70.0, -16_777_216.5]),
            c(1_048_576, -1_048_576)
        );
        assert_eq!(origin_near([16_777_216.0 + 2_047.0, 0.0, 0.0]).x, 1_048_576);
        assert_eq!(origin_near([16_777_216.0 + 2_049.0, 0.0, 0.0]).x, 1_048_576 + 256);
        assert_eq!(origin_near([f64::NAN, 0.0, 1e300]).x, 0);
        assert!(ORIGIN_STEP / 2.0 < REBASE_AT as f64);
        for a in [33_000.0, 16_777_216.7, -9_999_999.25, 123_456_789.0] {
            let o = origin_near([a, 0.0, a]);
            let local = a - (o.x as f64) * CHUNK_SIZE as f64;
            assert!(local.abs() <= ORIGIN_STEP / 2.0, "{a} : {local}");
        }
    }
}

#[cfg(test)]
mod f5_camera_tests {
    use super::*;

    /// Le point `p` est-il dans le pavé `b` (`player_model::solid_boxes`), à `margin` blocs au
    /// moins de ses faces (moins : un peu au-dehors) ?
    fn in_box(b: &(Mat4, [f32; 3], [f32; 3], usize), p: Vec3, margin: f32) -> bool {
        let q = b.0.transform_point3(p);
        let m = margin / crate::player_model::PX_BLOCKS;
        (0..3).all(|k| q[k] >= b.1[k] + m && q[k] <= b.2[k] - m)
    }

    /// La couleur `c` (linéaire, ombrée) est-elle celle d'un pixel du skin par défaut ?
    fn skin_like(c: [f32; 3], colors: &[[f32; 3]]) -> bool {
        let n = |c: [f32; 3]| {
            let m = c[0].max(c[1]).max(c[2]).max(1e-6);
            c.map(|v| v / m)
        };
        colors
            .iter()
            .any(|&k| (0..3).all(|i| (n(c)[i] - n(k)[i]).abs() < 2e-3))
    }

    /// Le point de l'écran (`x`, `y` : tangentes du rayon vers la droite et vers le haut) est-il
    /// couvert par un triangle de `tris` vu de la caméra (`cam`, repère `ahead`, `right`, `up`) ?
    fn covered(
        tris: &[minelib_render::EntityVertex],
        cam: Vec3,
        [ahead, right, up]: [Vec3; 3],
        (x, y): (f32, f32),
    ) -> bool {
        let screen = |v: &minelib_render::EntityVertex| {
            let q = Vec3::from(v.pos) - cam;
            let f = q.dot(ahead);
            (q.dot(right) / f, q.dot(up) / f)
        };
        tris.chunks(3).any(|t| {
            let [a, b, c] = [screen(&t[0]), screen(&t[1]), screen(&t[2])];
            let cross = |p: (f32, f32), q: (f32, f32), r: (f32, f32)| {
                (q.0 - p.0) * (r.1 - p.1) - (q.1 - p.1) * (r.0 - p.0)
            };
            let area = cross(a, b, c);
            if area.abs() < 1e-12 {
                return false;
            }
            let w = [cross(b, c, (x, y)), cross(c, a, (x, y)), cross(a, b, (x, y))];
            w.iter().all(|&w| w / area >= -1e-4)
        })
    }

    /// (v305) « Je dois pouvoir descendre ma caméra jusqu'au sol en F5, c'est les parties du
    /// skin visibles que tu dois régler, pas bloquer la vue à une hauteur » ; « je verrais son
    /// corps mais pas l'intérieur : l'intérieur visible, tu le textures de la texture visible
    /// de sa jambe au niveau de la caméra ». Sur un sol plat, debout ou accroupi, immobile ou
    /// en marchant, de dos comme de face, quel que soit le regard : la caméra F5 est à l'opposé
    /// du regard de dos, dans son sens de face, à 4 blocs ou ramenée par le sol (aucune limite :
    /// regard droit vers le bas de face, ou vers le haut de dos, elle descend jusqu'au sol, dans
    /// les jambes) ; tout endroit de l'écran d'où elle verrait l'intérieur d'un morceau du corps
    /// (le plan de la coupe y est dans le morceau) est couvert par la coupe pleine, aux couleurs
    /// du skin ; la coupe est tout entière dans le corps (cachée vu d'ailleurs), et rien quand
    /// la caméra est loin de lui.
    #[test]
    fn the_third_person_camera_goes_down_to_the_ground_without_showing_the_inside_of_the_body() {
        let feet = Vec3::new(8.5, 4.0, 8.5);
        let colors = crate::player_model::default_colors();
        // Champ de vision de 70° en hauteur, image en 16/9.
        let (tan_h, aspect) = (35f32.to_radians().tan(), 16.0 / 9.0);
        let (mut checked, mut cut_seen, mut inside_seen, mut grounded) = (0, 0, 0, 0);
        for third in [1u8, 2] {
            for yaw_deg in [0.0f32, 37.0, 90.0, 200.0] {
                for k in -23..=23 {
                    let pitch = (k as f32 * 4.0).clamp(-89.0, 89.0).to_radians();
                    let yaw = yaw_deg.to_radians();
                    for (crouch, walk, swing) in [
                        (0.0, 0.0, 0.0),
                        (1.0, 0.0, 0.0),
                        (0.0, 1.5, 1.0),
                        (0.0, -1.5, 1.0),
                        (0.0, 0.7, 1.0),
                        (1.0, 1.5, 1.0),
                        (1.0, -1.5, 1.0),
                    ] {
                        let eye = feet
                            + Vec3::Y
                                * (crate::player::EYE - crate::player::CROUCH_DROP * crouch);
                        let (sy, cy) = yaw.sin_cos();
                        let (sp, cp) = pitch.sin_cos();
                        let look = Vec3::new(sy * cp, sp, -cy * cp);
                        // Comme `view_camera` : à l'opposé du regard de dos, dans son sens de
                        // face, sans limite.
                        let back = if third == 1 { -look } else { look };
                        // Le sol (herbe sous les pieds) ramène la caméra comme `raycast`.
                        let ground = (eye.y - feet.y) / -back.y;
                        let dist = if back.y < 0.0 && ground < THIRD_PERSON_DIST {
                            (ground - 0.25).max(0.3)
                        } else {
                            THIRD_PERSON_DIST
                        };
                        let cam = eye + back * dist;
                        if cam.y - feet.y < 0.3 {
                            grounded += 1;
                        }
                        let pose = crate::player_model::Pose {
                            feet,
                            yaw,
                            pitch,
                            walk,
                            swing,
                            crouch,
                            light: [1.0; 3],
                            held: None,
                            skin: None,
                            bow: 0.0,
                            air: 0.0,
                            dir: 0.0,
                            run: 0.0,
                        };
                        let ahead = -back;
                        let cut = crate::player_model::cut(&pose, None, cam, ahead, CUT_AT);
                        assert_eq!(cut.len() % 3, 0);
                        let what = format!(
                            "F5 {third}, cap {yaw_deg}°, regard {:.0}°, accroupi {crouch}, \
                             marche {walk} : caméra en {cam}",
                            pitch.to_degrees()
                        );
                        if dist == THIRD_PERSON_DIST {
                            assert!(cut.is_empty(), "{what} : coupe loin du corps");
                        }
                        let boxes = crate::player_model::solid_boxes(&pose);
                        for v in &cut {
                            assert!(
                                boxes.iter().any(|b| in_box(b, Vec3::from(v.pos), -1e-4)),
                                "{what} : coupe hors du corps en {:?}",
                                v.pos
                            );
                            assert!(skin_like(v.color, &colors), "{what} : {:?}", v.color);
                        }
                        // Les endroits de l'écran (13 × 9) où le plan de la coupe est dans le
                        // corps : couverts.
                        let right = ahead.cross(Vec3::Y).normalize();
                        let up = right.cross(ahead);
                        for a in -6..=6 {
                            for b in -4..=4 {
                                let x = a as f32 / 6.0 * tan_h * aspect;
                                let y = b as f32 / 4.0 * tan_h;
                                let p = cam + (ahead + right * x + up * y) * CUT_AT;
                                if !boxes.iter().any(|bx| in_box(bx, p, 2e-3)) {
                                    continue;
                                }
                                inside_seen += 1;
                                assert!(
                                    covered(&cut, cam, [ahead, right, up], (x, y)),
                                    "{what} : l'intérieur du corps se voit en ({a}, {b})"
                                );
                            }
                        }
                        if !cut.is_empty() {
                            cut_seen += 1;
                        }
                        checked += 1;
                    }
                }
            }
        }
        assert!(checked > 2500);
        // La caméra descend jusqu'au sol (regard droit vers le bas de face, vers le haut de
        // dos), et l'écran y coupe bien le corps : l'essai voit le cas de la capture.
        assert!(grounded >= 2 * 4 * 7, "{grounded}");
        assert!(cut_seen > 0 && inside_seen > 0, "{cut_seen} {inside_seen}");
    }
}

#[cfg(test)]
mod zeus_click_tests {
    use super::*;

    /// (v295) « J'ai réussi à casser un bloc avec l'éclair, ça doit être impossible » : avec la
    /// Foudre de Zeus en main, un clic bref (appuyé et relâché dans la même image) est pour
    /// elle comme un clic tenu ; et rien ne se casse avec elle en main.
    #[test]
    fn the_legendary_bolt_never_breaks_a_block() {
        let zeus = Some(Item::Zeus);
        assert!(zeus_click(zeus, true, false), "clic bref, dans une seule image");
        assert!(zeus_click(zeus, true, true));
        assert!(zeus_click(zeus, false, true), "clic tenu");
        assert!(!zeus_click(zeus, false, false));
        assert!(!zeus_click(Some(Item::Wand), true, true));
        assert!(!zeus_click(None, true, false));
        // Et le bloc visé n'est pas cassable avec elle en main (`handle_blocks`).
        let src = include_str!("session.rs");
        let f = &src[src.find("fn handle_blocks(").unwrap()..];
        let f = &f[..f.find("\n    fn ").unwrap()];
        assert!(f.contains("&& !zeus;"), "le bloc se casse avec la Foudre en main");
        assert_eq!(f.matches("zeus_click(").count(), 2);
    }
}

#[cfg(test)]
mod name_tests {
    use super::*;

    /// (v189) Pseudos des NPC et des joueurs : un peu plus grands à toute distance, et
    /// lisibles bien plus loin (avant, ils disparaissaient sous 7 pixels de haut).
    #[test]
    fn names_are_a_bit_bigger_and_seen_from_further() {
        let s = 3.0; // écran de 1080 pixels de haut
        let before = |d: f32| s * (6.5 / d).min(4.0);
        for d in [2.0, 6.5, 10.0, 18.0] {
            let (ts, a) = name_label(d, s).unwrap();
            assert!(
                ts > before(d) * 1.2,
                "{d} blocs : {ts} contre {}",
                before(d)
            );
            assert_eq!(a, 1.0);
        }
        // Avant : rien au-delà de 22 blocs (15 sur un écran de 720 pixels).
        assert!(before(23.0) * 8.0 < 7.0);
        for (d, s) in [(23.0, 3.0), (27.0, 3.0), (27.0, 2.0), (27.0, 1.0)] {
            let (ts, a) = name_label(d, s).unwrap();
            assert!(
                ts * 8.0 >= 8.0 && a == 1.0,
                "{d} blocs, échelle {s} : {ts} {a}"
            );
        }
        // Effacé sur les 4 derniers blocs, plus rien à 32.
        let (_, a) = name_label(30.0, s).unwrap();
        assert!(a > 0.0 && a < 1.0, "{a}");
        assert!(name_label(32.0, s).is_none());
        assert!(name_label(40.0, s).is_none());
        // Tout près : pas plus de 4 fois la taille normale.
        assert_eq!(name_label(0.5, s).unwrap().0, s * 4.0);
    }
}

#[cfg(test)]
mod stairs_tests {
    use super::*;

    #[test]
    fn stairs_face_the_player_and_flip_on_ceilings_and_high_halves() {
        let reg = minelib_world::registry::registry();
        let st = reg.by_key("stairs:planks_oak:0:bottom").unwrap();
        let hit = |n: [i32; 3]| Hit {
            block: [4, 10, 4],
            normal: n,
            id: BlockId::STONE,
            distance: 3.0,
        };
        let key = |id: BlockId| id.def().key.clone();
        // On regarde vers −Z, posé sur le sol : marche haute côté −Z, bas vers soi.
        let id = stairs_placement(
            &hit([0, 1, 0]),
            st,
            Vec3::new(0.1, -0.5, -1.0),
            Vec3::new(4.5, 11.0, 4.5),
        )
        .unwrap();
        assert_eq!(key(id), "stairs:planks_oak:3:bottom");
        let s = minelib_world::registry::stairs_boxes(3, false);
        assert_eq!(s[1].0[2], 0, "marche haute du côté regardé (−Z)");
        // Au plafond : à l'envers.
        let id = stairs_placement(
            &hit([0, -1, 0]),
            st,
            Vec3::new(1.0, 0.5, 0.0),
            Vec3::new(4.5, 10.0, 4.5),
        )
        .unwrap();
        assert_eq!(key(id), "stairs:planks_oak:0:top");
        // Contre un mur, moitié haute : à l'envers ; moitié basse : à l'endroit.
        let id = stairs_placement(
            &hit([0, 0, 1]),
            st,
            Vec3::new(0.0, 0.0, -1.0),
            Vec3::new(4.5, 10.8, 5.0),
        )
        .unwrap();
        assert_eq!(key(id), "stairs:planks_oak:3:top");
        let id = stairs_placement(
            &hit([0, 0, 1]),
            st,
            Vec3::new(0.0, 0.0, -1.0),
            Vec3::new(4.5, 10.2, 5.0),
        )
        .unwrap();
        assert_eq!(key(id), "stairs:planks_oak:3:bottom");
        assert!(stairs_placement(&hit([0, 1, 0]), BlockId::STONE, Vec3::X, Vec3::ZERO).is_none());
    }
}

#[cfg(test)]
mod slab_tests {
    use super::*;

    fn key(id: BlockId) -> &'static str {
        &id.def().key
    }

    /// (v236) Ombres exactes de la foule : à distance égale, le joueur dans l'axe de vue
    /// passe avant celui de côté, qui passe avant celui de derrière ; un joueur tout près
    /// passe avant un plus loin dans l'axe.
    #[test]
    fn exact_shadows_go_to_the_nearest_and_to_the_view_axis() {
        let cam = Vec3::new(0.0, 1.6, 0.0);
        let look = -Vec3::Z;
        let at = |x: f32, z: f32| shadow_view_score(cam, look, Vec3::new(x, 0.7, z));
        let (ahead, side, behind) = (at(0.0, -10.0), at(10.0, 0.0), at(0.0, 10.0));
        assert!(ahead < side && side < behind, "{ahead} {side} {behind}");
        assert!(
            at(2.0, 0.0) < at(0.0, -6.0),
            "tout près de côté, avant loin devant"
        );
        assert!(
            at(0.0, -12.0) < at(9.0, 0.0),
            "loin dans l'axe, avant moins loin de côté"
        );
    }

    /// (v237) « L'ombre de mon bras qui tient l'éclair ne doit pas pouvoir disparaître » :
    /// la lumière de la Foudre de Zeus tenue est hors de tous les morceaux du joueur,
    /// quelle que soit sa pose : aucun n'est écarté, le bras garde toujours son ombre.
    /// L'ancienne règle (0,2 bloc autour de la boîte droite de chaque morceau) en écartait
    /// selon la pose : l'ombre allait et venait.
    #[test]
    fn the_arm_holding_the_bolt_always_casts_its_shadow() {
        let (mut poses, mut dropped) = (0, 0);
        for k in 0..48 {
            for crouch in [0.0, 1.0] {
                for pitch in [-1.2f32, 0.0, 1.2] {
                    let pose = crate::player_model::Pose {
                        feet: Vec3::new(3.0, 70.0, -2.0),
                        yaw: 0.7 + k as f32 * 0.4,
                        pitch,
                        walk: k as f32 * 0.37,
                        swing: (k % 3) as f32 * 0.5,
                        crouch,
                        light: [1.0; 3],
                        held: Some(crate::player_model::Held::Zeus),
                        skin: None,
                        bow: 0.0,
                        air: 0.0,
                        dir: 0.0,
                        run: 0.0,
                    };
                    let at = crate::player_model::zeus_frame(&pose)
                        .unwrap()
                        .transform_point3(Vec3::Y * (crate::lightning::HELD_LEN * 0.42));
                    let boxes = crate::player_model::shadow_boxes(&pose);
                    assert!(boxes.iter().all(|b| !inside_box(b, at)), "{k} {crouch} {pitch}");
                    poses += 1;
                    dropped += usize::from(boxes.iter().any(|b| near_box(b, at, 0.2)));
                }
            }
        }
        // (v244) L'éclair penché de 45° vers l'avant porte sa lumière plus loin encore du
        // joueur : l'ancienne règle elle-même n'écarterait plus aucun morceau.
        assert!(dropped < poses, "{dropped} poses sur {poses}");
        // Dans un morceau, la lumière l'écarte toujours (sinon tout serait dans l'ombre).
        let pose = crate::player_model::Pose {
            feet: Vec3::new(3.0, 70.0, -2.0),
            yaw: 0.7,
            pitch: 0.0,
            walk: 0.0,
            swing: 0.0,
            crouch: 0.0,
            light: [1.0; 3],
            held: Some(crate::player_model::Held::Zeus),
            skin: None,
            bow: 0.0,
            air: 0.0,
            dir: 0.0,
            run: 0.0,
        };
        let boxes = crate::player_model::shadow_boxes(&pose);
        let mid = (Vec3::from(boxes[0].min) + Vec3::from(boxes[0].max)) * 0.5;
        assert!(inside_box(&boxes[0], mid));
    }

    #[test]
    fn no_sun_shadow_from_a_player_under_a_roof() {
        let reg = minelib_world::registry::registry();
        let mut w = World::new(0);
        w.insert_empty(ChunkPos { x: 0, z: 0 });
        let stone = reg.by_key("stone").unwrap();
        let glass = reg.by_key("glass").unwrap();
        let pose = crate::player_model::Pose {
            feet: Vec3::new(8.5, 10.0, 8.5),
            yaw: 0.3,
            pitch: 0.0,
            walk: 0.0,
            swing: 0.0,
            crouch: 0.0,
            light: [1.0; 3],
            held: None,
            skin: None,
            bow: 0.0,
            air: 0.0,
            dir: 0.0,
            run: 0.0,
        };
        let boxes = crate::player_model::shadow_boxes(&pose);
        let sun = Vec3::new(0.3, 0.9, 0.2).normalize().to_array();
        // En plein air : le soleil atteint chaque morceau du joueur.
        assert!(boxes.iter().all(|b| sun_reaches(&w, b, sun)));
        // Sous un toit de verre : toujours.
        for x in 0..16 {
            for z in 0..16 {
                w.set(x, 14, z, glass);
            }
        }
        assert!(boxes.iter().all(|b| sun_reaches(&w, b, sun)));
        // Sous un toit de pierre : plus de soleil, plus d'ombre du soleil.
        for x in 0..16 {
            for z in 0..16 {
                w.set(x, 14, z, stone);
            }
        }
        assert!(boxes.iter().all(|b| !sun_reaches(&w, b, sun)));
        // La nuit (soleil sous l'horizon) : rien non plus.
        let w2 = World::new(0);
        assert!(!sun_reaches(&w2, &boxes[0], [0.0, -1.0, 0.0]));
    }

    #[test]
    fn slabs_orient_and_merge_into_a_full_block() {
        let reg = minelib_world::registry::registry();
        let mut w = World::new(0);
        w.insert_empty(ChunkPos { x: 0, z: 0 });
        let stone = reg.by_key("stone").unwrap();
        let slab = reg.by_key("slab:stone:bottom").unwrap();
        w.set(4, 10, 4, stone);
        let hit = |block: [i32; 3], normal: [i32; 3], id: BlockId| Hit {
            block,
            normal,
            id,
            distance: 2.0,
        };
        let pt = Vec3::new(4.5, 10.2, 4.5);
        // Sur le dessus d'un bloc : dalle du bas.
        let (p, id) =
            slab_placement(&w, &hit([4, 10, 4], [0, 1, 0], stone), slab, pt, false).unwrap();
        assert_eq!((p, key(id)), ([4, 11, 4], "slab:stone:bottom"));
        w.set(4, 11, 4, id);
        // Sur sa moitié libre : bloc complet.
        let (p, id) = slab_placement(&w, &hit([4, 11, 4], [0, 1, 0], id), slab, pt, false).unwrap();
        assert_eq!((p, key(id)), ([4, 11, 4], "stone"));
        // Accroupi contre un mur (face +X du bloc) : dalle verticale collée au mur (côté −X).
        let (p, id) =
            slab_placement(&w, &hit([4, 10, 4], [1, 0, 0], stone), slab, pt, true).unwrap();
        assert_eq!((p, key(id)), ([5, 10, 4], "slab:stone:1"));
        w.set(5, 10, 4, id);
        // Deuxième dalle contre la première : le bloc se remplit.
        let (p, id) = slab_placement(&w, &hit([5, 10, 4], [1, 0, 0], id), slab, pt, false).unwrap();
        assert_eq!((p, key(id)), ([5, 10, 4], "stone"));
        // Debout contre un mur, en visant le haut puis le bas : dalle du haut, du bas.
        let up = Vec3::new(4.5, 10.8, 5.0);
        let (_, id) =
            slab_placement(&w, &hit([4, 10, 4], [0, 0, 1], stone), slab, up, false).unwrap();
        assert_eq!(key(id), "slab:stone:top");
        let low = Vec3::new(4.5, 10.3, 5.0);
        let (_, id) =
            slab_placement(&w, &hit([4, 10, 4], [0, 0, 1], stone), slab, low, false).unwrap();
        assert_eq!(key(id), "slab:stone:bottom");
        // Accroupi au sol : verticale, du côté du bord visé (quatre positions).
        w.set(4, 11, 4, BlockId::AIR);
        for (x, z, want) in [
            (4.9, 4.5, "slab:stone:0"),
            (4.1, 4.5, "slab:stone:1"),
            (4.5, 4.9, "slab:stone:4"),
            (4.5, 4.1, "slab:stone:5"),
        ] {
            let at = Vec3::new(x, 11.0, z);
            let (p, id) =
                slab_placement(&w, &hit([4, 10, 4], [0, 1, 0], stone), slab, at, true).unwrap();
            assert_eq!((p, key(id)), ([4, 11, 4], want));
        }
        // Accroupi au plafond : verticale aussi.
        let at = Vec3::new(4.2, 10.0, 4.5);
        let (p, id) =
            slab_placement(&w, &hit([4, 10, 4], [0, -1, 0], stone), slab, at, true).unwrap();
        assert_eq!((p, key(id)), ([4, 9, 4], "slab:stone:1"));
        // Pas une dalle : rien de spécial.
        assert!(slab_placement(&w, &hit([4, 10, 4], [0, 1, 0], stone), stone, pt, false).is_none());
    }

    #[test]
    fn doors_and_trapdoors_open_and_close() {
        let reg = minelib_world::registry::registry();
        let mut w = World::new(0);
        w.insert_empty(ChunkPos { x: 0, z: 0 });
        let mut opened = HashMap::new();
        let lower = reg.by_key("door_wood:lower:1").unwrap();
        let upper = reg.by_key("door_wood:upper:1").unwrap();
        w.set(3, 10, 3, lower);
        w.set(3, 11, 3, upper);
        let look = Vec3::new(1.0, 0.0, 0.0);
        let ch = toggle_hinged(&w, &mut opened, [3, 11, 3], look);
        assert_eq!(ch.len(), 2, "les deux moitiés s'ouvrent");
        for (p, b) in &ch {
            w.set(p[0], p[1], p[2], *b);
        }
        assert_eq!(key(w.get(3, 10, 3)), "door_wood:lower:5:o");
        for (p, b) in toggle_hinged(&w, &mut opened, [3, 10, 3], look) {
            w.set(p[0], p[1], p[2], b);
        }
        assert_eq!(w.get(3, 10, 3), lower);
        assert_eq!(w.get(3, 11, 3), upper);
        // Trappe : se relève contre le côté opposé au joueur, puis se rabat.
        let trap = reg.by_key("trapdoor:3").unwrap();
        w.set(6, 10, 6, trap);
        for (p, b) in toggle_hinged(&w, &mut opened, [6, 10, 6], Vec3::new(0.0, -0.5, 1.0)) {
            w.set(p[0], p[1], p[2], b);
        }
        assert_eq!(key(w.get(6, 10, 6)), "trapdoor:4");
        for (p, b) in toggle_hinged(&w, &mut opened, [6, 10, 6], Vec3::Z) {
            w.set(p[0], p[1], p[2], b);
        }
        assert_eq!(w.get(6, 10, 6), trap);
        // Trappe posée : se relève toujours du côté choisi à la pose, d'où qu'on l'ouvre.
        let trap = reg.by_key("trapdoor:3:1").unwrap();
        for look in [Vec3::X, -Vec3::X, Vec3::Z] {
            w.set(6, 10, 6, trap);
            let mut opened = HashMap::new();
            for (p, b) in toggle_hinged(&w, &mut opened, [6, 10, 6], look) {
                w.set(p[0], p[1], p[2], b);
            }
            assert_eq!(key(w.get(6, 10, 6)), "trapdoor:1");
        }
        // Ouverte sans souvenir (importée) : se rabat et se relève du même côté.
        let mut opened = HashMap::new();
        for (p, b) in toggle_hinged(&w, &mut opened, [6, 10, 6], Vec3::Z) {
            w.set(p[0], p[1], p[2], b);
        }
        assert_eq!(key(w.get(6, 10, 6)), "trapdoor:3:1");
    }

    #[test]
    fn showelements_finds_single_and_double_doors() {
        let reg = minelib_world::registry::registry();
        let mut w = World::new(0);
        w.insert_empty(ChunkPos { x: 0, z: 0 });
        let lower = reg.by_key("door_wood:lower:1").unwrap();
        let upper = reg.by_key("door_wood:upper:1").unwrap();
        // Double porte le long de Z (z = 2 et 3), porte simple plus loin (z = 9).
        for z in [2, 3, 9] {
            w.set(3, 10, z, lower);
            w.set(3, 11, z, upper);
        }
        let mut doors = chunk_doors(&w, ChunkPos { x: 0, z: 0 });
        doors.sort_by_key(|d| d.min);
        assert_eq!(
            doors,
            vec![
                DoorBox {
                    min: [3, 10, 2],
                    max: [3, 11, 3],
                    double: true
                },
                DoorBox {
                    min: [3, 10, 9],
                    max: [3, 11, 9],
                    double: false
                },
            ]
        );
        // (v258) N'importe quel bloc d'une porte donne toute la porte (la sélection d'un seul
        // bloc devient la porte : deux blocs, quatre pour une double porte).
        let get = |c: [i32; 3]| w.get(c[0], c[1], c[2]);
        for c in [[3, 10, 2], [3, 11, 3], [3, 11, 2]] {
            assert_eq!(door_box(&get, c), Some(doors[0]), "{c:?}");
        }
        assert_eq!(door_box(&get, [3, 11, 9]), Some(doors[1]));
        assert_eq!(door_box(&get, [3, 12, 9]), None);
    }

    #[test]
    fn double_doors_open_mirrored_and_can_open_together() {
        let reg = minelib_world::registry::registry();
        let mut w = World::new(0);
        w.insert_empty(ChunkPos { x: 0, z: 0 });
        let lower = reg.by_key("door_wood:lower:1").unwrap();
        let upper = reg.by_key("door_wood:upper:1").unwrap();
        // Deux battants côte à côte le long de Z : A en z = 3, B en z = 2.
        for z in [2, 3] {
            w.set(3, 10, z, lower);
            w.set(3, 11, z, upper);
        }
        let look = Vec3::X;
        let mut opened = HashMap::new();
        let apply = |w: &mut World, ch: Vec<([i32; 3], BlockId)>| {
            for (p, b) in ch {
                w.set(p[0], p[1], p[2], b);
            }
        };
        // Chacun seul : en miroir, gonds à l'extérieur.
        let ch = toggle_door_pair_with(
            &|c| w.get(c[0], c[1], c[2]),
            &mut opened,
            [3, 10, 3],
            look,
            false,
        );
        assert_eq!(ch.len(), 2);
        apply(&mut w, ch);
        assert_eq!(key(w.get(3, 10, 3)), "door_wood:lower:4:m");
        let ch = toggle_door_pair_with(
            &|c| w.get(c[0], c[1], c[2]),
            &mut opened,
            [3, 11, 2],
            look,
            false,
        );
        apply(&mut w, ch);
        assert_eq!(key(w.get(3, 10, 2)), "door_wood:lower:5:o");
        assert_eq!(key(w.get(3, 11, 2)), "door_wood:upper:5:o");
        // Les deux se referment ensemble (doorsync), depuis n'importe quel battant.
        let ch = toggle_door_pair_with(
            &|c| w.get(c[0], c[1], c[2]),
            &mut opened,
            [3, 10, 2],
            look,
            true,
        );
        assert_eq!(ch.len(), 4);
        apply(&mut w, ch);
        for z in [2, 3] {
            assert_eq!(w.get(3, 10, z), lower);
            assert_eq!(w.get(3, 11, z), upper);
        }
        let ch = toggle_door_pair_with(
            &|c| w.get(c[0], c[1], c[2]),
            &mut opened,
            [3, 11, 3],
            look,
            true,
        );
        assert_eq!(ch.len(), 4);
        apply(&mut w, ch);
        assert_eq!(key(w.get(3, 11, 3)), "door_wood:upper:4:m");
        assert_eq!(key(w.get(3, 11, 2)), "door_wood:upper:5:o");
        // Une porte seule s'ouvre toujours du même côté.
        let mut solo = World::new(0);
        solo.insert_empty(ChunkPos { x: 0, z: 0 });
        solo.set(5, 10, 5, lower);
        let mut o = HashMap::new();
        let ch = toggle_door_pair_with(
            &|c| solo.get(c[0], c[1], c[2]),
            &mut o,
            [5, 10, 5],
            look,
            true,
        );
        assert_eq!(ch.len(), 1);
        assert_eq!(key(ch[0].1), "door_wood:lower:5:o");
    }
}

/// Durée de la chute en cloche d'un objet lâché (s).
const TOSS_FLIGHT: f32 = 0.6;

/// Position à l'instant `t` d'un objet lâché de `from` qui retombe en `to` : en cloche,
/// plus haut au milieu du trajet.
fn toss_arc(from: Vec3, to: Vec3, t: f32) -> Vec3 {
    let k = (t / TOSS_FLIGHT).clamp(0.0, 1.0);
    let lift = 0.9 + (from.y - to.y).max(0.0) * 0.1;
    from.lerp(to, k) + Vec3::Y * lift * 4.0 * k * (1.0 - k)
}

/// (v177) Distance (blocs) jusqu'à laquelle l'arme des autres joueurs est dessinée avec son
/// modèle (au-delà, sa silhouette).
const GEAR_DETAIL: f32 = 40.0;

/// (v177) Temps entre deux bips de la bombe posée, selon le temps qui lui reste (40 s au
/// départ) : 1 s au début, 0,39 s à 5 s de la fin, 0,15 s tout à la fin.
fn bomb_beep_gap(left: f32) -> f32 {
    0.15 + 0.85 * (left / 40.0).clamp(0.0, 1.0).powf(0.6)
}

/// Yeux d'un joueur aux pieds `feet` (accroupi 0..1), comme `Player::eye`.
pub fn eye_of(feet: Vec3, crouch: f32) -> Vec3 {
    feet + Vec3::Y * (crate::player::EYE - crate::player::CROUCH_DROP * crouch.clamp(0.0, 1.0))
}

/// Lumière du ciel reçue aux yeux `e` (linéaire) : abrité par un bloc plein au-dessus ou
/// non, jour ou nuit. La même pour soi (`light_at_player`) et pour les autres joueurs.
fn sky_light(world: &World, e: Vec3, daylight: f32) -> [f32; 3] {
    sky_light_covered(covered_above(world, e), daylight)
}

/// La même lumière quand on sait déjà si le ciel est bouché : la foule d'essai garde
/// l'abri de chacun en mémoire (voir `crowd`), c'est la partie qui coûte.
fn sky_light_covered(covered: bool, daylight: f32) -> [f32; 3] {
    [if covered { 0.35 } else { 1.0 } * (0.18 + 0.82 * daylight); 3]
}

/// Un bloc plein au-dessus de `e` : la colonne de blocs à parcourir, la partie chère de
/// `sky_light`.
pub fn covered_above(world: &World, e: Vec3) -> bool {
    let (x, z) = (e.x.floor() as i32, e.z.floor() as i32);
    // Un bloc plein plus haut que les yeux (au-dessus de la couche la plus haute du chunk,
    // que de l'air).
    let from = (e.y.floor() as i32 + 1).max(0) as usize;
    world.chunk(ChunkPos::containing(x, z)).is_some_and(|c| {
        let (lx, lz) = (
            x.rem_euclid(CHUNK_SIZE as i32) as usize,
            z.rem_euclid(CHUNK_SIZE as i32) as usize,
        );
        (from..c.top()).any(|y| c.get(lx, y, lz).is_solid())
    })
}
