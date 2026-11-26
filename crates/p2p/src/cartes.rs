//! (v352) Cartes partagées : chaque machine qui a une carte la donne aux autres.
//!
//! Les cartes (des parties FPS et des mondes de construction) sont nommées par leur
//! contenu. Le relais, les passeurs et les jeux qui en ont une la servent par iroh-blobs
//! (n0-computer, MIT / Apache-2.0) : des flux vérifiés BLAKE3, où celui qui télécharge voit
//! qu'un morceau est faux au plus tard 16 Kio plus loin, et le reprend ailleurs. Une carte
//! est prise à plusieurs machines à la fois, morceau par morceau (`PIECE`) ; sa signature
//! (SHA-256, `minelib_net::game::signature`) est vérifiée à la fin, comme avant.
//!
//! Chaque machine garde les cartes qu'elle a prises dans un dossier à elle, dans une place
//! limitée (`Limits::place` : celles prises ou servies le moins récemment partent
//! d'abord). Celles du relais restent dans son dossier `maps` : référencées, pas copiées,
//! et hors de cette place (c'est le relais qui fait la sienne).
//!
//! Les jeux restent anonymes (v313) : leur nœud n'a pas de transport IP, une carte passe
//! donc par les serveurs relais d'iroh ou par les passeurs, comme le reste. Elle va plus
//! vite parce qu'elle vient de plusieurs machines à la fois, chacune avec son propre
//! débit.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use iroh::protocol::ProtocolHandler;
use iroh_blobs::api::blobs::{AddPathOptions, BlobStatus, ImportMode};
use iroh_blobs::protocol::{ChunkRanges, ChunkRangesExt, GetRequest};
use iroh_blobs::provider::events::{
    AbortReason, ConnectMode, EventMask, EventSender, ProviderMessage, ThrottleMode,
};
use iroh_blobs::store::fs::{options::Options, FsStore};
use iroh_blobs::store::{GcConfig, ProtectCb, ProtectOutcome};
use iroh_blobs::{BlobFormat, BlobsProtocol, Hash};
use minelib_net::proto::{MapHeld, MapSource, MAP_MAX};
use serde::{Deserialize, Serialize};

/// Protocole des cartes dans les connexions iroh (celui d'iroh-blobs).
pub const ALPN: &[u8] = iroh_blobs::ALPN;

/// Morceau demandé à une machine à la fois (un multiple de 16 Kio, le grain des preuves).
const PIECE: u64 = 256 * 1024;
/// Machines au plus qui donnent une même carte en même temps ; les suivantes de la liste
/// remplacent celles qui échouent.
pub const SOURCES_MAX: usize = 6;
const CONNECT_WAIT: Duration = Duration::from_secs(15);
const PIECE_WAIT: Duration = Duration::from_secs(60);
/// Un morceau en cours depuis ce temps peut être demandé aussi à une machine libre : en fin
/// de carte, la plus lente ne fait plus attendre tout le monde.
const ENDGAME_AFTER: Duration = Duration::from_secs(2);
/// Ménage du magasin (les cartes sorties de la place, les morceaux d'essais ratés).
const GC_EVERY: Duration = Duration::from_secs(600);
/// Envoi permis d'avance au-delà du débit (octets).
const BURST: u64 = 256 * 1024;
const INDEX: &str = "cartes.bin";

/// Ce qu'une machine donne de sa place et de sa connexion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Place des cartes gardées (octets ; celles du relais n'y comptent pas).
    pub place: u64,
    /// Débit d'envoi au plus, toutes cartes servies ensemble (octets/s ; 0 : sans limite).
    pub rate: u64,
    /// Machines servies en même temps.
    pub conns: usize,
}

impl Limits {
    /// Jeu d'un joueur : sa connexion sert d'abord à jouer (64 Ko/s : la moitié d'un envoi
    /// d'ADSL ordinaire).
    pub const GAME: Limits = Limits {
        place: 1_000_000_000,
        rate: 64_000,
        conns: 3,
    };
    /// Relais et passeurs sans joueur : des machines faites pour servir.
    pub const SERVER: Limits = Limits {
        place: 2_000_000_000,
        rate: 0,
        conns: 64,
    };
}

/// (Réglages du jeu, lus dans ses options avant que son nœud démarre.)
static GAME_LIMITS: Mutex<Limits> = Mutex::new(Limits::GAME);

