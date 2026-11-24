//! Compte du joueur : écrans d'accueil (se connecter, créer un compte, jouer hors ligne),
//! profil et double authentification. Le dialogue avec le relais se fait dans un fil de
//! fond (`minelib_net::client`) : l'interface ne se fige jamais.
//!
//! Si le relais configuré est sur ce PC (127.0.0.1) et ne répond pas, le jeu en lance un
//! lui-même, avec ses données dans `.minelib/relay`.

use std::collections::HashSet;
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::time::Duration;

use minelib_net::client::{Client, Cmd, Event};
use minelib_net::guard;
use minelib_net::proto::{self, ServerEntry};
use minelib_net::{crypto, totp};
use minelib_platform::{Input, KeyCode};
use minelib_render::Ui;

use crate::menu::{self, Ctx, GOLD, GREY, RED, WHITE};

/// Le relais de secours n'a pas repris le monde après ce temps : abandon.
const TAKEOVER_GIVE_UP: Duration = Duration::from_secs(60);

/// Nœud iroh du jeu (un seul, gardé pour la durée du jeu). Son identité est gardée à côté
/// du dossier du relais (`noeud.key`) : la même à chaque lancement.
fn node(dir: &std::path::Path) -> Option<&'static minelib_p2p::Node> {
    p2p_node(dir)
}

/// Nœud iroh du jeu (voir `node`), pour les relais de région.
pub(crate) fn p2p_node(dir: &std::path::Path) -> Option<&'static minelib_p2p::Node> {
    static NODE: std::sync::OnceLock<Option<minelib_p2p::Node>> = std::sync::OnceLock::new();
    NODE.get_or_init(|| {
        let path = dir.with_file_name("noeud.key");
        let secret: [u8; 32] = std::fs::read(&path)
            .ok()
            .and_then(|b| b.try_into().ok())
            .unwrap_or_else(|| {
                let k: [u8; 32] = minelib_net::transport::generate_static().0;
                let _ = std::fs::write(&path, k);
                k
            });
        match minelib_p2p::Node::start_with(Some(secret)) {
            Ok(n) => Some(n),
            Err(e) => {
                log::warn!("iroh : {e:#}");
                None
            }
        }
    })
    .as_ref()
}

/// Relais du DHT qui répond vraiment : chaque candidat est essayé en même temps (tunnel
/// iroh, puis poignée de main chiffrée avec la clé annoncée), le premier qui répond est
/// pris, le relais déjà connu d'abord s'il répond aussi vite. Un relais éteint dont
/// l'annonce traîne encore est écarté en quelques secondes, sans erreur pour le joueur.
fn reachable_relay(
    node: &'static minelib_p2p::Node,
    ads: Vec<minelib_p2p::Ad>,
    known: Option<[u8; 32]>,
) -> Option<(SocketAddr, minelib_p2p::Ad)> {
    const WAIT: Duration = Duration::from_secs(4);
    if ads.is_empty() {
        return None;
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let n = ads.len();
    for ad in ads {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let found = node.tunnel_checked(ad.node, WAIT).ok().filter(|addr| {
                minelib_net::transport::Channel::connect(&addr.to_string(), WAIT)
                    .is_ok_and(|(_, key)| key == ad.noise)
            });
            if found.is_none() {
                log::info!(
                    "relais « {} » du DHT injoignable (nœud {}) : écarté",
                    ad.name,
                    crypto::hex(&ad.node[..8])
                );
            }
            let _ = tx.send(found.map(|a| (a, ad)));
        });
    }
    drop(tx);
    let end = std::time::Instant::now() + WAIT + WAIT + Duration::from_secs(1);
    let mut first: Option<(SocketAddr, minelib_p2p::Ad)> = None;
    let mut first_at = std::time::Instant::now();
    for _ in 0..n {
        // Un relais a répondu : on laisse 300 ms au relais connu pour répondre aussi.
        let limit = match &first {
            Some(_) => first_at + Duration::from_millis(300),
            None => end,
        };
        let left = limit.saturating_duration_since(std::time::Instant::now());
        match rx.recv_timeout(left) {
            Ok(Some(r)) => {
                if Some(r.1.noise) == known {
                    return Some(r);
                }
                if first.is_none() {
                    first_at = std::time::Instant::now();
                    first = Some(r);
                }
            }
            Ok(None) => {}
            Err(_) => break,
        }
    }
    first
}

/// Trouve un relais sans réglage : sur ce PC, sur le réseau local, puis dans le DHT de
/// BitTorrent (par un tunnel iroh) ; sinon ce PC devient le relais (visible du réseau
/// local et inscrit dans le DHT). Le réseau local et le DHT sont interrogés en même
/// temps, et un relais n'est retenu que s'il répond : jamais d'erreur « relais
/// injoignable » à cause d'une vieille annonce.
fn auto_relay(
    dir: &std::path::Path,
    failed: &mut Vec<[u8; 32]>,
) -> anyhow::Result<(String, Option<[u8; 32]>)> {
    let t0 = std::time::Instant::now();
    let port = minelib_net::DEFAULT_PORT;
    let local = SocketAddr::from(([127, 0, 0, 1], port));
    if TcpStream::connect_timeout(&local, Duration::from_millis(300)).is_ok() {
        return Ok((local.to_string(), None));
    }
    // Réseau local et DHT en même temps.
    let (lan_tx, lan_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = lan_tx.send(minelib_net::lan::discover(Duration::from_millis(1200)));
    });
    let (dht_tx, dht_rx) = std::sync::mpsc::channel();
    let dir2 = dir.to_path_buf();
    std::thread::spawn(move || {
        let node = node(&dir2);
        let list = minelib_p2p::find_relays();
        let _ = dht_tx.send((node, list));
    });
    if let Ok(Some((lan, key))) = lan_rx.recv() {
        log::info!("relais trouvé sur le réseau local : {lan}");
        return Ok((lan.to_string(), Some(key)));
    }
    // Le DHT répond en 2 à 3 secondes ; au-delà (réseau filtré), on fait sans.
    let left = Duration::from_secs(6).saturating_sub(t0.elapsed());
    let (found, list) = match dht_rx.recv_timeout(left) {
        Ok((n, Ok(list))) => (n, list),
        Ok((n, Err(e))) => {
            log::warn!("DHT : {e:#}");
            (n, Vec::new())
        }
        Err(_) => {
            log::warn!("DHT : pas de réponse à temps");
            (None, Vec::new())
        }
    };
    if let Some(node) = found {
        // Le relais où l'on jouait déjà passe avant les autres : la date d'un relais n'est
        // que ce qu'il annonce, un faux relais pourrait se dire plus vieux que tous pour
        // attirer les joueurs.
        let known_path = dir.with_file_name("relais_connu");
        let known: Option<[u8; 32]> = std::fs::read_to_string(&known_path)
            .ok()
            .and_then(|t| crypto::unhex(t.trim()))
            .and_then(|v| v.try_into().ok());
        // Ni un relais déjà essayé sans succès, ni soi-même : ce nœud, ou une ancienne
        // annonce de ce PC (reconnue à la clé de son propre relais).
        let mine = minelib_relay::public_key(dir);
        let ads: Vec<minelib_p2p::Ad> = list
            .into_iter()
            .filter(|a| !failed.contains(&a.node) && a.node != node.id() && Some(a.noise) != mine)
            .take(4)
            .collect();
        for a in &ads {
            failed.push(a.node);
        }
        if let Some((addr, ad)) = reachable_relay(node, ads, known) {
            log::info!(
                "relais « {} » trouvé dans le DHT (nœud {}), en {:.1} s",
                ad.name,
                crypto::hex(&ad.node[..8]),
                t0.elapsed().as_secs_f32()
            );
            if known.is_none() {
                let _ = std::fs::write(&known_path, crypto::hex(&ad.noise));
            }
            return Ok((addr.to_string(), Some(ad.noise)));
        }
    }
    // Personne : ce PC devient le relais.
    let h = minelib_relay::start(minelib_relay::Config {
        dir: dir.to_path_buf(),
        bind: format!("0.0.0.0:{port}"),
        pow_bits: 20,
        name: "Relais d'un joueur".into(),
    })
    .or_else(|_| {
        minelib_relay::start(minelib_relay::Config {
            dir: dir.to_path_buf(),
            bind: "0.0.0.0:0".into(),
            pow_bits: 20,
            name: "Relais d'un joueur".into(),
        })
    })?;
    log::info!(
        "ce PC devient le relais ({}), après {:.1} s de recherche",
        h.addr,
        t0.elapsed().as_secs_f32()
    );
    minelib_net::lan::announce(h.addr.port(), h.public);
    let mine = SocketAddr::from(([127, 0, 0, 1], h.addr.port()));
    let public = h.public;
    // Joignable de partout (tunnel iroh) et inscrit dans le DHT : en tâche de fond, le
    // joueur est connecté à son relais sans attendre.
    let dir = dir.to_path_buf();
    std::thread::spawn(move || {
        let Some(node) = node(&dir) else { return };
        node.serve(mine);
        // Pseudos uniques avec les autres relais du DHT.
        h.set_whois(minelib_relay::federated_whois(node, public));
        h.set_peer_servers(minelib_relay::federated_servers(node, public));
        minelib_p2p::publish(minelib_p2p::Ad {
            node: node.id(),
            noise: public,
            name: "Relais d'un joueur".into(),
            since: minelib_p2p::now(),
            seen: minelib_p2p::now(),
        });
    });
    Ok((mine.to_string(), Some(public)))
}

