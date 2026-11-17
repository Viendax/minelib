//! Paquets compacts de blocs changés.
//!
//! Dans une foule qui construit, chaque joueur reçoit des centaines de blocs changés par
//! seconde. Un par message, chacun coûtait 47 octets sur le fil (la case sur trois nombres,
//! le pseudo de l'auteur, l'en-tête et le sceau du message chiffré). Regroupés, triés par
//! chunk et écrits en relatif, ils en coûtent 5 à 8 : la case tient sur deux octets dans
//! son chunk, le rang s'écrit par rapport au plus petit du paquet.
//!
//! L'ordre des blocs dans un paquet ne compte pas : chacun porte son rang dans l'ordre du
//! monde, et le jeu n'applique jamais un bloc plus ancien que celui qu'il a déjà pour la
//! même case.

/// Un bloc changé : la case, le bloc, son rang dans l'ordre du monde.
pub type Cell = ([i32; 3], u16, u64);

/// Blocs au plus par paquet, et taille à partir de laquelle un paquet est fermé (il reste
/// bien en dessous de la taille maximale d'un message, même pour des blocs épars aux
/// rangs très éloignés).
pub const CELLS_MAX: usize = 4096;
const PACKET_BYTES: usize = 32 * 1024;
/// Blocs d'un même chunk écrits d'un seul tenant, au plus.
const RUN_MAX: usize = 512;

