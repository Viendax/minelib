//! Messages échangés entre le jeu et le relais (sérialisés en postcard, chiffrés par
//! `transport`).

use serde::{Deserialize, Serialize};

/// Version du protocole : le relais refuse un jeu trop ancien.
/// (v222) 55 : la lampe torche en main part avec la position (poses, `BuildMsg::Player`).
/// (v224) 56 : les lumières de l'éditeur dans les mondes de construction en ligne
/// (`BuildMsg::LightPut`, `Light`, `LightDenied`).
/// (v227) 57 : la foule d'essai des OP en construction en ligne (`BuildMsg::Crowd`,
/// `CrowdHit`, `CrowdDenied`).
/// (v229) 58 : la météo imposée par un OP pour tout le serveur (`SERVER_WEATHER`,
/// `Response::SetWeather`, `ProvinceSettings::weather`) et la foudre de `/thor` et de la
/// Foudre de Zeus montrée aux joueurs proches (`BuildMsg::ThorAsk`, `Thor`, `ThorDenied`).
/// (v231) 59 : où sont ses amis (`Response::Friends::places`), demandes d'ami
/// (`Response::Friends::requests`, `Request::FriendDecline`, `Response::FriendAdded`),
/// invitations dans un monde de construction (`Request::Invite`, `Response::Invited`), statut
/// invisible (`Request::Presence`).
/// (v240) 60 : clés d'accès (`Request::PasskeyChallenge`, `PasskeyLogin`, `PasskeyAdd`,
/// `PasskeyList`, `PasskeyRemove`).
/// (v241) 61 : la Foudre de Zeus dans le dos de qui l'a sur lui sans la tenir (bit
/// `poses::Held::BACK` de l'objet tenu, avec la position ; un jeu d'avant rejetait tout le
/// paquet des positions qui le contient).
/// (v242) 62 : invitations gardées par le relais sans limite de temps, jusqu'à la réponse
/// (`Response::Friends::invites`), Accepter / Refuser / Ignorer (`Request::InviteReply`), et
/// le refus dit à qui avait invité (`Response::InviteDeclined`).
/// (v243) 63 : partie FPS avec des bots sur un serveur de construction, menée par un OP
/// (`SERVER_MATCH`, `Response::BuildMatch`, `Response::MatchPeer`) ; dans un monde de
/// construction, `ToHost`, `ToPeer` et `Broadcast` vont entre l'hôte de cette partie et ses
/// joueurs.
/// (v244) 64 : la Foudre de Zeus légendaire en ligne, tenue par le relais du monde
/// (`Request::Zeus`, `Response::Zeus`, `BuildMsg::ZeusAsk`, `Zeus`).
/// (v247) 65 : la Foudre de Zeus rendue au menu créatif (`ZEUS_RETURN`) ; accroupi plus vite (la posture ne traîne plus à la fin) et plus bas (la tête,
/// donc sa boîte des coups en partie FPS, reste dans la boîte de 1,5 bloc) : la simulation et
/// les coups doivent être les mêmes chez l'hôte et ses invités.
/// (v249) 66 : les tirs d'une partie FPS disent la direction de chaque balle
/// (`game::ToPeer::Shot::dirs`) : chacun voit les impacts des autres sur les blocs.
/// (v256) 67 : en partie FPS, les balles touchent les sept pavés du modèle du joueur, tournés
/// avec lui (plus une boîte droite) : l'hôte et le vérificateur doivent compter les mêmes
/// touches.
/// (v260) 68 : la copie de la partie FPS confiée au garant porte le regard de la dernière
/// entrée de chaque joueur (`Backup::looks`) : ses boîtes des coups, tournées avec lui, sont
/// les mêmes chez l'hôte et chez le vérificateur ; accroupi, le dos du modèle se plie à la
/// taille (boîtes des coups comprises).
/// (v264) 69 : la tête du personnage rapetisse (4 colonnes du skin du corps) et son corps
/// grandit d'autant : ses boîtes des coups en partie FPS, les pavés du modèle, doivent être
/// les mêmes chez l'hôte, ses invités et le vérificateur.
/// (v270) 70 : voie rapide des parties FPS (`crate::fast`) : passeurs (`PasseurOffer`,
/// `PasseurTask`), routes des joueurs (`FastWant`, `FastGuest`, `FastHost`), entrées
/// envoyées aussi par la voie rapide (`game::ToHost::InputsFast`) et sondes
/// (`game::ToPeer::FastPing`).
/// (v274) 71 : voie rapide : l'hôte signale un passeur muet ou lent (`FastBad`), les états
/// complets partent aussi par les passeurs.
/// (v283) 72 : les tapis ont une hitbox (un seizième de bloc) et s'empilent (nouveaux blocs, en
/// fin de registre), on s'enfonce d'une couche dans la neige fine : la simulation des joueurs
/// d'une partie FPS (et les blocs d'un serveur) doivent être les mêmes chez tous.
/// (v285) 73 : positions rapprochées (`Request::NearbyClose`).
/// (v290) 74 : le personnage a des genoux : accroupi, ses jambes ne se tassent plus, elles se
/// plient (cuisse en avant, bas de jambe en arrière) ; ses boîtes des coups en partie FPS, les
/// pavés du modèle (onze : chaque jambe et chaque bras en deux), doivent être les mêmes chez
/// l'hôte, ses invités et le vérificateur.
/// (v301) 75 : tunnels iroh par les passeurs (`Request::TunnelWant`, `Response::TunnelWay`).
/// (v304-v312, toujours 75 : pas encore publié) chaînes de deux passeurs (`PasseurLink`),
/// journal réseau (`NetLog`, `NetLogWanted`), signalement d'une carte (`MapReport`), mondes
/// endormis (`ServerEntry` : record, dernière activité, gardien) et `DeleteWorld` ; (v312)
/// L'Évidé partagé en construction en ligne (`BuildMsg::Evide`, `EvideBite`, `EvideEat`,
/// surveillés par le relais : `evide::Watch`).
/// (v315, toujours 75) `LeaderboardFrom` : la suite du classement.
pub const PROTOCOL: u32 = 75;

/// Région souveraine : l'hôte principal envoie ses lots au relais du monde, et l'autre
/// hôte sa confirmation, au plus une fois par ce temps (ms) chacun, regroupés.
pub const ARCHIVE_EVERY_MS: u64 = 250;

/// Version du jeu annoncée dans le bonjour (`client` : « minelib 0.1.0 v95 ») ; 0 si
/// absente.
pub fn client_version(client: &str) -> u32 {
    client
        .split_whitespace()
        .rev()
        .find_map(|w| w.strip_prefix('v')?.parse().ok())
        .unwrap_or(0)
}

/// (v315) Lignes du classement par réponse (une ligne fait moins de 100 octets, une trame
/// 65 535 au plus).
pub const RANK_PAGE: usize = 200;

/// Morceau d'une carte transférée (octets).
pub const MAP_PART: usize = 48 * 1024;
/// Taille maximale d'une carte partagée.
pub const MAP_MAX: u64 = 64 * 1024 * 1024;
/// Joueurs d'une partie FPS classée (5 contre 5).
pub const FPS_PLAYERS: u16 = 10;
/// Joueurs au plus d'une carte FPS (`/fps set maxplayer`, toujours pair : 10 contre 10).
pub const FPS_MAX_PLAYERS: u16 = 20;

/// Pseudo : 3 à 16 caractères, lettres sans accent, chiffres et `_`. Unique sans tenir
/// compte des majuscules.
/// Actions des OP d'un serveur de construction (`Request::ServerAdmin`).
pub const SERVER_BAN: u8 = 0;
pub const SERVER_UNBAN: u8 = 1;
pub const SERVER_OP: u8 = 2;
pub const SERVER_DEOP: u8 = 3;
pub const SERVER_LIST: u8 = 4;
/// Tuer un joueur du serveur (OP).
pub const SERVER_KILL: u8 = 5;
/// Liste blanche (OP) : activer, désactiver, ajouter, retirer, montrer.
pub const SERVER_WL_ON: u8 = 6;
pub const SERVER_WL_OFF: u8 = 7;
pub const SERVER_WL_ADD: u8 = 8;
pub const SERVER_WL_REMOVE: u8 = 9;
pub const SERVER_WL_LIST: u8 = 10;
/// Changer de mode de jeu (OP) : `reason` = le mode, `pseudo` = qui (vide : soi-même).
pub const SERVER_GAMEMODE: u8 = 11;
/// Mode de jeu par défaut des joueurs à leur arrivée (OP) : `reason` = le mode (vide :
/// aucun).
pub const SERVER_DEFAULT_GAMEMODE: u8 = 12;
/// À l'arrivée sur un serveur de construction : le relais renvoie (`SetGamemode`) le mode
/// par défaut du serveur si l'on n'est pas OP. Réponse sans texte.
pub const SERVER_GAMEMODE_QUERY: u8 = 13;
/// Renommer le serveur (OP) : `reason` = le nouveau nom.
pub const SERVER_RENAME: u8 = 14;
/// Heure du serveur (OP) : `reason` = l'heure (0..24) ; l'arrêter (`reason` 1) ou la
/// relancer (0).
pub const SERVER_TIMESET: u8 = 15;
pub const SERVER_TIMEFIX: u8 = 16;
/// (v223) Téléporter (OP) : `pseudo` = qui (vide : soi-même), `reason` = « x y z », ou
/// « @pseudo » pour aller là où est ce joueur. Le relais renvoie `BuildSpawn` à celui qui
/// part.
pub const SERVER_TELEPORT: u8 = 17;
/// (v223) Point d'apparition du serveur (OP) : `reason` = « x y z lacet tangage » (là où
/// est l'OP). Un joueur qui arrive pour la première fois y apparaît (`BuildSpawn`).
pub const SERVER_SETSPAWN: u8 = 18;
/// (v229) Météo du serveur (OP), pour tous ses joueurs : `reason` = « clear », « rain » ou
/// « storm » (ce temps, pour une durée ordinaire que tire le relais ; sans fin si la météo
/// est figée), « fix <temps> » (figer : ce temps-là, celui que voit l'OP, ne change plus),
/// « auto » (la météo change de nouveau toute seule). Le relais le dit à tous
/// (`Response::SetWeather`), et à ceux qui arrivent. `reason` vide (tout joueur du serveur) :
/// rien ne change, le relais redit la météo du serveur à celui qui la demande.
pub const SERVER_WEATHER: u8 = 19;
/// (v243) Partie FPS sur le serveur de construction : `reason` = « open » (un OP l'ouvre et
/// la mène : son jeu en est l'hôte, avec ses bots), « close » (son hôte ou un OP la ferme),
/// « join » / « leave » (un joueur en mode FPS y entre, en sort), « query » (est-elle
/// ouverte ?). Le relais dit à tous les joueurs du serveur qu'elle s'ouvre ou se ferme
/// (`Response::BuildMatch`, aussi à ceux qui arrivent), et à son hôte qui y entre ou en sort
/// (`Response::MatchPeer`). Les messages de la partie (`ToHost`, `ToPeer`, `Broadcast`) ne vont
/// qu'entre son hôte et ses joueurs.
pub const SERVER_MATCH: u8 = 20;

/// (v229) Temps qu'il fait, dans `Response::SetWeather` : beau temps, pluie, orage ; ou
/// aucun temps imposé (celui des horloges du monde, que chaque jeu calcule).
pub const WEATHER_CLEAR: u8 = 0;
pub const WEATHER_RAIN: u8 = 1;
pub const WEATHER_STORM: u8 = 2;
pub const WEATHER_NATURAL: u8 = 255;

/// (v229) Durée ordinaire (s, de… à…) d'un temps imposé : celles de Minecraft (une période
/// de beau temps, une averse, un orage). `None` : pas un temps.
pub fn weather_span(kind: u8) -> Option<(f32, f32)> {
    match kind {
        WEATHER_CLEAR => Some((600.0, 9000.0)),
        WEATHER_RAIN => Some((600.0, 1200.0)),
        WEATHER_STORM => Some((180.0, 780.0)),
        _ => None,
    }
}

/// Triches trouvées par le rejeu : sauts parfaits en chaîne (script de bunny hop), appuis
/// plus rapides qu'une main (macro).
pub const CHEAT_BHOP_SCRIPT: u8 = 1;
pub const CHEAT_MACRO: u8 = 2;
// (Elle renvoie aussi `BuildSpawn` : la position qu'on avait sur ce serveur.)

pub fn valid_pseudo(p: &str) -> bool {
    (3..=16).contains(&p.chars().count())
        && p.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// (v271) Caractères qui se ressemblent dans un pseudo (« je n'arrive pas à ajouter mon pote
/// 1side » : son pseudo est « lside », un L minuscule que la police du jeu dessinait comme
/// un 1) : 1, l et i ; 0 et o ; 5 et s ; 2 et z ; 8 et b. En minuscules : les pseudos se
/// comparent sans la casse.
const LOOKALIKE: [&str; 5] = ["1li", "0o", "5s", "2z", "8b"];

/// (v271) Variantes d'un pseudo par ces ressemblances (en minuscules, lui compris en
/// premier), `max` au plus.
pub fn lookalikes(pseudo: &str, max: usize) -> Vec<String> {
    let mut out = vec![String::new()];
    for c in pseudo.to_ascii_lowercase().chars() {
        let class = LOOKALIKE.iter().find(|k| k.contains(c));
        let options: Vec<char> = match class {
            // Le caractère tapé d'abord : la première variante est le pseudo lui-même.
            Some(k) => std::iter::once(c)
                .chain(k.chars().filter(|&o| o != c))
                .collect(),
            None => vec![c],
        };
        let mut next = Vec::with_capacity(out.len() * options.len());
        'all: for v in &out {
            for &o in &options {
                if next.len() >= max.max(1) {
                    break 'all;
                }
                let mut s = v.clone();
                s.push(o);
                next.push(s);
            }
        }
        out = next;
    }
    out
}

