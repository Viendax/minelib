//! (v320) Passeur sans joueur : `minelib-relay --passeur`.
//!
//! Un passeur porte en UDP les paquets chiffrés d'autres joueurs (voie rapide des parties
//! FPS, tunnels vers les relais de région, lien avec le relais du monde) sans pouvoir les
//! lire, mis en chaîne par deux pour qu'aucun ne voie les deux bouts. Jusqu'ici, il fallait
//! qu'un joueur accepte que son jeu en soit un, et qu'il soit en train de jouer. Sans
//! passeurs, tout passe par les serveurs relais d'iroh, un tiers au débit limité (45 Ko/s
//! mesurés) : de quoi se connecter, pas de jouer en FPS à 256 tick.
//!
//! Ce mode fait du programme du relais un passeur seul, pour une machine toujours allumée
//! (serveur, machine virtuelle gratuite d'un hébergeur, Raspberry Pi) : il se connecte aux
//! relais qu'il trouve, y ouvre un compte de passeur (gardé dans son dossier, rouvert aux
//! lancements suivants) et s'y annonce avec son adresse publique. Ce qu'il voit : l'adresse
//! IP d'un bout de chaque route qu'il porte, comme le passeur d'un joueur (jamais les deux
//! bouts d'une même route, ni qui ils sont).

use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use minelib_net::client::{Client, Cmd, Event};

/// Trouve l'adresse locale (tunnel, ou adresse directe) d'un relais.
pub type Find = Arc<dyn Fn() -> anyhow::Result<String> + Send + Sync>;

/// Réglages : dossier (comptes de passeur, nœud iroh), adresse publique, premier port UDP
/// (0 : n'importe lesquels ; sinon un port par relais à partir de celui-ci).
pub struct Options {
    pub dir: PathBuf,
    pub ip: Ipv4Addr,
    pub port: u16,
}

/// Un relais servi.
struct Slot {
    name: String,
    key: [u8; 32],
    find: Find,
    port: u16,
    client: Option<Client>,
    joined: bool,
    /// Compte en train d'être créé (pseudo, mot de passe).
    making: Option<(String, String)>,
    next: Instant,
    fails: u32,
    /// Connexions ratées avec le compte gardé, de suite (au-delà, il est refait).
    refused: u32,
    /// (v345) Invitation de ce relais à lui redonner à chaque connexion (`--invitation`) :
    /// son compte y devient un passeur de confiance, qui porte aussi les parties FPS.
    invite: Option<[u8; 16]>,
    /// (v345) Invitation donnée, réponse attendue avant de s'annoncer.
    claiming: bool,
}

pub struct Headless {
    opts: Options,
    slots: Vec<Slot>,
    /// (v345) Invitations, par clé de relais (`invite`).
    invites: Vec<([u8; 32], [u8; 16])>,
}

/// Comptes de passeur, un par relais (clé du relais, pseudo, mot de passe).
fn accounts_path(dir: &std::path::Path) -> PathBuf {
    dir.join("comptes_passeur.txt")
}

fn read_accounts(dir: &std::path::Path) -> Vec<([u8; 32], String, String)> {
    std::fs::read_to_string(accounts_path(dir))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let key: [u8; 32] = minelib_net::crypto::unhex(it.next()?)?.try_into().ok()?;
            Some((key, it.next()?.to_string(), it.next()?.to_string()))
        })
        .collect()
}

fn write_accounts(dir: &std::path::Path, list: &[([u8; 32], String, String)]) {
    let text: String = list
        .iter()
        .map(|(k, p, w)| format!("{} {p} {w}\n", minelib_net::crypto::hex(k)))
        .collect();
    let _ = std::fs::write(accounts_path(dir), text);
}

