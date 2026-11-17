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

const PATTERN: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";
/// Taille maximale d'un message Noise.
const MAX: usize = 65535;

/// Côté relais : trames reçues d'un client et pas encore traitées au-delà desquelles il
/// est coupé (il envoie plus vite que le relais ne peut suivre : sa file gonflerait sans
/// fin).
pub const FLOOD_BACKLOG: u32 = 20_000;
/// Côté relais : un client qui ne lit plus rien pendant ce temps est coupé.
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(20);
/// Un message périssable (`push_loose` : la position d'un joueur, que la suivante
/// remplace) resté plus longtemps que cela dans la file n'est plus envoyé : un client lent
/// ne reçoit pas des positions vieilles de plusieurs secondes, et sa file ne gonfle pas.
const LOOSE_MAX_AGE: Duration = Duration::from_millis(250);
/// Les messages à envoyer sont regroupés : ils partent ensemble dès que la file est vide,
/// ou dès que le tampon atteint cette taille. Dans une foule, des centaines de positions
/// par seconde vont à chaque joueur ; une par paquet, chacune coûtait 40 octets
/// d'en-têtes et un appel au système.
const OUT_MAX: usize = 32 * 1024;

/// Ce qui arrive sur un canal : message chiffré reçu du réseau, message (en clair) déposé
/// par un autre fil pour être envoyé, ou fin de connexion.
pub enum Inbound {
    Frame(Vec<u8>),
    Push(Vec<u8>),
    /// Message périssable déposé à cet instant (voir `LOOSE_MAX_AGE`).
    Loose(Vec<u8>, Instant),
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
}

/// Dépose un message pour qu'il soit envoyé par le fil qui possède le canal.
pub fn push<T: Serialize>(tx: &Sender<Inbound>, msg: &T) -> bool {
    postcard::to_allocvec(msg)
        .ok()
        .is_some_and(|b| tx.send(Inbound::Push(b)).is_ok())
}

/// Sérialise un message une fois, pour le déposer ensuite chez plusieurs destinataires
/// (`push_raw`, `push_loose`) sans le resérialiser pour chacun.
pub fn encode<T: Serialize>(msg: &T) -> Option<Vec<u8>> {
    postcard::to_allocvec(msg).ok()
}

/// Dépose un message déjà sérialisé (`encode`).
pub fn push_raw(tx: &Sender<Inbound>, bytes: Vec<u8>) -> bool {
    tx.send(Inbound::Push(bytes)).is_ok()
}

/// Dépose un message périssable déjà sérialisé : s'il attend trop dans la file (client
/// lent, relais débordé), il est abandonné au lieu d'être envoyé en retard.
pub fn push_loose(tx: &Sender<Inbound>, bytes: Vec<u8>) -> bool {
    tx.send(Inbound::Loose(bytes, Instant::now())).is_ok()
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

    /// Prochain message reçu ou déposé ; `None` si rien avant `timeout`.
    pub fn recv_any<T: DeserializeOwned>(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<Incoming<T>>> {
        loop {
            // Plus rien dans la file : ce qui attendait dans le tampon d'envoi part
            // maintenant, d'un seul bloc, avant de se mettre en attente.
            let next = match self.rx.try_recv() {
                Ok(m) => m,
                Err(TryRecvError::Disconnected) => return Err(anyhow!("connexion fermée")),
                Err(TryRecvError::Empty) => {
                    self.flush()?;
                    match self.rx.recv_timeout(timeout) {
                        Ok(m) => m,
                        Err(RecvTimeoutError::Timeout) => return Ok(None),
                        Err(RecvTimeoutError::Disconnected) => {
                            return Err(anyhow!("connexion fermée"))
                        }
                    }
                }
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