/// Compte connecté.
#[derive(Clone, Debug)]
pub struct Account {
    pub pseudo: String,
    /// Empreinte de la clé du compte (vérifier qu'on parle du même compte).
    pub fingerprint: String,
    pub totp: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    /// Accueil : se connecter, créer un compte ou jouer hors ligne.
    Welcome,
    Login,
    Register,
    /// Compte connecté : double authentification, déconnexion.
    Profile,
    TotpSetup,
    TotpDisable,
}

/// Ce que l'écran demande au jeu.
pub enum Outcome {
    None,
    Goto(Page),
    /// Connecté (ou hors ligne) : on quitte les écrans de compte.
    Leave,
}

pub struct Accounts {
    client: Option<Client>,
    relay_addr: String,
    relay_key: Option<[u8; 32]>,
    relay_dir: PathBuf,
    embedded: Option<minelib_relay::Handle>,
    pub account: Option<Account>,
    pseudo: String,
    password: String,
    confirm: String,
    code: String,
    focus: usize,
    busy: Option<String>,
    error: Option<String>,
    info: Option<String>,
    need_totp: bool,
    /// Secret 2FA en cours d'activation et son adresse `otpauth://`.
    totp_setup: Option<(Vec<u8>, String)>,
    /// Clé de relais nouvellement retenue (à enregistrer dans les options).
    pub new_relay_key: Option<String>,
    /// Pseudo à enregistrer dans les options après une connexion réussie.
    pub new_pseudo: Option<String>,
    pub servers: Vec<ServerEntry>,
    /// File pour envoyer des données de jeu au relais sans attendre.
    pub link: Option<std::sync::mpsc::Sender<minelib_net::transport::Inbound>>,
    /// Clé du compte, une fois connecté : elle signe ses modifications dans une région
    /// souveraine (voir `minelib_net::sovereign`).
    pub identity: Option<minelib_net::ed25519_dalek::SigningKey>,
    /// Relais joint (adresse, clé) et identifiants de la session : une deuxième connexion
    /// du compte peut y servir de garant pendant qu'on joue.
    relay_at: Option<(String, [u8; 32])>,
    login_secret: Option<(String, String)>,
    /// Événements de partie (hôte, invité) à transmettre au jeu.
    pub game_events: Vec<Event>,
    /// Classement reçu : (amis ?, lignes, sa propre ligne).
    pub leaderboard: Option<(bool, Vec<proto::RankEntry>, Option<proto::RankEntry>)>,
    /// Amis et s'ils sont en ligne.
    pub friends: Vec<(String, bool)>,
    /// (v231) Où ils sont (même ordre), invitations reçues dans le monde d'un ami (la plus
    /// récente d'abord), statut invisible voulu (redit au relais à chaque connexion).
    pub friend_places: Vec<proto::FriendPlace>,
    pub world_invites: Vec<crate::friends::WorldInvite>,
    pub invisible: bool,
    /// (v231) Demandes d'ami reçues (dernière liste, et poussées depuis), celles pas encore
    /// annoncées (notification), et toutes celles déjà vues depuis le lancement (en
    /// minuscules : une reconnexion ne les annonce pas de nouveau).
    pub friend_requests: Vec<String>,
    pub new_requests: Vec<String>,
    requests_seen: HashSet<String>,
    /// (v240) Clés d'accès du compte (page Compte, demandées en l'ouvrant), l'opération en
    /// cours avec Windows (fil de fond), une connexion par clé voulue (en attente du défi),
    /// et la clé du coffre de la clé donnée.
    pub passkeys: Vec<proto::PasskeyInfo>,
    passkeys_asked: bool,
    passkey_job: Option<std::sync::mpsc::Receiver<crate::passkeys::Done>>,
    passkey_login: bool,
    passkey_key: Option<[u8; 32]>,
    /// Lobby : n° (0 : seul) et membres (l'hôte d'abord) ; invitations reçues.
    pub party: (u64, Vec<String>),
    pub invites: Vec<String>,
    /// Numéro de la dernière liste des cartes reçue du relais.
    pub maps_rev: u64,
    pub maps: Vec<proto::MapEntry>,
    /// Relais d'un autre PC du réseau local (sa clé n'est pas retenue).
    lan_relay: bool,
    /// Connexion en cours (pseudo, mot de passe) : gardée sur ce PC si elle réussit.
    sent: Option<(String, String)>,
    /// Partie à rejoindre dès la connexion à son relais (voir `join_elsewhere`).
    join_after_login: Option<u64>,
    lan_before: bool,
    /// La connexion automatique du lancement a été tentée.
    auto_tried: bool,
    /// Connexion automatique en cours ; essais ratés faute de relais joignable, et moment
    /// du prochain essai (silencieux : le joueur n'a rien fait de mal).
    auto_pending: bool,
    auto_fails: u8,
    auto_next: Option<std::time::Instant>,
    /// Monde de construction en ligne : où est son relais de secours, et le certificat de
    /// la clé du compte pour y entrer si le relais du monde tombe.
    standby: Option<proto::StandbyAt>,
    cert: Option<minelib_net::sovereign::Cert>,
    /// Reprise par le relais de secours en cours : depuis quand, prochain essai.
    taking_over: Option<(std::time::Instant, std::time::Instant)>,
    /// Monde en pause (son relais n'entend plus assez de gardiens), dernier message du
    /// relais du monde : on cherche alors son relais de secours sans quitter le relais du
    /// monde (connexion d'essai, prochain essai).
    world_paused: bool,
    last_heard: std::time::Instant,
    probe: Option<Client>,
    probe_next: Option<std::time::Instant>,
    /// Le monde a été repris : le relais qui le sert désormais (la connexion de garant y
    /// entre avec le certificat de la clé du compte).
    world_at: Option<proto::StandbyAt>,
    /// (v146) Veille : au fond d'un pays, la connexion au relais du monde est fermée (monde
    /// `room`) ; les commandes pour lui attendent qu'on y revienne. Demande envoyée ; retour
    /// en cours (depuis quand) et retours ratés.
    dozing: Option<u64>,
    doze_sent: bool,
    doze_queue: Vec<Cmd>,
    waking: Option<std::time::Instant>,
    wake_next: Option<std::time::Instant>,
    wake_fails: u8,
}

/// (v146) Retour au relais du monde après la veille : nouvel essai au bout de… ; au-delà de
/// tant d'essais ratés, on quitte le monde.
const WAKE_RETRY: Duration = Duration::from_secs(3);
const WAKE_GIVE_UP: u8 = 4;

impl Accounts {
    pub fn new(relay_addr: String, relay_key: &str, relay_dir: PathBuf, pseudo: String) -> Self {
        let relay_key = crypto::unhex(relay_key).and_then(|v| v.try_into().ok());
        Self {
            client: None,
            relay_addr,
            relay_key,
            relay_dir,
            embedded: None,
            account: None,
            pseudo,
            password: String::new(),
            confirm: String::new(),
            code: String::new(),
            focus: 0,
            busy: None,
            error: None,
            info: None,
            need_totp: false,
            totp_setup: None,
            new_relay_key: None,
            new_pseudo: None,
            servers: Vec::new(),
            link: None,
            identity: None,
            relay_at: None,
            login_secret: None,
            game_events: Vec::new(),
            leaderboard: None,
            friends: Vec::new(),
            friend_places: Vec::new(),
            world_invites: Vec::new(),
            invisible: false,
            friend_requests: Vec::new(),
            new_requests: Vec::new(),
            requests_seen: HashSet::new(),
            passkeys: Vec::new(),
            passkeys_asked: false,
            passkey_job: None,
            passkey_login: false,
            passkey_key: None,
            party: (0, Vec::new()),
            invites: Vec::new(),
            maps_rev: 0,
            maps: Vec::new(),
            lan_relay: false,
            join_after_login: None,
            lan_before: false,
            sent: None,
            auto_tried: false,
            auto_pending: false,
            auto_fails: 0,
            auto_next: None,
            standby: None,
            cert: None,
            taking_over: None,
            world_paused: false,
            last_heard: std::time::Instant::now(),
            probe: None,
            probe_next: None,
            world_at: None,
            dozing: None,
            doze_sent: false,
            doze_queue: Vec::new(),
            waking: None,
            wake_next: None,
            wake_fails: 0,
        }
    }

    /// Compte gardé sur ce PC pour la connexion automatique (chiffré avec une clé propre
    /// à cette session Windows : le fichier copié sur un autre PC ne s'ouvre pas).
    fn session_path(&self) -> PathBuf {
        self.relay_dir.with_file_name("comptes").join("session")
    }

    fn session_key(&self) -> [u8; 32] {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(b"minelib/session/v1");
        for var in ["COMPUTERNAME", "USERNAME", "USERDOMAIN", "HOSTNAME", "USER"] {
            h.update(std::env::var(var).unwrap_or_default().as_bytes());
            h.update([0]);
        }
        h.update(self.relay_dir.to_string_lossy().as_bytes());
        h.finalize().into()
    }

