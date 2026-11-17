//! Relais de région : une région d'un monde de construction très peuplé est servie par son
//! propre relais, que fait tourner le jeu d'un joueur sans lien avec ce monde (sa connexion
//! de garant, en arrière-plan, comme les garants des parties FPS). Personne dans le monde
//! n'a intérêt à l'attaquer, et le relais du monde n'a plus à tout transmettre.
//!
//! Il fait trois choses, pour sa région seulement :
//! - les joueurs y entrent avec le billet du relais du monde (`proto::shard_mac`) ;
//! - il transmet les positions aux joueurs proches (moins souvent au loin) ;
//! - il transmet les modifications de blocs et en garde l'historique compacté pour ceux
//!   qui arrivent.
//!
//! D'où viennent les modifications dépend de la région :
//! - **Région ordinaire** (un seul relais) : du relais du monde, qui les accepte et les met
//!   en ordre (reçues par la connexion de l'hôte, `ShardHandle::seed`). Ce relais ne décide
//!   rien.
//! - **Région souveraine** (au moins deux relais, depuis la v130 ; voir
//!   `minelib_net::sovereign`) : l'hôte principal (rang donné par le relais du monde)
//!   accepte et met en ordre les modifications de la région, que chaque joueur signe avec la
//!   clé de son compte. Il les publie par lots signés, enchaînés par une empreinte. Les
//!   autres relais de la région vérifient chaque lot avant de le transmettre à leurs
//!   joueurs, et le confirment au relais du monde, qui ne l'archive qu'alors. Une faute de
//!   l'hôte principal se prouve avec ses propres lots (`Request::Accuse`) : il est écarté
//!   et un autre prend sa place. Il ne peut ni inventer une modification, ni l'attribuer à
//!   un autre, ni réécrire ce qu'il a publié ; il peut seulement tarder ou refuser (le
//!   relais du monde le remplace alors), et montrer à ses propres joueurs, pendant ce
//!   temps, des modifications que personne d'autre ne verra.
//!
//! S'il disparaît, les joueurs repassent par le relais du monde ou par un autre hôte, qui
//! reçoit tout l'historique de la région : chacun reçoit ce qui lui manque.
//!
//! Une région très peuplée a plusieurs relais, chacun chez un hôte différent. Chaque
//! joueur n'entre que dans l'un d'eux (`proto::shard_for`), et les relais se relient entre
//! eux (`set_peers`) : la position d'un joueur part à ses voisins du même relais, et une
//! fois à chaque autre relais, qui la transmet à ses propres joueurs proches.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use minelib_net::client::{Client, Cmd, Event, Resolver};
use minelib_net::game::{self, BuildMsg};
use minelib_net::proto::{self, ErrorCode, Request, Response};
use minelib_net::sovereign::{self, Batch, Cert, Checker, Edit};
use minelib_net::transport::{self, Channel, Inbound, Incoming};

use crate::zones;

struct Conn {
    tx: Sender<Inbound>,
    pseudo: Option<String>,
    /// Un autre relais de la région (pas un joueur), et son rang.
    peer: bool,
    slot: Option<u16>,
}

/// Lots gardés pour un relais qui arrive et pour la passation en fin d'époque (s).
const KEEP_BATCHES: Duration = Duration::from_secs(120);
/// L'hôte principal scelle un lot au plus tard ce temps après la première modification
/// qu'il contient, ou dès qu'il en a ce nombre.
const BATCH_WAIT: Duration = Duration::from_millis(40);
const BATCH_MAX: usize = 256;
/// Lots arrivés en avance (un lot manquant avant eux) gardés au plus.
const EARLY_MAX: usize = 256;

/// D'où vient une modification qu'accepte l'hôte principal (à qui dire un refus).
#[derive(Clone, Copy)]
enum From {
    /// Un joueur de ce relais.
    Local(u64),
    /// Un joueur d'un autre relais de la région (rang de ce relais).
    Peer(u16),
    /// Le relais du monde.
    Root,
}

/// Région souveraine : ce que ce relais sait de l'époque en cours.
struct Sovereign {
    sov: proto::Sov,
    /// Ce relais est l'hôte principal.
    primary: bool,
    /// Clé de l'hôte principal.
    primary_key: Option<ed25519_dalek::VerifyingKey>,
    /// Vérifie ce que publie l'hôte principal (chez lui : admet et scelle).
    checker: Checker,
    /// Hôte principal : modifications admises, pas encore scellées en lot, et depuis quand.
    pending: Vec<Edit>,
    pending_since: Option<Instant>,
    /// Lots de l'époque (publiés ou vérifiés), avec leur heure.
    recent: VecDeque<(Instant, Batch)>,
    /// Lots arrivés en avance, par premier rang.
    early: BTreeMap<u64, Batch>,
    /// Une faute prouvée a été vue chez l'hôte principal : plus rien n'est suivi de lui.
    faulty: bool,
    /// Ce relais confirme au relais du monde ce qu'il vérifie (deux relais suffisent).
    acks: bool,
    /// Dernière fois que des lots manquants ont été redemandés à l'hôte principal.
    asked: Option<Instant>,
}

/// Lots arrivés avant que ce relais connaisse leur époque, gardés au plus.
const ORPHANS_MAX: usize = 256;
/// Des lots manquent : redemandés au plus une fois par ce temps.
const RESEND_EVERY: Duration = Duration::from_millis(500);
/// Voix (v137) : destinataires au plus par message, taille au plus, messages au plus par
/// seconde et par joueur (50 paquets de voix, plus les clés et les bonjours).
const VOICE_TO: usize = 256;
const VOICE_BYTES: usize = 4096;
const VOICE_RATE: u32 = 120;

/// Comment joindre un autre relais de la région (directement ou par un tunnel).
pub type Dial = Arc<dyn Fn(&proto::ShardInfo) -> Resolver + Send + Sync>;