/// (v271) Ce que le relais répond pour un pseudo inconnu, avec celui qui lui ressemble s'il
/// y en a un (voir `suggested_player`) : la lettre qui change est nommée, la police pouvant
/// montrer pareil un l et un 1.
pub fn unknown_player(who: &str, alike: Option<&str>) -> String {
    let Some(a) = alike else {
        return format!("Joueur « {who} » inconnu.");
    };
    let name = |c: char| {
        if c.is_ascii_digit() {
            format!("le chiffre {c}")
        } else if c.is_ascii_uppercase() {
            format!("un {c} majuscule")
        } else {
            format!("un {} minuscule", c.to_ascii_uppercase())
        }
    };
    let hint = who
        .chars()
        .zip(a.chars())
        .find(|(w, s)| !w.eq_ignore_ascii_case(s))
        .map(|(w, s)| format!(" ({}, pas {})", name(s), name(w)))
        .unwrap_or_default();
    format!("Joueur « {who} » inconnu. Voulais-tu dire « {a} »{hint} ?")
}

/// (v271) Pseudo proposé par un message de `unknown_player` (bouton « Ajouter » du panneau
/// des amis).
pub fn suggested_player(msg: &str) -> Option<&str> {
    let rest = msg.split_once("Voulais-tu dire « ")?.1;
    let p = rest.split_once(" »")?.0;
    valid_pseudo(p).then_some(p)
}