/// Pseudo et mot de passe neufs d'un compte de passeur.
fn fresh_account() -> (String, String) {
    use rand::Rng;
    const A: &[u8] = b"abcdefghijkmnpqrstuvwxyz23456789";
    let mut r = rand::thread_rng();
    let mut pick = |n: usize| -> String {
        (0..n).map(|_| A[r.gen_range(0..A.len())] as char).collect()
    };
    (format!("Ps{}", pick(10)), pick(24))
}

/// Attente avant de rejoindre de nouveau un relais : 15 s, puis le double, 10 min au plus.
fn backoff(fails: u32) -> Duration {
    Duration::from_secs(15u64 << fails.saturating_sub(1).min(6)).min(Duration::from_secs(600))
}

impl Headless {
    pub fn new(opts: Options) -> Self {
        let _ = std::fs::create_dir_all(&opts.dir);
        minelib_net::fast::set_passeur_allowed(true);
        Self {
            opts,
            slots: Vec::new(),
            invites: Vec::new(),
        }
    }

    /// (v345) Invitation du relais de clé `key` (`minelib_p2p::parse_invitation`) : donnée à
    /// chaque connexion à ce relais, avant de s'y annoncer.
    pub fn invite(&mut self, key: [u8; 32], secret: [u8; 16]) {
        self.invites.retain(|(k, _)| *k != key);
        self.invites.push((key, secret));
        for s in self.slots.iter_mut().filter(|s| s.key == key) {
            s.invite = Some(secret);
        }
    }

    /// Un relais à servir (une fois par clé).
    pub fn add(&mut self, name: &str, key: [u8; 32], find: Find) {
        if self.slots.iter().any(|s| s.key == key) {
            return;
        }
        let port = match self.opts.port {
            0 => 0,
            p => p.saturating_add(self.slots.len() as u16),
        };
        self.slots.push(Slot {
            name: name.to_string(),
            key,
            find,
            port,
            client: None,
            joined: false,
            making: None,
            next: Instant::now(),
            fails: 0,
            refused: 0,
            invite: self
                .invites
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, s)| *s),
            claiming: false,
        });
    }

    /// Relais où ce passeur est entré et annoncé.
    pub fn joined(&self) -> usize {
        self.slots.iter().filter(|s| s.joined).count()
    }

    pub fn relays(&self) -> usize {
        self.slots.len()
    }

    /// À appeler souvent : entre aux relais, s'y annonce, y retourne après une coupure.
    pub fn tend(&mut self) {
        let (dir, ip) = (self.opts.dir.clone(), self.opts.ip);
        for s in &mut self.slots {
            s.tend(&dir, ip);
        }
    }
}

