//! Chargement progressif du monde autour du joueur : chunks en pleine résolution jusqu'à
//! la distance de rendu, du plus proche au plus lointain.

use std::collections::{HashMap, HashSet};

use minelib_render::Renderer;
use minelib_world::mesh::Neighborhood;
use minelib_world::{ChunkPos, World, CHUNK_SIZE};

use crate::jobs::{Done, Job, Workers};

/// On ne recalcule la sélection que si le joueur a bougé d'au moins autant de blocs.
const RESELECT_DISTANCE: f32 = 8.0;

#[derive(Default, Clone, Copy, Debug)]
pub struct StreamStats {
    pub wanted_chunks: usize,
    pub pending_jobs: usize,
}

pub struct Streamer {
    /// Distance de rendu (chunks).
    pub radius: i32,
    /// Chunks voulus, du plus proche au plus lointain.
    wanted_list: Vec<ChunkPos>,
    /// Chunks à garder en mémoire (chunks voulus + un anneau pour le maillage).
    keep_chunks: HashSet<ChunkPos>,
    /// Chunks à afficher.
    needed_chunks: HashSet<ChunkPos>,
    meshed: HashSet<ChunkPos>,
    gen_pending: HashSet<ChunkPos>,
    mesh_pending: HashSet<ChunkPos>,
    versions: HashMap<ChunkPos, u32>,
    last_select: Option<(f32, f32)>,
    changed: bool,
    /// Plus rien à demander aux threads de travail (voir `schedule`).
    idle: bool,
    /// Premier chunk de `wanted_list` pas encore prêt.
    sched_from: usize,
    /// Résultats reçus pas encore envoyés au GPU (envois étalés sur plusieurs images).
    backlog: std::collections::VecDeque<Done>,
    pub stats: StreamStats,
}

/// Chunks dans le cercle de `radius` chunks autour de (`px`, `pz`), du plus proche au plus
/// lointain.
pub fn select(px: f32, pz: f32, radius: i32) -> Vec<ChunkPos> {
    let size = CHUNK_SIZE as f32;
    let reach = radius as f32 * size;
    let (cx, cz) = ((px / size).floor() as i32, (pz / size).floor() as i32);
    let dist = |c: &ChunkPos| {
        let (x, z) = (
            (c.x as f32 + 0.5) * size - px,
            (c.z as f32 + 0.5) * size - pz,
        );
        (x * x + z * z).sqrt()
    };
    let mut out: Vec<ChunkPos> = (cz - radius..=cz + radius)
        .flat_map(|z| (cx - radius..=cx + radius).map(move |x| ChunkPos { x, z }))
        .filter(|c| dist(c) <= reach)
        .collect();
    out.sort_by(|a, b| dist(a).total_cmp(&dist(b)));
    out
}

impl Streamer {
    /// Tout ce qui est voulu est chargé et maillé.
    pub fn idle(&self) -> bool {
        self.idle
    }

    pub fn new(radius: i32) -> Self {
        Self {
            radius,
            wanted_list: Vec::new(),
            keep_chunks: HashSet::new(),
            needed_chunks: HashSet::new(),
            meshed: HashSet::new(),
            gen_pending: HashSet::new(),
            mesh_pending: HashSet::new(),
            versions: HashMap::new(),
            last_select: None,
            changed: false,
            idle: false,
            sched_from: 0,
            backlog: std::collections::VecDeque::new(),
            stats: StreamStats::default(),
        }
    }

    /// Nouvelle distance de rendu : la sélection sera refaite à l'image suivante.
    pub fn set_radius(&mut self, radius: i32) {
        self.radius = radius;
        self.last_select = None;
        self.idle = false;
        self.sched_from = 0;
    }

    /// Chunk modifié (bloc, lumière) : nouveau maillage en tâche prioritaire sur les threads
    /// de travail (l'ancien reste affiché une image ou deux), et les maillages en cours de
    /// calcul sont invalidés. Le thread principal ne maille plus : pas de saccade.
    pub fn remesh_now(&mut self, world: &World, workers: &mut Workers, pos: ChunkPos) {
        let version = {
            let v = self.versions.entry(pos).or_default();
            *v += 1;
            *v
        };
        self.idle = false;
        if self.meshed.contains(&pos) || self.mesh_pending.contains(&pos) {
            if let Some(n) = Neighborhood::from_world(world, pos) {
                self.mesh_pending.insert(pos);
                workers.submit_urgent(Job::Mesh { n, version });
            }
        }
    }

    pub fn update(
        &mut self,
        world: &mut World,
        renderer: &mut Renderer,
        workers: &mut Workers,
        px: f32,
        pz: f32,
    ) {
        use minelib_render::perf::scope;
        {
            let _s = scope("chunks : réception et envoi GPU");
            self.collect(world, renderer, workers);
        }

        let moved = self
            .last_select
            .is_none_or(|(x, z)| ((x - px).powi(2) + (z - pz).powi(2)).sqrt() > RESELECT_DISTANCE);
        if moved {
            let _s = scope("chunks : sélection (déplacement)");
            self.reselect(world, px, pz);
        }
        {
            let _s = scope("chunks : planification");
            self.schedule(world, workers);
        }
        if self.changed {
            let _s = scope("chunks : retrait des anciens");
            self.retire(renderer);
            self.changed = false;
        }
        self.stats.pending_jobs = workers.in_flight();
    }

