//! Construction en ligne, sans limite de joueurs.
//!
//! Le relais découpe le monde en zones (`proto::ZONE` × `ZONE` chunks) et confie chaque
//! zone active à un garant : de préférence un joueur d'un autre monde, sans lien avec
//! celui-ci (sa connexion de garant, en arrière-plan), sinon un joueur du monde. Il y a
//! d'autant plus de garants que le monde a de joueurs et de zones actives.
//!
//! **Une modification** (poser, casser) suit toujours le même chemin, pour tout le monde :
//! 1. le joueur l'applique tout de suite chez lui et l'envoie comme demande (`Edit`) au
//!    garant de la zone, en gardant le bloc d'avant ;
//! 2. le garant vérifie (bloc valide, dans sa zone, pas plus de 20 par seconde) et publie
//!    (`Set`), ou refuse (`Reject`, remis au seul demandeur, qui remet le bloc d'avant) ;
//! 3. le relais transmet la publication aux joueurs proches et la garde dans l'historique
//!    de la zone, compacté (la dernière modification de chaque case, numérotée) ;
//! 4. sans réponse en 4 secondes (garant parti), le joueur annule.
//!
//! Chacun reçoit exactement ce qui lui manque : en s'approchant d'une zone (même après
//! s'en être éloigné un moment), en arrivant dans le monde, ou par paquets pour le
//! propriétaire, qui enregistre le monde. Le monde de chacun est celui des garants.
//!
//! **Les positions** des joueurs ne passent pas par les garants : le relais les transmet
//! aux joueurs proches, toutes tout près, une sur 4 un peu plus loin, une sur 20 au loin ;
//! chaque jeu fait avancer les autres en douceur entre deux positions.
//!
//! **Ajouter un comportement synchronisé** (porte, coffre, mécanisme…) : une nouvelle
//! sorte de `BuildMsg` (dans `minelib_net::game`), demandée au garant de la zone, vérifiée
//! dans `validate`, publiée comme `Set` ; le relais la garde dans l'historique (compactée
//! par case dans `zones::ZoneLog` si c'est un état). Les essais de bout en bout de ce
//! fichier (`run`) vérifient que tous les mondes restent identiques.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::Sender;

use glam::Vec3;
use minelib_net::client::{Client, Cmd, Event};
use minelib_net::game;
use minelib_net::proto::{self, Request};
use minelib_net::transport::{self, Inbound};
use minelib_world::{BlockId, ChunkPos, World};

/// Changement que l'écoulement d'un fluide peut faire : fluide qui avance (dans le vide,
/// une plante ou un fluide), qui sèche, ou qui se fige (obsidienne, pierre taillée, pierre).
fn fluid_edit(old: BlockId, new: BlockId) -> bool {
    use minelib_world::registry::{fluid, Shape};
    let open = |b: BlockId| {
        b == BlockId::AIR || fluid(b).is_some() || matches!(b.def().shape, Shape::Cross)
    };
    let set = |b: BlockId| {
        b == BlockId::AIR
            || fluid(b).is_some()
            || b == BlockId::COBBLESTONE
            || b == BlockId::STONE
            || Some(b) == minelib_world::registry::registry().by_key("obsidian")
    };
    (fluid(new).is_some() && open(old)) || (fluid(old).is_some() && set(new))
}
/// Position envoyée au moins 10 fois par seconde (foule), au plus 60 (peu de joueurs) ; le
/// jeu fait avancer chacun en douceur entre deux positions reçues.
///
/// Avant : de 20 à 256 fois par seconde, pour 4096 positions reçues par joueur. La
/// simulation de foule a montré ce que ça coûte au relais : 250 Mbit/s à envoyer pour
/// 100 joueurs au même endroit, et déjà 55 Mbit/s pour 16. C'est la connexion de celui
/// qui héberge le relais qui cassait en premier, bien avant le nombre de joueurs.
const POS_EVERY: f32 = 0.1;
const POS_FASTEST: f32 = 1.0 / 60.0;
/// Positions reçues par seconde qu'on vise au plus (tous les voisins proches réunis) : le
/// rythme d'envoi de chacun baisse quand il y a du monde autour.
const POS_BUDGET: f32 = 960.0;
/// Un joueur dont on n'a plus de nouvelles depuis ce temps (ou trois de ses intervalles,
/// pour un joueur lointain rafraîchi rarement) n'est plus montré.
const STALE_S: f32 = 3.0;
/// Servi par un relais de région : là où l'on est, redit au relais du monde toutes les…
/// (gardé pour notre retour sur ce serveur).
const SPOT_EVERY: f32 = 10.0;

/// Intervalle entre deux envois de sa position, selon le nombre de joueurs autour : 60
/// fois par seconde jusqu'à 16 voisins, puis de moins en moins (30 à 32 voisins), jamais
/// moins de 10.
fn pos_every(others: usize) -> f32 {
    let hz = (POS_BUDGET / others.max(1) as f32).clamp(1.0 / POS_EVERY, 1.0 / POS_FASTEST);
    1.0 / hz
}

pub use minelib_net::game::BuildMsg;

/// Une modification non confirmée par le relais du monde depuis ce temps est annulée
/// (message perdu) : le monde de chacun reste celui du relais.
const PENDING_TIMEOUT: f32 = 4.0;
/// Cases dont on retient le rang de la dernière modification, au plus (au-delà, tout est
/// oublié : ce rang ne sert qu'à écarter une modification arrivée en retard par un autre
/// chemin, ce qui se joue en quelques secondes).
const SEEN_MAX: usize = 1 << 20;
/// Pseudos retenus au plus (voir `BuildNet::names`) ; au-delà, on repart de zéro : chaque
/// relais redit un pseudo toutes les cinq secondes.
const NAMES_MAX: usize = 1 << 16;

pub struct Other {
    pub pseudo: String,
    /// Son numéro sur le relais du monde (vocal), s'il passe par lui.
    pub id: Option<u64>,
    /// Position montrée : elle rejoint la dernière reçue en douceur (les joueurs lointains
    /// envoient moins souvent, voir le relais).
    pub pos: Vec3,
    target: Vec3,
    /// Intervalle moyen entre deux positions reçues (s).
    interval: f32,
    pub yaw: f32,
    pub pitch: f32,
    pub crouch: f32,
    #[allow(dead_code)]
    pub speed: f32,
    seen: f32,
}

/// Le bloc qui fait foi en `pos` vient de changer (`truth` : publié par le garant, ou
/// revenu après un refus) alors que le joueur a déjà rechangé cette case sans que ce soit
/// encore envoyé : ces modifications partent désormais de `truth` (c'est lui qui reviendra
/// si elles sont refusées), et restent à l'écran. Vrai s'il y en avait.
fn rebase_journal(world: &mut World, pos: [i32; 3], truth: BlockId) -> bool {
    let Some(journal) = world.journal.as_mut() else {
        return false;
    };
    let mut found = false;
    for e in journal.iter_mut().filter(|e| e.0 == pos) {
        e.2 = truth;
        found = true;
    }
    found
}

/// Modification du joueur en attente de la confirmation du garant : bloc d'avant,
/// modifications de cette case pas encore confirmées, âge.
struct Pending {
    old: BlockId,
    count: u32,
    age: f32,
    /// Dernière modification de cette case partie signée au relais d'une région
    /// souveraine (bloc, fluide) : refusée par ce relais ou sans réponse, elle repart une
    /// fois au relais du monde (voir `retry_at_root`).
    signed: Option<(u16, bool)>,
    /// Dernière modification demandée (bloc, fluide) : elle repart après une pause du
    /// monde ou sa reprise par le relais de secours.
    last: (u16, bool),
}

pub struct BuildNet {
    link: Sender<Inbound>,
    owner: bool,
    pseudo: String,
    /// Zones dont ce jeu est le garant.
    pub zones: HashSet<[i32; 2]>,
    /// Joueurs proches, par pseudo (le même joueur peut passer d'un relais de région à
    /// l'autre).
    pub others: HashMap<String, Other>,
    chunk: Option<[i32; 2]>,
    pos_timer: f32,
    /// Rang (ordre du relais du monde) de la dernière modification appliquée à chaque
    /// case, et relais de région qui l'a remise (son numéro de connexion ; aucun : le
    /// relais du monde). Une modification plus ancienne arrivée après (doublon, autre
    /// chemin) est sans effet.
    seen: HashMap<[i32; 3], (u64, Option<u64>)>,
    /// Pseudo de chaque joueur dont un relais envoie les positions par son numéro :
    /// (relais de région qui le dit, aucun pour le relais du monde ; numéro) → pseudo.
    names: HashMap<(Option<u64>, u64), String>,
    /// Blocs remis par chaque relais de région (numéro de connexion) que le relais du
    /// monde n'a pas encore confirmés.
    heard: HashMap<u64, HashMap<[i32; 3], Heard>>,
    /// Ceux d'un relais quitté, et le temps qu'il leur reste : ce que le relais du monde
    /// n'a pas confirmé d'ici là revient comme avant (voir `forget_shard`).
    doubtful: HashMap<[i32; 3], (Heard, f32)>,
    /// Blocs défaits ainsi (essais).
    #[cfg(test)]
    undone: usize,
    /// Régions annoncées au relais du monde comme servies par un relais de région.
    served_sent: Vec<[i32; 2]>,
    /// Un relais de région a redit le battement du relais du monde depuis la dernière
    /// lecture (`take_relayed_beat`) : le relais du monde est en vie (v136 : il ne l'envoie
    /// plus lui-même deux fois par seconde aux joueurs servis par un relais de région).
    relayed_beat: bool,
    /// Prochain envoi au relais du monde de là où l'on est (`Request::Spot`).
    spot_timer: f32,
    /// Clé du compte (modifications signées dans une région souveraine), et date de la
    /// dernière signature.
    identity: Option<minelib_net::ed25519_dalek::SigningKey>,
    sign_t: u64,
    caps_sent: bool,
    pending: HashMap<[i32; 3], Pending>,
    shards: Shards,
    /// Modifications en masse (WorldEdit d'un OP) envoyées au relais : blocs d'avant, pour
    /// les remettre s'il refuse.
    bulks: HashMap<u32, Vec<([i32; 3], BlockId)>>,
    next_bulk: u32,
    /// États d'objets (au sol, coffres) et refus reçus du relais du monde, à traiter par
    /// la partie (voir `crate::drops`).
    pub item_inbox: Vec<BuildMsg>,
    /// Message en cours de traitement venu d'un relais de région (tenu par un joueur ; son
    /// numéro de connexion) : seul le relais du monde arbitre les objets, on n'en accepte
    /// pas d'ailleurs.
    from_shard: Option<u64>,
    /// Monde en pause (son relais n'entend plus assez de gardiens, voir
    /// `minelib_net::guard`) : les modifications du joueur attendent la reprise.
    paused: bool,
}

/// Bloc remis par un relais de région : celui d'avant, celui remis, son rang ; et si
/// c'était la confirmation d'une modification du joueur (fluide ou non).
#[derive(Clone, Copy, Debug)]
struct Heard {
    pre: BlockId,
    got: BlockId,
    seq: u64,
    mine: Option<bool>,
}

/// Blocs non confirmés gardés par relais de région (au-delà, ils ne pourront pas être
/// défaits).
const HEARD_MAX: usize = 1 << 17;
/// Temps laissé au relais du monde pour confirmer les blocs d'un relais de région quitté
/// (secondes) : il renvoie alors tout ce qui manque de la région.
const DOUBT_WAIT: f32 = 3.0;

/// Relais de région (voir `minelib_relay::shard`) : la liste reçue du relais du monde, le
/// billet pour y entrer, et une connexion à chaque région en vue. Chaque message part au
/// relais de la région qu'il concerne ; une région sans relais passe par le relais du monde.
#[derive(Default)]
struct Shards {
    ticket: Option<Ticket>,
    /// Relais de chaque région (plusieurs quand elle est très peuplée : on n'entre que
    /// dans le sien, `proto::shard_for`).
    list: HashMap<[i32; 2], Vec<proto::ShardInfo>>,
    pseudo: String,
    conns: HashMap<[i32; 2], ShardConn>,
    /// Dossier du nœud iroh (tunnel vers un relais de région hors du réseau local).
    node_dir: Option<std::path::PathBuf>,
    /// Dernier numéro de connexion donné.
    serial: u64,
}

/// Billet pour les relais de région : monde, pseudo, fin de validité, sceau, et
/// certificat de la clé du compte.
type Ticket = (
    u64,
    String,
    u64,
    [u8; 32],
    Option<minelib_net::sovereign::Cert>,
);

