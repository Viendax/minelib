//! Provinces (v138) : le relais du monde confie une province peuplée (`proto::PROVINCE`
//! × `proto::PROVINCE` régions) au relais d'un joueur qui n'y joue pas, sous bail. Ce
//! relais de province y décide des blocs : modifications des joueurs qu'aucun relais de
//! région ne sert, lots des régions souveraines (époques, confirmations, vérifications),
//! rattrapage ; les relais des régions de la province se rattachent à lui. Le relais du
//! monde garde le reste (objets, coffres, comptes, choix des hôtes) et une copie de ce que
//! publie la province, pour la reprendre aussitôt si son hôte tombe.
//!
//! Jamais deux versions d'une province : le relais de province ne sert que pendant son
//! bail (demandé toutes les `PROVINCE_RENEW_MS`, valable `PROVINCE_LEASE_MS` à partir de
//! la demande), et le relais du monde ne la reprend que `PROVINCE_MARGIN_MS` après la fin
//! du dernier bail qu'il a pu donner. Un monde en pause ne renouvelle aucun bail.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use ed25519_dalek::VerifyingKey;
use minelib_net::proto::{self, ErrorCode, ProvinceInfo, Request, Response};
use minelib_net::transport;

use super::{
    err, guards, leave_room, open_room, push_zone, regions_in_view, spill_chest, PlayersIndex,
    RegionShard, Session, State,
};

/// Plage de rangs réservée à une province (elle n'en verra jamais la fin).
const PROVINCE_SEQS: u64 = 1 << 40;
/// Une province vide depuis ce temps revient au relais du monde.
const EMPTY_FOR: Duration = Duration::from_secs(60);
/// Un hôte qui n'est pas prêt dans ce temps est abandonné (un autre sera choisi).
const START_WAIT: Duration = Duration::from_secs(30);
/// Joueurs à partir desquels une province reçoit son relais.
pub(crate) const PROVINCE_FROM: usize = 64;
/// Relais de province : battement à ses relais de région et à ses joueurs (ms).
const BEAT_MS: u64 = 500;
/// Historique envoyé par paquets de… entrées.
const SEED_CHUNK: usize = 4096;
/// Relais de province : ce qu'il publie part au relais du monde toutes les… (ms), ou dès
/// qu'il y en a `SEED_CHUNK`.
const ARCHIVE_MS: u64 = 100;

/// Relais du monde : une province confiée (ou en train de l'être).
pub(crate) struct ProvinceRef {
    /// Connexion de garant de son hôte.
    pub host: u64,
    pub epoch: u64,
    /// Prêt : où le joindre (annoncé aux joueurs). Avant, ce relais sert encore la
    /// province.
    pub info: Option<ProvinceInfo>,
    /// Tâche envoyée.
    since: Instant,
    /// Dernier renouvellement du bail reçu (et accordé).
    renewed: Instant,
    /// Dernier rang de l'historique envoyé avec la tâche (la suite part quand il est
    /// prêt).
    snap_seq: u64,
    /// Plus aucun joueur dans la province depuis.
    empty_since: Option<Instant>,
}

/// Relais de province : la province servie par cette salle.
pub(crate) struct ProvinceRoom {
    pub province: [i32; 2],
    pub size: i32,
    pub epoch: u64,
    /// Clé du relais du monde (certificats des comptes, règles des régions souveraines).
    pub root: VerifyingKey,
    /// Rangs donnés par le relais du monde : il peut servir (avec un bail).
    go: bool,
    /// Fin du bail (heure de ce relais, voir `guards::now_ms`), dernière demande.
    lease_until: u64,
    last_ask: u64,
    /// Servait-elle au dernier passage, dernier battement.
    serving: bool,
    last_beat: u64,
    /// Relais de région prêts annoncés au relais du monde.
    ready_sent: Vec<([i32; 2], u16)>,
    /// Arrêtée (rendue, ou reprise par le relais du monde).
    stopped: bool,
    /// Certificats des joueurs entrés (pseudo en minuscules) : ils accompagnent leurs
    /// modifications signées transmises à l'hôte principal d'une région souveraine.
    certs: HashMap<String, minelib_net::sovereign::Cert>,
    /// Ce qui attend de partir au relais du monde, numéro du prochain envoi, dernier envoi
    /// (heure).
    outbox: Vec<([i32; 2], u64, Vec<u8>)>,
    next_n: u64,
    last_archive: u64,
}

impl ProvinceRoom {
    /// Une région de la province.
    pub fn has(&self, region: [i32; 2]) -> bool {
        proto::province_of(region, self.size) == self.province
    }

    /// Sert-elle en ce moment (bail en cours) ?
    pub fn serving(&self, now: u64) -> bool {
        self.go && !self.stopped && now < self.lease_until
    }
}

fn zone_region(zone: [i32; 2]) -> [i32; 2] {
    proto::region_of([zone[0] * proto::ZONE, zone[1] * proto::ZONE])
}

/// Taille des provinces de ce relais (régions par côté).
pub(crate) fn size(st: &State) -> i32 {
    st.province_size
        .load(std::sync::atomic::Ordering::Relaxed)
        .max(1) as i32
}

