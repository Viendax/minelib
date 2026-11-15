//! Skins des joueurs, au format des jeux de blocs (PNG de 64 × 64, ou l'ancien 64 × 32) :
//! chargement et vérification, pixels de chaque face du modèle, visage pour le bandeau des
//! joueurs, et aperçu en 3D pour l'interface.
//!
//! Un skin vient d'un fichier choisi par le joueur ; il est envoyé au relais, qui le donne
//! aux autres joueurs. Aucun skin n'est fourni avec le jeu (le modèle par défaut est dessiné
//! pour Minelib).

use std::sync::Arc;

use anyhow::{bail, Context, Result};

/// Taille maximale d'un fichier de skin accepté (et envoyé au relais).
pub const MAX_BYTES: usize = 24 * 1024;

/// Skin décodé : 64 × 64 pixels RGBA (un 64 × 32 est complété comme le font les jeux de
/// blocs : bras et jambe gauches copiés des droits).
pub struct Skin {
    px: Vec<[u8; 4]>,
    /// Format récent (64 × 64) : calques du bras et de la jambe gauches, et du corps.
    wide: bool,
}

impl Skin {
    /// Lit et vérifie un PNG de skin.
    pub fn from_png(bytes: &[u8]) -> Result<Arc<Skin>> {
        if bytes.len() > MAX_BYTES {
            bail!(
                "fichier trop gros ({} Ko, 24 Ko au plus)",
                bytes.len() / 1024
            );
        }
        // Taille lue dans l'en-tête avant de décoder : un petit fichier qui annonce une
        // image immense ne fait pas réserver des gigaoctets.
        if bytes.len() < 24 || &bytes[12..16] != b"IHDR" {
            bail!("ce n'est pas une image PNG");
        }
        let hw = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
        let hh = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
        if hw != 64 || (hh != 64 && hh != 32) {
            bail!("un skin fait 64 × 64 (ou 64 × 32) pixels, celui-ci {hw} × {hh}");
        }
        let img = image::load_from_memory_with_format(bytes, image::ImageFormat::Png)
            .context("ce n'est pas une image PNG")?
            .to_rgba8();
        let (w, h) = img.dimensions();
        if w != 64 || (h != 64 && h != 32) {
            bail!("un skin fait 64 × 64 (ou 64 × 32) pixels, celui-ci {w} × {h}");
        }
        let mut px = vec![[0u8; 4]; 64 * 64];
        for y in 0..h {
            for x in 0..64 {
                px[(y * 64 + x) as usize] = img.get_pixel(x, y).0;
            }
        }
        let wide = h == 64;
        if !wide {
            // Ancien format : bras et jambe gauches = droits.
            for (from, to) in [((40, 16), (32, 48)), ((0, 16), (16, 48))] {
                for y in 0..16 {
                    for x in 0..16 {
                        px[((to.1 + y) * 64 + to.0 + x) as usize] =
                            px[((from.1 + y) * 64 + from.0 + x) as usize];
                    }
                }
            }
        }
        Ok(Arc::new(Skin { px, wide }))
    }

    fn at(&self, x: u32, y: u32) -> [u8; 4] {
        self.px[(y.min(63) * 64 + x.min(63)) as usize]
    }

