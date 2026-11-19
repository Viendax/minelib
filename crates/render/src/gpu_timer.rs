//! Temps GPU de chaque passe (scène, anticrénelage, affichage + interface, et celles des
//! lumières dynamiques), mesurés par des horodatages GPU et relus quelques images plus tard,
//! sans jamais attendre le GPU.

use std::sync::atomic::{AtomicU32, AtomicU8, Ordering};
use std::sync::Arc;

use crate::perf::GpuTimes;

/// Passes mesurées : 0 scène (opaques), 1 TAA, 2 affichage, 3 éclairage différé (moyenne
/// des voisins et calcul exact), 4 translucides, 5 rayons volumétriques (la marche), 6 cartes
/// d'ombre, 7 recomposition des rayons, 8 éclairage différé d'un pixel sur quatre.
pub const SCENE: u32 = 0;
pub const TAA: u32 = 1;
pub const PRESENT: u32 = 2;
pub const LIGHTS: u32 = 3;
pub const AFTER: u32 = 4;
pub const VOLUMETRIC: u32 = 5;
pub const BAKE: u32 = 6;
pub const VL_COMPOSITE: u32 = 7;
pub const LIGHTS_HALF: u32 = 8;
/// (v199) Lumières de l'éditeur sur l'eau, le verre et la glace (face translucide).
pub const WATER: u32 = 9;
const PASSES: u32 = 10;
/// Chaque passe × (début, fin).
const QUERIES: u32 = PASSES * 2;
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
    /// Passes mesurées dans cette image (bits), puis dans l'image de chaque tampon : une
    /// passe qui n'a pas eu lieu (pas de lumière) ne compte pas.
    marks: AtomicU32,
    slot_marks: Vec<u32>,
    /// Numéro de l'image en cours, celui de l'image de chaque tampon, et la fin (horodatage)
    /// de la dernière image relue : l'écart entre deux images qui se suivent.
    frame: u64,
    slot_frame: Vec<u64>,
    prev_end: Option<(u64, u64)>,
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
            marks: AtomicU32::new(0),
            slot_marks: vec![0; RING],
            frame: 0,
            slot_frame: vec![0; RING],
            prev_end: None,
        })
    }

    /// Début d'image : choisit un tampon libre pour cette image.
    pub fn begin(&mut self) {
        self.marks.store(0, Ordering::Relaxed);
        self.frame += 1;
        let i = self.next;
        self.slot = (self.readback[i].1.load(Ordering::Acquire) == FREE).then_some(i);
        if let Some(i) = self.slot {
            self.slot_frame[i] = self.frame;
            self.next = (self.next + 1) % RING;
        }
    }

    /// Horodatages d'une passe (voir les constantes).
    pub fn pass(&self, index: u32) -> Option<wgpu::RenderPassTimestampWrites<'_>> {
        self.slot?;
        self.marks.fetch_or(1 << index, Ordering::Relaxed);
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
            self.slot_marks[i] = self.marks.swap(0, Ordering::Relaxed);
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
        // Dans l'ordre des images (l'écart entre deux images se calcule de l'une à l'autre).
        let mut order: Vec<usize> = (0..RING).collect();
        order.sort_by_key(|&k| self.slot_frame[k]);
        for k in order {
            let (buf, state) = &self.readback[k];
            if state.load(Ordering::Acquire) != READY {
                continue;
            }
            {
                let data = buf.slice(..).get_mapped_range();
                let t: &[u64] = bytemuck::cast_slice(&data);
                let marks = self.slot_marks[k];
                let ms = |a: u32| {
                    let (b, e) = (t[a as usize * 2], t[a as usize * 2 + 1]);
                    if marks & (1 << a) != 0 && e > b && b != 0 {
                        (e - b) as f32 * self.period_ns / 1e6
                    } else {
                        0.0
                    }
                };
                let (first, last) = (t[SCENE as usize * 2], t[PRESENT as usize * 2 + 1]);
                // Début du travail de l'image : les cartes d'ombre si elles ont été calculées.
                let start = if marks & (1 << BAKE) != 0 && t[BAKE as usize * 2] != 0 {
                    t[BAKE as usize * 2].min(first)
                } else {
                    first
                };
                let frame = self.slot_frame[k];
                let gap = match self.prev_end {
                    Some((f, end)) if f + 1 == frame && start > end && end != 0 => {
                        Some((start - end) as f32 * self.period_ns / 1e6)
                    }
                    _ => None,
                };
                if last > first && first != 0 {
                    self.prev_end = Some((frame, last));
                }
                out = Some(GpuTimes {
                    scene: ms(SCENE),
                    taa: ms(TAA),
                    present: ms(PRESENT),
                    lights: ms(LIGHTS),
                    after: ms(AFTER),
                    volumetric: ms(VOLUMETRIC),
                    vl_composite: ms(VL_COMPOSITE),
                    lights_half: ms(LIGHTS_HALF),
                    water: ms(WATER),
                    bake: ms(BAKE),
                    span: if last > first && first != 0 {
                        (last - first) as f32 * self.period_ns / 1e6
                    } else {
                        0.0
                    },
                    // (Plus d'une demi-seconde : la fenêtre était cachée, pas une attente.)
                    gap: gap.filter(|g| *g < 500.0).unwrap_or(0.0),
                    gap_n: gap.filter(|g| *g < 500.0).map_or(0, |_| 1),
                });
            }
            buf.unmap();
            state.store(FREE, Ordering::Release);
        }
        out
    }
}
