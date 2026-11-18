//! Canal chiffré sur TCP (protocole Noise XX : Curve25519, ChaCha20-Poly1305, BLAKE2s).
//!
//! Chaque message : longueur sur 2 octets puis le message chiffré. Le relais a une clé
//! fixe ; le jeu la retient au premier contact et refuse ensuite un relais qui en change
//! (protection contre un faux relais placé entre les deux).

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use serde::de::DeserializeOwned;
use serde::Serialize;

/// Motif Noise de la poignée de main.
pub const PATTERN: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";
/// Taille maximale d'un message Noise.
pub const MAX: usize = 65535;

/// Côté relais : trames reçues d'un client et pas encore traitées au-delà desquelles il
/// est coupé (il envoie plus vite que le relais ne peut suivre : sa file gonflerait sans
/// fin).
pub const FLOOD_BACKLOG: u32 = 20_000;
/// Côté relais : un client qui ne lit plus rien pendant ce temps est coupé.
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(20);
/// Un message périssable (`push_loose` : la position d'un joueur, que la suivante
/// remplace) resté plus longtemps que cela dans la file n'est plus envoyé : un client lent
/// ne reçoit pas des positions vieilles de plusieurs secondes, et sa file ne gonfle pas.
pub const LOOSE_MAX_AGE: Duration = Duration::from_millis(250);
/// Les messages à envoyer sont regroupés : ils partent ensemble dès que la file est vide,
/// ou dès que le tampon atteint cette taille. Dans une foule, des centaines de positions
/// par seconde vont à chaque joueur ; une par paquet, chacune coûtait 40 octets
/// d'en-têtes et un appel au système.
pub const OUT_MAX: usize = 32 * 1024;
/// Les blocs changés (`push_cell`) partent regroupés en paquets compacts : le premier tout
/// de suite, les suivants au plus tôt ce temps après le paquet d'avant. Un joueur au
/// milieu d'une foule qui construit reçoit ainsi 50 paquets par seconde au plus, quel que
/// soit le nombre de blocs, et rien n'attend quand il y en a peu.
///
/// Les positions de joueurs (`push_pose`) partent de la même façon, avec eux : dans une
/// foule c'est de loin le plus gros du trafic (500 positions par seconde et par joueur).
const CELL_LINGER: Duration = Duration::from_millis(20);
/// Le pseudo d'un joueur est redit avec sa position au plus tôt ce temps après la dernière
/// fois (le jeu qui reçoit le retient entre-temps ; s'il l'a oublié, il le retrouve vite).
const NAME_EVERY: Duration = Duration::from_secs(5);

/// Position d'un joueur à remettre aux autres : son numéro sur ce relais, son pseudo
/// (celui de sa session), où il est.
pub struct PoseOf {
    pub id: u64,
    pub name: String,
    pub pose: crate::poses::Pose,
}

/// Ce qui arrive sur un canal : message chiffré reçu du réseau, message (en clair) déposé
/// par un autre fil pour être envoyé, ou fin de connexion.
pub enum Inbound {
    Frame(Vec<u8>),
    Push(Vec<u8>),
    /// Message périssable déposé à cet instant (voir `LOOSE_MAX_AGE`).
    Loose(Vec<u8>, Instant),
    /// Bloc changé, à envoyer regroupé avec les autres (voir `CELL_LINGER`).
    Cell(crate::cells::Cell),
    /// Position d'un joueur déposée à cet instant, à envoyer regroupée avec les autres
    /// (périssable : la suivante du même joueur la remplace).
    Pose(std::sync::Arc<PoseOf>, Instant),
    Closed,
}

/// Résultat de `Channel::recv_any`.
pub enum Incoming<T> {
    Msg(T),
    /// Message déposé par un autre fil (`push`), à envoyer tel quel avec `send_raw`.
    Push(Vec<u8>),
}

