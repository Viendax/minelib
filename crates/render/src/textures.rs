//! Textures tirées du pack de ressources :
//! - un **tableau de textures** (une couche par image de bloc, avec mipmaps) pour le monde ;
//! - un **atlas d'interface** : police 8×8, éléments `textures/gui/`, icônes d'objets.

use std::collections::HashMap;

use minelib_assets::{ResourcePack, Rgba};
use minelib_world::flame::{CHEMICALS, WOOD_FLAME};
use minelib_world::registry::{registry, TexSrc};
use minelib_world::BlockId;

/// Textures des blocs : une couche par entrée de `registry().textures`, dans le même ordre
/// (les sommets portent directement leur numéro de couche).
pub struct BlockTextures {
    pub size: u32,
    pub layers: Vec<Rgba>,
}

/// Image d'une source de texture du registre.
pub fn resolve(pack: &ResourcePack, src: &TexSrc) -> Rgba {
    let load = |name: &str| {
        pack.block_image(name).or_else(|| {
            log::warn!(
                "pack « {} » : textures/blocks/{name}.png manquante",
                pack.name
            );
            pack.image("textures/blocks/missing.png")
        })
    };
    match src {
        TexSrc::Pack(n) => load(n).unwrap_or_else(|| Rgba::missing(16)),
        TexSrc::Flat(c) => flat(*c),
        TexSrc::Tint(n, c) => {
            let mut img = load(n).unwrap_or_else(|| Rgba::missing(16));
            for px in img.data.chunks_exact_mut(4) {
                for k in 0..3 {
                    px[k] = (px[k] as u32 * c[k] as u32 / 255) as u8;
                }
            }
            img
        }
    }
}

/// Couleur unie avec un léger grain (16 × 16), pour les blocs absents du pack.
fn flat(c: [u8; 3]) -> Rgba {
    let mut data = Vec::with_capacity(16 * 16 * 4);
    for i in 0..256u32 {
        let h = (i.wrapping_mul(2654435761) >> 24) as f32 / 255.0;
        let k = 0.94 + 0.1 * h;
        data.extend_from_slice(&[
            (c[0] as f32 * k).min(255.0) as u8,
            (c[1] as f32 * k).min(255.0) as u8,
            (c[2] as f32 * k).min(255.0) as u8,
            255,
        ]);
    }
    Rgba {
        width: 16,
        height: 16,
        data,
    }
}

pub fn load_block_textures(pack: &ResourcePack) -> BlockTextures {
    // Les textures animées (bandes verticales) : on garde la première image.
    let mut images: Vec<Rgba> = registry()
        .textures
        .iter()
        .map(|t| {
            let img = resolve(pack, t);
            if img.height > img.width {
                crop(&img, img.width)
            } else {
                img
            }
        })
        .collect();
    // Toutes les couches à la même taille : la résolution la plus courante du pack
    // (32 px pour un pack 32x, même s'il contient quelques images plus grandes).
    let mut counts: HashMap<u32, usize> = HashMap::new();
    for i in &images {
        *counts.entry(i.width.next_power_of_two()).or_default() += 1;
    }
    let size = counts
        .into_iter()
        .max_by_key(|&(w, n)| (n, w))
        .map(|(w, _)| w)
        .unwrap_or(16)
        .clamp(16, 256);
    for img in &mut images {
        if img.width != size || img.height != size {
            *img = img.resize_nearest(size, size);
        }
    }
    BlockTextures {
        size,
        layers: images,
    }
}

fn crop(img: &Rgba, h: u32) -> Rgba {
    Rgba {
        width: img.width,
        height: h,
        data: img.data[..(img.width * h * 4) as usize].to_vec(),
    }
}