// ───────────────────────── relais du monde ─────────────────────────

/// Relais du monde : la région `region` est servie par un relais de province (prêt).
pub(crate) fn delegated(st: &State, rid: u64, region: [i32; 2]) -> bool {
    st.rooms.get(&rid).is_some_and(|room| {
        !room.delegated.is_empty()
            && room
                .delegated
                .contains(&proto::province_of(region, size(st)))
    })
}

/// Relais du monde : les provinces servies par un relais de province (pour en tester
/// beaucoup de régions sans tout rechercher à chaque fois) : taille et provinces.
pub(crate) fn delegated_set(
    st: &State,
    rid: u64,
) -> Option<(i32, std::sync::Arc<HashSet<[i32; 2]>>)> {
    let room = st.rooms.get(&rid)?;
    (!room.delegated.is_empty()).then(|| (size(st), room.delegated.clone()))
}

/// Les provinces prêtes ont changé : rangées à part (voir `Room::delegated`).
fn refresh_delegated(room: &mut super::Room) {
    room.delegated = std::sync::Arc::new(
        room.provinces
            .iter()
            .filter(|(_, p)| p.info.is_some())
            .map(|(k, _)| *k)
            .collect(),
    );
}

/// Région d'une province de `set` (voir `delegated_set`).
pub(crate) fn in_set(
    set: &Option<(i32, std::sync::Arc<HashSet<[i32; 2]>>)>,
    region: [i32; 2],
) -> bool {
    set.as_ref()
        .is_some_and(|(size, s)| s.contains(&proto::province_of(region, *size)))
}

/// Relais du monde : la province de cette région est en train d'être confiée (pas
/// encore prête) : on ne lui crée pas de relais de région rattaché ici.
pub(crate) fn starting(st: &State, rid: u64, region: [i32; 2]) -> bool {
    st.rooms.get(&rid).is_some_and(|room| {
        room.provinces
            .get(&proto::province_of(region, size(st)))
            .is_some_and(|p| p.info.is_none())
    })
}

/// Relais du monde : où se rattache le relais de rang `slot` de la région `region` (confié
/// à `pseudo`), si sa province est servie par un relais de province.
pub(crate) fn shard_at(
    st: &State,
    rid: u64,
    region: [i32; 2],
    slot: u16,
    pseudo: &str,
) -> Option<proto::ProvinceAt> {
    let room = st.rooms.get(&rid)?;
    let p = room.provinces.get(&proto::province_of(region, size(st)))?;
    let info = p.info.clone()?;
    let signer = super::registry_key(st, pseudo).unwrap_or([0; 32]);
    Some(proto::ProvinceAt {
        token: proto::province_token(
            &room.shard_secret,
            rid,
            region,
            slot,
            pseudo,
            &signer,
            p.epoch,
        ),
        info,
    })
}

/// Relais du monde, entretien (toutes les 2 s) : les provinces assez peuplées sont
/// confiées à un relais de province, les provinces vides depuis `EMPTY_FOR` reviennent.
pub(crate) fn upkeep(st: &mut State, rid: u64, index: &mut Option<PlayersIndex>) {
    let size = size(st);
    let from = st
        .province_from
        .load(std::sync::atomic::Ordering::Relaxed)
        .max(1);
    let Some(room) = st.rooms.get(&rid) else {
        return;
    };
    // (Une province ne se découpe pas elle-même ; un monde en survie garde tout au relais
    // du monde : les sacs ne passent pas encore d'un relais à l'autre.)
    if room.kind != 1 || room.province.is_some() || guards::frozen(st, rid) {
        return;
    }
    let survival = st.servers.default_gamemode(&room.server).is_some_and(|m| {
        matches!(
            m.trim().to_lowercase().as_str(),
            "0" | "s" | "survival" | "survie"
        )
    });
    let mut count: HashMap<[i32; 2], usize> = HashMap::new();
    for (id, chunk) in &room.build.positions {
        if !room.hidden.contains(id) {
            *count
                .entry(proto::province_of(proto::region_of(*chunk), size))
                .or_default() += 1;
        }
    }
    if room.provinces.is_empty() && count.values().all(|&n| n < from) {
        return;
    }
    // Provinces vides depuis longtemps, hôtes qui ne sont pas prêts à temps, hôtes dont le
    // joueur est entré dans la province qu'il sert (il ne décide pas pour lui-même).
    if !room.provinces.is_empty() && index.is_none() {
        *index = Some(super::players_index(st));
    }
    let inside: Vec<[i32; 2]> = match index.as_ref() {
        Some(index) => room
            .provinces
            .iter()
            .filter(|(p, r)| {
                st.conns
                    .get(&r.host)
                    .and_then(|c| c.pseudo.as_deref())
                    .and_then(|name| index.get(&name.to_lowercase()))
                    .filter(|(_, own)| *own == Some(rid))
                    .and_then(|(player, _)| room.build.positions.get(player))
                    .is_some_and(|c| proto::province_of(proto::region_of(*c), size) == **p)
            })
            .map(|(p, _)| *p)
            .collect(),
        None => Vec::new(),
    };
    let mut back = Vec::new();
    let room = st.rooms.get_mut(&rid).expect("partie");
    for (p, r) in room.provinces.iter_mut() {
        if inside.contains(p) {
            back.push(*p);
            continue;
        }
        if r.info.is_none() && r.since.elapsed() > START_WAIT {
            back.push(*p);
            continue;
        }
        if count.get(p).copied().unwrap_or(0) == 0 {
            let since = *r.empty_since.get_or_insert_with(Instant::now);
            if since.elapsed() > EMPTY_FOR {
                back.push(*p);
            }
        } else {
            r.empty_since = None;
        }
    }
    for p in back {
        stop(st, rid, p, "vide ou pas prête");
    }
    if survival {
        return;
    }
    let mut wanted: Vec<[i32; 2]> = count
        .iter()
        .filter(|(p, &n)| n >= from && !st.rooms[&rid].provinces.contains_key(*p))
        .map(|(p, _)| *p)
        .collect();
    wanted.sort();
    if wanted.is_empty() {
        return;
    }
    if index.is_none() {
        *index = Some(super::players_index(st));
    }
    let index = index.as_ref().expect("index");
    for p in wanted {
        let outside = move |chunk: [i32; 2]| proto::province_of(proto::region_of(chunk), size) != p;
        let Some(g) = super::pick_host(st, index, rid, &outside) else {
            break;
        };
        delegate(st, rid, p, size, g);
    }
}

