//! (v349) La place des mondes sur ce relais.
//!
//! Le relais ne garde pas les blocs d'un monde : le jeu de celui qui en garde le dernier état
//! les a. Il garde, pour chaque monde ouvert ou endormi, son entrée au registre (`mondes`),
//! ses réglages (`servers` : OP, bannis, liste blanche, heure, météo, point d'apparition,
//! positions des joueurs, graine et clé de terrain) et les sacs de ses joueurs en survie
//! (`bags`). Tout cela tient dans `place()` : `minelib-relay --place-mondes <Mo>`, ou
//! `place_mondes_mo=` dans les options du jeu qui tient le relais ; 256 Mo par défaut.
//!
//! Quand ça dépasse, le relais oublie des mondes endormis où personne n'a joué depuis au moins
//! `IDLE_MIN_DAYS` jours, le moins récemment actif d'abord, jusqu'à ce que ça tienne. Tant
//! qu'il y a de la place, ils restent, aussi longtemps qu'il faut : seuls les nouveaux mondes
//! qui arrivent en poussent dehors. Un monde joué depuis moins longtemps n'est jamais oublié ;
//! si ces mondes-là ne tiennent pas, le journal le dit.
//!
//! Un monde oublié ne disparaît pas pour ceux qui y ont joué : son créateur et celui qui en
//! garde le dernier état l'apprennent à leur prochaine connexion (`Response::WorldsForgotten`,
//! avec la clé de son terrain, et sa graine pour son créateur). Leur jeu fait alors de sa
//! copie un monde solo, complet, à rouvrir en ligne quand ils veulent.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use minelib_net::proto::{self, Response};
use serde::{Deserialize, Serialize};

use super::{mondes, transport, State};

/// Place donnée aux mondes par défaut (octets).
pub const DEFAULT_PLACE: u64 = 256 * 1_000_000;
/// Un monde endormi n'est oublié qu'après tant de jours sans joueurs.
pub const IDLE_MIN_DAYS: u64 = 30;
/// La place est regardée au plus toutes les…
const CHECK_EVERY: Duration = Duration::from_secs(3600);
/// Mondes oubliés dont on garde la trace (pour le dire à leur créateur et à leur gardien) :
/// combien au plus, et combien de jours.
const KEPT: usize = 20_000;
const KEPT_DAYS: u64 = 365;
/// Un jeu d'avant ne connaît pas `Response::WorldsForgotten`.
const TOLD_FROM_VERSION: u32 = 349;

static PLACE: AtomicU64 = AtomicU64::new(DEFAULT_PLACE);

/// La place donnée aux mondes de ce relais (octets ; 1 Mo au moins).
pub fn set_place(bytes: u64) {
    PLACE.store(bytes.max(1_000_000), Ordering::Relaxed);
}

pub fn place() -> u64 {
    PLACE.load(Ordering::Relaxed)
}

/// Un monde oublié, gardé jusqu'à ce que son créateur et son gardien l'aient appris.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Forgotten {
    pub key: String,
    pub name: String,
    pub creator: String,
    pub keeper: String,
    pub idle_days: u32,
    /// Oublié quand (secondes depuis 1970).
    pub at: u64,
    pub seed: u64,
    pub terrain: u64,
    /// À qui c'est déjà dit (minuscules).
    pub told: Vec<String>,
}

impl Forgotten {
    fn concerns(&self, pseudo: &str) -> bool {
        self.creator.eq_ignore_ascii_case(pseudo) || self.keeper.eq_ignore_ascii_case(pseudo)
    }

    fn all_told(&self) -> bool {
        [&self.creator, &self.keeper]
            .iter()
            .all(|p| p.is_empty() || self.told.iter().any(|t| t.eq_ignore_ascii_case(p)))
    }
}

/// Les mondes oubliés (`mondes_oublies.bin`).
pub struct Store {
    path: PathBuf,
    list: Vec<Forgotten>,
}

impl Store {
    pub fn open(path: PathBuf) -> Self {
        let list = std::fs::read(&path)
            .ok()
            .and_then(|b| postcard::from_bytes(&b).ok())
            .unwrap_or_default();
        Self { path, list }
    }

    fn save(&self) {
        if let Ok(b) = postcard::to_allocvec(&self.list) {
            let _ = crate::accounts::write_atomic(&self.path, &b);
        }
    }

    fn push(&mut self, f: Forgotten) {
        let now = f.at;
        self.list.retain(|o| o.key != f.key);
        self.list.push(f);
        self.list
            .retain(|o| now.saturating_sub(o.at) < KEPT_DAYS * 86_400);
        if self.list.len() > KEPT {
            let extra = self.list.len() - KEPT;
            self.list.drain(..extra);
        }
    }

    /// Ce qu'il faut dire à `pseudo` (créateur ou gardien), noté comme dit.
    pub fn take_for(&mut self, pseudo: &str) -> Vec<Forgotten> {
        let mut out = Vec::new();
        let me = pseudo.to_ascii_lowercase();
        for f in self.list.iter_mut().filter(|f| f.concerns(pseudo)) {
            if !f.told.contains(&me) {
                f.told.push(me.clone());
                out.push(f.clone());
            }
        }
        if !out.is_empty() {
            self.list.retain(|f| !f.all_told());
            self.save();
        }
        out
    }

