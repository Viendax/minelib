//! Maillages des chunks rangés dans quelques grands tampons GPU (« pages » de 32 Mo de
//! sommets + 16 Mo d'indices) au lieu d'un tampon par chunk : on dessine une page entière
//! en un seul appel (`multi_draw_indexed_indirect`), sans changer de tampon entre les
//! chunks. Le processeur passe beaucoup moins de temps par image à envoyer les dessins.

use minelib_world::mesh::Vertex;

// Grandes pages (128 Mo de sommets + 64 Mo d'indices) : tout le monde visible tient en général
// dans une seule page, donc un seul appel de dessin pour tous les blocs opaques.
const PAGE_VERTICES: u64 = 128 * 1024 * 1024 / std::mem::size_of::<Vertex>() as u64;
const PAGE_INDICES: u64 = 64 * 1024 * 1024 / 4;

/// Plages libres d'un tampon (début, longueur), triées et fusionnées.
#[derive(Debug)]
struct Free {
    ranges: Vec<(u64, u64)>,
}

impl Free {
    fn new(len: u64) -> Self {
        Self {
            ranges: vec![(0, len)],
        }
    }

    /// Première plage assez grande.
    fn alloc(&mut self, len: u64) -> Option<u64> {
        let i = self.ranges.iter().position(|r| r.1 >= len)?;
        let (start, l) = self.ranges[i];
        if l == len {
            self.ranges.remove(i);
        } else {
            self.ranges[i] = (start + len, l - len);
        }
        Some(start)
    }

    fn free(&mut self, start: u64, len: u64) {
        let i = self.ranges.partition_point(|r| r.0 < start);
        self.ranges.insert(i, (start, len));
        // Fusion avec les voisines.
        if i + 1 < self.ranges.len() && self.ranges[i].0 + self.ranges[i].1 == self.ranges[i + 1].0
        {
            self.ranges[i].1 += self.ranges[i + 1].1;
            self.ranges.remove(i + 1);
        }
        if i > 0 && self.ranges[i - 1].0 + self.ranges[i - 1].1 == self.ranges[i].0 {
            self.ranges[i - 1].1 += self.ranges[i].1;
            self.ranges.remove(i);
        }
    }
}

pub struct Page {
    pub vertices: wgpu::Buffer,
    pub indices: wgpu::Buffer,
    vfree: Free,
    ifree: Free,
}

/// Emplacement d'un maillage : page, premier sommet, premier indice.
#[derive(Clone, Copy, Debug)]
pub struct Slot {
    pub page: usize,
    pub vstart: u64,
    pub vlen: u64,
    pub istart: u64,
    pub ilen: u64,
}

#[derive(Default)]
pub struct Arena {
    pub pages: Vec<Page>,
    /// Mémoire vidéo à ne pas dépasser pour les chunks (0 = sans limite).
    pub budget: u64,
    /// Windows signale la mémoire vidéo presque pleine : plus de nouvelle page.
    pub pressure: bool,
    /// Maillages refusés faute de mémoire vidéo.
    pub refused: u64,
}

fn page_bytes() -> u64 {
    PAGE_VERTICES * std::mem::size_of::<Vertex>() as u64 + PAGE_INDICES * 4
}

impl Arena {
    /// Place `vertices` / `indices` dans une page (nouvelle page si aucune n'a la place).
    pub fn alloc(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        vertices: &[Vertex],
        indices: &[u32],
    ) -> Option<Slot> {
        let (vlen, ilen) = (vertices.len() as u64, indices.len() as u64);
        if vlen > PAGE_VERTICES || ilen > PAGE_INDICES || vlen == 0 {
            return None;
        }
        let mut found = None;
        for (p, page) in self.pages.iter_mut().enumerate() {
            if let Some(v) = page.vfree.alloc(vlen) {
                if let Some(i) = page.ifree.alloc(ilen.max(1)) {
                    found = Some((p, v, i));
                    break;
                }
                page.vfree.free(v, vlen);
            }
        }
        let (page, vstart, istart) = match found {
            Some(f) => f,
            None => {
                // Garde-fou : pas de nouvelle page au-delà du budget de mémoire vidéo.
                if self.pressure
                    || (self.budget > 0 && (self.pages.len() as u64 + 1) * page_bytes() > self.budget)
                {
                    self.refused += 1;
                    return None;
                }
                let mk = |label: &str, size: u64, usage| {
                    device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some(label),
                        size,
                        usage,
                        mapped_at_creation: false,
                    })
                };
                let mut p = Page {
                    vertices: mk(
                        "chunks (sommets)",
                        PAGE_VERTICES * std::mem::size_of::<Vertex>() as u64,
                        wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    ),
                    indices: mk(
                        "chunks (indices)",
                        PAGE_INDICES * 4,
                        wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
                    ),
                    vfree: Free::new(PAGE_VERTICES),
                    ifree: Free::new(PAGE_INDICES),
                };
                let v = p.vfree.alloc(vlen)?;
                let i = p.ifree.alloc(ilen.max(1))?;
                self.pages.push(p);
                crate::perf::event(|| {
                    format!(
                        "nouvelle page de mémoire vidéo pour les chunks ({} Mo en tout)",
                        self.pages.len() as u64 * page_bytes() / (1024 * 1024)
                    )
                });
                (self.pages.len() - 1, v, i)
            }
        };
        let pg = &self.pages[page];
        queue.write_buffer(
            &pg.vertices,
            vstart * std::mem::size_of::<Vertex>() as u64,
            bytemuck::cast_slice(vertices),
        );
        if ilen > 0 {
            queue.write_buffer(&pg.indices, istart * 4, bytemuck::cast_slice(indices));
        }
        Some(Slot {
            page,
            vstart,
            vlen,
            istart,
            ilen: ilen.max(1),
        })
    }

    pub fn free(&mut self, s: Slot) {
        if let Some(p) = self.pages.get_mut(s.page) {
            p.vfree.free(s.vstart, s.vlen);
            p.ifree.free(s.istart, s.ilen);
        }
    }

    /// Mémoire GPU réservée par les pages.
    pub fn bytes(&self) -> u64 {
        self.pages.len() as u64 * page_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn free_list_reuses_and_merges_ranges() {
        let mut f = Free::new(100);
        let a = f.alloc(30).unwrap();
        let b = f.alloc(30).unwrap();
        let c = f.alloc(30).unwrap();
        assert_eq!((a, b, c), (0, 30, 60));
        assert!(f.alloc(20).is_none());
        f.free(b, 30);
        f.free(a, 30);
        assert_eq!(f.ranges, vec![(0, 60), (90, 10)]);
        f.free(c, 30);
        assert_eq!(f.ranges, vec![(0, 100)]);
        assert_eq!(f.alloc(100), Some(0));
    }
}
