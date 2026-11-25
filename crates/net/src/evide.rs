//! (v312) L'Évidé partagé en construction en ligne : « tout le monde voit le même monstre, le
//! relais valide sa position, ses morsures, les points de vie qu'il ôte et ce qu'on lui
//! mange ».
//!
//! Un relais ne connaît pas le terrain : il ne peut pas faire marcher le monstre. Le jeu du
//! joueur chez qui il est sorti (le **meneur**) le mène, comme la foule d'essai (`crowd`) ; le
//! relais qui suit les positions de ce joueur **surveille** (`Watch`) et publie :
//!
//! - **sa place** : d'un paquet au suivant, elle n'avance pas plus vite qu'il ne peut courir
//!   ou tomber (au-delà, le relais ne la fait avancer que de ce qu'il aurait pu faire) ; un
//!   monstre trop loin de son meneur n'est pas publié ; seize au plus par meneur ;
//! - **ses morsures** : pas plus d'une toutes les `BITE_EVERY_S` secondes par monstre, et
//!   seulement sur un joueur que le relais voit à portée de la place qu'il lui connaît ; la
//!   victime s'ôte chez elle `BITE_HURTS` points de vie, jamais plus ;
//! - **sa vie et ce qu'on lui a mangé** : tenus par le relais. Un joueur demande à lui manger
//!   un morceau ; le relais vérifie qu'il est à portée, que le morceau n'est pas déjà mangé,
//!   lui ôte `EAT_HURTS` points de vie et le dit à tous. Dans les paquets qu'il publie, la vie
//!   et les morceaux mangés sont les siens, pas ceux qu'annonce le meneur.

use std::collections::HashMap;

/// Monstres au plus par meneur, taille d'un paquet d'états (octets).
pub const MAX_PER_OWNER: usize = 16;
pub const MAX_BYTES: usize = 1024;
/// Octets de la posture d'un monstre (voir le jeu : `evide::Anim`).
pub const POSE: usize = 10;
/// Sa vie (celle d'un joueur), ce qu'une de nos bouchées lui en retire, ce que sa morsure
/// nous en retire.
pub const LIFE: u8 = 20;
pub const EAT_HURTS: u8 = 4;
pub const BITE_HURTS: u8 = 2;
/// Publié aux joueurs à tant de chunks du meneur au plus ; pas publié s'il est à plus de tant
/// de blocs (à plat) de son meneur.
pub const CHUNKS: i32 = 8;
pub const LEAD_REACH: f32 = 96.0;
/// Sa vitesse au plus : à plat (blocs/s, la course d'un joueur et un peu de jeu), de haut en
/// bas (une chute) ; et le jeu laissé à chaque paquet (blocs).
pub const SPEED_MAX: f32 = 8.0;
pub const FALL_MAX: f32 = 60.0;
pub const SLACK: f32 = 0.6;
/// Une morsure par monstre toutes les tant de secondes au plus (il en fait une toutes les
/// 1,8 s) ; la victime à tant de blocs au plus de lui, à plat et en hauteur.
pub const BITE_EVERY_S: f64 = 1.4;
pub const BITE_FLAT: f32 = 3.2;
pub const BITE_HIGH: f32 = 4.5;
/// Pour lui manger un morceau : nos pieds à tant de blocs au plus des siens à plat (à plat
/// ventre il fait 3,5 blocs de long, et sa place est celle de l'avant de son corps), pas plus
/// bas que tant sous ses pieds, pas plus haut que tant au-dessus.
pub const EAT_FLAT: f32 = 5.5;
pub const EAT_BELOW: f32 = 3.0;
pub const EAT_ABOVE: f32 = 5.0;
/// Oublié par le relais après tant de secondes sans nouvelles de son meneur.
pub const FORGET_S: f64 = 10.0;

/// Drapeaux d'un monstre : il rampe, il crie (sa gueule ronde), il meurt.
pub const CRAWLING: u8 = 1;
pub const MAW: u8 = 2;
pub const DYING: u8 = 4;