/// Confie la province `p` à la connexion de garant `g` : tâche, et historique des blocs
/// de ses zones.
fn delegate(st: &mut State, rid: u64, p: [i32; 2], size: i32, g: u64) {
    let root = st.cert_key.verifying_key().to_bytes();
    let Some(room) = st.rooms.get_mut(&rid) else {
        return;
    };
    let epoch = room.next_epoch;
    room.next_epoch += 1;
    room.members.push(g);
    room.hidden.insert(g);
    let snap_seq = room.build.last_seq();
    let keep = |zone: [i32; 2]| proto::province_of(zone_region(zone), size) == p;
    let entries = room.build.blocks_after(keep, 0);
    let info = proto::StandbyInfo {
        name: room.name.clone(),
        map: room.map.clone(),
        map_sig: room.map_sig,
        max_players: room.max_players,
        creator: room.creator.clone(),
        server: room.server.clone(),
        owner: room.host_pseudo.clone(),
    };
    let secret = room.shard_secret;
    room.provinces.insert(
        p,
        ProvinceRef {
            host: g,
            epoch,
            info: None,
            since: Instant::now(),
            renewed: Instant::now(),
            snap_seq,
            empty_since: None,
        },
    );
    let Some(c) = st.conns.get_mut(&g) else {
        return;
    };
    c.room = Some(rid);
    transport::push(
        &c.tx,
        &Response::ProvinceTask {
            room: rid,
            province: p,
            size,
            epoch,
            secret,
            root,
            info,
        },
    );
    for part in entries.chunks(SEED_CHUNK) {
        transport::push(
            &c.tx,
            &Response::ProvinceSeed {
                room: rid,
                province: p,
                epoch,
                entries: part.to_vec(),
                go: None,
            },
        );
    }
    log::info!("monde {rid} : province {p:?} confiée au relais de {g} (époque {epoch})");
}

/// Relais du monde : l'hôte de la province est prêt. La suite de l'historique lui part,
/// avec sa plage de rangs ; il sert dès son premier bail. Les relais de région de la
/// province rattachés ici sont relevés (les suivants se rattacheront à lui), et les joueurs
/// qui l'ont en vue l'apprennent.
#[allow(clippy::too_many_arguments)]
pub(crate) fn ready(
    st: &mut State,
    rid: u64,
    id: u64,
    p: [i32; 2],
    epoch: u64,
    node: [u8; 32],
    addr: String,
    key: [u8; 32],
) {
    let size = size(st);
    let Some(room) = st.rooms.get_mut(&rid) else {
        return;
    };
    let Some(r) = room
        .provinces
        .get_mut(&p)
        .filter(|r| r.host == id && r.epoch == epoch && r.info.is_none())
    else {
        return;
    };
    let after = r.snap_seq;
    let keep = |zone: [i32; 2]| proto::province_of(zone_region(zone), size) == p;
    let delta = room.build.blocks_after(keep, after);
    let base = room.build.reserve(PROVINCE_SEQS);
    let r = room.provinces.get_mut(&p).expect("province");
    r.info = Some(ProvinceInfo {
        province: p,
        size,
        epoch,
        node,
        addr,
        key,
    });
    r.renewed = Instant::now();
    refresh_delegated(room);
    if let Some(c) = st.conns.get(&id) {
        let mut parts: Vec<&[super::zones::Entry]> = delta.chunks(SEED_CHUNK).collect();
        if parts.is_empty() {
            parts.push(&[]);
        }
        let n = parts.len();
        for (i, part) in parts.into_iter().enumerate() {
            transport::push(
                &c.tx,
                &Response::ProvinceSeed {
                    room: rid,
                    province: p,
                    epoch,
                    entries: part.to_vec(),
                    go: (i + 1 == n).then_some(base),
                },
            );
        }
    }
    // Ses régions : plus d'époque ni de relais rattachés ici.
    let room = st.rooms.get_mut(&rid).expect("partie");
    let regions: Vec<[i32; 2]> = room
        .shards
        .keys()
        .chain(room.sov.keys())
        .filter(|r| proto::province_of(**r, size) == p)
        .copied()
        .collect();
    let mut relieved = Vec::new();
    for r in &regions {
        relieved.extend(room.shards.remove(r).into_iter().flatten().map(|s| s.host));
        room.sov.remove(r);
        room.shards_dirty.insert(*r);
    }
    for h in relieved {
        leave_room(st, h);
        if let Some(c) = st.conns.get(&h) {
            transport::push(&c.tx, &Response::RoomClosed);
        }
    }
    log::info!("monde {rid} : province {p:?} servie par son relais (époque {epoch})");
    announce(st, rid, p);
}

