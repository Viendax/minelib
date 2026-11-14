//! Partie FPS en ligne : l'hôte (le joueur qui a ouvert la partie) fait foi, les invités lui
//! envoient leurs entrées par le relais. Les règles (manches, argent, bombe) sont celles de
//! CS2 (`cs`).
//!
//! - Tout le monde simule au même pas fixe (`session::SIM_TICK`, 256/s) avec le même code
//!   de déplacement : l'invité prédit son propre mouvement, et quand l'hôte le corrige il
//!   reprend l'état de l'hôte puis rejoue les entrées pas encore confirmées.
//! - Les autres joueurs sont montrés avec un léger retard (interpolation entre deux états
//!   reçus) ; pour juger un tir, l'hôte rembobine les cibles à ce que le tireur voyait.
//! - Les grenades lancées sont envoyées à tous : chacun les simule (fumée, flash, feu au
//!   même endroit) ; l'hôte en tire les dégâts.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::mpsc::Sender;

use glam::Vec3;
use minelib_net::game::{
    self, FpsMap, GroundGun, MatchHud, MatchSummary, NetInput, NetPlayer, ScoreRow, SimState,
    ToHost, ToPeer, CT,
};
use minelib_net::proto::Request;
use minelib_net::transport::{self, Inbound};
use minelib_world::raycast::raycast;
use minelib_world::World;

use crate::cs::{self, Ev, Hurt, Match, Place};
use crate::player::{MoveInput, Player, HALF_WIDTH, HEIGHT};
use crate::weapons::WEAPONS;

/// Un état envoyé aux invités tous les 4 pas (64 fois par seconde).
const SNAPSHOT_EVERY: u32 = 4;
/// Tableau des scores : toutes les demi-secondes.
const SCOREBOARD_EVERY: u32 = 128;
/// Positions gardées pour rembobiner les tirs : 1 s.
const HISTORY: usize = 256;
/// Retard d'affichage des autres joueurs chez l'invité (pas) : ~47 ms.
const INTERP_TICKS: f32 = 12.0;
/// Écart (blocs) au-delà duquel l'invité reprend la position de l'hôte.
const CORRECT: f32 = 0.01;
/// Au plus tant d'entrées en retard traitées par pas pour un invité (rattrapage).
const CATCH_UP: usize = 3;
const RANGE: f32 = 250.0;
/// Couteau : portée et dégâts (coup rapide, coup lourd).
const KNIFE_RANGE: f32 = 1.7;
pub const KNIFE_LIGHT: u8 = 255;
pub const KNIFE_HEAVY: u8 = 254;
/// Écart minimal entre deux joueurs qui apparaissent (blocs).
const SPAWN_GAP: f32 = 1.5;

pub fn to_net(m: &MoveInput, yaw: f32, pitch: f32) -> NetInput {
    NetInput {
        forward: m.forward.round().clamp(-1.0, 1.0) as i8,
        strafe: m.strafe.round().clamp(-1.0, 1.0) as i8,
        jump: m.jump,
        jump_pressed: m.jump_pressed,
        walk: !m.sprint,
        duck: m.sneak,
        yaw,
        pitch,
    }
}

pub fn from_net(n: &NetInput) -> MoveInput {
    MoveInput {
        forward: n.forward as f32,
        strafe: n.strafe as f32,
        up: 0.0,
        jump: n.jump,
        sprint: !n.walk,
        sneak: n.duck,
        jump_pressed: n.jump_pressed,
        fps: true,
    }
}

/// Entrées sans déplacement (départ de manche, pose de la bombe) : on peut encore
/// regarder et s'accroupir.
pub fn still(m: &MoveInput) -> MoveInput {
    MoveInput {
        forward: 0.0,
        strafe: 0.0,
        jump: false,
        jump_pressed: false,
        ..*m
    }
}

/// Un pas de simulation d'un joueur à partir de ses entrées.
fn step(p: &mut Player, world: &World, n: &NetInput, dt: f32) {
    p.yaw = n.yaw;
    p.pitch = n.pitch;
    p.update(world, &from_net(n), dt);
}

/// Un autre joueur, tel qu'il faut le dessiner.
pub struct View {
    pub pseudo: String,
    pub pos: Vec3,
    pub yaw: f32,
    pub pitch: f32,
    pub crouch: f32,
    pub speed: f32,
    pub alive: bool,
    pub weapon: u8,
    pub team: u8,
    pub flags: u8,
}

/// Ce que la partie demande à la session locale.
pub enum Local {
    /// Points de vie du joueur local.
    SetHp(u32),
    /// Le joueur local réapparaît (nouvelle manche : `reset` = pistolet de départ).
    Respawn {
        state: SimState,
        reset: bool,
        team: u8,
    },
    Chat(String),
    /// Ligne en couleur (annonces des règles).
    Notice(String),
    /// Marqueur de touche.
    Hit {
        head: bool,
    },
    /// Achat accepté.
    Give(u16),
    RoundEnd {
        winner: u8,
        reason: u8,
    },
    GameOver(MatchSummary),
    /// Grenade d'un autre joueur à simuler.
    Thrown {
        owner: u64,
        nade: u8,
        pos: Vec3,
        vel: Vec3,
    },
    /// La partie commence (10 joueurs) : à annoncer au relais.
    Started,
}

/// Ce que les grenades du joueur local ont produit à cette image (pour l'hôte).
#[derive(Default)]
pub struct NadeOut {
    /// Grenades lancées : (type, position, vitesse).
    pub throws: Vec<(u8, Vec3, Vec3)>,
    /// Explosions de HE : (position, lanceur).
    pub blasts: Vec<(Vec3, u64)>,
    /// Cases en feu : (position, lanceur).
    pub fires: Vec<(Vec3, u64)>,
}

struct Remote {
    pseudo: String,
    player: Player,
    queue: VecDeque<(u32, NetInput)>,
    ack: u32,
    weapon: u8,
    ready: bool,
    fall_from: Option<f32>,
}

/// Copie de la partie confiée au garant de secours : de quoi la reprendre si l'hôte
/// part.
#[derive(serde::Serialize, serde::Deserialize)]
struct Backup {
    m: Match,
    host_pseudo: String,
    /// Joueurs (0 = l'hôte) : pseudo, état de déplacement, arme en main.
    players: Vec<(u64, String, SimState, u8)>,
    ground: Vec<GroundGun>,
    next_gun: u32,
    loadouts: Vec<(u64, (u8, u8))>,
    tick: u32,
    spawn: [f32; 3],
    /// Coups joués depuis la copie précédente (pas, joueur, coup) : de quoi les rejouer.
    log: Vec<(u32, u64, Move)>,
    /// Tête de la chaîne des coups de la partie (empreinte de tous les coups joués).
    chain: [u8; 32],
    /// Hauteur d'où chacun tombe, et brûlure en cours (de quoi rejouer chutes et feu).
    falls: Vec<(u64, f32)>,
    burns: Vec<(u64, f32)>,
}

/// Un coup du journal de la partie. Tout ce qui change la partie y passe, dans l'ordre
/// où l'hôte l'a fait : le vérificateur rejoue la partie entière à partir de ce journal.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq)]
pub enum Move {
    /// Entrées d'un pas d'un invité (son numéro d'entrée).
    Input(u32, NetInput),
    /// Pas de l'hôte qui joue : ses entrées et l'état obtenu.
    Own(NetInput, SimState),
    /// Le joueur est replacé (réapparition).
    Set(SimState),
    /// Un pas des règles ou une demande d'un joueur.
    Rule(Rule),
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq)]
pub enum Rule {
    /// Le joueur se présente (`ok` : avec la bonne carte).
    Hello { pseudo: String, ok: bool },
    /// Le joueur s'en va.
    Leave,
    /// Chutes puis pas des règles (`Match::tick`).
    Tick,
    /// Positions de tous gardées pour rembobiner les tirs (pas n°).
    Frame(u32),
    /// Demande d'un joueur (tir, grenade, achat, touches, équipe), telle que reçue :
    /// le vérificateur décide lui-même si elle était recevable.
    Msg(ToHost),
    /// Explosions et cases en feu (position, lanceur) de cette image, et sa durée.
    Nades {
        blasts: Vec<([f32; 3], u64)>,
        fires: Vec<([f32; 3], u64)>,
        dt: f32,
    },
    /// Dégâts que l'hôte se fait lui-même (chute, vide).
    SelfHurt(u32),
}

/// Demandes d'un joueur que le relais montre aussi au vérificateur (il sait ainsi que
/// l'hôte ne les invente pas et ne les oublie pas).
pub fn witnessed(m: &ToHost) -> bool {
    matches!(
        m,
        ToHost::Fire { .. }
            | ToHost::Throw { .. }
            | ToHost::Buy(_)
            | ToHost::Action { .. }
            | ToHost::Team(_)
    )
}

/// Ajoute un coup à la chaîne : chaque maillon dépend de tous les précédents.
pub fn chain_step(prev: &[u8; 32], tick: u32, who: u64, mv: &Move) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(prev);
    h.update(tick.to_le_bytes());
    h.update(who.to_le_bytes());
    h.update(game::encode(mv));
    h.finalize().into()
}

/// Désaccord trouvé par le garant vérificateur.
#[derive(Clone, Debug, PartialEq)]
pub struct Conflict {
    /// Joueur (0 = l'hôte), dernier numéro d'entrée rejoué, positions selon l'hôte et
    /// selon le vérificateur ; `proof` : preuve sans appel (chaîne faussée, hôte qui
    /// triche sur lui-même).
    pub player: u64,
    pub seq: u32,
    pub host_pos: [f32; 3],
    pub audit_pos: [f32; 3],
    pub proof: bool,
    /// Ce qui ne va pas.
    pub why: &'static str,
}

/// Joueur placé dans l'état `s`, regard `yaw`/`pitch`.
fn player_from(s: &SimState) -> Player {
    let mut p = Player::new(Vec3::from(s.pos));
    p.set_net_state(s);
    p
}

/// Un tir est-il recevable ? Tireur vivant, tir depuis ses yeux, pas pendant le départ.
fn fire_ok(m: &Match, shooter: u64, eye: Option<Vec3>, origin: Vec3, rays: usize) -> bool {
    m.players.get(&shooter).is_some_and(|p| p.alive)
        && eye.is_some_and(|e| (e - origin).length() < 2.5)
        && rays <= 16
        && !m.frozen()
}

/// Positions gardées au pas le plus proche de `view_tick`.
fn pick_frame(history: &VecDeque<(u32, Frame)>, view_tick: u32) -> Frame {
    history
        .iter()
        .min_by_key(|(t, _)| t.abs_diff(view_tick))
        .map(|(_, f)| f.clone())
        .unwrap_or_default()
}

/// Dégâts d'un tir (rayons `dirs` depuis `origin`) aux joueurs vivants de `frame`.
fn shot(
    m: &Match,
    frame: &Frame,
    shooter: u64,
    weapon: u8,
    origin: Vec3,
    dirs: &[Vec3],
    world: &World,
) -> BTreeMap<u64, Vec<(u32, Hurt)>> {
    let knife = weapon == KNIFE_LIGHT || weapon == KNIFE_HEAVY;
    let range = if knife { KNIFE_RANGE } else { RANGE };
    let mut damage: BTreeMap<u64, Vec<(u32, Hurt)>> = BTreeMap::new();
    for &dir in dirs {
        let wall = raycast(world, origin.into(), dir.into(), range).map_or(range, |h| h.distance);
        let mut best: Option<(f32, u64, u32, Hurt)> = None;
        for &(id, feet, crouch) in frame {
            if id == shooter || !m.players.get(&id).is_some_and(|p| p.alive) {
                continue;
            }
            let (min, max, head_y) = hitbox(feet, crouch);
            if let Some(t) = ray_box(origin, dir, min, max) {
                if t < wall && best.is_none_or(|b| t < b.0) {
                    let y = (origin + dir * t).y;
                    let (d, hurt) = damage_at(weapon, y, feet.y, head_y);
                    best = Some((t, id, d, hurt));
                }
            }
        }
        if let Some((_, id, d, hurt)) = best {
            damage.entry(id).or_default().push((d, hurt));
        }
    }
    damage
}

/// Les chunks que traversent les rayons jusqu'aux joueurs qu'ils croisent sont-ils
/// chargés ? (Sinon le vérificateur ne peut pas juger les murs.)
fn rays_loaded(world: &World, frame: &Frame, origin: Vec3, dirs: &[Vec3]) -> bool {
    dirs.iter().all(|&dir| {
        let far = frame
            .iter()
            .filter_map(|&(_, feet, crouch)| {
                let (min, max, _) = hitbox(feet, crouch);
                ray_box(origin, dir, min, max)
            })
            .fold(0.0f32, f32::max)
            .min(RANGE);
        let mut d = 0.0;
        while d <= far + 1.0 {
            let p = origin + dir * d;
            if !world.contains(minelib_world::ChunkPos::containing(
                p.x.floor() as i32,
                p.z.floor() as i32,
            )) {
                return false;
            }
            d += 4.0;
        }
        true
    })
}

/// Chute d'un joueur : dégâts à l'atterrissage (ou dans le vide).
fn fall_check(p: &Player, from: &mut Option<f32>) -> Option<u32> {
    if p.pos.y < -40.0 {
        return Some(1000);
    }
    if !p.on_ground {
        *from = Some(from.map_or(p.pos.y, |f| f.max(p.pos.y)));
        None
    } else if let Some(f) = from.take() {
        let dmg = crate::fps::Fps::fall_damage(f - p.pos.y, crate::session::SAFE_FALL + 1.0);
        (dmg > 0).then_some(dmg)
    } else {
        None
    }
}

/// Dégâts des grenades d'une image : (lanceur, victime, dégâts, nature), dans l'ordre.
fn nade_hurts(
    world: &World,
    targets: &[(u64, Vec3)],
    blasts: &[(Vec3, u64)],
    fires: &[(Vec3, u64)],
    dt: f32,
    burns: &mut BTreeMap<u64, f32>,
) -> Vec<(u64, u64, u32, Hurt)> {
    let mut out = Vec::new();
    for &(pos, owner) in blasts {
        for &(id, feet) in targets {
            let chest = feet + Vec3::Y * 1.1;
            let d = (chest - pos).length();
            let clear = raycast(
                world,
                (pos + Vec3::Y * 0.2).into(),
                ((chest - pos).normalize_or_zero()).into(),
                d,
            )
            .is_none_or(|h| h.distance >= d - 0.3);
            if d < 9.0 && clear {
                let k = 1.0 - d / 9.0;
                let dmg = (98.0 * k * k) as u32;
                out.push((owner, id, dmg, Hurt::Grenade));
            }
        }
    }
    for &(id, feet) in targets {
        let burning = fires.iter().find(|(c, _)| {
            (c.x - feet.x).abs() < 0.8 && (c.z - feet.z).abs() < 0.8 && (feet.y - c.y).abs() < 1.2
        });
        let Some(&(_, owner)) = burning else { continue };
        let acc = burns.entry(id).or_insert(0.0);
        *acc += 40.0 * dt;
        let whole = acc.floor();
        *acc -= whole;
        if whole >= 1.0 {
            out.push((owner, id, whole as u32, Hurt::Fire));
        }
    }
    out
}

