//! Paquets compacts de positions de joueurs.
//!
//! Dans une foule, chaque joueur reçoit des centaines de positions par seconde : c'est de
//! loin ce qui pèse le plus (270 kbit/s par joueur, 26 Mbit/s à envoyer pour le joueur qui
//! héberge un relais de région d'une centaine de joueurs). Une par message, chacune
//! coûtait 67 octets sur le fil : le pseudo en toutes lettres, sept nombres sur quatre
//! octets, l'en-tête et le sceau du message chiffré. Regroupées, elles en coûtent une
//! dizaine : le pseudo n'est dit qu'une fois par connexion, la position s'écrit au 1/32 de
//! bloc par rapport à la première du paquet, les angles sur un octet.

/// Position d'un joueur, telle que les autres la voient.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pose {
    pub pos: [f32; 3],
    pub yaw: f32,
    pub pitch: f32,
    pub crouch: f32,
    pub speed: f32,
    /// (v165) Son ping avec son relais (ms), pour la liste des joueurs (Tab) : il ne part
    /// qu'avec une position sur huit environ (à 4 ms près, au plus 1020).
    pub ping: Option<u16>,
}

/// Paquets écrits jusqu'ici : le ping de chaque joueur part à tour de rôle.
static ROUND: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Positions au plus par paquet.
pub const POSES_MAX: usize = 2048;
/// Pas de la position (blocs) : 1/32, soit 3 cm.
const STEP: f32 = 32.0;
/// Au-delà (en blocs), une position est absurde et n'est pas écrite.
const FAR: f32 = 4.0e7;

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

impl Pose {
    /// Rien d'infini ni d'absurde (ce qu'un jeu modifié pourrait envoyer).
    pub fn sane(&self) -> bool {
        self.pos.iter().all(|v| v.is_finite() && v.abs() < FAR)
            && [self.yaw, self.pitch, self.crouch, self.speed]
                .iter()
                .all(|v| v.is_finite())
    }
}

/// Écrit des positions en un paquet : (numéro du joueur sur ce relais, son pseudo si la
/// connexion ne le connaît pas encore, sa position). Les positions absurdes ne sont pas
/// écrites ; au plus `POSES_MAX`.
pub fn pack_poses(entries: &mut Vec<(u64, Option<&str>, Pose)>) -> Vec<u8> {
    entries.retain(|e| e.2.sane());
    entries.truncate(POSES_MAX);
    entries.sort_unstable_by_key(|e| e.0);
    let fixed = |v: f32| (v * STEP).round() as i64;
    let base = entries.first().map_or([0; 3], |e| e.2.pos.map(fixed));
    let mut out = Vec::with_capacity(16 + entries.len() * 12);
    for b in base {
        put_signed(&mut out, b);
    }
    let mut last = 0u64;
    let round = ROUND.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    for (id, name, p) in entries.iter() {
        put(&mut out, id - last);
        last = *id;
        // Pseudo présent ou non, accroupi sur 2 bits, ping présent ou non (une position sur
        // huit, à tour de rôle), vitesse sur 4 (par pas de 2 blocs/s).
        let crouch = (p.crouch.clamp(0.0, 1.0) * 3.0).round() as u8;
        let speed = (p.speed.clamp(0.0, 30.0) / 2.0).round() as u8;
        let ping = p.ping.filter(|_| id.wrapping_add(round) % 8 == 0);
        let name = name.map(|n| &n.as_bytes()[..n.len().min(255)]);
        out.push(name.is_some() as u8 | crouch << 1 | (ping.is_some() as u8) << 3 | speed << 4);
        if let Some(n) = name {
            out.push(n.len() as u8);
            out.extend_from_slice(n);
        }
        if let Some(ms) = ping {
            out.push((ms / 4).min(255) as u8);
        }
        for (v, b) in p.pos.iter().zip(base) {
            put_signed(&mut out, fixed(*v) - b);
        }
        let turn = std::f32::consts::TAU;
        out.push((p.yaw.rem_euclid(turn) / turn * 256.0).round() as u32 as u8);
        let half = std::f32::consts::FRAC_PI_2;
        out.push(((p.pitch.clamp(-half, half) + half) / (2.0 * half) * 255.0).round() as u8);
    }
    out
}