/// Place (Mo, 0 : ne rien partager) et débit (Ko/s) des cartes partagées par ce jeu
/// (`partage_cartes_mo=`, `partage_cartes_ko=` dans `options.txt`).
pub fn set_game_limits(place_mb: u64, rate_kbps: u64) {
    let mut l = GAME_LIMITS.lock().unwrap();
    l.place = place_mb.saturating_mul(1_000_000);
    l.rate = rate_kbps.saturating_mul(1000);
}

pub fn game_limits() -> Limits {
    *GAME_LIMITS.lock().unwrap()
}

/// Une carte gardée ici.
#[derive(Serialize, Deserialize, Clone, Debug)]
struct Held {
    sig: [u8; 32],
    hash: [u8; 32],
    size: u64,
    /// Dernière fois prise ou donnée à ce programme (s, époque Unix).
    used: u64,
    /// Fichier du relais (son dossier `maps`) : hors de la place, jamais effacé ici.
    external: bool,
}

impl Held {
    fn map(&self) -> MapHeld {
        MapHeld {
            sig: self.sig,
            hash: self.hash,
            size: self.size,
        }
    }
}

pub struct Cartes {
    store: FsStore,
    blobs: BlobsProtocol,
    ep: iroh::Endpoint,
    rt: tokio::runtime::Handle,
    dir: PathBuf,
    limits: Arc<Mutex<Limits>>,
    index: Mutex<HashMap<[u8; 32], Held>>,
    /// Cartes sorties de la place depuis la dernière fois (`take_gone`).
    gone: Mutex<Vec<[u8; 32]>>,
    /// Empreintes en cours de téléchargement : le ménage n'y touche pas.
    fetching: Arc<Mutex<HashSet<Hash>>>,
    /// Octets servis aux autres depuis le lancement.
    served: Arc<AtomicU64>,
}

impl std::fmt::Debug for Cartes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cartes").field("dir", &self.dir).finish()
    }
}

fn now() -> u64 {
    crate::now()
}

fn tag_name(sig: &[u8; 32]) -> String {
    format!("carte/{}", minelib_net::crypto::hex(sig))
}

fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

/// Les cartes à sortir pour revenir dans `place` : celles prises ou servies le moins
/// récemment d'abord (jamais celles du relais).
fn over_place(index: &HashMap<[u8; 32], Held>, place: u64) -> Vec<[u8; 32]> {
    let mut own: Vec<&Held> = index.values().filter(|h| !h.external).collect();
    let mut total: u64 = own.iter().map(|h| h.size).sum();
    own.sort_by_key(|h| (h.used, h.sig));
    let mut out = Vec::new();
    for h in own {
        if total <= place {
            break;
        }
        total -= h.size;
        out.push(h.sig);
    }
    out
}

impl Cartes {
    pub(crate) fn open(
        dir: &Path,
        ep: iroh::Endpoint,
        rt: tokio::runtime::Handle,
        limits: Limits,
    ) -> Result<Arc<Self>> {
        std::fs::create_dir_all(dir)?;
        let fetching: Arc<Mutex<HashSet<Hash>>> = Arc::default();
        let options = || {
            let f = fetching.clone();
            let protect: ProtectCb = Arc::new(move |live: &mut HashSet<Hash>| {
                live.extend(f.lock().unwrap().iter().copied());
                Box::pin(async { ProtectOutcome::Continue })
            });
            let mut o = Options::new(dir);
            o.gc = Some(GcConfig {
                interval: GC_EVERY,
                add_protected: Some(protect),
            });
            o
        };
        let db = dir.join("blobs.db");
        let store = match rt.block_on(FsStore::load_with_opts(db.clone(), options())) {
            Ok(s) => s,
            Err(e) => {
                // Ce n'est qu'un cache (arrêt brutal, autre version) : on repart à vide.
                log::warn!("cartes partagées : magasin illisible ({e}), vidé");
                let _ = std::fs::remove_dir_all(dir);
                std::fs::create_dir_all(dir)?;
                rt.block_on(FsStore::load_with_opts(db, options()))
                    .map_err(|e| anyhow!("cartes partagées : {e}"))?
            }
        };
        let mut index: HashMap<[u8; 32], Held> = std::fs::read(dir.join(INDEX))
            .ok()
            .and_then(|b| postcard::from_bytes::<Vec<Held>>(&b).ok())
            .unwrap_or_default()
            .into_iter()
            .map(|h| (h.sig, h))
            .collect();
        // Seules restent les cartes entières dans le magasin.
        index.retain(|_, h| {
            matches!(
                rt.block_on(store.blobs().status(Hash::from(h.hash))),
                Ok(BlobStatus::Complete { size }) if size == h.size
            )
        });
        let mask = EventMask {
            connected: ConnectMode::Intercept,
            throttle: ThrottleMode::Intercept,
            ..EventMask::DEFAULT
        };
        let (events, rx) = EventSender::channel(256, mask);
        let limits = Arc::new(Mutex::new(limits));
        let served = Arc::new(AtomicU64::new(0));
        rt.spawn(provide(rx, limits.clone(), served.clone()));
        let blobs = BlobsProtocol::new(&store, Some(events));
        let c = Arc::new(Self {
            store,
            blobs,
            ep,
            rt,
            dir: dir.to_path_buf(),
            limits,
            index: Mutex::new(index),
            gone: Mutex::new(Vec::new()),
            fetching,
            served,
        });
        c.save();
        Ok(c)
    }

