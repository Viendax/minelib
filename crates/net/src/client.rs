//! Côté jeu : un fil de fond parle au relais (connexion, inscription, 2FA, liste des
//! parties) pour que l'interface ne se fige jamais. Le jeu envoie des `Cmd` et lit des
//! `Event` à chaque image.

use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use ed25519_dalek::Signer;

use crate::crypto;
use crate::proto::{self, ErrorCode, Request, Response, ServerEntry};
use crate::transport::{Channel, Inbound, Incoming};

/// Délai pour joindre le relais et pour chaque réponse.
const TIMEOUT: Duration = Duration::from_secs(10);
/// Ping régulier pour garder la session ouverte.
const KEEPALIVE: Duration = Duration::from_secs(20);

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
    },
    RoomStarted(bool),
    Join(u64),
    Leave,
    /// Cherche une partie FPS sur cette carte (ou en ouvre une).
    Search {
        map_sig: [u8; 32],
        map: String,
        players: u8,
    },
    Report(String),
    Unreport(String),
    GarantAvailable(bool),
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
    Leaderboard {
        friends: bool,
    },
    Friend {
        pseudo: String,
        add: bool,
    },
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
    Caps(crate::proto::GarantCaps),
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
    /// Exclu du serveur (banni), avec la raison.
    Kicked(String),
    /// Tué par un OP du serveur (`/kill`).
    Slain(String),
    Leaderboard {
        friends: bool,
        list: Vec<crate::proto::RankEntry>,
        me: Option<crate::proto::RankEntry>,
    },
    /// Amis, et s'ils sont en ligne.
    Friends(Vec<(String, bool)>),
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
    pow_bits: u8,
    /// Pseudo de la session ouverte.
    pseudo: String,
    /// Clé du relais connecté (sel de l'empreinte matérielle).
    relay_pub: [u8; 32],
    /// Connexion de garant (deuxième connexion du compte pendant qu'il joue).
    garant: bool,
}

impl Worker {
    fn new(addr: String, pinned: Option<[u8; 32]>, out: Sender<Event>) -> Self {
        Self {
            resolve: None,
            addr,
            pinned,
            out,
            chan: None,
            pow_bits: 20,
            pseudo: String::new(),
            relay_pub: [0; 32],
            garant: false,
        }
    }

    fn emit(&self, e: Event) {
        let _ = self.out.send(e);
    }

    fn run(mut self, cmds: Receiver<Cmd>) {
        let mut last_ping = Instant::now();
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
                continue;
            }
            // Messages poussés par le relais, envois déposés par le jeu.
            let Some(chan) = self.chan.as_mut() else {
                continue;
            };
            let got = match chan.recv_any::<Response>(Duration::from_millis(4)) {
                Ok(Some(Incoming::Msg(r))) => Ok(Some(r)),
                Ok(Some(Incoming::Push(b))) => chan.send_raw(&b).map(|_| None),
                Ok(None) => Ok(None),
                Err(e) => Err(e),
            };
            let got = got.and_then(|r| {
                if last_ping.elapsed() > KEEPALIVE {
                    last_ping = Instant::now();
                    chan.send(&Request::Ping)?;
                }
                Ok(r)
            });
            match got {
                Ok(Some(r)) => self.on_push(r),
                Ok(None) => {}
                Err(_) => {
                    self.chan = None;
                    self.emit(Event::LoggedOut);
                    self.emit(Event::Error("Connexion au relais perdue.".into()));
                }
            }
        }
    }

    /// Message poussé par le relais (parties, données de jeu).
    fn on_push(&mut self, r: Response) {
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
            client: format!("{name} {}", env!("CARGO_PKG_VERSION")),
        })?;
        self.emit(Event::Relay {
            addr: self.addr.clone(),
            key,
        });
        match chan.recv::<Response>()? {
            Response::Hello { pow_bits, .. } => self.pow_bits = pow_bits,
            Response::Error { code, message } => return Err(anyhow!(error_text(code, &message))),
            r => return Err(anyhow!("réponse inattendue : {r:?}")),
        }
        self.chan = Some(chan);
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
            match chan.recv_any::<Response>(left)? {
                None => return Err(anyhow!("le relais ne répond pas")),
                Some(Incoming::Push(b)) => chan.send_raw(&b)?,
                Some(Incoming::Msg(r)) if r.is_push() => self.on_push(r),
                Some(Incoming::Msg(Response::Pong)) if !matches!(req, Request::Ping) => {}
                Some(Incoming::Msg(r)) => return Ok(r),
            }
        }
    }

    fn handle(&mut self, cmd: Cmd) -> Result<()> {
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
                let deriving = std::thread::spawn(move || crypto::derive(&p, &w));
                self.connect()?;
                self.emit(Event::Progress("Vérification du mot de passe…"));
                let keys = deriving
                    .join()
                    .map_err(|_| anyhow!("calcul des clés du compte interrompu"))??;
                let wrong = error_text(ErrorCode::WrongCredentials, "");
                match self.login(&pseudo, &keys, totp, device) {
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
            } => match self.request(&Request::Host {
                kind,
                name,
                map,
                map_sig,
                max_players,
            })? {
                Response::Hosted { room } => {
                    self.emit(Event::Hosted(room));
                    Ok(())
                }
                r => self.unexpected(r),
            },
            Cmd::RoomStarted(started) => match self.request(&Request::RoomStarted { started })? {
                Response::Done => Ok(()),
                r => self.unexpected(r),
            },
            Cmd::Join(room) => match self.request(&Request::Join { room })? {
                Response::Joined {
                    room,
                    host,
                    map,
                    map_sig,
                } => {
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
                    } => {
                        self.emit(Event::Joined {
                            room,
                            host,
                            map,
                            map_sig,
                        });
                        Ok(())
                    }
                    Response::Hosted { room } => {
                        self.emit(Event::Hosted(room));
                        Ok(())
                    }
                    r => self.unexpected(r),
                }
            }
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
                Ok(())
            }
            Cmd::GarantReady => {
                self.oneway(&Request::GarantReady)?;
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
            Cmd::Friend { pseudo, add } => match self.request(&Request::Friend { pseudo, add })? {
                Response::Friends { list, online } => {
                    let both = list
                        .into_iter()
                        .enumerate()
                        .map(|(i, p)| (p, online.get(i).copied().unwrap_or(false)))
                        .collect();
                    self.emit(Event::Friends(both));
                    Ok(())
                }
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
    fn oneway(&mut self, req: &Request) -> Result<()> {
        self.chan
            .as_mut()
            .ok_or_else(|| anyhow!("pas connecté"))?
            .send(req)
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