/// Un Évidé, tel que les autres joueurs le voient.
#[derive(Clone, Debug, PartialEq)]
pub struct Mob {
    /// Son numéro chez son meneur.
    pub id: u16,
    /// Ses pieds.
    pub pos: [f32; 3],
    /// Son cap (rad, sur un octet).
    pub yaw: f32,
    /// La hauteur de sa boîte (blocs, au 1/50).
    pub tall: f32,
    pub pose: [u8; POSE],
    pub flags: u8,
    pub health: u8,
    /// Les morceaux qu'on lui a mangés, dans l'ordre : (pavé, cube).
    pub bites: Vec<[u8; 4]>,
}

fn angle(a: f32) -> u8 {
    (a.rem_euclid(std::f32::consts::TAU) / std::f32::consts::TAU * 256.0).round() as u32 as u8
}

/// Des monstres en octets (au plus `MAX_PER_OWNER`, les suivants sont laissés).
pub fn pack(mobs: &[Mob]) -> Vec<u8> {
    let mobs = &mobs[..mobs.len().min(MAX_PER_OWNER)];
    let mut out = Vec::with_capacity(1 + mobs.len() * 48);
    out.push(mobs.len() as u8);
    for m in mobs {
        out.extend_from_slice(&m.id.to_le_bytes());
        for v in m.pos {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.push(angle(m.yaw));
        out.push((m.tall.clamp(0.0, 5.0) * 50.0).round() as u8);
        out.extend_from_slice(&m.pose);
        out.push(m.flags);
        out.push(m.health);
        let bites = &m.bites[..m.bites.len().min((LIFE / EAT_HURTS) as usize)];
        out.push(bites.len() as u8);
        for b in bites {
            out.extend_from_slice(b);
        }
    }
    out
}

/// Lit un paquet de monstres. `None` s'il est mal formé ou absurde (rien n'en est gardé).
pub fn unpack(data: &[u8]) -> Option<Vec<Mob>> {
    if data.len() > MAX_BYTES {
        return None;
    }
    let (&n, rest) = data.split_first()?;
    if n as usize > MAX_PER_OWNER {
        return None;
    }
    let mut rest = Cursor(rest);
    let mut out: Vec<Mob> = Vec::with_capacity(n as usize);
    for _ in 0..n {
        let mut take = |k: usize| rest.take(k);
        let id = u16::from_le_bytes(take(2)?.try_into().ok()?);
        let mut pos = [0f32; 3];
        for p in &mut pos {
            *p = f32::from_le_bytes(take(4)?.try_into().ok()?);
        }
        if pos.iter().any(|v| !v.is_finite() || v.abs() > 4.0e7) {
            return None;
        }
        let yaw = take(1)?[0] as f32 / 256.0 * std::f32::consts::TAU;
        let tall = take(1)?[0] as f32 / 50.0;
        let pose: [u8; POSE] = take(POSE)?.try_into().ok()?;
        let flags = take(1)?[0];
        let health = take(1)?[0];
        let count = take(1)?[0] as usize;
        if flags & !(CRAWLING | MAW | DYING) != 0
            || health > LIFE
            || count > (LIFE / EAT_HURTS) as usize
            || out.iter().any(|m| m.id == id)
        {
            return None;
        }
        let mut bites = Vec::with_capacity(count);
        for _ in 0..count {
            let b: [u8; 4] = take(4)?.try_into().ok()?;
            if !bite_sane(b[0], [b[1], b[2], b[3]]) || bites.contains(&b) {
                return None;
            }
            bites.push(b);
        }
        out.push(Mob {
            id,
            pos,
            yaw,
            tall,
            pose,
            flags,
            health,
            bites,
        });
    }
    rest.0.is_empty().then_some(out)
}

/// Ce qu'il reste à lire d'un paquet.
struct Cursor<'a>(&'a [u8]);

impl<'a> Cursor<'a> {
    fn take(&mut self, k: usize) -> Option<&'a [u8]> {
        if self.0.len() < k {
            return None;
        }
        let (a, b) = self.0.split_at(k);
        self.0 = b;
        Some(a)
    }
}

