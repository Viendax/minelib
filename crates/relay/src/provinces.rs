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
/// (v143) Plage de rangs d'un pays (il en donne une à chaque époque de ses provinces), et
/// d'une province d'un pays.
const PAYS_SEQS: u64 = 1 << 48;
const CHILD_SEQS: u64 = 1 << 34;
/// (v143) Bail d'une province d'un pays (ms) : donné seulement si celui du pays court encore
/// au moins `CHILD_LEASE_MS + CHILD_GUARD_MS` ; il finit donc toujours avant lui.
const CHILD_LEASE_MS: u64 = 1000;
const CHILD_GUARD_MS: u64 = 100;
/// (v143) Hôtes donnés au plus pour un même bail de pays.
const WANTS_MAX: usize = 4;
/// (v144) Relais de secours d'un pays qui prend sa place : ses provinces ont ce temps pour
/// se rattacher à lui (au-delà, il les reprend ou les confie à leur relais de secours).
const REATTACH_GRACE: Duration = Duration::from_secs(1);
/// (v143) Sac de survie qu'un pays attend d'une de ses provinces : pour le rendre au relais
/// du monde (`UP`), pour s'en servir lui-même (`HERE`).
const UP: ([i32; 2], u64) = ([i32::MIN, i32::MIN], 0);
const HERE: ([i32; 2], u64) = ([i32::MIN, i32::MIN], 1);

/// Position de retour d'un joueur (pseudo, x, y, z, lacet, tangage).
type Spot = (String, [f32; 5]);

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
    /// (v143) Pays (il confie lui-même ses provinces) ; ses provinces servies, d'après lui.
    pub nest: bool,
    pub children: Vec<ProvinceInfo>,
    /// (v144) Relais de secours d'un pays qui a pris sa place : l'hôte (pseudo) qui doit se
    /// rattacher ici avec la province qu'il sert déjà.
    expect: Option<String>,
}

/// (v141) Relais de secours d'une province, choisi par le relais du monde.
pub(crate) struct Standby {
    /// Connexion de garant de son hôte.
    pub host: u64,
    /// Sa copie est à jour : il peut prendre la place du relais de la province.
    pub ready: bool,
    since: Instant,
    /// (v144) Celui qui doit se rattacher (relais de secours d'un pays qui a pris sa place).
    expect: Option<String>,
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
    /// (v143) Pays : il confie lui-même ses provinces peuplées. Envois de ses provinces
    /// gardés ici, à confirmer à chacune quand l'envoi qui les porte plus haut est gardé à
    /// son tour (numéro de cet envoi, province, époque, numéro de son envoi) : ce qui est
    /// confirmé est toujours sur deux machines. Dernier envoi confirmé ; arrêt demandé, qui
    /// attend que ses provinces lui aient tout rendu ; dernière liste de ses provinces
    /// servies dite au relais du monde.
    pub nest: Option<proto::Nest>,
    child_acks: VecDeque<(u64, [i32; 2], u64, u64)>,
    confirmed: u64,
    stopping: bool,
    children_sent: Option<Vec<ProvinceInfo>>,
    /// (v144) Où est son relais de secours (dit à ses provinces) ; ce qu'il sait de ses
    /// provinces (dit à son relais de secours) ; relais de secours : ce que son pays lui en a
    /// dit.
    follower_at: Option<ProvinceInfo>,
    children_said: Option<Vec<proto::ChildCopy>>,
    children_copy: Vec<proto::ChildCopy>,
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

    /// Arrêtée pour de bon.
    pub fn is_stopped(&self) -> bool {
        self.stopped
    }
}

fn zone_region(zone: [i32; 2]) -> [i32; 2] {
    proto::region_of([zone[0] * proto::ZONE, zone[1] * proto::ZONE])
}

/// (v143) Ce relais tient un pays qui peut donner des baux à ses provinces : il sert, avec
/// assez de bail devant lui, et ne s'arrête pas.
fn parent_ok(st: &State, rid: u64) -> bool {
    let now = guards::now_ms(st);
    st.province_room == Some(rid)
        && st
            .rooms
            .get(&rid)
            .and_then(|r| r.province.as_ref())
            .is_some_and(|p| {
                p.nest.is_some()
                    && p.go
                    && !p.stopped
                    && !p.standby
                    && !p.stopping
                    && p.lease_until >= now + CHILD_LEASE_MS + CHILD_GUARD_MS
            })
}

