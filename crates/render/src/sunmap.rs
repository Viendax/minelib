//! (v217) Carte d'ombre du soleil des joueurs.
//!
//! Les ombres exactes des joueurs (`ShadowBox`, boîtes essayées par chaque pixel) sont
//! limitées à 48 morceaux, soit 8 à 9 joueurs : les plus proches de la caméra ou d'une
//! lumière. Dans une foule, ou à 10 contre 10, les autres n'avaient pas d'ombre. Ceux-là sont
//! dessinés chaque image, en profondeur seule, dans une carte vue du soleil (projection
//! orthographique centrée sur la caméra, alignée sur ses texels pour que l'ombre ne
//! scintille pas quand on bouge) : le shader du monde y lit si un joueur est entre le point
//! et le soleil. Autant de joueurs qu'on veut, pour une passe de profondeur et quatre
//! lectures par pixel au soleil.

use glam::{Mat4, Vec3};

/// Côté de la carte (texels). Ombres jusqu'à 128 blocs : un texel fait 1/8 de bloc environ
/// (un bras ou une jambe, un quart de bloc, en fait deux).
pub const SIZE: u32 = 2048;
pub const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
/// Marge de profondeur (blocs) : un point du monde n'est pas dans l'ombre d'un joueur qui
/// est à moins que ça de lui le long du rayon du soleil (le dessous des pieds posés).
const BIAS_BLOCKS: f32 = 0.05;
/// Ce que la carte prend de part et d'autre de la caméra le long du soleil, au-delà de sa
/// demi-largeur (blocs).
const DEPTH_MARGIN: f32 = 96.0;

/// Vue-projection orthographique depuis le soleil (`sun` : direction vers le soleil),
/// centrée sur `center`, carré de demi-côté `half` (blocs). Le centre est aligné sur les
/// texels de la carte (le plan de la carte avance d'un texel entier) : une ombre ne bouge pas
/// quand seule la caméra bouge. Renvoie aussi la marge de profondeur (unités de la carte).
pub fn view_proj(sun: Vec3, center: Vec3, half: f32) -> (Mat4, f32) {
    let d = -sun.normalize_or(Vec3::Y);
    let up = if d.y.abs() > 0.99 { Vec3::Z } else { Vec3::Y };
    let right = d.cross(up).normalize();
    let up = right.cross(d).normalize();
    let texel = 2.0 * half / SIZE as f32;
    let cx = (center.dot(right) / texel).floor() * texel;
    let cy = (center.dot(up) / texel).floor() * texel;
    let c = right * cx + up * cy + d * center.dot(d);
    let depth = half + DEPTH_MARGIN;
    let view = Mat4::look_to_rh(c - d * depth, d, up);
    let proj = Mat4::orthographic_rh(-half, half, -half, half, 0.0, 2.0 * depth);
    (proj * view, BIAS_BLOCKS / (2.0 * depth))
}

/// La carte, sa vue factice (pour le groupe du monde de sa propre passe : on ne lit pas une
/// image pendant qu'on y dessine), et les uniformes de sa passe.
pub(crate) struct SunMap {
    pub view: wgpu::TextureView,
    pub dummy: wgpu::TextureView,
    pub globals: wgpu::Buffer,
}

impl SunMap {
    pub fn new(device: &wgpu::Device, globals_size: u64) -> Self {
        let tex = |label: &str, size: u32| {
            device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d {
                        width: size,
                        height: size,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: FORMAT,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                        | wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                })
                .create_view(&Default::default())
        };
        Self {
            view: tex("ombres des joueurs au soleil", SIZE),
            dummy: tex("ombres des joueurs au soleil (vide)", 1),
            globals: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("ombres des joueurs au soleil : uniformes"),
                size: globals_size,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Un point se projette dans la carte comme le shader le lit (uv, profondeur) : le joueur
    /// au-dessus d'un point du sol, le long du soleil, est plus près du soleil que lui ; la
    /// carte couvre la demi-largeur demandée ; et le centre avance par texels entiers.
    #[test]
    fn the_sun_map_sees_a_player_above_the_ground_point() {
        let uv_z = |m: Mat4, p: Vec3| {
            let c = m * p.extend(1.0);
            (glam::Vec2::new(c.x * 0.5 + 0.5, 0.5 - c.y * 0.5), c.z / c.w)
        };
        for sun in [
            Vec3::new(0.3, 0.8, 0.2),
            Vec3::new(-0.7, 0.25, 0.4),
            Vec3::Y,
        ] {
            let sun = sun.normalize();
            let cam = Vec3::new(1234.5, 70.0, -987.25);
            let (m, bias) = view_proj(sun, cam, 132.0);
            assert!(bias > 0.0 && bias < 1e-3);
            let ground = cam + Vec3::new(40.0, -5.0, -30.0);
            let head = ground + sun * 1.6;
            let (ug, zg) = uv_z(m, ground);
            let (uh, zh) = uv_z(m, head);
            assert!((ug - uh).length() < 1e-4, "{sun} : même texel");
            assert!(zh < zg - bias, "{sun} : la tête est plus près du soleil");
            assert!((0.0..=1.0).contains(&zg) && (0.0..=1.0).contains(&zh));
            assert!(ug.cmpge(glam::Vec2::ZERO).all() && ug.cmple(glam::Vec2::ONE).all());
            // À 130 blocs de la caméra (en travers du soleil), toujours dans la carte.
            let side = sun.cross(Vec3::X).normalize_or(Vec3::Z) * 130.0;
            let (us, _) = uv_z(m, cam + side);
            assert!(us.cmpge(glam::Vec2::ZERO).all() && us.cmple(glam::Vec2::ONE).all());
            // Caméra déplacée d'un peu moins d'un texel : la carte ne bouge pas, ou d'un
            // texel entier.
            let (m2, _) = view_proj(sun, cam + Vec3::new(0.03, 0.0, 0.02), 132.0);
            let (u2, _) = uv_z(m2, ground);
            let shift = (u2 - ug) * SIZE as f32;
            for k in [shift.x, shift.y] {
                assert!((k - k.round()).abs() < 1e-2, "{sun} : décalage {shift}");
            }
        }
    }
}
