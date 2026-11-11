use std::collections::HashMap;
use std::sync::Arc;

use crate::flame::{CHEMICALS, WOOD_FLAME};
use crate::gen::{generate, Generator};
use crate::{BlockId, Chunk, ChunkPos, CHUNK_HEIGHT, CHUNK_SIZE};

/// Portée de la lumière d'une torche (blocs). Doit rester < 16 : le maillage d'un
/// chunk ne voit que ses 8 voisins.
pub const LIGHT_RADIUS: i32 = 14;

/// Torche posée : élément chimique éventuel et instant où elle a été allumée.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Torch {
    /// Index dans [`CHEMICALS`], `None` pour une torche ordinaire.
    pub chemical: Option<usize>,
    /// Horloge du monde (s) à l'allumage.
    pub lit_at: f64,
    /// Posée en créatif : la couleur ne s'épuise jamais.
    pub infinite: bool,
}

/// Source de lumière transmise au maillage : position et couleur (intensité 0..255 par canal).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Light {
    pub pos: [i32; 3],
    pub rgb: [u8; 3],
}

/// Ensemble des chunks chargés. Les coordonnées passées ici sont en blocs monde.
pub struct World {
    pub seed: u32,
    chunks: HashMap<ChunkPos, Arc<Chunk>>,
    torches: HashMap<[i32; 3], Torch>,
    /// Blocs modifiés par le joueur, par chunk (index local → bloc). Réappliqués à chaque
    /// (re)génération du chunk et enregistrés dans la sauvegarde.
    edits: HashMap<ChunkPos, HashMap<u32, BlockId>>,
    /// Chunks importés (ex. monde Minecraft converti), compressés : ils remplacent la
    /// génération procédurale à leur emplacement.
    bases: HashMap<ChunkPos, Arc<Vec<u8>>>,
    /// Horloge du monde en secondes (avancée par le jeu).
    pub clock: f64,
    /// Génération des chunks hors zone importée.
    pub generator: Generator,
}

impl World {
    pub fn new(seed: u32) -> Self {
        Self {
            seed,
            chunks: HashMap::new(),
            torches: HashMap::new(),
            edits: HashMap::new(),
            bases: HashMap::new(),
            clock: 0.0,
            generator: Generator::Procedural,
        }
    }

    /// Installe des chunks importés (compressés en RLE). Les chunks invalides sont ignorés.
    pub fn set_bases(&mut self, bases: HashMap<ChunkPos, Vec<u8>>) {
        self.bases.clear();
        for (pos, rle) in bases {
            if Chunk::from_rle(pos, &rle).is_some() {
                self.bases.insert(pos, Arc::new(rle));
            }
        }
        // Les chunks déjà chargés à ces emplacements sont remplacés.
        let loaded: Vec<ChunkPos> = self
            .chunks
            .keys()
            .copied()
            .filter(|p| self.bases.contains_key(p))
            .collect();
        for p in loaded {
            self.chunks.remove(&p);
            self.ensure_chunk(p);
        }
    }

