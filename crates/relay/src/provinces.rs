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

use std::collections::{HashMap, HashSet, VecDeque};
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
/// Positions de retour des joueurs d'une province : envoyées au relais du monde toutes
/// les… (ms).
const SPOT_MS: u64 = 2000;
/// Sac de survie demandé sans réponse : redemandé après… (ms).
const BAG_ASK_MS: u64 = 3000;
/// (v141) Envois d'une province gardés ici pour son relais de secours, au plus (nombre,
/// octets) ; et envois pas encore gardés qu'elle garde pour lui.
const RECENT_MAX: usize = 128;
const RECENT_BYTES: usize = 16 << 20;
const COPIES_MAX: usize = 256;
/// Relais de secours prêt qui ne suit plus sa province depuis… : remplacé.
const UNFOLLOWED_FOR: Duration = Duration::from_secs(5);

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
    /// (v140) Ce qu'elle dit à chaque bail : joueurs de ses régions peuplées (rien avant
    /// son premier bail : on compte alors ceux que ce relais voit), relais de région dont
    /// l'hôte joue dans la région qu'il sert.
    pub counts: Option<Vec<([i32; 2], u32)>>,
    pub inside: Vec<([i32; 2], u16)>,
    /// (v141) Son relais de secours ; ses derniers envois gardés ici (numéro, entrées : ce
    /// que le relais de secours n'aurait pas encore s'il doit prendre sa place) ; après une
    /// promotion, ceux de l'époque d'avant.
    pub standby: Option<Standby>,
    recent: VecDeque<(u64, Vec<super::zones::Entry>)>,
    recent_bytes: usize,
    carry: Option<VecDeque<(u64, Vec<super::zones::Entry>)>>,
    /// Arrêt demandé pour de bon (province vide, pas prête) : elle revient ici, son relais
    /// de secours ne prend pas sa place.
    winding: bool,
    /// (v142) Son relais de secours garde la copie de ce qu'elle publie (plus ce relais) ;
    /// il la suit ; la copie d'ici n'est plus à jour (tout doit revenir : demande `dump_n`) ;
    /// arrêt à faire dès que la copie d'ici sera à jour.
    pub big: bool,
    followed: bool,
    stale: bool,
    dump_n: u64,
    stop_after_dump: bool,
    /// Son relais de secours ne la suit plus depuis (il est remplacé au bout de
    /// `UNFOLLOWED_FOR`).
    unfollowed_since: Option<Instant>,
}