/// Lien vers un autre relais de la région.
struct PeerOut {
    info: proto::ShardInfo,
    client: Client,
    link: Option<Sender<Inbound>>,
    /// Lien cassé : retenté après un instant.
    broken: Option<Instant>,
}

struct State {
    room: u64,
    region: [i32; 2],
    secret: [u8; 32],
    conns: HashMap<u64, Conn>,
    next_id: u64,
    build: zones::Build,
    closed: bool,
    /// Rang de ce relais dans sa région, et liens vers les autres (par rang).
    slot: u16,
    peers: HashMap<u16, PeerOut>,
    dial: Option<Dial>,
    /// Clé du relais du monde, et clé du compte de l'hôte (il signe ce qu'il publie).
    root: [u8; 32],
    signer: Option<ed25519_dalek::SigningKey>,
    /// Certificats des joueurs entrés ici (pseudo en minuscules).
    certs: HashMap<String, Cert>,
    sovereign: Option<Sovereign>,
    /// Lots de l'hôte principal arrivés avant que ce relais connaisse leur époque (le
    /// lien entre relais peut devancer l'annonce du relais du monde).
    orphans: Vec<Batch>,
    /// Rang de la dernière modification transmise pour chaque case : la même, arrivée
    /// par un autre chemin (relais du monde, hôte principal), n'est transmise qu'une fois.
    cells: HashMap<[i32; 3], u64>,
    /// Messages pour le relais du monde (par la connexion de l'hôte, voir `take_outbox`).
    outbox: Vec<Request>,
    /// Hôte principal : lots publiés pas encore partis au relais du monde, et depuis quand
    /// (ils partent ensemble, au plus toutes les `proto::ARCHIVE_EVERY_MS`). Autre relais :
    /// dernière confirmation à donner (époque, rang, empreinte), et depuis quand.
    archive: Vec<Batch>,
    archive_since: Option<Instant>,
    ack: Option<(u64, u64, [u8; 32])>,
    ack_since: Option<Instant>,
    /// Essais : le prochain lot scellé comme hôte principal contiendra une modification
    /// inventée (voir `ShardHandle::forge_once`).
    forge: bool,
    /// Voix envoyée par chaque joueur dans la seconde en cours (début, nombre).
    voice_rate: HashMap<u64, (Instant, u32)>,
    /// Joueurs entrés (pseudo en minuscules → connexions) : à qui remettre la voix.
    players: HashMap<String, Vec<u64>>,
    /// Messages de voix remis par ce relais (essais).
    voice_sent: usize,
}