struct ShardConn {
    /// Numéro de cette connexion (un autre relais de la même région en aura un autre).
    serial: u64,
    info: proto::ShardInfo,
    client: Client,
    link: Option<Sender<Inbound>>,
    /// Dernier chunk annoncé à ce relais.
    sent_chunk: Option<[i32; 2]>,
}

impl Shards {
    fn link(&self, region: [i32; 2]) -> Option<&Sender<Inbound>> {
        self.conns.get(&region).and_then(|c| c.link.as_ref())
    }

    /// Se connecte aux relais des régions en vue de `chunk`, quitte les autres.
    fn keep_near(&mut self, chunk: [i32; 2]) {
        let lo = proto::region_of([chunk[0] - VIEW_CHUNKS, chunk[1] - VIEW_CHUNKS]);
        let hi = proto::region_of([chunk[0] + VIEW_CHUNKS, chunk[1] + VIEW_CHUNKS]);
        let in_view =
            |r: [i32; 2]| (lo[0]..=hi[0]).contains(&r[0]) && (lo[1]..=hi[1]).contains(&r[1]);
        let me = self.pseudo.clone();
        let mine = |list: &Vec<proto::ShardInfo>| proto::shard_for(list, &me).cloned();
        // (Le même relais dont seule l'époque a changé : on y reste.)
        self.conns.retain(|r, c| {
            let Some(i) = self.list.get(r).and_then(&mine) else {
                return false;
            };
            if !in_view(*r) || !i.same_relay(&c.info) {
                return false;
            }
            // (Une modification n'y part signée que pendant une époque souveraine.)
            c.info.epoch = i.epoch;
            true
        });
        let Some((room, pseudo, expires, mac, cert)) = self.ticket.clone() else {
            return;
        };
        for (region, list) in &self.list {
            if !in_view(*region) || self.conns.contains_key(region) {
                continue;
            }
            let Some(info) = mine(list) else { continue };
            let info = &info;
            let (addr, key, node) = (info.addr.clone(), info.key, info.node);
            let dir = self.node_dir.clone();
            // Directement (réseau local), sinon par un tunnel iroh vers le nœud de l'hôte.
            let client = Client::spawn_resolved(Box::new(move || {
                if let Ok(a) = addr.parse::<std::net::SocketAddr>() {
                    if std::net::TcpStream::connect_timeout(
                        &a,
                        std::time::Duration::from_millis(800),
                    )
                    .is_ok()
                    {
                        return Ok((addr.clone(), Some(key)));
                    }
                }
                let n = dir
                    .as_deref()
                    .and_then(crate::account::p2p_node)
                    .ok_or_else(|| anyhow::anyhow!("relais de région injoignable"))?;
                let local = n.tunnel(node)?;
                Ok((local.to_string(), Some(key)))
            }));
            client.send(Cmd::ShardLogin {
                room,
                pseudo: pseudo.clone(),
                expires,
                mac,
                cert: cert.clone(),
            });
            self.serial += 1;
            self.conns.insert(
                *region,
                ShardConn {
                    serial: self.serial,
                    info: info.clone(),
                    client,
                    link: None,
                    sent_chunk: None,
                },
            );
        }
    }
}

/// Distance de vue (chunks) : un relais de région à moins de cela est rejoint.
const VIEW_CHUNKS: i32 = 12;

fn chunk_of(x: i32, z: i32) -> [i32; 2] {
    let c = ChunkPos::containing(x, z);
    [c.x, c.z]
}

impl BuildNet {
    pub fn new(link: Sender<Inbound>, pseudo: String, owner: bool) -> Self {
        Self {
            link,
            owner,
            pseudo,
            zones: HashSet::new(),
            others: HashMap::new(),
            chunk: None,
            pos_timer: 0.0,
            seen: HashMap::new(),
            names: HashMap::new(),
            served_sent: Vec::new(),
            relayed_beat: false,
            spot_timer: 0.0,
            identity: None,
            sign_t: 0,
            caps_sent: false,
            pending: HashMap::new(),
            item_inbox: Vec::new(),
            from_shard: None,
            paused: false,
            heard: HashMap::new(),
            doubtful: HashMap::new(),
            #[cfg(test)]
            undone: 0,
            bulks: HashMap::new(),
            next_bulk: 0,
            shards: Shards::default(),
        }
    }

    /// Dossier du nœud iroh de ce jeu (tunnels vers les relais de région).
    pub fn set_node_dir(&mut self, dir: std::path::PathBuf) {
        self.shards.node_dir = Some(dir);
    }

    /// Relais des régions du monde (envoyé par le relais du monde).
    pub fn set_shards(&mut self, list: Vec<proto::ShardInfo>) {
        self.shards.pseudo = self.pseudo.clone();
        self.shards.list.clear();
        for i in list {
            self.shards.list.entry(i.region).or_default().push(i);
        }
    }

    /// Billet pour les relais de région, et certificat de la clé du compte.
    pub fn set_ticket(
        &mut self,
        room: u64,
        pseudo: String,
        expires: u64,
        mac: [u8; 32],
        cert: Option<minelib_net::sovereign::Cert>,
    ) {
        self.shards.pseudo = pseudo.clone();
        self.shards.ticket = Some((room, pseudo, expires, mac, cert));
    }

    /// Clé du compte : signe ses modifications dans une région souveraine.
    pub fn set_identity(&mut self, key: Option<minelib_net::ed25519_dalek::SigningKey>) {
        self.identity = key;
    }

    /// Le monde est repris par son relais de secours : tout part désormais par cette
    /// connexion ; les relais de région de l'ancien relais du monde sont quittés (ce
    /// qu'ils ont montré sans confirmation sera défait, voir `forget_shard`), le relais
    /// de secours renvoie ce qui manque.
    pub fn set_link(&mut self, link: Sender<Inbound>) {
        self.link = link;
        self.shards.list.clear();
        self.shards.ticket = None;
        self.served_sent.clear();
        let chunk = self.chunk.take();
        self.caps_sent = false;
        // Ce qui attendait encore sa réponse repart au relais de secours (rien ne se perd :
        // l'ancien relais du monde n'avait peut-être pas pu le lui transmettre).
        self.paused = false;
        self.resend_pending(chunk);
    }

    /// Monde en pause (son relais n'entend plus assez de gardiens) : les modifications du
    /// joueur attendent, ni envoyées ni annulées ; à la reprise, celles restées sans
    /// réponse repartent. Vrai si ça change.
    pub fn set_paused(&mut self, on: bool) -> bool {
        if on == self.paused {
            return false;
        }
        self.paused = on;
        if !on {
            self.resend_pending(self.chunk);
        }
        true
    }

    #[cfg(test)]
    pub fn is_paused(&self) -> bool {
        self.paused
    }

    /// Les modifications en attente repartent (une seule demande par case : la dernière).
    fn resend_pending(&mut self, chunk: Option<[i32; 2]>) {
        let list: Vec<([i32; 3], (u16, bool))> =
            self.pending.iter().map(|(p, v)| (*p, v.last)).collect();
        for (pos, (block, fluid)) in list {
            let signed = chunk.is_some_and(|c| self.send_signed(pos, block, fluid, c));
            if !signed {
                self.send_edit(pos, block, fluid);
            }
            if let Some(p) = self.pending.get_mut(&pos) {
                p.count = 1;
                p.age = 0.0;
                p.signed = signed.then_some((block, fluid));
            }
        }
    }

    /// Région souveraine dont on a le relais : la modification y part signée (son hôte
    /// principal la met en ordre). Renvoie faux s'il faut passer par le relais du monde.
    fn send_signed(&mut self, pos: [i32; 3], block: u16, fluid: bool, at: [i32; 2]) -> bool {
        let chunk = [pos[0].div_euclid(16), pos[2].div_euclid(16)];
        let region = proto::region_of(chunk);
        let sovereign = self
            .shards
            .conns
            .get(&region)
            .is_some_and(|c| c.info.epoch > 0 && c.link.is_some());
        let (Some(key), Some((room, ..))) = (&self.identity, &self.shards.ticket) else {
            return false;
        };
        if !sovereign {
            return false;
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64);
        // Dates strictement croissantes (deux modifications dans la même milliseconde).
        self.sign_t = (self.sign_t + 1).max(now);
        let edit = minelib_net::sovereign::Edit::sign(
            key,
            *room,
            &self.pseudo,
            pos,
            block,
            fluid,
            at,
            self.sign_t,
            false,
        );
        let Some(l) = self.shards.link(region) else {
            return false;
        };
        transport::push(
            l,
            &Request::ToZone {
                chunk,
                data: game::encode(&BuildMsg::Signed(edit)),
            },
        )
    }

    /// Demande au relais du monde.
    fn send_edit(&self, pos: [i32; 3], block: u16, fluid: bool) {
        self.send(Request::ToZone {
            chunk: chunk_of(pos[0], pos[2]),
            data: game::encode(&BuildMsg::Edit { pos, block, fluid }),
        });
    }

    /// La dernière modification de `pos`, partie signée à un relais de région, est refusée
    /// par lui ou reste sans réponse : elle repart une fois au relais du monde, qui
    /// tranche (et la fait passer par l'hôte principal de la région, qui est remplacé
    /// s'il ne la met pas en ordre). Faux s'il n'y a rien à redemander.
    fn retry_at_root(&mut self, pos: [i32; 3]) -> bool {
        let Some((block, fluid)) = self.pending.get_mut(&pos).and_then(|p| {
            p.age = 0.0;
            p.signed.take()
        }) else {
            return false;
        };
        self.send_edit(pos, block, fluid);
        true
    }

    /// Relais de région rejoints (prêts).
    #[cfg(test)]
    pub fn shard_links(&self) -> usize {
        self.shards
            .conns
            .values()
            .filter(|c| c.link.is_some())
            .count()
    }

    /// Envoie au relais de la région de `chunk` (ou au relais du monde).
    fn send_at(&self, chunk: [i32; 2], req: Request) {
        match self.shards.link(proto::region_of(chunk)) {
            Some(l) => {
                transport::push(l, &req);
            }
            None => self.send(req),
        }
    }

    /// Ce jeu a ouvert le monde (il l'enregistre).
    pub fn is_owner(&self) -> bool {
        self.owner
    }

    /// Le propriétaire est parti : ce jeu le devient (il enregistrera le monde).
    pub fn set_owner(&mut self) {
        self.owner = true;
    }

    /// Un autre joueur garde maintenant le monde (meilleure connexion) : on n'en est plus
    /// le propriétaire.
    pub fn clear_owner(&mut self) {
        self.owner = false;
    }

    fn send(&self, req: Request) {
        transport::push(&self.link, &req);
    }

    /// Demande d'objet (lâcher, ramasser, coffre) : toujours au relais du monde, seul
    /// arbitre des objets, même dans une région servie par un autre relais.
    pub fn send_item(&self, chunk: [i32; 2], req: &BuildMsg) {
        if minelib_net::items::is_request(req) {
            self.send(Request::ToZone {
                chunk,
                data: game::encode(req),
            });
        }
    }

    /// Propriétaire du monde : remet au relais les objets de sa sauvegarde (le relais
    /// refuse ce qui ferait double emploi).
    pub fn seed_items(&self, saved: &[BuildMsg]) {
        if !self.owner {
            return;
        }
        for m in saved {
            if let Some(chunk) = minelib_net::items::chunk_of_msg(m) {
                self.send(Request::ToZone {
                    chunk,
                    data: game::encode(m),
                });
            }
        }
    }

    /// Mon pseudo sur ce serveur (celui que le relais met dans ses réponses).
    pub fn pseudo(&self) -> &str {
        &self.pseudo
    }

    /// WorldEdit d'un OP : les blocs changés (case, nouveau, ancien), déjà posés ici, partent
    /// au relais par paquets (il vérifie qu'on est OP et les publie à tous).
    pub fn bulk(&mut self, cells: Vec<([i32; 3], BlockId, BlockId)>) {
        for part in cells.chunks(4096) {
            self.next_bulk += 1;
            let id = self.next_bulk;
            self.bulks
                .insert(id, part.iter().map(|&(p, _, old)| (p, old)).collect());
            self.send(Request::BulkEdit {
                id,
                cells: part.iter().map(|&(p, b, _)| (p, b.0)).collect(),
            });
        }
    }

