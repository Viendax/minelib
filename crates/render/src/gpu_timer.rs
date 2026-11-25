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
/// (v200) Image des joueurs et des objets reflétés dans l'eau.
pub const WATER_MIRROR: u32 = 10;
const PASSES: u32 = 11;
/// (v316) Début et fin de toutes les commandes de l'image, passes sans mesure comprises
/// (`frame_start`, `resolve`) : seulement si la carte sait horodater hors d'une passe.
const WHOLE_BEGIN: u32 = PASSES * 2;
const WHOLE_END: u32 = PASSES * 2 + 1;
/// Chaque passe × (début, fin), puis les deux horodatages de l'image entière.
/// (v321) Horodatage écrit par un petit envoi à part juste après la présentation de l'image
/// d'avant (`open`) : il coupe « entre deux images » en deux, voir `after_submit`.
const OPENED: u32 = PASSES * 2 + 2;
const QUERIES: u32 = PASSES * 2 + 3;
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
    /// (v321) Fin de toutes les commandes de l'image d'avant (`WHOLE_END`) : son numéro et
    /// son horodatage.
    prev_whole_end: Option<(u64, u64)>,
    /// (v321) `begin` déjà fait par `open` pour l'image qui vient.
    opened: bool,
    /// (v316) La carte sait horodater entre deux passes (`WHOLE_BEGIN`, `WHOLE_END`).
    inside: bool,
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
            prev_whole_end: None,
            opened: false,
            inside: device
                .features()
                .contains(wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS),
        })
    }

    /// Début d'image : choisit un tampon libre pour cette image.
    /// Avant les commandes de l'image, si `open` ne l'a pas déjà fait.
    pub fn begin(&mut self) {
        if std::mem::take(&mut self.opened) {
            return;
        }
        self.start();
    }

    /// (v321) Juste après la présentation d'une image : réserve déjà la place de la suivante
    /// et y écrit l'horodatage `OPENED` dans un petit envoi à part. Sur la carte, entre la
    /// fin de l'image d'avant et le début de la suivante, on sait alors ce qui revient à la
    /// présentation (et à ce que Windows ou d'autres programmes font tourner sur la carte à
    /// ce moment), et ce qui revient aux données envoyées pour l'image (maillages, lumières,
    /// joueurs…), copiées avant ses commandes.
    pub fn open(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        self.start();
        self.opened = true;
        if self.inside && self.slot.is_some() {
            let mut e = device.create_command_encoder(&Default::default());
            e.write_timestamp(&self.set, OPENED);
            queue.submit([e.finish()]);
        }
    }

    fn start(&mut self) {
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

    /// (v316) Juste après la création des commandes de l'image, avant sa première passe :
    /// l'horodatage du début de tout son travail sur la carte (passes sans mesure comprises).
    pub fn frame_start(&self, encoder: &mut wgpu::CommandEncoder) {
        if self.inside && self.slot.is_some() {
            encoder.write_timestamp(&self.set, WHOLE_BEGIN);
        }
    }

    /// Après les passes, avant `submit`.
    pub fn resolve(&self, encoder: &mut wgpu::CommandEncoder) {
        let Some(i) = self.slot else { return };
        if self.inside {
            encoder.write_timestamp(&self.set, WHOLE_END);
        }
        encoder.resolve_query_set(&self.set, 0..QUERIES, &self.resolve, 0);
        encoder.copy_buffer_to_buffer(&self.resolve, 0, &self.readback[i].0, 0, QUERIES as u64 * 8);
    }

    /// Après `submit` : demande la relecture, et récupère les mesures déjà prêtes, dans
    /// l'ordre des images.
    ///
    /// (v316) **Toutes** les images prêtes, et plus seulement la dernière : quand la carte en
    /// finissait deux entre deux relectures (elle les dessinait par paires, voir
    /// `swap_latency`), la première était jetée avec son écart, toujours le plus long des
    /// deux. « Entre deux images » ne montrait alors que l'écart court, et la somme « du
    /// début à la fin + entre deux images » ne faisait plus la durée d'une image.
    pub fn after_submit(&mut self, device: &wgpu::Device) -> [Option<GpuTimes>; RING] {
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
        let mut out = [None; RING];
        let mut n = 0;
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
                let last = t[PRESENT as usize * 2 + 1];
                // Début du travail de l'image : sa première passe mesurée (les cartes d'ombre
                // si elles ont été calculées, (v203) et les reflets dans l'eau, avant la scène).
                // Avant, « du début à la fin » partait de la scène, et ce qui la précède était
                // compté « entre deux images » : il ne reste là que l'inoccupé et ce qui n'est
                // pas mesuré.
                let first = (0..PASSES)
                    .filter(|&a| marks & (1 << a) != 0 && t[a as usize * 2] != 0)
                    .map(|a| t[a as usize * 2])
                    .min()
                    .unwrap_or(0);
                let frame = self.slot_frame[k];
                let gap = match self.prev_end {
                    Some((f, end)) if f + 1 == frame && first > end && end != 0 => {
                        Some((first - end) as f32 * self.period_ns / 1e6)
                    }
                    _ => None,
                };
                if last > first && first != 0 {
                    self.prev_end = Some((frame, last));
                }
                // (v316) Toutes les commandes de l'image, passes sans mesure comprises.
                let (wb, we) = (t[WHOLE_BEGIN as usize], t[WHOLE_END as usize]);
                let whole = (self.inside && wb != 0 && we > wb)
                    .then(|| (we - wb) as f32 * self.period_ns / 1e6);
                // (v321) « Entre deux images » coupé en deux par `OPENED` : de la fin de
                // l'image d'avant à l'horodatage écrit après sa présentation, puis de là au
                // début des commandes de celle-ci. Écarté au-delà de 50 ms (menu, fenêtre
                // réduite : l'horodatage date alors d'avant la pause).
                let ms_between = |a: u64, b: u64| {
                    (a != 0 && b > a)
                        .then(|| (b - a) as f32 * self.period_ns / 1e6)
                        .filter(|m| *m < 50.0)
                };
                let op = t[OPENED as usize];
                let upload = if self.inside { ms_between(op, wb) } else { None };
                let presenting = match self.prev_whole_end {
                    Some((f, end)) if self.inside && f + 1 == frame => ms_between(end, op),
                    _ => None,
                };
                if self.inside && we > wb && wb != 0 {
                    self.prev_whole_end = Some((frame, we));
                }
                out[n] = Some(GpuTimes {
                    scene: ms(SCENE),
                    taa: ms(TAA),
                    present: ms(PRESENT),
                    lights: ms(LIGHTS),
                    after: ms(AFTER),
                    volumetric: ms(VOLUMETRIC),
                    vl_composite: ms(VL_COMPOSITE),
                    lights_half: ms(LIGHTS_HALF),
                    water: ms(WATER),
                    water_mirror: ms(WATER_MIRROR),
                    bake: ms(BAKE),
                    span: if last > first && first != 0 {
                        (last - first) as f32 * self.period_ns / 1e6
                    } else {
                        0.0
                    },
                    // (Plus d'une demi-seconde : la fenêtre était cachée, pas une attente.)
                    gap: gap.filter(|g| *g < 500.0).unwrap_or(0.0),
                    gap_n: gap.filter(|g| *g < 500.0).map_or(0, |_| 1),
                    whole: whole.unwrap_or(0.0),
                    whole_n: whole.map_or(0, |_| 1),
                    upload: upload.unwrap_or(0.0),
                    upload_n: upload.map_or(0, |_| 1),
                    presenting: presenting.unwrap_or(0.0),
                    presenting_n: presenting.map_or(0, |_| 1),
                });
                n += 1;
            }
            buf.unmap();
            state.store(FREE, Ordering::Release);
        }
        out
    }
}
