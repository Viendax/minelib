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
use std::time::Instant;

use glam::Vec3;
use minelib_net::game;
use minelib_net::proto::{self, Request};
use minelib_net::transport::{self, Inbound};
use minelib_world::{BlockId, ChunkPos, World};

/// Modifications acceptées par seconde et par joueur.
const MAX_EDITS_PER_S: u32 = 20;
/// Position envoyée 20 fois par seconde.
const POS_EVERY: f32 = 0.05;
/// Un joueur dont on n'a plus de nouvelles depuis ce temps n'est plus montré.
const STALE_S: f32 = 3.0;

pub use minelib_net::game::BuildMsg;

/// Une modification non confirmée par le garant depuis ce temps est annulée (garant parti,
/// message perdu) : le monde de chacun reste celui du garant.
const PENDING_TIMEOUT: f32 = 4.0;

pub struct Other {
    pub pseudo: String,
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

/// Modification du joueur en attente de la confirmation du garant : bloc d'avant,
/// modifications de cette case pas encore confirmées, âge.
struct Pending {
    old: BlockId,
    count: u32,
    age: f32,
}

pub struct BuildNet {
    link: Sender<Inbound>,
    owner: bool,
    pseudo: String,
    /// Zones dont ce jeu est le garant.
    pub zones: HashSet<[i32; 2]>,
    pub others: HashMap<u64, Other>,
    chunk: Option<[i32; 2]>,
    pos_timer: f32,
    /// Modifications acceptées par joueur dans la seconde en cours.
    rate: HashMap<u64, (Instant, u32)>,
    caps_sent: bool,
    pending: HashMap<[i32; 3], Pending>,
}

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
            rate: HashMap::new(),
            caps_sent: false,
            pending: HashMap::new(),
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

    fn send(&self, req: Request) {
        transport::push(&self.link, &req);
    }

    fn garant_of(&self, chunk: [i32; 2]) -> bool {
        self.zones.contains(&proto::zone_of(chunk))
    }

    /// Capacités de ce PC pour être garant (annoncées une fois).
    pub fn caps(fps: f32) -> proto::GarantCaps {
        proto::GarantCaps {
            cores: std::thread::available_parallelism().map_or(1, |n| n.get() as u16),
            fps: fps.clamp(0.0, 1000.0) as u16,
            ram_mb: crate::session::ram_view_limit().max(0) as u32 * 256,
        }
    }