/// Niveau de mipmap suivant (moyenne 2×2, pondérée par l'alpha).
pub fn downsample(img: &Rgba) -> Rgba {
    let (w, h) = ((img.width / 2).max(1), (img.height / 2).max(1));
    let mut data = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let mut acc = [0u32; 4];
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let p = img.pixel(
                    (x * 2 + dx).min(img.width - 1),
                    (y * 2 + dy).min(img.height - 1),
                );
                let a = p[3] as u32;
                for k in 0..3 {
                    acc[k] += p[k] as u32 * a;
                }
                acc[3] += a;
            }
            let a = acc[3].max(1);
            data.extend_from_slice(&[
                (acc[0] / a) as u8,
                (acc[1] / a) as u8,
                (acc[2] / a) as u8,
                (acc[3] / 4) as u8,
            ]);
        }
    }
    Rgba {
        width: w,
        height: h,
        data,
    }
}

// ---------------------------------------------------------------- atlas d'interface

pub const ATLAS_SIZE: u32 = 2048;
/// La police 8×8 occupe le coin (0, 0) : 16 × 16 cases de 8 px.
pub const FONT_CELL: u32 = 8;

/// Éléments d'interface chargés depuis `textures/gui/`.
pub const GUI_SPRITES: &[&str] = &[
    "button",
    "button_hover",
    "button_disabled",
    "panel",
    "panel_dark",
    "slot",
    "slot_highlight",
    "hotbar_selected",
    "menu_background",
    "crosshair",
    "checkbox_on",
    "checkbox_off",
    "icons/health_full",
    "icons/health_half",
    "icons/health_empty",
];

pub struct GuiAtlas {
    pub image: Rgba,
    /// Nom → rectangle (x, y, l, h) dans l'atlas.
    pub sprites: HashMap<String, [u32; 4]>,
}

/// Rangement simple par étagères.
struct Packer {
    x: u32,
    y: u32,
    row: u32,
}

impl Packer {
    fn place(&mut self, w: u32, h: u32) -> Option<(u32, u32)> {
        if self.x + w > ATLAS_SIZE {
            self.x = 0;
            self.y += self.row + 1;
            self.row = 0;
        }
        if self.y + h > ATLAS_SIZE {
            return None;
        }
        let p = (self.x, self.y);
        self.x += w + 1;
        self.row = self.row.max(h);
        Some(p)
    }
}

pub fn build_gui_atlas(pack: &ResourcePack, font: &[u8]) -> GuiAtlas {
    let mut image = Rgba {
        width: ATLAS_SIZE,
        height: ATLAS_SIZE,
        data: vec![0; (ATLAS_SIZE * ATLAS_SIZE * 4) as usize],
    };
    // Police : blanc, alpha = couverture.
    for y in 0..128u32 {
        for x in 0..128u32 {
            let i = ((y * ATLAS_SIZE + x) * 4) as usize;
            let a = font[(y * 128 + x) as usize];
            image.data[i..i + 4].copy_from_slice(&[255, 255, 255, a]);
        }
    }
    let mut sprites = HashMap::new();
    let mut packer = Packer {
        x: 0,
        y: 129,
        row: 0,
    };
    let mut add =
        |name: String, img: &Rgba, image: &mut Rgba, sprites: &mut HashMap<String, [u32; 4]>| {
            let Some((px, py)) = packer.place(img.width, img.height) else {
                log::warn!("atlas d'interface plein, {name} ignoré");
                return;
            };
            for y in 0..img.height {
                let src = ((y * img.width) * 4) as usize;
                let dst = (((py + y) * ATLAS_SIZE + px) * 4) as usize;
                image.data[dst..dst + (img.width * 4) as usize]
                    .copy_from_slice(&img.data[src..src + (img.width * 4) as usize]);
            }
            sprites.insert(name, [px, py, img.width, img.height]);
        };

    for name in GUI_SPRITES {
        let img = pack.gui_image(name).unwrap_or_else(|| Rgba::missing(16));
        add(format!("gui/{name}"), &img, &mut image, &mut sprites);
    }
    // Icônes 3D (vue isométrique) de tous les blocs, peintes depuis leurs vraies faces et
    // leur forme (dalles, escaliers, clôtures…) ; plantes : image plate.
    let layers: Vec<Rgba> = registry()
        .textures
        .iter()
        .map(|t| {
            let img = resolve(pack, t);
            if img.height > img.width {
                crop(&img, img.width)
            } else {
                img
            }
        })
        .collect();
    for i in 1..registry().len() {
        let block = BlockId(i as u16);
        let icon = block_icon(block, &layers);
        add(
            format!("icon/block/{}", block.0),
            &icon,
            &mut image,
            &mut sprites,
        );
    }
    // Torches : la flamme du pack recolorée selon le sel.
    let torch = pack
        .block_image("torch_on")
        .unwrap_or_else(|| Rgba::missing(16));
    let torch = if torch.height > torch.width {
        crop(&torch, torch.width)
    } else {
        torch
    };
    let torch = if torch.width != 16 {
        torch.resize_nearest(16, 16)
    } else {
        torch
    };
    add(
        "icon/torch".into(),
        &recolor_flame(&torch, WOOD_FLAME, true),
        &mut image,
        &mut sprites,
    );
    for (i, c) in CHEMICALS.iter().enumerate() {
        add(
            format!("icon/torch/{i}"),
            &recolor_flame(&torch, c.rgb, false),
            &mut image,
            &mut sprites,
        );
    }
    GuiAtlas { image, sprites }
}

