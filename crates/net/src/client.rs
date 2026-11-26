//! Côté jeu : un fil de fond parle au relais (connexion, inscription, 2FA, liste des
//! parties) pour que l'interface ne se fige jamais. Le jeu envoie des `Cmd` et lit des
//! `Event` à chaque image.

use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::time::{Duration, Instant};

/// Version du jeu (numéro des versions publiées), annoncée au relais : il ne fait jouer
/// ensemble que des jeux de la même version. Réglée par le jeu au démarrage.
pub static GAME_VERSION: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

use anyhow::{anyhow, Result};
use ed25519_dalek::Signer;

use crate::crypto;
use crate::proto::{self, ErrorCode, Request, Response, ServerEntry};
use crate::transport::{Channel, Inbound, Incoming};

/// Délai pour joindre le relais et pour chaque réponse.
const TIMEOUT: Duration = Duration::from_secs(10);
/// (v344) Une réponse en retard (voir `Worker::late`) est attendue au plus ce temps après le
/// délai de sa requête.
const LATE_KEEP: Duration = Duration::from_secs(120);

/// (v344) Ce qu'on dit d'un message du relais qu'on ne sait pas lire.
const UNREADABLE: &str = "message du relais illisible : il n'a sans doute pas la même version \
                          du jeu que toi (son hôte doit relancer son jeu pour qu'il se mette à \
                          jour)";

/// (v344) L'erreur vient-elle d'un message qu'on ne sait pas lire (format d'une autre
/// version) ?
fn unreadable(e: &anyhow::Error) -> bool {
    e.chain()
        .any(|c| c.downcast_ref::<postcard::Error>().is_some())
}
/// Ping régulier pour garder la session ouverte.
const KEEPALIVE: Duration = Duration::from_secs(20);
/// Mesure de l'aller-retour jusqu'au relais (envoyée au relais).
const RTT_EVERY: Duration = Duration::from_secs(2);
/// (v314) Un relais dont plus rien n'arrive depuis ce temps est tenu pour perdu : ni
/// message, ni sonde (il en envoie une toutes les 4 s), ni réponse à nos pings (toutes les
/// 2 s, toutes les 5 s quand ils restent sans réponse). Avant, un relais disparu sans
/// fermer sa connexion (PC éteint, réseau coupé) n'était remarqué qu'au bout des 30 s que
/// le tunnel iroh laisse à une connexion muette ; le jeu ne cherchait rien d'autre pendant
/// ce temps.
const SILENCE_MAX: Duration = Duration::from_secs(10);
/// (v314) … et il faut qu'un ping parti après le dernier message reçu soit resté sans
/// réponse ce temps-là : un jeu qui sort de veille (ou d'un long calcul) laisse au relais
/// le temps de répondre avant de le croire perdu.
const SILENCE_PING: Duration = Duration::from_secs(3);

#[derive(Debug, Clone)]
pub enum Cmd {
    Register {
        pseudo: String,
        password: String,
    },
    Login {
        pseudo: String,
        password: String,
        totp: Option<u32>,
        /// Coffre du compte gardé sur ce PC : sur un relais qui ne connaît pas le compte,
        /// il y est réinscrit tel quel (même clé, même pseudo).
        backup: Option<Vec<u8>>,
        /// Jeton d'appareil de confiance pour ce compte (pas de 2FA).
        device: Option<[u8; 32]>,
    },
    /// Connexion de garant au relais qui a repris le monde du joueur, avec le certificat de
    /// la clé de son compte (voir `Request::GarantLogin`).
    GarantLogin {
        cert: crate::sovereign::Cert,
        key: ed25519_dalek::SigningKey,
    },
    /// Entre comme témoin chez le relais de secours d'un monde (voir
    /// `Request::WitnessLogin`).
    WitnessLogin {
        cert: crate::sovereign::Cert,
        key: ed25519_dalek::SigningKey,
    },
    /// (v146) Au fond d'un pays : quitte la partie au relais du monde sans quitter le monde
    /// (ses provinces le servent), puis ferme la connexion (`Event::Dozed`).
    Doze {
        room: u64,
    },
    /// (v146) Revient au relais du monde (connexion neuve) après `Doze` : certificat de la
    /// clé du compte signé par ce relais, et cette clé. Comme une reprise : `TakenOver`.
    Wake {
        room: u64,
        cert: crate::sovereign::Cert,
        key: ed25519_dalek::SigningKey,
    },
    /// Reprend le monde sur son relais de secours (le relais du monde est tombé) : le
    /// certificat de la clé du compte signé par l'ancien relais du monde, et cette clé.
    Takeover {
        cert: crate::sovereign::Cert,
        key: ed25519_dalek::SigningKey,
        /// Le relais de secours attendu (voir `StandbyAt`) : il doit prouver qu'il a le
        /// droit de servir le monde (`crate::guard::Proof`), sinon on n'y entre pas.
        expect: Option<crate::proto::StandbyAt>,
    },
    /// Entre dans un relais de région avec le billet du relais du monde (et le certificat
    /// de la clé du compte, voir `crate::sovereign`).
    ShardLogin {
        room: u64,
        pseudo: String,
        expires: u64,
        sig: Vec<u8>,
        cert: Option<crate::sovereign::Cert>,
    },
    /// Hôte d'un relais de région rattaché à une province (v138) : entre chez le relais de
    /// la province avec le jeton du relais du monde (voir `proto::province_token`).
    RegionHost {
        room: u64,
        region: [i32; 2],
        slot: u16,
        pseudo: String,
        signer: [u8; 32],
        token: [u8; 32],
    },
    /// Relais de secours d'une province (v141) : suit le relais de la province avec le
    /// jeton du relais du monde (voir `proto::follow_token`).
    ProvinceFollow {
        room: u64,
        province: [i32; 2],
        epoch: u64,
        pseudo: String,
        signer: [u8; 32],
        token: [u8; 32],
        /// (v144) Où joindre le relais de secours lui-même.
        node: [u8; 32],
        addr: String,
        key: [u8; 32],
    },
    /// (v143) Hôte d'une province d'un pays (ou de son relais de secours) : entre chez le
    /// relais du pays avec le jeton du relais du monde (voir `proto::child_token`).
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
    TotpBegin,
    TotpConfirm(u32),
    TotpDisable(u32),
    Servers(u8),
    Logout,
    /// Ouvre une partie hébergée par ce jeu.
    Host {
        kind: u8,
        name: String,
        map: String,
        map_sig: [u8; 32],
        max_players: u16,
        /// (v330) Monde de construction : sa graine et sa clé de terrain (voir
        /// `Request::Host`) ; 0 en FPS.
        seed: u64,
        terrain: u64,
    },
    RoomStarted(bool),
    RoomPaused(bool),
    Join(u64),
    Leave,
    /// Cherche une partie FPS sur cette carte (ou en ouvre une).
    Search {
        map_sig: [u8; 32],
        map: String,
        players: u8,
    },
    Report(String),
    /// (v307) Signale la carte de la partie FPS en cours.
    MapReport,
    /// (v311) Le créateur supprime le monde de construction où il est (code de double
    /// authentification si son compte en a une).
    DeleteWorld(Option<u32>),
    Unreport(String),
    GarantAvailable(bool),
    /// Hôte d'une partie FPS : demande des passeurs pour ses joueurs (groupés par équipe).
    FastWant(Vec<Vec<u64>>),
    /// Sert (ou non) de passeur aux parties des autres (`crate::fast`), sans s'annoncer
    /// garant. (`GarantAvailable` le fait aussi : qui peut être garant peut être passeur.)
    Passeur(bool),
    /// (v320) Silence complet du relais au-delà duquel on le tient pour perdu (lien par
    /// des passeurs : plus long).
    Patience(Duration),
    /// (v326) Nouveau monde de construction qui fait place en supprimant le monde `replace`
    /// (celui que dit `Event::WorldLimit`), avec le code de double authentification si le
    /// compte en a une.
    HostReplacing {
        name: String,
        map: String,
        map_sig: [u8; 32],
        replace: String,
        code: Option<u32>,
        seed: u64,
        terrain: u64,
    },
    /// (v325) Réveille le monde endormi `world` (son entrée dans la liste) : il garde son
    /// créateur, ses OP, ses bannis, sa liste blanche ; son contenu est la copie en ligne
    /// de ce jeu (`map_sig`), jamais une version solo.
    WakeWorld {
        name: String,
        map: String,
        map_sig: [u8; 32],
        world: String,
        /// (v330) Ce que la copie en sait (0 : rien ; le relais le garde).
        seed: u64,
        terrain: u64,
    },
    /// (v320) Passeur sans joueur (`minelib-relay --passeur`), à dire avant `Passeur(true)` :
    /// son adresse publique (un serveur la connaît, il n'a pas de box à interroger) et son
    /// port UDP (0 : n'importe lequel).
    PasseurAt {
        ip: Option<std::net::Ipv4Addr>,
        port: u16,
    },
    GarantReady,
    Conflict {
        player: u64,
        seq: u32,
        host_pos: [f32; 3],
        audit_pos: [f32; 3],
        proof: bool,
    },
    Vote {
        conflict: u64,
        auditor_right: bool,
    },
    Ban {
        pseudo: String,
        reason: String,
    },
    /// Garant arbitre : verdict de l'affaire.
    Verdict {
        case: u64,
        guilty: bool,
    },
    /// OP du serveur de construction : `proto::SERVER_…`.
    ServerAdmin {
        action: u8,
        pseudo: String,
        reason: String,
    },
    MatchResult(Vec<u8>),
    /// Garant vérificateur : triche prouvée en rejouant (voir `Request::CheatFound`).
    CheatFound {
        pseudo: String,
        kind: u8,
        detail: String,
    },
    Leaderboard {
        friends: bool,
    },
    /// (v315) La suite du classement mondial, à partir du rang `from` (compté de zéro).
    LeaderboardMore {
        from: u32,
    },
    Friend {
        pseudo: String,
        add: bool,
    },
    /// (v231) Invite un ami dans le monde de construction où l'on est.
    Invite(String),
    /// (v231) Statut vu par ses amis : invisible ou non.
    Presence(bool),
    /// (v231) Refuse la demande d'ami de ce joueur.
    FriendDecline(String),
    /// (v242) Réponse à l'invitation de `from` (`proto::INVITE_ACCEPT`, `INVITE_DECLINE`,
    /// `INVITE_IGNORE`).
    InviteReply {
        from: String,
        reply: u8,
    },
    /// (v240) Défi pour une connexion par clé d'accès (nouvelle connexion au relais) :
    /// `Event::PasskeyChallenge`.
    PasskeyChallenge,
    /// (v240) Clé d'accès signée (sur la connexion du défi) : `Event::PasskeyOk`.
    PasskeyLogin {
        pseudo: String,
        id: Vec<u8>,
        authenticator_data: Vec<u8>,
        signature: Vec<u8>,
    },
    /// (v240) Compte connecté : nouvelle clé d'accès, ses clés, en retirer une
    /// (`Event::Passkeys`).
    PasskeyAdd {
        id: Vec<u8>,
        authenticator_data: Vec<u8>,
        vault: Vec<u8>,
        name: String,
    },
    PasskeyList,
    PasskeyRemove(Vec<u8>),
    /// Lobby (`proto::PARTY_*`).
    Party {
        action: u8,
        pseudo: String,
    },
    /// Envoie son skin (PNG) au relais.
    SkinPut(Vec<u8>),
    /// Demande le skin d'un joueur.
    SkinGet(String),
    Maps,
    /// Envoie une carte au relais (s'il ne l'a pas déjà).
    UploadMap {
        sig: [u8; 32],
        name: String,
        bytes: Vec<u8>,
        /// Carte de partie FPS en ligne (proposée à tous).
        fps: bool,
    },
    DownloadMap([u8; 32]),
    /// (v340) Certifie une carte FPS (ou lui retire sa certification) : compte
    /// `proto::CERTIFIER` seulement. Réponse : `Event::MapCertified`, ou `Event::Error`.
    MapCertify {
        sig: [u8; 32],
        on: bool,
    },
    Caps(crate::proto::GarantCaps),
    /// (v244) Foudre de Zeus légendaire du serveur de construction (`Request::Zeus`) : ce
    /// qu'en dit le relais arrive par `Event::Zeus`.
    Zeus {
        seq: u32,
        act: u8,
        chunk: [i32; 2],
        seed: u32,
        charges: u8,
    },
}

