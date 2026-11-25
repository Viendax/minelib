use std::collections::HashMap;
use std::sync::Arc;

use crate::flame::{CHEMICALS, WOOD_FLAME};
use crate::gen::{generate, Generator};
use crate::lap::{wrap_block, wrap_chunk};
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
///
/// (v309) **Coordonnées locales.** Loin du centre du monde, un f32 ne vaut plus qu'au bloc
/// près (16 777 216 = 2²⁴) : la partie compte tout depuis une **origine** proche du joueur
/// (`origin`, en chunks, multiple de 256 chunks ; (0, 0) près du centre, en mode FPS et en
/// ligne). Les coordonnées que le jeu passe ici, celles des chunks chargés, de leurs
/// maillages, du joueur et de tout le reste de la partie sont **locales** (monde − origine).
/// Ce qui est enregistré (modifications, torches, chunks importés : `edits`, `load_edit`,
/// `torches_abs`, `load_torch`, `bases`, `set_bases`) et la génération restent en
/// coordonnées **absolues**. Origine (0, 0) : local = absolu, rien ne change.
pub struct World {
    pub seed: u32,
    /// (v309) Origine des coordonnées locales, en chunks (voir plus haut).
    origin: ChunkPos,
    /// Chunks chargés, à leur place locale.
    chunks: HashMap<ChunkPos, Arc<Chunk>>,
    /// Torches posées, à leur place absolue.
    torches: HashMap<[i32; 3], Torch>,
    /// Blocs modifiés par le joueur, par chunk absolu (index local → bloc). Réappliqués à
    /// chaque (re)génération du chunk et enregistrés dans la sauvegarde.
    edits: HashMap<ChunkPos, HashMap<u32, BlockId>>,
    /// Chunks importés (ex. monde Minecraft converti), compressés, à leur place absolue : ils
    /// remplacent la génération procédurale à leur emplacement.
    bases: HashMap<ChunkPos, Arc<Vec<u8>>>,
    /// (v209) Numéro de l'ensemble `bases` actuel (un nouveau à chaque `set_bases`, unique
    /// dans le programme) : la sauvegarde ne réécrit `chunks.bin` que s'il a changé.
    bases_version: u64,
    /// Horloge du monde en secondes (avancée par le jeu).
    pub clock: f64,
    /// Génération des chunks hors zone importée.
    pub generator: Generator,
    /// Journal des blocs posés par `set` (activé pour le jeu en ligne : ce que le joueur
    /// change est envoyé aux autres).
    /// Blocs changés par le joueur local : (position, nouveau bloc, bloc d'avant).
    pub journal: Option<Vec<([i32; 3], BlockId, BlockId)>>,
    /// Cases changées ici (pas celles venues d'autres joueurs), pour l'écoulement de l'eau
    /// et de la lave ; `None` : pas relevées.
    pub touched: Option<Vec<[i32; 3]>>,
    /// Points de mesure de la lumière par chunk (voir `set_probes`).
    probes: HashMap<ChunkPos, Arc<Vec<[i32; 3]>>>,
    /// (v185) Chunks arrivés (chargés ou générés) depuis le dernier relevé ; `None` : pas
    /// relevés. En solo, l'eau et la lave changées par le joueur s'y remettent à couler :
    /// une partie enregistrée pendant que l'eau séchait ne la garde plus pour toujours.
    pub fresh: Option<Vec<ChunkPos>>,
    /// (v283) Piles de tapis de plusieurs couleurs, par chunk : index dans le chunk (celui des
    /// modifications) → sortes de leurs couches, du dessous au dessus (`CARPET_KINDS`). Une
    /// pile d'une seule couleur n'en a pas : son bloc dit tout (hauteur et sorte du dessus).
    /// Gardées avec le monde (`tapis.txt`), comme les modifications.
    carpets: HashMap<ChunkPos, Arc<HashMap<u32, Vec<u8>>>>,
}

