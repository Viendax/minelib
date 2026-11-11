//! Raytracing des reflets (écrit pour Minelib, sans SDK) : une grille de blocs autour du
//! joueur (texture 3D d'identifiants) et, pour chaque bloc, ses couches de texture.
//! Les surfaces réfléchissantes y lancent un rayon, case par case, depuis le shader.

use minelib_world::registry::{post_arms, registry, BlockDef, PostKind, Shape};

use crate::textures::BlockTextures;

/// Taille de la grille (blocs) : 128 × 64 × 128 autour du joueur.
pub const VOX: [u32; 3] = [128, 64, 128];
/// Nombre maximal d'identifiants de blocs dans la palette.
const PALETTE: u32 = 4096;
/// Largeur de la texture palette : couleurs (`id`) puis formes (`PALETTE + id`).
const PALETTE_W: u32 = PALETTE * 2;

pub struct Voxels {
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub palette: wgpu::Texture,
    pub palette_view: wgpu::TextureView,
    /// Coin bas de la grille (coordonnées monde), envoyé au shader.
    pub origin: [i32; 3],
}

impl Voxels {
    pub fn new(device: &wgpu::Device) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("raytracing : grille"),
            size: wgpu::Extent3d {
                width: VOX[0],
                height: VOX[1],
                depth_or_array_layers: VOX[2],
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D3,
            format: wgpu::TextureFormat::R16Uint,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let palette = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("raytracing : palette"),
            size: wgpu::Extent3d {
                width: PALETTE_W,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D1,
            format: wgpu::TextureFormat::Rgba16Uint,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        Self {
            view: texture.create_view(&Default::default()),
            palette_view: palette.create_view(&Default::default()),
            texture,
            palette,
            origin: [0; 3],
        }
    }

    /// Couches de texture de chaque bloc (dessus, côtés, dessous) : le rayon touché lit la
    /// vraie texture du bloc.
    pub fn upload_palette(&self, queue: &wgpu::Queue, _tex: &BlockTextures) {
        let reg = registry();
        let mut data = vec![0u16; (PALETTE_W * 4) as usize];
        for (i, def) in reg.defs.iter().enumerate().take(PALETTE as usize) {
            data[i * 4] = def.tex[0];
            data[i * 4 + 1] = def.tex[1];
            data[i * 4 + 2] = def.tex[2];
            // w : bit 0 = bloc lumineux, bits 4..7 = reflet, bits 8..10 = torche (1 droite,
            // 2..5 murale contre le mur 0, 1, 4, 5).
            let torch: u16 = match def.shape {
                Shape::Torch => 1,
                Shape::WallTorch(0) => 2,
                Shape::WallTorch(1) => 3,
                Shape::WallTorch(4) => 4,
                Shape::WallTorch(_) => 5,
                _ => 0,
            };
            let glow = (def.light.is_some() && torch == 0) as u16;
            data[i * 4 + 3] = glow | (def.reflect.min(15) as u16) << 4 | torch << 8;
            let shape = shape_texel(def);
            let j = (PALETTE as usize + i) * 4;
            data[j..j + 4].copy_from_slice(&shape);
        }
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.palette,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(&data),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(PALETTE_W * 8),
                rows_per_image: Some(1),
            },
            wgpu::Extent3d {
                width: PALETTE_W,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
    }

    /// Nouvelle grille : `ids[x + VOX.x * (y + VOX.y * z)]`, coin bas en `origin`.
    pub fn upload(&mut self, queue: &wgpu::Queue, origin: [i32; 3], ids: &[u16]) {
        if ids.len() != (VOX[0] * VOX[1] * VOX[2]) as usize {
            return;
        }
        self.origin = origin;
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(ids),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(VOX[0] * 2),
                rows_per_image: Some(VOX[1]),
            },
            wgpu::Extent3d {
                width: VOX[0],
                height: VOX[1],
                depth_or_array_layers: VOX[2],
            },
        );
    }
}

