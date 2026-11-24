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
use crate::player::{MoveInput, Other, Player, HALF_WIDTH, HEIGHT};
use crate::player_model::HitZone;
use crate::weapons::WEAPONS;

/// Un état envoyé aux invités tous les 4 pas (64 fois par seconde).
const SNAPSHOT_EVERY: u32 = 4;
/// Entre deux états complets, les positions partent à chaque pas (256 par seconde).
const MOVES_EVERY: u32 = 1;
/// Tableau des scores : toutes les demi-secondes.
const SCOREBOARD_EVERY: u32 = 128;
/// Voie rapide (`minelib_net::fast`) : pas d'entrées répétés dans chaque paquet (les
/// derniers, déjà partis par le relais). Un paquet perdu est ainsi couvert par le suivant.
const FAST_INPUTS: usize = 8;
/// Voie rapide : messages venus des passeurs que l'invité regarde avant de dire s'il
/// reçoit plus vite par eux ou par le relais (`Guest::fast_vote`).
const FAST_VOTES: u8 = 32;
/// Voie rapide : à un joueur qui reçoit plus vite par le relais, une sonde par les
/// passeurs tous les tant de pas (16 par seconde), pour savoir quand ce n'est plus vrai.
const FAST_PING_EVERY: u32 = 16;
/// (v274) Voie rapide : un passeur est signalé au relais quand, pendant `FAST_BAD_CHECKS`
/// demi-secondes de suite, il est muet ou plus lent que l'autre de `FAST_BAD_MS` (aller-
/// retour mesuré de bout en bout) alors que l'autre répond.
const FAST_BAD_MS: f32 = 30.0;
const FAST_BAD_CHECKS: u8 = 6;
/// Retard (pas) des nouvelles de l'hôte au-delà duquel une image compte comme bloquée
/// (≈ 23 ms) : la mesure qui dit si la voie rapide sert à quelque chose.
const STALL_TICKS: f32 = 6.0;
/// Positions gardées pour rembobiner les tirs : 1 s.
const HISTORY: usize = 256;
/// Retard d'affichage des autres joueurs chez l'invité (pas) : ~47 ms.
const INTERP_TICKS: f32 = 8.0;
/// Retard d'affichage au plus (pas) : au-delà, mieux vaut un saut qu'un retard énorme.
const MAX_DELAY: f32 = 40.0;
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

pub fn to_net(m: &MoveInput, yaw: f32, pitch: f32, carry: u8) -> NetInput {
    NetInput {
        forward: m.forward.round().clamp(-1.0, 1.0) as i8,
        strafe: m.strafe.round().clamp(-1.0, 1.0) as i8,
        jump: m.jump,
        jump_pressed: m.jump_pressed,
        walk: !m.sprint,
        duck: m.sneak,
        yaw,
        pitch,
        carry,
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
    // Jamais plus vite qu'au couteau, ni plus lentement qu'une arme lourde à la lunette.
    p.carry = n.carry.clamp(100, 250);
    // (v209) Terrain pas encore chargé là où il est (début de partie : les chunks arrivent
    // de près en loin) : il attend sur place. Avant, un bot sur un chunk pas encore là
    // tombait à travers le sol, faute de sol.
    if !ground_loaded(world, p.pos) {
        p.vel = Vec3::ZERO;
        return;
    }
    p.update(world, &from_net(n), dt);
}

/// (v209) Les chunks sous un joueur (sa boîte, et ce qu'il touche en un pas) sont-ils
/// chargés ?
fn ground_loaded(world: &World, feet: Vec3) -> bool {
    [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)]
        .iter()
        .all(|&(dx, dz)| {
            let x = (feet.x + dx).floor() as i32;
            let z = (feet.z + dz).floor() as i32;
            world
                .chunk(minelib_world::ChunkPos::containing(x, z))
                .is_some()
        })
}