    fn remember(&self, pseudo: &str, password: &str) {
        let p = self.session_path();
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let plain = format!("{pseudo}\n{password}");
        let _ = std::fs::write(p, crypto::seal(&self.session_key(), plain.as_bytes()));
    }

    fn remembered(&self) -> Option<(String, String)> {
        let sealed = std::fs::read(self.session_path()).ok()?;
        let plain = crypto::open(&self.session_key(), &sealed).ok()?;
        let text = String::from_utf8(plain).ok()?;
        let (pseudo, password) = text.split_once('\n')?;
        Some((pseudo.to_string(), password.to_string()))
    }

    /// Jeton d'appareil de confiance d'un compte (2FA déjà faite sur ce PC), chiffré comme
    /// la session.
    fn device_path(&self, pseudo: &str) -> PathBuf {
        self.relay_dir
            .with_file_name("comptes")
            .join(format!("{}.appareil", pseudo.to_lowercase()))
    }

    fn device(&self, pseudo: &str) -> Option<[u8; 32]> {
        let sealed = std::fs::read(self.device_path(pseudo)).ok()?;
        crypto::open(&self.session_key(), &sealed)
            .ok()?
            .try_into()
            .ok()
    }

    fn keep_device(&self, pseudo: &str, token: &[u8; 32]) {
        let p = self.device_path(pseudo);
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(p, crypto::seal(&self.session_key(), token));
    }

    /// Au lancement : reconnecte le compte gardé sur ce PC, s'il y en a un.
    fn auto_login(&mut self) {
        let Some((pseudo, password)) = self.remembered() else {
            return;
        };
        let backup = std::fs::read(self.backup_path(&pseudo)).ok();
        self.auto_pending = true;
        self.sent = Some((pseudo.clone(), password.clone()));
        self.pseudo = pseudo.clone();
        let device = self.device(&pseudo);
        self.send(Cmd::Login {
            pseudo,
            password,
            totp: None,
            backup,
            device,
        });
        self.busy = None;
    }

    /// Coffre du compte gardé sur ce PC (chiffré par le mot de passe).
    fn backup_path(&self, pseudo: &str) -> PathBuf {
        self.relay_dir
            .with_file_name("comptes")
            .join(format!("{}.coffre", pseudo.to_lowercase()))
    }

    pub fn logged_in(&self) -> bool {
        self.account.is_some()
    }

    /// La connexion automatique du lancement est-elle finie (connecté, code 2FA demandé,
    /// refusée, ou pas de compte gardé) ?
    pub fn auto_settled(&self) -> bool {
        self.auto_tried && (self.account.is_some() || self.need_totp || self.sent.is_none())
    }

    fn local_relay(&self) -> Option<SocketAddr> {
        let addr = self.relay_addr.to_socket_addrs().ok()?.next()?;
        addr.ip().is_loopback().then_some(addr)
    }

    /// Fil du relais (créé au premier besoin) ; lance le relais intégré si le relais
    /// configuré est local et ne répond pas.
    fn client(&mut self) -> &Client {
        // « auto » (par défaut) : relais trouvé tout seul (réseau local, DHT), ou ce PC le
        // devient. L'ancienne valeur par défaut (127.0.0.1:27650) compte comme « auto ».
        let auto = self.relay_addr == "auto" || self.relay_addr == "127.0.0.1:27650";
        if self.client.is_none() && auto {
            let dir = self.relay_dir.clone();
            let mut failed: Vec<[u8; 32]> = Vec::new();
            self.lan_relay = true;
            self.client = Some(Client::spawn_resolved(Box::new(move || {
                auto_relay(&dir, &mut failed)
            })));
        }
        if self.client.is_none() {
            let mut addr_str = self.relay_addr.clone();
            let mut key = self.relay_key;
            if let Some(addr) = self.local_relay() {
                let up = TcpStream::connect_timeout(&addr, Duration::from_millis(300)).is_ok();
                if !up && self.embedded.is_none() {
                    // Un autre PC de la maison a déjà un relais : on le rejoint.
                    if let Some((lan, lan_key)) =
                        minelib_net::lan::discover(Duration::from_millis(2500))
                    {
                        log::info!("relais trouvé sur le réseau local : {lan}");
                        addr_str = lan.to_string();
                        key = Some(lan_key);
                        self.lan_relay = true;
                    } else {
                        // Sinon ce PC en devient un, visible du réseau local.
                        let cfg = minelib_relay::Config {
                            dir: self.relay_dir.clone(),
                            bind: format!("0.0.0.0:{}", addr.port()),
                            pow_bits: 20,
                            name: "Relais local".into(),
                        };
                        match minelib_relay::start(cfg) {
                            Ok(h) => {
                                log::info!("relais intégré lancé sur {}", h.addr);
                                minelib_net::lan::announce(h.addr.port(), h.public);
                                self.embedded = Some(h);
                            }
                            Err(e) => log::warn!("relais intégré : {e:#}"),
                        }
                    }
                }
            }
            self.client = Some(Client::spawn(addr_str, key));
        }
        self.client.as_ref().expect("client")
    }

    fn send(&mut self, cmd: Cmd) {
        self.error = None;
        self.info = None;
        self.busy = Some("Connexion au relais…".into());
        self.client().send(cmd);
    }

    /// Lit les réponses du relais (à chaque image, quel que soit l'écran).
    /// Un relais de région a redit le battement du relais du monde : il est en vie (v136 :
    /// il ne l'envoie plus lui-même aux joueurs servis par un relais de région ; une pause,
    /// elle, arrive toujours directement de lui).
    pub fn relayed_beat(&mut self) {
        self.last_heard = std::time::Instant::now();
    }