/// Mot de passe : au moins 8 caractères, au plus 128.
pub fn valid_password(p: &str) -> bool {
    (8..=128).contains(&p.chars().count())
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum Request {
    Hello {
        protocol: u32,
        client: String,
    },
    /// Entre relais : ce pseudo a-t-il un compte ici (et avec quelle clé) ? Les pseudos
    /// sont uniques sur tous les relais qui se connaissent (ceux du DHT).
    Whois {
        pseudo: String,
    },
    /// Entre relais : les parties ouvertes ici (0 = FPS en attente, 1 = construction).
    /// Chaque relais montre à ses joueurs les parties des relais qu'il connaît.
    PeerServers {
        kind: u8,
    },
    /// Création d'un compte : clé publique, preuve de mot de passe, coffre chiffré et
    /// preuve de travail (`crypto::pow_ok`).
    Register {
        pseudo: String,
        pubkey: [u8; 32],
        auth: [u8; 32],
        vault: Vec<u8>,
        pow: u64,
    },
    /// Première étape de connexion : preuve de mot de passe (et code 2FA si activé).
    Login {
        pseudo: String,
        auth: [u8; 32],
        totp: Option<u32>,
        /// Empreintes des composants de la machine, salées par la clé du relais (`hw`).
        hw: Vec<[u8; 32]>,
        /// Jeton d'appareil de confiance (donné à une connexion avec 2FA) : la 2FA n'est
        /// pas redemandée sur cet appareil.
        device: Option<[u8; 32]>,
    },
    /// Administrateur du relais : bannit un compte (à vie) et ses machines (3 mois).
    Ban {
        pseudo: String,
        reason: String,
    },
    /// OP du serveur de construction où l'on est : `action` = `SERVER_BAN`, `…_UNBAN`,
    /// `…_OP`, `…_DEOP` ou `…_LIST`.
    ServerAdmin {
        action: u8,
        pseudo: String,
        reason: String,
    },
    /// Deuxième étape : signature du défi reçu avec le coffre.
    Prove {
        signature: Vec<u8>,
    },
    /// Double authentification (session ouverte) : nouveau secret, confirmation, retrait.
    TotpBegin,
    TotpConfirm {
        code: u32,
    },
    TotpDisable {
        code: u32,
    },
    /// Liste des parties d'un mode (0 = FPS en attente, 1 = construction).
    Servers {
        kind: u8,
    },
    Ping,
    /// Ouvre une partie dont ce jeu est l'hôte (serveur qui fait foi).
    Host {
        kind: u8,
        name: String,
        map: String,
        map_sig: [u8; 32],
        /// 0 : sans limite (construction).
        max_players: u16,
    },
    /// Mode FPS : cherche une partie en attente sur cette carte (compatible : personne
    /// qu'on a signalé, réputation proche) ; sans résultat, en ouvre une dont on est l'hôte.
    Search {
        map_sig: [u8; 32],
        map: String,
        /// Joueurs d'une partie sur cette carte (`FpsMap::players`).
        players: u8,
    },
    /// Signale un joueur de la partie en cours.
    Report {
        pseudo: String,
    },
    /// Hôte FPS : joueur garant de secours (il reçoit l'état complet de la partie et la
    /// reprend si l'hôte part).
    SetBackup {
        peer: u64,
    },
    /// Ce jeu peut servir de garant indépendant (au menu, sans partie).
    GarantAvailable {
        on: bool,
    },
    /// Garant en attente : copie reçue, prêt à reprendre la partie.
    GarantReady,
    /// Garant vérificateur : désaccord avec l'hôte.
    Conflict {
        player: u64,
        seq: u32,
        host_pos: [f32; 3],
        audit_pos: [f32; 3],
        proof: bool,
    },
    /// Jugement d'un joueur sur un désaccord le concernant.
    Vote {
        conflict: u64,
        auditor_right: bool,
    },
    /// Garant arbitre : après avoir rejoué les pièces de l'affaire `case`, l'hôte
    /// a-t-il triché ?
    Verdict {
        case: u64,
        guilty: bool,
    },
    /// Chat vocal (`voice::VoiceMsg` chiffré) à ces joueurs de la partie (0 = l'hôte en
    /// FPS). Le relais ne peut pas le lire.
    Voice {
        to: Vec<u64>,
        data: Vec<u8>,
    },
    /// Annule son signalement d'un joueur.
    Unreport {
        pseudo: String,
    },
    /// Résultat d'une partie (`game::MatchSummary` sérialisé), envoyé par chaque joueur.
    MatchResult {
        data: Vec<u8>,
    },
    /// Garant vérificateur : en rejouant la partie, triche prouvée sur les entrées d'un
    /// joueur (`kind` : `CHEAT_BHOP_SCRIPT`, `CHEAT_MACRO`).
    CheatFound {
        pseudo: String,
        kind: u8,
        detail: String,
    },
    /// Classement : les `RANK_PAGE` meilleurs (la suite : `LeaderboardFrom`), ou ses amis.
    Leaderboard {
        friends: bool,
    },
    /// Ajoute ou retire un ami (pseudo vide : seulement la liste).
    Friend {
        pseudo: String,
        add: bool,
    },
    /// Lobby (jusqu'à 5 joueurs qui lancent ensemble, dans la même équipe) : voir
    /// `PARTY_*` ; `pseudo` : l'invité, celui qui invite, ou le membre visé.
    Party {
        action: u8,
        pseudo: String,
    },
    /// Son skin (PNG 64 × 64 ou 64 × 32, 24 Ko au plus ; vide : retirer).
    SkinPut {
        png: Vec<u8>,
    },
    /// Skin d'un joueur.
    SkinGet {
        pseudo: String,
    },
    /// Cartes FPS connues du relais.
    Maps,
    /// Envoi d'une carte au relais, morceau par morceau.
    MapPut {
        sig: [u8; 32],
        name: String,
        offset: u64,
        total: u64,
        data: Vec<u8>,
        /// Carte de partie FPS en ligne (proposée à tous) ; sinon un monde de construction.
        fps: bool,
    },
    MapGet {
        sig: [u8; 32],
        offset: u64,
    },
    /// Construction : capacités de ce jeu pour être garant de zones.
    Caps {
        caps: GarantCaps,
    },
    /// Construction : position (le relais n'envoie que ce qui se passe autour).
    /// Relais de région : entrée avec le billet donné par le relais du monde, et le
    /// certificat de la clé du compte (les modifications signées dans une région
    /// souveraine ; aucun pour un autre relais de la région).
    ShardLogin {
        room: u64,
        pseudo: String,
        expires: u64,
        /// (v155) Signé par le relais du monde (`ticket_msg`) : avant, un code fait avec le
        /// secret du monde, que tout hôte de région connaissait.
        sig: Vec<u8>,
        cert: Option<crate::sovereign::Cert>,
    },
    /// Hôte d'une région (connexion de garant) : son relais de région est prêt.
    ShardReady {
        region: [i32; 2],
        /// Nœud iroh (joignable de partout) et adresse directe (réseau local, essais).
        node: [u8; 32],
        addr: String,
        key: [u8; 32],
    },
    /// Hôte principal d'une région souveraine, au relais du monde : les lots qu'il vient
    /// de publier, qui se suivent (archivés quand un autre hôte de la région les a
    /// confirmés, `Ack`). Regroupés : un message toutes les `ARCHIVE_EVERY_MS` au plus.
    Archive {
        region: [i32; 2],
        batches: Vec<crate::sovereign::Batch>,
    },
    /// Autre hôte d'une région souveraine, au relais du monde : il a vérifié tout ce qu'a
    /// publié l'hôte principal jusqu'au rang `seq` exclu, d'empreinte `chain`.
    Ack {
        region: [i32; 2],
        epoch: u64,
        seq: u64,
        chain: [u8; 32],
    },
    /// Autre hôte d'une région souveraine : faute de l'hôte principal, prouvée par ses
    /// propres lots signés (le relais du monde refait la vérification).
    Accuse {
        region: [i32; 2],
        epoch: u64,
        why: String,
        evidence: Vec<crate::sovereign::Batch>,
    },
    /// Hôte d'une région souveraine dont l'époque vient de finir (hôte principal parti ou
    /// remplacé) : les lots qu'il avait vérifiés, pour que rien ne se perde.
    Handover {
        region: [i32; 2],
        epoch: u64,
        batches: Vec<crate::sovereign::Batch>,
    },
    Position {
        chunk: [i32; 2],
    },
    /// Entre relais d'une même région : position d'un joueur entré par le relais qui
    /// envoie (chaque relais la transmet à ses propres joueurs proches).
    PeerNearby {
        peer: u64,
        chunk: [i32; 2],
        data: Vec<u8>,
    },
    /// Région souveraine, d'un autre relais de la région à l'hôte principal : la
    /// modification signée d'un de ses joueurs, avec son certificat.
    PeerAsk {
        edit: crate::sovereign::Edit,
        cert: Option<crate::sovereign::Cert>,
    },
    /// Région souveraine, de l'hôte principal aux autres relais de la région : un lot
    /// qu'il publie (ils le vérifient avant de le transmettre à leurs joueurs).
    PeerBatch {
        batch: crate::sovereign::Batch,
    },
    /// Région souveraine, de l'hôte principal : modification refusée, à dire à son auteur
    /// (joueur de l'autre relais).
    PeerReject {
        pseudo: String,
        pos: [i32; 3],
    },
    /// Hôte du relais de secours d'un monde, au relais du monde : son relais est prêt
    /// (nœud iroh, adresse directe, clé du canal chiffré).
    StandbyReady {
        node: [u8; 32],
        addr: String,
        key: [u8; 32],
    },
    /// Gardien d'un monde (relais de secours ou témoin), au relais du monde : son bail
    /// (« je t'entends »), qui rappelle l'heure `t` du battement entendu et la version de
    /// la liste des gardiens qu'il connaît (voir `crate::guard`).
    Lease {
        room: u64,
        version: u64,
        t: u64,
    },
    /// Gardien d'un monde qui s'en va proprement : il promet de ne jamais voter (il est
    /// remplacé sans attendre).
    Resign {
        room: u64,
    },
    /// Connexion de garant d'un joueur, au relais qui a repris son monde (il n'y a pas de
    /// compte) : entrée avec le certificat de la clé de son compte. Réponse :
    /// `TakeoverChallenge`, puis `GarantProve` → `Welcome`.
    GarantLogin {
        cert: crate::sovereign::Cert,
    },
    GarantProve {
        signature: Vec<u8>,
    },
    /// Témoin d'un monde, au relais de secours : entrée avec son certificat (signé par le
    /// relais du monde). Réponse : `TakeoverChallenge`, puis `WitnessProve` → `Done`.
    WitnessLogin {
        cert: crate::sovereign::Cert,
    },
    WitnessProve {
        signature: Vec<u8>,
    },
    /// Témoin, au relais de secours, quatre fois par seconde : la dernière liste des
    /// gardiens qu'il connaît, et son vote pour la reprise s'il n'entend plus le relais du
    /// monde (la liste qu'il avait en votant, et son vote signé pour elle).
    WitnessBeat {
        committee: crate::guard::Committee,
        vote: Option<(crate::guard::Committee, crate::guard::Vote)>,
    },
    /// Au relais de secours, après la chute du relais du monde : « j'étais dans ce
    /// monde » (certificat de la clé de mon compte, signé par l'ancien relais du monde).
    /// Réponse : `TakeoverChallenge`.
    Takeover {
        cert: crate::sovereign::Cert,
    },
    /// Signature du défi avec la clé du compte. Réponse : `Joined` (le monde repris).
    TakeoverProve {
        signature: Vec<u8>,
    },
    /// Région souveraine, d'un autre relais à l'hôte principal : des lots lui manquent,
    /// à partir du rang `from` (qu'il les renvoie).
    PeerResend {
        from: u64,
    },
    /// Construction, au relais du monde : demande sur ce chunk (changer un bloc, lâcher ou
    /// ramasser un objet, ouvrir un coffre). Lui seul décide et publie le résultat.
    ToZone {
        chunk: [i32; 2],
        data: Vec<u8>,
    },
    /// Construction : diffusion à tous les joueurs proches du chunk.
    Nearby {
        chunk: [i32; 2],
        data: Vec<u8>,
    },
    /// La partie a commencé (une partie FPS en cours n'est plus listée) ou non.
    RoomStarted {
        started: bool,
    },
    /// Partie FPS en pause (true) ou reprise : le temps de pause ne compte pas dans les
    /// absences.
    RoomPaused {
        paused: bool,
    },
    Join {
        room: u64,
    },
    Leave,
    /// Données de jeu d'un joueur vers l'hôte (sans réponse).
    ToHost {
        data: Vec<u8>,
    },
    /// Données de l'hôte vers un joueur, ou vers tous (sans réponse).
    ToPeer {
        peer: u64,
        data: Vec<u8>,
    },
    Broadcast {
        data: Vec<u8>,
    },
    /// Aller-retour mesuré par le jeu jusqu'au relais (ms). Le relais mesure aussi
    /// lui-même (`Response::Probe`) et retient le pire des deux : un jeu modifié ne peut
    /// pas s'annoncer mieux connecté qu'il ne l'est.
    Rtt {
        ms: u16,
    },
    /// Réponse immédiate à `Response::Probe` (même numéro).
    Echo {
        nonce: u32,
    },
    /// Modifications en masse d'un OP (WorldEdit) dans un monde de construction : le
    /// relais vérifie qu'il est OP, les publie comme des blocs validés et répond par
    /// `BulkDone` (même `id`).
    BulkEdit {
        id: u32,
        cells: Vec<([i32; 3], u16)>,
    },
    /// Construction : régions dont un relais de région sert en ce moment les blocs à ce
    /// joueur (liste complète à chaque changement). Le relais du monde ne lui envoie pas
    /// ces blocs une deuxième fois ; une région retirée de la liste : il lui remet aussitôt
    /// ce qui lui manque.
    Served {
        regions: Vec<[i32; 2]>,
    },
    /// Hôte d'un relais de région, au relais du monde : vérification d'un lot d'une autre
    /// région qu'il lui a confiée (`Response::Audit`), et la faute trouvée s'il y en a une.
    Audited {
        id: u64,
        fault: Option<String>,
    },
    /// Construction, au relais du monde, toutes les 10 s quand ses positions partent à un
    /// relais de région : où est le joueur (gardé pour son retour sur ce serveur).
    Spot {
        pos: [f32; 3],
        yaw: f32,
        pitch: f32,
    },
    /// Construction (v137) : voix adressée par pseudo, au relais de région de là où l'on
    /// est (qui la remet à ses joueurs et aux autres relais de la région), ou au relais du
    /// monde. Réponse aux destinataires : `Response::VoiceFrom`.
    VoiceTo {
        to: Vec<String>,
        data: Vec<u8>,
    },
    /// Entre relais d'une même région : la voix d'un de ses joueurs, pour les siens.
    PeerVoice {
        from: String,
        to: Vec<String>,
        data: Vec<u8>,
    },
    /// Provinces (v138), de l'hôte d'une province (connexion de garant) au relais du
    /// monde : son relais de province est prêt (voir `ProvinceInfo`).
    ProvinceReady {
        room: u64,
        province: [i32; 2],
        epoch: u64,
        node: [u8; 32],
        addr: String,
        key: [u8; 32],
        /// (v141) Rang jusqu'où il a l'historique de la province (relais de secours qui
        /// prend la place du relais tombé : ce qui manque lui part ; 0 sinon).
        have: u64,
    },
    /// (v141) Relais de secours d'une province → son relais : il en tient la copie (jeton du
    /// relais du monde, pseudo et clé du compte de son hôte).
    ProvinceFollow {
        room: u64,
        province: [i32; 2],
        epoch: u64,
        pseudo: String,
        signer: [u8; 32],
        token: [u8; 32],
        /// (v144) Où joindre son propre relais (les provinces d'un pays s'y rattacheront s'il
        /// prend la place du pays).
        node: [u8; 32],
        addr: String,
        key: [u8; 32],
    },
    /// (v146) Au fond d'un pays (tout ce qu'on a en vue est servi par ses provinces) : le
    /// joueur quitte la partie au relais du monde sans quitter le monde, puis ferme sa
    /// connexion. Réponse : `Dozed`.
    Doze {
        room: u64,
    },
    /// (v146) Retour au relais du monde d'un joueur qui l'avait quitté ainsi : certificat
    /// de la clé de son compte, signé par ce relais pour ce monde. Réponse :
    /// `TakeoverChallenge`, puis (`TakeoverProve`) `Joined`.
    Wake {
        room: u64,
        cert: crate::sovereign::Cert,
    },
    /// (v143) Hôte d'une province d'un pays (ou de son relais de secours) → relais du pays :
    /// il entre, avec le jeton que le relais du monde lui a donné avec sa tâche (voir
    /// `child_token`) ; tout ce qui concerne la province passe ensuite par cette connexion.
    ProvinceChild {
        room: u64,
        province: [i32; 2],
        size: i32,
        epoch: u64,
        pseudo: String,
        signer: [u8; 32],
        token: [u8; 32],
        standby: bool,
    },
    /// (v141) Hôte d'un relais de secours de province → relais du monde : la copie est à
    /// jour jusqu'au rang `have` ; il peut prendre la place du relais de la province.
    ProvinceStandbyReady {
        room: u64,
        province: [i32; 2],
        epoch: u64,
        have: u64,
    },
    /// Renouvellement du bail (toutes les `PROVINCE_RENEW_MS`) ; `t` : son heure à lui
    /// (ms), rendue dans la réponse (le bail court à partir de l'envoi).
    ProvinceLease {
        room: u64,
        province: [i32; 2],
        epoch: u64,
        t: u64,
        /// (v140) Joueurs de chaque région peuplée de la province (le relais du monde ne
        /// reçoit plus leurs positions), et relais de région dont l'hôte joue dans la
        /// région qu'il sert (région, rang : il doit être relevé).
        counts: Vec<([i32; 2], u32)>,
        inside: Vec<([i32; 2], u16)>,
        /// (v142) Son relais de secours la suit (connecté).
        followed: bool,
        /// (v143) Relais de pays : hôtes qu'il lui faut pour ses provinces (le relais du
        /// monde les choisit parmi les connexions de garant et leur donne la tâche), et ses
        /// provinces servies quand elles ont changé (le relais du monde y rattache les relais
        /// de leurs régions).
        wants: Vec<Want>,
        children: Option<Vec<ProvinceInfo>>,
        /// (v145) Province d'un pays : joueurs arrivés chez elle et partis depuis le dernier
        /// bail (pseudos) ; le pays sait ainsi où sont ses joueurs sans recevoir leurs
        /// positions.
        arrived: Vec<String>,
        left: Vec<String>,
        /// (v147) Dernier rang qu'elle a donné : un pays dont la plage s'épuise (il en donne
        /// une à chaque époque de ses provinces) en reçoit une nouvelle (`more`).
        seq: u64,
        /// (v150) Joueurs de toute la province (avec ceux de ses provinces) : `counts` ne dit
        /// plus que les régions assez peuplées pour avoir leurs propres relais (`dense`).
        total: u32,
    },
    /// Ce que le relais de province vient de publier (zone, rang, message) : le relais du
    /// monde en garde une copie, pour reprendre la province aussitôt si son hôte tombe.
    /// Numérotés (`n`) : il confirme chacun (`Response::ProvinceArchived`).
    ProvinceArchive {
        room: u64,
        province: [i32; 2],
        epoch: u64,
        n: u64,
        entries: Vec<([i32; 2], u64, Vec<u8>)>,
        /// (v139) Sacs de survie prêtés qui ont changé : (pseudo, objets, crédits de
        /// minage) ; positions de retour de ses joueurs (pseudo, x, y, z, lacet, tangage).
        bags: Vec<BagCopy>,
        spots: Vec<(String, [f32; 5])>,
        /// (v139) Blocs changés dans la province par des joueurs en survie dont elle n'a
        /// pas le sac (pseudo, bloc : 0 cassé, sinon posé) : comptés par le relais du monde.
        loose: Vec<(String, u16)>,
        /// (v142) `entries` est tout ce qu'elle a, en réponse à la demande `dump` du relais du
        /// monde (0 : un envoi ordinaire).
        dump: u64,
    },
    /// (v142) Relais de secours d'une province → son relais : il a copié l'envoi `n` (et
    /// ceux d'avant).
    ProvinceCopied {
        room: u64,
        province: [i32; 2],
        epoch: u64,
        n: u64,
    },
    /// (v139) Un joueur en survie est dans la province : son sac, s'il vous plaît (le
    /// relais du monde le prête à une seule province à la fois).
    ProvinceBag {
        room: u64,
        province: [i32; 2],
        epoch: u64,
        pseudo: String,
    },
    /// (v139) Sac rendu (le joueur est parti, ou le relais du monde l'a redemandé) : son
    /// état final.
    ProvinceBagBack {
        room: u64,
        province: [i32; 2],
        epoch: u64,
        pseudo: String,
        items: Vec<(u32, u32)>,
        mined: u32,
    },
    /// Relais de région prêts dans la province (région, rang), à chaque changement : le
    /// relais du monde en choisit toujours les hôtes et les remplace.
    ProvinceShards {
        room: u64,
        province: [i32; 2],
        epoch: u64,
        ready: Vec<([i32; 2], u16)>,
    },
    /// Départ propre : tout a été archivé (dernier `ProvinceArchive` juste avant), le
    /// relais du monde reprend la province tout de suite.
    ProvinceRelease {
        room: u64,
        province: [i32; 2],
        epoch: u64,
    },
    /// Hôte d'un relais de région, au relais de sa province : entrée avec le jeton donné
    /// par le relais du monde (voir `province_token`).
    RegionHost {
        room: u64,
        region: [i32; 2],
        slot: u16,
        pseudo: String,
        /// Clé de son compte (il signe ses lots d'hôte principal), dans le jeton.
        signer: [u8; 32],
        token: [u8; 32],
    },
    /// (v231) Invite un ami (dans les deux sens) dans le monde de construction où l'on est : il
    /// en est prévenu (`Response::Invited`) et peut le rejoindre. (v232) Hors ligne ou
    /// invisible : prévenu quand il demande sa liste d'amis (à sa connexion). (v242) Le relais
    /// la garde sans limite de temps (une par joueur qui invite, la plus récente), jusqu'à ce
    /// qu'il y réponde (`InviteReply`) : elle est dans sa liste d'amis
    /// (`Response::Friends::invites`).
    Invite {
        pseudo: String,
    },
    /// (v242) Réponse à l'invitation de `from` (`INVITE_ACCEPT`, `INVITE_DECLINE`,
    /// `INVITE_IGNORE`) : elle est retirée ; refusée, `from` en est prévenu
    /// (`Response::InviteDeclined`, gardé pour lui s'il n'est pas en ligne) ; ignorée, il n'en
    /// sait rien. Accepter ne fait pas entrer dans le monde (`Join` ensuite). Réponse :
    /// `Response::Friends`.
    InviteReply {
        from: String,
        reply: u8,
    },
    /// (v231) Statut vu par ses amis : `hidden` (invisible) : hors ligne pour eux, et pas
    /// d'invitations.
    Presence {
        hidden: bool,
    },
    /// (v231) Refuse la demande d'ami de `pseudo` (on est retiré de sa liste). Réponse :
    /// `Response::Friends`.
    FriendDecline {
        pseudo: String,
    },
    /// (v240) Défi pour se connecter par une clé d'accès (`Response::PasskeyChallenge`).
    PasskeyChallenge,
    /// (v240) Connexion par une clé d'accès (`crate::passkey`) : la clé `id` a signé les
    /// données de l'authentificateur et celles du client pour le dernier défi donné. Réponse :
    /// `Response::PasskeyOk` : le coffre de cette clé (les clés du compte, comme si le mot de
    /// passe avait été tapé) et un jeton d'appareil de confiance (pas de 2FA : la clé vérifie
    /// déjà l'utilisateur, visage, empreinte ou code) ; le jeu se connecte ensuite avec eux
    /// (`Login`).
    PasskeyLogin {
        pseudo: String,
        id: Vec<u8>,
        authenticator_data: Vec<u8>,
        signature: Vec<u8>,
        hw: Vec<[u8; 32]>,
    },
    /// (v240) Compte connecté : nouvelle clé d'accès (données de l'authentificateur de sa
    /// création, coffre chiffré par la clé gardée dans son identifiant d'utilisateur, nom).
    /// Réponse : `Response::Passkeys`.
    PasskeyAdd {
        id: Vec<u8>,
        authenticator_data: Vec<u8>,
        vault: Vec<u8>,
        name: String,
    },
    /// (v240) Compte connecté : ses clés d'accès (`Response::Passkeys`).
    PasskeyList,
    /// (v240) Compte connecté : retire cette clé d'accès (`Response::Passkeys`).
    PasskeyRemove {
        id: Vec<u8>,
    },
    /// (v244) Foudre de Zeus légendaire d'un serveur de construction, tenue par le relais du
    /// monde : `act` (`ZEUS_…`), demande `seq` du jeu. `chunk` : où vise le coup
    /// (`ZEUS_SKY`, `ZEUS_STRIKE`, `ZEUS_ARC`). `seed` : graine du monde (`ZEUS_SYNC` du
    /// créateur du monde : le relais en tire les éclairs des nuages qui la chargent).
    /// `charges` : ses charges en solo (`ZEUS_HOLD`). Réponse : `Response::Done` ; ce qu'en
    /// dit le relais arrive par `Response::Zeus` (poussé).
    Zeus {
        seq: u32,
        act: u8,
        chunk: [i32; 2],
        seed: u32,
        charges: u8,
    },
    /// Voie rapide (`crate::fast`) : ce jeu peut servir de passeur. `port` : son port UDP ;
    /// `external` : l'adresse publique (IPv4, port) que sa box lui a donnée en ouvrant ce
    /// port, si elle l'a fait (sinon le relais essaie l'adresse d'où vient la connexion,
    /// avec `port`). Le relais vérifie en y envoyant une sonde UDP, à laquelle le jeu
    /// répond par `PasseurEcho` : seul un passeur vérifié reçoit des routes.
    PasseurOffer {
        port: u16,
        external: Option<([u8; 4], u16)>,
    },
    /// La sonde UDP du relais est arrivée (même numéro) : le port est joignable.
    PasseurEcho {
        nonce: u64,
    },
    /// Ce jeu ne sert plus de passeur.
    PasseurOff,
    /// Hôte d'une partie FPS : demande des passeurs pour ses joueurs, groupés par équipe
    /// (un passeur ne porte qu'une équipe par partie). Refaite à chaque changement (début
    /// de partie, joueur arrivé ou parti, nouvel hôte) : tout est redonné (`FastHost` à
    /// l'hôte, `FastGuest` à chaque joueur), avec des routes et des clés neuves.
    FastWant {
        groups: Vec<Vec<u64>>,
    },
    /// (v274) Hôte d'une partie FPS : le passeur numéro `via` de la route du joueur `peer`
    /// est muet ou nettement plus lent que l'autre (aller-retour mesuré de bout en bout).
    /// Le relais l'écarte de cette partie et refait l'attribution avec un autre.
    FastBad {
        peer: u64,
        via: u8,
    },
    /// (v285) Construction, dans une foule : une position de plus, pour les seuls voisins
    /// les plus proches (3 : `POS_CLOSE_TO` côté relais). Avec elles, ces voisins voient le
    /// joueur 256 fois par seconde ; les autres continuent de recevoir les `Nearby`, au
    /// rythme d'avant.
    NearbyClose {
        chunk: [i32; 2],
        data: Vec<u8>,
    },
    /// (v301) Ce jeu ouvre, ou garde ouvert, un tunnel iroh vers le relais du nœud `node`
    /// (région, province, relais de secours, hébergés par d'autres joueurs) : le relais du
    /// monde lui donne une route par des passeurs (`Response::TunnelWay`, à lui et à
    /// l'hôte du nœud), s'il en a. Refaite toutes les 30 s tant que le tunnel sert : une
    /// route qu'on ne redemande plus est reprise. Sans réponse : le tunnel passe, comme
    /// avant, par les serveurs relais d'iroh.
    TunnelWant {
        node: [u8; 32],
    },
    /// (v304) D'un bout du tunnel de la route `route` : plus rien n'y passe depuis
    /// quelques secondes. Le relais écarte ses passeurs pour ce tunnel et en donne
    /// d'autres, sur la même route.
    TunnelBad {
        route: u64,
    },
    /// (v307) Lignes du journal réseau d'un joueur qui a accepté de les partager (voir
    /// `crate::netlog`), au relais qui a dit les recevoir (`Response::NetLogWanted`).
    /// Ni pseudo, ni adresse, ni nœud dans ces lignes.
    NetLog {
        lines: Vec<String>,
    },
    /// (v307) Pendant une partie FPS : ce joueur signale la carte (elle n'a rien d'une
    /// carte FPS, ou elle est faite pour gonfler un classement). Une fois par compte et
    /// par carte ; voir `maps::Store::rankable` côté relais.
    MapReport,
    /// (v311) Le créateur d'un monde de construction, qui y est, le supprime : tous ses
    /// joueurs en sortent, le relais oublie ses réglages (OP, bannis, liste blanche,
    /// positions) et il quitte la liste des mondes. Si le compte a la double
    /// authentification, `code` doit en être un code valable (sinon `WrongTotp`) ; sinon
    /// le jeu fait confirmer d'un clic avant d'envoyer. Les copies enregistrées sur les PC
    /// ne sont pas touchées.
    DeleteWorld {
        code: Option<u32>,
    },
    /// (v315) La suite du classement mondial, à partir du rang `from` (compté de zéro) :
    /// `RANK_PAGE` lignes au plus (réponse `Leaderboard`). Le classement n'est plus coupé
    /// aux 50 premiers : le jeu demande la page suivante quand on approche de la fin.
    LeaderboardFrom {
        from: u32,
    },
}

/// (v244) `Request::Zeus` : où en est l'éclair du serveur (rien ne change ; le créateur du
/// monde y joint la graine).
pub const ZEUS_SYNC: u8 = 0;
/// Le prendre dans le menu créatif ou par `/give` (une seule fois par serveur ; en
/// créatif d'après le relais).
pub const ZEUS_CLAIM: u8 = 1;
/// Un coup vers le ciel : deux font une charge.
pub const ZEUS_SKY: u8 = 2;
/// La foudre chargée, du ciel sur ce qu'on vise : une charge de moins.
pub const ZEUS_STRIKE: u8 = 3;
/// Le petit éclair sans charge, de sa pointe.
pub const ZEUS_ARC: u8 = 4;
/// Le créateur du monde l'a déjà, de son monde en solo (le serveur n'en a pas encore).
pub const ZEUS_HOLD: u8 = 5;
/// (Poussé par le relais seulement) l'orage vient de la charger ((v282) une charge toutes les
/// 10 secondes ; avant, un éclair des nuages passé au-dessus de celui qui la porte).
pub const ZEUS_CLOUD: u8 = 6;
/// (v247) La rendre au menu créatif (supprimée en créatif : « la supprimer depuis le créatif la
/// remet dans l'inventaire créatif ») : personne ne l'a plus, ses charges restent avec elle.
pub const ZEUS_RETURN: u8 = 7;

/// (v244) Réponses du relais (`Response::Zeus::why`).
pub const ZEUS_OK: u8 = 0;
/// Déjà prise par quelqu'un (`holder`).
pub const ZEUS_TAKEN: u8 = 1;
/// Pas en créatif (le catalogue et `/give` sont pour le mode créatif).
pub const ZEUS_NOT_CREATIVE: u8 = 2;
/// Ce n'est pas lui qui l'a.
pub const ZEUS_NOT_HOLDER: u8 = 3;
/// Pas de charge pour la foudre du ciel.
pub const ZEUS_EMPTY: u8 = 4;
/// Trop de coups à la suite, monde en pause, demande absurde.
pub const ZEUS_BUSY: u8 = 5;

/// (v244) Charges de la Foudre de Zeus au plus, et coups vers le ciel pour une charge
/// (règles de Viendax : `claude/foudre-de-zeus.md`).
pub const ZEUS_MAX_CHARGES: u8 = 10;
pub const ZEUS_SKY_HITS: u8 = 2;

/// (v244) Durée de vie d'un laissez-passer de la Foudre de Zeus (`Response::Zeus::permit`),
/// en secondes : l'arbitre du chunk visé le refuse ensuite.
pub const ZEUS_PERMIT_S: u64 = 10;

/// (v244) Ce que signe le relais du monde pour laisser passer un coup de la Foudre de Zeus
/// (`BuildMsg::ZeusAsk`) chez l'arbitre du chunk visé (lui, ou le relais d'une province, qui
/// connaît sa clé) : le monde, le joueur, sa demande, la sorte de coup, le chunk, la fin.
pub fn zeus_permit_msg(
    room: u64,
    pseudo: &str,
    seq: u32,
    act: u8,
    chunk: [i32; 2],
    expires: u64,
) -> Vec<u8> {
    let mut m = b"minelib/foudre/v244".to_vec();
    m.extend_from_slice(&room.to_le_bytes());
    m.extend_from_slice(pseudo.to_ascii_lowercase().as_bytes());
    m.push(0);
    m.extend_from_slice(&seq.to_le_bytes());
    m.push(act);
    m.extend_from_slice(&chunk[0].to_le_bytes());
    m.extend_from_slice(&chunk[1].to_le_bytes());
    m.extend_from_slice(&expires.to_le_bytes());
    m
}

impl Request {
    /// Envoi sans réponse attendue (données de jeu).
    pub fn is_oneway(&self) -> bool {
        matches!(
            self,
            Request::ToHost { .. }
                | Request::StandbyReady { .. }
                | Request::Lease { .. }
                | Request::Resign { .. }
                | Request::WitnessBeat { .. }
                | Request::SetBackup { .. }
                | Request::GarantAvailable { .. }
                | Request::GarantReady
                | Request::Conflict { .. }
                | Request::Vote { .. }
                | Request::Verdict { .. }
                | Request::Voice { .. }
                | Request::ToPeer { .. }
                | Request::Broadcast { .. }
                | Request::Position { .. }
                | Request::PeerNearby { .. }
                | Request::PeerAsk { .. }
                | Request::PeerBatch { .. }
                | Request::PeerReject { .. }
                | Request::PeerResend { .. }
                | Request::ToZone { .. }
                | Request::Nearby { .. }
                | Request::Archive { .. }
                | Request::Ack { .. }
                | Request::Accuse { .. }
                | Request::Handover { .. }
                | Request::ShardReady { .. }
                | Request::Rtt { .. }
                | Request::Echo { .. }
                | Request::PasseurOffer { .. }
                | Request::PasseurEcho { .. }
                | Request::PasseurOff
                | Request::FastWant { .. }
                | Request::FastBad { .. }
                | Request::NearbyClose { .. }
                | Request::TunnelWant { .. }
                | Request::TunnelBad { .. }
                | Request::NetLog { .. }
                | Request::MapReport
                | Request::BulkEdit { .. }
                | Request::Served { .. }
                | Request::CheatFound { .. }
                | Request::Audited { .. }
                | Request::Spot { .. }
                | Request::VoiceTo { .. }
                | Request::PeerVoice { .. }
                | Request::ProvinceLease { .. }
                | Request::ProvinceArchive { .. }
                | Request::ProvinceShards { .. }
                | Request::ProvinceRelease { .. }
                | Request::ProvinceBag { .. }
                | Request::ProvinceBagBack { .. }
                | Request::ProvinceStandbyReady { .. }
                | Request::ProvinceCopied { .. }
        )
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    /// Version du jeu trop ancienne ou trop récente.
    Protocol,
    BadPseudo,
    BadPassword,
    PseudoTaken,
    BadProofOfWork,
    /// Pseudo inconnu ou mot de passe faux (indistinguables, exprès).
    WrongCredentials,
    WrongTotp,
    /// Trop d'essais : attendre (secondes dans le message).
    TooManyAttempts,
    NotLoggedIn,
    Banned,
    Internal,
    /// Refus expliqué par le message.
    Refused,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ServerEntry {
    pub id: u64,
    pub host: String,
    pub name: String,
    pub map: String,
    pub players: u32,
    pub max_players: u32,
    pub region: String,
    /// Partie d'un autre relais (relais fédérés) : son nœud, sa clé et, s'il en donne une,
    /// son adresse directe. Tout à zéro et vide : une partie de ce relais.
    pub relay_node: [u8; 32],
    pub relay_key: [u8; 32],
    pub relay_addr: String,
    /// (v311) Mondes de construction : plus grand nombre de joueurs présents en même
    /// temps, dernière activité et première ouverture (secondes depuis 1970 ; 0 : inconnu).
    pub peak: u32,
    pub active: u64,
    pub opened: u64,
    /// (v311) Monde endormi : son dernier joueur est parti, on n'y entre pas tant que
    /// celui qui en garde le dernier état (`keeper`), ou son créateur, ne l'a pas rouvert.
    pub asleep: bool,
    pub keeper: String,
}

impl ServerEntry {
    /// La partie est-elle sur un autre relais que celui qui a donné la liste ?
    pub fn elsewhere(&self) -> bool {
        self.relay_key != [0; 32]
    }
}

/// Ligne du classement (statistiques façon CS).
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct RankEntry {
    pub rank: u32,
    pub pseudo: String,
    pub elo: i32,
    pub matches: u32,
    pub wins: u32,
    pub losses: u32,
    pub ties: u32,
    pub kills: u32,
    pub deaths: u32,
    pub assists: u32,
    pub headshots: u32,
    pub damage: u64,
    pub rounds: u32,
    pub mvps: u32,
}

/// Carte FPS connue du relais.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct MapEntry {
    pub sig: [u8; 32],
    pub name: String,
    pub author: String,
    pub size: u64,
    /// Parties jouées dessus.
    pub plays: u32,
    /// Joueurs en attente d'une partie sur cette carte.
    pub waiting: u32,
    /// Le relais a la carte complète (téléchargeable).
    pub stored: bool,
    /// Joueurs d'une partie sur cette carte (10 : 5 contre 5, la seule classée).
    pub players: u8,
}

/// Ce qu'un jeu peut offrir comme garant de zones (construction).
#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq)]
pub struct GarantCaps {
    /// Cœurs du processeur.
    pub cores: u16,
    /// Images par seconde moyennes.
    pub fps: u16,
    /// Mémoire libre (Mio).
    pub ram_mb: u32,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum Response {
    Hello {
        protocol: u32,
        relay: String,
        /// Difficulté de la preuve de travail demandée à l'inscription (bits à zéro).
        pow_bits: u8,
    },
    /// Compte inscrit au registre (numéro de la ligne).
    Registered {
        index: u64,
    },
    /// Le compte a la double authentification : renvoyer `Login` avec le code.
    NeedTotp,
    /// Clé du compte qui porte ce pseudo sur ce relais (aucune : pseudo libre).
    Whois {
        pubkey: Option<[u8; 32]>,
    },
    /// Coffre chiffré et défi à signer.
    Vault {
        vault: Vec<u8>,
        challenge: [u8; 32],
        /// Après un code 2FA juste : jeton de cet appareil, désormais de confiance.
        device: Option<[u8; 32]>,
    },
    Welcome {
        pseudo: String,
        totp: bool,
    },
    TotpSecret {
        secret: Vec<u8>,
    },
    TotpOn,
    TotpOff,
    Servers {
        list: Vec<ServerEntry>,
    },
    Pong,
    Hosted {
        room: u64,
    },
    Joined {
        room: u64,
        host: String,
        map: String,
        map_sig: [u8; 32],
    },
    Leaderboard {
        list: Vec<RankEntry>,
        me: Option<RankEntry>,
    },
    Friends {
        list: Vec<String>,
        /// En ligne sur ce relais (même ordre que `list`). (v231) Seulement pour un ami qui
        /// nous a ajouté aussi, et qui n'est pas invisible.
        online: Vec<bool>,
        /// (v231) Où ils sont (même ordre ; défaut pour ceux qui sont hors ligne,
        /// `FRIEND_PENDING` pour ceux qui ne nous ont pas ajouté).
        places: Vec<FriendPlace>,
        /// (v231) Demandes d'ami : ceux qui nous ont ajouté et qu'on n'a pas ajoutés.
        requests: Vec<String>,
        /// (v242) Invitations reçues, pas encore répondues (la plus récente d'abord).
        invites: Vec<WorldInvite>,
    },
    /// Skin d'un joueur (vide : aucun).
    Skin {
        pseudo: String,
        png: Vec<u8>,
    },
    /// Poussé : ce joueur de la partie de construction a changé de skin.
    SkinChanged {
        pseudo: String,
    },
    /// Poussé : invitation dans le lobby de `from`.
    PartyInvite {
        from: String,
    },
    /// Poussé : son lobby (id 0 et liste vide : plus de lobby). Le premier est l'hôte.
    Party {
        id: u64,
        members: Vec<String>,
    },
    /// Poussé : l'hôte du lobby a trouvé une partie, on la rejoint (même équipe).
    PartyGo {
        room: u64,
        map: String,
        map_sig: [u8; 32],
    },
    Maps {
        list: Vec<MapEntry>,
    },
    /// Carte reçue jusqu'à `offset` ; `complete` : le relais l'a entière (vérifiée).
    MapStored {
        offset: u64,
        complete: bool,
    },
    MapPart {
        sig: [u8; 32],
        offset: u64,
        total: u64,
        data: Vec<u8>,
    },
    /// Construction : zones (chunks `x / ZONE`, `z / ZONE`) dont ce jeu devient ou n'est
    /// plus le garant.
    Zones {
        gained: Vec<[i32; 2]>,
        lost: Vec<[i32; 2]>,
    },
    /// Construction : données d'un joueur (pour le garant de la zone, ou d'un garant).
    FromZone {
        peer: u64,
        chunk: [i32; 2],
        data: Vec<u8>,
    },
    Left,
    Done,
    // Messages poussés par le relais à tout moment (pas des réponses).
    PeerJoined {
        peer: u64,
        pseudo: String,
        /// Lobby du joueur (0 : seul) : les joueurs d'un même lobby jouent ensemble.
        party: u64,
    },
    PeerLeft {
        peer: u64,
    },
    /// L'hôte a fermé la partie (ou est parti).
    RoomClosed,
    /// L'hôte est parti : ce jeu devient l'hôte (avec les joueurs restants).
    Promoted {
        members: Vec<(u64, String)>,
        /// L'ancien hôte reste dans la partie (remplacé par un garant) : son numéro.
        old_host: Option<u64>,
    },
    /// Garant indépendant : une partie à servir (`standby` : en attente, vérificateur).
    GarantTask {
        room: u64,
        map_sig: [u8; 32],
        map: String,
        members: Vec<(u64, String)>,
        standby: bool,
    },
    /// À une connexion de garant : héberger le relais d'une région d'un monde (joueur d'une
    /// autre partie, sans lien avec celle-ci). L'historique de la région suit (`FromZone`).
    ShardTask {
        room: u64,
        region: [i32; 2],
        /// (v155) Son propre billet (au nom `#relaisN`, signé par le relais du monde) pour
        /// entrer chez les autres relais de sa région : il ne reçoit plus le secret du monde.
        peer: Vec<u8>,
        peer_expires: u64,
        /// Rang de ce relais dans sa région.
        slot: u16,
        /// Clé du relais du monde (il certifie les clés des joueurs et peut signer à leur
        /// place).
        root: [u8; 32],
        /// Région d'une province confiée à un relais de province (v138) : le relais de
        /// région s'y rattache (jeton d'entrée) au lieu du relais du monde, et l'historique
        /// de la région vient de lui.
        province: Option<ProvinceAt>,
    },
    /// À l'hôte d'un relais de région : tous les relais prêts de sa région (lui compris).
    /// Ils se relient entre eux : une région très peuplée est servie par plusieurs relais.
    /// `sov` : la région est souveraine (voir `crate::sovereign`), son époque en cours.
    ShardPeers {
        region: [i32; 2],
        list: Vec<ShardInfo>,
        sov: Option<Sov>,
    },
    /// À l'hôte principal d'une région souveraine : modifications arrivées au relais du
    /// monde (joueur pas encore relié au relais de la région, modification en masse d'un
    /// OP), signées par lui à la place de leurs auteurs.
    RootEdits {
        region: [i32; 2],
        edits: Vec<crate::sovereign::Edit>,
        /// Certificats des auteurs des modifications signées par eux (v138 : transmises par
        /// un relais de province, qui ne signe pas à leur place).
        certs: Vec<crate::sovereign::Cert>,
    },
    /// Au joueur choisi : héberger le relais de secours du monde `room` (voir
    /// `StandbyInfo`) ; `root` : clé qui signe les certificats des comptes de ce monde.
    StandbyTask {
        room: u64,
        root: [u8; 32],
        info: StandbyInfo,
    },
    /// À l'hôte du relais de secours : une entrée de l'historique du monde, à son rang.
    Replica {
        zone: [i32; 2],
        seq: u64,
        data: Vec<u8>,
    },
    /// À l'hôte du relais de secours : sac d'un joueur en survie, et mode donné par un OP.
    ReplicaBag {
        pseudo: String,
        items: Vec<(u32, u32)>,
        mined: u32,
    },
    ReplicaMode {
        pseudo: String,
        mode: String,
    },
    /// Aux gardiens d'un monde, quatre fois par seconde : battement du relais du monde
    /// (`t` : son heure, à rappeler dans le bail), la liste des gardiens (signée), et où
    /// joindre le relais de secours.
    Beat {
        t: u64,
        committee: crate::guard::Committee,
        standby: Option<StandbyAt>,
        /// Le monde est en pause chez lui (il n'a plus le quorum des témoins) : le relais de
        /// secours peut reprendre avec leurs votes même s'il l'entend encore.
        paused: bool,
    },
    /// Au garant choisi : être témoin du monde `room` (clé des listes et des certificats
    /// de ce relais ; certificat de la clé de son compte, pour entrer chez le relais de
    /// secours).
    WitnessTask {
        room: u64,
        root: [u8; 32],
        cert: crate::sovereign::Cert,
    },
    /// Relais de secours, au témoin qui a une liste plus ancienne que la sienne.
    Committee {
        committee: crate::guard::Committee,
    },
    /// Au relais de secours : le relais du monde s'arrête proprement et ne servira plus
    /// jamais ce monde ; il peut le reprendre tout de suite (`sig` : cet arrêt, signé par
    /// le relais du monde, que les joueurs vérifient).
    Released {
        room: u64,
        sig: Vec<u8>,
    },
    /// Aux joueurs d'un monde qui a des gardiens, deux fois par seconde au moins : le
    /// monde est en pause (le relais du monde n'entend plus assez de gardiens, rien n'y
    /// change) ou non.
    Paused {
        paused: bool,
    },
    /// Aux joueurs d'un monde : où est son relais de secours (aucun : pas encore).
    Standby {
        at: Option<StandbyAt>,
    },
    /// Relais de secours : défi à signer avec la clé du compte ; pour un joueur, la preuve
    /// qu'il a le droit de servir le monde (votes des gardiens, ou arrêt propre du relais du
    /// monde), à vérifier avant d'y entrer.
    TakeoverChallenge {
        challenge: [u8; 32],
        proof: Option<crate::guard::Proof>,
    },
    /// Aux joueurs d'un monde : relais de ses régions (les autres passent par ce relais).
    Shards {
        list: Vec<ShardInfo>,
    },
    /// Provinces (v138), au joueur choisi (connexion de garant) : tenir le relais de la
    /// province `province` du monde `room` (`size` régions de côté), époque `epoch`.
    /// L'historique des blocs suit (`ProvinceSeed`).
    ProvinceTask {
        room: u64,
        province: [i32; 2],
        size: i32,
        epoch: u64,
        secret: [u8; 32],
        root: [u8; 32],
        info: StandbyInfo,
        settings: ProvinceSettings,
        /// (v141) Relais de secours : la province est servie par ce relais (avec le jeton
        /// qui lui prouve que le relais du monde l'a choisi) ; on en tient une copie, prête
        /// à prendre sa place (`ProvincePromote`).
        follow: Option<Box<ProvinceAt>>,
        /// (v143) Province d'un pays : tout passe par le relais de ce pays (on y entre avec
        /// ce jeton, `Request::ProvinceChild`), pas par le relais du monde.
        parent: Option<Box<ProvinceAt>>,
        /// (v143) Pays : il confie lui-même ses provinces peuplées (de cette taille).
        nest: Option<Nest>,
        /// (v148) Étages d'en dessous : ses provinces seront à leur tour des pays (le premier
        /// est le leur), et ainsi de suite.
        deeper: Vec<Nest>,
    },
    /// (v141) Relais de province → son relais de secours : un envoi `n` gardé par le relais
    /// du monde (zone, rang, message), et les sacs de survie qu'elle tient.
    ProvinceCopy {
        room: u64,
        province: [i32; 2],
        epoch: u64,
        n: u64,
        entries: Vec<([i32; 2], u64, Vec<u8>)>,
        bags: Vec<BagCopy>,
        held: Vec<String>,
        /// (v144) Positions de retour de ses joueurs (toutes, de temps en temps).
        spots: Vec<(String, [f32; 5])>,
    },
    /// (v144) Relais d'un pays → son relais de secours : ses provinces (pour qu'elles se
    /// rattachent à lui s'il prend la place du pays). (v150) Seulement celles qui ont changé,
    /// et celles qui ne sont plus servies (`gone`) ; toutes d'un coup (`full`) quand il
    /// arrive.
    ProvinceChildren {
        room: u64,
        province: [i32; 2],
        epoch: u64,
        children: Vec<ChildCopy>,
        gone: Vec<[i32; 2]>,
        full: bool,
    },
    /// (v145) Relais d'un pays → une de ses provinces : ses voisines (les provinces du pays
    /// proches d'elle, elle comprise) ; elle les annonce à ses joueurs, qui n'ont plus besoin
    /// d'entrer chez le pays pour les connaître.
    ProvinceNeighbours {
        room: u64,
        province: [i32; 2],
        epoch: u64,
        list: Vec<ProvinceInfo>,
    },
    /// (v144) Relais d'un pays → hôte d'une de ses provinces (ou de son relais de secours) :
    /// où est le relais de secours du pays ; s'il prend la place du pays, la province s'y
    /// rattache (même jeton) au lieu de s'arrêter.
    ProvinceParent {
        room: u64,
        province: [i32; 2],
        epoch: u64,
        standby: Option<ProvinceInfo>,
    },
    /// (v141) Relais du monde → hôte du relais de secours d'une province (époque `old`) :
    /// le relais de la province ne répond plus ; ce relais prend sa place (époque `epoch`).
    ProvincePromote {
        room: u64,
        province: [i32; 2],
        old: u64,
        epoch: u64,
    },
    /// (v139) Réglages du serveur changés (OP, bannis, mode par défaut) : à chaque relais
    /// de province du monde.
    ProvinceSettings {
        room: u64,
        settings: ProvinceSettings,
    },
    /// (v139) Le sac de survie d'un joueur, prêté à la province (elle seule s'en sert
    /// jusqu'à ce qu'elle le rende) ; `mode` : son mode de jeu donné par un OP.
    ProvinceBagLent {
        room: u64,
        province: [i32; 2],
        epoch: u64,
        pseudo: String,
        items: Vec<(u32, u32)>,
        mined: u32,
        mode: Option<String>,
    },
    /// (v139) Le relais du monde redemande ce sac (le joueur est ailleurs) : il faut le
    /// rendre (`ProvinceBagBack`) et ne plus s'en servir.
    ProvinceBagRecall {
        room: u64,
        province: [i32; 2],
        epoch: u64,
        pseudo: String,
    },
    /// (v139) WorldEdit d'un OP dans la province : les cases, chacune signée par le relais
    /// du monde à la place de l'OP (transmise telle quelle à l'hôte principal d'une région
    /// souveraine).
    ProvinceBulk {
        room: u64,
        province: [i32; 2],
        epoch: u64,
        pseudo: String,
        edits: Vec<crate::sovereign::Edit>,
    },
    /// Historique des blocs de la province : (zone, rang, message). `go` (dernier envoi,
    /// quand l'hôte est prêt) : premier rang de sa plage ; il sert dès son premier bail.
    ProvinceSeed {
        room: u64,
        province: [i32; 2],
        epoch: u64,
        entries: Vec<([i32; 2], u64, Vec<u8>)>,
        go: Option<u64>,
    },
    /// Réponse au renouvellement du bail (`ok` : faux si le monde est en pause, ou si la
    /// province n'est plus confiée à cette époque).
    ProvinceLeased {
        room: u64,
        province: [i32; 2],
        epoch: u64,
        t: u64,
        ok: bool,
        /// (v142) Vrai : ce qu'elle publie part au relais du monde (il en garde la copie) ;
        /// faux : seulement à son relais de secours, qui la garde (le relais du monde n'en a
        /// plus que les sacs et les positions de retour).
        copies: bool,
        /// (v142) Sa copie n'est plus à jour : tout ce qu'elle a doit lui partir (numéro de
        /// la demande ; 0 : aucune).
        dump: u64,
        /// (v143) Durée du bail (ms, à partir de `t`) : celui d'une province d'un pays finit
        /// avant celui du pays.
        ms: u32,
        /// (v147) Pays → ses provinces : il appelle des garants parmi ses joueurs (elles le
        /// disent aux leurs, `GarantCall`).
        call: Option<GarantCall>,
        /// (v147) Relais du monde → pays : nouvelle plage de rangs (son début ; `low`).
        more: Option<u64>,
        /// (v150) Régions à dire une par une dans `counts` : celles qui ont au moins ce
        /// nombre de joueurs (les relais de région du relais du monde commencent là). Ses
        /// provinces servies (`children`) : seulement si c'est le relais du monde qui les
        /// demande (un pays n'en a pas besoin).
        dense: u32,
        kids: bool,
    },
    /// (v147) Relais d'un pays (ou d'une de ses provinces) → ses joueurs : le pays appelle des
    /// garants parmi eux ; chacun en est s'il tire moins que `share` (d'après son pseudo,
    /// `GarantCall::picks`), et prête alors sa connexion de garant au pays (`at`).
    GarantCall {
        room: u64,
        call: Option<GarantCall>,
    },
    /// Le relais du monde a gardé l'envoi `n` de la province (et tous ceux d'avant).
    ProvinceArchived {
        room: u64,
        province: [i32; 2],
        epoch: u64,
        n: u64,
    },
    /// Relais de province, à ses joueurs : ce qu'il vous a envoyé jusqu'ici part au relais
    /// du monde dans l'envoi `n` (voir `ProvinceConfirmed`).
    ProvinceMark {
        n: u64,
    },
    /// Relais de province, à ses joueurs : le relais du monde a gardé tout ce qui précède la
    /// marque `n`. Ce qui n'est pas confirmé ainsi reste en doute : si la province tombe,
    /// ce que le relais du monde n'a pas reçu revient comme avant (jamais deux versions du
    /// monde).
    ProvinceConfirmed {
        n: u64,
    },
    /// Le relais du monde reprend la province (ou la confie ailleurs) : le relais de
    /// province s'arrête.
    ProvinceStop {
        room: u64,
        province: [i32; 2],
        epoch: u64,
    },
    /// Aux joueurs d'un monde : les provinces confiées à un relais de province, en vue (les
    /// blocs de leurs zones viennent de ce relais ; les autres du relais du monde).
    Provinces {
        list: Vec<ProvinceInfo>,
    },
    /// Billet pour entrer dans les relais de région de ce monde, et certificat de la clé
    /// du compte (pour signer ses modifications dans une région souveraine).
    ShardTicket {
        room: u64,
        pseudo: String,
        expires: u64,
        sig: Vec<u8>,
        cert: Option<crate::sovereign::Cert>,
        /// Clé du relais du monde (celle des certificats) : le jeu vérifie avec elle les
        /// bonjours signés de la voix (v137).
        root: [u8; 32],
    },
    /// Garant extérieur d'un serveur de construction (un joueur d'une autre partie, sans
    /// lien avec celle-ci) : il recevra ses zones (`Zones`) et validera les modifications.
    BuildGarant {
        room: u64,
    },
    /// À l'hôte : le garant de secours désigné (garant indépendant).
    BackupIs {
        peer: u64,
    },
    /// À un troisième garant, sans lien avec la partie : le vérificateur accuse l'hôte de
    /// tricher. Les pièces suivent (`Evidence`) ; il rejoue tout et rend son `Verdict`.
    Arbitrate {
        case: u64,
        map_sig: [u8; 32],
        map: String,
    },
    /// Pièces de l'affaire, gardées par le relais lui-même (ce que le vérificateur a reçu
    /// de l'hôte et des joueurs, dans l'ordre), en morceaux : `Vec<(Option<u64>, Vec<u8>)>`
    /// (joueur et sa demande, ou une copie entière de la partie).
    Evidence {
        case: u64,
        part: u16,
        of: u16,
        data: Vec<u8>,
    },
    /// À un joueur : un désaccord le concerne ; juger avec sa propre prédiction.
    Judge {
        conflict: u64,
        seq: u32,
        host_pos: [f32; 3],
        audit_pos: [f32; 3],
    },
    /// Chat vocal d'un joueur (0 = l'hôte en FPS).
    Voice {
        from: u64,
        data: Vec<u8>,
    },
    /// Réponse à `ServerAdmin` : texte à afficher.
    ServerAdmin {
        text: String,
    },
    /// Exclu de la partie (banni de ce serveur), avec la raison.
    Kicked {
        reason: String,
    },
    /// Tué par un OP du serveur (`/kill`).
    Slain {
        by: String,
    },
    /// (v146) Réponse à `Doze` : plus rien à se dire (la connexion se ferme).
    Dozed {
        room: u64,
    },
    /// (v146) Relais du monde → relais de ses provinces (et d'un pays → ses provinces) : un
    /// OP a tué ce joueur, qui n'est pas connecté au relais du monde (au fond d'un pays) ;
    /// la province où il se trouve le lui dit.
    ProvinceSlay {
        room: u64,
        pseudo: String,
        by: String,
    },
    /// L'hôte est parti : `host` reprend la partie.
    HostChanged {
        host: String,
    },
    FromPeer {
        peer: u64,
        data: Vec<u8>,
    },
    FromHost {
        data: Vec<u8>,
    },
    /// Copie, pour le garant vérificateur, d'une demande d'un joueur à l'hôte (tir,
    /// grenade, achat…) : le relais atteste qu'elle vient bien de ce joueur.
    Witness {
        peer: u64,
        data: Vec<u8>,
    },
    Error {
        code: ErrorCode,
        message: String,
    },
    /// Mode de jeu donné par un OP du serveur de construction (`/gamemode`), ou mode par
    /// défaut du serveur à l'arrivée (`by` vide). `mode` : nom du mode (`creative`…).
    SetGamemode {
        mode: String,
        by: String,
    },
    /// Arrivée sur un serveur de construction : là où l'on était à son départ (le relais
    /// la garde ; ce qu'on a fait ailleurs entre-temps n'y change rien).
    BuildSpawn {
        pos: [f32; 3],
        yaw: f32,
        pitch: f32,
    },
    /// Réponse à `BulkEdit` : `error` vide si tout est publié, sinon pourquoi c'est refusé.
    /// Heure du serveur de construction (à l'arrivée, et quand un OP la change).
    SetTime {
        hours: f32,
        fixed: bool,
    },
    BulkDone {
        id: u32,
        error: String,
    },
    /// Poussé : le relais mesure l'aller-retour jusqu'à ce jeu, qui renvoie `Echo` aussitôt.
    Probe {
        nonce: u32,
    },
    /// Du relais du monde à l'hôte d'un relais de région (surveillance mutuelle) : un lot
    /// d'une région souveraine d'un autre hôte, tiré au hasard, à vérifier comme le ferait
    /// le relais du monde (`crate::sovereign::judge`) ; réponse `Request::Audited`. Le
    /// relais du monde ne revérifie lui-même que ce qui est signalé.
    Audit {
        id: u64,
        room: u64,
        region: [i32; 2],
        epoch: u64,
        /// Clé de l'hôte principal qui a signé le lot, et clé du relais du monde (celle des
        /// certificats).
        key: [u8; 32],
        root: [u8; 32],
        floor_t: u64,
        batch: crate::sovereign::Batch,
    },
    /// Du relais du monde à l'hôte d'un relais de région, deux fois par seconde : le monde
    /// `room` est servi (ou en pause). Le relais de région le redit à ses joueurs : le
    /// relais du monde n'a plus à l'envoyer lui-même à chacun d'eux.
    WorldBeat {
        room: u64,
        paused: bool,
    },
    /// Construction (v137) : voix d'un joueur, nommé par le relais qui la transmet (celui de
    /// sa session, pas celui qu'il annonce).
    VoiceFrom {
        from: String,
        data: Vec<u8>,
    },
    /// (v229) Météo du serveur de construction (à l'arrivée, et quand un OP la change) :
    /// `kind` (`WEATHER_…`) imposé pour encore `left` secondes (négatif : sans fin), ou
    /// `WEATHER_NATURAL` : celle des horloges du monde. `fixed` : figée (`/weather fix`).
    SetWeather {
        kind: u8,
        left: f32,
        fixed: bool,
    },
    /// (v231) Poussé : `from` nous invite dans son monde de construction `room` (`name`).
    Invited {
        from: String,
        room: u64,
        name: String,
    },
    /// (v243) Poussé : la partie FPS du serveur de construction est ouverte, menée par `host`
    /// (`open`), ou fermée.
    BuildMatch {
        host: String,
        open: bool,
    },
    /// (v243) Poussé à l'hôte de la partie FPS d'un serveur de construction : le joueur `peer`
    /// (`pseudo`) y entre (`joined`) ou en sort.
    MatchPeer {
        peer: u64,
        pseudo: String,
        joined: bool,
    },
    /// (v242) Poussé : `by` a refusé notre invitation dans le monde `name`.
    InviteDeclined {
        by: String,
        name: String,
    },
    /// (v231) Poussé : `from` vient de nous ajouter en ami ; `mutual` : on l'avait ajouté
    /// (vous êtes amis), sinon c'est une demande.
    FriendAdded {
        from: String,
        mutual: bool,
    },
    /// (v240) Défi à faire signer par une clé d'accès (deux minutes, une fois).
    PasskeyChallenge {
        challenge: [u8; 32],
    },
    /// (v240) Clé d'accès vérifiée : son coffre, et un jeton d'appareil de confiance.
    PasskeyOk {
        vault: Vec<u8>,
        device: [u8; 32],
    },
    /// (v240) Clés d'accès du compte.
    Passkeys {
        list: Vec<PasskeyInfo>,
    },
    /// (v244) Poussé : la Foudre de Zeus du serveur. En réponse à la demande `seq` d'un
    /// `Request::Zeus` (`act`, `why` : `ZEUS_OK` ou la raison du refus), à son arrivée, quand
    /// quelqu'un la prend (`seq` 0, à tous), ou quand l'orage la charge (`ZEUS_CLOUD`, à
    /// celui qui l'a). `holder` : celui qui l'a (vide : personne). Pour un
    /// coup accepté, `permit` (signé par le relais du monde, jusqu'à `expires`, secondes
    /// Unix) le fait passer chez l'arbitre du chunk visé (`BuildMsg::ZeusAsk`).
    Zeus {
        seq: u32,
        act: u8,
        why: u8,
        holder: String,
        charges: u8,
        sky_hits: u8,
        expires: u64,
        permit: Vec<u8>,
    },
    /// Voie rapide, à un passeur : les routes à porter pour l'attribution `lot` (route, clé
    /// du sceau). Elles remplacent celles qu'il portait pour elle ; liste vide : plus rien.
    /// Il ne sait ni de quelle partie il s'agit, ni qui sont les joueurs.
    PasseurTask {
        lot: u64,
        routes: Vec<(u64, [u8; 32])>,
    },
    /// Voie rapide, à un joueur d'une partie FPS : sa route vers l'hôte (clé qui scelle de
    /// bout en bout, passeurs de son équipe). Aucun passeur : voie fermée, tout passe par
    /// le relais comme avant.
    FastGuest {
        route: u64,
        key: [u8; 32],
        passeurs: Vec<FastHop>,
    },
    /// Voie rapide, à l'hôte : les routes de ses joueurs (elles remplacent les précédentes).
    FastHost {
        ways: Vec<FastWay>,
    },
    /// (v301) Route d'un tunnel iroh par des passeurs, entre un joueur et le relais du
    /// nœud `node` : au joueur qui l'a demandée (`host` faux) et à l'hôte du nœud (`host`
    /// vrai). Le numéro de route reste le même tant que le tunnel vit ; redonnée avec
    /// d'autres passeurs quand l'un s'en va ; aucun passeur : route reprise. `own` (à
    /// l'hôte seulement) : le passeur est celui de son propre jeu.
    TunnelWay {
        node: [u8; 32],
        route: u64,
        host: bool,
        own: bool,
        passeurs: Vec<FastHop>,
    },
    /// (v304) Ce relais a (`n` > 0) ou n'a plus de passeurs vérifiés : dit à chaque
    /// connexion ouverte, et quand cela change. Sans passeur, le jeu ouvre ses tunnels
    /// sans demander de route ni l'attendre.
    PasseursKnown {
        n: u32,
    },
    /// (v304) À un passeur, pour une chaîne de deux : la route `route` (clé du sceau
    /// `seal`), qui remplace ce qu'il portait pour l'attribution `lot`. Les paquets du
    /// sens `dir` lui viennent de l'autre passeur, à cette adresse. Chacun des deux ne
    /// voit qu'un bout de la route.
    PasseurLink {
        lot: u64,
        route: u64,
        seal: [u8; 32],
        dir: u8,
        addr: [u8; 4],
        port: u16,
    },
    /// (v307) L'hôte de ce relais reçoit (ou ne reçoit plus) les journaux réseau des
    /// joueurs qui ont accepté de les partager (`Request::NetLog`).
    NetLogWanted {
        on: bool,
    },
}

/// Passeur d'une route de la voie rapide : où lui écrire (IPv4, port UDP) et la clé du
/// sceau qu'il vérifie.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct FastHop {
    pub addr: [u8; 4],
    pub port: u16,
    pub seal: [u8; 32],
}

/// Route de la voie rapide entre l'hôte et le joueur `peer`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct FastWay {
    pub peer: u64,
    pub route: u64,
    pub key: [u8; 32],
    pub passeurs: Vec<FastHop>,
}

impl FastHop {
    pub fn via(&self) -> crate::fast::Via {
        crate::fast::Via {
            addr: std::net::SocketAddr::from((self.addr, self.port)),
            seal: self.seal,
        }
    }
}

impl FastWay {
    pub fn way(&self) -> crate::fast::Way {
        crate::fast::Way {
            peer: self.peer,
            route: self.route,
            key: self.key,
            vias: self.passeurs.iter().map(FastHop::via).collect(),
        }
    }
}

/// (v240) Clé d'accès d'un compte, telle que le joueur la voit.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct PasskeyInfo {
    pub id: Vec<u8>,
    pub name: String,
    /// Création, dernière connexion (secondes Unix ; 0 : jamais).
    pub created: u64,
    pub used: u64,
}