/// Forme d'un bloc non plein pour le shader, sur 64 bits (4 × u16) :
/// bits 0..24 boîte 0, 24..48 boîte 1, 48..50 genre (0 cube ou rien, 1 boîtes, 2 poteau),
/// 50..52 sorte de poteau (0 clôture, 1 muret, 2 vitre).
/// Boîte : min x, y, z puis max − 1 x, y, z, en seizièmes (4 bits chacun).
/// Poteau : boîte 0 = poteau, boîte 1 = bras vers +X (le shader le tourne vers chaque
/// voisin auquel le poteau se relie ; la clôture a une 2e traverse 6/16 plus haut).
pub fn shape_texel(def: &BlockDef) -> [u16; 4] {
    type Boxes = Vec<([u8; 3], [u8; 3])>;
    let pack = |(mn, mx): ([u8; 3], [u8; 3])| -> u64 {
        let c = |v: u8| v.min(15) as u64;
        let m = |v: u8| v.clamp(1, 16) as u64 - 1;
        c(mn[0]) | c(mn[1]) << 4 | c(mn[2]) << 8 | m(mx[0]) << 12 | m(mx[1]) << 16 | m(mx[2]) << 20
    };
    let (kind, post, boxes): (u64, u64, Boxes) = match def.shape {
        Shape::Panel { .. } | Shape::Stairs { .. } | Shape::Box { .. } => (1, 0, def.boxes()),
        Shape::Post(k) => {
            let arm = post_arms(k, 0)[0];
            let pk = match k {
                PostKind::Fence => 0,
                PostKind::Wall => 1,
                PostKind::Pane => 2,
            };
            (2, pk, vec![def.boxes()[0], arm])
        }
        _ => (0, 0, Vec::new()),
    };
    let mut w = kind << 48 | post << 50;
    for (k, b) in boxes.into_iter().take(2).enumerate() {
        w |= pack(b) << (24 * k);
    }
    [
        w as u16,
        (w >> 16) as u16,
        (w >> 32) as u16,
        (w >> 48) as u16,
    ]
}

/// Bloc retenu dans la grille : cubes opaques (et feuillages), blocs non pleins opaques
/// (dalles, escaliers, clôtures, murets, panneaux, boîtes : leurs boîtes sont retracées
/// dans le shader, voir `shape_texel`) et torches ; pas les plantes ni les blocs
/// translucides.
pub fn traced(def: &minelib_world::registry::BlockDef) -> bool {
    (matches!(
        def.shape,
        Shape::Cube
            | Shape::Panel { .. }
            | Shape::Stairs { .. }
            | Shape::Post(_)
            | Shape::Box { .. }
    ) && !def.translucent)
        || matches!(def.shape, Shape::Torch | Shape::WallTorch(_))
}

/// Bloc qui bloque la vue (pour trouver les faces visibles des miroirs).
pub fn solid(def: &minelib_world::registry::BlockDef) -> bool {
    matches!(def.shape, Shape::Cube) && !def.translucent
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slabs_and_stairs_are_packed_for_the_shader() {
        let reg = registry();
        let stairs = reg
            .defs
            .iter()
            .find(|d| matches!(d.shape, Shape::Stairs { top: false, .. }))
            .unwrap();
        let t = shape_texel(stairs);
        let w = t[0] as u64 | (t[1] as u64) << 16 | (t[2] as u64) << 32 | (t[3] as u64) << 48;
        assert_eq!(w >> 48 & 3, 1, "genre : boîtes");
        let b0 = w & 0xff_ffff;
        // Demi-dalle du bas : 0,0,0 → 16,8,16.
        let unpack = |b: u64| {
            (
                [b & 15, b >> 4 & 15, b >> 8 & 15],
                [(b >> 12 & 15) + 1, (b >> 16 & 15) + 1, (b >> 20 & 15) + 1],
            )
        };
        assert_eq!(unpack(b0), ([0, 0, 0], [16, 8, 16]));
        assert!(traced(stairs));
        let cube = reg.defs.iter().find(|d| d.key == "stone").unwrap();
        assert_eq!(shape_texel(cube)[3] & 3, 0);
    }
}
