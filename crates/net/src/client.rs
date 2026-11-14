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
    },
    Report(String),
    Unreport(String),
    Ban {
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
    Maps,
    /// Envoie une carte au relais (s'il ne l'a pas déjà).
    UploadMap {
        sig: [u8; 32],
        name: String,
        bytes: Vec<u8>,
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
    LoggedIn {
        pseudo: String,
        fingerprint: String,
        totp: bool,
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
    /// Session ouverte : file pour envoyer des données de jeu sans attendre
    /// (`transport::push` d'un `Request::ToHost`, `ToPeer` ou `Broadcast`).
    Link(Sender<Inbound>),
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
    Leaderboard {
        friends: bool,
        list: Vec<crate::proto::RankEntry>,
        me: Option<crate::proto::RankEntry>,
    },
    Friends(Vec<String>),
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
    },
    PeerLeft(u64),
    RoomClosed,
    Promoted(Vec<(u64, String)>),
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

    fn spawn_with(addr: String, pinned: Option<[u8; 32]>, resolve: Option<Resolver>) -> Self {
        let (tx, cmd_rx) = channel::<Cmd>();
        let (ev_tx, rx) = channel::<Event>();
        std::thread::Builder::new()
            .name("relais".into())
            .spawn(move || {
                let mut w = Worker::new(addr, pinned, ev_tx);
                w.resolve = resolve;
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

/// Message d'une erreur du relais, pour le joueur.
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
                    self.emit(Event::Error(format!("{e:#}")));
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
            Response::PeerJoined { peer, pseudo } => Event::PeerJoined { peer, pseudo },
            Response::PeerLeft { peer } => Event::PeerLeft(peer),
            Response::RoomClosed => Event::RoomClosed,
            Response::Promoted { members } => Event::Promoted(members),
            Response::Voice { from, data } => Event::Voice { from, data },
            Response::HostChanged { host } => Event::HostChanged(host),
            Response::FromPeer { peer, data } => Event::FromPeer { peer, data },
            Response::FromHost { data } => Event::FromHost(data),
            Response::Zones { gained, lost } => Event::Zones { gained, lost },
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

    /// Nouvelle connexion au relais (vérifie sa clé).
    fn connect(&mut self) -> Result<()> {
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
        chan.send(&Request::Hello {
            protocol: proto::PROTOCOL,
            client: format!("minelib {}", env!("CARGO_PKG_VERSION")),
        })?;
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
                self.login(&pseudo, &keys, None)
            }
            Cmd::Login {
                pseudo,
                password,
                totp,
            } => {
                self.chan = None;
                self.connect()?;
                self.emit(Event::Progress("Vérification du mot de passe…"));
                let keys = crypto::derive(&pseudo, &password)?;
                self.login(&pseudo, &keys, totp)
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
            Cmd::Search { map_sig, map } => {
                match self.request(&Request::Search { map_sig, map })? {
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
                Response::Friends { list } => {
                    self.emit(Event::Friends(list));
                    Ok(())
                }
                r => self.unexpected(r),
            },
            Cmd::Maps => match self.request(&Request::Maps)? {
                Response::Maps { list } => {
                    self.emit(Event::Maps(list));
                    Ok(())
                }
                r => self.unexpected(r),
            },
            Cmd::UploadMap { sig, name, bytes } => {
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

    fn unexpected(&self, r: Response) -> Result<()> {
        match r {
            Response::Error { code, message } => Err(anyhow!(error_text(code, &message))),
            r => Err(anyhow!("réponse inattendue : {r:?}")),
        }
    }

    fn login(&mut self, pseudo: &str, keys: &crypto::Keys, totp: Option<u32>) -> Result<()> {
        let hw = crate::hw::fingerprint_for(&self.relay_pub);
        let r = self.request(&Request::Login {
            pseudo: pseudo.to_string(),
            auth: keys.auth,
            totp,
            hw,
        })?;
        let (vault, challenge) = match r {
            Response::Vault { vault, challenge } => (vault, challenge),
            Response::NeedTotp => {
                self.emit(Event::NeedTotp);
                return Ok(());
            }
            r => return self.unexpected(r),
        };
        let id = crypto::open_identity(keys, &vault)?;
        let signature = id.sign(&challenge).to_bytes().to_vec();
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