/// Relais du monde : renouvellement du bail d'une province.
pub(crate) fn lease(st: &mut State, rid: u64, id: u64, p: [i32; 2], epoch: u64, t: u64) {
    let paused = guards::frozen(st, rid);
    let Some(room) = st.rooms.get_mut(&rid) else {
        return;
    };
    let ok = match room
        .provinces
        .get_mut(&p)
        .filter(|r| r.host == id && r.epoch == epoch && r.info.is_some())
    {
        Some(r) if !paused => {
            r.renewed = Instant::now();
            true
        }
        _ => false,
    };
    if let Some(c) = st.conns.get(&id) {
        transport::push(
            &c.tx,
            &Response::ProvinceLeased {
                room: rid,
                province: p,
                epoch,
                t,
                ok,
            },
        );
    }
}

/// Relais du monde : ce que la province vient de publier, gardé ici (sans le diffuser :
/// elle l'a fait). Un coffre cassé se vide ici (les objets restent au relais du monde).
#[allow(clippy::too_many_arguments)]
pub(crate) fn archive(
    st: &mut State,
    rid: u64,
    id: u64,
    p: [i32; 2],
    epoch: u64,
    n: u64,
    entries: Vec<([i32; 2], u64, Vec<u8>)>,
) {
    let size = size(st);
    let Some(room) = st.rooms.get_mut(&rid) else {
        return;
    };
    if !room
        .provinces
        .get(&p)
        .is_some_and(|r| r.host == id && r.epoch == epoch && r.info.is_some())
    {
        return;
    }
    let mut spilled = Vec::new();
    for (zone, seq, data) in entries {
        if proto::province_of(zone_region(zone), size) != p {
            continue;
        }
        room.build.mirror_at(zone, &data, Some(seq), &[]);
        if let Some(minelib_net::game::BuildMsg::Set { pos, .. }) =
            minelib_net::game::decode::<minelib_net::game::BuildMsg>(&data)
        {
            if room.build.items.chest(pos).iter().any(Option::is_some) {
                spilled.push(pos);
            }
        }
    }
    st.province_archived += 1;
    // (Confirmé à l'hôte : il le dit à ses joueurs, pour qui ce n'est plus en doute.)
    if let Some(c) = st.conns.get(&id) {
        transport::push(
            &c.tx,
            &Response::ProvinceArchived {
                room: rid,
                province: p,
                epoch,
                n,
            },
        );
    }
    for pos in spilled {
        spill_chest(st, rid, pos);
    }
}

/// Relais du monde : relais de région prêts dans la province (il en choisit toujours les
/// hôtes, et les remplace quand ils s'en vont).
pub(crate) fn shards_ready(
    st: &mut State,
    rid: u64,
    id: u64,
    p: [i32; 2],
    epoch: u64,
    ready: Vec<([i32; 2], u16)>,
) {
    let size = size(st);
    let Some(room) = st.rooms.get_mut(&rid) else {
        return;
    };
    if !room
        .provinces
        .get(&p)
        .is_some_and(|r| r.host == id && r.epoch == epoch && r.info.is_some())
    {
        return;
    }
    let ready: HashSet<([i32; 2], u16)> = ready.into_iter().collect();
    for (region, list) in room.shards.iter_mut() {
        if proto::province_of(*region, size) != p {
            continue;
        }
        for s in list.iter_mut() {
            let up = ready.contains(&(*region, s.slot));
            if up && s.info.is_none() {
                s.info = Some(proto::ShardInfo {
                    region: *region,
                    slot: s.slot,
                    node: [0; 32],
                    addr: String::new(),
                    key: [0; 32],
                    signer: [0; 32],
                    epoch: 0,
                });
            } else if !up {
                s.info = None;
            }
        }
    }
}