    /// Couleur (sRGB) du pixel (`u`, `v`) de la face `face` (0 devant, 1 derrière, 2 côté
    /// droit, 3 côté gauche, 4 dessus, 5 dessous) d'une partie du modèle (`part` : 0 jambe
    /// gauche, 1 jambe droite, 2 corps, 3 bras gauche, 4 bras droit, 5 tête) de taille
    /// `w` × `h` × `d` pixels. Le calque du dessus (chapeau, veste…) passe devant s'il est
    /// opaque. `None` : pixel transparent (on garde le modèle par défaut).
    pub fn color(&self, part: usize, face: usize, u: u32, v: u32, size: [u32; 3]) -> Option<u32> {
        let (base, over) = match part {
            0 => ((16, 48), (0, 48)),
            1 => ((0, 16), (0, 32)),
            2 => ((16, 16), (16, 32)),
            3 => ((32, 48), (48, 48)),
            4 => ((40, 16), (40, 32)),
            _ => ((0, 0), (32, 0)),
        };
        let [w, h, d] = size;
        let _ = h;
        let local = |o: (u32, u32)| -> (u32, u32) {
            let (ox, oy) = o;
            match face {
                0 => (ox + d + u, oy + d + v),
                1 => (ox + 2 * d + w + u, oy + d + v),
                2 => (ox + u, oy + d + v),
                3 => (ox + d + w + u, oy + d + v),
                4 => (ox + d + (w - 1 - u.min(w - 1)), oy + v),
                _ => (ox + d + w + (w - 1 - u.min(w - 1)), oy + v),
            }
        };
        // Calque du dessus : la tête l'a toujours, le reste seulement au format récent.
        if part == 5 || self.wide {
            let (x, y) = local(over);
            let p = self.at(x, y);
            if p[3] >= 128 {
                return Some(rgb(p));
            }
        }
        let (x, y) = local(base);
        let p = self.at(x, y);
        (p[3] >= 128).then(|| rgb(p))
    }

    /// Visage (8 × 8, chapeau compris), en sRGB, ligne par ligne.
    pub fn face(&self) -> [[u8; 3]; 64] {
        let mut out = [[0u8; 3]; 64];
        for v in 0..8 {
            for u in 0..8 {
                let hat = self.at(40 + u, 8 + v);
                let p = if hat[3] >= 128 {
                    hat
                } else {
                    self.at(8 + u, 8 + v)
                };
                out[(v * 8 + u) as usize] = [p[0], p[1], p[2]];
            }
        }
        out
    }
}

fn rgb(p: [u8; 4]) -> u32 {
    (p[0] as u32) << 16 | (p[1] as u32) << 8 | p[2] as u32
}

/// Dessine un visage de skin (8 × 8) dans un carré de l'interface.
pub fn draw_face(
    ui: &mut minelib_render::Ui,
    face: &[[u8; 3]; 64],
    x: f32,
    y: f32,
    size: f32,
    dim: f32,
) {
    let c = size / 8.0;
    for v in 0..8 {
        for u in 0..8 {
            let p = face[v * 8 + u];
            ui.rect(
                x + u as f32 * c,
                y + v as f32 * c,
                c + 0.5,
                c + 0.5,
                [
                    p[0] as f32 / 255.0 * dim,
                    p[1] as f32 / 255.0 * dim,
                    p[2] as f32 / 255.0 * dim,
                    1.0,
                ],
            );
        }
    }
}