/// Destinataires d'un message de voix : au plus `VOICE_TO`, en minuscules, sans doublon.
fn voice_names(to: Vec<String>) -> Vec<String> {
    let mut to: Vec<String> = to
        .into_iter()
        .take(VOICE_TO)
        .map(|p| p.to_ascii_lowercase())
        .collect();
    to.sort_unstable();
    to.dedup();
    to
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

impl State {
    /// Une modification publiée par le relais du monde (dans son ordre) : gardée pour ceux
    /// qui arriveront, et transmise aux joueurs de ce relais proches de là.
    fn deliver(&mut self, set: &[u8]) {
        if let Some(BuildMsg::Set {
            pos,
            block,
            by,
            seq,
        }) = game::decode::<BuildMsg>(set)
        {
            self.deliver_set(pos, block, &by, seq);
        }
    }

    /// Transmet une modification (si elle est nouvelle pour cette case) aux joueurs de ce
    /// relais proches de là, et à son auteur s'il est ici (c'est sa confirmation).
    fn deliver_set(&mut self, pos: [i32; 3], block: u16, by: &str, seq: u64) {
        // Seulement des blocs de cette région (objets et coffres ne passent que par le
        // relais du monde).
        let chunk = [pos[0].div_euclid(16), pos[2].div_euclid(16)];
        if proto::region_of(chunk) != self.region {
            return;
        }
        if self.cells.get(&pos).is_some_and(|s| *s >= seq) {
            return;
        }
        self.cells.insert(pos, seq);
        let set = game::encode(&BuildMsg::Set {
            pos,
            block,
            by: by.to_string(),
            seq,
        });
        let mut to = self.build.listeners(chunk, u64::MAX);
        if !by.is_empty() {
            let author = self
                .conns
                .iter()
                .find(|(_, c)| {
                    !c.peer
                        && c.pseudo
                            .as_deref()
                            .is_some_and(|p| p.eq_ignore_ascii_case(by))
                })
                .map(|(&id, _)| id);
            if let Some(a) = author.filter(|a| !to.contains(a)) {
                to.push(a);
            }
        }
        for (m, items) in self.build.mirror_to(proto::zone_of(chunk), &set, &to) {
            if let Some(c) = self.conns.get(&m) {
                for data in items {
                    crate::push_zone(&c.tx, chunk, data);
                }
            }
        }
    }

    /// Refus d'une modification : à son auteur (ici ou chez un autre relais).
    fn refuse(&self, from: From, pseudo: &str, pos: [i32; 3]) {
        let chunk = [pos[0].div_euclid(16), pos[2].div_euclid(16)];
        match from {
            From::Local(id) => {
                if let Some(c) = self.conns.get(&id) {
                    transport::push(
                        &c.tx,
                        &Response::FromZone {
                            peer: 0,
                            chunk,
                            data: game::encode(&BuildMsg::Reject { pos, peer: id }),
                        },
                    );
                }
            }
            From::Peer(slot) => {
                if let Some(l) = self.peers.get(&slot).and_then(|p| p.link.as_ref()) {
                    transport::push(
                        l,
                        &Request::PeerReject {
                            pseudo: pseudo.to_string(),
                            pos,
                        },
                    );
                }
            }
            From::Root => {}
        }
    }

    /// Hôte principal : une modification signée à accepter (ou à refuser).
    fn admit(&mut self, edit: Edit, cert: Option<Cert>, from: From) {
        let Some(sv) = self.sovereign.as_mut().filter(|s| s.primary && !s.faulty) else {
            self.refuse(from, &edit.pseudo, edit.pos);
            return;
        };
        let cert = cert.or_else(|| self.certs.get(&edit.pseudo.to_ascii_lowercase()).cloned());
        if let Err(why) = sv.checker.admit(&edit, cert.as_ref(), now_ms()) {
            log::debug!("relais de région : modification refusée : {why}");
            self.refuse(from, &edit.pseudo, edit.pos);
            return;
        }
        let seq = sv.checker.next + sv.pending.len() as u64;
        sv.pending.push(edit.clone());
        sv.pending_since.get_or_insert_with(Instant::now);
        self.deliver_set(edit.pos, edit.block, &edit.pseudo, seq);
    }

    /// Voix : ce joueur peut-il encore en envoyer dans cette seconde ?
    fn voice_due(&mut self, id: u64) -> bool {
        let r = self.voice_rate.entry(id).or_insert((Instant::now(), 0));
        if r.0.elapsed() >= Duration::from_secs(1) {
            *r = (Instant::now(), 0);
        }
        r.1 += 1;
        r.1 <= VOICE_RATE
    }

    /// Voix de `from` à ceux de ses destinataires (pseudos en minuscules, sans doublon)
    /// entrés dans ce relais.
    fn deliver_voice(&mut self, from: &str, to: &[String], data: &[u8]) {
        let Some(msg) = transport::encode(&Response::VoiceFrom {
            from: from.to_string(),
            data: data.to_vec(),
        }) else {
            return;
        };
        let me = from.to_ascii_lowercase();
        let mut sent = 0;
        for name in to.iter().filter(|n| **n != me) {
            for id in self.players.get(name).into_iter().flatten() {
                if let Some(c) = self.conns.get(id) {
                    transport::push_raw(&c.tx, msg.clone());
                    sent += 1;
                }
            }
        }
        self.voice_sent += sent;
    }

    /// Une connexion fermée : plus de voix pour elle.
    fn forget(&mut self, id: u64) {
        if let Some(name) = self
            .conns
            .remove(&id)
            .and_then(|c| c.pseudo)
            .map(|p| p.to_ascii_lowercase())
        {
            if let Some(ids) = self.players.get_mut(&name) {
                ids.retain(|i| *i != id);
                if ids.is_empty() {
                    self.players.remove(&name);
                }
            }
        }
        self.voice_rate.remove(&id);
    }

    /// Hôte principal : scelle et publie ce qui attend (aux autres relais de la région, et
    /// au relais du monde pour l'archive).
    fn flush(&mut self, force: bool) {
        let Some(signer) = self.signer.clone() else {
            return;
        };
        let Some(sv) = self
            .sovereign
            .as_mut()
            .filter(|s| s.primary && !s.pending.is_empty())
        else {
            return;
        };
        let due = sv.pending_since.is_some_and(|t| t.elapsed() >= BATCH_WAIT);
        if !(force || due || sv.pending.len() >= BATCH_MAX) {
            return;
        }
        let mut edits = std::mem::take(&mut sv.pending);
        sv.pending_since = None;
        // (Essais : un hôte malveillant ajoute une modification inventée au nom d'un
        // joueur, et la montre aux siens.)
        let mut forged = None;
        if std::mem::take(&mut self.forge) {
            let e = &edits[0];
            let liar = ed25519_dalek::SigningKey::from_bytes(&[0x66; 32]);
            let pos = [e.pos[0], e.pos[1] + 7, e.pos[2]];
            let fake = Edit::sign(&liar, self.room, &e.pseudo, pos, 1, false, e.at, e.t, false);
            forged = Some((pos, e.pseudo.clone(), sv.checker.next + edits.len() as u64));
            edits.push(fake);
        }
        let batch = sv.checker.seal(edits, &signer);
        if let Some((pos, by, seq)) = forged {
            self.deliver_set(pos, 1, &by, seq);
        }
        let Some(sv) = self.sovereign.as_mut() else {
            return;
        };
        sv.recent.push_back((Instant::now(), batch.clone()));
        if let Some(msg) = transport::encode(&Request::PeerBatch {
            batch: batch.clone(),
        }) {
            for l in self.peers.values().filter_map(|p| p.link.as_ref()) {
                transport::push_raw(l, msg.clone());
            }
        }
        self.archive.push(batch);
        self.archive_since.get_or_insert_with(Instant::now);
    }

    /// Lots à archiver et confirmation au relais du monde : regroupés (au plus un message
    /// de chaque toutes les `proto::ARCHIVE_EVERY_MS`), ou tout de suite (`now`, fin
    /// d'époque).
    fn send_root(&mut self, now: bool) {
        let every = Duration::from_millis(proto::ARCHIVE_EVERY_MS);
        let region = self.region;
        let due = |t: Option<Instant>| t.is_some_and(|t| now || t.elapsed() >= every);
        if due(self.archive_since) || self.archive.len() >= 64 {
            self.archive_since = None;
            let batches = std::mem::take(&mut self.archive);
            if !batches.is_empty() {
                self.outbox.push(Request::Archive { region, batches });
            }
        }
        if due(self.ack_since) {
            self.ack_since = None;
            if let Some((epoch, seq, chain)) = self.ack.take() {
                self.outbox.push(Request::Ack {
                    region,
                    epoch,
                    seq,
                    chain,
                });
            }
        }
    }

    /// Autre relais d'une région souveraine : un lot de l'hôte principal, à vérifier.
    fn follow(&mut self, batch: Batch) {
        let region = self.region;
        // (Époque pas encore annoncée ici : le lot attend.)
        let known = self
            .sovereign
            .as_ref()
            .is_some_and(|sv| batch.epoch <= sv.sov.epoch);
        if !known {
            if self.orphans.len() < ORPHANS_MAX {
                self.orphans.push(batch);
            }
            return;
        }
        let Some(sv) = self.sovereign.as_mut() else {
            return;
        };
        if sv.primary || sv.faulty || batch.epoch != sv.sov.epoch {
            return;
        }
        let Some(key) = sv.primary_key else { return };
        if batch.first > sv.checker.next {
            if sv.early.len() < EARLY_MAX {
                sv.early.insert(batch.first, batch);
            }
            // Il en manque avant : redemandés à l'hôte principal.
            if sv.asked.is_none_or(|t| t.elapsed() >= RESEND_EVERY) {
                sv.asked = Some(Instant::now());
                let from = sv.checker.next;
                let primary = sv.sov.primary;
                if let Some(l) = self.peers.get(&primary).and_then(|p| p.link.as_ref()) {
                    transport::push(l, &Request::PeerResend { from });
                }
            }
            return;
        }
        let mut queue = vec![batch];
        while let Some(b) = queue.pop() {
            let sv = self.sovereign.as_mut().expect("souveraine");
            match sv.checker.check(b.clone(), &key) {
                Ok(true) => {
                    let (epoch, seq, chain) = (sv.sov.epoch, sv.checker.next, sv.checker.chain);
                    if sv.acks {
                        self.ack = Some((epoch, seq, chain));
                        self.ack_since.get_or_insert_with(Instant::now);
                    }
                    let sv = self.sovereign.as_mut().expect("souveraine");
                    sv.recent.push_back((Instant::now(), b.clone()));
                    if let Some(next) = sv.early.remove(&sv.checker.next) {
                        queue.push(next);
                    }
                    for r in b.records {
                        self.deliver_set(r.edit.pos, r.edit.block, &r.edit.pseudo, r.seq);
                    }
                }
                Ok(false) => {}
                Err(fault) => {
                    log::warn!(
                        "relais de région {region:?} : faute de l'hôte principal : {}",
                        fault.why
                    );
                    sv.faulty = true;
                    let epoch = sv.sov.epoch;
                    self.outbox.push(Request::Accuse {
                        region,
                        epoch,
                        why: fault.why,
                        evidence: fault.evidence.iter().map(|b| (**b).clone()).collect(),
                    });
                    return;
                }
            }
        }
    }

    /// Fin d'une époque : les lots vérifiés partent au relais du monde (rien ne se perd).
    fn hand_over(&mut self) {
        if let Some(sv) = self.sovereign.take() {
            let batches: Vec<Batch> = sv.recent.into_iter().map(|(_, b)| b).collect();
            if !batches.is_empty() && !sv.faulty {
                self.outbox.push(Request::Handover {
                    region: self.region,
                    epoch: sv.sov.epoch,
                    batches,
                });
            }
        }
    }

    fn connect(&self, info: &proto::ShardInfo) -> Client {
        let resolve: Resolver = match &self.dial {
            Some(d) => d(info),
            None => {
                let (addr, key) = (info.addr.clone(), info.key);
                Box::new(move || Ok((addr.clone(), Some(key))))
            }
        };
        let client = Client::spawn_resolved(resolve);
        let pseudo = proto::shard_peer_name(self.slot);
        let expires = now_secs() + 24 * 3600;
        client.send(Cmd::ShardLogin {
            room: self.room,
            mac: proto::shard_mac(&self.secret, self.room, &pseudo, expires),
            pseudo,
            expires,
            cert: None,
        });
        client
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

pub struct ShardHandle {
    pub addr: SocketAddr,
    /// Clé publique du canal chiffré.
    pub public: [u8; 32],
    state: Arc<Mutex<State>>,
}

impl ShardHandle {
    /// Du relais du monde : l'historique de la région (avant l'arrivée des joueurs), puis
    /// chaque modification qu'il publie, dans son ordre.
    pub fn seed(&self, data: &[u8]) {
        let mut st = self.state.lock().unwrap();
        match game::decode::<BuildMsg>(data) {
            // Blocs regroupés : chacun entre dans l'historique comme une modification
            // (dans un paquet, ceux d'une même case sont rangés du plus ancien au plus
            // récent).
            Some(BuildMsg::Cells(packet)) => {
                let cells = minelib_net::cells::unpack_cells(&packet).unwrap_or_default();
                for (pos, block, seq) in cells {
                    st.deliver_set(pos, block, "", seq);
                }
            }
            _ => st.deliver(data),
        }
    }

    /// Du relais du monde, à l'hôte principal d'une région souveraine : modifications
    /// signées par lui à la place de leurs auteurs.
    pub fn root_edits(&self, edits: Vec<Edit>, certs: Vec<Cert>) {
        let mut st = self.state.lock().unwrap();
        // (Modifications signées par leurs auteurs, transmises par un relais de province :
        // leurs certificats viennent avec, vérifiés avec la clé du relais du monde.)
        let root = ed25519_dalek::VerifyingKey::from_bytes(&st.root).ok();
        for c in certs {
            if root.is_some_and(|k| c.valid(&k, st.room)) {
                if let Some(sv) = st.sovereign.as_mut() {
                    sv.checker.learn(&c);
                }
                st.certs.insert(c.pseudo.to_ascii_lowercase(), c);
            }
        }
        for e in edits {
            st.admit(e, None, From::Root);
        }
    }

    /// Le relais s'arrête (son hôte quitte le monde ou ferme le jeu) : tout ce qui n'est pas
    /// encore parti au relais du monde part maintenant (modifications en attente scellées en
    /// lot comme hôte principal, lots, confirmation), au lieu d'attendre la passation par
    /// l'autre hôte de la région.
    pub fn flush_root(&self) {
        let mut st = self.state.lock().unwrap();
        st.flush(true);
        st.send_root(true);
    }

    /// Du relais du monde (par la connexion de l'hôte), deux fois par seconde : le monde
    /// est servi, ou en pause. Redit à chaque joueur de ce relais : le relais du monde n'a
    /// plus à l'envoyer lui-même à chacun (v136).
    pub fn world_beat(&self, paused: bool) {
        let st = self.state.lock().unwrap();
        let msg = Response::Paused { paused };
        for c in st.conns.values().filter(|c| c.pseudo.is_some() && !c.peer) {
            transport::push(&c.tx, &msg);
        }
    }

    /// Messages pour le relais du monde (lots à archiver, confirmations, accusations,
    /// passations), à envoyer par la connexion de l'hôte.
    pub fn take_outbox(&self) -> Vec<Request> {
        std::mem::take(&mut self.state.lock().unwrap().outbox)
    }

    /// Clé du compte de l'hôte (il signe ce qu'il publie comme hôte principal).
    pub fn set_signer(&self, key: ed25519_dalek::SigningKey) {
        self.state.lock().unwrap().signer = Some(key);
    }

    /// Région souveraine : ce relais en est-il l'hôte principal ?
    pub fn is_primary(&self) -> bool {
        let st = self.state.lock().unwrap();
        st.sovereign.as_ref().is_some_and(|s| s.primary)
    }

    /// Région souveraine : modifications mises en ordre dans l'époque en cours (scellées
    /// comme hôte principal, ou vérifiées).
    pub fn ordered(&self) -> u64 {
        let st = self.state.lock().unwrap();
        st.sovereign
            .as_ref()
            .map_or(0, |s| s.checker.next.saturating_sub(s.sov.first))
    }

    /// Essais seulement : comme hôte principal, le prochain lot contiendra une
    /// modification inventée au nom d'un joueur (mal signée), montrée aux joueurs de ce
    /// relais. Les autres relais de la région doivent la voir, l'accuser, et le relais du
    /// monde écarter cet hôte.
    #[doc(hidden)]
    pub fn forge_once(&self) {
        self.state.lock().unwrap().forge = true;
    }

    /// Messages de voix remis par ce relais (essais).
    pub fn voice_sent(&self) -> usize {
        self.state.lock().unwrap().voice_sent
    }

    /// Joueurs entrés dans ce relais.
    pub fn players(&self) -> usize {
        let st = self.state.lock().unwrap();
        st.conns
            .values()
            .filter(|c| c.pseudo.is_some() && !c.peer)
            .count()
    }

    /// Autres relais de la région auxquels ce relais est relié.
    pub fn linked(&self) -> usize {
        let st = self.state.lock().unwrap();
        st.peers.values().filter(|p| p.link.is_some()).count()
    }

    /// Comment joindre les autres relais (tunnel) ; sans cela, par leur adresse directe.
    pub fn set_dial(&self, dial: Dial) {
        self.state.lock().unwrap().dial = Some(dial);
    }

    /// Relais prêts de la région (liste du relais du monde, celui-ci compris) : ce relais
    /// se relie aux autres et quitte ceux qui sont partis. `sov` : l'époque en cours si la
    /// région est souveraine.
    pub fn set_peers(&self, list: &[proto::ShardInfo], sov: Option<proto::Sov>) {
        let mut st = self.state.lock().unwrap();
        let (region, slot) = (st.region, st.slot);
        let others: Vec<&proto::ShardInfo> = list
            .iter()
            .filter(|i| i.region == region && i.slot != slot)
            .collect();
        st.peers
            .retain(|s, p| others.iter().any(|i| i.slot == *s && i.same_relay(&p.info)));
        for info in others {
            if !st.peers.contains_key(&info.slot) {
                let client = st.connect(info);
                st.peers.insert(
                    info.slot,
                    PeerOut {
                        info: info.clone(),
                        client,
                        link: None,
                        broken: None,
                    },
                );
            }
        }
        // Époque : la même continue ; une autre (ou plus de souveraineté) : passation.
        let same = match (&st.sovereign, &sov) {
            (Some(cur), Some(new)) => cur.sov.epoch == new.epoch,
            (None, None) => true,
            _ => false,
        };
        if same {
            return;
        }
        if st.sovereign.is_some() {
            if st.sovereign.as_ref().is_some_and(|s| s.primary) {
                st.flush(true);
            }
            st.send_root(true);
            st.hand_over();
        }
        let Some(sov) = sov else { return };
        let primary_key = list
            .iter()
            .find(|i| i.region == region && i.slot == sov.primary)
            .and_then(|i| ed25519_dalek::VerifyingKey::from_bytes(&i.signer).ok());
        // Deux relais confirment ce qu'ils vérifient : les deux plus petits rangs après
        // l'hôte principal.
        let mut witnesses: Vec<u16> = list
            .iter()
            .filter(|i| i.region == region && i.slot != sov.primary)
            .map(|i| i.slot)
            .collect();
        witnesses.sort_unstable();
        let acks = witnesses.iter().take(2).any(|&s| s == slot);
        let rules = sovereign::Rules {
            room: st.room,
            region,
            root: st.root,
            floor_t: sov.floor_t,
        };
        let mut checker = Checker::new(rules, sov.epoch, sov.base, sov.chain);
        for c in st.certs.values() {
            checker.learn(c);
        }
        log::info!(
            "relais {slot} de la région {region:?} : époque souveraine {} ({})",
            sov.epoch,
            if sov.primary == slot {
                "hôte principal"
            } else {
                "vérifie l'hôte principal"
            }
        );
        st.sovereign = Some(Sovereign {
            primary: sov.primary == slot,
            sov,
            primary_key,
            checker,
            pending: Vec::new(),
            pending_since: None,
            recent: VecDeque::new(),
            early: BTreeMap::new(),
            faulty: false,
            acks,
            asked: None,
        });
        // Les lots de cette époque arrivés avant son annonce.
        let epoch = st.sovereign.as_ref().map_or(0, |s| s.sov.epoch);
        let orphans = std::mem::take(&mut st.orphans);
        let (now, later): (Vec<Batch>, Vec<Batch>) =
            orphans.into_iter().partition(|b| b.epoch == epoch);
        st.orphans = later.into_iter().filter(|b| b.epoch > epoch).collect();
        let mut now = now;
        now.sort_by_key(|b| b.first);
        for b in now {
            st.follow(b);
        }
    }

    /// À appeler régulièrement : liens vers les autres relais (prêts, cassés, retentés).
    pub fn tick(&self) {
        let mut st = self.state.lock().unwrap();
        st.flush(false);
        st.send_root(false);
        // Lots trop vieux : plus gardés.
        if let Some(sv) = st.sovereign.as_mut() {
            while sv
                .recent
                .front()
                .is_some_and(|(t, _)| t.elapsed() > KEEP_BATCHES)
            {
                sv.recent.pop_front();
            }
        }
        let mut retry = Vec::new();
        let mut linked = Vec::new();
        for (&slot, p) in &mut st.peers {
            while let Some(e) = p.client.poll() {
                match e {
                    Event::Link(l) => {
                        p.link = Some(l);
                        linked.push(slot);
                    }
                    Event::Error(m) | Event::Offline(m) => {
                        log::info!("relais de région : lien vers le relais {slot} : {m}");
                        p.link = None;
                        p.broken.get_or_insert_with(Instant::now);
                    }
                    _ => {}
                }
            }
            if p.broken
                .is_some_and(|t| t.elapsed() > Duration::from_secs(2))
            {
                retry.push(slot);
            }
        }
        for slot in retry {
            let Some(info) = st.peers.get(&slot).map(|p| p.info.clone()) else {
                continue;
            };
            let client = st.connect(&info);
            if let Some(p) = st.peers.get_mut(&slot) {
                p.client = client;
                p.link = None;
                p.broken = None;
            }
        }
        // Hôte principal : un relais qui vient d'être relié reçoit les lots récents (il
        // vérifie à partir de ce que le relais du monde a archivé, et ignore le reste).
        let recent: Vec<Batch> = st
            .sovereign
            .as_ref()
            .filter(|s| s.primary)
            .map(|s| s.recent.iter().map(|(_, b)| b.clone()).collect())
            .unwrap_or_default();
        for slot in linked {
            if let Some(l) = st.peers.get(&slot).and_then(|p| p.link.clone()) {
                for b in &recent {
                    transport::push(&l, &Request::PeerBatch { batch: b.clone() });
                }
            }
        }
    }

    /// Ferme le relais (les joueurs repassent par le relais du monde ou un autre hôte).
    pub fn close(&self) {
        let mut st = self.state.lock().unwrap();
        st.closed = true;
        for c in st.conns.values() {
            let _ = c.tx.send(Inbound::Closed);
        }
    }
}

impl Drop for ShardHandle {
    fn drop(&mut self) {
        self.close();
    }
}

/// Lance le relais de rang `slot` de la région `region` du monde `room` (écoute sur
/// `bind`).
pub fn start(
    bind: &str,
    room: u64,
    region: [i32; 2],
    secret: [u8; 32],
    slot: u16,
    root: [u8; 32],
) -> Result<ShardHandle> {
    let listener = TcpListener::bind(bind)?;
    let addr = listener.local_addr()?;
    let (private, public) = transport::generate_static();
    let state = Arc::new(Mutex::new(State {
        room,
        region,
        secret,
        conns: HashMap::new(),
        // Numéros de joueurs propres à ce relais (uniques dans la région).
        next_id: ((slot as u64) << 40) | 1,
        build: zones::Build::default(),
        closed: false,
        slot,
        peers: HashMap::new(),
        dial: None,
        root,
        signer: None,
        certs: HashMap::new(),
        sovereign: None,
        orphans: Vec::new(),
        cells: HashMap::new(),
        outbox: Vec::new(),
        archive: Vec::new(),
        archive_since: None,
        ack: None,
        ack_since: None,
        forge: false,
        voice_rate: HashMap::new(),
        players: HashMap::new(),
        voice_sent: 0,
    }));
    let st = state.clone();
    std::thread::Builder::new()
        .name("relais de région".into())
        .spawn(move || {
            // Connexions ouvertes par joueur (nommé par le tunnel) : limitées, comme sur le
            // relais du monde.
            let open: Arc<Mutex<crate::OpenSlots>> = Arc::default();
            for conn in listener.incoming() {
                if st.lock().unwrap().closed {
                    return;
                }
                let Ok(stream) = conn else { continue };
                let st = st.clone();
                let open = open.clone();
                let _ = std::thread::Builder::new()
                    .name("relais de région : joueur".into())
                    .spawn(move || {
                        // Par le tunnel iroh : le préambule d'abord.
                        let local = stream.peer_addr().is_ok_and(|a| a.ip().is_loopback());
                        let who = if local {
                            match crate::tunnel_peer(&stream) {
                                Ok(w) => w,
                                Err(_) => return,
                            }
                        } else {
                            stream.peer_addr().ok().map(|a| a.ip().to_string())
                        };
                        // Connexion locale directe (essais) : pas de limite.
                        let _slot = match &who {
                            Some(w) => match crate::PeerSlot::take_named(&open, w) {
                                Some(s) => Some(s),
                                None => return,
                            },
                            None => None,
                        };
                        let _ = serve(stream, &st, &private);
                    });
            }
        })?;
    log::info!("relais {slot} de la région {region:?} du monde {room} sur {addr}");
    Ok(ShardHandle {
        addr,
        public,
        state,
    })
}

fn err(code: ErrorCode, message: &str) -> Response {
    Response::Error {
        code,
        message: message.into(),
    }
}

fn serve(stream: TcpStream, state: &Mutex<State>, private: &[u8; 32]) -> Result<()> {
    let mut chan = Channel::accept(stream, private, Duration::from_secs(10))?;
    let id = {
        let mut st = state.lock().unwrap();
        let id = st.next_id;
        st.next_id += 1;
        st.conns.insert(
            id,
            Conn {
                tx: chan.pusher(),
                pseudo: None,
                peer: false,
                slot: None,
            },
        );
        id
    };
    let r = serve_loop(&mut chan, state, id);
    let mut st = state.lock().unwrap();
    st.forget(id);
    st.build.left(id);
    r
}

fn serve_loop(chan: &mut Channel, state: &Mutex<State>, id: u64) -> Result<()> {
    loop {
        let req: Request = match chan.recv_any(Duration::from_secs(60))? {
            None => return Ok(()),
            Some(Incoming::Push(bytes)) => {
                chan.send_raw(&bytes)?;
                continue;
            }
            Some(Incoming::Msg(r)) => r,
        };
        if state.lock().unwrap().closed {
            return Ok(());
        }
        let resp = handle(req, state, id);
        if let Some(r) = resp {
            chan.send(&r)?;
        }
    }
}

fn handle(req: Request, state: &Mutex<State>, id: u64) -> Option<Response> {
    let mut st = state.lock().unwrap();
    let logged = st.conns.get(&id).and_then(|c| c.pseudo.clone());
    let peer = st.conns.get(&id).is_some_and(|c| c.peer);
    match req {
        Request::Hello { protocol, .. } => Some(if protocol == proto::PROTOCOL {
            Response::Hello {
                protocol,
                relay: "Relais de région".into(),
                pow_bits: 0,
            }
        } else {
            err(ErrorCode::Protocol, "")
        }),
        Request::Ping => Some(Response::Pong),
        Request::ShardLogin {
            room,
            pseudo,
            expires,
            mac,
            cert,
        } => {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            let ok = room == st.room
                && expires > now
                && proto::shard_mac(&st.secret, room, &pseudo, expires) == mac;
            if !ok {
                return Some(err(ErrorCode::WrongCredentials, "billet refusé"));
            }
            let mut was = None;
            if let Some(c) = st.conns.get_mut(&id) {
                // Un autre relais de la région : son nom ne peut pas être celui d'un
                // joueur, et seul un hôte de ce monde a pu le signer.
                c.peer = pseudo.starts_with('#');
                c.slot = pseudo
                    .strip_prefix("#relais")
                    .and_then(|n| n.parse::<u16>().ok());
                was = c.pseudo.replace(pseudo.clone());
            }
            // (Index de la voix : un joueur, pas un autre relais.)
            if let Some(old) = was.map(|p| p.to_ascii_lowercase()) {
                if let Some(ids) = st.players.get_mut(&old) {
                    ids.retain(|i| *i != id);
                }
                st.players.retain(|_, ids| !ids.is_empty());
            }
            if !pseudo.starts_with('#') {
                st.players
                    .entry(pseudo.to_ascii_lowercase())
                    .or_default()
                    .push(id);
            }
            // Certificat de la clé du compte (signé par le relais du monde) : ses
            // modifications signées seront reconnues.
            let root = ed25519_dalek::VerifyingKey::from_bytes(&st.root).ok();
            if let Some(cert) = cert.filter(|c| {
                c.pseudo.eq_ignore_ascii_case(&pseudo) && root.is_some_and(|k| c.valid(&k, room))
            }) {
                if let Some(sv) = st.sovereign.as_mut() {
                    sv.checker.learn(&cert);
                }
                st.certs.insert(pseudo.to_ascii_lowercase(), cert);
            }
            Some(Response::Welcome {
                pseudo,
                totp: false,
            })
        }
        _ if logged.is_none() => (!req.is_oneway()).then(|| err(ErrorCode::NotLoggedIn, "")),
        // D'un autre relais de la région : la position d'un de ses joueurs, pour les
        // joueurs d'ici qui en sont proches.
        Request::PeerNearby {
            peer: from,
            chunk,
            data,
        } if peer => {
            let (name, pose) = game::pose_of(&data)?;
            let pose = Arc::new(transport::PoseOf {
                id: from,
                name,
                pose,
            });
            let relays = st.peers.len() + 1;
            for m in st.build.nearby(from, chunk, relays) {
                if let Some(c) = st.conns.get(&m) {
                    transport::push_pose(&c.tx, &pose);
                }
            }
            None
        }
        // D'un autre relais : la modification signée d'un de ses joueurs (ce relais est
        // l'hôte principal).
        Request::PeerAsk { edit, cert } if peer => {
            let from = st.conns.get(&id).and_then(|c| c.slot);
            if let Some(slot) = from {
                st.admit(edit, cert, From::Peer(slot));
            }
            None
        }
        // De l'hôte principal : un lot à vérifier.
        Request::PeerBatch { batch } if peer => {
            st.follow(batch);
            None
        }
        // D'un autre relais : des lots lui manquent (ce relais est l'hôte principal).
        Request::PeerResend { from } if peer => {
            let slot = st.conns.get(&id).and_then(|c| c.slot);
            let batches: Vec<Batch> = st
                .sovereign
                .as_ref()
                .filter(|s| s.primary)
                .map(|s| {
                    s.recent
                        .iter()
                        .map(|(_, b)| b)
                        .filter(|b| b.end() > from)
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            let link = slot
                .and_then(|s| st.peers.get(&s))
                .and_then(|p| p.link.clone());
            if let Some(l) = link {
                for batch in batches {
                    transport::push(&l, &Request::PeerBatch { batch });
                }
            }
            None
        }
        // De l'hôte principal : la modification d'un joueur d'ici est refusée.
        Request::PeerReject { pseudo, pos } if peer => {
            let ids: Vec<u64> = st
                .conns
                .iter()
                .filter(|(_, c)| {
                    !c.peer
                        && c.pseudo
                            .as_deref()
                            .is_some_and(|p| p.eq_ignore_ascii_case(&pseudo))
                })
                .map(|(&id, _)| id)
                .collect();
            for i in ids {
                st.refuse(From::Local(i), &pseudo, pos);
            }
            None
        }
        // D'un autre relais de la région : la voix d'un de ses joueurs, pour les nôtres.
        Request::PeerVoice { from, to, data } if peer => {
            if data.len() <= VOICE_BYTES {
                st.deliver_voice(&from, &voice_names(to), &data);
            }
            None
        }
        Request::PeerNearby { .. }
        | Request::PeerAsk { .. }
        | Request::PeerBatch { .. }
        | Request::PeerReject { .. }
        | Request::PeerResend { .. }
        | Request::PeerVoice { .. } => None,
        _ if peer => None,
        // Voix d'un joueur d'ici (v137) : à ceux de ses destinataires entrés dans ce relais,
        // et une fois à chaque autre relais de la région (pour les siens). Le nom de celui
        // qui parle est celui de son billet ; le contenu, chiffré de bout en bout, reste
        // illisible ici.
        Request::VoiceTo { to, data } => {
            let from = logged.clone()?;
            if data.len() > VOICE_BYTES || !st.voice_due(id) {
                return None;
            }
            let to = voice_names(to);
            st.deliver_voice(&from, &to, &data);
            if let Some(fwd) = transport::encode(&Request::PeerVoice { from, to, data }) {
                for l in st.peers.values().filter_map(|p| p.link.as_ref()) {
                    transport::push_raw(l, fwd.clone());
                }
            }
            None
        }
        Request::Position { chunk } => {
            let logs = st.build.moved(id, chunk, &|_| true);
            if let Some(c) = st.conns.get(&id) {
                for (zone, log) in logs {
                    let chunk = [zone[0] * proto::ZONE, zone[1] * proto::ZONE];
                    for data in log {
                        crate::push_zone(&c.tx, chunk, data);
                    }
                }
            }
            None
        }
        Request::Nearby { chunk, data } => {
            // Le pseudo montré est celui du billet (pas celui que le joueur annonce).
            let data = crate::player_as(&data, logged.as_deref().unwrap_or_default())?;
            // Plus de positions que le jeu n'en envoie : pas diffusée.
            if !st.build.pos_due(id) {
                return None;
            }
            // Vers chaque voisin, regroupée avec celles des autres ; périssable (la
            // suivante la remplace). Chaque relais de la région ne sert que sa part des
            // voisins.
            let relays = st.peers.len() + 1;
            let (name, pose) = game::pose_of(&data)?;
            let pose = Arc::new(transport::PoseOf { id, name, pose });
            for m in st.build.nearby(id, chunk, relays) {
                if let Some(c) = st.conns.get(&m) {
                    transport::push_pose(&c.tx, &pose);
                }
            }
            // Une fois à chaque autre relais de la région, pour ses propres joueurs.
            if let Some(fwd) = transport::encode(&Request::PeerNearby {
                peer: id,
                chunk,
                data,
            }) {
                for l in st.peers.values().filter_map(|p| p.link.as_ref()) {
                    transport::push_loose(l, fwd.clone());
                }
            }
            // Modifications pas encore reçues (foule, voir `zones::LIVE_MAX`).
            if st.build.positions.contains_key(&id) {
                let logs = st.build.catch_up(id, Some(chunk), &|_| true);
                if let Some(c) = st.conns.get(&id) {
                    for (zone, log) in logs {
                        let chunk = [zone[0] * proto::ZONE, zone[1] * proto::ZONE];
                        for data in log {
                            crate::push_zone(&c.tx, chunk, data);
                        }
                    }
                }
            }
            None
        }
        // Région souveraine : une modification signée par son auteur (l'hôte principal
        // l'accepte ; un autre relais la lui passe). Région ordinaire : les modifications
        // vont au relais du monde, une demande arrivée ici quand même est refusée.
        Request::ToZone { data, .. } => {
            let me = logged.unwrap_or_default();
            match game::decode::<BuildMsg>(&data) {
                Some(BuildMsg::Signed(edit)) if edit.pseudo.eq_ignore_ascii_case(&me) => {
                    let (primary, faulty, primary_slot) = match &st.sovereign {
                        Some(sv) => (sv.primary, sv.faulty, Some(sv.sov.primary)),
                        None => (false, true, None),
                    };
                    if faulty {
                        st.refuse(From::Local(id), &edit.pseudo, edit.pos);
                    } else if primary {
                        st.admit(edit, None, From::Local(id));
                    } else {
                        let cert = st.certs.get(&me.to_ascii_lowercase()).cloned();
                        let link = primary_slot
                            .and_then(|s| st.peers.get(&s))
                            .and_then(|p| p.link.clone());
                        match link {
                            Some(l) => {
                                transport::push(&l, &Request::PeerAsk { edit, cert });
                            }
                            None => st.refuse(From::Local(id), &edit.pseudo, edit.pos),
                        }
                    }
                }
                Some(BuildMsg::Signed(edit)) => st.refuse(From::Local(id), &me, edit.pos),
                Some(BuildMsg::Edit { pos, .. }) => st.refuse(From::Local(id), &me, pos),
                _ => {}
            }
            None
        }
        r => (!r.is_oneway()).then(|| err(ErrorCode::Protocol, "relais de région")),
    }
}