pub struct Channel {
    /// Écriture (la lecture est faite par un fil dédié, qui dépose les trames dans `rx`).
    stream: TcpStream,
    noise: snow::TransportState,
    rx: Receiver<Inbound>,
    tx: Sender<Inbound>,
    /// Trames lues du réseau pas encore traitées.
    backlog: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    /// Trames chiffrées prêtes à partir, pas encore écrites (voir `OUT_MAX`).
    out: Vec<u8>,
    /// Blocs et positions à regrouper.
    group: Grouper,
    /// Message sorti de la file en ramassant les blocs, à traiter ensuite.
    held: Option<Inbound>,
}

/// Où déposer un message à envoyer sur une connexion : la file d'un canal (`Channel`), ou
/// celle d'une connexion servie autrement (tâches du relais du monde).
pub trait Sink {
    /// Dépose ; faux si la connexion est fermée.
    fn put(&self, m: Inbound) -> bool;
}

impl Sink for Sender<Inbound> {
    fn put(&self, m: Inbound) -> bool {
        self.send(m).is_ok()
    }
}

/// Blocs changés (`push_cell`) et positions de joueurs (`push_pose`) en attente d'être
/// regroupés en paquets compacts, et paquets prêts à partir. Le premier part tout de
/// suite, les suivants au plus tôt `CELL_LINGER` après le paquet d'avant.
pub struct Grouper {
    /// Blocs en attente, heure à laquelle ils partent, heure du dernier paquet parti.
    cells: Vec<crate::cells::Cell>,
    cells_due: Instant,
    cells_sent: Instant,
    /// Positions en attente (la dernière de chaque joueur), et quand le pseudo de chaque
    /// joueur a été dit sur cette connexion pour la dernière fois.
    poses: std::collections::HashMap<u64, (std::sync::Arc<PoseOf>, Instant)>,
    named: std::collections::HashMap<u64, Instant>,
    /// Paquets prêts (messages sérialisés, à chiffrer et envoyer).
    ready: std::collections::VecDeque<Vec<u8>>,
}

impl Default for Grouper {
    fn default() -> Self {
        Self::new()
    }
}

impl Grouper {
    pub fn new() -> Self {
        Self {
            cells: Vec::new(),
            cells_due: Instant::now(),
            cells_sent: Instant::now(),
            poses: std::collections::HashMap::new(),
            named: std::collections::HashMap::new(),
            ready: std::collections::VecDeque::new(),
        }
    }

    /// Des blocs ou des positions attendent-ils d'être regroupés ?
    pub fn waiting(&self) -> bool {
        !self.cells.is_empty() || !self.poses.is_empty()
    }

    /// Heure à laquelle ce qui attend doit partir.
    pub fn due(&self) -> Option<Instant> {
        self.waiting().then_some(self.cells_due)
    }

    /// Prochain paquet prêt.
    pub fn next_ready(&mut self) -> Option<Vec<u8>> {
        self.ready.pop_front()
    }

    /// Un bloc changé. Vrai s'il y en a assez pour un paquet plein (`pack` tout de suite).
    pub fn add_cell(&mut self, c: crate::cells::Cell) -> bool {
        if !self.waiting() {
            self.cells_due = (self.cells_sent + CELL_LINGER).max(Instant::now());
        }
        self.cells.push(c);
        self.cells.len() >= crate::cells::CELLS_MAX
    }

    /// La position d'un joueur, déposée à `at`. Vrai s'il y en a assez pour un paquet
    /// plein.
    pub fn add_pose(&mut self, p: std::sync::Arc<PoseOf>, at: Instant) -> bool {
        if !self.waiting() {
            self.cells_due = (self.cells_sent + CELL_LINGER).max(Instant::now());
        }
        self.poses.insert(p.id, (p, at));
        self.poses.len() >= crate::poses::POSES_MAX
    }

