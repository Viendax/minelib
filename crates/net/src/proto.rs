//! Messages échangés entre le jeu et le relais (sérialisés en postcard, chiffrés par
//! `transport`).

use serde::{Deserialize, Serialize};

/// Version du protocole : le relais refuse un jeu trop ancien.
pub const PROTOCOL: u32 = 3;

/// Morceau d'une carte transférée (octets).
pub const MAP_PART: usize = 48 * 1024;
/// Taille maximale d'une carte partagée.
pub const MAP_MAX: u64 = 64 * 1024 * 1024;
/// Joueurs d'une partie FPS (5 contre 5).
pub const FPS_PLAYERS: u16 = 10;

/// Pseudo : 3 à 16 caractères, lettres sans accent, chiffres et `_`. Unique sans tenir
/// compte des majuscules.
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
        /// Empreinte de la machine, salée par la clé du relais (`hw`).
        hw: [u8; 32],
    },
    /// Administrateur du relais : bannit un compte (à vie) et ses machines (3 mois).
    Ban {
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
    /// Classement : les 50 meilleurs, ou ses amis.
    Leaderboard {
        friends: bool,
    },
    /// Ajoute ou retire un ami (pseudo vide : seulement la liste).
    Friend {
        pseudo: String,
        add: bool,
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
}

impl Request {
    /// Envoi sans réponse attendue (données de jeu).
    pub fn is_oneway(&self) -> bool {
        matches!(
            self,
            Request::ToHost { .. }
                | Request::SetBackup { .. }
                | Request::Voice { .. }
                | Request::ToPeer { .. }
                | Request::Broadcast { .. }
                | Request::Position { .. }
                | Request::ToZone { .. }
                | Request::Nearby { .. }
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
    /// Coffre chiffré et défi à signer.
    Vault {
        vault: Vec<u8>,
        challenge: [u8; 32],
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
    },
    PeerLeft {
        peer: u64,
    },
    /// L'hôte a fermé la partie (ou est parti).
    RoomClosed,
    /// L'hôte est parti : ce jeu devient l'hôte (avec les joueurs restants).
    Promoted {
        members: Vec<(u64, String)>,
    },
    /// Chat vocal d'un joueur (0 = l'hôte en FPS).
    Voice {
        from: u64,
        data: Vec<u8>,
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
    Error {
        code: ErrorCode,
        message: String,
    },
}

impl Response {
    /// Message poussé par le relais (pas une réponse à une requête).
    pub fn is_push(&self) -> bool {
        matches!(
            self,
            Response::PeerJoined { .. }
                | Response::PeerLeft { .. }
                | Response::RoomClosed
                | Response::Promoted { .. }
                | Response::Voice { .. }
                | Response::HostChanged { .. }
                | Response::FromPeer { .. }
                | Response::FromHost { .. }
                | Response::Zones { .. }
                | Response::FromZone { .. }
        )
    }
}

/// Côté d'une zone de garant, en chunks.
pub const ZONE: i32 = 4;

/// Zone d'un chunk.
pub fn zone_of(chunk: [i32; 2]) -> [i32; 2] {
    [chunk[0].div_euclid(ZONE), chunk[1].div_euclid(ZONE)]
}