    /// Une machine demande des cartes (connexion `ALPN`).
    pub(crate) async fn accept(&self, conn: iroh::endpoint::Connection) {
        if let Err(e) = self.blobs.accept(conn).await {
            log::debug!("cartes : {e}");
        }
    }

    /// Identifiant du nœud iroh qui sert ces cartes.
    pub fn node(&self) -> [u8; 32] {
        *self.ep.id().as_bytes()
    }

    pub fn set_limits(&self, limits: Limits) {
        *self.limits.lock().unwrap() = limits;
        let out = over_place(&self.index.lock().unwrap(), limits.place);
        self.forget(&out);
    }

    pub fn limits(&self) -> Limits {
        *self.limits.lock().unwrap()
    }

    /// Octets servis aux autres depuis le lancement.
    pub fn served(&self) -> u64 {
        self.served.load(Ordering::Relaxed)
    }

    /// Place encore libre pour des cartes (octets).
    pub fn room(&self) -> u64 {
        let used: u64 = self
            .index
            .lock()
            .unwrap()
            .values()
            .filter(|h| !h.external)
            .map(|h| h.size)
            .sum();
        self.limits().place.saturating_sub(used)
    }

    fn save(&self) {
        let v: Vec<Held> = self.index.lock().unwrap().values().cloned().collect();
        if let Ok(b) = postcard::to_allocvec(&v) {
            let _ = write_atomic(&self.dir.join(INDEX), &b);
        }
    }

    /// Les cartes gardées ici.
    pub fn held_maps(&self) -> Vec<MapHeld> {
        let mut v: Vec<MapHeld> = self.index.lock().unwrap().values().map(Held::map).collect();
        v.sort_by_key(|m| m.sig);
        v
    }

    /// Cette carte est-elle ici ? (Son empreinte et sa taille.)
    pub fn held_map(&self, sig: &[u8; 32]) -> Option<MapHeld> {
        self.index.lock().unwrap().get(sig).map(Held::map)
    }

    /// Cartes sorties depuis la dernière fois (place, relais qui les a effacées).
    pub fn take_gone(&self) -> Vec<[u8; 32]> {
        std::mem::take(&mut self.gone.lock().unwrap())
    }

    /// Une carte gardée ici, entière (elle compte comme servie maintenant).
    pub fn get_map(&self, sig: &[u8; 32]) -> Option<Vec<u8>> {
        let hash = {
            let mut idx = self.index.lock().unwrap();
            let h = idx.get_mut(sig)?;
            h.used = now();
            Hash::from(h.hash)
        };
        let b = self.rt.block_on(self.store.blobs().get_bytes(hash)).ok()?;
        (minelib_net::game::signature(&b) == *sig).then(|| b.to_vec())
    }

    fn insert(&self, h: Held) {
        let out = {
            let mut idx = self.index.lock().unwrap();
            idx.insert(h.sig, h);
            over_place(&idx, self.limits().place)
        };
        self.forget(&out);
        self.save();
    }