#[derive(Debug, Clone)]
pub enum Event {
    /// Premier contact avec ce relais : sa clé, à retenir.
    Pinned([u8; 32]),
    /// Étape en cours (affichage).
    Progress(&'static str),
    /// Coffre chiffré du compte, à garder sur ce PC (réinscription sur un autre relais).
    AccountBackup {
        pseudo: String,
        vault: Vec<u8>,
    },
    /// (v344) Le compte vient d'être copié sur ce relais (il ne l'avait pas) avec le coffre
    /// gardé sur ce PC : sans sa double authentification, qui ne se copie pas.
    AccountCopied {
        pseudo: String,
        relay: String,
    },
    LoggedIn {
        pseudo: String,
        fingerprint: String,
        totp: bool,
    },
    /// Cet appareil est de confiance pour ce compte (la 2FA ne sera plus demandée) : jeton
    /// à garder.
    DeviceTrusted {
        pseudo: String,
        token: [u8; 32],
    },
    /// Le compte demande un code de double authentification.
    NeedTotp,
    TotpSecret {
        secret: Vec<u8>,
        uri: String,
    },
    TotpOn,
    TotpOff,
    Servers(Vec<ServerEntry>),
    LoggedOut,
    Error(String),
    /// Relais injoignable (réseau, relais éteint) : pas une erreur du joueur, on peut
    /// réessayer.
    Offline(String),
    /// Session ouverte : file pour envoyer des données de jeu sans attendre
    /// (`transport::push` d'un `Request::ToHost`, `ToPeer` ou `Broadcast`).
    Link(Sender<Inbound>),
    /// Relais joint : son adresse et sa clé (pour une deuxième connexion, de garant).
    Relay {
        addr: String,
        key: [u8; 32],
    },
    Hosted(u64),
    /// (v325) Clé du monde de construction où l'on vient d'entrer (avant `Hosted` ou
    /// `Joined`).
    World(String),
    /// (v325) Le relais n'a pas ouvert (ou pas réveillé) ce monde de construction : pourquoi.
    HostRefused(String),
    /// (v326) Ce nouveau monde ne s'ouvre qu'en supprimant le plus ancien monde endormi du
    /// joueur (`world`, nommé `name`), qu'il confirme (`Cmd::HostReplacing`) ; `totp` : avec
    /// un code de double authentification.
    WorldLimit {
        world: String,
        name: String,
        totp: bool,
    },
    Joined {
        room: u64,
        host: String,
        map: String,
        map_sig: [u8; 32],
    },
    Left,
    Reported(String),
    Unreported(String),
    BanDone(String),
    /// Garant arbitre : une affaire à juger (les pièces suivent).
    Arbitrate {
        case: u64,
        map_sig: [u8; 32],
        map: String,
    },
    Evidence {
        case: u64,
        part: u16,
        of: u16,
        data: Vec<u8>,
    },
    /// Réponse d'une commande d'OP (texte à afficher).
    ServerAdmin(String),
    /// (v344) Réponse à `SERVER_RESPAWN` : un placement (`BuildSpawn`) suit, ou non (point
    /// d'apparition du monde : tout de suite). Rien au tchat.
    RespawnReply {
        placed: bool,
    },
    /// Exclu du serveur (banni), avec la raison.
    Kicked(String),
    /// (v311) Suppression du monde : le compte a la double authentification, il faut un
    /// code (ou celui donné est faux).
    DeleteWorldCode,
    /// Tué par un OP du serveur (`/kill`).
    Slain(String),
    /// Mode de jeu imposé par un OP (`by`), ou par défaut du serveur (`by` vide).
    SetGamemode {
        mode: String,
        by: String,
    },
    /// Position qu'on avait sur ce serveur de construction (à l'arrivée). (v318) En double
    /// précision, sur la carte bouclée.
    BuildSpawn {
        pos: [f64; 3],
        yaw: f32,
        pitch: f32,
    },
    /// Heure du serveur de construction.
    SetTime {
        hours: f32,
        fixed: bool,
    },
    /// (v229) Météo du serveur de construction (voir `Response::SetWeather`).
    SetWeather {
        kind: u8,
        left: f32,
        fixed: bool,
    },
    /// (v328) La graine du monde du serveur de construction : verrouillée, et ce que ce
    /// joueur peut en faire (voir `Response::SeedLock`). (v330) `seed` : la graine, à qui
    /// peut la voir (0 sinon, ou si le relais ne la connaît pas).
    SeedLock {
        locked: bool,
        allowed: bool,
        seed: u64,
    },
    /// (v330) Clé de terrain du monde de construction où l'on entre (avant `Hosted` ou
    /// `Joined`) : le relais la donne, elle n'est pas dans la carte. 0 : il ne la connaît pas
    /// (ou une partie FPS : la carte la porte).
    WorldTerrain(u64),
    /// (v244) Foudre de Zeus légendaire du serveur (voir `Response::Zeus`).
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
    /// Réponse du relais à des modifications en masse (WorldEdit) : `error` vide si publiées.
    BulkDone {
        id: u32,
        error: String,
    },
    Leaderboard {
        friends: bool,
        list: Vec<crate::proto::RankEntry>,
        me: Option<crate::proto::RankEntry>,
    },
    /// (v315) La suite du classement mondial demandée par `Cmd::LeaderboardMore`.
    LeaderboardMore {
        from: u32,
        list: Vec<crate::proto::RankEntry>,
    },
    /// Amis, et s'ils sont en ligne.
    Friends(Vec<(String, bool)>),
    /// (v231) Où sont ses amis (même ordre que `Friends`, juste après lui), et les demandes
    /// d'ami reçues. (v242) Et les invitations pas encore répondues.
    FriendInfo {
        places: Vec<crate::proto::FriendPlace>,
        requests: Vec<String>,
        invites: Vec<crate::proto::WorldInvite>,
    },
    /// (v242) `by` a refusé notre invitation dans le monde `name`.
    InviteDeclined {
        by: String,
        name: String,
    },
    /// (v243) Partie FPS du serveur de construction ouverte (menée par `host`) ou fermée.
    /// (v322) `origin` : l'origine (en chunks) du jeu de son hôte.
    BuildMatch {
        host: String,
        open: bool,
        origin: [i32; 2],
    },
    /// (v243) Hôte de la partie FPS d'un serveur de construction : `peer` (`pseudo`) y entre
    /// ou en sort.
    MatchPeer {
        peer: u64,
        pseudo: String,
        joined: bool,
    },
    /// (v231) `from` vient de nous ajouter en ami (`mutual` : on l'avait ajouté ; sinon,
    /// c'est une demande).
    FriendAdded {
        from: String,
        mutual: bool,
    },
    /// (v231) Invitation de `from` dans son monde de construction `room` (`name`).
    Invited {
        from: String,
        room: u64,
        name: String,
    },
    /// (v231) Invitation envoyée à ce joueur.
    InviteSent(String),
    /// (v240) Défi à faire signer par une clé d'accès.
    PasskeyChallenge([u8; 32]),
    /// (v240) Clé d'accès vérifiée par le relais : son coffre (les clés du compte), et un
    /// jeton d'appareil de confiance.
    PasskeyOk {
        pseudo: String,
        vault: Vec<u8>,
        device: [u8; 32],
    },
    /// (v240) Clés d'accès du compte.
    Passkeys(Vec<crate::proto::PasskeyInfo>),
    PartyInvite(String),
    Skin {
        pseudo: String,
        png: Vec<u8>,
    },
    SkinChanged(String),
    Party {
        id: u64,
        members: Vec<String>,
    },
    PartyGo {
        room: u64,
        map: String,
        map_sig: [u8; 32],
    },
    Maps(Vec<crate::proto::MapEntry>),
    /// Transfert de carte : (signature, fraction faite, envoi ou réception).
    MapProgress([u8; 32], f32, bool),
    MapUploaded([u8; 32]),
    /// (v340) Le relais a pris la certification demandée (`Cmd::MapCertify`).
    MapCertified {
        sig: [u8; 32],
        on: bool,
    },
    MapDownloaded {
        sig: [u8; 32],
        bytes: Vec<u8>,
    },
    MapFailed([u8; 32], String),
    Zones {
        gained: Vec<[i32; 2]>,
        lost: Vec<[i32; 2]>,
    },
    FromZone {
        peer: u64,
        chunk: [i32; 2],
        data: Vec<u8>,
    },
    PeerJoined {
        peer: u64,
        pseudo: String,
        party: u64,
    },
    PeerLeft(u64),
    RoomClosed,
    Promoted {
        members: Vec<(u64, String)>,
        old_host: Option<u64>,
    },
    GarantTask {
        room: u64,
        map_sig: [u8; 32],
        map: String,
        members: Vec<(u64, String)>,
        standby: bool,
    },
    /// Garant extérieur d'un serveur de construction (voir `Response::BuildGarant`).
    BuildGarant(u64),
    /// Héberger le relais d'une région (voir `Response::ShardTask`).
    ShardTask {
        room: u64,
        region: [i32; 2],
        peer: Vec<u8>,
        peer_expires: u64,
        slot: u16,
        root: [u8; 32],
        province: Option<proto::ProvinceAt>,
    },
    /// Provinces (v138), à l'hôte d'un relais de province : `Response::ProvinceTask`,
    /// `ProvinceSeed`, `ProvinceLeased`, `ProvinceArchived`, `ProvinceStop` ; aux joueurs
    /// d'une province : `ProvinceMark`, `ProvinceConfirmed` (passés tels quels).
    Province(Response),
    /// Aux joueurs : provinces confiées à un relais de province, en vue.
    Provinces(Vec<proto::ProvinceInfo>),
    /// Hôte d'un relais de région : les relais prêts de sa région, et son époque si elle
    /// est souveraine.
    ShardPeers {
        region: [i32; 2],
        list: Vec<proto::ShardInfo>,
        sov: Option<proto::Sov>,
    },
    /// Hôte d'un relais de région : un lot d'une autre région à vérifier (surveillance
    /// mutuelle, voir `Response::Audit`).
    Audit {
        id: u64,
        room: u64,
        region: [i32; 2],
        epoch: u64,
        key: [u8; 32],
        root: [u8; 32],
        floor_t: u64,
        batch: crate::sovereign::Batch,
    },
    /// Hôte d'un relais de région : le monde `room` est servi ou en pause (à redire aux
    /// joueurs de ce relais).
    WorldBeat {
        room: u64,
        paused: bool,
    },
    /// Construction (v137) : voix d'un joueur, nommé par le relais qui la transmet.
    VoiceFrom {
        from: String,
        data: Vec<u8>,
    },
    /// Hôte principal d'une région souveraine : modifications signées par le relais du
    /// monde à la place de leurs auteurs.
    RootEdits {
        region: [i32; 2],
        edits: Vec<crate::sovereign::Edit>,
        certs: Vec<crate::sovereign::Cert>,
    },
    /// Héberger le relais de secours d'un monde (voir `Response::StandbyTask`).
    StandbyTask {
        room: u64,
        root: [u8; 32],
        info: crate::proto::StandbyInfo,
    },
    /// Hôte du relais de secours : ce qui change dans le monde (voir `Response::Replica`).
    Replica(Response),
    /// Où est le relais de secours du monde (aucun : pas encore).
    Standby(Option<crate::proto::StandbyAt>),
    /// Gardien d'un monde : battement du relais du monde (voir `Response::Beat`).
    Beat {
        t: u64,
        committee: crate::guard::Committee,
        standby: Option<crate::proto::StandbyAt>,
        paused: bool,
    },
    /// Être témoin d'un monde (voir `Response::WitnessTask`).
    WitnessTask {
        room: u64,
        root: [u8; 32],
        cert: crate::sovereign::Cert,
    },
    /// Liste des gardiens plus récente, du relais de secours (témoin).
    Committee(crate::guard::Committee),
    /// Le relais du monde s'arrête proprement (relais de secours : reprendre ; `sig` : cet
    /// arrêt signé par lui).
    Released {
        room: u64,
        sig: Vec<u8>,
    },
    /// Le monde est en pause (le relais du monde n'entend plus assez de gardiens), ou
    /// repart.
    Paused(bool),
    /// La connexion ouverte au relais vient d'être perdue (suivie de `LoggedOut` et d'une
    /// erreur).
    Lost,
    /// (v165) Aller-retour mesuré avec ce relais (ms) : le ping montré dans la liste des
    /// joueurs (Tab).
    Rtt(u16),
    /// (v146) `Doze` accepté : la connexion au relais du monde est fermée, la partie continue
    /// par les relais des provinces.
    Dozed {
        room: u64,
    },
    /// Le monde est repris par son relais de secours (le relais du monde est tombé) : la
    /// partie continue là, `host` en est le propriétaire.
    TakenOver {
        room: u64,
        host: String,
    },
    /// Clé du compte, une fois connecté (pour signer ses modifications dans une région
    /// souveraine, ou ce qu'on y publie comme hôte principal).
    Identity(ed25519_dalek::SigningKey),
    /// Relais des régions du monde.
    Shards(Vec<proto::ShardInfo>),
    /// Billet pour les relais de région.
    ShardTicket {
        room: u64,
        pseudo: String,
        expires: u64,
        sig: Vec<u8>,
        cert: Option<crate::sovereign::Cert>,
        root: [u8; 32],
    },
    BackupIs(u64),
    Judge {
        conflict: u64,
        seq: u32,
        host_pos: [f32; 3],
        audit_pos: [f32; 3],
    },
    Voice {
        from: u64,
        data: Vec<u8>,
    },
    HostChanged(String),
    FromPeer {
        peer: u64,
        data: Vec<u8>,
    },
    FromHost(Vec<u8>),
    /// Voie rapide de la partie FPS en cours (`crate::fast`) : le bout par lequel envoyer
    /// (rien : voie fermée). Ce qui en arrive est remis comme le reste, par `FromHost` et
    /// `FromPeer`.
    Fast(Option<crate::fast::Endpoint>),
    /// (v333) Voie directe des voisins d'un monde de construction (`crate::fast::Near`) :
    /// de quoi envoyer nos positions rapides à nos plus proches voisins par TURN (rien :
    /// plus de route). Ce qui en arrive vient par `NearPos`.
    Near(Option<crate::fast::Near>),
    /// (v333) Une position venue directement d'un voisin (`peer` : son numéro sur ce
    /// relais).
    NearPos {
        peer: u64,
        data: Vec<u8>,
    },
    /// Garant vérificateur : copie d'une demande d'un joueur à l'hôte.
    Witness {
        peer: u64,
        data: Vec<u8>,
    },
}

pub struct Client {
    tx: Sender<Cmd>,
    rx: Receiver<Event>,
}

impl Client {
    /// Fil de fond pour le relais `addr` ; `pinned` : clé retenue au premier contact.
    pub fn spawn(addr: String, pinned: Option<[u8; 32]>) -> Self {
        Self::spawn_with(addr, pinned, None)
    }

