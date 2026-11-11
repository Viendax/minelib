//! Pool de threads de travail : génération de chunks et maillage.
//!
//! Le thread principal soumet les tâches dans l'ordre de priorité (les plus proches
//! d'abord) et garde la file courte (`capacity`), pour que les priorités suivent le
//! joueur quand il se déplace.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Répartition du processeur (option `cpu_sharing`) :
/// - uniforme (par défaut) : un thread de fond par cœur, à la priorité la plus basse ; le
///   thread du jeu et du rendu a une priorité haute. Chaque cœur sert d'abord au jeu, ce qui
///   reste de son temps va au chargement du monde ;
/// - réservée : 1 cœur pour le jeu, 1 pour le pilote, les autres pour les tâches de fond.
static UNIFORM: AtomicBool = AtomicBool::new(true);

pub fn set_uniform_sharing(on: bool) {
    UNIFORM.store(on, Ordering::Relaxed);
}

pub fn uniform_sharing() -> bool {
    UNIFORM.load(Ordering::Relaxed)
}

/// Threads de fond autorisés à travailler (les autres attendent). Réduit quand le thread du
/// jeu se fait voler du temps pendant les gros chargements (voir `throttle`).
static ACTIVE_LIMIT: AtomicUsize = AtomicUsize::new(usize::MAX);

/// Régulation des threads de fond, appelée à chaque image : `stolen_ms` = temps perdu par le
/// thread du jeu hors de son travail (« hors mesure »), `loading` = chunks en cours de
/// chargement. Au-delà de 1,5 ms perdues en moyenne, un thread de fond de moins (jusqu'à la
/// moitié des cœurs) ; sous 0,5 ms, un de plus. Une fois tout chargé : tous réautorisés.
pub fn throttle(stolen_ms: f32, loading: bool, dt: f32) {
    use std::sync::Mutex;
    static STATE: Mutex<(f32, f32)> = Mutex::new((0.0, 0.0)); // (moyenne, minuterie)
    let cores = std::thread::available_parallelism().map_or(2, |n| n.get());
    let total = worker_count(cores);
    let Ok(mut st) = STATE.lock() else { return };
    st.0 += (stolen_ms.min(50.0) - st.0) * 0.05;
    st.1 += dt;
    if !loading {
        ACTIVE_LIMIT.store(usize::MAX, Ordering::Relaxed);
        return;
    }
    if st.1 < 0.5 {
        return;
    }
    st.1 = 0.0;
    let cur = ACTIVE_LIMIT.load(Ordering::Relaxed).min(total);
    let floor = (total / 2).max(1);
    let next = if st.0 > 1.5 {
        cur.saturating_sub(1).max(floor)
    } else if st.0 < 0.5 {
        (cur + 1).min(total)
    } else {
        cur
    };
    if next != cur {
        ACTIVE_LIMIT.store(next, Ordering::Relaxed);
        log::debug!(
            "threads de fond actifs : {next}/{total} (thread du jeu gêné : {:.2} ms)",
            st.0
        );
    }
    minelib_render::perf::gauge("threads de fond actifs", next as f32);
}

/// Nombre de threads de fond pour `cores` cœurs.
pub fn worker_count(cores: usize) -> usize {
    if uniform_sharing() {
        cores.clamp(1, 64)
    } else {
        cores.saturating_sub(2).clamp(1, 32)
    }
}
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

use minelib_world::gen::{generate, Generator};
use minelib_world::mesh::{mesh_neighborhood, MeshData, Neighborhood};
use minelib_world::{Chunk, ChunkPos};

pub enum Job {
    Generate(ChunkPos),
    Mesh { n: Neighborhood, version: u32 },
}

pub enum Done {
    Generated(Chunk),
    Meshed {
        pos: ChunkPos,
        version: u32,
        mesh: Box<MeshData>,
    },
}

/// File de tâches partagée : les urgentes (blocs modifiés par le joueur) passent avant.
#[derive(Default)]
struct Queue {
    jobs: Mutex<(VecDeque<Job>, VecDeque<Job>, bool)>,
    ready: Condvar,
}

impl Queue {
    fn push(&self, job: Job, urgent: bool) {
        if let Ok(mut q) = self.jobs.lock() {
            if urgent {
                q.0.push_back(job);
            } else {
                q.1.push_back(job);
            }
            self.ready.notify_one();
        }
    }

    /// Prochaine tâche (None : file fermée).
    fn pop(&self) -> Option<Job> {
        let mut q = self.jobs.lock().ok()?;
        loop {
            if let Some(j) = q.0.pop_front().or_else(|| q.1.pop_front()) {
                return Some(j);
            }
            if q.2 {
                return None;
            }
            q = self.ready.wait(q).ok()?;
        }
    }