/// (v141) Relais de secours d'une province, choisi par le relais du monde.
pub(crate) struct Standby {
    /// Connexion de garant de son hôte.
    pub host: u64,
    /// Sa copie est à jour : il peut prendre la place du relais de la province.
    pub ready: bool,
    since: Instant,
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
    /// (v139) Sacs de survie prêtés par le relais du monde (pseudos en minuscules) ; ceux
    /// demandés (pas encore reçus) ; ce qu'il faut lui dire (demandes, retours).
    held: HashSet<String>,
    asked: HashMap<String, u64>,
    requests: Vec<Request>,
    /// Positions de retour de ses joueurs, à transmettre au relais du monde (toutes les
    /// `SPOT_MS`), et dernier envoi.
    spots: HashMap<String, [f32; 5]>,
    last_spots: u64,
    /// Blocs changés ici par des joueurs en survie dont elle n'a pas le sac : comptés par
    /// le relais du monde (dans le prochain envoi).
    loose: Vec<(String, u16)>,
    /// (v141) Relais de secours : on suit la province sans la servir (jusqu'à la
    /// promotion) ; dernier envoi copié ici.
    standby: bool,
    have: u64,
    /// Relais de province : ses relais de secours (connexions), dernier envoi gardé par le
    /// relais du monde, et les envois pas encore gardés (ils leur partiront ensuite).
    followers: Vec<u64>,
    acked: u64,
    copies: VecDeque<(u64, Vec<super::zones::Entry>, Vec<proto::BagCopy>)>,
    /// (v142) Ce qu'elle publie part au relais du monde (sinon : à son relais de secours
    /// seulement, qui le garde) ; demande de tout renvoyer au relais du monde (numéro) et
    /// dernière servie ; envois pas encore confirmés à ses joueurs (numéro, gardé par le
    /// relais de secours plutôt que par le relais du monde), dernier copié par lui.
    to_root: bool,
    dump_asked: u64,
    dump_done: u64,
    unconfirmed: VecDeque<(u64, bool)>,
    acked_copy: u64,
    /// Dernier envoi qui portait tout (au relais du monde) ; tout ce qui le précède est
    /// gardé dès qu'il l'est.
    dump_archive: u64,
    covered: u64,
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

/// Relais du monde (v140) : provinces servies qui comptent elles-mêmes leurs joueurs (leurs
/// positions ne passent plus par ici).
pub(crate) fn counted_provinces(room: &super::Room) -> HashSet<[i32; 2]> {
    room.provinces
        .iter()
        .filter(|(p, r)| room.delegated.contains(*p) && r.counts.is_some())
        .map(|(p, _)| *p)
        .collect()
}

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
    // (Une province ne se découpe pas elle-même.)
    if room.kind != 1 || room.province.is_some() || guards::frozen(st, rid) {
        return;
    }
    // Joueurs par province : ceux que ce relais voit (comptés par région à chaque
    // déplacement), et (v140) ceux que compte chaque province servie (leurs positions ne
    // passent plus par ici).
    let room = st.rooms.get_mut(&rid).expect("partie");
    let counted = counted_provinces(room);
    let mut count: HashMap<[i32; 2], usize> = HashMap::new();
    for (r, n) in room.build.region_counts() {
        let p = proto::province_of(*r, size);
        if !counted.contains(&p) {
            *count.entry(p).or_default() += *n as usize;
        }
    }
    for p in &counted {
        let n = room.provinces[p]
            .counts
            .iter()
            .flatten()
            .map(|(_, n)| *n as usize)
            .sum::<usize>();
        *count.entry(*p).or_default() += n;
    }
    let room = st.rooms.get(&rid).expect("partie");
    if room.provinces.is_empty() && count.values().all(|&n| n < from) {
        return;
    }
    // Provinces vides depuis longtemps, hôtes qui ne sont pas prêts à temps, hôtes dont le
    // joueur est entré dans la province qu'il sert (il ne décide pas pour lui-même).
    // (Son joueur cherché parmi ceux de ce monde : sa position d'ici suffit, il la redit à
    // chaque changement de province.)
    let inside: Vec<[i32; 2]> = room
        .provinces
        .iter()
        .filter(|(p, r)| {
            st.conns
                .get(&r.host)
                .and_then(|c| c.pseudo.as_deref())
                .and_then(|name| room.by_pseudo.get(&name.to_ascii_lowercase()))
                .and_then(|player| room.build.positions.get(player))
                .is_some_and(|c| proto::province_of(proto::region_of(*c), size) == **p)
        })
        .map(|(p, _)| *p)
        .collect();
    // (Hôte entré dans sa province : son relais de secours prend sa place, s'il en a un.)
    let mut back = Vec::new();
    let room = st.rooms.get_mut(&rid).expect("partie");
    for (p, r) in room.provinces.iter_mut() {
        if inside.contains(p) {
            back.push((*p, true));
            continue;
        }
        if r.info.is_none() && r.since.elapsed() > START_WAIT {
            back.push((*p, false));
            continue;
        }
        if count.get(p).copied().unwrap_or(0) == 0 {
            let since = *r.empty_since.get_or_insert_with(Instant::now);
            if since.elapsed() > EMPTY_FOR {
                back.push((*p, false));
            }
        } else {
            r.empty_since = None;
        }
    }
    for (p, keep) in back {
        stop(st, rid, p, "vide, pas prête, ou hôte dedans", keep);
    }
    // (v141) Un relais de secours pour chaque province servie.
    standbys(st, rid, index);
    let mut wanted: Vec<[i32; 2]> = count
        .iter()
        .filter(|(p, &n)| n >= from && !st.rooms[&rid].provinces.contains_key(*p))
        .map(|(p, _)| *p)
        .collect();
    wanted.sort();
    if wanted.is_empty()
        || st
            .hosts_wait
            .is_some_and(|t| t.elapsed() < super::HOSTS_WAIT)
    {
        return;
    }
    if index.is_none() {
        *index = Some(super::players_index(st));
    }
    let index = index.as_ref().expect("index");
    for p in wanted {
        let outside = move |chunk: [i32; 2]| proto::province_of(proto::region_of(chunk), size) != p;
        let Some(g) = super::pick_host(st, index, rid, &outside) else {
            st.hosts_wait = Some(Instant::now());
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
    // (Blocs, objets et coffres : v139.)
    let entries = room.build.entries_after(keep, 0);
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
    let server = room.server.clone();
    let settings = settings_of(st, &server);
    let room = st.rooms.get_mut(&rid).expect("partie");
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
            counts: None,
            inside: Vec::new(),
            standby: None,
            recent: VecDeque::new(),
            recent_bytes: 0,
            carry: None,
            winding: false,
            big: false,
            followed: false,
            stale: false,
            dump_n: 0,
            stop_after_dump: false,
            unfollowed_since: None,
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
            settings,
            follow: None,
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
    have: u64,
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
    let carry = r.carry.take();
    let keep = |zone: [i32; 2]| proto::province_of(zone_region(zone), size) == p;
    // (v141) Relais de secours qui prend la place du relais tombé : les envois gardés ici
    // qu'il n'a pas encore (tout, s'il a trop de retard), puis ce que ce relais a publié
    // dans la province depuis.
    let mut delta = match carry {
        Some(recent) if recent.front().is_none_or(|(n, _)| *n <= have + 1) => recent
            .into_iter()
            .filter(|(n, _)| *n > have)
            .flat_map(|(_, e)| e)
            .collect(),
        Some(_) => room.build.entries_after(keep, 0),
        None => Vec::new(),
    };
    delta.extend(room.build.entries_after(keep, after));
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
#[allow(clippy::too_many_arguments)]
pub(crate) fn lease(
    st: &mut State,
    rid: u64,
    id: u64,
    p: [i32; 2],
    epoch: u64,
    t: u64,
    counts: Vec<([i32; 2], u32)>,
    inside: Vec<([i32; 2], u16)>,
    followed: bool,
) {
    let paused = guards::frozen(st, rid);
    let size = size(st);
    let copies_until = st.copies_until;
    let Some(room) = st.rooms.get_mut(&rid) else {
        return;
    };
    // (v142) Monde assez grand pour que la copie de ce que publient les provinces soit
    // gardée par leurs relais de secours plutôt qu'ici.
    let big_world = room.members.len().saturating_sub(room.hidden.len()) >= copies_until;
    let (mut copies, mut dump) = (true, 0);
    let ok = match room
        .provinces
        .get_mut(&p)
        .filter(|r| r.host == id && r.epoch == epoch && r.info.is_some())
    {
        Some(r) => {
            r.followed = followed;
            // (Relais de secours prêt qui ne la suit plus : remplacé un peu plus tard.)
            if followed || r.standby.as_ref().is_none_or(|s| !s.ready) {
                r.unfollowed_since = None;
            } else {
                r.unfollowed_since.get_or_insert_with(Instant::now);
            }
            let big =
                big_world && followed && !r.winding && r.standby.as_ref().is_some_and(|s| s.ready);
            if r.big && !big {
                // Repasse par ici : tout doit revenir d'abord.
                r.stale = true;
                r.dump_n += 1;
                log::info!("monde {rid} : copie de la province {p:?} de nouveau ici");
            } else if !r.big && big {
                log::info!(
                    "monde {rid} : copie de la province {p:?} gardée par son relais de secours"
                );
            }
            r.big = big;
            copies = !big;
            if r.stale {
                dump = r.dump_n;
            }
            // (Seulement ses propres régions, une fois chacune.)
            let mine = |reg: &[i32; 2]| proto::province_of(*reg, size) == p;
            r.counts = Some(
                counts
                    .into_iter()
                    .filter(|(reg, _)| mine(reg))
                    .take(4096)
                    .collect(),
            );
            r.inside = inside
                .into_iter()
                .filter(|(reg, _)| mine(reg))
                .take(4096)
                .collect();
            if !paused {
                r.renewed = Instant::now();
            }
            !paused
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
                copies,
                dump,
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
    bags: Vec<proto::BagCopy>,
    spots: Vec<(String, [f32; 5])>,
    loose: Vec<(String, u16)>,
    dump: u64,
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
    // (v141) Gardés aussi un moment pour son relais de secours.
    if let Some(r) = room.provinces.get_mut(&p).filter(|r| r.standby.is_some()) {
        let bytes: usize = entries.iter().map(|e| e.2.len() + 24).sum();
        r.recent.push_back((n, entries.clone()));
        r.recent_bytes += bytes;
        while r.recent.len() > RECENT_MAX || r.recent_bytes > RECENT_BYTES {
            let Some((_, old)) = r.recent.pop_front() else {
                break;
            };
            r.recent_bytes -= old.iter().map(|e| e.2.len() + 24).sum::<usize>();
        }
    }
    for (zone, seq, data) in entries {
        if proto::province_of(zone_region(zone), size) != p {
            continue;
        }
        room.build.mirror_at(zone, &data, Some(seq), &[]);
        // (Objets et coffres : arbitrés par la province depuis la v139 ; ce relais en
        // garde l'état, pour la reprendre.)
        if let Some(m) = minelib_net::game::decode::<minelib_net::game::BuildMsg>(&data) {
            room.build.items.absorb(&m);
        }
    }
    // Sacs de survie qu'elle tient : leur copie ; positions de retour de ses joueurs.
    let server = room.server.clone();
    let mine: Vec<proto::BagCopy> = bags
        .into_iter()
        .filter(|(who, _, _)| {
            room.bag_holders
                .get(&who.to_ascii_lowercase())
                .is_some_and(|h| *h == (p, epoch))
        })
        .collect();
    for (who, items, mined) in mine {
        st.bags.put(
            &server,
            &who,
            super::bags::Bag {
                items: items.into_iter().collect(),
                mined,
            },
        );
    }
    // (Une province ne place un joueur que chez elle.)
    for (who, at) in spots {
        let chunk = [
            (at[0].floor() as i32).div_euclid(16),
            (at[2].floor() as i32).div_euclid(16),
        ];
        if at.iter().all(|v| v.is_finite()) && near(chunk, p, size) {
            st.servers.set_position(&server, &who, at);
        }
    }
    // Blocs changés là-bas par des joueurs en survie dont elle n'avait pas le sac : ceux
    // de ses joueurs (ou partis depuis), au plus `LOOSE_MAX` chacun par envoi.
    let mut counted: HashMap<String, usize> = HashMap::new();
    for (who, block) in loose {
        let who = who.to_ascii_lowercase();
        let n = counted.entry(who.clone()).or_default();
        *n += 1;
        let ok = *n <= LOOSE_MAX
            && st
                .rooms
                .get(&rid)
                .is_some_and(|r| player_near(r, &who, p, size).unwrap_or(true));
        if ok {
            bag_block(st, rid, &server, &who, block);
        }
    }
    st.province_archived += 1;
    // (v142) Tout est revenu : la copie d'ici est de nouveau à jour.
    let mut stop_now = false;
    if let Some(r) = st
        .rooms
        .get_mut(&rid)
        .and_then(|room| room.provinces.get_mut(&p))
    {
        if dump > 0 && dump == r.dump_n && r.stale {
            r.stale = false;
            stop_now = r.stop_after_dump;
            log::info!("monde {rid} : copie de la province {p:?} de nouveau à jour ici");
        }
    }
    if stop_now {
        stop(st, rid, p, "copie revenue, arrêt", false);
    }
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
}

/// Relais du monde (v139) : une province veut le sac de survie d'un joueur qui s'y trouve.
/// Prêté s'il est ici ; tenu par une autre province : redemandé à elle, puis prêté.
pub(crate) fn bag_asked(st: &mut State, rid: u64, id: u64, p: [i32; 2], epoch: u64, pseudo: &str) {
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
    let who = pseudo.to_ascii_lowercase();
    // (Seulement pour un joueur qui s'y trouve, d'après ce relais.)
    if player_near(room, &who, p, size) != Some(true) {
        return;
    }
    match room.bag_holders.get(&who).copied() {
        Some(h) if h == (p, epoch) => lend(st, rid, &who),
        Some((q, e)) => {
            room.bag_waiting.insert(who.clone(), (p, epoch));
            recall(st, rid, &who, q, e);
        }
        None => {
            room.bag_holders.insert(who.clone(), (p, epoch));
            lend(st, rid, &who);
        }
    }
}

/// Relais du monde : un sac rendu par la province qui le tenait.
#[allow(clippy::too_many_arguments)]
pub(crate) fn bag_returned(
    st: &mut State,
    rid: u64,
    id: u64,
    p: [i32; 2],
    epoch: u64,
    pseudo: &str,
    items: Vec<(u32, u32)>,
    mined: u32,
) {
    let Some(room) = st.rooms.get_mut(&rid) else {
        return;
    };
    let who = pseudo.to_ascii_lowercase();
    let ok = room.bag_holders.get(&who) == Some(&(p, epoch))
        && room
            .provinces
            .get(&p)
            .is_some_and(|r| r.host == id && r.epoch == epoch);
    if !ok {
        return;
    }
    room.bag_holders.remove(&who);
    let server = room.server.clone();
    let next = room.bag_waiting.remove(&who);
    st.bags.put(
        &server,
        &who,
        super::bags::Bag {
            items: items.into_iter().collect(),
            mined,
        },
    );
    settle(st, rid, &who);
    // Une autre province l'attendait : à elle, si elle sert encore.
    if let Some((q, e)) = next {
        let room = st.rooms.get_mut(&rid).expect("partie");
        if room
            .provinces
            .get(&q)
            .is_some_and(|r| r.epoch == e && r.info.is_some())
        {
            room.bag_holders.insert(who.clone(), (q, e));
            lend(st, rid, &who);
        }
    }
}

/// Relais du monde : le sac de `who` (tenu ici) part à la province qui le tient désormais.
fn lend(st: &mut State, rid: u64, who: &str) {
    let Some(room) = st.rooms.get(&rid) else {
        return;
    };
    let Some(&(p, epoch)) = room.bag_holders.get(who) else {
        return;
    };
    let Some(host) = room.provinces.get(&p).map(|r| r.host) else {
        return;
    };
    let server = room.server.clone();
    let b = st.bags.get(&server, who);
    let mode = st.bags.mode(&server, who).map(str::to_string);
    st.province_lends += 1;
    if let Some(c) = st.conns.get(&host) {
        transport::push(
            &c.tx,
            &Response::ProvinceBagLent {
                room: rid,
                province: p,
                epoch,
                pseudo: who.to_string(),
                items: b.items.into_iter().collect(),
                mined: b.mined,
                mode,
            },
        );
    }
}

/// Relais du monde : la province qui tient le sac de `who` doit le rendre.
fn recall(st: &State, rid: u64, who: &str, p: [i32; 2], epoch: u64) {
    let Some(host) = st
        .rooms
        .get(&rid)
        .and_then(|r| r.provinces.get(&p))
        .map(|r| r.host)
    else {
        return;
    };
    if let Some(c) = st.conns.get(&host) {
        transport::push(
            &c.tx,
            &Response::ProvinceBagRecall {
                room: rid,
                province: p,
                epoch,
                pseudo: who.to_string(),
            },
        );
    }
}

/// Relais du monde : le sac de `pseudo` est-il prêté à une province ? Si oui, il le lui
/// redemande (le joueur veut s'en servir ici) : il faut attendre son retour.
pub(crate) fn bag_away(st: &mut State, rid: u64, pseudo: &str) -> bool {
    let who = pseudo.to_ascii_lowercase();
    let Some(room) = st.rooms.get_mut(&rid) else {
        return false;
    };
    let Some(&(p, e)) = room.bag_holders.get(&who) else {
        return false;
    };
    room.bag_waiting.remove(&who);
    recall(st, rid, &who, p, e);
    true
}

/// Blocs changés hors du sac comptés au plus par joueur et par envoi d'une province.
const LOOSE_MAX: usize = 64;

/// Le chunk `c` est-il dans la province `p` ou à moins de deux chunks d'elle ?
fn near(c: [i32; 2], p: [i32; 2], size: i32) -> bool {
    [[0, 0], [-2, -2], [-2, 2], [2, -2], [2, 2]]
        .iter()
        .any(|d| proto::province_of(proto::region_of([c[0] + d[0], c[1] + d[1]]), size) == p)
}

/// Le joueur `who` (minuscules) est-il dans la province `p` ou tout près, d'après ce
/// relais ? `None` : il n'est pas (ou plus) dans ce monde.
fn player_near(room: &super::Room, who: &str, p: [i32; 2], size: i32) -> Option<bool> {
    let id = room.by_pseudo.get(who)?;
    Some(
        room.build
            .positions
            .get(id)
            .is_some_and(|c| near(*c, p, size)),
    )
}

/// Un bloc changé par `who` (en survie) : compté dans son sac s'il est ici ; prêté à une
/// province, compté à son retour.
fn bag_block(st: &mut State, rid: u64, server: &str, who: &str, block: u16) {
    let who = who.to_ascii_lowercase();
    if let Some(room) = st.rooms.get_mut(&rid) {
        if room.bag_holders.contains_key(&who) {
            room.bag_deltas.entry(who).or_default().push(block);
            return;
        }
    }
    st.bags.block_set(server, &who, block);
}

/// Sac revenu ici : les blocs comptés pendant son absence y entrent.
fn settle(st: &mut State, rid: u64, who: &str) {
    let Some(room) = st.rooms.get_mut(&rid) else {
        return;
    };
    let server = room.server.clone();
    for block in room.bag_deltas.remove(who).unwrap_or_default() {
        st.bags.block_set(&server, who, block);
    }
}

/// Un bloc vient d'être publié par `by`, en survie (voir `note_block`). Vrai s'il ne doit
/// pas être compté dans le sac tenu ici : relais du monde, sac prêté à une province (compté
/// à son retour) ; relais de province, sac pas (ou plus) ici (compté par le relais du
/// monde).
pub(crate) fn bag_elsewhere(st: &mut State, rid: u64, by: &str, block: u16) -> bool {
    let who = by.to_ascii_lowercase();
    let Some(room) = st.rooms.get_mut(&rid) else {
        return false;
    };
    match room.province.as_mut() {
        Some(pr) => {
            if pr.held.contains(&who) {
                return false;
            }
            pr.loose.push((who, block));
            true
        }
        None => {
            if !room.bag_holders.contains_key(&who) {
                return false;
            }
            room.bag_deltas.entry(who).or_default().push(block);
            true
        }
    }
}

/// Réglages d'un serveur qu'un relais de province doit connaître.
pub(crate) fn settings_of(st: &State, server: &str) -> proto::ProvinceSettings {
    proto::ProvinceSettings {
        default_mode: st.servers.default_gamemode(server),
        ops: st.servers.ops(server),
        bans: st.servers.bans(server),
        modes: st.bags.modes_of(server),
    }
}

/// Relais du monde : les réglages de ce serveur ont changé ; à chaque relais de province de
/// ses mondes.
pub(crate) fn push_settings(st: &State, server: &str) {
    let settings = settings_of(st, server);
    for (&rid, room) in st.rooms.iter().filter(|(_, r)| r.server == server) {
        for r in room.provinces.values() {
            if let Some(c) = st.conns.get(&r.host) {
                transport::push(
                    &c.tx,
                    &Response::ProvinceSettings {
                        room: rid,
                        settings: settings.clone(),
                    },
                );
            }
        }
    }
}

/// Relais du monde : WorldEdit d'un OP dont des cases sont dans des provinces servies par
/// un relais de province. Chaque case y part signée par ce relais à la place de l'OP ;
/// renvoie celles qui restent à publier ici.
pub(crate) fn bulk_split(
    st: &mut State,
    rid: u64,
    pseudo: &str,
    cells: Vec<([i32; 3], u16)>,
) -> Vec<([i32; 3], u16)> {
    let size = size(st);
    let Some(room) = st.rooms.get(&rid) else {
        return cells;
    };
    if room.delegated.is_empty() {
        return cells;
    }
    let delegated = room.delegated.clone();
    let mut here = Vec::new();
    let mut out: HashMap<[i32; 2], Vec<minelib_net::sovereign::Edit>> = HashMap::new();
    for (pos, block) in cells {
        let chunk = [pos[0].div_euclid(16), pos[2].div_euclid(16)];
        let p = proto::province_of(proto::region_of(chunk), size);
        if !delegated.contains(&p) {
            here.push((pos, block));
            continue;
        }
        st.root_t = (st.root_t + 1).max(super::now_ms());
        out.entry(p)
            .or_default()
            .push(minelib_net::sovereign::Edit::sign(
                &st.cert_key,
                rid,
                pseudo,
                pos,
                block,
                false,
                chunk,
                st.root_t,
                true,
            ));
    }
    let Some(room) = st.rooms.get(&rid) else {
        return here;
    };
    for (p, edits) in out {
        let Some(r) = room.provinces.get(&p) else {
            continue;
        };
        if let Some(c) = st.conns.get(&r.host) {
            transport::push(
                &c.tx,
                &Response::ProvinceBulk {
                    room: rid,
                    province: p,
                    epoch: r.epoch,
                    pseudo: pseudo.to_string(),
                    edits,
                },
            );
        }
    }
    here
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
    if ok && !promote(st, rid, p, "rendue") {
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
        if !promote(st, rid, p, "bail fini") {
            take_back(st, rid, p, "bail fini");
        }
    }
}

/// Relais du monde : arrêt demandé à l'hôte d'une province. Pas encore prête : reprise
/// tout de suite ; prête : elle s'arrête et rend tout (`ProvinceRelease`), sinon reprise à
/// la fin du bail.
fn stop(st: &mut State, rid: u64, p: [i32; 2], why: &str, keep: bool) {
    let Some(r) = st
        .rooms
        .get_mut(&rid)
        .and_then(|room| room.provinces.get_mut(&p))
    else {
        return;
    };
    // (Pour de bon : son relais de secours ne prendra pas sa place.)
    r.winding = !keep;
    // (v142) La copie d'ici n'est pas à jour : elle doit d'abord tout renvoyer.
    if !keep && r.info.is_some() && (r.big || r.stale) {
        r.stop_after_dump = true;
        return;
    }
    r.stop_after_dump = false;
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
    drop_standby(st, rid, p);
    let Some(room) = st.rooms.get_mut(&rid) else {
        return;
    };
    let Some(r) = room.provinces.remove(&p) else {
        return;
    };
    if r.stale || r.big {
        log::warn!(
            "monde {rid} : province {p:?} reprise sans copie à jour (son relais et son relais \
             de secours sont tombés) : ce qu'elle a publié depuis est perdu"
        );
    }
    refresh_delegated(room);
    // Ses sacs de survie reviennent ici (copie de son dernier envoi) ; ceux qu'une autre
    // province attendait lui partent.
    let held: Vec<String> = room
        .bag_holders
        .iter()
        .filter(|(_, h)| h.0 == p)
        .map(|(w, _)| w.clone())
        .collect();
    room.bag_waiting.retain(|_, w| w.0 != p);
    let mut lend_now = Vec::new();
    for w in &held {
        room.bag_holders.remove(w);
    }
    for w in &held {
        if let Some(next) = room.bag_waiting.remove(w) {
            room.bag_holders.insert(w.clone(), next);
            lend_now.push(w.clone());
        }
    }
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
    // (Sa dernière copie fait foi ; ce qui a été compté depuis y entre.)
    for w in &held {
        settle(st, rid, w);
    }
    for w in lend_now {
        lend(st, rid, &w);
    }
    log::info!("monde {rid} : province {p:?} reprise par le relais du monde ({why})");
    announce(st, rid, p);
}

/// Ce qu'un relais de province (ou son relais de secours) doit savoir du monde.
fn task_info(room: &super::Room) -> proto::StandbyInfo {
    proto::StandbyInfo {
        name: room.name.clone(),
        map: room.map.clone(),
        map_sig: room.map_sig,
        max_players: room.max_players,
        creator: room.creator.clone(),
        server: room.server.clone(),
        owner: room.host_pseudo.clone(),
    }
}

/// Relais du monde (v141, à chaque entretien) : chaque province servie reçoit un relais de
/// secours (la connexion de garant d'un joueur hors de la province), qui en tient une
/// copie et prendra sa place si elle tombe ; celui qui n'est pas prêt à temps est remplacé.
fn standbys(st: &mut State, rid: u64, index: &mut Option<PlayersIndex>) {
    let size = size(st);
    let Some(room) = st.rooms.get(&rid) else {
        return;
    };
    let late: Vec<[i32; 2]> = room
        .provinces
        .iter()
        .filter(|(_, r)| {
            r.standby
                .as_ref()
                .is_some_and(|s| !s.ready && s.since.elapsed() > START_WAIT)
                || r.unfollowed_since
                    .is_some_and(|t| t.elapsed() > UNFOLLOWED_FOR)
        })
        .map(|(p, _)| *p)
        .collect();
    for p in late {
        drop_standby(st, rid, p);
    }
    let Some(room) = st.rooms.get(&rid) else {
        return;
    };
    let mut wanted: Vec<[i32; 2]> = room
        .provinces
        .iter()
        .filter(|(_, r)| r.info.is_some() && r.standby.is_none() && !r.winding)
        .map(|(p, _)| *p)
        .collect();
    wanted.sort();
    if wanted.is_empty()
        || st
            .hosts_wait
            .is_some_and(|t| t.elapsed() < super::HOSTS_WAIT)
    {
        return;
    }
    if index.is_none() {
        *index = Some(super::players_index(st));
    }
    let index = index.as_ref().expect("index");
    for p in wanted {
        let outside = move |chunk: [i32; 2]| proto::province_of(proto::region_of(chunk), size) != p;
        let Some(g) = super::pick_host(st, index, rid, &outside) else {
            st.hosts_wait = Some(Instant::now());
            break;
        };
        assign_standby(st, rid, p, size, g);
    }
}

/// Relais du monde : la connexion de garant `g` tiendra le relais de secours de `p`.
fn assign_standby(st: &mut State, rid: u64, p: [i32; 2], size: i32, g: u64) {
    let root = st.cert_key.verifying_key().to_bytes();
    let pseudo = st
        .conns
        .get(&g)
        .and_then(|c| c.pseudo.clone())
        .unwrap_or_default();
    let signer = super::account_key(st, g).unwrap_or([0; 32]);
    let Some(room) = st.rooms.get(&rid) else {
        return;
    };
    let Some(r) = room.provinces.get(&p) else {
        return;
    };
    let Some(info) = r.info.clone() else {
        return;
    };
    let epoch = r.epoch;
    let secret = room.shard_secret;
    let token = proto::follow_token(&secret, rid, p, &pseudo, &signer, epoch);
    let task = task_info(room);
    let server = room.server.clone();
    let settings = settings_of(st, &server);
    let room = st.rooms.get_mut(&rid).expect("partie");
    room.members.push(g);
    room.hidden.insert(g);
    if let Some(r) = room.provinces.get_mut(&p) {
        r.standby = Some(Standby {
            host: g,
            ready: false,
            since: Instant::now(),
        });
    }
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
            info: task,
            settings,
            follow: Some(Box::new(proto::ProvinceAt { info, token })),
        },
    );
    log::info!("monde {rid} : relais de secours de la province {p:?} confié à {g}");
}

/// Relais du monde : le relais de secours de `p` est relevé.
fn drop_standby(st: &mut State, rid: u64, p: [i32; 2]) {
    let Some(r) = st
        .rooms
        .get_mut(&rid)
        .and_then(|room| room.provinces.get_mut(&p))
    else {
        return;
    };
    let epoch = r.epoch;
    let Some(sb) = r.standby.take() else {
        return;
    };
    r.recent.clear();
    r.recent_bytes = 0;
    r.unfollowed_since = None;
    leave_room(st, sb.host);
    if let Some(c) = st.conns.get(&sb.host) {
        transport::push(
            &c.tx,
            &Response::ProvinceStop {
                room: rid,
                province: p,
                epoch,
            },
        );
    }
}

/// Relais du monde : la copie du relais de secours de `p` est à jour.
pub(crate) fn standby_ready(st: &mut State, rid: u64, id: u64, p: [i32; 2], epoch: u64) {
    if let Some(sb) = st
        .rooms
        .get_mut(&rid)
        .and_then(|room| room.provinces.get_mut(&p))
        .filter(|r| r.epoch == epoch)
        .and_then(|r| r.standby.as_mut())
        .filter(|s| s.host == id)
    {
        if !sb.ready {
            log::info!("monde {rid} : relais de secours de la province {p:?} prêt");
        }
        sb.ready = true;
    }
}

/// Relais du monde (v141) : le relais de la province `p` ne répond plus (ou s'en va) :
/// son relais de secours prend sa place, sous une nouvelle époque, sans que ce relais ait à
/// la servir (seulement le temps qu'il soit prêt). Faux s'il n'y en a pas de prêt (ou si
/// elle doit revenir ici pour de bon).
fn promote(st: &mut State, rid: u64, p: [i32; 2], why: &str) -> bool {
    let Some(room) = st.rooms.get_mut(&rid) else {
        return false;
    };
    let Some(r) = room.provinces.get(&p) else {
        return false;
    };
    // (Vers son relais de secours, sauf arrêt pour de bon ; v142 : toujours quand c'est
    // lui qui a la copie à jour.)
    let Some(sb) = r
        .standby
        .as_ref()
        .filter(|s| s.ready && (!r.winding || r.big || r.stale))
        .map(|s| s.host)
    else {
        return false;
    };
    let stale = r.big || r.stale;
    if !st.conns.contains_key(&sb) {
        return false;
    }
    let (old, old_host) = (r.epoch, r.host);
    let epoch = room.next_epoch;
    room.next_epoch += 1;
    let snap_seq = room.build.last_seq();
    let r = room.provinces.get_mut(&p).expect("province");
    let recent = std::mem::take(&mut r.recent);
    *r = ProvinceRef {
        host: sb,
        epoch,
        info: None,
        since: Instant::now(),
        renewed: Instant::now(),
        snap_seq,
        empty_since: None,
        counts: None,
        inside: Vec::new(),
        standby: None,
        recent: VecDeque::new(),
        recent_bytes: 0,
        carry: Some(recent),
        winding: false,
        big: false,
        followed: false,
        stale,
        dump_n: stale as u64,
        stop_after_dump: false,
        unfollowed_since: None,
    };
    refresh_delegated(room);
    // Ses sacs de survie restent prêtés, à la nouvelle époque : prêtés de nouveau (la copie
    // d'ici est la plus récente ; celle du relais de secours peut avoir du retard).
    let mut lend_again = Vec::new();
    for (who, h) in room.bag_holders.iter_mut() {
        if *h == (p, old) {
            *h = (p, epoch);
            lend_again.push(who.clone());
        }
    }
    for h in room.bag_waiting.values_mut() {
        if *h == (p, old) {
            *h = (p, epoch);
        }
    }
    // L'ancien hôte (tombé, coupé, ou parti) : arrêté pour de bon.
    leave_room(st, old_host);
    if let Some(c) = st.conns.get(&old_host) {
        transport::push(
            &c.tx,
            &Response::ProvinceStop {
                room: rid,
                province: p,
                epoch: old,
            },
        );
    }
    if let Some(c) = st.conns.get(&sb) {
        transport::push(
            &c.tx,
            &Response::ProvincePromote {
                room: rid,
                province: p,
                old,
                epoch,
            },
        );
    }
    for who in lend_again {
        lend(st, rid, &who);
    }
    st.province_promotions += 1;
    log::info!(
        "monde {rid} : province {p:?} confiée à son relais de secours ({why}, époque {epoch})"
    );
    announce(st, rid, p);
    true
}

/// Une connexion quitte la salle `rid` : relais de secours (relais du monde) ou copie qui
/// suit la province (relais de province).
pub(crate) fn left(st: &mut State, rid: u64, id: u64) {
    let Some(room) = st.rooms.get_mut(&rid) else {
        return;
    };
    for r in room.provinces.values_mut() {
        if r.standby.as_ref().is_some_and(|s| s.host == id) {
            r.standby = None;
            r.recent.clear();
            r.recent_bytes = 0;
        }
    }
    if let Some(pr) = room.province.as_mut() {
        pr.followers.retain(|f| *f != id);
        if pr.followers.is_empty() {
            pr.copies.clear();
        }
    }
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
    settings: proto::ProvinceSettings,
    standby: bool,
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
        held: HashSet::new(),
        asked: HashMap::new(),
        requests: Vec::new(),
        spots: HashMap::new(),
        last_spots: 0,
        loose: Vec::new(),
        standby,
        have: 0,
        followers: Vec::new(),
        acked: 0,
        copies: VecDeque::new(),
        to_root: true,
        dump_asked: 0,
        dump_done: 0,
        unconfirmed: VecDeque::new(),
        acked_copy: 0,
        dump_archive: 0,
        covered: 0,
    });
    // (Réglages du serveur : OP, bannis, mode par défaut ; sacs de survie suivis, pour la
    // copie envoyée au relais du monde.)
    st.servers.mirror(&info.server, &settings);
    for (who, m) in &settings.modes {
        st.bags.set_mode(&info.server, who, m);
    }
    st.bags.watch(&info.server, true);
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
            // (Objets au sol et coffres : retrouvés à partir des états publiés.)
            if let Some(m) = minelib_net::game::decode::<minelib_net::game::BuildMsg>(&data) {
                room.build.items.absorb(&m);
            }
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
    let server = room.server.clone();
    // (v142) Le relais du monde n'a plus une copie à jour : tout ce qu'elle a lui repart.
    let dump = room
        .province
        .as_ref()
        .filter(|p| p.to_root && p.dump_asked > p.dump_done)
        .map(|p| p.dump_asked);
    let all = dump.map(|_| room.build.entries_after(|_| true, 0));
    let Some(pr) = room.province.as_mut() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    // (Relais de secours : il suit, ne sert pas.)
    if !pr.go || pr.stopped || pr.standby {
        return out;
    }
    let (p, epoch) = (pr.province, pr.epoch);
    pr.outbox.extend(entries);
    // (Sacs de survie prêtés qui ont changé : leur copie part avec ce qu'elle publie.)
    let (changed, _) = st.bags.take_changed(&server);
    let pr = st
        .rooms
        .get_mut(&rid)
        .and_then(|r| r.province.as_mut())
        .expect("province");
    let bags: Vec<proto::BagCopy> = changed
        .into_iter()
        .filter(|(who, _)| pr.held.contains(who))
        .map(|(who, b)| (who, b.items.into_iter().collect(), b.mined))
        .collect();
    let due = now.saturating_sub(pr.last_archive) >= ARCHIVE_MS;
    let spots_due = !pr.spots.is_empty() && now.saturating_sub(pr.last_spots) >= SPOT_MS;
    // (Un sac changé part tout de suite, avec ce qui a été publié jusque-là : la copie du
    // relais du monde reste cohérente à chaque envoi.)
    let mut copy_now = None;
    if (!pr.outbox.is_empty() || !pr.loose.is_empty()) && due
        || pr.outbox.len() >= SEED_CHUNK
        || !bags.is_empty()
        || spots_due
        || all.is_some()
    {
        pr.last_archive = now;
        let spots = if spots_due {
            pr.last_spots = now;
            std::mem::take(&mut pr.spots).into_iter().collect()
        } else {
            Vec::new()
        };
        let n = pr.next_n;
        pr.next_n += 1;
        let mut entries = std::mem::take(&mut pr.outbox);
        let big = !pr.to_root;
        pr.unconfirmed.push_back((n, big));
        let mut dumped = 0;
        if big {
            // (v142) Gardé par son relais de secours (tout de suite) ; au relais du monde,
            // seulement les sacs, les retours et les blocs comptés.
            copy_now = Some((n, entries.clone(), bags.clone()));
            entries.clear();
        } else {
            if let (Some(d), Some(all)) = (dump, all) {
                entries = all;
                dumped = d;
                pr.dump_done = d;
                pr.dump_archive = n;
            }
            // (v141) Gardé pour ses relais de secours : il leur partira une fois gardé par
            // le relais du monde.
            if !pr.followers.is_empty() {
                pr.copies.push_back((n, entries.clone(), bags.clone()));
                while pr.copies.len() > COPIES_MAX {
                    pr.copies.pop_front();
                }
            }
        }
        out.push(Request::ProvinceArchive {
            room: rid,
            province: p,
            epoch,
            n,
            entries,
            bags,
            spots,
            loose: std::mem::take(&mut pr.loose),
            dump: dumped,
        });
        // Ses joueurs : ce qu'ils ont reçu jusqu'ici fait partie de cet envoi.
        for m in players {
            if let Some(c) = st.conns.get(&m) {
                transport::push(&c.tx, &Response::ProvinceMark { n });
            }
        }
    }
    if let Some((n, entries, bags)) = copy_now {
        send_copy(st, rid, n, entries, bags);
    }
    let Some(pr) = st.rooms.get_mut(&rid).and_then(|r| r.province.as_mut()) else {
        return out;
    };
    // Sacs demandés ou rendus (après la copie : un sac rendu part après ce qu'il a changé).
    out.append(&mut pr.requests);
    if now.saturating_sub(pr.last_ask) >= proto::PROVINCE_RENEW_MS {
        pr.last_ask = now;
        // (v140) Ses joueurs par région, et les hôtes de ses relais de région entrés dans
        // la région qu'ils servent : le relais du monde ne voit plus leurs positions.
        let (counts, inside) = census(st, rid);
        let followed = st
            .rooms
            .get(&rid)
            .and_then(|r| r.province.as_ref())
            .is_some_and(|p| !p.followers.is_empty());
        out.push(Request::ProvinceLease {
            room: rid,
            province: p,
            epoch,
            t: now,
            counts,
            inside,
            followed,
        });
    }
    let Some(pr) = st.rooms.get_mut(&rid).and_then(|r| r.province.as_mut()) else {
        return out;
    };
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

/// Relais de province (v140) : joueurs de chaque région peuplée de la province, et relais
/// de région dont l'hôte joue dans la région qu'il sert (région, rang).
type Census = (Vec<([i32; 2], u32)>, Vec<([i32; 2], u16)>);

fn census(st: &mut State, rid: u64) -> Census {
    let Some(room) = st.rooms.get_mut(&rid) else {
        return (Vec::new(), Vec::new());
    };
    let Some(pr) = room.province.as_ref() else {
        return (Vec::new(), Vec::new());
    };
    let (prov, size) = (pr.province, pr.size);
    let mut counts: Vec<([i32; 2], u32)> = room
        .build
        .region_counts()
        .iter()
        .filter(|(r, _)| proto::province_of(**r, size) == prov)
        .map(|(r, n)| (*r, *n))
        .collect();
    counts.sort_unstable();
    let room = st.rooms.get(&rid).expect("province");
    let mut inside = Vec::new();
    for (r, list) in &room.shards {
        for sh in list {
            if super::host_in_region(st, room, *r, sh.host) {
                inside.push((*r, sh.slot));
            }
        }
    }
    inside.sort_unstable();
    (counts, inside)
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
    // (v141) Ses relais de secours reçoivent ce que le relais du monde a gardé (jamais
    // plus : leur copie reste en deçà de la sienne).
    let room = st.rooms.get_mut(&rid).expect("province");
    let Some(pr) = room.province.as_mut() else {
        return;
    };
    pr.acked = pr.acked.max(n);
    if pr.dump_archive > 0 && pr.dump_archive <= n {
        pr.covered = pr.covered.max(pr.dump_archive);
    }
    let p = pr.province;
    let held: Vec<String> = pr.held.iter().cloned().collect();
    let mut out = Vec::new();
    while pr.copies.front().is_some_and(|(m, _, _)| *m <= n) {
        out.push(pr.copies.pop_front().expect("envoi"));
    }
    let followers = pr.followers.clone();
    for (m, entries, bags) in out {
        let msg = Response::ProvinceCopy {
            room: rid,
            province: p,
            epoch,
            n: m,
            entries,
            bags,
            held: held.clone(),
        };
        for f in &followers {
            if let Some(c) = st.conns.get(f) {
                transport::push(&c.tx, &msg);
            }
        }
    }
    confirm(st, rid);
}

/// Relais de province (v142) : son relais de secours a copié l'envoi `n`.
pub(crate) fn copied(st: &mut State, id: u64, rid: u64, epoch: u64, n: u64) {
    let Some(pr) = st
        .rooms
        .get_mut(&rid)
        .and_then(|r| r.province.as_mut())
        .filter(|p| p.epoch == epoch && p.followers.contains(&id))
    else {
        return;
    };
    pr.acked_copy = pr.acked_copy.max(n);
    confirm(st, rid);
}

/// Relais de province : les envois gardés (par le relais du monde, ou par son relais de
/// secours quand c'est lui qui garde la copie) sont confirmés à ses joueurs, dans l'ordre.
fn confirm(st: &mut State, rid: u64) {
    let Some(room) = st.rooms.get_mut(&rid) else {
        return;
    };
    let Some(pr) = room.province.as_mut() else {
        return;
    };
    let mut last = None;
    while let Some(&(n, big)) = pr.unconfirmed.front() {
        let kept = n <= pr.covered
            || if big {
                n <= pr.acked_copy
            } else {
                n <= pr.acked
            };
        if !kept {
            break;
        }
        pr.unconfirmed.pop_front();
        last = Some(n);
    }
    let Some(n) = last else {
        return;
    };
    let room = st.rooms.get(&rid).expect("province");
    for m in room.members.iter().filter(|m| !room.hidden.contains(m)) {
        if let Some(c) = st.conns.get(m) {
            transport::push(&c.tx, &Response::ProvinceConfirmed { n });
        }
    }
}

/// Relais de province (v142) : un envoi à ses relais de secours, tout de suite (c'est eux
/// qui en gardent la copie).
fn send_copy(
    st: &mut State,
    rid: u64,
    n: u64,
    entries: Vec<super::zones::Entry>,
    bags: Vec<proto::BagCopy>,
) {
    let Some(pr) = st.rooms.get(&rid).and_then(|r| r.province.as_ref()) else {
        return;
    };
    let msg = Response::ProvinceCopy {
        room: rid,
        province: pr.province,
        epoch: pr.epoch,
        n,
        entries,
        bags,
        held: pr.held.iter().cloned().collect(),
    };
    for f in &pr.followers {
        if let Some(c) = st.conns.get(f) {
            transport::push(&c.tx, &msg);
        }
    }
}

/// Relais de province (v141) : un relais de secours entre (jeton du relais du monde) ; il
/// reçoit tout ce qu'elle a, puis chaque envoi gardé par le relais du monde.
#[allow(clippy::too_many_arguments)]
pub(crate) fn follow(
    st: &mut State,
    session: &mut Session,
    id: u64,
    room: u64,
    p: [i32; 2],
    epoch: u64,
    pseudo: String,
    signer: [u8; 32],
    token: [u8; 32],
) -> Response {
    let ok = st.province_room == Some(room)
        && st.rooms.get(&room).is_some_and(|r| {
            r.province.as_ref().is_some_and(|pr| {
                pr.province == p
                    && pr.epoch == epoch
                    && !pr.standby
                    && !pr.stopped
                    && proto::follow_token(&r.shard_secret, room, p, &pseudo, &signer, epoch)
                        == token
            })
        });
    if !ok || pseudo.starts_with('#') {
        return err(
            ErrorCode::WrongCredentials,
            "jeton de relais de secours refusé",
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
    // Tout ce qu'elle a (ce qui part ensuite au relais du monde lui reviendra aussi).
    let entries = r.build.entries_after(|_| true, 0);
    let server = r.server.clone();
    let pr = r.province.as_mut().expect("province");
    pr.followers.push(id);
    let (acked, held): (u64, Vec<String>) = (pr.acked, pr.held.iter().cloned().collect());
    let bags: Vec<proto::BagCopy> = held
        .iter()
        .map(|who| {
            let b = st.bags.get(&server, who);
            (who.clone(), b.items.into_iter().collect(), b.mined)
        })
        .collect();
    if let Some(c) = st.conns.get(&id) {
        for part in entries.chunks(SEED_CHUNK) {
            transport::push(
                &c.tx,
                &Response::ProvinceSeed {
                    room,
                    province: p,
                    epoch,
                    entries: part.to_vec(),
                    go: None,
                },
            );
        }
        transport::push(
            &c.tx,
            &Response::ProvinceCopy {
                room,
                province: p,
                epoch,
                n: acked,
                entries: Vec::new(),
                bags,
                held,
            },
        );
    }
    log::info!("relais de province : relais de secours de {pseudo} entré");
    Response::Welcome {
        pseudo,
        totp: false,
    }
}

/// Relais de secours (v141) : un envoi gardé par le relais du monde, copié ici.
pub(crate) fn copy(
    st: &mut State,
    epoch: u64,
    n: u64,
    entries: Vec<super::zones::Entry>,
    bags: Vec<proto::BagCopy>,
    held: Vec<String>,
) -> Option<u64> {
    let rid = st.province_room?;
    let room = st.rooms.get_mut(&rid)?;
    let server = room.server.clone();
    let pr = room.province.as_ref()?;
    if !pr.standby || pr.epoch != epoch {
        return None;
    }
    let (prov, size) = (pr.province, pr.size);
    for (zone, seq, data) in entries {
        if proto::province_of(zone_region(zone), size) == prov {
            room.build.mirror_at(zone, &data, Some(seq), &[]);
            if let Some(m) = minelib_net::game::decode::<minelib_net::game::BuildMsg>(&data) {
                room.build.items.absorb(&m);
            }
        }
    }
    let pr = room.province.as_mut()?;
    pr.have = pr.have.max(n);
    pr.held = held.into_iter().collect();
    let have = pr.have;
    for (who, items, mined) in bags {
        st.bags.put(
            &server,
            &who,
            super::bags::Bag {
                items: items.into_iter().collect(),
                mined,
            },
        );
    }
    Some(have)
}

/// Relais de secours (v141) : il prend la place du relais de la province (époque
/// `epoch`) ; renvoie jusqu'où il a la copie (le relais du monde lui enverra le reste).
pub(crate) fn promote_here(st: &mut State, old: u64, epoch: u64) -> Option<u64> {
    let rid = st.province_room?;
    let room = st.rooms.get_mut(&rid)?;
    let pr = room.province.as_mut()?;
    if !pr.standby || pr.epoch != old || pr.stopped {
        return None;
    }
    pr.standby = false;
    pr.epoch = epoch;
    pr.go = false;
    pr.lease_until = 0;
    pr.last_ask = 0;
    pr.serving = false;
    pr.ready_sent.clear();
    pr.next_n = 1;
    pr.asked.clear();
    // (Les sacs qu'elle tenait lui sont prêtés de nouveau par le relais du monde, qui en a
    // la copie la plus récente.)
    pr.held.clear();
    log::info!(
        "relais de secours : il prend la place du relais de la province {:?} (époque {epoch})",
        pr.province
    );
    Some(pr.have)
}

/// Relais de province : réponse du relais du monde à une demande de bail faite à `t`.
pub(crate) fn leased(st: &mut State, epoch: u64, t: u64, ok: bool, copies: bool, dump: u64) {
    let Some(rid) = st.province_room else {
        return;
    };
    let Some(pr) = st.rooms.get_mut(&rid).and_then(|r| r.province.as_mut()) else {
        return;
    };
    if ok && epoch == pr.epoch && !pr.stopped {
        pr.lease_until = pr.lease_until.max(t + proto::PROVINCE_LEASE_MS);
        // (v142) Qui garde la copie de ce qu'elle publie : le relais du monde, ou son relais
        // de secours ; quand le relais du monde la garde de nouveau, tout lui repart.
        pr.dump_asked = pr.dump_asked.max(dump);
        if !copies && pr.to_root {
            // Ce qui attendait que le relais du monde le garde part tout de suite au relais
            // de secours.
            let flush: Vec<_> = pr.copies.drain(..).collect();
            pr.to_root = false;
            for (n, entries, bags) in flush {
                send_copy(st, rid, n, entries, bags);
            }
        } else if copies && !pr.to_root {
            pr.to_root = true;
        }
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
    let server = room.server.clone();
    let Some(pr) = room.province.as_mut() else {
        return Vec::new();
    };
    if pr.stopped {
        return Vec::new();
    }
    pr.stopped = true;
    // (Relais de secours : rien à rendre, il ne servait pas.)
    if pr.standby {
        log::info!("relais de secours de la province {:?} arrêté", pr.province);
        return Vec::new();
    }
    let (p, epoch) = (pr.province, pr.epoch);
    pr.outbox.extend(entries);
    // (Les sacs prêtés repartent tels qu'ils sont, avec le reste.)
    let held: Vec<String> = pr.held.drain().collect();
    let bags: Vec<proto::BagCopy> = held
        .into_iter()
        .map(|who| {
            let b = st.bags.get(&server, &who);
            (who, b.items.into_iter().collect(), b.mined)
        })
        .collect();
    let pr = st
        .rooms
        .get_mut(&rid)
        .and_then(|r| r.province.as_mut())
        .expect("province");
    let mut out = Vec::new();
    if release {
        if !pr.outbox.is_empty() || !bags.is_empty() || !pr.spots.is_empty() || !pr.loose.is_empty()
        {
            let n = pr.next_n;
            pr.next_n += 1;
            out.push(Request::ProvinceArchive {
                room: rid,
                province: p,
                epoch,
                n,
                entries: std::mem::take(&mut pr.outbox),
                bags,
                spots: std::mem::take(&mut pr.spots).into_iter().collect(),
                loose: std::mem::take(&mut pr.loose),
                dump: 0,
            });
        }
        out.push(Request::ProvinceRelease {
            room: rid,
            province: p,
            epoch,
        });
    }
    // Joueurs et relais de région : la province est fermée ici.
    let members: Vec<u64> = st
        .rooms
        .get(&rid)
        .map(|r| r.members.clone())
        .unwrap_or_default();
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
    if pr.stopped || pr.standby {
        return;
    }
    let serving = pr.serving(now);
    let changed = serving != pr.serving;
    pr.serving = serving;
    if !changed && now.saturating_sub(pr.last_beat) < BEAT_MS {
        return;
    }
    pr.last_beat = now;
    bags_upkeep(st, rid);
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
    let ok = cert.valid(&pr.root, rid)
        && cert.expires >= super::now_secs()
        && !pr.stopped
        && !pr.standby;
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
    // (Sans bail : une demande d'objet est refusée, une modification de bloc attend.)
    if guards::frozen(st, rid) {
        if let Some(seq) = msg
            .as_ref()
            .filter(|m| minelib_net::items::is_request(m))
            .and_then(minelib_net::items::request_seq)
        {
            super::publish_items(st, rid, id, vec![BuildMsg::Denied { seq }]);
        }
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
    // Objets et coffres de la province (v139) : ce relais arbitre ; un joueur en survie
    // attend son sac (prêté par le relais du monde).
    if let Some(m) = msg.as_ref().filter(|m| minelib_net::items::is_request(m)) {
        let mine = room
            .build
            .items
            .target_chunk(m)
            .is_some_and(|c| pr.has(proto::region_of(c)));
        // (Hors de ses zones, ou objet qu'elle ne connaît pas : refusée, le jeu la refera
        // auprès de qui en décide.)
        if !mine || !bag_needed(st, rid, id, pseudo) {
            let seq = minelib_net::items::request_seq(m).unwrap_or(0);
            super::publish_items(st, rid, id, vec![BuildMsg::Denied { seq }]);
            return;
        }
        super::item_request(st, rid, id, pseudo, m);
        return;
    }
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

/// Relais de province (v139) : un joueur en survie se trouve dans la province ; son sac,
/// s'il ne l'a pas déjà (ou ne l'a pas déjà demandé), est demandé au relais du monde.
/// Vrai si ce relais a son sac (il peut arbitrer ses objets).
pub(crate) fn bag_needed(st: &mut State, rid: u64, id: u64, pseudo: &str) -> bool {
    let Some(room) = st.rooms.get(&rid) else {
        return false;
    };
    let Some(pr) = room.province.as_ref() else {
        return false;
    };
    let (server, creator) = (room.server.clone(), room.creator.clone());
    if !super::in_survival(st, &server, &creator, pseudo) {
        return true;
    }
    let who = pseudo.to_ascii_lowercase();
    if pr.held.contains(&who) {
        return true;
    }
    let inside = room
        .build
        .positions
        .get(&id)
        .is_some_and(|c| pr.has(proto::region_of(*c)));
    let now = guards::now_ms(st);
    let pr = st
        .rooms
        .get_mut(&rid)
        .and_then(|r| r.province.as_mut())
        .expect("province");
    // (Demandé sans réponse depuis longtemps : redemandé.)
    let fresh = pr
        .asked
        .get(&who)
        .is_none_or(|t| now.saturating_sub(*t) >= BAG_ASK_MS);
    if inside && !pr.stopped && fresh {
        pr.asked.insert(who, now);
        let (p, epoch) = (pr.province, pr.epoch);
        pr.requests.push(Request::ProvinceBag {
            room: rid,
            province: p,
            epoch,
            pseudo: pseudo.to_string(),
        });
    }
    false
}

/// Relais de province : un sac prêté par le relais du monde (lui seul s'en sert ici).
pub(crate) fn bag_lent(
    st: &mut State,
    epoch: u64,
    pseudo: &str,
    items: Vec<(u32, u32)>,
    mined: u32,
    mode: Option<String>,
) {
    let Some(rid) = st.province_room else {
        return;
    };
    let Some(room) = st.rooms.get_mut(&rid) else {
        return;
    };
    let server = room.server.clone();
    let Some(pr) = room
        .province
        .as_mut()
        .filter(|p| p.epoch == epoch && !p.stopped)
    else {
        return;
    };
    let who = pseudo.to_ascii_lowercase();
    pr.asked.remove(&who);
    // (Déjà ici : ce prêt est un doublon en retard, plus ancien que ce qu'elle en a fait.)
    if !pr.held.insert(who) {
        return;
    }
    st.bags.put(
        &server,
        pseudo,
        super::bags::Bag {
            items: items.into_iter().collect(),
            mined,
        },
    );
    if let Some(m) = mode {
        st.bags.set_mode(&server, pseudo, &m);
    }
}

/// Relais de province : un sac à rendre (redemandé par le relais du monde, ou son joueur
/// n'est plus dans la province). Il n'en est plus fait usage ici.
pub(crate) fn bag_back(st: &mut State, pseudo: &str) {
    let Some(rid) = st.province_room else {
        return;
    };
    let Some(room) = st.rooms.get(&rid) else {
        return;
    };
    let server = room.server.clone();
    let who = pseudo.to_ascii_lowercase();
    let b = st.bags.get(&server, &who);
    let Some(pr) = st.rooms.get_mut(&rid).and_then(|r| r.province.as_mut()) else {
        return;
    };
    pr.asked.remove(&who);
    if pr.held.remove(&who) {
        let (p, epoch) = (pr.province, pr.epoch);
        pr.requests.push(Request::ProvinceBagBack {
            room: rid,
            province: p,
            epoch,
            pseudo: who,
            items: b.items.into_iter().collect(),
            mined: b.mined,
        });
    }
}

/// Relais de province (avec le battement) : les sacs dont le joueur n'est plus connecté ici,
/// ou plus dans la province, sont rendus.
fn bags_upkeep(st: &mut State, rid: u64) {
    let Some(room) = st.rooms.get(&rid) else {
        return;
    };
    let Some(pr) = room.province.as_ref() else {
        return;
    };
    let gone: Vec<String> = pr
        .held
        .iter()
        .filter(|who| {
            room.by_pseudo.get(*who).is_none_or(|id| {
                room.build
                    .positions
                    .get(id)
                    .is_none_or(|c| !pr.has(proto::region_of(*c)))
            })
        })
        .cloned()
        .collect();
    for who in gone {
        bag_back(st, &who);
    }
    // Les joueurs en survie dans la province : leur sac est demandé avant qu'ils n'en aient
    // besoin.
    let Some(room) = st.rooms.get(&rid) else {
        return;
    };
    let here: Vec<(u64, String)> = room
        .members
        .iter()
        .filter(|m| !room.hidden.contains(m))
        .filter_map(|m| Some((*m, st.conns.get(m)?.pseudo.clone()?)))
        .collect();
    for (id, pseudo) in here {
        bag_needed(st, rid, id, &pseudo);
    }
}

/// Relais de province : un joueur redit où il est (gardé pour son retour, transmis au
/// relais du monde).
pub(crate) fn spot(st: &mut State, rid: u64, pseudo: &str, at: [f32; 5]) {
    if let Some(pr) = st.rooms.get_mut(&rid).and_then(|r| r.province.as_mut()) {
        pr.spots.insert(pseudo.to_ascii_lowercase(), at);
    }
}

/// Relais de province : WorldEdit d'un OP, transmis par le relais du monde (chaque case
/// signée par lui à la place de l'OP) : à l'hôte principal d'une région souveraine, sinon
/// publié ici.
pub(crate) fn bulk(
    st: &mut State,
    epoch: u64,
    pseudo: &str,
    edits: Vec<minelib_net::sovereign::Edit>,
) {
    let Some(rid) = st.province_room else {
        return;
    };
    let Some(room) = st.rooms.get(&rid) else {
        return;
    };
    let Some(pr) = room.province.as_ref().filter(|p| p.epoch == epoch) else {
        return;
    };
    let mut sov: HashMap<[i32; 2], Vec<minelib_net::sovereign::Edit>> = HashMap::new();
    let mut plain = Vec::new();
    for e in edits {
        let region = proto::region_of([e.pos[0].div_euclid(16), e.pos[2].div_euclid(16)]);
        if !pr.has(region) || !e.by_root || !(0..256).contains(&e.pos[1]) {
            continue;
        }
        if room.sov.contains_key(&region) {
            sov.entry(region).or_default().push(e);
        } else {
            plain.push(e);
        }
    }
    for (region, edits) in sov {
        let primary = st
            .rooms
            .get(&rid)
            .and_then(|r| r.sov.get(&region))
            .map(|sv| sv.primary);
        if let Some(c) = primary.and_then(|h| st.conns.get(&h)) {
            transport::push(
                &c.tx,
                &Response::RootEdits {
                    region,
                    edits,
                    certs: Vec::new(),
                },
            );
        }
    }
    for e in plain {
        if super::publish_block(st, rid, 0, false, pseudo, e.pos, e.block, false) {
            spill_chest(st, rid, e.pos);
        }
    }
}

/// Relais de province : réglages du serveur changés (OP, bannis, mode par défaut). Un
/// joueur banni est mis dehors.
pub(crate) fn settings(st: &mut State, settings: proto::ProvinceSettings) {
    let Some(rid) = st.province_room else {
        return;
    };
    let Some(room) = st.rooms.get(&rid) else {
        return;
    };
    let server = room.server.clone();
    let banned: Vec<u64> = room
        .members
        .iter()
        .filter(|m| !room.hidden.contains(m))
        .filter(|m| {
            st.conns
                .get(m)
                .and_then(|c| c.pseudo.as_deref())
                .is_some_and(|p| settings.bans.iter().any(|b| b.eq_ignore_ascii_case(p)))
        })
        .copied()
        .collect();
    st.servers.mirror(&server, &settings);
    for (who, m) in &settings.modes {
        st.bags.set_mode(&server, who, m);
    }
    for id in banned {
        leave_room(st, id);
        if let Some(c) = st.conns.get(&id) {
            transport::push(&c.tx, &Response::RoomClosed);
        }
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
