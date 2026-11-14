//! Chat vocal chiffré de bout en bout.
//!
//! Chaque joueur a une paire de clés X25519 pour la session et chiffre sa voix avec une clé
//! à lui (ChaCha20-Poly1305), qu'il ne donne qu'à ceux qui doivent l'entendre (coéquipiers
//! en FPS, joueurs proches en construction), scellée pour chacun avec sa clé publique. Le
//! relais, l'hôte et les garants ne transportent que des octets illisibles. Quand un
//! auditeur ne doit plus entendre (changement d'équipe, il s'éloigne), la clé change.
//!
//! Le son : 16 000 échantillons par seconde, mono, paquets de 20 ms compressés en ADPCM
//! IMA (4 bits par échantillon, codec écrit ici).

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use hkdf::Hkdf;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use x25519_dalek::{EphemeralSecret, PublicKey, StaticSecret};

pub const RATE: u32 = 16_000;
/// Échantillons par paquet (20 ms).
pub const FRAME: usize = 320;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum VoiceMsg {
    /// Ma clé publique de session.
    Hello { pubkey: [u8; 32] },
    /// Ma clé de voix (époque `epoch`), scellée pour le destinataire.
    Key { epoch: u32, sealed: Vec<u8> },
    /// Un paquet de voix chiffré.
    Frame { epoch: u32, seq: u32, ct: Vec<u8> },
}

/// Clés de session d'un joueur.
pub struct Identity {
    secret: StaticSecret,
    pub public: [u8; 32],
}

impl Default for Identity {
    fn default() -> Self {
        Self::new()
    }
}

impl Identity {
    pub fn new() -> Self {
        let secret = StaticSecret::random_from_rng(rand::rngs::OsRng);
        let public = PublicKey::from(&secret).to_bytes();
        Self { secret, public }
    }

    /// Ouvre une clé de voix scellée pour soi.
    pub fn open(&self, sealed: &[u8]) -> Option<[u8; 32]> {
        if sealed.len() < 32 {
            return None;
        }
        let eph: [u8; 32] = sealed[..32].try_into().ok()?;
        let shared = self.secret.diffie_hellman(&PublicKey::from(eph));
        let key = seal_key(shared.as_bytes(), &eph, &self.public);
        let c = ChaCha20Poly1305::new(Key::from_slice(&key));
        let out = c.decrypt(Nonce::from_slice(&[0; 12]), &sealed[32..]).ok()?;
        out.try_into().ok()
    }
}

fn seal_key(shared: &[u8], eph: &[u8; 32], to: &[u8; 32]) -> [u8; 32] {
    let h = Hkdf::<Sha256>::new(Some(b"minelib/voix/scelle"), shared);
    let mut info = eph.to_vec();
    info.extend_from_slice(to);
    let mut k = [0u8; 32];
    h.expand(&info, &mut k).expect("hkdf");
    k
}

/// Scelle `voice_key` pour la clé publique `to` (seul son détenteur peut l'ouvrir).
pub fn seal(to: &[u8; 32], voice_key: &[u8; 32]) -> Vec<u8> {
    let eph = EphemeralSecret::random_from_rng(rand::rngs::OsRng);
    let eph_pub = PublicKey::from(&eph).to_bytes();
    let shared = eph.diffie_hellman(&PublicKey::from(*to));
    let key = seal_key(shared.as_bytes(), &eph_pub, to);
    let c = ChaCha20Poly1305::new(Key::from_slice(&key));
    let ct = c
        .encrypt(Nonce::from_slice(&[0; 12]), voice_key.as_slice())
        .expect("chiffrement");
    let mut out = eph_pub.to_vec();
    out.extend(ct);
    out
}

pub fn new_voice_key() -> [u8; 32] {
    let mut k = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut k);
    k
}

fn nonce(epoch: u32, seq: u32) -> [u8; 12] {
    let mut n = [0u8; 12];
    n[..4].copy_from_slice(&epoch.to_le_bytes());
    n[4..8].copy_from_slice(&seq.to_le_bytes());
    n
}

pub fn encrypt(key: &[u8; 32], epoch: u32, seq: u32, plain: &[u8]) -> Vec<u8> {
    ChaCha20Poly1305::new(Key::from_slice(key))
        .encrypt(Nonce::from_slice(&nonce(epoch, seq)), plain)
        .unwrap_or_default()
}

pub fn decrypt(key: &[u8; 32], epoch: u32, seq: u32, ct: &[u8]) -> Option<Vec<u8>> {
    ChaCha20Poly1305::new(Key::from_slice(key))
        .decrypt(Nonce::from_slice(&nonce(epoch, seq)), ct)
        .ok()
}

// ------------------------------------------------------------------ ADPCM IMA