/// Garant vérificateur : rejoue la partie entière entre deux copies (déplacements, tirs,
/// dégâts, grenades, achats, règles) et compare avec ce que l'hôte annonce.
#[derive(Default)]
pub struct Auditor {
    /// Positions rejouées (rembobinage des tirs), comme chez l'hôte.
    history: VecDeque<(u32, Frame)>,
    /// Grenades lancées : (pas, lanceur, type, position, vitesse, déjà explosée).
    throws: Vec<(u32, u64, u8, Vec3, f32, bool)>,
    /// Premier pas dont le vérificateur a vu tous les coups.
    since: Option<u32>,
    /// Le relais montre au vérificateur les demandes des joueurs.
    pub witnessing: bool,
    /// Demandes vues par le relais : (joueur, demande, copie n° à l'arrivée ; aucune :
    /// arrivée avant la première copie, peut-être déjà traitée sans être vérifiée).
    witness: Vec<(u64, ToHost, Option<u32>)>,
    /// Copies reçues.
    copies: u32,
    /// Fins de partie trouvées en rejouant (résultat à envoyer au relais).
    pub results: Vec<MatchSummary>,
}

/// Une demande vue par le relais doit apparaître au journal dans les copies qui suivent.
const WITNESS_LATE: u32 = 4;
const TICKS_PER_S: u32 = 256;

impl Auditor {
    pub fn witness(&mut self, from: u64, msg: ToHost) {
        if witnessed(&msg) && self.witness.len() < 8192 {
            self.witness.push((from, msg, Some(self.copies)));
        }
    }

    /// Première copie (rien à comparer) : les demandes déjà vues ont pu y être traitées.
    fn first_copy(&mut self) {
        for w in &mut self.witness {
            w.2 = None;
        }
    }

    /// L'hôte change : on repart de zéro.
    pub fn reset(&mut self) {
        let witnessing = self.witnessing;
        *self = Self {
            witnessing,
            ..Self::default()
        };
    }

    /// Rejoue le journal de `new` à partir de `prev` et compare.
    fn audit(&mut self, prev: &Backup, new: &Backup, world: &World) -> Vec<Conflict> {
        let dt = crate::session::SIM_TICK;
        self.copies += 1;
        let mut out = Vec::new();
        let mut wrong: Vec<&'static str> = Vec::new();
        // La chaîne doit prolonger la précédente avec exactement ces coups.
        let mut chain = prev.chain;
        for (t, who, mv) in &new.log {
            chain = chain_step(&chain, *t, *who, mv);
        }
        if chain != new.chain || prev.host_pseudo != new.host_pseudo {
            if chain != new.chain {
                out.push(Conflict::proof("chaîne des coups réécrite"));
            }
            self.reset();
            return out;
        }
        let since = *self.since.get_or_insert(prev.tick);
        let spawn = Vec3::from(prev.spawn);
        let mut m = prev.m.clone();
        let mut players: BTreeMap<u64, Player> = prev
            .players
            .iter()
            .map(|(id, _, s, _)| (*id, player_from(s)))
            .collect();
        let mut ready: BTreeSet<u64> = prev.players.iter().map(|p| p.0).collect();
        let mut falls: BTreeMap<u64, Option<f32>> =
            prev.falls.iter().map(|&(id, f)| (id, Some(f))).collect();
        let mut burns: BTreeMap<u64, f32> = prev.burns.iter().copied().collect();
        // L'hôte : son déplacement rejoué, et le point de réapparition pas encore pris.
        let mut sim0: Option<Player> = players.get(&0).cloned();
        let mut cand0: Option<Player> = None;
        let mut verifiable = true;
        let mut skip: BTreeSet<u64> = BTreeSet::new();
        let mut last_seq: BTreeMap<u64, u32> = BTreeMap::new();
        let mut events: Vec<Ev> = Vec::new();
        let alive = |m: &Match, id: u64| m.players.get(&id).is_some_and(|p| p.alive);
        for (t, who, mv) in &new.log {
            let (t, who) = (*t, *who);
            match mv {
                Move::Input(seq, input) => {
                    let Some(p) = players.get_mut(&who) else {
                        continue;
                    };
                    let moving =
                        input.forward != 0 || input.strafe != 0 || input.jump || input.jump_pressed;
                    if !alive(&m, who) || ((m.frozen() || m.busy(who)) && moving) {
                        wrong.push("déplacement d'un joueur mort ou immobilisé");
                    }
                    step(p, world, input, dt);
                    last_seq.insert(who, *seq);
                }
                Move::Own(input, state) => {
                    let mut s = sim0.take().unwrap_or_else(|| player_from(state));
                    step(&mut s, world, input, dt);
                    // Réapparition : l'hôte la prend à la fin de son image (quelques pas
                    // plus tard), en repartant du point d'apparition.
                    if let Some(c) = &cand0 {
                        let mut c = c.clone();
                        step(&mut c, world, input, dt);
                        if (c.pos - Vec3::from(state.pos)).length() < 0.05 {
                            s = c;
                            cand0 = None;
                        }
                    }
                    let off = (s.pos - Vec3::from(state.pos)).length();
                    let loaded = world.contains(minelib_world::ChunkPos::containing(
                        s.pos.x.floor() as i32,
                        s.pos.z.floor() as i32,
                    ));
                    if alive(&m, 0) && off > 0.05 && loaded {
                        out.push(Conflict {
                            player: 0,
                            seq: 0,
                            host_pos: state.pos,
                            audit_pos: s.pos.to_array(),
                            proof: true,
                            why: "l'hôte ne suit pas ses propres entrées",
                        });
                    }
                    if !alive(&m, 0) || off > 0.05 {
                        s = player_from(state);
                    }
                    sim0 = Some(s);
                    let mut p = player_from(state);
                    p.yaw = input.yaw;
                    p.pitch = input.pitch;
                    players.insert(0, p);
                }
                Move::Set(state) => {
                    if who == 0 {
                        cand0 = Some(player_from(state));
                    } else {
                        players.insert(who, player_from(state));
                        falls.remove(&who);
                        skip.insert(who);
                    }
                }
                Move::Rule(rule) => match rule {
                    Rule::Hello { pseudo, ok } => {
                        if *ok {
                            ready.insert(who);
                            players.entry(who).or_insert_with(|| Player::new(spawn));
                            if !m.players.get(&who).is_some_and(|p| p.present) {
                                events.extend(m.join(who, pseudo));
                            }
                        } else {
                            ready.remove(&who);
                        }
                    }
                    Rule::Leave => {
                        let at = players.remove(&who).map_or(spawn, |p| p.pos);
                        ready.remove(&who);
                        falls.remove(&who);
                        m.leave(who, Some(at));
                    }
                    Rule::Tick => {
                        let mut hurt = Vec::new();
                        for (&id, p) in &players {
                            if id == 0 || !ready.contains(&id) || !alive(&m, id) {
                                continue;
                            }
                            let f = falls.entry(id).or_insert(None);
                            if let Some(d) = fall_check(p, f) {
                                hurt.push((id, d));
                            }
                        }
                        for (id, d) in hurt {
                            events.extend(m.hurt(None, id, d, Hurt::World));
                        }
                        let places: BTreeMap<u64, Place> = players
                            .iter()
                            .filter(|(id, _)| **id == 0 || ready.contains(id))
                            .map(|(&id, p)| (id, place(p)))
                            .collect();
                        events.extend(m.tick(dt, &places));
                    }
                    Rule::Frame(tick) => {
                        let frame: Frame = players
                            .iter()
                            .filter(|(id, _)| **id == 0 || ready.contains(id))
                            .map(|(&id, p)| (id, p.pos, p.crouch))
                            .collect();
                        self.history.push_back((*tick, frame));
                        if self.history.len() > HISTORY {
                            self.history.pop_front();
                        }
                    }
                    Rule::Msg(msg) => {
                        if who != 0 && self.witnessing {
                            match self
                                .witness
                                .iter()
                                .position(|(f, w, _)| *f == who && w == msg)
                            {
                                Some(i) => {
                                    self.witness.remove(i);
                                }
                                None => wrong.push("demande inventée par l'hôte"),
                            }
                        }
                        let me = if who == 0 || ready.contains(&who) {
                            players.get(&who)
                        } else {
                            None
                        };
                        match msg {
                            ToHost::Fire {
                                weapon,
                                origin,
                                dirs,
                                view_tick,
                            } => {
                                let origin = Vec3::from(*origin);
                                let dirs: Vec<Vec3> = dirs
                                    .iter()
                                    .map(|d| Vec3::from(*d).normalize_or_zero())
                                    .collect();
                                if fire_ok(&m, who, me.map(|p| p.eye()), origin, dirs.len()) {
                                    let covered = self
                                        .history
                                        .front()
                                        .is_some_and(|f| f.0 <= *view_tick)
                                        || self.history.len() >= HISTORY;
                                    let frame = pick_frame(&self.history, *view_tick);
                                    if !covered || !rays_loaded(world, &frame, origin, &dirs) {
                                        verifiable = false;
                                    } else {
                                        let hits =
                                            shot(&m, &frame, who, *weapon, origin, &dirs, world);
                                        for (victim, hits) in hits {
                                            for (d, h) in hits {
                                                events.extend(m.hurt(Some(who), victim, d, h));
                                            }
                                        }
                                    }
                                }
                            }
                            ToHost::Throw { nade, pos, vel } => {
                                let pos = Vec3::from(*pos);
                                let vel = Vec3::from(*vel);
                                let ok = who == 0
                                    || (alive(&m, who)
                                        && players
                                            .get(&who)
                                            .is_some_and(|p| (p.eye() - pos).length() < 3.0)
                                        && *nade < 5
                                        && vel.length() < 40.0);
                                if ok {
                                    self.throws.push((t, who, *nade, pos, vel.length(), false));
                                }
                            }
                            ToHost::Buy(item) => {
                                let at = me.map(|p| p.pos);
                                let _ = m.buy(who, *item, at);
                            }
                            ToHost::Action { plant, defuse } => m.keys(who, *plant, *defuse),
                            ToHost::Team(team) => events.extend(m.switch_team(who, *team)),
                            _ => {}
                        }
                    }
                    Rule::Nades {
                        blasts,
                        fires,
                        dt: frame_dt,
                    } => {
                        if !(0.0..=0.25).contains(frame_dt) {
                            wrong.push("durée d'image impossible");
                        }
                        let seen_all = |window: u32| t >= since + window;
                        for (pos, owner) in blasts {
                            let pos = Vec3::from(*pos);
                            let found = self.throws.iter_mut().find(|th| {
                                th.1 == *owner
                                    && th.2 == HE
                                    && !th.5
                                    && t.saturating_sub(th.0) <= 3 * TICKS_PER_S
                                    && (pos - th.3).length() <= th.4 * 1.7 + 45.0
                            });
                            match found {
                                Some(th) => th.5 = true,
                                None if seen_all(3 * TICKS_PER_S) => {
                                    wrong.push("explosion sans grenade")
                                }
                                None => {}
                            }
                        }
                        for (pos, owner) in fires {
                            let pos = Vec3::from(*pos);
                            let found = self.throws.iter().any(|th| {
                                th.1 == *owner
                                    && th.2 == MOLOTOV
                                    && t.saturating_sub(th.0) <= 14 * TICKS_PER_S
                                    && (pos - th.3).length() <= th.4 * 2.1 + 70.0
                            });
                            if !found && seen_all(14 * TICKS_PER_S) {
                                wrong.push("feu sans molotov");
                            }
                        }
                        let targets: Vec<(u64, Vec3)> = players
                            .iter()
                            .filter(|(id, _)| **id == 0 || ready.contains(id))
                            .map(|(&id, p)| (id, p.pos))
                            .collect();
                        let blasts: Vec<(Vec3, u64)> =
                            blasts.iter().map(|(p, o)| (Vec3::from(*p), *o)).collect();
                        let fires: Vec<(Vec3, u64)> =
                            fires.iter().map(|(p, o)| (Vec3::from(*p), *o)).collect();
                        let hurts =
                            nade_hurts(world, &targets, &blasts, &fires, *frame_dt, &mut burns);
                        for (owner, victim, d, h) in hurts {
                            events.extend(m.hurt(Some(owner), victim, d, h));
                        }
                    }
                    Rule::SelfHurt(amount) => {
                        if who != 0 {
                            wrong.push("dégâts « à soi » d'un autre joueur");
                        } else {
                            events.extend(m.hurt(None, 0, *amount, Hurt::World));
                        }
                    }
                },
            }
        }
        let last = new.tick;
        self.throws
            .retain(|th| last.saturating_sub(th.0) <= 20 * TICKS_PER_S);
        // Demandes vues par le relais que l'hôte n'a jamais traitées.
        let copies = self.copies;
        let mut ignored = 0;
        self.witness.retain(|w| match w.2 {
            Some(g) => {
                let keep = g + WITNESS_LATE >= copies;
                ignored += usize::from(!keep);
                keep
            }
            None => copies <= WITNESS_LATE,
        });
        if ignored > 0 {
            wrong.push("demande d'un joueur ignorée par l'hôte");
        }
        // Positions annoncées par l'hôte.
        for (id, _, state, _) in &new.players {
            let Some(p) = players.get(id) else { continue };
            let loaded = world.contains(minelib_world::ChunkPos::containing(
                p.pos.x.floor() as i32,
                p.pos.z.floor() as i32,
            ));
            if skip.contains(id) || !loaded || (*id != 0 && !last_seq.contains_key(id)) {
                continue;
            }
            let d = (p.pos - Vec3::from(state.pos)).length();
            if d > 0.05 {
                out.push(Conflict {
                    player: *id,
                    seq: last_seq.get(id).copied().unwrap_or(0),
                    host_pos: state.pos,
                    audit_pos: p.pos.to_array(),
                    // L'hôte qui ne suit pas ses propres entrées : preuve.
                    proof: *id == 0,
                    why: "position annoncée différente",
                });
            }
        }
        // Règles : vies, argent, éliminations, scores, bombe… à l'identique.
        if verifiable {
            if game::encode(&m) != game::encode(&new.m) {
                wrong.push("règles (dégâts, éliminations, argent, scores)");
            } else {
                for e in &events {
                    if let Ev::GameOver(s) = e {
                        self.results.push(s.clone());
                    }
                }
            }
        }
        wrong.sort();
        wrong.dedup();
        for w in wrong {
            out.push(Conflict::proof(w));
        }
        for c in &out {
            log::warn!("vérificateur : {} (joueur {})", c.why, c.player);
        }
        out
    }
}

/// Où est un joueur, pour les règles.
fn place(p: &Player) -> Place {
    Place {
        feet: p.pos,
        on_ground: p.on_ground,
        speed: Vec3::new(p.vel.x, 0.0, p.vel.z).length(),
    }
}

impl Conflict {
    /// L'hôte a triché sans appel possible.
    fn proof(why: &'static str) -> Self {
        Conflict {
            player: 0,
            seq: 0,
            host_pos: [0.0; 3],
            audit_pos: [0.0; 3],
            proof: true,
            why,
        }
    }
}

/// Grenade HE et molotov (rang dans `equip::Nade::ALL`).
const HE: u8 = 0;
const MOLOTOV: u8 = 4;

/// Rejoue le journal entre deux copies et compare (vérificateur neuf).
#[cfg(test)]
fn audit(prev: &Backup, new: &Backup, world: &World) -> Vec<Conflict> {
    Auditor::default().audit(prev, new, world)
}