/// Relais du monde : départ propre de l'hôte (tout est archivé) : reprise tout de suite.
pub(crate) fn released(st: &mut State, rid: u64, id: u64, p: [i32; 2], epoch: u64) {
    let ok = st.rooms.get(&rid).is_some_and(|room| {
        room.provinces
            .get(&p)
            .is_some_and(|r| r.host == id && r.epoch == epoch)
    });
    if ok {
        take_back(st, rid, p, "rendue");
    }
}

/// Relais du monde (toutes les 50 ms) : une province dont le bail est fini depuis
/// `PROVINCE_MARGIN_MS` revient ici (son hôte est tombé, coupé, ou le monde est en pause).
pub(crate) fn upkeep_leases(st: &mut State) {
    let limit = Duration::from_millis(proto::PROVINCE_LEASE_MS + proto::PROVINCE_MARGIN_MS);
    let due: Vec<(u64, [i32; 2])> = st
        .rooms
        .iter()
        .filter(|(_, r)| !r.provinces.is_empty())
        .flat_map(|(&rid, r)| {
            r.provinces
                .iter()
                .filter(|(_, p)| p.info.is_some() && p.renewed.elapsed() > limit)
                .map(move |(&p, _)| (rid, p))
        })
        .collect();
    for (rid, p) in due {
        take_back(st, rid, p, "bail fini");
    }
}

/// Relais du monde : arrêt demandé à l'hôte d'une province. Pas encore prête : reprise
/// tout de suite ; prête : elle s'arrête et rend tout (`ProvinceRelease`), sinon reprise à
/// la fin du bail.
fn stop(st: &mut State, rid: u64, p: [i32; 2], why: &str) {
    let Some(r) = st.rooms.get(&rid).and_then(|room| room.provinces.get(&p)) else {
        return;
    };
    let (host, epoch, ready) = (r.host, r.epoch, r.info.is_some());
    if let Some(c) = st.conns.get(&host) {
        transport::push(
            &c.tx,
            &Response::ProvinceStop {
                room: rid,
                province: p,
                epoch,
            },
        );
    }
    if !ready {
        take_back(st, rid, p, why);
    } else {
        log::info!("monde {rid} : province {p:?} rendue au relais du monde ({why})");
    }
}

/// Relais du monde : il sert de nouveau la province (il a tout ce qu'elle a publié). Ses
/// relais de région rattachés à elle sont relevés (les suivants se rattacheront ici), son
/// hôte est libéré, les joueurs qui l'ont en vue l'apprennent.
fn take_back(st: &mut State, rid: u64, p: [i32; 2], why: &str) {
    let size = size(st);
    let Some(room) = st.rooms.get_mut(&rid) else {
        return;
    };
    let Some(r) = room.provinces.remove(&p) else {
        return;
    };
    refresh_delegated(room);
    let regions: Vec<[i32; 2]> = room
        .shards
        .keys()
        .filter(|r| proto::province_of(**r, size) == p)
        .copied()
        .collect();
    let mut relieved = Vec::new();
    for g in &regions {
        relieved.extend(room.shards.remove(g).into_iter().flatten().map(|s| s.host));
        room.shards_dirty.insert(*g);
    }
    relieved.push(r.host);
    for h in relieved {
        leave_room(st, h);
        if let Some(c) = st.conns.get(&h) {
            transport::push(
                &c.tx,
                &Response::ProvinceStop {
                    room: rid,
                    province: p,
                    epoch: r.epoch,
                },
            );
            if h != r.host {
                transport::push(&c.tx, &Response::RoomClosed);
            }
        }
    }
    log::info!("monde {rid} : province {p:?} reprise par le relais du monde ({why})");
    announce(st, rid, p);
}

/// Relais du monde : la liste des provinces a changé pour `p` : à chaque joueur qui l'a
/// en vue.
fn announce(st: &mut State, rid: u64, p: [i32; 2]) {
    let size = size(st);
    let Some(room) = st.rooms.get(&rid) else {
        return;
    };
    // (Les joueurs de la province et de ses abords, cherchés par zones.)
    let span = proto::REGION * size;
    let margin = super::SHARD_VIEW + proto::REGION;
    let lo = [p[0] * span - margin, p[1] * span - margin];
    let hi = [(p[0] + 1) * span + margin, (p[1] + 1) * span + margin];
    let players: Vec<u64> =
        room.build
            .players_in(lo, hi)
            .into_iter()
            .filter(|id| {
                !room.hidden.contains(id)
                    && room.build.positions.get(id).is_some_and(|c| {
                        regions_in_view(*c).any(|r| proto::province_of(r, size) == p)
                    })
            })
            .collect();
    for id in players {
        send_list(st, rid, id);
    }
}