/// (v231) Où est un ami en ligne : au menu (`FRIEND_MENU`), dans un monde de construction
/// (`FRIEND_BUILD`), dans une partie FPS (`FRIEND_FPS`) ; `room`, `name` : la partie ;
/// `joinable` : on peut l'y rejoindre (monde de construction ouvert à tous ou à nous).
/// `FRIEND_PENDING` : il ne nous a pas (encore) ajouté, on ne sait rien de lui.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct FriendPlace {
    pub kind: u8,
    pub room: u64,
    pub name: String,
    pub joinable: bool,
}

pub const FRIEND_MENU: u8 = 0;
pub const FRIEND_BUILD: u8 = 1;
pub const FRIEND_FPS: u8 = 2;
pub const FRIEND_PENDING: u8 = 3;

/// (v242) Invitation reçue dans le monde de construction d'un ami : de qui, le monde, sa
/// partie sur le relais s'il est ouvert (`open` ; sinon 0 : on ne peut pas encore y entrer).
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct WorldInvite {
    pub from: String,
    pub room: u64,
    pub name: String,
    pub open: bool,
}

/// (v242) Réponses à une invitation (`Request::InviteReply`).
pub const INVITE_ACCEPT: u8 = 0;
pub const INVITE_DECLINE: u8 = 1;
pub const INVITE_IGNORE: u8 = 2;