    /// Ce qui attend devient des paquets prêts. `more` donne les messages encore dans la
    /// file, pour les regrouper avec : le premier qui n'est ni un bloc ni une position est
    /// rendu (à traiter ensuite).
    pub fn pack(&mut self, mut more: impl FnMut() -> Option<Inbound>) -> Option<Inbound> {
        let mut held = None;
        while self.cells.len() < crate::cells::CELLS_MAX {
            match more() {
                Some(Inbound::Cell(c)) => self.cells.push(c),
                Some(Inbound::Pose(p, at)) => {
                    self.poses.insert(p.id, (p, at));
                }
                Some(m) => {
                    held = Some(m);
                    break;
                }
                None => break,
            }
        }
        self.cells_sent = Instant::now();
        if !self.poses.is_empty() {
            let now = self.cells_sent;
            if self.named.len() > 8192 {
                self.named
                    .retain(|_, t| now.duration_since(*t) < NAME_EVERY);
            }
            let poses = std::mem::take(&mut self.poses);
            let named = &mut self.named;
            let mut entries: Vec<(u64, Option<&str>, crate::poses::Pose)> = poses
                .values()
                .filter(|(_, at)| now.duration_since(*at) <= LOOSE_MAX_AGE)
                .map(|(p, _)| {
                    let known = named
                        .get(&p.id)
                        .is_some_and(|t| now.duration_since(*t) < NAME_EVERY);
                    if !known {
                        named.insert(p.id, now);
                    }
                    (p.id, (!known).then_some(p.name.as_str()), p.pose)
                })
                .collect();
            if !entries.is_empty() {
                let msg = crate::proto::Response::FromZone {
                    peer: 0,
                    chunk: [0, 0],
                    data: crate::game::encode(&crate::game::BuildMsg::Poses(
                        crate::poses::pack_poses(&mut entries),
                    )),
                };
                if let Ok(b) = postcard::to_allocvec(&msg) {
                    self.ready.push_back(b);
                }
            }
        }
        for packet in crate::cells::pack_cells(std::mem::take(&mut self.cells)) {
            let msg = crate::proto::Response::FromZone {
                peer: 0,
                chunk: [0, 0],
                data: crate::game::encode(&crate::game::BuildMsg::Cells(packet)),
            };
            if let Ok(b) = postcard::to_allocvec(&msg) {
                self.ready.push_back(b);
            }
        }
        held
    }
}

/// Dépose un message pour qu'il soit envoyé par le fil qui possède le canal.
pub fn push<T: Serialize>(tx: &impl Sink, msg: &T) -> bool {
    postcard::to_allocvec(msg)
        .ok()
        .is_some_and(|b| tx.put(Inbound::Push(b)))
}

/// Sérialise un message une fois, pour le déposer ensuite chez plusieurs destinataires
/// (`push_raw`, `push_loose`) sans le resérialiser pour chacun.
pub fn encode<T: Serialize>(msg: &T) -> Option<Vec<u8>> {
    postcard::to_allocvec(msg).ok()
}

/// Dépose un message déjà sérialisé (`encode`).
pub fn push_raw(tx: &impl Sink, bytes: Vec<u8>) -> bool {
    tx.put(Inbound::Push(bytes))
}

/// Dépose un bloc changé : il part regroupé avec ceux qui le suivent de près, dans un
/// paquet compact (`BuildMsg::Cells`). L'ordre entre blocs ne compte pas (chacun porte son
/// rang dans l'ordre du monde).
pub fn push_cell(tx: &impl Sink, cell: crate::cells::Cell) -> bool {
    tx.put(Inbound::Cell(cell))
}

/// Dépose la position d'un joueur : elle part regroupée avec celles des autres, dans un
/// paquet compact (`BuildMsg::Poses`) ; restée trop longtemps dans la file, ou remplacée
/// par la suivante du même joueur, elle n'est pas envoyée.
pub fn push_pose(tx: &impl Sink, pose: &std::sync::Arc<PoseOf>) -> bool {
    tx.put(Inbound::Pose(pose.clone(), Instant::now()))
}

/// Dépose un message périssable déjà sérialisé : s'il attend trop dans la file (client
/// lent, relais débordé), il est abandonné au lieu d'être envoyé en retard.
pub fn push_loose(tx: &impl Sink, bytes: Vec<u8>) -> bool {
    tx.put(Inbound::Loose(bytes, Instant::now()))
}