    #[cfg(test)]
    pub fn list(&self) -> &[Forgotten] {
        &self.list
    }
}

/// Ce que le monde `key` prend chez ce relais (octets, à peu près : comme écrit sur le disque).
pub(crate) fn bytes_of(st: &State, key: &str) -> u64 {
    st.worlds.bytes_of(key) + st.servers.bytes_of(key) + st.bags.bytes_of(key)
}

/// À chaque entretien : une fois par heure, la place.
pub(crate) fn upkeep(st: &mut State) {
    if st.place_checked.elapsed() < CHECK_EVERY {
        return;
    }
    st.place_checked = Instant::now();
    free(st, mondes::now());
}

/// Oublie des mondes endormis depuis au moins `IDLE_MIN_DAYS` jours, le moins récemment actif
/// d'abord, jusqu'à ce que tout tienne dans `place()`. Rend leurs clés.
pub(crate) fn free(st: &mut State, now: u64) -> Vec<String> {
    free_to(st, now, place())
}

/// Comme `free`, pour tenir dans `budget` octets.
pub(crate) fn free_to(st: &mut State, now: u64, budget: u64) -> Vec<String> {
    let sizes: Vec<(String, u64, bool, u64)> = st
        .worlds
        .all()
        .map(|w| {
            (
                w.key.clone(),
                w.active,
                w.asleep.is_some(),
                bytes_of(st, &w.key),
            )
        })
        .collect();
    let mut used: u64 = sizes.iter().map(|s| s.3).sum();
    if used <= budget {
        return Vec::new();
    }
    let open: std::collections::HashSet<&str> = st
        .rooms
        .values()
        .filter(|r| r.kind == 1)
        .map(|r| r.server.as_str())
        .collect();
    let mut old: Vec<&(String, u64, bool, u64)> = sizes
        .iter()
        .filter(|(k, active, asleep, _)| {
            *asleep
                && !open.contains(k.as_str())
                && now.saturating_sub(*active) >= IDLE_MIN_DAYS * 86_400
        })
        .collect();
    old.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)));
    let mut gone = Vec::new();
    for (k, _, _, b) in old {
        if used <= budget {
            break;
        }
        used = used.saturating_sub(*b);
        gone.push(k.clone());
    }
    drop(open);
    forget(st, &gone, now);
    if used > budget {
        log::warn!(
            "place des mondes : {:.1} Mo sur {:.1} Mo, et plus aucun monde endormi depuis {} \
             jours à oublier (--place-mondes pour en donner plus)",
            used as f64 / 1e6,
            budget as f64 / 1e6,
            IDLE_MIN_DAYS
        );
    }
    gone
}

/// Oublie ces mondes (registre, réglages, sacs), en gardant de quoi le dire à leur créateur
/// et à leur gardien.
fn forget(st: &mut State, keys: &[String], now: u64) {
    if keys.is_empty() {
        return;
    }
    let mut days = (u32::MAX, 0u32);
    for k in keys {
        let Some(w) = st.worlds.get(k).cloned() else {
            continue;
        };
        let seed = st.servers.world_seed(k);
        let idle_days = (now.saturating_sub(w.active) / 86_400) as u32;
        days = (days.0.min(idle_days), days.1.max(idle_days));
        st.forgotten.push(Forgotten {
            key: w.key,
            name: w.name,
            creator: w.creator,
            keeper: w.keeper,
            idle_days,
            at: now,
            seed: seed.seed,
            terrain: seed.terrain,
            told: Vec::new(),
        });
    }
    st.forgotten.save();
    st.worlds.delete_many(keys);
    st.servers.forget_many(keys);
    for k in keys {
        st.bags.forget(k);
    }
    log::info!(
        "place des mondes : {} mondes endormis oubliés (personne n'y avait joué depuis {} à {} \
         jours) ; leur créateur et leur gardien l'apprendront à leur prochaine connexion",
        keys.len(),
        days.0,
        days.1
    );
}

/// Une connexion vient d'entrer : les mondes oubliés dont son compte est le créateur ou le
/// gardien lui sont dits (une fois), si son jeu sait les lire.
pub(crate) fn tell(st: &mut State, id: u64) {
    let Some(c) = st.conns.get(&id) else {
        return;
    };
    if c.version < TOLD_FROM_VERSION || c.garant_only {
        return;
    }
    let Some(pseudo) = c.pseudo.clone() else {
        return;
    };
    let list = st.forgotten.take_for(&pseudo);
    if list.is_empty() {
        return;
    }
    let worlds = list
        .into_iter()
        .map(|f| proto::ForgottenWorld {
            world: f.key,
            name: f.name,
            idle_days: f.idle_days,
            terrain: f.terrain,
            // (La graine à son créateur seulement, comme aux OP.)
            seed: if f.creator.eq_ignore_ascii_case(&pseudo) {
                f.seed
            } else {
                0
            },
        })
        .collect();
    if let Some(c) = st.conns.get(&id) {
        transport::push(&c.tx, &Response::WorldsForgotten { worlds });
    }
}
