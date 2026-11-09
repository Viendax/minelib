//! Grottes et salles cachées : on ne dessine que les tranches de chunk (16 blocs de haut)
//! qu'on peut voir depuis la caméra en passant par l'air.
//!
//! Chaque tranche connaît ses passages (quelle face communique avec quelle autre par de
//! l'air, voir `mesh::section_connectivity`). Depuis la tranche de la caméra, un parcours
//! en largeur traverse les passages en s'éloignant toujours de la caméra (jamais de retour
//! dans une direction déjà prise en sens inverse), à partir de la tranche de la caméra et de
//! ses voisines. Ce qui n'est pas atteint est caché
//! derrière la roche : une grotte fermée sous la surface n'est pas dessinée, une grotte qui
//! s'ouvre sur l'extérieur l'est. Calculé en arrière-plan, seulement quand la caméra change
//! de tranche ou que des chunks changent.

use std::collections::{HashMap, VecDeque};

use minelib_world::mesh::SECTIONS;
use minelib_world::ChunkPos;

/// Masque « toutes les tranches visibles ».
pub const ALL: u16 = u16::MAX;

/// Tranches visibles de chaque chunk de `conn` depuis la tranche `cam_sec` du chunk `cam`.
/// Vide si la caméra n'est pas dans un chunk connu (alors rien n'est caché).
pub fn visible_sections(
    conn: &HashMap<ChunkPos, [u64; SECTIONS]>,
    cam: ChunkPos,
    cam_sec: i32,
) -> HashMap<ChunkPos, u16> {
    let mut seen: HashMap<ChunkPos, u16> = HashMap::with_capacity(conn.len());
    if !conn.contains_key(&cam) {
        return seen;
    }
    for p in conn.keys() {
        seen.insert(*p, 0);
    }
    let start = cam_sec.clamp(0, SECTIONS as i32 - 1);
    // (chunk, tranche, face d'entrée, directions déjà prises)
    let mut queue: VecDeque<(ChunkPos, i32, Option<usize>, u8)> = VecDeque::new();
    const STEP: [(i32, i32, i32); 6] = [
        (1, 0, 0),
        (-1, 0, 0),
        (0, 1, 0),
        (0, -1, 0),
        (0, 0, 1),
        (0, 0, -1),
    ];
    // Départ : la tranche de la caméra, puis ses 6 voisines par une face, comme si on y
    // entrait depuis elle. Le résultat reste juste quand la caméra passe dans une tranche
    // voisine, le temps que le calcul suivant arrive (pas de terrain manquant en sortant
    // d'une grotte).
    seen.insert(cam, 1 << start);
    queue.push_back((cam, start, None, 0));
    for (d, (dx, dy, dz)) in STEP.iter().enumerate() {
        let (p, sec) = (
            ChunkPos {
                x: cam.x + dx,
                z: cam.z + dz,
            },
            start + dy,
        );
        if !(0..SECTIONS as i32).contains(&sec) {
            continue;
        }
        if let Some(m) = seen.get_mut(&p) {
            if *m & (1 << sec) == 0 {
                *m |= 1 << sec;
                queue.push_back((p, sec, Some(d ^ 1), 1 << d));
            }
        }
    }
    while let Some((pos, sec, from, dirs)) = queue.pop_front() {
        let links = conn[&pos][sec as usize];
        for (d, (dx, dy, dz)) in STEP.iter().enumerate() {
            // Jamais en sens inverse d'une direction déjà prise : on s'éloigne de la caméra.
            if dirs & (1 << (d ^ 1)) != 0 {
                continue;
            }
            if let Some(f) = from {
                if links & minelib_world::mesh::link(f, d) == 0 {
                    continue;
                }
            }
            let (np, ns) = (
                ChunkPos {
                    x: pos.x + dx,
                    z: pos.z + dz,
                },
                sec + dy,
            );
            if !(0..SECTIONS as i32).contains(&ns) {
                continue;
            }
            let Some(mask) = seen.get_mut(&np) else {
                continue;
            };
            if *mask & (1 << ns) != 0 {
                continue;
            }
            *mask |= 1 << ns;
            queue.push_back((np, ns, Some(d ^ 1), dirs | (1 << d)));
        }
    }
    seen
}

#[cfg(test)]
mod tests {
    use super::*;
    use minelib_world::mesh::ALL_LINKS;

    #[test]
    fn sealed_cave_is_hidden_open_one_is_seen() {
        // 3 × 3 chunks : tranches 0..3 en roche pleine (aucun passage), au-dessus de l'air.
        let mut conn = HashMap::new();
        for x in -1..=1 {
            for z in -1..=1 {
                let mut c = [ALL_LINKS; SECTIONS];
                for s in c.iter_mut().take(4) {
                    *s = 0;
                }
                conn.insert(ChunkPos { x, z }, c);
            }
        }
        // Caméra dehors (tranche 6) : les tranches 0..2 (sous la roche) sont cachées. La
        // tranche 3 touche l'air : atteinte, mais sans géométrie (ses faces visibles sont
        // rangées dans la tranche d'air au-dessus).
        let v = visible_sections(&conn, ChunkPos { x: 0, z: 0 }, 6);
        let m = v[&ChunkPos { x: 1, z: 1 }];
        assert_eq!(m & 0b111, 0, "roche pleine cachée : {m:016b}");
        assert!(m & (1 << 4) != 0);
        // Un puits ouvert jusqu'en bas dans le chunk (1, 0) : sa tranche 0 devient visible.
        let mut shaft = [ALL_LINKS; SECTIONS];
        shaft[..4]
            .iter_mut()
            .for_each(|s| *s = mesh_link_vertical());
        conn.insert(ChunkPos { x: 1, z: 0 }, shaft);
        let v = visible_sections(&conn, ChunkPos { x: 0, z: 0 }, 6);
        assert!(
            v[&ChunkPos { x: 1, z: 0 }] & 1 != 0,
            "fond du puits visible"
        );
        // Caméra dans une poche fermée (tranche 1, roche tout autour) : les autres grottes
        // fermées restent cachées.
        let v = visible_sections(&conn, ChunkPos { x: -1, z: -1 }, 1);
        assert_eq!(v[&ChunkPos { x: 1, z: 1 }] & 0b111, 0);
        assert!(
            v[&ChunkPos { x: -1, z: -1 }] & 0b10 != 0,
            "sa propre poche visible"
        );
        // Caméra hors des chunks connus : rien n'est caché.
        assert!(visible_sections(&conn, ChunkPos { x: 9, z: 9 }, 6).is_empty());
    }

    fn mesh_link_vertical() -> u64 {
        use minelib_world::mesh::link;
        link(2, 3) | link(3, 2) | link(2, 2) | link(3, 3)
    }
}

#[cfg(test)]
mod bench {
    use super::*;
    use minelib_world::mesh::ALL_LINKS;

    #[test]
    #[ignore]
    fn bfs_time_at_42_chunks() {
        let mut conn = HashMap::new();
        for x in -42..=42 {
            for z in -42..=42 {
                let mut c = [ALL_LINKS; SECTIONS];
                for s in c.iter_mut().take(4) {
                    *s = 0;
                }
                conn.insert(ChunkPos { x, z }, c);
            }
        }
        let t = std::time::Instant::now();
        let v = visible_sections(&conn, ChunkPos { x: 0, z: 0 }, 5);
        println!("{} chunks, {:?}", v.len(), t.elapsed());
    }
}