fn write_frame(s: &mut TcpStream, data: &[u8]) -> Result<()> {
    let len = u16::try_from(data.len()).map_err(|_| anyhow!("message trop long"))?;
    s.write_all(&len.to_be_bytes())?;
    s.write_all(data)?;
    Ok(())
}

fn read_frame<R: Read>(s: &mut R, buf: &mut Vec<u8>) -> Result<usize> {
    let mut len = [0u8; 2];
    s.read_exact(&mut len)?;
    let n = u16::from_be_bytes(len) as usize;
    buf.resize(n, 0);
    s.read_exact(&mut buf[..n])?;
    Ok(n)
}

/// Paire de clés Noise (clé fixe du relais).
pub fn generate_static() -> ([u8; 32], [u8; 32]) {
    let kp = snow::Builder::new(PATTERN.parse().expect("motif"))
        .generate_keypair()
        .expect("clés Noise");
    let mut private = [0u8; 32];
    let mut public = [0u8; 32];
    private.copy_from_slice(&kp.private);
    public.copy_from_slice(&kp.public);
    (private, public)
}

impl Channel {
    /// Connexion du jeu au relais. Renvoie aussi la clé publique du relais, que l'appelant
    /// compare à celle retenue.
    pub fn connect(addr: &str, timeout: Duration) -> Result<(Self, [u8; 32])> {
        let sock = addr
            .parse()
            .or_else(|_| {
                use std::net::ToSocketAddrs;
                addr.to_socket_addrs()?
                    .next()
                    .ok_or_else(|| std::io::Error::other("adresse introuvable"))
            })
            .with_context(|| format!("adresse du relais « {addr} »"))?;
        let mut stream = TcpStream::connect_timeout(&sock, timeout)?;
        stream.set_nodelay(true)?;
        stream.set_read_timeout(Some(timeout))?;
        let (private, _) = generate_static();
        let mut hs = snow::Builder::new(PATTERN.parse()?)
            .local_private_key(&private)
            .build_initiator()?;
        let mut msg = vec![0u8; MAX];
        let mut buf = Vec::new();
        let n = hs.write_message(&[], &mut msg)?;
        write_frame(&mut stream, &msg[..n])?;
        let n = read_frame(&mut stream, &mut buf)?;
        hs.read_message(&buf[..n], &mut msg)?;
        let n = hs.write_message(&[], &mut msg)?;
        write_frame(&mut stream, &msg[..n])?;
        let mut remote = [0u8; 32];
        remote.copy_from_slice(
            hs.get_remote_static()
                .ok_or_else(|| anyhow!("clé du relais absente"))?,
        );
        let noise = hs.into_transport_mode()?;
        let _ = buf;
        Ok((Self::finish(stream, noise, None)?, remote))
    }

    /// Côté relais : poignée de main avec un jeu qui vient de se connecter.
    pub fn accept(mut stream: TcpStream, private: &[u8; 32], timeout: Duration) -> Result<Self> {
        stream.set_nodelay(true)?;
        stream.set_read_timeout(Some(timeout))?;
        let mut hs = snow::Builder::new(PATTERN.parse()?)
            .local_private_key(private)
            .build_responder()?;
        let mut msg = vec![0u8; MAX];
        let mut buf = Vec::new();
        let n = read_frame(&mut stream, &mut buf)?;
        hs.read_message(&buf[..n], &mut msg)?;
        let n = hs.write_message(&[], &mut msg)?;
        write_frame(&mut stream, &msg[..n])?;
        let n = read_frame(&mut stream, &mut buf)?;
        hs.read_message(&buf[..n], &mut msg)?;
        let noise = hs.into_transport_mode()?;
        let _ = buf;
        // Un client qui ne lit plus (file d'envoi qui gonfle) ou qui inonde le relais de
        // messages est coupé.
        stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
        Self::finish(stream, noise, Some(FLOOD_BACKLOG))
    }