    /// Modifications du joueur pas encore confirmées par leur garant.
    #[cfg(test)]
    pub fn pending(&self) -> usize {
        self.pending.len()
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
        // Ce que le joueur a changé.
        let edits = world
            .journal
            .as_mut()
            .map(std::mem::take)
            .unwrap_or_default();
        for (pos, id, old) in edits {
            // Toujours une demande au garant de la zone, même quand c'est ce jeu (le relais
            // sait mieux que lui qui l'est à cet instant) ; le bloc d'avant est gardé pour
            // un refus.
            let chunk = chunk_of(pos[0], pos[2]);
            let p = self.pending.entry(pos).or_insert(Pending {
                old,
                count: 0,
                age: 0.0,
            });
            p.count += 1;
            p.age = 0.0;
            self.send(Request::ToZone {
                chunk,
                data: game::encode(&BuildMsg::Edit { pos, block: id.0 }),
            });
        }
        // Sans réponse du garant : la modification n'a pas eu lieu.
        let mut dirty = Vec::new();
        let mut expired = Vec::new();
        for (pos, p) in &mut self.pending {
            p.age += dt;
            if p.age > PENDING_TIMEOUT {
                expired.push((*pos, p.old));
            }
        }
        for (pos, old) in expired {
            self.pending.remove(&pos);
            log::info!("construction : pas de réponse du garant pour {pos:?}, annulé");
            dirty.extend(world.set_remote(pos[0], pos[1], pos[2], old));
        }
        // Position : aux joueurs proches ; nouveau chunk : au relais.
        let chunk = chunk_of(feet.x.floor() as i32, feet.z.floor() as i32);
        if self.chunk != Some(chunk) {
            self.chunk = Some(chunk);
            self.send(Request::Position { chunk });
        }
        self.pos_timer -= dt;
        if self.pos_timer <= 0.0 {
            self.pos_timer = POS_EVERY;
            let msg = BuildMsg::Player {
                pseudo: self.pseudo.clone(),
                pos: feet.to_array(),
                yaw: look.0,
                pitch: look.1,
                crouch,
                speed,
            };
            self.send(Request::Nearby {
                chunk,
                data: game::encode(&msg),
            });
        }
        for o in self.others.values_mut() {
            o.seen += dt;
            // Rejoint la dernière position reçue en un intervalle : vitesse régulière même
            // pour un joueur lointain (moins de messages).
            let k = (dt / o.interval.max(POS_EVERY)).min(1.0);
            o.pos += (o.target - o.pos) * k;
        }
        self.others.retain(|_, o| o.seen < STALE_S);
        (caps, dirty)
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
        chunk: [i32; 2],
        data: &[u8],
        world: &mut World,
    ) -> Vec<ChunkPos> {
        let Some(msg) = game::decode::<BuildMsg>(data) else {
            return Vec::new();
        };
        match msg {
            BuildMsg::Edit { pos, block } => {
                // Seul le garant reçoit les demandes ; un refus revient au seul demandeur.
                if !self.validate(peer, chunk, pos, block) {
                    self.send(Request::ToZone {
                        chunk,
                        data: game::encode(&BuildMsg::Reject { pos, peer }),
                    });
                    return Vec::new();
                }
                let set = BuildMsg::Set {
                    pos,
                    block,
                    by: self
                        .others
                        .get(&peer)
                        .map_or(String::new(), |o| o.pseudo.clone()),
                };
                self.send(Request::ToZone {
                    chunk,
                    data: game::encode(&set),
                });
                // Sa propre modification (le garant ne reçoit pas ce qu'il publie) : confirmée.
                self.confirm(pos);
                world.set_remote(pos[0], pos[1], pos[2], BlockId(block))
            }
            BuildMsg::Set { pos, block, by } => {
                // Le garant d'une zone ne publie que pour sa zone (l'historique envoyé par le
                // relais porte le coin de la zone, d'où la comparaison par zone).
                if proto::zone_of(chunk_of(pos[0], pos[2])) != proto::zone_of(chunk)
                    || block as usize >= minelib_world::registry::registry().len()
                {
                    return Vec::new();
                }
                log::debug!("construction : bloc {block} en {pos:?} (par {by})");
                // Ce que dit le garant fait foi (et confirme une modification en attente).
                self.confirm(pos);
                world.set_remote(pos[0], pos[1], pos[2], BlockId(block))
            }
            BuildMsg::Reject { pos, .. } => {
                // Refusé : le bloc d'avant revient.
                match self.pending.remove(&pos) {
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
                let target = Vec3::from(pos);
                match self.others.get_mut(&peer) {
                    Some(o) => {
                        // Intervalle mesuré (lissé) ; téléportation : on y va d'un coup.
                        o.interval = o.interval * 0.7 + o.seen.clamp(POS_EVERY, 2.0) * 0.3;
                        if (target - o.pos).length() > 8.0 {
                            o.pos = target;
                        }
                        o.target = target;
                        o.yaw = yaw;
                        o.pitch = pitch;
                        o.crouch = crouch;
                        o.speed = speed;
                        o.seen = 0.0;
                    }
                    None => {
                        self.others.insert(
                            peer,
                            Other {
                                pseudo: pseudo.chars().take(16).collect(),
                                pos: target,
                                target,
                                interval: POS_EVERY,
                                yaw,
                                pitch,
                                crouch,
                                speed,
                                seen: 0.0,
                            },
                        );
                    }
                }
                Vec::new()
            }
        }
    }

    /// Une modification de cette case est confirmée.
    fn confirm(&mut self, pos: [i32; 3]) {
        if let Some(p) = self.pending.get_mut(&pos) {
            p.count = p.count.saturating_sub(1);
            if p.count == 0 {
                self.pending.remove(&pos);
            }
        }
    }

    /// Vérification du garant : bloc connu, dans une de ses zones, dans le chunk annoncé,
    /// hauteur valide, débit raisonnable.
    fn validate(&mut self, peer: u64, chunk: [i32; 2], pos: [i32; 3], block: u16) -> bool {
        if !self.garant_of(chunk)
            || chunk_of(pos[0], pos[2]) != chunk
            || !(0..minelib_world::CHUNK_HEIGHT as i32).contains(&pos[1])
            || block as usize >= minelib_world::registry::registry().len()
        {
            return false;
        }
        let now = Instant::now();
        let e = self.rate.entry(peer).or_insert((now, 0));
        if now.duration_since(e.0).as_secs_f32() >= 1.0 {
            *e = (now, 0);
        }
        e.1 += 1;
        e.1 <= MAX_EDITS_PER_S
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use minelib_net::client::{Client, Cmd, Event};
    use std::time::Duration;

    /// Un joueur simulé : sa connexion, son monde, ce qu'il a changé.
    struct Bot {
        name: String,
        c: Client,
        net: Option<BuildNet>,
        link: Option<Sender<Inbound>>,
        world: World,
        pos: Vec3,
        rejects: u32,
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
            };
            b.wait(|e| matches!(e, Event::LoggedIn { .. }));
            b
        }

        /// Traite les événements reçus ; renvoie vrai si `want` est arrivé.
        fn pump(&mut self, want: &dyn Fn(&Event) -> bool) -> bool {
            let mut hit = false;
            while let Some(e) = self.c.poll() {
                hit |= want(&e);
                match e {
                    Event::Link(l) => self.link = Some(l),
                    Event::BuildGarant(_) => {
                        let link = self.link.clone().expect("file d'envoi");
                        self.net = Some(BuildNet::new(link, self.name.clone(), false));
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
                    }
                    Event::Promoted { .. } => {
                        if let Some(n) = &mut self.net {
                            n.set_owner();
                        }
                    }
                    Event::Error(m) => panic!("{} : {m}", self.name),
                    _ => {}
                }
            }
            hit
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
            self.net = Some(BuildNet::new(link, self.name.clone(), owner));
        }

        fn tick(&mut self, dt: f32) {
            self.pump(&|_| false);
            if let Some(n) = &mut self.net {
                let _ = n.update(&mut self.world, self.pos, (0.0, 0.0), 0.0, 4.0, 144.0, dt);
            }
        }
    }