/// Taille des morceaux de la copie envoyée au garant de secours.
const BACKUP_PART: usize = 32 * 1024;

/// Garant de secours : état reçu toutes les…
const BACKUP_EVERY: u32 = 256;

/// Positions de tous les joueurs à un pas : (joueur, pieds, accroupi).
type Frame = Vec<(u64, Vec3, f32)>;

pub struct Host {
    link: Sender<Inbound>,
    pseudo: String,
    map_sig: [u8; 32],
    spawn: Vec3,
    peers: HashMap<u64, Remote>,
    tick: u32,
    /// Positions (joueur, pieds, accroupi) à chaque pas récent ; joueur 0 = l'hôte.
    history: VecDeque<(u32, Frame)>,
    m: Match,
    rng: u32,
    /// Réapparitions demandées par les règles, faites au prochain pas (monde nécessaire).
    spawns: Vec<(u64, bool)>,
    local_pos: Vec3,
    /// État du joueur de l'hôte au dernier pas (ses yeux, pour ses tirs).
    local_state: SimState,
    /// Brûlure en cours de chacun (fraction de point).
    burns: BTreeMap<u64, f32>,
    /// Touches maintenues de l'hôte (notées au journal quand elles changent).
    local_keys: (bool, bool),
    /// Copie à envoyer au prochain pas (fin de partie).
    backup_now: bool,
    /// Copies envoyées d'elles-mêmes (les essais les prennent à la main).
    auto_backup: bool,
    out: Vec<Local>,
    /// Armes au sol, et armes portées par chacun (principale, secondaire).
    ground: Vec<GroundGun>,
    next_gun: u32,
    ground_dirty: bool,
    loadouts: HashMap<u64, (u8, u8)>,
    /// Garant de secours actuel, et celui désigné par le relais (garant indépendant).
    backup: Option<u64>,
    designated: Option<u64>,
    /// L'hôte joue lui-même (sinon : garant indépendant, sans personnage).
    playing: bool,
    /// Journal des coups depuis la dernière copie, et tête de la chaîne.
    log: Vec<(u32, u64, Move)>,
    chain: [u8; 32],
    /// Dernier tir de chaque joueur (pas) : un tireur s'entend même caché.
    last_shot: HashMap<u64, u32>,
}

pub struct Guest {
    link: Sender<Inbound>,
    you: Option<u64>,
    seq: u32,
    /// Entrées pas encore confirmées par l'hôte, avec l'état prédit après chacune.
    pending: VecDeque<(u32, NetInput, SimState)>,
    outbox: Vec<NetInput>,
    outbox_first: u32,
    /// Derniers états reçus des autres joueurs (pas, joueurs).
    snaps: VecDeque<(u32, Vec<NetPlayer>)>,
    /// Dernier état reçu pour soi : (pas confirmé, état, vie, vivant).
    latest: Option<(u32, SimState, u8, bool)>,
    /// Horloge de l'hôte estimée (pas, avec fraction).
    clock: f32,
    weapon: u8,
    hud: MatchHud,
    rows: Vec<ScoreRow>,
    history: Vec<(u8, u8)>,
    keys: (bool, bool),
    ground: Vec<GroundGun>,
    loadout: (u8, u8),
    map_sig: [u8; 32],
    /// Dernier état complet reçu (si on est le garant de secours), et morceaux en cours.
    backup: Option<Vec<u8>>,
    parts: Vec<Vec<u8>>,
    /// Garant indépendant en attente : vérifie chaque copie en rejouant les coups.
    auditor: bool,
    audit: Auditor,
    /// Désaccords trouvés, à envoyer au relais.
    pub conflicts: Vec<Conflict>,
    /// Présentation renvoyée tant que l'hôte n'a pas répondu.
    hello_timer: f32,
    /// Positions prédites par numéro d'entrée (pour juger un désaccord sur soi).
    predicted: VecDeque<(u32, [f32; 3])>,
}

pub enum NetGame {
    Host(Box<Host>),
    Guest(Box<Guest>),
}

fn send(link: &Sender<Inbound>, req: &Request) {
    transport::push(link, req);
}

/// Boîte (min, max) d'un joueur et hauteur de sa tête.
fn hitbox(feet: Vec3, crouch: f32) -> (Vec3, Vec3, f32) {
    let h = HEIGHT - 0.3 * crouch;
    let min = feet - Vec3::new(HALF_WIDTH, 0.0, HALF_WIDTH);
    let max = feet + Vec3::new(HALF_WIDTH, h, HALF_WIDTH);
    (min, max, feet.y + h - 0.3)
}

/// Distance du rayon à la boîte, s'il la touche.
fn ray_box(o: Vec3, d: Vec3, min: Vec3, max: Vec3) -> Option<f32> {
    let inv = Vec3::new(1.0 / d.x, 1.0 / d.y, 1.0 / d.z);
    let t1 = (min - o) * inv;
    let t2 = (max - o) * inv;
    let near = t1.min(t2).max_element();
    let far = t1.max(t2).min_element();
    (far >= near.max(0.0)).then_some(near.max(0.0))
}

/// Dégâts d'un coup selon l'endroit touché : tête ×4, jambes ×0,75 (balles).
fn damage_at(weapon: u8, hit_y: f32, feet_y: f32, head_y: f32) -> (u32, Hurt) {
    if weapon == KNIFE_LIGHT || weapon == KNIFE_HEAVY {
        let d = if weapon == KNIFE_HEAVY { 55 } else { 34 };
        return (d, Hurt::Knife);
    }
    let base = WEAPONS.get(weapon as usize).map_or(30, |w| w.damage) as f32;
    let (head, legs) = (hit_y >= head_y, hit_y < feet_y + 0.75);
    let d = if head {
        base * 4.0
    } else if legs {
        base * 0.75
    } else {
        base
    };
    (d as u32, Hurt::Bullet { weapon, head, legs })
}

/// Ligne du fil des éliminations.
pub fn kill_line(killer: &str, victim: &str, weapon: u8, head: bool) -> String {
    let w = match weapon {
        cs::W_KNIFE | KNIFE_HEAVY => "couteau",
        cs::W_HE => "grenade",
        cs::W_FIRE => "feu",
        cs::W_BOMB => "bombe",
        cs::W_WORLD => "chute",
        w => WEAPONS.get(w as usize).map_or("?", |w| w.name),
    };
    let hs = if head { " (tête)" } else { "" };
    if killer.is_empty() {
        format!("[{w}] {victim}")
    } else {
        format!("{killer} [{w}{hs}] {victim}")
    }
}

impl NetGame {
    pub fn host(
        link: Sender<Inbound>,
        pseudo: String,
        map: FpsMap,
        map_sig: [u8; 32],
        spawn: Vec3,
    ) -> Self {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(1, |d| d.subsec_nanos());
        let mut m = Match::new(map, map_sig, seed);
        // Carte sans zones : départs de part et d'autre du point d'apparition du monde.
        m.fallback_spawns = Some([spawn - Vec3::X * 10.0, spawn + Vec3::X * 10.0]);
        let mut out = Vec::new();
        let ev = m.join(0, &pseudo);
        let mut h = Host {
            link,
            pseudo,
            map_sig,
            spawn,
            peers: HashMap::new(),
            tick: 0,
            history: VecDeque::new(),
            m,
            rng: seed | 1,
            spawns: Vec::new(),
            local_pos: spawn,
            local_state: Player::new(spawn).net_state(),
            burns: BTreeMap::new(),
            local_keys: (false, false),
            backup_now: false,
            auto_backup: true,
            out: Vec::new(),
            ground: Vec::new(),
            next_gun: 1,
            ground_dirty: false,
            loadouts: HashMap::new(),
            backup: None,
            designated: None,
            playing: true,
            log: Vec::new(),
            chain: [0; 32],
            last_shot: HashMap::new(),
        };
        h.events(ev, &mut out);
        h.out = out;
        NetGame::Host(Box::new(h))
    }

    /// Garant indépendant : serveur de la partie sans personnage (il ne joue pas).
    pub fn headless(
        link: Sender<Inbound>,
        pseudo: String,
        map: FpsMap,
        map_sig: [u8; 32],
        spawn: Vec3,
    ) -> Self {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(1, |d| d.subsec_nanos());
        let mut g = NetGame::host(link, pseudo, map.clone(), map_sig, spawn);
        if let NetGame::Host(h) = &mut g {
            h.m = Match::new(map, map_sig, seed);
            h.m.fallback_spawns = Some([spawn - Vec3::X * 10.0, spawn + Vec3::X * 10.0]);
            h.playing = false;
            h.out.clear();
            h.spawns.clear();
        }
        g
    }

    /// Garant indépendant en attente : reçoit les copies de la partie, les vérifie, et
    /// reprend la partie si l'hôte part (ou triche).
    pub fn standby(link: Sender<Inbound>, map_sig: [u8; 32]) -> Self {
        let mut g = Self::guest_quiet(link, map_sig);
        if let NetGame::Guest(x) = &mut g {
            x.auditor = true;
            x.audit.witnessing = true;
            x.hello_timer = f32::INFINITY;
        }
        g
    }

    fn guest_quiet(link: Sender<Inbound>, map_sig: [u8; 32]) -> Self {
        NetGame::Guest(Box::new(Guest {
            link,
            you: None,
            seq: 0,
            pending: VecDeque::new(),
            outbox: Vec::new(),
            outbox_first: 1,
            snaps: VecDeque::new(),
            latest: None,
            clock: 0.0,
            weapon: 255,
            hud: MatchHud::default(),
            rows: Vec::new(),
            history: Vec::new(),
            keys: (false, false),
            ground: Vec::new(),
            loadout: (255, 255),
            map_sig,
            backup: None,
            parts: Vec::new(),
            auditor: false,
            audit: Auditor::default(),
            conflicts: Vec::new(),
            hello_timer: 1.0,
            predicted: VecDeque::new(),
        }))
    }

    /// Garant en attente : a reçu au moins une copie (prêt à reprendre).
    pub fn has_backup(&self) -> bool {
        matches!(self, NetGame::Guest(g) if g.backup.is_some())
    }

    /// Positions des joueurs (garant : chunks à garder chargés autour d'eux).
    pub fn positions(&self) -> Vec<Vec3> {
        match self {
            NetGame::Host(h) => h.peers.values().map(|r| r.player.pos).collect(),
            NetGame::Guest(g) => g
                .backup
                .as_deref()
                .and_then(game::decode::<Backup>)
                .map(|b| b.players.iter().map(|p| Vec3::from(p.2.pos)).collect())
                .unwrap_or_default(),
        }
    }

    /// Désaccords trouvés par ce garant vérificateur (à envoyer au relais).
    pub fn take_conflicts(&mut self) -> Vec<Conflict> {
        match self {
            NetGame::Guest(g) => std::mem::take(&mut g.conflicts),
            NetGame::Host(_) => Vec::new(),
        }
    }

    /// Demande d'un joueur à l'hôte, montrée par le relais au vérificateur.
    pub fn witness(&mut self, from: u64, data: &[u8]) {
        if let NetGame::Guest(g) = self {
            if g.auditor {
                if let Some(msg) = game::decode::<ToHost>(data) {
                    g.audit.witness(from, msg);
                }
            }
        }
    }

    /// Fins de partie trouvées par le vérificateur (résultat à envoyer au relais).
    pub fn take_results(&mut self) -> Vec<MatchSummary> {
        match self {
            NetGame::Guest(g) => std::mem::take(&mut g.audit.results),
            NetGame::Host(_) => Vec::new(),
        }
    }

    /// Le relais désigne un garant indépendant comme garant de secours.
    pub fn designate_backup(&mut self, peer: u64) {
        if let NetGame::Host(h) = self {
            h.designated = Some(peer);
        }
    }

    /// Position prédite de soi après l'entrée `seq` (pour juger un désaccord).
    pub fn predicted_at(&self, seq: u32) -> Option<[f32; 3]> {
        match self {
            NetGame::Guest(g) => g.predicted.iter().find(|(s, _)| *s == seq).map(|p| p.1),
            NetGame::Host(_) => None,
        }
    }

    /// Ce jeu était l'hôte et un garant indépendant reprend la partie : il devient invité.
    pub fn demote(&mut self) {
        if let NetGame::Host(h) = self {
            let link = h.link.clone();
            let sig = h.map_sig;
            *self = NetGame::guest(link, sig);
        }
    }

    pub fn guest(link: Sender<Inbound>, map_sig: [u8; 32]) -> Self {
        send(
            &link,
            &Request::ToHost {
                data: game::encode(&ToHost::Hello { map_hash: map_sig }),
            },
        );
        Self::guest_quiet(link, map_sig)
    }

    pub fn is_host(&self) -> bool {
        matches!(self, NetGame::Host(_))
    }

    /// Un joueur arrive (hôte).
    pub fn peer_joined(&mut self, peer: u64, pseudo: String) {
        if let NetGame::Host(h) = self {
            let player = Player::new(h.spawn);
            h.peers.insert(
                peer,
                Remote {
                    pseudo: pseudo.clone(),
                    player,
                    queue: VecDeque::new(),
                    ack: 0,
                    weapon: 255,
                    ready: false,
                    fall_from: None,
                },
            );
        }
    }

    pub fn peer_left(&mut self, peer: u64) {
        if let NetGame::Host(h) = self {
            if let Some(r) = h.peers.remove(&peer) {
                h.record(peer, Move::Rule(Rule::Leave));
                h.burns.remove(&peer);
                h.m.leave(peer, Some(r.player.pos));
                let line = format!("{} a quitté la partie.", r.pseudo);
                h.broadcast_chat(&line);
                h.out.push(Local::Chat(line));
            }
        }
    }

    /// Données reçues du relais. Renvoie ce que la session locale doit faire.
    pub fn receive(&mut self, from: Option<u64>, data: &[u8], world: &World) -> Vec<Local> {
        match self {
            NetGame::Host(h) => match (from, game::decode::<ToHost>(data)) {
                (Some(peer), Some(msg)) => h.receive(peer, msg, world),
                _ => Vec::new(),
            },
            NetGame::Guest(g) => match game::decode::<ToPeer>(data) {
                Some(ToPeer::Backup { part, of, data }) => {
                    if part == 0 {
                        g.parts.clear();
                    }
                    if part as usize != g.parts.len() || of == 0 {
                        g.parts.clear();
                        return Vec::new();
                    }
                    g.parts.push(data);
                    if g.parts.len() < of as usize {
                        return Vec::new();
                    }
                    let data = std::mem::take(&mut g.parts).concat();
                    if g.auditor {
                        let prev = g.backup.as_deref().and_then(game::decode::<Backup>);
                        let new = game::decode::<Backup>(&data);
                        match (prev, new) {
                            (Some(p), Some(n)) => g.conflicts.extend(g.audit.audit(&p, &n, world)),
                            _ => g.audit.first_copy(),
                        }
                    }
                    g.backup = Some(data);
                    Vec::new()
                }
                Some(msg) => g.receive(msg),
                None => Vec::new(),
            },
        }
    }