fn put(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push(v as u8 | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn put_signed(out: &mut Vec<u8>, v: i64) {
    put(out, ((v << 1) ^ (v >> 63)) as u64);
}

fn get(data: &[u8], at: &mut usize) -> Option<u64> {
    let mut v = 0u64;
    for shift in (0..64).step_by(7) {
        let b = *data.get(*at)?;
        *at += 1;
        v |= ((b & 0x7f) as u64) << shift;
        if b < 0x80 {
            return Some(v);
        }
    }
    None
}

fn get_signed(data: &[u8], at: &mut usize) -> Option<i64> {
    let v = get(data, at)?;
    Some((v >> 1) as i64 ^ -((v & 1) as i64))
}

/// Écrit des blocs changés en paquets compacts (un seul la plupart du temps). Les blocs
/// hors du monde en hauteur (0 à 255) ne sont pas écrits.
pub fn pack_cells(mut cells: Vec<Cell>) -> Vec<Vec<u8>> {
    cells.retain(|c| (0..256).contains(&c.0[1]));
    let chunk = |c: &Cell| (c.0[0] >> 4, c.0[2] >> 4);
    cells.sort_unstable_by_key(|c| (chunk(c), c.0[1], c.0[2] & 15, c.0[0] & 15, c.2));
    let base = cells.iter().map(|c| c.2).min().unwrap_or(0);
    let mut out = Vec::new();
    let mut data = Vec::new();
    let (mut px, mut pz, mut count) = (0i64, 0i64, 0usize);
    let mut i = 0;
    while i < cells.len() {
        if data.is_empty() {
            put(&mut data, base);
            (px, pz, count) = (0, 0, 0);
        }
        let (cx, cz) = chunk(&cells[i]);
        let n = cells[i..]
            .iter()
            .take(RUN_MAX.min(CELLS_MAX - count))
            .take_while(|c| chunk(c) == (cx, cz))
            .count();
        put_signed(&mut data, cx as i64 - px);
        put_signed(&mut data, cz as i64 - pz);
        (px, pz) = (cx as i64, cz as i64);
        put(&mut data, n as u64);
        for c in &cells[i..i + n] {
            data.push((c.0[0] & 15) as u8 | ((c.0[2] & 15) as u8) << 4);
            data.push(c.0[1] as u8);
            put(&mut data, c.1 as u64);
            put(&mut data, c.2 - base);
        }
        i += n;
        count += n;
        if data.len() >= PACKET_BYTES || count >= CELLS_MAX {
            out.push(std::mem::take(&mut data));
        }
    }
    if !data.is_empty() {
        out.push(data);
    }
    out
}

/// Lit un paquet de blocs changés. `None` s'il est mal formé (rien n'en est gardé).
pub fn unpack_cells(data: &[u8]) -> Option<Vec<Cell>> {
    let mut at = 0;
    let base = get(data, &mut at)?;
    let mut out = Vec::new();
    let (mut cx, mut cz) = (0i64, 0i64);
    while at < data.len() {
        cx = cx.checked_add(get_signed(data, &mut at)?)?;
        cz = cz.checked_add(get_signed(data, &mut at)?)?;
        let n = get(data, &mut at)? as usize;
        // (Un chunk du monde : coordonnées de cases sur 32 bits.)
        let (x0, z0) = (
            i32::try_from(cx).ok()?.checked_mul(16)?,
            i32::try_from(cz).ok()?.checked_mul(16)?,
        );
        x0.checked_add(15)?;
        z0.checked_add(15)?;
        if n == 0 || n > CELLS_MAX || out.len() + n > CELLS_MAX {
            return None;
        }
        for _ in 0..n {
            let xz = *data.get(at)?;
            let y = *data.get(at + 1)?;
            at += 2;
            let block = u16::try_from(get(data, &mut at)?).ok()?;
            let seq = base.checked_add(get(data, &mut at)?)?;
            out.push((
                [x0 + (xz & 15) as i32, y as i32, z0 + (xz >> 4) as i32],
                block,
                seq,
            ));
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rnd(seed: &mut u64) -> u64 {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        *seed
    }

    #[test]
    fn cells_round_trip_whatever_they_are() {
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        for round in 0..300 {
            let n = (rnd(&mut seed) % 600) as usize * if round % 50 == 0 { 20 } else { 1 };
            let spread = [40i64, 3000, 4_000_000_000][round % 3];
            let mut cells: Vec<Cell> = (0..n)
                .map(|_| {
                    let x = (rnd(&mut seed) as i64 % spread) as i32;
                    let z = (rnd(&mut seed) as i64 % spread) as i32;
                    let y = (rnd(&mut seed) % 256) as i32;
                    let block = (rnd(&mut seed) % if round % 2 == 0 { 100 } else { 65536 }) as u16;
                    let seq = match round % 4 {
                        0 => rnd(&mut seed),
                        _ => 1_000_000 + rnd(&mut seed) % 5000,
                    };
                    ([x, y, z], block, seq)
                })
                .collect();
            // Les bords du monde aussi.
            if round % 7 == 0 {
                cells.push(([i32::MIN, 0, i32::MAX], 65535, u64::MAX));
                cells.push(([i32::MAX, 255, i32::MIN], 0, 0));
            }
            let packed = pack_cells(cells.clone());
            assert!(packed.len() >= cells.len().div_ceil(CELLS_MAX));
            let mut back: Vec<Cell> = Vec::new();
            for p in &packed {
                assert!(p.len() < 45_000, "tient dans un message : {}", p.len());
                back.extend(unpack_cells(p).expect("paquet lisible"));
            }
            cells.sort_unstable();
            back.sort_unstable();
            assert_eq!(back, cells);
        }
    }

    /// Ce qu'un joueur reçoit en un vingtième de seconde au milieu de 3000 joueurs qui
    /// construisent : 75 blocs dans une cinquantaine de chunks.
    #[test]
    fn a_crowd_tick_fits_in_a_few_hundred_bytes() {
        let mut seed = 77u64;
        let cells: Vec<Cell> = (0..75)
            .map(|i| {
                let x = 5000 + (rnd(&mut seed) % 120) as i32;
                let z = -3000 + (rnd(&mut seed) % 120) as i32;
                (
                    [x, 60 + (rnd(&mut seed) % 20) as i32, z],
                    (rnd(&mut seed) % 300) as u16,
                    9_000_000 + i,
                )
            })
            .collect();
        let packed = pack_cells(cells);
        assert_eq!(packed.len(), 1);
        assert!(
            packed[0].len() < 75 * 8,
            "{} octets pour 75 blocs",
            packed[0].len()
        );
        eprintln!(
            "75 blocs : {} octets ({:.1} par bloc)",
            packed[0].len(),
            packed[0].len() as f32 / 75.0
        );
        // Un seul bloc (la confirmation d'une modification) : une dizaine d'octets.
        let one = pack_cells(vec![([123, 64, -321], 37, 123_456)]);
        assert!(one[0].len() <= 12, "{}", one[0].len());
        eprintln!("1 bloc : {} octets", one[0].len());
    }

    #[test]
    fn a_malformed_packet_is_refused_whole() {
        let good = pack_cells(vec![([1, 2, 3], 4, 5), ([100, 2, 3], 4, 6)]).remove(0);
        assert!(unpack_cells(&good).is_some());
        // Tronqué n'importe où, ou n'importe quels octets : jamais de panique.
        for cut in 0..good.len() {
            let _ = unpack_cells(&good[..cut]);
        }
        assert!(unpack_cells(&good[..good.len() - 1]).is_none());
        assert!(unpack_cells(&[]).is_none());
        let mut seed = 5u64;
        for _ in 0..20_000 {
            let n = (rnd(&mut seed) % 40) as usize;
            let junk: Vec<u8> = (0..n).map(|_| rnd(&mut seed) as u8).collect();
            if let Some(cells) = unpack_cells(&junk) {
                assert!(cells.len() <= CELLS_MAX);
            }
        }
        // Un paquet qui annonce trop de blocs, ou un chunk hors du monde.
        let mut huge = Vec::new();
        put(&mut huge, 0);
        put_signed(&mut huge, 0);
        put_signed(&mut huge, 0);
        put(&mut huge, 1 << 40);
        assert!(unpack_cells(&huge).is_none());
        let mut far = Vec::new();
        put(&mut far, 0);
        put_signed(&mut far, i64::MAX / 2);
        put_signed(&mut far, 0);
        put(&mut far, 1);
        far.extend([0, 0, 0, 0]);
        assert!(unpack_cells(&far).is_none());
    }
}
