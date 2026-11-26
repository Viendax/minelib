//! (v352) Cartes partagées : qui garde quelle carte, pour qu'un jeu la prenne à toutes les
//! machines qui l'ont à la fois (`minelib_p2p::cartes`), le relais en dernier.
//!
//! Chaque jeu (et chaque passeur sans joueur) dit au relais les cartes qu'il garde et sert
//! (`Request::MapHave`), avec le nœud iroh qui les sert : celui de sa connexion par le
//! tunnel (authentifié par iroh, donné au relais par le préambule), pour que personne ne
//! fasse venir les téléchargements des autres chez un tiers. Le relais ne retient rien sur le disque : ce
//! qu'une connexion a dit part avec elle.
//!
//! Le relais sert aussi ses propres cartes par son nœud (`Handle::share_maps`), sans les
//! copier ; il les donne en dernier (`Request::MapWhere`), pour garder son débit à ceux
//! que personne d'autre ne peut servir. Les passeurs qui ont de la place prennent les
//! cartes FPS que peu de machines gardent (`Request::MapsToKeep`).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use minelib_net::proto::{MapHeld, MapSource, MAP_MAX};

use super::State;

/// Cartes retenues au plus par connexion.
pub(crate) const HELD_MAX: usize = 4096;
/// Machines données au plus pour une carte (le relais compris).
pub(crate) const NODES_MAX: usize = 12;
/// Une carte gardée par autant de machines n'est plus proposée aux passeurs.
const KEEPERS: usize = 3;
/// Cartes proposées au plus à un passeur à la fois.
const TO_KEEP_MAX: usize = 16;

/// `Request::MapHave` : retenu si `node` est bien le nœud de cette connexion.
pub(crate) fn have(
    st: &mut State,
    id: u64,
    node: [u8; 32],
    maps: Vec<MapHeld>,
    gone: Vec<[u8; 32]>,
) {
    let Some(c) = st.conns.get_mut(&id) else {
        return;
    };
    if c.tunnel_node != Some(node) {
        return;
    }
    c.map_node = Some(node);
    for sig in &gone {
        c.maps_held.remove(sig);
    }
    for m in maps {
        if m.size == 0 || m.size > MAP_MAX {
            continue;
        }
        if c.maps_held.len() >= HELD_MAX && !c.maps_held.contains_key(&m.sig) {
            break;
        }
        c.maps_held.insert(m.sig, m);
    }
}

/// Ordre pris au hasard (que les mêmes machines ne servent pas tout le monde).
fn shuffle(nodes: &mut [[u8; 32]]) {
    use rand::seq::SliceRandom;
    nodes.shuffle(&mut rand::thread_rng());
}

/// `Request::MapWhere` : les machines qui ont cette carte, sauf celle qui la demande, le
/// relais en dernier. L'empreinte est celle du relais s'il a la carte ; sinon celle que
/// disent le plus de machines (la signature vérifiée à la fin écarte une fausse).
pub(crate) fn sources(st: &State, id: u64, sig: &[u8; 32]) -> Option<MapSource> {
    let own = st
        .cartes
        .as_ref()
        .and_then(|c| c.held_map(sig).map(|m| (m, c.node())));
    let map = match own {
        Some((m, _)) => m,
        None => {
            let mut said: HashMap<MapHeld, usize> = HashMap::new();
            for c in st.conns.values() {
                if let (Some(_), Some(m)) = (c.map_node, c.maps_held.get(sig)) {
                    *said.entry(*m).or_default() += 1;
                }
            }
            said.into_iter()
                .max_by_key(|(m, n)| (*n, m.hash))
                .map(|(m, _)| m)?
        }
    };
    let asker = st.conns.get(&id).and_then(|c| c.map_node);
    let relay = own.map(|(_, n)| n);
    let mut nodes: Vec<[u8; 32]> = st
        .conns
        .iter()
        .filter(|(cid, _)| **cid != id)
        .filter_map(|(_, c)| {
            let n = c.map_node?;
            (c.maps_held.get(sig) == Some(&map)).then_some(n)
        })
        .filter(|n| Some(*n) != asker && Some(*n) != relay)
        .collect();
    nodes.sort_unstable();
    nodes.dedup();
    shuffle(&mut nodes);
    nodes.truncate(NODES_MAX - usize::from(relay.is_some()));
    nodes.extend(relay);
    Some(MapSource { map, nodes })
}

/// Combien de machines (en ligne) gardent cette carte, le relais non compris.
fn keepers(st: &State, sig: &[u8; 32]) -> usize {
    st.conns
        .values()
        .filter(|c| c.map_node.is_some() && c.maps_held.contains_key(sig))
        .count()
}

/// `Request::MapsToKeep` : les cartes FPS proposées à tous que moins de `KEEPERS`
/// machines gardent, les plus jouées d'abord, dans la place libre du passeur.
pub(crate) fn to_keep(st: &State, id: u64, room: u64) -> Vec<MapSource> {
    let Some(cartes) = st.cartes.as_ref() else {
        return Vec::new();
    };
    let mine: HashSet<[u8; 32]> = st
        .conns
        .get(&id)
        .map(|c| c.maps_held.keys().copied().collect())
        .unwrap_or_default();
    let mut list: Vec<(u32, MapHeld)> = st
        .maps
        .index
        .values()
        .filter(|m| st.maps.listed(&m.sig) && !mine.contains(&m.sig))
        .filter_map(|m| cartes.held_map(&m.sig).map(|h| (m.plays, h)))
        .filter(|(_, h)| keepers(st, &h.sig) < KEEPERS)
        .collect();
    list.sort_by_key(|(plays, h)| (std::cmp::Reverse(*plays), h.sig));
    let mut left = room;
    let mut out = Vec::new();
    for (_, h) in list {
        if out.len() >= TO_KEEP_MAX {
            break;
        }
        if h.size > left {
            continue;
        }
        if let Some(s) = sources(st, id, &h.sig) {
            left -= h.size;
            out.push(s);
        }
    }
    out
}

/// Sert ces cartes du dossier `maps` du relais par son nœud (dans un fil à part : chacune
/// est lue en entier une fois pour son empreinte). Celles qui n'y sont plus ne sont plus
/// servies.
pub(crate) fn serve_files(
    cartes: Arc<minelib_p2p::cartes::Cartes>,
    dir: std::path::PathBuf,
    all: HashSet<[u8; 32]>,
    new: Vec<[u8; 32]>,
) {
    let _ = std::thread::Builder::new()
        .name("cartes du relais".into())
        .spawn(move || {
            cartes.retain_external(&all);
            let mut n = 0;
            for sig in new {
                let path = dir.join(super::maps::file(&sig));
                match cartes.keep_file(sig, &path) {
                    Ok(_) => n += 1,
                    Err(e) => log::warn!(
                        "carte {} pas servie par le nœud : {e:#}",
                        minelib_net::crypto::hex(&sig[..6])
                    ),
                }
            }
            if n > 0 {
                log::info!("cartes : {n} servies aussi par le nœud du relais");
            }
        });
}

/// Les cartes du relais ont changé (une reçue, d'autres effacées) : son nœud suit.
pub(crate) fn maps_changed(st: &State, new: Vec<[u8; 32]>) {
    let Some(c) = st.cartes.clone() else {
        return;
    };
    let all: HashSet<[u8; 32]> = st.maps.index.keys().copied().collect();
    serve_files(c, st.maps.dir().to_path_buf(), all, new);
}