    /// Comme `spawn`, mais l'adresse du relais (et sa clé) est trouvée par `resolve`, dans
    /// le fil du relais, au premier besoin (recherche dans le DHT, tunnel…).
    pub fn spawn_resolved(resolve: Resolver) -> Self {
        Self::spawn_with(String::new(), None, Some(resolve))
    }

    /// Deuxième connexion du compte, qui ne sert que de garant indépendant pendant que le
    /// joueur joue sa propre partie (même relais, même clé).
    pub fn spawn_garant(addr: String, pinned: [u8; 32]) -> Self {
        Self::spawn_inner(addr, Some(pinned), None, true)
    }

    /// Comme `spawn_garant`, l'adresse trouvée par `resolve` (relais d'un monde repris,
    /// joint par un tunnel iroh).
    pub fn spawn_garant_resolved(resolve: Resolver) -> Self {
        Self::spawn_inner(String::new(), None, Some(resolve), true)
    }

    /// (v320) Comme `spawn_resolved`, pour un lien avec le relais par des passeurs : un
    /// silence est attendu `SILENCE_PASSEURS` avant de tenir le relais pour perdu.
    pub fn spawn_by_passeurs(resolve: Resolver) -> Self {
        let c = Self::spawn_inner(String::new(), None, Some(resolve), false);
        c.send(Cmd::Patience(SILENCE_PASSEURS));
        c
    }

    fn spawn_with(addr: String, pinned: Option<[u8; 32]>, resolve: Option<Resolver>) -> Self {
        Self::spawn_inner(addr, pinned, resolve, false)
    }

    fn spawn_inner(
        addr: String,
        pinned: Option<[u8; 32]>,
        resolve: Option<Resolver>,
        garant: bool,
    ) -> Self {
        let (tx, cmd_rx) = channel::<Cmd>();
        let (ev_tx, rx) = channel::<Event>();
        std::thread::Builder::new()
            .name("relais".into())
            .spawn(move || {
                let mut w = Worker::new(addr, pinned, ev_tx);
                w.resolve = resolve;
                w.garant = garant;
                w.run(cmd_rx)
            })
            .expect("fil du relais");
        Self { tx, rx }
    }

    pub fn send(&self, cmd: Cmd) {
        let _ = self.tx.send(cmd);
    }

    pub fn poll(&self) -> Option<Event> {
        self.rx.try_recv().ok()
    }
}

/// Marque d'une erreur de connexion au relais (pas d'une réponse du relais).
#[derive(Debug)]
struct Offline;

impl std::fmt::Display for Offline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("pas de connexion au relais")
    }
}

/// Demande au relais `addr` (clé `key`) si `pseudo` y a un compte : sa clé publique.
pub fn whois(addr: &str, key: [u8; 32], pseudo: &str) -> Result<Option<[u8; 32]>> {
    let (mut chan, got) = Channel::connect(addr, Duration::from_secs(4))?;
    if got != key {
        return Err(anyhow!("clé du relais inattendue"));
    }
    chan.send(&Request::Hello {
        protocol: proto::PROTOCOL,
        client: format!("relais minelib {}", env!("CARGO_PKG_VERSION")),
    })?;
    match chan.recv::<Response>()? {
        Response::Hello { .. } => {}
        r => return Err(anyhow!("réponse inattendue : {r:?}")),
    }
    chan.send(&Request::Whois {
        pseudo: pseudo.to_string(),
    })?;
    match chan.recv::<Response>()? {
        Response::Whois { pubkey } => Ok(pubkey),
        r => Err(anyhow!("réponse inattendue : {r:?}")),
    }
}

/// Demande au relais `addr` (clé `key`) ses parties ouvertes (voir
/// `Request::PeerServers`).
pub fn peer_servers(addr: &str, key: [u8; 32], kind: u8) -> Result<Vec<proto::ServerEntry>> {
    let (mut chan, got) = Channel::connect(addr, Duration::from_secs(4))?;
    if got != key {
        return Err(anyhow!("clé du relais inattendue"));
    }
    chan.send(&Request::Hello {
        protocol: proto::PROTOCOL,
        client: format!("relais minelib {}", env!("CARGO_PKG_VERSION")),
    })?;
    match chan.recv::<Response>()? {
        Response::Hello { .. } => {}
        r => return Err(anyhow!("réponse inattendue : {r:?}")),
    }
    chan.send(&Request::PeerServers { kind })?;
    match chan.recv::<Response>()? {
        Response::Servers { list } => Ok(list),
        r => Err(anyhow!("réponse inattendue : {r:?}")),
    }
}

/// (v314) Le relais est-il muet ? `quiet` : temps depuis son dernier message ; `pinged` : un
/// ping est parti depuis ; `ping_age` : il y a combien de temps.
#[cfg(test)]
fn silent(quiet: Duration, pinged: bool, ping_age: Duration) -> bool {
    silent_after(SILENCE_MAX, quiet, pinged, ping_age)
}

/// (v320) Même règle, au bout de `limit` (lien par des passeurs : `SILENCE_PASSEURS`).
fn silent_after(limit: Duration, quiet: Duration, pinged: bool, ping_age: Duration) -> bool {
    quiet > limit && pinged && ping_age > SILENCE_PING
}

/// (v320) Lien avec le relais par des passeurs : si ses deux chaînes se taisent à la fois
/// (deux passeurs perdus d'un coup), iroh le fait passer par ses serveurs au bout de 15 s.
/// Le jeu l'attend, plutôt que de tenir le relais pour perdu à 10 s (et de quitter le
/// monde où l'on est).
const SILENCE_PASSEURS: Duration = Duration::from_secs(20);

/// (v314) Sonde un relais : sa clé, et s'il parle le même protocole que ce jeu (il répond
/// à son bonjour). Le jeu s'en sert pour choisir son relais : après une mise à jour qui
/// change le protocole, un relais resté dans l'ancienne version (ou déjà passé à la
/// nouvelle) est écarté au lieu de laisser le joueur devant « version incompatible ».
pub fn probe(addr: &str, wait: Duration) -> Result<([u8; 32], bool)> {
    probe_as(addr, wait, proto::PROTOCOL)
}

/// (Essais : le bonjour d'un jeu d'un autre protocole.)
#[doc(hidden)]
pub fn probe_as(addr: &str, wait: Duration, protocol: u32) -> Result<([u8; 32], bool)> {
    let (mut chan, key) = Channel::connect(addr, wait)?;
    chan.send(&Request::Hello {
        protocol,
        client: format!(
            "minelib-sonde {} v{}",
            env!("CARGO_PKG_VERSION"),
            GAME_VERSION.load(std::sync::atomic::Ordering::Relaxed)
        ),
    })?;
    let same = matches!(
        chan.recv_timeout::<Response>(wait)?,
        Some(Response::Hello { .. })
    );
    Ok((key, same))
}

pub fn error_text(code: ErrorCode, message: &str) -> String {
    let base = match code {
        ErrorCode::Protocol => "Version du jeu incompatible avec le relais.",
        ErrorCode::BadPseudo => "Pseudo invalide : 3 à 16 lettres, chiffres ou _.",
        ErrorCode::BadPassword => "Mot de passe trop court (8 caractères minimum).",
        ErrorCode::PseudoTaken => "Ce pseudo est déjà pris.",
        ErrorCode::BadProofOfWork => "Inscription refusée (preuve de travail).",
        ErrorCode::WrongCredentials => "Pseudo ou mot de passe incorrect.",
        ErrorCode::WrongTotp => "Code de double authentification incorrect.",
        ErrorCode::TooManyAttempts => "Trop d'essais.",
        ErrorCode::NotLoggedIn => "Session expirée : reconnecte-toi.",
        ErrorCode::Banned => "Banni :",
        ErrorCode::Internal => "Erreur du relais.",
        ErrorCode::Refused => "",
    };
    if message.is_empty() || base.is_empty() {
        format!("{base}{message}")
    } else {
        format!("{base} {message}")
    }
}

/// Trouve l'adresse du relais (et sa clé à retenir).
pub type Resolver = Box<dyn FnMut() -> Result<(String, Option<[u8; 32]>)> + Send>;

struct Worker {
    resolve: Option<Resolver>,
    addr: String,
    pinned: Option<[u8; 32]>,
    out: Sender<Event>,
    chan: Option<Channel>,
    /// (v344) Requêtes restées sans réponse dans le délai (`request`), la plus ancienne
    /// d'abord, et depuis quand : le relais répond à chacune, dans l'ordre, même en retard.
    /// Ces réponses-là sont mises de côté (`late_reply`) : avant, chacune devenait la réponse
    /// de la requête suivante, et tout restait décalé (26 novembre, 02:58 : un /tp recevait
    /// le skin de « pnj4002 », la requête d'après la réponse du /tp).
    late: std::collections::VecDeque<Instant>,
    /// (v344) Un message illisible du relais a déjà été noté au journal (une fois suffit).
    unreadable_said: bool,
    pow_bits: u8,
    /// (v344) Nom du relais (son bonjour), pour le dire au joueur.
    relay_name: String,
    /// Pseudo de la session ouverte.
    pseudo: String,
    /// Clé du relais connecté (sel de l'empreinte matérielle).
    relay_pub: [u8; 32],
    /// Connexion de garant (deuxième connexion du compte pendant qu'il joue).
    garant: bool,
    /// (v320) Silence complet du relais au-delà duquel on le tient pour perdu.
    patience: Duration,
    /// (v320) Passeur sans joueur : adresse publique et port UDP (voir `Cmd::PasseurAt`).
    passeur_at: Option<(Option<std::net::Ipv4Addr>, u16)>,
    /// Voie rapide de la partie FPS en cours : sens de ce qu'on envoie, et le bout.
    fast: Option<(u8, crate::fast::Endpoint)>,
    /// Ce jeu sert de passeur aux parties des autres (tant qu'il s'annonce garant) : le
    /// passeur, les sondes du relais qu'il reçoit, et la réponse attendue de la box à qui
    /// l'ouverture de son port a été demandée.
    passeur: Option<Passeur>,
    /// (v304) Le relais connecté a des passeurs vérifiés (`Response::PasseursKnown`).
    relay_passeurs: bool,
    /// (v307) Le relais connecté reçoit les journaux réseau (`Response::NetLogWanted`),
    /// et dernier envoi.
    relay_logs: bool,
    logs_at: Instant,
    /// (v301) Dernière demande de tunnel (`crate::fast::tunnel_want`) déjà envoyée.
    tunnel_seen: u64,
    /// (v329) Dernières adresses TURN dites à ce relais (`crate::fast::turn_addrs`).
    turn_seen: u64,
    /// (v333) Voie directe des voisins donnée par ce relais ; ce qu'on lui a dit de notre
    /// lien (par TURN ou non), et quand on l'a regardé.
    near: Option<crate::fast::Near>,
    link_turn: Option<bool>,
    link_at: Instant,
}

/// Passeur tenu par une connexion au relais. Il est gardé quand le jeu cesse de s'annoncer
/// (même port, même redirection sur la box à la prochaine annonce) ; il ne porte alors rien.
struct Passeur {
    p: crate::fast::Passeur,
    probes: Receiver<u64>,
    /// Annoncé au relais en ce moment.
    on: bool,
    /// Réponse attendue de la box, puis l'adresse qu'elle a donnée.
    mapped: Option<Receiver<Option<std::net::SocketAddrV4>>>,
    external: Option<std::net::SocketAddrV4>,
    /// Dernière sonde du relais reçue (ou dernière annonce) : sans sonde depuis
    /// `PASSEUR_SILENCE`, le relais ne nous joint plus, on se réannonce.
    heard: Instant,
    /// (v307) Dernier relevé au journal, et ce qu'il avait porté et refusé jusque-là.
    logged: (Instant, u64, u64),
}

/// Le relais sonde ses passeurs toutes les 30 s : après ce temps sans aucune sonde, il ne
/// nous joint plus (adresse changée, port refermé par la box). On redemande le port à la box
/// et on se réannonce.
const PASSEUR_SILENCE: Duration = Duration::from_secs(75);
/// (v333) Le chemin de notre lien avec le relais (par TURN ou non) est regardé à ce rythme.
const LINK_EVERY: Duration = Duration::from_secs(2);

