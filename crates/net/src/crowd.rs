//! (v227) Paquets compacts de la foule d'essai partagée en construction en ligne.
//!
//! Le jeu d'un OP mène la foule (`/npc stresstest on`, jusqu'à 4 000 PNJ autour de lui) ;
//! les joueurs proches la reçoivent par le relais du monde et font avancer chaque PNJ
//! comme lui entre deux nouvelles : tout droit, à sa vitesse, sur le sol de leur propre
//! monde (le même). Il suffit donc de dire ce qui change (un PNJ qui tourne, s'arrête,
//! repart, reçoit un coup, réapparaît ailleurs) et, pour ceux qui arrivent, chaque PNJ de
//! temps en temps : une quinzaine d'octets par PNJ, la position au 1/32 de bloc par rapport
//! au centre de la foule (le joueur qui la mène), les angles sur un octet.

/// Un PNJ de la foule, tel que les autres joueurs le font avancer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NpcState {
    /// Sa place dans la foule (0 … 3 999).
    pub id: u16,
    /// Numéro de son pseudo (`Pnj` suivi de ce numéro).
    pub num: u32,
    pub pos: [f32; 3],
    pub yaw: f32,
    /// Vitesse au sol (blocs/s, au 1/20).
    pub speed: f32,
    /// Bond d'un coup reçu (blocs/s, au 1/10), `None` au sol.
    pub push: Option<[f32; 3]>,
    /// Il vient d'être frappé (rouge, cri).
    pub hurt: bool,
    /// C'est le personnage caché.
    pub hidden: bool,
}

/// Ce que dit un paquet de la foule.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Packet {
    /// Centre de la foule (le joueur qui la mène) : les positions s'écrivent par rapport à
    /// lui.
    pub center: [f32; 3],
    /// Nombre de PNJ de la foule (ceux de numéro plus grand n'y sont plus).
    pub count: u16,
    pub npcs: Vec<NpcState>,
    /// Le personnage caché vient d'être trouvé par ce joueur, là (pour prévenir ceux qui
    /// sont près).
    pub found: Option<(String, [f32; 3])>,
    /// La foule s'arrête (`/npc stresstest off`, ou celui qui la menait part).
    pub stop: bool,
}

const HURT: u8 = 1;
const HIDDEN: u8 = 2;
const AIR: u8 = 4;

/// Au 1/32 de bloc par rapport au centre (± 1 024 blocs ; au-delà, ramené au bord).
fn rel(v: f32, c: f32) -> i16 {
    ((v - c) * 32.0).round().clamp(i16::MIN as f32, i16::MAX as f32) as i16
}

fn angle(a: f32) -> u8 {
    (a.rem_euclid(std::f32::consts::TAU) / std::f32::consts::TAU * 256.0).round() as u32 as u8
}

/// Le paquet en octets.
pub fn pack(p: &Packet) -> Vec<u8> {
    let mut out = Vec::with_capacity(32 + p.npcs.len() * 16);
    for c in p.center {
        out.extend_from_slice(&c.to_le_bytes());
    }
    out.extend_from_slice(&p.count.to_le_bytes());
    out.push(p.stop as u8);
    match &p.found {
        Some((who, at)) => {
            let name = &who.as_bytes()[..who.len().min(64)];
            out.push(name.len() as u8 + 1);
            out.extend_from_slice(name);
            for a in at {
                out.extend_from_slice(&a.to_le_bytes());
            }
        }
        None => out.push(0),
    }
    out.extend_from_slice(&(p.npcs.len() as u16).to_le_bytes());
    for n in &p.npcs {
        out.extend_from_slice(&n.id.to_le_bytes());
        out.extend_from_slice(&n.num.to_le_bytes());
        out.extend_from_slice(&rel(n.pos[0], p.center[0]).to_le_bytes());
        out.extend_from_slice(&rel(n.pos[1], p.center[1]).to_le_bytes());
        out.extend_from_slice(&rel(n.pos[2], p.center[2]).to_le_bytes());
        out.push(angle(n.yaw));
        out.push((n.speed * 20.0).round().clamp(0.0, 255.0) as u8);
        let flags = (n.hurt as u8 * HURT) | (n.hidden as u8 * HIDDEN) | (n.push.is_some() as u8 * AIR);
        out.push(flags);
        if let Some(v) = n.push {
            for c in v {
                out.push(((c * 10.0).round().clamp(-127.0, 127.0) as i8) as u8);
            }
        }
    }
    out
}

/// Lecteur d'octets (`None` dès qu'il en manque).
struct Read<'a>(&'a [u8]);

