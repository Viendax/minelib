//! Messages échangés entre le jeu et le relais (sérialisés en postcard, chiffrés par
//! `transport`).

use serde::{Deserialize, Serialize};

/// Version du protocole : le relais refuse un jeu trop ancien.
pub const PROTOCOL: u32 = 24;

/// Version du jeu annoncée dans le bonjour (`client` : « minelib 0.1.0 v95 ») ; 0 si
/// absente.
pub fn client_version(client: &str) -> u32 {
    client
        .split_whitespace()
        .rev()
        .find_map(|w| w.strip_prefix('v')?.parse().ok())
        .unwrap_or(0)
}

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

/// Triches trouvées par le rejeu : sauts parfaits en chaîne (script de bunny hop), appuis
/// plus rapides qu'une main (macro).
pub const CHEAT_BHOP_SCRIPT: u8 = 1;
pub const CHEAT_MACRO: u8 = 2;
// (Elle renvoie aussi `BuildSpawn` : la position qu'on avait sur ce serveur.)

pub fn valid_pseudo(p: &str) -> bool {
    (3..=16).contains(&p.chars().count())
        && p.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
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
    /// Classement : les 50 meilleurs, ou ses amis.
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
    /// Relais de région : entrée avec le billet donné par le relais du monde.
    ShardLogin {
        room: u64,
        pseudo: String,
        expires: u64,
        mac: [u8; 32],
    },
    /// Hôte d'une région (connexion de garant) : son relais de région est prêt.
    ShardReady {
        region: [i32; 2],
        /// Nœud iroh (joignable de partout) et adresse directe (réseau local, essais).
        node: [u8; 32],
        addr: String,
        key: [u8; 32],
    },
    /// Hôte d'une région : les modifications que son relais a publiées (copie gardée par
    /// le relais du monde, qui la remet au propriétaire et à l'hôte suivant).
    RegionLog {
        entries: Vec<([i32; 2], Vec<u8>)>,
    },
    Position {
        chunk: [i32; 2],
    },
    /// Construction : données pour le garant de la zone de ce chunk (ou pour tous les
    /// joueurs proches si l'expéditeur en est le garant).
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
    /// Aller-retour mesuré par le jeu jusqu'au relais (ms) : le relais confie la partie
    /// à qui a la meilleure connexion quand celle de l'hôte est mauvaise.
    Rtt {
        ms: u16,
    },
    /// Modifications en masse d'un OP (WorldEdit) dans un monde de construction : le
    /// relais vérifie qu'il est OP, les publie comme des blocs validés et répond par
    /// `BulkDone` (même `id`).
    BulkEdit {
        id: u32,
        cells: Vec<([i32; 3], u16)>,
    },
}

impl Request {
    /// Envoi sans réponse attendue (données de jeu).
    pub fn is_oneway(&self) -> bool {
        matches!(
            self,
            Request::ToHost { .. }
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
                | Request::ToZone { .. }
                | Request::Nearby { .. }
                | Request::RegionLog { .. }
                | Request::ShardReady { .. }
                | Request::Rtt { .. }
                | Request::BulkEdit { .. }
                | Request::CheatFound { .. }
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
        /// En ligne sur ce relais (même ordre que `list`).
        online: Vec<bool>,
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
        secret: [u8; 32],
    },
    /// Aux joueurs d'un monde : relais de ses régions (les autres passent par ce relais).
    Shards {
        list: Vec<ShardInfo>,
    },
    /// Billet pour entrer dans les relais de région de ce monde.
    ShardTicket {
        room: u64,
        pseudo: String,
        expires: u64,
        mac: [u8; 32],
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
}

impl Response {
    /// Message poussé par le relais (pas une réponse à une requête).
    pub fn is_push(&self) -> bool {
        matches!(
            self,
            Response::PeerJoined { .. }
                | Response::PartyInvite { .. }
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
                | Response::SetTime { .. }
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
                | Response::Shards { .. }
                | Response::ShardTicket { .. }
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

/// Relais d'une région d'un monde.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ShardInfo {
    pub region: [i32; 2],
    pub node: [u8; 32],
    pub addr: String,
    pub key: [u8; 32],
}

/// Signature d'un billet de relais de région (secret partagé par le relais du monde et
/// les hôtes de ses régions).
pub fn shard_mac(secret: &[u8; 32], room: u64, pseudo: &str, expires: u64) -> [u8; 32] {
    use hmac::Mac;
    let mut m = hmac::Hmac::<sha2::Sha256>::new_from_slice(secret).expect("clé");
    m.update(&room.to_le_bytes());
    m.update(pseudo.to_ascii_lowercase().as_bytes());
    m.update(&expires.to_le_bytes());
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
