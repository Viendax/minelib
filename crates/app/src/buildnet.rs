//! Construction en ligne, sans limite de joueurs.
//!
//! Le relais découpe le monde en zones (`proto::ZONE` × `ZONE` chunks) et confie chaque
//! zone active à un garant : de préférence un joueur d'un autre monde, sans lien avec
//! celui-ci (sa connexion de garant, en arrière-plan), sinon un joueur du monde. Il y a
//! d'autant plus de garants que le monde a de joueurs et de zones actives.
//!
//! **Une modification** (poser, casser) suit toujours le même chemin, pour tout le monde :
//! 1. le joueur l'applique tout de suite chez lui et l'envoie comme demande (`Edit`) au
//!    garant de la zone, en gardant le bloc d'avant ;
//! 2. le garant vérifie (bloc valide, dans sa zone, pas plus de 20 par seconde) et publie
//!    (`Set`), ou refuse (`Reject`, remis au seul demandeur, qui remet le bloc d'avant) ;
//! 3. le relais transmet la publication aux joueurs proches et la garde dans l'historique
//!    de la zone, compacté (la dernière modification de chaque case, numérotée) ;
//! 4. sans réponse en 4 secondes (garant parti), le joueur annule.
//!
//! Chacun reçoit exactement ce qui lui manque : en s'approchant d'une zone (même après
//! s'en être éloigné un moment), en arrivant dans le monde, ou par paquets pour le
//! propriétaire, qui enregistre le monde. Le monde de chacun est celui des garants.
//!
//! **Les positions** des joueurs ne passent pas par les garants : le relais les transmet
//! aux joueurs proches, toutes tout près, une sur 4 un peu plus loin, une sur 20 au loin ;
//! chaque jeu fait avancer les autres en douceur entre deux positions.
//!
//! **Ajouter un comportement synchronisé** (porte, coffre, mécanisme…) : une nouvelle
//! sorte de `BuildMsg` (dans `minelib_net::game`), demandée au garant de la zone, vérifiée
//! dans `validate`, publiée comme `Set` ; le relais la garde dans l'historique (compactée
//! par case dans `zones::ZoneLog` si c'est un état). Les essais de bout en bout de ce
//! fichier (`run`) vérifient que tous les mondes restent identiques.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::Sender;

use glam::Vec3;
use minelib_net::client::{Client, Cmd, Event};
use minelib_net::game;
use minelib_net::proto::{self, Request, Response};
use minelib_net::transport::{self, Inbound};
use minelib_world::{BlockId, ChunkPos, World};

/// Changement que l'écoulement d'un fluide peut faire : fluide qui avance (dans le vide,
/// une plante, (v185) une torche ou de la redstone qu'il emporte, ou un fluide), qui sèche,
/// ou qui se fige (obsidienne, pierre taillée, pierre).
fn fluid_edit(old: BlockId, new: BlockId) -> bool {
    use minelib_world::registry::{fluid, washed_by_fluid};
    let open = |b: BlockId| b == BlockId::AIR || fluid(b).is_some() || washed_by_fluid(b);
    let set = |b: BlockId| {
        b == BlockId::AIR
            || fluid(b).is_some()
            || b == BlockId::COBBLESTONE
            || b == BlockId::STONE
            || Some(b) == minelib_world::registry::registry().by_key("obsidian")
    };
    (fluid(new).is_some() && open(old)) || (fluid(old).is_some() && set(new))
}
/// Position envoyée au moins 10 fois par seconde (foule), au plus 60 (peu de joueurs) ; le
/// jeu fait avancer chacun en douceur entre deux positions reçues.
///
/// Avant : de 20 à 256 fois par seconde, pour 4096 positions reçues par joueur. La
/// simulation de foule a montré ce que ça coûte au relais : 250 Mbit/s à envoyer pour
/// 100 joueurs au même endroit, et déjà 55 Mbit/s pour 16. C'est la connexion de celui
/// qui héberge le relais qui cassait en premier, bien avant le nombre de joueurs.
const POS_EVERY: f32 = 0.1;
const POS_CROWD: f32 = 1.0 / 60.0;
/// (v275) Avec peu de voisins (`POS_CLOSE` au plus) : voir `pos_every`.
const POS_FASTEST: f32 = 1.0 / 256.0;
const POS_CLOSE: usize = 3;
/// Positions reçues par seconde qu'on vise au plus (tous les voisins proches réunis) : le
/// rythme d'envoi de chacun baisse quand il y a du monde autour.
const POS_BUDGET: f32 = 960.0;
/// Un joueur dont on n'a plus de nouvelles depuis ce temps (ou trois de ses intervalles,
/// pour un joueur lointain rafraîchi rarement) n'est plus montré.
const STALE_S: f32 = 3.0;
/// Servi par un relais de région : là où l'on est, redit au relais du monde toutes les…
/// (gardé pour notre retour sur ce serveur).
const SPOT_EVERY: f32 = 10.0;

/// Intervalle entre deux envois de sa position, selon le nombre de joueurs autour : 60
/// fois par seconde jusqu'à 16 voisins, puis de moins en moins (30 à 32 voisins), jamais
/// moins de 10.
///
/// (v275) Avec `POS_CLOSE` voisins au plus (4 joueurs au même endroit), 256 fois par
/// seconde : chacun en reçoit alors 768 au plus, sous `POS_BUDGET`, et le relais ne les
/// retient plus 20 ms (`transport::FAST_LINGER`). Une position part au plus par image :
/// il faut que le jeu tourne à 256 images par seconde pour y arriver. Au-delà de
/// `POS_CLOSE` voisins rien ne change (le relais regroupe les positions par 20 ms : en
/// envoyer plus de 60 n'arriverait pas), et seul non plus (personne ne les reçoit).
///
/// `slow` : la position part au relais d'une région ou d'une province hébergé par un
/// joueur, et ce relais n'est joint que par les serveurs relais publics d'iroh (hôte
/// anonyme, ou box qui ne laisse pas passer le direct). Rien ne change là non plus : ces
/// serveurs limitent le débit, et c'est ce qui casserait en premier. (v285 : un hôte
/// joint en direct, `hote_direct=true` chez lui, reçoit les positions comme le relais du
/// monde. v301 : un hôte joint par un passeur aussi, voir `minelib_p2p::passeur`.)
///
/// (v285) Dans une foule (plus de `POS_CLOSE` voisins), des positions rapprochées partent
/// entre deux de celles-ci (`Request::NearbyClose`, 256 par seconde en tout) : le relais
/// ne les remet qu'aux 3 voisins les plus proches. Chacun reçoit alors au plus
/// `POS_BUDGET` positions comme avant, plus 768 de ses 3 voisins proches.
fn pos_every(others: usize, slow: bool) -> f32 {
    if !slow && (1..=POS_CLOSE).contains(&others) {
        return POS_FASTEST;
    }
    let hz = (POS_BUDGET / others.max(1) as f32).clamp(1.0 / POS_EVERY, 1.0 / POS_CROWD);
    1.0 / hz
}

pub use minelib_net::game::BuildMsg;

/// Une modification non confirmée par le relais du monde depuis ce temps est annulée
/// (message perdu) : le monde de chacun reste celui du relais.
const PENDING_TIMEOUT: f32 = 4.0;
/// Cases dont on retient le rang de la dernière modification, au plus (au-delà, tout est
/// oublié : ce rang ne sert qu'à écarter une modification arrivée en retard par un autre
/// chemin, ce qui se joue en quelques secondes).
const SEEN_MAX: usize = 1 << 20;
/// Pseudos retenus au plus (voir `BuildNet::names`) ; au-delà, on repart de zéro : chaque
/// relais redit un pseudo toutes les cinq secondes.
const NAMES_MAX: usize = 1 << 16;

pub struct Other {
    pub pseudo: String,
    /// Son numéro sur le relais du monde (vocal), s'il passe par lui.
    pub id: Option<u64>,
    /// Position montrée : elle rejoint la dernière reçue en douceur (les joueurs lointains
    /// envoient moins souvent, voir le relais).
    pub pos: Vec3,
    target: Vec3,
    /// Intervalle moyen entre deux positions reçues (s).
    interval: f32,
    pub yaw: f32,
    pub pitch: f32,
    pub crouch: f32,
    #[allow(dead_code)]
    pub speed: f32,
    seen: f32,
    /// (v165) Son ping avec son relais (ms), s'il est connu.
    pub ping: Option<u16>,
    /// (v222) Ce qu'il tient en main et qu'on voit (lampe torche, Foudre de Zeus).
    pub held: Option<minelib_net::poses::Held>,
    /// (v241) Il a la Foudre de Zeus sur lui sans la tenir : on la voit dans son dos.
    pub back: bool,
}

/// Le bloc qui fait foi en `pos` vient de changer (`truth` : publié par le garant, ou
/// revenu après un refus) alors que le joueur a déjà rechangé cette case sans que ce soit
/// encore envoyé : ces modifications partent désormais de `truth` (c'est lui qui reviendra
/// si elles sont refusées), et restent à l'écran. Vrai s'il y en avait.
fn rebase_journal(world: &mut World, pos: [i32; 3], truth: BlockId) -> bool {
    let Some(journal) = world.journal.as_mut() else {
        return false;
    };
    let mut found = false;
    for e in journal.iter_mut().filter(|e| e.0 == pos) {
        e.2 = truth;
        found = true;
    }
    found
}

/// Modification du joueur en attente de la confirmation du garant : bloc d'avant,
/// modifications de cette case pas encore confirmées, âge.
struct Pending {
    old: BlockId,
    count: u32,
    age: f32,
    /// Dernière modification de cette case partie signée au relais d'une région
    /// souveraine (bloc, fluide) : refusée par ce relais ou sans réponse, elle repart une
    /// fois au relais du monde (voir `retry_at_root`).
    signed: Option<(u16, bool)>,
    /// Dernière modification demandée (bloc, fluide) : elle repart après une pause du
    /// monde ou sa reprise par le relais de secours.
    last: (u16, bool),
}

/// Provinces (v138) : connexion au relais d'une province en vue (il décide des blocs de
/// ses zones, à la place du relais du monde).
struct ProvinceConn {
    info: proto::ProvinceInfo,
    client: Client,
    link: Option<Sender<Inbound>>,
    /// Entré dans la province (certificat accepté).
    joined: bool,
    /// Sans bail (le relais du monde va la reprendre, ou le monde est en pause) : les
    /// modifications de ses zones attendent.
    paused: bool,
    sent_chunk: Option<[i32; 2]>,
    served_sent: Option<Vec<[i32; 2]>>,
    /// Relais de ses régions (annoncés par elle).
    shards: Vec<proto::ShardInfo>,
    /// Numéro de cette connexion (comme pour un relais de région : ce qu'elle remet reste
    /// en doute jusqu'à ce que le relais du monde l'ait gardé, voir `ProvinceConfirmed`).
    serial: u64,
    /// Blocs reçus depuis sa dernière marque, et ceux de chaque envoi marqué pas encore
    /// confirmé : (case, rang).
    unmarked: Marked,
    marked: std::collections::VecDeque<(u64, Marked)>,
    /// (v139) États d'objets (au sol, coffres) qu'elle a publiés : ils n'entrent dans la
    /// partie qu'une fois gardés par le relais du monde (une province qui tombe avant n'a
    /// rien donné, rien pris : le relais du monde la reprend telle qu'il l'a gardée).
    unmarked_items: Vec<BuildMsg>,
    marked_items: std::collections::VecDeque<(u64, Vec<BuildMsg>)>,
    /// (v145) Depuis combien de temps elle ne décide de rien en vue (au-delà de
    /// `LEAVE_AFTER`, on la quitte).
    idle: f32,
    /// (v147) L'appel de garants valable chez elle (celui de son pays).
    call: Option<proto::GarantCall>,
}

/// (v145) Relais de province (ou pays) qui ne décide plus de rien en vue depuis… (s) :
/// quitté (un pays n'a plus à recevoir les positions de ceux qui sont au fond de ses
/// provinces).
const LEAVE_AFTER: f32 = 5.0;

/// (v143) Une province (ou un pays) : ses coordonnées et sa taille (régions par côté).
type PKey = ([i32; 2], i32);

fn pkey(i: &proto::ProvinceInfo) -> PKey {
    (i.province, i.size)
}

/// Blocs remis par un relais de province dans un même envoi : (case, rang).
type Marked = Vec<([i32; 3], u64)>;

/// Qui décide des blocs d'un chunk.
enum Authority {
    /// Le relais du monde.
    Root,
    /// Un relais de province prêt (sa connexion).
    Province(Sender<Inbound>),
    /// Un relais de province pas encore prêt (connexion en cours, sans bail) : on attend.
    Waiting,
}

/// Un relais de province perdu est rappelé au plus toutes les… (s).
const PROVINCE_RETRY: f32 = 1.0;
/// (v146) Au fond d'un pays depuis ce temps (s) : veille (le jeu ferme sa connexion au
/// relais du monde, qui n'a plus rien à lui dire).
const DOZE_AFTER: f32 = 5.0;
/// En veille, ce qu'on voit n'est plus tout servi par des provinces où l'on est entré
/// depuis ce temps (s) : retour au relais du monde.
const WAKE_AFTER: f32 = 4.0;
/// Pas de veille avec un certificat qui expire dans moins de… (s) : on ne pourrait pas
/// revenir.
const DOZE_CERT_MIN: u64 = 3600;
/// Demandes au relais du monde gardées pendant la veille, au plus.
const DOZE_QUEUE_MAX: usize = 256;

/// (v146) Un skin que les relais acceptent : PNG de 64 × 64 (ou 64 × 32), 24 Ko au plus.
fn skin_ok(png: &[u8]) -> bool {
    const MAGIC: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    if png.len() > 24 * 1024 || png.len() < 33 || png[..8] != MAGIC || &png[12..16] != b"IHDR" {
        return false;
    }
    let w = u32::from_be_bytes([png[16], png[17], png[18], png[19]]);
    let h = u32::from_be_bytes([png[20], png[21], png[22], png[23]]);
    w == 64 && (h == 64 || h == 32)
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}
/// Demandes d'objets parties à des relais de province dont on garde la trace, au plus.
const ITEM_ASKED_MAX: usize = 4096;
/// (v224) Demande de lumière dont l'arbitre n'est pas prêt : refusée après ce temps (s).
const LIGHT_WAIT: f32 = 8.0;
/// Chunks autour de soi dont le relais du monde annonce les relais (comme `SHARD_VIEW` du
/// relais).
const ROOT_VIEW: i32 = 16;

pub struct BuildNet {
    link: Sender<Inbound>,
    owner: bool,
    pseudo: String,
    /// Zones dont ce jeu est le garant.
    pub zones: HashSet<[i32; 2]>,
    /// Joueurs proches, par pseudo (le même joueur peut passer d'un relais de région à
    /// l'autre).
    pub others: HashMap<String, Other>,
    chunk: Option<[i32; 2]>,
    pos_timer: f32,
    /// (v285) Prochaine position rapprochée (foule), et si le lien vers le relais qui
    /// reçoit nos positions est lent (hôte joint par les serveurs relais d'iroh), revu
    /// une fois par seconde.
    close_timer: f32,
    slow_link: (f32, bool),
    /// (v287) Relevé du réseau, écrit au journal chaque minute sans rien qui désigne un
    /// joueur : positions parties (pour tous, rapprochées), positions reçues, modifications
    /// annulées faute de réponse, blocs remis comme avant, et depuis quand on compte.
    net: [u32; 5],
    net_since: std::time::Instant,
    /// Rang (ordre du relais du monde) de la dernière modification appliquée à chaque
    /// case, et relais de région qui l'a remise (son numéro de connexion ; aucun : le
    /// relais du monde). Une modification plus ancienne arrivée après (doublon, autre
    /// chemin) est sans effet.
    seen: HashMap<[i32; 3], (u64, Option<u64>)>,
    /// Pseudo de chaque joueur dont un relais envoie les positions par son numéro :
    /// (relais de région qui le dit, aucun pour le relais du monde ; numéro) → pseudo.
    names: HashMap<(Option<u64>, u64), String>,
    /// Blocs remis par chaque relais de région (numéro de connexion) que le relais du
    /// monde n'a pas encore confirmés.
    heard: HashMap<u64, HashMap<[i32; 3], Heard>>,
    /// Ceux d'un relais quitté, et le temps qu'il leur reste : ce que le relais du monde
    /// n'a pas confirmé d'ici là revient comme avant (voir `forget_shard`).
    doubtful: HashMap<[i32; 3], (Heard, f32)>,
    /// Blocs défaits ainsi (essais).
    #[cfg(test)]
    undone: usize,
    /// Régions annoncées au relais du monde comme servies par un relais de région.
    served_sent: Vec<[i32; 2]>,
    /// Un relais de région a redit le battement du relais du monde depuis la dernière
    /// lecture (`take_relayed_beat`) : le relais du monde est en vie (v136 : il ne l'envoie
    /// plus lui-même deux fois par seconde aux joueurs servis par un relais de région).
    relayed_beat: bool,
    /// Prochain envoi au relais du monde de là où l'on est (`Request::Spot`).
    spot_timer: f32,
    /// Voix arrivée par les relais de région depuis la dernière lecture (qui parle,
    /// message).
    voice_in: Vec<(String, Vec<u8>)>,
    /// (v165) Son ping (ms) avec le relais qui sert l'endroit où l'on est (0 : pas encore
    /// mesuré) : il part avec ses positions, montré aux autres dans leur liste (Tab).
    ping: u16,
    /// (v220) Son monde n'est pas encore chargé (voir `Session::loading`) : sa position ne
    /// part pas, les autres ne le voient pas apparaître avant. (Ce qu'il faut du monde, si :
    /// le relais apprend dans quel chunk on arrive et envoie ce qu'il y a de changé.)
    pub hidden: bool,
    /// (v222) Ce qu'on tient en main et que les autres voient (lampe torche, Foudre de
    /// Zeus ; donné par la partie à chaque image), envoyé avec ses positions.
    pub held: Option<minelib_net::poses::Held>,
    /// (v241) On a la Foudre de Zeus sur soi sans la tenir : les autres la voient dans
    /// notre dos (donné par la partie, envoyé avec `held`).
    pub back: bool,
    /// Clé du relais du monde (dans le billet) : vérifie les bonjours de la voix.
    root: Option<[u8; 32]>,
    /// Clé du compte (modifications signées dans une région souveraine), et date de la
    /// dernière signature.
    identity: Option<minelib_net::ed25519_dalek::SigningKey>,
    sign_t: u64,
    caps_sent: bool,
    pending: HashMap<[i32; 3], Pending>,
    shards: Shards,
    /// Modifications en masse (WorldEdit d'un OP) envoyées au relais : blocs d'avant, pour
    /// les remettre s'il refuse.
    bulks: HashMap<u32, Vec<([i32; 3], BlockId)>>,
    next_bulk: u32,
    /// États d'objets (au sol, coffres) et refus reçus du relais du monde, à traiter par
    /// la partie (voir `crate::drops`) ; (v224) et lumières de l'éditeur (`crate::lightnet`).
    pub item_inbox: Vec<BuildMsg>,
    /// Message en cours de traitement venu d'un relais de région (tenu par un joueur ; son
    /// numéro de connexion) : seul le relais du monde arbitre les objets, on n'en accepte
    /// pas d'ailleurs.
    from_shard: Option<u64>,
    /// Monde en pause (son relais n'entend plus assez de gardiens, voir
    /// `minelib_net::guard`) : les modifications du joueur attendent la reprise.
    paused: bool,
    /// Provinces (v138) servies par un relais de province, en vue (liste du relais du
    /// monde), et la connexion à chacune ; relais de région annoncés par le relais du monde
    /// (hors de ces provinces) ; province dont vient le message en cours (elle ne décide
    /// que des blocs de ses zones) ; prochain rappel d'une province perdue.
    province_list: Vec<proto::ProvinceInfo>,
    provinces: HashMap<PKey, ProvinceConn>,
    /// (v143) Liste du relais du monde (provinces ou pays) ; `province_list` y ajoute celles
    /// des relais de pays où l'on est entré.
    root_list: Vec<proto::ProvinceInfo>,
    /// (v144) Dernière liste de chaque pays (gardée quand son relais tombe : ses provinces
    /// continuent, rattachées au relais de secours du pays).
    children_of: HashMap<PKey, Vec<proto::ProvinceInfo>>,
    shards_root: Vec<proto::ShardInfo>,
    from_province: Option<proto::ProvinceInfo>,
    province_retry: f32,
    /// Demandes d'objets parties à un relais de province (numéro → province) : lui seul
    /// peut les refuser.
    item_asked: HashMap<u32, PKey>,
    /// (v224) Pareil pour les demandes de lumières de l'éditeur ; et celles dont l'arbitre
    /// n'est pas encore prêt (province sans bail, connexion en cours) : elles attendent, au
    /// plus `LIGHT_WAIT` s, puis sont refusées ici.
    light_asked: HashMap<u32, PKey>,
    /// (v229) Demandes de foudre faites au relais d'une province (numéro → laquelle) : un
    /// refus n'est écouté que de celle à qui la demande a été faite.
    thor_asked: HashMap<u32, PKey>,
    /// (v229) Foudres publiées par les arbitres et refus de nos demandes (`BuildMsg::Thor`,
    /// `ThorDenied`), pour la partie (`Session::update_thor`).
    pub thor_inbox: Vec<BuildMsg>,
    light_queue: Vec<(BuildMsg, f32)>,
    /// (v227) Foules d'essai menées par d'autres joueurs (OP) : paquets reçus, par pseudo de
    /// celui qui la mène (voir `crowd::RemoteCrowd`).
    pub crowd_inbox: Vec<(String, Vec<u8>)>,
    /// (v227) Coups des autres joueurs à notre foule d'essai : qui, quel PNJ, d'où (ses yeux).
    pub crowd_hits: Vec<(String, u16, [f32; 3])>,
    /// (v227) Notre foule d'essai a été refusée par le relais (pas OP, trop de paquets).
    pub crowd_denied: bool,
    /// (v140) Tout ce qu'on avait en vue au dernier passage était servi par des relais de
    /// province où l'on est entré ; province dite au relais du monde avec la dernière
    /// position qu'il a reçue.
    was_deep: bool,
    root_province: Option<PKey>,
    /// (v146) Veille : au fond d'un pays, plus de connexion au relais du monde. Ce qui devait
    /// lui partir attend qu'on y revienne (`woke`) ; on y revient quand il le faut.
    dozing: bool,
    doze_queue: std::cell::RefCell<Vec<Request>>,
    wake_wanted: std::cell::Cell<bool>,
    /// Depuis combien de temps la veille est possible ; en veille, depuis combien de temps
    /// ce qu'on voit n'est plus tout servi par des provinces où l'on est entré.
    doze_t: f32,
    wake_t: f32,
    /// (v146) Son skin, déposé chez chaque province où l'on entre ; skins reçus des provinces
    /// (en veille) ; messages des OP arrivés par elles (mode, mort, heure), en veille.
    skin_png: Option<Vec<u8>>,
    skins_in: Vec<(String, Vec<u8>)>,
    notices: Vec<Event>,
}

/// Bloc remis par un relais de région : celui d'avant, celui remis, son rang ; et si
/// c'était la confirmation d'une modification du joueur (fluide ou non).
#[derive(Clone, Copy, Debug)]
struct Heard {
    pre: BlockId,
    got: BlockId,
    seq: u64,
    mine: Option<bool>,
}

/// Blocs non confirmés gardés par relais de région (au-delà, ils ne pourront pas être
/// défaits).
const HEARD_MAX: usize = 1 << 17;
/// Temps laissé au relais du monde pour confirmer les blocs d'un relais de région quitté
/// (secondes) : il renvoie alors tout ce qui manque de la région.
const DOUBT_WAIT: f32 = 3.0;

/// Relais de région (voir `minelib_relay::shard`) : la liste reçue du relais du monde, le
/// billet pour y entrer, et une connexion à chaque région en vue. Chaque message part au
/// relais de la région qu'il concerne ; une région sans relais passe par le relais du monde.
#[derive(Default)]
struct Shards {
    ticket: Option<Ticket>,
    /// Relais de chaque région (plusieurs quand elle est très peuplée : on n'entre que
    /// dans le sien, `proto::shard_for`).
    list: HashMap<[i32; 2], Vec<proto::ShardInfo>>,
    pseudo: String,
    conns: HashMap<[i32; 2], ShardConn>,
    /// Dossier du nœud iroh (tunnel vers un relais de région hors du réseau local).
    node_dir: Option<std::path::PathBuf>,
    /// Dernier numéro de connexion donné.
    serial: u64,
}

/// Billet pour les relais de région : monde, pseudo, fin de validité, sceau, et
/// certificat de la clé du compte.
type Ticket = (
    u64,
    String,
    u64,
    Vec<u8>,
    Option<minelib_net::sovereign::Cert>,
);

struct ShardConn {
    /// Numéro de cette connexion (un autre relais de la même région en aura un autre).
    serial: u64,
    info: proto::ShardInfo,
    client: Client,
    link: Option<Sender<Inbound>>,
    /// Dernier chunk annoncé à ce relais.
    sent_chunk: Option<[i32; 2]>,
}

impl Shards {
    fn link(&self, region: [i32; 2]) -> Option<&Sender<Inbound>> {
        self.conns.get(&region).and_then(|c| c.link.as_ref())
    }

    /// Se connecte aux relais des régions en vue de `chunk`, quitte les autres.
    fn keep_near(&mut self, chunk: [i32; 2]) {
        let lo = proto::region_of([chunk[0] - VIEW_CHUNKS, chunk[1] - VIEW_CHUNKS]);
        let hi = proto::region_of([chunk[0] + VIEW_CHUNKS, chunk[1] + VIEW_CHUNKS]);
        let in_view =
            |r: [i32; 2]| (lo[0]..=hi[0]).contains(&r[0]) && (lo[1]..=hi[1]).contains(&r[1]);
        let me = self.pseudo.clone();
        let mine = |list: &Vec<proto::ShardInfo>| proto::shard_for(list, &me).cloned();
        // (Le même relais dont seule l'époque a changé : on y reste.)
        self.conns.retain(|r, c| {
            let Some(i) = self.list.get(r).and_then(&mine) else {
                return false;
            };
            if !in_view(*r) || !i.same_relay(&c.info) {
                return false;
            }
            // (Une modification n'y part signée que pendant une époque souveraine.)
            c.info.epoch = i.epoch;
            true
        });
        let Some((room, pseudo, expires, sig, cert)) = self.ticket.clone() else {
            return;
        };
        for (region, list) in &self.list {
            if !in_view(*region) || self.conns.contains_key(region) {
                continue;
            }
            let Some(info) = mine(list) else { continue };
            let info = &info;
            let (addr, key, node) = (info.addr.clone(), info.key, info.node);
            let dir = self.node_dir.clone();
            // Directement (réseau local), sinon par un tunnel iroh vers le nœud de l'hôte.
            let client = Client::spawn_resolved(Box::new(move || {
                if let Ok(a) = addr.parse::<std::net::SocketAddr>() {
                    if std::net::TcpStream::connect_timeout(
                        &a,
                        std::time::Duration::from_millis(800),
                    )
                    .is_ok()
                    {
                        return Ok((addr.clone(), Some(key)));
                    }
                }
                let n = dir
                    .as_deref()
                    .and_then(crate::account::p2p_node)
                    .ok_or_else(|| anyhow::anyhow!("relais de région injoignable"))?;
                let local = n.tunnel(node)?;
                Ok((local.to_string(), Some(key)))
            }));
            client.send(Cmd::ShardLogin {
                room,
                pseudo: pseudo.clone(),
                expires,
                sig: sig.clone(),
                cert: cert.clone(),
            });
            self.serial += 1;
            self.conns.insert(
                *region,
                ShardConn {
                    serial: self.serial,
                    info: info.clone(),
                    client,
                    link: None,
                    sent_chunk: None,
                },
            );
        }
    }
}

/// Distance de vue (chunks) : un relais de région à moins de cela est rejoint.
const VIEW_CHUNKS: i32 = 12;

fn chunk_of(x: i32, z: i32) -> [i32; 2] {
    let c = ChunkPos::containing(x, z);
    [c.x, c.z]
}

impl BuildNet {
    /// (v243) Lien vers le relais du monde : la partie FPS du serveur passe par lui.
    pub fn world_link(&self) -> Sender<Inbound> {
        self.link.clone()
    }

    pub fn new(link: Sender<Inbound>, pseudo: String, owner: bool) -> Self {
        Self {
            link,
            owner,
            pseudo,
            zones: HashSet::new(),
            others: HashMap::new(),
            chunk: None,
            pos_timer: 0.0,
            close_timer: 0.0,
            slow_link: (0.0, false),
            net: [0; 5],
            net_since: std::time::Instant::now(),
            seen: HashMap::new(),
            names: HashMap::new(),
            served_sent: Vec::new(),
            relayed_beat: false,
            spot_timer: 0.0,
            voice_in: Vec::new(),
            ping: 0,
            hidden: false,
            held: None,
            back: false,
            root: None,
            identity: None,
            sign_t: 0,
            caps_sent: false,
            pending: HashMap::new(),
            item_inbox: Vec::new(),
            from_shard: None,
            paused: false,
            heard: HashMap::new(),
            doubtful: HashMap::new(),
            #[cfg(test)]
            undone: 0,
            bulks: HashMap::new(),
            next_bulk: 0,
            shards: Shards::default(),
            province_list: Vec::new(),
            provinces: HashMap::new(),
            root_list: Vec::new(),
            children_of: HashMap::new(),
            shards_root: Vec::new(),
            from_province: None,
            province_retry: 0.0,
            item_asked: HashMap::new(),
            light_asked: HashMap::new(),
            thor_asked: HashMap::new(),
            thor_inbox: Vec::new(),
            light_queue: Vec::new(),
            crowd_inbox: Vec::new(),
            crowd_hits: Vec::new(),
            crowd_denied: false,
            was_deep: false,
            root_province: None,
            dozing: false,
            doze_queue: std::cell::RefCell::new(Vec::new()),
            wake_wanted: std::cell::Cell::new(false),
            doze_t: 0.0,
            wake_t: 0.0,
            skin_png: None,
            skins_in: Vec::new(),
            notices: Vec::new(),
        }
    }

    /// Dossier du nœud iroh de ce jeu (tunnels vers les relais de région).
    pub fn set_node_dir(&mut self, dir: std::path::PathBuf) {
        self.shards.node_dir = Some(dir);
    }

    /// Relais des régions du monde (envoyé par le relais du monde ; ceux des provinces
    /// servies par un relais de province viennent de lui).
    pub fn set_shards(&mut self, list: Vec<proto::ShardInfo>) {
        self.shards_root = list;
        self.rebuild_shards();
    }

    /// Liste des relais de région : chaque région d'après celui qui en décide (relais du
    /// monde, ou relais de sa province).
    fn rebuild_shards(&mut self) {
        self.shards.pseudo = self.pseudo.clone();
        self.shards.list.clear();
        let delegated = |r: [i32; 2]| self.province_list.iter().any(|p| p.has(r));
        let from_root = self.shards_root.iter().filter(|i| !delegated(i.region));
        // (Chaque région d'après le relais qui en décide : le plus petit qui la contient.)
        let from_provinces = self.provinces.values().flat_map(|c| {
            c.shards.iter().filter(|i| {
                self.best(i.region)
                    .is_some_and(|b| pkey(b) == pkey(&c.info))
            })
        });
        let all: Vec<proto::ShardInfo> = from_root.chain(from_provinces).cloned().collect();
        for i in all {
            self.shards.list.entry(i.region).or_default().push(i);
        }
    }

    /// Provinces (v138) servies par un relais de province, en vue (liste du relais du
    /// monde). Une province qui n'y est plus revient au relais du monde : ce qui y attendait
    /// sa réponse lui repart.
    pub fn set_provinces(&mut self, list: Vec<proto::ProvinceInfo>) {
        self.root_list = list;
        self.merge_provinces();
    }

    /// (v143) Liste complète : celle du relais du monde, et les provinces que confient les
    /// relais de pays où l'on est entré (dans leur pays). Une connexion dont la province
    /// n'y est plus (ou plus au même relais) est quittée ; ce qui attendait la réponse d'une
    /// province qui n'y est plus repart à qui en décide maintenant.
    fn merge_provinces(&mut self) {
        // (v144, v145) Les provinces des pays du relais du monde, d'après les listes reçues
        // (celle de chaque pays, gardée même si son relais vient de tomber, et celles de
        // ses provinces, qui annoncent leurs voisines) ; la plus récente époque l'emporte.
        let mut best: HashMap<PKey, proto::ProvinceInfo> = self
            .root_list
            .iter()
            .map(|i| (pkey(i), i.clone()))
            .collect();
        let root = self.root_list.clone();
        self.children_of.retain(|sender, _| {
            root.iter().any(|r| {
                pkey(r) == *sender
                    || (r.size > sender.1
                        && r.has([sender.0[0] * sender.1, sender.0[1] * sender.1]))
            })
        });
        for kids in self.children_of.values() {
            for k in kids {
                if !root.iter().any(|r| r.contains(k)) {
                    continue;
                }
                if best.get(&pkey(k)).is_none_or(|old| old.epoch < k.epoch) {
                    best.insert(pkey(k), k.clone());
                }
            }
        }
        let mut list: Vec<proto::ProvinceInfo> = best.into_values().collect();
        list.sort_by_key(|i| (i.size, i.province));
        let gone: Vec<PKey> = self
            .provinces
            .iter()
            .filter(|(k, c)| !list.iter().any(|i| pkey(i) == **k && i.same_relay(&c.info)))
            .map(|(k, _)| *k)
            .collect();
        for k in &gone {
            if let Some(c) = self.provinces.remove(k) {
                self.forget_shard(c.serial);
            }
        }
        let back: Vec<proto::ProvinceInfo> = self
            .province_list
            .iter()
            .filter(|i| !list.iter().any(|j| pkey(j) == pkey(i)))
            .cloned()
            .collect();
        let changed = list != self.province_list;
        if changed {
            log::debug!(
                "construction ({}) : provinces {:?}",
                self.pseudo,
                list.iter()
                    .map(|i| (i.province, i.size, i.epoch))
                    .collect::<Vec<_>>()
            );
        }
        self.province_list = list;
        if changed {
            self.province_retry = 0.0;
        }
        self.rebuild_shards();
        for i in back {
            log::info!(
                "construction : province {:?} (taille {}) plus servie par son relais",
                i.province,
                i.size
            );
            self.resend_in(|r| i.has(r));
        }
    }

    /// (v143) Le relais de province dont vient ce qu'on traite décide-t-il de ce chunk (la
    /// plus petite province qui le contient) ? Un pays ne parle pas pour ses provinces.
    fn sender_decides(&self, chunk: [i32; 2]) -> bool {
        self.from_province
            .as_ref()
            .is_none_or(|i| self.best(proto::region_of(chunk)).map(pkey) == Some(pkey(i)))
    }