impl Response {
    /// Message poussé par le relais (pas une réponse à une requête).
    pub fn is_push(&self) -> bool {
        matches!(
            self,
            Response::PeerJoined { .. }
                | Response::PasseurTask { .. }
                | Response::TunnelWay { .. }
                | Response::PasseursKnown { .. }
                | Response::PasseurLink { .. }
                | Response::NetLogWanted { .. }
                | Response::FastGuest { .. }
                | Response::FastHost { .. }
                | Response::PartyInvite { .. }
                | Response::Invited { .. }
                | Response::InviteDeclined { .. }
                | Response::BuildMatch { .. }
                | Response::MatchPeer { .. }
                | Response::FriendAdded { .. }
                | Response::Party { .. }
                | Response::PartyGo { .. }
                | Response::SkinChanged { .. }
                | Response::PeerLeft { .. }
                | Response::RoomClosed
                | Response::Promoted { .. }
                | Response::GarantTask { .. }
                | Response::BackupIs { .. }
                | Response::Judge { .. }
                | Response::Voice { .. }
                | Response::HostChanged { .. }
                | Response::Kicked { .. }
                | Response::Slain { .. }
                | Response::SetGamemode { .. }
                | Response::BulkDone { .. }
                | Response::Probe { .. }
                | Response::SetTime { .. }
                | Response::SetWeather { .. }
                | Response::Zeus { .. }
                | Response::BuildSpawn { .. }
                | Response::Arbitrate { .. }
                | Response::Evidence { .. }
                | Response::FromPeer { .. }
                | Response::FromHost { .. }
                | Response::Witness { .. }
                | Response::Zones { .. }
                | Response::FromZone { .. }
                | Response::BuildGarant { .. }
                | Response::ShardTask { .. }
                | Response::ShardPeers { .. }
                | Response::RootEdits { .. }
                | Response::Shards { .. }
                | Response::ShardTicket { .. }
                | Response::ProvinceTask { .. }
                | Response::ProvinceCopy { .. }
                | Response::ProvincePromote { .. }
                | Response::ProvinceSeed { .. }
                | Response::ProvinceLeased { .. }
                | Response::ProvinceArchived { .. }
                | Response::ProvinceMark { .. }
                | Response::ProvinceConfirmed { .. }
                | Response::ProvinceStop { .. }
                | Response::Provinces { .. }
                | Response::ProvinceSettings { .. }
                | Response::ProvinceBagLent { .. }
                | Response::ProvinceBagRecall { .. }
                | Response::ProvinceBulk { .. }
                | Response::ProvinceChildren { .. }
                | Response::ProvinceParent { .. }
                | Response::ProvinceNeighbours { .. }
                | Response::ProvinceSlay { .. }
                | Response::GarantCall { .. }
                // (Relais de secours et gardiens, v133 à v135 : poussés eux aussi ; une
                // requête en cours ne doit pas les prendre pour sa réponse.)
                | Response::StandbyTask { .. }
                | Response::Replica { .. }
                | Response::ReplicaBag { .. }
                | Response::ReplicaMode { .. }
                | Response::Beat { .. }
                | Response::WitnessTask { .. }
                | Response::Committee { .. }
                | Response::Released { .. }
                | Response::Paused { .. }
                | Response::Standby { .. }
                | Response::Audit { .. }
                | Response::WorldBeat { .. }
                | Response::VoiceFrom { .. }
        )
    }
}

