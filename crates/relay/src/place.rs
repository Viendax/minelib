//! (v349) La place des mondes sur ce relais.
//!
//! Le relais garde, pour chaque monde ouvert ou endormi, son entrée au registre (`mondes`),
//! ses réglages (`servers` : OP, bannis, liste blanche, heure, météo, point d'apparition,
//! positions des joueurs, graine et clé de terrain), les sacs de ses joueurs en survie
//! (`bags`) et, (v350) endormi, sa copie (`gardes` : sa carte et tout ce qui y a été fait).
//! Tout cela tient dans `place()` : `minelib-relay --place-mondes <Mo>`, ou `place_mondes_mo=`
//! dans les options du jeu qui tient le relais ; (v350) 1 Go par défaut (256 Mo en v349).
//!
//! Quand ça dépasse, le relais lâche d'abord les copies de mondes endormis où personne n'a
//! joué depuis au moins `IDLE_MIN_DAYS` jours, le moins récemment actif d'abord, jusqu'à ce que
//! ça tienne : ces mondes-là ne se réveillent plus que depuis le PC de celui qui en garde le
//! dernier état. S'il le faut encore, il oublie ces mondes eux-mêmes. Tant qu'il y a de la
//! place, tout reste, aussi longtemps qu'il faut : seuls les nouveaux mondes qui arrivent
//! poussent les anciens dehors. Un monde joué depuis moins longtemps n'est jamais touché ; si
//! ces mondes-là ne tiennent pas, le journal le dit. (v350) Les cartes d'avant des mondes (une
//! par ouverture) que plus rien ne demande sont effacées à chaque fois.
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
pub const DEFAULT_PLACE: u64 = 1_000 * 1_000_000;
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
    st.worlds.bytes_of(key) + st.servers.bytes_of(key) + st.bags.bytes_of(key) + kept_bytes(st, key)
}

/// (v350) Ce que prend la copie gardée du monde `key` : elle-même, et sa carte.
fn kept_bytes(st: &State, key: &str) -> u64 {
    st.kept
        .meta(key)
        .map_or(0, |m| m.size + st.maps.size_of(&m.map_sig))
}

/// (v350) Une carte reçue il y a moins de ce temps n'est jamais effacée (un monde est peut-être
/// en train de s'ouvrir avec elle).
const MAP_GRACE: Duration = Duration::from_secs(24 * 3600);

/// (v350) Efface les cartes que plus rien ne demande : ni carte FPS, ni celle d'une partie ou
/// d'un monde ouverts, ni celle d'un monde gardé.
pub(crate) fn sweep_maps(st: &mut State) -> (usize, u64) {
    let keep: std::collections::HashSet<[u8; 32]> = st
        .rooms
        .values()
        .map(|r| r.map_sig)
        .chain(st.kept.map_sigs())
        .collect();
    let (n, bytes) = st.maps.sweep(&keep, MAP_GRACE);
    if n > 0 {
        // (v352) Son nœud ne les sert plus.
        super::partage::maps_changed(st, Vec::new());
        log::info!(
            "place des mondes : {n} cartes d'avant effacées ({:.1} Mo), plus rien ne les demandait",
            bytes as f64 / 1e6
        );
    }
    (n, bytes)
}

/// À chaque entretien : une fois par heure, la place.
pub(crate) fn upkeep(st: &mut State) {
    if st.place_checked.elapsed() < CHECK_EVERY {
        return;
    }
    st.place_checked = Instant::now();
    sweep_maps(st);
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
    let open: std::collections::HashSet<String> = st
        .rooms
        .values()
        .filter(|r| r.kind == 1)
        .map(|r| r.server.clone())
        .collect();
    let mut old: Vec<&(String, u64, bool, u64)> = sizes
        .iter()
        .filter(|(k, active, asleep, _)| {
            *asleep && !open.contains(k) && now.saturating_sub(*active) >= IDLE_MIN_DAYS * 86_400
        })
        .collect();
    old.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)));
    // (v350) D'abord les copies gardées : le monde reste dans la liste, et celui qui en garde
    // le dernier état peut toujours le réveiller depuis son PC.
    let mut lighter = 0usize;
    for (k, _, _, _) in &old {
        if used <= budget {
            break;
        }
        let b = kept_bytes(st, k);
        if b > 0 {
            used = used.saturating_sub(b);
            st.kept.drop(k);
            lighter += 1;
        }
    }
    if lighter > 0 {
        log::info!(
            "place des mondes : copies de {lighter} mondes endormis lâchées (personne n'y avait \
             joué depuis {IDLE_MIN_DAYS} jours ou plus) ; ils se réveillent encore depuis le PC \
             de leur gardien"
        );
        sweep_maps(st);
    }
    let mut gone = Vec::new();
    for (k, _, _, b) in old {
        if used <= budget {
            break;
        }
        let b = b.saturating_sub(kept_bytes(st, k));
        used = used.saturating_sub(b);
        gone.push(k.clone());
    }
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
        st.kept.drop(k);
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
/// gardien lui sont dits (une fois), si son jeu sait les lire. (v350) Et ce que sait faire ce
/// relais (`proto::RELAY_LEVEL`), à un jeu qui sait le lire.
pub(crate) fn tell(st: &mut State, id: u64) {
    let Some(c) = st.conns.get(&id) else {
        return;
    };
    if c.version >= proto::RELAY_LEVEL_SINCE {
        transport::push(
            &c.tx,
            &Response::RelayLevel {
                level: proto::RELAY_LEVEL,
            },
        );
    }
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