    fn close(&self) {
        if let Ok(mut q) = self.jobs.lock() {
            q.0.clear();
            q.1.clear();
            q.2 = true;
        }
        self.ready.notify_all();
    }
}

pub struct Workers {
    queue: Arc<Queue>,
    rx: Receiver<Done>,
    threads: Vec<JoinHandle<()>>,
    /// Monde quitté : les tâches restantes sont abandonnées.
    cancel: Arc<AtomicBool>,
    in_flight: usize,
}

impl Workers {
    pub fn new(seed: u32, gen: Generator) -> Self {
        let cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(2);
        let count = worker_count(cores);
        ACTIVE_LIMIT.store(usize::MAX, Ordering::Relaxed);
        let uniform = uniform_sharing();
        let queue = Arc::new(Queue::default());
        let (done_tx, rx) = channel::<Done>();
        let cancel = Arc::new(AtomicBool::new(false));
        let threads = (0..count)
            .filter_map(|i| {
                let queue = queue.clone();
                let done_tx = done_tx.clone();
                let cancel = cancel.clone();
                std::thread::Builder::new()
                    .name(format!("minelib-worker-{i}"))
                    .stack_size(8 << 20)
                    .spawn(move || {
                        // Le rendu passe avant : pas de saccade quand tous les cœurs travaillent.
                        if uniform {
                            minelib_platform::lowest_thread_priority();
                        } else {
                            minelib_platform::lower_thread_priority();
                        }
                        worker_loop(i, &queue, &cancel, &done_tx, gen, seed)
                    })
                    .ok()
            })
            .collect::<Vec<_>>();
        log::info!(
            "{} threads de travail ({}, {cores} cœurs)",
            threads.len(),
            if uniform {
                "répartition uniforme : priorité la plus basse sur tous les cœurs"
            } else {
                "répartition réservée"
            }
        );
        Self {
            queue,
            rx,
            threads,
            cancel,
            in_flight: 0,
        }
    }

    pub fn count(&self) -> usize {
        self.threads.len()
    }

    /// Nombre de tâches qu'on accepte d'avoir en attente ou en cours.
    pub fn capacity(&self) -> usize {
        self.threads.len() * 3
    }

    pub fn in_flight(&self) -> usize {
        self.in_flight
    }

    pub fn has_room(&self) -> bool {
        self.in_flight < self.capacity()
    }

    pub fn submit(&mut self, job: Job) {
        self.queue.push(job, false);
        self.in_flight += 1;
    }

    /// Tâche à faire avant toutes les autres (chunk modifié par le joueur).
    pub fn submit_urgent(&mut self, job: Job) {
        self.queue.push(job, true);
        self.in_flight += 1;
    }

    /// Résultats terminés depuis le dernier appel.
    pub fn poll(&mut self) -> Vec<Done> {
        let done: Vec<Done> = self.rx.try_iter().collect();
        self.in_flight = self.in_flight.saturating_sub(done.len());
        done
    }
}

fn worker_loop(
    index: usize,
    queue: &Queue,
    cancel: &AtomicBool,
    done_tx: &std::sync::mpsc::Sender<Done>,
    gen: Generator,
    seed: u32,
) {
    loop {
        // Thread mis en pause par la régulation : il attend sans prendre de tâche.
        while index >= ACTIVE_LIMIT.load(Ordering::Relaxed) && !cancel.load(Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let Some(job) = queue.pop() else { return };
        if cancel.load(Ordering::Relaxed) {
            continue;
        }
        // Une erreur dans un chunk ne doit jamais arrêter le thread (sinon plus aucun chunk
        // ne se calcule et le monde reste invisible) : le chunk fautif est noté dans le
        // journal et rendu vide, le reste continue.
        let done = match job {
            Job::Generate(pos) => match std::panic::catch_unwind(|| generate(gen, seed, pos)) {
                Ok(c) => Done::Generated(c),
                Err(_) => {
                    log::error!("génération du chunk {pos:?} en erreur : chunk ignoré");
                    continue;
                }
            },
            Job::Mesh { n, version } => {
                let pos = n.center;
                let mesh = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    mesh_neighborhood(&n)
                }))
                .unwrap_or_else(|_| {
                    log::error!("maillage du chunk {pos:?} en erreur : chunk laissé vide");
                    MeshData::default()
                });
                Done::Meshed {
                    pos,
                    version,
                    mesh: Box::new(mesh),
                }
            }
        };
        if done_tx.send(done).is_err() {
            return;
        }
    }
}

impl Drop for Workers {
    fn drop(&mut self) {
        // Abandonne les tâches pas encore commencées (monde quitté), puis ferme la file :
        // les threads sortent de recv().
        self.cancel.store(true, Ordering::Relaxed);
        self.queue.close();
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}