/// Lobby : inviter, accepter, refuser, quitter, exclure, promouvoir hôte.
pub const PARTY_INVITE: u8 = 0;
pub const PARTY_ACCEPT: u8 = 1;
pub const PARTY_DECLINE: u8 = 2;
pub const PARTY_LEAVE: u8 = 3;
pub const PARTY_KICK: u8 = 4;
pub const PARTY_PROMOTE: u8 = 5;
/// Joueurs au plus dans un lobby (une équipe de la plus grande carte). Pour lancer, le
/// lobby doit tenir dans une équipe de la carte choisie (la moitié de ses joueurs).
pub const PARTY_MAX: usize = FPS_MAX_PLAYERS as usize / 2;

/// Côté d'une zone de garant, en chunks.
pub const ZONE: i32 = 4;

/// Zone d'un chunk.
pub fn zone_of(chunk: [i32; 2]) -> [i32; 2] {
    [chunk[0].div_euclid(ZONE), chunk[1].div_euclid(ZONE)]
}

/// Côté d'une région (servie par son propre relais quand elle est peuplée), en chunks.
pub const REGION: i32 = 32;

pub fn region_of(chunk: [i32; 2]) -> [i32; 2] {
    [chunk[0].div_euclid(REGION), chunk[1].div_euclid(REGION)]
}

