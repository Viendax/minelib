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
#[derive(PartialEq)]
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

    /// (v177) Skin dessiné par le jeu (64 × 64 pixels RGBA, format récent) : les bots.
    pub fn from_pixels(px: Vec<[u8; 4]>) -> Arc<Skin> {
        assert_eq!(px.len(), 64 * 64);
        Arc::new(Skin { px, wide: true })
    }

    /// Pixels RGBA (64 × 64, ligne par ligne), pour la carte graphique.
    pub fn rgba(&self) -> Vec<u8> {
        self.px.iter().flatten().copied().collect()
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
        if part == 5 || self.wide {
            if let Some(c) = self.layer_color(part, face, u, v, size, true) {
                return Some(c);
            }
        }
        self.layer_color(part, face, u, v, size, false)
    }

    /// Couleur d'un seul calque (`top` : celui du dessus) ; `None` : pixel transparent.
    pub fn layer_color(
        &self,
        part: usize,
        face: usize,
        u: u32,
        v: u32,
        size: [u32; 3],
        top: bool,
    ) -> Option<u32> {
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
        if top && part != 5 && !self.wide {
            return None;
        }
        let (x, y) = local(if top { over } else { base });
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

/// Dessine un visage de skin (8 × 8) dans un carré de l'interface. (v209) Passé au gris
/// (`gray` : 0 en couleurs, 1 tout gris), puis assombri (`dim`).
pub fn draw_face_gray(
    ui: &mut minelib_render::Ui,
    face: &[[u8; 3]; 64],
    x: f32,
    y: f32,
    size: f32,
    dim: f32,
    gray: f32,
) {
    let c = size / 8.0;
    for v in 0..8 {
        for u in 0..8 {
            let p = face[v * 8 + u].map(|k| k as f32 / 255.0);
            let l = 0.3 * p[0] + 0.59 * p[1] + 0.11 * p[2];
            let m = |k: f32| (k + (l - k) * gray) * dim;
            ui.rect(
                x + u as f32 * c,
                y + v as f32 * c,
                c + 0.5,
                c + 0.5,
                [m(p[0]), m(p[1]), m(p[2]), 1.0],
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
        bow: 0.0,
        air: 0.0,
        dir: 0.0,
        run: 0.0,
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

    /// (v347) « Si y'avait vraiment 4000 joueurs » : les skins des plus proches sont demandés
    /// d'abord, et seuls les `WANTED_MAX` plus proches attendent leur tour ; les autres
    /// gardent le modèle par défaut jusqu'à ce qu'ils s'approchent.
    #[test]
    fn the_nearest_players_skins_are_asked_first_in_a_crowd() {
        let mut skins = Skins::default();
        // 4 000 joueurs vus, du plus loin au plus près.
        for i in (0..4000).rev() {
            assert!(skins.of_near(&format!("Joueur{i}"), i as f32).is_none());
        }
        assert_eq!(skins.wanted.borrow().len(), WANTED_MAX);
        assert_eq!(
            skins.take_wanted_max(4),
            vec!["joueur0", "joueur1", "joueur2", "joueur3"]
        );
        // Un joueur lointain qui s'approche passe devant.
        assert!(skins.of_near("Joueur3999", 0.5).is_none());
        assert_eq!(skins.take_wanted_max(1), vec!["joueur3999"]);
        // Déjà demandé : pas redemandé en attendant sa réponse.
        assert!(skins.of_near("Joueur0", 0.0).is_none());
        assert_eq!(skins.take_wanted_max(1), vec!["joueur4"]);
        // Sans distance (liste des joueurs) : après ceux qu'on voit.
        assert!(skins.of("Lointain").is_none());
        let rest = skins.take_wanted_max(usize::MAX);
        assert_eq!(rest.first().map(String::as_str), Some("joueur5"));
        assert_eq!(rest.last().map(String::as_str), Some("lointain"));
        assert!(skins.take_wanted_max(usize::MAX).is_empty());
        // « Plus tard » : redemandé quand on le revoit ; un skin déjà connu reste.
        assert!(skins.received("Joueur5", minelib_net::proto::SKIN_LATER));
        assert!(skins.of_near("Joueur5", 3.0).is_none());
        assert_eq!(skins.take_wanted_max(1), vec!["joueur5"]);
        let png = {
            let img = image::RgbaImage::from_pixel(64, 64, image::Rgba([200, 10, 10, 255]));
            let mut out = std::io::Cursor::new(Vec::new());
            img.write_to(&mut out, image::ImageFormat::Png).unwrap();
            out.into_inner()
        };
        assert!(!skins.received("Joueur6", &png));
        assert!(skins.received("Joueur6", minelib_net::proto::SKIN_LATER));
        assert!(skins.of_near("Joueur6", 1.0).is_some());
        // Ni un skin illisible pour un jeu d'avant : le relais ne l'accepte pas comme skin.
        assert!(Skin::from_png(minelib_net::proto::SKIN_LATER).is_err());
    }
}

/// (v347) Pseudos à demander gardés au plus : les plus proches (`Skins::of_near`).
const WANTED_MAX: usize = 64;

/// Skins connus : le sien et ceux des autres (demandés au relais au besoin).
#[derive(Default)]
pub struct Skins {
    mine: Option<Arc<Skin>>,
    /// Pseudo (minuscules) → skin (`None` : pas de skin, modèle par défaut).
    others: std::collections::HashMap<String, Option<Arc<Skin>>>,
    /// Pseudos à demander au relais (vus sans skin connu) : (v347) la distance du plus près
    /// qu'on les a vus, et leur ordre d'arrivée. Les plus proches partent d'abord, à égalité
    /// les premiers vus. Le modèle par défaut est dessiné en attendant.
    wanted: std::cell::RefCell<std::collections::HashMap<String, (f32, u64)>>,
    /// (v347) Ordre d'arrivée du prochain pseudo à demander.
    wanted_seq: std::cell::Cell<u64>,
    /// (v347) `wanted` plein : la plus grande distance gardée (ceux de plus loin sont
    /// refusés d'un coup, sans parcourir la liste à chaque image).
    farthest: std::cell::Cell<f32>,
    /// (v177) Skins des bots (nom, équipe), dessinés une fois.
    bots: std::cell::RefCell<std::collections::HashMap<(String, u8), Arc<Skin>>>,
}

impl Skins {
    pub fn set_mine(&mut self, skin: Option<Arc<Skin>>) {
        self.mine = skin;
    }

    pub fn mine(&self) -> Option<Arc<Skin>> {
        self.mine.clone()
    }

    /// Skin d'un autre joueur ; inconnu : demandé au relais (une fois), après ceux qu'on voit
    /// près de soi (`of_near`).
    pub fn of(&self, pseudo: &str) -> Option<Arc<Skin>> {
        self.of_near(pseudo, f32::MAX)
    }

    /// (v347) Skin d'un autre joueur vu à `dist` blocs ; inconnu : demandé au relais, les
    /// plus proches d'abord. Dans une foule (« si y'avait vraiment 4000 joueurs »), seuls
    /// les `WANTED_MAX` plus proches attendent leur tour ; ceux de plus loin gardent le modèle
    /// par défaut jusqu'à ce qu'ils s'approchent.
    pub fn of_near(&self, pseudo: &str, dist: f32) -> Option<Arc<Skin>> {
        // Joueurs anonymes de l'échauffement (« Joueur 3 ») : modèle par défaut.
        if pseudo.is_empty() || pseudo.contains(' ') {
            return None;
        }
        // (v191) Des milliers de joueurs autour de soi : la clé en minuscules tient sur
        // la pile, et non une chaîne allouée par joueur et par image.
        let mut buf = [0u8; 48];
        let owned;
        let key: &str = if pseudo.len() <= buf.len() && pseudo.is_ascii() {
            let b = &mut buf[..pseudo.len()];
            b.copy_from_slice(pseudo.as_bytes());
            b.make_ascii_lowercase();
            std::str::from_utf8(b).unwrap_or(pseudo)
        } else {
            owned = pseudo.to_lowercase();
            &owned
        };
        match self.others.get(key) {
            Some(s) => s.clone(),
            None => {
                self.want(key, dist);
                None
            }
        }
    }

    /// (v347) À demander, vu à `dist` blocs (le plus près qu'on l'ait vu compte).
    fn want(&self, key: &str, dist: f32) {
        let mut w = self.wanted.borrow_mut();
        if let Some(d) = w.get_mut(key) {
            d.0 = d.0.min(dist);
            return;
        }
        if w.len() >= WANTED_MAX {
            // Plein : seulement plus près que le plus loin gardé, qui laisse sa place.
            if dist >= self.farthest.get() {
                return;
            }
            // (`farthest` peut être trop grand : un joueur gardé s'est approché depuis.)
            let Some((k, d)) = w
                .iter()
                .max_by(|a, b| a.1 .0.total_cmp(&b.1 .0))
                .map(|(k, d)| (k.clone(), d.0))
            else {
                return;
            };
            if dist >= d {
                self.farthest.set(d);
                return;
            }
            w.remove(&k);
        }
        let seq = self.wanted_seq.get();
        self.wanted_seq.set(seq + 1);
        w.insert(key.to_string(), (dist, seq));
        if w.len() >= WANTED_MAX {
            self.farthest
                .set(w.values().fold(0.0f32, |m, d| m.max(d.0)));
        }
    }

    /// (v344) Skin connu de ce pseudo, sans rien demander au relais : pour les joueurs
    /// simulés de la foule d'essai, qui n'ont pas de compte. Avant, chacun était demandé au
    /// relais (`of`) : 4 000 demandes d'affilée pour une foule de 4 000, qui bloquaient la
    /// connexion du joueur pendant des minutes (26 novembre, 02:54).
    pub fn cached(&self, pseudo: &str) -> Option<Arc<Skin>> {
        if pseudo.is_empty() || pseudo.contains(' ') {
            return None;
        }
        self.others.get(&pseudo.to_lowercase()).cloned().flatten()
    }

    /// (v177) Skin d'un joueur d'une partie FPS : un bot a le sien (dessiné pour lui, selon
    /// son équipe : `bot_skin`), les autres comme `of`.
    pub fn in_match(&self, pseudo: &str, team: u8) -> Option<Arc<Skin>> {
        if !pseudo.starts_with("BOT ") {
            return self.of(pseudo);
        }
        let mut bots = self.bots.borrow_mut();
        Some(
            bots.entry((pseudo.to_string(), team))
                .or_insert_with(|| crate::bot_skin::bot_skin(pseudo, team))
                .clone(),
        )
    }

    /// (v256) Skin que porte un joueur d'une partie FPS : « en FPS, mets les skins CT et T des
    /// bots aux joueurs ». Tout le monde, bots et joueurs, porte la tenue de son équipe
    /// (`bot_skin` : tirée de son pseudo, toujours la même, la même sur tous les écrans) ; son
    /// skin personnel reste celui de son visage au tableau des scores (`in_match`) et hors
    /// partie. Sans équipe (spectateur) : comme `in_match`.
    pub fn worn(&self, pseudo: &str, team: u8) -> Option<Arc<Skin>> {
        if team != minelib_net::game::CT && team != minelib_net::game::T {
            return self.in_match(pseudo, team);
        }
        let mut bots = self.bots.borrow_mut();
        Some(
            bots.entry((pseudo.to_string(), team))
                .or_insert_with(|| crate::bot_skin::bot_skin(pseudo, team))
                .clone(),
        )
    }

    /// Redemande ce skin au relais (il a pu changer), en gardant celui connu d'ici là.
    pub fn refresh(&self, pseudo: &str) {
        if pseudo.is_empty() || pseudo.contains(' ') {
            return;
        }
        self.want(&pseudo.to_lowercase(), f32::MAX);
    }

    /// Pseudos à demander (et marqués comme demandés). (v344) Au plus `max` ; les autres
    /// restent à demander. Chaque
    /// demande attend sa réponse sur la connexion du joueur : par paquets, ses autres
    /// commandes ne restent pas derrière elles. (v347) Les plus proches d'abord.
    pub fn take_wanted_max(&mut self, max: usize) -> Vec<String> {
        if max == 0 {
            return Vec::new();
        }
        let w: Vec<String> = {
            let mut all = self.wanted.borrow_mut();
            let mut order: Vec<(String, (f32, u64))> =
                all.iter().map(|(k, d)| (k.clone(), *d)).collect();
            order.sort_by(|a, b| a.1 .0.total_cmp(&b.1 .0).then(a.1 .1.cmp(&b.1 .1)));
            order.truncate(max);
            for (k, _) in &order {
                all.remove(k);
            }
            order.into_iter().map(|(k, _)| k).collect()
        };
        for k in &w {
            self.others.entry(k.clone()).or_insert(None);
        }
        w
    }

    /// Réponse du relais (PNG vide : pas de skin). (v347) Vrai pour « plus tard »
    /// (`proto::SKIN_LATER`) : le relais en a déjà envoyé assez. Ce pseudo sera redemandé
    /// quand on le reverra, avec les autres, les plus proches d'abord ; d'ici là, le modèle
    /// par défaut (ou le skin déjà connu).
    pub fn received(&mut self, pseudo: &str, png: &[u8]) -> bool {
        let key = pseudo.to_lowercase();
        if png == minelib_net::proto::SKIN_LATER {
            if matches!(self.others.get(&key), Some(None)) {
                self.others.remove(&key);
            }
            return true;
        }
        let skin = (!png.is_empty())
            .then(|| Skin::from_png(png).ok())
            .flatten();
        self.others.insert(key, skin);
        false
    }

    /// Ce joueur a changé de skin : on le redemandera.
    pub fn forget(&mut self, pseudo: &str) {
        self.others.remove(&pseudo.to_lowercase());
    }
}