    pub fn bases(&self) -> impl Iterator<Item = (ChunkPos, &[u8])> + '_ {
        self.bases.iter().map(|(p, d)| (*p, d.as_slice()))
    }

    pub fn base_count(&self) -> usize {
        self.bases.len()
    }

    /// Chunk importé à cet emplacement, sinon génération procédurale.
    fn base_or_generate(&self, pos: ChunkPos) -> Chunk {
        self.bases
            .get(&pos)
            .and_then(|d| Chunk::from_rle(pos, d))
            .unwrap_or_else(|| generate(self.generator, self.seed, pos))
    }

    // ------------------------------------------------------------ torches

    pub fn torch(&self, pos: [i32; 3]) -> Option<Torch> {
        self.torches.get(&pos).copied()
    }

    /// Secondes de couleur chimique restantes (`None` : torche ordinaire ou sel épuisé).
    pub fn torch_remaining(&self, t: &Torch) -> Option<f32> {
        let c = CHEMICALS.get(t.chemical?)?;
        if t.infinite {
            return Some(f32::INFINITY);
        }
        let left = c.burn_time_s() as f64 - (self.clock - t.lit_at);
        (left > 0.0).then_some(left as f32)
    }

    /// Couleur actuelle de la flamme (sRGB).
    pub fn torch_color(&self, t: &Torch) -> [u8; 3] {
        match (t.chemical, self.torch_remaining(t)) {
            (Some(i), Some(_)) => CHEMICALS[i].rgb,
            _ => WOOD_FLAME,
        }
    }

    /// Pose une torche (éventuellement chargée d'un sel). Renvoie les chunks à remailler.
    pub fn place_torch(
        &mut self,
        x: i32,
        y: i32,
        z: i32,
        chemical: Option<usize>,
    ) -> Vec<ChunkPos> {
        self.place_torch_block(x, y, z, BlockId::TORCH, chemical)
    }

    /// Pose une torche (au sol ou murale : `block`) avec son sel éventuel.
    pub fn place_torch_block(
        &mut self,
        x: i32,
        y: i32,
        z: i32,
        block: BlockId,
        chemical: Option<usize>,
    ) -> Vec<ChunkPos> {
        let mut dirty = self.set(x, y, z, block);
        if self.get(x, y, z).is_torch() {
            self.torches.insert(
                [x, y, z],
                Torch {
                    chemical,
                    lit_at: self.clock,
                    infinite: false,
                },
            );
            dirty = Self::chunks_in_light_radius(x, z);
        }
        dirty
    }

    /// Torches dont la couleur chimique s'est éteinte entre `t0` et `t1`.
    pub fn torches_expiring(&self, t0: f64, t1: f64) -> Vec<[i32; 3]> {
        self.torches
            .iter()
            .filter(|(_, t)| !t.infinite)
            .filter_map(|(p, t)| {
                let c = CHEMICALS.get(t.chemical?)?;
                let end = t.lit_at + c.burn_time_s() as f64;
                (end > t0 && end <= t1).then_some(*p)
            })
            .collect()
    }

    pub fn torch_count(&self) -> usize {
        self.torches.len()
    }

    /// Sources de lumière pouvant éclairer le chunk `pos` (chunk et voisins).
    pub fn lights_near(&self, pos: ChunkPos) -> Vec<Light> {
        let (ox, oz) = pos.origin();
        let s = CHUNK_SIZE as i32;
        self.torches
            .iter()
            .filter(|(p, _)| {
                p[0] >= ox - LIGHT_RADIUS
                    && p[0] < ox + s + LIGHT_RADIUS
                    && p[2] >= oz - LIGHT_RADIUS
                    && p[2] < oz + s + LIGHT_RADIUS
            })
            .map(|(p, t)| {
                let c = self.torch_color(t);
                // Intensité : le canal le plus fort à 255, la teinte conservée.
                let m = c.iter().copied().max().unwrap_or(1).max(1) as u32;
                Light {
                    pos: *p,
                    rgb: c.map(|v| (v as u32 * 255 / m) as u8),
                }
            })
            .chain(self.block_lights(pos))
            .collect()
    }

    /// Blocs lumineux des chunks voisins pouvant éclairer `pos`.
    fn block_lights(&self, pos: ChunkPos) -> Vec<Light> {
        let (ox, oz) = pos.origin();
        let s = CHUNK_SIZE as i32;
        let mut out = Vec::new();
        for dz in -1..=1 {
            for dx in -1..=1 {
                let c = ChunkPos {
                    x: pos.x + dx,
                    z: pos.z + dz,
                };
                let Some(ch) = self.chunks.get(&c) else {
                    continue;
                };
                let (cx, cz) = c.origin();
                for ([x, y, z], id) in ch.emitters() {
                    let (wx, wz) = (cx + x as i32, cz + z as i32);
                    if wx >= ox - LIGHT_RADIUS
                        && wx < ox + s + LIGHT_RADIUS
                        && wz >= oz - LIGHT_RADIUS
                        && wz < oz + s + LIGHT_RADIUS
                    {
                        if let Some(rgb) = id.def().light {
                            out.push(Light {
                                pos: [wx, y as i32, wz],
                                rgb,
                            });
                        }
                    }
                }
            }
        }
        out
    }

    /// Chunks que la lumière d'une source en (`x`, `z`) peut atteindre.
    pub fn chunks_in_light_radius(x: i32, z: i32) -> Vec<ChunkPos> {
        let r = LIGHT_RADIUS + 1;
        let (c0, c1) = (
            ChunkPos::containing(x - r, z - r),
            ChunkPos::containing(x + r, z + r),
        );
        let mut out = Vec::new();
        for cz in c0.z..=c1.z {
            for cx in c0.x..=c1.x {
                out.push(ChunkPos { x: cx, z: cz });
            }
        }
        out
    }

    fn torch_nearby(&self, x: i32, y: i32, z: i32) -> bool {
        let r = LIGHT_RADIUS + 1;
        self.torches
            .keys()
            .any(|p| (p[0] - x).abs() <= r && (p[1] - y).abs() <= r && (p[2] - z).abs() <= r)
    }

    pub fn chunk(&self, pos: ChunkPos) -> Option<&Chunk> {
        self.chunks.get(&pos).map(|c| &**c)
    }

    /// Référence partagée vers un chunk (envoyable à un thread de maillage).
    pub fn chunk_arc(&self, pos: ChunkPos) -> Option<Arc<Chunk>> {
        self.chunks.get(&pos).cloned()
    }

    /// Ajoute un chunk généré ailleurs (thread de travail). Ignoré s'il existe déjà.
    /// Les modifications du joueur sont réappliquées.
    pub fn insert(&mut self, mut chunk: Chunk) {
        if self.chunks.contains_key(&chunk.pos) {
            return;
        }
        if let Some(c) = self
            .bases
            .get(&chunk.pos)
            .and_then(|d| Chunk::from_rle(chunk.pos, d))
        {
            chunk = c;
        } else if self.generator != Generator::Procedural {
            chunk = generate(self.generator, self.seed, chunk.pos);
        }
        self.apply_edits(&mut chunk);
        self.chunks.insert(chunk.pos, Arc::new(chunk));
    }

    fn apply_edits(&self, chunk: &mut Chunk) {
        if let Some(e) = self.edits.get(&chunk.pos) {
            for (&i, &id) in e {
                let (x, z, y) = (i % 16, (i / 16) % 16, i / 256);
                chunk.set(x as usize, y as usize, z as usize, id);
            }
        }
    }

    /// Modifications du joueur : (chunk, index local = x + 16 z + 256 y, bloc).
    pub fn edits(&self) -> impl Iterator<Item = (ChunkPos, u32, BlockId)> + '_ {
        self.edits
            .iter()
            .flat_map(|(c, m)| m.iter().map(move |(i, b)| (*c, *i, *b)))
    }

    /// Recharge des modifications (sauvegarde) avant que les chunks soient générés.
    pub fn load_edit(&mut self, chunk: ChunkPos, index: u32, id: BlockId) {
        self.edits.entry(chunk).or_default().insert(index, id);
    }

    pub fn torches(&self) -> impl Iterator<Item = ([i32; 3], Torch)> + '_ {
        self.torches.iter().map(|(p, t)| (*p, *t))
    }

    /// Rend la couleur de la torche en `pos` inépuisable (torches posées en créatif).
    pub fn set_torch_infinite(&mut self, pos: [i32; 3]) {
        if let Some(t) = self.torches.get_mut(&pos) {
            t.infinite = true;
        }
    }

    pub fn load_torch(&mut self, pos: [i32; 3], t: Torch) {
        self.torches.insert(pos, t);
    }

    pub fn contains(&self, pos: ChunkPos) -> bool {
        self.chunks.contains_key(&pos)
    }

    /// Génère le chunk s'il n'est pas déjà chargé.
    pub fn ensure_chunk(&mut self, pos: ChunkPos) {
        if !self.chunks.contains_key(&pos) {
            let mut c = self.base_or_generate(pos);
            self.apply_edits(&mut c);
            self.chunks.insert(pos, Arc::new(c));
        }
    }

    /// Insère un chunk vide (tests, mondes construits à la main).
    pub fn insert_empty(&mut self, pos: ChunkPos) {
        self.chunks.insert(pos, Arc::new(Chunk::new(pos)));
    }

    /// Décharge un chunk. Ses modifications et ses torches sont gardées.
    /// Retire un chunk et rend ses données (libérées par l'appelant, éventuellement ailleurs).
    pub fn unload(&mut self, pos: ChunkPos) -> Option<Arc<Chunk>> {
        self.chunks.remove(&pos)
    }

    pub fn loaded(&self) -> impl Iterator<Item = ChunkPos> + '_ {
        self.chunks.keys().copied()
    }

    pub fn len(&self) -> usize {
        self.chunks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.chunks.is_empty()
    }

    /// Bloc en coordonnées monde. Hors du monde chargé : air (et socle sous y = 0).
    #[inline]
    pub fn get(&self, x: i32, y: i32, z: i32) -> BlockId {
        if y < 0 {
            return BlockId::BEDROCK;
        }
        if y >= CHUNK_HEIGHT as i32 {
            return BlockId::AIR;
        }
        let pos = ChunkPos::containing(x, z);
        match self.chunks.get(&pos) {
            Some(c) => c.get(
                x.rem_euclid(CHUNK_SIZE as i32) as usize,
                y as usize,
                z.rem_euclid(CHUNK_SIZE as i32) as usize,
            ),
            None => BlockId::AIR,
        }
    }

    /// Modifie un bloc et renvoie les chunks dont le maillage doit être refait
    /// (le chunk lui-même et ses voisins si le bloc est sur un bord).
    pub fn set(&mut self, x: i32, y: i32, z: i32, id: BlockId) -> Vec<ChunkPos> {
        if !(0..CHUNK_HEIGHT as i32).contains(&y) {
            return Vec::new();
        }
        let pos = ChunkPos::containing(x, z);
        let Some(chunk) = self.chunks.get_mut(&pos) else {
            return Vec::new();
        };
        // Copie à l'écriture si un thread de maillage tient encore l'ancienne version.
        let chunk = Arc::make_mut(chunk);
        let (lx, lz) = (
            x.rem_euclid(CHUNK_SIZE as i32),
            z.rem_euclid(CHUNK_SIZE as i32),
        );
        let old = chunk.get(lx as usize, y as usize, lz as usize);
        chunk.set(lx as usize, y as usize, lz as usize, id);
        let index = lx as u32 + 16 * lz as u32 + 256 * y as u32;
        self.edits.entry(pos).or_default().insert(index, id);
        if old.is_torch() && !id.is_torch() {
            self.torches.remove(&[x, y, z]);
        }
        // Une source de lumière à portée : la lumière change sur toute sa zone d'influence.
        if old.is_torch()
            || old.def().light.is_some()
            || id.def().light.is_some()
            || self.torch_nearby(x, y, z)
        {
            return Self::chunks_in_light_radius(x, z);
        }

        let mut dirty = vec![pos];
        let last = CHUNK_SIZE as i32 - 1;
        if lx == 0 {
            dirty.push(ChunkPos {
                x: pos.x - 1,
                z: pos.z,
            });
        }
        if lx == last {
            dirty.push(ChunkPos {
                x: pos.x + 1,
                z: pos.z,
            });
        }
        if lz == 0 {
            dirty.push(ChunkPos {
                x: pos.x,
                z: pos.z - 1,
            });
        }
        if lz == last {
            dirty.push(ChunkPos {
                x: pos.x,
                z: pos.z + 1,
            });
        }
        // Les coins influencent l'occlusion ambiante du chunk diagonal.
        if (lx == 0 || lx == last) && (lz == 0 || lz == last) {
            let dx = if lx == 0 { -1 } else { 1 };
            let dz = if lz == 0 { -1 } else { 1 };
            dirty.push(ChunkPos {
                x: pos.x + dx,
                z: pos.z + dz,
            });
        }
        dirty
    }

    /// y du plus haut bloc solide de la colonne, s'il y en a un.
    pub fn top_solid(&self, x: i32, z: i32) -> Option<i32> {
        (0..CHUNK_HEIGHT as i32)
            .rev()
            .find(|&y| self.get(x, y, z).is_solid())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_reports_neighbors_on_edges() {
        let mut w = World::new(0);
        w.ensure_chunk(ChunkPos { x: 0, z: 0 });
        assert_eq!(
            w.set(5, 200, 5, BlockId::STONE),
            vec![ChunkPos { x: 0, z: 0 }]
        );
        assert_eq!(w.get(5, 200, 5), BlockId::STONE);
        let d = w.set(0, 200, 15, BlockId::STONE);
        assert_eq!(d.len(), 4);
        assert!(d.contains(&ChunkPos { x: -1, z: 1 }));
    }

    #[test]
    fn torches_track_color_and_expire() {
        let mut w = World::new(0);
        for dz in -1..=1 {
            for dx in -1..=1 {
                w.insert_empty(ChunkPos { x: dx, z: dz });
            }
        }
        let li = crate::flame::CHEMICALS
            .iter()
            .position(|c| c.symbol == "Li")
            .unwrap();
        let dirty = w.place_torch(8, 100, 8, Some(li));
        assert_eq!(dirty.len(), 9);
        let t = w.torch([8, 100, 8]).unwrap();
        assert_eq!(w.torch_color(&t), crate::flame::CHEMICALS[li].rgb);
        let d = crate::flame::CHEMICALS[li].burn_time_s() as f64;
        assert_eq!(w.torches_expiring(0.0, d + 1.0), vec![[8, 100, 8]]);
        w.clock = d + 1.0;
        assert_eq!(w.torch_color(&t), crate::flame::WOOD_FLAME);
        assert_eq!(w.lights_near(ChunkPos { x: 0, z: 0 }).len(), 1);
        assert!(w.lights_near(ChunkPos { x: 3, z: 0 }).is_empty());
        // Casser la torche l'oublie.
        w.set(8, 100, 8, BlockId::AIR);
        assert!(w.torch([8, 100, 8]).is_none());
    }

    #[test]
    fn creative_torches_never_run_out() {
        let mut w = World::new(0);
        w.insert_empty(ChunkPos { x: 0, z: 0 });
        w.place_torch(8, 100, 8, Some(0));
        w.set_torch_infinite([8, 100, 8]);
        let t = w.torch([8, 100, 8]).unwrap();
        assert_eq!(w.torch_remaining(&t), Some(f32::INFINITY));
        assert!(w.torches_expiring(0.0, 1e9).is_empty());
        w.clock = 1e9;
        assert_eq!(w.torch_color(&t), crate::flame::CHEMICALS[0].rgb);
    }

    #[test]
    fn edits_survive_unload_and_regeneration() {
        let mut w = World::new(3);
        let pos = ChunkPos { x: -1, z: 2 };
        w.ensure_chunk(pos);
        w.set(-5, 120, 40, BlockId::BRICK);
        w.unload(pos);
        assert_eq!(w.get(-5, 120, 40), BlockId::AIR);
        w.insert(crate::gen::generate_chunk(3, pos));
        assert_eq!(w.get(-5, 120, 40), BlockId::BRICK);
        let e: Vec<_> = w.edits().collect();
        assert_eq!(e.len(), 1);
        let mut w2 = World::new(3);
        w2.load_edit(e[0].0, e[0].1, e[0].2);
        w2.ensure_chunk(pos);
        assert_eq!(w2.get(-5, 120, 40), BlockId::BRICK);
    }

    #[test]
    fn negative_coordinates() {
        let mut w = World::new(0);
        w.ensure_chunk(ChunkPos { x: -1, z: -1 });
        w.set(-1, 150, -1, BlockId::BRICK);
        assert_eq!(w.get(-1, 150, -1), BlockId::BRICK);
        assert_eq!(
            w.chunk(ChunkPos { x: -1, z: -1 }).unwrap().get(15, 150, 15),
            BlockId::BRICK
        );
    }
}
