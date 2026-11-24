//! Connexions des jeux au relais du monde, servies par des tâches (tokio) au lieu de deux
//! fils d'exécution chacune.
//!
//! Avant la v131, chaque connexion avait son fil de lecture et son fil de traitement :
//! 60 000 joueurs, 120 000 fils, et autant de piles réservées et de changements de fil.
//! Maintenant quelques fils (un par cœur, huit au plus) servent toutes les connexions :
//! une tâche lit les trames d'une connexion et les dépose dans sa file, une autre les
//! traite avec ce que les autres connexions y déposent (comme le faisaient les deux fils,
//! voir `minelib_net::transport::Channel`), et attend sans fil quand il n'y a rien.
//!
//! Le traitement des demandes ne change pas (`handle_prepared`) ; seule la question posée aux
//! autres relais à l'inscription d'un pseudo (qui attend le réseau) part sur un fil à
//! part, pour ne pas bloquer les autres connexions.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use minelib_net::proto::{self, ErrorCode, Request, Response};
use minelib_net::transport::{self, Grouper, Inbound, Incoming, Sink};
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpStream;
use tokio::sync::mpsc::{
    error::TryRecvError, unbounded_channel, UnboundedReceiver, UnboundedSender,
};

use super::{err, leave_room, party_leave, taken_elsewhere, Conn, Session, State};

/// Délai de la poignée de main.
const HANDSHAKE: Duration = Duration::from_secs(10);
/// Le jeu envoie un ping toutes les 20 s : 60 s sans rien, la connexion est morte.
const IDLE: Duration = Duration::from_secs(60);

/// Où déposer les messages à envoyer à une connexion : la file d'un canal à fil dédié
/// (essais, relais de région) ou celle d'une connexion servie par tâches.
#[derive(Clone)]
pub(crate) enum Outbox {
    Thread(std::sync::mpsc::Sender<Inbound>),
    Task(UnboundedSender<Inbound>),
}

impl Sink for Outbox {
    fn put(&self, m: Inbound) -> bool {
        match self {
            Outbox::Thread(tx) => tx.send(m).is_ok(),
            Outbox::Task(tx) => tx.send(m).is_ok(),
        }
    }
}

impl From<std::sync::mpsc::Sender<Inbound>> for Outbox {
    fn from(tx: std::sync::mpsc::Sender<Inbound>) -> Self {
        Outbox::Thread(tx)
    }
}

/// Fils qui servent les connexions : un par cœur, de deux à huit (le traitement se fait
/// sous un seul verrou : au-delà, ils s'attendraient).
pub(crate) fn workers() -> usize {
    std::thread::available_parallelism()
        .map_or(2, |n| n.get())
        .clamp(2, 8)
}

async fn read_frame(r: &mut BufReader<OwnedReadHalf>, buf: &mut Vec<u8>) -> Result<usize> {
    let mut len = [0u8; 2];
    r.read_exact(&mut len).await?;
    let n = u16::from_be_bytes(len) as usize;
    buf.resize(n, 0);
    r.read_exact(&mut buf[..n]).await?;
    Ok(n)
}

async fn write_frame(w: &mut OwnedWriteHalf, data: &[u8]) -> Result<()> {
    let len = u16::try_from(data.len()).map_err(|_| anyhow!("message trop long"))?;
    let mut b = Vec::with_capacity(2 + data.len());
    b.extend_from_slice(&len.to_be_bytes());
    b.extend_from_slice(data);
    w.write_all(&b).await?;
    Ok(())
}