    /// Après la poignée de main : un fil lit les trames et les dépose dans la file.
    /// `flood` : trames en attente au-delà desquelles la connexion est coupée.
    fn finish(stream: TcpStream, noise: snow::TransportState, flood: Option<u32>) -> Result<Self> {
        stream.set_read_timeout(None)?;
        // (Lecture par blocs : dans une foule, des centaines de petites trames par seconde.)
        let mut reader = std::io::BufReader::with_capacity(32 * 1024, stream.try_clone()?);
        let (tx, rx) = channel();
        let out = tx.clone();
        let backlog = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = backlog.clone();
        std::thread::Builder::new()
            .name("réseau : lecture".into())
            .spawn(move || {
                let mut buf = Vec::new();
                loop {
                    match read_frame(&mut reader, &mut buf) {
                        Ok(n) => {
                            // Le relais ne suit plus ce client (il l'inonde) : coupé.
                            let waiting = seen.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            if flood.is_some_and(|max| waiting > max as usize) {
                                let _ = out.send(Inbound::Closed);
                                let _ = reader.get_ref().shutdown(std::net::Shutdown::Both);
                                return;
                            }
                            if out.send(Inbound::Frame(buf[..n].to_vec())).is_err() {
                                return;
                            }
                        }
                        Err(_) => {
                            let _ = out.send(Inbound::Closed);
                            return;
                        }
                    }
                }
            })?;
        Ok(Self {
            stream,
            noise,
            rx,
            tx,
            backlog,
            out: Vec::new(),
            group: Grouper::new(),
            held: None,
        })
    }

    /// Pour déposer des messages à envoyer depuis un autre fil (`push`).
    pub fn pusher(&self) -> Sender<Inbound> {
        self.tx.clone()
    }

    /// Envoie un message tout de suite (avec ceux qui attendaient, dans l'ordre).
    pub fn send<T: Serialize>(&mut self, msg: &T) -> Result<()> {
        let plain = postcard::to_allocvec(msg)?;
        self.send_raw(&plain)?;
        self.flush()
    }

    /// Prépare l'envoi d'un message déjà sérialisé (déposé par `push`) : il part avec les
    /// suivants, dès que la file est vide (au prochain `recv_any`) ou que le tampon est
    /// plein.
    pub fn send_raw(&mut self, plain: &[u8]) -> Result<()> {
        if plain.len() + 16 > MAX {
            return Err(anyhow!("message trop long"));
        }
        let mut frame = vec![0u8; plain.len() + 16];
        let n = self.noise.write_message(plain, &mut frame)?;
        let len = u16::try_from(n).map_err(|_| anyhow!("message trop long"))?;
        self.out.extend_from_slice(&len.to_be_bytes());
        self.out.extend_from_slice(&frame[..n]);
        if self.out.len() >= OUT_MAX {
            self.flush()?;
        }
        Ok(())
    }

    /// Écrit sur le réseau ce qui attendait dans le tampon d'envoi.
    pub fn flush(&mut self) -> Result<()> {
        if !self.out.is_empty() {
            let r = self.stream.write_all(&self.out);
            self.out.clear();
            r?;
        }
        Ok(())
    }

    fn decode<T: DeserializeOwned>(&mut self, frame: &[u8]) -> Result<T> {
        let mut plain = vec![0u8; frame.len()];
        let m = self.noise.read_message(frame, &mut plain)?;
        Ok(postcard::from_bytes(&plain[..m])?)
    }

    /// Les blocs et les positions en attente deviennent des paquets prêts à partir (avec
    /// ceux qui sont déjà dans la file).
    fn pack_waiting_cells(&mut self) {
        if self.held.is_some() {
            self.group.pack(|| None);
            return;
        }
        let rx = &self.rx;
        self.held = self.group.pack(|| rx.try_recv().ok());
    }

