//! Cadres accrochés aux murs des cartes importées (`item_frame` de Minecraft) : le cadre
//! lui-même (bordure de bois et fond, s'il est visible) et ce qu'il tient : un bloc plein
//! (à moitié de sa taille), ou une image à plat (vitre, fleur, objet du pack de ressources).
//! Une tête qu'il tient est dessinée avec les têtes (`heads`). Éclairés comme les blocs
//! autour d'eux (lumière mesurée par le maillage, voir `World::set_probes`).
//!
//! Lignes `x,y,z,f<côté>,<contenu>` du fichier des têtes (voir `mcimport::frame_line`) :
//! `f` cadre visible, `i` cadre invisible (seul son contenu se voit) ; côté vers lequel il
//! regarde ; contenu `bloc:<clé>` (vide : rien) ou `objet:<nom>` (image du pack).

use std::collections::HashMap;

use glam::{Mat4, Vec3};
use minelib_render::{EntityVertex, RayBoxKind};
use minelib_world::{BlockId, ChunkPos};

/// Ce que montre un cadre.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Content {
    Empty,
    Block(BlockId),
    /// Image d'un objet (couche de texture, voir `registry::FRAME_ITEMS`).
    Item(u16),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    pub pos: [i32; 3],
    /// Côté vers lequel il regarde (0 bas, 1 haut, 2 nord, 3 sud, 4 ouest, 5 est).
    pub facing: u8,
    /// Le cadre se voit (sinon, seulement ce qu'il tient).
    pub visible: bool,
    pub content: Content,
    /// Sa ligne dans le fichier (pour l'enlever quand il est cassé).
    pub line: String,
}