const STEPS: [i32; 89] = [
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66,
    73, 80, 88, 97, 107, 118, 130, 143, 157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408, 449,
    494, 544, 598, 658, 724, 796, 876, 963, 1060, 1166, 1282, 1411, 1552, 1707, 1878, 2066, 2272,
    2499, 2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358, 5894, 6484, 7132, 7845, 8630, 9493,
    10442, 11487, 12635, 13899, 15289, 16818, 18500, 20350, 22385, 24623, 27086, 29794, 32767,
];
const INDEX: [i32; 16] = [-1, -1, -1, -1, 2, 4, 6, 8, -1, -1, -1, -1, 2, 4, 6, 8];

fn step(code: u8, pred: &mut i32, index: &mut i32) {
    let s = STEPS[*index as usize];
    let mut diff = s >> 3;
    if code & 4 != 0 {
        diff += s;
    }
    if code & 2 != 0 {
        diff += s >> 1;
    }
    if code & 1 != 0 {
        diff += s >> 2;
    }
    if code & 8 != 0 {
        *pred -= diff;
    } else {
        *pred += diff;
    }
    *pred = (*pred).clamp(-32768, 32767);
    *index = (*index + INDEX[code as usize]).clamp(0, 88);
}

/// Compresse un paquet (en-tête : prédiction et pas de départ, puis 4 bits par échantillon).
pub fn encode(samples: &[i16]) -> Vec<u8> {
    let mut pred = samples.first().copied().unwrap_or(0) as i32;
    let mut index = 0i32;
    // Pas de départ adapté à la première différence.
    if samples.len() > 1 {
        let d = (samples[1] as i32 - pred).abs();
        while index < 88 && STEPS[index as usize] < d {
            index += 1;
        }
    }
    let mut out = Vec::with_capacity(3 + samples.len() / 2);
    out.extend_from_slice(&(pred as i16).to_le_bytes());
    out.push(index as u8);
    let mut byte = 0u8;
    for (i, &s) in samples.iter().enumerate() {
        let st = STEPS[index as usize];
        let mut diff = s as i32 - pred;
        let mut code = 0u8;
        if diff < 0 {
            code = 8;
            diff = -diff;
        }
        if diff >= st {
            code |= 4;
            diff -= st;
        }
        if diff >= st >> 1 {
            code |= 2;
            diff -= st >> 1;
        }
        if diff >= st >> 2 {
            code |= 1;
        }
        step(code, &mut pred, &mut index);
        if i % 2 == 0 {
            byte = code;
        } else {
            out.push(byte | (code << 4));
        }
    }
    if samples.len() % 2 == 1 {
        out.push(byte);
    }
    out
}

pub fn decode(data: &[u8], count: usize) -> Vec<i16> {
    if data.len() < 3 {
        return Vec::new();
    }
    let mut pred = i16::from_le_bytes([data[0], data[1]]) as i32;
    let mut index = (data[2] as i32).clamp(0, 88);
    let mut out = Vec::with_capacity(count);
    for &b in &data[3..] {
        for code in [b & 0x0f, b >> 4] {
            if out.len() >= count {
                break;
            }
            step(code, &mut pred, &mut index);
            out.push(pred as i16);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_intended_listener_opens_the_voice_key() {
        let alice = Identity::new();
        let eve = Identity::new();
        let k = new_voice_key();
        let sealed = seal(&alice.public, &k);
        assert_eq!(alice.open(&sealed), Some(k));
        assert_eq!(
            eve.open(&sealed),
            None,
            "le relais ou le garant ne peut pas l'ouvrir"
        );
        let ct = encrypt(&k, 3, 7, b"bonjour");
        assert_eq!(decrypt(&k, 3, 7, &ct).as_deref(), Some(&b"bonjour"[..]));
        assert!(decrypt(&k, 3, 8, &ct).is_none());
    }

    #[test]
    fn adpcm_keeps_a_voice_like_signal() {
        let s: Vec<i16> = (0..FRAME)
            .map(|i| {
                let t = i as f32 / RATE as f32;
                ((t * 440.0 * std::f32::consts::TAU).sin() * 8000.0
                    + (t * 1250.0 * std::f32::consts::TAU).sin() * 3000.0) as i16
            })
            .collect();
        let enc = encode(&s);
        assert_eq!(enc.len(), 3 + FRAME / 2, "4 bits par échantillon");
        let dec = decode(&enc, FRAME);
        let err: f64 = s
            .iter()
            .zip(&dec)
            .map(|(a, b)| (*a as f64 - *b as f64).powi(2))
            .sum::<f64>()
            / FRAME as f64;
        let sig: f64 = s.iter().map(|a| (*a as f64).powi(2)).sum::<f64>() / FRAME as f64;
        let snr = 10.0 * (sig / err.max(1.0)).log10();
        assert!(snr > 15.0, "rapport signal/bruit {snr:.1} dB");
    }
}