    /// Prochain message reçu ou déposé ; `None` si rien avant `timeout`.
    pub fn recv_any<T: DeserializeOwned>(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<Incoming<T>>> {
        let end = Instant::now() + timeout;
        loop {
            if let Some(p) = self.group.next_ready() {
                return Ok(Some(Incoming::Push(p)));
            }
            // Des blocs ou des positions attendent et leur heure est venue : ils partent,
            // regroupés.
            if self.group.due().is_some_and(|d| Instant::now() >= d) {
                self.pack_waiting_cells();
                continue;
            }
            let next = match self.held.take() {
                Some(m) => m,
                None => match self.rx.try_recv() {
                    Ok(m) => m,
                    Err(TryRecvError::Disconnected) => return Err(anyhow!("connexion fermée")),
                    // Plus rien dans la file : ce qui attendait dans le tampon d'envoi
                    // part maintenant, d'un seul bloc, avant de se mettre en attente (du
                    // prochain message, ou de l'heure des blocs en attente).
                    Err(TryRecvError::Empty) => {
                        self.flush()?;
                        let now = Instant::now();
                        let until = match self.group.due() {
                            Some(d) => end.min(d),
                            None => end,
                        };
                        match self.rx.recv_timeout(until.saturating_duration_since(now)) {
                            Ok(m) => m,
                            // (Délai de l'appelant passé avant l'heure des blocs : ils
                            // partiront à son prochain appel.)
                            Err(RecvTimeoutError::Timeout) if Instant::now() >= end => {
                                return Ok(None)
                            }
                            Err(RecvTimeoutError::Timeout) => continue,
                            Err(RecvTimeoutError::Disconnected) => {
                                return Err(anyhow!("connexion fermée"))
                            }
                        }
                    }
                },
            };
            match next {
                Inbound::Frame(f) => {
                    self.backlog
                        .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                    return Ok(Some(Incoming::Msg(self.decode(&f)?)));
                }
                Inbound::Push(p) => return Ok(Some(Incoming::Push(p))),
                // Périssable : trop vieux, il est abandonné (le suivant le remplace).
                Inbound::Loose(_, at) if at.elapsed() > LOOSE_MAX_AGE => {}
                Inbound::Loose(p, _) => return Ok(Some(Incoming::Push(p))),
                Inbound::Cell(c) => {
                    if self.group.add_cell(c) {
                        self.pack_waiting_cells();
                    }
                }
                Inbound::Pose(p, at) => {
                    if self.group.add_pose(p, at) {
                        self.pack_waiting_cells();
                    }
                }
                Inbound::Closed => return Err(anyhow!("connexion fermée")),
            }
        }
    }

    /// Prochain message reçu du réseau (les dépôts sont envoyés au passage).
    pub fn recv_timeout<T: DeserializeOwned>(&mut self, timeout: Duration) -> Result<Option<T>> {
        let end = std::time::Instant::now() + timeout;
        loop {
            let left = end.saturating_duration_since(std::time::Instant::now());
            match self.recv_any::<T>(left)? {
                Some(Incoming::Msg(m)) => return Ok(Some(m)),
                Some(Incoming::Push(p)) => self.send_raw(&p)?,
                None => return Ok(None),
            }
        }
    }

    /// Prochain message reçu (10 s au plus).
    pub fn recv<T: DeserializeOwned>(&mut self) -> Result<T> {
        self.recv_timeout(Duration::from_secs(10))?
            .ok_or_else(|| anyhow!("pas de réponse"))
    }
}

/// À la fermeture : ce qui attendait encore dans le tampon d'envoi part (un dernier
/// message d'erreur, par exemple).
impl Drop for Channel {
    fn drop(&mut self) {
        let _ = self.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    /// Les positions de trente joueurs, soixante fois par seconde chacun, arrivent en une
    /// cinquantaine de paquets par seconde ; le pseudo de chacun n'est dit qu'une fois, et
    /// la dernière position de chacun est bien la dernière envoyée.
    #[test]
    fn poses_leave_grouped_with_each_name_said_once() {
        use crate::game::{self, BuildMsg};
        use crate::poses::Pose;
        use crate::proto::{Request, Response};
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let (private, _) = generate_static();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut c = Channel::accept(stream, &private, Duration::from_secs(5)).unwrap();
            let tx = c.pusher();
            std::thread::spawn(move || {
                for step in 0..30u32 {
                    for id in 1..=30u64 {
                        let pose = std::sync::Arc::new(PoseOf {
                            id,
                            name: format!("Joueur{id}"),
                            pose: Pose {
                                pos: [id as f32, 64.0, step as f32],
                                yaw: 1.0,
                                pitch: 0.0,
                                crouch: 0.0,
                                speed: 4.0,
                                ping: None,
                            },
                        });
                        push_pose(&tx, &pose);
                    }
                    std::thread::sleep(Duration::from_millis(16));
                }
                std::thread::sleep(Duration::from_millis(60));
                push(&tx, &Response::Pong);
            });
            loop {
                match c.recv_any::<Request>(Duration::from_millis(500)) {
                    Ok(Some(Incoming::Push(p))) => c.send_raw(&p).unwrap(),
                    Ok(Some(Incoming::Msg(_))) => {}
                    Ok(None) | Err(_) => break,
                }
            }
        });
        let (mut c, _) = Channel::connect(&addr, Duration::from_secs(5)).unwrap();
        let (mut packets, mut bytes, mut entries) = (0, 0, 0);
        let mut names = std::collections::HashMap::new();
        let mut last = std::collections::HashMap::new();
        let t = Instant::now();
        loop {
            assert!(t.elapsed() < Duration::from_secs(10));
            match c
                .recv_timeout::<Response>(Duration::from_millis(200))
                .unwrap()
            {
                Some(Response::FromZone { data, .. }) => {
                    let Some(BuildMsg::Poses(p)) = game::decode(&data) else {
                        panic!("un paquet de positions")
                    };
                    packets += 1;
                    bytes += p.len();
                    for (id, name, pose) in crate::poses::unpack_poses(&p).expect("lisible") {
                        entries += 1;
                        if let Some(n) = name {
                            *names.entry((id, n)).or_insert(0) += 1;
                        }
                        last.insert(id, pose.pos[2]);
                    }
                }
                Some(Response::Pong) => break,
                _ => {}
            }
        }
        drop(c);
        server.join().unwrap();
        assert_eq!(last.len(), 30);
        assert!(last.values().all(|z| *z == 29.0), "la dernière de chacun");
        assert_eq!(names.len(), 30, "chaque pseudo est dit");
        assert!(names
            .iter()
            .all(|((id, n), k)| *n == format!("Joueur{id}") && *k == 1));
        assert!((300..=900).contains(&entries), "{entries}");
        assert!(
            packets <= 40,
            "regroupées : {packets} paquets pour {entries} positions"
        );
        assert!(
            bytes < entries * 12 + packets * 12 + 30 * 12,
            "{bytes} octets pour {entries} positions"
        );
    }

