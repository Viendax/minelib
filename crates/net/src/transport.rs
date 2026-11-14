//! Canal chiffré sur TCP (protocole Noise XX : Curve25519, ChaCha20-Poly1305, BLAKE2s).
//!
//! Chaque message : longueur sur 2 octets puis le message chiffré. Le relais a une clé
//! fixe ; le jeu la retient au premier contact et refuse ensuite un relais qui en change
//! (protection contre un faux relais placé entre les deux).

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde::de::DeserializeOwned;
use serde::Serialize;

const PATTERN: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";
/// Taille maximale d'un message Noise.
const MAX: usize = 65535;

/// Ce qui arrive sur un canal : message chiffré reçu du réseau, message (en clair) déposé
/// par un autre fil pour être envoyé, ou fin de connexion.
pub enum Inbound {
    Frame(Vec<u8>),
    Push(Vec<u8>),
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
}

/// Dépose un message pour qu'il soit envoyé par le fil qui possède le canal.
pub fn push<T: Serialize>(tx: &Sender<Inbound>, msg: &T) -> bool {
    postcard::to_allocvec(msg)
        .ok()
        .is_some_and(|b| tx.send(Inbound::Push(b)).is_ok())
}

fn write_frame(s: &mut TcpStream, data: &[u8]) -> Result<()> {
    let len = u16::try_from(data.len()).map_err(|_| anyhow!("message trop long"))?;
    s.write_all(&len.to_be_bytes())?;
    s.write_all(data)?;
    Ok(())
}

fn read_frame(s: &mut TcpStream, buf: &mut Vec<u8>) -> Result<usize> {
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
        Ok((Self::finish(stream, noise)?, remote))
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
        Self::finish(stream, noise)
    }

    /// Après la poignée de main : un fil lit les trames et les dépose dans la file.
    fn finish(stream: TcpStream, noise: snow::TransportState) -> Result<Self> {
        stream.set_read_timeout(None)?;
        let mut reader = stream.try_clone()?;
        let (tx, rx) = channel();
        let out = tx.clone();
        std::thread::Builder::new()
            .name("réseau : lecture".into())
            .spawn(move || {
                let mut buf = Vec::new();
                loop {
                    match read_frame(&mut reader, &mut buf) {
                        Ok(n) => {
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
        })
    }

    /// Pour déposer des messages à envoyer depuis un autre fil (`push`).
    pub fn pusher(&self) -> Sender<Inbound> {
        self.tx.clone()
    }

    pub fn send<T: Serialize>(&mut self, msg: &T) -> Result<()> {
        let plain = postcard::to_allocvec(msg)?;
        self.send_raw(&plain)
    }

    /// Envoie un message déjà sérialisé (déposé par `push`).
    pub fn send_raw(&mut self, plain: &[u8]) -> Result<()> {
        if plain.len() + 16 > MAX {
            return Err(anyhow!("message trop long"));
        }
        let mut out = vec![0u8; plain.len() + 16];
        let n = self.noise.write_message(plain, &mut out)?;
        write_frame(&mut self.stream, &out[..n])
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
        match self.rx.recv_timeout(timeout) {
            Ok(Inbound::Frame(f)) => Ok(Some(Incoming::Msg(self.decode(&f)?))),
            Ok(Inbound::Push(p)) => Ok(Some(Incoming::Push(p))),
            Ok(Inbound::Closed) | Err(RecvTimeoutError::Disconnected) => {
                Err(anyhow!("connexion fermée"))
            }
            Err(RecvTimeoutError::Timeout) => Ok(None),
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
}