    fn collect(&mut self, world: &mut World, renderer: &mut Renderer, workers: &mut Workers) {
        // Envois au GPU étalés : au plus ~1,5 ms par image, le reste à l'image suivante.
        let before = self.backlog.len();
        self.backlog.extend(workers.poll());
        if self.backlog.len() != before {
            self.idle = false;
        }
        let start = std::time::Instant::now();
        let (mut meshes, mut bytes) = (0usize, 0usize);
        while let Some(done) = self.backlog.pop_front() {
            if meshes > 0 && start.elapsed() > std::time::Duration::from_micros(1500) {
                self.backlog.push_front(done);
                break;
            }
            if let Done::Meshed { mesh, .. } = &done {
                meshes += 1;
                bytes += mesh.vertices.len() * std::mem::size_of::<minelib_world::mesh::Vertex>()
                    + mesh.indices.len() * 4;
            }
            match done {
                Done::Generated(chunk) => {
                    minelib_render::perf::count("chunks générés", 1);
                    self.gen_pending.remove(&chunk.pos);
                    if self.keep_chunks.contains(&chunk.pos) {
                        world.insert(chunk);
                    }
                }
                Done::Meshed { pos, version, mesh } => {
                    self.mesh_pending.remove(&pos);
                    let current = self.versions.get(&pos).copied().unwrap_or(0);
                    if version == current && self.needed_chunks.contains(&pos) {
                        renderer.upload_chunk(pos, &mesh);
                        self.meshed.insert(pos);
                        self.changed = true;
                    }
                }
            }
        }
        if meshes > 0 {
            minelib_render::perf::count("maillages reçus", meshes as u64);
            let left = self.backlog.len();
            minelib_render::perf::event(|| {
                format!(
                    "{meshes} maillages envoyés au GPU ({} Ko), {left} en attente",
                    bytes / 1024
                )
            });
        }
    }

    fn reselect(&mut self, world: &mut World, px: f32, pz: f32) {
        self.last_select = Some((px, pz));
        self.idle = false;
        self.sched_from = 0;
        let list = select(px, pz, self.radius);
        self.needed_chunks = list.iter().copied().collect();
        self.keep_chunks.clear();
        for c in &self.needed_chunks {
            for dz in -1..=1 {
                for dx in -1..=1 {
                    self.keep_chunks.insert(ChunkPos {
                        x: c.x + dx,
                        z: c.z + dz,
                    });
                }
            }
        }
        // Données de chunks qui ne servent plus. Les libérer peut prendre des centaines de
        // millisecondes quand des milliers de chunks partent d'un coup (distance de rendu
        // baissée) : c'est fait sur un thread à part, pas sur celui du jeu.
        let unload: Vec<ChunkPos> = world
            .loaded()
            .filter(|c| !self.keep_chunks.contains(c))
            .collect();
        let freed: Vec<_> = unload.into_iter().filter_map(|c| world.unload(c)).collect();
        if freed.len() > 16 {
            let _ = std::thread::Builder::new()
                .name("libération des chunks".into())
                .spawn(move || drop(freed));
        }

        self.stats.wanted_chunks = self.needed_chunks.len();
        self.wanted_list = list;
        self.changed = true;
    }

    fn schedule(&mut self, world: &World, workers: &mut Workers) {
        // Tout est déjà chargé et maillé : rien à parcourir (on ne refait la liste qu'après
        // un déplacement, un résultat reçu ou un chunk modifié).
        if self.idle {
            return;
        }
        let mut busy = false;
        // Les chunks avant `sched_from` sont déjà prêts : on reprend où on s'était arrêté
        // (avec 256 chunks de distance, la liste compte des dizaines de milliers d'entrées).
        let mut first_busy: Option<usize> = None;
        let start = self.sched_from.min(self.wanted_list.len());
        for i in start..self.wanted_list.len() {
            if !workers.has_room() {
                // Le chunk précédent n'est peut-être pas fini : on reprendra à partir de lui.
                self.sched_from = first_busy.unwrap_or(if busy { i.saturating_sub(1) } else { i });
                return;
            }
            if busy && first_busy.is_none() {
                first_busy = Some(i.saturating_sub(1));
            }
            let pos = self.wanted_list[i];
            if self.mesh_pending.contains(&pos) {
                busy = true;
                continue;
            }
            if self.meshed.contains(&pos) {
                continue;
            }
            busy = true;
            // Le maillage a besoin des 8 voisins.
            let mut ready = true;
            for dz in -1..=1 {
                for dx in -1..=1 {
                    let c = ChunkPos {
                        x: pos.x + dx,
                        z: pos.z + dz,
                    };
                    if world.contains(c) {
                        continue;
                    }
                    ready = false;
                    if !self.gen_pending.contains(&c) && workers.has_room() {
                        self.gen_pending.insert(c);
                        workers.submit(Job::Generate(c));
                    }
                }
            }
            if ready && workers.has_room() {
                if let Some(n) = Neighborhood::from_world(world, pos) {
                    let version = self.versions.get(&pos).copied().unwrap_or(0);
                    self.mesh_pending.insert(pos);
                    workers.submit(Job::Mesh { n, version });
                }
            }
        }
        self.idle = !busy;
        let len = self.wanted_list.len();
        self.sched_from = first_busy.unwrap_or(if busy { len.saturating_sub(1) } else { len });
    }

    /// Retire de l'écran les chunks qui ne sont plus voulus.
    fn retire(&mut self, renderer: &mut Renderer) {
        let stale: Vec<ChunkPos> = self
            .meshed
            .iter()
            .copied()
            .filter(|c| !self.needed_chunks.contains(c))
            .collect();
        for c in stale {
            renderer.remove_chunk(c);
            self.meshed.remove(&c);
            self.versions.remove(&c);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_is_a_disc_sorted_by_distance() {
        let l = select(8.0, 8.0, 4);
        assert_eq!(l[0], ChunkPos { x: 0, z: 0 });
        assert!(l.contains(&ChunkPos { x: 4, z: 0 }));
        assert!(!l.contains(&ChunkPos { x: 4, z: 4 }));
        assert!(l.len() > 40 && l.len() < 81);
    }
}