/// Un morceau possible : un des sept pavés de son corps, un cube dans des bornes larges (le
/// jeu connaît la grille exacte).
pub fn bite_sane(part: u8, cell: [u8; 3]) -> bool {
    part < 7 && cell.iter().all(|c| *c < 32)
}

fn flat(a: [f32; 3], b: [f32; 3]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

/// Ce que le relais sait d'un monstre.
#[derive(Clone, Debug)]
struct Seen {
    pos: [f32; 3],
    at: f64,
    health: u8,
    bites: Vec<[u8; 4]>,
    last_bite: f64,
}

/// La surveillance des Évidés d'un monde par un relais (voir le haut du fichier). Les
/// heures sont en secondes d'une même horloge qui ne recule pas.
#[derive(Default)]
pub struct Watch {
    mobs: HashMap<(String, u16), Seen>,
}

impl Watch {
    /// Paquet d'états du meneur `owner` (le pseudo de sa session, en minuscules), qui est en
    /// `lead` : rend le paquet à publier aux joueurs proches, ou `None` s'il n'y a rien à
    /// publier (illisible, ou plus aucun monstre à portée). Dans le paquet rendu : la place de
    /// chaque monstre telle que le relais l'admet, sa vie et ses morceaux mangés tels que le
    /// relais les tient.
    pub fn states(&mut self, owner: &str, lead: [f32; 3], now: f64, data: &[u8]) -> Option<Vec<u8>> {
        let mut mobs = unpack(data)?;
        let mut held = self.mobs.keys().filter(|k| k.0 == owner).count();
        mobs.retain_mut(|m| {
            if flat(m.pos, lead) > LEAD_REACH {
                return false;
            }
            let key = (owner.to_string(), m.id);
            let seen = match self.mobs.get_mut(&key) {
                Some(seen) => {
                    // Pas plus vite qu'il ne peut courir, ni tomber.
                    let dt = (now - seen.at).clamp(0.0, 2.0) as f32;
                    let (far, reach) = (flat(m.pos, seen.pos), SPEED_MAX * dt + SLACK);
                    if far > reach {
                        let k = reach / far;
                        m.pos[0] = seen.pos[0] + (m.pos[0] - seen.pos[0]) * k;
                        m.pos[2] = seen.pos[2] + (m.pos[2] - seen.pos[2]) * k;
                    }
                    let high = FALL_MAX * dt + SLACK;
                    m.pos[1] = m.pos[1].clamp(seen.pos[1] - high, seen.pos[1] + high);
                    seen.pos = m.pos;
                    seen.at = now;
                    seen
                }
                None => {
                    if held >= MAX_PER_OWNER {
                        return false;
                    }
                    held += 1;
                    // Vu pour la première fois (son meneur arrive avec lui) : les morceaux
                    // qu'il lui manque déjà sont crus ; sa vie s'en déduit.
                    self.mobs.entry(key).or_insert(Seen {
                        pos: m.pos,
                        at: now,
                        health: LIFE.saturating_sub(EAT_HURTS.saturating_mul(m.bites.len() as u8)),
                        bites: m.bites.clone(),
                        last_bite: f64::MIN,
                    })
                }
            };
            m.health = seen.health;
            m.bites.clone_from(&seen.bites);
            if seen.health == 0 {
                m.flags |= DYING;
            }
            true
        });
        (!mobs.is_empty()).then(|| pack(&mobs))
    }

    /// Le monstre `id` de `owner` mord un joueur que le relais voit en `victim` : permis ?
    /// (Vivant, pas plus d'une morsure toutes les `BITE_EVERY_S` secondes, la victime à
    /// portée de la place que le relais lui connaît.)
    pub fn bite(&mut self, owner: &str, id: u16, victim: [f32; 3], now: f64) -> bool {
        let Some(seen) = self.mobs.get_mut(&(owner.to_string(), id)) else {
            return false;
        };
        let near = flat(seen.pos, victim) <= BITE_FLAT && (victim[1] - seen.pos[1]).abs() <= BITE_HIGH;
        if seen.health == 0 || now - seen.last_bite < BITE_EVERY_S || now - seen.at > 2.0 || !near {
            return false;
        }
        seen.last_bite = now;
        true
    }

    /// Un joueur que le relais voit en `eater` (ses pieds) mange le morceau (`part`, `cell`)
    /// du monstre `id` de `owner` : la vie qu'il lui reste si c'est permis (vivant, à portée,
    /// morceau possible et pas déjà mangé).
    pub fn eat(
        &mut self,
        owner: &str,
        id: u16,
        part: u8,
        cell: [u8; 3],
        eater: [f32; 3],
        now: f64,
    ) -> Option<u8> {
        let seen = self.mobs.get_mut(&(owner.to_string(), id))?;
        let bite = [part, cell[0], cell[1], cell[2]];
        let dy = eater[1] - seen.pos[1];
        let near = flat(seen.pos, eater) <= EAT_FLAT && (-EAT_BELOW..=EAT_ABOVE).contains(&dy);
        if seen.health == 0
            || now - seen.at > 2.0
            || !near
            || !bite_sane(part, cell)
            || seen.bites.contains(&bite)
        {
            return None;
        }
        seen.bites.push(bite);
        seen.health = seen.health.saturating_sub(EAT_HURTS);
        Some(seen.health)
    }

    /// Oublie les monstres dont le meneur ne dit plus rien depuis `FORGET_S` secondes.
    pub fn sweep(&mut self, now: f64) {
        self.mobs.retain(|_, s| now - s.at <= FORGET_S);
    }

    /// Combien de monstres sont suivis.
    pub fn len(&self) -> usize {
        self.mobs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.mobs.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mob(id: u16, pos: [f32; 3]) -> Mob {
        Mob {
            id,
            pos,
            yaw: 1.5,
            tall: 3.5,
            pose: [7; POSE],
            flags: CRAWLING,
            health: LIFE,
            bites: Vec::new(),
        }
    }

    /// Un paquet se relit tel quel (aux arrondis des angles et de la hauteur près) ; tronqué,
    /// trop long, absurde ou tiré au hasard : refusé en entier, sans jamais arrêter le relais.
    #[test]
    fn packets_read_back_and_bad_ones_are_refused() {
        let mut a = mob(3, [10.5, 64.0, -7.25]);
        a.bites = vec![[0, 1, 2, 3], [5, 7, 15, 7]];
        a.health = 12;
        let b = mob(9, [-3.0, 70.0, 2.0]);
        let data = pack(&[a.clone(), b.clone()]);
        let back = unpack(&data).unwrap();
        assert_eq!(back.len(), 2);
        assert_eq!((back[0].id, back[0].pos, back[0].health), (3, a.pos, 12));
        assert_eq!((&back[0].bites, back[0].pose, back[0].flags), (&a.bites, a.pose, a.flags));
        assert!((back[0].yaw - 1.5).abs() < 0.02 && (back[0].tall - 3.5).abs() < 0.011);
        assert_eq!(back[1].pos, b.pos);
        // Tronqué, avec un octet en trop, deux fois le même numéro, une place infinie.
        assert_eq!(unpack(&data[..data.len() - 1]), None);
        let mut long = data.clone();
        long.push(0);
        assert_eq!(unpack(&long), None);
        assert_eq!(unpack(&pack(&[b.clone(), b.clone()])), None);
        assert_eq!(unpack(&pack(&[mob(1, [f32::NAN, 0.0, 0.0])])), None);
        assert_eq!(unpack(&pack(&[mob(1, [1.0e9, 0.0, 0.0])])), None);
        // Un morceau impossible (pavé 9), une vie de trop, un drapeau inconnu.
        let mut bad = mob(1, [0.0; 3]);
        bad.bites = vec![[9, 0, 0, 0]];
        assert_eq!(unpack(&pack(&[bad])), None);
        let mut bad = mob(1, [0.0; 3]);
        bad.health = 99;
        assert_eq!(unpack(&pack(&[bad])), None);
        let mut bad = mob(1, [0.0; 3]);
        bad.flags = 64;
        assert_eq!(unpack(&pack(&[bad])), None);
        // N'importe quels octets : jamais d'arrêt.
        let mut seed = 0x1234_5678u32;
        for n in 0..20_000 {
            let bytes: Vec<u8> = (0..n % 90)
                .map(|_| {
                    seed ^= seed << 13;
                    seed ^= seed >> 17;
                    seed ^= seed << 5;
                    seed as u8
                })
                .collect();
            let _ = unpack(&bytes);
        }
        // Pas plus de seize par paquet.
        let many: Vec<Mob> = (0..20).map(|i| mob(i, [0.0; 3])).collect();
        assert_eq!(unpack(&pack(&many)).unwrap().len(), MAX_PER_OWNER);
    }

    /// Le relais tient sa place : un monstre que son meneur déplace d'un coup n'avance, pour
    /// les autres, que de ce qu'il aurait pu courir ; trop loin de son meneur, il n'est pas
    /// publié ; seize au plus par meneur.
    #[test]
    fn the_relay_holds_its_place() {
        let mut w = Watch::default();
        let lead = [0.0, 64.0, 0.0];
        let read = |d: Option<Vec<u8>>| unpack(&d.unwrap()).unwrap();
        let first = read(w.states("op", lead, 0.0, &pack(&[mob(1, [5.0, 64.0, 0.0])])));
        assert_eq!(first[0].pos, [5.0, 64.0, 0.0]);
        // 0,05 s plus tard, il serait 30 blocs plus loin : il n'a avancé que d'un bloc.
        let jump = read(w.states("op", lead, 0.05, &pack(&[mob(1, [35.0, 64.0, 0.0])])));
        assert!((jump[0].pos[0] - (5.0 + SPEED_MAX * 0.05 + SLACK)).abs() < 1e-3, "{:?}", jump[0].pos);
        // Une course normale passe telle quelle.
        let run = read(w.states("op", lead, 0.10, &pack(&[mob(1, [jump[0].pos[0] + 0.3, 64.0, 0.2])])));
        assert_eq!(run[0].pos, [jump[0].pos[0] + 0.3, 64.0, 0.2]);
        // À 200 blocs de son meneur : pas publié ; aucun autre : rien à publier.
        assert_eq!(w.states("op", lead, 0.2, &pack(&[mob(2, [200.0, 64.0, 0.0])])), None);
        // Seize d'un coup, puis un dix-septième : seize suivis, pas plus.
        let all: Vec<Mob> = (10..26).map(|i| mob(i, [1.0, 64.0, 1.0])).collect();
        let seen = read(w.states("eve", lead, 0.3, &pack(&all)));
        assert_eq!(seen.len(), MAX_PER_OWNER);
        assert_eq!(w.states("eve", lead, 0.4, &pack(&[mob(99, [1.0, 64.0, 1.0])])), None);
        assert_eq!(w.len(), 1 + MAX_PER_OWNER);
        // Sans nouvelles : oubliés.
        w.sweep(0.4 + FORGET_S + 1.0);
        assert!(w.is_empty());
        // Illisible : rien.
        assert_eq!(w.states("op", lead, 20.0, &[1, 2, 3]), None);
    }

    /// Ses morsures : sur un joueur à portée seulement, pas plus d'une toutes les 1,4 s, pas
    /// d'un monstre mort ni inconnu.
    #[test]
    fn the_relay_allows_a_bite_only_in_reach_and_in_time() {
        let mut w = Watch::default();
        let lead = [0.0, 64.0, 0.0];
        w.states("op", lead, 0.0, &pack(&[mob(1, [5.0, 64.0, 0.0])]));
        // Inconnu ; trop loin ; trop haut.
        assert!(!w.bite("op", 2, [5.0, 64.0, 0.0], 0.1));
        assert!(!w.bite("eve", 1, [5.0, 64.0, 0.0], 0.1));
        assert!(!w.bite("op", 1, [5.0 + BITE_FLAT + 0.5, 64.0, 0.0], 0.1));
        assert!(!w.bite("op", 1, [5.0, 64.0 + BITE_HIGH + 0.5, 0.0], 0.1));
        // À portée : permis, puis plus avant 1,4 s.
        assert!(w.bite("op", 1, [6.5, 64.0, 0.5], 0.1));
        assert!(!w.bite("op", 1, [6.5, 64.0, 0.5], 1.0));
        w.states("op", lead, 1.5, &pack(&[mob(1, [5.0, 64.0, 0.0])]));
        assert!(w.bite("op", 1, [6.5, 64.0, 0.5], 1.6));
        // Son meneur ne dit plus rien depuis 2 s : plus de morsure.
        assert!(!w.bite("op", 1, [6.5, 64.0, 0.5], 4.0));
    }

    /// Sa vie et ce qu'on lui mange : au relais. Cinq bouchées le tuent ; pas deux fois le
    /// même morceau, pas de trop loin ; le meneur a beau l'annoncer entier, le paquet publié
    /// dit ce que le relais a compté.
    #[test]
    fn the_relay_keeps_its_life_and_what_was_eaten() {
        let mut w = Watch::default();
        let lead = [0.0, 64.0, 0.0];
        let at = [5.0, 64.0, 0.0];
        w.states("op", lead, 0.0, &pack(&[mob(1, at)]));
        let me = [6.0, 64.0, 1.0];
        assert_eq!(w.eat("op", 1, 2, [1, 2, 3], me, 0.1), Some(16));
        // Le même morceau ; un pavé impossible ; de trop loin ; un monstre inconnu.
        assert_eq!(w.eat("op", 1, 2, [1, 2, 3], me, 0.2), None);
        assert_eq!(w.eat("op", 1, 7, [0, 0, 0], me, 0.2), None);
        assert_eq!(w.eat("op", 1, 2, [0, 0, 0], [5.0 + EAT_FLAT + 1.0, 64.0, 0.0], 0.2), None);
        assert_eq!(w.eat("op", 1, 2, [0, 0, 0], [5.0, 64.0 + EAT_ABOVE + 1.0, 0.0], 0.2), None);
        assert_eq!(w.eat("op", 7, 2, [0, 0, 0], me, 0.2), None);
        // Le meneur l'annonce entier : le relais publie sa vie à lui, et le morceau.
        let out = unpack(&w.states("op", lead, 0.3, &pack(&[mob(1, at)])).unwrap()).unwrap();
        assert_eq!((out[0].health, &out[0].bites), (16, &vec![[2, 1, 2, 3]]));
        assert_eq!(out[0].flags & DYING, 0);
        for (k, want) in [(1u8, 12u8), (2, 8), (3, 4), (4, 0)] {
            assert_eq!(w.eat("op", 1, 0, [k, 0, 0], me, 0.3 + k as f64 * 0.01), Some(want));
        }
        // Mort : plus rien à manger, plus de morsure ; publié mourant.
        assert_eq!(w.eat("op", 1, 1, [0, 0, 0], me, 0.5), None);
        assert!(!w.bite("op", 1, me, 5.0));
        let out = unpack(&w.states("op", lead, 0.6, &pack(&[mob(1, at)])).unwrap()).unwrap();
        assert_eq!((out[0].health, out[0].bites.len()), (0, 5));
        assert_ne!(out[0].flags & DYING, 0);
        // Un monstre qui arrive déjà entamé (son meneur entre avec lui) : cru, sa vie s'en
        // déduit.
        let mut old = mob(4, at);
        old.bites = vec![[1, 0, 0, 0], [1, 1, 0, 0]];
        let out = unpack(&w.states("zoe", lead, 1.0, &pack(&[old])).unwrap()).unwrap();
        assert_eq!((out[0].health, out[0].bites.len()), (12, 2));
    }
}