/// (v143) Ce relais tient un pays (et `rid` en est la salle).
fn pays_here(st: &State, rid: u64) -> bool {
    st.province_room == Some(rid)
        && st
            .rooms
            .get(&rid)
            .and_then(|r| r.province.as_ref())
            .is_some_and(|p| p.nest.is_some())
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
    // (v143) Région d'une province d'un pays : elle s'y rattache (la plus petite qui la
    // contient décide de ses blocs).
    let info = p
        .children
        .iter()
        .find(|c| c.has(region))
        .cloned()
        .or_else(|| p.info.clone())?;
    let signer = super::registry_key(st, pseudo).unwrap_or([0; 32]);
    Some(proto::ProvinceAt {
        token: proto::province_token(
            &room.shard_secret,
            rid,
            region,
            slot,
            pseudo,
            &signer,
            info.epoch,
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
    // (Une province ne se découpe pas elle-même ; v143 : un pays, si, tant qu'il sert.)
    let pays = pays_here(st, rid);
    if room.kind != 1 || (room.province.is_some() && !pays) || guards::frozen(st, rid) {
        return;
    }
    if pays && !parent_ok(st, rid) {
        return;
    }
    // Joueurs par province : ceux que ce relais voit (comptés par région à chaque
    // déplacement), et (v140) ceux que compte chaque province servie (leurs positions ne
    // passent plus par ici). (Un pays : seulement les siennes.)
    let room = st.rooms.get_mut(&rid).expect("partie");
    let counted = counted_provinces(room);
    let mut count: HashMap<[i32; 2], usize> = HashMap::new();
    for (r, n) in room.build.region_counts() {
        if room.province.as_ref().is_some_and(|pr| !pr.has(*r)) {
            continue;
        }
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
    // (v143) Un pays demande ses hôtes au relais du monde, avec son prochain bail.
    if pays {
        for p in wanted {
            delegate_pending(st, rid, p);
        }
        return;
    }
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
    // (v143) Un pays, si ce relais en confie.
    let nest = st.nest;
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
            nest: nest.is_some(),
            children: Vec::new(),
            expect: None,
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
            parent: None,
            nest,
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

/// (v143) Relais de pays : sa province `p` est peuplée. Le relais du monde lui choisira un
/// hôte (demandé avec le prochain bail) et lui donnera sa tâche ; l'historique lui partira
/// quand il entrera ici (`child_hello`).
fn delegate_pending(st: &mut State, rid: u64, p: [i32; 2]) {
    let Some(room) = st.rooms.get_mut(&rid) else {
        return;
    };
    let epoch = room.next_epoch;
    room.next_epoch += 1;
    let snap_seq = room.build.last_seq();
    room.provinces.insert(
        p,
        ProvinceRef {
            host: 0,
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
            nest: false,
            children: Vec::new(),
            expect: None,
        },
    );
    log::info!("pays : province {p:?} à confier (époque {epoch})");
}

/// (v143) Relais de pays : les hôtes qu'il attend (provinces à confier, relais de secours de
/// ses provinces servies), redemandés à chaque bail jusqu'à ce qu'ils entrent.
fn wants(st: &State, rid: u64) -> Vec<proto::Want> {
    let size = size(st);
    let Some(room) = st.rooms.get(&rid) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (p, r) in &room.provinces {
        // (v144) Ceux qui doivent se rattacher ne sont pas redemandés.
        if r.expect.is_some() {
            continue;
        }
        if r.host == 0 && r.info.is_none() {
            out.push(proto::Want {
                province: *p,
                size,
                epoch: r.epoch,
                follow: None,
            });
        }
        if let (Some(sb), Some(info)) = (&r.standby, &r.info) {
            if sb.host == 0 && sb.expect.is_none() {
                out.push(proto::Want {
                    province: *p,
                    size,
                    epoch: r.epoch,
                    follow: Some(info.clone()),
                });
            }
        }
    }
    out.sort_by_key(|w| (w.province, w.follow.is_some()));
    out
}

/// (v143) Relais du monde : un pays attend des hôtes ; chacun est choisi parmi les
/// connexions de garant (hors de la province qu'il servira) et reçoit sa tâche, avec le
/// jeton qui le fait entrer chez le relais du pays.
fn serve_wants(st: &mut State, rid: u64, p: [i32; 2], wants: Vec<proto::Want>) {
    let size = size(st);
    let Some(nest) = st.nest else {
        return;
    };
    let Some(pays) = st
        .rooms
        .get(&rid)
        .and_then(|room| room.provinces.get(&p))
        .filter(|r| r.nest)
        .and_then(|r| r.info.clone())
    else {
        return;
    };
    st.wants_served
        .retain(|_, t| t.elapsed() < START_WAIT + Duration::from_secs(5));
    let mut todo = Vec::new();
    for w in wants.into_iter().take(64) {
        let first = [w.province[0] * w.size, w.province[1] * w.size];
        if w.size != nest.size || proto::province_of(first, size) != p {
            continue;
        }
        if w.follow.as_ref().is_some_and(|i| i.province != w.province) {
            continue;
        }
        let key = (rid, w.province, w.epoch, w.follow.is_some());
        if st.wants_served.contains_key(&key) {
            continue;
        }
        todo.push((key, w));
        if todo.len() >= WANTS_MAX {
            break;
        }
    }
    if todo.is_empty()
        || st
            .hosts_wait
            .is_some_and(|t| t.elapsed() < super::HOSTS_WAIT)
    {
        return;
    }
    let index = super::players_index(st);
    for (key, w) in todo {
        let (cp, cs) = (w.province, w.size);
        let outside = move |chunk: [i32; 2]| proto::province_of(proto::region_of(chunk), cs) != cp;
        let Some(g) = super::pick_host(st, &index, rid, &outside) else {
            st.hosts_wait = Some(Instant::now());
            break;
        };
        st.wants_served.insert(key, Instant::now());
        give_child_task(st, rid, &pays, w, g);
    }
}

/// (v143) Relais du monde : la connexion de garant `g` tiendra la province d'un pays (ou son
/// relais de secours) ; tout passera ensuite par le relais du pays.
fn give_child_task(st: &mut State, rid: u64, pays: &ProvinceInfo, w: proto::Want, g: u64) {
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
    let secret = room.shard_secret;
    let info = task_info(room);
    let server = room.server.clone();
    let settings = settings_of(st, &server);
    let standby = w.follow.is_some();
    let follow = w.follow.map(|i| {
        Box::new(proto::ProvinceAt {
            token: proto::follow_token(&secret, rid, w.province, &pseudo, &signer, w.epoch),
            info: i,
        })
    });
    let parent = Some(Box::new(proto::ProvinceAt {
        info: pays.clone(),
        token: proto::child_token(
            &secret, rid, w.province, w.size, &pseudo, &signer, w.epoch, standby,
        ),
    }));
    let room = st.rooms.get_mut(&rid).expect("partie");
    room.members.push(g);
    room.hidden.insert(g);
    let Some(c) = st.conns.get_mut(&g) else {
        return;
    };
    c.room = Some(rid);
    transport::push(
        &c.tx,
        &Response::ProvinceTask {
            room: rid,
            province: w.province,
            size: w.size,
            epoch: w.epoch,
            secret,
            root,
            info,
            settings,
            follow,
            parent,
            nest: None,
        },
    );
    log::info!(
        "monde {rid} : {} de la province {:?} du pays {:?} confié à {g}",
        if standby {
            "relais de secours"
        } else {
            "relais"
        },
        w.province,
        pays.province
    );
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
    let pays = st.province_room == Some(rid);
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
    // (v143) Plage de rangs : plus grande pour un pays (il en donne à ses provinces), plus
    // petite pour une province d'un pays (prise dans celle du pays).
    let span = if pays {
        CHILD_SEQS
    } else if r.nest {
        PAYS_SEQS
    } else {
        PROVINCE_SEQS
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
    let base = room.build.reserve(span);
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
    wants: Vec<proto::Want>,
    children: Option<Vec<ProvinceInfo>>,
) {
    // (v143) Chez un pays : bail donné seulement s'il en a assez lui-même (celui de sa
    // province finit avant le sien), et toujours avec la copie chez lui.
    let pays = st.province_room == Some(rid);
    let paused = if pays {
        !parent_ok(st, rid)
    } else {
        guards::frozen(st, rid)
    };
    let ms = if pays {
        CHILD_LEASE_MS
    } else {
        proto::PROVINCE_LEASE_MS
    } as u32;
    let size = size(st);
    let copies_until = st.copies_until;
    let nest_size = st.nest.map(|n| n.size);
    let Some(room) = st.rooms.get_mut(&rid) else {
        return;
    };
    // (v142) Monde assez grand pour que la copie de ce que publient les provinces soit
    // gardée par leurs relais de secours plutôt qu'ici.
    // (v144) Chez un pays : quand le pays garde lui-même sa copie chez son relais de
    // secours, ses provinces aussi (rien de ce qui s'y construit ne passe plus par lui).
    let big_world = if pays {
        room.province.as_ref().is_some_and(|p| !p.to_root)
    } else {
        room.members.len().saturating_sub(room.hidden.len()) >= copies_until
    };
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
            // (v143) Un pays : ses provinces servies (le relais du monde y rattache les
            // relais de leurs régions).
            if let (Some(list), Some(cs)) = (children, nest_size.filter(|_| r.nest)) {
                r.children = list
                    .into_iter()
                    .filter(|i| i.size == cs && proto::province_of(i.first_region(), size) == p)
                    .take(4096)
                    .collect();
            }
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
                ms,
            },
        );
    }
    // (v143) Un pays attend des hôtes pour ses provinces.
    if ok && !pays && !wants.is_empty() {
        serve_wants(st, rid, p, wants);
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
    // (v143) Chez un pays, ce qui vient de ses provinces repart plus haut avec ce qu'il
    // publie lui-même (copie de ses zones, sacs qu'il tient, positions de retour).
    let pays = st.province_room == Some(rid);
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
            if pays {
                spot(st, rid, &who, at);
            } else {
                st.servers.set_position(&server, &who, at);
            }
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
    // (v143) Chez un pays : confirmé seulement quand l'envoi qui le porte plus haut est
    // gardé à son tour (toujours sur deux machines).
    if pays {
        if let Some(pr) = st.rooms.get_mut(&rid).and_then(|r| r.province.as_mut()) {
            pr.child_acks.push_back((pr.next_n, p, epoch, n));
        }
        return;
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
    // (v143) Un pays qui ne l'a pas encore : demandé au relais du monde ; il partira à la
    // province dès son arrivée.
    if room
        .province
        .as_ref()
        .is_some_and(|pr| !pr.held.contains(&who))
    {
        room.bag_waiting.insert(who.clone(), (p, epoch));
        ask_bag(st, rid, &who);
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
    // (v143) Un pays qui devait le rendre au relais du monde, ou s'en servir lui-même.
    if next == Some(UP) {
        give_back(st, rid, &who);
        return;
    }
    if next == Some(HERE) {
        return;
    }
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
        // (v143) Un pays qui n'a pas ce sac : compté plus haut.
        if let Some(pr) = room.province.as_mut() {
            if !pr.held.contains(&who) {
                pr.loose.push((who, block));
                return;
            }
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
    // (v143) Un pays qui l'a prêté à une de ses provinces : compté à son retour.
    if room.province.is_some() && room.bag_holders.contains_key(&who) {
        room.bag_deltas.entry(who).or_default().push(block);
        return true;
    }
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
    let pays = st.province_room == Some(rid);
    let Some(room) = st.rooms.get_mut(&rid) else {
        return;
    };
    // (v143) Une province d'un pays : le relais du monde choisit les hôtes de ses régions,
    // le pays lui transmet ; il la connaît par la liste du pays.
    let direct = room
        .provinces
        .get(&p)
        .is_some_and(|r| r.host == id && r.epoch == epoch && r.info.is_some());
    let child = if direct || pays {
        None
    } else {
        room.provinces
            .values()
            .filter(|r| r.host == id && r.nest)
            .flat_map(|r| r.children.iter())
            .find(|i| i.province == p && i.epoch == epoch)
            .cloned()
    };
    if !direct && child.is_none() {
        return;
    }
    if pays {
        if let Some(pr) = room.province.as_mut() {
            pr.requests.push(Request::ProvinceShards {
                room: rid,
                province: p,
                epoch,
                ready,
            });
        }
        return;
    }
    let area = child.unwrap_or(ProvinceInfo {
        province: p,
        size,
        epoch,
        node: [0; 32],
        addr: String::new(),
        key: [0; 32],
    });
    let ready: HashSet<([i32; 2], u16)> = ready.into_iter().collect();
    for (region, list) in room.shards.iter_mut() {
        if !area.has(*region) {
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
    // (v143) Chez un pays, les baux de ses provinces sont plus courts.
    let lease = if st.province_room.is_some() {
        CHILD_LEASE_MS
    } else {
        proto::PROVINCE_LEASE_MS
    };
    let limit = Duration::from_millis(lease + proto::PROVINCE_MARGIN_MS);
    // (v144) Un pays qui ne sert pas (pas encore, ou plus) ne reprend rien : il ne pourrait
    // ni servir lui-même ni donner de bail.
    let now = guards::now_ms(st);
    let due: Vec<(u64, [i32; 2])> = st
        .rooms
        .iter()
        .filter(|(_, r)| !r.provinces.is_empty())
        .filter(|(_, r)| r.province.as_ref().is_none_or(|p| p.serving(now)))
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
    let mut up_now = Vec::new();
    for w in &held {
        match room.bag_waiting.remove(w) {
            // (v143) Un pays : à rendre au relais du monde, ou à garder.
            Some(UP) => up_now.push(w.clone()),
            Some(HERE) | None => {}
            Some(next) => {
                room.bag_holders.insert(w.clone(), next);
                lend_now.push(w.clone());
            }
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
    for w in up_now {
        give_back(st, rid, &w);
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
    // (v143) Un pays : demandé au relais du monde avec le prochain bail.
    if st.province_room == Some(rid) {
        let room = st.rooms.get_mut(&rid).expect("pays");
        for p in wanted {
            if let Some(r) = room.provinces.get_mut(&p) {
                r.standby = Some(Standby {
                    host: 0,
                    ready: false,
                    since: Instant::now(),
                    expect: None,
                });
            }
        }
        return;
    }
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
    // (v143) Le relais de secours d'un pays en sera un aussi s'il prend sa place.
    let nest = if r.nest { st.nest } else { None };
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
            expect: None,
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
            parent: None,
            nest,
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
    let nest = r.nest;
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
        nest,
        children: Vec::new(),
        expect: None,
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
    let mut tell = false;
    if let Some(pr) = room.province.as_mut() {
        let had = pr.followers.len();
        pr.followers.retain(|f| *f != id);
        if pr.followers.is_empty() {
            pr.copies.clear();
            // (v144) Plus de relais de secours : ses provinces l'apprennent.
            if had > 0 && pr.follower_at.take().is_some() {
                pr.children_said = None;
                tell = true;
            }
        }
    }
    if tell {
        tell_parent(st, rid, None);
    }
}

/// (v144) Relais d'un pays : à ses provinces (hôtes et relais de secours), où est son
/// relais de secours ; `only` : à celle-là seulement.
fn tell_parent(st: &State, rid: u64, only: Option<u64>) {
    let Some(room) = st.rooms.get(&rid) else {
        return;
    };
    let Some(pr) = room.province.as_ref().filter(|p| p.nest.is_some()) else {
        return;
    };
    for (p, r) in &room.provinces {
        let msg = Response::ProvinceParent {
            room: rid,
            province: *p,
            epoch: r.epoch,
            standby: pr.follower_at.clone(),
        };
        let hosts = std::iter::once(r.host).chain(r.standby.as_ref().map(|s| s.host));
        for h in hosts.filter(|h| *h != 0 && only.is_none_or(|o| o == *h)) {
            if let Some(c) = st.conns.get(&h) {
                transport::push(&c.tx, &msg);
            }
        }
    }
}

/// (v144) Relais d'un pays : ce que son relais de secours doit savoir de ses provinces
/// servies pour les reprendre avec leurs hôtes.
fn snapshot(st: &State, rid: u64) -> Vec<proto::ChildCopy> {
    let Some(room) = st.rooms.get(&rid) else {
        return Vec::new();
    };
    let mut bags: HashMap<([i32; 2], u64), Vec<String>> = HashMap::new();
    for (who, h) in &room.bag_holders {
        bags.entry(*h).or_default().push(who.clone());
    }
    let pseudo = |id: u64| st.conns.get(&id).and_then(|c| c.pseudo.clone());
    let mut v: Vec<proto::ChildCopy> = room
        .provinces
        .iter()
        .filter(|(_, r)| r.info.is_some())
        .map(|(p, r)| proto::ChildCopy {
            province: *p,
            epoch: r.epoch,
            info: r.info.clone(),
            host: pseudo(r.host)
                .or_else(|| r.expect.clone())
                .unwrap_or_default(),
            standby: r
                .standby
                .as_ref()
                .and_then(|s| pseudo(s.host).or_else(|| s.expect.clone())),
            big: r.big,
            bags: {
                let mut b = bags.remove(&(*p, r.epoch)).unwrap_or_default();
                b.sort();
                b
            },
            dump_n: r.dump_n,
        })
        .collect();
    v.sort_by_key(|c| c.province);
    v
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
    // (Un pays n'en donne pas : il ne certifie rien, le billet vient du relais du monde.)
    let ticket = (!list.is_empty() && room.province.is_none())
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
    nest: Option<proto::Nest>,
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
    // (v143) Un pays numérote les époques de ses provinces à partir de la sienne : jamais
    // deux fois la même, même après son relais de secours.
    room.next_epoch = (epoch << 20) + 1;
    if let Some(n) = nest {
        st.province_size
            .store(n.size.max(1) as usize, std::sync::atomic::Ordering::Relaxed);
        st.province_from
            .store(n.from.max(1) as usize, std::sync::atomic::Ordering::Relaxed);
    }
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
        nest,
        child_acks: VecDeque::new(),
        confirmed: 0,
        stopping: false,
        children_sent: None,
        follower_at: None,
        children_said: None,
        children_copy: Vec::new(),
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
    // (v143) Pays qui s'arrête : quand toutes ses provinces lui ont tout rendu (ou que leur
    // bail est fini), il rend tout lui-même.
    if st.rooms.get(&rid).is_some_and(|r| {
        r.provinces.is_empty()
            && r.province
                .as_ref()
                .is_some_and(|p| p.stopping && !p.stopped)
    }) {
        return finish_stop(st, rid, true);
    }
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
    // (v144) Un pays ne renvoie tout qu'une fois ses provinces revenues à lui (copie chez lui,
    // à jour) : ce qu'il renvoie est alors vraiment tout.
    let kids_fresh = room
        .provinces
        .values()
        .all(|r| r.info.is_none() || (!r.big && !r.stale));
    // (v142) Le relais du monde n'a plus une copie à jour : tout ce qu'elle a lui repart.
    let dump = room
        .province
        .as_ref()
        .filter(|p| p.to_root && p.dump_asked > p.dump_done && kids_fresh)
        .map(|p| p.dump_asked);
    let all = dump.map(|_| room.build.entries_after(|_| true, 0));
    // (v144) Joueurs connectés ici (grand monde : seules les positions de retour de ceux qui
    // sont partis remontent).
    let here: HashSet<String> = room.by_pseudo.keys().cloned().collect();
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
    // (v143) Envois de ses provinces qui attendent de partir plus haut pour être confirmés.
    let kids_wait = pr.child_acks.back().is_some_and(|a| a.0 == pr.next_n);
    if (!pr.outbox.is_empty() || !pr.loose.is_empty() || kids_wait) && due
        || pr.outbox.len() >= SEED_CHUNK
        || !bags.is_empty()
        || spots_due
        || all.is_some()
    {
        pr.last_archive = now;
        let big = !pr.to_root;
        // (v144) Grand monde : seules remontent les positions de retour de ceux qui sont
        // partis ; toutes partent à son relais de secours (qui la reprendrait).
        let (spots, spots_copy): (Vec<Spot>, Vec<Spot>) = if !spots_due {
            (Vec::new(), Vec::new())
        } else if big {
            pr.last_spots = now;
            let gone: Vec<String> = pr
                .spots
                .keys()
                .filter(|w| !here.contains(*w))
                .cloned()
                .collect();
            let up = gone
                .into_iter()
                .filter_map(|w| pr.spots.remove(&w).map(|a| (w, a)))
                .collect();
            (up, pr.spots.iter().map(|(w, a)| (w.clone(), *a)).collect())
        } else {
            pr.last_spots = now;
            (
                std::mem::take(&mut pr.spots).into_iter().collect(),
                Vec::new(),
            )
        };
        let n = pr.next_n;
        pr.next_n += 1;
        let mut entries = std::mem::take(&mut pr.outbox);
        pr.unconfirmed.push_back((n, big));
        let mut dumped = 0;
        if big {
            // (v142) Gardé par son relais de secours (tout de suite) ; au relais du monde,
            // seulement les sacs, les retours et les blocs comptés.
            copy_now = Some((n, entries.clone(), bags.clone(), spots_copy));
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
    if let Some((n, entries, bags, spots)) = copy_now {
        send_copy(st, rid, n, entries, bags, spots);
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
        // (v143) Un pays : les hôtes qu'il attend, et ses provinces servies si elles ont
        // changé (redites toutes les 10 s).
        let (wants, children) = if pays_here(st, rid) {
            let list: Vec<ProvinceInfo> = {
                let room = st.rooms.get(&rid).expect("pays");
                let mut v: Vec<ProvinceInfo> = room
                    .provinces
                    .values()
                    .filter_map(|r| r.info.clone())
                    .collect();
                v.sort_by_key(|i| i.province);
                v
            };
            let pr = st
                .rooms
                .get_mut(&rid)
                .and_then(|r| r.province.as_mut())
                .expect("pays");
            let again = now % 10_000 < proto::PROVINCE_RENEW_MS;
            let children = (pr.children_sent.as_ref() != Some(&list) || again).then(|| {
                pr.children_sent = Some(list.clone());
                list
            });
            // (v144) Son relais de secours apprend ses provinces quand elles changent.
            let snap = snapshot(st, rid);
            let pr = st
                .rooms
                .get_mut(&rid)
                .and_then(|r| r.province.as_mut())
                .expect("pays");
            if !pr.followers.is_empty() && pr.children_said.as_ref() != Some(&snap) {
                pr.children_said = Some(snap.clone());
                let msg = Response::ProvinceChildren {
                    room: rid,
                    province: p,
                    epoch,
                    children: snap,
                };
                for f in pr.followers.clone() {
                    if let Some(c) = st.conns.get(&f) {
                        transport::push(&c.tx, &msg);
                    }
                }
            }
            (wants(st, rid), children)
        } else {
            (Vec::new(), None)
        };
        out.push(Request::ProvinceLease {
            room: rid,
            province: p,
            epoch,
            t: now,
            counts,
            inside,
            followed,
            wants,
            children,
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
    // (v143) Un pays : aussi ce que disent ses provinces.
    inside.extend(
        room.provinces
            .values()
            .flat_map(|c| c.inside.iter().copied()),
    );
    inside.sort_unstable();
    inside.dedup();
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
            spots: Vec::new(),
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
    // (v143) Un pays : les envois de ses provinces portés par ce qui vient d'être gardé
    // leur sont confirmés.
    pr.confirmed = pr.confirmed.max(n);
    let mut kids = Vec::new();
    while pr.child_acks.front().is_some_and(|a| a.0 <= n) {
        kids.push(pr.child_acks.pop_front().expect("envoi"));
    }
    for (_, p, epoch, m) in kids {
        let host = room
            .provinces
            .get(&p)
            .filter(|r| r.epoch == epoch)
            .map(|r| r.host);
        if let Some(c) = host.and_then(|h| st.conns.get(&h)) {
            transport::push(
                &c.tx,
                &Response::ProvinceArchived {
                    room: rid,
                    province: p,
                    epoch,
                    n: m,
                },
            );
        }
    }
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
    spots: Vec<(String, [f32; 5])>,
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
        spots,
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
    at: ([u8; 32], String, [u8; 32]),
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
    // (v144) Où le joindre : les provinces d'un pays s'y rattacheront s'il prend sa place.
    pr.follower_at = Some(ProvinceInfo {
        province: p,
        size: pr.size,
        epoch,
        node: at.0,
        addr: at.1,
        key: at.2,
    });
    pr.children_said = None;
    let spots: Vec<(String, [f32; 5])> = pr.spots.iter().map(|(w, a)| (w.clone(), *a)).collect();
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
                spots,
            },
        );
    }
    // (v144) Un pays : ses provinces apprennent où est son relais de secours.
    tell_parent(st, room, None);
    log::info!("relais de province : relais de secours de {pseudo} entré");
    Response::Welcome {
        pseudo,
        totp: false,
    }
}

/// Relais de secours (v141) : un envoi gardé par le relais du monde, copié ici.
#[allow(clippy::too_many_arguments)]
pub(crate) fn copy(
    st: &mut State,
    epoch: u64,
    n: u64,
    entries: Vec<super::zones::Entry>,
    bags: Vec<proto::BagCopy>,
    held: Vec<String>,
    spots: Vec<(String, [f32; 5])>,
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
    // (v144) Les positions de retour (grand monde : elles ne remontent plus).
    if !spots.is_empty() {
        pr.spots = spots.into_iter().collect();
    }
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

/// (v144) Relais de secours d'un pays : ce que le pays sait de ses provinces (pour les
/// reprendre avec leurs hôtes s'il prend sa place).
pub(crate) fn children_here(st: &mut State, epoch: u64, list: Vec<proto::ChildCopy>) {
    let Some(rid) = st.province_room else {
        return;
    };
    if let Some(pr) = st
        .rooms
        .get_mut(&rid)
        .and_then(|r| r.province.as_mut())
        .filter(|p| p.standby && p.epoch == epoch)
    {
        pr.children_copy = list.into_iter().take(65_536).collect();
    }
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
    pr.child_acks.clear();
    pr.children_sent = None;
    pr.lease_until = 0;
    pr.last_ask = 0;
    pr.serving = false;
    pr.ready_sent.clear();
    pr.next_n = 1;
    pr.asked.clear();
    // (Les sacs qu'elle tenait lui sont prêtés de nouveau par le relais du monde, qui en a
    // la copie la plus récente.)
    pr.held.clear();
    let have = pr.have;
    log::info!(
        "relais de secours : il prend la place du relais de la province {:?} (époque {epoch})",
        pr.province
    );
    // (v144) Un pays : ses provinces servies se rattacheront ici avec leurs hôtes (elles
    // continuent ; ce qu'elles ont publié depuis la copie lui sera renvoyé).
    let kids = std::mem::take(&mut pr.children_copy);
    pr.follower_at = None;
    pr.children_said = None;
    room.next_epoch = (epoch << 20) + 1;
    let now = Instant::now();
    for c in kids {
        let Some(info) = c.info else {
            continue;
        };
        room.provinces.insert(
            c.province,
            ProvinceRef {
                host: 0,
                epoch: c.epoch,
                info: Some(info),
                since: now,
                renewed: now + REATTACH_GRACE,
                snap_seq: 0,
                empty_since: None,
                counts: None,
                inside: Vec::new(),
                standby: c.standby.map(|who| Standby {
                    host: 0,
                    ready: false,
                    since: now,
                    expect: Some(who),
                }),
                recent: VecDeque::new(),
                recent_bytes: 0,
                carry: None,
                winding: false,
                big: false,
                followed: false,
                stale: true,
                dump_n: c.dump_n + 1,
                stop_after_dump: false,
                unfollowed_since: None,
                nest: false,
                children: Vec::new(),
                expect: Some(c.host),
            },
        );
        for who in c.bags {
            room.bag_holders.insert(who, (c.province, c.epoch));
        }
    }
    refresh_delegated(room);
    Some(have)
}

/// Relais de province : réponse du relais du monde à une demande de bail faite à `t`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn leased(
    st: &mut State,
    epoch: u64,
    t: u64,
    ok: bool,
    copies: bool,
    dump: u64,
    ms: u32,
) {
    let Some(rid) = st.province_room else {
        return;
    };
    let Some(pr) = st.rooms.get_mut(&rid).and_then(|r| r.province.as_mut()) else {
        return;
    };
    if ok && epoch == pr.epoch && !pr.stopped {
        // (v143) Une province d'un pays a un bail plus court.
        let ms = if ms == 0 {
            proto::PROVINCE_LEASE_MS
        } else {
            (ms as u64).min(proto::PROVINCE_LEASE_MS)
        };
        pr.lease_until = pr.lease_until.max(t + ms);
        // (v142) Qui garde la copie de ce qu'elle publie : le relais du monde, ou son relais
        // de secours ; quand le relais du monde la garde de nouveau, tout lui repart.
        pr.dump_asked = pr.dump_asked.max(dump);
        if !copies && pr.to_root {
            // Ce qui attendait que le relais du monde le garde part tout de suite au relais
            // de secours.
            let flush: Vec<_> = pr.copies.drain(..).collect();
            pr.to_root = false;
            for (n, entries, bags) in flush {
                send_copy(st, rid, n, entries, bags, Vec::new());
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
    // (v143) Un pays qui sert encore des provinces : elles s'arrêtent d'abord et lui rendent
    // tout (leur bail finit de toute façon avant le sien) ; il s'arrêtera ensuite (`tick`).
    let kids: Vec<[i32; 2]> = st
        .rooms
        .get(&rid)
        .filter(|r| {
            r.province
                .as_ref()
                .is_some_and(|p| !p.stopped && !p.standby && p.nest.is_some())
        })
        .map(|r| r.provinces.keys().copied().collect())
        .unwrap_or_default();
    if release && !kids.is_empty() {
        if let Some(pr) = st.rooms.get_mut(&rid).and_then(|r| r.province.as_mut()) {
            if !pr.stopping {
                log::info!("pays : arrêt demandé, ses provinces d'abord");
            }
            pr.stopping = true;
        }
        for p in kids {
            stop(st, rid, p, "le pays s'arrête", false);
        }
        return Vec::new();
    }
    finish_stop(st, rid, release)
}

/// Relais de province : arrêt pour de bon (voir `stop_here`).
fn finish_stop(st: &mut State, rid: u64, release: bool) -> Vec<Request> {
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
        log::debug!("province : demande de {pseudo} en {chunk:?} sans bail, ignorée");
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
    // (v143) Région confiée à une province du pays : c'est elle qui en décide (une demande
    // d'objet est refusée, le jeu la refera auprès d'elle ; un bloc lui sera renvoyé).
    if delegated(st, rid, region) {
        log::debug!("pays : demande de {pseudo} en {chunk:?} pour une de ses provinces, ignorée");
        if let Some(seq) = msg
            .as_ref()
            .filter(|m| minelib_net::items::is_request(m))
            .and_then(minelib_net::items::request_seq)
        {
            super::publish_items(st, rid, id, vec![BuildMsg::Denied { seq }]);
        }
        return;
    }
    let Some(room) = st.rooms.get(&rid) else {
        return;
    };
    let Some(pr) = room.province.as_ref().filter(|p| p.has(region)) else {
        log::debug!("province : demande de {pseudo} en {chunk:?} hors de la province, ignorée");
        return;
    };
    let sovereign = room.sov.get(&region).map(|sv| sv.primary);
    // Objets et coffres de la province (v139) : ce relais arbitre ; un joueur en survie
    // attend son sac (prêté par le relais du monde).
    if let Some(m) = msg.as_ref().filter(|m| minelib_net::items::is_request(m)) {
        let mine = room.build.items.target_chunk(m).is_some_and(|c| {
            pr.has(proto::region_of(c)) && !delegated(st, rid, proto::region_of(c))
        });
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
        // (v143) Un pays qui l'a prêté à une de ses provinces : il revient ici si le joueur
        // est dans une région que le pays sert lui-même.
        let Some(&(q, e)) = room.bag_holders.get(&who) else {
            return true;
        };
        let own = room.build.positions.get(&id).is_some_and(|c| {
            pr.has(proto::region_of(*c)) && !delegated(st, rid, proto::region_of(*c))
        });
        if own && !room.bag_waiting.contains_key(&who) {
            if let Some(room) = st.rooms.get_mut(&rid) {
                room.bag_waiting.insert(who.clone(), HERE);
            }
            recall(st, rid, &who, q, e);
        }
        return false;
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
    if !pr.held.insert(who.clone()) {
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
    // (v143) Un pays : une de ses provinces l'attendait.
    let Some(room) = st.rooms.get_mut(&rid) else {
        return;
    };
    match room.bag_waiting.get(&who).copied() {
        Some(next) if next != UP && next != HERE => {
            room.bag_waiting.remove(&who);
            if room
                .provinces
                .get(&next.0)
                .is_some_and(|r| r.epoch == next.1 && r.info.is_some())
            {
                room.bag_holders.insert(who.clone(), next);
                lend(st, rid, &who);
            }
        }
        _ => {}
    }
}

/// Relais de province : un sac à rendre (redemandé par le relais du monde, ou son joueur
/// n'est plus dans la province). Il n'en est plus fait usage ici.
pub(crate) fn bag_back(st: &mut State, pseudo: &str) {
    let Some(rid) = st.province_room else {
        return;
    };
    let who = pseudo.to_ascii_lowercase();
    // (v143) Un pays qui l'a prêté à une de ses provinces : elle le lui rend d'abord.
    let lent = st.rooms.get_mut(&rid).and_then(|room| {
        let h = room.bag_holders.get(&who).copied()?;
        room.bag_waiting.insert(who.clone(), UP);
        Some(h)
    });
    if let Some((q, e)) = lent {
        recall(st, rid, &who, q, e);
        return;
    }
    give_back(st, rid, &who);
}

/// (v143) Relais de province (ou pays) : demande au relais du monde le sac de `who` (au
/// plus toutes les `BAG_ASK_MS`).
fn ask_bag(st: &mut State, rid: u64, who: &str) {
    let now = guards::now_ms(st);
    let Some(pr) = st.rooms.get_mut(&rid).and_then(|r| r.province.as_mut()) else {
        return;
    };
    let fresh = pr
        .asked
        .get(who)
        .is_none_or(|t| now.saturating_sub(*t) >= BAG_ASK_MS);
    if !pr.stopped && fresh {
        pr.asked.insert(who.to_string(), now);
        let (p, epoch) = (pr.province, pr.epoch);
        pr.requests.push(Request::ProvinceBag {
            room: rid,
            province: p,
            epoch,
            pseudo: who.to_string(),
        });
    }
}

/// Relais de province : le sac de `who` part au relais du monde (état final).
fn give_back(st: &mut State, rid: u64, who: &str) {
    let Some(room) = st.rooms.get(&rid) else {
        return;
    };
    let server = room.server.clone();
    let who = who.to_ascii_lowercase();
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
    let mut kids: HashMap<[i32; 2], Vec<minelib_net::sovereign::Edit>> = HashMap::new();
    let child_size = size(st);
    for e in edits {
        let region = proto::region_of([e.pos[0].div_euclid(16), e.pos[2].div_euclid(16)]);
        if !pr.has(region) || !e.by_root || !(0..256).contains(&e.pos[1]) {
            continue;
        }
        // (v143) Région confiée à une province du pays : à elle, telle quelle (signée par le
        // relais du monde).
        if room
            .delegated
            .contains(&proto::province_of(region, child_size))
        {
            kids.entry(proto::province_of(region, child_size))
                .or_default()
                .push(e);
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
    for (p, edits) in kids {
        let Some((host, epoch)) = st
            .rooms
            .get(&rid)
            .and_then(|r| r.provinces.get(&p))
            .map(|r| (r.host, r.epoch))
        else {
            continue;
        };
        if let Some(c) = st.conns.get(&host) {
            transport::push(
                &c.tx,
                &Response::ProvinceBulk {
                    room: rid,
                    province: p,
                    epoch,
                    pseudo: pseudo.to_string(),
                    edits,
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
    // (v143) Un pays : à ses provinces aussi.
    push_settings(st, &server);
}

/// (v143) Relais de pays : l'hôte d'une de ses provinces (ou de son relais de secours) entre,
/// avec le jeton que le relais du monde lui a donné avec sa tâche. L'hôte reçoit
/// l'historique de sa province ; la suite comme chez le relais du monde (`ready`, baux,
/// envois gardés ici).
#[allow(clippy::too_many_arguments)]
pub(crate) fn child_hello(
    st: &mut State,
    session: &mut Session,
    id: u64,
    room: u64,
    p: [i32; 2],
    size: i32,
    epoch: u64,
    pseudo: String,
    signer: [u8; 32],
    token: [u8; 32],
    standby: bool,
) -> Response {
    let ok = st.province_room == Some(room)
        && size == self::size(st)
        && !pseudo.starts_with('#')
        && st.rooms.get(&room).is_some_and(|r| {
            r.province
                .as_ref()
                .is_some_and(|pr| pr.nest.is_some() && !pr.stopped && !pr.standby)
                && proto::child_token(
                    &r.shard_secret,
                    room,
                    p,
                    size,
                    &pseudo,
                    &signer,
                    epoch,
                    standby,
                ) == token
                && r.provinces.get(&p).is_some_and(|c| {
                    // (v144) Ou qui se rattache, après que le relais de secours du pays a
                    // pris sa place : celui-là seulement.
                    let mine = |e: &Option<String>| {
                        e.as_deref()
                            .is_some_and(|e| e.eq_ignore_ascii_case(&pseudo))
                    };
                    c.epoch == epoch
                        && if standby {
                            c.info.is_some()
                                && c.standby.as_ref().is_some_and(|s| {
                                    s.host == 0 && (s.expect.is_none() || mine(&s.expect))
                                })
                        } else {
                            c.host == 0 && (c.info.is_none() || mine(&c.expect))
                        }
                })
        });
    if !ok {
        return err(
            ErrorCode::WrongCredentials,
            "jeton de province de pays refusé",
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
    let r = st.rooms.get_mut(&room).expect("pays");
    r.members.push(id);
    r.hidden.insert(id);
    if standby {
        if let Some(sb) = r.provinces.get_mut(&p).and_then(|c| c.standby.as_mut()) {
            sb.host = id;
            sb.since = Instant::now();
            sb.ready = false;
            sb.expect = None;
        }
        log::info!("pays : relais de secours de la province {p:?} entré ({pseudo})");
    } else if r.provinces.get(&p).is_some_and(|c| c.expect.is_some()) {
        // (v144) Elle se rattache avec tout ce qu'elle a : son bail reprend, et ce qu'elle a
        // publié depuis la copie du pays lui sera renvoyé (elle est « en retard » ici).
        if let Some(c) = r.provinces.get_mut(&p) {
            c.host = id;
            c.expect = None;
            c.renewed = Instant::now();
        }
        log::info!("pays : province {p:?} rattachée ({pseudo}, époque {epoch})");
    } else {
        // L'historique de la province ; la suite quand il sera prêt.
        let snap_seq = r.build.last_seq();
        let keep = |zone: [i32; 2]| proto::province_of(zone_region(zone), size) == p;
        let entries = r.build.entries_after(keep, 0);
        if let Some(c) = r.provinces.get_mut(&p) {
            c.host = id;
            c.since = Instant::now();
            c.snap_seq = snap_seq;
        }
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
        }
        log::info!("pays : hôte de la province {p:?} entré ({pseudo}, époque {epoch})");
    }
    // (v144) Où est le relais de secours du pays.
    tell_parent(st, room, Some(id));
    Response::Welcome {
        pseudo,
        totp: false,
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