    pub fn poll(&mut self) {
        self.passkey_upkeep();
        if !self.auto_tried {
            self.auto_tried = true;
            if self.account.is_none() && self.client.is_none() {
                self.auto_login();
            }
            // Essais : `MINELIB_TEST_ACCOUNT=pseudo:motdepasse` crée ce compte au premier
            // lancement (il est ensuite gardé, comme après une inscription à la main).
            if let Some((p, w)) = std::env::var("MINELIB_TEST_ACCOUNT")
                .ok()
                .as_deref()
                .and_then(|v| v.split_once(':'))
                .filter(|_| self.sent.is_none())
            {
                self.sent = Some((p.to_string(), w.to_string()));
                self.send(Cmd::Register {
                    pseudo: p.to_string(),
                    password: w.to_string(),
                });
            }
        }
        // Connexion automatique ratée faute de relais joignable : nouvel essai, en silence.
        if self
            .auto_next
            .is_some_and(|t| std::time::Instant::now() >= t)
        {
            self.auto_next = None;
            if self.account.is_none() && !self.need_totp {
                self.auto_login();
            }
        }
        // Reprise par le relais de secours : il n'a pas encore repris le monde (il attend
        // d'être sûr que le relais du monde est tombé) ; nouvel essai.
        if let Some((since, next)) = self.taking_over {
            let now = std::time::Instant::now();
            if since.elapsed() > TAKEOVER_GIVE_UP {
                self.taking_over = None;
                self.standby = None;
                let m = "Le relais du monde ne répond plus, et son relais de secours non plus."
                    .to_string();
                self.error = Some(m.clone());
                self.game_events.push(Event::Error(m));
            } else if now >= next {
                self.taking_over = Some((since, now + Duration::from_secs(60)));
                self.start_takeover();
            }
        }
        // Monde en pause, ou relais du monde muet : on cherche son relais de secours, sans
        // quitter le relais du monde (il peut repartir). Dès que le relais de secours a
        // repris le monde, la partie continue chez lui.
        let now = std::time::Instant::now();
        let silent = self.standby.is_some()
            && self.last_heard.elapsed() > Duration::from_millis(guard::SILENT_MS);
        // (En veille, le relais du monde ne nous dit plus rien : ce n'est pas un silence.)
        let want_probe = (self.world_paused || silent)
            && self.taking_over.is_none()
            && self.can_take_over()
            && self.dozing.is_none();
        if !want_probe {
            self.probe = None;
            self.probe_next = None;
        } else if self.probe.is_none() && self.probe_next.is_none_or(|t| now >= t) {
            self.probe = self.takeover_client();
            self.probe_next = Some(now + Duration::from_millis(guard::PROBE_MS));
        }
        let mut events = Vec::new();
        if let Some(p) = &self.probe {
            let mut got = Vec::new();
            while let Some(e) = p.poll() {
                got.push(e);
            }
            if got.iter().any(|e| matches!(e, Event::TakenOver { .. })) {
                log::info!("relais de secours : il a repris le monde, la partie continue chez lui");
                self.client = self.probe.take();
                self.world_paused = false;
                self.probe_next = None;
                self.last_heard = now;
                events = got;
            } else if got.iter().any(|e| {
                matches!(
                    e,
                    Event::Error(_) | Event::Offline(_) | Event::Lost | Event::LoggedOut
                )
            }) {
                // (Il n'a pas encore repris le monde, ou ne répond pas : nouvel essai.)
                self.probe = None;
            }
        }
        let Some(client) = &self.client else { return };
        if events.is_empty() {
            while let Some(e) = client.poll() {
                events.push(e);
            }
            if !events.is_empty() {
                self.last_heard = now;
            }
        }
        // L'autre relais (partie d'un relais fédéré) n'a pas voulu de nous : retour au
        // relais d'origine.
        let mut go_home = false;
        for e in events {
            match e {
                // (v146) Veille : la connexion au relais du monde est fermée, la partie
                // continue par les provinces.
                Event::Dozed { room } => {
                    log::info!("monde {room} : veille (au fond d'un pays)");
                    self.dozing = Some(room);
                    self.doze_sent = false;
                    self.client = None;
                    self.link = None;
                    self.probe = None;
                    self.probe_next = None;
                    self.game_events.push(Event::Dozed { room });
                    break;
                }
                // (v146) De retour au relais du monde (le même) : ce qui attendait part.
                Event::TakenOver { .. } if self.waking.is_some() => {
                    log::info!("veille finie : de retour au relais du monde");
                    self.waking = None;
                    self.wake_next = None;
                    self.wake_fails = 0;
                    self.dozing = None;
                    self.last_heard = now;
                    if let Some(c) = &self.client {
                        for cmd in self.doze_queue.drain(..) {
                            c.send(cmd);
                        }
                    }
                    self.game_events.push(e);
                }
                Event::Error(m) | Event::Offline(m) if self.waking.is_some() => {
                    self.wake_failed(&m);
                    break;
                }
                Event::Lost | Event::LoggedOut if self.waking.is_some() => {
                    self.wake_failed("connexion perdue");
                    break;
                }
                // (Veille refusée par le relais du monde : on n'insiste pas dans ce monde.)
                Event::Error(m) if self.doze_sent && self.dozing.is_none() => {
                    log::info!("veille refusée : {m}");
                }
                Event::Pinned(k) => {
                    if !self.lan_relay {
                        self.relay_key = Some(k);
                        self.new_relay_key = Some(crypto::hex(&k));
                    }
                }
                Event::Progress(p) => self.busy = Some(p.into()),
                Event::Identity(key) => self.identity = Some(key),
                Event::LoggedIn {
                    pseudo,
                    fingerprint,
                    totp,
                } => {
                    self.busy = None;
                    self.need_totp = false;
                    self.auto_pending = false;
                    self.auto_fails = 0;
                    // Compte gardé sur ce PC : connexion automatique au prochain lancement.
                    if let Some((p, w)) = self.sent.take() {
                        self.login_secret = Some((p.clone(), w.clone()));
                        self.remember(&p, &w);
                    }
                    self.password.clear();
                    self.confirm.clear();
                    self.code.clear();
                    self.new_pseudo = Some(pseudo.clone());
                    self.account = Some(Account {
                        pseudo,
                        fingerprint,
                        totp,
                    });
                    if let (Some(room), Some(c)) = (self.join_after_login.take(), &self.client) {
                        c.send(Cmd::Join(room));
                    }
                    // (v231) Invisible pour ses amis : le relais l'apprend à chaque connexion.
                    // Liste d'amis demandée tout de suite : les demandes d'ami reçues hors
                    // ligne sont annoncées.
                    if let Some(c) = &self.client {
                        if self.invisible {
                            c.send(Cmd::Presence(true));
                        }
                        c.send(Cmd::Friend {
                            pseudo: String::new(),
                            add: false,
                        });
                    }
                }
                // Code 2FA juste : ce PC est de confiance, la 2FA ne sera plus demandée ici.
                Event::DeviceTrusted { pseudo, token } => self.keep_device(&pseudo, &token),
                Event::NeedTotp => {
                    self.busy = None;
                    self.auto_pending = false;
                    self.need_totp = true;
                    self.code.clear();
                    self.focus = 2;
                    self.info =
                        Some("Entre le code de ton appli de double authentification.".into());
                }
                Event::TotpSecret { secret, uri } => {
                    self.busy = None;
                    self.totp_setup = Some((secret, uri));
                }
                Event::TotpOn => {
                    self.busy = None;
                    self.totp_setup = None;
                    self.code.clear();
                    if let Some(a) = &mut self.account {
                        a.totp = true;
                    }
                    self.info = Some("Double authentification activée.".into());
                }
                Event::TotpOff => {
                    self.busy = None;
                    self.code.clear();
                    if let Some(a) = &mut self.account {
                        a.totp = false;
                    }
                    self.info = Some("Double authentification désactivée.".into());
                }
                Event::Servers(list) => {
                    self.busy = None;
                    self.servers = list;
                }
                // (v240) Clés d'accès : défi reçu (connexion), clé vérifiée par le relais
                // (on se connecte avec les clés du compte de son coffre), clés du compte.
                Event::PasskeyChallenge(challenge) => {
                    if self.passkey_login {
                        self.passkey_login = false;
                        self.busy = Some("Clé d'accès : réponds à la fenêtre de Windows…".into());
                        self.passkey_job = Some(crate::passkeys::start_get(challenge));
                    }
                }
                Event::PasskeyOk {
                    pseudo,
                    vault,
                    device,
                } => self.passkey_verified(pseudo, &vault, device),
                Event::Passkeys(list) => {
                    if self.busy.is_some() && list.len() > self.passkeys.len() {
                        self.info = Some(
                            "Clé d'accès ajoutée : « Se connecter » → « Avec une clé d'accès »."
                                .into(),
                        );
                    }
                    self.busy = None;
                    self.passkeys = list;
                }
                Event::LoggedOut => {
                    self.busy = None;
                    self.account = None;
                    self.party = (0, Vec::new());
                    self.invites.clear();
                    self.forget_friends();
                }
                // Le relais du monde est tombé pendant qu'on joue dans un monde qui a un
                // relais de secours : on y reprend la partie, sans la quitter (la suite de
                // ce que dit l'ancienne connexion ne compte plus).
                Event::Lost if self.can_take_over() && self.taking_over.is_none() => {
                    log::info!("relais du monde perdu : reprise par le relais de secours");
                    self.game_events.push(Event::Error(
                        "Le relais du monde ne répond plus : reprise par son relais de secours…"
                            .into(),
                    ));
                    let now = std::time::Instant::now();
                    self.taking_over = Some((now, now));
                    self.probe = None;
                    break;
                }
                // Le relais de secours n'a pas encore repris le monde, ou ne répond pas :
                // nouvel essai bientôt.
                Event::Offline(m) | Event::Error(m) if self.taking_over.is_some() => {
                    log::debug!("relais de secours : {m} ; nouvel essai");
                    let now = std::time::Instant::now();
                    if let Some(t) = &mut self.taking_over {
                        t.1 = now + Duration::from_millis(guard::PROBE_MS);
                    }
                }
                Event::Lost => {}
                // Monde en pause, ou qui repart (le jeu garde les modifications du joueur
                // en attendant).
                Event::Paused(p) => {
                    self.world_paused = p;
                    self.game_events.push(e);
                }
                // (Pour les connexions de garant : gardiens des mondes, surveillance
                // mutuelle, relais de région.)
                Event::Beat { .. }
                | Event::WitnessTask { .. }
                | Event::Committee(_)
                | Event::Released { .. }
                | Event::Audit { .. }
                | Event::WorldBeat { .. } => {}
                Event::Standby(at) => {
                    self.standby = at;
                    self.last_heard = std::time::Instant::now();
                }
                Event::StandbyTask { .. } | Event::Replica(_) => {}
                // Le monde est repris : le relais de secours est désormais le relais du
                // monde (il n'a pas lui-même de relais de secours : plus rien à chercher).
                Event::TakenOver { .. } => {
                    self.taking_over = None;
                    self.world_at = self.standby.take();
                    self.world_paused = false;
                    self.probe = None;
                    self.probe_next = None;
                    self.game_events.push(e);
                }
                Event::ShardTicket { ref cert, .. } => {
                    if cert.is_some() {
                        self.cert = cert.clone();
                    }
                    self.game_events.push(e);
                }
                Event::Error(m) => {
                    go_home |= self.join_after_login.take().is_some();
                    self.passkey_login = false;
                    self.passkey_key = None;
                    self.busy = None;
                    self.sent = None;
                    self.auto_pending = false;
                    self.error = Some(m.clone());
                    self.game_events.push(Event::Error(m));
                }
                // Pas de relais joignable pendant la connexion automatique : ce n'est pas
                // une erreur du joueur. On réessaie tout seul (5 fois, de plus en plus
                // espacées), sans message ; le jeu s'ouvre en attendant.
                Event::Offline(m) if self.auto_pending && self.auto_fails < 5 => {
                    self.busy = None;
                    self.sent = None;
                    self.auto_pending = false;
                    self.auto_fails += 1;
                    let wait = Duration::from_secs(2 * self.auto_fails as u64);
                    self.auto_next = Some(std::time::Instant::now() + wait);
                    log::info!(
                        "connexion automatique : {m} — nouvel essai dans {} s ({}/5)",
                        wait.as_secs(),
                        self.auto_fails
                    );
                }
                Event::Offline(m) => {
                    go_home |= self.join_after_login.take().is_some();
                    self.busy = None;
                    self.sent = None;
                    self.auto_pending = false;
                    self.error = Some(m.clone());
                    self.game_events.push(Event::Error(m));
                }
                Event::Link(l) => self.link = Some(l),
                Event::Relay { addr, key } => self.relay_at = Some((addr, key)),
                Event::AccountBackup { pseudo, vault } => {
                    let p = self.backup_path(&pseudo);
                    if let Some(d) = p.parent() {
                        let _ = std::fs::create_dir_all(d);
                    }
                    let _ = std::fs::write(p, vault);
                }
                Event::Leaderboard { friends, list, me } => {
                    self.busy = None;
                    self.leaderboard = Some((friends, list, me));
                }
                // (v231) Liste d'amis : passée au jeu avec ce qui la suit (où ils sont, demandes).
                Event::Friends(list) => {
                    self.busy = None;
                    self.friends = list;
                }
                Event::FriendInfo {
                    places,
                    requests,
                    invites,
                } => {
                    self.friend_places = places;
                    // (v242) Le relais garde les invitations jusqu'à la réponse : sa liste fait
                    // foi.
                    self.world_invites = invites;
                    for r in &requests {
                        if self.requests_seen.insert(r.to_lowercase()) {
                            self.new_requests.push(r.clone());
                        }
                    }
                    self.friend_requests = requests;
                    self.game_events.push(Event::Friends(self.friends.clone()));
                }
                // Demande d'ami poussée (annoncée), ou « vous êtes amis » (au jeu).
                Event::FriendAdded { from, mutual } => {
                    if mutual {
                        // Il est maintenant en ligne pour nous, et l'on sait où : liste à jour.
                        if let Some(c) = &self.client {
                            c.send(Cmd::Friend {
                                pseudo: String::new(),
                                add: false,
                            });
                        }
                        self.game_events.push(Event::FriendAdded { from, mutual });
                    } else {
                        if !self
                            .friend_requests
                            .iter()
                            .any(|r| r.eq_ignore_ascii_case(&from))
                        {
                            self.friend_requests.insert(0, from.clone());
                        }
                        // Poussée : il vient de nous ajouter (encore, après un refus) : annoncée.
                        self.requests_seen.insert(from.to_lowercase());
                        self.new_requests.push(from);
                    }
                }
                Event::Invited { from, room, name } => {
                    self.world_invites.retain(|i| !i.from.eq_ignore_ascii_case(&from));
                    self.world_invites.insert(
                        0,
                        crate::friends::WorldInvite {
                            from: from.clone(),
                            room,
                            name: name.clone(),
                            open: room != 0,
                        },
                    );
                    self.game_events.push(Event::Invited { from, room, name });
                }
                e @ (Event::InviteSent(_) | Event::InviteDeclined { .. }) => {
                    self.game_events.push(e)
                }
                Event::Maps(list) => {
                    self.busy = None;
                    self.maps = list;
                    self.maps_rev += 1;
                }
                Event::Party { id, members } => {
                    // Dans un lobby : ses invitations deviennent sans objet.
                    if id != 0 {
                        self.invites.retain(|p| !members.contains(p));
                    }
                    self.party = (id, members);
                }
                Event::PartyInvite(from) => {
                    self.invites.retain(|p| !p.eq_ignore_ascii_case(&from));
                    self.invites.insert(0, from.clone());
                    self.invites.truncate(4);
                    self.game_events.push(Event::PartyInvite(from));
                }
                e @ (Event::Hosted(_)
                | Event::Reported(_)
                | Event::Unreported(_)
                | Event::BanDone(_)
                | Event::ServerAdmin(_)
                | Event::Arbitrate { .. }
                | Event::Evidence { .. }
                | Event::Kicked(_)
                | Event::Slain(_)
                | Event::SetGamemode { .. }
                | Event::BulkDone { .. }
                | Event::SetTime { .. }
                | Event::SetWeather { .. }
                | Event::Zeus { .. }
                | Event::BuildSpawn { .. }
                | Event::MapProgress(..)
                | Event::MapUploaded(_)
                | Event::MapDownloaded { .. }
                | Event::MapFailed(..)
                | Event::Zones { .. }
                | Event::FromZone { .. }
                | Event::Joined { .. }
                | Event::PartyGo { .. }
                | Event::Skin { .. }
                | Event::SkinChanged(_)
                | Event::Left
                | Event::PeerJoined { .. }
                | Event::PeerLeft(_)
                | Event::RoomClosed
                | Event::Promoted { .. }
                | Event::GarantTask { .. }
                | Event::BackupIs(_)
                | Event::BuildGarant(_)
                | Event::ShardTask { .. }
                | Event::ShardPeers { .. }
                | Event::RootEdits { .. }
                | Event::Shards(_)
                | Event::Province(_)
                | Event::Provinces(_)
                | Event::Judge { .. }
                | Event::Voice { .. }
                | Event::VoiceFrom { .. }
                | Event::HostChanged(_)
                | Event::FromPeer { .. }
                | Event::Witness { .. }
                | Event::Rtt(_)
                | Event::BuildMatch { .. }
                | Event::MatchPeer { .. }
                | Event::Fast(_)
                | Event::FromHost(_)) => self.game_events.push(e),
            }
        }
        if go_home {
            self.client = None;
            self.account = None;
            self.link = None;
            self.identity = None;
            self.lan_relay = self.lan_before;
            self.auto_login();
        }
    }

