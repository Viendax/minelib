//! Compte du joueur : écrans d'accueil (se connecter, créer un compte, jouer hors ligne),
//! profil et double authentification. Le dialogue avec le relais se fait dans un fil de
//! fond (`minelib_net::client`) : l'interface ne se fige jamais.
//!
//! Si le relais configuré est sur ce PC (127.0.0.1) et ne répond pas, le jeu en lance un
//! lui-même, avec ses données dans `.minelib/relay`.

use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::time::Duration;

use minelib_net::client::{Client, Cmd, Event};
use minelib_net::proto::{self, ServerEntry};
use minelib_net::{crypto, totp};
use minelib_platform::{Input, KeyCode};
use minelib_render::Ui;

use crate::menu::{self, Ctx, GOLD, GREY, RED, WHITE};

/// Nœud iroh du jeu (un seul, gardé pour la durée du jeu).
fn node() -> Option<&'static minelib_p2p::Node> {
    static NODE: std::sync::OnceLock<Option<minelib_p2p::Node>> = std::sync::OnceLock::new();
    NODE.get_or_init(|| match minelib_p2p::Node::start() {
        Ok(n) => Some(n),
        Err(e) => {
            log::warn!("iroh : {e:#}");
            None
        }
    })
    .as_ref()
}

/// Trouve un relais sans réglage : sur ce PC, sur le réseau local, puis dans le DHT de
/// BitTorrent (le plus ancien en ligne, par un tunnel iroh) ; sinon ce PC devient le
/// relais (visible du réseau local et inscrit dans le DHT).
fn auto_relay(
    dir: &std::path::Path,
    failed: &mut Vec<[u8; 32]>,
) -> anyhow::Result<(String, Option<[u8; 32]>)> {
    let port = minelib_net::DEFAULT_PORT;
    let local = SocketAddr::from(([127, 0, 0, 1], port));
    if TcpStream::connect_timeout(&local, Duration::from_millis(300)).is_ok() {
        return Ok((local.to_string(), None));
    }
    if let Some((lan, key)) = minelib_net::lan::discover(Duration::from_millis(1500)) {
        log::info!("relais trouvé sur le réseau local : {lan}");
        return Ok((lan.to_string(), Some(key)));
    }
    if let Some(node) = node() {
        match minelib_p2p::find_relays() {
            Ok(list) => {
                if let Some(ad) = list
                    .iter()
                    .find(|a| !failed.contains(&a.node) && a.node != node.id())
                {
                    failed.push(ad.node);
                    let addr = node.tunnel(ad.node)?;
                    log::info!(
                        "relais « {} » trouvé dans le DHT (nœud {})",
                        ad.name,
                        crypto::hex(&ad.node[..8])
                    );
                    return Ok((addr.to_string(), Some(ad.noise)));
                }
            }
            Err(e) => log::warn!("DHT : {e:#}"),
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
    log::info!("ce PC devient le relais ({})", h.addr);
    minelib_net::lan::announce(h.addr.port(), h.public);
    let mine = SocketAddr::from(([127, 0, 0, 1], h.addr.port()));
    if let Some(node) = node() {
        node.serve(mine);
        // Pseudos uniques avec les autres relais du DHT.
        h.set_whois(minelib_relay::federated_whois(node, h.public));
        minelib_p2p::publish(minelib_p2p::Ad {
            node: node.id(),
            noise: h.public,
            name: "Relais d'un joueur".into(),
            since: minelib_p2p::now(),
            seen: minelib_p2p::now(),
        });
    }
    Ok((mine.to_string(), Some(h.public)))
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
    /// Événements de partie (hôte, invité) à transmettre au jeu.
    pub game_events: Vec<Event>,
    /// Classement reçu : (amis ?, lignes, sa propre ligne).
    pub leaderboard: Option<(bool, Vec<proto::RankEntry>, Option<proto::RankEntry>)>,
    pub friends: Vec<String>,
    pub maps: Vec<proto::MapEntry>,
    /// Relais d'un autre PC du réseau local (sa clé n'est pas retenue).
    lan_relay: bool,
}

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
            game_events: Vec::new(),
            leaderboard: None,
            friends: Vec::new(),
            maps: Vec::new(),
            lan_relay: false,
        }
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
    pub fn poll(&mut self) {
        let Some(client) = &self.client else { return };
        let mut events = Vec::new();
        while let Some(e) = client.poll() {
            events.push(e);
        }
        for e in events {
            match e {
                Event::Pinned(k) => {
                    if !self.lan_relay {
                        self.relay_key = Some(k);
                        self.new_relay_key = Some(crypto::hex(&k));
                    }
                }
                Event::Progress(p) => self.busy = Some(p.into()),
                Event::LoggedIn {
                    pseudo,
                    fingerprint,
                    totp,
                } => {
                    self.busy = None;
                    self.need_totp = false;
                    self.password.clear();
                    self.confirm.clear();
                    self.code.clear();
                    self.new_pseudo = Some(pseudo.clone());
                    self.account = Some(Account {
                        pseudo,
                        fingerprint,
                        totp,
                    });
                }
                Event::NeedTotp => {
                    self.busy = None;
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
                Event::LoggedOut => {
                    self.busy = None;
                    self.account = None;
                }
                Event::Error(m) => {
                    self.busy = None;
                    self.error = Some(m.clone());
                    self.game_events.push(Event::Error(m));
                }
                Event::Link(l) => self.link = Some(l),
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
                Event::Friends(list) => {
                    self.busy = None;
                    self.friends = list.clone();
                    self.game_events.push(Event::Friends(list));
                }
                Event::Maps(list) => {
                    self.busy = None;
                    self.maps = list;
                }
                e @ (Event::Hosted(_)
                | Event::Reported(_)
                | Event::Unreported(_)
                | Event::BanDone(_)
                | Event::ServerAdmin(_)
                | Event::Kicked(_)
                | Event::MapProgress(..)
                | Event::MapUploaded(_)
                | Event::MapDownloaded { .. }
                | Event::MapFailed(..)
                | Event::Zones { .. }
                | Event::FromZone { .. }
                | Event::Joined { .. }
                | Event::Left
                | Event::PeerJoined { .. }
                | Event::PeerLeft(_)
                | Event::RoomClosed
                | Event::Promoted { .. }
                | Event::GarantTask { .. }
                | Event::BackupIs(_)
                | Event::Judge { .. }
                | Event::Voice { .. }
                | Event::HostChanged(_)
                | Event::FromPeer { .. }
                | Event::Witness { .. }
                | Event::FromHost(_)) => self.game_events.push(e),
            }
        }
    }

    /// Commande de partie (héberger, rejoindre, quitter).
    pub fn room(&mut self, cmd: Cmd) {
        if self.logged_in() {
            self.client().send(cmd);
        }
    }

    pub fn pseudo(&self) -> String {
        self.account
            .as_ref()
            .map_or(String::new(), |a| a.pseudo.clone())
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
        let back = c.button("Retour", cx + 80.0 * s, by, 150.0, true);
        if go || (ok && input.pressed(KeyCode::Enter)) {
            let pseudo = self.pseudo.trim().to_string();
            let password = self.password.clone();
            if page == Page::Login {
                let totp = self.need_totp.then(|| self.code.parse().unwrap_or(0));
                let backup = std::fs::read(self.backup_path(&pseudo)).ok();
                self.send(Cmd::Login {
                    pseudo,
                    password,
                    totp,
                    backup,
                });
            } else {
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
        let back = c.button("Retour", cx, c.h - 48.0 * s, 200.0, true);
        if toggle {
            return Outcome::Goto(if a.totp {
                Page::TotpDisable
            } else {
                Page::TotpSetup
            });
        }
        if logout {
            self.client().send(Cmd::Logout);
            self.account = None;
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
