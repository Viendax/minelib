//! Temps GPU de chaque passe (scène, anticrénelage, affichage + interface), mesurés par
//! des horodatages GPU et relus quelques images plus tard, sans jamais attendre le GPU.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

use crate::perf::GpuTimes;

/// 3 passes × (début, fin).
const QUERIES: u32 = 6;
/// Tampons de relecture en rotation (le GPU a quelques images d'avance).
const RING: usize = 4;

const FREE: u8 = 0;
const MAPPING: u8 = 1;
const READY: u8 = 2;

pub struct GpuTimer {
    set: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    readback: Vec<(wgpu::Buffer, Arc<AtomicU8>)>,
    next: usize,
    period_ns: f32,
    /// Écrit ce frame-ci dans `readback[slot]` (None : tous occupés, pas de mesure).
    slot: Option<usize>,
}

impl GpuTimer {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Option<Self> {
        if !device.features().contains(wgpu::Features::TIMESTAMP_QUERY) {
            return None;
        }
        let set = device.create_query_set(&wgpu::QuerySetDescriptor {
            label: Some("temps GPU"),
            ty: wgpu::QueryType::Timestamp,
            count: QUERIES,
        });
        let size = QUERIES as u64 * 8;
        let resolve = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("temps GPU"),
            size,
            usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let readback = (0..RING)
            .map(|_| {
                let b = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("temps GPU (lecture)"),
                    size,
                    usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                (b, Arc::new(AtomicU8::new(FREE)))
            })
            .collect();
        Some(Self {
            set,
            resolve,
            readback,
            next: 0,
            period_ns: queue.get_timestamp_period(),
            slot: None,
        })
    }

    /// Début d'image : choisit un tampon libre pour cette image.
    pub fn begin(&mut self) {
        let i = self.next;
        self.slot = (self.readback[i].1.load(Ordering::Acquire) == FREE).then_some(i);
        if self.slot.is_some() {
            self.next = (self.next + 1) % RING;
        }
    }

    /// Horodatages d'une passe (0 scène, 1 TAA, 2 affichage).
    pub fn pass(&self, index: u32) -> Option<wgpu::RenderPassTimestampWrites<'_>> {
        self.slot?;
        Some(wgpu::RenderPassTimestampWrites {
            query_set: &self.set,
            beginning_of_pass_write_index: Some(index * 2),
            end_of_pass_write_index: Some(index * 2 + 1),
        })
    }

    /// Après les passes, avant `submit`.
    pub fn resolve(&self, encoder: &mut wgpu::CommandEncoder) {
        let Some(i) = self.slot else { return };
        encoder.resolve_query_set(&self.set, 0..QUERIES, &self.resolve, 0);
        encoder.copy_buffer_to_buffer(&self.resolve, 0, &self.readback[i].0, 0, QUERIES as u64 * 8);
    }

    /// Après `submit` : demande la relecture, et récupère les mesures déjà prêtes.
    pub fn after_submit(&mut self, device: &wgpu::Device) -> Option<GpuTimes> {
        if let Some(i) = self.slot.take() {
            let state = self.readback[i].1.clone();
            state.store(MAPPING, Ordering::Release);
            self.readback[i]
                .0
                .slice(..)
                .map_async(wgpu::MapMode::Read, move |r| {
                    state.store(if r.is_ok() { READY } else { FREE }, Ordering::Release);
                });
        }
        let _ = device.poll(wgpu::PollType::Poll);
        let mut out = None;
        for (buf, state) in &self.readback {
            if state.load(Ordering::Acquire) != READY {
                continue;
            }
            {
                let data = buf.slice(..).get_mapped_range();
                let t: &[u64] = bytemuck::cast_slice(&data);
                let ms = |a: usize| {
                    let (b, e) = (t[a * 2], t[a * 2 + 1]);
                    if e > b && b != 0 {
                        (e - b) as f32 * self.period_ns / 1e6
                    } else {
                        0.0
                    }
                };
                out = Some(GpuTimes {
                    scene: ms(0),
                    taa: ms(1),
                    present: ms(2),
                    span: if t[5] > t[0] && t[0] != 0 {
                        (t[5] - t[0]) as f32 * self.period_ns / 1e6
                    } else {
                        0.0
                    },
                });
            }
            buf.unmap();
            state.store(FREE, Ordering::Release);
        }
        out
    }
}
