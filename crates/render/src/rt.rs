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
    /// Skin du joueur (64 × 64 RGBA) pour le voir avec dans les reflets ; transparent :
    /// modèle par défaut.
    pub skin: wgpu::Texture,
    pub skin_view: wgpu::TextureView,
    /// Coin bas de la grille (coordonnées monde), envoyé au shader.
    pub origin: [i32; 3],
    /// Une grille a été envoyée (sinon elle est vide).
    pub uploaded: bool,
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
        let skin = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("raytracing : skin"),
            size: wgpu::Extent3d {
                width: 64,
                height: 64,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        Self {
            view: texture.create_view(&Default::default()),
            palette_view: palette.create_view(&Default::default()),
            skin_view: skin.create_view(&Default::default()),
            texture,
            palette,
            skin,
            origin: [0; 3],
            uploaded: false,
        }
    }

    /// Skin du joueur vu dans les reflets (64 × 64 RGBA, ligne par ligne) ; `None` : modèle
    /// par défaut.
    pub fn upload_skin(&self, queue: &wgpu::Queue, rgba: Option<&[u8]>) {
        let zero = vec![0u8; 64 * 64 * 4];
        let data = rgba.filter(|d| d.len() == 64 * 64 * 4).unwrap_or(&zero);
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.skin,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(64 * 4),
                rows_per_image: Some(64),
            },
            wgpu::Extent3d {
                width: 64,
                height: 64,
                depth_or_array_layers: 1,
            },
        );
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
        self.uploaded = true;
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
/// bits 0..24 boîte 0, 24..48 boîte 1, 48..50 genre (0 cube ou rien, 1 boîtes, 2 poteau,
/// 3 plante en croix : deux plans en diagonale, seulement pour les cartes d'ombre),
/// 50..52 sorte de poteau (0 clôture, 1 muret, 2 vitre), 52 cube à trous (feuillage).
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
        Shape::Cross => (3, 0, Vec::new()),
        _ => (0, 0, Vec::new()),
    };
    // Bit 52 : cube aux trous dans sa texture (feuillage, verre clair) : les ombres des
    // lumières dynamiques passent par ses trous (voir shadow_bake.wgsl).
    let cutout = (matches!(def.shape, Shape::Cube) && !def.full) as u64;
    let mut w = kind << 48 | post << 50 | cutout << 52;
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
/// dans le shader, voir `shape_texel`), torches et plantes en croix (elles font de l'ombre
/// aux lumières de l'éditeur ; les reflets les traversent) ; pas les blocs translucides.
pub fn traced(def: &minelib_world::registry::BlockDef) -> bool {
    (matches!(
        def.shape,
        Shape::Cube
            | Shape::Panel { .. }
            | Shape::Stairs { .. }
            | Shape::Post(_)
            | Shape::Box { .. }
            | Shape::Cross
    ) && !def.translucent)
        || matches!(def.shape, Shape::Torch | Shape::WallTorch(_))
}

/// Bloc qui bloque la vue (pour trouver les faces visibles des miroirs).
pub fn solid(def: &minelib_world::registry::BlockDef) -> bool {
    matches!(def.shape, Shape::Cube) && !def.translucent
}

/// Qualité adaptative du raytracing : baisse dès que la scène coûte trop au GPU (miroirs
/// face à face, grande étendue d'eau, chute d'eau entourée de lampes…), remonte
/// doucement quand il y a de la marge. 1 = pleine qualité (64 rebonds, 64 blocs) ;
/// 0 = un seul rebond et seulement les surfaces à moins de 6 blocs.
///
/// Les seuils visent plus de 200 images par seconde. Avant (seuil haut à 6 ms, plancher à
/// 2 rebonds et 16 blocs), un joueur à 250–380 images par seconde tombait à 116 devant
/// une chute d'eau éclairée (scène à 7 ms, mesuré), sans que la qualité puisse descendre
/// davantage.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RtQuality(pub f32);

pub const MIN_QUALITY: f32 = 0.0;
/// Temps de scène visé (ms) : au-dessus on baisse, bien en dessous on remonte.
const SCENE_HIGH_MS: f32 = 4.0;
const SCENE_LOW_MS: f32 = 2.8;

impl Default for RtQuality {
    fn default() -> Self {
        Self(1.0)
    }
}

impl RtQuality {
    /// Nouveau temps GPU de la scène (ms).
    pub fn update(&mut self, scene_ms: f32) {
        let q = self.0;
        self.0 = if scene_ms > SCENE_HIGH_MS * 3.0 {
            q * 0.5 - 0.004 // gros pic : on coupe fort tout de suite
        } else if scene_ms > SCENE_HIGH_MS {
            q * 0.9 - 0.004
        } else if scene_ms < SCENE_LOW_MS {
            q + 0.01
        } else {
            q
        }
        .clamp(MIN_QUALITY, 1.0);
    }

    /// (rebonds, portée en blocs) pour les uniformes (`screen.zw`).
    pub fn uniforms(self) -> [f32; 2] {
        let t = self.0.clamp(0.0, 1.0);
        let bounces = (1.0 + 63.0 * t * t).round();
        let range = 6.0 + 58.0 * t;
        [bounces, range]
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn raytracing_quality_drops_on_gpu_spikes_and_recovers_slowly() {
        let mut q = super::RtQuality::default();
        assert_eq!(q.uniforms(), [64.0, 64.0]);
        q.update(270.0);
        assert!(q.0 <= 0.5);
        // Scène à 7 ms (la chute d'eau éclairée du joueur) : la qualité descend jusqu'au
        // plancher, un rebond et 6 blocs de portée, bien en dessous de l'ancien plancher
        // (2 rebonds, 16 blocs) qui laissait la scène à 7 ms.
        for _ in 0..60 {
            q.update(7.0);
        }
        assert_eq!(q.0, super::MIN_QUALITY);
        assert_eq!(q.uniforms(), [1.0, 6.0]);
        // Entre les deux seuils : on ne touche à rien.
        q.update(3.4);
        assert_eq!(q.0, super::MIN_QUALITY);
        for _ in 0..200 {
            q.update(2.0);
        }
        assert_eq!(q.0, 1.0);
        // 5 ms suffisent maintenant à faire baisser (avant : seulement au-delà de 6 ms).
        q.update(5.0);
        assert!(q.0 < 1.0);
    }

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

    #[test]
    fn plants_are_in_the_grid_as_crosses() {
        // Les plantes en croix entrent dans la grille (ombres des projecteurs), avec le
        // genre 3 que les cartes d'ombre savent couper et que les reflets traversent.
        let reg = registry();
        let plants: Vec<_> = reg
            .defs
            .iter()
            .filter(|d| matches!(d.shape, Shape::Cross) && !d.translucent)
            .collect();
        assert!(!plants.is_empty());
        for p in plants {
            assert!(traced(p), "{}", p.key);
            assert_eq!(shape_texel(p)[3] & 3, 3, "{}", p.key);
        }
    }
}
