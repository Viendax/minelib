//! Raytracing des reflets (écrit pour Minelib, sans SDK) : une grille de blocs autour du
//! joueur (texture 3D d'identifiants) et, pour chaque bloc, ses couches de texture.
//! Les surfaces réfléchissantes y lancent un rayon, case par case, depuis le shader.

use minelib_world::registry::{registry, Shape};

use crate::textures::BlockTextures;

/// Taille de la grille (blocs) : 128 × 64 × 128 autour du joueur.
pub const VOX: [u32; 3] = [128, 64, 128];
/// Nombre maximal d'identifiants de blocs dans la palette.
const PALETTE: u32 = 4096;

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
                width: PALETTE,
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
        let mut data = vec![0u16; (PALETTE * 4) as usize];
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
                bytes_per_row: Some(PALETTE * 8),
                rows_per_image: Some(1),
            },
            wgpu::Extent3d {
                width: PALETTE,
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

/// Bloc retenu dans la grille : cubes opaques (et feuillages) et torches (leur modèle est
/// retracé dans le shader) ; pas les autres objets fins.
pub fn traced(def: &minelib_world::registry::BlockDef) -> bool {
    (matches!(def.shape, Shape::Cube) && !def.translucent)
        || matches!(def.shape, Shape::Torch | Shape::WallTorch(_))
}

/// Bloc qui bloque la vue (pour trouver les faces visibles des miroirs).
pub fn solid(def: &minelib_world::registry::BlockDef) -> bool {
    matches!(def.shape, Shape::Cube) && !def.translucent
}