    /// Invité : avant les pas de l'image, reprend l'état de l'hôte s'il diffère de la
    /// prédiction, puis rejoue les entrées qu'il n'a pas encore prises en compte.
    pub fn reconcile(&mut self, local: &mut Player, world: &World, dt: f32) {
        let NetGame::Guest(g) = self else { return };
        let Some((ack, state, _, alive)) = g.latest.take() else {
            return;
        };
        if !alive {
            g.pending.clear();
            return;
        }
        while g.pending.front().is_some_and(|(s, _, _)| *s < ack) {
            g.pending.pop_front();
        }
        let predicted = g.pending.front().filter(|(s, _, _)| *s == ack).map(|p| p.2);
        if let Some(p) = predicted {
            g.pending.pop_front();
            let err = (Vec3::from(p.pos) - Vec3::from(state.pos)).length();
            if err <= CORRECT {
                return;
            }
        }
        // Correction : état de l'hôte, puis les entrées suivantes rejouées.
        let (yaw, pitch) = (local.yaw, local.pitch);
        local.set_net_state(&state);
        for (_, input, after) in g.pending.iter_mut() {
            step(local, world, input, dt);
            *after = local.net_state();
        }
        local.yaw = yaw;
        local.pitch = pitch;
    }

    /// Le joueur local ne peut pas bouger (départ de manche, pose ou désamorçage).
    pub fn frozen(&self) -> bool {
        match self {
            NetGame::Host(h) => h.m.frozen() || h.m.busy(0),
            NetGame::Guest(g) => {
                matches!(
                    g.hud.phase,
                    game::phase::FREEZE | game::phase::HALFTIME | game::phase::GAME_OVER
                ) || g.hud.progress > 0.0
            }
        }
    }

    /// Après chaque pas du joueur local.
    pub fn on_tick(&mut self, world: &World, local: &Player, input: &MoveInput, dt: f32) {
        match self {
            NetGame::Host(h) => {
                if h.playing {
                    let mv = Move::Own(to_net(input, local.yaw, local.pitch), local.net_state());
                    h.record(0, mv);
                }
                h.tick(world, local, dt)
            }
            NetGame::Guest(g) => {
                g.seq += 1;
                let n = to_net(input, local.yaw, local.pitch);
                g.pending.push_back((g.seq, n, local.net_state()));
                g.predicted.push_back((g.seq, local.pos.to_array()));
                if g.predicted.len() > 2048 {
                    g.predicted.pop_front();
                }
                if g.pending.len() > 1024 {
                    g.pending.pop_front();
                }
                if g.outbox.is_empty() {
                    g.outbox_first = g.seq;
                }
                g.outbox.push(n);
            }
        }
    }

    /// Fin d'image : envoie les entrées (invité), juge les tirs et les grenades (hôte).
    pub fn end_frame(
        &mut self,
        world: &World,
        shots: Vec<(u8, Vec3, Vec<Vec3>)>,
        weapon: u8,
        nades: NadeOut,
        dt: f32,
    ) -> Vec<Local> {
        match self {
            NetGame::Host(h) => {
                let mut out = std::mem::take(&mut h.out);
                for (w, origin, dirs) in shots {
                    let msg = ToHost::Fire {
                        weapon: w,
                        origin: origin.to_array(),
                        dirs: dirs.iter().map(|d| d.to_array()).collect(),
                        view_tick: h.tick,
                    };
                    out.extend(h.request(0, msg, world));
                }
                for (nade, pos, vel) in nades.throws {
                    h.record(
                        0,
                        Move::Rule(Rule::Msg(ToHost::Throw {
                            nade,
                            pos: pos.to_array(),
                            vel: vel.to_array(),
                        })),
                    );
                    h.broadcast(&ToPeer::Throw {
                        owner: 0,
                        nade,
                        pos: pos.to_array(),
                        vel: vel.to_array(),
                    });
                }
                h.nade_damage(world, &nades.blasts, &nades.fires, dt, &mut out);
                out
            }
            NetGame::Guest(g) => {
                g.weapon = weapon;
                g.clock += dt / crate::session::SIM_TICK;
                // L'hôte (garant qui démarre) n'a pas encore répondu : on se représente.
                g.hello_timer -= dt;
                if g.hello_timer <= 0.0 {
                    g.hello_timer = 1.0;
                    g.to_host(&ToHost::Hello {
                        map_hash: g.map_sig,
                    });
                }
                if !g.outbox.is_empty() {
                    let inputs = std::mem::take(&mut g.outbox);
                    let msg = ToHost::Inputs {
                        first_seq: g.outbox_first,
                        inputs,
                        weapon,
                    };
                    g.to_host(&msg);
                }
                let view_tick = (g.clock - INTERP_TICKS).max(0.0) as u32;
                for (w, origin, dirs) in shots {
                    g.to_host(&ToHost::Fire {
                        weapon: w,
                        origin: origin.to_array(),
                        dirs: dirs.iter().map(|d| d.to_array()).collect(),
                        view_tick,
                    });
                }
                for (nade, pos, vel) in nades.throws {
                    g.to_host(&ToHost::Throw {
                        nade,
                        pos: pos.to_array(),
                        vel: vel.to_array(),
                    });
                }
                Vec::new()
            }
        }
    }

    /// Les autres joueurs à dessiner.
    pub fn views(&self) -> Vec<View> {
        match self {
            NetGame::Host(h) => h
                .peers
                .iter()
                .filter(|(_, r)| r.ready)
                .map(|(&id, r)| {
                    let p = h.m.players.get(&id);
                    View {
                        pseudo: r.pseudo.clone(),
                        pos: r.player.pos,
                        yaw: r.player.yaw,
                        pitch: r.player.pitch,
                        crouch: r.player.crouch,
                        speed: Vec3::new(r.player.vel.x, 0.0, r.player.vel.z).length(),
                        alive: p.is_some_and(|p| p.alive),
                        weapon: r.weapon,
                        team: p.map_or(CT, |p| p.team),
                        flags: h.m.flags(id),
                    }
                })
                .collect(),
            NetGame::Guest(g) => g.views(),
        }
    }

    /// Tableau des scores.
    pub fn rows(&self) -> Vec<ScoreRow> {
        match self {
            NetGame::Host(h) => h.m.rows(),
            NetGame::Guest(g) => g.rows.clone(),
        }
    }

    /// Vainqueur (équipe de départ) et raison de chaque manche jouée.
    pub fn round_history(&self) -> Vec<(u8, u8)> {
        match self {
            NetGame::Host(h) => h.m.history.clone(),
            NetGame::Guest(g) => g.history.clone(),
        }
    }

    /// Mon identifiant dans le tableau des scores.
    pub fn me(&self) -> u64 {
        match self {
            NetGame::Host(_) => 0,
            NetGame::Guest(g) => g.you.unwrap_or(u64::MAX),
        }
    }

    pub fn hud(&self) -> MatchHud {
        match self {
            NetGame::Host(h) => h.m.hud(0, Some(h.local_pos)),
            NetGame::Guest(g) => g.hud,
        }
    }

    /// Pseudos des joueurs de la partie (complétion de `/report`).
    pub fn pseudos(&self) -> Vec<String> {
        self.rows().into_iter().map(|r| r.pseudo).collect()
    }

    /// Hôte : dégâts au joueur local hors tir (chute).
    pub fn local_hurt(&mut self, amount: u32) -> Vec<Local> {
        let mut out = Vec::new();
        if let NetGame::Host(h) = self {
            h.record(0, Move::Rule(Rule::SelfHurt(amount)));
            let ev = h.m.hurt(None, 0, amount, Hurt::World);
            h.events(ev, &mut out);
        }
        out
    }

    /// Achat (le code d'article de `game::ITEM_…`).
    pub fn buy(&mut self, item: u16) -> Vec<Local> {
        let mut out = Vec::new();
        match self {
            NetGame::Host(h) => {
                h.record(0, Move::Rule(Rule::Msg(ToHost::Buy(item))));
                match h.m.buy(0, item, Some(h.local_pos)) {
                    Ok(()) => out.push(Local::Give(item)),
                    Err(e) => out.push(Local::Notice(e)),
                }
            }
            NetGame::Guest(g) => g.to_host(&ToHost::Buy(item)),
        }
        out
    }

    /// Touches maintenues : poser la bombe, désamorcer.
    pub fn keys(&mut self, plant: bool, defuse: bool) {
        match self {
            NetGame::Host(h) => {
                if h.local_keys != (plant, defuse) {
                    h.local_keys = (plant, defuse);
                    h.record(0, Move::Rule(Rule::Msg(ToHost::Action { plant, defuse })));
                    h.m.keys(0, plant, defuse);
                }
            }
            NetGame::Guest(g) => {
                if g.keys != (plant, defuse) {
                    g.keys = (plant, defuse);
                    g.to_host(&ToHost::Action { plant, defuse });
                }
            }
        }
    }

    /// L'hôte est parti et ce jeu (garant de secours) reprend la partie là où elle en
    /// était à la dernière copie reçue. Les autres joueurs se présentent de nouveau.
    pub fn promote(
        &mut self,
        pseudo: String,
        members: &[(u64, String)],
        local: &Player,
        map: FpsMap,
        old_host: Option<u64>,
    ) -> bool {
        let NetGame::Guest(g) = self else {
            return false;
        };
        let link = g.link.clone();
        let me = g.you.unwrap_or(u64::MAX);
        let backup = g.backup.as_deref().and_then(game::decode::<Backup>);
        let map_sig = g.map_sig;
        let Some(b) = backup else {
            // Pas encore de copie : nouvelle partie (échauffement) sur la même carte.
            let mut h = NetGame::host(link, pseudo, map, map_sig, local.pos);
            for (id, p) in members {
                h.peer_joined(*id, p.clone());
            }
            *self = h;
            return false;
        };
        let mut m = b.m;
        // L'ancien hôte reste au résultat (parti) ; ce jeu devient le joueur 0.
        // Ancien hôte : parti (reste au résultat) ou resté (il devient un invité).
        let gone = old_host.unwrap_or(u64::MAX - 1);
        m.rename(0, Some(gone));
        if old_host.is_none() {
            if let Some(p) = m.players.get_mut(&gone) {
                p.present = false;
                p.alive = false;
            }
        }
        m.rename(me, Some(0));
        let mut peers = HashMap::new();
        for (id, p) in members {
            let key = if Some(*id) == old_host { 0 } else { *id };
            let state = b.players.iter().find(|q| q.0 == key);
            let mut player = Player::new(Vec3::from(b.spawn));
            if let Some((_, _, s, _)) = state {
                player.set_net_state(s);
            }
            let fall_from = b.falls.iter().find(|f| f.0 == key).map(|f| f.1);
            peers.insert(
                *id,
                Remote {
                    pseudo: p.clone(),
                    player,
                    queue: VecDeque::new(),
                    ack: 0,
                    weapon: state.map_or(255, |q| q.3),
                    ready: false,
                    fall_from,
                },
            );
        }
        let mut loadouts: HashMap<u64, (u8, u8)> = b.loadouts.into_iter().collect();
        let old = loadouts.remove(&0);
        if let (Some(o), Some(l)) = (old_host, old) {
            loadouts.insert(o, l);
        }
        if let Some(l) = loadouts.remove(&me) {
            loadouts.insert(0, l);
        }
        *self = NetGame::Host(Box::new(Host {
            link,
            pseudo,
            map_sig,
            spawn: Vec3::from(b.spawn),
            peers,
            tick: b.tick,
            history: VecDeque::new(),
            m,
            rng: b.tick | 1,
            spawns: Vec::new(),
            local_pos: local.pos,
            local_state: local.net_state(),
            burns: b.burns.iter().copied().collect(),
            local_keys: (false, false),
            backup_now: false,
            auto_backup: true,
            out: Vec::new(),
            ground: b.ground,
            next_gun: b.next_gun,
            ground_dirty: true,
            loadouts,
            backup: None,
            designated: None,
            playing: true,
            log: Vec::new(),
            chain: b.chain,
            last_shot: HashMap::new(),
        }));
        if let NetGame::Host(h) = self {
            h.playing = h.m.players.contains_key(&0);
        }
        let _ = b.host_pseudo;
        true
    }

    /// Un autre joueur reprend la partie : on se présente à lui.
    pub fn host_changed(&mut self) {
        if let NetGame::Guest(g) = self {
            g.pending.clear();
            g.outbox.clear();
            g.latest = None;
            g.snaps.clear();
            g.keys = (false, false);
            g.loadout = (255, 255);
            g.backup = None;
            g.audit.reset();
            // Un garant en attente n'est pas un joueur : il ne se présente pas.
            if !g.auditor {
                g.to_host(&ToHost::Hello {
                    map_hash: g.map_sig,
                });
            }
        }
    }

    /// Armes au sol.
    pub fn ground(&self) -> Vec<GroundGun> {
        match self {
            NetGame::Host(h) => h.ground.clone(),
            NetGame::Guest(g) => g.ground.clone(),
        }
    }

    /// Armes portées (envoyées à l'hôte quand elles changent).
    pub fn set_loadout(&mut self, primary: u8, secondary: u8) {
        match self {
            NetGame::Host(h) => {
                h.loadouts.insert(0, (primary, secondary));
            }
            NetGame::Guest(g) => {
                if g.loadout != (primary, secondary) {
                    g.loadout = (primary, secondary);
                    g.to_host(&ToHost::Loadout { primary, secondary });
                }
            }
        }
    }

    /// Arme lâchée (touche G).
    pub fn drop_weapon(&mut self, weapon: u8) {
        match self {
            NetGame::Host(h) => {
                let at = h.local_pos;
                h.drop_gun(0, weapon, at);
            }
            NetGame::Guest(g) => g.to_host(&ToHost::Drop { weapon }),
        }
    }

    /// Ramasser l'arme au sol `id`.
    pub fn pickup(&mut self, id: u32) -> Vec<Local> {
        match self {
            NetGame::Host(h) => h.pickup(0, id),
            NetGame::Guest(g) => {
                g.to_host(&ToHost::Pickup { id });
                Vec::new()
            }
        }
    }

    /// Changer d'équipe pendant l'échauffement.
    pub fn choose_team(&mut self, team: u8) {
        match self {
            NetGame::Host(h) => {
                h.record(0, Move::Rule(Rule::Msg(ToHost::Team(team))));
                if let Some(ev) = h.m.switch_team(0, team) {
                    let mut out = Vec::new();
                    h.events(vec![ev], &mut out);
                    h.out.extend(out);
                }
            }
            NetGame::Guest(g) => g.to_host(&ToHost::Team(team)),
        }
    }

    pub fn chat(&mut self, text: &str) {
        match self {
            NetGame::Host(h) => {
                let line = format!("<{}> {text}", h.pseudo);
                h.broadcast_chat(&line);
            }
            NetGame::Guest(g) => g.to_host(&ToHost::Chat(text.to_string())),
        }
    }
}

