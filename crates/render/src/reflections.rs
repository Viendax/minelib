//! Reflets calculés à leur propre rythme (raytracing désynchronisé du rendu).
//!
//! Une passe à part redessine seulement les surfaces réfléchissantes visibles et y lance les
//! rayons (`fs_reflect`) dans une image de reflets ; elle ne tourne que quelques centaines
//! de fois par seconde au plus. Entre deux, chaque surface relit son reflet dans la dernière
//! image en se retrouvant par sa position dans le monde (`cached_reflection`) : le rendu
//! principal garde ainsi un nombre d'images par seconde élevé et constant.
//! Deux images alternent : la passe écrit l'une pendant que l'autre est lue.

use std::time::{Duration, Instant};

use crate::DEPTH_FORMAT;

pub const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

pub struct Reflections {
    pub size: (u32, u32),
    pub views: [wgpu::TextureView; 2],
    pub depth: wgpu::TextureView,
    /// Image à lire (la dernière écrite).
    pub latest: usize,
    /// L'image `latest` contient des reflets valables.
    pub valid: bool,
    /// Vue-projection et caméra du dernier calcul.
    pub view_proj: [[f32; 4]; 4],
    pub cam: [f32; 3],
    /// Fréquence voulue (calculs par seconde).
    pub rate: f32,
    last: Option<Instant>,
    // Compteur affiché (F3).
    count: u32,
    count_start: Instant,
    pub fps: f32,
}

impl Reflections {
    pub fn new(device: &wgpu::Device, size: (u32, u32)) -> Self {
        let size = (size.0.max(1), size.1.max(1));
        let tex = |label: &str, format, usage| {
            device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d {
                        width: size.0,
                        height: size.1,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage,
                    view_formats: &[],
                })
                .create_view(&Default::default())
        };
        let rt = wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING;
        Self {
            size,
            views: [tex("reflets 0", FORMAT, rt), tex("reflets 1", FORMAT, rt)],
            depth: tex(
                "reflets (profondeur)",
                DEPTH_FORMAT,
                wgpu::TextureUsages::RENDER_ATTACHMENT,
            ),
            latest: 0,
            valid: false,
            view_proj: [[0.0; 4]; 4],
            cam: [0.0; 3],
            rate: 144.0,
            last: None,
            count: 0,
            count_start: Instant::now(),
            fps: 0.0,
        }
    }

    /// Faut-il recalculer les reflets à cette image ?
    pub fn due(&self) -> bool {
        self.last
            .is_none_or(|t| t.elapsed() >= Duration::from_secs_f32(1.0 / self.rate.max(1.0)))
    }

    /// Un calcul vient d'être lancé.
    pub fn ran(&mut self, view_proj: [[f32; 4]; 4], cam: [f32; 3]) {
        let now = Instant::now();
        // Rythme régulier : on vise l'échéance suivante, sans dériver.
        self.last = Some(match self.last {
            Some(t)
                if now.duration_since(t) < Duration::from_secs_f32(2.0 / self.rate.max(1.0)) =>
            {
                t + Duration::from_secs_f32(1.0 / self.rate.max(1.0))
            }
            _ => now,
        });
        self.view_proj = view_proj;
        self.cam = cam;
        self.latest = 1 - self.latest;
        self.valid = true;
        self.count += 1;
    }

    /// À appeler à chaque image : met à jour le compteur affiché.
    pub fn tick(&mut self) {
        let e = self.count_start.elapsed().as_secs_f32();
        if e >= 0.5 {
            self.fps = self.count as f32 / e;
            self.count = 0;
            self.count_start = Instant::now();
        }
    }

    pub fn globals(&self) -> ([[f32; 4]; 4], [f32; 4]) {
        (
            self.view_proj,
            [
                self.cam[0],
                self.cam[1],
                self.cam[2],
                if self.valid { 1.0 } else { 0.0 },
            ],
        )
    }
}