    /// Sort ces cartes (le ménage du magasin efface leurs octets ensuite).
    fn forget(&self, sigs: &[[u8; 32]]) {
        if sigs.is_empty() {
            return;
        }
        {
            let mut idx = self.index.lock().unwrap();
            for s in sigs {
                idx.remove(s);
            }
        }
        for s in sigs {
            let _ = self.rt.block_on(self.store.tags().delete(tag_name(s)));
        }
        self.gone.lock().unwrap().extend_from_slice(sigs);
        self.save();
    }

    /// Garde une carte qu'on a déjà entière (reçue, ou faite et envoyée par ce jeu). Rend
    /// son empreinte BLAKE3.
    pub fn keep_bytes(&self, sig: [u8; 32], bytes: &[u8]) -> Result<[u8; 32]> {
        if let Some(h) = self.index.lock().unwrap().get_mut(&sig) {
            h.used = now();
            return Ok(h.hash);
        }
        let size = bytes.len() as u64;
        anyhow::ensure!(size > 0 && size <= MAP_MAX, "carte trop grande");
        anyhow::ensure!(
            minelib_net::game::signature(bytes) == sig,
            "signature de la carte fausse"
        );
        let tag = self
            .rt
            .block_on(
                self.store
                    .blobs()
                    .add_bytes(bytes.to_vec())
                    .with_named_tag(tag_name(&sig)),
            )
            .map_err(|e| anyhow!("{e}"))?;
        let hash = *tag.hash.as_bytes();
        self.insert(Held {
            sig,
            hash,
            size,
            used: now(),
            external: false,
        });
        Ok(hash)
    }

    /// (Relais) Sert une carte de son dossier `maps` sans la copier. Le fichier ne doit
    /// plus changer : les cartes sont nommées par leur contenu.
    pub fn keep_file(&self, sig: [u8; 32], path: &Path) -> Result<[u8; 32]> {
        if let Some(h) = self.index.lock().unwrap().get(&sig) {
            if h.external {
                return Ok(h.hash);
            }
        }
        let size = std::fs::metadata(path)?.len();
        anyhow::ensure!(size > 0 && size <= MAP_MAX, "carte trop grande");
        let opts = AddPathOptions {
            path: path.to_path_buf(),
            format: BlobFormat::Raw,
            mode: ImportMode::TryReference,
        };
        let tag = self
            .rt
            .block_on(
                self.store
                    .blobs()
                    .add_path_with_opts(opts)
                    .with_named_tag(tag_name(&sig)),
            )
            .map_err(|e| anyhow!("{e}"))?;
        let hash = *tag.hash.as_bytes();
        self.insert(Held {
            sig,
            hash,
            size,
            used: now(),
            external: true,
        });
        Ok(hash)
    }

    /// (Relais) Les cartes de son dossier qui n'y sont plus ne sont plus servies.
    pub fn retain_external(&self, keep: &HashSet<[u8; 32]>) {
        let out: Vec<[u8; 32]> = self
            .index
            .lock()
            .unwrap()
            .values()
            .filter(|h| h.external && !keep.contains(&h.sig))
            .map(|h| h.sig)
            .collect();
        self.forget(&out);
    }

    /// Prend une carte aux machines `source.nodes` (dans l'ordre : les premières d'abord,
    /// `SOURCES_MAX` à la fois), et la garde ici si sa signature est bonne. `progress` est
    /// appelé de ce fil, de temps en temps, avec la part reçue.
    pub fn fetch_map(&self, source: &MapSource, progress: &mut dyn FnMut(f32)) -> Result<Vec<u8>> {
        let MapHeld { sig, hash, size } = source.map;
        if let Some(b) = self.get_map(&sig) {
            return Ok(b);
        }
        anyhow::ensure!(size > 0 && size <= MAP_MAX, "carte trop grande");
        let me = self.ep.id();
        let mut from: Vec<iroh::EndpointId> = Vec::new();
        for n in &source.nodes {
            if let Ok(id) = iroh::EndpointId::from_bytes(n) {
                if id != me && !from.contains(&id) {
                    from.push(id);
                }
            }
        }
        anyhow::ensure!(!from.is_empty(), "aucune machine n'a donné cette carte");
        let h = Hash::from(hash);
        self.fetching.lock().unwrap().insert(h);
        let done = Arc::new(AtomicU64::new(0));
        let store: iroh_blobs::api::Store = (*self.store).clone();
        let job = self.rt.spawn(fetch_from(
            store,
            self.ep.clone(),
            h,
            size,
            from,
            done.clone(),
        ));
        while !job.is_finished() {
            progress(done.load(Ordering::Relaxed) as f32 / size as f32);
            std::thread::sleep(Duration::from_millis(250));
        }
        let got = self
            .rt
            .block_on(job)
            .map_err(|e| anyhow!("{e}"))
            .and_then(|r| r)
            .and_then(|()| {
                let b = self
                    .rt
                    .block_on(self.store.blobs().get_bytes(h))
                    .map_err(|e| anyhow!("{e}"))?;
                anyhow::ensure!(
                    minelib_net::game::signature(&b) == sig,
                    "carte altérée (ce n'est pas celle demandée)"
                );
                Ok(b.to_vec())
            });
        if got.is_ok() {
            let tagged = self.rt.block_on(
                self.store
                    .tags()
                    .set(tag_name(&sig), iroh_blobs::HashAndFormat::raw(h)),
            );
            if tagged.is_ok() {
                self.insert(Held {
                    sig,
                    hash,
                    size,
                    used: now(),
                    external: false,
                });
            }
        }
        // (Une carte ratée ou fausse reste sans nom : le ménage l'efface.)
        self.fetching.lock().unwrap().remove(&h);
        got
    }
}