impl World {
    pub fn new(seed: u32) -> Self {
        Self {
            seed,
            origin: ChunkPos { x: 0, z: 0 },
            chunks: HashMap::new(),
            torches: HashMap::new(),
            edits: HashMap::new(),
            bases: HashMap::new(),
            bases_version: 0,
            clock: 0.0,
            generator: Generator::Procedural,
            journal: None,
            touched: None,
            probes: HashMap::new(),
            fresh: None,
            carpets: HashMap::new(),
        }
    }

    /// (v309) Origine des coordonnées locales, en chunks (voir `World`).
    pub fn origin(&self) -> ChunkPos {
        self.origin
    }

    /// (v309) Origine des coordonnées locales, en blocs (y = 0).
    pub fn origin_blocks(&self) -> [i32; 3] {
        let (x, z) = self.origin.origin();
        [x, 0, z]
    }

    /// (v309) Chunk absolu du chunk local `p`. (v309) Sur la carte bouclée (`lap`).
    pub fn abs_chunk(&self, p: ChunkPos) -> ChunkPos {
        ChunkPos {
            x: wrap_chunk(p.x as i64 + self.origin.x as i64),
            z: wrap_chunk(p.z as i64 + self.origin.z as i64),
        }
    }

    /// (v309) Chunk local du chunk absolu `p`. (v309) Le plus proche de l'origine, sur la
    /// carte bouclée : de l'autre côté de la limite, un chunk voisin reste voisin.
    pub fn local_chunk(&self, p: ChunkPos) -> ChunkPos {
        ChunkPos {
            x: wrap_chunk(p.x as i64 - self.origin.x as i64),
            z: wrap_chunk(p.z as i64 - self.origin.z as i64),
        }
    }

    /// (v309) Place absolue du bloc local `p`. (v309) Sur la carte bouclée.
    pub fn abs_block(&self, p: [i32; 3]) -> [i32; 3] {
        let o = self.origin_blocks();
        [
            wrap_block(p[0] as i64 + o[0] as i64),
            p[1],
            wrap_block(p[2] as i64 + o[2] as i64),
        ]
    }

    /// (v309) Place locale du bloc absolu `p`. (v309) La plus proche de l'origine, sur la
    /// carte bouclée.
    pub fn local_block(&self, p: [i32; 3]) -> [i32; 3] {
        let o = self.origin_blocks();
        [
            wrap_block(p[0] as i64 - o[0] as i64),
            p[1],
            wrap_block(p[2] as i64 - o[2] as i64),
        ]
    }

    /// (v309) Change l'origine des coordonnées locales (voir `World`). Les chunks chargés
    /// (et ce qui est relevé en coordonnées locales) passent à leur nouvelle place locale ;
    /// ce qui est enregistré est absolu, rien n'y change. Le reste de la partie compte depuis
    /// l'origine : à faire avant d'y charger quoi que ce soit.
    pub fn set_origin(&mut self, origin: ChunkPos) {
        // (v309) Le décalage le plus court sur la carte bouclée.
        let (dx, dz) = (
            wrap_chunk(self.origin.x as i64 - origin.x as i64),
            wrap_chunk(self.origin.z as i64 - origin.z as i64),
        );
        self.origin = origin;
        if dx == 0 && dz == 0 {
            return;
        }
        let chunk = |p: ChunkPos| ChunkPos {
            x: p.x + dx,
            z: p.z + dz,
        };
        let block = |p: [i32; 3]| [p[0] + dx * CHUNK_SIZE as i32, p[1], p[2] + dz * CHUNK_SIZE as i32];
        self.chunks = std::mem::take(&mut self.chunks)
            .into_iter()
            .map(|(p, mut c)| {
                let q = chunk(p);
                Arc::make_mut(&mut c).pos = q;
                (q, c)
            })
            .collect();
        self.probes = std::mem::take(&mut self.probes)
            .into_iter()
            .map(|(p, v)| (chunk(p), Arc::new(v.iter().map(|&b| block(b)).collect())))
            .collect();
        if let Some(j) = &mut self.journal {
            for e in j.iter_mut() {
                e.0 = block(e.0);
            }
        }
        if let Some(t) = &mut self.touched {
            for b in t.iter_mut() {
                *b = block(*b);
            }
        }
        if let Some(f) = &mut self.fresh {
            for c in f.iter_mut() {
                *c = chunk(*c);
            }
        }
    }