    /// Commande de partie (héberger, rejoindre, quitter).
    pub fn room(&mut self, cmd: Cmd) {
        // (v146) En veille : quitter le monde n'a rien à dire au relais du monde ; le reste
        // attend qu'on y revienne.
        if self.dozing.is_some() {
            if matches!(cmd, Cmd::Leave) {
                self.end_doze();
            } else {
                if self.doze_queue.len() < 64 {
                    self.doze_queue.push(cmd);
                }
                self.wake();
            }
            return;
        }
        if self.logged_in() {
            self.client().send(cmd);
        }
    }

    /// (v146) Peut-on se mettre en veille (et revenir) : connecté au relais du monde, avec
    /// le certificat et la clé du compte, sans reprise ni pause en cours.
    pub fn can_doze(&self) -> bool {
        self.dozing.is_none()
            && !self.doze_sent
            && self.client.is_some()
            && self.logged_in()
            && self.cert.is_some()
            && self.identity.is_some()
            && self.relay_at.is_some()
            && self.taking_over.is_none()
            && self.probe.is_none()
            && !self.world_paused
    }

    /// (v146) Au fond d'un pays : veille (le relais du monde n'a plus rien à nous dire).
    pub fn doze(&mut self, room: u64) {
        if !self.can_doze() {
            return;
        }
        self.doze_sent = true;
        if let Some(c) = &self.client {
            c.send(Cmd::Doze { room });
        }
    }

    pub fn dozing(&self) -> bool {
        self.dozing.is_some()
    }

    /// (v146) Retour au relais du monde après la veille : connexion neuve, avec le
    /// certificat de la clé du compte (ni mot de passe ni code à redemander).
    pub fn wake(&mut self) {
        let Some(room) = self.dozing else {
            return;
        };
        let now = std::time::Instant::now();
        if self.waking.is_some() || self.wake_next.is_some_and(|t| now < t) {
            return;
        }
        let (Some(cert), Some(key), Some((addr, rkey))) = (
            self.cert.clone(),
            self.identity.clone(),
            self.relay_at.clone(),
        ) else {
            return;
        };
        let client = Client::spawn(addr, Some(rkey));
        client.send(Cmd::Wake { room, cert, key });
        self.client = Some(client);
        self.waking = Some(now);
    }

    /// Retour raté : nouvel essai bientôt ; au bout de quelques-uns, on quitte le monde.
    fn wake_failed(&mut self, why: &str) {
        log::warn!("retour au relais du monde raté : {why}");
        self.client = None;
        self.link = None;
        self.waking = None;
        self.wake_fails += 1;
        if self.wake_fails >= WAKE_GIVE_UP {
            self.end_doze();
            self.game_events.push(Event::RoomClosed);
            return;
        }
        self.wake_next = Some(std::time::Instant::now() + WAKE_RETRY);
    }

    /// (v146) Fin de la veille sans revenir dans le monde (on le quitte) : la session du
    /// compte se rouvre.
    fn end_doze(&mut self) {
        if self.dozing.take().is_none() {
            return;
        }
        self.doze_queue.clear();
        self.waking = None;
        self.wake_next = None;
        self.wake_fails = 0;
        self.client = None;
        self.link = None;
        if self.remembered().is_some() {
            self.auto_login();
        } else {
            self.account = None;
        }
    }

    /// (v240) Connexion par une clé d'accès : un défi du relais d'abord (suite :
    /// `Event::PasskeyChallenge`, puis la fenêtre de Windows dans un fil de fond).
    fn passkey_login_start(&mut self) {
        self.passkey_login = true;
        self.passkey_key = None;
        self.send(Cmd::PasskeyChallenge);
    }