/// Ce que le relais de secours d'un monde doit savoir pour le reprendre.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct StandbyInfo {
    pub name: String,
    pub map: String,
    pub map_sig: [u8; 32],
    pub max_players: u32,
    /// Qui a créé le monde, et sa clé de serveur (sacs de survie, modes).
    pub creator: String,
    pub server: String,
    /// Pseudo du propriétaire du moment (celui qui enregistre le monde).
    pub owner: String,
}

/// Où joindre le relais de secours d'un monde.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct StandbyAt {
    /// Nœud iroh de son hôte (tunnel), adresse directe, clé du canal chiffré.
    pub node: [u8; 32],
    pub addr: String,
    pub key: [u8; 32],
    /// Le monde (son numéro chez le relais du monde), le pseudo de son hôte, et la clé
    /// des listes de gardiens du relais du monde : de quoi vérifier la preuve qu'il montre
    /// avant d'y reprendre la partie.
    pub room: u64,
    pub pseudo: String,
    pub root: [u8; 32],
}

/// Provinces (v138) : régions par côté d'une province. Le relais du monde confie une
/// province peuplée au relais d'un joueur (hors de cette province), sous bail.
pub const PROVINCE: i32 = 4;
/// Bail d'un relais de province : renouvelé toutes les `PROVINCE_RENEW_MS`, valable
/// `PROVINCE_LEASE_MS` à partir de l'envoi de la demande. Le relais du monde ne reprend
/// la province que `PROVINCE_MARGIN_MS` après la fin du dernier bail qu'il a donné : les
/// deux ne la servent jamais en même temps.
pub const PROVINCE_RENEW_MS: u64 = 300;
pub const PROVINCE_LEASE_MS: u64 = 1500;
pub const PROVINCE_MARGIN_MS: u64 = 250;

/// Province d'une région, pour des provinces de `size` régions de côté.
pub fn province_of(region: [i32; 2], size: i32) -> [i32; 2] {
    let size = size.max(1);
    [region[0].div_euclid(size), region[1].div_euclid(size)]
}

/// (v147) Un pays appelle des garants parmi ses joueurs : où le joindre, et la part de ses
/// joueurs appelés.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct GarantCall {
    pub at: ProvinceInfo,
    pub share: f32,
}

impl GarantCall {
    /// Ce joueur est-il appelé ? Tirage fixe d'après son pseudo (les mêmes joueurs d'un
    /// appel à l'autre).
    pub fn picks(&self, pseudo: &str) -> bool {
        let mut h: u32 = 0x811c_9dc5;
        for b in pseudo.to_ascii_lowercase().bytes() {
            h ^= b as u32;
            h = h.wrapping_mul(0x0100_0193);
        }
        (h as f64 / u32::MAX as f64) < self.share as f64
    }
}

/// Où joindre le relais d'une province (v138).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ProvinceInfo {
    pub province: [i32; 2],
    /// Régions par côté.
    pub size: i32,
    pub epoch: u64,
    /// Nœud iroh de son hôte (tunnel), adresse directe (réseau local), clé du canal.
    pub node: [u8; 32],
    pub addr: String,
    pub key: [u8; 32],
}

impl ProvinceInfo {
    /// La région `region` en fait partie.
    pub fn has(&self, region: [i32; 2]) -> bool {
        province_of(region, self.size) == self.province
    }

    /// (v143) Sa première région (coin nord-ouest).
    pub fn first_region(&self) -> [i32; 2] {
        let s = self.size.max(1);
        [self.province[0] * s, self.province[1] * s]
    }

    /// (v143) Toute la province `inner` (plus petite) en fait partie.
    pub fn contains(&self, inner: &ProvinceInfo) -> bool {
        inner.size < self.size
            && self.size % inner.size.max(1) == 0
            && self.has(inner.first_region())
    }

    /// Le même relais de province (même époque).
    pub fn same_relay(&self, other: &ProvinceInfo) -> bool {
        (
            self.province,
            self.size,
            self.epoch,
            self.node,
            &self.addr,
            self.key,
        ) == (
            other.province,
            other.size,
            other.epoch,
            other.node,
            &other.addr,
            other.key,
        )
    }
}

