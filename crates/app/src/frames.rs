//! Cadres accrochés aux murs des cartes importées (`item_frame` de Minecraft) : le bloc
//! qu'ils tiennent (béton, laine, tapis…), à moitié de sa taille, ou un cadre vide.
//! Lignes `x,y,z,f<côté>,bloc:<clé>` du fichier des têtes (voir `mcimport::read_frames`).

use glam::{Mat4, Vec3};
use minelib_render::{EntityVertex, RayBoxKind};

#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    pub pos: [i32; 3],
    /// Côté vers lequel il regarde (0 bas, 1 haut, 2 nord, 3 sud, 4 ouest, 5 est).
    pub facing: u8,
    /// Bloc tenu (`None` : cadre vide).
    pub block: Option<minelib_world::BlockId>,
    /// Sa ligne dans le fichier (pour l'enlever quand il est cassé).
    pub line: String,
}

pub fn parse(line: &str) -> Option<Frame> {
    let v: Vec<&str> = line.trim().split(',').collect();
    let [x, y, z, f, b] = v.as_slice() else {
        return None;
    };
    let facing: u8 = f.strip_prefix('f')?.parse().ok().filter(|f| *f < 6)?;
    let key = b.strip_prefix("bloc:")?;
    let block = if key.is_empty() {
        None
    } else {
        Some(minelib_world::registry::registry().by_key(key)?)
    };
    Some(Frame {
        pos: [x.parse().ok()?, y.parse().ok()?, z.parse().ok()?],
        facing,
        block,
        line: line.trim().to_string(),
    })
}

fn dir(f: u8) -> Vec3 {
    match f {
        0 => -Vec3::Y,
        1 => Vec3::Y,
        2 => -Vec3::Z,
        3 => Vec3::Z,
        4 => -Vec3::X,
        _ => Vec3::X,
    }
}

/// Boîte (coins min, max, monde) de ce que montre le cadre.
pub fn boxes(fr: &Frame) -> (Vec3, Vec3) {
    let n = dir(fr.facing);
    let cell = Vec3::new(fr.pos[0] as f32, fr.pos[1] as f32, fr.pos[2] as f32) + 0.5;
    let wall = cell - n * 0.5;
    // Largeur dans le plan du mur, épaisseur le long de `n`.
    let (half, depth) = match fr.block {
        None => (0.375, 1.0 / 16.0),
        Some(b) if b.def().key.starts_with("carpet") => (0.25, 1.0 / 32.0),
        Some(_) => (0.25, 0.25),
    };
    let lift = 1.0 / 16.0;
    let center = wall + n * (lift + depth / 2.0);
    let ext = (Vec3::ONE - n.abs()) * half + n.abs() * depth / 2.0;
    (center - ext, center + ext)
}

pub struct Frames {
    list: Vec<Frame>,
    built: Option<([f32; 3], Vec<EntityVertex>)>,
    /// Version des triangles (change quand ils sont refaits).
    gen: u64,
}