impl Host {
    fn rand(&mut self) -> u32 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 17;
        self.rng ^= self.rng << 5;
        self.rng
    }

    fn send_to(&self, peer: u64, msg: &ToPeer) {
        send(
            &self.link,
            &Request::ToPeer {
                peer,
                data: game::encode(msg),
            },
        );
    }

    fn broadcast(&self, msg: &ToPeer) {
        send(
            &self.link,
            &Request::Broadcast {
                data: game::encode(msg),
            },
        );
    }

    fn broadcast_chat(&self, text: &str) {
        self.broadcast(&ToPeer::Chat {
            from: String::new(),
            text: text.to_string(),
        });
    }

    fn pseudo_of(&self, id: u64) -> String {
        if id == 0 {
            self.pseudo.clone()
        } else {
            self.peers.get(&id).map_or_else(
                || {
                    self.m
                        .players
                        .get(&id)
                        .map_or(String::new(), |p| p.pseudo.clone())
                },
                |r| r.pseudo.clone(),
            )
        }
    }

    fn feet_of(&self, id: u64) -> Option<Vec3> {
        if id == 0 {
            Some(self.local_pos)
        } else {
            self.peers
                .get(&id)
                .filter(|r| r.ready)
                .map(|r| r.player.pos)
        }
    }

    /// Point d'apparition libre dans la zone de l'équipe : sol dessous, deux blocs d'air,
    /// à au moins 1,5 bloc des autres joueurs.
    fn spawn_point(&mut self, world: &World, team: u8, taken: &[Vec3]) -> Vec3 {
        let zone = if team == CT {
            self.m.map.ct
        } else {
            self.m.map.t
        };
        let free = |p: Vec3, taken: &[Vec3]| {
            taken
                .iter()
                .all(|q| Vec3::new(p.x - q.x, 0.0, p.z - q.z).length() >= SPAWN_GAP)
        };
        // Chunks pas encore chargés ici : générés à part pour savoir où est le sol.
        let mut probe: HashMap<minelib_world::ChunkPos, minelib_world::Chunk> = HashMap::new();
        let mut solid = |x: i32, y: i32, z: i32| -> bool {
            if !(0..minelib_world::CHUNK_HEIGHT as i32).contains(&y) {
                return false;
            }
            let pos = minelib_world::ChunkPos::containing(x, z);
            if world.contains(pos) {
                return world.get(x, y, z).is_solid();
            }
            let c = probe.entry(pos).or_insert_with(|| world.make_chunk(pos));
            c.get(
                x.rem_euclid(minelib_world::CHUNK_SIZE as i32) as usize,
                y as usize,
                z.rem_euclid(minelib_world::CHUNK_SIZE as i32) as usize,
            )
            .is_solid()
        };
        let mut stand =
            |x: i32, y: i32, z: i32| solid(x, y - 1, z) && !solid(x, y, z) && !solid(x, y + 1, z);
        if let Some(z) = zone {
            let w = (z.max[0] - z.min[0] + 1).max(1) as u32;
            let d = (z.max[2] - z.min[2] + 1).max(1) as u32;
            for _ in 0..300 {
                let fx = (self.rand() % 1000) as f32 / 1000.0;
                let fz = (self.rand() % 1000) as f32 / 1000.0;
                let x = z.min[0] as f32 + 0.3 + fx * (w as f32 - 0.6);
                let zz = z.min[2] as f32 + 0.3 + fz * (d as f32 - 0.6);
                let (bx, bz) = (x.floor() as i32, zz.floor() as i32);
                for y in (z.min[1]..=z.max[1] + 1).rev() {
                    if stand(bx, y, bz) {
                        let p = Vec3::new(x, y as f32, zz);
                        if free(p, taken) {
                            return p;
                        }
                        break;
                    }
                }
            }
            let c = z.center();
            return Vec3::new(c[0], c[1], c[2]);
        }
        // Carte sans zones : de part et d'autre du point d'apparition du monde.
        let side = if team == CT { -1.0 } else { 1.0 };
        let base = self.spawn + Vec3::new(10.0 * side, 0.0, 0.0);
        for _ in 0..300 {
            let a = (self.rand() % 360) as f32 * std::f32::consts::PI / 180.0;
            let r = (self.rand() % 400) as f32 / 100.0;
            let x = base.x + a.cos() * r;
            let z = base.z + a.sin() * r;
            let (bx, bz) = (x.floor() as i32, z.floor() as i32);
            let Some(y) = (1..minelib_world::CHUNK_HEIGHT as i32 - 2)
                .rev()
                .find(|&y| stand(bx, y, bz))
            else {
                continue;
            };
            let p = Vec3::new(x, y as f32, z);
            if free(p, taken) {
                return p;
            }
        }
        base
    }

    /// Copie complète de la partie (et journal des coups depuis la précédente).
    fn make_backup(&mut self, local: &Player) -> Backup {
        let mut players = Vec::new();
        if self.playing {
            players.push((0, self.pseudo.clone(), local.net_state(), 255));
        }
        players.extend(
            self.peers
                .iter()
                .filter(|(_, r)| r.ready)
                .map(|(&id, r)| (id, r.pseudo.clone(), r.player.net_state(), r.weapon)),
        );
        Backup {
            m: self.m.clone(),
            host_pseudo: self.pseudo.clone(),
            players,
            ground: self.ground.clone(),
            next_gun: self.next_gun,
            loadouts: self.loadouts.iter().map(|(&k, &v)| (k, v)).collect(),
            tick: self.tick,
            spawn: self.spawn.to_array(),
            log: std::mem::take(&mut self.log),
            chain: self.chain,
            falls: self
                .peers
                .iter()
                .filter_map(|(&id, r)| r.fall_from.map(|f| (id, f)))
                .collect(),
            burns: self.burns.iter().map(|(&k, &v)| (k, v)).collect(),
        }
    }

    /// Note un coup au journal et à la chaîne.
    fn record(&mut self, who: u64, mv: Move) {
        self.chain = chain_step(&self.chain, self.tick, who, &mv);
        self.log.push((self.tick, who, mv));
        if self.log.len() > 100_000 {
            self.log.clear();
        }
    }

    /// Envoie l'état complet au garant de secours (le premier joueur prêt).
    fn send_backup(&mut self, local: &Player) {
        // Garant indépendant désigné par le relais, sinon le premier joueur prêt.
        let Some(b) = self.designated.or_else(|| {
            self.peers
                .iter()
                .filter(|(_, r)| r.ready)
                .map(|(&id, _)| id)
                .min()
        }) else {
            self.log.clear();
            return;
        };
        if self.backup != Some(b) {
            self.backup = Some(b);
            send(&self.link, &Request::SetBackup { peer: b });
        }
        let data = game::encode(&self.make_backup(local));
        let parts: Vec<&[u8]> = data.chunks(BACKUP_PART).collect();
        let of = parts.len() as u16;
        for (i, p) in parts.into_iter().enumerate() {
            self.send_to(
                b,
                &ToPeer::Backup {
                    part: i as u16,
                    of,
                    data: p.to_vec(),
                },
            );
        }
    }

    /// Pose une arme au sol (lâchée ou d'un mort).
    fn drop_gun(&mut self, id: u64, weapon: u8, at: Vec3) {
        if WEAPONS.get(weapon as usize).is_none() {
            return;
        }
        // Seulement une arme qu'il porte.
        let lo = self.loadouts.get(&id).copied().unwrap_or((255, 255));
        if lo.0 != weapon && lo.1 != weapon {
            return;
        }
        let e = self.loadouts.entry(id).or_insert((255, 255));
        if e.0 == weapon {
            e.0 = 255;
        } else {
            e.1 = 255;
        }
        if self.ground.len() >= 48 {
            self.ground.remove(0);
        }
        self.ground.push(GroundGun {
            id: self.next_gun,
            weapon,
            pos: (at + Vec3::Y * 0.05).to_array(),
        });
        self.next_gun += 1;
        self.ground_dirty = true;
    }

    /// Ramasse une arme : à portée, vivant ; l'arme du même emplacement est lâchée à
    /// la place (échange, comme dans CS).
    fn pickup(&mut self, id: u64, gun: u32) -> Vec<Local> {
        let mut out = Vec::new();
        let alive = self.m.players.get(&id).is_some_and(|p| p.alive);
        let Some(feet) = self.feet_of(id) else {
            return out;
        };
        let Some(k) = self
            .ground
            .iter()
            .position(|g| g.id == gun && (Vec3::from(g.pos) - feet).length() < 2.0)
        else {
            return out;
        };
        if !alive {
            return out;
        }
        let g = self.ground.remove(k);
        self.ground_dirty = true;
        let primary = WEAPONS[g.weapon as usize].primary();
        let lo = self.loadouts.get(&id).copied().unwrap_or((255, 255));
        let old = if primary { lo.0 } else { lo.1 };
        if old != 255 {
            self.drop_gun(id, old, feet);
        }
        let e = self.loadouts.entry(id).or_insert((255, 255));
        if primary {
            e.0 = g.weapon;
        } else {
            e.1 = g.weapon;
        }
        if id == 0 {
            out.push(Local::Give(g.weapon as u16));
        } else {
            self.send_to(
                id,
                &ToPeer::Give {
                    item: g.weapon as u16,
                },
            );
        }
        out
    }

    /// Fait réapparaître ceux que les règles ont demandés.
    fn do_spawns(&mut self, world: &World) {
        let spawns = std::mem::take(&mut self.spawns);
        let mut taken: Vec<Vec3> = Vec::new();
        // Les vivants qui ne réapparaissent pas occupent leur place.
        for (&id, p) in &self.m.players {
            if p.alive && !spawns.iter().any(|(s, _)| *s == id) {
                if let Some(f) = self.feet_of(id) {
                    taken.push(f);
                }
            }
        }
        for (id, reset) in spawns {
            let Some(team) = self.m.players.get(&id).map(|p| p.team) else {
                continue;
            };
            let at = self.spawn_point(world, team, &taken);
            taken.push(at);
            // Face au camp adverse.
            let target = {
                let other = if team == CT {
                    self.m.map.t
                } else {
                    self.m.map.ct
                };
                other.map(|z| Vec3::from(z.center()))
            };
            let yaw = target.map_or(0.0, |t| {
                let d = t - at;
                d.x.atan2(-d.z)
            });
            let mut p = Player::new(at);
            p.yaw = yaw;
            let state = p.net_state();
            self.record(id, Move::Set(state));
            if id == 0 {
                self.out.push(Local::Respawn { state, reset, team });
            } else if let Some(r) = self.peers.get_mut(&id) {
                r.player = p;
                r.queue.clear();
                r.fall_from = None;
                self.send_to(id, &ToPeer::Respawn { state, reset, team });
            }
        }
    }

    /// Traduit les événements des règles.
    fn events(&mut self, ev: Vec<Ev>, out: &mut Vec<Local>) {
        for e in ev {
            match e {
                Ev::Spawn { id, reset } => self.spawns.push((id, reset)),
                Ev::Killed {
                    victim,
                    killer,
                    weapon,
                    head,
                } => {
                    // Le mort lâche sa meilleure arme.
                    if let Some(at) = self.feet_of(victim) {
                        let lo = self.loadouts.get(&victim).copied().unwrap_or((255, 255));
                        let best = if lo.0 != 255 { lo.0 } else { lo.1 };
                        if best != 255 {
                            self.drop_gun(victim, best, at);
                        }
                    }
                    let k = killer.map(|k| self.pseudo_of(k)).unwrap_or_default();
                    let v = self.pseudo_of(victim);
                    self.broadcast(&ToPeer::Kill {
                        killer: k.clone(),
                        victim: v.clone(),
                        weapon,
                        head,
                    });
                    out.push(Local::Chat(kill_line(&k, &v, weapon, head)));
                }
                Ev::Hp(id) => {
                    if id == 0 {
                        out.push(Local::SetHp(self.m.players.get(&0).map_or(0, |p| p.hp)));
                    }
                }
                Ev::RoundEnd { winner, reason } => {
                    self.broadcast(&ToPeer::RoundEnd { winner, reason });
                    out.push(Local::RoundEnd { winner, reason });
                }
                Ev::Say(text) => {
                    self.broadcast_chat(&text);
                    out.push(Local::Notice(text));
                }
                Ev::GameOver(summary) => {
                    // Copie tout de suite : le vérificateur confirme le résultat.
                    self.backup_now = true;
                    self.broadcast(&ToPeer::GameOver {
                        summary: summary.clone(),
                    });
                    self.broadcast(&ToPeer::Scoreboard {
                        rows: self.m.rows(),
                        history: self.m.history.clone(),
                    });
                    out.push(Local::GameOver(summary));
                }
                Ev::Started => out.push(Local::Started),
                Ev::NewRound => {
                    self.ground.clear();
                    self.ground_dirty = true;
                }
            }
        }
    }

    /// Tir de `who` (0 = l'hôte) : noté au journal, puis jugé s'il est recevable.
    fn request(&mut self, who: u64, msg: ToHost, world: &World) -> Vec<Local> {
        let ToHost::Fire {
            weapon,
            origin,
            ref dirs,
            view_tick,
        } = msg
        else {
            return Vec::new();
        };
        let origin = Vec3::from(origin);
        let dirs: Vec<Vec3> = dirs
            .iter()
            .map(|d| Vec3::from(*d).normalize_or_zero())
            .collect();
        let eye = if who == 0 {
            self.playing.then(|| player_from(&self.local_state).eye())
        } else {
            self.peers
                .get(&who)
                .filter(|r| r.ready)
                .map(|r| r.player.eye())
        };
        let ok = fire_ok(&self.m, who, eye, origin, dirs.len());
        if who == 0 {
            self.record(0, Move::Rule(Rule::Msg(msg)));
        }
        if ok {
            self.fire(who, weapon, origin, &dirs, view_tick, world)
        } else {
            Vec::new()
        }
    }

    fn receive(&mut self, peer: u64, msg: ToHost, world: &World) -> Vec<Local> {
        let mut out = Vec::new();
        if !self.peers.contains_key(&peer) {
            return out;
        }
        // Tout ce qui change la partie passe au journal (le vérificateur le rejoue).
        if witnessed(&msg) {
            self.record(peer, Move::Rule(Rule::Msg(msg.clone())));
        }
        match msg {
            ToHost::Hello { map_hash } => {
                let ok = map_hash == self.map_sig;
                let pseudo = self.pseudo_of(peer);
                self.record(peer, Move::Rule(Rule::Hello { pseudo, ok }));
                if let Some(r) = self.peers.get_mut(&peer) {
                    r.ready = ok;
                }
                self.send_to(
                    peer,
                    &ToPeer::Welcome {
                        you: peer,
                        tick: self.tick,
                        map_ok: ok,
                    },
                );
                if ok {
                    self.send_to(
                        peer,
                        &ToPeer::Ground {
                            guns: self.ground.clone(),
                        },
                    );
                    let pseudo = self.pseudo_of(peer);
                    // Déjà dans la partie (reprise par un nouvel hôte) : il continue.
                    let back = self.m.players.get(&peer).is_some_and(|p| p.present);
                    if !back {
                        let ev = self.m.join(peer, &pseudo);
                        self.events(ev, &mut out);
                    }
                    if !self.m.started() {
                        // Combien en attendent-on encore.
                        let n = self.m.present();
                        let line = format!("{n} / {} joueurs.", cs::PLAYERS);
                        self.broadcast_chat(&line);
                        out.push(Local::Notice(line));
                    }
                }
            }
            ToHost::Inputs {
                first_seq,
                inputs,
                weapon,
            } => {
                if let Some(r) = self.peers.get_mut(&peer) {
                    r.weapon = weapon;
                    for (i, n) in inputs.into_iter().enumerate() {
                        let seq = first_seq + i as u32;
                        if seq > r.ack + r.queue.len() as u32 && r.queue.len() < 512 {
                            r.queue.push_back((seq, n));
                        }
                    }
                }
            }
            msg @ ToHost::Fire { .. } => out.extend(self.request(peer, msg, world)),
            ToHost::Chat(text) => {
                if let Some(r) = self.peers.get(&peer) {
                    let text: String = text.chars().take(200).collect();
                    let line = format!("<{}> {text}", r.pseudo);
                    self.broadcast_chat(&line);
                    out.push(Local::Chat(line));
                }
            }
            ToHost::Buy(item) => {
                let at = self.feet_of(peer);
                match self.m.buy(peer, item, at) {
                    Ok(()) => self.send_to(peer, &ToPeer::Give { item }),
                    Err(e) => self.send_to(
                        peer,
                        &ToPeer::Chat {
                            from: String::new(),
                            text: e,
                        },
                    ),
                }
            }
            ToHost::Action { plant, defuse } => self.m.keys(peer, plant, defuse),
            ToHost::Team(team) => {
                if let Some(ev) = self.m.switch_team(peer, team) {
                    self.events(vec![ev], &mut out);
                }
            }
            ToHost::Loadout { primary, secondary } => {
                self.loadouts.insert(peer, (primary, secondary));
            }
            ToHost::Drop { weapon } => {
                if let Some(at) = self.feet_of(peer) {
                    self.drop_gun(peer, weapon, at);
                }
            }
            ToHost::Pickup { id } => out.extend(self.pickup(peer, id)),
            ToHost::Throw { nade, pos, vel } => {
                let alive = self.m.players.get(&peer).is_some_and(|p| p.alive);
                let near = self
                    .peers
                    .get(&peer)
                    .is_some_and(|r| (r.player.eye() - Vec3::from(pos)).length() < 3.0);
                if alive && near && nade < 5 && Vec3::from(vel).length() < 40.0 {
                    let msg = ToPeer::Throw {
                        owner: peer,
                        nade,
                        pos,
                        vel,
                    };
                    for &id in self.peers.keys() {
                        if id != peer {
                            self.send_to(id, &msg);
                        }
                    }
                    out.push(Local::Thrown {
                        owner: peer,
                        nade,
                        pos: Vec3::from(pos),
                        vel: Vec3::from(vel),
                    });
                }
            }
        }
        out
    }

    fn tick(&mut self, world: &World, local: &Player, dt: f32) {
        self.tick += 1;
        self.local_pos = local.pos;
        self.local_state = local.net_state();
        let frozen = self.m.frozen();
        let mut falls: Vec<(u64, u32)> = Vec::new();
        let mut moves: Vec<(u64, Move)> = Vec::new();
        for (&id, r) in self.peers.iter_mut() {
            if !r.ready {
                continue;
            }
            let alive = self.m.players.get(&id).is_some_and(|p| p.alive);
            if !alive {
                // Mort : les entrées sont confirmées sans bouger.
                while let Some((seq, _)) = r.queue.pop_front() {
                    r.ack = seq;
                }
                continue;
            }
            let still_now = frozen || self.m.busy(id);
            // Une entrée par pas ; quelques-unes de plus si elles ont pris du retard.
            let n = if r.queue.len() > CATCH_UP {
                CATCH_UP
            } else {
                1
            };
            for _ in 0..n {
                if let Some((seq, mut input)) = r.queue.pop_front() {
                    if still_now {
                        input.forward = 0;
                        input.strafe = 0;
                        input.jump = false;
                        input.jump_pressed = false;
                    }
                    step(&mut r.player, world, &input, dt);
                    r.ack = seq;
                    moves.push((id, Move::Input(seq, input)));
                }
            }
            // Chute (comme pour le joueur local) et vide.
            if let Some(d) = fall_check(&r.player, &mut r.fall_from) {
                falls.push((id, d));
            }
        }
        // Coups dans l'ordre des joueurs (le vérificateur les rejoue de même).
        moves.sort_by_key(|m| m.0);
        for (id, mv) in moves {
            self.record(id, mv);
        }
        self.record(0, Move::Rule(Rule::Tick));
        falls.sort();
        let mut out = Vec::new();
        for (id, d) in falls {
            let ev = self.m.hurt(None, id, d, Hurt::World);
            self.events(ev, &mut out);
        }
        // Règles.
        let mut places = BTreeMap::new();
        if self.playing {
            places.insert(0, place(local));
        }
        for (&id, r) in self.peers.iter().filter(|(_, r)| r.ready) {
            places.insert(id, place(&r.player));
        }
        let ev = self.m.tick(dt, &places);
        self.events(ev, &mut out);
        self.out.extend(out);
        if !self.spawns.is_empty() {
            self.do_spawns(world);
        }
        // Mémoire des positions (rembobinage des tirs).
        let mut frame: Frame = Vec::new();
        if self.playing {
            frame.push((0, local.pos, local.crouch));
        }
        frame.extend(
            self.peers
                .iter()
                .filter(|(_, r)| r.ready)
                .map(|(&id, r)| (id, r.player.pos, r.player.crouch)),
        );
        frame.sort_by_key(|f| f.0);
        self.record(0, Move::Rule(Rule::Frame(self.tick)));
        self.history.push_back((self.tick, frame));
        if self.history.len() > HISTORY {
            self.history.pop_front();
        }
        if self.tick.is_multiple_of(SNAPSHOT_EVERY) {
            self.snapshots(local, world);
        }
        let due = self.tick.is_multiple_of(BACKUP_EVERY) || std::mem::take(&mut self.backup_now);
        if due && self.auto_backup {
            self.send_backup(local);
        }
        if self.ground_dirty {
            self.ground_dirty = false;
            self.broadcast(&ToPeer::Ground {
                guns: self.ground.clone(),
            });
        }
        if self.tick.is_multiple_of(SCOREBOARD_EVERY) {
            self.broadcast(&ToPeer::Scoreboard {
                rows: self.m.rows(),
                history: self.m.history.clone(),
            });
        }
    }

    fn net_player(&self, id: u64, pseudo: &str, p: &Player, weapon: u8) -> NetPlayer {
        let mp = self.m.players.get(&id);
        NetPlayer {
            id,
            pseudo: pseudo.to_string(),
            pos: p.pos.to_array(),
            yaw: p.yaw,
            pitch: p.pitch,
            crouch: p.crouch,
            speed: Vec3::new(p.vel.x, 0.0, p.vel.z).length(),
            hp: mp.map_or(0, |p| p.hp.min(255) as u8),
            alive: mp.is_some_and(|p| p.alive),
            weapon,
            kills: mp.map_or(0, |p| p.kills),
            deaths: mp.map_or(0, |p| p.deaths),
            team: mp.map_or(CT, |p| p.team),
            flags: self.m.flags(id),
        }
    }

    /// Ce que voit `viewer` (yeux) de `target` (pieds) : un adversaire caché derrière un
    /// mur n'est pas envoyé (contre les « wallhacks » et le « ghosting »), sauf tout près
    /// ou s'il vient de tirer (on l'entend).
    fn sees(&self, world: &World, eye: Vec3, target: u64, feet: Vec3) -> bool {
        if (feet - eye).length() < 6.0 {
            return true;
        }
        if self
            .last_shot
            .get(&target)
            .is_some_and(|t| self.tick.saturating_sub(*t) < 256)
        {
            return true;
        }
        [0.3f32, 1.0, 1.65].iter().any(|h| {
            let p = feet + Vec3::Y * *h;
            let d = p - eye;
            let len = d.length();
            raycast(world, eye.into(), (d / len.max(1e-3)).into(), len)
                .is_none_or(|hit| hit.distance >= len - 0.6)
        })
    }

    fn snapshots(&self, local: &Player, world: &World) {
        let me = self
            .playing
            .then(|| self.net_player(0, &self.pseudo, local, 255));
        let all: Vec<(u64, NetPlayer)> = self
            .peers
            .iter()
            .filter(|(_, r)| r.ready)
            .map(|(&id, r)| (id, self.net_player(id, &r.pseudo, &r.player, r.weapon)))
            .collect();
        let team = |id: u64| self.m.players.get(&id).map(|p| p.team);
        for (&id, r) in &self.peers {
            if !r.ready {
                continue;
            }
            let mp = self.m.players.get(&id);
            let eye = r.player.eye();
            let my_team = team(id);
            let visible = |o: u64, p: &NetPlayer| {
                team(o) == my_team || !p.alive || self.sees(world, eye, o, Vec3::from(p.pos))
            };
            let mut players = Vec::new();
            if let Some(me) = &me {
                if visible(0, me) {
                    players.push(me.clone());
                }
            }
            players.extend(
                all.iter()
                    .filter(|(o, p)| *o != id && visible(*o, p))
                    .map(|(_, p)| p.clone()),
            );
            self.send_to(
                id,
                &ToPeer::Snapshot {
                    tick: self.tick,
                    ack: r.ack,
                    you: r.player.net_state(),
                    hp: mp.map_or(0, |p| p.hp.min(255) as u8),
                    alive: mp.is_some_and(|p| p.alive),
                    players,
                    hud: self.m.hud(id, Some(r.player.pos)),
                },
            );
        }
    }

    /// Tir de `shooter` (0 = l'hôte) : cibles rembobinées au pas `view_tick`.
    fn fire(
        &mut self,
        shooter: u64,
        weapon: u8,
        origin: Vec3,
        dirs: &[Vec3],
        view_tick: u32,
        world: &World,
    ) -> Vec<Local> {
        let mut out = Vec::new();
        self.last_shot.insert(shooter, self.tick);
        let frame = pick_frame(&self.history, view_tick);
        let damage = shot(&self.m, &frame, shooter, weapon, origin, dirs, world);
        for (victim, hits) in damage {
            let head = hits
                .iter()
                .any(|(_, h)| matches!(h, Hurt::Bullet { head: true, .. }));
            // Marqueur de touche au tireur.
            if shooter == 0 {
                out.push(Local::Hit { head });
            } else {
                let total: u32 = hits.iter().map(|h| h.0).sum();
                self.send_to(
                    shooter,
                    &ToPeer::Hit {
                        damage: total.min(255) as u8,
                        head,
                    },
                );
            }
            for (d, hurt) in hits {
                let ev = self.m.hurt(Some(shooter), victim, d, hurt);
                self.events(ev, &mut out);
            }
        }
        out
    }

    /// Dégâts des grenades à tous les joueurs (les explosions et le feu simulés chez
    /// l'hôte, pour ses grenades et celles des autres).
    fn nade_damage(
        &mut self,
        world: &World,
        blasts: &[(Vec3, u64)],
        fires: &[(Vec3, u64)],
        dt: f32,
        out: &mut Vec<Local>,
    ) {
        if blasts.is_empty() && fires.is_empty() {
            return;
        }
        self.record(
            0,
            Move::Rule(Rule::Nades {
                blasts: blasts.iter().map(|(p, o)| (p.to_array(), *o)).collect(),
                fires: fires.iter().map(|(p, o)| (p.to_array(), *o)).collect(),
                dt,
            }),
        );
        // Mêmes nombres que ceux du journal (le vérificateur refait ce calcul).
        let blasts: Vec<(Vec3, u64)> = blasts
            .iter()
            .map(|(p, o)| (Vec3::from(p.to_array()), *o))
            .collect();
        let mut targets: Vec<(u64, Vec3)> = Vec::new();
        if self.playing {
            targets.push((0, self.local_pos));
        }
        let mut ids: Vec<u64> = self
            .peers
            .iter()
            .filter(|(_, r)| r.ready)
            .map(|(&id, _)| id)
            .collect();
        ids.sort();
        targets.extend(ids.iter().map(|id| (*id, self.peers[id].player.pos)));
        let hurts = nade_hurts(world, &targets, &blasts, fires, dt, &mut self.burns);
        for (owner, victim, d, h) in hurts {
            let ev = self.m.hurt(Some(owner), victim, d, h);
            self.events(ev, out);
        }
    }
}