pub fn parse(line: &str) -> Option<Frame> {
    let v: Vec<&str> = line.trim().split(',').collect();
    let [x, y, z, f, c] = v.as_slice() else {
        return None;
    };
    let (visible, f) = match (f.strip_prefix('f'), f.strip_prefix('i')) {
        (Some(f), _) => (true, f),
        (_, Some(f)) => (false, f),
        _ => return None,
    };
    let facing: u8 = f.parse().ok().filter(|f| *f < 6)?;
    let reg = minelib_world::registry::registry();
    let content = if let Some(key) = c.strip_prefix("bloc:") {
        if key.is_empty() {
            Content::Empty
        } else {
            Content::Block(reg.by_key(key)?)
        }
    } else {
        Content::Item(reg.item_texture(c.strip_prefix("objet:")?)?)
    };
    Some(Frame {
        pos: [x.parse().ok()?, y.parse().ok()?, z.parse().ok()?],
        facing,
        visible,
        content,
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

/// Repère du cadre : centre du mur derrière lui, vers la pièce (`n`), vers la droite et
/// vers le bas de qui le regarde (`right`, `down`).
fn basis(fr: &Frame) -> (Vec3, Vec3, Vec3, Vec3) {
    let n = dir(fr.facing);
    let cell = Vec3::new(fr.pos[0] as f32, fr.pos[1] as f32, fr.pos[2] as f32) + 0.5;
    let wall = cell - n * 0.5;
    let (right, down) = if n.y.abs() > 0.5 {
        (Vec3::X, Vec3::Z * n.y.signum())
    } else {
        ((-n).cross(Vec3::Y), -Vec3::Y)
    };
    (wall, n, right, down)
}

/// Pavé (coins min, max, monde) d'une boîte du repère du cadre : `u`, `v` (pixels, dans le
/// plan du mur depuis son centre) et `d` (pixels, depuis le mur).
fn local_box(fr: &Frame, u: [f32; 2], v: [f32; 2], d: [f32; 2]) -> (Vec3, Vec3) {
    let (wall, n, right, down) = basis(fr);
    let p = |a: f32, b: f32, c: f32| wall + (right * a + down * b + n * c) / 16.0;
    let (a, b) = (p(u[0], v[0], d[0]), p(u[1], v[1], d[1]));
    (a.min(b), a.max(b))
}

/// Bloc montré en volume (plein, tapis, verre) ; les autres à plat, comme une image.
fn solid(b: BlockId) -> bool {
    let d = b.def();
    d.full || d.key.starts_with("carpet") || d.key.starts_with("glass")
}

/// Boîte du contenu (coins min, max, monde), s'il y en a un.
fn content_box(fr: &Frame) -> Option<(Vec3, Vec3)> {
    // Posé sur le fond du cadre (1 pixel), ou sur le mur (cadre invisible).
    let lift = if fr.visible { 1.0 } else { 0.0 };
    match fr.content {
        Content::Empty => None,
        Content::Block(b) if b.def().key.starts_with("carpet") => {
            Some(local_box(fr, [-4.0, 4.0], [-4.0, 4.0], [lift, lift + 0.5]))
        }
        Content::Block(b) if solid(b) => {
            Some(local_box(fr, [-4.0, 4.0], [-4.0, 4.0], [lift, lift + 8.0]))
        }
        Content::Block(_) | Content::Item(_) => {
            Some(local_box(fr, [-4.0, 4.0], [-4.0, 4.0], [lift, lift + 0.25]))
        }
    }
}

/// Boîte (coins min, max, monde) de ce qui se voit du cadre : lui-même s'il est visible et
/// ce qu'il tient.
pub fn boxes(fr: &Frame) -> (Vec3, Vec3) {
    let frame = fr
        .visible
        .then(|| local_box(fr, [-6.0, 6.0], [-6.0, 6.0], [0.0, 1.0]));
    match (frame, content_box(fr)) {
        (Some(a), Some(b)) => (a.0.min(b.0), a.1.max(b.1)),
        (Some(a), None) | (None, Some(a)) => a,
        // (Un cadre invisible et vide n'est pas lu.)
        (None, None) => local_box(fr, [-6.0, 6.0], [-6.0, 6.0], [0.0, 1.0]),
    }
}

pub struct Frames {
    list: Vec<Frame>,
    /// Lumière reçue par chaque cadre (`[r, g, b, ciel]`, indices de `list`), mesurée par
    /// le maillage de son chunk.
    light: Vec<Option<[u8; 4]>>,
    built: Option<(f32, Vec<EntityVertex>)>,
    /// Version des triangles (change quand ils sont refaits).
    gen: u64,
}

impl Frames {
    /// Cadres du monde `dir`. (v309) Le fichier a leurs places absolues ; gardés comptés depuis
    /// l'origine de la partie `origin` (leur ligne reste celle du fichier, pour `remove`).
    pub fn load(dir: &std::path::Path, origin: [i32; 3]) -> Self {
        let list: Vec<Frame> = std::fs::read_to_string(dir.join(crate::mcimport::HEADS_FILE))
            .unwrap_or_default()
            .lines()
            .filter_map(parse)
            .map(|mut f| {
                // (v309) La place la plus proche de l'origine, sur la carte bouclée.
                let wb = minelib_world::lap::wrap_block;
                f.pos = [
                    wb(f.pos[0] as i64 - origin[0] as i64),
                    f.pos[1],
                    wb(f.pos[2] as i64 - origin[2] as i64),
                ];
                f
            })
            .collect();
        Self {
            light: vec![None; list.len()],
            list,
            built: None,
            gen: 0,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// Points où mesurer la lumière (la case de chaque cadre), par chunk, dans l'ordre de
    /// `set_light`.
    pub fn probes(&self) -> Vec<(ChunkPos, Vec<[i32; 3]>)> {
        let mut by: HashMap<ChunkPos, Vec<[i32; 3]>> = HashMap::new();
        for fr in &self.list {
            by.entry(ChunkPos::containing(fr.pos[0], fr.pos[2]))
                .or_default()
                .push(fr.pos);
        }
        by.into_iter().collect()
    }

    /// Lumière mesurée aux points de `probes` du chunk `pos`.
    pub fn set_light(&mut self, pos: ChunkPos, lights: &[[u8; 4]]) {
        let mut k = 0;
        let mut changed = false;
        for (i, fr) in self.list.iter().enumerate() {
            if ChunkPos::containing(fr.pos[0], fr.pos[2]) != pos {
                continue;
            }
            let Some(&l) = lights.get(k) else { break };
            k += 1;
            if self.light[i] != Some(l) {
                self.light[i] = Some(l);
                changed = true;
            }
        }
        if changed {
            self.built = None;
        }
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

    /// Bloc que montre le cadre `i` (`None` : cadre vide, ou un objet).
    pub fn block(&self, i: usize) -> Option<BlockId> {
        match self.list.get(i)?.content {
            Content::Block(b) => Some(b),
            _ => None,
        }
    }

    /// Case du cadre `i`.
    pub fn pos(&self, i: usize) -> Option<[i32; 3]> {
        Some(self.list.get(i)?.pos)
    }

    /// Boîte du cadre `i` (pour montrer qu'il est visé).
    pub fn bounds(&self, i: usize) -> Option<([f32; 3], [f32; 3])> {
        let (mn, mx) = boxes(self.list.get(i)?);
        Some((mn.to_array(), mx.to_array()))
    }

    /// Casse le cadre `i` : enlevé de l'affichage et du fichier du monde `dir`.
    pub fn remove(&mut self, i: usize, dir: &std::path::Path) {
        let fr = self.list.remove(i);
        self.light.remove(i);
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

    /// Refait les triangles si la lumière du jour a changé (`day` : 1 plein jour, 0 nuit),
    /// ou celle d'un cadre, ou qu'un cadre a été cassé.
    pub fn refresh(&mut self, day: f32) {
        let fresh = self
            .built
            .as_ref()
            .is_some_and(|(d, _)| (d - day).abs() < 0.02);
        if fresh {
            return;
        }
        let mut out = Vec::new();
        let birch = minelib_world::registry::registry()
            .by_key("planks_birch")
            .map_or(0, |b| b.def().tex[1] as u32);
        for (fr, l) in self.list.iter().zip(&self.light) {
            // Pas encore mesurée : comme au grand air.
            let light = crate::heads::lit(l.unwrap_or([0, 0, 0, 15]), day);
            if fr.visible {
                emit_frame(&mut out, fr, birch, light);
            }
            let Some((mn, mx)) = content_box(fr) else {
                continue;
            };
            match fr.content {
                Content::Block(b) if solid(b) || b.def().key.starts_with("carpet") => {
                    let kind = RayBoxKind::Block {
                        layer: b.def().tex[1] as u32,
                        uv: [0.0, 0.0, 1.0, 1.0],
                        glow: b.def().light.is_some(),
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
                Content::Block(b) => emit_sprite(&mut out, fr, b.def().tex[1], light),
                Content::Item(layer) => emit_sprite(&mut out, fr, layer, light),
                Content::Empty => {}
            }
        }
        self.built = Some((day, out));
        self.gen += 1;
    }

    /// Triangles de tous les cadres et leur version.
    pub fn current(&self) -> (&[EntityVertex], u64) {
        (
            self.built.as_ref().map_or(&[][..], |b| b.1.as_slice()),
            self.gen,
        )
    }
}

/// Le cadre : un fond de cuir (10 × 10 pixels) dans une bordure de bois de bouleau d'un
/// pixel (12 × 12), comme celui de Minecraft.
fn emit_frame(out: &mut Vec<EntityVertex>, fr: &Frame, birch: u32, light: [f32; 3]) {
    let (mn, mx) = local_box(fr, [-5.0, 5.0], [-5.0, 5.0], [0.0, 0.5]);
    crate::player_model::emit_item(
        out,
        Mat4::IDENTITY,
        mn.to_array(),
        mx.to_array(),
        RayBoxKind::Flat([110, 70, 42]),
        light,
    );
    for (u, v) in [
        ([-6.0, 6.0], [-6.0, -5.0]),
        ([-6.0, 6.0], [5.0, 6.0]),
        ([-6.0, -5.0], [-5.0, 5.0]),
        ([5.0, 6.0], [-5.0, 5.0]),
    ] {
        let (mn, mx) = local_box(fr, u, v, [0.0, 1.0]);
        // La partie de la texture qui correspond à la baguette (pas toute l'image écrasée).
        let uv = [
            (u[0] + 8.0) / 16.0,
            (v[0] + 8.0) / 16.0,
            (u[1] + 8.0) / 16.0,
            (v[1] + 8.0) / 16.0,
        ];
        crate::player_model::emit_item(
            out,
            Mat4::IDENTITY,
            mn.to_array(),
            mx.to_array(),
            RayBoxKind::Block {
                layer: birch,
                uv,
                glow: false,
            },
            light,
        );
    }
}

/// Image à plat (8 × 8 pixels) devant le fond du cadre, vue des deux côtés ; ses pixels
/// transparents sont jetés.
fn emit_sprite(out: &mut Vec<EntityVertex>, fr: &Frame, layer: u16, light: [f32; 3]) {
    let (wall, n, right, down) = basis(fr);
    let lift = if fr.visible { 1.1 } else { 0.1 };
    let c = wall + n * lift / 16.0;
    let h = 4.0 / 16.0;
    let corner = |a: f32, b: f32| (c + right * (a * h) + down * (b * h)).to_array();
    let lit = EntityVertex::light(light, true);
    let shade = (0.78 + 0.22 * n.y + 0.06 * n.x).clamp(0.45, 1.0);
    let quad = [
        (corner(-1.0, -1.0), [0.0, 0.0]),
        (corner(1.0, -1.0), [1.0, 0.0]),
        (corner(1.0, 1.0), [1.0, 1.0]),
        (corner(-1.0, 1.0), [0.0, 1.0]),
    ];
    // Les deux sens de parcours : vue de face quel que soit le tri des faces.
    for order in [[0usize, 1, 2, 2, 3, 0], [0, 3, 2, 2, 1, 0]] {
        for i in order {
            out.push(EntityVertex {
                pos: quad[i].0,
                color: [shade; 3],
                uv: quad[i].1,
                layer: layer as i32,
                light: lit,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_hang_on_their_wall() {
        let f = parse("10,64,-3,f5,bloc:concrete_red").unwrap();
        assert_eq!(f.facing, 5);
        assert!(f.visible);
        let (mn, mx) = content_box(&f).unwrap();
        // Contre le mur ouest de la case (x = 10), dépasse vers l'est : un demi-bloc posé
        // sur le fond du cadre.
        assert!(mn.x > 10.0 && mx.x < 10.6, "{mn} {mx}");
        assert!((mx.y - mn.y - 0.5).abs() < 1e-4);
        // Le cadre visible : 12 pixels de côté, un pixel d'épaisseur.
        let (a, b) = boxes(&f);
        assert!(
            (b.z - a.z - 0.75).abs() < 1e-4 && (a.x - 10.0).abs() < 1e-4,
            "{a} {b}"
        );
        assert!(parse("1,2,3,4,abcdef").is_none());
        assert!(parse("1,2,3,f2,bloc:").unwrap().content == Content::Empty);
        // Cadre invisible : seul son contenu compte.
        let g = parse("1,2,3,i2,objet:stick").unwrap();
        assert!(!g.visible);
        assert!(matches!(g.content, Content::Item(_)));
        assert!(parse("1,2,3,f2,objet:pas_un_objet").is_none());
    }

    #[test]
    fn visible_frames_are_drawn_with_their_border_and_lit_like_blocks() {
        let mut f = Frames {
            list: vec![
                parse("0,64,0,f3,bloc:").unwrap(),
                parse("1,64,0,i3,objet:stick").unwrap(),
            ],
            light: vec![None; 2],
            built: None,
            gen: 0,
        };
        f.refresh(1.0);
        let (v, gen) = f.current();
        let v = v.to_vec();
        // Fond et quatre baguettes (6 faces × 6 sommets chacun), image à deux sens.
        assert_eq!(v.len(), 5 * 36 + 12);
        // La nuit, sous un toit : plus sombre ; la lumière mesurée refait les triangles.
        f.set_light(ChunkPos { x: 0, z: 0 }, &[[0, 0, 0, 4], [0, 0, 0, 4]]);
        f.refresh(1.0);
        let (w, gen2) = f.current();
        assert!(gen2 > gen);
        assert!(
            w[0].light[0] < v[0].light[0],
            "{:?} {:?}",
            w[0].light,
            v[0].light
        );
        // Visé, puis cassé.
        let i = f
            .hit(Vec3::new(0.5, 64.5, 3.0), -Vec3::Z, 5.0, None)
            .expect("cadre visé");
        assert_eq!(i, 0);
        assert!(f.block(1).is_none());
    }
}