/// Lit un paquet de positions. `None` s'il est mal formé (rien n'en est gardé).
pub fn unpack_poses(data: &[u8]) -> Option<Vec<(u64, Option<String>, Pose)>> {
    let mut at = 0;
    let mut base = [0i64; 3];
    for b in &mut base {
        *b = get_signed(data, &mut at)?;
    }
    let mut out = Vec::new();
    let mut id = 0u64;
    while at < data.len() {
        if out.len() >= POSES_MAX {
            return None;
        }
        id = id.checked_add(get(data, &mut at)?)?;
        let flags = *data.get(at)?;
        at += 1;
        let name = if flags & 1 == 1 {
            let n = *data.get(at)? as usize;
            let bytes = data.get(at + 1..at + 1 + n)?;
            at += 1 + n;
            Some(std::str::from_utf8(bytes).ok()?.to_string())
        } else {
            None
        };
        let ping = if flags & 8 != 0 {
            let v = *data.get(at)? as u16 * 4;
            at += 1;
            Some(v)
        } else {
            None
        };
        let mut pos = [0f32; 3];
        for (a, p) in pos.iter_mut().enumerate() {
            *p = base[a].checked_add(get_signed(data, &mut at)?)? as f32 / STEP;
        }
        let yaw = *data.get(at)? as f32 / 256.0 * std::f32::consts::TAU;
        let pitch =
            *data.get(at + 1)? as f32 / 255.0 * std::f32::consts::PI - std::f32::consts::FRAC_PI_2;
        at += 2;
        let pose = Pose {
            pos,
            yaw,
            pitch,
            crouch: (flags >> 1 & 3) as f32 / 3.0,
            speed: (flags >> 4) as f32 * 2.0,
            ping,
        };
        if !pose.sane() {
            return None;
        }
        out.push((id, name, pose));
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

    fn unit(seed: &mut u64) -> f32 {
        (rnd(seed) % 1_000_000) as f32 / 1_000_000.0
    }

    #[test]
    fn poses_come_back_within_three_centimetres_and_two_degrees() {
        let mut seed = 0xDEAD_BEEF_1234u64;
        for round in 0..200 {
            let n = (rnd(&mut seed) % 300) as usize;
            let spread = [30.0f32, 400.0, 3.0e7][round % 3];
            let names: Vec<String> = (0..n).map(|i| format!("Joueur{i}")).collect();
            let poses: Vec<(u64, Pose)> = (0..n)
                .map(|i| {
                    let id = if round % 2 == 0 {
                        i as u64 * 3 + 1
                    } else {
                        (5u64 << 40) + rnd(&mut seed) % 100_000
                    };
                    let p = Pose {
                        pos: [
                            (unit(&mut seed) - 0.5) * spread,
                            unit(&mut seed) * 255.0,
                            (unit(&mut seed) - 0.5) * spread,
                        ],
                        yaw: (unit(&mut seed) - 0.5) * 20.0,
                        pitch: (unit(&mut seed) - 0.5) * 3.1,
                        crouch: unit(&mut seed),
                        speed: unit(&mut seed) * 30.0,
                        ping: Some((rnd(&mut seed) % 900) as u16),
                    };
                    (id, p)
                })
                .collect();
            let mut entries: Vec<(u64, Option<&str>, Pose)> = poses
                .iter()
                .enumerate()
                .map(|(i, (id, p))| (*id, (i % 3 == 0).then_some(names[i].as_str()), *p))
                .collect();
            let packed = pack_poses(&mut entries);
            let back = unpack_poses(&packed).expect("paquet lisible");
            assert_eq!(back.len(), entries.len());
            for ((id, name, p), (bid, bname, b)) in entries.iter().zip(&back) {
                assert_eq!((id, name.map(str::to_string)), (bid, bname.clone()));
                // (Au loin, un nombre sur 32 bits n'a plus cette finesse lui-même.)
                let fine = if spread > 1.0e6 { 4.0 } else { 0.02 };
                for a in 0..3 {
                    assert!((p.pos[a] - b.pos[a]).abs() <= fine, "{p:?} {b:?}");
                }
                let turn = std::f32::consts::TAU;
                let d = (p.yaw - b.yaw).rem_euclid(turn);
                assert!(d.min(turn - d) < 0.03, "{} {}", p.yaw, b.yaw);
                let half = std::f32::consts::FRAC_PI_2;
                assert!((p.pitch.clamp(-half, half) - b.pitch).abs() < 0.02);
                assert!((p.crouch - b.crouch).abs() < 0.17);
                assert!((p.speed - b.speed).abs() <= 1.01);
                if let Some(ms) = b.ping {
                    assert!(p.ping.is_some_and(|q| q.abs_diff(ms) < 4), "{p:?} {b:?}");
                }
            }
        }
    }

    /// Ce qu'un joueur reçoit en un cinquantième de seconde au milieu d'une foule : une
    /// dizaine de positions de voisins déjà connus.
    #[test]
    fn a_crowd_tick_costs_about_ten_bytes_per_player() {
        let mut seed = 99u64;
        let mut entries: Vec<(u64, Option<&str>, Pose)> = (0..10)
            .map(|i| {
                let p = Pose {
                    pos: [
                        5000.0 + unit(&mut seed) * 60.0,
                        64.0 + unit(&mut seed) * 8.0,
                        -3000.0 + unit(&mut seed) * 60.0,
                    ],
                    yaw: unit(&mut seed) * 6.0,
                    pitch: unit(&mut seed) - 0.5,
                    crouch: 0.0,
                    speed: 4.0,
                    ping: Some(40),
                };
                ((3u64 << 40) + 17 + i * 5, None, p)
            })
            .collect();
        let packed = pack_poses(&mut entries);
        eprintln!("10 positions : {} octets", packed.len());
        // (Plus un octet de ping pour une position sur huit.)
        assert!(packed.len() <= 10 * 11 + 16 + 3, "{}", packed.len());
        // La première fois, avec le pseudo.
        let mut one = vec![(42u64, Some("Viendax"), entries[0].2)];
        let first = pack_poses(&mut one);
        eprintln!("1 position avec pseudo : {} octets", first.len());
        assert!(first.len() <= 32);
    }

    /// (v165) Le ping de chacun part avec une de ses positions sur huit, à tour de rôle.
    #[test]
    fn every_player_ping_goes_out_now_and_then() {
        let p = Pose {
            pos: [1.0, 64.0, 3.0],
            yaw: 0.0,
            pitch: 0.0,
            crouch: 0.0,
            speed: 0.0,
            ping: Some(123),
        };
        let mut heard = [false; 20];
        for _ in 0..8 {
            let mut e: Vec<(u64, Option<&str>, Pose)> = (0..20u64).map(|i| (i, None, p)).collect();
            for (id, _, b) in unpack_poses(&pack_poses(&mut e)).unwrap() {
                if let Some(ms) = b.ping {
                    assert_eq!(ms, 120);
                    heard[id as usize] = true;
                }
            }
        }
        assert!(heard.iter().all(|h| *h), "{heard:?}");
    }

    #[test]
    fn absurd_positions_are_left_out_and_malformed_packets_refused() {
        let good = Pose {
            pos: [1.0, 2.0, 3.0],
            yaw: 0.0,
            pitch: 0.0,
            crouch: 0.0,
            speed: 0.0,
            ping: None,
        };
        let mut e = vec![
            (1u64, None, good),
            (
                2,
                None,
                Pose {
                    pos: [f32::NAN, 0.0, 0.0],
                    ..good
                },
            ),
            (
                3,
                None,
                Pose {
                    pos: [0.0, f32::INFINITY, 0.0],
                    ..good
                },
            ),
            (
                4,
                None,
                Pose {
                    pos: [5.0e7, 0.0, 0.0],
                    ..good
                },
            ),
            (
                5,
                None,
                Pose {
                    yaw: f32::NAN,
                    ..good
                },
            ),
        ];
        let packed = pack_poses(&mut e);
        let back = unpack_poses(&packed).unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].0, 1);
        // Tronqué n'importe où, ou n'importe quels octets : jamais de panique, et jamais
        // de position absurde.
        let mut full = vec![(7u64, Some("Abc"), good), (9, None, good)];
        let packed = pack_poses(&mut full);
        for cut in 0..packed.len() {
            let _ = unpack_poses(&packed[..cut]);
        }
        let mut seed = 3u64;
        for _ in 0..50_000 {
            let n = (rnd(&mut seed) % 40) as usize;
            let junk: Vec<u8> = (0..n).map(|_| rnd(&mut seed) as u8).collect();
            if let Some(list) = unpack_poses(&junk) {
                assert!(list.iter().all(|e| e.2.sane()));
            }
        }
    }
}