/// Un autre joueur, tel qu'il faut le dessiner.
pub struct View {
    pub pseudo: String,
    pub pos: Vec3,
    pub yaw: f32,
    pub pitch: f32,
    pub crouch: f32,
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
    /// (v174) Le joueur local reprend la partie solo (après le mode spectateur), là où il
    /// se trouve : comme il l'avait laissée, ou comme à un départ de manche (`fresh` ;
    /// `reset` : pistolet de départ).
    Resume {
        fresh: bool,
        reset: bool,
        team: u8,
    },
    /// (v209) Le joueur local prend la place du bot `bot` (partie solo, mort) : là où il
    /// est, avec ce qu'il tenait en main et les munitions de ses armes (voir
    /// `NetGame::take_over` ; vie et armes arrivent juste avant, par `SetHp` et `Guns`).
    TakeOver {
        state: SimState,
        bot: String,
        held: u8,
        ammo: Vec<(u8, (u32, u32))>,
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
    /// Élimination, pour le fil en haut à droite.
    Kill(Kill),
    /// Exclu de la partie (trois coéquipiers tués, vote) : on la quitte.
    Kicked(String),
    /// Pause de la partie commencée / finie (hôte : le dire au relais).
    Paused(bool),
    /// Un joueur a tiré depuis `pos` (`heard` : un autre que soi, à faire entendre) et
    /// touché des joueurs en `hits` (sang). (v249) `dirs` : la direction de chaque balle
    /// (les impacts sur les blocs).
    Shot {
        weapon: u8,
        pos: Vec3,
        heard: bool,
        hits: Vec<Vec3>,
        dirs: Vec<Vec3>,
    },
    /// Bruit d'un joueur en `pos` : pas (`STEP_RUN`), réception d'une chute (`STEP_LAND`),
    /// cri de douleur après une chute (`STEP_PAIN`).
    Step(Vec3, u8),
    /// Portes et trappes changées (case, bloc) ; `at` : où l'on entend la porte.
    Doors {
        cells: Vec<([i32; 3], u16)>,
        at: Option<Vec3>,
    },
    /// Ce que le joueur porte vraiment selon l'hôte (armes, grenades).
    Guns {
        guns: (u8, u8),
        nades: [u8; 5],
    },
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
    /// Lobby (0 : seul).
    party: u64,
    player: Player,
    queue: VecDeque<(u32, NetInput)>,
    ack: u32,
    weapon: u8,
    ready: bool,
    fall_from: Option<f32>,
    /// (v260) Regard (cap, hauteur) de sa dernière entrée jouée : celui de ses boîtes des
    /// coups (`Frame`). Une réapparition le replace sans y toucher ; le vérificateur le suit
    /// de la même façon, d'après le journal des coups (il ne connaît que les entrées).
    look: (f32, f32),
    /// Entrées qu'il peut encore jouer : une de plus à chaque pas (personne n'avance
    /// plus vite que le temps), avec un peu d'avance pour rattraper un à-coup du réseau.
    budget: u32,
}

/// Copie de la partie confiée au garant de secours : de quoi la reprendre si l'hôte
/// part.
#[derive(serde::Serialize, serde::Deserialize)]
struct Backup {
    m: Match,
    host_pseudo: String,
    /// Joueurs (0 = l'hôte) : pseudo, état de déplacement, arme en main.
    players: Vec<(u64, String, SimState, u8)>,
    /// (v260) Regard (cap, hauteur) de la dernière entrée de chacun : ses boîtes des coups
    /// sont les pavés du modèle, tournés avec lui (`Frame`).
    looks: Vec<(u64, f32, f32)>,
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

impl Backup {
    /// (v258) Les portes posées pour la partie (`FpsMap::door_list`) ne sont pas dans la copie
    /// (l'encodage de la carte ne les a pas, pour que les cartes déjà partagées se lisent
    /// toujours) : celui qui la reçoit y remet celles de sa carte, la même que celle de l'hôte
    /// (sa signature les compte). Sans elles, le vérificateur refusait la porte qu'un joueur
    /// ouvrait, et prenait l'hôte honnête pour un tricheur.
    fn with_doors(mut self, doors: &[game::Zone]) -> Self {
        self.m.map.door_list = doors.to_vec();
        self
    }
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
    Hello {
        pseudo: String,
        ok: bool,
        /// Lobby du joueur (0 : seul).
        party: u64,
    },
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

/// Activité d'un joueur lue dans le journal (chez l'hôte comme chez le vérificateur, au
/// même endroit) : des entrées qui changent, ou n'importe quelle demande. Sert à sortir de
/// la partie un joueur en vie qui ne touche plus à rien (`Match::tick_afk`).
fn note_activity(m: &mut Match, who: u64, mv: &Move) {
    let sig = |i: &NetInput| {
        (i.forward as u8 as u64)
            | (i.strafe as u8 as u64) << 8
            | (i.jump as u64) << 16
            | (i.jump_pressed as u64) << 17
            | (i.walk as u64) << 18
            | (i.duck as u64) << 19
            | ((i.yaw.to_bits() ^ i.pitch.to_bits().rotate_left(13)) as u64) << 24
    };
    match mv {
        Move::Input(_, i) | Move::Own(i, _) => m.input_seen(who, sig(i)),
        Move::Rule(Rule::Msg(_)) => m.touch(who),
        _ => {}
    }
}

/// Demandes d'un joueur que le relais montre aussi au vérificateur (il sait ainsi que
/// l'hôte ne les invente pas et ne les oublie pas).
pub fn witnessed(m: &ToHost) -> bool {
    matches!(
        m,
        ToHost::Fire { .. }
            | ToHost::Throw { .. }
            | ToHost::Buy(_)
            | ToHost::Sell(_)
            | ToHost::Action { .. }
            | ToHost::Team(_)
            | ToHost::Drop { .. }
            | ToHost::Pickup { .. }
            | ToHost::Color(_)
            | ToHost::Door { .. }
            | ToHost::CallVote { .. }
            | ToHost::Ballot(_)
    )
}

/// Distance (pieds du joueur au centre de la case) à laquelle on peut ouvrir une porte.
pub const DOOR_REACH: f32 = 3.5;

/// Porte ou trappe ouverte / fermée par un joueur pendant la partie, si la carte le
/// permet : renvoie les cases changées (déjà notées dans `m.doors`). Hôte et vérificateur
/// passent par ici, avec les mêmes données : ils trouvent la même chose.
pub fn use_door(
    m: &mut Match,
    world: &World,
    feet: Option<Vec3>,
    alive: bool,
    pos: [i32; 3],
    side: u8,
) -> Vec<([i32; 3], u16)> {
    use minelib_world::BlockId;
    // (v258) Toutes les portes (`/fps set door on`), ou celles posées pour la partie.
    let Some(feet) = feet.filter(|_| alive && m.map.door_usable(pos)) else {
        return Vec::new();
    };
    let center = Vec3::new(pos[0] as f32, pos[1] as f32, pos[2] as f32) + Vec3::splat(0.5);
    if (feet + Vec3::Y - center).length() > DOOR_REACH {
        return Vec::new();
    }
    let look = match side {
        0 => Vec3::X,
        1 => -Vec3::X,
        4 => Vec3::Z,
        _ => -Vec3::Z,
    };
    let changes: Vec<([i32; 3], u16, u16)> = {
        let doors = &m.doors;
        let get = |c: [i32; 3]| {
            doors
                .get(&c)
                .map_or_else(|| world.get(c[0], c[1], c[2]), |e| BlockId(e[0]))
        };
        let mut opened: HashMap<[i32; 3], BlockId> =
            doors.iter().map(|(c, e)| (*c, BlockId(e[1]))).collect();
        crate::session::toggle_door_pair_with(&get, &mut opened, pos, look, m.map.door_sync())
            .into_iter()
            .map(|(c, b)| (c, b.0, doors.get(&c).map_or(get(c).0, |e| e[1])))
            .collect()
    };
    let mut out = Vec::new();
    for (c, b, orig) in changes {
        if b == orig {
            m.doors.remove(&c);
        } else {
            m.doors.insert(c, [b, orig]);
        }
        out.push((c, b));
    }
    out
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

/// Un tir est-il recevable ? Tireur vivant, pas pendant le départ, tir depuis ses yeux
/// (au retard du réseau près, et sans mur entre ses yeux et le départ du tir) ; puis les
/// règles : il porte l'arme, elle tire autant de plombs, à sa cadence (`Match::shoot`,
/// qui compte le tir).
fn fire_ok(
    m: &mut Match,
    world: &World,
    shooter: u64,
    who: Option<&Player>,
    weapon: u8,
    origin: Vec3,
    rays: usize,
) -> bool {
    let Some(p) = who else { return false };
    let eye = p.eye();
    // On ne court pas au couteau en tirant à l'AWP : la vitesse annoncée doit être celle de
    // l'arme (lunette comprise).
    let carry_ok = WEAPONS
        .get(weapon as usize)
        .is_none_or(|w| p.carry <= w.speed(false));
    let gap = origin - eye;
    let d = gap.length();
    let clear =
        d < 0.05 || raycast(world, eye.into(), (gap / d).into(), d).is_none_or(|h| h.distance >= d);
    m.players.get(&shooter).is_some_and(|p| p.alive)
        && !m.frozen()
        && d < 2.5
        && clear
        && carry_ok
        && m.shoot(shooter, weapon, rays)
}

/// Entrées d'avance qu'un joueur peut rattraper d'un coup (après un à-coup du réseau).
const INPUT_BURST: u32 = 64;

/// Rembobinage maximal d'un tir (pas) : 400 ms. Au-delà, on tirerait sur le passé.
const MAX_REWIND: u32 = 102;

/// Les autres joueurs, obstacles pour `me`.
fn others_except(feet: &BTreeMap<u64, Vec3>, me: u64) -> Vec<Other> {
    let list: Vec<Vec3> = feet
        .iter()
        .filter(|(id, _)| **id != me)
        .map(|(_, f)| *f)
        .collect();
    crate::player::others_from(&list)
}

/// Pieds d'un joueur pour les règles (le vérificateur : seulement ceux qui sont en jeu).
fn feet_in(players: &BTreeMap<u64, Player>, ready: &BTreeSet<u64>, id: u64) -> Option<Vec3> {
    (id == 0 || ready.contains(&id))
        .then(|| players.get(&id).map(|p| p.pos))
        .flatten()
}

/// Positions gardées au pas le plus proche de `view_tick`.
fn pick_frame(history: &VecDeque<(u32, Frame)>, view_tick: u32) -> Frame {
    history
        .iter()
        .min_by_key(|(t, _)| t.abs_diff(view_tick))
        .map(|(_, f)| f.clone())
        .unwrap_or_default()
}

/// Une touche d'un tir : dégâts, nature, à travers un mur ?, où.
#[derive(Clone, Copy, Debug)]
struct Touch {
    damage: u32,
    hurt: Hurt,
    wall: bool,
    point: Vec3,
}

/// Dégâts d'un tir (rayons `dirs` depuis `origin`) aux joueurs vivants de `frame`. Une
/// balle traverse un mur d'un bloc (dégâts réduits derrière), pas le couteau.
fn shot(
    m: &Match,
    frame: &Frame,
    shooter: u64,
    weapon: u8,
    origin: Vec3,
    dirs: &[Vec3],
    world: &World,
) -> BTreeMap<u64, Vec<Touch>> {
    let knife = weapon == KNIFE_LIGHT || weapon == KNIFE_HEAVY;
    let range = if knife { KNIFE_RANGE } else { RANGE };
    let mut damage: BTreeMap<u64, Vec<Touch>> = BTreeMap::new();
    for &dir in dirs {
        let path = if knife {
            crate::weapons::BulletPath {
                first: raycast(world, origin.into(), dir.into(), range)
                    .map_or(range, |h| h.distance),
                ..Default::default()
            }
        } else {
            crate::weapons::bullet_path(world, origin, dir, range)
        };
        let mut best: Option<(f32, u64, Touch)> = None;
        for &(id, feet, crouch, yaw, pitch) in frame {
            if id == shooter || !m.players.get(&id).is_some_and(|p| p.alive) {
                continue;
            }
            // (v256) Une balle touche les pavés du modèle (`ray_model`) ; le couteau garde
            // sa boîte large (un coup de lame balaie devant soi, ce n'est pas un point).
            let touched = if knife {
                let (body, head, _) = hitbox(feet, crouch, yaw);
                ray_player(origin, dir, body, head).map(|(t, _)| (t, HitZone::Chest))
            } else {
                ray_model(origin, dir, feet, crouch, yaw, pitch)
            };
            if let Some((t, zone)) = touched {
                let factor = path.factor_at(t);
                let wall = t >= path.first;
                if let (Some(factor), true) = (factor, best.is_none_or(|b| t < b.0)) {
                    let point = origin + dir * t;
                    let (d, hurt) = damage_at(weapon, t, zone);
                    let damage = if wall { (d as f32 * factor) as u32 } else { d };
                    best = Some((
                        t,
                        id,
                        Touch {
                            damage,
                            hurt,
                            wall,
                            point,
                        },
                    ));
                }
            }
        }
        if let Some((_, id, touch)) = best {
            damage.entry(id).or_default().push(touch);
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
            .filter_map(|&(_, feet, crouch, yaw, pitch)| {
                // Le plus loin des deux sortes de boîtes (couteau, balles : `shot`).
                let (body, head, _) = hitbox(feet, crouch, yaw);
                let wide = ray_player(origin, dir, body, head).map(|(t, _)| t);
                let model = ray_model(origin, dir, feet, crouch, yaw, pitch).map(|(t, _)| t);
                match (wide, model) {
                    (Some(a), Some(b)) => Some(a.max(b)),
                    (a, b) => a.or(b),
                }
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
/// Déplacement d'un joueur mort ou immobilisé (soupçon, pas une preuve).
const DEAD_MOVE: &str = "déplacement d'un joueur mort ou immobilisé";

/// Essais (`MINELIB_AUDIT_DEBUG`) : imprime qui meurt ou revit, à quel moment du journal.
fn dbg_alive(tag: &str, t: u64, who: u64, what: &str, m: &Match) {
    use std::sync::Mutex;
    static SEEN: Mutex<Option<BTreeMap<(String, u64), bool>>> = Mutex::new(None);
    if std::env::var_os("MINELIB_AUDIT_DEBUG").is_none() {
        return;
    }
    let mut g = SEEN.lock().unwrap();
    let map = g.get_or_insert_with(BTreeMap::new);
    for (&id, p) in &m.players {
        let k = (tag.to_string(), id);
        if map.get(&k) != Some(&p.alive) {
            map.insert(k, p.alive);
            eprintln!(
                "ALIVE {tag} t={t} after who={who} {what}: joueur {id} -> {}",
                p.alive
            );
        }
    }
}

fn fall_check(p: &Player, world: &World, from: &mut Option<f32>) -> Option<u32> {
    if p.pos.y < -40.0 {
        return Some(1000);
    }
    // L'eau amortit la chute : aucun dégât. Sur une échelle, on ne tombe pas.
    if p.on_ladder || crate::player::in_water(world, p.pos) {
        *from = None;
        return None;
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
    /// Point de réapparition de l'hôte qu'il n'a pas encore pris.
    cand0: Option<Player>,
    /// Bloc d'origine des portes que le vérificateur a changées dans son monde.
    door_orig: HashMap<[i32; 3], u16>,
    /// Sauts des joueurs (scripts de bunny hop, macros).
    hops: crate::anticheat::HopTracker,
    /// Triches prouvées en rejouant (pseudo, preuve), à envoyer au relais.
    pub cheats: Vec<(String, crate::anticheat::Finding)>,
}

/// Une demande vue par le relais doit apparaître au journal dans les copies qui suivent.
const WITNESS_LATE: u32 = 10;
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
        let door_orig = std::mem::take(&mut self.door_orig);
        *self = Self {
            witnessing,
            door_orig,
            ..Self::default()
        };
    }

    /// Met les portes du monde du vérificateur dans l'état `doors` (celui d'une copie).
    fn sync_doors(&mut self, world: &mut World, doors: &BTreeMap<[i32; 3], [u16; 2]>) {
        for (c, e) in doors {
            self.door_orig.entry(*c).or_insert(e[1]);
        }
        for (c, orig) in &self.door_orig {
            let want = minelib_world::BlockId(doors.get(c).map_or(*orig, |e| e[0]));
            if world.get(c[0], c[1], c[2]) != want {
                world.set(c[0], c[1], c[2], want);
            }
        }
    }

    /// Portes changées en rejouant : dans le monde du vérificateur aussi (on s'y cogne).
    fn apply_doors(&mut self, world: &mut World, m: &Match, cells: &[([i32; 3], u16)]) {
        for &(c, b) in cells {
            let orig = m.doors.get(&c).map_or(b, |e| e[1]);
            self.door_orig.entry(c).or_insert(orig);
            world.set(c[0], c[1], c[2], minelib_world::BlockId(b));
        }
    }

    /// Rejoue le journal de `new` à partir de `prev` et compare.
    fn audit(&mut self, prev: &Backup, new: &Backup, world: &mut World) -> Vec<Conflict> {
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
        self.sync_doors(world, &m.doors);
        let mut players: BTreeMap<u64, Player> = prev
            .players
            .iter()
            .map(|(id, _, s, _)| {
                // (v260) Avec le regard de sa dernière entrée (ses boîtes des coups).
                let mut p = player_from(s);
                if let Some(l) = prev.looks.iter().find(|l| l.0 == *id) {
                    (p.yaw, p.pitch) = (l.1, l.2);
                }
                (*id, p)
            })
            .collect();
        let mut ready: BTreeSet<u64> = prev.players.iter().map(|p| p.0).collect();
        let mut falls: BTreeMap<u64, Option<f32>> =
            prev.falls.iter().map(|&(id, f)| (id, Some(f))).collect();
        let mut burns: BTreeMap<u64, f32> = prev.burns.iter().copied().collect();
        // L'hôte : son déplacement rejoué, et le point de réapparition pas encore pris.
        let mut sim0: Option<Player> = players.get(&0).cloned();
        // (Gardé d'une copie à l'autre : la réapparition peut tomber à la jointure.)
        let mut cand0: Option<Player> = self.cand0.take();
        let mut verifiable = true;
        let mut skip: BTreeSet<u64> = BTreeSet::new();
        let mut last_seq: BTreeMap<u64, u32> = BTreeMap::new();
        let mut events: Vec<Ev> = Vec::new();
        let alive = |m: &Match, id: u64| m.players.get(&id).is_some_and(|p| p.alive);
        // Pas de l'hôte (dernières positions gardées) et entrées jouées par chacun.
        let mut now = prev.tick;
        let mut ticks = 0u32;
        let mut inputs: BTreeMap<u64, u32> = BTreeMap::new();
        // Obstacles que sont les autres joueurs (vivants, en jeu) pour `me`, comme chez l'hôte.
        let solid = |m: &Match,
                     players: &BTreeMap<u64, Player>,
                     ready: &BTreeSet<u64>,
                     me: u64|
         -> Vec<Other> {
            let list: Vec<Vec3> = players
                .iter()
                .filter(|(id, _)| {
                    **id != me
                        && (**id == 0 || ready.contains(id))
                        && m.players.get(id).is_some_and(|p| p.alive)
                })
                .map(|(_, p)| p.pos)
                .collect();
            crate::player::others_from(&list)
        };
        // Suites des événements des règles, comme chez l'hôte : un mort lâche son arme.
        macro_rules! settle {
            ($ev:expr) => {{
                let ev: Vec<Ev> = $ev;
                for e in &ev {
                    if let Ev::Killed { victim, .. } = e {
                        let at = feet_in(&players, &ready, *victim);
                        m.dropped_dead(*victim, at);
                    }
                }
                events.extend(ev);
            }};
        }
        for (t, who, mv) in &new.log {
            let (t, who) = (*t, *who);
            dbg_alive("verif", t as u64, who, "(avant)", &m);
            note_activity(&mut m, who, mv);
            match mv {
                Move::Input(seq, input) => {
                    let others = solid(&m, &players, &ready, who);
                    let Some(p) = players.get_mut(&who) else {
                        continue;
                    };
                    p.others = others;
                    let moving =
                        input.forward != 0 || input.strafe != 0 || input.jump || input.jump_pressed;
                    if !alive(&m, who) || ((m.frozen() || m.busy(who)) && moving) {
                        if std::env::var_os("MINELIB_AUDIT_DEBUG").is_some() {
                            eprintln!(
                                "AUDIT t={t} who={who} seq={seq} alive={} frozen={} busy={} phase={} input={input:?}",
                                alive(&m, who),
                                m.frozen(),
                                m.busy(who),
                                m.phase
                            );
                        }
                        wrong.push(DEAD_MOVE);
                    }
                    let before = p.on_ground;
                    step(p, world, input, dt);
                    let jumped = before && !p.on_ground && p.vel.y > 0.0;
                    self.hops
                        .observe(who, t, input.jump_pressed, before, p.on_ground, jumped);
                    last_seq.insert(who, *seq);
                    *inputs.entry(who).or_insert(0) += 1;
                }
                Move::Own(input, state) => {
                    let others = solid(&m, &players, &ready, 0);
                    let mut s = sim0.take().unwrap_or_else(|| player_from(state));
                    s.others = others.clone();
                    let before = s.on_ground;
                    step(&mut s, world, input, dt);
                    let jumped = before && !s.on_ground && s.vel.y > 0.0;
                    self.hops
                        .observe(0, t, input.jump_pressed, before, s.on_ground, jumped);
                    // Réapparition : l'hôte la prend à la fin de son image (quelques pas
                    // plus tard), en repartant du point d'apparition.
                    if let Some(c) = &cand0 {
                        let mut c = c.clone();
                        c.others = others;
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
                    p.carry = input.carry.clamp(100, 250);
                    players.insert(0, p);
                }
                Move::Set(state) => {
                    if who == 0 {
                        cand0 = Some(player_from(state));
                    } else {
                        // (v260) Replacé, pas retourné : il garde le regard de sa dernière
                        // entrée (comme `Remote::look` chez l'hôte).
                        let mut p = player_from(state);
                        if let Some(old) = players.get(&who) {
                            (p.yaw, p.pitch) = (old.yaw, old.pitch);
                        }
                        players.insert(who, p);
                        falls.remove(&who);
                        skip.insert(who);
                    }
                }
                Move::Rule(rule) => match rule {
                    Rule::Hello { pseudo, ok, party } => {
                        if *ok {
                            ready.insert(who);
                            players.entry(who).or_insert_with(|| Player::new(spawn));
                            if !m.players.get(&who).is_some_and(|p| p.present) {
                                settle!(m.join_party(who, pseudo, *party));
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
                            if let Some(d) = fall_check(p, world, f) {
                                hurt.push((id, d));
                            }
                        }
                        for (id, d) in hurt {
                            settle!(m.hurt(None, id, d, Hurt::World));
                        }
                        let places: BTreeMap<u64, Place> = players
                            .iter()
                            .filter(|(id, _)| **id == 0 || ready.contains(id))
                            .map(|(&id, p)| (id, place(p)))
                            .collect();
                        ticks += 1;
                        settle!(m.tick(dt, &places));
                    }
                    Rule::Frame(tick) => {
                        let frame: Frame = players
                            .iter()
                            .filter(|(id, _)| **id == 0 || ready.contains(id))
                            .map(|(&id, p)| (id, p.pos, p.crouch, p.yaw, p.pitch))
                            .collect();
                        now = *tick;
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
                                ..
                            } => {
                                let origin = Vec3::from(*origin);
                                let dirs: Vec<Vec3> = dirs
                                    .iter()
                                    .map(|d| Vec3::from(*d).normalize_or_zero())
                                    .collect();
                                if fire_ok(&mut m, world, who, me, *weapon, origin, dirs.len()) {
                                    let view =
                                        (*view_tick).clamp(now.saturating_sub(MAX_REWIND), now);
                                    let covered = self.history.front().is_some_and(|f| f.0 <= view)
                                        || self.history.len() >= HISTORY;
                                    let frame = pick_frame(&self.history, view);
                                    if !covered || !rays_loaded(world, &frame, origin, &dirs) {
                                        verifiable = false;
                                    } else {
                                        let hits =
                                            shot(&m, &frame, who, *weapon, origin, &dirs, world);
                                        for (victim, hits) in hits {
                                            for h in hits {
                                                settle!(m.hurt(
                                                    Some(who),
                                                    victim,
                                                    h.damage,
                                                    h.hurt
                                                ));
                                            }
                                        }
                                    }
                                }
                            }
                            ToHost::Throw { nade, pos, vel } => {
                                let pos = Vec3::from(*pos);
                                let vel = Vec3::from(*vel);
                                // Il faut porter la grenade (elle est retirée).
                                let ok = if who == 0 {
                                    m.throw(0, *nade)
                                } else {
                                    alive(&m, who)
                                        && players
                                            .get(&who)
                                            .is_some_and(|p| (p.eye() - pos).length() < 3.0)
                                        && vel.length() < 40.0
                                        && m.throw(who, *nade)
                                };
                                if ok {
                                    self.throws.push((t, who, *nade, pos, vel.length(), false));
                                }
                            }
                            ToHost::Buy(item) => {
                                let at = me.map(|p| p.pos);
                                let _ = m.buy(who, *item, at);
                            }
                            ToHost::Sell(item) => {
                                let at = me.map(|p| p.pos);
                                let _ = m.sell(who, *item, at);
                            }
                            ToHost::Action { plant, defuse } => m.keys(who, *plant, *defuse),
                            ToHost::Team(team) => {
                                settle!(m.switch_team(who, *team).into_iter().collect())
                            }
                            ToHost::Drop { weapon, at } => {
                                if let Some(from) = me.map(|p| p.pos) {
                                    m.toss(who, *weapon, from, Vec3::from(*at));
                                }
                            }
                            ToHost::Pickup { id } => {
                                if let Some(at) = me.map(|p| p.pos) {
                                    m.pickup(who, *id, at);
                                }
                            }
                            ToHost::Color(c) => m.set_color(who, *c),
                            ToHost::Door { pos, side } => {
                                let living = alive(&m, who);
                                let cells =
                                    use_door(&mut m, world, me.map(|p| p.pos), living, *pos, *side);
                                self.apply_doors(world, &m, &cells);
                            }
                            ToHost::CallVote { kind, target } => {
                                settle!(m.call_vote(who, *kind, *target).unwrap_or_default())
                            }
                            ToHost::Ballot(yes) => settle!(m.ballot(who, *yes)),
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
                            settle!(m.hurt(Some(owner), victim, d, h));
                        }
                    }
                    Rule::SelfHurt(amount) => {
                        if who != 0 {
                            wrong.push("dégâts « à soi » d'un autre joueur");
                        } else {
                            settle!(m.hurt(None, 0, *amount, Hurt::World));
                        }
                    }
                },
            }
        }
        self.cand0 = cand0;
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
        // Personne n'avance plus vite que le temps : une entrée par pas, à la marge près.
        if inputs.values().any(|&n| n > ticks + INPUT_BURST + 8) {
            wrong.push("joueur plus rapide que le temps");
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
            // Un joueur mort ou figé qui bouge : peut venir d'un décalage d'une image entre
            // l'hôte et le vérificateur sous forte charge. Soupçon seulement, jamais une
            // preuve (qui peut mener au bannissement automatique).
            let soft = w == DEAD_MOVE;
            out.push(Conflict {
                proof: !soft,
                ..Conflict::proof(w)
            });
        }
        for c in &out {
            log::warn!("vérificateur : {} (joueur {})", c.why, c.player);
        }
        // Scripts trouvés dans les sauts (preuve sur les entrées mêmes du joueur).
        for (who, f) in self.hops.take_found() {
            if let Some(p) = m.players.get(&who) {
                log::warn!("vérificateur : « {} » : {}", p.pseudo, f.detail());
                self.cheats.push((p.pseudo.clone(), f));
            }
        }
        out
    }
}

/// Garant arbitre : rejoue les pièces d'une affaire (ce que le vérificateur a reçu, dans
/// l'ordre : demandes des joueurs attestées par le relais, copies de la partie par
/// l'hôte), exactement comme un vérificateur. Vrai si l'hôte a triché sans appel possible.
/// `around` charge le monde autour d'un point.
pub fn arbitrate(
    items: &[(Option<u64>, Vec<u8>)],
    world: &mut World,
    doors: &[game::Zone],
    mut around: impl FnMut(&mut World, Vec3),
) -> bool {
    let mut auditor = Auditor {
        witnessing: true,
        ..Default::default()
    };
    let mut prev: Option<Backup> = None;
    let mut guilty = false;
    for (who, data) in items {
        match who {
            Some(peer) => {
                if let Some(msg) = game::decode::<ToHost>(data) {
                    auditor.witness(*peer, msg);
                }
            }
            None => {
                // (v258) Avec les portes posées pour la partie (`Backup::with_doors`).
                let Some(new) = game::decode::<Backup>(data).map(|b| b.with_doors(doors)) else {
                    continue;
                };
                for p in &new.players {
                    around(world, Vec3::from(p.2.pos));
                }
                match &prev {
                    Some(p) => {
                        let found = auditor.audit(p, &new, world);
                        guilty |= found.iter().any(|c| c.proof);
                    }
                    None => auditor.first_copy(),
                }
                prev = Some(new);
            }
        }
    }
    guilty
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
fn audit(prev: &Backup, new: &Backup, world: &mut World) -> Vec<Conflict> {
    Auditor::default().audit(prev, new, world)
}

/// Taille des morceaux de la copie envoyée au garant de secours.
const BACKUP_PART: usize = 32 * 1024;

/// Garant de secours : état reçu toutes les…
const BACKUP_EVERY: u32 = 256;

/// Positions de tous les joueurs à un pas : (joueur, pieds, accroupi, cap du regard, hauteur
/// du regard). (v234) Le cap : accroupi, la tête avance avec le cou, vers le regard
/// (`hitbox`). (v256) La hauteur du regard : les boîtes des coups sont les pavés du modèle,
/// dont la tête et les bras suivent le regard (`ray_model`). Le regard est celui de la
/// dernière entrée jouée de chacun (`Remote::look`, `Host::look0`) : l'hôte et le vérificateur
/// le tirent du même journal, au bit près.
type Frame = Vec<(u64, Vec3, f32, f32, f32)>;

/// Bruits de pas : au-dessus de cette vitesse au sol (blocs/s), on court et les autres
/// l'entendent (135 unités de CS sur 250).
pub const STEP_SPEED: f32 = crate::player::FPS_SPRINT * 135.0 / 250.0;

/// Part de sa propre vitesse de course (selon l'arme en main) à atteindre pour que ses
/// pas s'entendent : en prenant de la vitesse, la course reste silencieuse jusque-là.
pub const STEP_SHARE: f32 = 0.8;

/// Vitesse au sol (blocs/s) à partir de laquelle les pas d'un joueur qui tient une arme
/// de vitesse `carry` (unités de CS, 0 : 250) s'entendent : `STEP_SHARE` de sa vitesse de
/// course, et jamais sous `STEP_SPEED` (marcher reste silencieux, comme dans CS).
pub fn step_speed(carry: u8) -> f32 {
    let carry = if carry == 0 { 250.0 } else { carry as f32 } / 250.0;
    (crate::player::FPS_SPRINT * carry * STEP_SHARE).max(STEP_SPEED)
}
pub use minelib_net::game::{STEP_LADDER, STEP_LAND, STEP_PAIN, STEP_RUN, STEP_SPLASH};

/// Hauteur de chute dans l'eau à partir de laquelle on entend le plongeon (blocs).
pub const SPLASH_HEIGHT: f32 = 1.0;

/// Chute d'au moins cette hauteur (blocs, du plus haut point atteint au sol où l'on
/// retombe, saut compris) : bruit de réception, entendu des autres.
pub const LAND_HEIGHT: f32 = 1.3;

/// Suivi d'un joueur pour ses bruits : dernière position, distance courue depuis le dernier
/// pas, plus haut point atteint depuis qu'il a quitté le sol.
#[derive(Clone, Copy)]
struct Stride {
    last: Vec3,
    dist: f32,
    top: Option<f32>,
    /// Hauteur grimpée à pleine vitesse sur une échelle depuis le dernier bruit.
    climb: f32,
}

/// Distance entre deux pas (blocs).
pub const STRIDE: f32 = crate::player::FPS_SPRINT * 0.34;
/// Portée des bruits de pas (blocs).
pub const HEARD_STEPS: f32 = 32.0;

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
    /// Le joueur local est accroché à une échelle.
    local_ladder: bool,
    /// État du joueur de l'hôte au dernier pas (ses yeux, pour ses tirs).
    local_state: SimState,
    /// Brûlure en cours de chacun (fraction de point).
    burns: BTreeMap<u64, f32>,
    /// Touches maintenues de l'hôte (notées au journal quand elles changent).
    local_keys: (bool, bool),
    /// Copie à envoyer au prochain pas (fin de partie).
    backup_now: bool,
    /// Fumées en cours (centre, rayon), joueur de l'hôte aveuglé, et marques de
    /// l'élimination en cours (pour le fil des éliminations).
    smokes: Vec<(Vec3, f32)>,
    local_blind: bool,
    /// Vitesse permise par ce que tient le joueur de l'hôte (unités de CS).
    local_carry: u8,
    /// (v165) Ping de chaque joueur (ms), qu'il mesure et envoie : montré dans le tableau
    /// des scores, hors des règles (le vérificateur n'a pas à le rejouer).
    pings: HashMap<u64, u16>,
    /// Bruits de pas et de réception, par joueur.
    strides: HashMap<u64, Stride>,
    /// Blessés par une chute à cette image (cri de douleur, entendu de tous).
    pains: Vec<u64>,
    kill_flags: u8,
    /// Copies envoyées d'elles-mêmes (les essais les prennent à la main).
    auto_backup: bool,
    out: Vec<Local>,
    /// Les armes au sol (dans les règles) ont changé : à renvoyer à tous.
    ground_dirty: bool,
    /// Dernier rappel de ses armes envoyé à chacun (pas).
    resync: HashMap<u64, u32>,
    /// Qui a vu qui (spectateur, cible) : dernier pas où il le voyait.
    seen: std::cell::RefCell<HashMap<(u64, u64), u32>>,
    /// Garant de secours actuel, et celui désigné par le relais (garant indépendant).
    backup: Option<u64>,
    designated: Option<u64>,
    /// L'hôte joue lui-même (sinon : garant indépendant, sans personnage).
    playing: bool,
    /// (v260) Regard (cap, hauteur) de la dernière entrée notée de l'hôte qui joue
    /// (`Move::Own`) : celui de ses boîtes des coups (`Frame`), comme `Remote::look`.
    look0: (f32, f32),
    /// Journal des coups depuis la dernière copie, et tête de la chaîne.
    log: Vec<(u32, u64, Move)>,
    chain: [u8; 32],
    /// Dernier tir de chaque joueur (pas) : un tireur s'entend même caché.
    last_shot: HashMap<u64, u32>,
    /// Position montrée de chaque joueur distant : elle suit sa position simulée en douceur
    /// (ses entrées arrivent par paquets : sans cela, il avance par sauts à l'écran).
    shown: HashMap<u64, Vec3>,
    /// (v172) Partie solo contre des bots : pas de relais (rien n'est envoyé), et les bots
    /// qui jouent comme des invités.
    solo: bool,
    bots: Option<Box<crate::bots::Bots>>,
    /// (v209) Graine des noms des bots (ceux ajoutés en cours de partie prennent la suite).
    bot_seed: u32,
    /// (v174) Partie solo regardée en spectateur : le joueur local en est sorti (manches
    /// jouées à ce moment, et s'il était en vie), les bots continuent entre eux.
    away: Option<(u32, bool)>,
    /// (v243) Partie d'un serveur de construction, menée par un OP : ses bots, les joueurs du
    /// serveur en mode FPS, sans classement ni vérificateur (ni copies, ni journal).
    build: bool,
    /// Voie rapide (`minelib_net::fast`) : le bout par lequel partent les positions quand
    /// elle est ouverte (rien : tout passe par le relais), et les équipes pour lesquelles
    /// des passeurs ont été demandés au relais (`fast_want`).
    fast: Option<minelib_net::fast::Endpoint>,
    fast_groups: Vec<Vec<u64>>,
    /// (v274) Demi-secondes de suite où un passeur (joueur, numéro) a été trouvé muet ou
    /// lent (`fast_check`).
    fast_marks: HashMap<(u64, u8), u8>,
}

pub struct Guest {
    link: Sender<Inbound>,
    /// Voie rapide (`minelib_net::fast`) : le bout par lequel les entrées partent aussi.
    fast: Option<minelib_net::fast::Endpoint>,
    /// Pas du dernier état de soi pris en compte, et pas le plus récent reçu de l'hôte :
    /// par la voie rapide, un message peut en doubler un autre ; un plus ancien ne doit ni
    /// ramener la prédiction en arrière ni passer pour un à-coup du réseau.
    own_tick: Option<u32>,
    seen_tick: Option<u32>,
    /// Voie rapide : messages des passeurs comptés depuis le dernier avis, ceux qui sont
    /// arrivés après ce qu'ils annonçaient (le relais avait été plus rapide), et l'avis en
    /// cours : vrai si l'on reçoit plus vite par le relais (dit à l'hôte, qui n'envoie plus
    /// alors les positions par les passeurs).
    fast_votes: u8,
    fast_late: u8,
    fast_slow: bool,
    /// (v274) Pas du dernier état complet pris en compte : ils arrivent par le relais et
    /// par les passeurs, le second (ou un plus ancien) est ignoré.
    snap_tick: Option<u32>,
    /// Sa vie et s'il est vivant, d'après le dernier état complet reçu.
    vitals: Option<(u8, bool)>,
    /// Sa mort vient d'être apprise par un état complet arrivé après des positions plus
    /// récentes (voie rapide) : les entrées en attente sont à oublier (`reconcile`).
    died: bool,
    /// Images jouées, et celles où les nouvelles de l'hôte avaient plus de `STALL_TICKS`
    /// de retard (dit au journal toutes les 30 s, puis remis à zéro).
    net_frames: u32,
    net_stalled: u32,
    net_since: std::time::Instant,
    you: Option<u64>,
    seq: u32,
    /// Entrées pas encore confirmées par l'hôte, avec l'état prédit après chacune.
    pending: VecDeque<(u32, NetInput, SimState)>,
    outbox: Vec<NetInput>,
    outbox_first: u32,
    /// Derniers états reçus des autres joueurs (pas, joueurs).
    snaps: VecDeque<(u32, Vec<NetPlayer>)>,
    /// Joueurs du dernier état complet (pseudo, vie, arme… entre deux).
    full: Vec<NetPlayer>,
    /// Aveuglé par une flash (dit à l'hôte avec chaque tir, pour le fil des éliminations).
    blind: bool,
    /// Dernier état reçu pour soi : (pas confirmé, état, vie, vivant).
    latest: Option<(u32, SimState, u8, bool)>,
    /// Horloge de l'hôte estimée (pas, avec fraction).
    clock: f32,
    /// Retard d'affichage des autres joueurs (pas) : grandit avec les à-coups du réseau
    /// pour toujours avoir deux états entre lesquels glisser (sinon ils sautillent).
    delay: f32,
    /// Plus grand retard d'arrivée récent d'un état (pas), qui s'oublie lentement.
    late_peak: f32,
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
    /// (v258) Portes posées pour la partie de la carte (`FpsMap::door_list`, de son
    /// `fps.txt`) : remises dans chaque copie reçue (`Backup::with_doors`).
    map_doors: Vec<game::Zone>,
    /// Désaccords trouvés, à envoyer au relais.
    pub conflicts: Vec<Conflict>,
    /// Présentation renvoyée tant que l'hôte n'a pas répondu.
    hello_timer: f32,
    /// (v220) Son monde n'est pas encore chargé : il ne se présente pas (voir
    /// `NetGame::guest_loading`).
    loading: bool,
    /// Positions prédites par numéro d'entrée (pour juger un désaccord sur soi).
    predicted: VecDeque<(u32, [f32; 3])>,
    /// (v165) Envois d'entrées (dernier numéro, quand) pas encore confirmés : leur
    /// confirmation donne le ping avec l'hôte (lissé, ms), dit à l'hôte chaque seconde.
    sent: VecDeque<(u32, std::time::Instant)>,
    ping: Option<f32>,
    ping_timer: f32,
}

pub enum NetGame {
    Host(Box<Host>),
    Guest(Box<Guest>),
}

fn send(link: &Sender<Inbound>, req: &Request) {
    transport::push(link, req);
}

/// (v225) Demi-côté de la boîte de la tête (blocs) : un peu plus que la moitié de la tête
/// du modèle (`player_model::HEAD_BLOCKS`, 0,27 bloc depuis la v234), qui tourne avec le
/// regard. (v234) Accroupi, la tête avance avec le cou vers le regard : cette boîte la suit
/// (`hitbox`).
const HEAD_HALF: f32 = 0.15;
/// (v256) Ces boîtes ne servent plus qu'au couteau ; les balles touchent les pavés du modèle
/// (`ray_model`).
/// Boîtes (min, max) d'un joueur : son corps (jusqu'au bas de la tête) et sa tête ; hauteur
/// du bas de sa tête. (v225) Aux proportions de CS2 du modèle : la tête fait un septième de
/// la hauteur (0,3 bloc avant ; v234 : 7,5 % de plus, `player_model::HEAD_BLOCKS`), dans sa
/// propre boîte, plus étroite que le corps (une balle qui passe à côté d'elle, à sa hauteur,
/// ne touche pas). (v234) Accroupi, la tête du modèle avance avec le cou vers le regard
/// (`yaw`) : sa boîte la suit (`player_model::head_place`), à sa hauteur.
fn hitbox(feet: Vec3, crouch: f32, yaw: f32) -> ([Vec3; 2], [Vec3; 2], f32) {
    let (ahead, bottom) = crate::player_model::head_place(crouch);
    let head_y = feet.y + bottom;
    let c = feet + Vec3::new(yaw.sin(), 0.0, -yaw.cos()) * ahead;
    let body = [
        feet - Vec3::new(HALF_WIDTH, 0.0, HALF_WIDTH),
        Vec3::new(feet.x + HALF_WIDTH, head_y, feet.z + HALF_WIDTH),
    ];
    let head = [
        Vec3::new(c.x - HEAD_HALF, head_y, c.z - HEAD_HALF),
        Vec3::new(
            c.x + HEAD_HALF,
            head_y + crate::player_model::HEAD_BLOCKS,
            c.z + HEAD_HALF,
        ),
    ];
    (body, head, head_y)
}

/// (v225) Distance du rayon à un joueur (boîtes de `hitbox`), s'il le touche, et s'il le
/// touche d'abord à la tête.
fn ray_player(o: Vec3, d: Vec3, body: [Vec3; 2], head: [Vec3; 2]) -> Option<(f32, bool)> {
    match (
        ray_box(o, d, body[0], body[1]),
        ray_box(o, d, head[0], head[1]),
    ) {
        (Some(b), Some(h)) => Some(if h < b { (h, true) } else { (b, false) }),
        (Some(b), None) => Some((b, false)),
        (None, Some(h)) => Some((h, true)),
        (None, None) => None,
    }
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

/// (v256) Où une balle touche un joueur : distance et partie du corps. « Les hitbox devront
/// être celles de celui-ci aussi » : ses boîtes sont les sept pavés du modèle, tournés avec lui
/// (`player_model::hit_boxes` : tête qui suit le regard, corps penché accroupi, bras tendus
/// vers le regard). Une balle qui passe entre ses jambes, ou à côté de son ventre sous la
/// poitrine, ne le touche pas.
fn ray_model(
    o: Vec3,
    d: Vec3,
    feet: Vec3,
    crouch: f32,
    yaw: f32,
    pitch: f32,
) -> Option<(f32, HitZone)> {
    // Loin de lui : rien à calculer (il tient dans 1,3 bloc autour de son milieu).
    let to = feet + Vec3::Y * (HEIGHT / 2.0) - o;
    if (to - d * to.dot(d)).length_squared() > 1.3 * 1.3 * d.length_squared().max(1e-6) {
        return None;
    }
    let mut best: Option<(f32, HitZone)> = None;
    for (inv, min, max, zone) in crate::player_model::hit_boxes(feet, yaw, pitch, crouch) {
        // Le rayon dans le repère du pavé : les distances le long du rayon restent les mêmes.
        let (lo, ld) = (inv.transform_point3(o), inv.transform_vector3(d));
        let (mut near, mut far) = (0.0f32, f32::INFINITY);
        let mut out = false;
        for i in 0..3 {
            if ld[i].abs() < 1e-9 {
                out |= lo[i] < min[i] || lo[i] > max[i];
            } else {
                let (a, b) = ((min[i] - lo[i]) / ld[i], (max[i] - lo[i]) / ld[i]);
                near = near.max(a.min(b));
                far = far.min(a.max(b));
            }
        }
        if !out && far >= near && best.is_none_or(|(t, _)| near < t) {
            best = Some((near, zone));
        }
    }
    best
}

/// Dégâts d'un coup selon l'endroit touché : tête ×4, jambes ×0,75 (balles). (v256) `zone` :
/// le pavé du modèle touché (`ray_model`). Avant, la hauteur du point touché dans une boîte
/// droite.
fn damage_at(weapon: u8, dist: f32, zone: HitZone) -> (u32, Hurt) {
    if weapon == KNIFE_LIGHT || weapon == KNIFE_HEAVY {
        let d = if weapon == KNIFE_HEAVY { 55 } else { 34 };
        return (d, Hurt::Knife);
    }
    // Comme dans CS2 : dégâts de l'arme, perdus avec la distance, puis partie du corps
    // (tête ×4, ventre ×1,25, poitrine et bras ×1, jambes ×0,75). Le gilet et le casque
    // sont comptés par les règles.
    let base = WEAPONS
        .get(weapon as usize)
        .map_or(30.0, |w| w.damage_at(dist));
    let (head, legs) = (zone == HitZone::Head, zone == HitZone::Leg);
    let d = match zone {
        HitZone::Head => base * 4.0,
        HitZone::Leg => base * 0.75,
        HitZone::Belly => base * 1.25,
        HitZone::Chest | HitZone::Arm => base,
    };
    (d as u32, Hurt::Bullet { weapon, head, legs })
}

/// Le segment `a`–`b` passe-t-il à moins de `r` de `c` ?
fn segment_near(a: Vec3, b: Vec3, c: Vec3, r: f32) -> bool {
    let ab = b - a;
    let t = ((c - a).dot(ab) / ab.length_squared().max(1e-6)).clamp(0.0, 1.0);
    (a + ab * t - c).length() < r
}

/// Une élimination du fil : qui, par qui (vide : personne), avec quoi et comment.
#[derive(Clone, Debug, PartialEq)]
pub struct Kill {
    pub killer: String,
    pub victim: String,
    pub weapon: u8,
    pub head: bool,
    /// `game::KILL_WALL`, `KILL_SMOKE`, `KILL_BLIND`.
    pub flags: u8,
    /// Équipes du tueur (255 : personne) et du mort.
    pub teams: [u8; 2],
}

/// Nom de l'arme d'une élimination.
pub fn weapon_name(weapon: u8) -> &'static str {
    match weapon {
        cs::W_KNIFE | KNIFE_HEAVY => "couteau",
        cs::W_HE => "grenade",
        cs::W_FIRE => "feu",
        cs::W_BOMB => "bombe",
        cs::W_WORLD => "chute",
        w => WEAPONS.get(w as usize).map_or("?", |w| w.name),
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
            local_ladder: false,
            local_state: Player::new(spawn).net_state(),
            burns: BTreeMap::new(),
            local_keys: (false, false),
            backup_now: false,
            smokes: Vec::new(),
            local_blind: false,
            local_carry: 250,
            pings: HashMap::new(),
            strides: HashMap::new(),
            pains: Vec::new(),
            kill_flags: 0,
            auto_backup: true,
            out: Vec::new(),
            ground_dirty: false,
            resync: HashMap::new(),
            seen: Default::default(),
            backup: None,
            designated: None,
            playing: true,
            look0: (0.0, 0.0),
            log: Vec::new(),
            chain: [0; 32],
            last_shot: HashMap::new(),
            shown: HashMap::new(),
            solo: false,
            bots: None,
            bot_seed: 0,
            away: None,
            build: false,
            fast: None,
            fast_groups: Vec::new(),
            fast_marks: HashMap::new(),
        };
        h.events(ev, &mut out);
        h.out = out;
        NetGame::Host(Box::new(h))
    }

    /// (v243) Partie FPS d'un serveur de construction, dont ce jeu (celui de l'OP qui l'a
    /// ouverte) est l'hôte : comme la partie solo contre des bots (`players` joueurs en tout,
    /// soi compris, complétés par des bots ; elle commence tout de suite), mais en ligne : les
    /// joueurs du serveur en mode FPS y entrent comme des invités (le relais leur passe ses
    /// messages). Pas de classement, ni vérificateur, ni copie de la partie.
    pub fn build_host(
        link: Sender<Inbound>,
        pseudo: String,
        map: FpsMap,
        spawn: Vec3,
        nav: Option<std::sync::mpsc::Receiver<std::sync::Arc<crate::bots::Nav>>>,
        level: u8,
        players: usize,
    ) -> Self {
        let mut g = NetGame::host(link, pseudo, map, [0; 32], spawn);
        if let NetGame::Host(h) = &mut g {
            h.build = true;
            h.auto_backup = false;
            let seed = h.rand();
            let mut bots = crate::bots::Bots::new(seed, level);
            if let Some(rx) = nav {
                bots.set_building(rx);
            }
            let names = crate::bots::Bots::names(players.saturating_sub(1), seed);
            for (i, name) in names.into_iter().enumerate() {
                let id = crate::bots::BOT_BASE + i as u64 + 1;
                bots.add(id);
                h.add_bot(id, name);
            }
            h.bot_seed = seed;
            h.bots = Some(Box::new(bots));
            h.spawns.clear();
            let ev = h.m.start_now();
            let mut out = Vec::new();
            h.events(ev, &mut out);
            h.out.extend(out);
        }
        g
    }

    /// (v243) Partie FPS d'un serveur de construction (hôte ou invité).
    pub fn is_build(&self) -> bool {
        matches!(self, NetGame::Host(h) if h.build)
    }

    /// (v172) Partie solo contre des bots (mode FPS dans un monde solo) : ce jeu en est
    /// l'hôte, sans relais ; la carte se remplit de bots (`map.players()` joueurs en tout),
    /// la partie démarre et chacun est placé à son départ. `nav` : relevé de la carte en
    /// préparation (voir `bots::nav_in_background`).
    pub fn solo(
        pseudo: String,
        map: FpsMap,
        spawn: Vec3,
        nav: Option<std::sync::mpsc::Receiver<std::sync::Arc<crate::bots::Nav>>>,
        level: u8,
    ) -> Self {
        // Personne au bout du fil : les messages pour les autres ne partent nulle part.
        let (link, _) = std::sync::mpsc::channel();
        let players = map.players() as usize;
        let mut g = NetGame::host(link, pseudo, map, [0; 32], spawn);
        if let NetGame::Host(h) = &mut g {
            h.solo = true;
            h.auto_backup = false;
            let seed = h.rand();
            let mut bots = crate::bots::Bots::new(seed, level);
            if let Some(rx) = nav {
                bots.set_building(rx);
            }
            // (v209) Les joueurs choisis (`/bot maxbot`), sinon ceux de la carte ; soi compris.
            let players = crate::bots::saved_players().unwrap_or(players);
            let names = crate::bots::Bots::names(players.saturating_sub(1), seed);
            for (i, name) in names.into_iter().enumerate() {
                let id = crate::bots::BOT_BASE + i as u64 + 1;
                bots.add(id);
                h.add_bot(id, name);
            }
            h.bot_seed = seed;
            h.bots = Some(Box::new(bots));
            // Tout le monde est là : la partie commence tout de suite, chacun à son départ.
            h.spawns.clear();
            let ev = h.m.start_now();
            let mut out = Vec::new();
            h.events(ev, &mut out);
            h.out.extend(out);
        }
        g
    }

    /// (v209) Partie solo : autant de bots que `want`, ajoutés ou retirés tout de suite,
    /// sans recommencer la partie (`/bot`, `/bot maxbot`). Rend le nombre de bots.
    pub fn set_bot_count(&mut self, want: usize) -> usize {
        let NetGame::Host(h) = self else {
            return 0;
        };
        // (v243) Aussi la partie d'un serveur de construction (son hôte).
        if !h.solo && !h.build {
            return 0;
        }
        let Some(mut bots) = h.bots.take() else {
            return 0;
        };
        let mut ids: Vec<u64> = h
            .peers
            .keys()
            .copied()
            .filter(|&id| id > crate::bots::BOT_BASE)
            .collect();
        ids.sort_unstable();
        if ids.len() < want {
            let used: Vec<String> = h.peers.values().map(|r| r.pseudo.clone()).collect();
            let mut names = crate::bots::Bots::names(want + used.len() + 4, h.bot_seed)
                .into_iter()
                .filter(|n| !used.contains(n));
            let mut next = ids.last().copied().unwrap_or(crate::bots::BOT_BASE) + 1;
            while ids.len() < want {
                let Some(name) = names.next() else { break };
                bots.add(next);
                h.add_bot(next, name);
                ids.push(next);
                next += 1;
            }
        } else {
            for id in ids.split_off(want) {
                bots.remove(id);
                if let Some(r) = h.peers.remove(&id) {
                    h.m.remove_bot(id, Some(r.player.pos));
                }
                h.shown.remove(&id);
                h.strides.remove(&id);
            }
        }
        h.bots = Some(bots);
        ids.len()
    }

    /// (v209) Niveau des bots changé en cours de partie solo (sans la recommencer).
    pub fn set_bot_level(&mut self, level: u8) {
        if let NetGame::Host(h) = self {
            if let Some(b) = h.bots.as_mut() {
                b.set_level(level);
            }
        }
    }

    /// (v209) Bots de la partie solo.
    pub fn bot_count(&self) -> usize {
        match self {
            NetGame::Host(h) if h.solo || h.build => h
                .peers
                .keys()
                .filter(|&&id| id > crate::bots::BOT_BASE)
                .count(),
            _ => 0,
        }
    }

    /// (v243) Joueurs de la partie qui ne sont pas des bots (l'hôte compris s'il joue).
    pub fn human_count(&self) -> usize {
        match self {
            NetGame::Host(h) => {
                usize::from(h.playing)
                    + h.peers
                        .keys()
                        .filter(|&&id| id <= crate::bots::BOT_BASE)
                        .count()
            }
            NetGame::Guest(_) => 1,
        }
    }

    /// (v209) Partie solo encore à l'échauffement : elle commence tout de suite (`/start`).
    /// Rend vrai si elle a commencé.
    pub fn start_now(&mut self) -> bool {
        let NetGame::Host(h) = self else {
            return false;
        };
        if (!h.solo && !h.build) || h.m.phase != game::phase::WARMUP {
            return false;
        }
        let ev = h.m.start_now();
        let mut out = Vec::new();
        h.events(ev, &mut out);
        h.out.extend(out);
        true
    }

    /// (v209) Partie solo, mort, en regardant un bot de son équipe : on prend sa place (son
    /// corps, sa vie, ses armes et leurs munitions, la bombe s'il la porte) ; le bot sort
    /// de la manche, sans mort comptée. Rend vrai si c'est fait.
    pub fn take_over(&mut self, pseudo: &str) -> bool {
        let NetGame::Host(h) = self else {
            return false;
        };
        if !h.solo {
            return false;
        }
        let Some((&id, r)) = h
            .peers
            .iter()
            .find(|(&id, r)| id > crate::bots::BOT_BASE && r.pseudo == pseudo)
        else {
            return false;
        };
        let mut player = r.player.clone();
        player.vel = Vec3::ZERO;
        let hands = h
            .bots
            .as_ref()
            .and_then(|b| b.hands(id))
            .unwrap_or_default();
        if !h.m.take_over(0, id) {
            return false;
        }
        let Some(me) = h.m.players.get(&0) else {
            return false;
        };
        let (hp, guns, nades) = (me.hp, me.guns, me.nades);
        let state = player.net_state();
        h.local_pos = player.pos;
        h.local_state = state;
        h.record(0, Move::Set(state));
        h.out.push(Local::SetHp(hp));
        h.out.push(Local::Guns { guns, nades });
        h.out.push(Local::TakeOver {
            state,
            bot: pseudo.to_string(),
            held: hands.weapon,
            ammo: hands.ammo,
        });
        true
    }

    /// (v209) Le joueur `pseudo` est un bot (partie solo).
    pub fn is_bot(&self, pseudo: &str) -> bool {
        match self {
            NetGame::Host(h) => h
                .peers
                .iter()
                .any(|(&id, r)| id > crate::bots::BOT_BASE && r.pseudo == pseudo),
            NetGame::Guest(_) => false,
        }
    }

    /// (v209) Ce que l'on sait du joueur `pseudo`, regardé en spectateur : de quoi montrer
    /// son écran comme si c'était soi qui jouait (vie, équipement, arme en main ; pour un
    /// bot, ses munitions). L'hôte connaît tout l'équipement ; un invité, la vie et l'arme
    /// en main.
    pub fn watched(&self, pseudo: &str) -> Option<crate::fps::Watched> {
        use crate::fps::{Online, Watched};
        match self {
            NetGame::Host(h) => {
                let (&id, r) = h.peers.iter().find(|(_, r)| r.pseudo == pseudo)?;
                let p = h.m.players.get(&id)?;
                let hands = h.bots.as_ref().and_then(|b| b.hands(id));
                let held = hands.as_ref().map_or(r.weapon, |b| b.weapon);
                let ammo = hands
                    .as_ref()
                    .and_then(|b| b.ammo.iter().find(|(w, _)| *w == held).map(|a| a.1));
                Some(Watched {
                    hp: p.hp,
                    weapon: if hands.is_some() { held } else { r.weapon },
                    guns: Some(p.guns),
                    nades: Some(p.nades),
                    online: Online {
                        team: p.team,
                        money: p.money,
                        armor: p.armor,
                        helmet: p.helmet,
                        kit: p.kit,
                        bomb: h.m.carrier == Some(id),
                        can_buy: false,
                        warmup: h.m.phase == game::phase::WARMUP,
                    },
                    ammo,
                    reload: hands.as_ref().and_then(|b| b.reload),
                    scoped: hands.as_ref().is_some_and(|b| b.scoped),
                })
            }
            NetGame::Guest(g) => {
                let p = g.full.iter().find(|p| p.pseudo == pseudo)?;
                let snap = g
                    .snaps
                    .back()
                    .and_then(|(_, ps)| ps.iter().find(|q| q.pseudo == pseudo));
                let weapon = snap.map_or(p.weapon, |q| q.weapon);
                let flags = snap.map_or(p.flags, |q| q.flags);
                Some(Watched {
                    hp: snap.map_or(p.hp, |q| q.hp) as u32,
                    weapon,
                    guns: None,
                    nades: None,
                    online: Online {
                        team: p.team,
                        money: g.hud.money,
                        kit: flags & game::FLAG_KIT != 0,
                        bomb: flags & game::FLAG_BOMB != 0,
                        ..Online::default()
                    },
                    ammo: None,
                    reload: None,
                    scoped: false,
                })
            }
        }
    }

    /// (v174) Partie solo contre des bots : le joueur local la regarde en spectateur
    /// (`away` : il en sort, la bombe tombe s'il l'avait, les bots continuent entre eux)
    /// ou y revient tout de suite, là où il se trouve (`Match::step_in`). Rend s'il est en
    /// vie dans la partie.
    pub fn set_away(&mut self, away: bool) -> bool {
        let NetGame::Host(h) = self else {
            return true;
        };
        if h.solo {
            if away && h.away.is_none() {
                let alive = h.m.players.get(&0).is_some_and(|p| p.alive);
                h.away = Some((h.m.played, alive));
                let at = h.local_pos;
                h.m.step_out(0, Some(at));
            } else if !away {
                if let Some((played, was_alive)) = h.away.take() {
                    let team = h.m.players.get(&0).map_or(game::T, |p| p.team);
                    match h.m.step_in(0, played == h.m.played, was_alive) {
                        Some(crate::cs::Resume::AsBefore) => h.out.push(Local::Resume {
                            fresh: false,
                            reset: false,
                            team,
                        }),
                        Some(crate::cs::Resume::Fresh { reset }) => {
                            h.out.push(Local::Resume {
                                fresh: true,
                                reset,
                                team,
                            });
                        }
                        None if !h.m.players.contains_key(&0) => {
                            // Plus dans la partie : il la rejoint comme un nouveau joueur.
                            let pseudo = h.pseudo.clone();
                            let ev = h.m.join_party(0, &pseudo, 0);
                            let mut out = Vec::new();
                            h.events(ev, &mut out);
                            h.out.extend(out);
                        }
                        None => {}
                    }
                }
            }
        }
        h.m.players.get(&0).is_some_and(|p| p.alive)
    }

    /// (v172) Partie solo contre des bots.
    pub fn is_solo(&self) -> bool {
        matches!(self, NetGame::Host(h) if h.solo)
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

    /// (v220) Invité dont le monde n'est pas encore chargé (voir `Session::loading`) : il
    /// ne se présente pas à l'hôte, qui ne le met donc pas dans la partie (personne ne le
    /// voit apparaître) tant que `loaded` n'a pas été appelé.
    pub fn guest_loading(link: Sender<Inbound>, map_sig: [u8; 32]) -> Self {
        let mut g = Self::guest_quiet(link, map_sig);
        if let NetGame::Guest(x) = &mut g {
            x.hello_timer = f32::INFINITY;
            x.loading = true;
        }
        g
    }

    /// (v220) Le monde de l'invité est chargé : il se présente à l'hôte tout de suite.
    pub fn loaded(&mut self) {
        if let NetGame::Guest(g) = self {
            if g.loading {
                g.loading = false;
                if !g.auditor && g.you.is_none() {
                    g.hello_timer = 0.0;
                }
            }
        }
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
            full: Vec::new(),
            blind: false,
            delay: INTERP_TICKS,
            late_peak: 0.0,
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
            map_doors: Vec::new(),
            conflicts: Vec::new(),
            hello_timer: 1.0,
            loading: false,
            sent: VecDeque::new(),
            ping: None,
            ping_timer: 1.0,
            predicted: VecDeque::new(),
            fast: None,
            fast_votes: 0,
            fast_late: 0,
            fast_slow: false,
            own_tick: None,
            seen_tick: None,
            snap_tick: None,
            vitals: None,
            died: false,
            net_frames: 0,
            net_stalled: 0,
            net_since: std::time::Instant::now(),
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

    /// Pour le fil des éliminations : le joueur de ce PC est-il aveuglé, et où sont les
    /// fumées (centre, rayon) ? À donner à chaque image.
    pub fn set_sight(&mut self, blind: bool, smokes: Vec<(Vec3, f32)>) {
        match self {
            NetGame::Host(h) => {
                h.local_blind = blind;
                h.smokes = smokes;
            }
            NetGame::Guest(g) => g.blind = blind,
        }
    }

    /// Les autres joueurs, obstacles pour le joueur de ce PC (à donner à son `Player`
    /// avant chaque pas) : exacts chez l'hôte, tels qu'on les voit chez un invité.
    pub fn obstacles(&self) -> Vec<Other> {
        match self {
            NetGame::Host(h) => others_except(&h.solid_feet(), 0),
            NetGame::Guest(g) => {
                let list: Vec<Vec3> = g
                    .views()
                    .iter()
                    .filter(|v| v.alive)
                    .map(|v| v.pos)
                    .collect();
                crate::player::others_from(&list)
            }
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

    /// Triches prouvées par le vérificateur en rejouant (à envoyer au relais).
    pub fn take_cheats(&mut self) -> Vec<(String, crate::anticheat::Finding)> {
        match self {
            NetGame::Guest(g) => std::mem::take(&mut g.audit.cheats),
            NetGame::Host(_) => Vec::new(),
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
            let doors = h.m.map.door_list.clone();
            *self = NetGame::guest(link, sig);
            self.set_map_doors(doors);
        }
    }

    /// (v258) Portes posées pour la partie de la carte de ce jeu (`FpsMap::door_list`) : un
    /// invité les remet dans les copies de la partie qu'il reçoit (garant, vérificateur).
    pub fn set_map_doors(&mut self, doors: Vec<game::Zone>) {
        if let NetGame::Guest(g) = self {
            g.map_doors = doors;
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

    /// Voie rapide (`minelib_net::fast`) : le bout que le relais vient de donner à ce jeu
    /// pour cette partie (rien : voie fermée, tout repasse par le relais).
    pub fn set_fast(&mut self, end: Option<minelib_net::fast::Endpoint>) {
        match self {
            NetGame::Host(h) => {
                h.fast = end;
                h.fast_marks.clear();
            }
            NetGame::Guest(g) => {
                // Nouvelles routes, nouveaux passeurs : l'avis repart de zéro.
                g.fast = end;
                g.fast_votes = 0;
                g.fast_late = 0;
                g.fast_slow = false;
            }
        }
    }

    /// La voie rapide est ouverte (invité : vers l'hôte ; hôte : vers au moins un joueur).
    #[cfg(test)]
    pub fn fast_open(&self) -> bool {
        match self {
            NetGame::Host(h) => h
                .fast
                .as_ref()
                .is_some_and(|f| h.peers.keys().any(|p| f.open(*p))),
            NetGame::Guest(g) => g.fast.as_ref().is_some_and(|f| f.open(0)),
        }
    }

    /// Lobby de l'hôte lui-même (ses amis le rejoindront dans son équipe).
    pub fn set_own_party(&mut self, party: u64) {
        if let NetGame::Host(h) = self {
            if let Some(p) = h.m.players.get_mut(&0) {
                p.party = party;
            }
        }
    }

    /// Un joueur arrive (hôte).
    pub fn peer_joined(&mut self, peer: u64, pseudo: String) {
        self.peer_joined_party(peer, pseudo, 0);
    }

    /// Un joueur arrive avec son lobby (hôte) : il ira dans l'équipe de ses amis.
    pub fn peer_joined_party(&mut self, peer: u64, pseudo: String, party: u64) {
        if let NetGame::Host(h) = self {
            let player = Player::new(h.spawn);
            h.peers.insert(
                peer,
                Remote {
                    party,
                    pseudo: pseudo.clone(),
                    player,
                    queue: VecDeque::new(),
                    ack: 0,
                    weapon: 255,
                    ready: false,
                    fall_from: None,
                    look: (0.0, 0.0),
                    budget: INPUT_BURST / 4,
                },
            );
        }
    }

    pub fn peer_left(&mut self, peer: u64) {
        if let NetGame::Host(h) = self {
            if let Some(r) = h.peers.remove(&peer) {
                h.record(peer, Move::Rule(Rule::Leave));
                h.burns.remove(&peer);
                let name = if h.m.started() {
                    r.pseudo.clone()
                } else {
                    h.m.shown(peer)
                };
                h.m.leave(peer, Some(r.player.pos));
                let line = format!("{name} a quitté la partie.");
                h.broadcast_chat(&line);
                h.out.push(Local::Chat(line));
            }
        }
    }

    /// Données reçues du relais. Renvoie ce que la session locale doit faire.
    pub fn receive(&mut self, from: Option<u64>, data: &[u8], world: &mut World) -> Vec<Local> {
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
                        let doors = &g.map_doors;
                        let prev = g
                            .backup
                            .as_deref()
                            .and_then(game::decode::<Backup>)
                            .map(|b| b.with_doors(doors));
                        let new = game::decode::<Backup>(&data).map(|b| b.with_doors(doors));
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
        if std::mem::take(&mut g.died) {
            g.pending.clear();
        }
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

    /// Phase de la partie (`game::phase`).
    pub fn phase(&self) -> u8 {
        match self {
            NetGame::Host(h) => h.m.phase,
            NetGame::Guest(g) => g.hud.phase,
        }
    }

    /// Partie commencée (après l'échauffement) et pas encore finie.
    pub fn in_progress(&self) -> bool {
        !matches!(self.phase(), game::phase::WARMUP | game::phase::GAME_OVER)
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
                    let n = to_net(input, local.yaw, local.pitch, local.carry);
                    h.look0 = (n.yaw, n.pitch);
                    let mv = Move::Own(n, local.net_state());
                    h.record(0, mv);
                }
                h.tick(world, local, dt)
            }
            NetGame::Guest(g) => {
                g.seq += 1;
                let n = to_net(input, local.yaw, local.pitch, local.carry);
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
                        blind: h.local_blind,
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
                    let _ = h.m.throw(0, nade);
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
                    let last = g.outbox_first + inputs.len() as u32 - 1;
                    let msg = ToHost::Inputs {
                        first_seq: g.outbox_first,
                        inputs,
                        weapon,
                    };
                    g.to_host(&msg);
                    g.sent.push_back((last, std::time::Instant::now()));
                    if g.sent.len() > 512 {
                        g.sent.pop_front();
                    }
                    // Voie rapide : les derniers pas partent aussi par les passeurs.
                    g.fast_inputs(weapon);
                }
                g.count_stall(dt);
                // (v165) Son ping, dit à l'hôte chaque seconde (tableau des scores).
                g.ping_timer -= dt;
                if g.ping_timer <= 0.0 {
                    g.ping_timer = 1.0;
                    if let Some(ms) = g.ping {
                        g.to_host(&ToHost::Ping(ms.round().clamp(1.0, 9999.0) as u16));
                    }
                }
                let view_tick = (g.clock - g.delay).max(0.0) as u32;
                for (w, origin, dirs) in shots {
                    g.to_host(&ToHost::Fire {
                        weapon: w,
                        origin: origin.to_array(),
                        dirs: dirs.iter().map(|d| d.to_array()).collect(),
                        view_tick,
                        blind: g.blind,
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
                        pos: h.shown.get(&id).copied().unwrap_or(r.player.pos),
                        yaw: r.player.yaw,
                        pitch: r.player.pitch,
                        crouch: r.player.crouch,
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
            NetGame::Host(h) => h.rows(),
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
            if amount > 0 {
                h.pains.push(0);
            }
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

    /// (v177) Revente d'un achat de cette manche (le code d'article de `game::ITEM_…`).
    pub fn sell(&mut self, item: u16) -> Vec<Local> {
        let mut out = Vec::new();
        match self {
            NetGame::Host(h) => {
                h.record(0, Move::Rule(Rule::Msg(ToHost::Sell(item))));
                match h.m.sell(0, item, Some(h.local_pos)) {
                    Ok(()) => {
                        if let Some(p) = h.m.players.get(&0) {
                            out.push(Local::Guns {
                                guns: p.guns,
                                nades: p.nades,
                            });
                        }
                    }
                    Err(e) => out.push(Local::Notice(e)),
                }
            }
            NetGame::Guest(g) => g.to_host(&ToHost::Sell(item)),
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
        // (v258) Les portes posées pour la partie (`Backup::with_doors`) : celles de la carte
        // de ce jeu.
        m.map.door_list = if map.door_list.is_empty() {
            g.map_doors.clone()
        } else {
            map.door_list.clone()
        };
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
                    party: 0,
                    pseudo: p.clone(),
                    player,
                    queue: VecDeque::new(),
                    ack: 0,
                    weapon: state.map_or(255, |q| q.3),
                    ready: false,
                    fall_from,
                    look: b
                        .looks
                        .iter()
                        .find(|l| l.0 == key)
                        .map_or((0.0, 0.0), |l| (l.1, l.2)),
                    budget: INPUT_BURST / 4,
                },
            );
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
            local_ladder: local.on_ladder,
            local_state: local.net_state(),
            burns: b.burns.iter().copied().collect(),
            local_keys: (false, false),
            backup_now: false,
            smokes: Vec::new(),
            local_blind: false,
            local_carry: 250,
            pings: HashMap::new(),
            strides: HashMap::new(),
            pains: Vec::new(),
            kill_flags: 0,
            auto_backup: true,
            out: Vec::new(),
            ground_dirty: true,
            resync: HashMap::new(),
            seen: Default::default(),
            backup: None,
            designated: None,
            playing: true,
            look0: (0.0, 0.0),
            log: Vec::new(),
            chain: b.chain,
            last_shot: HashMap::new(),
            shown: HashMap::new(),
            solo: false,
            bots: None,
            bot_seed: 0,
            away: None,
            build: false,
            fast: None,
            fast_groups: Vec::new(),
            fast_marks: HashMap::new(),
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
            g.own_tick = None;
            g.seen_tick = None;
            g.snap_tick = None;
            g.vitals = None;
            g.died = false;
            g.keys = (false, false);
            g.loadout = (255, 255);
            g.backup = None;
            g.audit.reset();
            // Un garant en attente n'est pas un joueur : il ne se présente pas ; (v220) un
            // invité pas encore chargé non plus (il le fera en fin de chargement).
            if !g.auditor && !g.loading {
                g.to_host(&ToHost::Hello {
                    map_hash: g.map_sig,
                });
            }
        }
    }

    /// Armes au sol.
    pub fn ground(&self) -> Vec<GroundGun> {
        match self {
            NetGame::Host(h) => h.m.ground.clone(),
            NetGame::Guest(g) => g.ground.clone(),
        }
    }

    /// Armes portées selon l'hôte (il fait foi) : principale, secondaire, grenades.
    pub fn my_guns(&self) -> Option<((u8, u8), [u8; 5])> {
        match self {
            NetGame::Host(h) => h.m.players.get(&0).map(|p| (p.guns, p.nades)),
            NetGame::Guest(_) => None,
        }
    }

    /// Arme, grenade ou bombe lâchée (touche G), qui retombe en `at`.
    pub fn drop_weapon(&mut self, weapon: u8, at: Vec3) {
        let msg = ToHost::Drop {
            weapon,
            at: at.to_array(),
        };
        match self {
            NetGame::Host(h) => {
                let from = h.local_pos;
                h.record(0, Move::Rule(Rule::Msg(msg)));
                h.ground_dirty |= h.m.toss(0, weapon, from, at);
            }
            NetGame::Guest(g) => g.to_host(&msg),
        }
    }

    /// Ramasser l'arme au sol `id`.
    pub fn pickup(&mut self, id: u32) -> Vec<Local> {
        match self {
            NetGame::Host(h) => {
                h.record(0, Move::Rule(Rule::Msg(ToHost::Pickup { id })));
                h.pickup(0, id)
            }
            NetGame::Guest(g) => {
                g.to_host(&ToHost::Pickup { id });
                Vec::new()
            }
        }
    }

    /// Ouvre ou ferme la porte / trappe visée en `pos` (touche Utiliser, si la carte le
    /// permet). `side` : côté vers lequel on regarde.
    pub fn use_door(&mut self, world: &World, pos: [i32; 3], side: u8) -> Vec<Local> {
        let msg = ToHost::Door { pos, side };
        match self {
            NetGame::Host(h) => {
                h.record(0, Move::Rule(Rule::Msg(msg)));
                let alive = h.m.players.get(&0).is_some_and(|p| p.alive);
                let feet = h.playing.then_some(h.local_pos);
                let cells = use_door(&mut h.m, world, feet, alive, pos, side);
                let mut out = Vec::new();
                h.doors_changed(cells, &mut out);
                out
            }
            NetGame::Guest(g) => {
                g.to_host(&msg);
                Vec::new()
            }
        }
    }

    /// Propose un vote à son équipe (`VOTE_KICK` : exclure `target`, `VOTE_PAUSE`).
    pub fn call_vote(&mut self, kind: u8, target: u64) -> Vec<Local> {
        let msg = ToHost::CallVote { kind, target };
        match self {
            NetGame::Host(h) => {
                h.record(0, Move::Rule(Rule::Msg(msg)));
                let mut out = Vec::new();
                match h.m.call_vote(0, kind, target) {
                    Ok(ev) => h.events(ev, &mut out),
                    Err(e) => out.push(Local::Notice(e)),
                }
                out
            }
            NetGame::Guest(g) => {
                g.to_host(&msg);
                Vec::new()
            }
        }
    }

    /// Réponse au vote de son équipe (F1 oui, F2 non).
    pub fn ballot(&mut self, yes: bool) -> Vec<Local> {
        match self {
            NetGame::Host(h) => {
                h.record(0, Move::Rule(Rule::Msg(ToHost::Ballot(yes))));
                let ev = h.m.ballot(0, yes);
                let mut out = Vec::new();
                h.events(ev, &mut out);
                out
            }
            NetGame::Guest(g) => {
                g.to_host(&ToHost::Ballot(yes));
                Vec::new()
            }
        }
    }

    /// Couleur préférée (bandeau des têtes, triangles des coéquipiers).
    pub fn choose_color(&mut self, color: u8) {
        match self {
            NetGame::Host(h) => {
                h.record(0, Move::Rule(Rule::Msg(ToHost::Color(color))));
                h.m.set_color(0, color);
            }
            NetGame::Guest(g) => g.to_host(&ToHost::Color(color)),
        }
    }

    pub fn chat(&mut self, text: &str) {
        match self {
            NetGame::Host(h) => {
                let line = format!("<{}> {text}", h.shown_of(0));
                h.broadcast_chat(&line);
            }
            NetGame::Guest(g) => g.to_host(&ToHost::Chat(text.to_string())),
        }
    }
}

impl Host {
    /// Voie rapide : demande au relais des passeurs pour les joueurs, groupés par équipe,
    /// une fois la partie commencée, et de nouveau dès que les équipes changent (joueur
    /// arrivé ou parti, nouvel hôte). Les groupes sont rangés de la même façon quel que
    /// soit le côté joué : changer de côté à la mi-temps ne redemande rien.
    fn fast_want(&mut self) {
        if self.solo || self.build {
            return;
        }
        let mut groups: Vec<Vec<u64>> = Vec::new();
        if self.m.started() {
            let mut by_team: BTreeMap<u8, Vec<u64>> = BTreeMap::new();
            for (&id, r) in &self.peers {
                if !r.ready {
                    continue;
                }
                if let Some(p) = self.m.players.get(&id) {
                    by_team.entry(p.team).or_default().push(id);
                }
            }
            groups = by_team.into_values().collect();
            for g in &mut groups {
                g.sort_unstable();
            }
            groups.sort();
        }
        if groups != self.fast_groups {
            self.fast_groups = groups.clone();
            send(&self.link, &Request::FastWant { groups });
        }
        self.fast_check();
    }

    /// (v274) Voie rapide : un passeur muet ou nettement plus lent que l'autre, pour un
    /// joueur, pendant trois secondes, est signalé au relais, qui en donne un autre. (Avec
    /// un seul passeur il n'y a rien à comparer : le relais le resonde de son côté, et les
    /// positions repassent par le relais s'il est trop lent.)
    fn fast_check(&mut self) {
        let Some(f) = &self.fast else { return };
        for &id in self.peers.keys() {
            let paths = f.paths(id);
            let best = paths
                .iter()
                .filter(|p| p.open)
                .filter_map(|p| p.rtt_ms)
                .fold(f32::INFINITY, f32::min);
            for (i, p) in paths.iter().enumerate() {
                let other = paths.iter().enumerate().any(|(j, q)| j != i && q.open);
                let bad =
                    other && (!p.open || p.rtt_ms.is_some_and(|ms| ms > best + FAST_BAD_MS));
                let n = self.fast_marks.entry((id, i as u8)).or_insert(0);
                if !bad {
                    *n = 0;
                    continue;
                }
                *n += 1;
                if *n >= FAST_BAD_CHECKS {
                    *n = 0;
                    send(
                        &self.link,
                        &Request::FastBad {
                            peer: id,
                            via: i as u8,
                        },
                    );
                }
            }
        }
    }

    /// (v172) Un bot rejoint la partie solo (prêt tout de suite, comme un invité qui a la
    /// bonne carte).
    fn add_bot(&mut self, id: u64, pseudo: String) {
        self.peers.insert(
            id,
            Remote {
                party: 0,
                pseudo: pseudo.clone(),
                player: Player::new(self.spawn),
                queue: VecDeque::new(),
                ack: 0,
                weapon: 255,
                ready: true,
                fall_from: None,
                look: (0.0, 0.0),
                budget: INPUT_BURST / 4,
            },
        );
        // (Sans les annonces « … rejoint les CT » : neuf lignes d'un coup en solo.)
        let ev: Vec<Ev> = self
            .m
            .join_party(id, &pseudo, 0)
            .into_iter()
            .filter(|e| !matches!(e, Ev::Say(_)))
            .collect();
        let mut out = Vec::new();
        self.events(ev, &mut out);
        self.out.extend(out);
    }

    /// (v172) La partie telle que les bots la voient à ce pas.
    fn scene(&self) -> crate::bots::Scene<'_> {
        use crate::bots::Body;
        let mut bodies = Vec::new();
        for (&id, p) in &self.m.players {
            if !p.present {
                continue;
            }
            let b = if id == 0 {
                if !self.playing {
                    continue;
                }
                let s = &self.local_state;
                Body {
                    id,
                    team: p.team,
                    alive: p.alive,
                    feet: Vec3::from(s.pos),
                    vel: Vec3::from(s.vel),
                    crouch: s.crouch,
                    on_ground: s.on_ground,
                    on_ladder: self.local_ladder,
                    yaw: 0.0,
                    carry: self.local_carry,
                }
            } else {
                let Some(r) = self.peers.get(&id).filter(|r| r.ready) else {
                    continue;
                };
                Body {
                    id,
                    team: p.team,
                    alive: p.alive,
                    feet: r.player.pos,
                    vel: r.player.vel,
                    crouch: r.player.crouch,
                    on_ground: r.player.on_ground,
                    on_ladder: r.player.on_ladder,
                    yaw: r.player.yaw,
                    carry: r.player.carry,
                }
            };
            bodies.push(b);
        }
        crate::bots::Scene {
            tick: self.tick,
            m: &self.m,
            bodies,
            smokes: &self.smokes,
            last_shot: &self.last_shot,
        }
    }

    /// (v172) Un pas des bots : leurs entrées vont dans leur file comme celles d'un invité,
    /// leurs tirs et leurs achats passent par les mêmes règles.
    fn drive_bots(&mut self, world: &World, dt: f32) {
        use crate::bots::Act;
        let Some(mut bots) = self.bots.take() else {
            return;
        };
        let acts = {
            let scene = self.scene();
            bots.think(&scene, world, dt)
        };
        let mut out = Vec::new();
        for a in acts {
            match a {
                Act::Input(id, n) => {
                    if let Some(r) = self.peers.get_mut(&id) {
                        let seq = r.ack + r.queue.len() as u32 + 1;
                        r.queue.push_back((seq, n));
                    }
                }
                Act::Fire {
                    bot,
                    weapon,
                    origin,
                    dirs,
                } => {
                    let msg = ToHost::Fire {
                        weapon,
                        origin: origin.to_array(),
                        dirs: dirs.iter().map(|d| d.to_array()).collect(),
                        view_tick: self.tick,
                        blind: false,
                    };
                    self.m.touch(bot);
                    out.extend(self.request(bot, msg, world));
                }
                Act::Keys { bot, plant, defuse } => self.m.keys(bot, plant, defuse),
                Act::Buy { bot, item } => {
                    let at = self.feet_of(bot);
                    let _ = self.m.buy(bot, item, at);
                }
                Act::Hold { bot, weapon } => {
                    if let Some(r) = self.peers.get_mut(&bot) {
                        r.weapon = weapon;
                    }
                }
                Act::Door { bot, pos, side } => {
                    let alive = self.m.players.get(&bot).is_some_and(|p| p.alive);
                    let feet = self.feet_of(bot);
                    let cells = use_door(&mut self.m, world, feet, alive, pos, side);
                    self.doors_changed(cells, &mut out);
                }
            }
        }
        self.out.extend(out);
        self.bots = Some(bots);
    }

    /// Tableau des scores, avec le ping de chacun (gardé par l'hôte, hors des règles).
    fn rows(&self) -> Vec<ScoreRow> {
        let mut rows = self.m.rows();
        for r in &mut rows {
            r.ping = self.pings.get(&r.id).copied().unwrap_or(0);
        }
        rows
    }

    fn rand(&mut self) -> u32 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 17;
        self.rng ^= self.rng << 5;
        self.rng
    }

    fn send_to(&self, peer: u64, msg: &ToPeer) {
        if self.solo {
            return;
        }
        send(
            &self.link,
            &Request::ToPeer {
                peer,
                data: game::encode(msg),
            },
        );
    }

    /// Bruits de pas : qui court (au-dessus de la vitesse de marche, au sol, vivant) fait
    /// un pas tous les `STRIDE` blocs, entendu par les joueurs à moins de `HEARD_STEPS`
    /// blocs (comme dans CS : marcher ou s'accroupir est silencieux pour les autres).
    fn footsteps(&mut self, world: &World, local_ground: bool, dt: f32, out: &mut Vec<Local>) {
        let mut walkers: Vec<(u64, Vec3, bool, u8)> = Vec::new();
        let mut on_ladder: Vec<u64> = Vec::new();
        if self.playing {
            walkers.push((0, self.local_pos, local_ground, self.local_carry));
            if self.local_ladder {
                on_ladder.push(0);
            }
        }
        walkers.extend(
            self.peers
                .iter()
                .filter(|(_, r)| r.ready)
                .map(|(&id, r)| (id, r.player.pos, r.player.on_ground, r.player.carry)),
        );
        on_ladder.extend(
            self.peers
                .iter()
                .filter(|(_, r)| r.ready && r.player.on_ladder)
                .map(|(&id, _)| id),
        );
        walkers.sort_by_key(|w| w.0);
        let alive = |m: &Match, id: u64| m.players.get(&id).is_some_and(|p| p.alive);
        let mut steps = Vec::new();
        for &(id, pos, ground, carry) in &walkers {
            let e = self.strides.entry(id).or_insert(Stride {
                last: pos,
                dist: 0.0,
                top: None,
                climb: 0.0,
            });
            let d = Vec3::new(pos.x - e.last.x, 0.0, pos.z - e.last.z).length();
            let teleport = (pos - e.last).length() > 3.0;
            let before = e.last;
            e.last = pos;
            let living = alive(&self.m, id);
            // Échelle : à pleine vitesse (pas accroupi), un bruit par bloc grimpé (v236).
            if on_ladder.contains(&id) {
                e.top = None;
                let dy = (pos.y - before.y).abs();
                if living && !teleport && dy / dt.max(1e-4) >= crate::player::LADDER_NOISY {
                    e.climb += dy;
                    if e.climb >= crate::player::LADDER_RUNG {
                        e.climb -= crate::player::LADDER_RUNG;
                        steps.push((id, pos, STEP_LADDER));
                    }
                } else {
                    e.climb = crate::player::LADDER_RUNG * 0.8;
                }
                continue;
            }
            // Plongeon : tombé dans l'eau d'au moins un bloc (tout le monde l'entend).
            if crate::player::in_water(world, pos) {
                if let Some(top) = e.top.take() {
                    if living && !teleport && top - pos.y >= SPLASH_HEIGHT {
                        steps.push((id, pos, STEP_SPLASH));
                    }
                }
            } else if ground {
                // Réception : tombé d'assez haut (pas une réapparition).
                if let Some(top) = e.top.take() {
                    if living && !teleport && top - pos.y >= LAND_HEIGHT {
                        steps.push((id, pos, STEP_LAND));
                    }
                }
            } else {
                // Plus haut point atteint en l'air (un saut normal monte à ~1,45 bloc).
                e.top = Some(e.top.map_or(before.y.max(pos.y), |t| t.max(pos.y)));
            }
            let running = d / dt.max(1e-4) > step_speed(carry) && d < 2.0;
            if ground && running && living {
                e.dist += d;
                if e.dist >= STRIDE {
                    e.dist -= STRIDE;
                    steps.push((id, pos, STEP_RUN));
                }
            } else {
                // Le premier pas d'une course vient vite.
                e.dist = STRIDE * 0.7;
            }
        }
        self.strides
            .retain(|id, _| walkers.iter().any(|w| w.0 == *id));
        // Cris de douleur (chute) : aussi pour le blessé lui-même.
        for who in std::mem::take(&mut self.pains) {
            if let Some(w) = walkers.iter().find(|w| w.0 == who) {
                steps.push((who, w.1, STEP_PAIN));
            }
        }
        for (who, pos, kind) in steps {
            let msg = ToPeer::Step {
                who,
                pos: pos.to_array(),
                kind,
            };
            for &(id, at, _, _) in &walkers {
                let me = id == who && kind == STEP_PAIN;
                if (id != who || me) && (at - pos).length() < HEARD_STEPS {
                    if id == 0 {
                        out.push(Local::Step(pos, kind));
                    } else {
                        self.send_to(id, &msg);
                    }
                }
            }
        }
    }

    fn broadcast(&self, msg: &ToPeer) {
        if self.solo {
            return;
        }
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

    /// Nom montré aux joueurs (caché pendant l'échauffement, voir `Match::shown`).
    fn shown_of(&self, id: u64) -> String {
        if self.m.started() || !self.m.players.contains_key(&id) {
            self.pseudo_of(id)
        } else {
            self.m.shown(id)
        }
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

    /// Portes changées : à tous (avec leur bruit), et au monde de l'hôte.
    fn doors_changed(&mut self, cells: Vec<([i32; 3], u16)>, out: &mut Vec<Local>) {
        let Some(&(c, _)) = cells.first() else {
            return;
        };
        let at = Vec3::new(c[0] as f32, c[1] as f32, c[2] as f32) + Vec3::splat(0.5);
        self.broadcast(&ToPeer::Doors {
            cells: cells.clone(),
            at: Some(at.to_array()),
        });
        out.push(Local::Doors {
            cells,
            at: Some(at),
        });
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
        let mut looks = Vec::new();
        if self.playing {
            players.push((0, self.pseudo.clone(), local.net_state(), 255));
            looks.push((0, self.look0.0, self.look0.1));
        }
        players.extend(
            self.peers
                .iter()
                .filter(|(_, r)| r.ready)
                .map(|(&id, r)| (id, r.pseudo.clone(), r.player.net_state(), r.weapon)),
        );
        looks.extend(
            self.peers
                .iter()
                .filter(|(_, r)| r.ready)
                .map(|(&id, r)| (id, r.look.0, r.look.1)),
        );
        Backup {
            m: self.m.clone(),
            host_pseudo: self.pseudo.clone(),
            players,
            looks,
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
        dbg_alive("hote", self.tick as u64, who, "(avant)", &self.m);
        note_activity(&mut self.m, who, &mv);
        // Partie solo : pas de vérificateur, rien à garder. (v243) Ni sur un serveur de
        // construction.
        if self.solo || self.build {
            return;
        }
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

    /// Ramasse une arme au sol (règles : vivant, à portée, échange avec l'ancienne).
    fn pickup(&mut self, id: u64, gun: u32) -> Vec<Local> {
        let mut out = Vec::new();
        let Some(feet) = self.feet_of(id) else {
            return out;
        };
        let Some(weapon) = self.m.pickup(id, gun, feet) else {
            return out;
        };
        self.ground_dirty = true;
        if id == 0 {
            out.push(Local::Give(weapon as u16));
        } else {
            self.send_to(
                id,
                &ToPeer::Give {
                    item: weapon as u16,
                },
            );
        }
        out
    }

    /// Rappelle à un joueur ce qu'il porte vraiment (au plus une fois par demi-seconde) :
    /// son jeu s'y remet s'il s'en est écarté (changement d'hôte, message perdu).
    fn send_guns(&mut self, id: u64) {
        if id == 0 {
            return;
        }
        let last = self.resync.get(&id).copied();
        if last.is_some_and(|t| self.tick.wrapping_sub(t) < 128) {
            return;
        }
        self.resync.insert(id, self.tick);
        if let Some(p) = self.m.players.get(&id) {
            self.send_to(
                id,
                &ToPeer::Guns {
                    primary: p.guns.0,
                    secondary: p.guns.1,
                    nades: p.nades,
                },
            );
        }
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
            let mut at = self.spawn_point(world, team, &taken);
            // (v209) Essais : départ fixé (MINELIB_FPS_START), même vue d'une partie à l'autre.
            let fixed = (id == 0).then(fixed_fps_start).flatten();
            if let Some((p, _)) = fixed {
                at = p;
            }
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
            let yaw = fixed.map(|(_, y)| y).unwrap_or_else(|| {
                target.map_or(0.0, |t| {
                    let d = t - at;
                    d.x.atan2(-d.z)
                })
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
                    // Le mort lâche sa meilleure arme et perd le reste.
                    let at = self.feet_of(victim);
                    self.m.dropped_dead(victim, at);
                    self.ground_dirty = true;
                    let k = killer.map(|k| self.shown_of(k)).unwrap_or_default();
                    let v = self.shown_of(victim);
                    let team = |id: u64| self.m.players.get(&id).map_or(255, |p| p.team);
                    let teams = [killer.map_or(255, team), team(victim)];
                    let flags = self.kill_flags;
                    self.broadcast(&ToPeer::Kill {
                        killer: k.clone(),
                        victim: v.clone(),
                        weapon,
                        head,
                        flags,
                        teams,
                    });
                    out.push(Local::Kill(Kill {
                        killer: k,
                        victim: v,
                        weapon,
                        head,
                        flags,
                        teams,
                    }));
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
                        rows: self.rows(),
                        history: self.m.history.clone(),
                    });
                    out.push(Local::GameOver(summary));
                }
                Ev::Started => out.push(Local::Started),
                Ev::NewRound => {
                    self.ground_dirty = true;
                }
                Ev::VoteKick(id) => {
                    let reason = "Exclu de la partie par un vote de ton équipe.".to_string();
                    if id == 0 {
                        out.push(Local::Kicked(reason));
                    } else {
                        self.send_to(id, &ToPeer::Kicked { reason });
                    }
                }
                Ev::Paused(on) => out.push(Local::Paused(on)),
                Ev::AfkKick(id) => {
                    // Il quitte la partie de lui-même à la réception (le relais compte
                    // alors son absence comme une déconnexion) et peut la rejoindre.
                    let reason = "Sorti de la partie pour inactivité (3 minutes en vie sans \
                                  jouer, comme dans Counter-Strike). Tu peux la rejoindre."
                        .to_string();
                    if id == 0 {
                        out.push(Local::Kicked(reason));
                    } else {
                        self.send_to(id, &ToPeer::Kicked { reason });
                    }
                }
                Ev::TeamKillKick(id) => {
                    let reason = format!(
                        "Exclu de la partie : {} coéquipiers tués.",
                        crate::cs::TEAMKILL_KICK
                    );
                    if id == 0 {
                        out.push(Local::Kicked(reason));
                    } else {
                        self.send_to(id, &ToPeer::Kicked { reason });
                    }
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
            blind,
        } = msg
        else {
            return Vec::new();
        };
        let origin = Vec3::from(origin);
        let dirs: Vec<Vec3> = dirs
            .iter()
            .map(|d| Vec3::from(*d).normalize_or_zero())
            .collect();
        let local = self.playing.then(|| {
            let mut p = player_from(&self.local_state);
            p.carry = self.local_carry;
            p
        });
        let shooter = if who == 0 {
            local.as_ref()
        } else {
            self.peers.get(&who).filter(|r| r.ready).map(|r| &r.player)
        };
        let ok = fire_ok(&mut self.m, world, who, shooter, weapon, origin, dirs.len());
        let present = shooter.is_some();
        if who == 0 {
            self.record(0, Move::Rule(Rule::Msg(msg)));
        }
        if ok {
            let view = view_tick.clamp(self.tick.saturating_sub(MAX_REWIND), self.tick);
            self.fire(who, weapon, origin, &dirs, view, blind, world)
        } else {
            // Refusé (arme qu'il ne porte pas ?) : on lui rappelle ce qu'il porte.
            if !self.m.has_gun(who, weapon) && weapon < 254 {
                self.send_guns(who);
            }
            // Coup de couteau dans le vide pendant le temps d'achat : sans effet, mais les
            // autres l'entendent quand même.
            let alive = self.m.players.get(&who).is_some_and(|p| p.alive);
            if weapon >= KNIFE_HEAVY && alive && self.m.frozen() && present {
                self.broadcast(&ToPeer::Shot {
                    who,
                    weapon,
                    pos: origin.to_array(),
                    hits: Vec::new(),
                    dirs: Vec::new(),
                });
                // (v209) L'hôte l'entend aussi (contre des bots, c'est le seul joueur) : les
                // mêmes bruits qu'en ligne.
                if self.playing {
                    return vec![Local::Shot {
                        weapon,
                        pos: origin,
                        heard: who != 0,
                        hits: Vec::new(),
                        dirs: Vec::new(),
                    }];
                }
            }
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
                let party = self.peers.get(&peer).map_or(0, |r| r.party);
                self.record(peer, Move::Rule(Rule::Hello { pseudo, ok, party }));
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
                    self.resync.remove(&peer);
                    self.send_guns(peer);
                    self.send_to(
                        peer,
                        &ToPeer::Ground {
                            guns: self.m.ground.clone(),
                        },
                    );
                    // Portes déjà ouvertes ou fermées pendant la partie.
                    if !self.m.doors.is_empty() {
                        self.send_to(
                            peer,
                            &ToPeer::Doors {
                                cells: self.m.doors.iter().map(|(c, e)| (*c, e[0])).collect(),
                                at: None,
                            },
                        );
                    }
                    let pseudo = self.pseudo_of(peer);
                    // Déjà dans la partie (reprise par un nouvel hôte) : il continue.
                    let back = self.m.players.get(&peer).is_some_and(|p| p.present);
                    if !back {
                        let party = self.peers.get(&peer).map_or(0, |r| r.party);
                        let ev = self.m.join_party(peer, &pseudo, party);
                        self.events(ev, &mut out);
                    }
                    if !self.m.started() {
                        // Combien en attendent-on encore.
                        let n = self.m.present();
                        let line = format!("{n} / {} joueurs.", self.m.team_max() * 2);
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
                        // (Le dernier pas qu'on a : la voie rapide a pu en apporter.)
                        let top = r.queue.back().map_or(r.ack, |q| q.0);
                        if seq > top && r.queue.len() < 512 {
                            r.queue.push_back((seq, n));
                        }
                    }
                }
            }
            // Par la voie rapide : seulement le pas qui suit exactement le dernier qu'on a.
            // Jamais de trou (ce qui manque arrive par le relais, dans l'ordre), jamais de
            // doublon : l'hôte joue exactement les mêmes entrées qu'avant, plus tôt.
            ToHost::InputsFast {
                first_seq,
                inputs,
                weapon,
            } => {
                if let Some(r) = self.peers.get_mut(&peer) {
                    for (i, n) in inputs.into_iter().enumerate() {
                        let seq = first_seq.wrapping_add(i as u32);
                        let top = r.queue.back().map_or(r.ack, |q| q.0);
                        if seq == top.wrapping_add(1) && r.queue.len() < 512 {
                            r.queue.push_back((seq, n));
                            r.weapon = weapon;
                        }
                    }
                }
            }
            msg @ ToHost::Fire { .. } => out.extend(self.request(peer, msg, world)),
            ToHost::Chat(text) => {
                if self.peers.contains_key(&peer) {
                    let text: String = text.chars().take(200).collect();
                    let line = format!("<{}> {}", self.shown_of(peer), text);
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
            // (v177) Revente : ce qu'il porte vraiment lui est redit (l'arme d'avant revient).
            ToHost::Sell(item) => {
                let at = self.feet_of(peer);
                match self.m.sell(peer, item, at) {
                    Ok(()) => {
                        if let Some(p) = self.m.players.get(&peer) {
                            let msg = ToPeer::Guns {
                                primary: p.guns.0,
                                secondary: p.guns.1,
                                nades: p.nades,
                            };
                            self.send_to(peer, &msg);
                        }
                    }
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
            // Ce que le joueur dit porter ne compte pas : les règles le savent.
            ToHost::Loadout { .. } => {}
            ToHost::Drop { weapon, at } => {
                if let Some(from) = self.feet_of(peer) {
                    self.ground_dirty |= self.m.toss(peer, weapon, from, Vec3::from(at));
                }
            }
            ToHost::Pickup { id } => out.extend(self.pickup(peer, id)),
            ToHost::Color(c) => self.m.set_color(peer, c),
            ToHost::Door { pos, side } => {
                let alive = self.m.players.get(&peer).is_some_and(|p| p.alive);
                let feet = self.feet_of(peer);
                let cells = use_door(&mut self.m, world, feet, alive, pos, side);
                self.doors_changed(cells, &mut out);
            }
            ToHost::CallVote { kind, target } => match self.m.call_vote(peer, kind, target) {
                Ok(ev) => self.events(ev, &mut out),
                Err(e) => self.send_to(
                    peer,
                    &ToPeer::Chat {
                        from: String::new(),
                        text: e,
                    },
                ),
            },
            ToHost::Ballot(yes) => {
                let ev = self.m.ballot(peer, yes);
                self.events(ev, &mut out);
            }
            // (v165) Son ping, montré dans le tableau des scores.
            // (Hors des règles : rien que le vérificateur ait à rejouer.)
            ToHost::Ping(ms) => {
                self.pings.insert(peer, ms);
            }
            ToHost::Throw { nade, pos, vel } => {
                let alive = self.m.players.get(&peer).is_some_and(|p| p.alive);
                let near = self
                    .peers
                    .get(&peer)
                    .is_some_and(|r| (r.player.eye() - Vec3::from(pos)).length() < 3.0);
                let ok =
                    alive && near && Vec3::from(vel).length() < 40.0 && self.m.throw(peer, nade);
                if !ok {
                    self.send_guns(peer);
                }
                if ok {
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

    /// Pieds des joueurs vivants et en jeu (obstacles les uns pour les autres).
    fn solid_feet(&self) -> BTreeMap<u64, Vec3> {
        let alive = |id: u64| self.m.players.get(&id).is_some_and(|p| p.alive);
        let mut feet = BTreeMap::new();
        if self.playing && alive(0) {
            feet.insert(0, self.local_pos);
        }
        for (&id, r) in &self.peers {
            if r.ready && alive(id) {
                feet.insert(id, r.player.pos);
            }
        }
        feet
    }

    fn tick(&mut self, world: &World, local: &Player, dt: f32) {
        self.tick += 1;
        // Positions montrées des autres joueurs (affichage seulement).
        let peers = &self.peers;
        self.shown.retain(|id, _| peers.contains_key(id));
        for (&id, r) in self.peers.iter().filter(|(_, r)| r.ready) {
            let target = r.player.pos;
            let p = self.shown.entry(id).or_insert(target);
            if (target - *p).length() > 3.0 {
                *p = target;
            } else {
                *p += (target - *p) * 0.3;
            }
        }
        self.local_pos = local.pos;
        self.local_ladder = local.on_ladder;
        self.local_state = local.net_state();
        self.local_carry = local.carry;
        // (v172) Les bots jouent leur pas (leurs entrées passent ensuite comme celles des
        // invités).
        self.drive_bots(world, dt);
        let frozen = self.m.frozen();
        let mut falls: Vec<(u64, u32)> = Vec::new();
        let mut moves: Vec<(u64, Move)> = Vec::new();
        // Les joueurs sont solides entre eux : chacun avance contre les autres là où ils
        // sont, dans l'ordre des numéros (le vérificateur rejoue dans le même ordre).
        let mut feet = self.solid_feet();
        let mut ids: Vec<u64> = self.peers.keys().copied().collect();
        ids.sort();
        for id in ids {
            let r = self.peers.get_mut(&id).expect("joueur");
            if !r.ready {
                continue;
            }
            r.budget = (r.budget + 1).min(INPUT_BURST);
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
            // Trop de retard pour être rattrapé (longue coupure, ou jeu accéléré) : les
            // plus vieilles entrées sont perdues, le joueur repart de là où il est ici.
            while r.queue.len() > r.budget as usize + 8 {
                if let Some((seq, _)) = r.queue.pop_front() {
                    r.ack = seq;
                }
            }
            let n = if r.queue.len() > CATCH_UP {
                CATCH_UP
            } else {
                1
            };
            for _ in 0..n.min(r.budget as usize) {
                if let Some((seq, mut input)) = r.queue.pop_front() {
                    r.budget -= 1;
                    if still_now {
                        input.forward = 0;
                        input.strafe = 0;
                        input.jump = false;
                        input.jump_pressed = false;
                    }
                    r.player.others = others_except(&feet, id);
                    step(&mut r.player, world, &input, dt);
                    r.look = (input.yaw, input.pitch);
                    feet.insert(id, r.player.pos);
                    r.ack = seq;
                    moves.push((id, Move::Input(seq, input)));
                }
            }
            // Chute (comme pour le joueur local) et vide.
            if let Some(d) = fall_check(&r.player, world, &mut r.fall_from) {
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
            if d > 0 {
                self.pains.push(id);
            }
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
        self.footsteps(world, local.on_ground, dt, &mut out);
        self.out.extend(out);
        if !self.spawns.is_empty() {
            self.do_spawns(world);
        }
        // Mémoire des positions (rembobinage des tirs).
        let mut frame: Frame = Vec::new();
        if self.playing {
            frame.push((0, local.pos, local.crouch, self.look0.0, self.look0.1));
        }
        frame.extend(
            self.peers
                .iter()
                .filter(|(_, r)| r.ready)
                .map(|(&id, r)| (id, r.player.pos, r.player.crouch, r.look.0, r.look.1)),
        );
        frame.sort_by_key(|f| f.0);
        self.record(0, Move::Rule(Rule::Frame(self.tick)));
        self.history.push_back((self.tick, frame));
        if self.history.len() > HISTORY {
            self.history.pop_front();
        }
        if self.tick.is_multiple_of(MOVES_EVERY) && !self.solo {
            self.snapshots(local, world, self.tick.is_multiple_of(SNAPSHOT_EVERY));
        }
        let due = self.tick.is_multiple_of(BACKUP_EVERY) || std::mem::take(&mut self.backup_now);
        if due && self.auto_backup {
            self.send_backup(local);
        }
        if self.ground_dirty {
            self.ground_dirty = false;
            self.broadcast(&ToPeer::Ground {
                guns: self.m.ground.clone(),
            });
        }
        if self.tick.is_multiple_of(SCOREBOARD_EVERY) {
            self.fast_want();
            self.broadcast(&ToPeer::Scoreboard {
                rows: self.rows(),
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
    /// `viewer` (yeux `eye`, vitesse `vel`) peut-il voir `target` ? Large exprès : on
    /// regarde aussi là où chacun sera dans un instant (le joueur qui sort de derrière un
    /// mur doit apparaître avant d'être visible, pas après), et les épaules, pas que le
    /// centre. Seul celui qui est vraiment caché n'est pas envoyé.
    fn sees(&self, world: &World, eye: Vec3, vel: Vec3, target: u64, feet: Vec3) -> bool {
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
        let tvel = self.vel_of(target);
        let ahead = 0.2; // s : retard du réseau et du lissage, avec de la marge
        let eyes = [eye, eye + Vec3::new(vel.x, 0.0, vel.z) * ahead];
        let to = (feet - eye).normalize_or_zero();
        let side = Vec3::new(-to.z, 0.0, to.x) * 0.4;
        let mut points = Vec::with_capacity(10);
        for f in [feet, feet + Vec3::new(tvel.x, 0.0, tvel.z) * ahead] {
            // (v225) Le milieu de la tête du modèle.
            points.push(f + Vec3::Y * (HEIGHT - crate::player_model::HEAD_BLOCKS / 2.0));
            points.push(f + Vec3::Y * 1.1 + side);
            points.push(f + Vec3::Y * 1.1 - side);
            points.push(f + Vec3::Y * 0.3);
        }
        eyes.iter().any(|&e| {
            points.iter().any(|&p| {
                let d = p - e;
                let len = d.length();
                raycast(world, e.into(), (d / len.max(1e-3)).into(), len)
                    .is_none_or(|hit| hit.distance >= len - 0.6)
            })
        })
    }

    /// Vitesse d'un joueur (0 : l'hôte).
    fn vel_of(&self, id: u64) -> Vec3 {
        if id == 0 {
            self.local_state.vel.into()
        } else {
            self.peers.get(&id).map_or(Vec3::ZERO, |r| r.player.vel)
        }
    }

    fn snapshots(&self, local: &Player, world: &World, full: bool) {
        let me = self
            .playing
            .then(|| self.net_player(0, &self.shown_of(0), local, 255));
        let all: Vec<(u64, NetPlayer)> = self
            .peers
            .iter()
            .filter(|(_, r)| r.ready)
            .map(|(&id, r)| {
                (
                    id,
                    self.net_player(id, &self.shown_of(id), &r.player, r.weapon),
                )
            })
            .collect();
        let team = |id: u64| self.m.players.get(&id).map(|p| p.team);
        for (&id, r) in &self.peers {
            if !r.ready {
                continue;
            }
            let mp = self.m.players.get(&id);
            let eye = r.player.eye();
            let my_team = team(id);
            // Qui voit qui : recalculé avec chaque état complet, gardé entre deux.
            let visible = |o: u64, p: &NetPlayer| {
                if team(o) == my_team || !p.alive {
                    return true;
                }
                // Vu : gardé un quart de seconde avant de redevenir caché (pas de
                // clignotement au bord d'un mur).
                let mut seen = self.seen.borrow_mut();
                let last = seen.get(&(id, o)).copied();
                if full {
                    let recent = last.is_some_and(|t| self.tick.wrapping_sub(t) < 64);
                    if recent || self.sees(world, eye, r.player.vel, o, Vec3::from(p.pos)) {
                        if !recent {
                            seen.insert((id, o), self.tick);
                        }
                        return true;
                    }
                    false
                } else {
                    last.is_some_and(|t| self.tick.wrapping_sub(t) < 64)
                }
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
            if !full {
                let moves = ToPeer::Moves {
                    tick: self.tick,
                    ack: r.ack,
                    you: r.player.net_state(),
                    players: players.iter().map(game::Pose::of).collect(),
                };
                // Par un passeur si la voie rapide vers ce joueur est ouverte (un paquet
                // perdu est remplacé par le suivant, 4 ms après), sinon par le relais.
                let fast = self
                    .fast
                    .as_ref()
                    .is_some_and(|f| f.send_one(id, &game::encode(&moves)));
                if !fast {
                    self.send_to(id, &moves);
                    // Ce joueur reçoit plus vite par le relais : une sonde de temps en
                    // temps par les passeurs, pour qu'il dise quand ce n'est plus vrai.
                    if self.tick % FAST_PING_EVERY == 1 {
                        if let Some(f) = self.fast.as_ref().filter(|f| f.slow(id)) {
                            f.send_any(id, &game::encode(&ToPeer::FastPing { tick: self.tick }));
                        }
                    }
                }
                continue;
            }
            let state = ToPeer::Snapshot {
                tick: self.tick,
                ack: r.ack,
                you: r.player.net_state(),
                hp: mp.map_or(0, |p| p.hp.min(255) as u8),
                alive: mp.is_some_and(|p| p.alive),
                players,
                hud: self.m.hud(id, Some(r.player.pos)),
            };
            self.send_to(id, &state);
            // (v274) Aussi par un passeur quand la voie rapide est ouverte : le premier
            // arrivé compte (un canal du relais qui cale ne fige plus la vie, l'arme et le
            // tableau de bord). Trop long pour un paquet : il ne part que par le relais.
            if let Some(f) = &self.fast {
                f.send_one(id, &game::encode(&state));
            }
        }
    }

    /// Tir de `shooter` (0 = l'hôte) : cibles rembobinées au pas `view_tick`.
    #[allow(clippy::too_many_arguments)]
    fn fire(
        &mut self,
        shooter: u64,
        weapon: u8,
        origin: Vec3,
        dirs: &[Vec3],
        view_tick: u32,
        blind: bool,
        world: &World,
    ) -> Vec<Local> {
        let mut out = Vec::new();
        self.last_shot.insert(shooter, self.tick);
        let frame = pick_frame(&self.history, view_tick);
        let damage = shot(&self.m, &frame, shooter, weapon, origin, dirs, world);
        // Tout le monde entend le tir et voit le sang des touches.
        let hits: Vec<Vec3> = damage.values().flatten().map(|t| t.point).collect();
        self.broadcast(&ToPeer::Shot {
            who: shooter,
            weapon,
            pos: origin.to_array(),
            hits: hits.iter().map(|p| p.to_array()).collect(),
            dirs: dirs.iter().take(16).map(|d| d.to_array()).collect(),
        });
        out.push(Local::Shot {
            weapon,
            pos: origin,
            heard: shooter != 0,
            hits,
            dirs: dirs.to_vec(),
        });
        for (victim, hits) in damage {
            let head = hits
                .iter()
                .any(|t| matches!(t.hurt, Hurt::Bullet { head: true, .. }));
            // Marqueur de touche au tireur.
            if shooter == 0 {
                out.push(Local::Hit { head });
            } else {
                let total: u32 = hits.iter().map(|h| h.damage).sum();
                self.send_to(
                    shooter,
                    &ToPeer::Hit {
                        damage: total.min(255) as u8,
                        head,
                    },
                );
            }
            for t in hits {
                // Pour le fil des éliminations : à travers un mur, une fumée, aveuglé.
                let smoke = self
                    .smokes
                    .iter()
                    .any(|&(c, r)| segment_near(origin, t.point, c, r));
                self.kill_flags = if t.wall { game::KILL_WALL } else { 0 }
                    | if smoke { game::KILL_SMOKE } else { 0 }
                    | if blind { game::KILL_BLIND } else { 0 };
                let ev = self.m.hurt(Some(shooter), victim, t.damage, t.hurt);
                self.events(ev, &mut out);
                self.kill_flags = 0;
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
    /// Voie rapide : les derniers pas d'entrées (jusqu'à `FAST_INPUTS`, tous déjà partis par
    /// le relais) par chaque passeur. L'hôte n'en garde que ce qui lui manque.
    fn fast_inputs(&self, weapon: u8) {
        let Some(f) = &self.fast else { return };
        let k = FAST_INPUTS.min(self.pending.len());
        if k == 0 {
            return;
        }
        let from = self.pending.len() - k;
        let msg = ToHost::InputsFast {
            first_seq: self.pending[from].0,
            inputs: self.pending.iter().skip(from).map(|p| p.1).collect(),
            weapon,
        };
        f.send_all(0, &game::encode(&msg));
    }

    /// Une image de plus ; bloquée si les nouvelles de l'hôte ont trop de retard. Dit au
    /// journal toutes les 30 s : c'est ce qui permet de comparer avec et sans voie rapide.
    fn count_stall(&mut self, dt: f32) {
        let Some(seen) = self.seen_tick else { return };
        self.net_frames += 1;
        // (L'horloge vient d'avancer d'une image : elle ne compte pas dans le retard.)
        if self.clock - seen as f32 > STALL_TICKS + dt / crate::session::SIM_TICK {
            self.net_stalled += 1;
        }
        if self.net_since.elapsed() >= std::time::Duration::from_secs(30) {
            log::info!(
                "réseau : {} images sur {} avec plus de {:.0} ms de retard sur l'hôte (voie \
                 rapide {}, ping {:.0} ms)",
                self.net_stalled,
                self.net_frames,
                STALL_TICKS * crate::session::SIM_TICK * 1000.0,
                if self.fast.as_ref().is_some_and(|f| f.open(0)) {
                    "ouverte"
                } else {
                    "fermée"
                },
                self.ping.unwrap_or(0.0)
            );
            self.net_frames = 0;
            self.net_stalled = 0;
            self.net_since = std::time::Instant::now();
        }
    }

    /// Images comptées depuis le dernier relevé, et celles qui étaient bloquées.
    #[cfg(test)]
    pub fn stalls(&self) -> (u32, u32) {
        (self.net_frames, self.net_stalled)
    }

    /// Voie rapide : un message au pas `tick` vient d'arriver par les passeurs (positions,
    /// ou sonde `ping` quand on a demandé le relais). S'il arrive alors qu'on a déjà reçu
    /// ce pas ou un plus récent, le relais a été plus rapide. Tous les `FAST_VOTES`
    /// messages, l'avis : trois quarts en retard, on demande les positions par le relais ;
    /// une fois au relais, il faut que trois quarts des sondes soient en avance pour revenir
    /// aux passeurs (pas d'aller-retour pour quelques millisecondes).
    fn fast_vote(&mut self, tick: u32, ping: bool) {
        // (En temps normal ce sont les positions qui votent ; au relais, les sondes.)
        if self.fast.is_none() || ping != self.fast_slow {
            return;
        }
        self.fast_votes += 1;
        if self.seen_tick.is_some_and(|s| tick <= s) {
            self.fast_late += 1;
        }
        if self.fast_votes < FAST_VOTES {
            return;
        }
        let slow = if self.fast_slow {
            self.fast_late > FAST_VOTES / 4
        } else {
            self.fast_late >= FAST_VOTES / 4 * 3
        };
        if slow != self.fast_slow {
            self.fast_slow = slow;
            if let Some(f) = &self.fast {
                f.set_slow(0, slow);
            }
            log::info!(
                "voie rapide : les positions arrivent plus vite par {}",
                if slow { "le relais" } else { "les passeurs" }
            );
        }
        self.fast_votes = 0;
        self.fast_late = 0;
    }

    /// Un état de l'hôte au pas `tick` vient d'arriver : est-il plus récent que tout ce
    /// qu'on a reçu ? (Sinon il a été doublé par un autre, par la voie rapide : il ne compte
    /// pas pour l'horloge.)
    fn newest(&mut self, tick: u32) -> bool {
        let new = self.seen_tick.is_none_or(|s| tick > s);
        if new {
            self.seen_tick = Some(tick);
            self.arrived(tick as f32);
        }
        new
    }

    /// Un état de soi au pas `tick` est-il plus récent que le dernier pris en compte ? (Un
    /// plus ancien ramènerait la prédiction en arrière : ses entrées ne sont plus là.)
    fn own_fresh(&self, tick: u32) -> bool {
        self.own_tick.is_none_or(|s| tick > s)
    }

    /// (v165) L'hôte a pris en compte les entrées jusqu'au numéro `ack` : le temps depuis
    /// leur envoi donne le ping (lissé).
    fn acked(&mut self, ack: u32) {
        let mut rtt = None;
        while let Some(&(seq, t)) = self.sent.front() {
            if seq > ack {
                break;
            }
            self.sent.pop_front();
            rtt = Some(t.elapsed().as_secs_f32() * 1000.0);
        }
        if let Some(ms) = rtt {
            self.ping = Some(match self.ping {
                Some(p) => p * 0.8 + ms * 0.2,
                None => ms,
            });
        }
    }

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
            ToPeer::Guns {
                primary,
                secondary,
                nades,
            } => out.push(Local::Guns {
                guns: (primary, secondary),
                nades,
            }),
            ToPeer::Welcome { you, tick, map_ok } => {
                self.hello_timer = f32::INFINITY;
                self.you = Some(you);
                self.clock = tick as f32;
                self.seen_tick = None;
                self.own_tick = None;
                self.snap_tick = None;
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
                // (v274) Déjà reçu par l'autre chemin (relais ou passeurs), ou dépassé par
                // un état complet plus récent : rien à en tirer.
                if self.snap_tick.is_some_and(|s| tick <= s) {
                    return out;
                }
                self.snap_tick = Some(tick);
                // Horloge : rattrape l'hôte en douceur.
                self.newest(tick);
                // (Pseudo, vie, arme des autres : toujours ceux du dernier état complet,
                // même si des positions plus récentes sont déjà là par la voie rapide.)
                self.full = players.clone();
                if self.snaps.back().is_none_or(|(s, _)| *s < tick) {
                    self.snaps.push_back((tick, players));
                    while self.snaps.len() > 128 {
                        self.snaps.pop_front();
                    }
                }
                self.hud = hud;
                let prev_hp = self.latest.map(|l| l.2);
                // Sa vie vient toujours d'ici ; son déplacement, seulement s'il est plus
                // récent que celui qu'on a déjà.
                self.vitals = Some((hp, alive));
                match (self.own_fresh(tick), self.latest) {
                    (false, Some(l)) => self.latest = Some((l.0, l.1, hp, alive)),
                    // Dépassé par des positions venues par la voie rapide, et déjà prises
                    // en compte : il ne reste à en retenir que sa vie.
                    (false, None) if self.fast.is_some() => self.died |= !alive,
                    _ => {
                        self.own_tick = Some(tick);
                        self.latest = Some((ack, you, hp, alive));
                    }
                }
                self.acked(ack);
                if prev_hp != Some(hp) {
                    out.push(Local::SetHp(hp as u32));
                }
            }
            ToPeer::Moves {
                tick,
                ack,
                you,
                players,
            } => {
                self.fast_vote(tick, false);
                self.newest(tick);
                if self.snaps.back().is_none_or(|(s, _)| *s < tick) {
                    // Le reste (pseudo, vie, arme…) vient du dernier état complet.
                    let list: Vec<NetPlayer> = players
                        .iter()
                        .filter_map(|pose| {
                            let mut p = self.full.iter().find(|p| p.id == pose.id)?.clone();
                            pose.apply(&mut p);
                            Some(p)
                        })
                        .collect();
                    self.snaps.push_back((tick, list));
                    while self.snaps.len() > 128 {
                        self.snaps.pop_front();
                    }
                }
                if self.own_fresh(tick) {
                    let vitals = match (self.latest, self.vitals) {
                        (Some(l), _) => Some((l.2, l.3)),
                        // Voie rapide : ces positions peuvent arriver avant l'état complet
                        // de leur image. En vie, on se corrige quand même, avec la vie du
                        // dernier état complet.
                        (None, Some((hp, true))) if self.fast.is_some() => Some((hp, true)),
                        _ => None,
                    };
                    if let Some((hp, alive)) = vitals {
                        self.own_tick = Some(tick);
                        self.latest = Some((ack, you, hp, alive));
                    }
                }
                self.acked(ack);
            }
            ToPeer::FastPing { tick } => self.fast_vote(tick, true),
            ToPeer::Hit { head, .. } => out.push(Local::Hit { head }),
            ToPeer::Kill {
                killer,
                victim,
                weapon,
                head,
                flags,
                teams,
            } => out.push(Local::Kill(Kill {
                killer,
                victim,
                weapon,
                head,
                flags,
                teams,
            })),
            ToPeer::Kicked { reason } => out.push(Local::Kicked(reason)),
            ToPeer::Shot {
                who,
                weapon,
                pos,
                hits,
                dirs,
            } => out.push(Local::Shot {
                weapon,
                pos: Vec3::from(pos),
                heard: self.you != Some(who),
                hits: hits.into_iter().map(Vec3::from).collect(),
                dirs: dirs.into_iter().take(16).map(Vec3::from).collect(),
            }),
            ToPeer::Step { who, pos, kind } => {
                if self.you != Some(who) || kind == STEP_PAIN {
                    out.push(Local::Step(Vec3::from(pos), kind));
                }
            }
            ToPeer::Respawn { state, reset, team } => {
                self.pending.clear();
                self.outbox.clear();
                self.hud.team = team;
                out.push(Local::Respawn { state, reset, team });
            }
            ToPeer::Doors { cells, at } => out.push(Local::Doors {
                cells,
                at: at.map(Vec3::from),
            }),
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
    /// Un état de l'hôte du pas `t` arrive : horloge et retard d'affichage.
    fn arrived(&mut self, t: f32) {
        if (self.clock - t).abs() > 64.0 {
            self.clock = t;
            self.late_peak = 0.0;
        } else {
            // Retard de cet état sur l'horloge (les à-coups du réseau).
            let late = (self.clock - t).max(0.0);
            self.late_peak = late.max(self.late_peak * 0.997);
            self.clock += (t - self.clock) * 0.0125;
        }
        let want = (self.late_peak + 4.0).clamp(INTERP_TICKS, MAX_DELAY);
        // Monte vite (plus de sautillement), redescend doucement (pas de ralenti visible).
        let rate = if want > self.delay { 0.2 } else { 0.01 };
        self.delay += (want - self.delay) * rate;
    }

    fn views(&self) -> Vec<View> {
        let t = self.clock - self.delay;
        let (a, b, k) = match self.snaps.iter().position(|(s, _)| *s as f32 > t) {
            Some(i) if i > 0 => {
                let (a, b) = (&self.snaps[i - 1], &self.snaps[i]);
                let k = ((t - a.0 as f32) / (b.0 - a.0).max(1) as f32).clamp(0.0, 1.0);
                (Some(a), Some(b), k)
            }
            Some(_) => (self.snaps.front(), self.snaps.front(), 1.0),
            // Plus rien de récent : on prolonge un peu le dernier mouvement (au plus 6 pas)
            // plutôt que de figer les joueurs puis de les faire sauter.
            None => {
                let n = self.snaps.len();
                match (n >= 2).then(|| (&self.snaps[n - 2], &self.snaps[n - 1])) {
                    Some((a, b)) => {
                        let span = (b.0 - a.0).max(1) as f32;
                        let k = 1.0 + ((t - b.0 as f32) / span).clamp(0.0, 6.0 / span);
                        (Some(a), Some(b), k)
                    }
                    None => (self.snaps.back(), self.snaps.back(), 1.0),
                }
            }
        };
        let (Some((_, pa)), Some((_, pb))) = (a, b) else {
            return Vec::new();
        };
        pb.iter()
            .map(|p| {
                let prev = pa.iter().find(|q| q.id == p.id).unwrap_or(p);
                let lerp_angle = |a: f32, b: f32| {
                    let d = (b - a + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU)
                        - std::f32::consts::PI;
                    a + d * k.min(1.0)
                };
                // Au-delà du dernier état : la position seulement (pas de saut au retour
                // d'un joueur, ni d'angle exagéré).
                let kp = if (Vec3::from(p.pos) - Vec3::from(prev.pos)).length() > 2.0 {
                    k.min(1.0)
                } else {
                    k
                };
                let k = k.min(1.0);
                View {
                    pseudo: p.pseudo.clone(),
                    pos: Vec3::from(prev.pos).lerp(Vec3::from(p.pos), kp),
                    yaw: lerp_angle(prev.yaw, p.yaw),
                    pitch: prev.pitch + (p.pitch - prev.pitch) * k,
                    crouch: prev.crouch + (p.crouch - prev.crouch) * k,

                    alive: p.alive,
                    weapon: p.weapon,
                    team: p.team,
                    flags: p.flags,
                }
            })
            .collect()
    }
}

/// (v209) Essais : `MINELIB_FPS_START=x,y,z,lacet°` place le joueur local là à chaque départ
/// d'une partie FPS (mesures à vue identique).
fn fixed_fps_start() -> Option<(Vec3, f32)> {
    let v: Vec<f32> = std::env::var("MINELIB_FPS_START")
        .ok()?
        .split(',')
        .filter_map(|s| s.trim().parse().ok())
        .collect();
    (v.len() >= 4).then(|| (Vec3::new(v[0], v[1], v[2]), v[3].to_radians()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use minelib_net::client::{Client, Cmd, Event};
    use minelib_net::game::FpsMap;
    use minelib_world::{BlockId, ChunkPos};
    use std::time::{Duration, Instant};

    #[test]
    fn steps_are_heard_from_80_percent_of_ones_own_running_speed() {
        use crate::player::{FPS_SPRINT, FPS_WALK};
        // Couteau : 80 % de la pleine course.
        assert!((step_speed(250) - 0.8 * FPS_SPRINT).abs() < 1e-4);
        assert_eq!(step_speed(0), step_speed(250));
        // AK-47 (215) : 80 % de sa propre course, plus lente.
        assert!((step_speed(215) - 0.8 * FPS_SPRINT * 215.0 / 250.0).abs() < 1e-4);
        // AWP à la lunette (100) : on ne descend jamais sous le seuil de CS.
        assert_eq!(step_speed(100), STEP_SPEED);
        // Marcher reste silencieux, quelle que soit l'arme.
        for carry in [100, 200, 215, 250] {
            assert!(FPS_WALK * carry as f32 / 250.0 < step_speed(carry));
        }
    }

    #[test]
    fn doors_open_only_if_the_map_allows_it_and_within_reach() {
        let reg = minelib_world::registry::registry();
        let lower = reg.by_key("door_wood:lower:1").unwrap();
        let upper = reg.by_key("door_wood:upper:1").unwrap();
        let mut world = floor();
        world.set(5, 11, 5, lower);
        world.set(5, 12, 5, upper);
        let feet = Some(Vec3::new(5.5, 11.0, 7.5));
        let mut map = FpsMap::default();
        let mut m = Match::new(map.clone(), [0; 32], 1);
        assert!(
            use_door(&mut m, &world, feet, true, [5, 11, 5], 5).is_empty(),
            "carte sans portes"
        );
        map.set_doors(true);
        assert_eq!(
            map.players(),
            10,
            "le réglage ne change pas le nombre de joueurs"
        );
        let mut m = Match::new(map, [0; 32], 1);
        let far = Some(Vec3::new(5.5, 11.0, 20.0));
        assert!(
            use_door(&mut m, &world, far, true, [5, 11, 5], 5).is_empty(),
            "trop loin"
        );
        assert!(
            use_door(&mut m, &world, feet, false, [5, 11, 5], 5).is_empty(),
            "mort"
        );
        let open = use_door(&mut m, &world, feet, true, [5, 11, 5], 5);
        assert_eq!(open.len(), 2, "les deux moitiés");
        assert_eq!(m.doors.len(), 2);
        assert!(open.iter().all(|&(_, b)| b != lower.0 && b != upper.0));
        // Fermée par la moitié du haut, avant même que le monde ait changé : chaque moitié
        // retrouve son bloc d'origine.
        let mut close = use_door(&mut m, &world, feet, true, [5, 12, 5], 5);
        close.sort();
        assert_eq!(close, vec![([5, 11, 5], lower.0), ([5, 12, 5], upper.0)]);
        assert!(m.doors.is_empty());
    }

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
        // Tous les joueurs simulés sont sur cette machine.
        minelib_relay::ONE_FPS_PER_MACHINE.store(false, std::sync::atomic::Ordering::Relaxed);
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
        let mut world = floor();
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
                    Event::PeerJoined { peer, pseudo, .. } => {
                        host.peer_joined(peer, pseudo);
                        if guest.is_none() {
                            guest = Some(NetGame::guest(gl.clone(), hash));
                        }
                    }
                    Event::FromPeer { peer, data } => {
                        for l in host.receive(Some(peer), &data, &mut world) {
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
                        for l in g.receive(None, &data, &mut world) {
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
            // L'invité porte une AK (les règles le savent : on ne tire qu'avec son arme).
            if let NetGame::Host(h) = &mut host {
                for p in h.m.players.values_mut() {
                    p.guns.0 = ak();
                }
            }
            for _ in 0..4 {
                host_p.update(&world, &idle, dt);
                host.on_tick(&world, &host_p, &idle, dt);
                if let Some(g) = &mut guest {
                    guest_p.yaw = 0.0;
                    guest_p.carry = 215; // AK-47 en main
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

    /// Ce qu'on a mesuré dans une partie d'essai de la voie rapide (`fast_match`).
    struct FastRun {
        /// Écart entre l'invité et ce que l'hôte en voit, en pleine course puis à la fin.
        mid_gap: f32,
        end_gap: f32,
        /// Messages du relais retenus, entrées venues par les passeurs.
        held: u32,
        by_fast: u32,
        /// Route de l'invité chez l'hôte, à la fin.
        stats: minelib_net::fast::Stats,
        /// Images de l'invité, et celles où l'hôte avait trop de retard.
        frames: u32,
        stalled: u32,
        /// Passeurs en retard : l'invité a demandé le relais, l'hôte l'a su, et le retard
        /// des nouvelles de l'hôte (pas) à ce moment-là ; puis, les passeurs redevenus les
        /// plus rapides, tout le monde y est revenu.
        asked_relay: bool,
        host_knew: bool,
        behind: f32,
        came_back: bool,
        /// Retard (pas) du dernier état complet pris par l'invité, en pleine course.
        snap_behind: f32,
    }

    /// Voie rapide (`minelib_net::fast`) : une partie à deux par un vrai relais, avec deux
    /// passeurs. Une fois la voie ouverte et la manche lancée :
    /// - `passeurs_lag` faux : le relais prend un retard permanent de 40 images (un canal TCP
    ///   qui cale), les passeurs restent rapides ;
    /// - `passeurs_lag` vrai : ce sont les passeurs qui ont 40 images de retard (mal placés)
    ///   pendant 400 images, puis ils redeviennent plus rapides que le relais.
    ///
    /// L'invité court pendant 200 images, et on regarde où l'hôte le voit.
    fn fast_match(passeurs_lag: bool) -> FastRun {
        const LAG: usize = 40;
        let dir = std::env::temp_dir().join(format!("minelib-net-rapide-{}", rand_u32()));
        for p in ["Pas1", "Pas2"] {
            minelib_relay::test_seed_games(&dir, p, 5);
        }
        minelib_relay::ONE_FPS_PER_MACHINE.store(false, std::sync::atomic::Ordering::Relaxed);
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
        // Deux joueurs sans lien avec la partie prêtent leur port.
        let passeurs: Vec<Client> = ["Pas1", "Pas2"]
            .iter()
            .map(|p| {
                let (c, _) = login(p);
                c.send(Cmd::Passeur(true));
                c
            })
            .collect();
        let (hc, hl) = login("Hote");
        let (gc, gl) = login("Invite");
        hc.send(Cmd::Host {
            kind: 0,
            name: String::new(),
            map: "plat".into(),
            map_sig: [1; 32],
            max_players: 2,
        });
        let room = match wait(&hc, |e| matches!(e, Event::Hosted(_))) {
            Event::Hosted(r) => r,
            _ => unreachable!(),
        };
        gc.send(Cmd::Join(room));
        wait(&gc, |e| matches!(e, Event::Joined { .. }));
        let mut world = floor();
        let hash = [1u8; 32];
        let spawn = Vec3::new(8.5, 11.0, 8.5);
        // Un contre un : la partie commence dès que l'invité est là.
        let mut map = FpsMap::default();
        map.set_players(2);
        let mut host = NetGame::host(hl, "Hote".into(), map, hash, spawn);
        let mut host_p = Player::new(Vec3::new(8.5, 11.0, 2.5));
        let mut guest_p = Player::new(Vec3::new(0.0, 30.0, 0.0));
        let mut guest: Option<NetGame> = None;
        let dt = crate::session::SIM_TICK;
        // Ce qui est retenu en route, et jusqu'à quelle image.
        let mut late_h: VecDeque<(usize, Event)> = VecDeque::new();
        let mut late_g: VecDeque<(usize, Event)> = VecDeque::new();
        let mut slow_from: Option<usize> = None;
        let (mut held, mut by_fast) = (0u32, 0u32);
        let mut mid_gap = f32::NAN;
        let mut hfast: Option<minelib_net::fast::Endpoint> = None;
        let (mut asked_relay, mut host_knew, mut behind) = (false, false, f32::NAN);
        let mut snap_behind = f32::NAN;
        // États complets déjà vus passer : ils viennent par le relais et par les passeurs,
        // la première copie de chaque pas est celle qui n'a pas de retard.
        let mut snaps_seen: std::collections::HashSet<u32> = std::collections::HashSet::new();
        let mut done = false;
        for frame in 0..8000 {
            // Depuis combien d'images le retard est en place, et sur quel chemin.
            let since = slow_from.map(|f| frame - f);
            let healed = passeurs_lag && since.is_some_and(|n| n >= 400);
            let (lag_relay, lag_fast) = match since {
                None => (0, 0),
                Some(_) if !passeurs_lag => (LAG, 0),
                Some(_) if healed => (3, 0),
                Some(_) => (0, LAG),
            };
            // L'invité demande-t-il le relais en ce moment ? (Alors les positions viennent
            // du relais et les sondes des passeurs ; sinon les positions viennent des
            // passeurs.)
            let via_relay = matches!(&guest, Some(NetGame::Guest(g)) if g.fast_slow);
            // Hôte.
            let mut now: Vec<Event> = Vec::new();
            while late_h.front().is_some_and(|(at, _)| *at <= frame) {
                now.push(late_h.pop_front().unwrap().1);
            }
            while let Some(e) = hc.poll() {
                let fast = matches!(&e, Event::FromPeer { data, .. }
                    if matches!(game::decode::<ToHost>(data), Some(ToHost::InputsFast { .. })));
                if fast {
                    by_fast += 1;
                }
                let lag = match &e {
                    Event::FromPeer { .. } if fast => lag_fast,
                    Event::FromPeer { .. } => lag_relay,
                    _ => 0,
                };
                if lag > 0 {
                    held += 1;
                    late_h.push_back((frame + lag, e));
                } else {
                    now.push(e);
                }
            }
            for e in now {
                match e {
                    Event::PeerJoined { peer, pseudo, .. } => {
                        host.peer_joined(peer, pseudo);
                        if guest.is_none() {
                            guest = Some(NetGame::guest(gl.clone(), hash));
                        }
                    }
                    Event::FromPeer { peer, data } => {
                        host.receive(Some(peer), &data, &mut world);
                    }
                    Event::Fast(end) => {
                        hfast = end.clone();
                        host.set_fast(end);
                    }
                    _ => {}
                }
            }
            // Invité.
            if let Some(g) = &mut guest {
                let mut now: Vec<Event> = Vec::new();
                while late_g.front().is_some_and(|(at, _)| *at <= frame) {
                    now.push(late_g.pop_front().unwrap().1);
                }
                while let Some(e) = gc.poll() {
                    let fast = match &e {
                        Event::FromHost(data) => match game::decode::<ToPeer>(data) {
                            Some(ToPeer::Moves { .. }) => !via_relay,
                            Some(ToPeer::FastPing { .. }) => true,
                            // Relais en retard : la copie venue par les passeurs passe.
                            Some(ToPeer::Snapshot { tick, .. }) => {
                                since.is_some() && !passeurs_lag && snaps_seen.insert(tick)
                            }
                            _ => false,
                        },
                        _ => false,
                    };
                    let lag = match &e {
                        Event::FromHost(_) if fast => lag_fast,
                        Event::FromHost(_) => lag_relay,
                        _ => 0,
                    };
                    if lag > 0 {
                        late_g.push_back((frame + lag, e));
                    } else {
                        now.push(e);
                    }
                }
                for e in now {
                    match e {
                        Event::FromHost(data) => {
                            for l in g.receive(None, &data, &mut world) {
                                if let Local::Respawn { state, .. } = l {
                                    guest_p.set_net_state(&state);
                                }
                            }
                        }
                        Event::Fast(end) => g.set_fast(end),
                        _ => {}
                    }
                }
            }
            let live = guest
                .as_ref()
                .is_some_and(|g| g.phase() == game::phase::LIVE && !g.frozen());
            // La manche est lancée et la voie ouverte des deux côtés : le retard commence.
            if slow_from.is_none()
                && live
                && host.fast_open()
                && guest.as_ref().is_some_and(|g| g.fast_open())
            {
                slow_from = Some(frame);
            }
            // L'invité court de la 80e à la 280e image du retard.
            let walking = since.is_some_and(|n| (2 * LAG..2 * LAG + 200).contains(&n));
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
            for l in host.end_frame(&world, Vec::new(), 255, NadeOut::default(), 4.0 * dt) {
                if let Local::Respawn { state, .. } = l {
                    host_p.set_net_state(&state);
                }
            }
            if let Some(g) = &mut guest {
                g.end_frame(&world, Vec::new(), 255, NadeOut::default(), 4.0 * dt);
            }
            let (NetGame::Host(hs), Some(NetGame::Guest(gs))) = (&host, &guest) else {
                continue;
            };
            let Some((&peer, remote)) = hs.peers.iter().next() else {
                continue;
            };
            // En pleine course, 150 images après le départ : où l'hôte voit-il l'invité ?
            if since == Some(2 * LAG + 150) {
                mid_gap = (remote.player.pos - guest_p.pos).length();
                snap_behind = gs.clock - gs.snap_tick.unwrap_or(0) as f32;
            }
            // Passeurs en retard depuis 390 images : qu'en ont conclu l'invité et l'hôte ?
            if passeurs_lag && since == Some(390) {
                asked_relay = gs.fast_slow;
                host_knew = hfast.as_ref().is_some_and(|f| f.slow(peer));
                behind = gs.clock - gs.seen_tick.unwrap_or(0) as f32;
            }
            // Fin : tout ce qui était retenu est arrivé (et les passeurs ont eu le temps de
            // redevenir les plus rapides).
            if since == Some(if passeurs_lag { 1200 } else { 2 * LAG + 200 + 3 * LAG }) {
                done = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(done, "la voie rapide ne s'est jamais ouverte");
        let (NetGame::Host(hs), Some(NetGame::Guest(gs))) = (&host, &guest) else {
            unreachable!()
        };
        let (&peer, remote) = hs.peers.iter().next().expect("invité chez l'hôte");
        let (frames, stalled) = gs.stalls();
        let run = FastRun {
            mid_gap,
            end_gap: (remote.player.pos - guest_p.pos).length(),
            held,
            by_fast,
            stats: hfast.as_ref().and_then(|f| f.stats(peer)).expect("route"),
            frames,
            stalled,
            asked_relay,
            host_knew,
            behind,
            came_back: !gs.fast_slow && !hfast.as_ref().is_some_and(|f| f.slow(peer)),
            snap_behind,
        };
        drop(passeurs);
        let _ = std::fs::remove_dir_all(dir);
        run
    }

    /// Le relais cale (40 images de retard) : l'hôte suit quand même l'invité de près, parce
    /// que ses entrées arrivent par les passeurs, et l'invité continue de se corriger sur
    /// l'hôte. À la fin, quand tout ce qui était retenu est arrivé (en double, donc), l'hôte
    /// a joué exactement les entrées de l'invité : ni trou, ni doublon.
    #[test]
    fn the_fast_way_keeps_up_when_the_relay_lags() {
        let r = fast_match(false);
        // L'invité court à plus de 4 blocs par seconde : par le relais seul, l'hôte l'aurait
        // vu avec 40 images de retard, soit 3 blocs en arrière.
        assert!(r.mid_gap < 1.0, "en pleine course, l'hôte suit l'invité à {} bloc", r.mid_gap);
        assert!(r.held > 100, "le relais a bien été retenu ({} messages)", r.held);
        assert!(r.by_fast > 100, "entrées venues par les passeurs : {}", r.by_fast);
        // Ni trou ni doublon : la prédiction de l'invité est l'état de l'hôte.
        assert!(r.end_gap < 0.05, "prédiction de l'invité = hôte (écart {})", r.end_gap);
        // Les positions sont parties par les passeurs, chacun sa part.
        assert_eq!(r.stats.heard, 3, "les deux passeurs répondent");
        assert!(r.stats.sent > 500, "paquets partis par la voie rapide : {}", r.stats.sent);
        assert!(
            r.stalled * 10 < r.frames.max(1),
            "images bloquées chez l'invité : {} sur {}",
            r.stalled,
            r.frames
        );
        // (v274) Les états complets viennent aussi par les passeurs : la vie, l'arme et le
        // tableau de bord ne sont pas figés par le retard du relais (160 pas).
        assert!(r.snap_behind < 24.0, "dernier état complet vieux de {} pas", r.snap_behind);
    }

    /// Les passeurs sont mal placés (40 images de retard) : la voie rapide ne doit jamais
    /// faire moins bien que le relais. L'invité s'en aperçoit et demande ses positions par
    /// le relais, l'hôte suit, et rien n'a de retard ; les entrées, prises à la première
    /// arrivée, viennent du relais. Puis les passeurs redeviennent les plus rapides : les
    /// sondes le montrent, et tout le monde y revient.
    #[test]
    fn the_relay_takes_over_when_the_passeurs_are_slower() {
        let r = fast_match(true);
        assert!(r.asked_relay, "l'invité a demandé ses positions par le relais");
        assert!(r.host_knew, "l'hôte a su que l'invité préférait le relais");
        assert!(r.behind < 8.0, "nouvelles de l'hôte en retard de {} pas", r.behind);
        assert!(r.mid_gap < 1.0, "en pleine course, l'hôte suit l'invité à {} bloc", r.mid_gap);
        assert!(r.end_gap < 0.05, "prédiction de l'invité = hôte (écart {})", r.end_gap);
        assert!(r.came_back, "les passeurs redevenus rapides, on y revient");
    }

    #[test]
    fn weapons_are_dropped_and_picked_up_with_a_swap() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let spawn = Vec3::new(0.5, 11.0, 0.5);
        let mut g = NetGame::host(tx, "Hote".into(), FpsMap::default(), [0; 32], spawn);
        let awp = WEAPONS.iter().position(|w| w.name == "AWP").unwrap() as u8;
        let ak = ak();
        host(&mut g).m.players.get_mut(&0).unwrap().guns = (ak, 255);
        g.drop_weapon(awp, spawn);
        assert!(g.ground().is_empty(), "pas une arme qu'il porte");
        g.drop_weapon(ak, spawn);
        assert_eq!(g.ground().len(), 1);
        let h = host(&mut g);
        h.m.ground.push(GroundGun {
            id: 99,
            weapon: awp,
            pos: spawn.to_array(),
            from: spawn.to_array(),
        });
        h.m.players.get_mut(&0).unwrap().guns = (ak, 255);
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
        // Tous les joueurs simulés sont sur cette machine.
        minelib_relay::ONE_FPS_PER_MACHINE.store(false, std::sync::atomic::Ordering::Relaxed);
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
            players: 10,
        });
        wait(&hc, |e| matches!(e, Event::Hosted(_)));
        gc.send(Cmd::Search {
            map_sig: sig,
            map: "plat".into(),
            players: 10,
        });
        wait(&gc, |e| matches!(e, Event::Joined { .. }));
        let mut world = floor();
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
                    Event::PeerJoined { peer, pseudo, .. } => host.peer_joined(peer, pseudo),
                    Event::FromPeer { peer, data } => {
                        host.receive(Some(peer), &data, &mut world);
                    }
                    _ => {}
                }
            }
            while let Some(e) = gc.poll() {
                match e {
                    Event::FromHost(data) => {
                        guest.receive(None, &data, &mut world);
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
        let mut world = floor();
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
        let c = audit(&b1, &b2, &mut world);
        assert!(c.is_empty(), "partie honnête : {c:?}");
        // L'hôte se téléporte (sans entrée qui l'explique) : preuve.
        let mut tele = game::decode::<Backup>(&game::encode(&b2)).unwrap();
        tele.players.iter_mut().find(|p| p.0 == 0).unwrap().2.pos[0] += 3.0;
        let c = audit(&b1, &tele, &mut world);
        assert!(c.iter().any(|c| c.player == 0 && c.proof));
        // L'hôte déplace un invité : désaccord à juger par l'invité.
        let mut moved = game::decode::<Backup>(&game::encode(&b2)).unwrap();
        moved.players.iter_mut().find(|p| p.0 == 7).unwrap().2.pos[2] -= 2.0;
        let c = audit(&b1, &moved, &mut world);
        assert!(c.iter().any(|c| c.player == 7 && !c.proof && c.seq > 0));
        // Coup effacé du journal : chaîne fausse.
        let mut cut = game::decode::<Backup>(&game::encode(&b2)).unwrap();
        cut.log.remove(5);
        assert!(audit(&b1, &cut, &mut world).iter().any(|c| c.proof));
    }

    /// Une vraie partie à 10 joueurs par le relais, jusqu'au bout : 5 contre 5, tirs,
    /// achats, grenades, manches, fin de partie chez tous. Le garant de secours vérifie
    /// chaque copie en rejouant toute la partie : pas un seul désaccord, et il trouve le
    /// même résultat final que l'hôte.
    #[test]
    fn ten_players_play_a_whole_match_and_the_auditor_agrees() {
        let dir = std::env::temp_dir().join(format!("minelib-dix-{}", rand_u32()));
        // Tous les joueurs simulés sont sur cette machine.
        minelib_relay::ONE_FPS_PER_MACHINE.store(false, std::sync::atomic::Ordering::Relaxed);
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
        let mut world = floor();
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
        let mut he_thrown = 0;
        // Lancers acceptés par l'hôte (la grenade n'explose que si elle a été lancée).
        let mut accepted: Vec<u64> = Vec::new();
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
                    Event::PeerJoined { peer, pseudo, .. } => {
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
                        for l in host.receive(Some(peer), &data, &mut world) {
                            match l {
                                Local::Started => started = true,
                                Local::GameOver(s) => host_over = Some(s),
                                Local::Thrown { owner, .. } => accepted.push(owner),
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
                        for l in b.net.receive(None, &data, &mut world) {
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
            let state: Vec<(u64, bool, u8, Vec3)> =
                h.m.players
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
            // Qui porte une HE (achetée pendant le départ de manche).
            let with_he: Vec<u64> =
                h.m.players
                    .iter()
                    .filter(|(_, p)| p.alive && p.nades[HE as usize] > 0)
                    .map(|(&id, _)| id)
                    .collect();
            let gun = |id: u64| {
                state
                    .iter()
                    .find(|s| s.0 == id)
                    .map_or(255, |s| crate::cs::starter(s.2))
            };
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
                host_p.others = host.obstacles();
                host_p.carry = 240; // pistolet en main
                host_p.update(&world, &hm, dt);
                host.on_tick(&world, &host_p, &hm, dt);
                for (k, b) in bots.iter_mut().enumerate() {
                    let m = walk(k + 1);
                    b.p.others = b.net.obstacles();
                    b.p.carry = 240;
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
                    } else {
                        let _ = b.net.buy(game::ITEM_NADE + HE as u16);
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
                if let Some(k) = bots.iter().position(|b| with_he.contains(&b.id)) {
                    let b = &bots[k];
                    he_thrown += 1;
                    thrown.push((frame as usize + 100, b.id, b.p.eye()));
                    let nades = NadeOut {
                        throws: vec![(HE, b.p.eye(), Vec3::new(0.0, 2.0, 3.0))],
                        ..Default::default()
                    };
                    bots[k]
                        .net
                        .end_frame(&world, Vec::new(), ak(), nades, 4.0 * dt);
                }
            }
            thrown.retain(|&(when, owner, at)| {
                let ok = accepted.iter().position(|o| *o == owner);
                if let (true, Some(k)) = (when == frame as usize, ok) {
                    accepted.remove(k);
                    host_nades
                        .blasts
                        .push((at + Vec3::new(0.0, -1.0, 4.0), owner));
                }
                when > frame as usize
            });
            let mut host_shots = Vec::new();
            if frame.is_multiple_of(12) {
                if let Some(d) = aim(0, host_p.eye()) {
                    host_shots.push((gun(0), host_p.eye(), vec![d]));
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
                        shots.push((gun(b.id), b.p.eye(), vec![d]));
                    }
                }
                b.net
                    .end_frame(&world, shots, ak(), NadeOut::default(), 4.0 * dt);
            }
            if frame.is_multiple_of(4) {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        // Les derniers messages (copie finale) arrivent.
        for _ in 0..200 {
            for b in &mut bots {
                while let Some(e) = b.c.poll() {
                    if let Event::FromHost(data) = e {
                        b.net.receive(None, &data, &mut world);
                    }
                }
                conflicts.extend(b.net.take_conflicts());
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(started, "la partie a commencé à 10");
        // Les robots n'achètent pas toujours une HE à temps quand la machine est chargée :
        // signalé, pas bloquant (les grenades ont leurs propres essais).
        if he_thrown == 0 {
            eprintln!("aucune HE lancée pendant cette partie d'essai");
        }
        let summary = host_over.expect("fin de partie chez l'hôte");
        assert!(
            summary.score[0] >= 13 || summary.score[1] >= 13,
            "{:?}",
            summary.score
        );
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
        // Aucune preuve contre l'hôte honnête ; un soupçon isolé (décalage d'une image sous
        // forte charge) est toléré, jamais plus de quelques-uns.
        let proofs: Vec<_> = conflicts.iter().filter(|c| c.proof).collect();
        assert!(proofs.is_empty(), "aucune preuve : {proofs:?}");
        assert!(
            conflicts.len() <= 3,
            "presque aucun soupçon : {conflicts:?}"
        );
        assert_eq!(
            g.audit.results.first().map(game::encode),
            Some(game::encode(&summary)),
            "le vérificateur trouve le même résultat"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// (v258) Une porte posée pour la partie (`/fps set door`), ouverte par un invité alors que
    /// toutes ne s'ouvrent pas : la copie de la partie n'emporte pas les portes posées
    /// (l'encodage de la carte ne les a pas) ; le vérificateur et l'arbitre y remettent celles
    /// de leur carte et n'ont rien à redire. Sans elles, ils refusaient la porte et prenaient
    /// l'hôte honnête pour un tricheur.
    #[test]
    fn a_door_set_for_the_match_is_not_held_against_the_host() {
        fn run(g: &mut NetGame, world: &World, local: &mut Player, seq: &mut u32, n: usize) {
            let dt = crate::session::SIM_TICK;
            let idle = MoveInput {
                fps: true,
                ..Default::default()
            };
            for _ in 0..n {
                let NetGame::Host(h) = g else { unreachable!() };
                *seq += 1;
                let r = h.peers.get_mut(&7).unwrap();
                r.queue.push_back((*seq, NetInput::default()));
                local.update(world, &idle, dt);
                g.on_tick(world, local, &idle, dt);
                for l in g.end_frame(world, Vec::new(), 255, NadeOut::default(), dt) {
                    if let Local::Respawn { state, .. } = l {
                        local.set_net_state(&state);
                    }
                }
            }
        }
        let reg = minelib_world::registry::registry();
        let lower = reg.by_key("door_wood:lower:1").unwrap();
        let upper = reg.by_key("door_wood:upper:1").unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut world = floor();
        let spawn = Vec3::new(8.5, 11.0, 8.5);
        let mut g = NetGame::host(tx, "Hote".into(), FpsMap::default(), [0; 32], spawn);
        g.peer_joined(7, "Invite".into());
        let hello = game::encode(&ToHost::Hello { map_hash: [0; 32] });
        g.receive(Some(7), &hello, &mut world);
        host(&mut g).auto_backup = false;
        let mut local = Player::new(Vec3::new(4.5, 11.0, 4.5));
        let mut seq = 0;
        run(&mut g, &world, &mut local, &mut seq, 300);
        // La porte, à côté de l'invité, posée pour la partie (toutes ne s'ouvrent pas).
        let at = host(&mut g).peers[&7].player.pos;
        let d = [at.x.floor() as i32 + 1, at.y.floor() as i32, at.z.floor() as i32];
        let with_door = |mut w: World| {
            w.set(d[0], d[1], d[2], lower);
            w.set(d[0], d[1] + 1, d[2], upper);
            w
        };
        world = with_door(world);
        let doors = vec![game::Zone::new(d, [d[0], d[1] + 1, d[2]])];
        host(&mut g).m.map.door_list = doors.clone();
        assert!(!host(&mut g).m.map.doors());
        // Les copies, telles qu'elles voyagent (encodées).
        let sent = |b: Backup| game::decode::<Backup>(&game::encode(&b)).unwrap();
        let b0 = sent(host(&mut g).make_backup(&local));
        assert!(b0.m.map.door_list.is_empty(), "la copie n'emporte pas les portes posées");
        let door = ToHost::Door { pos: d, side: 5 };
        g.receive(Some(7), &game::encode(&door), &mut world);
        assert_eq!(host(&mut g).m.doors.len(), 2, "l'hôte ouvre la porte posée");
        run(&mut g, &world, &mut local, &mut seq, 100);
        let b1 = sent(host(&mut g).make_backup(&local));
        let audit = |doors: &[game::Zone]| {
            let mut a = Auditor {
                witnessing: true,
                ..Default::default()
            };
            a.witness(7, door.clone());
            let mut aw = with_door(floor());
            a.audit(
                &host_copy(&b0).with_doors(doors),
                &host_copy(&b1).with_doors(doors),
                &mut aw,
            )
        };
        let fine = audit(&doors);
        assert!(fine.is_empty(), "{fine:?}");
        let wrong = audit(&[]);
        assert!(wrong.iter().any(|c| c.proof), "sans les portes posées : {wrong:?}");
        // L'arbitre rejoue les mêmes pièces avec les portes de sa carte : rien à reprocher.
        let pieces = vec![
            (None, game::encode(&b0)),
            (Some(7), game::encode(&door)),
            (None, game::encode(&b1)),
        ];
        assert!(!arbitrate(&pieces, &mut with_door(floor()), &doors, |_, _| {}));
        assert!(arbitrate(&pieces, &mut with_door(floor()), &[], |_, _| {}));
    }

    /// Copie d'une copie de la partie (elle n'est pas `Clone`) : encodée puis relue.
    fn host_copy(b: &Backup) -> Backup {
        game::decode::<Backup>(&game::encode(b)).unwrap()
    }

    /// Le vérificateur attrape aussi les faux dégâts, les tirs inventés ou ignorés, les
    /// explosions sans grenade et les scores retouchés.
    #[test]
    fn the_auditor_catches_fake_hits_damage_and_scores() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let world = floor();
        // Monde à part pour le vérificateur (il y remet les portes de chaque copie).
        let mut aw = floor();
        let spawn = Vec3::new(8.5, 11.0, 8.5);
        let mut g = NetGame::host(tx, "Hote".into(), FpsMap::default(), [0; 32], spawn);
        g.peer_joined(7, "Invite".into());
        let hello = game::encode(&ToHost::Hello { map_hash: [0; 32] });
        g.receive(Some(7), &hello, &mut aw);
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
        for p in host(&mut g).m.players.values_mut() {
            p.guns.0 = ak();
        }
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
            blind: false,
        };
        a.witness(7, fire.clone());
        g.receive(Some(7), &game::encode(&fire), &mut aw);
        run(&mut g, &mut local, 100);
        let b1 = backup(&mut g, &local);
        assert!(a.audit(&b0, &b1, &mut aw).is_empty());
        assert!(host(&mut g).m.players[&0].hp < 100, "l'hôte est touché");
        // Dégâts qui ne viennent d'aucun coup du journal : règles différentes.
        run(&mut g, &mut local, 50);
        let _ = host(&mut g).m.hurt(Some(7), 0, 30, Hurt::Knife);
        run(&mut g, &mut local, 50);
        let b2 = backup(&mut g, &local);
        let c = a.audit(&b1, &b2, &mut aw);
        assert!(why(&c).iter().any(|w| w.starts_with("règles")), "{c:?}");
        // Le troisième garant (arbitre) rejoue les mêmes pièces, gardées par le relais :
        // rien à reprocher tant que l'hôte est honnête, coupable avec les faux dégâts.
        let mut pieces = vec![
            (None, game::encode(&b0)),
            (Some(7), game::encode(&fire)),
            (None, game::encode(&b1)),
        ];
        let mut w2 = floor();
        assert!(!arbitrate(&pieces, &mut w2, &[], |_, _| {}));
        pieces.push((None, game::encode(&b2)));
        assert!(arbitrate(&pieces, &mut w2, &[], |_, _| {}));
        // Tir inventé par l'hôte au nom de l'invité (le relais ne l'a jamais vu passer).
        let fake = ToHost::Fire {
            weapon: ak(),
            origin: eye.to_array(),
            dirs: vec![[1.0, 0.0, 0.0]],
            view_tick: host(&mut g).tick,
            blind: false,
        };
        g.receive(Some(7), &game::encode(&fake), &mut aw);
        run(&mut g, &mut local, 100);
        let b3 = backup(&mut g, &local);
        let c = a.audit(&b2, &b3, &mut aw);
        assert!(why(&c).contains(&"demande inventée par l'hôte"), "{c:?}");
        // Tir de l'invité que l'hôte « oublie » : signalé quelques copies plus tard.
        a.witness(7, fake.clone());
        let mut prev = b3;
        let mut found = false;
        for _ in 0..=WITNESS_LATE + 1 {
            run(&mut g, &mut local, 50);
            let b = backup(&mut g, &local);
            found |= why(&a.audit(&prev, &b, &mut aw))
                .contains(&"demande d'un joueur ignorée par l'hôte");
            prev = b;
        }
        assert!(found, "tir ignoré signalé");
        // Explosion d'une HE que personne n'a lancée, longtemps après le début du suivi.
        run(&mut g, &mut local, 4 * 256);
        let b4 = backup(&mut g, &local);
        let _ = a.audit(&prev, &b4, &mut aw);
        let nades = NadeOut {
            blasts: vec![(local.pos + Vec3::Y, 7)],
            ..Default::default()
        };
        g.end_frame(&world, Vec::new(), 255, nades, dt);
        run(&mut g, &mut local, 20);
        let b5 = backup(&mut g, &local);
        let c = a.audit(&b4, &b5, &mut aw);
        assert!(why(&c).contains(&"explosion sans grenade"), "{c:?}");
        // Éliminations ajoutées à la main dans la copie.
        run(&mut g, &mut local, 20);
        let mut b6 = backup(&mut g, &local);
        b6.m.players.get_mut(&7).unwrap().kills += 5;
        let c = a.audit(&b5, &b6, &mut aw);
        assert!(why(&c).iter().any(|w| w.starts_with("règles")), "{c:?}");
    }

    /// Mi-hauteur de la tête du modèle (blocs).
    const HEAD_HALF_HEIGHT: f32 = crate::player_model::HEAD_BLOCKS / 2.0;

    /// (v225) Tête aux proportions de CS2 : sa boîte est plus étroite que le corps. Une balle
    /// qui passe à côté d'elle, à sa hauteur, ne touche pas ; juste dessous, c'est l'épaule.
    #[test]
    fn the_head_hitbox_is_narrower_than_the_body() {
        let feet = Vec3::new(3.5, 64.0, -2.5);
        let (body, head, head_y) = hitbox(feet, 0.0, 0.0);
        let shoot = |y: f32, dz: f32| {
            ray_player(
                Vec3::new(feet.x - 5.0, feet.y + y, feet.z + dz),
                Vec3::X,
                body,
                head,
            )
        };
        assert!(matches!(shoot(1.7, 0.0), Some((_, true))), "tête");
        assert!(shoot(1.7, 0.25).is_none(), "à côté de la tête");
        assert!(matches!(shoot(1.45, 0.25), Some((_, false))), "épaule");
        assert!(matches!(shoot(0.5, 0.0), Some((_, false))), "jambes");
        assert!(
            (head_y - feet.y - (HEIGHT - crate::player_model::HEAD_BLOCKS)).abs() < 1e-5
        );
        // (v234) Accroupi : la boîte de la tête suit la tête du modèle, qui avance avec le cou
        // vers le regard (ici −z, puis +x) et descend.
        let (ahead, bottom) = crate::player_model::head_place(1.0);
        for (yaw, dir) in [(0.0, -Vec3::Z), (std::f32::consts::FRAC_PI_2, Vec3::X)] {
            let (_, low, low_y) = hitbox(feet, 1.0, yaw);
            let mid = (low[0] + low[1]) / 2.0;
            let want = feet + dir * ahead + Vec3::Y * (bottom + HEAD_HALF_HEIGHT);
            assert!(
                (mid - want).length() < 1e-4,
                "{yaw} : {mid} au lieu de {want}"
            );
            assert!((low_y - feet.y - bottom).abs() < 1e-5);
            assert!(low[1].y < feet.y + 1.6, "la tête descend : {}", low[1].y);
        }
        // Une balle visée sur la tête avancée la touche ; au-dessus des pieds, plus.
        let (body, head, _) = hitbox(feet, 1.0, 0.0);
        let y = bottom + HEAD_HALF_HEIGHT;
        let at = |dz: f32| {
            ray_player(
                Vec3::new(feet.x - 5.0, feet.y + y, feet.z + dz),
                Vec3::X,
                body,
                head,
            )
        };
        assert!(matches!(at(-ahead), Some((_, true))), "tête avancée");
        assert!(!matches!(at(0.2), Some((_, true))), "derrière la tête");
    }

    /// Dégâts comme dans CS2 : une balle de Glock dans la tête sans casque tue de près
    /// (120), plus à 20 blocs ; l'AK tue toujours d'une balle dans la tête ; ventre ×1,25.
    #[test]
    fn bullet_damage_follows_cs2() {
        let gun = |n: &str| WEAPONS.iter().position(|w| w.name == n).unwrap() as u8;
        let dmg = |w: u8, dist: f32, zone: HitZone| damage_at(w, dist, zone).0;
        let glock = gun("Glock-18");
        assert_eq!(dmg(glock, 0.0, HitZone::Head), 120);
        let far = dmg(glock, 20.0, HitZone::Head);
        assert!(far < 100, "{far}");
        assert_eq!(dmg(glock, 0.0, HitZone::Chest), 30, "poitrine");
        assert_eq!(dmg(glock, 0.0, HitZone::Arm), 30, "bras");
        assert_eq!(dmg(glock, 0.0, HitZone::Belly), 37, "ventre");
        assert_eq!(dmg(glock, 0.0, HitZone::Leg), 22, "jambes");
        let ak = gun("AK-47");
        assert!(
            dmg(ak, 60.0, HitZone::Head) > 100,
            "AK : une balle dans la tête, même de loin"
        );
        // Avec casque : le Glock ne tue jamais d'une balle (47 % passent), l'AK si.
        let mut m = Match::new(FpsMap::default(), [0; 32], 7);
        m.join(1, "A");
        m.join(2, "B");
        let hit = |m: &mut Match, w: u8| {
            let p = m.players.get_mut(&2).unwrap();
            (p.hp, p.armor, p.helmet, p.alive) = (100, 100, true, true);
            let (d, h) = damage_at(w, 0.0, HitZone::Head);
            m.hurt(Some(1), 2, d, h);
            m.players[&2].hp
        };
        assert_eq!(hit(&mut m, glock), 100 - 56);
        assert_eq!(hit(&mut m, ak), 0);
    }

    /// (v256) « Les hitbox devront être celles de celui-ci aussi » : une balle touche les
    /// pavés du modèle. De face (le joueur regarde vers −z) : tête, poitrine, ventre plus
    /// étroit que la poitrine, jambes écartées (une balle passe entre elles). Les boîtes
    /// tournent avec lui et la tête suit son regard.
    #[test]
    fn bullets_hit_the_boxes_of_the_model() {
        let feet = Vec3::new(3.5, 64.0, -2.5);
        // Tir de face, à `x` du milieu du joueur et `y` au-dessus de ses pieds.
        let front = |x: f32, y: f32, yaw: f32, pitch: f32, crouch: f32| {
            let (s, c) = yaw.sin_cos();
            // Devant lui : là où il regarde ; sa droite : +x quand il regarde vers −z.
            let (fwd, right) = (Vec3::new(s, 0.0, -c), Vec3::new(c, 0.0, s));
            let o = feet + fwd * 5.0 + right * x + Vec3::Y * y;
            ray_model(o, -fwd, feet, crouch, yaw, pitch).map(|(_, z)| z)
        };
        for yaw in [0.0, 0.7, std::f32::consts::FRAC_PI_2, 3.0] {
            assert_eq!(front(0.0, 1.66, yaw, 0.0, 0.0), Some(HitZone::Head), "{yaw}");
            assert_eq!(front(0.25, 1.66, yaw, 0.0, 0.0), None, "à côté de la tête");
            assert_eq!(front(0.1, 1.2, yaw, 0.0, 0.0), Some(HitZone::Chest), "{yaw}");
            assert_eq!(front(0.17, 1.2, yaw, 0.0, 0.0), Some(HitZone::Chest), "bord");
            assert_eq!(front(0.1, 1.0, yaw, 0.0, 0.0), Some(HitZone::Belly), "{yaw}");
            assert_eq!(front(0.17, 1.0, yaw, 0.0, 0.0), None, "à côté du ventre");
            assert_eq!(front(0.1, 0.4, yaw, 0.0, 0.0), Some(HitZone::Leg), "{yaw}");
            assert_eq!(front(-0.1, 0.4, yaw, 0.0, 0.0), Some(HitZone::Leg), "{yaw}");
            assert_eq!(front(0.0, 0.4, yaw, 0.0, 0.0), None, "entre les jambes");
            assert_eq!(front(0.0, 1.9, yaw, 0.0, 0.0), None, "au-dessus");
        }
        // Les bras sont tendus vers le regard : le poing droit, devant l'épaule.
        assert_eq!(front(0.3, 1.44, 0.0, 0.0, 0.0), Some(HitZone::Arm));
        // Le regard baissé : la tête penche vers l'avant, son haut descend.
        assert_eq!(front(0.0, 1.78, 0.0, 0.0, 0.0), Some(HitZone::Head));
        assert_ne!(front(0.0, 1.78, 0.0, -1.2, 0.0), Some(HitZone::Head));
        // Accroupi : la tête avance avec le cou et descend ; plus rien à 1,6 bloc.
        let (_, bottom) = crate::player_model::head_place(1.0);
        let y = bottom + HEAD_HALF_HEIGHT;
        assert_eq!(front(0.0, y, 0.0, 0.0, 1.0), Some(HitZone::Head));
        assert_eq!(front(0.0, 1.6, 0.0, 0.0, 1.0), None);
        // Le milieu de la poitrine que visent les bots est bien dans la poitrine, de tous
        // les côtés, debout comme accroupi.
        for crouch in [0.0, 1.0] {
            for k in 0..8 {
                let yaw = 0.4;
                let at = crate::player_model::chest_point(feet, yaw, crouch);
                let a = k as f32 * std::f32::consts::FRAC_PI_4;
                let from = at + Vec3::new(a.cos(), 0.1, a.sin()) * 6.0;
                let dir = (at - from).normalize();
                let hit = ray_model(from, dir, feet, crouch, yaw, 0.0);
                assert!(
                    matches!(hit, Some((_, HitZone::Chest | HitZone::Arm))),
                    "{crouch} {k} : {hit:?}"
                );
            }
        }
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

    // --- (v172) Bots de la partie solo --------------------------------------------------

    /// Partie de bots seuls (l'hôte ne joue pas), relevé de la carte fait d'avance.
    fn bots_only(world: &World, map: FpsMap, spawn: Vec3, calm: bool) -> NetGame {
        let (link, _) = std::sync::mpsc::channel();
        let players = map.players() as usize;
        let mut g = NetGame::host(link, "Hote".into(), map.clone(), [0; 32], spawn);
        if let NetGame::Host(h) = &mut g {
            h.m = Match::new(map.clone(), [0; 32], 7);
            h.m.fallback_spawns = Some([spawn - Vec3::X * 10.0, spawn + Vec3::X * 10.0]);
            h.playing = false;
            h.out.clear();
            h.spawns.clear();
            h.solo = true;
            h.auto_backup = false;
            let mut bots = crate::bots::Bots::new(11, 2);
            bots.set_nav(crate::bots::Nav::build(world, &map, &[spawn]));
            bots.set_calm(calm);
            for (i, name) in crate::bots::Bots::names(players, 3).into_iter().enumerate() {
                let id = crate::bots::BOT_BASE + i as u64 + 1;
                bots.add(id);
                h.add_bot(id, name);
            }
            h.bots = Some(Box::new(bots));
        }
        g
    }

    fn host_of(g: &mut NetGame) -> &mut Host {
        match g {
            NetGame::Host(h) => h,
            NetGame::Guest(_) => panic!("hôte attendu"),
        }
    }

    /// Fait jouer la partie de bots `g` au plus `seconds` secondes de jeu (temps d'achat et
    /// échauffement écourtés), en appelant `watch` à chaque pas (vrai : on arrête).
    fn run_bots(
        g: &mut NetGame,
        world: &World,
        seconds: f32,
        mut watch: impl FnMut(&Host, &[Local]) -> bool,
    ) {
        let dt = 1.0 / 256.0;
        let nobody = Player::new(Vec3::new(0.5, -100.0, 0.5));
        let input = MoveInput {
            fps: true,
            ..Default::default()
        };
        for _ in 0..(seconds / dt) as u32 {
            {
                let h = host_of(g);
                // Échauffement et départ de manche écourtés (les achats se font quand même).
                let wait = matches!(h.m.phase, game::phase::WARMUP | game::phase::FREEZE);
                if wait && h.m.time > 3.6 {
                    h.m.time = 3.6;
                }
            }
            g.on_tick(world, &nobody, &input, dt);
            let out = g.end_frame(world, Vec::new(), 255, NadeOut::default(), dt);
            if watch(host_of(g), &out) {
                return;
            }
        }
    }

    /// (v174) Partie solo : en spectateur, on en sort et elle continue sans nous ; de
    /// retour, on y est tout de suite.
    #[test]
    fn a_solo_match_goes_on_while_the_player_watches() {
        let (world, map, spawn) = obstacle_course();
        let mut g = NetGame::solo("Moi".into(), map, spawn, None, 0);
        let me = Player::new(spawn);
        let input = MoveInput {
            fps: true,
            ..Default::default()
        };
        let dt = 1.0 / 256.0;
        let step = |g: &mut NetGame, secs: f32| -> Vec<Local> {
            let mut all = Vec::new();
            for _ in 0..(secs / dt) as u32 {
                g.on_tick(&world, &me, &input, dt);
                all.extend(g.end_frame(&world, Vec::new(), 255, NadeOut::default(), dt));
            }
            all
        };
        step(&mut g, 1.0);
        assert!(g.set_away(false), "en jeu : rien ne change");
        assert!(!g.set_away(true), "sorti de la partie");
        let before = host_of(&mut g).m.time;
        step(&mut g, 2.0);
        assert!(
            host_of(&mut g).m.time < before - 1.5,
            "elle continue sans lui"
        );
        assert!(g.set_away(false), "de retour tout de suite");
        let out = step(&mut g, 0.1);
        assert!(out.iter().any(|l| matches!(
            l,
            Local::Resume {
                fresh: false,
                reset: false,
                ..
            }
        )));
    }

    /// (v243) Partie d'un serveur de construction : comme la partie solo (bots ajoutés et
    /// retirés, elle commence tout de suite), mais ses messages partent au relais (ses joueurs
    /// les reçoivent), et un joueur du serveur y entre comme un invité ; pas de journal pour un
    /// vérificateur.
    #[test]
    fn a_build_match_has_bots_and_talks_to_its_players() {
        let (world, map, spawn) = obstacle_course();
        let (link, rx) = std::sync::mpsc::channel();
        let mut g = NetGame::build_host(link, "Op".into(), map, spawn, None, 0, 4);
        assert!(g.is_build() && !g.is_solo());
        assert_eq!((g.bot_count(), g.human_count()), (3, 1));
        assert_ne!(g.phase(), game::phase::WARMUP, "commencée tout de suite");
        // Un joueur du serveur entre : il compte parmi les joueurs, un bot lui laisse sa place
        // quand on demande quatre joueurs en tout.
        g.peer_joined(41, "Eve".into());
        assert_eq!(g.human_count(), 2);
        assert_eq!(g.set_bot_count(4 - g.human_count()), 2);
        let me = Player::new(spawn);
        let input = MoveInput {
            fps: true,
            ..Default::default()
        };
        let dt = 1.0 / 256.0;
        for _ in 0..64 {
            g.on_tick(&world, &me, &input, dt);
            g.end_frame(&world, Vec::new(), 255, NadeOut::default(), dt);
        }
        assert!(rx.try_iter().count() > 0, "l'état de la partie part au relais");
        assert!(host_of(&mut g).log.is_empty(), "pas de journal");
        g.peer_left(41);
        assert_eq!(g.human_count(), 1);
    }

    /// Parcours d'obstacles : départ des T, une marche, un trou à sauter, une échelle qui
    /// monte sur une plate-forme où sont le site et le départ des CT.
    fn obstacle_course() -> (World, FpsMap, Vec3) {
        let mut w = World::new(0);
        for cx in -1..=3 {
            for cz in -1..=1 {
                w.insert_empty(ChunkPos { x: cx, z: cz });
            }
        }
        let reg = minelib_world::registry::registry();
        let ladder = reg.by_key("ladder:0").expect("échelle");
        for z in 0..=4 {
            // Sol (pieds à 11), marche (pieds à 12) à partir de x = 8.
            for x in 0..=13 {
                w.set(x, 10, z, BlockId::STONE);
                if x >= 8 {
                    w.set(x, 11, z, BlockId::STONE);
                }
            }
            // Trou en x = 14 et 15 (le vide), puis la suite (pieds à 12).
            for x in 16..=21 {
                w.set(x, 10, z, BlockId::STONE);
                w.set(x, 11, z, BlockId::STONE);
            }
            // Plate-forme (pieds à 16), mur de 4 blocs en x = 22, échelle devant.
            for x in 22..=34 {
                for y in 10..=15 {
                    w.set(x, y, z, BlockId::STONE);
                }
            }
            for y in 12..=15 {
                w.set(21, y, z, ladder);
            }
        }
        let map = FpsMap {
            ct: Some(game::Zone::new([31, 16, 1], [33, 16, 3])),
            t: Some(game::Zone::new([1, 11, 1], [3, 11, 3])),
            sites: [Some(game::Zone::new([26, 16, 0], [29, 16, 4])), None, None],
            max_players: 2,
            door_list: Vec::new(),
        };
        (w, map, Vec3::new(2.5, 11.0, 2.5))
    }

    /// Le relevé des bots passe la marche, le trou (en sautant) et l'échelle.
    #[test]
    fn the_bots_nav_crosses_a_step_a_gap_and_a_ladder() {
        let (w, map, spawn) = obstacle_course();
        let nav = crate::bots::Nav::build(&w, &map, &[spawn]);
        let site = nav.near(Vec3::new(27.5, 16.0, 2.5)).expect("site relevé");
        let start = nav.near(spawn).expect("départ relevé");
        assert!(nav.len() > 100, "{} cases", nav.len());
        assert!(nav.path_steps(start, site).is_some(), "pas de chemin");
        let steps = nav.path_steps(start, site).unwrap();
        use crate::radar::Step;
        assert!(steps.contains(&Step::Jump), "{steps:?}");
        assert!(steps.contains(&Step::Ladder), "{steps:?}");
    }

    /// Un T (bot) mène la bombe par le parcours jusqu'au site et la pose ; le CT (bot)
    /// vient la désamorcer. (Bots « calmes » : ils ne se battent pas.)
    #[test]
    fn a_bot_carries_the_bomb_over_the_course_plants_and_a_bot_defuses() {
        let (w, map, spawn) = obstacle_course();
        let mut g = bots_only(&w, map, spawn, true);
        let mut planted = false;
        let mut end = None;
        let trace = std::env::var("BOT_TRACE").is_ok();
        let mut ticks = 0u32;
        run_bots(&mut g, &w, 200.0, |h, out| {
            ticks += 1;
            if trace && ticks.is_multiple_of(256) {
                if let Some(b) = &h.bots {
                    for line in b.debug() {
                        let id = u64::from_str_radix(line.split(' ').next().unwrap_or("0"), 16)
                            .unwrap_or(0)
                            + crate::bots::BOT_BASE;
                        let p = h.peers.get(&id).map(|r| r.player.pos);
                        println!("{} s phase {} : {line} {p:.2?}", ticks / 256, h.m.phase);
                    }
                }
            }
            planted |= h.m.bomb == game::bomb::PLANTED;
            for l in out {
                if let Local::RoundEnd { winner, reason } = l {
                    end = Some((*winner, *reason, h.m.bomb));
                    return true;
                }
            }
            false
        });
        assert!(planted, "bombe jamais posée (fin : {end:?})");
        assert_eq!(
            end.map(|e| (e.0, e.1)),
            Some((game::CT, game::reason::DEFUSED)),
            "fin de manche"
        );
    }

    /// Bots sur de vraies cartes (`BOT_MAPS` : dossiers de mondes séparés par des `;`,
    /// `BOT_MINUTES` : minutes de jeu, 3 par défaut) : relevé, puis une partie de bots
    /// seuls, avec ce qui s'y passe (éliminations, poses, désamorçages, bots coincés).
    #[test]
    #[ignore]
    fn bots_on_real_maps() {
        let maps = std::env::var("BOT_MAPS").expect("BOT_MAPS");
        let minutes: f32 = std::env::var("BOT_MINUTES")
            .ok()
            .and_then(|m| m.parse().ok())
            .unwrap_or(3.0);
        for dir in maps.split(';').filter(|d| !d.is_empty()) {
            let dir = std::path::PathBuf::from(dir);
            let map = crate::fpsmap::load(&dir);
            let t = Instant::now();
            let (data, _) = crate::save::load_world(&dir).expect("monde");
            let spawn = data
                .player
                .map(|p| Vec3::from(p.pos))
                .unwrap_or(Vec3::new(0.5, 80.0, 0.5));
            let world = crate::radar::load_map_world(&dir, &map).expect("monde");
            let loaded = t.elapsed().as_secs_f32();
            let t = Instant::now();
            let nav = crate::bots::Nav::build(&world, &map, &[spawn]);
            println!(
                "{} : monde lu en {loaded:.1} s, relevé {} cases en {:.1} s",
                dir.display(),
                nav.len(),
                t.elapsed().as_secs_f32()
            );
            print!("{}", nav.report(&map));
            if std::env::var("BOT_NAV_ONLY").is_ok() {
                continue;
            }
            let mut g = bots_only(&world, map, spawn, false);
            if let Some(b) = host_of(&mut g).bots.as_deref_mut() {
                b.set_nav(nav);
            }
            let (mut kills, mut rounds, mut plants, mut defused, mut exploded) = (0, 0, 0, 0, 0);
            let mut was_planted = false;
            let mut ends: Vec<String> = Vec::new();
            let t = Instant::now();
            let trace = std::env::var("BOT_TRACE").is_ok();
            let mut ticks = 0u32;
            run_bots(&mut g, &world, minutes * 60.0, |h, out| {
                ticks += 1;
                if trace && ticks.is_multiple_of(256 * 3) {
                    println!(
                        "  t {} s, phase {}, temps {:.0}, bombe {} porteur {:?}",
                        ticks / 256,
                        h.m.phase,
                        h.m.time,
                        h.m.bomb,
                        h.m.carrier.map(|c| c & 0xff)
                    );
                    if let Some(b) = &h.bots {
                        for (line, (id, r)) in b.debug().iter().zip(h.peers.iter().take(0)) {
                            let _ = (line, id, r);
                        }
                        for line in b.debug() {
                            let id = u64::from_str_radix(line.split(' ').next().unwrap_or("0"), 16)
                                .unwrap_or(0)
                                + crate::bots::BOT_BASE;
                            let p = h.peers.get(&id).map(|r| r.player.pos);
                            let mp = h.m.players.get(&id);
                            if h.m.carrier == Some(id) {
                                if let Some(r) = h.peers.get(&id) {
                                    let f = r.player.pos;
                                    let inside: Vec<bool> =
                                        h.m.map
                                            .sites
                                            .iter()
                                            .flatten()
                                            .map(|z| z.contains(f.to_array()))
                                            .collect();
                                    println!(
                                        "      porteur : pieds {:.2?}, sol {}, vitesse {:.2}, dans les sites {inside:?}, touche {}",
                                        f,
                                        r.player.on_ground,
                                        Vec3::new(r.player.vel.x, 0.0, r.player.vel.z).length(),
                                        h.m.players.get(&id).is_some_and(|p| p.plant_key)
                                    );
                                }
                            }
                            println!(
                                "    {} {} {} {:?}",
                                if mp.is_some_and(|p| p.team == game::T) {
                                    "T "
                                } else {
                                    "CT"
                                },
                                if mp.is_some_and(|p| p.alive) {
                                    "vivant"
                                } else {
                                    "mort  "
                                },
                                line,
                                p.map(|p| [p.x as i32, p.y as i32, p.z as i32])
                            );
                        }
                    }
                }
                let planted = h.m.bomb == game::bomb::PLANTED;
                if planted && !was_planted {
                    plants += 1;
                }
                was_planted = planted;
                for l in out {
                    match l {
                        Local::Kill(_) => kills += 1,
                        Local::RoundEnd { winner, reason } => {
                            rounds += 1;
                            ends.push(format!(
                                "{}:{}",
                                if *winner == game::CT { "CT" } else { "T" },
                                match *reason {
                                    game::reason::T_ELIMINATED => "élim",
                                    game::reason::CT_ELIMINATED => "élim",
                                    game::reason::TIME => "temps",
                                    game::reason::EXPLODED => "boum",
                                    game::reason::DEFUSED => "désamorcée",
                                    _ => "?",
                                }
                            ));
                            if *reason == game::reason::DEFUSED {
                                defused += 1;
                            }
                            if *reason == game::reason::EXPLODED {
                                exploded += 1;
                            }
                        }
                        _ => {}
                    }
                }
                false
            });
            let stuck = host_of(&mut g)
                .bots
                .as_ref()
                .map_or(0, |b| b.stuck_events());
            println!("  manches : {}", ends.join(" "));
            println!(
                "  {minutes} min de jeu en {:.0} s : {rounds} manches, {kills} éliminations, \
                 {plants} poses, {defused} désamorcées, {exploded} explosions, {stuck} fois \
                 coincés",
                t.elapsed().as_secs_f32()
            );
        }
    }

    /// Dix bots dans une arène : ils se trouvent, se tirent dessus, les manches se jouent.
    #[test]
    fn ten_bots_fight_and_rounds_end() {
        let mut w = World::new(0);
        for cx in -2..=1 {
            for cz in -2..=1 {
                w.insert_empty(ChunkPos { x: cx, z: cz });
            }
        }
        for z in -24..=24 {
            for x in -24..=24 {
                w.set(x, 10, z, BlockId::STONE);
            }
        }
        // Quelques piliers (de quoi se cacher).
        for (px, pz) in [(-6, 0), (6, 0), (0, -6), (0, 6), (-12, 10), (12, 10)] {
            for y in 11..=13 {
                for dz in 0..2 {
                    for dx in 0..2 {
                        w.set(px + dx, y, pz + dz, BlockId::STONE);
                    }
                }
            }
        }
        let map = FpsMap {
            ct: Some(game::Zone::new([-2, 11, -22], [2, 11, -19])),
            t: Some(game::Zone::new([-2, 11, 19], [2, 11, 22])),
            sites: [
                Some(game::Zone::new([-14, 11, -12], [-9, 11, -7])),
                Some(game::Zone::new([9, 11, -12], [14, 11, -7])),
                None,
            ],
            max_players: 10,
            door_list: Vec::new(),
        };
        let spawn = Vec3::new(0.5, 11.0, 0.5);
        let mut g = bots_only(&w, map, spawn, false);
        let (mut kills, mut rounds, mut shots) = (0, 0, 0);
        run_bots(&mut g, &w, 240.0, |_, out| {
            for l in out {
                match l {
                    Local::Kill(_) => kills += 1,
                    Local::RoundEnd { .. } => rounds += 1,
                    Local::Shot { .. } => shots += 1,
                    _ => {}
                }
            }
            rounds >= 2
        });
        assert!(shots > 20, "{shots} tirs");
        assert!(kills >= 4, "{kills} éliminations");
        assert!(rounds >= 2, "{rounds} manches finies");
    }
}