    /// Ce qui se passe pendant un essai.
    #[derive(Default)]
    struct Scenario {
        players: usize,
        /// Le garant extérieur s'en va au milieu (ses zones reviennent aux joueurs).
        garant_leaves: bool,
        /// Le propriétaire s'en va au milieu (un joueur reprend le monde et l'enregistre).
        owner_leaves: bool,
    }

    /// Construction en ligne à petite échelle, de bout en bout (vrai relais, vraies
    /// connexions) : des joueurs qui se déplacent et construisent à cheval sur deux zones,
    /// un garant extérieur (joueur d'un autre monde), un joueur qui inonde (refusé au-delà
    /// de 20 par seconde), un joueur qui arrive après coup. À la fin, tous les mondes sont
    /// identiques à celui du propriétaire, case par case, et rien n'est en attente.
    fn run(sc: Scenario) {
        let dir = std::env::temp_dir().join(format!("minelib-sync-{}", rand_u32()));
        minelib_relay::test_seed_games(&dir, "Garant", 5);
        let h = minelib_relay::start(minelib_relay::Config {
            dir: dir.clone(),
            bind: "127.0.0.1:0".into(),
            pow_bits: 4,
            name: "essai".into(),
        })
        .unwrap();
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
            if step == 150 && sc.garant_leaves {
                garant.c.send(Cmd::Leave);
                garant.net = None;
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
            had_garant |= garant.net.as_ref().is_some_and(|n| !n.zones.is_empty());
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
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            had_garant,
            "le monde a reçu son garant extérieur, et ses zones"
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
        for b in &live {
            assert_eq!(
                b.net.as_ref().unwrap().pending(),
                0,
                "{} : en attente",
                b.name
            );
        }
        // La référence : le propriétaire du moment (celui qui enregistre le monde).
        let owner = live
            .iter()
            .find(|b| b.net.as_ref().unwrap().is_owner())
            .expect("un propriétaire");
        let mut differ = Vec::new();
        for b in &live {
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
    fn build_worlds_stay_identical_for_everyone() {
        run(Scenario {
            players: 6,
            ..Default::default()
        });
    }

    #[test]
    fn build_worlds_stay_identical_when_the_outside_garant_leaves() {
        run(Scenario {
            players: 6,
            garant_leaves: true,
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
            shown.push(b.others[&7].pos.x);
        }
        // Après la mise en route : jamais de bond de plus de 0,5 bloc d'une image à l'autre.
        let jumps = shown[60..]
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0, f32::max);
        assert!(jumps < 0.5, "bond de {jumps}");
        assert!(shown[119] > 15.0, "il avance : {}", shown[119]);
    }

    fn rand_u32() -> u32 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(1, |d| d.subsec_nanos())
    }

    #[test]
    fn the_garant_refuses_edits_outside_its_zones_and_floods() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut b = BuildNet::new(tx, "G".into(), false);
        b.zones.insert([0, 0]);
        assert!(b.validate(7, [1, 1], [20, 64, 20], 1));
        assert!(!b.validate(7, [9, 9], [150, 64, 150], 1), "pas sa zone");
        assert!(
            !b.validate(7, [1, 1], [0, 64, 0], 1),
            "pas le chunk annoncé"
        );
        assert!(!b.validate(7, [1, 1], [20, 64, 20], 60000), "bloc inconnu");
        let ok = (0..30)
            .filter(|_| b.validate(8, [1, 1], [20, 64, 20], 1))
            .count();
        assert_eq!(ok, MAX_EDITS_PER_S as usize, "20 par seconde au plus");
    }
}