impl Guest {
    fn to_host(&self, msg: &ToHost) {
        send(
            &self.link,
            &Request::ToHost {
                data: game::encode(msg),
            },
        );
    }

    fn receive(&mut self, msg: ToPeer) -> Vec<Local> {
        let mut out = Vec::new();
        match msg {
            ToPeer::Welcome { you, tick, map_ok } => {
                self.hello_timer = f32::INFINITY;
                self.you = Some(you);
                self.clock = tick as f32;
                if !map_ok {
                    out.push(Local::Notice(
                        "Ta carte est différente de celle de l'hôte : impossible de jouer.".into(),
                    ));
                }
            }
            ToPeer::Snapshot {
                tick,
                ack,
                you,
                hp,
                alive,
                players,
                hud,
            } => {
                // Horloge : rattrape l'hôte en douceur.
                let t = tick as f32;
                if (self.clock - t).abs() > 64.0 {
                    self.clock = t;
                } else {
                    self.clock += (t - self.clock) * 0.05;
                }
                if self.snaps.back().is_none_or(|(s, _)| *s < tick) {
                    self.snaps.push_back((tick, players));
                    while self.snaps.len() > 32 {
                        self.snaps.pop_front();
                    }
                }
                self.hud = hud;
                let prev_hp = self.latest.map(|l| l.2);
                self.latest = Some((ack, you, hp, alive));
                if prev_hp != Some(hp) {
                    out.push(Local::SetHp(hp as u32));
                }
            }
            ToPeer::Hit { head, .. } => out.push(Local::Hit { head }),
            ToPeer::Kill {
                killer,
                victim,
                weapon,
                head,
            } => out.push(Local::Chat(kill_line(&killer, &victim, weapon, head))),
            ToPeer::Respawn { state, reset, team } => {
                self.pending.clear();
                self.outbox.clear();
                self.hud.team = team;
                out.push(Local::Respawn { state, reset, team });
            }
            ToPeer::Chat { from, text } => {
                if from.is_empty() {
                    out.push(Local::Notice(text));
                } else {
                    out.push(Local::Chat(format!("<{from}> {text}")));
                }
            }
            ToPeer::Give { item } => out.push(Local::Give(item)),
            ToPeer::Scoreboard { rows, history } => {
                self.rows = rows;
                self.history = history;
            }
            ToPeer::RoundEnd { winner, reason } => out.push(Local::RoundEnd { winner, reason }),
            ToPeer::GameOver { summary } => {
                self.hud.phase = game::phase::GAME_OVER;
                out.push(Local::GameOver(summary));
            }
            ToPeer::Ground { guns } => self.ground = guns,
            ToPeer::Backup { .. } => {}
            ToPeer::Throw {
                owner,
                nade,
                pos,
                vel,
            } => out.push(Local::Thrown {
                owner,
                nade,
                pos: Vec3::from(pos),
                vel: Vec3::from(vel),
            }),
        }
        out
    }