pub const ICON_SIZE: u32 = 32;

/// Icône isométrique d'un bloc (32 × 32), comme dans l'inventaire de Minecraft : dessus
/// clair, côté gauche moyen, côté droit sombre.
pub fn block_icon(block: BlockId, layers: &[Rgba]) -> Rgba {
    use minelib_world::registry::Shape;
    let n = ICON_SIZE;
    let mut out = Rgba {
        width: n,
        height: n,
        data: vec![0; (n * n * 4) as usize],
    };
    let def = block.def();
    let tex = |i: u16| &layers[(i as usize).min(layers.len().saturating_sub(1))];
    if matches!(
        def.shape,
        Shape::Cross | Shape::Torch | Shape::WallTorch(_) | Shape::Invisible | Shape::Air
    ) {
        // Image plate, centrée.
        let t = tex(def.tex[1]).resize_nearest(n, n);
        return t;
    }
    let boxes = if def.full {
        vec![([0u8; 3], [16u8; 3])]
    } else {
        def.boxes()
    };
    let mut boxes: Vec<_> = boxes
        .into_iter()
        .map(|(a, b)| (a.map(|v| v as f32 / 16.0), b.map(|v| v as f32 / 16.0)))
        .collect();
    // Du plus loin au plus proche (le regard vient de +X, +Y, +Z).
    boxes.sort_by(|p, q| {
        let k = |b: &([f32; 3], [f32; 3])| (0..3).map(|i| b.0[i] + b.1[i]).sum::<f32>();
        k(p).total_cmp(&k(q))
    });
    let k = 14.5f32;
    let (cx, cy) = (n as f32 / 2.0, n as f32 / 2.0);
    let project =
        |x: f32, y: f32, z: f32| ((x - z) * 0.866 * k + cx, (x + z) * 0.5 * k - y * k + cy);
    let mut plot = |sx: f32, sy: f32, c: [u8; 4], shade: f32| {
        let (px, py) = (sx.floor() as i32, sy.floor() as i32);
        if px < 0 || py < 0 || px >= n as i32 || py >= n as i32 || c[3] < 8 {
            return;
        }
        let i = ((py as u32 * n + px as u32) * 4) as usize;
        let a = c[3] as f32 / 255.0;
        for (ch, v) in c.iter().take(3).enumerate() {
            let src = *v as f32 * shade;
            out.data[i + ch] = (src * a + out.data[i + ch] as f32 * (1.0 - a)).round() as u8;
        }
        out.data[i + 3] = out.data[i + 3].max(c[3]);
    };
    let sample = |img: &Rgba, u: f32, v: f32| {
        let x = ((u.clamp(0.0, 0.9999)) * img.width as f32) as u32;
        let y = ((v.clamp(0.0, 0.9999)) * img.height as f32) as u32;
        img.pixel(x, y)
    };
    const STEP: f32 = 1.0 / 72.0;
    for (a, b) in boxes {
        // Côté gauche (+Z), côté droit (+X), puis le dessus (+Y).
        let mut u = a[0];
        while u < b[0] {
            let mut y = a[1];
            while y < b[1] {
                let (sx, sy) = project(u, y, b[2]);
                plot(sx, sy, sample(tex(def.tex[1]), u, 1.0 - y), 0.8);
                y += STEP;
            }
            u += STEP;
        }
        let mut w = a[2];
        while w < b[2] {
            let mut y = a[1];
            while y < b[1] {
                let (sx, sy) = project(b[0], y, w);
                plot(sx, sy, sample(tex(def.tex[1]), w, 1.0 - y), 0.6);
                y += STEP;
            }
            w += STEP;
        }
        let mut x = a[0];
        while x < b[0] {
            let mut z = a[2];
            while z < b[2] {
                let (sx, sy) = project(x, b[1], z);
                plot(sx, sy, sample(tex(def.tex[0]), x, z), 1.0);
                z += STEP;
            }
            x += STEP;
        }
    }
    out
}