/// Demande à la box d'ouvrir `port`, dans un fil à part (elle peut mettre plusieurs
/// secondes à répondre).
fn ask_box(port: u16) -> Receiver<Option<std::net::SocketAddrV4>> {
    let (tx, rx) = channel();
    let _ = std::thread::Builder::new()
        .name("passeur (box)".into())
        .spawn(move || {
            let _ = tx.send(crate::fast::map_port(port));
        });
    rx
}

impl Worker {
    fn new(addr: String, pinned: Option<[u8; 32]>, out: Sender<Event>) -> Self {
        Self {
            resolve: None,
            addr,
            pinned,
            out,
            chan: None,
            late: std::collections::VecDeque::new(),
            unreadable_said: false,
            pow_bits: 20,
            relay_name: String::new(),
            pseudo: String::new(),
            relay_pub: [0; 32],
            garant: false,
            patience: SILENCE_MAX,
            passeur_at: None,
            fast: None,
            passeur: None,
            relay_passeurs: false,
            relay_logs: false,
            // (Le premier envoi n'attend pas.)
            logs_at: Instant::now()
                .checked_sub(Duration::from_secs(10))
                .unwrap_or_else(Instant::now),
            // (Les demandes d'avant cette connexion ne la regardent pas.)
            tunnel_seen: crate::fast::tunnel_wants(u64::MAX, "").1,
            turn_seen: 0,
            near: None,
            link_turn: None,
            link_at: Instant::now(),
        }
    }

    /// Voie rapide : routes données par le relais (aucune : voie fermée). Les messages
    /// qui en arrivent sont remis au jeu comme ceux du relais.
    fn set_fast(&mut self, dir: u8, ways: Vec<crate::fast::Way>) {
        use crate::fast::{Endpoint, TO_HOST};
        if ways.iter().all(|w| w.vias.is_empty() && w.turn.is_none()) {
            self.close_fast();
            return;
        }
        let end = match &self.fast {
            Some((d, e)) if *d == dir => e.clone(),
            _ => {
                self.close_fast();
                let out = self.out.clone();
                let made = Endpoint::start(
                    dir,
                    Box::new(move |peer, data| {
                        let _ = out.send(if dir == TO_HOST {
                            Event::FromHost(data)
                        } else {
                            Event::FromPeer { peer, data }
                        });
                    }),
                );
                match made {
                    Ok(e) => e,
                    Err(e) => {
                        log::warn!("voie rapide : {e}");
                        return;
                    }
                }
            }
        };
        let turned = ways.iter().filter(|w| w.turn.is_some()).count();
        log::info!(
            "voie rapide : {} route(s), {turned} avec un chemin par TURN",
            ways.len()
        );
        end.set_ways(ways);
        self.fast = Some((dir, end.clone()));
        self.emit(Event::Fast(Some(end)));
    }

    fn close_fast(&mut self) {
        if let Some((_, e)) = self.fast.take() {
            e.close();
            self.emit(Event::Fast(None));
        }
    }

    /// (v333) Route de la voie directe vers le voisin `peer` (rien : reprise).
    fn set_near(&mut self, peer: u64, way: Option<(crate::fast::Way, u8, bool)>) {
        if way.is_none() && self.near.is_none() {
            return;
        }
        if self.near.is_none() {
            let out = self.out.clone();
            match crate::fast::Near::start(Box::new(move |peer, data| {
                let _ = out.send(Event::NearPos { peer, data });
            })) {
                Ok(n) => {
                    self.near = Some(n.clone());
                    self.emit(Event::Near(Some(n)));
                }
                Err(e) => {
                    log::warn!("voie directe des voisins : {e}");
                    return;
                }
            }
        }
        let Some(n) = &self.near else { return };
        n.set(peer, way);
        if n.peers() == 0 {
            self.close_near();
        }
    }

    fn close_near(&mut self) {
        if let Some(n) = self.near.take() {
            n.close();
            self.emit(Event::Near(None));
        }
    }

    /// Ce jeu s'annonce (ou non) comme passeur : son port UDP est dit au relais tout de
    /// suite, puis l'adresse que la box lui donne si elle ouvre le port (demandé dans un
    /// fil à part : elle peut mettre plusieurs secondes à répondre).
    fn set_passeur(&mut self, on: bool) -> anyhow::Result<()> {
        if !on {
            if let Some(ps) = self.passeur.as_mut().filter(|ps| ps.on) {
                ps.on = false;
                ps.p.clear();
                self.oneway(&Request::PasseurOff)?;
            }
            return Ok(());
        }
        if self.passeur.as_ref().is_some_and(|ps| ps.on) {
            return Ok(());
        }
        if self.passeur.is_none() {
            let (tx, probes) = channel();
            // (v320) Passeur sans joueur : le port voulu, et son adresse publique, connue
            // (serveur) : pas de box à interroger.
            let (ip, port) = self.passeur_at.unwrap_or((None, 0));
            let started = if port != 0 {
                crate::fast::Passeur::start(port, tx)
            } else {
                crate::fast::Passeur::start_any(tx)
            };
            let p = match started {
                Ok(p) => p,
                Err(e) => {
                    log::warn!("passeur : {e}");
                    return Ok(());
                }
            };
            let fixed = ip.map(|ip| std::net::SocketAddrV4::new(ip, p.port()));
            crate::fast::set_own_passeur(p.port(), fixed);
            let mapped = if fixed.is_some() {
                None
            } else {
                Some(ask_box(p.port()))
            };
            self.passeur = Some(Passeur {
                p,
                probes,
                on: false,
                mapped,
                external: fixed,
                heard: Instant::now(),
                logged: (Instant::now(), 0, 0),
            });
        }
        let Some(ps) = self.passeur.as_mut() else {
            return Ok(());
        };
        ps.on = true;
        ps.heard = Instant::now();
        let (port, external) = (ps.p.port(), ps.external);
        self.oneway(&Request::PasseurOffer {
            port,
            external: None,
        })?;
        // (Déjà ouvert par la box lors d'une annonce précédente.)
        if let Some(a) = external {
            self.oneway(&Request::PasseurOffer {
                port,
                external: Some((a.ip().octets(), a.port())),
            })?;
        }
        Ok(())
    }

    /// (v301) Tunnels iroh : les routes que le nœud du jeu demande partent à ce relais
    /// (seul celui du monde y répond, s'il a des passeurs).
    fn tend_tunnels(&mut self) {
        if self.chan.is_none() {
            self.relay_passeurs = false;
            self.relay_logs = false;
            // (v333) Ses routes de voie directe ne valaient que pour cette connexion.
            self.close_near();
            self.link_turn = None;
        }
        // (v333) Notre lien avec ce relais passe-t-il par TURN ? Dit à chaque connexion et
        // quand cela change (regardé toutes les deux secondes). Relais joint sans tunnel de
        // ce jeu (réseau local, cette machine) : rien n'est dit, le relais compte que non.
        if self.chan.is_some() && !self.pseudo.is_empty() && self.link_at.elapsed() >= LINK_EVERY {
            self.link_at = Instant::now();
            if let Some(on) = crate::fast::link_on_turn(&self.addr) {
                if self.link_turn != Some(on) && self.oneway(&Request::LinkTurn { on }).is_ok() {
                    self.link_turn = Some(on);
                }
            }
        }
        // (v307) Journal réseau partagé : quelques lignes toutes les 10 s, au relais qui
        // les demande.
        if self.relay_logs
            && !self.pseudo.is_empty()
            && self.logs_at.elapsed() >= Duration::from_secs(10)
        {
            self.logs_at = Instant::now();
            let lines = crate::netlog::take(20);
            if !lines.is_empty() {
                let _ = self.oneway(&Request::NetLog { lines });
            }
        }
        if self.relay_passeurs {
            crate::fast::note_passeurs();
        }
        let (nodes, last) = crate::fast::tunnel_wants(self.tunnel_seen, &self.pseudo);
        self.tunnel_seen = last;
        if self.pseudo.is_empty() {
            return;
        }
        let v4 = |a: std::net::SocketAddrV4| (a.ip().octets(), a.port());
        for ask in nodes {
            let req = match ask {
                crate::fast::TunnelAsk::Want(node, turn) => Request::TunnelWant {
                    node,
                    turn: turn.map(v4),
                },
                crate::fast::TunnelAsk::Bad(route) => Request::TunnelBad { route },
            };
            if self.oneway(&req).is_err() {
                return;
            }
        }
        // (v329) Nos adresses chez un serveur TURN : quand elles changent, et à chaque
        // connexion (le relais les oublie avec elle).
        if self.chan.is_none() {
            self.turn_seen = 0;
        } else if let Some((version, all)) = crate::fast::turn_addrs(self.turn_seen) {
            for (node, a) in all {
                let req = Request::TurnAt {
                    node,
                    turn: a.map(v4),
                };
                if self.oneway(&req).is_err() {
                    return;
                }
            }
            self.turn_seen = version;
        }
    }