/// Aperçu en 3D du joueur avec ce skin (ou le modèle par défaut) pour l'interface : debout,
/// tourné de `yaw`, centré en (`cx`, `cy`), haut de `height` pixels.
pub fn preview(
    skin: Option<Arc<Skin>>,
    cx: f32,
    cy: f32,
    height: f32,
    yaw: f32,
) -> Vec<minelib_render::VmVertex> {
    use glam::Vec3;
    let pose = crate::player_model::Pose {
        feet: Vec3::ZERO,
        yaw,
        pitch: 0.0,
        walk: 0.0,
        swing: 0.0,
        crouch: 0.0,
        light: [1.0; 3],
        held: None,
        skin,
    };
    let k = height / 1.9;
    // Un peu vu d'en haut, avec une légère perspective : on voit que c'est en volume.
    let tilt = glam::Mat3::from_rotation_x(-0.22);
    let mid = Vec3::new(0.0, 0.9, 0.0);
    crate::player_model::build(&pose)
        .into_iter()
        .map(|v| {
            let p = tilt * (Vec3::from(v.pos) - mid);
            let persp = 1.0 / (1.0 + p.z * 0.12);
            let p = Vec3::new(p.x * persp, p.y * persp, p.z) + mid;
            minelib_render::VmVertex {
                // Pixels de l'écran ; profondeur à l'envers (plus grand = plus proche, le
                // regard du modèle est vers −z, vers nous).
                clip: [
                    cx + p.x * k,
                    cy + height / 2.0 - p.y * k,
                    0.5 - p.z * 0.1,
                    1.0,
                ],
                uv: [0.0; 2],
                layer: minelib_render::NO_TEXTURE,
                _pad: 0,
                color: [v.color[0], v.color[1], v.color[2], 1.0],
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(w: u32, h: u32, f: impl Fn(u32, u32) -> [u8; 4]) -> Vec<u8> {
        let mut img = image::RgbaImage::new(w, h);
        for y in 0..h {
            for x in 0..w {
                img.put_pixel(x, y, image::Rgba(f(x, y)));
            }
        }
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    #[test]
    fn reads_both_formats_and_rejects_the_rest() {
        // Visage rouge, chapeau transparent ; le reste bleu.
        let bytes = png(64, 64, |x, y| {
            if (8..16).contains(&x) && (8..16).contains(&y) {
                [255, 0, 0, 255]
            } else if y < 16 && x >= 32 {
                [0, 0, 0, 0]
            } else {
                [0, 0, 255, 255]
            }
        });
        let s = Skin::from_png(&bytes).unwrap();
        assert_eq!(s.face()[0], [255, 0, 0]);
        // Devant de la tête = visage.
        assert_eq!(s.color(5, 0, 3, 3, [8, 8, 8]), Some(0xff0000));
        // Ancien format accepté, le bras gauche copié du droit.
        let old = png(64, 32, |x, y| {
            if (40..56).contains(&x) && (16..32).contains(&y) {
                [0, 255, 0, 255]
            } else {
                [9, 9, 9, 255]
            }
        });
        let s = Skin::from_png(&old).unwrap();
        assert_eq!(s.color(3, 0, 1, 1, [4, 12, 4]), Some(0x00ff00));
        // Mauvaise taille, pas un PNG, trop gros.
        assert!(Skin::from_png(&png(32, 32, |_, _| [0; 4])).is_err());
        assert!(Skin::from_png(b"pas une image").is_err());
        assert!(Skin::from_png(&vec![0u8; MAX_BYTES + 1]).is_err());
    }
}

/// Skins connus : le sien et ceux des autres (demandés au relais au besoin).
#[derive(Default)]
pub struct Skins {
    mine: Option<Arc<Skin>>,
    /// Pseudo (minuscules) → skin (`None` : pas de skin, modèle par défaut).
    others: std::collections::HashMap<String, Option<Arc<Skin>>>,
    /// Pseudos à demander au relais (vus sans skin connu).
    wanted: std::cell::RefCell<Vec<String>>,
}

impl Skins {
    pub fn set_mine(&mut self, skin: Option<Arc<Skin>>) {
        self.mine = skin;
    }

    pub fn mine(&self) -> Option<Arc<Skin>> {
        self.mine.clone()
    }

    /// Skin d'un autre joueur ; inconnu : demandé au relais (une fois).
    pub fn of(&self, pseudo: &str) -> Option<Arc<Skin>> {
        // Joueurs anonymes de l'échauffement (« Joueur 3 ») : modèle par défaut.
        if pseudo.is_empty() || pseudo.contains(' ') {
            return None;
        }
        let key = pseudo.to_lowercase();
        match self.others.get(&key) {
            Some(s) => s.clone(),
            None => {
                let mut w = self.wanted.borrow_mut();
                if !w.contains(&key) && w.len() < 64 {
                    w.push(key);
                }
                None
            }
        }
    }

    /// Pseudos à demander (et marqués comme demandés).
    pub fn take_wanted(&mut self) -> Vec<String> {
        let w = std::mem::take(&mut *self.wanted.borrow_mut());
        for k in &w {
            self.others.entry(k.clone()).or_insert(None);
        }
        w
    }

    /// Réponse du relais (PNG vide : pas de skin).
    pub fn received(&mut self, pseudo: &str, png: &[u8]) {
        let skin = (!png.is_empty())
            .then(|| Skin::from_png(png).ok())
            .flatten();
        self.others.insert(pseudo.to_lowercase(), skin);
    }

    /// Ce joueur a changé de skin : on le redemandera.
    pub fn forget(&mut self, pseudo: &str) {
        self.others.remove(&pseudo.to_lowercase());
    }
}