/// Relais du monde : à un joueur, les provinces servies par un relais de province qu'il a
/// en vue, si cette liste a changé pour lui.
pub(crate) fn send_list(st: &mut State, rid: u64, id: u64) {
    use std::hash::{Hash, Hasher};
    let size = size(st);
    let Some(room) = st.rooms.get_mut(&rid) else {
        return;
    };
    if room.provinces.is_empty() && !room.provinces_sent.contains_key(&id) {
        return;
    }
    let Some(&chunk) = room.build.positions.get(&id) else {
        return;
    };
    let mut seen = HashSet::new();
    let mut list = Vec::new();
    for r in regions_in_view(chunk) {
        let p = proto::province_of(r, size);
        if seen.insert(p) {
            if let Some(info) = room.provinces.get(&p).and_then(|x| x.info.clone()) {
                list.push(info);
            }
        }
    }
    list.sort_by_key(|i| i.province);
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for i in &list {
        (i.province, i.epoch, &i.addr, i.key, i.node).hash(&mut h);
    }
    let print = h.finish() | 1;
    let had = room.provinces_sent.get(&id).copied().unwrap_or(0);
    if had == print || (had == 0 && list.is_empty()) {
        return;
    }
    room.provinces_sent.insert(id, print);
    // (Avec son billet : il entre chez le relais de province avec le certificat de la clé
    // de son compte, même sans relais de région en vue.)
    let ticket = (!list.is_empty())
        .then(|| super::ticket_for(st, rid, id))
        .flatten();
    if let Some(c) = st.conns.get(&id) {
        if let Some(t) = ticket {
            transport::push(&c.tx, &t);
        }
        transport::push(&c.tx, &Response::Provinces { list });
    }
}

// ───────────────────────── relais de province ─────────────────────────

/// Relais de province : la salle qui sert la province (même numéro que le monde chez le
/// relais du monde : les signatures et les certificats le citent).
#[allow(clippy::too_many_arguments)]
pub(crate) fn begin(
    st: &mut State,
    rid: u64,
    p: [i32; 2],
    size: i32,
    epoch: u64,
    secret: [u8; 32],
    root: [u8; 32],
    info: proto::StandbyInfo,
) -> bool {
    let Ok(root) = VerifyingKey::from_bytes(&root) else {
        return false;
    };
    let local = open_room(
        st,
        0,
        "",
        1,
        info.name.clone(),
        info.map.clone(),
        info.map_sig,
        0,
    );
    let Some(mut room) = st.rooms.remove(&local) else {
        return false;
    };
    room.by_pseudo.clear();
    room.pseudo_of.clear();
    room.host_pseudo = info.owner.clone();
    room.creator = info.creator.clone();
    room.server = info.server.clone();
    room.shard_secret = secret;
    room.build = super::zones::Build::default();
    room.build
        .set_page_dir(st.cfg.dir.join("zones").join(format!("province-{rid}")));
    room.province = Some(ProvinceRoom {
        province: p,
        size,
        epoch,
        root,
        go: false,
        lease_until: 0,
        last_ask: 0,
        serving: false,
        last_beat: 0,
        ready_sent: Vec::new(),
        stopped: false,
        certs: HashMap::new(),
        outbox: Vec::new(),
        next_n: 1,
        last_archive: 0,
    });
    st.rooms.insert(rid, room);
    st.province_room = Some(rid);
    log::info!(
        "relais de province : province {p:?} du monde « {} » (époque {epoch})",
        info.name
    );
    true
}

/// Relais de province : historique reçu du relais du monde ; `go` : sa plage de rangs
/// (il peut servir, dès son premier bail).
pub(crate) fn seed(st: &mut State, entries: Vec<([i32; 2], u64, Vec<u8>)>, go: Option<u64>) {
    let Some(rid) = st.province_room else {
        return;
    };
    let Some(room) = st.rooms.get_mut(&rid) else {
        return;
    };
    let Some(pr) = room.province.as_ref() else {
        return;
    };
    let (prov, size) = (pr.province, pr.size);
    for (zone, seq, data) in entries {
        if proto::province_of(zone_region(zone), size) == prov {
            room.build.mirror_at(zone, &data, Some(seq), &[]);
        }
    }
    if let Some(base) = go {
        room.build.start_at(base);
        // Ce qu'elle publie désormais part aussi au relais du monde (`tick`).
        room.build.set_replicating(true);
        if let Some(pr) = room.province.as_mut() {
            pr.go = true;
        }
    }
}