impl minelib_net::cartes::Peers for Cartes {
    fn node(&self) -> [u8; 32] {
        Cartes::node(self)
    }
    fn held(&self) -> Vec<MapHeld> {
        self.held_maps()
    }
    fn gone(&self) -> Vec<[u8; 32]> {
        self.take_gone()
    }
    fn room(&self) -> u64 {
        Cartes::room(self)
    }
    fn keep(&self, sig: [u8; 32], bytes: &[u8]) {
        if let Err(e) = self.keep_bytes(sig, bytes) {
            log::info!(
                "carte {} pas gardée : {e:#}",
                minelib_net::crypto::hex(&sig[..6])
            );
        }
    }
    fn fetch(&self, source: &MapSource, progress: &mut dyn FnMut(f32)) -> Result<Vec<u8>> {
        self.fetch_map(source, progress)
    }
}

/// Répond à ce que demandent les machines servies : combien à la fois (`Limits::conns`),
/// et à quel débit (`Limits::rate` ; la bibliothèque demande avant chaque bloc de 16 Kio).
async fn provide(
    mut rx: tokio::sync::mpsc::Receiver<ProviderMessage>,
    limits: Arc<Mutex<Limits>>,
    served: Arc<AtomicU64>,
) {
    let mut open: HashSet<u64> = HashSet::new();
    let mut pace = Pace::default();
    while let Some(msg) = rx.recv().await {
        match msg {
            ProviderMessage::ClientConnected(m) => {
                let max = limits.lock().unwrap().conns;
                let ok = open.len() < max;
                if ok {
                    open.insert(m.inner.connection_id);
                }
                let r = if ok {
                    Ok(())
                } else {
                    Err(AbortReason::RateLimited)
                };
                let _ = m.tx.send(r).await;
            }
            ProviderMessage::ConnectionClosed(m) => {
                open.remove(&m.inner.connection_id);
            }
            ProviderMessage::Throttle(m) => {
                served.fetch_add(m.inner.size, Ordering::Relaxed);
                let rate = limits.lock().unwrap().rate;
                let wait = pace.delay(m.inner.size, rate);
                tokio::spawn(async move {
                    if !wait.is_zero() {
                        tokio::time::sleep(wait).await;
                    }
                    let _ = m.tx.send(Ok(())).await;
                });
            }
            _ => {}
        }
    }
}

/// Débit d'envoi : chaque bloc part à son tour, `BURST` d'avance au plus.
#[derive(Default)]
struct Pace {
    next: Option<Instant>,
}

impl Pace {
    fn delay(&mut self, size: u64, rate: u64) -> Duration {
        self.delay_at(Instant::now(), size, rate)
    }