impl Slot {
    fn tend(&mut self, dir: &std::path::Path, ip: Ipv4Addr) {
        let now = Instant::now();
        let Some(c) = &self.client else {
            if now < self.next {
                return;
            }
            let (find, key) = (self.find.clone(), self.key);
            let c = Client::spawn_resolved(Box::new(move || Ok((find()?, Some(key)))));
            c.send(Cmd::PasseurAt {
                ip: Some(ip),
                port: self.port,
            });
            let kept = read_accounts(dir).into_iter().find(|a| a.0 == key);
            match kept {
                Some((_, pseudo, password)) => c.send(Cmd::Login {
                    pseudo,
                    password,
                    totp: None,
                    backup: None,
                    device: None,
                }),
                None => {
                    let (pseudo, password) = fresh_account();
                    self.making = Some((pseudo.clone(), password.clone()));
                    c.send(Cmd::Register { pseudo, password });
                }
            }
            self.client = Some(c);
            return;
        };
        let mut failed: Option<String> = None;
        while let Some(e) = c.poll() {
            match e {
                Event::LoggedIn { pseudo, .. } => {
                    if let Some((p, w)) = self.making.take() {
                        let mut list = read_accounts(dir);
                        list.retain(|a| a.0 != self.key);
                        list.push((self.key, p, w));
                        write_accounts(dir, &list);
                    }
                    // (v345) Son invitation d'abord : s'annoncer ensuite, une fois la réponse
                    // venue (de confiance ou non, il porte au moins les tunnels).
                    match self.invite {
                        Some(secret) => {
                            c.send(Cmd::ServerAdmin {
                                action: minelib_net::proto::SERVER_PASSEUR_CLAIM,
                                pseudo: String::new(),
                                reason: minelib_net::crypto::hex(&secret),
                            });
                            self.claiming = true;
                        }
                        None => c.send(Cmd::Passeur(true)),
                    }
                    self.joined = true;
                    self.fails = 0;
                    self.refused = 0;
                    log::info!("passeur : entré au relais « {} » ({pseudo})", self.name);
                }
                Event::ServerAdmin(text) if self.claiming => {
                    self.claiming = false;
                    if text == minelib_net::proto::PASSEUR_TRUSTED {
                        log::info!("passeur : de confiance au relais « {} »", self.name);
                    } else {
                        log::warn!("passeur : invitation du relais « {} » : {text}", self.name);
                    }
                    c.send(Cmd::Passeur(true));
                }
                // Un relais d'avant la v345 ne connaît pas les invitations : passeur quand même.
                Event::Error(m) if self.claiming => {
                    self.claiming = false;
                    log::warn!("passeur : invitation du relais « {} » : {m}", self.name);
                    c.send(Cmd::Passeur(true));
                }
                Event::Error(m) | Event::Offline(m) => failed = Some(m),
                Event::NeedTotp => failed = Some("code 2FA demandé".into()),
                Event::Lost | Event::LoggedOut => failed = Some("connexion perdue".into()),
                _ => {}
            }
            if failed.is_some() {
                break;
            }
        }
        if let Some(why) = failed {
            // Un compte gardé refusé plusieurs fois de suite (relais remis à zéro) : refait.
            if !self.joined && self.making.is_none() {
                self.refused += 1;
                if self.refused >= 3 {
                    let mut list = read_accounts(dir);
                    list.retain(|a| a.0 != self.key);
                    write_accounts(dir, &list);
                    self.refused = 0;
                }
            }
            self.making = None;
            self.client = None;
            self.joined = false;
            self.claiming = false;
            self.fails += 1;
            let wait = backoff(self.fails);
            self.next = now + wait;
            log::info!(
                "passeur : relais « {} » : {why} — de nouveau dans {} s",
                self.name,
                wait.as_secs()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{start, Config};

    /// (v320) Un passeur sans joueur entre à un relais avec un compte neuf, s'y annonce à
    /// l'adresse donnée et y est vérifié ; relancé, il y retourne avec le même compte.
    #[test]
    fn a_headless_passeur_joins_a_relay_and_comes_back_with_its_account() {
        let dir = std::env::temp_dir().join(format!("minelib-passeur-seul-{}", rand::random::<u32>()));
        let h = start(Config {
            dir: dir.join("relais"),
            bind: "127.0.0.1:0".into(),
            pow_bits: 4,
            name: "essai".into(),
        })
        .unwrap();
        let addr = h.addr.to_string();
        let mine = dir.join("passeur");
        let run = |port: u16| {
            let mut p = Headless::new(Options {
                dir: mine.clone(),
                ip: Ipv4Addr::LOCALHOST,
                port,
            });
            let a = addr.clone();
            p.add("essai", h.public, Arc::new(move || Ok(a.clone())));
            let t = Instant::now();
            while p.joined() < 1 || h.state.lock().unwrap().fast.verified() < 1 {
                p.tend();
                assert!(t.elapsed() < Duration::from_secs(30), "passeur pas vérifié");
                std::thread::sleep(Duration::from_millis(5));
            }
            p
        };
        let first = run(0);
        let kept = read_accounts(&mine);
        assert_eq!(kept.len(), 1);
        assert!(minelib_net::proto::valid_pseudo(&kept[0].1));
        drop(first);
        let t = Instant::now();
        while h.state.lock().unwrap().fast.verified() != 0 {
            assert!(t.elapsed() < Duration::from_secs(20));
            std::thread::sleep(Duration::from_millis(5));
        }
        // Relancé, sur un port choisi : même compte, rien de créé.
        let port = std::net::UdpSocket::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let _second = run(port);
        assert_eq!(read_accounts(&mine), kept);
        let st = h.state.lock().unwrap();
        assert!(st.fast.offered_at(port), "annoncé sur le port voulu");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// (v345) Un passeur sans joueur installé avec une invitation (`--invitation`) la donne
    /// au relais qui l'a faite : son compte y devient de confiance, puis il s'annonce comme
    /// d'habitude. Relancé, il la redonne sans rien user de plus.
    #[test]
    fn a_headless_passeur_with_an_invitation_becomes_trusted() {
        let dir =
            std::env::temp_dir().join(format!("minelib-passeur-invite-{}", rand::random::<u32>()));
        std::fs::create_dir_all(dir.join("relais")).unwrap();
        std::fs::write(dir.join("relais").join("admins.txt"), "Chef\n").unwrap();
        let h = start(Config {
            dir: dir.join("relais"),
            bind: "127.0.0.1:0".into(),
            pow_bits: 4,
            name: "essai".into(),
        })
        .unwrap();
        let addr = h.addr.to_string();
        let chef = Client::spawn(addr.clone(), Some(h.public));
        chef.send(Cmd::Register {
            pseudo: "Chef".into(),
            password: "motdepasse".into(),
        });
        let t = Instant::now();
        let mut secret = None;
        let mut asked = false;
        while secret.is_none() {
            match chef.poll() {
                Some(Event::LoggedIn { .. }) if !asked => {
                    asked = true;
                    chef.send(Cmd::ServerAdmin {
                        action: minelib_net::proto::SERVER_PASSEUR_INVITE,
                        pseudo: String::new(),
                        reason: "1".into(),
                    });
                }
                // (L'invitation a son événement à elle, que le jeu copie et enregistre.)
                Some(Event::PasseurInvitation(text)) => secret = Some(text),
                Some(Event::Error(m)) => panic!("{m}"),
                _ => std::thread::sleep(Duration::from_millis(2)),
            }
            assert!(t.elapsed() < Duration::from_secs(30), "pas d'invitation");
        }
        let bytes: [u8; 16] = minelib_net::crypto::unhex(&secret.unwrap())
            .unwrap()
            .try_into()
            .unwrap();
        let mine = dir.join("passeur");
        let run = || {
            let mut p = Headless::new(Options {
                dir: mine.clone(),
                ip: Ipv4Addr::LOCALHOST,
                port: 0,
            });
            p.invite(h.public, bytes);
            let a = addr.clone();
            p.add("essai", h.public, Arc::new(move || Ok(a.clone())));
            let t = Instant::now();
            while p.joined() < 1 || h.state.lock().unwrap().fast.verified() < 1 {
                p.tend();
                assert!(t.elapsed() < Duration::from_secs(30), "passeur pas vérifié");
                std::thread::sleep(Duration::from_millis(5));
            }
            p
        };
        let first = run();
        let pseudo = read_accounts(&mine)[0].1.clone();
        assert!(h.state.lock().unwrap().passeurs_ok.trusts(&pseudo));
        drop(first);
        let t = Instant::now();
        while h.state.lock().unwrap().fast.verified() != 0 {
            assert!(t.elapsed() < Duration::from_secs(20));
            std::thread::sleep(Duration::from_millis(5));
        }
        // Relancé : la même invitation (épuisée pour tout autre) ne lui retire rien.
        let _second = run();
        let st = h.state.lock().unwrap();
        assert!(st.passeurs_ok.trusts(&pseudo));
        assert_eq!(st.passeurs_ok.list().len(), 1);
        drop(st);
        drop(chef);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