    /// Des blocs déposés un par un arrivent regroupés : le premier tout de suite, les
    /// suivants ensemble, et aucun ne manque.
    #[test]
    fn cells_leave_in_a_few_compact_packets_and_none_is_lost() {
        use crate::game::{self, BuildMsg};
        use crate::proto::{Request, Response};
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let (private, _) = generate_static();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut c = Channel::accept(stream, &private, Duration::from_secs(5)).unwrap();
            let tx = c.pusher();
            // Un bloc seul, puis une rafale pendant que le canal tourne.
            push_cell(&tx, ([0, 1, 2], 3, 1));
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(60));
                for i in 0..3000u64 {
                    push_cell(
                        &tx,
                        (
                            [i as i32 % 200, (i % 256) as i32, -7],
                            (i % 50) as u16,
                            2 + i,
                        ),
                    );
                    if i % 500 == 0 {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                }
                push(&tx, &Response::Pong);
            });
            loop {
                match c.recv_any::<Request>(Duration::from_millis(500)) {
                    Ok(Some(Incoming::Push(p))) => c.send_raw(&p).unwrap(),
                    Ok(Some(Incoming::Msg(_))) => {}
                    Ok(None) | Err(_) => break,
                }
            }
        });
        let (mut c, _) = Channel::connect(&addr, Duration::from_secs(5)).unwrap();
        let (mut cells, mut packets, mut first, mut pong) = (Vec::new(), 0, None, false);
        let t = Instant::now();
        while (cells.len() < 3001 || !pong) && t.elapsed() < Duration::from_secs(10) {
            match c
                .recv_timeout::<Response>(Duration::from_millis(200))
                .unwrap()
            {
                Some(Response::FromZone { data, .. }) => {
                    let Some(BuildMsg::Cells(p)) = game::decode(&data) else {
                        panic!("un paquet de blocs")
                    };
                    let got = crate::cells::unpack_cells(&p).expect("paquet lisible");
                    first.get_or_insert((got.len(), t.elapsed()));
                    packets += 1;
                    cells.extend(got);
                }
                Some(Response::Pong) => pong = true,
                _ => {}
            }
        }
        drop(c);
        server.join().unwrap();
        assert_eq!(cells.len(), 3001, "aucun bloc perdu");
        cells.sort_unstable_by_key(|c| c.2);
        assert!(cells.iter().enumerate().all(|(i, c)| c.2 == 1 + i as u64));
        let (n, after) = first.unwrap();
        assert_eq!(n, 1, "le premier bloc n'attend pas les autres");
        assert!(after < Duration::from_millis(50), "{after:?}");
        assert!(
            packets <= 12,
            "regroupés : {packets} paquets pour 3001 blocs"
        );
    }

    #[test]
    fn encrypted_round_trip_and_relay_key() {
        let (private, public) = generate_static();
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap().to_string();
        let t = std::thread::spawn(move || {
            let (s, _) = l.accept().unwrap();
            let mut c = Channel::accept(s, &private, Duration::from_secs(5)).unwrap();
            let v: String = c.recv().unwrap();
            c.send(&format!("{v} !")).unwrap();
        });
        let (mut c, remote) = Channel::connect(&addr, Duration::from_secs(5)).unwrap();
        assert_eq!(remote, public);
        c.send(&"bonjour".to_string()).unwrap();
        let r: String = c.recv().unwrap();
        assert_eq!(r, "bonjour !");
        t.join().unwrap();
    }

    /// Les messages déposés partent regroupés, dans l'ordre, et arrivent un par un ; un
    /// message périssable resté trop longtemps dans la file n'est pas envoyé.
    #[test]
    fn pushed_messages_leave_together_in_order_and_stale_loose_ones_are_dropped() {
        let (private, _) = generate_static();
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap().to_string();
        let t = std::thread::spawn(move || {
            let (s, _) = l.accept().unwrap();
            let mut c = Channel::accept(s, &private, Duration::from_secs(5)).unwrap();
            let tx = c.pusher();
            // Un périssable déjà vieux, puis 500 messages, dont des périssables frais.
            tx.send(Inbound::Loose(
                encode(&u32::MAX).unwrap(),
                Instant::now() - Duration::from_secs(1),
            ))
            .unwrap();
            for i in 0..500u32 {
                if i % 2 == 0 {
                    assert!(push(&tx, &i));
                } else {
                    assert!(push_loose(&tx, encode(&i).unwrap()));
                }
            }
            // Le fil du canal : envoie ce qui est déposé, jusqu'au mot de la fin.
            loop {
                match c.recv_any::<String>(Duration::from_secs(5)).unwrap() {
                    Some(Incoming::Push(b)) => c.send_raw(&b).unwrap(),
                    Some(Incoming::Msg(m)) => {
                        assert_eq!(m, "fini");
                        break;
                    }
                    None => panic!("rien reçu"),
                }
            }
        });
        let (mut c, _) = Channel::connect(&addr, Duration::from_secs(5)).unwrap();
        for i in 0..500u32 {
            let v: u32 = c.recv().unwrap();
            assert_eq!(v, i, "dans l'ordre, sans le périssable trop vieux");
        }
        c.send(&"fini".to_string()).unwrap();
        t.join().unwrap();
    }
}