    /// Réponse du relais à un paquet de `bulk` : refusé, on remet les blocs d'avant.
    /// Renvoie les chunks à remailler.
    pub fn bulk_done(&mut self, id: u32, error: &str, world: &mut World) -> Vec<ChunkPos> {
        let Some(olds) = self.bulks.remove(&id) else {
            return Vec::new();
        };
        if error.is_empty() {
            return Vec::new();
        }
        let mut dirty = Vec::new();
        for (p, old) in olds.into_iter().rev() {
            dirty.extend(world.set_remote(p[0], p[1], p[2], old));
        }
        dirty
    }

    /// Capacités de ce PC pour être garant (annoncées une fois).
    pub fn caps(fps: f32) -> proto::GarantCaps {
        proto::GarantCaps {
            cores: std::thread::available_parallelism().map_or(1, |n| n.get() as u16),
            fps: fps.clamp(0.0, 1000.0) as u16,
            ram_mb: crate::session::ram_view_limit().max(0) as u32 * 256,
        }
    }

    /// Modifications du joueur pas encore confirmées par le relais du monde.
    #[cfg(test)]
    pub fn pending(&self) -> usize {
        self.pending.len()
    }

    /// Cases dont la dernière modification est arrivée par un relais de région.
    #[cfg(test)]
    pub fn via_shard(&self) -> usize {
        self.seen.values().filter(|v| v.1.is_some()).count()
    }

    /// À chaque image : modifications du joueur local (journal du monde), position,
    /// modifications refusées en silence (annulées), joueurs proches qui avancent en douceur.
    /// Renvoie les capacités à annoncer (une fois) et les chunks à remailler.
    #[allow(clippy::too_many_arguments)]
    pub fn update(
        &mut self,
        world: &mut World,
        feet: Vec3,
        look: (f32, f32),
        crouch: f32,
        speed: f32,
        fps: f32,
        dt: f32,
    ) -> (Option<proto::GarantCaps>, Vec<ChunkPos>) {
        let mut caps = None;
        if !self.caps_sent && fps > 0.0 {
            self.caps_sent = true;
            caps = Some(Self::caps(fps));
        }
        // Nouveau chunk : le relais du monde le sait avant de recevoir une demande de là
        // (il refuse une modification hors de portée de son auteur).
        let chunk = chunk_of(feet.x.floor() as i32, feet.z.floor() as i32);
        if self.chunk != Some(chunk) {
            self.chunk = Some(chunk);
            self.send(Request::Position { chunk });
        }
        // Ce que le joueur a changé (monde en pause : ça attend dans le journal).
        let edits = if self.paused {
            Vec::new()
        } else {
            world
                .journal
                .as_mut()
                .map(std::mem::take)
                .unwrap_or_default()
        };
        for (pos, id, old) in edits {
            // Une demande au relais du monde, qui accepte la modification et lui donne son
            // rang ; dans une région souveraine, signée au relais de la région, dont l'hôte
            // principal le fait (voir `minelib_net::sovereign`). Le bloc d'avant est gardé
            // pour un refus.
            let fluid = fluid_edit(old, id);
            let signed = self.send_signed(pos, id.0, fluid, chunk);
            if !signed {
                self.send_edit(pos, id.0, fluid);
            }
            let p = self.pending.entry(pos).or_insert(Pending {
                old,
                count: 0,
                age: 0.0,
                signed: None,
                last: (id.0, fluid),
            });
            p.count += 1;
            p.age = 0.0;
            p.signed = signed.then_some((id.0, fluid));
            p.last = (id.0, fluid);
        }
        // Sans réponse du relais : la modification n'a pas eu lieu (en pause, on attend).
        let mut dirty = Vec::new();
        let mut expired = Vec::new();
        for (pos, p) in &mut self.pending {
            if self.paused {
                break;
            }
            p.age += dt;
            if p.age > PENDING_TIMEOUT {
                expired.push((*pos, p.old));
            }
        }
        // Blocs d'un relais de région quitté que le relais du monde n'a pas confirmés : ils
        // reviennent comme avant (sauf si la case a changé depuis).
        let mut undone = 0;
        let late: Vec<[i32; 3]> = self
            .doubtful
            .iter_mut()
            .filter_map(|(p, (_, t))| {
                *t -= dt;
                (*t <= 0.0).then_some(*p)
            })
            .collect();
        for pos in late {
            let Some((h, _)) = self.doubtful.remove(&pos) else {
                continue;
            };
            if self.pending.contains_key(&pos) || world.get(pos[0], pos[1], pos[2]) != h.got {
                continue;
            }
            // Une modification du joueur que ce relais avait confirmée : elle est redemandée
            // au relais du monde (rien ne se perd pour un joueur honnête, même quand l'hôte
            // de la région triche ou tombe).
            if let Some(fluid) = h.mine {
                self.send_edit(pos, h.got.0, fluid);
                self.pending.insert(
                    pos,
                    Pending {
                        old: h.pre,
                        count: 1,
                        age: 0.0,
                        signed: None,
                        last: (h.got.0, fluid),
                    },
                );
                continue;
            }
            undone += 1;
            dirty.extend(world.set_remote(pos[0], pos[1], pos[2], h.pre));
        }
        #[cfg(test)]
        {
            self.undone += undone;
        }
        if undone > 0 {
            log::info!(
                "construction : {undone} blocs d'un relais de région quitté jamais confirmés par le relais du monde, remis comme avant"
            );
        }
        for (pos, old) in expired {
            if self.retry_at_root(pos) {
                log::info!("construction : pas de réponse du relais de région pour {pos:?}, demandé au relais du monde");
                continue;
            }
            self.pending.remove(&pos);
            log::info!("construction : pas de réponse du relais pour {pos:?}, annulé");
            dirty.extend(world.set_remote(pos[0], pos[1], pos[2], old));
        }
        // Relais de région en vue : connexions, messages reçus.
        let before: Vec<u64> = self.shards.conns.values().map(|c| c.serial).collect();
        self.shards.keep_near(chunk);
        let mut received = Vec::new();
        let mut broken = Vec::new();
        for (region, c) in &mut self.shards.conns {
            while let Some(e) = c.client.poll() {
                match e {
                    Event::Link(l) => c.link = Some(l),
                    Event::FromZone { chunk, data, .. } => received.push((c.serial, chunk, data)),
                    Event::Paused(_) => self.relayed_beat = true,
                    Event::Error(m) | Event::Offline(m) => {
                        log::info!("relais de la région {region:?} : {m}");
                        broken.push(*region);
                    }
                    _ => {}
                }
            }
            // Chaque relais doit savoir où l'on est (qui doit recevoir quoi).
            if c.link.is_some() && c.sent_chunk != Some(chunk) {
                c.sent_chunk = Some(chunk);
                if let Some(l) = &c.link {
                    transport::push(l, &Request::Position { chunk });
                }
            }
        }
        for r in broken {
            // La liste sera remise à jour par le relais du monde ; en attendant, on passe
            // par un autre relais de la région s'il y en a, sinon par le relais du monde.
            if let Some(c) = self.shards.conns.remove(&r) {
                if let Some(l) = self.shards.list.get_mut(&r) {
                    l.retain(|i| *i != c.info);
                }
            }
            self.shards.list.retain(|_, l| !l.is_empty());
        }
        // Un relais de région quitté : ce qu'il a dit de l'ordre des modifications est
        // oublié (un relais malveillant ne laisse rien derrière lui : la suite, venue
        // d'ailleurs, s'applique normalement).
        for s in before {
            if !self.shards.conns.values().any(|c| c.serial == s) {
                self.forget_shard(s);
            }
        }
        // Le relais du monde sait quelles régions un relais de région nous sert (il ne
        // nous en renvoie pas les blocs) : à chaque changement.
        let mut served: Vec<[i32; 2]> = self
            .shards
            .conns
            .iter()
            .filter(|(_, c)| c.link.is_some() && c.sent_chunk.is_some())
            .map(|(r, _)| *r)
            .collect();
        served.sort_unstable();
        if served != self.served_sent {
            self.served_sent = served.clone();
            self.send(Request::Served { regions: served });
        }
        for (serial, c, data) in received {
            self.from_shard = Some(serial);
            dirty.extend(self.receive(0, c, &data, world));
        }
        self.from_shard = None;
        self.pos_timer -= dt;
        self.spot_timer -= dt;
        if self.pos_timer <= 0.0 {
            self.pos_timer = pos_every(self.others.len());
            let msg = BuildMsg::Player {
                pseudo: self.pseudo.clone(),
                pos: feet.to_array(),
                yaw: look.0,
                pitch: look.1,
                crouch,
                speed,
            };
            self.send_at(
                chunk,
                Request::Nearby {
                    chunk,
                    data: game::encode(&msg),
                },
            );
            // Positions parties au relais de région : le relais du monde n'en sait plus
            // rien ; il garde tout de même où l'on est pour notre retour (toutes les 10 s).
            if self.spot_timer <= 0.0 && self.shards.link(proto::region_of(chunk)).is_some() {
                self.spot_timer = SPOT_EVERY;
                self.send(Request::Spot {
                    pos: feet.to_array(),
                    yaw: look.0,
                    pitch: look.1,
                });
            }
        }
        for o in self.others.values_mut() {
            o.seen += dt;
            // Rejoint la dernière position reçue en un intervalle : vitesse régulière même
            // pour un joueur lointain (moins de messages).
            let k = (dt / o.interval.max(POS_FASTEST)).min(1.0);
            o.pos += (o.target - o.pos) * k;
        }
        self.others
            .retain(|_, o| o.seen < STALE_S.max(o.interval * 3.0));
        (caps, dirty)
    }

    /// Un relais de région a redit le battement du relais du monde depuis le dernier appel
    /// (le relais du monde est en vie : rien à chercher chez son relais de secours).
    pub fn take_relayed_beat(&mut self) -> bool {
        std::mem::take(&mut self.relayed_beat)
    }

    /// Zones gagnées ou perdues (annoncé par le relais).
    pub fn zones_changed(&mut self, gained: &[[i32; 2]], lost: &[[i32; 2]]) -> Option<String> {
        for z in lost {
            self.zones.remove(z);
        }
        for z in gained {
            self.zones.insert(*z);
        }
        log::info!("garant : {} zone(s)", self.zones.len());
        None
    }

    /// Message d'un autre joueur ou d'un garant. Renvoie les chunks à remailler.
    pub fn receive(
        &mut self,
        peer: u64,
        _chunk: [i32; 2],
        data: &[u8],
        world: &mut World,
    ) -> Vec<ChunkPos> {
        let Some(msg) = game::decode::<BuildMsg>(data) else {
            return Vec::new();
        };
        match msg {
            // Les demandes ne vont qu'aux relais.
            BuildMsg::Edit { .. } | BuildMsg::Signed(_) => Vec::new(),
            BuildMsg::Set {
                pos, block, seq, ..
            } => self.block_set(pos, block, seq, world),
            // Blocs regroupés : comme autant de modifications.
            BuildMsg::Cells(packet) => {
                let mut dirty = Vec::new();
                for (pos, block, seq) in
                    minelib_net::cells::unpack_cells(&packet).unwrap_or_default()
                {
                    for c in self.block_set(pos, block, seq, world) {
                        if !dirty.contains(&c) {
                            dirty.push(c);
                        }
                    }
                }
                dirty
            }
            // Objets au sol et coffres : états publiés par le relais du monde (jamais par un
            // joueur, `peer` 0, ni par un relais de région).
            BuildMsg::Item { .. } | BuildMsg::Slot { .. } | BuildMsg::Denied { .. } => {
                if peer == 0 && self.from_shard.is_none() {
                    self.item_inbox.push(msg);
                }
                Vec::new()
            }
            // Les demandes ne vont qu'au relais.
            BuildMsg::DropItem { .. }
            | BuildMsg::TakeItem { .. }
            | BuildMsg::ChestPut { .. }
            | BuildMsg::ChestTake { .. } => Vec::new(),
            BuildMsg::Reject { pos, .. } => {
                // Refusé par le relais d'une région souveraine (son hôte principal vient de
                // changer, ou il ne l'est plus) : la demande repart au relais du monde.
                if self.from_shard.is_some() && self.retry_at_root(pos) {
                    return Vec::new();
                }
                // Refusé : le bloc d'avant revient.
                match self.pending.remove(&pos) {
                    // (Sauf s'il a déjà rechangé la case : voir `rebase_journal`.)
                    Some(p) if rebase_journal(world, pos, p.old) => Vec::new(),
                    Some(p) => world.set_remote(pos[0], pos[1], pos[2], p.old),
                    None => Vec::new(),
                }
            }
            BuildMsg::Player {
                pseudo,
                pos,
                yaw,
                pitch,
                crouch,
                speed,
            } => {
                let pose = minelib_net::poses::Pose {
                    pos,
                    yaw,
                    pitch,
                    crouch,
                    speed,
                };
                // Numéro sur le relais du monde (le vocal passe par lui).
                self.player_seen((peer != 0).then_some(peer), &pseudo, pose);
                Vec::new()
            }
            // Positions regroupées : comme autant de `Player`. Le pseudo n'est dit que de
            // temps en temps ; entre-temps, le numéro du joueur sur ce relais suffit.
            BuildMsg::Poses(packet) => {
                let from = self.from_shard;
                for (id, name, pose) in
                    minelib_net::poses::unpack_poses(&packet).unwrap_or_default()
                {
                    if let Some(n) = name {
                        if self.names.len() >= NAMES_MAX {
                            self.names.clear();
                        }
                        self.names.insert((from, id), n);
                    }
                    let Some(pseudo) = self.names.get(&(from, id)).cloned() else {
                        continue;
                    };
                    // (Le numéro n'est celui du relais du monde que s'il vient de lui.)
                    self.player_seen(from.is_none().then_some(id), &pseudo, pose);
                }
                Vec::new()
            }
        }
    }