    /// (v145) Les provinces (et pays) qui décident d'une région en vue de ce chunk.
    fn needed(&self, chunk: [i32; 2]) -> HashSet<PKey> {
        let lo = proto::region_of([chunk[0] - ROOT_VIEW, chunk[1] - ROOT_VIEW]);
        let hi = proto::region_of([chunk[0] + ROOT_VIEW, chunk[1] + ROOT_VIEW]);
        (lo[0]..=hi[0])
            .flat_map(|x| (lo[1]..=hi[1]).map(move |z| [x, z]))
            .filter_map(|r| self.best(r).map(pkey))
            .collect()
    }

    /// (v143) Ce qui décide de la région `region` : la plus petite province (ou le plus
    /// petit pays) qui la contient.
    fn best(&self, region: [i32; 2]) -> Option<&proto::ProvinceInfo> {
        self.province_list
            .iter()
            .filter(|i| i.has(region))
            .min_by_key(|i| i.size)
    }

    /// Qui décide des blocs du chunk `chunk`.
    fn authority(&self, chunk: [i32; 2]) -> Authority {
        let region = proto::region_of(chunk);
        let Some(info) = self.best(region) else {
            return Authority::Root;
        };
        match self.provinces.get(&pkey(info)) {
            Some(c) if c.joined && !c.paused => match &c.link {
                Some(l) => Authority::Province(l.clone()),
                None => Authority::Waiting,
            },
            _ => Authority::Waiting,
        }
    }

    /// Les modifications en attente dans les régions choisies par `keep` repartent (à qui
    /// en décide maintenant).
    fn resend_in(&mut self, keep: impl Fn([i32; 2]) -> bool) {
        let list: Vec<([i32; 3], (u16, bool))> = self
            .pending
            .iter()
            .filter(|(p, _)| keep(proto::region_of(chunk_of(p[0], p[2]))))
            .map(|(p, v)| (*p, v.last))
            .collect();
        for (pos, (block, fluid)) in list {
            let provisional = self.send_edit(pos, block, fluid);
            if let Some(p) = self.pending.get_mut(&pos) {
                p.count = 1;
                p.age = 0.0;
                p.signed = provisional.then_some((block, fluid));
            }
        }
    }

    /// Relais de province : connexions (entrée avec le certificat de la clé du compte),
    /// messages reçus. Renvoie les chunks à remailler.
    fn pump_provinces(&mut self, world: &mut World, chunk: [i32; 2], dt: f32) -> Vec<ChunkPos> {
        let mut dirty = Vec::new();
        // Connexions manquantes (une province perdue est rappelée toutes les
        // `PROVINCE_RETRY` s).
        self.province_retry -= dt;
        let cert = self
            .shards
            .ticket
            .as_ref()
            .and_then(|t| t.4.clone())
            .filter(|c| c.pseudo.eq_ignore_ascii_case(&self.pseudo));
        // (v145) Seulement les relais qui décident de quelque chose en vue (un pays dont les
        // provinces servent tout ce qu'on voit n'est pas joint).
        let needed = self.needed(chunk);
        if self.province_retry <= 0.0 {
            self.province_retry = PROVINCE_RETRY;
            if let (Some(key), Some(cert)) = (self.identity.clone(), cert) {
                for info in &self.province_list {
                    if self.provinces.contains_key(&pkey(info)) || !needed.contains(&pkey(info)) {
                        continue;
                    }
                    let client = crate::province_host::dial(info, self.shards.node_dir.clone());
                    client.send(Cmd::Takeover {
                        cert: cert.clone(),
                        key: key.clone(),
                        expect: None,
                    });
                    self.shards.serial += 1;
                    self.provinces.insert(
                        pkey(info),
                        ProvinceConn {
                            info: info.clone(),
                            client,
                            link: None,
                            joined: false,
                            paused: true,
                            sent_chunk: None,
                            served_sent: None,
                            shards: Vec::new(),
                            serial: self.shards.serial,
                            unmarked: Vec::new(),
                            marked: std::collections::VecDeque::new(),
                            unmarked_items: Vec::new(),
                            marked_items: std::collections::VecDeque::new(),
                            idle: 0.0,
                            call: None,
                        },
                    );
                }
            }
        }
        let mut received = Vec::new();
        let mut broken = Vec::new();
        let mut ready = Vec::new();
        let mut shards_changed = false;
        let mut confirmed = Vec::new();
        let mut items_ok = Vec::new();
        let mut voice = Vec::new();
        let mut lists_changed = false;
        let mut kids_lists = Vec::new();
        // (v146) Son skin, déposé chez chaque province où l'on entre ; skins et messages
        // des OP qu'elles nous font passer (en veille : seulement ceux de la province où l'on
        // est, chacune de celles où l'on est entré les redit).
        let skin = self.skin_png.clone().filter(|p| skin_ok(p));
        let dozing = self.dozing;
        let here = self.best(proto::region_of(chunk)).map(pkey);
        let mut skins = Vec::new();
        let mut notices = Vec::new();
        for (p, c) in &mut self.provinces {
            while let Some(e) = c.client.poll() {
                match e {
                    Event::Link(l) => c.link = Some(l),
                    // Ce qu'elle a remis jusqu'ici part au relais du monde dans cet envoi ;
                    // gardé par lui, ce n'est plus en doute.
                    Event::Province(Response::ProvinceMark { n }) => {
                        let list = std::mem::take(&mut c.unmarked);
                        c.marked.push_back((n, list));
                        let items = std::mem::take(&mut c.unmarked_items);
                        if !items.is_empty() {
                            c.marked_items.push_back((n, items));
                        }
                    }
                    Event::Province(Response::ProvinceConfirmed { n }) => {
                        while c.marked.front().is_some_and(|(m, _)| *m <= n) {
                            let (_, list) = c.marked.pop_front().expect("envoi");
                            confirmed.push((c.serial, list));
                        }
                        while c.marked_items.front().is_some_and(|(m, _)| *m <= n) {
                            let (_, list) = c.marked_items.pop_front().expect("envoi");
                            items_ok.extend(list);
                        }
                    }
                    Event::TakenOver { .. } => {
                        log::debug!(
                            "construction ({}) : entré dans la province {:?} (taille {})",
                            self.pseudo,
                            c.info.province,
                            c.info.size
                        );
                        c.joined = true;
                        c.sent_chunk = None;
                        c.served_sent = None;
                        lists_changed = true;
                        if let Some(png) = &skin {
                            c.client.send(Cmd::SkinPut(png.clone()));
                        }
                    }
                    Event::Skin { pseudo, png } if !png.is_empty() => skins.push((pseudo, png)),
                    // (v147) Le pays appelle des garants parmi ses joueurs.
                    Event::Province(Response::GarantCall { call, .. }) => c.call = call,
                    Event::SetGamemode { .. }
                    | Event::Slain(_)
                    | Event::SetTime { .. }
                    | Event::SetWeather { .. }
                        if dozing && here == Some(*p) =>
                    {
                        notices.push(e)
                    }
                    // (v143) Relais de pays : les provinces qu'il confie, en vue.
                    Event::Provinces(list) => {
                        kids_lists.push((*p, list));
                        lists_changed = true;
                    }
                    Event::FromZone { chunk, data, .. } => received.push((*p, chunk, data)),
                    Event::Paused(on) => {
                        if c.paused && !on && c.joined {
                            ready.push(c.info.clone());
                        }
                        c.paused = on;
                    }
                    Event::Shards(list) => {
                        c.shards = list;
                        shards_changed = true;
                    }
                    // (v140) La voix de ceux qui parlent dans cette province.
                    Event::VoiceFrom { from, data } => voice.push((from, data)),
                    // (v165) Ping avec la province de là où l'on est.
                    Event::Rtt(ms) if here.is_some_and(|h| h == *p) => self.ping = ms,
                    Event::RoomClosed | Event::Error(_) | Event::Offline(_) | Event::Lost => {
                        log::info!("construction : relais de la province {:?} perdu", p.0);
                        broken.push(*p);
                        break;
                    }
                    _ => {}
                }
            }
            // Il doit savoir où l'on est (ce qu'il nous envoie).
            if c.joined && c.sent_chunk != Some(chunk) {
                c.sent_chunk = Some(chunk);
                if let Some(l) = &c.link {
                    transport::push(l, &Request::Position { chunk });
                }
            }
        }
        // Objets gardés par le relais du monde : ils entrent dans la partie.
        self.item_inbox.extend(items_ok);
        self.voice_in.extend(voice);
        self.skins_in.extend(skins);
        self.notices.extend(notices);
        for (serial, list) in confirmed {
            if let Some(m) = self.heard.get_mut(&serial) {
                for (pos, seq) in list {
                    if m.get(&pos).is_some_and(|h| h.seq <= seq) {
                        m.remove(&pos);
                    }
                }
            }
        }
        for (p, list) in kids_lists {
            self.children_of.insert(p, list);
        }
        // (v145) Ceux qui ne décident plus de rien en vue depuis un moment : quittés, s'il
        // en reste des nouvelles par d'autres (la liste qu'ils donnaient est dite aussi par
        // les provinces où l'on est entré).
        for (k, c) in self.provinces.iter_mut() {
            if needed.contains(k) {
                c.idle = 0.0;
            } else {
                c.idle += dt;
            }
        }
        let idle: Vec<PKey> = self
            .provinces
            .iter()
            .filter(|(_, c)| c.idle > LEAVE_AFTER)
            .map(|(k, _)| *k)
            .collect();
        for k in idle {
            let alone = self.children_of.get(&k).is_some_and(|list| {
                list.iter().any(|e| {
                    needed.contains(&pkey(e))
                        && !self
                            .children_of
                            .iter()
                            .any(|(s, l)| *s != k && l.iter().any(|x| pkey(x) == pkey(e)))
                })
            });
            if alone {
                continue;
            }
            if let Some(c) = self.provinces.remove(&k) {
                log::debug!(
                    "construction ({}) : province {:?} (taille {}) quittée, plus rien en vue",
                    self.pseudo,
                    k.0,
                    k.1
                );
                shards_changed |= !c.shards.is_empty();
                self.forget_shard(c.serial);
                self.children_of.remove(&k);
                lists_changed = true;
            }
        }
        let root: Vec<PKey> = self.root_list.iter().map(pkey).collect();
        for p in broken {
            // (v145) La liste d'une province tombée ne vaut plus rien (celle d'un pays reste
            // jusqu'à ce que son relais de secours en redonne une, v144).
            if !root.contains(&p) && self.children_of.remove(&p).is_some() {
                lists_changed = true;
            }
            if let Some(c) = self.provinces.remove(&p) {
                shards_changed |= !c.shards.is_empty();
                // Ce qu'elle a remis sans que le relais du monde l'ait gardé : en doute.
                self.forget_shard(c.serial);
            }
        }
        if lists_changed {
            self.merge_provinces();
        } else if shards_changed {
            self.rebuild_shards();
        }
        for (p, c, data) in received {
            let Some(info) = self.provinces.get(&p).map(|c| c.info.clone()) else {
                continue;
            };
            self.from_province = Some(info);
            dirty.extend(self.receive(0, c, &data, world));
        }
        self.from_province = None;
        // Un relais de province qui sert de nouveau (ou enfin) : ce qui attendait part.
        for info in ready {
            self.resend_in(|r| info.has(r));
        }
        dirty
    }

    /// Billet pour les relais de région, et certificat de la clé du compte.
    pub fn set_ticket(
        &mut self,
        room: u64,
        pseudo: String,
        expires: u64,
        sig: Vec<u8>,
        cert: Option<minelib_net::sovereign::Cert>,
    ) {
        self.shards.pseudo = pseudo.clone();
        self.shards.ticket = Some((room, pseudo, expires, sig, cert));
    }

    /// Clé du relais du monde (dans le billet) : vérifie les bonjours de la voix.
    pub fn set_root(&mut self, root: [u8; 32]) {
        self.root = Some(root);
    }

    /// Clé du compte : signe ses modifications dans une région souveraine.
    pub fn set_identity(&mut self, key: Option<minelib_net::ed25519_dalek::SigningKey>) {
        self.identity = key;
    }

    /// Le monde est repris par son relais de secours : tout part désormais par cette
    /// connexion ; les relais de région de l'ancien relais du monde sont quittés (ce
    /// qu'ils ont montré sans confirmation sera défait, voir `forget_shard`), le relais
    /// de secours renvoie ce qui manque.
    pub fn set_link(&mut self, link: Sender<Inbound>) {
        self.link = link;
        self.shards.list.clear();
        self.shards_root.clear();
        self.shards.ticket = None;
        // (Le relais repris sert tout lui-même : plus de provinces.)
        self.province_list.clear();
        self.root_list.clear();
        self.children_of.clear();
        self.provinces.clear();
        self.was_deep = false;
        self.root_province = None;
        self.served_sent.clear();
        let chunk = self.chunk.take();
        self.caps_sent = false;
        // Ce qui attendait encore sa réponse repart au relais de secours (rien ne se perd :
        // l'ancien relais du monde n'avait peut-être pas pu le lui transmettre).
        self.paused = false;
        self.resend_pending(chunk);
    }

    /// Monde en pause (son relais n'entend plus assez de gardiens) : les modifications du
    /// joueur attendent, ni envoyées ni annulées ; à la reprise, celles restées sans
    /// réponse repartent. Vrai si ça change.
    pub fn set_paused(&mut self, on: bool) -> bool {
        if on == self.paused {
            return false;
        }
        self.paused = on;
        if !on {
            self.resend_pending(self.chunk);
        }
        true
    }

    #[cfg(test)]
    pub fn is_paused(&self) -> bool {
        self.paused
    }

    /// Les modifications en attente repartent (une seule demande par case : la dernière).
    fn resend_pending(&mut self, chunk: Option<[i32; 2]>) {
        let list: Vec<([i32; 3], (u16, bool))> =
            self.pending.iter().map(|(p, v)| (*p, v.last)).collect();
        for (pos, (block, fluid)) in list {
            let signed = chunk.is_some_and(|c| self.send_signed(pos, block, fluid, c))
                || self.send_edit(pos, block, fluid);
            if let Some(p) = self.pending.get_mut(&pos) {
                p.count = 1;
                p.age = 0.0;
                p.signed = signed.then_some((block, fluid));
            }
        }
    }

    /// Région souveraine dont on a le relais : la modification y part signée (son hôte
    /// principal la met en ordre). Renvoie faux s'il faut passer par le relais du monde.
    fn send_signed(&mut self, pos: [i32; 3], block: u16, fluid: bool, at: [i32; 2]) -> bool {
        let chunk = [pos[0].div_euclid(16), pos[2].div_euclid(16)];
        let region = proto::region_of(chunk);
        let sovereign = self
            .shards
            .conns
            .get(&region)
            .is_some_and(|c| c.info.epoch > 0 && c.link.is_some());
        let (Some(key), Some((room, ..))) = (&self.identity, &self.shards.ticket) else {
            return false;
        };
        if !sovereign {
            return false;
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64);
        // Dates strictement croissantes (deux modifications dans la même milliseconde).
        self.sign_t = (self.sign_t + 1).max(now);
        let edit = minelib_net::sovereign::Edit::sign(
            key,
            *room,
            &self.pseudo,
            pos,
            block,
            fluid,
            at,
            self.sign_t,
            false,
        );
        let Some(l) = self.shards.link(region) else {
            return false;
        };
        transport::push(
            l,
            &Request::ToZone {
                chunk,
                data: game::encode(&BuildMsg::Signed(edit)),
            },
        )
    }

    /// Demande à celui qui décide des blocs de cette case : le relais du monde, ou le
    /// relais de sa province (v138 : signée par la clé du compte, il ne signe jamais à la
    /// place d'un joueur). Province pas encore prête : elle attend (renvoyée quand elle
    /// l'est).
    ///
    /// Vrai si elle est partie à un relais de province (sa confirmation reste en doute
    /// comme celle d'un relais de région : voir `Pending::signed`).
    fn send_edit(&mut self, pos: [i32; 3], block: u16, fluid: bool) -> bool {
        let chunk = chunk_of(pos[0], pos[2]);
        match self.authority(chunk) {
            Authority::Root => {
                self.send(Request::ToZone {
                    chunk,
                    data: game::encode(&BuildMsg::Edit { pos, block, fluid }),
                });
                false
            }
            Authority::Province(l) => {
                let at = self.chunk.unwrap_or(chunk);
                let data = match self.sign_edit(pos, block, fluid, at) {
                    Some(e) => game::encode(&BuildMsg::Signed(e)),
                    None => game::encode(&BuildMsg::Edit { pos, block, fluid }),
                };
                transport::push(&l, &Request::ToZone { chunk, data });
                true
            }
            Authority::Waiting => false,
        }
    }