    /// Autres joueurs interpolés entre les deux états reçus qui entourent l'instant
    /// affiché (un peu dans le passé).
    fn views(&self) -> Vec<View> {
        let t = self.clock - INTERP_TICKS;
        let (a, b) = match self.snaps.iter().position(|(s, _)| *s as f32 > t) {
            Some(0) | None => {
                let last = self.snaps.back();
                (last, last)
            }
            Some(i) => (self.snaps.get(i - 1), self.snaps.get(i)),
        };
        let (Some((ta, pa)), Some((tb, pb))) = (a, b) else {
            return Vec::new();
        };
        let k = if tb > ta {
            ((t - *ta as f32) / (*tb - *ta) as f32).clamp(0.0, 1.0)
        } else {
            1.0
        };
        pb.iter()
            .map(|p| {
                let prev = pa.iter().find(|q| q.id == p.id).unwrap_or(p);
                let lerp_angle = |a: f32, b: f32| {
                    let d = (b - a + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU)
                        - std::f32::consts::PI;
                    a + d * k
                };
                View {
                    pseudo: p.pseudo.clone(),
                    pos: Vec3::from(prev.pos).lerp(Vec3::from(p.pos), k),
                    yaw: lerp_angle(prev.yaw, p.yaw),
                    pitch: prev.pitch + (p.pitch - prev.pitch) * k,
                    crouch: prev.crouch + (p.crouch - prev.crouch) * k,
                    speed: p.speed,
                    alive: p.alive,
                    weapon: p.weapon,
                    team: p.team,
                    flags: p.flags,
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use minelib_net::client::{Client, Cmd, Event};
    use minelib_net::game::FpsMap;
    use minelib_world::{BlockId, ChunkPos};
    use std::time::{Duration, Instant};

    fn floor() -> World {
        let mut w = World::new(0);
        for dz in -1..=1 {
            for dx in -1..=1 {
                w.insert_empty(ChunkPos { x: dx, z: dz });
            }
        }
        for z in -16..32 {
            for x in -16..32 {
                w.set(x, 10, z, BlockId::STONE);
            }
        }
        w
    }

    fn wait(c: &Client, pred: impl Fn(&Event) -> bool) -> Event {
        let t = Instant::now();
        loop {
            if let Some(e) = c.poll() {
                if pred(&e) {
                    return e;
                }
                assert!(!matches!(e, Event::Error(_)), "{e:?}");
            }
            assert!(t.elapsed() < Duration::from_secs(30));
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// Hôte et invité reliés par un vrai relais : l'invité avance, l'hôte le voit avancer
    /// au même endroit que sa prédiction ; l'invité tire sur l'hôte et le touche.
    #[test]
    fn guest_moves_on_the_host_and_shoots_it_through_the_relay() {
        let dir = std::env::temp_dir().join(format!("minelib-net-{}", rand_u32()));
        let h = minelib_relay::start(minelib_relay::Config {
            dir: dir.clone(),
            bind: "127.0.0.1:0".into(),
            pow_bits: 4,
            name: "essai".into(),
        })
        .unwrap();
        let login = |pseudo: &str| {
            let c = Client::spawn(h.addr.to_string(), Some(h.public));
            c.send(Cmd::Register {
                pseudo: pseudo.into(),
                password: "motdepasse".into(),
            });
            let link = match wait(&c, |e| matches!(e, Event::Link(_))) {
                Event::Link(l) => l,
                _ => unreachable!(),
            };
            wait(&c, |e| matches!(e, Event::LoggedIn { .. }));
            (c, link)
        };
        let (hc, hl) = login("Hote");
        let (gc, gl) = login("Invite");
        hc.send(Cmd::Host {
            kind: 0,
            name: String::new(),
            map: "plat".into(),
            map_sig: [1; 32],
            max_players: 4,
        });
        let room = match wait(&hc, |e| matches!(e, Event::Hosted(_))) {
            Event::Hosted(r) => r,
            _ => unreachable!(),
        };
        gc.send(Cmd::Join(room));
        wait(&gc, |e| matches!(e, Event::Joined { .. }));
        let world = floor();
        let hash = [1u8; 32];
        let spawn = Vec3::new(8.5, 11.0, 8.5);
        let mut host = NetGame::host(hl, "Hote".into(), FpsMap::default(), hash, spawn);
        let mut host_p = Player::new(Vec3::new(8.5, 11.0, 2.5));
        let mut guest_p = Player::new(Vec3::new(0.0, 30.0, 0.0));
        let mut guest: Option<NetGame> = None;
        let dt = crate::session::SIM_TICK;
        let mut fired = false;
        let mut guest_start: Option<Vec3> = None;
        let mut host_hp = 100;
        for frame in 0..900 {
            // Messages reçus des deux côtés.
            while let Some(e) = hc.poll() {
                match e {
                    Event::PeerJoined { peer, pseudo } => {
                        host.peer_joined(peer, pseudo);
                        if guest.is_none() {
                            guest = Some(NetGame::guest(gl.clone(), hash));
                        }
                    }
                    Event::FromPeer { peer, data } => {
                        for l in host.receive(Some(peer), &data, &world) {
                            if let Local::SetHp(hp) = l {
                                host_hp = hp;
                            }
                        }
                    }
                    _ => {}
                }
            }
            if let Some(g) = &mut guest {
                while let Some(e) = gc.poll() {
                    if let Event::FromHost(data) = e {
                        for l in g.receive(None, &data, &world) {
                            if let Local::Respawn { state, .. } = l {
                                guest_p.set_net_state(&state);
                                guest_start = Some(guest_p.pos);
                            }
                        }
                    }
                }
            }
            // Quatre pas par image ; l'invité avance vers +x pendant 100 images.
            let walking = (100..180).contains(&frame) && guest_start.is_some();
            let m = MoveInput {
                strafe: if walking { 1.0 } else { 0.0 },
                sprint: true,
                fps: true,
                ..Default::default()
            };
            let idle = MoveInput {
                fps: true,
                ..Default::default()
            };
            if let Some(g) = &mut guest {
                g.reconcile(&mut guest_p, &world, dt);
            }
            for _ in 0..4 {
                host_p.update(&world, &idle, dt);
                host.on_tick(&world, &host_p, &idle, dt);
                if let Some(g) = &mut guest {
                    guest_p.yaw = 0.0;
                    guest_p.update(&world, &m, dt);
                    g.on_tick(&world, &guest_p, &m, dt);
                }
            }
            // Après l'arrêt, l'invité tire une fois vers la poitrine de l'hôte.
            let mut shots = Vec::new();
            if frame == 700 && !fired {
                fired = true;
                let eye = guest_p.eye();
                let target = host_p.pos + Vec3::Y * 1.2;
                shots.push((ak(), eye, vec![(target - eye).normalize()]));
            }
            for l in host.end_frame(&world, Vec::new(), 255, NadeOut::default(), 4.0 * dt) {
                match l {
                    Local::Respawn { state, .. } => host_p.set_net_state(&state),
                    Local::SetHp(hp) => host_hp = hp,
                    _ => {}
                }
            }
            if let Some(g) = &mut guest {
                g.end_frame(&world, shots, ak(), NadeOut::default(), 4.0 * dt);
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        let NetGame::Host(hs) = &host else {
            unreachable!()
        };
        let remote = hs.peers.values().next().expect("invité chez l'hôte");
        assert!(remote.ready, "même carte");
        let moved = remote.player.pos.x - guest_start.expect("réapparition").x;
        assert!(moved > 4.0, "l'invité a avancé chez l'hôte : {moved}");
        let gap = (remote.player.pos - guest_p.pos).length();
        assert!(gap < 0.05, "prédiction de l'invité = hôte (écart {gap})");
        assert!(host_hp < 100, "l'hôte est touché");
        assert_eq!(hs.m.players.len(), 2, "deux joueurs dans la partie");
        let teams: Vec<u8> = hs.m.players.values().map(|p| p.team).collect();
        assert_ne!(teams[0], teams[1], "équipes équilibrées");
        let views = guest.as_ref().unwrap().views();
        assert_eq!(views.len(), 1);
        assert!(
            (views[0].pos - host_p.pos).length() < 0.1,
            "l'invité voit l'hôte"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn weapons_are_dropped_and_picked_up_with_a_swap() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let spawn = Vec3::new(0.5, 11.0, 0.5);
        let mut g = NetGame::host(tx, "Hote".into(), FpsMap::default(), [0; 32], spawn);
        let awp = WEAPONS.iter().position(|w| w.name == "AWP").unwrap() as u8;
        let ak = ak();
        g.set_loadout(ak, 255);
        g.drop_weapon(awp);
        assert!(g.ground().is_empty(), "pas une arme qu'il porte");
        g.drop_weapon(ak);
        assert_eq!(g.ground().len(), 1);
        let NetGame::Host(h) = &mut g else {
            unreachable!()
        };
        h.ground.push(GroundGun {
            id: 99,
            weapon: awp,
            pos: spawn.to_array(),
        });
        h.loadouts.insert(0, (ak, 255));
        let out = g.pickup(99);
        assert!(matches!(out.as_slice(), [Local::Give(w)] if *w == awp as u16));
        let ground = g.ground();
        assert_eq!(ground.len(), 2, "l'AK posée à la place de l'AWP");
        assert!(ground.iter().any(|x| x.weapon == ak && x.id != 99));
    }

    /// L'hôte part : le garant de secours reprend la partie avec ses scores.
    #[test]
    fn the_backup_garant_takes_over_when_the_host_leaves() {
        let dir = std::env::temp_dir().join(format!("minelib-reprise-{}", rand_u32()));
        let h = minelib_relay::start(minelib_relay::Config {
            dir: dir.clone(),
            bind: "127.0.0.1:0".into(),
            pow_bits: 4,
            name: "essai".into(),
        })
        .unwrap();
        let login = |pseudo: &str| {
            let c = Client::spawn(h.addr.to_string(), Some(h.public));
            c.send(Cmd::Register {
                pseudo: pseudo.into(),
                password: "motdepasse".into(),
            });
            let link = match wait(&c, |e| matches!(e, Event::Link(_))) {
                Event::Link(l) => l,
                _ => unreachable!(),
            };
            wait(&c, |e| matches!(e, Event::LoggedIn { .. }));
            (c, link)
        };
        let (hc, hl) = login("Hote");
        let (gc, gl) = login("Garant");
        let sig = [5u8; 32];
        hc.send(Cmd::Search {
            map_sig: sig,
            map: "plat".into(),
        });
        wait(&hc, |e| matches!(e, Event::Hosted(_)));
        gc.send(Cmd::Search {
            map_sig: sig,
            map: "plat".into(),
        });
        wait(&gc, |e| matches!(e, Event::Joined { .. }));
        let world = floor();
        let spawn = Vec3::new(8.5, 11.0, 8.5);
        let mut host = NetGame::host(hl, "Hote".into(), FpsMap::default(), sig, spawn);
        let mut guest = NetGame::guest(gl, sig);
        let mut hp = Player::new(spawn);
        let mut gp = Player::new(spawn);
        let dt = crate::session::SIM_TICK;
        let idle = MoveInput {
            fps: true,
            ..Default::default()
        };
        let mut promoted = None;
        let t0 = Instant::now();
        for frame in 0..2000 {
            while let Some(e) = hc.poll() {
                match e {
                    Event::PeerJoined { peer, pseudo } => host.peer_joined(peer, pseudo),
                    Event::FromPeer { peer, data } => {
                        host.receive(Some(peer), &data, &world);
                    }
                    _ => {}
                }
            }
            while let Some(e) = gc.poll() {
                match e {
                    Event::FromHost(data) => {
                        guest.receive(None, &data, &world);
                    }
                    Event::Promoted { members, .. } => promoted = Some(members),
                    _ => {}
                }
            }
            if let Some(m) = promoted.take() {
                assert!(guest.promote("Garant".into(), &m, &gp, FpsMap::default(), None));
                break;
            }
            for _ in 0..4 {
                hp.update(&world, &idle, dt);
                host.on_tick(&world, &hp, &idle, dt);
                gp.update(&world, &idle, dt);
                guest.on_tick(&world, &gp, &idle, dt);
            }
            host.end_frame(&world, Vec::new(), 255, NadeOut::default(), 4.0 * dt);
            guest.end_frame(&world, Vec::new(), 255, NadeOut::default(), 4.0 * dt);
            // Au bout de 2 s de jeu (copie reçue), l'hôte s'en va.
            if frame == 300 {
                hc.send(Cmd::Leave);
            }
            assert!(t0.elapsed() < Duration::from_secs(60));
            std::thread::sleep(Duration::from_millis(1));
        }
        let NetGame::Host(nh) = &guest else {
            panic!("le garant de secours est devenu l'hôte");
        };
        assert_eq!(
            nh.m.players.get(&0).map(|p| p.pseudo.as_str()),
            Some("Garant")
        );
        assert!(
            nh.m.players
                .values()
                .any(|p| p.pseudo == "Hote" && !p.present),
            "l'ancien hôte reste au résultat"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Le vérificateur rejoue les coups : rien à redire sur une partie honnête ; une
    /// position faussée ou une chaîne réécrite est signalée.
    #[test]
    fn the_auditor_replays_moves_and_catches_a_cheating_host() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let world = floor();
        let spawn = Vec3::new(8.5, 11.0, 8.5);
        let mut g = NetGame::host(tx, "Hote".into(), FpsMap::default(), [0; 32], spawn);
        g.peer_joined(7, "Invite".into());
        let NetGame::Host(h) = &mut g else {
            unreachable!()
        };
        h.peers.get_mut(&7).unwrap().ready = true;
        h.m.join(7, "Invite");
        let dt = crate::session::SIM_TICK;
        let mut local = Player::new(Vec3::new(4.5, 11.0, 4.5));
        let walk = MoveInput {
            forward: 1.0,
            sprint: true,
            fps: true,
            ..Default::default()
        };
        let mut seq = 0;
        let mut run = |g: &mut NetGame, local: &mut Player, n: usize| {
            for _ in 0..n {
                let NetGame::Host(h) = g else { unreachable!() };
                seq += 1;
                let r = h.peers.get_mut(&7).unwrap();
                r.queue.push_back((
                    seq,
                    NetInput {
                        strafe: 1,
                        yaw: 0.3,
                        ..Default::default()
                    },
                ));
                local.update(&world, &walk, dt);
                g.on_tick(&world, local, &walk, dt);
            }
        };
        // (La partie envoie elle-même une copie au pas 256 : on prend les nôtres entre.)
        run(&mut g, &mut local, 258);
        let NetGame::Host(h) = &mut g else {
            unreachable!()
        };
        let b1 = h.make_backup(&local);
        run(&mut g, &mut local, 200);
        let NetGame::Host(h) = &mut g else {
            unreachable!()
        };
        let b2 = h.make_backup(&local);
        assert!(!b2.log.is_empty());
        let c = audit(&b1, &b2, &world);
        assert!(c.is_empty(), "partie honnête : {c:?}");
        // L'hôte se téléporte (sans entrée qui l'explique) : preuve.
        let mut tele = game::decode::<Backup>(&game::encode(&b2)).unwrap();
        tele.players.iter_mut().find(|p| p.0 == 0).unwrap().2.pos[0] += 3.0;
        let c = audit(&b1, &tele, &world);
        assert!(c.iter().any(|c| c.player == 0 && c.proof));
        // L'hôte déplace un invité : désaccord à juger par l'invité.
        let mut moved = game::decode::<Backup>(&game::encode(&b2)).unwrap();
        moved.players.iter_mut().find(|p| p.0 == 7).unwrap().2.pos[2] -= 2.0;
        let c = audit(&b1, &moved, &world);
        assert!(c.iter().any(|c| c.player == 7 && !c.proof && c.seq > 0));
        // Coup effacé du journal : chaîne fausse.
        let mut cut = game::decode::<Backup>(&game::encode(&b2)).unwrap();
        cut.log.remove(5);
        assert!(audit(&b1, &cut, &world).iter().any(|c| c.proof));
    }

    /// Une vraie partie à 10 joueurs par le relais, jusqu'au bout : 5 contre 5, tirs,
    /// achats, grenades, manches, fin de partie chez tous. Le garant de secours vérifie
    /// chaque copie en rejouant toute la partie : pas un seul désaccord, et il trouve le
    /// même résultat final que l'hôte.
    #[test]
    fn ten_players_play_a_whole_match_and_the_auditor_agrees() {
        let dir = std::env::temp_dir().join(format!("minelib-dix-{}", rand_u32()));
        let relay = minelib_relay::start(minelib_relay::Config {
            dir: dir.clone(),
            bind: "127.0.0.1:0".into(),
            pow_bits: 4,
            name: "essai".into(),
        })
        .unwrap();
        let login = |pseudo: &str| {
            let c = Client::spawn(relay.addr.to_string(), Some(relay.public));
            c.send(Cmd::Register {
                pseudo: pseudo.into(),
                password: "motdepasse".into(),
            });
            let link = match wait(&c, |e| matches!(e, Event::Link(_))) {
                Event::Link(l) => l,
                _ => unreachable!(),
            };
            wait(&c, |e| matches!(e, Event::LoggedIn { .. }));
            (c, link)
        };
        let (hc, hl) = login("Hote");
        hc.send(Cmd::Host {
            kind: 0,
            name: String::new(),
            map: "plat".into(),
            map_sig: [1; 32],
            max_players: 10,
        });
        let room = match wait(&hc, |e| matches!(e, Event::Hosted(_))) {
            Event::Hosted(r) => r,
            _ => unreachable!(),
        };
        let world = floor();
        let hash = [1u8; 32];
        let spawn = Vec3::new(8.5, 11.0, 8.5);
        let mut host = NetGame::host(hl, "Hote".into(), FpsMap::default(), hash, spawn);
        let mut host_p = Player::new(spawn);
        struct Bot {
            c: Client,
            net: NetGame,
            p: Player,
            id: u64,
            over: bool,
        }
        let mut bots: Vec<Bot> = Vec::new();
        for i in 1..10 {
            let (c, link) = login(&format!("Joueur{i}"));
            c.send(Cmd::Join(room));
            wait(&c, |e| matches!(e, Event::Joined { .. }));
            let mut net = NetGame::guest(link, hash);
            if let NetGame::Guest(g) = &mut net {
                // Chacun vérifie les copies s'il en reçoit (seul le garant de secours en reçoit).
                g.auditor = true;
                g.audit.witnessing = true;
            }
            bots.push(Bot {
                c,
                net,
                p: Player::new(Vec3::new(0.0, 30.0, 0.0)),
                id: 0,
                over: false,
            });
        }
        let dt = crate::session::SIM_TICK;
        let mut host_over: Option<MatchSummary> = None;
        let mut started = false;
        let mut conflicts = Vec::new();
        let mut thrown: Vec<(usize, u64, Vec3)> = Vec::new();
        let begin = Instant::now();
        let mut frame: u64 = 0;
        // Après la fin chez tous, encore un peu de temps (dernière copie au vérificateur).
        let mut after = 0;
        while after < 300 {
            if bots.iter().all(|b| b.over) && host_over.is_some() {
                after += 1;
            }
            frame += 1;
            assert!(
                begin.elapsed() < Duration::from_secs(900),
                "la partie ne finit pas (image {frame})"
            );
            // Messages reçus : l'hôte (et la copie attestée pour le vérificateur).
            while let Some(e) = hc.poll() {
                match e {
                    Event::PeerJoined { peer, pseudo } => {
                        let i: usize = pseudo.trim_start_matches("Joueur").parse().unwrap();
                        bots[i - 1].id = peer;
                        host.peer_joined(peer, pseudo);
                    }
                    Event::FromPeer { peer, data } => {
                        if game::decode::<ToHost>(&data).is_some_and(|m| witnessed(&m)) {
                            for b in &mut bots {
                                b.net.witness(peer, &data);
                            }
                        }
                        for l in host.receive(Some(peer), &data, &world) {
                            match l {
                                Local::Started => started = true,
                                Local::GameOver(s) => host_over = Some(s),
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }
            }
            for b in &mut bots {
                while let Some(e) = b.c.poll() {
                    if let Event::FromHost(data) = e {
                        for l in b.net.receive(None, &data, &world) {
                            match l {
                                Local::Respawn { state, .. } => b.p.set_net_state(&state),
                                Local::GameOver(_) => b.over = true,
                                _ => {}
                            }
                        }
                    }
                }
                b.net.reconcile(&mut b.p, &world, dt);
                conflicts.extend(b.net.take_conflicts());
            }
            // Qui est vivant, où, dans quelle équipe (vu par l'hôte).
            let NetGame::Host(h) = &host else {
                unreachable!()
            };
            let frozen = h.m.frozen();
            let state: Vec<(u64, bool, u8, Vec3)> = h
                .m
                .players
                .iter()
                .map(|(&id, p)| {
                    let at = if id == 0 {
                        host_p.pos
                    } else {
                        h.peers.get(&id).map_or(Vec3::ZERO, |r| r.player.pos)
                    };
                    (id, p.alive, p.team, at)
                })
                .collect();
            let phase_frame = frame % 400;
            // Déplacements : quelques pas de côté au début de chaque période, puis immobiles.
            let walk = |k: usize| MoveInput {
                strafe: if phase_frame < 60 {
                    if (frame / 400 + k as u64).is_multiple_of(2) {
                        1.0
                    } else {
                        -1.0
                    }
                } else {
                    0.0
                },
                sprint: true,
                fps: true,
                ..Default::default()
            };
            let aim = |me: u64, eye: Vec3| -> Option<Vec3> {
                let (_, alive, team, _) = state.iter().find(|s| s.0 == me)?;
                if !alive || frozen {
                    return None;
                }
                state
                    .iter()
                    .filter(|s| s.1 && s.2 != *team)
                    .map(|s| s.3 + Vec3::Y * 1.1)
                    .min_by(|a, b| (*a - eye).length().total_cmp(&(*b - eye).length()))
                    .map(|t| (t - eye).normalize())
            };
            let hm = walk(0);
            for _ in 0..4 {
                host_p.update(&world, &hm, dt);
                host.on_tick(&world, &host_p, &hm, dt);
                for (k, b) in bots.iter_mut().enumerate() {
                    let m = walk(k + 1);
                    b.p.update(&world, &m, dt);
                    b.net.on_tick(&world, &b.p, &m, dt);
                }
            }
            // Achats pendant le départ de manche, touches, grenades, tirs.
            if frozen && phase_frame == 10 {
                let _ = host.buy(game::ITEM_KEVLAR);
                for (k, b) in bots.iter_mut().enumerate() {
                    if k % 2 == 0 {
                        let _ = b.net.buy(game::ITEM_KEVLAR);
                    }
                    b.net.keys(k % 3 == 0, false);
                }
                host.keys(true, false);
            }
            if phase_frame == 20 {
                host.keys(false, false);
                for b in &mut bots {
                    b.net.keys(false, false);
                }
            }
            let mut host_nades = NadeOut::default();
            // Une HE de temps en temps : lancée par un joueur, elle explose 1,6 s après.
            if started && !frozen && phase_frame == 80 {
                let k = (frame / 400) as usize % bots.len();
                let b = &bots[k];
                if state.iter().any(|s| s.0 == b.id && s.1) {
                    thrown.push((frame as usize + 100, b.id, b.p.eye()));
                    let nades = NadeOut {
                        throws: vec![(HE, b.p.eye(), Vec3::new(0.0, 2.0, 3.0))],
                        ..Default::default()
                    };
                    bots[k].net.end_frame(&world, Vec::new(), ak(), nades, 4.0 * dt);
                }
            }
            thrown.retain(|&(when, owner, at)| {
                if when == frame as usize {
                    host_nades.blasts.push((at + Vec3::new(0.0, -1.0, 4.0), owner));
                }
                when > frame as usize
            });
            let mut host_shots = Vec::new();
            if frame.is_multiple_of(12) {
                if let Some(d) = aim(0, host_p.eye()) {
                    host_shots.push((ak(), host_p.eye(), vec![d]));
                }
            }
            for l in host.end_frame(&world, host_shots, ak(), host_nades, 4.0 * dt) {
                match l {
                    Local::Respawn { state, .. } => host_p.set_net_state(&state),
                    Local::Started => started = true,
                    Local::GameOver(s) => host_over = Some(s),
                    _ => {}
                }
            }
            for (k, b) in bots.iter_mut().enumerate() {
                let mut shots = Vec::new();
                if (frame + k as u64).is_multiple_of(12) && b.id != 0 {
                    if let Some(d) = aim(b.id, b.p.eye()) {
                        shots.push((ak(), b.p.eye(), vec![d]));
                    }
                }
                b.net.end_frame(&world, shots, ak(), NadeOut::default(), 4.0 * dt);
            }
            if frame.is_multiple_of(8) {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        // Les derniers messages (copie finale) arrivent.
        for _ in 0..200 {
            for b in &mut bots {
                while let Some(e) = b.c.poll() {
                    if let Event::FromHost(data) = e {
                        b.net.receive(None, &data, &world);
                    }
                }
                conflicts.extend(b.net.take_conflicts());
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(started, "la partie a commencé à 10");
        let summary = host_over.expect("fin de partie chez l'hôte");
        assert!(summary.score[0] >= 13 || summary.score[1] >= 13, "{:?}", summary.score);
        assert_eq!(summary.players.len(), 10);
        let NetGame::Host(h) = &host else {
            unreachable!()
        };
        let backup = h.backup.expect("un garant de secours");
        let auditor = bots.iter_mut().find(|b| b.id == backup).unwrap();
        let NetGame::Guest(g) = &mut auditor.net else {
            unreachable!()
        };
        assert!(
            g.audit.copies > 100,
            "copies vérifiées : {}",
            g.audit.copies
        );
        assert!(conflicts.is_empty(), "aucun désaccord : {conflicts:?}");
        assert_eq!(
            g.audit.results.first().map(game::encode),
            Some(game::encode(&summary)),
            "le vérificateur trouve le même résultat"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Le vérificateur attrape aussi les faux dégâts, les tirs inventés ou ignorés, les
    /// explosions sans grenade et les scores retouchés.
    #[test]
    fn the_auditor_catches_fake_hits_damage_and_scores() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let world = floor();
        let spawn = Vec3::new(8.5, 11.0, 8.5);
        let mut g = NetGame::host(tx, "Hote".into(), FpsMap::default(), [0; 32], spawn);
        g.peer_joined(7, "Invite".into());
        let hello = game::encode(&ToHost::Hello { map_hash: [0; 32] });
        g.receive(Some(7), &hello, &world);
        let dt = crate::session::SIM_TICK;
        let mut local = Player::new(Vec3::new(4.5, 11.0, 4.5));
        let idle = MoveInput {
            fps: true,
            ..Default::default()
        };
        let mut seq = 0;
        let mut run = |g: &mut NetGame, local: &mut Player, n: usize| {
            for _ in 0..n {
                let NetGame::Host(h) = g else { unreachable!() };
                seq += 1;
                let r = h.peers.get_mut(&7).unwrap();
                r.queue.push_back((seq, NetInput::default()));
                local.update(&world, &idle, dt);
                g.on_tick(&world, local, &idle, dt);
                for l in g.end_frame(&world, Vec::new(), 255, NadeOut::default(), dt) {
                    if let Local::Respawn { state, .. } = l {
                        local.set_net_state(&state);
                    }
                }
            }
        };
        let backup = |g: &mut NetGame, local: &Player| {
            let NetGame::Host(h) = g else { unreachable!() };
            h.make_backup(local)
        };
        let why = |c: &[Conflict]| c.iter().map(|c| c.why).collect::<Vec<_>>();
        host(&mut g).auto_backup = false;
        run(&mut g, &mut local, 300);
        let mut a = Auditor {
            witnessing: true,
            ..Default::default()
        };
        let b0 = backup(&mut g, &local);
        run(&mut g, &mut local, 100);
        // Un vrai tir de l'invité, vu par le relais : rien à redire.
        let eye = host(&mut g).peers[&7].player.eye();
        let fire = ToHost::Fire {
            weapon: ak(),
            origin: eye.to_array(),
            dirs: vec![((local.pos + Vec3::Y * 1.1) - eye).normalize().to_array()],
            view_tick: host(&mut g).tick,
        };
        a.witness(7, fire.clone());
        g.receive(Some(7), &game::encode(&fire), &world);
        run(&mut g, &mut local, 100);
        let b1 = backup(&mut g, &local);
        assert!(a.audit(&b0, &b1, &world).is_empty());
        assert!(host(&mut g).m.players[&0].hp < 100, "l'hôte est touché");
        // Dégâts qui ne viennent d'aucun coup du journal : règles différentes.
        run(&mut g, &mut local, 50);
        let _ = host(&mut g).m.hurt(Some(7), 0, 30, Hurt::Knife);
        run(&mut g, &mut local, 50);
        let b2 = backup(&mut g, &local);
        let c = a.audit(&b1, &b2, &world);
        assert!(why(&c).iter().any(|w| w.starts_with("règles")), "{c:?}");
        // Tir inventé par l'hôte au nom de l'invité (le relais ne l'a jamais vu passer).
        let fake = ToHost::Fire {
            weapon: ak(),
            origin: eye.to_array(),
            dirs: vec![[1.0, 0.0, 0.0]],
            view_tick: host(&mut g).tick,
        };
        g.receive(Some(7), &game::encode(&fake), &world);
        run(&mut g, &mut local, 100);
        let b3 = backup(&mut g, &local);
        let c = a.audit(&b2, &b3, &world);
        assert!(why(&c).contains(&"demande inventée par l'hôte"), "{c:?}");
        // Tir de l'invité que l'hôte « oublie » : signalé quelques copies plus tard.
        a.witness(7, fake.clone());
        let mut prev = b3;
        let mut found = false;
        for _ in 0..=WITNESS_LATE + 1 {
            run(&mut g, &mut local, 50);
            let b = backup(&mut g, &local);
            found |= why(&a.audit(&prev, &b, &world)).contains(&"demande d'un joueur ignorée par l'hôte");
            prev = b;
        }
        assert!(found, "tir ignoré signalé");
        // Explosion d'une HE que personne n'a lancée, longtemps après le début du suivi.
        run(&mut g, &mut local, 4 * 256);
        let b4 = backup(&mut g, &local);
        let _ = a.audit(&prev, &b4, &world);
        let nades = NadeOut {
            blasts: vec![(local.pos + Vec3::Y, 7)],
            ..Default::default()
        };
        g.end_frame(&world, Vec::new(), 255, nades, dt);
        run(&mut g, &mut local, 20);
        let b5 = backup(&mut g, &local);
        let c = a.audit(&b4, &b5, &world);
        assert!(why(&c).contains(&"explosion sans grenade"), "{c:?}");
        // Éliminations ajoutées à la main dans la copie.
        run(&mut g, &mut local, 20);
        let mut b6 = backup(&mut g, &local);
        b6.m.players.get_mut(&7).unwrap().kills += 5;
        let c = a.audit(&b5, &b6, &world);
        assert!(why(&c).iter().any(|w| w.starts_with("règles")), "{c:?}");
    }

    fn host(g: &mut NetGame) -> &mut Host {
        let NetGame::Host(h) = g else { unreachable!() };
        h
    }

    fn ak() -> u8 {
        WEAPONS.iter().position(|d| d.name == "AK-47").unwrap() as u8
    }

    fn rand_u32() -> u32 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos()
    }
}