    /// (v240) Nouvelle clé d'accès pour le compte connecté (fenêtre de Windows, dans un fil
    /// de fond ; suite dans `passkey_upkeep`).
    fn passkey_add_start(&mut self) {
        let (Some(a), Some((_, password))) = (self.account.clone(), self.login_secret.clone())
        else {
            self.error = Some("Reconnecte-toi avec ton mot de passe d'abord.".into());
            return;
        };
        self.error = None;
        self.info = None;
        self.busy = Some("Clé d'accès : réponds à la fenêtre de Windows…".into());
        self.passkey_job = Some(crate::passkeys::start_create(a.pseudo, password));
    }

    /// (v240) Réponse de la fenêtre de Windows (fil de fond).
    fn passkey_upkeep(&mut self) {
        let Some(rx) = &self.passkey_job else {
            return;
        };
        let done = match rx.try_recv() {
            Ok(d) => d,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.passkey_job = None;
                self.busy = None;
                return;
            }
        };
        self.passkey_job = None;
        match done {
            crate::passkeys::Done::Created {
                result: Ok(c),
                key,
                keys,
            } => {
                self.busy = Some("Enregistrement de la clé d'accès…".into());
                self.client().send(Cmd::PasskeyAdd {
                    id: c.id,
                    authenticator_data: c.authenticator_data,
                    vault: crypto::seal(&key, &keys),
                    name: crate::passkeys::new_name(),
                });
            }
            crate::passkeys::Done::Got(Ok(a)) => {
                match minelib_net::passkey::parse_user_handle(&a.user_handle) {
                    Some((key, pseudo)) => {
                        self.passkey_key = Some(key);
                        self.pseudo = pseudo.clone();
                        self.busy = Some("Vérification de la clé d'accès…".into());
                        self.client().send(Cmd::PasskeyLogin {
                            pseudo,
                            id: a.id,
                            authenticator_data: a.authenticator_data,
                            signature: a.signature,
                        });
                    }
                    None => {
                        self.busy = None;
                        self.error = Some("Cette clé d'accès n'est pas une clé de Minelib.".into());
                    }
                }
            }
            crate::passkeys::Done::Created { result: Err(e), .. }
            | crate::passkeys::Done::Got(Err(e)) => {
                self.busy = None;
                self.error = Some(e);
            }
        }
    }

    /// (v240) Clé d'accès vérifiée par le relais : son coffre donne les clés du compte, avec
    /// lesquelles on se connecte (comme avec le mot de passe, gardées pour la suite) ; ce PC
    /// est de confiance (pas de 2FA).
    fn passkey_verified(&mut self, pseudo: String, vault: &[u8], device: [u8; 32]) {
        let keys = self
            .passkey_key
            .take()
            .and_then(|k| crypto::open(&k, vault).ok())
            .and_then(|raw| crypto::keys_from_bytes(&raw));
        let Some(keys) = keys else {
            self.busy = None;
            self.error = Some("Coffre de la clé d'accès illisible.".into());
            return;
        };
        self.keep_device(&pseudo, &device);
        let password = crypto::keys_as_password(&keys);
        let backup = std::fs::read(self.backup_path(&pseudo)).ok();
        self.sent = Some((pseudo.clone(), password.clone()));
        self.pseudo = pseudo.clone();
        self.send(Cmd::Login {
            pseudo,
            password,
            totp: None,
            backup,
            device: Some(device),
        });
    }

    /// (v231) Déconnexion voulue : rien des amis de ce compte ne reste (un autre compte peut se
    /// connecter ensuite).
    fn forget_friends(&mut self) {
        self.passkeys.clear();
        self.passkeys_asked = false;
        self.friends.clear();
        self.friend_places.clear();
        self.world_invites.clear();
        self.friend_requests.clear();
        self.new_requests.clear();
        self.requests_seen.clear();
    }

    /// Dossier du relais (le nœud iroh du jeu est gardé à côté).
    /// Peut-on reprendre la partie sur le relais de secours de son monde ?
    fn can_take_over(&self) -> bool {
        self.standby.is_some() && self.cert.is_some() && self.identity.is_some()
    }

    /// Le relais du monde est tombé : nouvelle connexion, au relais de secours du monde
    /// (directement, sinon par un tunnel iroh vers son hôte), avec le certificat de la clé
    /// du compte.
    fn start_takeover(&mut self) {
        if let Some(c) = self.takeover_client() {
            self.client = Some(c);
        }
    }

    /// Connexion au relais de secours du monde, qui demande à y reprendre la partie.
    fn takeover_client(&self) -> Option<Client> {
        let (Some(at), Some(cert), Some(key)) = (
            self.standby.clone(),
            self.cert.clone(),
            self.identity.clone(),
        ) else {
            return None;
        };
        let dir = self.relay_dir();
        let expect = at.clone();
        let client = Client::spawn_resolved(Box::new(move || {
            if let Ok(a) = at.addr.parse::<SocketAddr>() {
                if TcpStream::connect_timeout(&a, Duration::from_millis(800)).is_ok() {
                    return Ok((at.addr.clone(), Some(at.key)));
                }
            }
            let n =
                p2p_node(&dir).ok_or_else(|| anyhow::anyhow!("relais de secours injoignable"))?;
            Ok((n.tunnel(at.node)?.to_string(), Some(at.key)))
        }));
        client.send(Cmd::Takeover {
            cert,
            key,
            expect: Some(expect),
        });
        Some(client)
    }

    /// Quitter un monde : plus de relais de secours à rejoindre.
    pub fn forget_standby(&mut self) {
        self.doze_sent = false;
        self.end_doze();
        self.standby = None;
        self.world_at = None;
        self.taking_over = None;
        self.world_paused = false;
        self.probe = None;
        self.probe_next = None;
    }

    /// Le jeu se ferme : si ce PC tient un relais (relais intégré), les mondes qui ont un
    /// relais de secours lui sont confiés tout de suite (sans attendre que les gardiens
    /// remarquent son silence).
    pub fn release_relay(&self) {
        if let Some(h) = &self.embedded {
            let n = h.release();
            if n > 0 {
                log::info!("relais intégré : {n} monde(s) confié(s) à leur relais de secours");
                // Le temps que les messages partent.
                std::thread::sleep(Duration::from_millis(300));
            }
        }
    }

    pub fn relay_dir(&self) -> PathBuf {
        self.relay_dir.clone()
    }

    /// De quoi ouvrir une deuxième connexion du compte, de garant : relais joint,
    /// identifiants de la session, appareil de confiance (pas de 2FA à redemander).
    pub fn garant_login(&self) -> Option<crate::garant_worker::Login> {
        let votes = Some(self.relay_dir.with_file_name("votes_temoin.txt"));
        // Monde repris : chez le relais qui le sert, par le certificat de la clé du compte.
        if let Some(at) = self.world_at.clone() {
            let (Some(cert), Some(key)) = (self.cert.clone(), self.identity.clone()) else {
                return None;
            };
            return Some(crate::garant_worker::Login {
                addr: at.addr.clone(),
                key: at.key,
                pseudo: cert.pseudo.clone(),
                password: String::new(),
                device: None,
                node_dir: Some(self.relay_dir.clone()),
                votes,
                takeover: Some(crate::garant_worker::CertLogin { at, cert, key }),
            });
        }
        let (addr, key) = self.relay_at.clone()?;
        let (pseudo, password) = self.login_secret.clone().or_else(|| self.remembered())?;
        if !self.logged_in() || !pseudo.eq_ignore_ascii_case(&self.pseudo()) {
            return None;
        }
        let device = self.device(&pseudo);
        Some(crate::garant_worker::Login {
            addr,
            key,
            pseudo,
            password,
            device,
            node_dir: Some(self.relay_dir.clone()),
            votes,
            takeover: None,
        })
    }

    /// (v147) De quoi prêter sa connexion de garant à son pays, qui l'appelle (il choisit
    /// lui-même les hôtes de ses provinces) : par le certificat de la clé du compte, à
    /// l'adresse du relais du pays.
    pub fn pays_garant_login(
        &self,
        call: &proto::GarantCall,
    ) -> Option<crate::garant_worker::Login> {
        let (Some(cert), Some(key)) = (self.cert.clone(), self.identity.clone()) else {
            return None;
        };
        let at = proto::StandbyAt {
            node: call.at.node,
            addr: call.at.addr.clone(),
            key: call.at.key,
            room: 0,
            pseudo: String::new(),
            root: [0; 32],
        };
        Some(crate::garant_worker::Login {
            addr: call.at.addr.clone(),
            key: call.at.key,
            pseudo: cert.pseudo.clone(),
            password: String::new(),
            device: None,
            node_dir: Some(self.relay_dir.clone()),
            votes: Some(self.relay_dir.with_file_name("votes_temoin.txt")),
            takeover: Some(crate::garant_worker::CertLogin { at, cert, key }),
        })
    }

    pub fn pseudo(&self) -> String {
        self.account
            .as_ref()
            .map_or(String::new(), |a| a.pseudo.clone())
    }

    /// Rejoint une partie d'un autre relais (relais fédérés) : le jeu passe sur ce relais
    /// avec le même compte (il y est inscrit avec le coffre gardé sur ce PC s'il n'y est
    /// pas connu), puis entre dans la partie.
    pub fn join_elsewhere(&mut self, e: &ServerEntry) {
        let Some((pseudo, password)) = self.login_secret.clone().or_else(|| self.remembered())
        else {
            self.error = Some("Reconnecte-toi pour rejoindre une partie d'un autre relais.".into());
            return;
        };
        let backup = std::fs::read(self.backup_path(&pseudo)).ok();
        let device = self.device(&pseudo);
        let (addr, key, node) = (e.relay_addr.clone(), e.relay_key, e.relay_node);
        let dir = self.relay_dir.clone();
        log::info!(
            "partie « {} » sur le relais « {} » : on y passe",
            e.name,
            e.region
        );
        // Directement s'il donne une adresse joignable, sinon par un tunnel vers son nœud.
        self.client = Some(Client::spawn_resolved(Box::new(move || {
            if let Ok(a) = addr.parse::<SocketAddr>() {
                if TcpStream::connect_timeout(&a, Duration::from_millis(800)).is_ok() {
                    return Ok((addr.clone(), Some(key)));
                }
            }
            let n = p2p_node(&dir).ok_or_else(|| anyhow::anyhow!("relais injoignable"))?;
            Ok((n.tunnel(node)?.to_string(), Some(key)))
        })));
        // (La clé du relais d'origine reste celle des options.)
        self.lan_before = self.lan_relay;
        self.lan_relay = true;
        self.link = None;
        self.relay_at = None;
        self.servers.clear();
        self.party = (0, Vec::new());
        self.join_after_login = Some(e.id);
        self.sent = Some((pseudo.clone(), password.clone()));
        self.busy = Some(format!("Passage sur le relais « {} »…", e.region));
        if let Some(c) = &self.client {
            c.send(Cmd::Login {
                pseudo,
                password,
                totp: None,
                backup,
                device,
            });
        }
    }

    pub fn refresh_servers(&mut self, kind: u8) {
        if self.logged_in() {
            self.client().send(Cmd::Servers(kind));
        }
    }

    /// Préparation en entrant sur une page.
    pub fn enter(&mut self, page: Page) {
        self.error = None;
        if page != Page::Profile {
            self.info = None;
        }
        self.focus = if page == Page::Login && !self.pseudo.is_empty() {
            1
        } else {
            0
        };
        self.code.clear();
        if page != Page::Login {
            self.need_totp = false;
        }
        if page == Page::TotpSetup {
            self.totp_setup = None;
            self.send(Cmd::TotpBegin);
            self.busy = Some("Création du secret…".into());
        }
    }

    /// Champs de la page : (titre, valeur, masqué, chiffres seulement).
    fn fields(&mut self, page: Page) -> Vec<(&'static str, &mut String, bool, bool)> {
        match page {
            Page::Login => {
                let mut v = vec![
                    ("Pseudo", &mut self.pseudo, false, false),
                    ("Mot de passe", &mut self.password, true, false),
                ];
                if self.need_totp {
                    v.push((
                        "Code de double authentification",
                        &mut self.code,
                        false,
                        true,
                    ));
                }
                v
            }
            Page::Register => vec![
                ("Pseudo", &mut self.pseudo, false, false),
                (
                    "Mot de passe (8 caractères minimum)",
                    &mut self.password,
                    true,
                    false,
                ),
                ("Confirmer le mot de passe", &mut self.confirm, true, false),
            ],
            Page::TotpSetup | Page::TotpDisable => {
                vec![("Code à 6 chiffres", &mut self.code, false, true)]
            }
            _ => Vec::new(),
        }
    }

    pub fn screen(&mut self, c: &mut Ctx, input: &Input, page: Page, time: f32) -> Outcome {
        // Saisie du champ actif, Tab pour passer au suivant.
        let n = self.fields(page).len();
        if n > 0 {
            self.focus = self.focus.min(n - 1);
            if input.pressed(KeyCode::Tab) {
                self.focus = (self.focus + 1) % n;
            }
            let focus = self.focus;
            if let Some((_, value, _, digits)) = self.fields(page).into_iter().nth(focus) {
                let max = if digits {
                    6
                } else if focus == 0 {
                    16
                } else {
                    128
                };
                menu::edit_text(value, input, max);
                if digits {
                    value.retain(|ch| ch.is_ascii_digit());
                }
            }
        }
        c.background();
        let s = c.s;
        let cx = c.w / 2.0;
        let out = match page {
            Page::Welcome => self.welcome(c),
            Page::Login | Page::Register => self.form(c, input, page, time),
            Page::Profile => self.profile(c),
            Page::TotpSetup | Page::TotpDisable => self.totp_page(c, input, page, time),
        };
        // Ligne d'état : en cours, erreur ou information.
        let (msg, color) = if let Some(b) = &self.busy {
            (b.clone(), GOLD)
        } else if let Some(e) = &self.error {
            (e.clone(), RED)
        } else if let Some(i) = &self.info {
            (i.clone(), GREY)
        } else {
            (String::new(), WHITE)
        };
        if !msg.is_empty() {
            let ds = (s / 2.0).floor().max(1.0);
            let scale = if Ui::text_width(&msg, s) < c.w * 0.9 {
                s
            } else {
                ds
            };
            let tw = Ui::text_width(&msg, scale);
            c.ui.text(cx - tw / 2.0, c.h - 74.0 * s, scale, color, msg);
        }
        let ds = (s / 2.0).floor().max(1.0);
        let relay = format!("Relais : {}", self.relay_addr);
        c.ui.text(4.0 * s, c.h - 10.0 * ds - 2.0 * s, ds, GREY, relay);
        out
    }

    fn welcome(&mut self, c: &mut Ctx) -> Outcome {
        let s = c.s;
        c.text_center("MINELIB", c.h * 0.16, s * 4.0, GOLD);
        c.text_center("Compte", c.h * 0.16 + 36.0 * s, s, GREY);
        let cx = c.w / 2.0;
        let mut y = c.h * 0.42;
        let step = 24.0 * s;
        let login = c.button("Se connecter", cx, y, 200.0, true);
        y += step;
        let register = c.button("Créer un compte", cx, y, 200.0, true);
        y += step * 1.5;
        let offline = c.button("Jouer hors ligne (solo)", cx, y, 200.0, true);
        // Compte gardé sur ce PC : la connexion se fait toute seule.
        if self.account.is_some() {
            return Outcome::Leave;
        }
        if let Some((p, _)) = &self.sent {
            c.text_center(
                &format!("Connexion de {p}…"),
                c.h * 0.42 - 20.0 * s,
                s,
                WHITE,
            );
        }
        let ds = (s / 2.0).floor().max(1.0);
        c.text_center(
            "Un pseudo et un mot de passe suffisent : ni e-mail, ni numéro.",
            y + 28.0 * s,
            ds,
            GREY,
        );
        if login {
            Outcome::Goto(Page::Login)
        } else if register {
            Outcome::Goto(Page::Register)
        } else if offline {
            Outcome::Leave
        } else {
            Outcome::None
        }
    }

    fn draw_fields(&mut self, c: &mut Ctx, page: Page, mut y: f32, time: f32) -> f32 {
        let s = c.s;
        let cx = c.w / 2.0;
        let focus = self.focus;
        let mut clicked = None;
        for (i, (label, value, masked, _)) in self.fields(page).into_iter().enumerate() {
            let shown = if masked {
                "*".repeat(value.chars().count())
            } else {
                value.clone()
            };
            if c.text_field(label, &shown, "", cx, y, 220.0, focus == i, time) {
                clicked = Some(i);
            }
            y += 38.0 * s;
        }
        if let Some(i) = clicked {
            self.focus = i;
        }
        y
    }

    fn form(&mut self, c: &mut Ctx, input: &Input, page: Page, time: f32) -> Outcome {
        let s = c.s;
        let cx = c.w / 2.0;
        let title = if page == Page::Login {
            "Se connecter"
        } else {
            "Créer un compte"
        };
        c.text_center(title, 14.0 * s, s * 2.0, WHITE);
        let y = self.draw_fields(c, page, c.h * 0.2, time);
        let ds = (s / 2.0).floor().max(1.0);
        if page == Page::Register {
            c.text_center(
                "Pseudo : 3 à 16 lettres, chiffres ou _. Sans e-mail, un mot de passe",
                y - 6.0 * s,
                ds,
                GREY,
            );
            c.text_center(
                "oublié ne se récupère pas : note-le dans un endroit sûr.",
                y + 4.0 * s,
                ds,
                GREY,
            );
        }
        let idle = self.busy.is_none();
        let ok = idle
            && match page {
                Page::Login => {
                    !self.pseudo.is_empty()
                        && !self.password.is_empty()
                        && (!self.need_totp || self.code.len() == 6)
                }
                _ => {
                    proto::valid_pseudo(&self.pseudo)
                        && proto::valid_password(&self.password)
                        && self.password == self.confirm
                }
            };
        if page == Page::Register
            && !self.confirm.is_empty()
            && self.password != self.confirm
            && idle
        {
            c.text_center(
                "Les deux mots de passe sont différents.",
                y + 16.0 * s,
                ds,
                RED,
            );
        }
        let by = c.h - 48.0 * s;
        let label = if page == Page::Login {
            "Se connecter"
        } else {
            "Créer le compte"
        };
        let go = c.button(label, cx - 80.0 * s, by, 150.0, ok);
        // (v240) Connexion par une clé d'accès (pas de pseudo ni de mot de passe à taper).
        if page == Page::Login
            && crate::passkeys::available()
            && c.button("Avec une clé d'accès", cx, y + 18.0 * s, 310.0, idle)
        {
            self.passkey_login_start();
        }
        let back = c.button("Retour", cx + 80.0 * s, by, 150.0, true);
        if go || (ok && input.pressed(KeyCode::Enter)) {
            let pseudo = self.pseudo.trim().to_string();
            let password = self.password.clone();
            if page == Page::Login {
                let totp = self.need_totp.then(|| self.code.parse().unwrap_or(0));
                let backup = std::fs::read(self.backup_path(&pseudo)).ok();
                self.sent = Some((pseudo.clone(), password.clone()));
                let device = self.device(&pseudo);
                self.send(Cmd::Login {
                    pseudo,
                    password,
                    totp,
                    backup,
                    device,
                });
            } else {
                self.sent = Some((pseudo.clone(), password.clone()));
                self.send(Cmd::Register { pseudo, password });
            }
        }
        if self.account.is_some() {
            return Outcome::Leave;
        }
        if back || input.pressed(KeyCode::Escape) {
            self.password.clear();
            self.confirm.clear();
            return Outcome::Goto(Page::Welcome);
        }
        Outcome::None
    }

    fn profile(&mut self, c: &mut Ctx) -> Outcome {
        let s = c.s;
        let cx = c.w / 2.0;
        c.text_center("Compte", 14.0 * s, s * 2.0, WHITE);
        let Some(a) = self.account.clone() else {
            return Outcome::Goto(Page::Welcome);
        };
        let mut y = c.h * 0.24;
        c.text_center(&a.pseudo, y, s * 2.0, GOLD);
        y += 24.0 * s;
        c.text_center(
            &format!("Empreinte du compte : {}", a.fingerprint),
            y,
            s,
            GREY,
        );
        y += 14.0 * s;
        let state = if a.totp {
            "Double authentification : activée"
        } else {
            "Double authentification : désactivée"
        };
        c.text_center(state, y, s, if a.totp { WHITE } else { GREY });
        y += 24.0 * s;
        let toggle_label = if a.totp {
            "Désactiver la double authentification"
        } else {
            "Activer la double authentification"
        };
        let idle = self.busy.is_none();
        let toggle = c.button(toggle_label, cx, y, 310.0, idle);
        y += 24.0 * s;
        let logout = c.button("Se déconnecter", cx, y, 310.0, idle);
        // (v240) Clés d'accès : les voir, en ajouter, en retirer.
        if !self.passkeys_asked {
            self.passkeys_asked = true;
            self.client().send(Cmd::PasskeyList);
        }
        y += 34.0 * s;
        let ds = (s / 2.0).floor().max(1.0);
        let n = self.passkeys.len();
        let state = match n {
            0 => "Clés d'accès : aucune".to_string(),
            1 => "Clés d'accès : 1".to_string(),
            n => format!("Clés d'accès : {n}"),
        };
        c.text_center(&state, y, s, if n > 0 { WHITE } else { GREY });
        y += 14.0 * s;
        let usable = crate::passkeys::available();
        let can_add = usable && idle && self.login_secret.is_some();
        let add = c.button("Ajouter une clé d'accès", cx, y, 310.0, can_add);
        if !usable {
            c.text_center(
                "Clés d'accès : Windows 10 (1903) ou plus récent.",
                y + 22.0 * s,
                ds,
                GREY,
            );
        } else if n == 0 {
            c.text_center(
                "Windows Hello (visage, empreinte, code PIN), une clé de sécurité ou ton téléphone.",
                y + 22.0 * s,
                ds,
                GREY,
            );
        }
        y += 32.0 * s;
        let mut remove = None;
        let w = 310.0 * s;
        for (i, k) in self.passkeys.iter().enumerate() {
            // Autant qu'il en tient au-dessus de « Retour ».
            if y + 22.0 * s > c.h - 52.0 * s {
                break;
            }
            let used = if k.used > 0 {
                format!(" - dernière connexion le {}", crate::save::format_date(k.used))
            } else {
                String::new()
            };
            let t = crate::menu::fit(&format!("{}{used}", k.name), w - 30.0 * s, ds);
            c.ui.text(cx - w / 2.0, y + 6.0 * s, ds, GREY, t);
            if c.button("x", cx + w / 2.0 - 10.0 * s, y, 20.0, idle) {
                remove = Some(i);
            }
            y += 22.0 * s;
        }
        let back = c.button("Retour", cx, c.h - 48.0 * s, 200.0, true);
        if add {
            self.passkey_add_start();
        }
        if let Some(id) = remove.and_then(|i| self.passkeys.get(i)).map(|k| k.id.clone()) {
            self.busy = Some("Clé d'accès retirée…".into());
            self.client().send(Cmd::PasskeyRemove(id));
        }
        if toggle {
            return Outcome::Goto(if a.totp {
                Page::TotpDisable
            } else {
                Page::TotpSetup
            });
        }
        if logout {
            self.login_secret = None;
            self.client().send(Cmd::Logout);
            self.account = None;
            self.forget_friends();
            // Déconnexion voulue : le compte n'est plus gardé sur ce PC.
            let _ = std::fs::remove_file(self.session_path());
            return Outcome::Goto(Page::Welcome);
        }
        if back {
            return Outcome::Leave;
        }
        Outcome::None
    }

    fn totp_page(&mut self, c: &mut Ctx, input: &Input, page: Page, time: f32) -> Outcome {
        let s = c.s;
        let cx = c.w / 2.0;
        let ds = (s / 2.0).floor().max(1.0);
        let mut y;
        if page == Page::TotpSetup {
            c.text_center("Double authentification", 10.0 * s, s * 2.0, WHITE);
            c.text_center(
                "Scanne ce code avec Authy, Aegis, Google Authenticator, 2FAS ou Bitwarden,",
                30.0 * s,
                ds,
                GREY,
            );
            c.text_center(
                "puis entre le code à 6 chiffres affiché par l'appli.",
                38.0 * s,
                ds,
                GREY,
            );
            y = 48.0 * s;
            if let Some((secret, uri)) = self.totp_setup.clone() {
                let side = (c.h * 0.36).min(c.w * 0.5);
                draw_qr(c.ui, &uri, cx - side / 2.0, y, side);
                y += side + 6.0 * s;
                let key = totp::base32(&secret);
                let grouped: Vec<String> = key
                    .as_bytes()
                    .chunks(4)
                    .map(|g| String::from_utf8_lossy(g).into_owned())
                    .collect();
                c.text_center(&format!("Clé : {}", grouped.join(" ")), y, ds, WHITE);
                y += 16.0 * s;
            }
        } else {
            c.text_center(
                "Désactiver la double authentification",
                14.0 * s,
                s * 2.0,
                WHITE,
            );
            c.text_center(
                "Entre un code de ton appli pour confirmer.",
                40.0 * s,
                ds,
                GREY,
            );
            y = c.h * 0.32;
        }
        let y = y + 11.0 * s;
        self.draw_fields(c, page, y, time);
        let ok = self.busy.is_none()
            && self.code.len() == 6
            && (page == Page::TotpDisable || self.totp_setup.is_some());
        let by = c.h - 48.0 * s;
        let go = c.button("Valider", cx - 80.0 * s, by, 150.0, ok);
        let back = c.button("Annuler", cx + 80.0 * s, by, 150.0, true);
        if go || (ok && input.pressed(KeyCode::Enter)) {
            let code = self.code.parse().unwrap_or(0);
            self.send(if page == Page::TotpSetup {
                Cmd::TotpConfirm(code)
            } else {
                Cmd::TotpDisable(code)
            });
        }
        let done = self
            .account
            .as_ref()
            .is_some_and(|a| a.totp == (page == Page::TotpSetup));
        if done || back || input.pressed(KeyCode::Escape) {
            return Outcome::Goto(Page::Profile);
        }
        Outcome::None
    }
}

/// QR code noir sur blanc (marge comprise) dans un carré de `side` pixels.
fn draw_qr(ui: &mut Ui, data: &str, x: f32, y: f32, side: f32) {
    let Ok(code) = qrcode::QrCode::with_error_correction_level(data, qrcode::EcLevel::M) else {
        return;
    };
    let w = code.width();
    let colors = code.to_colors();
    let quiet = 2;
    let m = (side / (w + quiet * 2) as f32).floor().max(1.0);
    let full = m * (w + quiet * 2) as f32;
    let (x, y) = (x + (side - full) / 2.0, y + (side - full) / 2.0);
    ui.rect(x, y, full, full, WHITE);
    for (i, c) in colors.iter().enumerate() {
        if *c == qrcode::Color::Dark {
            let (cx, cy) = ((i % w + quiet) as f32, (i / w + quiet) as f32);
            ui.rect(x + cx * m, y + cy * m, m, m, [0.0, 0.0, 0.0, 1.0]);
        }
    }
}