    fn delay_at(&mut self, now: Instant, size: u64, rate: u64) -> Duration {
        if rate == 0 {
            return Duration::ZERO;
        }
        let slack = Duration::from_secs_f64(BURST as f64 / rate as f64);
        let floor = now.checked_sub(slack).unwrap_or(now);
        let start = self.next.map_or(floor, |n| n.max(floor));
        self.next = Some(start + Duration::from_secs_f64(size as f64 / rate as f64));
        start.saturating_duration_since(now)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Piece {
    Todo,
    /// Demandé à une ou deux machines, depuis.
    Doing(u8, Instant),
    Done,
}

#[derive(Debug, PartialEq)]
enum Take {
    Piece(usize),
    /// Tout est en cours ailleurs : attendre (une machine peut encore échouer).
    Wait,
    Finished,
}

/// Les morceaux d'une carte en cours de téléchargement.
struct Plan {
    pieces: Vec<Piece>,
}

impl Plan {
    fn new(size: u64) -> Self {
        Self {
            pieces: vec![Piece::Todo; size.div_ceil(PIECE).max(1) as usize],
        }
    }

    fn finished(&self) -> bool {
        self.pieces.iter().all(|p| *p == Piece::Done)
    }

    fn take(&mut self, now: Instant) -> Take {
        if let Some(i) = self.pieces.iter().position(|p| *p == Piece::Todo) {
            self.pieces[i] = Piece::Doing(1, now);
            return Take::Piece(i);
        }
        let late = self
            .pieces
            .iter()
            .enumerate()
            .filter_map(|(i, p)| match p {
                Piece::Doing(1, t) if now.duration_since(*t) >= ENDGAME_AFTER => Some((i, *t)),
                _ => None,
            })
            .min_by_key(|(_, t)| *t);
        if let Some((i, t)) = late {
            self.pieces[i] = Piece::Doing(2, t);
            return Take::Piece(i);
        }
        if self.finished() {
            Take::Finished
        } else {
            Take::Wait
        }
    }

    /// Vrai la première fois que ce morceau arrive.
    fn done(&mut self, i: usize) -> bool {
        let first = self.pieces[i] != Piece::Done;
        self.pieces[i] = Piece::Done;
        first
    }

    fn failed(&mut self, i: usize) {
        if let Piece::Doing(n, t) = self.pieces[i] {
            self.pieces[i] = if n > 1 {
                Piece::Doing(n - 1, t)
            } else {
                Piece::Todo
            };
        }
    }
}

async fn fetch_from(
    store: iroh_blobs::api::Store,
    ep: iroh::Endpoint,
    hash: Hash,
    size: u64,
    from: Vec<iroh::EndpointId>,
    done: Arc<AtomicU64>,
) -> Result<()> {
    let plan = Arc::new(Mutex::new(Plan::new(size)));
    let mut queue: VecDeque<iroh::EndpointId> = from.into();
    let mut set = tokio::task::JoinSet::new();
    let start = |set: &mut tokio::task::JoinSet<()>, id| {
        set.spawn(source(
            store.clone(),
            ep.clone(),
            id,
            hash,
            size,
            plan.clone(),
            done.clone(),
        ));
    };
    for _ in 0..SOURCES_MAX {
        let Some(id) = queue.pop_front() else { break };
        start(&mut set, id);
    }
    while set.join_next().await.is_some() {
        if !plan.lock().unwrap().finished() {
            if let Some(id) = queue.pop_front() {
                start(&mut set, id);
            }
        }
    }
    match store
        .blobs()
        .status(hash)
        .await
        .map_err(|e| anyhow!("{e}"))?
    {
        BlobStatus::Complete { size: s } if s == size => Ok(()),
        BlobStatus::Complete { size: s } => Err(anyhow!("carte de {s} octets, {size} annoncés")),
        _ => Err(anyhow!("aucune machine n'a donné la carte en entier")),
    }
}

/// Une machine qui a la carte : ses morceaux, un à la fois, jusqu'à la fin ou au premier
/// échec (son morceau repart alors aux autres).
async fn source(
    store: iroh_blobs::api::Store,
    ep: iroh::Endpoint,
    id: iroh::EndpointId,
    hash: Hash,
    size: u64,
    plan: Arc<Mutex<Plan>>,
    done: Arc<AtomicU64>,
) {
    let conn = match tokio::time::timeout(CONNECT_WAIT, ep.connect(id, ALPN)).await {
        Ok(Ok(c)) => c,
        Ok(Err(e)) => {
            log::debug!("carte : {} injoignable ({e})", id.fmt_short());
            return;
        }
        Err(_) => {
            log::debug!("carte : {} ne répond pas", id.fmt_short());
            return;
        }
    };
    loop {
        let take = plan.lock().unwrap().take(Instant::now());
        let i = match take {
            Take::Piece(i) => i,
            Take::Wait => {
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
            Take::Finished => break,
        };
        let a = i as u64 * PIECE;
        let b = (a + PIECE).min(size);
        let req = GetRequest::blob_ranges(hash, ChunkRanges::bytes(a..b));
        let got = tokio::time::timeout(
            PIECE_WAIT,
            store.remote().execute_get(conn.clone(), req).complete(),
        )
        .await;
        let mut p = plan.lock().unwrap();
        match got {
            Ok(Ok(_)) => {
                if p.done(i) {
                    done.fetch_add(b - a, Ordering::Relaxed);
                }
            }
            _ => {
                p.failed(i);
                log::debug!("carte : {} a échoué au morceau {i}", id.fmt_short());
                break;
            }
        }
    }
    conn.close(0u32.into(), b"merci");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pieces_go_to_free_sources_and_slow_ones_are_doubled_at_the_end() {
        let t0 = Instant::now();
        let mut p = Plan::new(3 * PIECE - 10);
        assert_eq!(p.pieces.len(), 3);
        assert_eq!(p.take(t0), Take::Piece(0));
        assert_eq!(p.take(t0), Take::Piece(1));
        assert_eq!(p.take(t0), Take::Piece(2));
        // Tout est en cours depuis peu : on attend.
        assert_eq!(p.take(t0 + Duration::from_secs(1)), Take::Wait);
        assert!(p.done(1));
        // Le plus ancien en cours est demandé aussi à la machine libre.
        assert_eq!(p.take(t0 + ENDGAME_AFTER), Take::Piece(0));
        // Celle qui l'avait échoue : l'autre l'a encore.
        p.failed(0);
        assert_eq!(p.pieces[0], Piece::Doing(1, t0));
        assert!(p.done(0));
        assert!(!p.done(0), "un morceau reçu deux fois ne compte qu'une");
        // Une machine qui échoue seule rend son morceau.
        p.failed(2);
        assert_eq!(p.take(t0), Take::Piece(2));
        assert!(p.done(2));
        assert!(p.finished());
        assert_eq!(p.take(t0), Take::Finished);
    }

    #[test]
    fn the_send_rate_is_kept_after_a_burst() {
        let t0 = Instant::now();
        let mut p = Pace::default();
        let (rate, block) = (100_000, 16 * 1024);
        // D'avance : `BURST` sans attendre.
        let free = (BURST / block) as usize;
        for _ in 0..free {
            assert!(p.delay_at(t0, block, rate).is_zero());
        }
        // Ensuite, au débit : 50 blocs de plus ≈ 8 s à 100 Ko/s.
        let mut last = Duration::ZERO;
        for _ in 0..50 {
            last = p.delay_at(t0, block, rate);
        }
        let want = (50 * block) as f64 / rate as f64;
        assert!(
            (last.as_secs_f64() - want).abs() < 0.2,
            "{last:?} au lieu d'environ {want} s"
        );
        assert!(
            Pace::default().delay_at(t0, block, 0).is_zero(),
            "0 : sans limite"
        );
    }

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("minelib-cartes-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    /// Une carte gardée par deux machines est prise aux deux à la fois, vérifiée, puis
    /// gardée et servie à son tour ; une machine qui ne l'a pas est passée. Une carte qui
    /// n'est pas celle demandée (signature) n'est pas gardée.
    #[test]
    fn a_map_comes_from_every_machine_that_has_it() {
        let net = crate::LocalNet::new();
        let node = || crate::Node::start_local(&net).unwrap();
        let (a, b, c, d, e) = (node(), node(), node(), node(), node());
        let open =
            |n: &crate::Node, name: &str, l: Limits| n.share_maps(&scratch(name), l).unwrap();
        let ca = open(&a, "a", Limits::SERVER);
        let cb = open(&b, "b", Limits::SERVER);
        let cc = open(&c, "c", Limits::GAME);
        let cd = open(&d, "d", Limits::SERVER);
        let ce = open(&e, "e", Limits::GAME);
        // 1,3 Mo de bruit : six morceaux.
        let mut x = 0x1234_5678u32;
        let bytes: Vec<u8> = (0..1_300_000)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                x as u8
            })
            .collect();
        let sig = minelib_net::game::signature(&bytes);
        let hash = ca.keep_bytes(sig, &bytes).unwrap();
        assert_eq!(cb.keep_bytes(sig, &bytes).unwrap(), hash);
        assert_eq!(hash, *Hash::new(&bytes).as_bytes());
        assert!(
            cd.keep_bytes([7; 32], &bytes).is_err(),
            "une signature fausse n'est pas gardée"
        );
        let map = MapHeld {
            sig,
            hash,
            size: bytes.len() as u64,
        };
        let source = MapSource {
            map,
            nodes: vec![d.id(), a.id(), b.id()],
        };
        let mut seen = Vec::new();
        let got = cc.fetch_map(&source, &mut |f| seen.push(f)).unwrap();
        assert!(got == bytes, "la carte reçue n'est pas la même");
        assert!(
            ca.served() > 0 && cb.served() > 0,
            "les deux machines ont servi"
        );
        assert!(seen.windows(2).all(|w| w[0] <= w[1]));
        assert_eq!(cc.held_map(&sig), Some(map));
        assert!(cc.get_map(&sig).is_some_and(|b| b == bytes));
        // Elle la sert ensuite à son tour, à son débit : 400 Ko/s, 256 Kio d'avance.
        cc.set_limits(Limits {
            rate: 400_000,
            ..Limits::GAME
        });
        let again = MapSource {
            map,
            nodes: vec![c.id()],
        };
        let t0 = Instant::now();
        assert!(ce.fetch_map(&again, &mut |_| {}).unwrap() == bytes);
        let took = t0.elapsed().as_secs_f64();
        let want = (bytes.len() as u64 - BURST) as f64 / 400_000.0;
        assert!(took >= want * 0.8, "{took:.1} s : le débit n'est pas tenu");
        // Personne de joignable : une erreur, sans attendre longtemps.
        let t0 = Instant::now();
        let nobody = MapSource {
            map,
            nodes: vec![*iroh::SecretKey::from_bytes(&[3; 32]).public().as_bytes()],
        };
        assert!(cd.fetch_map(&nobody, &mut |_| {}).is_err());
        assert!(t0.elapsed() < CONNECT_WAIT + Duration::from_secs(5));
        // La même empreinte sous une autre signature : refusée, pas gardée.
        let lie = MapSource {
            map: MapHeld {
                sig: [9; 32],
                ..map
            },
            nodes: vec![a.id()],
        };
        assert!(cd.fetch_map(&lie, &mut |_| {}).is_err());
        assert_eq!(cd.held_map(&[9; 32]), None);
    }

    /// Hors de la place, la carte la moins récente sort et est dite partie.
    #[test]
    fn a_full_place_lets_the_oldest_map_go() {
        let n = crate::Node::start_local(&crate::LocalNet::new()).unwrap();
        let c = n
            .share_maps(
                &scratch("place"),
                Limits {
                    place: 250_000,
                    ..Limits::GAME
                },
            )
            .unwrap();
        let map = |k: u8| vec![k; 100_000];
        let sig = |k: u8| minelib_net::game::signature(&map(k));
        for k in 1..=3 {
            c.keep_bytes(sig(k), &map(k)).unwrap();
            std::thread::sleep(Duration::from_millis(1100));
        }
        assert_eq!(c.held_maps().len(), 2);
        assert_eq!(c.held_map(&sig(1)), None);
        assert_eq!(c.take_gone(), vec![sig(1)]);
        assert!(c.take_gone().is_empty());
        assert_eq!(c.room(), 50_000);
    }

    #[test]
    fn the_oldest_maps_leave_first_and_the_relays_never() {
        let held = |n: u8, size: u64, used: u64, external: bool| Held {
            sig: [n; 32],
            hash: [n; 32],
            size,
            used,
            external,
        };
        let idx: HashMap<[u8; 32], Held> = [
            held(1, 400, 10, false),
            held(2, 400, 30, false),
            held(3, 400, 20, false),
            held(4, 5000, 1, true),
        ]
        .into_iter()
        .map(|h| (h.sig, h))
        .collect();
        assert!(over_place(&idx, 1200).is_empty());
        assert_eq!(over_place(&idx, 800), vec![[1; 32]]);
        assert_eq!(over_place(&idx, 300), vec![[1; 32], [3; 32], [2; 32]]);
    }
}