/// Recolore la flamme (pixels clairs et saturés de la moitié haute) d'une image de torche.
fn recolor_flame(img: &Rgba, rgb: [u8; 3], keep: bool) -> Rgba {
    let mut out = img.clone();
    if keep {
        return out;
    }
    for y in 0..img.height / 2 {
        for x in 0..img.width {
            let [r, g, b, a] = img.pixel(x, y);
            if a == 0 {
                continue;
            }
            let bright = (r as u32 + g as u32 + b as u32) / 3;
            let sat = r.max(g).max(b) - r.min(g).min(b);
            if sat > 60 || bright > 200 {
                let k = (r.max(g).max(b) as f32 / 255.0).max(0.6);
                let i = ((y * img.width + x) * 4) as usize;
                for (c, v) in rgb.iter().enumerate() {
                    out.data[i + c] = (*v as f32 * k).min(255.0) as u8;
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_pack_has_every_registry_texture() {
        let pack = ResourcePack::builtin().unwrap();
        for t in &registry().textures {
            if let TexSrc::Pack(n) | TexSrc::Tint(n, _) = t {
                // « missing » : damier dessiné par le jeu si le pack n'en a pas.
                assert!(
                    *n == "missing" || pack.block_image(n).is_some(),
                    "texture manquante : {n}"
                );
            }
        }
        let t = load_block_textures(&pack);
        assert_eq!(t.size, 16);
        assert_eq!(t.layers.len(), registry().textures.len());
        let m = downsample(&t.layers[1]);
        assert_eq!((m.width, m.height), (8, 8));
    }

    /// `MINELIB_PACK=/chemin/pack.zip cargo test -p minelib-render real_pack -- --nocapture`
    #[test]
    fn real_pack_coverage() {
        let Ok(path) = std::env::var("MINELIB_PACK") else {
            return;
        };
        let path = std::path::PathBuf::from(path);
        let info = minelib_assets::PackInfo {
            name: "test".into(),
            path,
        };
        let pack = ResourcePack::open(&info).unwrap();
        let mut missing = Vec::new();
        let mut names = Vec::new();
        for t in &registry().textures {
            if let TexSrc::Pack(n) | TexSrc::Tint(n, _) = t {
                names.push(*n);
                if pack.own_block_textures(&[n]) == 0 {
                    missing.push(*n);
                }
            }
        }
        println!(
            "{} / {} fournies ; absentes : {:?}",
            names.len() - missing.len(),
            names.len(),
            missing
        );
        let t = load_block_textures(&pack);
        println!("taille {}", t.size);
        assert!(missing.len() < names.len() / 4);
    }

    #[test]
    fn gui_atlas_contains_sprites_and_torches() {
        let pack = ResourcePack::builtin().unwrap();
        let a = build_gui_atlas(&pack, &vec![0; 128 * 128]);
        assert!(a.sprites.contains_key("gui/button"));
        assert!(a.sprites.contains_key("icon/torch/0"));
        assert!(a
            .sprites
            .contains_key(&format!("icon/block/{}", BlockId::GRASS.0)));
    }
}
