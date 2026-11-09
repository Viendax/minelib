//! Captures d'écran (F2) : l'image affichée est copiée dans un tampon lisible juste avant
//! d'être présentée, puis relue quelques images plus tard sans jamais attendre la carte.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

/// Image capturée, en RGBA 8 bits (sRGB), ligne par ligne.
pub struct Screenshot {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

const WAITING: u8 = 0;
const READY: u8 = 1;
const FAILED: u8 = 2;

struct Pending {
    buffer: wgpu::Buffer,
    width: u32,
    height: u32,
    padded_row: u32,
    bgra: bool,
    state: Arc<AtomicU8>,
    mapping: bool,
}

#[derive(Default)]
pub struct Capture {
    requested: bool,
    pending: Option<Pending>,
}

impl Capture {
    pub fn request(&mut self) {
        self.requested = true;
    }

    /// Avant `submit` : copie de l'image de la fenêtre si une capture est demandée.
    pub fn encode(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        texture: &wgpu::Texture,
    ) {
        if !self.requested || self.pending.is_some() {
            return;
        }
        if !texture.usage().contains(wgpu::TextureUsages::COPY_SRC) {
            log::warn!("capture d'écran impossible : l'image de la fenêtre n'est pas lisible");
            self.requested = false;
            return;
        }
        self.requested = false;
        let (width, height) = (texture.width(), texture.height());
        let padded_row = (width * 4).div_ceil(256) * 256;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("capture d'écran"),
            size: padded_row as u64 * height as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_row),
                    rows_per_image: Some(height),
                },
            },
            texture.size(),
        );
        let bgra = matches!(
            texture.format(),
            wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb
        );
        self.pending = Some(Pending {
            buffer,
            width,
            height,
            padded_row,
            bgra,
            state: Arc::new(AtomicU8::new(WAITING)),
            mapping: false,
        });
    }

    /// Après `submit` : lance la relecture, et rend l'image quand elle est prête.
    pub fn poll(&mut self) -> Option<Screenshot> {
        let p = self.pending.as_mut()?;
        if !p.mapping {
            p.mapping = true;
            let state = p.state.clone();
            p.buffer.slice(..).map_async(wgpu::MapMode::Read, move |r| {
                state.store(if r.is_ok() { READY } else { FAILED }, Ordering::Release);
            });
            return None;
        }
        match p.state.load(Ordering::Acquire) {
            WAITING => None,
            FAILED => {
                log::warn!("capture d'écran : relecture impossible");
                self.pending = None;
                None
            }
            _ => {
                let p = self.pending.take()?;
                let mut rgba = Vec::with_capacity((p.width * p.height * 4) as usize);
                {
                    let data = p.buffer.slice(..).get_mapped_range();
                    for row in 0..p.height as usize {
                        let start = row * p.padded_row as usize;
                        let line = &data[start..start + p.width as usize * 4];
                        if p.bgra {
                            for px in line.chunks_exact(4) {
                                rgba.extend_from_slice(&[px[2], px[1], px[0], 255]);
                            }
                        } else {
                            for px in line.chunks_exact(4) {
                                rgba.extend_from_slice(&[px[0], px[1], px[2], 255]);
                            }
                        }
                    }
                }
                p.buffer.unmap();
                Some(Screenshot {
                    width: p.width,
                    height: p.height,
                    rgba,
                })
            }
        }
    }
}