    /// La modification signée par la clé du compte (avec le billet du monde).
    fn sign_edit(
        &mut self,
        pos: [i32; 3],
        block: u16,
        fluid: bool,
        at: [i32; 2],
    ) -> Option<minelib_net::sovereign::Edit> {
        let key = self.identity.as_ref()?;
        let (room, ..) = self.shards.ticket.as_ref()?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64);
        // Dates strictement croissantes (deux modifications dans la même milliseconde).
        self.sign_t = (self.sign_t + 1).max(now);
        Some(minelib_net::sovereign::Edit::sign(
            key,
            *room,
            &self.pseudo,
            pos,
            block,
            fluid,
            at,
            self.sign_t,
            false,
        ))
    }

    /// La dernière modification de `pos`, partie signée à un relais de région, est refusée
    /// par lui ou reste sans réponse : elle repart une fois au relais du monde, qui
    /// tranche (et la fait passer par l'hôte principal de la région, qui est remplacé
    /// s'il ne la met pas en ordre). Faux s'il n'y a rien à redemander.
    fn retry_at_root(&mut self, pos: [i32; 3]) -> bool {
        let Some((block, fluid)) = self.pending.get_mut(&pos).and_then(|p| {
            p.age = 0.0;
            p.signed.take()
        }) else {
            return false;
        };
        self.send_edit(pos, block, fluid);
        true
    }

    /// Relais de région rejoints (prêts).
    #[cfg(test)]
    pub fn shard_links(&self) -> usize {
        self.shards
            .conns
            .values()
            .filter(|c| c.link.is_some())
            .count()
    }

    /// Envoie au relais de la région de `chunk` (ou au relais du monde).
    /// Position : au relais de la région si l'on y est servi ; sinon (v139) au relais de sa
    /// province, entré ; sinon au relais du monde.
    fn send_at(&self, chunk: [i32; 2], req: Request) {
        if let Some(l) = self.shards.link(proto::region_of(chunk)) {
            transport::push(l, &req);
            return;
        }
        match self.province_link(chunk) {
            Some(l) => {
                transport::push(l, &req);
            }
            None => self.send(req),
        }
    }

    /// (v140) Tout ce qu'on a en vue de ce chunk est servi par des relais de province où
    /// l'on est entré.
    fn deep(&self, chunk: [i32; 2]) -> bool {
        if self.province_list.is_empty() {
            return false;
        }
        let lo = proto::region_of([chunk[0] - ROOT_VIEW, chunk[1] - ROOT_VIEW]);
        let hi = proto::region_of([chunk[0] + ROOT_VIEW, chunk[1] + ROOT_VIEW]);
        (lo[0]..=hi[0]).all(|x| {
            (lo[1]..=hi[1]).all(|z| {
                self.best([x, z])
                    .and_then(|i| self.provinces.get(&pkey(i)))
                    .is_some_and(|c| c.joined && c.link.is_some())
            })
        })
    }

    /// Relais de la province de ce chunk, s'il est entré (v139 : positions, positions de
    /// retour).
    fn province_link(&self, chunk: [i32; 2]) -> Option<&Sender<Inbound>> {
        let region = proto::region_of(chunk);
        let info = self.best(region)?;
        let c = self.provinces.get(&pkey(info))?;
        c.link.as_ref().filter(|_| c.joined)
    }

    /// Ce jeu a ouvert le monde (il l'enregistre).
    pub fn is_owner(&self) -> bool {
        self.owner
    }

    /// Le propriétaire est parti : ce jeu le devient (il enregistrera le monde).
    pub fn set_owner(&mut self) {
        self.owner = true;
    }

    /// Un autre joueur garde maintenant le monde (meilleure connexion) : on n'en est plus
    /// le propriétaire.
    pub fn clear_owner(&mut self) {
        self.owner = false;
    }

    fn send(&self, req: Request) {
        if self.dozing {
            // (v146) En veille : le relais du monde ne nous suit plus (positions, voix,
            // régions servies, retours : rien à lui dire) ; le reste attend qu'on y revienne.
            if !matches!(
                req,
                Request::Position { .. }
                    | Request::Served { .. }
                    | Request::Spot { .. }
                    | Request::Nearby { .. }
                    | Request::NearbyClose { .. }
                    | Request::VoiceTo { .. }
            ) {
                let mut q = self.doze_queue.borrow_mut();
                if q.len() < DOZE_QUEUE_MAX {
                    q.push(req);
                }
                self.wake_wanted.set(true);
            }
            return;
        }
        transport::push(&self.link, &req);
    }

    /// (v147) L'appel de garants valable là où l'on est (celui de son pays) : si ce joueur en
    /// est (`GarantCall::picks`), sa connexion de garant va chez ce pays.
    pub fn garant_call(&self) -> Option<&proto::GarantCall> {
        let chunk = self.chunk?;
        let info = self.best(proto::region_of(chunk))?;
        self.provinces.get(&pkey(info))?.call.as_ref()
    }

    /// (v146) La veille est possible : le jeu peut fermer sa connexion au relais du monde.
    pub fn doze_ready(&self) -> bool {
        !self.dozing && self.doze_t >= DOZE_AFTER
    }

    /// (v146) En veille, il faut revenir au relais du monde.
    pub fn wake_needed(&self) -> bool {
        self.dozing && self.wake_wanted.get()
    }

    pub fn is_dozing(&self) -> bool {
        self.dozing
    }

    /// Le monde (du billet).
    pub fn room(&self) -> Option<u64> {
        self.shards.ticket.as_ref().map(|t| t.0)
    }

    /// (v146) Veille commencée : la connexion au relais du monde est fermée.
    pub fn set_dozing(&mut self) {
        self.dozing = true;
        self.doze_t = 0.0;
        self.wake_t = 0.0;
        self.wake_wanted.set(false);
    }

    /// (v146) De retour au relais du monde, par cette connexion neuve (le même monde : les
    /// provinces restent). Il réapprend où l'on est et quelles régions nous servent ; ce qui
    /// attendait part.
    pub fn woke(&mut self, link: Sender<Inbound>) {
        self.link = link;
        self.dozing = false;
        self.doze_t = 0.0;
        self.wake_t = 0.0;
        self.wake_wanted.set(false);
        self.chunk = None;
        self.was_deep = false;
        self.root_province = None;
        self.served_sent = vec![[i32::MIN; 2]];
        self.caps_sent = false;
        for r in self.doze_queue.borrow_mut().drain(..) {
            transport::push(&self.link, &r);
        }
    }

    /// (v146) Veille possible ici : tout ce qu'on voit décidé par des provinces où l'on est
    /// entré, la sienne étant celle d'un pays ; un certificat pour revenir ; rien qui attende
    /// une réponse du relais du monde. En veille : retour quand une région en vue n'a plus
    /// de province, quand elles ne nous servent plus depuis un moment, ou avant que le
    /// certificat n'expire.
    fn doze_tick(&mut self, chunk: [i32; 2], deep: bool, dt: f32) {
        let cert_ok = self
            .shards
            .ticket
            .as_ref()
            .and_then(|t| t.4.as_ref())
            .is_some_and(|c| c.expires > now_secs() + DOZE_CERT_MIN);
        if !self.dozing {
            let in_pays = self.best(proto::region_of(chunk)).is_some_and(|i| {
                self.children_of
                    .values()
                    .any(|l| l.iter().any(|k| pkey(k) == pkey(i)))
            });
            let at_root = self
                .pending
                .keys()
                .any(|p| self.best(proto::region_of(chunk_of(p[0], p[2]))).is_none());
            let ok = deep && in_pays && cert_ok && !self.owner && !self.paused && !at_root;
            self.doze_t = if ok { self.doze_t + dt } else { 0.0 };
            return;
        }
        let lo = proto::region_of([chunk[0] - ROOT_VIEW, chunk[1] - ROOT_VIEW]);
        let hi = proto::region_of([chunk[0] + ROOT_VIEW, chunk[1] + ROOT_VIEW]);
        let root_needed =
            (lo[0]..=hi[0]).any(|x| (lo[1]..=hi[1]).any(|z| self.best([x, z]).is_none()));
        self.wake_t = if deep { 0.0 } else { self.wake_t + dt };
        if root_needed || !cert_ok || self.wake_t > WAKE_AFTER {
            self.wake_wanted.set(true);
        }
    }

    /// (v146) Son skin : déposé chez chaque province où l'on entre (les joueurs en veille l'y
    /// trouvent).
    pub fn set_skin(&mut self, png: Vec<u8>) {
        self.skin_png = Some(png);
    }

    pub fn has_skin(&self) -> bool {
        self.skin_png.is_some()
    }

    /// (v146) En veille : le skin d'un joueur, demandé aux provinces où l'on est entré.
    pub fn ask_skin(&self, pseudo: &str) {
        for c in self.provinces.values().filter(|c| c.joined) {
            c.client.send(Cmd::SkinGet(pseudo.to_string()));
        }
    }

    /// Skins reçus des provinces depuis le dernier appel : (pseudo, PNG).
    pub fn take_skins(&mut self) -> Vec<(String, Vec<u8>)> {
        std::mem::take(&mut self.skins_in)
    }

    /// (v146) Messages des OP arrivés par les provinces pendant la veille (mode de jeu, mort,
    /// heure) : à traiter comme ceux du relais du monde.
    pub fn take_notices(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.notices)
    }

    /// Demande d'objet (lâcher, ramasser, coffre) : à l'arbitre des objets de ce chunk, le
    /// relais du monde, ou (v139) le relais de sa province (jamais un relais de région).
    /// Province pas encore prête : refusée ici (ce qui a été lâché revient).
    pub fn send_item(&mut self, chunk: [i32; 2], req: &BuildMsg) {
        if !minelib_net::items::is_request(req) {
            return;
        }
        let data = game::encode(req);
        match self.authority(chunk) {
            Authority::Root => self.send(Request::ToZone { chunk, data }),
            Authority::Province(l) => {
                if let Some(seq) = minelib_net::items::request_seq(req) {
                    if self.item_asked.len() >= ITEM_ASKED_MAX {
                        self.item_asked.clear();
                    }
                    if let Some(p) = self.best(proto::region_of(chunk)).map(pkey) {
                        self.item_asked.insert(seq, p);
                    }
                }
                transport::push(&l, &Request::ToZone { chunk, data });
            }
            Authority::Waiting => {
                if let Some(seq) = minelib_net::items::request_seq(req) {
                    self.item_inbox.push(BuildMsg::Denied { seq });
                }
            }
        }
    }

    /// (v227) Paquet de notre foule d'essai (`minelib_net::crowd::pack`) : avec nos positions,
    /// au relais qui les suit (relais du monde, ou de notre province), qui le remet aux joueurs
    /// proches. Pas encore placé dans le monde : rien.
    pub fn send_crowd(&self, data: Vec<u8>) {
        let Some(chunk) = self.chunk else {
            return;
        };
        let msg = BuildMsg::Crowd {
            owner: self.pseudo.clone(),
            data,
        };
        self.send_at(
            chunk,
            Request::Nearby {
                chunk,
                data: game::encode(&msg),
            },
        );
    }

    /// (v227) Coup au PNJ `id` de la foule d'essai que mène `owner`, depuis nos yeux en `from` :
    /// au relais qui suit nos positions, qui le remet à `owner`.
    pub fn send_crowd_hit(&self, owner: &str, id: u16, from: [f32; 3]) {
        let Some(chunk) = self.chunk else {
            return;
        };
        let msg = BuildMsg::CrowdHit {
            owner: owner.to_string(),
            by: self.pseudo.clone(),
            id,
            from,
        };
        self.send_at(
            chunk,
            Request::Nearby {
                chunk,
                data: game::encode(&msg),
            },
        );
    }

    /// (v224) Demande de lumière de l'éditeur (`BuildMsg::LightPut`, d'un OP) : à l'arbitre
    /// de son chunk, comme une demande d'objet (le relais du monde, ou le relais de sa
    /// province). Province pas encore prête : refusée ici (la lumière revient comme les
    /// autres l'ont, voir `lightnet`).
    pub fn send_light(&mut self, req: &BuildMsg) {
        // (Derrière celles qui attendent : l'ordre des demandes compte.)
        if !self.light_queue.is_empty() || !self.try_light(req) {
            self.light_queue.push((req.clone(), 0.0));
        }
    }

    /// (v224) Envoie la demande de lumière à son arbitre, s'il est prêt.
    fn try_light(&mut self, req: &BuildMsg) -> bool {
        let BuildMsg::LightPut { seq, chunk, .. } = req else {
            return true;
        };
        let (seq, chunk) = (*seq, *chunk);
        match self.authority(chunk) {
            Authority::Root => {
                self.send(Request::ToZone {
                    chunk,
                    data: game::encode(req),
                });
                true
            }
            Authority::Province(l) => {
                if self.light_asked.len() >= ITEM_ASKED_MAX {
                    self.light_asked.clear();
                }
                if let Some(p) = self.best(proto::region_of(chunk)).map(pkey) {
                    self.light_asked.insert(seq, p);
                }
                transport::push(
                    &l,
                    &Request::ToZone {
                        chunk,
                        data: game::encode(req),
                    },
                );
                true
            }
            Authority::Waiting => false,
        }
    }

    /// (v229) Demande de foudre d'un OP (`BuildMsg::ThorAsk` : `/thor`, Foudre de Zeus) : à
    /// l'arbitre du chunk où elle tombe (le relais du monde, ou le relais de sa province),
    /// qui la publie aux joueurs proches. Arbitre pas prêt : refusée ici (un éclair
    /// n'attend pas).
    /// (v244) De même pour un coup de la Foudre de Zeus (`BuildMsg::ZeusAsk`, avec le
    /// laissez-passer du relais du monde).
    pub fn send_thor(&mut self, req: &BuildMsg) {
        let (BuildMsg::ThorAsk { seq, chunk, .. } | BuildMsg::ZeusAsk { seq, chunk, .. }) = req
        else {
            return;
        };
        let (seq, chunk) = (*seq, *chunk);
        let ask = Request::ToZone {
            chunk,
            data: game::encode(req),
        };
        match self.authority(chunk) {
            Authority::Root => self.send(ask),
            Authority::Province(l) => {
                if self.thor_asked.len() >= ITEM_ASKED_MAX {
                    self.thor_asked.clear();
                }
                if let Some(p) = self.best(proto::region_of(chunk)).map(pkey) {
                    self.thor_asked.insert(seq, p);
                }
                transport::push(&l, &ask);
            }
            Authority::Waiting => self.thor_inbox.push(BuildMsg::ThorDenied {
                seq,
                why: game::THOR_BUSY,
            }),
        }
    }

    /// (v224) Demandes de lumières qui attendent leur arbitre : envoyées dès qu'il est prêt,
    /// dans l'ordre ; refusées ici après `LIGHT_WAIT` s.
    fn flush_lights(&mut self, dt: f32) {
        if self.light_queue.is_empty() {
            return;
        }
        let queue = std::mem::take(&mut self.light_queue);
        let mut blocked: HashSet<[i32; 2]> = HashSet::new();
        for (req, age) in queue {
            let BuildMsg::LightPut { seq, chunk, .. } = &req else {
                continue;
            };
            let region = proto::region_of(*chunk);
            // (Une demande d'une région qui attend passe après les précédentes.)
            if !blocked.contains(&region) && self.try_light(&req) {
                continue;
            }
            let age = age + dt;
            if age >= LIGHT_WAIT {
                self.item_inbox.push(BuildMsg::LightDenied { seq: *seq });
            } else {
                blocked.insert(region);
                self.light_queue.push((req, age));
            }
        }
    }

    /// Propriétaire du monde : remet au relais les objets de sa sauvegarde (le relais
    /// refuse ce qui ferait double emploi).
    pub fn seed_items(&self, saved: &[BuildMsg]) {
        if !self.owner {
            return;
        }
        for m in saved {
            if let Some(chunk) = minelib_net::items::chunk_of_msg(m) {
                self.send(Request::ToZone {
                    chunk,
                    data: game::encode(m),
                });
            }
        }
    }

    /// Mon pseudo sur ce serveur (celui que le relais met dans ses réponses).
    /// (v165) Ping mesuré avec le relais du monde (s'il sert l'endroit où l'on est, pas de
    /// relais de province ou de région qui le mesure).
    pub fn set_world_ping(&mut self, ms: u16) {
        let served = self.chunk.is_some_and(|c| {
            self.shards.link(proto::region_of(c)).is_some() || self.province_link(c).is_some()
        });
        if !served {
            self.ping = ms;
        }
    }

    /// Son ping (ms ; 0 : pas encore mesuré).
    pub fn ping(&self) -> u16 {
        self.ping
    }

    pub fn pseudo(&self) -> &str {
        &self.pseudo
    }

    /// WorldEdit d'un OP : les blocs changés (case, nouveau, ancien), déjà posés ici, partent
    /// au relais par paquets (il vérifie qu'on est OP et les publie à tous).
    pub fn bulk(&mut self, cells: Vec<([i32; 3], BlockId, BlockId)>) {
        for part in cells.chunks(4096) {
            self.next_bulk += 1;
            let id = self.next_bulk;
            self.bulks
                .insert(id, part.iter().map(|&(p, _, old)| (p, old)).collect());
            self.send(Request::BulkEdit {
                id,
                cells: part.iter().map(|&(p, b, _)| (p, b.0)).collect(),
            });
        }
    }

    /// Réponse du relais à un paquet de `bulk` : refusé, on remet les blocs d'avant.
    /// Renvoie les chunks à remailler.
    pub fn bulk_done(&mut self, id: u32, error: &str, world: &mut World) -> Vec<ChunkPos> {
        let Some(olds) = self.bulks.remove(&id) else {
            return Vec::new();
        };
        if error.is_empty() {
            return Vec::new();
        }
        let mut dirty = Vec::new();
        for (p, old) in olds.into_iter().rev() {
            dirty.extend(world.set_remote(p[0], p[1], p[2], old));
        }
        dirty
    }

    /// Capacités de ce PC pour être garant (annoncées une fois).
    pub fn caps(fps: f32) -> proto::GarantCaps {
        proto::GarantCaps {
            cores: std::thread::available_parallelism().map_or(1, |n| n.get() as u16),
            fps: fps.clamp(0.0, 1000.0) as u16,
            ram_mb: crate::session::ram_view_limit().max(0) as u32 * 256,
        }
    }

    /// Modifications du joueur pas encore confirmées par le relais du monde.
    #[cfg(test)]
    pub fn pending(&self) -> usize {
        self.pending.len()
    }

    /// (v143) Provinces connues (coordonnées, taille, époque, entrée) : essais.
    #[cfg(test)]
    pub fn province_view(&self) -> Vec<([i32; 2], i32, u64, bool)> {
        let mut v: Vec<_> = self
            .province_list
            .iter()
            .map(|i| {
                (
                    i.province,
                    i.size,
                    i.epoch,
                    self.provinces.get(&pkey(i)).is_some_and(|c| c.joined),
                )
            })
            .collect();
        v.sort();
        v
    }

    /// Cases dont la dernière modification est arrivée par un relais de région.
    #[cfg(test)]
    pub fn via_shard(&self) -> usize {
        self.seen.values().filter(|v| v.1.is_some()).count()
    }

    /// À chaque image : modifications du joueur local (journal du monde), position,
    /// modifications refusées en silence (annulées), joueurs proches qui avancent en douceur.
    /// Renvoie les capacités à annoncer (une fois) et les chunks à remailler.
    #[allow(clippy::too_many_arguments)]
    pub fn update(
        &mut self,
        world: &mut World,
        feet: Vec3,
        look: (f32, f32),
        crouch: f32,
        speed: f32,
        fps: f32,
        dt: f32,
    ) -> (Option<proto::GarantCaps>, Vec<ChunkPos>) {
        let mut caps = None;
        if !self.caps_sent && fps > 0.0 {
            self.caps_sent = true;
            caps = Some(Self::caps(fps));
        }
        // Nouveau chunk : le relais du monde le sait avant de recevoir une demande de là
        // (il refuse une modification hors de portée de son auteur). (v140) Au fond des
        // provinces servies par un relais de province (tout ce qu'on a en vue), seulement
        // quand on change de province : c'est à leurs relais qu'on le dit.
        let chunk = chunk_of(feet.x.floor() as i32, feet.z.floor() as i32);
        let deep = self.deep(chunk);
        if self.chunk != Some(chunk) || deep != self.was_deep {
            self.chunk = Some(chunk);
            // (v143) La province (ou le pays) du relais du monde : c'est elle qu'il connaît.
            let province = self
                .root_list
                .iter()
                .find(|i| i.has(proto::region_of(chunk)))
                .map(pkey);
            if !deep || !self.was_deep || province != self.root_province {
                self.send(Request::Position { chunk });
                self.root_province = province;
            }
            self.was_deep = deep;
        }
        self.doze_tick(chunk, deep, dt);
        self.flush_lights(dt);
        // Ce que le joueur a changé (monde en pause : ça attend dans le journal).
        let edits = if self.paused {
            Vec::new()
        } else {
            world
                .journal
                .as_mut()
                .map(std::mem::take)
                .unwrap_or_default()
        };
        for (pos, id, old) in edits {
            // Une demande au relais du monde, qui accepte la modification et lui donne son
            // rang ; dans une région souveraine, signée au relais de la région, dont l'hôte
            // principal le fait (voir `minelib_net::sovereign`). Le bloc d'avant est gardé
            // pour un refus.
            let fluid = fluid_edit(old, id);
            // (Relais de région, ou de province : sa confirmation reste en doute.)
            let signed =
                self.send_signed(pos, id.0, fluid, chunk) || self.send_edit(pos, id.0, fluid);
            let p = self.pending.entry(pos).or_insert(Pending {
                old,
                count: 0,
                age: 0.0,
                signed: None,
                last: (id.0, fluid),
            });
            p.count += 1;
            p.age = 0.0;
            p.signed = signed.then_some((id.0, fluid));
            p.last = (id.0, fluid);
        }
        // Sans réponse du relais : la modification n'a pas eu lieu (en pause, on attend ;
        // de même dans une province dont le relais n'est pas prêt).
        let mut dirty = Vec::new();
        let mut expired = Vec::new();
        let waiting: Vec<PKey> = self
            .province_list
            .iter()
            .filter(|i| {
                self.provinces
                    .get(&pkey(i))
                    .is_none_or(|c| !c.joined || c.paused || c.link.is_none())
            })
            .map(pkey)
            .collect();
        for (pos, p) in &mut self.pending {
            if self.paused {
                break;
            }
            if !waiting.is_empty() {
                // (v143) La plus petite province qui la contient décide.
                let r = proto::region_of(chunk_of(pos[0], pos[2]));
                let best = self
                    .province_list
                    .iter()
                    .filter(|i| i.has(r))
                    .min_by_key(|i| i.size)
                    .map(pkey);
                if best.is_some_and(|b| waiting.contains(&b)) {
                    continue;
                }
            }
            p.age += dt;
            if p.age > PENDING_TIMEOUT {
                expired.push((*pos, p.old));
            }
        }
        // Blocs d'un relais de région quitté que le relais du monde n'a pas confirmés : ils
        // reviennent comme avant (sauf si la case a changé depuis).
        let mut undone = 0;
        let late: Vec<[i32; 3]> = self
            .doubtful
            .iter_mut()
            .filter_map(|(p, (_, t))| {
                *t -= dt;
                (*t <= 0.0).then_some(*p)
            })
            .collect();
        for pos in late {
            let Some((h, _)) = self.doubtful.remove(&pos) else {
                continue;
            };
            if self.pending.contains_key(&pos) || world.get(pos[0], pos[1], pos[2]) != h.got {
                continue;
            }
            // Une modification du joueur que ce relais avait confirmée : elle est redemandée
            // au relais du monde (rien ne se perd pour un joueur honnête, même quand l'hôte
            // de la région triche ou tombe).
            if let Some(fluid) = h.mine {
                let provisional = self.send_edit(pos, h.got.0, fluid);
                self.pending.insert(
                    pos,
                    Pending {
                        old: h.pre,
                        count: 1,
                        age: 0.0,
                        signed: provisional.then_some((h.got.0, fluid)),
                        last: (h.got.0, fluid),
                    },
                );
                continue;
            }
            undone += 1;
            // (v288) Défaite, elle n'est plus « déjà vue » : si un relais qui décide la
            // remet plus tard (au même rang), elle s'applique.
            self.seen.remove(&pos);
            dirty.extend(world.set_remote(pos[0], pos[1], pos[2], h.pre));
        }
        #[cfg(test)]
        {
            self.undone += undone;
        }
        self.net[4] += undone as u32;
        if undone > 0 {
            log::info!(
                "construction : {undone} blocs d'un relais de région quitté jamais confirmés par le relais du monde, remis comme avant"
            );
        }
        for (pos, old) in expired {
            if self.retry_at_root(pos) {
                log::info!("construction : pas de réponse du relais de région pour {pos:?}, demandé au relais du monde");
                continue;
            }
            self.pending.remove(&pos);
            self.net[3] += 1;
            log::info!("construction : pas de réponse du relais pour {pos:?}, annulé");
            dirty.extend(world.set_remote(pos[0], pos[1], pos[2], old));
        }
        // Relais de province en vue (v138) : connexions, messages reçus.
        dirty.extend(self.pump_provinces(world, chunk, dt));
        // Relais de région en vue : connexions, messages reçus.
        let before: Vec<u64> = self.shards.conns.values().map(|c| c.serial).collect();
        self.shards.keep_near(chunk);
        let mut received = Vec::new();
        let mut broken = Vec::new();
        for (region, c) in &mut self.shards.conns {
            while let Some(e) = c.client.poll() {
                match e {
                    Event::Link(l) => c.link = Some(l),
                    Event::FromZone { chunk, data, .. } => received.push((c.serial, chunk, data)),
                    Event::Paused(_) => self.relayed_beat = true,
                    Event::VoiceFrom { from, data } => self.voice_in.push((from, data)),
                    // (v165) Ping avec le relais de la région de là où l'on est.
                    Event::Rtt(ms) if *region == proto::region_of(chunk) => self.ping = ms,
                    Event::Error(m) | Event::Offline(m) => {
                        log::info!("relais de la région {region:?} : {m}");
                        broken.push(*region);
                    }
                    _ => {}
                }
            }
            // Chaque relais doit savoir où l'on est (qui doit recevoir quoi).
            if c.link.is_some() && c.sent_chunk != Some(chunk) {
                c.sent_chunk = Some(chunk);
                if let Some(l) = &c.link {
                    transport::push(l, &Request::Position { chunk });
                }
            }
        }
        for r in broken {
            // La liste sera remise à jour par le relais du monde ; en attendant, on passe
            // par un autre relais de la région s'il y en a, sinon par le relais du monde.
            if let Some(c) = self.shards.conns.remove(&r) {
                if let Some(l) = self.shards.list.get_mut(&r) {
                    l.retain(|i| *i != c.info);
                }
            }
            self.shards.list.retain(|_, l| !l.is_empty());
        }
        // Un relais de région quitté : ce qu'il a dit de l'ordre des modifications est
        // oublié (un relais malveillant ne laisse rien derrière lui : la suite, venue
        // d'ailleurs, s'applique normalement).
        for s in before {
            if !self.shards.conns.values().any(|c| c.serial == s) {
                self.forget_shard(s);
            }
        }
        // Le relais du monde sait quelles régions un relais de région nous sert (il ne
        // nous en renvoie pas les blocs) : à chaque changement.
        let mut served: Vec<[i32; 2]> = self
            .shards
            .conns
            .iter()
            .filter(|(_, c)| c.link.is_some() && c.sent_chunk.is_some())
            .map(|(r, _)| *r)
            .collect();
        served.sort_unstable();
        if served != self.served_sent {
            self.served_sent = served.clone();
            self.send(Request::Served {
                regions: served.clone(),
            });
        }
        // (Et à chaque relais de province, celles de ses régions.)
        for c in self.provinces.values_mut().filter(|c| c.joined) {
            let mine: Vec<[i32; 2]> = served.iter().copied().filter(|r| c.info.has(*r)).collect();
            if c.served_sent.as_ref() != Some(&mine) {
                if let Some(l) = &c.link {
                    transport::push(
                        l,
                        &Request::Served {
                            regions: mine.clone(),
                        },
                    );
                    c.served_sent = Some(mine);
                }
            }
        }
        for (serial, c, data) in received {
            self.from_shard = Some(serial);
            dirty.extend(self.receive(0, c, &data, world));
        }
        self.from_shard = None;
        self.pos_timer -= dt;
        self.spot_timer -= dt;
        self.close_timer -= dt;
        // (v285) Le relais qui reçoit nos positions est-il joint par les serveurs relais
        // d'iroh ? (Le relais du monde ne compte pas : rien ne change pour lui.)
        self.slow_link.0 -= dt;
        if self.slow_link.0 <= 0.0 {
            let node = match self.shards.conns.get(&proto::region_of(chunk)) {
                Some(c) if c.link.is_some() => Some(c.info.node),
                _ => self
                    .best(proto::region_of(chunk))
                    .and_then(|i| self.provinces.get(&pkey(i)))
                    .filter(|c| c.joined && c.link.is_some())
                    .map(|c| c.info.node),
            };
            let slow = node.is_some_and(|n| minelib_p2p::node_direct(n) == Some(false));
            self.slow_link = (1.0, slow);
        }
        // (v285) Jamais plus d'une position par 1/256 s, de l'une ou l'autre sorte
        // (`close_timer`) : le relais n'en accepte pas davantage.
        let mut sent = false;
        if self.pos_timer <= 0.0 && self.close_timer <= 0.0 && !self.hidden {
            // (v275) Le retard pris est reporté sur l'envoi suivant : à 300 images par
            // seconde il en part bien 256, et non une image sur deux (150).
            let slow = self.slow_link.1;
            self.pos_timer = (self.pos_timer + pos_every(self.others.len(), slow)).max(0.0);
            self.close_timer = (self.close_timer + POS_FASTEST).max(0.0);
            sent = true;
            self.net[0] += 1;
            let msg = BuildMsg::Player {
                pseudo: self.pseudo.clone(),
                pos: feet.to_array(),
                yaw: look.0,
                pitch: look.1,
                crouch,
                speed,
                ping: self.ping,
                held: minelib_net::poses::Held::pack(self.held, self.back),
            };
            self.send_at(
                chunk,
                Request::Nearby {
                    chunk,
                    data: game::encode(&msg),
                },
            );
            // Positions parties au relais de région : le relais du monde n'en sait plus
            // rien ; il garde tout de même où l'on est pour notre retour (toutes les 10 s).
            // (Dans une province, à son relais : il les regroupe pour le relais du monde.)
            if self.spot_timer <= 0.0 && self.shards.link(proto::region_of(chunk)).is_some() {
                self.spot_timer = SPOT_EVERY;
                let spot = Request::Spot {
                    pos: feet.to_array(),
                    yaw: look.0,
                    pitch: look.1,
                };
                match self.province_link(chunk) {
                    Some(l) => {
                        transport::push(l, &spot);
                    }
                    None => self.send(spot),
                }
            }
        }
        // (v285) Foule : entre deux positions pour tous, une position rapprochée par image
        // (256 par seconde au plus), que le relais ne remet qu'aux voisins les plus proches.
        if !sent
            && self.close_timer <= 0.0
            && !self.hidden
            && !self.slow_link.1
            && self.others.len() > POS_CLOSE
        {
            self.close_timer = (self.close_timer + POS_FASTEST).max(0.0);
            self.net[1] += 1;
            let msg = BuildMsg::Player {
                pseudo: self.pseudo.clone(),
                pos: feet.to_array(),
                yaw: look.0,
                pitch: look.1,
                crouch,
                speed,
                ping: self.ping,
                held: minelib_net::poses::Held::pack(self.held, self.back),
            };
            self.send_at(
                chunk,
                Request::NearbyClose {
                    chunk,
                    data: game::encode(&msg),
                },
            );
        }
        // (v287) Une ligne par minute au journal : de quoi constater un problème de réseau
        // (positions qui n'arrivent pas, lien lent, modifications sans réponse).
        let since = self.net_since.elapsed().as_secs_f32();
        if since >= 60.0 {
            let link = if self.shards.link(proto::region_of(chunk)).is_some()
                || self.province_link(chunk).is_some()
            {
                if self.slow_link.1 {
                    "relais de région ou de province, par les serveurs relais d'iroh"
                } else {
                    "relais de région ou de province, en direct"
                }
            } else {
                "relais du monde"
            };
            log::info!(
                "réseau (construction, {since:.0} s) : {:.0} positions envoyées par seconde \
                 (dont {:.0} rapprochées), {:.0} reçues par seconde de {} voisins ; {link} ; \
                 {} modifications annulées faute de réponse, {} blocs remis comme avant, {} \
                 en attente",
                (self.net[0] + self.net[1]) as f32 / since,
                self.net[1] as f32 / since,
                self.net[2] as f32 / since,
                self.others.len(),
                self.net[3],
                self.net[4],
                self.pending.len(),
            );
            self.net = [0; 5];
            self.net_since = std::time::Instant::now();
        }
        for o in self.others.values_mut() {
            o.seen += dt;
            // Rejoint la dernière position reçue en un intervalle : vitesse régulière même
            // pour un joueur lointain (moins de messages).
            let k = (dt / o.interval.max(POS_FASTEST)).min(1.0);
            o.pos += (o.target - o.pos) * k;
        }
        self.others
            .retain(|_, o| o.seen < STALE_S.max(o.interval * 3.0));
        (caps, dirty)
    }

    /// Voix (v137) : par le relais de région de là où l'on est, s'il est relié (il la
    /// remet à ceux de ses joueurs qui doivent l'entendre, et aux autres relais de la
    /// région pour les leurs) ; sinon (v140) par le relais de sa province, où entrent tous
    /// ceux qui l'ont en vue ; sinon par le relais du monde.
    ///
    /// (v165) En veille (plus de connexion au relais du monde), si la province de là où l'on
    /// est change d'hôte (quelques secondes sans relais) : par le relais de la province la
    /// plus proche où l'on est entré, que les joueurs d'à côté ont rejoint aussi (chacun
    /// rejoint toutes celles en vue). Sans cela, la voix se perdait le temps de la reprise.
    pub fn send_voice(&self, to: Vec<String>, data: Vec<u8>) {
        if to.is_empty() {
            return;
        }
        let req = Request::VoiceTo { to, data };
        let here = self.chunk.and_then(|c| {
            self.shards
                .link(proto::region_of(c))
                .or_else(|| self.province_link(c))
        });
        match here {
            Some(l) => {
                transport::push(l, &req);
            }
            None if self.dozing => {
                if let Some(l) = self.chunk.and_then(|c| self.nearest_province_link(c)) {
                    transport::push(l, &req);
                }
            }
            None => self.send(req),
        }
    }

    /// (v165) Relais de la province la plus proche du chunk `chunk` où l'on est entré (la
    /// plus petite à distance égale).
    fn nearest_province_link(&self, chunk: [i32; 2]) -> Option<&Sender<Inbound>> {
        let region = proto::region_of(chunk);
        self.provinces
            .values()
            .filter(|c| c.joined)
            .filter_map(|c| c.link.as_ref().map(|l| (c, l)))
            .min_by_key(|(c, _)| {
                let s = c.info.size.max(1);
                let lo = c.info.first_region();
                let hi = [lo[0] + s - 1, lo[1] + s - 1];
                let dx = (lo[0] - region[0]).max(region[0] - hi[0]).max(0);
                let dz = (lo[1] - region[1]).max(region[1] - hi[1]).max(0);
                (dx.max(dz), c.info.size)
            })
            .map(|(_, l)| l)
    }

    /// Voix arrivée par les relais de région depuis le dernier appel : (qui parle,
    /// message).
    pub fn take_voice(&mut self) -> Vec<(String, Vec<u8>)> {
        std::mem::take(&mut self.voice_in)
    }

    /// De quoi signer et vérifier les bonjours de la voix : clé du compte, certificat et
    /// monde (du billet), clé du relais du monde.
    pub fn voice_keys(
        &self,
    ) -> Option<(
        minelib_net::ed25519_dalek::SigningKey,
        minelib_net::sovereign::Cert,
        u64,
        [u8; 32],
    )> {
        let key = self.identity.clone()?;
        let (room, _, _, _, cert) = self.shards.ticket.as_ref()?;
        Some((key, cert.clone()?, *room, self.root?))
    }

    /// Un relais de région a redit le battement du relais du monde depuis le dernier appel
    /// (le relais du monde est en vie : rien à chercher chez son relais de secours).
    pub fn take_relayed_beat(&mut self) -> bool {
        std::mem::take(&mut self.relayed_beat)
    }

    /// Zones gagnées ou perdues (annoncé par le relais).
    pub fn zones_changed(&mut self, gained: &[[i32; 2]], lost: &[[i32; 2]]) -> Option<String> {
        for z in lost {
            self.zones.remove(z);
        }
        for z in gained {
            self.zones.insert(*z);
        }
        log::info!("garant : {} zone(s)", self.zones.len());
        None
    }

    /// Message d'un autre joueur ou d'un garant. Renvoie les chunks à remailler.
    pub fn receive(
        &mut self,
        peer: u64,
        _chunk: [i32; 2],
        data: &[u8],
        world: &mut World,
    ) -> Vec<ChunkPos> {
        let Some(msg) = game::decode::<BuildMsg>(data) else {
            return Vec::new();
        };
        match msg {
            // Les demandes ne vont qu'aux relais.
            BuildMsg::Edit { .. } | BuildMsg::Signed(_) => Vec::new(),
            BuildMsg::Set {
                pos, block, seq, ..
            } => self.block_set(pos, block, seq, world),
            // Blocs regroupés : comme autant de modifications.
            BuildMsg::Cells(packet) => {
                let mut dirty = Vec::new();
                for (pos, block, seq) in
                    minelib_net::cells::unpack_cells(&packet).unwrap_or_default()
                {
                    for c in self.block_set(pos, block, seq, world) {
                        if !dirty.contains(&c) {
                            dirty.push(c);
                        }
                    }
                }
                dirty
            }
            // Objets au sol et coffres : états publiés par le relais du monde, ou (v139) par
            // le relais d'une province pour ses zones (jamais par un joueur, `peer` 0, ni par
            // un relais de région). (v224) Les lumières de l'éditeur aussi.
            BuildMsg::Item { .. }
            | BuildMsg::Slot { .. }
            | BuildMsg::Denied { .. }
            | BuildMsg::Light { .. }
            | BuildMsg::LightDenied { .. } => {
                if peer != 0 || self.from_shard.is_some() {
                    return Vec::new();
                }
                let Some(info) = self.from_province.clone() else {
                    self.item_inbox.push(msg);
                    return Vec::new();
                };
                // Un refus, seulement d'une demande qui lui a été faite : tout de suite.
                if let BuildMsg::Denied { seq } = msg {
                    if self.item_asked.get(&seq) == Some(&pkey(&info)) {
                        self.item_asked.remove(&seq);
                        self.item_inbox.push(msg);
                    }
                    return Vec::new();
                }
                if let BuildMsg::LightDenied { seq } = msg {
                    if self.light_asked.get(&seq) == Some(&pkey(&info)) {
                        self.light_asked.remove(&seq);
                        self.item_inbox.push(msg);
                    }
                    return Vec::new();
                }
                // Un état de ses zones : quand le relais du monde l'aura gardé.
                if minelib_net::items::chunk_of_msg(&msg)
                    .is_some_and(|c| info.has(proto::region_of(c)) && self.sender_decides(c))
                {
                    if let Some(c) = self.provinces.get_mut(&pkey(&info)) {
                        c.unmarked_items.push(msg);
                    }
                }
                Vec::new()
            }
            // (v229) Foudre d'un OP, publiée par l'arbitre du chunk où elle tombe : le relais
            // du monde, ou le relais d'une province pour ses zones (jamais un joueur ni un
            // relais de région). Rien à garder : elle part tout de suite.
            // (v244) De même un coup de la Foudre de Zeus.
            BuildMsg::Thor { chunk: at, .. } | BuildMsg::Zeus { chunk: at, .. } => {
                if peer != 0 || self.from_shard.is_some() {
                    return Vec::new();
                }
                let ours = match self.from_province.clone() {
                    None => true,
                    Some(info) => info.has(proto::region_of(at)) && self.sender_decides(at),
                };
                if ours {
                    self.thor_inbox.push(msg);
                }
                Vec::new()
            }
            // Un refus : du relais du monde, ou de la province à qui la demande a été faite.
            BuildMsg::ThorDenied { seq, .. } => {
                if peer != 0 || self.from_shard.is_some() {
                    return Vec::new();
                }
                match self.from_province.clone() {
                    None => self.thor_inbox.push(msg),
                    Some(info) => {
                        if self.thor_asked.get(&seq) == Some(&pkey(&info)) {
                            self.thor_asked.remove(&seq);
                            self.thor_inbox.push(msg);
                        }
                    }
                }
                Vec::new()
            }
            // Les demandes ne vont qu'au relais.
            BuildMsg::DropItem { .. }
            | BuildMsg::TakeItem { .. }
            | BuildMsg::ChestPut { .. }
            | BuildMsg::ChestTake { .. }
            | BuildMsg::LightPut { .. }
            | BuildMsg::ThorAsk { .. }
            | BuildMsg::ZeusAsk { .. } => Vec::new(),
            // (v227) Foule d'essai d'un OP, coups à la nôtre, refus de la nôtre : remis par un
            // relais (jamais par un joueur, `peer` 0).
            BuildMsg::Crowd { owner, data } => {
                if peer == 0 && !owner.eq_ignore_ascii_case(&self.pseudo) {
                    self.crowd_inbox.push((owner, data));
                }
                Vec::new()
            }
            BuildMsg::CrowdHit {
                owner, by, id, from,
            } => {
                if peer == 0 && owner.eq_ignore_ascii_case(&self.pseudo) {
                    self.crowd_hits.push((by, id, from));
                }
                Vec::new()
            }
            BuildMsg::CrowdDenied => {
                if peer == 0 {
                    self.crowd_denied = true;
                }
                Vec::new()
            }
            BuildMsg::Reject { pos, .. } => {
                // (Un relais de province ne refuse que dans ses zones.)
                if !self.sender_decides(chunk_of(pos[0], pos[2])) {
                    return Vec::new();
                }
                // Refusé par le relais d'une région souveraine (son hôte principal vient de
                // changer, ou il ne l'est plus) : la demande repart au relais du monde.
                if self.from_shard.is_some() && self.retry_at_root(pos) {
                    return Vec::new();
                }
                // Refusé : le bloc d'avant revient.
                match self.pending.remove(&pos) {
                    // (Sauf s'il a déjà rechangé la case : voir `rebase_journal`.)
                    Some(p) if rebase_journal(world, pos, p.old) => Vec::new(),
                    Some(p) => world.set_remote(pos[0], pos[1], pos[2], p.old),
                    None => Vec::new(),
                }
            }
            BuildMsg::Player {
                pseudo,
                pos,
                yaw,
                pitch,
                crouch,
                speed,
                ping,
                held,
            } => {
                let pose = minelib_net::poses::Pose {
                    pos,
                    yaw,
                    pitch,
                    crouch,
                    speed,
                    ping: (ping > 0).then_some(ping),
                    held,
                };
                // Numéro sur le relais du monde (le vocal passe par lui).
                self.player_seen((peer != 0).then_some(peer), &pseudo, pose);
                Vec::new()
            }
            // Positions regroupées : comme autant de `Player`. Le pseudo n'est dit que de
            // temps en temps ; entre-temps, le numéro du joueur sur ce relais suffit.
            BuildMsg::Poses(packet) => {
                // (Numéros propres à chaque relais : région, province, ou celui du monde.)
                let from = self.from_shard.or_else(|| {
                    self.from_province
                        .as_ref()
                        .and_then(|i| self.provinces.get(&pkey(i)))
                        .map(|c| c.serial)
                });
                for (id, name, pose) in
                    minelib_net::poses::unpack_poses(&packet).unwrap_or_default()
                {
                    if let Some(n) = name {
                        if self.names.len() >= NAMES_MAX {
                            self.names.clear();
                        }
                        self.names.insert((from, id), n);
                    }
                    let Some(pseudo) = self.names.get(&(from, id)).cloned() else {
                        continue;
                    };
                    // (Le numéro n'est celui du relais du monde que s'il vient de lui.)
                    self.player_seen(from.is_none().then_some(id), &pseudo, pose);
                }
                Vec::new()
            }
        }
    }

    /// Un autre joueur est vu à cette position (`home` : son numéro sur le relais du monde,
    /// si c'est lui qui le dit).
    fn player_seen(&mut self, home: Option<u64>, pseudo: &str, p: minelib_net::poses::Pose) {
        // Valeurs d'un autre joueur : rien d'infini ni d'absurde n'entre dans le jeu.
        if !p.sane() {
            return;
        }
        self.net[2] += 1;
        let target = Vec3::from(p.pos);
        let key: String = pseudo.chars().take(16).collect();
        match self.others.get_mut(&key) {
            Some(o) => {
                if home.is_some() {
                    o.id = home;
                }
                // Intervalle mesuré (lissé) ; téléportation : on y va d'un coup.
                o.interval = o.interval * 0.7 + o.seen.clamp(POS_FASTEST, 4.0) * 0.3;
                if (target - o.pos).length() > 8.0 {
                    o.pos = target;
                }
                o.target = target;
                o.yaw = p.yaw;
                o.pitch = p.pitch;
                o.crouch = p.crouch;
                o.speed = p.speed;
                o.seen = 0.0;
                if p.ping.is_some() {
                    o.ping = p.ping;
                }
                o.held = minelib_net::poses::Held::of(p.held);
                o.back = minelib_net::poses::Held::back(p.held);
            }
            None => {
                self.others.insert(
                    key.clone(),
                    Other {
                        pseudo: key,
                        id: home,
                        pos: target,
                        target,
                        interval: POS_EVERY,
                        yaw: p.yaw,
                        pitch: p.pitch,
                        crouch: p.crouch,
                        speed: p.speed,
                        seen: 0.0,
                        ping: p.ping,
                        held: minelib_net::poses::Held::of(p.held),
                        back: minelib_net::poses::Held::back(p.held),
                    },
                );
            }
        }
    }

    /// Le relais de cette région n'est plus le nôtre : ce qu'il a dit de l'ordre des
    /// modifications est oublié.
    fn forget_shard(&mut self, serial: u64) {
        self.seen.retain(|_, v| v.1 != Some(serial));
        self.names.retain(|k, _| k.0 != Some(serial));
        // Ce qu'il a remis sans que le relais du monde le confirme : en doute. Le relais du
        // monde renvoie tout ce qui manque de la région (`Request::Served`) ; ce qu'il ne
        // confirme pas (un bloc inventé par un hôte malveillant, montré à ses seuls
        // joueurs) revient comme avant.
        for (pos, h) in self.heard.remove(&serial).unwrap_or_default() {
            self.doubtful.insert(pos, (h, DOUBT_WAIT));
        }
    }

    /// Le relais du monde publie `pos` au rang `seq` : ce qu'un relais de région en avait
    /// remis, à ce rang ou avant, est confirmé (ou dépassé).
    fn settle(&mut self, pos: [i32; 3], seq: u64) {
        if self.doubtful.get(&pos).is_some_and(|h| h.0.seq <= seq) {
            self.doubtful.remove(&pos);
        }
        for m in self.heard.values_mut() {
            if m.get(&pos).is_some_and(|h| h.seq <= seq) {
                m.remove(&pos);
            }
        }
    }

    /// Un relais de région remet `block` en `pos` : gardé, avec le bloc d'avant, jusqu'à ce
    /// que le relais du monde le confirme.
    fn note_heard(&mut self, serial: u64, pos: [i32; 3], block: u16, seq: u64, world: &World) {
        // (Le bloc d'avant d'origine : celui d'avant une modification en attente du joueur,
        // ou d'avant un relais déjà quitté.)
        let pre = match self.doubtful.remove(&pos) {
            Some((h, _)) => h.pre,
            None => self
                .pending
                .get(&pos)
                .map_or_else(|| world.get(pos[0], pos[1], pos[2]), |p| p.old),
        };
        // (La confirmation de sa propre modification signée.)
        let mine = self
            .pending
            .get(&pos)
            .and_then(|p| p.signed)
            .filter(|s| s.0 == block)
            .map(|s| s.1);
        let m = self.heard.entry(serial).or_default();
        let full = m.len() >= HEARD_MAX;
        match m.get_mut(&pos) {
            Some(h) => {
                h.got = BlockId(block);
                h.seq = seq;
                h.mine = mine;
            }
            None if !full => {
                m.insert(
                    pos,
                    Heard {
                        pre,
                        got: BlockId(block),
                        seq,
                        mine,
                    },
                );
            }
            None => {}
        }
    }

    /// Le relais du monde a publié `block` en `pos`, au rang `seq` de l'ordre du monde.
    /// Renvoie les chunks à remailler.
    fn block_set(
        &mut self,
        pos: [i32; 3],
        block: u16,
        seq: u64,
        world: &mut World,
    ) -> Vec<ChunkPos> {
        if block as usize >= minelib_world::registry::registry().len() {
            return Vec::new();
        }
        // Un relais de province ne décide que des blocs de ses zones (v143 : celles que
        // ne décide pas une de ses provinces).
        if !self.sender_decides(chunk_of(pos[0], pos[2])) {
            return Vec::new();
        }
        // (Un relais de province décide, mais ce qu'il remet reste en doute jusqu'à ce que
        // le relais du monde l'ait gardé : voir `ProvinceConfirmed`.)
        let province = self
            .from_province
            .as_ref()
            .and_then(|i| self.provinces.get(&pkey(i)))
            .map(|c| c.serial);
        let source = self.from_shard.or(province);
        if source.is_none() {
            self.settle(pos, seq);
        }
        // L'ordre du relais du monde : plus ancienne que ce qu'on a déjà pour cette case
        // (doublon, même modification arrivée par deux chemins), elle est sans effet.
        if self.seen.get(&pos).is_some_and(|s| s.0 >= seq) {
            if self.pending.contains_key(&pos) {
                log::debug!(
                    "construction ({}) : {pos:?} au rang {seq} déjà vu au rang {:?}",
                    self.pseudo,
                    self.seen.get(&pos)
                );
            }
            // (v288) La même modification (même rang, même bloc), remise par un autre
            // relais de région ou de province que celui par qui on l'a eue : c'est lui
            // qui en répond maintenant. Sans cela, quitter le premier la mettait en doute
            // et la défaisait 3 s plus tard, alors que le second, qui sert la zone à sa
            // place, l'avait bien remise (et ne la redonnerait jamais).
            if let (Some(new), Some(&(old_seq, Some(old)))) = (source, self.seen.get(&pos)) {
                let same = self
                    .heard
                    .get(&old)
                    .and_then(|m| m.get(&pos))
                    .is_some_and(|h| h.seq == seq && h.got == BlockId(block));
                if old_seq == seq && old != new && same {
                    if let Some(h) = self.heard.get_mut(&old).and_then(|m| m.remove(&pos)) {
                        self.heard.entry(new).or_default().insert(pos, h);
                        self.seen.insert(pos, (seq, Some(new)));
                        if let Some(c) = self
                            .from_province
                            .as_ref()
                            .and_then(|i| self.provinces.get_mut(&pkey(i)))
                        {
                            c.unmarked.push((pos, seq));
                        }
                    }
                }
            }
            return Vec::new();
        }
        if self.seen.len() >= SEEN_MAX {
            self.seen.clear();
        }
        self.seen.insert(pos, (seq, source));
        if let Some(s) = source {
            self.note_heard(s, pos, block, seq, world);
        }
        if let Some(c) = self
            .from_province
            .as_ref()
            .and_then(|i| self.provinces.get_mut(&pkey(i)))
        {
            c.unmarked.push((pos, seq));
        }
        // Ce que publie le relais fait foi (et confirme une modification en attente).
        self.confirm(pos, BlockId(block));
        // Le joueur a déjà rechangé cette case (pas encore envoyé) : sa nouvelle
        // modification reste à l'écran, et part de ce bloc-ci.
        if rebase_journal(world, pos, BlockId(block)) {
            return Vec::new();
        }
        world.set_remote(pos[0], pos[1], pos[2], BlockId(block))
    }

    /// Une modification de cette case est confirmée.
    /// Le relais vient de publier `block` en `pos` : c'est désormais le bloc qui revient
    /// si une modification encore en attente à cet endroit est refusée (et non celui
    /// d'avant la première : deux modifications de suite dont la seconde est refusée, ou
    /// celle d'un autre joueur passée entre-temps, laissaient un monde différent).
    fn confirm(&mut self, pos: [i32; 3], block: BlockId) {
        if let Some(p) = self.pending.get_mut(&pos) {
            p.old = block;
            p.count = p.count.saturating_sub(1);
            if p.count == 0 {
                self.pending.remove(&pos);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use minelib_net::client::{Client, Cmd, Event};
    use std::time::{Duration, Instant};

    /// (v275) 256 positions par seconde avec 3 voisins au plus ; au-delà et seul, comme
    /// avant (60, puis de moins en moins dans une foule, jamais moins de 10).
    #[test]
    fn a_position_goes_256_times_a_second_to_three_neighbours_at_most() {
        let hz = |others: usize| (1.0 / pos_every(others, false)).round() as u32;
        assert_eq!([hz(1), hz(2), hz(3)], [256, 256, 256]);
        // Par le relais d'une région ou d'une province (hébergé par un joueur) : 60.
        assert_eq!((1.0 / pos_every(2, true)).round() as u32, 60);
        assert_eq!([hz(0), hz(4), hz(16), hz(32), hz(96), hz(3000)], [60, 60, 60, 30, 10, 10]);
        // Chacun reçoit au plus `POS_BUDGET` positions par seconde, à tout nombre de voisins.
        for others in 1..200 {
            let got = hz(others) as f32 * others as f32;
            assert!(got <= POS_BUDGET + 100.0 || hz(others) == 10, "{others} voisins : {got}");
        }
    }

    /// Un joueur simulé : sa connexion, son monde, ce qu'il a changé.
    struct Bot {
        name: String,
        c: Client,
        net: Option<BuildNet>,
        link: Option<Sender<Inbound>>,
        world: World,
        pos: Vec3,
        rejects: u32,
        /// Relais de région hébergé (connexion de garant).
        shard: Option<crate::garant_worker::ShardHost>,
        /// A rejoint au moins un relais de région.
        used_shard: bool,
        /// Clé du compte (reçue après la preuve du mot de passe).
        identity: Option<minelib_net::ed25519_dalek::SigningKey>,
        /// Relais de secours tenu par ce joueur (connexion de garant).
        standby: Option<crate::garant_worker::StandbyHost>,
        /// Où est le relais de secours de son monde ; reprise en cours (prochain essai) ;
        /// reprises réussies.
        standby_at: Option<minelib_net::proto::StandbyAt>,
        retake: Option<Instant>,
        taken: u32,
        /// Témoin de mondes (connexion de garant).
        witnesses: HashMap<u64, crate::garant_worker::WitnessRole>,
        /// Monde en pause : connexion d'essai au relais de secours (sans quitter le relais
        /// du monde), prochain essai ; pauses vues.
        probe: Option<Client>,
        probe_next: Option<Instant>,
        pauses: u32,
        /// Mondes pour lesquels il a voté la reprise (il n'y redevient jamais gardien).
        voted: crate::garant_worker::VoteBook,
        /// Connexion de garant entrée par certificat chez un relais repris : quand ce
        /// relais tombe, elle n'a rien à faire (pas d'erreur fatale).
        follower: bool,
        /// Dernier certificat de la clé du compte reçu (comme `Accounts` : il survit à une
        /// reprise, le billet des relais de région non).
        cert: Option<minelib_net::sovereign::Cert>,
        /// Lots d'autres régions vérifiés pour le relais du monde.
        audits: u32,
        /// Voix (sans micro : bonjours et clés seulement), acheminée comme dans le jeu.
        voice: Option<crate::voice::Voice>,
        /// Provinces (v138, connexion de garant) : relais de province tenu par ce joueur,
        /// et relais de région rattaché au relais de sa province.
        province: Option<crate::province_host::ProvinceHost>,
        attach: Option<crate::province_host::RegionAttach>,
        /// (v146) Veille permise (comme `Accounts`) ; relais du monde où revenir ; demande
        /// partie, retour en cours ; veilles et retours faits ; messages des OP arrivés par
        /// les provinces (modes, morts).
        doze: bool,
        relay: (String, [u8; 32]),
        doze_sent: bool,
        waking: bool,
        dozed: u32,
        woken: u32,
        modes_seen: Vec<String>,
        slain: u32,
        times_seen: Vec<f32>,
        /// (v229) Météos du serveur apprises en veille (par les provinces).
        weather_seen: Vec<u8>,
        /// (v229) Foudres publiées par les arbitres et refus reçus (`BuildNet::thor_inbox`).
        thor_seen: Vec<BuildMsg>,
    }

    /// Chunks du terrain d'essai : 2 zones de part et d'autre d'une frontière.
    const CHUNKS: std::ops::Range<i32> = 2..6;

    fn flat() -> World {
        let mut w = World::new(0);
        for cx in CHUNKS {
            for cz in 0..2 {
                w.insert_empty(ChunkPos { x: cx, z: cz });
            }
        }
        for x in CHUNKS.start * 16..CHUNKS.end * 16 {
            for z in 0..32 {
                w.set(x, 10, z, BlockId::STONE);
            }
        }
        w.journal = Some(Vec::new());
        w
    }

    impl Bot {
        fn new(addr: &str, key: [u8; 32], name: &str, garant: bool) -> Self {
            let c = if garant {
                Client::spawn_garant(addr.into(), key)
            } else {
                Client::spawn(addr.into(), Some(key))
            };
            if garant {
                c.send(Cmd::Login {
                    pseudo: name.into(),
                    password: "motdepasse".into(),
                    totp: None,
                    backup: None,
                    device: None,
                });
            } else {
                c.send(Cmd::Register {
                    pseudo: name.into(),
                    password: "motdepasse".into(),
                });
            }
            let mut b = Bot {
                name: name.into(),
                c,
                net: None,
                link: None,
                world: if garant { World::new(0) } else { flat() },
                pos: Vec3::new(64.0, 11.0, 16.0),
                rejects: 0,
                shard: None,
                used_shard: false,
                identity: None,
                standby: None,
                standby_at: None,
                retake: None,
                taken: 0,
                witnesses: HashMap::new(),
                probe: None,
                probe_next: None,
                pauses: 0,
                voted: crate::garant_worker::VoteBook::open(None),
                follower: false,
                cert: None,
                audits: 0,
                voice: None,
                province: None,
                attach: None,
                doze: false,
                relay: (addr.to_string(), key),
                doze_sent: false,
                waking: false,
                dozed: 0,
                woken: 0,
                modes_seen: Vec::new(),
                slain: 0,
                times_seen: Vec::new(),
                weather_seen: Vec::new(),
                thor_seen: Vec::new(),
            };
            b.wait(|e| matches!(e, Event::LoggedIn { .. }));
            b
        }

        /// La connexion de garant d'un joueur suit son monde chez le relais qui l'a repris,
        /// avec le certificat de la clé de son compte (comme `garant_worker`).
        fn cert_garant(
            at: &minelib_net::proto::StandbyAt,
            name: &str,
            cert: minelib_net::sovereign::Cert,
            key: minelib_net::ed25519_dalek::SigningKey,
        ) -> Self {
            let c = Client::spawn_garant(at.addr.clone(), at.key);
            c.send(Cmd::GarantLogin { cert, key });
            let mut b = Bot {
                name: name.into(),
                c,
                net: None,
                link: None,
                world: World::new(0),
                pos: Vec3::new(64.0, 11.0, 16.0),
                rejects: 0,
                shard: None,
                used_shard: false,
                identity: None,
                standby: None,
                standby_at: None,
                retake: None,
                taken: 0,
                witnesses: HashMap::new(),
                probe: None,
                probe_next: None,
                pauses: 0,
                voted: crate::garant_worker::VoteBook::open(None),
                follower: true,
                cert: None,
                audits: 0,
                voice: None,
                province: None,
                attach: None,
                doze: false,
                relay: (at.addr.clone(), at.key),
                doze_sent: false,
                waking: false,
                dozed: 0,
                woken: 0,
                modes_seen: Vec::new(),
                slain: 0,
                times_seen: Vec::new(),
                weather_seen: Vec::new(),
                thor_seen: Vec::new(),
            };
            b.wait(|e| matches!(e, Event::LoggedIn { .. }));
            b.c.send(Cmd::GarantAvailable(true));
            b
        }

        /// Connexion au relais de secours qui demande à y reprendre la partie.
        fn takeover_client(&self) -> Option<Client> {
            let (Some(at), Some(key)) = (self.standby_at.clone(), self.identity.clone()) else {
                return None;
            };
            let cert = self.cert.clone()?;
            let c = Client::spawn(at.addr.clone(), Some(at.key));
            c.send(Cmd::Takeover {
                cert,
                key,
                expect: Some(at),
            });
            Some(c)
        }

        /// Le relais du monde est tombé : nouvelle connexion au relais de secours, avec le
        /// certificat de la clé du compte (comme le jeu, voir `Accounts::start_takeover`).
        fn take_over(&mut self) {
            let Some(c) = self.takeover_client() else {
                return;
            };
            self.c = c;
            self.probe = None;
            self.retake = Some(Instant::now() + Duration::from_millis(1500));
        }

        /// Traite les événements reçus ; renvoie vrai si `want` est arrivé.
        fn pump(&mut self, want: &dyn Fn(&Event) -> bool) -> bool {
            let mut hit = false;
            // Monde en pause : on cherche le relais de secours sans quitter le relais du
            // monde (comme le jeu, voir `Accounts`) ; dès qu'il a repris, on y passe.
            let paused = self.net.as_ref().is_some_and(|n| n.is_paused());
            if !paused || self.retake.is_some() {
                self.probe = None;
                self.probe_next = None;
            } else if self.probe.is_none() && self.probe_next.is_none_or(|t| Instant::now() >= t) {
                self.probe = self.takeover_client();
                self.probe_next = Some(Instant::now() + Duration::from_millis(250));
            }
            if let Some(p) = &self.probe {
                let mut got = Vec::new();
                while let Some(e) = p.poll() {
                    got.push(e);
                }
                if got.iter().any(|e| matches!(e, Event::TakenOver { .. })) {
                    self.c = self.probe.take().expect("essai");
                    for e in got {
                        hit |= want(&e);
                        self.event(e);
                    }
                } else if got.iter().any(|e| {
                    matches!(
                        e,
                        Event::Error(_) | Event::Offline(_) | Event::Lost | Event::LoggedOut
                    )
                }) {
                    self.probe = None;
                }
            }
            while let Some(e) = self.c.poll() {
                hit |= want(&e);
                if self.event(e) {
                    break;
                }
            }
            if let Some(s) = &mut self.standby {
                s.tick();
                if s.replaced() {
                    self.standby = None;
                }
            }
            // Provinces (comme `garant_worker`) : relais de région rattaché, relais de
            // province tenu ici.
            let mut closed = false;
            if let Some(a) = self.attach.as_mut() {
                while let Some(e) = a.client.poll() {
                    if crate::province_host::attach_event(
                        a,
                        &mut self.shard,
                        e,
                        self.identity.as_ref(),
                        false,
                    ) {
                        closed = true;
                        break;
                    }
                }
            }
            if closed {
                self.attach = None;
                self.shard = None;
                self.c.send(Cmd::Leave);
            }
            if let (Some(p), Some(l)) = (self.province.as_mut(), &self.link) {
                for r in p.tick() {
                    transport::push(l, &r);
                }
            }
            if self.province.as_ref().is_some_and(|p| p.stopped) {
                // (v143) Province d'un pays : le garant redevient libre (comme
                // `garant_worker`).
                if self.province.as_ref().is_some_and(|p| p.is_child()) {
                    self.c.send(Cmd::Leave);
                }
                self.province = None;
            }
            let key = self.identity.clone();
            self.witnesses.retain(|_, w| w.pump(key.as_ref()));
            for w in self.witnesses.values().filter(|w| w.voted()) {
                self.voted.add(w.root(), w.room);
            }
            if self.retake.is_some_and(|t| Instant::now() >= t) {
                self.take_over();
            }
            hit
        }

        /// Un événement ; vrai si la connexion vient d'être remplacée (la suite de
        /// l'ancienne ne compte plus).
        fn event(&mut self, e: Event) -> bool {
            match e {
                Event::Link(l) => self.link = Some(l),
                Event::BuildGarant(_) => {
                    let link = self.link.clone().expect("file d'envoi");
                    let mut n = BuildNet::new(link, self.name.clone(), false);
                    n.set_identity(self.identity.clone());
                    self.net = Some(n);
                }
                Event::ShardTask {
                    room,
                    region,
                    peer,
                    peer_expires,
                    slot,
                    root,
                    province: Some(at),
                } => {
                    self.shard = None;
                    let signer = self
                        .identity
                        .as_ref()
                        .map_or([0; 32], |k| k.verifying_key().to_bytes());
                    self.attach = Some(crate::province_host::RegionAttach::connect(
                        room,
                        region,
                        (peer, peer_expires),
                        slot,
                        root,
                        at,
                        &self.name,
                        signer,
                        None,
                    ));
                }
                Event::Province(Response::ProvinceTask {
                    room,
                    province,
                    size,
                    epoch,
                    secret,
                    root,
                    info,
                    settings,
                    follow,
                    parent,
                    nest,
                    ref deeper,
                }) => {
                    if let Some(l) = &self.link {
                        if self.province.is_none() {
                            let signer = self
                                .identity
                                .as_ref()
                                .map_or([0; 32], |k| k.verifying_key().to_bytes());
                            let child = parent.is_some();
                            self.province = match follow {
                                Some(at) => crate::province_host::ProvinceHost::standby(
                                    room,
                                    province,
                                    size,
                                    epoch,
                                    secret,
                                    root,
                                    info,
                                    settings,
                                    *at,
                                    &self.name,
                                    signer,
                                    None,
                                    false,
                                    parent.map(|p| *p),
                                    (nest, deeper.clone()),
                                ),
                                None => crate::province_host::ProvinceHost::start(
                                    room,
                                    province,
                                    size,
                                    epoch,
                                    secret,
                                    root,
                                    info,
                                    settings,
                                    l,
                                    false,
                                    (nest, deeper.clone()),
                                    parent.map(|at| crate::province_host::Up {
                                        at: *at,
                                        pseudo: self.name.clone(),
                                        signer,
                                        node_dir: None,
                                    }),
                                ),
                            };
                            if self.province.is_none() && child {
                                self.c.send(Cmd::Leave);
                            }
                        }
                    }
                }
                Event::Province(r) => {
                    if let Response::ProvinceStop {
                        room, province: p, ..
                    } = &r
                    {
                        if self
                            .attach
                            .as_ref()
                            .is_some_and(|a| a.room == *room && a.province == *p)
                        {
                            self.attach = None;
                            self.shard = None;
                        }
                    }
                    if let (Some(h), Some(l)) = (self.province.as_mut(), &self.link) {
                        for req in h.event(&r) {
                            transport::push(l, &req);
                        }
                    }
                }
                Event::Provinces(list) => {
                    if let Some(n) = &mut self.net {
                        n.set_provinces(list);
                    }
                }
                Event::ShardTask {
                    room,
                    region,
                    peer,
                    peer_expires,
                    slot,
                    root,
                    province: None,
                } => {
                    self.attach = None;
                    let link = self.link.clone().expect("file d'envoi");
                    self.shard = crate::garant_worker::ShardHost::start(
                        room,
                        region,
                        (peer, peer_expires),
                        link,
                        false,
                        slot,
                        root,
                        self.identity.clone(),
                    );
                }
                Event::ShardPeers { list, sov, .. } if self.attach.is_none() => {
                    if let Some(s) = &self.shard {
                        s.set_peers(&list, sov);
                    }
                }
                Event::RootEdits { edits, certs, .. } if self.attach.is_none() => {
                    if let Some(s) = &self.shard {
                        s.root_edits(edits, certs);
                    }
                }
                // (Comme `garant_worker` : vérifie les lots d'autres régions qu'on lui
                // confie, redit le battement du relais du monde à ses joueurs.)
                Event::Audit { .. } => {
                    if let (Some(r), Some(l)) = (crate::garant_worker::audit_reply(&e), &self.link)
                    {
                        transport::push(l, &r);
                        self.audits += 1;
                    }
                }
                Event::WorldBeat { room, paused } if self.attach.is_none() => {
                    if let Some(s) = self.shard.as_ref().filter(|s| s.room == room) {
                        s.world_beat(paused);
                    }
                }
                Event::VoiceFrom { from, data } => {
                    if let Some(v) = &mut self.voice {
                        v.receive(
                            crate::voice::key_of(&from),
                            Some((&from, false)),
                            &data,
                            0.0,
                        );
                    }
                }
                Event::Identity(key) => {
                    if let Some(n) = &mut self.net {
                        n.set_identity(Some(key.clone()));
                    }
                    if let Some(s) = &self.shard {
                        s.set_signer(key.clone());
                    }
                    self.identity = Some(key);
                }
                Event::Shards(list) => {
                    if let Some(n) = &mut self.net {
                        n.set_shards(list);
                    }
                }
                Event::ShardTicket {
                    room,
                    pseudo,
                    expires,
                    sig,
                    cert,
                    root,
                } => {
                    if cert.is_some() {
                        self.cert = cert.clone();
                    }
                    if let Some(n) = &mut self.net {
                        n.set_ticket(room, pseudo, expires, sig, cert);
                        n.set_root(root);
                    }
                }
                Event::RoomClosed => {
                    self.shard = None;
                    self.attach = None;
                    if self.standby.as_ref().is_some_and(|s| !s.taken) {
                        self.standby = None;
                    }
                }
                Event::Zones { gained, lost } => {
                    if let Some(n) = &mut self.net {
                        n.zones_changed(&gained, &lost);
                    }
                }
                Event::FromZone { peer, chunk, data } => {
                    if matches!(
                        game::decode::<BuildMsg>(&data),
                        Some(BuildMsg::Reject { .. })
                    ) {
                        self.rejects += 1;
                    }
                    if let Some(n) = &mut self.net {
                        n.receive(peer, chunk, &data, &mut self.world);
                    }
                    // Hôte d'un relais de région : ce que publie le relais du monde
                    // part à ses joueurs (comme dans `garant_worker`) ; rattaché à sa
                    // province, c'est d'elle que ça vient.
                    if let (Some(s), None) = (&self.shard, &self.attach) {
                        s.seed(&data);
                    }
                }
                Event::Promoted { .. } => {
                    if let Some(n) = &mut self.net {
                        n.set_owner();
                    }
                }
                // (Comme `garant_worker` : un relais de secours qui sert un monde repris
                // garde sa place ; jamais témoin d'un monde où il a voté.)
                Event::StandbyTask { .. } if self.standby.as_ref().is_some_and(|s| s.taken) => {
                    self.c.send(Cmd::Leave);
                }
                Event::StandbyTask { room, root, info } => {
                    let link = self.link.clone().expect("file d'envoi");
                    self.standby = crate::garant_worker::StandbyHost::start(
                        room, root, info, &link, false, &self.name,
                    );
                }
                // Gardien : battement du relais du monde (comme dans `garant_worker`).
                Event::Beat {
                    t,
                    committee,
                    standby: at,
                    paused,
                } => {
                    let room = committee.room;
                    let mut leases = Vec::new();
                    if let Some(s) = self.standby.as_ref().filter(|s| s.room == room) {
                        leases.extend(s.beat(t, &committee, paused));
                    }
                    if let Some(w) = self.witnesses.get_mut(&room) {
                        leases.extend(w.beat(t, &committee, at));
                    }
                    if let Some(l) = &self.link {
                        for r in leases {
                            transport::push(l, &r);
                        }
                    }
                }
                Event::WitnessTask { room, root, .. }
                    if self.voted.contains(&root, room) || self.witnesses.contains_key(&room) => {}
                Event::WitnessTask { room, root, cert } => {
                    if let Some(w) =
                        crate::garant_worker::WitnessRole::new(room, &self.name, root, cert, None)
                    {
                        self.witnesses.insert(room, w);
                    }
                }
                Event::Released { room, sig } => {
                    if let Some(s) = &mut self.standby {
                        s.released(room, &sig);
                    }
                }
                Event::Paused(p) => {
                    if let Some(n) = &mut self.net {
                        if n.set_paused(p) && p {
                            self.pauses += 1;
                        }
                    }
                }
                Event::Replica(ref r) => {
                    if let Some(s) = &self.standby {
                        s.feed(r);
                    }
                }
                Event::Standby(at) => self.standby_at = at,
                // (v146) Veille : la connexion au relais du monde est fermée.
                Event::Dozed { .. } => {
                    self.doze_sent = false;
                    self.dozed += 1;
                    if let Some(n) = &mut self.net {
                        n.set_dozing();
                    }
                }
                // (v146) De retour au relais du monde après la veille (le même monde).
                Event::TakenOver { .. } if self.waking => {
                    self.waking = false;
                    self.woken += 1;
                    if let (Some(n), Some(l)) = (&mut self.net, &self.link) {
                        n.woke(l.clone());
                    }
                }
                Event::TakenOver { host, .. } => {
                    self.retake = None;
                    self.taken += 1;
                    if let Some(n) = &mut self.net {
                        n.set_link(self.link.clone().expect("file d'envoi"));
                        if host.eq_ignore_ascii_case(&self.name) {
                            n.set_owner();
                        } else {
                            n.clear_owner();
                        }
                    }
                }
                // Relais du monde tombé : son relais de secours le reprendra (avec les votes
                // des gardiens), et les joueurs y reprennent la partie.
                Event::Lost if self.standby.is_some() || !self.witnesses.is_empty() => {}
                Event::Lost if self.standby_at.is_some() && self.net.is_some() => {
                    if self.retake.is_none() {
                        self.take_over();
                    }
                    return true;
                }
                // (La suite de l'ancienne connexion, ou le relais de secours qui n'a
                // pas encore repris le monde : nouvel essai bientôt.)
                Event::Error(_) | Event::Offline(_) | Event::LoggedOut
                    if self.standby.is_some()
                        || self.retake.is_some()
                        || self.follower
                        || !self.witnesses.is_empty() =>
                {
                    if self.retake.is_some() {
                        self.retake = Some(Instant::now() + Duration::from_millis(250));
                    }
                }
                Event::Error(m) => panic!("{} : {m}", self.name),
                _ => {}
            }
            false
        }

        fn wait(&mut self, want: impl Fn(&Event) -> bool) {
            let t = Instant::now();
            while !self.pump(&want) {
                assert!(
                    t.elapsed() < Duration::from_secs(20),
                    "{} : délai",
                    self.name
                );
                std::thread::sleep(Duration::from_millis(5));
            }
        }

        fn start_net(&mut self, owner: bool) {
            let link = self.link.clone().expect("file d'envoi");
            let mut n = BuildNet::new(link, self.name.clone(), owner);
            n.set_identity(self.identity.clone());
            self.net = Some(n);
        }

        fn tick(&mut self, dt: f32) {
            self.pump(&|_| false);
            if let Some(s) = &self.shard {
                s.pump();
            }
            if let Some(n) = &mut self.net {
                let _ = n.update(&mut self.world, self.pos, (0.0, 0.0), 0.0, 4.0, 144.0, dt);
                self.used_shard |= n.shard_links() > 0;
                for e in n.take_notices() {
                    match e {
                        Event::SetGamemode { mode, .. } => self.modes_seen.push(mode),
                        Event::Slain(_) => self.slain += 1,
                        Event::SetTime { hours, .. } => self.times_seen.push(hours),
                        Event::SetWeather { kind, .. } => self.weather_seen.push(kind),
                        _ => {}
                    }
                }
                self.thor_seen.append(&mut n.thor_inbox);
            }
            self.doze_tick();
            self.voice_tick(dt);
        }

        /// (v146) Veille, comme `Accounts` et le jeu : au fond d'un pays, la connexion au
        /// relais du monde se ferme ; elle se rouvre (connexion neuve, certificat) quand il
        /// le faut.
        fn doze_tick(&mut self) {
            let Some(n) = &self.net else {
                return;
            };
            if !self.doze || self.waking || self.doze_sent {
                return;
            }
            if n.doze_ready() {
                if let Some(room) = n.room() {
                    self.c.send(Cmd::Doze { room });
                    self.doze_sent = true;
                }
            } else if n.wake_needed() {
                if let (Some(cert), Some(key), Some(room)) =
                    (self.cert.clone(), self.identity.clone(), n.room())
                {
                    self.c = Client::spawn(self.relay.0.clone(), Some(self.relay.1));
                    self.c.send(Cmd::Wake { room, cert, key });
                    self.waking = true;
                }
            }
        }

        /// Voix, comme `Session::route_voice` : tous les joueurs vus sont voisins.
        fn voice_tick(&mut self, dt: f32) {
            let (Some(v), Some(n)) = (&mut self.voice, &mut self.net) else {
                return;
            };
            if let Some((key, cert, room, root)) = n.voice_keys() {
                v.set_verify(root, room);
                v.set_signer(crate::voice::Signer { key, cert, room });
            }
            let names: HashMap<u64, String> = n
                .others
                .values()
                .map(|o| (crate::voice::key_of(&o.pseudo), o.pseudo.clone()))
                .collect();
            let near: Vec<u64> = names.keys().copied().collect();
            v.route_by_pseudo();
            v.update(dt, &near, &near, false);
            for (keys, data) in v.take_out() {
                let to: Vec<String> = keys.iter().filter_map(|k| names.get(k).cloned()).collect();
                n.send_voice(to, data);
            }
            for (from, data) in n.take_voice() {
                v.receive(crate::voice::key_of(&from), Some((&from, true)), &data, 0.0);
            }
        }
    }

    /// (v222) Ce que tient le joueur `i` d'un scénario (voir `run`).
    fn item_of(i: usize) -> Option<minelib_net::poses::Held> {
        use minelib_net::poses::{Held, Lamp};
        match i {
            1 => Some(Held::Zeus),
            2 => Some(Held::Lamp(Lamp { on: true, step: 4 })),
            _ => None,
        }
    }

    /// Ce qui se passe pendant un essai.
    #[derive(Default)]
    struct Scenario {
        players: usize,
        /// L'hôte du relais de région s'en va au milieu (les joueurs repassent par le
        /// relais du monde, ou par l'autre relais de la région).
        garant_leaves: bool,
        /// Le propriétaire s'en va au milieu (un joueur reprend le monde et l'enregistre).
        owner_leaves: bool,
        /// La région est assez peuplée pour avoir son propre relais (hébergé par le joueur
        /// d'un autre monde).
        shards: bool,
        /// La région a deux relais (chez deux hôtes), reliés entre eux : elle devient
        /// souveraine (l'un met ses modifications en ordre, l'autre vérifie).
        two_relays: bool,
        /// Trois relais : deux hôtes confirment ce que publie l'hôte principal.
        three_relays: bool,
        /// Son hôte principal triche une fois : il ajoute une modification inventée au
        /// nom d'un joueur et la montre aux siens. Il doit être pris, écarté et remplacé,
        /// et les mondes rester identiques (le bloc inventé défait chez ceux qui l'ont vu).
        forger: bool,
        /// Les joueurs ont la voix (bonjours signés, clés) : elle doit passer par les relais
        /// de la région, presque plus par le relais du monde.
        voice: bool,
    }

    /// Construction en ligne à petite échelle, de bout en bout (vrai relais, vraies
    /// connexions) : des joueurs qui se déplacent et construisent à cheval sur deux zones,
    /// le joueur d'un autre monde qui héberge le relais de la région quand elle en a un,
    /// un joueur qui inonde (refusé au-delà de 20 par seconde), un joueur qui arrive après
    /// coup. À la fin, tous les mondes sont identiques à celui du propriétaire, case par
    /// case, et rien n'est en attente.
    fn run(sc: Scenario) {
        let dir = std::env::temp_dir().join(format!("minelib-sync-{}", rand_u32()));
        minelib_relay::test_seed_games(&dir, "Garant", 5);
        minelib_relay::test_seed_games(&dir, "Garant2", 5);
        minelib_relay::test_seed_games(&dir, "Garant3", 5);
        let h = minelib_relay::start(minelib_relay::Config {
            dir: dir.clone(),
            bind: "127.0.0.1:0".into(),
            pow_bits: 4,
            name: "essai".into(),
        })
        .unwrap();
        if sc.shards {
            h.set_shard_from(2);
        }
        // Relais de la région : 1 + (joueurs - 2) / part.
        let relays = if sc.three_relays {
            h.set_shard_share(3);
            3
        } else if sc.two_relays {
            h.set_shard_share(4);
            2
        } else {
            1
        };
        let addr = h.addr.to_string();
        // Un autre monde, dont le joueur prête sa connexion de garant.
        let mut ailleurs = Bot::new(&addr, h.public, "Garant", false);
        ailleurs.c.send(Cmd::Host {
            kind: 1,
            name: "Autre".into(),
            map: "autre".into(),
            map_sig: [8; 32],
            max_players: 0,
        });
        ailleurs.wait(|e| matches!(e, Event::Hosted(_)));
        let mut garant = Bot::new(&addr, h.public, "Garant", true);
        garant.c.send(Cmd::GarantAvailable(true));
        // D'autres hôtes possibles (joueurs d'autres mondes).
        let mut extra: Vec<(Bot, Bot)> = (2..relays + 1)
            .map(|k| {
                let name = format!("Garant{k}");
                let mut w = Bot::new(&addr, h.public, &name, false);
                w.c.send(Cmd::Host {
                    kind: 1,
                    name: format!("Encore{k}"),
                    map: format!("encore{k}"),
                    map_sig: [7 + k as u8; 32],
                    max_players: 0,
                });
                w.wait(|e| matches!(e, Event::Hosted(_)));
                let g = Bot::new(&addr, h.public, &name, true);
                g.c.send(Cmd::GarantAvailable(true));
                (w, g)
            })
            .collect();
        let (mut linked, mut both_used, mut left) = (false, false, false);
        // Modifications mises en ordre par un hôte principal ; triche faite.
        let (mut ordered, mut forged) = (0u64, false);
        // Des blocs sont arrivés à un joueur par un relais de région.
        let mut via_shard = false;
        // Le monde essayé : un propriétaire et des joueurs.
        let mut bots = vec![Bot::new(&addr, h.public, "Proprio", false)];
        bots[0].c.send(Cmd::Host {
            kind: 1,
            name: "Essai".into(),
            map: "essai".into(),
            map_sig: [7; 32],
            max_players: 0,
        });
        let mut room = None;
        let t = Instant::now();
        while room.is_none() {
            while let Some(e) = bots[0].c.poll() {
                match e {
                    Event::Hosted(r) => room = Some(r),
                    Event::Link(l) => bots[0].link = Some(l),
                    _ => {}
                }
            }
            assert!(t.elapsed() < Duration::from_secs(20));
            std::thread::sleep(Duration::from_millis(5));
        }
        let room = room.unwrap();
        bots[0].start_net(true);
        for i in 1..sc.players {
            let mut b = Bot::new(&addr, h.public, &format!("Joueur{i}"), false);
            b.c.send(Cmd::Join(room));
            b.wait(|e| matches!(e, Event::Joined { .. }));
            b.start_net(false);
            bots.push(b);
        }
        let mut seed = 12345u32;
        let mut rnd = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        let dt = 0.02;
        let mut edited = std::collections::BTreeSet::new();
        let mut had_garant = false;
        let mut gone: Vec<usize> = Vec::new();
        // (Voix : étape où tous sont reliés au relais de leur région, et ce qu'avaient alors
        // remis les relais de région et le relais du monde.)
        let mut voice_from: Option<(usize, usize, usize)> = None;
        // 6 secondes de jeu : on bouge, on construit des deux côtés de la frontière.
        for step in 0..300 {
            // (Une fois les relais de la région reliés : sur une machine chargée, ils
            // peuvent mettre plus de 3 s.)
            if step >= 150 && sc.garant_leaves && !left && (linked || relays == 1 || step >= 250) {
                left = true;
                garant.c.send(Cmd::Leave);
                garant.net = None;
                garant.shard = None;
            }
            // La voix (tant qu'un joueur n'est pas relié au relais de sa région, la sienne
            // passe par le relais du monde) ; puis on parle (un paquet de voix de temps en
            // temps). Compté à partir du moment où tous sont reliés.
            if sc.voice && step == 100 {
                for b in &mut bots {
                    let link = b.link.clone().expect("file d'envoi");
                    b.voice = Some(crate::voice::Voice::new(link));
                }
            }
            if sc.voice
                && step >= 100
                && voice_from.is_none()
                && bots.iter().all(|b| {
                    let n = b.net.as_ref().unwrap();
                    n.chunk
                        .is_some_and(|c| n.shards.link(proto::region_of(c)).is_some())
                })
            {
                let by_relays: usize = std::iter::once(garant.shard.as_ref())
                    .chain(extra.iter().map(|(_, g)| g.shard.as_ref()))
                    .flatten()
                    .map(|s| s.voice_sent())
                    .sum();
                voice_from = Some((step, by_relays, h.voice_relayed()));
            }
            if sc.voice && step >= 150 && step % 10 == 0 {
                for b in &mut bots {
                    if let Some(v) = &mut b.voice {
                        v.talk_once();
                    }
                }
            }
            if step == 150 && sc.owner_leaves {
                bots[0].c.send(Cmd::Leave);
                gone.push(0);
            }
            for (i, b) in bots.iter_mut().enumerate() {
                if gone.contains(&i) {
                    continue;
                }
                // (v222) Le deuxième tient la Foudre de Zeus, le troisième sa lampe allumée.
                if let Some(n) = b.net.as_mut() {
                    n.held = item_of(i);
                }
                let r = rnd();
                b.pos.x = (b.pos.x + (r % 3) as f32 - 1.0).clamp(33.0, 95.0);
                b.pos.z = (b.pos.z + ((r >> 4) % 3) as f32 - 1.0).clamp(1.0, 30.0);
                // Un joueur inonde pendant la première seconde ; les autres ~3 blocs/s.
                let edits = if i == 3 && step < 50 {
                    2
                } else {
                    (rnd() % 17 == 0) as u32
                };
                for _ in 0..edits {
                    let x = (CHUNKS.start * 16 + (rnd() % 64) as i32).min(CHUNKS.end * 16 - 1);
                    let z = (rnd() % 32) as i32;
                    let y = 11 + (rnd() % 3) as i32;
                    let block = if rnd() % 4 == 0 {
                        BlockId::AIR
                    } else {
                        BlockId(1 + rnd() as u16 % 8)
                    };
                    b.world.set(x, y, z, block);
                    edited.insert([x, y, z]);
                }
                b.tick(dt);
            }
            garant.tick(dt);
            had_garant |= garant.shard.is_some();
            via_shard |= bots
                .iter()
                .any(|b| b.net.as_ref().is_some_and(|n| n.via_shard() > 0));
            for (w, g) in &mut extra {
                w.pump(&|_| false);
                g.tick(dt);
            }
            let hosts: Vec<_> = std::iter::once(garant.shard.as_ref())
                .chain(extra.iter().map(|(_, g)| g.shard.as_ref()))
                .collect();
            if relays > 1 && hosts.iter().all(|s| s.is_some()) {
                linked |= hosts.iter().flatten().all(|s| s.linked() == relays - 1);
                both_used |= hosts.iter().flatten().all(|s| s.players() > 0);
            }
            for sh in hosts.iter().flatten() {
                ordered = ordered.max(sh.ordered());
            }
            if sc.forger && !forged && step >= 100 {
                if let Some(sh) = hosts.iter().flatten().find(|s| s.is_primary()) {
                    sh.forge_once();
                    forged = true;
                }
            }
            ailleurs.pump(&|_| false);
            std::thread::sleep(Duration::from_millis(20));
        }
        // Un joueur arrive après coup ; puis tout se pose (confirmations, paquets du
        // propriétaire, annulations).
        let mut late = Bot::new(&addr, h.public, "Retard", false);
        late.c.send(Cmd::Join(room));
        late.wait(|e| matches!(e, Event::Joined { .. }));
        late.start_net(false);
        bots.push(late);
        for _ in 0..300 {
            for (i, b) in bots.iter_mut().enumerate() {
                if !gone.contains(&i) {
                    b.tick(dt);
                }
            }
            garant.tick(dt);
            for (w, g) in &mut extra {
                w.pump(&|_| false);
                g.tick(dt);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        if relays > 1 {
            assert!(linked, "les relais de la région se sont reliés");
            let _ = both_used;
            assert!(
                ordered > 0,
                "région souveraine : ses modifications mises en ordre par son hôte principal"
            );
            eprintln!(
                "mises en ordre (époque en cours) : {ordered}, archivées : {}",
                h.sov_committed()
            );
            if !sc.forger {
                assert!(
                    h.sov_committed() as u64 >= ordered,
                    "tout ce que l'hôte principal a mis en ordre est archivé par le relais du monde"
                );
            }
        }
        if sc.forger {
            assert!(forged, "l'hôte principal a triché");
            assert!(h.sov_faults() >= 1, "la faute est prouvée, l'hôte écarté");
            let still = std::iter::once(garant.shard.as_ref())
                .chain(extra.iter().map(|(_, g)| g.shard.as_ref()))
                .flatten()
                .count();
            assert_eq!(still, relays - 1, "l'hôte qui a triché n'héberge plus rien");
        } else {
            assert_eq!(h.sov_faults(), 0, "aucun hôte honnête n'est condamné");
        }
        assert!(
            !sc.shards || had_garant,
            "la région a reçu son relais, hébergé par le joueur d'un autre monde"
        );
        assert_eq!(
            via_shard, sc.shards,
            "les blocs passent par le relais de région quand il y en a un, et seulement là"
        );
        assert!(
            edited.len() > 50,
            "assez de modifications : {}",
            edited.len()
        );
        assert!(
            bots[3].rejects > 0,
            "l'inondation est refusée au-delà de 20/s"
        );
        let live: Vec<&Bot> = bots
            .iter()
            .enumerate()
            .filter(|(i, _)| !gone.contains(i))
            .map(|(_, b)| b)
            .collect();
        if sc.forger {
            let undone: usize = live.iter().map(|b| b.net.as_ref().unwrap().undone).sum();
            eprintln!("blocs inventés défaits : {undone}");
        }
        for b in &live {
            assert_eq!(
                b.net.as_ref().unwrap().pending(),
                0,
                "{} : en attente",
                b.name
            );
        }
        // (v222) Ce que tiennent les autres arrive avec leurs positions, quel que soit le
        // chemin (relais du monde, relais de la région, d'un relais à l'autre).
        for who in [1, 2] {
            if who >= bots.len() || gone.contains(&who) {
                continue;
            }
            let name: String = bots[who].name.chars().take(16).collect();
            let seen: Vec<_> = live
                .iter()
                .filter(|b| b.name != bots[who].name)
                .filter_map(|b| b.net.as_ref().unwrap().others.get(&name))
                .collect();
            assert!(!seen.is_empty(), "personne ne voit {name}");
            for o in seen {
                assert_eq!(o.held, item_of(who), "ce que tient {name}");
            }
        }
        // Plusieurs relais : chacun voit quand même tous les autres (les positions passent
        // d'un relais à l'autre).
        if relays > 1 {
            for b in &live {
                let seen = b.net.as_ref().unwrap().others.len();
                assert!(
                    seen + 1 >= live.len(),
                    "{} ne voit que {seen} joueurs sur {}",
                    b.name,
                    live.len() - 1
                );
            }
        }
        if sc.voice {
            // Chacun a reçu, par les relais de la région, la clé publique (signée par la
            // clé de son compte) et la clé de voix d'au moins la moitié des autres ; le
            // relais du monde n'a presque rien transmis.
            let talkers: Vec<&&Bot> = live.iter().filter(|b| b.voice.is_some()).collect();
            for b in &talkers {
                let (keys, voices) = b.voice.as_ref().unwrap().known();
                let frames = b.voice.as_ref().unwrap().frames;
                assert!(frames > 0, "{} n'a rien entendu", b.name);
                assert!(
                    keys * 2 >= talkers.len() - 1,
                    "{} : {keys} clés publiques sur {}",
                    b.name,
                    talkers.len() - 1
                );
                assert!(
                    voices * 2 >= talkers.len() - 1,
                    "{} : {voices} clés de voix sur {}",
                    b.name,
                    talkers.len() - 1
                );
            }
            let by_relays: usize = std::iter::once(garant.shard.as_ref())
                .chain(extra.iter().map(|(_, g)| g.shard.as_ref()))
                .flatten()
                .map(|s| s.voice_sent())
                .sum();
            let by_root = h.voice_relayed();
            let (step, relays0, root0) = voice_from.expect("tous reliés au relais de leur région");
            let (by_relays, by_root) = (by_relays - relays0, by_root - root0);
            eprintln!(
                "voix (depuis l'étape {step}) : {by_relays} messages remis par les relais de \
                 région, {by_root} par le relais du monde"
            );
            assert!(
                by_relays > 20 * by_root.max(1),
                "la voix passe par les relais de région ({by_relays} contre {by_root})"
            );
        }
        // La référence : le propriétaire du moment (celui qui enregistre le monde).
        let owner = live
            .iter()
            .find(|b| b.net.as_ref().unwrap().is_owner())
            .expect("un propriétaire");
        let mut differ = Vec::new();
        // Toutes les cases où l'on construit, et au-dessus (où un hôte principal tricheur
        // invente sa modification).
        for b in &live {
            for x in CHUNKS.start * 16..CHUNKS.end * 16 {
                for z in 0..32 {
                    for y in 11..21 {
                        let (a, o) = (b.world.get(x, y, z), owner.world.get(x, y, z));
                        if a != o {
                            differ.push((b.name.clone(), [x, y, z], a, o));
                        }
                    }
                }
            }
        }
        assert!(differ.is_empty(), "mondes différents : {differ:?}");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Ce qui arrive au relais du monde pendant la partie.
    #[derive(Clone, Copy, PartialEq, Debug)]
    enum Fault {
        /// Il tombe (connexions coupées, plus rien ne répond).
        Crash,
        /// Le réseau entre lui et tous ses gardiens est coupé ; les joueurs le joignent
        /// encore. Il doit mettre le monde en pause, et le relais de secours le reprendre :
        /// jamais les deux à la fois.
        Isolated,
        /// Seul le relais de secours est coupé de lui : le témoin l'entend encore, il
        /// continue ; le relais de secours ne reprend pas le monde.
        StandbyCut,
        /// Il s'arrête proprement : le relais de secours reprend tout de suite.
        Release,
        /// Seul le témoin est coupé de lui ; le relais de secours l'entend encore (et ses
        /// baux ne comptent pas : il pourrait tricher). Il doit mettre le monde en pause, et
        /// le relais de secours le reprendre avec le vote du témoin.
        WitnessCut,
        /// Le témoin s'en va sans prévenir (il a peut-être voté) : pause sans issue, le
        /// relais du monde confie le monde au relais de secours.
        WitnessGone,
        /// Il tombe ; le monde repris se choisit son propre relais de secours et un témoin
        /// (les connexions de garant des joueurs le suivent), puis le relais repris tombe à
        /// son tour : le nouveau relais de secours reprend le monde.
        Chain,
    }

    /// Un monde de construction gardé : son relais du monde, un relais de secours (chez
    /// le joueur d'un autre monde) et un témoin (chez un troisième), quatre joueurs qui
    /// construisent sans s'arrêter. Au milieu, `fault` arrive au relais du monde. À aucun
    /// moment deux relais ne servent le monde ; quand le relais de secours reprend, tous
    /// les joueurs y reprennent la partie sans la quitter (le premier arrivé en devient le
    /// propriétaire), un joueur arrive après coup ; à la fin, tous les mondes sont
    /// identiques case par case (ce qui a été construit avant, pendant et après), et rien
    /// n'est en attente.
    fn guarded_world(fault: Fault) {
        let dir = std::env::temp_dir().join(format!("minelib-secours-{}", rand_u32()));
        minelib_relay::test_seed_games(&dir, "Garant", 5);
        minelib_relay::test_seed_games(&dir, "Garant2", 5);
        let h = minelib_relay::start(minelib_relay::Config {
            dir: dir.clone(),
            bind: "127.0.0.1:0".into(),
            pow_bits: 4,
            name: "essai".into(),
        })
        .unwrap();
        h.set_standby_local(true);
        let addr = h.addr.to_string();
        // Les joueurs de deux autres mondes, qui prêtent leur connexion de garant : le
        // premier tiendra le relais de secours, le second sera témoin.
        let mut ailleurs = Vec::new();
        let mut garants = Vec::new();
        for (name, map) in [("Garant", 8u8), ("Garant2", 9u8)] {
            let mut a = Bot::new(&addr, h.public, name, false);
            a.c.send(Cmd::Host {
                kind: 1,
                name: format!("Autre {map}"),
                map: format!("autre{map}"),
                map_sig: [map; 32],
                max_players: 0,
            });
            a.wait(|e| matches!(e, Event::Hosted(_)));
            ailleurs.push(a);
            let g = Bot::new(&addr, h.public, name, true);
            g.c.send(Cmd::GarantAvailable(true));
            garants.push(g);
            // (Le premier garant a le temps de devenir le relais de secours.)
            if garants.len() == 1 {
                let t = Instant::now();
                while t.elapsed() < Duration::from_millis(100) {
                    garants[0].pump(&|_| false);
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        }
        let mut bots = vec![Bot::new(&addr, h.public, "Proprio", false)];
        bots[0].c.send(Cmd::Host {
            kind: 1,
            name: "Essai".into(),
            map: "essai".into(),
            map_sig: [7; 32],
            max_players: 0,
        });
        let mut room = None;
        let t = Instant::now();
        while room.is_none() {
            while let Some(e) = bots[0].c.poll() {
                match e {
                    Event::Hosted(r) => room = Some(r),
                    Event::Link(l) => bots[0].link = Some(l),
                    Event::Identity(k) => bots[0].identity = Some(k),
                    _ => {}
                }
            }
            assert!(t.elapsed() < Duration::from_secs(20));
            std::thread::sleep(Duration::from_millis(5));
        }
        let room = room.unwrap();
        bots[0].start_net(true);
        for i in 1..4 {
            let mut b = Bot::new(&addr, h.public, &format!("Joueur{i}"), false);
            b.c.send(Cmd::Join(room));
            b.wait(|e| matches!(e, Event::Joined { .. }));
            b.start_net(false);
            bots.push(b);
        }
        let mut seed = 777u32;
        let mut rnd = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        let dt = 0.02;
        let mut edited = std::collections::BTreeSet::new();
        let mut after = 0;
        let mut fault_at: Option<Instant> = None;
        let mut fault_step = None;
        let mut resumed_after = None;
        let mut frozen_seen = false;
        for step in 0..650 {
            // La panne, une fois le relais de secours prêt chez tous, la copie faite, et
            // les gardiens au complet (relais de secours et témoin).
            let guarded = h
                .committee(room)
                .is_some_and(|c| c.standby.is_some() && !c.witnesses.is_empty());
            if fault_at.is_none()
                && step >= 150
                && guarded
                && bots.iter().all(|b| b.standby_at.is_some())
            {
                match fault {
                    Fault::Crash | Fault::Chain => h.kill(),
                    Fault::Isolated => h.guard_isolate(true),
                    Fault::StandbyCut => h.guard_cut(&["Garant"]),
                    Fault::WitnessCut => h.guard_cut(&["Garant2"]),
                    Fault::WitnessGone => {
                        garants.truncate(1);
                    }
                    Fault::Release => assert_eq!(h.release(), 1, "monde confié"),
                }
                fault_at = Some(Instant::now());
                fault_step = Some(step);
            }
            // Jamais deux relais à la fois : le relais de secours ne sert le monde que si
            // le relais du monde ne le peut plus (tombé, ou en pause pour toujours).
            let taken = garants[0].standby.as_ref().is_some_and(|s| s.taken);
            if !matches!(fault, Fault::Crash | Fault::Chain) && fault_at.is_some() {
                // (Le monde fermé là-bas, ses joueurs partis : il ne le sert plus.)
                let state = h.frozen(room);
                frozen_seen |= state == Some(true);
                assert!(
                    !taken || state != Some(false),
                    "deux relais servent le monde (étape {step}, {fault:?})"
                );
            }
            if taken && resumed_after.is_none() && bots.iter().all(|b| b.taken > 0) {
                resumed_after = fault_at.map(|t| t.elapsed());
            }
            for b in bots.iter_mut() {
                let r = rnd();
                b.pos.x = (b.pos.x + (r % 3) as f32 - 1.0).clamp(33.0, 95.0);
                b.pos.z = (b.pos.z + ((r >> 4) % 3) as f32 - 1.0).clamp(1.0, 30.0);
                // Relais du monde tombé et partie pas encore reprise : pas de relais du
                // tout, on ne construit pas. Sinon on construit, même en pause (le jeu
                // garde les modifications et les envoie à la reprise).
                let live = !matches!(fault, Fault::Crash | Fault::Chain)
                    || fault_at.is_none()
                    || b.taken > 0;
                if live && step < 520 && rnd() % 9 == 0 {
                    let x = (CHUNKS.start * 16 + (rnd() % 64) as i32).min(CHUNKS.end * 16 - 1);
                    let z = (rnd() % 32) as i32;
                    let y = 11 + (rnd() % 3) as i32;
                    let block = if rnd() % 4 == 0 {
                        BlockId::AIR
                    } else {
                        BlockId(1 + rnd() as u16 % 8)
                    };
                    b.world.set(x, y, z, block);
                    edited.insert([x, y, z]);
                    if fault_at.is_some() {
                        after += 1;
                    }
                }
                b.tick(dt);
            }
            for g in garants.iter_mut() {
                g.tick(dt);
            }
            if !matches!(fault, Fault::Crash | Fault::Chain) || fault_at.is_none() {
                for a in ailleurs.iter_mut() {
                    a.pump(&|_| false);
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            fault_at.is_some(),
            "gardiens au complet et relais de secours prêt"
        );
        assert!(after > 20, "on a construit après la panne : {after}");
        let took = garants[0].standby.as_ref().is_some_and(|s| s.taken);
        if fault == Fault::StandbyCut {
            // Le témoin entend toujours le relais du monde : il continue, sans pause ;
            // personne ne passe au relais de secours.
            assert!(!took, "le relais de secours n'a pas repris le monde");
            assert!(
                !frozen_seen,
                "le relais du monde n'a jamais mis le monde en pause"
            );
            for b in &bots {
                assert_eq!(b.taken, 0, "{} est resté sur le relais du monde", b.name);
                assert_eq!(b.pauses, 0, "{} n'a pas vu de pause", b.name);
            }
        } else {
            assert!(took, "le relais de secours a repris le monde");
            for b in &bots {
                assert_eq!(b.taken, 1, "{} a repris la partie", b.name);
            }
            let after = resumed_after.expect("tous repris");
            eprintln!(
                "{fault:?} : tous les joueurs sur le relais de secours {} ms après la panne \
                 (étape {fault_step:?})",
                after.as_millis()
            );
            if matches!(
                fault,
                Fault::Isolated | Fault::WitnessCut | Fault::WitnessGone
            ) {
                assert!(frozen_seen, "le relais du monde a mis le monde en pause");
                assert!(
                    bots.iter().all(|b| b.pauses > 0),
                    "les joueurs ont vu la pause"
                );
            }
            let h2 = garants[0].standby.as_ref().expect("relais de secours");
            let resumed = h2.resumed().expect("monde repris");
            // Un joueur arrive après coup, sur le relais de secours.
            let at = bots[0].standby_at.clone().unwrap();
            let mut late = Bot::new(&at.addr, at.key, "Retard", false);
            late.c.send(Cmd::Join(resumed));
            late.wait(|e| matches!(e, Event::Joined { .. }));
            late.start_net(false);
            bots.push(late);
        }
        if fault == Fault::Chain {
            // Les connexions de garant de trois joueurs suivent le monde chez le relais
            // repris, par le certificat de leur compte : il s'y choisit un relais de
            // secours et un témoin, et les joueurs apprennent où est le nouveau.
            let first = bots[0].standby_at.clone().expect("relais repris");
            let mut followers: Vec<Bot> = bots[1..4]
                .iter()
                .map(|b| {
                    let cert = b.cert.clone().expect("certificat");
                    let key = b.identity.clone().expect("clé du compte");
                    Bot::cert_garant(&first, &b.name, cert, key)
                })
                .collect();
            let t = Instant::now();
            let mut step = 0u32;
            loop {
                step += 1;
                for b in bots.iter_mut() {
                    // On continue de construire pendant ce temps.
                    if rnd() % 9 == 0 {
                        let x = (CHUNKS.start * 16 + (rnd() % 64) as i32).min(CHUNKS.end * 16 - 1);
                        let (z, y) = ((rnd() % 32) as i32, 11 + (rnd() % 3) as i32);
                        let block = BlockId(1 + rnd() as u16 % 8);
                        b.world.set(x, y, z, block);
                        edited.insert([x, y, z]);
                    }
                    b.tick(dt);
                }
                for g in garants.iter_mut().chain(followers.iter_mut()) {
                    g.tick(dt);
                }
                let resumed = garants[0]
                    .standby
                    .as_ref()
                    .and_then(|s| s.resumed())
                    .expect("monde repris");
                let guarded = garants[0]
                    .standby
                    .as_ref()
                    .and_then(|s| s.relay().committee(resumed))
                    .is_some_and(|c| c.standby.is_some() && !c.witnesses.is_empty());
                let moved = bots
                    .iter()
                    .all(|b| b.standby_at.as_ref().is_some_and(|a| a.key != first.key));
                if guarded && moved && step > 50 {
                    break;
                }
                assert!(
                    t.elapsed() < Duration::from_secs(40),
                    "le monde repris se choisit un relais de secours"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
            // Le relais repris tombe à son tour (chacun doit reprendre la partie une fois de
            // plus, le joueur arrivé après coup compris).
            let before: Vec<u32> = bots.iter().map(|b| b.taken).collect();
            garants[0]
                .standby
                .as_ref()
                .expect("relais repris")
                .relay()
                .kill();
            let t = Instant::now();
            while !bots.iter().zip(&before).all(|(b, n)| b.taken == n + 1) {
                for b in bots.iter_mut() {
                    b.tick(dt);
                }
                for g in followers.iter_mut() {
                    g.tick(dt);
                }
                assert!(
                    t.elapsed() < Duration::from_secs(30),
                    "tous les joueurs passent au relais de secours du monde repris : {:?}",
                    bots.iter()
                        .map(|b| (b.name.clone(), b.taken))
                        .collect::<Vec<_>>()
                );
                std::thread::sleep(Duration::from_millis(20));
            }
            eprintln!(
                "Chain : tous les joueurs sur le deuxième relais de secours {} ms après la \
                 chute du premier",
                t.elapsed().as_millis()
            );
            for _ in 0..200 {
                for b in bots.iter_mut() {
                    if rnd() % 9 == 0 {
                        let x = (CHUNKS.start * 16 + (rnd() % 64) as i32).min(CHUNKS.end * 16 - 1);
                        let (z, y) = ((rnd() % 32) as i32, 11 + (rnd() % 3) as i32);
                        let block = BlockId(1 + rnd() as u16 % 8);
                        b.world.set(x, y, z, block);
                        edited.insert([x, y, z]);
                    }
                    b.tick(dt);
                }
                for g in followers.iter_mut() {
                    g.tick(dt);
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            garants.extend(followers);
        }
        for _ in 0..250 {
            for b in bots.iter_mut() {
                b.tick(dt);
            }
            for g in garants.iter_mut() {
                g.tick(dt);
            }
            if !matches!(fault, Fault::Crash | Fault::Chain) {
                for a in ailleurs.iter_mut() {
                    a.pump(&|_| false);
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        for b in &bots {
            assert_eq!(
                b.net.as_ref().unwrap().pending(),
                0,
                "{} : en attente",
                b.name
            );
        }
        let owners: Vec<&Bot> = bots
            .iter()
            .filter(|b| b.net.as_ref().unwrap().is_owner())
            .collect();
        assert_eq!(owners.len(), 1, "un seul propriétaire");
        let owner = owners[0];
        let mut differ = Vec::new();
        for b in &bots {
            for p in &edited {
                let (a, o) = (
                    b.world.get(p[0], p[1], p[2]),
                    owner.world.get(p[0], p[1], p[2]),
                );
                if a != o {
                    differ.push((b.name.clone(), *p, a, o));
                }
            }
        }
        assert!(differ.is_empty(), "mondes différents : {differ:?}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_build_world_survives_its_relay() {
        guarded_world(Fault::Crash);
    }

    #[test]
    fn a_taken_over_world_gets_its_own_standby() {
        guarded_world(Fault::Chain);
    }

    #[test]
    fn a_witness_cut_from_the_world_relay_pauses_it_whatever_the_standby_says() {
        guarded_world(Fault::WitnessCut);
    }

    #[test]
    fn a_world_relay_stuck_in_pause_hands_over_to_its_standby() {
        guarded_world(Fault::WitnessGone);
    }

    #[test]
    fn a_world_relay_cut_from_its_guardians_pauses_and_the_standby_takes_over() {
        guarded_world(Fault::Isolated);
    }

    #[test]
    fn a_standby_cut_from_the_world_relay_alone_does_not_take_over() {
        guarded_world(Fault::StandbyCut);
    }

    #[test]
    fn a_world_relay_that_stops_cleanly_hands_over_at_once() {
        guarded_world(Fault::Release);
    }

    #[test]
    fn build_worlds_stay_identical_for_everyone() {
        run(Scenario {
            players: 6,
            ..Default::default()
        });
    }

    #[test]
    fn build_worlds_stay_identical_when_the_owner_leaves() {
        run(Scenario {
            players: 6,
            owner_leaves: true,
            ..Default::default()
        });
    }

    #[test]
    fn build_worlds_stay_identical_in_a_crowd() {
        run(Scenario {
            players: 14,
            ..Default::default()
        });
    }

    #[test]
    fn build_worlds_stay_identical_with_a_region_relay() {
        run(Scenario {
            players: 6,
            shards: true,
            ..Default::default()
        });
    }

    #[test]
    fn build_worlds_stay_identical_with_two_relays_in_a_region() {
        run(Scenario {
            players: 8,
            shards: true,
            two_relays: true,
            ..Default::default()
        });
    }

    #[test]
    fn build_worlds_stay_identical_with_three_relays_in_a_region() {
        run(Scenario {
            players: 8,
            shards: true,
            two_relays: true,
            three_relays: true,
            ..Default::default()
        });
    }

    #[test]
    fn a_cheating_primary_host_among_three_is_caught_and_replaced() {
        run(Scenario {
            players: 8,
            shards: true,
            two_relays: true,
            three_relays: true,
            forger: true,
            ..Default::default()
        });
    }

    /// Voix dans une région souveraine (v137) : les joueurs s'entendent par les relais de
    /// la région (bonjours signés par la clé de leur compte, clés de voix), et la voix ne
    /// passe presque plus par le relais du monde.
    #[test]
    fn voice_goes_through_region_relays() {
        run(Scenario {
            players: 8,
            shards: true,
            two_relays: true,
            voice: true,
            ..Default::default()
        });
    }

    /// Provinces (v138) : ce qui arrive à l'hôte d'un relais de province pendant l'essai.
    #[derive(Clone, Copy, PartialEq, Debug)]
    enum ProvinceFault {
        None,
        /// Son jeu tombe (plus rien ne part, plus personne n'est servi).
        Crash,
        /// Il s'en va proprement (la province est rendue au relais du monde).
        Leaves,
        /// Il est coupé du relais du monde, pas des joueurs : il s'arrête à la fin de son
        /// bail, sans que le monde se sépare (ce qu'il a publié sans pouvoir l'archiver est
        /// défait chez ceux qui l'avaient vu, ou renvoyé par son auteur).
        Cut,
    }

    /// Terrain plat sur les chunks `xs` × 0..2 (sol en y = 10).
    fn flat_on(xs: std::ops::Range<i32>) -> World {
        let mut w = World::new(0);
        for cx in xs.clone() {
            for cz in 0..2 {
                w.insert_empty(ChunkPos { x: cx, z: cz });
            }
        }
        for x in xs.start * 16..xs.end * 16 {
            for z in 0..32 {
                w.set(x, 10, z, BlockId::STONE);
            }
        }
        w.journal = Some(Vec::new());
        w
    }

    /// Provinces de bout en bout (vrai relais du monde, vrais relais de province et de
    /// région chez les joueurs d'autres mondes) : une province peuplée est confiée à un
    /// relais de province ; une de ses régions, assez peuplée pour deux relais, y devient
    /// souveraine ; on construit des deux côtés de la frontière avec la province voisine,
    /// et un joueur passe d'une province à l'autre. À la fin, tous les mondes sont
    /// identiques case par case à celui du propriétaire (qui reçoit tout du relais du
    /// monde), et rien n'est en attente — quoi qu'il arrive à l'hôte de la province.
    /// Monde de l'essai des provinces : le relais du monde (une province par région, qui
    /// reçoit son relais à partir de 3 joueurs), quatre joueurs d'autres mondes qui prêtent
    /// leur connexion de garant, le propriétaire et un joueur à l'ouest de la frontière
    /// (x < 512, la province [0, 0]), quatre joueurs à l'est (province [1, 0]). `survival` :
    /// le monde est en survie par défaut (l'inventaire des joueurs, sauf le propriétaire,
    /// est tenu par les relais).
    #[allow(clippy::type_complexity)]
    fn province_setup(
        survival: bool,
    ) -> (
        std::path::PathBuf,
        minelib_relay::Handle,
        Vec<(Bot, Bot)>,
        Vec<Bot>,
        u64,
    ) {
        province_setup_with(survival, 1, 2)
    }

    /// Comme `province_setup`, avec des provinces de `size` × `size` régions, et des relais
    /// de région à partir de `shard_from` joueurs.
    #[allow(clippy::type_complexity)]
    fn province_setup_with(
        survival: bool,
        size: usize,
        shard_from: usize,
    ) -> (
        std::path::PathBuf,
        minelib_relay::Handle,
        Vec<(Bot, Bot)>,
        Vec<Bot>,
        u64,
    ) {
        province_setup_full(survival, size, shard_from, 5, None, &[])
    }

    /// Comme `province_setup_with`, avec `garants` joueurs d'autres mondes qui prêtent leur
    /// connexion de garant, et (v143) des pays : les provinces sont des pays qui confient
    /// eux-mêmes leurs provinces de `nest.0` régions de côté à partir de `nest.1` joueurs.
    #[allow(clippy::type_complexity)]
    fn province_setup_full(
        survival: bool,
        size: usize,
        shard_from: usize,
        garants: usize,
        nest: Option<(usize, usize)>,
        deeper: &[(usize, usize)],
    ) -> (
        std::path::PathBuf,
        minelib_relay::Handle,
        Vec<(Bot, Bot)>,
        Vec<Bot>,
        u64,
    ) {
        let dir = std::env::temp_dir().join(format!("minelib-provinces-{}", rand_u32()));
        let names: Vec<String> = (0..garants)
            .map(|k| {
                if k == 0 {
                    "Garant".to_string()
                } else {
                    format!("Garant{}", k + 1)
                }
            })
            .collect();
        for n in &names {
            minelib_relay::test_seed_games(&dir, n, 5);
        }
        let h = minelib_relay::start(minelib_relay::Config {
            dir: dir.clone(),
            bind: "127.0.0.1:0".into(),
            pow_bits: 4,
            name: "essai".into(),
        })
        .unwrap();
        // Une région de 2 joueurs a son relais, deux à partir de 4 (souveraine) ; une
        // province (une région) reçoit son relais à partir de 3 joueurs.
        h.set_shard_from(shard_from);
        h.set_shard_share(2);
        h.set_provinces(size, 3);
        if let Some((n, from)) = nest {
            h.set_nest(n, from);
        }
        h.set_deeper(deeper);
        let addr = h.addr.to_string();
        // Quatre autres mondes, dont les joueurs prêtent leur connexion de garant.
        let hosts: Vec<(Bot, Bot)> = names
            .iter()
            .enumerate()
            .map(|(k, name)| {
                let mut w = Bot::new(&addr, h.public, name, false);
                w.c.send(Cmd::Host {
                    kind: 1,
                    name: format!("Ailleurs{k}"),
                    map: format!("ailleurs{k}"),
                    map_sig: [20 + k as u8; 32],
                    max_players: 0,
                });
                w.wait(|e| matches!(e, Event::Hosted(_)));
                let g = Bot::new(&addr, h.public, name, true);
                g.c.send(Cmd::GarantAvailable(true));
                (w, g)
            })
            .collect();
        // Le monde : le propriétaire et un joueur à l'ouest de la frontière (x < 512, la
        // province [0, 0]), quatre joueurs à l'est (province [1, 0]).
        let mut bots = vec![Bot::new(&addr, h.public, "Proprio", false)];
        bots[0].c.send(Cmd::Host {
            kind: 1,
            name: "Provinces".into(),
            map: "provinces".into(),
            map_sig: [9; 32],
            max_players: 0,
        });
        let mut room = None;
        let t = Instant::now();
        while room.is_none() {
            while let Some(e) = bots[0].c.poll() {
                match e {
                    Event::Hosted(r) => room = Some(r),
                    Event::Link(l) => bots[0].link = Some(l),
                    _ => {}
                }
            }
            assert!(t.elapsed() < Duration::from_secs(20));
            std::thread::sleep(Duration::from_millis(5));
        }
        let room = room.unwrap();
        bots[0].start_net(true);
        if survival {
            bots[0].c.send(Cmd::ServerAdmin {
                action: proto::SERVER_DEFAULT_GAMEMODE,
                pseudo: String::new(),
                reason: "survival".into(),
            });
            bots[0].wait(|e| matches!(e, Event::ServerAdmin(_)));
        }
        for i in 1..6 {
            let mut b = Bot::new(&addr, h.public, &format!("Joueur{i}"), false);
            b.c.send(Cmd::Join(room));
            b.wait(|e| matches!(e, Event::Joined { .. }));
            b.start_net(false);
            bots.push(b);
        }
        for (i, b) in bots.iter_mut().enumerate() {
            b.world = flat_on(29..36);
            b.pos = Vec3::new(if i < 2 { 498.0 } else { 528.0 }, 11.0, 16.0);
        }
        (dir, h, hosts, bots, room)
    }

    fn province_world(fault: ProvinceFault) {
        let (dir, h, mut hosts, mut bots, _room) = province_setup(false);
        let west = |x: f32| x.clamp(488.0, 508.0);
        let east = |x: f32| x.clamp(516.0, 540.0);
        let mut seed = 777u32;
        let mut rnd = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        let dt = 0.02;
        let mut edited = std::collections::BTreeSet::new();
        let mut delegated_max = 0;
        let mut attached_max = 0;
        let mut ordered = 0u64;
        let mut fault_at: Option<(Instant, [i32; 2])> = None;
        let mut promos_at_fault = 0;
        let mut taken_back: Option<Duration> = None;
        let mut served_again: Option<Duration> = None;
        let mut bulk_sent = false;
        // (v165) Tous parlent tout du long, de part et d'autre de la frontière des deux
        // provinces : après la panne aussi, chacun doit entendre tous les autres.
        let mut heard_cleared = false;
        // 9 secondes de jeu, la panne au milieu (une fois la province servie).
        for step in 0..450 {
            let delegated = h.provinces_delegated();
            delegated_max = delegated_max.max(delegated);
            if step == 60 {
                for b in bots.iter_mut() {
                    let link = b.link.clone().expect("file d'envoi");
                    b.voice = Some(crate::voice::Voice::new(link));
                }
            }
            if step >= 100 && step % 10 == 0 {
                for b in bots.iter_mut() {
                    if let Some(v) = &mut b.voice {
                        v.talk_once();
                    }
                }
            }
            if fault != ProvinceFault::None && fault_at.is_none() && step >= 250 && delegated > 0 {
                let g = hosts
                    .iter_mut()
                    .map(|(_, g)| g)
                    .find(|g| g.province.as_ref().is_some_and(|p| !p.standby))
                    .expect("hôte de la province");
                let which = g.province.as_ref().expect("province").province;
                match fault {
                    ProvinceFault::Crash => {
                        if let Some(p) = g.province.take() {
                            p.kill();
                        }
                    }
                    ProvinceFault::Leaves => {
                        if let (Some(p), Some(l)) = (g.province.as_mut(), &g.link) {
                            for r in p.release() {
                                transport::push(l, &r);
                            }
                        }
                    }
                    ProvinceFault::Cut => {
                        // (Plus rien ne part de lui vers le relais du monde.)
                        let (tx, _) = std::sync::mpsc::channel();
                        g.link = Some(tx);
                    }
                    ProvinceFault::None => {}
                }
                fault_at = Some((Instant::now(), which));
                promos_at_fault = h.province_promotions();
            }
            // Ce qui compte : ce qu'on entend une fois la province de nouveau servie.
            if !heard_cleared
                && (served_again.is_some() || (fault == ProvinceFault::None && step == 250))
            {
                heard_cleared = true;
                for b in bots.iter_mut() {
                    if let Some(v) = &mut b.voice {
                        v.heard.clear();
                    }
                }
            }
            // (Relevée : reprise par le relais du monde, ou confiée à son relais de secours,
            // parfois trop vite pour la voir quitter la liste.)
            if let (Some((t, which)), None) = (fault_at, taken_back) {
                if !h.delegated_provinces().contains(&which)
                    || h.province_promotions() > promos_at_fault
                {
                    taken_back = Some(t.elapsed());
                }
            }
            // (v141) Son relais de secours a pris sa place.
            if let (Some((t, which)), Some(_), None) = (fault_at, taken_back, served_again) {
                if h.delegated_provinces().contains(&which) {
                    served_again = Some(t.elapsed());
                }
            }
            // WorldEdit du propriétaire (OP, à l'ouest) dans la province de l'est (v139 : ses
            // cases partent au relais de la province, signées par le relais du monde).
            if step == 200 && delegated > 0 {
                let b = &mut bots[0];
                let before = b.world.journal.as_ref().map_or(0, Vec::len);
                for x in 520..524 {
                    for z in 4..8 {
                        b.world.set(x, 13, z, BlockId(5));
                        edited.insert([x, 13, z]);
                    }
                }
                if let (Some(n), Some(j)) = (&mut b.net, &mut b.world.journal) {
                    let cells: Vec<_> = j.drain(before.min(j.len())..).collect();
                    n.bulk(cells);
                }
                bulk_sent = true;
            }
            for (i, b) in bots.iter_mut().enumerate() {
                let r = rnd();
                let x = b.pos.x + (r % 3) as f32 - 1.0;
                // Joueur2 passe d'une province à l'autre (aller vers l'ouest, puis retour).
                b.pos.x = if i == 2 {
                    if (100..300).contains(&step) {
                        (b.pos.x - 0.2).max(490.0)
                    } else {
                        (b.pos.x + 0.2).min(535.0)
                    }
                } else if i < 2 {
                    west(x)
                } else {
                    east(x)
                };
                b.pos.z = (b.pos.z + ((r >> 4) % 3) as f32 - 1.0).clamp(1.0, 30.0);
                // ~3 blocs/s chacun, jusqu'à 12 blocs autour de soi (de l'autre côté de la
                // frontière aussi).
                if rnd() % 17 == 0 {
                    let x = (b.pos.x as i32 - 12 + (rnd() % 25) as i32).clamp(29 * 16, 36 * 16 - 1);
                    let z = (rnd() % 32) as i32;
                    let y = 11 + (rnd() % 3) as i32;
                    let block = if rnd() % 4 == 0 {
                        BlockId::AIR
                    } else {
                        BlockId(1 + rnd() as u16 % 8)
                    };
                    b.world.set(x, y, z, block);
                    edited.insert([x, y, z]);
                }
                b.tick(dt);
            }
            for (w, g) in &mut hosts {
                w.pump(&|_| false);
                g.tick(dt);
                ordered = ordered.max(g.shard.as_ref().map_or(0, |s| s.ordered()));
            }
            attached_max =
                attached_max.max(hosts.iter().filter(|(_, g)| g.attach.is_some()).count());
            std::thread::sleep(Duration::from_millis(20));
        }
        // Tout se pose (confirmations, doutes, paquets du propriétaire) ; on parle encore
        // un peu.
        for step in 0..350 {
            for b in bots.iter_mut() {
                if step < 150 && step % 10 == 0 {
                    if let Some(v) = &mut b.voice {
                        v.talk_once();
                    }
                }
                b.tick(dt);
            }
            for (w, g) in &mut hosts {
                w.pump(&|_| false);
                g.tick(dt);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let mut deaf = Vec::new();
        for b in &bots {
            let v = b.voice.as_ref().expect("voix");
            for o in &bots {
                if o.name != b.name && !v.heard.contains(&crate::voice::key_of(&o.name)) {
                    deaf.push(format!("{} n'entend pas {}", b.name, o.name));
                }
            }
        }
        eprintln!("voix après la panne : {} sans voix {deaf:?}", deaf.len());
        assert!(heard_cleared, "la province est de nouveau servie");
        assert!(
            deaf.is_empty(),
            "après la panne, chacun entend les autres : {deaf:?}"
        );
        eprintln!(
            "provinces : {fault:?}, déléguées au plus {delegated_max}, relais de région \
             rattachés {attached_max}, mises en ordre {ordered}, envois archivés {}, reprise \
             après {taken_back:?}, servie de nouveau (relais de secours) après \
             {served_again:?}, promotions {}",
            h.province_archives(),
            h.province_promotions()
        );
        assert!(delegated_max >= 1, "la province peuplée a reçu son relais");
        assert!(bulk_sent, "WorldEdit dans la province");
        assert!(
            h.province_archives() > 0,
            "le relais du monde garde ce qu'elle publie"
        );
        assert!(
            attached_max >= 2,
            "ses relais de région se rattachent à elle"
        );
        assert!(
            ordered > 0,
            "sa région souveraine met en ordre ses modifications"
        );
        assert!(
            edited.len() > 50,
            "assez de modifications : {}",
            edited.len()
        );
        if fault != ProvinceFault::None {
            let t = taken_back.expect("la province quitte son relais");
            assert!(
                t < Duration::from_secs(3),
                "relais tombé remplacé en moins de 3 s : {t:?}"
            );
            // (v141) Par son relais de secours, pas par le relais du monde.
            assert!(
                h.province_promotions() >= 1,
                "son relais de secours a pris sa place"
            );
            let t = served_again.expect("la province est de nouveau servie par un relais");
            assert!(
                t < Duration::from_secs(3),
                "servie de nouveau par son relais de secours en moins de 3 s : {t:?}"
            );
        }
        for b in &bots {
            assert_eq!(
                b.net.as_ref().unwrap().pending(),
                0,
                "{} : en attente",
                b.name
            );
        }
        let mut differ = Vec::new();
        let owner = &bots[0];
        for b in &bots[1..] {
            for p in &edited {
                for y in 11..14 {
                    let q = [p[0], y, p[2]];
                    let (a, o) = (
                        b.world.get(q[0], q[1], q[2]),
                        owner.world.get(q[0], q[1], q[2]),
                    );
                    if a != o {
                        differ.push((b.name.clone(), q, a.0, o.0));
                    }
                }
            }
        }
        differ.sort();
        differ.dedup();
        assert!(
            differ.is_empty(),
            "{} cases différentes du monde du propriétaire, par exemple {:?}",
            differ.len(),
            &differ[..differ.len().min(5)]
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// (v143) Ce qui arrive pendant l'essai des pays.
    #[derive(Clone, Copy, PartialEq, Debug)]
    enum PaysFault {
        None,
        /// L'hôte de la province du pays tombe : le pays donne sa place à son relais de
        /// secours.
        ChildCrash,
        /// L'hôte du pays tombe : le relais du monde donne sa place au relais de secours du
        /// pays ; (v144) les provinces du pays s'arrêtent de servir à la fin de leur bail, puis
        /// se rattachent à lui.
        PaysCrash,
        /// (v144) L'hôte du pays et celui de sa province de l'est tombent ensemble : le relais
        /// de secours de la province se rattache au nouveau pays, qui lui donne sa place.
        BothCrash,
    }

    /// Pays de bout en bout (v143) : le relais du monde confie un pays (2 × 2 régions) au
    /// relais d'un joueur, qui confie lui-même sa région peuplée de l'est (une province d'une
    /// région) au relais d'un autre ; le relais du monde ne voit que le pays. On construit
    /// des deux côtés de la frontière entre la province et le reste du pays, un joueur passe
    /// de l'une à l'autre. À la fin, les mondes sont identiques case par case à celui du
    /// propriétaire (qui reçoit tout du relais du monde), rien n'est en attente, quoi qu'il
    /// arrive à l'hôte de la province ou du pays ; et le relais est remplacé en moins de 3 s.
    /// `big` (v144) : grand monde (la règle est abaissée à 0 joueur) : le pays garde sa copie
    /// chez son relais de secours, et ses provinces chez les leurs ; le relais du monde n'a
    /// plus tout, on compare alors les mondes des joueurs entre eux.
    fn pays_world(fault: PaysFault, regions: bool, big: bool) {
        let _ = env_logger::builder().is_test(true).try_init();
        let (dir, h, mut hosts, mut bots, room) = province_setup_full(
            false,
            2,
            if regions { 3 } else { 100 },
            8,
            Some((1, 3)),
            &[],
        );
        if big {
            h.set_copies_until(0);
        }
        let west = |x: f32| x.clamp(488.0, 508.0);
        let east = |x: f32| x.clamp(516.0, 540.0);
        let mut seed = 991u32;
        let mut rnd = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        let dt = 0.02;
        let mut edited = std::collections::BTreeSet::new();
        let mut kids_max = 0;
        let mut attached_max = 0;
        let mut fault_at: Option<(Instant, u64)> = None;
        let mut served_again: Option<Duration> = None;
        let mut pays_back: Option<Duration> = None;
        // (v144) Le pays tombe : l'hôte de sa province de l'est (qui doit rester le même, et
        // servir de nouveau), son époque ; ce que le pays garde de cette province pendant
        // qu'on y construit (grand monde : rien de plus).
        let mut kept: Option<(String, u64)> = None;
        let mut child_paused = false;
        let mut child_again: Option<Duration> = None;
        // (v144) Les deux ensemble : époque de la province tombée, et quand son relais de
        // secours la sert (promu par le nouveau pays).
        let mut both: Option<u64> = None;
        let mut pays_copy: Vec<usize> = Vec::new();
        let mut pays_positions: Vec<(usize, usize)> = Vec::new();
        // 10 secondes de jeu (12 en grand monde), la panne au milieu (une fois la province du
        // pays servie).
        let steps = match (big, fault) {
            (true, PaysFault::BothCrash) => 750,
            (true, _) => 600,
            _ => 500,
        };
        // (v145 : sans panne, un peu plus, pour voir les joueurs quitter le pays.)
        let steps = if fault == PaysFault::None && !big {
            560
        } else {
            steps
        };
        for step in 0..steps {
            let kids = h.pays_children();
            kids_max = kids_max.max(kids.len());
            // (La province du pays tombe une fois son relais de secours prêt.)
            let standby_ready = hosts.iter().any(|(_, g)| {
                g.province
                    .as_ref()
                    .is_some_and(|p| p.is_child() && p.standby_ready() && p.province == [1, 0])
            });
            let due = match fault {
                PaysFault::ChildCrash | PaysFault::BothCrash => {
                    standby_ready && kids.iter().any(|i| i.province == [1, 0])
                }
                _ => !kids.is_empty(),
            };
            // (Grand monde : une fois la province du pays passée chez son relais de secours.)
            // (v145) Changements de chunk reçus par le pays, une fois ses deux régions confiées
            // (au fond de ses provinces, les joueurs n'entrent plus chez lui).
            // (Tous entrés dans les deux provinces du pays, et sortis du pays.)
            let all_left = kids.len() == 2
                && bots.iter().all(|b| {
                    b.net.as_ref().is_some_and(|n| {
                        let v = n.province_view();
                        v.iter().filter(|v| v.1 == 1 && v.3).count() == 2
                            && v.iter().any(|v| v.1 == 2 && !v.3)
                    })
                });
            if fault == PaysFault::None
                && ((all_left && pays_positions.is_empty()) || step + 1 == steps)
            {
                if let Some(n) = hosts.iter().find_map(|(_, g)| {
                    g.province
                        .as_ref()
                        .filter(|p| !p.standby && !p.is_child())
                        .map(|p| p.positions_seen())
                }) {
                    pays_positions.push((n, step));
                }
            }
            if big && (step == 300 || step == 400) {
                if let Some(n) = hosts.iter().find_map(|(_, g)| {
                    g.province
                        .as_ref()
                        .filter(|p| !p.standby && !p.is_child())
                        .map(|p| p.entries_of([1, 0]))
                }) {
                    pays_copy.push(n);
                }
            }
            let fault_step = if big { 410 } else { 250 };
            if fault != PaysFault::None && fault_at.is_none() && step >= fault_step && due {
                let child = fault == PaysFault::ChildCrash;
                if fault == PaysFault::PaysCrash {
                    kept = hosts.iter().find_map(|(_, g)| {
                        g.province
                            .as_ref()
                            .filter(|p| !p.standby && p.is_child() && p.province == [1, 0])
                            .map(|p| (g.name.clone(), p.epoch))
                    });
                }
                // (Les deux ensemble : d'abord l'hôte de la province.)
                if fault == PaysFault::BothCrash {
                    if let Some(p) = hosts
                        .iter_mut()
                        .map(|(_, g)| g)
                        .find(|g| {
                            g.province
                                .as_ref()
                                .is_some_and(|p| !p.standby && p.is_child() && p.province == [1, 0])
                        })
                        .and_then(|g| g.province.take())
                    {
                        p.kill();
                    }
                }
                let g = hosts
                    .iter_mut()
                    .map(|(_, g)| g)
                    .find(|g| {
                        g.province.as_ref().is_some_and(|p| {
                            !p.standby && p.is_child() == child && (!child || p.province == [1, 0])
                        })
                    })
                    .expect("hôte");
                if let Some(p) = g.province.take() {
                    p.kill();
                }
                let mark = if child {
                    kids.iter()
                        .find(|i| i.province == [1, 0])
                        .map_or(0, |i| i.epoch)
                } else {
                    h.province_promotions() as u64
                };
                if fault == PaysFault::BothCrash {
                    both = kids.iter().find(|i| i.province == [1, 0]).map(|i| i.epoch);
                }
                fault_at = Some((Instant::now(), mark));
            }
            if let (Some((t, mark)), None) = (fault_at, served_again) {
                let again = match fault {
                    PaysFault::ChildCrash => {
                        kids.iter().any(|i| i.province == [1, 0] && i.epoch != mark)
                    }
                    _ => {
                        h.province_promotions() as u64 > mark
                            && h.delegated_provinces().contains(&[0, 0])
                    }
                };
                if again {
                    served_again = Some(t.elapsed());
                }
            }
            // (Le pays repris : sa province peuplée de nouveau servie.)
            if let (Some((t, _)), Some(_), None, PaysFault::PaysCrash) =
                (fault_at, served_again, pays_back, fault)
            {
                if !kids.is_empty() {
                    pays_back = Some(t.elapsed());
                }
            }
            if let (Some((t, _)), Some(old), None) = (fault_at, both, child_again) {
                if h.province_promotions() > 0
                    && kids.iter().any(|i| i.province == [1, 0] && i.epoch != old)
                {
                    child_again = Some(t.elapsed());
                }
            }
            // (v144) La province du pays tombé, rattachée au relais de secours du pays, sert
            // de nouveau (sans avoir changé d'hôte).
            if let (Some((t, _)), Some((name, _)), None) = (fault_at, &kept, child_again) {
                let serving = hosts.iter().any(|(_, g)| {
                    &g.name == name
                        && g.province
                            .as_ref()
                            .is_some_and(|p| p.province == [1, 0] && p.serving())
                });
                // (Son bail finit avant celui du pays : elle s'arrête d'abord de servir.)
                if !serving {
                    child_paused = true;
                } else if child_paused {
                    child_again = Some(t.elapsed());
                }
            }
            for (i, b) in bots.iter_mut().enumerate() {
                let r = rnd();
                let x = b.pos.x + (r % 3) as f32 - 1.0;
                // Joueur2 passe de la province du pays au reste du pays, puis revient.
                b.pos.x = if i == 2 {
                    if (100..300).contains(&step) {
                        (b.pos.x - 0.2).max(490.0)
                    } else {
                        (b.pos.x + 0.2).min(535.0)
                    }
                } else if i < 2 {
                    west(x)
                } else {
                    east(x)
                };
                b.pos.z = (b.pos.z + ((r >> 4) % 3) as f32 - 1.0).clamp(1.0, 30.0);
                if rnd() % 17 == 0 {
                    let x = (b.pos.x as i32 - 12 + (rnd() % 25) as i32).clamp(29 * 16, 36 * 16 - 1);
                    let z = (rnd() % 32) as i32;
                    let y = 11 + (rnd() % 3) as i32;
                    let block = if rnd() % 4 == 0 {
                        BlockId::AIR
                    } else {
                        BlockId(1 + rnd() as u16 % 8)
                    };
                    b.world.set(x, y, z, block);
                    edited.insert([x, y, z]);
                }
                b.tick(dt);
            }
            for (w, g) in &mut hosts {
                w.pump(&|_| false);
                g.tick(dt);
            }
            attached_max =
                attached_max.max(hosts.iter().filter(|(_, g)| g.attach.is_some()).count());
            std::thread::sleep(Duration::from_millis(20));
        }
        // Tout se pose (confirmations, doutes, paquets du propriétaire).
        for _ in 0..400 {
            for b in bots.iter_mut() {
                b.tick(dt);
            }
            for (w, g) in &mut hosts {
                w.pump(&|_| false);
                g.tick(dt);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        for b in &bots {
            eprintln!(
                "{} : provinces {:?}",
                b.name,
                b.net.as_ref().unwrap().province_view()
            );
        }
        eprintln!(
            "pays : {fault:?}, relais de région {regions}, grand {big}, provinces du pays au \
             plus {kids_max}, relais de région rattachés {attached_max}, servi de nouveau \
             après {served_again:?}, province du pays de nouveau servie après {pays_back:?} \
             (rattachée et servie après {child_again:?}), copie du pays pour sa province \
             {pays_copy:?}, changements de chunk reçus par le pays {pays_positions:?}, \
             promotions {}, envois archivés {}",
            h.province_promotions(),
            h.province_archives()
        );
        eprintln!(
            "provinces des pays (relais du monde) : {:?}",
            h.pays_children()
                .iter()
                .map(|i| (i.province, i.epoch))
                .collect::<Vec<_>>()
        );
        let _ = room;
        assert!(kids_max >= 1, "le pays a confié sa province peuplée");
        assert!(
            edited.len() > 50,
            "assez de modifications : {}",
            edited.len()
        );
        if regions {
            assert!(attached_max >= 1, "des relais de région rattachés");
        }
        if fault != PaysFault::None {
            let t = served_again.expect("de nouveau servi par un relais de secours");
            assert!(
                t < Duration::from_secs(3),
                "de nouveau servi en moins de 3 s : {t:?}"
            );
        }
        // (v144) Le pays tombé : sa province ne s'est pas arrêtée, elle s'est rattachée au
        // relais de secours du pays (même hôte, même époque), et sert de nouveau à temps.
        if let Some((name, epoch)) = &kept {
            let t = child_again.expect("la province du pays sert de nouveau");
            assert!(
                t < Duration::from_secs(3),
                "province du pays de nouveau servie en moins de 3 s : {t:?}"
            );
            let g = hosts.iter().find(|(_, g)| &g.name == name).expect("hôte");
            assert!(
                g.1.province
                    .as_ref()
                    .is_some_and(|p| p.epoch == *epoch && !p.stopped),
                "la province du pays a gardé son hôte et son époque"
            );
        }
        // (v144) Le pays et sa province tombés ensemble : le relais de secours de la province
        // s'est rattaché au nouveau pays, qui lui a donné sa place (plus de 3 s : deux relais
        // sont tombés, mais rien n'est perdu).
        if fault == PaysFault::BothCrash {
            let t = child_again.expect("la province du pays servie par son relais de secours");
            assert!(
                t < Duration::from_secs(6),
                "province de nouveau servie en moins de 6 s : {t:?}"
            );
        }
        // (v145) Ses deux régions confiées : le pays ne reçoit plus les changements de chunk
        // de personne (tous au fond de ses provinces).
        if let ([(a, s1), (b, s2)], 2) = (&pays_positions[..], kids_max) {
            if s2 - s1 >= 30 {
                assert_eq!(a, b, "le pays reçoit encore des changements de chunk");
            }
        }
        // (v144) Grand monde : ce qui se construit dans la province du pays ne passe plus
        // par le pays (sa copie de la province ne grandit pas).
        if big && pays_copy.len() == 2 {
            assert_eq!(
                pays_copy[0], pays_copy[1],
                "la copie du pays pour sa province ne grandit pas"
            );
        }
        for b in &bots {
            assert_eq!(
                b.net.as_ref().unwrap().pending(),
                0,
                "{} : en attente",
                b.name
            );
        }
        let mut differ = Vec::new();
        // (Grand monde : le relais du monde n'a pas tout, le propriétaire non plus.)
        let reference = if big { 1 } else { 0 };
        let owner = &bots[reference];
        for b in bots.iter().skip(1).filter(|b| b.name != owner.name) {
            for p in &edited {
                for y in 11..14 {
                    let q = [p[0], y, p[2]];
                    let (a, o) = (
                        b.world.get(q[0], q[1], q[2]),
                        owner.world.get(q[0], q[1], q[2]),
                    );
                    if a != o {
                        differ.push((b.name.clone(), q, a.0, o.0));
                    }
                }
            }
        }
        differ.sort();
        differ.dedup();
        for b in &bots[1..] {
            let mine: Vec<_> = differ.iter().filter(|d| d.0 == b.name).collect();
            let east = mine.iter().filter(|d| d.1[0] >= 512).count();
            let zero_me = mine.iter().filter(|d| d.2 == 0).count();
            let zero_owner = mine.iter().filter(|d| d.3 == 0).count();
            eprintln!(
                "{} : {} différences, {east} à l'est, {zero_me} vides chez lui, {zero_owner} vides chez le propriétaire",
                b.name,
                mine.len()
            );
        }
        assert!(
            differ.is_empty(),
            "{} cases différentes du monde du propriétaire, par exemple {:?}",
            differ.len(),
            &differ[..differ.len().min(5)]
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_crowded_region_of_a_pays_gets_its_own_relay_from_the_pays() {
        pays_world(PaysFault::None, false, false);
    }

    #[test]
    fn region_relays_inside_a_pays_province_attach_to_it() {
        pays_world(PaysFault::None, true, false);
    }

    #[test]
    fn a_pays_province_whose_host_falls_is_served_again_by_its_standby() {
        pays_world(PaysFault::ChildCrash, false, false);
    }

    #[test]
    fn a_pays_whose_host_falls_keeps_its_provinces() {
        pays_world(PaysFault::PaysCrash, false, false);
    }

    #[test]
    fn a_big_pays_keeps_province_copies_at_their_standbys() {
        pays_world(PaysFault::None, false, true);
    }

    #[test]
    fn a_big_pays_whose_host_falls_loses_nothing() {
        pays_world(PaysFault::PaysCrash, false, true);
    }

    #[test]
    fn a_big_pays_and_its_province_falling_together_lose_nothing() {
        pays_world(PaysFault::BothCrash, false, true);
    }

    /// (v148) Pays de pays : le relais du monde confie un pays de 4 × 4 régions, qui confie un
    /// pays de 2 × 2 régions, qui confie ses deux régions peuplées (une région chacune) ; les
    /// hôtes viennent des joueurs (appels de garants) aussi bas qu'il le faut. On construit
    /// des deux côtés ; avec `crash_middle`, l'hôte du pays du milieu tombe : son relais de
    /// secours prend sa place, ses deux provinces s'y rattachent et servent de nouveau en
    /// moins de 3 s. À la fin, les mondes sont identiques à celui du propriétaire.
    /// Pays à qui une connexion de garant est prêtée (province, taille, époque).
    type PCall = ([i32; 2], i32, u64);

    /// Comme le jeu (v147, v148) : chaque joueur appelé par son pays lui prête sa connexion de
    /// garant ; appelé par un autre (plus bas, ou repris), elle y va si elle ne tient rien.
    fn lend(bots: &[Bot], lent: &mut Vec<Bot>, lent_to: &mut HashMap<String, PCall>, room: u64) {
        for b in bots {
            let Some(call) = b
                .net
                .as_ref()
                .and_then(|n| n.garant_call().cloned())
                .filter(|c| c.picks(&b.name))
            else {
                continue;
            };
            let target = (call.at.province, call.at.size, call.at.epoch);
            if let Some(i) = lent.iter().position(|g| g.name == b.name) {
                let g = &lent[i];
                let idle = g.province.is_none() && g.attach.is_none() && g.shard.is_none();
                if lent_to.get(&b.name) == Some(&target) || !idle {
                    continue;
                }
                lent.remove(i);
            }
            let (Some(cert), Some(key)) = (b.cert.clone(), b.identity.clone()) else {
                continue;
            };
            let at = proto::StandbyAt {
                node: call.at.node,
                addr: call.at.addr.clone(),
                key: call.at.key,
                room,
                pseudo: String::new(),
                root: [0; 32],
            };
            lent.push(Bot::cert_garant(&at, &b.name, cert, key));
            lent_to.insert(b.name.clone(), target);
        }
    }

    /// Relais de province tenus par ces garants (d'ailleurs, ou prêtés par des joueurs).
    fn held_by<'a>(
        hosts: &'a [(Bot, Bot)],
        lent: &'a [Bot],
    ) -> impl Iterator<Item = &'a crate::province_host::ProvinceHost> {
        hosts
            .iter()
            .map(|(_, g)| g)
            .chain(lent.iter())
            .filter_map(|g| g.province.as_ref())
    }

    /// `voice` : (v165) les joueurs se parlent de part et d'autre de la frontière des deux
    /// provinces ; chacun doit entendre tous les autres, par les relais des provinces.
    fn deep_world(crash_middle: bool, voice: bool) {
        let _ = env_logger::builder().is_test(true).try_init();
        let (dir, h, mut hosts, mut bots, room) =
            province_setup_full(false, 4, 100, 8, Some((2, 3)), &[(1, 3)]);
        for (i, b) in bots.iter_mut().enumerate() {
            b.world = flat_at(29..36, 29..32);
            b.pos = Vec3::new(if i < 3 { 498.0 } else { 528.0 }, 11.0, 490.0);
        }
        let west = |x: f32| x.clamp(488.0, 508.0);
        let east = |x: f32| x.clamp(516.0, 540.0);
        let mut seed = 8080u32;
        let mut rnd = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        let dt = 0.02;
        let mut edited = std::collections::BTreeSet::new();
        let mut lent: Vec<Bot> = Vec::new();
        let mut lent_to: HashMap<String, PCall> = HashMap::new();
        // Plus petites provinces vues par les joueurs ; panne (quand, époque du milieu) ;
        // servi de nouveau (milieu, ses provinces).
        let mut sizes_seen = std::collections::BTreeSet::new();
        let mut fault_at: Option<(Instant, u64)> = None;
        let mut middle_back: Option<Duration> = None;
        let mut leaves_back: Option<Duration> = None;
        let mut leaves_paused = false;
        let steps = if crash_middle { 800 } else { 600 };
        // Voix : relayée par les relais de province et par le relais du monde depuis que
        // tous parlent.
        let mut voice_from: Option<(usize, usize)> = None;
        for step in 0..steps {
            if voice && step == 60 {
                for b in bots.iter_mut() {
                    let link = b.link.clone().expect("file d'envoi");
                    b.voice = Some(crate::voice::Voice::new(link));
                }
            }
            if voice && step >= 250 && step % 10 == 0 {
                if voice_from.is_none() {
                    let by_prov: usize = held_by(&hosts, &lent).map(|p| p.voice_relayed()).sum();
                    voice_from = Some((by_prov, h.voice_relayed()));
                }
                for b in bots.iter_mut() {
                    if let Some(v) = &mut b.voice {
                        v.talk_once();
                    }
                }
            }
            for b in bots.iter_mut() {
                if let Some(n) = &b.net {
                    for v in n.province_view() {
                        if v.3 {
                            sizes_seen.insert(v.1);
                        }
                    }
                }
            }
            lend(&bots, &mut lent, &mut lent_to, room);
            // Qui tient les provinces (de bas en haut).
            let leaves_serving = held_by(&hosts, &lent)
                .filter(|p| p.size_of() == 1 && !p.standby && !p.stopped && p.serving())
                .count();
            let middle_ready = held_by(&hosts, &lent)
                .any(|p| p.size_of() == 2 && p.standby && p.standby_ready() && !p.stopped);
            if crash_middle
                && fault_at.is_none()
                && step >= 300
                && leaves_serving == 2
                && middle_ready
            {
                let g = hosts
                    .iter_mut()
                    .map(|(_, g)| g)
                    .chain(lent.iter_mut())
                    .find(|g| {
                        g.province
                            .as_ref()
                            .is_some_and(|p| p.size_of() == 2 && !p.standby && !p.stopped)
                    })
                    .expect("hôte du pays du milieu");
                let p = g.province.take().expect("pays du milieu");
                let epoch = p.epoch;
                p.kill();
                fault_at = Some((Instant::now(), epoch));
            }
            if let Some((t, old)) = fault_at {
                if middle_back.is_none()
                    && held_by(&hosts, &lent)
                        .any(|p| p.size_of() == 2 && !p.standby && p.epoch != old && p.serving())
                {
                    middle_back = Some(t.elapsed());
                }
                if leaves_serving < 2 {
                    leaves_paused = true;
                } else if leaves_paused && leaves_back.is_none() {
                    leaves_back = Some(t.elapsed());
                }
            }
            for (i, b) in bots.iter_mut().enumerate() {
                let r = rnd();
                let x = b.pos.x + (r % 3) as f32 - 1.0;
                b.pos.x = if i < 3 { west(x) } else { east(x) };
                b.pos.z = (b.pos.z + ((r >> 4) % 3) as f32 - 1.0).clamp(475.0, 505.0);
                if rnd() % 17 == 0 && step + 100 < steps {
                    let x = (b.pos.x as i32 - 12 + (rnd() % 25) as i32).clamp(29 * 16, 36 * 16 - 1);
                    let z = 470 + (rnd() % 36) as i32;
                    let y = 11 + (rnd() % 3) as i32;
                    let block = if rnd() % 4 == 0 {
                        BlockId::AIR
                    } else {
                        BlockId(1 + rnd() as u16 % 8)
                    };
                    b.world.set(x, y, z, block);
                    edited.insert([x, y, z]);
                }
                b.tick(dt);
            }
            for g in lent.iter_mut() {
                g.tick(dt);
            }
            for (w, g) in &mut hosts {
                w.pump(&|_| false);
                g.tick(dt);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        for _ in 0..400 {
            for b in bots.iter_mut() {
                b.tick(dt);
            }
            for g in lent.iter_mut() {
                g.tick(dt);
            }
            for (w, g) in &mut hosts {
                w.pump(&|_| false);
                g.tick(dt);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let held: Vec<(String, [i32; 2], i32, bool)> = hosts
            .iter()
            .map(|(_, g)| g)
            .chain(lent.iter())
            .filter_map(|g| {
                g.province
                    .as_ref()
                    .filter(|p| !p.stopped)
                    .map(|p| (g.name.clone(), p.province, p.size_of(), p.standby))
            })
            .collect();
        eprintln!(
            "pays de pays : tailles vues {sizes_seen:?}, tenues {held:?}, panne du milieu \
             {fault_at:?}, milieu de nouveau servi après {middle_back:?}, ses provinces après \
             {leaves_back:?}, plages de rangs renouvelées {}",
            h.pays_refills()
        );
        assert!(
            [1, 2, 4].iter().all(|s| sizes_seen.contains(s)),
            "trois étages vus par les joueurs : {sizes_seen:?}"
        );
        assert!(
            held.iter().filter(|h| h.2 == 1 && !h.3).count() == 2,
            "les deux régions peuplées confiées par le pays du milieu : {held:?}"
        );
        if crash_middle {
            let t = middle_back.expect("pays du milieu de nouveau servi");
            assert!(
                t < Duration::from_secs(3),
                "pays du milieu servi en moins de 3 s : {t:?}"
            );
            let t = leaves_back.expect("ses provinces servent de nouveau");
            assert!(
                t < Duration::from_secs(3),
                "ses provinces servent en moins de 3 s : {t:?}"
            );
        }
        if voice {
            // Chacun a entendu tous les autres, de son côté de la frontière et de l'autre.
            let mut deaf = Vec::new();
            for b in &bots {
                let v = b.voice.as_ref().expect("voix");
                for o in &bots {
                    let key = crate::voice::key_of(&o.name);
                    if o.name != b.name && !v.heard.contains(&key) {
                        deaf.push(format!("{} n'entend pas {}", b.name, o.name));
                    }
                }
            }
            let by_prov: usize = held_by(&hosts, &lent).map(|p| p.voice_relayed()).sum();
            let (prov0, root0) = voice_from.expect("voix");
            let (by_prov, by_root) = (by_prov - prov0, h.voice_relayed() - root0);
            eprintln!(
                "voix au fond du monde : {by_prov} messages par les relais des provinces, \
                 {by_root} par le relais du monde ; {} sans voix",
                deaf.len()
            );
            assert!(deaf.is_empty(), "{deaf:?}");
            assert!(
                by_prov > 10 * by_root.max(1),
                "la voix passe par les relais des provinces ({by_prov} contre {by_root})"
            );
        }
        for b in &bots {
            assert_eq!(
                b.net.as_ref().unwrap().pending(),
                0,
                "{} : en attente",
                b.name
            );
        }
        let owner = &bots[0];
        let mut differ = Vec::new();
        for b in &bots[1..] {
            for p in &edited {
                let (a, o) = (
                    b.world.get(p[0], p[1], p[2]),
                    owner.world.get(p[0], p[1], p[2]),
                );
                if a != o {
                    differ.push((b.name.clone(), *p, a.0, o.0));
                }
            }
        }
        assert!(
            differ.is_empty(),
            "{} cases différentes du monde du propriétaire, par exemple {:?}",
            differ.len(),
            &differ[..differ.len().min(5)]
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_pays_of_pays_serves_three_levels_deep() {
        deep_world(false, false);
    }

    #[test]
    fn a_middle_pays_whose_host_falls_keeps_its_provinces() {
        deep_world(true, false);
    }

    /// (v165) Toute la planète a la voix de proximité : au fond d'un monde de pays de pays,
    /// les joueurs de deux provinces voisines (de part et d'autre de leur frontière) se
    /// parlent et s'entendent tous, par les relais des provinces.
    #[test]
    fn players_hear_each_other_across_province_borders_deep_in_the_world() {
        deep_world(false, true);
    }

    /// (v147) Un pays trouve les hôtes de ses provinces parmi ses propres joueurs : le relais
    /// du monde n'a que trois garants (pour le pays et son relais de secours) ; le pays appelle
    /// des garants parmi ses joueurs, ceux qui sont appelés lui prêtent leur connexion de
    /// garant, et il leur confie ses deux provinces et leurs relais de secours (jamais à un
    /// joueur de la province qu'il servira). Sa plage de rangs, petite exprès, est
    /// renouvelée par le relais du monde. On construit pendant ce temps ; à la fin, les
    /// mondes sont identiques à celui du propriétaire.
    #[test]
    fn a_pays_finds_hosts_for_its_provinces_among_its_players() {
        let _ = env_logger::builder().is_test(true).try_init();
        let (dir, h, mut hosts, mut bots, room) =
            province_setup_full(false, 2, 100, 3, Some((1, 3)), &[]);
        h.set_pays_seqs(3 << 34);
        for (i, b) in bots.iter_mut().enumerate() {
            b.world = flat_at(29..36, 29..32);
            b.pos = Vec3::new(if i < 3 { 498.0 } else { 528.0 }, 11.0, 490.0);
        }
        let west = |x: f32| x.clamp(488.0, 508.0);
        let east = |x: f32| x.clamp(516.0, 540.0);
        let mut seed = 5150u32;
        let mut rnd = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        let dt = 0.02;
        let mut edited = std::collections::BTreeSet::new();
        // Connexions de garant prêtées au pays par ses joueurs appelés (comme le jeu).
        let mut lent: Vec<Bot> = Vec::new();
        let mut lent_to: HashMap<String, PCall> = HashMap::new();
        let mut kids_max = 0;
        for step in 0..700 {
            kids_max = kids_max.max(h.pays_children().len());
            lend(&bots, &mut lent, &mut lent_to, room);
            for (i, b) in bots.iter_mut().enumerate() {
                let r = rnd();
                let x = b.pos.x + (r % 3) as f32 - 1.0;
                b.pos.x = if i < 3 { west(x) } else { east(x) };
                b.pos.z = (b.pos.z + ((r >> 4) % 3) as f32 - 1.0).clamp(475.0, 505.0);
                if rnd() % 17 == 0 && step < 600 {
                    let x = (b.pos.x as i32 - 12 + (rnd() % 25) as i32).clamp(29 * 16, 36 * 16 - 1);
                    let z = 470 + (rnd() % 36) as i32;
                    let y = 11 + (rnd() % 3) as i32;
                    let block = if rnd() % 4 == 0 {
                        BlockId::AIR
                    } else {
                        BlockId(1 + rnd() as u16 % 8)
                    };
                    b.world.set(x, y, z, block);
                    edited.insert([x, y, z]);
                }
                b.tick(dt);
            }
            for g in lent.iter_mut() {
                g.tick(dt);
            }
            for (w, g) in &mut hosts {
                w.pump(&|_| false);
                g.tick(dt);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        for _ in 0..400 {
            for b in bots.iter_mut() {
                b.tick(dt);
            }
            for g in lent.iter_mut() {
                g.tick(dt);
            }
            for (w, g) in &mut hosts {
                w.pump(&|_| false);
                g.tick(dt);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        // Qui tient quoi : provinces du pays tenues par ses joueurs, jamais par un joueur de
        // la province servie.
        let held: Vec<(String, [i32; 2], bool)> = lent
            .iter()
            .filter_map(|g| {
                g.province
                    .as_ref()
                    .filter(|p| !p.stopped)
                    .map(|p| (g.name.clone(), p.province, p.standby))
            })
            .collect();
        let chosen: usize = hosts
            .iter()
            .filter_map(|(_, g)| g.province.as_ref())
            .filter(|p| !p.is_child() && !p.standby)
            .map(|p| p.local_tasks())
            .sum();
        let outside = hosts.iter().any(|(_, g)| {
            g.province
                .as_ref()
                .is_some_and(|p| p.is_child() && !p.stopped)
        });
        eprintln!(
            "pays : provinces au plus {kids_max}, tenues par ses joueurs {held:?}, par un garant \
             d'ailleurs {outside}, hôtes choisis par le pays {chosen}, nouvelles plages de rangs \
             {}",
            h.pays_refills()
        );
        assert!(
            chosen >= 4,
            "le pays a choisi lui-même au moins quatre hôtes : {chosen}"
        );
        assert_eq!(kids_max, 2, "le pays a confié ses deux régions");
        assert!(
            !outside,
            "aucune province du pays tenue par un garant d'ailleurs"
        );
        assert!(
            held.iter().filter(|h| !h.2).count() == 2 && held.iter().filter(|h| h.2).count() == 2,
            "ses deux provinces et leurs relais de secours tenus par ses joueurs : {held:?}"
        );
        // (Joueurs 0 à 2 à l'ouest, province [0, 0] ; 3 à 5 à l'est, [1, 0].)
        for (name, p, _) in &held {
            let west_player = ["Proprio", "Joueur1", "Joueur2"].contains(&name.as_str());
            assert_ne!(
                *p == [0, 0],
                west_player,
                "{name} ne tient pas la province où il joue"
            );
        }
        assert!(h.pays_refills() >= 1, "plage de rangs du pays renouvelée");
        for b in &bots {
            assert_eq!(
                b.net.as_ref().unwrap().pending(),
                0,
                "{} : en attente",
                b.name
            );
        }
        let owner = &bots[0];
        let mut differ = Vec::new();
        for b in &bots[1..] {
            for p in &edited {
                let (a, o) = (
                    b.world.get(p[0], p[1], p[2]),
                    owner.world.get(p[0], p[1], p[2]),
                );
                if a != o {
                    differ.push((b.name.clone(), *p, a.0, o.0));
                }
            }
        }
        assert!(
            differ.is_empty(),
            "{} cases différentes du monde du propriétaire, par exemple {:?}",
            differ.len(),
            &differ[..differ.len().min(5)]
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// (v146) Veille : au fond d'un pays (ses deux régions confiées à leurs provinces, tout ce
    /// qu'on voit dans le pays), les joueurs ferment leur connexion au relais du monde et
    /// continuent par les provinces ; on construit sans s'arrêter. Un OP change le mode d'un
    /// joueur en veille et en tue un autre : leurs provinces le leur disent. Puis un joueur
    /// s'approche de là où décide le relais du monde : il y revient tout seul (connexion
    /// neuve, certificat), puis retourne au fond du pays. À la fin, les mondes sont
    /// identiques à celui du propriétaire, rien en attente.
    #[test]
    fn players_deep_in_a_pays_doze_and_come_back() {
        let _ = env_logger::builder().is_test(true).try_init();
        let (dir, h, mut hosts, mut bots, room) =
            province_setup_full(false, 2, 100, 8, Some((1, 3)), &[]);
        // Trois joueurs de chaque côté (les deux régions du pays assez peuplées pour leurs
        // provinces), au milieu du pays (tout ce qu'ils voient en fait partie).
        for (i, b) in bots.iter_mut().enumerate() {
            b.world = flat_at(29..36, 29..32);
            b.pos = Vec3::new(if i < 3 { 498.0 } else { 528.0 }, 11.0, 490.0);
            b.doze = i > 0;
        }
        // (Le skin de Joueur1, déposé chez chaque province où il entre : en veille, les
        // autres l'y trouvent.)
        let mut png = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13];
        png.extend(b"IHDR");
        png.extend(64u32.to_be_bytes());
        png.extend(64u32.to_be_bytes());
        png.extend([8, 6, 0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
        bots[1].net.as_mut().unwrap().set_skin(png.clone());
        let mut skin_seen = false;
        let west = |x: f32| x.clamp(488.0, 508.0);
        let east = |x: f32| x.clamp(516.0, 540.0);
        let mut seed = 4242u32;
        let mut rnd = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        let dt = 0.02;
        let mut edited = std::collections::BTreeSet::new();
        // Étape où le relais du monde n'a plus que le propriétaire ; où Joueur3 part vers le
        // relais du monde ; s'il y est revenu (connecté de nouveau là-bas).
        let mut all_dozing: Option<usize> = None;
        let mut walk_from: Option<usize> = None;
        let mut back_seen = false;
        let mut edits_dozing = 0;
        let mut step = 0;
        while step < 1600 {
            let here = h.players_here(room);
            if all_dozing.is_none() && here.len() == 1 && bots[1..].iter().all(|b| b.dozed > 0) {
                all_dozing = Some(step);
            }
            if let Some(a) = all_dozing {
                // Un OP change le mode d'un joueur en veille, en tue un autre.
                if step == a + 30 {
                    bots[0].c.send(Cmd::ServerAdmin {
                        action: proto::SERVER_TIMESET,
                        pseudo: String::new(),
                        reason: "6".into(),
                    });
                    // (v229) Et il impose l'orage.
                    bots[0].c.send(Cmd::ServerAdmin {
                        action: proto::SERVER_WEATHER,
                        pseudo: String::new(),
                        reason: "storm".into(),
                    });
                    bots[0].c.send(Cmd::ServerAdmin {
                        action: proto::SERVER_GAMEMODE,
                        pseudo: "Joueur4".into(),
                        reason: "survival".into(),
                    });
                    bots[0].c.send(Cmd::ServerAdmin {
                        action: proto::SERVER_KILL,
                        pseudo: "Joueur5".into(),
                        reason: String::new(),
                    });
                }
                if step == a + 80 {
                    walk_from = Some(step);
                }
                // Le skin de Joueur1, demandé par Joueur4 en veille (à ses provinces).
                if step == a + 40 {
                    bots[4].net.as_ref().unwrap().ask_skin("Joueur1");
                }
                if let Some(n) = bots[4].net.as_mut() {
                    skin_seen |= n
                        .take_skins()
                        .iter()
                        .any(|(p, s)| p.eq_ignore_ascii_case("Joueur1") && *s == png);
                }
            }
            back_seen |= walk_from.is_some() && here.iter().any(|p| p == "Joueur3");
            // Joueur3 va jusqu'à voir là où décide le relais du monde (x ≥ 1024), y reste un
            // peu, puis revient.
            let walk = walk_from.map(|w| step - w);
            for (i, b) in bots.iter_mut().enumerate() {
                let r = rnd();
                let x = b.pos.x + (r % 3) as f32 - 1.0;
                b.pos.x = match (i, walk) {
                    (3, Some(t)) if t < 140 => (b.pos.x + 2.0).min(800.0),
                    (3, Some(t)) if t < 260 => b.pos.x,
                    (3, Some(_)) => (b.pos.x - 2.0).max(530.0),
                    _ if i < 3 => west(x),
                    _ => east(x),
                };
                b.pos.z = (b.pos.z + ((r >> 4) % 3) as f32 - 1.0).clamp(475.0, 505.0);
                if rnd() % 17 == 0 {
                    let x = (b.pos.x as i32 - 12 + (rnd() % 25) as i32).clamp(29 * 16, 36 * 16 - 1);
                    let z = 470 + (rnd() % 36) as i32;
                    let y = 11 + (rnd() % 3) as i32;
                    let block = if rnd() % 4 == 0 {
                        BlockId::AIR
                    } else {
                        BlockId(1 + rnd() as u16 % 8)
                    };
                    b.world.set(x, y, z, block);
                    edited.insert([x, y, z]);
                    if b.net.as_ref().is_some_and(|n| n.is_dozing()) {
                        edits_dozing += 1;
                    }
                }
                b.tick(dt);
            }
            for (w, g) in &mut hosts {
                w.pump(&|_| false);
                g.tick(dt);
            }
            std::thread::sleep(Duration::from_millis(20));
            step += 1;
            // (Jusqu'à un peu après le retour de Joueur3 au fond du pays.)
            if walk_from.is_some_and(|w| step > w + 480) {
                break;
            }
        }
        // Tout se pose (confirmations, doutes, paquets du propriétaire).
        for _ in 0..400 {
            for b in bots.iter_mut() {
                b.tick(dt);
            }
            for (w, g) in &mut hosts {
                w.pump(&|_| false);
                g.tick(dt);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        for b in &bots {
            eprintln!(
                "{} : veilles {}, retours {}, modes {:?}, morts {}, en veille {}, provinces {:?}",
                b.name,
                b.dozed,
                b.woken,
                b.modes_seen,
                b.slain,
                b.net.as_ref().unwrap().is_dozing(),
                b.net.as_ref().unwrap().province_view()
            );
        }
        eprintln!(
            "veille : tous en veille à l'étape {all_dozing:?}, départ de Joueur3 à {walk_from:?}, \
             revenu au relais du monde {back_seen}, {edits_dozing} modifications faites en \
             veille, {} en tout ; au relais du monde à la fin : {:?}",
            edited.len(),
            h.players_here(room)
        );
        assert!(all_dozing.is_some(), "tous en veille au fond du pays");
        assert!(
            skin_seen,
            "le skin d'un joueur trouvé chez les provinces, en veille"
        );
        assert!(
            bots[1..].iter().all(|b| b.times_seen.contains(&6.0)),
            "l'heure donnée par l'OP arrive par les provinces à ceux qui sont en veille"
        );
        assert!(
            bots[1..]
                .iter()
                .all(|b| b.weather_seen.contains(&proto::WEATHER_STORM)),
            "la météo imposée par l'OP arrive de même : {:?}",
            bots.iter().map(|b| b.weather_seen.clone()).collect::<Vec<_>>()
        );
        assert!(edits_dozing > 30, "on construit en veille : {edits_dozing}");
        assert_eq!(
            bots[4].modes_seen,
            vec!["survival".to_string()],
            "le mode donné par l'OP arrive par la province (une fois)"
        );
        assert_eq!(
            bots[5].slain, 1,
            "la mort donnée par l'OP arrive par la province (une fois)"
        );
        assert!(
            bots[3].woken > 0 && back_seen,
            "Joueur3 est revenu au relais du monde"
        );
        for b in &bots {
            assert_eq!(
                b.net.as_ref().unwrap().pending(),
                0,
                "{} : en attente",
                b.name
            );
        }
        let owner = &bots[0];
        let mut differ = Vec::new();
        for b in &bots[1..] {
            for p in &edited {
                let (a, o) = (
                    b.world.get(p[0], p[1], p[2]),
                    owner.world.get(p[0], p[1], p[2]),
                );
                if a != o {
                    differ.push((b.name.clone(), *p, a.0, o.0));
                }
            }
        }
        assert!(
            differ.is_empty(),
            "{} cases différentes du monde du propriétaire, par exemple {:?}",
            differ.len(),
            &differ[..differ.len().min(5)]
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Objets et sacs de survie dans les provinces (v139), de bout en bout : la province
    /// peuplée arbitre les objets de ses zones (lâchés, ramassés), ce qu'elle publie n'entre
    /// chez les joueurs qu'une fois gardé par le relais du monde ; en survie, le sac de
    /// chaque joueur lui est prêté tant qu'il y est, et suit celui qui passe d'une province
    /// à l'autre. À la fin, rien n'a été créé : les inventaires et le sol du relais du monde
    /// font le compte du début (sans panne : exactement ; avec : au plus — un objet peut se
    /// perdre, jamais se doubler) ; en survie, chaque sac du relais du monde est l'inventaire
    /// de son joueur.
    fn province_items(fault: ProvinceFault, survival: bool) {
        province_items_in(fault, survival, 0);
    }

    /// Comme `province_items` ; `pays` (v143) : la province de l'est est une province du pays
    /// (2 × 2 régions) que sert un relais de pays, qui prête lui-même les sacs à ses
    /// provinces (le relais du monde les lui prête). (v148) `pays` = 2 : un pays de 4 × 4
    /// régions, dont la province de 2 × 2 régions est elle-même un pays (les sacs passent par
    /// les trois étages).
    fn province_items_in(fault: ProvinceFault, survival: bool, pays: u8) {
        use minelib_net::items::Pile;
        let (dir, h, mut hosts, mut bots, room) = match pays {
            0 => province_setup(survival),
            1 => province_setup_full(survival, 2, 100, 8, Some((1, 3)), &[]),
            _ => province_setup_full(survival, 4, 100, 10, Some((2, 3)), &[(1, 3)]),
        };
        let mut items: Vec<crate::drops::Items> = bots
            .iter()
            .map(|_| crate::drops::Items::new(false))
            .collect();
        let west = |x: f32| x.clamp(488.0, 508.0);
        let east = |x: f32| x.clamp(516.0, 540.0);
        let mut seed = 4242u32;
        let mut rnd = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        let dt = 0.02;
        // Inventaire de chacun selon son jeu (pierres) ; ramassées dans la province, et
        // lâchés acceptés par la survie là-bas (vus).
        let mut inv: Vec<i64> = vec![1000; bots.len()];
        let mut picked_east = 0u32;
        let mut dropped_east = 0u32;
        // Un passage : bots et hôtes, objets demandés, publiés, gagnés.
        let step_all = |bots: &mut Vec<Bot>,
                        hosts: &mut Vec<(Bot, Bot)>,
                        items: &mut Vec<crate::drops::Items>,
                        inv: &mut Vec<i64>,
                        picked_east: &mut u32,
                        dropped_east: &mut u32| {
            for (i, b) in bots.iter_mut().enumerate() {
                b.tick(dt);
                let it = &mut items[i];
                it.pick_up(b.pos, |_| true);
                let n = b.net.as_mut().expect("réseau");
                for (chunk, req) in std::mem::take(&mut it.outbox) {
                    n.send_item(chunk, &req);
                }
                for m in std::mem::take(&mut n.item_inbox) {
                    if let BuildMsg::Item {
                        to, drop, got, at, ..
                    } = &m
                    {
                        if to.eq_ignore_ascii_case(&b.name) && at[0] >= 512.0 {
                            *picked_east += got.is_some() as u32;
                            *dropped_east += drop.is_some() as u32;
                        }
                    }
                    it.hear(&m, &b.name, None);
                }
                for p in std::mem::take(&mut it.credits) {
                    inv[i] += p.count as i64;
                }
            }
            for (w, g) in hosts.iter_mut() {
                w.pump(&|_| false);
                g.tick(dt);
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        // Avant : la province de l'est reçoit son relais ; en survie, chacun (sauf le
        // propriétaire, OP) casse des blocs du sol autour de lui (ses crédits de minage).
        // (v148 : trois étages à confier l'un après l'autre, un peu plus long.)
        let east_leaf = |bots: &Vec<Bot>| {
            bots.iter().any(|b| {
                b.net.as_ref().is_some_and(|n| {
                    n.province_view()
                        .iter()
                        .any(|v| v.0 == [1, 0] && v.1 == 1 && v.3)
                })
            })
        };
        for step in 0..if pays == 2 { 800 } else { 200 } {
            if pays == 2 && step >= 200 && east_leaf(&bots) {
                break;
            }
            if survival && step % 20 == 5 && step < 125 {
                for b in bots.iter_mut().skip(1) {
                    let x = b.pos.x as i32 - 3 + (rnd() % 7) as i32;
                    let z = (b.pos.z as i32 - 3 + (rnd() % 7) as i32).clamp(0, 31);
                    if b.world.get(x, 10, z) != BlockId::AIR {
                        b.world.set(x, 10, z, BlockId::AIR);
                    }
                }
            }
            step_all(
                &mut bots,
                &mut hosts,
                &mut items,
                &mut inv,
                &mut picked_east,
                &mut dropped_east,
            );
        }
        let t = Instant::now();
        while bots.iter().any(|b| b.net.as_ref().unwrap().pending() > 0) {
            assert!(t.elapsed() < Duration::from_secs(20), "blocs en attente");
            step_all(
                &mut bots,
                &mut hosts,
                &mut items,
                &mut inv,
                &mut picked_east,
                &mut dropped_east,
            );
        }
        for _ in 0..50 {
            step_all(
                &mut bots,
                &mut hosts,
                &mut items,
                &mut inv,
                &mut picked_east,
                &mut dropped_east,
            );
        }
        assert!(
            h.provinces_delegated() >= 1,
            "la province de l'est est servie"
        );
        if pays == 1 {
            assert!(
                h.pays_children().iter().any(|i| i.province == [1, 0]),
                "la province de l'est est servie par une province du pays"
            );
        }
        if pays == 2 {
            assert!(
                east_leaf(&bots),
                "la province de l'est est servie par une province d'un pays de pays"
            );
        }
        // En survie, l'inventaire d'un joueur est son sac (ses crédits de minage).
        if survival {
            for (i, b) in bots.iter().enumerate().skip(1) {
                inv[i] = h.bag_total(room, &b.name) as i64;
                assert!(inv[i] > 0, "{} : des crédits de minage", b.name);
            }
        }
        let before: i64 = inv.iter().sum::<i64>() + h.world_items(room) as i64;
        let mut fault_at: Option<(Instant, [i32; 2])> = None;
        let mut promos_at_fault = 0;
        let mut taken_back: Option<Duration> = None;
        // On lâche, on ramasse, on se déplace (Joueur2 passe d'une province à l'autre) ;
        // la panne au milieu.
        for step in 0..450 {
            let delegated = h.provinces_delegated();
            if fault != ProvinceFault::None && fault_at.is_none() && step >= 200 && delegated > 0 {
                let g = hosts
                    .iter_mut()
                    .map(|(_, g)| g)
                    .find(|g| g.province.as_ref().is_some_and(|p| !p.standby))
                    .expect("hôte de la province");
                let which = g.province.as_ref().expect("province").province;
                match fault {
                    ProvinceFault::Crash => {
                        if let Some(p) = g.province.take() {
                            p.kill();
                        }
                    }
                    ProvinceFault::Leaves => {
                        if let (Some(p), Some(l)) = (g.province.as_mut(), &g.link) {
                            for r in p.release() {
                                transport::push(l, &r);
                            }
                        }
                    }
                    ProvinceFault::Cut => {
                        let (tx, _) = std::sync::mpsc::channel();
                        g.link = Some(tx);
                    }
                    ProvinceFault::None => {}
                }
                fault_at = Some((Instant::now(), which));
                promos_at_fault = h.province_promotions();
            }
            // (Relevée : reprise par le relais du monde, ou confiée à son relais de secours,
            // parfois trop vite pour la voir quitter la liste.)
            if let (Some((t, which)), None) = (fault_at, taken_back) {
                if !h.delegated_provinces().contains(&which)
                    || h.province_promotions() > promos_at_fault
                {
                    taken_back = Some(t.elapsed());
                }
            }
            for (i, b) in bots.iter_mut().enumerate() {
                let r = rnd();
                let x = b.pos.x + (r % 3) as f32 - 1.0;
                b.pos.x = if i == 2 {
                    if (100..300).contains(&step) {
                        (b.pos.x - 0.2).max(490.0)
                    } else {
                        (b.pos.x + 0.2).min(535.0)
                    }
                } else if i < 2 {
                    west(x)
                } else {
                    east(x)
                };
                b.pos.z = (b.pos.z + ((r >> 4) % 3) as f32 - 1.0).clamp(1.0, 30.0);
                // De temps en temps, vers un objet vu (pour le ramasser).
                if (step + i * 7) % 30 == 0 && i != 2 {
                    let near: Vec<[f32; 3]> = items[i]
                        .view
                        .live_drops()
                        .map(|(_, d)| d.pos)
                        .filter(|p| (p[0] - b.pos.x).abs() < 12.0)
                        .collect();
                    if !near.is_empty() {
                        let p = near[rnd() as usize % near.len()];
                        b.pos.x = if i < 2 { west(p[0]) } else { east(p[0]) };
                        b.pos.z = p[2].clamp(1.0, 30.0);
                    }
                }
                // Une pierre lâchée à deux pas, de temps en temps (s'il en a).
                if (step + i * 5) % 15 == 0 && inv[i] > 0 && step < 400 {
                    let a = (rnd() % 628) as f32 / 100.0;
                    let to = Vec3::new(
                        b.pos.x + 2.5 * a.cos(),
                        11.0,
                        (b.pos.z + 2.5 * a.sin()).clamp(1.0, 30.0),
                    );
                    inv[i] -= 1;
                    items[i].drop_item(b.pos + Vec3::Y * 1.5, to, Pile { item: 1, count: 1 });
                }
            }
            step_all(
                &mut bots,
                &mut hosts,
                &mut items,
                &mut inv,
                &mut picked_east,
                &mut dropped_east,
            );
        }
        // Tout se pose (confirmations, sacs rendus, reprises).
        for _ in 0..300 {
            step_all(
                &mut bots,
                &mut hosts,
                &mut items,
                &mut inv,
                &mut picked_east,
                &mut dropped_east,
            );
        }
        let after: i64 = inv.iter().sum::<i64>() + h.world_items(room) as i64;
        eprintln!(
            "objets des provinces : {fault:?}, survie {survival}, avant {before}, après {after}, \
             ramassés à l'est {picked_east}, lâchés à l'est {dropped_east}, sacs prêtés {}, \
             reprise après {taken_back:?}",
            h.province_lends()
        );
        assert!(picked_east > 0, "des objets ramassés dans la province");
        if fault == ProvinceFault::None {
            assert_eq!(after, before, "aucun objet créé ni perdu");
        } else {
            assert!(after <= before, "aucun objet créé : {after} > {before}");
            let t = taken_back.expect("la province revient au relais du monde");
            assert!(
                t < Duration::from_secs(3),
                "reprise en moins de 3 s : {t:?}"
            );
        }
        if survival {
            assert!(
                dropped_east > 0,
                "lâchés en survie dans la province (sac prêté)"
            );
            assert!(
                h.province_lends() >= 2,
                "des sacs prêtés, et qui suivent leur joueur"
            );
            for (i, b) in bots.iter().enumerate().skip(1) {
                let bag = h.bag_total(room, &b.name) as i64;
                if fault == ProvinceFault::None {
                    assert_eq!(bag, inv[i], "{} : sac du relais et inventaire", b.name);
                } else {
                    assert!(
                        inv[i] <= bag,
                        "{} : rien de créé ({} > {bag})",
                        b.name,
                        inv[i]
                    );
                }
            }
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Terrain plat sur les chunks `xs` × `zs` (sol en y = 10).
    fn flat_at(xs: std::ops::Range<i32>, zs: std::ops::Range<i32>) -> World {
        let mut w = World::new(0);
        for cx in xs.clone() {
            for cz in zs.clone() {
                w.insert_empty(ChunkPos { x: cx, z: cz });
            }
        }
        for x in xs.start * 16..xs.end * 16 {
            for z in zs.start * 16..zs.end * 16 {
                w.set(x, 10, z, BlockId::STONE);
            }
        }
        w.journal = Some(Vec::new());
        w
    }

    /// Au fond d'une province (v140) : des provinces de 2 × 2 régions, cinq joueurs au
    /// milieu de l'une d'elles (tout ce qu'ils ont en vue est servi par son relais), sans
    /// relais de région. Ils construisent et se parlent ; l'un d'eux va jusqu'au bord de la
    /// province et revient. Le relais du monde ne reçoit plus leurs positions (sauf celles
    /// de celui qui s'approche du bord), la voix passe par le relais de la province, et à
    /// la fin les mondes sont identiques à celui du propriétaire (loin de là).
    #[test]
    fn deep_in_a_province_players_no_longer_talk_to_the_world_relay() {
        let (dir, h, mut hosts, mut bots, _room) = province_setup_with(false, 2, 1000);
        for (i, b) in bots.iter_mut().enumerate() {
            b.world = flat_at(5..40, 30..34);
            b.pos = if i == 0 {
                Vec3::new(2100.0, 11.0, 520.0)
            } else {
                Vec3::new(480.0 + 15.0 * i as f32, 11.0, 515.0)
            };
        }
        let mut seed = 99u32;
        let mut rnd = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        let dt = 0.02;
        let mut edited = std::collections::BTreeSet::new();
        let mut deep_from: Option<(usize, usize)> = None;
        let mut deep_until: Option<usize> = None;
        let mut voice_from: Option<(usize, usize)> = None;
        for step in 0..450 {
            if step == 60 {
                for b in bots.iter_mut().skip(1) {
                    let link = b.link.clone().expect("file d'envoi");
                    b.voice = Some(crate::voice::Voice::new(link));
                }
            }
            let all_deep = bots
                .iter()
                .skip(1)
                .all(|b| b.net.as_ref().is_some_and(|n| n.was_deep));
            if all_deep && deep_from.is_none() && step < 150 {
                deep_from = Some((step, h.positions_seen()));
                let by_prov: usize = hosts
                    .iter()
                    .filter_map(|(_, g)| g.province.as_ref())
                    .map(|p| p.voice_relayed())
                    .sum();
                voice_from = Some((by_prov, h.voice_relayed()));
            }
            if step == 150 {
                deep_until = Some(h.positions_seen());
            }
            if step >= 100 && step % 10 == 0 {
                for b in bots.iter_mut().skip(1) {
                    if let Some(v) = &mut b.voice {
                        v.talk_once();
                    }
                }
            }
            for (i, b) in bots.iter_mut().enumerate() {
                if i == 0 {
                    b.tick(dt);
                    continue;
                }
                let r = rnd();
                // Joueur5 va jusqu'au bord ouest de la province, puis revient.
                b.pos.x = if i == 5 {
                    if (150..300).contains(&step) {
                        (b.pos.x - 3.0).max(100.0)
                    } else {
                        (b.pos.x + 3.0).min(555.0)
                    }
                } else {
                    (b.pos.x + (r % 3) as f32 - 1.0).clamp(470.0, 560.0)
                };
                b.pos.z = (b.pos.z + ((r >> 4) % 3) as f32 - 1.0).clamp(500.0, 530.0);
                if rnd() % 17 == 0 {
                    let x = (b.pos.x as i32 - 10 + (rnd() % 21) as i32).clamp(5 * 16, 40 * 16 - 1);
                    let z = (b.pos.z as i32 - 10 + (rnd() % 21) as i32).clamp(30 * 16, 34 * 16 - 1);
                    let y = 11 + (rnd() % 3) as i32;
                    let block = if rnd() % 4 == 0 {
                        BlockId::AIR
                    } else {
                        BlockId(1 + rnd() as u16 % 8)
                    };
                    b.world.set(x, y, z, block);
                    edited.insert([x, y, z]);
                }
                b.tick(dt);
            }
            for (w, g) in &mut hosts {
                w.pump(&|_| false);
                g.tick(dt);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        for _ in 0..300 {
            for b in bots.iter_mut() {
                b.tick(dt);
            }
            for (w, g) in &mut hosts {
                w.pump(&|_| false);
                g.tick(dt);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let (from_step, seen0) = deep_from.expect("tous au fond de la province");
        let seen1 = deep_until.expect("étape 150");
        let by_prov: usize = hosts
            .iter()
            .filter_map(|(_, g)| g.province.as_ref())
            .map(|p| p.voice_relayed())
            .sum();
        let (prov0, root0) = voice_from.expect("voix");
        let (by_prov, by_root) = (by_prov - prov0, h.voice_relayed() - root0);
        eprintln!(
            "au fond d'une province : tous depuis l'étape {from_step}, positions reçues par le \
             relais du monde ensuite {} (jusqu'à l'étape 150), {} en tout ; voix : {by_prov} \
             messages par le relais de la province, {by_root} par le relais du monde",
            seen1 - seen0,
            h.positions_seen()
        );
        assert!(h.provinces_delegated() >= 1, "la province est servie");
        assert!(
            seen1 - seen0 <= 2,
            "au fond de la province, plus de positions au relais du monde : {}",
            seen1 - seen0
        );
        assert!(
            h.positions_seen() > seen1,
            "celui qui s'approche du bord les redonne"
        );
        assert!(
            by_prov > 20 * by_root.max(1),
            "la voix passe par le relais de la province ({by_prov} contre {by_root})"
        );
        for b in &bots {
            assert_eq!(
                b.net.as_ref().unwrap().pending(),
                0,
                "{} : en attente",
                b.name
            );
        }
        // (Chacun a ce qui est en vue de là où il est : le reste lui arrivera en s'en
        // approchant.)
        let owner = &bots[0];
        let mut differ = Vec::new();
        let mut compared = 0;
        for b in &bots[1..] {
            for p in edited
                .iter()
                .filter(|p| (p[0] as f32 - b.pos.x).abs() < 128.0)
            {
                compared += 1;
                let (a, o) = (
                    b.world.get(p[0], p[1], p[2]),
                    owner.world.get(p[0], p[1], p[2]),
                );
                if a != o {
                    differ.push((b.name.clone(), *p, a.0, o.0));
                }
            }
        }
        assert!(compared > 100, "assez de cases comparées : {compared}");
        assert!(
            differ.is_empty(),
            "{} cases différentes du monde du propriétaire, par exemple {:?}",
            differ.len(),
            &differ[..differ.len().min(5)]
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// (v224) Lumières de l'éditeur : l'OP (le propriétaire, à l'ouest) en pose une de chaque
    /// côté de la frontière (la province de l'est est servie par le relais d'un joueur, qui
    /// l'arbitre ; l'ouest par le relais du monde), en déplace une de l'ouest à l'est (d'un
    /// arbitre à l'autre), en change une, en retire une ; un joueur de l'est qui n'est pas OP
    /// en pose une : refusée, elle disparaît chez lui. Tous finissent avec les lumières de
    /// l'OP, et rien n'est en attente.
    /// Un passage de l'essai des lumières : bots, hôtes, scènes, partages, versions des
    /// scènes, refus reçus.
    type LightStep<'a> = dyn FnMut(
            &mut Vec<Bot>,
            &mut Vec<(Bot, Bot)>,
            &mut Vec<Vec<crate::lights::PlacedLight>>,
            &mut Vec<crate::lightnet::LightSync>,
            &mut Vec<u64>,
            &mut Vec<u32>,
        ) + 'a;

    /// (v229) La foudre d'un OP à travers les provinces : demandée à l'arbitre du chunk où
    /// elle tombe (le relais du monde à l'ouest, le relais de la province à l'est, tenu par
    /// un joueur d'un autre monde), elle arrive à tous les joueurs proches, des deux côtés
    /// de la frontière, avec la même graine ; celle d'un joueur qui n'est pas OP est
    /// refusée par l'arbitre, à lui seul.
    #[test]
    fn lightning_thrown_by_an_op_reaches_everyone_through_provinces() {
        let (dir, h, mut hosts, mut bots, _room) = province_setup(false);
        let dt = 0.02;
        let step = |bots: &mut Vec<Bot>, hosts: &mut Vec<(Bot, Bot)>| {
            for b in bots.iter_mut() {
                b.tick(dt);
            }
            for (w, g) in hosts.iter_mut() {
                w.pump(&|_| false);
                g.tick(dt);
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let ready = |bots: &Vec<Bot>| {
            bots.iter().all(|b| {
                b.net.as_ref().is_some_and(|n| {
                    n.province_view()
                        .iter()
                        .any(|v| v.0 == [1, 0] && v.3)
                })
            })
        };
        let t = Instant::now();
        while !ready(&bots) {
            assert!(t.elapsed() < Duration::from_secs(40), "province de l'est");
            step(&mut bots, &mut hosts);
        }
        assert!(h.provinces_delegated() >= 1);
        let ask = |seq: u32, x: f32, up: bool, seed: u32| BuildMsg::ThorAsk {
            seq,
            chunk: chunk_of(x.floor() as i32, 16),
            up,
            pos: [x, 12.0, 16.5],
            dir: if up { [0.0, 1.0, 0.0] } else { [0.0, -1.0, 0.0] },
            seed,
        };
        // L'OP : une à l'ouest (relais du monde), une à l'est (province), une qui monte.
        // Joueur3 (à l'est, pas OP) : une aussi, à l'est. La province vient d'être confiée :
        // tant que son relais n'a pas son bail, il refuse (« pas prêt ») ; un éclair n'attend
        // pas, on le redemande (sous un nouveau numéro) jusqu'à ce qu'il réponde pour de bon.
        let send = |b: &mut Bot, m: BuildMsg| b.net.as_mut().expect("réseau").send_thor(&m);
        let seeds = |b: &Bot| {
            let mut s: Vec<(u32, bool, String)> = b
                .thor_seen
                .iter()
                .filter_map(|m| match m {
                    BuildMsg::Thor { seed, up, by, .. } => Some((*seed, *up, by.clone())),
                    _ => None,
                })
                .collect();
            s.sort();
            s
        };
        let refused = |b: &Bot| {
            b.thor_seen
                .iter()
                .any(|m| matches!(m, BuildMsg::ThorDenied { why, .. } if *why == game::THOR_NOT_OP))
        };
        let want = vec![
            (111, false, bots[0].name.clone()),
            (222, false, bots[0].name.clone()),
            (333, true, bots[0].name.clone()),
        ];
        let plan = [(500.5, false, 111), (530.5, false, 222), (531.5, true, 333)];
        let mut seq = 0;
        let mut tries = 0;
        let t = Instant::now();
        loop {
            if tries % 40 == 0 {
                let got = seeds(&bots[0]);
                for (x, up, seed) in plan {
                    if !got.iter().any(|g| g.0 == seed) {
                        seq += 1;
                        send(&mut bots[0], ask(seq, x, up, seed));
                    }
                }
                if !refused(&bots[3]) {
                    seq += 1;
                    send(&mut bots[3], ask(seq, 529.5, false, 999));
                }
            }
            tries += 1;
            step(&mut bots, &mut hosts);
            if refused(&bots[3]) && bots.iter().all(|b| seeds(b) == want) {
                break;
            }
            assert!(
                t.elapsed() < Duration::from_secs(30),
                "foudres reçues : {:?}",
                bots.iter().map(|b| b.thor_seen.clone()).collect::<Vec<_>>()
            );
        }
        // Encore un moment : rien n'arrive en double, le refus n'est allé qu'à Joueur3, et
        // c'est parce qu'il n'est pas OP.
        for _ in 0..50 {
            step(&mut bots, &mut hosts);
        }
        for (i, b) in bots.iter().enumerate() {
            assert_eq!(seeds(b), want, "{} : chaque foudre une fois", b.name);
            let denied: Vec<&BuildMsg> = b
                .thor_seen
                .iter()
                .filter(|m| matches!(m, BuildMsg::ThorDenied { .. }))
                .collect();
            // (Les refus « pas prêt » du début ne vont qu'à ceux qui ont demandé ; « pas OP »
            // qu'à Joueur3, une fois.)
            let not_op = denied
                .iter()
                .filter(|m| matches!(m, BuildMsg::ThorDenied { why, .. } if *why == game::THOR_NOT_OP))
                .count();
            assert_eq!(not_op, usize::from(i == 3), "{} : {denied:?}", b.name);
            assert!(i == 0 || i == 3 || denied.is_empty(), "{} : {denied:?}", b.name);
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn lights_placed_by_an_op_reach_everyone_through_provinces() {
        use crate::lightnet::LightSync;
        use crate::lights::{Kind, PlacedLight};
        let (dir, h, mut hosts, mut bots, _room) = province_setup(false);
        let n = bots.len();
        let mut scenes: Vec<Vec<PlacedLight>> = vec![Vec::new(); n];
        let mut syncs: Vec<LightSync> = bots.iter().map(|b| LightSync::new(&b.name, &[])).collect();
        let mut gens = vec![0u64; n];
        let mut denied = vec![0u32; n];
        let mut next_id = 1u64;
        let dt = 0.02;
        let mut step = |bots: &mut Vec<Bot>,
                        hosts: &mut Vec<(Bot, Bot)>,
                        scenes: &mut Vec<Vec<PlacedLight>>,
                        syncs: &mut Vec<LightSync>,
                        gens: &mut Vec<u64>,
                        denied: &mut Vec<u32>| {
            for (i, b) in bots.iter_mut().enumerate() {
                b.tick(dt);
                let net = b.net.as_mut().expect("réseau");
                let inbox = std::mem::take(&mut net.item_inbox);
                let mut id = || {
                    next_id += 1;
                    next_id
                };
                for m in &inbox {
                    let heard = syncs[i].hear(m, &mut scenes[i], &mut id);
                    if heard.changed {
                        gens[i] += 1;
                    }
                    denied[i] += heard.denied as u32;
                }
                for r in syncs[i].outgoing(&scenes[i], gens[i], dt) {
                    net.send_light(&r);
                }
            }
            for (w, g) in hosts.iter_mut() {
                w.pump(&|_| false);
                g.tick(dt);
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        // La province de l'est reçoit son relais, et tous y entrent (elle est en vue).
        let ready = |bots: &Vec<Bot>| {
            bots.iter().all(|b| {
                b.net.as_ref().is_some_and(|n| {
                    n.province_view()
                        .iter()
                        .any(|v| v.0 == [1, 0] && v.3)
                })
            })
        };
        let t = Instant::now();
        while !ready(&bots) {
            assert!(t.elapsed() < Duration::from_secs(40), "province de l'est");
            step(&mut bots, &mut hosts, &mut scenes, &mut syncs, &mut gens, &mut denied);
        }
        assert!(h.provinces_delegated() >= 1);
        let lamp = |uid: u64, x: f64, name: &str| PlacedLight {
            id: uid,
            uid,
            name: name.into(),
            kind: Kind::Spot,
            position: [x, 14.0, 16.0],
            color: [0.3, 0.6, 1.0],
            ..Default::default()
        };
        // L'OP pose une lumière de chaque côté ; Joueur3 (à l'est, pas OP) aussi.
        scenes[0].push(lamp(101, 500.5, "Ouest"));
        scenes[0].push(lamp(102, 530.5, "Est"));
        gens[0] += 1;
        scenes[3].push(lamp(301, 528.5, "Pirate"));
        gens[3] += 1;
        let same = |scenes: &Vec<Vec<PlacedLight>>| {
            let key = |v: &Vec<PlacedLight>| {
                let mut k: Vec<String> = v.iter().map(|l| l.to_text()).collect();
                k.sort();
                k
            };
            scenes.iter().all(|s| key(s) == key(&scenes[0]))
        };
        let settle = |bots: &mut Vec<Bot>,
                      hosts: &mut Vec<(Bot, Bot)>,
                      scenes: &mut Vec<Vec<PlacedLight>>,
                      syncs: &mut Vec<LightSync>,
                      gens: &mut Vec<u64>,
                      denied: &mut Vec<u32>,
                      step: &mut LightStep,
                      what: &str| {
            let t = Instant::now();
            loop {
                step(bots, hosts, scenes, syncs, gens, denied);
                if same(scenes) && syncs.iter().all(|s| s.pending() == 0) {
                    break;
                }
                assert!(
                    t.elapsed() < Duration::from_secs(20),
                    "{what} : {:?}",
                    scenes
                        .iter()
                        .map(|s| s.iter().map(|l| (l.name.clone(), l.position[0])).collect::<Vec<_>>())
                        .collect::<Vec<_>>()
                );
            }
            // (Les retraits des zones quittées partent juste après.)
            for _ in 0..25 {
                step(bots, hosts, scenes, syncs, gens, denied);
            }
            assert!(same(scenes), "{what} : encore pareil");
        };
        settle(
            &mut bots, &mut hosts, &mut scenes, &mut syncs, &mut gens, &mut denied, &mut step,
            "posées",
        );
        assert_eq!(
            scenes[0].len(),
            2,
            "les deux de l'OP, pas celle de Joueur3 : {:?} refus {denied:?}",
            scenes[0].iter().map(|l| &l.name).collect::<Vec<_>>()
        );
        assert!(denied[3] >= 1 && denied[0] == 0);
        assert!(!scenes[3].iter().any(|l| l.uid == 301));
        // Il déplace celle de l'ouest à l'est (de l'arbitre du relais du monde à celui de
        // la province), change la couleur de celle de l'est.
        scenes[0][0].position[0] = 524.5;
        scenes[0][1].color = [1.0, 0.1, 0.1];
        gens[0] += 1;
        settle(
            &mut bots, &mut hosts, &mut scenes, &mut syncs, &mut gens, &mut denied, &mut step,
            "déplacée",
        );
        for s in &scenes {
            let w = s.iter().filter(|l| l.uid == 101).collect::<Vec<_>>();
            assert_eq!(w.len(), 1, "une seule fois");
            assert_eq!(w[0].position[0], 524.5);
        }
        // Il retire celle de l'est.
        scenes[0].retain(|l| l.uid != 102);
        gens[0] += 1;
        settle(
            &mut bots, &mut hosts, &mut scenes, &mut syncs, &mut gens, &mut denied, &mut step,
            "retirée",
        );
        assert!(scenes.iter().all(|s| s.len() == 1 && s[0].uid == 101));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn items_in_a_province_are_arbitrated_there_and_nothing_is_created() {
        province_items(ProvinceFault::None, false);
    }

    #[test]
    fn survival_bags_follow_their_player_from_province_to_province() {
        province_items(ProvinceFault::None, true);
    }

    #[test]
    fn survival_bags_lose_nothing_when_a_province_host_falls() {
        province_items(ProvinceFault::Crash, true);
    }

    #[test]
    fn items_in_a_pays_province_are_arbitrated_there_and_nothing_is_created() {
        province_items_in(ProvinceFault::None, false, 1);
    }

    #[test]
    fn survival_bags_go_through_the_pays_to_its_provinces() {
        province_items_in(ProvinceFault::None, true, 1);
    }

    #[test]
    fn survival_bags_go_through_three_levels_of_pays() {
        province_items_in(ProvinceFault::None, true, 2);
    }

    /// (v142) Grand monde : la copie de ce que publie la province est gardée par son relais
    /// de secours, plus par le relais du monde (sa copie ne grandit plus pendant qu'on
    /// construit). Puis `crash_primary` : le relais de la province tombe (son relais de
    /// secours prend sa place avec tout) ; sinon c'est le relais de secours qui tombe (tout
    /// repart au relais du monde). Dans les deux cas, tout revient au relais du monde, et à
    /// la fin les mondes sont identiques case par case à celui du propriétaire.
    fn province_big(crash_primary: bool) {
        let (dir, h, mut hosts, mut bots, room) = province_setup(false);
        h.set_copies_until(0);
        let east = [1, 0];
        let west = |x: f32| x.clamp(488.0, 508.0);
        let eastx = |x: f32| x.clamp(516.0, 540.0);
        let mut seed = 4711u32;
        let mut rnd = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        let dt = 0.02;
        let mut edited = std::collections::BTreeSet::new();
        let mut big_at: Option<usize> = None;
        let mut big_entries: Option<usize> = None;
        let mut before_fault: Option<usize> = None;
        let mut fault_at: Option<Instant> = None;
        let mut edits_big = 0;
        // (Jusqu'à un peu après la panne, même sur une machine chargée : la copie gardée
        // ailleurs peut mettre plus ou moins longtemps à arriver.)
        let mut fault_step: Option<usize> = None;
        for step in 0..3000 {
            if fault_step.is_some_and(|f| step >= f + 320) {
                break;
            }
            if big_at.is_none() && h.provinces_big() > 0 {
                big_at = Some(step);
            }
            // (La copie du relais du monde, une fois le changement arrivé partout.)
            if big_at.is_some_and(|b| step == b + 25) {
                big_entries = Some(h.province_entries(room, east));
            }
            if big_at.is_some_and(|b| step == b + 200) {
                before_fault = Some(h.province_entries(room, east));
                let g = hosts
                    .iter_mut()
                    .map(|(_, g)| g)
                    .find(|g| {
                        g.province
                            .as_ref()
                            .is_some_and(|p| p.standby != crash_primary)
                    })
                    .expect("relais à faire tomber");
                if let Some(p) = g.province.take() {
                    p.kill();
                }
                fault_at = Some(Instant::now());
                fault_step = Some(step);
            }
            for (i, b) in bots.iter_mut().enumerate() {
                let r = rnd();
                let x = b.pos.x + (r % 3) as f32 - 1.0;
                b.pos.x = if i < 2 { west(x) } else { eastx(x) };
                b.pos.z = (b.pos.z + ((r >> 4) % 3) as f32 - 1.0).clamp(1.0, 30.0);
                if rnd() % 17 == 0 && fault_step.is_none_or(|f| step < f + 220) {
                    let x = (b.pos.x as i32 - 12 + (rnd() % 25) as i32).clamp(29 * 16, 36 * 16 - 1);
                    let z = (rnd() % 32) as i32;
                    let y = 11 + (rnd() % 3) as i32;
                    let block = if rnd() % 4 == 0 {
                        BlockId::AIR
                    } else {
                        BlockId(1 + rnd() as u16 % 8)
                    };
                    b.world.set(x, y, z, block);
                    edited.insert([x, y, z]);
                    if x >= 512 && big_at.is_some_and(|b| step > b + 25) && fault_at.is_none() {
                        edits_big += 1;
                    }
                }
                b.tick(dt);
            }
            for (w, g) in &mut hosts {
                w.pump(&|_| false);
                g.tick(dt);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        for _ in 0..400 {
            for b in bots.iter_mut() {
                b.tick(dt);
            }
            for (w, g) in &mut hosts {
                w.pump(&|_| false);
                g.tick(dt);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let (b0, b1) = (
            big_entries.expect("copie gardée par le relais de secours"),
            before_fault.expect("panne"),
        );
        eprintln!(
            "grand monde : copie chez le relais de secours dès l'étape {big_at:?} ; copie du \
             relais du monde {b0} puis {b1} entrées malgré {edits_big} modifications dans la \
             province ; à la fin {} entrées, promotions {}",
            h.province_entries(room, east),
            h.province_promotions()
        );
        assert!(edits_big > 20, "assez de modifications : {edits_big}");
        assert!(
            b1 <= b0 + 2,
            "la copie du relais du monde ne grandit plus ({b0} puis {b1})"
        );
        if crash_primary {
            assert!(
                h.province_promotions() >= 1,
                "le relais de secours a pris sa place"
            );
        }
        for b in &bots {
            assert_eq!(
                b.net.as_ref().unwrap().pending(),
                0,
                "{} : en attente",
                b.name
            );
        }
        // Tout est revenu au relais du monde : le propriétaire (qui reçoit tout de lui pour
        // enregistrer le monde) a ce que tout le monde a.
        let owner = &bots[0];
        let mut differ = Vec::new();
        for b in &bots[1..] {
            for p in &edited {
                let (a, o) = (
                    b.world.get(p[0], p[1], p[2]),
                    owner.world.get(p[0], p[1], p[2]),
                );
                if a != o {
                    differ.push((b.name.clone(), *p, a.0, o.0));
                }
            }
        }
        assert!(
            differ.is_empty(),
            "{} cases différentes du monde du propriétaire, par exemple {:?}",
            differ.len(),
            &differ[..differ.len().min(5)]
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_big_world_province_copy_is_kept_by_its_standby_then_returns() {
        province_big(false);
    }

    #[test]
    fn a_big_world_province_whose_host_falls_loses_nothing() {
        province_big(true);
    }

    #[test]
    fn a_crowded_province_gets_its_own_relay_and_worlds_stay_identical() {
        province_world(ProvinceFault::None);
    }

    #[test]
    fn a_province_whose_host_falls_comes_back_within_three_seconds() {
        province_world(ProvinceFault::Crash);
    }

    #[test]
    fn a_province_whose_host_leaves_is_handed_back_at_once() {
        province_world(ProvinceFault::Leaves);
    }

    #[test]
    fn a_province_cut_from_the_world_relay_stops_without_splitting_the_world() {
        province_world(ProvinceFault::Cut);
    }

    #[test]
    fn a_cheating_primary_host_is_caught_and_replaced() {
        run(Scenario {
            players: 8,
            shards: true,
            two_relays: true,
            forger: true,
            ..Default::default()
        });
    }

    #[test]
    fn build_worlds_stay_identical_when_the_first_of_two_relays_leaves() {
        run(Scenario {
            players: 8,
            shards: true,
            two_relays: true,
            garant_leaves: true,
            ..Default::default()
        });
    }

    #[test]
    fn build_worlds_stay_identical_when_the_region_relay_leaves() {
        run(Scenario {
            players: 6,
            shards: true,
            garant_leaves: true,
            ..Default::default()
        });
    }

    /// (v275) Deux joueurs au même endroit, par un vrai relais : chacun voit arriver les
    /// positions de l'autre au rythme de ses images, jusqu'à 256 par seconde (avant : 60
    /// envoyées, et 50 livrées au plus, regroupées par 20 ms).
    #[test]
    fn two_neighbours_see_each_other_at_every_frame() {
        let dir = std::env::temp_dir().join(format!("minelib-proches-{}", rand_u32()));
        let h = minelib_relay::start(minelib_relay::Config {
            dir: dir.clone(),
            bind: "127.0.0.1:0".into(),
            pow_bits: 4,
            name: "essai".into(),
        })
        .unwrap();
        let addr = h.addr.to_string();
        let mut a = Bot::new(&addr, h.public, "Proprio", false);
        a.c.send(Cmd::Host {
            kind: 1,
            name: "Essai".into(),
            map: "essai".into(),
            map_sig: [7; 32],
            max_players: 0,
        });
        let mut room = None;
        let t = Instant::now();
        while room.is_none() {
            while let Some(e) = a.c.poll() {
                match e {
                    Event::Hosted(r) => room = Some(r),
                    Event::Link(l) => a.link = Some(l),
                    _ => {}
                }
            }
            assert!(t.elapsed() < Duration::from_secs(20));
            std::thread::sleep(Duration::from_millis(5));
        }
        a.start_net(true);
        let mut b = Bot::new(&addr, h.public, "Joueur1", false);
        b.c.send(Cmd::Join(room.unwrap()));
        b.wait(|e| matches!(e, Event::Joined { .. }));
        b.start_net(false);
        // Une image toutes les 2 à 3 ms (plus de 256 par seconde quand la machine suit),
        // pendant 3 s ; compté sur les 2 dernières : les images de B où une position de A
        // vient d'arriver (`seen` repart de zéro).
        let (mut frames, mut got) = (0u32, 0u32);
        let (t, mut last) = (Instant::now(), Instant::now());
        while t.elapsed() < Duration::from_secs(3) {
            let dt = last.elapsed().as_secs_f32();
            last = Instant::now();
            a.pos.x += 4.0 * dt;
            a.tick(dt);
            b.tick(dt);
            if t.elapsed() > Duration::from_secs(1) {
                frames += 1;
                let seen = b.net.as_ref().and_then(|n| n.others.get("Proprio"));
                got += u32::from(seen.is_some_and(|o| o.seen <= dt));
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        eprintln!("PROCHES {frames} images en 2 s, {got} avec une position de l'autre");
        // Machine chargée : moins d'images, donc moins de positions (une par image au
        // plus) ; on demande la moitié de ce qui pouvait arriver, 256 par seconde au plus.
        // Avant, 100 au plus en 2 s.
        assert!(frames > 150, "{frames} images : machine trop chargée pour juger");
        assert!(got * 2 >= frames.min(512), "{got} positions pour {frames} images");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// (v285) Six joueurs au même endroit, par un vrai relais : chacun voit arriver à
    /// chaque image les positions de ses 3 voisins les plus proches (256 par seconde quand
    /// la machine suit), et celles des 2 autres au rythme d'avant (60 par seconde).
    #[test]
    fn in_a_crowd_the_three_closest_are_seen_at_every_frame() {
        let dir = std::env::temp_dir().join(format!("minelib-foule-{}", rand_u32()));
        let h = minelib_relay::start(minelib_relay::Config {
            dir: dir.clone(),
            bind: "127.0.0.1:0".into(),
            pow_bits: 4,
            name: "essai".into(),
        })
        .unwrap();
        let addr = h.addr.to_string();
        let mut bots = vec![Bot::new(&addr, h.public, "Proprio", false)];
        bots[0].c.send(Cmd::Host {
            kind: 1,
            name: "Essai".into(),
            map: "essai".into(),
            map_sig: [7; 32],
            max_players: 0,
        });
        let mut room = None;
        let t = Instant::now();
        while room.is_none() {
            while let Some(e) = bots[0].c.poll() {
                match e {
                    Event::Hosted(r) => room = Some(r),
                    Event::Link(l) => bots[0].link = Some(l),
                    _ => {}
                }
            }
            assert!(t.elapsed() < Duration::from_secs(20));
            std::thread::sleep(Duration::from_millis(5));
        }
        bots[0].start_net(true);
        for i in 1..6 {
            let mut b = Bot::new(&addr, h.public, &format!("Joueur{i}"), false);
            b.c.send(Cmd::Join(room.unwrap()));
            b.wait(|e| matches!(e, Event::Joined { .. }));
            b.start_net(false);
            bots.push(b);
        }
        // Une image toutes les 2 à 4 ms pendant 4 s ; compté sur les 2 dernières : pour
        // le premier joueur, les images où une position de chacun des autres vient
        // d'arriver.
        let mut frames = 0u32;
        let mut got: HashMap<String, u32> = HashMap::new();
        let (t, mut last) = (Instant::now(), Instant::now());
        while t.elapsed() < Duration::from_secs(4) {
            let dt = last.elapsed().as_secs_f32();
            last = Instant::now();
            for (i, b) in bots.iter_mut().enumerate() {
                b.pos.x += (i as f32 - 2.5) * 0.2 * dt;
                b.tick(dt);
            }
            if t.elapsed() > Duration::from_secs(2) {
                frames += 1;
                for (name, o) in &bots[0].net.as_ref().unwrap().others {
                    *got.entry(name.clone()).or_default() += u32::from(o.seen <= dt);
                }
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        let mut counts: Vec<u32> = got.values().copied().collect();
        counts.sort_unstable_by(|a, b| b.cmp(a));
        eprintln!("FOULE {frames} images en 2 s ; positions reçues de chacun : {counts:?}");
        assert_eq!(counts.len(), 5, "les 5 autres sont vus");
        assert!(frames > 150, "{frames} images : machine trop chargée pour juger");
        // Les 3 plus proches : au moins la moitié de ce qui pouvait arriver (une par image,
        // 256 par seconde au plus). Les 2 autres : 60 par seconde, soit 120 (un peu plus
        // quand la machine hoquette) ; jugé seulement quand il y a assez d'images pour
        // les distinguer.
        assert!(counts[2] * 2 >= frames.min(512), "{counts:?} pour {frames} images");
        if frames >= 400 {
            assert!(counts[3] <= 170, "{counts:?} : les autres au rythme d'avant");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn far_players_move_smoothly_with_fewer_messages() {
        // Un joueur lointain n'envoie qu'une position par seconde : on le voit avancer à
        // vitesse régulière, pas par bonds.
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut b = BuildNet::new(tx, "Moi".into(), false);
        let mut w = World::new(0);
        let msg = |x: f32| {
            game::encode(&BuildMsg::Player {
                pseudo: "Loin".into(),
                pos: [x, 64.0, 0.0],
                yaw: 0.0,
                pitch: 0.0,
                crouch: 0.0,
                speed: 4.0,
                ping: 30,
                held: 0,
            })
        };
        let dt = 0.05;
        let mut x = 0.0;
        let mut shown = Vec::new();
        for step in 0..120 {
            if step % 20 == 0 {
                b.receive(7, [0, 0], &msg(x), &mut w);
                x += 4.0;
            }
            let _ = b.update(&mut w, Vec3::ZERO, (0.0, 0.0), 0.0, 0.0, 0.0, dt);
            shown.push(b.others["Loin"].pos.x);
        }
        // Après la mise en route : jamais de bond de plus de 0,5 bloc d'une image à l'autre.
        let jumps = shown[60..]
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0, f32::max);
        assert!(jumps < 0.5, "bond de {jumps}");
        assert!(shown[119] > 15.0, "il avance : {}", shown[119]);
    }

    /// (v222) Ce que tient un autre joueur arrive avec sa position : la lampe torche allumée à
    /// tel cran, éteinte, la Foudre de Zeus, puis rien.
    #[test]
    fn another_players_held_item_comes_with_his_position() {
        use minelib_net::poses::{Held, Lamp};
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut b = BuildNet::new(tx, "Moi".into(), false);
        let mut w = World::new(0);
        let msg = |held: u8| {
            game::encode(&BuildMsg::Player {
                pseudo: "Lampe".into(),
                pos: [2.0, 64.0, 0.0],
                yaw: 0.0,
                pitch: 0.0,
                crouch: 0.0,
                speed: 0.0,
                ping: 30,
                held,
            })
        };
        let on = Lamp { on: true, step: 3 };
        b.receive(7, [0, 0], &msg(on.byte()), &mut w);
        let _ = b.update(&mut w, Vec3::ZERO, (0.0, 0.0), 0.0, 0.0, 0.0, 0.05);
        assert_eq!(b.others["Lampe"].held, Some(Held::Lamp(on)));
        let off = Lamp { on: false, step: 10 };
        b.receive(7, [0, 0], &msg(off.byte()), &mut w);
        assert_eq!(b.others["Lampe"].held, Some(Held::Lamp(off)));
        b.receive(7, [0, 0], &msg(Held::Zeus.byte()), &mut w);
        assert_eq!(b.others["Lampe"].held, Some(Held::Zeus));
        b.receive(7, [0, 0], &msg(0), &mut w);
        assert_eq!(b.others["Lampe"].held, None);
    }

    /// (v227) Foule d'essai : celle d'un OP arrive (remise par un relais, pas par un joueur ; la
    /// nôtre ne nous revient pas), les coups à la nôtre aussi, le refus de la nôtre ; la nôtre
    /// et nos coups partent avec nos positions, à notre nom.
    #[test]
    fn test_crowds_come_and_go_with_positions() {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut b = BuildNet::new(tx, "Moi".into(), false);
        let mut w = World::new(0);
        let data = vec![1u8, 2, 3];
        let crowd = |owner: &str| {
            game::encode(&BuildMsg::Crowd {
                owner: owner.into(),
                data: data.clone(),
            })
        };
        b.receive(0, [0, 0], &crowd("Op"), &mut w);
        b.receive(9, [0, 0], &crowd("Tricheur"), &mut w);
        b.receive(0, [0, 0], &crowd("moi"), &mut w);
        assert_eq!(b.crowd_inbox, vec![("Op".to_string(), data.clone())]);
        let hit = |owner: &str| {
            game::encode(&BuildMsg::CrowdHit {
                owner: owner.into(),
                by: "Eve".into(),
                id: 7,
                from: [1.0, 2.0, 3.0],
            })
        };
        b.receive(0, [0, 0], &hit("MOI"), &mut w);
        b.receive(0, [0, 0], &hit("Op"), &mut w);
        assert_eq!(b.crowd_hits, vec![("Eve".to_string(), 7, [1.0, 2.0, 3.0])]);
        b.receive(0, [0, 0], &game::encode(&BuildMsg::CrowdDenied), &mut w);
        assert!(b.crowd_denied);
        // Pas encore placé : rien ne part ; placé, la foule et le coup partent avec nos
        // positions (`Nearby`), à notre nom.
        b.send_crowd(vec![4, 5]);
        assert!(rx.try_recv().is_err());
        let _ = b.update(&mut w, Vec3::new(40.0, 64.0, -20.0), (0.0, 0.0), 0.0, 0.0, 0.0, 0.05);
        while rx.try_recv().is_ok() {}
        b.send_crowd(vec![4, 5]);
        b.send_crowd_hit("Op", 3, [1.0, 2.0, 3.0]);
        let mut sent = Vec::new();
        while let Ok(m) = rx.try_recv() {
            if let transport::Inbound::Push(bytes) = m {
                if let Some(Request::Nearby { chunk, data }) = game::decode::<Request>(&bytes) {
                    sent.push((chunk, game::decode::<BuildMsg>(&data).unwrap()));
                }
            }
        }
        assert_eq!(
            sent,
            vec![
                (
                    [2, -2],
                    BuildMsg::Crowd {
                        owner: "Moi".into(),
                        data: vec![4, 5]
                    }
                ),
                (
                    [2, -2],
                    BuildMsg::CrowdHit {
                        owner: "Op".into(),
                        by: "Moi".into(),
                        id: 3,
                        from: [1.0, 2.0, 3.0]
                    }
                )
            ]
        );
    }

    #[test]
    fn a_refused_edit_gives_back_what_the_relay_last_published() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut b = BuildNet::new(tx, "Moi".into(), false);
        let mut w = World::new(0);
        w.ensure_chunk(minelib_world::ChunkPos::containing(1, 1));
        w.journal = Some(Vec::new());
        let pos = [1, 100, 1];
        let before = w.get(1, 100, 1);
        let step = |b: &mut BuildNet, w: &mut World| {
            let _ = b.update(w, Vec3::ZERO, (0.0, 0.0), 0.0, 0.0, 0.0, 0.05);
        };
        let from_garant = |b: &mut BuildNet, w: &mut World, m: BuildMsg| {
            b.receive(9, [0, 0], &game::encode(&m), w);
        };
        // (Chaque modification publiée porte un rang plus grand que la précédente.)
        let rank = std::cell::Cell::new(0u64);
        let set = |block: u16| {
            rank.set(rank.get() + 1);
            BuildMsg::Set {
                pos,
                block,
                by: String::new(),
                seq: rank.get(),
            }
        };
        // Deux modifications de suite au même endroit ; le relais accepte la première et
        // refuse la seconde (trop vite) : c'est la première qui reste, pas le bloc d'avant.
        w.set(1, 100, 1, BlockId(1));
        step(&mut b, &mut w);
        w.set(1, 100, 1, BlockId(2));
        step(&mut b, &mut w);
        assert_eq!(b.pending(), 1);
        from_garant(&mut b, &mut w, set(1));
        from_garant(&mut b, &mut w, BuildMsg::Reject { pos, peer: 0 });
        assert_eq!(w.get(1, 100, 1), BlockId(1));
        assert_eq!(b.pending(), 0);
        // Ma modification attend ; celle d'un autre passe avant, puis la mienne est
        // refusée : je vois celle de l'autre.
        w.set(1, 100, 1, BlockId(3));
        step(&mut b, &mut w);
        from_garant(&mut b, &mut w, set(5));
        from_garant(&mut b, &mut w, BuildMsg::Reject { pos, peer: 0 });
        assert_eq!(w.get(1, 100, 1), BlockId(5));
        // Refus reçu alors que j'ai déjà rechangé la case (pas encore envoyé) : ma
        // nouvelle modification reste, et si elle est refusée aussi, c'est le bloc qui
        // fait foi qui revient, pas celui de ma première tentative.
        w.set(1, 100, 1, BlockId(6));
        step(&mut b, &mut w);
        w.set(1, 100, 1, BlockId(7));
        from_garant(&mut b, &mut w, BuildMsg::Reject { pos, peer: 0 });
        assert_eq!(w.get(1, 100, 1), BlockId(7));
        step(&mut b, &mut w);
        from_garant(&mut b, &mut w, BuildMsg::Reject { pos, peer: 0 });
        assert_eq!(w.get(1, 100, 1), BlockId(5));
        // Refusée sans rien d'autre entre-temps : le bloc d'avant revient.
        w.set(1, 100, 1, BlockId(4));
        step(&mut b, &mut w);
        from_garant(&mut b, &mut w, BuildMsg::Reject { pos, peer: 0 });
        assert_eq!(w.get(1, 100, 1), BlockId(5));
        assert_ne!(before, BlockId(5));
    }

    fn rand_u32() -> u32 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(1, |d| d.subsec_nanos())
    }

    /// L'ordre du relais du monde fait foi : une modification plus ancienne que celle déjà
    /// appliquée à la même case (doublon, même modification arrivée par le relais de
    /// région et par le relais du monde) est sans effet, et ne confirme rien deux fois.
    /// Ce qu'un relais de région a dit est oublié quand on le quitte.
    #[test]
    fn an_older_or_repeated_edit_changes_nothing() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut b = BuildNet::new(tx, "Moi".into(), false);
        let mut w = World::new(0);
        w.ensure_chunk(minelib_world::ChunkPos::containing(1, 1));
        w.journal = Some(Vec::new());
        let pos = [1, 100, 1];
        let set = |block: u16, seq: u64| {
            game::encode(&BuildMsg::Set {
                pos,
                block,
                by: "Eve".into(),
                seq,
            })
        };
        b.receive(0, [0, 0], &set(1, 10), &mut w);
        b.receive(0, [0, 0], &set(2, 12), &mut w);
        // La plus ancienne arrive après, par un autre chemin ; puis un doublon.
        b.receive(0, [0, 0], &set(1, 10), &mut w);
        b.receive(0, [0, 0], &set(3, 11), &mut w);
        b.receive(0, [0, 0], &set(2, 12), &mut w);
        assert_eq!(w.get(1, 100, 1), BlockId(2));
        // Deux modifications à moi en attente sur cette case : la première confirmée,
        // reçue deux fois (relais du monde, puis relais de région), ne confirme pas la
        // seconde.
        w.set(1, 100, 1, BlockId(4));
        let _ = b.update(&mut w, Vec3::ZERO, (0.0, 0.0), 0.0, 0.0, 0.0, 0.05);
        w.set(1, 100, 1, BlockId(5));
        let _ = b.update(&mut w, Vec3::ZERO, (0.0, 0.0), 0.0, 0.0, 0.0, 0.05);
        b.receive(0, [0, 0], &set(4, 13), &mut w);
        b.receive(0, [0, 0], &set(4, 13), &mut w);
        assert_eq!(b.pending(), 1, "la seconde attend toujours sa réponse");
        b.receive(0, [0, 0], &set(5, 14), &mut w);
        assert_eq!(b.pending(), 0);
        assert_eq!(w.get(1, 100, 1), BlockId(5));
        // Un relais de région (tenu par un joueur) annonce un rang énorme : tant qu'on
        // passe par lui, c'est lui qu'on voit ; quitté, il ne laisse rien derrière lui.
        b.from_shard = Some(1);
        b.receive(0, [0, 0], &set(9, u64::MAX), &mut w);
        b.from_shard = None;
        assert_eq!(w.get(1, 100, 1), BlockId(9));
        b.receive(0, [0, 0], &set(6, 15), &mut w);
        assert_eq!(w.get(1, 100, 1), BlockId(9), "encore servi par ce relais");
        b.forget_shard(1);
        b.receive(0, [0, 0], &set(6, 15), &mut w);
        assert_eq!(w.get(1, 100, 1), BlockId(6), "le relais du monde fait foi");
        // Un relais de région remet deux blocs : le relais du monde confirme le premier
        // (au même rang), jamais le second (inventé par un hôte malveillant, montré à ses
        // seuls joueurs). Quitté, le second revient comme avant, pas le premier.
        let at = |p: [i32; 3], block: u16, seq: u64| {
            game::encode(&BuildMsg::Set {
                pos: p,
                block,
                by: "Eve".into(),
                seq,
            })
        };
        let (real, fake) = ([2, 100, 2], [3, 100, 3]);
        let fake_before = w.get(3, 100, 3);
        b.from_shard = Some(2);
        b.receive(0, [0, 0], &at(real, 7, 1 << 40), &mut w);
        b.receive(0, [0, 0], &at(fake, 8, (1 << 40) + 1), &mut w);
        b.from_shard = None;
        assert_eq!(
            (w.get(2, 100, 2), w.get(3, 100, 3)),
            (BlockId(7), BlockId(8))
        );
        b.forget_shard(2);
        b.receive(0, [0, 0], &at(real, 7, 1 << 40), &mut w);
        for _ in 0..80 {
            let _ = b.update(&mut w, Vec3::ZERO, (0.0, 0.0), 0.0, 0.0, 0.0, 0.05);
        }
        assert_eq!(w.get(2, 100, 2), BlockId(7), "confirmé : il reste");
        assert_eq!(w.get(3, 100, 3), fake_before, "jamais confirmé : défait");
    }

    /// (v288) La même modification remise par deux relais, au même rang (celui qu'on va
    /// quitter, et celui qui sert la zone à sa place) : quitter le premier ne la défait
    /// pas, le second en répond. Avant, le doublon était écarté sans rien retenir ; le
    /// premier relais quitté, elle passait en doute et était défaite 3 s plus tard chez ce
    /// joueur seulement, et le second relais, qui l'avait déjà remise, ne la redonnait
    /// jamais : un bloc en moins que chez les autres. Quitté à son tour sans confirmation
    /// du relais du monde, elle est défaite ; remise plus tard, elle s'applique de nouveau.
    #[test]
    fn a_block_also_given_by_the_next_relay_survives_leaving_the_first() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut b = BuildNet::new(tx, "Moi".into(), false);
        let mut w = World::new(0);
        w.ensure_chunk(minelib_world::ChunkPos::containing(2, 2));
        w.journal = Some(Vec::new());
        let pos = [2, 100, 2];
        let before = w.get(2, 100, 2);
        let set = game::encode(&BuildMsg::Set {
            pos,
            block: 7,
            by: "Eve".into(),
            seq: 1 << 40,
        });
        let wait = |b: &mut BuildNet, w: &mut World| {
            for _ in 0..80 {
                let _ = b.update(w, Vec3::ZERO, (0.0, 0.0), 0.0, 0.0, 0.0, 0.05);
            }
        };
        b.from_shard = Some(1);
        b.receive(0, [0, 0], &set, &mut w);
        b.from_shard = Some(2);
        b.receive(0, [0, 0], &set, &mut w);
        b.from_shard = None;
        assert_eq!(w.get(2, 100, 2), BlockId(7));
        b.forget_shard(1);
        wait(&mut b, &mut w);
        assert_eq!(w.get(2, 100, 2), BlockId(7), "le second relais en répond");
        // Le second quitté à son tour, jamais confirmée par le relais du monde : défaite.
        b.forget_shard(2);
        wait(&mut b, &mut w);
        assert_eq!(w.get(2, 100, 2), before, "plus personne n'en répond");
        // Remise plus tard au même rang (par le relais du monde) : elle revient.
        b.receive(0, [0, 0], &set, &mut w);
        assert_eq!(w.get(2, 100, 2), BlockId(7), "une modification défaite peut revenir");
    }

    #[test]
    fn flowing_fluids_are_told_apart_from_placed_blocks() {
        let water = minelib_world::registry::Fluid::Water;
        assert!(fluid_edit(BlockId::AIR, water.block(7)));
        assert!(fluid_edit(water.block(7), BlockId::AIR));
        assert!(
            !fluid_edit(BlockId::STONE, water.block(7)),
            "l'eau ne casse pas la pierre"
        );
        assert!(
            !fluid_edit(BlockId::AIR, BlockId::STONE),
            "un bloc posé n'est pas de l'eau"
        );
    }
}