impl Read<'_> {
    fn take<const N: usize>(&mut self) -> Option<[u8; N]> {
        let (a, b) = self.0.split_first_chunk::<N>()?;
        self.0 = b;
        Some(*a)
    }
    fn u8(&mut self) -> Option<u8> {
        self.take::<1>().map(|b| b[0])
    }
    fn u16(&mut self) -> Option<u16> {
        self.take().map(u16::from_le_bytes)
    }
    fn i16(&mut self) -> Option<i16> {
        self.take().map(i16::from_le_bytes)
    }
    fn u32(&mut self) -> Option<u32> {
        self.take().map(u32::from_le_bytes)
    }
    fn f32(&mut self) -> Option<f32> {
        self.take().map(f32::from_le_bytes).filter(|v| v.is_finite())
    }
}

/// Plus de PNJ dans un paquet que n'en a une foule : paquet absurde.
pub const MAX_NPCS: usize = 4096;

/// Le paquet que disent ces octets (`None` : tronqué ou absurde).
pub fn unpack(data: &[u8]) -> Option<Packet> {
    let mut r = Read(data);
    let center = [r.f32()?, r.f32()?, r.f32()?];
    if center.iter().any(|c| c.abs() > 3.0e7) {
        return None;
    }
    let count = r.u16()?;
    let stop = r.u8()? != 0;
    let found = match r.u8()? {
        0 => None,
        n => {
            let len = n as usize - 1;
            if len > 64 || r.0.len() < len {
                return None;
            }
            let (name, rest) = r.0.split_at(len);
            r.0 = rest;
            let who = String::from_utf8(name.to_vec()).ok()?;
            Some((who, [r.f32()?, r.f32()?, r.f32()?]))
        }
    };
    let n = r.u16()? as usize;
    if n > MAX_NPCS || count as usize > MAX_NPCS {
        return None;
    }
    let mut npcs = Vec::with_capacity(n);
    for _ in 0..n {
        let id = r.u16()?;
        let num = r.u32()?;
        let pos = [
            center[0] + r.i16()? as f32 / 32.0,
            center[1] + r.i16()? as f32 / 32.0,
            center[2] + r.i16()? as f32 / 32.0,
        ];
        let yaw = r.u8()? as f32 / 256.0 * std::f32::consts::TAU;
        let speed = r.u8()? as f32 / 20.0;
        let flags = r.u8()?;
        let push = if flags & AIR != 0 {
            let v = [r.u8()?, r.u8()?, r.u8()?];
            Some(v.map(|b| b as i8 as f32 / 10.0))
        } else {
            None
        };
        if id as usize >= MAX_NPCS {
            return None;
        }
        npcs.push(NpcState {
            id,
            num,
            pos,
            yaw,
            speed,
            push,
            hurt: flags & HURT != 0,
            hidden: flags & HIDDEN != 0,
        });
    }
    (r.0.is_empty()).then_some(Packet {
        center,
        count,
        npcs,
        found,
        stop,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_crowd_packet_comes_back_as_it_left() {
        let p = Packet {
            center: [1000.5, 70.0, -2000.25],
            count: 4000,
            npcs: vec![
                NpcState {
                    id: 0,
                    num: 1,
                    pos: [1010.0, 71.0, -1990.0],
                    yaw: 1.0,
                    speed: 4.3,
                    push: None,
                    hurt: false,
                    hidden: false,
                },
                NpcState {
                    id: 3999,
                    num: 123_456,
                    pos: [850.03125, 64.5, -2150.0],
                    yaw: 6.0,
                    speed: 0.0,
                    push: Some([-5.0, 3.2, 0.5]),
                    hurt: true,
                    hidden: true,
                },
            ],
            found: Some(("Viendax".into(), [1.5, 2.0, -3.0])),
            stop: false,
        };
        let data = pack(&p);
        // Une quinzaine d'octets par PNJ.
        assert!(data.len() < 40 + 2 * 18, "{} octets", data.len());
        let q = unpack(&data).unwrap();
        assert_eq!(q.center, p.center);
        assert_eq!((q.count, q.stop), (4000, false));
        assert_eq!(q.found, p.found);
        for (a, b) in p.npcs.iter().zip(&q.npcs) {
            assert_eq!((a.id, a.num, a.hurt, a.hidden), (b.id, b.num, b.hurt, b.hidden));
            for k in 0..3 {
                assert!((a.pos[k] - b.pos[k]).abs() <= 1.0 / 64.0);
            }
            let d = (a.yaw - b.yaw).rem_euclid(std::f32::consts::TAU);
            assert!(d.min(std::f32::consts::TAU - d) < 0.013);
            assert!((a.speed - b.speed).abs() <= 0.025);
            match (a.push, b.push) {
                (None, None) => {}
                (Some(x), Some(y)) => {
                    for k in 0..3 {
                        assert!((x[k] - y[k]).abs() <= 0.05);
                    }
                }
                _ => panic!("bond perdu"),
            }
        }
        // Tronqué, ou avec des octets de trop : refusé.
        assert!(unpack(&data[..data.len() - 1]).is_none());
        let mut long = data.clone();
        long.push(0);
        assert!(unpack(&long).is_none());
        assert!(unpack(&[]).is_none());
    }
}
