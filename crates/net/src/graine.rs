//! (v330) Graine d'un monde créé depuis la v330, et ce qu'on en tire.
//!
//! Demande de Viendax (25 novembre) : la graine verrouillée d'un serveur de construction doit
//! être **impossible** à retrouver pour qui n'est pas OP, même en fouillant les fichiers du jeu.
//!
//! - La **graine** (64 bits) n'est connue que du créateur, des OP du serveur (que le relais
//!   la leur donne) et de qui a la carte en solo.
//! - Le terrain vient d'une **clé de terrain** (64 bits, `terrain_key`), tirée de la graine
//!   par Argon2id : un calcul exprès lent et gourmand en mémoire. Les autres joueurs ne
//!   reçoivent que cette clé. Pour remonter de la clé à une graine (la vraie, ou une autre qui
//!   ferait le même monde), il faudrait essayer de l'ordre de 2⁶⁴ graines à quelques
//!   centièmes de seconde chacune : des milliards d'années de calcul.
//! - La **graine publique** (32 bits, `public_seed`), tirée de la clé, fait la météo, les
//!   éclairs et la Foudre de Zeus (le relais la reçoit du créateur, comme avant).
//!
//! Les mondes d'avant la v330 gardent leur graine de 32 bits, qui fait elle-même le terrain :
//! le leur ne change pas.

use argon2::{Algorithm, Argon2, Params, Version};

/// Mémoire d'Argon2id pour la clé de terrain (Kio) : 16 Mio.
const KEY_MEM: u32 = 16 * 1024;
/// Passes d'Argon2id.
const KEY_PASSES: u32 = 2;
/// Sel commun à tous les mondes : une même graine fait toujours le même monde.
const KEY_SALT: &[u8] = b"minelib/graine/v2";

/// Clé de terrain d'un monde de graine `seed` (quelques centièmes de seconde : à calculer une
/// fois, en ouvrant ou en créant le monde). Jamais 0 (0 : « pas de clé »).
pub fn terrain_key(seed: u64) -> u64 {
    let params = Params::new(KEY_MEM, KEY_PASSES, 1, Some(8)).expect("réglages d'Argon2id");
    let mut out = [0u8; 8];
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(&seed.to_le_bytes(), KEY_SALT, &mut out)
        .expect("Argon2id");
    u64::from_le_bytes(out).max(1)
}

/// Graine publique (météo, éclairs) d'un monde de clé de terrain `key`.
pub fn public_seed(key: u64) -> u32 {
    let mut z = key ^ 0x6d65_7465_6f21_7632; // « meteo!v2 »
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    (z ^ (z >> 31)) as u32
}

/// Graine d'un nouveau monde à partir de ce que le joueur a tapé : rien (au hasard), un nombre
/// (même négatif), sinon un texte (son empreinte).
pub fn parse(text: &str, random: u64) -> u64 {
    let t = text.trim();
    if t.is_empty() {
        let mut z = random.wrapping_add(0x9e37_79b9_7f4a_7c15);
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        return z ^ (z >> 31);
    }
    if let Ok(v) = t.parse::<u64>() {
        return v;
    }
    if let Ok(v) = t.parse::<i64>() {
        return v as u64;
    }
    // FNV-1a sur 64 bits.
    t.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
        (h ^ b as u64).wrapping_mul(0x100_0000_01b3)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// La même graine donne toujours la même clé (sur tout PC) ; deux graines voisines, des
    /// clés sans rapport ; la graine publique vient de la clé.
    #[test]
    fn the_key_comes_from_the_seed_only() {
        let a = terrain_key(4242);
        assert_eq!(a, terrain_key(4242));
        assert_ne!(a, 0);
        let b = terrain_key(4243);
        assert!((a ^ b).count_ones() > 12, "{a:#x} {b:#x}");
        assert_eq!(public_seed(a), public_seed(a));
        assert_ne!(public_seed(a), public_seed(b));
        // Valeur fixée : un changement de réglage changerait le terrain de tous les mondes.
        assert_eq!(a, 0x889b_d8ec_ec95_5e24, "clé de la graine 4242 : {a:#x}");
    }

    /// Ce que le joueur tape : un nombre, un nombre négatif, un texte, rien.
    #[test]
    fn typed_seeds() {
        assert_eq!(parse("4242", 0), 4242);
        assert_eq!(parse(" 18446744073709551615 ", 0), u64::MAX);
        assert_eq!(parse("-1", 0), u64::MAX);
        assert_eq!(parse("Minelib", 0), parse("Minelib", 99));
        assert_ne!(parse("Minelib", 0), parse("minelib", 0));
        assert_ne!(parse("", 1), parse("", 2));
    }
}