    /// Installe des chunks importés (compressés en RLE), à leur place absolue. Les chunks
    /// invalides sont ignorés.
    pub fn set_bases(&mut self, bases: HashMap<ChunkPos, Vec<u8>>) {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        self.bases_version = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
            .filter(|p| self.bases.contains_key(&self.abs_chunk(*p)))
            .collect();
        for p in loaded {
            self.chunks.remove(&p);
            self.ensure_chunk(p);
        }
    }

    /// Chunks importés, à leur place absolue.
    pub fn bases(&self) -> impl Iterator<Item = (ChunkPos, &[u8])> + '_ {
        self.bases.iter().map(|(p, d)| (*p, d.as_slice()))
    }

    /// (v209) Numéro de l'ensemble des chunks importés (voir `bases_version`).
    pub fn bases_version(&self) -> u64 {
        self.bases_version
    }

    pub fn base_count(&self) -> usize {
        self.bases.len()
    }

    /// Chunk importé à cet emplacement (local), sinon génération procédurale (faite à sa
    /// place absolue).
    fn base_or_generate(&self, pos: ChunkPos) -> Chunk {
        let at = self.abs_chunk(pos);
        let mut c = self
            .bases
            .get(&at)
            .and_then(|d| Chunk::from_rle(pos, d))
            .unwrap_or_else(|| generate(self.generator, self.seed, at));
        c.pos = pos;
        c
    }

    // ------------------------------------------------------------ torches

    pub fn torch(&self, pos: [i32; 3]) -> Option<Torch> {
        self.torches.get(&self.abs_block(pos)).copied()
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
                self.abs_block([x, y, z]),
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

    /// Torches dont la couleur chimique s'est éteinte entre `t0` et `t1` (places locales).
    pub fn torches_expiring(&self, t0: f64, t1: f64) -> Vec<[i32; 3]> {
        self.torches
            .iter()
            .filter(|(_, t)| !t.infinite)
            .filter_map(|(p, t)| {
                let c = CHEMICALS.get(t.chemical?)?;
                let end = t.lit_at + c.burn_time_s() as f64;
                (end > t0 && end <= t1).then_some(self.local_block(*p))
            })
            .collect()
    }

    pub fn torch_count(&self) -> usize {
        self.torches.len()
    }

    /// Sources de lumière pouvant éclairer le chunk `pos` (chunk et voisins).
    pub fn lights_near(&self, pos: ChunkPos) -> Vec<Light> {
        // (Les torches sont à leur place absolue.) (v309) Écart compté sur la carte bouclée : une
        // torche juste de l'autre côté de la limite éclaire aussi.
        let (ox, oz) = self.abs_chunk(pos).origin();
        let s = CHUNK_SIZE as i32;
        let near = |p: i32, o: i32| {
            let d = wrap_block(p as i64 - o as i64);
            d >= -LIGHT_RADIUS && d < s + LIGHT_RADIUS
        };
        self.torches
            .iter()
            .filter(|(p, _)| near(p[0], ox) && near(p[2], oz))
            .map(|(p, t)| {
                let c = self.torch_color(t);
                // Intensité : le canal le plus fort à 255, la teinte conservée.
                let m = c.iter().copied().max().unwrap_or(1).max(1) as u32;
                Light {
                    pos: self.local_block(*p),
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
        let [x, y, z] = self.abs_block([x, y, z]);
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

    /// Points du chunk `pos` où le maillage mesure la lumière reçue (`MeshData::probes`) :
    /// ce qui est dessiné à part (têtes portées par des entités des cartes) est éclairé
    /// comme les blocs. Vide : plus de mesure.
    pub fn set_probes(&mut self, pos: ChunkPos, points: Vec<[i32; 3]>) {
        if points.is_empty() {
            self.probes.remove(&pos);
        } else {
            self.probes.insert(pos, Arc::new(points));
        }
    }

    pub fn probes(&self, pos: ChunkPos) -> Option<Arc<Vec<[i32; 3]>>> {
        self.probes.get(&pos).cloned()
    }

    /// Ajoute un chunk généré ailleurs (thread de travail). Ignoré s'il existe déjà.
    /// Les modifications du joueur sont réappliquées.
    pub fn insert(&mut self, mut chunk: Chunk) {
        if self.chunks.contains_key(&chunk.pos) {
            return;
        }
        let at = self.abs_chunk(chunk.pos);
        if let Some(c) = self
            .bases
            .get(&at)
            .and_then(|d| Chunk::from_rle(chunk.pos, d))
        {
            chunk = c;
        } else if self.generator != Generator::Procedural {
            let pos = chunk.pos;
            chunk = generate(self.generator, self.seed, at);
            chunk.pos = pos;
        }
        self.apply_edits(&mut chunk);
        if let Some(f) = &mut self.fresh {
            f.push(chunk.pos);
        }
        self.chunks.insert(chunk.pos, Arc::new(chunk));
    }

    fn apply_edits(&self, chunk: &mut Chunk) {
        if let Some(e) = self.edits.get(&self.abs_chunk(chunk.pos)) {
            for (&i, &id) in e {
                let (x, z, y) = (i % 16, (i / 16) % 16, i / 256);
                chunk.set(x as usize, y as usize, z as usize, id);
            }
        }
    }

    /// Modifications du joueur : (chunk absolu, index dans le chunk = x + 16 z + 256 y,
    /// bloc). Pour la sauvegarde.
    pub fn edits(&self) -> impl Iterator<Item = (ChunkPos, u32, BlockId)> + '_ {
        self.edits
            .iter()
            .flat_map(|(c, m)| m.iter().map(move |(i, b)| (*c, *i, *b)))
    }

    /// (v185) Modifications du joueur dans le chunk (local) `pos` : (position locale, bloc).
    pub fn edits_in(&self, pos: ChunkPos) -> Vec<([i32; 3], BlockId)> {
        let (ox, oz) = pos.origin();
        self.edits.get(&self.abs_chunk(pos)).map_or_else(Vec::new, |m| {
            m.iter()
                .map(|(&i, &b)| {
                    let (x, z, y) = (i % 16, (i / 16) % 16, i / 256);
                    ([ox + x as i32, y as i32, oz + z as i32], b)
                })
                .collect()
        })
    }

    // ------------------------------------------------------------ tapis empilés

    /// (v283) Sortes des couches (du dessous au dessus) du tapis ou de la pile de tapis en
    /// `pos` : celles gardées pour une pile de plusieurs couleurs, si elles vont avec son bloc
    /// (même nombre, même sorte au-dessus) ; sinon toutes de la sorte du dessus. `None` : pas
    /// un tapis.
    pub fn carpet_layers(&self, pos: [i32; 3]) -> Option<Vec<u8>> {
        let [x, y, z] = pos;
        let Some(crate::registry::Layers::Carpet { n, kind }) =
            crate::registry::Layers::of(self.get(x, y, z))
        else {
            return None;
        };
        let (c, i) = Self::cell(self.abs_block(pos));
        match self.carpets.get(&c).and_then(|m| m.get(&i)) {
            Some(v) if v.len() == n as usize && v.last() == Some(&kind) => Some(v.clone()),
            _ => Some(vec![kind; n as usize]),
        }
    }

    /// (v283) Garde les sortes des couches de la pile de tapis en `pos` (rien à garder si
    /// elles sont toutes de la même : le bloc suffit). À appeler après avoir posé son bloc.
    pub fn set_carpet_layers(&mut self, pos: [i32; 3], layers: &[u8]) {
        let (c, i) = Self::cell(self.abs_block(pos));
        let mixed = layers.len() >= 2 && layers.iter().any(|&k| Some(&k) != layers.last());
        if mixed {
            Arc::make_mut(self.carpets.entry(c).or_default()).insert(i, layers.to_vec());
        } else {
            self.forget_carpet(pos);
        }
    }

    fn forget_carpet(&mut self, pos: [i32; 3]) {
        let (c, i) = Self::cell(self.abs_block(pos));
        if let Some(m) = self.carpets.get_mut(&c) {
            if m.contains_key(&i) {
                Arc::make_mut(m).remove(&i);
                if m.is_empty() {
                    self.carpets.remove(&c);
                }
            }
        }
    }

    /// (v283) Chunk et index dans le chunk de la case `pos` (ici, toujours absolue : les piles
    /// sont gardées à leur place absolue, comme les modifications).
    fn cell(pos: [i32; 3]) -> (ChunkPos, u32) {
        let c = ChunkPos::containing(pos[0], pos[2]);
        let i = pos[0].rem_euclid(CHUNK_SIZE as i32) as u32
            + 16 * pos[2].rem_euclid(CHUNK_SIZE as i32) as u32
            + 256 * pos[1].clamp(0, CHUNK_HEIGHT as i32 - 1) as u32;
        (c, i)
    }

    /// (v283) Piles de tapis de plusieurs couleurs du chunk (local) `pos` (index dans le chunk
    /// → couches), pour son maillage.
    pub fn carpets_in(&self, pos: ChunkPos) -> Option<Arc<HashMap<u32, Vec<u8>>>> {
        self.carpets.get(&self.abs_chunk(pos)).cloned()
    }

    /// (v283) Toutes les piles de tapis de plusieurs couleurs : (case absolue, couches). Pour
    /// la sauvegarde.
    pub fn carpets(&self) -> impl Iterator<Item = ([i32; 3], &[u8])> + '_ {
        self.carpets.iter().flat_map(|(c, m)| {
            let (ox, oz) = c.origin();
            m.iter().map(move |(&i, v)| {
                let (x, z, y) = (i % 16, (i / 16) % 16, i / 256);
                ([ox + x as i32, y as i32, oz + z as i32], v.as_slice())
            })
        })
    }

    /// (v283) Recharge une pile de tapis enregistrée, à sa place absolue (avant ou après que
    /// son chunk arrive).
    pub fn load_carpet(&mut self, pos: [i32; 3], layers: Vec<u8>) {
        if layers.len() >= 2 && layers.len() <= 16 {
            let (c, i) = Self::cell(pos);
            Arc::make_mut(self.carpets.entry(c).or_default()).insert(i, layers);
        }
    }

    /// Recharge des modifications (sauvegarde) avant que les chunks soient générés.
    /// Pose un bloc venu d'un autre joueur (sans le noter au journal) ; chunk pas chargé :
    /// gardé pour sa génération.
    pub fn set_remote(&mut self, x: i32, y: i32, z: i32, id: BlockId) -> Vec<ChunkPos> {
        if !(0..CHUNK_HEIGHT as i32).contains(&y) {
            return Vec::new();
        }
        let pos = ChunkPos::containing(x, z);
        if !self.chunks.contains_key(&pos) {
            let index = x.rem_euclid(CHUNK_SIZE as i32) as u32
                + 16 * z.rem_euclid(CHUNK_SIZE as i32) as u32
                + 256 * y as u32;
            self.load_edit(self.abs_chunk(pos), index, id);
            return Vec::new();
        }
        let journal = self.journal.take();
        let touched = self.touched.take();
        let dirty = self.set(x, y, z, id);
        self.journal = journal;
        self.touched = touched;
        dirty
    }

    /// Recharge une modification enregistrée (chunk absolu).
    pub fn load_edit(&mut self, chunk: ChunkPos, index: u32, id: BlockId) {
        self.edits.entry(chunk).or_default().insert(index, id);
    }

    /// Torches posées, à leur place locale.
    pub fn torches(&self) -> impl Iterator<Item = ([i32; 3], Torch)> + '_ {
        self.torches.iter().map(|(p, t)| (self.local_block(*p), *t))
    }

    /// (v309) Torches posées, à leur place absolue (pour la sauvegarde).
    pub fn torches_abs(&self) -> impl Iterator<Item = ([i32; 3], Torch)> + '_ {
        self.torches.iter().map(|(p, t)| (*p, *t))
    }

    /// Rend la couleur de la torche en `pos` inépuisable (torches posées en créatif).
    pub fn set_torch_infinite(&mut self, pos: [i32; 3]) {
        if let Some(t) = self.torches.get_mut(&self.abs_block(pos)) {
            t.infinite = true;
        }
    }

    /// Recharge une torche enregistrée (place absolue).
    pub fn load_torch(&mut self, pos: [i32; 3], t: Torch) {
        self.torches.insert(pos, t);
    }

    pub fn contains(&self, pos: ChunkPos) -> bool {
        self.chunks.contains_key(&pos)
    }

    /// Chunk tel qu'il serait chargé (génération ou import, puis modifications), sans le
    /// garder : pour sonder un endroit éloigné.
    pub fn make_chunk(&self, pos: ChunkPos) -> Chunk {
        let mut c = self.base_or_generate(pos);
        self.apply_edits(&mut c);
        c
    }

    /// Génère le chunk s'il n'est pas déjà chargé.
    pub fn ensure_chunk(&mut self, pos: ChunkPos) {
        if !self.chunks.contains_key(&pos) {
            let mut c = self.base_or_generate(pos);
            self.apply_edits(&mut c);
            if let Some(f) = &mut self.fresh {
                f.push(pos);
            }
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
        if let Some(j) = &mut self.journal {
            if old != id {
                j.push(([x, y, z], id, old));
            }
        }
        if let Some(t) = &mut self.touched {
            if old != id {
                t.push([x, y, z]);
            }
        }
        let index = lx as u32 + 16 * lz as u32 + 256 * y as u32;
        let at = self.abs_chunk(pos);
        self.edits.entry(at).or_default().insert(index, id);
        if old.is_torch() && !id.is_torch() {
            let p = self.abs_block([x, y, z]);
            self.torches.remove(&p);
        }
        // (v283) Plus une pile de tapis : ses couleurs sont oubliées. (Une pile qui change de
        // hauteur garde les siennes jusqu'à ce que `set_carpet_layers` les remplace ; si elles
        // ne vont plus avec le bloc, `carpet_layers` ne les prend pas.)
        if old != id
            && !matches!(
                crate::registry::Layers::of(id),
                Some(crate::registry::Layers::Carpet { n: 2.., .. })
            )
        {
            self.forget_carpet([x, y, z]);
        }
        // Une source de lumière à portée : la lumière change sur toute sa zone d'influence.
        // (v185) Sauf si ni l'ancien ni le nouveau bloc n'arrête la lumière (eau qui coule,
        // air, plantes) : elle passe pareil, seul le chunk (et ses bords) change. L'eau qui
        // coulait près d'une torche faisait remailler une dizaine de chunks à chaque pas.
        let open = |b: BlockId| crate::registry::light_cover(b) == [0; 6];
        if old.is_torch()
            || old.def().light.is_some()
            || id.def().light.is_some()
            || (!(open(old) && open(id)) && self.torch_nearby(x, y, z))
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

    /// (v185) Près d'une torche, un bloc qui arrête la lumière fait remailler toute sa
    /// zone ; de l'eau qui coule (ni l'ancien ni le nouveau bloc n'arrête la lumière),
    /// seulement son chunk.
    #[test]
    fn flowing_water_near_a_torch_remeshes_only_its_chunk() {
        let mut w = World::new(0);
        for dz in -1..=1 {
            for dx in -1..=1 {
                w.ensure_chunk(ChunkPos { x: dx, z: dz });
            }
        }
        w.place_torch(8, 200, 8, None);
        assert_eq!(
            w.set(6, 200, 8, BlockId::WATER),
            vec![ChunkPos { x: 0, z: 0 }]
        );
        let flow = crate::registry::registry().by_key("water_flow:3").unwrap();
        assert_eq!(w.set(6, 200, 8, flow), vec![ChunkPos { x: 0, z: 0 }]);
        assert_eq!(
            w.set(6, 200, 8, BlockId::AIR),
            vec![ChunkPos { x: 0, z: 0 }]
        );
        assert!(w.set(6, 200, 8, BlockId::STONE).len() > 1);
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

    /// (v283) Une pile de tapis de plusieurs couleurs garde ses couches ; une d'une seule
    /// couleur n'a rien à garder ; cassée ou remplacée, elle les oublie ; des couches qui ne
    /// vont plus avec le bloc (pile changée par un autre joueur) ne sont pas prises.
    #[test]
    fn carpet_stacks_keep_the_color_of_each_layer() {
        use crate::registry::Layers;
        let mut w = World::new(0);
        w.insert_empty(ChunkPos { x: -1, z: 0 });
        let p = [-3, 100, 5];
        let stack = |n, kind| Layers::Carpet { n, kind }.block().unwrap();
        w.set(p[0], p[1], p[2], stack(1, 14));
        assert_eq!(w.carpet_layers(p), Some(vec![14]));
        w.set(p[0], p[1], p[2], stack(3, 2));
        w.set_carpet_layers(p, &[14, 0, 2]);
        assert_eq!(w.carpet_layers(p), Some(vec![14, 0, 2]));
        assert_eq!(w.carpets().count(), 1);
        let back = w.carpets_in(ChunkPos { x: -1, z: 0 }).unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(w.carpets().next().unwrap(), (p, &[14u8, 0, 2][..]));
        // Toutes pareilles : rien à garder.
        w.set(p[0], p[1], p[2], stack(4, 2));
        w.set_carpet_layers(p, &[2, 2, 2, 2]);
        assert_eq!(w.carpets().count(), 0);
        assert_eq!(w.carpet_layers(p), Some(vec![2; 4]));
        // Relue d'une sauvegarde, avant le chunk ; puis un bloc d'une autre hauteur.
        let mut w2 = World::new(0);
        w2.load_carpet(p, vec![1, 1, 5]);
        w2.insert_empty(ChunkPos { x: -1, z: 0 });
        w2.set(p[0], p[1], p[2], stack(3, 5));
        assert_eq!(w2.carpet_layers(p), Some(vec![1, 1, 5]));
        w2.set(p[0], p[1], p[2], stack(5, 5));
        assert_eq!(w2.carpet_layers(p), Some(vec![5; 5]));
        // Cassée : oubliée.
        w2.set(p[0], p[1], p[2], BlockId::AIR);
        assert_eq!(w2.carpets().count(), 0);
        assert_eq!(w2.carpet_layers(p), None);
    }

    /// (v309) La même partie du monde comptée depuis une autre origine : les mêmes blocs
    /// (générés à leur place absolue), les modifications et les torches enregistrées à leur
    /// place absolue, relues pareil depuis une troisième origine ; la lumière des torches au
    /// même endroit ; `set_origin` remet les chunks chargés à leur nouvelle place.
    #[test]
    fn a_local_origin_changes_only_the_numbers() {
        // ≈ 16,7 millions de blocs du centre.
        let far = ChunkPos {
            x: 1_043_712,
            z: -1_043_200,
        };
        let mut a = World::new(7);
        let mut b = World::new(7);
        b.set_origin(far);
        assert_eq!(b.origin_blocks(), [far.x * 16, 0, far.z * 16]);
        let abs = ChunkPos {
            x: far.x + 2,
            z: far.z - 1,
        };
        let local = b.local_chunk(abs);
        assert_eq!(local, ChunkPos { x: 2, z: -1 });
        assert_eq!(b.abs_chunk(local), abs);
        for dz in -1..=1 {
            for dx in -1..=1 {
                a.ensure_chunk(ChunkPos {
                    x: abs.x + dx,
                    z: abs.z + dz,
                });
                b.ensure_chunk(ChunkPos {
                    x: local.x + dx,
                    z: local.z + dz,
                });
            }
        }
        assert_eq!(b.chunk(local).unwrap().pos, local);
        for y in (0..300).step_by(7) {
            for z in 0..16 {
                for x in 0..16 {
                    assert_eq!(
                        a.chunk(abs).unwrap().get(x, y, z),
                        b.chunk(local).unwrap().get(x, y, z)
                    );
                }
            }
        }
        let (lx, lz) = local.origin();
        let (ax, az) = abs.origin();
        assert_eq!(b.abs_block([lx, 5, lz]), [ax, 5, az]);
        assert_eq!(b.local_block([ax, 5, az]), [lx, 5, lz]);
        a.set(ax + 3, 200, az + 4, BlockId::BRICK);
        b.set(lx + 3, 200, lz + 4, BlockId::BRICK);
        assert_eq!(a.edits().collect::<Vec<_>>(), b.edits().collect::<Vec<_>>());
        assert_eq!(b.edits_in(local), vec![([lx + 3, 200, lz + 4], BlockId::BRICK)]);
        b.place_torch(lx + 8, 201, lz + 8, None);
        assert_eq!(b.torches_abs().next().unwrap().0, [ax + 8, 201, az + 8]);
        assert_eq!(b.torches().next().unwrap().0, [lx + 8, 201, lz + 8]);
        assert!(b.torch([lx + 8, 201, lz + 8]).is_some());
        assert!(b
            .lights_near(local)
            .iter()
            .any(|l| l.pos == [lx + 8, 201, lz + 8]));
        // Relu depuis ce qui est enregistré, avec une troisième origine.
        let mut c = World::new(7);
        for (ch, i, id) in b.edits() {
            c.load_edit(ch, i, id);
        }
        for (p, t) in b.torches_abs() {
            c.load_torch(p, t);
        }
        c.set_origin(ChunkPos {
            x: far.x + 256,
            z: far.z,
        });
        let lc = c.local_chunk(abs);
        c.ensure_chunk(lc);
        let (cx, cz) = lc.origin();
        assert_eq!(c.get(cx + 3, 200, cz + 4), BlockId::BRICK);
        assert_eq!(c.get(cx + 8, 201, cz + 8), BlockId::TORCH);
        assert!(c.torch([cx + 8, 201, cz + 8]).is_some());
        // Casser la torche (coordonnées locales) l'oublie à sa place absolue.
        c.set(cx + 8, 201, cz + 8, BlockId::AIR);
        assert_eq!(c.torches_abs().count(), 0);
        // Les chunks chargés suivent un changement d'origine.
        b.set_origin(ChunkPos { x: 0, z: 0 });
        assert_eq!(b.chunk(abs).unwrap().pos, abs);
        assert_eq!(b.get(ax + 3, 200, az + 4), BlockId::BRICK);
        assert!(b.torch([ax + 8, 201, az + 8]).is_some());
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