impl Frames {
    pub fn load(dir: &std::path::Path) -> Self {
        let list = std::fs::read_to_string(dir.join(crate::mcimport::HEADS_FILE))
            .unwrap_or_default()
            .lines()
            .filter_map(parse)
            .collect();
        Self {
            list,
            built: None,
            gen: 0,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// Premier cadre touché par le rayon (`eye`, `dir` unitaire) avant `max` blocs.
    /// `seen` : premier bloc visible sur le rayon (sa case, sa distance) ; il cache les
    /// cadres derrière lui, sauf celui accroché dans sa propre case (panneau, barreau,
    /// tige… posés au même endroit que le cadre) : un cadre qu'on voit se casse toujours.
    pub fn hit(
        &self,
        eye: Vec3,
        dir: Vec3,
        max: f32,
        seen: Option<([i32; 3], f32)>,
    ) -> Option<usize> {
        let inv = dir.map(|d| if d.abs() < 1e-6 { 1e6 } else { 1.0 / d });
        let mut best: Option<(usize, f32)> = None;
        for (i, fr) in self.list.iter().enumerate() {
            let (mn, mx) = boxes(fr);
            // Un rien plus large que ce qu'on voit : le bord se vise aussi.
            let (mn, mx) = (mn - 0.02, mx + 0.02);
            let t0 = (mn - eye) * inv;
            let t1 = (mx - eye) * inv;
            let near = t0.min(t1).max_element();
            let far = t0.max(t1).min_element();
            let limit = match seen {
                Some((cell, _)) if cell == fr.pos => max,
                Some((_, d)) => d.min(max),
                None => max,
            };
            if far >= near.max(0.0) && near < limit && best.is_none_or(|b| near < b.1) {
                best = Some((i, near));
            }
        }
        best.map(|b| b.0)
    }

    /// Bloc que montre le cadre `i` (`None` : cadre vide).
    pub fn block(&self, i: usize) -> Option<minelib_world::BlockId> {
        self.list.get(i)?.block
    }

    /// Boîte du cadre `i` (pour montrer qu'il est visé).
    pub fn bounds(&self, i: usize) -> Option<([f32; 3], [f32; 3])> {
        let (mn, mx) = boxes(self.list.get(i)?);
        Some((mn.to_array(), mx.to_array()))
    }

    /// Casse le cadre `i` : enlevé de l'affichage et du fichier du monde `dir`.
    pub fn remove(&mut self, i: usize, dir: &std::path::Path) {
        let fr = self.list.remove(i);
        self.built = None;
        let path = dir.join(crate::mcimport::HEADS_FILE);
        if let Ok(text) = std::fs::read_to_string(&path) {
            let mut gone = false;
            let kept: Vec<&str> = text
                .lines()
                .filter(|l| {
                    let same = !gone && l.trim() == fr.line;
                    gone |= same;
                    !same
                })
                .collect();
            let mut t = kept.join("\n");
            if !t.is_empty() {
                t.push('\n');
            }
            let _ = std::fs::write(&path, t);
        }
    }

    /// Refait les triangles si la lumière a changé (ou qu'un cadre a été cassé).
    pub fn refresh(&mut self, light: [f32; 3]) {
        let fresh = self
            .built
            .as_ref()
            .is_some_and(|(l, _)| l.iter().zip(light).all(|(a, b)| (a - b).abs() < 0.03));
        if !fresh {
            let mut out = Vec::new();
            for fr in &self.list {
                let (mn, mx) = boxes(fr);
                let kind = match fr.block {
                    Some(b) => RayBoxKind::Block {
                        layer: b.def().tex[1] as u32,
                        uv: [0.0, 0.0, 1.0, 1.0],
                        glow: b.def().light.is_some(),
                    },
                    None => RayBoxKind::Flat([150, 105, 60]),
                };
                crate::player_model::emit_item(
                    &mut out,
                    Mat4::IDENTITY,
                    mn.to_array(),
                    mx.to_array(),
                    kind,
                    light,
                );
            }
            self.built = Some((light, out));
            self.gen += 1;
        }
    }

    /// Triangles de tous les cadres et leur version.
    pub fn current(&self) -> (&[EntityVertex], u64) {
        (
            self.built.as_ref().map_or(&[][..], |b| b.1.as_slice()),
            self.gen,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_hang_on_their_wall() {
        let f = parse("10,64,-3,f5,bloc:concrete_red").unwrap();
        assert_eq!(f.facing, 5);
        let (mn, mx) = boxes(&f);
        // Contre le mur ouest de la case (x = 10), dépasse vers l'est.
        assert!(mn.x > 10.0 && mx.x < 10.5, "{mn} {mx}");
        assert!((mx.y - mn.y - 0.5).abs() < 1e-4);
        assert!(parse("1,2,3,4,abcdef").is_none());
        assert!(parse("1,2,3,f2,bloc:").unwrap().block.is_none());
    }
}