    /// Un autre joueur est vu à cette position (`home` : son numéro sur le relais du monde,
    /// si c'est lui qui le dit).
    fn player_seen(&mut self, home: Option<u64>, pseudo: &str, p: minelib_net::poses::Pose) {
        // Valeurs d'un autre joueur : rien d'infini ni d'absurde n'entre dans le jeu.
        if !p.sane() {
            return;
        }
        let target = Vec3::from(p.pos);
        let key: String = pseudo.chars().take(16).collect();
        match self.others.get_mut(&key) {
            Some(o) => {
                if home.is_some() {
                    o.id = home;
                }
                // Intervalle mesuré (lissé) ; téléportation : on y va d'un coup.
                o.interval = o.interval * 0.7 + o.seen.clamp(POS_FASTEST, 4.0) * 0.3;
                if (target - o.pos).length() > 8.0 {
                    o.pos = target;
                }
                o.target = target;
                o.yaw = p.yaw;
                o.pitch = p.pitch;
                o.crouch = p.crouch;
                o.speed = p.speed;
                o.seen = 0.0;
            }
            None => {
                self.others.insert(
                    key.clone(),
                    Other {
                        pseudo: key,
                        id: home,
                        pos: target,
                        target,
                        interval: POS_EVERY,
                        yaw: p.yaw,
                        pitch: p.pitch,
                        crouch: p.crouch,
                        speed: p.speed,
                        seen: 0.0,
                    },
                );
            }
        }
    }

    /// Le relais de cette région n'est plus le nôtre : ce qu'il a dit de l'ordre des
    /// modifications est oublié.
    fn forget_shard(&mut self, serial: u64) {
        self.seen.retain(|_, v| v.1 != Some(serial));
        self.names.retain(|k, _| k.0 != Some(serial));
        // Ce qu'il a remis sans que le relais du monde le confirme : en doute. Le relais du
        // monde renvoie tout ce qui manque de la région (`Request::Served`) ; ce qu'il ne
        // confirme pas (un bloc inventé par un hôte malveillant, montré à ses seuls
        // joueurs) revient comme avant.
        for (pos, h) in self.heard.remove(&serial).unwrap_or_default() {
            self.doubtful.insert(pos, (h, DOUBT_WAIT));
        }
    }

    /// Le relais du monde publie `pos` au rang `seq` : ce qu'un relais de région en avait
    /// remis, à ce rang ou avant, est confirmé (ou dépassé).
    fn settle(&mut self, pos: [i32; 3], seq: u64) {
        if self.doubtful.get(&pos).is_some_and(|h| h.0.seq <= seq) {
            self.doubtful.remove(&pos);
        }
        for m in self.heard.values_mut() {
            if m.get(&pos).is_some_and(|h| h.seq <= seq) {
                m.remove(&pos);
            }
        }
    }

    /// Un relais de région remet `block` en `pos` : gardé, avec le bloc d'avant, jusqu'à ce
    /// que le relais du monde le confirme.
    fn note_heard(&mut self, serial: u64, pos: [i32; 3], block: u16, seq: u64, world: &World) {
        // (Le bloc d'avant d'origine : celui d'avant une modification en attente du joueur,
        // ou d'avant un relais déjà quitté.)
        let pre = match self.doubtful.remove(&pos) {
            Some((h, _)) => h.pre,
            None => self
                .pending
                .get(&pos)
                .map_or_else(|| world.get(pos[0], pos[1], pos[2]), |p| p.old),
        };
        // (La confirmation de sa propre modification signée.)
        let mine = self
            .pending
            .get(&pos)
            .and_then(|p| p.signed)
            .filter(|s| s.0 == block)
            .map(|s| s.1);
        let m = self.heard.entry(serial).or_default();
        let full = m.len() >= HEARD_MAX;
        match m.get_mut(&pos) {
            Some(h) => {
                h.got = BlockId(block);
                h.seq = seq;
                h.mine = mine;
            }
            None if !full => {
                m.insert(
                    pos,
                    Heard {
                        pre,
                        got: BlockId(block),
                        seq,
                        mine,
                    },
                );
            }
            None => {}
        }
    }

    /// Le relais du monde a publié `block` en `pos`, au rang `seq` de l'ordre du monde.
    /// Renvoie les chunks à remailler.
    fn block_set(
        &mut self,
        pos: [i32; 3],
        block: u16,
        seq: u64,
        world: &mut World,
    ) -> Vec<ChunkPos> {
        if block as usize >= minelib_world::registry::registry().len() {
            return Vec::new();
        }
        if self.from_shard.is_none() {
            self.settle(pos, seq);
        }
        // L'ordre du relais du monde : plus ancienne que ce qu'on a déjà pour cette case
        // (doublon, même modification arrivée par deux chemins), elle est sans effet.
        if self.seen.get(&pos).is_some_and(|s| s.0 >= seq) {
            return Vec::new();
        }
        if self.seen.len() >= SEEN_MAX {
            self.seen.clear();
        }
        self.seen.insert(pos, (seq, self.from_shard));
        if let Some(s) = self.from_shard {
            self.note_heard(s, pos, block, seq, world);
        }
        // Ce que publie le relais fait foi (et confirme une modification en attente).
        self.confirm(pos, BlockId(block));
        // Le joueur a déjà rechangé cette case (pas encore envoyé) : sa nouvelle
        // modification reste à l'écran, et part de ce bloc-ci.
        if rebase_journal(world, pos, BlockId(block)) {
            return Vec::new();
        }
        world.set_remote(pos[0], pos[1], pos[2], BlockId(block))
    }

