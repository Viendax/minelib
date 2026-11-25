//! (v309) La carte bouclée : « si on se tp plus loin, on fait le tour de la map » (Viendax,
//! 25 novembre). Sur x comme sur z, la carte fait `LAP` blocs de tour : passé +`HALF`, on
//! arrive à −`HALF`, et tout droit on revient à son point de départ. Pas de bord, pas de
//! farlands : la place d'un bloc reste un entier de 32 bits, celle du joueur est comptée
//! depuis une origine proche de lui (`World::origin`).
//!
//! Le relief, les grottes et les arbres viennent de bruits et de tirages faits sur des
//! grilles de cases (`noise`) ; `LAP` est un multiple de la taille de chacune de ces cases :
//! en faisant boucler le numéro des cases (`wrap_cell`), chaque bruit se répète exactement à
//! chaque tour. De part et d'autre de la limite, deux colonnes voisines sont deux voisines
//! ordinaires du même bruit : pas de raccord. Dans la carte, les numéros des cases ne changent
//! pas : les mondes existants restent les mêmes, bloc pour bloc.

use crate::CHUNK_SIZE;

/// Tour de la carte, en blocs (sur x et sur z). Multiple de 2 240 blocs, le plus petit commun
/// multiple des cases des bruits du monde (relief : 320, 160, 80 ; 70, 35, 17,5, 8,75, 4,375 ;
/// 140 ; grottes : 28) : chacun s'y répète exactement, avec un nombre pair de cases. Donc aussi
/// de 16 : aucun chunk n'est à cheval sur la limite. Le plus grand sous 2³¹ (958 698 fois
/// 2 240) : les places des blocs restent des entiers de 32 bits.
pub const LAP: i64 = 2_147_483_520;
/// La carte va de −`HALF` (compris) à `HALF` (exclu), sur x et sur z.
pub const HALF: i64 = LAP / 2;
/// Le tour, en chunks.
pub const LAP_CHUNKS: i64 = LAP / CHUNK_SIZE as i64;

const _: () = assert!(LAP % 2240 == 0 && (LAP / 2240) % 2 == 0 && LAP < 1 << 31);

/// Le bloc `v` ramené sur la carte : dans [−`HALF`, `HALF`).
pub fn wrap_block(v: i64) -> i32 {
    ((v + HALF).rem_euclid(LAP) - HALF) as i32
}

/// Le chunk `v` ramené sur la carte.
pub fn wrap_chunk(v: i64) -> i32 {
    let half = LAP_CHUNKS / 2;
    ((v + half).rem_euclid(LAP_CHUNKS) - half) as i32
}

/// La place `v` (blocs, double précision) ramenée sur la carte.
pub fn wrap_f64(v: f64) -> f64 {
    if !v.is_finite() {
        return 0.0;
    }
    let l = LAP as f64;
    let w = v - l * ((v + HALF as f64) / l).floor();
    // (Un arrondi tout au bord peut donner +HALF : c'est −HALF.)
    if w >= HALF as f64 {
        w - l
    } else {
        w
    }
}

/// La place entière `v` (blocs, exacte, aussi loin qu'on veut) : (tours faits vers les +,
/// place sur la carte). `v` = tours × `LAP` + place.
pub fn split(v: i128) -> (i128, i64) {
    let l = LAP as i128;
    let laps = (v + HALF as i128).div_euclid(l);
    (laps, (v - laps * l) as i64)
}

/// Le numéro `i` d'une case de bruit, bouclé : `cells` cases par tour (un nombre pair). Dans
/// la carte (|`i`| < `cells` / 2), le même numéro.
pub fn wrap_cell(i: i64, cells: i64) -> i32 {
    let half = cells / 2;
    ((i + half).rem_euclid(cells) - half) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn places_come_back_on_the_map() {
        assert_eq!(wrap_block(0), 0);
        assert_eq!(wrap_block(HALF - 1), (HALF - 1) as i32);
        assert_eq!(wrap_block(HALF), -HALF as i32);
        assert_eq!(wrap_block(-HALF), -HALF as i32);
        assert_eq!(wrap_block(-HALF - 1), (HALF - 1) as i32);
        assert_eq!(wrap_block(5 * LAP + 7), 7);
        assert_eq!(wrap_chunk(LAP_CHUNKS / 2), -(LAP_CHUNKS / 2) as i32);
        assert_eq!(wrap_chunk(-3), -3);
        assert_eq!(wrap_f64(HALF as f64), -HALF as f64);
        assert_eq!(wrap_f64(HALF as f64 + 0.5), -HALF as f64 + 0.5);
        assert_eq!(wrap_f64(-HALF as f64 - 0.25), HALF as f64 - 0.25);
        assert_eq!(wrap_f64(12.5), 12.5);
        assert_eq!(wrap_f64(f64::NAN), 0.0);
        // Exact, même très loin.
        let v: i128 = 170_141_183_460_469_231_731_687_303_715_884_105_727 / 3;
        let (laps, at) = split(v);
        assert_eq!(laps * LAP as i128 + at as i128, v);
        assert!((-HALF..HALF).contains(&at));
        assert_eq!(split(HALF as i128), (1, -HALF));
        assert_eq!(split(-HALF as i128 - 1), (-1, HALF - 1));
        assert_eq!(split(0), (0, 0));
    }

    #[test]
    fn noise_cells_keep_their_number_on_the_map() {
        let n = 30_678_336;
        for i in [-n / 2, -n / 2 + 1, -5, 0, 5, n / 2 - 1] {
            assert_eq!(wrap_cell(i, n), i as i32);
        }
        assert_eq!(wrap_cell(n / 2, n), (-n / 2) as i32);
        assert_eq!(wrap_cell(-n / 2 - 1, n), (n / 2 - 1) as i32);
    }
}
