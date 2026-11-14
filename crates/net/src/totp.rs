//! Double authentification par codes TOTP (RFC 6238, standard libre) : codes à 6 chiffres
//! toutes les 30 s, lus par Authy, Aegis, Google Authenticator, 2FAS, Bitwarden…

use hmac::{Hmac, Mac};
use sha1::Sha1;

pub const STEP: u64 = 30;

/// Code de la période `step` (temps Unix / 30).
pub fn code(secret: &[u8], step: u64) -> u32 {
    let mut mac = Hmac::<Sha1>::new_from_slice(secret).expect("clé HMAC");
    mac.update(&step.to_be_bytes());
    let h = mac.finalize().into_bytes();
    let off = (h[19] & 0x0f) as usize;
    let bin = u32::from_be_bytes([h[off] & 0x7f, h[off + 1], h[off + 2], h[off + 3]]);
    bin % 1_000_000
}

pub fn now_step() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
        / STEP
}

/// Vérifie `given` à ±1 période (horloges un peu décalées) ; renvoie la période trouvée
/// pour refuser qu'un même code serve deux fois.
pub fn check(secret: &[u8], given: u32, now: u64) -> Option<u64> {
    (now.saturating_sub(1)..=now + 1).find(|&s| code(secret, s) == given)
}

/// Base32 sans remplissage (RFC 4648), comme les applis 2FA l'attendent.
pub fn base32(data: &[u8]) -> String {
    const A: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut out = String::new();
    let (mut buf, mut bits) = (0u32, 0u32);
    for &b in data {
        buf = (buf << 8) | b as u32;
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(A[((buf >> bits) & 31) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(A[((buf << (5 - bits)) & 31) as usize] as char);
    }
    out
}

/// Adresse à mettre dans le QR code (format `otpauth://`).
pub fn uri(pseudo: &str, secret: &[u8]) -> String {
    format!(
        "otpauth://totp/Minelib:{pseudo}?secret={}&issuer=Minelib&digits=6&period=30",
        base32(secret)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc_6238_reference_codes() {
        // Valeurs de référence de la RFC 6238 (SHA-1, 8 chiffres ramenés à 6).
        let secret = b"12345678901234567890";
        assert_eq!(code(secret, 59 / 30), 287082);
        assert_eq!(code(secret, 1111111109 / 30), 81804);
        assert_eq!(code(secret, 1234567890 / 30), 5924);
        assert_eq!(check(secret, 287082, 2), Some(1));
        assert_eq!(check(secret, 287082, 5), None);
        assert_eq!(base32(b"foobar"), "MZXW6YTBOI");
    }
}
