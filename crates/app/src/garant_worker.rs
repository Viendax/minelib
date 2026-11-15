//! Garant pendant qu'on joue : une deuxième connexion du compte au même relais, dans son
//! propre fil, sert (ou vérifie) la partie FPS d'autres joueurs pendant que ce joueur joue
//! la sienne. Le relais le choisit de préférence quand sa partie vient de commencer (il a
//! tout intérêt à rester en jeu) et le fait relever par un autre garant avant qu'elle ne
//! se termine. Rien ne passe par le fil du jeu : les images du joueur ne ralentissent pas.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use minelib_net::client::{Client, Cmd, Event};

use crate::garant::Garant;

/// De quoi ouvrir la connexion de garant (voir `Accounts::garant_login`).
#[derive(Clone)]
pub struct Login {
    pub addr: String,
    pub key: [u8; 32],
    pub pseudo: String,
    pub password: String,
    pub device: Option<[u8; 32]>,
}

pub struct GarantWorker {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl GarantWorker {
    /// Lance le fil. `maps` : cartes de ce PC (signature, dossier) ; `maps_root` : où
    /// ranger une carte téléchargée.
    pub fn start(login: Login, maps: Vec<([u8; 32], PathBuf)>, maps_root: PathBuf) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = std::thread::Builder::new()
            .name("garant".into())
            .spawn(move || run(login, maps, maps_root, flag))
            .ok();
        Self { stop, thread }
    }

    /// Le fil tourne encore.
    pub fn alive(&self) -> bool {
        self.thread.as_ref().is_some_and(|t| !t.is_finished())
    }
}

impl Drop for GarantWorker {
    /// Fin de sa propre partie : il rend la partie servie (le garant en attente reprend) et
    /// ferme sa connexion de garant.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn local_dir(
    maps: &[([u8; 32], PathBuf)],
    root: &std::path::Path,
    sig: &[u8; 32],
) -> Option<PathBuf> {
    if let Some((_, d)) = maps.iter().find(|(s, _)| s == sig) {
        return Some(d.clone());
    }
    let dir = crate::fpsmap::downloaded_dir(root, sig);
    dir.join("level.txt").exists().then_some(dir)
}

fn run(login: Login, maps: Vec<([u8; 32], PathBuf)>, root: PathBuf, stop: Arc<AtomicBool>) {
    let client = Client::spawn_garant(login.addr.clone(), login.key);
    client.send(Cmd::Login {
        pseudo: login.pseudo.clone(),
        password: login.password.clone(),
        totp: None,
        backup: None,
        device: login.device,
    });
    let mut logged = false;
    let mut link = None;
    let mut garant: Option<Garant> = None;
    // Garant extérieur d'un monde de construction : il valide les modifications de ses
    // zones (sans avoir besoin du monde lui-même).
    let mut build: Option<(crate::buildnet::BuildNet, minelib_world::World)> = None;
    let mut last = Instant::now();
    let started = Instant::now();
    let mut wiped = Instant::now();
    log::info!("garant en jeu : connexion de garant ouverte");
    loop {
        if stop.load(Ordering::Relaxed) {
            if garant.is_some() || build.is_some() {
                client.send(Cmd::Leave);
            }
            client.send(Cmd::GarantAvailable(false));
            client.send(Cmd::Logout);
            // Le temps que ces messages partent.
            std::thread::sleep(Duration::from_millis(300));
            log::info!("garant en jeu : connexion de garant fermée");
            return;
        }
        while let Some(e) = client.poll() {
            match e {
                Event::LoggedIn { .. } => {
                    logged = true;
                    client.send(Cmd::GarantAvailable(true));
                }
                Event::Link(l) => link = Some(l),
                Event::NeedTotp | Event::Offline(_) if !logged => {
                    log::info!("garant en jeu : connexion impossible, abandon");
                    return;
                }
                Event::GarantTask {
                    room,
                    map_sig,
                    map,
                    members,
                    standby,
                } => {
                    let Some(l) = link.clone() else {
                        client.send(Cmd::Leave);
                        continue;
                    };
                    let dir = local_dir(&maps, &root, &map_sig);
                    if dir.is_none() {
                        client.send(Cmd::DownloadMap(map_sig));
                    }
                    garant = Some(Garant::new(
                        room,
                        map_sig,
                        map,
                        members,
                        standby,
                        l,
                        login.pseudo.clone(),
                        dir,
                    ));
                }
                Event::Arbitrate { case, map_sig, map } if garant.is_none() => {
                    if let Some(l) = link.clone() {
                        let dir = local_dir(&maps, &root, &map_sig);
                        if dir.is_none() {
                            client.send(Cmd::DownloadMap(map_sig));
                        }
                        garant = Some(Garant::arbiter(case, map_sig, map, l, dir));
                    }
                }
                Event::Evidence {
                    case,
                    part,
                    of,
                    ref data,
                } => {
                    if let Some(g) = &mut garant {
                        g.evidence(case, part, of, data);
                    }
                }
                Event::MapDownloaded { sig, ref bytes } => {
                    if let Some(g) = garant.as_mut().filter(|g| g.needs_map() && g.sig == sig) {
                        match crate::fpsmap::unpack(&root, &g.map, bytes) {
                            Ok(dir) => g.set_dir(dir),
                            Err(e) => log::warn!("garant en jeu : carte : {e:#}"),
                        }
                    }
                }
                Event::RoomClosed => {
                    garant = None;
                    build = None;
                }
                Event::BuildGarant(room) => {
                    if let Some(l) = link.clone() {
                        log::info!("garant en jeu : zones du monde {room}");
                        let net = crate::buildnet::BuildNet::new(l, login.pseudo.clone(), false);
                        build = Some((net, minelib_world::World::new(0)));
                    }
                }
                Event::Zones {
                    ref gained,
                    ref lost,
                } => {
                    if let Some((b, _)) = &mut build {
                        b.zones_changed(gained, lost);
                    }
                }
                Event::FromZone {
                    peer,
                    chunk,
                    ref data,
                } => {
                    if let Some((b, w)) = &mut build {
                        b.receive(peer, chunk, data, w);
                    }
                }
                Event::PeerJoined { .. }
                | Event::PeerLeft(_)
                | Event::FromPeer { .. }
                | Event::Witness { .. }
                | Event::FromHost(_)
                | Event::Promoted { .. }
                | Event::HostChanged(_)
                | Event::BackupIs(_) => {
                    if let Some(g) = &mut garant {
                        g.event(&e);
                    }
                }
                _ => {}
            }
        }
        // Pas de réponse du relais : on n'insiste pas.
        if !logged && started.elapsed() > Duration::from_secs(30) {
            log::info!("garant en jeu : pas de session, abandon");
            return;
        }
        // Le monde du garant extérieur ne sert à rien (il ne fait que valider) : on vide
        // de temps en temps les modifications qu'il a gardées.
        if let Some((_, w)) = &mut build {
            if wiped.elapsed() > Duration::from_secs(600) {
                wiped = Instant::now();
                *w = minelib_world::World::new(0);
            }
        }
        let dt = last.elapsed().as_secs_f32().min(0.1);
        last = Instant::now();
        if let Some(g) = &mut garant {
            for cmd in g.update(dt) {
                client.send(cmd);
            }
            if g.done {
                garant = None;
            }
        }
        std::thread::sleep(Duration::from_millis(if garant.is_some() { 2 } else { 20 }));
    }
}