/// Le préambule « MLPX » du tunnel iroh (voir `super::tunnel_peer`), lu sans rien
/// consommer s'il n'y est pas. Seules les connexions venues de ce PC sont lues ainsi.
pub(crate) async fn tunnel_peer(stream: &TcpStream) -> Result<Option<String>> {
    const MAGIC: &[u8; 4] = minelib_p2p::PREAMBLE;
    let start = Instant::now();
    let mut head = [0u8; 5];
    loop {
        let n = tokio::time::timeout(HANDSHAKE, stream.peek(&mut head)).await??;
        let n = n.min(4);
        if n == 0 || head[..n] != MAGIC[..n] {
            return Ok(None);
        }
        if n == 4 {
            break;
        }
        if start.elapsed() > HANDSHAKE {
            return Err(anyhow!("préambule incomplet"));
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    // Le préambule entier : « MLPX », longueur, nom.
    let mut buf = vec![0u8; 5 + 255];
    loop {
        let n = tokio::time::timeout(HANDSHAKE, stream.peek(&mut buf)).await??;
        if n >= 5 && n >= 5 + buf[4] as usize {
            let len = 5 + buf[4] as usize;
            let mut take = vec![0u8; len];
            let mut got = 0;
            while got < len {
                stream.readable().await?;
                match stream.try_read(&mut take[got..]) {
                    Ok(0) => return Err(anyhow!("connexion fermée")),
                    Ok(k) => got += k,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(e) => return Err(e.into()),
                }
            }
            let who = String::from_utf8_lossy(&take[5..]).into_owned();
            return Ok(Some(if who.is_empty() { "iroh:?".into() } else { who }));
        }
        if start.elapsed() > HANDSHAKE {
            return Err(anyhow!("préambule incomplet"));
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// Une connexion ouverte, du côté de la tâche qui la traite.
struct Link {
    wr: OwnedWriteHalf,
    noise: snow::TransportState,
    rx: UnboundedReceiver<Inbound>,
    /// Trames lues pas encore traitées (voir `transport::FLOOD_BACKLOG`).
    backlog: Arc<AtomicUsize>,
    /// Trames chiffrées prêtes à partir.
    out: Vec<u8>,
    group: Grouper,
    held: Option<Inbound>,
}

impl Link {
    fn decode(&mut self, frame: &[u8]) -> Result<Request> {
        let mut plain = vec![0u8; frame.len()];
        let m = self.noise.read_message(frame, &mut plain)?;
        Ok(postcard::from_bytes(&plain[..m])?)
    }

    /// Chiffre un message sérialisé dans le tampon d'envoi (il part au prochain `flush`,
    /// ou tout de suite si le tampon est plein).
    async fn send_raw(&mut self, plain: &[u8]) -> Result<()> {
        if plain.len() + 16 > transport::MAX {
            return Err(anyhow!("message trop long"));
        }
        let mut frame = vec![0u8; plain.len() + 16];
        let n = self.noise.write_message(plain, &mut frame)?;
        let len = u16::try_from(n).map_err(|_| anyhow!("message trop long"))?;
        self.out.extend_from_slice(&len.to_be_bytes());
        self.out.extend_from_slice(&frame[..n]);
        if self.out.len() >= transport::OUT_MAX {
            self.flush().await?;
        }
        Ok(())
    }

    async fn send(&mut self, msg: &Response) -> Result<()> {
        let plain = postcard::to_allocvec(msg)?;
        self.send_raw(&plain).await?;
        self.flush().await
    }

    /// Écrit ce qui attend ; un client qui ne lit plus pendant `WRITE_TIMEOUT` est coupé.
    async fn flush(&mut self) -> Result<()> {
        if self.out.is_empty() {
            return Ok(());
        }
        let r = tokio::time::timeout(transport::WRITE_TIMEOUT, self.wr.write_all(&self.out)).await;
        self.out.clear();
        match r {
            Ok(r) => Ok(r?),
            Err(_) => Err(anyhow!("client qui ne lit plus")),
        }
    }

    fn pack(&mut self) {
        if self.held.is_some() {
            self.group.pack(|| None);
            return;
        }
        let rx = &mut self.rx;
        self.held = self.group.pack(|| rx.try_recv().ok());
    }

    /// Comme `Channel::recv_any` : prochaine demande reçue, ou prochain message déposé
    /// (à envoyer) ; `None` si rien avant `timeout`.
    async fn recv_any(&mut self, timeout: Duration) -> Result<Option<Incoming<Request>>> {
        let end = Instant::now() + timeout;
        loop {
            if let Some(p) = self.group.next_ready() {
                return Ok(Some(Incoming::Push(p)));
            }
            if self.group.due().is_some_and(|d| Instant::now() >= d) {
                self.pack();
                continue;
            }
            let next = match self.held.take() {
                Some(m) => m,
                None => match self.rx.try_recv() {
                    Ok(m) => m,
                    Err(TryRecvError::Disconnected) => return Err(anyhow!("connexion fermée")),
                    // Plus rien dans la file : ce qui attend part, d'un seul bloc, puis on
                    // attend sans fil (le prochain message, ou l'heure des blocs).
                    Err(TryRecvError::Empty) => {
                        self.flush().await?;
                        let until = match self.group.due() {
                            Some(d) => end.min(d),
                            None => end,
                        };
                        let deadline = tokio::time::Instant::from_std(until);
                        match tokio::time::timeout_at(deadline, self.rx.recv()).await {
                            Ok(Some(m)) => m,
                            Ok(None) => return Err(anyhow!("connexion fermée")),
                            Err(_) if Instant::now() >= end => return Ok(None),
                            Err(_) => continue,
                        }
                    }
                },
            };
            match next {
                Inbound::Frame(f) => {
                    self.backlog.fetch_sub(1, Ordering::Relaxed);
                    return Ok(Some(Incoming::Msg(self.decode(&f)?)));
                }
                Inbound::Push(p) => return Ok(Some(Incoming::Push(p))),
                Inbound::Loose(_, at) if at.elapsed() > transport::LOOSE_MAX_AGE => {}
                Inbound::Loose(p, _) => return Ok(Some(Incoming::Push(p))),
                Inbound::Cell(c) => {
                    if self.group.add_cell(c) {
                        self.pack();
                    }
                }
                Inbound::Pose(p, at) => {
                    if self.group.add_pose(p, at) {
                        self.pack();
                    }
                }
                Inbound::Closed => return Err(anyhow!("connexion fermée")),
            }
        }
    }
}

/// Lecture d'une connexion : les trames vont dans sa file ; un client qui envoie plus vite
/// que le relais ne traite est coupé.
async fn read_loop(
    mut rd: BufReader<OwnedReadHalf>,
    tx: UnboundedSender<Inbound>,
    backlog: Arc<AtomicUsize>,
) {
    let mut buf = Vec::new();
    loop {
        match read_frame(&mut rd, &mut buf).await {
            Ok(n) => {
                let waiting = backlog.fetch_add(1, Ordering::Relaxed);
                if waiting > transport::FLOOD_BACKLOG as usize {
                    let _ = tx.send(Inbound::Closed);
                    return;
                }
                if tx.send(Inbound::Frame(buf[..n].to_vec())).is_err() {
                    return;
                }
            }
            Err(_) => {
                let _ = tx.send(Inbound::Closed);
                return;
            }
        }
    }
}

async fn handshake(
    rd: &mut BufReader<OwnedReadHalf>,
    wr: &mut OwnedWriteHalf,
    private: &[u8; 32],
) -> Result<snow::TransportState> {
    let mut hs = snow::Builder::new(transport::PATTERN.parse()?)
        .local_private_key(private)
        .build_responder()?;
    let mut msg = vec![0u8; transport::MAX];
    let mut buf = Vec::new();
    let n = read_frame(rd, &mut buf).await?;
    hs.read_message(&buf[..n], &mut msg)?;
    let n = hs.write_message(&[], &mut msg)?;
    write_frame(wr, &msg[..n]).await?;
    let n = read_frame(rd, &mut buf).await?;
    hs.read_message(&buf[..n], &mut msg)?;
    Ok(hs.into_transport_mode()?)
}

/// Sert une connexion jusqu'à sa fin (comme `super::serve`, sans fil à elle).
pub(crate) async fn serve(stream: TcpStream, state: Arc<Mutex<State>>, peer: String) -> Result<()> {
    stream.set_nodelay(true)?;
    let private = state.lock().unwrap().id.noise_private;
    let (rd, mut wr) = stream.into_split();
    // (Un joueur envoie au plus une centaine de petites trames par seconde : un petit
    // tampon suffit, et 60 000 connexions n'en prennent que 240 Mo de moins.)
    let mut rd = BufReader::with_capacity(4 * 1024, rd);
    let noise = tokio::time::timeout(HANDSHAKE, handshake(&mut rd, &mut wr, &private))
        .await
        .map_err(|_| anyhow!("poignée de main trop lente"))??;
    let (tx, rx) = unbounded_channel();
    let backlog = Arc::new(AtomicUsize::new(0));
    let reader = tokio::spawn(read_loop(rd, tx.clone(), backlog.clone()));
    let id = {
        let mut st = state.lock().unwrap();
        let id = st.next_id;
        st.next_id += 1;
        let mut c = Conn::new(Outbox::Task(tx));
        // (Venue de cette machine, pas par le tunnel : voir `upkeep_standby`.)
        c.local = peer
            .parse::<std::net::IpAddr>()
            .is_ok_and(|a| a.is_loopback());
        st.conns.insert(id, c);
        id
    };
    let mut link = Link {
        wr,
        noise,
        rx,
        backlog,
        out: Vec::new(),
        group: Grouper::new(),
        held: None,
    };
    let result = serve_loop(&mut link, &state, &peer, id).await;
    // (Un dernier message d'erreur, par exemple.)
    let _ = link.flush().await;
    reader.abort();
    let mut st = state.lock().unwrap();
    party_leave(&mut st, id);
    leave_room(&mut st, id);
    st.maps.forget(id);
    // (Passeur de la voie rapide : les parties qu'il portait en reçoivent un autre.)
    crate::passeurs::gone(&mut st, id);
    st.conns.remove(&id);
    result
}

async fn serve_loop(link: &mut Link, state: &Arc<Mutex<State>>, peer: &str, id: u64) -> Result<()> {
    let mut session = Session::Anonymous;
    let mut hello = false;
    loop {
        let req = match link.recv_any(IDLE).await? {
            None => return Ok(()),
            Some(Incoming::Push(bytes)) => {
                link.send_raw(&bytes).await?;
                continue;
            }
            Some(Incoming::Msg(r)) => r,
        };
        if state
            .lock()
            .unwrap()
            .conns
            .get(&id)
            .is_some_and(|c| c.kicked)
        {
            link.send(&err(ErrorCode::Banned, "Compte banni.")).await?;
            return Ok(());
        }
        let resp = if !hello {
            match req {
                Request::Hello { protocol, client } if protocol == proto::PROTOCOL => {
                    hello = true;
                    let mut st = state.lock().unwrap();
                    if let Some(c) = st.conns.get_mut(&id) {
                        c.garant_only = client.starts_with("minelib-garant");
                        c.version = proto::client_version(&client);
                    }
                    Some(Response::Hello {
                        protocol: proto::PROTOCOL,
                        relay: st.cfg.name.clone(),
                        pow_bits: st.cfg.pow_bits,
                    })
                }
                Request::Hello { .. } => Some(err(ErrorCode::Protocol, "")),
                _ => Some(err(ErrorCode::Protocol, "bonjour attendu")),
            }
        } else {
            // L'inscription d'un pseudo demande aux autres relais s'il est pris : sur un
            // fil à part (réseau), sans bloquer les autres connexions.
            let taken = if matches!(req, Request::Register { .. }) {
                let (st, r) = (state.clone(), req.clone());
                tokio::task::spawn_blocking(move || taken_elsewhere(&r, &st))
                    .await
                    .ok()
                    .flatten()
            } else {
                None
            };
            match taken {
                Some(t) => Some(t),
                None => {
                    // (v276) Le mot de passe d'une connexion ou d'une inscription est
                    // vérifié ou haché ici, sur un fil à part : le relais n'attend pas.
                    let prepared =
                        if matches!(req, Request::Login { .. } | Request::Register { .. }) {
                            let (st, r, p) = (state.clone(), req.clone(), peer.to_string());
                            tokio::task::spawn_blocking(move || crate::prepare(&r, &st, &p))
                                .await
                                .ok()
                                .flatten()
                        } else {
                            None
                        };
                    crate::handle_prepared(req, prepared, state, &mut session, peer, id)
                }
            }
        };
        if let Some(r) = resp {
            link.send_raw(&postcard::to_allocvec(&r)?).await?;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    /// Comme `super::super::tunnel_peer` : le préambule du tunnel donne la vraie adresse
    /// et est consommé ; sans préambule, rien n'est consommé. Envoyé en plusieurs fois
    /// aussi.
    #[test]
    fn tunnel_preamble_gives_the_real_address() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = l.local_addr().unwrap();
            let read5 = |s: tokio::net::TcpStream| async move {
                use tokio::io::AsyncReadExt;
                let mut s = s;
                let mut rest = [0u8; 5];
                s.read_exact(&mut rest).await.unwrap();
                rest
            };
            // D'un bloc.
            let mut c = std::net::TcpStream::connect(addr).unwrap();
            c.write_all(&minelib_p2p::preamble("203.0.113.7")).unwrap();
            c.write_all(b"suite").unwrap();
            let (s, _) = l.accept().await.unwrap();
            assert_eq!(
                super::tunnel_peer(&s).await.unwrap().as_deref(),
                Some("203.0.113.7")
            );
            assert_eq!(&read5(s).await, b"suite");
            // En morceaux.
            let mut c = std::net::TcpStream::connect(addr).unwrap();
            c.set_nodelay(true).unwrap();
            let p = minelib_p2p::preamble("198.51.100.2");
            let (s, _) = l.accept().await.unwrap();
            let writer = std::thread::spawn(move || {
                for part in p.chunks(3) {
                    c.write_all(part).unwrap();
                    std::thread::sleep(std::time::Duration::from_millis(15));
                }
                c.write_all(b"suite").unwrap();
                c
            });
            assert_eq!(
                super::tunnel_peer(&s).await.unwrap().as_deref(),
                Some("198.51.100.2")
            );
            assert_eq!(&read5(s).await, b"suite");
            drop(writer.join());
            // Connexion locale ordinaire : rien n'est consommé.
            let mut c = std::net::TcpStream::connect(addr).unwrap();
            c.write_all(b"\x00\x20abc").unwrap();
            let (s, _) = l.accept().await.unwrap();
            assert_eq!(super::tunnel_peer(&s).await.unwrap(), None);
            assert_eq!(&read5(s).await, b"\x00\x20abc");
        });
    }
}