/// Relais de province : ce qu'il faut envoyer au relais du monde (demande de bail, ce qui
/// a été publié, relais de région prêts).
pub(crate) fn tick(st: &mut State) -> Vec<Request> {
    let now = guards::now_ms(st);
    let Some(rid) = st.province_room else {
        return Vec::new();
    };
    let Some(room) = st.rooms.get_mut(&rid) else {
        return Vec::new();
    };
    let ready: Vec<([i32; 2], u16)> = {
        let mut v: Vec<([i32; 2], u16)> = room
            .shards
            .iter()
            .flat_map(|(r, l)| l.iter().filter(|s| s.info.is_some()).map(|s| (*r, s.slot)))
            .collect();
        v.sort_unstable();
        v
    };
    let entries = room.build.take_replica();
    let players: Vec<u64> = room
        .members
        .iter()
        .filter(|m| !room.hidden.contains(m))
        .copied()
        .collect();
    let Some(pr) = room.province.as_mut() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if !pr.go || pr.stopped {
        return out;
    }
    let (p, epoch) = (pr.province, pr.epoch);
    pr.outbox.extend(entries);
    if !pr.outbox.is_empty()
        && (now.saturating_sub(pr.last_archive) >= ARCHIVE_MS || pr.outbox.len() >= SEED_CHUNK)
    {
        pr.last_archive = now;
        let n = pr.next_n;
        pr.next_n += 1;
        out.push(Request::ProvinceArchive {
            room: rid,
            province: p,
            epoch,
            n,
            entries: std::mem::take(&mut pr.outbox),
        });
        // Ses joueurs : ce qu'ils ont reçu jusqu'ici fait partie de cet envoi.
        for m in players {
            if let Some(c) = st.conns.get(&m) {
                transport::push(&c.tx, &Response::ProvinceMark { n });
            }
        }
    }
    let Some(pr) = st.rooms.get_mut(&rid).and_then(|r| r.province.as_mut()) else {
        return out;
    };
    if now.saturating_sub(pr.last_ask) >= proto::PROVINCE_RENEW_MS {
        pr.last_ask = now;
        out.push(Request::ProvinceLease {
            room: rid,
            province: p,
            epoch,
            t: now,
        });
    }
    if ready != pr.ready_sent {
        pr.ready_sent = ready.clone();
        out.push(Request::ProvinceShards {
            room: rid,
            province: p,
            epoch,
            ready,
        });
    }
    out
}

/// Relais de province : le relais du monde a gardé l'envoi `n` ; ses joueurs l'apprennent
/// (ce qui précède la marque `n` n'est plus en doute).
pub(crate) fn archived(st: &mut State, epoch: u64, n: u64) {
    let Some(rid) = st.province_room else {
        return;
    };
    let Some(room) = st.rooms.get(&rid) else {
        return;
    };
    if room.province.as_ref().is_none_or(|p| p.epoch != epoch) {
        return;
    }
    for m in room.members.iter().filter(|m| !room.hidden.contains(m)) {
        if let Some(c) = st.conns.get(m) {
            transport::push(&c.tx, &Response::ProvinceConfirmed { n });
        }
    }
}

/// Relais de province : réponse du relais du monde à une demande de bail faite à `t`.
pub(crate) fn leased(st: &mut State, epoch: u64, t: u64, ok: bool) {
    let Some(rid) = st.province_room else {
        return;
    };
    let Some(pr) = st.rooms.get_mut(&rid).and_then(|r| r.province.as_mut()) else {
        return;
    };
    if ok && epoch == pr.epoch && !pr.stopped {
        pr.lease_until = pr.lease_until.max(t + proto::PROVINCE_LEASE_MS);
    }
}

/// Relais de province : arrêt (reprise par le relais du monde, ou départ de son hôte).
/// Renvoie ce qu'il reste à envoyer au relais du monde (dernières publications, et le
/// départ propre si le bail court encore).
pub(crate) fn stop_here(st: &mut State, release: bool) -> Vec<Request> {
    let Some(rid) = st.province_room else {
        return Vec::new();
    };
    let Some(room) = st.rooms.get_mut(&rid) else {
        return Vec::new();
    };
    let entries = room.build.take_replica();
    let Some(pr) = room.province.as_mut() else {
        return Vec::new();
    };
    if pr.stopped {
        return Vec::new();
    }
    pr.stopped = true;
    let (p, epoch) = (pr.province, pr.epoch);
    pr.outbox.extend(entries);
    let mut out = Vec::new();
    if release {
        if !pr.outbox.is_empty() {
            let n = pr.next_n;
            pr.next_n += 1;
            out.push(Request::ProvinceArchive {
                room: rid,
                province: p,
                epoch,
                n,
                entries: std::mem::take(&mut pr.outbox),
            });
        }
        out.push(Request::ProvinceRelease {
            room: rid,
            province: p,
            epoch,
        });
    }
    // Joueurs et relais de région : la province est fermée ici.
    let members: Vec<u64> = room.members.clone();
    guards::send_pause(st, rid, true, true);
    for m in members {
        if let Some(c) = st.conns.get(&m) {
            transport::push(&c.tx, &Response::RoomClosed);
        }
    }
    log::info!("relais de province : province {p:?} arrêtée");
    out
}

/// Relais de province (toutes les 50 ms) : quand le bail commence ou finit, tout le monde
/// l'apprend (pause) ; et un battement à ses relais de région et à ses joueurs non servis.
pub(crate) fn upkeep_here(st: &mut State) {
    let now = guards::now_ms(st);
    let Some(rid) = st.province_room else {
        return;
    };
    let Some(pr) = st.rooms.get_mut(&rid).and_then(|r| r.province.as_mut()) else {
        return;
    };
    if pr.stopped {
        return;
    }
    let serving = pr.serving(now);
    let changed = serving != pr.serving;
    pr.serving = serving;
    if !changed && now.saturating_sub(pr.last_beat) < BEAT_MS {
        return;
    }
    pr.last_beat = now;
    if changed {
        log::info!(
            "relais de province : {}",
            if serving {
                "bail en cours"
            } else {
                "plus de bail, pause"
            }
        );
    }
    guards::send_pause(st, rid, !serving, changed);
}