    /// Une modification de cette case est confirmée.
    /// Le relais vient de publier `block` en `pos` : c'est désormais le bloc qui revient
    /// si une modification encore en attente à cet endroit est refusée (et non celui
    /// d'avant la première : deux modifications de suite dont la seconde est refusée, ou
    /// celle d'un autre joueur passée entre-temps, laissaient un monde différent).
    fn confirm(&mut self, pos: [i32; 3], block: BlockId) {
        if let Some(p) = self.pending.get_mut(&pos) {
            p.old = block;
            p.count = p.count.saturating_sub(1);
            if p.count == 0 {
                self.pending.remove(&pos);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use minelib_net::client::{Client, Cmd, Event};
    use std::time::{Duration, Instant};

    /// Un joueur simulé : sa connexion, son monde, ce qu'il a changé.
    struct Bot {
        name: String,
        c: Client,
        net: Option<BuildNet>,
        link: Option<Sender<Inbound>>,
        world: World,
        pos: Vec3,
        rejects: u32,
        /// Relais de région hébergé (connexion de garant).
        shard: Option<crate::garant_worker::ShardHost>,
        /// A rejoint au moins un relais de région.
        used_shard: bool,
        /// Clé du compte (reçue après la preuve du mot de passe).
        identity: Option<minelib_net::ed25519_dalek::SigningKey>,
        /// Relais de secours tenu par ce joueur (connexion de garant).
        standby: Option<crate::garant_worker::StandbyHost>,
        /// Où est le relais de secours de son monde ; reprise en cours (prochain essai) ;
        /// reprises réussies.
        standby_at: Option<minelib_net::proto::StandbyAt>,
        retake: Option<Instant>,
        taken: u32,
        /// Témoin de mondes (connexion de garant).
        witnesses: HashMap<u64, crate::garant_worker::WitnessRole>,
        /// Monde en pause : connexion d'essai au relais de secours (sans quitter le relais
        /// du monde), prochain essai ; pauses vues.
        probe: Option<Client>,
        probe_next: Option<Instant>,
        pauses: u32,
        /// Mondes pour lesquels il a voté la reprise (il n'y redevient jamais gardien).
        voted: crate::garant_worker::VoteBook,
        /// Connexion de garant entrée par certificat chez un relais repris : quand ce
        /// relais tombe, elle n'a rien à faire (pas d'erreur fatale).
        follower: bool,
        /// Dernier certificat de la clé du compte reçu (comme `Accounts` : il survit à une
        /// reprise, le billet des relais de région non).
        cert: Option<minelib_net::sovereign::Cert>,
        /// Lots d'autres régions vérifiés pour le relais du monde.
        audits: u32,
    }

    /// Chunks du terrain d'essai : 2 zones de part et d'autre d'une frontière.
    const CHUNKS: std::ops::Range<i32> = 2..6;

    fn flat() -> World {
        let mut w = World::new(0);
        for cx in CHUNKS {
            for cz in 0..2 {
                w.insert_empty(ChunkPos { x: cx, z: cz });
            }
        }
        for x in CHUNKS.start * 16..CHUNKS.end * 16 {
            for z in 0..32 {
                w.set(x, 10, z, BlockId::STONE);
            }
        }
        w.journal = Some(Vec::new());
        w
    }

    impl Bot {
        fn new(addr: &str, key: [u8; 32], name: &str, garant: bool) -> Self {
            let c = if garant {
                Client::spawn_garant(addr.into(), key)
            } else {
                Client::spawn(addr.into(), Some(key))
            };
            if garant {
                c.send(Cmd::Login {
                    pseudo: name.into(),
                    password: "motdepasse".into(),
                    totp: None,
                    backup: None,
                    device: None,
                });
            } else {
                c.send(Cmd::Register {
                    pseudo: name.into(),
                    password: "motdepasse".into(),
                });
            }
            let mut b = Bot {
                name: name.into(),
                c,
                net: None,
                link: None,
                world: if garant { World::new(0) } else { flat() },
                pos: Vec3::new(64.0, 11.0, 16.0),
                rejects: 0,
                shard: None,
                used_shard: false,
                identity: None,
                standby: None,
                standby_at: None,
                retake: None,
                taken: 0,
                witnesses: HashMap::new(),
                probe: None,
                probe_next: None,
                pauses: 0,
                voted: crate::garant_worker::VoteBook::open(None),
                follower: false,
                cert: None,
                audits: 0,
            };
            b.wait(|e| matches!(e, Event::LoggedIn { .. }));
            b
        }

        /// La connexion de garant d'un joueur suit son monde chez le relais qui l'a repris,
        /// avec le certificat de la clé de son compte (comme `garant_worker`).
        fn cert_garant(
            at: &minelib_net::proto::StandbyAt,
            name: &str,
            cert: minelib_net::sovereign::Cert,
            key: minelib_net::ed25519_dalek::SigningKey,
        ) -> Self {
            let c = Client::spawn_garant(at.addr.clone(), at.key);
            c.send(Cmd::GarantLogin { cert, key });
            let mut b = Bot {
                name: name.into(),
                c,
                net: None,
                link: None,
                world: World::new(0),
                pos: Vec3::new(64.0, 11.0, 16.0),
                rejects: 0,
                shard: None,
                used_shard: false,
                identity: None,
                standby: None,
                standby_at: None,
                retake: None,
                taken: 0,
                witnesses: HashMap::new(),
                probe: None,
                probe_next: None,
                pauses: 0,
                voted: crate::garant_worker::VoteBook::open(None),
                follower: true,
                cert: None,
                audits: 0,
            };
            b.wait(|e| matches!(e, Event::LoggedIn { .. }));
            b.c.send(Cmd::GarantAvailable(true));
            b
        }

        /// Connexion au relais de secours qui demande à y reprendre la partie.
        fn takeover_client(&self) -> Option<Client> {
            let (Some(at), Some(key)) = (self.standby_at.clone(), self.identity.clone()) else {
                return None;
            };
            let cert = self.cert.clone()?;
            let c = Client::spawn(at.addr.clone(), Some(at.key));
            c.send(Cmd::Takeover {
                cert,
                key,
                expect: Some(at),
            });
            Some(c)
        }

        /// Le relais du monde est tombé : nouvelle connexion au relais de secours, avec le
        /// certificat de la clé du compte (comme le jeu, voir `Accounts::start_takeover`).
        fn take_over(&mut self) {
            let Some(c) = self.takeover_client() else {
                return;
            };
            self.c = c;
            self.probe = None;
            self.retake = Some(Instant::now() + Duration::from_millis(1500));
        }

        /// Traite les événements reçus ; renvoie vrai si `want` est arrivé.
        fn pump(&mut self, want: &dyn Fn(&Event) -> bool) -> bool {
            let mut hit = false;
            // Monde en pause : on cherche le relais de secours sans quitter le relais du
            // monde (comme le jeu, voir `Accounts`) ; dès qu'il a repris, on y passe.
            let paused = self.net.as_ref().is_some_and(|n| n.is_paused());
            if !paused || self.retake.is_some() {
                self.probe = None;
                self.probe_next = None;
            } else if self.probe.is_none() && self.probe_next.is_none_or(|t| Instant::now() >= t) {
                self.probe = self.takeover_client();
                self.probe_next = Some(Instant::now() + Duration::from_millis(250));
            }
            if let Some(p) = &self.probe {
                let mut got = Vec::new();
                while let Some(e) = p.poll() {
                    got.push(e);
                }
                if got.iter().any(|e| matches!(e, Event::TakenOver { .. })) {
                    self.c = self.probe.take().expect("essai");
                    for e in got {
                        hit |= want(&e);
                        self.event(e);
                    }
                } else if got.iter().any(|e| {
                    matches!(
                        e,
                        Event::Error(_) | Event::Offline(_) | Event::Lost | Event::LoggedOut
                    )
                }) {
                    self.probe = None;
                }
            }
            while let Some(e) = self.c.poll() {
                hit |= want(&e);
                if self.event(e) {
                    break;
                }
            }
            if let Some(s) = &mut self.standby {
                s.tick();
                if s.replaced() {
                    self.standby = None;
                }
            }
            let key = self.identity.clone();
            self.witnesses.retain(|_, w| w.pump(key.as_ref()));
            for w in self.witnesses.values().filter(|w| w.voted()) {
                self.voted.add(w.root(), w.room);
            }
            if self.retake.is_some_and(|t| Instant::now() >= t) {
                self.take_over();
            }
            hit
        }

        /// Un événement ; vrai si la connexion vient d'être remplacée (la suite de
        /// l'ancienne ne compte plus).
        fn event(&mut self, e: Event) -> bool {
            match e {
                Event::Link(l) => self.link = Some(l),
                Event::BuildGarant(_) => {
                    let link = self.link.clone().expect("file d'envoi");
                    let mut n = BuildNet::new(link, self.name.clone(), false);
                    n.set_identity(self.identity.clone());
                    self.net = Some(n);
                }
                Event::ShardTask {
                    room,
                    region,
                    secret,
                    slot,
                    root,
                } => {
                    let link = self.link.clone().expect("file d'envoi");
                    self.shard = crate::garant_worker::ShardHost::start(
                        room,
                        region,
                        secret,
                        link,
                        false,
                        slot,
                        root,
                        self.identity.clone(),
                    );
                }
                Event::ShardPeers { list, sov, .. } => {
                    if let Some(s) = &self.shard {
                        s.set_peers(&list, sov);
                    }
                }
                Event::RootEdits { edits, .. } => {
                    if let Some(s) = &self.shard {
                        s.root_edits(edits);
                    }
                }
                // (Comme `garant_worker` : vérifie les lots d'autres régions qu'on lui
                // confie, redit le battement du relais du monde à ses joueurs.)
                Event::Audit { .. } => {
                    if let (Some(r), Some(l)) = (crate::garant_worker::audit_reply(&e), &self.link)
                    {
                        transport::push(l, &r);
                        self.audits += 1;
                    }
                }
                Event::WorldBeat { room, paused } => {
                    if let Some(s) = self.shard.as_ref().filter(|s| s.room == room) {
                        s.world_beat(paused);
                    }
                }
                Event::Identity(key) => {
                    if let Some(n) = &mut self.net {
                        n.set_identity(Some(key.clone()));
                    }
                    if let Some(s) = &self.shard {
                        s.set_signer(key.clone());
                    }
                    self.identity = Some(key);
                }
                Event::Shards(list) => {
                    if let Some(n) = &mut self.net {
                        n.set_shards(list);
                    }
                }
                Event::ShardTicket {
                    room,
                    pseudo,
                    expires,
                    mac,
                    cert,
                } => {
                    if cert.is_some() {
                        self.cert = cert.clone();
                    }
                    if let Some(n) = &mut self.net {
                        n.set_ticket(room, pseudo, expires, mac, cert);
                    }
                }
                Event::RoomClosed => {
                    self.shard = None;
                    if self.standby.as_ref().is_some_and(|s| !s.taken) {
                        self.standby = None;
                    }
                }
                Event::Zones { gained, lost } => {
                    if let Some(n) = &mut self.net {
                        n.zones_changed(&gained, &lost);
                    }
                }
                Event::FromZone { peer, chunk, data } => {
                    if matches!(
                        game::decode::<BuildMsg>(&data),
                        Some(BuildMsg::Reject { .. })
                    ) {
                        self.rejects += 1;
                    }
                    if let Some(n) = &mut self.net {
                        n.receive(peer, chunk, &data, &mut self.world);
                    }
                    // Hôte d'un relais de région : ce que publie le relais du monde
                    // part à ses joueurs (comme dans `garant_worker`).
                    if let Some(s) = &self.shard {
                        s.seed(&data);
                    }
                }
                Event::Promoted { .. } => {
                    if let Some(n) = &mut self.net {
                        n.set_owner();
                    }
                }
                // (Comme `garant_worker` : un relais de secours qui sert un monde repris
                // garde sa place ; jamais témoin d'un monde où il a voté.)
                Event::StandbyTask { .. } if self.standby.as_ref().is_some_and(|s| s.taken) => {
                    self.c.send(Cmd::Leave);
                }
                Event::StandbyTask { room, root, info } => {
                    let link = self.link.clone().expect("file d'envoi");
                    self.standby = crate::garant_worker::StandbyHost::start(
                        room, root, info, &link, false, &self.name,
                    );
                }
                // Gardien : battement du relais du monde (comme dans `garant_worker`).
                Event::Beat {
                    t,
                    committee,
                    standby: at,
                    paused,
                } => {
                    let room = committee.room;
                    let mut leases = Vec::new();
                    if let Some(s) = self.standby.as_ref().filter(|s| s.room == room) {
                        leases.extend(s.beat(t, &committee, paused));
                    }
                    if let Some(w) = self.witnesses.get_mut(&room) {
                        leases.extend(w.beat(t, &committee, at));
                    }
                    if let Some(l) = &self.link {
                        for r in leases {
                            transport::push(l, &r);
                        }
                    }
                }
                Event::WitnessTask { room, root, .. }
                    if self.voted.contains(&root, room) || self.witnesses.contains_key(&room) => {}
                Event::WitnessTask { room, root, cert } => {
                    if let Some(w) =
                        crate::garant_worker::WitnessRole::new(room, &self.name, root, cert, None)
                    {
                        self.witnesses.insert(room, w);
                    }
                }
                Event::Released { room, sig } => {
                    if let Some(s) = &mut self.standby {
                        s.released(room, &sig);
                    }
                }
                Event::Paused(p) => {
                    if let Some(n) = &mut self.net {
                        if n.set_paused(p) && p {
                            self.pauses += 1;
                        }
                    }
                }
                Event::Replica(ref r) => {
                    if let Some(s) = &self.standby {
                        s.feed(r);
                    }
                }
                Event::Standby(at) => self.standby_at = at,
                Event::TakenOver { host, .. } => {
                    self.retake = None;
                    self.taken += 1;
                    if let Some(n) = &mut self.net {
                        n.set_link(self.link.clone().expect("file d'envoi"));
                        if host.eq_ignore_ascii_case(&self.name) {
                            n.set_owner();
                        } else {
                            n.clear_owner();
                        }
                    }
                }
                // Relais du monde tombé : son relais de secours le reprendra (avec les votes
                // des gardiens), et les joueurs y reprennent la partie.
                Event::Lost if self.standby.is_some() || !self.witnesses.is_empty() => {}
                Event::Lost if self.standby_at.is_some() && self.net.is_some() => {
                    if self.retake.is_none() {
                        self.take_over();
                    }
                    return true;
                }
                // (La suite de l'ancienne connexion, ou le relais de secours qui n'a
                // pas encore repris le monde : nouvel essai bientôt.)
                Event::Error(_) | Event::Offline(_) | Event::LoggedOut
                    if self.standby.is_some()
                        || self.retake.is_some()
                        || self.follower
                        || !self.witnesses.is_empty() =>
                {
                    if self.retake.is_some() {
                        self.retake = Some(Instant::now() + Duration::from_millis(250));
                    }
                }
                Event::Error(m) => panic!("{} : {m}", self.name),
                _ => {}
            }
            false
        }

        fn wait(&mut self, want: impl Fn(&Event) -> bool) {
            let t = Instant::now();
            while !self.pump(&want) {
                assert!(
                    t.elapsed() < Duration::from_secs(20),
                    "{} : délai",
                    self.name
                );
                std::thread::sleep(Duration::from_millis(5));
            }
        }

        fn start_net(&mut self, owner: bool) {
            let link = self.link.clone().expect("file d'envoi");
            let mut n = BuildNet::new(link, self.name.clone(), owner);
            n.set_identity(self.identity.clone());
            self.net = Some(n);
        }

        fn tick(&mut self, dt: f32) {
            self.pump(&|_| false);
            if let Some(s) = &self.shard {
                s.pump();
            }
            if let Some(n) = &mut self.net {
                let _ = n.update(&mut self.world, self.pos, (0.0, 0.0), 0.0, 4.0, 144.0, dt);
                self.used_shard |= n.shard_links() > 0;
            }
        }
    }

    /// Ce qui se passe pendant un essai.
    #[derive(Default)]
    struct Scenario {
        players: usize,
        /// L'hôte du relais de région s'en va au milieu (les joueurs repassent par le
        /// relais du monde, ou par l'autre relais de la région).
        garant_leaves: bool,
        /// Le propriétaire s'en va au milieu (un joueur reprend le monde et l'enregistre).
        owner_leaves: bool,
        /// La région est assez peuplée pour avoir son propre relais (hébergé par le joueur
        /// d'un autre monde).
        shards: bool,
        /// La région a deux relais (chez deux hôtes), reliés entre eux : elle devient
        /// souveraine (l'un met ses modifications en ordre, l'autre vérifie).
        two_relays: bool,
        /// Trois relais : deux hôtes confirment ce que publie l'hôte principal.
        three_relays: bool,
        /// Son hôte principal triche une fois : il ajoute une modification inventée au
        /// nom d'un joueur et la montre aux siens. Il doit être pris, écarté et remplacé,
        /// et les mondes rester identiques (le bloc inventé défait chez ceux qui l'ont vu).
        forger: bool,
    }

    /// Construction en ligne à petite échelle, de bout en bout (vrai relais, vraies
    /// connexions) : des joueurs qui se déplacent et construisent à cheval sur deux zones,
    /// le joueur d'un autre monde qui héberge le relais de la région quand elle en a un,
    /// un joueur qui inonde (refusé au-delà de 20 par seconde), un joueur qui arrive après
    /// coup. À la fin, tous les mondes sont identiques à celui du propriétaire, case par
    /// case, et rien n'est en attente.
    fn run(sc: Scenario) {
        let dir = std::env::temp_dir().join(format!("minelib-sync-{}", rand_u32()));
        minelib_relay::test_seed_games(&dir, "Garant", 5);
        minelib_relay::test_seed_games(&dir, "Garant2", 5);
        minelib_relay::test_seed_games(&dir, "Garant3", 5);
        let h = minelib_relay::start(minelib_relay::Config {
            dir: dir.clone(),
            bind: "127.0.0.1:0".into(),
            pow_bits: 4,
            name: "essai".into(),
        })
        .unwrap();
        if sc.shards {
            h.set_shard_from(2);
        }
        // Relais de la région : 1 + (joueurs - 2) / part.
        let relays = if sc.three_relays {
            h.set_shard_share(3);
            3
        } else if sc.two_relays {
            h.set_shard_share(4);
            2
        } else {
            1
        };
        let addr = h.addr.to_string();
        // Un autre monde, dont le joueur prête sa connexion de garant.
        let mut ailleurs = Bot::new(&addr, h.public, "Garant", false);
        ailleurs.c.send(Cmd::Host {
            kind: 1,
            name: "Autre".into(),
            map: "autre".into(),
            map_sig: [8; 32],
            max_players: 0,
        });
        ailleurs.wait(|e| matches!(e, Event::Hosted(_)));
        let mut garant = Bot::new(&addr, h.public, "Garant", true);
        garant.c.send(Cmd::GarantAvailable(true));
        // D'autres hôtes possibles (joueurs d'autres mondes).
        let mut extra: Vec<(Bot, Bot)> = (2..relays + 1)
            .map(|k| {
                let name = format!("Garant{k}");
                let mut w = Bot::new(&addr, h.public, &name, false);
                w.c.send(Cmd::Host {
                    kind: 1,
                    name: format!("Encore{k}"),
                    map: format!("encore{k}"),
                    map_sig: [7 + k as u8; 32],
                    max_players: 0,
                });
                w.wait(|e| matches!(e, Event::Hosted(_)));
                let g = Bot::new(&addr, h.public, &name, true);
                g.c.send(Cmd::GarantAvailable(true));
                (w, g)
            })
            .collect();
        let (mut linked, mut both_used, mut left) = (false, false, false);
        // Modifications mises en ordre par un hôte principal ; triche faite.
        let (mut ordered, mut forged) = (0u64, false);
        // Des blocs sont arrivés à un joueur par un relais de région.
        let mut via_shard = false;
        // Le monde essayé : un propriétaire et des joueurs.
        let mut bots = vec![Bot::new(&addr, h.public, "Proprio", false)];
        bots[0].c.send(Cmd::Host {
            kind: 1,
            name: "Essai".into(),
            map: "essai".into(),
            map_sig: [7; 32],
            max_players: 0,
        });
        let mut room = None;
        let t = Instant::now();
        while room.is_none() {
            while let Some(e) = bots[0].c.poll() {
                match e {
                    Event::Hosted(r) => room = Some(r),
                    Event::Link(l) => bots[0].link = Some(l),
                    _ => {}
                }
            }
            assert!(t.elapsed() < Duration::from_secs(20));
            std::thread::sleep(Duration::from_millis(5));
        }
        let room = room.unwrap();
        bots[0].start_net(true);
        for i in 1..sc.players {
            let mut b = Bot::new(&addr, h.public, &format!("Joueur{i}"), false);
            b.c.send(Cmd::Join(room));
            b.wait(|e| matches!(e, Event::Joined { .. }));
            b.start_net(false);
            bots.push(b);
        }
        let mut seed = 12345u32;
        let mut rnd = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        let dt = 0.02;
        let mut edited = std::collections::BTreeSet::new();
        let mut had_garant = false;
        let mut gone: Vec<usize> = Vec::new();
        // 6 secondes de jeu : on bouge, on construit des deux côtés de la frontière.
        for step in 0..300 {
            // (Une fois les relais de la région reliés : sur une machine chargée, ils
            // peuvent mettre plus de 3 s.)
            if step >= 150 && sc.garant_leaves && !left && (linked || relays == 1 || step >= 250) {
                left = true;
                garant.c.send(Cmd::Leave);
                garant.net = None;
                garant.shard = None;
            }
            if step == 150 && sc.owner_leaves {
                bots[0].c.send(Cmd::Leave);
                gone.push(0);
            }
            for (i, b) in bots.iter_mut().enumerate() {
                if gone.contains(&i) {
                    continue;
                }
                let r = rnd();
                b.pos.x = (b.pos.x + (r % 3) as f32 - 1.0).clamp(33.0, 95.0);
                b.pos.z = (b.pos.z + ((r >> 4) % 3) as f32 - 1.0).clamp(1.0, 30.0);
                // Un joueur inonde pendant la première seconde ; les autres ~3 blocs/s.
                let edits = if i == 3 && step < 50 {
                    2
                } else {
                    (rnd() % 17 == 0) as u32
                };
                for _ in 0..edits {
                    let x = (CHUNKS.start * 16 + (rnd() % 64) as i32).min(CHUNKS.end * 16 - 1);
                    let z = (rnd() % 32) as i32;
                    let y = 11 + (rnd() % 3) as i32;
                    let block = if rnd() % 4 == 0 {
                        BlockId::AIR
                    } else {
                        BlockId(1 + rnd() as u16 % 8)
                    };
                    b.world.set(x, y, z, block);
                    edited.insert([x, y, z]);
                }
                b.tick(dt);
            }
            garant.tick(dt);
            had_garant |= garant.shard.is_some();
            via_shard |= bots
                .iter()
                .any(|b| b.net.as_ref().is_some_and(|n| n.via_shard() > 0));
            for (w, g) in &mut extra {
                w.pump(&|_| false);
                g.tick(dt);
            }
            let hosts: Vec<_> = std::iter::once(garant.shard.as_ref())
                .chain(extra.iter().map(|(_, g)| g.shard.as_ref()))
                .collect();
            if relays > 1 && hosts.iter().all(|s| s.is_some()) {
                linked |= hosts.iter().flatten().all(|s| s.linked() == relays - 1);
                both_used |= hosts.iter().flatten().all(|s| s.players() > 0);
            }
            for sh in hosts.iter().flatten() {
                ordered = ordered.max(sh.ordered());
            }
            if sc.forger && !forged && step >= 100 {
                if let Some(sh) = hosts.iter().flatten().find(|s| s.is_primary()) {
                    sh.forge_once();
                    forged = true;
                }
            }
            ailleurs.pump(&|_| false);
            std::thread::sleep(Duration::from_millis(20));
        }
        // Un joueur arrive après coup ; puis tout se pose (confirmations, paquets du
        // propriétaire, annulations).
        let mut late = Bot::new(&addr, h.public, "Retard", false);
        late.c.send(Cmd::Join(room));
        late.wait(|e| matches!(e, Event::Joined { .. }));
        late.start_net(false);
        bots.push(late);
        for _ in 0..300 {
            for (i, b) in bots.iter_mut().enumerate() {
                if !gone.contains(&i) {
                    b.tick(dt);
                }
            }
            garant.tick(dt);
            for (w, g) in &mut extra {
                w.pump(&|_| false);
                g.tick(dt);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        if relays > 1 {
            assert!(linked, "les relais de la région se sont reliés");
            let _ = both_used;
            assert!(
                ordered > 0,
                "région souveraine : ses modifications mises en ordre par son hôte principal"
            );
            eprintln!(
                "mises en ordre (époque en cours) : {ordered}, archivées : {}",
                h.sov_committed()
            );
            if !sc.forger {
                assert!(
                    h.sov_committed() as u64 >= ordered,
                    "tout ce que l'hôte principal a mis en ordre est archivé par le relais du monde"
                );
            }
        }
        if sc.forger {
            assert!(forged, "l'hôte principal a triché");
            assert!(h.sov_faults() >= 1, "la faute est prouvée, l'hôte écarté");
            let still = std::iter::once(garant.shard.as_ref())
                .chain(extra.iter().map(|(_, g)| g.shard.as_ref()))
                .flatten()
                .count();
            assert_eq!(still, relays - 1, "l'hôte qui a triché n'héberge plus rien");
        } else {
            assert_eq!(h.sov_faults(), 0, "aucun hôte honnête n'est condamné");
        }
        assert!(
            !sc.shards || had_garant,
            "la région a reçu son relais, hébergé par le joueur d'un autre monde"
        );
        assert_eq!(
            via_shard, sc.shards,
            "les blocs passent par le relais de région quand il y en a un, et seulement là"
        );
        assert!(
            edited.len() > 50,
            "assez de modifications : {}",
            edited.len()
        );
        assert!(
            bots[3].rejects > 0,
            "l'inondation est refusée au-delà de 20/s"
        );
        let live: Vec<&Bot> = bots
            .iter()
            .enumerate()
            .filter(|(i, _)| !gone.contains(i))
            .map(|(_, b)| b)
            .collect();
        if sc.forger {
            let undone: usize = live.iter().map(|b| b.net.as_ref().unwrap().undone).sum();
            eprintln!("blocs inventés défaits : {undone}");
        }
        for b in &live {
            assert_eq!(
                b.net.as_ref().unwrap().pending(),
                0,
                "{} : en attente",
                b.name
            );
        }
        // Plusieurs relais : chacun voit quand même tous les autres (les positions passent
        // d'un relais à l'autre).
        if relays > 1 {
            for b in &live {
                let seen = b.net.as_ref().unwrap().others.len();
                assert!(
                    seen + 1 >= live.len(),
                    "{} ne voit que {seen} joueurs sur {}",
                    b.name,
                    live.len() - 1
                );
            }
        }
        // La référence : le propriétaire du moment (celui qui enregistre le monde).
        let owner = live
            .iter()
            .find(|b| b.net.as_ref().unwrap().is_owner())
            .expect("un propriétaire");
        let mut differ = Vec::new();
        // Toutes les cases où l'on construit, et au-dessus (où un hôte principal tricheur
        // invente sa modification).
        for b in &live {
            for x in CHUNKS.start * 16..CHUNKS.end * 16 {
                for z in 0..32 {
                    for y in 11..21 {
                        let (a, o) = (b.world.get(x, y, z), owner.world.get(x, y, z));
                        if a != o {
                            differ.push((b.name.clone(), [x, y, z], a, o));
                        }
                    }
                }
            }
        }
        assert!(differ.is_empty(), "mondes différents : {differ:?}");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Ce qui arrive au relais du monde pendant la partie.
    #[derive(Clone, Copy, PartialEq, Debug)]
    enum Fault {
        /// Il tombe (connexions coupées, plus rien ne répond).
        Crash,
        /// Le réseau entre lui et tous ses gardiens est coupé ; les joueurs le joignent
        /// encore. Il doit mettre le monde en pause, et le relais de secours le reprendre :
        /// jamais les deux à la fois.
        Isolated,
        /// Seul le relais de secours est coupé de lui : le témoin l'entend encore, il
        /// continue ; le relais de secours ne reprend pas le monde.
        StandbyCut,
        /// Il s'arrête proprement : le relais de secours reprend tout de suite.
        Release,
        /// Seul le témoin est coupé de lui ; le relais de secours l'entend encore (et ses
        /// baux ne comptent pas : il pourrait tricher). Il doit mettre le monde en pause, et
        /// le relais de secours le reprendre avec le vote du témoin.
        WitnessCut,
        /// Le témoin s'en va sans prévenir (il a peut-être voté) : pause sans issue, le
        /// relais du monde confie le monde au relais de secours.
        WitnessGone,
        /// Il tombe ; le monde repris se choisit son propre relais de secours et un témoin
        /// (les connexions de garant des joueurs le suivent), puis le relais repris tombe à
        /// son tour : le nouveau relais de secours reprend le monde.
        Chain,
    }

    /// Un monde de construction gardé : son relais du monde, un relais de secours (chez
    /// le joueur d'un autre monde) et un témoin (chez un troisième), quatre joueurs qui
    /// construisent sans s'arrêter. Au milieu, `fault` arrive au relais du monde. À aucun
    /// moment deux relais ne servent le monde ; quand le relais de secours reprend, tous
    /// les joueurs y reprennent la partie sans la quitter (le premier arrivé en devient le
    /// propriétaire), un joueur arrive après coup ; à la fin, tous les mondes sont
    /// identiques case par case (ce qui a été construit avant, pendant et après), et rien
    /// n'est en attente.
    fn guarded_world(fault: Fault) {
        let dir = std::env::temp_dir().join(format!("minelib-secours-{}", rand_u32()));
        minelib_relay::test_seed_games(&dir, "Garant", 5);
        minelib_relay::test_seed_games(&dir, "Garant2", 5);
        let h = minelib_relay::start(minelib_relay::Config {
            dir: dir.clone(),
            bind: "127.0.0.1:0".into(),
            pow_bits: 4,
            name: "essai".into(),
        })
        .unwrap();
        h.set_standby_local(true);
        let addr = h.addr.to_string();
        // Les joueurs de deux autres mondes, qui prêtent leur connexion de garant : le
        // premier tiendra le relais de secours, le second sera témoin.
        let mut ailleurs = Vec::new();
        let mut garants = Vec::new();
        for (name, map) in [("Garant", 8u8), ("Garant2", 9u8)] {
            let mut a = Bot::new(&addr, h.public, name, false);
            a.c.send(Cmd::Host {
                kind: 1,
                name: format!("Autre {map}"),
                map: format!("autre{map}"),
                map_sig: [map; 32],
                max_players: 0,
            });
            a.wait(|e| matches!(e, Event::Hosted(_)));
            ailleurs.push(a);
            let g = Bot::new(&addr, h.public, name, true);
            g.c.send(Cmd::GarantAvailable(true));
            garants.push(g);
            // (Le premier garant a le temps de devenir le relais de secours.)
            if garants.len() == 1 {
                let t = Instant::now();
                while t.elapsed() < Duration::from_millis(100) {
                    garants[0].pump(&|_| false);
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        }
        let mut bots = vec![Bot::new(&addr, h.public, "Proprio", false)];
        bots[0].c.send(Cmd::Host {
            kind: 1,
            name: "Essai".into(),
            map: "essai".into(),
            map_sig: [7; 32],
            max_players: 0,
        });
        let mut room = None;
        let t = Instant::now();
        while room.is_none() {
            while let Some(e) = bots[0].c.poll() {
                match e {
                    Event::Hosted(r) => room = Some(r),
                    Event::Link(l) => bots[0].link = Some(l),
                    Event::Identity(k) => bots[0].identity = Some(k),
                    _ => {}
                }
            }
            assert!(t.elapsed() < Duration::from_secs(20));
            std::thread::sleep(Duration::from_millis(5));
        }
        let room = room.unwrap();
        bots[0].start_net(true);
        for i in 1..4 {
            let mut b = Bot::new(&addr, h.public, &format!("Joueur{i}"), false);
            b.c.send(Cmd::Join(room));
            b.wait(|e| matches!(e, Event::Joined { .. }));
            b.start_net(false);
            bots.push(b);
        }
        let mut seed = 777u32;
        let mut rnd = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        let dt = 0.02;
        let mut edited = std::collections::BTreeSet::new();
        let mut after = 0;
        let mut fault_at: Option<Instant> = None;
        let mut fault_step = None;
        let mut resumed_after = None;
        let mut frozen_seen = false;
        for step in 0..650 {
            // La panne, une fois le relais de secours prêt chez tous, la copie faite, et
            // les gardiens au complet (relais de secours et témoin).
            let guarded = h
                .committee(room)
                .is_some_and(|c| c.standby.is_some() && !c.witnesses.is_empty());
            if fault_at.is_none()
                && step >= 150
                && guarded
                && bots.iter().all(|b| b.standby_at.is_some())
            {
                match fault {
                    Fault::Crash | Fault::Chain => h.kill(),
                    Fault::Isolated => h.guard_isolate(true),
                    Fault::StandbyCut => h.guard_cut(&["Garant"]),
                    Fault::WitnessCut => h.guard_cut(&["Garant2"]),
                    Fault::WitnessGone => {
                        garants.truncate(1);
                    }
                    Fault::Release => assert_eq!(h.release(), 1, "monde confié"),
                }
                fault_at = Some(Instant::now());
                fault_step = Some(step);
            }
            // Jamais deux relais à la fois : le relais de secours ne sert le monde que si
            // le relais du monde ne le peut plus (tombé, ou en pause pour toujours).
            let taken = garants[0].standby.as_ref().is_some_and(|s| s.taken);
            if !matches!(fault, Fault::Crash | Fault::Chain) && fault_at.is_some() {
                // (Le monde fermé là-bas, ses joueurs partis : il ne le sert plus.)
                let state = h.frozen(room);
                frozen_seen |= state == Some(true);
                assert!(
                    !taken || state != Some(false),
                    "deux relais servent le monde (étape {step}, {fault:?})"
                );
            }
            if taken && resumed_after.is_none() && bots.iter().all(|b| b.taken > 0) {
                resumed_after = fault_at.map(|t| t.elapsed());
            }
            for b in bots.iter_mut() {
                let r = rnd();
                b.pos.x = (b.pos.x + (r % 3) as f32 - 1.0).clamp(33.0, 95.0);
                b.pos.z = (b.pos.z + ((r >> 4) % 3) as f32 - 1.0).clamp(1.0, 30.0);
                // Relais du monde tombé et partie pas encore reprise : pas de relais du
                // tout, on ne construit pas. Sinon on construit, même en pause (le jeu
                // garde les modifications et les envoie à la reprise).
                let live = !matches!(fault, Fault::Crash | Fault::Chain)
                    || fault_at.is_none()
                    || b.taken > 0;
                if live && step < 520 && rnd() % 9 == 0 {
                    let x = (CHUNKS.start * 16 + (rnd() % 64) as i32).min(CHUNKS.end * 16 - 1);
                    let z = (rnd() % 32) as i32;
                    let y = 11 + (rnd() % 3) as i32;
                    let block = if rnd() % 4 == 0 {
                        BlockId::AIR
                    } else {
                        BlockId(1 + rnd() as u16 % 8)
                    };
                    b.world.set(x, y, z, block);
                    edited.insert([x, y, z]);
                    if fault_at.is_some() {
                        after += 1;
                    }
                }
                b.tick(dt);
            }
            for g in garants.iter_mut() {
                g.tick(dt);
            }
            if !matches!(fault, Fault::Crash | Fault::Chain) || fault_at.is_none() {
                for a in ailleurs.iter_mut() {
                    a.pump(&|_| false);
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            fault_at.is_some(),
            "gardiens au complet et relais de secours prêt"
        );
        assert!(after > 20, "on a construit après la panne : {after}");
        let took = garants[0].standby.as_ref().is_some_and(|s| s.taken);
        if fault == Fault::StandbyCut {
            // Le témoin entend toujours le relais du monde : il continue, sans pause ;
            // personne ne passe au relais de secours.
            assert!(!took, "le relais de secours n'a pas repris le monde");
            assert!(
                !frozen_seen,
                "le relais du monde n'a jamais mis le monde en pause"
            );
            for b in &bots {
                assert_eq!(b.taken, 0, "{} est resté sur le relais du monde", b.name);
                assert_eq!(b.pauses, 0, "{} n'a pas vu de pause", b.name);
            }
        } else {
            assert!(took, "le relais de secours a repris le monde");
            for b in &bots {
                assert_eq!(b.taken, 1, "{} a repris la partie", b.name);
            }
            let after = resumed_after.expect("tous repris");
            eprintln!(
                "{fault:?} : tous les joueurs sur le relais de secours {} ms après la panne \
                 (étape {fault_step:?})",
                after.as_millis()
            );
            if matches!(
                fault,
                Fault::Isolated | Fault::WitnessCut | Fault::WitnessGone
            ) {
                assert!(frozen_seen, "le relais du monde a mis le monde en pause");
                assert!(
                    bots.iter().all(|b| b.pauses > 0),
                    "les joueurs ont vu la pause"
                );
            }
            let h2 = garants[0].standby.as_ref().expect("relais de secours");
            let resumed = h2.resumed().expect("monde repris");
            // Un joueur arrive après coup, sur le relais de secours.
            let at = bots[0].standby_at.clone().unwrap();
            let mut late = Bot::new(&at.addr, at.key, "Retard", false);
            late.c.send(Cmd::Join(resumed));
            late.wait(|e| matches!(e, Event::Joined { .. }));
            late.start_net(false);
            bots.push(late);
        }
        if fault == Fault::Chain {
            // Les connexions de garant de trois joueurs suivent le monde chez le relais
            // repris, par le certificat de leur compte : il s'y choisit un relais de
            // secours et un témoin, et les joueurs apprennent où est le nouveau.
            let first = bots[0].standby_at.clone().expect("relais repris");
            let mut followers: Vec<Bot> = bots[1..4]
                .iter()
                .map(|b| {
                    let cert = b.cert.clone().expect("certificat");
                    let key = b.identity.clone().expect("clé du compte");
                    Bot::cert_garant(&first, &b.name, cert, key)
                })
                .collect();
            let t = Instant::now();
            let mut step = 0u32;
            loop {
                step += 1;
                for b in bots.iter_mut() {
                    // On continue de construire pendant ce temps.
                    if rnd() % 9 == 0 {
                        let x = (CHUNKS.start * 16 + (rnd() % 64) as i32).min(CHUNKS.end * 16 - 1);
                        let (z, y) = ((rnd() % 32) as i32, 11 + (rnd() % 3) as i32);
                        let block = BlockId(1 + rnd() as u16 % 8);
                        b.world.set(x, y, z, block);
                        edited.insert([x, y, z]);
                    }
                    b.tick(dt);
                }
                for g in garants.iter_mut().chain(followers.iter_mut()) {
                    g.tick(dt);
                }
                let resumed = garants[0]
                    .standby
                    .as_ref()
                    .and_then(|s| s.resumed())
                    .expect("monde repris");
                let guarded = garants[0]
                    .standby
                    .as_ref()
                    .and_then(|s| s.relay().committee(resumed))
                    .is_some_and(|c| c.standby.is_some() && !c.witnesses.is_empty());
                let moved = bots
                    .iter()
                    .all(|b| b.standby_at.as_ref().is_some_and(|a| a.key != first.key));
                if guarded && moved && step > 50 {
                    break;
                }
                assert!(
                    t.elapsed() < Duration::from_secs(40),
                    "le monde repris se choisit un relais de secours"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
            // Le relais repris tombe à son tour (chacun doit reprendre la partie une fois de
            // plus, le joueur arrivé après coup compris).
            let before: Vec<u32> = bots.iter().map(|b| b.taken).collect();
            garants[0]
                .standby
                .as_ref()
                .expect("relais repris")
                .relay()
                .kill();
            let t = Instant::now();
            while !bots.iter().zip(&before).all(|(b, n)| b.taken == n + 1) {
                for b in bots.iter_mut() {
                    b.tick(dt);
                }
                for g in followers.iter_mut() {
                    g.tick(dt);
                }
                assert!(
                    t.elapsed() < Duration::from_secs(30),
                    "tous les joueurs passent au relais de secours du monde repris : {:?}",
                    bots.iter()
                        .map(|b| (b.name.clone(), b.taken))
                        .collect::<Vec<_>>()
                );
                std::thread::sleep(Duration::from_millis(20));
            }
            eprintln!(
                "Chain : tous les joueurs sur le deuxième relais de secours {} ms après la \
                 chute du premier",
                t.elapsed().as_millis()
            );
            for _ in 0..200 {
                for b in bots.iter_mut() {
                    if rnd() % 9 == 0 {
                        let x = (CHUNKS.start * 16 + (rnd() % 64) as i32).min(CHUNKS.end * 16 - 1);
                        let (z, y) = ((rnd() % 32) as i32, 11 + (rnd() % 3) as i32);
                        let block = BlockId(1 + rnd() as u16 % 8);
                        b.world.set(x, y, z, block);
                        edited.insert([x, y, z]);
                    }
                    b.tick(dt);
                }
                for g in followers.iter_mut() {
                    g.tick(dt);
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            garants.extend(followers);
        }
        for _ in 0..250 {
            for b in bots.iter_mut() {
                b.tick(dt);
            }
            for g in garants.iter_mut() {
                g.tick(dt);
            }
            if !matches!(fault, Fault::Crash | Fault::Chain) {
                for a in ailleurs.iter_mut() {
                    a.pump(&|_| false);
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        for b in &bots {
            assert_eq!(
                b.net.as_ref().unwrap().pending(),
                0,
                "{} : en attente",
                b.name
            );
        }
        let owners: Vec<&Bot> = bots
            .iter()
            .filter(|b| b.net.as_ref().unwrap().is_owner())
            .collect();
        assert_eq!(owners.len(), 1, "un seul propriétaire");
        let owner = owners[0];
        let mut differ = Vec::new();
        for b in &bots {
            for p in &edited {
                let (a, o) = (
                    b.world.get(p[0], p[1], p[2]),
                    owner.world.get(p[0], p[1], p[2]),
                );
                if a != o {
                    differ.push((b.name.clone(), *p, a, o));
                }
            }
        }
        assert!(differ.is_empty(), "mondes différents : {differ:?}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_build_world_survives_its_relay() {
        guarded_world(Fault::Crash);
    }

    #[test]
    fn a_taken_over_world_gets_its_own_standby() {
        guarded_world(Fault::Chain);
    }

    #[test]
    fn a_witness_cut_from_the_world_relay_pauses_it_whatever_the_standby_says() {
        guarded_world(Fault::WitnessCut);
    }

    #[test]
    fn a_world_relay_stuck_in_pause_hands_over_to_its_standby() {
        guarded_world(Fault::WitnessGone);
    }

    #[test]
    fn a_world_relay_cut_from_its_guardians_pauses_and_the_standby_takes_over() {
        guarded_world(Fault::Isolated);
    }

    #[test]
    fn a_standby_cut_from_the_world_relay_alone_does_not_take_over() {
        guarded_world(Fault::StandbyCut);
    }

    #[test]
    fn a_world_relay_that_stops_cleanly_hands_over_at_once() {
        guarded_world(Fault::Release);
    }

    #[test]
    fn build_worlds_stay_identical_for_everyone() {
        run(Scenario {
            players: 6,
            ..Default::default()
        });
    }

    #[test]
    fn build_worlds_stay_identical_when_the_owner_leaves() {
        run(Scenario {
            players: 6,
            owner_leaves: true,
            ..Default::default()
        });
    }

    #[test]
    fn build_worlds_stay_identical_in_a_crowd() {
        run(Scenario {
            players: 14,
            ..Default::default()
        });
    }

    #[test]
    fn build_worlds_stay_identical_with_a_region_relay() {
        run(Scenario {
            players: 6,
            shards: true,
            ..Default::default()
        });
    }

    #[test]
    fn build_worlds_stay_identical_with_two_relays_in_a_region() {
        run(Scenario {
            players: 8,
            shards: true,
            two_relays: true,
            ..Default::default()
        });
    }

    #[test]
    fn build_worlds_stay_identical_with_three_relays_in_a_region() {
        run(Scenario {
            players: 8,
            shards: true,
            two_relays: true,
            three_relays: true,
            ..Default::default()
        });
    }

    #[test]
    fn a_cheating_primary_host_among_three_is_caught_and_replaced() {
        run(Scenario {
            players: 8,
            shards: true,
            two_relays: true,
            three_relays: true,
            forger: true,
            ..Default::default()
        });
    }

    #[test]
    fn a_cheating_primary_host_is_caught_and_replaced() {
        run(Scenario {
            players: 8,
            shards: true,
            two_relays: true,
            forger: true,
            ..Default::default()
        });
    }

    #[test]
    fn build_worlds_stay_identical_when_the_first_of_two_relays_leaves() {
        run(Scenario {
            players: 8,
            shards: true,
            two_relays: true,
            garant_leaves: true,
            ..Default::default()
        });
    }

    #[test]
    fn build_worlds_stay_identical_when_the_region_relay_leaves() {
        run(Scenario {
            players: 6,
            shards: true,
            garant_leaves: true,
            ..Default::default()
        });
    }

    #[test]
    fn far_players_move_smoothly_with_fewer_messages() {
        // Un joueur lointain n'envoie qu'une position par seconde : on le voit avancer à
        // vitesse régulière, pas par bonds.
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut b = BuildNet::new(tx, "Moi".into(), false);
        let mut w = World::new(0);
        let msg = |x: f32| {
            game::encode(&BuildMsg::Player {
                pseudo: "Loin".into(),
                pos: [x, 64.0, 0.0],
                yaw: 0.0,
                pitch: 0.0,
                crouch: 0.0,
                speed: 4.0,
            })
        };
        let dt = 0.05;
        let mut x = 0.0;
        let mut shown = Vec::new();
        for step in 0..120 {
            if step % 20 == 0 {
                b.receive(7, [0, 0], &msg(x), &mut w);
                x += 4.0;
            }
            let _ = b.update(&mut w, Vec3::ZERO, (0.0, 0.0), 0.0, 0.0, 0.0, dt);
            shown.push(b.others["Loin"].pos.x);
        }
        // Après la mise en route : jamais de bond de plus de 0,5 bloc d'une image à l'autre.
        let jumps = shown[60..]
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0, f32::max);
        assert!(jumps < 0.5, "bond de {jumps}");
        assert!(shown[119] > 15.0, "il avance : {}", shown[119]);
    }

    #[test]
    fn a_refused_edit_gives_back_what_the_relay_last_published() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut b = BuildNet::new(tx, "Moi".into(), false);
        let mut w = World::new(0);
        w.ensure_chunk(minelib_world::ChunkPos::containing(1, 1));
        w.journal = Some(Vec::new());
        let pos = [1, 100, 1];
        let before = w.get(1, 100, 1);
        let step = |b: &mut BuildNet, w: &mut World| {
            let _ = b.update(w, Vec3::ZERO, (0.0, 0.0), 0.0, 0.0, 0.0, 0.05);
        };
        let from_garant = |b: &mut BuildNet, w: &mut World, m: BuildMsg| {
            b.receive(9, [0, 0], &game::encode(&m), w);
        };
        // (Chaque modification publiée porte un rang plus grand que la précédente.)
        let rank = std::cell::Cell::new(0u64);
        let set = |block: u16| {
            rank.set(rank.get() + 1);
            BuildMsg::Set {
                pos,
                block,
                by: String::new(),
                seq: rank.get(),
            }
        };
        // Deux modifications de suite au même endroit ; le relais accepte la première et
        // refuse la seconde (trop vite) : c'est la première qui reste, pas le bloc d'avant.
        w.set(1, 100, 1, BlockId(1));
        step(&mut b, &mut w);
        w.set(1, 100, 1, BlockId(2));
        step(&mut b, &mut w);
        assert_eq!(b.pending(), 1);
        from_garant(&mut b, &mut w, set(1));
        from_garant(&mut b, &mut w, BuildMsg::Reject { pos, peer: 0 });
        assert_eq!(w.get(1, 100, 1), BlockId(1));
        assert_eq!(b.pending(), 0);
        // Ma modification attend ; celle d'un autre passe avant, puis la mienne est
        // refusée : je vois celle de l'autre.
        w.set(1, 100, 1, BlockId(3));
        step(&mut b, &mut w);
        from_garant(&mut b, &mut w, set(5));
        from_garant(&mut b, &mut w, BuildMsg::Reject { pos, peer: 0 });
        assert_eq!(w.get(1, 100, 1), BlockId(5));
        // Refus reçu alors que j'ai déjà rechangé la case (pas encore envoyé) : ma
        // nouvelle modification reste, et si elle est refusée aussi, c'est le bloc qui
        // fait foi qui revient, pas celui de ma première tentative.
        w.set(1, 100, 1, BlockId(6));
        step(&mut b, &mut w);
        w.set(1, 100, 1, BlockId(7));
        from_garant(&mut b, &mut w, BuildMsg::Reject { pos, peer: 0 });
        assert_eq!(w.get(1, 100, 1), BlockId(7));
        step(&mut b, &mut w);
        from_garant(&mut b, &mut w, BuildMsg::Reject { pos, peer: 0 });
        assert_eq!(w.get(1, 100, 1), BlockId(5));
        // Refusée sans rien d'autre entre-temps : le bloc d'avant revient.
        w.set(1, 100, 1, BlockId(4));
        step(&mut b, &mut w);
        from_garant(&mut b, &mut w, BuildMsg::Reject { pos, peer: 0 });
        assert_eq!(w.get(1, 100, 1), BlockId(5));
        assert_ne!(before, BlockId(5));
    }

    fn rand_u32() -> u32 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(1, |d| d.subsec_nanos())
    }

    /// L'ordre du relais du monde fait foi : une modification plus ancienne que celle déjà
    /// appliquée à la même case (doublon, même modification arrivée par le relais de
    /// région et par le relais du monde) est sans effet, et ne confirme rien deux fois.
    /// Ce qu'un relais de région a dit est oublié quand on le quitte.
    #[test]
    fn an_older_or_repeated_edit_changes_nothing() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut b = BuildNet::new(tx, "Moi".into(), false);
        let mut w = World::new(0);
        w.ensure_chunk(minelib_world::ChunkPos::containing(1, 1));
        w.journal = Some(Vec::new());
        let pos = [1, 100, 1];
        let set = |block: u16, seq: u64| {
            game::encode(&BuildMsg::Set {
                pos,
                block,
                by: "Eve".into(),
                seq,
            })
        };
        b.receive(0, [0, 0], &set(1, 10), &mut w);
        b.receive(0, [0, 0], &set(2, 12), &mut w);
        // La plus ancienne arrive après, par un autre chemin ; puis un doublon.
        b.receive(0, [0, 0], &set(1, 10), &mut w);
        b.receive(0, [0, 0], &set(3, 11), &mut w);
        b.receive(0, [0, 0], &set(2, 12), &mut w);
        assert_eq!(w.get(1, 100, 1), BlockId(2));
        // Deux modifications à moi en attente sur cette case : la première confirmée,
        // reçue deux fois (relais du monde, puis relais de région), ne confirme pas la
        // seconde.
        w.set(1, 100, 1, BlockId(4));
        let _ = b.update(&mut w, Vec3::ZERO, (0.0, 0.0), 0.0, 0.0, 0.0, 0.05);
        w.set(1, 100, 1, BlockId(5));
        let _ = b.update(&mut w, Vec3::ZERO, (0.0, 0.0), 0.0, 0.0, 0.0, 0.05);
        b.receive(0, [0, 0], &set(4, 13), &mut w);
        b.receive(0, [0, 0], &set(4, 13), &mut w);
        assert_eq!(b.pending(), 1, "la seconde attend toujours sa réponse");
        b.receive(0, [0, 0], &set(5, 14), &mut w);
        assert_eq!(b.pending(), 0);
        assert_eq!(w.get(1, 100, 1), BlockId(5));
        // Un relais de région (tenu par un joueur) annonce un rang énorme : tant qu'on
        // passe par lui, c'est lui qu'on voit ; quitté, il ne laisse rien derrière lui.
        b.from_shard = Some(1);
        b.receive(0, [0, 0], &set(9, u64::MAX), &mut w);
        b.from_shard = None;
        assert_eq!(w.get(1, 100, 1), BlockId(9));
        b.receive(0, [0, 0], &set(6, 15), &mut w);
        assert_eq!(w.get(1, 100, 1), BlockId(9), "encore servi par ce relais");
        b.forget_shard(1);
        b.receive(0, [0, 0], &set(6, 15), &mut w);
        assert_eq!(w.get(1, 100, 1), BlockId(6), "le relais du monde fait foi");
        // Un relais de région remet deux blocs : le relais du monde confirme le premier
        // (au même rang), jamais le second (inventé par un hôte malveillant, montré à ses
        // seuls joueurs). Quitté, le second revient comme avant, pas le premier.
        let at = |p: [i32; 3], block: u16, seq: u64| {
            game::encode(&BuildMsg::Set {
                pos: p,
                block,
                by: "Eve".into(),
                seq,
            })
        };
        let (real, fake) = ([2, 100, 2], [3, 100, 3]);
        let fake_before = w.get(3, 100, 3);
        b.from_shard = Some(2);
        b.receive(0, [0, 0], &at(real, 7, 1 << 40), &mut w);
        b.receive(0, [0, 0], &at(fake, 8, (1 << 40) + 1), &mut w);
        b.from_shard = None;
        assert_eq!(
            (w.get(2, 100, 2), w.get(3, 100, 3)),
            (BlockId(7), BlockId(8))
        );
        b.forget_shard(2);
        b.receive(0, [0, 0], &at(real, 7, 1 << 40), &mut w);
        for _ in 0..80 {
            let _ = b.update(&mut w, Vec3::ZERO, (0.0, 0.0), 0.0, 0.0, 0.0, 0.05);
        }
        assert_eq!(w.get(2, 100, 2), BlockId(7), "confirmé : il reste");
        assert_eq!(w.get(3, 100, 3), fake_before, "jamais confirmé : défait");
    }

    #[test]
    fn flowing_fluids_are_told_apart_from_placed_blocks() {
        let water = minelib_world::registry::Fluid::Water;
        assert!(fluid_edit(BlockId::AIR, water.block(7)));
        assert!(fluid_edit(water.block(7), BlockId::AIR));
        assert!(
            !fluid_edit(BlockId::STONE, water.block(7)),
            "l'eau ne casse pas la pierre"
        );
        assert!(
            !fluid_edit(BlockId::AIR, BlockId::STONE),
            "un bloc posé n'est pas de l'eau"
        );
    }
}