    /// Entretien du passeur : la box a répondu, une sonde du relais est arrivée.
    fn tend_passeur(&mut self) {
        let Some(ps) = self.passeur.as_mut() else {
            return;
        };
        // (v307) Une ligne par minute quand il a servi, ou quand on lui a envoyé des
        // paquets qu'il a refusés (route inconnue, sceau faux, débit dépassé) : un port
        // arrosé se voit là.
        if ps.logged.0.elapsed() >= Duration::from_secs(60) {
            let (carried, refused) = (ps.p.carried(), ps.p.refused());
            let (dc, dr) = (carried - ps.logged.1, refused - ps.logged.2);
            ps.logged = (Instant::now(), carried, refused);
            if dc > 0 || dr > 0 {
                log::info!(
                    "passeur (60 s) : {dc} paquets portés, {dr} refusés, {} routes",
                    ps.p.routes()
                );
            }
        }
        let mut offers = Vec::new();
        if let Some(rx) = &ps.mapped {
            match rx.try_recv() {
                Ok(ext) => {
                    ps.mapped = None;
                    if let Some(a) = ext {
                        log::info!("passeur : la box a ouvert le port {}", a.port());
                        ps.external = Some(a);
                        crate::fast::set_own_passeur(ps.p.port(), Some(a));
                        if ps.on {
                            offers.push(Request::PasseurOffer {
                                port: ps.p.port(),
                                external: Some((a.ip().octets(), a.port())),
                            });
                        }
                    }
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => ps.mapped = None,
            }
        }
        while let Ok(nonce) = ps.probes.try_recv() {
            ps.heard = Instant::now();
            crate::fast::note_probed();
            if ps.on {
                offers.push(Request::PasseurEcho { nonce });
            }
        }
        // Plus aucune sonde : le relais ne nous joint plus. On se réannonce, et on
        // redemande à la box où elle en est.
        if ps.on && ps.heard.elapsed() >= PASSEUR_SILENCE {
            ps.heard = Instant::now();
            let port = ps.p.port();
            offers.push(Request::PasseurOffer {
                port,
                external: None,
            });
            if let Some(a) = ps.external {
                offers.push(Request::PasseurOffer {
                    port,
                    external: Some((a.ip().octets(), a.port())),
                });
            }
            if ps.mapped.is_none() {
                ps.mapped = Some(ask_box(port));
            }
        }
        if let Some(c) = self.chan.as_mut() {
            for r in &offers {
                let _ = c.send(r);
            }
        }
    }

    fn emit(&self, e: Event) {
        let _ = self.out.send(e);
    }

    fn run(mut self, cmds: Receiver<Cmd>) {
        let mut last_ping = Instant::now();
        // Ping en cours de mesure (envoyé à cet instant).
        let mut ping_sent: Option<Instant> = None;
        // (v314) Dernier message reçu du relais (voir `SILENCE_MAX`).
        let mut heard = Instant::now();
        loop {
            // Commandes du jeu (sans attendre si une connexion est ouverte).
            let cmd = if self.chan.is_some() {
                match cmds.try_recv() {
                    Ok(c) => Some(c),
                    Err(TryRecvError::Empty) => None,
                    Err(TryRecvError::Disconnected) => return,
                }
            } else {
                match cmds.recv_timeout(Duration::from_millis(100)) {
                    Ok(c) => Some(c),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            };
            if let Some(cmd) = cmd {
                if let Err(e) = self.handle(cmd) {
                    log::warn!("relais : {e:#}");
                    if e.downcast_ref::<Offline>().is_some() {
                        // Le message utile est la cause (« Relais injoignable… »).
                        self.emit(Event::Offline(format!("{:#}", e.root_cause())));
                    } else {
                        self.emit(Event::Error(format!("{e:#}")));
                    }
                }
                // (Une commande peut attendre une réponse, ou calculer longtemps.)
                heard = Instant::now();
                continue;
            }
            // Sans connexion au relais : plus de voie rapide, plus de routes à porter.
            if self.chan.is_none() {
                self.close_fast();
                self.passeur = None;
                heard = Instant::now();
            }
            self.tend_passeur();
            self.tend_tunnels();
            // Messages poussés par le relais, envois déposés par le jeu.
            let Some(chan) = self.chan.as_mut() else {
                continue;
            };
            let got = match chan.recv_any::<Response>(Duration::from_millis(4)) {
                Ok(Some(Incoming::Msg(r))) => {
                    heard = Instant::now();
                    Ok(Some(r))
                }
                Ok(Some(Incoming::Push(b))) => chan.send_raw(&b).map(|_| None),
                // (v314) Relais muet : perdu (voir `SILENCE_MAX`).
                Ok(None)
                    if silent_after(
                        self.patience,
                        heard.elapsed(),
                        last_ping > heard,
                        last_ping.elapsed(),
                    ) =>
                {
                    Err(anyhow!("le relais ne répond plus"))
                }
                Ok(None) => Ok(None),
                // (v344) Un message poussé illisible (relais d'une autre version du jeu) :
                // laissé, la connexion reste. Avant, elle était tenue pour perdue.
                Err(e) if unreadable(&e) => {
                    if !self.unreadable_said {
                        self.unreadable_said = true;
                        log::warn!("relais : {UNREADABLE} ({e:#})");
                    }
                    Ok(None)
                }
                Err(e) => Err(e),
            };
            let mut measured = None;
            let got = got.and_then(|r| {
                // Pong : aller-retour mesuré, dit au relais (et au jeu).
                if matches!(r, Some(Response::Pong)) {
                    if let Some(t) = ping_sent.take() {
                        let ms = t.elapsed().as_millis().min(u16::MAX as u128) as u16;
                        measured = Some(ms);
                        chan.send(&Request::Rtt { ms })?;
                    }
                    return Ok(None);
                }
                // Pong perdu (répondu pendant une requête) : on remesure.
                if ping_sent.is_some_and(|t| t.elapsed() > Duration::from_secs(5)) {
                    ping_sent = None;
                }
                let every = if ping_sent.is_none() {
                    RTT_EVERY
                } else {
                    KEEPALIVE
                };
                if last_ping.elapsed() > every {
                    last_ping = Instant::now();
                    ping_sent = Some(last_ping);
                    chan.send(&Request::Ping)?;
                }
                Ok(r)
            });
            if let Some(ms) = measured {
                self.emit(Event::Rtt(ms));
            }
            match got {
                // (v344) Pas un message poussé : la réponse en retard d'une requête d'avant.
                Ok(Some(r)) if !r.is_push() && self.late_pending() => self.late_reply(r),
                Ok(Some(r)) => self.on_push(r),
                Ok(None) => {}
                Err(_) => {
                    self.chan = None;
                    self.emit(Event::Lost);
                    self.emit(Event::LoggedOut);
                    self.emit(Event::Error("Connexion au relais perdue.".into()));
                }
            }
        }
    }

    /// Message poussé par le relais (parties, données de jeu).
    fn on_push(&mut self, r: Response) {
        // Mesure de l'aller-retour par le relais : réponse immédiate.
        if let Response::Probe { nonce } = r {
            if let Some(c) = self.chan.as_mut() {
                let _ = c.send(&Request::Echo { nonce });
            }
            return;
        }
        if matches!(r, Response::RoomClosed | Response::Kicked { .. }) {
            self.close_fast();
        }
        let r = match r {
            Response::FastGuest {
                route,
                key,
                passeurs,
                turn,
            } => {
                let way = crate::proto::FastWay {
                    peer: 0,
                    route,
                    key,
                    passeurs,
                    turn,
                };
                self.set_fast(crate::fast::TO_HOST, vec![way.way()]);
                return;
            }
            Response::FastHost { ways } => {
                let ways = ways.iter().map(|w| w.way()).collect();
                self.set_fast(crate::fast::TO_GUEST, ways);
                return;
            }
            Response::NearWay {
                peer,
                route,
                key,
                dir,
                send,
                turn,
            } => {
                let way = turn.map(|t| {
                    let w = crate::proto::FastWay {
                        peer,
                        route,
                        key,
                        passeurs: Vec::new(),
                        turn: Some(t),
                    };
                    (w.way(), dir, send)
                });
                self.set_near(peer, way);
                return;
            }
            Response::TunnelWay {
                node,
                route,
                host,
                own,
                passeurs,
                turn,
            } => {
                let vias = passeurs
                    .iter()
                    .map(|h| crate::fast::Via {
                        addr: std::net::SocketAddr::from((h.addr, h.port)),
                        seal: h.seal,
                    })
                    .collect();
                crate::fast::tunnel_given(
                    crate::fast::TunnelRoute {
                        node,
                        route,
                        host,
                        vias,
                        turn: turn.map(|h| h.via()),
                    },
                    own,
                );
                return;
            }
            Response::NetLogWanted { on } => {
                self.relay_logs = on;
                return;
            }
            Response::PasseursKnown { n } => {
                self.relay_passeurs = n > 0;
                if n > 0 {
                    crate::fast::note_passeurs();
                }
                return;
            }
            Response::PasseurLink {
                lot,
                route,
                seal,
                dir,
                addr,
                port,
            } => {
                if let Some(ps) = self.passeur.as_ref().filter(|ps| ps.on) {
                    let from = std::net::SocketAddr::from((addr, port));
                    ps.p.set_link(lot, route, seal, dir, from);
                }
                return;
            }
            Response::PasseurTask { lot, routes } => {
                if let Some(ps) = self.passeur.as_ref().filter(|ps| ps.on) {
                    ps.p.set_lot(lot, &routes);
                    log::info!("passeur : {} route(s) portée(s)", ps.p.routes());
                }
                return;
            }
            r => r,
        };
        let e = match r {
            Response::PeerJoined {
                peer,
                pseudo,
                party,
            } => Event::PeerJoined {
                peer,
                pseudo,
                party,
            },
            Response::PartyInvite { from } => Event::PartyInvite(from),
            Response::Invited { from, room, name } => Event::Invited { from, room, name },
            Response::InviteDeclined { by, name } => Event::InviteDeclined { by, name },
            Response::BuildMatch { host, open, origin } => Event::BuildMatch { host, open, origin },
            Response::MatchPeer {
                peer,
                pseudo,
                joined,
            } => Event::MatchPeer {
                peer,
                pseudo,
                joined,
            },
            Response::FriendAdded { from, mutual } => Event::FriendAdded { from, mutual },
            Response::SkinChanged { pseudo } => Event::SkinChanged(pseudo),
            Response::Party { id, members } => Event::Party { id, members },
            Response::PartyGo { room, map, map_sig } => Event::PartyGo { room, map, map_sig },
            Response::PeerLeft { peer } => Event::PeerLeft(peer),
            Response::RoomClosed => Event::RoomClosed,
            Response::Promoted { members, old_host } => Event::Promoted { members, old_host },
            Response::GarantTask {
                room,
                map_sig,
                map,
                members,
                standby,
            } => Event::GarantTask {
                room,
                map_sig,
                map,
                members,
                standby,
            },
            Response::BackupIs { peer } => Event::BackupIs(peer),
            Response::Judge {
                conflict,
                seq,
                host_pos,
                audit_pos,
            } => Event::Judge {
                conflict,
                seq,
                host_pos,
                audit_pos,
            },
            Response::Voice { from, data } => Event::Voice { from, data },
            Response::HostChanged { host } => Event::HostChanged(host),
            Response::Kicked { reason } => Event::Kicked(reason),
            Response::Slain { by } => Event::Slain(by),
            Response::SetGamemode { mode, by } => Event::SetGamemode { mode, by },
            Response::BulkDone { id, error } => Event::BulkDone { id, error },
            Response::SetTime { hours, fixed } => Event::SetTime { hours, fixed },
            Response::SetWeather { kind, left, fixed } => Event::SetWeather { kind, left, fixed },
            Response::SeedLock {
                locked,
                allowed,
                seed,
            } => Event::SeedLock {
                locked,
                allowed,
                seed,
            },
            Response::Zeus {
                seq,
                act,
                why,
                holder,
                charges,
                sky_hits,
                expires,
                permit,
            } => Event::Zeus {
                seq,
                act,
                why,
                holder,
                charges,
                sky_hits,
                expires,
                permit,
            },
            Response::BuildSpawn { pos, yaw, pitch } => Event::BuildSpawn { pos, yaw, pitch },
            Response::Arbitrate { case, map_sig, map } => Event::Arbitrate { case, map_sig, map },
            Response::Evidence {
                case,
                part,
                of,
                data,
            } => Event::Evidence {
                case,
                part,
                of,
                data,
            },
            Response::FromPeer { peer, data } => Event::FromPeer { peer, data },
            Response::FromHost { data } => Event::FromHost(data),
            Response::Witness { peer, data } => Event::Witness { peer, data },
            Response::Zones { gained, lost } => Event::Zones { gained, lost },
            Response::BuildGarant { room } => Event::BuildGarant(room),
            Response::ShardTask {
                room,
                region,
                peer,
                peer_expires,
                slot,
                root,
                province,
            } => Event::ShardTask {
                room,
                region,
                peer,
                peer_expires,
                slot,
                root,
                province,
            },
            r @ (Response::ProvinceTask { .. }
            | Response::ProvinceCopy { .. }
            | Response::ProvincePromote { .. }
            | Response::ProvinceSeed { .. }
            | Response::ProvinceLeased { .. }
            | Response::ProvinceArchived { .. }
            | Response::ProvinceMark { .. }
            | Response::ProvinceConfirmed { .. }
            | Response::ProvinceStop { .. }
            | Response::ProvinceSettings { .. }
            | Response::ProvinceBagLent { .. }
            | Response::ProvinceBagRecall { .. }
            | Response::ProvinceBulk { .. }
            | Response::ProvinceChildren { .. }
            | Response::ProvinceParent { .. }
            | Response::ProvinceNeighbours { .. }
            | Response::ProvinceSlay { .. }
            | Response::GarantCall { .. }) => Event::Province(r),
            Response::Provinces { list } => Event::Provinces(list),
            Response::ShardPeers { region, list, sov } => Event::ShardPeers { region, list, sov },
            Response::RootEdits {
                region,
                edits,
                certs,
            } => Event::RootEdits {
                region,
                edits,
                certs,
            },
            Response::StandbyTask { room, root, info } => Event::StandbyTask { room, root, info },
            r @ (Response::Replica { .. }
            | Response::ReplicaBag { .. }
            | Response::ReplicaMode { .. }) => Event::Replica(r),
            Response::Standby { at } => Event::Standby(at),
            Response::Beat {
                t,
                committee,
                standby,
                paused,
            } => Event::Beat {
                t,
                committee,
                standby,
                paused,
            },
            Response::WitnessTask { room, root, cert } => Event::WitnessTask { room, root, cert },
            Response::Committee { committee } => Event::Committee(committee),
            Response::Released { room, sig } => Event::Released { room, sig },
            Response::Paused { paused } => Event::Paused(paused),
            Response::Audit {
                id,
                room,
                region,
                epoch,
                key,
                root,
                floor_t,
                batch,
            } => Event::Audit {
                id,
                room,
                region,
                epoch,
                key,
                root,
                floor_t,
                batch,
            },
            Response::WorldBeat { room, paused } => Event::WorldBeat { room, paused },
            Response::VoiceFrom { from, data } => Event::VoiceFrom { from, data },
            Response::Shards { list } => Event::Shards(list),
            Response::ShardTicket {
                room,
                pseudo,
                expires,
                sig,
                cert,
                root,
            } => Event::ShardTicket {
                room,
                pseudo,
                expires,
                sig,
                cert,
                root,
            },
            Response::FromZone { peer, chunk, data } => Event::FromZone { peer, chunk, data },
            // Banni pendant la session : déconnecté.
            Response::Error { code, message } => {
                if code == ErrorCode::Banned {
                    self.chan = None;
                    self.emit(Event::LoggedOut);
                }
                Event::Error(error_text(code, &message))
            }
            _ => return,
        };
        self.emit(e);
    }

    /// Nouvelle connexion au relais (vérifie sa clé). Relais trouvé automatiquement : en cas
    /// d'échec on en cherche un autre, sans rien montrer au joueur (trois essais).
    fn connect(&mut self) -> Result<()> {
        let tries = if self.resolve.is_some() { 3 } else { 1 };
        let mut last = None;
        for n in 1..=tries {
            match self.connect_once() {
                Ok(()) => return Ok(()),
                Err(e) => {
                    if n < tries {
                        log::info!("connexion au relais : {e:#} — nouvel essai ({n}/{tries})");
                    }
                    last = Some(e);
                }
            }
        }
        Err(last.expect("au moins un essai").context(Offline))
    }

    fn connect_once(&mut self) -> Result<()> {
        if self.addr.is_empty() && self.resolve.is_some() {
            {
                let _ = self
                    .out
                    .send(Event::Progress("Recherche d'un relais en ligne…"));
                let Some(r) = &mut self.resolve else {
                    unreachable!()
                };
                let (addr, key) = r()?;
                self.addr = addr;
                self.pinned = key;
            }
        }
        self.emit(Event::Progress("Connexion au relais…"));
        let (mut chan, key) = match Channel::connect(&self.addr, TIMEOUT) {
            Ok(c) => c,
            Err(e) => {
                let addr = self.addr.clone();
                // Relais trouvé automatiquement : on en cherchera un autre la prochaine fois.
                if self.resolve.is_some() {
                    self.addr.clear();
                    self.pinned = None;
                }
                return Err(anyhow!("Relais injoignable ({addr}) : {e}"));
            }
        };
        match self.pinned {
            Some(p) if p != key => {
                return Err(anyhow!(
                    "Le relais {} a changé de clé : connexion refusée (faux relais possible). \
                     Si c'est voulu, efface relay_key dans options.txt.",
                    self.addr
                ))
            }
            None => {
                self.pinned = Some(key);
                self.emit(Event::Pinned(key));
            }
            _ => {}
        }
        self.relay_pub = key;
        let name = if self.garant {
            "minelib-garant"
        } else {
            "minelib"
        };
        chan.send(&Request::Hello {
            protocol: proto::PROTOCOL,
            client: format!(
                "{name} {} v{}",
                env!("CARGO_PKG_VERSION"),
                GAME_VERSION.load(std::sync::atomic::Ordering::Relaxed)
            ),
        })?;
        self.emit(Event::Relay {
            addr: self.addr.clone(),
            key,
        });
        match chan.recv::<Response>()? {
            Response::Hello {
                pow_bits, relay, ..
            } => {
                self.pow_bits = pow_bits;
                self.relay_name = relay.chars().take(48).collect();
            }
            Response::Error { code, message } => {
                // (v314) Relais d'une autre version du jeu, trouvé automatiquement : on
                // en cherchera un autre au prochain essai (la recherche écarte ceux qui ne
                // parlent pas le même protocole). Avant, le jeu y revenait sans fin.
                if matches!(code, ErrorCode::Protocol) && self.resolve.is_some() {
                    self.addr.clear();
                    self.pinned = None;
                }
                return Err(anyhow!(error_text(code, &message)));
            }
            r => return Err(anyhow!("réponse inattendue : {r:?}")),
        }
        self.chan = Some(chan);
        self.late.clear();
        Ok(())
    }

    /// Envoie une requête et attend sa réponse ; les messages poussés reçus entre-temps
    /// sont transmis au jeu.
    fn request(&mut self, req: &Request) -> Result<Response> {
        self.chan
            .as_mut()
            .ok_or_else(|| anyhow!("pas connecté"))?
            .send(req)?;
        let end = Instant::now() + TIMEOUT;
        loop {
            let left = end.saturating_duration_since(Instant::now());
            let chan = self.chan.as_mut().ok_or_else(|| anyhow!("pas connecté"))?;
            let got = chan.recv_any::<Response>(left).map_err(|e| {
                // (v344) Avant : « Found a bool that wasn't 0 or 1 » (26 novembre, 02:38).
                if unreadable(&e) {
                    anyhow!("{UNREADABLE} ({e:#})")
                } else {
                    e
                }
            })?;
            match got {
                None => {
                    self.late.push_back(Instant::now());
                    return Err(anyhow!("le relais ne répond pas"));
                }
                Some(Incoming::Push(b)) => chan.send_raw(&b)?,
                Some(Incoming::Msg(r)) if r.is_push() => self.on_push(r),
                Some(Incoming::Msg(Response::Pong)) if !matches!(req, Request::Ping) => {}
                // (v344) La réponse d'une requête d'avant, arrivée après son délai : pas la
                // nôtre (elle vient après).
                Some(Incoming::Msg(r)) if self.late_pending() => self.late_reply(r),
                Some(Incoming::Msg(r)) => return Ok(r),
            }
        }
    }

    /// (v344) Une réponse en retard est-elle attendue ? (Au-delà de `LATE_KEEP`, on ne
    /// l'attend plus : le relais l'aurait donnée.)
    fn late_pending(&mut self) -> bool {
        while self.late.front().is_some_and(|t| t.elapsed() > LATE_KEEP) {
            self.late.pop_front();
        }
        !self.late.is_empty()
    }

    /// (v344) Réponse arrivée après le délai de sa requête : ce qu'elle dit d'elle-même arrive
    /// quand même au jeu (le skin de qui, le texte d'une commande d'OP, un refus) ; le reste
    /// est laissé.
    fn late_reply(&mut self, r: Response) {
        self.late.pop_front();
        match r {
            Response::Skin { pseudo, png } => self.emit(Event::Skin { pseudo, png }),
            Response::ServerAdmin { text }
                if !text.is_empty() && text != proto::RESPAWN_FOLLOWS =>
            {
                self.emit(Event::ServerAdmin(text))
            }
            Response::Error { code, message } => {
                self.emit(Event::Error(error_text(code, &message)))
            }
            _ => log::debug!("relais : réponse arrivée après son délai, laissée"),
        }
    }

    fn handle(&mut self, cmd: Cmd) -> Result<()> {
        // On change de partie : la voie rapide de la précédente se ferme.
        if matches!(
            cmd,
            Cmd::Host { .. } | Cmd::Join(_) | Cmd::Search { .. } | Cmd::Leave
        ) {
            self.close_fast();
        }
        match cmd {
            Cmd::Register { pseudo, password } => {
                if !proto::valid_pseudo(&pseudo) {
                    return Err(anyhow!(error_text(ErrorCode::BadPseudo, "")));
                }
                if !proto::valid_password(&password) {
                    return Err(anyhow!(error_text(ErrorCode::BadPassword, "")));
                }
                self.chan = None;
                self.connect()?;
                self.emit(Event::Progress("Création des clés du compte…"));
                let keys = crypto::derive(&pseudo, &password)?;
                let id = crypto::new_identity();
                let pubkey = id.verifying_key().to_bytes();
                self.emit(Event::Progress("Preuve de travail…"));
                let pow = crypto::pow_solve(&pseudo, &pubkey, self.pow_bits);
                let vault = crypto::seal_identity(&keys, &id);
                match self.request(&Request::Register {
                    pseudo: pseudo.clone(),
                    pubkey,
                    auth: keys.auth,
                    vault,
                    pow,
                })? {
                    Response::Registered { .. } => {}
                    Response::Error { code, message } => {
                        return Err(anyhow!(error_text(code, &message)))
                    }
                    r => return Err(anyhow!("réponse inattendue : {r:?}")),
                }
                self.login(&pseudo, &keys, None, None)
            }
            Cmd::GarantLogin { cert, key } => {
                self.chan = None;
                self.connect()?;
                let challenge = match self.request(&Request::GarantLogin { cert: cert.clone() })? {
                    Response::TakeoverChallenge { challenge, .. } => challenge,
                    r => return self.unexpected(r),
                };
                use ed25519_dalek::Signer;
                let signature = key.sign(&challenge).to_bytes().to_vec();
                match self.request(&Request::GarantProve { signature })? {
                    Response::Welcome { pseudo, .. } => {
                        self.pseudo = pseudo.clone();
                        self.emit(Event::Identity(key));
                        if let Some(c) = &self.chan {
                            self.emit(Event::Link(c.pusher()));
                        }
                        self.emit(Event::LoggedIn {
                            pseudo,
                            fingerprint: String::new(),
                            totp: false,
                        });
                        Ok(())
                    }
                    r => self.unexpected(r),
                }
            }
            Cmd::WitnessLogin { cert, key } => {
                self.chan = None;
                self.connect()?;
                let challenge = match self.request(&Request::WitnessLogin { cert })? {
                    Response::TakeoverChallenge { challenge, .. } => challenge,
                    r => return self.unexpected(r),
                };
                use ed25519_dalek::Signer;
                let signature = key.sign(&challenge).to_bytes().to_vec();
                match self.request(&Request::WitnessProve { signature })? {
                    Response::Done => {
                        if let Some(c) = &self.chan {
                            self.emit(Event::Link(c.pusher()));
                        }
                        Ok(())
                    }
                    r => self.unexpected(r),
                }
            }
            Cmd::Doze { room } => match self.request(&Request::Doze { room })? {
                Response::Dozed { room } => {
                    // Plus rien à se dire : la connexion se ferme (sans `Lost`).
                    self.chan = None;
                    self.emit(Event::Dozed { room });
                    Ok(())
                }
                r => self.unexpected(r),
            },
            Cmd::Wake { room, cert, key } => {
                self.chan = None;
                self.connect()?;
                let challenge = match self.request(&Request::Wake {
                    room,
                    cert: cert.clone(),
                })? {
                    Response::TakeoverChallenge { challenge, .. } => challenge,
                    r => return self.unexpected(r),
                };
                self.prove_takeover(cert, key, challenge)
            }
            Cmd::Takeover { cert, key, expect } => {
                self.chan = None;
                self.connect()?;
                let (challenge, proof) =
                    match self.request(&Request::Takeover { cert: cert.clone() })? {
                        Response::TakeoverChallenge { challenge, proof } => (challenge, proof),
                        r => return self.unexpected(r),
                    };
                // Un relais de secours qui ne prouve pas que les gardiens lui ont confié le
                // monde (ou que le relais du monde s'est arrêté) : on n'y entre pas.
                if let Some(at) = &expect {
                    if !proof.is_some_and(|p| p.check(&at.root, at.room, &at.pseudo)) {
                        self.chan = None;
                        self.emit(Event::Error(
                            "Le relais de secours ne prouve pas qu'il a repris le monde.".into(),
                        ));
                        return Ok(());
                    }
                }
                self.prove_takeover(cert, key, challenge)
            }
            Cmd::RegionHost {
                room,
                region,
                slot,
                pseudo,
                signer,
                token,
            } => {
                self.chan = None;
                self.connect()?;
                match self.request(&Request::RegionHost {
                    room,
                    region,
                    slot,
                    pseudo,
                    signer,
                    token,
                })? {
                    Response::Welcome { pseudo, .. } => {
                        self.pseudo = pseudo.clone();
                        if let Some(c) = &self.chan {
                            self.emit(Event::Link(c.pusher()));
                        }
                        self.emit(Event::LoggedIn {
                            pseudo,
                            fingerprint: String::new(),
                            totp: false,
                        });
                        Ok(())
                    }
                    r => self.unexpected(r),
                }
            }
            Cmd::ProvinceFollow {
                room,
                province,
                epoch,
                pseudo,
                signer,
                token,
                node,
                addr,
                key,
            } => {
                self.chan = None;
                self.connect()?;
                match self.request(&Request::ProvinceFollow {
                    room,
                    province,
                    epoch,
                    pseudo,
                    signer,
                    token,
                    node,
                    addr,
                    key,
                })? {
                    Response::Welcome { pseudo, .. } => {
                        self.pseudo = pseudo;
                        if let Some(c) = &self.chan {
                            self.emit(Event::Link(c.pusher()));
                        }
                        Ok(())
                    }
                    r => self.unexpected(r),
                }
            }
            Cmd::ProvinceChild {
                room,
                province,
                size,
                epoch,
                pseudo,
                signer,
                token,
                standby,
            } => {
                self.chan = None;
                self.connect()?;
                match self.request(&Request::ProvinceChild {
                    room,
                    province,
                    size,
                    epoch,
                    pseudo,
                    signer,
                    token,
                    standby,
                })? {
                    Response::Welcome { pseudo, .. } => {
                        self.pseudo = pseudo;
                        if let Some(c) = &self.chan {
                            self.emit(Event::Link(c.pusher()));
                        }
                        Ok(())
                    }
                    r => self.unexpected(r),
                }
            }
            Cmd::ShardLogin {
                room,
                pseudo,
                expires,
                sig,
                cert,
            } => {
                self.chan = None;
                self.connect()?;
                match self.request(&Request::ShardLogin {
                    room,
                    pseudo,
                    expires,
                    sig,
                    cert,
                })? {
                    Response::Welcome { pseudo, .. } => {
                        self.pseudo = pseudo.clone();
                        if let Some(c) = &self.chan {
                            self.emit(Event::Link(c.pusher()));
                        }
                        self.emit(Event::LoggedIn {
                            pseudo,
                            fingerprint: String::new(),
                            totp: false,
                        });
                        Ok(())
                    }
                    r => self.unexpected(r),
                }
            }
            Cmd::Login {
                pseudo,
                password,
                totp,
                backup,
                device,
            } => {
                self.chan = None;
                // Les clés du compte se calculent (une seconde environ) pendant qu'on
                // cherche le relais et qu'on s'y connecte.
                let (p, w) = (pseudo.clone(), password.clone());
                // (v240) Clés du compte gardées à la place du mot de passe (clé d'accès) :
                // rien à calculer.
                let deriving = std::thread::spawn(move || match crypto::keys_from_password(&w) {
                    Some(k) => Ok(k),
                    None => crypto::derive(&p, &w),
                });
                self.connect()?;
                self.emit(Event::Progress("Vérification du mot de passe…"));
                let keys = deriving
                    .join()
                    .map_err(|_| anyhow!("calcul des clés du compte interrompu"))??;
                let wrong = error_text(ErrorCode::WrongCredentials, "");
                match self.login(&pseudo, &keys, totp, device) {
                    // (v344) Sans copie du compte sur ce PC : peut-être un compte qu'un autre
                    // relais garde (celui où il a été créé). Avant, seulement « Pseudo ou mot
                    // de passe incorrect » (26 novembre, l'autre PC de Viendax).
                    Err(e) if e.to_string() == wrong && backup.is_none() => Err(anyhow!(
                        "{wrong} Ou ce relais (« {} ») ne connaît pas ton compte : il est gardé \
                         par le relais où tu l'as créé. Connecte-toi une fois ici depuis un PC \
                         où le compte est déjà ouvert : il y sera copié.",
                        self.relay_name
                    )),
                    Err(e) if e.to_string() == wrong && backup.is_some() => {
                        // Compte inconnu de ce relais (nouveau relais) : réinscription avec
                        // le coffre gardé, s'il s'ouvre avec ce mot de passe.
                        let vault = backup.unwrap_or_default();
                        let id = crypto::open_identity(&keys, &vault).map_err(|_| e)?;
                        let pubkey = id.verifying_key().to_bytes();
                        self.emit(Event::Progress("Compte transféré sur ce relais…"));
                        let pow = crypto::pow_solve(&pseudo, &pubkey, self.pow_bits);
                        match self.request(&Request::Register {
                            pseudo: pseudo.clone(),
                            pubkey,
                            auth: keys.auth,
                            vault,
                            pow,
                        })? {
                            Response::Registered { .. } => {}
                            r => return self.unexpected(r),
                        }
                        log::info!(
                            "compte copié sur le relais « {} » (il ne l'avait pas)",
                            self.relay_name
                        );
                        self.emit(Event::AccountCopied {
                            pseudo: pseudo.clone(),
                            relay: self.relay_name.clone(),
                        });
                        self.login(&pseudo, &keys, totp, device)
                    }
                    r => r,
                }
            }
            Cmd::TotpBegin => match self.request(&Request::TotpBegin)? {
                Response::TotpSecret { secret } => {
                    self.emit(Event::TotpSecret {
                        uri: crate::totp::uri(&self.pseudo, &secret),
                        secret,
                    });
                    Ok(())
                }
                r => self.unexpected(r),
            },
            Cmd::TotpConfirm(code) => match self.request(&Request::TotpConfirm { code })? {
                Response::TotpOn => {
                    self.emit(Event::TotpOn);
                    Ok(())
                }
                r => self.unexpected(r),
            },
            Cmd::TotpDisable(code) => match self.request(&Request::TotpDisable { code })? {
                Response::TotpOff => {
                    self.emit(Event::TotpOff);
                    Ok(())
                }
                r => self.unexpected(r),
            },
            Cmd::Servers(kind) => match self.request(&Request::Servers { kind })? {
                Response::Servers { list } => {
                    self.emit(Event::Servers(list));
                    Ok(())
                }
                r => self.unexpected(r),
            },
            Cmd::Host {
                kind,
                name,
                map,
                map_sig,
                max_players,
                seed,
                terrain,
            } => match self.request(&Request::Host {
                kind,
                name,
                map,
                map_sig,
                max_players,
                wake: String::new(),
                replace: String::new(),
                code: None,
                seed,
                terrain,
            })? {
                Response::Hosted {
                    room,
                    world,
                    terrain,
                } => {
                    self.emit_world(world);
                    self.emit(Event::WorldTerrain(terrain));
                    self.emit(Event::Hosted(room));
                    Ok(())
                }
                Response::WorldLimit { world, name, totp } => {
                    self.emit(Event::WorldLimit { world, name, totp });
                    Ok(())
                }
                Response::Error { code, message } if kind == 1 => {
                    self.emit(Event::HostRefused(error_text(code, &message)));
                    Ok(())
                }
                r => self.unexpected(r),
            },
            Cmd::HostReplacing {
                name,
                map,
                map_sig,
                replace,
                code,
                seed,
                terrain,
            } => match self.request(&Request::Host {
                kind: 1,
                name,
                map,
                map_sig,
                max_players: 0,
                wake: String::new(),
                replace,
                code,
                seed,
                terrain,
            })? {
                Response::Hosted {
                    room,
                    world,
                    terrain,
                } => {
                    self.emit_world(world);
                    self.emit(Event::WorldTerrain(terrain));
                    self.emit(Event::Hosted(room));
                    Ok(())
                }
                Response::WorldLimit { world, name, totp } => {
                    self.emit(Event::WorldLimit { world, name, totp });
                    Ok(())
                }
                Response::Error { code, message } => {
                    self.emit(Event::HostRefused(error_text(code, &message)));
                    Ok(())
                }
                r => self.unexpected(r),
            },
            Cmd::WakeWorld {
                name,
                map,
                map_sig,
                world,
                seed,
                terrain,
            } => match self.request(&Request::Host {
                kind: 1,
                name,
                map,
                map_sig,
                max_players: 0,
                wake: world,
                replace: String::new(),
                code: None,
                seed,
                terrain,
            })? {
                Response::Hosted {
                    room,
                    world,
                    terrain,
                } => {
                    self.emit_world(world);
                    self.emit(Event::WorldTerrain(terrain));
                    self.emit(Event::Hosted(room));
                    Ok(())
                }
                Response::Error { code, message } => {
                    self.emit(Event::HostRefused(error_text(code, &message)));
                    Ok(())
                }
                r => self.unexpected(r),
            },
            Cmd::RoomStarted(started) => match self.request(&Request::RoomStarted { started })? {
                Response::Done => Ok(()),
                r => self.unexpected(r),
            },
            Cmd::RoomPaused(paused) => match self.request(&Request::RoomPaused { paused })? {
                Response::Done => Ok(()),
                r => self.unexpected(r),
            },
            Cmd::Join(room) => match self.request(&Request::Join { room })? {
                Response::Joined {
                    room,
                    host,
                    map,
                    map_sig,
                    world,
                    terrain,
                } => {
                    self.emit_world(world);
                    self.emit(Event::WorldTerrain(terrain));
                    self.emit(Event::Joined {
                        room,
                        host,
                        map,
                        map_sig,
                    });
                    Ok(())
                }
                r => self.unexpected(r),
            },
            Cmd::Search {
                map_sig,
                map,
                players,
            } => {
                match self.request(&Request::Search {
                    map_sig,
                    map,
                    players,
                })? {
                    Response::Joined {
                        room,
                        host,
                        map,
                        map_sig,
                        world,
                        terrain,
                    } => {
                        self.emit_world(world);
                        self.emit(Event::WorldTerrain(terrain));
                        self.emit(Event::Joined {
                            room,
                            host,
                            map,
                            map_sig,
                        });
                        Ok(())
                    }
                    Response::Hosted {
                        room,
                        world,
                        terrain,
                    } => {
                        self.emit_world(world);
                        self.emit(Event::WorldTerrain(terrain));
                        self.emit(Event::Hosted(room));
                        Ok(())
                    }
                    r => self.unexpected(r),
                }
            }
            Cmd::MapReport => self.oneway(&Request::MapReport),
            // (v311) Accepté : le relais nous sort du monde (`Kicked` suit). Code demandé
            // ou faux : le jeu le (re)demande.
            Cmd::DeleteWorld(code) => match self.request(&Request::DeleteWorld { code })? {
                Response::Done => Ok(()),
                Response::Error {
                    code: ErrorCode::WrongTotp,
                    ..
                } => {
                    self.emit(Event::DeleteWorldCode);
                    Ok(())
                }
                r => self.unexpected(r),
            },
            Cmd::Report(pseudo) => match self.request(&Request::Report {
                pseudo: pseudo.clone(),
            })? {
                Response::Done => {
                    self.emit(Event::Reported(pseudo));
                    Ok(())
                }
                r => self.unexpected(r),
            },
            Cmd::Ban { pseudo, reason } => match self.request(&Request::Ban {
                pseudo: pseudo.clone(),
                reason,
            })? {
                Response::Done => {
                    self.emit(Event::BanDone(pseudo));
                    Ok(())
                }
                r => self.unexpected(r),
            },
            // (v344) Réapparition : la réponse ne s'affiche pas. Un relais d'avant ne connaît
            // pas l'action (il refuse, ou répond la liste des OP) : pas de placement.
            Cmd::ServerAdmin {
                action: proto::SERVER_RESPAWN,
                pseudo,
                reason,
            } => {
                let r = self.request(&Request::ServerAdmin {
                    action: proto::SERVER_RESPAWN,
                    pseudo,
                    reason,
                })?;
                let placed =
                    matches!(&r, Response::ServerAdmin { text } if text == proto::RESPAWN_FOLLOWS);
                self.emit(Event::RespawnReply { placed });
                Ok(())
            }
            Cmd::ServerAdmin {
                action,
                pseudo,
                reason,
            } => match self.request(&Request::ServerAdmin {
                action,
                pseudo,
                reason,
            })? {
                Response::ServerAdmin { text } => {
                    self.emit(Event::ServerAdmin(text));
                    Ok(())
                }
                r => self.unexpected(r),
            },
            Cmd::GarantAvailable(on) => {
                self.oneway(&Request::GarantAvailable { on })?;
                // Qui peut être garant peut être passeur (`crate::fast`), (v308) s'il l'a
                // accepté.
                self.set_passeur(on && crate::fast::passeur_allowed())
            }
            Cmd::FastWant(groups) => {
                self.oneway(&Request::FastWant { groups })?;
                Ok(())
            }
            Cmd::Passeur(on) => self.set_passeur(on),
            Cmd::Patience(d) => {
                self.patience = d;
                Ok(())
            }
            Cmd::PasseurAt { ip, port } => {
                self.passeur_at = Some((ip, port));
                Ok(())
            }
            Cmd::GarantReady => {
                self.oneway(&Request::GarantReady)?;
                Ok(())
            }
            Cmd::CheatFound {
                pseudo,
                kind,
                detail,
            } => {
                self.oneway(&Request::CheatFound {
                    pseudo,
                    kind,
                    detail,
                })?;
                Ok(())
            }
            Cmd::Conflict {
                player,
                seq,
                host_pos,
                audit_pos,
                proof,
            } => {
                self.oneway(&Request::Conflict {
                    player,
                    seq,
                    host_pos,
                    audit_pos,
                    proof,
                })?;
                Ok(())
            }
            Cmd::Verdict { case, guilty } => {
                self.oneway(&Request::Verdict { case, guilty })?;
                Ok(())
            }
            Cmd::Vote {
                conflict,
                auditor_right,
            } => {
                self.oneway(&Request::Vote {
                    conflict,
                    auditor_right,
                })?;
                Ok(())
            }
            Cmd::Unreport(pseudo) => match self.request(&Request::Unreport {
                pseudo: pseudo.clone(),
            })? {
                Response::Done => {
                    self.emit(Event::Unreported(pseudo));
                    Ok(())
                }
                r => self.unexpected(r),
            },
            Cmd::MatchResult(data) => match self.request(&Request::MatchResult { data })? {
                Response::Done => Ok(()),
                r => self.unexpected(r),
            },
            Cmd::Leaderboard { friends } => {
                match self.request(&Request::Leaderboard { friends })? {
                    Response::Leaderboard { list, me } => {
                        self.emit(Event::Leaderboard { friends, list, me });
                        Ok(())
                    }
                    r => self.unexpected(r),
                }
            }
            Cmd::LeaderboardMore { from } => {
                match self.request(&Request::LeaderboardFrom { from })? {
                    Response::Leaderboard { list, .. } => {
                        self.emit(Event::LeaderboardMore { from, list });
                        Ok(())
                    }
                    r => self.unexpected(r),
                }
            }
            Cmd::Friend { pseudo, add } => {
                let r = self.request(&Request::Friend { pseudo, add })?;
                self.friends_answer(r)
            }
            Cmd::FriendDecline(pseudo) => {
                let r = self.request(&Request::FriendDecline { pseudo })?;
                self.friends_answer(r)
            }
            Cmd::InviteReply { from, reply } => {
                let r = self.request(&Request::InviteReply { from, reply })?;
                self.friends_answer(r)
            }
            Cmd::PasskeyChallenge => {
                self.chan = None;
                self.connect()?;
                match self.request(&Request::PasskeyChallenge)? {
                    Response::PasskeyChallenge { challenge } => {
                        self.emit(Event::PasskeyChallenge(challenge));
                        Ok(())
                    }
                    r => self.unexpected(r),
                }
            }
            Cmd::PasskeyLogin {
                pseudo,
                id,
                authenticator_data,
                signature,
            } => {
                self.emit(Event::Progress("Vérification de la clé d'accès…"));
                let hw = crate::hw::fingerprints_for(&self.relay_pub);
                let r = self.request(&Request::PasskeyLogin {
                    pseudo: pseudo.clone(),
                    id,
                    authenticator_data,
                    signature,
                    hw,
                })?;
                match r {
                    Response::PasskeyOk { vault, device } => {
                        self.emit(Event::PasskeyOk {
                            pseudo,
                            vault,
                            device,
                        });
                        Ok(())
                    }
                    r => self.unexpected(r),
                }
            }
            Cmd::PasskeyAdd {
                id,
                authenticator_data,
                vault,
                name,
            } => {
                let r = self.request(&Request::PasskeyAdd {
                    id,
                    authenticator_data,
                    vault,
                    name,
                })?;
                self.passkeys_answer(r)
            }
            Cmd::PasskeyList => {
                let r = self.request(&Request::PasskeyList)?;
                self.passkeys_answer(r)
            }
            Cmd::PasskeyRemove(id) => {
                let r = self.request(&Request::PasskeyRemove { id })?;
                self.passkeys_answer(r)
            }
            Cmd::Invite(pseudo) => match self.request(&Request::Invite {
                pseudo: pseudo.clone(),
            })? {
                Response::Done => {
                    self.emit(Event::InviteSent(pseudo));
                    Ok(())
                }
                r => self.unexpected(r),
            },
            Cmd::Presence(hidden) => match self.request(&Request::Presence { hidden })? {
                Response::Done => Ok(()),
                r => self.unexpected(r),
            },
            Cmd::Zeus {
                seq,
                act,
                chunk,
                seed,
                charges,
            } => match self.request(&Request::Zeus {
                seq,
                act,
                chunk,
                seed,
                charges,
            })? {
                Response::Done => Ok(()),
                r => self.unexpected(r),
            },
            Cmd::SkinPut(png) => match self.request(&Request::SkinPut { png })? {
                Response::Done => Ok(()),
                r => self.unexpected(r),
            },
            Cmd::SkinGet(pseudo) => match self.request(&Request::SkinGet { pseudo })? {
                Response::Skin { pseudo, png } => {
                    self.emit(Event::Skin { pseudo, png });
                    Ok(())
                }
                r => self.unexpected(r),
            },
            Cmd::Party { action, pseudo } => {
                match self.request(&Request::Party { action, pseudo })? {
                    Response::Done => Ok(()),
                    r => self.unexpected(r),
                }
            }
            Cmd::Maps => match self.request(&Request::Maps)? {
                Response::Maps { list } => {
                    self.emit(Event::Maps(list));
                    Ok(())
                }
                r => self.unexpected(r),
            },
            Cmd::MapCertify { sig, on } => match self.request(&Request::MapCertify { sig, on })? {
                Response::Done => {
                    self.emit(Event::MapCertified { sig, on });
                    Ok(())
                }
                r => self.unexpected(r),
            },
            Cmd::UploadMap {
                sig,
                name,
                bytes,
                fps,
            } => {
                let total = bytes.len() as u64;
                let mut offset = 0u64;
                loop {
                    let end = (offset as usize + crate::proto::MAP_PART).min(bytes.len());
                    let data = bytes[offset as usize..end].to_vec();
                    match self.request(&Request::MapPut {
                        sig,
                        name: name.clone(),
                        offset,
                        total,
                        data,
                        fps,
                    })? {
                        Response::MapStored { complete: true, .. } => {
                            self.emit(Event::MapUploaded(sig));
                            return Ok(());
                        }
                        Response::MapStored { offset: o, .. } => {
                            if o <= offset && end == bytes.len() {
                                return Err(anyhow!("envoi de la carte refusé"));
                            }
                            offset = o;
                            self.emit(Event::MapProgress(
                                sig,
                                o as f32 / total.max(1) as f32,
                                true,
                            ));
                        }
                        Response::Error { code, message } => {
                            self.emit(Event::MapFailed(sig, error_text(code, &message)));
                            return Ok(());
                        }
                        r => return self.unexpected(r),
                    }
                }
            }
            Cmd::DownloadMap(sig) => {
                let mut bytes = Vec::new();
                loop {
                    match self.request(&Request::MapGet {
                        sig,
                        offset: bytes.len() as u64,
                    })? {
                        Response::MapPart {
                            total,
                            data,
                            offset,
                            ..
                        } if offset == bytes.len() as u64 => {
                            if data.is_empty() || total > crate::proto::MAP_MAX {
                                self.emit(Event::MapFailed(sig, "carte incomplète".into()));
                                return Ok(());
                            }
                            bytes.extend_from_slice(&data);
                            self.emit(Event::MapProgress(
                                sig,
                                bytes.len() as f32 / total.max(1) as f32,
                                false,
                            ));
                            if bytes.len() as u64 >= total {
                                if crate::game::signature(&bytes) != sig {
                                    self.emit(Event::MapFailed(sig, "carte altérée".into()));
                                } else {
                                    self.emit(Event::MapDownloaded { sig, bytes });
                                }
                                return Ok(());
                            }
                        }
                        Response::Error { code, message } => {
                            self.emit(Event::MapFailed(sig, error_text(code, &message)));
                            return Ok(());
                        }
                        r => return self.unexpected(r),
                    }
                }
            }
            Cmd::Caps(caps) => match self.request(&Request::Caps { caps })? {
                Response::Done => Ok(()),
                r => self.unexpected(r),
            },
            Cmd::Leave => match self.request(&Request::Leave)? {
                Response::Left => {
                    self.emit(Event::Left);
                    Ok(())
                }
                r => self.unexpected(r),
            },
            Cmd::Logout => {
                self.chan = None;
                self.emit(Event::LoggedOut);
                Ok(())
            }
        }
    }

    /// Envoi sans réponse attendue.
    /// (v325) Clé du monde de construction où l'on entre (rien en FPS).
    fn emit_world(&self, world: String) {
        if !world.is_empty() {
            self.emit(Event::World(world));
        }
    }

    fn oneway(&mut self, req: &Request) -> Result<()> {
        self.chan
            .as_mut()
            .ok_or_else(|| anyhow!("pas connecté"))?
            .send(req)
    }

    /// Reprise (ou retour après `Doze`) : signe le défi avec la clé du compte ; entré, la
    /// partie continue par cette connexion (`TakenOver`).
    fn prove_takeover(
        &mut self,
        cert: crate::sovereign::Cert,
        key: ed25519_dalek::SigningKey,
        challenge: [u8; 32],
    ) -> Result<()> {
        use ed25519_dalek::Signer;
        let signature = key.sign(&challenge).to_bytes().to_vec();
        match self.request(&Request::TakeoverProve { signature })? {
            Response::Joined {
                room,
                host,
                map,
                map_sig,
                world,
                terrain: _,
            } => {
                self.pseudo = cert.pseudo.clone();
                self.emit_world(world);
                self.emit(Event::Identity(key));
                if let Some(c) = &self.chan {
                    self.emit(Event::Link(c.pusher()));
                }
                self.emit(Event::LoggedIn {
                    pseudo: cert.pseudo,
                    fingerprint: String::new(),
                    totp: false,
                });
                let _ = (map, map_sig);
                self.emit(Event::TakenOver { room, host });
                Ok(())
            }
            r => self.unexpected(r),
        }
    }

    /// (v240) Clés d'accès du compte reçues.
    fn passkeys_answer(&mut self, r: Response) -> Result<()> {
        match r {
            Response::Passkeys { list } => {
                self.emit(Event::Passkeys(list));
                Ok(())
            }
            r => self.unexpected(r),
        }
    }

    /// Liste d'amis reçue : `Event::Friends`, puis `Event::FriendInfo` (où ils sont, demandes).
    fn friends_answer(&mut self, r: Response) -> Result<()> {
        match r {
            Response::Friends {
                list,
                online,
                places,
                requests,
                invites,
            } => {
                let both = list
                    .into_iter()
                    .enumerate()
                    .map(|(i, p)| (p, online.get(i).copied().unwrap_or(false)))
                    .collect();
                self.emit(Event::Friends(both));
                self.emit(Event::FriendInfo {
                    places,
                    requests,
                    invites,
                });
                Ok(())
            }
            r => self.unexpected(r),
        }
    }

    fn unexpected(&self, r: Response) -> Result<()> {
        match r {
            Response::Error { code, message } => Err(anyhow!(error_text(code, &message))),
            r => Err(anyhow!("réponse inattendue : {r:?}")),
        }
    }

    fn login(
        &mut self,
        pseudo: &str,
        keys: &crypto::Keys,
        totp: Option<u32>,
        device: Option<[u8; 32]>,
    ) -> Result<()> {
        let hw = crate::hw::fingerprints_for(&self.relay_pub);
        let r = self.request(&Request::Login {
            pseudo: pseudo.to_string(),
            auth: keys.auth,
            totp,
            hw,
            device,
        })?;
        let (vault, challenge) = match r {
            Response::Vault {
                vault,
                challenge,
                device,
            } => {
                if let Some(token) = device {
                    self.emit(Event::DeviceTrusted {
                        pseudo: pseudo.to_string(),
                        token,
                    });
                }
                (vault, challenge)
            }
            Response::NeedTotp => {
                self.emit(Event::NeedTotp);
                return Ok(());
            }
            r => return self.unexpected(r),
        };
        let id = crypto::open_identity(keys, &vault)?;
        let signature = id.sign(&challenge).to_bytes().to_vec();
        self.emit(Event::AccountBackup {
            pseudo: pseudo.to_string(),
            vault: vault.clone(),
        });
        match self.request(&Request::Prove { signature })? {
            Response::Welcome { pseudo, totp } => {
                self.pseudo = pseudo.clone();
                self.emit(Event::Identity(id.clone()));
                if let Some(c) = &self.chan {
                    self.emit(Event::Link(c.pusher()));
                }
                self.emit(Event::LoggedIn {
                    pseudo,
                    fingerprint: crypto::fingerprint(&id.verifying_key().to_bytes()),
                    totp,
                });
                Ok(())
            }
            r => self.unexpected(r),
        }
    }
}

#[cfg(test)]
mod late_tests {
    use super::*;

    /// (v344) Une requête restée sans réponse dans le délai : sa réponse, quand elle arrive
    /// enfin, n'est plus prise pour celle de la requête suivante (avant, tout restait
    /// décalé d'une réponse : 26 novembre, 02:58). Ce qu'elle dit d'elle-même arrive quand
    /// même au jeu.
    #[test]
    fn a_reply_that_comes_after_its_timeout_is_set_aside() {
        let (tx, rx) = channel();
        let mut w = Worker::new("essai".into(), None, tx);
        assert!(!w.late_pending());
        w.late.push_back(Instant::now());
        w.late.push_back(Instant::now());
        assert!(w.late_pending());
        w.late_reply(Response::Skin {
            pseudo: "pnj4002".into(),
            png: Vec::new(),
        });
        assert!(w.late_pending(), "une autre attendue");
        w.late_reply(Response::ServerAdmin {
            text: "Téléporté.".into(),
        });
        assert!(!w.late_pending());
        let got: Vec<Event> = rx.try_iter().collect();
        assert!(matches!(&got[0], Event::Skin { pseudo, .. } if pseudo == "pnj4002"));
        assert!(matches!(&got[1], Event::ServerAdmin(t) if t == "Téléporté."));
        // Trop vieille : plus attendue (la réponse suivante est bien celle de sa requête).
        w.late
            .push_back(Instant::now() - LATE_KEEP - Duration::from_secs(1));
        assert!(!w.late_pending());
        let src = include_str!("client.rs");
        let req = &src[src
            .find("    fn request(&mut self, req: &Request)")
            .unwrap()..];
        let req = &req[..req.find("\n    }\n").unwrap()];
        let timeout = req.find("self.late.push_back(Instant::now())").unwrap();
        assert!(timeout < req.find("le relais ne répond pas").unwrap());
        let late = req
            .find("if self.late_pending() => self.late_reply(r)")
            .unwrap();
        assert!(late < req.find("Some(Incoming::Msg(r)) => return Ok(r)").unwrap());
        assert!(src
            .contains("Ok(Some(r)) if !r.is_push() && self.late_pending() => self.late_reply(r)"));
        let connect = &src[src.find("    fn connect_once(&mut self)").unwrap()..];
        assert!(connect[..connect.find("\n    }\n").unwrap()].contains("self.late.clear()"));
    }
}

#[cfg(test)]
mod unreadable_tests {
    use super::*;

    /// (v344) « Found a bool that wasn't 0 or 1 » (26 novembre, 02:38) : la liste des cartes
    /// d'un relais d'une version d'avant, que ce jeu lisait avec un champ de plus. Le jeu dit
    /// maintenant ce que c'est ; et un message poussé illisible ne coupe plus la connexion.
    #[test]
    fn an_unreadable_relay_message_says_the_versions_differ() {
        let e: anyhow::Error = match postcard::from_bytes::<proto::Response>(&[0xff; 4]) {
            Err(e) => e.into(),
            Ok(_) => unreachable!(),
        };
        assert!(unreadable(&e));
        assert!(unreadable(&e.context("en lisant la réponse")));
        assert!(!unreadable(&anyhow!("connexion fermée")));
        assert!(UNREADABLE.contains("pas la même version"));
        let src = include_str!("client.rs");
        assert!(src.contains("Err(e) if unreadable(&e) => {"));
    }
}

#[cfg(test)]
mod silence_tests {
    use super::*;

    /// (v314) Un relais est tenu pour perdu après dix secondes sans rien recevoir, pourvu
    /// qu'un ping parti depuis soit resté trois secondes sans réponse : ni un relais
    /// simplement calme, ni un jeu qui sort de veille ne le perdent.
    #[test]
    fn a_relay_is_lost_after_ten_silent_seconds_and_an_unanswered_ping() {
        let s = Duration::from_secs;
        assert!(!silent(s(9), true, s(4)), "pas encore dix secondes");
        assert!(silent(s(11), true, s(4)));
        assert!(!silent(s(11), false, s(4)), "aucun ping depuis : sortie de veille");
        assert!(!silent(s(40), true, s(1)), "le ping vient de partir");
        assert!(silent(s(40), true, s(4)));
    }
}