/// Relais de province : un joueur du monde entre (certificat de la clé de son compte,
/// signé par le relais du monde). `None` si ce relais ne tient pas de province ; sinon si
/// le certificat est bon (il est gardé : il accompagnera ses modifications signées).
pub(crate) fn admits(st: &mut State, cert: &minelib_net::sovereign::Cert) -> Option<bool> {
    let rid = st.province_room?;
    let pr = st.rooms.get_mut(&rid)?.province.as_mut()?;
    let ok = cert.valid(&pr.root, rid) && cert.expires >= super::now_secs() && !pr.stopped;
    if ok {
        pr.certs
            .insert(cert.pseudo.to_ascii_lowercase(), cert.clone());
    }
    Some(ok)
}

/// Relais de province : une demande d'un joueur pour une zone. Seulement les blocs de sa
/// province (les objets restent au relais du monde) ; dans une région souveraine, la
/// modification doit être signée par son auteur et part à l'hôte principal (ce relais ne
/// signe jamais à la place d'un joueur) ; ailleurs, il la publie lui-même.
#[allow(clippy::too_many_arguments)]
pub(crate) fn to_zone(
    st: &mut State,
    rid: u64,
    id: u64,
    pseudo: &str,
    chunk: [i32; 2],
    msg: Option<minelib_net::game::BuildMsg>,
) {
    use minelib_net::game::BuildMsg;
    if guards::frozen(st, rid) {
        return;
    }
    let region = proto::region_of(chunk);
    let Some(room) = st.rooms.get(&rid) else {
        return;
    };
    let Some(pr) = room.province.as_ref().filter(|p| p.has(region)) else {
        return;
    };
    let sovereign = room.sov.get(&region).map(|sv| sv.primary);
    match msg {
        Some(BuildMsg::Edit { pos, block, fluid }) if sovereign.is_none() => {
            super::build_edit(st, rid, id, pseudo, chunk, pos, block, fluid);
        }
        Some(BuildMsg::Signed(e)) if e.pseudo.eq_ignore_ascii_case(pseudo) && !e.by_root => {
            let e_chunk = [e.pos[0].div_euclid(16), e.pos[2].div_euclid(16)];
            if e_chunk != chunk {
                return;
            }
            match sovereign {
                Some(primary) => {
                    let certs: Vec<_> = pr
                        .certs
                        .get(&pseudo.to_ascii_lowercase())
                        .cloned()
                        .into_iter()
                        .collect();
                    if let Some(c) = st.conns.get(&primary) {
                        transport::push(
                            &c.tx,
                            &Response::RootEdits {
                                region,
                                edits: vec![e],
                                certs,
                            },
                        );
                    }
                }
                None => {
                    super::build_edit(st, rid, id, pseudo, chunk, e.pos, e.block, e.fluid);
                }
            }
        }
        _ => {}
    }
}

/// Relais de province : l'hôte d'un relais de région s'y rattache (jeton du relais du
/// monde, qui donne aussi la clé de son compte) ; il reçoit l'historique de sa région.
#[allow(clippy::too_many_arguments)]
pub(crate) fn region_host(
    st: &mut State,
    session: &mut Session,
    id: u64,
    room: u64,
    region: [i32; 2],
    slot: u16,
    pseudo: String,
    signer: [u8; 32],
    token: [u8; 32],
) -> Response {
    let ok = st.province_room == Some(room)
        && st.rooms.get(&room).is_some_and(|r| {
            r.province.as_ref().is_some_and(|p| {
                p.has(region)
                    && !p.stopped
                    && proto::province_token(
                        &r.shard_secret,
                        room,
                        region,
                        slot,
                        &pseudo,
                        &signer,
                        p.epoch,
                    ) == token
            })
        });
    if !ok || pseudo.starts_with('#') {
        return err(
            ErrorCode::WrongCredentials,
            "jeton de relais de région refusé",
        );
    }
    if let Some(c) = st.conns.get_mut(&id) {
        c.pseudo = Some(pseudo.clone());
        c.room = Some(room);
        c.garant_only = true;
        c.account = Some(signer);
    }
    *session = Session::LoggedIn {
        pseudo: pseudo.clone(),
    };
    let r = st.rooms.get_mut(&room).expect("province");
    r.members.push(id);
    r.hidden.insert(id);
    let list = r.shards.entry(region).or_default();
    list.retain(|s| s.slot != slot);
    list.push(RegionShard {
        host: id,
        slot,
        info: None,
        previous: None,
    });
    let history = r.build.logs_in(|zone| zone_region(zone) == region);
    if let Some(c) = st.conns.get(&id) {
        for (zone, log) in history {
            let chunk = [zone[0] * proto::ZONE, zone[1] * proto::ZONE];
            for data in log {
                push_zone(&c.tx, chunk, data);
            }
        }
    }
    log::info!("relais de province : région {region:?} rattachée (rang {slot}, {pseudo})");
    Response::Welcome {
        pseudo,
        totp: false,
    }
}