/// (v143) Pays : un relais de province qui confie lui-même ses provinces peuplées (de `size`
/// régions de côté, à partir de `from` joueurs).
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
pub struct Nest {
    pub size: i32,
    pub from: u32,
}

/// (v144) Ce que le relais de secours d'un pays doit savoir de chacune de ses provinces pour
/// la reprendre avec son hôte : où la joindre (servie) et à quelle époque, qui la tient, qui
/// tient son relais de secours, si sa copie est chez lui, quels sacs de survie elle tient.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ChildCopy {
    pub province: [i32; 2],
    pub epoch: u64,
    pub info: Option<ProvinceInfo>,
    pub host: String,
    pub standby: Option<String>,
    pub big: bool,
    pub bags: Vec<String>,
    /// Dernière demande de tout renvoyer qu'elle a reçue (la suivante sera plus grande).
    pub dump_n: u64,
}

/// (v143) Relais de pays → relais du monde : il lui faut un hôte pour sa province `province`
/// (`size` régions de côté) à l'époque `epoch` ; `follow` : pour son relais de secours (la
/// province servie, à suivre).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Want {
    pub province: [i32; 2],
    pub size: i32,
    pub epoch: u64,
    pub follow: Option<ProvinceInfo>,
}

/// Copie d'un sac de survie (v139) : pseudo, objets (code, nombre), crédits de minage.
pub type BagCopy = (String, Vec<(u32, u32)>, u32);

/// Réglages d'un serveur de construction qu'un relais de province doit connaître (v139) :
/// mode de jeu par défaut, OP, bannis, et modes donnés par un OP à des joueurs (pseudo,
/// mode) : c'est le mode qui décide si l'inventaire d'un joueur est tenu par le relais.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct ProvinceSettings {
    pub default_mode: Option<String>,
    pub ops: Vec<String>,
    pub bans: Vec<String>,
    pub modes: Vec<(String, String)>,
    /// (v146) Heure du serveur (heures, figée), quand un OP vient de la changer : la
    /// province la dit à ses joueurs (ceux qui ne sont plus connectés au relais du monde).
    pub time: Option<(f32, bool)>,
    /// (v229) Météo du serveur (temps, secondes qu'il lui reste, figée : comme dans
    /// `Response::SetWeather`), quand un OP vient de la changer.
    pub weather: Option<(u8, f32, bool)>,
}

/// Province d'un relais de région (dans `ShardTask`) : où la joindre, et le jeton qui
/// prouve que le relais du monde a confié la région à cet hôte.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ProvinceAt {
    pub info: ProvinceInfo,
    pub token: [u8; 32],
}

/// Jeton d'entrée de l'hôte d'un relais de région chez le relais de sa province (secret
/// partagé du monde).
pub fn province_token(
    secret: &[u8; 32],
    room: u64,
    region: [i32; 2],
    slot: u16,
    pseudo: &str,
    signer: &[u8; 32],
    epoch: u64,
) -> [u8; 32] {
    use hmac::Mac;
    let mut m = hmac::Hmac::<sha2::Sha256>::new_from_slice(secret).expect("clé");
    m.update(b"minelib/province/hote");
    m.update(&room.to_le_bytes());
    m.update(&region[0].to_le_bytes());
    m.update(&region[1].to_le_bytes());
    m.update(&slot.to_le_bytes());
    m.update(pseudo.to_ascii_lowercase().as_bytes());
    m.update(signer);
    m.update(&epoch.to_le_bytes());
    m.finalize().into_bytes().into()
}

/// (v141) Jeton du relais du monde qui fait d'un joueur (pseudo, clé de son compte) le
/// relais de secours de la province `province` à l'époque `epoch` (vérifié par le relais
/// de la province, qui a le secret du monde).
pub fn follow_token(
    secret: &[u8; 32],
    room: u64,
    province: [i32; 2],
    pseudo: &str,
    signer: &[u8; 32],
    epoch: u64,
) -> [u8; 32] {
    use hmac::Mac;
    let mut m = hmac::Hmac::<sha2::Sha256>::new_from_slice(secret).expect("clé");
    m.update(b"minelib/province/secours");
    m.update(&room.to_le_bytes());
    m.update(&province[0].to_le_bytes());
    m.update(&province[1].to_le_bytes());
    m.update(pseudo.to_ascii_lowercase().as_bytes());
    m.update(signer);
    m.update(&epoch.to_le_bytes());
    m.finalize().into_bytes().into()
}

/// (v143) Jeton du relais du monde qui fait d'un joueur (pseudo, clé de son compte) l'hôte de
/// la province `province` d'un pays (`size` régions de côté, époque `epoch`), ou de son
/// relais de secours (vérifié par le relais du pays, qui a le secret du monde).
#[allow(clippy::too_many_arguments)]
pub fn child_token(
    secret: &[u8; 32],
    room: u64,
    province: [i32; 2],
    size: i32,
    pseudo: &str,
    signer: &[u8; 32],
    epoch: u64,
    standby: bool,
) -> [u8; 32] {
    use hmac::Mac;
    let mut m = hmac::Hmac::<sha2::Sha256>::new_from_slice(secret).expect("clé");
    m.update(b"minelib/province/pays");
    m.update(&room.to_le_bytes());
    m.update(&province[0].to_le_bytes());
    m.update(&province[1].to_le_bytes());
    m.update(&size.to_le_bytes());
    m.update(pseudo.to_ascii_lowercase().as_bytes());
    m.update(signer);
    m.update(&epoch.to_le_bytes());
    m.update(&[standby as u8]);
    m.finalize().into_bytes().into()
}

/// Relais d'une région d'un monde.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ShardInfo {
    pub region: [i32; 2],
    /// Rang dans la région (plusieurs relais par région peuplée).
    pub slot: u16,
    pub node: [u8; 32],
    pub addr: String,
    pub key: [u8; 32],
    /// Clé du compte de l'hôte (il signe ce qu'il publie quand il est l'hôte principal
    /// d'une région souveraine).
    pub signer: [u8; 32],
    /// Époque de la région souveraine (0 : pas souveraine, les modifications vont au
    /// relais du monde).
    pub epoch: u64,
}

impl ShardInfo {
    /// Le même relais (son époque peut avoir changé : pas de quoi s'y reconnecter).
    pub fn same_relay(&self, other: &ShardInfo) -> bool {
        (
            self.region,
            self.slot,
            self.node,
            &self.addr,
            self.key,
            self.signer,
        ) == (
            other.region,
            other.slot,
            other.node,
            &other.addr,
            other.key,
            other.signer,
        )
    }
}

/// Époque d'une région souveraine (voir `crate::sovereign`), dite aux hôtes de la région.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Sov {
    pub epoch: u64,
    /// Rang (`ShardInfo::slot`) de l'hôte principal.
    pub primary: u16,
    /// Premier rang de l'époque.
    pub first: u64,
    /// Ce que le relais du monde a déjà archivé : rang suivant et empreinte. Un hôte qui
    /// arrive en cours d'époque vérifie à partir de là.
    pub base: u64,
    pub chain: [u8; 32],
    /// Date (ms) en dessous de laquelle une modification est périmée dans l'époque.
    pub floor_t: u64,
}

/// Nom sous lequel le relais de rang `slot` entre chez les autres relais de sa région
/// (jamais un pseudo de joueur : `#` n'y est pas permis).
pub fn shard_peer_name(slot: u16) -> String {
    format!("#relais{slot}")
}

/// Le relais de la région que prend le joueur `pseudo` parmi `list` : toujours le même
/// pour un joueur, les joueurs répartis entre les relais, et quand un relais s'en va seuls
/// ses joueurs changent (chacun garde le sien tant qu'il est là).
pub fn shard_for<'a>(list: &'a [ShardInfo], pseudo: &str) -> Option<&'a ShardInfo> {
    use sha2::Digest;
    list.iter().max_by_key(|i| {
        let mut h = sha2::Sha256::new();
        h.update(pseudo.to_ascii_lowercase().as_bytes());
        h.update(i.key);
        h.update(i.slot.to_le_bytes());
        let d: [u8; 32] = h.finalize().into();
        u64::from_le_bytes(d[..8].try_into().expect("8 octets"))
    })
}

/// (v155) Ce que signe le relais du monde (avec sa clé des certificats) dans un billet pour
/// les relais de région : le monde, le pseudo, la fin de validité. Les hôtes des régions le
/// vérifient avec la clé publique du relais du monde ; ils ne savent pas en faire.
pub fn ticket_msg(room: u64, pseudo: &str, expires: u64) -> Vec<u8> {
    let mut m = b"minelib/billet".to_vec();
    m.extend_from_slice(&room.to_le_bytes());
    m.extend_from_slice(pseudo.to_ascii_lowercase().as_bytes());
    m.push(0);
    m.extend_from_slice(&expires.to_le_bytes());
    m
}

/// (v155) Un billet signé par le relais du monde dont la clé publique est `root`.
pub fn ticket_ok(root: &[u8; 32], room: u64, pseudo: &str, expires: u64, sig: &[u8]) -> bool {
    use ed25519_dalek::Verifier;
    let Ok(key) = ed25519_dalek::VerifyingKey::from_bytes(root) else {
        return false;
    };
    let Ok(sig) = ed25519_dalek::Signature::from_slice(sig) else {
        return false;
    };
    key.verify(&ticket_msg(room, pseudo, expires), &sig).is_ok()
}

/// (v155) Clé d'une province (ou d'un pays) : dérivée de celle de son pays, ou du secret du
/// monde pour celles du relais du monde. Elle sert à vérifier les jetons de ceux qui y
/// entrent (son relais de secours, ses provinces, les relais de ses régions) ; seuls elle,
/// son pays (et ceux au-dessus) et le relais du monde savent en faire. Avant, chaque hôte
/// recevait le secret du monde, et pouvait faire les jetons de n'importe quelle province.
pub fn province_key(parent: &[u8; 32], room: u64, province: [i32; 2], size: i32) -> [u8; 32] {
    use hmac::Mac;
    let mut m = hmac::Hmac::<sha2::Sha256>::new_from_slice(parent).expect("clé");
    m.update(b"minelib/province/cle");
    m.update(&room.to_le_bytes());
    m.update(&province[0].to_le_bytes());
    m.update(&province[1].to_le_bytes());
    m.update(&size.to_le_bytes());
    m.finalize().into_bytes().into()
}

#[cfg(test)]
mod version_tests {
    #[test]
    fn reads_the_game_version_in_the_hello() {
        assert_eq!(super::client_version("minelib 0.1.0 v95"), 95);
        assert_eq!(super::client_version("minelib-garant 0.1.0 v7"), 7);
        assert_eq!(super::client_version("minelib 0.1.0"), 0);
        assert_eq!(super::client_version("relais minelib 0.1.0"), 0);
    }
}

#[cfg(test)]
mod lookalike_tests {
    use super::*;

    /// (v271) « Je n'arrive pas à ajouter mon pote 1side » : il s'appelle « lside ». Les
    /// variantes d'un pseudo par ses lettres qui se ressemblent, et le message du relais qui
    /// propose la bonne (et que le jeu relit pour un bouton « Ajouter »).
    #[test]
    fn a_lookalike_pseudo_is_proposed() {
        let v = lookalikes("1side", 64);
        assert_eq!(v[0], "1side");
        assert!(v.contains(&"lside".to_string()), "{v:?}");
        assert!(v.contains(&"iside".to_string()));
        assert!(v.contains(&"lslde".to_string()));
        assert_eq!(lookalikes("Fox", 64), vec!["fox".to_string(), "f0x".to_string()]);
        assert!(lookalikes("1111111111111111", 256).len() <= 256);
        let m = unknown_player("1side", Some("lside"));
        assert_eq!(
            m,
            "Joueur « 1side » inconnu. Voulais-tu dire « lside » (un L minuscule, pas le chiffre 1) ?"
        );
        assert_eq!(suggested_player(&m), Some("lside"));
        assert_eq!(unknown_player("1side", None), "Joueur « 1side » inconnu.");
        assert_eq!(suggested_player("Joueur « 1side » inconnu."), None);
        let m = unknown_player("lnes", Some("Ines"));
        assert!(m.ends_with("« Ines » (un I majuscule, pas un L minuscule) ?"), "{m}");
    }
}

#[cfg(test)]
mod push_tests {
    use super::*;

    /// (v307) Ce que le relais envoie de lui-même pour les tunnels, les passeurs et les
    /// journaux n'est jamais pris pour la réponse à une demande en cours. (Les v301 et
    /// v304 l'avaient oublié : un jeu qui attendait une réponse recevait « réponse
    /// inattendue ».)
    #[test]
    fn what_the_relay_sends_on_its_own_is_never_taken_for_a_reply() {
        for r in [
            Response::TunnelWay {
                node: [1; 32],
                route: 2,
                host: false,
                own: false,
                passeurs: Vec::new(),
            },
            Response::PasseursKnown { n: 1 },
            Response::PasseurLink {
                lot: 1,
                route: 1,
                seal: [0; 32],
                dir: 0,
                addr: [1, 2, 3, 4],
                port: 5,
            },
            Response::NetLogWanted { on: true },
            Response::PasseurTask {
                lot: 1,
                routes: Vec::new(),
            },
        ] {
            assert!(r.is_push(), "{r:?}");
        }
    }
}
